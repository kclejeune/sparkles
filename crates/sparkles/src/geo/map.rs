//! Indexed geometries in a CRS84 box as a GeoJSON `FeatureCollection`, for map views
//! (`GET /{ds}/geo`): one feature per indexed row, simplified for the box's scale.
//!
//! Not implemented yet: [`features_in_box`] fails with `Error::Unsupported`.

use crate::error::{Error, Result};
use crate::store::Snapshot;

/// Default and largest number of features of one answer.
pub const DEFAULT_LIMIT: usize = 5_000;
pub const MAX_LIMIT: usize = 50_000;

/// What `GET /{ds}/geo` asks for.
#[derive(Clone, Debug)]
pub struct BoxQuery {
    /// `[minLon, minLat, maxLon, maxLat]` in CRS84 degrees
    pub bbox: [f64; 4],
    /// only rows of this graph (an IRI)
    pub graph: Option<String>,
    /// only rows of this serialization predicate (an IRI)
    pub predicate: Option<String>,
    /// at most this many features (`truncated: true` when there are more)
    pub limit: usize,
    /// Douglas–Peucker tolerance in degrees (`None`: the box's width / 1024)
    pub tolerance: Option<f64>,
}

/// The `FeatureCollection` of `snap`'s indexed geometries meeting `q.bbox`: `id` and
/// `properties.subject` the row's subject, `properties.feature` a feature linked to it,
/// `properties.graph` and `properties.predicate`, and a top-level `truncated`.
pub fn features_in_box(snap: &Snapshot, q: &BoxQuery) -> Result<serde_json::Value> {
    let _ = (snap, q);
    Err(Error::Unsupported(
        "the map view is not supported yet".into(),
    ))
}
