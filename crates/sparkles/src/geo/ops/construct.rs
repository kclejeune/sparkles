//! Constructed geometries: buffer, convex hull, envelope, boundary, centroid.
//!
//! Results are 2D, in the CRS of the input. An empty input gives an empty result: a
//! `POLYGON EMPTY` buffer, a `GEOMETRYCOLLECTION EMPTY` hull, envelope, boundary or
//! centroid.

use super::aeqd::Aeqd;
use super::distance::geodesic;
use super::{OpError, guarded, is_geographic, made, type_error};
use crate::geo::geom::{Geom, GeomType};
use crate::geo::units::{Unit, UnitKind};
use georust::algorithm::buffer::{BufferStyle, LineCap, LineJoin};
use georust::{
    BoundingRect, Buffer, Centroid, ConvexHull, Coord, CoordsIter, Geometry, GeometryCollection,
    LineString, MapCoords, MultiLineString, MultiPoint, MultiPolygon, Point, Polygon,
};

/// Largest extent plus radius of a metric buffer on geographic coordinates (beyond it
/// the local projection distorts too much).
pub const METRIC_BUFFER_LIMIT_M: f64 = 1_000_000.0;

/// The angle per segment of round joins and caps: 8 segments per quarter circle.
const ROUND: f64 = std::f64::consts::FRAC_PI_2 / 8.0;

/// `geof:buffer(g, radius, unit)`. A length unit on a geographic CRS buffers in metres
/// through a local azimuthal equidistant projection; an angle unit buffers planarly in
/// degrees; projected CRSs buffer planarly in metres. A negative radius shrinks areal
/// geometries and is an error for points and curves.
pub fn buffer(g: &Geom, radius: f64, u: &Unit) -> Result<Geom, OpError> {
    if !radius.is_finite() {
        return Err(type_error("buffer: the radius is not a finite number"));
    }
    let geographic =
        is_geographic(&g.crs).ok_or_else(|| type_error("buffer: the CRS has no known unit"))?;
    if radius < 0.0 && g.dim() < 2 && !g.empty {
        return Err(type_error(
            "buffer: a negative radius needs an areal geometry",
        ));
    }
    if g.empty {
        return Ok(made(
            g,
            Geometry::Polygon(Polygon::new(LineString(vec![]), vec![])),
        ));
    }
    match (u.kind, geographic) {
        (UnitKind::Length, true) => metric_buffer(g, radius * u.factor),
        (UnitKind::Angle, true) => planar_buffer(g, (radius * u.factor).to_degrees()),
        (UnitKind::Length, false) => planar_buffer(g, radius * u.factor),
        (UnitKind::Angle, false) => Err(type_error("buffer: an angle unit needs a geographic CRS")),
        (UnitKind::Area, _) => Err(type_error("buffer: an area unit is not a length")),
    }
}

fn style(d: f64) -> BufferStyle<f64> {
    BufferStyle::new(d)
        .line_join(LineJoin::Round(ROUND))
        .line_cap(LineCap::Round(ROUND))
}

fn planar(g: &Geometry<f64>, d: f64) -> Result<MultiPolygon<f64>, OpError> {
    guarded("buffer", || g.buffer_with_style(style(d)))
}

fn planar_buffer(g: &Geom, d: f64) -> Result<Geom, OpError> {
    Ok(made(g, polygonal(planar(&g.g, d)?)))
}

/// Buffer by `r` metres on the ellipsoid: project around the envelope's centre, buffer
/// in the plane, project back.
fn metric_buffer(g: &Geom, r: f64) -> Result<Geom, OpError> {
    let Some(rect) = g.g.bounding_rect() else {
        return Ok(made(
            g,
            Geometry::Polygon(Polygon::new(LineString(vec![]), vec![])),
        ));
    };
    let c = rect.center();
    let proj = Aeqd::ellipsoid(c.x, c.y);
    let extent =
        g.g.coords_iter()
            .map(|p| geodesic(c, p))
            .fold(0.0_f64, f64::max);
    if extent + r.abs() > METRIC_BUFFER_LIMIT_M {
        return Err(type_error(
            "buffer: geometry too large for a metric buffer (extent + radius > 1000 km)",
        ));
    }
    let reach = extent + r.max(0.0);
    for pole in [90.0, -90.0] {
        if geodesic(c, Coord { x: c.x, y: pole }) <= reach {
            return Err(type_error("buffer: a metric buffer must not cover a pole"));
        }
    }
    let planar_g = g.g.map_coords(|p| {
        let (x, y) = proj.forward(p.x, p.y);
        Coord { x, y }
    });
    let out = planar(&planar_g, r)?.map_coords(|p| {
        let (x, y) = proj.inverse(p.x, p.y);
        Coord { x, y }
    });
    Ok(made(g, polygonal(out)))
}

