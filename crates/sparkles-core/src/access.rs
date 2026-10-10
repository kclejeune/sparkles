//! Graph-level access: which graphs of a dataset a request may read and write.
//!
//! A [`GraphAccess`] is the view of one request. Queries see only the graphs of
//! [`GraphAccess::read`] (the planner intersects the query's RDF dataset with them),
//! and a write transaction refuses to insert or delete a quad outside
//! [`GraphAccess::write`]. Rules match graph names, never data, so every decision is
//! the same whether or not a hidden graph exists or holds a quad.
//!
//! A view may also carry [`TripleRules`]: protections that hide some triples of the
//! graphs it reads (see [`triples`]). Reads then see a masked snapshot, which holds the
//! hidden quads as extra deletions, so every scan, count and index of the engine sees
//! only the visible triples.

pub mod triples;
pub use triples::{Caller, Limits, Mask, Protection, Rule, TripleRules};

use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::sparql::ctx::{DEFAULT_GRAPH_IRI, UNION_GRAPH_IRI};
use crate::store::Snapshot;
use oxrdf::Term;
use std::sync::Arc;

/// `*` matches any run (possibly empty) of characters; everything else matches itself.
pub fn glob(pattern: &str, name: &str) -> bool {
    let (p, n) = (pattern.as_bytes(), name.as_bytes());
    // iterative wildcard matching with backtracking to the last `*`
    let (mut i, mut j) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while j < n.len() {
        if i < p.len() && p[i] == b'*' {
            star = Some((i, j));
            i += 1;
        } else if i < p.len() && p[i] == n[j] {
            i += 1;
            j += 1;
        } else if let Some((si, sj)) = star {
            i = si + 1;
            j = sj + 1;
            star = Some((si, sj + 1));
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == b'*')
}

/// A set of graphs given by name: the default graph, exact graph IRIs and IRI patterns
/// with `*`. Blank-node graph names are never in it, and the `protected` names are in
/// it only when listed exactly.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct GraphRule {
    pub default_graph: bool,
    /// graph IRIs (sorted)
    pub iris: Vec<String>,
    /// IRI patterns with `*` (sorted)
    pub patterns: Vec<String>,
    /// IRIs that patterns never match (the graph of materialized inferences)
    pub protected: Vec<String>,
    /// blank-node graph names are in the set (only in a rule made by
    /// [`Graphs::without_iri`] from every graph)
    pub blank_nodes: bool,
    /// IRI patterns with `*` that are never in the set, whatever else names them
    /// (made by [`Graphs::without_pattern`])
    pub excluded: Vec<String>,
}

impl GraphRule {
    /// A rule from names: `urn:x-arq:DefaultGraph` (or `default`) for the default graph,
    /// IRIs, and IRIs with `*`.
    pub fn new(names: impl IntoIterator<Item = impl AsRef<str>>, protected: &[&str]) -> GraphRule {
        let mut r = GraphRule {
            protected: protected.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        for n in names {
            let n = n.as_ref();
            if n == DEFAULT_GRAPH_IRI || n == "default" {
                r.default_graph = true;
            } else if n.contains('*') {
                r.patterns.push(n.to_string());
            } else {
                r.iris.push(n.to_string());
            }
        }
        r.normalize();
        r
    }

    fn normalize(&mut self) {
        for v in [
            &mut self.iris,
            &mut self.patterns,
            &mut self.protected,
            &mut self.excluded,
        ] {
            v.sort_unstable();
            v.dedup();
        }
    }

    /// Add `other`'s graphs.
    pub fn extend(&mut self, other: &GraphRule) {
        self.default_graph |= other.default_graph;
        self.blank_nodes |= other.blank_nodes;
        self.iris.extend(other.iris.iter().cloned());
        self.patterns.extend(other.patterns.iter().cloned());
        self.protected.extend(other.protected.iter().cloned());
        self.excluded.extend(other.excluded.iter().cloned());
        self.normalize();
    }

    /// Whether the named graph `iri` is in the set.
    pub fn matches_iri(&self, iri: &str) -> bool {
        if self.excluded.iter().any(|p| glob(p, iri)) {
            return false;
        }
        if self.iris.binary_search_by(|x| x.as_str().cmp(iri)).is_ok() {
            return true;
        }
        if self.protected.iter().any(|p| p == iri) {
            return false;
        }
        self.patterns.iter().any(|p| glob(p, iri))
    }
}

/// Graphs a request may read or write: all of them, or a [`GraphRule`]'s.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Graphs {
    All,
    Only(GraphRule),
}

