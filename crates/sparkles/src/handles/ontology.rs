//! The OWL 2 ontology of a dataset's graphs (spec F21): its axioms, read by the
//! reverse mapping of the OWL 2 Mapping to RDF Graphs, its profile checks, and edits
//! that add and remove axioms through the forward mapping.
//!
//! An open ontology reads the terminology of its graphs eagerly: the triples of the
//! structural vocabulary, the declarations and the annotations, which
//! [`is_terminological`](sparkles_owl::is_terminological) selects, and the class
//! assertions whose class is a class expression. The other assertions stay in the store
//! and are mapped one triple at a time when [`Opened::assertions`] streams them, so a
//! large data graph costs no memory to open. The dataset keeps the last few open
//! ontologies of callers who read every graph, keyed by commit; any commit makes them
//! stale.
//!
//! ```
//! use sparkles::Dataset;
//! use sparkles::handles::ontology::{EditRequest, OntologyRequest};
//! use sparkles::owl::{Axiom, Class, ClassExpression, ObjectProperty};
//!
//! let ds = Dataset::memory();
//! ds.update(
//!     "PREFIX owl: <http://www.w3.org/2002/07/owl#>
//!      PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
//!      INSERT DATA {
//!        <http://ex.org/o> a owl:Ontology .
//!        <http://ex.org/Professor> a owl:Class . <http://ex.org/Course> a owl:Class .
//!        <http://ex.org/teaches> a owl:ObjectProperty .
//!        <http://ex.org/Professor> rdfs:subClassOf
//!          [ a owl:Restriction ; owl:onProperty <http://ex.org/teaches> ;
//!            owl:someValuesFrom <http://ex.org/Course> ] .
//!      }",
//! )?;
//! let req = OntologyRequest::default();
//! let axiom = Axiom::sub_class_of(
//!     Class::new("http://ex.org/Professor"),
//!     ClassExpression::some(
//!         ObjectProperty::new("http://ex.org/teaches"),
//!         Class::new("http://ex.org/Course"),
//!     ),
//! );
//! let o = ds.ontology().open(&req, &Default::default())?;
//! assert!(o.ontology.contains(&axiom));
//!
//! // remove the axiom: its restriction node goes with it
//! let edit = EditRequest { remove: vec![axiom.clone()], ..Default::default() };
//! let done = ds.ontology().edit(&req, &edit, &Default::default())?;
//! assert_eq!(done.removed, 1);
//! assert_eq!(done.deleted, 4);
//! let o = ds.ontology().open(&req, &Default::default())?;
//! assert!(!o.ontology.contains(&axiom));
//! # Ok::<_, sparkles::Error>(())
//! ```

use crate::Dataset;
use crate::commit::{CommitKind, Receipt};
use crate::error::{Error, Result};
use crate::history::{At, HistoryOptions};
use crate::id::Id;
use crate::index::Perm;
use crate::sparql::QueryOptions;
use crate::store::{Chunk, Snapshot};
use oxrdf::{NamedOrBlankNode, Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_owl::profiles::{CheckOptions, Checker, Profile, Report};
use sparkles_owl::read::{self, Mode, ReadOptions};
use sparkles_owl::{Axiom, AxiomType, Entity, EntityKind, vocab, write};
use sparkles_reasoner::inputs::{GraphRef, Import, ImportMode, Inputs};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

/// The most open ontologies a dataset keeps.
pub const CACHE_ENTRIES: usize = 4;

/// The most triples of an open ontology the dataset keeps; a larger terminology is read
/// again by each request.
pub const CACHE_MAX_TRIPLES: usize = 2_000_000;

/// The label prefix of the blank nodes an edit writes, which no stored node has.
const FRESH: &str = "owl-new-";

/// What ontology to open: its graphs, their imports, the mode of the read and the state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OntologyRequest {
    /// the graphs read (the default graph when empty); the first one takes edits
    pub graphs: Vec<GraphRef>,
    /// follow `owl:imports` to graphs of the dataset ([`ImportMode::Dataset`], the
    /// default) or not ([`ImportMode::None`]); fetching is refused
    pub imports: ImportMode,
    pub mode: Mode,
    /// the state read (`None`: the head); edits always start from the head
    pub at: Option<At>,
}

impl Default for OntologyRequest {
    fn default() -> OntologyRequest {
        OntologyRequest {
            graphs: vec![GraphRef::Default],
            imports: ImportMode::Dataset,
            mode: Mode::Lenient,
            at: None,
        }
    }
}

impl OntologyRequest {
    fn validate(&self) -> Result<()> {
        if self.imports == ImportMode::Fetch {
            return Err(Error::invalid(
                "an ontology reads the imports that resolve to graphs of the dataset; imports=fetch is not supported",
            ));
        }
        Ok(())
    }

    /// The graphs named, each once, in the order first named.
    fn graphs(&self) -> Vec<GraphRef> {
        if self.graphs.is_empty() {
            return vec![GraphRef::Default];
        }
        let mut out: Vec<GraphRef> = Vec::new();
        for g in &self.graphs {
            if !out.contains(g) {
                out.push(g.clone());
            }
        }
        out
    }

    fn key(&self, commit: u64) -> String {
        format!(
            "{commit}|{:?}|{:?}|{}",
            self.graphs(),
            self.imports,
            self.mode.as_str()
        )
    }
}

/// An open ontology: its terminology read from one state, through the caller's view.
#[derive(Clone)]
pub struct Opened {
    pub ontology: Arc<read::Ontology>,
    /// the state read, as the caller's view sees it
    pub snapshot: Arc<Snapshot>,
    /// the graphs read: index `n` is graph `n` of [`read::Ontology::graph_of`]. The
    /// requested graphs come first, then those imports resolved to.
    pub graphs: Vec<GraphRef>,
    /// how many of [`graphs`](Self::graphs) the request named
    pub requested: usize,
    pub imports: Vec<Import>,
    pub warnings: Vec<String>,
    /// whether the dataset's kept copy answered
    pub cached: bool,
    inner: Arc<Inner>,
}

