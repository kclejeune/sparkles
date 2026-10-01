//! Units of measure accepted by the `geof:` functions (OGC, QUDT and EPSG IRIs). Pure
//! data, compiled without the `geo` feature too.
//!
//! Stub: no units are known yet.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnitKind {
    Length,
    Angle,
    Area,
}

/// A unit: its kind and its size in the base unit of the kind (metre, radian, square
/// metre).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Unit {
    pub kind: UnitKind,
    pub factor: f64,
}

/// The unit an IRI names.
pub fn unit(iri: &str) -> Option<Unit> {
    let _ = iri;
    None
}
