//! Forward-chaining reasoning for Sparkles (Jena `GenericRuleReasoner` FORWARD mode /
//! `infer` equivalent).
//!
//! The reasoner loads its input graphs into memory as id-level triples, runs a rule set
//! to a fixpoint with semi-naive evaluation, and writes every derived triple that is not
//! already asserted into the named graph [`INFERRED_GRAPH`]. The input graphs are the
//! **default graph** unless [`ReasonOptions::inputs`] names others, plus the graphs their
//! `owl:imports` lead to (see [`inputs`]). Queries see the entailments by adding that
//! graph to their default graph (`QueryOptions::default_graph_extra`).
//!
//! Rule sets: [`Profile::Rdfs`] (full RDFS entailment), [`Profile::RdfsSimple`]
//! (subClassOf / subPropertyOf / domain / range), [`Profile::OwlRl`] (an OWL 2 RL subset
//! comparable to Jena's OWL Mini) or any Jena rule text ([`Profile::Rules`]).
//!
//! [`materialize_incremental`] updates a previous materialization from the changes to
//! the input graphs since its commit, with the same result as a full run.

#[cfg(test)]
mod alloc_probe;
pub mod analysis;
mod budget;
mod builtins;
mod closure;
#[doc(hidden)]
pub use closure::set_batch as set_closure_batch;
mod demand;
pub mod diagnostics;
mod engine;
pub mod explain;
pub mod extras;
mod facts;
pub mod fixpoint;
mod graph;
#[cfg(test)]
mod hierarchy_tests;
mod incremental;
pub mod inputs;
mod ir;
mod lookups;
mod maintain;
pub mod parser;
mod pool;
pub mod qlrewrite;
pub mod query;
mod reuse;
pub mod shacl_af;
mod specialize;
pub mod tables;
mod terms;
mod tier;
pub mod whynot;
mod witness;

pub use budget::MemoryExceeded as MemoryLimitExceeded;
pub use extras::{Extras, UnknownVocabulary, Vocabulary};
pub use inputs::{
    Fetched, GraphRef, Import, ImportMode, Inputs, LocationMapping, Resolved, fetch_imports,
};
pub use maintain::{
    Cache, DEFAULT_CACHE_TRIPLES, FactWitnesses, literal_typings, literal_typings_controlled,
    literal_typings_controlled_guarded,
};
pub use parser::{
    BuiltinCall, Clause, Direction, Node, RelationAtom, Rule, RuleParseError, TriplePattern,
    parse_rules, rules_text, uses_program,
};
pub use sparkles_budget::ParentHandle;

use anyhow::Context as _;
use budget::{Budget, Held, WorkingWarnings};
use incremental::Fallback;
use oxrdf::{NamedNode, NamedOrBlankNode, Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_core::id::{Id, Tag};
use sparkles_core::index::Perm;
use sparkles_core::store::{Chunk, Snapshot, Store};
use std::fmt;
use std::mem::size_of;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use terms::{LocalTerm, Terms};

pub use sparkles_core::INFERRED_GRAPH;
pub use witness::{DEFAULT_MAX_WITNESSES, MAX_WITNESSES_LIMIT, WitnessReport};

/// Full RDFS entailment (Jena `etc/rdfs.rules`, W3C RDF 1.1 Semantics rules).
pub const RDFS_RULES: &str = include_str!("../rules/rdfs.rules");
/// RDFS subset: subClassOf / subPropertyOf closure and inheritance, domain, range.
pub const RDFS_SIMPLE_RULES: &str = include_str!("../rules/rdfs-simple.rules");
/// OWL 2 RL subset (comparable to Jena OWL Mini/Micro).
pub const OWL_RL_RULES: &str = include_str!("../rules/owl-rl.rules");
/// The rules of OWL 2 RL that [`OWL_RL_RULES`] covers only up to a size, one per line:
/// the rule, the largest size covered and what the size counts. Lines that start with
/// `#` are comments.
pub const OWL_RL_GAPS: &str = include_str!("../rules/owl-rl.gaps");

/// Which rules to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Profile {
    Rdfs,
    RdfsSimple,
    OwlRl,
    /// OWL 2 QL query rewriting (spec F17 §7): queries are rewritten against the OWL 2 QL
    /// axioms of the input graphs, and a pattern the rewriting does not cover is derived
    /// with the rules of [`Profile::OwlRl`]. Only query-time reasoning takes it; a
    /// materialization refuses it.
    OwlQl,
    /// Jena rule text
    Rules(String),
    /// SHACL Advanced Features rules, and SPIN rules, read from a shapes graph
    /// ([`shacl_af`]); a materialization runs them, and nothing else does
    ShaclAf(shacl_af::ShaclAf),
}

impl Profile {
    pub fn name(&self) -> &str {
        match self {
            Profile::Rdfs => "rdfs",
            Profile::RdfsSimple => "rdfs-simple",
            Profile::OwlRl => "owl-rl",
            Profile::OwlQl => "owl-ql",
            Profile::Rules(_) => "rules",
            Profile::ShaclAf(_) => "shacl-af",
        }
    }

    /// The rule text of this profile.
    pub fn text(&self) -> &str {
        match self {
            Profile::Rdfs => RDFS_RULES,
            Profile::RdfsSimple => RDFS_SIMPLE_RULES,
            Profile::OwlRl | Profile::OwlQl => OWL_RL_RULES,
            Profile::Rules(t) => t,
            Profile::ShaclAf(_) => "",
        }
    }

    pub fn rules(&self) -> Result<Vec<Rule>, RuleParseError> {
        if let Profile::ShaclAf(_) = self {
            return Err(RuleParseError {
                line: 0,
                column: 0,
                message: "the shacl-af profile reads its rules from a shapes graph, and only \
                          a materialization runs them"
                    .into(),
            });
        }
        parse_rules(self.text())
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "unknown reasoning profile '{0}' (expected rdfs, rdfs-simple, owl-rl, owl, owl-ql or shacl-af)"
)]
pub struct UnknownProfile(pub String);

impl FromStr for Profile {
    type Err = UnknownProfile;
    fn from_str(s: &str) -> Result<Profile, UnknownProfile> {
        let (name, params) = s.trim().split_once('?').unwrap_or((s.trim(), ""));
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "shacl-af" | "shaclaf" | "shacl-rules"
        ) {
            return shacl_af::ShaclAf::parse_params(params)
                .map(Profile::ShaclAf)
                .map_err(|e| UnknownProfile(format!("{s}: {e}")));
        }
        match s.trim().to_ascii_lowercase().as_str() {
            "rdfs" | "rdfs-full" => Ok(Profile::Rdfs),
            "rdfs-simple" | "rdfssimple" | "simple" => Ok(Profile::RdfsSimple),
            "owl-rl" | "owlrl" | "owl" | "owl-mini" | "owlmini" | "owl-micro" | "owlmicro" => {
                Ok(Profile::OwlRl)
            }
            "owl-ql" | "owlql" | "owl2ql" | "ql" => Ok(Profile::OwlQl),
            _ => Err(UnknownProfile(s.to_string())),
        }
    }
}

/// Progress callback: (fraction done in `[0, 1]`, message).
pub use sparkles_core::task::ProgressFn;

/// The default of [`ReasonOptions::max_inferred`]: 50 million derived triples.
pub const DEFAULT_MAX_INFERRED: usize = 50_000_000;

/// A run stopped because it derived, or was about to derive, more triples than
/// [`ReasonOptions::max_inferred`] allows. The run changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "reasoning exceeded the limit of {limit} inferred triples{context}; raise the limit with \
     --max-inferred (sparkles infer), --reason-max-inferred or maxInferred (the server), or \
     max_inferred (the library and bindings)"
)]
pub struct InferredLimitExceeded {
    /// the limit the run had
    pub limit: usize,
    /// the rule that produced the most candidates in the last iteration, for the message
    /// (with a leading space), or empty
    pub context: String,
}

/// Reasoning limits, cancellation and progress reporting.
#[derive(Clone)]
pub struct ReasonOptions {
    /// Maximum number of semi-naive iterations before giving up.
    /// Request-local controls for isolated entailment analyses. Ordinary runs leave this absent.
    pub analysis: Option<Arc<analysis::AnalysisContext>>,
    pub max_iterations: usize,
    /// Maximum number of derived triples before giving up, [`DEFAULT_MAX_INFERRED`] by
    /// default. A run that passes it fails with [`InferredLimitExceeded`]. `usize::MAX`
    /// sets no limit.
    pub max_inferred: usize,
    /// Set to `true` to cancel.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Called with (fraction done in `[0, 1]`, message).
    pub progress: Option<ProgressFn>,
    /// The graphs the rules read (the default graph and its imports by default).
    pub inputs: Inputs,
    /// The estimated heap a run may use at any time, in bytes, before it fails. It
    /// covers the derived facts and their indexes, the local terms the rules make, the
    /// candidates and caches of rule evaluation, the previous closure of an incremental
    /// run with its maintenance state, the buffers that write the inferred graph and the
    /// closure kept for the next run. Each is charged before it grows. The store's block
    /// cache is not counted. `None` sets no limit.
    pub max_memory: Option<usize>,
    /// Parent memory handle that the run's budget reserves from in chunks (`None`: unpooled).
    pub memory_parent: Option<ParentHandle>,
    /// Threads for rule evaluation: at most the size of [`pool`](Self::pool) when there
    /// is one. `None` uses the pool, or the caller's threads.
    pub threads: Option<usize>,
    /// The reasoning pool runs evaluate on and are admitted to (the server's), see
    /// [`ReasoningPool`].
    pub pool: Option<Arc<ReasoningPool>>,
    /// Evaluate transitive and equivalence rules with closure operators, which reach the
    /// same closure in fewer iterations. `true` by default. `false` evaluates them as
    /// ordinary rules, for differential tests and measurements.
    #[doc(hidden)]
    pub closure_operators: bool,
    /// Order the bodies of rules with three or more atoms from sampled bindings where
    /// the sample makes an order at least twice as cheap as the greedy planner's
    /// (spec F23, roadmap item 939). `true` by default. `false` keeps the greedy
    /// planner's order of every body, for differential tests and measurements. A run
    /// derives the same facts either way.
    #[doc(hidden)]
    pub sampled_planning: bool,
    /// Keep up to this many witnesses per fact of the closure (spec F18 §7.1): sets of
    /// input quads from which the rules derive the fact, kept with the provenance
    /// algebra of F13 and maintained by incremental runs. They live with the closure a
    /// [`Cache`] keeps, where [`Cache::witnesses`] reads them. `None`, the default, keeps
    /// none, and a run then does no work for them. A protection that shows inferences
    /// by their support turns them on.
    pub witnesses: Option<usize>,
    /// Report the change of [`INFERRED_GRAPH`] that an incremental run commits, as
    /// [`ReasonReport::inferred_change`]. Off by default, since the change is held in
    /// memory until the report is read.
    pub inferred_change: bool,
    /// Called once with the state the run reads, while the run holds the writer lock and
    /// before it derives anything, after a progress report. The warnings it returns join
    /// the report's, so that a check of the input graphs sees exactly what the rules
    /// read. An error fails the run, and a long check observes [`cancel`](Self::cancel)
    /// itself.
    pub inspect: Option<InspectFn>,
}

/// A check of the state a run reads ([`ReasonOptions::inspect`]).
pub type InspectFn =
    Arc<dyn Fn(&Arc<sparkles_core::store::Snapshot>) -> anyhow::Result<Vec<String>> + Send + Sync>;

impl Default for ReasonOptions {
    fn default() -> Self {
        ReasonOptions {
            analysis: None,
            max_iterations: 10_000,
            max_inferred: DEFAULT_MAX_INFERRED,
            cancel: None,
            progress: None,
            inputs: Inputs::default(),
            max_memory: None,
            memory_parent: None,
            threads: None,
            pool: None,
            closure_operators: true,
            sampled_planning: true,
            witnesses: None,
            inferred_change: false,
            inspect: None,
        }
    }
}

