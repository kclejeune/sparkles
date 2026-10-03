//! GeoSPARQL in the planner: the Jena `spatial:` property functions as search leaves,
//! and spatial FILTERs on an indexed predicate's object pushed into a `SpatialScan`.
//!
//! ```sparql
//! ?f spatial:nearby (51.5 -0.12 5 uom:kilometre 10)
//! ?x geo:asWKT ?w FILTER(geof:sfWithin(?w, "POLYGON((…))"^^geo:wktLiteral))
//! ```
//!
//! A pushed FILTER keeps its meaning exactly: the index only proposes candidates (rows
//! whose envelope meets a window around the constant), and every candidate is tested
//! with the same relation code the `geof:` functions use. Conjuncts whose exact test is
//! not one of [`SpatialTest`] (distance comparisons, relations with the constant first
//! and no converse) contribute their window and stay in the ordinary filter above the
//! scan. When the index cannot be used (not enabled, building, failed, a transaction's
//! own view), the filter stays a plain filter and the plan carries a warning naming the
//! reason; `spatial:` calls then enumerate the serialization predicates instead of
//! searching the index, with the same answer.
//!
//! Compiled without the `geo` feature too: the property functions are then recognized
//! and refused with [`crate::geo::not_built`], and nothing is pushed down.

use super::ctx::Ctx;
pub use super::ctx::PlanWarning;
use super::expr::Expr;
use super::plan::{ActiveGraph, GraphFilter, Node, PathEnd, Planner, ScanSpec};
use super::table::VarId;
use crate::error::Result;
use crate::geo::{GeomRef, Relation, SpatialPfKind};
use crate::id::Id;
use spargebra::term::{TermPattern, TriplePattern};
use std::sync::Arc;

/// A scan of the indexed predicate `pred` whose object `geom_var` passes spatial tests.
#[derive(Clone)]
pub struct SpatialScanSpec {
    /// the scan it replaces (prefix, columns, graph)
    pub scan: ScanSpec,
    pub pred: Id,
    pub geom_var: VarId,
    pub subj_var: Option<VarId>,
    /// CRS84 boxes that contain every match
    pub windows: Vec<[f64; 4]>,
    /// the pushed conjuncts as exact tests (all must hold)
    pub tests: Vec<SpatialTest>,
    /// conjuncts evaluated on the operator's rows as an ordinary filter
    pub filter: Vec<Expr>,
    /// result-cache key of the constant geometries, tests, windows and radius
    pub key: u64,
}

/// An exact test of a row's geometry `?w` against a constant geometry.
#[derive(Clone)]
pub enum SpatialTest {
    /// `geof:<relation>(?w, q)`
    Relation(Relation, GeomRef),
    /// `geof:relate(?w, q, pattern)`
    Relate(Arc<str>, GeomRef),
    /// distance to `q` below `metres` (`<=` when inclusive)
    Distance {
        q: GeomRef,
        metres: f64,
        inclusive: bool,
    },
}

/// A `spatial:` property function planned as a leaf.
#[derive(Clone)]
pub struct SpatialPfSpec {
    pub func: SpatialPfKind,
    /// the query geometry (latitude/longitude arguments as an EPSG:4326 point)
    pub query: GeomRef,
    pub radius_m: Option<f64>,
    /// keep the `limit` matches nearest to the query
    pub limit: Option<usize>,
    /// the feature slot: a variable, or a constant restricting the answer
    pub subject: PathEnd,
    /// graph scope of the active graph
    pub graph: GraphFilter,
    /// `GRAPH ?g { … }` around the call: bound from each match's graph
    pub graph_var: Option<VarId>,
    /// one solution per feature across graphs (merged default graph)
    pub dedup: bool,
    /// result-cache key of the call's arguments
    pub key: u64,
    /// Arguments bound by the rest of the group (the node's child): the call's argument
    /// slots, each a constant or a variable. `query`, `radius_m` and `limit` are then
    /// decoded once per distinct binding of the variables.
    pub deferred: Option<Arc<[PathEnd]>>,
}

impl SpatialPfSpec {
    /// Whether the call reads the rest of its group (child 0): it has variable
    /// arguments.
    pub fn needs_input(&self) -> bool {
        self.deferred.is_some()
    }
}

/// A `spatial:` triple taken out of a basic graph pattern, with its list elements.
#[derive(Clone, Debug)]
pub struct SpatialCall {
    pub func: SpatialPfKind,
    pub subjects: Vec<TermPattern>,
    pub args: Vec<TermPattern>,
}

/// Take the `spatial:` property function calls out of `patterns`; returns the calls and
/// the remaining triple patterns.
pub fn take_spatial_calls(
    patterns: &[TriplePattern],
) -> Result<(Vec<SpatialCall>, Vec<TriplePattern>)> {
    let (calls, rest) = super::textpf::take_calls_where(
        patterns,
        |iri| SpatialPfKind::from_iri(iri).is_some(),
        |iri| SpatialPfKind::from_iri(iri).map_or_else(|| iri.to_string(), |k| k.name()),
    )?;
    let calls = calls
        .into_iter()
        .map(|(iri, subjects, args)| SpatialCall {
            func: SpatialPfKind::from_iri(iri.as_str()).expect("a spatial: call"),
            subjects,
            args,
        })
        .collect();
    Ok((calls, rest))
}

/// What a scan reads, by quad component: the predicate (a constant), the object (a
/// variable: the geometry), the subject and the graph.
#[cfg(feature = "geo")]
#[derive(Clone, Debug)]
pub(crate) struct ScanShape {
    pub pred: Id,
    pub obj: VarId,
    pub subj: Option<PathEnd>,
    /// the graph: a variable column, or a constant of the prefix
    pub graph: Option<PathEnd>,
}

#[cfg(feature = "geo")]
impl ScanShape {
    /// The shape of `spec`, if it scans one predicate with a variable object and no
    /// repeated variables.
    pub(crate) fn of(spec: &ScanSpec) -> Option<ScanShape> {
        use crate::index::{G, O, P, S};
        if !spec.eqs.is_empty() {
            return None;
        }
        let order = spec.perm.order();
        let mut comp: [Option<PathEnd>; 4] = [None, None, None, None];
        for (i, &v) in spec.prefix.iter().enumerate() {
            comp[order[i]] = Some(PathEnd::Const(Id(v)));
        }
        for &(kc, v) in &spec.cols {
            comp[order[kc]] = Some(PathEnd::Var(v));
        }
        let Some(PathEnd::Const(pred)) = comp[P] else {
            return None;
        };
        let Some(PathEnd::Var(obj)) = comp[O] else {
            return None;
        };
        Some(ScanShape {
            pred,
            obj,
            subj: comp[S].clone(),
            graph: comp[G].clone(),
        })
    }

    pub(crate) fn subj_var(&self) -> Option<VarId> {
        match self.subj {
            Some(PathEnd::Var(v)) => Some(v),
            _ => None,
        }
    }

    pub(crate) fn graph_var(&self) -> Option<VarId> {
        match self.graph {
            Some(PathEnd::Var(v)) => Some(v),
            _ => None,
        }
    }

    /// The graphs whose rows the scan reads.
    pub(crate) fn graph_filter(&self, spec: &ScanSpec) -> GraphFilter {
        match self.graph {
            Some(PathEnd::Const(g)) => GraphFilter::One(g.0),
            _ => spec.graph.clone(),
        }
    }

    /// The output order of a spatial scan: subject, object, graph.
    pub(crate) fn sorted(&self) -> Vec<VarId> {
        let mut v = Vec::new();
        for x in [self.subj_var(), Some(self.obj), self.graph_var()]
            .into_iter()
            .flatten()
        {
            if !v.contains(&x) {
                v.push(x);
            }
        }
        v
    }
}

/// A `spatial:` call as a search leaf.
pub(super) fn spatial_leaf(p: &Planner<'_>, c: SpatialCall, g: &ActiveGraph) -> Result<Node> {
    #[cfg(not(feature = "geo"))]
    {
        let _ = (p, g, &c);
        Err(crate::geo::not_built())
    }
    #[cfg(feature = "geo")]
    on::spatial_leaf(p, c, g)
}

/// Move spatial conjuncts over the object of an indexed predicate's scan into a
/// `SpatialScan`; returns the plan and the conjuncts left for an ordinary filter.
pub fn push_spatial(n: Node, exprs: Vec<Expr>, ctx: &Ctx) -> (Node, Vec<Expr>) {
    #[cfg(not(feature = "geo"))]
    {
        let _ = ctx;
        (n, exprs)
    }
    #[cfg(feature = "geo")]
    on::push_spatial(n, exprs, ctx)
}

/// The value of an expression without variables, evaluated once at plan time (`None`:
/// not constant, not deterministic, or an error).
pub fn fold_const(e: &Expr, ctx: &Ctx) -> Option<super::value::Value> {
    use super::expr::{Row, eval};
    use super::table::Table;
    match e {
        Expr::Lit(_, v) => return Some(v.clone()),
        Expr::Const(id) => return ctx.value(*id),
        _ => {}
    }
    if !e.var_set().is_empty() || e.has_exists() || !super::cache::deterministic(e) {
        return None;
    }
    let t = Table::unit();
    let map = t.var_map(ctx.nvars());
    let row = Row {
        table: &t,
        i: 0,
        map: &map,
        dec: None,
    };
    eval(e, &row, ctx).ok()?.value(ctx).ok()
}

/// Helpers the spatial join and nearest-neighbour planning share.
#[cfg(feature = "geo")]
pub(crate) use on::{
    constant_geom, decode_values, distance_text, geof_call, number, scope_covers, unit_iri, warn,
};

