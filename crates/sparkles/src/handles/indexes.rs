//! The full-text, vector and spatial indexes.
//!
//! The vector and spatial index builds run in the store's background threads, so their
//! `put`, `enable` and `rebuild` return once the build starts, and `wait` blocks until it
//! ends. The full-text index builds in the call.

use crate::Dataset;
use crate::error::Result;
use crate::geo::{GeoConfig, GeoStatus};
use crate::text::{TextConfig, TextStatus};
use crate::vector::config::VectorRecall;
use crate::vector::embed::EmbeddingStatus;
use crate::vector::{VectorIndexConfig, VectorIndexStatus};
use std::time::Duration;

/// The dataset's indexes (from [`Dataset::indexes`]).
#[derive(Clone)]
pub struct Indexes {
    pub(crate) ds: Dataset,
}

impl Indexes {
    /// The full-text index (feature `text`; without it the calls fail with
    /// `Unsupported`).
    pub fn text(&self) -> TextIndex {
        TextIndex {
            ds: self.ds.clone(),
        }
    }

    /// The vector indexes.
    pub fn vector(&self) -> VectorIndexes {
        VectorIndexes {
            ds: self.ds.clone(),
        }
    }

    /// The spatial index (feature `geo`; without it the calls fail with
    /// `Unsupported`).
    pub fn geo(&self) -> GeoIndex {
        GeoIndex {
            ds: self.ds.clone(),
        }
    }
}

#[cfg(not(feature = "text"))]
fn no_text() -> crate::Error {
    crate::Error::unsupported("full-text search needs the `text` feature")
}

/// The full-text index.
#[derive(Clone)]
pub struct TextIndex {
    #[cfg_attr(not(feature = "text"), allow(dead_code))]
    ds: Dataset,
}

impl TextIndex {
    /// Its status, or `None` when it is not enabled.
    pub fn status(&self) -> Option<TextStatus> {
        #[cfg(feature = "text")]
        return self.ds.store().text_status();
        #[cfg(not(feature = "text"))]
        None
    }

    /// Enable (or reconfigure) the index and build it from the current state.
    pub fn enable(&self, cfg: TextConfig) -> Result<TextStatus> {
        #[cfg(feature = "text")]
        return self.ds.store().enable_text(cfg);
        #[cfg(not(feature = "text"))]
        {
            let _ = cfg;
            Err(no_text())
        }
    }

    /// Turn the index off and remove its files.
    pub fn disable(&self) -> Result<()> {
        #[cfg(feature = "text")]
        return self.ds.store().disable_text();
        #[cfg(not(feature = "text"))]
        Err(no_text())
    }

    /// Rebuild the index from the current state (writes wait meanwhile).
    pub fn rebuild(&self) -> Result<TextStatus> {
        #[cfg(feature = "text")]
        return self.ds.store().rebuild_text();
        #[cfg(not(feature = "text"))]
        Err(no_text())
    }
}

/// Options of [`VectorIndexes::recall`]: `samples` stored vectors are the queries, and
/// recall@`k` is measured against the exact search. `ef` overrides the index's
/// `efSearch`.
#[derive(Clone, Debug)]
pub struct RecallOptions {
    pub samples: usize,
    pub k: usize,
    pub ef: Option<usize>,
}

impl Default for RecallOptions {
    fn default() -> RecallOptions {
        RecallOptions {
            samples: 100,
            k: 10,
            ef: None,
        }
    }
}

/// The vector indexes.
#[derive(Clone)]
pub struct VectorIndexes {
    ds: Dataset,
}

impl VectorIndexes {
    pub fn list(&self) -> Vec<VectorIndexStatus> {
        self.ds.store().vector_indexes()
    }

    pub fn get(&self, name: &str) -> Option<VectorIndexStatus> {
        self.ds.store().vector_index(name)
    }

    /// Create or reconfigure index `name`; whether its build started. The build runs in
    /// the background ([`wait`](Self::wait) blocks until it ends).
    pub fn put(&self, name: &str, cfg: VectorIndexConfig) -> Result<bool> {
        self.ds.store().create_vector_index(name, cfg)
    }

    /// Remove index `name`.
    pub fn drop(&self, name: &str) -> Result<()> {
        self.ds.store().drop_vector_index(name)
    }

    /// Rebuild index `name` in the background.
    pub fn rebuild(&self, name: &str) -> Result<()> {
        self.ds.store().rebuild_vector_index(name)
    }

    /// Block until index `name`'s build ends, and return its status.
    pub fn wait(&self, name: &str) -> Option<VectorIndexStatus> {
        self.ds.store().wait_vector_index(name)
    }

    /// Measure index `name`'s recall against the exact search.
    pub fn recall(&self, name: &str, opts: &RecallOptions) -> Result<VectorRecall> {
        self.ds
            .store()
            .vector_recall(name, opts.samples, opts.k, opts.ef)
    }

    /// Compute index `name`'s embeddings again with its embedding service.
    pub fn reembed(&self, name: &str) -> Result<()> {
        self.ds.store().reembed(name)
    }

    /// The embedding worker's status of index `name`.
    pub fn embedding_status(&self, name: &str) -> Option<EmbeddingStatus> {
        self.ds.store().embedding_status(name)
    }

    /// Block until every embedding worker is idle, or fail after `timeout`.
    pub fn embed_until_idle(&self, timeout: Duration) -> Result<()> {
        self.ds.store().embed_until_idle(timeout)
    }
}

/// The spatial index.
#[derive(Clone)]
pub struct GeoIndex {
    ds: Dataset,
}

impl GeoIndex {
    /// Its status, or `None` when it is not enabled.
    pub fn status(&self) -> Option<GeoStatus> {
        self.ds.store().geo_status()
    }

    /// Enable (or reconfigure) the index; the build runs in the background.
    pub fn enable(&self, cfg: GeoConfig) -> Result<GeoStatus> {
        self.ds.store().enable_geo(cfg)
    }

    /// Turn the index off.
    pub fn disable(&self) -> Result<()> {
        self.ds.store().disable_geo()
    }

    /// Rebuild the index of the current generation in the background.
    pub fn rebuild(&self) -> Result<GeoStatus> {
        self.ds.store().rebuild_geo()
    }

    /// Block until the build ends, and return the status.
    pub fn wait(&self) -> Option<GeoStatus> {
        self.ds.store().wait_geo()
    }

    /// The GeoJSON `FeatureCollection` of the indexed geometries in a box, of the graphs
    /// `graphs` reads (every graph when `None`).
    #[cfg(feature = "geo")]
    pub fn features(
        &self,
        q: &crate::geo::map::BoxQuery,
        graphs: Option<&crate::access::GraphAccess>,
    ) -> Result<serde_json::Value> {
        crate::geo::map::features_in_box_of(&self.ds.snapshot(), q, graphs)
    }
}
