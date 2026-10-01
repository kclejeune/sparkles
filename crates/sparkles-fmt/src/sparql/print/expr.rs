//! Expressions: single spaces around binary operators, tight unary operators and
//! calls, `||`/`&&` chains that break one operand per line with the operator leading
//! or trailing (`operator-position`), argument lists, `IN`, `EXISTS`, and `?v+1`
//! printed `?v + 1`.
//!
//! Only `||` and `&&` chains, brackets and argument lists ever break. A broken chain
//! keeps its first operand where the chain starts and puts every later operand on a
//! continuation line one level deeper; brackets and argument lists break as `(`, the
//! content one level deeper, then `)` at the indentation of the line that opened them.
//! Parentheses are printed exactly as written.

use super::{Ctx, node};
use crate::doc::DocId;
use crate::lex::TokenKind;
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeId, TokenId};
use crate::{OperatorPosition, trivia};

/// `OrChain`: one group, broken one operand per line.
pub fn or_chain(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    chain(cx, n)
}

/// `AndChain`: as [`or_chain`].
pub fn and_chain(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    chain(cx, n)
}

/// A chain: the first operand where the chain starts, the others on continuation lines
/// one level deeper. The operator starts the continuation line (`leading`) or ends the
/// line before it (`trailing`).
///
/// The chain prints its operands' comments itself rather than through [`node`]: in the
/// leading position a later operand's own-line comments go between its operator and the
/// operand (before the operator they would belong to the operand before it), and its
/// trailing comment (one after the operator, too) goes after the operand, which now ends
/// the line.
fn chain(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let leading = cx.opts.operator_position == OperatorPosition::Leading;
    let mut first = Vec::new();
    let mut rest = Vec::new();
    let mut prev_op: Option<TokenId> = None;
    for (i, o) in cx.child_nodes(n).into_iter().enumerate() {
        let (body, op) = operand(cx, o);
        let out = if i == 0 {
            &mut first
        } else {
            rest.push(cx.line());
            &mut rest
        };
        let comments = operand_leading(cx, o);
        if leading {
            match prev_op {
                // the operator, then the operand's own-line comments, so that they still
                // come right before the operand: before the operator they would belong
                // to the operand before it
                Some(op) if !comments.is_empty() => {
                    out.push(cx.tok(op));
                    out.push(cx.hard_line());
                    out.extend(comments);
                }
                Some(op) => {
                    out.push(cx.tok(op));
                    out.push(cx.space());
                }
                None => out.extend(comments),
            }
            out.push(body);
        } else {
            out.extend(comments);
            out.push(body);
            if let Some(op) = op {
                out.push(cx.space());
                out.push(cx.tok(op));
            }
        }
        for c in cx.comments.trailing(o).to_vec() {
            if !cx.comments.copied(c) {
                out.push(trivia::trailing_comment(&mut cx.arena, cx.comments, c));
            }
        }
        prev_op = op;
    }
    let rest = cx.concat(rest);
    first.push(cx.indent(rest));
    let doc = cx.concat(first);
    cx.group(doc)
}

/// A chain operand's expression (verbatim under an ignore pragma), and the operator
/// after it.
fn operand(cx: &mut Ctx<'_, '_>, o: NodeId) -> (DocId, Option<TokenId>) {
    let mut body = None;
    let mut op = None;
    for e in cx.children(o) {
        match e {
            Element::Token(t)
                if matches!(cx.tree.token_kind(t), TokenKind::OrOr | TokenKind::AndAnd) =>
            {
                op = Some(t);
            }
            e if body.is_none() => {
                body = Some(match e {
                    Element::Node(c) if cx.comments.ignored(o) => cx.verbatim(c),
                    Element::Token(t) if cx.comments.ignored(o) => cx.tok(t),
                    e => element(cx, e),
                });
            }
            _ => {}
        }
    }
    let body = body.unwrap_or_else(|| cx.nil());
    (body, op)
}

/// The comments on their own lines before a chain operand, each line ending with a line
/// break: its detached blocks (a blank line after each), then its leading block.
fn operand_leading(cx: &mut Ctx<'_, '_>, o: NodeId) -> Vec<DocId> {
    let live = |cs: &[TokenId]| -> Vec<TokenId> {
        cs.iter()
            .copied()
            .filter(|&c| !cx.comments.copied(c))
            .collect()
    };
    let mut out = Vec::new();
    for block in cx.comments.detached_before(o).to_vec() {
        let block = live(&block);
        if !block.is_empty() {
            out.push(trivia::comment_lines(&mut cx.arena, cx.comments, &block));
            out.push(cx.empty_line());
        }
    }
    let lead = live(cx.comments.leading(o));
    if !lead.is_empty() {
        out.push(trivia::comment_lines(&mut cx.arena, cx.comments, &lead));
        out.push(cx.hard_line());
    }
    out
}

