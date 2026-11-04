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

use super::{
    EditOutcome, EditRequest, Extra, INFERRED_GRAPH, Ontologies, OntologyRequest, Opened, check,
};
use crate::error::{Error, Result};
use crate::sparql::QueryOptions;
use crate::store::Snapshot;
use crate::{Budget, BudgetKind};
use oxrdf::{Literal, NamedNode, Term};
use rustc_hash::FxHashSet;
use sparkles_core::algo::Control;
use sparkles_core::algo::pool::Par;
use sparkles_owl::classify::{
    Changes, Classification, Excluded, Expansion, Limits, Saturation, logical,
};
use sparkles_owl::read::Mode;
use sparkles_owl::{
    AnnotationProperty, AnnotationValue, Axiom, AxiomKind, ClassExpression, Entity, vocab,
};
use sparkles_reasoner::inputs::{GraphRef, ImportMode};
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
    /// ([`sparkles_owl::classify::Limits::el_only`]). Such a run does not use the
    /// dataset's kept classification.
    pub el_only: bool,
    /// a run with the disjunctive rules that its deadline, cancellation or memory budget
    /// stops answers with what it proved, a [`Classification`] whose
    /// [`complete`](Classification::complete) is false, instead of failing
    /// ([`sparkles_owl::classify::Limits::partial`]). A run of the other tiers fails as
    /// before. A classification that writes its taxonomy refuses it.
    pub partial: bool,
    /// the expansion strategy of the disjunctive rules, which gives the same answer
    /// either way ([`sparkles_owl::classify::Limits::expansion`])
    pub expansion: Expansion,
}

/// The opening of the comment the classifier writes on a taxonomy graph's header, by
/// which a later write knows the comment it replaces.
const COMMENT: &str = "The OWL 2 EL taxonomy that the Sparkles classifier computed from commit ";

/// A classification.
#[derive(Clone)]
pub struct Classified {
    pub opened: Opened,
    pub classification: Arc<Classification>,
    /// the axioms left out, as outside OWL 2 EL, or used only in part
    pub excluded: Arc<Vec<Excluded>>,
    /// whether the dataset's kept classification of the same state answered
    pub reused: bool,
    /// the edit that wrote the taxonomy to the target graph
    pub written: Option<EditOutcome>,
}

impl Classified {
    /// Whether every subsumption between named classes that the ontology entails is in
    /// the taxonomy: true when no axiom is left out or used only in part and the run was
    /// not stopped part way ([`ClassifyRequest::partial`]).
    pub fn complete(&self) -> bool {
        self.excluded.is_empty() && self.classification.complete
    }
}

/// The classification the dataset keeps: the saturated contexts and the open ontology
/// they were saturated from, whose axioms the saturation reads in place rather than
/// copying them.
pub struct CacheEntry {
    key: String,
    commit: u64,
    ontology: Option<Arc<sparkles_owl::read::Ontology>>,
    /// the axioms beyond the ontology's that the saturation last read, such as a DL
    /// query's defining axiom, which the next run removes unless it reads them again
    extra: Vec<AxiomKind>,
    pub(super) saturation: Saturation,
    /// the taxonomy of the ontology at `commit`, once a classification computed it,
    /// which a DL query's run over the saturation leaves as it is
    result: Option<(Arc<Classification>, Arc<Vec<Excluded>>)>,
}

impl CacheEntry {
    /// The estimated bytes of the kept taxonomy, 0 when none is kept.
    pub(super) fn taxonomy_memory(&self) -> usize {
        self.result
            .as_ref()
            .map_or(0, |(cl, _)| cl.stats.taxonomy_memory)
    }

