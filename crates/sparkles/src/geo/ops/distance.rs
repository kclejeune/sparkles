//! Distances: geodesic (WGS 84), haversine, or Euclidean in projected CRSs, and the
//! search windows of a radius.
//!
//! On geographic CRSs the geometries' edges are straight in longitude/latitude (the
//! standard's computation model, as for the relations). The geodesic model measures
//! point to point with Karney's inverse solution; other pairs are 0 when they
//! intersect, else their closest points are found in an azimuthal equidistant
//! projection centred on the gap between their envelopes, refined along the edge, and
//! measured with the geodesic. The result is always the geodesic length between two
//! points of the geometries, so it never underestimates. The haversine model finds the
//! closest points planarly in degrees and measures them on a sphere of the mean Earth
//! radius (Jena's model).

use super::aeqd::{Aeqd, MEAN_RADIUS, wgs84};
use super::{OpError, guarded, in_crs, is_geographic, type_error};
use crate::geo::DistanceModel;
use crate::geo::geom::Geom;
use crate::geo::units::{Unit, UnitKind};
use geo_index::rtree::sort::HilbertSort;
use geo_index::rtree::{NeighborsOptions, RTreeBuilder, RTreeIndex};
use geographiclib_rs::InverseGeodesic;
use georust::{BoundingRect, Coord, Geometry, Intersects, Relate};

/// The radius of the sphere on which great-circle distances never exceed WGS 84
/// geodesic distances between the same coordinates: `a(1 − e²)`, the smallest
/// meridional radius of curvature (at the equator).
pub const LOWER_BOUND_RADIUS: f64 = 6_335_439.0;

/// Shortest distance in metres between `a` and `b` (0 when they intersect).
pub fn distance_m(a: &Geom, b: &Geom, m: DistanceModel) -> Result<f64, OpError> {
    let b = pair(a, b)?;
    let b = &*b;
    match is_geographic(&a.crs) {
        None => Err(unknown_crs()),
        Some(false) => euclidean(a, b),
        Some(true) => Ok(match closest(a, b, m)? {
            None => 0.0,
            Some(c) => c.metres,
        }),
    }
}

/// Shortest distance between `a` and `b` in unit `u` (an angle unit gives the central
/// angle on geographic CRSs).
pub fn distance(a: &Geom, b: &Geom, u: &Unit, m: DistanceModel) -> Result<f64, OpError> {
    match u.kind {
        UnitKind::Length => Ok(distance_m(a, b, m)? / u.factor),
        UnitKind::Angle => {
            let b = pair(a, b)?;
            match is_geographic(&a.crs) {
                Some(true) => {}
                Some(false) => {
                    return Err(type_error("distance: an angle unit needs a geographic CRS"));
                }
                None => return Err(unknown_crs()),
            }
            Ok(match closest(a, &b, m)? {
                None => 0.0,
                Some(c) => central_angle(c.p, c.q) / u.factor,
            })
        }
        UnitKind::Area => Err(type_error("distance: an area unit is not a length")),
    }
}

/// A lower bound in metres of the distance between the CRS84 point `p` and anything in
/// the CRS84 box `bbox`, valid for every distance model: the great-circle distance on
/// the sphere of [`LOWER_BOUND_RADIUS`] to the nearest point of the box.
pub fn lower_bound_m(p: [f64; 2], bbox: [f64; 4]) -> f64 {
    let [lon, lat] = p;
    let [x0, y0, x1, y1] = bbox;
    let (y0, y1) = (y0.max(-90.0), y1.min(90.0));
    if !(x0 <= x1 && y0 <= y1) {
        return 0.0;
    }
    let lat = lat.clamp(-90.0, 90.0);
    // within the box's longitudes (in any of their 360° representations): along the
    // meridian
    if x1 - x0 >= 360.0
        || [lon, lon - 360.0, lon + 360.0]
            .iter()
            .any(|&l| x0 <= l && l <= x1)
    {
        return (lat - lat.clamp(y0, y1)).abs().to_radians() * LOWER_BOUND_RADIUS;
    }
    // else the nearest point lies on one of the two bounding meridians: the foot of the
    // perpendicular when it falls within the edge, else an end
    let edge = |x: f64| -> f64 {
        let dl = (x - lon).to_radians();
        let (sp, cp) = lat.to_radians().sin_cos();
        let foot = sp.atan2(cp * dl.cos()).to_degrees();
        [foot.clamp(y0, y1), y0, y1]
            .into_iter()
            .map(|y| central_angle(Coord { x: lon, y: lat }, Coord { x, y }))
            .fold(f64::INFINITY, f64::min)
    };
    edge(x0).min(edge(x1)) * LOWER_BOUND_RADIUS
}

