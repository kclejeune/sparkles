//! Execution of the spatial plan operators (`SpatialScan`, `SpatialPf`).
//!
//! Both find candidates first (a window or nearest-first search of the index, or, when
//! the index is not ready for the snapshot, a scan of the serialization predicates),
//! then test each distinct geometry literal exactly, once, with the code the `geof:`
//! functions use. The index and the scan therefore return the same rows: the index
//! only saves reading the rows whose envelope misses every window.
//!
//! `SpatialPf` then maps the matching geometries to their features through the feature
//! links (`POS` lookups of `[link, geometry]`; under `GRAPH ?g` the link must be in the
//! geometry's graph), keeps one solution per feature, and with a `limit` keeps the
//! features nearest to the query (box and cardinal functions: to the centre of the
//! query's envelope), ties by subject id.

use super::GeomRef;
use super::column::ColumnEntry;
use super::config::{DistanceModel, GeoConfig, IndexState};
use super::crs::{CRS84, CrsRef};
use super::geom::{Geom, GeomError};
use super::ops::distance;
use super::ops::relate::{self, Prepared};
use super::search::{self, SearchStats};
use super::vocab::{Relation, SpatialPfKind};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::Perm;
use crate::sparql::ctx::Ctx;
use crate::sparql::geopf::{ScanShape, SpatialPfSpec, SpatialScanSpec, SpatialTest};
use crate::sparql::plan::PathEnd;
use crate::sparql::table::{Table, VarId};
use crate::sparql::value::Value;
use crate::store::Chunk;
use oxrdf::Term;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// Per-operator explain counters (`candidates`, `refined`, `matched`, …).
pub type Counters = serde_json::Map<String, serde_json::Value>;

/// Exact tests between two cancellation checks.
const CHECK_EVERY: usize = 256;

// ----------------------------------------------------------------- shared helpers --

/// The spatial index configuration the snapshot sees (the defaults without an index).
pub(crate) fn config(ctx: &Ctx) -> Arc<GeoConfig> {
    ctx.snap
        .geo
        .as_ref()
        .map_or_else(|| Arc::new(GeoConfig::default()), |v| v.config.clone())
}

/// The index state the snapshot sees.
pub(crate) fn state(ctx: &Ctx) -> IndexState {
    ctx.snap.geo.as_ref().map_or(IndexState::Off, |v| v.state())
}

/// The message of a malformed geometry literal of datatype `dt`.
pub(crate) fn malformed(dt: &str, e: &GeomError) -> String {
    let local = dt.rsplit(['#', '/']).next().unwrap_or(dt);
    match e.offset {
        Some(o) => format!("geo: malformed {local} at offset {o}: {}", e.msg),
        None => format!("geo: malformed {local}: {}", e.msg),
    }
}

/// The IRI of a graph id (the default graph by its Jena name).
pub(crate) fn graph_iri(ctx: &Ctx, g: Id) -> Option<String> {
    if g == Id::DEFAULT_GRAPH {
        return Some(crate::text::DEFAULT_GRAPH_IRI.to_string());
    }
    match ctx.term(g)? {
        Term::NamedNode(n) => Some(n.into_string()),
        _ => None,
    }
}

/// The CRS84 geometry of an envelope: a point or a line when it is degenerate (as
/// `geof:envelope`), else a rectangle.
pub(crate) fn envelope_geom(b: [f64; 4]) -> Geom {
    use georust::{Coord, LineString, Point, Rect};
    let g: georust::Geometry<f64> = if b[0] == b[2] && b[1] == b[3] {
        Point::new(b[0], b[1]).into()
    } else if b[0] == b[2] || b[1] == b[3] {
        LineString::from(vec![(b[0], b[1]), (b[2], b[3])]).into()
    } else {
        Rect::new(Coord { x: b[0], y: b[1] }, Coord { x: b[2], y: b[3] })
            .to_polygon()
            .into()
    };
    Geom::from_geometry(CrsRef::Known(CRS84), g)
}

