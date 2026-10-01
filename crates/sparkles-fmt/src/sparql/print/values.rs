//! `VALUES`: one variable inline when it fits (`VALUES ?x { a b c }`), else one value
//! per line; several variables one row per line, each row `(v₁ v₂)`, the columns not
//! aligned (`align-values` is not implemented yet and only warns).

use super::Ctx;
use super::expr::term_token;
use crate::doc::DocId;
use crate::lex::TokenKind;
use crate::tree::{Element, NodeId};

/// `ValuesClause`: the `VALUES` block after a query, as [`inline_values`].
pub fn values_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    data_block(cx, n)
}

/// `InlineValues`: `VALUES` inside a group.
pub fn inline_values(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    data_block(cx, n)
}

/// `VALUES`, the variables, then the block: inline values for one variable, rows for a
/// variable list (always one row per line).
fn data_block(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let children = cx.children(n);
    let Some(brace) = children.iter().position(
        |e| matches!(*e, Element::Token(t) if cx.tree.token_kind(t) == TokenKind::LBrace),
    ) else {
        return cx.verbatim(n);
    };
    let Some(Element::Token(close)) = children.last().copied() else {
        return cx.verbatim(n);
    };
    // `VALUES` and the variables: `?x`, `(?a ?b)` or `()`
    let mut head = Vec::new();
    let mut vars: Option<Vec<DocId>> = None;
    for &e in &children[..brace] {
        match e {
            Element::Token(t) if cx.tree.token_kind(t) == TokenKind::LParen => {
                vars = Some(vec![cx.tok(t)]);
            }
            Element::Token(t) if cx.tree.token_kind(t) == TokenKind::RParen => {
                let mut v = vars.take().unwrap_or_default();
                let close = cx.tok(t);
                let open = v.remove(0);
                let inner = cx.spaced(v);
                head.push(cx.concat([open, inner, close]));
            }
            e => {
                let d = term(cx, e);
                match vars.as_mut() {
                    Some(v) => v.push(d),
                    None => head.push(d),
                }
            }
        }
    }
    // `VALUES ?x {`: the variable right before the brace
    let one_var = brace > 0
        && matches!(children[brace - 1], Element::Token(t)
            if matches!(cx.tree.token_kind(t), TokenKind::Var1 | TokenKind::Var2));
    let Element::Token(open) = children[brace] else {
        unreachable!("a token")
    };
    let open = cx.tok(open);
    let close = cx.tok(close);
    let items = cx.child_nodes(n);
    let block = if one_var {
        let docs: Vec<DocId> = items.iter().map(|&c| cx.node(c)).collect();
        cx.delimited(n, open, &docs, close, true)
    } else {
        let docs: Vec<(NodeId, DocId)> = items.iter().map(|&c| (c, cx.node(c))).collect();
        cx.block(n, open, &docs, close)
    };
    head.push(block);
    cx.spaced(head)
}

/// `ValuesRow`: `(v₁ v₂)`, `()` for no variables; broken (by a comment) one value per
/// line.
pub fn values_row(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let children = cx.children(n);
    match (children.first(), children.last()) {
        (Some(&Element::Token(open)), Some(&Element::Token(close))) if children.len() >= 2 => {
            let docs: Vec<DocId> = cx.child_nodes(n).iter().map(|&c| cx.node(c)).collect();
            let open = cx.tok(open);
            let close = cx.tok(close);
            cx.delimited(n, open, &docs, close, false)
        }
        (Some(&e), _) if children.len() == 1 => term(cx, e),
        _ => cx.verbatim(n),
    }
}

/// `DataValue`: an IRI, a literal, `UNDEF` or a triple term.
pub fn data_value(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let docs: Vec<DocId> = cx.children(n).into_iter().map(|e| term(cx, e)).collect();
    cx.concat(docs)
}

