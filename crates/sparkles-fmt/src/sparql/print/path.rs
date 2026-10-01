//! Property paths, printed tight (`a/rdfs:subClassOf*`, `^ex:p`, `!(a|^b)`).

use super::Ctx;
use crate::doc::DocId;
use crate::tree::NodeId;

/// `PathAlternative`.
///
/// TODO: the layout (stub: as written).
pub fn path_alternative(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `PathSequence`.
///
/// TODO: the layout (stub: as written).
pub fn path_sequence(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `PathElt`.
///
/// TODO: the layout (stub: as written).
pub fn path_elt(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `PathInverse`.
///
/// TODO: the layout (stub: as written).
pub fn path_inverse(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `PathNegated`.
///
/// TODO: the layout (stub: as written).
pub fn path_negated(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `PathBracketed`.
///
/// TODO: the layout (stub: as written).
pub fn path_bracketed(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}
