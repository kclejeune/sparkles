//! The smallest enclosing circle and the concave hull, planar in the CRS of the input
//! like the convex hull.
//!
//! An empty input gives `GEOMETRYCOLLECTION EMPTY`; inputs whose points are all equal
//! give that point, and collinear inputs a segment (as the convex hull does).

use super::construct::convex_hull;
use super::{OpError, guarded, made, type_error};
use crate::geo::geom::Geom;
use georust::concave_hull::ConcaveHullOptions;
use georust::{ConcaveHull, Coord, CoordsIter, Geometry, LineString, MultiPoint, Point, Polygon};

/// Segments per quarter circle of a polygonized bounding circle.
pub const CIRCLE_SEGMENTS_PER_QUADRANT: usize = 32;

/// `geo`'s concavity when `concaveHull` has no target percentage.
pub const DEFAULT_CONCAVITY: f64 = 2.0;

/// `geof:boundingCircle`: the smallest circle that holds every vertex of `g` (Welzl's
/// algorithm), as a polygon of 32 segments per quarter circle that circumscribes the
/// circle, so every input point is inside it. A single distinct point is its own
/// bounding circle.
pub fn bounding_circle(g: &Geom) -> Result<Geom, OpError> {
    let pts = distinct_coords(g);
    if pts.is_empty() {
        return Ok(Geom::empty(
            g.crs.clone(),
            crate::geo::GeomType::GeometryCollection,
        ));
    }
    let (c, r) = smallest_circle(pts);
    if r == 0.0 {
        return Ok(made(g, Geometry::Point(Point(c))));
    }
    Ok(made(g, Geometry::Polygon(circle_polygon(c, r))))
}

/// The vertices of `g` without repeats, in a deterministic order.
pub(crate) fn distinct_coords(g: &Geom) -> Vec<Coord<f64>> {
    if g.empty {
        return Vec::new();
    }
    let mut pts: Vec<Coord<f64>> = g.g.coords_iter().collect();
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup();
    pts
}

/// A polygon with `4 × CIRCLE_SEGMENTS_PER_QUADRANT` sides around the circle (its
/// edges touch the circle), counter-clockwise from the east.
fn circle_polygon(c: Coord<f64>, r: f64) -> Polygon<f64> {
    let n = 4 * CIRCLE_SEGMENTS_PER_QUADRANT;
    let step = std::f64::consts::TAU / n as f64;
    // the vertex radius of a regular n-gon whose edges touch the circle
    let rv = r / (step / 2.0).cos();
    let mut ring: Vec<Coord<f64>> = (0..n)
        .map(|k| {
            let a = step * k as f64;
            Coord {
                x: c.x + rv * a.cos(),
                y: c.y + rv * a.sin(),
            }
        })
        .collect();
    ring.push(ring[0]);
    Polygon::new(LineString(ring), vec![])
}

/// The smallest circle holding `pts` (not empty): Welzl's algorithm in its iterative
/// form, over the points in a fixed pseudo-random order (expected linear time).
pub(crate) fn smallest_circle(mut pts: Vec<Coord<f64>>) -> (Coord<f64>, f64) {
    shuffle(&mut pts);
    // a relative tolerance: points on the circle up to rounding count as inside
    let scale = pts
        .iter()
        .fold(0.0_f64, |m, p| m.max(p.x.abs()).max(p.y.abs()))
        .max(1.0);
    let eps = 1e-12 * scale;
    let inside = |(c, r): (Coord<f64>, f64), p: Coord<f64>| dist(c, p) <= r + eps;
    let mut circle = (pts[0], 0.0);
    for i in 1..pts.len() {
        if inside(circle, pts[i]) {
            continue;
        }
        circle = (pts[i], 0.0);
        for j in 0..i {
            if inside(circle, pts[j]) {
                continue;
            }
            circle = diametral(pts[i], pts[j]);
            for k in 0..j {
                if !inside(circle, pts[k]) {
                    circle = circumscribed(pts[i], pts[j], pts[k]);
                }
            }
        }
    }
    circle
}

fn dist(a: Coord<f64>, b: Coord<f64>) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

/// The circle with diameter `ab`.
fn diametral(a: Coord<f64>, b: Coord<f64>) -> (Coord<f64>, f64) {
    let c = Coord {
        x: (a.x + b.x) / 2.0,
        y: (a.y + b.y) / 2.0,
    };
    (c, dist(a, b) / 2.0)
}

