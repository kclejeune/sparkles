//! Forward-chaining reasoning for Sparkles (Jena `GenericRuleReasoner` FORWARD mode /
//! `infer` equivalent).
//!
//! The reasoner loads the store's **default graph** into memory as id-level triples,
//! runs a rule set to a fixpoint with semi-naive evaluation, and writes every derived
//! triple that is not already asserted into the named graph [`INFERRED_GRAPH`]. Queries
//! see the entailments by adding that graph to their default graph
//! (`QueryOptions::default_graph_extra`).
//!
//! Rule sets: [`Profile::Rdfs`] (full RDFS entailment), [`Profile::RdfsSimple`]
//! (subClassOf / subPropertyOf / domain / range), [`Profile::OwlRl`] (an OWL 2 RL subset
//! comparable to Jena's OWL Mini) or any Jena rule text ([`Profile::Rules`]).
//!
//! [`materialize_incremental`] updates a previous materialization from the changes to
//! the default graph since its commit, with the same result as a full run.

mod builtins;
pub mod diagnostics;
mod engine;
pub mod extras;
mod graph;
mod incremental;
mod maintain;
pub mod parser;
mod terms;

pub use extras::{Extras, UnknownVocabulary, Vocabulary};
pub use maintain::{Cache, DEFAULT_CACHE_TRIPLES};
pub use parser::{
    BuiltinCall, Clause, Direction, Node, Rule, RuleParseError, TriplePattern, parse_rules,
};

use anyhow::Context as _;
use incremental::Fallback;
use oxrdf::{NamedNode, NamedOrBlankNode, Term, Triple};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles::id::{Id, Tag};
use sparkles::index::Perm;
use sparkles::store::{Chunk, Snapshot, Store};
use std::fmt;
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
pub type ProgressFn = Arc<dyn Fn(f32, &str) + Send + Sync>;

/// Reasoning limits, cancellation and progress reporting.
#[derive(Clone)]
pub struct ReasonOptions {
    /// Maximum number of semi-naive iterations before giving up.
    pub max_iterations: usize,
    /// Maximum number of derived triples before giving up.
    pub max_inferred: usize,
    /// Set to `true` to cancel.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Called with (fraction done in `[0, 1]`, message).
    pub progress: Option<ProgressFn>,
}

