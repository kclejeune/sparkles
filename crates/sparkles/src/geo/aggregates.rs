//! The GeoSPARQL aggregates: `geof:aggBoundingBox`, `aggBoundingCircle`, `aggCentroid`,
//! `aggConvexHull`, `aggUnion` and `aggConcaveHull` over one geometry expression. An
//! ill-typed input makes the aggregate an error (unbound), as for the numeric
//! aggregates; results are in the CRS of the first input.
//!
//! Not implemented yet: the parser does not read these IRIs as aggregates.

use crate::id::Id;
use crate::sparql::ctx::Ctx;

/// The value of the aggregate `geof:<local>` over one group's evaluated values (after
/// DISTINCT; `Err` for a row whose expression was an error); `None` when `local` is not
/// a GeoSPARQL aggregate.
pub fn evaluate(ctx: &Ctx, local: &str, vals: &[std::result::Result<Id, ()>]) -> Option<Id> {
    let _ = (ctx, local, vals);
    None
}
