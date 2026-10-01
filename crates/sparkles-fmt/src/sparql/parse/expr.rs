//! Expressions: one function per precedence level (`||`, `&&`, relational, additive,
//! multiplicative, unary), built-in calls, aggregates, function calls, `IN`, `EXISTS`,
//! and RDF 1.2 triple terms in expressions.
//!
//! The entry points the pattern and query parsers call: [`expression`], [`bracketted`],
//! [`builtin_or_call`], [`constraint`].
//!
//! The shapes it builds:
//! - an `||` or `&&` chain of two or more operands is an `OrChain` or `AndChain` of
//!   `ChainOperand`s, each holding its operand and the operator after it;
//! - `a op b` is a `Binary` (left-associative within a level);
//! - in `a +1` the signed number is the operator and the right operand in one token: a
//!   `Binary` of two children, the left operand and the right one (the number alone, or
//!   the `Binary` of the `*` `/` operations it starts);
//! - `!e`, `+e`, `-e` are `Unary`; `( e )` is `Bracketed`;
//! - calls are `Call` (a built-in name or an IRI, then an `ArgList`), `Aggregate` (the
//!   name, then an `ArgList`), `Exists` and `NotExists` (the keywords, then the group);
//! - `e IN (…)` and `e NOT IN (…)` are `InList` (the operand, the keywords, an `ArgList`);
//! - an `ArgList` is `NIL`, or `(`, an optional `DISTINCT`, the `Arg`s (each an
//!   expression, or the `*` of `COUNT(*)`, with the `,` after it, or the `;
//!   SEPARATOR = "…"` of `GROUP_CONCAT`), and `)`;
//! - `<<( s p o )>>` is a `TripleTerm` of its tokens (the object may be a `Literal` or a
//!   nested `TripleTerm`).
//!
//! A variable, an IRI, a number or a boolean alone is a token; a tagged or typed string
//! is a `Literal` node.

use super::term::{self, TripleTermCtx};
use super::{Completed, Marker, Parser};
use crate::lex::TokenKind;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind;

/// `Expression ::= ConditionalOrExpression`.
///
/// Returns the expression's node, or `None` when the expression is a single term (a
/// variable, an IRI, a literal, a triple term) or after an error ([`Parser::has_error`]
/// tells which).
pub fn expression(p: &mut Parser<'_>) -> Option<Completed> {
    chain(p, TokenKind::OrOr, NodeKind::OrChain, and_chain)
}

fn and_chain(p: &mut Parser<'_>) -> Option<Completed> {
    chain(p, TokenKind::AndAnd, NodeKind::AndChain, relational)
}

