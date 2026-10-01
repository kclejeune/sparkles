//! The spatial index: a packed R-tree over a generation's base rows, an overlay of rows
//! inserted by transactions since, and the view a snapshot holds of both.
//!
//! * The **base** ([`GeoBase`]) is built from the generation's PSO index for the
//!   configured predicates: one row per base quad whose object is an indexable geometry,
//!   and a Hilbert-packed R-tree (`f32` boxes rounded outward, 16-entry nodes) over them.
//! * The **overlay** holds rows inserted by commits since, in a packed tree of its own,
//!   and the **tail** the rows inserted since that tree was built (a persistent vector, so
//!   each snapshot keeps its own cheaply).
//! * A row is valid for a snapshot S iff it is a base row whose quad is not in
//!   `S.delta.del`, or an overlay or tail row whose quad is in `S.delta.ins`: since the
//!   delta keeps `ins` disjoint from the base and `del` within it, the rows of S are
//!   exactly `(base − del) ∪ (overlay ∪ tail) ∩ ins`.

use super::column::{Column, ColumnEntry, Counts, Slot};
use super::config::{
    FORMAT_VERSION, GeoBuild, GeoConfig, GeoMemory, GeoRows, GeoSkipped, GeoStatus, IndexState,
};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::{Key, Perm};
use crate::store::Snapshot;
use crate::text::PredicateSet;
use geo_index::rtree::sort::HilbertSort;
use geo_index::rtree::{RTree, RTreeBuilder, RTreeIndex};
use parking_lot::{Condvar, Mutex, RwLock};
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Entries per tree node.
pub(crate) const NODE_SIZE: u16 = 16;

/// An indexed quad (raw ids).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Row {
    pub s: u64,
    pub p: u64,
    pub o: u64,
    pub g: u64,
}

impl Row {
    pub fn of(q: &[Id; 4]) -> Row {
        Row {
            s: q[0].0,
            p: q[1].0,
            o: q[2].0,
            g: q[3].0,
        }
    }

    /// The row's key in the PSO permutation.
    #[inline]
    pub fn pso(&self) -> Key {
        [self.p, self.s, self.o, self.g]
    }
}

/// A row inserted by a commit, with its geometry.
#[derive(Clone)]
pub(crate) struct TailRow {
    pub row: Row,
    pub entry: Arc<ColumnEntry>,
}

/// Pack boxes into a Hilbert-sorted R-tree (`None` without boxes); the tree's item `i`
/// is the `i`-th box.
pub(crate) fn pack(boxes: impl ExactSizeIterator<Item = [f32; 4]>) -> Option<RTree<f32>> {
    let n = boxes.len();
    if n == 0 {
        return None;
    }
    let mut b = RTreeBuilder::<f32>::new_with_node_size(n as u32, NODE_SIZE);
    for x in boxes {
        b.add(x[0], x[1], x[2], x[3]);
    }
    Some(b.finish::<HilbertSort>())
}

fn tree_bytes(t: &Option<RTree<f32>>) -> u64 {
    t.as_ref()
        .map_or(0, |t| t.metadata().data_buffer_length() as u64)
}

/// Whether the `f32` box `b` intersects the CRS84 window `w`.
#[inline]
pub(crate) fn intersects(b: &[f32; 4], w: &[f64; 4]) -> bool {
    f64::from(b[0]) <= w[2]
        && f64::from(b[2]) >= w[0]
        && f64::from(b[1]) <= w[3]
        && f64::from(b[3]) >= w[1]
}

/// The window as tree coordinates, rounded outward (so no box touching it is missed).
pub(crate) fn window_f32(w: &[f64; 4]) -> [f32; 4] {
    super::column::round_out(*w)
}

