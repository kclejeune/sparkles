//! The prologue: `BASE`, `PREFIX` and `VERSION` declarations.

use super::Parser;
use crate::lex::TokenKind;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind;

/// `Prologue ::= ( BaseDecl | PrefixDecl | VersionDecl )*`: a `Prologue` node when there
/// is at least one declaration.
pub fn prologue(p: &mut Parser<'_>) {
    if !at_decl(p) {
        return;
    }
    let m = p.start(NodeKind::Prologue);
    while at_decl(p) {
        if p.at_kw(Kw::Base) {
            let d = p.start(NodeKind::BaseDecl);
            p.bump_as(TokenKind::Kw(Kw::Base));
            p.expect(TokenKind::IriRef);
            d.complete(p);
        } else if p.at_kw(Kw::Prefix) {
            let d = p.start(NodeKind::PrefixDecl);
            p.bump_as(TokenKind::Kw(Kw::Prefix));
            p.expect(TokenKind::PnameNs);
            p.expect(TokenKind::IriRef);
            d.complete(p);
        } else {
            let d = p.start(NodeKind::VersionDecl);
            p.bump_as(TokenKind::Kw(Kw::Version));
            if !(p.eat(TokenKind::String1) || p.eat(TokenKind::String2)) {
                p.error("expected a version string");
            }
            d.complete(p);
        }
    }
    m.complete(p);
}

fn at_decl(p: &Parser<'_>) -> bool {
    p.at_kw(Kw::Base) || p.at_kw(Kw::Prefix) || p.at_kw(Kw::Version)
}
