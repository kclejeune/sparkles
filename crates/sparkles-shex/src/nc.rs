//! Node constraints on store ids: kind and datatype from the id's tag or term kind,
//! lexical validity through [`sparkles::xsd`], string facets in code points, patterns
//! with SPARQL `REGEX` semantics, numeric facets with XPath promotion, and value sets
//! as id sets, base-vocabulary id ranges for IRI stems, and language ranges. Results
//! for vocabulary ids are cached per constraint.

use crate::ShexFailure;
use crate::error::todo;
use crate::ir::{NcId, NcIr};
use oxrdf::Term;
use sparkles::id::Id;
use sparkles::store::Snapshot;
use std::sync::Arc;

/// The node constraints of a compiled schema, resolved against one snapshot.
pub struct NcPlan {
    pub snap: Arc<Snapshot>,
}

impl NcPlan {
    pub fn new(snap: &Arc<Snapshot>, ncs: &[NcIr]) -> NcPlan {
        let _ = ncs;
        NcPlan { snap: snap.clone() }
    }

    /// Does the stored node `node` satisfy constraint `nc`?
    pub fn check(&self, nc: NcId, node: Id) -> anyhow::Result<bool> {
        let _ = (nc, node);
        Err(todo("node constraints"))
    }

    /// Does `term`, which is not in the store, satisfy constraint `nc`?
    pub fn check_term(&self, nc: NcId, term: &Term) -> anyhow::Result<bool> {
        let _ = (nc, term);
        Err(todo("node constraints"))
    }

    /// Why `node` fails `nc` (`None` if it does not).
    pub fn explain(&self, nc: NcId, node: Id) -> anyhow::Result<Option<ShexFailure>> {
        let _ = (nc, node);
        Err(todo("node constraints"))
    }
}