/// Estimated rows of `tree` whose box intersects one of `windows`: the subtree sizes of
/// the intersecting nodes of the highest level with at least 256 nodes (an upper bound
/// within one node per window boundary).
fn tree_estimate(tree: &RTree<f32>, windows: &[[f64; 4]]) -> f64 {
    let n = tree.num_items() as usize;
    let ws: Vec<[f32; 4]> = windows.iter().map(window_f32).collect();
    let levels = tree.num_levels();
    let mut level = 0;
    for l in (0..levels).rev() {
        if tree.boxes_at_level(l).map_or(0, |b| b.len() / 4) >= 256 {
            level = l;
            break;
        }
    }
    let Ok(boxes) = tree.boxes_at_level(level) else {
        return n as f64;
    };
    let span = (NODE_SIZE as usize).saturating_pow(level as u32);
    let mut total = 0usize;
    for (j, b) in boxes.as_chunks::<4>().0.iter().enumerate() {
        let hit = ws
            .iter()
            .any(|w| b[0] <= w[2] && b[2] >= w[0] && b[1] <= w[3] && b[3] >= w[1]);
        if hit {
            let lo = j.saturating_mul(span);
            total += j.saturating_add(1).saturating_mul(span).min(n) - lo.min(n);
        }
    }
    total as f64
}

/// Predicate and graph decisions of one generation and configuration, cached by id: the
/// commit path pays one lookup per logged quad.
pub(crate) struct Lookup {
    /// predicate id → slot (index in the configuration's predicates), or not indexed
    preds: RwLock<FxHashMap<u64, Option<u16>>>,
    /// graph id → in scope (empty when every graph is)
    graphs: RwLock<FxHashMap<u64, bool>>,
    every_graph: bool,
}

impl Lookup {
    /// The lookup for `snap`'s generation, with the configured predicates that `snap`
    /// knows resolved.
    pub fn new(snap: &Snapshot, cfg: &GeoConfig) -> Lookup {
        let l = Lookup::empty(cfg);
        l.resolve(snap, cfg);
        l
    }

    /// Learn the ids `snap` has for the configured predicates (a predicate first used
    /// after the lookup was made has a delta id it does not know yet).
    pub fn resolve(&self, snap: &Snapshot, cfg: &GeoConfig) {
        let mut preds = self.preds.write();
        for (i, iri) in cfg.predicates.iter().enumerate() {
            if let Some(id) = snap.lookup_iri(iri) {
                preds.insert(id.0, Some(i as u16));
            }
        }
    }

    /// A lookup that knows no ids yet.
    pub fn empty(cfg: &GeoConfig) -> Lookup {
        Lookup {
            preds: RwLock::new(FxHashMap::default()),
            graphs: RwLock::new(FxHashMap::default()),
            every_graph: matches!(cfg.graphs.include, PredicateSet::All)
                && cfg.graphs.exclude.is_empty(),
        }
    }

    /// The slot of predicate `p` if it is indexed; `snap` decodes unseen ids.
    #[inline]
    pub fn slot(&self, p: Id, snap: &Snapshot, cfg: &GeoConfig) -> Option<u16> {
        if let Some(&s) = self.preds.read().get(&p.0) {
            return s;
        }
        let s = match snap.key(p) {
            Some(k) if k.first() == Some(&b'<') => cfg
                .predicates
                .iter()
                .position(|x| x.as_bytes() == &k[1..])
                .map(|i| i as u16),
            _ => None,
        };
        self.preds.write().insert(p.0, s);
        s
    }

    /// The slot of `p`, from what is known already.
    pub fn known_slot(&self, p: Id) -> Option<u16> {
        self.preds.read().get(&p.0).copied().flatten()
    }

    /// The ids known for the configured predicates.
    pub fn indexed(&self) -> Vec<(u64, u16)> {
        let mut v: Vec<(u64, u16)> = self
            .preds
            .read()
            .iter()
            .filter_map(|(&p, s)| s.map(|s| (p, s)))
            .collect();
        v.sort_unstable();
        v
    }

    /// Whether quads of graph `g` are indexed.
    #[inline]
    pub fn graph(&self, g: Id, snap: &Snapshot, cfg: &GeoConfig) -> bool {
        if self.every_graph {
            return true;
        }
        if let Some(&b) = self.graphs.read().get(&g.0) {
            return b;
        }
        let b = graph_name(snap, g).is_some_and(|n| cfg.graph_in_scope(&n));
        self.graphs.write().insert(g.0, b);
        b
    }
}

