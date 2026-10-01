//! RDF terms: variables, IRIs, prefixed names, literals (with a language tag or a
//! datatype), blank nodes, `NIL`.

use super::Parser;
use crate::lex::TokenKind;
use crate::syntax::NodeKind;

/// `VarOrTerm`: one token, or a `Literal` node for a string with a language tag or a
/// `^^` datatype.
pub fn var_or_term(p: &mut Parser<'_>) {
    if p.current().is_string() && matches!(p.nth(1), TokenKind::LangDir | TokenKind::HatHat) {
        let m = p.start(NodeKind::Literal);
        p.bump();
        if p.eat(TokenKind::HatHat) {
            if !(p.eat(TokenKind::IriRef) || p.eat(TokenKind::PnameLn) || p.eat(TokenKind::PnameNs))
            {
                p.error("expected a datatype IRI");
            }
        } else {
            p.bump();
        }
        m.complete(p);
    } else {
        p.bump();
    }
}
