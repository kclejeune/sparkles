//! Footprints: which changed quads can affect a circuit (spec F13 §3.3).
//!
//! A footprint lists, for every triple pattern and path of a query, its graph and its
//! constant subject, predicate and object. A commit whose changes match no entry cannot
//! change the result, so the pass skips the circuit after one hash probe per changed
//! quad.

use crate::id::Id;
use crate::store::Snapshot;
use oxrdf::Term;
use rustc_hash::FxHashMap;

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

#[derive(Clone, Copy, Debug)]
enum RGraph {
    Any,
    Default,
    Named,
    Graph(Id),
}

#[derive(Clone, Copy, Debug)]
struct REntry {
    g: RGraph,
    s: Option<Id>,
    o: Option<Id>,
    p: Option<Id>,
}

/// A footprint with its constants resolved against a snapshot.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    all: bool,
    by_p: FxHashMap<Id, Vec<REntry>>,
    any_p: Vec<REntry>,
    union: bool,
    /// the generation and delta vocabulary size the constants were resolved against
    at: (u64, u64),
}

impl Footprint {
    pub fn add(&mut self, e: Entry) {
        if !self.entries.contains(&e) {
            self.entries.push(e);
        }
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
            let re = REntry { g, s, o, p };
            match p {
                Some(p) => r.by_p.entry(p).or_default().push(re),
                None => r.any_p.push(re),
            }
        }
        r
    }
}

impl Resolved {
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

    /// Whether a changed quad can affect the circuit.
    pub fn matches(&self, q: &[Id; 4]) -> bool {
        if self.all {
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