/// What the dataset keeps of an open ontology besides the axioms.
struct Inner {
    /// the stored quad of each triple of the ontology
    quads: Vec<[Id; 4]>,
    /// the id of each graph read that has quads
    graph_ids: Vec<Option<Id>>,
    /// the predicates whose triples were read eagerly
    terminological: FxHashSet<Id>,
    /// the main triples of reified axioms whose predicate is not terminological, read
    /// eagerly with their `owl:Axiom` node and left out of the assertion stream
    mains: FxHashSet<[Id; 4]>,
}

/// One kept ontology.
pub struct CacheEntry {
    key: String,
    ontology: Arc<read::Ontology>,
    graphs: Vec<GraphRef>,
    requested: usize,
    imports: Vec<Import>,
    warnings: Vec<String>,
    inner: Arc<Inner>,
}

/// The counts that describe an open ontology.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub header: sparkles_owl::Header,
    /// the axioms of the terminology by type (assertions are counted by
    /// [`Opened::assertions`])
    pub axioms: BTreeMap<AxiomType, usize>,
    /// the declared entities by kind
    pub entities: BTreeMap<EntityKind, usize>,
    pub triples: usize,
    pub unmapped: usize,
    pub warnings: usize,
    pub errors: usize,
}

impl Opened {
    /// The name of graph `n` of the ontology.
    pub fn graph_name(&self, n: u32) -> &str {
        self.graphs.get(n as usize).map_or("", |g| g.as_str())
    }

    /// The counts of the ontology's terminology.
    pub fn summary(&self) -> Summary {
        let o = &self.ontology;
        let mut axioms = BTreeMap::new();
        let mut entities = BTreeMap::new();
        for ax in o.axiom_set() {
            *axioms.entry(ax.axiom_type()).or_insert(0) += 1;
            if let sparkles_owl::AxiomKind::Declaration(e) = &ax.kind {
                *entities.entry(e.kind()).or_insert(0) += 1;
            }
        }
        let errors = o
            .diagnostics
            .iter()
            .filter(|d| d.severity == read::Severity::Error)
            .count();
        Summary {
            header: o.header.clone(),
            axioms,
            entities,
            triples: o.triples.len(),
            unmapped: o.unmapped.len(),
            warnings: o.diagnostics.len() - errors + o.diagnostics_dropped,
            errors,
        }
    }

    /// Stream the assertions that the open read left in the store: each triple of the
    /// graphs read whose predicate is not terminological, mapped by
    /// [`read::Ontology::map_assertion`]. `f` gets the axiom, or `None` for a triple that
    /// maps to no axiom, with the triple and its graph, and returns `false` to stop.
    pub fn assertions(
        &self,
        opts: &QueryOptions,
        mut f: impl FnMut(Option<Axiom>, &Triple, u32) -> Result<bool>,
    ) -> Result<()> {
        let opts = &*crate::overview::one_deadline(opts)?;
        let snap = &self.snapshot;
        let rdf_type = snap.lookup_iri(vocab::RDF_TYPE);
        let mut n = 0usize;
        let stopped = std::cell::Cell::new(false);
        for (g, id) in self.inner.graph_ids.iter().enumerate() {
            let Some(id) = id else { continue };
            let mut each = |q: &[Id; 4]| -> Result<bool> {
                n += 1;
                if n.is_multiple_of(4096) {
                    check(opts)?;
                }
                if self.inner.terminological.contains(&q[1]) || self.inner.mains.contains(q) {
                    return Ok(true);
                }
                let Some(t) = triple_of(snap, q) else {
                    return Ok(true);
                };
                // class assertions with a structural type or a class expression were read
                if Some(q[1]) == rdf_type && read::is_terminological(&t, &|_| false) {
                    return Ok(true);
                }
                let ax = self.ontology.map_assertion(&t);
                let more = f(ax, &t, g as u32)?;
                stopped.set(!more);
                Ok(more)
            };
            snap.scan(Perm::Gspo, &[id.0], |c| {
                match c {
                    Chunk::Block(b, s, e) => {
                        for i in s..e {
                            if !each(&Perm::Gspo.to_quad(&b.key(i)))? {
                                return Ok(false);
                            }
                        }
                    }
                    Chunk::Row(k) => return each(&Perm::Gspo.to_quad(&k)),
                }
                Ok(true)
            })?;
            if stopped.get() {
                break;
            }
        }
        Ok(())
    }
}

/// What a check covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckRequest {
    pub profiles: Vec<Profile>,
    /// also check the assertions the store holds, one triple at a time
    pub assertions: bool,
    /// the most violations each report lists
    pub max_violations: usize,
}

impl Default for CheckRequest {
    fn default() -> CheckRequest {
        CheckRequest {
            profiles: vec![Profile::Dl],
            assertions: true,
            max_violations: CheckOptions::default().max_violations,
        }
    }
}

/// The axioms an edit adds and removes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditRequest {
    pub add: Vec<Axiom>,
    pub remove: Vec<Axiom>,
    /// also add the declarations of the entities an added axiom uses that the
    /// ontology does not declare (`None`: true)
    pub declare: Option<bool>,
    /// the graph that takes the added triples (`None`: the first graph of the request)
    pub graph: Option<GraphRef>,
    /// the commit message
    pub message: Option<String>,
}

/// What an edit did.
#[derive(Clone, Debug)]
pub struct EditOutcome {
    pub receipt: Receipt,
    /// the axioms added, declarations included
    pub added: usize,
    /// the axioms asked to be added that the ontology already had
    pub present: usize,
    pub removed: usize,
    /// the axioms asked to be removed that the ontology does not have
    pub absent: Vec<Axiom>,
    /// the declarations added for the entities the added axioms use
    pub declared: Vec<Entity>,
    pub inserted: usize,
    pub deleted: usize,
    /// triples of removed axioms kept because another axiom or triple uses them
    pub kept: usize,
}

/// The ontology handle of a dataset.
#[derive(Clone)]
pub struct Ontologies {
    pub(crate) ds: Dataset,
}

impl Dataset {
    /// The OWL 2 ontology of this dataset's graphs (spec F21).
    pub fn ontology(&self) -> Ontologies {
        Ontologies { ds: self.clone() }
    }
}

fn check(opts: &QueryOptions) -> Result<()> {
    if opts
        .cancel
        .as_ref()
        .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
    {
        return Err(Error::Cancelled);
    }
    if let Some(c) = &opts.cancellation {
        c.check()?;
    }
    if opts.deadline.is_some_and(|d| Instant::now() >= d) {
        return Err(Error::Timeout);
    }
    Ok(())
}