impl fmt::Debug for ReasonOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReasonOptions")
            .field("max_iterations", &self.max_iterations)
            .field("max_inferred", &self.max_inferred)
            .field("cancel", &self.cancel)
            .field("progress", &self.progress.as_ref().map(|_| "…"))
            .field("inputs", &self.inputs)
            .field("max_memory", &self.max_memory)
            .field("memory_parent", &self.memory_parent.as_ref().map(|_| "…"))
            .field("threads", &self.threads)
            .field("pool", &self.pool.as_ref().map(|p| p.threads()))
            .field("closure_operators", &self.closure_operators)
            .field("sampled_planning", &self.sampled_planning)
            .field("witnesses", &self.witnesses)
            .field("inspect", &self.inspect.as_ref().map(|_| "…"))
            .finish()
    }
}

/// Outcome of a reasoning run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReasonReport {
    pub profile: String,
    /// number of rules that were run (after flattening nested rules and skipping
    /// unsupported ones)
    pub rules: usize,
    pub iterations: usize,
    /// triples in [`INFERRED_GRAPH`] after the run
    pub inferred: u64,
    pub millis: u64,
    pub warnings: Vec<String>,
    /// the commit that wrote the inferences, or the unchanged head (at which the data
    /// was read) when the run changed nothing; `None` for [`infer`] (a dry run)
    pub receipt: Option<sparkles_core::commit::Receipt>,
    /// how the run materialized
    pub method: Method,
    /// why a run asked to update a previous materialization ran in full
    pub fallback: Option<String>,
    /// what an incremental run changed
    pub changes: Option<Changes>,
    /// triples the run added to [`INFERRED_GRAPH`]
    pub inferred_added: u64,
    /// triples the run removed from [`INFERRED_GRAPH`]
    pub inferred_removed: u64,
    /// With [`ReasonOptions::inferred_change`], the change an incremental run committed
    /// to [`INFERRED_GRAPH`]: each triple it added with weight `+1` and each it removed
    /// with weight `-1`, once each, in the ids of the snapshot that
    /// [`receipt`](Self::receipt) names. `None` for a full run, a run that fell back to
    /// one, and a run without the option, which tells a reader of the change to read the
    /// inferred graph again.
    pub inferred_change: Option<Vec<([Id; 3], i64)>>,
    /// the graphs the run read, with its imports
    pub inputs: Option<Resolved>,
    /// the largest estimated heap the run used at any time, within
    /// [`ReasonOptions::max_memory`] when the run succeeds
    pub peak_bytes: u64,
    /// index scans of the input graphs the run issued: rule evaluation and the GeoSPARQL
    /// default geometries. A read that the answers a kept closure remembers serve counts
    /// as the scan it saves.
    pub base_scans: u64,
    /// threads the rules were evaluated on
    pub threads: usize,
    /// the witnesses the run built or updated, when [`ReasonOptions::witnesses`] asks
    /// for them
    pub witnesses: Option<WitnessReport>,
    /// the round cap of a `shacl-af` run ([`shacl_af::ShaclAf::max_rounds`])
    pub max_rounds: Option<usize>,
    /// the groups of rules of a `shacl-af` run, in the order they ran, with their rounds
    pub rule_groups: Vec<shacl_af::RuleGroup>,
}

/// How a run materialized.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Method {
    /// every rule over the whole default graph
    #[default]
    Full,
    /// the previous closure, updated with the changes since its commit
    Incremental,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Full => "full",
            Method::Incremental => "incremental",
        }
    }
}

/// What an incremental run changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// triples added to the input graphs since the previous run (to their union)
    pub base_added: u64,
    /// triples removed from the input graphs since the previous run (from their union)
    pub base_removed: u64,
    /// derived facts whose other proofs were searched for
    pub checked: u64,
    /// facts that left the closure (some came back with the added triples)
    pub removed: u64,
    /// facts that joined the closure
    pub derived: u64,
    /// where the previous closure came from: `memory` or `store`
    pub source: String,
}

/// The wall time of a run's phases in microseconds, for the trace event that [`finish`]
/// writes with the target `sparkles_reasoner::timing`.
struct Laps {
    last: Instant,
    laps: Vec<(&'static str, u64)>,
}

impl Laps {
    fn new(start: Instant) -> Laps {
        Laps {
            last: start,
            laps: Vec::new(),
        }
    }

    /// The time since the lap before is the time of `phase`.
    fn lap(&mut self, phase: &'static str) {
        let now = Instant::now();
        self.add(phase, now.duration_since(self.last).as_micros() as u64);
        self.last = now;
    }

    /// A phase timed by its own code, which lies inside the next lap.
    fn add(&mut self, phase: &'static str, micros: u64) {
        self.laps.push((phase, micros));
    }
}

impl fmt::Display for Laps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, (phase, micros)) in self.laps.iter().enumerate() {
            let sep = if i == 0 { "" } else { " " };
            write!(f, "{sep}{phase}={micros}")?;
        }
        Ok(())
    }
}

fn progress(opts: &ReasonOptions, f: f32, msg: &str) {
    if let Some(p) = &opts.progress {
        p(f, msg);
    }
}

/// In-memory reasoning result (before writing): the derived facts, with the input graphs
/// as the graph's base tier.
struct Derivation {
    graph: graph::Graph,
    terms: Terms,
    rules: usize,
    iterations: usize,
    warnings: WorkingWarnings,
    /// the GeoSPARQL default geometries the run added as facts, sorted, and their charge
    defaults: Option<(Vec<[u64; 3]>, Held)>,
    base_scans: u64,
    threads: usize,
    /// the run's memory: declared last so every payload and `Held` drops before parent credit returns
    budget: Arc<Budget>,
}

pub use pool::ReasoningPool;

/// Test hook: the threads of the own pools of reasoning runs that are working now. It
/// drops before a run gives its workers back, so it never exceeds the capacity of the
/// reasoning pool the runs are admitted to.
#[doc(hidden)]
pub fn run_pool_threads() -> usize {
    pool::run_pool_threads()
}

/// A reasoning pool of `threads` threads, for [`ReasonOptions::pool`].
pub fn thread_pool(threads: usize) -> anyhow::Result<Arc<ReasoningPool>> {
    Ok(Arc::new(ReasoningPool::new(threads)?))
}

/// Run `f` off rayon, admitted to the threads the options ask for (see [`pool`]).
fn admitted<T: Send>(
    opts: &ReasonOptions,
    f: impl FnOnce(&pool::Exec) -> anyhow::Result<T> + Send,
) -> anyhow::Result<T> {
    pool::Exec::refuse_own_threads(opts.pool.as_ref())?;
    pool::off_rayon(|| {
        let exec = pool::Exec::admit(opts.threads, opts.pool.as_ref(), opts.cancel.as_deref())?;
        f(&exec)
    })
}

fn derive(
    exec: &pool::Exec,
    snap: Arc<Snapshot>,
    profile: &Profile,
    extras: &Extras,
    graphs: &[Id],
    opts: &ReasonOptions,
) -> anyhow::Result<Derivation> {
    exec.install(|| derive_here(snap, profile, extras, graphs, opts))
}

fn derive_here(
    snap: Arc<Snapshot>,
    profile: &Profile,
    extras: &Extras,
    graphs: &[Id],
    opts: &ReasonOptions,
) -> anyhow::Result<Derivation> {
    let rules = profile_rules(profile, extras)?;
    progress(opts, 0.0, "reading the input graphs");
    let budget = Budget::with_parent(opts.max_memory, opts.memory_parent.clone());
    let base = Arc::new(facts::Base::new(snap.clone(), graphs)?);
    let mut g = graph::Graph::with_base_in(base.clone(), &budget)?;
    let mut terms = Terms::new_in(snap, &budget)?;
    if let Some(context) = &opts.analysis {
        terms.set_analysis(context.clone())?;
    }
    let mut defaults = None;
    if extras.geo_default_geometry {
        let d = default_geometries_for_run(&base, &terms, &budget, opts)?;
        g.try_add_batch(d.0.iter().copied())?;
        defaults = Some(d);
    }
    let mut warnings = WorkingWarnings::new(&budget);
    if parser::uses_program(&rules) {
        // a rule program (item 839): its strata run in order, and it reads lists again
        // when their cells arrive, so it needs neither the closure operators nor a restart
        let info = engine::frontend::compile(&rules, &terms, &mut warnings, false)?;
        if let Some(context) = &opts.analysis {
            context.check()?;
        }
        let limits = engine::Limits {
            max_iterations: opts.max_iterations,
            max_inferred: opts.max_inferred,
            cancel: opts.cancel.clone(),
            deadline: None,
            progress: opts.progress.clone(),
            budget: budget.clone(),
            closures: false,
            magic: None,
            sampled_planning: opts.sampled_planning,
        };
        progress(opts, 0.1, "reasoning");
        let out = engine::run_program(&mut g, &info.program, &terms, &limits, &mut warnings)?;
        budget.check_peak()?;
        return Ok(Derivation {
            graph: g,
            terms,
            rules: info.program.rules.len(),
            iterations: out.iterations,
            warnings,
            budget,
            defaults,
            base_scans: base.scans(),
            threads: rayon::current_num_threads(),
        });
    }
    let compiled = engine::compile(&rules, &terms, &mut warnings)?;
    if let Some(context) = &opts.analysis {
        context.check()?;
    }
    let mut limits = engine::Limits {
        max_iterations: opts.max_iterations,
        max_inferred: opts.max_inferred,
        cancel: opts.cancel.clone(),
        deadline: None,
        progress: opts.progress.clone(),
        budget: budget.clone(),
        closures: opts.closure_operators && opts.analysis.is_none(),
        magic: None,
        sampled_planning: opts.sampled_planning,
    };
    progress(opts, 0.1, "reasoning");
    let out = loop {
        match engine::run(&mut g, &compiled, &terms, &limits, None) {
            Err(e) if limits.closures && e.is::<closure::Restart>() => {
                // the run derived list structure that its rules read: it starts again
                // with the rules evaluated as joins, so that their order of derivation
                // decides what the list builtins see
                tracing::debug!("{e}");
                limits.closures = false;
                drop(g);
                g = graph::Graph::with_base_in(base.clone(), &budget)?;
                if let Some(d) = &defaults {
                    g.try_add_batch(d.0.iter().copied())?;
                }
            }
            r => break r?,
        }
    };
    budget.check_peak()?;
    Ok(Derivation {
        graph: g,
        terms,
        rules: compiled.len(),
        iterations: out.iterations,
        warnings,
        budget,
        defaults,
        base_scans: base.scans(),
        threads: rayon::current_num_threads(),
    })
}

const GEO: &str = "http://www.opengis.net/ont/geosparql#";

/// The ids of `geo:hasGeometry` and `geo:hasDefaultGeometry`.
fn geo_ids(terms: &Terms) -> Result<(u64, u64), budget::BudgetFailure> {
    let iri = |l: &str| {
        terms.id_for(&Term::NamedNode(NamedNode::new_unchecked(format!(
            "{GEO}{l}"
        ))))
    };
    Ok((iri("hasGeometry")?, iri("hasDefaultGeometry")?))
}

