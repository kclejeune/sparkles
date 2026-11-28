//! DL queries over an open ontology (spec U12): the equivalent classes, the direct and
//! all superclasses and subclasses, and the instances of a class expression, as
//! Protégé's DL Query tab answers them.
//!
//! The query names a fresh class equivalent to the expression. Inside OWL 2 EL it is
//! answered by the EL classifier of [`sparkles_owl::classify`]: the dataset's kept
//! saturation reads the ontology's axioms in place with the defining axiom beside them,
//! saturates again only what the axiom and any change since its last run reach, and
//! gives the answer from its saturated contexts without building a taxonomy. The next run
//! over it lets the axiom go, so the ontology's own classification is not repeated. The class answers read the terminology, as the kept
//! classification does, and the instances come from a classification with the
//! assertions, which the dataset does not keep. Outside OWL 2 EL the query is answered
//! under the OWL 2 RL rules by demand reasoning, which derives only what the query's
//! patterns need, over a copy in memory of the ontology with the defining axiom. Every
//! answer says which reasoning gave it and what it may be missing, as a classification
//! reports the axioms it uses in part.
//!
//! ```
//! use sparkles::Dataset;
//! use sparkles::handles::ontology::{OntologyRequest, dl_query::DlQueryRequest};
//! use sparkles::owl::{Class, ClassExpression, ObjectProperty};
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
//!      }",
//! )?;
//! let q = DlQueryRequest::new(ClassExpression::some(
//!     ObjectProperty::new("http://ex.org/partOf"),
//!     Class::new("http://ex.org/Cell"),
//! ));
//! let a = ds.ontology().dl_query(&OntologyRequest::default(), &q, &Default::default())?;
//! assert_eq!(a.subclasses, Some(vec![Class::new("http://ex.org/Nucleus")]));
//! assert!(a.reasoning.complete);
//! # Ok::<_, sparkles::Error>(())
//! ```

use super::classify::{Working, admit, logical_axioms, saturate, working};
use super::{Ontologies, OntologyRequest, Opened, check};
use crate::Dataset;
use crate::error::{Error, Result};
use crate::reasoning::{Demand, ReasoningMode, memory::Memory};
use crate::sparql::QueryOptions;
use crate::store::Snapshot;
use oxrdf::{GraphName, Term};
use sparkles_core::algo::pool::Par;
use sparkles_owl::classify::{Changes, Description, Excluded, Gap, Saturation};
use sparkles_owl::manchester::{self, Parsed, Vocabulary, What};
use sparkles_owl::profiles::{self, Profile};
use sparkles_owl::{Axiom, AxiomKind, Class, ClassExpression, EntityKind, Individual, write};
use std::collections::BTreeSet;
use std::fmt::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The fresh class a query defines as equivalent to its expression.
pub const QUERY_CLASS: &str = "urn:x-sparkles:dl-query";

/// The reasoning a query asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DlReasoning {
    /// the EL classifier when the expression and the terminology are inside OWL 2 EL,
    /// and the OWL 2 RL rules otherwise
    #[default]
    Auto,
    /// the EL classifier, which leaves out the axioms outside OWL 2 EL and reports them;
    /// an expression outside OWL 2 EL is refused
    El,
    /// the OWL 2 RL rules by demand reasoning
    Rl,
}

impl DlReasoning {
    pub fn parse(s: &str) -> Option<DlReasoning> {
        match s {
            "auto" => Some(DlReasoning::Auto),
            "el" => Some(DlReasoning::El),
            "rl" => Some(DlReasoning::Rl),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DlReasoning::Auto => "auto",
            DlReasoning::El => "el",
            DlReasoning::Rl => "rl",
        }
    }
}

/// The reasoning that answered a query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DlEngine {
    /// the OWL 2 EL classifier
    El,
    /// the OWL 2 RL rules by demand reasoning
    Rl,
}

impl DlEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            DlEngine::El => "el",
            DlEngine::Rl => "rl",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            DlEngine::El => "OWL 2 EL classifier",
            DlEngine::Rl => "OWL 2 RL rules by demand reasoning",
        }
    }
}

/// The answers a query asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DlAnswers {
    pub equivalents: bool,
    pub superclasses: bool,
    pub direct_superclasses: bool,
    pub subclasses: bool,
    pub direct_subclasses: bool,
    pub instances: bool,
}

impl Default for DlAnswers {
    fn default() -> DlAnswers {
        DlAnswers {
            equivalents: true,
            superclasses: true,
            direct_superclasses: true,
            subclasses: true,
            direct_subclasses: true,
            instances: true,
        }
    }
}

impl DlAnswers {
    /// The names of the answers, as the HTTP API and the bindings write them.
    pub const NAMES: [&'static str; 6] = [
        "equivalents",
        "superclasses",
        "directSuperclasses",
        "subclasses",
        "directSubclasses",
        "instances",
    ];

    /// No answers; [`set`](Self::set) adds them.
    pub fn none() -> DlAnswers {
        DlAnswers {
            equivalents: false,
            superclasses: false,
            direct_superclasses: false,
            subclasses: false,
            direct_subclasses: false,
            instances: false,
        }
    }

    /// Asks for the answer named `name`, one of [`NAMES`](Self::NAMES).
    pub fn set(&mut self, name: &str) -> Result<()> {
        match name {
            "equivalents" => self.equivalents = true,
            "superclasses" => self.superclasses = true,
            "directSuperclasses" => self.direct_superclasses = true,
            "subclasses" => self.subclasses = true,
            "directSubclasses" => self.direct_subclasses = true,
            "instances" => self.instances = true,
            _ => {
                return Err(Error::invalid(format!(
                    "unknown answer '{name}': {}",
                    Self::NAMES.join(", ")
                )));
            }
        }
        Ok(())
    }

