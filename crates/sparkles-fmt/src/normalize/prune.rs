//! `prune-prefixes`: the prefix declarations nothing uses. "Unused" is computed per
//! declaration scope: a declaration is used when a prefixed name with its label appears
//! after it and before a later declaration re-maps the label (Turtle, TriG), or anywhere
//! after it in the request (SPARQL updates: each operation's prologue stays in force for
//! the later operations). The same tree shapes serve SPARQL and Turtle: `PrefixDecl`
//! nodes and `PnameNs`/`PnameLn` tokens.
//!
//! Not written yet: `prune-prefixes = true` warns `option-not-implemented`.

use super::PrefixScope;
use crate::tree::{NodeId, Tree};
use std::collections::HashSet;

/// Whether `prune-prefixes` acts.
pub const IMPLEMENTED: bool = false;

/// The `PrefixDecl` nodes of `tree` that no prefixed name uses in their scope.
pub fn unused_declarations(tree: &Tree<'_>, scope: &PrefixScope) -> HashSet<NodeId> {
    let _ = (tree, scope);
    HashSet::new()
}
