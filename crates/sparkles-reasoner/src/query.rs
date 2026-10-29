//! Query-time reasoning (spec F17 Phase 1): the [`ReasoningProvider`] a query carries
//! to derive, on demand, the facts its patterns need.
//!
//! A session compiles the rules for the snapshot the query reads, with backward rules
//! among them, makes the demand program of [`demand`] for the query's demands, and
//! evaluates it with the semi-naive engine over the base tier of the input graphs as the
//! caller sees them. Lower stages (the patterns `noValue` reads) run first, to their
//! fixpoint. Each call of [`derive`](ReasoningSession::derive) adds the magic facts of
//! its demands' constants and seeds and continues the evaluation from them, so later
//! calls build on what earlier calls derived. Each derivation runs under the query's
//! deadline, cancellation and memory budget and a limit of derived facts.

use crate::budget::{Budget, MemoryExceeded};
use crate::demand::{self, Key, KeyP, Pass, Program, TransformError, Vocab};
use crate::engine::{self, Atom, CRule, Slot};
use crate::facts::Base;
use crate::graph::{Graph, Triple};
use crate::inputs::{self, Inputs};
use crate::parser::Rule;
use crate::tables::{self, Chunk, Table};
use crate::terms::{LocalTerm, Terms};
use crate::{Extras, InferredLimitExceeded, Profile};
use oxrdf::Term;
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_core::error::ComponentError;
use sparkles_core::id::{Id, Tag};
use sparkles_core::sparql::reasoning::{
    Demand, DemandInfo, DemandReport, DemandTerm, Derived, LocalTerm as OutTerm, ReasoningProvider,
    ReasoningSession, Report, SessionRequest,
};
use sparkles_core::store::Snapshot;
use sparkles_core::{Error, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The facts one query may derive by default (spec F17 §8). Magic facts do not count:
/// they are charged to the query's memory budget.
pub const DEFAULT_MAX_DERIVED: usize = 1_000_000;

/// The code of an error for a derivation that passed a limit.
pub const BUDGET_CODE: &str = "reasoning-budget";
/// The code of an error for rules that cannot be evaluated on demand.
pub const RULES_CODE: &str = "reasoning-rules";

/// How often a derivation checks the query's deadline and cancellation.
const STOP_POLL: Duration = Duration::from_millis(5);

struct Shared {
    profile: Profile,
    extras: Extras,
    rules: Vec<Rule>,
    /// the profile or vocabulary of each rule of `rules`
    sources: Vec<String>,
    /// whether the materialization adds GeoSPARQL default geometries
    geo_default: bool,
    inputs: Inputs,
    digest: u64,
    max_derived: usize,
    max_iterations: usize,
    warnings: Vec<String>,
}

/// A reasoner that derives facts for queries on demand, with the rules of a profile.
#[derive(Clone)]
pub struct QueryReasoner {
    shared: Arc<Shared>,
}

impl QueryReasoner {
    /// The rules of `profile` with the vocabularies of `extras`, over the input graphs
    /// `inputs`, with [`DEFAULT_MAX_DERIVED`] as the limit of derived facts.
    pub fn new(profile: Profile, extras: &Extras, inputs: Inputs) -> anyhow::Result<QueryReasoner> {
        let rules = crate::profile_rules(&profile, extras)?;
        let sources = crate::rule_sources(&profile, extras)?;
        let mut warnings = Vec::new();
        if extras.geo_default_geometry {
            warnings.push(
                "geo:hasDefaultGeometry is materialized only; query-time reasoning does not derive it"
                    .to_string(),
            );
        }
        Ok(QueryReasoner {
            shared: Arc::new(Shared {
                digest: crate::rules_digest(&profile, extras),
                profile,
                extras: extras.clone(),
                rules,
                sources,
                geo_default: extras.geo_default_geometry,
                inputs,
                max_derived: DEFAULT_MAX_DERIVED,
                // a linear chain of n nodes takes n iterations, so the limit of derived
                // facts and the query's deadline bound the run instead
                max_iterations: usize::MAX,
                warnings,
            }),
        })
    }

    /// The same reasoner with at most `n` derived facts per query (`usize::MAX` sets no
    /// limit).
    pub fn with_max_derived(self, n: usize) -> QueryReasoner {
        let s = &self.shared;
        QueryReasoner {
            shared: Arc::new(Shared {
                profile: s.profile.clone(),
                extras: s.extras.clone(),
                rules: s.rules.clone(),
                sources: s.sources.clone(),
                geo_default: s.geo_default,
                inputs: s.inputs.clone(),
                digest: s.digest,
                max_derived: n,
                max_iterations: s.max_iterations,
                warnings: s.warnings.clone(),
            }),
        }
    }

    /// The limit of derived facts per query.
    pub fn max_derived(&self) -> usize {
        self.shared.max_derived
    }

    /// The profile.
    pub fn profile(&self) -> &Profile {
        &self.shared.profile
    }

    /// The configured inputs of this query-time program.
    pub fn inputs(&self) -> &Inputs {
        &self.shared.inputs
    }

    /// The vocabularies and default-geometry setting of the configured program.
    pub fn extras(&self) -> &Extras {
        &self.shared.extras
    }

    /// This reasoner as the provider [`QueryOptions::reasoning`] takes.
    ///
    /// [`QueryOptions::reasoning`]: sparkles_core::sparql::QueryOptions::reasoning
    pub fn provider(&self) -> Arc<dyn ReasoningProvider> {
        Arc::new(self.clone())
    }
}

impl ReasoningProvider for QueryReasoner {
    fn label(&self) -> String {
        format!("demand, profile {}", self.shared.profile.name())
    }

    fn start(&self, req: SessionRequest) -> Result<Box<dyn ReasoningSession>> {
        Ok(Box::new(Session::new(self.shared.clone(), req)?))
    }
}

impl QueryReasoner {
    /// A session for an explanation (spec F18 §3.1). Linear recursion is not rewritten,
    /// so every fact it derives has an instance of an original rule among the facts
    /// derived. Its demands neither read nor fill the table cache, since a table's facts
    /// go to a query and not into the session's graph, which the search reads. With
    /// `cached` set it reads and fills the schema's table, whose facts join the graph, and
    /// without it not even that, since facts added to the session (a why-not hypothesis)
    /// change the schema.
    pub(crate) fn explain_session(&self, mut req: SessionRequest, cached: bool) -> Result<Session> {
        req.no_cache = true;
        let mut s = Session::new(self.shared.clone(), req)?;
        s.rewrite_linear = false;
        s.view = None;
        if !cached {
            s.schema_view = None;
        }
        Ok(s)
    }
}

fn component(code: &str, msg: impl Into<String>) -> Error {
    Error::Component(Box::new(ComponentError::new("reasoner", code, msg)))
}

fn memory_error(e: MemoryExceeded, what: &str) -> Error {
    component(
        BUDGET_CODE,
        format!("query-time reasoning passed the query's memory budget while deriving {what}: {e}"),
    )
}

/// The query's demand `d`, written for messages.
fn demand_text(d: &Demand) -> String {
    let t = |x: &DemandTerm| match x {
        DemandTerm::Var => "?".to_string(),
        DemandTerm::Const(t) => t.to_string(),
        DemandTerm::Seeded => "(seeds)".to_string(),
    };
    format!("{{ {} {} {} }}", t(&d.s), t(&d.p), t(&d.o))
}

/// A short name of a term for EXPLAIN.
fn short(t: &Term) -> String {
    const NS: [(&str, &str); 4] = [
        ("rdf:", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("rdfs:", "http://www.w3.org/2000/01/rdf-schema#"),
        ("owl:", "http://www.w3.org/2002/07/owl#"),
        ("xsd:", "http://www.w3.org/2001/XMLSchema#"),
    ];
    if let Term::NamedNode(n) = t {
        for (p, ns) in NS {
            if let Some(l) = n.as_str().strip_prefix(ns) {
                return format!("{p}{l}");
            }
        }
    }
    t.to_string()
}

/// Which position of a demand its seeds bind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seeded {
    None,
    Subject,
    Object,
}

/// The parts of a session an explanation's search reads (see [`Session::parts`]).
pub(crate) struct Parts<'a> {
    pub graph: &'a mut Graph,
    pub terms: &'a Terms,
    pub rules: &'a [CRule],
    pub infos: &'a [engine::RuleInfo],
    pub sources: &'a [String],
    pub geo_default: bool,
    pub inputs: &'a [Id],
    pub snap: &'a Arc<Snapshot>,
    pub budget: &'a Arc<Budget>,
}

