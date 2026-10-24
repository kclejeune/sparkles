//! Why is this triple inferred? Derivation trees over the materialized inferences, or
//! over what RDFS on read answers (spec U10 §3.3, the `why` mode of F18 §2.1).
//!
//! The materialized inferences are explained at the commit their run read, when the
//! store still has that state, so the trees use the input graphs the closure was computed
//! from. A store whose history no longer reaches that commit is read at the head, and the
//! answer says so. Both searches read the caller's view: the input graphs are those the
//! caller reads, the snapshot hides the triples the caller's protections hide, and a tree
//! is returned only when all its leaves come from what the caller reads.

use crate::Dataset;
use crate::access::GraphAccess;
use crate::error::Result;
use crate::explain::Explained;
use crate::id::Id;
use crate::sparql::rdfs::{SchemaLeaves, SchemaSource};
use crate::store::Snapshot;
use oxrdf::Term;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// The code of an explanation over its limits.
pub const REASONING_BUDGET: &str = "reasoning-budget";
/// The code of a triple with no derivation from facts the caller reads.
pub const NO_VISIBLE_DERIVATION: &str = "no-visible-derivation";

/// The most distinct top-level derivations a request may ask for.
pub const MAX_TREES: usize = 10;
/// The depth of a returned tree by default, and the most a request may ask for.
pub const DEFAULT_MAX_DEPTH: u32 = 32;
pub const MAX_DEPTH: u32 = 64;
/// The rule instances one explanation visits.
pub const MAX_INSTANCES: usize = 100_000;
/// The timeout of an explanation that gives none.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// What to explain, and the limits.
#[derive(Clone)]
pub struct ExplainRequest {
    pub triple: [Term; 3],
    /// distinct top-level derivations, 1 by default, at most [`MAX_TREES`]
    pub max_trees: usize,
    /// the depth of the trees, deeper nodes are cut as `ref` nodes
    pub max_depth: u32,
    /// [`DEFAULT_TIMEOUT`] when `None`
    pub timeout: Option<Duration>,
    /// the caller's view, `None` for one that reads everything
    pub access: Option<Arc<GraphAccess>>,
    pub cancel: Option<Arc<AtomicBool>>,
    /// the memory the rule evaluations may hold
    pub max_memory: Option<usize>,
}

impl ExplainRequest {
    pub fn new(triple: [Term; 3]) -> ExplainRequest {
        ExplainRequest {
            triple,
            max_trees: 1,
            max_depth: DEFAULT_MAX_DEPTH,
            timeout: None,
            access: None,
            cancel: None,
            max_memory: None,
        }
    }
}

/// Where a derivation comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExplainSource {
    /// the materialized inferences
    Materialized,
    /// RDFS on read
    RdfsOnRead,
}

impl ExplainSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ExplainSource::Materialized => "materialized",
            ExplainSource::RdfsOnRead => "rdfsOnRead",
        }
    }
}

/// An explanation.
#[derive(Clone, Debug)]
pub struct Explanation {
    pub explained: Explained,
    /// the commit the search read
    pub commit: u64,
    /// where the trees come from, `None` when none was found
    pub source: Option<ExplainSource>,
    /// for the materialized inferences, the head commit when the search read it because
    /// the store's history no longer has the run's commit
    pub read_at_head: bool,
}

#[cfg(feature = "reasoning")]
fn budget_error(m: String) -> crate::Error {
    crate::Error::Component(Box::new(
        crate::error::ComponentError::new("reasoner", REASONING_BUDGET, m.clone())
            .with_source(crate::explain::ExplainBudgetExceeded(m)),
    ))
}

/// The snapshot of the caller's view.
fn view_of(access: Option<&Arc<GraphAccess>>, snap: &Arc<Snapshot>) -> Result<Arc<Snapshot>> {
    match access {
        None => Ok(snap.clone()),
        Some(a) => a.bound(snap).masked(snap),
    }
}

fn readable(access: Option<&Arc<GraphAccess>>, snap: &Arc<Snapshot>, g: Id) -> bool {
    access.is_none_or(|a| a.bound(snap).readable_id(snap, g))
}