impl Default for ReasonOptions {
    fn default() -> Self {
        ReasonOptions {
            max_iterations: 10_000,
            max_inferred: 50_000_000,
            cancel: None,
            progress: None,
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
    pub receipt: Option<sparkles::commit::Receipt>,
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
    /// triples added to the default graph since the previous run
    pub base_added: u64,
    /// triples removed from the default graph since the previous run
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

/// In-memory reasoning result (before writing).
struct Derivation {
    graph: graph::Graph,
    terms: Terms,
    base_len: u32,
    rules: usize,
    iterations: usize,
    warnings: Vec<String>,
}

fn load_default_graph(snap: &Snapshot) -> anyhow::Result<graph::Graph> {
    let n = snap.count(Perm::Gspo, &[Id::DEFAULT_GRAPH.0]).unwrap_or(0) as usize;
    let mut triples = Vec::with_capacity(n);
    snap.scan(Perm::Gspo, &[Id::DEFAULT_GRAPH.0], |c| {
        match c {
            Chunk::Block(b, s, e) => {
                triples.extend((s..e).map(|i| [b.cols[1][i], b.cols[2][i], b.cols[3][i]]));
            }
            Chunk::Row(k) => triples.push([k[1], k[2], k[3]]),
        }
        Ok(true)
    })?;
    let mut g = graph::Graph::with_capacity(n + n / 2);
    g.add_batch(triples);
    Ok(g)
}

fn derive(
    snap: Arc<Snapshot>,
    profile: &Profile,
    extras: &Extras,
    opts: &ReasonOptions,
) -> anyhow::Result<Derivation> {
    let rules = profile_rules(profile, extras)?;
    progress(opts, 0.0, "loading default graph");
    let mut g = load_default_graph(&snap)?;
    // rows from here on are derived
    let base_len = g.len();
    let terms = Terms::new(snap);
    if extras.geo_default_geometry {
        let defaults = default_geometries(&g, &terms);
        g.add_batch(defaults);
    }
    let mut warnings = Vec::new();
    let compiled = engine::compile(&rules, &terms, &mut warnings);
    let limits = engine::Limits {
        max_iterations: opts.max_iterations,
        max_inferred: opts.max_inferred,
        cancel: opts.cancel.clone(),
        progress: opts.progress.clone(),
    };
    progress(opts, 0.1, "reasoning");
    let out = engine::run(&mut g, &compiled, &terms, &limits, None)?;
    Ok(Derivation {
        base_len,
        graph: g,
        terms,
        rules: compiled.len(),
        iterations: out.iterations,
        warnings,
    })
}

/// `F geo:hasDefaultGeometry G` for every `F` of the default graph with exactly one
/// `geo:hasGeometry` (`G`) and no `geo:hasDefaultGeometry` (Jena's
/// `applyDefaultGeometry`).
fn default_geometries(g: &graph::Graph, terms: &Terms) -> Vec<[u64; 3]> {
    const GEO: &str = "http://www.opengis.net/ont/geosparql#";
    let iri = |l: &str| {
        terms.id_for(&Term::NamedNode(NamedNode::new_unchecked(format!(
            "{GEO}{l}"
        ))))
    };
    let (has, default) = (iri("hasGeometry"), iri("hasDefaultGeometry"));
    // feature → its only geometry (`None` once it has a second)
    let mut one: FxHashMap<u64, Option<u64>> = FxHashMap::default();
    let mut with_default: FxHashSet<u64> = FxHashSet::default();
    for t in &g.triples {
        if t[1] == has {
            one.entry(t[0])
                .and_modify(|x| {
                    if *x != Some(t[2]) {
                        *x = None;
                    }
                })
                .or_insert(Some(t[2]));
        } else if t[1] == default {
            with_default.insert(t[0]);
        }
    }
    let mut out: Vec<[u64; 3]> = one
        .into_iter()
        .filter(|(f, _)| !with_default.contains(f))
        .filter_map(|(f, geom)| Some([f, default, geom?]))
        .collect();
    out.sort_unstable();
    out
}

impl Derivation {
    fn derived(&self) -> &[[u64; 3]] {
        &self.graph.triples[self.base_len as usize..]
    }

    /// Is this (possibly generalized) triple valid RDF?
    fn valid(&self, t: &[u64; 3]) -> bool {
        self.terms.valid(t)
    }
}

/// The rules of a profile with the axioms of the vocabularies of `extras`.
fn profile_rules(profile: &Profile, extras: &Extras) -> anyhow::Result<Vec<Rule>> {
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
pub fn materialize_incremental(
    store: &Store,
    profile: &Profile,
    extras: &Extras,
    inc: Incremental<'_>,
    opts: &ReasonOptions,
) -> anyhow::Result<ReasonReport> {
    extras.validate()?;
    let t0 = Instant::now();
    let (mut txn, raw) = lock_with_changes(store, inc);
    let snap = txn.base().clone();
    let digest = rules_digest(profile, extras);
    let mut fallback = None;
    if let (Some(since), Some(raw)) = (inc.since, raw) {
        match update_closure(
            store, &mut txn, &snap, profile, extras, since, digest, inc, raw, opts,
        ) {
            Ok(done) => {
                return finish(store, txn, &snap, profile, digest, inc, opts, t0, done);
            }
            Err(e) => match e.downcast::<Fallback>() {
                Ok(f) => {
                    tracing::info!(reason = %f.0, "materializing in full");
                    fallback = Some(f.0);
                }
                Err(e) => return Err(e),
            },
        }
    }
    let d = derive(snap.clone(), profile, extras, opts)?;
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
        closure: (!extras.geo_default_geometry)
            .then(|| incremental::Closure::new(d.graph, d.terms, d.base_len)),
        generalized,
        generalized_saved: None,
        written: w,
        rules: d.rules,
        iterations: d.iterations,
        warnings,
        method: Method::Full,
        fallback,
        changes: None,
    };
    finish(store, txn, &snap, profile, digest, inc, opts, t0, done)
}

/// Take the writer lock, with the changes since `inc.since` up to the state it locks.
///
/// The commit diff takes the writer lock itself, so the changes are read before. A commit
/// that lands in between makes them stale. A closure kept in memory then gives the
/// changes from its deltas, under the lock; otherwise the lock is released and they are
/// read again, up to three times.
fn lock_with_changes<'s>(
    store: &'s Store,
    inc: Incremental<'_>,
) -> (
    sparkles::store::WriteTxn<'s>,
    Option<Result<maintain::RawChanges, String>>,
) {
    let Some(since) = inc.since else {
        return (store.write_as(sparkles::commit::CommitKind::Reason), None);
    };
    for _ in 0..3 {
        let live = store.snapshot();
        let raw = maintain::changes(store, since, &live, inc.cache);
        let txn = store.write_as(sparkles::commit::CommitKind::Reason);
        if txn.base().commit == live.commit {
            return (txn, Some(raw));
        }
        if let Some(raw) = maintain::kept_changes(store, since, txn.base(), inc.cache) {
            return (txn, Some(raw));
        }
    }
    (
        store.write_as(sparkles::commit::CommitKind::Reason),
        Some(Err(
            "commits kept arriving while the changes were read".into()
        )),
    )
}

/// A run's result before its commit.
struct Done {
    /// the closure, for the next incremental run
    closure: Option<incremental::Closure>,
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
    txn: sparkles::store::WriteTxn<'_>,
    snap: &Arc<Snapshot>,
    profile: &Profile,
    digest: u64,
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
    let receipt = txn.commit()?;
    progress(opts, 1.0, "done");
    let warnings = done.warnings;
    if let Some(closure) = done.closure {
        let saved = maintain::save(
            store,
            &receipt,
            digest,
            &closure.terms,
            &done.generalized,
            done.generalized_saved.as_ref().map(|(m, c)| (m, c)),
        )
        .unwrap_or_else(|e| {
            tracing::warn!("saving the closure state for incremental runs: {e:#}");
            None
        });
        maintain::keep(
            store,
            inc.cache,
            &receipt,
            snap,
            digest,
            closure,
            done.generalized,
            &done.written.stored,
            saved,
        );
    } else if let Some(c) = inc.cache {
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
    txn: &mut sparkles::store::WriteTxn<'_>,
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
    txn: &mut sparkles::store::WriteTxn<'_>,
    snap: &Snapshot,
    d: &Derivation,
    opts: &ReasonOptions,
) -> anyhow::Result<Written> {
    let mut w = Written::default();
    // existing inferred graph
    let old_graph = snap.lookup_iri(INFERRED_GRAPH);
    let old: Vec<[Id; 4]> = match old_graph {
        Some(g) => snap
            .scan_keys(Perm::Gspo, &[g.0])?
            .iter()
            .map(|k| Perm::Gspo.to_quad(k))
            .collect(),
        None => Vec::new(),
    };

    // map local ids to store ids and collect the new graph content
    let mut new_triples: Vec<[Id; 3]> = Vec::new();
    for t in d.derived() {
        if !d.valid(t) {
            w.generalized += 1;
            continue;
        }
        new_triples.push(store_ids(txn, &d.terms, &mut w.stored, t)?);
    }

    let new_set: FxHashSet<[Id; 3]> = new_triples.iter().copied().collect();
    let mut old_set: FxHashSet<[Id; 3]> = FxHashSet::default();
    for q in &old {
        let t = [q[0], q[1], q[2]];
        if !new_set.contains(&t) {
            txn.delete(*q)?;
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
    Ok(w)
}

/// Update the closure of the materialization at `since` to the store's state now, and
/// write the changes of the inferred graph.
#[allow(clippy::too_many_arguments)]
fn update_closure(
    store: &Store,
    txn: &mut sparkles::store::WriteTxn<'_>,
    snap: &Arc<Snapshot>,
    profile: &Profile,
    extras: &Extras,
    since: u64,
    digest: u64,
    inc: Incremental<'_>,
    raw: Result<maintain::RawChanges, String>,
    opts: &ReasonOptions,
) -> anyhow::Result<Done> {
    if extras.geo_default_geometry {
        return Err(Fallback("GeoSPARQL default geometries are not monotonic".into()).into());
    }
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
    if let Ok(r) = &raw {
        let removed = r.base_removed() as u64;
        let explicit = snap.count(Perm::Gspo, &[Id::DEFAULT_GRAPH.0])? + removed;
        if explicit >= 10_000 && removed * LARGE_DELETION > explicit {
            return Err(Fallback(format!(
                "{removed} of {explicit} explicit triples were removed, more than a full run handles faster"
            ))
            .into());
        }
    }
    progress(opts, 0.0, "reading the previous closure");
    let prev = maintain::previous(store, snap, since, digest, inc.cache, raw)?;
    let mut closure = prev.closure;
    let mut warnings = Vec::new();
    let compiled = engine::compile(&rules, &closure.terms, &mut warnings);
    let limits = engine::Limits {
        max_iterations: opts.max_iterations,
        max_inferred: opts.max_inferred,
        cancel: opts.cancel.clone(),
        progress: opts.progress.clone(),
    };
    let ch = &prev.changes;
    progress(opts, 0.1, "updating the closure");
    let up = incremental::update(
        &mut closure,
        &compiled,
        &limits,
        &ch.base_added,
        &ch.base_removed,
    )?;
    let mut generalized = prev.generalized;
    let mut gen_changes = maintain::GeneralizedChanges::default();
    for t in &up.deleted {
        if closure.graph.position(t).is_none() && generalized.remove(t) {
            gen_changes.removed.push(*t);
        }
    }
    let added: Vec<[u64; 3]> = up.added(&closure).collect();
    for t in &added {
        if !closure.explicit(t) && !closure.terms.valid(t) && generalized.insert(*t) {
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
        .chain(&ch.inferred_removed);
    let written = write_changes(txn, snap, &closure, candidates)?;
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
        closure: Some(closure),
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

/// Bring the inferred graph in line with the closure for the triples that may have
/// changed: a triple belongs there when it is live, derived (not explicit) and valid RDF.
fn write_changes<'a>(
    txn: &mut sparkles::store::WriteTxn<'_>,
    snap: &Snapshot,
    c: &incremental::Closure,
    candidates: impl Iterator<Item = &'a [u64; 3]>,
) -> anyhow::Result<Written> {
    let mut w = Written::default();
    let graph = snap.lookup_iri(INFERRED_GRAPH);
    let mut seen: FxHashSet<[u64; 3]> = FxHashSet::default();
    let mut insert: Vec<[u64; 3]> = Vec::new();
    for t in candidates {
        if !seen.insert(*t) {
            continue;
        }
        let want = c.graph.position(t).is_some_and(|r| !c.is_base(r)) && c.terms.valid(t);
        let local = t.iter().any(|&x| Id(x).tag() == Tag::Local);
        let have = match graph {
            Some(g) if !local => txn.contains(&[Id(t[0]), Id(t[1]), Id(t[2]), g])?,
            _ => false,
        };
        if want && !have {
            insert.push(*t);
        } else if !want && have {
            let g = graph.expect("present");
            txn.delete([Id(t[0]), Id(t[1]), Id(t[2]), g])?;
            w.removed += 1;
        }
    }
    let before = match graph {
        Some(g) => snap.count(Perm::Gspo, &[g.0])?,
        None => 0,
    };
    if !insert.is_empty() {
        let g = match graph {
            Some(g) => g,
            None => txn.intern(&Term::NamedNode(NamedNode::new_unchecked(INFERRED_GRAPH)))?,
        };
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

/// Clear [`INFERRED_GRAPH`], run the rules over the default graph, and write every
/// derived triple that is not already in the default graph into [`INFERRED_GRAPH`] in
/// one write transaction.
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
    let mut txn = store.write_as(sparkles::commit::CommitKind::ReasonClear);
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

/// Run the rules over a snapshot's default graph and return the derived triples that
/// are valid RDF, without writing anything (dry run / testing). Blank nodes created by
/// `makeTemp` / `makeSkolem` get labels `r<hex>`.
pub fn infer(
    snap: Arc<Snapshot>,
    profile: &Profile,
    opts: &ReasonOptions,
) -> anyhow::Result<(Vec<Triple>, ReasonReport)> {
    let t0 = Instant::now();
    let d = derive(snap, profile, &Extras::default(), opts)?;
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
        ..Default::default()
    };
    Ok((out, report))
}
