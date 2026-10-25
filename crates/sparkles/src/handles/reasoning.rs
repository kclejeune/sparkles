//! Materialized inferences and RDFS on read.

use crate::Dataset;
use crate::error::Result;
use crate::reasoning::rdfs::NewSchema;
use crate::reasoning::{Freshness, ReasoningRecord, ReasoningStatus};
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

    /// The record of the last materialization with the freshness of its inferences at
    /// the head, or `None` when the dataset has no materialized inferences.
    pub fn status(&self) -> Option<ReasoningStatus> {
        Some(ReasoningStatus::of(self.record()?, self.ds.store()))
    }

    /// The freshness of the materialized inferences at commit `seq`, or `None` when the
    /// dataset has none.
    pub fn freshness_at(&self, seq: u64) -> Option<Freshness> {
        Some(crate::reasoning::freshness(
            &self.record()?,
            self.ds.store(),
            seq,
        ))
    }

    /// Materialize inferences: read the inputs that `req` names, update the previous
    /// materialization when the record and the closure the dataset keeps allow it (and
    /// `req.incremental` asks for it), write the inferred graph, and record the run.
    /// A run rejected by the write guard fails with [`Error::Rejected`](crate::Error).
    #[cfg(feature = "reasoning")]
    pub fn run(
        &self,
        req: &crate::reasoning::ReasonRequest,
    ) -> Result<crate::reasoning::ReasonOutcome> {
        self.run_with(req, &crate::task::Control::none())
    }

    /// [`run`](Self::run), cancelled by `ctl` and reporting to its progress. A cancelled
    /// run fails with [`Error::Cancelled`](crate::Error) and writes nothing, and a run
    /// that yields to a write ([`ReasonRequest::yield_to_writers`]) fails with a
    /// `reasoner` component error whose code is
    /// [`SUPERSEDED`](crate::reasoning::run::SUPERSEDED).
    ///
    /// [`ReasonRequest::yield_to_writers`]: crate::reasoning::ReasonRequest::yield_to_writers
    #[cfg(feature = "reasoning")]
    pub fn run_with(
        &self,
        req: &crate::reasoning::ReasonRequest,
        ctl: &crate::task::Control,
    ) -> Result<crate::reasoning::ReasonOutcome> {
        crate::reasoning::run::run(&self.ds, req, ctl)
    }

    /// Inconsistency checks over the data, and over the inferences when `opts` reads
    /// them.
    #[cfg(feature = "reasoning")]
    pub fn diagnostics(
        &self,
        opts: &sparkles_reasoner::diagnostics::DiagnoseOptions,
    ) -> Result<crate::reasoning::Diagnostics> {
        let state = self.ds.state();
        crate::reasoning::diagnose_with(
            self.ds.store(),
            self.record().as_ref(),
            Some(&state.closure),
            opts,
        )
    }

    /// Diagnose and attach the first minimal support to each finding under one deadline
    /// and a shared bound on oracle checks.
    #[cfg(feature = "reasoning")]
    pub fn diagnostics_justified(
        &self,
        options: &sparkles_reasoner::diagnostics::DiagnoseOptions,
        analysis: &crate::reasoning::analysis::AnalysisOptions,
        max_checks: usize,
    ) -> Result<crate::reasoning::justify::JustifiedDiagnostics> {
        crate::reasoning::justify::diagnose(&self.ds, options, analysis, max_checks)
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

    /// Why the dataset entails a triple: derivation trees over the materialized
    /// inferences, or over what RDFS on read answers, from the facts `req.access` reads
    /// (spec U10 §3.3). A search over its limits fails with a `reasoner` component error
    /// whose code is [`REASONING_BUDGET`](crate::reasoning::explain::REASONING_BUDGET).
    pub fn explain(
        &self,
        req: &crate::reasoning::explain::ExplainRequest,
    ) -> Result<crate::reasoning::explain::Explanation> {
        crate::reasoning::explain::explain(&self.ds, req)
    }

    /// Compare the entailments of two retained states under the same rule program.
    /// The analysis writes only to private memory and leaves this dataset unchanged.
    #[cfg(feature = "reasoning")]
    pub fn diff(
        &self,
        request: &crate::reasoning::diff::DiffRequest,
    ) -> Result<crate::reasoning::diff::EntailmentDiff> {
        crate::reasoning::diff::diff(&self.ds, request)
    }

    /// Compute an entailment delta over the actual complete states of a write preview.
    #[cfg(feature = "reasoning")]
    pub fn diff_preview(
        &self,
        preview: &crate::preview::Preview,
        request: &crate::reasoning::diff::DiffRequest,
    ) -> Result<crate::reasoning::diff::EntailmentDiff> {
        crate::reasoning::diff::preview(&self.ds, preview, request)
    }

    /// Find minimal asserted supports for a diagnostic finding or an unsatisfiable class.
    #[cfg(feature = "reasoning")]
    pub fn justify(
        &self,
        request: &crate::reasoning::justify::JustifyRequest,
    ) -> Result<crate::reasoning::justify::JustificationReport> {
        crate::reasoning::justify::justify(&self.ds, request)
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
