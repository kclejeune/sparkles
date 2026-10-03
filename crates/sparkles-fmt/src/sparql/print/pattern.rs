//! Group graph patterns: always expanded, one element per line, ` .` after triples
//! only; `OPTIONAL {`, `MINUS {`, `} UNION {`, `GRAPH g {`, `SERVICE [SILENT] x {`,
//! `FILTER(…)`, `BIND(… AS ?v)`, and ARQ's `LET(?v := …)` and `UNFOLD(… AS ?v, ?w)`.

use super::{Ctx, term};
use crate::doc::DocId;
use crate::lex::TokenKind;
use crate::tree::{Element, NodeId};

/// `GroupGraphPattern`: `{`, the elements (or the subquery) one per line one level
/// deeper, `}`; `{}` when empty. A `.` after an element that is not a triples
/// statement is dropped.
pub fn group_graph_pattern(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let items: Vec<(NodeId, DocId)> = cx
        .child_nodes(n)
        .into_iter()
        .map(|c| (c, cx.node(c)))
        .collect();
    let open = match cx.child_token(n, TokenKind::LBrace) {
        Some(t) => cx.tok(t),
        None => cx.text("{"),
    };
    let close = match cx.child_token(n, TokenKind::RBrace) {
        Some(t) => cx.tok(t),
        None => cx.text("}"),
    };
    cx.block(n, open, &items, close)
}

/// `Optional`: `OPTIONAL {`.
pub fn optional(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    spaced(cx, n)
}

/// `Lateral`: `LATERAL {`.
pub fn lateral(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    spaced(cx, n)
}

/// `Minus`: `MINUS {`.
pub fn minus(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    spaced(cx, n)
}

/// `Union`: the branches one space apart (`} UNION {`); a lone group as itself. A
/// branch with a leading comment starts its own line, the comment above its `UNION`,
/// and so does a branch after one with a trailing comment.
pub fn union(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let mut parts = Vec::new();
    let mut after_comment = false;
    for (i, e) in cx.children(n).into_iter().enumerate() {
        if i > 0 {
            parts.push(match e {
                Element::Node(b) if after_comment || cx.has_leading(b) => cx.hard_line(),
                _ => cx.space(),
            });
        }
        after_comment = matches!(e, Element::Node(b) if !cx.comments.trailing(b).is_empty());
        parts.push(term::element(cx, e));
    }
    cx.concat(parts)
}

/// `UnionBranch`: its group, after `UNION` except in the first branch.
pub fn union_branch(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    spaced(cx, n)
}

/// `GraphPattern`: `GRAPH g {`.
pub fn graph_pattern(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    spaced(cx, n)
}

/// `Service`: `SERVICE [SILENT] x {`.
pub fn service(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    spaced(cx, n)
}

/// `Filter`: `FILTER(expr)` when the constraint is bracketed, `FILTER REGEX(…)`,
/// `FILTER NOT EXISTS {` and `FILTER <iri>(…)` otherwise.
pub fn filter(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let children = cx.children(n);
    let mut parts = Vec::new();
    for (i, &e) in children.iter().enumerate() {
        if i > 0 && !starts_with_paren(cx, e) {
            parts.push(cx.space());
        }
        parts.push(term::element(cx, e));
    }
    cx.concat(parts)
}

/// `Bind`: `BIND(expr AS ?v)`; broken, the expression goes one level deeper on its own
/// lines with `AS ?v` on its last line, and `)` comes back.
pub fn bind(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let children = cx.children(n);
    let open = children.iter().position(
        |&e| matches!(e, Element::Token(t) if cx.tree.token_kind(t) == TokenKind::LParen),
    );
    let close = children.iter().rposition(
        |&e| matches!(e, Element::Token(t) if cx.tree.token_kind(t) == TokenKind::RParen),
    );
    let (Some(open), Some(close)) = (open, close) else {
        return cx.verbatim(n);
    };
    let mut head = Vec::new();
    for &e in &children[..=open] {
        head.push(term::element(cx, e));
    }
    // one space between the parts, but `:=` (LET) stays together and `,` (UNFOLD)
    // follows its variable
    let mut inner = Vec::new();
    let kind = |e: Element| match e {
        Element::Token(t) => Some(cx.tree.token_kind(t)),
        Element::Node(_) => None,
    };
    let parts = &children[open + 1..close];
    let mut i = 0;
    while i < parts.len() {
        let e = parts[i];
        if i > 0 && kind(e) != Some(TokenKind::Comma) {
            inner.push(cx.space());
        }
        if kind(e) == Some(TokenKind::PnameNs)
            && parts.get(i + 1).copied().and_then(kind) == Some(TokenKind::Eq)
        {
            inner.push(cx.text(":="));
            i += 2;
            continue;
        }
        inner.push(term::element(cx, e));
        i += 1;
    }
    let inner = cx.concat(inner);
    let sl = cx.soft_line();
    let inner = cx.concat([sl, inner]);
    let inner = cx.indent(inner);
    let sl = cx.soft_line();
    let close = term::element(cx, children[close]);
    head.extend([inner, sl, close]);
    let d = cx.concat(head);
    cx.group(d)
}

