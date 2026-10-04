//! Spatial joins and k-nearest-neighbour ordering in the planner.
//!
//! ```sparql
//! ?a geo:asWKT ?wa . ?b geo:asWKT ?wb FILTER(geof:sfContains(?wa, ?wb))
//! ?g geo:asWKT ?w BIND(geof:metricDistance(?w, "POINT(9 1)"^^geo:wktLiteral) AS ?d)
//!   FILTER(BOUND(?d)) } ORDER BY ?d LIMIT 2
//! ```
//!
//! * A FILTER conjunct `geof:R(?a, ?b)` (a relation that needs the geometries to meet),
//!   `geof:relate(?a, ?b, pattern)` with such a pattern, or `geof:distance(?a, ?b, u) < r`
//!   with a constant `r`, whose two geometries are bound by different join components,
//!   joins those components with a [`SpatialJoinSpec`] instead of a cross product and a
//!   filter. A component that is a plain scan of an indexed predicate is searched in the
//!   spatial index instead of being read; any other is planned as usual and its distinct
//!   geometries are packed into a tree for the query.
//! * `ORDER BY ASC(geof:distance(?w, C, u)) LIMIT k` (or a variable bound to it) over a
//!   group whose geometries come from one indexed scan, and whose other patterns connect
//!   to that scan (a star on its subject, or a path from it), becomes a
//!   [`SpatialKnnSpec`] below the top-k: the group runs over the scan's rows in
//!   increasing distance, in batches, until `k` rows are proven.
//!
//! Both keep the plain plan's answers: the trees only propose candidates, and every
//! candidate is tested with the code the `geof:` functions use. A shape that is
//! recognized but cannot be rewritten (a disjointness test, a lower bound on a distance,
//! an angular unit, an index that is not ready, …) keeps the generic plan, and the plan
//! carries a warning naming the reason.

use super::ctx::Ctx;
use super::expr::Expr;
use super::plan::{Node, ScanSpec};
use super::table::VarId;
use crate::geo::{GeomRef, Relation};
use crate::id::Id;
use std::sync::Arc;

/// The exact test of a spatial join of the left geometry `?a` with the right one `?b`.
#[derive(Clone, Debug)]
pub enum JoinTest {
    /// `geof:<relation>(?a, ?b)`; never a disjoint relation
    Relation(Relation),
    /// `geof:relate(?a, ?b, pattern)`, with a pattern that needs the geometries to meet
    Relate(Arc<str>),
    /// `geof:distance(?a, ?b, u)` below `metres` (`<=` when inclusive)
    Within { metres: f64, inclusive: bool },
}

/// One input of a spatial join.
#[derive(Clone)]
pub enum JoinSide {
    /// a scan of an indexed predicate, probed in the spatial index instead of being read
    /// as a table
    Index {
        scan: ScanSpec,
        pred: Id,
        geom_var: VarId,
        subj_var: Option<VarId>,
        graph_var: Option<VarId>,
    },
    /// the join node's child `child`, any plan; its distinct geometries are boxed and
    /// packed into a tree for the query
    Plan { child: usize, geom_var: VarId },
}

/// A join of two inputs on a spatial test of one geometry from each.
#[derive(Clone)]
pub struct SpatialJoinSpec {
    pub test: JoinTest,
    pub left: JoinSide,
    pub right: JoinSide,
    /// conjuncts over the joined rows evaluated as an ordinary filter (the pushed
    /// conjunct is not among them, unless `test` only bounds it)
    pub filter: Vec<Expr>,
    /// result-cache key of the test, the sides' scans and constants
    pub key: u64,
}

/// The rows of a template plan in increasing distance of an indexed scan's geometry to a
/// constant, until the first `k` are proven (the top-k above finishes the order).
///
/// The node's only child is the template: the group's plan with the indexed scan
/// replaced by an empty `Values` leaf at `placeholder`, which each batch of candidates
/// fills.
#[derive(Clone)]
pub struct SpatialKnnSpec {
    pub scan: ScanSpec,
    pub pred: Id,
    pub geom_var: VarId,
    pub subj_var: Option<VarId>,
    pub graph_var: Option<VarId>,
    /// the constant geometry distances are measured to
    pub q: GeomRef,
    pub k: usize,
    /// metres in one unit of the ordering key
    pub metres_per_unit: f64,
    /// the template may keep rows whose distance is an error (they sort first)
    pub errors: bool,
    /// child positions from the template's root to the placeholder leaf
    pub placeholder: Vec<usize>,
    /// result-cache key of the scan, the constant, `k` and the unit
    pub key: u64,
    /// the ordering key as the template's rows evaluate it: the distance call, or the
    /// variable bound to it
    pub order: Expr,
    /// the scan's geometry is the call's first argument (the distance is measured in
    /// its CRS)
    pub w_first: bool,
}

