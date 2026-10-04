//! Packed vectors of a generation: the implicit partitions packed on a predicate's first
//! search, and the configured indexes built in the background ([`build_index`]).

use super::config::{VectorIndexConfig, VectorSkipped};
use super::hnsw::{self, Graph};
use super::persist::{self, Identity, Mapped, Out, Problem, Slice};
use super::{Metric, from_key, graph_dist, norm};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::store::Snapshot;
use parking_lot::{Mutex, RwLock};
use rustc_hash::FxHashMap;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Packed vectors of one dimension: rows in PSO order, so the rows of one subject are
/// adjacent, and so are those of one (subject, vector) pair in several graphs.
#[derive(Default)]
pub struct Segment {
    pub dim: usize,
    /// (s, o, g) per row
    pub ids: Slice<[u64; 3]>,
    pub norms: Slice<f32>,
    /// row-major, `dim` values per row
    pub data: Slice<f32>,
}

impl Segment {
    pub fn rows(&self) -> usize {
        self.ids.len()
    }

    #[inline]
    pub fn row(&self, i: usize) -> &[f32] {
        &self.data[i * self.dim..(i + 1) * self.dim]
    }

    pub fn bytes(&self) -> u64 {
        (self.data.len() * 4 + self.norms.len() * 4 + self.ids.len() * 24) as u64
    }

    /// The end of the run of rows from `start` with its (subject, vector) pair.
    #[inline]
    pub fn run_end(&self, start: usize) -> usize {
        let [s, o, _] = self.ids[start];
        let mut e = start + 1;
        while e < self.ids.len() && self.ids[e][0] == s && self.ids[e][1] == o {
            e += 1;
        }
        e
    }

    /// The rows of subject `s`.
    pub fn subject_rows(&self, s: u64) -> std::ops::Range<usize> {
        let lo = self.ids.partition_point(|r| r[0] < s);
        let hi = lo + self.ids[lo..].partition_point(|r| r[0] == s);
        lo..hi
    }
}

/// The base-index vectors of one predicate, by dimension (an implicit partition).
pub struct PredicateVectors {
    pub by_dim: FxHashMap<usize, Segment>,
    /// literals of the vector datatype that are not valid vectors
    pub malformed: u64,
    pub bytes: u64,
}

/// One packed predicate, for status reports.
pub struct PackedStatus {
    pub predicate: u64,
    pub bytes: u64,
    pub malformed: u64,
    /// (dimension, rows): a vector in several graphs counts once per graph
    pub dims: Vec<(usize, usize)>,
}

/// A configured index as built for one generation.
pub struct Built {
    pub name: String,
    pub config: VectorIndexConfig,
    /// the predicate's id in the generation (`None`: not in it, so no rows)
    pub pred: Option<u64>,
    pub segment: Arc<Segment>,
    /// the HNSW graph over the nodes (`None`: none configured, or still being built)
    pub graph: Option<Arc<Graph>>,
    /// the first row of each graph node's (subject, vector) run
    pub node_rows: Arc<Slice<u32>>,
    /// rows per graph id, for the selectivity of a graph filter
    pub graph_rows: Arc<Vec<(u64, u64)>>,
    pub skipped: VectorSkipped,
    pub built_ms: f64,
    /// read from a file written before
    pub opened: bool,
    pub file_bytes: u64,
    /// the segment is usable and the graph is still being built
    pub hnsw_pending: bool,
}

impl Built {
    pub fn bytes(&self) -> u64 {
        self.segment.bytes()
            + self.graph.as_ref().map_or(0, |g| g.bytes())
            + self.node_rows.len() as u64 * 4
    }

    pub fn mapped(&self) -> bool {
        self.segment.ids.is_mapped()
    }

    /// Rows whose graph `graph` accepts (an upper bound once deletions are subtracted).
    pub fn rows_in(&self, graph: &dyn Fn(u64) -> bool) -> u64 {
        self.graph_rows
            .iter()
            .filter(|(g, _)| graph(*g))
            .map(|(_, n)| n)
            .sum()
    }
}

