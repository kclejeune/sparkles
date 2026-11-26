//! Clones of a store: a new, independent database built from one consistent snapshot,
//! either in a directory ([`Store::clone_to`]) or in memory
//! ([`Store::clone_to_memory`]).
//!
//! The copy's generation is made in one of two ways. The rebuild path writes every quad
//! of the snapshot into the bulk builder, as a compaction does. The file path shares the
//! source generation's index files instead: by reflink where the file system supports
//! it (btrfs, XFS, ZFS block cloning), by hard link when asked for, and by copying them
//! otherwise. Index files are immutable once written, so a reflinked or copied file is
//! independent of the source, and a hard-linked one is never written through either
//! name. The file path applies when the snapshot is exactly the source's base
//! generation (an empty delta), it is the head, and every graph is kept.
//!
//! The source generation is leased while its files are shared, as a backup leases it,
//! so a compaction or bulk commit that switches generations meanwhile keeps the old
//! directory until the clone has its copy.

use super::{
    CommitInfo, CommitKind, Error, Generation, Id, IndexMeta, LeaseGuard, Perm, ProgressFn, Result,
    Snapshot, Store, StoreOptions, dir_size, sync_dir, write_atomic, write_synced,
};
use crate::access::{GraphAccess, Graphs};
use crate::commit::{self, Catalog, ForkedFrom};
use oxrdf::NamedNode;
use std::collections::{BTreeMap, HashSet};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// How a clone may produce its generation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CloneMode {
    /// share the source's index files by reflink, or copy them, when the snapshot
    /// allows it; rebuild otherwise
    #[default]
    Auto,
    /// as `Auto`, with hard links first (same file system only)
    Link,
    /// always rebuild from the snapshot
    Rebuild,
}

impl CloneMode {
    pub fn parse(s: &str) -> Option<CloneMode> {
        match s {
            "auto" => Some(CloneMode::Auto),
            "link" => Some(CloneMode::Link),
            "rebuild" => Some(CloneMode::Rebuild),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            CloneMode::Auto => "auto",
            CloneMode::Link => "link",
            CloneMode::Rebuild => "rebuild",
        }
    }
}

/// How a clone's generation was produced. With several file methods (a hard link that
/// fell back to a copy), the slowest one is reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CloneMethod {
    /// hard links to the source's index files
    Link,
    /// reflinks (copy-on-write clones) of the source's index files
    Reflink,
    /// byte copies of the source's index files (`copy_file_range` on Linux, which some
    /// file systems turn into block clones)
    Copy,
    /// rebuilt from the snapshot by the bulk builder
    Rebuild,
}

impl CloneMethod {
    pub fn name(self) -> &'static str {
        match self {
            CloneMethod::Link => "link",
            CloneMethod::Reflink => "reflink",
            CloneMethod::Copy => "copy",
            CloneMethod::Rebuild => "rebuild",
        }
    }
}

/// What [`Store::clone_to`] copies, how, and how it reports progress.
#[derive(Clone, Default)]
pub struct CloneOptions {
    /// graphs to leave out (e.g. the materialized inferences)
    pub exclude_graphs: Vec<NamedNode>,
    /// copy only these graphs: the default graph, graph IRIs and IRI patterns with `*`,
    /// as in a graph access rule (`None`: every graph). Graphs named by blank nodes are
    /// never in such a set.
    pub graphs: Option<Graphs>,
    /// set to `true` to cancel (checked every 65536 quads, or between files)
    pub cancel: Option<Arc<AtomicBool>>,
    /// Stop capture admission and rebuilding after this deadline.
    pub deadline: Option<std::time::Instant>,
    /// Refuse capture immediately while another writer holds the store.
    pub no_wait: bool,
    pub progress: Option<ProgressFn>,
    /// clone the state at this commit instead of the head (it must be readable)
    pub at: Option<crate::history::At>,
    /// whether the source's files may be shared
    pub mode: CloneMode,
    /// the name of the clone, shown as `clone:<label>` in the source's history status
    /// while its generation is leased (`clone` when empty)
    pub label: String,
}

/// Outcome of [`Store::clone_to`].
#[derive(Clone, Debug)]
pub struct CloneReport {
    /// the new database's dataset id
    pub dataset_id: uuid::Uuid,
    /// this store's id and the commit of the copied snapshot
    pub forked_from: ForkedFrom,
    /// snapshot version and generation of the source
    pub version: u64,
    pub generation: String,
    /// quads in the source snapshot
    pub source_quads: u64,
    /// quads in the clone (fewer when graphs were left out)
    pub quads: u64,
    /// graphs in the clone, the default graph included when it has quads
    pub graphs: u64,
    pub millis: u64,
    /// how the generation was produced
    pub method: CloneMethod,
    /// bytes of the index files shared or written
    pub bytes: u64,
    /// why the source's files could not be shared, for a rebuilt clone
    pub rebuild_reason: Option<&'static str>,
}