/// Join components of one group that a spatial conjunct of `filters` connects (the
/// conjunct moves into the join).
pub(super) fn spatial_joins(parts: &mut Vec<Node>, filters: &mut Vec<Expr>, ctx: &Ctx) {
    #[cfg(not(feature = "geo"))]
    let _ = (parts, filters, ctx);
    #[cfg(feature = "geo")]
    on::spatial_joins(parts, filters, ctx);
}

/// An `ORDER BY … LIMIT k` node as a k-nearest-neighbour search when its shape allows,
/// else `n` unchanged.
pub(super) fn spatial_knn(n: Node, ctx: &Ctx) -> Node {
    #[cfg(not(feature = "geo"))]
    {
        let _ = ctx;
        n
    }
    #[cfg(feature = "geo")]
    on::spatial_knn(n, ctx)
}

#[cfg(feature = "geo")]
mod on {
    use super::super::expr::CmpOp;
    use super::super::geopf::{
        ScanShape, constant_geom, distance_text, fold_const, geof_call, number, scope_covers,
        unit_iri, warn,
    };
    use super::super::plan::{self, JoinAlgo, Kind, merge_dist, short};
    use super::super::table::Table;
    use super::super::value::Value;
    use super::*;
    use crate::geo::exec::{config, radius_windows, summary};
    use crate::geo::ops::relate;
    use crate::geo::units::{self, UnitKind};
    use crate::geo::{Fnv, GeoConfig, Geom, IndexState};

    const NOT_JOINED: &str = "geo-not-joined";
    const NOT_KNN: &str = "geo-not-knn";
    /// The description of the placeholder leaf of a k-NN template.
    const PLACEHOLDER: &str = "nearest candidates";
    /// Index boxes sampled for the estimate of a join.
    const SAMPLE: usize = 1024;
    /// Metres per radian of arc on a sphere at least as large as every radius of
    /// curvature of the WGS 84 ellipsoid: no distance between two points exceeds their
    /// central angle times this.
    const REACH_PER_RADIAN: f64 = 6_400_000.0;

    fn var(e: &Expr) -> Option<VarId> {
        match e {
            Expr::Var(v) => Some(*v),
            _ => None,
        }
    }

    fn name(ctx: &Ctx, v: VarId) -> String {
        format!("?{}", ctx.var_name(v))
    }

    // ------------------------------------------------------------------ joins ------

    /// What a join conjunct tests, once recognized.
    struct Shape {
        test: JoinTest,
        /// the conjunct stays as the exact test (the kernel's test only bounds it)
        keep: bool,
        /// for the plan's description, between the two variables
        text: String,
    }

    /// A conjunct comparing two geometry variables `(a, b)` (in argument order) that a
    /// spatial join could serve: its test, or why it cannot serve it.
    fn conjunct(e: &Expr, ctx: &Ctx) -> Option<(VarId, VarId, std::result::Result<Shape, String>)> {
        if let Some((fname, args)) = geof_call(e) {
            let rel = Relation::from_local(fname);
            if rel.is_none() && fname != "relate" {
                return None;
            }
            if args.len() != if rel.is_some() { 2 } else { 3 } {
                return None;
            }
            let (Some(a), Some(b)) = (var(&args[0]), var(&args[1])) else {
                return None;
            };
            if a == b {
                return None;
            }
            let shape = match rel {
                Some(r) if !r.index_usable() => Err(format!(
                    "geof:{fname} holds for geometries that do not meet, which no tree finds"
                )),
                Some(r) => Ok(Shape {
                    test: JoinTest::Relation(r),
                    keep: false,
                    text: fname.to_string(),
                }),
                None => match fold_const(&args[2], ctx) {
                    Some(Value::Str(p)) => match relate::pattern_needs_intersection(&p) {
                        Ok(true) => Ok(Shape {
                            text: format!("relate {p}"),
                            test: JoinTest::Relate(p),
                            keep: false,
                        }),
                        Ok(false) => Err(format!(
                            "geof:relate pattern {p:?} holds for geometries that do not intersect"
                        )),
                        Err(_) => Err(format!("geof:relate: invalid pattern {p:?}")),
                    },
                    _ => Err("geof:relate: the pattern is not a constant string".into()),
                },
            };
            return Some((a, b, shape));
        }
        // distance(?a, ?b, unit) < r and the mirrored forms
        let Expr::Cmp(x, y, op) = e else {
            return None;
        };
        let (call, bound, upper) = match (geof_call(x), geof_call(y), op) {
            (Some(c), _, CmpOp::Lt | CmpOp::Le) => (c, &**y, true),
            (_, Some(c), CmpOp::Gt | CmpOp::Ge) => (c, &**x, true),
            (Some(c), _, _) => (c, &**y, false),
            (_, Some(c), _) => (c, &**x, false),
            _ => return None,
        };
        let (fname, args) = call;
        match (fname, args.len()) {
            ("distance", 3) | ("metricDistance", 2) => {}
            _ => return None,
        }
        let (Some(a), Some(b)) = (var(&args[0]), var(&args[1])) else {
            return None;
        };
        if a == b {
            return None;
        }
        Some((a, b, distance_shape(fname, args, bound, upper, *op, ctx)))
    }