/// `ChainOperand` outside a chain's own layout: the operand and the operator after it.
pub fn chain_operand(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let (body, op) = operand(cx, n);
    match op {
        Some(op) => {
            let op = cx.tok(op);
            cx.spaced([body, op])
        }
        None => body,
    }
}

/// `Binary`: the operands and the operator separated by single spaces; it never breaks.
/// A `Binary` of two children is `a +1`: the signed number is printed `+ 1`.
pub fn binary(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    binary_doc(cx, n, false)
}

/// A `Binary`, whose first token is a signed number standing for the operator and the
/// operand (`signed_first`) when it is the right side of `a +1 * 2`.
fn binary_doc(cx: &mut Ctx<'_, '_>, n: NodeId, signed_first: bool) -> DocId {
    let children = cx.children(n);
    let fused_rhs = children.len() == 2;
    let docs: Vec<DocId> = children
        .iter()
        .enumerate()
        .map(|(i, &e)| {
            let signed = (i == 0 && signed_first) || (i == 1 && fused_rhs);
            if signed {
                signed_operand(cx, e)
            } else {
                element(cx, e)
            }
        })
        .collect();
    cx.spaced(docs)
}

/// The right operand of `a +1`: `+ 1`, or `+ 1 * 2` when the number starts operations.
fn signed_operand(cx: &mut Ctx<'_, '_>, e: Element) -> DocId {
    match e {
        Element::Token(t)
            if crate::sparql::parse::expr::is_signed_number(cx.tree.token_kind(t)) =>
        {
            let text = cx.tree.token_text(t);
            let printed = format!("{} {}", &text[..1], &text[1..]);
            cx.tok_as(t, printed)
        }
        Element::Node(m) if cx.tree.kind(m) == NodeKind::Binary => {
            let doc = binary_doc(cx, m, true);
            trivia::wrap(&mut cx.arena, cx.comments, m, doc)
        }
        e => element(cx, e),
    }
}

/// `Unary`: the operator directly before its operand (`!BOUND(?x)`, `-?x`), except
/// that a sign stays apart from an unsigned number, which it would otherwise join
/// (`- 1` is not the token `-1`).
pub fn unary(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let mut parts = Vec::with_capacity(3);
    for (i, e) in cx.children(n).into_iter().enumerate() {
        if i == 1 {
            let joins = first_token(cx, e).is_some_and(|t| {
                matches!(
                    cx.tree.token_kind(t),
                    TokenKind::Integer | TokenKind::Decimal | TokenKind::Double
                )
            });
            if joins {
                parts.push(cx.space());
            }
        }
        parts.push(element(cx, e));
    }
    cx.concat(parts)
}

/// `Bracketed`: `(expr)`, or broken as `(`, the expression one level deeper, `)`.
pub fn bracketed(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let children = cx.children(n);
    let (open, close) = match (children.first(), children.last()) {
        (Some(&Element::Token(o)), Some(&Element::Token(c))) if children.len() >= 2 => (o, c),
        _ => return cx.verbatim(n),
    };
    let inner: Vec<DocId> = children[1..children.len() - 1]
        .iter()
        .map(|&e| element(cx, e))
        .collect();
    let open = cx.tok(open);
    let close = cx.tok(close);
    cx.delimited(n, open, &inner, close, false)
}

/// `Call`: the name directly before its arguments: `STR(?x)`, `ex:fn(?a, ?b)`.
pub fn call(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let parts: Vec<DocId> = cx.children(n).into_iter().map(|e| element(cx, e)).collect();
    cx.concat(parts)
}

