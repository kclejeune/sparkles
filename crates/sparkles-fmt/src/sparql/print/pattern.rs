//! Group graph patterns: always expanded, one element per line, ` .` after triples
//! only; `OPTIONAL {`, `MINUS {`, `} UNION {`, `GRAPH g {`, `SERVICE [SILENT] x {`,
//! `FILTER(…)`, `BIND(… AS ?v)`.

use super::Ctx;
use crate::doc::DocId;
use crate::tree::NodeId;

/// `GroupGraphPattern`.
///
/// TODO: the layout (stub: as written).
pub fn group_graph_pattern(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Optional`.
///
/// TODO: the layout (stub: as written).
pub fn optional(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Minus`.
///
/// TODO: the layout (stub: as written).
pub fn minus(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Union`.
///
/// TODO: the layout (stub: as written).
pub fn union(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `UnionBranch`.
///
/// TODO: the layout (stub: as written).
pub fn union_branch(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `GraphPattern`.
///
/// TODO: the layout (stub: as written).
pub fn graph_pattern(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Service`.
///
/// TODO: the layout (stub: as written).
pub fn service(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Filter`.
///
/// TODO: the layout (stub: as written).
pub fn filter(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Bind`.
///
/// TODO: the layout (stub: as written).
pub fn bind(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}