/// Per-generation vector state.
#[derive(Default)]
pub struct GenerationVectors {
    by_pred: Mutex<FxHashMap<u64, Arc<PredicateVectors>>>,
    /// configured indexes built (or being built) for this generation, by name
    built: RwLock<BTreeMap<String, Arc<Built>>>,
    /// the dataset's configured indexes (the store keeps it current)
    configured: RwLock<Arc<BTreeMap<String, VectorIndexConfig>>>,
    /// parsed vectors of the delta's literals, by id (ids are stable in a generation)
    overlay: Mutex<OverlayCache>,
    /// the generation was replaced: its index files are written no more
    retired: Mutex<bool>,
    /// the store's embedding state (environment and cache), for searches with text
    embedder: RwLock<Option<std::sync::Weak<super::embed::Embedder>>>,
}

/// Parsed delta vectors by literal id, and their bytes.
type OverlayCache = (FxHashMap<u64, Option<Arc<[f32]>>>, u64);

/// Rows of one dimension being packed: ids, norms and values.
type Packing = (Vec<[u64; 3]>, Vec<f32>, Vec<f32>);

/// Bytes of parsed delta vectors a generation keeps before it starts over.
const OVERLAY_CACHE_BYTES: u64 = 64 << 20;

impl GenerationVectors {
    /// The predicates packed so far (on their first search), by predicate id.
    pub fn status(&self) -> Vec<PackedStatus> {
        let mut out: Vec<PackedStatus> = self
            .by_pred
            .lock()
            .iter()
            .map(|(&p, v)| {
                let mut dims: Vec<(usize, usize)> =
                    v.by_dim.values().map(|s| (s.dim, s.rows())).collect();
                dims.sort_unstable();
                PackedStatus {
                    predicate: p,
                    bytes: v.bytes,
                    malformed: v.malformed,
                    dims,
                }
            })
            .collect();
        out.sort_unstable_by_key(|s| s.predicate);
        out
    }

    /// Bytes of packed vectors and graphs (mapped files count as resident).
    pub fn used_bytes(&self) -> u64 {
        self.by_pred.lock().values().map(|p| p.bytes).sum::<u64>()
            + self.built.read().values().map(|b| b.bytes()).sum::<u64>()
    }

    /// Bytes in use apart from index `name` and the implicit partition of `pred`, which
    /// a build of that index replaces.
    fn used_bytes_without(&self, name: &str, pred: Option<u64>) -> u64 {
        self.by_pred
            .lock()
            .iter()
            .filter(|(p, _)| Some(**p) != pred)
            .map(|(_, v)| v.bytes)
            .sum::<u64>()
            + self
                .built
                .read()
                .iter()
                .filter(|(n, _)| n.as_str() != name)
                .map(|(_, b)| b.bytes())
                .sum::<u64>()
    }

    pub fn set_configured(&self, c: Arc<BTreeMap<String, VectorIndexConfig>>) {
        *self.configured.write() = c;
    }

    pub fn configured(&self) -> Arc<BTreeMap<String, VectorIndexConfig>> {
        self.configured.read().clone()
    }

    pub(crate) fn set_embedder(&self, e: &Arc<super::embed::Embedder>) {
        *self.embedder.write() = Some(Arc::downgrade(e));
    }

    /// The store's embedding state, while the store is open.
    pub(crate) fn embedder(&self) -> Option<Arc<super::embed::Embedder>> {
        self.embedder.read().as_ref().and_then(|w| w.upgrade())
    }

    /// The configured index of predicate `pred` (an id of `snap`), if any.
    pub fn configured_for(
        &self,
        snap: &Snapshot,
        pred: u64,
    ) -> Option<(String, VectorIndexConfig)> {
        let c = self.configured();
        c.iter()
            .find(|(_, c)| snap.lookup_iri(&c.predicate).map(|i| i.0) == Some(pred))
            .map(|(n, c)| (n.clone(), c.clone()))
    }

    /// Index `name` as built for this generation.
    pub fn built(&self, name: &str) -> Option<Arc<Built>> {
        self.built.read().get(name).cloned()
    }

    /// Install a build of an index (replacing the previous one); the implicit partition
    /// of its predicate goes, as the index's segment replaces it.
    pub(crate) fn install(&self, b: Arc<Built>) {
        if let Some(p) = b.pred {
            self.by_pred.lock().remove(&p);
        }
        self.built.write().insert(b.name.clone(), b);
    }

    pub(crate) fn remove_built(&self, name: &str) -> Option<Arc<Built>> {
        self.built.write().remove(name)
    }