/// A `spatial:` call with variable arguments, joined to the rest of its group (`left`),
/// which must bind them.
pub(super) fn attach_spatial(p: &Planner<'_>, left: Node, search: Node) -> Result<Node> {
    use super::plan::Kind;
    let Kind::SpatialPf(spec) = &search.kind else {
        unreachable!("only spatial: calls with variable arguments are attached here");
    };
    let slots = spec.deferred.as_deref().unwrap_or_default();
    for s in slots {
        if let PathEnd::Var(v) = s
            && !left.vars.contains(v)
        {
            return Err(crate::error::Error::invalid(format!(
                "{}: the argument ?{} is not bound by the rest of the group",
                spec.func.name(),
                p.ctx.var_name(*v)
            )));
        }
    }
    let mut vars = left.vars.clone();
    for v in &search.vars {
        if !vars.contains(v) {
            vars.push(*v);
        }
    }
    let mut certain = left.certain.clone();
    certain.extend(search.vars.iter().copied());
    let est = (left.est * search.est).max(1.0);
    Ok(Node {
        dist: vars.iter().map(|&v| (v, est)).collect(),
        cost: left.cost + left.est.max(1.0) * search.cost,
        vars,
        certain,
        sorted: Vec::new(),
        est,
        desc: search.desc,
        kind: search.kind,
        children: vec![left],
    })
}

#[cfg(feature = "geo")]
mod on {
    use super::super::expr::{CmpOp, Func};
    use super::super::plan::{Kind, PT, short};
    use super::super::value::{Num, Value};
    use super::*;
    use crate::error::Error;
    use crate::geo::crs::{CRS84, CrsRef};
    use crate::geo::exec::{
        config, envelope_geom, graph_iri, malformed, pf_windows, radius_windows, summary, transpose,
    };
    use crate::geo::ops::relate;
    use crate::geo::{Fnv, GeoConfig, Geom, IndexState, units, vocab};
    use crate::index::Perm;

    /// Metres in one degree of arc on the equator (WGS 84): angle units of a radius.
    pub(crate) const METRES_PER_DEGREE: f64 = 111_319.490_793_273_57;
    /// Exact tests per window row of a cheap test (a point in a prepared polygon).
    const REFINE_POINT: f64 = 0.5;

    pub(crate) fn warn(ctx: &Ctx, code: &'static str, message: String) {
        ctx.warn(PlanWarning { code, message });
    }

    /// A geometry literal's value parsed as functions parse it (within `maxVertices`).
    fn geometry(v: &Value, cfg: &GeoConfig) -> Option<std::result::Result<Geom, String>> {
        let Value::Other { lex, dt } = v else {
            return None;
        };
        if !vocab::is_geometry_datatype(dt) {
            return None;
        }
        Some(crate::geo::parse_limited(lex, dt, cfg.max_vertices).map_err(|e| malformed(dt, &e)))
    }

    pub(crate) fn number(v: &Value) -> Option<f64> {
        let x: f64 = Num::of(v).ok()?.to_double().into();
        x.is_finite().then_some(x)
    }

    /// The unit IRI of an argument: an IRI, or an `xsd:anyURI` or string literal.
    pub(crate) fn unit_iri(v: &Value) -> Option<Arc<str>> {
        match v {
            Value::Iri(i) => Some(i.clone()),
            Value::Str(s) => Some(s.clone()),
            Value::Other { lex, dt } if &**dt == oxrdf::vocab::xsd::ANY_URI.as_str() => {
                Some(lex.clone())
            }
            _ => None,
        }
    }

    /// Metres of `r` in the length or angle unit `iri` (an angle is measured along the
    /// equator).
    fn metres(r: f64, iri: &str) -> std::result::Result<f64, String> {
        let u = units::unit(iri).ok_or_else(|| format!("unknown unit <{iri}>"))?;
        match u.kind {
            units::UnitKind::Length => Ok(r * u.factor),
            units::UnitKind::Angle => Ok(r * u.factor.to_degrees() * METRES_PER_DEGREE),
            units::UnitKind::Area => Err(format!("<{iri}> is not a unit of length")),
        }
    }

    // ----------------------------------------------------------- property functions --

    /// The decoded arguments of a `spatial:` call.
    pub(crate) struct PfArgs {
        pub(crate) query: Geom,
        pub(crate) radius_m: Option<f64>,
        pub(crate) limit: Option<usize>,
        /// the query has no extent (an empty geometry): nothing matches
        pub(crate) empty: bool,
    }

    /// The argument list a function takes, and its fewest and most arguments.
    fn shape(func: SpatialPfKind) -> (&'static str, usize, usize) {
        use SpatialPfKind::*;
        match func {
            Nearby | WithinCircle => ("(lat lon radius [unit [limit]])", 3, 5),
            NearbyGeom | WithinCircleGeom => ("(geometry radius [unit [limit]])", 2, 4),
            WithinBox | IntersectBox => ("(latMin lonMin latMax lonMax [limit])", 4, 5),
            WithinBoxGeom | IntersectBoxGeom | NorthGeom | SouthGeom | EastGeom | WestGeom => {
                ("(geometry [limit])", 1, 2)
            }
            North | South | East | West => ("(lat lon [limit])", 2, 3),
        }
    }

    /// The arguments of a call whose arguments are all constants.
    fn decode(p: &Planner<'_>, c: &SpatialCall) -> Result<PfArgs> {
        let name = c.func.name();
        let (shape, _, _) = shape(c.func);
        let mut vals = Vec::with_capacity(c.args.len());
        for a in &c.args {
            match p.term_pattern(a) {
                PT::V(_) => unreachable!("planned with deferred arguments"),
                PT::C(id) => vals.push(
                    p.ctx
                        .value(id)
                        .ok_or_else(|| Error::invalid(format!("{name}: {shape}")))?,
                ),
            }
        }
        decode_values(c.func, &vals, &config(p.ctx))
    }

    /// The arguments of a call from their values: at plan time for constants, and per
    /// binding for variable arguments.
    pub(crate) fn decode_values(
        func: SpatialPfKind,
        vals: &[Value],
        cfg: &GeoConfig,
    ) -> Result<PfArgs> {
        use SpatialPfKind::*;
        let name = func.name();
        let bad = |m: String| Error::invalid(format!("{name}: {m}"));
        let (shape, fixed, max) = shape(func);
        if vals.len() < fixed || vals.len() > max {
            return Err(bad(format!("expected {shape}")));
        }
        let num = |i: usize, what: &str| {
            number(&vals[i]).ok_or_else(|| bad(format!("the {what} is not a number")))
        };
        let lat = |i: usize, what: &str| {
            let x = num(i, what)?;
            if (-90.0..=90.0).contains(&x) {
                Ok(x)
            } else {
                Err(bad(format!("{what} {x} is outside -90..90")))
            }
        };
        let lon = |i: usize, what: &str| {
            let x = num(i, what)?;
            if (-180.0..=180.0).contains(&x) {
                Ok(x)
            } else {
                Err(bad(format!("{what} {x} is outside -180..180")))
            }
        };
        let geom = |i: usize| -> Result<Geom> {
            match geometry(&vals[i], cfg) {
                Some(Ok(g)) if g.bbox84().is_some() || g.empty => Ok(g),
                Some(Ok(g)) => Err(bad(format!(
                    "the geometry's CRS {} is not supported",
                    match &g.crs {
                        CrsRef::Unknown(iri) => format!("<{iri}>"),
                        CrsRef::Known(_) => "here".into(),
                    }
                ))),
                Some(Err(m)) => Err(bad(m)),
                None => Err(bad("expected a geometry literal".into())),
            }
        };
        let point = |lat: f64, lon: f64| {
            Geom::from_geometry(CrsRef::Known(CRS84), georust::Point::new(lon, lat).into())
        };
        let limit = |i: usize| -> Result<Option<usize>> {
            let Some(v) = vals.get(i) else {
                return Ok(None);
            };
            let Value::Integer(n) = v else {
                return Err(bad("the limit is not an integer".into()));
            };
            let n = i64::from(*n);
            Ok((n > 0).then(|| usize::try_from(n).unwrap_or(usize::MAX)))
        };
        let radius = |i: usize, unit: Option<usize>| -> Result<f64> {
            let r = num(i, "radius")?;
            if r < 0.0 {
                return Err(bad(format!("the radius {r} is negative")));
            }
            match unit.and_then(|u| vals.get(u)) {
                None => Ok(r * 1000.0), // uom:kilometre
                Some(u) => {
                    let iri = unit_iri(u).ok_or_else(|| bad("the unit is not an IRI".into()))?;
                    metres(r, &iri).map_err(bad)
                }
            }
        };
        let mut a = match func {
            Nearby | WithinCircle => PfArgs {
                query: point(lat(0, "latitude")?, lon(1, "longitude")?),
                radius_m: Some(radius(2, Some(3))?),
                limit: limit(4)?,
                empty: false,
            },
            NearbyGeom | WithinCircleGeom => PfArgs {
                query: geom(0)?,
                radius_m: Some(radius(1, Some(2))?),
                limit: limit(3)?,
                empty: false,
            },
            WithinBox | IntersectBox => {
                let (s, w) = (lat(0, "latMin")?, lon(1, "lonMin")?);
                let (n, e) = (lat(2, "latMax")?, lon(3, "lonMax")?);
                if s > n || w > e {
                    return Err(bad(format!(
                        "the box ({s} {w} {n} {e}) has its minimum above its maximum"
                    )));
                }
                PfArgs {
                    query: envelope_geom([w, s, e, n]),
                    radius_m: None,
                    limit: limit(4)?,
                    empty: false,
                }
            }
            WithinBoxGeom | IntersectBoxGeom => {
                let g = geom(0)?;
                PfArgs {
                    query: g.bbox84().map_or(g, envelope_geom),
                    radius_m: None,
                    limit: limit(1)?,
                    empty: false,
                }
            }
            North | South | East | West => PfArgs {
                query: point(lat(0, "latitude")?, lon(1, "longitude")?),
                radius_m: None,
                limit: limit(2)?,
                empty: false,
            },
            NorthGeom | SouthGeom | EastGeom | WestGeom => PfArgs {
                query: geom(0)?,
                radius_m: None,
                limit: limit(1)?,
                empty: false,
            },
        };
        a.empty = a.query.bbox84().is_none();
        Ok(a)
    }

