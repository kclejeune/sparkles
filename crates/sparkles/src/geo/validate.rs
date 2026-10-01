//! Plan-time checks of the GeoSPARQL parts of a query: malformed geometry constants are
//! a `400` (a constant that can never evaluate is reported, not silently false), and a
//! build without the `geo` feature warns about `geof:` calls.
//!
//! Stub: accepts every query.

use crate::error::Result;
use crate::sparql::ctx::PlanWarning;
use spargebra::algebra::GraphPattern;

/// Check the query (or update WHERE clause) `gp`; warnings go to `warn`.
pub fn validate_query(gp: &GraphPattern, warn: &mut dyn FnMut(PlanWarning)) -> Result<()> {
    let _ = (gp, warn);
    Ok(())
}