/// Controlled [`geo_ids`], admitting vocabulary lookup scratch and polling live controls.
pub(crate) fn geo_ids_controlled(
    terms: &Terms,
    mut check: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<(u64, u64)> {
    let has = terms.try_id_for_controlled(
        &Term::NamedNode(NamedNode::new_unchecked(format!("{GEO}hasGeometry"))),
        &mut check,
    )?;
    let default = terms.try_id_for_controlled(
        &Term::NamedNode(NamedNode::new_unchecked(format!("{GEO}hasDefaultGeometry"))),
        &mut check,
    )?;
    Ok((has, default))
}

/// Room in `held` for one more key of a table of capacity `cap` with `len` keys of
/// `elem` bytes (with their values): the table it grows to is charged while the old one
/// is still charged.
fn room_for_key(
    held: &mut Held,
    cap: usize,
    len: usize,
    elem: usize,
) -> Result<(), budget::BudgetFailure> {
    let to = budget::table_grown(cap, len, 1);
    if to != cap {
        held.try_grow(budget::table_bytes(to, elem))?;
        held.shrink(budget::table_bytes(cap, elem));
    }
    Ok(())
}

/// `F geo:hasDefaultGeometry G` for every `F` of the input graphs with exactly one
/// `geo:hasGeometry` (`G`) and no `geo:hasDefaultGeometry` (Jena's
/// `applyDefaultGeometry`), sorted, with their charge to `budget`. Two scans of the
/// input graphs, one per property.
fn default_geometries_for_run(
    base: &facts::Base,
    terms: &Terms,
    budget: &Arc<Budget>,
    opts: &ReasonOptions,
) -> anyhow::Result<(Vec<[u64; 3]>, Held)> {
    let mut entered = false;
    let mut reported = false;
    default_geometries_controlled(base, terms, budget, || {
        if !entered {
            entered = true;
        } else if !reported {
            reported = true;
            progress(opts, 0.0, "finding default geometries");
        }
        if let Some(context) = &opts.analysis {
            context.check()?;
        }
        if opts
            .cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            anyhow::bail!("reasoning cancelled");
        }
        Ok(())
    })
}

fn default_geometries(
    base: &facts::Base,
    terms: &Terms,
    budget: &Arc<Budget>,
) -> anyhow::Result<(Vec<[u64; 3]>, Held)> {
    default_geometries_controlled(base, terms, budget, || Ok(()))
}

fn default_geometries_controlled(
    base: &facts::Base,
    terms: &Terms,
    budget: &Arc<Budget>,
    mut check: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<(Vec<[u64; 3]>, Held)> {
    check()?;
    let (has, default) = geo_ids_controlled(terms, &mut check)?;
    let mut held = Held::new(budget);
    let mut refused = None;
    let mut stopped = None;
    // feature → its only geometry (`None` once it has a second)
    let mut one: FxHashMap<u64, Option<u64>> = FxHashMap::default();
    let entry = size_of::<(u64, Option<u64>)>();
    base.for_each(None, Some(has), None, |t| {
        if let Err(e) = check() {
            stopped = Some(e);
            return false;
        }
        if !one.contains_key(&t[0])
            && let Err(e) = room_for_key(&mut held, one.capacity(), one.len(), entry)
        {
            refused = Some(e);
            return false;
        }
        one.entry(t[0])
            .and_modify(|x| {
                if *x != Some(t[2]) {
                    *x = None;
                }
            })
            .or_insert(Some(t[2]));
        true
    })?;
    if let Some(e) = stopped.take() {
        return Err(e);
    }
    let mut with_default: FxHashSet<u64> = FxHashSet::default();
    if refused.is_none() {
        base.for_each(None, Some(default), None, |t| {
            if let Err(e) = check() {
                stopped = Some(e);
                return false;
            }
            if one.contains_key(&t[0])
                && !with_default.contains(&t[0])
                && let Err(e) = room_for_key(
                    &mut held,
                    with_default.capacity(),
                    with_default.len(),
                    size_of::<u64>(),
                )
            {
                refused = Some(e);
                return false;
            }
            if one.contains_key(&t[0]) {
                with_default.insert(t[0]);
            }
            true
        })?;
    }
    if let Some(e) = stopped {
        return Err(e);
    }
    if let Some(e) = refused {
        return Err(e
            .with_context(" (finding the GeoSPARQL default geometries)".into())
            .into());
    }
    let mut n = 0;
    for (feature, geometry) in &one {
        check()?;
        if geometry.is_some() && !with_default.contains(feature) {
            n += 1;
        }
    }
    let tables = budget::table_bytes(one.capacity(), entry)
        + budget::table_bytes(with_default.capacity(), size_of::<u64>());
    held.try_grow(n * size_of::<[u64; 3]>())?;
    let mut out: Vec<[u64; 3]> = Vec::with_capacity(n);
    for (feature, geometry) in one {
        check()?;
        if !with_default.contains(&feature)
            && let Some(geometry) = geometry
        {
            out.push([feature, default, geometry]);
        }
    }
    drop(with_default);
    held.shrink(tables);
    out.sort_unstable();
    Ok((out, held))
}

/// Bring the default geometries of `closure` up to date with the changes `ch` of the
/// input graphs, and add their changes to `ch`: a default geometry is added as an
/// explicit fact, and one that no longer holds is removed as one. Returns the default
/// geometries that changed, which the inferred graph may have to follow even when the
/// explicit facts do not change.
///
/// Only the features whose `geo:hasGeometry` or `geo:hasDefaultGeometry` facts changed
/// can change, and each is read with two probes of the input graphs bound to the feature
/// and the property. The previous default geometries are the closure's when it knows
/// them (it came from the run before, kept in memory). A closure read from the store
/// does not know them: they are then the default geometries now, from two scans of the
/// input graphs, except for the changed features, whose facts then are their facts now
/// without the added ones and with the removed ones. Those are flagged explicit in the
/// closure, as a run that knows them has them.
fn default_geometry_changes(
    base: &facts::Base,
    closure: &mut incremental::Closure,
    ch: &mut maintain::Changes,
    budget: &Arc<Budget>,
) -> anyhow::Result<Vec<[u64; 3]>> {
    let (has, default) = geo_ids(&closure.terms)?;
    let geo = |t: &&[u64; 3]| t[1] == has || t[1] == default;
    let mut changed: Vec<u64> = ch
        .base_added
        .iter()
        .chain(&ch.base_removed)
        .filter(geo)
        .map(|t| t[0])
        .collect();
    changed.sort_unstable();
    changed.dedup();
    let added: FxHashSet<[u64; 3]> = ch.base_added.iter().filter(geo).copied().collect();
    let removed: FxHashSet<[u64; 3]> = ch.base_removed.iter().filter(geo).copied().collect();
    // the default geometry of a feature, given its facts of the two properties
    let of = |f: u64, facts: &[[u64; 3]]| -> Option<[u64; 3]> {
        if facts.iter().any(|t| t[1] == default) {
            return None;
        }
        let mut g = facts.iter().filter(|t| t[1] == has).map(|t| t[2]);
        let first = g.next()?;
        g.all(|x| x == first).then_some([f, default, first])
    };
    let known = closure.defaults.take();
    let unknown = known.is_none();
    let mut defaults = match known {
        Some(d) => d,
        None => {
            let (now, _held) = default_geometries(base, &closure.terms, budget)?;
            incremental::Defaults {
                pred: default,
                of: now
                    .iter()
                    .filter(|t| changed.binary_search(&t[0]).is_err())
                    .map(|t| (t[0], t[2]))
                    .collect(),
            }
        }
    };
    let (mut gone, mut new) = (Vec::new(), Vec::new());
    let mut then_of = Vec::new();
    for &f in &changed {
        let mut facts: Vec<[u64; 3]> = Vec::new();
        for p in [has, default] {
            base.for_each(Some(f), Some(p), None, |t| {
                facts.push(t);
                true
            })?;
        }
        let after = of(f, &facts);
        let then = if unknown {
            facts.retain(|t| !added.contains(t));
            facts.extend(removed.iter().filter(|t| t[0] == f));
            of(f, &facts)
        } else {
            defaults.of.get(&f).map(|g| [f, default, *g])
        };
        then_of.extend(then);
        if then != after {
            gone.extend(then);
            new.extend(after);
        }
    }
    if unknown {
        for (f, g) in then_of.iter().map(|t| (t[0], t[2])) {
            defaults.of.insert(f, g);
        }
        for d in defaults.facts() {
            if let Some(r) = closure.graph.position(&d) {
                closure.set_flag(r, true);
            }
        }
    }
    for d in &gone {
        defaults.of.remove(&d[0]);
    }
    for d in &new {
        defaults.of.insert(d[0], d[2]);
    }
    closure.defaults = Some(defaults);
    closure.settle()?;
    budget.check()?;
    let touched: Vec<[u64; 3]> = gone.iter().chain(&new).copied().collect();
    // a default geometry that is also stated, or no longer stated, does not change. It
    // only moves between the tiers: a stated one is in the base tier, and the derived
    // tier holds the others, flagged explicit
    for d in gone {
        match ch.base_added.iter().position(|t| *t == d) {
            Some(i) => {
                ch.base_added.swap_remove(i);
                if let Some(r) = closure.graph.position(&d) {
                    closure.set_flag(r, false);
                    closure.graph.kill(&d)?;
                }
            }
            None => ch.base_removed.push(d),
        }
    }
    for d in new {
        match ch.base_removed.iter().position(|t| *t == d) {
            Some(i) => {
                ch.base_removed.swap_remove(i);
                if closure.graph.position(&d).is_none() {
                    closure.graph.try_add_batch([d])?;
                }
                if let Some(r) = closure.graph.position(&d) {
                    closure.set_flag_with(r, true, true)?;
                }
            }
            None => ch.base_added.push(d),
        }
    }
    closure.settle()?;
    Ok(touched)
}

impl Derivation {
    /// The derived triples, without the facts of declared relations (item 839), which
    /// are rows of the derived tier but no RDF.
    fn derived(&self) -> impl Iterator<Item = [u64; 3]> + '_ {
        self.graph.facts().filter(|t| !ir::lower::is_key(t[1]))
    }

    /// Is this (possibly generalized) triple valid RDF?
    fn valid(&self, t: &[u64; 3]) -> bool {
        self.terms.valid(t)
    }

    /// The derived facts.
    fn closure_len(&self) -> usize {
        self.graph.derived_len()
    }

    /// The closure, for incremental runs: the derivation's graph as it is, the input
    /// graphs as its base tier and the derived facts in memory, with the default
    /// geometries flagged explicit.
    fn into_closure(self) -> anyhow::Result<incremental::Closure> {
        let Derivation {
            graph,
            terms,
            budget,
            defaults,
            ..
        } = self;
        if graph.base().is_none() {
            anyhow::bail!("a derivation without a base tier");
        }
        let mut c = incremental::Closure::new(graph, terms)?;
        if let Some((d, _held)) = defaults {
            let (_, pred) = geo_ids(&c.terms)?;
            let d = incremental::Defaults {
                pred,
                of: d.iter().map(|t| (t[0], t[2])).collect(),
            };
            for t in d.facts() {
                if let Some(r) = c.graph.position(&t) {
                    c.set_flag_with(r, true, true)?;
                }
            }
            c.defaults = Some(d);
            c.try_settle()?;
        }
        budget.check_peak()?;
        Ok(c)
    }
}

/// The rules of a profile with the axioms of the vocabularies of `extras`.
/// The profile or vocabulary each rule of [`profile_rules`] comes from, by its name.
pub(crate) fn rule_sources(profile: &Profile, extras: &Extras) -> anyhow::Result<Vec<String>> {
    let mut out = vec![profile.name().to_string(); profile.rules()?.len()];
    for v in &extras.vocabularies {
        out.extend(std::iter::repeat_n(v.name().to_string(), v.rules()?.len()));
    }
    Ok(out)
}

pub(crate) fn profile_rules(profile: &Profile, extras: &Extras) -> anyhow::Result<Vec<Rule>> {
    let mut rules = profile
        .rules()
        .with_context(|| format!("parsing rules for profile '{}'", profile.name()))?;
    for v in &extras.vocabularies {
        rules.extend(
            v.rules()
                .with_context(|| format!("parsing the {v} vocabulary"))?,
        );
    }
    Ok(rules)
}