    /// Mark the generation replaced: no index file is written for it any more.
    pub(crate) fn retire(&self) {
        *self.retired.lock() = true;
    }

    /// Run `f` (a write or removal of the generation's files) unless it was replaced.
    pub(crate) fn with_files<R>(&self, f: impl FnOnce() -> R) -> Option<R> {
        let retired = self.retired.lock();
        (!*retired).then(f)
    }

    /// The vector of a delta literal (`None`: not a well-typed vector), parsed once per
    /// generation.
    pub(crate) fn overlay_vector(&self, snap: &Snapshot, o: u64) -> Option<Arc<[f32]>> {
        if let Some(v) = self.overlay.lock().0.get(&o) {
            return v.clone();
        }
        let v: Option<Arc<[f32]>> = snap.key(Id(o)).and_then(|k| from_key(&k)).map(Arc::from);
        let mut c = self.overlay.lock();
        let add = v.as_ref().map_or(8, |v| v.len() as u64 * 4 + 32);
        if c.1 + add > OVERLAY_CACHE_BYTES {
            c.0.clear();
            c.1 = 0;
        }
        c.1 += add;
        c.0.insert(o, v.clone());
        v
    }

    /// The base vectors of predicate `p` in `snap`'s generation, built on first use
    /// within the memory budget.
    pub fn predicate(&self, snap: &Snapshot, p: u64, budget: u64) -> Result<Arc<PredicateVectors>> {
        if let Some(v) = self.by_pred.lock().get(&p) {
            return Ok(v.clone());
        }
        let packed = pack(snap, p, None, &|_| {}, &|| false)?;
        let bytes: u64 = packed.by_dim.values().map(Segment::bytes).sum();
        let used = self.used_bytes();
        if used + bytes > budget {
            // the packed vectors of every predicate share one budget
            return Err(Error::BudgetExceeded(crate::Budget {
                kind: crate::BudgetKind::Memory,
                limit: budget,
                requested: used + bytes,
            }));
        }
        let pv = Arc::new(PredicateVectors {
            by_dim: packed.by_dim,
            malformed: packed.malformed,
            bytes,
        });
        self.by_pred.lock().insert(p, pv.clone());
        Ok(pv)
    }
}

/// What [`pack`] found: segments by dimension and the skipped rows.
struct Packed {
    by_dim: FxHashMap<usize, Segment>,
    /// literals (implicit partitions) or rows (configured indexes) of the datatype that
    /// are not valid vectors
    malformed: u64,
    wrong_dim: u64,
}

/// Literals parsed per batch while packing.
const PARSE_BATCH: usize = 1 << 15;

