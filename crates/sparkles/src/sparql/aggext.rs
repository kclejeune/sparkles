//! Custom aggregates: extension IRIs that the parser reads as aggregate calls
//! (`SELECT (geof:aggUnion(?w) AS ?u)` groups like `SUM`), and their evaluation.
//!
//! None is registered yet: [`register`] leaves the parser as it is, and an unknown
//! custom aggregate is unbound.

use super::ctx::Ctx;
use crate::id::Id;
use spargebra::SparqlParser;

/// The parser with every custom aggregate IRI of this build registered.
pub fn register(p: SparqlParser) -> SparqlParser {
    p
}

/// The value of the custom aggregate `iri` over the evaluated values of one group
/// (after DISTINCT; `Err` for a row whose expression was an error); unbound when `iri`
/// is not a known aggregate or the aggregate is an error.
pub(crate) fn aggregate(ctx: &Ctx, iri: &str, vals: &[std::result::Result<Id, ()>]) -> Id {
    let _ = (ctx, iri, vals);
    Id::UNDEF
}