pub(crate) struct Session {
    shared: Arc<Shared>,
    snap: Arc<Snapshot>,
    terms: Terms,
    rules: Vec<CRule>,
    /// what an explanation names of each rule
    infos: Vec<engine::RuleInfo>,
    /// whether the demand program rewrites linear recursion (see [`demand::transform`])
    rewrite_linear: bool,
    graphs: Vec<Id>,
    absent: bool,
    stop: Arc<dyn Fn() -> Result<()> + Send + Sync>,
    budget: Arc<Budget>,
    /// the cache's view key, when the view is a committed one and the query may read
    /// cached tables
    view: Option<String>,
    /// the view key of the schema's table, which `nocache` does not bypass: like the
    /// compiled rules, the schema is the same for every query of the view
    schema_view: Option<String>,
    warnings: Vec<String>,
    demands: Vec<Demand>,
    keys: Vec<Key>,
    /// the constants of each demand (0 where none), and its seeded position
    consts: Vec<[u64; 3]>,
    seeded: Vec<Seeded>,
    program: Option<Program>,
    graph: Option<Graph>,
    /// derived rows already returned, from the start of the derived tier
    exported: usize,
    locals_sent: FxHashSet<u64>,
    /// the facts returned, and whether any holds a term the store does not have
    chunks: Vec<Chunk>,
    /// the local terms with keys of the exported facts, which tables carry
    local_keys: Vec<(u64, Vec<u8>)>,
    /// whether an exported fact holds a blank node a rule made, which no table may hold
    made_bnodes: bool,
    /// whether the lower stages were evaluated
    lower_done: bool,
    /// the schema pass's program, when it ran in this session
    pre: Option<Program>,
    /// whether the program was specialized to the schema, or found not to need it
    specialized: bool,
    /// the magic predicates of every program of the session
    hidden: FxHashSet<u64>,
    /// the schema the rules were specialized to, and its rows in the derived tier, which
    /// go out only to the demands that can read them
    closed: Option<demand::Closed>,
    schema_rows: Option<(usize, usize)>,
    /// the rows the lower stages of the schema pass derived, the facts its `noValue`
    /// builtins read, which go out with the next derivation like any other
    lower_rows: Option<(usize, usize)>,
    /// tables read from the cache
    cached: Vec<Arc<Table>>,
    reports: Vec<DemandReport>,
    seeds: Vec<FxHashSet<u64>>,
}

