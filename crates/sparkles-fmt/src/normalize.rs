//! The normalizations: prefix scopes, prefix sorting and grouping (N5), IRI compaction
//! (N7), `rdf:type` → `a` (N8), numeric and boolean shorthand (N9) and quote style
//! (N10). Pure functions over the tree; printers pass what they return to
//! [`crate::doc::DocArena::token`].

use crate::lex::TokenKind;
use crate::tree::{NodeId, TokenId, Tree};

/// One prefix declaration as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declared {
    /// the label without its `:` (`""` for the empty prefix)
    pub label: String,
    /// the namespace IRI without its `<` `>` (escapes as written)
    pub iri: String,
    /// the declaration's token: it is in scope after it
    pub at: TokenId,
}

/// The prefix declarations of a document, to know which are in scope at a token.
#[derive(Clone, Debug, Default)]
pub struct PrefixScope {
    declared: Vec<Declared>,
}

impl PrefixScope {
    /// TODO: collect the `PrefixDecl` nodes (stub: no prefixes, so nothing compacts).
    pub fn from_tree(tree: &Tree<'_>) -> PrefixScope {
        let _ = tree;
        PrefixScope::default()
    }

    /// The declarations in scope at `token`, a later one of a label shadowing an earlier
    /// one.
    pub fn at(&self, token: TokenId) -> Vec<&Declared> {
        let mut seen = std::collections::HashSet::new();
        let mut v: Vec<&Declared> = self
            .declared
            .iter()
            .rev()
            .filter(|d| d.at < token && seen.insert(d.label.as_str()))
            .collect();
        v.reverse();
        v
    }
}

/// A prefix declaration of a prologue, for [`plan_runs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefixDecl {
    pub node: NodeId,
    pub label: String,
    pub iri: String,
    /// it has comments of its own (it is never dropped as a duplicate)
    pub has_comments: bool,
    /// a run ends before it: a `BASE`, a detached comment block, or (without prefix
    /// groups) a blank line
    pub barrier_before: bool,
}

/// How to print one run of declarations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    /// indexes into the declarations, in printing order
    pub order: Vec<usize>,
    /// indexes of exact, comment-free duplicates that are not printed
    pub drop: Vec<usize>,
    /// positions in `order` before which a blank line separates two groups
    pub group_breaks: Vec<usize>,
}

/// Sort and group the prefix declarations within their runs (N5 and `prefix-groups`).
///
/// TODO: sorting, deduplication and grouping (stub: each run in source order).
pub fn plan_runs(decls: &[PrefixDecl], groups: &[Vec<String>]) -> Vec<Run> {
    let _ = groups;
    let mut runs: Vec<Run> = Vec::new();
    for (i, d) in decls.iter().enumerate() {
        if d.barrier_before || runs.is_empty() {
            runs.push(Run::default());
        }
        runs.last_mut().expect("a run").order.push(i);
    }
    runs
}

/// The prefixed name for a full IRI token (`<…>` as written) when a prefix in scope at
/// `at` covers it (N7).
///
/// TODO: N7 (stub: never).
pub fn compact_iri(iri_token: &str, scope: &PrefixScope, at: TokenId) -> Option<String> {
    let _ = (iri_token, scope, at);
    None
}

/// Whether a verb token denotes `rdf:type` (an `IRIREF` or a prefixed name), for N8.
///
/// TODO: N8 (stub: never).
pub fn is_rdf_type(tree: &Tree<'_>, token: TokenId, scope: &PrefixScope) -> bool {
    let _ = (tree, token, scope);
    false
}

/// The numeric or boolean token a typed literal can be written as: `lexical` is the
/// string token as written (quotes included), `datatype` the datatype's full IRI (N9).
///
/// TODO: N9 (stub: never).
pub fn literal_shorthand<'a>(lexical: &'a str, datatype: &str) -> Option<&'a str> {
    let _ = (lexical, datatype);
    None
}

/// A string token rewritten with double quotes, if it can be (N10).
///
/// TODO: N10 (stub: never).
pub fn requote(text: &str, kind: TokenKind) -> Option<String> {
    let _ = (text, kind);
    None
}