fn graph_name(snap: &Snapshot, g: Id) -> Option<String> {
    if g == Id::DEFAULT_GRAPH {
        return Some(crate::text::DEFAULT_GRAPH_IRI.to_string());
    }
    match snap.term(g)? {
        oxrdf::Term::NamedNode(n) => Some(n.into_string()),
        oxrdf::Term::BlankNode(b) => Some(format!("_:{}", b.as_str())),
        _ => None,
    }
}

/// The base of one generation under one configuration: rows, tree and column.
pub(crate) struct GeoBase {
    /// `Generation::uid`
    pub generation: u64,
    pub generation_name: String,
    /// base rows in PSO order (the tree's items)
    pub rows: Vec<Row>,
    /// base rows whose literal was skipped but may still match (see
    /// [`Slot::rechecked`]): candidates of every search
    pub skipped: Vec<Row>,
    pub tree: Option<RTree<f32>>,
    pub column: Column,
    /// base rows per predicate slot
    pub slot_rows: Vec<u64>,
    pub built_ms: f64,
}

impl GeoBase {
    /// An empty base (no rows, empty column).
    pub fn empty(snap: &Snapshot, cfg: &GeoConfig) -> GeoBase {
        GeoBase {
            generation: snap.generation.uid,
            generation_name: snap.generation.name.clone(),
            rows: Vec::new(),
            skipped: Vec::new(),
            tree: None,
            column: Column::empty(),
            slot_rows: vec![0; cfg.predicates.len()],
            built_ms: 0.0,
        }
    }

    /// Memory of rows, tree and column.
    pub fn bytes(&self) -> u64 {
        self.tree_bytes() + self.column.bytes()
    }

    fn tree_bytes(&self) -> u64 {
        ((self.rows.len() + self.skipped.len()) * std::mem::size_of::<Row>()) as u64
            + tree_bytes(&self.tree)
    }
}

/// How a build is told its budget, reports progress and learns it should stop.
pub(crate) struct BuildCtl<'a> {
    pub budget: u64,
    /// progress 0–1 as `f32` bits
    pub progress: &'a AtomicU32,
    /// true: give up (the store closes, or the build was superseded)
    pub cancel: &'a (dyn Fn() -> bool + Sync),
}

fn set_progress(p: &AtomicU32, x: f32) {
    p.store(x.to_bits(), Ordering::Relaxed);
}

fn over_budget(limit: u64, requested: u64) -> Error {
    Error::BudgetExceeded(crate::Budget {
        kind: crate::BudgetKind::Memory,
        limit,
        requested,
    })
}