/// The rules that an incremental run or a witness pass evaluates: Jena rules as
/// [`engine::compile`] gives them, or the lowered rules of a rule program (item 839) of
/// every stratum, with its relation part, which an update rebuilds when the closure
/// lacks its relation facts.
enum RunRules {
    Jena(engine::CompiledRules),
    Program {
        rules: Vec<engine::CRule>,
        part: Option<Box<ir::Program>>,
        /// a stratum after the first: the program negates, and is not maintained
        strata: usize,
        _held: Box<(Held, ir::lower::Lowered, engine::frontend::ProgramInfo)>,
    },
}

impl std::ops::Deref for RunRules {
    type Target = [engine::CRule];
    fn deref(&self) -> &[engine::CRule] {
        match self {
            RunRules::Jena(c) => c,
            RunRules::Program { rules, .. } => rules,
        }
    }
}

impl RunRules {
    /// Compile `rules` over `terms`, through the frontend when they are a rule program.
    fn new(
        rules: &[Rule],
        terms: &Terms,
        warnings: &mut WorkingWarnings,
    ) -> anyhow::Result<RunRules> {
        if !parser::uses_program(rules) {
            return Ok(RunRules::Jena(engine::compile(rules, terms, warnings)?));
        }
        let budget = terms.budget();
        let info = engine::frontend::compile(rules, terms, warnings, false)?;
        let mut lowered = info
            .program
            .lower(&budget)
            .map_err(ir::lower::LowerError::into_anyhow)?;
        let strata = lowered.strata.len();
        let part = info
            .program
            .relation_part(&ir::Vocab::of(terms), &budget)
            .map_err(|e| match e {
                ir::ProgramError::Memory(m) => anyhow::Error::from(m),
                e => anyhow::anyhow!("{e}"),
            })?;
        let mut held = Held::new(&budget);
        let (flat, _infos) =
            explain::flat_rules(&info.program, &mut lowered, &mut held, &info.lines)?;
        Ok(RunRules::Program {
            rules: flat,
            part: part.map(Box::new),
            strata,
            _held: Box::new((held, lowered, info)),
        })
    }

    /// The relation part of a rule program.
    fn part(&self) -> Option<&ir::Program> {
        match self {
            RunRules::Jena(_) => None,
            RunRules::Program { part, .. } => part.as_deref(),
        }
    }

    /// Why the closure of these rules is not maintained incrementally, if it is not.
    fn blocker(&self, terms: &Terms) -> Option<String> {
        if let RunRules::Program { strata, .. } = self
            && *strata > 1
        {
            return Some(
                "the rule program negates an atom, so it runs by strata and in full on every update"
                    .to_string(),
            );
        }
        self.iter()
            .find_map(|r| engine::incremental_blocker(r, terms))
    }
}

/// The most rule sets that [`incremental_rules`] keeps.
const KEPT_RULE_SETS: usize = 8;

/// The most bytes of parsed rules, as [`rules_bytes`] estimates them, that the kept rule
/// sets hold in all. A larger rule set is not kept, and its runs parse it every time.
const KEPT_RULE_BYTES: usize = 4 << 20;

/// The parsed rule sets of the incremental runs made last, oldest first, by their digest
/// ([`rules_digest`]). They stay with the process and no run's budget counts them, so
/// their number and their size are bounded.
struct KeptRuleSets {
    sets: Vec<KeptRules>,
    /// the bytes of `sets`
    bytes: usize,
}

/// A kept rule set: its digest, the estimate of its bytes, its rules, and why they cannot
/// be maintained incrementally, if they cannot.
struct KeptRules {
    digest: u64,
    bytes: usize,
    rules: Arc<Vec<Rule>>,
    blocker: Option<String>,
}

impl KeptRuleSets {
    const fn new() -> KeptRuleSets {
        KeptRuleSets {
            sets: Vec::new(),
            bytes: 0,
        }
    }

    fn get(&self, digest: u64) -> Option<(Arc<Vec<Rule>>, Option<String>)> {
        self.sets
            .iter()
            .find(|k| k.digest == digest)
            .map(|k| (k.rules.clone(), k.blocker.clone()))
    }

    /// Keep `set` within `max_sets` sets and `max_bytes` bytes in all, in place of the
    /// oldest sets. A set over `max_bytes` by itself is not kept.
    fn keep(&mut self, set: KeptRules, max_sets: usize, max_bytes: usize) {
        if set.bytes > max_bytes || max_sets == 0 || self.get(set.digest).is_some() {
            return;
        }
        while self.sets.len() == max_sets || self.bytes + set.bytes > max_bytes {
            self.bytes -= self.sets.remove(0).bytes;
        }
        self.bytes += set.bytes;
        self.sets.push(set);
    }
}

static INCREMENTAL_RULES: std::sync::Mutex<KeptRuleSets> =
    std::sync::Mutex::new(KeptRuleSets::new());

/// An estimate of the memory of parsed rules: their clauses and nodes with the text of
/// their names, variables and constants. A rule that an `@include` or a prefix expands
/// counts at its parsed size, which the length of the rule text does not bound.
fn rules_bytes(rules: &[Rule]) -> usize {
    fn node(n: &Node) -> usize {
        size_of::<Node>()
            + match n {
                Node::Var(v) => v.len(),
                Node::Const(t) => t.to_string().len(),
                Node::Any => 0,
                Node::Functor(name, args) => name.len() + args.iter().map(node).sum::<usize>(),
            }
    }
    fn clause(c: &Clause) -> usize {
        size_of::<Clause>()
            + match c {
                Clause::Triple(t) => node(&t.subject) + node(&t.predicate) + node(&t.object),
                Clause::Builtin(b) => b.name.len() + b.args.iter().map(node).sum::<usize>(),
                Clause::Rule(r) => rule(r),
                Clause::Relation(a) => a.name.len() + a.args.iter().map(node).sum::<usize>(),
                Clause::Not(c) => clause(c),
            }
    }
    fn rule(r: &Rule) -> usize {
        size_of::<Rule>()
            + r.name.as_ref().map_or(0, |n| n.len())
            + r.body.iter().chain(&r.head).map(clause).sum::<usize>()
    }
    rules.iter().map(rule).sum()
}

/// The parsed rules of a profile and its extras for an incremental run, and why they
/// keep a materialization from being maintained incrementally, if they do
/// ([`engine::incremental_blocker`]). Both follow from the rule text alone, which the
/// digest covers, so they are kept for the next runs with the same digest: a run after
/// each small write would otherwise parse the profile and compile it for the check
/// every time, about a tenth of the time of a change of 100 triples. The kept sets cost
/// memory that outlives the runs, at most [`KEPT_RULE_SETS`] sets of
/// [`KEPT_RULE_BYTES`] bytes in all.
fn incremental_rules(
    profile: &Profile,
    extras: &Extras,
    digest: u64,
    snap: &Arc<Snapshot>,
    budget: &Arc<Budget>,
) -> anyhow::Result<(Arc<Vec<Rule>>, Option<String>)> {
    let kept = INCREMENTAL_RULES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(digest);
    if let Some(found) = kept {
        return Ok(found);
    }
    let rules = Arc::new(profile_rules(profile, extras)?);
    let terms = Terms::new_in(snap.clone(), budget)?;
    let mut w = WorkingWarnings::new(budget);
    // a rule program that cannot be lowered, such as one with an existential head, has
    // its error reported by the full run; transient memory or core refusals must propagate
    // without poisoning the digest's cached incremental capability.
    let blocker = match RunRules::new(&rules, &terms, &mut w) {
        Ok(compiled) => compiled.blocker(&terms),
        Err(e)
            if budget::BudgetFailure::from_anyhow(&e).is_some()
                || e.downcast_ref::<sparkles_core::Error>().is_some() =>
        {
            return Err(e);
        }
        Err(e) if parser::uses_program(&rules) => Some(format!("{e:#}")),
        Err(e) => return Err(e),
    };
    let set = KeptRules {
        digest,
        bytes: rules_bytes(&rules),
        rules: rules.clone(),
        blocker: blocker.clone(),
    };
    INCREMENTAL_RULES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .keep(set, KEPT_RULE_SETS, KEPT_RULE_BYTES);
    Ok((rules, blocker))
}

/// A digest of everything that determines a materialization besides the data: the rule
/// text of the profile and its vocabularies, and the switches of `extras`. An incremental
/// run needs the previous run's digest to be the same.
pub fn rules_digest(profile: &Profile, extras: &Extras) -> u64 {
    use maintain::{FNV_START, fnv};
    let mut h = fnv(b"sparkles-reasoner closure 1\0", FNV_START);
    h = fnv(profile.name().as_bytes(), h);
    h = fnv(b"\0", h);
    h = match profile {
        Profile::ShaclAf(c) => fnv(c.text().as_bytes(), h),
        _ => fnv(profile.text().as_bytes(), h),
    };
    for v in &extras.vocabularies {
        h = fnv(b"\0", h);
        h = fnv(v.text().as_bytes(), h);
    }
    fnv(&[extras.geo_default_geometry as u8], h)
}

/// A run that removes more than one explicit triple in this many (of 10,000 or more)
/// runs in full: the search for other proofs of their consequences then costs more than
/// deriving everything again (measured on the benchmark data: removing 10% of 100,000 triples
/// took longer incrementally, removing 1% of 1,000,000 took a quarter of a full run).
const LARGE_DELETION: u64 = 20;

/// What an incremental run asks for.
#[derive(Clone, Copy, Default)]
pub struct Incremental<'a> {
    /// The commit of the materialization to update: the one its recorded status names,
    /// when that status belongs to this dataset. `None` materializes in full.
    pub since: Option<u64>,
    /// The closure kept in memory between runs. Without it, a run reads the previous
    /// closure from the dataset (persistent datasets only).
    pub cache: Option<&'a Cache>,
}

/// [`materialize`] with the vocabularies and switches of `extras` added to the profile.
///
/// A vocabulary's axioms join the rules (their triples, and what the rules derive from
/// them, land in [`INFERRED_GRAPH`] like the RDFS axiomatic triples); the GeoSPARQL
/// default geometries are derived before the rules run, so the rules see them, and are
/// written with the other derived triples (a re-run or [`clear`] keeps them consistent).
///
/// The first progress report comes after the store's writer lock is taken. The cancel
/// flag is checked during the rule iterations and once more before the commit, and a
/// cancelled run changes nothing.
pub fn materialize_with(
    store: &Store,
    profile: &Profile,
    extras: &Extras,
    opts: &ReasonOptions,
) -> anyhow::Result<ReasonReport> {
    materialize_incremental(store, profile, extras, Incremental::default(), opts)
}

/// [`materialize_with`], updating the materialization of commit `inc.since`
/// incrementally when it can.
///
/// An incremental run takes the explicit triples added to and removed from the default
/// graph since that commit (the commit diff), and maintains the previous closure with the
/// backward/forward algorithm: it deletes the consequences of removed triples that have
/// no other proof, then derives the consequences of added ones semi-naively. The
/// inferred graph it writes is the one a full run would write.
///
/// It needs the previous closure, from `inc.cache` or saved with a persistent dataset,
/// the same rules (see [`rules_digest`]), and a commit the diff still reaches. Rules
/// that are not monotonic, or that create blank nodes, need a full run, and so do RDF
/// lists that change while the rules read lists. The run then materializes in full and
/// says why in [`ReasonReport::fallback`].
///
/// A run is admitted to [`ReasonOptions::pool`] before it takes the writer lock, and a
/// run called on a rayon worker thread runs on a native thread while the worker waits
/// (see [`ReasoningPool`]).
pub fn materialize_incremental(
    store: &Store,
    profile: &Profile,
    extras: &Extras,
    inc: Incremental<'_>,
    opts: &ReasonOptions,
) -> anyhow::Result<ReasonReport> {
    extras.validate()?;
    opts.inputs.validate()?;
    refuse_query_only(profile)?;
    let t0 = Instant::now();
    admitted(opts, |exec| {
        materialize_admitted(exec, store, profile, extras, inc, opts, t0)
    })
}