impl Session {
    fn new(shared: Arc<Shared>, req: SessionRequest) -> Result<Session> {
        let t0 = std::time::Instant::now();
        let snap = req.snap;
        let budget = Budget::new(req.max_memory.map(|m| m.min(usize::MAX as u64) as usize));
        let terms = Terms::new(snap.clone());
        terms
            .set_budget(&budget)
            .map_err(|e| memory_error(e, "the rules"))?;
        let mut warnings = shared.warnings.clone();
        let (rules, infos) = engine::compile_info(&shared.rules, &terms, &mut warnings, true);
        // imports are followed only through the graphs the caller reads (spec F17 §4.5)
        let resolved = match &req.readable {
            Some(readable) => inputs::resolve_readable(&snap, &shared.inputs, &|g| readable(g)),
            None => inputs::resolve(&snap, &shared.inputs),
        };
        warnings.extend(resolved.warnings.iter().cloned());
        let mut graphs = resolved.ids(&snap);
        tracing::debug!(target: "sparkles_reasoner::demand", "session: rules compiled and inputs resolved in {:?}", t0.elapsed());
        if let Some(readable) = &req.readable {
            graphs.retain(|g| readable(*g));
        }
        let absent = graphs.contains(&Id::DEFAULT_GRAPH);
        let schema_view = snap.cacheable.then(|| {
            format!(
                "{}|{}|{}|{:?}",
                snap.generation.uid,
                snap.commit,
                snap.mask.as_ref().map_or("", |m| m.key.as_str()),
                graphs
            )
        });
        let view = schema_view.clone().filter(|_| !req.no_cache);
        Ok(Session {
            shared,
            snap,
            terms,
            rules,
            infos,
            rewrite_linear: true,
            graphs,
            absent,
            stop: req.stop,
            budget,
            view,
            schema_view,
            warnings,
            demands: Vec::new(),
            keys: Vec::new(),
            consts: Vec::new(),
            seeded: Vec::new(),
            program: None,
            graph: None,
            exported: 0,
            locals_sent: FxHashSet::default(),
            chunks: Vec::new(),
            local_keys: Vec::new(),
            made_bnodes: false,
            lower_done: false,
            pre: None,
            specialized: false,
            hidden: FxHashSet::default(),
            closed: None,
            schema_rows: None,
            lower_rows: None,
            cached: Vec::new(),
            reports: Vec::new(),
            seeds: Vec::new(),
        })
    }

    fn id_of(&self, t: &Term) -> u64 {
        self.terms.id_for(t)
    }

    /// The table cache's key of demand `d` with `seeds` (sorted), when it may be cached.
    fn table_key(&self, d: usize, seeds: &[u64]) -> Option<tables::Key> {
        let view = self.view.as_ref()?;
        // the demand names its constants by term, but seeds by the session's ids
        if seeds.iter().any(|&s| Id(s).tag() == Tag::Local) {
            return None;
        }
        let mut h = crate::maintain::FNV_START;
        for s in seeds {
            h = crate::maintain::fnv(&s.to_le_bytes(), h);
        }
        Some(tables::Key {
            view: view.clone(),
            digest: self.shared.digest,
            demand: format!("{:?}|{:?}", self.demands[d], self.seeded[d] as u8),
            seeds: (seeds.len(), h),
        })
    }

    /// Evaluate `rules` from row `start` on, under the limits, checking the query's
    /// deadline and cancellation meanwhile.
    fn run(&mut self, pre: bool, stage: usize, start: u32, what: &str) -> Result<()> {
        let program = if pre { &self.pre } else { &self.program };
        let program = program.as_ref().expect("prepared");
        let rules = &program.stages[stage].rules;
        let g = self.graph.as_mut().expect("graph made");
        // magic facts are charged to the memory budget, not to the limit of facts
        let derived = g.inferred_since(g.offset());
        let max = self.shared.max_derived;
        if derived > max {
            return Err(component(
                BUDGET_CODE,
                format!(
                    "query-time reasoning passed the limit of {max} derived facts while deriving {what}"
                ),
            ));
        }
        let flag = Arc::new(AtomicBool::new(false));
        let limits = engine::Limits {
            max_iterations: self.shared.max_iterations,
            max_inferred: max - derived,
            cancel: Some(flag.clone()),
            progress: None,
            budget: self.budget.clone(),
        };
        let terms = &self.terms;
        let (r, stopped) = watched(&self.stop, &flag, || {
            engine::run(g, rules, terms, &limits, Some(start))
        });
        if tracing::enabled!(target: "sparkles_reasoner::demand", tracing::Level::DEBUG) {
            self.trace_facts(pre);
        }
        if let Some(e) = stopped {
            return Err(e);
        }
        r.map(|_| ()).map_err(|e| {
            if let Some(l) = e.downcast_ref::<InferredLimitExceeded>() {
                return component(
                    BUDGET_CODE,
                    format!(
                        "query-time reasoning passed the limit of {max} derived facts while \
                         deriving {what}{}; raise the limit with maxDerived",
                        l.context
                    ),
                );
            }
            if let Some(m) = e.downcast_ref::<MemoryExceeded>() {
                return memory_error(m.clone(), what);
            }
            let msg = format!("{e:#}");
            if msg.contains("did not reach a fixpoint") {
                return component(
                    BUDGET_CODE,
                    format!("query-time reasoning stopped while deriving {what}: {msg}"),
                );
            }
            component(
                "reasoning-failed",
                format!("query-time reasoning failed: {msg}"),
            )
        })
    }

    /// Make the graph over the input graphs, once.
    fn make_graph(&mut self) -> Result<()> {
        if self.graph.is_some() {
            return Ok(());
        }
        let base = Base::new(self.snap.clone(), &self.graphs)
            .map_err(|e| component("reasoning-failed", format!("{e:#}")))?;
        let mut g = Graph::with_base(Arc::new(base));
        g.set_budget(&self.budget)
            .map_err(|e| memory_error(e, "the input graphs"))?;
        self.graph = Some(g);
        Ok(())
    }

