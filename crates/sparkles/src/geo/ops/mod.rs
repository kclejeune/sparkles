//! The geometric operations behind the `geof:` and `spatialF:` functions and the
//! GeoSPARQL aggregates, over parsed geometries.

pub mod accessors;
pub mod aeqd;
pub mod construct;
pub mod distance;
pub mod hull;
pub mod measure;
pub mod overlay;
pub mod relate;
pub mod simple;

use super::crs::CrsRef;
use super::geom::Geom;
use std::borrow::Cow;

/// Why an operation has no result. Both are SPARQL type errors for the function call.
#[derive(Clone, Debug, PartialEq)]
pub enum OpError {
    /// the arguments do not fit the operation (types, CRSs, units, …)
    Type(String),
    /// the inputs have more vertices than `maxOpVertices` allows
    TooLarge(u64),
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpError::Type(m) => f.write_str(m),
            OpError::TooLarge(n) => write!(f, "geometry operation too large ({n} vertices)"),
        }
    }
}

impl std::error::Error for OpError {}

pub(crate) fn type_error(msg: impl Into<String>) -> OpError {
    OpError::Type(msg.into())
}

/// Run a computation of the geometry crates, which may panic on degenerate input: a
/// panic becomes a type error.
pub(crate) fn guarded<T>(what: &str, f: impl FnOnce() -> T) -> Result<T, OpError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .map_err(|_| type_error(format!("{what}: the computation failed on this input")))
}

/// Whether coordinates in `crs` are longitude/latitude (`None`: a CRS this build does
/// not know, whose units are unknown).
pub fn is_geographic(crs: &CrsRef) -> Option<bool> {
    crs.known().map(|id| id.is_geographic())
}

/// `g` in the CRS `to`: the geometry itself when no coordinate changes (the same CRS,
/// or two geographic CRSs, whose internal coordinates agree), else transformed; a type
/// error when no transform exists (an unknown CRS on either side, or a coordinate
/// outside the target's domain).
pub fn in_crs<'a>(g: &'a Geom, to: &CrsRef) -> Result<Cow<'a, Geom>, OpError> {
    if &g.crs == to {
        return Ok(Cow::Borrowed(g));
    }
    let no_transform = || {
        type_error(format!(
            "no transform from <{}> to <{}>",
            g.crs.iri(),
            to.iri()
        ))
    };
    match (g.crs.known(), to.known()) {
        (Some(a), Some(b)) if a.is_geographic() && b.is_geographic() => Ok(Cow::Borrowed(g)),
        (Some(_), Some(b)) => g.transformed(b).map(Cow::Owned).ok_or_else(no_transform),
        _ => Err(no_transform()),
    }
}

/// `g` in the CRS `to` (labelled with it).
pub fn transform(g: &Geom, to: &CrsRef) -> Result<Geom, OpError> {
    let mut out = in_crs(g, to)?.into_owned();
    out.crs = to.clone();
    Ok(out)
}

/// A constructed geometry in the CRS of `like`.
pub(crate) fn made(like: &Geom, g: georust::Geometry<f64>) -> Geom {
    Geom::from_geometry(like.crs.clone(), g)
}

/// A WKT literal, for tests.
#[cfg(test)]
pub(crate) fn wkt(s: &str) -> Geom {
    super::parse::parse(s, super::vocab::WKT_LITERAL).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::crs::{CRS84, EPSG_4326, WEB_MERCATOR};

    #[test]
    fn transforms() {
        let p = wkt("<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)");
        // geographic CRSs share internal coordinates
        let q = transform(&p, &CrsRef::Known(CRS84)).unwrap();
        assert_eq!((&q.crs, &q.g), (&CrsRef::Known(CRS84), &p.g));
        assert!(matches!(
            in_crs(&p, &CrsRef::Known(CRS84)),
            Ok(Cow::Borrowed(_))
        ));
        let m = transform(&p, &CrsRef::Known(WEB_MERCATOR)).unwrap();
        let back = transform(&m, &CrsRef::Known(EPSG_4326)).unwrap();
        let (georust::Geometry::Point(a), georust::Geometry::Point(b)) = (&p.g, &back.g) else {
            panic!("{:?}", back.g)
        };
        assert!((a.x() - b.x()).abs() < 1e-9 && (a.y() - b.y()).abs() < 1e-9);
        let mars = wkt("<http://example.org/crs/mars> POINT(1 1)");
        assert!(in_crs(&mars, &CrsRef::Known(CRS84)).is_err());
        assert!(in_crs(&p, &mars.crs).is_err());
        assert!(in_crs(&mars, &mars.crs.clone()).is_ok());
        // a pole has no Web Mercator coordinates
        assert!(transform(&wkt("POINT(0 90)"), &CrsRef::Known(WEB_MERCATOR)).is_err());
    }

    #[test]
    fn panics_become_type_errors() {
        let r: Result<(), OpError> = guarded("test", || panic!("degenerate"));
        assert!(matches!(r, Err(OpError::Type(_))));
        assert_eq!(
            OpError::TooLarge(1100).to_string(),
            "geometry operation too large (1100 vertices)"
        );
    }
}