/// `Let`: `LET(?v := expr)`, broken as `BIND` is.
pub fn assign(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    bind(cx, n)
}

/// `Unfold`: `UNFOLD(expr AS ?v, ?w)`, broken as `BIND` is.
pub fn unfold(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    bind(cx, n)
}

/// The children one space apart: keywords in the grammar's spelling, IRIs compacted.
fn spaced(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let parts: Vec<DocId> = cx
        .children(n)
        .into_iter()
        .map(|e| term::element(cx, e))
        .collect();
    cx.spaced(parts)
}

/// Whether element `e` starts with `(`.
fn starts_with_paren(cx: &Ctx<'_, '_>, e: Element) -> bool {
    let first = match e {
        Element::Token(t) => Some(t),
        Element::Node(c) => cx.tree.first_token(c),
    };
    first.is_some_and(|t| cx.tree.token_kind(t) == TokenKind::LParen)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::{Ctx, RULES};
    use crate::lex::{LexMode, lex};
    use crate::sparql::Unit;
    use crate::syntax::NodeKind;
    use crate::tree::NodeId;
    use crate::trivia::Comments;
    use crate::{Options, QuoteStyle};

    /// The first node of `kind` in the query or update `src`, printed on its own with
    /// `opts`.
    pub(crate) fn print_with(src: &str, kind: NodeKind, opts: &Options) -> String {
        let tokens = lex(src, LexMode::Sparql);
        let unit = crate::check::sparql_unit(src, &tokens).unwrap_or(Unit::Query);
        let tree = crate::sparql::parse::parse(src, tokens, unit).unwrap();
        let n = (0..tree.len() as u32)
            .map(NodeId)
            .find(|&n| tree.kind(n) == kind)
            .expect("a node of that kind");
        let comments = Comments::attach(&tree, &RULES);
        let mut cx = Ctx::new(&tree, &comments, opts);
        let d = cx.node(n);
        let p = crate::doc::print(&cx.arena, d, src, opts.line_width, opts.indent_width, None);
        p.unwrap().text
    }

    pub(crate) fn print(src: &str, kind: NodeKind) -> String {
        print_with(src, kind, &Options::default())
    }

    pub(crate) const P: &str = "PREFIX ex: <http://example.org/> ";

    fn group(body: &str) -> String {
        print(
            &format!("{P}SELECT * {{ {body} }}"),
            NodeKind::GroupGraphPattern,
        )
    }

    #[test]
    fn groups_are_expanded_one_element_per_line() {
        assert_eq!(group(""), "{}");
        assert_eq!(
            group("?s ex:p ?o optional{?s ex:q ?q}. minus { } ?a ?b ?c"),
            "{\n  ?s ex:p ?o .\n  OPTIONAL {\n    ?s ex:q ?q .\n  }\n  MINUS {}\n  ?a ?b ?c .\n}"
        );
        assert_eq!(
            group("graph ?g {} service silent <http://example.org/s> {} SERVICE ?x {}"),
            "{\n  GRAPH ?g {}\n  SERVICE SILENT ex:s {}\n  SERVICE ?x {}\n}"
        );
        assert_eq!(
            group("{ ?a ?b ?c } union {} UNION { ?d ?e ?f } {}"),
            "{\n  {\n    ?a ?b ?c .\n  } UNION {} UNION {\n    ?d ?e ?f .\n  }\n  {}\n}"
        );
    }

    #[test]
    fn filters_and_binds() {
        assert_eq!(
            group("filter(?a) FILTER ex:f(?b) bind(?a as ?c)"),
            "{\n  FILTER(?a)\n  FILTER ex:f(?b)\n  BIND(?a AS ?c)\n}"
        );
        let opts = Options {
            line_width: 40,
            ..Options::default()
        };
        let src = format!("{P}SELECT * {{ BIND(ex:function(?argument) AS ?longResult) }}");
        assert_eq!(
            print_with(&src, NodeKind::Bind, &opts),
            "BIND(\n  ex:function(?argument) AS ?longResult\n)"
        );
    }

    #[test]
    fn comments_in_groups() {
        assert_eq!(
            group("# a\n?s ?p ?o . # b\n\n\n# c\n{ ?a ?b ?c } # d\n# e\nUNION { } # f\n"),
            "{\n  # a\n  ?s ?p ?o . # b\n\n  # c\n  {\n    ?a ?b ?c .\n  } # d\n  # e\n  UNION {} # f\n}"
        );
        // a branch after a trailing comment starts its own line
        assert_eq!(group("{} # d\nUNION {}"), "{\n  {} # d\n  UNION {}\n}");
    }

    #[test]
    fn quote_style_preserve_keeps_single_quotes() {
        let opts = Options {
            quote_style: QuoteStyle::Preserve,
            ..Options::default()
        };
        let src = format!("{P}SELECT * {{ ?s ex:p 'x', '''y''' }}");
        assert_eq!(
            print_with(&src, NodeKind::TriplesStmt, &opts),
            "?s ex:p 'x', '''y''' ."
        );
        assert_eq!(
            print(&src, NodeKind::TriplesStmt),
            "?s ex:p \"x\", \"\"\"y\"\"\" ."
        );
    }
}
