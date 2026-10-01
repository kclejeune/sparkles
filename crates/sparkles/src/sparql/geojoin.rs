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
//!   filter.
//! * `ORDER BY ASC(geof:distance(?w, C, u)) LIMIT k` (or a variable bound to it) over a
//!   group whose geometries come from one indexed scan becomes a [`SpatialKnnSpec`] below
//!   the top-k: candidates in increasing distance, in batches, until `k` rows are proven.
//!
//! Neither is planned yet: [`spatial_joins`] and [`spatial_knn`] leave their input as it
//! is, so these plans are unreachable.

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
    /// conjunct is not among them)
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
}

/// Join components of one group that a spatial conjunct of `filters` connects (the
/// conjunct moves into the join).
pub(super) fn spatial_joins(parts: &mut Vec<Node>, filters: &mut Vec<Expr>, ctx: &Ctx) {
    let _ = (parts, filters, ctx);
}

/// An `ORDER BY … LIMIT k` node as a k-nearest-neighbour search when its shape allows,
/// else `n` unchanged.
pub(super) fn spatial_knn(n: Node, ctx: &Ctx) -> Node {
    let _ = ctx;
    n
}
