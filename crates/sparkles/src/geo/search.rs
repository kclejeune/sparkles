//! Search kernels over a snapshot's spatial rows: window and nearest-first searches over
//! the base and overlay trees and the tail, checked against the snapshot.
//!
//! Rows come out with their geometry; the caller runs the exact test. When the
//! snapshot's index is not ready (building, failed, over budget, a transaction's or a
//! past state), the same searches scan the predicates instead (`fallback`), with the
//! same answers.

use super::column::{ColumnEntry, Slot, classify};
use super::config::GeoConfig;
use super::geom::Geom;
use super::index::{GeoBase, GeoView, Row, intersects, window_f32};
use super::ops::distance::lower_bound_m;
use crate::error::Result;
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::sparql::ctx::Ctx;
use crate::sparql::plan::GraphFilter;
use crate::store::{Chunk, Snapshot};
use geo_index::rtree::{RTree, RTreeIndex};
use rustc_hash::{FxHashMap, FxHashSet};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;

/// A candidate row: the quad and its geometry.
#[derive(Clone)]
pub struct Hit {
    pub s: Id,
    pub p: Id,
    pub o: Id,
    pub g: Id,
    pub entry: Arc<ColumnEntry>,
}

impl Hit {
    fn new(r: &Row, entry: Arc<ColumnEntry>) -> Hit {
        Hit {
            s: Id(r.s),
            p: Id(r.p),
            o: Id(r.o),
            g: Id(r.g),
            entry,
        }
    }
}

/// What a search did (explain counters).
#[derive(Clone, Debug, Default)]
pub struct SearchStats {
    /// rows whose envelope matched
    pub candidates: u64,
    /// exact tests run
    pub refined: u64,
    /// rows that passed the exact test
    pub matched: u64,
    /// tree nodes visited
    pub nodes: u64,
    /// the index was not used
    pub fallback: bool,
}

/// Rows handed to a sink at once, and between cancellation checks.
const CHUNK: usize = 4096;

/// Rows of the predicates `preds` whose envelope intersects one of the CRS84 `windows`,
/// in chunks.
pub fn window(
    ctx: &Ctx,
    preds: &[Id],
    windows: &[[f64; 4]],
    graph: &GraphFilter,
    st: &mut SearchStats,
    sink: &mut dyn FnMut(&[Hit]) -> Result<()>,
) -> Result<()> {
    window_with(ctx, preds, windows, graph, false, st, sink)
}

/// [`window`]; with `dedup`, one row per triple across graphs (a merged default graph
/// without a graph variable).
pub fn window_with(
    ctx: &Ctx,
    preds: &[Id],
    windows: &[[f64; 4]],
    graph: &GraphFilter,
    dedup: bool,
    st: &mut SearchStats,
    sink: &mut dyn FnMut(&[Hit]) -> Result<()>,
) -> Result<()> {
    let snap = &ctx.snap;
    let Some((view, base)) = indexed(snap, preds) else {
        return scan_fallback(ctx, preds, Some(windows), graph, dedup, st, sink);
    };
    let mut f = Filter::new(snap, preds, graph, dedup);
    let mut out: Vec<Hit> = Vec::with_capacity(CHUNK);
    let mut flush = |out: &mut Vec<Hit>, st: &mut SearchStats, force: bool| -> Result<()> {
        if out.len() >= CHUNK || (force && !out.is_empty()) {
            ctx.check()?;
            st.candidates += out.len() as u64;
            sink(out)?;
            out.clear();
        }
        Ok(())
    };
    // base rows
    if let Some(t) = &base.tree {
        let found = search_tree(t, windows, &mut st.nodes);
        for (n, &i) in found.iter().enumerate() {
            if n % CHUNK == 0 {
                ctx.check()?;
            }
            let r = &base.rows[i as usize];
            if f.base_row(r)
                && let Some(e) = base.column.base_entry(r.o)
            {
                out.push(Hit::new(r, e.clone()));
                flush(&mut out, st, false)?;
            }
        }
    }
    // overlay and tail rows
    if let Some(t) = &view.overlay.tree {
        let found = search_tree(t, windows, &mut st.nodes);
        for (n, &i) in found.iter().enumerate() {
            if n % CHUNK == 0 {
                ctx.check()?;
            }
            let r = &view.overlay.rows[i as usize];
            if f.delta_row(&r.row) {
                out.push(Hit::new(&r.row, r.entry.clone()));
                flush(&mut out, st, false)?;
            }
        }
    }
    for (n, r) in view.tail.iter().enumerate() {
        if n % CHUNK == 0 {
            ctx.check()?;
        }
        let b = r.entry.bbox();
        if windows.iter().any(|w| intersects(&b, w)) && f.delta_row(&r.row) {
            out.push(Hit::new(&r.row, r.entry.clone()));
            flush(&mut out, st, false)?;
        }
    }
    flush(&mut out, st, true)
}