    /// Evaluate the lower stages, the first time a demand needs them.
    fn ensure_graph(&mut self) -> Result<()> {
        self.make_graph()?;
        if self.lower_done {
            return Ok(());
        }
        self.lower_done = true;
        self.run_lower(false, "the patterns noValue reads")
    }

    /// Evaluate the lower stages of the query's program or the schema pass's (`pre`), the
    /// last first.
    fn run_lower(&mut self, pre: bool, what: &str) -> Result<()> {
        let program = if pre { &self.pre } else { &self.program };
        let stages = program.as_ref().map_or(0, |p| p.stages.len());
        for si in (1..stages).rev() {
            let program = if pre { &self.pre } else { &self.program };
            let stage = &program.as_ref().expect("prepared").stages[si];
            let g = self.graph.as_mut().expect("graph made");
            if let Some(p) = stage.only_if
                && g.count_p(p, 0, g.len()) == 0
            {
                continue;
            }
            let seeds = stage.seeds.clone();
            let start = g.len();
            g.try_add_batch(seeds).map_err(|e| memory_error(e, what))?;
            self.run(pre, si, start, what)?;
        }
        Ok(())
    }

    fn hide(&mut self, preds: &FxHashSet<u64>) {
        self.hidden.extend(preds.iter().copied());
        if let Some(g) = self.graph.as_mut() {
            g.set_hidden(Arc::new(self.hidden.clone()));
        }
    }

    fn transform(&self, rules: &[CRule], roots: &[Key], pass: Pass<'_>) -> Result<Program> {
        let g = self.graph.as_ref().expect("graph made");
        let est = |a: &Atom, bound: &[bool]| estimate(g, a, bound);
        demand::transform(rules, &self.terms, roots, &est, pass, self.rewrite_linear).map_err(|e| {
            match e {
                TransformError::Memory(m) => memory_error(m, "the demand program"),
                TransformError::TooLarge(_) => component(BUDGET_CODE, e.to_string()),
                TransformError::NotStratified => component(RULES_CODE, e.to_string()),
            }
        })
    }

