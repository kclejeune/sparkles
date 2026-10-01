//! Coordinate reference systems: the built-in table, IRI aliases and transforms between
//! the built-in CRSs. Pure data, compiled without the `geo` feature too.
//!
//! Stub: only the default CRS is known so far.

use std::sync::Arc;

/// The default CRS of WKT literals without a CRS IRI (longitude, latitude on WGS 84).
pub const CRS84_IRI: &str = "http://www.opengis.net/def/crs/OGC/1.3/CRS84";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CrsKind {
    Geographic2D,
    Geographic3D,
    Projected,
}

/// A built-in CRS: an index into the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CrsId(pub u8);

pub const CRS84: CrsId = CrsId(0);

/// The CRS of a geometry: a built-in one, or an IRI this build does not know (valid,
/// but only planar operations between geometries of that same CRS work).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CrsRef {
    Known(CrsId),
    Unknown(Arc<str>),
}

/// The built-in CRS an IRI (or one of its aliases) names.
pub fn lookup(iri: &str) -> Option<CrsId> {
    (iri == CRS84_IRI).then_some(CRS84)
}
