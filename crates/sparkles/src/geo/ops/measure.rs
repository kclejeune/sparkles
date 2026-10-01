//! Area, length and perimeter: geodesic on geographic CRSs (or on the haversine sphere
//! under that distance model), planar otherwise.

use super::aeqd::MEAN_RADIUS;
use super::distance::{geodesic, haversine};
use super::{OpError, guarded, is_geographic, type_error};
use crate::geo::DistanceModel;
use crate::geo::geom::Geom;
use crate::geo::units::{Unit, UnitKind};
use georust::{
    Area, Coord, GeodesicArea, Geometry, LineString, MultiPolygon, Polygon, unary_union,
};

/// How a geometry's coordinates are measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Metric {
    Planar,
    Geodesic,
    Sphere,
}

fn metric(g: &Geom, m: DistanceModel, what: &str) -> Result<Metric, OpError> {
    match is_geographic(&g.crs) {
        None => Err(type_error(format!("{what}: the CRS has no known unit"))),
        Some(false) => Ok(Metric::Planar),
        Some(true) => Ok(match m {
            DistanceModel::Geodesic => Metric::Geodesic,
            DistanceModel::Haversine => Metric::Sphere,
        }),
    }
}

/// Area in square metres (square CRS units for projected CRSs): the union of the areal
/// parts (overlapping members count once); 0 for points and curves.
pub fn area_m2(g: &Geom, m: DistanceModel) -> Result<f64, OpError> {
    let metric = metric(g, m, "area")?;
    let polys = areal_union(&g.g)?;
    Ok(match metric {
        Metric::Planar => polys.unsigned_area(),
        Metric::Geodesic => rings_area(&polys, geodesic_ring_area),
        Metric::Sphere => rings_area(&polys, sphere_ring_area),
    })
}

/// Length in metres: curves, the rings of areal parts, the sum over members; 0 for
/// points.
pub fn length_m(g: &Geom, m: DistanceModel) -> Result<f64, OpError> {
    let metric = metric(g, m, "length")?;
    let mut total = 0.0;
    lines(&g.g, &mut |l| total += line_length(l, metric));
    Ok(total)
}

/// Perimeter in metres: the boundary length of the areal parts (of their union); 0 for
/// points and curves.
pub fn perimeter_m(g: &Geom, m: DistanceModel) -> Result<f64, OpError> {
    let metric = metric(g, m, "perimeter")?;
    let polys = areal_union(&g.g)?;
    Ok(polys
        .0
        .iter()
        .flat_map(|p| std::iter::once(p.exterior()).chain(p.interiors()))
        .map(|r| line_length(r, metric))
        .sum())
}

/// [`area_m2`] in an area unit.
pub fn area(g: &Geom, u: &Unit, m: DistanceModel) -> Result<f64, OpError> {
    if u.kind != UnitKind::Area {
        return Err(type_error("area: the unit is not an area unit"));
    }
    Ok(area_m2(g, m)? / u.factor)
}

/// [`length_m`] in a length unit.
pub fn length(g: &Geom, u: &Unit, m: DistanceModel) -> Result<f64, OpError> {
    if u.kind != UnitKind::Length {
        return Err(type_error("length: the unit is not a length unit"));
    }
    Ok(length_m(g, m)? / u.factor)
}

/// [`perimeter_m`] in a length unit.
pub fn perimeter(g: &Geom, u: &Unit, m: DistanceModel) -> Result<f64, OpError> {
    if u.kind != UnitKind::Length {
        return Err(type_error("perimeter: the unit is not a length unit"));
    }
    Ok(perimeter_m(g, m)? / u.factor)
}

/// The polygons of a geometry, unioned when there are several (so overlaps count once).
pub(crate) fn areal_union(g: &Geometry<f64>) -> Result<MultiPolygon<f64>, OpError> {
    let mut polys = Vec::new();
    polygons(g, &mut polys);
    if polys.len() <= 1 {
        return Ok(MultiPolygon(polys));
    }
    guarded("area", || unary_union(&polys))
}

