//! OWL 2 EL classification of an open ontology (spec F24): the inferred class hierarchy
//! and the unsatisfiable classes, computed by [`sparkles_owl::classify`] and, on request,
//! written to a named graph as axioms with PROV-O provenance on that graph's header.
//!
//! The dataset keeps its last classification of an ontology read by a caller who reads
//! every graph, with the classifier's saturated state. A classification of the same
//! ontology at the same commit answers from it, and one at a later commit gives the
//! classifier the axioms that changed, so that it saturates again only what they reach.
//! One classification of a dataset runs at a time on that path, and a second caller waits
//! for it under its own deadline and then reads its answer.
//!
//! ```
//! use sparkles::Dataset;
//! use sparkles::handles::ontology::{OntologyRequest, classify::ClassifyRequest};
//! use sparkles::owl::Class;
//!
//! let ds = Dataset::memory();
//! ds.update(
//!     "PREFIX owl: <http://www.w3.org/2002/07/owl#>
//!      PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
//!      PREFIX : <http://ex.org/>
//!      INSERT DATA {
//!        :partOf a owl:ObjectProperty .
//!        :Nucleus rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :partOf ;
//!                                   owl:someValuesFrom :Cell ] .
//!        :CellPart owl:equivalentClass [ a owl:Restriction ; owl:onProperty :partOf ;
//!                                        owl:someValuesFrom :Cell ] .
//!      }",
//! )?;
//! let c = ds.ontology().classify(
//!     &OntologyRequest::default(),
//!     &ClassifyRequest::default(),
//!     &Default::default(),
//! )?;
//! let t = &c.classification.taxonomy;
//! assert!(t.is_subclass(&Class::new("http://ex.org/Nucleus"), &Class::new("http://ex.org/CellPart")));
//! # Ok::<_, sparkles::Error>(())
//! ```

use super::kept::{
    InputKey, KeptClassification, KeptFlight, classification_retained_bytes, retention_cap,
};
use super::{
    EditOutcome, EditRequest, Extra, INFERRED_GRAPH, Ontologies, OntologyRequest, Opened, check,
};
use crate::error::{Error, Result};
use crate::sparql::QueryOptions;
use crate::store::Snapshot;
use crate::{Budget, BudgetKind};
use oxrdf::{Literal, NamedNode, Term, Triple};
use rustc_hash::FxHashSet;
use sparkles_core::algo::Control;
use sparkles_core::algo::pool::Par;
use sparkles_owl::classify::{
    Changes, Classification, Excluded, Expansion, Limits, Saturation, Unmapped, logical,
};
use sparkles_owl::read::Mode;
use sparkles_owl::transform::Dropped;
use sparkles_owl::{
    AnnotationProperty, AnnotationValue, Axiom, AxiomKind, ClassExpression, Entity, vocab,
};
use sparkles_reasoner::inputs::{GraphRef, ImportMode};
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// The most estimated bytes of a classifier the dataset keeps between classifications.
/// A larger one is dropped after its run, and the next run starts again.
pub const CACHE_MAX_BYTES: usize = 1 << 30;

const PROV: &str = "http://www.w3.org/ns/prov#";

/// What a classification covers and where it writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassifyRequest {
    /// also classify with the assertions the store holds, which can make the ontology
    /// inconsistent; such a run is never kept by the dataset
    pub assertions: bool,
    /// the worker threads asked for (0: the most one call of the algorithm pool may
    /// take); the algorithm pool admits them
    pub threads: usize,
    /// a named graph to write the taxonomy to, which must not be one of the graphs read
    pub target: Option<GraphRef>,
    /// the message of the commit that writes the taxonomy
    pub message: Option<String>,
    /// a class the caller asks about: a taxonomy without it is not found, and then
    /// nothing is written
    pub about: Option<String>,
    /// classify by the OWL 2 EL rules alone, as ELK does, with every axiom outside
    /// OWL 2 EL left out; the default also takes the Horn axioms outside OWL 2 EL
    /// ([`sparkles_owl::classify::Limits::el_only`]). The dataset keeps EL-only
    /// classifications separately from default runs.
    pub el_only: bool,
    /// a classification run (EL, Horn, or disjunctive) that its deadline, cancellation or
    /// memory budget stops after saturation begins answers with what it proved, a
    /// [`Classification`] whose [`complete`](Classification::complete) is false, instead
    /// of failing ([`sparkles_owl::classify::Limits::partial`]). A classification that
    /// writes its taxonomy refuses it.
    pub partial: bool,
    /// the expansion strategy of the disjunctive rules, which gives the same answer
    /// either way ([`sparkles_owl::classify::Limits::expansion`])
    pub expansion: Expansion,
    /// refuse the classification when it leaves an axiom out or uses one only in part,
    /// or stopped part way, as a complete reasoner such as HermiT refuses what it does
    /// not support, with [`STRICT_CODE`] and the axioms in the message. The check reads
    /// the final answer, a kept one included, and comes before a taxonomy is written.
    /// It does not change what the dataset keeps.
    pub strict: bool,
    /// skip the bottom-locality modules that decide, when axioms are left out, which
    /// classes keep their complete answer, for a caller that reads only the taxonomy:
    /// every class is then incomplete and the consistency unknown when something is left
    /// out ([`sparkles_owl::classify::Limits::coverage`])
    pub skip_coverage: bool,
}

/// The code of a strict classification's refusal ([`ClassifyRequest::strict`]), whose
/// component is `classifier`.
pub const STRICT_CODE: &str = "classification-incomplete";

/// A strict request's answer to a run that is not complete for the whole ontology: the
/// caller's own deadline or cancellation when it stopped the modules' pass, which is no
/// refusal of the ontology, and otherwise [`strict_refusal`].
pub(super) fn refuse_strict(
    classification: &Classification,
    excluded: &[Excluded],
    opts: &QueryOptions,
) -> Error {
    if classification.stats.coverage_stopped
        && let Err(e) = check(opts)
    {
        return e;
    }
    strict_refusal(classification, excluded)
}

