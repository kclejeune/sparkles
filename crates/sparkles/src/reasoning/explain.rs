//! Why is this triple inferred? Derivation trees over the materialized inferences, or
//! over what RDFS on read answers (spec U10 §3.3, the `why` mode of F18 §2.1).
//!
//! The materialized inferences are explained at the commit their run read, when the
//! store still has that state, so the trees use the input graphs the closure was computed
//! from. A store whose history no longer reaches that commit is read at the head, and the
//! answer says so. Both searches read the caller's view: the input graphs are those the
//! caller reads, the snapshot hides the triples the caller's protections hide, and a tree
//! is returned only when all its leaves come from what the caller reads.

use crate::Dataset;
use crate::access::GraphAccess;
use crate::error::Result;
use crate::explain::Explained;
use crate::id::Id;
use crate::sparql::rdfs::{SchemaLeaves, SchemaSource};
use crate::store::Snapshot;
use oxrdf::Term;
use serde_json::{Value as J, json};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// The code of an explanation over its limits.
pub const REASONING_BUDGET: &str = "reasoning-budget";
/// The code of a triple with no derivation from facts the caller reads.
pub const NO_VISIBLE_DERIVATION: &str = "no-visible-derivation";

/// The most distinct top-level derivations a request may ask for.
pub const MAX_TREES: usize = 10;
/// The depth of a returned tree by default, and the most a request may ask for.
pub const DEFAULT_MAX_DEPTH: u32 = 32;
pub const MAX_DEPTH: u32 = 64;
/// The rule instances one explanation visits.
pub const MAX_INSTANCES: usize = 100_000;
/// The timeout of an explanation that gives none.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// What to explain, and the limits.
#[derive(Clone)]
pub struct ExplainRequest {
    pub triple: [Term; 3],
    /// distinct top-level derivations, 1 by default, at most [`MAX_TREES`]
    pub max_trees: usize,
    /// the depth of the trees, deeper nodes are cut as `ref` nodes
    pub max_depth: u32,
    /// [`DEFAULT_TIMEOUT`] when `None`
    pub timeout: Option<Duration>,
    /// the caller's view, `None` for one that reads everything
    pub access: Option<Arc<GraphAccess>>,
    pub cancel: Option<Arc<AtomicBool>>,
    /// the memory the rule evaluations may hold
    pub max_memory: Option<usize>,
}

impl ExplainRequest {
    pub fn new(triple: [Term; 3]) -> ExplainRequest {
        ExplainRequest {
            triple,
            max_trees: 1,
            max_depth: DEFAULT_MAX_DEPTH,
            timeout: None,
            access: None,
            cancel: None,
            max_memory: None,
        }
    }
}

/// Where a derivation comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExplainSource {
    /// the materialized inferences
    Materialized,
    /// RDFS on read
    RdfsOnRead,
}

impl ExplainSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ExplainSource::Materialized => "materialized",
            ExplainSource::RdfsOnRead => "rdfsOnRead",
        }
    }
}

/// An explanation.
#[derive(Clone, Debug)]
pub struct Explanation {
    pub explained: Explained,
    /// the commit the search read
    pub commit: u64,
    /// where the trees come from, `None` when none was found
    pub source: Option<ExplainSource>,
    /// for the materialized inferences, the head commit when the search read it because
    /// the store's history no longer has the run's commit
    pub read_at_head: bool,
    /// for the materialized inferences, their freshness at the head: a tree explains the
    /// closure as its run computed it
    pub freshness: Option<super::Freshness>,
}

#[cfg(feature = "reasoning")]
fn budget_error(m: String) -> crate::Error {
    crate::Error::Component(Box::new(
        crate::error::ComponentError::new("reasoner", REASONING_BUDGET, m.clone())
            .with_source(crate::explain::ExplainBudgetExceeded(m)),
    ))
}

/// The snapshot of the caller's view.
fn view_of(access: Option<&Arc<GraphAccess>>, snap: &Arc<Snapshot>) -> Result<Arc<Snapshot>> {
    match access {
        None => Ok(snap.clone()),
        Some(a) => a.bound(snap).masked(snap),
    }
}

