//! Data graph access over a store [`Snapshot`]: the shared data graph of
//! [`sparkles::validation`] (index scans restricted to the graphs that make it up), plus
//! term resolution for shape constants that are not in the store (local ids) and the
//! subclass closure of `sh:class`.

use crate::shapes::Shapes;
use crate::vocab::{rdf, rdfs};
use anyhow::{Result, bail};
use oxrdf::{Graph, Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles::id::{Id, Tag};
use sparkles::index::{Key, Perm};
use sparkles::sparql::value::Value;
use sparkles::store::{Chunk, Snapshot};
use sparkles::validation::{self, graph_id};
use std::sync::{Arc, RwLock};

/// The data graph ([`sparkles::validation::DataGraph`], whose scans it derefs to) plus
/// the id mapping of a shapes graph's terms.
pub(crate) struct DataGraph {
    graph: validation::DataGraph,
    /// terms that are not in the store; `Id::local(i)` ↔ `locals[i]`
    locals: Vec<Term>,
    local_index: FxHashMap<Term, Id>,
    pub rdf_type: Id,
    pub sub_class_of: Id,
    /// `rdf:first`, `rdf:rest` and `rdf:nil` (local ids when the store lacks them)
    pub rdf_first: Id,
    pub rdf_rest: Id,
    pub rdf_nil: Id,
    /// class → the class and all its subclasses (data graph)
    subclasses: RwLock<FxHashMap<Id, Arc<FxHashSet<Id>>>>,
}

impl std::ops::Deref for DataGraph {
    type Target = validation::DataGraph;
    fn deref(&self) -> &validation::DataGraph {
        &self.graph
    }
}

impl DataGraph {
    pub fn new(
        snap: Arc<Snapshot>,
        data_graph: Option<&str>,
        extra: &[String],
        exclude: &[String],
        shapes: &Shapes,
    ) -> Result<(DataGraph, Vec<Id>)> {
        let graph = validation::DataGraph::new(snap, data_graph, extra, exclude)?;
        let mut d = DataGraph {
            rdf_type: Id::UNDEF,
            sub_class_of: Id::UNDEF,
            rdf_first: Id::UNDEF,
            rdf_rest: Id::UNDEF,
            rdf_nil: Id::UNDEF,
            graph,
            locals: Vec::new(),
            local_index: FxHashMap::default(),
            subclasses: RwLock::new(FxHashMap::default()),
        };
        d.rdf_type = d.resolve(&rdf::TYPE.into_owned().into(), false);
        d.sub_class_of = d.resolve(&rdfs::SUB_CLASS_OF.into_owned().into(), false);
        d.rdf_first = d.resolve(&rdf::FIRST.into_owned().into(), false);
        d.rdf_rest = d.resolve(&rdf::REST.into_owned().into(), false);
        d.rdf_nil = d.resolve(&rdf::NIL.into_owned().into(), false);
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
