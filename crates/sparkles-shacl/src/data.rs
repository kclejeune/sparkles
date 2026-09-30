//! Data graph access over a store [`Snapshot`]: index scans for `(s p ?)`, `(? p o)`,
//! `(s ? ?)`, `(? p ?)`, restricted to the graphs that make up the data graph, plus
//! term resolution for shape constants that are not in the store (local ids).

use crate::shapes::Shapes;
use crate::vocab::{rdf, rdfs};
use anyhow::{Result, bail};
use oxrdf::{Graph, Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles::id::{Id, Tag};
use sparkles::index::{Key, Perm};
use sparkles::sparql::ctx::{DEFAULT_GRAPH_IRI, UNION_GRAPH_IRI};
use sparkles::sparql::value::Value;
use sparkles::store::{Chunk, Snapshot};
use std::sync::{Arc, RwLock};

/// Which graphs of the store form the data graph.
#[derive(Clone, Debug)]
pub(crate) enum GraphSel {
    /// every graph (union default graph)
    All,
    /// these graph ids (sorted)
    Set(Vec<u64>),
}

impl GraphSel {
    #[inline]
    fn accepts(&self, g: u64) -> bool {
        match self {
            GraphSel::All => true,
            GraphSel::Set(gs) => gs.len() == 1 && gs[0] == g || gs.binary_search(&g).is_ok(),
        }
    }

    pub fn ids(&self) -> Option<Vec<Id>> {
        match self {
            GraphSel::All => None,
            GraphSel::Set(gs) => Some(gs.iter().map(|&g| Id(g)).collect()),
        }
    }
}

/// The data graph plus the id mapping of a shapes graph's terms.
pub(crate) struct DataGraph {
    pub snap: Arc<Snapshot>,
    pub sel: GraphSel,
    /// terms that are not in the store; `Id::local(i)` ↔ `locals[i]`
    locals: Vec<Term>,
    local_index: FxHashMap<Term, Id>,
    pub rdf_type: Id,
    pub sub_class_of: Id,
    /// class → the class and all its subclasses (data graph)
    subclasses: RwLock<FxHashMap<Id, Arc<FxHashSet<Id>>>>,
}

/// Resolve a graph IRI to its id (`None` if the graph does not exist).
fn graph_id(snap: &Snapshot, iri: &str) -> Option<Id> {
    if iri == DEFAULT_GRAPH_IRI {
        return Some(Id::DEFAULT_GRAPH);
    }
    snap.lookup_iri(iri)
}

impl DataGraph {
    pub fn new(
        snap: Arc<Snapshot>,
        data_graph: Option<&str>,
        extra: &[String],
        shapes: &Shapes,
    ) -> Result<(DataGraph, Vec<Id>)> {
        let sel = match data_graph {
            Some(UNION_GRAPH_IRI) => GraphSel::All,
            None if snap.union_default_graph => GraphSel::All,
            _ => {
                let mut gs = vec![match data_graph {
                    None => Id::DEFAULT_GRAPH.0,
                    Some(iri) => match graph_id(&snap, iri) {
                        Some(g) => g.0,
                        None => bail!("data graph <{iri}> does not exist"),
                    },
                }];
                gs.extend(extra.iter().filter_map(|g| graph_id(&snap, g)).map(|g| g.0));
                gs.sort_unstable();
                gs.dedup();
                GraphSel::Set(gs)
            }
        };
        let mut d = DataGraph {
            rdf_type: Id::UNDEF,
            sub_class_of: Id::UNDEF,
            snap,
            sel,
            locals: Vec::new(),
            local_index: FxHashMap::default(),
            subclasses: RwLock::new(FxHashMap::default()),
        };
        d.rdf_type = d.resolve(&rdf::TYPE.into_owned().into(), false);
        d.sub_class_of = d.resolve(&rdfs::SUB_CLASS_OF.into_owned().into(), false);
        let ids = shapes
            .terms
            .iter()
            .map(|t| d.resolve(t, shapes.bnodes_in_store))
            .collect();
        Ok((d, ids))
    }

    /// Id of a term: store id if it exists, else a local id.
    pub fn resolve(&mut self, t: &Term, bnodes_in_store: bool) -> Id {
        if let Some(&id) = self.local_index.get(t) {
            return id;
        }
        let stored = match t {
            Term::BlankNode(_) if !bnodes_in_store => None,
            _ => self.snap.lookup_term(t),
        };
        if let Some(id) = stored {
            return id;
        }
        let id = Id::local(self.locals.len() as u64);
        self.locals.push(t.clone());
        self.local_index.insert(t.clone(), id);
        id
    }

    pub fn term(&self, id: Id) -> Option<Term> {
        match id.tag() {
            Tag::Local => self.locals.get(id.payload() as usize).cloned(),
            _ => self.snap.term(id),
        }
    }

    pub fn value(&self, id: Id) -> Option<Value> {
        self.term(id).map(|t| Value::from_term(&t))
    }

    #[inline]
    fn stored(id: Id) -> bool {
        !matches!(id.tag(), Tag::Local | Tag::Undef)
    }

    fn scan(&self, perm: Perm, prefix: &[u64], mut f: impl FnMut(&Key)) -> Result<()> {
        let sel = &self.sel;
        let gcol = 3; // graph is the last key column of every non-GSPO permutation
        self.snap.scan(perm, prefix, |c| {
            match c {
                Chunk::Block(b, s, e) => {
                    let gs = &b.cols[gcol];
                    for (i, &g) in gs.iter().enumerate().take(e).skip(s) {
                        if sel.accepts(g) {
                            f(&b.key(i));
                        }
                    }
                }
                Chunk::Row(k) => {
                    if sel.accepts(k[gcol]) {
                        f(&k)
                    }
                }
            }
            Ok(true)
        })?;
        Ok(())
    }

    /// Objects of `(s, p, ?)` (distinct).
    pub fn objects(&self, s: Id, p: Id) -> Result<Vec<Id>> {
        let mut out: Vec<Id> = Vec::new();
        if !Self::stored(s) || !Self::stored(p) {
            return Ok(out);
        }
        self.scan(Perm::Spo, &[s.0, p.0], |k| {
            if out.last().map(|l| l.0) != Some(k[2]) {
                out.push(Id(k[2]));
            }
        })?;
        Ok(out)
    }

    /// Subjects of `(?, p, o)` (distinct).
    pub fn subjects(&self, p: Id, o: Id) -> Result<Vec<Id>> {
        let mut out: Vec<Id> = Vec::new();
        if !Self::stored(o) || !Self::stored(p) {
            return Ok(out);
        }
        self.scan(Perm::Pos, &[p.0, o.0], |k| {
            if out.last().map(|l| l.0) != Some(k[2]) {
                out.push(Id(k[2]));
            }
        })?;
        Ok(out)
    }

    /// `(p, o)` pairs of all triples with subject `s` (distinct).
    pub fn out_edges(&self, s: Id) -> Result<Vec<(Id, Id)>> {
        let mut out: Vec<(Id, Id)> = Vec::new();
        if !Self::stored(s) {
            return Ok(out);
        }
        self.scan(Perm::Spo, &[s.0], |k| {
            let e = (Id(k[1]), Id(k[2]));
            if out.last() != Some(&e) {
                out.push(e);
            }
        })?;
        Ok(out)
    }

    /// Distinct subjects of triples with predicate `p`.
    pub fn subjects_of(&self, p: Id) -> Result<Vec<Id>> {
        self.distinct_second(Perm::Pso, p)
    }

    /// Distinct objects of triples with predicate `p`.
    pub fn objects_of(&self, p: Id) -> Result<Vec<Id>> {
        self.distinct_second(Perm::Pos, p)
    }

    fn distinct_second(&self, perm: Perm, p: Id) -> Result<Vec<Id>> {
        let mut out: Vec<Id> = Vec::new();
        if !Self::stored(p) {
            return Ok(out);
        }
        self.scan(perm, &[p.0], |k| {
            if out.last().map(|l| l.0) != Some(k[1]) {
                out.push(Id(k[1]));
            }
        })?;
        Ok(out)
    }

    /// `class` and all its subclasses (rdfs:subClassOf* in the data graph), cached.
    pub fn subclasses(&self, class: Id) -> Result<Arc<FxHashSet<Id>>> {
        if let Some(s) = self.subclasses.read().unwrap().get(&class) {
            return Ok(s.clone());
        }
        let mut set = FxHashSet::default();
        set.insert(class);
        let mut stack = vec![class];
        while let Some(c) = stack.pop() {
            for sub in self.subjects(self.sub_class_of, c)? {
                if set.insert(sub) {
                    stack.push(sub);
                }
            }
        }
        let set = Arc::new(set);
        self.subclasses.write().unwrap().insert(class, set.clone());
        Ok(set)
    }

    /// Is `node` a SHACL instance of `class` (rdf:type/rdfs:subClassOf*)?
    pub fn is_instance(&self, node: Id, class: Id) -> Result<bool> {
        let types = self.objects(node, self.rdf_type)?;
        if types.is_empty() {
            return Ok(false);
        }
        if types.contains(&class) {
            return Ok(true);
        }
        let subs = self.subclasses(class)?;
        Ok(types.iter().any(|t| subs.contains(t)))
    }

    /// All SHACL instances of `class`.
    pub fn instances(&self, class: Id) -> Result<Vec<Id>> {
        let subs = self.subclasses(class)?;
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        let mut classes: Vec<Id> = subs.iter().copied().collect();
        classes.sort_unstable();
        for c in classes {
            for s in self.subjects(self.rdf_type, c)? {
                if seen.insert(s) {
                    out.push(s);
                }
            }
        }
        Ok(out)
    }
}

/// Read one graph of the store into an oxrdf graph (`None` / `urn:x-arq:DefaultGraph` =
/// the default graph). Blank nodes keep their store labels.
pub(crate) fn read_graph(snap: &Snapshot, graph: Option<&str>) -> Result<Graph> {
    let g = match graph {
        None => Id::DEFAULT_GRAPH,
        Some(iri) => match graph_id(snap, iri) {
            Some(g) => g,
            None => bail!("graph <{iri}> does not exist"),
        },
    };
    let mut out = Graph::new();
    let mut err = None;
    snap.scan(Perm::Gspo, &[g.0], |c| {
        let mut add = |k: Key| {
            let q = Perm::Gspo.to_quad(&k);
            match snap.quad_to_terms(&q) {
                Some(q) => {
                    out.insert(&Triple::new(q.subject, q.predicate, q.object));
                }
                None => err = Some(format!("cannot decode quad {q:?}")),
            }
        };
        match c {
            Chunk::Block(b, s, e) => (s..e).for_each(|i| add(b.key(i))),
            Chunk::Row(k) => add(k),
        }
        Ok(true)
    })?;
    if let Some(e) = err {
        bail!(e);
    }
    Ok(out)
}