/// Build the base of `snap`'s generation: rows of the configured predicates in the
/// generation's own index (not the delta), their literals parsed in parallel blocks,
/// and the packed tree. Fails with [`Error::BudgetExceeded`] past the budget and
/// [`Error::Cancelled`] when told to stop.
pub(crate) fn build_base(
    snap: &Snapshot,
    cfg: &GeoConfig,
    lookup: &Lookup,
    ctl: &BuildCtl<'_>,
) -> Result<GeoBase> {
    let t0 = std::time::Instant::now();
    set_progress(ctl.progress, 0.0);
    let mut base = GeoBase::empty(snap, cfg);
    let perm = snap.perm(Perm::Pso);
    // the base vocabulary ids of the configured predicates
    let mut preds: Vec<(u64, u16)> = Vec::new();
    for (i, iri) in cfg.predicates.iter().enumerate() {
        if let Ok(v) = snap.generation.vocab.find(&crate::id::iri_key(iri)) {
            preds.push((Id::vocab(v).0, i as u16));
        }
    }
    let mut n = 0u64;
    for &(p, _) in &preds {
        n += perm.count(&snap.cache, &[p])?;
    }
    // rows alone must fit (the parsed geometries are checked as they come)
    let row_bytes = std::mem::size_of::<Row>() as u64;
    if n * row_bytes > ctl.budget {
        return Err(over_budget(ctl.budget, n * row_bytes));
    }
    let mut rows: Vec<Row> = Vec::with_capacity(n as usize);
    for &(p, _) in &preds {
        perm.for_each_range(&snap.cache, &[p], |b, s, e| {
            if (ctl.cancel)() {
                return Err(Error::Cancelled);
            }
            for i in s..e {
                let k = b.key(i);
                if lookup.graph(Id(k[3]), snap, cfg) {
                    rows.push(Row {
                        s: k[1],
                        p: k[0],
                        o: k[2],
                        g: k[3],
                    });
                }
            }
            Ok(())
        })?;
    }
    set_progress(ctl.progress, 0.1);
    let mut objs: Vec<u64> = rows
        .iter()
        .map(|r| r.o)
        .filter(|&o| Id(o).tag() == Tag::Vocab)
        .collect();
    objs.sort_unstable();
    objs.dedup();
    let total = objs.len().max(1) as f32;
    let done = AtomicU64::new(0);
    let used = AtomicU64::new(rows.len() as u64 * row_bytes);
    let column = Column::build(snap, &objs, cfg, &|k, bytes| {
        if (ctl.cancel)() {
            return Err(Error::Cancelled);
        }
        let u = used.fetch_add(bytes, Ordering::Relaxed) + bytes;
        if u > ctl.budget {
            return Err(over_budget(ctl.budget, u));
        }
        let d = done.fetch_add(k as u64, Ordering::Relaxed) + k as u64;
        set_progress(ctl.progress, 0.1 + 0.8 * (d as f32 / total));
        Ok(())
    })?;
    let mut boxes: Vec<[f32; 4]> = Vec::with_capacity(rows.len());
    let mut skipped = Vec::new();
    rows.retain(|r| match column.base_entry(r.o) {
        Some(e) => {
            boxes.push(e.bbox());
            true
        }
        None => {
            if column.get(r.o).is_some_and(|s| s.rechecked()) {
                skipped.push(*r);
            }
            false
        }
    });
    for r in &rows {
        if let Some(&(_, slot)) = preds.iter().find(|(p, _)| *p == r.p) {
            base.slot_rows[slot as usize] += 1;
        }
    }
    if (ctl.cancel)() {
        return Err(Error::Cancelled);
    }
    base.tree = pack(boxes.into_iter());
    base.rows = rows;
    base.rows.shrink_to_fit();
    base.skipped = skipped;
    base.column = column;
    let need = base.bytes();
    if need > ctl.budget {
        return Err(over_budget(ctl.budget, need));
    }
    base.built_ms = t0.elapsed().as_secs_f64() * 1000.0;
    set_progress(ctl.progress, 1.0);
    Ok(base)
}

/// Rows inserted by commits, in a packed tree.
pub(crate) struct Overlay {
    pub rows: Vec<TailRow>,
    pub tree: Option<RTree<f32>>,
    /// inserted rows whose literal was skipped but may still match
    /// ([`Slot::rechecked`])
    pub skipped: Vec<Row>,
}

impl Overlay {
    pub fn empty() -> Overlay {
        Overlay {
            rows: Vec::new(),
            tree: None,
            skipped: Vec::new(),
        }
    }

    /// The overlay of `rows` and the skipped rows `skipped` (each sorted, one per quad).
    pub fn build(rows: Vec<TailRow>, mut skipped: Vec<Row>) -> Overlay {
        skipped.sort_unstable();
        skipped.dedup();
        Overlay {
            skipped,
            ..Overlay::tree(rows)
        }
    }

    fn tree(mut rows: Vec<TailRow>) -> Overlay {
        rows.sort_unstable_by_key(|r| r.row);
        rows.dedup_by_key(|r| r.row);
        let tree = pack(
            rows.iter()
                .map(|r| r.entry.bbox())
                .collect::<Vec<_>>()
                .into_iter(),
        );
        Overlay {
            rows,
            tree,
            skipped: Vec::new(),
        }
    }

    pub fn bytes(&self) -> u64 {
        (self.rows.len() * std::mem::size_of::<TailRow>()
            + self.skipped.len() * std::mem::size_of::<Row>()) as u64
            + tree_bytes(&self.tree)
    }
}