/// Rows of the predicates `preds` in increasing lower bound of their distance to `q`,
/// in chunks with that bound; the sink returns `false` to stop.
///
/// The bound handed with a chunk holds for every row after it: none is nearer to `q`
/// than that many metres. Chunks start small and grow, so a search for a few nearest
/// rows refines few. Ties are broken by `(s, o, g)`.
pub fn nearest(
    ctx: &Ctx,
    preds: &[Id],
    q: &Geom,
    graph: &GraphFilter,
    st: &mut SearchStats,
    sink: &mut dyn FnMut(&[Hit], f64) -> Result<bool>,
) -> Result<()> {
    nearest_with(ctx, preds, q, graph, false, st, sink)
}

/// [`nearest`]; with `dedup`, one row per triple across graphs.
pub fn nearest_with(
    ctx: &Ctx,
    preds: &[Id],
    q: &Geom,
    graph: &GraphFilter,
    dedup: bool,
    st: &mut SearchStats,
    sink: &mut dyn FnMut(&[Hit], f64) -> Result<bool>,
) -> Result<()> {
    // a query that cannot be placed on the globe is near nothing
    let Some(lb) = Bound::of(q) else {
        return Ok(());
    };
    let snap = &ctx.snap;
    let Some((view, base)) = indexed(snap, preds) else {
        return nearest_fallback(ctx, preds, &lb, graph, dedup, st, sink);
    };
    let mut f = Filter::new(snap, preds, graph, dedup);
    let trees = [
        base.tree.as_ref().map(Tree::new),
        view.overlay.tree.as_ref().map(Tree::new),
    ];
    let mut heap: BinaryHeap<Entry> = BinaryHeap::new();
    for (t, src) in trees.iter().zip([Src::Base, Src::Overlay]) {
        if let Some(t) = t {
            heap.push(Entry::node(&lb, t, t.root(), src));
        }
    }
    for r in view.tail.iter() {
        let mut e = Entry::row(&lb, &r.row, r.entry.clone());
        e.from_tail = true;
        heap.push(e);
    }
    let mut out: Vec<Hit> = Vec::new();
    let mut size = 16;
    let mut pops = 0usize;
    while let Some(e) = heap.pop() {
        pops += 1;
        if pops.is_multiple_of(CHUNK) {
            ctx.check()?;
        }
        match e.item {
            Item::Node(n, src) => {
                let Some(t) = &trees[src as usize] else {
                    continue;
                };
                // a tree of one item has it as its root
                let items: Vec<usize> = if t.is_item(n) {
                    vec![n]
                } else {
                    st.nodes += 1;
                    let mut items = Vec::new();
                    for c in t.children(n) {
                        if t.is_item(c) {
                            items.push(c);
                        } else {
                            heap.push(Entry::node(&lb, t, c, src));
                        }
                    }
                    items
                };
                for c in items {
                    let i = t.item(c);
                    match src {
                        Src::Base => {
                            let r = &base.rows[i];
                            if f.base_valid(r)
                                && let Some(en) = base.column.base_entry(r.o)
                            {
                                heap.push(Entry::row(&lb, r, en.clone()));
                            }
                        }
                        Src::Overlay => {
                            let r = &view.overlay.rows[i];
                            if f.delta_row_unseen(&r.row) {
                                heap.push(Entry::row(&lb, &r.row, r.entry.clone()));
                            }
                        }
                    }
                }
            }
            Item::Row(r, entry) => {
                // tail rows are checked here; tree rows were when their leaf was opened
                if !f.emit(&r, e.from_tail) {
                    continue;
                }
                out.push(Hit::new(&r, entry));
                if out.len() >= size {
                    let bound = heap.peek().map_or(f64::INFINITY, |e| e.lb);
                    st.candidates += out.len() as u64;
                    if !sink(&out, bound)? {
                        return Ok(());
                    }
                    out.clear();
                    size = (size * 2).min(CHUNK);
                }
            }
        }
    }
    if !out.is_empty() {
        st.candidates += out.len() as u64;
        sink(&out, f64::INFINITY)?;
    }
    Ok(())
}