impl Graphs {
    /// No graph at all.
    pub fn none() -> Graphs {
        Graphs::Only(GraphRule::default())
    }

    pub fn is_all(&self) -> bool {
        matches!(self, Graphs::All)
    }

    /// The union of two sets.
    pub fn union(&self, other: &Graphs) -> Graphs {
        match (self, other) {
            (Graphs::All, _) | (_, Graphs::All) => Graphs::All,
            (Graphs::Only(a), Graphs::Only(b)) => {
                let mut r = a.clone();
                r.extend(b);
                Graphs::Only(r)
            }
        }
    }

    /// Whether the default graph is in the set.
    pub fn default_graph(&self) -> bool {
        match self {
            Graphs::All => true,
            Graphs::Only(r) => r.default_graph,
        }
    }

    /// Whether the graph named by `term` (the default graph when `None`) is in the set.
    pub fn allows(&self, term: Option<&Term>) -> bool {
        match (self, term) {
            (Graphs::All, _) => true,
            (Graphs::Only(r), None) => r.default_graph,
            (Graphs::Only(r), Some(Term::NamedNode(n))) => {
                if n.as_str() == DEFAULT_GRAPH_IRI {
                    r.default_graph
                } else {
                    n.as_str() != UNION_GRAPH_IRI && r.matches_iri(n.as_str())
                }
            }
            (Graphs::Only(r), Some(Term::BlankNode(_))) => r.blank_nodes,
            (Graphs::Only(_), Some(_)) => false,
        }
    }

    /// The set without the named graph `iri`, which only an exact name may bring back.
    pub fn without_iri(&self, iri: &str) -> Graphs {
        let mut r = match self {
            Graphs::All => GraphRule {
                default_graph: true,
                patterns: vec!["*".into()],
                blank_nodes: true,
                ..Default::default()
            },
            Graphs::Only(r) => r.clone(),
        };
        r.iris.retain(|x| x != iri);
        r.protected.push(iri.to_string());
        r.normalize();
        Graphs::Only(r)
    }

    /// The set without the named graphs that `pattern` (an IRI, or an IRI pattern with
    /// `*`) matches, even where the set names them exactly.
    pub fn without_pattern(&self, pattern: &str) -> Graphs {
        let mut r = match self {
            Graphs::All => GraphRule {
                default_graph: true,
                patterns: vec!["*".into()],
                blank_nodes: true,
                ..Default::default()
            },
            Graphs::Only(r) => r.clone(),
        };
        r.excluded.push(pattern.to_string());
        r.normalize();
        Graphs::Only(r)
    }

    /// Whether the graph named by `iri` is in the set (`urn:x-arq:DefaultGraph` is the
    /// default graph).
    pub fn allows_iri(&self, iri: &str) -> bool {
        match self {
            Graphs::All => true,
            Graphs::Only(r) if iri == DEFAULT_GRAPH_IRI || iri == "default" => r.default_graph,
            Graphs::Only(r) => iri != UNION_GRAPH_IRI && r.matches_iri(iri),
        }
    }
}

/// What one request may read and write. Writes need both: a graph is writable when it
/// is in [`write`](Self::write) and in [`read`](Self::read).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GraphAccess {
    pub read: Graphs,
    pub write: Graphs,
    /// protections that hide some triples of the graphs read or written (`None`: none)
    pub triples: Option<Arc<TripleRules>>,
}

impl GraphAccess {
    /// Every graph, for reading and writing.
    pub fn all() -> GraphAccess {
        GraphAccess {
            read: Graphs::All,
            write: Graphs::All,
            triples: None,
        }
    }

    /// Graphs only, without protections of triples.
    pub fn graphs(read: Graphs, write: Graphs) -> GraphAccess {
        GraphAccess {
            read,
            write,
            triples: None,
        }
    }