fn readable(access: Option<&Arc<GraphAccess>>, snap: &Arc<Snapshot>, g: Id) -> bool {
    access.is_none_or(|a| a.bound(snap).readable_id(snap, g))
}

/// Explain `req.triple` in `ds`.
pub fn explain(ds: &Dataset, req: &ExplainRequest) -> Result<Explanation> {
    let deadline = Instant::now() + req.timeout.unwrap_or(DEFAULT_TIMEOUT);
    let max_trees = req.max_trees.clamp(1, MAX_TREES);
    let max_depth = req.max_depth.min(MAX_DEPTH);
    let store = ds.store();
    let head = store.snapshot();
    let access = req.access.as_ref();
    #[cfg(feature = "reasoning")]
    if let Some(rec) = ds.reasoning_record() {
        let (snap, read_at_head) = match rec.commit {
            Some(c) if c != head.commit => match store.snapshot_at(
                &crate::history::At::Commit(c),
                &crate::history::HistoryOptions::default(),
            ) {
                Ok((s, _)) => (s, false),
                Err(_) => (head.clone(), true),
            },
            _ => (head.clone(), false),
        };
        let view = view_of(access, &snap)?;
        let names = rec
            .input_graphs
            .clone()
            .unwrap_or_else(|| vec!["default".to_string()]);
        let mut inputs: Vec<Id> = Vec::new();
        for n in &names {
            let id = if n == "default" {
                Some(Id::DEFAULT_GRAPH)
            } else {
                snap.lookup_iri(n)
            };
            if let Some(id) = id
                && readable(access, &snap, id)
                && !inputs.contains(&id)
            {
                inputs.push(id);
            }
        }
        let inferred = snap
            .lookup_iri(super::INFERRED_GRAPH)
            .filter(|&g| readable(access, &snap, g));
        let profile = rec.profile()?;
        let extras = rec.extras()?;
        let input = sparkles_reasoner::explain::ExplainInput {
            snap: view,
            profile: &profile,
            extras: &extras,
            inputs: &inputs,
            inferred,
        };
        let opts = sparkles_reasoner::explain::ExplainOptions {
            max_trees,
            max_depth,
            max_instances: MAX_INSTANCES,
            deadline: Some(deadline),
            cancel: req.cancel.clone(),
            max_memory: req.max_memory,
        };
        let found =
            sparkles_reasoner::explain::explain(&input, &req.triple, &opts).map_err(|e| {
                match e.downcast::<crate::explain::ExplainBudgetExceeded>() {
                    Ok(b) => budget_error(b.0),
                    Err(e) => crate::handles::from_anyhow(e),
                }
            })?;
        if let Explained::Found { .. } = found {
            return Ok(Explanation {
                explained: found,
                commit: snap.commit,
                source: Some(ExplainSource::Materialized),
                read_at_head,
                freshness: Some(super::freshness(&rec, store, head.commit)),
            });
        }
    }
    if let Some(on) = ds.reasoning().rdfs().get() {
        let view = view_of(access, &head)?;
        let schema = on.schema(&view)?;
        let leaves = match &on.source {
            SchemaSource::Fixed(_) => SchemaLeaves::Document,
            SchemaSource::Graph(g) => {
                let id = match g {
                    None => Some(Id::DEFAULT_GRAPH),
                    Some(iri) => head.lookup_iri(iri),
                };
                match id {
                    Some(id) if readable(access, &head, id) => SchemaLeaves::Graph(id),
                    _ => SchemaLeaves::Hidden,
                }
            }
        };
        let inferred = head.lookup_iri(super::INFERRED_GRAPH);
        let mut graphs = vec![Id::DEFAULT_GRAPH];
        graphs.extend(head.graph_ids()?);
        graphs.retain(|&g| Some(g) != inferred && readable(access, &head, g));
        let found = schema.explain(
            &view,
            leaves,
            &graphs,
            &req.triple,
            crate::explain::ExplainLimits {
                max_trees,
                max_depth,
                deadline: Some(deadline),
            },
        )?;
        if let Explained::Found { .. } = found {
            return Ok(Explanation {
                explained: found,
                commit: head.commit,
                source: Some(ExplainSource::RdfsOnRead),
                read_at_head: false,
                freshness: None,
            });
        }
    }
    Ok(Explanation {
        explained: Explained::NotFound,
        commit: head.commit,
        source: None,
        read_at_head: false,
        freshness: None,
    })
}

