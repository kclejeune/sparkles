//! Query Rewrite: the asserted and derived triples of a topological property.
//!
//! A spatial object resolves to geometry literals: a feature through
//! `geo:hasDefaultGeometry` and the configured serialization predicates, a geometry
//! through the serialization predicates, a literal to itself. `(so1, geo:R, so2)` is
//! derived when some pair of their literals passes `geof:R`; the answer is the set union
//! with the asserted triples. One constant end searches the index (or tests every
//! literal without it), two variable ends join (see [`super::join`]), two constants test.
//!
//! Not implemented yet: the planner never plans a rewrite.

use super::exec::Counters;
use crate::error::{Error, Result};
use crate::sparql::ctx::Ctx;
use crate::sparql::georewrite::SpatialRelateSpec;
use crate::sparql::table::{Table, VarId};

/// Execute a [`SpatialRelateSpec`].
pub fn spatial_relate(
    ctx: &Ctx,
    spec: &SpatialRelateSpec,
    vars: &[VarId],
) -> Result<(Table, Counters)> {
    let _ = (ctx, vars);
    Err(Error::Unsupported(format!(
        "geo:{}: query rewrite is not supported yet",
        spec.rel.local()
    )))
}
