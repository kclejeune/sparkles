//! Coordinate reference systems: the built-in table, IRI aliases and transforms between
//! the built-in CRSs. Pure data, compiled without the `geo` feature too.
//!
//! Geometries are held in internal (east, north) order: (longitude, latitude) in degrees
//! for the geographic CRSs, (easting, northing) in metres for the projected ones. A
//! literal in a latitude-first CRS (EPSG:4326, EPSG:4979) is swapped on parse and
//! swapped back when written, so transforms between the geographic CRSs (all on WGS 84)
//! leave internal coordinates unchanged.

use std::borrow::Cow;
use std::sync::Arc;

/// The default CRS of WKT literals without a CRS IRI (longitude, latitude on WGS 84).
pub const CRS84_IRI: &str = "http://www.opengis.net/def/crs/OGC/1.3/CRS84";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CrsKind {
    Geographic2D,
    Geographic3D,
    Projected,
}

/// A built-in CRS: an index into the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CrsId(pub u8);

pub const CRS84: CrsId = CrsId(0);
/// CRS84 with ellipsoidal height
pub const CRS84H: CrsId = CrsId(1);
/// EPSG:4326, latitude first
pub const EPSG_4326: CrsId = CrsId(2);
/// EPSG:4979, latitude first, with ellipsoidal height
pub const EPSG_4979: CrsId = CrsId(3);
/// `http://www.opengis.net/def/crs/EPSG/4326` (no version): GeoSPARQL 1.0 examples use
/// it with longitude first, and Jena reads it as CRS84
pub const EPSG_4326_LEGACY: CrsId = CrsId(4);
/// EPSG:3857, spherical Web Mercator
pub const WEB_MERCATOR: CrsId = CrsId(5);

/// How a CRS maps to longitude/latitude on WGS 84.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Projection {
    /// geographic: coordinates are longitude/latitude
    None,
    /// spherical Mercator on the WGS 84 semi-major axis
    WebMercator,
}

struct CrsDef {
    iri: &'static str,
    kind: CrsKind,
    /// the literal's first axis is northing (latitude)
    lat_first: bool,
    projection: Projection,
}

const TABLE: [CrsDef; 6] = [
    CrsDef {
        iri: CRS84_IRI,
        kind: CrsKind::Geographic2D,
        lat_first: false,
        projection: Projection::None,
    },
    CrsDef {
        iri: "http://www.opengis.net/def/crs/OGC/0/CRS84h",
        kind: CrsKind::Geographic3D,
        lat_first: false,
        projection: Projection::None,
    },
    CrsDef {
        iri: "http://www.opengis.net/def/crs/EPSG/0/4326",
        kind: CrsKind::Geographic2D,
        lat_first: true,
        projection: Projection::None,
    },
    CrsDef {
        iri: "http://www.opengis.net/def/crs/EPSG/0/4979",
        kind: CrsKind::Geographic3D,
        lat_first: true,
        projection: Projection::None,
    },
    CrsDef {
        iri: "http://www.opengis.net/def/crs/EPSG/4326",
        kind: CrsKind::Geographic2D,
        lat_first: false,
        projection: Projection::None,
    },
    CrsDef {
        iri: "http://www.opengis.net/def/crs/EPSG/0/3857",
        kind: CrsKind::Projected,
        lat_first: false,
        projection: Projection::WebMercator,
    },
];

/// Semi-major axis of WGS 84, the sphere radius of Web Mercator.
const WGS84_A: f64 = 6_378_137.0;

impl CrsId {
    fn def(self) -> &'static CrsDef {
        &TABLE[usize::from(self.0)]
    }

    /// The canonical IRI.
    pub fn iri(self) -> &'static str {
        self.def().iri
    }

    pub fn kind(self) -> CrsKind {
        self.def().kind
    }

    /// Longitude/latitude (degrees), as opposed to projected coordinates (metres).
    pub fn is_geographic(self) -> bool {
        self.kind() != CrsKind::Projected
    }

    /// The literal writes northing (latitude) first; internal coordinates are swapped.
    pub fn lat_first(self) -> bool {
        self.def().lat_first
    }

    /// Every built-in CRS.
    pub fn all() -> impl Iterator<Item = CrsId> {
        (0..TABLE.len() as u8).map(CrsId)
    }
}

/// The CRS of a geometry: a built-in one, or an IRI this build does not know (valid,
/// but only planar operations between geometries of that same CRS work).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CrsRef {
    Known(CrsId),
    Unknown(Arc<str>),
}