fn bad(m: impl Into<String>) -> crate::Error {
    crate::Error::invalid(m)
}

/// A term of a request: the SPARQL JSON results form, `{"type": "uri" | "literal" |
/// "bnode", "value": …}` with `datatype` or `xml:lang` for a literal. A blank node is
/// named by the label a query result gave it.
pub fn term_from_json(v: &J, what: &str) -> Result<Term> {
    if let Some(o) = v.as_object()
        && o.get("type").and_then(J::as_str) == Some("bnode")
    {
        let label = o
            .get("value")
            .and_then(J::as_str)
            .ok_or_else(|| bad(format!("{what}.value must be a string")))?;
        let b = oxrdf::BlankNode::new(label)
            .map_err(|e| bad(format!("{what}: invalid blank node label '{label}': {e}")))?;
        return Ok(Term::BlankNode(b));
    }
    crate::overview::facets::rdf_term_from_json(v, what)
}

/// The mode of a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExplainMode {
    Why,
    WhyNot,
}

/// A request in the JSON form of `POST /$/reason/{ds}/explain` (spec F18 §2.1): `triple`
/// with `s`, `p` and `o`, `mode`, and the limits `maxTrees`, `maxDepth` and `timeout` in
/// seconds. The timeout is returned apart, for the caller to cap.
pub fn request_from_json(j: &J) -> Result<(ExplainRequest, ExplainMode, Option<f64>)> {
    let o = j
        .as_object()
        .ok_or_else(|| bad("the request must be a JSON object"))?;
    const KNOWN: &[&str] = &[
        "triple",
        "mode",
        "source",
        "maxTrees",
        "maxDepth",
        "maxMissing",
        "maxHypotheses",
        "timeout",
    ];
    if let Some(k) = o.keys().find(|k| !KNOWN.contains(&k.as_str())) {
        return Err(bad(format!(
            "unknown member '{k}' of the request: {}",
            KNOWN.join(", ")
        )));
    }
    let mode = match o.get("mode").and_then(J::as_str) {
        Some("why") | None => ExplainMode::Why,
        Some("whyNot") => ExplainMode::WhyNot,
        Some(m) => {
            return Err(bad(format!(
                "mode must be \"why\" or \"whyNot\", not '{m}'"
            )));
        }
    };
    match o.get("source").and_then(J::as_str) {
        None | Some("materialized") => {}
        Some(s) => {
            return Err(bad(format!(
                "source '{s}' is not available: only \"materialized\" is"
            )));
        }
    }
    let t = o
        .get("triple")
        .and_then(J::as_object)
        .ok_or_else(|| bad("triple must be an object with s, p and o"))?;
    let term = |k: &str| -> Result<Term> {
        term_from_json(
            t.get(k)
                .ok_or_else(|| bad(format!("triple.{k} is missing")))?,
            &format!("triple.{k}"),
        )
    };
    let triple = [term("s")?, term("p")?, term("o")?];
    if !matches!(triple[1], Term::NamedNode(_)) {
        return Err(bad("triple.p must be an IRI"));
    }
    if matches!(triple[0], Term::Literal(_)) {
        return Err(bad("triple.s must be an IRI or a blank node"));
    }
    let int = |k: &str, lo: u64, hi: u64| -> Result<Option<u64>> {
        match o.get(k) {
            None | Some(J::Null) => Ok(None),
            Some(v) => v
                .as_u64()
                .filter(|n| (lo..=hi).contains(n))
                .map(Some)
                .ok_or_else(|| bad(format!("{k} must be an integer from {lo} to {hi}"))),
        }
    };
    let mut req = ExplainRequest::new(triple);
    if let Some(n) = int("maxTrees", 1, MAX_TREES as u64)? {
        req.max_trees = n as usize;
    }
    if let Some(n) = int("maxDepth", 1, MAX_DEPTH as u64)? {
        req.max_depth = n as u32;
    }
    let timeout = match o.get("timeout") {
        None | Some(J::Null) => None,
        Some(v) => Some(
            v.as_f64()
                .filter(|t| t.is_finite() && *t > 0.0)
                .ok_or_else(|| bad("timeout must be a positive number of seconds"))?,
        ),
    };
    Ok((req, mode, timeout))
}

