//! Configured vector indexes in the store: creating, dropping and rebuilding them, their
//! status, and the hooks that build them at open and after a generation switch.
//!
//! * **Builds** pack a generation's base vectors and build the HNSW graph on a
//!   background thread, without the writer lock: an index depends only on the
//!   generation's base, and every search overlays its own snapshot's changes exactly. A
//!   build first publishes the packed vectors (searches then scan them exactly) and then
//!   the graph. Until then searches pack the predicate themselves, as without a
//!   configuration. Commits never wait for a build and do no index work.
//! * **Generation switches** (bulk commits, compactions) start a build for the new
//!   generation of every index; the replaced generation's files are written no more.
//! * **Files.** A persistent store writes each build to `gen-NNNN/vectors/<name>.spkv`
//!   and maps it from there, so opening the store again reads the index instead of
//!   building it. Files that are missing, damaged or made for another configuration or
//!   generation are removed and built again.

use super::{Snapshot, Store};
use crate::error::{Error, Result};
use crate::vector::config::{
    CONFIG_FILE, HnswStatus, MAX_INDEXES, VectorBuild, VectorConfigFile, VectorFiles,
    VectorIndexConfig, VectorIndexStatus, VectorMemory, VectorOverlay, validate_name,
};
use crate::vector::persist::{self, Identity};
use crate::vector::{BuildCtl, Built, Outcome, build_index};
use parking_lot::{Condvar, Mutex, RwLock};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// The configured indexes of a store.
#[derive(Default)]
pub(crate) struct VectorRegistry {
    indexes: RwLock<BTreeMap<String, Arc<IndexEntry>>>,
    /// test hook: background builds wait before they publish
    paused: AtomicBool,
    /// test hook: builds insert graph nodes one at a time, so graphs are reproducible
    sequential: AtomicBool,
}

/// One configured index and the state of its builds.
struct IndexEntry {
    name: String,
    config: VectorIndexConfig,
    /// +1 per build started: a build of an older epoch publishes nothing
    epoch: AtomicU64,
    /// progress of the running build (`f32` bits)
    progress: AtomicU32,
    info: Mutex<EntryInfo>,
    done: Condvar,
    /// dropped or replaced: running builds publish nothing
    retired: AtomicBool,
}

#[derive(Default)]
struct EntryInfo {
    /// the last epoch whose build is over
    finished: u64,
    /// `building`, `ready`, `failed` or `over-budget`
    state: &'static str,
    message: Option<String>,
    last_build: Option<VectorBuild>,
}

impl IndexEntry {
    fn new(name: &str, config: VectorIndexConfig) -> IndexEntry {
        IndexEntry {
            name: name.to_string(),
            config,
            epoch: AtomicU64::new(0),
            progress: AtomicU32::new(0),
            info: Mutex::new(EntryInfo {
                state: "building",
                ..Default::default()
            }),
            done: Condvar::new(),
            retired: AtomicBool::new(false),
        }
    }

    fn finish(
        &self,
        epoch: u64,
        state: &'static str,
        message: Option<String>,
        last: Option<VectorBuild>,
    ) {
        let mut i = self.info.lock();
        if epoch >= i.finished && epoch == self.epoch.load(Ordering::SeqCst) {
            i.finished = epoch;
            i.state = state;
            i.message = message;
            if last.is_some() {
                i.last_build = last;
            }
        }
        self.done.notify_all();
    }

    fn wait(&self, epoch: u64) {
        let mut i = self.info.lock();
        while i.finished < epoch
            && !self.retired.load(Ordering::SeqCst)
            && self.epoch.load(Ordering::SeqCst) == epoch
        {
            self.done
                .wait_for(&mut i, std::time::Duration::from_millis(50));
        }
    }
}

impl Store {
    /// The configured vector indexes, as `vector.json` holds them.
    pub fn vector_configs(&self) -> BTreeMap<String, VectorIndexConfig> {
        self.vector
            .indexes
            .read()
            .iter()
            .map(|(n, e)| (n.clone(), e.config.clone()))
            .collect()
    }