    pub(super) fn spatial_leaf(p: &Planner<'_>, c: SpatialCall, g: &ActiveGraph) -> Result<Node> {
        let ctx = p.ctx;
        let name = c.func.name();
        let [subject] = c.subjects.as_slice() else {
            return Err(Error::invalid(format!(
                "{name}: the subject is one feature (a variable or an IRI)"
            )));
        };
        if matches!(subject, TermPattern::Literal(_) | TermPattern::Triple(_)) {
            return Err(Error::invalid(format!(
                "{name}: the subject is a variable or an IRI"
            )));
        }
        let subject = match p.term_pattern(subject) {
            PT::V(v) => PathEnd::Var(v),
            PT::C(id) => PathEnd::Const(id),
        };
        // variable arguments are bound by the rest of the group, and decoded per binding
        let slots: Vec<PathEnd> = c
            .args
            .iter()
            .map(|a| match p.term_pattern(a) {
                PT::V(v) => PathEnd::Var(v),
                PT::C(id) => PathEnd::Const(id),
            })
            .collect();
        let deferred = slots.iter().any(|s| matches!(s, PathEnd::Var(_)));
        let a = if deferred {
            let (shape, fixed, max) = shape(c.func);
            if !(fixed..=max).contains(&slots.len()) {
                return Err(Error::invalid(format!("{name}: expected {shape}")));
            }
            None
        } else {
            Some(decode(p, &c)?)
        };
        let mut vars = Vec::new();
        if let PathEnd::Var(v) = subject {
            vars.push(v);
        }
        let Some((graph, graph_var)) = p.graph_filter(g) else {
            return Ok(Node::empty(vars));
        };
        if let Some(gv) = graph_var
            && !vars.contains(&gv)
        {
            vars.push(gv);
        }
        if a.as_ref().is_some_and(|a| a.empty)
            || matches!(subject, PathEnd::Const(id) if id.tag() == crate::id::Tag::Local)
        {
            return Ok(Node::empty(vars));
        }
        let dedup = graph_var.is_none() && graph.multi();
        let mut h = Fnv::new();
        h.field(c.func.local().as_bytes());
        for t in &c.args {
            h.field(t.to_string().as_bytes());
        }
        let Some(a) = a else {
            // one search per binding of the arguments, with a guess at its size
            let desc = format!(
                "{}{}{} ({}) [per binding of the arguments]",
                vars.iter()
                    .map(|v| format!("?{} ", ctx.var_name(*v)))
                    .collect::<String>(),
                if vars.is_empty() { "" } else { "← " },
                name,
                slots
                    .iter()
                    .map(|s| match s {
                        PathEnd::Var(v) => format!("?{}", ctx.var_name(*v)),
                        PathEnd::Const(id) => p.pt_str(&PT::C(*id)),
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            let spec = SpatialPfSpec {
                func: c.func,
                query: Arc::new(envelope_geom([0.0, 0.0, 0.0, 0.0])),
                radius_m: None,
                limit: None,
                subject,
                graph,
                graph_var,
                dedup,
                key: h.finish(),
                deferred: Some(slots.into()),
            };
            let sorted = vars.clone();
            let mut n = Node::leaf(Kind::SpatialPf(Box::new(spec)), vars, 8.0, desc);
            n.cost = 64.0;
            n.sorted = sorted;
            return Ok(n);
        };
        let cfg = config(ctx);
        let windows = pf_windows(c.func, &a.query, a.radius_m);
        // estimate: window rows (from the index, or a share of the predicates' rows by
        // area), times features per geometry, half of them passing the exact test
        let preds: Vec<Id> = cfg
            .predicates
            .iter()
            .filter_map(|p| ctx.snap.lookup_iri(p))
            .collect();
        let rows: f64 = preds
            .iter()
            .map(|p| ctx.snap.estimate(Perm::Pso, &[p.0]) as f64)
            .sum();
        let ready = ctx.snap.geo.as_ref().filter(|v| v.state().ready());
        let window_rows = match ready {
            Some(v) => {
                let slots: Vec<u16> = preds.iter().filter_map(|p| v.predicate_slot(*p)).collect();
                v.estimate(&slots, &windows)
            }
            None => rows * area_share(&windows),
        };
        let (mut subjects, mut objects) = (0.0, 0.0);
        for l in &cfg.feature_links {
            if let Some(ps) = ctx
                .snap
                .lookup_iri(l)
                .and_then(|l| ctx.snap.predicate_stat(l.0))
            {
                subjects += ps.distinct_subjects as f64;
                objects += ps.distinct_objects as f64;
            }
        }
        let fanout = if objects > 0.0 {
            subjects / objects
        } else {
            1.0
        };
        let mut est = (window_rows * fanout * 0.5).max(1.0);
        if let Some(l) = a.limit {
            est = est.min(l as f64);
        }
        if let PathEnd::Const(_) = subject {
            est = est.min(1.0);
        }
        let refine = refine_cost(&a.query);
        let cost = match (ready, &subject) {
            // a constant feature: its links and their geometries only
            (_, PathEnd::Const(_)) => 8.0 * (1.0 + refine),
            (Some(v), _) => {
                window_rows * (1.0 + refine) + 4.0 * v.levels() as f64 + 2.0 * window_rows
            }
            (None, _) => rows * (1.0 + refine) + 2.0 * window_rows,
        };
        let links: Vec<&str> = cfg.feature_links.iter().map(|l| local(l)).collect();
        let desc = format!(
            "{}{}{} {}{}{} [features via {}]",
            vars.iter()
                .map(|v| format!("?{} ", ctx.var_name(*v)))
                .collect::<String>(),
            if vars.is_empty() { "" } else { "← " },
            name,
            summary(&a.query),
            a.radius_m
                .map(|r| format!(" r={}", distance_text(r)))
                .unwrap_or_default(),
            a.limit.map(|l| format!(" limit {l}")).unwrap_or_default(),
            links.join("|"),
        );
        let spec = SpatialPfSpec {
            func: c.func,
            query: Arc::new(a.query),
            radius_m: a.radius_m,
            limit: a.limit,
            subject,
            graph,
            graph_var,
            dedup,
            key: h.finish(),
            deferred: None,
        };
        let sorted = vars.clone();
        let mut n = Node::leaf(Kind::SpatialPf(Box::new(spec)), vars, est, desc);
        n.cost = cost;
        n.sorted = sorted;
        Ok(n)
    }

    /// The local name of an IRI (after the last `#` or `/`).
    fn local(iri: &str) -> &str {
        iri.rsplit(['#', '/']).next().unwrap_or(iri)
    }

    /// `1500 m`, `5 km`.
    pub(crate) fn distance_text(m: f64) -> String {
        if m >= 1000.0 {
            format!("{} km", m / 1000.0)
        } else {
            format!("{m} m")
        }
    }

    /// The share of the world's area (in degrees) the windows cover.
    fn area_share(windows: &[[f64; 4]]) -> f64 {
        let a: f64 = windows
            .iter()
            .map(|w| (w[2] - w[0]).max(0.0) * (w[3] - w[1]).max(0.0))
            .sum();
        (a / (360.0 * 180.0)).clamp(0.0001, 1.0)
    }

    /// Cost of one exact test against `q`, relative to reading a row: a point in a
    /// prepared polygon is cheap; polygon–polygon tests grow with the vertices.
    fn refine_cost(q: &Geom) -> f64 {
        REFINE_POINT + 0.25 * f64::from(q.vertices.max(1)).log2()
    }

    // ---------------------------------------------------------------- pushdown ------

    /// A FILTER conjunct over the geometry variable that the index can serve.
    struct Atom {
        /// the constant geometry the window is built around
        q: GeomRef,
        /// expand the window by this many metres
        radius_m: Option<f64>,
        /// the exact test (`None`: the conjunct stays in the filter above the scan)
        test: Option<SpatialTest>,
        /// for the plan's description
        text: String,
        /// share of window rows expected to pass
        sel: f64,
    }

    /// `geof:<name>(…)` calls: the local name and the arguments.
    pub(crate) fn geof_call(e: &Expr) -> Option<(&str, &[Expr])> {
        match e {
            Expr::Call(Func::Ext(iri), args) => Some((iri.strip_prefix(vocab::GEOF)?, args)),
            _ => None,
        }
    }

    /// Whether a (conjunction of) filter expressions has a `geof:` test of `w` at its
    /// top level (a quick check before the conjuncts are taken apart).
    fn spatial_conjunct(e: &Expr, w: VarId) -> bool {
        match e {
            Expr::And(a, b) => spatial_conjunct(a, w) || spatial_conjunct(b, w),
            Expr::Cmp(a, b, _) => {
                (geof_call(a).is_some() || geof_call(b).is_some()) && mentions(e, w)
            }
            e => geof_call(e).is_some() && mentions(e, w),
        }
    }

    fn mentions(e: &Expr, v: VarId) -> bool {
        e.var_set().contains(&v)
    }

    /// The constant geometry of an argument (a literal or a constant expression).
    pub(crate) fn constant_geom(
        e: &Expr,
        ctx: &Ctx,
        cfg: &GeoConfig,
    ) -> std::result::Result<Geom, String> {
        let Some(v) = fold_const(e, ctx) else {
            return Err("the other geometry is not a constant".into());
        };
        match geometry(&v, cfg) {
            Some(Ok(g)) if g.empty || g.bbox84().is_some() => Ok(g),
            Some(Ok(_)) => Err("the constant geometry's CRS is not supported by the index".into()),
            Some(Err(m)) => Err(m),
            None => Err("the other argument is not a geometry literal".into()),
        }
    }

    /// The conjunct `e` as an index atom over `w`: `None` when it is not a spatial test
    /// of `w`, `Err` (the reason) when it is one the index cannot serve.
    fn atom(
        e: &Expr,
        w: VarId,
        ctx: &Ctx,
        cfg: &GeoConfig,
    ) -> Option<std::result::Result<Atom, String>> {
        let is_w = |x: &Expr| matches!(x, Expr::Var(v) if *v == w);
        // relations and relate
        if let Some((name, args)) = geof_call(e) {
            let rel = Relation::from_local(name);
            if rel.is_none() && name != "relate" {
                return None;
            }
            if !args.iter().any(|a| mentions(a, w)) {
                return None;
            }
            let wanted = if name == "relate" { 3 } else { 2 };
            if args.len() != wanted {
                return None;
            }
            let first = if is_w(&args[0]) {
                true
            } else if is_w(&args[1]) {
                false
            } else {
                return Some(Err(format!(
                    "geof:{name} does not compare ?{} itself",
                    ctx.var_name(w)
                )));
            };
            let other = &args[usize::from(first)];
            if mentions(other, w) {
                return None;
            }
            let q = match constant_geom(other, ctx, cfg) {
                Ok(q) => Arc::new(q),
                Err(m) => return Some(Err(format!("geof:{name}: {m}"))),
            };
            let text = format!("{name} {}", summary(&q));
            if let Some(r) = rel {
                if !r.index_usable() {
                    return Some(Err(format!(
                        "geof:{name} holds away from the geometry, where the index has nothing to offer"
                    )));
                }
                // the test is r(?w, q); with the constant first, its converse
                let test = if first {
                    Some(SpatialTest::Relation(r, q.clone()))
                } else {
                    r.converse().map(|c| SpatialTest::Relation(c, q.clone()))
                };
                let sel = if matches!(
                    r,
                    Relation::SfWithin | Relation::EhInside | Relation::EhCoveredBy
                ) && first
                    || matches!(
                        r,
                        Relation::SfContains | Relation::EhContains | Relation::EhCovers
                    ) && !first
                {
                    area_ratio(&q)
                } else {
                    0.5
                };
                return Some(Ok(Atom {
                    q,
                    radius_m: None,
                    test,
                    text,
                    sel,
                }));
            }
            // relate(…, pattern): the pattern must need an intersection
            let Some(Value::Str(pattern)) = fold_const(&args[2], ctx) else {
                return Some(Err(
                    "geof:relate: the pattern is not a constant string".into()
                ));
            };
            match relate::pattern_needs_intersection(&pattern) {
                Ok(true) => {}
                Ok(false) => {
                    return Some(Err(format!(
                        "geof:relate pattern {pattern:?} holds for geometries that do not intersect"
                    )));
                }
                Err(_) => return Some(Err(format!("geof:relate: invalid pattern {pattern:?}"))),
            }
            let pattern: Arc<str> = if first {
                pattern
            } else {
                transpose(&pattern).into()
            };
            let text = format!("relate {pattern} {}", summary(&q));
            return Some(Ok(Atom {
                test: Some(SpatialTest::Relate(pattern, q.clone())),
                q,
                radius_m: None,
                text,
                sel: 0.5,
            }));
        }
        // distance(?w, C, unit) < r and the mirrored forms
        let Expr::Cmp(a, b, op) = e else {
            return None;
        };
        let (call, bound, upper) = match (geof_call(a), geof_call(b), op) {
            (Some(c), _, CmpOp::Lt | CmpOp::Le) => (c, &**b, true),
            (_, Some(c), CmpOp::Gt | CmpOp::Ge) => (c, &**a, true),
            (Some(c), _, _) | (_, Some(c), _) => (c, &**b, false),
            _ => return None,
        };
        let (name, args) = call;
        if !matches!(name, "distance" | "metricDistance")
            || !args.iter().take(2).any(|a| mentions(a, w))
        {
            return None;
        }
        if !upper {
            return Some(Err(format!(
                "a lower bound on geof:{name} keeps far geometries"
            )));
        }
        let wanted = if name == "distance" { 3 } else { 2 };
        if args.len() != wanted {
            return None;
        }
        let other = if is_w(&args[0]) {
            &args[1]
        } else if is_w(&args[1]) {
            &args[0]
        } else {
            return Some(Err(format!(
                "geof:{name} does not measure from ?{} itself",
                ctx.var_name(w)
            )));
        };
        if mentions(other, w) {
            return None;
        }
        let q = match constant_geom(other, ctx, cfg) {
            Ok(q) => Arc::new(q),
            Err(m) => return Some(Err(format!("geof:{name}: {m}"))),
        };
        let Some(r) = fold_const(bound, ctx).as_ref().and_then(number) else {
            return Some(Err(format!(
                "the bound on geof:{name} is not a constant number"
            )));
        };
        let r_m = if name == "metricDistance" {
            Ok(r)
        } else {
            match fold_const(&args[2], ctx).as_ref().and_then(unit_iri) {
                Some(u) => metres(r, &u),
                None => Err("the unit is not a constant IRI".into()),
            }
        };
        let r_m = match r_m {
            Ok(m) => m.max(0.0),
            Err(m) => return Some(Err(format!("geof:{name}: {m}"))),
        };
        let text = format!(
            "{name} {} {} of {}",
            if matches!(op, CmpOp::Le | CmpOp::Ge) {
                "≤"
            } else {
                "<"
            },
            distance_text(r_m),
            summary(&q)
        );
        Some(Ok(Atom {
            q,
            radius_m: Some(r_m),
            test: None,
            text,
            sel: 0.5,
        }))
    }

    /// area(q) / area(envelope(q)), for points within a polygon.
    fn area_ratio(q: &Geom) -> f64 {
        use georust::Area;
        let Some(b) = q.bbox84() else {
            return 0.5;
        };
        let boxed = (b[2] - b[0]) * (b[3] - b[1]);
        if boxed <= 0.0 {
            return 0.5;
        }
        (q.g.unsigned_area() / boxed).clamp(0.01, 1.0)
    }

    /// Whether the index covers every graph of `gf` (the index skips graphs out of its
    /// scope, a plain scan does not).
    pub(crate) fn scope_covers(cfg: &GeoConfig, gf: &GraphFilter, ctx: &Ctx) -> bool {
        use crate::text::PredicateSet;
        if matches!(cfg.graphs.include, PredicateSet::All) && cfg.graphs.exclude.is_empty() {
            return true;
        }
        let iri = |g: u64| graph_iri(ctx, Id(g));
        match gf {
            GraphFilter::Default => cfg.graph_in_scope(crate::text::DEFAULT_GRAPH_IRI),
            GraphFilter::One(g) => iri(*g).is_some_and(|i| cfg.graph_in_scope(&i)),
            GraphFilter::Set(s) => s
                .iter()
                .all(|g| iri(*g).is_some_and(|i| cfg.graph_in_scope(&i))),
            GraphFilter::All | GraphFilter::Named => false,
        }
    }

    fn intersect(a: &[[f64; 4]], b: &[[f64; 4]]) -> Vec<[f64; 4]> {
        let mut out = Vec::new();
        for x in a {
            for y in b {
                let w = [
                    x[0].max(y[0]),
                    x[1].max(y[1]),
                    x[2].min(y[2]),
                    x[3].min(y[3]),
                ];
                if w[0] <= w[2] && w[1] <= w[3] {
                    out.push(w);
                }
            }
        }
        out
    }

    pub(super) fn push_spatial(n: Node, exprs: Vec<Expr>, ctx: &Ctx) -> (Node, Vec<Expr>) {
        let Kind::Scan(spec) = &n.kind else {
            return (n, exprs);
        };
        if !ctx.opt.spatial_pushdown || exprs.is_empty() {
            return (n, exprs);
        }
        let Some(shape) = ScanShape::of(spec) else {
            return (n, exprs);
        };
        let w = shape.obj;
        if !exprs.iter().any(|e| spatial_conjunct(e, w)) {
            return (n, exprs);
        }
        let cfg = config(ctx);
        let mut atoms = Vec::new();
        let mut pushed = Vec::new();
        let mut rest = Vec::new();
        let mut reasons = Vec::new();
        for e in exprs.iter().cloned().flat_map(Expr::conjuncts) {
            match atom(&e, w, ctx, &cfg) {
                Some(Ok(a)) => {
                    atoms.push(a);
                    pushed.push(e);
                }
                Some(Err(m)) => {
                    reasons.push(m);
                    rest.push(e);
                }
                None => rest.push(e),
            }
        }
        let wname = format!("?{}", ctx.var_name(w));
        let not_pushed = |why: &str| {
            warn(
                ctx,
                "geo-not-pushed",
                format!("spatial filter on {wname} not pushed down: {why}"),
            )
        };
        for m in &reasons {
            not_pushed(m);
        }
        // nothing pushed: the filter as it was
        if atoms.is_empty() {
            return (n, exprs);
        }
        let Some(view) = ctx.snap.geo.as_ref() else {
            not_pushed("the dataset has no spatial index");
            return (n, exprs);
        };
        match view.state() {
            IndexState::Ready => {}
            IndexState::Building(p) => {
                warn(
                    ctx,
                    "geo-index-building",
                    format!(
                        "spatial index building ({:.0}%): the filter on {wname} runs without it",
                        p * 100.0
                    ),
                );
                return (n, exprs);
            }
            s => {
                not_pushed(&format!("the spatial index is {s}"));
                return (n, exprs);
            }
        }
        let Some(slot) = view.predicate_slot(shape.pred) else {
            let p = ctx
                .term(shape.pred)
                .map_or_else(|| "?".into(), |t| short(&t));
            not_pushed(&format!("{p} is not an indexed predicate"));
            return (n, exprs);
        };
        let graph = shape.graph_filter(spec);
        if !scope_covers(&cfg, &graph, ctx) {
            not_pushed("the query reads graphs out of the spatial index's scope");
            return (n, exprs);
        }
        // windows: every match lies in each atom's window
        let world = vec![[-180.0, -90.0, 180.0, 90.0]];
        let mut windows = world;
        for a in &atoms {
            let ws = match (a.q.bbox84(), a.radius_m) {
                (None, _) => Vec::new(),
                (Some(b), None) => vec![b],
                (Some(b), Some(r)) => radius_windows(b, r),
            };
            windows = intersect(&windows, &ws);
        }
        let window_rows = view.estimate(&[slot], &windows).min(n.est);
        let sel: f64 = atoms.iter().map(|a| a.sel).product();
        let refine: f64 = atoms.iter().map(|a| refine_cost(&a.q)).sum();
        // window rows with their exact tests, the tree's levels, and sorting the matches
        // by subject (a scan's rows come sorted)
        let cost = window_rows * (1.0 + refine)
            + 4.0 * view.levels() as f64
            + 0.25 * window_rows * window_rows.max(2.0).log2();
        // the plain scan reads every row and tests it in the filter
        let plain = n.est * (1.0 + refine);
        if cost >= plain {
            return (n, exprs);
        }
        // conjuncts whose exact test the operator does not run stay in the filter
        let mut tests = Vec::new();
        let mut h = Fnv::new();
        h.field(&shape.pred.0.to_le_bytes());
        for (a, e) in atoms.iter().zip(pushed) {
            h.field(e.display(ctx).as_bytes());
            match &a.test {
                Some(t) => tests.push(t.clone()),
                None => rest.push(e),
            }
        }
        for w in &windows {
            for x in w {
                h.field(&x.to_bits().to_le_bytes());
            }
        }
        let est = (window_rows * sel).max(if window_rows > 0.0 { 1.0 } else { 0.0 });
        let desc = format!(
            "{}← {} {} [window ≈ {} of {} rows]",
            n.vars
                .iter()
                .map(|v| format!("?{} ", ctx.var_name(*v)))
                .collect::<String>(),
            ctx.term(shape.pred)
                .map_or_else(|| "?".into(), |t| short(&t)),
            atoms
                .iter()
                .map(|a| a.text.as_str())
                .collect::<Vec<_>>()
                .join(" && "),
            window_rows.round(),
            n.est.round(),
        );
        let s = SpatialScanSpec {
            scan: spec.clone(),
            pred: shape.pred,
            geom_var: w,
            subj_var: shape.subj_var(),
            windows,
            tests,
            filter: Vec::new(),
            key: h.finish(),
        };
        let mut dist = n.dist.clone();
        for d in dist.values_mut() {
            *d = d.min(est.max(1.0));
        }
        let node = Node {
            kind: Kind::SpatialScan(Box::new(s)),
            children: Vec::new(),
            vars: n.vars.clone(),
            certain: n.certain.clone(),
            sorted: shape.sorted(),
            est,
            cost,
            dist,
            desc,
        };
        (node, rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spargebra::SparqlParser;
    use spargebra::algebra::GraphPattern;

    fn bgp(q: &str) -> Vec<TriplePattern> {
        let q = SparqlParser::new()
            .with_prefix("spatial", crate::geo::vocab::SPATIAL)
            .unwrap()
            .parse_query(q)
            .unwrap();
        let spargebra::Query::Select { pattern, .. } = q else {
            panic!()
        };
        let mut gp = &pattern;
        loop {
            match gp {
                GraphPattern::Project { inner, .. } => gp = inner,
                GraphPattern::Bgp { patterns } => return patterns.clone(),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn calls_are_taken_with_their_lists() {
        let ps = bgp("SELECT * { ?f spatial:nearby (51.5 -0.12 5) . ?f ?p ?o . \
             ?g spatial:withinBoxGeom (\"POINT(1 2)\") }");
        let (calls, rest) = take_spatial_calls(&ps).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(rest.len(), 1);
        assert_eq!(calls[0].func, SpatialPfKind::Nearby);
        assert_eq!(calls[0].args.len(), 3);
        assert_eq!(calls[1].func, SpatialPfKind::WithinBoxGeom);
        // unknown names in the namespace are ordinary triples
        let ps = bgp("SELECT * { ?f spatial:nearbyish ?x }");
        assert!(take_spatial_calls(&ps).unwrap().0.is_empty());
    }
}

/// End to end over the GeoSPARQL acceptance fixture: `spatial:` property functions and
/// pushed spatial filters, with and without the index.
#[cfg(all(test, feature = "geo"))]
mod spatial_tests {
    use super::super::{QueryOptions, QueryResult, query};
    use crate::error::Error;
    use crate::io::{RdfFormat, Source};
    use crate::sparql::exec::PlanInfo;
    use crate::store::{Store, StoreOptions};

    const PREFIXES: &str = "PREFIX ex: <http://example.org/> \
        PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
        PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
        PREFIX uom: <http://www.opengis.net/def/uom/OGC/1.0/> \
        PREFIX spatial: <http://jena.apache.org/spatial#> ";

    const FIXTURE: &str = r#"
@prefix ex: <http://example.org/> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
ex:A geo:hasDefaultGeometry ex:gA . ex:gA geo:asWKT "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"^^geo:wktLiteral .
ex:B geo:hasDefaultGeometry ex:gB . ex:gB geo:asWKT "POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))"^^geo:wktLiteral .
ex:C geo:hasDefaultGeometry ex:gC . ex:gC geo:asWKT "POLYGON((10 0, 20 0, 20 10, 10 10, 10 0))"^^geo:wktLiteral .
ex:p1 geo:hasGeometry ex:g1 .  ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral .
ex:p2 geo:hasGeometry ex:g2 .  ex:g2 geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)"^^geo:wktLiteral .
ex:p3 geo:hasGeometry ex:g3 .  ex:g3 geo:asGeoJSON "{\"type\":\"Point\",\"coordinates\":[30,30]}"^^geo:geoJSONLiteral .
ex:bad geo:hasGeometry ex:gX . ex:gX geo:asWKT "POINT(1)"^^geo:wktLiteral .
ex:nil geo:hasGeometry ex:gE . ex:gE geo:asWKT ""^^geo:wktLiteral .
ex:mars geo:hasGeometry ex:gM . ex:gM geo:asWKT "<http://example.org/crs/mars> POINT(1 1)"^^geo:wktLiteral .
ex:G1 { ex:p4 geo:hasGeometry ex:g4 . ex:g4 geo:asWKT "POINT(3 3)"^^geo:wktLiteral . }
"#;

    fn store() -> Store {
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            FIXTURE.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
        s
    }

    fn opts(pushdown: bool) -> QueryOptions {
        let mut o = super::super::ctx::Optimizations::ALL;
        o.spatial_pushdown = pushdown;
        QueryOptions {
            optimizations: Some(o),
            no_cache: true,
            ..Default::default()
        }
    }

    fn run(s: &Store, q: &str) -> crate::error::Result<QueryResult> {
        query(s.snapshot(), &format!("{PREFIXES}{q}"), &opts(true))
    }

    /// The solutions with `ex:` IRIs as local names, sorted.
    fn names(r: &QueryResult) -> Vec<String> {
        let mut v: Vec<String> = r
            .rows()
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|t| {
                        t.map_or("UNDEF".into(), |t| {
                            t.to_string()
                                .replace("<http://example.org/", "")
                                .replace('>', "")
                        })
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|r: &String| !r.contains("fill/"))
            .collect();
        v.sort();
        v
    }

    fn select(s: &Store, q: &str) -> Vec<String> {
        names(&run(s, q).unwrap_or_else(|e| panic!("{q}: {e}")))
    }

    /// An index join in the plan tests a spatial filter on the rows it reads.
    fn filtered_probe(p: &PlanInfo) -> bool {
        (p.operator == "IndexJoin" && p.description.contains("geosparql/sf"))
            || p.children.iter().any(filtered_probe)
    }

    fn find<'a>(p: &'a PlanInfo, op: &str) -> Option<&'a PlanInfo> {
        if p.operator == op {
            return Some(p);
        }
        p.children.iter().find_map(|c| find(c, op))
    }

    #[test]
    fn nearby_finds_features_by_distance() {
        let s = store();
        let r = run(&s, "SELECT ?f { ?f spatial:nearby (2 2 50 uom:kilometre) }").unwrap();
        assert_eq!(names(&r), ["A", "p1"]);
        // without an index the serialization predicates are read
        let pf = find(&r.plan, "SpatialPf").expect("a SpatialPf node");
        let c = pf.counters.as_ref().unwrap();
        assert_eq!(c["fallback"], true);
        assert_eq!(c["index"], "off");
        assert!(
            pf.description.contains(
                "spatial:nearby POINT(2 2) r=50 km [features via hasDefaultGeometry|hasGeometry]"
            ),
            "{}",
            pf.description
        );
        assert_eq!(
            select(
                &s,
                "SELECT ?f { ?f spatial:nearby (2 2 2000 uom:kilometre) }"
            ),
            ["A", "B", "C", "p1", "p2"]
        );
        // the default unit is the kilometre; withinCircle is a synonym
        assert_eq!(
            select(&s, "SELECT ?f { ?f spatial:withinCircle (2 2 50) }"),
            ["A", "p1"]
        );
        assert_eq!(
            select(&s, "SELECT ?f { ?f spatial:nearby (2 2 50000 uom:metre) }"),
            ["A", "p1"]
        );
        assert_eq!(
            select(
                &s,
                "SELECT ?f { ?f spatial:nearbyGeom (\"POINT(2 2)\"^^geo:wktLiteral 50) }"
            ),
            ["A", "p1"]
        );
    }

    #[test]
    fn boxes_are_latitude_first() {
        let s = store();
        assert_eq!(
            select(&s, "SELECT ?f { ?f spatial:withinBox (0 0 5 5) }"),
            ["p1"]
        );
        assert_eq!(
            select(&s, "SELECT ?f { ?f spatial:intersectBox (0 0 5 5) }"),
            ["A", "B", "p1"]
        );
        // the envelope of a geometry argument
        assert_eq!(
            select(
                &s,
                "SELECT ?f { ?f spatial:withinBoxGeom \
                 (\"LINESTRING(-1 -1, 5 5)\"^^geo:wktLiteral) }"
            ),
            ["p1"]
        );
        assert_eq!(
            select(
                &s,
                "SELECT ?f { ?f spatial:intersectBoxGeom \
                 (\"POLYGON((11 1, 12 1, 12 3, 11 3, 11 1))\"^^geo:wktLiteral) }"
            ),
            ["C", "p2"]
        );
    }

    #[test]
    fn cardinal_directions_use_envelopes() {
        let s = store();
        assert_eq!(select(&s, "SELECT ?f { ?f spatial:north (20 0) }"), ["p3"]);
        assert_eq!(
            select(&s, "SELECT ?f { ?f spatial:south (4 0) }"),
            ["A", "C", "p1", "p2"]
        );
        // east of longitude 25 up to 205 = -155, wrapping
        assert_eq!(select(&s, "SELECT ?f { ?f spatial:east (0 25) }"), ["p3"]);
        assert_eq!(
            select(&s, "SELECT ?f { ?f spatial:west (0 4) }"),
            ["A", "p1"]
        );
        assert_eq!(
            select(
                &s,
                "SELECT ?f { ?f spatial:eastGeom (\"POINT(16 0)\"^^geo:wktLiteral) }"
            ),
            ["C", "p3"]
        );
    }

    #[test]
    fn a_limit_keeps_the_nearest_features() {
        let s = store();
        assert_eq!(
            select(
                &s,
                "SELECT ?f { ?f spatial:nearby (0 0 5000 uom:kilometre 2) }"
            ),
            ["A", "p1"]
        );
        // the box functions rank by the distance to the box's centre (here (2.5 2.5))
        assert_eq!(
            select(&s, "SELECT ?f { ?f spatial:intersectBox (0 0 5 5 1) }"),
            ["A"]
        );
        // a limit of 0 means all
        assert_eq!(
            select(&s, "SELECT ?f { ?f spatial:intersectBox (0 0 5 5 0) }"),
            ["A", "B", "p1"]
        );
    }

    #[test]
    fn graphs_bind_the_serialization_graph() {
        let s = store();
        assert_eq!(
            select(
                &s,
                "SELECT ?f ?g { GRAPH ?g { ?f spatial:nearby (3 3 10 uom:kilometre) } }"
            ),
            ["p4 G1"]
        );
        // the default graph alone: A contains the point
        assert_eq!(
            select(&s, "SELECT ?f { ?f spatial:nearby (3 3 10 uom:kilometre) }"),
            ["A"]
        );
        assert_eq!(
            select(
                &s,
                "SELECT ?f FROM ex:G1 { ?f spatial:nearby (3 3 10 uom:kilometre) }"
            ),
            ["p4"]
        );
    }

    #[test]
    fn a_constant_subject_restricts_the_answer() {
        let s = store();
        let ask = |q: &str| run(&s, q).unwrap().boolean;
        assert!(ask("ASK { ex:A spatial:nearby (2 2 50) }"));
        assert!(!ask("ASK { ex:p3 spatial:nearby (2 2 50) }"));
        // a geometry is not a feature
        assert!(!ask("ASK { ex:gA spatial:nearby (2 2 50) }"));
        // joined with other patterns
        assert_eq!(
            select(
                &s,
                "SELECT ?f ?g { ?f spatial:nearby (2 2 50) . ?f geo:hasGeometry ?g }"
            ),
            ["p1 g1"]
        );
    }

    /// GML and KML serializations are indexed and searched like WKT.
    #[test]
    fn gml_and_kml_literals_are_searched() {
        let s = Store::in_memory(StoreOptions::default());
        let data = r#"
@prefix ex: <http://example.org/> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
ex:fg geo:hasGeometry ex:gg . ex:gg geo:asGML "<gml:Point xmlns:gml='http://www.opengis.net/gml/3.2' srsName='http://www.opengis.net/def/crs/EPSG/0/4326'><gml:pos>2 2</gml:pos></gml:Point>"^^geo:gmlLiteral .
ex:fk geo:hasGeometry ex:gk . ex:gk geo:asKML "<Point><coordinates>2.1,2</coordinates></Point>"^^geo:kmlLiteral .
ex:far geo:hasGeometry ex:gf . ex:gf geo:asKML "<Point><coordinates>40,40</coordinates></Point>"^^geo:kmlLiteral .
"#;
        s.load(&[Source::from_bytes(
            data.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        for indexed in [false, true] {
            if indexed {
                s.enable_geo(crate::geo::GeoConfig::default()).unwrap();
            }
            assert_eq!(
                select(&s, "SELECT ?f { ?f spatial:nearby (2 2 50) }"),
                ["fg", "fk"]
            );
            assert_eq!(
                select(
                    &s,
                    "SELECT ?g { ?g geo:asKML ?w FILTER(geof:sfWithin(?w, \
                     \"POLYGON((0 0, 5 0, 5 5, 0 5, 0 0))\"^^geo:wktLiteral)) }"
                ),
                ["gk"]
            );
        }
    }

    #[test]
    fn variable_arguments() {
        let s = store();
        for indexed in [false, true] {
            if indexed {
                s.enable_geo(crate::geo::GeoConfig::default()).unwrap();
            }
            let constant = select(&s, "SELECT ?f { ?f spatial:nearby (2 2 50) }");
            assert_eq!(constant, ["A", "p1"]);
            assert_eq!(
                select(
                    &s,
                    "SELECT ?f { VALUES (?lat ?lon) { (2 2) } \
                     ?f spatial:nearby (?lat ?lon 50 uom:kilometre) }"
                ),
                constant
            );
            // bound by a triple pattern of the group, before or after the call
            assert_eq!(
                select(
                    &s,
                    "SELECT ?f { ?f spatial:nearbyGeom (?w 50 uom:kilometre) . ex:g1 geo:asWKT ?w }"
                ),
                constant
            );
            // one search per binding, each joined with its rows
            assert_eq!(
                select(
                    &s,
                    "SELECT ?f { VALUES ?r { 50 2000 } ?f spatial:nearby (2 2 ?r uom:kilometre) }"
                ),
                ["A", "A", "B", "C", "p1", "p1", "p2"]
            );
            // a binding that is not a valid argument matches nothing; so does an unbound one
            assert_eq!(
                select(
                    &s,
                    "SELECT ?f { VALUES ?lat { 91 \"x\" 2 UNDEF } ?f spatial:nearby (?lat 2 50) }"
                ),
                ["A", "p1"]
            );
            // a feature the group binds is checked against the search
            assert_eq!(
                select(
                    &s,
                    "SELECT ?f { VALUES (?f ?lat) { (ex:A 2) (ex:p3 2) } ?f spatial:nearby (?lat 2 50) }"
                ),
                ["A"]
            );
            // a limit from a variable
            assert_eq!(
                select(
                    &s,
                    "SELECT ?f { BIND(1 AS ?k) ?f spatial:nearby (0 0 5000 uom:kilometre ?k) }"
                ),
                ["A"]
            );
            let r = run(
                &s,
                "SELECT ?f { VALUES ?lat { 2 } ?f spatial:nearby (?lat 2 50) }",
            )
            .unwrap();
            let pf = find(&r.plan, "SpatialPf").expect("a SpatialPf node");
            assert!(
                pf.description.contains("per binding of the arguments"),
                "{}",
                pf.description
            );
        }
    }

    #[test]
    fn argument_errors() {
        let s = store();
        let err = |q: &str| run(&s, q).err().unwrap_or_else(|| panic!("{q}: no error"));
        let invalid = |q: &str| match err(q) {
            Error::Invalid(m) => m,
            e => panic!("{q}: {e:?}"),
        };
        let m = invalid("SELECT ?f { ?f spatial:nearby (91 0 1) }");
        assert!(m.starts_with("spatial:nearby: latitude 91"), "{m}");
        let m = invalid("SELECT ?f { ?f spatial:nearby (0 0 1 uom:parsec) }");
        assert!(m.starts_with("spatial:nearby: unknown unit"), "{m}");
        let m = invalid("SELECT ?f { ?f spatial:nearby (?lat 0 1) }");
        assert_eq!(
            m,
            "spatial:nearby: the argument ?lat is not bound by the rest of the group"
        );
        let m = invalid("SELECT ?f { ?f spatial:nearby (?lat 0) }");
        assert!(m.starts_with("spatial:nearby: expected (lat lon"), "{m}");
        let m = invalid(
            "SELECT ?w { ?g geo:asWKT ?w \
             FILTER(geof:sfWithin(?w, \"POLYGON((0 0, 1 1\"^^geo:wktLiteral)) }",
        );
        assert!(m.starts_with("geo: malformed wktLiteral at offset "), "{m}");
        for (q, start) in [
            (
                "?f spatial:nearby (0 181 1)",
                "spatial:nearby: longitude 181",
            ),
            (
                "?f spatial:nearby (0 0)",
                "spatial:nearby: expected (lat lon",
            ),
            (
                "?f spatial:nearby (0 0 \"x\")",
                "spatial:nearby: the radius",
            ),
            (
                "?f spatial:nearby (0 0 -1)",
                "spatial:nearby: the radius -1",
            ),
            (
                "?f spatial:nearby (0 0 1 uom:metre 1.5)",
                "spatial:nearby: the limit",
            ),
            (
                "?f spatial:withinBox (5 0 0 5)",
                "spatial:withinBox: the box",
            ),
            (
                "?f spatial:nearbyGeom (\"POINT(1\"^^geo:wktLiteral 1)",
                "spatial:nearbyGeom: geo: malformed wktLiteral",
            ),
            (
                "?f spatial:nearbyGeom (\"x\" 1)",
                "spatial:nearbyGeom: expected a geometry",
            ),
            ("(?f ?g) spatial:north (0 0)", "spatial:north: the subject"),
        ] {
            let m = invalid(&format!("SELECT * {{ {q} }}"));
            assert!(m.starts_with(start), "{q}: {m}");
        }
    }

    // ------------------------------------------------------------- filter pushdown --

    use crate::geo::GeoConfig;
    use crate::store::Snapshot;
    use std::sync::Arc;

    const GA: &str = "\"POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))\"^^geo:wktLiteral";
    const GC: &str = "\"POLYGON((10 0, 20 0, 20 10, 10 10, 10 0))\"^^geo:wktLiteral";

    /// Points far from the fixture (no features link them), so that the index is worth
    /// using: a plain scan of `geo:asWKT` reads them all.
    fn fill(s: &Store, n: usize) {
        let mut ttl = String::from("@prefix geo: <http://www.opengis.net/ont/geosparql#> .\n");
        for i in 0..n {
            ttl.push_str(&format!(
                "<http://example.org/fill/{i}> geo:asWKT \"POINT({} -60)\"^^geo:wktLiteral .\n",
                (i % 3000) as f64 / 10.0 - 150.0
            ));
        }
        s.load(&[Source::from_bytes(
            ttl.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    }

    /// The store's snapshot with filler points and the spatial index enabled and built.
    fn indexed(s: &Store) -> Arc<Snapshot> {
        fill(s, 3000);
        s.enable_geo(GeoConfig::default()).unwrap();
        s.snapshot()
    }

    fn run_on(snap: &Arc<Snapshot>, q: &str, pushdown: bool) -> QueryResult {
        let q = format!("{PREFIXES}{q}");
        query(snap.clone(), &q, &opts(pushdown)).unwrap_or_else(|e| panic!("{q}: {e}"))
    }

    /// The answer with the index and without pushdown must be equal; returns it and the
    /// pushed plan.
    fn pushed(snap: &Arc<Snapshot>, q: &str) -> (Vec<String>, QueryResult) {
        let fast = run_on(snap, q, true);
        let slow = run_on(snap, q, false);
        assert_eq!(names(&fast), names(&slow), "{q}");
        assert!(find(&slow.plan, "SpatialScan").is_none(), "{q}");
        // the filter is tested row by row, in a FILTER or on the rows an index join reads
        assert!(
            find(&slow.plan, "Filter").is_some() || filtered_probe(&slow.plan),
            "{q}: {:#?}",
            slow.plan
        );
        assert!(
            find(&fast.plan, "SpatialScan").is_some(),
            "{q}: not pushed: {:#?}",
            fast.plan
        );
        (names(&fast), fast)
    }

    #[test]
    fn relation_filters_search_the_index() {
        let s = store();
        let snap = indexed(&s);
        // a polygon is within itself; g4 is in a named graph, g2 at longitude 12, gX is
        // ill-typed, gE empty, gM in an unknown CRS
        let q = format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, {GA})) }}");
        let (a, r) = pushed(&snap, &q);
        assert_eq!(a, ["g1", "gA"]);
        let scan = find(&r.plan, "SpatialScan").unwrap();
        assert!(
            scan.description
                .contains("<http://www.opengis.net/ont/geosparql#asWKT> sfWithin POLYGON(5 pts)"),
            "{}",
            scan.description
        );
        let c = scan.counters.as_ref().unwrap();
        assert_eq!(c["fallback"], false);
        assert_eq!(c["index"], "ready");
        assert_eq!(c["matched"], 2);
        // EPSG:4326 POINT(2 12) is longitude 12, latitude 2
        let q = format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, {GC})) }}");
        assert_eq!(pushed(&snap, &q).0, ["g2", "gC"]);
        // the constant first: the converse relation
        let q = format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfContains({GA}, ?w)) }}");
        assert_eq!(pushed(&snap, &q).0, ["g1", "gA"]);
        let q = format!(
            "SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfIntersects(?w, {GA}) && ?g != ex:gA) }}"
        );
        assert_eq!(pushed(&snap, &q).0, ["g1", "gB", "gC"]);
        // a folded constant
        let q = format!(
            "SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfTouches(?w, IF(true, {GA}, {GC}))) }}"
        );
        assert_eq!(pushed(&snap, &q).0, ["gC"]);
        // relate with a pattern that needs an intersection, in both argument orders
        let q =
            format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:relate(?w, {GA}, \"T********\")) }}");
        assert_eq!(pushed(&snap, &q).0, ["g1", "gA", "gB"]);
        let q =
            format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:relate({GA}, ?w, \"T*F**F***\")) }}");
        assert_eq!(pushed(&snap, &q).0, ["gA"]);
        let q =
            format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:relate(?w, {GA}, \"T*F**F***\")) }}");
        assert_eq!(pushed(&snap, &q).0, ["g1", "gA"]);
        // with GRAPH ?g, the graph column
        let q = "SELECT ?x ?g { GRAPH ?g { ?x geo:asWKT ?w \
                 FILTER(geof:sfEquals(?w, \"POINT(3 3)\"^^geo:wktLiteral)) } }";
        assert_eq!(pushed(&snap, q).0, ["g4 G1"]);
    }

