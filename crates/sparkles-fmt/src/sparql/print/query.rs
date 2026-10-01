//! Queries: the query unit (prologue, body, a blank line between), the query forms,
//! `SELECT` and projection groups, dataset clauses, `WHERE` (inserted or dropped),
//! solution modifiers one per line (`LIMIT` before `OFFSET`), subqueries.

use super::Ctx;
use crate::doc::DocId;
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeId};

/// `QueryUnit`: the file header, the prologue, one blank line, the query, the trailing
/// `VALUES`, then the comments at the end of the file and one final newline.
pub fn query_unit(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let mut parts = Vec::new();
    parts.extend(cx.header());
    let mut prev: Option<NodeId> = None;
    for e in cx.children(n) {
        let doc = cx.element(e);
        if let Some(p) = prev {
            let blank = match e {
                Element::Node(c) => cx.comments.blank_before(c),
                Element::Token(t) => cx.comments.blank_before_token(t),
            };
            parts.push(match blank || cx.tree.kind(p) == NodeKind::Prologue {
                true => cx.empty_line(),
                false => cx.hard_line(),
            });
        }
        parts.push(doc);
        prev = match e {
            Element::Node(c) => Some(c),
            Element::Token(_) => prev.or(Some(n)),
        };
    }
    parts.extend(cx.dangling(n, prev.is_some()));
    parts.push(cx.hard_line());
    cx.concat(parts)
}

/// `SelectQuery`.
///
/// TODO: the layout (stub: as written).
pub fn select_query(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `ConstructQuery`.
///
/// TODO: the layout (stub: as written).
pub fn construct_query(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `DescribeQuery`.
///
/// TODO: the layout (stub: as written).
pub fn describe_query(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `AskQuery`.
///
/// TODO: the layout (stub: as written).
pub fn ask_query(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `SubSelect`.
///
/// TODO: the layout (stub: as written).
pub fn sub_select(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `SelectClause`.
///
/// TODO: the layout (stub: as written).
pub fn select_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `ProjectionItem`.
///
/// TODO: the layout (stub: as written).
pub fn projection_item(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `ConstructTemplate`.
///
/// TODO: the layout (stub: as written).
pub fn construct_template(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `DescribeClause`.
///
/// TODO: the layout (stub: as written).
pub fn describe_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `DatasetClause`.
///
/// TODO: the layout (stub: as written).
pub fn dataset_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `WhereClause`.
///
/// TODO: the layout (stub: as written).
pub fn where_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `GroupBy`.
///
/// TODO: the layout (stub: as written).
pub fn group_by(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `GroupCondition`.
///
/// TODO: the layout (stub: as written).
pub fn group_condition(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Having`.
///
/// TODO: the layout (stub: as written).
pub fn having(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `OrderBy`.
///
/// TODO: the layout (stub: as written).
pub fn order_by(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `OrderCondition`.
///
/// TODO: the layout (stub: as written).
pub fn order_condition(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Limit`.
///
/// TODO: the layout (stub: as written).
pub fn limit(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Offset`.
///
/// TODO: the layout (stub: as written).
pub fn offset(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}