/// The refusal of a strict classification whose answer is not complete for the whole
/// ontology: the axioms left out, which the error's source holds as
/// [`sparkles_owl::classify::Error::Incomplete`], or the run that stopped.
pub fn strict_refusal(classification: &Classification, excluded: &[Excluded]) -> Error {
    let unmapped = classification.stats.unmapped;
    let mut message = if !excluded.is_empty() {
        sparkles_owl::classify::refusal(excluded)
    } else if unmapped > 0 {
        String::from("the classification would not be complete")
    } else {
        format!(
            "the classification would not be complete: the run stopped before it decided {} classes",
            classification.incomplete.len()
        )
    };
    if unmapped > 0 {
        message.push_str(&format!(
            "; {unmapped} triples of the input map to no axiom, such as the domain of a \
             property that is not declared, and may carry anything: declare the property \
             or remove the triples"
        ));
        let first: Vec<String> = classification
            .unmapped
            .triples
            .iter()
            .take(3)
            .map(|t| format!("{t} ."))
            .collect();
        if !first.is_empty() {
            message.push_str(&format!(", such as {}", first.join(" ")));
        }
    }
    incomplete(message, excluded)
}

fn incomplete(message: String, excluded: &[Excluded]) -> Error {
    Error::Component(Box::new(
        crate::error::ComponentError::new("classifier", STRICT_CODE, message)
            .with_source(sparkles_owl::classify::Error::Incomplete(excluded.to_vec())),
    ))
}

/// The opening of the comment the classifier writes on a taxonomy graph's header, by
/// which a later write knows the comment it replaces.
const COMMENT: &str = "The OWL 2 EL taxonomy that the Sparkles classifier computed from commit ";

/// A classification.
#[derive(Clone)]
pub struct Classified {
    pub opened: Opened,
    pub classification: Arc<Classification>,
    /// Immutable source, producer policy and content identity of this computation.
    pub(super) publication: Arc<KeptClassification>,
    pub(super) retained: bool,
    // Caller permissions and cancellation only. The snapshot has triple masks,
    // while graph restrictions still need their immutable access rules. Do not
    // hold a dataset owner or query memory-pool grant for projection reads.
    pub(super) read_options: QueryOptions,
    /// the axioms left out, as outside OWL 2 EL, or used only in part
    pub excluded: Arc<ClassifiedExclusions>,
    /// whether the dataset's kept classification of the same state answered
    pub reused: bool,
    /// the edit that wrote the taxonomy to the target graph
    pub written: Option<EditOutcome>,
}

/// A classification's exclusions, sharing the credit for their backing data.
#[derive(Debug)]
pub struct ClassifiedExclusions {
    values: Vec<Excluded>,
    pub(super) memory: Option<Arc<sparkles_budget::MemoryLease>>,
}

impl ClassifiedExclusions {
    pub(super) fn new(values: Vec<Excluded>) -> Self {
        Self {
            values,
            memory: None,
        }
    }
}

impl PartialEq for ClassifiedExclusions {
    fn eq(&self, other: &Self) -> bool {
        self.values == other.values
    }
}

impl Eq for ClassifiedExclusions {}

impl std::ops::Deref for ClassifiedExclusions {
    type Target = Vec<Excluded>;

    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

impl AsRef<Vec<Excluded>> for ClassifiedExclusions {
    fn as_ref(&self) -> &Vec<Excluded> {
        &self.values
    }
}

impl Classified {
    /// Whether every subsumption between named classes that the ontology entails is in
    /// the taxonomy: true when no axiom is left out or used only in part and the run was
    /// not stopped part way ([`ClassifyRequest::partial`]).
    pub fn complete(&self) -> bool {
        self.excluded.is_empty() && self.classification.complete
    }
}

/// A completed run still obeys the caller's stop controls during projection.
/// An actual partial result has already acknowledged the compute stop: finish
/// its immutable metadata/answer as the existing partial API requires. This
/// changes only result-local options, never the shared flags, workers or cache.
pub(super) fn result_options(opts: &QueryOptions, complete: bool) -> QueryOptions {
    result_controls(opts, complete)
}

/// Whether a run acknowledged a stop of its caller: a partial answer, or a modules' pass
/// that the deadline or a cancellation stopped. Its metadata and answer then finish
/// without the stop controls ([`result_options`]).
pub(super) fn acknowledged_stop(c: &Classification) -> bool {
    !c.complete_for_taken || c.stats.coverage_stopped
}

fn result_controls(opts: &QueryOptions, complete: bool) -> QueryOptions {
    QueryOptions {
        graphs: opts.graphs.clone(),
        cancel: if complete { opts.cancel.clone() } else { None },
        cancellation: if complete {
            opts.cancellation.clone()
        } else {
            None
        },
        deadline: if complete { opts.deadline } else { None },
        ..Default::default()
    }
}

/// The classification the dataset keeps: the saturated contexts and the open ontology
/// they were saturated from, whose axioms the saturation reads in place rather than
/// copying them.
pub struct CacheEntry {
    pub(super) key: InputKey,
    pub(super) el_only: bool,
    pub(super) retained_bytes: usize,
    pub(super) mutated: bool,
    ontology: Option<Arc<super::SharedOntology>>,
    /// the axioms beyond the ontology's that the saturation last read, such as a DL
    /// query's defining axiom, which the next run removes unless it reads them again
    extra: Vec<AxiomKind>,
    pub(super) saturation: Saturation,
    /// the taxonomy of the ontology at `commit`, once a classification computed it,
    /// which a DL query's run over the saturation leaves as it is
    pub(super) result: Option<Arc<KeptClassification>>,
    memory: Option<crate::reasoning::memory::Memory>,
}

impl CacheEntry {
    pub(super) fn new(key: InputKey, el_only: bool) -> Self {
        Self {
            key,
            el_only,
            retained_bytes: 0,
            mutated: false,
            ontology: None,
            extra: Vec::new(),
            saturation: Saturation::new(),
            result: None,
            memory: None,
        }
    }
    /// The estimated bytes of the kept taxonomy, 0 when none is kept.
    pub(super) fn taxonomy_memory(&self) -> usize {
        self.result
            .as_ref()
            .map_or(0, |p| p.classification.stats.taxonomy_memory)
    }
    /// Lets both flight finalization and the next classification know it has gone.
    pub(super) fn drop_taxonomy(&mut self) {
        self.result = None;
    }

