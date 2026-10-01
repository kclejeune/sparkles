//! Queries: the query unit (prologue, body, a blank line between), the query forms,
//! `SELECT` and projection groups, dataset clauses, `WHERE` (inserted or dropped),
//! solution modifiers one per line (`LIMIT` before `OFFSET`), subqueries.

use super::Ctx;
use crate::doc::DocId;
use crate::lex::TokenKind;
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

/// The clauses of a query form or subquery, each starting a line (blank lines kept as
/// written). The form line keeps together what belongs on it: `CONSTRUCT {` with its
/// template, and `ASK {` or `CONSTRUCT WHERE {` with the pattern when no dataset clause
/// comes between. `LIMIT` goes before `OFFSET`.
fn clauses(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let children = cx.children(n);
    // the lines: their first node (for blank lines) and their parts
    let mut lines: Vec<(Option<NodeId>, Vec<DocId>)> = Vec::new();
    let mut joinable = false;
    for e in children {
        match e {
            Element::Token(t) => {
                // the form keyword
                let d = cx.term(t);
                lines.push((None, vec![d]));
                joinable = true;
            }
            Element::Node(c) => {
                let kind = cx.tree.kind(c);
                let joins = joinable
                    && matches!(kind, NodeKind::ConstructTemplate | NodeKind::WhereClause)
                    && !cx.has_leading(c);
                let d = cx.node(c);
                match (joins, lines.last_mut()) {
                    (true, Some((_, parts))) => {
                        parts.push(cx.arena.text(" "));
                        parts.push(d);
                    }
                    _ => lines.push((Some(c), vec![d])),
                }
                joinable = false;
            }
        }
    }
    // LIMIT before OFFSET
    let at = |lines: &[(Option<NodeId>, Vec<DocId>)], kind: NodeKind| {
        lines
            .iter()
            .position(|(c, _)| c.is_some_and(|c| cx.tree.kind(c) == kind))
    };
    if let (Some(o), Some(l)) = (at(&lines, NodeKind::Offset), at(&lines, NodeKind::Limit))
        && o < l
    {
        let limit = lines.remove(l);
        lines.insert(o, limit);
    }
    let mut parts = Vec::new();
    for (i, (first, docs)) in lines.into_iter().enumerate() {
        if i > 0 {
            let blank = first.is_some_and(|c| cx.comments.blank_before(c));
            parts.push(match blank {
                true => cx.empty_line(),
                false => cx.hard_line(),
            });
        }
        parts.extend(docs);
    }
    cx.concat(parts)
}

/// `SelectQuery`: the `SELECT` line, then each clause on its own line.
pub fn select_query(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    clauses(cx, n)
}

/// `ConstructQuery`: `CONSTRUCT {` with its template, then `WHERE {` on its own line;
/// the short form `CONSTRUCT WHERE {`.
pub fn construct_query(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    clauses(cx, n)
}

/// `DescribeQuery`: the `DESCRIBE` line, then each clause on its own line.
pub fn describe_query(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    clauses(cx, n)
}

/// `AskQuery`: `ASK {` (its `WHERE` dropped), the other clauses each on a line.
pub fn ask_query(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    clauses(cx, n)
}

/// `SubSelect`: laid out as a query; the enclosing group puts it between its braces.
pub fn sub_select(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    clauses(cx, n)
}

/// A keyword and the items after it (`SELECT DISTINCT ?a ?b`, `DESCRIBE ?s <x>`,
/// `GROUP BY ?a ?b`): one line when it fits, else the keywords alone and each item on
/// its own line one level deeper. `keywords` is how many leading tokens are keywords.
fn keyword_list(cx: &mut Ctx<'_, '_>, n: NodeId, keywords: usize) -> DocId {
    let children = cx.children(n);
    let (head, items) = children.split_at(keywords.min(children.len()));
    let head: Vec<DocId> = head.iter().map(|&e| cx.element(e)).collect();
    let head = cx.spaced(head);
    if items.is_empty() {
        return head;
    }
    let mut inner = Vec::new();
    for &e in items {
        inner.push(cx.line());
        inner.push(cx.element(e));
    }
    let inner = cx.concat(inner);
    let inner = cx.indent(inner);
    let all = cx.concat([head, inner]);
    cx.group(all)
}

/// How many of `n`'s first children are keyword tokens.
fn leading_keywords(cx: &Ctx<'_, '_>, n: NodeId) -> usize {
    cx.tree
        .children(n)
        .iter()
        .take_while(|e| {
            matches!(e, Element::Token(t) if matches!(cx.tree.token_kind(*t), TokenKind::Kw(_)))
        })
        .count()
}