/// A short description of a geometry for plans: `POINT(2 2)`, `POLYGON(5 pts)`.
pub(crate) fn summary(g: &Geom) -> String {
    let name = g.declared.wkt_name();
    if g.empty {
        return format!("{name} EMPTY");
    }
    match &g.g {
        georust::Geometry::Point(p) => {
            let mut s = format!("{name}(");
            super::write::number(&mut s, p.x());
            s.push(' ');
            super::write::number(&mut s, p.y());
            s.push(')');
            s
        }
        _ => format!("{name}({} pts)", g.vertices),
    }
}

/// Whether the box `b` meets (closed) one of `windows`.
pub(crate) fn windows_meet(b: [f64; 4], windows: &[[f64; 4]]) -> bool {
    windows
        .iter()
        .any(|w| b[0] <= w[2] && b[2] >= w[0] && b[1] <= w[3] && b[3] >= w[1])
}

/// CRS84 boxes holding everything within `r_m` metres of the box `b`, with a margin
/// for rounding (the windows only bound the candidates; the exact test decides).
pub(crate) fn radius_windows(b: [f64; 4], r_m: f64) -> Vec<[f64; 4]> {
    distance::radius_windows(b, r_m * (1.0 + 1e-6) + 0.01)
}

/// The DE-9IM pattern of the converse relation (rows and columns swapped).
pub(crate) fn transpose(p: &str) -> String {
    let c: Vec<char> = p.chars().collect();
    if c.len() != 9 {
        return p.to_string();
    }
    (0..9).map(|i| c[(i % 3) * 3 + i / 3]).collect()
}

/// The windows of a `spatial:` call: around the query (expanded by the radius), the
/// box itself, or the half-world strips of the cardinal functions (Jena's definition:
/// north from the top of the query's envelope to the pole, all longitudes; east up to
/// 180° of longitude from its right edge, wrapping at ±180°).
pub(crate) fn pf_windows(func: SpatialPfKind, q: &Geom, radius_m: Option<f64>) -> Vec<[f64; 4]> {
    use SpatialPfKind::*;
    let Some(b) = q.bbox84() else {
        return Vec::new();
    };
    match func {
        Nearby | WithinCircle | NearbyGeom | WithinCircleGeom => {
            radius_windows(b, radius_m.unwrap_or(0.0))
        }
        WithinBox | WithinBoxGeom | IntersectBox | IntersectBoxGeom => vec![b],
        North | NorthGeom => vec![[-180.0, b[3], 180.0, 90.0]],
        South | SouthGeom => vec![[-180.0, -90.0, 180.0, b[1]]],
        East | EastGeom => {
            let (x1, x2) = (b[2], b[2] + 180.0);
            if x2 <= 180.0 {
                vec![[x1, -90.0, x2, 90.0]]
            } else {
                vec![[x1, -90.0, 180.0, 90.0], [-180.0, -90.0, x2 - 360.0, 90.0]]
            }
        }
        West | WestGeom => {
            let (x1, x2) = (b[0] - 180.0, b[0]);
            if x1 >= -180.0 {
                vec![[x1, -90.0, x2, 90.0]]
            } else {
                vec![[-180.0, -90.0, x2, 90.0], [x1 + 360.0, -90.0, 180.0, 90.0]]
            }
        }
    }
}

/// The exact tests of a `spatial:` call (the cardinal functions have none: meeting
/// their windows is the test).
fn pf_tests(spec: &SpatialPfSpec) -> Vec<SpatialTest> {
    use SpatialPfKind::*;
    let q = spec.query.clone();
    match spec.func {
        Nearby | WithinCircle | NearbyGeom | WithinCircleGeom => vec![SpatialTest::Distance {
            q,
            metres: spec.radius_m.unwrap_or(0.0),
            inclusive: false,
        }],
        WithinBox | WithinBoxGeom => vec![SpatialTest::Relation(Relation::SfWithin, q)],
        IntersectBox | IntersectBoxGeom => vec![SpatialTest::Relation(Relation::SfIntersects, q)],
        _ => Vec::new(),
    }
}

// -------------------------------------------------------------------- refinement --

/// Where a candidate's geometry comes from.
#[derive(Clone)]
enum Src {
    /// the index's geometry column
    Entry(Arc<ColumnEntry>),
    /// the literal itself, parsed as the index would parse it
    Literal,
}

