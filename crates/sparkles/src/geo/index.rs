//! The spatial index: a packed R-tree over a generation's base rows, an overlay of rows
//! inserted by transactions since, and the view a snapshot holds of both.
//!
//! Stub: there is never an index; views are not ready.

use super::config::{GeoConfig, IndexState};
use crate::id::Id;
use std::sync::Arc;

/// The spatial data of one generation (geometry column, base tree), dropped with it.
#[derive(Default)]
pub struct GenerationGeo {}

/// A dataset's spatial index (the store holds one while the index is enabled).
pub struct GeoIndex {}

/// The spatial index as one snapshot sees it (immutable).
#[derive(Clone)]
pub struct GeoView {
    pub config: Arc<GeoConfig>,
    /// +1 per enable, reconfiguration or rebuild (part of result-cache keys)
    pub epoch: u64,
    /// the `uid` of the generation the base belongs to
    pub generation: u64,
    /// the commit of the snapshot
    pub commit: u64,
}

impl GeoView {
    /// Whether (and why not) queries on this view can use the index.
    pub fn state(&self) -> IndexState {
        IndexState::Off
    }

    /// The slot of an indexed predicate (`None`: the predicate is not indexed).
    pub fn predicate_slot(&self, p: Id) -> Option<u16> {
        let _ = p;
        None
    }

    /// Estimated rows of the predicates `preds` whose envelope intersects one of the
    /// CRS84 `windows`.
    pub fn estimate(&self, preds: &[u16], windows: &[[f64; 4]]) -> f64 {
        let _ = (preds, windows);
        0.0
    }

    /// Height of the base tree (for costs).
    pub fn levels(&self) -> u32 {
        0
    }

    /// The view of a transaction's uncommitted changes, which the index does not cover.
    pub fn for_txn(&self) -> GeoView {
        self.clone()
    }
}