    /// Whether any answer about classes is asked for.
    pub fn classes(&self) -> bool {
        self.equivalents
            || self.superclasses
            || self.direct_superclasses
            || self.subclasses
            || self.direct_subclasses
    }
}

/// A DL query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DlQueryRequest {
    pub expression: ClassExpression,
    pub reasoning: DlReasoning,
    pub answers: DlAnswers,
    /// the worker threads of a classification (0: what the algorithm pool gives)
    pub threads: usize,
    /// the most facts demand reasoning derives for one pattern, or its default
    pub max_derived: Option<usize>,
    /// classify by the OWL 2 EL rules alone, as ELK does, leaving out every axiom
    /// outside OWL 2 EL and using the dataset's EL-only kept saturation
    pub el_only: bool,
}

impl DlQueryRequest {
    /// A query of `expression` for every answer, with automatic reasoning.
    pub fn new(expression: ClassExpression) -> DlQueryRequest {
        DlQueryRequest {
            expression,
            reasoning: DlReasoning::Auto,
            answers: DlAnswers::default(),
            threads: 0,
            max_derived: None,
            el_only: false,
        }
    }
}

/// An axiom the reasoning leaves out or uses only in part, or the query expression
/// itself when it falls outside the profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DlGap {
    /// the axiom, or `None` for a gap of the query expression
    pub axiom: Option<Axiom>,
    /// the rule of the OWL 2 Profiles recommendation that leaves it out
    pub rule: Option<String>,
    pub message: String,
    /// true for an axiom used in part, false for one left out
    pub partial: bool,
}

/// What reasoning answered a query and how complete its answers are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DlReasoningUsed {
    pub engine: DlEngine,
    /// why automatic reasoning chose the engine, or that the caller named it
    pub why: String,
    /// whether the classifier ran in EL-only (ELK) mode
    pub el_only: bool,
    /// whether the class answers are every entailed one
    pub complete: bool,
    /// whether the instances are every entailed one, when they were asked for
    pub instances_complete: Option<bool>,
    /// what the reasoning left out or used in part
    pub gaps: Vec<DlGap>,
    /// what else limits the answers
    pub notes: Vec<String>,
}

/// The answer of a DL query. Each list the query asked for is `Some`, sorted by IRI.
#[derive(Clone)]
pub struct DlQueryAnswer {
    pub expression: ClassExpression,
    pub reasoning: DlReasoningUsed,
    /// false when the expression is unsatisfiable; `None` when the reasoning cannot tell
    pub satisfiable: Option<bool>,
    /// false when the ontology, with its assertions when instances were asked for, has
    /// no model; `None` when the reasoning cannot tell
    pub consistent: Option<bool>,
    pub equivalents: Option<Vec<Class>>,
    pub superclasses: Option<Vec<Class>>,
    pub direct_superclasses: Option<Vec<Class>>,
    pub subclasses: Option<Vec<Class>>,
    pub direct_subclasses: Option<Vec<Class>>,
    pub instances: Option<Vec<Individual>>,
    /// whether the dataset's kept saturation answered the class answers, saturating
    /// again only what the query reaches
    pub incremental: bool,
    pub elapsed: Duration,
    pub opened: Opened,
}

/// A DL answer that holds its request memory credit. Clones share the answer's
/// backing and credit. Its fields can be borrowed through this handle.
#[derive(Clone)]
pub struct AdmittedDlQueryAnswer(Arc<DlQueryBacking>);

struct DlQueryBacking {
    value: DlQueryAnswer,
    _memory: [Option<Memory>; 2],
}

impl AdmittedDlQueryAnswer {
    fn into_value(self) -> DlQueryAnswer {
        Arc::into_inner(self.0).expect("unshared DL answer").value
    }
}

impl std::ops::Deref for AdmittedDlQueryAnswer {
    type Target = DlQueryAnswer;

    fn deref(&self) -> &DlQueryAnswer {
        &self.0.value
    }
}

/// The query's defining axiom.
fn defining(e: &ClassExpression) -> Axiom {
    Axiom::equivalent_classes([Class::new(QUERY_CLASS).into(), e.clone()])
}

fn logical(k: &AxiomKind) -> bool {
    !matches!(
        k,
        AxiomKind::AnnotationAssertion(..)
            | AxiomKind::SubAnnotationPropertyOf(..)
            | AxiomKind::AnnotationPropertyDomain(..)
            | AxiomKind::AnnotationPropertyRange(..)
            | AxiomKind::Declaration(_)
    )
}

fn gap_of(e: &Excluded) -> DlGap {
    let axiom = Some(Axiom::new(e.axiom.clone()));
    match &e.gap {
        Gap::Outside { rule, message } => DlGap {
            axiom,
            rule: Some(rule.clone()),
            message: message.clone(),
            partial: false,
        },
        Gap::Partial(why) => DlGap {
            axiom,
            rule: None,
            message: format!("used in part: {why}"),
            partial: true,
        },
    }
}

fn is_query_axiom(k: &AxiomKind) -> bool {
    Axiom::new(k.clone()).mentions(QUERY_CLASS)
}

/// The gaps of a classifier's report, with the query's own axiom reported as a gap of
/// the expression.
fn gaps_of(excluded: &[Excluded]) -> Vec<DlGap> {
    excluded
        .iter()
        .map(|e| {
            let mut g = gap_of(e);
            if is_query_axiom(&e.axiom) {
                g.axiom = None;
                g.message = format!("the query expression is {}", g.message);
            }
            g
        })
        .collect()
}