    /// Create or replace vector index `name` and start building it in the background
    /// (writes go on meanwhile); `Ok(true)` when it was created. `409` when another
    /// index has the predicate.
    pub fn create_vector_index(&self, name: &str, cfg: VectorIndexConfig) -> Result<bool> {
        validate_name(name)?;
        cfg.validate()?;
        let _w = self.writer.lock();
        let mut map = self.vector.indexes.write();
        if let Some((other, _)) = map
            .iter()
            .find(|(n, e)| n.as_str() != name && e.config.predicate == cfg.predicate)
        {
            return Err(Error::Conflict(format!(
                "<{}> is already indexed by vector index {other}",
                cfg.predicate
            )));
        }
        if !map.contains_key(name) && map.len() >= MAX_INDEXES {
            return Err(Error::invalid(format!(
                "a dataset has at most {MAX_INDEXES} vector indexes"
            )));
        }
        let created = !map.contains_key(name);
        let mut file = VectorConfigFile {
            indexes: map
                .iter()
                .map(|(n, e)| (n.clone(), e.config.clone()))
                .collect(),
            ..Default::default()
        };
        file.indexes.insert(name.to_string(), cfg.clone());
        self.write_vector_config(&file)?;
        let entry = Arc::new(IndexEntry::new(name, cfg.clone()));
        if let Some(old) = map.insert(name.to_string(), entry.clone()) {
            old.retired.store(true, Ordering::SeqCst);
            old.done.notify_all();
        }
        drop(map);
        let snap = self.snapshot();
        snap.generation
            .vectors
            .set_configured(Arc::new(self.vector_configs()));
        snap.generation.vectors.set_embedder(&self.embed);
        self.embed_configure(name, Some(&cfg));
        // an unchanged build (only efSearch, the threshold or the model differ) is kept
        let gv = &snap.generation.vectors;
        let kept = gv
            .built(name)
            .filter(|b| !b.hnsw_pending && b.config.build_hash() == cfg.build_hash());
        match kept {
            Some(b) => {
                gv.install(Arc::new(b.reconfigured(&cfg)));
                let e = entry.epoch.fetch_add(1, Ordering::SeqCst) + 1;
                let last = b.built_ms;
                entry.finish(
                    e,
                    "ready",
                    None,
                    Some(VectorBuild {
                        at: crate::commit::rfc3339_ms(crate::commit::now_ms()),
                        ms: last,
                        rows: b.segment.rows() as u64,
                    }),
                );
            }
            None => {
                gv.remove_built(name);
                self.spawn_vector_build(entry, snap, true);
            }
        }
        Ok(created)
    }

    /// Drop vector index `name`: its configuration and files go (`404` if unknown).
    pub fn drop_vector_index(&self, name: &str) -> Result<()> {
        let _w = self.writer.lock();
        let mut map = self.vector.indexes.write();
        let Some(old) = map.remove(name) else {
            return Err(Error::NotFound(format!("no vector index {name}")));
        };
        old.retired.store(true, Ordering::SeqCst);
        old.done.notify_all();
        let file = VectorConfigFile {
            indexes: map
                .iter()
                .map(|(n, e)| (n.clone(), e.config.clone()))
                .collect(),
            ..Default::default()
        };
        drop(map);
        self.write_vector_config(&file)?;
        let snap = self.snapshot();
        let gv = &snap.generation.vectors;
        gv.set_configured(Arc::new(file.indexes));
        gv.remove_built(name);
        self.embed_configure(name, None);
        if let Some(dir) = self.vector_dir(&snap) {
            gv.with_files(|| persist::remove(&dir, Some(name)));
        }
        Ok(())
    }