#[allow(clippy::too_many_arguments)]
fn materialize_admitted(
    exec: &pool::Exec,
    store: &Store,
    profile: &Profile,
    extras: &Extras,
    inc: Incremental<'_>,
    opts: &ReasonOptions,
    t0: Instant,
) -> anyhow::Result<ReasonReport> {
    let mut laps = Laps::new(t0);
    let (mut txn, raw, resolved) = lock_with_changes(store, inc, &opts.inputs, &mut laps);
    let snap = txn.base().clone();
    let inspected = match &opts.inspect {
        Some(f) => {
            // the first progress report says the run holds the lock, so that a waiting
            // write can supersede it during the check
            progress(opts, 0.05, "checking the input axioms");
            f(&snap)?
        }
        None => Vec::new(),
    };
    let digest = rules_digest(profile, extras);
    let mut fallback = None;
    if let Profile::ShaclAf(cfg) = profile {
        if inc.since.is_some() {
            fallback = Some(
                "SHACL-AF rules are materialized in full; their maintenance follows roadmap \
                 item 958"
                    .to_string(),
            );
        }
        if !extras.vocabularies.is_empty() || extras.geo_default_geometry {
            anyhow::bail!("the shacl-af profile takes no vocabularies or default geometries");
        }
        let ids = resolved.ids(&snap);
        // the run interns the terms its rules compute in the transaction: a run that
        // fails, is cancelled or passes its budget before the commit removes them again
        let run = (|| -> anyhow::Result<_> {
            let (d, groups) = shacl_af::derive(&mut txn, snap.clone(), cfg, &ids, opts)?;
            laps.lap("derive");
            progress(opts, 0.8, "writing inferred graph");
            let mut warnings = WorkingWarnings::try_clone_from(d.warnings.as_slice(), &d.budget)?;
            warnings.extend_borrowed(&inspected)?;
            if wanted_witnesses(opts)?.is_some() {
                warnings.push_fmt(format_args!(
                    "the shacl-af profile keeps no witnesses of its inferences"
                ))?;
            }
            let w = write_full(&mut txn, &snap, &d, opts)?;
            laps.lap("write");
            Ok((d, groups, warnings, w))
        })();
        let (d, groups, warnings, w) = match run {
            Ok(r) => r,
            Err(e) => {
                if let Err(r) = txn.discard_terms() {
                    tracing::error!("a failed shacl-af run could not remove its terms: {r}");
                }
                return Err(e);
            }
        };
        // the shapes graphs the rules came from make the inferences stale as well
        let mut resolved = resolved.clone();
        for g in &cfg.shapes {
            let g = if g == sparkles_core::sparql::ctx::DEFAULT_GRAPH_IRI {
                GraphRef::Default
            } else {
                GraphRef::Named(g.clone())
            };
            if !resolved.graphs.contains(&g) && !resolved.also_read.contains(&g) {
                resolved.also_read.push(g);
            }
        }
        let done = Done {
            rule_groups: groups,
            rules: d.rules,
            iterations: d.iterations,
            budget: d.budget.clone(),
            base_scans: d.base_scans,
            threads: d.threads,
            closure: None,
            witnesses: None,
            generalized: FxHashSet::default(),
            generalized_saved: None,
            written: w,
            warnings,
            method: Method::Full,
            fallback,
            changes: None,
        };
        return finish(
            store, txn, &snap, profile, digest, &resolved, inc, opts, t0, done, laps,
        );
    }
    if let (Some(since), Some(raw)) = (inc.since, raw) {
        match update_closure(
            exec, store, &mut txn, &snap, profile, extras, since, digest, &resolved, inc, raw,
            opts, &mut laps,
        ) {
            Ok(mut done) => {
                done.warnings.extend_borrowed(&inspected)?;
                return finish(
                    store, txn, &snap, profile, digest, &resolved, inc, opts, t0, done, laps,
                );
            }
            Err(e) => {
                let f = e.downcast::<Fallback>()?;
                tracing::info!(reason = %f.0, "materializing in full");
                fallback = Some(f.0);
            }
        }
    }
    let d = derive(
        exec,
        snap.clone(),
        profile,
        extras,
        &resolved.ids(&snap),
        opts,
    )?;
    laps.lap("derive");
    progress(opts, 0.8, "writing inferred graph");
    let mut warnings = WorkingWarnings::try_clone_from(d.warnings.as_slice(), &d.budget)?;
    warnings.extend_borrowed(&inspected)?;
    let w = write_full(&mut txn, &snap, &d, opts)?;
    laps.lap("write");
    if w.generalized > 0 {
        warnings.push_fmt(format_args!(
            "{} derived generalized triples (literal subjects or non-IRI predicates) were not written",
            w.generalized
        ))?;
    }
    let generalized: FxHashSet<[u64; 3]> = d.derived().filter(|t| !d.valid(t)).collect();
    let (rules, iterations, budget) = (d.rules, d.iterations, d.budget.clone());
    let (mut base_scans, threads) = (d.base_scans, d.threads);
    let (closure, witnesses) = match wanted_witnesses(opts)? {
        Some(max) => {
            progress(opts, 0.9, "recording witnesses");
            let mut c = d.into_closure()?;
            let w =
                exec.install(|| build_witnesses(&mut c, profile, extras, max, &budget, opts))?;
            // the witness pass reads the input graphs too
            if let Some(b) = c.graph.base() {
                base_scans = base_scans.max(b.scans());
            }
            (ClosureState::Ready(c), Some(w))
        }
        None => (ClosureState::Derived(d), None),
    };
    let done = Done {
        rule_groups: Vec::new(),
        rules,
        iterations,
        budget,
        base_scans,
        threads,
        closure: Some(closure),
        witnesses,
        generalized,
        generalized_saved: None,
        written: w,
        warnings,
        method: Method::Full,
        fallback,
        changes: None,
    };
    finish(
        store, txn, &snap, profile, digest, &resolved, inc, opts, t0, done, laps,
    )
}

/// Take the writer lock, with the input graphs resolved in the state it locks, and the
/// changes since `inc.since` up to that state.
///
/// The commit diff takes the writer lock itself, so the changes are read before. A commit
/// that lands in between makes them stale. A closure kept in memory then gives the
/// changes from its deltas, under the lock; otherwise the lock is released and they are
/// read again, up to three times. The changes are those of the input graphs that the
/// state before the lock resolves; when the locked state resolves others, the run is a
/// full one.
fn lock_with_changes<'s>(
    store: &'s Store,
    inc: Incremental<'_>,
    inputs: &Inputs,
    laps: &mut Laps,
) -> (
    sparkles_core::store::WriteTxn<'s>,
    Option<Result<maintain::RawChanges, String>>,
    Resolved,
) {
    let locked = |txn: sparkles_core::store::WriteTxn<'s>| {
        let r = inputs::resolve(txn.base(), inputs);
        (txn, r)
    };
    let Some(since) = inc.since else {
        let (txn, r) = locked(store.write_as(sparkles_core::commit::CommitKind::Reason));
        laps.lap("lock");
        return (txn, None, r);
    };
    for _ in 0..3 {
        let live = store.snapshot();
        let graphs = inputs::resolve(&live, inputs).graph_set();
        let raw = maintain::changes(store, since, &live, inc.cache, &graphs);
        laps.lap("changes");
        let (txn, r) = locked(store.write_as(sparkles_core::commit::CommitKind::Reason));
        laps.lap("lock");
        let raw = if r.graph_set() != graphs {
            Err("the input graphs changed while the changes were read".into())
        } else if txn.base().commit == live.commit {
            raw
        } else if let Some(raw) =
            maintain::kept_changes(store, since, txn.base(), inc.cache, &graphs)
        {
            raw
        } else {
            continue;
        };
        return (txn, Some(raw), r);
    }
    let (txn, r) = locked(store.write_as(sparkles_core::commit::CommitKind::Reason));
    laps.lap("lock");
    (
        txn,
        Some(Err(
            "commits kept arriving while the changes were read".into()
        )),
        r,
    )
}

/// The closure a run ends with, for the next incremental run.
enum ClosureState {
    /// an incremental run's
    Ready(incremental::Closure),
    /// a full run's derived facts over the input graphs
    Derived(Derivation),
}

impl ClosureState {
    fn terms(&self) -> &Terms {
        match self {
            ClosureState::Ready(c) => &c.terms,
            ClosureState::Derived(d) => &d.terms,
        }
    }

    /// The closure in memory, when a cache would keep it.
    fn for_cache(self, cache: &Cache) -> Option<incremental::Closure> {
        match self {
            ClosureState::Ready(c) => Some(c),
            ClosureState::Derived(d) if d.closure_len() <= cache.limit() => d
                .into_closure()
                .map_err(|e| tracing::warn!("keeping the closure for incremental runs: {e:#}"))
                .ok(),
            ClosureState::Derived(_) => None,
        }
    }
}

/// The bound of witnesses a run keeps, checked: none for an entailment analysis.
fn wanted_witnesses(opts: &ReasonOptions) -> anyhow::Result<Option<usize>> {
    match opts.witnesses {
        Some(max) if opts.analysis.is_none() => {
            witness::Witnesses::check_max(max)?;
            Ok(Some(max))
        }
        _ => Ok(None),
    }
}

/// The limits of the witness passes of a run.
fn witness_limits(budget: &Arc<Budget>, opts: &ReasonOptions) -> engine::Limits {
    engine::Limits {
        max_iterations: opts.max_iterations,
        max_inferred: opts.max_inferred,
        cancel: opts.cancel.clone(),
        deadline: None,
        progress: None,
        budget: budget.clone(),
        closures: false,
        magic: None,
        sampled_planning: false,
    }
}

/// Build the witnesses of a closure.
fn build_witnesses(
    c: &mut incremental::Closure,
    profile: &Profile,
    extras: &Extras,
    max: usize,
    budget: &Arc<Budget>,
    opts: &ReasonOptions,
) -> anyhow::Result<witness::Witnesses> {
    let rules = profile_rules(profile, extras)?;
    let mut warnings = WorkingWarnings::new(budget);
    let compiled = RunRules::new(&rules, &c.terms, &mut warnings)?;
    witness::Witnesses::build(c, &compiled, max, &witness_limits(budget, opts))
}

/// A run's result before its commit.
struct Done {
    /// the groups of a `shacl-af` run
    rule_groups: Vec<shacl_af::RuleGroup>,
    /// the closure, for the next incremental run
    closure: Option<ClosureState>,
    /// the witnesses of the closure's facts, when the run keeps them
    witnesses: Option<witness::Witnesses>,
    /// the run's memory, whose peak the report gives
    budget: Arc<Budget>,
    base_scans: u64,
    threads: usize,
    generalized: FxHashSet<[u64; 3]>,
    /// the saved generalized facts the run started from, and its changes to them
    generalized_saved: Option<(maintain::SavedMeta, maintain::GeneralizedChanges)>,
    written: Written,
    rules: usize,
    iterations: usize,
    warnings: WorkingWarnings,
    method: Method,
    fallback: Option<String>,
    changes: Option<Changes>,
}

/// What a run wrote to the inferred graph.
#[derive(Default)]
struct Written {
    added: u64,
    removed: u64,
    /// triples in the inferred graph after the run
    inferred: u64,
    /// derived triples that are not valid RDF, not written
    generalized: u64,
    /// local ids the run stored, with their store ids
    stored: FxHashMap<u64, Id>,
    /// the change written, when the run reports it ([`ReasonOptions::inferred_change`]),
    /// with its charge
    change: Option<(InferredChange, Held)>,
}

/// A change of [`INFERRED_GRAPH`]: triples with weights `+1` and `-1`.
type InferredChange = Vec<([Id; 3], i64)>;