/// The view and base when `snap`'s index can answer for every predicate of `preds`.
fn indexed<'a>(snap: &'a Snapshot, preds: &[Id]) -> Option<(&'a GeoView, &'a Arc<GeoBase>)> {
    let view = snap.geo.as_deref()?;
    let base = view.usable()?;
    preds
        .iter()
        .all(|&p| view.predicate_slot(p).is_some())
        .then_some((view, base))
}

/// A packed tree read through its layout: nodes are positions in `boxes` (four
/// coordinates each), level by level from the items up to the root; a node's index entry
/// is its first child's position, an item's its insertion index.
struct Tree<'a> {
    boxes: &'a [f32],
    indices: geo_index::indices::Indices<'a>,
    /// positions below this are items
    items: usize,
    node: usize,
    bounds: &'a [usize],
}

impl<'a> Tree<'a> {
    fn new(t: &'a RTree<f32>) -> Tree<'a> {
        Tree {
            boxes: t.boxes(),
            indices: t.indices(),
            items: t.num_items() as usize * 4,
            node: t.node_size() as usize * 4,
            bounds: t.level_bounds(),
        }
    }

    fn root(&self) -> usize {
        self.boxes.len() - 4
    }

    fn bbox(&self, pos: usize) -> [f32; 4] {
        [
            self.boxes[pos],
            self.boxes[pos + 1],
            self.boxes[pos + 2],
            self.boxes[pos + 3],
        ]
    }

    fn is_item(&self, pos: usize) -> bool {
        pos < self.items
    }

    /// The insertion index of the item at `pos`.
    fn item(&self, pos: usize) -> usize {
        self.indices.get(pos >> 2)
    }

    /// The positions of the children of the node at `pos`.
    fn children(&self, pos: usize) -> impl Iterator<Item = usize> + use<> {
        let start = self.indices.get(pos >> 2);
        // the end of the children's level
        let level_end = self
            .bounds
            .iter()
            .copied()
            .find(|&b| b > start)
            .unwrap_or(self.boxes.len());
        (start..(start + self.node).min(level_end)).step_by(4)
    }
}

/// The tree items whose box intersects one of `windows` (each once).
fn search_tree(t: &RTree<f32>, windows: &[[f64; 4]], nodes: &mut u64) -> Vec<u32> {
    let ws: Vec<[f32; 4]> = windows.iter().map(window_f32).collect();
    let hits = |b: [f32; 4]| {
        ws.iter()
            .any(|w| b[0] <= w[2] && b[2] >= w[0] && b[1] <= w[3] && b[3] >= w[1])
    };
    let t = Tree::new(t);
    let mut out = Vec::new();
    let root = t.root();
    if t.is_item(root) {
        if hits(t.bbox(root)) {
            out.push(t.item(root) as u32);
        }
        return out;
    }
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        *nodes += 1;
        for c in t.children(n) {
            if !hits(t.bbox(c)) {
                continue;
            }
            if t.is_item(c) {
                out.push(t.item(c) as u32);
            } else {
                stack.push(c);
            }
        }
    }
    out
}

/// The row checks of a search: predicate, graph, validity in the snapshot, duplicates.
struct Filter<'a> {
    snap: &'a Snapshot,
    preds: &'a [Id],
    graph: &'a GraphFilter,
    dedup: bool,
    /// triples handed out (with `dedup`)
    triples: FxHashSet<(u64, u64, u64)>,
    /// overlay and tail quads handed out: a quad deleted and inserted again may have
    /// two rows until the overlay is rebuilt
    quads: FxHashSet<Row>,
}

impl<'a> Filter<'a> {
    fn new(snap: &'a Snapshot, preds: &'a [Id], graph: &'a GraphFilter, dedup: bool) -> Self {
        Filter {
            snap,
            preds,
            graph,
            dedup,
            triples: FxHashSet::default(),
            quads: FxHashSet::default(),
        }
    }

    #[inline]
    fn wanted(&self, r: &Row) -> bool {
        self.graph.accepts(r.g) && self.preds.iter().any(|p| p.0 == r.p)
    }

    #[inline]
    fn unseen_triple(&mut self, r: &Row) -> bool {
        !self.dedup || self.triples.insert((r.s, r.p, r.o))
    }

    /// A base row: valid unless the snapshot deleted its quad.
    fn base_row(&mut self, r: &Row) -> bool {
        self.base_valid(r) && self.unseen_triple(r)
    }

    /// [`Self::base_row`] without the triple check (done at emission).
    fn base_valid(&self, r: &Row) -> bool {
        if !self.wanted(r) {
            return false;
        }
        let del = &self.snap.delta.del[Perm::Pso.index()];
        del.is_empty() || !del.contains(&r.pso())
    }

