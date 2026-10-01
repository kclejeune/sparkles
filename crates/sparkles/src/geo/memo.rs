//! The per-query geometry memo (a bounded LRU of parsed literals) and the geometry
//! arguments of functions.
//!
//! Stub: remembers nothing, and no argument is a geometry yet.

use super::GeomRef;
use crate::sparql::ctx::Ctx;
use crate::sparql::expr::{Expr, Row};
use crate::sparql::value::{EvalResult, TypeError};

/// Parsed geometries of one query, keyed by term id or by a hash of `(datatype, lexical
/// form)`.
#[derive(Default)]
pub struct GeoMemo {}

/// Argument `i` of a function call as a geometry: the generation's geometry column for
/// stored literals, else the memo (parsing on a miss). Not a geometry: a type error.
#[allow(dead_code)] // the functions call it
pub(crate) fn geom_arg(args: &[Expr], i: usize, row: &Row<'_>, ctx: &Ctx) -> EvalResult<GeomRef> {
    let _ = (args, i, row, ctx);
    Err(TypeError)
}