fn triple_of(snap: &Snapshot, q: &[Id; 4]) -> Option<Triple> {
    let s = match snap.term(q[0])? {
        Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
        Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
        _ => return None,
    };
    let Term::NamedNode(p) = snap.term(q[1])? else {
        return None;
    };
    Some(Triple::new(s, p, snap.term(q[2])?))
}

impl Ontologies {
    /// The state `req.at` names through the caller's view, with the options anchored to
    /// one deadline.
    fn state(
        &self,
        req: &OntologyRequest,
        opts: &QueryOptions,
    ) -> Result<(Arc<Snapshot>, QueryOptions)> {
        let defaults = self.ds.with_query_defaults(opts);
        let opts = crate::overview::one_deadline(&defaults)?.into_owned();
        let snap = match &req.at {
            None => self.ds.store().snapshot(),
            Some(at) => {
                let o = HistoryOptions {
                    cancel: opts.cancel.clone(),
                    deadline: opts.deadline,
                };
                self.ds.store().snapshot_at(at, &o)?.0
            }
        };
        Ok((snap, opts))
    }

    /// `opts` with the dataset's defaults and one deadline, anchored now: pass the
    /// answer to every stage of one request (open, check, listing), so that a relative
    /// timeout covers them all.
    pub fn anchored(&self, opts: &QueryOptions) -> Result<QueryOptions> {
        let defaults = self.ds.with_query_defaults(opts);
        Ok(crate::overview::one_deadline(&defaults)?.into_owned())
    }

    /// Open the ontology `req` names, as the caller of `opts` may read it.
    pub fn open(&self, req: &OntologyRequest, opts: &QueryOptions) -> Result<Opened> {
        req.validate()?;
        let (snap, opts) = self.state(req, opts)?;
        self.open_at(&snap, req, &opts)
    }

    /// [`open`](Self::open) at a state the caller has.
    pub fn open_at(
        &self,
        snap: &Arc<Snapshot>,
        req: &OntologyRequest,
        opts: &QueryOptions,
    ) -> Result<Opened> {
        req.validate()?;
        let access = opts.graphs.as_ref().map(|a| a.bound(snap));
        let view = match &access {
            Some(a) => a.masked(snap)?,
            None => snap.clone(),
        };
        let keep = access.is_none() && snap.cacheable;
        let key = req.key(snap.commit);
        if keep {
            let mut cache = self.ds.state().ontology_cache.lock();
            if let Some(i) = cache.iter().position(|e| e.key == key) {
                let e = cache.remove(i);
                let opened = Opened {
                    ontology: e.ontology.clone(),
                    snapshot: view,
                    graphs: e.graphs.clone(),
                    requested: e.requested,
                    imports: e.imports.clone(),
                    warnings: e.warnings.clone(),
                    cached: true,
                    inner: e.inner.clone(),
                };
                cache.push(e);
                return Ok(opened);
            }
        }
        let readable = |g: Id| access.as_ref().is_none_or(|a| a.readable_id(snap, g));
        let inputs = Inputs {
            data_graphs: req.graphs(),
            ontology_graphs: Vec::new(),
            imports: req.imports,
            location_mapping: Default::default(),
        };
        let resolved = sparkles_reasoner::inputs::resolve_readable(&view, &inputs, &readable);
        let graphs = resolved.graphs.clone();
        let requested = req.graphs().len();
        let graph_ids: Vec<Option<Id>> = graphs
            .iter()
            .map(|g| g.id(&view).filter(|id| readable(*id)))
            .collect();
        let (ontology, inner) = read_store(&view, &graph_ids, requested, req.mode, opts)?;
        check(opts)?;
        let ontology = Arc::new(ontology);
        let inner = Arc::new(inner);
        if keep && ontology.triples.len() <= CACHE_MAX_TRIPLES {
            let mut cache = self.ds.state().ontology_cache.lock();
            cache.retain(|e| e.key != key);
            if cache.len() >= CACHE_ENTRIES {
                cache.remove(0);
            }
            cache.push(CacheEntry {
                key,
                ontology: ontology.clone(),
                graphs: graphs.clone(),
                requested,
                imports: resolved.imports.clone(),
                warnings: resolved.warnings.clone(),
                inner: inner.clone(),
            });
        }
        Ok(Opened {
            ontology,
            snapshot: view,
            graphs,
            requested,
            imports: resolved.imports,
            warnings: resolved.warnings,
            cached: false,
            inner,
        })
    }

    /// Check the ontology `req` names against `check.profiles`: its terminology, and its
    /// assertions when `check.assertions` is set.
    pub fn check(
        &self,
        req: &OntologyRequest,
        check: &CheckRequest,
        opts: &QueryOptions,
    ) -> Result<Vec<Report>> {
        let opts = self.anchored(opts)?;
        let o = self.open(req, &opts)?;
        check_opened(&o, check, &opts)
    }

    /// Remove an entity (spec F21 §5.3): its declarations, the axioms of the
    /// terminology that mention it, and the assertions about it that the store holds, in
    /// one commit. The answer lists them under `removed`; it is `0` for an IRI the
    /// ontology does not mention.
    pub fn remove_entity(
        &self,
        req: &OntologyRequest,
        iri: &str,
        opts: &QueryOptions,
    ) -> Result<EditOutcome> {
        self.edit_with(req, &EditRequest::default(), Some(iri), opts)
    }

    /// Add and remove axioms in one commit. The ontology is read from the head the
    /// write starts from, under the writer lock, so no commit comes between the read
    /// and the write. An added axiom goes to `edit.graph` (the first graph of `req`),
    /// with the declarations of the entities it uses that the ontology lacks. A removed
    /// axiom takes its triples with it, except those that another axiom of the ontology
    /// or another triple of the graph still uses. `opts.write` carries the commit's
    /// options: the caller's graphs, a dry run, a precondition and the author.
    pub fn edit(
        &self,
        req: &OntologyRequest,
        edit: &EditRequest,
        opts: &QueryOptions,
    ) -> Result<EditOutcome> {
        self.edit_with(req, edit, None, opts)
    }

