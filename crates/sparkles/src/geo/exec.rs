//! Execution of the spatial plan operators (`SpatialScan`, `SpatialPf`).
//!
//! Stub: both are refused.

use crate::error::{Error, Result};
use crate::sparql::ctx::Ctx;
use crate::sparql::geopf::{SpatialPfSpec, SpatialScanSpec};
use crate::sparql::table::{Table, VarId};

/// Per-operator explain counters (`candidates`, `refined`, `matched`, …).
pub type Counters = serde_json::Map<String, serde_json::Value>;

fn not_yet() -> Error {
    Error::Unsupported("spatial operators are not supported yet".into())
}

/// A scan of an indexed predicate restricted by spatial filters.
pub fn spatial_scan(
    ctx: &Ctx,
    spec: &SpatialScanSpec,
    vars: &[VarId],
) -> Result<(Table, Counters)> {
    let _ = (ctx, spec, vars);
    Err(not_yet())
}

/// A `spatial:` property function.
pub fn spatial_pf(ctx: &Ctx, spec: &SpatialPfSpec, vars: &[VarId]) -> Result<(Table, Counters)> {
    let _ = (ctx, spec, vars);
    Err(not_yet())
}
