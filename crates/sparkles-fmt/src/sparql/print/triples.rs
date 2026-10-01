//! Triples: subject blocks in the compact form, object lists, `[ … ]` blocks,
//! collections, and the RDF 1.2 reified triples, triple terms, reifiers and annotation
//! blocks.

use super::Ctx;
use crate::doc::DocId;
use crate::tree::NodeId;

/// `TriplesStmt`.
///
/// TODO: the layout (stub: as written).
pub fn triples_stmt(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `PropertyListEntry`.
///
/// TODO: the layout (stub: as written).
pub fn property_list_entry(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Object`.
///
/// TODO: the layout (stub: as written).
pub fn object(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `BNodePropertyList`.
///
/// TODO: the layout (stub: as written).
pub fn bnode_property_list(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Collection`.
///
/// TODO: the layout (stub: as written).
pub fn collection(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `CollectionItem`.
///
/// TODO: the layout (stub: as written).
pub fn collection_item(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `ReifiedTriple`.
///
/// TODO: the layout (stub: as written).
pub fn reified_triple(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `TripleTerm`.
///
/// TODO: the layout (stub: as written).
pub fn triple_term(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Reifier`.
///
/// TODO: the layout (stub: as written).
pub fn reifier(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `AnnotationBlock`.
///
/// TODO: the layout (stub: as written).
pub fn annotation_block(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}
