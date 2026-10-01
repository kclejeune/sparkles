//! Topological relations: the 24 GeoSPARQL relations and DE-9IM pattern matching.
//!
//! Stub: every test fails with a type error.

use super::{OpError, not_yet};
use crate::geo::geom::Geom;
use crate::geo::{GeomRef, Relation};

/// Whether `r(a, b)` holds (`b` is transformed into `a`'s CRS).
pub fn relation(a: &Geom, b: &Geom, r: Relation) -> Result<bool, OpError> {
    let _ = (a, b, r);
    Err(not_yet())
}

/// Whether the DE-9IM matrix of `(a, b)` matches `pattern` (9 characters of `TF*012`).
pub fn relate(a: &Geom, b: &Geom, pattern: &str) -> Result<bool, OpError> {
    let _ = (a, b, pattern);
    Err(not_yet())
}

/// Whether every match of `pattern` needs the geometries to intersect (one of the
/// II, IB, BI, BB entries is `T`, `0`, `1` or `2`), so the index can find candidates.
pub fn pattern_needs_intersection(pattern: &str) -> Result<bool, OpError> {
    let _ = pattern;
    Err(not_yet())
}

/// A geometry prepared for many tests against other geometries.
pub struct Prepared {
    g: GeomRef,
}

impl Prepared {
    pub fn new(g: GeomRef) -> Prepared {
        Prepared { g }
    }

    pub fn geom(&self) -> &GeomRef {
        &self.g
    }

    /// `r(self, b)`.
    pub fn relation(&self, b: &Geom, r: Relation) -> Result<bool, OpError> {
        relation(&self.g, b, r)
    }

    /// The DE-9IM matrix of `(self, b)` against `pattern`.
    pub fn relate(&self, b: &Geom, pattern: &str) -> Result<bool, OpError> {
        relate(&self.g, b, pattern)
    }
}
