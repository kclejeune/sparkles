//! Spatial joins: pairs of geometries from two inputs that pass a spatial test.
//!
//! Each input is an indexed scan, probed in the spatial index, or a table whose distinct
//! geometries are boxed and packed into a tree for the query. Candidate pairs come from
//! the trees (a probe per outer box, or a synchronous traversal of both), and every pair
//! is tested exactly with the relation code the `geof:` functions use.
//!
//! Not implemented yet: the planner never plans a join.

use super::GeomRef;
use super::exec::Counters;
use super::search::SearchStats;
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::Ctx;
use crate::sparql::geojoin::{JoinTest, SpatialJoinSpec};
use crate::sparql::table::{Table, VarId};

/// A geometry of one join input: where it came from and its geometry.
#[derive(Clone)]
pub struct JoinItem {
    /// the input's row (a table row, or a candidate of an index side)
    pub row: u32,
    /// the literal's id
    pub id: Id,
    /// CRS84 envelope (internal lon/lat)
    pub bbox84: [f64; 4],
    pub geom: GeomRef,
}

/// A pair of input rows `(left.row, right.row)`.
pub type Pair = (u32, u32);

/// The pairs `(left.row, right.row)` whose geometries pass `test` (left first), in
/// chunks; `ctx`'s budgets bound the candidates.
pub fn pairs(
    ctx: &Ctx,
    left: &[JoinItem],
    right: &[JoinItem],
    test: &JoinTest,
    st: &mut SearchStats,
    sink: &mut dyn FnMut(&[Pair]) -> Result<()>,
) -> Result<()> {
    let _ = (ctx, left, right, test, st, sink);
    Err(not_yet())
}

/// Execute a [`SpatialJoinSpec`] over the tables of the node's children (`inputs`, in
/// child order).
pub fn spatial_join(
    ctx: &Ctx,
    spec: &SpatialJoinSpec,
    inputs: Vec<Table>,
    vars: &[VarId],
) -> Result<(Table, Counters)> {
    let _ = (ctx, spec, inputs, vars);
    Err(not_yet())
}

fn not_yet() -> Error {
    Error::Unsupported("spatial joins are not supported yet".into())
}