/// The overlay of every indexable inserted quad of `snap` (a scan of `delta.ins` for
/// the configured predicates), as at open and after a build.
pub(crate) fn overlay_of(
    snap: &Snapshot,
    cfg: &GeoConfig,
    base: &GeoBase,
    lookup: &Lookup,
) -> Overlay {
    lookup.resolve(snap, cfg);
    let ins = &snap.delta.ins[Perm::Pso.index()];
    let mut rows = Vec::new();
    let mut skipped = Vec::new();
    if ins.is_empty() {
        return Overlay::empty();
    }
    for (p, _) in lookup.indexed() {
        let lo = [p, 0, 0, 0];
        let hi = [p, u64::MAX, u64::MAX, u64::MAX];
        for k in ins.range(lo..=hi) {
            if !lookup.graph(Id(k[3]), snap, cfg) {
                continue;
            }
            let row = Row {
                s: k[1],
                p: k[0],
                o: k[2],
                g: k[3],
            };
            match base.column.get_or_classify(Id(k[2]), snap, cfg) {
                Slot::Geom(entry) => rows.push(TailRow { row, entry }),
                s if s.rechecked() => skipped.push(row),
                _ => {}
            }
        }
    }
    Overlay::build(rows, skipped)
}

/// The tail length past which the overlay tree is rebuilt.
pub(crate) fn tail_limit(overlay_rows: usize) -> usize {
    (overlay_rows / 8).max(4096)
}

/// What a view allows.
#[derive(Clone)]
pub(crate) enum ViewState {
    Ready,
    /// the base is being built: progress as `f32` bits
    Building(Arc<AtomicU32>),
    Failed,
    OverBudget,
    Txn,
    Historical,
}

/// The spatial index as one snapshot sees it (immutable).
#[derive(Clone)]
pub struct GeoView {
    pub config: Arc<GeoConfig>,
    /// +1 per enable, reconfiguration or rebuild (part of result-cache keys)
    pub epoch: u64,
    /// the `uid` of the generation the base belongs to
    pub generation: u64,
    /// the commit of the snapshot
    pub commit: u64,
    pub(crate) state: ViewState,
    pub(crate) lookup: Arc<Lookup>,
    /// set when ready
    pub(crate) base: Option<Arc<GeoBase>>,
    pub(crate) overlay: Arc<Overlay>,
    pub(crate) tail: imbl::Vector<TailRow>,
    /// skipped rows inserted since the overlay was built (as the tail)
    pub(crate) skipped: imbl::Vector<Row>,
}

impl GeoView {
    /// A view without a usable base (building, failed, over budget, …).
    pub(crate) fn pending(
        config: Arc<GeoConfig>,
        epoch: u64,
        snap: &Snapshot,
        lookup: Arc<Lookup>,
        state: ViewState,
    ) -> GeoView {
        GeoView {
            config,
            epoch,
            generation: snap.generation.uid,
            commit: snap.commit,
            state,
            lookup,
            base: None,
            overlay: Arc::new(Overlay::empty()),
            tail: imbl::Vector::new(),
            skipped: imbl::Vector::new(),
        }
    }

    /// A ready view of `base` and `overlay`.
    pub(crate) fn ready(
        config: Arc<GeoConfig>,
        epoch: u64,
        commit: u64,
        lookup: Arc<Lookup>,
        base: Arc<GeoBase>,
        overlay: Overlay,
    ) -> GeoView {
        GeoView {
            config,
            epoch,
            generation: base.generation,
            commit,
            state: ViewState::Ready,
            lookup,
            base: Some(base),
            overlay: Arc::new(overlay),
            tail: imbl::Vector::new(),
            skipped: imbl::Vector::new(),
        }
    }

    /// The same view in state `s`, without its rows.
    pub(crate) fn without_rows(&self, s: ViewState) -> GeoView {
        GeoView {
            state: s,
            base: None,
            overlay: Arc::new(Overlay::empty()),
            tail: imbl::Vector::new(),
            skipped: imbl::Vector::new(),
            ..self.clone()
        }
    }

    /// Whether (and why not) queries on this view can use the index.
    pub fn state(&self) -> IndexState {
        match &self.state {
            ViewState::Ready => IndexState::Ready,
            ViewState::Building(p) => {
                IndexState::Building(f32::from_bits(p.load(Ordering::Relaxed)))
            }
            ViewState::Failed => IndexState::Failed,
            ViewState::OverBudget => IndexState::OverBudget,
            ViewState::Txn => IndexState::Txn,
            ViewState::Historical => IndexState::Historical,
        }
    }