/// Pack the base rows of predicate `p`: every dimension, or only `only`.
fn pack(
    snap: &Snapshot,
    p: u64,
    only: Option<usize>,
    progress: &dyn Fn(f32),
    cancel: &dyn Fn() -> bool,
) -> Result<Packed> {
    use rayon::prelude::*;
    let base = snap.perm(Perm::Pso);
    let rows = base.count(&snap.cache, &[p])?;
    let mut keys: Vec<[u64; 3]> = Vec::with_capacity(rows as usize);
    base.for_each_range(&snap.cache, &[p], |b, s, e| {
        for i in s..e {
            let k = b.key(i);
            keys.push([k[1], k[2], k[3]]);
        }
        Ok(())
    })?;
    /// What a literal is to the packing.
    enum Lit {
        Vector(Vec<f32>),
        /// a vector of another dimension than the configured one
        Wrong,
        /// of the vector datatype, but not a valid vector
        Malformed,
    }
    // rows in batches: each batch's literals are parsed in parallel and copied into the
    // segments in row order, so only one batch of parsed vectors is held at a time (a
    // literal in rows of several batches is parsed once per batch)
    let mut by_dim: FxHashMap<usize, Packing> = FxHashMap::default();
    if let Some(d) = only {
        // a configured index: room for every row at once, not doubling as it grows
        let n = keys.len();
        by_dim.insert(
            d,
            (
                Vec::with_capacity(n),
                Vec::with_capacity(n),
                Vec::with_capacity(n * d),
            ),
        );
    }
    let (mut malformed_rows, mut wrong_dim) = (0u64, 0u64);
    let mut malformed_lits: rustc_hash::FxHashSet<u64> = Default::default();
    let total = keys.len().max(1);
    for (bi, batch) in keys.chunks(PARSE_BATCH).enumerate() {
        if cancel() {
            return Err(Error::Cancelled);
        }
        let mut objs: Vec<u64> = batch
            .iter()
            .map(|k| k[1])
            .filter(|&o| Id(o).tag() == Tag::Vocab)
            .collect();
        objs.sort_unstable();
        objs.dedup();
        let payloads: Vec<u64> = objs.iter().map(|&o| Id(o).payload()).collect();
        let mut raw: Vec<(u64, Vec<u8>)> = Vec::with_capacity(objs.len());
        snap.generation.vocab.get_sorted(&payloads, |pl, key| {
            // only literals of the vector datatype
            if super::is_vector_key(key) {
                raw.push((pl, key.to_vec()));
            }
        });
        let parsed: FxHashMap<u64, Lit> = raw
            .par_iter()
            .map(|(pl, key)| {
                let lit = match from_key(key) {
                    None => Lit::Malformed,
                    Some(v) if only.is_some_and(|d| v.len() != d) => Lit::Wrong,
                    Some(v) => Lit::Vector(v),
                };
                (Id::vocab(*pl).0, lit)
            })
            .collect();
        drop(raw);
        for k in batch {
            match parsed.get(&k[1]) {
                Some(Lit::Vector(v)) => {
                    let (ids, norms, data) = by_dim.entry(v.len()).or_default();
                    ids.push(*k);
                    norms.push(norm(v));
                    data.extend_from_slice(v);
                }
                Some(Lit::Wrong) => wrong_dim += 1,
                Some(Lit::Malformed) => {
                    malformed_rows += 1;
                    malformed_lits.insert(k[1]);
                }
                None => {}
            }
        }
        progress(((bi + 1) * PARSE_BATCH).min(total) as f32 / total as f32);
    }
    let malformed_lits = malformed_lits.len() as u64;
    Ok(Packed {
        by_dim: by_dim
            .into_iter()
            .map(|(d, (ids, norms, data))| {
                (
                    d,
                    Segment {
                        dim: d,
                        ids: Slice::Owned(ids),
                        norms: Slice::Owned(norms),
                        data: Slice::Owned(data),
                    },
                )
            })
            .collect(),
        malformed: if only.is_some() {
            malformed_rows
        } else {
            malformed_lits
        },
        wrong_dim,
    })
}

/// The nodes of a graph over `seg`: the first row of each (subject, vector) run, except
/// rows a metric cannot score (zero norms under cosine).
fn nodes_of(seg: &Segment, metric: Metric) -> Vec<u32> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < seg.rows() {
        if !(metric == Metric::Cosine && seg.norms[i] == 0.0) {
            out.push(i as u32);
        }
        i = seg.run_end(i);
    }
    out
}

struct SegSpace<'a> {
    seg: &'a Segment,
    nodes: &'a [u32],
    metric: Metric,
}

impl hnsw::Space for SegSpace<'_> {
    fn len(&self) -> usize {
        self.nodes.len()
    }
    fn dist(&self, a: u32, b: u32) -> f32 {
        let (ra, rb) = (
            self.nodes[a as usize] as usize,
            self.nodes[b as usize] as usize,
        );
        graph_dist(
            self.metric,
            self.seg.row(ra),
            self.seg.norms[ra],
            self.seg.row(rb),
            self.seg.norms[rb],
        )
    }
}

/// The rows per graph of a segment.
fn graph_rows(seg: &Segment) -> Vec<(u64, u64)> {
    let mut m: FxHashMap<u64, u64> = FxHashMap::default();
    for r in seg.ids.iter() {
        *m.entry(r[2]).or_default() += 1;
    }
    let mut v: Vec<(u64, u64)> = m.into_iter().collect();
    v.sort_unstable();
    v
}

/// How a build reports and stops, and where its file goes.
pub(crate) struct BuildCtl<'a> {
    /// memory all packed vectors and graphs may use
    pub budget: u64,
    /// build progress, 0–1
    pub progress: &'a (dyn Fn(f32) + Sync),
    pub cancel: &'a (dyn Fn() -> bool + Sync),
    /// the generation's index directory and what its files must match (persistent
    /// stores)
    pub files: Option<(PathBuf, Identity)>,
    /// read the index from its file when it fits
    pub load: bool,
    /// write the index's file after a build
    pub write: bool,
    /// build the graph one node at a time (see [`hnsw::Params::sequential`])
    pub sequential: bool,
}