    /// [`edit`](Self::edit), also removing what mentions `entity`, selected from the
    /// head under the writer lock.
    fn edit_with(
        &self,
        req: &OntologyRequest,
        edit: &EditRequest,
        entity: Option<&str>,
        opts: &QueryOptions,
    ) -> Result<EditOutcome> {
        req.validate()?;
        if req.at.is_some() {
            return Err(Error::invalid(
                "an edit starts from the head: leave out 'at'",
            ));
        }
        for ax in edit.add.iter().chain(&edit.remove) {
            sparkles_owl::validate::axiom(ax).map_err(|e| Error::invalid(e.0))?;
        }
        let opts = self.anchored(opts)?;
        let store = self.ds.store();
        let mut wopts = opts.write.clone();
        if wopts.graphs.is_none() {
            wopts.graphs = opts.graphs.clone();
        }
        if let Some(m) = &edit.message {
            wopts.message = Some(Arc::from(m.as_str()));
        }
        if wopts.deadline.is_none() {
            wopts.deadline = opts.deadline;
        }
        if wopts.cancel.is_none() {
            wopts.cancel = opts.cancel.clone();
        }
        let mut txn = store.try_write_with(CommitKind::Update, wopts)?;
        let head = txn.base().clone();
        let o = self.open_at(&head, req, &opts)?;
        let target = edit
            .graph
            .clone()
            .unwrap_or_else(|| req.graphs()[0].clone());
        let target_id = match &target {
            GraphRef::Default => Id::DEFAULT_GRAPH,
            GraphRef::Named(g) => txn.intern(&Term::NamedNode(
                oxrdf::NamedNode::new(g.as_str())
                    .map_err(|e| Error::invalid(format!("graph <{g}>: {e}")))?,
            ))?,
        };
        let mut outcome = Plan::default();
        // removals edit the graphs the request names and the target it names (§5.4):
        // an axiom read from an imported graph is removed there only when the edit
        // names that graph
        let named = req.graphs();
        let scope: BTreeSet<u32> = o
            .graphs
            .iter()
            .enumerate()
            .filter(|(_, g)| named.contains(g) || edit.graph.as_ref() == Some(*g))
            .map(|(n, _)| n as u32)
            .collect();
        let scope_ids: FxHashSet<Id> = scope
            .iter()
            .filter_map(|n| o.inner.graph_ids.get(*n as usize).copied().flatten())
            .collect();
        let all_graphs: BTreeSet<u32> = (0..o.graphs.len() as u32).collect();
        let imported = |g: u32| {
            Error::invalid(format!(
                "the axiom is read from the imported graph {}: name that graph as the edit's \
                 graph to change it there",
                o.graph_name(g)
            ))
        };
        let mut removal: Vec<Axiom> = edit.remove.clone();
        if let Some(iri) = entity {
            removal.extend(
                o.ontology
                    .axioms
                    .iter()
                    .filter(|(a, origins)| {
                        a.mentions(iri) && origins.iter().any(|x| scope.contains(&x.graph))
                    })
                    .map(|(a, _)| a.clone()),
            );
            o.assertions(&opts, |ax, _, g| {
                if let Some(ax) = ax
                    && scope.contains(&g)
                    && ax.mentions(iri)
                {
                    removal.push(ax);
                }
                Ok(true)
            })?;
        }
        // removals first: an axiom removed and added again keeps its triples
        let mut deleted: Vec<[Id; 4]> = Vec::new();
        let mut users: FxHashMap<usize, usize> = FxHashMap::default();
        for origins in o.ontology.axioms.values() {
            for origin in origins {
                for i in &origin.triples {
                    *users.entry(*i).or_insert(0) += 1;
                }
            }
        }
        let removing: BTreeSet<&Axiom> = removal.iter().collect();
        let mut drop: BTreeSet<usize> = BTreeSet::new();
        let mut mains: FxHashSet<usize> = FxHashSet::default();
        // each normal form once, with whether it was found
        let mut done: BTreeMap<Axiom, bool> = BTreeMap::new();
        for ax in &removing {
            let mut found = false;
            for form in ax.normal_forms() {
                if let Some(hit) = done.get(&form) {
                    found |= *hit;
                    continue;
                }
                let hit = match o.ontology.axioms.get(&form) {
                    Some(origins) => {
                        let mut here = false;
                        for origin in origins.iter().filter(|x| scope.contains(&x.graph)) {
                            here = true;
                            mains.extend(origin.main);
                            for i in &origin.triples {
                                let u = users.get_mut(i).expect("counted");
                                *u -= 1;
                                if *u == 0 {
                                    drop.insert(*i);
                                }
                            }
                        }
                        if !here && entity.is_none() {
                            return Err(imported(origins.first().map_or(0, |x| x.graph)));
                        }
                        here
                    }
                    None => match assertion_quads(&o, &form, &txn, &scope)? {
                        Some(qs) => {
                            deleted.extend(qs);
                            true
                        }
                        None => {
                            if entity.is_none()
                                && let Some(qs) = assertion_quads(&o, &form, &txn, &all_graphs)?
                            {
                                let g = o
                                    .inner
                                    .graph_ids
                                    .iter()
                                    .position(|id| *id == Some(qs[0][3]))
                                    .unwrap_or(0);
                                return Err(imported(g as u32));
                            }
                            false
                        }
                    },
                };
                done.insert(form, hit);
                found |= hit;
            }
            if found {
                outcome.removed += 1;
            } else {
                outcome.absent.push((*ax).clone());
            }
        }
        // a dropped triple stays when a triple outside the dropped set still points at
        // its blank node subject, unless it is the main triple of a removed axiom
        let kept_nodes = referenced_elsewhere(&o, &drop)?;
        for i in drop {
            let q = o.inner.quads[i];
            if kept_nodes.contains(&q[0]) && !mains.contains(&i) {
                outcome.kept += 1;
                continue;
            }
            // an axiom encoded partly in an imported graph changes there only when the
            // edit names that graph
            if !scope_ids.contains(&q[3]) {
                let g = o
                    .inner
                    .graph_ids
                    .iter()
                    .position(|id| *id == Some(q[3]))
                    .unwrap_or(0);
                return Err(imported(g as u32));
            }
            deleted.push(q);
        }
        let mut count_deleted = 0;
        for q in deleted {
            check(&opts)?;
            if txn.delete(q)? {
                count_deleted += 1;
            }
        }
        // additions
        let declare = edit.declare.unwrap_or(true);
        let mut adding: Vec<Axiom> = Vec::new();
        let mut seen: BTreeSet<&Axiom> = BTreeSet::new();
        for ax in &edit.add {
            if !seen.insert(ax) {
                continue;
            }
            // present after the removals above: a form this edit removed is not
            let mut present = true;
            for form in ax.normal_forms() {
                let kept =
                    if done.get(&form) == Some(&true) {
                        // removed here, but it may still be read from a graph outside the
                        // scope, or as an assertion this transaction left
                        o.ontology.axioms.get(&form).is_some_and(|origins| {
                            origins.iter().any(|x| !scope.contains(&x.graph))
                        }) || assertion_quads(&o, &form, &txn, &all_graphs)?.is_some()
                    } else {
                        o.ontology.contains(&form)
                            || assertion_quads(&o, &form, &txn, &all_graphs)?.is_some()
                    };
                if !kept {
                    present = false;
                    break;
                }
            }
            if present {
                outcome.present += 1;
                continue;
            }
            adding.push(ax.clone());
        }
        if declare {
            let declared: FxHashSet<&Entity> = o
                .ontology
                .axiom_set()
                .filter_map(|a| match &a.kind {
                    sparkles_owl::AxiomKind::Declaration(e) => Some(e),
                    _ => None,
                })
                .collect();
            let mut new: BTreeSet<Entity> = BTreeSet::new();
            for ax in &adding {
                if matches!(ax.kind, sparkles_owl::AxiomKind::Declaration(_)) {
                    continue;
                }
                for e in ax.signature() {
                    if e.is_builtin()
                        || declared.contains(&e)
                        || e.kind() == EntityKind::NamedIndividual
                    {
                        continue;
                    }
                    new.insert(e);
                }
            }
            for ax in &adding {
                if let sparkles_owl::AxiomKind::Declaration(e) = &ax.kind {
                    new.remove(e);
                }
            }
            for e in new {
                outcome.declared.push(e.clone());
                adding.push(Axiom::declaration(e));
            }
        }
        let mut n = 0u64;
        let mut fresh = move || {
            n += 1;
            oxrdf::BlankNode::new_unchecked(format!("{FRESH}{n}"))
        };
        let mut labels: FxHashMap<String, Id> = FxHashMap::default();
        let mut inserted = 0;
        for ax in &adding {
            check(&opts)?;
            for t in write::axiom_triples(ax, &mut fresh) {
                let s = subject_term(&t.subject);
                let q = [
                    encode(&mut txn, &s, &mut labels)?,
                    encode(&mut txn, &Term::NamedNode(t.predicate.clone()), &mut labels)?,
                    encode(&mut txn, &t.object, &mut labels)?,
                    target_id,
                ];
                if txn.insert(q)? {
                    inserted += 1;
                }
            }
        }
        let added = adding.len();
        let receipt = txn.commit()?;
        Ok(EditOutcome {
            receipt,
            added,
            present: outcome.present,
            removed: outcome.removed,
            absent: outcome.absent,
            declared: outcome.declared,
            inserted,
            deleted: count_deleted,
            kept: outcome.kept,
        })
    }
}