/// The circle through `a`, `b` and `c`; for (nearly) collinear points, the circle on
/// the farthest pair.
fn circumscribed(a: Coord<f64>, b: Coord<f64>, c: Coord<f64>) -> (Coord<f64>, f64) {
    let (bx, by) = (b.x - a.x, b.y - a.y);
    let (cx, cy) = (c.x - a.x, c.y - a.y);
    let d = 2.0 * (bx * cy - by * cx);
    let span = (bx.abs() + by.abs()).max(cx.abs() + cy.abs());
    if d.abs() <= 1e-14 * span * span {
        let pairs = [(a, b), (a, c), (b, c)];
        let (p, q) = pairs
            .into_iter()
            .max_by(|x, y| dist(x.0, x.1).total_cmp(&dist(y.0, y.1)))
            .expect("three pairs");
        return diametral(p, q);
    }
    let (b2, c2) = (bx * bx + by * by, cx * cx + cy * cy);
    let ux = (cy * b2 - by * c2) / d;
    let uy = (bx * c2 - cx * b2) / d;
    let centre = Coord {
        x: a.x + ux,
        y: a.y + uy,
    };
    // the largest of the three distances, so rounding never leaves one outside
    let r = dist(centre, a).max(dist(centre, b)).max(dist(centre, c));
    (centre, r)
}

/// A deterministic Fisher–Yates shuffle (xorshift), so results do not depend on runs.
fn shuffle<T>(v: &mut [T]) {
    let mut s: u64 = 0x9e37_79b9_7f4a_7c15 ^ v.len() as u64;
    for i in (1..v.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        v.swap(i, (s % (i as u64 + 1)) as usize);
    }
}

/// The concavity of `geo`'s concave hull for `geof:concaveHull(g, targetPercent)`:
/// `targetPercent / 25`, so 50 gives the default concavity 2.0 and smaller values
/// follow the input more closely; 100 is the convex hull. Outside (0, 100]: `None`.
pub fn concavity(target_percent: f64) -> Option<f64> {
    if target_percent > 0.0 && target_percent < 100.0 {
        Some(target_percent / 25.0)
    } else if target_percent == 100.0 {
        Some(f64::INFINITY)
    } else {
        None
    }
}