    /// Keep immutable taxonomy credit with the publication and mutable saturation
    /// credit with this cache entry. A cache miss is safe if retention is refused.
    #[cfg(test)]
    fn admit_retention(
        &mut self,
        pool: Option<&Arc<sparkles_core::sparql::pool::QueryPool>>,
    ) -> bool {
        let bytes = self
            .retained_bytes
            .saturating_sub(self.taxonomy_memory())
            .saturating_sub(self.result.as_ref().map_or(0, |p| p.auxiliary_bytes))
            as u64;
        self.admit_retention_bytes(pool, bytes)
    }

    fn admit_retention_bytes(
        &mut self,
        pool: Option<&Arc<sparkles_core::sparql::pool::QueryPool>>,
        bytes: u64,
    ) -> bool {
        let Some(pool) = pool else { return true };
        let held = self.memory.get_or_insert_with(|| {
            crate::reasoning::memory::Memory::new(None, Some(super::kept::cache_parent(pool)))
        });
        if held.cover(bytes).is_err() {
            return false;
        }
        held.shrink_to(bytes);
        true
    }

    fn credit(&self) -> u64 {
        self.memory.as_ref().map_or(0, |memory| memory.bytes())
    }

    pub(super) fn reused_credit(&self) -> u64 {
        self.credit()
            .saturating_sub(super::kept::entry_aux_bytes(self) as u64)
    }

    // A work-only cache can take over its own working credit without asking
    // the same pool to fund another copy. Caller grants must remain independent.
    fn shares_work(
        pool: Option<&Arc<sparkles_core::sparql::pool::QueryPool>>,
        opts: &QueryOptions,
    ) -> bool {
        opts.grant.is_none()
            && pool.is_some_and(|pool| {
                pool.domain_stats().retained_capacity == 0
                    && opts
                        .pool
                        .as_ref()
                        .is_some_and(|work| Arc::ptr_eq(work, pool))
            })
    }

    pub(super) fn bind_work(
        &self,
        pool: Option<&Arc<sparkles_core::sparql::pool::QueryPool>>,
        opts: &QueryOptions,
        work: &Working,
    ) {
        if Self::shares_work(pool, opts)
            && let Some(parent) = self.memory.as_ref().and_then(|memory| memory.parent())
        {
            let mut work = work.lock();
            if work.bytes() == 0 {
                *work = crate::reasoning::memory::Memory::new(work.limit(), Some(parent));
            }
        }
    }

    pub(super) fn reconcile(
        &mut self,
        pool: Option<&Arc<sparkles_core::sparql::pool::QueryPool>>,
        opts: &QueryOptions,
        work: &Working,
        bytes: u64,
    ) -> bool {
        if !Self::shares_work(pool, opts) {
            return self.admit_retention_bytes(pool, bytes);
        }
        let old = self.credit();
        if old >= bytes {
            if let Some(held) = &mut self.memory {
                held.shrink_to(bytes);
            }
            return true;
        }
        let Ok(mut extra) = work.lock().split(bytes - old) else {
            return false;
        };
        if let Some(held) = &mut self.memory {
            held.absorb(&mut extra).is_ok()
        } else {
            self.memory = Some(extra);
            true
        }
    }

    pub(super) fn stage_input(&self, o: &Opened, extra: usize, work: &Working) -> Result<()> {
        stage_axioms(
            work,
            o.ontology.axioms.len()
                + self.ontology.as_ref().map_or(0, |old| old.axioms.len())
                + self.extra.len()
                + extra,
        )
    }

