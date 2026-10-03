//! Accessors of a geometry (`geof:dimension`, `geometryType`, `numGeometries`,
//! `geometryN`, `minX` … `maxZ`, …).

use super::{OpError, made, type_error};
use crate::geo::geom::{Geom, GeomType};
use crate::geo::vocab::SF;
use georust::Geometry;

/// `geof:coordinateDimension`: 2, 3 (XYZ or XYM) or 4.
pub fn coordinate_dimension(g: &Geom) -> i64 {
    g.layout.ordinates() as i64
}

/// `geof:spatialDimension`: 2, or 3 with Z.
pub fn spatial_dimension(g: &Geom) -> i64 {
    if g.layout.has_z() { 3 } else { 2 }
}

/// `geof:geometryType`: the `sf:` IRI of the type as written.
pub fn geometry_type(g: &Geom) -> String {
    format!("{SF}{}", g.declared.sf_name())
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
    match g.declared {
        GeomType::Tin => out.declared = GeomType::Triangle,
        GeomType::PolyhedralSurface => out.declared = GeomType::Polygon,
        _ => {}
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
    let [x0, y0, x1, y1] = g
        .bbox_own_axes()
        .ok_or_else(|| type_error("an empty geometry has no coordinates"))?;
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

    fn g(s: &str) -> Geom {
        crate::geo::ops::wkt(s)
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
        let z = g("LINESTRING Z (0 0 5, 1 1 7)");
        assert_eq!((coordinate_dimension(&z), spatial_dimension(&z)), (3, 3));
        assert_eq!((z_bound(&z, false), z_bound(&z, true)), (Ok(5.0), Ok(7.0)));
        let m = g("POINT ZM (1 2 3 4)");
        assert_eq!((coordinate_dimension(&m), spatial_dimension(&m)), (4, 3));
    }

    #[test]
    fn own_axis_order() {
        // EPSG:4326 writes latitude first: x is the latitude
        let p = g("<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)");
        assert_eq!(bound(&p, Bound::MinX).unwrap(), 2.0);
        assert_eq!(bound(&p, Bound::MaxY).unwrap(), 12.0);
        let tin = g("TIN (((0 0, 1 0, 0 1, 0 0)), ((1 0, 1 1, 0 1, 1 0)))");
        assert_eq!(geometry_type(&tin), format!("{SF}TIN"));
        assert_eq!(num_geometries(&tin), 2);
        assert_eq!(geometry_n(&tin, 2).unwrap().declared, GeomType::Triangle);
    }
}