impl CrsRef {
    /// The IRI: the canonical one of a built-in CRS, else as written.
    pub fn iri(&self) -> &str {
        match self {
            CrsRef::Known(id) => id.iri(),
            CrsRef::Unknown(iri) => iri,
        }
    }

    pub fn known(&self) -> Option<CrsId> {
        match self {
            CrsRef::Known(id) => Some(*id),
            CrsRef::Unknown(_) => None,
        }
    }

    /// The CRS a literal names (`None`: no IRI, CRS84).
    pub fn from_iri(iri: Option<&str>) -> CrsRef {
        match iri {
            None => CrsRef::Known(CRS84),
            Some(iri) => match lookup(iri) {
                Some(id) => CrsRef::Known(id),
                None => CrsRef::Unknown(iri.into()),
            },
        }
    }
}

/// The built-in CRS an IRI (or one of its aliases) names.
pub fn lookup(iri: &str) -> Option<CrsId> {
    let n = normalize(iri);
    CrsId::all().find(|id| id.iri() == n)
}

/// The `http://www.opengis.net/def/crs/…` form of a CRS IRI: `https` → `http`, OGC URNs
/// (`urn:ogc:def:crs:EPSG::4326`) and short forms (`EPSG:4326`, `CRS:84`) → the
/// `http` form, any EPSG version → `0`, Web Mercator's old code 900913 → 3857. The
/// legacy `…/EPSG/4326` (no version) is a CRS of its own and stays.
pub fn normalize(iri: &str) -> Cow<'_, str> {
    const DEF: &str = "http://www.opengis.net/def/crs/";
    let iri = iri.trim();
    let rest = if let Some(r) = iri.strip_prefix(DEF) {
        Some(r)
    } else {
        iri.strip_prefix("https://www.opengis.net/def/crs/")
    };
    let (authority, version, code): (String, &str, &str) = if let Some(rest) = rest {
        let parts: Vec<&str> = rest.split('/').collect();
        match parts.as_slice() {
            [auth, ver, code] => (auth.to_string(), ver, code),
            // the legacy EPSG form without a version
            [auth, code] if auth.eq_ignore_ascii_case("EPSG") => {
                return Cow::Owned(format!("{DEF}EPSG/{code}"));
            }
            _ => return Cow::Borrowed(iri),
        }
    } else if let Some(rest) = strip_prefix_ci(iri, "urn:ogc:def:crs:") {
        // urn:ogc:def:crs:{authority}:{version}:{code}
        let parts: Vec<&str> = rest.split(':').collect();
        match parts.as_slice() {
            [auth, ver, code] => (auth.to_ascii_uppercase(), ver, code),
            _ => return Cow::Borrowed(iri),
        }
    } else if let Some(code) = strip_prefix_ci(iri, "EPSG:") {
        ("EPSG".into(), "0", code)
    } else if iri.eq_ignore_ascii_case("CRS:84") {
        ("OGC".into(), "1.3", "CRS84")
    } else {
        return Cow::Borrowed(iri);
    };
    match authority.as_str() {
        "EPSG" => {
            let code = if code == "900913" { "3857" } else { code };
            Cow::Owned(format!("{DEF}EPSG/0/{code}"))
        }
        "OGC" => {
            // CRS84 is defined in OGC version 1.3, CRS84h in version 0
            let (version, code) = if code.eq_ignore_ascii_case("CRS84") {
                ("1.3", "CRS84")
            } else if code.eq_ignore_ascii_case("CRS84h") {
                ("0", "CRS84h")
            } else if version.is_empty() {
                ("0", code)
            } else {
                (version, code)
            };
            Cow::Owned(format!("{DEF}OGC/{version}/{code}"))
        }
        _ => Cow::Borrowed(iri),
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Internal coordinates of `crs` → (longitude, latitude) in degrees (`None`: not finite).
pub fn to_lonlat(crs: CrsId, x: f64, y: f64) -> Option<(f64, f64)> {
    let (lon, lat) = match crs.def().projection {
        Projection::None => (x, y),
        Projection::WebMercator => (
            (x / WGS84_A).to_degrees(),
            (y / WGS84_A).sinh().atan().to_degrees(),
        ),
    };
    (lon.is_finite() && lat.is_finite()).then_some((lon, lat))
}

/// (longitude, latitude) in degrees → internal coordinates of `crs` (`None`: outside
/// the projection's domain, such as a pole in Web Mercator).
pub fn from_lonlat(crs: CrsId, lon: f64, lat: f64) -> Option<(f64, f64)> {
    let (x, y) = match crs.def().projection {
        Projection::None => (lon, lat),
        Projection::WebMercator => {
            if lat.abs() >= 90.0 {
                return None;
            }
            (
                WGS84_A * lon.to_radians(),
                // = R ln tan(π/4 + φ/2), exact at the equator
                WGS84_A * lat.to_radians().tan().asinh(),
            )
        }
    };
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

/// Internal coordinates of `from` → internal coordinates of `to`.
pub fn transform(from: CrsId, to: CrsId, x: f64, y: f64) -> Option<(f64, f64)> {
    if from.def().projection == to.def().projection {
        return Some((x, y));
    }
    let (lon, lat) = to_lonlat(from, x, y)?;
    from_lonlat(to, lon, lat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases() {
        let epsg = "http://www.opengis.net/def/crs/EPSG/0/4326";
        for a in [
            epsg,
            "https://www.opengis.net/def/crs/EPSG/0/4326",
            "urn:ogc:def:crs:EPSG::4326",
            "urn:ogc:def:crs:EPSG:6.6:4326",
            "URN:OGC:DEF:CRS:epsg::4326",
            "EPSG:4326",
            "http://www.opengis.net/def/crs/EPSG/9.9.1/4326",
        ] {
            assert_eq!(lookup(a), Some(EPSG_4326), "{a}");
        }
        for a in [
            CRS84_IRI,
            "https://www.opengis.net/def/crs/OGC/1.3/CRS84",
            "urn:ogc:def:crs:OGC:1.3:CRS84",
            "urn:ogc:def:crs:OGC::CRS84",
            "http://www.opengis.net/def/crs/OGC/0/CRS84",
            "CRS:84",
        ] {
            assert_eq!(lookup(a), Some(CRS84), "{a}");
        }
        assert_eq!(lookup("urn:ogc:def:crs:OGC::CRS84h"), Some(CRS84H));
        assert_eq!(lookup("EPSG:4979"), Some(EPSG_4979));
        assert_eq!(
            lookup("http://www.opengis.net/def/crs/EPSG/4326"),
            Some(EPSG_4326_LEGACY)
        );
        assert!(!EPSG_4326_LEGACY.lat_first());
        for a in [
            "http://www.opengis.net/def/crs/EPSG/0/3857",
            "http://www.opengis.net/def/crs/EPSG/0/900913",
            "EPSG:900913",
            "urn:ogc:def:crs:EPSG::3857",
        ] {
            assert_eq!(lookup(a), Some(WEB_MERCATOR), "{a}");
        }
        assert_eq!(lookup("http://example.org/crs/mars"), None);
        assert_eq!(lookup("http://www.opengis.net/def/crs/EPSG/0/27700"), None);
        assert_eq!(lookup("EPSG"), None);
    }

    #[test]
    fn table() {
        assert_eq!(CRS84.iri(), CRS84_IRI);
        assert!(EPSG_4326.lat_first() && EPSG_4979.lat_first() && !CRS84.lat_first());
        assert_eq!(EPSG_4979.kind(), CrsKind::Geographic3D);
        assert!(!WEB_MERCATOR.is_geographic());
        for id in CrsId::all() {
            assert_eq!(lookup(id.iri()), Some(id));
        }
        assert_eq!(
            CrsRef::from_iri(Some("http://example.org/crs/mars")).iri(),
            "http://example.org/crs/mars"
        );
        assert_eq!(CrsRef::from_iri(None), CrsRef::Known(CRS84));
    }

    #[test]
    fn web_mercator() {
        // the half circumference of the WGS 84 equator, and the latitude where the
        // square Web Mercator world ends
        let (x, y) = from_lonlat(WEB_MERCATOR, 180.0, 0.0).unwrap();
        assert!((x - 20_037_508.342_789_244).abs() < 1e-6 && y.abs() < 1e-9);
        let (_, y) = from_lonlat(WEB_MERCATOR, 0.0, 85.051_128_779_806_59).unwrap();
        assert!((y - 20_037_508.342_789_244).abs() < 1e-3, "{y}");
        let (lon, lat) = to_lonlat(WEB_MERCATOR, x, y).unwrap();
        assert!((lon - 180.0).abs() < 1e-9 && (lat - 85.051_128_779_806_59).abs() < 1e-9);
        assert!(from_lonlat(WEB_MERCATOR, 0.0, 90.0).is_none());
        let (x, y) = transform(EPSG_4326, WEB_MERCATOR, 2.3522, 48.8566).unwrap();
        let (lon, lat) = transform(WEB_MERCATOR, CRS84, x, y).unwrap();
        assert!((lon - 2.3522).abs() < 1e-9 && (lat - 48.8566).abs() < 1e-9);
        assert_eq!(transform(EPSG_4326, CRS84, 1.0, 2.0), Some((1.0, 2.0)));
    }
}
