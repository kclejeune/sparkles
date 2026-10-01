//! Neighbourhoods from snapshots: for a (node, shape) pair, only the arcs the shape can
//! observe, through prefix scans of the data graph: outgoing arcs per predicate of the
//! shape, incoming arcs per inverse predicate, and, for a CLOSED shape, the other
//! outgoing arcs up to the first one that is not allowed. With a single-graph data graph,
//! index counts above the shape's maximum occurrences fail the pair before any value is
//! read.

use crate::error::todo;
use crate::ir::{Ir, ShapeId};
use sparkles::id::Id;
use sparkles::store::Snapshot;
use sparkles::validation::DataGraph;

/// A compiled schema resolved against one snapshot.
#[derive(Clone, Debug, Default)]
pub struct SnapPlan {
    /// per shape, the store id of the predicate of each [`crate::ir::ShapeIr::preds`]
    /// entry (`None`: not in the store, so it matches no arcs)
    pub preds: Vec<Vec<Option<Id>>>,
}

impl SnapPlan {
    pub fn new(snap: &Snapshot, ir: &Ir) -> SnapPlan {
        SnapPlan {
            preds: ir
                .shapes
                .iter()
                .map(|s| s.preds.iter().map(|(p, _, _)| snap.lookup_iri(p)).collect())
                .collect(),
        }
    }
}

/// The arcs of one node that a shape observes.
#[derive(Clone, Debug, Default)]
pub struct Neigh {
    /// the values of the arcs for each entry of the shape's `preds`, in order (objects
    /// of outgoing arcs, subjects of incoming ones)
    pub values: Vec<Vec<Id>>,
    /// for a CLOSED shape: an outgoing arc whose predicate is in no `preds` entry
    pub closed_violation: Option<(Id, Id)>,
}

/// How a fetch ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchOutcome {
    /// `out` holds the neighbourhood
    Ok,
    /// the pair fails without matching (index counts above the maximum occurrences)
    FailFast,
}

/// Fetch the neighbourhood of `node` for `shape` into `out` (cleared first).
pub fn fetch(
    data: &DataGraph,
    plan: &SnapPlan,
    node: Id,
    shape: ShapeId,
    out: &mut Neigh,
) -> anyhow::Result<FetchOutcome> {
    let _ = (data, plan, node, shape, out);
    Err(todo("neighbourhoods"))
}
