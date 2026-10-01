//! Custom aggregates: extension IRIs that the parser reads as aggregate calls
//! (`SELECT (geof:aggUnion(?w) AS ?u)` groups like `SUM`), and their evaluation.
//!
//! The GeoSPARQL aggregates (`geof:aggBoundingBox`, `aggBoundingCircle`, `aggCentroid`,
//! `aggConcaveHull`, `aggConvexHull`, `aggUnion`) are registered in every build, so a
//! query groups the same way with or without the `geo` feature; without it their value
//! is unbound, like an unknown function's.

use super::ctx::Ctx;
use crate::geo::vocab::{AGGREGATES, GEOF};
use crate::id::Id;
use oxrdf::NamedNode;
use spargebra::SparqlParser;

/// The parser with every custom aggregate IRI of this build registered.
pub fn register(mut p: SparqlParser) -> SparqlParser {
    for local in AGGREGATES {
        p = p.with_custom_aggregate_function(NamedNode::new_unchecked(format!("{GEOF}{local}")));
    }
    p
}

/// The value of the custom aggregate `iri` over the evaluated values of one group
/// (after DISTINCT; `Err` for a row whose expression was an error); unbound when `iri`
/// is not a known aggregate or the aggregate is an error.
pub(crate) fn aggregate(ctx: &Ctx, iri: &str, vals: &[std::result::Result<Id, ()>]) -> Id {
    #[cfg(feature = "geo")]
    if let Some(id) = iri
        .strip_prefix(GEOF)
        .and_then(|local| crate::geo::aggregates::evaluate(ctx, local, vals))
    {
        return id;
    }
    let _ = (ctx, iri, vals);
    Id::UNDEF
}