    /// Specialize the rules to the schema (see [`crate::specialize`]), the first time a
    /// demand derives: the schema comes from the table cache or a pass of its own, and the
    /// query's program is made again over the specialized rules.
    fn ensure_program(&mut self) -> Result<()> {
        if self.specialized {
            return Ok(());
        }
        self.specialized = true;
        let iri = |s: &str| Term::NamedNode(oxrdf::NamedNode::new_unchecked(s));
        let rdf_type = self.id_of(&iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"));
        let vocab = Vocab::of(&self.rules, &self.terms);
        let roots = demand::schema_roots(&vocab, rdf_type);
        self.make_graph()?;
        let t0 = std::time::Instant::now();
        // A preliminary pass would allocate a second fresh witness for stored
        // bindings when ordinary demand later reaches the same rule. Keep these
        // non-idempotent programs in one demand derivation, with their complete
        // ordinary lower stages and existing result-table policy.
        let fresh_witnesses = self.rules.iter().any(|r| {
            r.builtins
                .iter()
                .any(|b| b.kind == crate::builtins::BuiltinKind::MakeTemp)
        });
        if roots.is_empty() || fresh_witnesses {
            let program = self.transform(&self.rules, &self.keys, Pass::Plain)?;
            self.hide(&program.magic_preds);
            self.program = Some(program);
            self.lower_done = false;
            return Ok(());
        }
        let key = self.schema_view.as_ref().map(|v| tables::Key {
            view: v.clone(),
            digest: self.shared.digest,
            demand: "schema".into(),
            seeds: (0, 0),
        });
        let schema_from = self.graph.as_ref().expect("graph made").derived().len();
        let mut lower_end = schema_from;
        if let Some(t) = key.as_ref().and_then(tables::get) {
            let facts = self.remap(&t)?;
            let g = self.graph.as_mut().expect("graph made");
            g.try_add_batch(facts)
                .map_err(|e| memory_error(e, "the schema"))?;
        } else {
            let keys: Vec<Key> = roots.iter().map(|r| r.0).collect();
            let pre = self.transform(&self.rules, &keys, Pass::Schema)?;
            self.hide(&pre.magic_preds);
            let seeds: Vec<Triple> = roots
                .iter()
                .map(|(k, v)| {
                    let m = pre.stages[0]
                        .magic(k)
                        .expect("a root key has a magic predicate");
                    demand::magic_fact(k, m, pre.dummy, v[0], v[1], v[2])
                })
                .collect();
            self.pre = Some(pre);
            // the patterns the schema rules' noValue reads are complete before stage zero
            // reads them (Codex review F17-P1 #3)
            self.run_lower(true, "the schema")?;
            let g = self.graph.as_mut().expect("graph made");
            lower_end = g.derived().len();
            let start = g.len();
            g.try_add_batch(seeds)
                .map_err(|e| memory_error(e, "the schema"))?;
            self.run(true, 0, start, "the schema")?;
            if let Some(key) = key {
                // the schema facts the lower stages gave belong to the schema, and their
                // instance facts are derived again by the sessions that read the table
                let closed = crate::specialize::closed(&vocab, rdf_type);
                self.cache_schema(key, schema_from, lower_end, &closed);
            }
        }
        let schema_to = self.graph.as_ref().expect("graph made").derived().len();
        if self.exported == schema_from {
            self.exported = schema_to;
            self.schema_rows = Some((lower_end, schema_to));
            if lower_end > schema_from {
                self.lower_rows = Some((schema_from, lower_end));
            }
        }
        // Cached schema rows are valid seeds even when a relation is not yet
        // complete. Keep that vocabulary for exposure independently of closure.
        self.closed = Some(crate::specialize::closed(&vocab, rdf_type));
        let same_as = self.id_of(&iri("http://www.w3.org/2002/07/owl#sameAs"));
        let flag = AtomicBool::new(false);
        let (complete, stopped) = watched(&self.stop, &flag, || {
            demand::complete_schema(&self.rules, &vocab, rdf_type, same_as, &self.budget, &flag)
        });
        if let Some(error) = stopped {
            return Err(error);
        }
        let mut complete = complete
            .map_err(|e| memory_error(e, "schema completeness"))?
            .expect("an unstopped completeness analysis returns its result");
        let flag = AtomicBool::new(false);
        let (refined, stopped) = watched(&self.stop, &flag, || {
            demand::refine_schema(
                &self.rules,
                &vocab,
                &self.terms,
                self.graph.as_ref().expect("graph made"),
                &mut complete.closed,
                &self.budget,
                &flag,
            )
        });
        if let Some(error) = stopped {
            return Err(error);
        }
        refined.map_err(|e| match e.downcast::<MemoryExceeded>() {
            Ok(e) => memory_error(e, "schema completeness"),
            Err(e) => component("reasoning-failed", format!("{e:#}")),
        })?;
        let closed = &complete.closed;
        // RDF lists are read as stored unless a rule gives their facts
        let lists: Vec<u64> = ["first", "rest"]
            .into_iter()
            .map(|l| {
                self.id_of(&iri(&format!(
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#{l}"
                )))
            })
            .filter(|&p| {
                !self.rules.iter().any(|r| {
                    r.head
                        .iter()
                        .any(|h| demand::head_slots(h).1 == Slot::Const(p))
                })
            })
            .collect();
        let g = self.graph.as_ref().expect("graph made");
        let t1 = std::time::Instant::now();
        // the schema joins can be long, so they stop with the query (Codex review F17-P1 #5)
        let flag = AtomicBool::new(false);
        let (rules, stopped) = watched(&self.stop, &flag, || {
            crate::specialize::specialize(
                &self.rules,
                closed,
                &lists,
                g,
                &self.terms,
                &self.budget,
                &flag,
            )
        });
        if let Some(e) = stopped {
            return Err(e);
        }
        let rules = rules.map_err(|e| memory_error(e, "the rules specialized to the schema"))?;
        let g = self.graph.as_ref().expect("graph made");
        let rules = crate::specialize::without_equality(rules.rules, same_as, g);
        let t2 = std::time::Instant::now();
        let program = self.transform(&rules, &self.keys, Pass::Closed(closed))?;
        tracing::debug!(
            target: "sparkles_reasoner::demand",
            "schema {:?}, {} rules specialized in {:?}, program of {} rules in {:?}, stages {:?}",
            t1 - t0,
            rules.len(),
            t2 - t1,
            program.stages.iter().map(|s| s.rules.len()).sum::<usize>(),
            t2.elapsed(),
            program
                .stages
                .iter()
                .map(|s| (s.keys().count(), s.rules.len()))
                .collect::<Vec<_>>()
        );
        self.hide(&program.magic_preds);
        self.program = Some(program);
        self.lower_done = false;
        Ok(())
    }

    /// The parts a search over the session's facts reads, borrowed at once.
    pub(crate) fn parts(&mut self) -> Result<Parts<'_>> {
        self.make_graph()?;
        Ok(Parts {
            graph: self.graph.as_mut().expect("graph made"),
            terms: &self.terms,
            rules: &self.rules,
            infos: &self.infos,
            sources: &self.shared.sources,
            geo_default: self.shared.geo_default,
            inputs: &self.graphs,
            snap: &self.snap,
            budget: &self.budget,
        })
    }

    /// Add `facts` to the session's graph before it derives, as facts of the derived
    /// tier: a why-not hypothesis (spec F18 §4.2) or the GeoSPARQL default geometries a
    /// materialization adds.
    pub(crate) fn add_facts(&mut self, facts: Vec<Triple>) -> Result<()> {
        self.make_graph()?;
        let g = self.graph.as_mut().expect("graph made");
        g.try_add_batch(facts)
            .map_err(|e| memory_error(e, "the facts of an explanation"))?;
        Ok(())
    }

    /// Derive every fact of `demands`, each with its constants alone.
    pub(crate) fn derive_all(&mut self, demands: &[Demand]) -> Result<()> {
        self.prepare(demands)?;
        let seeds = (0..demands.len()).map(|d| (d, Vec::new())).collect();
        self.derive(seeds)?;
        Ok(())
    }

    /// The session's graph, made when no demand derived yet.
    pub(crate) fn graph_mut(&mut self) -> Result<&mut Graph> {
        self.make_graph()?;
        Ok(self.graph.as_mut().expect("graph made"))
    }

    pub(crate) fn graph(&self) -> Option<&Graph> {
        self.graph.as_ref()
    }

    pub(crate) fn terms(&self) -> &Terms {
        &self.terms
    }

    pub(crate) fn rules(&self) -> &[CRule] {
        &self.rules
    }

    pub(crate) fn infos(&self) -> &[engine::RuleInfo] {
        &self.infos
    }

    /// The profile or vocabulary of each parsed rule.
    pub(crate) fn sources(&self) -> &[String] {
        &self.shared.sources
    }

    pub(crate) fn geo_default(&self) -> bool {
        self.shared.geo_default
    }

    pub(crate) fn budget(&self) -> &Arc<Budget> {
        &self.budget
    }