/// How a build ended.
pub(crate) enum Outcome {
    Ready(Arc<Built>),
    /// the index would exceed the memory budget (the message says by how much)
    OverBudget(String),
    /// superseded by a newer build, a generation switch or a drop
    Cancelled,
}

/// The seed of every graph build (builds are reproducible).
const SEED: u64 = 0x5eed_5eed;

/// Build index `name` for `snap`'s generation from its base: read it from its file when
/// `ctl.load` and the file fits, else pack the predicate's vectors, publish them through
/// `partial` (searches can use the packed rows while the graph is built), build the
/// graph, and write the file.
pub(crate) fn build_index(
    snap: &Snapshot,
    name: &str,
    cfg: &VectorIndexConfig,
    ctl: &BuildCtl<'_>,
    partial: &dyn Fn(Arc<Built>),
) -> Result<Outcome> {
    let t0 = std::time::Instant::now();
    let pred = snap.lookup_iri(&cfg.predicate).map(|i| i.0);
    let gv = &snap.generation.vectors;
    if let (Some((dir, ident)), true) = (&ctl.files, ctl.load) {
        let path = persist::file_of(dir, name);
        match read_file(&path, ident, name, cfg, pred) {
            Ok(b) => return Ok(Outcome::Ready(Arc::new(b))),
            Err(Problem::Missing) => {}
            Err(p) => {
                tracing::warn!(
                    "vector index file {}: {p}; it is built again",
                    path.display()
                );
                if ctl.write {
                    gv.with_files(|| persist::remove(dir, Some(name)));
                }
            }
        }
    }
    let cancelled = || (ctl.cancel)();
    let hnsw_share = if cfg.hnsw.is_some() { 0.3 } else { 1.0 };
    let (segment, skipped) = match pred {
        None => (
            Segment {
                dim: cfg.dimension,
                ..Default::default()
            },
            VectorSkipped::default(),
        ),
        Some(p) => {
            let mut packed = match pack(
                snap,
                p,
                Some(cfg.dimension),
                &|f| (ctl.progress)(f * hnsw_share),
                &cancelled,
            ) {
                Ok(x) => x,
                Err(Error::Cancelled) => return Ok(Outcome::Cancelled),
                Err(e) => return Err(e),
            };
            let seg = packed.by_dim.remove(&cfg.dimension).unwrap_or(Segment {
                dim: cfg.dimension,
                ..Default::default()
            });
            let zero = seg.norms.iter().filter(|&&n| n == 0.0).count() as u64;
            (
                seg,
                VectorSkipped {
                    malformed: packed.malformed,
                    wrong_dimension: packed.wrong_dim,
                    zero_norm: zero,
                },
            )
        }
    };
    if cancelled() {
        return Ok(Outcome::Cancelled);
    }
    let nodes = match cfg.hnsw {
        Some(_) => nodes_of(&segment, cfg.metric),
        None => Vec::new(),
    };
    let graph_estimate = cfg.hnsw.map_or(0, |h| {
        nodes.len() as u64 * ((2 * h.m as u64 + 1) * 4 + 4) * 11 / 10
    });
    let need = segment.bytes() + graph_estimate;
    let used = gv.used_bytes_without(name, pred);
    if used + need > ctl.budget {
        return Ok(Outcome::OverBudget(format!(
            "index {name} needs {need} bytes; {} of {} bytes are free (--vector-memory-mb)",
            ctl.budget.saturating_sub(used),
            ctl.budget
        )));
    }
    let segment = Arc::new(segment);
    let graph_rows = Arc::new(graph_rows(&segment));
    let mut built = Built {
        name: name.to_string(),
        config: cfg.clone(),
        pred,
        segment: segment.clone(),
        graph: None,
        node_rows: Arc::new(Slice::Owned(Vec::new())),
        graph_rows,
        skipped,
        built_ms: 0.0,
        opened: false,
        file_bytes: 0,
        hnsw_pending: cfg.hnsw.is_some(),
    };
    if let Some(h) = cfg.hnsw {
        partial(Arc::new(Built {
            node_rows: Arc::new(Slice::Owned(Vec::new())),
            ..clone_meta(&built)
        }));
        let space = SegSpace {
            seg: &segment,
            nodes: &nodes,
            metric: cfg.metric,
        };
        let n = nodes.len().max(1);
        let progress =
            |d: usize| (ctl.progress)(hnsw_share + (1.0 - hnsw_share) * d as f32 / n as f32);
        let params = hnsw::Params {
            m: h.m,
            ef_construction: h.ef_construction,
            seed: SEED,
            sequential: ctl.sequential,
        };
        let Some(g) = Graph::build(
            &space,
            &params,
            &hnsw::Ctl {
                progress: &progress,
                cancel: &cancelled,
            },
        ) else {
            return Ok(Outcome::Cancelled);
        };
        built.graph = Some(Arc::new(g));
        built.node_rows = Arc::new(Slice::Owned(nodes));
        built.hnsw_pending = false;
    }
    built.built_ms = t0.elapsed().as_secs_f64() * 1000.0;
    if let (Some((dir, ident)), true) = (&ctl.files, ctl.write) {
        let path = persist::file_of(dir, name);
        let written = gv.with_files(|| {
            if cancelled() {
                return None;
            }
            let r = write_file(&path, ident, &built)
                .map_err(|e| e.to_string())
                .and_then(|()| read_file(&path, ident, name, cfg, pred).map_err(|p| p.to_string()));
            if r.is_err() {
                persist::remove(dir, Some(name));
            }
            Some(r)
        });
        match written.flatten() {
            Some(Ok(mut b)) => {
                b.opened = false;
                b.built_ms = built.built_ms;
                return Ok(Outcome::Ready(Arc::new(b)));
            }
            Some(Err(e)) => tracing::warn!(
                "cannot write vector index file {}: {e}; the index stays in memory",
                path.display()
            ),
            None => {}
        }
    }
    Ok(Outcome::Ready(Arc::new(built)))
}