/// What a geometry must pass, and what to rank it by.
struct Check<'a> {
    /// its envelope meets one of these
    windows: &'a [[f64; 4]],
    /// all of these hold
    tests: &'a [SpatialTest],
    /// rank by the distance from this geometry
    rank: Option<&'a GeomRef>,
    model: DistanceModel,
    cfg: &'a GeoConfig,
}

/// [`Check`] for one worker: the constants prepared once (a prepared geometry is not
/// shared between threads).
struct Tester<'a> {
    check: &'a Check<'a>,
    prepared: Vec<Option<Prepared>>,
    /// transposed `relate` patterns, for the prepared constant
    transposed: Vec<Option<String>>,
    /// the query's limit on the vertices of one operation
    op_vertices: u64,
}

impl<'a> Tester<'a> {
    fn new(ctx: &Ctx, check: &'a Check<'a>) -> Tester<'a> {
        Tester {
            check,
            op_vertices: ctx.geo.op_vertices(),
            prepared: check.tests.iter().map(|_| None).collect(),
            transposed: check
                .tests
                .iter()
                .map(|t| match t {
                    SpatialTest::Relate(p, _) => Some(transpose(p)),
                    _ => None,
                })
                .collect(),
        }
    }

    /// `None`: `w` fails; else its ranking distance (0 without ranking).
    fn check(&mut self, w: &Geom) -> Option<f64> {
        let b = w.bbox84()?;
        if !windows_meet(b, self.check.windows) {
            return None;
        }
        let model = self.check.model;
        let mut near: Option<(&GeomRef, f64)> = None;
        for (i, t) in self.check.tests.iter().enumerate() {
            match t {
                SpatialTest::Distance {
                    q,
                    metres,
                    inclusive,
                } => {
                    let d = distance::distance_m(q, w, model).ok()?;
                    if !(d < *metres || *inclusive && d == *metres) {
                        return None;
                    }
                    near = Some((q, d));
                }
                t => {
                    if !holds(
                        t,
                        &mut self.prepared[i],
                        self.transposed[i].as_deref(),
                        w,
                        self.op_vertices,
                    ) {
                        return None;
                    }
                }
            }
        }
        match self.check.rank {
            None => Some(0.0),
            Some(r) => Some(match near {
                Some((q, d)) if Arc::ptr_eq(q, r) => d,
                _ => distance::distance_m(r, w, model).unwrap_or(f64::INFINITY),
            }),
        }
    }
}

/// Whether `a` and `b` have the same internal coordinates: the same CRS, or both
/// geographic (longitude, latitude), where a transform changes nothing.
fn same_frame(a: &Geom, b: &Geom) -> bool {
    a.crs == b.crs
        || matches!((a.crs.known(), b.crs.known()),
            (Some(x), Some(y)) if x.is_geographic() && y.is_geographic())
}

/// `t(w, q)` for a relation or `relate` test, as the `geof:` functions compute it. In
/// the same frame the constant is prepared once and asked the converse question
/// (`r(w, q)` is `converse(r)(q, w)`); otherwise `q` is transformed into `w`'s CRS as
/// the functions transform their second argument. Errors (no common CRS, a sum of
/// vertices over the operation limit) fail the test, as they fail a FILTER.
fn holds(
    t: &SpatialTest,
    prep: &mut Option<Prepared>,
    transposed: Option<&str>,
    w: &Geom,
    op_vertices: u64,
) -> bool {
    let q = match t {
        SpatialTest::Relation(_, q) | SpatialTest::Relate(_, q) => q,
        SpatialTest::Distance { .. } => unreachable!("distance tests are run by the caller"),
    };
    if u64::from(w.vertices) + u64::from(q.vertices) > op_vertices {
        return false;
    }
    match t {
        SpatialTest::Relation(r, q) => match r.converse() {
            Some(c) if same_frame(w, q) => prep
                .get_or_insert_with(|| Prepared::new(q.clone()))
                .relation(w, c),
            _ => relate::relation(w, q, *r),
        },
        SpatialTest::Relate(p, q) => match transposed {
            Some(tp) if same_frame(w, q) => prep
                .get_or_insert_with(|| Prepared::new(q.clone()))
                .relate(w, tp),
            _ => relate::relate(w, q, p),
        },
        SpatialTest::Distance { .. } => unreachable!(),
    }
    .unwrap_or(false)
}

