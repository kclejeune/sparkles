//! Distances: geodesic (WGS 84), haversine, or Euclidean in projected CRSs, and the
//! search windows of a radius.
//!
//! Stub: distances are refused; the windows and the lower bound are trivially valid.

use super::{OpError, not_yet};
use crate::geo::DistanceModel;
use crate::geo::geom::Geom;
use crate::geo::units::Unit;

/// Shortest distance in metres between `a` and `b` (0 when they intersect).
pub fn distance_m(a: &Geom, b: &Geom, m: DistanceModel) -> Result<f64, OpError> {
    let _ = (a, b, m);
    Err(not_yet())
}

/// Shortest distance between `a` and `b` in unit `u` (an angle unit gives the central
/// angle on geographic CRSs).
pub fn distance(a: &Geom, b: &Geom, u: &Unit, m: DistanceModel) -> Result<f64, OpError> {
    let _ = (a, b, u, m);
    Err(not_yet())
}

/// A lower bound in metres of the distance between the CRS84 point `p` and anything in
/// the CRS84 box `bbox`, valid for every distance model.
pub fn lower_bound_m(p: [f64; 2], bbox: [f64; 4]) -> f64 {
    let _ = (p, bbox);
    0.0
}

/// CRS84 boxes covering everything within `r_m` metres of the box `bbox84` (split at
/// the antimeridian).
pub fn radius_windows(bbox84: [f64; 4], r_m: f64) -> Vec<[f64; 4]> {
    let _ = (bbox84, r_m);
    vec![[-180.0, -90.0, 180.0, 90.0]]
}
