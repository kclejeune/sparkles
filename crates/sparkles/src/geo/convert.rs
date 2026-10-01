//! Geometry literals as CRS84 GeoJSON geometries, for clients that draw result columns
//! (`POST /$/geo/convert`).
//!
//! Not implemented yet: [`convert`] fails with `Error::Unsupported`.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// Most literals in one request.
pub const MAX_ITEMS: usize = 10_000;

/// A literal to convert.
#[derive(Clone, Debug, Deserialize)]
pub struct ConvertItem {
    /// the lexical form
    pub value: String,
    /// the datatype IRI (`geo:wktLiteral`, `geo:geoJSONLiteral`)
    pub datatype: String,
}

/// The outcome for one literal: a GeoJSON geometry in CRS84 (longitude, latitude), or
/// why there is none (the message of a malformed literal, an unknown CRS, …).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Converted {
    Geometry(serde_json::Value),
    Error(String),
}

/// Convert each literal, in order (at most [`MAX_ITEMS`]).
pub fn convert(items: &[ConvertItem]) -> Result<Vec<Converted>> {
    let _ = items;
    Err(Error::Unsupported(
        "geometry conversion is not supported yet".into(),
    ))
}