/// `ArgList`: `()`, or `(a, b)` broken one argument per line. `DISTINCT` goes before the
/// first argument.
pub fn arg_list(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let children = cx.children(n);
    let (open, close) = match (children.first(), children.last()) {
        (Some(&Element::Token(o)), Some(&Element::Token(c))) if children.len() >= 2 => (o, c),
        (Some(&Element::Token(t)), _) if children.len() == 1 => return term_token(cx, t),
        _ => return cx.verbatim(n),
    };
    let mut items: Vec<Vec<DocId>> = Vec::new();
    let mut before_first: Vec<DocId> = Vec::new();
    for &e in &children[1..children.len() - 1] {
        match e {
            Element::Node(a) => {
                let mut item = std::mem::take(&mut before_first);
                item.push(node(cx, a));
                items.push(item);
            }
            // `DISTINCT`
            Element::Token(t) => {
                let doc = term_token(cx, t);
                let space = cx.space();
                before_first.extend([doc, space]);
            }
        }
    }
    let items: Vec<DocId> = items.into_iter().map(|i| cx.concat(i)).collect();
    let open = cx.tok(open);
    let close = cx.tok(close);
    cx.delimited(n, open, &items, close, false)
}

/// `Arg`: the expression and the `,` after it, or `GROUP_CONCAT`'s `?n; SEPARATOR =
/// ", "`.
pub fn arg(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let mut parts = Vec::new();
    let mut after_semicolon = false;
    for e in cx.children(n) {
        if after_semicolon {
            parts.push(cx.space());
        }
        if let Element::Token(t) = e {
            after_semicolon |= cx.tree.token_kind(t) == TokenKind::Semicolon;
        }
        parts.push(element(cx, e));
    }
    cx.concat(parts)
}

/// `Aggregate`: as a call, `COUNT(DISTINCT ?x)`, `GROUP_CONCAT(?n; SEPARATOR = ", ")`.
pub fn aggregate(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    call(cx, n)
}

/// `InList`: `?x IN (1, 2)`, `?x NOT IN (…)`.
pub fn in_list(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    spaced(cx, n)
}

/// `Exists`: `EXISTS {`, the group, `}`.
pub fn exists(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    spaced(cx, n)
}

/// `NotExists`: `NOT EXISTS {`, the group, `}`.
pub fn not_exists(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    spaced(cx, n)
}

/// The children separated by single spaces.
fn spaced(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let docs: Vec<DocId> = cx.children(n).into_iter().map(|e| element(cx, e)).collect();
    cx.spaced(docs)
}

/// The first significant token of a child.
fn first_token(cx: &Ctx<'_, '_>, e: Element) -> Option<TokenId> {
    match e {
        Element::Token(t) => Some(t),
        Element::Node(c) => cx.tree.first_token(c),
    }
}

/// A child of an expression node: a node through the dispatcher, a token as
/// [`term_token`] prints it.
fn element(cx: &mut Ctx<'_, '_>, e: Element) -> DocId {
    match e {
        Element::Node(c) => node(cx, c),
        Element::Token(t) => term_token(cx, t),
    }
}

