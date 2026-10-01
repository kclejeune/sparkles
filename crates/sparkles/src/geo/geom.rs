//! A parsed geometry literal.

use super::crs::{self, CrsId, CrsRef};
use georust::dimensions::Dimensions;
use georust::{BoundingRect, CoordsIter, HasDimensions, MapCoords};

/// The geometry type as written (kept for `geof:geometryType`; LINEARRING is held as a
/// line string, TRIANGLE as a polygon, TIN and POLYHEDRALSURFACE as multipolygons).
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

impl GeomType {
    /// The WKT keyword.
    pub fn wkt_name(self) -> &'static str {
        match self {
            GeomType::Point => "POINT",
            GeomType::LineString => "LINESTRING",
            GeomType::Polygon => "POLYGON",
            GeomType::MultiPoint => "MULTIPOINT",
            GeomType::MultiLineString => "MULTILINESTRING",
            GeomType::MultiPolygon => "MULTIPOLYGON",
            GeomType::GeometryCollection => "GEOMETRYCOLLECTION",
            GeomType::LinearRing => "LINEARRING",
            GeomType::Triangle => "TRIANGLE",
            GeomType::Tin => "TIN",
            GeomType::PolyhedralSurface => "POLYHEDRALSURFACE",
        }
    }

    /// The local name in the Simple Features vocabulary (`sf:`), for `geof:geometryType`.
    pub fn sf_name(self) -> &'static str {
        match self {
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
        }
    }

    /// The type a `geo` geometry has when nothing else is known.
    pub fn of(g: &georust::Geometry<f64>) -> GeomType {
        use georust::Geometry as G;
        match g {
            G::Point(_) => GeomType::Point,
            G::Line(_) | G::LineString(_) => GeomType::LineString,
            G::Polygon(_) | G::Rect(_) | G::Triangle(_) => GeomType::Polygon,
            G::MultiPoint(_) => GeomType::MultiPoint,
            G::MultiLineString(_) => GeomType::MultiLineString,
            G::MultiPolygon(_) => GeomType::MultiPolygon,
            G::GeometryCollection(_) => GeomType::GeometryCollection,
        }
    }
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

impl Layout {
    /// Ordinates per coordinate.
    pub fn ordinates(self) -> usize {
        match self {
            Layout::Xy => 2,
            Layout::Xyz | Layout::Xym => 3,
            Layout::Xyzm => 4,
        }
    }

    pub fn has_z(self) -> bool {
        matches!(self, Layout::Xyz | Layout::Xyzm)
    }

    pub fn has_m(self) -> bool {
        matches!(self, Layout::Xym | Layout::Xyzm)
    }
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

impl GeomError {
    pub fn new(msg: impl Into<String>) -> GeomError {
        GeomError {
            offset: None,
            msg: msg.into(),
        }
    }

    pub fn at(offset: usize, msg: impl Into<String>) -> GeomError {
        GeomError {
            offset: Some(offset),
            msg: msg.into(),
        }
    }
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
        Geom {
            crs,
            declared: GeomType::of(&g),
            layout: Layout::Xy,
            empty: g.coords_count() == 0,
            vertices: u32::try_from(g.coords_count()).unwrap_or(u32::MAX),
            g,
            z: None,
        }
    }

    /// An empty geometry of type `declared` (`POINT EMPTY` is held as an empty
    /// multipoint: `geo` has no empty point).
    pub fn empty(crs: CrsRef, declared: GeomType) -> Geom {
        use georust::{
            Geometry, GeometryCollection, LineString, MultiLineString, MultiPoint, MultiPolygon,
        };
        let g = match declared {
            GeomType::LineString | GeomType::LinearRing => LineString::new(vec![]).into(),
            GeomType::Polygon | GeomType::Triangle => {
                georust::Polygon::new(LineString::new(vec![]), vec![]).into()
            }
            GeomType::MultiLineString => MultiLineString::new(vec![]).into(),
            GeomType::MultiPolygon | GeomType::Tin | GeomType::PolyhedralSurface => {
                MultiPolygon::new(vec![]).into()
            }
            GeomType::GeometryCollection => {
                Geometry::GeometryCollection(GeometryCollection::new_from(vec![]))
            }
            GeomType::Point | GeomType::MultiPoint => MultiPoint::new(vec![]).into(),
        };
        Geom {
            crs,
            declared,
            layout: Layout::Xy,
            g,
            z: None,
            empty: true,
            vertices: 0,
        }
    }

    /// The CRS IRI (canonical for a built-in CRS, else as written).
    pub fn crs_iri(&self) -> &str {
        self.crs.iri()
    }

    /// The envelope in internal coordinates (`[min_x, min_y, max_x, max_y]`).
    pub fn bbox(&self) -> Option<[f64; 4]> {
        if self.empty {
            return None;
        }
        let r = self.g.bounding_rect()?;
        Some([r.min().x, r.min().y, r.max().x, r.max().y])
    }

    /// The envelope in the literal's own axis order (for EPSG:4326, x is latitude), as
    /// `geof:minX` and friends report it.
    pub fn bbox_own_axes(&self) -> Option<[f64; 4]> {
        let b = self.bbox()?;
        Some(match self.crs.known() {
            Some(id) if id.lat_first() => [b[1], b[0], b[3], b[2]],
            _ => b,
        })
    }