    /// The test of `geof:<fname>(?a, ?b[, unit]) < bound` (`upper`: the bound is above
    /// the distance).
    fn distance_shape(
        fname: &str,
        args: &[Expr],
        bound: &Expr,
        upper: bool,
        op: CmpOp,
        ctx: &Ctx,
    ) -> std::result::Result<Shape, String> {
        if !upper {
            return Err(format!(
                "a lower bound on geof:{fname} keeps far geometries"
            ));
        }
        let Some(r) = fold_const(bound, ctx).as_ref().and_then(number) else {
            return Err(format!(
                "the bound on geof:{fname} is not a constant number"
            ));
        };
        let (unit, unit_text) = if fname == "metricDistance" {
            (units::Unit::METRE, None)
        } else {
            let iri = fold_const(&args[2], ctx)
                .as_ref()
                .and_then(unit_iri)
                .ok_or_else(|| format!("geof:{fname}: the unit is not a constant IRI"))?;
            let u =
                units::unit(&iri).ok_or_else(|| format!("geof:{fname}: unknown unit <{iri}>"))?;
            let local = iri.rsplit(['#', '/']).next().unwrap_or(&iri).to_string();
            (u, Some(local))
        };
        let inclusive = matches!(op, CmpOp::Le | CmpOp::Ge);
        // the kernel tests in metres: exactly for metres; for other units within a bound
        // that every match keeps, with the conjunct itself as the exact test
        let (metres, keep) = match unit.kind {
            UnitKind::Length if unit.factor == 1.0 => (r, false),
            UnitKind::Length => (r * unit.factor * (1.0 + 1e-9), true),
            UnitKind::Angle => (r * unit.factor * REACH_PER_RADIAN * (1.0 + 1e-9), true),
            UnitKind::Area => return Err(format!("geof:{fname}: the unit is not a length")),
        };
        let cmp = if inclusive { "≤" } else { "<" };
        let text = match unit_text {
            None => format!("{fname} {cmp} {}", distance_text(r)),
            Some(u) => format!("{fname} {cmp} {r} {u}"),
        };
        Ok(Shape {
            test: JoinTest::Within {
                metres,
                inclusive: inclusive || keep,
            },
            keep,
            text,
        })
    }

    pub(super) fn spatial_joins(parts: &mut Vec<Node>, filters: &mut Vec<Expr>, ctx: &Ctx) {
        let mut i = 0;
        while parts.len() >= 2 && i < filters.len() {
            let Some((a, b, shape)) = conjunct(&filters[i], ctx) else {
                i += 1;
                continue;
            };
            let at = |v: VarId| parts.iter().position(|p| p.vars.contains(&v));
            let (Some(x), Some(y)) = (at(a), at(b)) else {
                i += 1;
                continue;
            };
            if x == y || !ctx.opt.spatial_join {
                i += 1;
                continue;
            }
            let shape = match shape {
                Ok(s) => s,
                Err(why) => {
                    warn(
                        ctx,
                        NOT_JOINED,
                        format!(
                            "spatial filter on {} and {} not joined: {why}",
                            name(ctx, a),
                            name(ctx, b)
                        ),
                    );
                    i += 1;
                    continue;
                }
            };
            let conj = filters.remove(i);
            let hi = parts.remove(x.max(y));
            let lo = parts.remove(x.min(y));
            let (l, r) = if x < y { (lo, hi) } else { (hi, lo) };
            let n = join_node(l, r, (a, b), shape, conj, ctx);
            parts.push(n);
        }
    }

    /// The join side of the component `n` for its geometry `v`: a plain scan of an
    /// indexed predicate is searched in the index (when it is ready and covers what the
    /// scan reads); anything else is planned as usual.
    fn index_side(n: &Node, v: VarId, test: &JoinTest, ctx: &Ctx) -> Option<JoinSide> {
        let Kind::Scan(spec) = &n.kind else {
            return None;
        };
        let shape = ScanShape::of(spec)?;
        if shape.obj != v {
            return None;
        }
        let view = ctx.snap.geo.as_deref()?;
        let base = view.usable()?;
        view.predicate_slot(shape.pred)?;
        if !scope_covers(&view.config, &shape.graph_filter(spec), ctx) {
            return None;
        }
        // literals in a CRS the index cannot place are not in it, but two of them in the
        // same CRS can be in a relation: such a scan is read instead
        if !matches!(test, JoinTest::Within { .. }) && base.column.counts().unknown_crs > 0 {
            return None;
        }
        Some(JoinSide::Index {
            scan: spec.clone(),
            pred: shape.pred,
            geom_var: v,
            subj_var: shape.subj_var(),
            graph_var: shape.graph_var(),
        })
    }

