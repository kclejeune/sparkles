//! Parsing `geo:wktLiteral` and `geo:geoJSONLiteral` lexical forms.
//!
//! Stub: every literal is refused.

use super::geom::{Geom, GeomError};

/// Parse a geometry literal of datatype `dt`.
pub fn parse(lex: &str, dt: &str) -> Result<Geom, GeomError> {
    parse_limited(lex, dt, u32::MAX)
}

/// [`parse`], refusing geometries with more than `max_vertices` vertices.
pub fn parse_limited(lex: &str, dt: &str, max_vertices: u32) -> Result<Geom, GeomError> {
    let _ = (lex, dt, max_vertices);
    Err(GeomError {
        offset: None,
        msg: "geometry literals are not supported yet".into(),
    })
}
