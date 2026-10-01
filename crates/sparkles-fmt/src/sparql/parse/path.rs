//! Property paths: alternatives, sequences, inverses, negated sets, modifiers.

use super::Parser;
use crate::syntax::NodeKind;

/// `Path ::= PathAlternative`.
///
/// TODO: the grammar (stub: one token or bracketed group, as is).
pub fn path(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::PathElt);
    p.bump_balanced();
    m.complete(p);
}
