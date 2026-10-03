//! Spatial joins: pairs of geometries from two inputs that pass a spatial test.
//!
//! Each input is a table (a child plan's rows, grouped by their distinct geometry) or an
//! indexed scan, searched in the spatial index. Two tables are joined by packing the
//! larger one's boxes into a tree for the query and probing it with each box of the
//! smaller one. An indexed scan is probed per geometry of the other input (index nested
//! loop) when that input is small next to it; otherwise its rows near the other input
//! are read once and joined like a table. Every candidate pair is then tested exactly
//! with the relation code the `geof:` functions use, the outer geometry prepared once
//! when it has more than a few candidates, so the join answers as the cross product and
//! the filter it replaces do.
//!
//! Geometries in a CRS that cannot be placed on the globe are kept apart: they can only
//! be in a relation with geometries of the same CRS, and are tested against those.

use super::GeomRef;
use super::column::round_out;
use super::config::DistanceModel;
use super::exec::{Counters, config, radius_windows, state, transpose};
use super::geom::Geom;
use super::ops::distance;
use super::ops::relate::{self, Prepared};
use super::search::{self, Hit, SearchStats};
use super::tree::PackedTree;
use crate::error::{Budget, BudgetKind, Error, Result};
use crate::id::Id;
use crate::sparql::ctx::Ctx;
use crate::sparql::geojoin::{JoinSide, JoinTest, SpatialJoinSpec};
use crate::sparql::geopf::ScanShape;
use crate::sparql::plan::{PathEnd, ScanSpec};
use crate::sparql::table::{Table, VarId};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// A geometry of one join input: where it came from and its geometry.
#[derive(Clone)]
pub struct JoinItem {
    /// the input's row (a table row, or a candidate of an index side)
    pub row: u32,
    /// the literal's id
    pub id: Id,
    /// CRS84 envelope (internal lon/lat); NaN for a geometry whose CRS cannot be placed
    /// on the globe, which is only tested against such geometries of the same CRS
    pub bbox84: [f64; 4],
    pub geom: GeomRef,
}

/// A pair of input rows `(left.row, right.row)`.
pub type Pair = (u32, u32);

/// Candidates of one outer geometry above which it is prepared for its tests.
const PREPARE_ABOVE: usize = 8;
/// Exact tests between two cancellation checks.
const CHECK_TESTS: u64 = 256;
/// Candidates between two cancellation checks.
const CHECK_CANDIDATES: u64 = 4096;
/// An indexed scan is probed per geometry of the other input when it has at least this
/// many rows per such geometry; otherwise its rows are read once.
const PROBE_RATIO: usize = 32;
/// Outer geometries handed to the workers at once (their pairs then go to the sink).
const WAVE: usize = 16_384;

/// The exact test of a pair, `test(a, b)` with `a` from the left input, as the `geof:`
/// function computes it: over the operation limit, or on an error, it fails, as the
/// function's error fails a FILTER.
struct Exact<'a> {
    test: &'a JoinTest,
    /// the `relate` pattern of the converse, for a prepared right geometry
    transposed: Option<String>,
    model: DistanceModel,
    op_vertices: u64,
}

/// Whether `a` and `b` have the same internal coordinates: the same CRS, or both
/// longitude/latitude (a transform changes nothing).
fn same_frame(a: &Geom, b: &Geom) -> bool {
    a.crs == b.crs
        || matches!((a.crs.known(), b.crs.known()),
            (Some(x), Some(y)) if x.is_geographic() && y.is_geographic())
}