    /// Whether demand `d` can read facts of the schema.
    fn reads_schema(&self, d: usize) -> bool {
        let Some(c) = &self.closed else {
            return true;
        };
        let k = &self.keys[d];
        match k.p {
            KeyP::Const(p) if p == c.rdf_type => k.class.is_none_or(|x| c.classes.contains(&x)),
            KeyP::Const(p) => c.preds.contains(&p),
            _ => true,
        }
    }

    /// Keep the facts the schema pass derived from row `from` of the derived tier in the
    /// table cache, unless a rule made a blank node among them. Of the rows before
    /// `lower_end`, which its lower stages derived, only the schema facts `closed` names
    /// are kept (Codex re-review F17-P1 #1).
    fn cache_schema(
        &self,
        key: tables::Key,
        from: usize,
        lower_end: usize,
        closed: &demand::Closed,
    ) {
        let g = self.graph.as_ref().expect("graph made");
        let mut facts: Vec<[u64; 3]> = Vec::new();
        let mut keys: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut seen: FxHashSet<u64> = FxHashSet::default();
        for (i, t) in g.derived()[from..].iter().enumerate() {
            if self.hidden.contains(&t[1]) || !self.terms.valid(t) {
                continue;
            }
            let schema = || {
                closed.closes(&Atom {
                    s: Slot::Const(t[0]),
                    p: Slot::Const(t[1]),
                    o: Slot::Const(t[2]),
                })
            };
            if from + i < lower_end && !schema() {
                continue;
            }
            for &x in t {
                if Id(x).tag() == Tag::Local && seen.insert(x) {
                    match self.terms.local(x) {
                        Some(LocalTerm::Key(k)) => keys.push((x, k.clone())),
                        _ => return,
                    }
                }
            }
            facts.push(*t);
        }
        tables::put(
            key,
            Arc::new(Table {
                chunks: vec![Arc::new(facts)],
                keys,
            }),
        );
    }

    /// The facts of a cached table in this session's ids.
    fn remap(&self, t: &Table) -> Result<Vec<[u64; 3]>> {
        let mut map: FxHashMap<u64, u64> = FxHashMap::default();
        for (old, key) in &t.keys {
            let new = self
                .terms
                .try_id_for_key(key)
                .map_err(|e| memory_error(e, "a cached table"))?;
            map.insert(*old, new);
        }
        Ok(t.facts()
            .map(|f| f.map(|x| map.get(&x).copied().unwrap_or(x)))
            .collect())
    }

    /// The derived facts not returned yet, without magic facts and facts RDF cannot state.
    fn export(&mut self, out: &mut Derived) {
        let end = self.graph.as_ref().expect("graph made").derived().len();
        self.export_rows(self.exported, end, out);
        self.exported = end;
    }

    /// The derived facts of rows `lo` to `hi` of the derived tier, as [`export`] returns
    /// them.
    fn export_rows(&mut self, lo: usize, hi: usize, out: &mut Derived) {
        let g = self.graph.as_ref().expect("graph made");
        let rows = g.derived();
        let mut chunk: Vec<[u64; 3]> = Vec::new();
        for t in &rows[lo..hi] {
            if self.hidden.contains(&t[1]) || !self.terms.valid(t) {
                continue;
            }
            for &x in t {
                if Id(x).tag() == Tag::Local && self.locals_sent.insert(x) {
                    let lt = match self.terms.local(x) {
                        Some(LocalTerm::Key(k)) => {
                            self.local_keys.push((x, k.clone()));
                            OutTerm::Key(k)
                        }
                        _ => {
                            self.made_bnodes = true;
                            OutTerm::BNode
                        }
                    };
                    out.locals.push((x, lt));
                }
            }
            chunk.push(*t);
        }
        out.facts.extend_from_slice(&chunk);
        if !chunk.is_empty() {
            self.chunks.push(Arc::new(chunk));
        }
    }

    /// A cached table in this session's ids: the local terms it holds, which another
    /// session made, get this session's ids, and the new ones go out with `out`.
    fn adopt(&mut self, t: Arc<Table>, out: &mut Derived) -> Result<Arc<Table>> {
        if t.keys.is_empty() {
            return Ok(t);
        }
        let mut map: FxHashMap<u64, u64> = FxHashMap::default();
        for (old, key) in &t.keys {
            let new = self
                .terms
                .try_id_for_key(key)
                .map_err(|e| memory_error(e, "a cached table"))?;
            map.insert(*old, new);
            if Id(new).tag() == Tag::Local && self.locals_sent.insert(new) {
                out.locals.push((new, OutTerm::Key(key.clone())));
            }
        }
        let facts: Vec<[u64; 3]> = t
            .facts()
            .map(|f| f.map(|x| map.get(&x).copied().unwrap_or(x)))
            .collect();
        Ok(Arc::new(Table {
            chunks: vec![Arc::new(facts)],
            keys: Vec::new(),
        }))
    }