/// The geometry of a candidate literal: the index's column, or the literal parsed as
/// functions parse it (through the query's memo) when the index would hold it.
fn load(ctx: &Ctx, id: Id, src: &Src, cfg: &GeoConfig) -> Option<GeomRef> {
    use super::memo::{MemoKey, max_vertices, parse_value};
    match src {
        Src::Entry(e) => e.geom(&ctx.snap).ok(),
        Src::Literal => {
            let v = ctx.value(id)?;
            let Value::Other { lex, .. } = &v else {
                return None;
            };
            if lex.len() > cfg.max_geometry_bytes {
                return None;
            }
            let max = max_vertices(ctx);
            ctx.geo
                .get_or_parse(MemoKey::Id(id), || parse_value(&v, max))
        }
    }
}

/// Test each distinct candidate literal once (in parallel for many); `None` where it
/// fails, else its ranking distance.
fn refine(
    ctx: &Ctx,
    check: &Check<'_>,
    items: &[(Id, Src)],
    st: &mut SearchStats,
) -> Result<Vec<Option<f64>>> {
    let run = |part: &[(Id, Src)]| -> Result<Vec<Option<f64>>> {
        let mut t = Tester::new(ctx, check);
        let mut out = Vec::with_capacity(part.len());
        for (i, (id, src)) in part.iter().enumerate() {
            if i % CHECK_EVERY == CHECK_EVERY - 1 {
                ctx.check()?;
            }
            out.push(load(ctx, *id, src, check.cfg).and_then(|g| t.check(&g)));
        }
        Ok(out)
    };
    st.refined += items.len() as u64;
    if items.len() <= 2 * CHECK_EVERY {
        return run(items);
    }
    let size = (items.len() / (rayon::current_num_threads() * 4)).max(CHECK_EVERY);
    let parts: Vec<Vec<Option<f64>>> = items.par_chunks(size).map(run).collect::<Result<_>>()?;
    Ok(parts.into_iter().flatten().collect())
}

/// The rows whose literal passed, and each passing literal's ranking distance.
type Refined = (Vec<[Id; 3]>, FxHashMap<Id, f64>);

/// Candidate rows `(s, o, g)` of serialization predicates and their literals.
#[derive(Default)]
struct Candidates {
    rows: Vec<[Id; 3]>,
    geoms: FxHashMap<Id, Src>,
}

impl Candidates {
    fn push(&mut self, ctx: &Ctx, s: Id, o: Id, g: Id, src: impl FnOnce() -> Src) -> Result<()> {
        self.rows.push([s, o, g]);
        self.geoms.entry(o).or_insert_with(src);
        if self.rows.len().is_multiple_of(4096) {
            ctx.check()?;
            ctx.check_rows(self.rows.len())?;
        }
        Ok(())
    }

    /// Refine the literals; returns the passing literals with their ranking distance.
    fn refine(self, ctx: &Ctx, check: &Check<'_>, st: &mut SearchStats) -> Result<Refined> {
        st.candidates = st.candidates.max(self.rows.len() as u64);
        let items: Vec<(Id, Src)> = self.geoms.into_iter().collect();
        let res = refine(ctx, check, &items, st)?;
        let pass: FxHashMap<Id, f64> = items
            .iter()
            .zip(res)
            .filter_map(|((id, _), r)| r.map(|d| (*id, d)))
            .collect();
        let mut rows = self.rows;
        rows.retain(|r| pass.contains_key(&r[1]));
        st.matched = rows.len() as u64;
        Ok((rows, pass))
    }
}

/// Whether quads of graph `g` are in the index's scope (cached per graph).
struct Scope<'a> {
    cfg: &'a GeoConfig,
    all: bool,
    memo: FxHashMap<u64, bool>,
}