    /// Rebuild vector index `name` of the current generation from RDF, in the
    /// background. The current build serves searches until the new one replaces it.
    pub fn rebuild_vector_index(&self, name: &str) -> Result<()> {
        let entry = self
            .vector
            .indexes
            .read()
            .get(name)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("no vector index {name}")))?;
        self.spawn_vector_build(entry, self.snapshot(), false);
        Ok(())
    }

    /// Wait for the running build of index `name` (`None`: no such index). Returns at
    /// once while builds are paused.
    pub fn wait_vector_index(&self, name: &str) -> Option<VectorIndexStatus> {
        let entry = self.vector.indexes.read().get(name).cloned()?;
        if !self.vector.paused.load(Ordering::SeqCst) {
            entry.wait(entry.epoch.load(Ordering::SeqCst));
        }
        self.vector_index(name)
    }

    /// Wait for every running build.
    pub fn wait_vector_indexes(&self) -> Vec<VectorIndexStatus> {
        let names: Vec<String> = self.vector.indexes.read().keys().cloned().collect();
        for n in &names {
            self.wait_vector_index(n);
        }
        self.vector_indexes()
    }

    /// Status of every configured index, by name.
    pub fn vector_indexes(&self) -> Vec<VectorIndexStatus> {
        let snap = self.snapshot();
        let entries: Vec<Arc<IndexEntry>> = self.vector.indexes.read().values().cloned().collect();
        entries
            .iter()
            .map(|e| self.entry_status(e, &snap))
            .collect()
    }

    /// Status of index `name`.
    pub fn vector_index(&self, name: &str) -> Option<VectorIndexStatus> {
        let e = self.vector.indexes.read().get(name).cloned()?;
        Some(self.entry_status(&e, &self.snapshot()))
    }

    /// Measure index `name`'s recall@k against the exact search: `samples` stored
    /// vectors, spread evenly over the base rows, are the queries. `ef` overrides the
    /// index's `efSearch`.
    pub fn vector_recall(
        &self,
        name: &str,
        samples: usize,
        k: usize,
        ef: Option<usize>,
    ) -> Result<crate::vector::config::VectorRecall> {
        use crate::vector::{Search, SearchMode, search};
        let snap = self.snapshot();
        let b = snap
            .generation
            .vectors
            .built(name)
            .filter(|b| b.graph.is_some())
            .ok_or_else(|| {
                if self.vector.indexes.read().contains_key(name) {
                    Error::invalid(format!("vector index {name} has no graph ready"))
                } else {
                    Error::NotFound(format!("no vector index {name}"))
                }
            })?;
        let (Some(pred), true) = (b.pred, samples > 0 && k > 0) else {
            return Err(Error::invalid("the index is empty, or samples or k is 0"));
        };
        if let Some(ef) = ef {
            crate::vector::SearchMode::validate_ef(ef)?;
        }
        let seg = &b.segment;
        let rows = seg.rows();
        let samples = samples.min(rows).min(10_000);
        let all = |_: u64| true;
        let (mut hits, mut ann_ms, mut exact_ms, mut used_ef) = (0usize, 0.0, 0.0, 0);
        for j in 0..samples {
            let r = j * rows / samples;
            let query = seg.row(r).to_vec();
            let run = |exact: bool| {
                let t = std::time::Instant::now();
                let q = Search {
                    pred,
                    query: &query,
                    k,
                    metric: b.config.metric,
                    graph: &all,
                    dedup: false,
                    distinct_subject: false,
                    subjects: None,
                    mode: SearchMode { exact, ef },
                };
                search(&snap, &q, &|| Ok(())).map(|(h, i)| (h, i, t.elapsed().as_secs_f64() * 1e3))
            };
            let (truth, _, te) = match run(true) {
                Ok(x) => x,
                // a zero vector under cosine is no query
                Err(Error::Invalid(_)) => continue,
                Err(e) => return Err(e),
            };
            let (got, info, ta) = run(false)?;
            used_ef = used_ef.max(info.ef);
            exact_ms += te;
            ann_ms += ta;
            hits += got
                .iter()
                .filter(|g| truth.iter().any(|t| (t.s, t.o, t.g) == (g.s, g.o, g.g)))
                .count()
                .min(truth.len());
        }
        let n = samples.max(1) as f64;
        Ok(crate::vector::config::VectorRecall {
            k,
            samples,
            ef: used_ef,
            recall: hits as f64 / (n * k.min(rows) as f64),
            hnsw_ms: ann_ms / n,
            exact_ms: exact_ms / n,
        })
    }

    /// Test hook: hold (or release) background builds of vector indexes before they
    /// publish their result.
    #[doc(hidden)]
    pub fn pause_vector_builds(&self, on: bool) {
        self.vector.paused.store(on, Ordering::SeqCst);
    }

    /// Test hook: build the HNSW graphs of later builds one node at a time. A parallel
    /// build's graph depends on the order in which its threads insert nodes, and so on
    /// the machine's load; a sequential one is the same every time.
    #[doc(hidden)]
    pub fn sequential_vector_builds(&self, on: bool) {
        self.vector.sequential.store(on, Ordering::SeqCst);
    }

    fn entry_status(&self, e: &IndexEntry, snap: &Snapshot) -> VectorIndexStatus {
        let built = snap.generation.vectors.built(&e.name);
        let info = e.info.lock();
        let c = &e.config;
        let pred = snap.lookup_iri(&c.predicate).map(|i| i.0);
        let (inserts, deletes) = crate::vector::overlay_counts(snap, pred);
        let state = if info.state == "ready" && built.as_ref().is_none_or(|b| b.hnsw_pending) {
            "building"
        } else {
            info.state
        };
        VectorIndexStatus {
            name: e.name.clone(),
            predicate: c.predicate.clone(),
            dimension: c.dimension,
            metric: c.metric,
            model: c.model.clone(),
            state: state.into(),
            progress: (state == "building")
                .then(|| f32::from_bits(e.progress.load(Ordering::Relaxed))),
            message: info.message.clone(),
            generation: snap.generation.name.clone(),
            rows: built.as_ref().map_or(0, |b| b.segment.rows() as u64),
            overlay: VectorOverlay { inserts, deletes },
            skipped: built.as_ref().map(|b| b.skipped).unwrap_or_default(),
            memory: VectorMemory {
                segment_bytes: built.as_ref().map_or(0, |b| b.segment.bytes()),
                hnsw_bytes: built.as_ref().map_or(0, |b| {
                    b.graph.as_ref().map_or(0, |g| g.bytes()) + b.node_rows.len() as u64 * 4
                }),
                residency: if built.as_ref().is_some_and(|b| b.mapped()) {
                    "mmap"
                } else {
                    "heap"
                }
                .into(),
            },
            hnsw: c.hnsw.map(|h| HnswStatus {
                m: h.m,
                ef_construction: h.ef_construction,
                ef_search: h.ef_search,
                nodes: built
                    .as_ref()
                    .and_then(|b| b.graph.as_ref())
                    .map_or(0, |g| g.nodes as u64),
                layers: built
                    .as_ref()
                    .and_then(|b| b.graph.as_ref())
                    .map_or(0, |g| g.top + 1),
            }),
            exact_threshold: c.exact_threshold,
            files: built.as_ref().filter(|b| b.mapped()).map(|b| VectorFiles {
                bytes: b.file_bytes,
                opened: b.opened,
            }),
            last_build: info.last_build.clone(),
            embedding: self.embedding_status(&e.name),
        }
    }

    fn write_vector_config(&self, file: &VectorConfigFile) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let path = root.join(CONFIG_FILE);
        if file.indexes.is_empty() {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            }
        } else {
            super::write_atomic(
                &path,
                &serde_json::to_vec_pretty(file).expect("serializable"),
            )
        }
    }

    /// At open: read `vector.json` and start a build (from the index files when they fit)
    /// of every configured index.
    pub(super) fn open_vectors(&self) {
        let Some(root) = &self.root else {
            return;
        };
        let file = match std::fs::read(root.join(CONFIG_FILE)) {
            Ok(b) => match serde_json::from_slice::<VectorConfigFile>(&b)
                .map_err(|e| Error::invalid(format!("{CONFIG_FILE}: {e}")))
                .and_then(|f| f.validate().map(|()| f))
            {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!("vector indexes of {}: {e}", root.display());
                    return;
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::error!("vector indexes of {}: {e}", root.display());
                return;
            }
        };
        let entries: Vec<Arc<IndexEntry>> = file
            .indexes
            .iter()
            .map(|(n, c)| Arc::new(IndexEntry::new(n, c.clone())))
            .collect();
        *self.vector.indexes.write() = entries
            .iter()
            .map(|e| (e.name.clone(), e.clone()))
            .collect();
        let snap = self.snapshot();
        snap.generation.vectors.set_embedder(&self.embed);
        for (n, c) in &file.indexes {
            self.embed_configure(n, Some(c));
        }
        snap.generation
            .vectors
            .set_configured(Arc::new(file.indexes));
        for e in entries {
            self.spawn_vector_build(e, snap.clone(), true);
        }
    }

    /// After a generation switch (with the writer lock held): the replaced generation's
    /// files are written no more, and every index is built for the new one.
    pub(super) fn vectors_switched(&self, prev: &Snapshot) {
        prev.generation.vectors.retire();
        let entries: Vec<Arc<IndexEntry>> = self.vector.indexes.read().values().cloned().collect();
        if entries.is_empty() {
            return;
        }
        let snap = self.snapshot();
        snap.generation
            .vectors
            .set_configured(Arc::new(self.vector_configs()));
        snap.generation.vectors.set_embedder(&self.embed);
        for e in entries {
            self.spawn_vector_build(e, snap.clone(), true);
        }
    }

    /// The directory of `snap`'s generation's index files (`None`: an in-memory store).
    fn vector_dir(&self, snap: &Snapshot) -> Option<std::path::PathBuf> {
        let root = self.root.as_ref()?;
        let gdir = snap
            .generation
            .dir
            .as_ref()
            .filter(|d| d.starts_with(root))?;
        Some(persist::dir_of(gdir))
    }

    /// Build index `entry` for `snap`'s generation on a background thread (with `load`,
    /// from its file when it fits) and install it, unless it was superseded: by a newer
    /// build, a drop, a generation switch or the store's closing.
    fn spawn_vector_build(&self, entry: Arc<IndexEntry>, snap: Arc<Snapshot>, load: bool) {
        let epoch = entry.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut i = entry.info.lock();
            i.state = "building";
            i.message = None;
        }
        entry.progress.store(0f32.to_bits(), Ordering::Relaxed);
        let files = if persist::SUPPORTED {
            self.vector_dir(&snap).map(|dir| {
                let gdir = snap.generation.dir.clone().unwrap_or_default();
                (
                    dir,
                    Identity::of(
                        &gdir,
                        snap.generation.meta.quads,
                        snap.generation.meta.terms,
                        entry.config.build_hash(),
                    ),
                )
            })
        } else {
            None
        };
        let write = self.opts.vector_files;
        let sequential = self.vector.sequential.load(Ordering::SeqCst);
        let current = Arc::downgrade(&self.current);
        let registry = Arc::downgrade(&self.vector);
        let uid = snap.generation.uid;
        let failed = entry.clone();
        let spawned = std::thread::Builder::new()
            .name("vector-build".into())
            .spawn(move || {
                let superseded = || {
                    entry.retired.load(Ordering::SeqCst)
                        || entry.epoch.load(Ordering::SeqCst) != epoch
                        || current
                            .upgrade()
                            .is_none_or(|c| c.load().generation.uid != uid)
                };
                let progress = |f: f32| entry.progress.store(f.to_bits(), Ordering::Relaxed);
                let ctl = BuildCtl {
                    budget: crate::vector::budget(),
                    progress: &progress,
                    cancel: &superseded,
                    files,
                    load,
                    write,
                    sequential,
                };
                let gv = &snap.generation.vectors;
                let install = |b: Arc<Built>| {
                    // under the entry's lock, so no older build replaces a newer one
                    let _i = entry.info.lock();
                    if !superseded() {
                        gv.install(b);
                    }
                };
                let reading = snap.without_cache_fill();
                let t0 = std::time::Instant::now();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    build_index(&reading, &entry.name, &entry.config, &ctl, &install)
                }));
                drop(reading);
                while registry
                    .upgrade()
                    .is_some_and(|r| r.paused.load(Ordering::SeqCst))
                    && !superseded()
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                match r {
                    Ok(Ok(Outcome::Ready(b))) => {
                        let rows = b.segment.rows() as u64;
                        install(b);
                        tracing::info!(
                            "vector index {}: {rows} rows ready in {:?}",
                            entry.name,
                            t0.elapsed()
                        );
                        entry.finish(
                            epoch,
                            "ready",
                            None,
                            Some(VectorBuild {
                                at: crate::commit::rfc3339_ms(crate::commit::now_ms()),
                                ms: t0.elapsed().as_secs_f64() * 1000.0,
                                rows,
                            }),
                        );
                    }
                    Ok(Ok(Outcome::OverBudget(m))) => {
                        tracing::warn!("vector index {}: {m}", entry.name);
                        // searches scan exactly, as without a configuration
                        let _i = entry.info.lock();
                        if !superseded() {
                            gv.remove_built(&entry.name);
                        }
                        drop(_i);
                        entry.finish(epoch, "over-budget", Some(m), None);
                    }
                    Ok(Ok(Outcome::Cancelled)) => {
                        entry.done.notify_all();
                    }
                    Ok(Err(e)) => {
                        tracing::error!("vector index {} build failed: {e}", entry.name);
                        entry.finish(epoch, "failed", Some(format!("build failed: {e}")), None);
                    }
                    Err(p) => {
                        let m = p
                            .downcast_ref::<&str>()
                            .map(|s| s.to_string())
                            .or_else(|| p.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "panic".into());
                        tracing::error!("vector index {} build failed: {m}", entry.name);
                        entry.finish(epoch, "failed", Some(format!("build failed: {m}")), None);
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::error!("cannot start the vector index build: {e}");
            failed.finish(epoch, "failed", Some(format!("build failed: {e}")), None);
        }
    }
}
