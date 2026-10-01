//! Property paths, printed tight and with their parentheses exactly as written:
//! `a/rdfs:subClassOf*`, `^ex:p`, `(ex:a|ex:b)+`, `!(rdf:type|^rdf:type)`. The entry
//! that holds a path puts a space after it, so `?`, `*` and `+` never touch the next
//! token.

use super::{Ctx, term};
use crate::doc::DocId;
use crate::tree::NodeId;

/// `PathAlternative`: `a|b`.
pub fn path_alternative(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    tight(cx, n)
}

/// `PathSequence`: `a/b`.
pub fn path_sequence(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    tight(cx, n)
}

/// `PathElt`: `p?`, `p*`, `p+`.
pub fn path_elt(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    tight(cx, n)
}

/// `PathInverse`: `^p`.
pub fn path_inverse(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    tight(cx, n)
}

/// `PathNegated`: `!p`, `!(a|^b)`.
pub fn path_negated(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    tight(cx, n)
}

/// `PathBracketed`: `(p)`.
pub fn path_bracketed(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    tight(cx, n)
}

/// The children with nothing between them; IRIs compacted, `a` as `a`.
fn tight(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let parts: Vec<DocId> = cx
        .children(n)
        .into_iter()
        .map(|e| term::element(cx, e))
        .collect();
    cx.concat(parts)
}