    /// The share of the index's rows whose box a sample of its own boxes meets (within
    /// the distance of the test), or `None` without a tree.
    fn sample_share(test: &JoinTest, ctx: &Ctx) -> Option<f64> {
        let base = ctx.snap.geo.as_deref()?.usable()?;
        let tree = base.tree.as_ref()?;
        let items = tree.tree().level_boxes(0);
        let total = items.len() / 4;
        if total == 0 {
            return None;
        }
        let (mut sum, mut k) = (0.0, 0usize);
        for i in (0..total).step_by(total.div_ceil(SAMPLE).max(1)) {
            let b = [
                f64::from(items[4 * i]),
                f64::from(items[4 * i + 1]),
                f64::from(items[4 * i + 2]),
                f64::from(items[4 * i + 3]),
            ];
            let windows = match test {
                JoinTest::Within { metres, .. } => radius_windows(b, *metres),
                _ => vec![b],
            };
            sum += tree.estimate(&windows);
            k += 1;
        }
        Some((sum / k as f64 / total as f64).clamp(0.0, 1.0))
    }

    fn join_node(
        l: Node,
        r: Node,
        (a, b): (VarId, VarId),
        shape: Shape,
        conj: Expr,
        ctx: &Ctx,
    ) -> Node {
        let (n, m) = (l.est, r.est);
        // candidate pairs: as the index's own boxes meet each other, else each row of
        // the larger side meeting about one of the smaller
        let cands = match sample_share(&shape.test, ctx) {
            Some(s) => n * m * s,
            None => n.max(m),
        };
        let est = (0.5 * cands).max(if n > 0.0 && m > 0.0 { 1.0 } else { 0.0 });
        let mut vars = l.vars.clone();
        vars.extend(r.vars.iter().filter(|v| !l.vars.contains(v)));
        let mut certain = l.certain.clone();
        certain.extend(r.certain.iter().filter(|v| !l.certain.contains(v)));
        let dist = merge_dist(&l, &r, est, None);
        // packing or probing the boxes, and the exact test of each candidate
        let mut cost = (n + m) * (n.min(m) + 2.0).log2() + cands;
        let mut children = Vec::new();
        let mut index_pred = None;
        let mut side = |p: Node, v: VarId| match index_side(&p, v, &shape.test, ctx) {
            Some(s) => {
                if let JoinSide::Index { pred, .. } = &s {
                    index_pred.get_or_insert(*pred);
                }
                s
            }
            None => {
                cost += p.cost;
                // the fusion of subject stars that follows the join ordering does not
                // look into a spatial join
                children.push(super::super::indexjoin::fuse_stars(p, ctx));
                JoinSide::Plan {
                    child: children.len() - 1,
                    geom_var: v,
                }
            }
        };
        let left = side(l, a);
        let right = side(r, b);
        let how = match index_pred {
            Some(p) => format!(
                "index nested loop on {}",
                ctx.term(p).map_or_else(|| "?".into(), |t| short(&t))
            ),
            None => "tree join".into(),
        };
        let desc = format!("{} {} {} [{how}]", name(ctx, a), shape.text, name(ctx, b));
        let mut h = Fnv::new();
        match &shape.test {
            JoinTest::Relation(r) => h.field(r.local().as_bytes()),
            JoinTest::Relate(p) => h.field(format!("relate {p}").as_bytes()),
            JoinTest::Within { metres, inclusive } => {
                h.field(&metres.to_bits().to_le_bytes());
                h.field(&[u8::from(*inclusive)]);
            }
        }
        h.field(conj.display(ctx).as_bytes());
        for s in [&left, &right] {
            h.field(side_key(s, ctx).as_bytes());
        }
        let spec = SpatialJoinSpec {
            test: shape.test,
            left,
            right,
            filter: if shape.keep { vec![conj] } else { Vec::new() },
            key: h.finish(),
        };
        Node {
            kind: Kind::SpatialJoin(Box::new(spec)),
            children,
            vars,
            certain,
            sorted: Vec::new(),
            est,
            cost,
            dist,
            desc,
        }
    }

    fn side_key(s: &JoinSide, ctx: &Ctx) -> String {
        match s {
            JoinSide::Index {
                scan,
                pred,
                geom_var,
                subj_var,
                graph_var,
            } => format!(
                "index {scan:?} {} {} {:?} {:?}",
                pred.0,
                name(ctx, *geom_var),
                subj_var.map(|v| name(ctx, v)),
                graph_var.map(|v| name(ctx, v))
            ),
            JoinSide::Plan { child, geom_var } => {
                format!("plan {child} {}", name(ctx, *geom_var))
            }
        }
    }

    // -------------------------------------------------------------------- k-NN ------

    /// An ordering key `geof:metricDistance(?w, C)` or `geof:distance(?w, C, unit)` (either
    /// argument order) with a constant geometry `C`.
    struct Distance {
        w: VarId,
        w_first: bool,
        q: Geom,
        fname: &'static str,
        /// the unit of the key (metres for `metricDistance`)
        unit: std::result::Result<units::Unit, String>,
        /// the unit's local name, for the description
        unit_text: Option<String>,
    }