impl Ontologies {
    /// The vocabulary that Manchester text about `o` is read against: the dataset's
    /// prefixes and the entities and labels of `o`.
    pub fn vocabulary(&self, o: &Opened) -> Vocabulary {
        Vocabulary::of_ontology(&o.ontology, self.ds.prefixes())
    }

    /// Read Manchester `text` as `what` against the ontology `o` (spec U12). A text
    /// that does not parse is an [`Error::Component`] of the `owl` component with the
    /// code `invalid-manchester`, whose source is the [`manchester::Error`] with its
    /// span and its candidates.
    ///
    /// Individuals that only the store's assertions name, such as the objects of
    /// property assertions, are not entities of `o`. A name the vocabulary does not know
    /// or one known only as another kind is therefore looked for among the entities of
    /// the assertions the caller can read,
    /// which streams them once per such name, up to eight names. Only the entities the
    /// name can mean are kept, so the vocabulary grows by a few entries, and a text that
    /// reads costs no pass over the assertions.
    pub fn parse_manchester(
        &self,
        o: &Opened,
        text: &str,
        what: What,
        opts: &QueryOptions,
    ) -> Result<Parsed> {
        let mut v = self.vocabulary(o);
        let mut tries = 0;
        loop {
            let e = match manchester::parse(text, what, &v) {
                Ok(p) => return Ok(p),
                Err(e) => e,
            };
            // an unknown name, or a known one of another kind, such as an individual
            // punned with a class
            let retry = matches!(
                e.kind,
                manchester::ErrorKind::Unknown | manchester::ErrorKind::Kind
            );
            if !retry || tries == 8 {
                return Err(manchester_error(e, text));
            }
            tries += 1;
            let token = text.get(e.span.start..e.span.end).unwrap_or("");
            let meant = v.meant(token);
            let mut seen = rustc_hash::FxHashSet::default();
            let mut found = Vec::new();
            o.assertions(opts, |a, _, _| {
                if let Some(a) = a {
                    for ent in a.signature() {
                        if found.len() == RETRY_ENTITIES {
                            break;
                        }
                        let iri = ent.iri().as_str();
                        if !ent.is_builtin()
                            && meant.matches(iri)
                            && !v.kinds(iri).contains(&ent.kind())
                            && seen.insert((iri.to_string(), ent.kind()))
                        {
                            found.push(ent);
                        }
                    }
                }
                Ok(found.len() < RETRY_ENTITIES)
            })?;
            if found.is_empty() {
                return Err(manchester_error(e, text));
            }
            for ent in found {
                v.add_entity(ent);
            }
        }
    }

    /// Answer a DL query over the ontology `req` names (spec U12): the equivalent
    /// classes, the superclasses and subclasses and the instances of `q.expression`, as
    /// the [module documentation](self) describes. The returned value is owned by
    /// the application. Use [Self::dl_query_admitted] to retain the answer credit.
    pub fn dl_query(
        &self,
        req: &OntologyRequest,
        q: &DlQueryRequest,
        opts: &QueryOptions,
    ) -> Result<DlQueryAnswer> {
        self.dl_query_admitted(req, q, opts)
            .map(AdmittedDlQueryAnswer::into_value)
    }

    /// Answer a DL query while retaining credit for the answer's owned data.
    pub fn dl_query_admitted(
        &self,
        req: &OntologyRequest,
        q: &DlQueryRequest,
        opts: &QueryOptions,
    ) -> Result<AdmittedDlQueryAnswer> {
        req.validate()?;
        if req.inferences {
            return Err(Error::invalid(
                "a DL query reasons over the asserted axioms: leave out 'inferences'",
            ));
        }
        let opts = self.anchored(opts)?;
        let o = self.open(req, &opts)?;
        self.dl_query_opened_admitted(o, req, q, &opts)
    }

    /// [`dl_query`](Self::dl_query) over an ontology the caller opened with `req` and
    /// `opts`, such as one it read the expression against. The returned value
    /// is owned by the application.
    pub fn dl_query_opened(
        &self,
        o: Opened,
        req: &OntologyRequest,
        q: &DlQueryRequest,
        opts: &QueryOptions,
    ) -> Result<DlQueryAnswer> {
        self.dl_query_opened_admitted(o, req, q, opts)
            .map(AdmittedDlQueryAnswer::into_value)
    }