    /// The base and overlay when queries may use them.
    pub(crate) fn usable(&self) -> Option<&Arc<GeoBase>> {
        match self.state {
            ViewState::Ready => self.base.as_ref(),
            _ => None,
        }
    }

    /// The slot of an indexed predicate (`None`: the predicate is not indexed).
    pub fn predicate_slot(&self, p: Id) -> Option<u16> {
        self.lookup.known_slot(p)
    }

    /// Estimated rows of the predicates `preds` whose envelope intersects one of the
    /// CRS84 `windows`.
    pub fn estimate(&self, preds: &[u16], windows: &[[f64; 4]]) -> f64 {
        let Some(base) = self.usable() else {
            return 0.0;
        };
        let all: u64 = base.slot_rows.iter().sum();
        let frac = if preds.is_empty() || all == 0 {
            1.0
        } else {
            let mine: u64 = preds
                .iter()
                .filter_map(|&s| base.slot_rows.get(s as usize))
                .sum();
            mine as f64 / all as f64
        };
        let mut est = base
            .tree
            .as_ref()
            .map_or(0.0, |t| tree_estimate(t, windows))
            * frac;
        if let Some(t) = &self.overlay.tree {
            est += tree_estimate(t, windows) * frac;
        }
        est + self
            .tail
            .iter()
            .filter(|r| windows.iter().any(|w| intersects(&r.entry.bbox(), w)))
            .count() as f64
    }

    /// Height of the base tree (for costs).
    pub fn levels(&self) -> u32 {
        self.usable()
            .and_then(|b| b.tree.as_ref())
            .map_or(0, |t| t.num_levels() as u32)
    }

    /// The view of a transaction's uncommitted changes, which the index does not cover.
    pub fn for_txn(&self) -> GeoView {
        GeoView {
            state: ViewState::Txn,
            ..self.clone()
        }
    }

    /// Memory of the rows this view adds to its base (overlay and tail).
    pub(crate) fn overlay_bytes(&self) -> u64 {
        self.overlay.bytes()
            + (self.tail.len() * std::mem::size_of::<TailRow>()
                + self.skipped.len() * std::mem::size_of::<Row>()) as u64
    }

    /// Memory of the index as this view sees it.
    pub(crate) fn bytes(&self) -> u64 {
        self.base.as_ref().map_or(0, |b| b.bytes()) + self.overlay_bytes()
    }
}

/// The last build and its outcome, for the status.
#[derive(Default)]
pub(crate) struct BuildInfo {
    pub message: Option<String>,
    pub last_build: Option<GeoBuild>,
    /// the latest epoch whose build finished (published or not)
    pub finished: u64,
}

/// A dataset's spatial index (the store holds one while the index is enabled).
pub struct GeoIndex {
    pub(crate) config: Arc<GeoConfig>,
    pub(crate) epoch: AtomicU64,
    pub(crate) budget: u64,
    /// progress of the running build (`f32` bits)
    pub(crate) progress: Arc<AtomicU32>,
    pub(crate) info: Mutex<BuildInfo>,
    done: Condvar,
    /// replaced or disabled: running builds publish nothing
    pub(crate) retired: AtomicBool,
    /// test hook: background builds wait before they publish
    pub(crate) paused: AtomicBool,
    /// test hook: the next commit-path update fails
    #[cfg(any(test, feature = "failpoints"))]
    pub(crate) fail_next: AtomicBool,
}