/// One level of `||` or `&&`: a chain node only when the operator occurs.
fn chain(
    p: &mut Parser<'_>,
    op: TokenKind,
    kind: NodeKind,
    operand: fn(&mut Parser<'_>) -> Option<Completed>,
) -> Option<Completed> {
    let first = p.start(NodeKind::ChainOperand);
    let single = operand(p);
    if !p.at(op) {
        first.abandon(p);
        return single;
    }
    p.bump();
    let m = first.complete(p).precede(p, kind);
    loop {
        let o = p.start(NodeKind::ChainOperand);
        operand(p);
        let more = p.eat(op);
        o.complete(p);
        if !more || p.has_error() {
            break;
        }
    }
    Some(m.complete(p))
}

/// `RelationalExpression ::= NumericExpression ( ( '=' | '!=' | '<' | '>' | '<=' | '>=' )
/// NumericExpression | 'IN' ExpressionList | 'NOT' 'IN' ExpressionList )?`.
fn relational(p: &mut Parser<'_>) -> Option<Completed> {
    use TokenKind as T;
    let m = p.start(NodeKind::Binary);
    let lhs = additive(p);
    if matches!(
        p.current(),
        T::Eq | T::NotEq | T::Lt | T::Gt | T::Le | T::Ge
    ) {
        p.bump();
        additive(p);
        return Some(m.complete(p));
    }
    if p.at_kw(Kw::In) || (p.at_kw(Kw::Not) && p.nth_at_kw(1, Kw::In)) {
        p.eat_kw(Kw::Not);
        p.expect_kw(Kw::In);
        arg_list(p, false);
        return Some(m.complete_as(p, NodeKind::InList));
    }
    m.abandon(p);
    lhs
}

/// `AdditiveExpression ::= MultiplicativeExpression ( '+' MultiplicativeExpression | '-'
/// MultiplicativeExpression | ( NumericLiteralPositive | NumericLiteralNegative ) ( (
/// '*' UnaryExpression ) | ( '/' UnaryExpression ) )* )*`.
fn additive(p: &mut Parser<'_>) -> Option<Completed> {
    // opened before the first operand, which may be a token `precede` cannot wrap
    let mut open = Some(p.start(NodeKind::Binary));
    let mut last = multiplicative(p);
    loop {
        let k = p.current();
        let signed = is_signed_number(k);
        if !(signed || matches!(k, TokenKind::Plus | TokenKind::Minus)) {
            break;
        }
        let m = left(p, &mut open, last);
        if signed {
            // the sign is the operator, and the number starts the right operand
            let rhs = p.start(NodeKind::Binary);
            p.bump();
            multiplicative_rest(p, rhs, None);
        } else {
            p.bump();
            multiplicative(p);
        }
        last = Some(m.complete(p));
    }
    if let Some(m) = open {
        m.abandon(p);
    }
    last
}

/// `MultiplicativeExpression ::= UnaryExpression ( '*' UnaryExpression | '/'
/// UnaryExpression )*`.
fn multiplicative(p: &mut Parser<'_>) -> Option<Completed> {
    let m = p.start(NodeKind::Binary);
    let first = unary(p);
    multiplicative_rest(p, m, first)
}

/// The `*` `/` operations after an operand: `m` was opened before the operand, `last`
/// is the operand's node if it has one.
fn multiplicative_rest(
    p: &mut Parser<'_>,
    m: Marker,
    mut last: Option<Completed>,
) -> Option<Completed> {
    let mut open = Some(m);
    while matches!(p.current(), TokenKind::Star | TokenKind::Slash) {
        let m = left(p, &mut open, last);
        p.bump();
        unary(p);
        last = Some(m.complete(p));
    }
    if let Some(m) = open {
        m.abandon(p);
    }
    last
}

/// The `Binary` around the left operand: the marker opened before the first operand,
/// or one around the previous operation (left associativity).
fn left(p: &mut Parser<'_>, open: &mut Option<Marker>, last: Option<Completed>) -> Marker {
    match (open.take(), last) {
        (Some(m), _) => m,
        (None, Some(c)) => c.precede(p, NodeKind::Binary),
        (None, None) => p.start(NodeKind::Binary),
    }
}

/// `UnaryExpression ::= '!' UnaryExpression | '+' PrimaryExpression | '-'
/// PrimaryExpression | PrimaryExpression` (SPARQL 1.2 allows `!!e`).
fn unary(p: &mut Parser<'_>) -> Option<Completed> {
    match p.current() {
        TokenKind::Bang => {
            let m = p.start(NodeKind::Unary);
            p.bump();
            unary(p);
            Some(m.complete(p))
        }
        TokenKind::Plus | TokenKind::Minus => {
            let m = p.start(NodeKind::Unary);
            p.bump();
            primary(p);
            Some(m.complete(p))
        }
        _ => primary(p),
    }
}

/// `PrimaryExpression ::= BrackettedExpression | BuiltInCall | iriOrFunction |
/// RDFLiteral | NumericLiteral | BooleanLiteral | Var | ExprTripleTerm`.
fn primary(p: &mut Parser<'_>) -> Option<Completed> {
    use TokenKind as T;
    match p.current() {
        T::LParen => bracketted(p),
        T::LtLtParen => {
            // `ExprTripleTerm`: an IRI or variable subject, a verb, a term or a nested
            // triple term as the object
            term::triple_term(p, TripleTermCtx::Expr);
            None
        }
        k if term::is_iri(k) => {
            if matches!(p.nth(1), T::LParen | T::Nil) {
                builtin_or_call(p)
            } else {
                term::iri(p);
                None
            }
        }
        T::Var1 | T::Var2 => {
            p.bump();
            None
        }
        _ if term::at_literal(p) => {
            term::literal(p);
            None
        }
        T::Word => match p.current_kw() {
            Some(kw) if kw.is_builtin() || matches!(kw, Kw::Exists | Kw::Not) => builtin_or_call(p),
            _ => {
                p.error("expected an expression");
                None
            }
        },
        _ => {
            p.error("expected an expression");
            None
        }
    }
}

/// `BrackettedExpression ::= '(' Expression ')'`: a `Bracketed` node.
pub fn bracketted(p: &mut Parser<'_>) -> Option<Completed> {
    if !p.at(TokenKind::LParen) {
        p.error("expected (");
        return None;
    }
    let m = p.start(NodeKind::Bracketed);
    p.bump();
    expression(p);
    p.expect(TokenKind::RParen);
    Some(m.complete(p))
}

/// `BuiltInCall | FunctionCall`: a built-in name and its arguments, an aggregate,
/// `EXISTS { … }`, `NOT EXISTS { … }`, or an IRI and its arguments (a function, or a
/// custom aggregate with `DISTINCT`).
pub fn builtin_or_call(p: &mut Parser<'_>) -> Option<Completed> {
    use TokenKind as T;
    match p.current() {
        k if term::is_iri(k) => {
            let m = p.start(NodeKind::Call);
            term::iri(p);
            arg_list(p, true);
            return Some(m.complete(p));
        }
        T::Word => {}
        _ => {
            p.error("expected a function call");
            return None;
        }
    }
    match p.current_kw() {
        Some(Kw::Exists) => {
            let m = p.start(NodeKind::Exists);
            p.bump_as(T::Kw(Kw::Exists));
            super::pattern::group_graph_pattern(p);
            Some(m.complete(p))
        }
        Some(Kw::Not) if p.nth_at_kw(1, Kw::Exists) => {
            let m = p.start(NodeKind::NotExists);
            p.bump_as(T::Kw(Kw::Not));
            p.bump_as(T::Kw(Kw::Exists));
            super::pattern::group_graph_pattern(p);
            Some(m.complete(p))
        }
        Some(kw) if kw.is_aggregate() => {
            let m = p.start(NodeKind::Aggregate);
            p.bump_as(T::Kw(kw));
            aggregate_args(p, kw);
            Some(m.complete(p))
        }
        Some(kw) if kw.is_builtin() => {
            let m = p.start(NodeKind::Call);
            p.bump_as(T::Kw(kw));
            arg_list(p, false);
            Some(m.complete(p))
        }
        _ => {
            p.error("expected a function call");
            None
        }
    }
}

/// `Constraint ::= BrackettedExpression | BuiltInCall | FunctionCall`.
pub fn constraint(p: &mut Parser<'_>) -> Option<Completed> {
    if p.at(TokenKind::LParen) {
        bracketted(p)
    } else {
        builtin_or_call(p)
    }
}

/// `ArgList ::= NIL | '(' 'DISTINCT'? Expression ( ',' Expression )* ')'`, and
/// `ExpressionList` (without `DISTINCT`): an `ArgList` node of `Arg`s.
fn arg_list(p: &mut Parser<'_>, distinct: bool) {
    let m = p.start(NodeKind::ArgList);
    if !p.eat(TokenKind::Nil) {
        p.expect(TokenKind::LParen);
        if distinct {
            p.eat_kw(Kw::Distinct);
        }
        loop {
            let a = p.start(NodeKind::Arg);
            expression(p);
            let more = p.eat(TokenKind::Comma);
            a.complete(p);
            if !more || p.has_error() {
                break;
            }
        }
        p.expect(TokenKind::RParen);
    }
    m.complete(p);
}

/// The brackets of an aggregate: `'(' 'DISTINCT'? ( '*' | Expression ) ')'`, and for
/// `GROUP_CONCAT` the `';' 'SEPARATOR' '=' String` before the `)`.
fn aggregate_args(p: &mut Parser<'_>, kw: Kw) {
    let m = p.start(NodeKind::ArgList);
    p.expect(TokenKind::LParen);
    p.eat_kw(Kw::Distinct);
    let a = p.start(NodeKind::Arg);
    if kw == Kw::Count && p.at(TokenKind::Star) {
        p.bump();
    } else {
        let from = p.events.len();
        expression(p);
        if contains_aggregate(&p.events[from..]) {
            p.error("aggregate functions cannot be nested");
        }
    }
    // the separator goes with the argument, so that a comment after the argument stays
    // the argument's wherever it is printed
    if kw == Kw::GroupConcat && p.eat(TokenKind::Semicolon) {
        p.expect_kw(Kw::Separator);
        p.expect(TokenKind::Eq);
        if p.current().is_string() && !matches!(p.nth(1), TokenKind::LangDir | TokenKind::HatHat) {
            // a plain string, its escapes checked
            term::literal(p);
        } else {
            p.error("expected a string");
        }
    }
    a.complete(p);
    p.expect(TokenKind::RParen);
    m.complete(p);
}

/// Whether the events of an aggregate's argument start an aggregate of the same query
/// level: one outside the groups of `EXISTS` (whose subqueries have their own
/// aggregates). SPARQL 1.2 §19.7: "The expression argument of an aggregate function
/// cannot contain an aggregate function."
fn contains_aggregate(events: &[super::Event]) -> bool {
    let mut open: Vec<NodeKind> = Vec::new();
    for e in events {
        match e {
            super::Event::Start { kind, .. } => {
                if *kind == NodeKind::Aggregate && !open.contains(&NodeKind::GroupGraphPattern) {
                    return true;
                }
                open.push(*kind);
            }
            super::Event::Finish => {
                open.pop();
            }
            _ => {}
        }
    }
    false
}

/// The signed numeric literal kinds: directly after an operand, the sign is the
/// additive operator (`?v+1` is `?v + 1`).
pub fn is_signed_number(k: TokenKind) -> bool {
    use TokenKind as T;
    matches!(
        k,
        T::IntegerPositive
            | T::DecimalPositive
            | T::DoublePositive
            | T::IntegerNegative
            | T::DecimalNegative
            | T::DoubleNegative
    )
}

/// The tree of `src` parsed with `f` under a `QueryUnit` root (for tests of the
/// expression parser and printer).
#[cfg(test)]
pub(crate) fn parse_with(
    src: &str,
    f: fn(&mut Parser<'_>) -> Option<Completed>,
) -> crate::tree::Tree<'_> {
    let tokens = crate::lex::lex(src, crate::lex::LexMode::Sparql);
    let events = {
        let mut p = Parser::new(src, &tokens);
        let root = p.start(NodeKind::QueryUnit);
        f(&mut p);
        assert!(p.at(TokenKind::Eof), "not all consumed: {src}");
        root.complete(&mut p);
        p.finish().unwrap_or_else(|e| panic!("{src}: {e}"))
    };
    super::build(src, tokens, events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{LexMode, lex};
    use crate::sparql::parse::build;

    /// The dump of the expression's tree, without the root line.
    fn dump(src: &str) -> String {
        let t = parse_with(src, expression);
        t.dump().split_once('\n').unwrap().1.to_string()
    }

    fn error(src: &str) -> String {
        let tokens = lex(src, LexMode::Sparql);
        let mut p = Parser::new(src, &tokens);
        let root = p.start(NodeKind::QueryUnit);
        expression(&mut p);
        if !p.at(TokenKind::Eof) {
            p.error("expected the end of the input");
        }
        root.complete(&mut p);
        p.finish().unwrap_err().to_string()
    }

    #[test]
    fn single_tokens_have_no_node() {
        for src in [
            "?x",
            "$x",
            "<http://e/>",
            "ex:a",
            "1",
            "-1.5",
            "true",
            "\"s\"",
        ] {
            let t = parse_with(src, expression);
            assert_eq!(t.child_nodes(t.root()).count(), 0, "{src}");
        }
        assert_eq!(dump("TRUE"), "  Kw(True) \"TRUE\"\n");
    }

    #[test]
    fn chains_are_flat_and_nest_by_precedence() {
        assert_eq!(
            dump("?a || ?b && ?c || ?d"),
            "  OrChain
    ChainOperand
      Var1 \"?a\"
      OrOr \"||\"
    ChainOperand
      AndChain
        ChainOperand
          Var1 \"?b\"
          AndAnd \"&&\"
        ChainOperand
          Var1 \"?c\"
      OrOr \"||\"
    ChainOperand
      Var1 \"?d\"
"
        );
    }

    #[test]
    fn binary_operators_are_left_associative() {
        assert_eq!(
            dump("?a - ?b - ?c * ?d = 3"),
            "  Binary
    Binary
      Binary
        Var1 \"?a\"
        Minus \"-\"
        Var1 \"?b\"
      Minus \"-\"
      Binary
        Var1 \"?c\"
        Star \"*\"
        Var1 \"?d\"
    Eq \"=\"
    Integer \"3\"
"
        );
    }

    #[test]
    fn a_signed_number_after_an_operand_is_the_operator() {
        assert_eq!(
            dump("?v+1"),
            "  Binary\n    Var1 \"?v\"\n    IntegerPositive \"+1\"\n"
        );
        assert_eq!(
            dump("?v -1*2/?x"),
            "  Binary
    Var1 \"?v\"
    Binary
      Binary
        IntegerNegative \"-1\"
        Star \"*\"
        Integer \"2\"
      Slash \"/\"
      Var1 \"?x\"
"
        );
        // not after an operand: a literal
        assert_eq!(
            dump("?v * -1"),
            "  Binary\n    Var1 \"?v\"\n    Star \"*\"\n    IntegerNegative \"-1\"\n"
        );
    }

    #[test]
    fn unary_operators() {
        assert_eq!(
            dump("!!BOUND(?x)"),
            "  Unary
    Bang \"!\"
    Unary
      Bang \"!\"
      Call
        Kw(Bound) \"BOUND\"
        ArgList
          LParen \"(\"
          Arg
            Var1 \"?x\"
          RParen \")\"
"
        );
        assert_eq!(dump("-?x"), "  Unary\n    Minus \"-\"\n    Var1 \"?x\"\n");
    }

    #[test]
    fn in_and_not_in() {
        assert_eq!(
            dump("?x not in (1, 2)"),
            "  InList
    Var1 \"?x\"
    Kw(Not) \"not\"
    Kw(In) \"in\"
    ArgList
      LParen \"(\"
      Arg
        Integer \"1\"
        Comma \",\"
      Arg
        Integer \"2\"
      RParen \")\"
"
        );
        assert_eq!(
            dump("?x IN ()"),
            "  InList\n    Var1 \"?x\"\n    Kw(In) \"IN\"\n    ArgList\n      Nil \"()\"\n"
        );
    }

    #[test]
    fn calls_and_aggregates() {
        assert_eq!(
            dump("ex:f(DISTINCT ?a)"),
            "  Call
    PnameLn \"ex:f\"
    ArgList
      LParen \"(\"
      Kw(Distinct) \"DISTINCT\"
      Arg
        Var1 \"?a\"
      RParen \")\"
"
        );
        assert_eq!(
            dump("count(distinct *)"),
            "  Aggregate
    Kw(Count) \"count\"
    ArgList
      LParen \"(\"
      Kw(Distinct) \"distinct\"
      Arg
        Star \"*\"
      RParen \")\"
"
        );
        assert_eq!(
            dump("group_concat(?n;separator=\", \")"),
            "  Aggregate
    Kw(GroupConcat) \"group_concat\"
    ArgList
      LParen \"(\"
      Arg
        Var1 \"?n\"
        Semicolon \";\"
        Kw(Separator) \"separator\"
        Eq \"=\"
        String2 \"\\\", \\\"\"
      RParen \")\"
"
        );
        assert_eq!(
            dump("now()"),
            "  Call\n    Kw(Now) \"now\"\n    ArgList\n      Nil \"()\"\n"
        );
        assert_eq!(
            dump("<http://e/f>( )"),
            "  Call\n    IriRef \"<http://e/f>\"\n    ArgList\n      Nil \"( )\"\n"
        );
    }

    #[test]
    fn exists_and_literals() {
        assert_eq!(
            dump("not exists { }"),
            "  NotExists
    Kw(Not) \"not\"
    Kw(Exists) \"exists\"
    GroupGraphPattern
      LBrace \"{\"
      RBrace \"}\"
"
        );
        assert_eq!(
            dump("\"a\"@en = \"1\"^^xsd:int"),
            "  Binary
    Literal
      String2 \"\\\"a\\\"\"
      LangDir \"@en\"
    Eq \"=\"
    Literal
      String2 \"\\\"1\\\"\"
      HatHat \"^^\"
      PnameLn \"xsd:int\"
"
        );
    }

    #[test]
    fn triple_terms() {
        assert_eq!(
            dump("<<( ?s a <<( ?a :p \"x\"@en )>> )>>"),
            "  TripleTerm
    LtLtParen \"<<(\"
    Var1 \"?s\"
    Kw(A) \"a\"
    TripleTerm
      LtLtParen \"<<(\"
      Var1 \"?a\"
      PnameLn \":p\"
      Literal
        String2 \"\\\"x\\\"\"
        LangDir \"@en\"
      ParenGtGt \")>>\"
    ParenGtGt \")>>\"
"
        );
        assert_eq!(dump("TRIPLE(?s, ?p, ?o)").lines().next(), Some("  Call"));
    }

    #[test]
    fn constraint_and_bracketted() {
        let t = parse_with("(?a)", constraint);
        assert_eq!(
            t.dump(),
            "QueryUnit\n  Bracketed\n    LParen \"(\"\n    Var1 \"?a\"\n    RParen \")\"\n"
        );
        let t = parse_with("regex(?a, \"x\", \"i\")", constraint);
        let c = t.child_nodes(t.root()).next().unwrap();
        assert_eq!(t.kind(c), NodeKind::Call);
        let t = parse_with("<f>(?a)", constraint);
        let c = t.child_nodes(t.root()).next().unwrap();
        assert_eq!(t.kind(c), NodeKind::Call);
    }

    #[test]
    fn errors() {
        assert!(
            error("?a +").contains("expected an expression"),
            "{}",
            error("?a +")
        );
        assert!(error("?a ||").contains("expected an expression"));
        assert!(error("foo(?a)").contains("expected an expression"));
        assert!(error("STR(?a").contains("expected RParen"));
        assert!(error("?a ?b").contains("expected the end"));
    }

    #[test]
    fn stops_at_as_and_separators() {
        let src = "?a + ex:f(?b, ?c) AS ?x";
        let tokens = lex(src, LexMode::Sparql);
        let mut p = Parser::new(src, &tokens);
        let root = p.start(NodeKind::QueryUnit);
        expression(&mut p);
        assert!(p.at_kw(Kw::As));
        while !p.at(TokenKind::Eof) {
            p.bump();
        }
        root.complete(&mut p);
        let events = p.finish().unwrap();
        let t = build(src, tokens, events);
        let e = t.child_nodes(t.root()).next().unwrap();
        assert_eq!(t.text(e), "?a + ex:f(?b, ?c)");
    }
}