    /// The envelope in CRS84 (`[min_lon, min_lat, max_lon, max_lat]`); `None` for empty
    /// geometries and CRSs that cannot be transformed.
    pub fn bbox84(&self) -> Option<[f64; 4]> {
        let b = self.bbox()?;
        let id = self.crs.known()?;
        if id.is_geographic() {
            return Some(b);
        }
        // the built-in projections map each axis monotonically
        let (x0, y0) = crs::to_lonlat(id, b[0], b[1])?;
        let (x1, y1) = crs::to_lonlat(id, b[2], b[3])?;
        Some([x0, y0, x1, y1])
    }

    /// This geometry in the built-in CRS `to` (`None`: no common built-in CRS, or a
    /// coordinate outside the target's domain).
    pub fn transformed(&self, to: CrsId) -> Option<Geom> {
        let from = self.crs.known()?;
        if from == to {
            return Some(self.clone());
        }
        let g = self.g.try_map_coords(|c| {
            crs::transform(from, to, c.x, c.y)
                .map(|(x, y)| georust::Coord { x, y })
                .ok_or(())
        });
        Some(Geom {
            crs: CrsRef::Known(to),
            g: g.ok()?,
            ..self.clone()
        })
    }

    /// This geometry in the CRS of `other` (`None`: no common built-in CRS). Geometries
    /// of the same unknown CRS need no transform.
    pub fn in_crs_of(&self, other: &Geom) -> Option<std::borrow::Cow<'_, Geom>> {
        use std::borrow::Cow;
        if self.crs == other.crs {
            return Some(Cow::Borrowed(self));
        }
        self.transformed(other.crs.known()?).map(Cow::Owned)
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

    /// Estimated heap bytes (for memory budgets).
    pub fn mem_size(&self) -> usize {
        16 * self.vertices as usize + 160
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::crs::{CRS84, EPSG_4326, WEB_MERCATOR};
    use crate::geo::parse::parse;
    use crate::geo::vocab::WKT_LITERAL;

    #[test]
    fn envelopes() {
        let g = parse(
            "<http://www.opengis.net/def/crs/EPSG/0/4326> LINESTRING(2 12, 4 13)",
            WKT_LITERAL,
        )
        .unwrap();
        // internally (lon, lat)
        assert_eq!(g.bbox(), Some([12.0, 2.0, 13.0, 4.0]));
        assert_eq!(g.bbox84(), Some([12.0, 2.0, 13.0, 4.0]));
        // geof:minX reports the literal's own first axis: latitude
        assert_eq!(g.bbox_own_axes(), Some([2.0, 12.0, 4.0, 13.0]));
        let mars = parse("<http://example.org/crs/mars> POINT(1 1)", WKT_LITERAL).unwrap();
        assert_eq!(mars.bbox84(), None);
        assert_eq!(mars.bbox_own_axes(), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(parse("POINT EMPTY", WKT_LITERAL).unwrap().bbox84(), None);
    }

    #[test]
    fn projected_envelope_in_crs84() {
        let g = parse(
            "<http://www.opengis.net/def/crs/EPSG/0/3857> LINESTRING(0 0, 20037508.342789244 20037508.342789244)",
            WKT_LITERAL,
        )
        .unwrap();
        let b = g.bbox84().unwrap();
        assert!(b[0].abs() < 1e-9 && b[1].abs() < 1e-9);
        assert!((b[2] - 180.0).abs() < 1e-9 && (b[3] - 85.051_128_779_806_59).abs() < 1e-9);
    }

    #[test]
    fn transforms() {
        let g = parse(
            "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)",
            WKT_LITERAL,
        )
        .unwrap();
        let t = g.transformed(CRS84).unwrap();
        assert_eq!(t.crs, CrsRef::Known(CRS84));
        assert_eq!(t.bbox(), g.bbox());
        let m = g.transformed(WEB_MERCATOR).unwrap();
        let back = m.transformed(EPSG_4326).unwrap();
        let (a, b) = (g.bbox().unwrap(), back.bbox().unwrap());
        assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9);
        let pole = parse("POINT(0 90)", WKT_LITERAL).unwrap();
        assert!(pole.transformed(WEB_MERCATOR).is_none());
        let mars = parse("<http://example.org/crs/mars> POINT(1 1)", WKT_LITERAL).unwrap();
        assert!(mars.transformed(CRS84).is_none());
        assert!(mars.in_crs_of(&mars).is_some());
        assert!(mars.in_crs_of(&pole).is_none());
        assert!(g.in_crs_of(&pole).is_some());
    }

    #[test]
    fn empty_and_dimensions() {
        for (t, d) in [
            (GeomType::Point, 0),
            (GeomType::LineString, 1),
            (GeomType::Polygon, 2),
            (GeomType::GeometryCollection, -1),
        ] {
            let g = Geom::empty(CrsRef::Known(CRS84), t);
            assert!(g.empty && g.g.is_empty());
            assert_eq!(g.dim(), d);
        }
    }
}