/// A copy of `b` sharing its parts.
fn clone_meta(b: &Built) -> Built {
    Built {
        name: b.name.clone(),
        config: b.config.clone(),
        pred: b.pred,
        segment: b.segment.clone(),
        graph: b.graph.clone(),
        node_rows: b.node_rows.clone(),
        graph_rows: b.graph_rows.clone(),
        skipped: b.skipped,
        built_ms: b.built_ms,
        opened: b.opened,
        file_bytes: b.file_bytes,
        hnsw_pending: b.hnsw_pending,
    }
}

impl Built {
    /// The same build with another configuration of equal [`build_hash`] (a new
    /// `efSearch` or exact threshold).
    ///
    /// [`build_hash`]: VectorIndexConfig::build_hash
    pub(crate) fn reconfigured(&self, cfg: &VectorIndexConfig) -> Built {
        Built {
            config: cfg.clone(),
            ..clone_meta(self)
        }
    }
}

/// Words of the meta section before the rows per graph.
const META_WORDS: usize = 10;

fn write_file(path: &std::path::Path, ident: &Identity, b: &Built) -> Result<()> {
    let seg = &b.segment;
    let g = b.graph.as_deref();
    let mut meta: Vec<u64> = vec![
        seg.dim as u64,
        b.skipped.malformed,
        b.skipped.wrong_dimension,
        b.skipped.zero_norm,
        b.pred.unwrap_or(u64::MAX),
        g.is_some() as u64,
        g.map_or(0, |g| g.m as u64),
        g.map_or(0, |g| g.entry as u64),
        g.map_or(0, |g| g.top as u64),
        g.map_or(0, |g| g.nodes as u64),
    ];
    meta.push(b.graph_rows.len() as u64);
    for (gid, n) in b.graph_rows.iter() {
        meta.extend([*gid, *n]);
    }
    let mut sections = vec![
        Out::of(&meta),
        Out::of(&seg.ids),
        Out::of(&seg.norms),
        Out::of(&seg.data),
    ];
    if let Some(g) = g {
        sections.push(Out::of(&b.node_rows));
        sections.push(Out::of(&g.level0));
        for (ns, ls) in &g.upper {
            sections.push(Out::of(ns));
            sections.push(Out::of(ls));
        }
    }
    persist::write(path, ident, seg.rows() as u64, &sections)
}