impl GeoIndex {
    pub(crate) fn new(config: GeoConfig, epoch: u64, budget: u64) -> GeoIndex {
        GeoIndex {
            config: Arc::new(config),
            epoch: AtomicU64::new(epoch),
            budget,
            progress: Arc::new(AtomicU32::new(0)),
            info: Mutex::new(BuildInfo::default()),
            done: Condvar::new(),
            retired: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            #[cfg(any(test, feature = "failpoints"))]
            fail_next: AtomicBool::new(false),
        }
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Record that the build of `epoch` is over (`message`: why it was not published).
    pub(crate) fn finish(&self, epoch: u64, message: Option<String>, last: Option<GeoBuild>) {
        let mut i = self.info.lock();
        if epoch >= i.finished {
            i.finished = epoch;
            i.message = message;
            if last.is_some() {
                i.last_build = last;
            }
        }
        self.done.notify_all();
    }

    /// Set the status message (failures of the commit path).
    pub(crate) fn note(&self, message: String) {
        self.info.lock().message = Some(message);
    }

    /// Wait until the build of `epoch` (or a later one) is over.
    pub(crate) fn wait(&self, epoch: u64) {
        let mut i = self.info.lock();
        while i.finished < epoch && !self.retired.load(Ordering::SeqCst) {
            self.done
                .wait_for(&mut i, std::time::Duration::from_millis(50));
        }
    }

    /// Wake waiters (the index was retired).
    pub(crate) fn wake(&self) {
        let _i = self.info.lock();
        self.done.notify_all();
    }

    /// The status as of `snap`.
    pub(crate) fn status(&self, snap: &Snapshot) -> GeoStatus {
        let view = snap.geo.as_deref();
        let state = view.map_or(IndexState::Building(0.0), GeoView::state);
        let info = self.info.lock();
        let base = view.and_then(|v| v.base.as_ref());
        let counts = base.map(|b| b.column.counts()).unwrap_or_default();
        let crs = counts
            .crs
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();
        let Counts {
            literals,
            malformed,
            unknown_crs,
            too_large,
            empty,
            ..
        } = counts;
        GeoStatus {
            enabled: true,
            state: match state {
                // a snapshot's own states never reach the live status
                IndexState::Txn | IndexState::Historical | IndexState::Off => {
                    IndexState::Failed.as_str().into()
                }
                s => s.as_str().into(),
            },
            progress: match state {
                IndexState::Building(p) => Some(p),
                _ => None,
            },
            message: info.message.clone(),
            generation: base
                .map(|b| b.generation_name.clone())
                .unwrap_or_else(|| snap.generation.name.clone()),
            commit: snap.commit,
            rows: GeoRows {
                base: base.map_or(0, |b| b.rows.len() as u64),
                overlay: view.map_or(0, |v| v.overlay.rows.len() as u64),
                tail: view.map_or(0, |v| v.tail.len() as u64),
            },
            literals,
            skipped: GeoSkipped {
                malformed,
                unknown_crs,
                too_large,
                empty,
            },
            crs,
            memory: GeoMemory {
                tree_bytes: base.map_or(0, |b| b.tree_bytes())
                    + view.map_or(0, |v| tree_bytes(&v.overlay.tree)),
                geometry_bytes: base.map_or(0, |b| b.column.bytes()),
                overlay_bytes: view.map_or(0, |v| v.overlay_bytes()),
                budget_bytes: self.budget,
            },
            config: (*self.config).clone(),
            format_version: FORMAT_VERSION,
            last_build: info.last_build.clone(),
        }
    }
}

/// The spatial data of one generation. The base, its column and the overlay live in the
/// views of the generation's snapshots (a reconfiguration replaces them for the same
/// generation), so nothing is kept here yet.
#[derive(Default)]
pub struct GenerationGeo {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_bound_the_true_count() {
        // a 100 × 100 grid of unit boxes
        let mut boxes = Vec::new();
        for i in 0..100 {
            for j in 0..100 {
                let (x, y) = (i as f32, j as f32);
                boxes.push([x, y, x + 0.5, y + 0.5]);
            }
        }
        let tree = pack(boxes.clone().into_iter()).unwrap();
        let w = [10.0, 10.0, 19.9, 19.9];
        let exact = boxes.iter().filter(|b| intersects(b, &w)).count() as f64;
        assert_eq!(exact, 100.0);
        let est = tree_estimate(&tree, &[w]);
        assert!(est >= exact && est <= 10_000.0 / 4.0, "{est}");
        assert_eq!(tree_estimate(&tree, &[[500.0, 500.0, 600.0, 600.0]]), 0.0);
        let world = tree_estimate(&tree, &[[-1000.0, -1000.0, 1000.0, 1000.0]]);
        assert_eq!(world, 10_000.0);
        assert!(
            pack(
                std::iter::empty::<[f32; 4]>()
                    .collect::<Vec<_>>()
                    .into_iter()
            )
            .is_none()
        );
    }
}