impl<'a> Scope<'a> {
    fn new(cfg: &'a GeoConfig) -> Scope<'a> {
        let all = matches!(cfg.graphs.include, crate::text::PredicateSet::All)
            && cfg.graphs.exclude.is_empty();
        Scope {
            cfg,
            all,
            memo: FxHashMap::default(),
        }
    }

    fn contains(&mut self, ctx: &Ctx, g: u64) -> bool {
        if self.all {
            return true;
        }
        let cfg = self.cfg;
        *self
            .memo
            .entry(g)
            .or_insert_with(|| graph_iri(ctx, Id(g)).is_some_and(|i| cfg.graph_in_scope(&i)))
    }
}

/// Visit the keys of a prefix scan.
fn scan_keys(
    ctx: &Ctx,
    perm: Perm,
    prefix: &[u64],
    mut f: impl FnMut([u64; 4]) -> Result<()>,
) -> Result<()> {
    ctx.snap.scan(perm, prefix, |c| {
        match c {
            Chunk::Block(b, s, e) => {
                for i in s..e {
                    f(b.key(i))?;
                }
            }
            Chunk::Row(k) => f(k)?,
        }
        Ok(true)
    })
}

fn counters(st: &SearchStats, state: IndexState) -> Counters {
    let mut c = Counters::new();
    c.insert("candidates".into(), st.candidates.into());
    c.insert("refined".into(), st.refined.into());
    c.insert("matched".into(), st.matched.into());
    c.insert("treeNodesVisited".into(), st.nodes.into());
    c.insert("index".into(), state.to_string().into());
    c.insert("fallback".into(), st.fallback.into());
    c
}

// ------------------------------------------------------------------ SpatialScan --

/// A scan of an indexed predicate restricted by spatial filters.
pub fn spatial_scan(
    ctx: &Ctx,
    spec: &SpatialScanSpec,
    vars: &[VarId],
) -> Result<(Table, Counters)> {
    let shape = ScanShape::of(&spec.scan)
        .ok_or_else(|| Error::invalid("spatial scan of an unexpected pattern"))?;
    let cfg = config(ctx);
    let state = state(ctx);
    let graph = shape.graph_filter(&spec.scan);
    let subj = match shape.subj {
        Some(PathEnd::Const(s)) => Some(s),
        _ => None,
    };
    let mut st = SearchStats::default();
    let mut cands = Candidates::default();
    if state.ready() {
        search::window(
            ctx,
            &[spec.pred],
            &spec.windows,
            &graph,
            &mut st,
            &mut |hits| {
                for h in hits {
                    if subj.is_none_or(|s| s == h.s) {
                        cands.push(ctx, h.s, h.o, h.g, || Src::Entry(h.entry.clone()))?;
                    }
                }
                Ok(())
            },
        )?;
    } else {
        // the index became unusable since planning: read the predicate
        st.fallback = true;
        let s = &spec.scan;
        scan_keys(ctx, s.perm, &s.prefix, |k| {
            let q = s.perm.to_quad(&k);
            if graph.accepts(q[3].0) && subj.is_none_or(|x| x == q[0]) {
                cands.push(ctx, q[0], q[2], q[3], || Src::Literal)?;
            }
            Ok(())
        })?;
    }
    let check = Check {
        windows: &spec.windows,
        tests: &spec.tests,
        rank: None,
        model: cfg.distance,
        cfg: &cfg,
    };
    let (mut rows, _) = cands.refine(ctx, &check, &mut st)?;
    rows.sort_unstable();
    if spec.scan.dedup {
        // the same triple in several graphs of a merged default graph
        rows.dedup_by_key(|r| (r[0], r[1]));
    }
    st.matched = rows.len() as u64;
    ctx.check_output(rows.len(), vars.len())?;
    let (sv, gv) = (shape.subj_var(), shape.graph_var());
    let cols: Vec<usize> = vars
        .iter()
        .map(|v| {
            if Some(*v) == sv {
                0
            } else if *v == shape.obj {
                1
            } else if Some(*v) == gv {
                2
            } else {
                usize::MAX
            }
        })
        .collect();
    let mut t = Table::new(vars.to_vec());
    let mut row = vec![Id::UNDEF; vars.len()];
    for r in &rows {
        for (x, &c) in row.iter_mut().zip(&cols) {
            *x = r.get(c).copied().unwrap_or(Id::UNDEF);
        }
        t.push_row(&row);
    }
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
        st.matched = t.len() as u64;
    }
    t.sorted = shape.sorted();
    Ok((t, counters(&st, state)))
}

// -------------------------------------------------------------------- SpatialPf --

