//! Materialized inferences and RDFS on read.

use crate::Dataset;
use crate::error::Result;
use crate::reasoning::ReasoningRecord;
use crate::reasoning::rdfs::NewSchema;
use crate::sparql::rdfs::RdfsOnRead;
use std::sync::Arc;

/// The dataset's reasoning (from [`Dataset::reasoning`]).
#[derive(Clone)]
pub struct Reasoning {
    pub(crate) ds: Dataset,
}

impl Reasoning {
    /// The record of the last materialization, if any.
    pub fn record(&self) -> Option<ReasoningRecord> {
        self.ds.reasoning_record()
    }

    /// Remove the materialized inferences and the record; the triples removed.
    #[cfg(feature = "reasoning")]
    pub fn clear(&self) -> Result<u64> {
        let state = self.ds.state();
        let n = sparkles_reasoner::clear(&state.store).map_err(super::from_anyhow)?;
        state.closure.clear();
        state.set_reasoning(None)?;
        Ok(n)
    }

    /// RDFS on read.
    pub fn rdfs(&self) -> RdfsSetting {
        RdfsSetting {
            ds: self.ds.clone(),
        }
    }
}

/// RDFS on read: queries match the RDFS closure of each graph with respect to a schema.
#[derive(Clone)]
pub struct RdfsSetting {
    ds: Dataset,
}

impl RdfsSetting {
    /// The setting, or `None` when it is off.
    pub fn get(&self) -> Option<Arc<RdfsOnRead>> {
        self.ds.state().rdfs.read().clone()
    }

    /// Set the schema: a graph of the dataset, or triples given once.
    pub fn set(&self, schema: NewSchema) -> Result<()> {
        crate::reasoning::rdfs::set(&self.ds, Some(schema))
    }

    /// Turn RDFS on read off.
    pub fn reset(&self) -> Result<()> {
        crate::reasoning::rdfs::set(&self.ds, None)
    }
}