    /// Answer over a previously opened ontology while retaining the answer credit.
    pub fn dl_query_opened_admitted(
        &self,
        o: Opened,
        req: &OntologyRequest,
        q: &DlQueryRequest,
        opts: &QueryOptions,
    ) -> Result<AdmittedDlQueryAnswer> {
        let start = Instant::now();
        self.check_pinned_opened(&o, req)?;
        let defaults = self.ds.with_query_defaults(opts);
        let opts = defaults.as_ref();
        if req.inferences {
            return Err(Error::invalid(
                "a DL query reasons over the asserted axioms: leave out 'inferences'",
            ));
        }
        if !o.ontology.kinds(QUERY_CLASS).is_empty() {
            return Err(Error::invalid(format!(
                "the ontology uses <{QUERY_CLASS}>, the class a DL query defines"
            )));
        }
        let mut preparation = crate::reasoning::memory::Memory::query_after(opts, o.request_bytes);
        let expression_bytes =
            sparkles_owl::classify::class_expression_heap_bytes(&q.expression) as u64;
        let defining_bytes = 1024 + expression_bytes;
        preparation.grow(defining_bytes)?;
        let ax = defining(&q.expression);
        preparation.grow(profile_scratch(&ax))?;
        let expression_el = profiles::axiom_violations(&ax, Profile::El);
        if q.el_only && q.reasoning == DlReasoning::Rl {
            return Err(Error::invalid(
                "elOnly applies to the classifier, not reasoning=rl",
            ));
        }
        let (engine, why) = match q.reasoning {
            DlReasoning::El => {
                if let Some((rule, message)) = expression_el.first() {
                    return Err(Error::invalid(if q.el_only {
                        format!(
                            "the expression is outside OWL 2 EL ({rule}: {message}); leave out elOnly and use reasoning=auto or rl"
                        )
                    } else {
                        format!(
                            "the expression is outside OWL 2 EL ({rule}: {message}); ask for reasoning=rl or auto"
                        )
                    }));
                }
                (
                    DlEngine::El,
                    if q.el_only {
                        "the caller asked for the EL classifier in EL-only mode"
                    } else {
                        "the caller asked for the EL classifier"
                    }
                    .to_string(),
                )
            }
            DlReasoning::Rl => (
                DlEngine::Rl,
                "the caller asked for the OWL 2 RL rules".to_string(),
            ),
            DlReasoning::Auto if q.el_only => {
                if let Some((rule, message)) = expression_el.first() {
                    return Err(Error::invalid(format!(
                        "the expression is outside OWL 2 EL ({rule}: {message}); leave out elOnly and use reasoning=auto or rl"
                    )));
                }
                (
                    DlEngine::El,
                    "the caller asked for EL-only mode".to_string(),
                )
            }
            DlReasoning::Auto => {
                if let Some((rule, message)) = expression_el.first() {
                    (
                        DlEngine::Rl,
                        format!("the expression is outside OWL 2 EL ({rule}: {message})"),
                    )
                } else {
                    let mut outside = 0;
                    for axiom in o.ontology.axiom_set().filter(|axiom| logical(&axiom.kind)) {
                        check(opts)?;
                        let base = preparation.bytes();
                        preparation.grow(profile_scratch(axiom))?;
                        outside +=
                            usize::from(!profiles::axiom_violations(axiom, Profile::El).is_empty());
                        preparation.shrink_to(base);
                    }
                    if outside > 0 {
                        (
                            DlEngine::Rl,
                            format!(
                                "{outside} axiom{} of the ontology {} outside OWL 2 EL",
                                if outside == 1 { "" } else { "s" },
                                if outside == 1 { "is" } else { "are" }
                            ),
                        )
                    } else {
                        (
                            DlEngine::El,
                            "the expression and the ontology are inside OWL 2 EL".to_string(),
                        )
                    }
                }
            }
        };
        drop(expression_el);
        preparation.shrink_to(defining_bytes);
        let opts = QueryOptions {
            max_memory_bytes: opts
                .max_memory_bytes
                .map(|limit| limit.saturating_sub(defining_bytes)),
            ..opts.clone()
        };
        let mut a = match engine {
            DlEngine::El => self.el(&o, req, q, &ax, why, &opts)?,
            DlEngine::Rl => rl(o, q, &ax, why, &opts)?,
        };
        a.value.elapsed = start.elapsed();
        Ok(AdmittedDlQueryAnswer(Arc::new(a)))
    }

