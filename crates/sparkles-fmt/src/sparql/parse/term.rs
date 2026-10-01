//! RDF terms: variables, IRIs, prefixed names, literals (with a language tag or a
//! datatype), blank nodes, `NIL`, and the RDF 1.2 triple terms `<<( s p o )>>`.
//!
//! Terms are single tokens, except a string with a language tag or a `^^` datatype (a
//! [`NodeKind::Literal`]) and a triple term (a [`NodeKind::TripleTerm`]). The escapes of
//! IRIs and strings are checked here, as the reference parser checks them when it
//! decodes the token: a code point that is not a character (a surrogate) and an unknown
//! string escape are errors.

use super::Parser;
use crate::lex::TokenKind;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind;

/// `Var`
pub fn at_var(p: &Parser<'_>) -> bool {
    matches!(p.current(), TokenKind::Var1 | TokenKind::Var2)
}

/// `iri ::= IRIREF | PrefixedName`
pub fn at_iri(p: &Parser<'_>) -> bool {
    is_iri(p.current())
}

/// Whether a token is an IRI or a prefixed name.
pub fn is_iri(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::IriRef | TokenKind::PnameLn | TokenKind::PnameNs
    )
}

/// `BlankNode ::= BLANK_NODE_LABEL | ANON`
pub fn at_blank_node(p: &Parser<'_>) -> bool {
    matches!(p.current(), TokenKind::BlankNodeLabel | TokenKind::Anon)
}

/// `RDFLiteral | NumericLiteral | BooleanLiteral`
pub fn at_literal(p: &Parser<'_>) -> bool {
    p.current().is_string() || p.current().is_number() || at_boolean(p)
}

fn at_boolean(p: &Parser<'_>) -> bool {
    p.at_kw(Kw::True) || p.at_kw(Kw::False)
}

/// `VarOrIri`
pub fn at_var_or_iri(p: &Parser<'_>) -> bool {
    at_var(p) || at_iri(p)
}

/// Consume a variable, or fail.
pub fn var(p: &mut Parser<'_>) -> bool {
    if at_var(p) {
        p.bump();
        true
    } else {
        p.error("expected a variable");
        false
    }
}

/// Consume an IRI or a prefixed name, or fail.
pub fn iri(p: &mut Parser<'_>) -> bool {
    if at_iri(p) {
        checked_bump(p);
        !p.has_error()
    } else {
        p.error("expected an IRI");
        false
    }
}

/// `VarOrIri`, or fail.
pub fn var_or_iri(p: &mut Parser<'_>) -> bool {
    if at_var(p) {
        p.bump();
        true
    } else {
        iri(p)
    }
}

/// `Verb ::= VarOrIri | 'a'`, or fail.
pub fn verb(p: &mut Parser<'_>) -> bool {
    p.eat_kw(Kw::A) || var_or_iri(p)
}

/// `VarOrTerm`: a variable, an IRI, a literal, a blank node, `NIL` or a triple term, or
/// fail.
pub fn var_or_term(p: &mut Parser<'_>) -> bool {
    if p.at(TokenKind::LtLtParen) {
        triple_term(p, TripleTermCtx::Pattern);
        return !p.has_error();
    }
    if at_var(p) || at_blank_node(p) || p.at(TokenKind::Nil) {
        p.bump();
        return true;
    }
    if at_iri(p) || at_literal(p) {
        return iri_or_literal(p);
    }
    p.error("expected an RDF term");
    false
}

/// An IRI or a literal (`RDFLiteral | NumericLiteral | BooleanLiteral`), or fail.
pub fn iri_or_literal(p: &mut Parser<'_>) -> bool {
    if at_iri(p) {
        return iri(p);
    }
    literal(p)
}

/// `RDFLiteral | NumericLiteral | BooleanLiteral`, or fail: one token, or a `Literal`
/// node for a string with a language tag or a `^^` datatype.
pub fn literal(p: &mut Parser<'_>) -> bool {
    if p.current().is_number() {
        p.bump();
        return true;
    }
    if p.eat_kw(Kw::True) || p.eat_kw(Kw::False) {
        return true;
    }
    if !p.current().is_string() {
        p.error("expected a literal");
        return false;
    }
    match p.nth(1) {
        TokenKind::LangDir => {
            let m = p.start(NodeKind::Literal);
            checked_bump(p);
            if !valid_direction(p.nth_text(0)) {
                p.error("the only base directions are ltr and rtl");
            }
            p.bump();
            m.complete(p);
        }
        TokenKind::HatHat => {
            let m = p.start(NodeKind::Literal);
            checked_bump(p);
            p.bump();
            iri(p);
            m.complete(p);
        }
        _ => checked_bump(p),
    }
    !p.has_error()
}

