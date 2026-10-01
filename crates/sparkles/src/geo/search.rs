//! Search kernels over a snapshot's spatial rows: window and nearest-first searches over
//! the base and overlay trees and the tail, checked against the snapshot.
//!
//! Rows come out with their geometry; the caller runs the exact test. When the
//! snapshot's index is not ready (building, failed, over budget, a transaction's or a
//! past state), the same searches scan the predicates instead (`fallback`), with the
//! same answers.
//!
//! Rows whose literal the index skipped although a `geof:` function could still match
//! it (too long to index; see [`Slot::rechecked`]) are candidates of every search,
//! whatever the window, so a search never misses a row the plain filter would keep.
//!
//! With `"wgs84": true`, searching the `wgs84_pos:lat` predicate finds the W3C Basic Geo
//! points (rows whose object is a point's id, see [`super::wgs84`]), indexed or scanned.

use super::column::{Column, ColumnEntry, Slot, classify, recheck};
use super::config::GeoConfig;
use super::geom::Geom;
use super::index::{GeoBase, GeoView, Row, intersects};
use super::ops::distance::lower_bound_m;
use super::tree::{PackedTree, Tree};
use crate::error::Result;
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::sparql::ctx::Ctx;
use crate::sparql::plan::GraphFilter;
use crate::store::{Chunk, Snapshot};
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
    pub(crate) fn new(r: &Row, entry: Arc<ColumnEntry>) -> Hit {
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
    /// candidates whose literal the index skipped (tested whatever the window)
    pub rechecked: u64,
}

/// The geometries of skipped literals that searches hand out anyway, parsed once per
/// search.
pub(crate) struct Rechecked<'a> {
    snap: &'a Snapshot,
    cfg: Arc<GeoConfig>,
    memo: FxHashMap<u64, Option<Arc<ColumnEntry>>>,
}