#[derive(Default)]
struct Plan {
    present: usize,
    removed: usize,
    absent: Vec<Axiom>,
    declared: Vec<Entity>,
    kept: usize,
}

/// The id of a term an edit writes: a fresh blank node of the forward mapping, or a
/// stored term.
fn encode(
    txn: &mut crate::store::WriteTxn<'_>,
    t: &Term,
    labels: &mut FxHashMap<String, Id>,
) -> Result<Id> {
    if let Term::BlankNode(b) = t
        && b.as_str().starts_with(FRESH)
    {
        if let Some(id) = labels.get(b.as_str()) {
            return Ok(*id);
        }
        let id = txn.new_bnode();
        labels.insert(b.as_str().to_string(), id);
        return Ok(id);
    }
    txn.intern(t).map_err(|e| match e {
        Error::Invalid(m) if matches!(t, Term::BlankNode(_)) => Error::invalid(format!(
            "an anonymous individual of an added axiom must name a stored blank node: {m}"
        )),
        e => e,
    })
}

/// The stored quads of an assertion that the open read left in the store, in every
/// graph read; `None` when it has none. Such an assertion is one triple: the one the
/// forward mapping writes, or for an equality or inequality of two individuals, that
/// triple in either direction.
fn assertion_quads(
    o: &Opened,
    ax: &Axiom,
    txn: &crate::store::WriteTxn<'_>,
    graphs: &BTreeSet<u32>,
) -> Result<Option<Vec<[Id; 4]>>> {
    use sparkles_owl::AxiomKind as K;
    if !ax.annotations.is_empty() {
        return Ok(None);
    }
    let mut fresh = || oxrdf::BlankNode::new_unchecked(format!("{FRESH}x"));
    let mut triples = write::axiom_triples(ax, &mut fresh);
    if triples.len() != 1 {
        return Ok(None);
    }
    if let K::SameIndividual(_) | K::DifferentIndividuals(_) = &ax.kind {
        let t = triples[0].clone();
        if let Some(s) = term_subject(&t.object) {
            triples.push(Triple::new(s, t.predicate, subject_term(&t.subject)));
        }
    }
    let snap = &o.snapshot;
    let mut out = Vec::new();
    for t in &triples {
        let (Some(s), Some(p), Some(obj)) = (
            snap.lookup_term(&subject_term(&t.subject)),
            snap.lookup_iri(t.predicate.as_str()),
            snap.lookup_term(&t.object),
        ) else {
            continue;
        };
        if o.ontology.map_assertion(t).as_ref() != Some(ax) {
            continue;
        }
        for (n, g) in o.inner.graph_ids.iter().enumerate() {
            let Some(g) = g else { continue };
            if !graphs.contains(&(n as u32)) {
                continue;
            }
            let q = [s, p, obj, *g];
            // through the caller's view, so a protected triple is absent, and as this
            // transaction leaves it
            if snap.contains(&q)? && txn.contains(&q)? {
                out.push(q);
            }
        }
    }
    Ok((!out.is_empty()).then_some(out))
}

fn subject_term(s: &NamedOrBlankNode) -> Term {
    match s {
        NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n.clone()),
        NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.clone()),
    }
}