    fn distance_call(
        e: &Expr,
        ctx: &Ctx,
        cfg: &GeoConfig,
    ) -> Option<std::result::Result<Distance, String>> {
        let (fname, args) = geof_call(e)?;
        let fname = match (fname, args.len()) {
            ("metricDistance", 2) => "metricDistance",
            ("distance", 3) => "distance",
            _ => return None,
        };
        let (w, w_first, other) = match (var(&args[0]), var(&args[1])) {
            (Some(w), None) => (w, true, &args[1]),
            (None, Some(w)) => (w, false, &args[0]),
            _ => return None,
        };
        if !other.var_set().is_empty() || other.has_exists() {
            return None;
        }
        let q = match constant_geom(other, ctx, cfg) {
            Ok(q) => q,
            Err(m) => return Some(Err(format!("geof:{fname}: {m}"))),
        };
        let (unit, unit_text) = if fname == "metricDistance" {
            (Ok(units::Unit::METRE), None)
        } else {
            match fold_const(&args[2], ctx).as_ref().and_then(unit_iri) {
                Some(iri) => (
                    units::unit(&iri).ok_or_else(|| format!("unknown unit <{iri}>")),
                    Some(iri.rsplit(['#', '/']).next().unwrap_or(&iri).to_string()),
                ),
                None => (Err("the unit is not a constant IRI".to_string()), None),
            }
        };
        Some(Ok(Distance {
            w,
            w_first,
            q,
            fname,
            unit,
            unit_text,
        }))
    }

    /// The unary operators over a group's joins (filters, binds, optional parts on the
    /// right), top down, and the joins below them.
    fn split_chain(mut n: &Node) -> (Vec<&Node>, &Node) {
        let mut chain = Vec::new();
        while matches!(
            n.kind,
            Kind::Filter(_) | Kind::Extend(..) | Kind::LeftJoin { .. }
        ) {
            chain.push(n);
            n = &n.children[0];
        }
        (chain, n)
    }

    /// The leaves and filters of a tree of joins; `false` when something else is in it.
    fn flatten(n: &Node, ctx: &Ctx, leaves: &mut Vec<Node>, filters: &mut Vec<Expr>) -> bool {
        match &n.kind {
            Kind::Join { algo, .. } if !matches!(algo, JoinAlgo::Cross) => {
                n.children.iter().all(|c| flatten(c, ctx, leaves, filters))
            }
            Kind::Sort(_) => flatten(&n.children[0], ctx, leaves, filters),
            Kind::Filter(es) if !es.iter().any(Expr::has_exists) => {
                filters.extend(es.iter().cloned());
                flatten(&n.children[0], ctx, leaves, filters)
            }
            Kind::IndexJoin(j) => {
                if !flatten(&n.children[0], ctx, leaves, filters) {
                    return false;
                }
                // each probe as the scan it reads
                for p in &j.probes {
                    let vars: Vec<VarId> = p.scan.cols.iter().map(|c| c.1).collect();
                    let est = ctx
                        .snap
                        .count(p.scan.perm, &p.scan.prefix)
                        .map_or(n.est, |c| c as f64);
                    let mut leaf = Node::leaf(
                        Kind::Scan(p.scan.clone()),
                        vars.clone(),
                        est,
                        format!("{} {:?}", p.scan.perm.name().to_uppercase(), p.scan.prefix),
                    );
                    leaf.sorted = vars;
                    leaves.push(leaf);
                    filters.extend(p.filter.iter().cloned());
                }
                true
            }
            Kind::Scan(_)
            | Kind::RangeScan(..)
            | Kind::Values(_)
            | Kind::VectorSearch(_)
            | Kind::HybridSearch(_)
            | Kind::SpatialScan(_)
            | Kind::SpatialRelate(_) => {
                leaves.push(n.clone());
                true
            }
            Kind::SpatialPf(_) | Kind::TextSearch(_) if n.children.is_empty() => {
                leaves.push(n.clone());
                true
            }
            _ => false,
        }
    }

    pub(super) fn spatial_knn(n: Node, ctx: &Ctx) -> Node {
        let Kind::OrderBy {
            keys,
            limit: Some(k),
        } = &n.kind
        else {
            return n;
        };
        let k = *k;
        let Some((key, asc)) = keys.first() else {
            return n;
        };
        let child = &n.children[0];
        let (chain, _) = split_chain(child);
        let call = match key {
            Expr::Var(d) => {
                let bound = chain.iter().find_map(|c| match &c.kind {
                    Kind::Extend(v, e) if v == d => Some(e),
                    _ => None,
                });
                match bound {
                    Some(e) => e,
                    None => return n,
                }
            }
            e => e,
        };
        let cfg = config(ctx);
        let Some(dist) = distance_call(call, ctx, &cfg) else {
            return n;
        };
        if !ctx.opt.spatial_knn || k == 0 {
            return n;
        }
        let dist = match dist {
            Ok(d) => d,
            Err(why) => {
                warn(
                    ctx,
                    NOT_KNN,
                    format!("nearest-neighbour order not used: {why}"),
                );
                return n;
            }
        };
        let w = dist.w;
        let knn = if *asc {
            knn_node(child, key, call, dist, k, ctx)
        } else {
            Err("the order is descending".into())
        };
        match knn {
            Ok(node) => {
                let mut n = n;
                n.cost += node.cost - n.children[0].cost;
                n.children[0] = node;
                n
            }
            Err(why) => {
                warn(
                    ctx,
                    NOT_KNN,
                    format!(
                        "nearest-neighbour order on {} not used: {why}",
                        name(ctx, w)
                    ),
                );
                n
            }
        }
    }