impl<'a> Exact<'a> {
    fn new(ctx: &Ctx, test: &'a JoinTest) -> Exact<'a> {
        Exact {
            test,
            transposed: match test {
                JoinTest::Relate(p) => Some(transpose(p)),
                _ => None,
            },
            model: config(ctx).distance,
            op_vertices: ctx.geo.op_vertices(),
        }
    }

    fn sized(&self, a: &Geom, b: &Geom) -> bool {
        u64::from(a.vertices) + u64::from(b.vertices) <= self.op_vertices
    }

    fn holds(&self, a: &Geom, b: &Geom) -> bool {
        if !self.sized(a, b) {
            return false;
        }
        match self.test {
            JoinTest::Relation(r) => relate::relation(a, b, *r),
            JoinTest::Relate(p) => relate::relate(a, b, p),
            JoinTest::Within { metres, inclusive } => {
                return distance::distance_m(a, b, self.model)
                    .is_ok_and(|d| d < *metres || *inclusive && d == *metres);
            }
        }
        .unwrap_or(false)
    }

    /// The test with the left geometry prepared.
    fn holds_left(&self, a: &Prepared, b: &Geom) -> bool {
        if !self.sized(a.geom(), b) {
            return false;
        }
        match self.test {
            JoinTest::Relation(r) => a.relation(b, *r).unwrap_or(false),
            JoinTest::Relate(p) => a.relate(b, p).unwrap_or(false),
            JoinTest::Within { .. } => self.holds(a.geom(), b),
        }
    }

    /// The test with the right geometry prepared: the converse question, when the two
    /// share their coordinates (otherwise the left one's CRS decides, as in the
    /// function).
    fn holds_right(&self, a: &Geom, b: &Prepared) -> bool {
        if !self.sized(a, b.geom()) {
            return false;
        }
        if !same_frame(a, b.geom()) {
            return self.holds(a, b.geom());
        }
        match (self.test, &self.transposed) {
            (JoinTest::Relation(r), _) => match r.converse() {
                Some(c) => b.relation(a, c).unwrap_or(false),
                None => self.holds(a, b.geom()),
            },
            (JoinTest::Relate(_), Some(tp)) => b.relate(a, tp).unwrap_or(false),
            _ => self.holds(a, b.geom()),
        }
    }

    /// Whether preparing an outer geometry helps its tests.
    fn prepares(&self) -> bool {
        !matches!(self.test, JoinTest::Within { .. })
    }
}

/// The windows holding every geometry that can pass `test` with one whose box is `b`.
fn windows(test: &JoinTest, b: [f64; 4]) -> Vec<[f64; 4]> {
    match test {
        JoinTest::Within { metres, .. } => radius_windows(b, *metres),
        _ => vec![b],
    }
}

fn placed(b: &[f64; 4]) -> bool {
    !b[0].is_nan()
}

/// The candidate-pair budget of a join (the query's row limit), shared by the workers.
struct Candidates<'a> {
    ctx: &'a Ctx,
    total: AtomicU64,
}

impl Candidates<'_> {
    fn add(&self, n: u64) -> Result<()> {
        let before = self.total.fetch_add(n, Ordering::Relaxed);
        let total = before + n;
        if total > self.ctx.max_rows as u64 {
            // a candidate pair is an intermediate row
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Rows,
                limit: self.ctx.max_rows as u64,
                requested: total,
            }));
        }
        if before / CHECK_CANDIDATES != total / CHECK_CANDIDATES {
            self.ctx.check()?;
        }
        Ok(())
    }
}

