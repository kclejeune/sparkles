//! `prune-prefixes`: the prefix declarations nothing uses. "Unused" is computed per
//! declaration scope: a declaration is used when a prefixed name with its label appears
//! after it and before a later declaration re-maps the label (Turtle, TriG), or anywhere
//! after it in the request (SPARQL updates: each operation's prologue stays in force for
//! the later operations). The same tree shapes serve SPARQL and Turtle: `PrefixDecl`
//! nodes and `PnameNs`/`PnameLn` tokens.
//!
//! A use is counted in the printed output, not in the input, so pruning is idempotent: a
//! full IRI the printer compacts uses the declaration it compacts with, and an
//! `rdf:type` verb printed `a` or a datatype dropped by a numeric or boolean
//! shorthand uses nothing. Nodes printed as written (an ignore pragma, `Opaque`)
//! keep every prefixed name and compact nothing.

use super::PrefixScope;
use crate::Options;
use crate::lex::TokenKind;
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeId, TokenId, Tree};
use std::collections::{HashMap, HashSet};

/// Whether `prune-prefixes` acts.
pub const IMPLEMENTED: bool = true;

/// The `PrefixDecl` nodes of `tree` that no prefixed name uses in their scope, as the
/// default options print the tree and without ignore pragmas
/// ([`unused_declarations_with`]).
pub fn unused_declarations(tree: &Tree<'_>, scope: &PrefixScope) -> HashSet<NodeId> {
    unused_declarations_with(tree, scope, &Options::default(), |_| false)
}

/// The `PrefixDecl` nodes of `tree` that no prefixed name of the output uses in their
/// scope, when the printer applies `compact-iris` and `type-shorthand` as `opts` say and
/// prints the nodes for which `as_written` holds (and their subtrees) verbatim. A
/// printer drops only those of them that carry no comments.
pub fn unused_declarations_with(
    tree: &Tree<'_>,
    scope: &PrefixScope,
    opts: &Options,
    as_written: impl Fn(NodeId) -> bool,
) -> HashSet<NodeId> {
    let mut w = Walk {
        tree,
        scope,
        opts,
        as_written: &as_written,
        decl_at: HashMap::new(),
        uses: Vec::new(),
    };
    w.node(tree.root(), false);
    // a forward pass: the declaration each label stands for at each use
    let mut uses = std::mem::take(&mut w.uses);
    uses.sort_unstable_by_key(|u| u.1);
    let mut decl_at: Vec<(TokenId, &str, NodeId)> = w
        .decl_at
        .iter()
        .map(|(&t, &(label, n))| (t, label, n))
        .collect();
    decl_at.sort_unstable_by_key(|d| d.0);
    let mut current: HashMap<&str, NodeId> = HashMap::new();
    let mut used = HashSet::new();
    let mut next_decl = decl_at.iter().peekable();
    for (label, at) in uses {
        while let Some(&&(t, l, n)) = next_decl.peek()
            && t < at
        {
            current.insert(l, n);
            next_decl.next();
        }
        if let Some(&n) = current.get(label.as_str()) {
            used.insert(n);
        }
    }
    decl_at
        .iter()
        .map(|d| d.2)
        .filter(|n| !used.contains(n))
        .collect()
}