    /// An overlay or tail row: valid if the snapshot inserted its quad.
    fn delta_row(&mut self, r: &Row) -> bool {
        self.delta_row_unseen(r) && self.unseen_triple(r)
    }

    /// [`Self::delta_row`] without the triple check (done at emission).
    fn delta_row_unseen(&mut self, r: &Row) -> bool {
        self.wanted(r)
            && self.snap.delta.ins[Perm::Pso.index()].contains(&r.pso())
            && self.quads.insert(*r)
    }

    /// Whether a row taken from the queue goes out: tail rows are checked now.
    fn emit(&mut self, r: &Row, from_tail: bool) -> bool {
        if from_tail && !self.delta_row_unseen(r) {
            return false;
        }
        self.unseen_triple(r)
    }
}

/// A lower bound of the distance from the query to a box: from the centre of the
/// query's envelope, less the farthest the query reaches from it.
struct Bound {
    c: [f64; 2],
    reach: f64,
}

/// A radius at least as large as every radius of curvature of the WGS 84 ellipsoid (and
/// the spheres of the other distance models), so `reach` is never too short.
const R_MAX: f64 = 6_400_000.0;

impl Bound {
    fn of(q: &Geom) -> Option<Bound> {
        let b = q.bbox84()?;
        let c = [(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0];
        if b[0] == b[2] && b[1] == b[3] {
            return Some(Bound { c, reach: 0.0 });
        }
        // from the centre, along the meridian to any latitude of the box, then along the
        // parallel: a path no shorter than the shortest one
        let min_abs_lat = if b[1] <= 0.0 && b[3] >= 0.0 {
            0.0
        } else {
            b[1].abs().min(b[3].abs())
        };
        let half_lat = ((b[3] - b[1]) / 2.0).to_radians();
        let half_lon = ((b[2] - b[0]) / 2.0).to_radians();
        let reach = R_MAX * (half_lat + min_abs_lat.to_radians().cos() * half_lon);
        Some(Bound { c, reach })
    }

    fn of_box(&self, b: [f32; 4]) -> f64 {
        (lower_bound_m(self.c, b.map(f64::from)) - self.reach).max(0.0)
    }
}

#[derive(Clone, Copy)]
enum Src {
    Base = 0,
    Overlay = 1,
}

enum Item {
    /// a node (or the single item) of a tree, by position
    Node(usize, Src),
    Row(Row, Arc<ColumnEntry>),
}

/// A queue entry of the nearest-first search: nearest bound first; at equal bounds,
/// nodes before rows (they may hold rows that sort first), then rows by `(s, o, g)`.
struct Entry {
    lb: f64,
    from_tail: bool,
    item: Item,
}

impl Entry {
    fn node(b: &Bound, t: &Tree<'_>, pos: usize, src: Src) -> Entry {
        Entry {
            lb: b.of_box(t.bbox(pos)),
            from_tail: false,
            item: Item::Node(pos, src),
        }
    }

    fn row(b: &Bound, r: &Row, e: Arc<ColumnEntry>) -> Entry {
        Entry {
            lb: b.of_box(e.bbox()),
            from_tail: false,
            item: Item::Row(*r, e),
        }
    }

