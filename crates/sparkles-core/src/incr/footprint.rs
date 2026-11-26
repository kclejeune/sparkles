//! Footprints: which changed quads can affect a circuit (spec F13 §3.3).
//!
//! A footprint lists, for every triple pattern and path of a query, its graph and its
//! constant subject, predicate and object. A commit whose changes match no entry cannot
//! change the result, so the pass skips the circuit after one hash probe per changed
//! quad.

use crate::id::Id;
use crate::store::Snapshot;
use oxrdf::Term;
use rustc_hash::{FxHashMap, FxHashSet};

/// The graph an entry reads.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FGraph {
    /// any graph (a query with a dataset clause)
    Any,
    /// the default graph (the union of the named graphs when the store says so)
    Default,
    /// any named graph (`GRAPH ?g`)
    Named,
    /// this named graph
    Graph(Term),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    pub g: FGraph,
    pub s: Option<Term>,
    pub p: Option<Term>,
    pub o: Option<Term>,
}

#[derive(Clone, Debug, Default)]
pub struct Footprint {
    pub entries: Vec<Entry>,
    /// every change matches
    pub all: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RGraph {
    Any,
    Default,
    Named,
    Graph(Id),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct REntry {
    g: RGraph,
    s: Option<Id>,
    o: Option<Id>,
    p: Option<Id>,
}

/// A footprint with its constants resolved against a snapshot (or against id tables).
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    all: bool,
    by_p: FxHashMap<Id, Vec<REntry>>,
    any_p: Vec<REntry>,
    /// Graph-unrestricted (`RGraph::Any`) triple patterns indexed in O(1) hash sets so
    /// large rule footprints (such as query-time specialization) match without linear scans.
    any_preds: FxHashSet<Id>,
    any_ps: FxHashSet<(Id, Id)>,
    any_po: FxHashSet<(Id, Id)>,
    any_pso: FxHashSet<[Id; 3]>,
    any_free: Vec<(Option<Id>, Option<Id>)>,
    union: bool,
    /// the generation and delta vocabulary size the constants were resolved against
    at: (u64, u64),
}

impl Footprint {
    /// A footprint that matches every quad.
    pub fn all() -> Footprint {
        Footprint {
            entries: Vec::new(),
            all: true,
        }
    }

    /// Whether no entry and no wildcard is recorded.
    pub fn is_empty(&self) -> bool {
        !self.all && self.entries.is_empty()
    }

    pub fn add(&mut self, e: Entry) {
        if !self.entries.contains(&e) {
            self.entries.push(e);
        }
    }

    /// Add a whole-graph entry matching every triple in `g`.
    pub fn add_graph(&mut self, g: FGraph) {
        self.add(Entry {
            g,
            s: None,
            p: None,
            o: None,
        });
    }

    pub fn extend(&mut self, other: &Footprint) {
        self.all |= other.all;
        for e in &other.entries {
            self.add(e.clone());
        }
    }

    /// The footprint with its constants as ids of `snap`. A constant `snap` does not
    /// hold matches no quad, so its entry is left out.
    pub fn resolve(&self, snap: &Snapshot) -> Resolved {
        let mut r = Resolved {
            all: self.all,
            union: snap.union_default_graph,
            at: (snap.generation.uid, snap.dvocab_len),
            ..Default::default()
        };
        let id = |t: &Option<Term>| -> Option<Option<Id>> {
            match t {
                None => Some(None),
                Some(t) => snap.lookup_term(t).map(Some),
            }
        };
        for e in &self.entries {
            let g = match &e.g {
                FGraph::Any => RGraph::Any,
                FGraph::Default => RGraph::Default,
                FGraph::Named => RGraph::Named,
                FGraph::Graph(t) => match snap.lookup_term(t) {
                    Some(i) => RGraph::Graph(i),
                    None => continue,
                },
            };
            let (Some(s), Some(p), Some(o)) = (id(&e.s), id(&e.p), id(&e.o)) else {
                continue;
            };
            if g == RGraph::Any {
                r.add_triple_pattern(s, p, o);
            } else {
                let re = REntry { g, s, o, p };
                match p {
                    Some(p) => r.by_p.entry(p).or_default().push(re),
                    None => r.any_p.push(re),
                }
            }
        }
        r
    }
}

impl Resolved {
    /// A resolved footprint that matches every quad.
    pub fn all() -> Resolved {
        Resolved {
            all: true,
            ..Default::default()
        }
    }