/// The pairs `(left.row, right.row)` whose geometries pass `test` (left first), in
/// chunks; `ctx`'s budgets bound the candidates.
///
/// The boxes of the larger input are packed into a tree; each geometry of the smaller one
/// probes it, and is prepared for its tests when it has more than a few candidates.
/// Geometries without a place on the globe (a NaN box) are tested against those of the
/// other input in the same CRS.
pub fn pairs(
    ctx: &Ctx,
    left: &[JoinItem],
    right: &[JoinItem],
    test: &JoinTest,
    st: &mut SearchStats,
    sink: &mut dyn FnMut(&[Pair]) -> Result<()>,
) -> Result<()> {
    let exact = Exact::new(ctx, test);
    let budget = Candidates {
        ctx,
        total: AtomicU64::new(0),
    };
    let boxed = |items: &[JoinItem]| -> Vec<usize> {
        (0..items.len())
            .filter(|&i| placed(&items[i].bbox84))
            .collect()
    };
    let (lb, rb) = (boxed(left), boxed(right));
    let left_outer = lb.len() <= rb.len();
    let (outer, inner, ob, ib) = if left_outer {
        (left, right, lb, rb)
    } else {
        (right, left, rb, lb)
    };
    if let Some(tree) = PackedTree::pack(ib.iter().map(|&i| round_out(inner[i].bbox84))) {
        let size = (ob.len() / (rayon::current_num_threads() * 4)).clamp(1, 256);
        for wave in ob.chunks(WAVE) {
            let parts: Vec<(Vec<Pair>, SearchStats)> = wave
                .par_chunks(size)
                .map(|chunk| {
                    let mut st = SearchStats::default();
                    let mut out = Vec::new();
                    for &o in chunk {
                        let it = &outer[o];
                        let found = tree.search(&windows(test, it.bbox84), &mut st.nodes);
                        if found.is_empty() {
                            continue;
                        }
                        st.candidates += found.len() as u64;
                        budget.add(found.len() as u64)?;
                        let prep = (exact.prepares() && found.len() > PREPARE_ABOVE)
                            .then(|| Prepared::new(it.geom.clone()));
                        for c in found {
                            let other = &inner[ib[c as usize]];
                            st.refined += 1;
                            if st.refined.is_multiple_of(CHECK_TESTS) {
                                ctx.check()?;
                            }
                            let ok = match (&prep, left_outer) {
                                (Some(p), true) => exact.holds_left(p, &other.geom),
                                (Some(p), false) => exact.holds_right(&other.geom, p),
                                (None, true) => exact.holds(&it.geom, &other.geom),
                                (None, false) => exact.holds(&other.geom, &it.geom),
                            };
                            if ok {
                                out.push(if left_outer {
                                    (it.row, other.row)
                                } else {
                                    (other.row, it.row)
                                });
                            }
                        }
                    }
                    st.matched = out.len() as u64;
                    Ok((out, st))
                })
                .collect::<Result<_>>()?;
            for (out, s) in parts {
                add_stats(st, &s);
                if !out.is_empty() {
                    sink(&out)?;
                }
            }
        }
    }
    // geometries off the globe: only those of the same CRS can be in a relation
    let unplaced = |items: &[JoinItem]| -> Vec<usize> {
        (0..items.len())
            .filter(|&i| !placed(&items[i].bbox84))
            .collect()
    };
    let (lu, ru) = (unplaced(left), unplaced(right));
    let mut out = Vec::new();
    for &i in &lu {
        for &j in &ru {
            let (a, b) = (&left[i], &right[j]);
            if a.geom.crs != b.geom.crs {
                continue;
            }
            st.candidates += 1;
            st.refined += 1;
            budget.add(1)?;
            if exact.holds(&a.geom, &b.geom) {
                st.matched += 1;
                out.push((a.row, b.row));
            }
        }
    }
    if !out.is_empty() {
        sink(&out)?;
    }
    Ok(())
}

fn add_stats(st: &mut SearchStats, s: &SearchStats) {
    st.candidates += s.candidates;
    st.refined += s.refined;
    st.matched += s.matched;
    st.nodes += s.nodes;
    st.rechecked += s.rechecked;
    st.fallback |= s.fallback;
}

// ------------------------------------------------------------------- the operator --

/// One input's rows and its distinct geometries (item `i` is `items[i]`, its rows
/// `rows[i]` of `table`).
struct Side {
    table: Table,
    items: Vec<JoinItem>,
    rows: Vec<Vec<u32>>,
    /// literal → its item, for the rows read from the index
    by_id: FxHashMap<Id, u32>,
}

/// The box a join gives a geometry (`None`: it passes no test, being empty).
fn item_box(g: &Geom) -> Option<[f64; 4]> {
    if g.empty {
        return None;
    }
    Some(match (g.bbox84(), g.crs.known()) {
        (Some(b), _) => b,
        // a built-in CRS whose envelope has no place in longitude and latitude: every
        // window meets it
        (None, Some(_)) => [-180.0, -90.0, 180.0, 90.0],
        (None, None) => [f64::NAN; 4],
    })
}