    /// Whether the conjunct removes the rows whose ordering key is an error.
    fn drops_errors(e: &Expr, key: &str, call: &str, ctx: &Ctx) -> bool {
        let is_key = |x: &Expr| {
            let d = x.display(ctx);
            d == key || d == call
        };
        match e {
            Expr::Bound(v) => is_key(&Expr::Var(*v)),
            Expr::Cmp(a, b, _) => is_key(a) || is_key(b),
            _ => false,
        }
    }

    fn knn_node(
        child: &Node,
        key: &Expr,
        call: &Expr,
        dist: Distance,
        k: usize,
        ctx: &Ctx,
    ) -> std::result::Result<Node, String> {
        let w = dist.w;
        let unit = dist.unit?;
        let metres_per_unit = match unit.kind {
            UnitKind::Length => unit.factor,
            UnitKind::Angle => return Err("the unit is an angle".into()),
            UnitKind::Area => return Err("the unit is not a length".into()),
        };
        if dist.q.empty {
            return Err("the constant geometry is empty".into());
        }
        if !dist.q.crs.known().is_some_and(|c| c.is_geographic()) {
            return Err("the constant geometry is not in longitude and latitude".into());
        }
        let view = ctx
            .snap
            .geo
            .as_deref()
            .ok_or("the dataset has no spatial index")?;
        match view.state() {
            IndexState::Ready => {}
            IndexState::Building(p) => {
                return Err(format!("the spatial index is building ({:.0}%)", p * 100.0));
            }
            s => return Err(format!("the spatial index is {s}")),
        }
        let (chain, core) = split_chain(child);
        let mut leaves = Vec::new();
        let mut filters = Vec::new();
        if !flatten(core, ctx, &mut leaves, &mut filters) {
            return Err("the group is not a join of patterns under its filters and binds".into());
        }
        let at: Vec<usize> = (0..leaves.len())
            .filter(|&i| leaves[i].vars.contains(&w))
            .collect();
        let [at] = at.as_slice() else {
            return Err(format!(
                "{} is bound by more than one pattern",
                name(ctx, w)
            ));
        };
        let scan_node = leaves.remove(*at);
        let Kind::Scan(spec) = &scan_node.kind else {
            return Err(format!("{} is not read by a plain scan", name(ctx, w)));
        };
        let spec = spec.clone();
        let shape = ScanShape::of(&spec)
            .filter(|s| s.obj == w)
            .ok_or_else(|| format!("{} is not the object of a scan", name(ctx, w)))?;
        if view.predicate_slot(shape.pred).is_none() {
            let p = ctx
                .term(shape.pred)
                .map_or_else(|| "?".into(), |t| short(&t));
            return Err(format!("{p} is not an indexed predicate"));
        }
        if !scope_covers(&view.config, &shape.graph_filter(&spec), ctx) {
            return Err("the query reads graphs out of the spatial index's scope".into());
        }
        // the other patterns are read per batch, each joined on a variable bound before
        // it (from the batch's subjects on), so that they are probed for its keys
        let mut bound = scan_node.vars.clone();
        let mut joined = Vec::new();
        while !leaves.is_empty() {
            let Some(i) = leaves
                .iter()
                .position(|l| l.vars.iter().any(|v| bound.contains(v)))
            else {
                return Err(format!(
                    "the group's other patterns are not connected to the scan of {}",
                    name(ctx, w)
                ));
            };
            let l = leaves.remove(i);
            bound.extend(l.vars.iter().copied());
            joined.push(l);
        }
        // the template: a batch of the scan's rows joined with the other patterns, the
        // filters among them, then the operators above the joins
        let batch = (2 * k).max(64) as f64;
        let mut ph = Node::leaf(
            Kind::Values(Table::new(scan_node.vars.clone())),
            scan_node.vars.clone(),
            batch,
            PLACEHOLDER.into(),
        );
        ph.dist = scan_node
            .vars
            .iter()
            .map(|&v| (v, batch.min(scan_node.d(v))))
            .collect();
        let mut t = ph;
        for leaf in joined {
            t = plan::join(t, leaf, ctx);
        }
        if !filters.is_empty() {
            t = plan::filter(t, filters.clone(), ctx);
        }
        t = super::super::indexjoin::fuse_stars(t, ctx);
        for c in chain.iter().rev() {
            let mut c = (*c).clone();
            c.cost = c.cost - c.children[0].cost + t.cost;
            c.children[0] = t;
            c.sorted.clear();
            t = c;
        }
        let mut placeholder = Vec::new();
        if !path_to(&t, &mut placeholder) {
            return Err("the template lost its placeholder".into());
        }
        // rows whose distance is an error sort first, unless a filter on the key removes
        // them
        let (key_text, call_text) = (key.display(ctx), call.display(ctx));
        let errors = !chain
            .iter()
            .filter_map(|c| match &c.kind {
                Kind::Filter(es) => Some(es.iter()),
                _ => None,
            })
            .flatten()
            .chain(filters.iter())
            .flat_map(|e| e.clone().conjuncts())
            .any(|e| drops_errors(&e, &key_text, &call_text, ctx));
        let mut h = Fnv::new();
        h.field(format!("{spec:?}").as_bytes());
        h.field(&shape.pred.0.to_le_bytes());
        h.field(key_text.as_bytes());
        h.field(call_text.as_bytes());
        h.field(&(k as u64).to_le_bytes());
        h.field(&metres_per_unit.to_bits().to_le_bytes());
        h.field(&[u8::from(errors), u8::from(dist.w_first)]);
        let desc = format!(
            "{} k={k} {}{} {}",
            name(ctx, w),
            dist.fname,
            dist.unit_text.map(|u| format!(" {u}")).unwrap_or_default(),
            summary(&dist.q)
        );
        // a few batches of the template, and the nearest-first search
        let est = child.est.min(t.est.max(k as f64));
        let cost = 2.0 * t.cost + k as f64 * (scan_node.est + 2.0).log2();
        let mut dists = child.dist.clone();
        for d in dists.values_mut() {
            *d = d.min(est.max(1.0));
        }
        let knn = SpatialKnnSpec {
            scan: spec,
            pred: shape.pred,
            geom_var: w,
            subj_var: shape.subj_var(),
            graph_var: shape.graph_var(),
            q: Arc::new(dist.q),
            k,
            metres_per_unit,
            errors,
            placeholder,
            key: h.finish(),
            order: key.clone(),
            w_first: dist.w_first,
        };
        Ok(Node {
            kind: Kind::SpatialKnn(Box::new(knn)),
            vars: child.vars.clone(),
            certain: child.certain.clone(),
            sorted: Vec::new(),
            est,
            cost,
            dist: dists,
            desc,
            children: vec![t],
        })
    }

