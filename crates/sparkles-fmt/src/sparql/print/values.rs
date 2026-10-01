//! `VALUES`: one variable inline when it fits (`VALUES ?x { a b c }`), else one value
//! per line; several variables one row per line, each row `(v₁ v₂)`, the columns
//! aligned only with `align-values`.

use super::Ctx;
use super::expr::term_token;
use crate::doc::DocId;
use crate::lex::TokenKind;
use crate::tree::{Element, NodeId, TokenId};
use unicode_width::UnicodeWidthStr;

/// Whether `align-values` is implemented.
pub const ALIGN_IMPLEMENTED: bool = true;

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
        let aligned = match cx.opts.align_values {
            true => aligned_rows(cx, &items),
            false => None,
        };
        let docs = aligned.unwrap_or_else(|| items.iter().map(|&c| (c, cx.node(c))).collect());
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

/// The rows of a multi-variable block with their cells padded into columns
/// (`align-values`): every cell but the last of its row is followed by spaces up to the
/// width of its column's widest cell, then one space. `None` leaves the block as
/// [`values_row`] prints it: a row under an ignore pragma, a comment inside a row, a
/// cell holding a line break (a long string), or fewer than two columns.
///
/// Whether the aligned rows fit the line width is known only where they are printed,
/// so every row follows one group printed at the start of the first row: it holds as
/// many spaces as the widest aligned row is wider than the first (the printer drops
/// spaces at the start of a line, but measures them), so it fits exactly when the widest
/// aligned row does. Flat, the rows print aligned; broken, as without the key.
fn aligned_rows(cx: &mut Ctx<'_, '_>, rows: &[NodeId]) -> Option<Vec<(NodeId, DocId)>> {
    let mut parsed: Vec<(NodeId, TokenId, Vec<NodeId>, TokenId)> = Vec::new();
    for &r in rows {
        if cx.comments.ignored(r) {
            return None;
        }
        let children = cx.children(r);
        let (Some(&Element::Token(open)), Some(&Element::Token(close))) =
            (children.first(), children.last())
        else {
            return None;
        };
        if cx.tree.token_kind(open) != TokenKind::LParen || children.len() < 2 {
            return None;
        }
        let cells = cx.child_nodes(r);
        let comment_inside =
            (open.0 + 1..close.0).any(|i| cx.tree.tokens[i as usize].kind == TokenKind::Comment);
        if comment_inside || cells.iter().any(|&c| cx.has_comments(c)) {
            return None;
        }
        parsed.push((r, open, cells, close));
    }
    if parsed.iter().map(|p| p.2.len()).max()? < 2 {
        return None;
    }
    // each cell's document and printed width
    let mut cells: Vec<Vec<(DocId, usize)>> = Vec::new();
    for (_, _, row, _) in &parsed {
        let mut docs = Vec::new();
        for &c in row {
            let d = cx.node(c);
            let src = cx.tree.src;
            let printed =
                crate::doc::print(&cx.arena, d, src, u16::MAX, cx.opts.indent_width, None);
            let text = printed.ok()?.text;
            if text.contains(['\n', '\r']) {
                return None;
            }
            docs.push((d, text.width()));
        }
        cells.push(docs);
    }
    // a column's width: its widest cell that is not the last of its row
    let mut widths: Vec<usize> = Vec::new();
    for row in &cells {
        for (j, &(_, w)) in row.iter().enumerate().take(row.len().saturating_sub(1)) {
            match widths.get_mut(j) {
                Some(c) => *c = (*c).max(w),
                None => widths.push(w),
            }
        }
    }
    let row_width = |row: &[(DocId, usize)]| -> usize {
        let Some((&(_, last), init)) = row.split_last() else {
            return 2;
        };
        2 + last + widths[..init.len()].iter().map(|w| w + 1).sum::<usize>()
    };
    let widest = cells.iter().map(|r| row_width(r)).max()?;
    let measure = cx.text(&" ".repeat(widest - row_width(&cells[0])));
    let (measure, fits) = cx.group_with_id(measure);
    let mut out = Vec::new();
    for (k, ((r, open, _, close), row)) in parsed.into_iter().zip(&cells).enumerate() {
        let open = cx.tok(open);
        let close = cx.tok(close);
        let mut aligned = vec![open];
        for (j, &(d, w)) in row.iter().enumerate() {
            aligned.push(d);
            if j + 1 < row.len() {
                aligned.push(cx.text(&" ".repeat(widths[j] - w + 1)));
            }
        }
        aligned.push(close);
        let aligned = cx.concat(aligned);
        let docs: Vec<DocId> = row.iter().map(|c| c.0).collect();
        let plain = cx.delimited(r, open, &docs, close, false);
        let mut doc = cx.if_break(plain, aligned, Some(fits));
        if k == 0 {
            doc = cx.concat([measure, doc]);
        }
        out.push((r, crate::trivia::wrap(&mut cx.arena, cx.comments, r, doc)));
    }
    Some(out)
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

    /// `src` formatted with `align-values` at `width`, through every check.
    fn aligned(src: &str, width: u16) -> String {
        let opts = Options {
            align_values: true,
            line_width: width,
            ..Options::default()
        };
        let out = crate::format(src, crate::Language::Sparql, &opts).unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        let again = crate::format(&out.text, crate::Language::Sparql, &opts).unwrap();
        assert_eq!(again.text, out.text, "not a fixpoint");
        out.text
    }

    #[test]
    fn align_values_pads_every_cell_but_the_last() {
        assert_eq!(
            aligned(
                "PREFIX ex: <http://e/>\nSELECT * { VALUES (?s ?currency ?n) { (<https://example.org/p1> \"EUR\" 1) (ex:p2 UNDEF 22) (ex:p3 'GBP' 3) } }",
                100
            ),
            "PREFIX ex: <http://e/>

SELECT *
WHERE {
  VALUES (?s ?currency ?n) {
    (<https://example.org/p1> \"EUR\" 1)
    (ex:p2                    UNDEF 22)
    (ex:p3                    \"GBP\" 3)
  }
}
"
        );
        // wide characters count by display width; trailing comments follow the `)`
        assert_eq!(
            aligned(
                "SELECT * {} VALUES (?a ?b) { (\"日本\" 1) # wide\n (\"ab\" 2) (\"abcdef\" 3) }",
                100
            ),
            "SELECT *
WHERE {}
VALUES (?a ?b) {
  (\"日本\"   1) # wide
  (\"ab\"     2)
  (\"abcdef\" 3)
}
"
        );
    }

    #[test]
    fn align_values_leaves_some_blocks_as_written() {
        // too wide once aligned (each row alone fits)
        let src = "SELECT * { VALUES (?a ?b) { (<http://example.org/a-rather-long-name> 1) (2 <http://example.org/another-long-name>) } }";
        assert_eq!(
            aligned(src, 60),
            crate::format(
                src,
                crate::Language::Sparql,
                &Options {
                    line_width: 60,
                    ..Options::default()
                }
            )
            .unwrap()
            .text
        );
        assert_eq!(
            aligned(src, 100),
            "SELECT *
WHERE {
  VALUES (?a ?b) {
    (<http://example.org/a-rather-long-name> 1)
    (2                                       <http://example.org/another-long-name>)
  }
}
"
        );
        // the widest aligned row is 80 columns at indent 4: it fits 84 exactly
        assert!(aligned(src, 84).contains("(2                                       <"));
        assert!(aligned(src, 83).contains("(2 <"));
        // a comment inside a row, an ignored row, a long string with a line break
        for src in [
            "SELECT * { VALUES (?a ?b) { (1 # one\n 2) (333 4) } }",
            "SELECT * { VALUES (?a ?b) {\n # sparkles-fmt: ignore\n (1   2)\n (333 4) } }",
            "SELECT * { VALUES (?a ?b) { (\"\"\"x\ny\"\"\" 2) (333 4) } }",
        ] {
            let plain = crate::format(src, crate::Language::Sparql, &Options::default())
                .unwrap()
                .text;
            assert_eq!(aligned(src, 100), plain, "{src}");
        }
        // one column: nothing to pad
        assert_eq!(
            aligned("SELECT * { VALUES (?a) { (1) (333) } }", 100),
            "SELECT *\nWHERE {\n  VALUES (?a) {\n    (1)\n    (333)\n  }\n}\n"
        );
    }
}