impl Side {
    fn new(table: Table) -> Side {
        Side {
            table,
            items: Vec::new(),
            rows: Vec::new(),
            by_id: FxHashMap::default(),
        }
    }

    /// A child's rows, grouped by their geometry `v` as the functions read it (rows
    /// whose `v` is unbound or not a geometry pass no test and are dropped).
    fn of_table(ctx: &Ctx, table: Table, v: VarId) -> Result<Side> {
        let mut s = Side::new(table);
        let Some(c) = s.table.col_of(v) else {
            return Ok(s);
        };
        // literal → its item (u32::MAX: no geometry)
        let mut seen: FxHashMap<Id, u32> = FxHashMap::default();
        for i in 0..s.table.len() {
            if i % CHECK_CANDIDATES as usize == 0 {
                ctx.check()?;
            }
            let id = s.table.cols[c][i];
            if id.is_undef() {
                continue;
            }
            let item = *seen.entry(id).or_insert_with(|| {
                let g = super::memo::by_id(ctx, id, None).ok();
                match g.as_ref().and_then(|g| item_box(g)) {
                    Some(b) => {
                        s.items.push(JoinItem {
                            row: s.items.len() as u32,
                            id,
                            bbox84: b,
                            geom: g.unwrap(),
                        });
                        s.rows.push(Vec::new());
                        (s.items.len() - 1) as u32
                    }
                    None => u32::MAX,
                }
            });
            if item != u32::MAX {
                s.rows[item as usize].push(i as u32);
            }
        }
        Ok(s)
    }

    /// Add the index's rows of a literal not seen before (all its rows come together:
    /// a search hands out every row of a literal whose box meets a window).
    fn add_hits(&mut self, ctx: &Ctx, ix: &IndexSide, hits: &[Hit]) -> Option<u32> {
        let o = hits.first()?.o;
        if let Some(&i) = self.by_id.get(&o) {
            return Some(i);
        }
        let g = super::memo::noted(ctx, hits[0].entry.geom(&ctx.snap)).ok()?;
        let b = item_box(&g)?;
        let item = self.items.len() as u32;
        self.items.push(JoinItem {
            row: item,
            id: o,
            bbox84: b,
            geom: g,
        });
        let mut rows = Vec::with_capacity(hits.len());
        for h in hits {
            rows.push(self.table.len() as u32);
            let row: Vec<Id> = ix
                .cols
                .iter()
                .map(|c| match c {
                    0 => h.s,
                    1 => h.o,
                    _ => h.g,
                })
                .collect();
            self.table.push_row(&row);
        }
        self.rows.push(rows);
        self.by_id.insert(o, item);
        Some(item)
    }

    /// Union of the boxes of the placed items, expanded for the test (`None`: no item).
    fn reach(&self, test: &JoinTest) -> Option<Vec<[f64; 4]>> {
        let mut u: Option<[f64; 4]> = None;
        for it in self.items.iter().filter(|it| placed(&it.bbox84)) {
            let b = it.bbox84;
            u = Some(match u {
                None => b,
                Some(x) => [
                    x[0].min(b[0]),
                    x[1].min(b[1]),
                    x[2].max(b[2]),
                    x[3].max(b[3]),
                ],
            });
        }
        u.map(|b| windows(test, b))
    }
}

/// An input read from the spatial index.
struct IndexSide<'a> {
    scan: &'a ScanSpec,
    pred: Id,
    /// the scan's subject when it is a constant
    subj: Option<Id>,
    graph: crate::sparql::plan::GraphFilter,
    /// the side's variables and the hit component of each (0 subject, 1 object, 2
    /// graph)
    vars: Vec<VarId>,
    cols: Vec<u8>,
}

