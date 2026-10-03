//! Property paths: alternatives, sequences, inverses, negated sets, modifiers.
//!
//! A node only where the path has structure: a plain IRI or `a` stays a token, `p*` is a
//! `PathElt`, `^p` a `PathInverse`, `a/b` a `PathSequence`, `a|b` a `PathAlternative`,
//! `!p` and `!(…)` a `PathNegated`, `(…)` a `PathBracketed`.

use super::Parser;
use super::term;
use crate::lex::TokenKind;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind;

/// `Path ::= PathAlternative`. Returns whether the path is a plain predicate: an IRI or
/// `a`, possibly inverted or bracketed (one that reifiers and annotations may follow).
pub fn path(p: &mut Parser<'_>) -> bool {
    path_alternative(p)
}

/// `PathAlternative ::= PathSequence ( '|' PathSequence )*`
fn path_alternative(p: &mut Parser<'_>) -> bool {
    let m = p.start(NodeKind::PathAlternative);
    let plain = path_sequence(p);
    if !p.at(TokenKind::Pipe) {
        m.abandon(p);
        return plain;
    }
    while p.eat(TokenKind::Pipe) {
        path_sequence(p);
    }
    m.complete(p);
    false
}

/// `PathSequence ::= PathEltOrInverse ( '/' PathEltOrInverse )*`
fn path_sequence(p: &mut Parser<'_>) -> bool {
    let m = p.start(NodeKind::PathSequence);
    let plain = path_elt_or_inverse(p);
    if !p.at(TokenKind::Slash) {
        m.abandon(p);
        return plain;
    }
    while p.eat(TokenKind::Slash) {
        path_elt_or_inverse(p);
    }
    m.complete(p);
    false
}

/// `PathEltOrInverse ::= PathElt | '^' PathElt`
fn path_elt_or_inverse(p: &mut Parser<'_>) -> bool {
    if p.at(TokenKind::Hat) {
        let m = p.start(NodeKind::PathInverse);
        p.bump();
        let plain = path_elt(p);
        m.complete(p);
        plain
    } else {
        path_elt(p)
    }
}

/// `PathElt ::= PathPrimary PathMod?`, with Jena ARQ's ranges as `PathMod`s.
fn path_elt(p: &mut Parser<'_>) -> bool {
    let m = p.start(NodeKind::PathElt);
    let plain = path_primary(p);
    if matches!(
        p.current(),
        TokenKind::Question | TokenKind::Star | TokenKind::Plus
    ) {
        p.bump();
        m.complete(p);
        false
    } else if p.at(TokenKind::LBrace) {
        path_range(p);
        m.complete(p);
        false
    } else {
        m.abandon(p);
        plain
    }
}

/// ARQ's `PathMod` braces: `{*}`, `{+}`, `{n}`, `{n,m}`, `{n,}` and `{,m}`.
fn path_range(p: &mut Parser<'_>) {
    p.expect(TokenKind::LBrace);
    if !p.eat(TokenKind::Star) && !p.eat(TokenKind::Plus) {
        let min = p.eat(TokenKind::Integer);
        if p.eat(TokenKind::Comma) {
            if !p.eat(TokenKind::Integer) && !min {
                p.error("expected the most steps of a path range");
            }
        } else if !min {
            p.error("expected a path range");
        }
    }
    p.expect(TokenKind::RBrace);
}

/// `PathPrimary ::= iri | 'a' | '!' PathNegatedPropertySet | '(' Path ')'`
fn path_primary(p: &mut Parser<'_>) -> bool {
    match p.current() {
        TokenKind::Bang => {
            let m = p.start(NodeKind::PathNegated);
            p.bump();
            if p.eat(TokenKind::LParen) {
                loop {
                    path_one_in_property_set(p);
                    if !p.eat(TokenKind::Pipe) {
                        break;
                    }
                }
                p.expect(TokenKind::RParen);
            } else {
                path_one_in_property_set(p);
            }
            m.complete(p);
            false
        }
        TokenKind::LParen => {
            let m = p.start(NodeKind::PathBracketed);
            p.bump();
            let plain = path(p);
            p.expect(TokenKind::RParen);
            m.complete(p);
            plain
        }
        _ => p.eat_kw(Kw::A) || term::iri(p),
    }
}

/// `PathOneInPropertySet ::= iri | 'a' | '^' ( iri | 'a' )`
fn path_one_in_property_set(p: &mut Parser<'_>) {
    if p.at(TokenKind::Hat) {
        let m = p.start(NodeKind::PathInverse);
        p.bump();
        if !p.eat_kw(Kw::A) {
            term::iri(p);
        }
        m.complete(p);
    } else if !p.eat_kw(Kw::A) {
        term::iri(p);
    }
}
