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

use super::column::{Column, ColumnEntry, Slot, classify_controlled, slot_bytes};
use super::config::GeoConfig;
use super::geom::Geom;
use super::index::{GeoBase, GeoView, Row, intersects};
use super::ops::distance::lower_bound_m;
use super::tree::{PackedTree, Tree};
use crate::error::Result;
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::sparql::ctx::{Charge, Ctx};
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
/// search. `_charge` is declared last so `memo` drops before its retained credit.
pub(crate) struct Rechecked<'a> {
    ctx: &'a Ctx,
    cfg: Arc<GeoConfig>,
    memo: FxHashMap<u64, Option<Arc<ColumnEntry>>>,
    bytes: u64,
    step: u64,
    _charge: Charge<'a>,
}

impl<'a> Rechecked<'a> {
    pub(crate) fn new(ctx: &'a Ctx) -> Result<Rechecked<'a>> {
        Ok(Rechecked {
            ctx,
            cfg: ctx
                .snap
                .geo
                .as_ref()
                .map_or_else(|| Arc::new(GeoConfig::default()), |v| v.config.clone()),
            memo: FxHashMap::default(),
            bytes: 0,
            step: ctx.reservation_step(),
            _charge: ctx.charge(0)?,
        })
    }

    pub(crate) fn entry(&mut self, o: u64) -> Result<Option<Arc<ColumnEntry>>> {
        if let Some(cached) = self.memo.get(&o) {
            return Ok(cached.clone());
        }
        let parse_charge = self.ctx.charge(0)?;
        let slot = classify_controlled(self.ctx, Id(o), &self.cfg, true, &parse_charge)?;
        let entry = match slot {
            Slot::Geom(e) => Some(e),
            _ => None,
        };
        let next = self.bytes.saturating_add(64);
        self._charge.cover(next, self.step)?;
        self.bytes = next;
        self.memo.insert(o, entry.clone());
        drop(parse_charge);
        Ok(entry)
    }

    pub(crate) fn retained_bytes(&self) -> u64 {
        self.memo.capacity() as u64
            * (size_of::<u64>() + size_of::<Option<Arc<ColumnEntry>>>() + 1) as u64
    }

    pub(crate) fn needs_growth(&self, o: u64) -> bool {
        self.memo.len() == self.memo.capacity() && !self.memo.contains_key(&o)
    }

    pub(crate) fn projected_bytes_after_insert(&self) -> u64 {
        next_cap(self.memo.len(), self.memo.capacity(), 1) as u64
            * (size_of::<u64>() + size_of::<Option<Arc<ColumnEntry>>>() + 1) as u64
    }
}

#[inline]
fn next_cap(len: usize, cap: usize, extra: usize) -> usize {
    if len.saturating_add(extra) <= cap {
        cap
    } else {
        cap.saturating_mul(2).max(len.saturating_add(extra)).max(8)
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
    let mut re = Rechecked::new(ctx)?;
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
        if valid && let Some(e) = re.entry(r.o)? {
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
    let mut re = Rechecked::new(ctx)?;
    for r in &base.skipped {
        if f.base_valid(r)
            && let Some(en) = re.entry(r.o)?
        {
            st.rechecked += 1;
            heap.push(Entry::row(&lb, r, en));
        }
    }
    for r in view.overlay.skipped.iter().chain(view.skipped.iter()) {
        if let Some(en) = re.entry(r.o)? {
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
    ctx: &'a Ctx,
    cfg: Arc<GeoConfig>,
    base: Option<&'a Arc<GeoBase>>,
    memo: FxHashMap<u64, Slot>,
    rechecked: Rechecked<'a>,
    bytes: u64,
    step: u64,
    _charge: Charge<'a>,
}

impl<'a> Literals<'a> {
    fn new(ctx: &'a Ctx) -> Result<Literals<'a>> {
        let view = ctx.snap.geo.as_deref();
        Ok(Literals {
            ctx,
            cfg: view.map_or_else(|| Arc::new(GeoConfig::default()), |v| v.config.clone()),
            base: view.and_then(|v| v.base.as_ref()),
            memo: FxHashMap::default(),
            rechecked: Rechecked::new(ctx)?,
            bytes: 0,
            step: ctx.reservation_step(),
            _charge: ctx.charge(0)?,
        })
    }

    fn retained_bytes(&self) -> u64 {
        (self.memo.capacity() as u64 * (size_of::<u64>() + size_of::<Slot>() + 1) as u64)
            .saturating_add(self.rechecked.retained_bytes())
    }

    /// The geometry of literal `o`, and whether the index skipped it (a candidate
    /// anyway, see [`Slot::rechecked`]).
    fn entry_charged(
        &mut self,
        o: u64,
        base: u64,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Option<(Arc<ColumnEntry>, bool)>> {
        if !matches!(Id(o).tag(), Tag::Vocab | Tag::Delta) {
            return Ok(None);
        }
        let slot = match self.base.and_then(|b| b.column.get(o)) {
            Some(s) => s,
            None => {
                if let Some(cached) = self.memo.get(&o) {
                    cached.clone()
                } else {
                    if self.memo.len() == self.memo.capacity() {
                        let memo_bytes = next_cap(self.memo.len(), self.memo.capacity(), 1) as u64
                            * (size_of::<u64>() + size_of::<Slot>() + 1) as u64;
                        admit(
                            base.saturating_add(memo_bytes)
                                .saturating_add(self.rechecked.retained_bytes()),
                        )?;
                    }
                    let parse_charge = self.ctx.charge(0)?;
                    let slot =
                        classify_controlled(self.ctx, Id(o), &self.cfg, false, &parse_charge)?;
                    let next = self.bytes.saturating_add(slot_bytes(&slot));
                    self._charge.cover(next, self.step)?;
                    self.bytes = next;
                    self.memo.insert(o, slot.clone());
                    drop(parse_charge);
                    slot
                }
            }
        };
        match slot {
            Slot::Geom(e) => Ok(Some((e, false))),
            s if s.rechecked() => {
                if self.rechecked.needs_growth(o) {
                    let memo_bytes = self.memo.capacity() as u64
                        * (size_of::<u64>() + size_of::<Slot>() + 1) as u64;
                    admit(
                        base.saturating_add(memo_bytes)
                            .saturating_add(self.rechecked.projected_bytes_after_insert()),
                    )?;
                }
                Ok(self.rechecked.entry(o)?.map(|e| (e, true)))
            }
            _ => Ok(None),
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
    scan_fallback_charged(ctx, preds, windows, graph, dedup, st, &mut |_| Ok(()), sink)
}

#[allow(clippy::too_many_arguments)]
fn scan_fallback_charged(
    ctx: &Ctx,
    preds: &[Id],
    windows: Option<&[[f64; 4]]>,
    graph: &GraphFilter,
    dedup: bool,
    st: &mut SearchStats,
    admit: &mut dyn FnMut(u64) -> Result<()>,
    sink: &mut dyn FnMut(&[Hit]) -> Result<()>,
) -> Result<()> {
    st.fallback = true;
    let snap = &*ctx.snap;
    let view = snap.geo.as_deref();
    let mut lits = Literals::new(ctx)?;
    let mut triples: FxHashSet<(u64, u64, u64)> = FxHashSet::default();
    let mut out: Vec<Hit> = Vec::new();
    let mut n = 0usize;
    let mut err: Option<crate::error::Error> = None;
    let lat = wgs84_lat(snap);
    let mut points = Points::default();
    for &p in preds {
        if Some(p) == lat {
            let base = lits
                .retained_bytes()
                .saturating_add(
                    triples.capacity() as u64 * (size_of::<(u64, u64, u64)>() + 1) as u64,
                )
                .saturating_add(out.capacity() as u64 * size_of::<Hit>() as u64);
            points.scan_charged(
                ctx,
                p,
                windows,
                graph,
                dedup,
                st,
                &mut out,
                &mut |b| admit(base.saturating_add(b)),
                sink,
            )?;
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
            let fixed = (triples.capacity() as u64 * (size_of::<(u64, u64, u64)>() + 1) as u64)
                .saturating_add(out.capacity() as u64 * size_of::<Hit>() as u64);
            let Some((e, skipped)) = lits.entry_charged(r.o, fixed, admit)? else {
                return Ok(());
            };
            if let Some(ws) = windows
                && !ws.iter().any(|w| intersects(&e.bbox(), w))
            {
                return Ok(());
            }
            if dedup {
                if triples.len() == triples.capacity() && !triples.contains(&(r.s, r.p, r.o)) {
                    let t_bytes = next_cap(triples.len(), triples.capacity(), 1) as u64
                        * (size_of::<(u64, u64, u64)>() + 1) as u64;
                    admit(
                        lits.retained_bytes()
                            .saturating_add(t_bytes)
                            .saturating_add(out.capacity() as u64 * size_of::<Hit>() as u64),
                    )?;
                }
                if !triples.insert((r.s, r.p, r.o)) {
                    return Ok(());
                }
            }
            if skipped {
                st.rechecked += 1;
            }
            if out.len() == out.capacity() {
                let out_bytes =
                    next_cap(out.len(), out.capacity(), 1) as u64 * size_of::<Hit>() as u64;
                let t_bytes = triples.capacity() as u64 * (size_of::<(u64, u64, u64)>() + 1) as u64;
                admit(
                    lits.retained_bytes()
                        .saturating_add(t_bytes)
                        .saturating_add(out_bytes),
                )?;
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
    fn retained_bytes(&self) -> u64 {
        (self.ids.capacity() as u64 * (size_of::<[u64; 4]>() + size_of::<u64>() + 1) as u64)
            .saturating_add(
                self.values.capacity() as u64
                    * (size_of::<u64>() + size_of::<Option<f64>>() + 1) as u64,
            )
    }

    /// Hand out (through `out`, flushed to `sink` in chunks) the points of the `lat`
    /// predicate `lat` whose envelope meets one of `windows` (any, without windows).
    #[allow(clippy::too_many_arguments)]
    fn scan_charged(
        &mut self,
        ctx: &Ctx,
        lat: Id,
        windows: Option<&[[f64; 4]]>,
        graph: &GraphFilter,
        dedup: bool,
        st: &mut SearchStats,
        out: &mut Vec<Hit>,
        admit: &mut dyn FnMut(u64) -> Result<()>,
        sink: &mut dyn FnMut(&[Hit]) -> Result<()>,
    ) -> Result<()> {
        let snap = &*ctx.snap;
        let Some(view) = snap.geo.as_deref() else {
            return Ok(());
        };
        let Some((_, long)) = super::wgs84::predicates(snap) else {
            return Ok(());
        };
        let mut triples: FxHashSet<(u64, u64)> = FxHashSet::default();
        let mut lo = Some([lat.0, 0, 0, 0]);
        let hi = [lat.0, u64::MAX, u64::MAX, u64::MAX];
        let mut lats: Vec<[u64; 4]> = Vec::new();
        while let Some(start) = lo {
            ctx.check()?;
            lats.clear();
            let mut last_seen = None;
            let mut stopped = false;
            snap.scan_between(Perm::Pso, start, hi, |c| {
                match c {
                    Chunk::Block(b, s, e) => {
                        for i in s..e {
                            let k = b.key(i);
                            last_seen = Some(k);
                            lats.push(k);
                            if lats.len() >= 256 {
                                stopped = true;
                                return Ok(false);
                            }
                        }
                    }
                    Chunk::Row(k) => {
                        last_seen = Some(k);
                        lats.push(k);
                        if lats.len() >= 256 {
                            stopped = true;
                            return Ok(false);
                        }
                    }
                }
                Ok(true)
            })?;
            lo = if stopped {
                last_seen.and_then(next_key_after)
            } else {
                None
            };
            let lats_bytes = lats.capacity() as u64 * size_of::<[u64; 4]>() as u64;
            for &k in &lats {
                let (s, lat_o, g) = (k[1], k[2], k[3]);
                if !graph.accepts(g) || !view.lookup.graph(Id(g), snap, &view.config) {
                    continue;
                }
                let t_bytes = triples.capacity() as u64 * (size_of::<(u64, u64)>() + 1) as u64;
                let out_bytes = out.capacity() as u64 * size_of::<Hit>() as u64;
                let Some(y) = self.value_charged(
                    snap,
                    lat_o,
                    lats_bytes.saturating_add(t_bytes).saturating_add(out_bytes),
                    admit,
                )?
                else {
                    continue;
                };
                for long_o in super::wgs84::objects(snap, s, long.0, g)? {
                    let Some(x) = self.value_charged(
                        snap,
                        long_o,
                        lats_bytes.saturating_add(t_bytes).saturating_add(out_bytes),
                        admit,
                    )?
                    else {
                        continue;
                    };
                    let Some(e) = super::wgs84::point(y, x) else {
                        continue;
                    };
                    if let Some(ws) = windows
                        && !ws.iter().any(|w| intersects(&e.bbox(), w))
                    {
                        continue;
                    }
                    let id_key = [s, g, lat_o, long_o];
                    if self.ids.len() == self.ids.capacity() && !self.ids.contains_key(&id_key) {
                        let next_ids = next_cap(self.ids.len(), self.ids.capacity(), 1) as u64
                            * (size_of::<[u64; 4]>() + size_of::<u64>() + 1) as u64;
                        let val_bytes = self.values.capacity() as u64
                            * (size_of::<u64>() + size_of::<Option<f64>>() + 1) as u64;
                        admit(
                            next_ids
                                .saturating_add(val_bytes)
                                .saturating_add(lats_bytes)
                                .saturating_add(t_bytes)
                                .saturating_add(out_bytes),
                        )?;
                    }
                    let next = super::wgs84::pair_id(self.ids.len() as u64);
                    let o = *self.ids.entry(id_key).or_insert(next);
                    if dedup {
                        if triples.len() == triples.capacity() && !triples.contains(&(s, o)) {
                            let next_t = next_cap(triples.len(), triples.capacity(), 1) as u64
                                * (size_of::<(u64, u64)>() + 1) as u64;
                            admit(
                                self.retained_bytes()
                                    .saturating_add(lats_bytes)
                                    .saturating_add(next_t)
                                    .saturating_add(out_bytes),
                            )?;
                        }
                        if !triples.insert((s, o)) {
                            continue;
                        }
                    }
                    if out.len() == out.capacity() {
                        let next_out =
                            next_cap(out.len(), out.capacity(), 1) as u64 * size_of::<Hit>() as u64;
                        admit(
                            self.retained_bytes()
                                .saturating_add(lats_bytes)
                                .saturating_add(
                                    triples.capacity() as u64
                                        * (size_of::<(u64, u64)>() + 1) as u64,
                                )
                                .saturating_add(next_out),
                        )?;
                    }
                    out.push(Hit::new(&Row { s, p: lat.0, o, g }, e));
                    if out.len() >= CHUNK {
                        st.candidates += out.len() as u64;
                        sink(out)?;
                        out.clear();
                    }
                }
            }
        }
        Ok(())
    }

    fn value_charged(
        &mut self,
        snap: &Snapshot,
        o: u64,
        base: u64,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Option<f64>> {
        if let Some(&v) = self.values.get(&o) {
            return Ok(v);
        }
        if self.values.len() == self.values.capacity() {
            let ids_bytes =
                self.ids.capacity() as u64 * (size_of::<[u64; 4]>() + size_of::<u64>() + 1) as u64;
            let val_bytes = next_cap(self.values.len(), self.values.capacity(), 1) as u64
                * (size_of::<u64>() + size_of::<Option<f64>>() + 1) as u64;
            admit(base.saturating_add(ids_bytes).saturating_add(val_bytes))?;
        }
        let v = super::wgs84::number(snap, Id(o));
        self.values.insert(o, v);
        Ok(v)
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

/// The smallest key strictly greater than `k`, or `None` at `[u64::MAX; 4]`.
pub(crate) fn next_key_after(mut k: [u64; 4]) -> Option<[u64; 4]> {
    for c in (0..4).rev() {
        if k[c] < u64::MAX {
            k[c] += 1;
            k[c + 1..].fill(0);
            return Some(k);
        }
    }
    None
}

/// Visit a key range until exhausted or the visitor stops. Poll before scanning and
/// during blocks, including keys the visitor rejects.
pub(super) fn visit_keys(
    snap: &Snapshot,
    perm: Perm,
    lo: [u64; 4],
    hi: [u64; 4],
    check: impl Fn() -> Result<()>,
    mut each: impl FnMut([u64; 4]) -> Result<bool>,
) -> Result<Option<[u64; 4]>> {
    if lo > hi {
        return Ok(None);
    }
    let mut ticks = 0usize;
    let mut resume = None;
    snap.scan_between_cols_checked(perm, lo, hi, crate::index::ALL_COLS, &check, |chunk| {
        let mut visit = |key| {
            ticks += 1;
            if ticks.is_multiple_of(256) {
                check()?;
            }
            if each(key)? {
                Ok(true)
            } else {
                resume = next_key_after(key).filter(|&next| next <= hi);
                Ok(false)
            }
        };
        match chunk {
            Chunk::Block(block, from, to) => {
                for i in from..to {
                    if !visit(block.key(i))? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Chunk::Row(key) => visit(key),
        }
    })?;
    Ok(resume)
}

struct LongitudeScan {
    subject: u64,
    graph: u64,
    latitude: f64,
    next: [u64; 4],
}

/// Resumable, owned, `Send` window search over a snapshot's spatial index (or predicate
/// scan fallback). Holds only the tree DFS stack (or scan cursor key) and dedup/recheck
/// state, reporting its retained bytes through `admit` as it grows.
pub(crate) struct WindowIter {
    snap: Arc<Snapshot>,
    preds: Vec<Id>,
    windows: Vec<[f64; 4]>,
    ws_f32: Vec<[f32; 4]>,
    graph: GraphFilter,
    dedup: bool,
    pub(crate) st: SearchStats,
    mode: WindowMode,
}

enum WindowMode {
    Indexed {
        view: Arc<GeoView>,
        base: Arc<GeoBase>,
        stage: IndexStage,
        pending: std::collections::VecDeque<Hit>,
        triples: FxHashSet<(u64, u64, u64)>,
        quads: FxHashSet<Row>,
        rechecked: FxHashMap<u64, Option<Arc<ColumnEntry>>>,
    },
    Fallback {
        pred_at: usize,
        lo: Option<[u64; 4]>,
        pending: std::collections::VecDeque<Hit>,
        memo: FxHashMap<u64, Slot>,
        rechecked: FxHashMap<u64, Option<Arc<ColumnEntry>>>,
        triples: FxHashSet<(u64, u64, u64)>,
        longitude: Option<LongitudeScan>,
        pair_at: u64,
        lat_done: bool,
        cfg: Arc<GeoConfig>,
        lat: Option<Id>,
    },
}

enum IndexStage {
    BaseTree { stack: Vec<usize> },
    OverlayTree { stack: Vec<usize> },
    Tail { at: usize },
    BaseSkipped { at: usize },
    OverlaySkipped { at: usize },
    ViewSkipped { at: usize },
    Done,
}

impl WindowIter {
    pub(crate) fn open(
        ctx: &Ctx,
        preds: Vec<Id>,
        windows: Vec<[f64; 4]>,
        graph: GraphFilter,
        dedup: bool,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Self> {
        let snap = ctx.snap.clone();
        let ws_f32: Vec<[f32; 4]> = windows.iter().map(super::index::window_f32).collect();
        let mut st = SearchStats::default();
        let mode = if let Some((_, base)) = indexed(&snap, &preds) {
            let view = snap.geo.clone().expect("indexed snapshot has geo view");
            let base = base.clone();
            let stage = match &base.tree {
                Some(t) => {
                    admit(size_of::<usize>() as u64)?;
                    IndexStage::BaseTree {
                        stack: vec![t.tree().root()],
                    }
                }
                None => match &view.overlay.tree {
                    Some(t) => {
                        admit(size_of::<usize>() as u64)?;
                        IndexStage::OverlayTree {
                            stack: vec![t.tree().root()],
                        }
                    }
                    None => IndexStage::Tail { at: 0 },
                },
            };
            WindowMode::Indexed {
                view,
                base,
                stage,
                pending: std::collections::VecDeque::new(),
                triples: FxHashSet::default(),
                quads: FxHashSet::default(),
                rechecked: FxHashMap::default(),
            }
        } else {
            st.fallback = true;
            let cfg = snap
                .geo
                .as_ref()
                .map_or_else(|| Arc::new(GeoConfig::default()), |v| v.config.clone());
            let lat = wgs84_lat(&snap);
            WindowMode::Fallback {
                pred_at: 0,
                lo: None,
                pending: std::collections::VecDeque::new(),
                memo: FxHashMap::default(),
                rechecked: FxHashMap::default(),
                triples: FxHashSet::default(),
                longitude: None,
                pair_at: 0,
                lat_done: false,
                cfg,
                lat,
            }
        };
        let iter = Self {
            snap,
            preds,
            windows,
            ws_f32,
            graph,
            dedup,
            st,
            mode,
        };
        admit(iter.retained_bytes())?;
        Ok(iter)
    }

    pub(crate) fn retained_bytes(&self) -> u64 {
        match &self.mode {
            WindowMode::Indexed {
                stage,
                pending,
                triples,
                quads,
                rechecked,
                ..
            } => {
                let stack = match stage {
                    IndexStage::BaseTree { stack } | IndexStage::OverlayTree { stack } => {
                        stack.capacity() as u64 * size_of::<usize>() as u64
                    }
                    _ => 0,
                };
                stack
                    .saturating_add(pending.capacity() as u64 * size_of::<Hit>() as u64)
                    .saturating_add(
                        triples.capacity() as u64 * (size_of::<(u64, u64, u64)>() + 1) as u64,
                    )
                    .saturating_add(quads.capacity() as u64 * (size_of::<Row>() + 1) as u64)
                    .saturating_add(
                        rechecked.capacity() as u64
                            * (size_of::<u64>() + size_of::<Option<Arc<ColumnEntry>>>() + 1) as u64,
                    )
            }
            WindowMode::Fallback {
                pending,
                memo,
                rechecked,
                triples,
                ..
            } => fallback_retained_bytes(pending, memo, rechecked, triples),
        }
    }

    /// Pull up to `want` hits into `out`. Returns `true` when the iterator is exhausted.
    pub(crate) fn next_hits(
        &mut self,
        ctx: &Ctx,
        out: &mut Vec<Hit>,
        want: usize,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<bool> {
        let want = want.max(1);
        let start_len = out.len();
        let target_len = start_len.saturating_add(want);
        match &mut self.mode {
            WindowMode::Indexed {
                view,
                base,
                stage,
                pending,
                triples,
                quads,
                rechecked,
            } => {
                let ws_f32 = &self.ws_f32;
                let windows = &self.windows;
                let preds = &self.preds;
                let graph = &self.graph;
                let dedup = self.dedup;
                let snap = &self.snap;
                let hits_f32 = |b: [f32; 4]| {
                    ws_f32
                        .iter()
                        .any(|w| b[0] <= w[2] && b[2] >= w[0] && b[1] <= w[3] && b[3] >= w[1])
                };
                let wanted = |r: &Row| graph.accepts(r.g) && preds.iter().any(|p| p.0 == r.p);
                let pair_live = |r: &Row| base.column.pair(r.o).is_some_and(|p| p.live(snap, r));
                let base_valid = |r: &Row| {
                    if !wanted(r) {
                        return false;
                    }
                    if super::wgs84::is_pair(r.o) {
                        return pair_live(r);
                    }
                    let del = &snap.delta.del[Perm::Pso.index()];
                    del.is_empty() || !del.contains(&r.pso())
                };
                let delta_valid = |r: &Row, quads: &mut FxHashSet<Row>| {
                    wanted(r)
                        && if super::wgs84::is_pair(r.o) {
                            pair_live(r)
                        } else {
                            snap.delta.ins[Perm::Pso.index()].contains(&r.pso())
                        }
                        && quads.insert(*r)
                };
                let mut steps = 0usize;
                let indexed_bytes = |stack_cap: usize,
                                     pending_cap: usize,
                                     triples_cap: usize,
                                     quads_cap: usize,
                                     rechecked_cap: usize|
                 -> u64 {
                    (stack_cap as u64 * size_of::<usize>() as u64)
                        .saturating_add(pending_cap as u64 * size_of::<Hit>() as u64)
                        .saturating_add(
                            triples_cap as u64 * (size_of::<(u64, u64, u64)>() + 1) as u64,
                        )
                        .saturating_add(quads_cap as u64 * (size_of::<Row>() + 1) as u64)
                        .saturating_add(
                            rechecked_cap as u64
                                * (size_of::<u64>() + size_of::<Option<Arc<ColumnEntry>>>() + 1)
                                    as u64,
                        )
                };
                loop {
                    while out.len() < target_len {
                        let Some(h) = pending.pop_front() else {
                            break;
                        };
                        self.st.candidates += 1;
                        out.push(h);
                    }
                    if out.len() >= target_len {
                        let bytes = self.retained_bytes();
                        admit(bytes)?;
                        return Ok(false);
                    }
                    steps += 1;
                    if steps.is_multiple_of(CHUNK) {
                        ctx.check()?;
                    }
                    let stack_cap = match stage {
                        IndexStage::BaseTree { stack } | IndexStage::OverlayTree { stack } => {
                            stack.capacity()
                        }
                        _ => 0,
                    };
                    match stage {
                        IndexStage::BaseTree { stack } => {
                            let Some(t_packed) = &base.tree else {
                                *stage = match &view.overlay.tree {
                                    Some(t) => {
                                        admit(indexed_bytes(
                                            1,
                                            pending.capacity(),
                                            triples.capacity(),
                                            quads.capacity(),
                                            rechecked.capacity(),
                                        ))?;
                                        IndexStage::OverlayTree {
                                            stack: vec![t.tree().root()],
                                        }
                                    }
                                    None => IndexStage::Tail { at: 0 },
                                };
                                continue;
                            };
                            let Some(n) = stack.pop() else {
                                *stage = match &view.overlay.tree {
                                    Some(t) => {
                                        admit(indexed_bytes(
                                            1,
                                            pending.capacity(),
                                            triples.capacity(),
                                            quads.capacity(),
                                            rechecked.capacity(),
                                        ))?;
                                        IndexStage::OverlayTree {
                                            stack: vec![t.tree().root()],
                                        }
                                    }
                                    None => IndexStage::Tail { at: 0 },
                                };
                                continue;
                            };
                            let t = t_packed.tree();
                            if t.is_item(n) {
                                if hits_f32(t.bbox(n)) {
                                    let r = &base.rows[t.item(n)];
                                    if base_valid(r) {
                                        if dedup
                                            && triples.len() == triples.capacity()
                                            && !triples.contains(&(r.s, r.p, r.o))
                                        {
                                            admit(indexed_bytes(
                                                stack.capacity(),
                                                pending.capacity(),
                                                next_cap(triples.len(), triples.capacity(), 1),
                                                quads.capacity(),
                                                rechecked.capacity(),
                                            ))?;
                                        }
                                        if (!dedup || triples.insert((r.s, r.p, r.o)))
                                            && let Some(e) = base.column.base_entry(r.o)
                                        {
                                            if pending.len() == pending.capacity() {
                                                admit(indexed_bytes(
                                                    stack.capacity(),
                                                    next_cap(pending.len(), pending.capacity(), 1),
                                                    triples.capacity(),
                                                    quads.capacity(),
                                                    rechecked.capacity(),
                                                ))?;
                                            }
                                            pending.push_back(Hit::new(r, e.clone()));
                                        }
                                    }
                                }
                            } else {
                                self.st.nodes += 1;
                                for c in t.children(n) {
                                    if !hits_f32(t.bbox(c)) {
                                        continue;
                                    }
                                    if t.is_item(c) {
                                        let r = &base.rows[t.item(c)];
                                        if base_valid(r) {
                                            if dedup
                                                && triples.len() == triples.capacity()
                                                && !triples.contains(&(r.s, r.p, r.o))
                                            {
                                                admit(indexed_bytes(
                                                    stack.capacity(),
                                                    pending.capacity(),
                                                    next_cap(triples.len(), triples.capacity(), 1),
                                                    quads.capacity(),
                                                    rechecked.capacity(),
                                                ))?;
                                            }
                                            if (!dedup || triples.insert((r.s, r.p, r.o)))
                                                && let Some(e) = base.column.base_entry(r.o)
                                            {
                                                if pending.len() == pending.capacity() {
                                                    admit(indexed_bytes(
                                                        stack.capacity(),
                                                        next_cap(
                                                            pending.len(),
                                                            pending.capacity(),
                                                            1,
                                                        ),
                                                        triples.capacity(),
                                                        quads.capacity(),
                                                        rechecked.capacity(),
                                                    ))?;
                                                }
                                                pending.push_back(Hit::new(r, e.clone()));
                                            }
                                        }
                                    } else {
                                        if stack.len() == stack.capacity() {
                                            admit(indexed_bytes(
                                                next_cap(stack.len(), stack.capacity(), 1),
                                                pending.capacity(),
                                                triples.capacity(),
                                                quads.capacity(),
                                                rechecked.capacity(),
                                            ))?;
                                        }
                                        stack.push(c);
                                    }
                                }
                            }
                        }
                        IndexStage::OverlayTree { stack } => {
                            let Some(t_packed) = &view.overlay.tree else {
                                *stage = IndexStage::Tail { at: 0 };
                                continue;
                            };
                            let Some(n) = stack.pop() else {
                                *stage = IndexStage::Tail { at: 0 };
                                continue;
                            };
                            let t = t_packed.tree();
                            if t.is_item(n) {
                                if hits_f32(t.bbox(n)) {
                                    let r = &view.overlay.rows[t.item(n)];
                                    if quads.len() == quads.capacity() && !quads.contains(&r.row) {
                                        admit(indexed_bytes(
                                            stack.capacity(),
                                            pending.capacity(),
                                            triples.capacity(),
                                            next_cap(quads.len(), quads.capacity(), 1),
                                            rechecked.capacity(),
                                        ))?;
                                    }
                                    if delta_valid(&r.row, quads) {
                                        if dedup
                                            && triples.len() == triples.capacity()
                                            && !triples.contains(&(r.row.s, r.row.p, r.row.o))
                                        {
                                            admit(indexed_bytes(
                                                stack.capacity(),
                                                pending.capacity(),
                                                next_cap(triples.len(), triples.capacity(), 1),
                                                quads.capacity(),
                                                rechecked.capacity(),
                                            ))?;
                                        }
                                        if !dedup || triples.insert((r.row.s, r.row.p, r.row.o)) {
                                            if pending.len() == pending.capacity() {
                                                admit(indexed_bytes(
                                                    stack.capacity(),
                                                    next_cap(pending.len(), pending.capacity(), 1),
                                                    triples.capacity(),
                                                    quads.capacity(),
                                                    rechecked.capacity(),
                                                ))?;
                                            }
                                            pending.push_back(Hit::new(&r.row, r.entry.clone()));
                                        }
                                    }
                                }
                            } else {
                                self.st.nodes += 1;
                                for c in t.children(n) {
                                    if !hits_f32(t.bbox(c)) {
                                        continue;
                                    }
                                    if t.is_item(c) {
                                        let r = &view.overlay.rows[t.item(c)];
                                        if quads.len() == quads.capacity()
                                            && !quads.contains(&r.row)
                                        {
                                            admit(indexed_bytes(
                                                stack.capacity(),
                                                pending.capacity(),
                                                triples.capacity(),
                                                next_cap(quads.len(), quads.capacity(), 1),
                                                rechecked.capacity(),
                                            ))?;
                                        }
                                        if delta_valid(&r.row, quads) {
                                            if dedup
                                                && triples.len() == triples.capacity()
                                                && !triples.contains(&(r.row.s, r.row.p, r.row.o))
                                            {
                                                admit(indexed_bytes(
                                                    stack.capacity(),
                                                    pending.capacity(),
                                                    next_cap(triples.len(), triples.capacity(), 1),
                                                    quads.capacity(),
                                                    rechecked.capacity(),
                                                ))?;
                                            }
                                            if !dedup || triples.insert((r.row.s, r.row.p, r.row.o))
                                            {
                                                if pending.len() == pending.capacity() {
                                                    admit(indexed_bytes(
                                                        stack.capacity(),
                                                        next_cap(
                                                            pending.len(),
                                                            pending.capacity(),
                                                            1,
                                                        ),
                                                        triples.capacity(),
                                                        quads.capacity(),
                                                        rechecked.capacity(),
                                                    ))?;
                                                }
                                                pending
                                                    .push_back(Hit::new(&r.row, r.entry.clone()));
                                            }
                                        }
                                    } else {
                                        if stack.len() == stack.capacity() {
                                            admit(indexed_bytes(
                                                next_cap(stack.len(), stack.capacity(), 1),
                                                pending.capacity(),
                                                triples.capacity(),
                                                quads.capacity(),
                                                rechecked.capacity(),
                                            ))?;
                                        }
                                        stack.push(c);
                                    }
                                }
                            }
                        }
                        IndexStage::Tail { at } => {
                            if *at >= view.tail.len() {
                                *stage = IndexStage::BaseSkipped { at: 0 };
                                continue;
                            }
                            let r = &view.tail[*at];
                            *at += 1;
                            let b = r.entry.bbox();
                            if windows.iter().any(|w| intersects(&b, w)) {
                                if quads.len() == quads.capacity() && !quads.contains(&r.row) {
                                    admit(indexed_bytes(
                                        stack_cap,
                                        pending.capacity(),
                                        triples.capacity(),
                                        next_cap(quads.len(), quads.capacity(), 1),
                                        rechecked.capacity(),
                                    ))?;
                                }
                                if delta_valid(&r.row, quads) {
                                    if dedup
                                        && triples.len() == triples.capacity()
                                        && !triples.contains(&(r.row.s, r.row.p, r.row.o))
                                    {
                                        admit(indexed_bytes(
                                            stack_cap,
                                            pending.capacity(),
                                            next_cap(triples.len(), triples.capacity(), 1),
                                            quads.capacity(),
                                            rechecked.capacity(),
                                        ))?;
                                    }
                                    if !dedup || triples.insert((r.row.s, r.row.p, r.row.o)) {
                                        if pending.len() == pending.capacity() {
                                            admit(indexed_bytes(
                                                stack_cap,
                                                next_cap(pending.len(), pending.capacity(), 1),
                                                triples.capacity(),
                                                quads.capacity(),
                                                rechecked.capacity(),
                                            ))?;
                                        }
                                        pending.push_back(Hit::new(&r.row, r.entry.clone()));
                                    }
                                }
                            }
                        }
                        IndexStage::BaseSkipped { at } => {
                            if *at >= base.skipped.len() {
                                *stage = IndexStage::OverlaySkipped { at: 0 };
                                continue;
                            }
                            let r = &base.skipped[*at];
                            *at += 1;
                            if base_valid(r) {
                                if dedup
                                    && triples.len() == triples.capacity()
                                    && !triples.contains(&(r.s, r.p, r.o))
                                {
                                    admit(indexed_bytes(
                                        stack_cap,
                                        pending.capacity(),
                                        next_cap(triples.len(), triples.capacity(), 1),
                                        quads.capacity(),
                                        rechecked.capacity(),
                                    ))?;
                                }
                                if !dedup || triples.insert((r.s, r.p, r.o)) {
                                    if rechecked.len() == rechecked.capacity()
                                        && !rechecked.contains_key(&r.o)
                                    {
                                        admit(indexed_bytes(
                                            stack_cap,
                                            pending.capacity(),
                                            triples.capacity(),
                                            quads.capacity(),
                                            next_cap(rechecked.len(), rechecked.capacity(), 1),
                                        ))?;
                                    }
                                    let entry = rechecked
                                        .entry(r.o)
                                        .or_insert_with(|| {
                                            recheck(&snap.key(Id(r.o))?, &view.config)
                                        })
                                        .clone();
                                    if let Some(e) = entry {
                                        self.st.rechecked += 1;
                                        if pending.len() == pending.capacity() {
                                            admit(indexed_bytes(
                                                stack_cap,
                                                next_cap(pending.len(), pending.capacity(), 1),
                                                triples.capacity(),
                                                quads.capacity(),
                                                rechecked.capacity(),
                                            ))?;
                                        }
                                        pending.push_back(Hit::new(r, e));
                                    }
                                }
                            }
                        }
                        IndexStage::OverlaySkipped { at } => {
                            if *at >= view.overlay.skipped.len() {
                                *stage = IndexStage::ViewSkipped { at: 0 };
                                continue;
                            }
                            let r = &view.overlay.skipped[*at];
                            *at += 1;
                            if quads.len() == quads.capacity() && !quads.contains(r) {
                                admit(indexed_bytes(
                                    stack_cap,
                                    pending.capacity(),
                                    triples.capacity(),
                                    next_cap(quads.len(), quads.capacity(), 1),
                                    rechecked.capacity(),
                                ))?;
                            }
                            if delta_valid(r, quads) {
                                if dedup
                                    && triples.len() == triples.capacity()
                                    && !triples.contains(&(r.s, r.p, r.o))
                                {
                                    admit(indexed_bytes(
                                        stack_cap,
                                        pending.capacity(),
                                        next_cap(triples.len(), triples.capacity(), 1),
                                        quads.capacity(),
                                        rechecked.capacity(),
                                    ))?;
                                }
                                if !dedup || triples.insert((r.s, r.p, r.o)) {
                                    if rechecked.len() == rechecked.capacity()
                                        && !rechecked.contains_key(&r.o)
                                    {
                                        admit(indexed_bytes(
                                            stack_cap,
                                            pending.capacity(),
                                            triples.capacity(),
                                            quads.capacity(),
                                            next_cap(rechecked.len(), rechecked.capacity(), 1),
                                        ))?;
                                    }
                                    let entry = rechecked
                                        .entry(r.o)
                                        .or_insert_with(|| {
                                            recheck(&snap.key(Id(r.o))?, &view.config)
                                        })
                                        .clone();
                                    if let Some(e) = entry {
                                        self.st.rechecked += 1;
                                        if pending.len() == pending.capacity() {
                                            admit(indexed_bytes(
                                                stack_cap,
                                                next_cap(pending.len(), pending.capacity(), 1),
                                                triples.capacity(),
                                                quads.capacity(),
                                                rechecked.capacity(),
                                            ))?;
                                        }
                                        pending.push_back(Hit::new(r, e));
                                    }
                                }
                            }
                        }
                        IndexStage::ViewSkipped { at } => {
                            if *at >= view.skipped.len() {
                                *stage = IndexStage::Done;
                                continue;
                            }
                            let r = &view.skipped[*at];
                            *at += 1;
                            if quads.len() == quads.capacity() && !quads.contains(r) {
                                admit(indexed_bytes(
                                    stack_cap,
                                    pending.capacity(),
                                    triples.capacity(),
                                    next_cap(quads.len(), quads.capacity(), 1),
                                    rechecked.capacity(),
                                ))?;
                            }
                            if delta_valid(r, quads) {
                                if dedup
                                    && triples.len() == triples.capacity()
                                    && !triples.contains(&(r.s, r.p, r.o))
                                {
                                    admit(indexed_bytes(
                                        stack_cap,
                                        pending.capacity(),
                                        next_cap(triples.len(), triples.capacity(), 1),
                                        quads.capacity(),
                                        rechecked.capacity(),
                                    ))?;
                                }
                                if !dedup || triples.insert((r.s, r.p, r.o)) {
                                    if rechecked.len() == rechecked.capacity()
                                        && !rechecked.contains_key(&r.o)
                                    {
                                        admit(indexed_bytes(
                                            stack_cap,
                                            pending.capacity(),
                                            triples.capacity(),
                                            quads.capacity(),
                                            next_cap(rechecked.len(), rechecked.capacity(), 1),
                                        ))?;
                                    }
                                    let entry = rechecked
                                        .entry(r.o)
                                        .or_insert_with(|| {
                                            recheck(&snap.key(Id(r.o))?, &view.config)
                                        })
                                        .clone();
                                    if let Some(e) = entry {
                                        self.st.rechecked += 1;
                                        if pending.len() == pending.capacity() {
                                            admit(indexed_bytes(
                                                stack_cap,
                                                next_cap(pending.len(), pending.capacity(), 1),
                                                triples.capacity(),
                                                quads.capacity(),
                                                rechecked.capacity(),
                                            ))?;
                                        }
                                        pending.push_back(Hit::new(r, e));
                                    }
                                }
                            }
                        }
                        IndexStage::Done => {
                            let bytes = self.retained_bytes();
                            admit(bytes)?;
                            return Ok(true);
                        }
                    }
                }
            }
            WindowMode::Fallback {
                pred_at,
                lo,
                pending,
                memo,
                rechecked,
                triples,

                cfg,
                lat,
                longitude,
                pair_at,
                lat_done,
            } => {
                let snap = &self.snap;
                let view = snap.geo.as_deref();
                let base_opt = view.and_then(|v| v.base.as_ref());
                let windows = &self.windows;
                let graph = &self.graph;
                let dedup = self.dedup;
                loop {
                    while out.len() < target_len {
                        let Some(h) = pending.pop_front() else {
                            break;
                        };
                        self.st.candidates += 1;
                        out.push(h);
                    }
                    if out.len() >= target_len {
                        let bytes = self.retained_bytes();
                        admit(bytes)?;
                        return Ok(false);
                    }
                    if *pred_at >= self.preds.len() {
                        let bytes = self.retained_bytes();
                        admit(bytes)?;
                        return Ok(true);
                    }
                    let p = self.preds[*pred_at];
                    if Some(p) == *lat {
                        if out.len() > start_len {
                            // Return ordinary entries before producing owned points.
                            admit(fallback_retained_bytes(pending, memo, rechecked, triples))?;
                            return Ok(false);
                        }
                        if dedup && self.preds[..*pred_at].contains(&p) {
                            *pred_at += 1;
                            continue;
                        }
                        let pair = view.zip(super::wgs84::predicates(snap));
                        let Some((v, (_, long))) = pair else {
                            *pred_at += 1;
                            *lo = None;
                            continue;
                        };
                        if longitude.is_none() && !*lat_done {
                            *lo = visit_keys(
                                snap,
                                Perm::Pso,
                                lo.unwrap_or([p.0, 0, 0, 0]),
                                [p.0, u64::MAX, u64::MAX, u64::MAX],
                                || ctx.check(),
                                |[_, subject, object, g]| {
                                    if graph.accepts(g)
                                        && v.lookup.graph(Id(g), snap, &v.config)
                                        && let Some(latitude) =
                                            super::wgs84::number(snap, Id(object))
                                                .filter(|y| (-90.0..=90.0).contains(y))
                                    {
                                        *longitude = Some(LongitudeScan {
                                            subject,
                                            graph: g,
                                            latitude,
                                            next: [subject, long.0, 0, 0],
                                        });
                                        return Ok(false);
                                    }
                                    Ok(true)
                                },
                            )?;
                            *lat_done = lo.is_none();
                        }
                        if let Some(scan) = longitude {
                            let (s, g, y) = (scan.subject, scan.graph, scan.latitude);
                            let base = fallback_retained_bytes(pending, memo, rechecked, triples);
                            let resume = super::wgs84::visit_objects(
                                snap,
                                scan.next,
                                [s, long.0, u64::MAX, u64::MAX],
                                g,
                                || ctx.check(),
                                |object| {
                                    let Some(x) = super::wgs84::number(snap, Id(object))
                                        .filter(|x| (-180.0..=180.0).contains(x))
                                    else {
                                        return Ok(true);
                                    };
                                    let bbox = super::column::round_out([x, y, x, y]);
                                    if !windows.iter().any(|w| intersects(&bbox, w)) {
                                        return Ok(true);
                                    }
                                    let elem = size_of::<Hit>();
                                    let capacity = sparkles_budget::sizes::vec_grown(
                                        out.capacity(),
                                        out.len(),
                                        1,
                                        elem,
                                    );
                                    let growth = if capacity > out.capacity() {
                                        out.capacity()
                                    } else {
                                        0
                                    };
                                    admit(
                                        base.saturating_add(
                                            (capacity + growth) as u64 * elem as u64,
                                        )
                                        .saturating_add(
                                            (out.len() - start_len + 1) as u64
                                                * super::wgs84::POINT_BYTES,
                                        ),
                                    )?;
                                    let entry =
                                        super::wgs84::point(y, x).expect("validated coordinates");
                                    let o = super::wgs84::pair_id(*pair_at);
                                    *pair_at += 1;
                                    self.st.candidates += 1;
                                    out.push(Hit::new(&Row { s, p: p.0, o, g }, entry));
                                    Ok(out.len() < target_len)
                                },
                            )?;
                            if let Some(next) = resume {
                                scan.next = next;
                            } else {
                                *longitude = None;
                            }
                            if out.len() > start_len {
                                // Keep generated points charged until the consumer gets the batch.
                                return Ok(false);
                            }
                        }
                        if *lat_done {
                            *pred_at += 1;
                            *lo = None;
                            *lat_done = false;
                            *pair_at = 0;
                        }
                        continue;
                    }
                    let start_key = lo.unwrap_or([p.0, 0, 0, 0]);
                    let end_key = [p.0, u64::MAX, u64::MAX, u64::MAX];
                    let target_pending = want.clamp(64, CHUNK);
                    let mut last_seen: Option<[u64; 4]> = None;
                    let mut stopped_early = false;
                    let mut scanned = 0usize;
                    let mut err: Option<crate::error::Error> = None;
                    let mut process = |k: [u64; 4]| -> Result<bool> {
                        last_seen = Some(k);
                        scanned += 1;
                        if scanned.is_multiple_of(CHUNK) {
                            ctx.check()?;
                        }
                        let r = Row {
                            s: k[1],
                            p: k[0],
                            o: k[2],
                            g: k[3],
                        };
                        if !graph.accepts(r.g) {
                            return Ok(true);
                        }
                        if let Some(v) = view
                            && !v.lookup.graph(Id(r.g), snap, &v.config)
                        {
                            return Ok(true);
                        }
                        if !matches!(Id(r.o).tag(), Tag::Vocab | Tag::Delta) {
                            return Ok(true);
                        }
                        let slot = match base_opt.and_then(|b| b.column.get(r.o)) {
                            Some(s) => s,
                            None => {
                                if let Some(s) = memo.get(&r.o) {
                                    s.clone()
                                } else {
                                    if memo.len() == memo.capacity() {
                                        let extra = (next_cap(memo.len(), memo.capacity(), 1)
                                            - memo.capacity())
                                            as u64
                                            * (size_of::<u64>() + size_of::<Slot>() + 1) as u64;
                                        admit(
                                            fallback_retained_bytes(
                                                pending, memo, rechecked, triples,
                                            )
                                            .saturating_add(extra),
                                        )?;
                                    }
                                    let s = match snap.key(Id(r.o)) {
                                        Some(k) => classify(&k, cfg),
                                        None => Slot::Other,
                                    };
                                    memo.insert(r.o, s.clone());
                                    s
                                }
                            }
                        };
                        let found = match slot {
                            Slot::Geom(e) => Some((e, false)),
                            s if s.rechecked() => {
                                if rechecked.len() == rechecked.capacity()
                                    && !rechecked.contains_key(&r.o)
                                {
                                    let extra = (next_cap(rechecked.len(), rechecked.capacity(), 1)
                                        - rechecked.capacity())
                                        as u64
                                        * (size_of::<u64>()
                                            + size_of::<Option<Arc<ColumnEntry>>>()
                                            + 1) as u64;
                                    admit(
                                        fallback_retained_bytes(pending, memo, rechecked, triples)
                                            .saturating_add(extra),
                                    )?;
                                }
                                rechecked
                                    .entry(r.o)
                                    .or_insert_with(|| recheck(&snap.key(Id(r.o))?, cfg))
                                    .clone()
                                    .map(|e| (e, true))
                            }
                            _ => None,
                        };
                        let Some((e, skipped)) = found else {
                            return Ok(true);
                        };
                        if !windows.iter().any(|w| intersects(&e.bbox(), w)) {
                            return Ok(true);
                        }
                        if dedup {
                            if triples.len() == triples.capacity()
                                && !triples.contains(&(r.s, r.p, r.o))
                            {
                                let extra = (next_cap(triples.len(), triples.capacity(), 1)
                                    - triples.capacity())
                                    as u64
                                    * (size_of::<(u64, u64, u64)>() + 1) as u64;
                                admit(
                                    fallback_retained_bytes(pending, memo, rechecked, triples)
                                        .saturating_add(extra),
                                )?;
                            }
                            if !triples.insert((r.s, r.p, r.o)) {
                                return Ok(true);
                            }
                        }
                        if skipped {
                            self.st.rechecked += 1;
                        }
                        if pending.len() == pending.capacity() {
                            let extra = (next_cap(pending.len(), pending.capacity(), 1)
                                - pending.capacity())
                                as u64
                                * size_of::<Hit>() as u64;
                            admit(
                                fallback_retained_bytes(pending, memo, rechecked, triples)
                                    .saturating_add(extra),
                            )?;
                        }
                        pending.push_back(Hit::new(&r, e));
                        if pending.len() >= target_pending {
                            stopped_early = true;
                            return Ok(false);
                        }
                        Ok(true)
                    };
                    snap.scan_between(Perm::Pso, start_key, end_key, |c| {
                        let cont = match c {
                            Chunk::Block(b, s, e) => {
                                let mut go = true;
                                for i in s..e {
                                    match process(b.key(i)) {
                                        Ok(true) => {}
                                        Ok(false) => {
                                            go = false;
                                            break;
                                        }
                                        Err(e) => {
                                            err = Some(e);
                                            return Ok(false);
                                        }
                                    }
                                }
                                go
                            }
                            Chunk::Row(k) => match process(k) {
                                Ok(go) => go,
                                Err(e) => {
                                    err = Some(e);
                                    return Ok(false);
                                }
                            },
                        };
                        Ok(cont)
                    })?;
                    if let Some(e) = err {
                        return Err(e);
                    }
                    if stopped_early && let Some(next) = last_seen.and_then(next_key_after) {
                        *lo = Some(next);
                    } else {
                        *pred_at += 1;
                        *lo = None;
                    }
                    let bytes = fallback_retained_bytes(pending, memo, rechecked, triples);
                    admit(bytes)?;
                }
            }
        }
    }
}

fn fallback_retained_bytes(
    pending: &std::collections::VecDeque<Hit>,
    memo: &FxHashMap<u64, Slot>,
    rechecked: &FxHashMap<u64, Option<Arc<ColumnEntry>>>,
    triples: &FxHashSet<(u64, u64, u64)>,
) -> u64 {
    (pending.capacity() as u64 * size_of::<Hit>() as u64)
        .saturating_add(memo.capacity() as u64 * (size_of::<u64>() + size_of::<Slot>() + 1) as u64)
        .saturating_add(
            rechecked.capacity() as u64
                * (size_of::<u64>() + size_of::<Option<Arc<ColumnEntry>>>() + 1) as u64,
        )
        .saturating_add(triples.capacity() as u64 * (size_of::<(u64, u64, u64)>() + 1) as u64)
}

/// Resumable, owned, `Send` nearest-first search over a snapshot's spatial index (or
/// sorted predicate scan fallback).
pub(crate) struct NearestIter {
    snap: Arc<Snapshot>,
    preds: Vec<Id>,
    graph: GraphFilter,
    dedup: bool,
    pub(crate) st: SearchStats,
    mode: NearestMode,
}

enum NearestMode {
    Empty,
    Indexed {
        view: Arc<GeoView>,
        base: Arc<GeoBase>,
        lb: Bound,
        heap: BinaryHeap<Entry>,
        triples: FxHashSet<(u64, u64, u64)>,
        quads: FxHashSet<Row>,
        size: usize,
    },
    Fallback {
        all: Vec<(f64, Hit)>,
        at: usize,
        size: usize,
    },
}

impl NearestIter {
    pub(crate) fn open(
        ctx: &Ctx,
        preds: Vec<Id>,
        q: &Geom,
        graph: GraphFilter,
        dedup: bool,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Self> {
        let snap = ctx.snap.clone();
        let mut st = SearchStats::default();
        let Some(lb) = Bound::of(q) else {
            return Ok(Self {
                snap,
                preds,
                graph,
                dedup,
                st,
                mode: NearestMode::Empty,
            });
        };
        let mode = if let Some((_, base)) = indexed(&snap, &preds) {
            let view = snap.geo.clone().expect("indexed snapshot has geo view");
            let base = base.clone();
            let mut heap: BinaryHeap<Entry> = BinaryHeap::new();
            let push_heap = |heap: &mut BinaryHeap<Entry>,
                             e: Entry,
                             re_bytes: u64,
                             admit: &mut dyn FnMut(u64) -> Result<()>|
             -> Result<()> {
                if heap.len() == heap.capacity() {
                    let bytes =
                        next_cap(heap.len(), heap.capacity(), 1) as u64 * size_of::<Entry>() as u64;
                    admit(bytes.saturating_add(re_bytes))?;
                }
                heap.push(e);
                Ok(())
            };
            if let Some(t) = base.tree.as_ref().map(PackedTree::tree) {
                push_heap(
                    &mut heap,
                    Entry::node(&lb, &t, t.root(), Src::Base),
                    0,
                    admit,
                )?;
            }
            if let Some(t) = view.overlay.tree.as_ref().map(PackedTree::tree) {
                push_heap(
                    &mut heap,
                    Entry::node(&lb, &t, t.root(), Src::Overlay),
                    0,
                    admit,
                )?;
            }
            for (idx, r) in view.tail.iter().enumerate() {
                if idx.is_multiple_of(CHUNK) {
                    ctx.check()?;
                }
                let mut e = Entry::row(&lb, &r.row, r.entry.clone());
                e.from_tail = true;
                push_heap(&mut heap, e, 0, admit)?;
            }
            let f = Filter::new(&snap, &preds, &graph, dedup);
            let mut re = Rechecked::new(&snap);
            for (idx, r) in base.skipped.iter().enumerate() {
                if idx.is_multiple_of(CHUNK) {
                    ctx.check()?;
                }
                if f.base_valid(r) {
                    if re.needs_growth(r.o) {
                        let heap_bytes = heap.capacity() as u64 * size_of::<Entry>() as u64;
                        admit(heap_bytes.saturating_add(re.projected_bytes_after_insert()))?;
                    }
                    if let Some(en) = re.entry(r.o) {
                        st.rechecked += 1;
                        push_heap(
                            &mut heap,
                            Entry::row(&lb, r, en),
                            re.retained_bytes(),
                            admit,
                        )?;
                    }
                }
            }
            for (idx, r) in view
                .overlay
                .skipped
                .iter()
                .chain(view.skipped.iter())
                .enumerate()
            {
                if idx.is_multiple_of(CHUNK) {
                    ctx.check()?;
                }
                if re.needs_growth(r.o) {
                    let heap_bytes = heap.capacity() as u64 * size_of::<Entry>() as u64;
                    admit(heap_bytes.saturating_add(re.projected_bytes_after_insert()))?;
                }
                if let Some(en) = re.entry(r.o) {
                    st.rechecked += 1;
                    let mut e = Entry::row(&lb, r, en);
                    e.from_tail = true;
                    push_heap(&mut heap, e, re.retained_bytes(), admit)?;
                }
            }
            NearestMode::Indexed {
                view,
                base,
                lb,
                heap,
                triples: FxHashSet::default(),
                quads: FxHashSet::default(),
                size: 16,
            }
        } else {
            let mut all: Vec<(f64, Hit)> = Vec::new();
            let mut scan = SearchStats::default();
            let all_cap_bytes = std::cell::Cell::new(0u64);
            let scan_scratch_bytes = std::cell::Cell::new(0u64);
            let admit_cell = std::cell::RefCell::new(&mut *admit);
            scan_fallback_charged(
                ctx,
                &preds,
                None,
                &graph,
                dedup,
                &mut scan,
                &mut |scratch| {
                    scan_scratch_bytes.set(scratch);
                    (admit_cell.borrow_mut())(all_cap_bytes.get().saturating_add(scratch))
                },
                &mut |hits| {
                    let needed = all.len().saturating_add(hits.len());
                    if needed > all.capacity() {
                        let next = next_cap(all.len(), all.capacity(), hits.len());
                        let next_bytes = next as u64 * size_of::<(f64, Hit)>() as u64;
                        (admit_cell.borrow_mut())(
                            next_bytes.saturating_add(scan_scratch_bytes.get()),
                        )?;
                    }
                    all.extend(hits.iter().map(|h| (lb.of_box(h.entry.bbox()), h.clone())));
                    all_cap_bytes.set(all.capacity() as u64 * size_of::<(f64, Hit)>() as u64);
                    (admit_cell.borrow_mut())(
                        all_cap_bytes.get().saturating_add(scan_scratch_bytes.get()),
                    )
                },
            )?;
            st.fallback = true;
            st.rechecked += scan.rechecked;
            all.sort_by(|a, b| {
                a.0.total_cmp(&b.0)
                    .then_with(|| (a.1.s, a.1.o, a.1.g).cmp(&(b.1.s, b.1.o, b.1.g)))
            });
            NearestMode::Fallback {
                all,
                at: 0,
                size: 16,
            }
        };
        let iter = Self {
            snap,
            preds,
            graph,
            dedup,
            st,
            mode,
        };
        admit(iter.retained_bytes())?;
        Ok(iter)
    }

    /// Construct a deterministic fallback iterator for unit tests that exercise
    /// chunk-to-candidate transfer admission.
    #[cfg(test)]
    pub(crate) fn fallback_for_test(
        snap: Arc<Snapshot>,
        hits: Vec<Hit>,
        first_chunk: usize,
    ) -> Self {
        Self {
            snap,
            preds: Vec::new(),
            graph: GraphFilter::All,
            dedup: false,
            st: SearchStats::default(),
            mode: NearestMode::Fallback {
                all: hits
                    .into_iter()
                    .enumerate()
                    .map(|(i, h)| (i as f64, h))
                    .collect(),
                at: 0,
                size: first_chunk,
            },
        }
    }

    pub(crate) fn retained_bytes(&self) -> u64 {
        match &self.mode {
            NearestMode::Empty => 0,
            NearestMode::Indexed {
                heap,
                triples,
                quads,
                ..
            } => (heap.capacity() as u64 * size_of::<Entry>() as u64)
                .saturating_add(
                    triples.capacity() as u64 * (size_of::<(u64, u64, u64)>() + 1) as u64,
                )
                .saturating_add(quads.capacity() as u64 * (size_of::<Row>() + 1) as u64),
            NearestMode::Fallback { all, .. } => (all.capacity() * size_of::<(f64, Hit)>()) as u64,
        }
    }

    /// Pull the next chunk of nearest-first hits into `out` and return the lower bound
    /// (in metres) on every hit not yet returned (`f64::INFINITY` if exhausted), or
    /// `None` if no more hits exist.
    pub(crate) fn next_chunk(
        &mut self,
        ctx: &Ctx,
        out: &mut Vec<Hit>,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Option<f64>> {
        out.clear();
        let rows = match &self.mode {
            NearestMode::Empty => 0,
            NearestMode::Fallback { all, at, size } => (all.len() - *at).min(*size),
            NearestMode::Indexed { size, .. } => *size,
        };
        let elem = size_of::<Hit>() as u64;
        let retained = self.retained_bytes();
        if rows > out.capacity() {
            admit(retained.saturating_add((rows + out.capacity()) as u64 * elem))?;
            out.reserve_exact(rows);
        }
        let chunk_bytes = out.capacity() as u64 * elem;
        let mut admit = |bytes: u64| admit(bytes.saturating_add(chunk_bytes));
        admit(retained)?;
        match &mut self.mode {
            NearestMode::Empty => Ok(None),
            NearestMode::Fallback { all, at, size } => {
                if *at >= all.len() {
                    return Ok(None);
                }
                ctx.check()?;
                let end = (*at + *size).min(all.len());
                out.extend(all[*at..end].iter().map(|(_, h)| h.clone()));
                *at = end;
                *size = (*size * 2).min(CHUNK);
                self.st.candidates += out.len() as u64;
                let bound = all.get(*at).map_or(f64::INFINITY, |(b, _)| *b);
                Ok(Some(bound))
            }
            NearestMode::Indexed {
                view,
                base,
                lb,
                heap,
                triples,
                quads,
                size,
            } => {
                let snap = &self.snap;
                let preds = &self.preds;
                let graph = &self.graph;
                let dedup = self.dedup;
                let wanted = |r: &Row| graph.accepts(r.g) && preds.iter().any(|p| p.0 == r.p);
                let pair_live = |r: &Row| base.column.pair(r.o).is_some_and(|p| p.live(snap, r));
                let base_valid = |r: &Row| {
                    if !wanted(r) {
                        return false;
                    }
                    if super::wgs84::is_pair(r.o) {
                        return pair_live(r);
                    }
                    let del = &snap.delta.del[Perm::Pso.index()];
                    del.is_empty() || !del.contains(&r.pso())
                };
                let delta_valid = |r: &Row, quads: &mut FxHashSet<Row>| {
                    wanted(r)
                        && if super::wgs84::is_pair(r.o) {
                            pair_live(r)
                        } else {
                            snap.delta.ins[Perm::Pso.index()].contains(&r.pso())
                        }
                        && quads.insert(*r)
                };
                let trees = [
                    base.tree.as_ref().map(PackedTree::tree),
                    view.overlay.tree.as_ref().map(PackedTree::tree),
                ];
                let nearest_bytes =
                    |heap_cap: usize, triples_cap: usize, quads_cap: usize| -> u64 {
                        (heap_cap as u64 * size_of::<Entry>() as u64)
                            .saturating_add(
                                triples_cap as u64 * (size_of::<(u64, u64, u64)>() + 1) as u64,
                            )
                            .saturating_add(quads_cap as u64 * (size_of::<Row>() + 1) as u64)
                    };
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
                            let is_item = t.is_item(n);
                            self.st.nodes += u64::from(!is_item);
                            let children = is_item
                                .then_some(n)
                                .into_iter()
                                .chain((!is_item).then(|| t.children(n)).into_iter().flatten());
                            for c in children {
                                if !t.is_item(c) {
                                    if heap.len() == heap.capacity() {
                                        admit(nearest_bytes(
                                            next_cap(heap.len(), heap.capacity(), 1),
                                            triples.capacity(),
                                            quads.capacity(),
                                        ))?;
                                    }
                                    heap.push(Entry::node(lb, t, c, src));
                                    continue;
                                }
                                let i = t.item(c);
                                match src {
                                    Src::Base => {
                                        let r = &base.rows[i];
                                        if base_valid(r)
                                            && let Some(en) = base.column.base_entry(r.o)
                                        {
                                            if heap.len() == heap.capacity() {
                                                admit(nearest_bytes(
                                                    next_cap(heap.len(), heap.capacity(), 1),
                                                    triples.capacity(),
                                                    quads.capacity(),
                                                ))?;
                                            }
                                            heap.push(Entry::row(lb, r, en.clone()));
                                        }
                                    }
                                    Src::Overlay => {
                                        let r = &view.overlay.rows[i];
                                        if quads.len() == quads.capacity()
                                            && !quads.contains(&r.row)
                                        {
                                            admit(nearest_bytes(
                                                heap.capacity(),
                                                triples.capacity(),
                                                next_cap(quads.len(), quads.capacity(), 1),
                                            ))?;
                                        }
                                        if delta_valid(&r.row, quads) {
                                            if heap.len() == heap.capacity() {
                                                admit(nearest_bytes(
                                                    next_cap(heap.len(), heap.capacity(), 1),
                                                    triples.capacity(),
                                                    quads.capacity(),
                                                ))?;
                                            }
                                            heap.push(Entry::row(lb, &r.row, r.entry.clone()));
                                        }
                                    }
                                }
                            }
                        }
                        Item::Row(r, entry) => {
                            if e.from_tail {
                                if quads.len() == quads.capacity() && !quads.contains(&r) {
                                    admit(nearest_bytes(
                                        heap.capacity(),
                                        triples.capacity(),
                                        next_cap(quads.len(), quads.capacity(), 1),
                                    ))?;
                                }
                                if !delta_valid(&r, quads) {
                                    continue;
                                }
                            }
                            if dedup {
                                if triples.len() == triples.capacity()
                                    && !triples.contains(&(r.s, r.p, r.o))
                                {
                                    admit(nearest_bytes(
                                        heap.capacity(),
                                        next_cap(triples.len(), triples.capacity(), 1),
                                        quads.capacity(),
                                    ))?;
                                }
                                if !triples.insert((r.s, r.p, r.o)) {
                                    continue;
                                }
                            }
                            out.push(Hit::new(&r, entry));
                            if out.len() >= *size {
                                let bound = heap.peek().map_or(f64::INFINITY, |e| e.lb);
                                self.st.candidates += out.len() as u64;
                                *size = (*size * 2).min(CHUNK);
                                let bytes = self.retained_bytes();
                                admit(bytes)?;
                                return Ok(Some(bound));
                            }
                        }
                    }
                }
                let bytes = self.retained_bytes();
                admit(bytes)?;
                if out.is_empty() {
                    Ok(None)
                } else {
                    self.st.candidates += out.len() as u64;
                    Ok(Some(f64::INFINITY))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::Source;
    use crate::store::{Store, StoreOptions};
    use oxrdfio::RdfFormat;

    #[test]
    fn r2_basic_geo_resumes_longitudes_under_one_row_admission() {
        let store = Store::in_memory(StoreOptions::default());
        let mut data = format!("<urn:s> <{}> 0 .\n", super::super::wgs84::LAT);
        for i in 0..6000 {
            data.push_str(&format!(
                "<urn:s> <{}> 1.{i:04} .\n",
                super::super::wgs84::LONG
            ));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        store
            .enable_geo(GeoConfig {
                wgs84: true,
                ..GeoConfig::default()
            })
            .unwrap();
        let ctx = Ctx::new(store.snapshot());
        let lat = wgs84_lat(&ctx.snap).unwrap();
        let mut refuse = |bytes| {
            if bytes > 4096 {
                return Err(crate::Error::invalid(format!(
                    "unexpected reservation of {bytes} bytes"
                )));
            }
            Ok(())
        };
        let mut iter = WindowIter::open(
            &ctx,
            vec![lat, Id::UNDEF],
            vec![[0.0, -1.0, 2.0, 1.0]],
            GraphFilter::Default,
            false,
            &mut refuse,
        )
        .unwrap();
        assert!(iter.st.fallback);
        let mut hits = Vec::new();
        let mut prev = None;
        for _ in 0..3 {
            hits.clear();
            assert!(!iter.next_hits(&ctx, &mut hits, 1, &mut refuse).unwrap());
            assert_eq!(hits.len(), 1);
            assert_ne!(prev, Some(hits[0].o));
            prev = Some(hits[0].o);
        }

        // A rejected graph still reaches a physical polling boundary inside the scan.
        let long = ctx.snap.lookup_iri(super::super::wgs84::LONG).unwrap().0;
        let subject = ctx.snap.lookup_iri("urn:s").unwrap().0;
        let polls = std::cell::Cell::new(0);
        let result = super::super::wgs84::visit_objects(
            &ctx.snap,
            [subject, long, 0, 0],
            [subject, long, u64::MAX, u64::MAX],
            Id::UNDEF.0,
            || {
                polls.set(polls.get() + 1);
                if polls.get() == 8 {
                    Err(crate::Error::Cancelled)
                } else {
                    Ok(())
                }
            },
            |_| panic!("object in rejected graph"),
        );
        assert!(matches!(result, Err(crate::Error::Cancelled)));
        assert_eq!(polls.get(), 8);
    }

    #[test]
    fn r2_nearest_fallback_reserves_output_before_writing() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let entry = super::super::wgs84::point(0.0, 1.0).unwrap();
        let hit = Hit::new(
            &Row {
                s: 1,
                p: 2,
                o: 3,
                g: 4,
            },
            entry,
        );
        let mut iter = NearestIter {
            snap: ctx.snap.clone(),
            preds: Vec::new(),
            graph: GraphFilter::All,
            dedup: false,
            st: SearchStats::default(),
            mode: NearestMode::Fallback {
                all: vec![(0.0, hit)],
                at: 0,
                size: 32,
            },
        };
        let retained = iter.retained_bytes();
        let mut out = Vec::new();
        let result = iter.next_chunk(&ctx, &mut out, &mut |bytes| {
            assert_eq!(bytes, retained + size_of::<Hit>() as u64);
            Err(crate::Error::Cancelled)
        });
        assert!(matches!(result, Err(crate::Error::Cancelled)));
        assert_eq!(out.capacity(), 0);
        assert_eq!(iter.st.candidates, 0);
        assert_eq!(
            iter.next_chunk(&ctx, &mut out, &mut |_| Ok(())).unwrap(),
            Some(f64::INFINITY)
        );
        assert_eq!(out.len(), 1);
    }
}
