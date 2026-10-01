//! k-nearest-neighbour ordering: an indexed scan's rows in increasing lower bound of
//! their distance to a constant, in batches, each joined with the rest of the group
//! (the template plan), until `k` rows whose exact distance is within the next bound
//! exist. Rows whose distance is an error come first, as SPARQL orders them.
//!
//! Not implemented yet: the planner never plans a k-NN search.

use super::exec::Counters;
use crate::error::{Error, Result};
use crate::sparql::ctx::Ctx;
use crate::sparql::exec::PlanInfo;
use crate::sparql::geojoin::SpatialKnnSpec;
use crate::sparql::plan::Node;
use crate::sparql::table::{Table, VarId};

/// Execute a [`SpatialKnnSpec`] with its template plan; returns the rows (a superset of
/// the first `k` in the ordering), the counters and the template's runs for EXPLAIN.
pub fn spatial_knn(
    ctx: &Ctx,
    spec: &SpatialKnnSpec,
    template: &Node,
    vars: &[VarId],
) -> Result<(Table, Counters, Vec<PlanInfo>)> {
    let _ = (ctx, spec, template, vars);
    Err(Error::Unsupported(
        "nearest-neighbour search is not supported yet".into(),
    ))
}
