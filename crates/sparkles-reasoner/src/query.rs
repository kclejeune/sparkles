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

use crate::budget::{Budget, Held, MemoryExceeded};
use crate::demand::{self, Key, KeyP, Pass, Program, TransformError, Vocab};
use crate::engine::{self, Atom, CHead, CRule, Slot};
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

/// Which rows of a range of the derived tier [`Session::export_rows`] returns.
#[derive(Clone, Copy)]
enum Rows {
    All,
    /// the facts of the schema vocabulary
    Schema,
    /// the others
    Instances,
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
    pub compact_rounds: &'a [u32],
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
    /// A bounded finite positive program whose adorned copies cost more than its closure.
    compact: bool,
    /// The fixed visible graph is closed under all original positive rules.
    compact_done: bool,
    /// A retry may start from nonasserted partial facts, not original inputs.
    compact_started: bool,
    /// Initial rows, then successful merge ends. Overflow keeps ordinary search.
    compact_rounds: [u32; 65],
    compact_round_count: usize,
    compact_round_overflow: bool,
    /// the magic predicates of every program of the session
    hidden: FxHashSet<u64>,
    /// the schema vocabulary, and the rows of the derived tier that the schema pass gave.
    /// The schema facts among those rows go out only to the demands that can read them.
    /// The instance facts the pass derived on the way go out with the next derivation
    /// like any other, since the query's program finds them already held and does not
    /// derive them again.
    closed: Option<demand::Closed>,
    schema_rows: Option<(usize, usize)>,
    schema_instance_rows: Option<(usize, usize)>,
    /// the rows the lower stages of the schema pass derived, the facts its `noValue`
    /// builtins read, which go out with the next derivation like any other
    lower_rows: Option<(usize, usize)>,
    /// whether the equality stages of the program may read and fill the table cache:
    /// the program is specialized to the schema, its view is cacheable, and no fact was
    /// added to the session's graph from outside the view
    equality_cacheable: bool,
    /// the facts of the equality stages were read from the table cache
    cached_equality: bool,
    /// a fact was added to the session's graph from outside the view
    added_facts: bool,
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
            compact: false,
            compact_done: false,
            compact_started: false,
            compact_rounds: [0; 65],
            compact_round_count: 0,
            compact_round_overflow: false,
            hidden: FxHashSet::default(),
            closed: None,
            schema_rows: None,
            schema_instance_rows: None,
            lower_rows: None,
            equality_cacheable: false,
            cached_equality: false,
            added_facts: false,
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
        if !pre && self.compact_done {
            (self.stop)()?;
            return Ok(());
        }
        let program = if pre { &self.pre } else { &self.program };
        let program = program.as_ref().expect("prepared");
        let compact = !pre && self.compact;
        let rules = if compact {
            &self.rules
        } else {
            &program.stages[stage].rules
        };
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
        let ends = &mut self.compact_rounds;
        let count = &mut self.compact_round_count;
        let overflow = &mut self.compact_round_overflow;
        if compact {
            *count = 0;
            *overflow = self.compact_started;
            self.compact_started = true;
        }
        let mut note = |end| {
            if *overflow || (*count > 0 && ends[*count - 1] == end) {
                return;
            }
            if *count == ends.len() {
                *overflow = true;
                return;
            }
            ends[*count] = end;
            *count += 1;
        };
        let (r, stopped) = watched(&self.stop, &flag, || {
            if compact {
                engine::run_observed(g, rules, terms, &limits, None, Some(&mut note))
            } else {
                engine::run(g, rules, terms, &limits, Some(start))
            }
        });
        if tracing::enabled!(target: "sparkles_reasoner::demand", tracing::Level::DEBUG) {
            self.trace_facts(pre);
        }
        if let Some(e) = stopped {
            return Err(e);
        }
        if r.is_ok() && compact {
            self.compact_done = true;
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
        // the equality stages come last, and do not depend on the query: their facts are
        // read from the table cache, or kept there once they are derived
        let equality = if pre {
            None
        } else {
            program
                .as_ref()
                .and_then(|p| p.stages.iter().position(|s| s.equality))
                .filter(|&e| e > 0)
                .and_then(|e| self.equality_key().map(|k| (e, k)))
        };
        if let Some((_, key)) = &equality
            && let Some(t) = tables::get(key)
        {
            // the table's local terms get this session's ids, and its facts go to the
            // graph, which charges them, as they are read
            let _map_held = Held::try_new(
                &self.budget,
                t.keys
                    .len()
                    .saturating_mul(2 * std::mem::size_of::<(u64, u64)>()),
            )
            .map_err(|e| memory_error(e, "the equality stages"))?;
            let mut map: FxHashMap<u64, u64> = FxHashMap::default();
            for (old, key) in &t.keys {
                let new = self
                    .terms
                    .try_id_for_key(key)
                    .map_err(|e| memory_error(e, "the equality stages"))?;
                map.insert(*old, new);
            }
            let g = self.graph.as_mut().expect("graph made");
            g.try_add_batch(
                t.facts()
                    .map(|f| f.map(|x| map.get(&x).copied().unwrap_or(x))),
            )
            .map_err(|e| memory_error(e, "the equality stages"))?;
            self.cached_equality = true;
        }
        for si in (1..stages).rev() {
            let program = if pre { &self.pre } else { &self.program };
            let stage = &program.as_ref().expect("prepared").stages[si];
            if !pre && self.cached_equality && stage.equality {
                continue;
            }
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
        if let Some((_, key)) = equality
            && !self.cached_equality
        {
            self.cache_equality(key);
        }
        Ok(())
    }

    /// The table cache's key of the facts of the equality stages, when they may be
    /// cached: those of a program specialized to the schema of a cacheable view, whose
    /// graph holds the view's facts alone. Explanation sessions, which keep linear
    /// recursion as the rules state it, derive other intermediate facts and keep a key
    /// of their own.
    fn equality_key(&self) -> Option<tables::Key> {
        // `now` is read once per session, so its facts are the session's own
        let now = self.rules.iter().any(|r| {
            r.builtins
                .iter()
                .any(|b| b.kind == crate::builtins::BuiltinKind::Now)
        });
        if !self.equality_cacheable || self.added_facts || now {
            return None;
        }
        Some(tables::Key {
            view: self.schema_view.clone()?,
            digest: self.shared.digest,
            demand: format!("equality|{}", self.rewrite_linear),
            seeds: (0, 0),
        })
    }

    /// Keep every derived fact of the session's graph in the table cache, after its
    /// lower stages: they include the premises of each fact the equality stages derived,
    /// so that an explanation in a session that reads the table still finds them. Facts
    /// RDF cannot state are kept too, since the stages' consequences can follow from
    /// them. Magic facts are left out, and nothing is kept when a rule made a blank
    /// node, when the table would pass the cache's capacity, or when the workspace that
    /// builds it does not fit the session's memory budget.
    fn cache_equality(&self, key: tables::Key) {
        let g = self.graph.as_ref().expect("graph made");
        let rows = g
            .derived()
            .iter()
            .filter(|t| !self.hidden.contains(&t[1]))
            .count();
        let bytes = rows.saturating_mul(std::mem::size_of::<[u64; 3]>());
        if bytes as u64 > tables::capacity() {
            return;
        }
        let Ok(mut held) = Held::try_new(&self.budget, bytes) else {
            return;
        };
        let mut facts: Vec<[u64; 3]> = Vec::with_capacity(rows);
        let mut keys: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut seen: FxHashSet<u64> = FxHashSet::default();
        for t in g.derived() {
            if self.hidden.contains(&t[1]) {
                continue;
            }
            for &x in t {
                if Id(x).tag() == Tag::Local && seen.insert(x) {
                    match self.terms.local(x) {
                        Some(LocalTerm::Key(k)) => {
                            // the key's copy, its entry and the seen set's slot
                            if held.try_grow(k.len() + 64).is_err() {
                                return;
                            }
                            keys.push((x, k.clone()))
                        }
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

    /// A finite, deliberately small original closure can be cheaper than many
    /// thousands of adorned copies. No constructor, negation, explicit list
    /// writer or head action can enter this plan. Existing engine limits still
    /// apply; conservative capacity checks avoid trading a selective success
    /// for a full-closure budget failure.
    fn prefer_compact(&self, expanded: usize) -> Result<bool> {
        let graph = self.graph.as_ref().expect("graph made");
        if self.rules.len() > 256
            || self.rules.iter().any(|r| {
                r.atoms
                    .len()
                    .saturating_add(r.builtins.len())
                    .saturating_add(r.head.len())
                    > 64
            })
            || expanded < 4096
            || expanded < self.rules.len().saturating_mul(32)
            || graph.len() > 64
            || !self.hidden.is_empty()
            || !finite_positive(&self.rules, &self.terms)
        {
            return Ok(false);
        }
        fn insert(ids: &mut [u64; 64], count: &mut usize, id: u64) -> bool {
            if ids[..*count].contains(&id) {
                return true;
            }
            if *count == ids.len() {
                return false;
            }
            ids[*count] = id;
            *count += 1;
            true
        }
        let mut ids = [0u64; 64];
        let mut count = 0usize;
        for rule in &self.rules {
            (self.stop)()?;
            for atom in &rule.atoms {
                for slot in [atom.s, atom.p, atom.o] {
                    if let Slot::Const(id) = slot
                        && !insert(&mut ids, &mut count, id)
                    {
                        return Ok(false);
                    }
                }
            }
            for head in &rule.head {
                let (s, p, o) = demand::head_slots(head);
                for slot in [s, p, o] {
                    if let Slot::Const(id) = slot
                        && !insert(&mut ids, &mut count, id)
                    {
                        return Ok(false);
                    }
                }
            }
            for builtin in &rule.builtins {
                for slot in &builtin.args {
                    if let Slot::Const(id) = slot
                        && !insert(&mut ids, &mut count, *id)
                    {
                        return Ok(false);
                    }
                }
            }
        }
        let constants = count;
        let mut stopped = None;
        let mut refused = false;
        let mut feedback = false;
        let mut visit = |triple: Triple| {
            if let Err(error) = (self.stop)() {
                stopped = Some(error);
                return false;
            }
            // Without a structural-list term in input or a rule head, finite
            // non-generating rules cannot introduce one. Positive list tests
            // then remain stable, including the empty rdf:nil list.
            if triple.contains(&self.terms.rdf_first) || triple.contains(&self.terms.rdf_rest) {
                refused = true;
                return false;
            }
            // A relation between program symbols signals the measured schema
            // feedback case. Ordinary instance/schema data stays selective.
            feedback |= triple.iter().all(|id| ids[..constants].contains(id));
            refused = !triple
                .into_iter()
                .all(|id| insert(&mut ids, &mut count, id));
            !refused
        };
        let base_complete = if let Some(base) = graph.base() {
            base.for_each(None, None, None, &mut visit)
                .map_err(|e| component("reasoning-failed", format!("{e:#}")))?
        } else {
            true
        };
        if base_complete {
            for triple in graph.derived() {
                if !visit(*triple) {
                    break;
                }
            }
        }
        if let Some(error) = stopped {
            return Err(error);
        }
        if refused || !feedback {
            return Ok(false);
        }
        let closure = count.saturating_pow(3);
        Ok(closure <= self.shared.max_derived
            && closure <= self.shared.max_iterations
            && closure.saturating_mul(1024) <= self.budget.available())
    }

    /// Specialize the rules to the schema (see [`crate::specialize`]), the first time a
    /// demand derives: the schema comes from the table cache or a pass of its own, and the
    /// query's program is made again over the specialized rules.
    fn ensure_program(&mut self) -> Result<()> {
        if self.specialized {
            return Ok(());
        }
        self.specialized = true;
        self.equality_cacheable = false;
        self.cached_equality = false;
        if self.compact_done {
            let program = demand::compact_roots(&self.terms, &self.keys)
                .map_err(|e| memory_error(e, "compact demand roots"))?;
            self.hide(&program.magic_preds);
            self.program = Some(program);
            self.lower_done = true;
            return Ok(());
        }
        self.compact = false;
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
            let expanded = pre.stages.iter().map(|stage| stage.rules.len()).sum();
            if self.prefer_compact(expanded)? {
                drop(pre);
                let program = demand::compact_roots(&self.terms, &self.keys)
                    .map_err(|e| memory_error(e, "compact demand roots"))?;
                self.hide(&program.magic_preds);
                self.program = Some(program);
                self.compact = true;
                self.lower_done = false;
                tracing::debug!(target: "sparkles_reasoner::demand", expanded, rules = self.rules.len(), "selected bounded compact positive evaluation");
                return Ok(());
            }
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
            self.schema_instance_rows = Some((lower_end, schema_to));
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
        self.equality_cacheable = true;
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
            compact_rounds: if self.compact_done && !self.compact_round_overflow {
                &self.compact_rounds[..self.compact_round_count]
            } else {
                &[]
            },
        })
    }

    /// Add `facts` to the session's graph before it derives, as facts of the derived
    /// tier: a why-not hypothesis (spec F18 §4.2) or the GeoSPARQL default geometries a
    /// materialization adds.
    pub(crate) fn add_facts(&mut self, facts: Vec<Triple>) -> Result<()> {
        self.make_graph()?;
        self.added_facts = true;
        let g = self.graph.as_mut().expect("graph made");
        g.try_add_batch(facts)
            .map_err(|e| memory_error(e, "the facts of an explanation"))?;
        if self.compact_done {
            self.compact_done = false;
            self.compact = false;
            self.specialized = false;
            self.lower_done = false;
            self.program = None;
        }
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

    /// Whether `t` is a fact of the schema vocabulary, which only the demands that
    /// [`Session::reads_schema`] can read.
    fn schema_fact(&self, t: &[u64; 3]) -> bool {
        self.closed.as_ref().is_some_and(|c| {
            c.closes(&Atom {
                s: Slot::Const(t[0]),
                p: Slot::Const(t[1]),
                o: Slot::Const(t[2]),
            })
        })
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
        self.export_rows(self.exported, end, Rows::All, out);
        self.exported = end;
    }

    /// The derived facts of rows `lo` to `hi` of the derived tier, as [`export`] returns
    /// them.
    fn export_rows(&mut self, lo: usize, hi: usize, which: Rows, out: &mut Derived) {
        let g = self.graph.as_ref().expect("graph made");
        let rows = g.derived();
        let mut chunk: Vec<[u64; 3]> = Vec::new();
        for t in &rows[lo..hi] {
            if self.hidden.contains(&t[1]) || !self.terms.valid(t) {
                continue;
            }
            match which {
                Rows::All => {}
                Rows::Schema if !self.schema_fact(t) => continue,
                Rows::Instances if self.schema_fact(t) => continue,
                Rows::Schema | Rows::Instances => {}
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

/// The compact plan cannot introduce new RDF terms or bypass negative/list
/// strata or fresh generators. Positive list reads use the same evaluator as
/// full materialization; explicit list-producing programs retain their stages.
fn finite_positive(rules: &[CRule], terms: &Terms) -> bool {
    use crate::builtins::BuiltinKind::*;
    rules.iter().all(|rule| {
        rule.head.iter().all(|head| match head {
            CHead::Triple(Slot::Any, _, _)
            | CHead::Triple(_, Slot::Any, _)
            | CHead::Triple(_, _, Slot::Any) => false,
            CHead::Triple(s, p, o) => [s, p, o].iter().all(|slot|
                !matches!(slot, Slot::Const(id) if *id == terms.rdf_first || *id == terms.rdf_rest)),
            CHead::Action(..) => false,
        }) && rule.builtins.iter().all(|builtin| {
            matches!(
                builtin.kind,
                Equal
                    | NotEqual
                    | LessThan
                    | GreaterThan
                    | Le
                    | Ge
                    | IsLiteral
                    | NotLiteral
                    | IsBNode
                    | NotBNode
                    | IsDType
                    | NotDType
                    | IsFunctor
                    | NotFunctor
                    | ListMember
                    | ListContains
                    | ListForAll
            )
        })
    })
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
                self.export_rows(lo, hi, Rows::All, &mut out);
            }
            if let Some((lo, hi)) = self.schema_instance_rows.take() {
                self.export_rows(lo, hi, Rows::Instances, &mut out);
            }
            if let Some((lo, hi)) = self.schema_rows {
                if misses.iter().any(|(d, _, _)| self.reads_schema(*d)) {
                    self.schema_rows = None;
                    self.export_rows(lo, hi, Rows::Schema, &mut out);
                } else {
                    // a withheld fact is one that no demand of this derivation can read
                    debug_assert!(
                        self.graph.as_ref().expect("graph made").derived()[lo..hi]
                            .iter()
                            .filter(|t| self.schema_fact(t))
                            .all(|t| misses.iter().all(|(d, _, _)| !self.matches(*d, t)))
                    );
                }
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

#[cfg(test)]
mod compact_tests {
    use super::*;
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::store::{Store, StoreOptions};

    const PREFIX: &str = "@prefix ex: <http://ex.org/> . @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . @prefix owl: <http://www.w3.org/2002/07/owl#> . ";
    fn session(ttl: &str, profile: Profile) -> Session {
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                format!("{PREFIX}{ttl}").into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let reasoner = QueryReasoner::new(profile, &Extras::default(), Inputs::default()).unwrap();
        let mut session = Session::new(
            reasoner.shared.clone(),
            SessionRequest {
                snap: store.snapshot(),
                readable: None,
                stop: Arc::new(|| Ok(())),
                max_memory: None,
                no_cache: true,
            },
        )
        .unwrap();
        session.make_graph().unwrap();
        session
    }
    fn dense() -> Session {
        session(
            "ex:ann ex:owns ex:rex . rdf:type rdfs:domain owl:SymmetricProperty . ex:Pet rdf:type ex:rex .",
            Profile::OwlRl,
        )
    }

    #[test]
    fn compact_strategy_selects_measured_dense_fixture() {
        let session = dense();
        let rdf_type = session
            .terms
            .id_for(&Term::NamedNode(oxrdf::vocab::rdf::TYPE.into_owned()));
        let roots = demand::schema_roots(&Vocab::of(&session.rules, &session.terms), rdf_type);
        let keys: Vec<_> = roots.iter().map(|(key, _)| *key).collect();
        let program = session
            .transform(&session.rules, &keys, Pass::Schema)
            .unwrap();
        let expanded = program.stages.iter().map(|stage| stage.rules.len()).sum();
        eprintln!(
            "COMPACT_SELECTION schema_rules={expanded} original_rules={}",
            session.rules.len()
        );
        assert!(session.prefer_compact(expanded).unwrap());
        assert!(!session.prefer_compact(4095).unwrap());
    }

    #[test]
    fn compact_strategy_preserves_memory_fact_iteration_and_control_limits() {
        let mut session = dense();
        assert!(session.prefer_compact(8192).unwrap());
        session.budget = Budget::new(Some(1024));
        assert!(!session.prefer_compact(8192).unwrap());
        session.budget = Budget::unlimited();
        Arc::get_mut(&mut session.shared).unwrap().max_derived = 1;
        assert!(!session.prefer_compact(8192).unwrap());
        Arc::get_mut(&mut session.shared).unwrap().max_derived = DEFAULT_MAX_DERIVED;
        Arc::get_mut(&mut session.shared).unwrap().max_iterations = 1;
        assert!(!session.prefer_compact(8192).unwrap());
        Arc::get_mut(&mut session.shared).unwrap().max_iterations = usize::MAX;
        session.stop = Arc::new(|| Err(Error::Cancelled));
        assert!(matches!(
            session.prefer_compact(8192),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn compact_strategy_keeps_large_views_and_prior_magic_streaming() {
        let ttl: String = (0..65).map(|n| format!("ex:s ex:p ex:o{n} .")).collect();
        assert!(!session(&ttl, Profile::OwlRl).prefer_compact(8192).unwrap());
        let ttl: String = (0..22)
            .map(|n| format!("ex:s{n} ex:p{n} ex:o{n} ."))
            .collect();
        assert!(!session(&ttl, Profile::OwlRl).prefer_compact(8192).unwrap());
        assert!(
            !session("ex:s ex:p ex:o . ex:p rdfs:domain ex:C .", Profile::OwlRl)
                .prefer_compact(8192)
                .unwrap()
        );
        let mut session = dense();
        session.hidden.insert(999);
        assert!(!session.prefer_compact(8192).unwrap());
    }

    #[test]
    fn compact_strategy_excludes_negation_generators_actions_and_list_writers() {
        for rule in [
            "[r: (?s ex:p ?o), noValue(?s, ex:q) -> (?s ex:r ?o)]",
            "[r: (?s ex:p ?o), makeTemp(?x) -> (?s ex:r ?x)]",
            "[r: (?s ex:p ?o), makeSkolem(?x, ?s) -> (?s ex:r ?x)]",
            "[r: (?s ex:p ?o), now(?x) -> (?s ex:r ?x)]",
            "[r: (?s ex:p ?o), sum(?o, 1, ?x) -> (?s ex:r ?x)]",
            "[r: (?s ex:p ?o), listNotContains(?o, ?s) -> (?s ex:r ?o)]",
            "[r: (?s ex:p ?o) -> (?s rdf:first ?o)]",
        ] {
            let s = session(
                "ex:s ex:p ex:o .",
                Profile::Rules(format!("@prefix ex: <http://ex.org/> . {rule}")),
            );
            assert!(!finite_positive(&s.rules, &s.terms), "{rule}");
        }
        let s = session("", Profile::OwlRl);
        let mut rule = s.rules[0].clone_shallow();
        rule.head = vec![CHead::Action(
            crate::builtins::HeadAction::ListMapAsSubject,
            Vec::new(),
        )];
        assert!(!finite_positive(&[rule], &s.terms));
        let mut wildcard = s.rules[0].clone_shallow();
        wildcard.head = vec![CHead::Triple(Slot::Any, Slot::Const(1), Slot::Const(2))];
        assert!(!finite_positive(&[wildcard], &s.terms));
    }

    #[test]
    fn compact_session_reuses_one_closure_and_invalidates_overlay_proof_order() {
        let mut session = session(
            "ex:s ex:p ex:o . rdf:type rdfs:subPropertyOf rdf:type .",
            Profile::OwlRl,
        );
        let term = |iri: &str| Term::NamedNode(oxrdf::NamedNode::new_unchecked(iri));
        let target = [
            term("http://www.w3.org/1999/02/22-rdf-syntax-ns#nil"),
            term("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            term("http://www.w3.org/1999/02/22-rdf-syntax-ns#List"),
        ];
        let demand = crate::explain::demand_of(&target);
        let budget = session.budget.clone();
        session.derive_all(std::slice::from_ref(&demand)).unwrap();
        assert!(session.compact && session.compact_done);
        assert!(!session.compact_round_overflow);
        assert!(session.compact_round_count > 1);
        assert!(
            session.compact_rounds[..session.compact_round_count]
                .windows(2)
                .all(|ends| ends[0] < ends[1])
        );
        let inferred = session
            .graph()
            .unwrap()
            .inferred_since(session.graph().unwrap().offset());
        let rounds = session.compact_rounds;
        session.derive_all(&[demand]).unwrap();
        assert!(session.compact_done);
        assert_eq!(session.compact_rounds, rounds);
        assert_eq!(
            session
                .graph()
                .unwrap()
                .inferred_since(session.graph().unwrap().offset()),
            inferred
        );
        assert!(Arc::ptr_eq(&budget, &session.budget));
        let extra = [
            term("http://ex.org/new"),
            term("http://ex.org/p"),
            term("http://ex.org/o"),
        ]
        .map(|term| session.terms.id_for(&term));
        session.add_facts(vec![extra]).unwrap();
        assert!(!session.compact_done && !session.compact);
        assert!(session.parts().unwrap().compact_rounds.is_empty());
        assert!(Arc::ptr_eq(&budget, &session.budget));
    }

    #[test]
    fn compact_strategy_bounds_original_rule_count_and_width() {
        let mut session = dense();
        let rule = session.rules[0].clone_shallow();
        session.rules = (0..257).map(|_| rule.clone_shallow()).collect();
        assert!(!session.prefer_compact(50_000).unwrap());
        session.rules = vec![rule];
        session.rules[0].atoms = vec![
            Atom {
                s: Slot::Any,
                p: Slot::Any,
                o: Slot::Any
            };
            65
        ];
        assert!(!session.prefer_compact(8192).unwrap());
    }

    #[test]
    fn compact_strategy_preserves_structural_list_origins_in_every_position() {
        for triple in [
            "rdf:first ex:p ex:o .",
            "ex:s rdf:first ex:o .",
            "ex:s ex:p rdf:first .",
            "rdf:rest ex:p ex:o .",
            "ex:s ex:p rdf:rest .",
        ] {
            let data = format!("rdf:type rdfs:subPropertyOf rdf:type . {triple}");
            assert!(
                !session(&data, Profile::OwlRl).prefer_compact(8192).unwrap(),
                "{triple}"
            );
        }
        for head in [
            "rdf:first ex:p ?o",
            "?s ex:p rdf:first",
            "rdf:rest ex:p ?o",
            "?s ex:p rdf:rest",
        ] {
            let rule = format!("@prefix ex: <http://ex.org/> . [r: (?s ex:p ?o) -> ({head})]");
            let s = session("ex:s ex:p ex:o .", Profile::Rules(rule));
            assert!(!finite_positive(&s.rules, &s.terms), "{head}");
        }
    }

    #[test]
    fn compact_strategy_observes_overlay_feedback_and_list_refusal() {
        let mut session = session("ex:ann ex:owns ex:rex .", Profile::OwlRl);
        assert!(!session.prefer_compact(8192).unwrap());
        let term = |iri: &str| Term::NamedNode(oxrdf::NamedNode::new_unchecked(iri));
        let feedback = [
            term("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            term("http://www.w3.org/2000/01/rdf-schema#domain"),
            term("http://www.w3.org/2002/07/owl#SymmetricProperty"),
        ]
        .map(|term| session.terms.id_for(&term));
        session.add_facts(vec![feedback]).unwrap();
        assert!(session.prefer_compact(8192).unwrap());
        let structural = [
            term("http://ex.org/s"),
            term("http://ex.org/p"),
            term("http://www.w3.org/1999/02/22-rdf-syntax-ns#first"),
        ]
        .map(|term| session.terms.id_for(&term));
        session.add_facts(vec![structural]).unwrap();
        assert!(!session.prefer_compact(8192).unwrap());
    }

    #[test]
    fn compact_retry_after_failed_evaluation_does_not_claim_initial_round_order() {
        let mut session = session(
            "ex:s ex:p ex:o . rdf:type rdfs:subPropertyOf rdf:type .",
            Profile::OwlRl,
        );
        let term = |iri: &str| Term::NamedNode(oxrdf::NamedNode::new_unchecked(iri));
        let target = [
            term("http://www.w3.org/1999/02/22-rdf-syntax-ns#nil"),
            term("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            term("http://www.w3.org/1999/02/22-rdf-syntax-ns#List"),
        ];
        session
            .prepare(&[crate::explain::demand_of(&target)])
            .unwrap();
        session.ensure_program().unwrap();
        assert!(session.compact);
        Arc::get_mut(&mut session.shared).unwrap().max_iterations = 1;
        assert!(session.derive(vec![(0, Vec::new())]).is_err());
        assert!(!session.compact_done);
        assert!(
            session
                .graph()
                .unwrap()
                .inferred_since(session.graph().unwrap().offset())
                > 0
        );
        Arc::get_mut(&mut session.shared).unwrap().max_iterations = usize::MAX;
        session.derive(vec![(0, Vec::new())]).unwrap();
        assert!(session.compact_done);
        assert!(session.parts().unwrap().compact_rounds.is_empty());
    }

    #[test]
    fn compact_control_profiles_select_the_real_compact_plan() {
        let simple = "ex:s ex:p ex:o . rdf:type rdfs:subPropertyOf rdf:type .";
        let edges = "ex:s ex:edge ex:a . ex:a ex:edge ex:o . ex:s ex:edge ex:b . ex:b ex:edge ex:o . rdf:type rdfs:subPropertyOf rdf:type .";
        for (name, data, rules) in [
            (
                "later",
                simple,
                "[r-long1: (?s ex:p ?o) -> (?s ex:x ?o)] [r-long2: (?s ex:x ?o) -> (?s ex:q ?o)] [r-short: (?s ex:p ?o) -> (?s ex:q ?o)]",
            ),
            (
                "excluded-root",
                simple,
                "[r-short: (?s ex:p ?o) -> (?s ex:q ?o)] [early: (?s ex:q ?o) -> (?s ex:x ?o)] [late1: (?s ex:p ?o) -> (?s ex:u ?o)] [late2: (?s ex:u ?o) -> (?s ex:v ?o)] [late3: (?s ex:v ?o) -> (?s ex:x ?o)] [alt: (?s ex:x ?o) -> (?s ex:q ?o)]",
            ),
            (
                "cycle",
                simple,
                "[r-short: (?s ex:p ?o) -> (?s ex:q ?o)] [cycle1: (?s ex:q ?o) -> (?s ex:x ?o)] [cycle2: (?s ex:x ?o) -> (?s ex:q ?o)]",
            ),
            (
                "first-round",
                simple,
                "[first: (?s ex:p ?o) -> (?s ex:q ?o)] [second: (?s ex:p ?o) -> (?s ex:q ?o)]",
            ),
            (
                "same-rule",
                edges,
                "[choose: (?s ex:edge ?m), (?m ex:edge ?o) -> (?s ex:q ?o)]",
            ),
        ] {
            let profile = Profile::Rules(format!(
                "{}\n@prefix ex: <http://ex.org/> .\n{rules}",
                include_str!("../rules/owl-rl.rules")
            ));
            let mut session = session(data, profile);
            let target = ["s", "q", "o"].map(|name| {
                Term::NamedNode(oxrdf::NamedNode::new_unchecked(format!(
                    "http://ex.org/{name}"
                )))
            });
            session
                .prepare(&[crate::explain::demand_of(&target)])
                .unwrap();
            session.ensure_program().unwrap();
            assert!(
                session.compact,
                "control profile {name} bypassed the compact plan"
            );
        }
    }
}

#[cfg(test)]
mod equality_cache_tests {
    use super::*;
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::store::{Store, StoreOptions};

    fn store(ttl: &str) -> Store {
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                format!(
                    "@prefix ex: <http://ex.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> . {ttl}"
                )
                .into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        store
    }

    fn session(reasoner: &QueryReasoner, store: &Store) -> Session {
        Session::new(
            reasoner.shared.clone(),
            SessionRequest {
                snap: store.snapshot(),
                readable: None,
                stop: Arc::new(|| Ok(())),
                max_memory: None,
                no_cache: true,
            },
        )
        .unwrap()
    }

    fn types_of_c() -> Demand {
        Demand {
            s: DemandTerm::Var,
            p: DemandTerm::Const(Term::NamedNode(oxrdf::vocab::rdf::TYPE.into_owned())),
            o: DemandTerm::Const(Term::NamedNode(oxrdf::NamedNode::new_unchecked(
                "http://ex.org/C",
            ))),
        }
    }

    fn holds(session: &Session, s: &str) -> bool {
        let id = |t: &str| {
            session
                .terms
                .id_for(&Term::NamedNode(oxrdf::NamedNode::new_unchecked(t)))
        };
        let t = [
            id(s),
            id("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            id("http://ex.org/C"),
        ];
        session.graph().is_some_and(|g| g.contains(&t))
    }

    /// The equality stages run in the first session of a view and are read from the table
    /// cache in the next, with the same facts. The sessions ask for no result tables,
    /// which would answer the demand without a derivation, but `no_cache` does not
    /// bypass the view's equality table, as it does not bypass the schema table.
    #[test]
    fn equality_stages_are_read_from_the_cache_in_later_sessions() {
        let store = store("ex:a owl:sameAs ex:b . ex:b a ex:C . ex:unrelated ex:p ex:x .");
        let reasoner =
            QueryReasoner::new(Profile::OwlRl, &Extras::default(), Inputs::default()).unwrap();
        let mut first = session(&reasoner, &store);
        first.derive_all(&[types_of_c()]).unwrap();
        assert!(!first.cached_equality);
        assert!(holds(&first, "http://ex.org/a"));
        for _ in 0..2 {
            let mut next = session(&reasoner, &store);
            next.derive_all(&[types_of_c()]).unwrap();
            assert!(next.cached_equality);
            assert!(holds(&next, "http://ex.org/a"));
        }
        // an explanation session keeps a table of its own
        let mut explaining = reasoner
            .explain_session(
                SessionRequest {
                    snap: store.snapshot(),
                    readable: None,
                    stop: Arc::new(|| Ok(())),
                    max_memory: None,
                    no_cache: false,
                },
                true,
            )
            .unwrap();
        explaining.derive_all(&[types_of_c()]).unwrap();
        assert!(!explaining.cached_equality);
    }

    /// A session whose graph holds facts from outside the view, as a why-not hypothesis
    /// adds them, neither reads nor fills the equality table.
    #[test]
    fn added_facts_bypass_the_equality_table() {
        let store = store("ex:a owl:sameAs ex:b . ex:b a ex:C .");
        let reasoner =
            QueryReasoner::new(Profile::OwlRl, &Extras::default(), Inputs::default()).unwrap();
        let mut hypothesis = session(&reasoner, &store);
        let id = |t: &str| {
            hypothesis
                .terms
                .id_for(&Term::NamedNode(oxrdf::NamedNode::new_unchecked(t)))
        };
        let extra = [
            id("http://ex.org/z"),
            id("http://www.w3.org/2002/07/owl#sameAs"),
            id("http://ex.org/a"),
        ];
        hypothesis.add_facts(vec![extra]).unwrap();
        hypothesis.derive_all(&[types_of_c()]).unwrap();
        assert!(!hypothesis.cached_equality);
        assert!(holds(&hypothesis, "http://ex.org/z"));
        let mut plain = session(&reasoner, &store);
        plain.derive_all(&[types_of_c()]).unwrap();
        assert!(!plain.cached_equality, "the hypothesis filled no table");
        assert!(!holds(&plain, "http://ex.org/z"));
        let mut next = session(&reasoner, &store);
        next.derive_all(&[types_of_c()]).unwrap();
        assert!(next.cached_equality);
        assert!(!holds(&next, "http://ex.org/z"));
    }
}

#[cfg(test)]
mod equality_workspace_tests {
    use super::*;
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::store::{Store, StoreOptions};

    /// The workspace that builds the equality table is charged to the session's budget,
    /// and the table, which is optional, is not kept when the workspace does not fit.
    #[test]
    fn equality_table_workspace_is_charged() {
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                "@prefix ex: <http://ex.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> . ex:a owl:sameAs ex:b . ex:b a ex:C ."
                    .as_bytes()
                    .to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let reasoner =
            QueryReasoner::new(Profile::OwlRl, &Extras::default(), Inputs::default()).unwrap();
        let mut session = Session::new(
            reasoner.shared.clone(),
            SessionRequest {
                snap: store.snapshot(),
                readable: None,
                stop: Arc::new(|| Ok(())),
                max_memory: Some(1 << 30),
                no_cache: true,
            },
        )
        .unwrap();
        session.make_graph().unwrap();
        let id = |t: &str| {
            session
                .terms
                .id_for(&Term::NamedNode(oxrdf::NamedNode::new_unchecked(t)))
        };
        let fact = [
            id("http://ex.org/a"),
            id("http://ex.org/p"),
            id("http://ex.org/b"),
        ];
        session.graph_mut().unwrap().add_batch([fact]);
        session.equality_cacheable = true;
        let key = session.equality_key().expect("cacheable");
        let _rest = Held::try_new(&session.budget, session.budget.available()).unwrap();
        session.cache_equality(key.clone());
        assert!(tables::get(&key).is_none());
        drop(_rest);
        session.cache_equality(key.clone());
        assert!(tables::get(&key).is_some());
    }
}
