//! Writing geometries in the canonical WKT and GeoJSON forms.
//!
//! Stub: writes nothing yet.

use super::geom::Geom;

/// The canonical WKT form (with the CRS IRI unless it is CRS84).
pub fn to_wkt(g: &Geom) -> String {
    let _ = g;
    String::new()
}

/// GeoJSON (always CRS84).
pub fn to_geojson(g: &Geom) -> String {
    let _ = g;
    String::new()
}

/// A literal of datatype `dt` (`geo:wktLiteral` or `geo:geoJSONLiteral`).
pub fn literal(g: &Geom, dt: &str) -> oxrdf::Literal {
    let lex = if dt == super::vocab::GEOJSON_LITERAL {
        to_geojson(g)
    } else {
        to_wkt(g)
    };
    oxrdf::Literal::new_typed_literal(lex, oxrdf::NamedNode::new_unchecked(dt))
}