fn read_file(
    path: &std::path::Path,
    ident: &Identity,
    name: &str,
    cfg: &VectorIndexConfig,
    pred: Option<u64>,
) -> std::result::Result<Built, Problem> {
    let bad = |m: &str| Problem::Unusable(m.to_string());
    let f = Mapped::open(path, Some(ident), false)?;
    let meta: Slice<u64> = f.slice(0).ok_or_else(|| bad("bad meta section"))?;
    if meta.len() < META_WORDS + 1 {
        return Err(bad("short meta section"));
    }
    let dim = meta[0] as usize;
    let file_pred = (meta[4] != u64::MAX).then_some(meta[4]);
    if dim != cfg.dimension || file_pred != pred {
        return Err(bad("built for another predicate or dimension"));
    }
    let ngraphs = meta[META_WORDS] as usize;
    if meta.len() != META_WORDS + 1 + 2 * ngraphs {
        return Err(bad("bad meta section"));
    }
    let graph_rows: Vec<(u64, u64)> = (0..ngraphs)
        .map(|i| (meta[META_WORDS + 1 + 2 * i], meta[META_WORDS + 2 + 2 * i]))
        .collect();
    let rows = f.rows as usize;
    let seg = Segment {
        dim,
        ids: f.slice(1).ok_or_else(|| bad("bad ids section"))?,
        norms: f.slice(2).ok_or_else(|| bad("bad norms section"))?,
        data: f.slice(3).ok_or_else(|| bad("bad data section"))?,
    };
    if seg.ids.len() != rows || seg.norms.len() != rows || seg.data.len() != rows * dim {
        return Err(bad("sections disagree with the row count"));
    }
    let has_graph = meta[5] == 1;
    if has_graph != cfg.hnsw.is_some() {
        return Err(bad("built with another graph setting"));
    }
    let (graph, node_rows) = if has_graph {
        let (m, entry, top, nodes) = (
            meta[6] as usize,
            meta[7] as u32,
            meta[8] as usize,
            meta[9] as usize,
        );
        if f.sections() != 6 + 2 * top {
            return Err(bad("bad graph sections"));
        }
        let node_rows: Slice<u32> = f.slice(4).ok_or_else(|| bad("bad node section"))?;
        if node_rows.len() != nodes || node_rows.iter().any(|&r| r as usize >= rows) {
            return Err(bad("bad node section"));
        }
        let mut upper = Vec::with_capacity(top);
        for l in 0..top {
            upper.push((
                f.slice(6 + 2 * l).ok_or_else(|| bad("bad graph layer"))?,
                f.slice(7 + 2 * l).ok_or_else(|| bad("bad graph layer"))?,
            ));
        }
        let g = Graph {
            m,
            entry,
            top,
            nodes,
            level0: f.slice(5).ok_or_else(|| bad("bad graph layer"))?,
            upper,
        };
        if !g.check() {
            return Err(bad("damaged graph"));
        }
        (Some(Arc::new(g)), node_rows)
    } else {
        if f.sections() != 4 {
            return Err(bad("unexpected sections"));
        }
        (None, Slice::Owned(Vec::new()))
    };
    let zero = seg.norms.iter().filter(|&&n| n == 0.0).count() as u64;
    Ok(Built {
        name: name.to_string(),
        config: cfg.clone(),
        pred,
        segment: Arc::new(seg),
        graph,
        node_rows: Arc::new(node_rows),
        graph_rows: Arc::new(graph_rows),
        skipped: VectorSkipped {
            malformed: meta[1],
            wrong_dimension: meta[2],
            zero_norm: meta[3].max(zero),
        },
        built_ms: 0.0,
        opened: true,
        file_bytes: f.file_len(),
        hnsw_pending: false,
    })
}

/// Check a generation's vector index files (`sparkles check`): every `*.spkv` must open,
/// with its data checksum when `full`. Returns (file name, problem) per bad file.
pub fn check_files(gen_dir: &std::path::Path, full: bool) -> Vec<(String, String)> {
    let dir = persist::dir_of(gen_dir);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == persist::EXT)
            && let Err(problem) = Mapped::open(&p, None, full)
        {
            out.push((
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                problem.to_string(),
            ));
        }
    }
    out
}