/// CRS84 boxes covering everything within `r_m` metres of the box `bbox84` (on the
/// sphere of [`LOWER_BOUND_RADIUS`], so every distance model's matches are inside):
/// wider in longitude at high latitudes, the full longitude range when a pole is
/// within reach, and split at the antimeridian.
pub fn radius_windows(bbox84: [f64; 4], r_m: f64) -> Vec<[f64; 4]> {
    let [x0, y0, x1, y1] = bbox84;
    let d = (r_m.max(0.0) / LOWER_BOUND_RADIUS).to_degrees();
    if !d.is_finite() {
        return vec![[-180.0, -90.0, 180.0, 90.0]];
    }
    let (lat0, lat1) = (y0 - d, y1 + d);
    if lat0 <= -90.0 || lat1 >= 90.0 {
        return vec![[
            x0.min(-180.0),
            lat0.max(-90.0),
            x1.max(180.0),
            lat1.min(90.0),
        ]];
    }
    // the widest longitude reach of a cap of radius d around a point of the box: at
    // the box's latitude farthest from the equator
    let far = y0.abs().max(y1.abs()).to_radians();
    let (sd, cf) = (d.to_radians().sin(), far.cos());
    if sd >= cf {
        return vec![[x0.min(-180.0), lat0, x1.max(180.0), lat1]];
    }
    let dl = (sd / cf).asin().to_degrees();
    let (lon0, lon1) = (x0 - dl, x1 + dl);
    if lon1 - lon0 >= 360.0 {
        return vec![[x0.min(-180.0), lat0, x1.max(180.0), lat1]];
    }
    // the part beyond ±180 is clipped (unless the box itself extends there) and also
    // searched where it wraps to
    let mut out = vec![[
        lon0.max(x0.min(-180.0)),
        lat0,
        lon1.min(x1.max(180.0)),
        lat1,
    ]];
    if lon0 < -180.0 {
        out.push([lon0 + 360.0, lat0, 180.0, lat1]);
    }
    if lon1 > 180.0 {
        out.push([-180.0, lat0, lon1 - 360.0, lat1]);
    }
    out
}

fn unknown_crs() -> OpError {
    type_error("distance: the CRS has no known unit")
}

/// `b` in `a`'s CRS, both not empty.
fn pair<'a>(a: &Geom, b: &'a Geom) -> Result<std::borrow::Cow<'a, Geom>, OpError> {
    if a.empty || b.empty {
        return Err(type_error(
            "distance: an empty geometry has no closest point",
        ));
    }
    in_crs(b, &a.crs)
}

/// Two closest points (longitude/latitude) of disjoint geometries, and their distance.
#[derive(Clone, Copy, Debug)]
struct Closest {
    p: Coord<f64>,
    q: Coord<f64>,
    metres: f64,
}