    /// A view with protections of triples. Rules the caller's grants lift in every
    /// graph are left out, and while a rule that hides materialized inferences is in
    /// force for reads, the read graphs leave out the graph of inferences.
    pub fn with_triples(read: Graphs, write: Graphs, rules: TripleRules) -> GraphAccess {
        let rules = rules.in_force();
        let read = if rules.hides_inferences() {
            read.without_iri(triples::INFERRED_GRAPH)
        } else {
            read
        };
        GraphAccess {
            read,
            write,
            triples: (!rules.rules.is_empty()).then(|| Arc::new(rules)),
        }
    }

    /// Whether some protection hides triples from reads.
    pub fn hides_triples(&self) -> bool {
        self.triples.as_ref().is_some_and(|t| t.hides())
    }

    /// Whether reads see everything: every graph, and every triple of them.
    pub fn reads_everything(&self) -> bool {
        self.read.is_all() && !self.hides_triples()
    }

    /// Whether reads see every graph (no filtering is needed).
    pub fn reads_all(&self) -> bool {
        self.read.is_all()
    }

    /// Whether the graph named by `term` (the default graph when `None`) is readable.
    pub fn readable(&self, term: Option<&Term>) -> bool {
        self.read.allows(term)
    }

    /// Whether the graph named by `term` (the default graph when `None`) is writable.
    pub fn writable(&self, term: Option<&Term>) -> bool {
        self.write.allows(term) && self.read.allows(term)
    }

    /// Whether the graph named by `iri` is writable (see [`Graphs::allows_iri`]).
    pub fn writable_iri(&self, iri: &str) -> bool {
        self.write.allows_iri(iri) && self.read.allows_iri(iri)
    }

    /// A stable key of the read rule (for caches).
    pub fn read_key(&self) -> String {
        format!("{:?}", self.read)
    }

    /// The snapshot as this view reads it: `snap` with the quads its protections hide
    /// taken out (see [`triples`]). Kept with the snapshot per view, so repeated
    /// requests at one commit build it once. `snap` itself when nothing is hidden.
    pub fn masked(&self, snap: &Arc<Snapshot>) -> Result<Arc<Snapshot>> {
        match self.triples.as_ref().filter(|t| t.hides()) {
            Some(t) if snap.mask.is_none() => triples::masked(self, t, snap),
            _ => Ok(snap.clone()),
        }
    }

    /// The error of a write outside the write graphs.
    pub fn refused(term: Option<&Term>) -> Error {
        Error::NotPermitted(match term {
            None => "write access to the default graph required".to_string(),
            Some(Term::NamedNode(n)) if n.as_str() == DEFAULT_GRAPH_IRI => {
                "write access to the default graph required".to_string()
            }
            Some(t) => format!("write access to graph {t} required"),
        })
    }

    /// [`refused`](Self::refused) for a graph IRI.
    pub fn refused_iri(iri: &str) -> Error {
        if iri == DEFAULT_GRAPH_IRI || iri == "default" {
            Self::refused(None)
        } else {
            Error::NotPermitted(format!("write access to graph <{iri}> required"))
        }
    }

    /// Check that the graph `g` of `snap` is writable: [`Error::NotPermitted`] if not.
    pub fn check_write(&self, snap: &Snapshot, g: Id) -> Result<()> {
        if self.write.is_all() && self.read.is_all() {
            return Ok(());
        }
        if g == Id::DEFAULT_GRAPH {
            return if self.writable(None) {
                Ok(())
            } else {
                Err(Self::refused(None))
            };
        }
        match graph_term(snap, g) {
            Some(t) if self.writable(Some(&t)) => Ok(()),
            Some(t) => Err(Self::refused(Some(&t))),
            None => Err(Error::NotPermitted(
                "write access to an unnamed graph required".into(),
            )),
        }
    }

    /// The named graphs of `snap` that are readable, sorted by id. Kept with the
    /// snapshot per rule, so repeated queries at one commit list them once.
    pub fn visible_named(&self, snap: &Snapshot) -> Result<Arc<Vec<Id>>> {
        Ok(self.visible_named_all(snap)?.0)
    }

