//! Data graph access over a store [`Snapshot`]: the shared data graph of
//! [`sparkles_core::validation`] (index scans restricted to the graphs that make it up), plus
//! term resolution for shape constants that are not in the store (local ids) and the
//! subclass closure of `sh:class`.

use crate::shapes::Shapes;
use crate::vocab::{rdf, rdfs};
use anyhow::{Result, bail};
use oxrdf::{Graph, Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_core::id::{Id, Tag};
use sparkles_core::index::{Key, Perm};
use sparkles_core::sparql::value::Value;
use sparkles_core::store::{Chunk, Snapshot};
use sparkles_core::validation::{self, graph_id};
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// Terms with local ids.
#[derive(Default)]
struct Locals {
    terms: Vec<Term>,
    index: FxHashMap<Term, Id>,
}

/// The data graph ([`sparkles_core::validation::DataGraph`], whose scans it derefs to) plus
/// the id mapping of a shapes graph's terms.
pub(crate) struct DataGraph {
    graph: validation::DataGraph,
    /// terms that are not in the store; `Id::local(i)` ↔ `locals.terms[i]`. Node
    /// expressions add the terms they compute while a validation runs.
    locals: RwLock<Locals>,
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
        Self::new_until(snap, data_graph, extra, exclude, shapes, None)
    }

    pub fn new_until(
        snap: Arc<Snapshot>,
        data_graph: Option<&str>,
        extra: &[String],
        exclude: &[String],
        shapes: &Shapes,
        deadline: Option<Instant>,
    ) -> Result<(DataGraph, Vec<Id>)> {
        check_deadline(deadline)?;
        let graph = validation::DataGraph::new(snap, data_graph, extra, exclude)?;
        let mut d = DataGraph {
            rdf_type: Id::UNDEF,
            sub_class_of: Id::UNDEF,
            rdf_first: Id::UNDEF,
            rdf_rest: Id::UNDEF,
            rdf_nil: Id::UNDEF,
            graph,
            locals: RwLock::new(Locals::default()),
            subclasses: RwLock::new(FxHashMap::default()),
        };
        d.rdf_type = d.resolve(&rdf::TYPE.into_owned().into(), false);
        d.sub_class_of = d.resolve(&rdfs::SUB_CLASS_OF.into_owned().into(), false);
        d.rdf_first = d.resolve(&rdf::FIRST.into_owned().into(), false);
        d.rdf_rest = d.resolve(&rdf::REST.into_owned().into(), false);
        d.rdf_nil = d.resolve(&rdf::NIL.into_owned().into(), false);
        let mut ids = Vec::with_capacity(shapes.terms.len());
        for (i, term) in shapes.terms.iter().enumerate() {
            if i % 256 == 0 {
                check_deadline(deadline)?;
            }
            ids.push(d.resolve(term, shapes.bnodes_in_store));
        }
        check_deadline(deadline)?;
        Ok((d, ids))
    }

    /// Id of a term: store id if it exists, else a local id.
    pub fn resolve(&mut self, t: &Term, bnodes_in_store: bool) -> Id {
        self.resolve_shared(t, bnodes_in_store)
    }

    /// [`DataGraph::resolve`] for a term computed during a validation.
    pub fn resolve_shared(&self, t: &Term, bnodes_in_store: bool) -> Id {
        if let Some(&id) = self.locals.read().unwrap().index.get(t) {
            return id;
        }
        let stored = match t {
            Term::BlankNode(_) if !bnodes_in_store => None,
            _ => self.snap.lookup_term(t),
        };
        if let Some(id) = stored {
            return id;
        }
        let mut l = self.locals.write().unwrap();
        if let Some(&id) = l.index.get(t) {
            return id;
        }
        let id = Id::local(l.terms.len() as u64);
        l.terms.push(t.clone());
        l.index.insert(t.clone(), id);
        id
    }

    pub fn term(&self, id: Id) -> Option<Term> {
        match id.tag() {
            Tag::Local => self
                .locals
                .read()
                .unwrap()
                .terms
                .get(id.payload() as usize)
                .cloned(),
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
    read_graph_by_id(snap, g, None)
}

#[cfg(test)]
thread_local! {
    static DEADLINE_CHECKS_LEFT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_deadline_checks_left(checks: Option<usize>) {
    DEADLINE_CHECKS_LEFT.set(checks);
}

pub(crate) fn check_deadline(deadline: Option<Instant>) -> sparkles_core::Result<()> {
    #[cfg(test)]
    if DEADLINE_CHECKS_LEFT.with(|left| match left.get() {
        Some(0) => true,
        Some(n) => {
            left.set(Some(n - 1));
            false
        }
        None => false,
    }) {
        return Err(sparkles_core::Error::Timeout);
    }
    if deadline.is_some_and(|limit| Instant::now() >= limit) {
        return Err(sparkles_core::Error::Timeout);
    }
    Ok(())
}

/// Read the graph at a pinned physical id and preserve the store's blank-node labels.
pub(crate) fn read_graph_by_id(snap: &Snapshot, g: Id, deadline: Option<Instant>) -> Result<Graph> {
    check_deadline(deadline)?;
    let mut out = Graph::new();
    snap.scan(Perm::Gspo, &[g.0], |c| {
        check_deadline(deadline)?;
        let mut add = |k: Key| -> sparkles_core::Result<()> {
            let q = Perm::Gspo.to_quad(&k);
            let q = snap.quad_to_terms(&q).ok_or_else(|| {
                sparkles_core::Error::invalid(format!("cannot decode quad {q:?}"))
            })?;
            out.insert(&Triple::new(q.subject, q.predicate, q.object));
            Ok(())
        };
        match c {
            Chunk::Block(b, s, e) => {
                for i in s..e {
                    if i % 256 == 0 {
                        check_deadline(deadline)?;
                    }
                    add(b.key(i))?;
                }
            }
            Chunk::Row(k) => add(k)?,
        }
        Ok(true)
    })?;
    check_deadline(deadline)?;
    Ok(out)
}