/// Explain `req.triple` in `ds`.
pub fn explain(ds: &Dataset, req: &ExplainRequest) -> Result<Explanation> {
    let deadline = Instant::now() + req.timeout.unwrap_or(DEFAULT_TIMEOUT);
    let max_trees = req.max_trees.clamp(1, MAX_TREES);
    let max_depth = req.max_depth.min(MAX_DEPTH);
    let store = ds.store();
    let head = store.snapshot();
    let access = req.access.as_ref();
    #[cfg(feature = "reasoning")]
    if let Some(rec) = ds.reasoning_record() {
        let (snap, read_at_head) = match rec.commit {
            Some(c) if c != head.commit => match store.snapshot_at(
                &crate::history::At::Commit(c),
                &crate::history::HistoryOptions::default(),
            ) {
                Ok((s, _)) => (s, false),
                Err(_) => (head.clone(), true),
            },
            _ => (head.clone(), false),
        };
        let view = view_of(access, &snap)?;
        let names = rec
            .input_graphs
            .clone()
            .unwrap_or_else(|| vec!["default".to_string()]);
        let mut inputs: Vec<Id> = Vec::new();
        for n in &names {
            let id = if n == "default" {
                Some(Id::DEFAULT_GRAPH)
            } else {
                snap.lookup_iri(n)
            };
            if let Some(id) = id
                && readable(access, &snap, id)
                && !inputs.contains(&id)
            {
                inputs.push(id);
            }
        }
        let inferred = snap
            .lookup_iri(super::INFERRED_GRAPH)
            .filter(|&g| readable(access, &snap, g));
        let profile = rec.profile()?;
        let extras = rec.extras()?;
        let input = sparkles_reasoner::explain::ExplainInput {
            snap: view,
            profile: &profile,
            extras: &extras,
            inputs: &inputs,
            inferred,
        };
        let opts = sparkles_reasoner::explain::ExplainOptions {
            max_trees,
            max_depth,
            max_instances: MAX_INSTANCES,
            deadline: Some(deadline),
            cancel: req.cancel.clone(),
            max_memory: req.max_memory,
        };
        let found =
            sparkles_reasoner::explain::explain(&input, &req.triple, &opts).map_err(|e| {
                match e.downcast::<crate::explain::ExplainBudgetExceeded>() {
                    Ok(b) => budget_error(b.0),
                    Err(e) => crate::handles::from_anyhow(e),
                }
            })?;
        if let Explained::Found { .. } = found {
            return Ok(Explanation {
                explained: found,
                commit: snap.commit,
                source: Some(ExplainSource::Materialized),
                read_at_head,
            });
        }
    }
    if let Some(on) = ds.reasoning().rdfs().get() {
        let view = view_of(access, &head)?;
        let schema = on.schema(&view)?;
        let leaves = match &on.source {
            SchemaSource::Fixed(_) => SchemaLeaves::Document,
            SchemaSource::Graph(g) => {
                let id = match g {
                    None => Some(Id::DEFAULT_GRAPH),
                    Some(iri) => head.lookup_iri(iri),
                };
                match id {
                    Some(id) if readable(access, &head, id) => SchemaLeaves::Graph(id),
                    _ => SchemaLeaves::Hidden,
                }
            }
        };
        let inferred = head.lookup_iri(super::INFERRED_GRAPH);
        let mut graphs = vec![Id::DEFAULT_GRAPH];
        graphs.extend(head.graph_ids()?);
        graphs.retain(|&g| Some(g) != inferred && readable(access, &head, g));
        let found = schema.explain(
            &view,
            leaves,
            &graphs,
            &req.triple,
            crate::explain::ExplainLimits {
                max_trees,
                max_depth,
                deadline: Some(deadline),
            },
        )?;
        if let Explained::Found { .. } = found {
            return Ok(Explanation {
                explained: found,
                commit: head.commit,
                source: Some(ExplainSource::RdfsOnRead),
                read_at_head: false,
            });
        }
    }
    Ok(Explanation {
        explained: Explained::NotFound,
        commit: head.commit,
        source: None,
        read_at_head: false,
    })
}