#[allow(clippy::too_many_arguments)]
fn finish(
    store: &Store,
    txn: sparkles_core::store::WriteTxn<'_>,
    snap: &Arc<Snapshot>,
    profile: &Profile,
    digest: u64,
    resolved: &Resolved,
    inc: Incremental<'_>,
    opts: &ReasonOptions,
    t0: Instant,
    done: Done,
    mut laps: Laps,
) -> anyhow::Result<ReasonReport> {
    let mut txn = txn;
    let checked = (|| -> anyhow::Result<WorkingWarnings> {
        if opts
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        {
            anyhow::bail!("reasoning cancelled");
        }
        // a run that went over its limit fails before its commit, also when its charges
        // are back under it now
        done.budget.check_peak()?;
        if let Some(context) = &opts.analysis {
            if store.is_persistent() {
                anyhow::bail!("analysis witnesses cannot be persisted");
            }
            if let Some(closure) = &done.closure {
                context.capture_typings(closure.terms(), &done.generalized)?;
            } else {
                anyhow::bail!("analysis closure was unavailable before diagnostics capture");
            }
        }
        // Required report storage must be admitted while the transaction can still
        // roll back. Keep the complete warning owner alive through commit/transfer.
        let mut warnings = WorkingWarnings::try_clone_from(&resolved.warnings, &done.budget)?;
        warnings.extend_borrowed(done.warnings.as_slice())?;
        done.budget.check_peak()?;
        Ok(warnings)
    })();
    let warnings = match checked {
        Ok(w) => w,
        Err(e) => {
            // a shacl-af run interned the terms its rules computed: nothing commits them
            if !done.rule_groups.is_empty()
                && let Err(r) = txn.discard_terms()
            {
                tracing::error!("a failed shacl-af run could not remove its terms: {r}");
            }
            return Err(e);
        }
    };
    laps.lap("checks");
    let receipt = txn.commit()?;
    laps.lap("commit");
    progress(opts, 1.0, "done");
    let graphs = resolved.graph_set();
    let mut kept = false;
    let witness_report = done.witnesses.as_ref().map(|w| w.report.clone());
    if let Some(state) = done.closure {
        let saved = maintain::save(
            store,
            &receipt,
            digest,
            &graphs,
            state.terms(),
            &done.generalized,
            done.generalized_saved.as_ref().map(|(m, c)| (m, c)),
        )
        .unwrap_or_else(|e| {
            tracing::warn!("saving the closure state for incremental runs: {e:#}");
            None
        });
        laps.lap("save");
        // keeping the closure after the commit is charged strictly: a closure that does
        // not fit is not kept, and the run still succeeds
        if let Some(cache) = inc.cache
            && let Some(closure) = state.for_cache(cache)
        {
            match maintain::keep(
                store,
                inc.cache,
                &receipt,
                snap,
                digest,
                &graphs,
                closure,
                done.witnesses,
                done.generalized,
                &done.written.stored,
                saved,
            ) {
                Ok(k) => kept = k,
                Err(e) => tracing::warn!("keeping the closure for incremental runs: {e:#}"),
            }
        }
    }
    if !kept && let Some(c) = inc.cache {
        c.clear();
    }
    laps.lap("keep");
    let report = ReasonReport {
        profile: profile.name().to_string(),
        rules: done.rules,
        iterations: done.iterations,
        inferred: done.written.inferred,
        millis: t0.elapsed().as_millis() as u64,
        warnings: warnings.into_legacy_report(),
        receipt: Some(receipt),
        method: done.method,
        fallback: done.fallback,
        changes: done.changes,
        inferred_added: done.written.added,
        inferred_removed: done.written.removed,
        inferred_change: done.written.change.map(|(c, _)| c),
        inputs: Some(resolved.clone()),
        peak_bytes: done.budget.peak() as u64,
        base_scans: done.base_scans,
        threads: done.threads,
        witnesses: witness_report,
        max_rounds: match profile {
            Profile::ShaclAf(c) => Some(c.max_rounds),
            _ => None,
        },
        rule_groups: done.rule_groups,
    };
    tracing::info!(
        profile = %report.profile,
        method = report.method.as_str(),
        rules = report.rules,
        iterations = report.iterations,
        inferred = report.inferred,
        added = report.inferred_added,
        removed = report.inferred_removed,
        millis = report.millis,
        "materialized inferences"
    );
    tracing::trace!(
        target: "sparkles_reasoner::timing",
        method = report.method.as_str(),
        micros = t0.elapsed().as_micros() as u64,
        phases = %laps,
        "run phases"
    );
    Ok(report)
}

/// Map a derived triple's local ids to store ids, interning the terms.
fn store_ids(
    txn: &mut sparkles_core::store::WriteTxn<'_>,
    terms: &Terms,
    stored: &mut FxHashMap<u64, Id>,
    t: &[u64; 3],
) -> anyhow::Result<[Id; 3]> {
    let mut ids = [Id::UNDEF; 3];
    for (i, &x) in t.iter().enumerate() {
        ids[i] = if Id(x).tag() == Tag::Local {
            match stored.get(&x) {
                Some(&id) => id,
                None => {
                    let id = match terms.local(x) {
                        Some(LocalTerm::Key(k)) => txn.intern_key(&k)?,
                        Some(LocalTerm::BNode | LocalTerm::RuleBNode(_)) => txn.new_bnode(),
                        Some(LocalTerm::Tail(_)) => {
                            anyhow::bail!("a relation tail {x:#x} reached the store")
                        }
                        None => anyhow::bail!("dangling local term id {x:#x}"),
                    };
                    stored.insert(x, id);
                    id
                }
            }
        } else {
            Id(x)
        };
    }
    Ok(ids)
}

/// Replace the inferred graph with the valid derived triples of a full run.
fn write_full(
    txn: &mut sparkles_core::store::WriteTxn<'_>,
    snap: &Snapshot,
    d: &Derivation,
    opts: &ReasonOptions,
) -> anyhow::Result<Written> {
    let mut w = Written::default();
    let fact = size_of::<[Id; 3]>();
    let n = d.closure_len();
    // the new graph's triples and their set
    let mut held = Held::try_new(
        &d.budget,
        n * fact + budget::table_bytes(budget::table_capacity(n), fact),
    )
    .map_err(|e| e.with_context(" (writing the inferred graph)".into()))?;
    // existing inferred graph
    let old_graph = snap.lookup_iri(INFERRED_GRAPH);
    let old: Vec<[Id; 3]> = match old_graph {
        Some(g) => {
            let m = snap.count(Perm::Gspo, &[g.0])? as usize;
            held.try_grow(m * fact + budget::table_bytes(budget::table_capacity(m.min(n)), fact))
                .map_err(|e| e.with_context(" (writing the inferred graph)".into()))?;
            let mut old = Vec::with_capacity(m);
            snap.scan(Perm::Gspo, &[g.0], |c| {
                match c {
                    Chunk::Block(b, s, e) => old.extend(
                        (s..e).map(|i| [Id(b.cols[1][i]), Id(b.cols[2][i]), Id(b.cols[3][i])]),
                    ),
                    Chunk::Row(k) => old.push([Id(k[1]), Id(k[2]), Id(k[3])]),
                }
                Ok(true)
            })?;
            old
        }
        None => Vec::new(),
    };

    // map local ids to store ids and collect the new graph content
    let mut new_triples: Vec<[Id; 3]> = Vec::with_capacity(n);
    for t in d.derived() {
        if !d.valid(&t) {
            w.generalized += 1;
            continue;
        }
        new_triples.push(store_ids(txn, &d.terms, &mut w.stored, &t)?);
    }

    let new_set: FxHashSet<[Id; 3]> = new_triples.iter().copied().collect();
    let mut old_set: FxHashSet<[Id; 3]> = FxHashSet::default();
    for t in old {
        if !new_set.contains(&t) {
            let g = old_graph.expect("present");
            txn.delete([t[0], t[1], t[2], g])?;
            w.removed += 1;
        } else {
            old_set.insert(t);
        }
    }
    if !new_triples.is_empty() {
        let g = match old_graph {
            Some(g) => g,
            None => txn.intern(&Term::NamedNode(NamedNode::new_unchecked(INFERRED_GRAPH)))?,
        };
        held.try_grow(new_triples.len() * size_of::<[Id; 4]>())
            .map_err(|e| e.with_context(" (writing the inferred graph)".into()))?;
        let added: Vec<[Id; 4]> = new_triples
            .iter()
            .filter(|t| !old_set.contains(*t))
            .map(|t| [t[0], t[1], t[2], g])
            .collect();
        w.added = added.len() as u64;
        if opts
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        {
            anyhow::bail!("reasoning cancelled");
        }
        // large batches are merged into a rebuilt index generation on commit
        txn.insert_bulk(added)?;
    }
    w.inferred = new_set.len() as u64;
    drop(held);
    Ok(w)
}