impl<'a> Rechecked<'a> {
    pub(crate) fn new(snap: &'a Snapshot) -> Rechecked<'a> {
        Rechecked {
            snap,
            cfg: snap
                .geo
                .as_ref()
                .map_or_else(|| Arc::new(GeoConfig::default()), |v| v.config.clone()),
            memo: FxHashMap::default(),
        }
    }

    pub(crate) fn entry(&mut self, o: u64) -> Option<Arc<ColumnEntry>> {
        let (snap, cfg) = (self.snap, &self.cfg);
        self.memo
            .entry(o)
            .or_insert_with(|| recheck(&snap.key(Id(o))?, cfg))
            .clone()
    }
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
        let found = t.search(windows, &mut st.nodes);
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
        let found = t.search(windows, &mut st.nodes);
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
    // skipped rows that may still match, whatever the window
    let mut re = Rechecked::new(snap);
    let delta = view.overlay.skipped.iter().chain(view.skipped.iter());
    for (r, from_base) in base
        .skipped
        .iter()
        .map(|r| (r, true))
        .chain(delta.map(|r| (r, false)))
    {
        let valid = if from_base {
            f.base_row(r)
        } else {
            f.delta_row(r)
        };
        if valid && let Some(e) = re.entry(r.o) {
            st.rechecked += 1;
            out.push(Hit::new(r, e));
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
        base.tree.as_ref().map(PackedTree::tree),
        view.overlay.tree.as_ref().map(PackedTree::tree),
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
    // skipped rows that may still match, by the bound of their own envelope (the whole
    // world, a bound of 0, when it has no place on the globe)
    let mut re = Rechecked::new(snap);
    for r in &base.skipped {
        if f.base_valid(r)
            && let Some(en) = re.entry(r.o)
        {
            st.rechecked += 1;
            heap.push(Entry::row(&lb, r, en));
        }
    }
    for r in view.overlay.skipped.iter().chain(view.skipped.iter()) {
        if let Some(en) = re.entry(r.o) {
            st.rechecked += 1;
            let mut e = Entry::row(&lb, r, en);
            e.from_tail = true;
            heap.push(e);
        }
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
pub(crate) fn indexed<'a>(
    snap: &'a Snapshot,
    preds: &[Id],
) -> Option<(&'a GeoView, &'a Arc<GeoBase>)> {
    let view = snap.geo.as_deref()?;
    let base = view.usable()?;
    let lat = wgs84_lat(snap);
    preds
        .iter()
        .all(|&p| view.predicate_slot(p).is_some() || Some(p) == lat)
        .then_some((view, base))
}

/// The `wgs84_pos:lat` predicate when `snap`'s index (or its configuration) makes W3C
/// Basic Geo points.
pub(crate) fn wgs84_lat(snap: &Snapshot) -> Option<Id> {
    let view = snap.geo.as_deref()?;
    if !view.config.wgs84 {
        return None;
    }
    super::wgs84::predicates(snap).map(|(lat, _)| lat)
}

/// The row checks of a search: predicate, graph, validity in the snapshot, duplicates.
pub(crate) struct Filter<'a> {
    snap: &'a Snapshot,
    preds: &'a [Id],
    graph: &'a GraphFilter,
    dedup: bool,
    /// triples handed out (with `dedup`)
    triples: FxHashSet<(u64, u64, u64)>,
    /// overlay and tail quads handed out: a quad deleted and inserted again may have
    /// two rows until the overlay is rebuilt
    quads: FxHashSet<Row>,
    /// the column of the snapshot's base (W3C Basic Geo pairs)
    column: Option<&'a Column>,
}

impl<'a> Filter<'a> {
    pub(crate) fn new(
        snap: &'a Snapshot,
        preds: &'a [Id],
        graph: &'a GraphFilter,
        dedup: bool,
    ) -> Self {
        Filter {
            snap,
            preds,
            graph,
            dedup,
            triples: FxHashSet::default(),
            quads: FxHashSet::default(),
            column: snap
                .geo
                .as_deref()
                .and_then(|v| v.base.as_deref())
                .map(|b| &b.column),
        }
    }

    /// Whether both quads of the W3C Basic Geo point row `r` are in the snapshot.
    fn pair_live(&self, r: &Row) -> bool {
        self.column
            .and_then(|c| c.pair(r.o))
            .is_some_and(|p| p.live(self.snap, r))
    }

    #[inline]
    pub(crate) fn wanted(&self, r: &Row) -> bool {
        self.graph.accepts(r.g) && self.preds.iter().any(|p| p.0 == r.p)
    }

    #[inline]
    pub(crate) fn unseen_triple(&mut self, r: &Row) -> bool {
        !self.dedup || self.triples.insert((r.s, r.p, r.o))
    }

    /// A base row: valid unless the snapshot deleted its quad.
    pub(crate) fn base_row(&mut self, r: &Row) -> bool {
        self.base_valid(r) && self.unseen_triple(r)
    }

    /// [`Self::base_row`] without the triple check (done at emission).
    pub(crate) fn base_valid(&self, r: &Row) -> bool {
        if !self.wanted(r) {
            return false;
        }
        if super::wgs84::is_pair(r.o) {
            return self.pair_live(r);
        }
        let del = &self.snap.delta.del[Perm::Pso.index()];
        del.is_empty() || !del.contains(&r.pso())
    }

    /// An overlay or tail row: valid if the snapshot inserted its quad.
    pub(crate) fn delta_row(&mut self, r: &Row) -> bool {
        self.delta_row_unseen(r) && self.unseen_triple(r)
    }

    /// [`Self::delta_row`] without the triple check (done at emission).
    pub(crate) fn delta_row_unseen(&mut self, r: &Row) -> bool {
        self.wanted(r)
            && if super::wgs84::is_pair(r.o) {
                self.pair_live(r)
            } else {
                self.snap.delta.ins[Perm::Pso.index()].contains(&r.pso())
            }
            && self.quads.insert(*r)
    }

    /// Whether a row taken from the queue goes out: tail rows are checked now.
    pub(crate) fn emit(&mut self, r: &Row, from_tail: bool) -> bool {
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
    rechecked: Rechecked<'a>,
}

impl<'a> Literals<'a> {
    fn new(snap: &'a Snapshot) -> Literals<'a> {
        let view = snap.geo.as_deref();
        Literals {
            snap,
            cfg: view.map_or_else(|| Arc::new(GeoConfig::default()), |v| v.config.clone()),
            base: view.and_then(|v| v.base.as_ref()),
            memo: FxHashMap::default(),
            rechecked: Rechecked::new(snap),
        }
    }

    /// The geometry of literal `o`, and whether the index skipped it (a candidate
    /// anyway, see [`Slot::rechecked`]).
    fn entry(&mut self, o: u64) -> Option<(Arc<ColumnEntry>, bool)> {
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
            Slot::Geom(e) => Some((e, false)),
            s if s.rechecked() => self.rechecked.entry(o).map(|e| (e, true)),
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
    let lat = wgs84_lat(snap);
    let mut points = Points::default();
    for &p in preds {
        if Some(p) == lat {
            points.scan(ctx, p, windows, graph, dedup, st, &mut out, sink)?;
            continue;
        }
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
            let Some((e, skipped)) = lits.entry(r.o) else {
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
            if skipped {
                st.rechecked += 1;
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

/// The W3C Basic Geo points a scan finds (without the index), numbered per search.
#[derive(Default)]
struct Points {
    ids: FxHashMap<[u64; 4], u64>,
    values: FxHashMap<u64, Option<f64>>,
}

impl Points {
    /// Hand out (through `out`, flushed to `sink` in chunks) the points of the `lat`
    /// predicate `lat` whose envelope meets one of `windows` (any, without windows).
    #[allow(clippy::too_many_arguments)]
    fn scan(
        &mut self,
        ctx: &Ctx,
        lat: Id,
        windows: Option<&[[f64; 4]]>,
        graph: &GraphFilter,
        dedup: bool,
        st: &mut SearchStats,
        out: &mut Vec<Hit>,
        sink: &mut dyn FnMut(&[Hit]) -> Result<()>,
    ) -> Result<()> {
        let snap = &*ctx.snap;
        let Some(view) = snap.geo.as_deref() else {
            return Ok(());
        };
        let Some((_, long)) = super::wgs84::predicates(snap) else {
            return Ok(());
        };
        let mut lats: Vec<[u64; 4]> = Vec::new();
        snap.scan(Perm::Pso, &[lat.0], |c| {
            match c {
                Chunk::Block(b, s, e) => lats.extend((s..e).map(|i| b.key(i))),
                Chunk::Row(k) => lats.push(k),
            }
            Ok(true)
        })?;
        let mut triples: FxHashSet<(u64, u64)> = FxHashSet::default();
        for (n, k) in lats.into_iter().enumerate() {
            if n % CHUNK == CHUNK - 1 {
                ctx.check()?;
            }
            let (s, lat_o, g) = (k[1], k[2], k[3]);
            if !graph.accepts(g) || !view.lookup.graph(Id(g), snap, &view.config) {
                continue;
            }
            let Some(y) = self.value(snap, lat_o) else {
                continue;
            };
            for long_o in super::wgs84::objects(snap, s, long.0, g)? {
                let Some(e) = self
                    .value(snap, long_o)
                    .and_then(|x| super::wgs84::point(y, x))
                else {
                    continue;
                };
                if let Some(ws) = windows
                    && !ws.iter().any(|w| intersects(&e.bbox(), w))
                {
                    continue;
                }
                let next = super::wgs84::pair_id(self.ids.len() as u64);
                let o = *self.ids.entry([s, g, lat_o, long_o]).or_insert(next);
                if dedup && !triples.insert((s, o)) {
                    continue;
                }
                out.push(Hit::new(&Row { s, p: lat.0, o, g }, e));
                if out.len() >= CHUNK {
                    st.candidates += out.len() as u64;
                    sink(out)?;
                    out.clear();
                }
            }
        }
        Ok(())
    }

    fn value(&mut self, snap: &Snapshot, o: u64) -> Option<f64> {
        *self
            .values
            .entry(o)
            .or_insert_with(|| super::wgs84::number(snap, Id(o)))
    }
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
    st.rechecked += scan.rechecked;
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