fn derivation_json(d: &crate::explain::Derivation) -> J {
    use crate::overview::facets::term_json;
    let mut o = json!({
        "id": d.id,
        "triple": {
            "s": term_json(&d.triple[0]),
            "p": term_json(&d.triple[1]),
            "o": term_json(&d.triple[2]),
        },
        "kind": d.kind.as_str(),
        "depth": d.depth,
    });
    if let Some(g) = &d.graph {
        o["graph"] = g.as_str().into();
    }
    if let Some(r) = &d.rule {
        let mut rj = json!({ "name": r.name, "profile": r.profile });
        if let Some(id) = &r.owl2rl {
            rj["owl2rl"] = id.as_str().into();
        }
        o["rule"] = rj;
    }
    if d.shared {
        o["shared"] = true.into();
    }
    if !d.premises.is_empty() {
        o["premises"] = d.premises.iter().map(derivation_json).collect();
    }
    o
}

/// The JSON answer of an explanation that found trees (spec F18 §2.1, `mode: "why"`),
/// with where the trees come from and the inferences' freshness.
pub fn explanation_json(e: &Explanation) -> J {
    let mut o = json!({ "mode": "why", "commit": e.commit });
    match &e.explained {
        Explained::Found { trees, more } => {
            o["entailed"] = true.into();
            o["trees"] = trees.iter().map(derivation_json).collect();
            o["more"] = (*more).into();
        }
        Explained::NotFound => {
            o["entailed"] = false.into();
        }
    }
    if let Some(s) = e.source {
        o["source"] = s.as_str().into();
    }
    if e.read_at_head {
        o["readAtHead"] = true.into();
    }
    if let Some(f) = &e.freshness {
        o["stale"] = f.stale.into();
        if let Some(n) = f.commits_since {
            o["commitsSince"] = n.into();
        }
        if let Some(r) = &f.reason {
            o["staleReason"] = r.as_str().into();
        }
    }
    o
}

const KNOWN_PREFIXES: &[(&str, &str)] = &[
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
];

/// The prefixes of `own` with rdf, rdfs, owl and xsd added where `own` does not bind them.
fn with_known(own: &[(String, String)]) -> Vec<(String, String)> {
    let mut all: Vec<(String, String)> = own.to_vec();
    for (p, ns) in KNOWN_PREFIXES {
        if !all.iter().any(|(q, _)| q == p) {
            all.push((p.to_string(), ns.to_string()));
        }
    }
    all
}

/// A triple written as Turtle, such as `<http://ex.org/tom> a ex:Animal`, with the
/// prefixes `prefixes` and rdf, rdfs, owl and xsd known. A blank node is named by its
/// label.
pub fn triple_from_text(text: &str, prefixes: &[(String, String)]) -> Result<[Term; 3]> {
    let mut doc = String::new();
    for (p, ns) in with_known(prefixes) {
        doc.push_str(&format!("@prefix {p}: <{ns}> .\n"));
    }
    doc.push_str(text.trim().trim_end_matches('.'));
    doc.push_str(" .\n");
    let parser =
        oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::Turtle).for_slice(doc.as_bytes());
    let mut triples = Vec::new();
    for q in parser {
        let q = q.map_err(|e| bad(format!("the triple does not parse: {e}")))?;
        triples.push(q);
    }
    let [q] = <[oxrdf::Quad; 1]>::try_from(triples)
        .map_err(|v| bad(format!("give one triple, not {}", v.len())))?;
    let s = match q.subject {
        oxrdf::NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n),
        oxrdf::NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b),
    };
    Ok([s, Term::NamedNode(q.predicate), q.object])
}

/// A term with the longest matching prefix, or as N-Triples.
pub fn short_term(t: &Term, prefixes: &[(String, String)]) -> String {
    if let Term::NamedNode(n) = t {
        if n.as_str() == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type" {
            return "a".to_string();
        }
        let best = with_known(prefixes)
            .into_iter()
            .filter(|(_, ns)| !ns.is_empty() && n.as_str().starts_with(ns.as_str()))
            .max_by_key(|(_, ns)| ns.len());
        if let Some((p, ns)) = best {
            let local = &n.as_str()[ns.len()..];
            if !local.is_empty()
                && local
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            {
                return format!("{p}:{local}");
            }
        }
    }
    t.to_string()
}

