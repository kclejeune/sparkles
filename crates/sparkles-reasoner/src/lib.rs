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

mod builtins;
pub mod diagnostics;
mod engine;
pub mod extras;
mod graph;
pub mod parser;
mod terms;

pub use extras::{Extras, UnknownVocabulary, Vocabulary};
pub use parser::{
    BuiltinCall, Clause, Direction, Node, Rule, RuleParseError, TriplePattern, parse_rules,
};

use anyhow::Context as _;
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
use terms::{Kind, LocalTerm, Terms};

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
    let mut rules = profile
        .rules()
        .with_context(|| format!("parsing rules for profile '{}'", profile.name()))?;
    for v in &extras.vocabularies {
        rules.extend(
            v.rules()
                .with_context(|| format!("parsing the {v} vocabulary"))?,
        );
    }
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
    let out = engine::run(&mut g, &compiled, &terms, &limits)?;
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
        matches!(self.terms.kind(t[0]), Kind::Iri | Kind::BNode)
            && self.terms.kind(t[1]) == Kind::Iri
            && self.terms.kind(t[2]) != Kind::Other
    }
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
    extras.validate()?;
    let t0 = Instant::now();
    let mut txn = store.write_as(sparkles::commit::CommitKind::Reason);
    let snap = txn.base().clone();
    let d = derive(snap.clone(), profile, extras, opts)?;
    progress(opts, 0.8, "writing inferred graph");
    let mut warnings = d.warnings.clone();

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
    let mut local_ids: FxHashMap<u64, Id> = FxHashMap::default();
    let mut generalized = 0u64;
    for t in d.derived() {
        if !d.valid(t) {
            generalized += 1;
            continue;
        }
        let mut ids = [Id::UNDEF; 3];
        for (i, &x) in t.iter().enumerate() {
            ids[i] = if Id(x).tag() == Tag::Local {
                match local_ids.get(&x) {
                    Some(&id) => id,
                    None => {
                        let id = match d.terms.local(x) {
                            Some(LocalTerm::Key(k)) => txn.intern_key(&k)?,
                            Some(LocalTerm::BNode) => txn.new_bnode(),
                            None => anyhow::bail!("dangling local term id {x:#x}"),
                        };
                        local_ids.insert(x, id);
                        id
                    }
                }
            } else {
                Id(x)
            };
        }
        new_triples.push(ids);
    }
    if generalized > 0 {
        warnings.push(format!(
            "{generalized} derived generalized triples (literal subjects or non-IRI predicates) were not written"
        ));
    }

    let new_set: FxHashSet<[Id; 3]> = new_triples.iter().copied().collect();
    let mut old_set: FxHashSet<[Id; 3]> = FxHashSet::default();
    for q in &old {
        let t = [q[0], q[1], q[2]];
        if !new_set.contains(&t) {
            txn.delete(*q)?;
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
    let receipt = txn.commit()?;
    progress(opts, 1.0, "done");
    let report = ReasonReport {
        profile: profile.name().to_string(),
        rules: d.rules,
        iterations: d.iterations,
        inferred: new_set.len() as u64,
        millis: t0.elapsed().as_millis() as u64,
        warnings,
        receipt: Some(receipt),
    };
    tracing::info!(
        profile = %report.profile,
        rules = report.rules,
        iterations = report.iterations,
        inferred = report.inferred,
        millis = report.millis,
        "materialized inferences"
    );
    Ok(report)
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
    };
    Ok((out, report))
}
