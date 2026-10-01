//! Jena's filter functions (`spatialF:`, `http://jena.apache.org/function/spatial#`):
//! `convertLatLon`, `convertLatLonBox`, `equals`, `nearby`, `withinCircle`, `distance`,
//! `greatCircle`, `greatCircleGeom`, `angle`, `angleDeg`, `azimuth`, `azimuthDeg`,
//! `transform`, `transformDatatype` and `transformSRS`, with Jena's argument forms (a
//! unit may be an IRI, an `xsd:anyURI` literal or a string).
//!
//! Not implemented yet: every `spatialF:` call is an unknown function (a type error).

use crate::sparql::ctx::Ctx;
use crate::sparql::expr::{Expr, Row, Val};
use crate::sparql::value::EvalResult;

/// Evaluate the Jena filter function `iri`; `None` when `iri` is not one.
pub fn call(iri: &str, args: &[Expr], row: &Row<'_>, ctx: &Ctx) -> Option<EvalResult<Val>> {
    let _ = (iri, args, row, ctx);
    None
}