fn short_triple(t: &[Term; 3], prefixes: &[(String, String)]) -> String {
    format!(
        "{} {} {}",
        short_term(&t[0], prefixes),
        short_term(&t[1], prefixes),
        short_term(&t[2], prefixes)
    )
}

fn how(d: &crate::explain::Derivation) -> String {
    use crate::explain::DerivationKind as K;
    match d.kind {
        K::Asserted => match &d.graph {
            Some(g) => format!("asserted in graph {g}"),
            None => "asserted by the RDFS-on-read schema".to_string(),
        },
        K::Axiom => format!(
            "axiom of {}",
            d.rule.as_ref().map_or("the rules", |r| r.name.as_str())
        ),
        K::Derived => {
            let r = d.rule.as_ref();
            let mut s = format!("derived by {}", r.map_or("a rule", |r| r.name.as_str()));
            if let Some(r) = r {
                s.push_str(&format!(" ({}", r.profile));
                if let Some(id) = &r.owl2rl {
                    s.push_str(&format!(", OWL 2 RL {id}"));
                }
                s.push(')');
            }
            if d.shared {
                s.push_str(&format!(", as #{} above", d.id));
            }
            s
        }
        K::Ref => "derived deeper than the depth limit".to_string(),
    }
}

fn tree_lines(
    d: &crate::explain::Derivation,
    level: usize,
    prefixes: &[(String, String)],
    out: &mut String,
) {
    out.push_str(&format!(
        "{}{}    {}\n",
        "  ".repeat(level),
        short_triple(&d.triple, prefixes),
        how(d)
    ));
    for p in &d.premises {
        tree_lines(p, level + 1, prefixes, out);
    }
}

/// The trees of an explanation indented, one premise per line.
pub fn explanation_text(e: &Explanation, prefixes: &[(String, String)]) -> String {
    let mut out = String::new();
    let Explained::Found { trees, more } = &e.explained else {
        return "no derivation from facts you can read was found\n".to_string();
    };
    if e.freshness.as_ref().is_some_and(|f| f.stale == Some(true)) {
        out.push_str(
            "the inferences are stale: these trees explain the closure as its run computed it\n",
        );
    }
    for (i, t) in trees.iter().enumerate() {
        out.push_str(&format!(
            "derivation {} of {}{} (depth {}, from commit {}):\n",
            i + 1,
            trees.len(),
            if *more { "+" } else { "" },
            t.depth,
            e.commit
        ));
        tree_lines(t, 1, prefixes, &mut out);
    }
    out
}

/// One line for a tree: the triple, its premises and the rule, such as `ex:tom a
/// ex:Animal because ex:tom a ex:Cat (asserted) and ex:Cat rdfs:subClassOf ex:Animal
/// (asserted), by rdfs9`.
pub fn summary(d: &crate::explain::Derivation, prefixes: &[(String, String)]) -> String {
    use crate::explain::DerivationKind as K;
    let head = short_triple(&d.triple, prefixes);
    match d.kind {
        K::Asserted => format!("{head} is asserted"),
        K::Axiom => format!("{head} is an axiom"),
        K::Ref => format!("{head} is derived deeper than the depth limit"),
        K::Derived => {
            let parts: Vec<String> = d
                .premises
                .iter()
                .map(|p| {
                    let k = match p.kind {
                        K::Asserted => "asserted",
                        K::Axiom => "axiom",
                        _ => "derived",
                    };
                    format!("{} ({k})", short_triple(&p.triple, prefixes))
                })
                .collect();
            let because = match parts.len() {
                0 => String::new(),
                1 => parts[0].clone(),
                n => format!("{} and {}", parts[..n - 1].join(", "), parts[n - 1]),
            };
            format!(
                "{head} because {because}, by {}",
                d.rule.as_ref().map_or("a rule", |r| r.name.as_str())
            )
        }
    }
}