    fn key(&self) -> (u8, u64, u64, u64) {
        match &self.item {
            Item::Node(..) => (0, 0, 0, 0),
            Item::Row(r, _) => (1, r.s, r.o, r.g),
        }
    }
}

impl PartialEq for Entry {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Entry {}
impl PartialOrd for Entry {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Entry {
    fn cmp(&self, o: &Self) -> Ordering {
        // reversed: the heap pops the smallest bound first
        o.lb.total_cmp(&self.lb)
            .then_with(|| o.key().cmp(&self.key()))
    }
}

/// How the fallback reads a predicate's geometries: the column of the snapshot's view
/// when it has one, else literals parsed here (once per search).
struct Literals<'a> {
    snap: &'a Snapshot,
    cfg: Arc<GeoConfig>,
    base: Option<&'a Arc<GeoBase>>,
    memo: FxHashMap<u64, Slot>,
}

impl<'a> Literals<'a> {
    fn new(snap: &'a Snapshot) -> Literals<'a> {
        let view = snap.geo.as_deref();
        Literals {
            snap,
            cfg: view.map_or_else(|| Arc::new(GeoConfig::default()), |v| v.config.clone()),
            base: view.and_then(|v| v.base.as_ref()),
            memo: FxHashMap::default(),
        }
    }

    fn entry(&mut self, o: u64) -> Option<Arc<ColumnEntry>> {
        if !matches!(Id(o).tag(), Tag::Vocab | Tag::Delta) {
            return None;
        }
        let slot = match self.base.and_then(|b| b.column.get(o)) {
            Some(s) => s,
            None => {
                let (snap, cfg) = (self.snap, &self.cfg);
                self.memo
                    .entry(o)
                    .or_insert_with(|| match snap.key(Id(o)) {
                        Some(k) => classify(&k, cfg),
                        None => Slot::Other,
                    })
                    .clone()
            }
        };
        match slot {
            Slot::Geom(e) => Some(e),
            _ => None,
        }
    }
}

/// Every row of `preds` in `snap` with an indexable geometry (whose envelope meets one
/// of `windows`, when given), by scanning the predicates. The graph scope of the
/// snapshot's configuration applies, as it does to the index.
fn scan_fallback(
    ctx: &Ctx,
    preds: &[Id],
    windows: Option<&[[f64; 4]]>,
    graph: &GraphFilter,
    dedup: bool,
    st: &mut SearchStats,
    sink: &mut dyn FnMut(&[Hit]) -> Result<()>,
) -> Result<()> {
    st.fallback = true;
    let snap = &*ctx.snap;
    let view = snap.geo.as_deref();
    let mut lits = Literals::new(snap);
    let mut triples: FxHashSet<(u64, u64, u64)> = FxHashSet::default();
    let mut out: Vec<Hit> = Vec::with_capacity(CHUNK);
    let mut n = 0usize;
    let mut err: Option<crate::error::Error> = None;
    for &p in preds {
        let mut row = |k: [u64; 4]| -> Result<()> {
            n += 1;
            if n.is_multiple_of(CHUNK) {
                ctx.check()?;
            }
            let r = Row {
                s: k[1],
                p: k[0],
                o: k[2],
                g: k[3],
            };
            if !graph.accepts(r.g) {
                return Ok(());
            }
            if let Some(v) = view
                && !v.lookup.graph(Id(r.g), snap, &v.config)
            {
                return Ok(());
            }
            let Some(e) = lits.entry(r.o) else {
                return Ok(());
            };
            if let Some(ws) = windows
                && !ws.iter().any(|w| intersects(&e.bbox(), w))
            {
                return Ok(());
            }
            if dedup && !triples.insert((r.s, r.p, r.o)) {
                return Ok(());
            }
            out.push(Hit::new(&r, e));
            if out.len() >= CHUNK {
                st.candidates += out.len() as u64;
                sink(&out)?;
                out.clear();
            }
            Ok(())
        };
        snap.scan(Perm::Pso, &[p.0], |c| {
            let r = match c {
                Chunk::Block(b, s, e) => (s..e).try_for_each(|i| row(b.key(i))),
                Chunk::Row(k) => row(k),
            };
            match r {
                Ok(()) => Ok(true),
                Err(e) => {
                    err = Some(e);
                    Ok(false)
                }
            }
        })?;
        if let Some(e) = err.take() {
            return Err(e);
        }
    }
    if !out.is_empty() {
        st.candidates += out.len() as u64;
        sink(&out)?;
    }
    Ok(())
}

/// [`nearest`] by scanning the predicates: every row, sorted by its bound.
fn nearest_fallback(
    ctx: &Ctx,
    preds: &[Id],
    lb: &Bound,
    graph: &GraphFilter,
    dedup: bool,
    st: &mut SearchStats,
    sink: &mut dyn FnMut(&[Hit], f64) -> Result<bool>,
) -> Result<()> {
    let mut all: Vec<(f64, Hit)> = Vec::new();
    let mut scan = SearchStats::default();
    scan_fallback(ctx, preds, None, graph, dedup, &mut scan, &mut |hits| {
        all.extend(hits.iter().map(|h| (lb.of_box(h.entry.bbox()), h.clone())));
        Ok(())
    })?;
    st.fallback = true;
    all.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then_with(|| (a.1.s, a.1.o, a.1.g).cmp(&(b.1.s, b.1.o, b.1.g)))
    });
    let mut i = 0;
    let mut size = 16;
    while i < all.len() {
        ctx.check()?;
        let j = (i + size).min(all.len());
        let hits: Vec<Hit> = all[i..j].iter().map(|(_, h)| h.clone()).collect();
        let bound = all.get(j).map_or(f64::INFINITY, |(b, _)| *b);
        st.candidates += hits.len() as u64;
        if !sink(&hits, bound)? {
            return Ok(());
        }
        i = j;
        size = (size * 2).min(CHUNK);
    }
    Ok(())
}