    /// Lets the kept taxonomy go, so that the next classification builds it again.
    pub(super) fn drop_taxonomy(&mut self) {
        self.result = None;
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
        let (snap, opts) = self.state(req, opts)?;
        let o = self.open_at(&snap, req, &opts)?;
        let keep = opts.graphs.is_none() && snap.cacheable && !c.assertions && !c.el_only;
        let (classification, excluded, reused) = if keep {
            self.classify_kept(&o, req, c, &opts)?
        } else {
            let mut assertions: Vec<Axiom> = Vec::new();
            if c.assertions {
                o.assertions(&opts, |ax, _, _| {
                    if let Some(ax) = ax {
                        assertions.push(ax);
                    }
                    Ok(true)
                })?;
            }
            let mut axioms = logical_axioms(&o.ontology);
            axioms.extend(assertions.iter().map(|a| &a.kind).filter(|k| logical(k)));
            let (out, excluded) = classify_once(&axioms, c, &opts)?;
            (Arc::new(out), Arc::new(excluded), false)
        };
        if let Some(about) = &c.about
            && classification
                .taxonomy
                .node_of(&sparkles_owl::Class::new(about.as_str()))
                .is_none()
        {
            return Err(Error::NotFound(format!(
                "the taxonomy has no class <{about}>"
            )));
        }
        let mut classified = Classified {
            opened: o,
            classification,
            excluded,
            reused,
            written: None,
        };
        if let Some(target) = &c.target {
            classified.written = Some(self.write_taxonomy(req, &classified, target, c, &opts)?);
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
    ) -> Result<(Arc<Classification>, Arc<Vec<Excluded>>, bool)> {
        let commit = o.snapshot.commit;
        self.with_kept(req, opts, |e| {
            if e.commit == commit
                && let Some((cl, ex)) = &e.result
            {
                return Ok(((cl.clone(), ex.clone(), true), true));
            }
            // the threads come first: a wait that ends at the deadline leaves the kept
            // state as it was
            let par = admit(c.threads, opts)?;
            let out = e.run_over(o, Vec::new(), |s, axioms, changes| {
                run(s, axioms, changes, par, opts, c)
            })?;
            let cl = Arc::new(out);
            let ex = Arc::new(e.saturation.excluded().to_vec());
            let keep = cl.stats.memory <= CACHE_MAX_BYTES;
            e.commit = commit;
            // a partial answer is never kept, so that no later call reuses it
            e.result = (keep && cl.complete).then(|| (cl.clone(), ex.clone()));
            Ok(((cl, ex, false), keep))
        })
    }

    /// Runs `f` on the dataset's kept classification of the ontology `req` names: its
    /// saturation, which [`CacheEntry::run_over`] brings to an open ontology's axioms,
    /// and the taxonomy of the commit it last classified. One caller at a time holds it,
    /// and a second waits under its own deadline. `f` answers whether the dataset keeps
    /// the entry afterwards, and an error from `f` keeps it. Only callers who read every
    /// graph of a state that can be kept use it.
    pub(super) fn with_kept<T>(
        &self,
        req: &OntologyRequest,
        opts: &QueryOptions,
        f: impl FnOnce(&mut CacheEntry) -> Result<(T, bool)>,
    ) -> Result<T> {
        let key = format!("{:?}|{:?}|{}", req.graphs(), req.imports, req.mode.as_str());
        let state = self.ds.state();
        // one run at a time: a second caller waits, then reads the first one's answer
        let _flight = loop {
            if let Some(g) = state
                .classify_flight
                .try_lock_for(Duration::from_millis(50))
            {
                break g;
            }
            check(opts)?;
        };
        let kept = state.classify_cache.lock().take();
        let mut e = match kept {
            Some(e) if e.key == key => e,
            _ => CacheEntry {
                key,
                commit: 0,
                ontology: None,
                extra: Vec::new(),
                saturation: Saturation::new(),
                result: None,
            },
        };
        match f(&mut e) {
            Ok((out, keep)) => {
                if keep {
                    *state.classify_cache.lock() = Some(e);
                }
                Ok(out)
            }
            Err(err) => {
                *state.classify_cache.lock() = Some(e);
                Err(err)
            }
        }
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
    pub(super) static REFUSE_ADMISSION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
pub(crate) fn classify_once(
    axioms: &[&AxiomKind],
    c: &ClassifyRequest,
    opts: &QueryOptions,
) -> Result<(Classification, Vec<Excluded>)> {
    let mut s = Saturation::new();
    let par = admit(c.threads, opts)?;
    let out = run(&mut s, axioms, None, par, opts, c)?;
    Ok((out, s.excluded().to_vec()))
}

/// Runs a classification on the threads of `par` under the options' deadline,
/// cancellation and memory budget, with the rules, the partial answer and the expansion
/// strategy that `c` asks for.
fn run(
    k: &mut Saturation,
    axioms: &[&AxiomKind],
    changes: Option<Changes<'_>>,
    par: Par,
    opts: &QueryOptions,
    c: &ClassifyRequest,
) -> Result<Classification> {
    bounded(par, opts, |limits| {
        let limits = Limits {
            el_only: c.el_only,
            partial: c.partial,
            expansion: c.expansion,
            ..limits.clone()
        };
        k.classify(axioms, changes, &limits)
    })
}

/// Saturates without a taxonomy, under the bounds [`run`] sets.
pub(super) fn saturate(
    k: &mut Saturation,
    axioms: &[&AxiomKind],
    changes: Option<Changes<'_>>,
    par: Par,
    opts: &QueryOptions,
) -> Result<sparkles_owl::classify::Saturated> {
    bounded(par, opts, |limits| k.saturate(axioms, changes, limits))
}

/// Calls `f` on the threads of `par` with the limits of `opts`: its deadline,
/// cancellation and memory budget.
fn bounded<T>(
    par: Par,
    opts: &QueryOptions,
    f: impl FnOnce(&Limits) -> std::result::Result<T, sparkles_owl::classify::Error>,
) -> Result<T> {
    let cancel = opts.cancel.clone();
    let cancellation = opts.cancellation.clone();
    let grant = opts.grant.clone();
    let limits = Limits {
        el_only: false,
        threads: par.threads(),
        deadline: opts.deadline,
        max_memory: opts.max_memory_bytes.map(|m| m as usize),
        cancelled: Some(Arc::new(move || {
            cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed))
                || cancellation.as_ref().is_some_and(|c| c.check().is_err())
        })),
        charge: grant.map(|g| {
            Arc::new(move |bytes: usize| g.charge(bytes as u64).is_ok())
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
        sparkles_owl::classify::Error::Memory { limit, used } => {
            let over_budget = opts.max_memory_bytes.is_some_and(|m| used as u64 > m);
            Error::BudgetExceeded(Budget {
                kind: if over_budget || opts.grant.is_none() {
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