/// A token of an expression: `NIL` as `()`, anything else as [`Ctx::term`] prints it
/// (keywords in the grammar's spelling, IRIs compacted, strings requoted).
pub fn term_token(cx: &mut Ctx<'_, '_>, t: TokenId) -> DocId {
    match cx.tree.token_kind(t) {
        TokenKind::Nil => cx.tok_as(t, "()"),
        _ => cx.term(t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparql::parse::expr;
    use crate::trivia::Comments;
    use crate::{Options, QuoteStyle};

    /// `FILTER` and the constraint `src`, printed one level deep (as in a `WHERE`
    /// group) with `opts`.
    fn filter_with(src: &str, opts: &Options) -> String {
        let tree = expr::parse_with(src, expr::constraint);
        let comments = Comments::attach(&tree, &super::super::RULES);
        let mut cx = Ctx::new(&tree, &comments, opts);
        let c = tree.child_nodes(tree.root()).next().unwrap();
        let doc = node(&mut cx, c);
        let nl = cx.arena.hard_line();
        let kw = cx.arena.text("FILTER");
        let sp = if tree.kind(c) == NodeKind::Bracketed {
            cx.arena.nil()
        } else {
            cx.arena.text(" ")
        };
        let all = cx.arena.concat([nl, kw, sp, doc]);
        let all = cx.arena.indent(all);
        let printed = crate::doc::print(
            &cx.arena,
            all,
            tree.src,
            opts.line_width,
            opts.indent_width,
            None,
        )
        .unwrap();
        printed.text.strip_prefix('\n').unwrap().to_string()
    }

    fn filter(src: &str) -> String {
        filter_with(src, &Options::default())
    }

    #[test]
    fn spaces_around_binary_operators_and_tight_unary() {
        assert_eq!(filter("(?a+?b*2>=3)"), "  FILTER(?a + ?b * 2 >= 3)");
        assert_eq!(filter("(!bound(?x))"), "  FILTER(!BOUND(?x))");
        assert_eq!(filter("(-?x<-1)"), "  FILTER(-?x < -1)");
        assert_eq!(filter("(!!?x)"), "  FILTER(!!?x)");
        // a sign stays apart from an unsigned number
        assert_eq!(filter("(- 1 = + 2.5)"), "  FILTER(- 1 = + 2.5)");
        assert_eq!(filter("(--1 = -?x)"), "  FILTER(--1 = -?x)");
    }

    #[test]
    fn a_signed_number_after_an_operand_gets_its_spaces() {
        assert_eq!(filter("(?v+1)"), "  FILTER(?v + 1)");
        assert_eq!(
            filter("(?v-1.5e3*2/?y = 0)"),
            "  FILTER(?v - 1.5e3 * 2 / ?y = 0)"
        );
        assert_eq!(filter("(?v * -1 +-1)"), "  FILTER(?v * -1 + -1)");
        assert_eq!(filter("((?v)+1)"), "  FILTER((?v) + 1)");
        assert_eq!(filter("(NOW()-1)"), "  FILTER(NOW() - 1)");
    }

    #[test]
    fn calls_keywords_and_lists() {
        assert_eq!(
            filter("( lang(?name)=\"en\" || lang(?name)=\"\" )"),
            "  FILTER(LANG(?name) = \"en\" || LANG(?name) = \"\")"
        );
        assert_eq!(
            filter("regex(?a,'x' , \"i\")"),
            "  FILTER REGEX(?a, \"x\", \"i\")"
        );
        let preserve = Options {
            quote_style: QuoteStyle::Preserve,
            ..Options::default()
        };
        assert_eq!(
            filter_with("regex(?a,'x' , \"i\")", &preserve),
            "  FILTER REGEX(?a, 'x', \"i\")"
        );
        assert_eq!(
            filter("<http://e/f>(?a,?b)"),
            "  FILTER <http://e/f>(?a, ?b)"
        );
        assert_eq!(
            filter("(sameterm(?a, ?b) && isiri(?a))"),
            "  FILTER(sameTerm(?a, ?b) && isIRI(?a))"
        );
        assert_eq!(filter("(?x in(1,2))"), "  FILTER(?x IN (1, 2))");
        assert_eq!(filter("(?x not  in ( ))"), "  FILTER(?x NOT IN ())");
        assert_eq!(
            filter("(bnode( ) != uuid())"),
            "  FILTER(BNODE() != UUID())"
        );
        assert_eq!(
            filter("(count(distinct *) > sum(DISTINCT ?x))"),
            "  FILTER(COUNT(DISTINCT *) > SUM(DISTINCT ?x))"
        );
        assert_eq!(
            filter("(group_concat(?n;separator=\", \") != ex:agg(distinct ?n))"),
            "  FILTER(GROUP_CONCAT(?n; SEPARATOR = \", \") != ex:agg(DISTINCT ?n))"
        );
        assert_eq!(
            filter("(haslangdir(?x) || STRLANGDIR(\"a\", \"en\", \"ltr\") = triple(?s,?p,?o))"),
            "  FILTER(hasLANGDIR(?x) || STRLANGDIR(\"a\", \"en\", \"ltr\") = TRIPLE(?s, ?p, ?o))"
        );
        assert_eq!(filter("(?b = TRUE)"), "  FILTER(?b = true)");
    }

    #[test]
    fn exists() {
        assert_eq!(
            filter("not  exists {?s ?p ?o}"),
            "  FILTER NOT EXISTS {\n    ?s ?p ?o .\n  }"
        );
        assert_eq!(filter("(exists{} || ?a)"), "  FILTER(EXISTS {} || ?a)");
    }

    fn opts(width: u16, position: OperatorPosition) -> Options {
        Options {
            line_width: width,
            operator_position: position,
            ..Options::default()
        }
    }

    const LONG: &str = "(?price > \"10\"^^<http://www.w3.org/2001/XMLSchema#decimal> && ?price < 1000 && contains(lcase(str(?s)), \"sale\") && ?currency != \"GBP\")";

    #[test]
    fn a_long_chain_breaks_with_leading_operators() {
        assert_eq!(
            filter(LONG),
            "  FILTER(
    ?price > \"10\"^^<http://www.w3.org/2001/XMLSchema#decimal>
      && ?price < 1000
      && CONTAINS(LCASE(STR(?s)), \"sale\")
      && ?currency != \"GBP\"
  )"
        );
    }

    #[test]
    fn a_long_chain_breaks_with_trailing_operators() {
        assert_eq!(
            filter_with(LONG, &opts(100, OperatorPosition::Trailing)),
            "  FILTER(
    ?price > \"10\"^^<http://www.w3.org/2001/XMLSchema#decimal> &&
      ?price < 1000 &&
      CONTAINS(LCASE(STR(?s)), \"sale\") &&
      ?currency != \"GBP\"
  )"
        );
    }

    #[test]
    fn short_chains_break_only_when_they_do_not_fit() {
        let src = "(?price > 10 && ?currency != \"GBP\")";
        assert_eq!(
            filter_with(src, &opts(43, OperatorPosition::Leading)),
            "  FILTER(?price > 10 && ?currency != \"GBP\")"
        );
        assert_eq!(
            filter_with(src, &opts(30, OperatorPosition::Leading)),
            "  FILTER(
    ?price > 10
      && ?currency != \"GBP\"
  )"
        );
        assert_eq!(
            filter_with(src, &opts(30, OperatorPosition::Trailing)),
            "  FILTER(
    ?price > 10 &&
      ?currency != \"GBP\"
  )"
        );
    }

    #[test]
    fn nested_chains_go_one_level_deeper() {
        assert_eq!(
            filter_with("(?a || (?b && ?c))", &opts(12, OperatorPosition::Leading)),
            "  FILTER(
    ?a
      || (
        ?b
          && ?c
      )
  )"
        );
        assert_eq!(
            filter_with("(?a || (?b && ?c))", &opts(12, OperatorPosition::Trailing)),
            "  FILTER(
    ?a ||
      (
        ?b &&
          ?c
      )
  )"
        );
        // an && chain is an operand of the || chain without brackets
        assert_eq!(
            filter_with(
                "(?a || ?bb && ?cc || ?d)",
                &opts(20, OperatorPosition::Leading)
            ),
            "  FILTER(
    ?a
      || ?bb && ?cc
      || ?d
  )"
        );
        assert_eq!(
            filter_with(
                "(?aaaa || ?bbbb && ?cccc)",
                &opts(16, OperatorPosition::Leading)
            ),
            "  FILTER(
    ?aaaa
      || ?bbbb
        && ?cccc
  )"
        );
    }

    #[test]
    fn argument_lists_and_in_lists_break_one_item_per_line() {
        assert_eq!(
            filter_with(
                "concat(?aaaa, ?bbbb, \"cccc\")",
                &opts(20, OperatorPosition::Leading)
            ),
            "  FILTER CONCAT(
    ?aaaa,
    ?bbbb,
    \"cccc\"
  )"
        );
        assert_eq!(
            filter_with(
                "(?x not in (?aaaa, ?bbbb))",
                &opts(24, OperatorPosition::Leading)
            ),
            "  FILTER(
    ?x NOT IN (
      ?aaaa,
      ?bbbb
    )
  )"
        );
        assert_eq!(
            filter_with(
                "(group_concat(distinct ?label; separator=\", \") != \"\")",
                &opts(40, OperatorPosition::Leading)
            ),
            "  FILTER(
    GROUP_CONCAT(
      DISTINCT ?label; SEPARATOR = \", \"
    ) != \"\"
  )"
        );
    }

    #[test]
    fn comments_after_operators() {
        for src in ["(?a && # c\n ?b)", "(?a # c\n && ?b)"] {
            assert_eq!(
                filter(src),
                "  FILTER(
    ?a # c
      && ?b
  )",
                "{src:?}"
            );
            assert_eq!(
                filter_with(src, &opts(100, OperatorPosition::Trailing)),
                "  FILTER(
    ?a && # c
      ?b
  )",
                "{src:?}"
            );
        }
        // an own-line comment before an operand stays right before it: after the operator
        let src = "(?a &&\n # c\n ?b || ?d)";
        assert_eq!(
            filter(src),
            "  FILTER(
    ?a
      &&
      # c
      ?b
      || ?d
  )"
        );
        assert_eq!(
            filter_with(src, &opts(100, OperatorPosition::Trailing)),
            "  FILTER(
    ?a &&
      # c
      ?b ||
      ?d
  )"
        );
    }

    #[test]
    fn comments_in_argument_lists() {
        assert_eq!(
            filter("concat(?a, # c\n ?b)"),
            "  FILTER CONCAT(
    ?a, # c
    ?b
  )"
        );
        assert_eq!(
            filter("(?a\n # dangling\n)"),
            "  FILTER(
    ?a
    # dangling
  )"
        );
    }
}