/// A `spatial:` property function.
pub fn spatial_pf(ctx: &Ctx, spec: &SpatialPfSpec, vars: &[VarId]) -> Result<(Table, Counters)> {
    use SpatialPfKind::*;
    let cfg = config(ctx);
    let state = state(ctx);
    let ids = |iris: &[String]| -> Vec<Id> {
        iris.iter().filter_map(|i| ctx.snap.lookup_iri(i)).collect()
    };
    let (preds, links) = (ids(&cfg.predicates), ids(&cfg.feature_links));
    let windows = pf_windows(spec.func, &spec.query, spec.radius_m);
    let tests = pf_tests(spec);
    let nearby = matches!(
        spec.func,
        Nearby | WithinCircle | NearbyGeom | WithinCircleGeom
    );
    // ranked by the distance to the query, or to the centre of its envelope
    let rank: Option<GeomRef> = match (spec.limit, spec.query.bbox84()) {
        (None, _) | (_, None) => None,
        (Some(_), _) if nearby => match &tests[0] {
            SpatialTest::Distance { q, .. } => Some(q.clone()),
            _ => None,
        },
        (Some(_), Some(b)) => Some(Arc::new(envelope_geom([
            (b[0] + b[2]) / 2.0,
            (b[1] + b[3]) / 2.0,
            (b[0] + b[2]) / 2.0,
            (b[1] + b[3]) / 2.0,
        ]))),
    };
    let check = Check {
        windows: &windows,
        tests: &tests,
        rank: rank.as_ref(),
        model: cfg.distance,
        cfg: &cfg,
    };
    let mut st = SearchStats::default();
    // feature (and graph, under GRAPH ?g) → its ranking distance
    let mut feats: FxHashMap<(Id, Id), f64> = FxHashMap::default();
    let ready = state.ready() && !windows.is_empty();
    match (&spec.subject, spec.limit) {
        (PathEnd::Var(_), Some(k)) if ready && nearby => {
            knn(ctx, spec, &preds, &links, &check, k, &mut st, &mut feats)?;
        }
        _ if windows.is_empty() => {}
        (subject, _) => {
            let mut cands = Candidates::default();
            let mut scope = Scope::new(&cfg);
            if let PathEnd::Const(f) = subject {
                // a given feature: its links, then its geometries' literals
                for l in &links {
                    scan_keys(ctx, Perm::Spo, &[f.0, l.0], |k| {
                        let (geom, g1) = (k[2], k[3]);
                        if !spec.graph.accepts(g1) {
                            return Ok(());
                        }
                        for p in &preds {
                            scan_keys(ctx, Perm::Spo, &[geom, p.0], |k2| {
                                let g = k2[3];
                                if spec.graph.accepts(g) && scope.contains(ctx, g) {
                                    cands.push(ctx, Id(geom), Id(k2[2]), Id(g), || Src::Literal)?;
                                }
                                Ok(())
                            })?;
                        }
                        Ok(())
                    })?;
                }
            } else if ready {
                search::window(ctx, &preds, &windows, &spec.graph, &mut st, &mut |hits| {
                    for h in hits {
                        cands.push(ctx, h.s, h.o, h.g, || Src::Entry(h.entry.clone()))?;
                    }
                    Ok(())
                })?;
            } else {
                st.fallback = true;
                for p in &preds {
                    scan_keys(ctx, Perm::Pso, &[p.0], |k| {
                        let g = k[3];
                        if spec.graph.accepts(g) && scope.contains(ctx, g) {
                            cands.push(ctx, Id(k[1]), Id(k[2]), Id(g), || Src::Literal)?;
                        }
                        Ok(())
                    })?;
                }
            }
            let (rows, pass) = cands.refine(ctx, &check, &mut st)?;
            // each geometry (in its graph) once, at its nearest literal
            let mut geoms: FxHashMap<(Id, Id), f64> = FxHashMap::default();
            for r in rows {
                let d = pass[&r[1]];
                let e = geoms.entry((r[0], r[2])).or_insert(d);
                *e = e.min(d);
            }
            let mut geoms: Vec<_> = geoms.into_iter().collect();
            geoms.sort_unstable_by_key(|((s, g), _)| (*s, *g));
            for ((s, g), d) in geoms {
                features(ctx, spec, &links, s, g, d, &mut feats)?;
            }
        }
    }
    let mut out: Vec<((Id, Id), f64)> = feats.into_iter().collect();
    if let Some(k) = spec.limit {
        out.sort_unstable_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        out.truncate(k);
    }
    out.sort_unstable_by_key(|x| x.0);
    let fv = match spec.subject {
        PathEnd::Var(v) => Some(v),
        PathEnd::Const(_) => None,
    };
    if let (Some(f), Some(g)) = (fv, spec.graph_var)
        && f == g
    {
        out.retain(|((s, g), _)| s == g);
    }
    ctx.check_output(out.len(), vars.len())?;
    let mut t = Table::new(vars.to_vec());
    let mut row = vec![Id::UNDEF; vars.len()];
    for ((f, g), _) in &out {
        for (x, v) in row.iter_mut().zip(vars) {
            *x = if Some(*v) == fv {
                *f
            } else if Some(*v) == spec.graph_var {
                *g
            } else {
                Id::UNDEF
            };
        }
        t.push_row(&row);
    }
    t.sorted = vars.to_vec();
    Ok((t, counters(&st, state)))
}