    /// Debug output: the magic facts of each key and the derived facts of each predicate.
    fn trace_facts(&self, pre: bool) {
        let program = if pre { &self.pre } else { &self.program };
        let program = program.as_ref().expect("prepared");
        let g = self.graph.as_ref().expect("graph made");
        let mut by_magic: FxHashMap<u64, usize> = FxHashMap::default();
        let mut by_pred: FxHashMap<u64, usize> = FxHashMap::default();
        let mut first: FxHashMap<u64, Vec<[u64; 3]>> = FxHashMap::default();
        for t in g.derived() {
            if self.hidden.contains(&t[1]) {
                *by_magic.entry(t[1]).or_default() += 1;
                let f = first.entry(t[1]).or_default();
                if f.len() < 3 {
                    f.push(*t);
                }
            } else {
                *by_pred.entry(t[1]).or_default() += 1;
            }
        }
        let name = |x: u64| {
            self.terms
                .term(x)
                .map_or_else(|| format!("#{x}"), |t| t.to_string())
        };
        for (si, st) in program.stages.iter().enumerate() {
            for (k, m) in st.keys() {
                if let Some(n) = by_magic.get(&m) {
                    let p = match k.p {
                        KeyP::Const(c) => name(c),
                        _ => format!("{:?}", k.p),
                    };
                    let sample: Vec<String> = first
                        .get(&m)
                        .into_iter()
                        .flatten()
                        .map(|t| {
                            let v = |x: u64| {
                                if x == program.dummy {
                                    "_".to_string()
                                } else {
                                    name(x)
                                }
                            };
                            format!("({} {})", v(t[0]), v(t[2]))
                        })
                        .collect();
                    tracing::debug!(
                        target: "sparkles_reasoner::demand",
                        "stage {si} key {p}^{}{} magic facts {n} first {}",
                        k.adornment(),
                        if k.neq { " neq" } else { "" },
                        sample.join(" ")
                    );
                }
            }
        }
        for (p, n) in by_pred {
            tracing::debug!(target: "sparkles_reasoner::demand", "derived {} {n}", name(p));
        }
    }

    fn matches(&self, d: usize, t: &[u64; 3]) -> bool {
        let c = &self.consts[d];
        let k = &self.keys[d];
        let pos = |i: usize, bound: bool| -> bool {
            if !bound {
                return true;
            }
            let seeded = match self.seeded[d] {
                Seeded::Subject => i == 0,
                Seeded::Object => i == 2,
                Seeded::None => false,
            };
            if seeded {
                self.seeds[d].contains(&t[i])
            } else {
                t[i] == c[i]
            }
        };
        pos(0, k.s) && pos(1, matches!(k.p, KeyP::Const(_))) && pos(2, k.o)
    }
}

/// The demand transformation's estimate of the facts atom `a` matches with the variables
/// `bound` bound, before any derivation: from the input graphs' statistics, and for a
/// pattern of unknown predicate.
fn estimate(g: &Graph, a: &Atom, bound: &[bool]) -> (f64, f64) {
    let b = |s: Slot| match s {
        Slot::Const(_) => true,
        Slot::Var(v) => bound[v],
        Slot::Any => false,
    };
    let (sb, pb, ob) = (b(a.s), b(a.p), b(a.o));
    let n = g.len().max(1) as f64;
    let fan = (n / g.distinct_subjects() as f64).max(1.0);
    let per_p = (n / g.distinct_predicates() as f64).max(1.0);
    let unknown = match (sb, ob) {
        (true, true) => 1.0,
        (true, false) | (false, true) => {
            if pb {
                fan
            } else {
                fan * 4.0
            }
        }
        (false, false) => {
            if pb {
                per_p
            } else {
                n * 2.0
            }
        }
    };
    let stored = match a.p {
        Slot::Const(p) => {
            let cnt = g.count_p(p, 0, g.len()) as f64;
            let (ds, dos) = g.pstats(p);
            match (sb, ob) {
                (true, true) => cnt.min(1.0),
                (true, false) => cnt / ds.max(1) as f64,
                (false, true) => cnt / dos.max(1) as f64,
                (false, false) => cnt,
            }
        }
        _ => unknown,
    };
    (stored, unknown)
}

impl ReasoningSession for Session {
    fn intensional(&self, s: Option<&Term>, p: Option<&Term>, o: Option<&Term>) -> bool {
        let slot = |t: Option<&Term>, v: usize| match t {
            Some(t) => Slot::Const(self.id_of(t)),
            None => Slot::Var(v),
        };
        demand::intensional(&self.rules, (slot(s, 0), slot(p, 1), slot(o, 2)))
    }

    fn prepare(&mut self, demands: &[Demand]) -> Result<Vec<DemandInfo>> {
        self.demands = demands.to_vec();
        self.keys.clear();
        self.consts.clear();
        self.seeded.clear();
        for d in demands {
            let mut c = [0u64; 3];
            let mut val = |t: &DemandTerm, i: usize| -> bool {
                match t {
                    DemandTerm::Const(t) => {
                        let id = self.terms.id_for(t);
                        c[i] = id;
                        true
                    }
                    DemandTerm::Seeded => true,
                    DemandTerm::Var => false,
                }
            };
            let s = val(&d.s, 0);
            let p = val(&d.p, 1);
            let o = val(&d.o, 2);
            let kp = if p { KeyP::Const(c[1]) } else { KeyP::Free };
            let k = Key::new(s, kp, o);
            let rdf_type = self.id_of(&Term::NamedNode(oxrdf::vocab::rdf::TYPE.into_owned()));
            self.keys.push(match (&d.p, &d.o) {
                (DemandTerm::Const(_), DemandTerm::Const(_)) if c[1] == rdf_type => {
                    k.with_class(c[2])
                }
                _ => k,
            });
            self.consts.push(c);
            self.seeded.push(match (&d.s, &d.o) {
                (DemandTerm::Seeded, _) => Seeded::Subject,
                (_, DemandTerm::Seeded) => Seeded::Object,
                _ => Seeded::None,
            });
        }
        // the program is made at the first derivation, which a cached table may spare
        let infos = demands
            .iter()
            .zip(&self.keys)
            .zip(&self.consts)
            .map(|((d, k), c)| {
                let pred = match &d.p {
                    DemandTerm::Const(t) => short(t),
                    _ => "?p".to_string(),
                };
                let slot = |t: &DemandTerm, i: usize| match t {
                    DemandTerm::Const(_) => Slot::Const(c[i]),
                    _ => Slot::Var(i),
                };
                let pattern = (slot(&d.s, 0), slot(&d.p, 1), slot(&d.o, 2));
                DemandInfo {
                    adornment: format!("{pred}^{}", k.adornment()),
                    rules: demand::cone(&self.rules, pattern).into_iter().collect(),
                }
            })
            .collect();
        self.reports = vec![DemandReport::default(); demands.len()];
        self.seeds = vec![FxHashSet::default(); demands.len()];
        self.program = None;
        self.specialized = false;
        Ok(infos)
    }