fn term_subject(t: &Term) -> Option<NamedOrBlankNode> {
    match t {
        Term::NamedNode(n) => Some(NamedOrBlankNode::NamedNode(n.clone())),
        Term::BlankNode(b) => Some(NamedOrBlankNode::BlankNode(b.clone())),
        _ => None,
    }
}

/// The blank nodes among the subjects of `drop` that a triple outside `drop` uses as
/// its object, in the graphs read: their structures stay.
fn referenced_elsewhere(o: &Opened, drop: &BTreeSet<usize>) -> Result<FxHashSet<Id>> {
    let snap = &o.snapshot;
    let mut nodes: FxHashSet<Id> = FxHashSet::default();
    for i in drop {
        let q = o.inner.quads[*i];
        if q[0].tag() == crate::id::Tag::BNode {
            nodes.insert(q[0]);
        }
    }
    let dropped: FxHashSet<[Id; 4]> = drop.iter().map(|i| o.inner.quads[*i]).collect();
    let read: FxHashSet<Id> = o.inner.graph_ids.iter().flatten().copied().collect();
    let mut kept = FxHashSet::default();
    for n in &nodes {
        for k in snap.scan_keys(Perm::Osp, &[n.0])? {
            let q = Perm::Osp.to_quad(&k);
            if read.contains(&q[3]) && !dropped.contains(&q) {
                kept.insert(*n);
            }
        }
    }
    // a kept node keeps the nodes its own triples lead to
    let mut stack: Vec<Id> = kept.iter().copied().collect();
    while let Some(n) = stack.pop() {
        for q in &dropped {
            if q[0] == n && nodes.contains(&q[2]) && kept.insert(q[2]) {
                stack.push(q[2]);
            }
        }
    }
    Ok(kept)
}

/// Check an open ontology.
pub fn check_opened(o: &Opened, req: &CheckRequest, opts: &QueryOptions) -> Result<Vec<Report>> {
    let opts = &*crate::overview::one_deadline(opts)?;
    let copts = CheckOptions {
        max_violations: req.max_violations,
    };
    let mut c = Checker::new(&o.ontology, &req.profiles, copts);
    for (ax, origins) in &o.ontology.axioms {
        c.axiom(ax, origins.first().map(|x| x.graph));
    }
    if req.assertions {
        let mut n = 0usize;
        o.assertions(opts, |ax, t, g| {
            n += 1;
            if n.is_multiple_of(4096) {
                check(opts)?;
            }
            match ax {
                Some(ax) => c.axiom(&ax, Some(g)),
                None => c.unmapped(t, Some(g)),
            }
            Ok(true)
        })?;
    }
    Ok(c.finish())
}

/// Read the terminology of the graphs `graph_ids` (`None`: a graph without quads or one
/// the caller cannot read) from `snap`.
fn read_store(
    snap: &Arc<Snapshot>,
    graph_ids: &[Option<Id>],
    requested: usize,
    mode: Mode,
    opts: &QueryOptions,
) -> Result<(read::Ontology, Inner)> {
    let graph_of_id: FxHashMap<Id, u32> = graph_ids
        .iter()
        .enumerate()
        .filter_map(|(n, g)| g.map(|g| (g, n as u32)))
        .collect();
    let mut c = Collect {
        snap,
        graphs: &graph_of_id,
        quads: Vec::new(),
        seen: FxHashSet::default(),
        opts,
    };
    let mut terminological: FxHashSet<Id> = FxHashSet::default();
    let predicates = vocab::STRUCTURAL_PREDICATES
        .iter()
        .chain(vocab::FACETS.iter())
        .chain(vocab::BUILTIN_ANNOTATION_PROPERTIES.iter());
    for p in predicates {
        if let Some(id) = snap.lookup_iri(p)
            && terminological.insert(id)
        {
            check(opts)?;
            c.prefix(&[id.0])?;
        }
    }
    if let Some(rdf_type) = snap.lookup_iri(vocab::RDF_TYPE) {
        let before = c.quads.len();
        for t in vocab::STRUCTURAL_TYPES {
            if let Some(o) = snap.lookup_iri(t) {
                check(opts)?;
                c.prefix(&[rdf_type.0, o.0])?;
            }
        }
        // the annotation properties declared in the graphs read
        let ap = snap.lookup_iri(vocab::OWL_ANNOTATION_PROPERTY);
        let declared: Vec<Id> = c.quads[before..]
            .iter()
            .filter(|q| Some(q[2]) == ap)
            .map(|q| q[0])
            .collect();
        for p in declared {
            if terminological.insert(p) {
                check(opts)?;
                c.prefix(&[p.0])?;
            }
        }
        // class assertions whose class is a class expression: `x rdf:type _:c` for each
        // blank node subject read so far
        let bnodes: BTreeSet<Id> = c
            .quads
            .iter()
            .filter(|q| q[0].tag() == crate::id::Tag::BNode)
            .map(|q| q[0])
            .collect();
        for b in bnodes {
            check(opts)?;
            c.prefix(&[rdf_type.0, b.0])?;
        }
    }
    // the main triples of reified axioms whose predicate was not read: an annotated
    // assertion keeps its main triple outside the terminology's predicates
    let mut mains: FxHashSet<[Id; 4]> = FxHashSet::default();
    let ids = |p: &str| snap.lookup_iri(p);
    if let (Some(src), Some(prop), Some(tgt)) = (
        ids(vocab::OWL_ANNOTATED_SOURCE),
        ids(vocab::OWL_ANNOTATED_PROPERTY),
        ids(vocab::OWL_ANNOTATED_TARGET),
    ) {
        let mut parts: FxHashMap<(Id, Id), [Option<Id>; 3]> = FxHashMap::default();
        for q in &c.quads {
            let slot = if q[1] == src {
                0
            } else if q[1] == prop {
                1
            } else if q[1] == tgt {
                2
            } else {
                continue;
            };
            parts.entry((q[0], q[3])).or_default()[slot] = Some(q[2]);
        }
        let graphs: Vec<Id> = graph_of_id.keys().copied().collect();
        for (_, part) in parts {
            let [Some(s), Some(p), Some(o)] = part else {
                continue;
            };
            check(opts)?;
            for g in &graphs {
                let q = [s, p, o, *g];
                if !terminological.contains(&p) && snap.contains(&q)? && c.seen.insert(q) {
                    c.quads.push(q);
                    mains.insert(q);
                }
            }
        }
    }
    let quads = c.quads;
    finish(
        snap,
        quads,
        &graph_of_id,
        graph_ids.len(),
        requested,
        mode,
        (terminological, mains),
    )
}