/// Every polygon of a geometry (collections flattened).
pub(crate) fn polygons(g: &Geometry<f64>, out: &mut Vec<Polygon<f64>>) {
    match g {
        Geometry::Polygon(p) => out.push(p.clone()),
        Geometry::MultiPolygon(m) => out.extend(m.0.iter().cloned()),
        Geometry::Rect(r) => out.push(r.to_polygon()),
        Geometry::Triangle(t) => out.push(t.to_polygon()),
        Geometry::GeometryCollection(c) => c.0.iter().for_each(|m| polygons(m, out)),
        _ => {}
    }
}

/// Every curve of a geometry: line strings, and the rings of polygons.
fn lines(g: &Geometry<f64>, f: &mut impl FnMut(&LineString<f64>)) {
    match g {
        Geometry::Line(l) => f(&LineString(vec![l.start, l.end])),
        Geometry::LineString(l) => f(l),
        Geometry::MultiLineString(m) => m.0.iter().for_each(f),
        Geometry::Polygon(_)
        | Geometry::MultiPolygon(_)
        | Geometry::Rect(_)
        | Geometry::Triangle(_) => {
            let mut ps = Vec::new();
            polygons(g, &mut ps);
            for p in &ps {
                f(p.exterior());
                p.interiors().iter().for_each(&mut *f);
            }
        }
        Geometry::GeometryCollection(c) => c.0.iter().for_each(|m| lines(m, f)),
        Geometry::Point(_) | Geometry::MultiPoint(_) => {}
    }
}

fn line_length(l: &LineString<f64>, m: Metric) -> f64 {
    l.0.windows(2).map(|w| segment(w[0], w[1], m)).sum()
}

fn segment(p: Coord<f64>, q: Coord<f64>, m: Metric) -> f64 {
    match m {
        Metric::Planar => (q.x - p.x).hypot(q.y - p.y),
        Metric::Geodesic => geodesic(p, q),
        Metric::Sphere => haversine(p, q),
    }
}

/// The area of polygons from the areas of their rings (each ring taken as the smaller
/// region it bounds, whatever its orientation), holes subtracted.
fn rings_area(polys: &MultiPolygon<f64>, ring: fn(&LineString<f64>) -> f64) -> f64 {
    polys
        .0
        .iter()
        .map(|p| {
            let holes: f64 = p.interiors().iter().map(ring).sum();
            (ring(p.exterior()) - holes).max(0.0)
        })
        .sum()
}

/// Area of a ring on the WGS 84 ellipsoid (Karney), oriented counter-clockwise first
/// (a clockwise ring would bound the rest of the Earth).
fn geodesic_ring_area(r: &LineString<f64>) -> f64 {
    let twice: f64 =
        r.0.windows(2)
            .map(|w| w[0].x * w[1].y - w[1].x * w[0].y)
            .sum();
    let mut r = r.clone();
    if twice < 0.0 {
        r.0.reverse();
    }
    Polygon::new(r, vec![]).geodesic_area_unsigned()
}