/// A multipolygon as the simplest areal geometry (`POLYGON EMPTY` when empty).
fn polygonal(mut m: MultiPolygon<f64>) -> Geometry<f64> {
    match m.0.len() {
        0 => Geometry::Polygon(Polygon::new(LineString(vec![]), vec![])),
        1 => Geometry::Polygon(m.0.pop().expect("one member")),
        _ => Geometry::MultiPolygon(m),
    }
}

fn empty_collection(g: &Geom) -> Geom {
    made(g, Geometry::GeometryCollection(GeometryCollection(vec![])))
}

/// `geof:convexHull`: planar in the CRS of `g`; a point or a segment when the hull is
/// degenerate.
pub fn convex_hull(g: &Geom) -> Result<Geom, OpError> {
    if g.empty {
        return Ok(empty_collection(g));
    }
    let hull = guarded("convexHull", || g.g.convex_hull())?;
    let mut pts: Vec<Coord<f64>> = hull.exterior().0.clone();
    pts.dedup();
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    Ok(made(
        g,
        match pts.len() {
            0 => Geometry::GeometryCollection(GeometryCollection(vec![])),
            1 => Geometry::Point(Point(pts[0])),
            2 => Geometry::LineString(LineString(pts)),
            _ => Geometry::Polygon(hull),
        },
    ))
}

/// `geof:envelope`: the bounding box as a polygon; a point, or a segment when the box
/// is degenerate (as in JTS).
pub fn envelope(g: &Geom) -> Result<Geom, OpError> {
    let Some(r) = (!g.empty).then(|| g.g.bounding_rect()).flatten() else {
        return Ok(empty_collection(g));
    };
    let (lo, hi) = (r.min(), r.max());
    Ok(made(
        g,
        if lo == hi {
            Geometry::Point(Point(lo))
        } else if lo.x == hi.x || lo.y == hi.y {
            Geometry::LineString(LineString(vec![lo, hi]))
        } else {
            Geometry::Polygon(r.to_polygon())
        },
    ))
}

/// `geof:boundary`, as OGC defines it: points have none, a curve's boundary is its end
/// points by the mod-2 rule (none for closed curves), a polygon's its rings.
pub fn boundary(g: &Geom) -> Result<Geom, OpError> {
    if g.empty {
        return Ok(empty_collection(g));
    }
    Ok(made(g, boundary_of(&g.g)))
}

fn boundary_of(g: &Geometry<f64>) -> Geometry<f64> {
    use Geometry as G;
    let rings = |ps: &[Polygon<f64>]| {
        let mut ls: Vec<LineString<f64>> = Vec::new();
        for p in ps {
            if p.exterior().0.is_empty() {
                continue;
            }
            ls.push(p.exterior().clone());
            ls.extend(p.interiors().iter().cloned());
        }
        if ls.len() == 1 {
            G::LineString(ls.pop().expect("one ring"))
        } else {
            G::MultiLineString(MultiLineString(ls))
        }
    };
    match g {
        G::Point(_) | G::MultiPoint(_) => G::GeometryCollection(GeometryCollection(vec![])),
        G::Line(l) => G::MultiPoint(MultiPoint(vec![Point(l.start), Point(l.end)])),
        G::LineString(l) => ends(std::slice::from_ref(l)),
        G::MultiLineString(m) => ends(&m.0),
        G::Polygon(p) => rings(std::slice::from_ref(p)),
        G::MultiPolygon(m) => rings(&m.0),
        G::Rect(r) => rings(&[r.to_polygon()]),
        G::Triangle(t) => rings(&[t.to_polygon()]),
        G::GeometryCollection(c) => G::GeometryCollection(GeometryCollection(
            c.0.iter()
                .map(boundary_of)
                .filter(|b| !georust::HasDimensions::is_empty(b))
                .collect(),
        )),
    }
}