/// `geof:concaveHull(g[, targetPercent])`: `geo`'s concave hull (concaveman) of the
/// vertices of `g` with the given concavity ([`concavity`]; infinite: the convex
/// hull), planar in the CRS of `g`.
pub fn concave_hull(g: &Geom, concavity: f64) -> Result<Geom, OpError> {
    if concavity.is_nan() || concavity <= 0.0 {
        return Err(type_error("concaveHull: the concavity must be positive"));
    }
    let convex = convex_hull(g)?;
    // degenerate hulls (a point, a segment, nothing) and the convex limit
    if concavity.is_infinite() || !matches!(convex.g, Geometry::Polygon(_)) {
        return Ok(convex);
    }
    let pts = MultiPoint(distinct_coords(g).into_iter().map(Point).collect());
    let hull = guarded("concaveHull", || {
        pts.concave_hull_with_options(ConcaveHullOptions {
            concavity,
            length_threshold: 0.0,
        })
    })?;
    Ok(made(g, Geometry::Polygon(hull)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::ops::measure::area_m2;
    use crate::geo::ops::relate::relation;
    use crate::geo::ops::wkt;
    use crate::geo::{DistanceModel, Relation};

    fn covers(a: &Geom, b: &Geom) -> bool {
        relation(a, b, Relation::SfContains).unwrap()
    }

    #[test]
    fn bounding_circles() {
        // two points: the circle on their segment as diameter
        let c = bounding_circle(&wkt("MULTIPOINT((0 0),(4 0))")).unwrap();
        let Geometry::Polygon(p) = &c.g else {
            panic!("{:?}", c.g)
        };
        assert_eq!(p.exterior().0.len(), 4 * CIRCLE_SEGMENTS_PER_QUADRANT + 1);
        for v in &p.exterior().0 {
            let d = dist(*v, Coord { x: 2.0, y: 0.0 });
            assert!((2.0..2.002).contains(&d), "{d}");
        }
        // an obtuse triangle: still the diameter of its longest side
        let (centre, r) = smallest_circle(vec![
            Coord { x: 0.0, y: 0.0 },
            Coord { x: 10.0, y: 0.0 },
            Coord { x: 5.0, y: 1.0 },
        ]);
        assert!(dist(centre, Coord { x: 5.0, y: 0.0 }) < 1e-12 && (r - 5.0).abs() < 1e-12);
        // an equilateral triangle: the circumcircle
        let h = 3f64.sqrt();
        let (centre, r) = smallest_circle(vec![
            Coord { x: 0.0, y: 0.0 },
            Coord { x: 2.0, y: 0.0 },
            Coord { x: 1.0, y: h },
        ]);
        assert!(dist(centre, Coord { x: 1.0, y: h / 3.0 }) < 1e-12);
        assert!((r - 2.0 / h).abs() < 1e-12);
        // degenerate inputs
        assert!(matches!(
            bounding_circle(&wkt("MULTIPOINT((1 1),(1 1))")).unwrap().g,
            Geometry::Point(_)
        ));
        assert!(bounding_circle(&wkt("POLYGON EMPTY")).unwrap().empty);
        let line = bounding_circle(&wkt("LINESTRING(0 0, 1 1, 2 2)")).unwrap();
        assert!(covers(&line, &wkt("LINESTRING(0 0, 2 2)")));
    }

    /// Pseudo-random points (deterministic), in a box of side `size`.
    fn cloud(n: usize, seed: u64, size: f64) -> Vec<Coord<f64>> {
        let mut s = seed | 1;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64 * size
        };
        (0..n)
            .map(|_| Coord {
                x: next(),
                y: next(),
            })
            .collect()
    }

    /// Every input inside the circle, and no smaller circle through two or three of the
    /// inputs holds them all (the smallest one always passes through two or three).
    #[test]
    fn bounding_circles_are_minimal() {
        for seed in 1..40 {
            let pts = cloud(3 + seed as usize % 9, seed, 10.0);
            let (c, r) = smallest_circle(pts.clone());
            assert!(pts.iter().all(|p| dist(c, *p) <= r * (1.0 + 1e-12)));
            let holds_all = |(c, r): (Coord<f64>, f64)| {
                pts.iter().all(|p| dist(c, *p) <= r * (1.0 + 1e-9) + 1e-12)
            };
            let mut best = f64::INFINITY;
            for i in 0..pts.len() {
                for j in i + 1..pts.len() {
                    let d = diametral(pts[i], pts[j]);
                    if holds_all(d) {
                        best = best.min(d.1);
                    }
                    for k in j + 1..pts.len() {
                        let t = circumscribed(pts[i], pts[j], pts[k]);
                        if holds_all(t) {
                            best = best.min(t.1);
                        }
                    }
                }
            }
            assert!(
                (r - best).abs() <= 1e-9 * best,
                "seed {seed}: {r} vs {best}"
            );
            // the polygon holds every input too
            let mp = MultiPoint(pts.iter().copied().map(Point).collect());
            let g = Geom::from_geometry(
                crate::geo::crs::CrsRef::Known(crate::geo::crs::CRS84),
                Geometry::MultiPoint(mp),
            );
            assert!(covers(&bounding_circle(&g).unwrap(), &g), "seed {seed}");
        }
    }

    #[test]
    fn concave_hulls() {
        // a crescent of points (two arcs, open to the east): the concave hull follows
        // the hollow, the convex one does not
        let mut pts = Vec::new();
        for deg in (30..=330).step_by(5) {
            let a = f64::from(deg).to_radians();
            for r in [10.0, 9.0] {
                pts.push(format!("({} {})", r * a.cos(), r * a.sin()));
            }
        }
        let g = wkt(&format!("MULTIPOINT({})", pts.join(",")));
        let convex = convex_hull(&g).unwrap();
        // smaller percentages follow the notch more closely
        let mut last = 0.0;
        let area = |g: &Geom| area_m2(g, DistanceModel::Geodesic).unwrap();
        for pct in [1.0, 10.0, 30.0, 50.0] {
            let hull = concave_hull(&g, concavity(pct).unwrap()).unwrap();
            assert!(covers(&convex, &hull), "{pct}");
            assert!(covers(&hull, &g), "{pct}");
            let a = area(&hull);
            assert!(a >= last && a < area(&convex), "{pct}: {a}");
            last = a;
        }
        assert!(area(&concave_hull(&g, concavity(1.0).unwrap()).unwrap()) < 0.6 * area(&convex));
        let full = concave_hull(&g, concavity(100.0).unwrap()).unwrap();
        assert!(relation(&full, &convex, Relation::SfEquals).unwrap());
        // degenerate inputs are their convex hulls
        let seg = concave_hull(&wkt("MULTIPOINT((0 0),(1 1),(2 2))"), 2.0).unwrap();
        assert!(matches!(seg.g, Geometry::LineString(_)));
        assert!(concave_hull(&wkt("POINT EMPTY"), 2.0).unwrap().empty);
        for bad in [0.0, -5.0, 100.5, f64::NAN] {
            assert_eq!(concavity(bad), None, "{bad}");
        }
        assert_eq!(concavity(50.0), Some(DEFAULT_CONCAVITY));
    }
}