/// A term: a node through the dispatcher, a token as an expression prints it (keywords
/// in the grammar's spelling, `()`, strings requoted).
fn term(cx: &mut Ctx<'_, '_>, e: Element) -> DocId {
    match e {
        Element::Node(c) => cx.node(c),
        Element::Token(t) => term_token(cx, t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;
    use crate::lex::{LexMode, lex};
    use crate::sparql::Unit;
    use crate::sparql::parse::parse;
    use crate::sparql::print::{RULES, node};
    use crate::syntax::NodeKind;
    use crate::trivia::Comments;

    /// The first `VALUES` block of query `src`, printed at the start of a line.
    fn values_with(src: &str, width: u16) -> String {
        let tree = parse(src, lex(src, LexMode::Sparql), Unit::Query).unwrap();
        let comments = Comments::attach(&tree, &RULES);
        let opts = Options {
            line_width: width,
            ..Options::default()
        };
        let mut cx = Ctx::new(&tree, &comments, &opts);
        let n = (0..tree.len() as u32)
            .map(NodeId)
            .find(|&n| {
                matches!(
                    tree.kind(n),
                    NodeKind::InlineValues | NodeKind::ValuesClause
                )
            })
            .expect("a VALUES block");
        let doc = node(&mut cx, n);
        crate::doc::print(&cx.arena, doc, tree.src, width, 2, None)
            .unwrap()
            .text
    }

    fn values(src: &str) -> String {
        values_with(src, 100)
    }

    #[test]
    fn one_variable_stays_inline_when_it_fits() {
        assert_eq!(
            values("SELECT * { values ?x { <http://e/a> 'b' 1 undef } }"),
            "VALUES ?x { <http://e/a> \"b\" 1 UNDEF }"
        );
        assert_eq!(
            values_with(
                "SELECT * { VALUES ?x { <http://e/aaaa> <http://e/bbbb> } }",
                30
            ),
            "VALUES ?x {\n  <http://e/aaaa>\n  <http://e/bbbb>\n}"
        );
        assert_eq!(values("SELECT * {} VALUES $x { }"), "VALUES $x {}");
    }

    #[test]
    fn a_comment_breaks_the_inline_form() {
        assert_eq!(
            values(
                "PREFIX ex: <http://e/>\nSELECT * {\n  VALUES ?org {\n    ex:acme   # main customer\n    ex:globex\n  }\n}"
            ),
            "VALUES ?org {\n  ex:acme # main customer\n  ex:globex\n}"
        );
        assert_eq!(
            values("SELECT * { VALUES ?x { 1\n # dangling\n } }"),
            "VALUES ?x {\n  1\n  # dangling\n}"
        );
    }

    #[test]
    fn several_variables_take_one_row_per_line() {
        assert_eq!(
            values(
                "SELECT * { VALUES (?s ?currency) { (<https://example.org/p1> \"EUR\") (<https://example.org/p2> UNDEF) } }"
            ),
            "VALUES (?s ?currency) {
  (<https://example.org/p1> \"EUR\")
  (<https://example.org/p2> UNDEF)
}"
        );
        assert_eq!(
            values("SELECT * { VALUES ( ?a ) { ( 1 ) } }"),
            "VALUES (?a) {\n  (1)\n}"
        );
        assert_eq!(
            values("SELECT * { VALUES () { () ( ) } }"),
            "VALUES () {\n  ()\n  ()\n}"
        );
        assert_eq!(values("SELECT * {} VALUES (?a $b) {}"), "VALUES (?a $b) {}");
        assert_eq!(
            values("SELECT * { VALUES (?a ?b) { (<<( <http://e/s> a \"o\" )>> false) } }"),
            "VALUES (?a ?b) {\n  (<<( <http://e/s> a \"o\" )>> false)\n}"
        );
    }

    #[test]
    fn a_comment_in_a_row_breaks_the_row() {
        assert_eq!(
            values("SELECT * { VALUES (?a ?b) { (1 # one\n 2) (3 4) # three\n } }"),
            "VALUES (?a ?b) {\n  (\n    1 # one\n    2\n  )\n  (3 4) # three\n}"
        );
    }
}
