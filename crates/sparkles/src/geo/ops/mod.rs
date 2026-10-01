//! The geometric operations behind the `geof:` functions, over parsed geometries.

pub mod accessors;
pub mod aeqd;
pub mod construct;
pub mod distance;
pub mod measure;
pub mod overlay;
pub mod relate;

use super::crs::{CRS84, CrsId, CrsRef};
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

/// The answer of an operation that is not implemented yet.
#[allow(dead_code)]
pub(crate) fn not_yet() -> OpError {
    OpError::Type("not supported yet".into())
}

pub(crate) fn type_error(msg: impl Into<String>) -> OpError {
    OpError::Type(msg.into())
}

/// Default of the largest sum of input vertices of one operation
/// (`StoreOptions::geo_op_vertices`).
pub const DEFAULT_OP_VERTICES: u64 = 2_000_000;

/// Refuse an operation whose inputs have more than `limit` vertices together.
pub fn check_vertices(limit: u64, gs: &[&Geom]) -> Result<(), OpError> {
    let n: u64 = gs.iter().map(|g| u64::from(g.vertices)).sum();
    if n > limit {
        return Err(OpError::TooLarge(n));
    }
    Ok(())
}

/// Run a computation of the geometry crates, which may panic on degenerate input: a
/// panic becomes a type error.
pub(crate) fn guarded<T>(what: &str, f: impl FnOnce() -> T) -> Result<T, OpError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .map_err(|_| type_error(format!("{what}: the computation failed on this input")))
}

/// Whether a built-in CRS is geographic (longitude/latitude in degrees on WGS 84).
pub(crate) fn geographic(id: CrsId) -> bool {
    id == CRS84
}

/// Whether coordinates in `crs` are longitude/latitude (`None`: a CRS this build does
/// not know, whose units are unknown).
pub fn is_geographic(crs: &CrsRef) -> Option<bool> {
    match crs {
        CrsRef::Known(id) => Some(geographic(*id)),
        CrsRef::Unknown(_) => None,
    }
}

/// `g` in the CRS `to`: the geometry itself when no coordinate changes (the same CRS,
/// or two geographic CRSs, whose internal coordinates agree); a type error when no
/// transform exists (an unknown CRS on either side, or a pair this build cannot
/// convert).
pub fn in_crs<'a>(g: &'a Geom, to: &CrsRef) -> Result<Cow<'a, Geom>, OpError> {
    if &g.crs == to {
        return Ok(Cow::Borrowed(g));
    }
    match (&g.crs, to) {
        (CrsRef::Known(a), CrsRef::Known(b)) if geographic(*a) && geographic(*b) => {
            Ok(Cow::Borrowed(g))
        }
        _ => Err(type_error(format!(
            "no transform from <{}> to <{}>",
            crs_iri(&g.crs),
            crs_iri(to)
        ))),
    }
}

fn crs_iri(c: &CrsRef) -> &str {
    match c {
        CrsRef::Known(_) => super::crs::CRS84_IRI,
        CrsRef::Unknown(iri) => iri,
    }
}