    #[test]
    fn distance_filters_search_the_index() {
        let s = store();
        let snap = indexed(&s);
        let p = "\"POINT(2 2)\"^^geo:wktLiteral";
        for f in [
            format!("geof:metricDistance(?w, {p}) < 50000"),
            format!("50000 > geof:metricDistance({p}, ?w)"),
            format!("geof:distance(?w, {p}, uom:kilometre) <= 50"),
        ] {
            let q = format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER({f}) }}");
            let (a, r) = pushed(&snap, &q);
            assert_eq!(a, ["g1", "gA"], "{f}");
            // the exact comparison stays in a filter over the scan's candidates
            assert!(find(&r.plan, "Filter").is_some(), "{f}");
        }
        let q = format!(
            "SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:metricDistance(?w, {p}) < 2000000) }}"
        );
        assert_eq!(pushed(&snap, &q).0, ["g1", "g2", "gA", "gB", "gC"]);
    }

    #[test]
    fn filters_the_index_cannot_serve_warn() {
        let s = store();
        let snap = indexed(&s);
        let warned = |q: &str, needle: &str| {
            let r = run_on(&snap, q, true);
            assert!(find(&r.plan, "SpatialScan").is_none(), "{q}");
            assert!(
                r.plan
                    .warnings
                    .iter()
                    .any(|w| w.code == "geo-not-pushed" && w.message.contains(needle)),
                "{q}: {:?}",
                r.plan.warnings
            );
            names(&r)
        };
        let q = format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfDisjoint(?w, {GA})) }}");
        // the empty geometry is disjoint from everything; gM has no common CRS with A
        assert_eq!(warned(&q, "sfDisjoint"), ["g2", "gE"]);
        let q =
            format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:relate(?w, {GA}, \"FF*FF****\")) }}");
        warned(&q, "do not intersect");
        let q = format!(
            "SELECT ?g {{ ?g geo:asWKT ?w BIND(5 AS ?r) FILTER(geof:metricDistance(?w, {GA}) < ?r) }}"
        );
        let _ = q;
        let q =
            format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:metricDistance(?w, {GA}) > 5) }}");
        warned(&q, "lower bound");
        // without an index
        let r = run(
            &store(),
            &format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, {GA})) }}"),
        )
        .unwrap();
        assert_eq!(names(&r), ["g1", "gA"]);
        assert!(
            r.plan
                .warnings
                .iter()
                .any(|w| w.code == "geo-not-pushed" && w.message.contains("no spatial index"))
        );
        // a filter without geof: calls is untouched
        let r = run_on(
            &snap,
            "SELECT ?g { ?g geo:asWKT ?w FILTER(isLiteral(?w)) }",
            true,
        );
        assert!(r.plan.warnings.is_empty());
    }

    /// The planner's node of a query and its context.
    fn plan(snap: &Arc<Snapshot>, q: &str) -> (super::super::ctx::Ctx, super::super::plan::Node) {
        let q = spargebra::SparqlParser::new()
            .parse_query(&format!("{PREFIXES}{q}"))
            .unwrap();
        let spargebra::Query::Select { pattern, .. } = q else {
            panic!()
        };
        let ctx = super::super::ctx::Ctx::new(snap.clone());
        let n = super::super::plan::Planner::new(&ctx)
            .plan(
                &pattern,
                &super::super::plan::ActiveGraph::Default,
                Vec::new(),
            )
            .unwrap();
        (ctx, n)
    }

    fn find_node<'a>(
        n: &'a super::super::plan::Node,
        op: &str,
    ) -> Option<&'a super::super::plan::Node> {
        if n.operator() == op {
            return Some(n);
        }
        n.children.iter().find_map(|c| find_node(c, op))
    }

    #[test]
    fn constants_with_the_same_description_have_different_cache_keys() {
        let s = store();
        let snap = indexed(&s);
        let key = |poly: &str| {
            let (ctx, n) = plan(
                &snap,
                &format!(
                    "SELECT ?g {{ ?g geo:asWKT ?w \
                     FILTER(geof:sfWithin(?w, \"{poly}\"^^geo:wktLiteral)) }}"
                ),
            );
            let scan = find_node(&n, "SpatialScan").expect("pushed");
            (
                scan.desc.clone(),
                super::super::cache::key(scan, &ctx).unwrap().key,
            )
        };
        let a = key("POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))");
        let b = key("POLYGON((0 0, 11 0, 11 11, 0 11, 0 0))");
        assert_eq!(a.0, b.0);
        assert_ne!(a.1, b.1);
        assert_eq!(a, key("POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"));
    }

    #[test]
    fn property_functions_search_the_index() {
        let s = store();
        let snap = indexed(&s);
        let sel = |q: &str| {
            let r = run_on(&snap, q, true);
            let c = find(&r.plan, "SpatialPf")
                .unwrap()
                .counters
                .clone()
                .unwrap();
            assert_eq!(c["fallback"], false, "{q}");
            names(&r)
        };
        assert_eq!(
            sel("SELECT ?f { ?f spatial:nearby (2 2 50 uom:kilometre) }"),
            ["A", "p1"]
        );
        assert_eq!(
            sel("SELECT ?f { ?f spatial:nearby (2 2 2000 uom:kilometre) }"),
            ["A", "B", "C", "p1", "p2"]
        );
        assert_eq!(sel("SELECT ?f { ?f spatial:withinBox (0 0 5 5) }"), ["p1"]);
        assert_eq!(
            sel("SELECT ?f { ?f spatial:intersectBox (0 0 5 5) }"),
            ["A", "B", "p1"]
        );
        assert_eq!(
            sel("SELECT ?f ?g { GRAPH ?g { ?f spatial:nearby (3 3 10 uom:kilometre) } }"),
            ["p4 G1"]
        );
        assert_eq!(sel("SELECT ?f { ?f spatial:east (0 25) }"), ["p3"]);
    }

    #[test]
    fn the_index_is_used_when_it_reads_fewer_rows() {
        let s = store();
        let snap = indexed(&s);
        // a window holding every row: the plain scan is cheaper
        let q = "SELECT ?g { ?g geo:asWKT ?w FILTER(geof:sfIntersects(?w, \
                 \"POLYGON((-180 -90, 180 -90, 180 90, -180 90, -180 -90))\"^^geo:wktLiteral)) }";
        let r = run_on(&snap, q, true);
        assert!(find(&r.plan, "SpatialScan").is_none(), "{:#?}", r.plan);
        assert_eq!(names(&r), names(&run_on(&snap, q, false)));
        assert!(r.plan.warnings.is_empty(), "{:?}", r.plan.warnings);
        // a small window: the index
        let q = format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, {GA})) }}");
        let r = run_on(&snap, &q, true);
        let scan = find(&r.plan, "SpatialScan").expect("pushed");
        assert!(scan.estimated_cost < 1000.0, "{}", scan.estimated_cost);
        // joined with the feature links, the scan's subject order serves a merge join
        let q = format!(
            "SELECT ?f {{ ?f geo:hasDefaultGeometry ?g . ?g geo:asWKT ?w \
             FILTER(geof:sfWithin(?w, {GA})) }}"
        );
        assert_eq!(pushed(&snap, &q).0, ["A"]);
    }

    #[test]
    fn a_building_index_falls_back_with_the_same_answers() {
        let s = store();
        s.pause_geo_build(true);
        s.enable_geo(GeoConfig::default()).unwrap();
        let snap = s.snapshot();
        let q = format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, {GA})) }}");
        let r = run_on(&snap, &q, true);
        assert_eq!(names(&r), ["g1", "gA"]);
        assert!(find(&r.plan, "SpatialScan").is_none());
        assert!(
            r.plan
                .warnings
                .iter()
                .any(|w| w.code == "geo-index-building"),
            "{:?}",
            r.plan.warnings
        );
        let r = run_on(
            &snap,
            "SELECT ?f { ?f spatial:nearby (2 2 50 uom:kilometre) }",
            true,
        );
        assert_eq!(names(&r), ["A", "p1"]);
        let c = find(&r.plan, "SpatialPf")
            .unwrap()
            .counters
            .clone()
            .unwrap();
        assert_eq!(c["fallback"], true);
        assert!(
            c["index"].as_str().unwrap().starts_with("building"),
            "{c:?}"
        );
        s.pause_geo_build(false);
        s.wait_geo();
        let r = run_on(
            &s.snapshot(),
            "SELECT ?f { ?f spatial:nearby (2 2 50 uom:kilometre) }",
            true,
        );
        assert_eq!(names(&r), ["A", "p1"]);
        let c = find(&r.plan, "SpatialPf")
            .unwrap()
            .counters
            .clone()
            .unwrap();
        assert_eq!(
            (&c["fallback"], &c["index"]),
            (&false.into(), &"ready".into())
        );
    }

    /// A literal the index skips for its length still matches a pushed filter and the
    /// property functions, as it matches the plain filter.
    #[test]
    fn literals_too_long_to_index_still_match() {
        let s = store();
        fill(&s, 3000);
        // longer than the 60 bytes indexed below; within A, 1 km from (2 2)
        let big = "POLYGON((1.99 1.99, 2.0 1.99, 2.01 1.99, 2.01 2.01, 1.99 2.01, 1.99 1.99))";
        let ttl = format!(
            "@prefix ex: <http://example.org/> . \
             @prefix geo: <http://www.opengis.net/ont/geosparql#> . \
             ex:bigF geo:hasGeometry ex:big . ex:big geo:asWKT \"{big}\"^^geo:wktLiteral ."
        );
        s.load(&[Source::from_bytes(
            ttl.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        let st = s
            .enable_geo(GeoConfig {
                max_geometry_bytes: 60,
                ..GeoConfig::default()
            })
            .unwrap();
        assert_eq!(st.skipped.too_large, 1);
        let check = |s: &Store| {
            let snap = s.snapshot();
            let q = format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, {GA})) }}");
            let (a, r) = pushed(&snap, &q);
            let c = find(&r.plan, "SpatialScan")
                .unwrap()
                .counters
                .clone()
                .unwrap();
            assert!(c["rechecked"].as_u64().unwrap() >= 1, "{c:?}");
            let q = "SELECT ?g { ?g geo:asWKT ?w FILTER(geof:metricDistance(?w, \
                     \"POINT(2 2)\"^^geo:wktLiteral) < 50000) }";
            let mut near = pushed(&snap, q).0;
            near.retain(|g| g.starts_with("big") || g.starts_with("new"));
            let pf = |q: &str| {
                let r = run_on(&snap, q, true);
                let c = find(&r.plan, "SpatialPf")
                    .unwrap()
                    .counters
                    .clone()
                    .unwrap();
                assert_eq!(c["fallback"], false, "{q}");
                names(&r)
            };
            (
                a,
                near,
                pf("SELECT ?f { ?f spatial:nearby (2 2 50 uom:kilometre) }"),
                pf("SELECT ?f { ?f spatial:nearby (2 2 50 uom:kilometre 3) }"),
                pf("SELECT ?f { ?f spatial:intersectBox (1 1 3 3) }"),
            )
        };
        let (within, near, nearby, nearest, boxed) = check(&s);
        assert_eq!(within, ["big", "g1", "gA"]);
        assert_eq!(near, ["big"]);
        assert_eq!(nearby, ["A", "bigF", "p1"]);
        assert_eq!(nearest, ["A", "bigF", "p1"]);
        assert_eq!(boxed, ["A", "bigF", "p1"]);
        // inserted after the build: the commit path keeps such rows too
        update(
            &s,
            &format!(
                "INSERT DATA {{ ex:newF geo:hasGeometry ex:new . \
                 ex:new geo:asWKT \"{big}\"^^geo:wktLiteral }}"
            ),
        );
        let (within, near, nearby, _, _) = check(&s);
        assert_eq!(within, ["big", "g1", "gA", "new"]);
        assert_eq!(near, ["big", "new"]);
        assert_eq!(nearby, ["A", "bigF", "newF", "p1"]);
        // a given feature reads its links, whatever the index
        let r = run_on(
            &s.snapshot(),
            "ASK { ex:newF spatial:nearby (2 2 50) }",
            true,
        );
        assert!(r.boolean);
        let c = find(&r.plan, "SpatialPf")
            .unwrap()
            .counters
            .clone()
            .unwrap();
        assert_eq!(
            (&c["index"], &c["fallback"]),
            (&"feature-links".into(), &false.into())
        );
    }

    fn update(s: &Store, u: &str) {
        super::super::update::update(s, &format!("{PREFIXES}{u}"), &QueryOptions::default())
            .unwrap();
    }

    #[test]
    fn the_stores_operation_limit_reaches_queries() {
        let s = Store::in_memory(StoreOptions {
            geo_op_vertices: 9,
            ..Default::default()
        });
        let ctx =
            super::super::make_ctx(s.snapshot(), &QueryOptions::default(), None, None).unwrap();
        assert_eq!(ctx.geo.op_vertices(), 9);
        let ctx = super::super::make_ctx(store().snapshot(), &QueryOptions::default(), None, None)
            .unwrap();
        assert_eq!(ctx.geo.op_vertices(), 2_000_000);
        // a union of two 5-point polygons is over a limit of 9 vertices: unbound
        let q = format!("SELECT ?u {{ BIND(geof:union({GA}, {GC}) AS ?u) }}");
        let u = query(s.snapshot(), &format!("{PREFIXES}{q}"), &opts(true)).unwrap();
        assert_eq!(names(&u), ["UNDEF"]);
        let u = run(&store(), &q).unwrap();
        assert!(names(&u)[0].contains("POLYGON"), "{:?}", names(&u));
    }

    #[test]
    fn an_update_sees_its_own_geometries() {
        let s = store();
        super::super::update::update(
            &s,
            &format!(
                "{PREFIXES} INSERT DATA {{ ex:p5 geo:hasGeometry ex:g5 . \
                 ex:g5 geo:asWKT \"POINT(1 80)\"^^geo:wktLiteral }} ; \
                 INSERT {{ ?f a ex:Northern }} WHERE {{ ?f spatial:north (75 0) }}"
            ),
            &QueryOptions::default(),
        )
        .unwrap();
        assert_eq!(select(&s, "SELECT ?f { ?f a ex:Northern }"), ["p5"]);
        // with the index: the transaction's own view runs without it
        s.enable_geo(GeoConfig::default()).unwrap();
        super::super::update::update(
            &s,
            &format!(
                "{PREFIXES} INSERT DATA {{ ex:p6 geo:hasGeometry ex:g6 . \
                 ex:g6 geo:asWKT \"POINT(1 -80)\"^^geo:wktLiteral }} ; \
                 INSERT {{ ?f a ex:Southern }} WHERE {{ ?f spatial:south (-75 0) }} ; \
                 INSERT {{ ?g a ex:Polar }} WHERE {{ ?g geo:asWKT ?w \
                   FILTER(geof:sfIntersects(?w, \"LINESTRING(-180 -80, 180 -80)\"^^geo:wktLiteral)) }}"
            ),
            &QueryOptions::default(),
        )
        .unwrap();
        assert_eq!(select(&s, "SELECT ?f { ?f a ex:Southern }"), ["p6"]);
        assert_eq!(select(&s, "SELECT ?g { ?g a ex:Polar }"), ["g6"]);
    }
}
