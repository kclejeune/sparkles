//! Update requests: operations separated by `;`, each with its own prologue.

use super::Parser;
use crate::lex::TokenKind;
use crate::syntax::NodeKind;

/// `UpdateUnit ::= Update`, the root of an update request.
///
/// TODO: the grammar. Until it lands every token is a direct child of the unit, which
/// prints verbatim.
pub fn update_unit(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::UpdateUnit);
    while !p.at(TokenKind::Eof) {
        p.bump();
    }
    m.complete(p);
}