    fn el(
        &self,
        o: &Opened,
        req: &OntologyRequest,
        q: &DlQueryRequest,
        ax: &Axiom,
        why: String,
        opts: &QueryOptions,
    ) -> Result<DlQueryBacking> {
        use super::classify::{copy_excluded, stage_assertion, stage_axioms};
        let expression_bytes = 1024
            + why.len()
            + o.source_bytes()
            + o.closure
                .iter()
                .map(|edge| {
                    128 + edge.from.as_str().len()
                        + edge.iri.len()
                        + edge.graphs.iter().map(String::len).sum::<usize>()
                })
                .sum::<usize>()
            + sparkles_owl::classify::class_expression_heap_bytes(&q.expression);
        let mut expression_memory =
            crate::reasoning::memory::Memory::query_after(opts, o.request_bytes);
        expression_memory.grow(expression_bytes as u64)?;
        let opts = QueryOptions {
            max_memory_bytes: opts
                .max_memory_bytes
                .map(|limit| limit.saturating_sub(expression_bytes as u64)),
            ..opts.clone()
        };
        let opts = &opts;
        let work = working(opts, o);
        let mut retained = 0;
        let qc = Class::new(QUERY_CLASS);
        let mut answer = DlQueryAnswer {
            opened: o.clone(),
            expression: q.expression.clone(),
            reasoning: DlReasoningUsed {
                engine: DlEngine::El,
                why,
                el_only: q.el_only,
                complete: true,
                instances_complete: None,
                gaps: Vec::new(),
                notes: Vec::new(),
            },
            satisfiable: None,
            consistent: None,
            equivalents: None,
            superclasses: None,
            direct_superclasses: None,
            subclasses: None,
            direct_subclasses: None,
            instances: None,
            incremental: false,
            elapsed: Duration::ZERO,
        };
        // saturates `axioms` with the defining axiom and describes the query class, on
        // the threads of the request
        let describe = |s: &mut Saturation,
                        axioms: &[&AxiomKind],
                        changes: Option<Changes<'_>>,
                        par: Par,
                        reused_credit: u64|
         -> Result<(bool, Description, Vec<Excluded>, u64)> {
            let staged = work.lock().bytes();
            let r = saturate(
                s,
                axioms,
                changes,
                par,
                q.el_only,
                opts,
                &work,
                reused_credit,
            )?;
            work.lock().shrink_to(
                staged.saturating_add((r.stats.memory as u64).saturating_sub(reused_credit)),
            );
            work.lock().grow(s.description_memory() as u64)?;
            let d = s.describe(&qc).ok_or_else(|| {
                Error::invalid("the classifier holds no saturation of the query class")
            })?;
            Ok((
                r.consistent,
                d,
                copy_excluded(s, &work)?,
                r.stats.memory.saturating_add(s.cached_description_bytes()) as u64,
            ))
        };
        if q.answers.classes() {
            let keep = self.can_keep(o, opts)
                && opts.graphs.is_none()
                && o.snapshot.cacheable
                && self.may_replace_kept(&o.snapshot);
            // the kept saturation reads the defining axiom beside the ontology's, and
            // the next run over it lets the axiom go. A pinned run that the head passed
            // while it waited for the flight runs on its own.
            let kept = if keep {
                self.with_kept_flight(o, req, q.el_only, opts, "dlQuery", |e| {
                    // the threads come first: a wait that ends at the deadline leaves the
                    // kept state as it was
                    let par = admit(q.threads, opts)?;
                    let pool = self.ds.pool();
                    e.bind_work(pool, opts, &work);
                    let reused_credit = e.reused_credit();
                    e.stage_input(o, 1, &work)?;
                    stage_assertion(&work, ax)?;
                    let (c, d, ex, memory) =
                        e.run_over(o, vec![ax.kind.clone()], |s, axioms, changes| {
                            describe(s, axioms, changes, par, reused_credit)
                        })?;
                    let keep = super::kept::retain_dl(e, memory as usize)
                        && e.reconcile(
                            pool,
                            opts,
                            &work,
                            memory + super::kept::entry_aux_bytes(e) as u64,
                        );
                    Ok(((c, d, ex), keep))
                })?
            } else {
                None
            };
            let (consistent, d, excluded) = match kept {
                Some(k) => {
                    answer.incremental = true;
                    k
                }
                None => {
                    stage_axioms(&work, o.ontology.axioms.len() + 1)?;
                    let mut axioms = logical_axioms(&o.ontology);
                    axioms.push(&ax.kind);
                    let par = admit(q.threads, opts)?;
                    let (c, d, ex, _) = describe(&mut Saturation::new(), &axioms, None, par, 0)?;
                    (c, d, ex)
                }
            };
            answer.consistent = Some(consistent);
            answer.satisfiable = Some(d.satisfiable);
            retained += 1024 + sparkles_owl::classify::excluded_copy_bytes(&excluded) * 3;
            retained += [
                &d.equivalents,
                &d.superclasses,
                &d.direct_superclasses,
                &d.subclasses,
                &d.direct_subclasses,
            ]
            .iter()
            .flat_map(|classes| classes.iter())
            .map(|class| 48 + class.as_str().len())
            .sum::<usize>();
            work.lock().grow(retained as u64)?;
            answer.reasoning.gaps = gaps_of(&excluded);
            answer.reasoning.complete = excluded.is_empty();
            let a = &q.answers;
            answer.equivalents = a.equivalents.then_some(d.equivalents);
            answer.superclasses = a.superclasses.then_some(d.superclasses);
            answer.direct_superclasses = a.direct_superclasses.then_some(d.direct_superclasses);
            answer.subclasses = a.subclasses.then_some(d.subclasses);
            answer.direct_subclasses = a.direct_subclasses.then_some(d.direct_subclasses);
            if a.instances {
                answer.reasoning.notes.push(
                    "the class answers read the terminology, and the instances the terminology with the assertions"
                        .into(),
                );
            }
            work.lock().shrink_to(retained as u64);
        }
        if q.answers.instances {
            // the assertions take part in this classification only, which the dataset
            // does not keep
            let mut assertions: Vec<Axiom> = Vec::new();
            o.assertions_with_memory(opts, Some(&work), |a, _, _| {
                if let Some(a) = a {
                    stage_assertion(&work, &a)?;
                    assertions.push(a);
                }
                Ok(true)
            })?;
            // an individual that only a declaration names is an instance of owl:Thing,
            // which gives it a context of its own
            stage_axioms(&work, o.ontology.axioms.len() * 2 + assertions.len() + 1)?;
            let declared: Vec<Axiom> = o
                .ontology
                .entities(EntityKind::NamedIndividual)
                .into_iter()
                .filter_map(|e| match e {
                    sparkles_owl::Entity::NamedIndividual(i) => {
                        Some(Axiom::class_assertion(Class::thing(), Individual::Named(i)))
                    }
                    _ => None,
                })
                .collect();
            let mut axioms = logical_axioms(&o.ontology);
            axioms.extend(
                assertions
                    .iter()
                    .chain(&declared)
                    .map(|a| &a.kind)
                    .filter(|k| sparkles_owl::classify::logical(k)),
            );
            axioms.push(&ax.kind);
            let par = admit(q.threads, opts)?;
            let (consistent, d, excluded, _) =
                describe(&mut Saturation::new(), &axioms, None, par, 0)?;
            answer.consistent = Some(consistent);
            if answer.satisfiable.is_none() {
                answer.satisfiable = Some(d.satisfiable);
            }
            let added = d
                .instances
                .iter()
                .map(|i| 64 + i.as_str().len())
                .sum::<usize>()
                + sparkles_owl::classify::excluded_copy_bytes(&excluded) * 3
                + 256;
            work.lock().grow(added as u64)?;
            retained += added;
            answer.instances = Some(d.instances.into_iter().map(Individual::Named).collect());
            answer.reasoning.instances_complete = Some(excluded.is_empty());
            for g in gaps_of(&excluded) {
                if !answer.reasoning.gaps.contains(&g) {
                    answer.reasoning.gaps.push(g);
                }
            }
            if !q.answers.classes() {
                answer.reasoning.complete = excluded.is_empty();
            }
        }
        Ok(DlQueryBacking {
            value: answer,
            _memory: [
                Some(work.lock().split(retained as u64)?),
                Some(expression_memory),
            ],
        })
    }
}