/// Children one space apart, tight inside parentheses and between a keyword and the
/// `(` that opens its argument: `(COUNT(?x) AS ?n)`, `DESC(?n)`, `HAVING(…)`; a
/// keyword before anything else takes a space (`HAVING BOUND(?x)`).
fn tight(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let children = cx.children(n);
    let mut parts = Vec::new();
    let mut prev: Option<Element> = None;
    for e in children {
        if let Some(p) = prev {
            let after_open =
                matches!(p, Element::Token(t) if cx.tree.token_kind(t) == TokenKind::LParen);
            let before_close =
                matches!(e, Element::Token(t) if cx.tree.token_kind(t) == TokenKind::RParen);
            let keyword_call = matches!(p, Element::Token(t)
                if matches!(cx.tree.token_kind(t), TokenKind::Kw(_)))
                && first_kind(cx, e) == Some(TokenKind::LParen);
            if !(after_open || before_close || keyword_call) {
                parts.push(cx.space());
            }
        }
        parts.push(cx.element(e));
        prev = Some(e);
    }
    cx.concat(parts)
}

/// The kind of an element's first token.
fn first_kind(cx: &Ctx<'_, '_>, e: Element) -> Option<TokenKind> {
    let t = match e {
        Element::Token(t) => t,
        Element::Node(c) => cx.tree.first_token(c)?,
    };
    Some(cx.tree.token_kind(t))
}

/// `SelectClause`: `SELECT [DISTINCT|REDUCED]` and the projection, one line when it
/// fits, else `SELECT` alone and one item per line at +1 indent.
pub fn select_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let k = leading_keywords(cx, n);
    keyword_list(cx, n, k)
}

/// `ProjectionItem`: a variable, or `(expr AS ?v)`.
pub fn projection_item(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    tight(cx, n)
}

/// `ConstructTemplate`: an always expanded block of triples.
pub fn construct_template(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    block_of_nodes(cx, n)
}

/// `{`, the child nodes one per line, `}` (a lone `.` inside is dropped).
fn block_of_nodes(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let children = cx.children(n);
    let mut open = None;
    let mut close = None;
    let mut items = Vec::new();
    for e in children {
        match e {
            Element::Token(t) => match cx.tree.token_kind(t) {
                TokenKind::LBrace => open = Some(cx.tok(t)),
                TokenKind::RBrace => close = Some(cx.tok(t)),
                _ => {}
            },
            Element::Node(c) => {
                let d = cx.node(c);
                items.push((c, d));
            }
        }
    }
    let open = open.unwrap_or_else(|| cx.text("{"));
    let close = close.unwrap_or_else(|| cx.text("}"));
    cx.block(n, open, &items, close)
}

/// `DescribeClause`: `DESCRIBE` and its terms (or `*`), as the `SELECT` line.
pub fn describe_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    keyword_list(cx, n, 1)
}

/// `DatasetClause`: `FROM [NAMED] <g>`.
pub fn dataset_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `WhereClause`: `WHERE {`, the keyword inserted when missing (N13), and dropped
/// after `ASK`.
pub fn where_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let after_ask = cx
        .tree
        .parent(n)
        .is_some_and(|p| cx.tree.kind(p) == NodeKind::AskQuery);
    let mut parts = Vec::new();
    let mut has_where = false;
    for e in cx.children(n) {
        match e {
            Element::Token(t) => {
                if !after_ask {
                    parts.push(cx.kw(t));
                    parts.push(cx.space());
                }
                has_where = true;
            }
            Element::Node(c) => {
                if !has_where && !after_ask {
                    parts.push(cx.text("WHERE "));
                }
                parts.push(cx.node(c));
            }
        }
    }
    cx.concat(parts)
}

/// `GroupBy`: `GROUP BY` and its conditions, as the `SELECT` line.
pub fn group_by(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    keyword_list(cx, n, 2)
}

/// `GroupCondition`: a variable, a call, `(expr)` or `(expr AS ?v)`.
pub fn group_condition(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    tight(cx, n)
}

/// `Having`: `HAVING(expr)`, tight before a bracketed constraint, a space before a
/// call; several constraints one space apart.
pub fn having(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    tight(cx, n)
}

/// `OrderBy`: `ORDER BY` and its conditions, as the `SELECT` line.
pub fn order_by(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    keyword_list(cx, n, 2)
}

/// `OrderCondition`: `ASC(expr)`, `DESC(expr)`, a variable or a constraint.
pub fn order_condition(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    tight(cx, n)
}

/// `Limit`: `LIMIT n`.
pub fn limit(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `Offset`: `OFFSET n`.
pub fn offset(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}
