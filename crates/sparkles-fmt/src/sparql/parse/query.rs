//! Queries: `SELECT`, `CONSTRUCT`, `DESCRIBE` and `ASK` forms, their clauses and
//! solution modifiers, subqueries and the trailing `VALUES`.

use super::Parser;
use crate::lex::TokenKind;
use crate::syntax::NodeKind;

/// `QueryUnit ::= Query`, the root of a query.
///
/// TODO: the grammar. Until it lands every token is a direct child of the unit, which
/// prints verbatim.
pub fn query_unit(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::QueryUnit);
    while !p.at(TokenKind::Eof) {
        p.bump();
    }
    m.complete(p);
}

/// `SubSelect`: `SELECT …` inside a group's braces.
///
/// TODO: the grammar (stub: the tokens up to the group's closing brace, as is).
pub fn sub_select(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::SubSelect);
    while !p.at(TokenKind::Eof) && !p.at(TokenKind::RBrace) {
        p.bump_balanced();
    }
    m.complete(p);
}
