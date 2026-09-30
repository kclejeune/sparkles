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
mod engine;
mod graph;
pub mod parser;
mod terms;

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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
    opts: &ReasonOptions,
) -> anyhow::Result<Derivation> {
    let rules = profile
        .rules()
        .with_context(|| format!("parsing rules for profile '{}'", profile.name()))?;
    progress(opts, 0.0, "loading default graph");
    let mut g = load_default_graph(&snap)?;
    let terms = Terms::new(snap);
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
        base_len: out.base_len,
        graph: g,
        terms,
        rules: compiled.len(),
        iterations: out.iterations,
        warnings,
    })
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
    let t0 = Instant::now();
    let mut txn = store.write();
    let snap = txn.base().clone();
    let d = derive(snap.clone(), profile, opts)?;
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
    txn.commit()?;
    progress(opts, 1.0, "done");
    let report = ReasonReport {
        profile: profile.name().to_string(),
        rules: d.rules,
        iterations: d.iterations,
        inferred: new_set.len() as u64,
        millis: t0.elapsed().as_millis() as u64,
        warnings,
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

/// Remove [`INFERRED_GRAPH`]. Returns the number of triples removed.
pub fn clear(store: &Store) -> anyhow::Result<u64> {
    let mut txn = store.write();
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
    let d = derive(snap, profile, opts)?;
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
    };
    Ok((out, report))
}