/// Unsigned area of a ring on the sphere of the mean Earth radius (the edges taken as
/// rhumb-like lines: Chamberlain and Duquette's formula).
fn sphere_ring_area(r: &LineString<f64>) -> f64 {
    let mut sum = 0.0;
    for w in r.0.windows(2) {
        let mut dl = (w[1].x - w[0].x).to_radians();
        // the shorter way round
        if dl > std::f64::consts::PI {
            dl -= 2.0 * std::f64::consts::PI;
        } else if dl < -std::f64::consts::PI {
            dl += 2.0 * std::f64::consts::PI;
        }
        sum += dl * (2.0 + w[0].y.to_radians().sin() + w[1].y.to_radians().sin());
    }
    (sum * MEAN_RADIUS * MEAN_RADIUS / 2.0).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(s: &str) -> Geom {
        crate::geo::ops::wkt(s)
    }

    const GEO: DistanceModel = DistanceModel::Geodesic;
    const SQUARE: &str = "POLYGON((0 0, 1 0, 1 1, 0 1, 0 0))";

    #[test]
    fn reference_values() {
        // GeographicLib's test of a one-degree square at the equator: area
        // 12,308,778,361.469 m², perimeter 443,770.917 m
        let a = area_m2(&g(SQUARE), GEO).unwrap();
        assert!((a - 12_308_778_361.469).abs() / a < 1e-6, "{a}");
        let p = perimeter_m(&g(SQUARE), GEO).unwrap();
        assert!((p - 443_770.917).abs() < 1e-3, "{p}");
        // a polygon's length is its perimeter; a line's, its geodesic length
        assert_eq!(length_m(&g(SQUARE), GEO).unwrap(), p);
        let l = length_m(&g("LINESTRING(0 0, 1 0)"), GEO).unwrap();
        assert!((l - 111_319.490_793_273_57).abs() < 1e-6);
        let km2 = Unit {
            kind: UnitKind::Area,
            factor: 1e6,
        };
        assert!((area(&g(SQUARE), &km2, GEO).unwrap() - 12_308.778).abs() < 1e-3);
    }

    #[test]
    fn non_areal_and_collections() {
        assert_eq!(area_m2(&g("POINT(0 0)"), GEO).unwrap(), 0.0);
        assert_eq!(area_m2(&g("LINESTRING(0 0, 1 1)"), GEO).unwrap(), 0.0);
        assert_eq!(perimeter_m(&g("LINESTRING(0 0, 1 1)"), GEO).unwrap(), 0.0);
        assert_eq!(length_m(&g("MULTIPOINT((0 0),(1 1))"), GEO).unwrap(), 0.0);
        // a hole is subtracted; overlapping members count once
        let one = area_m2(&g(SQUARE), GEO).unwrap();
        let holed = area_m2(
            &g("POLYGON((0 0, 2 0, 2 2, 0 2, 0 0),(0 0, 1 0, 1 1, 0 1, 0 0))"),
            GEO,
        );
        let big = area_m2(&g("POLYGON((0 0, 2 0, 2 2, 0 2, 0 0))"), GEO).unwrap();
        // the orientation of a ring does not matter
        let cw = area_m2(&g("POLYGON((0 0, 0 2, 2 2, 2 0, 0 0))"), GEO).unwrap();
        assert!((cw - big).abs() / big < 1e-12, "{cw} {big}");
        let holed = holed.unwrap();
        assert!(
            (holed - (big - one)).abs() / big < 1e-9,
            "{holed} {big} {one}"
        );
        let twice = area_m2(
            &g("MULTIPOLYGON(((0 0, 1 0, 1 1, 0 1, 0 0)),((0 0, 1 0, 1 1, 0 1, 0 0)))"),
            GEO,
        );
        assert!((twice.unwrap() - one).abs() / one < 1e-9);
        let gc = length_m(
            &g("GEOMETRYCOLLECTION(POINT(5 5), LINESTRING(0 0, 1 0), LINESTRING(0 0, 1 0))"),
            GEO,
        );
        assert!((gc.unwrap() - 2.0 * 111_319.490_793_273_57).abs() < 1e-6);
    }

    #[test]
    fn models_and_units() {
        let s = area_m2(&g(SQUARE), DistanceModel::Haversine).unwrap();
        let e = area_m2(&g(SQUARE), GEO).unwrap();
        assert!((s - e).abs() / e < 0.01, "{s} {e}");
        let l = length_m(&g("LINESTRING(0 0, 1 0)"), DistanceModel::Haversine).unwrap();
        assert!((l - MEAN_RADIUS.to_radians()).abs() < 1e-6);
        let metre = Unit {
            kind: UnitKind::Length,
            factor: 1.0,
        };
        assert!(area(&g(SQUARE), &metre, GEO).is_err());
        let m2 = Unit {
            kind: UnitKind::Area,
            factor: 1.0,
        };
        assert!(length(&g(SQUARE), &m2, GEO).is_err());
        let mars = crate::geo::ops::wkt(&format!("<http://example.org/crs/mars> {}", SQUARE));
        assert!(area_m2(&mars, GEO).is_err());
    }
}