impl<'a> IndexSide<'a> {
    fn of(s: &'a JoinSide) -> Result<IndexSide<'a>> {
        let JoinSide::Index {
            scan,
            pred,
            geom_var,
            subj_var,
            graph_var,
        } = s
        else {
            unreachable!("an index side")
        };
        let shape = ScanShape::of(scan)
            .ok_or_else(|| Error::invalid("spatial join of an unexpected pattern"))?;
        let mut vars = Vec::new();
        let mut cols = Vec::new();
        for (v, c) in [(*subj_var, 0u8), (Some(*geom_var), 1), (*graph_var, 2)] {
            if let Some(v) = v
                && !vars.contains(&v)
            {
                vars.push(v);
                cols.push(c);
            }
        }
        Ok(IndexSide {
            scan,
            pred: *pred,
            subj: match shape.subj {
                Some(PathEnd::Const(s)) => Some(s),
                _ => None,
            },
            graph: shape.graph_filter(scan),
            vars,
            cols,
        })
    }

    /// The scan's rows whose box meets one of `windows`, grouped by literal, through
    /// `f`.
    fn search(
        &self,
        ctx: &Ctx,
        windows: &[[f64; 4]],
        st: &mut SearchStats,
        f: &mut dyn FnMut(&[Hit]) -> Result<()>,
    ) -> Result<()> {
        let mut hits: Vec<Hit> = Vec::new();
        search::window_with(
            ctx,
            &[self.pred],
            windows,
            &self.graph,
            self.scan.dedup,
            st,
            &mut |found| {
                hits.extend(
                    found
                        .iter()
                        .filter(|h| self.subj.is_none_or(|s| s == h.s))
                        .cloned(),
                );
                Ok(())
            },
        )?;
        hits.sort_unstable_by_key(|h| (h.o, h.s, h.g));
        for group in hits.chunk_by(|a, b| a.o == b.o) {
            f(group)?;
        }
        Ok(())
    }

    /// Every row of the scan near `windows`, as a side.
    fn read(&self, ctx: &Ctx, windows: &[[f64; 4]], st: &mut SearchStats) -> Result<Side> {
        let mut side = Side::new(Table::new(self.vars.clone()));
        self.search(ctx, windows, st, &mut |group| {
            side.add_hits(ctx, self, group);
            ctx.check_output(side.table.len(), side.table.width())
        })?;
        Ok(side)
    }

    /// Rows of the scan (an upper bound on what the index holds for it).
    fn rows(&self, ctx: &Ctx) -> u64 {
        ctx.snap
            .count(self.scan.perm, &self.scan.prefix)
            .unwrap_or(u64::MAX)
    }
}

/// The index nested loop: each item of `outer` searches the index side, and its
/// candidates are tested; returns the index side's rows read and the passing pairs
/// `(outer item, inner item)`.
fn probe(
    ctx: &Ctx,
    exact: &Exact<'_>,
    outer: &Side,
    ix: &IndexSide<'_>,
    outer_is_left: bool,
    st: &mut SearchStats,
) -> Result<(Side, Vec<Pair>)> {
    let budget = Candidates {
        ctx,
        total: AtomicU64::new(0),
    };
    let test = exact.test;
    let placed_items: Vec<usize> = (0..outer.items.len())
        .filter(|&i| placed(&outer.items[i].bbox84))
        .collect();
    let size = (placed_items.len() / (rayon::current_num_threads() * 4)).clamp(1, 64);
    // per outer item, the index rows of the literals that pass
    type Found = Vec<(u32, Vec<Hit>)>;
    let mut side = Side::new(Table::new(ix.vars.clone()));
    let mut out = Vec::new();
    for wave in placed_items.chunks(WAVE) {
        let parts: Vec<(Found, SearchStats)> = wave
            .par_chunks(size)
            .map(|chunk| {
                let mut st = SearchStats::default();
                let mut found: Found = Vec::new();
                for &o in chunk {
                    let it = &outer.items[o];
                    let prep = std::cell::OnceCell::new();
                    let mut cands = 0usize;
                    let mut groups: Vec<Vec<Hit>> = Vec::new();
                    ix.search(ctx, &windows(test, it.bbox84), &mut st, &mut |g| {
                        cands += 1;
                        groups.push(g.to_vec());
                        Ok(())
                    })?;
                    budget.add(cands as u64)?;
                    let prepared = exact.prepares() && cands > PREPARE_ABOVE;
                    let mut pass = Vec::new();
                    for g in groups {
                        st.refined += 1;
                        if st.refined.is_multiple_of(CHECK_TESTS) {
                            ctx.check()?;
                        }
                        let Ok(geom) = super::memo::noted(ctx, g[0].entry.geom(&ctx.snap)) else {
                            continue;
                        };
                        let ok = match (prepared, outer_is_left) {
                            (true, true) => exact.holds_left(
                                prep.get_or_init(|| Prepared::new(it.geom.clone())),
                                &geom,
                            ),
                            (true, false) => exact.holds_right(
                                &geom,
                                prep.get_or_init(|| Prepared::new(it.geom.clone())),
                            ),
                            (false, true) => exact.holds(&it.geom, &geom),
                            (false, false) => exact.holds(&geom, &it.geom),
                        };
                        if ok {
                            st.matched += 1;
                            pass.extend(g);
                        }
                    }
                    if !pass.is_empty() {
                        found.push((o as u32, pass));
                    }
                }
                Ok((found, st))
            })
            .collect::<Result<_>>()?;
        for (found, s) in parts {
            add_stats(st, &s);
            for (o, hits) in found {
                for g in hits.chunk_by(|a, b| a.o == b.o) {
                    if let Some(i) = side.add_hits(ctx, ix, g) {
                        out.push(if outer_is_left { (o, i) } else { (i, o) });
                    }
                }
            }
            ctx.check_output(side.table.len(), side.table.width())?;
        }
    }
    st.candidates = st.candidates.max(budget.total.load(Ordering::Relaxed));
    Ok((side, out))
}

/// Execute a [`SpatialJoinSpec`] over the tables of the node's children (`inputs`, in
/// child order).
pub fn spatial_join(
    ctx: &Ctx,
    spec: &SpatialJoinSpec,
    inputs: Vec<Table>,
    vars: &[VarId],
) -> Result<(Table, Counters)> {
    let state = state(ctx);
    let exact = Exact::new(ctx, &spec.test);
    let mut st = SearchStats::default();
    let mut probes = false;
    let mut inputs: Vec<Option<Table>> = inputs.into_iter().map(Some).collect();
    let mut table_side = |s: &JoinSide| -> Result<Option<Side>> {
        match s {
            JoinSide::Plan { child, geom_var } => {
                let t = inputs
                    .get_mut(*child)
                    .and_then(Option::take)
                    .ok_or_else(|| Error::invalid("spatial join without its input"))?;
                Side::of_table(ctx, t, *geom_var).map(Some)
            }
            JoinSide::Index { .. } => Ok(None),
        }
    };
    let (mut ls, mut rs) = (table_side(&spec.left)?, table_side(&spec.right)?);
    // every row, wherever it is written
    let world = [[-1e30, -1e30, 1e30, 1e30]];
    // an index side is probed only when the index can serve it; two index sides: the
    // smaller is read first
    if let (None, None) = (&ls, &rs) {
        let (l, r) = (IndexSide::of(&spec.left)?, IndexSide::of(&spec.right)?);
        if l.rows(ctx) <= r.rows(ctx) {
            ls = Some(l.read(ctx, &world, &mut st)?);
        } else {
            rs = Some(r.read(ctx, &world, &mut st)?);
        }
    }
    let (ls, rs, pairs_found) = match (ls, rs) {
        (Some(l), Some(r)) => {
            let mut found = Vec::new();
            pairs(ctx, &l.items, &r.items, &spec.test, &mut st, &mut |p| {
                found.extend_from_slice(p);
                Ok(())
            })?;
            (l, r, found)
        }
        (Some(l), None) => {
            let ix = IndexSide::of(&spec.right)?;
            let (r, found) = with_index(ctx, &exact, &l, &ix, true, &mut st, &mut probes)?;
            (l, r, found)
        }
        (None, Some(r)) => {
            let ix = IndexSide::of(&spec.left)?;
            let (l, found) = with_index(ctx, &exact, &r, &ix, false, &mut st, &mut probes)?;
            (l, r, found)
        }
        (None, None) => unreachable!("one index side was read"),
    };
    let mut t = assemble(ctx, &ls, &rs, &pairs_found, vars)?;
    let npairs = pairs_found.len();
    if !spec.filter.is_empty() {
        use crate::sparql::expr::{Row, ebv};
        let map = t.var_map(ctx.nvars());
        let keep: Vec<bool> = (0..t.len())
            .map(|i| {
                let row = Row {
                    table: &t,
                    i,
                    map: &map,
                    dec: None,
                };
                spec.filter
                    .iter()
                    .all(|e| ebv(e, &row, ctx).unwrap_or(false))
            })
            .collect();
        t.filter_rows(&keep);
    }
    let mut c = Counters::new();
    c.insert("candidates".into(), st.candidates.into());
    c.insert("refined".into(), st.refined.into());
    c.insert("matched".into(), (t.len() as u64).into());
    c.insert("pairs".into(), (npairs as u64).into());
    c.insert("treeNodesVisited".into(), st.nodes.into());
    c.insert("rechecked".into(), st.rechecked.into());
    c.insert("indexProbes".into(), probes.into());
    c.insert("index".into(), state.to_string().into());
    c.insert("fallback".into(), st.fallback.into());
    Ok((t, c))
}

/// A table joined with an index side: probed per geometry of the table when the table is
/// small next to the scan (and the index can serve it), else the scan's rows near the
/// table read once and joined as a table. Returns the index side's rows and the pairs.
fn with_index(
    ctx: &Ctx,
    exact: &Exact<'_>,
    table: &Side,
    ix: &IndexSide<'_>,
    table_is_left: bool,
    st: &mut SearchStats,
    probes: &mut bool,
) -> Result<(Side, Vec<Pair>)> {
    let usable = search::indexed(&ctx.snap, &[ix.pred]).is_some();
    if usable && (table.items.len() as u64).saturating_mul(PROBE_RATIO as u64) <= ix.rows(ctx) {
        *probes = true;
        return probe(ctx, exact, table, ix, table_is_left, st);
    }
    let Some(reach) = table.reach(exact.test) else {
        return Ok((Side::new(Table::new(ix.vars.clone())), Vec::new()));
    };
    let other = ix.read(ctx, &reach, st)?;
    let (l, r) = if table_is_left {
        (&table.items, &other.items)
    } else {
        (&other.items, &table.items)
    };
    let mut found = Vec::new();
    pairs(ctx, l, r, exact.test, st, &mut |p| {
        found.extend_from_slice(p);
        Ok(())
    })?;
    Ok((other, found))
}

/// The joined rows: each passing pair's rows of both sides, in `vars` order.
fn assemble(ctx: &Ctx, l: &Side, r: &Side, found: &[Pair], vars: &[VarId]) -> Result<Table> {
    let total: usize = found
        .iter()
        .map(|&(a, b)| l.rows[a as usize].len() * r.rows[b as usize].len())
        .sum();
    ctx.check_output(total, vars.len())?;
    let cols: Vec<(bool, Option<usize>)> = vars
        .iter()
        .map(|v| match l.table.col_of(*v) {
            Some(c) => (true, Some(c)),
            None => (false, r.table.col_of(*v)),
        })
        .collect();
    let mut t = Table::new(vars.to_vec());
    for col in &mut t.cols {
        col.reserve(total);
    }
    for (n, &(a, b)) in found.iter().enumerate() {
        if n % CHECK_CANDIDATES as usize == 0 {
            ctx.check()?;
        }
        for &lr in &l.rows[a as usize] {
            for &rr in &r.rows[b as usize] {
                for (x, &(left, c)) in t.cols.iter_mut().zip(&cols) {
                    x.push(match (left, c) {
                        (true, Some(c)) => l.table.cols[c][lr as usize],
                        (false, Some(c)) => r.table.cols[c][rr as usize],
                        _ => Id::UNDEF,
                    });
                }
                t.len += 1;
            }
        }
    }
    Ok(t)
}