    fn derive(&mut self, seeds: Vec<(usize, Vec<u64>)>) -> Result<Derived> {
        (self.stop)()?;
        let mut out = Derived {
            absent_from_default: self.absent,
            ..Default::default()
        };
        let mut misses: Vec<(usize, Vec<u64>, Option<tables::Key>)> = Vec::new();
        let mut hits: Vec<Arc<Table>> = Vec::new();
        for (d, mut s) in seeds {
            s.sort_unstable();
            s.dedup();
            let rep = &mut self.reports[d];
            rep.used = true;
            rep.seeds += s.len() as u64;
            self.seeds[d].extend(s.iter().copied());
            let key = self.table_key(d, &s);
            if let Some(t) = key.as_ref().and_then(tables::get) {
                self.reports[d].cached = true;
                hits.push(t);
                continue;
            }
            misses.push((d, s, key));
        }
        if !misses.is_empty() {
            let what = misses
                .iter()
                .map(|(d, _, _)| demand_text(&self.demands[*d]))
                .collect::<Vec<_>>()
                .join(", ");
            self.ensure_program()?;
            self.ensure_graph()?;
            if let Some((lo, hi)) = self.lower_rows.take() {
                self.export_rows(lo, hi, &mut out);
            }
            if let Some((lo, hi)) = self.schema_rows
                && misses.iter().any(|(d, _, _)| self.reads_schema(*d))
            {
                self.schema_rows = None;
                self.export_rows(lo, hi, &mut out);
            }
            let program = self.program.as_ref().expect("prepared");
            let stage = &program.stages[0];
            let mut magic: Vec<Triple> = Vec::new();
            for (d, s, _) in &misses {
                let k = &self.keys[*d];
                let m = stage.magic(k).expect("a root key has a magic predicate");
                let c = self.consts[*d];
                match self.seeded[*d] {
                    Seeded::None => {
                        magic.push(demand::magic_fact(k, m, program.dummy, c[0], c[1], c[2]))
                    }
                    Seeded::Subject => magic.extend(
                        s.iter()
                            .map(|&v| demand::magic_fact(k, m, program.dummy, v, c[1], c[2])),
                    ),
                    Seeded::Object => magic.extend(
                        s.iter()
                            .map(|&v| demand::magic_fact(k, m, program.dummy, c[0], c[1], v)),
                    ),
                }
            }
            let g = self.graph.as_mut().expect("graph made");
            let start = g.len();
            g.try_add_batch(magic).map_err(|e| memory_error(e, &what))?;
            let t0 = std::time::Instant::now();
            self.run(false, 0, start, &what)?;
            tracing::debug!(target: "sparkles_reasoner::demand", "derived {what} in {:?}", t0.elapsed());
            self.export(&mut out);
            if !self.made_bnodes {
                let table = Arc::new(Table {
                    chunks: self.chunks.clone(),
                    keys: self.local_keys.clone(),
                });
                for (_, _, key) in misses {
                    if let Some(k) = key {
                        tables::put(k, table.clone());
                    }
                }
            }
        }
        for t in hits {
            let t = self.adopt(t, &mut out)?;
            out.facts.extend(t.facts().copied());
            self.cached.push(t);
        }
        Ok(out)
    }

    fn report(&self) -> Report {
        let mut demands = self.reports.clone();
        let derived: u64 = self.chunks.iter().map(|c| c.len() as u64).sum();
        let facts = self
            .chunks
            .iter()
            .chain(self.cached.iter().flat_map(|t| t.chunks.iter()));
        let used: Vec<usize> = (0..demands.len()).filter(|&d| demands[d].used).collect();
        let mut seen: FxHashMap<usize, FxHashSet<[u64; 3]>> = FxHashMap::default();
        for c in facts {
            for t in c.iter() {
                for &d in &used {
                    if self.matches(d, t) {
                        seen.entry(d).or_default().insert(*t);
                    }
                }
            }
        }
        for (d, s) in seen {
            demands[d].derived = s.len() as u64;
        }
        Report {
            derived,
            tables: used.len() as u64,
            cached: used.iter().filter(|&&d| demands[d].cached).count() as u64,
            demands,
            warnings: self.warnings.clone(),
        }
    }
}

/// `f` run while a thread polls `stop`, the query's deadline and cancellation, and sets
/// `flag` when it fails, with the error it gave.
fn watched<R>(
    stop: &Arc<dyn Fn() -> Result<()> + Send + Sync>,
    flag: &AtomicBool,
    f: impl FnOnce() -> R,
) -> (R, Option<Error>) {
    let stopped: Mutex<Option<Error>> = Mutex::new(None);
    let done = AtomicBool::new(false);
    let r = std::thread::scope(|sc| {
        let watch = sc.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                if let Err(e) = stop() {
                    *stopped.lock() = Some(e);
                    flag.store(true, Ordering::Relaxed);
                    return;
                }
                std::thread::park_timeout(STOP_POLL);
            }
        });
        let r = f();
        done.store(true, Ordering::Relaxed);
        watch.thread().unpark();
        r
    });
    (r, stopped.into_inner())
}