/// The quads of the graphs read that POS prefixes select, each once.
struct Collect<'a> {
    snap: &'a Snapshot,
    graphs: &'a FxHashMap<Id, u32>,
    quads: Vec<[Id; 4]>,
    seen: FxHashSet<[Id; 4]>,
    opts: &'a QueryOptions,
}

impl Collect<'_> {
    fn prefix(&mut self, prefix: &[u64]) -> Result<()> {
        let mut n = 0usize;
        let (graphs, seen, quads, opts) = (self.graphs, &mut self.seen, &mut self.quads, self.opts);
        let mut each = |q: [Id; 4]| -> Result<()> {
            n += 1;
            if n.is_multiple_of(4096) {
                check(opts)?;
            }
            if graphs.contains_key(&q[3]) && seen.insert(q) {
                quads.push(q);
            }
            Ok(())
        };
        self.snap.scan(Perm::Pos, prefix, |c| {
            match c {
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        each(Perm::Pos.to_quad(&b.key(i)))?;
                    }
                }
                Chunk::Row(k) => each(Perm::Pos.to_quad(&k))?,
            }
            Ok(true)
        })
    }
}

fn finish(
    snap: &Snapshot,
    quads: Vec<[Id; 4]>,
    graph_of_id: &FxHashMap<Id, u32>,
    graphs: usize,
    requested: usize,
    mode: Mode,
    (terminological, mains): (FxHashSet<Id>, FxHashSet<[Id; 4]>),
) -> Result<(read::Ontology, Inner)> {
    let mut triples = Vec::with_capacity(quads.len());
    let mut graph_of = Vec::with_capacity(quads.len());
    let mut kept = Vec::with_capacity(quads.len());
    for q in quads {
        if let Some(t) = triple_of(snap, &q) {
            triples.push(t);
            graph_of.push(graph_of_id[&q[3]]);
            kept.push(q);
        }
    }
    let primary: Vec<u32> = (0..requested.min(graphs) as u32).collect();
    let o = read::read(
        triples,
        graph_of,
        &primary,
        ReadOptions {
            mode,
            terminology_only: true,
            max_diagnostics: 0,
        },
    );
    let graph_ids = {
        let mut ids = vec![None; graphs];
        for (id, n) in graph_of_id {
            ids[*n as usize] = Some(*id);
        }
        ids
    };
    Ok((
        o,
        Inner {
            quads: kept,
            graph_ids,
            terminological,
            mains,
        },
    ))
}

/// The most changed quads an ontology diff reads from the store's diff.
pub const DIFF_MAX_QUADS: u64 = 10_000_000;

/// One axiom of an ontology diff, with the graph it is in.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AxiomChange {
    pub axiom: Axiom,
    pub graph: String,
}

/// The axioms that differ between two states of an ontology (spec F21 Phase 3).
#[derive(Clone, Debug)]
pub struct OntologyDiff {
    /// the commits of the two states
    pub from: u64,
    pub to: u64,
    /// the axioms of `to` that `from` lacks, in order
    pub added: Vec<AxiomChange>,
    /// the axioms of `from` that `to` lacks, in order
    pub removed: Vec<AxiomChange>,
    /// the quads of the store's diff in the graphs the two reads cover
    pub changed_quads: u64,
    /// how the store's diff was computed
    pub method: crate::store::DiffMethod,
}