/// The closest points of `a` and `b` under the model; `None` when they intersect.
fn closest(a: &Geom, b: &Geom, m: DistanceModel) -> Result<Option<Closest>, OpError> {
    if let (Geometry::Point(p), Geometry::Point(q)) = (&a.g, &b.g) {
        let (p, q) = (p.0, q.0);
        return Ok(Some(Closest {
            p,
            q,
            metres: measure(m, p, q),
        }));
    }
    if intersects(a, b)? {
        return Ok(None);
    }
    let (sa, sb) = (Shape::of(&a.g), Shape::of(&b.g));
    let (Some(ra), Some(rb)) = (a.g.bounding_rect(), b.g.bounding_rect()) else {
        return Ok(None);
    };
    // b in the 360° representation nearest to a
    let shift = {
        let (ca, cb) = (ra.center().x, rb.center().x);
        if cb - ca > 180.0 {
            -360.0
        } else if ca - cb > 180.0 {
            360.0
        } else {
            0.0
        }
    };
    match m {
        DistanceModel::Haversine => {
            let pb: Vec<Coord<f64>> = sb
                .pts
                .iter()
                .map(|c| Coord {
                    x: c.x + shift,
                    y: c.y,
                })
                .collect();
            let best = nearest_pairs(&sa, &sa.pts, &sb, &pb, 1);
            Ok(best.first().map(|c| {
                let (p, q) = c.points(&sa, &sb, c.t);
                Closest {
                    p,
                    q,
                    metres: haversine(p, q),
                }
            }))
        }
        DistanceModel::Geodesic => {
            // centre of the gap between the envelopes (the middle of their overlap
            // on an axis where they overlap)
            let mid = |a0: f64, a1: f64, b0: f64, b1: f64| {
                let (lo, hi) = (a0.max(b0), a1.min(b1));
                (lo + hi) / 2.0
            };
            let lon0 = mid(
                ra.min().x,
                ra.max().x,
                rb.min().x + shift,
                rb.max().x + shift,
            );
            let lat0 = mid(ra.min().y, ra.max().y, rb.min().y, rb.max().y).clamp(-90.0, 90.0);
            let proj = Aeqd::sphere(lon0, lat0);
            let fwd = |s: &Shape| -> Vec<Coord<f64>> {
                s.pts
                    .iter()
                    .map(|c| {
                        let (x, y) = proj.forward(c.x, c.y);
                        Coord { x, y }
                    })
                    .collect()
            };
            let (pa, pb) = (fwd(&sa), fwd(&sb));
            let mut best: Option<Closest> = None;
            for c in nearest_pairs(&sa, &pa, &sb, &pb, 3) {
                let found = c.refine(&sa, &sb);
                if best.is_none_or(|b| found.metres < b.metres) {
                    best = Some(found);
                }
            }
            Ok(best)
        }
    }
}

/// Euclidean distance in CRS units (metres for the projected CRSs).
fn euclidean(a: &Geom, b: &Geom) -> Result<f64, OpError> {
    if intersects(a, b)? {
        return Ok(0.0);
    }
    let (sa, sb) = (Shape::of(&a.g), Shape::of(&b.g));
    Ok(nearest_pairs(&sa, &sa.pts, &sb, &sb.pts, 1)
        .first()
        .map_or(0.0, |c| c.d2.sqrt()))
}

fn intersects(a: &Geom, b: &Geom) -> Result<bool, OpError> {
    // the relate graph is O((n + m) log(n + m)); the direct test can be O(n·m)
    if u64::from(a.vertices) * u64::from(b.vertices) > 1 << 20 {
        guarded("distance", || a.g.relate(&b.g).is_intersects())
    } else {
        guarded("distance", || a.g.intersects(&b.g))
    }
}

fn measure(m: DistanceModel, p: Coord<f64>, q: Coord<f64>) -> f64 {
    match m {
        DistanceModel::Geodesic => geodesic(p, q),
        DistanceModel::Haversine => haversine(p, q),
    }
}

/// WGS 84 geodesic distance in metres between two (longitude, latitude) points.
pub(crate) fn geodesic(p: Coord<f64>, q: Coord<f64>) -> f64 {
    wgs84().inverse(p.y, p.x, q.y, q.x)
}

/// Great-circle distance on the sphere of the mean Earth radius.
pub(crate) fn haversine(p: Coord<f64>, q: Coord<f64>) -> f64 {
    central_angle(p, q) * MEAN_RADIUS
}

/// The central angle in radians between two (longitude, latitude) points (haversine
/// formula).
pub(crate) fn central_angle(p: Coord<f64>, q: Coord<f64>) -> f64 {
    let (lat1, lat2) = (p.y.to_radians(), q.y.to_radians());
    let h = ((lat2 - lat1) / 2.0).sin().powi(2)
        + lat1.cos() * lat2.cos() * ((q.x - p.x).to_radians() / 2.0).sin().powi(2);
    2.0 * h.clamp(0.0, 1.0).sqrt().asin()
}

/// A geometry as vertices and segments between them (a point is a segment from a
/// vertex to itself; polygon rings are their edges).
struct Shape {
    pts: Vec<Coord<f64>>,
    segs: Vec<[u32; 2]>,
}

impl Shape {
    fn of(g: &Geometry<f64>) -> Shape {
        let mut s = Shape {
            pts: Vec::new(),
            segs: Vec::new(),
        };
        s.add(g);
        s
    }