/// Update the closure of the materialization at `since` to the store's state now, and
/// write the changes of the inferred graph.
#[allow(clippy::too_many_arguments)]
fn update_closure(
    exec: &pool::Exec,
    store: &Store,
    txn: &mut sparkles_core::store::WriteTxn<'_>,
    snap: &Arc<Snapshot>,
    profile: &Profile,
    extras: &Extras,
    since: u64,
    digest: u64,
    resolved: &Resolved,
    inc: Incremental<'_>,
    raw: Result<maintain::RawChanges, String>,
    opts: &ReasonOptions,
    laps: &mut Laps,
) -> anyhow::Result<Done> {
    if since > snap.commit {
        return Err(Fallback("the store position moved backwards".into()).into());
    }
    let budget = Budget::with_parent(opts.max_memory, opts.memory_parent.clone());
    let (rules, blocker) = incremental_rules(profile, extras, digest, snap, &budget)?;
    if let Some(why) = blocker {
        return Err(Fallback(why).into());
    }
    let graphs = resolved.graph_set();
    if let Ok(r) = &raw {
        let removed = r.base_removed() as u64;
        let mut explicit = removed;
        for g in resolved.ids(snap) {
            explicit += snap.count(Perm::Gspo, &[g.0])?;
        }
        if explicit >= 10_000 && removed * LARGE_DELETION > explicit {
            return Err(Fallback(format!(
                "{removed} of {explicit} explicit triples were removed, more than a full run handles faster"
            ))
            .into());
        }
    }
    laps.lap("rules");
    progress(opts, 0.0, "reading the previous closure");
    // on the run's threads: this thread may stand in for a parked worker of the pool
    // that called, so its parallel work must not go to that pool
    let run_ids = resolved.ids(snap);
    let wanted = wanted_witnesses(opts)?;
    let prev = exec.install(|| {
        maintain::previous(
            store, snap, since, digest, &graphs, &run_ids, inc.cache, raw, &budget, wanted,
        )
    })?;
    laps.lap("closure");
    let mut closure = prev.closure;
    let mut ch = prev.changes;
    let _prev_held = prev.held;
    // the input graphs now, the closure's base tier, whose scans the run reports
    let Some(base) = closure.graph.base_arc() else {
        anyhow::bail!("a closure without a base tier");
    };
    // the default geometries count as explicit facts, so that the update keeps the facts
    // derived from them; those of the changed features change with the explicit ones
    let touched = if extras.geo_default_geometry {
        exec.install(|| default_geometry_changes(&base, &mut closure, &mut ch, &budget))?
    } else {
        Vec::new()
    };
    let mut warnings = WorkingWarnings::new(&budget);
    let compiled = RunRules::new(&rules, &closure.terms, &mut warnings)?;
    // a closure read back from the dataset has no relation facts, which the update
    // rebuilds first (item 839's plan, §12.2)
    let rebuild = (prev.source != "memory").then(|| compiled.part()).flatten();
    let limits = engine::Limits {
        max_iterations: opts.max_iterations,
        max_inferred: opts.max_inferred,
        cancel: opts.cancel.clone(),
        deadline: None,
        progress: opts.progress.clone(),
        budget: budget.clone(),
        closures: false,
        magic: None,
        sampled_planning: opts.sampled_planning,
    };
    let ch = &ch;
    // the witnesses of the previous closure, when the run keeps witnesses with the same
    // bound; otherwise they are built again after the update. With default geometries the
    // change set no longer shows which input quads went away, since a removed default can
    // come back as a synthetic one, so the witnesses are built again
    let mut prior = match (wanted, prev.witnesses) {
        (Some(max), Some(w)) if w.max() == max && !extras.geo_default_geometry => Some(w),
        _ => None,
    };
    let wl = witness_limits(&budget, opts);
    let mut prepared: Option<witness::Prepared> = None;
    laps.lap("compile");
    progress(opts, 0.1, "updating the closure");
    let (up, threads) = exec.install(|| {
        let has = prior.is_some();
        let mut hook = |c: &mut incremental::Closure| -> anyhow::Result<()> {
            if let Some(w) = prior.as_mut() {
                prepared = Some(w.prepare(c, &compiled, &wl, &ch.base_removed, &ch.base_moved)?);
            }
            Ok(())
        };
        let hook: Option<incremental::BeforeDelete<'_>> = if has { Some(&mut hook) } else { None };
        let up = incremental::update_with(
            &mut closure,
            &compiled,
            &limits,
            &ch.base_added,
            &ch.base_removed,
            hook,
            rebuild,
        );
        (up, rayon::current_num_threads())
    });
    let up = up?;
    laps.add("delete", up.delete_micros);
    laps.add("insert", up.insert_micros);
    laps.lap("update");
    let mut base_scans = base.scans();
    drop(base);
    let mut generalized = prev.generalized;
    let mut gen_changes = maintain::GeneralizedChanges::default();
    for t in &up.deleted {
        if closure.graph.position(t).is_none() && generalized.remove(t) {
            gen_changes.removed.push(*t);
        }
    }
    let _added_held = Held::try_new(
        &budget,
        (closure.graph.len() - up.added_from) as usize * size_of::<[u64; 3]>(),
    )?;
    let added: Vec<[u64; 3]> = up.added(&closure).collect();
    for t in &added {
        if !closure.explicit_held(t)
            && !closure.terms.valid(t)
            && !ir::lower::is_key(t[1])
            && generalized.insert(*t)
        {
            gen_changes.added.push(*t);
        }
    }
    let witnesses = match wanted {
        None => None,
        Some(max) => {
            progress(opts, 0.75, "updating witnesses");
            let updated = match (prior, prepared) {
                (Some(mut w), Some(p)) => {
                    let ok = exec.install(|| {
                        w.update(
                            &mut closure,
                            &compiled,
                            &wl,
                            p,
                            &ch.base_added,
                            &ch.base_removed,
                            &up.deleted,
                            &added,
                            &touched,
                        )
                    })?;
                    ok.then_some(w)
                }
                _ => None,
            };
            match updated {
                Some(w) => Some(w),
                None => Some(
                    exec.install(|| witness::Witnesses::build(&mut closure, &compiled, max, &wl))?,
                ),
            }
        }
    };
    // the witness pass reads the input graphs too
    if witnesses.is_some()
        && let Some(b) = closure.graph.base()
    {
        base_scans = base_scans.max(b.scans());
    }
    laps.lap("witnesses");
    progress(opts, 0.8, "writing inferred graph");
    let candidates = up
        .deleted
        .iter()
        .chain(&added)
        .chain(&ch.base_added)
        .chain(&ch.base_removed)
        .chain(&ch.inferred_added)
        .chain(&ch.inferred_removed)
        .chain(&touched);
    let written = write_changes(
        exec,
        txn,
        snap,
        &closure,
        candidates,
        &budget,
        opts.inferred_change,
    )?;
    laps.lap("write");
    let changes = Changes {
        base_added: ch.base_added.len() as u64,
        base_removed: ch.base_removed.len() as u64,
        checked: up.checked,
        removed: up.deleted.len() as u64,
        derived: added.len() as u64,
        source: prev.source.to_string(),
    };
    tracing::debug!(
        ?changes,
        visited = up.visited,
        held_beside = up.held_beside,
        "incremental update"
    );
    if !generalized.is_empty() {
        warnings.push_fmt(format_args!(
            "{} derived generalized triples (literal subjects or non-IRI predicates) were not written",
            generalized.len()
        ))?;
    }
    Ok(Done {
        rule_groups: Vec::new(),
        budget,
        base_scans,
        threads,
        closure: Some(ClosureState::Ready(closure)),
        witnesses,
        generalized_saved: prev.saved.map(|m| (m, gen_changes)),
        generalized,
        written,
        rules: compiled.len(),
        iterations: up.iterations,
        warnings,
        method: Method::Incremental,
        fallback: None,
        changes: Some(changes),
    })
}

/// Candidates of [`write_changes`] from which their probes of the inferred graph run in
/// parallel.
const PAR_PROBES: usize = 256;

/// Bring the inferred graph in line with the closure for the triples that may have
/// changed: a triple belongs there when it is live, derived (not explicit) and valid RDF.
///
/// Whether the inferred graph holds each candidate is read from the snapshot on the
/// run's threads while the transaction has written nothing, since the transaction then
/// reads the snapshot, and one candidate at a time otherwise.
fn write_changes<'a>(
    exec: &pool::Exec,
    txn: &mut sparkles_core::store::WriteTxn<'_>,
    snap: &Snapshot,
    c: &incremental::Closure,
    candidates: impl Iterator<Item = &'a [u64; 3]>,
    budget: &Arc<Budget>,
    report_change: bool,
) -> anyhow::Result<Written> {
    let mut w = Written::default();
    let defaults = c.defaults.as_ref();
    let graph = snap.lookup_iri(INFERRED_GRAPH);
    let fact = size_of::<[u64; 3]>();
    let context = || " (writing the inferred graph)".to_string();
    let mut held = Held::new(budget);
    let mut seen: FxHashSet<[u64; 3]> = FxHashSet::default();
    // the distinct candidates, with whether the inferred graph should hold them
    let mut cands: Vec<([u64; 3], bool)> = Vec::new();
    for t in candidates {
        if !seen.contains(t) {
            room_for_key(&mut held, seen.capacity(), seen.len(), fact)
                .map_err(|e| e.with_context(context()))?;
        }
        if !seen.insert(*t) {
            continue;
        }
        // the default geometries are flagged explicit in the closure but belong to the
        // inferred graph
        let want = c
            .graph
            .position(t)
            .is_some_and(|r| !c.flagged(r) || defaults.is_some_and(|d| d.contains(t)))
            && c.terms.valid(t);
        if cands.len() == cands.capacity() {
            let old = cands.capacity();
            let new = (old * 2).max(4);
            held.try_grow(new * size_of::<([u64; 3], bool)>())
                .map_err(|e| e.with_context(context()))?;
            cands.reserve_exact(new - cands.len());
            held.shrink(old * size_of::<([u64; 3], bool)>());
        }
        cands.push((*t, want));
    }
    drop(seen);
    let quad = |t: &[u64; 3], g: Id| [Id(t[0]), Id(t[1]), Id(t[2]), g];
    let stored = |t: &[u64; 3]| t.iter().all(|&x| Id(x).tag() != Tag::Local);
    held.try_grow(cands.len())
        .map_err(|e| e.with_context(context()))?;
    let have: Vec<bool> = match graph {
        None => vec![false; cands.len()],
        Some(g) if !txn.is_dirty() && cands.len() >= PAR_PROBES => {
            use rayon::prelude::*;
            let parts: Vec<anyhow::Result<Vec<bool>>> = exec.install(|| {
                let chunk = cands
                    .len()
                    .div_ceil(rayon::current_num_threads() * 4)
                    .max(PAR_PROBES / 4);
                cands
                    .par_chunks(chunk)
                    .map(|ch| {
                        ch.iter()
                            .map(|(t, _)| Ok(stored(t) && snap.contains(&quad(t, g))?))
                            .collect()
                    })
                    .collect()
            });
            let mut have = Vec::with_capacity(cands.len());
            for p in parts {
                have.extend(p?);
            }
            have
        }
        Some(g) => {
            let mut have = Vec::with_capacity(cands.len());
            for (t, _) in &cands {
                have.push(stored(t) && txn.contains(&quad(t, g))?);
            }
            have
        }
    };
    let mut insert: Vec<[u64; 3]> = Vec::new();
    // the change, charged before each growth of its buffer
    let mut change: Option<(InferredChange, Held)> =
        report_change.then(|| (Vec::new(), Held::new(budget)));
    let mut note = |t: [Id; 3], weight: i64| -> anyhow::Result<()> {
        if let Some((v, h)) = change.as_mut() {
            if v.len() == v.capacity() {
                let elem = size_of::<([Id; 3], i64)>();
                let old = v.capacity();
                let new = (old * 2).max(4);
                h.try_grow(new * elem)
                    .map_err(|e| e.with_context(context()))?;
                v.reserve_exact(new - v.len());
                h.shrink(old * elem);
            }
            v.push((t, weight));
        }
        Ok(())
    };
    for ((t, want), have) in cands.iter().zip(have) {
        if *want && !have {
            if insert.len() == insert.capacity() {
                // the new buffer is charged before it is allocated, and the old one is
                // released once it holds the facts
                let old = insert.capacity();
                let new = (old * 2).max(4);
                held.try_grow(new * fact)
                    .map_err(|e| e.with_context(context()))?;
                insert.reserve_exact(new - insert.len());
                held.shrink(old * fact);
            }
            insert.push(*t);
        } else if !want && have {
            let g = graph.expect("present");
            txn.delete(quad(t, g))?;
            w.removed += 1;
            note([Id(t[0]), Id(t[1]), Id(t[2])], -1)?;
        }
    }
    drop(cands);
    let before = match graph {
        Some(g) => snap.count(Perm::Gspo, &[g.0])?,
        None => 0,
    };
    if !insert.is_empty() {
        let g = match graph {
            Some(g) => g,
            None => txn.intern(&Term::NamedNode(NamedNode::new_unchecked(INFERRED_GRAPH)))?,
        };
        held.try_grow(insert.len() * size_of::<[Id; 4]>())
            .map_err(|e| e.with_context(context()))?;
        let mut quads = Vec::with_capacity(insert.len());
        for t in &insert {
            let ids = store_ids(txn, &c.terms, &mut w.stored, t)?;
            note(ids, 1)?;
            quads.push([ids[0], ids[1], ids[2], g]);
        }
        w.added = quads.len() as u64;
        txn.insert_bulk(quads)?;
    }
    w.inferred = before + w.added - w.removed;
    w.change = change;
    Ok(w)
}

/// Clear [`INFERRED_GRAPH`], run the rules over the input graphs, and write every derived
/// triple that is not already in one of them into [`INFERRED_GRAPH`] in one write
/// transaction.
///
/// The store's writer lock is held for the whole run, so the entailments are exactly
/// those of the committed default graph at the start.
pub fn materialize(
    store: &Store,
    profile: &Profile,
    opts: &ReasonOptions,
) -> anyhow::Result<ReasonReport> {
    materialize_with(store, profile, &Extras::default(), opts)
}

/// An error for a profile that only query-time reasoning runs: `owl-ql` answers queries
/// by rewriting them, and its materialization would be that of `owl-rl` under another
/// name, which `reasoning=auto` could not tell apart.
pub fn refuse_query_only(profile: &Profile) -> anyhow::Result<()> {
    if let Profile::OwlQl = profile {
        anyhow::bail!(
            "the owl-ql profile answers queries by rewriting them and is not materialized; \
             set it as the query-time profile, or materialize with owl-rl"
        );
    }
    Ok(())
}