/// Map the matching geometry `s` (in graph `g`, at ranking distance `d`) to its
/// features through the feature links.
fn features(
    ctx: &Ctx,
    spec: &SpatialPfSpec,
    links: &[Id],
    s: Id,
    g: Id,
    d: f64,
    feats: &mut FxHashMap<(Id, Id), f64>,
) -> Result<()> {
    for l in links {
        scan_keys(ctx, Perm::Pos, &[l.0, s.0], |k| {
            let (f, g1) = (Id(k[2]), k[3]);
            let same = match spec.graph_var {
                // the link in the geometry's own graph
                Some(_) => g1 == g.0,
                None => spec.graph.accepts(g1),
            };
            if !same || matches!(spec.subject, PathEnd::Const(c) if c != f) {
                return Ok(());
            }
            let key = (
                f,
                if spec.graph_var.is_some() {
                    g
                } else {
                    Id::UNDEF
                },
            );
            let e = feats.entry(key).or_insert(d);
            *e = e.min(d);
            Ok(())
        })?;
    }
    Ok(())
}

/// `nearby` with a limit: a nearest-first search that stops once `k` features are
/// nearer than anything left (or the radius is reached).
#[allow(clippy::too_many_arguments)]
fn knn(
    ctx: &Ctx,
    spec: &SpatialPfSpec,
    preds: &[Id],
    links: &[Id],
    check: &Check<'_>,
    k: usize,
    st: &mut SearchStats,
    feats: &mut FxHashMap<(Id, Id), f64>,
) -> Result<()> {
    let radius = spec.radius_m.unwrap_or(0.0);
    let mut tester = Tester::new(ctx, check);
    // literal → its ranking distance (None: fails), each tested once
    let mut seen: FxHashMap<Id, Option<f64>> = FxHashMap::default();
    let mut refined = 0u64;
    let mut matched = 0u64;
    let mut candidates = 0u64;
    let mut nth = Vec::new();
    search::nearest(
        ctx,
        preds,
        &spec.query,
        &spec.graph,
        st,
        &mut |hits, bound| {
            if bound >= radius {
                return Ok(false);
            }
            if feats.len() >= k {
                // the k-th nearest feature so far; nothing left can come closer than `bound`
                nth.clear();
                nth.extend(feats.values().copied());
                let (_, kth, _) = nth.select_nth_unstable_by(k - 1, f64::total_cmp);
                if *kth < bound {
                    return Ok(false);
                }
            }
            for h in hits {
                candidates += 1;
                let d = match seen.get(&h.o) {
                    Some(d) => *d,
                    None => {
                        refined += 1;
                        if refined.is_multiple_of(CHECK_EVERY as u64) {
                            ctx.check()?;
                        }
                        let d = h.entry.geom(&ctx.snap).ok().and_then(|g| tester.check(&g));
                        seen.insert(h.o, d);
                        d
                    }
                };
                if let Some(d) = d {
                    matched += 1;
                    features(ctx, spec, links, h.s, h.g, d, feats)?;
                }
            }
            Ok(true)
        },
    )?;
    st.candidates = st.candidates.max(candidates);
    st.refined = refined;
    st.matched = matched;
    Ok(())
}