/// Manchester text that does not parse, with the error: the source of the
/// [`Error::Component`] that [`Ontologies::parse_manchester`] answers.
#[derive(Debug)]
pub struct ManchesterFailure {
    pub text: String,
    pub error: manchester::Error,
}

impl ManchesterFailure {
    /// The JSON form of the error, with its span in UTF-16 code units of the text
    /// ([`manchester::Error::json`]).
    pub fn json(&self) -> serde_json::Value {
        self.error.json(&self.text)
    }
}

impl std::fmt::Display for ManchesterFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (start, end) = self.error.span.utf16(&self.text);
        write!(f, "{} (characters {start} to {end})", self.error.message)
    }
}

impl std::error::Error for ManchesterFailure {}

/// The library error of Manchester `text` that does not parse.
pub fn manchester_error(e: manchester::Error, text: &str) -> Error {
    let failure = ManchesterFailure {
        text: text.to_string(),
        error: e,
    };
    Error::Component(Box::new(crate::error::ComponentError {
        component: "owl",
        code: "invalid-manchester".into(),
        message: failure.to_string(),
        source: Some(Box::new(failure)),
    }))
}

/// The entities of the assertions that one retry of [`Ontologies::parse_manchester`]
/// adds at most for an unknown name. A name that more entities share reads as ambiguous
/// among the first ones the assertions hold.
const RETRY_ENTITIES: usize = 256;

/// The bytes a copy in memory of a triple takes, roughly, for the memory budget.
const COPY_BYTES_PER_TRIPLE: u64 = 256;

/// Bytes charged to a memory grant, released when it drops.
#[derive(Debug)]
pub(super) struct Charged(
    pub(super) std::sync::Arc<sparkles_core::sparql::pool::PoolGrant>,
    pub(super) u64,
);

impl Drop for Charged {
    fn drop(&mut self) {
        self.0.release(self.1);
    }
}