impl Ontologies {
    /// The axioms that differ between the states `from` and `to` of the ontology `req`
    /// names (its `at` is ignored), as the caller of `opts` may read them. The
    /// terminology of each state is read as [`open`](Self::open) reads it and the two
    /// sets of axioms are compared. With `assertions`, the assertions that the store's
    /// diff of triples ([`crate::store::Store::diff`]) adds or removes in the graphs read
    /// are mapped one triple at a time, as [`Opened::assertions`] maps them, so an
    /// assertion that no changed triple carries is not read. When nothing changed in the
    /// graphs read, nothing is opened and the diff is empty.
    pub fn diff(
        &self,
        req: &OntologyRequest,
        from: &At,
        to: &At,
        assertions: bool,
        opts: &QueryOptions,
    ) -> Result<OntologyDiff> {
        req.validate()?;
        let opts = self.anchored(opts)?;
        let store = self.ds.store();
        let hist = HistoryOptions {
            cancel: opts.cancel.clone(),
            deadline: opts.deadline,
        };
        let (a, _) = store.snapshot_at(from, &hist)?;
        let (b, _) = store.snapshot_at(to, &hist)?;
        let triples = store.diff(
            from,
            to,
            &crate::store::DiffOptions {
                max_quads: DIFF_MAX_QUADS,
                cancel: opts.cancel.clone(),
                deadline: opts.deadline,
                graphs: opts.graphs.clone(),
                ..Default::default()
            },
        )?;
        let mut req = req.clone();
        req.at = None;
        let mut out = OntologyDiff {
            from: a.commit,
            to: b.commit,
            added: Vec::new(),
            removed: Vec::new(),
            changed_quads: 0,
            method: triples.method,
        };
        // the graphs a read of either state covers: the requested ones and their imports
        let graphs_of =
            |snap: &Arc<Snapshot>| -> Result<Opened> { self.open_at(snap, &req, &opts) };
        let changed_in = |o: &Opened, g: &oxrdf::GraphName| {
            o.graphs.iter().any(|r| match (r, g) {
                (GraphRef::Default, oxrdf::GraphName::DefaultGraph) => true,
                (GraphRef::Named(n), oxrdf::GraphName::NamedNode(m)) => n == m.as_str(),
                _ => false,
            })
        };
        if triples.is_empty() {
            return Ok(out);
        }
        let oa = graphs_of(&a)?;
        let ob = graphs_of(&b)?;
        out.changed_quads = triples
            .iter()
            .filter(|(_, q)| changed_in(&oa, &q.graph_name) || changed_in(&ob, &q.graph_name))
            .count() as u64;
        if out.changed_quads == 0 {
            return Ok(out);
        }
        check(&opts)?;
        let graph = |o: &Opened, ax: &Axiom| -> String {
            o.ontology
                .axioms
                .get(ax)
                .and_then(|v| v.first())
                .map_or_else(String::new, |x| o.graph_name(x.graph).to_string())
        };
        for ax in ob.ontology.axiom_set() {
            if !oa.ontology.contains(ax) {
                out.added.push(AxiomChange {
                    axiom: ax.clone(),
                    graph: graph(&ob, ax),
                });
            }
        }
        for ax in oa.ontology.axiom_set() {
            if !ob.ontology.contains(ax) {
                out.removed.push(AxiomChange {
                    axiom: ax.clone(),
                    graph: graph(&oa, ax),
                });
            }
        }
        if assertions {
            let (ka, kb) = (annotated_kinds(&oa), annotated_kinds(&ob));
            let mut added = BTreeSet::new();
            let mut removed = BTreeSet::new();
            for (op, q) in triples.iter() {
                let (o, kinds, set) = match op {
                    crate::store::DiffOp::Add => (&ob, &kb, &mut added),
                    crate::store::DiffOp::Remove => (&oa, &ka, &mut removed),
                };
                if !changed_in(o, &q.graph_name) {
                    continue;
                }
                let t = Triple::new(q.subject, q.predicate, q.object);
                if let Some(ax) = assertion_of(o, kinds, &t) {
                    let g = match &q.graph_name {
                        oxrdf::GraphName::NamedNode(n) => n.as_str().to_string(),
                        _ => "default".to_string(),
                    };
                    set.insert(AxiomChange {
                        axiom: ax,
                        graph: g,
                    });
                }
            }
            // an annotated assertion that lost or gained its annotations keeps its
            // triple, which the store's diff then does not name: the plain assertion
            // appears or disappears with them
            for (ours, theirs, kinds, snap, set) in [
                (&out.removed, &ob, &kb, &ob.snapshot, &mut added),
                (&out.added, &oa, &ka, &oa.snapshot, &mut removed),
            ] {
                for c in ours {
                    if c.axiom.annotations.is_empty() || !c.axiom.axiom_type().is_assertion() {
                        continue;
                    }
                    let plain = c.axiom.without_annotations();
                    if theirs.ontology.contains(&plain) || kinds.contains(&plain.kind) {
                        continue;
                    }
                    if holds_main(snap, theirs, &plain) {
                        set.insert(AxiomChange {
                            axiom: plain,
                            graph: c.graph.clone(),
                        });
                    }
                }
            }
            out.added.extend(added);
            out.removed.extend(removed);
        }
        Ok(out)
    }
}

/// The kinds of the annotated assertions of the terminology, whose main triples a read
/// of the assertions skips.
fn annotated_kinds(o: &Opened) -> FxHashSet<sparkles_owl::AxiomKind> {
    o.ontology
        .axiom_set()
        .filter(|a| !a.annotations.is_empty() && a.axiom_type().is_assertion())
        .map(|a| a.kind.clone())
        .collect()
}

/// The assertion a triple outside the terminology of `o` maps to, as
/// [`Opened::assertions`] maps it.
fn assertion_of(
    o: &Opened,
    annotated: &FxHashSet<sparkles_owl::AxiomKind>,
    t: &Triple,
) -> Option<Axiom> {
    let annotation_property = |p: &str| {
        o.ontology
            .typing
            .is_declared(p, EntityKind::AnnotationProperty)
    };
    if read::is_terminological(t, &annotation_property) {
        return None;
    }
    let ax = o.ontology.map_assertion(t)?;
    if annotated.contains(&ax.kind) {
        return None;
    }
    Some(ax)
}

/// Whether the state holds the one triple of a plain assertion in a graph `o` reads.
fn holds_main(snap: &Snapshot, o: &Opened, plain: &Axiom) -> bool {
    let mut fresh = write::counter(FRESH);
    let ts = write::axiom_triples(plain, &mut fresh);
    let [t] = ts.as_slice() else { return false };
    let ids = (
        snap.lookup_term(&t.subject.clone().into()),
        snap.lookup_term(&Term::NamedNode(t.predicate.clone())),
        snap.lookup_term(&t.object),
    );
    let (Some(s), Some(p), Some(x)) = ids else {
        return false;
    };
    o.inner
        .graph_ids
        .iter()
        .flatten()
        .any(|g| snap.contains(&[s, p, x, *g]).unwrap_or(false))
}

/// The profiles of a list such as `el,ql`.
pub fn parse_profiles(s: &str) -> Result<Vec<Profile>> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            if p.eq_ignore_ascii_case("all") {
                return Ok(Profile::ALL.to_vec());
            }
            Profile::parse(p).map(|p| vec![p]).ok_or_else(|| {
                Error::invalid(format!("unknown profile '{p}': el, ql, rl, dl or all"))
            })
        })
        .collect::<Result<Vec<Vec<Profile>>>>()
        .map(|v| v.into_iter().flatten().collect())
}

/// The profiles' reports, for the JSON forms.
pub fn report_json(
    o: &Opened,
    r: &Report,
    render: &sparkles_owl::ofn::Renderer,
) -> serde_json::Value {
    let violations: Vec<serde_json::Value> = r
        .violations
        .iter()
        .map(|v| {
            let mut j = serde_json::json!({ "rule": v.rule, "message": v.message });
            if let Some(ax) = &v.axiom {
                j["axiom"] = render.axiom(ax).into();
            }
            if let Some(g) = v.graph {
                j["graph"] = o.graph_name(g).into();
            }
            j
        })
        .collect();
    serde_json::json!({
        "profile": r.profile.as_str(),
        "title": r.profile.title(),
        "conforms": r.conforms,
        "checked": r.checked,
        "violations": violations,
        "total": r.total,
        "truncated": r.truncated,
    })
}

/// The kinds of axioms a filter names, such as `SubClassOf,ClassAssertion`.
pub fn parse_axiom_types(s: &str) -> Result<BTreeSet<AxiomType>> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| {
            AxiomType::parse(t).ok_or_else(|| Error::invalid(format!("unknown axiom type '{t}'")))
        })
        .collect()
}

#[cfg(test)]
mod tests;
