//! The reasoning mode of a query and the one view of inferred data that it selects.
//!
//! A caller names a [`ReasoningMode`], and [`Dataset::inference_view`] turns it into an
//! [`InferenceView`]. The view gives the query options that read the selected data, the
//! input of incremental views and the source of an explanation, so that no caller decides
//! from a string which inferred data a query reads.

use super::explain::ExplainSource;
use crate::Dataset;
use crate::dataset::DatasetState;
use crate::error::{Error, Result};
use crate::incr::Inference;
use crate::sparql::QueryOptions;
use crate::sparql::rdfs::RdfsOnRead;
use crate::sparql::reasoning::ReasoningProvider;
use std::sync::Arc;

/// Which inferred data a query reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ReasoningMode {
    /// The asserted data alone, without the materialized inferences and RDFS on read.
    Off,
    /// The materialized inferences and RDFS on read, as the dataset configures them.
    #[default]
    Materialized,
    /// Facts derived while the query runs, by the query-time reasoner.
    Demand(Demand),
    /// A choice between materialized and demand reasoning by cost. No such choice is made
    /// at present, and [`Dataset::inference_view`] refuses it.
    Auto,
}

/// The query-time reasoner that [`ReasoningMode::Demand`] asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Demand {
    /// The profile whose rules derive the facts. Without one, the profile and custom rules
    /// of the last materialization apply.
    #[cfg(feature = "reasoning")]
    pub profile: Option<sparkles_reasoner::Profile>,
    /// The most facts the reasoner derives for one query, or the reasoner's default.
    pub max_derived: Option<usize>,
}

impl ReasoningMode {
    /// The mode named `false`, `true`, `materialized`, `demand` or `auto`. A demand mode
    /// has the default profile and limit.
    pub fn parse(name: &str) -> Option<ReasoningMode> {
        match name {
            "false" => Some(ReasoningMode::Off),
            "true" | "materialized" => Some(ReasoningMode::Materialized),
            "demand" => Some(ReasoningMode::Demand(Demand::default())),
            "auto" => Some(ReasoningMode::Auto),
            _ => None,
        }
    }

    /// The mode a request parameter names, where an absent or unknown value keeps the
    /// dataset's materialized inferences, as the query endpoints have always read it.
    pub fn from_param(value: Option<&str>) -> ReasoningMode {
        value
            .and_then(ReasoningMode::parse)
            .unwrap_or(ReasoningMode::Materialized)
    }

    /// Whether the mode reads inferred data of any kind.
    pub fn reads_inferences(&self) -> bool {
        *self != ReasoningMode::Off
    }
}

/// What the dataset's queries read beyond the stored quads under materialized
/// reasoning: RDFS on read, and the graph of materialized inferences while the dataset
/// has a reasoning record.
pub(crate) fn materialized(state: &DatasetState) -> (Option<Arc<RdfsOnRead>>, Vec<String>) {
    (
        state.rdfs.read().clone(),
        if state.reasoning.read().is_some() {
            vec![super::INFERRED_GRAPH.to_string()]
        } else {
            Vec::new()
        },
    )
}

/// The inferred data that one reasoning mode selects on one dataset.
#[derive(Clone)]
pub struct InferenceView {
    mode: ReasoningMode,
    provider: Option<Arc<dyn ReasoningProvider>>,
    dataset: Dataset,
}

impl std::fmt::Debug for InferenceView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InferenceView")
            .field("mode", &self.mode)
            .field("demand", &self.provider.is_some())
            .finish()
    }
}

impl InferenceView {
    /// The mode the view was made for.
    pub fn mode(&self) -> &ReasoningMode {
        &self.mode
    }

    /// The dataset's query options, as [`Dataset::query_options`] gives them, reading
    /// the view's inferred data.
    pub fn query_options(&self) -> QueryOptions {
        let mut opts = self.dataset.query_options();
        self.apply(&mut opts);
        opts
    }

    /// Change `opts` as the mode changes the dataset's defaults. Off leaves the
    /// materialized inferences and RDFS on read out, and demand installs the query-time
    /// reasoner, which reads the asserted data alone. Materialized reasoning keeps what
    /// `opts` already read, so a caller's own RDFS on read stays.
    pub fn apply(&self, opts: &mut QueryOptions) {
        match &self.mode {
            ReasoningMode::Off => {
                opts.default_graph_extra.clear();
                opts.rdfs = None;
            }
            ReasoningMode::Demand(_) => opts.reasoning = self.provider.clone(),
            ReasoningMode::Materialized | ReasoningMode::Auto => {}
        }
    }

    /// The inferred data that incremental views and live queries read under the view.
    /// Demand reasoning derives nothing for them, so they read what materialized
    /// reasoning reads.
    pub fn inference(&self) -> Option<Inference> {
        match self.mode {
            ReasoningMode::Off => None,
            _ => {
                let (rdfs, extra) = materialized(self.dataset.state());
                Inference::new(rdfs, extra)
            }
        }
    }

    /// Where an explanation under the view comes from, when the mode names one.
    pub fn explain_source(&self) -> Option<ExplainSource> {
        match self.mode {
            ReasoningMode::Materialized => Some(ExplainSource::Materialized),
            ReasoningMode::Demand(_) => Some(ExplainSource::Demand),
            ReasoningMode::Off | ReasoningMode::Auto => None,
        }
    }
}

impl Dataset {
    /// The inferred data that `mode` selects. Demand reasoning builds the query-time
    /// reasoner, as [`Dataset::query_reasoner`] does, and fails as it does. Automatic
    /// reasoning is refused, since no cost-based choice exists.
    pub fn inference_view(&self, mode: ReasoningMode) -> Result<InferenceView> {
        let provider = match &mode {
            ReasoningMode::Auto => {
                return Err(Error::invalid(
                    "reasoning=auto is not available yet; use demand, materialized or false",
                ));
            }
            #[cfg(feature = "reasoning")]
            ReasoningMode::Demand(demand) => {
                let reasoner = self.query_reasoner(demand.profile.clone())?;
                let reasoner = match demand.max_derived {
                    Some(n) => reasoner.with_max_derived(n),
                    None => reasoner,
                };
                Some(reasoner.provider())
            }
            #[cfg(not(feature = "reasoning"))]
            ReasoningMode::Demand(_) => {
                return Err(Error::unsupported("built without the `reasoning` feature"));
            }
            ReasoningMode::Off | ReasoningMode::Materialized => None,
        };
        Ok(self.view(mode, provider))
    }

    /// The view of the materialized inferences when `inferences` is set, and of the
    /// asserted data alone otherwise. Neither can fail.
    pub fn stored_inference_view(&self, inferences: bool) -> InferenceView {
        let mode = if inferences {
            ReasoningMode::Materialized
        } else {
            ReasoningMode::Off
        };
        self.view(mode, None)
    }

    fn view(
        &self,
        mode: ReasoningMode,
        provider: Option<Arc<dyn ReasoningProvider>>,
    ) -> InferenceView {
        InferenceView {
            mode,
            provider,
            dataset: self.clone(),
        }
    }
}