    /// Runs `go` on the kept saturation over the logical axioms of `o` and `extra`, with
    /// the changes since the axioms it last read, so that it saturates again only what
    /// they reach. The saturation holds the new axioms afterwards whether the run ends
    /// or not.
    pub(super) fn run_over<R>(
        &mut self,
        o: &Opened,
        extra: Vec<AxiomKind>,
        go: impl FnOnce(&mut Saturation, &[&AxiomKind], Option<Changes<'_>>) -> Result<R>,
    ) -> Result<R> {
        self.mutated = true;
        let before = self.ontology.take();
        let before_extra = std::mem::take(&mut self.extra);
        let out = {
            let mut axioms = logical_axioms(&o.ontology);
            axioms.extend(extra.iter().filter(|k| logical(k)));
            let (added, removed): (Vec<&AxiomKind>, Vec<&AxiomKind>) = match &before {
                Some(b) => {
                    let now: FxHashSet<&AxiomKind> = axioms.iter().copied().collect();
                    let mut then: FxHashSet<&AxiomKind> = logical_axioms(b).into_iter().collect();
                    then.extend(before_extra.iter().filter(|k| logical(k)));
                    (
                        now.iter().copied().filter(|k| !then.contains(k)).collect(),
                        then.iter().copied().filter(|k| !now.contains(k)).collect(),
                    )
                }
                None => (Vec::new(), Vec::new()),
            };
            let changes = before.as_ref().map(|_| Changes {
                added: &added,
                removed: &removed,
            });
            go(&mut self.saturation, &axioms, changes)
        };
        self.ontology = Some(o.ontology.clone());
        self.extra = extra;
        out
    }
}

/// The logical axioms of an ontology, in its order.
pub(super) type Working = Arc<parking_lot::Mutex<crate::reasoning::memory::Memory>>;

pub(super) fn working(opts: &QueryOptions, opened: &Opened) -> Working {
    Arc::new(parking_lot::Mutex::new(
        crate::reasoning::memory::Memory::query_after(opts, opened.request_bytes),
    ))
}

pub(super) fn stage_axioms(memory: &Working, count: usize) -> Result<()> {
    // Includes the reference vectors, the old and new sets and their delta lists.
    memory.lock().grow((128 + count.saturating_mul(96)) as u64)
}

pub(super) fn stage_assertion(memory: &Working, axiom: &Axiom) -> Result<()> {
    memory
        .lock()
        .grow((2 * sparkles_owl::classify::axiom_copy_bytes(&axiom.kind)) as u64)
}

pub(crate) fn logical_axioms(o: &sparkles_owl::read::Ontology) -> Vec<&AxiomKind> {
    o.axioms
        .keys()
        .map(|a| &a.kind)
        .filter(|k| logical(k))
        .collect()
}

/// The logical content of an open ontology's terminology, without annotations.
fn kinds_of(o: &Opened) -> FxHashSet<&AxiomKind> {
    logical_axioms(&o.ontology).into_iter().collect()
}

/// Whether an axiom is one a taxonomy writes: a subsumption or an equivalence between
/// named classes, without annotations.
fn taxonomy_axiom(a: &Axiom) -> bool {
    a.annotations.is_empty()
        && match &a.kind {
            AxiomKind::SubClassOf(ClassExpression::Class(_), ClassExpression::Class(_)) => true,
            AxiomKind::EquivalentClasses(cs) => {
                cs.iter().all(|c| matches!(c, ClassExpression::Class(_)))
            }
            _ => false,
        }
}

impl Ontologies {
    /// Classify the ontology `req` names (spec F24): its inferred class hierarchy and
    /// unsatisfiable classes. `c.target` also writes the taxonomy to that graph in one
    /// commit, replacing the taxonomy it held. The run takes its threads from the
    /// algorithm pool, its memory budget from `opts.max_memory_bytes` and the memory
    /// pool's grant, and stops at the deadline or a cancellation.
    pub fn classify(
        &self,
        req: &OntologyRequest,
        c: &ClassifyRequest,
        opts: &QueryOptions,
    ) -> Result<Classified> {
        req.validate()?;
        if req.inferences {
            return Err(Error::invalid(
                "a classification reads the asserted axioms: leave out 'inferences'",
            ));
        }
        if c.target.is_some() && c.partial {
            return Err(Error::invalid(
                "a classification that writes its taxonomy needs a complete answer: leave out 'partial' or 'target'",
            ));
        }
        if c.target.is_some() && req.at.is_some() {
            return Err(Error::invalid(
                "a classification that writes reads the head: leave out 'at'",
            ));
        }
        let (snap, mut opts) = self.state(req, opts)?;
        let caller_graphs = opts.graphs.clone();
        // Decide data-dependent graph guards once on the ORIGINAL snapshot.
        // Projection names read the already triple-masked snapshot, where policy
        // triples can be absent. Re-deciding there could widen the graph grant.
        opts.graphs = opts.graphs.as_ref().map(|access| access.bound(&snap));
        let o = self.open_at(&snap, req, &opts)?;
        // a run that skips the modules' pass answers differently, so it is not kept
        let keep = self.can_keep(&o, &opts)
            && opts.graphs.is_none()
            && snap.cacheable
            && !c.assertions
            && !c.skip_coverage;
        let (publication, reused) = if keep {
            self.classify_kept(&o, req, c, &opts)?
        } else {
            let work = working(&opts, &o);
            let mut assertions: Vec<Axiom> = Vec::new();
            // the assertion triples that map to no axiom
            let mut unread = Unmapped::default();
            if c.assertions {
                o.assertions_with_memory(&opts, Some(&work), |ax, t, _| {
                    match ax {
                        Some(ax) => {
                            stage_assertion(&work, &ax)?;
                            assertions.push(ax);
                        }
                        None => {
                            if unread.triples.len() < sparkles_owl::classify::UNMAPPED_LISTED {
                                let subject = match &t.subject {
                                    oxrdf::NamedOrBlankNode::NamedNode(n) => n.as_str().len(),
                                    oxrdf::NamedOrBlankNode::BlankNode(b) => b.as_str().len(),
                                };
                                work.lock().grow(
                                    (256 + subject
                                        + t.predicate.as_str().len()
                                        + crate::explain::term_bytes(&t.object))
                                        as u64,
                                )?;
                            }
                            unread.push(t);
                        }
                    }
                    Ok(true)
                })?;
            }
            stage_axioms(&work, o.ontology.axioms.len() + assertions.len())?;
            let mut axioms = logical_axioms(&o.ontology);
            axioms.extend(assertions.iter().map(|a| &a.kind).filter(|k| logical(k)));
            let (out, excluded) = {
                let seen = RefCell::new(None);
                let refused = RefCell::new(None);
                stage_unmapped(&o, &unread, &work)?;
                let mut dropped = dropped_assertions(&o, &opts, &seen, &work, &refused);
                let dropped = (!c.assertions).then_some(&mut dropped as &mut Dropped<'_>);
                let unmapped = || unmapped_of(&o.ontology, &unread, &seen);
                let answer = classify_once_working(&axioms, c, &opts, dropped, &unmapped, &work);
                if let Some(error) = refused.borrow_mut().take() {
                    return Err(error);
                }
                answer?
            };
            drop(axioms);
            drop(assertions);
            drop(unread);
            work.lock().shrink_to(
                (out.stats.taxonomy_memory
                    + out.incomplete_bytes()
                    + sparkles_owl::classify::excluded_copy_bytes(&excluded)
                    + unmapped_bytes(&out.unmapped)) as u64,
            );
            (
                KeptClassification::capture(self, req, c, &o, (out, excluded), &opts, &work)?
                    .seal(),
                false,
            )
        };
        if c.strict && !publication.classification.complete {
            return Err(refuse_strict(
                &publication.classification,
                &publication.excluded,
                &opts,
            ));
        }
        if let Some(about) = &c.about
            && publication
                .classification
                .taxonomy
                .node_of(&sparkles_owl::Class::new(about.as_str()))
                .is_none()
        {
            return Err(Error::NotFound(format!(
                "the taxonomy has no class <{about}>"
            )));
        }
        let retained = keep
            && self
                .ds
                .state()
                .classify_cache
                .lock()
                .slot(c.el_only)
                .publication
                .as_ref()
                .is_some_and(|p| p.id == publication.id);
        let classified_complete = !acknowledged_stop(&publication.classification);
        let mut classified = Classified {
            opened: o,
            classification: publication.classification.clone(),
            excluded: publication.excluded.clone(),
            publication,
            retained,
            read_options: result_options(&opts, classified_complete),
            reused,
            written: None,
        };
        if let Some(target) = &c.target {
            // Preserve the writer's existing authorization path with the original
            // caller rules. A bound projection read is not a write capability.
            let write_opts = QueryOptions {
                graphs: caller_graphs,
                ..opts
            };
            classified.written =
                Some(self.write_taxonomy(req, &classified, target, c, &write_opts)?);
        }
        Ok(classified)
    }

    /// A classification through the dataset's kept classifier.
    fn classify_kept(
        &self,
        o: &Opened,
        req: &OntologyRequest,
        c: &ClassifyRequest,
        opts: &QueryOptions,
    ) -> Result<(Arc<KeptClassification>, bool)> {
        // Preserve the legacy no-work idle cache hit, including its past-compute-
        // deadline behavior. An active flight still waits under caller controls;
        // GET/frame name and page work has its own read checks.
        let key = InputKey::of(self, req, &o.snapshot);
        let state = self.ds.state();
        if let Some(_idle) = state.classify_flight.try_lock() {
            let cached = state
                .classify_cache
                .lock()
                .slot(c.el_only)
                .publication
                .clone();
            if let Some(p) = cached.filter(|p| {
                p.source.input == key
                    && p.source.commit == o.snapshot.commit
                    && p.source.el_only == c.el_only
            }) {
                return Ok((p, true));
            }
        }
        let work = working(opts, o);
        self.with_kept(o, req, c.el_only, opts, "classify", |e| {
            if let Some(p) = &e.result
                && p.source.commit == o.snapshot.commit
                && p.source.el_only == c.el_only
            {
                return Ok(((p.clone(), true), true));
            }
            // Admission precedes mutation, so a refused admission preserves a proven
            // old entry/publication. Later errors conservatively discard both.
            let pool = state.pool.as_ref();
            e.bind_work(pool, opts, &work);
            let reused_credit = e.reused_credit();
            let par = admit(c.threads, opts)?;
            let seen = RefCell::new(None);
            let refused = RefCell::new(None);
            let none = Unmapped::default();
            stage_unmapped(o, &none, &work)?;
            let mut dropped = dropped_assertions(o, opts, &seen, &work, &refused);
            let unmapped = || unmapped_of(&o.ontology, &none, &seen);
            e.stage_input(o, 0, &work)?;
            let answer = e.run_over(o, Vec::new(), |s, axioms, changes| {
                run(
                    s,
                    axioms,
                    changes,
                    par,
                    opts,
                    c,
                    Some(&mut dropped),
                    &unmapped,
                    &work,
                    reused_credit,
                )
            });
            if let Some(error) = refused.borrow_mut().take() {
                return Err(error);
            }
            let (out, witness) = answer?;
            #[cfg(test)]
            let out = if FORCE_PARTIAL.with(|f| f.replace(false)) {
                let mut partial = out;
                partial.complete = false;
                partial.complete_for_taken = false;
                partial.consistency = sparkles_owl::classify::Consistency::Unknown;
                partial.incomplete = partial.taxonomy.classes().cloned().collect();
                partial
            } else {
                out
            };
            #[cfg(test)]
            let out = if FORCE_COVERAGE_STOP.with(|f| f.replace(false)) {
                // the caller's stop that the pass acknowledged
                if let Some(c) = &opts.cancel {
                    c.store(true, Ordering::Relaxed);
                }
                let mut stopped = out;
                stopped.complete = false;
                stopped.stats.coverage_stopped = true;
                stopped.consistency = sparkles_owl::classify::Consistency::Unknown;
                stopped.incomplete = stopped.taxonomy.classes().cloned().collect();
                stopped
            } else {
                out
            };
            let saturation_bytes = out
                .stats
                .memory
                .saturating_sub(out.stats.taxonomy_memory)
                .saturating_sub(out.incomplete_bytes()) as u64;
            let used_reuse = saturation_bytes.min(reused_credit);
            let witness_bytes = witness.as_ref().map_or(0, |w| {
                sparkles_owl::classify::excluded_slice_copy_bytes(std::slice::from_ref(w))
            });
            work.lock().shrink_to(
                (out.stats.memory + witness_bytes + unmapped_bytes(&out.unmapped)) as u64
                    - used_reuse,
            );
            let mut excluded = copy_excluded(&e.saturation, &work)?;
            excluded.extend(witness);
            let mut p = KeptClassification::capture(self, req, c, o, (out, excluded), opts, &work)?;
            let retained_bytes = classification_retained_bytes(e, &p);
            let keep = p.classification.complete_for_taken && retained_bytes <= retention_cap();
            // A modules' pass that the deadline stopped answers by timing, so the next
            // request takes the pass again over the kept saturation.
            let publish = keep && !p.classification.stats.coverage_stopped;
            // Mutable saturation and immutable taxonomy must both have cache credit.
            // A refusal leaves the answer under its request credit.
            let keep = if keep {
                e.retained_bytes = if publish {
                    retained_bytes
                } else {
                    retained_bytes - p.classification.stats.taxonomy_memory - p.auxiliary_bytes
                };
                let publication_bytes = if publish {
                    p.classification.stats.taxonomy_memory + p.auxiliary_bytes
                } else {
                    0
                };
                e.reconcile(
                    pool,
                    opts,
                    &work,
                    (e.retained_bytes - publication_bytes) as u64,
                ) && (!publish || p.admit_retention(opts, pool))
            } else {
                false
            };
            let p = p.seal();
            e.result = (keep && publish).then(|| p.clone());
            Ok(((p, false), keep))
        })
    }

    /// One owned computing flight. Readers never take this lock or construct an
    /// entry. Cancellation before mutation preserves the old state; mutating errors
    /// and unwinds discard it. Publication changes are finalized as one pair.
    pub(super) fn with_kept<T>(
        &self,
        o: &Opened,
        req: &OntologyRequest,
        el_only: bool,
        opts: &QueryOptions,
        kind: &'static str,
        f: impl FnOnce(&mut CacheEntry) -> Result<(T, bool)>,
    ) -> Result<T> {
        sparkles_core::sparql::pool::QueryPool::with_deferred_release_wakes(|| {
            check(opts)?;
            let key = InputKey::of(self, req, &o.snapshot);
            let state = self.ds.state();
            let lock = loop {
                if let Some(g) = state
                    .classify_flight
                    .try_lock_for(Duration::from_millis(50))
                {
                    break g;
                }
                #[cfg(test)]
                FLIGHT_WAIT_NOTIFY.with(|n| {
                    if let Some(tx) = n.borrow_mut().take() {
                        let _ = tx.send(());
                    }
                });
                check(opts)?;
            };
            check(opts)?;
            let mut flight = KeptFlight::begin(state, lock, key, el_only, o.snapshot.commit, kind);
            match f(flight.entry()) {
                Ok((out, keep)) => {
                    flight.finish(keep);
                    Ok(out)
                }
                Err(err) => {
                    let preserve = !flight.entry().mutated;
                    flight.finish(preserve);
                    Err(err)
                }
            }
        })
    }

    /// Write the taxonomy to `target` in one commit: add its axioms, remove the taxonomy
    /// axioms the graph held that it no longer has, and replace the provenance on the
    /// graph's ontology header. The write refuses with a conflict when the terminology
    /// read changed between the classification and the writer lock.
    fn write_taxonomy(
        &self,
        req: &OntologyRequest,
        cl: &Classified,
        target: &GraphRef,
        c: &ClassifyRequest,
        opts: &QueryOptions,
    ) -> Result<EditOutcome> {
        let GraphRef::Named(g) = target else {
            return Err(Error::invalid(
                "a classification writes to a named graph, not the default graph",
            ));
        };
        let g_node =
            NamedNode::new(g.as_str()).map_err(|e| Error::invalid(format!("target <{g}>: {e}")))?;
        if g == INFERRED_GRAPH || cl.opened.graphs.contains(target) {
            return Err(Error::invalid(format!(
                "the target <{g}> is a graph the classification reads; name another graph"
            )));
        }
        let treq = OntologyRequest {
            graphs: vec![target.clone()],
            imports: ImportMode::None,
            mode: Mode::Lenient,
            at: None,
            inferences: false,
            import_targets: Default::default(),
        };
        let props = ["wasGeneratedBy", "generatedAtTime", "wasDerivedFrom"]
            .map(|l| AnnotationProperty::new(format!("{PROV}{l}")));
        let mut add: Vec<Axiom> = cl.classification.taxonomy.axioms();
        add.extend(
            props
                .iter()
                .map(|p| Axiom::declaration(Entity::AnnotationProperty(p.clone()))),
        );
        let edit = EditRequest {
            add,
            remove: Vec::new(),
            declare: Some(true),
            graph: Some(target.clone()),
            message: c.message.clone(),
        };
        let read_commit = cl.opened.snapshot.commit;
        let read_kinds = kinds_of(&cl.opened);
        let mut classified_req = req.clone();
        classified_req.at = None;
        let derived: Vec<String> = cl
            .opened
            .graphs
            .iter()
            .filter_map(|g| match g {
                GraphRef::Named(n) => Some(n.clone()),
                GraphRef::Default => None,
            })
            .collect();
        let activity = format!("urn:uuid:{}", uuid::Uuid::new_v4());
        let mut hook = |txn: &mut crate::store::WriteTxn<'_>, head: &Arc<Snapshot>| -> Result<()> {
            if head.commit != read_commit && c.assertions {
                // the assertions read are not kept to compare, so any commit refuses
                return Err(Error::Conflict(
                    "the dataset changed after the classification with its assertions: classify again".into(),
                ));
            }
            if head.commit != read_commit {
                let now = self.open_at(head, &classified_req, opts)?;
                if kinds_of(&now) != read_kinds {
                    return Err(Error::Conflict(
                        "the ontology changed after it was classified: classify again".into(),
                    ));
                }
            }
            let t = self.open_at(head, &treq, opts)?;
            for ax in t.ontology.axiom_set() {
                if let AxiomKind::Declaration(Entity::ObjectProperty(p)) = &ax.kind
                    && p.as_str().starts_with(PROV)
                {
                    return Err(Error::Conflict(format!(
                        "the target types the PROV-O property <{}> as an object property, so it cannot carry provenance as an annotation",
                        p.as_str()
                    )));
                }
                if let AxiomKind::Declaration(Entity::DataProperty(p)) = &ax.kind
                    && p.as_str().starts_with(PROV)
                {
                    return Err(Error::Conflict(format!(
                        "the target types the PROV-O property <{}> as a data property, so it cannot carry provenance as an annotation",
                        p.as_str()
                    )));
                }
            }
            let gid = txn.intern(&Term::NamedNode(g_node.clone()))?;
            // the provenance goes on the target's header, or on a new one named by the
            // graph when it has none
            let subject = match (&t.ontology.header.iri, t.ontology.header_graph) {
                (Some(iri), _) => Term::NamedNode(iri.clone()),
                (None, Some(_)) => {
                    return Err(Error::Conflict(format!(
                        "the ontology header of the target <{g}> has no IRI, so it cannot be named to carry the provenance"
                    )));
                }
                (None, None) => Term::NamedNode(g_node.clone()),
            };
            // the previous provenance of the header goes, and the comment the classifier
            // wrote, but not the other comments
            if t.ontology.header.iri.is_some() {
                for a in &t.ontology.header.annotations {
                    let ours = props.contains(&a.property)
                        || (a.property.as_str() == vocab::RDFS_COMMENT
                            && matches!(&a.value, AnnotationValue::Literal(l) if l.value().starts_with(COMMENT)));
                    if !ours {
                        continue;
                    }
                    let object = match &a.value {
                        AnnotationValue::Iri(i) => Term::NamedNode(i.clone()),
                        AnnotationValue::Literal(l) => Term::Literal(l.as_rdf().clone()),
                        AnnotationValue::Anonymous(_) => continue,
                    };
                    let q = [
                        txn.intern(&subject)?,
                        txn.intern(&Term::NamedNode(a.property.0.clone()))?,
                        txn.intern(&object)?,
                        gid,
                    ];
                    txn.delete(q)?;
                }
            }
            let now = sparkles_core::builder::now_rfc3339();
            let mut header: Vec<(String, Term)> = vec![
                (
                    vocab::RDF_TYPE.to_string(),
                    Term::NamedNode(NamedNode::new_unchecked(vocab::OWL_ONTOLOGY)),
                ),
                (
                    format!("{PROV}wasGeneratedBy"),
                    Term::NamedNode(NamedNode::new_unchecked(&activity)),
                ),
                (
                    format!("{PROV}generatedAtTime"),
                    Term::Literal(Literal::new_typed_literal(
                        now,
                        NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#dateTime"),
                    )),
                ),
                (
                    vocab::RDFS_COMMENT.to_string(),
                    Term::Literal(Literal::new_simple_literal(format!(
                        "{COMMENT}{read_commit}"
                    ))),
                ),
            ];
            for d in &derived {
                header.push((
                    format!("{PROV}wasDerivedFrom"),
                    Term::NamedNode(NamedNode::new_unchecked(d)),
                ));
            }
            for (p, o) in header {
                let q = [
                    txn.intern(&subject)?,
                    txn.intern(&Term::NamedNode(NamedNode::new_unchecked(p)))?,
                    txn.intern(&o)?,
                    gid,
                ];
                txn.insert(q)?;
            }
            Ok(())
        };
        let replace = |a: &Axiom| taxonomy_axiom(a);
        self.edit_with(
            &treq,
            &edit,
            None,
            opts,
            Extra {
                replace: Some(&replace),
                before_commit: Some(&mut hook),
            },
        )
    }
}

#[cfg(test)]
thread_local! {
    /// Makes the next admission on this thread time out, as a full pool would.
    pub(super) static FORCE_PARTIAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(super) static FLIGHT_WAIT_NOTIFY: std::cell::RefCell<Option<std::sync::mpsc::Sender<()>>> = const { std::cell::RefCell::new(None) };
    pub(super) static REFUSE_ADMISSION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Makes the next kept classification on this thread answer as one whose modules'
    /// pass the deadline stopped.
    pub(super) static FORCE_COVERAGE_STOP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The threads of a classification, from the algorithm pool. A caller takes them before
/// it changes any kept state, since the wait can end at the deadline or a cancellation.
pub(super) fn admit(threads: usize, opts: &QueryOptions) -> Result<Par> {
    #[cfg(test)]
    if REFUSE_ADMISSION.with(|r| r.replace(false)) {
        return Err(Error::Timeout);
    }
    let flag = opts.cancel.clone().unwrap_or_default();
    let ctl = Control::with_cancellation(flag, opts.cancellation.clone(), opts.deadline, u64::MAX);
    Par::acquire(threads, &ctl)
}

/// A classification of `axioms` by a saturation of its own, on the threads `c` asks the
/// algorithm pool for and under the deadline, cancellation and memory budget of `opts`,
/// with the axioms it left out or used in part.
///
/// `dropped` streams the axioms of the ontology that `axioms` leaves out, such as the
/// assertions of a classification of the terminology alone, which count as left out
/// when one has logical content ([`Classification::cover`]). `unmapped` answers, after
/// that read, how many triples of the input map to no axiom, which count as left out
/// too ([`Classification::leave_out_unmapped`]).
pub(crate) fn classify_once(
    axioms: &[&AxiomKind],
    c: &ClassifyRequest,
    opts: &QueryOptions,
    dropped: Option<&mut Dropped<'_>>,
    unmapped: &dyn Fn() -> Unmapped,
) -> Result<(Classification, Vec<Excluded>)> {
    let work = Arc::new(parking_lot::Mutex::new(
        crate::reasoning::memory::Memory::query(opts),
    ));
    classify_once_working(axioms, c, opts, dropped, unmapped, &work)
}

fn classify_once_working(
    axioms: &[&AxiomKind],
    c: &ClassifyRequest,
    opts: &QueryOptions,
    dropped: Option<&mut Dropped<'_>>,
    unmapped: &dyn Fn() -> Unmapped,
    work: &Working,
) -> Result<(Classification, Vec<Excluded>)> {
    let mut s = Saturation::new();
    let par = admit(c.threads, opts)?;
    let staged = work.lock().bytes();
    let (out, witness) = run(
        &mut s, axioms, None, par, opts, c, dropped, unmapped, work, 0,
    )?;
    work.lock()
        .shrink_to(staged.saturating_add(out.stats.memory as u64));
    let mut excluded = copy_excluded(&s, work)?;
    excluded.extend(witness);
    Ok((out, excluded))
}

pub(super) fn copy_excluded(saturation: &Saturation, work: &Working) -> Result<Vec<Excluded>> {
    let excluded = saturation.excluded();
    work.lock()
        .grow(sparkles_owl::classify::excluded_slice_copy_bytes(excluded) as u64)?;
    Ok(excluded.to_vec())
}

/// The assertions of an open ontology, as the axioms a classification of its terminology
/// alone drops ([`classify_once`]). A triple that maps to no axiom stops the read and
/// sets `unmapped`.
pub(super) fn dropped_assertions<'o>(
    o: &'o Opened,
    opts: &'o QueryOptions,
    unmapped: &'o RefCell<Option<Triple>>,
    work: &'o Working,
    refused: &'o RefCell<Option<Error>>,
) -> impl FnMut(&mut dyn FnMut(&AxiomKind) -> bool) -> bool + 'o {
    move |f| {
        let result = o.assertions_with_memory(opts, Some(work), |ax, t, _| {
            Ok(match ax {
                Some(ax) => {
                    if logical(&ax.kind) && !matches!(&ax.kind, AxiomKind::Declaration(_)) {
                        // Coverage may keep one cloned logical axiom as its witness.
                        stage_assertion(work, &ax)?;
                    }
                    f(&ax.kind)
                }
                None => {
                    work.lock().grow(triple_copy_bytes(t) * 3)?;
                    *unmapped.borrow_mut() = Some(t.clone());
                    false
                }
            })
        });
        match result {
            Ok(()) => true,
            Err(Error::Cancelled | Error::Timeout) => false,
            Err(error) => {
                *refused.borrow_mut() = Some(error);
                false
            }
        }
    }
}

fn triple_copy_bytes(triple: &Triple) -> u64 {
    256 + sparkles_owl::read::triple_payload(triple) as u64
}

pub(super) fn unmapped_bytes(unmapped: &Unmapped) -> usize {
    unmapped
        .triples
        .iter()
        .map(|triple| triple_copy_bytes(triple) as usize)
        .sum()
}

fn stage_unmapped(o: &Opened, unread: &Unmapped, work: &Working) -> Result<()> {
    let bytes = o
        .ontology
        .unmapped
        .iter()
        .map(|i| &o.ontology.triples[*i])
        .filter(|t| !is_rdfs_property(t))
        .chain(&unread.triples)
        .take(sparkles_owl::classify::UNMAPPED_LISTED)
        .map(triple_copy_bytes)
        .sum::<u64>();
    work.lock().grow(bytes * 2)
}

fn is_rdfs_property(t: &Triple) -> bool {
    t.predicate.as_str() == vocab::RDF_TYPE
        && matches!(&t.object, Term::NamedNode(n) if n.as_str() == vocab::RDF_PROPERTY)
}

/// The triples that map to no axiom of a classification of `o`: the terminology's, then
/// the assertion triples a run read (`unread`), then the one at which a stream of the
/// assertions that the run did not read stopped (`seen`). A typing as `rdf:Property`
/// that the read left unmapped, which no OWL type accompanies, as RDFS vocabularies type
/// their properties, carries no logical content, so the classification does not count
/// it; the read still reports it, as the profile checks do.
pub(super) fn unmapped_of(
    o: &sparkles_owl::read::Ontology,
    unread: &Unmapped,
    seen: &RefCell<Option<Triple>>,
) -> Unmapped {
    Unmapped::default()
        .extend(
            o.unmapped
                .iter()
                .map(|i| &o.triples[*i])
                .filter(|t| !is_rdfs_property(t)),
        )
        .merge(unread)
        .extend(seen.borrow().as_ref())
}

/// Runs a classification on the threads of `par` under the options' deadline,
/// cancellation and memory budget, with the rules, the partial answer and the expansion
/// strategy that `c` asks for.
///
/// The modules' pass ([`Classification::cover`]) runs after the saturation under the same
/// bounds, with the `dropped` axioms, and answers the one of them it reports as left out.
/// The triples that `unmapped` then counts are left out too.
#[allow(clippy::too_many_arguments)]
fn run(
    k: &mut Saturation,
    axioms: &[&AxiomKind],
    changes: Option<Changes<'_>>,
    par: Par,
    opts: &QueryOptions,
    c: &ClassifyRequest,
    dropped: Option<&mut Dropped<'_>>,
    unmapped: &dyn Fn() -> Unmapped,
    work: &Working,
    reused_credit: u64,
) -> Result<(Classification, Option<Excluded>)> {
    bounded(par, opts, work, reused_credit, |limits| {
        let limits = Limits {
            el_only: c.el_only,
            partial: c.partial,
            expansion: c.expansion,
            coverage: !c.skip_coverage,
            cover_later: true,
            ..limits.clone()
        };
        let mut out = k.classify_with_credit(axioms, changes, &limits, reused_credit as usize)?;
        let witness = out.cover(axioms, k.excluded(), dropped)?;
        out.leave_out_unmapped(unmapped())?;
        Ok((out.into_classification(), witness))
    })
}

/// Saturates without a taxonomy, under the bounds [`run`] sets.
#[allow(clippy::too_many_arguments)]
pub(super) fn saturate(
    k: &mut Saturation,
    axioms: &[&AxiomKind],
    changes: Option<Changes<'_>>,
    par: Par,
    el_only: bool,
    opts: &QueryOptions,
    work: &Working,
    reused_credit: u64,
) -> Result<sparkles_owl::classify::Saturated> {
    bounded(par, opts, work, reused_credit, |limits| {
        let limits = Limits {
            el_only,
            ..limits.clone()
        };
        k.saturate_with_credit(axioms, changes, &limits, reused_credit as usize)
    })
}

/// Calls `f` on the threads of `par` with the limits of `opts`: its deadline,
/// cancellation and memory budget.
fn bounded<T>(
    par: Par,
    opts: &QueryOptions,
    memory: &Working,
    reused_credit: u64,
    f: impl FnOnce(&Limits) -> std::result::Result<T, sparkles_owl::classify::Error>,
) -> Result<T> {
    let cancel = opts.cancel.clone();
    let cancellation = opts.cancellation.clone();
    // The classifier reports growth of its admitted peak. Own that debit here so
    // success, refusal and unwind all restore the enclosing grant's live usage.
    let max_memory = {
        let memory = memory.lock();
        memory
            .remaining()
            .map(|remaining| remaining.saturating_add(reused_credit) as usize)
    };
    let memory = memory.clone();
    let pooled = opts.grant.is_some() || opts.pool.is_some();
    let limits = Limits {
        el_only: false,
        threads: par.threads(),
        deadline: opts.deadline,
        max_memory,
        cancelled: Some(Arc::new(move || {
            cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed))
                || cancellation.as_ref().is_some_and(|c| c.check().is_err())
        })),
        charge: (pooled || max_memory.is_some()).then(|| {
            Arc::new(move |bytes: usize| memory.lock().grow(bytes as u64).is_ok())
                as Arc<dyn Fn(usize) -> bool + Send + Sync>
        }),
        partial: false,
        expansion: Default::default(),
        ..Limits::default()
    };
    let out = f(&limits);
    drop(par);
    out.map_err(|e| match e {
        sparkles_owl::classify::Error::Cancelled => Error::Cancelled,
        sparkles_owl::classify::Error::Timeout => Error::Timeout,
        sparkles_owl::classify::Error::Incomplete(excluded) => {
            incomplete(sparkles_owl::classify::refusal(&excluded), &excluded)
        }
        sparkles_owl::classify::Error::Memory { limit, used } => {
            let over_budget = max_memory.is_some_and(|m| used > m);
            Error::BudgetExceeded(Budget {
                kind: if over_budget || !pooled {
                    BudgetKind::Memory
                } else {
                    BudgetKind::ServerMemory
                },
                limit: limit as u64,
                requested: used as u64,
            })
        }
    })
}

#[cfg(test)]
#[test]
fn retained_saturation_shrinks_to_its_current_size() {
    let dataset = crate::Dataset::memory();
    let ontology = dataset.ontology();
    let request = OntologyRequest::default();
    let snapshot = dataset.store().snapshot();
    let mut entry = CacheEntry::new(InputKey::of(&ontology, &request, &snapshot), false);
    let pool =
        sparkles_core::sparql::pool::QueryPool::with_retained_capacity(8 << 20, 4 << 20).unwrap();
    entry.retained_bytes = 1 << 20;
    assert!(entry.admit_retention(Some(&pool)));
    assert!(pool.domain_stats().retained_reserved >= 1 << 20);
    entry.retained_bytes = 0;
    assert!(entry.admit_retention(Some(&pool)));
    assert_eq!(pool.domain_stats().retained_reserved, 0);
}
