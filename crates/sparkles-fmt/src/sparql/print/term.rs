//! RDF terms: literals with their language tag or datatype (N9 shorthand, N10
//! quotes), IRIs (N7), keywords in the grammar's spelling.

use super::Ctx;
use crate::doc::DocId;
use crate::tree::NodeId;

/// `Literal`.
///
/// TODO: the layout (stub: as written).
pub fn literal(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}