/// Remove [`INFERRED_GRAPH`]. Returns the number of triples removed.
pub fn clear(store: &Store) -> anyhow::Result<u64> {
    maintain::remove_saved(store);
    let mut txn = store.write_as(sparkles_core::commit::CommitKind::ReasonClear);
    let snap = txn.base().clone();
    let Some(g) = snap.lookup_iri(INFERRED_GRAPH) else {
        return Ok(0);
    };
    let keys = snap.scan_keys(Perm::Gspo, &[g.0])?;
    let mut n = 0;
    for k in &keys {
        if txn.delete(Perm::Gspo.to_quad(k))? {
            n += 1;
        }
    }
    txn.commit()?;
    Ok(n)
}

/// How many facts of the closure of `snap`'s default graph DRed would delete first if
/// the triples `removed` were removed from it: the facts with a derivation that uses one
/// of them, transitively. Backward/forward deletes only those without another proof. For
/// measurements.
#[doc(hidden)]
pub fn dred_overdeletion(
    snap: Arc<Snapshot>,
    profile: &Profile,
    removed: &[Triple],
) -> anyhow::Result<usize> {
    let opts = ReasonOptions::default();
    let d = admitted(&opts, |exec| {
        derive(
            exec,
            snap,
            profile,
            &Extras::default(),
            &[Id::DEFAULT_GRAPH],
            &opts,
        )
    })?;
    let mut warnings = WorkingWarnings::new(&d.budget);
    let parsed = profile_rules(profile, &Extras::default())?;
    if parser::uses_program(&parsed) {
        anyhow::bail!("the over-deletion measurement takes Jena rules, not a rule program");
    }
    let rules = engine::compile(&parsed, &d.terms, &mut warnings)?;
    let removed: Vec<[u64; 3]> = removed
        .iter()
        .map(|t| {
            Ok([
                d.terms.id_for(&t.subject.clone().into())?,
                d.terms.id_for(&t.predicate.clone().into())?,
                d.terms.id_for(&t.object)?,
            ])
        })
        .collect::<Result<Vec<_>, budget::BudgetFailure>>()?;
    let limits = engine::Limits {
        max_iterations: usize::MAX,
        max_inferred: usize::MAX,
        cancel: None,
        deadline: None,
        progress: None,
        budget: d.budget.clone(),
        closures: false,
        magic: None,
        sampled_planning: false,
    };
    let mut c = d.into_closure()?;
    incremental::dred_overdeletion(&mut c, &rules, &limits, &removed)
}

/// Run the rules over a snapshot's input graphs ([`ReasonOptions::inputs`]) and return
/// the derived triples that are valid RDF, without writing anything (dry run / testing).
/// Blank nodes created by `makeTemp` / `makeSkolem` get labels `r<hex>`.
pub fn infer(
    snap: Arc<Snapshot>,
    profile: &Profile,
    opts: &ReasonOptions,
) -> anyhow::Result<(Vec<Triple>, ReasonReport)> {
    let t0 = Instant::now();
    refuse_query_only(profile)?;
    if let Profile::ShaclAf(_) = profile {
        anyhow::bail!(
            "the shacl-af profile runs only in a materialization, whose write transaction \
             holds the terms its rules compute"
        );
    }
    opts.inputs.validate()?;
    let resolved = inputs::resolve(&snap, &opts.inputs);
    let ids = resolved.ids(&snap);
    let d = admitted(opts, |exec| {
        derive(exec, snap, profile, &Extras::default(), &ids, opts)
    })?;
    let mut out = Vec::new();
    for t in d.derived() {
        if !d.valid(&t) {
            continue;
        }
        let (Some(s), Some(Term::NamedNode(p)), Some(o)) =
            (d.terms.term(t[0]), d.terms.term(t[1]), d.terms.term(t[2]))
        else {
            continue;
        };
        let s = match s {
            Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
            Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
            Term::Literal(_) | Term::Triple(_) => continue,
        };
        out.push(Triple::new(s, p, o));
    }
    let report = ReasonReport {
        profile: profile.name().to_string(),
        rules: d.rules,
        iterations: d.iterations,
        inferred: out.len() as u64,
        millis: t0.elapsed().as_millis() as u64,
        warnings: d.warnings.into_legacy_report(),
        receipt: None,
        inputs: Some(resolved),
        peak_bytes: d.budget.peak() as u64,
        base_scans: d.base_scans,
        threads: d.threads,
        ..Default::default()
    };
    Ok((out, report))
}

/// The rule plans of this process that took an order from sampled bindings
/// ([`ReasonOptions::sampled_planning`]) so far. For tests.
#[doc(hidden)]
pub fn bench_sampled_plans() -> u64 {
    engine::sampled::changed_plans()
}

/// Sample and search every rule body of three atoms or more in the runs of this process
/// ([`ReasonOptions::sampled_planning`]), whatever its plan costs. Tests on small inputs
/// set it, since small plans keep the greedy planner's order without a sample.
#[doc(hidden)]
pub fn bench_sampled_search_all(on: bool) {
    engine::sampled::search_all(on);
}

/// Turn the plan probe on or off for the runs of this process: a diagnostic that times
/// every atom order of each rule plan and prints one line per plan to the standard
/// error. It exists only with the `plan-probe` feature, and it perturbs the runs it
/// measures: they build both orders over the whole tier and take far longer. For
/// measurements.
#[cfg(feature = "plan-probe")]
#[doc(hidden)]
pub fn bench_plan_probe(on: bool) {
    engine::planprobe::set(on);
}

/// Run the rules over a snapshot's input graphs and return the number of derived facts
/// (generalized ones included), the iterations, a digest of the derived facts that does
/// not depend on their order or ids, and one that depends on their order but not their
/// ids. `derived` is called when the rules are done, before the digests are worked out.
/// For benchmarks.
#[doc(hidden)]
pub fn bench_derive(
    snap: Arc<Snapshot>,
    profile: &Profile,
    opts: &ReasonOptions,
    derived: &dyn Fn(),
) -> anyhow::Result<(usize, usize, u64, u64)> {
    opts.inputs.validate()?;
    let resolved = inputs::resolve(&snap, &opts.inputs);
    let ids = resolved.ids(&snap);
    let d = admitted(opts, |exec| {
        derive(exec, snap, profile, &Extras::default(), &ids, opts)
    })?;
    derived();
    let (mut set, mut seq) = (0u64, maintain::FNV_START);
    for t in d.derived() {
        let mut h = maintain::FNV_START;
        for x in &t {
            let s = d.terms.term(*x).map_or(String::new(), |t| t.to_string());
            h = maintain::fnv(s.as_bytes(), h);
            h = maintain::fnv(b"\0", h);
        }
        set = set.wrapping_add(h.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        seq = maintain::fnv(&h.to_le_bytes(), seq);
    }
    Ok((d.closure_len(), d.iterations, set, seq))
}

#[cfg(test)]
mod derivation_tests {
    use super::*;

    /// Relation rows are not derived triples, so the write path neither writes them nor
    /// counts them as generalized triples.
    #[test]
    fn relation_rows_are_no_derived_triples() {
        let store = Store::in_memory(sparkles_core::store::StoreOptions::default());
        let terms = Terms::new(store.snapshot());
        let iri = |l: &str| {
            let n = oxrdf::NamedNode::new_unchecked(format!("http://ex.org/{l}"));
            terms.id_for(&n.into()).unwrap()
        };
        let mut graph = graph::Graph::default();
        let triple = [iri("a"), iri("p"), iri("b")];
        graph.add(triple);
        graph.add([iri("a"), ir::lower::key(ir::RelId(0)), iri("b")]);
        let budget = Budget::unlimited();
        let d = Derivation {
            graph,
            terms,
            rules: 0,
            iterations: 0,
            warnings: WorkingWarnings::new(&budget),
            budget,
            defaults: None,
            base_scans: 0,
            threads: 1,
        };
        assert_eq!(d.derived().collect::<Vec<_>>(), vec![triple]);
        assert_eq!(d.derived().filter(|t| !d.valid(t)).count(), 0);
    }
}

#[cfg(test)]
mod kept_rule_sets_tests {
    use super::*;

    fn set(digest: u64, bytes: usize) -> KeptRules {
        KeptRules {
            digest,
            bytes,
            rules: Arc::new(Vec::new()),
            blocker: None,
        }
    }

    fn digests(k: &KeptRuleSets) -> Vec<u64> {
        k.sets.iter().map(|s| s.digest).collect()
    }

    #[test]
    fn kept_rule_sets_are_bounded_by_number_and_bytes() {
        let mut k = KeptRuleSets::new();
        // a set over the bound by itself is not kept and evicts nothing
        k.keep(set(1, 40), 3, 100);
        k.keep(set(2, 101), 3, 100);
        assert_eq!(digests(&k), [1]);
        assert!(k.get(2).is_none());
        // the oldest sets leave until the new one fits
        k.keep(set(3, 40), 3, 100);
        k.keep(set(4, 70), 3, 100);
        assert_eq!(digests(&k), [4]);
        assert_eq!(k.bytes, 70);
        // a set that is kept already stays as it is
        k.keep(set(4, 10), 3, 100);
        assert_eq!(k.bytes, 70);
        // the number of sets
        for d in 5..9 {
            k.keep(set(d, 1), 3, 100);
        }
        assert_eq!(digests(&k), [6, 7, 8]);
        assert_eq!(k.bytes, 3);
        assert!(k.get(7).is_some() && k.get(4).is_none());
    }

    #[test]
    fn rules_bytes_count_what_includes_and_prefixes_expand() {
        let own = parse_rules("[r: (?a <urn:p> ?b) -> (?b <urn:q> ?a)]").unwrap();
        let included = parse_rules("@include <owl-rl> .").unwrap();
        let prefixed = parse_rules(&format!(
            "@prefix ex: <urn:{}> .\n[r: (?a ex:p ?b) -> (?b ex:q ?a)]",
            "x".repeat(1000)
        ))
        .unwrap();
        let small = rules_bytes(&own);
        assert!(small >= size_of::<Rule>() + 6 * size_of::<Node>());
        assert!(rules_bytes(&included) > OWL_RL_RULES.len());
        assert!(rules_bytes(&prefixed) >= small + 2000);
    }
}

#[cfg(test)]
mod geometry_analysis_control_tests {
    use super::*;
    use crate::analysis::{AnalysisContext, WitnessMap};
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::store::StoreOptions;

    #[test]
    fn analysis_cancel_is_returned_by_geometry_before_rule_compilation() {
        let store = Store::in_memory(StoreOptions::default());
        store.load(&[Source::from_bytes(b"<urn:a> <http://www.opengis.net/ont/geosparql#hasGeometry> <urn:g> . <urn:b> <http://www.opengis.net/ont/geosparql#hasGeometry> <urn:h> .".to_vec(),RdfFormat::Turtle,None)]).unwrap();
        let snapshot = store.snapshot();
        let terms = Terms::new(snapshot.clone());
        let base = facts::Base::new(snapshot, &[Id::DEFAULT_GRAPH]).unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let context = AnalysisContext::new(
            WitnessMap::new(1, None, stopped.clone(), None),
            None,
            "controlled geometry unit".into(),
        )
        .unwrap();
        let flag = stopped.clone();
        let options = ReasonOptions {
            analysis: Some(context),
            cancel: None,
            progress: Some(Arc::new(move |_, message| {
                if message == "finding default geometries" {
                    flag.store(true, Ordering::Relaxed);
                }
            })),
            ..Default::default()
        };
        let error = default_geometries_for_run(&base, &terms, &Budget::unlimited(), &options)
            .expect_err(
                "the wrapper must stop while constructing defaults, before any engine check",
            );
        assert!(stopped.load(Ordering::Relaxed));
        assert!(
            error.downcast_ref::<MemoryLimitExceeded>().is_some(),
            "{error:#}"
        );
    }
}

#[cfg(test)]
mod finish_warning_tests;
