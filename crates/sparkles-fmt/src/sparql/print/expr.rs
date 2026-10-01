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
use crate::{OperatorPosition, QuoteStyle};

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
/// The chain prints its operands' comments itself, because in the leading position an
/// operand's leading comments go before the operator that starts its line, and its
/// trailing comment after the operand, where the operator no longer is.
fn chain(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let leading = cx.opts.operator_position == OperatorPosition::Leading;
    let operands: Vec<NodeId> = cx.tree.child_nodes(n).collect();
    let mut first = Vec::new();
    let mut rest = Vec::new();
    let mut prev_op: Option<TokenId> = None;
    for (i, &o) in operands.iter().enumerate() {
        let (body, op) = operand(cx, o);
        let out = if i == 0 {
            &mut first
        } else {
            rest.push(cx.arena.line());
            &mut rest
        };
        for c in operand_leading(cx, o) {
            out.push(c);
            out.push(cx.arena.hard_line());
        }
        if leading {
            if let Some(op) = prev_op {
                out.push(cx.arena.token(op, None));
                out.push(cx.arena.text(" "));
            }
            out.push(body);
        } else {
            out.push(body);
            if let Some(op) = op {
                out.push(cx.arena.text(" "));
                out.push(cx.arena.token(op, None));
            }
        }
        for c in cx.comments.trailing(o).to_vec() {
            out.push(trailing_comment(cx, c));
        }
        prev_op = op;
    }
    let rest = cx.arena.concat(rest);
    first.push(cx.arena.indent(rest));
    let doc = cx.arena.concat(first);
    cx.arena.group(doc).0
}

/// A chain operand's expression (verbatim under an ignore pragma), and the operator
/// after it.
fn operand(cx: &mut Ctx<'_, '_>, o: NodeId) -> (DocId, Option<TokenId>) {
    let mut body = None;
    let mut op = None;
    for &e in cx.tree.children(o) {
        match e {
            Element::Token(t)
                if matches!(cx.tree.token_kind(t), TokenKind::OrOr | TokenKind::AndAnd) =>
            {
                op = Some(t);
            }
            e if body.is_none() => {
                body = Some(if cx.comments.ignored(o) {
                    match e {
                        Element::Node(c) => cx.verbatim(c),
                        Element::Token(t) => cx.arena.token(t, None),
                    }
                } else {
                    element(cx, e)
                });
            }
            _ => {}
        }
    }
    let body = body.unwrap_or_else(|| cx.arena.nil());
    (body, op)
}

/// The comments on their own lines before a chain operand: its detached blocks, then
/// its leading block.
fn operand_leading(cx: &mut Ctx<'_, '_>, o: NodeId) -> Vec<DocId> {
    let mut v: Vec<TokenId> = cx
        .comments
        .detached_before(o)
        .iter()
        .flatten()
        .copied()
        .collect();
    v.extend_from_slice(cx.comments.leading(o));
    v.into_iter().map(|c| comment(cx, c)).collect()
}

/// A comment token, without trailing whitespace.
fn comment(cx: &mut Ctx<'_, '_>, c: TokenId) -> DocId {
    let text = cx.tree.token_text(c);
    let trimmed = text.trim_end();
    let printed = (trimmed.len() != text.len()).then(|| trimmed.into());
    cx.arena.token(c, printed)
}

/// ` # comment` at the end of the line, which then cannot be joined with the next.
fn trailing_comment(cx: &mut Ctx<'_, '_>, c: TokenId) -> DocId {
    let space = cx.arena.text(" ");
    let c = comment(cx, c);
    let both = cx.arena.concat([space, c]);
    let suffix = cx.arena.line_suffix(both);
    let brk = cx.arena.break_parent();
    cx.arena.concat([suffix, brk])
}