/// What a clone captured while it held the writer lock.
struct Capture {
    snap: Arc<Snapshot>,
    next_bnode: u64,
    prefixes: BTreeMap<String, String>,
    /// the source generation's directory, leased, when its files may be shared
    files: Option<(PathBuf, LeaseGuard)>,
    /// the managed definitions current when the snapshot was taken
    definitions: Vec<super::DefinitionFile>,
    /// the managed-graph registry's files (see [`crate::managed`]) at that time
    managed: Vec<(&'static str, Vec<u8>)>,
    /// captured configuration-history state and source provenance, when activated
    activated: Option<ActivatedCapture>,
}

struct ActivatedCapture {
    prep: crate::access::Preparation,
    managed_state: Arc<crate::managed::ManagedState>,
    src_family: uuid::Uuid,
    src_logical: uuid::Uuid,
    src_head: u64,
    src_generation: String,
    source_files: Vec<(String, super::managed_history::Bytes)>,
    prior_imports: Vec<super::configuration_history::PriorImportFiles>,
    prior_direct_sources: Vec<crate::evidence::Sha256Digest>,
}

/// A clone's generation, built or shared.
struct Built {
    meta: IndexMeta,
    method: CloneMethod,
    bytes: u64,
    graphs: u64,
    reason: Option<&'static str>,
}

/// Index files of a generation that are never shared: they grow (`wal.log`,
/// `delta.vocab`), or the clone writes its own (`commit.json`, `meta.json`).
const OWN_FILES: [&str; 4] = ["wal.log", "delta.vocab", "commit.json", "meta.json"];

impl Store {
    /// Build a new, independent database in `dir` (absent or empty) from one consistent
    /// snapshot of this store: every quad (of the graphs `opts.graphs` selects, except
    /// those in `opts.exclude_graphs`) in a generation `gen-0001` with an empty delta,
    /// the prefixes, and a new dataset id whose root commit records this store's id and
    /// the snapshot's commit as `forkedFrom`. Blank nodes keep their ids (`_:b<hex>`
    /// labels), and the blank-node counter is carried over, so new blank nodes never
    /// collide with copied ones. The clone carries no history: its commit catalog
    /// starts at its root commit, and named snapshots and retention settings stay with
    /// this store.
    ///
    /// When the snapshot is this store's base generation (no changes since the last
    /// compaction or bulk commit), the head is cloned and every graph is kept, the
    /// generation's index files are shared ([`CloneMode`]); otherwise they are rebuilt.
    ///
    /// When this store has full-text search, the clone's text index is built from the
    /// clone's data before this returns, so the clone serves text queries as soon as it
    /// is opened. `opts.cancel` and `opts.deadline` stop that build too.
    ///
    /// The managed definitions current at the snapshot, such as the views, are copied
    /// paused (see [`super::definitions`]); a clone of a past state copies the current
    /// ones. The writer lock is held only to capture the snapshot and the definitions;
    /// this store is never written.
    /// On any error `dir` is left as it was found (removed, or emptied).
    pub fn clone_to(&self, dir: &Path, opts: &CloneOptions) -> Result<CloneReport> {
        self.clone_to_inner(dir, opts, None)
    }

    #[cfg(test)]
    pub(crate) fn clone_to_with_preparation(
        &self,
        dir: &Path,
        opts: &CloneOptions,
        prep: &crate::access::Preparation,
    ) -> Result<CloneReport> {
        self.clone_to_inner(dir, opts, Some(prep))
    }

