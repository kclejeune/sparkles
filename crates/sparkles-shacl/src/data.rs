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
        Self::new_checked(snap, data_graph, extra, exclude, shapes, || Ok(()))
    }

    pub fn new_checked(
        snap: Arc<Snapshot>,
        data_graph: Option<&str>,
        extra: &[String],
        exclude: &[String],
        shapes: &Shapes,
        check: impl Fn() -> sparkles_core::Result<()>,
    ) -> Result<(DataGraph, Vec<Id>)> {
        check()?;
        let graph = validation::DataGraph::new(snap, data_graph, extra, exclude)?;
        check()?;
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
                check()?;
            }
            ids.push(d.resolve(term, shapes.bnodes_in_store));
            #[cfg(test)]
            observe_term_mapping();
        }
        check()?;
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
        self.subclasses_checked(class, || Ok(()))
    }

    fn subclasses_checked(
        &self,
        class: Id,
        check: impl Fn() -> sparkles_core::Result<()>,
    ) -> Result<Arc<FxHashSet<Id>>> {
        check()?;
        let cached = self.subclasses.read().unwrap().get(&class).cloned();
        if let Some(cached) = cached {
            check()?;
            return Ok(cached);
        }
        let mut set = FxHashSet::default();
        set.insert(class);
        let mut stack = vec![class];
        while let Some(c) = stack.pop() {
            check()?;
            for sub in self.subjects_checked(self.sub_class_of, c, &check)? {
                check()?;
                if set.insert(sub) {
                    stack.push(sub);
                }
            }
        }
        check()?;
        let set = Arc::new(set);
        self.subclasses.write().unwrap().insert(class, set.clone());
        Ok(set)
    }

    /// Is `node` a SHACL instance of `class` (rdf:type/rdfs:subClassOf*)?
    pub fn is_instance(&self, node: Id, class: Id) -> Result<bool> {
        self.is_instance_checked(node, class, || Ok(()))
    }

    pub fn is_instance_checked(
        &self,
        node: Id,
        class: Id,
        check: impl Fn() -> sparkles_core::Result<()>,
    ) -> Result<bool> {
        let types = self.objects_checked(node, self.rdf_type, &check)?;
        if types.is_empty() {
            return Ok(false);
        }
        for ty in &types {
            check()?;
            if *ty == class {
                return Ok(true);
            }
        }
        let subs = self.subclasses_checked(class, &check)?;
        for ty in types {
            check()?;
            if subs.contains(&ty) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// All SHACL instances of `class`.
    pub fn instances(&self, class: Id) -> Result<Vec<Id>> {
        self.instances_checked(class, || Ok(()))
    }

    pub fn instances_checked(
        &self,
        class: Id,
        check: impl Fn() -> sparkles_core::Result<()>,
    ) -> Result<Vec<Id>> {
        let subs = self.subclasses_checked(class, &check)?;
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        let mut classes = Vec::new();
        for &class in subs.iter() {
            check()?;
            classes.push(class);
        }
        classes.sort_unstable();
        check()?;
        for c in classes {
            for s in self.subjects_checked(self.rdf_type, c, &check)? {
                check()?;
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
    snap.scan_checked(
        Perm::Gspo,
        &[g.0],
        || check_deadline(deadline),
        |c| {
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
        },
    )?;
    check_deadline(deadline)?;
    Ok(out)
}

#[cfg(test)]
pub(crate) struct ScopedDeadlineChecks;

#[cfg(test)]
impl ScopedDeadlineChecks {
    pub(crate) fn expire_after(checks: usize) -> Self {
        set_deadline_checks_left(Some(checks));
        Self
    }
}

#[cfg(test)]
impl Drop for ScopedDeadlineChecks {
    fn drop(&mut self) {
        set_deadline_checks_left(None);
    }
}

#[cfg(test)]
pub(crate) fn scan_fixture() -> (
    sparkles_core::store::Store,
    Arc<Snapshot>,
    Arc<Snapshot>,
    Id,
) {
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::store::{Store, StoreOptions};
    use std::fmt::Write;

    let store = Store::in_memory(StoreOptions {
        bulk_threshold: 1,
        ..Default::default()
    });
    let mut nq = String::new();
    for i in 0..4096 {
        writeln!(nq, "<urn:s:{i}> <urn:p> <urn:o:{i}> <urn:shapes> .").unwrap();
    }
    store
        .load(&[Source::from_bytes(nq.into_bytes(), RdfFormat::NQuads, None)])
        .unwrap();
    let before = store.snapshot();
    assert_eq!(before.len(), 4096);
    assert_eq!(before.delta.inserts(), 0);
    let g = before.lookup_iri("urn:shapes").unwrap();
    let mut txn = store.write();
    for key in before.scan_keys(Perm::Gspo, &[g.0]).unwrap() {
        assert!(txn.delete(Perm::Gspo.to_quad(&key)).unwrap());
    }
    txn.commit().unwrap();
    let after = store.snapshot();
    assert_eq!(after.delta.deletes(), 4096);
    assert_eq!(after.count(Perm::Gspo, &[g.0]).unwrap(), 0);
    (store, before, after, g)
}

#[cfg(test)]
mod control_tests {
    use super::*;

    #[test]
    fn pinned_graph_load_expires_inside_deleted_rows() {
        let (_store, before, after, graph) = scan_fixture();
        let deadline = Instant::now().checked_add(std::time::Duration::from_secs(60));
        assert_eq!(
            read_graph_by_id(&before, graph, deadline).unwrap().len(),
            4096
        );
        assert!(
            read_graph_by_id(&after, graph, deadline)
                .unwrap()
                .is_empty()
        );
        let _clock = ScopedDeadlineChecks::expire_after(8);
        let result = read_graph_by_id(&after, graph, deadline);
        assert!(
            matches!(
                result.as_ref().err().and_then(|e| e.downcast_ref()),
                Some(sparkles_core::Error::Timeout)
            ),
            "read finished without a raw-scan timeout: {result:?}"
        );
    }
}

#[cfg(test)]
thread_local! {
    static SETUP_MAPPING: std::cell::RefCell<(usize, Option<Arc<std::sync::atomic::AtomicBool>>)> =
        const { std::cell::RefCell::new((0, None)) };
}

#[cfg(test)]
pub(crate) fn observe_mapping_with(flag: Option<Arc<std::sync::atomic::AtomicBool>>) {
    SETUP_MAPPING.with(|state| state.replace((0, flag)));
}

#[cfg(test)]
pub(crate) fn mapped_terms() -> usize {
    SETUP_MAPPING.with(|state| state.borrow().0)
}

#[cfg(test)]
fn observe_term_mapping() {
    SETUP_MAPPING.with(|state| {
        let mut state = state.borrow_mut();
        state.0 += 1;
        if state.0 == 5
            && let Some(flag) = state.1.take()
        {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    });
}