    /// [`visible_named`](Self::visible_named), and whether those are all the named graphs
    /// of `snap`.
    pub fn visible_named_all(&self, snap: &Snapshot) -> Result<(Arc<Vec<Id>>, bool)> {
        let key = self.read_key();
        if let Some(v) = snap.counts.view(&key) {
            return Ok(v);
        }
        let all = snap.graph_ids()?;
        let n = all.len();
        let mut v: Vec<Id> = all
            .into_iter()
            .filter(|&g| self.readable_id(snap, g))
            .collect();
        v.sort_unstable();
        let v = (Arc::new(v), false);
        let v = (v.0.clone(), v.0.len() == n);
        snap.counts.put_view(key, v.clone());
        Ok(v)
    }

    /// The quads of `snap` in the graphs this view reads (from the index counts of each
    /// graph).
    pub fn visible_quads(&self, snap: &Snapshot) -> Result<u64> {
        if self.read.is_all() {
            return Ok(snap.len());
        }
        let mut n = 0;
        if self.read.default_graph() {
            n += snap.count(crate::index::Perm::Gspo, &[Id::DEFAULT_GRAPH.0])?;
        }
        for g in self.visible_named(snap)?.iter() {
            n += snap.count(crate::index::Perm::Gspo, &[g.0])?;
        }
        Ok(n)
    }

    /// Whether the graph id `g` of `snap` is readable.
    pub fn readable_id(&self, snap: &Snapshot, g: Id) -> bool {
        if self.read.is_all() {
            return true;
        }
        if g == Id::DEFAULT_GRAPH {
            return self.read.default_graph();
        }
        graph_term(snap, g).is_some_and(|t| self.read.allows(Some(&t)))
    }
}

/// The term naming graph `g` other than the default graph (`None` for ids that name no
/// term).
pub(crate) fn graph_term(snap: &Snapshot, g: Id) -> Option<Term> {
    match g.tag() {
        Tag::BNode => Some(Term::BlankNode(crate::store::bnode_for(g))),
        Tag::Undef | Tag::Special => None,
        _ => snap.term(g),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::NamedNode;

    #[test]
    fn globs() {
        assert!(glob("*", "anything"));
        assert!(glob("http://ex/a/*", "http://ex/a/1"));
        assert!(!glob("http://ex/a/*", "http://ex/b/1"));
        assert!(glob("http://ex/*/x", "http://ex/a/b/x"));
    }

    #[test]
    fn rules() {
        let r = GraphRule::new(
            ["http://ex/a/*", "default", "http://ex/one"],
            &["urn:x-sparkles:inferred"],
        );
        assert!(r.default_graph);
        assert!(r.matches_iri("http://ex/a/1"));
        assert!(r.matches_iri("http://ex/one"));
        assert!(!r.matches_iri("http://ex/one/two"));
        let any = GraphRule::new(["*"], &["urn:x-sparkles:inferred"]);
        assert!(any.matches_iri("http://ex/z"));
        assert!(!any.matches_iri("urn:x-sparkles:inferred"));
        assert!(!any.default_graph);
        let exact = GraphRule::new(["urn:x-sparkles:inferred"], &["urn:x-sparkles:inferred"]);
        assert!(exact.matches_iri("urn:x-sparkles:inferred"));
        let g = Graphs::Only(r);
        let n = |s: &str| Term::NamedNode(NamedNode::new_unchecked(s));
        assert!(g.allows(None));
        assert!(g.allows(Some(&n(DEFAULT_GRAPH_IRI))));
        assert!(!g.allows(Some(&n(UNION_GRAPH_IRI))));
        assert!(!g.allows(Some(&Term::BlankNode(oxrdf::BlankNode::default()))));
        let a = GraphAccess {
            read: g.clone(),
            write: Graphs::Only(GraphRule::new(["http://ex/a/1", "http://ex/z"], &[])),
            triples: None,
        };
        assert!(a.writable(Some(&n("http://ex/a/1"))));
        // writable only when readable too
        assert!(!a.writable(Some(&n("http://ex/z"))));
        assert!(!a.writable(None));
        assert_eq!(Graphs::All.union(&g), Graphs::All);
        assert!(Graphs::none().union(&g).allows(None));
    }
}
