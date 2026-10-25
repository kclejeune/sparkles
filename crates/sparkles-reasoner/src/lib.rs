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

mod budget;
mod builtins;
mod demand;
pub mod diagnostics;
mod engine;
pub mod explain;
pub mod extras;
mod facts;
mod graph;
mod incremental;
pub mod inputs;
mod lookups;
mod maintain;
pub mod parser;
mod pool;
pub mod query;
mod specialize;
pub mod tables;
mod terms;
pub mod whynot;

pub use extras::{Extras, UnknownVocabulary, Vocabulary};
pub use inputs::{
    Fetched, GraphRef, Import, ImportMode, Inputs, LocationMapping, Resolved, fetch_imports,
};
pub use maintain::{Cache, DEFAULT_CACHE_TRIPLES, literal_typings};
pub use parser::{
    BuiltinCall, Clause, Direction, Node, Rule, RuleParseError, TriplePattern, parse_rules,
};

use anyhow::Context as _;
use budget::{Budget, Held};
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
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use terms::{LocalTerm, Terms};

/// Named graph holding materialized entailments.
pub const INFERRED_GRAPH: &str = "urn:x-sparkles:inferred";

/// Full RDFS entailment (Jena `etc/rdfs.rules`, W3C RDF 1.1 Semantics rules).
pub const RDFS_RULES: &str = include_str!("../rules/rdfs.rules");
/// RDFS subset: subClassOf / subPropertyOf closure and inheritance, domain, range.
pub const RDFS_SIMPLE_RULES: &str = include_str!("../rules/rdfs-simple.rules");
/// OWL 2 RL subset (comparable to Jena OWL Mini/Micro).
pub const OWL_RL_RULES: &str = include_str!("../rules/owl-rl.rules");

/// Which rules to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Profile {
    Rdfs,
    RdfsSimple,
    OwlRl,
    /// Jena rule text
    Rules(String),
}

impl Profile {
    pub fn name(&self) -> &str {
        match self {
            Profile::Rdfs => "rdfs",
            Profile::RdfsSimple => "rdfs-simple",
            Profile::OwlRl => "owl-rl",
            Profile::Rules(_) => "rules",
        }
    }

    /// The rule text of this profile.
    pub fn text(&self) -> &str {
        match self {
            Profile::Rdfs => RDFS_RULES,
            Profile::RdfsSimple => RDFS_SIMPLE_RULES,
            Profile::OwlRl => OWL_RL_RULES,
            Profile::Rules(t) => t,
        }
    }