/// A query under the OWL 2 RL rules by demand reasoning, over a private copy of
/// the terminology with the defining axiom and, for instance answers, the assertions.
fn rl(
    o: Opened,
    q: &DlQueryRequest,
    ax: &Axiom,
    why: String,
    opts: &QueryOptions,
) -> Result<DlQueryBacking> {
    let work = working(opts, &o);
    let (count, bytes) = write::axiom_projection_cost(ax);
    work.lock().grow((2048 + count * 256 + bytes * 4) as u64)?;
    let mut quads = Vec::new();
    let mut push = |triple: &oxrdf::Triple| -> Result<()> {
        check(opts)?;
        let terms = sparkles_owl::read::triple_payload(triple) as u64;
        work.lock()
            .grow(COPY_BYTES_PER_TRIPLE + terms.saturating_mul(4))?;
        quads.push(triple.clone().in_graph(GraphName::DefaultGraph));
        Ok(())
    };
    for t in &o.ontology.triples {
        push(t)?;
    }
    let prefix = format!("dlq{}x", uuid::Uuid::new_v4().simple());
    for t in write::axiom_triples(ax, &mut write::counter(&prefix)) {
        push(&t)?;
    }
    if q.answers.instances {
        o.assertion_triples_with_memory(opts, Some(&work), |t, _| {
            push(t)?;
            Ok(true)
        })?;
    }
    let mem = Dataset::memory();
    mem.extend(quads.iter().map(|q| q.as_ref()))?;
    drop(quads);
    let view = mem.inference_view(ReasoningMode::Demand(Demand {
        profile: Some(sparkles_reasoner::Profile::OwlRl),
        max_derived: q.max_derived,
    }))?;
    let mut qo = view.query_options();
    qo.deadline = opts.deadline;
    qo.cancel = opts.cancel.clone();
    qo.cancellation = opts.cancellation.clone();
    qo.grant = opts.grant.clone();
    qo.pool = opts.pool.clone();
    qo.no_cache = true;
    let snapshot = private_rl_snapshot(&mem);

    let rdfs = sparkles_owl::vocab::RDFS_SUB_CLASS_OF;
    let nothing = sparkles_owl::vocab::OWL_NOTHING;
    let is_class = |iri: &str| {
        iri != QUERY_CLASS
            && (o.ontology.kinds(iri).contains(&EntityKind::Class)
                || iri == sparkles_owl::vocab::OWL_THING)
    };
    let a = &q.answers;
    let mut supers = Vec::new();
    let mut subs = Vec::new();
    if a.classes() {
        // Any class answer needs both directions to distinguish equivalence.
        for (pattern, target) in [
            (format!("<{QUERY_CLASS}> <{rdfs}> ?c"), &mut supers),
            (format!("?c <{rdfs}> <{QUERY_CLASS}>"), &mut subs),
        ] {
            rl_select(
                &snapshot,
                &qo,
                &work,
                &format!("SELECT DISTINCT ?c WHERE {{ {pattern} }}"),
                |row, overlap| {
                    if let Some(iri) =
                        named_iri(row.values()[0].as_ref()).filter(|iri| is_class(iri))
                    {
                        work.lock()
                            .grow_with_overlap((96 + iri.len()) as u64, overlap)?;
                        target.push(iri.to_string());
                    }
                    Ok(())
                },
            )?;
            target.sort_unstable();
            target.dedup();
        }
        subs.retain(|iri| iri != nothing);
    }

    work.lock()
        .grow(((supers.len() + subs.len()) * 3 * size_of::<&str>()) as u64)?;
    let equivalents: Vec<_> = supers
        .iter()
        .filter(|iri| subs.binary_search(iri).is_ok())
        .map(String::as_str)
        .collect();
    let strict_supers: Vec<_> = supers
        .iter()
        .filter(|iri| iri.as_str() != nothing && subs.binary_search(iri).is_err())
        .map(String::as_str)
        .collect();
    let strict_subs: Vec<_> = subs
        .iter()
        .filter(|iri| supers.binary_search(iri).is_err())
        .map(String::as_str)
        .collect();

    let mut retained = 2048
        + why.len() as u64
        + sparkles_owl::classify::class_expression_heap_bytes(&q.expression) as u64;
    work.lock().grow(retained)?;
    let mut instances = None;
    if a.instances {
        let mut values = Vec::new();
        rl_select(
            &snapshot,
            &qo,
            &work,
            &format!("SELECT DISTINCT ?x WHERE {{ ?x a <{QUERY_CLASS}> }}"),
            |row, overlap| {
                if let Some(iri) = named_iri(row.values()[0].as_ref()) {
                    let bytes = (96 + iri.len()) as u64;
                    work.lock().grow_with_overlap(bytes, overlap)?;
                    retained += bytes;
                    values.push(Individual::named(iri));
                }
                Ok(())
            },
        )?;
        values.sort();
        instances = Some(values);
    }
    let (gaps, instances_complete, gap_bytes) = rl_gaps(&o, q, &work, opts)?;
    retained += gap_bytes;
    let direct_supers = if a.direct_superclasses {
        Some(rl_direct(&snapshot, &qo, &work, &strict_supers, true)?)
    } else {
        None
    };
    let direct_subs = if a.direct_subclasses {
        Some(rl_direct(&snapshot, &qo, &work, &strict_subs, false)?)
    } else {
        None
    };
    let mut classes = |iris: Option<&[&str]>| -> Result<Option<Vec<Class>>> {
        iris.map(|iris| {
            let bytes = iris
                .iter()
                .map(|iri| size_of::<Class>() + iri.len())
                .sum::<usize>();
            work.lock().grow(bytes as u64)?;
            retained += bytes as u64;
            Ok(iris.iter().map(|&iri| Class::new(iri)).collect())
        })
        .transpose()
    };
    let equivalents_answer = classes(a.equivalents.then_some(equivalents.as_slice()))?;
    let supers_answer = classes(a.superclasses.then_some(strict_supers.as_slice()))?;
    let subs_answer = classes(a.subclasses.then_some(strict_subs.as_slice()))?;
    let direct_supers_answer = classes(direct_supers.as_deref())?;
    let direct_subs_answer = classes(direct_subs.as_deref())?;
    let notes = vec![
        "the OWL 2 RL rules derive subsumptions between classes only in part, so the class answers can miss entailed ones".to_string(),
        "OWL 2 RL cannot tell whether the expression is satisfiable or the ontology consistent".to_string(),
    ];
    let value = DlQueryAnswer {
        opened: o,
        expression: q.expression.clone(),
        reasoning: DlReasoningUsed {
            engine: DlEngine::Rl,
            why,
            el_only: false,
            complete: false,
            instances_complete,
            gaps,
            notes,
        },
        satisfiable: None,
        consistent: None,
        equivalents: equivalents_answer,
        superclasses: supers_answer,
        direct_superclasses: direct_supers_answer,
        subclasses: subs_answer,
        direct_subclasses: direct_subs_answer,
        instances,
        incremental: false,
        elapsed: Duration::ZERO,
    };
    Ok(DlQueryBacking {
        value,
        _memory: [Some(work.lock().split(retained)?), None],
    })
}

fn private_rl_snapshot(dataset: &Dataset) -> Arc<Snapshot> {
    // The private generation dies with the DL query, so no schema publication
    // can be reused. Exclude it from demand caches before session construction.
    let mut snapshot = dataset.store().snapshot();
    Arc::make_mut(&mut snapshot).cacheable = false;
    snapshot
}

