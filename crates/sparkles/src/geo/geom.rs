//! A parsed geometry literal.
//!
//! Partial: [`Geom::from_geometry`] builds geometries for tests until the parsers land;
//! [`Geom::bbox84`] treats every known CRS as longitude/latitude.

use super::crs::CrsRef;
use georust::dimensions::Dimensions;
use georust::{BoundingRect, CoordsIter, HasDimensions};

/// The geometry type as written (kept for `geof:geometryType`; LINEARRING, TRIANGLE, TIN
/// and POLYHEDRALSURFACE are held as polygons and line strings).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GeomType {
    Point,
    LineString,
    Polygon,
    MultiPoint,
    MultiLineString,
    MultiPolygon,
    GeometryCollection,
    LinearRing,
    Triangle,
    Tin,
    PolyhedralSurface,
}

/// Coordinate layout of the literal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Layout {
    #[default]
    Xy,
    Xyz,
    Xym,
    Xyzm,
}

/// A geometry in internal (east, north) coordinates, with what the literal said about it.
#[derive(Clone, Debug)]
pub struct Geom {
    pub crs: CrsRef,
    pub declared: GeomType,
    pub layout: Layout,
    pub g: georust::Geometry<f64>,
    /// (minZ, maxZ) when the literal has Z values (the values themselves are dropped)
    pub z: Option<(f64, f64)>,
    pub empty: bool,
    pub vertices: u32,
}

/// Why a literal is not a valid geometry; `offset` is the byte offset in the lexical form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeomError {
    pub offset: Option<usize>,
    pub msg: String,
}

impl std::fmt::Display for GeomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.offset {
            Some(o) => write!(f, "at offset {o}: {}", self.msg),
            None => f.write_str(&self.msg),
        }
    }
}

impl std::error::Error for GeomError {}

impl Geom {
    /// A 2D geometry of the given CRS (coordinates already in internal order).
    pub fn from_geometry(crs: CrsRef, g: georust::Geometry<f64>) -> Geom {
        use georust::Geometry as G;
        let declared = match &g {
            G::Point(_) => GeomType::Point,
            G::Line(_) | G::LineString(_) => GeomType::LineString,
            G::Polygon(_) | G::Rect(_) | G::Triangle(_) => GeomType::Polygon,
            G::MultiPoint(_) => GeomType::MultiPoint,
            G::MultiLineString(_) => GeomType::MultiLineString,
            G::MultiPolygon(_) => GeomType::MultiPolygon,
            G::GeometryCollection(_) => GeomType::GeometryCollection,
        };
        Geom {
            crs,
            declared,
            layout: Layout::Xy,
            empty: g.is_empty(),
            vertices: u32::try_from(g.coords_count()).unwrap_or(u32::MAX),
            g,
            z: None,
        }
    }

    /// The envelope in CRS84 (`[min_lon, min_lat, max_lon, max_lat]`); `None` for empty
    /// geometries and CRSs that cannot be transformed.
    pub fn bbox84(&self) -> Option<[f64; 4]> {
        if self.empty {
            return None;
        }
        match &self.crs {
            CrsRef::Known(_) => {
                let r = self.g.bounding_rect()?;
                Some([r.min().x, r.min().y, r.max().x, r.max().y])
            }
            CrsRef::Unknown(_) => None,
        }
    }

    /// Topological dimension: 0, 1 or 2; an empty geometry has its type's dimension, and
    /// an empty collection -1.
    pub fn dim(&self) -> i8 {
        match self.declared {
            GeomType::Point | GeomType::MultiPoint => 0,
            GeomType::LineString | GeomType::MultiLineString | GeomType::LinearRing => 1,
            GeomType::Polygon
            | GeomType::MultiPolygon
            | GeomType::Triangle
            | GeomType::Tin
            | GeomType::PolyhedralSurface => 2,
            GeomType::GeometryCollection => match self.g.dimensions() {
                Dimensions::Empty => -1,
                Dimensions::ZeroDimensional => 0,
                Dimensions::OneDimensional => 1,
                Dimensions::TwoDimensional => 2,
            },
        }
    }
}
