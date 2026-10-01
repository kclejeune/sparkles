//! The GeoSPARQL Query Rewrite Extension and Jena's `spatial:equals` in the planner.
//!
//! With `queryRewrite` on in the dataset's `geo.json` (and not switched off for the
//! server), a triple pattern whose predicate is one of the 24 topological properties
//! matches the asserted triples and the derived ones: `so1 geo:R so2` holds when some
//! geometry literal of `so1` and some of `so2` satisfy `geof:R`, where a feature's
//! literals are those of its `geo:hasDefaultGeometry`, a geometry's are its
//! serializations, and a literal is itself.
//!
//! ```sparql
//! ?x geo:sfContains ex:g1        # asserted ∪ derived, one solution per ?x
//! ex:A spatial:equals ?y         # sfEquals between features or geometries
//! ```
//!
//! Neither is planned yet: [`take_rewrite_triples`] takes nothing, so the leaf is
//! unreachable.

use super::ctx::Ctx;
use super::plan::{ActiveGraph, GraphFilter, Node, Planner};
use super::table::VarId;
use crate::error::Result;
use crate::geo::{GeomRef, Relation};
use crate::id::Id;
use spargebra::term::{TermPattern, TriplePattern};

/// A topological triple (or `spatial:equals`) taken out of a basic graph pattern.
#[derive(Clone, Debug)]
pub struct RewriteCall {
    pub rel: Relation,
    pub subject: TermPattern,
    pub object: TermPattern,
    /// the predicate is a data property whose asserted triples count too (false for
    /// `spatial:equals`, which is never data)
    pub asserted: bool,
}

/// One end of a [`SpatialRelateSpec`].
#[derive(Clone)]
pub enum RelateEnd {
    Var(VarId),
    /// an IRI or blank node of the data (a feature or a geometry)
    Node(Id),
    /// a geometry literal written in the query
    Geometry(Id, GeomRef),
    /// a constant the data does not hold: it relates to nothing
    Absent,
}

/// A topological property (or `spatial:equals`) planned as a leaf: the asserted triples
/// of `property` and the derived ones, as a set.
#[derive(Clone)]
pub struct SpatialRelateSpec {
    pub rel: Relation,
    pub subject: RelateEnd,
    pub object: RelateEnd,
    /// the property's id when asserted triples count and the data holds it
    pub property: Option<Id>,
    /// graph scope of the active graph
    pub graph: GraphFilter,
    /// `GRAPH ?g { … }` around the pattern: bound from each match's graph
    pub graph_var: Option<VarId>,
    /// one solution per pair across graphs (merged default graph)
    pub dedup: bool,
    /// result-cache key of the relation and the constant ends
    pub key: u64,
}

/// Whether the snapshot's dataset rewrites topological properties.
pub fn rewrite_enabled(ctx: &Ctx) -> bool {
    cfg!(feature = "geo")
        && ctx
            .snap
            .geo
            .as_ref()
            .is_some_and(|v| v.config.query_rewrite)
}

/// Take the topological triples (when the dataset rewrites them) and `spatial:equals`
/// triples out of `patterns`; returns them and the remaining patterns.
pub fn take_rewrite_triples(
    patterns: Vec<TriplePattern>,
    ctx: &Ctx,
) -> Result<(Vec<RewriteCall>, Vec<TriplePattern>)> {
    let _ = ctx;
    Ok((Vec::new(), patterns))
}

/// A taken triple as a leaf.
pub(super) fn rewrite_leaf(p: &Planner<'_>, c: RewriteCall, g: &ActiveGraph) -> Result<Node> {
    let _ = (p, g);
    Err(crate::error::Error::Unsupported(format!(
        "geo:{}: query rewrite is not supported yet",
        c.rel.local()
    )))
}