    fn clone_to_inner(
        &self,
        dir: &Path,
        opts: &CloneOptions,
        preparation: Option<&crate::access::Preparation>,
    ) -> Result<CloneReport> {
        let t0 = std::time::Instant::now();
        let existed = dir.exists();
        if existed && std::fs::read_dir(dir)?.next().is_some() {
            return Err(Error::Invalid(format!(
                "{} exists and is not empty",
                dir.display()
            )));
        }
        let mut cap = self.clone_capture(opts, preparation)?;
        // a definition the clone could not open is refused before anything is written
        super::definitions::check_all(&cap.definitions)?;
        std::fs::create_dir_all(dir)?;
        let mut guard = CleanDir {
            dir,
            remove: !existed,
            armed: true,
        };
        let name = "gen-0001";
        let gdir = dir.join(name);
        // the clone's file system keeps the same free space as this store's
        let built = self.clone_generation(&gdir, &cap, opts, self.opts.min_free_disk_bytes)?;
        let snap = &cap.snap;
        // a new lineage: its own id and root commit, forked from the snapshot
        let id = uuid::Uuid::new_v4();
        let now = self.clone_root_time(snap);
        let root = root_commit(now, built.meta.quads, 1);
        let forked_from = ForkedFrom {
            id: self.dataset_id,
            seq: snap.commit,
        };
        let has_activated = cap.activated.is_some();
        let dataset_bytes = if has_activated {
            commit::with_reader(
                &commit::clone_dataset_file_bytes(id, now, forked_from),
                commit::IMPORTED_DESTINATION_READER,
            )?
        } else {
            commit::clone_dataset_file_bytes(id, now, forked_from)
        };
        write_atomic(&dir.join("dataset.json"), &dataset_bytes)?;
        let commit_bytes = if let Some(activated) = cap.activated.take() {
            let installed = match activated.managed_state.as_ref() {
                crate::managed::ManagedState::Installed(i) => i,
                crate::managed::ManagedState::Unavailable { reason, .. } => {
                    return Err(crate::managed::ManagedError::error(
                        crate::managed::ManagedErrorKind::Unavailable,
                        reason.clone(),
                    ));
                }
            };
            let genesis_id = super::configuration_history::write_clone_destination(
                dir,
                id,
                &root,
                installed,
                activated.src_family,
                activated.src_logical,
                activated.src_head,
                &activated.src_generation,
                activated.source_files,
                &activated.prior_imports,
                activated.prior_direct_sources,
                &activated.prep,
            )?;
            commit::gen_commit_bytes_with_binding(
                id,
                "clone",
                &root,
                crate::history::ConfigurationBinding::Known {
                    configuration: genesis_id,
                },
            )
        } else {
            commit::gen_commit_bytes(id, "clone", &root)
        };
        write_synced(&gdir.join("commit.json"), &commit_bytes)?;
        super::wal::create(
            &gdir.join("wal.log"),
            if has_activated {
                super::wal::version_with(true, self.control_enabled())
            } else {
                super::wal::PLAIN_VERSION
            },
        )?;
        sync_dir(&gdir)?;
        Catalog::create(&dir.join("commits.bin"), id, root)?;
        if !cap.prefixes.is_empty() {
            write_atomic(
                &dir.join("prefixes.json"),
                &serde_json::to_vec_pretty(&cap.prefixes).unwrap(),
            )?;
        }
        // Full-text search, the spatial index and the vector indexes stay configured.
        // The full-text index is built here, before the clone exists, so the clone
        // answers text queries as soon as it is opened. The source's index cannot be
        // copied, because its checkpoint names a commit of the source and the clone
        // starts again at commit 0. The spatial and vector indexes build in the
        // background when the clone is opened, and queries give the same answers
        // without them meanwhile.
        for (file, cfg) in self.index_config_files()? {
            #[cfg(feature = "text")]
            if file == "text.json" {
                let c: crate::text::TextConfig = serde_json::from_slice(&cfg)
                    .map_err(|e| Error::Corrupt(format!("text.json: {e}")))?;
                report(opts, 0.9, "building the full-text index");
                let gen_ = Arc::new(Generation::open(&gdir, name, false)?);
                let mut text_snap = self.base_snapshot(&gen_, root.seq);
                text_snap.dataset_id = id;
                crate::text::TextIndex::build_unopened(dir, &c, &text_snap, &|| {
                    check_control(opts)
                })?;
            }
            write_atomic(&dir.join(file), &cfg)?;
        }
        // write-time validation stays configured (the clone judges its first write in
        // full), and the stored queries, the named projections and the GraphQL configuration come
        // along
        if let Some(root) = &self.root {
            for f in crate::guard::config::FILES.iter().chain([
                &crate::stored::FILE,
                &crate::stored_mappings::FILE,
                &crate::algo::catalog::FILE,
                &"graphql.json",
            ]) {
                match std::fs::read(root.join(f)) {
                    Ok(b) => write_atomic(&dir.join(f), &b)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        // the managed definitions come along paused: the clone is a new dataset, whose
        // views run only once an operator has checked their principals and inputs
        super::definitions::write_all(dir, &cap.definitions, Some("cloned"))?;
        // A clone starts with an empty control keyspace: its copied `jobs.json` needs
        // one, and it holds no run, lease or delivery identity of its source (spec C26
        // §7.3). The reader rises first, and the checkpoint precedes the log.
        if self.control_enabled() {
            commit::require_reader(dir, crate::jobs::READER)?;
            write_atomic(
                &dir.join(crate::control::CHECKPOINT_FILE),
                &crate::control::encode_checkpoint(&Default::default()),
            )?;
            write_atomic(
                &dir.join(crate::control::LOG_FILE),
                &crate::control::log_header(),
            )?;
        }
        // the managed graphs stay protected: the registry needs its reader first
        if !cap.managed.is_empty() {
            commit::require_reader(dir, crate::managed::READER)?;
            for (name, bytes) in &cap.managed {
                write_atomic(&dir.join(name), bytes)?;
            }
        }
        // the branch ordinals the copied blank nodes may carry are not given out again
        let next_ordinal = self.branching.next_ordinal.load(Ordering::Relaxed);
        if next_ordinal > 1 {
            super::branching::write_initial_table(dir, id, next_ordinal)?;
        }
        // CURRENT last: the commit point of the new database
        write_atomic(&dir.join("CURRENT"), name.as_bytes())?;
        sync_dir(dir)?;
        guard.armed = false;
        report(opts, 1.0, "done");
        Ok(CloneReport {
            dataset_id: id,
            forked_from,
            version: snap.version,
            generation: snap.generation.name.clone(),
            source_quads: snap.len(),
            quads: built.meta.quads,
            graphs: built.graphs,
            millis: t0.elapsed().as_millis() as u64,
            method: built.method,
            bytes: built.bytes,
            rebuild_reason: built.reason,
        })
    }

    /// [`clone_to`](Self::clone_to) into a new in-memory store with options
    /// `store_opts`. Its base is a generation in a temporary directory, as an
    /// in-memory store's is after a bulk load, which the store removes when it is
    /// dropped. The full-text, spatial and vector indexes are configured as in this
    /// store and built again; write-time validation and stored queries are not
    /// carried over. A clone over `store_opts.max_memory_bytes` fails with `507`.
    pub fn clone_to_memory(
        &self,
        opts: &CloneOptions,
        store_opts: StoreOptions,
    ) -> Result<(Store, CloneReport)> {
        let t0 = std::time::Instant::now();
        let mut cap = self.clone_capture(opts, None)?;
        let tmp = tempfile::Builder::new().prefix("sparkles-mem-").tempdir()?;
        let built =
            self.clone_generation(tmp.path(), &cap, opts, store_opts.min_free_disk_bytes)?;
        if let Some(max) = store_opts.max_memory_bytes {
            let size = dir_size(tmp.path());
            if size > max {
                let h = crate::error::human_bytes;
                return Err(Error::StorageFull(format!(
                    "the in-memory clone would take about {}, over its limit of {}",
                    h(size),
                    h(max)
                )));
            }
        }
        let mut gen_ = Generation::open(tmp.path(), "mem", false)?;
        gen_._tmp = Some(tmp);
        let id = uuid::Uuid::new_v4();
        let snap = &cap.snap;
        let forked_from = ForkedFrom {
            id: self.dataset_id,
            seq: snap.commit,
        };
        let root = root_commit(self.clone_root_time(snap), built.meta.quads, 0);
        let next_bnode = built.meta.next_bnode.max(cap.next_bnode);
        let store = Store::in_memory_from(
            store_opts,
            Arc::new(gen_),
            next_bnode,
            cap.prefixes.clone(),
            root,
            id,
            Some(forked_from),
        );
        if self.control_enabled() {
            store.enable_control()?;
        }
        if let Some(activated) = cap.activated.take() {
            let prep = activated.prep;
            let installed = match activated.managed_state.as_ref() {
                crate::managed::ManagedState::Installed(i) => i,
                crate::managed::ManagedState::Unavailable { reason, .. } => {
                    return Err(crate::managed::ManagedError::error(
                        crate::managed::ManagedErrorKind::Unavailable,
                        reason.clone(),
                    ));
                }
            };
            let mut dest_installed = crate::managed::Installed::new(
                installed.graphs.clone(),
                installed.settings.clone(),
            )?;
            let genesis = super::managed_history::prepare(
                id,
                uuid::Uuid::new_v4(),
                1,
                None,
                &dest_installed,
                &prep,
            )?;
            let coverage = super::managed_history::Coverage {
                format: super::managed_history::FORMAT,
                dataset_id: id,
                lineage_id: genesis.identity.lineage_id,
                activation_generation: genesis.identity.generation,
                branches: BTreeMap::from([(
                    id,
                    super::managed_history::BranchCoverage {
                        activation_head: 0,
                        first_bound_own: 1,
                    },
                )]),
            };
            // Pin the canonical `generation: 1` creation commit so materialized
            // backups of this in-memory clone at `seq == 0` carry a valid format 2
            // `origin.json` with `creationCommit` and restore cleanly.
            let materialized_root = CommitInfo {
                generation: 1,
                ..root
            };
            let creation_commit = super::configuration_history::clone_creation_digest(
                id,
                &materialized_root,
                genesis.identity,
                &prep,
            )?;
            let mut origin_retained = 0u64;
            for (_, b) in &activated.source_files {
                origin_retained = origin_retained.saturating_add(b.held_bytes());
            }
            for (_, files) in &activated.prior_imports {
                for (_, b) in files {
                    origin_retained = origin_retained.saturating_add(b.held_bytes());
                }
            }
            let mem_origin = Arc::new(super::managed_history::MemoryOrigin {
                creation_commit,
                src_family: activated.src_family,
                src_logical: activated.src_logical,
                src_head: activated.src_head,
                src_generation: activated.src_generation,
                source_files: activated.source_files,
                prior_imports: activated.prior_imports,
                prior_direct_sources: activated.prior_direct_sources,
                retained_bytes: origin_retained,
            });
            let (memory, hist_root) = super::managed_history::MemoryHistory::initial_with_origin(
                genesis,
                coverage,
                Some(mem_origin),
                &prep,
            )?;
            let s = store.snapshot();
            store.check_memory(
                s.generation
                    .disk_bytes()
                    .saturating_add(super::delta_bytes(&s.delta))
                    .saturating_add(s.generation.dvocab.with(|v| v.bytes()) as u64)
                    .saturating_add(memory.retained()),
            )?;
            dest_installed.configuration_history = Some(hist_root.clone());
            dest_installed.history_memory = Some(memory);
            store
                .family
                .managed
                .store(Some(Arc::new(crate::managed::ManagedState::Installed(
                    dest_installed,
                ))));
            if let Some(mem_hist) = &store.mem_history {
                mem_hist.lock().configurations.insert(
                    0,
                    crate::history::ConfigurationBinding::Known {
                        configuration: hist_root.identity(id),
                    },
                );
            }
        } else {
            store.install_managed_files(&cap.managed);
        }
        report(opts, 0.95, "configuring indexes");
        self.configure_indexes(&store)?;
        report(opts, 1.0, "done");
        let report = CloneReport {
            dataset_id: id,
            forked_from,
            version: snap.version,
            generation: snap.generation.name.clone(),
            source_quads: snap.len(),
            quads: built.meta.quads,
            graphs: built.graphs,
            millis: t0.elapsed().as_millis() as u64,
            method: built.method,
            bytes: built.bytes,
            rebuild_reason: built.reason,
        };
        Ok((store, report))
    }

    /// Take the snapshot to clone, with the blank-node counter and the prefixes, under
    /// the writer lock (no commit falls in between), and lease its generation when its
    /// files may be shared.
    /// The time of a clone's root commit: the store's clock, never before the commit
    /// the clone copies. A cloned write therefore never enters a state of the clone
    /// that precedes the write's own commit, even when the clock runs behind (spec C27
    /// §4.3). A copied commit the catalog no longer lists is bounded by the head.
    fn clone_root_time(&self, snap: &Snapshot) -> i64 {
        let copied = self
            .commit(snap.commit)
            .unwrap_or_else(|| self.head_commit())
            .timestamp_ms;
        self.now_ms().max(copied)
    }

    fn clone_capture(
        &self,
        opts: &CloneOptions,
        preparation: Option<&crate::access::Preparation>,
    ) -> Result<Capture> {
        for _ in 0..2 {
            if let Some(cap) = self.clone_capture_once(opts, preparation)? {
                return Ok(cap);
            }
        }
        Err(Error::Conflict(
            "configuration changed twice during clone capture; retry".into(),
        ))
    }

    fn clone_capture_once(
        &self,
        opts: &CloneOptions,
        preparation: Option<&crate::access::Preparation>,
    ) -> Result<Option<Capture>> {
        self.failpoint("clone-admitting");
        let share = opts.mode != CloneMode::Rebuild && opts.at.is_none();
        let write_opts = crate::guard::WriteOptions {
            cancel: opts.cancel.clone(),
            deadline: opts.deadline,
            no_wait: opts.no_wait,
            ..Default::default()
        };
        let prep = match preparation {
            Some(p) => p.clone(),
            None => clone_preparation(opts)?,
        };
        struct LockedActivated {
            managed_state: Arc<crate::managed::ManagedState>,
            identities: super::configuration_history::OriginalIdentities,
            src_head: u64,
            src_generation: String,
            local_dataset: Option<super::managed_history::Bytes>,
            local_current: Option<super::managed_history::Bytes>,
            local_commit: Option<super::managed_history::Bytes>,
            local_meta: Option<super::managed_history::Bytes>,
        }
        let (snap, next_bnode, lease, definitions, managed, locked_activated) = {
            let w = self.lock_writer(&write_opts)?;
            let snap = self.snapshot();
            // under the same lock as the snapshot: a definition changed after it is
            // not the clone's. Read before the lease is taken, so that a failure
            // leaves no lease behind.
            let definitions = self.capture_definitions()?;
            let (managed, locked_activated) = if self.configuration_activated() {
                let state = self.managed_state_shared();
                let is_installed = matches!(
                    state.as_deref(),
                    Some(crate::managed::ManagedState::Installed(i))
                        if i.configuration_history.is_some()
                );
                if !is_installed {
                    let reason = match state.as_deref() {
                        Some(crate::managed::ManagedState::Unavailable { reason, .. }) => {
                            reason.clone()
                        }
                        _ => "activated configuration history is unavailable".into(),
                    };
                    return Err(crate::managed::ManagedError::error(
                        crate::managed::ManagedErrorKind::Unavailable,
                        format!("cannot clone dataset: {reason}"),
                    ));
                }
                let identities = super::configuration_history::OriginalIdentities::capture(
                    self, &w, &state, &prep,
                )?;
                let src_generation = if self.root.is_some() {
                    snap.generation.name.clone()
                } else {
                    "gen-0001".to_owned()
                };
                let (local_dataset, local_current, local_commit, local_meta) =
                    if let Some(root) = &self.root {
                        let ds = super::configuration_history::read_anchored_local_file(
                            root,
                            "dataset.json",
                            false,
                            &prep,
                        )?;
                        let cur = super::configuration_history::read_anchored_local_file(
                            root, "CURRENT", false, &prep,
                        )?;
                        let (cj, mj) = if snap.generation.link.is_none() {
                            (
                                super::configuration_history::read_anchored_local_file(
                                    root,
                                    &format!("{src_generation}/commit.json"),
                                    true,
                                    &prep,
                                )?,
                                super::configuration_history::read_anchored_local_file(
                                    root,
                                    &format!("{src_generation}/meta.json"),
                                    true,
                                    &prep,
                                )?,
                            )
                        } else {
                            (None, None)
                        };
                        (ds, cur, cj, mj)
                    } else {
                        (None, None, None, None)
                    };
                (
                    Vec::new(),
                    Some(LockedActivated {
                        managed_state: state.expect("checked installed"),
                        identities,
                        src_head: w.head.seq,
                        src_generation,
                        local_dataset,
                        local_current,
                        local_commit,
                        local_meta,
                    }),
                )
            } else {
                (self.managed_files()?, None)
            };
            let lease = match (&self.root, &self.history) {
                // a linked generation's base files are its upstream's: rebuilt
                (Some(root), Some(h))
                    if share && snap.delta.is_empty() && snap.generation.link.is_none() =>
                {
                    let no = commit::generation_number(&snap.generation.name);
                    let label = if opts.label.is_empty() {
                        "clone"
                    } else {
                        opts.label.as_str()
                    };
                    let id = h.lock().lease_for(no, label, true);
                    Some((root.join(&snap.generation.name), no, label.to_string(), id))
                }
                _ => None,
            };
            (
                snap,
                w.next_bnode,
                lease,
                definitions,
                managed,
                locked_activated,
            )
        };
        // from here on, dropping the guard releases the lease
        let files = lease.map(|(dir, no, label, id)| {
            let collector = self.collector();
            let guard = LeaseGuard {
                generation: no,
                label,
                release: Some(Box::new(move || {
                    if let Some(c) = collector {
                        c.release(id);
                    }
                })),
            };
            (dir, guard)
        });
        let activated = if let Some(locked) = locked_activated {
            let installed = match locked.managed_state.as_ref() {
                crate::managed::ManagedState::Installed(i) => i,
                _ => unreachable!("checked installed"),
            };
            let mut source_files: Vec<(String, super::managed_history::Bytes)> = Vec::new();
            let push_src = |list: &mut Vec<(String, super::managed_history::Bytes)>,
                            rel: String,
                            bytes: super::managed_history::Bytes|
             -> Result<()> {
                if list.len() >= super::configuration_history::SOURCE_MEMBER_LIMIT {
                    return Err(super::managed_history::unavailable(
                        "clone source provenance exceeds member limit",
                    ));
                }
                list.push((rel, bytes));
                Ok(())
            };
            push_src(
                &mut source_files,
                "family-dataset.json".into(),
                super::managed_history::Bytes::from_vec(
                    locked.identities.family_document().to_vec(),
                    &prep,
                )?,
            )?;
            let ds_bytes = match locked.local_dataset {
                Some(b) => b,
                None if locked.identities.family() == locked.identities.logical() => {
                    super::managed_history::Bytes::from_vec(
                        locked.identities.family_document().to_vec(),
                        &prep,
                    )?
                }
                None => super::managed_history::Bytes::from_vec(
                    commit::with_reader(
                        &commit::dataset_file_bytes(
                            locked.identities.logical(),
                            "memory-capture",
                            self.now_ms(),
                        ),
                        super::managed_history::READER,
                    )?,
                    &prep,
                )?,
            };
            push_src(&mut source_files, "dataset.json".into(), ds_bytes)?;
            let cur_bytes = match locked.local_current {
                Some(b) => b,
                None => super::managed_history::Bytes::from_vec(
                    format!("{}\n", locked.src_generation).into_bytes(),
                    &prep,
                )?,
            };
            push_src(&mut source_files, "CURRENT".into(), cur_bytes)?;
            if let Some(branch_bytes) = locked.identities.branch_document() {
                push_src(
                    &mut source_files,
                    "branch.json".into(),
                    super::managed_history::Bytes::from_vec(branch_bytes.to_vec(), &prep)?,
                )?;
            }
            if let Some(cj) = locked.local_commit {
                push_src(
                    &mut source_files,
                    format!("{}/commit.json", locked.src_generation),
                    cj,
                )?;
            }
            if let Some(mj) = locked.local_meta {
                push_src(
                    &mut source_files,
                    format!("{}/meta.json", locked.src_generation),
                    mj,
                )?;
            }
            let (prior_imports, prior_direct_sources) = if let Some(family_root) =
                self.managed_root()
            {
                let captured = super::managed_history::capture::files_native_only(
                    &family_root,
                    self.owner_dataset_id(),
                    installed,
                    &write_opts,
                );
                if !self.same_managed_state(&Some(locked.managed_state.clone())) {
                    return Ok(None);
                }
                for cf in captured? {
                    prep.check()?;
                    let held = prep.reserve(cf.len.saturating_add(64))?;
                    let len = usize::try_from(cf.len).map_err(|_| prep.exhausted())?;
                    let mut buf = Vec::new();
                    buf.try_reserve_exact(len).map_err(|_| prep.exhausted())?;
                    buf.resize(len, 0u8);
                    if !buf.is_empty() {
                        cf.read_exact_at(0, &mut buf)?;
                    }
                    push_src(
                        &mut source_files,
                        cf.path,
                        super::managed_history::Bytes {
                            value: buf,
                            _held: held,
                        },
                    )?;
                }
                // Include family's top-level origin.json as a source member when present.
                if let Some(origin_bytes) = super::configuration_history::read_anchored_local_file(
                    &family_root,
                    "evidence-history/origin.json",
                    true,
                    &prep,
                )? {
                    push_src(
                        &mut source_files,
                        "evidence-history/origin.json".into(),
                        origin_bytes,
                    )?;
                }
                super::configuration_history::read_prior_imports_for_clone(&family_root, &prep)?
            } else {
                let memory = installed.history_memory.as_ref().ok_or_else(|| {
                    super::managed_history::unavailable("in-memory configuration history missing")
                })?;
                push_src(
                    &mut source_files,
                    crate::managed::FILE.into(),
                    super::managed_history::Bytes::from_vec(installed.file_bytes(), &prep)?,
                )?;
                for (owner, bytes) in &installed.settings {
                    push_src(
                        &mut source_files,
                        owner.settings_file().into(),
                        super::managed_history::Bytes::from_vec(bytes.to_vec(), &prep)?,
                    )?;
                }
                let lineage = memory.coverage.value.lineage_id;
                let cov_bytes = super::managed_history::encode(&memory.coverage.value, &prep)?;
                push_src(
                    &mut source_files,
                    format!("evidence-history/{lineage}/coverage.json"),
                    cov_bytes,
                )?;
                let mut node = Some(memory.as_ref());
                while let Some(cur) = node {
                    prep.check()?;
                    let hex = cur.prepared.identity.generation.to_string();
                    let hex = hex.strip_prefix("sha256:").expect("canonical digest");
                    let held =
                        prep.reserve((cur.prepared.bytes.value.len() as u64).saturating_add(64))?;
                    let mut copied = Vec::new();
                    copied
                        .try_reserve_exact(cur.prepared.bytes.value.len())
                        .map_err(|_| prep.exhausted())?;
                    copied.extend_from_slice(&cur.prepared.bytes.value);
                    push_src(
                        &mut source_files,
                        format!("evidence-history/{lineage}/generations/{hex}.json"),
                        super::managed_history::Bytes {
                            value: copied,
                            _held: held,
                        },
                    )?;
                    node = cur.previous.as_deref();
                }
                (Vec::new(), Vec::new())
            };
            Some(ActivatedCapture {
                prep,
                managed_state: locked.managed_state,
                src_family: locked.identities.family(),
                src_logical: locked.identities.logical(),
                src_head: locked.src_head,
                src_generation: locked.src_generation,
                source_files,
                prior_imports,
                prior_direct_sources,
            })
        } else {
            None
        };
        self.failpoint("clone-captured");
        // a past state: its blank nodes are older than the counter, which never decreases
        let snap = match &opts.at {
            Some(at) => {
                let o = crate::history::HistoryOptions {
                    cancel: opts.cancel.clone(),
                    deadline: opts.deadline,
                };
                self.snapshot_at(at, &o)?.0
            }
            None => snap,
        };
        Ok(Some(Capture {
            snap,
            next_bnode,
            prefixes: self.prefixes(),
            files,
            definitions,
            managed,
            activated,
        }))
    }

    /// Make the clone's generation in `gdir`: share the captured generation's files when
    /// the clone allows it, else rebuild from the snapshot.
    fn clone_generation(
        &self,
        gdir: &Path,
        cap: &Capture,
        opts: &CloneOptions,
        reserve: Option<u64>,
    ) -> Result<Built> {
        let snap = &cap.snap;
        let excluded: HashSet<u64> = opts
            .exclude_graphs
            .iter()
            .filter_map(|g| snap.lookup_iri(g.as_str()))
            .map(|g| g.0)
            .collect();
        let view = opts
            .graphs
            .clone()
            .map(|read| GraphAccess::graphs(read, Graphs::none()));
        // the graphs that have quads, and those the clone keeps
        let all = snap.distinct_first(Perm::Gspo)?;
        let kept: Vec<u64> = all
            .iter()
            .copied()
            .filter(|g| !excluded.contains(g))
            .filter(|&g| view.as_ref().is_none_or(|v| v.readable_id(snap, Id(g))))
            .collect();
        let partial = kept.len() != all.len();
        let reason = if opts.mode == CloneMode::Rebuild {
            Some("a rebuild was asked for")
        } else if opts.at.is_some() {
            Some("a past state is cloned")
        } else if self.root.is_none() {
            Some("the source is in memory")
        } else if !snap.delta.is_empty() {
            Some("the source has changes since its last compaction")
        } else if partial {
            Some("some graphs are left out")
        } else {
            None
        };
        if reason.is_none()
            && let Some((src, _lease)) = &cap.files
        {
            let (meta, method, bytes) = share_generation(src, gdir, cap, opts, reserve)?;
            return Ok(Built {
                meta,
                method,
                bytes,
                graphs: kept.len() as u64,
                reason: None,
            });
        }
        let total = if partial {
            let mut n = 0;
            for &g in &kept {
                n += snap.count(Perm::Gspo, &[g])?;
            }
            n
        } else {
            snap.len()
        }
        .max(1);
        let mut seen = 0u64;
        report(opts, 0.0, "copying quads");
        let meta = self.build_from_snapshot(
            gdir,
            snap,
            partial.then_some(kept.as_slice()),
            cap.next_bnode,
            reserve,
            cap.prefixes.clone(),
            |_| {
                seen += 1;
                if seen.is_multiple_of(65_536) {
                    check_control(opts)?;
                    report(opts, 0.7 * seen as f32 / total as f32, "copying quads");
                }
                Ok(true)
            },
            || report(opts, 0.7, "building indexes"),
        )?;
        Ok(Built {
            meta,
            method: CloneMethod::Rebuild,
            bytes: dir_size(gdir),
            graphs: kept.len() as u64,
            reason,
        })
    }

    /// Configure the full-text, spatial and vector indexes of `dst` (an in-memory
    /// clone) as they are configured here; each is built from `dst`'s data.
    pub(super) fn configure_indexes(&self, dst: &Store) -> Result<()> {
        for (file, cfg) in self.index_config_files()? {
            match file {
                #[cfg(feature = "text")]
                "text.json" => {
                    let c: crate::text::TextConfig = serde_json::from_slice(&cfg)
                        .map_err(|e| Error::Corrupt(format!("text.json: {e}")))?;
                    dst.enable_text(c)?;
                }
                #[cfg(feature = "geo")]
                f if f == crate::geo::CONFIG_FILE => {
                    let c: crate::geo::GeoConfig = serde_json::from_slice(&cfg)
                        .map_err(|e| Error::Corrupt(format!("{f}: {e}")))?;
                    dst.enable_geo(c)?;
                }
                f if f == crate::vector::config::CONFIG_FILE => {
                    let c: crate::vector::VectorConfigFile = serde_json::from_slice(&cfg)
                        .map_err(|e| Error::Corrupt(format!("{f}: {e}")))?;
                    for (name, ic) in c.indexes {
                        dst.create_vector_index(&name, ic)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// The root commit of a clone holding `quads` in generation `generation`.
fn root_commit(timestamp_ms: i64, quads: u64, generation: u32) -> CommitInfo {
    CommitInfo {
        seq: 0,
        timestamp_ms,
        kind: CommitKind::Create,
        inserted: quads,
        deleted: 0,
        quads,
        generation,
        bulk: true,
        exact: true,
        reconstructed: false,
        default_graph: true,
        unvalidated: false,
    }
}

fn report(opts: &CloneOptions, f: f32, msg: &str) {
    if let Some(p) = &opts.progress {
        p(f, msg);
    }
}

fn check_control(opts: &CloneOptions) -> Result<()> {
    crate::guard::WriteOptions {
        cancel: opts.cancel.clone(),
        deadline: opts.deadline,
        ..Default::default()
    }
    .check()
}

/// Share the index files of the generation in `src` into `gdir`, and write `gdir`'s own
/// `meta.json`: the source's, with the captured blank-node counter and prefixes.
/// Returns that metadata, the slowest method used and the bytes shared.
fn share_generation(
    src: &Path,
    gdir: &Path,
    cap: &Capture,
    opts: &CloneOptions,
    reserve: Option<u64>,
) -> Result<(IndexMeta, CloneMethod, u64)> {
    std::fs::create_dir_all(gdir)?;
    let mut files: Vec<(String, u64)> = Vec::new();
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        // subdirectories hold derived indexes (spatial, vector), rebuilt where opened
        if !e.file_type()?.is_file() || name.ends_with(".tmp") || OWN_FILES.contains(&name.as_str())
        {
            continue;
        }
        files.push((name, e.metadata()?.len()));
    }
    files.sort();
    let total: u64 = files.iter().map(|f| f.1).sum::<u64>().max(1);
    let (mut done, mut method) = (0u64, CloneMethod::Link);
    let mut reflink = true;
    report(opts, 0.0, "copying files");
    for (name, len) in &files {
        check_control(opts)?;
        let m = share_file(
            &src.join(name),
            &gdir.join(name),
            *len,
            opts.mode,
            &mut reflink,
            reserve.map(|r| (gdir, r)),
        )?;
        method = method.max(m);
        done += len;
        report(opts, 0.9 * done as f32 / total as f32, "copying files");
    }
    // the version first, so a newer meta in another shape is refused as newer
    let written_by = src.parent().and_then(crate::commit::read_written_by);
    let mut meta: IndexMeta = crate::builder::read_index_meta(src, written_by.as_deref())?;
    if meta.quads != cap.snap.len() {
        return Err(Error::Corrupt(format!(
            "{}: meta.json counts {} quads, the snapshot {}",
            src.display(),
            meta.quads,
            cap.snap.len()
        )));
    }
    // writes since the generation was built may have used blank-node ids (deleted again
    // since, as the delta is empty): the clone never hands them out
    meta.next_bnode = meta.next_bnode.max(cap.next_bnode);
    meta.prefixes = cap.prefixes.clone();
    write_synced(
        &gdir.join("meta.json"),
        &serde_json::to_vec_pretty(&meta).unwrap(),
    )?;
    sync_dir(gdir)?;
    Ok((meta, method, done))
}

/// Make `to` a copy of the immutable file `from` (`len` bytes): a hard link with
/// [`CloneMode::Link`] when the file system allows it, else a reflink (while
/// `reflink` holds: the first refusal turns it off for the clone's other files), else a
/// byte copy, which first checks that `reserve` bytes stay free.
fn share_file(
    from: &Path,
    to: &Path,
    len: u64,
    mode: CloneMode,
    reflink: &mut bool,
    reserve: Option<(&Path, u64)>,
) -> Result<CloneMethod> {
    if mode == CloneMode::Link {
        match std::fs::hard_link(from, to) {
            Ok(()) => return Ok(CloneMethod::Link),
            // another file system, or links not supported: fall back
            Err(e) => {
                tracing::debug!(target: "sparkles::store::clone", "hard link {} failed: {e}", from.display())
            }
        }
    }
    if *reflink {
        if reflink_file(from, to)? {
            return Ok(CloneMethod::Reflink);
        }
        *reflink = false;
    }
    if let Some((dir, r)) = reserve {
        crate::disk::check_reserve(dir, r, len, false)?;
    }
    std::fs::copy(from, to)?;
    OpenOptions::new().write(true).open(to)?.sync_all()?;
    Ok(CloneMethod::Copy)
}

/// Clone `from` into the new file `to` with the `FICLONE` ioctl: `false` (and no `to`)
/// when the file system cannot (no reflink support, or another file system).
#[cfg(any(target_os = "linux", target_os = "android"))]
fn reflink_file(from: &Path, to: &Path) -> Result<bool> {
    use std::os::fd::AsRawFd;
    let src = std::fs::File::open(from)?;
    let dst = OpenOptions::new().write(true).create_new(true).open(to)?;
    // SAFETY: both descriptors are open for the duration of the call, and FICLONE
    // takes the source descriptor as its argument
    let r = unsafe { libc::ioctl(dst.as_raw_fd(), libc::FICLONE, src.as_raw_fd()) };
    if r == 0 {
        dst.sync_all()?;
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    drop(dst);
    std::fs::remove_file(to)?;
    tracing::debug!(target: "sparkles::store::clone", "reflink of {} failed: {e}", from.display());
    Ok(false)
}

/// No reflinks on this platform (`std::fs::copy` clones files where the platform's copy
/// call does, as on macOS).
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn reflink_file(_: &Path, _: &Path) -> Result<bool> {
    Ok(false)
}

/// Removes (or empties) a directory on drop unless disarmed.
struct CleanDir<'a> {
    dir: &'a Path,
    /// remove the directory itself (it did not exist before)
    remove: bool,
    armed: bool,
}

impl Drop for CleanDir<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.remove {
            let _ = std::fs::remove_dir_all(self.dir);
        } else if let Ok(rd) = std::fs::read_dir(self.dir) {
            for e in rd.flatten() {
                let p = e.path();
                let _ = if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
            }
        }
    }
}

#[cfg(test)]
#[path = "clone_tests.rs"]
mod tests;

fn clone_preparation(opts: &CloneOptions) -> Result<crate::access::Preparation> {
    super::managed_history::administrative_preparation_with(crate::task::Control {
        cancel: opts
            .cancel
            .clone()
            .map(crate::task::Cancel::from_flag)
            .unwrap_or_default(),
        deadline: opts.deadline,
        ..Default::default()
    })
}