fn rl_select(
    snapshot: &Arc<Snapshot>,
    opts: &QueryOptions,
    work: &Working,
    sparql: &str,
    mut visit: impl FnMut(crate::sparql::output::RowRef<'_>, u64) -> Result<()>,
) -> Result<()> {
    let opts = QueryOptions {
        max_memory_bytes: work.lock().remaining(),
        ..opts.clone()
    };
    let result = crate::sparql::query(snapshot.clone(), sparql, &opts)?;
    let rows = result.rows()?;
    let overlap = result.live_memory_bytes();
    for i in 0..rows.len() {
        check(&opts)?;
        visit(rows.row(i)?, overlap)?;
    }
    Ok(())
}

fn named_iri(term: Option<&Term>) -> Option<&str> {
    match term {
        Some(Term::NamedNode(iri)) => Some(iri.as_str()),
        _ => None,
    }
}

fn rl_direct<'a>(
    snapshot: &Arc<Snapshot>,
    opts: &QueryOptions,
    work: &Working,
    classes: &[&'a str],
    up: bool,
) -> Result<Vec<&'a str>> {
    if classes.len() < 2 {
        work.lock().grow(size_of_val(classes) as u64)?;
        return Ok(classes.to_vec());
    }
    let start = work.lock().bytes();
    let values_bytes = classes.iter().map(|iri| iri.len() + 3).sum::<usize>();
    work.lock().grow((values_bytes * 3 + 256) as u64)?;
    let mut values = String::with_capacity(values_bytes);
    for iri in classes {
        write!(values, "<{iri}> ").expect("writing to a String");
    }
    let rdfs = sparkles_owl::vocab::RDFS_SUB_CLASS_OF;
    let mut pairs = BTreeSet::new();
    rl_select(
        snapshot,
        opts,
        work,
        &format!(
            "SELECT ?a ?b WHERE {{ VALUES ?a {{ {values}}} VALUES ?b {{ {values}}} ?a <{rdfs}> ?b FILTER(?a != ?b) }}"
        ),
        |row, overlap| {
            if let (Some(a), Some(b)) = (
                named_iri(row.values()[0].as_ref()),
                named_iri(row.values()[1].as_ref()),
            ) && let (Ok(a), Ok(b)) = (classes.binary_search(&a), classes.binary_search(&b))
                && !pairs.contains(&(a, b))
            {
                work.lock().grow_with_overlap(96, overlap)?;
                pairs.insert((a, b));
            }
            Ok(())
        },
    )?;
    let view_bytes = size_of_val(classes) as u64;
    work.lock().grow(view_bytes)?;
    let mut direct = Vec::with_capacity(classes.len());
    for (i, &iri) in classes.iter().enumerate() {
        check(opts)?;
        if !(0..classes.len()).any(|j| {
            let (a, b) = if up { (j, i) } else { (i, j) };
            pairs.contains(&(a, b)) && !pairs.contains(&(b, a))
        }) {
            direct.push(iri);
        }
    }
    drop((values, pairs));
    work.lock().shrink_to(start + view_bytes);
    Ok(direct)
}

fn profile_scratch(axiom: &Axiom) -> u64 {
    let (nodes, bytes) = write::axiom_projection_cost(axiom);
    (1024 + nodes.saturating_mul(512) + bytes) as u64
}

fn rl_gaps(
    o: &Opened,
    q: &DlQueryRequest,
    work: &Working,
    opts: &QueryOptions,
) -> Result<(Vec<DlGap>, Option<bool>, u64)> {
    let mut outside = Vec::new();
    let mut subset = Vec::new();
    let mut retained = 0u64;
    let rules = super::rl_gaps();
    for axiom in o.ontology.axiom_set() {
        check(opts)?;
        let (_, bytes) = write::axiom_projection_cost(axiom);
        let scratch = profile_scratch(axiom);
        work.lock().grow(scratch)?;
        let violations = profiles::axiom_violations(axiom, Profile::Rl);
        let copy = sparkles_owl::classify::axiom_copy_bytes(&axiom.kind) as u64
            + (bytes as u64).saturating_mul(4)
            + 256;
        if violations.is_empty() {
            for &(rule, max, what) in &rules {
                if super::gap_size(rule, axiom).is_some_and(|n| n > max) {
                    work.lock().grow(copy + 256)?;
                    retained += copy + 256;
                    subset.push(DlGap {
                        axiom: Some(axiom.clone()),
                        rule: Some(rule.to_string()),
                        message: format!("needs rule {rule} for more than {max} {what}, which the rule set does not have"),
                        partial: true,
                    });
                }
            }
        } else {
            for (rule, message) in violations {
                let bytes = copy + (rule.len() + message.len()) as u64;
                work.lock().grow(bytes)?;
                retained += bytes;
                outside.push(DlGap {
                    axiom: Some(axiom.clone()),
                    rule: Some(rule),
                    message,
                    partial: false,
                });
            }
        }
        let mut memory = work.lock();
        let live = memory.bytes() - scratch;
        memory.shrink_to(live);
    }
    let complete = outside.is_empty() && subset.is_empty();
    // Retain the partition's outside entries first, then subset gaps in rule order.
    subset.sort_by_key(|gap| {
        rules
            .iter()
            .position(|(rule, _, _)| Some(*rule) == gap.rule.as_deref())
    });
    outside.extend(subset);
    let qc: ClassExpression = Class::new(QUERY_CLASS).into();
    let scratch =
        2048 + sparkles_owl::classify::class_expression_heap_bytes(&q.expression) as u64 * 16;
    work.lock().grow(scratch)?;
    let as_sub = profiles::axiom_violations(
        &Axiom::sub_class_of(q.expression.clone(), qc.clone()),
        Profile::Rl,
    );
    let as_sup =
        profiles::axiom_violations(&Axiom::sub_class_of(qc, q.expression.clone()), Profile::Rl);
    let complete = complete && as_sub.is_empty();
    for (violations, side) in [(as_sub, "subclass"), (as_sup, "superclass")] {
        for (rule, message) in violations {
            let bytes = 256 + (rule.len() + message.len()) as u64;
            work.lock().grow(bytes)?;
            retained += bytes;
            outside.push(DlGap {
                axiom: None,
                rule: Some(rule),
                message: format!("the query expression as a {side} expression: {message}"),
                partial: false,
            });
        }
    }
    let mut memory = work.lock();
    let live = memory.bytes() - scratch;
    memory.shrink_to(live);
    Ok((outside, q.answers.instances.then_some(complete), retained))
}

#[cfg(test)]
mod tests;
