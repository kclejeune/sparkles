//! The geometry column of a generation: literal id → parsed geometry and its CRS84
//! envelope, for the base literals of indexed predicates and the delta literals the
//! commit path parsed.
//!
//! Stub: entries hold only an envelope.

use super::GeomRef;
use super::geom::GeomError;
use crate::store::Snapshot;

/// A geometry literal of the column.
pub struct ColumnEntry {
    bbox84: [f64; 4],
}

impl ColumnEntry {
    /// The envelope in CRS84, rounded outward to `f32`.
    pub fn bbox84(&self) -> [f64; 4] {
        self.bbox84
    }

    /// The parsed geometry.
    pub fn geom(&self, snap: &Snapshot) -> Result<GeomRef, GeomError> {
        let _ = snap;
        Err(GeomError {
            offset: None,
            msg: "the geometry column is not supported yet".into(),
        })
    }
}
