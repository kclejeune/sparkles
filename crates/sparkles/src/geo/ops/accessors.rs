//! Accessors of a geometry (`geof:dimension`, `geometryType`, `numGeometries`,
//! `geometryN`, `minX` … `maxZ`, …).

use super::{OpError, lat_first, made, type_error};
use crate::geo::geom::{Geom, GeomType, Layout};
use crate::geo::vocab::SF;
use georust::{BoundingRect, Geometry};

/// `geof:coordinateDimension`: 2, 3 (XYZ or XYM) or 4.
pub fn coordinate_dimension(g: &Geom) -> i64 {
    match g.layout {
        Layout::Xy => 2,
        Layout::Xyz | Layout::Xym => 3,
        Layout::Xyzm => 4,
    }
}

/// `geof:spatialDimension`: 2, or 3 with Z.
pub fn spatial_dimension(g: &Geom) -> i64 {
    if is_3d(g) { 3 } else { 2 }
}

pub fn is_3d(g: &Geom) -> bool {
    matches!(g.layout, Layout::Xyz | Layout::Xyzm)
}

pub fn is_measured(g: &Geom) -> bool {
    matches!(g.layout, Layout::Xym | Layout::Xyzm)
}

/// `geof:geometryType`: the `sf:` IRI of the type as written.
pub fn geometry_type(g: &Geom) -> String {
    let local = match g.declared {
        GeomType::Point => "Point",
        GeomType::LineString => "LineString",
        GeomType::Polygon => "Polygon",
        GeomType::MultiPoint => "MultiPoint",
        GeomType::MultiLineString => "MultiLineString",
        GeomType::MultiPolygon => "MultiPolygon",
        GeomType::GeometryCollection => "GeometryCollection",
        GeomType::LinearRing => "LinearRing",
        GeomType::Triangle => "Triangle",
        GeomType::Tin => "TIN",
        GeomType::PolyhedralSurface => "PolyhedralSurface",
    };
    format!("{SF}{local}")
}

/// Whether the type is a collection of members (rather than one atomic geometry).
fn is_collection(t: GeomType) -> bool {
    matches!(
        t,
        GeomType::MultiPoint
            | GeomType::MultiLineString
            | GeomType::MultiPolygon
            | GeomType::GeometryCollection
            | GeomType::Tin
            | GeomType::PolyhedralSurface
    )
}

fn members(g: &Geometry<f64>) -> Vec<Geometry<f64>> {
    use Geometry as G;
    match g {
        G::MultiPoint(m) => m.0.iter().map(|p| G::Point(*p)).collect(),
        G::MultiLineString(m) => m.0.iter().cloned().map(G::LineString).collect(),
        G::MultiPolygon(m) => m.0.iter().cloned().map(G::Polygon).collect(),
        G::GeometryCollection(c) => c.0.clone(),
        other => vec![other.clone()],
    }
}

/// `geof:numGeometries`: the direct members of a collection; 1 for an atomic geometry.
pub fn num_geometries(g: &Geom) -> i64 {
    if is_collection(g.declared) {
        members(&g.g).len() as i64
    } else {
        1
    }
}

/// `geof:geometryN`: member `n` (1-based) of a collection; an atomic geometry is its own
/// first member. Out of range: a type error.
pub fn geometry_n(g: &Geom, n: i64) -> Result<Geom, OpError> {
    let out_of_range = || type_error(format!("geometryN: no member {n}"));
    if !is_collection(g.declared) {
        return if n == 1 {
            Ok(g.clone())
        } else {
            Err(out_of_range())
        };
    }
    let i = usize::try_from(n - 1).map_err(|_| out_of_range())?;
    let m = members(&g.g).into_iter().nth(i).ok_or_else(out_of_range)?;
    let mut out = made(g, m);
    if matches!(g.declared, GeomType::Tin | GeomType::PolyhedralSurface) {
        out.declared = if g.declared == GeomType::Tin {
            GeomType::Triangle
        } else {
            GeomType::Polygon
        };
    }
    Ok(out)
}

/// Which bound `geof:minX` … `geof:maxY` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    MinX,
    MinY,
    MaxX,
    MaxY,
}

/// `geof:minX` … `maxY`, in the literal's own axis order (for a latitude-first CRS, X is
/// the latitude). Empty: a type error.
pub fn bound(g: &Geom, b: Bound) -> Result<f64, OpError> {
    let r = (!g.empty)
        .then(|| g.g.bounding_rect())
        .flatten()
        .ok_or_else(|| type_error("an empty geometry has no coordinates"))?;
    let (min, max) = (r.min(), r.max());
    // internal (east, north) → the literal's axes
    let (x0, y0, x1, y1) = if lat_first(&g.crs) {
        (min.y, min.x, max.y, max.x)
    } else {
        (min.x, min.y, max.x, max.y)
    };
    Ok(match b {
        Bound::MinX => x0,
        Bound::MinY => y0,
        Bound::MaxX => x1,
        Bound::MaxY => y1,
    })
}

/// `geof:minZ` / `geof:maxZ`: a type error without Z values.
pub fn z_bound(g: &Geom, max: bool) -> Result<f64, OpError> {
    let (lo, hi) =
        g.z.ok_or_else(|| type_error("the geometry has no Z coordinates"))?;
    Ok(if max { hi } else { lo })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::crs::{CRS84, CrsRef};
    use wkt::TryFromWkt;

    fn g(s: &str) -> Geom {
        let geometry = Geometry::<f64>::try_from_wkt_str(s).unwrap();
        Geom::from_geometry(CrsRef::Known(CRS84), geometry)
    }

    #[test]
    fn accessors() {
        let ga = g("POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))");
        assert_eq!(bound(&ga, Bound::MaxY).unwrap(), 10.0);
        assert_eq!(bound(&ga, Bound::MinX).unwrap(), 0.0);
        assert!(bound(&g("POLYGON EMPTY"), Bound::MinX).is_err());
        assert_eq!(geometry_type(&ga), format!("{SF}Polygon"));
        assert_eq!(num_geometries(&ga), 1);
        assert!(geometry_n(&ga, 2).is_err());
        assert_eq!(geometry_n(&ga, 1).unwrap().g, ga.g);
        let mp = g("MULTIPOINT((1 1),(2 2))");
        assert_eq!(num_geometries(&mp), 2);
        let second = geometry_n(&mp, 2).unwrap();
        assert_eq!(second.g, Geometry::Point(georust::Point::new(2.0, 2.0)));
        assert_eq!(second.declared, GeomType::Point);
        assert!(geometry_n(&mp, 0).is_err() && geometry_n(&mp, 3).is_err());
        assert_eq!(num_geometries(&g("GEOMETRYCOLLECTION EMPTY")), 0);
        assert!(z_bound(&ga, false).is_err());
        assert_eq!((coordinate_dimension(&ga), spatial_dimension(&ga)), (2, 2));
        assert!(!is_3d(&ga) && !is_measured(&ga));
    }
}