    /// The child positions from `n` to the placeholder leaf.
    fn path_to(n: &Node, path: &mut Vec<usize>) -> bool {
        if matches!(n.kind, Kind::Values(_)) && n.desc == PLACEHOLDER {
            return true;
        }
        for (i, c) in n.children.iter().enumerate() {
            path.push(i);
            if path_to(c, path) {
                return true;
            }
            path.pop();
        }
        false
    }
}

#[cfg(all(test, feature = "geo"))]
mod tests {
    use super::super::ctx::{Ctx, Optimizations};
    use super::super::plan::{ActiveGraph, Planner};
    use super::super::{QueryOptions, QueryResult, query};
    use super::*;
    use crate::geo::GeoConfig;
    use crate::io::{RdfFormat, Source};
    use crate::sparql::exec::PlanInfo;
    use crate::store::{Snapshot, Store, StoreOptions};

    const PREFIXES: &str = "PREFIX ex: <http://example.org/> \
        PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
        PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
        PREFIX uom: <http://www.opengis.net/def/uom/OGC/1.0/> ";

    /// A grid of squares and their centre points.
    fn store() -> Store {
        let s = Store::in_memory(StoreOptions::default());
        let mut ttl = String::from(
            "@prefix ex: <http://example.org/> .\n\
             @prefix geo: <http://www.opengis.net/ont/geosparql#> .\n",
        );
        for i in 0..20 {
            for j in 0..10 {
                let (x, y) = (f64::from(i), f64::from(j));
                ttl.push_str(&format!(
                    "ex:s{i}_{j} a ex:Square ; geo:asWKT \"POLYGON(({x} {y}, {} {y}, {} {}, \
                     {x} {}, {x} {y}))\"^^geo:wktLiteral .\n\
                     ex:p{i}_{j} a ex:Point ; geo:asWKT \"POINT({} {})\"^^geo:wktLiteral .\n",
                    x + 1.0,
                    x + 1.0,
                    y + 1.0,
                    y + 1.0,
                    x + 0.5,
                    y + 0.5
                ));
            }
        }
        s.load(&[Source::from_bytes(
            ttl.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        s
    }

    fn opts(on: bool) -> QueryOptions {
        let mut o = Optimizations::ALL;
        o.spatial_join = on;
        o.spatial_knn = on;
        QueryOptions {
            optimizations: Some(o),
            no_cache: true,
            ..Default::default()
        }
    }

    fn run(snap: &Arc<Snapshot>, q: &str, on: bool) -> QueryResult {
        query(snap.clone(), &format!("{PREFIXES}{q}"), &opts(on)).unwrap()
    }

    fn rows(r: &QueryResult) -> Vec<String> {
        let mut v: Vec<String> = r.rows().into_iter().map(|r| format!("{r:?}")).collect();
        v.sort();
        v
    }

    fn find<'a>(p: &'a PlanInfo, op: &str) -> Option<&'a PlanInfo> {
        if p.operator == op {
            return Some(p);
        }
        p.children.iter().find_map(|c| find(c, op))
    }

    fn plan(snap: &Arc<Snapshot>, q: &str) -> (Ctx, Node) {
        let q = spargebra::SparqlParser::new()
            .parse_query(&format!("{PREFIXES}{q}"))
            .unwrap();
        let spargebra::Query::Select { pattern, .. } = q else {
            panic!()
        };
        let ctx = Ctx::new(snap.clone());
        let n = Planner::new(&ctx)
            .plan(&pattern, &ActiveGraph::Default, Vec::new())
            .unwrap();
        (ctx, n)
    }

    fn find_node<'a>(n: &'a Node, op: &str) -> Option<&'a Node> {
        if n.operator() == op {
            return Some(n);
        }
        n.children.iter().find_map(|c| find_node(c, op))
    }

    /// The result-cache key of the plan's `op` node.
    fn key(snap: &Arc<Snapshot>, q: &str, op: &str) -> String {
        let (ctx, n) = plan(snap, q);
        let node = find_node(&n, op).unwrap_or_else(|| panic!("{q}: no {op}"));
        super::super::cache::key(node, &ctx).unwrap().key
    }

    const JOIN: &str = "SELECT ?s ?p { ?s a ex:Square ; geo:asWKT ?ws . ?p a ex:Point ; \
        geo:asWKT ?wp FILTER(geof:sfContains(?ws, ?wp)) }";

    fn nearest(point: &str, k: usize, unit: &str) -> String {
        format!(
            "SELECT ?p ?d {{ ?p geo:asWKT ?w \
             BIND(geof:distance(?w, \"POINT({point})\"^^geo:wktLiteral, {unit}) AS ?d) \
             FILTER(BOUND(?d)) }} ORDER BY ?d LIMIT {k}"
        )
    }

    #[test]
    fn cache_keys_cover_every_constant() {
        let s = store();
        s.enable_geo(GeoConfig::default()).unwrap();
        let snap = s.snapshot();
        let k = |q: &str| key(&snap, q, "SpatialKnn");
        let base = k(&nearest("3 3", 5, "uom:metre"));
        assert_eq!(base, k(&nearest("3 3", 5, "uom:metre")));
        for other in [
            nearest("3 3.5", 5, "uom:metre"),
            nearest("3 3", 6, "uom:metre"),
            nearest("3 3", 5, "uom:kilometre"),
        ] {
            assert_ne!(base, k(&other), "{other}");
        }
        let j = |r: u32| {
            key(
                &snap,
                &format!(
                    "SELECT ?a ?b {{ ?a geo:asWKT ?wa . ?b geo:asWKT ?wb \
                     FILTER(geof:metricDistance(?wa, ?wb) < {r}) }}"
                ),
                "SpatialJoin",
            )
        };
        assert_ne!(j(1000), j(2000));
        assert_eq!(j(1000), j(1000));
        assert_ne!(
            key(&snap, JOIN, "SpatialJoin"),
            key(
                &snap,
                &JOIN.replace("sfContains", "sfIntersects"),
                "SpatialJoin"
            )
        );
    }

    #[test]
    fn a_building_index_keeps_the_answers() {
        let s = store();
        s.pause_geo_build(true);
        s.enable_geo(GeoConfig::default()).unwrap();
        let snap = s.snapshot();
        // the join packs its own trees
        let fast = run(&snap, JOIN, true);
        assert_eq!(rows(&fast), rows(&run(&snap, JOIN, false)));
        assert_eq!(fast.table.len(), 200);
        let j = find(&fast.plan, "SpatialJoin").expect("joined");
        assert!(j.description.ends_with("[tree join]"), "{}", j.description);
        // the nearest-neighbour order waits for the index
        let q = nearest("3.2 3.2", 4, "uom:metre");
        let r = run(&snap, &q, true);
        assert!(find(&r.plan, "SpatialKnn").is_none());
        assert!(
            r.plan
                .warnings
                .iter()
                .any(|w| w.code == "geo-not-knn" && w.message.contains("building")),
            "{:?}",
            r.plan.warnings
        );
        s.pause_geo_build(false);
        s.wait_geo();
        let snap = s.snapshot();
        let fast = run(&snap, &q, true);
        assert!(find(&fast.plan, "SpatialKnn").is_some());
        assert_eq!(rows(&fast), rows(&run(&snap, &q, false)));
        assert_eq!(rows(&fast), rows(&r));
        assert_eq!(run(&snap, JOIN, true).table.len(), 200);
    }
}
