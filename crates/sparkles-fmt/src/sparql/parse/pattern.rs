//! Group graph patterns and their elements: triples blocks, `OPTIONAL`, `MINUS`,
//! `UNION`, `GRAPH`, `SERVICE`, `FILTER`, `BIND`, `VALUES` and subqueries.

use super::Parser;
use crate::lex::TokenKind;
use crate::syntax::NodeKind;

/// `GroupGraphPattern ::= '{' ( SubSelect | GroupGraphPatternSub ) '}'`.
///
/// TODO: the elements (stub: the braces and everything between them, as is).
pub fn group_graph_pattern(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::GroupGraphPattern);
    if p.at(TokenKind::LBrace) {
        p.bump_balanced();
    } else {
        p.error("expected {");
    }
    m.complete(p);
}
