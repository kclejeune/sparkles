//! The azimuthal equidistant projection centred on a point, for metric buffers and
//! distances between geometries that are not both points.
//!
//! Two variants: on a sphere (closed forms, for finding closest points, where only the
//! relative positions matter) and on the WGS 84 ellipsoid (through the geodesic inverse
//! and direct problems, for buffers, where distances from the centre must be exact).
//! Planar coordinates are metres east (x) and north (y) of the centre.

use geographiclib_rs::{DirectGeodesic, Geodesic, InverseGeodesic};
use std::sync::OnceLock;

/// The mean Earth radius (IUGG R1 of WGS 84), the radius of the haversine model.
pub const MEAN_RADIUS: f64 = 6_371_008.771_4;

/// The WGS 84 ellipsoid.
pub(crate) fn wgs84() -> &'static Geodesic {
    static G: OnceLock<Geodesic> = OnceLock::new();
    G.get_or_init(Geodesic::wgs84)
}

/// An azimuthal equidistant projection.
#[derive(Clone, Copy, Debug)]
pub struct Aeqd {
    lon0: f64,
    lat0: f64,
    sin0: f64,
    cos0: f64,
    ellipsoid: bool,
}

impl Aeqd {
    /// On the sphere of [`MEAN_RADIUS`], centred on (`lon0`, `lat0`) in degrees.
    pub fn sphere(lon0: f64, lat0: f64) -> Aeqd {
        let (sin0, cos0) = lat0.to_radians().sin_cos();
        Aeqd {
            lon0,
            lat0,
            sin0,
            cos0,
            ellipsoid: false,
        }
    }

    /// On the WGS 84 ellipsoid: distances and azimuths from the centre are geodesic.
    pub fn ellipsoid(lon0: f64, lat0: f64) -> Aeqd {
        Aeqd {
            ellipsoid: true,
            ..Aeqd::sphere(lon0, lat0)
        }
    }

    /// (longitude, latitude) in degrees → (x, y) in metres.
    pub fn forward(&self, lon: f64, lat: f64) -> (f64, f64) {
        if self.ellipsoid {
            let (s12, azi1, _, _): (f64, f64, f64, f64) =
                wgs84().inverse(self.lat0, self.lon0, lat, lon);
            let (sa, ca) = azi1.to_radians().sin_cos();
            return (s12 * sa, s12 * ca);
        }
        let (sin1, cos1) = lat.to_radians().sin_cos();
        let (sdl, cdl) = (lon - self.lon0).to_radians().sin_cos();
        // the central angle, by the haversine formula (accurate near the centre)
        let h = ((lat - self.lat0).to_radians() / 2.0).sin().powi(2)
            + self.cos0 * cos1 * ((lon - self.lon0).to_radians() / 2.0).sin().powi(2);
        let c = 2.0 * h.clamp(0.0, 1.0).sqrt().asin();
        let k = if c < 1e-12 { 1.0 } else { c / c.sin() };
        (
            MEAN_RADIUS * k * cos1 * sdl,
            MEAN_RADIUS * k * (self.cos0 * sin1 - self.sin0 * cos1 * cdl),
        )
    }

    /// (x, y) in metres → (longitude, latitude) in degrees.
    pub fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let rho = x.hypot(y);
        if rho == 0.0 {
            return (self.lon0, self.lat0);
        }
        if self.ellipsoid {
            let azi = x.atan2(y).to_degrees();
            let (lat, lon): (f64, f64) = wgs84().direct(self.lat0, self.lon0, azi, rho);
            return (lon, lat);
        }
        let c = rho / MEAN_RADIUS;
        let (sc, cc) = c.sin_cos();
        let lat = (cc * self.sin0 + y * sc * self.cos0 / rho)
            .clamp(-1.0, 1.0)
            .asin();
        let lon =
            self.lon0.to_radians() + (x * sc).atan2(rho * self.cos0 * cc - y * self.sin0 * sc);
        (lon.to_degrees(), lat.to_degrees())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for (lon0, lat0) in [(0.0, 0.0), (2.35, 48.85), (-120.0, -60.0), (179.5, 10.0)] {
            for p in [Aeqd::sphere(lon0, lat0), Aeqd::ellipsoid(lon0, lat0)] {
                for (dlon, dlat) in [(0.0, 0.0), (1.0, 0.5), (-3.0, 2.0), (0.7, -4.0)] {
                    let (x, y) = p.forward(lon0 + dlon, lat0 + dlat);
                    let (lon, lat) = p.inverse(x, y);
                    let dl = (lon - lon0 - dlon).rem_euclid(360.0);
                    assert!(dl.min(360.0 - dl) < 1e-9, "{p:?} {dlon} {lon}");
                    assert!((lat - lat0 - dlat).abs() < 1e-9, "{p:?} {dlat} {lat}");
                }
            }
        }
    }

    #[test]
    fn distances_from_the_centre() {
        // one degree of longitude on the equator: a·π/180 on the ellipsoid, R·π/180 on
        // the sphere
        let (x, y) = Aeqd::ellipsoid(0.0, 0.0).forward(1.0, 0.0);
        assert!(
            (x - 111_319.490_793_273_57).abs() < 1e-6 && y.abs() < 1e-6,
            "{x} {y}"
        );
        let (x, y) = Aeqd::sphere(0.0, 0.0).forward(1.0, 0.0);
        assert!((x - MEAN_RADIUS.to_radians()).abs() < 1e-6, "{x}");
        assert!(y.abs() < 1e-9);
        // due north on the sphere
        let (x, y) = Aeqd::sphere(10.0, 40.0).forward(10.0, 41.0);
        assert!(x.abs() < 1e-9 && (y - MEAN_RADIUS.to_radians()).abs() < 1e-6);
    }
}