    pub fn rules(&self) -> Result<Vec<Rule>, RuleParseError> {
        parse_rules(self.text())
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown reasoning profile '{0}' (expected rdfs, rdfs-simple, owl-rl or owl)")]
pub struct UnknownProfile(pub String);

impl FromStr for Profile {
    type Err = UnknownProfile;
    fn from_str(s: &str) -> Result<Profile, UnknownProfile> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rdfs" | "rdfs-full" => Ok(Profile::Rdfs),
            "rdfs-simple" | "rdfssimple" | "simple" => Ok(Profile::RdfsSimple),
            "owl-rl" | "owlrl" | "owl" | "owl-mini" | "owlmini" | "owl-micro" | "owlmicro" => {
                Ok(Profile::OwlRl)
            }
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
    /// Threads for rule evaluation: at most the size of [`pool`](Self::pool) when there
    /// is one. `None` uses the pool, or the caller's threads.
    pub threads: Option<usize>,
    /// The reasoning pool runs evaluate on and are admitted to (the server's), see
    /// [`ReasoningPool`].
    pub pool: Option<Arc<ReasoningPool>>,
}

impl Default for ReasonOptions {
    fn default() -> Self {
        ReasonOptions {
            max_iterations: 10_000,
            max_inferred: DEFAULT_MAX_INFERRED,
            cancel: None,
            progress: None,
            inputs: Inputs::default(),
            max_memory: None,
            threads: None,
            pool: None,
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
            .field("threads", &self.threads)
            .field("pool", &self.pool.as_ref().map(|p| p.threads()))
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
    warnings: Vec<String>,
    /// the run's memory: the graph and the local terms stay charged to it
    budget: Arc<Budget>,
    /// the GeoSPARQL default geometries the run added as facts, sorted, and their charge
    defaults: Option<(Vec<[u64; 3]>, Held)>,
    base_scans: u64,
    threads: usize,
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
    let budget = Budget::new(opts.max_memory);
    let base = Arc::new(facts::Base::new(snap.clone(), graphs)?);
    let mut g = graph::Graph::with_base(base.clone());
    g.set_budget(&budget)?;
    let terms = Terms::new(snap);
    terms.set_budget(&budget)?;
    let mut defaults = None;
    if extras.geo_default_geometry {
        let d = default_geometries(&base, &terms, &budget)?;
        g.try_add_batch(d.0.iter().copied())?;
        defaults = Some(d);
    }
    let mut warnings = Vec::new();
    let compiled = engine::compile(&rules, &terms, &mut warnings);
    let limits = engine::Limits {
        max_iterations: opts.max_iterations,
        max_inferred: opts.max_inferred,
        cancel: opts.cancel.clone(),
        progress: opts.progress.clone(),
        budget: budget.clone(),
    };
    progress(opts, 0.1, "reasoning");
    let out = engine::run(&mut g, &compiled, &terms, &limits, None)?;
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
fn geo_ids(terms: &Terms) -> (u64, u64) {
    let iri = |l: &str| {
        terms.id_for(&Term::NamedNode(NamedNode::new_unchecked(format!(
            "{GEO}{l}"
        ))))
    };
    (iri("hasGeometry"), iri("hasDefaultGeometry"))
}

/// Room in `held` for one more key of a table of capacity `cap` with `len` keys of
/// `elem` bytes (with their values): the table it grows to is charged while the old one
/// is still charged.
fn room_for_key(
    held: &mut Held,
    cap: usize,
    len: usize,
    elem: usize,
) -> Result<(), budget::MemoryExceeded> {
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
    let (has, default) = geo_ids(terms);
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
    let (has, default) = geo_ids(&closure.terms);
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
    closure.settle();
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
                    closure.graph.kill(&d);
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
    closure.settle();
    Ok(touched)
}

impl Derivation {
    fn derived(&self) -> &[[u64; 3]] {
        self.graph.derived()
    }

    /// Is this (possibly generalized) triple valid RDF?
    fn valid(&self, t: &[u64; 3]) -> bool {
        self.terms.valid(t)
    }

    /// The derived facts.
    fn closure_len(&self) -> usize {
        self.graph.derived().len()
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
            let (_, pred) = geo_ids(&c.terms);
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

/// A digest of everything that determines a materialization besides the data: the rule
/// text of the profile and its vocabularies, and the switches of `extras`. An incremental
/// run needs the previous run's digest to be the same.
pub fn rules_digest(profile: &Profile, extras: &Extras) -> u64 {
    use maintain::{FNV_START, fnv};
    let mut h = fnv(b"sparkles-reasoner closure 1\0", FNV_START);
    h = fnv(profile.name().as_bytes(), h);
    h = fnv(b"\0", h);
    h = fnv(profile.text().as_bytes(), h);
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
    let (mut txn, raw, resolved) = lock_with_changes(store, inc, &opts.inputs);
    let snap = txn.base().clone();
    let digest = rules_digest(profile, extras);
    let mut fallback = None;
    if let (Some(since), Some(raw)) = (inc.since, raw) {
        match update_closure(
            exec, store, &mut txn, &snap, profile, extras, since, digest, &resolved, inc, raw, opts,
        ) {
            Ok(done) => {
                return finish(
                    store, txn, &snap, profile, digest, &resolved, inc, opts, t0, done,
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
    progress(opts, 0.8, "writing inferred graph");
    let mut warnings = d.warnings.clone();
    let w = write_full(&mut txn, &snap, &d, opts)?;
    if w.generalized > 0 {
        warnings.push(format!(
            "{} derived generalized triples (literal subjects or non-IRI predicates) were not written",
            w.generalized
        ));
    }
    let generalized: FxHashSet<[u64; 3]> = d
        .derived()
        .iter()
        .filter(|t| !d.valid(t))
        .copied()
        .collect();
    let done = Done {
        rules: d.rules,
        iterations: d.iterations,
        budget: d.budget.clone(),
        base_scans: d.base_scans,
        threads: d.threads,
        closure: Some(ClosureState::Derived(d)),
        generalized,
        generalized_saved: None,
        written: w,
        warnings,
        method: Method::Full,
        fallback,
        changes: None,
    };
    finish(
        store, txn, &snap, profile, digest, &resolved, inc, opts, t0, done,
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
        return (txn, None, r);
    };
    for _ in 0..3 {
        let live = store.snapshot();
        let graphs = inputs::resolve(&live, inputs).graph_set();
        let raw = maintain::changes(store, since, &live, inc.cache, &graphs);
        let (txn, r) = locked(store.write_as(sparkles_core::commit::CommitKind::Reason));
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

/// A run's result before its commit.
struct Done {
    /// the closure, for the next incremental run
    closure: Option<ClosureState>,
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
    warnings: Vec<String>,
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
}

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
) -> anyhow::Result<ReasonReport> {
    if opts
        .cancel
        .as_ref()
        .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
    {
        anyhow::bail!("reasoning cancelled");
    }
    // a run that went over its limit fails before its commit, also when its charges are
    // back under it now
    done.budget.check_peak()?;
    let receipt = txn.commit()?;
    progress(opts, 1.0, "done");
    let mut warnings = resolved.warnings.clone();
    warnings.extend(done.warnings);
    let graphs = resolved.graph_set();
    let mut kept = false;
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
    let report = ReasonReport {
        profile: profile.name().to_string(),
        rules: done.rules,
        iterations: done.iterations,
        inferred: done.written.inferred,
        millis: t0.elapsed().as_millis() as u64,
        warnings,
        receipt: Some(receipt),
        method: done.method,
        fallback: done.fallback,
        changes: done.changes,
        inferred_added: done.written.added,
        inferred_removed: done.written.removed,
        inputs: Some(resolved.clone()),
        peak_bytes: done.budget.peak() as u64,
        base_scans: done.base_scans,
        threads: done.threads,
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
                        Some(LocalTerm::BNode) => txn.new_bnode(),
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
    let n = d.derived().len();
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
        if !d.valid(t) {
            w.generalized += 1;
            continue;
        }
        new_triples.push(store_ids(txn, &d.terms, &mut w.stored, t)?);
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
) -> anyhow::Result<Done> {
    if since > snap.commit {
        return Err(Fallback("the store position moved backwards".into()).into());
    }
    let rules = profile_rules(profile, extras)?;
    {
        let terms = Terms::new(snap.clone());
        let mut w = Vec::new();
        for r in engine::compile(&rules, &terms, &mut w) {
            if let Some(why) = engine::incremental_blocker(&r, &terms) {
                return Err(Fallback(why).into());
            }
        }
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
    progress(opts, 0.0, "reading the previous closure");
    let budget = Budget::new(opts.max_memory);
    // on the run's threads: this thread may stand in for a parked worker of the pool
    // that called, so its parallel work must not go to that pool
    let run_ids = resolved.ids(snap);
    let prev = exec.install(|| {
        maintain::previous(
            store, snap, since, digest, &graphs, &run_ids, inc.cache, raw, &budget,
        )
    })?;
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
    let mut warnings = Vec::new();
    let compiled = engine::compile(&rules, &closure.terms, &mut warnings);
    let limits = engine::Limits {
        max_iterations: opts.max_iterations,
        max_inferred: opts.max_inferred,
        cancel: opts.cancel.clone(),
        progress: opts.progress.clone(),
        budget: budget.clone(),
    };
    let ch = &ch;
    progress(opts, 0.1, "updating the closure");
    let (up, threads) = exec.install(|| {
        let up = incremental::update(
            &mut closure,
            &compiled,
            &limits,
            &ch.base_added,
            &ch.base_removed,
        );
        (up, rayon::current_num_threads())
    });
    let up = up?;
    let base_scans = base.scans();
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
        if !closure.explicit_held(t) && !closure.terms.valid(t) && generalized.insert(*t) {
            gen_changes.added.push(*t);
        }
    }
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
    let written = write_changes(exec, txn, snap, &closure, candidates, &budget)?;
    let changes = Changes {
        base_added: ch.base_added.len() as u64,
        base_removed: ch.base_removed.len() as u64,
        checked: up.checked,
        removed: up.deleted.len() as u64,
        derived: added.len() as u64,
        source: prev.source.to_string(),
    };
    tracing::debug!(?changes, "incremental update");
    if !generalized.is_empty() {
        warnings.push(format!(
            "{} derived generalized triples (literal subjects or non-IRI predicates) were not written",
            generalized.len()
        ));
    }
    Ok(Done {
        budget,
        base_scans,
        threads,
        closure: Some(ClosureState::Ready(closure)),
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
            quads.push([ids[0], ids[1], ids[2], g]);
        }
        w.added = quads.len() as u64;
        txn.insert_bulk(quads)?;
    }
    w.inferred = before + w.added - w.removed;
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
    let mut warnings = Vec::new();
    let rules = engine::compile(
        &profile_rules(profile, &Extras::default())?,
        &d.terms,
        &mut warnings,
    );
    let removed: Vec<[u64; 3]> = removed
        .iter()
        .map(|t| {
            [
                d.terms.id_for(&t.subject.clone().into()),
                d.terms.id_for(&t.predicate.clone().into()),
                d.terms.id_for(&t.object),
            ]
        })
        .collect();
    let limits = engine::Limits {
        max_iterations: usize::MAX,
        max_inferred: usize::MAX,
        cancel: None,
        progress: None,
        budget: Budget::unlimited(),
    };
    let mut c = d.into_closure()?;
    Ok(incremental::dred_overdeletion(
        &mut c, &rules, &limits, &removed,
    ))
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
    opts.inputs.validate()?;
    let resolved = inputs::resolve(&snap, &opts.inputs);
    let ids = resolved.ids(&snap);
    let d = admitted(opts, |exec| {
        derive(exec, snap, profile, &Extras::default(), &ids, opts)
    })?;
    let mut out = Vec::new();
    for t in d.derived() {
        if !d.valid(t) {
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
        warnings: d.warnings,
        receipt: None,
        inputs: Some(resolved),
        peak_bytes: d.budget.peak() as u64,
        base_scans: d.base_scans,
        threads: d.threads,
        ..Default::default()
    };
    Ok((out, report))
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
    let facts = d.derived();
    let (mut set, mut seq) = (0u64, maintain::FNV_START);
    for t in facts {
        let mut h = maintain::FNV_START;
        for x in t {
            let s = d.terms.term(*x).map_or(String::new(), |t| t.to_string());
            h = maintain::fnv(s.as_bytes(), h);
            h = maintain::fnv(b"\0", h);
        }
        set = set.wrapping_add(h.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        seq = maintain::fnv(&h.to_le_bytes(), seq);
    }
    Ok((facts.len(), d.iterations, set, seq))
}
