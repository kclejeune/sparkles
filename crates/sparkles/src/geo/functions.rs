//! The `geof:` (and later `spatialF:`) functions in SPARQL expressions.
//!
//! Stub: no function is known yet, so calls fall through to the unknown-function path.

use crate::sparql::ctx::Ctx;
use crate::sparql::expr::{Expr, Row, Val};
use crate::sparql::value::EvalResult;

/// Evaluate the GeoSPARQL function `iri`; `None` when `iri` is not one.
pub fn call(iri: &str, args: &[Expr], row: &Row<'_>, ctx: &Ctx) -> Option<EvalResult<Val>> {
    let _ = (iri, args, row, ctx);
    None
}
