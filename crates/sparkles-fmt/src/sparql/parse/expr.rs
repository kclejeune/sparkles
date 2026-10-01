//! Expressions: a Pratt loop over the SPARQL precedence levels (`||`, `&&`, relational,
//! additive, multiplicative, unary), built-in calls, aggregates, function calls, `IN`,
//! `EXISTS`, and RDF 1.2 triple terms in expressions.
//!
//! The entry points the pattern and query parsers call: [`expression`], [`bracketted`],
//! [`builtin_or_call`], [`constraint`].

use super::{Completed, Parser};
use crate::lex::TokenKind;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind;

/// `Expression`.
///
/// TODO: the grammar (stub: an `Opaque` node over the balanced tokens up to `AS`, `,`,
/// `;` or a closing bracket at depth 0).
pub fn expression(p: &mut Parser<'_>) -> Option<Completed> {
    let m = p.start(NodeKind::Opaque);
    loop {
        let k = p.current();
        if k == TokenKind::Eof
            || matches!(k, TokenKind::Comma | TokenKind::Semicolon)
            || super::is_closer(k)
            || p.at_kw(Kw::As)
        {
            break;
        }
        p.bump_balanced();
    }
    Some(m.complete(p))
}

/// `BrackettedExpression ::= '(' Expression ')'`.
///
/// TODO: the grammar (stub: an `Opaque` node over the brackets).
pub fn bracketted(p: &mut Parser<'_>) -> Option<Completed> {
    if !p.at(TokenKind::LParen) {
        p.error("expected (");
        return None;
    }
    let m = p.start(NodeKind::Opaque);
    p.bump_balanced();
    Some(m.complete(p))
}

/// `BuiltInCall | FunctionCall`: a name and its arguments.
///
/// TODO: the grammar (stub: an `Opaque` node over the name and the bracketed arguments).
pub fn builtin_or_call(p: &mut Parser<'_>) -> Option<Completed> {
    let m = p.start(NodeKind::Opaque);
    p.bump();
    if p.at(TokenKind::LParen) || p.at(TokenKind::Nil) {
        p.bump_balanced();
    }
    Some(m.complete(p))
}

/// `Constraint ::= BrackettedExpression | BuiltInCall | FunctionCall`.
pub fn constraint(p: &mut Parser<'_>) -> Option<Completed> {
    if p.at(TokenKind::LParen) {
        bracketted(p)
    } else {
        builtin_or_call(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{LexMode, lex};
    use crate::sparql::parse::build;

    #[test]
    fn stub_scans_to_depth_zero_stops() {
        let src = "?a + f(?b, ?c) AS ?x";
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
        assert_eq!(t.text(e), "?a + f(?b, ?c)");
    }
}