    /// Whether every quad matches.
    pub fn everything(&self) -> bool {
        self.all
    }

    /// The number of distinct resolved pattern entries held.
    pub fn pattern_count(&self) -> usize {
        self.any_preds.len()
            + self.any_ps.len()
            + self.any_po.len()
            + self.any_pso.len()
            + self.any_free.len()
            + self.by_p.values().map(Vec::len).sum::<usize>()
            + self.any_p.len()
    }

    /// Add a graph-unrestricted (`RGraph::Any`) triple pattern by resolved ids.
    pub fn add_triple_pattern(&mut self, s: Option<Id>, p: Option<Id>, o: Option<Id>) {
        if self.all {
            return;
        }
        match (s, p, o) {
            (None, None, None) => {
                self.all = true;
            }
            (s, None, o) => {
                self.any_free.push((s, o));
            }
            (None, Some(p), None) => {
                self.any_preds.insert(p);
            }
            (Some(s), Some(p), None) => {
                self.any_ps.insert((p, s));
            }
            (None, Some(p), Some(o)) => {
                self.any_po.insert((p, o));
            }
            (Some(s), Some(p), Some(o)) => {
                self.any_pso.insert([s, p, o]);
            }
        }
    }

    /// Whether the resolution still holds for `snap`.
    pub fn current(&self, snap: &Snapshot) -> bool {
        self.at == (snap.generation.uid, snap.dvocab_len) && self.union == snap.union_default_graph
    }

    fn entry_matches(&self, e: &REntry, q: &[Id; 4]) -> bool {
        let g_ok = match e.g {
            RGraph::Any => true,
            RGraph::Default => {
                q[3] == Id::DEFAULT_GRAPH || (self.union && q[3] != Id::DEFAULT_GRAPH)
            }
            RGraph::Named => q[3] != Id::DEFAULT_GRAPH,
            RGraph::Graph(g) => q[3] == g,
        };
        g_ok && e.s.is_none_or(|s| s == q[0])
            && e.p.is_none_or(|p| p == q[1])
            && e.o.is_none_or(|o| o == q[2])
    }

    /// Whether a triple `[s, p, o]` matches any graph-unrestricted pattern in this
    /// resolved footprint.
    pub fn matches_triple(&self, t: &[Id; 3]) -> bool {
        if self.all {
            return true;
        }
        let [s, p, o] = *t;
        self.any_preds.contains(&p)
            || self.any_ps.contains(&(p, s))
            || self.any_po.contains(&(p, o))
            || self.any_pso.contains(t)
            || self
                .any_free
                .iter()
                .any(|&(fs, fo)| fs.is_none_or(|x| x == s) && fo.is_none_or(|x| x == o))
    }

    /// Whether a predicate `p` matches a whole-predicate `(None, Some(p), None)` entry.
    pub fn matches_predicate(&self, p: Id) -> bool {
        self.all || self.any_preds.contains(&p)
    }

    /// Whether a changed quad can affect the circuit.
    pub fn matches(&self, q: &[Id; 4]) -> bool {
        if self.matches_triple(&[q[0], q[1], q[2]]) {
            return true;
        }
        if let Some(es) = self.by_p.get(&q[1])
            && es.iter().any(|e| self.entry_matches(e, q))
        {
            return true;
        }
        self.any_p.iter().any(|e| self.entry_matches(e, q))
    }

    /// Whether any of the changes can affect the circuit.
    pub fn touches<'a>(&self, mut quads: impl Iterator<Item = &'a [Id; 4]>) -> bool {
        quads.any(|q| self.matches(q))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triple_pattern_matching_and_wildcards() {
        let mut r = Resolved::default();
        let (s, p, o) = (Id(10), Id(20), Id(30));
        r.add_triple_pattern(None, Some(p), Some(o));
        assert!(r.matches_triple(&[s, p, o]));
        assert!(r.matches_triple(&[Id(99), p, o]));
        assert!(!r.matches_triple(&[s, p, Id(31)]));
        assert!(!r.matches_predicate(p));
        r.add_triple_pattern(None, Some(p), None);
        assert!(r.matches_predicate(p));
        assert!(r.matches(&[s, p, Id(31), Id::DEFAULT_GRAPH]));
        assert_eq!(r.pattern_count(), 2);
        r.add_triple_pattern(None, None, None);
        assert!(r.everything());
        assert!(r.matches_triple(&[Id(1), Id(2), Id(3)]));
    }
}