    fn add(&mut self, g: &Geometry<f64>) {
        use Geometry as G;
        match g {
            G::Point(p) => self.path(std::iter::once(p.0)),
            G::Line(l) => self.path([l.start, l.end].into_iter()),
            G::LineString(l) => self.path(l.0.iter().copied()),
            G::Polygon(p) => {
                self.path(p.exterior().0.iter().copied());
                for r in p.interiors() {
                    self.path(r.0.iter().copied());
                }
            }
            G::MultiPoint(m) => m.0.iter().for_each(|p| self.path(std::iter::once(p.0))),
            G::MultiLineString(m) => m.0.iter().for_each(|l| self.path(l.0.iter().copied())),
            G::MultiPolygon(m) => m.0.iter().for_each(|p| self.add(&G::Polygon(p.clone()))),
            G::GeometryCollection(c) => c.0.iter().for_each(|m| self.add(m)),
            G::Rect(r) => self.add(&G::Polygon(r.to_polygon())),
            G::Triangle(t) => self.add(&G::Polygon(t.to_polygon())),
        }
    }

    /// A polyline (a single vertex: a point).
    fn path(&mut self, cs: impl Iterator<Item = Coord<f64>>) {
        let first = self.pts.len() as u32;
        self.pts.extend(cs);
        let n = self.pts.len() as u32 - first;
        match n {
            0 => {}
            1 => self.segs.push([first, first]),
            _ => self.segs.extend((first..first + n - 1).map(|i| [i, i + 1])),
        }
    }
}

/// A vertex of one geometry and the nearest point of a segment of the other, in the
/// plane the search ran in.
#[derive(Clone, Copy, Debug)]
struct Cand {
    d2: f64,
    /// the vertex belongs to the first geometry
    a_vertex: bool,
    v: u32,
    seg: u32,
    t: f64,
}

impl Cand {
    /// The two points in the geometries' own coordinates: the vertex and the point at
    /// `t` along the segment, first geometry first.
    fn points(&self, a: &Shape, b: &Shape, t: f64) -> (Coord<f64>, Coord<f64>) {
        let (vs, ss) = if self.a_vertex { (a, b) } else { (b, a) };
        let v = vs.pts[self.v as usize];
        let [i, j] = ss.segs[self.seg as usize];
        let (s0, s1) = (ss.pts[i as usize], ss.pts[j as usize]);
        let on = Coord {
            x: s0.x + (s1.x - s0.x) * t,
            y: s0.y + (s1.y - s0.y) * t,
        };
        if self.a_vertex { (v, on) } else { (on, v) }
    }

    /// The geodesic distance from the vertex to the segment near `t`: a golden-section
    /// search of the parameter in a bracket around the projection's answer.
    fn refine(&self, a: &Shape, b: &Shape) -> Closest {
        let f = |t: f64| {
            let (p, q) = self.points(a, b, t);
            (geodesic(p, q), p, q)
        };
        let mut best = f(self.t);
        let [i, j] = if self.a_vertex { b } else { a }.segs[self.seg as usize];
        if i != j {
            const PHI: f64 = 0.618_033_988_749_894_8;
            let (mut lo, mut hi) = ((self.t - 0.05).max(0.0), (self.t + 0.05).min(1.0));
            let mut x1 = hi - PHI * (hi - lo);
            let mut x2 = lo + PHI * (hi - lo);
            let (mut f1, mut f2) = (f(x1), f(x2));
            for _ in 0..30 {
                if f1.0 <= f2.0 {
                    hi = x2;
                    (x2, f2) = (x1, f1);
                    x1 = hi - PHI * (hi - lo);
                    f1 = f(x1);
                } else {
                    lo = x1;
                    (x1, f1) = (x2, f2);
                    x2 = lo + PHI * (hi - lo);
                    f2 = f(x2);
                }
                if hi - lo < 1e-9 {
                    break;
                }
            }
            for c in [f1, f2] {
                if c.0 < best.0 {
                    best = c;
                }
            }
        }
        Closest {
            p: best.1,
            q: best.2,
            metres: best.0,
        }
    }
}

/// Squared distance from `p` to the segment `s0 s1`, and the parameter of the nearest
/// point.
fn point_segment(p: Coord<f64>, s0: Coord<f64>, s1: Coord<f64>) -> (f64, f64) {
    let (dx, dy) = (s1.x - s0.x, s1.y - s0.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.x - s0.x) * dx + (p.y - s0.y) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (ex, ey) = (s0.x + dx * t - p.x, s0.y + dy * t - p.y);
    (ex * ex + ey * ey, t)
}

