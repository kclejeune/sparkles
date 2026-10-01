//! Triples: subjects with property lists, object lists, blank node property lists,
//! collections, and the RDF 1.2 reified triples, triple terms, reifiers and annotations.

use super::Parser;
use crate::lex::TokenKind;
use crate::syntax::NodeKind;

/// `TriplesSameSubject(Path)` with its `.`: one `TriplesStmt`.
///
/// TODO: the grammar (stub: the tokens up to the `.` or the group's end, as is).
pub fn triples_stmt(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::TriplesStmt);
    while !matches!(
        p.current(),
        TokenKind::Eof | TokenKind::RBrace | TokenKind::Dot
    ) {
        p.bump_balanced();
    }
    p.eat(TokenKind::Dot);
    m.complete(p);
}
