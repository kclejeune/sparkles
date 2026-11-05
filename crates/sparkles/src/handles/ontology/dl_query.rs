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

use super::classify::{admit, logical_axioms, saturate};
use super::{Ontologies, OntologyRequest, Opened};
use crate::Dataset;
use crate::error::{Error, Result};
use crate::reasoning::{Demand, ReasoningMode};
use crate::sparql::QueryOptions;
use oxrdf::{GraphName, Quad, Term};
use sparkles_core::algo::pool::Par;
use sparkles_owl::classify::{Changes, Description, Excluded, Gap, Saturation};
use sparkles_owl::manchester::{self, Parsed, Vocabulary, What};
use sparkles_owl::profiles::{self, Profile};
use sparkles_owl::{Axiom, AxiomKind, Class, ClassExpression, EntityKind, Individual, write};
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
    pub opened: Opened,
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
    /// the [module documentation](self) describes.
    pub fn dl_query(
        &self,
        req: &OntologyRequest,
        q: &DlQueryRequest,
        opts: &QueryOptions,
    ) -> Result<DlQueryAnswer> {
        req.validate()?;
        if req.inferences {
            return Err(Error::invalid(
                "a DL query reasons over the asserted axioms: leave out 'inferences'",
            ));
        }
        let opts = self.anchored(opts)?;
        let o = self.open(req, &opts)?;
        self.dl_query_opened(o, req, q, &opts)
    }

    /// [`dl_query`](Self::dl_query) over an ontology the caller opened with `req` and
    /// `opts`, such as one it read the expression against.
    pub fn dl_query_opened(
        &self,
        o: Opened,
        req: &OntologyRequest,
        q: &DlQueryRequest,
        opts: &QueryOptions,
    ) -> Result<DlQueryAnswer> {
        let start = Instant::now();
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
        let ax = defining(&q.expression);
        let expression_el = profiles::axiom_violations(&ax, Profile::El);
        let (engine, why) = match q.reasoning {
            DlReasoning::El => {
                if let Some((rule, message)) = expression_el.first() {
                    return Err(Error::invalid(format!(
                        "the expression is outside OWL 2 EL ({rule}: {message}); ask for reasoning=rl or auto"
                    )));
                }
                (
                    DlEngine::El,
                    "the caller asked for the EL classifier".to_string(),
                )
            }
            DlReasoning::Rl => (
                DlEngine::Rl,
                "the caller asked for the OWL 2 RL rules".to_string(),
            ),
            DlReasoning::Auto => {
                if let Some((rule, message)) = expression_el.first() {
                    (
                        DlEngine::Rl,
                        format!("the expression is outside OWL 2 EL ({rule}: {message})"),
                    )
                } else {
                    let outside = o
                        .ontology
                        .axiom_set()
                        .filter(|a| logical(&a.kind))
                        .filter(|a| !profiles::axiom_violations(a, Profile::El).is_empty())
                        .count();
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
        let mut a = match engine {
            DlEngine::El => self.el(&o, req, q, &ax, why, opts)?,
            DlEngine::Rl => rl(&o, q, &ax, why, opts)?,
        };
        a.elapsed = start.elapsed();
        Ok(a)
    }

    fn el(
        &self,
        o: &Opened,
        req: &OntologyRequest,
        q: &DlQueryRequest,
        ax: &Axiom,
        why: String,
        opts: &QueryOptions,
    ) -> Result<DlQueryAnswer> {
        let qc = Class::new(QUERY_CLASS);
        let mut answer = DlQueryAnswer {
            opened: o.clone(),
            expression: q.expression.clone(),
            reasoning: DlReasoningUsed {
                engine: DlEngine::El,
                why,
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
                        par: Par|
         -> Result<(bool, Description, Vec<Excluded>, u64)> {
            let r = saturate(s, axioms, changes, par, opts)?;
            let d = s.describe(&qc).ok_or_else(|| {
                Error::invalid("the classifier holds no saturation of the query class")
            })?;
            Ok((
                r.consistent,
                d,
                s.excluded().to_vec(),
                r.stats.memory as u64,
            ))
        };
        if q.answers.classes() {
            let keep = opts.graphs.is_none() && o.snapshot.cacheable;
            let (consistent, d, excluded) = if keep {
                answer.incremental = true;
                // the kept saturation reads the defining axiom beside the ontology's, and
                // the next run over it lets the axiom go
                self.with_kept(o, req, opts, "dlQuery", |e| {
                    // the threads come first: a wait that ends at the deadline leaves the
                    // kept state as it was
                    let par = admit(q.threads, opts)?;
                    let (c, d, ex, memory) =
                        e.run_over(o, vec![ax.kind.clone()], |s, axioms, changes| {
                            describe(s, axioms, changes, par)
                        })?;
                    let keep = super::kept::retain_dl(e, memory as usize);
                    Ok(((c, d, ex), keep))
                })?
            } else {
                let mut axioms = logical_axioms(&o.ontology);
                axioms.push(&ax.kind);
                let par = admit(q.threads, opts)?;
                let (c, d, ex, _) = describe(&mut Saturation::new(), &axioms, None, par)?;
                (c, d, ex)
            };
            answer.consistent = Some(consistent);
            answer.satisfiable = Some(d.satisfiable);
            answer.reasoning.gaps = gaps_of(&excluded);
            answer.reasoning.complete = excluded.is_empty();
            let a = &q.answers;
            answer.equivalents = a.equivalents.then(|| d.equivalents.clone());
            answer.superclasses = a.superclasses.then(|| d.superclasses.clone());
            answer.direct_superclasses =
                a.direct_superclasses.then(|| d.direct_superclasses.clone());
            answer.subclasses = a.subclasses.then(|| d.subclasses.clone());
            answer.direct_subclasses = a.direct_subclasses.then(|| d.direct_subclasses.clone());
            if a.instances {
                answer.reasoning.notes.push(
                    "the class answers read the terminology, and the instances the terminology with the assertions"
                        .into(),
                );
            }
        }
        if q.answers.instances {
            // the assertions take part in this classification only, which the dataset
            // does not keep
            let mut assertions: Vec<Axiom> = Vec::new();
            o.assertions(opts, |a, _, _| {
                if let Some(a) = a {
                    assertions.push(a);
                }
                Ok(true)
            })?;
            // an individual that only a declaration names is an instance of owl:Thing,
            // which gives it a context of its own
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
                describe(&mut Saturation::new(), &axioms, None, par)?;
            answer.consistent = Some(consistent);
            if answer.satisfiable.is_none() {
                answer.satisfiable = Some(d.satisfiable);
            }
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
        Ok(answer)
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
struct Charged(std::sync::Arc<sparkles_core::sparql::pool::PoolGrant>, u64);

impl Drop for Charged {
    fn drop(&mut self) {
        self.0.release(self.1);
    }
}

/// A query under the OWL 2 RL rules by demand reasoning, over a copy in memory of the
/// terminology with the defining axiom, and with the assertions when instances are
/// asked for.
fn rl(
    o: &Opened,
    q: &DlQueryRequest,
    ax: &Axiom,
    why: String,
    opts: &QueryOptions,
) -> Result<DlQueryAnswer> {
    let mut quads: Vec<Quad> = o
        .ontology
        .triples
        .iter()
        .map(|t| t.clone().in_graph(GraphName::DefaultGraph))
        .collect();
    let prefix = format!("dlq{}x", uuid::Uuid::new_v4().simple());
    quads.extend(
        write::axiom_triples(ax, &mut write::counter(&prefix))
            .into_iter()
            .map(|t| t.in_graph(GraphName::DefaultGraph)),
    );
    if q.answers.instances {
        o.assertions(opts, |_, t, _| {
            quads.push(t.clone().in_graph(GraphName::DefaultGraph));
            Ok(true)
        })?;
    }
    let bytes = quads.len() as u64 * COPY_BYTES_PER_TRIPLE;
    if let Some(limit) = opts.max_memory_bytes
        && bytes > limit
    {
        return Err(Error::BudgetExceeded(crate::Budget {
            kind: crate::BudgetKind::Memory,
            limit,
            requested: bytes,
        }));
    }
    // the copy is charged to the server's memory pool for as long as it lives, and the
    // queries over it charge their own working memory to the same grant
    let _charge = match &opts.grant {
        Some(g) => {
            g.charge(bytes).map_err(|_| {
                Error::BudgetExceeded(crate::Budget {
                    kind: crate::BudgetKind::ServerMemory,
                    limit: g.granted(),
                    requested: bytes,
                })
            })?;
            Some(Charged(g.clone(), bytes))
        }
        None => None,
    };
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
    qo.max_memory_bytes = opts.max_memory_bytes;
    qo.grant = opts.grant.clone();
    qo.pool = opts.pool.clone();
    let select = |sparql: &str| -> Result<crate::dataset::Solutions> {
        let r = mem.query_with(sparql, &qo)?;
        Ok(crate::dataset::Solutions::from_result(&r))
    };
    let iri = |t: Option<&Term>| match t {
        Some(Term::NamedNode(n)) => Some(n.as_str().to_string()),
        _ => None,
    };
    let rdfs = sparkles_owl::vocab::RDFS_SUB_CLASS_OF;
    let nothing = sparkles_owl::vocab::OWL_NOTHING;
    let is_class = |i: &str| {
        i != QUERY_CLASS
            && (o.ontology.kinds(i).contains(&EntityKind::Class)
                || i == sparkles_owl::vocab::OWL_THING)
    };
    let a = &q.answers;
    // the equivalents are the superclasses that are subclasses too, and each strict list
    // leaves them out, so any class answer needs both directions
    let want_supers = a.equivalents
        || a.superclasses
        || a.direct_superclasses
        || a.subclasses
        || a.direct_subclasses;
    let want_subs = want_supers;
    let mut supers: Vec<String> = Vec::new();
    let mut subs: Vec<String> = Vec::new();
    if want_supers || want_subs {
        for s in &select(&format!(
            "SELECT DISTINCT ?c WHERE {{ <{QUERY_CLASS}> <{rdfs}> ?c }}"
        ))? {
            if let Some(c) = iri(s.get("c")).filter(|c| is_class(c)) {
                supers.push(c);
            }
        }
    }
    if want_subs {
        for s in &select(&format!(
            "SELECT DISTINCT ?c WHERE {{ ?c <{rdfs}> <{QUERY_CLASS}> }}"
        ))? {
            if let Some(c) = iri(s.get("c")).filter(|c| is_class(c) && c != nothing) {
                subs.push(c);
            }
        }
    }
    supers.sort();
    supers.dedup();
    subs.sort();
    subs.dedup();
    let sub_set: std::collections::BTreeSet<&String> = subs.iter().collect();
    let equivalents: Vec<String> = supers
        .iter()
        .filter(|c| sub_set.contains(c))
        .cloned()
        .collect();
    let eq_set: std::collections::BTreeSet<&String> = equivalents.iter().collect();
    let strict_supers: Vec<String> = supers
        .iter()
        .filter(|c| !eq_set.contains(c) && c.as_str() != nothing)
        .cloned()
        .collect();
    let strict_subs: Vec<String> = subs
        .iter()
        .filter(|c| !eq_set.contains(c))
        .cloned()
        .collect();
    // the subsumptions between the classes found, for the direct ones
    let pairs = |vs: &[String]| -> Result<Vec<(String, String)>> {
        if vs.len() < 2 {
            return Ok(Vec::new());
        }
        let values: String = vs.iter().map(|c| format!("<{c}> ")).collect();
        let mut out = Vec::new();
        for s in &select(&format!(
            "SELECT ?a ?b WHERE {{ VALUES ?a {{ {values}}} VALUES ?b {{ {values}}} ?a <{rdfs}> ?b FILTER(?a != ?b) }}"
        ))? {
            if let (Some(x), Some(y)) = (iri(s.get("a")), iri(s.get("b"))) {
                out.push((x, y));
            }
        }
        Ok(out)
    };
    let direct = |vs: &[String], up: bool| -> Result<Vec<String>> {
        let ps: std::collections::BTreeSet<(String, String)> = pairs(vs)?.into_iter().collect();
        let below = |a: &String, b: &String| ps.contains(&(a.clone(), b.clone()));
        Ok(vs
            .iter()
            .filter(|s| {
                !vs.iter().any(|t| {
                    t != *s
                        && if up {
                            // a superclass with another one strictly below it
                            below(t, s) && !below(s, t)
                        } else {
                            // a subclass with another one strictly above it
                            below(s, t) && !below(t, s)
                        }
                })
            })
            .cloned()
            .collect())
    };
    let classes = |v: Vec<String>| v.into_iter().map(Class::new).collect::<Vec<_>>();
    let mut instances = None;
    if a.instances {
        let mut is: Vec<Individual> = Vec::new();
        for s in &select(&format!(
            "SELECT DISTINCT ?x WHERE {{ ?x a <{QUERY_CLASS}> }}"
        ))? {
            if let Some(x) = iri(s.get("x")) {
                is.push(Individual::named(x));
            }
        }
        is.sort();
        instances = Some(is);
    }
    // what the RL rules may miss: the axioms outside OWL 2 RL, and the expression where
    // it falls outside the profile on the side the answer needs
    let part = profiles::partition(&o.ontology, Profile::Rl);
    let mut gaps: Vec<DlGap> = part
        .outside
        .iter()
        .map(|v| DlGap {
            axiom: v.axiom.clone(),
            rule: Some(v.rule.clone()),
            message: v.message.clone(),
            partial: false,
        })
        .collect();
    // axioms in OWL 2 RL that need a rule the rule set covers only up to a size, such as
    // chains of more than three properties
    let mut subset_gaps = 0;
    for (rule, max, what) in super::rl_gaps() {
        for ax in part
            .inside
            .iter()
            .filter(|ax| super::gap_size(rule, ax).is_some_and(|n| n > max))
        {
            subset_gaps += 1;
            gaps.push(DlGap {
                axiom: Some((*ax).clone()),
                rule: Some(rule.to_string()),
                message: format!(
                    "needs rule {rule} for more than {max} {what}, which the rule set does not have"
                ),
                partial: true,
            });
        }
    }
    let qc: ClassExpression = Class::new(QUERY_CLASS).into();
    let as_sub = profiles::axiom_violations(
        &Axiom::sub_class_of(q.expression.clone(), qc.clone()),
        Profile::Rl,
    );
    let as_sup =
        profiles::axiom_violations(&Axiom::sub_class_of(qc, q.expression.clone()), Profile::Rl);
    for (rule, message, side) in as_sub
        .iter()
        .map(|(r, m)| (r, m, "subclass"))
        .chain(as_sup.iter().map(|(r, m)| (r, m, "superclass")))
    {
        gaps.push(DlGap {
            axiom: None,
            rule: Some(rule.clone()),
            message: format!("the query expression as a {side} expression: {message}"),
            partial: false,
        });
    }
    let notes = vec![
        "the OWL 2 RL rules derive subsumptions between classes only in part, so the class answers can miss entailed ones".to_string(),
        "OWL 2 RL cannot tell whether the expression is satisfiable or the ontology consistent".to_string(),
    ];
    // Theorem PR1 of OWL 2 Profiles: the rules derive every entailed assertion of an
    // ontology inside OWL 2 RL
    let instances_complete = a
        .instances
        .then_some(part.outside.is_empty() && as_sub.is_empty() && subset_gaps == 0);
    Ok(DlQueryAnswer {
        opened: o.clone(),
        expression: q.expression.clone(),
        reasoning: DlReasoningUsed {
            engine: DlEngine::Rl,
            why,
            complete: false,
            instances_complete,
            gaps,
            notes,
        },
        satisfiable: None,
        consistent: None,
        equivalents: a.equivalents.then(|| classes(equivalents.clone())),
        superclasses: a.superclasses.then(|| classes(strict_supers.clone())),
        direct_superclasses: if a.direct_superclasses {
            Some(classes(direct(&strict_supers, true)?))
        } else {
            None
        },
        subclasses: a.subclasses.then(|| classes(strict_subs.clone())),
        direct_subclasses: if a.direct_subclasses {
            Some(classes(direct(&strict_subs, false)?))
        } else {
            None
        },
        instances,
        incremental: false,
        elapsed: Duration::ZERO,
    })
}

#[cfg(test)]
mod tests;