/// Where a triple term is: its subject and object are restricted differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TripleTermCtx {
    /// in a graph pattern or a template: any term but a collection or a blank node
    /// property list
    Pattern,
    /// in `VALUES` data: an IRI subject, no variables, no blank nodes
    Data,
    /// in an expression: an IRI or variable subject, no blank nodes
    Expr,
}

/// `TripleTerm ::= '<<(' TripleTermSubject Verb TripleTermObject ')>>'` (and its data and
/// expression variants): a `TripleTerm` node.
pub fn triple_term(p: &mut Parser<'_>, cx: TripleTermCtx) {
    let m = p.start(NodeKind::TripleTerm);
    p.expect(TokenKind::LtLtParen);
    // the subject
    match cx {
        TripleTermCtx::Pattern => {
            triple_term_object(p, cx);
        }
        TripleTermCtx::Data => {
            iri(p);
        }
        TripleTermCtx::Expr => {
            var_or_iri(p);
        }
    }
    // the predicate
    match cx {
        TripleTermCtx::Data => {
            if !p.eat_kw(Kw::A) {
                iri(p);
            }
        }
        _ => {
            verb(p);
        }
    }
    triple_term_object(p, cx);
    p.expect(TokenKind::ParenGtGt);
    m.complete(p);
}

/// `TripleTermObject` (and `TripleTermDataObject`, `ExprTripleTermObject`).
fn triple_term_object(p: &mut Parser<'_>, cx: TripleTermCtx) {
    if p.at(TokenKind::LtLtParen) {
        triple_term(p, cx);
    } else if (at_var(p) && cx != TripleTermCtx::Data)
        || (at_blank_node(p) && cx == TripleTermCtx::Pattern)
    {
        p.bump();
    } else if at_iri(p) || at_literal(p) {
        iri_or_literal(p);
    } else {
        p.error("expected the object of a triple term");
    }
}

/// Bump an IRI, a prefixed name or a string after checking its escapes.
fn checked_bump(p: &mut Parser<'_>) {
    let kind = p.current();
    let text = p.nth_text(0);
    let ok = match kind {
        TokenKind::IriRef => escapes_ok(text, false),
        k if k.is_string() => escapes_ok(text, true),
        _ => true,
    };
    if ok {
        p.bump();
    } else {
        p.error("invalid escape sequence");
    }
}

/// Whether every `\` escape of an IRI or string token is valid: `\u`/`\U` with the hex
/// digits of a character (no surrogate), and in strings also `\t \b \n \r \f \" \' \\`.
pub fn escapes_ok(text: &str, string: bool) -> bool {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        let n = match b.get(i + 1) {
            Some(b'u') => 4,
            Some(b'U') => 8,
            Some(b't' | b'b' | b'n' | b'r' | b'f' | b'"' | b'\'' | b'\\') if string => {
                i += 2;
                continue;
            }
            _ => return false,
        };
        let Some(hex) = text.get(i + 2..i + 2 + n) else {
            return false;
        };
        let valid = hex.bytes().all(|c| c.is_ascii_hexdigit())
            && u32::from_str_radix(hex, 16)
                .ok()
                .and_then(char::from_u32)
                .is_some();
        if !valid {
            return false;
        }
        i += 2 + n;
    }
    true
}

/// A `LANG_DIR` token's direction, if any, is `ltr` or `rtl`.
fn valid_direction(langdir: &str) -> bool {
    match langdir.split_once("--") {
        None => true,
        Some((_, dir)) => dir == "ltr" || dir == "rtl",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert!(escapes_ok("\"a\\tb\\u00e9\\U0001F47E\"", true));
        assert!(escapes_ok("<http://e/\\u0061>", false));
        assert!(!escapes_ok("<http://e/\\t>", false));
        assert!(!escapes_ok("\"\\q\"", true));
        assert!(!escapes_ok("\"\\uD83C\\uDCA1\"", true));
        assert!(!escapes_ok("\"\\U00110000\"", true));
        assert!(!escapes_ok("\"\\u12\"", true));
    }

    #[test]
    fn directions() {
        assert!(valid_direction("@en"));
        assert!(valid_direction("@ar--rtl"));
        assert!(!valid_direction("@ar--up"));
    }
}