/// End points that end an odd number of the curves.
fn ends(ls: &[LineString<f64>]) -> Geometry<f64> {
    let mut seen: Vec<(Coord<f64>, usize)> = Vec::new();
    for l in ls {
        let (Some(&a), Some(&b)) = (l.0.first(), l.0.last()) else {
            continue;
        };
        for p in [a, b] {
            match seen.iter_mut().find(|(q, _)| *q == p) {
                Some((_, n)) => *n += 1,
                None => seen.push((p, 1)),
            }
        }
    }
    Geometry::MultiPoint(MultiPoint(
        seen.into_iter()
            .filter(|(_, n)| n % 2 == 1)
            .map(|(p, _)| Point(p))
            .collect(),
    ))
}

/// `geof:centroid`: planar in the CRS of `g` (the highest dimension's parts weigh).
pub fn centroid(g: &Geom) -> Result<Geom, OpError> {
    if g.empty {
        return Ok(empty_collection(g));
    }
    match guarded("centroid", || g.g.centroid())? {
        Some(p) => Ok(made(g, Geometry::Point(p))),
        None => Ok(empty_collection(g)),
    }
}

/// The empty geometry of a type, in the CRS of `like`.
pub fn empty_of(like: &Geom, t: GeomType) -> Geom {
    use Geometry as G;
    let g = match t {
        GeomType::Point | GeomType::MultiPoint => G::MultiPoint(MultiPoint(vec![])),
        GeomType::LineString | GeomType::LinearRing => G::LineString(LineString(vec![])),
        GeomType::MultiLineString => G::MultiLineString(MultiLineString(vec![])),
        GeomType::Polygon | GeomType::Triangle => {
            G::Polygon(Polygon::new(LineString(vec![]), vec![]))
        }
        GeomType::MultiPolygon | GeomType::Tin | GeomType::PolyhedralSurface => {
            G::MultiPolygon(MultiPolygon(vec![]))
        }
        GeomType::GeometryCollection => G::GeometryCollection(GeometryCollection(vec![])),
    };
    let mut out = made(like, g);
    out.declared = t;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::crs::{CRS84, CrsRef};
    use crate::geo::ops::measure::area_m2;
    use crate::geo::ops::relate::relation;
    use crate::geo::{DistanceModel, Relation};
    use wkt::TryFromWkt;

    fn g(s: &str) -> Geom {
        let geometry = Geometry::<f64>::try_from_wkt_str(s).unwrap();
        Geom::from_geometry(CrsRef::Known(CRS84), geometry)
    }

    fn same(a: &Geom, b: &str) -> bool {
        relation(a, &g(b), Relation::SfEquals).unwrap()
    }

    const METRE: Unit = Unit {
        kind: UnitKind::Length,
        factor: 1.0,
    };
    const DEGREE: Unit = Unit {
        kind: UnitKind::Angle,
        factor: std::f64::consts::PI / 180.0,
    };

    #[test]
    fn hulls_and_envelopes() {
        let h = convex_hull(&g("MULTIPOINT((0 0),(2 0),(1 1),(1 0.5))")).unwrap();
        assert!(same(&h, "POLYGON((0 0,2 0,1 1,0 0))"));
        let e = envelope(&g("LINESTRING(0 0, 2 1)")).unwrap();
        assert!(same(&e, "POLYGON((0 0,2 0,2 1,0 1,0 0))"));
        assert!(same(&envelope(&g("POINT(3 4)")).unwrap(), "POINT(3 4)"));
        let flat = envelope(&g("LINESTRING(0 1, 2 1)")).unwrap();
        assert!(matches!(flat.g, Geometry::LineString(_)));
        assert!(same(&flat, "LINESTRING(0 1, 2 1)"));
        let seg = convex_hull(&g("MULTIPOINT((0 0),(1 1),(2 2))")).unwrap();
        assert!(same(&seg, "LINESTRING(0 0, 2 2)"), "{:?}", seg.g);
        assert!(same(&convex_hull(&g("POINT(1 1)")).unwrap(), "POINT(1 1)"));
        for f in [convex_hull, envelope, boundary, centroid] {
            let e = f(&g("POLYGON EMPTY")).unwrap();
            assert!(e.empty && e.declared == GeomType::GeometryCollection);
        }
    }

    #[test]
    fn boundaries() {
        assert!(boundary(&g("POINT(1 1)")).unwrap().empty);
        let b = boundary(&g("LINESTRING(0 0, 1 1, 2 0)")).unwrap();
        assert!(same(&b, "MULTIPOINT((0 0),(2 0))"));
        assert!(
            boundary(&g("LINESTRING(0 0, 1 1, 2 0, 0 0)"))
                .unwrap()
                .empty
        );
        // mod 2: the shared end point of two curves is interior
        let b = boundary(&g("MULTILINESTRING((0 0, 1 1),(1 1, 2 0))")).unwrap();
        assert!(same(&b, "MULTIPOINT((0 0),(2 0))"));
        let b = boundary(&g(
            "POLYGON((0 0, 4 0, 4 4, 0 4, 0 0),(1 1, 2 1, 2 2, 1 1))",
        ))
        .unwrap();
        assert!(same(
            &b,
            "MULTILINESTRING((0 0, 4 0, 4 4, 0 4, 0 0),(1 1, 2 1, 2 2, 1 1))"
        ));
        assert!(matches!(
            boundary(&g("POLYGON((0 0, 4 0, 4 4, 0 0))")).unwrap().g,
            Geometry::LineString(_)
        ));
    }

    #[test]
    fn centroids() {
        let c = centroid(&g("POLYGON((0 0, 4 0, 4 2, 0 2, 0 0))")).unwrap();
        assert!(same(&c, "POINT(2 1)"));
    }

    #[test]
    fn buffers() {
        // a 1 km disc at the equator: a 32-gon has 0.64 % less area than its circle
        let disc = buffer(&g("POINT(0 0)"), 1000.0, &METRE).unwrap();
        let a = area_m2(&disc, DistanceModel::Geodesic).unwrap();
        let circle = std::f64::consts::PI * 1e6;
        assert!(a > circle * 0.993 && a < circle * 1.001, "{a} {circle}");
        // every vertex lies at the radius (geodesic), within 0.5 %
        for p in disc.g.coords_iter() {
            let d = geodesic(Coord { x: 0.0, y: 0.0 }, p);
            assert!((d - 1000.0).abs() < 5.0, "{d}");
        }
        // a line at 60°N: its buffer's vertices stay within 0.5 % of 10 km from it
        let line = g("LINESTRING(10 60, 11 60.5)");
        let b = buffer(&line, 10_000.0, &METRE).unwrap();
        for p in b.g.coords_iter() {
            let pt = Geom::from_geometry(CrsRef::Known(CRS84), Geometry::Point(Point(p)));
            let d =
                super::super::distance::distance_m(&pt, &line, DistanceModel::Geodesic).unwrap();
            assert!((d - 10_000.0).abs() < 50.0, "{d}");
        }
        // degrees: a planar circle of radius 1°
        let deg = buffer(&g("POINT(0 0)"), 1.0, &DEGREE).unwrap();
        for p in deg.g.coords_iter() {
            assert!((p.x.hypot(p.y) - 1.0).abs() < 1e-6);
        }
        // shrinking an area; refusing to shrink points and lines
        let shrunk = buffer(&g("POLYGON((0 0, 4 0, 4 4, 0 4, 0 0))"), -1.0, &DEGREE).unwrap();
        assert!(same(&shrunk, "POLYGON((1 1, 3 1, 3 3, 1 3, 1 1))"));
        assert!(
            buffer(&g("POLYGON((0 0, 1 0, 1 1, 0 1, 0 0))"), -2.0, &DEGREE)
                .unwrap()
                .empty
        );
        assert!(buffer(&g("POINT(0 0)"), -1.0, &DEGREE).is_err());
        assert!(buffer(&g("LINESTRING(0 0, 1 1)"), -1.0, &METRE).is_err());
        // too large; covering a pole; no unit for the CRS
        assert!(buffer(&g("POINT(0 0)"), 1_000_001.0, &METRE).is_err());
        assert!(buffer(&g("LINESTRING(0 0, 18 0)"), 1000.0, &METRE).is_err());
        assert!(buffer(&g("POINT(0 89.9)"), 20_000.0, &METRE).is_err());
        let mars = Geom::from_geometry(
            CrsRef::Unknown("http://example.org/crs/mars".into()),
            Geometry::Point(Point::new(1.0, 1.0)),
        );
        assert!(buffer(&mars, 1.0, &DEGREE).is_err());
        assert!(buffer(&g("POINT EMPTY"), 1.0, &METRE).unwrap().empty);
    }
}