struct Walk<'a, 's, F: Fn(NodeId) -> bool> {
    tree: &'a Tree<'s>,
    scope: &'a PrefixScope,
    opts: &'a Options,
    as_written: &'a F,
    /// each declaration by the token it is in scope after (its IRI): label and node
    decl_at: HashMap<TokenId, (&'s str, NodeId)>,
    /// each use: the label and the token
    uses: Vec<(String, TokenId)>,
}

impl<'s, F: Fn(NodeId) -> bool> Walk<'_, 's, F> {
    fn node(&mut self, n: NodeId, verbatim: bool) {
        let tree = self.tree;
        let verbatim = verbatim || tree.kind(n) == NodeKind::Opaque || (self.as_written)(n);
        match tree.kind(n) {
            NodeKind::PrefixDecl => {
                let mut label = None;
                for e in tree.children(n) {
                    if let Element::Token(t) = *e {
                        match tree.token_kind(t) {
                            TokenKind::PnameNs => label = tree.token_text(t).strip_suffix(':'),
                            TokenKind::IriRef => {
                                if let Some(l) = label {
                                    self.decl_at.insert(t, (l, n));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                return;
            }
            // never compacted
            NodeKind::BaseDecl => return,
            _ => {}
        }
        let children = tree.children(n);
        let dropped = match verbatim {
            true => None,
            false => self.dropped(n, children),
        };
        for &e in children {
            match e {
                Element::Node(c) => self.node(c, verbatim),
                Element::Token(t) if Some(t) == dropped => {}
                Element::Token(t) => self.token(t, verbatim),
            }
        }
    }

    /// The token of node `n` the printer replaces by something without a prefix: an
    /// `rdf:type` verb (`a`), a datatype after a numeric or boolean shorthand.
    fn dropped(&self, n: NodeId, children: &[Element]) -> Option<TokenId> {
        let tree = self.tree;
        match tree.kind(n) {
            NodeKind::PropertyListEntry if self.opts.type_shorthand => match children.first() {
                Some(&Element::Token(t)) if super::is_rdf_type(tree, t, self.scope) => Some(t),
                _ => None,
            },
            NodeKind::Literal => {
                let tokens: Vec<TokenId> = children
                    .iter()
                    .filter_map(|e| match *e {
                        Element::Token(t) => Some(t),
                        Element::Node(_) => None,
                    })
                    .collect();
                let &[string, hathat, datatype] = tokens.as_slice() else {
                    return None;
                };
                if tree.token_kind(hathat) != TokenKind::HatHat {
                    return None;
                }
                let dt_text = tree.token_text(datatype);
                let dt = match tree.token_kind(datatype) {
                    TokenKind::IriRef => dt_text.to_string(),
                    TokenKind::PnameLn => {
                        let (label, local) = dt_text.split_once(':')?;
                        format!("{}{local}", self.scope.resolve(label, datatype)?)
                    }
                    _ => return None,
                };
                super::literal_shorthand(tree.token_text(string), &dt).map(|_| datatype)
            }
            _ => None,
        }
    }

    fn token(&mut self, t: TokenId, verbatim: bool) {
        let tree = self.tree;
        let text = tree.token_text(t);
        let label = match tree.token_kind(t) {
            TokenKind::PnameNs | TokenKind::PnameLn => text.split_once(':').map(|p| p.0.into()),
            TokenKind::IriRef if !verbatim && self.opts.compact_iris => {
                super::compact_iri(text, self.scope, t)
                    .and_then(|p| p.split_once(':').map(|p| p.0.to_string()))
            }
            _ => None,
        };
        if let Some(label) = label {
            self.uses.push((label, t));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{LexMode, lex};
    use crate::sparql::Unit;

    /// The labels of the unused declarations of `src` (a query, or an update when it
    /// does not parse as one), in source order.
    fn unused_with(src: &str, opts: &Options) -> Vec<String> {
        let parse = |unit| crate::sparql::parse::parse(src, lex(src, LexMode::Sparql), unit);
        let tree = parse(Unit::Query).or_else(|_| parse(Unit::Update)).unwrap();
        let scope = PrefixScope::from_tree(&tree);
        let mut nodes: Vec<NodeId> = unused_declarations_with(&tree, &scope, opts, |_| false)
            .into_iter()
            .collect();
        nodes.sort();
        nodes
            .into_iter()
            .map(|n| {
                let text = tree.text(n);
                text.split_whitespace().nth(1).unwrap_or("").to_string()
            })
            .collect()
    }

    fn unused(src: &str) -> Vec<String> {
        unused_with(src, &Options::default())
    }

    #[test]
    fn prefixed_names_use_their_declaration() {
        assert_eq!(
            unused(
                "PREFIX a: <http://e/a#> PREFIX b: <http://e/b#> PREFIX c: <http://e/c#> PREFIX : <http://e/>\nSELECT * { ?s a:p b: . ?s ?p : }"
            ),
            ["c:"]
        );
        assert_eq!(unused("PREFIX a: <http://e/a#> ASK {}"), ["a:"]);
        // a name before its declaration does not count (it is undeclared there)
        assert_eq!(
            unused("INSERT DATA { a:s a:p 1 } ; PREFIX a: <http://e/a#> CLEAR ALL"),
            ["a:"]
        );
    }

    #[test]
    fn a_redefinition_ends_a_scope() {
        assert_eq!(
            unused("PREFIX a: <http://e/1#> PREFIX a: <http://e/2#> SELECT * { ?s a:p ?o }"),
            ["a:"]
        );
        // an update: a prologue stays in force for the later operations, until re-mapped
        let src = "PREFIX a: <http://e/1#> PREFIX b: <http://e/b#> INSERT DATA { a:s a:p 1 } ;\nPREFIX a: <http://e/2#> INSERT DATA { b:s a:p 2 }";
        assert_eq!(unused(src), Vec::<String>::new());
        let src = "PREFIX a: <http://e/1#> CLEAR ALL ;\nPREFIX a: <http://e/2#> INSERT DATA { a:s a:p 2 }";
        assert_eq!(unused(src), ["a:"]);
    }

    #[test]
    fn uses_are_counted_in_the_output() {
        // a full IRI the printer compacts uses the longest namespace, the earliest on a tie
        let src = "PREFIX a: <http://e/> PREFIX b: <http://e/x/> PREFIX c: <http://e/x/> SELECT * { ?s ?p <http://e/x/y> }";
        assert_eq!(unused(src), ["a:", "c:"]);
        let no_compact = Options {
            compact_iris: false,
            ..Options::default()
        };
        assert_eq!(unused_with(src, &no_compact), ["a:", "b:", "c:"]);
        // `rdf:type` printed `a`, a datatype dropped by the shorthand
        let src = "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\nSELECT * { ?s rdf:type ?t ; ?p \"1\"^^xsd:integer }";
        assert_eq!(unused(src), ["rdf:", "xsd:"]);
        let no_shorthand = Options {
            type_shorthand: false,
            ..Options::default()
        };
        assert_eq!(unused_with(src, &no_shorthand), ["xsd:"]);
        // used elsewhere: kept
        let src = "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\nSELECT * { ?s rdf:type ?t ; ?p \"1.\"^^xsd:decimal . ?t rdf:value ?v }";
        assert_eq!(unused(src), Vec::<String>::new());
        // `rdf:type` in a path or an object stays
        let src = "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> SELECT * { ?s rdf:type/rdf:rest ?t }";
        assert_eq!(unused(src), Vec::<String>::new());
    }

    #[test]
    fn nodes_printed_as_written_keep_their_names() {
        let src = "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> PREFIX ex: <http://e/>\nSELECT * { ?s rdf:type <http://e/x> }";
        let tree =
            crate::sparql::parse::parse(src, lex(src, LexMode::Sparql), Unit::Query).unwrap();
        let scope = PrefixScope::from_tree(&tree);
        let opts = Options::default();
        let n = unused_declarations_with(&tree, &scope, &opts, |_| false);
        assert_eq!(n.len(), 1, "rdf: is printed `a`; ex: compacts <http://e/x>");
        let all = unused_declarations_with(&tree, &scope, &opts, |n| {
            tree.kind(n) == NodeKind::TriplesStmt
        });
        assert_eq!(all.len(), 1, "rdf:type stays, the IRI is not compacted");
        assert_ne!(n, all);
    }
}