/// The `keep` closest (vertex, segment) pairs between two shapes whose vertices are at
/// `pa` and `pb` in a plane. Two disjoint segments are closest at a vertex of one of
/// them, so the vertices of each against the segments of the other cover every pair.
fn nearest_pairs(
    a: &Shape,
    pa: &[Coord<f64>],
    b: &Shape,
    pb: &[Coord<f64>],
    keep: usize,
) -> Vec<Cand> {
    let mut out = Vec::with_capacity(pa.len() + pb.len());
    nearest_to(pa, b, pb, true, &mut out);
    nearest_to(pb, a, pa, false, &mut out);
    out.sort_by(|x, y| x.d2.total_cmp(&y.d2));
    out.truncate(keep);
    out
}

/// For each vertex at `vs`, the nearest segment of `s` (vertices at `ps`).
fn nearest_to(
    vs: &[Coord<f64>],
    s: &Shape,
    ps: &[Coord<f64>],
    a_vertex: bool,
    out: &mut Vec<Cand>,
) {
    let seg = |k: usize| {
        let [i, j] = s.segs[k];
        (ps[i as usize], ps[j as usize])
    };
    if s.segs.is_empty() {
        return;
    }
    if vs.len().saturating_mul(s.segs.len()) <= 4096 || s.segs.len() < 16 {
        for (v, &p) in vs.iter().enumerate() {
            let mut best = (f64::INFINITY, 0, 0.0);
            for k in 0..s.segs.len() {
                let (s0, s1) = seg(k);
                let (d2, t) = point_segment(p, s0, s1);
                if d2 < best.0 {
                    best = (d2, k, t);
                }
            }
            out.push(Cand {
                d2: best.0,
                a_vertex,
                v: v as u32,
                seg: best.1 as u32,
                t: best.2,
            });
        }
        return;
    }
    let mut builder = RTreeBuilder::<f64>::new(s.segs.len() as u32);
    for k in 0..s.segs.len() {
        let (s0, s1) = seg(k);
        builder.add(
            s0.x.min(s1.x),
            s0.y.min(s1.y),
            s0.x.max(s1.x),
            s0.y.max(s1.y),
        );
    }
    let tree = builder.finish::<HilbertSort>();
    for (v, &p) in vs.iter().enumerate() {
        let box_d2 = |[x0, y0, x1, y1]: [f64; 4]| {
            let dx = (x0 - p.x).max(0.0).max(p.x - x1);
            let dy = (y0 - p.y).max(0.0).max(p.y - y1);
            dx * dx + dy * dy
        };
        let found = tree.neighbors_with_callbacks(NeighborsOptions::new(), box_d2, |k, _| {
            let (s0, s1) = seg(k as usize);
            Some(point_segment(p, s0, s1).0)
        });
        if let Some(&(k, d2)) = found.first() {
            let (s0, s1) = seg(k as usize);
            let (_, t) = point_segment(p, s0, s1);
            out.push(Cand {
                d2,
                a_vertex,
                v: v as u32,
                seg: k,
                t,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::{RngExt, SeedableRng};

    fn g(s: &str) -> Geom {
        crate::geo::ops::wkt(s)
    }

    fn metres(a: &str, b: &str, m: DistanceModel) -> f64 {
        distance_m(&g(a), &g(b), m).unwrap()
    }

    // a·π/180: one degree of longitude on the equator of WGS 84
    const EQUATOR_DEGREE: f64 = 111_319.490_793_273_57;

    #[test]
    fn point_to_point() {
        let d = metres("POINT(0 0)", "POINT(1 0)", DistanceModel::Geodesic);
        assert!((d - EQUATOR_DEGREE).abs() < 1e-6, "{d}");
        let h = metres("POINT(0 0)", "POINT(1 0)", DistanceModel::Haversine);
        assert!((h - 111_195.079_734_368_7).abs() < 1e-6, "{h}");
        // GeographicLib's published test cases: Wellington to Salamanca (the classic
        // failure of Vincenty's method) and a nearly antipodal pair
        let d = metres(
            "POINT(174.816666666666666 -41.3166666666666667)",
            "POINT(-5.5 40.9666666666666667)",
            DistanceModel::Geodesic,
        );
        assert!((d - 19_960_543.857_179).abs() < 1e-5, "{d}");
        let d = metres(
            "POINT(0 27.2)",
            "POINT(179.5 -27.1)",
            DistanceModel::Geodesic,
        );
        assert!((d - 19_974_354.765_767).abs() < 1e-5, "{d}");
        assert_eq!(
            metres("POINT(3 4)", "POINT(3 4)", DistanceModel::Geodesic),
            0.0
        );
    }

    #[test]
    fn units_and_angles() {
        let unit = |kind, factor| Unit { kind, factor };
        let (a, b) = (g("POINT(0 0)"), g("POINT(1 0)"));
        let km = distance(
            &a,
            &b,
            &unit(UnitKind::Length, 1000.0),
            DistanceModel::Geodesic,
        );
        assert!((km.unwrap() - EQUATOR_DEGREE / 1000.0).abs() < 1e-9);
        let deg = unit(UnitKind::Angle, std::f64::consts::PI / 180.0);
        for m in [DistanceModel::Geodesic, DistanceModel::Haversine] {
            let d = distance(&a, &b, &deg, m).unwrap();
            assert!((d - 1.0).abs() < 1e-9, "{d}");
        }
        assert!(distance(&a, &b, &unit(UnitKind::Area, 1.0), DistanceModel::Geodesic).is_err());
    }

    #[test]
    fn errors() {
        let mars = g("<http://example.org/crs/mars> POINT(1 1)");
        assert!(distance_m(&mars, &mars, DistanceModel::Geodesic).is_err());
        assert!(distance_m(&mars, &g("POINT(1 1)"), DistanceModel::Geodesic).is_err());
        assert!(distance_m(&g("POINT(1 1)"), &mars, DistanceModel::Geodesic).is_err());
        assert!(distance_m(&g("POINT EMPTY"), &g("POINT(1 1)"), DistanceModel::Geodesic).is_err());
        assert!(
            distance_m(
                &g("POINT(1 1)"),
                &g("POLYGON EMPTY"),
                DistanceModel::Geodesic
            )
            .is_err()
        );
    }

    const GA: &str = "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))";

    #[test]
    fn geometries() {
        for m in [DistanceModel::Geodesic, DistanceModel::Haversine] {
            assert_eq!(metres(GA, "POINT(2 2)", m), 0.0);
            assert_eq!(metres(GA, "POINT(10 5)", m), 0.0);
            assert_eq!(metres(GA, "LINESTRING(-5 5, 5 5)", m), 0.0);
            assert_eq!(metres(GA, "POLYGON((5 5, 15 5, 15 15, 5 5))", m), 0.0);
        }
        // from (12 5) to the edge at longitude 10: about the geodesic to (10 5), a
        // little shorter (the perpendicular foot is not on the parallel)
        let want = geodesic(Coord { x: 10.0, y: 5.0 }, Coord { x: 12.0, y: 5.0 });
        let d = metres(GA, "POINT(12 5)", DistanceModel::Geodesic);
        assert!(d <= want && d >= want * 0.9999, "{d} {want}");
        assert!((d - 221_800.0).abs() / 221_800.0 < 0.001, "{d}");
        // closest points of the spec's examples: B (5 5) ≈ 471 km and C (10 2) ≈ 890 km
        // from POINT(2 2), measured to the nearest corner/edge
        let b = metres(
            "POINT(2 2)",
            "POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))",
            DistanceModel::Geodesic,
        );
        let to_corner = geodesic(Coord { x: 2.0, y: 2.0 }, Coord { x: 5.0, y: 5.0 });
        assert!(
            b <= to_corner + 1e-6 && b > to_corner * 0.999,
            "{b} {to_corner}"
        );
        assert!((b - 471_000.0).abs() < 1_000.0, "{b}");
        let c = metres(
            "POINT(2 2)",
            "POLYGON((10 0, 20 0, 20 10, 10 10, 10 0))",
            DistanceModel::Geodesic,
        );
        assert!((c - 890_000.0).abs() < 1_500.0, "{c}");
        // across the antimeridian
        let d = metres(
            "LINESTRING(179 0, 179 10)",
            "POINT(-179 5)",
            DistanceModel::Geodesic,
        );
        let want = geodesic(Coord { x: 179.0, y: 5.0 }, Coord { x: -179.0, y: 5.0 });
        assert!(d <= want && d >= want * 0.9999, "{d} {want}");
        let h = metres(
            "LINESTRING(179 0, 179 10)",
            "POINT(-179 5)",
            DistanceModel::Haversine,
        );
        assert!(
            (h - haversine(Coord { x: 179.0, y: 5.0 }, Coord { x: -179.0, y: 5.0 })).abs() < 1e-6
        );
    }

    #[test]
    fn large_shapes_use_the_tree() {
        // 2,000-vertex polylines, so both sides search an R-tree
        let line = |x0: f64, y: f64| {
            let pts: Vec<String> = (0..2000)
                .map(|i| format!("{} {}", x0 + f64::from(i) * 0.001, y))
                .collect();
            format!("LINESTRING({})", pts.join(", "))
        };
        let (a, b) = (line(0.0, 0.0), line(0.5, 0.5));
        let d = metres(&a, &b, DistanceModel::Geodesic);
        let want = geodesic(Coord { x: 0.5, y: 0.0 }, Coord { x: 0.5, y: 0.5 });
        assert!(d >= want - 1e-6 && d <= want * 1.0001, "{d} {want}");
    }

    /// A brute-force reference: both geometries densified along their edges (straight
    /// in longitude/latitude), each sample against the other's edges by a fine 1-D
    /// search.
    fn brute(a: &[Coord<f64>], b: &[Coord<f64>]) -> f64 {
        let dense = |l: &[Coord<f64>]| -> Vec<Coord<f64>> {
            let mut out = vec![l[0]];
            for w in l.windows(2) {
                for k in 1..=40 {
                    let t = f64::from(k) / 40.0;
                    out.push(Coord {
                        x: w[0].x + (w[1].x - w[0].x) * t,
                        y: w[0].y + (w[1].y - w[0].y) * t,
                    });
                }
            }
            out
        };
        let edge_min = |p: Coord<f64>, s0: Coord<f64>, s1: Coord<f64>| {
            let at = |t: f64| {
                geodesic(
                    p,
                    Coord {
                        x: s0.x + (s1.x - s0.x) * t,
                        y: s0.y + (s1.y - s0.y) * t,
                    },
                )
            };
            let (mut best_t, mut best) = (0.0, f64::INFINITY);
            for k in 0..=40 {
                let t = f64::from(k) / 40.0;
                let d = at(t);
                if d < best {
                    (best_t, best) = (t, d);
                }
            }
            let (mut lo, mut hi) = ((best_t - 0.025_f64).max(0.0), (best_t + 0.025_f64).min(1.0));
            for _ in 0..60 {
                let (m1, m2) = (lo + (hi - lo) / 3.0, hi - (hi - lo) / 3.0);
                if at(m1) <= at(m2) { hi = m2 } else { lo = m1 }
            }
            best.min(at((lo + hi) / 2.0))
        };
        let one_way = |x: &[Coord<f64>], y: &[Coord<f64>]| {
            dense(x)
                .into_iter()
                .map(|p| {
                    y.windows(2)
                        .map(|w| edge_min(p, w[0], w[1]))
                        .fold(f64::INFINITY, f64::min)
                })
                .fold(f64::INFINITY, f64::min)
        };
        one_way(a, b).min(one_way(b, a))
    }

    fn linestring(cs: &[Coord<f64>]) -> String {
        let pts: Vec<String> = cs.iter().map(|c| format!("{} {}", c.x, c.y)).collect();
        format!("LINESTRING({})", pts.join(", "))
    }

    /// Random disjoint polylines with gaps up to about 1,000 km: the geodesic result is
    /// never below the reference by more than its own sampling error, and at most
    /// 0.1 % above it.
    fn gap_search(pairs: usize) {
        let mut rng = StdRng::seed_from_u64(7);
        let mut checked = 0;
        while checked < pairs {
            let lat0: f64 = rng.random_range(-70.0..70.0);
            let lon0: f64 = rng.random_range(-180.0..180.0);
            let walk = |rng: &mut StdRng, x: f64, y: f64| -> Vec<Coord<f64>> {
                let mut c = Coord { x, y };
                let mut out = vec![c];
                for _ in 0..rng.random_range(1..4) {
                    c = Coord {
                        x: c.x + rng.random_range(-2.0..2.0),
                        y: (c.y + rng.random_range(-2.0..2.0)).clamp(-85.0, 85.0),
                    };
                    out.push(c);
                }
                out
            };
            let a = walk(&mut rng, lon0, lat0);
            let (dx, dy): (f64, f64) = (rng.random_range(-8.0..8.0), rng.random_range(-8.0..8.0));
            let b = walk(&mut rng, lon0 + dx, lat0 + dy);
            let (ga, gb) = (g(&linestring(&a)), g(&linestring(&b)));
            let d = distance_m(&ga, &gb, DistanceModel::Geodesic).unwrap();
            if d == 0.0 || d > 1_000_000.0 {
                continue;
            }
            let r = brute(&a, &b);
            assert!(
                d >= r * (1.0 - 1e-9),
                "{d} below the reference {r}: {a:?} {b:?}"
            );
            assert!(d <= r * 1.001, "{d} more than 0.1 % above {r}: {a:?} {b:?}");
            checked += 1;
        }
    }

    #[test]
    fn gap_search_against_brute_force() {
        gap_search(100);
    }

    #[test]
    #[ignore = "slow: 1,000 pairs"]
    fn gap_search_against_brute_force_full() {
        gap_search(1000);
    }

    #[test]
    fn lower_bound() {
        // inside, beside, beyond the antimeridian, near a pole
        assert_eq!(lower_bound_m([5.0, 5.0], [0.0, 0.0, 10.0, 10.0]), 0.0);
        let d = lower_bound_m([5.0, 12.0], [0.0, 0.0, 10.0, 10.0]);
        assert!((d - 2f64.to_radians() * LOWER_BOUND_RADIUS).abs() < 1e-6);
        let mut rng = StdRng::seed_from_u64(11);
        for _ in 0..2000 {
            let x0: f64 = rng.random_range(-180.0..170.0);
            let y0: f64 = rng.random_range(-89.0..80.0);
            let bbox = [
                x0,
                y0,
                x0 + rng.random_range(0.0..10.0),
                (y0 + rng.random_range(0.0..10.0)).min(90.0),
            ];
            let p = [
                rng.random_range(-180.0..180.0),
                rng.random_range(-90.0..90.0),
            ];
            let lb = lower_bound_m(p, bbox);
            // never above the geodesic or haversine distance to any point of the box
            for _ in 0..20 {
                let q = Coord {
                    x: rng.random_range(bbox[0]..=bbox[2]),
                    y: rng.random_range(bbox[1]..=bbox[3]),
                };
                let pc = Coord { x: p[0], y: p[1] };
                assert!(lb <= geodesic(pc, q) + 1e-6, "{p:?} {bbox:?} {q:?}");
                assert!(lb <= haversine(pc, q) + 1e-6);
            }
        }
        let d = lower_bound_m([179.5, 0.0], [-180.0, -1.0, -179.5, 1.0]);
        assert!(
            (d - 0.5f64.to_radians() * LOWER_BOUND_RADIUS).abs() < 1e-6,
            "{d}"
        );
    }

    #[test]
    fn windows() {
        let covered = |ws: &[[f64; 4]], q: Coord<f64>| {
            ws.iter().any(|w| {
                q.y >= w[1]
                    && q.y <= w[3]
                    && [q.x, q.x - 360.0, q.x + 360.0]
                        .iter()
                        .any(|&x| x >= w[0] && x <= w[2])
            })
        };
        assert_eq!(radius_windows([0.0, 85.0, 1.0, 86.0], 1_000_000.0).len(), 1);
        assert_eq!(
            radius_windows([0.0, 85.0, 1.0, 86.0], 1_000_000.0)[0][0],
            -180.0
        );
        let w = radius_windows([179.0, 0.0, 179.5, 1.0], 200_000.0);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().all(|b| b[0] >= -180.0 && b[2] <= 180.0));
        // every point within the radius (geodesic and haversine) is in a window
        let mut rng = StdRng::seed_from_u64(5);
        for _ in 0..500 {
            let c = Coord {
                x: rng.random_range(-180.0..180.0),
                y: rng.random_range(-80.0..80.0),
            };
            let r: f64 = rng.random_range(1.0..2_000_000.0);
            let ws = radius_windows([c.x, c.y, c.x, c.y], r);
            for _ in 0..40 {
                let az: f64 = rng.random_range(0.0..360.0);
                let s = r * rng.random_range(0.0..1.0_f64).sqrt();
                let (lat, lon): (f64, f64) =
                    geographiclib_rs::DirectGeodesic::direct(wgs84(), c.y, c.x, az, s);
                assert!(
                    covered(&ws, Coord { x: lon, y: lat }),
                    "{c:?} r={r} {lon} {lat} {ws:?}"
                );
            }
        }
    }
}
