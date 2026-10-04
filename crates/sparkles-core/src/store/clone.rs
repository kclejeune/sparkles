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
use std::fs::{File, OpenOptions};
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
    /// The writer lock is held only to capture the snapshot; this store is never written.
    /// On any error `dir` is left as it was found (removed, or emptied).
    pub fn clone_to(&self, dir: &Path, opts: &CloneOptions) -> Result<CloneReport> {
        let t0 = std::time::Instant::now();
        let existed = dir.exists();
        if existed && std::fs::read_dir(dir)?.next().is_some() {
            return Err(Error::Invalid(format!(
                "{} exists and is not empty",
                dir.display()
            )));
        }
        let cap = self.clone_capture(opts)?;
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
        let now = commit::now_ms();
        let root = root_commit(now, built.meta.quads, 1);
        let forked_from = ForkedFrom {
            id: self.dataset_id,
            seq: snap.commit,
        };
        write_synced(
            &gdir.join("commit.json"),
            &commit::gen_commit_bytes(id, "clone", &root),
        )?;
        File::create(gdir.join("wal.log"))?.sync_all()?;
        sync_dir(&gdir)?;
        write_atomic(
            &dir.join("dataset.json"),
            &commit::clone_dataset_file_bytes(id, now, forked_from),
        )?;
        Catalog::create(&dir.join("commits.bin"), id, root)?;
        if !cap.prefixes.is_empty() {
            write_atomic(
                &dir.join("prefixes.json"),
                &serde_json::to_vec_pretty(&cap.prefixes).unwrap(),
            )?;
        }
        // full-text search, the spatial index and the vector indexes stay on: the clone
        // rebuilds them when opened
        for (file, cfg) in self.index_config_files()? {
            write_atomic(&dir.join(file), &cfg)?;
        }
        // write-time validation stays configured (the clone judges its first write in
        // full), and the stored queries and the GraphQL configuration come along
        if let Some(root) = &self.root {
            for f in crate::guard::config::FILES
                .iter()
                .chain([&crate::stored::FILE, &"graphql.json"])
            {
                match std::fs::read(root.join(f)) {
                    Ok(b) => write_atomic(&dir.join(f), &b)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
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
        let cap = self.clone_capture(opts)?;
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
        let root = root_commit(commit::now_ms(), built.meta.quads, 0);
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
    fn clone_capture(&self, opts: &CloneOptions) -> Result<Capture> {
        let share = opts.mode != CloneMode::Rebuild && opts.at.is_none();
        let (snap, next_bnode, lease) = {
            let w = self.writer.lock();
            let snap = self.snapshot();
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
            (snap, w.next_bnode, lease)
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
        self.failpoint("clone-captured");
        // a past state: its blank nodes are older than the counter, which never decreases
        let snap = match &opts.at {
            Some(at) => {
                let o = crate::history::HistoryOptions {
                    cancel: opts.cancel.clone(),
                    deadline: None,
                };
                self.snapshot_at(at, &o)?.0
            }
            None => snap,
        };
        Ok(Capture {
            snap,
            next_bnode,
            prefixes: self.prefixes(),
            files,
        })
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
                    if cancelled(opts) {
                        return Err(Error::Cancelled);
                    }
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
    fn configure_indexes(&self, dst: &Store) -> Result<()> {
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

fn cancelled(opts: &CloneOptions) -> bool {
    opts.cancel
        .as_ref()
        .is_some_and(|c| c.load(Ordering::Relaxed))
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
        if cancelled(opts) {
            return Err(Error::Cancelled);
        }
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
    let mut meta: IndexMeta = serde_json::from_slice(&std::fs::read(src.join("meta.json"))?)
        .map_err(|e| Error::Corrupt(format!("meta.json: {e}")))?;
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
            Err(e) => tracing::debug!("hard link {} failed: {e}", from.display()),
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
    let src = File::open(from)?;
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
    tracing::debug!("reflink of {} failed: {e}", from.display());
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