/// `ChainOperand` outside a chain's own layout: the operand and the operator after it.
pub fn chain_operand(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let (body, op) = operand(cx, n);
    match op {
        Some(op) => {
            let space = cx.arena.text(" ");
            let op = cx.arena.token(op, None);
            cx.arena.concat([body, space, op])
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
    let children = cx.tree.children(n).to_vec();
    let fused_rhs = children.len() == 2;
    let mut parts = Vec::with_capacity(children.len() * 2);
    for (i, &e) in children.iter().enumerate() {
        if i > 0 {
            parts.push(cx.arena.text(" "));
        }
        let signed = (i == 0 && signed_first) || (i == 1 && fused_rhs);
        parts.push(if signed {
            signed_operand(cx, e)
        } else {
            element(cx, e)
        });
    }
    cx.arena.concat(parts)
}

/// The right operand of `a +1`: `+ 1`, or `+ 1 * 2` when the number starts operations.
fn signed_operand(cx: &mut Ctx<'_, '_>, e: Element) -> DocId {
    match e {
        Element::Token(t)
            if crate::sparql::parse::expr::is_signed_number(cx.tree.token_kind(t)) =>
        {
            let text = cx.tree.token_text(t);
            let printed = format!("{} {}", &text[..1], &text[1..]);
            cx.arena.token(t, Some(printed.into()))
        }
        Element::Node(m) if cx.tree.kind(m) == NodeKind::Binary => {
            let doc = binary_doc(cx, m, true);
            crate::trivia::wrap(&mut cx.arena, cx.comments, m, doc)
        }
        e => element(cx, e),
    }
}

/// `Unary`: the operator directly before its operand (`!BOUND(?x)`, `-?x`), except
/// that a sign stays apart from an unsigned number, which it would otherwise join
/// (`- 1` is not the token `-1`).
pub fn unary(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let children = cx.tree.children(n).to_vec();
    let mut parts = Vec::with_capacity(3);
    for (i, &e) in children.iter().enumerate() {
        if i == 1 {
            let joins = first_token(cx, e).is_some_and(|t| {
                matches!(
                    cx.tree.token_kind(t),
                    TokenKind::Integer | TokenKind::Decimal | TokenKind::Double
                )
            });
            if joins {
                parts.push(cx.arena.text(" "));
            }
        }
        parts.push(element(cx, e));
    }
    cx.arena.concat(parts)
}

/// `Bracketed`: `(expr)`, or broken as `(`, the expression one level deeper, `)`.
pub fn bracketed(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let children = cx.tree.children(n).to_vec();
    let (open, close) = match (children.first(), children.last()) {
        (Some(&Element::Token(o)), Some(&Element::Token(c))) if children.len() >= 2 => (o, c),
        _ => return cx.verbatim(n),
    };
    let inner: Vec<DocId> = children[1..children.len() - 1]
        .iter()
        .map(|&e| element(cx, e))
        .collect();
    brackets(cx, n, open, inner, close)
}

/// `(`, the items separated by lines, the container's dangling comments, `)`: one
/// group, broken one item per line one level deeper.
fn brackets(
    cx: &mut Ctx<'_, '_>,
    n: NodeId,
    open: TokenId,
    items: Vec<DocId>,
    close: TokenId,
) -> DocId {
    let mut inner = vec![cx.arena.soft_line()];
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            inner.push(cx.arena.line());
        }
        inner.push(item);
    }
    for c in cx.comments.dangling(n).to_vec() {
        inner.push(cx.arena.hard_line());
        inner.push(comment(cx, c));
    }
    let inner = cx.arena.concat(inner);
    let open = cx.arena.token(open, None);
    let inner = cx.arena.indent(inner);
    let soft = cx.arena.soft_line();
    let close = cx.arena.token(close, None);
    let doc = cx.arena.concat([open, inner, soft, close]);
    cx.arena.group(doc).0
}

/// `Call`: the name directly before its arguments: `STR(?x)`, `ex:fn(?a, ?b)`.
pub fn call(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let parts: Vec<DocId> = cx
        .tree
        .children(n)
        .to_vec()
        .into_iter()
        .map(|e| element(cx, e))
        .collect();
    cx.arena.concat(parts)
}

/// `ArgList`: `()`, or `(a, b)` broken one argument per line. `DISTINCT` goes before the
/// first argument, and `GROUP_CONCAT`'s `SEPARATOR = "…"` after the last.
pub fn arg_list(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let children = cx.tree.children(n).to_vec();
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
            Element::Token(t) => {
                let doc = term_token(cx, t);
                match items.last_mut() {
                    // `SEPARATOR = "…"` after the last argument
                    Some(item) => {
                        let space = cx.arena.text(" ");
                        item.extend([space, doc]);
                    }
                    // `DISTINCT`
                    None => {
                        let space = cx.arena.text(" ");
                        before_first.extend([doc, space]);
                    }
                }
            }
        }
    }
    let items = items.into_iter().map(|i| cx.arena.concat(i)).collect();
    brackets(cx, n, open, items, close)
}

/// `Arg`: the expression and the `,` or `;` after it.
pub fn arg(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    call(cx, n)
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
    let children = cx.tree.children(n).to_vec();
    let mut parts = Vec::with_capacity(children.len() * 2);
    for (i, &e) in children.iter().enumerate() {
        if i > 0 {
            parts.push(cx.arena.text(" "));
        }
        parts.push(element(cx, e));
    }
    cx.arena.concat(parts)
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

/// A token of an expression: keywords and built-in names in the grammar's spelling,
/// `NIL` as `()`, strings in double quotes unless `quote-style = "preserve"`; anything
/// else as written.
pub fn term_token(cx: &mut Ctx<'_, '_>, t: TokenId) -> DocId {
    let kind = cx.tree.token_kind(t);
    let text = cx.tree.token_text(t);
    let printed: Option<Box<str>> = match kind {
        TokenKind::Kw(k) => (text != k.canonical()).then(|| k.canonical().into()),
        TokenKind::Nil => (text != "()").then(|| "()".into()),
        k if k.is_string() && cx.opts.quote_style == QuoteStyle::Double => {
            crate::normalize::requote(text, k).map(Into::into)
        }
        _ => None,
    };
    cx.arena.token(t, printed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;
    use crate::doc::DocArena;
    use crate::sparql::parse::expr;
    use crate::trivia::Comments;

    /// `FILTER` and the constraint `src`, printed one level deep (as in a `WHERE`
    /// group) with `opts`.
    fn filter_with(src: &str, opts: &Options) -> String {
        let tree = expr::parse_with(src, expr::constraint);
        let comments = Comments::attach(&tree, &super::super::RULES);
        let mut cx = Ctx {
            tree: &tree,
            arena: DocArena::new(&tree.tokens),
            opts,
            comments: &comments,
        };
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
            "  FILTER NOT EXISTS {?s ?p ?o}"
        );
        assert_eq!(filter("(exists{} || ?a)"), "  FILTER(EXISTS {} || ?a)");
    }
}
