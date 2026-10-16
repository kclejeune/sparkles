//! Graph-algorithm runs as requests (spec F14 §2.5 and §2.7): the JSON form of a call, and
//! write-back of its results to a graph in batched commits.
//!
//! A request names an algorithm, a projection (a named one or an inline definition) and
//! the parameters of the call. [`query_text`] turns it into the `SERVICE algo:…` query it
//! stands for, so a run through a request has the budgets, access control, caching and
//! results of any other query. [`run_write`] runs that query on one snapshot and writes
//! the rows to a graph as triples.
//!
//! A write-back run commits in batches of at most `batchSize` triples, cut at node
//! boundaries, so a node never has half its values updated. The first commit clears the
//! graph (mode `replace`) and holds the run record, and each commit after it updates the
//! record's counters in the same transaction as the triples they count, so after a crash
//! or a cancel the counters match what is in the graph. The last commit adds the end time
//! and the status. A record that has no end time belongs to a run that did not finish.
//! With `atomic` the whole run is one commit. Mode `merge` replaces only the values of
//! the written predicates of the nodes in the result, read inside each batch's own
//! transaction.

use super::{QueryOptions, query};
use crate::algo::catalog::{PredicateDef, ProjectionDef};
use crate::commit::CommitKind;
use crate::error::{Error, Result};
use crate::guard::WriteOptions;
use crate::id::Id;
use crate::index::Perm;
use crate::store::{Chunk, Store};
use crate::task::Control;
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{Literal, NamedNode, NamedOrBlankNode, Term};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The namespace of the algorithm run records (`spk:`).
pub const SPK: &str = "urn:x-sparkles:";
const PROV: &str = "http://www.w3.org/ns/prov#";
const SKOS_BROADER: &str = "http://www.w3.org/2004/02/skos/core#broader";
/// Triples in a batch when a request gives no `batchSize`.
pub const DEFAULT_BATCH: usize = 100_000;

/// A projection in a request: the name of one the dataset has, or its definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProjectionRef {
    Named(String),
    Inline(ProjectionDef),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WriteMode {
    /// clear the graph first
    #[default]
    Replace,
    /// replace the values of the written predicates for the nodes of the result
    Merge,
}

/// Where and how a run writes its results.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WriteSpec {
    /// the graph the results and the run record go to
    pub graph: String,
    #[serde(default)]
    pub mode: WriteMode,
    /// the predicate that each output is written with, such as `{"score": "ex:rank"}`
    pub predicates: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<usize>,
    /// one commit for the whole run
    #[serde(default)]
    pub atomic: bool,
}

/// A run of one algorithm.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlgoRequest {
    pub algorithm: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<ProjectionRef>,
    /// the parameters of the algorithm, named as in `algo:` without the prefix
    #[serde(default)]
    pub params: serde_json::Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<WriteSpec>,
}

impl AlgoRequest {
    /// A request from JSON, where the predicates of an inline projection may be bare
    /// strings.
    pub fn from_json(mut v: Value) -> Result<AlgoRequest> {
        if let Some(p) = v.get_mut("projection").filter(|p| p.is_object()) {
            ProjectionDef::bare_predicates(p);
        }
        serde_json::from_value(v).map_err(|e| invalid(format!("invalid request: {e}")))
    }
}

fn invalid(m: impl Into<String>) -> Error {
    Error::invalid(m)
}

/// An IRI of a request: `<iri>`, an absolute IRI, or a prefixed name of `prefixes`.
pub fn expand_iri(s: &str, prefixes: &[(String, String)]) -> Result<String> {
    let s = s.trim();
    let bare = s
        .strip_prefix('<')
        .and_then(|r| r.strip_suffix('>'))
        .unwrap_or(s);
    if let Some((p, local)) = bare.split_once(':')
        && !local.starts_with("//")
        && let Some((_, ns)) = prefixes.iter().find(|(n, _)| n == p)
    {
        // The expansion is spliced into query text between `<` and `>`, so it must be a
        // valid absolute IRI on its own. A local part with `>` or a space is refused.
        let full = format!("{ns}{local}");
        return oxiri::Iri::parse(full.as_str())
            .map(|_| full.clone())
            .map_err(|_| not_an_iri(s));
    }
    oxiri::Iri::parse(bare)
        .map(|_| bare.to_string())
        .map_err(|_| not_an_iri(s))
}

fn not_an_iri(s: &str) -> Error {
    invalid(format!(
        "'{s}' is not an absolute IRI or a known prefixed name"
    ))
}

fn iri_term(s: &str) -> String {
    format!("<{s}>")
}

fn number(v: &Value, name: &str) -> Result<String> {
    match v {
        Value::Number(n) => Ok(n.to_string()),
        _ => Err(invalid(format!("{name} takes a number"))),
    }
}

/// The projection parameters of a definition, as triples of the block.
fn projection_triples(def: &ProjectionDef, prefixes: &[(String, String)]) -> Result<Vec<String>> {
    let mut t = Vec::new();
    for p in def.predicates.iter().flatten() {
        let iri = iri_term(&expand_iri(p.iri(), prefixes)?);
        t.push(match p.direction() {
            Some(d) => format!("algo:predicate ({iri} algo:{d})"),
            None => format!("algo:predicate {iri}"),
        });
    }
    if let Some(d) = &def.direction {
        t.push(format!("algo:direction algo:{d}"));
    }
    for c in &def.node_classes {
        t.push(format!(
            "algo:nodeClass {}",
            iri_term(&expand_iri(c, prefixes)?)
        ));
    }
    if let Some(w) = &def.weight {
        t.push(format!(
            "algo:weight {}",
            iri_term(&expand_iri(w, prefixes)?)
        ));
    }
    if let Some(w) = def.default_weight {
        t.push(format!("algo:defaultWeight {w:?}"));
    }
    if let Some(a) = &def.aggregate {
        t.push(format!("algo:aggregate algo:{a}"));
    }
    Ok(t)
}

/// The outputs a request selects, in the order of its query's variables.
fn outputs(req: &AlgoRequest) -> Result<Vec<&'static str>> {
    let a = super::algosvc::Algo::from_name(&req.algorithm)
        .ok_or_else(|| invalid(format!("unknown algorithm '{}'", req.algorithm)))?;
    let graphalytics = match req.params.get("graphalytics") {
        Some(Value::Bool(b)) => *b,
        None => false,
        _ => return Err(invalid("unknown or malformed parameter 'graphalytics'")),
    };
    let clustering = matches!(a, super::algosvc::Algo::Clustering) && graphalytics;
    let mut v = vec!["node"];
    v.extend(
        a.outputs()
            .iter()
            .copied()
            .filter(|o| !clustering || *o != "triangles"),
    );
    Ok(v)
}

fn check_write_outputs(req: &AlgoRequest, w: &WriteSpec) -> Result<()> {
    let supported = outputs(req)?;
    for out in w.predicates.keys() {
        if !supported.contains(&out.as_str()) || out == "node" {
            return Err(invalid(format!(
                "'{out}' is not an output of {} that can be written",
                req.algorithm
            )));
        }
    }
    Ok(())
}

/// The query a request stands for: the rows of the algorithm, with the variables named
/// as the outputs (`?node`, `?score`, …). With `stats` only the summary is selected.
pub fn query_text(
    req: &AlgoRequest,
    prefixes: &[(String, String)],
    stats: bool,
) -> Result<(String, Vec<&'static str>)> {
    build(req, prefixes, !stats, stats)
}

/// The query of a request that selects the rows and, as the last variable `?summary`, the
/// summary of the run (the same literal in every row).
pub fn query_text_with_summary(
    req: &AlgoRequest,
    prefixes: &[(String, String)],
) -> Result<(String, Vec<&'static str>)> {
    build(req, prefixes, true, true)
}

fn build(
    req: &AlgoRequest,
    prefixes: &[(String, String)],
    rows: bool,
    summary: bool,
) -> Result<(String, Vec<&'static str>)> {
    let algo = super::algosvc::Algo::from_name(&req.algorithm)
        .ok_or_else(|| invalid(format!("unknown algorithm '{}'", req.algorithm)))?;
    let mut triples: Vec<String> = Vec::new();
    match &req.projection {
        None => {}
        Some(ProjectionRef::Named(n)) => triples.push(format!(
            "algo:projection {}",
            serde_json::to_string(n).unwrap()
        )),
        Some(ProjectionRef::Inline(d)) => {
            d.check()?;
            triples.extend(projection_triples(d, prefixes)?);
        }
    }
    for (k, v) in &req.params {
        let numeric = super::algosvc::NUMERIC.iter().any(|(n, _)| n == k);
        match (k.as_str(), v) {
            ("source", Value::String(s)) => triples.push(format!(
                "algo:source {}",
                iri_term(&expand_iri(s, prefixes)?)
            )),
            ("source", Value::Array(items)) => {
                for i in items {
                    let Value::String(s) = i else {
                        return Err(invalid("source takes IRIs"));
                    };
                    triples.push(format!(
                        "algo:source {}",
                        iri_term(&expand_iri(s, prefixes)?)
                    ));
                }
            }
            ("metric", Value::String(s)) => {
                triples.push(format!("algo:metric {}", serde_json::to_string(s).unwrap()))
            }
            ("graphalytics", Value::Bool(b)) => triples.push(format!("algo:graphalytics {b}")),
            ("iterationWeights", Value::Array(items)) => {
                let w: Result<Vec<String>> = items
                    .iter()
                    .map(|i| number(i, "iterationWeights"))
                    .collect();
                triples.push(format!("algo:iterationWeights ({})", w?.join(" ")));
            }
            (_, v) if numeric => triples.push(format!("algo:{k} {}", number(v, k)?)),
            _ => return Err(invalid(format!("unknown or malformed parameter '{k}'"))),
        }
    }
    let outs = outputs(req)?;
    let mut selected: Vec<&'static str> = if rows { outs } else { Vec::new() };
    if summary {
        selected.push("summary");
    }
    for o in &selected {
        triples.push(format!("algo:{o} ?{o}"));
    }
    let vars: String = selected.iter().map(|o| format!("?{o} ")).collect();
    let q = format!(
        "PREFIX algo: <{}> SELECT {vars}WHERE {{ SERVICE algo:{} {{ [] {} }} }}",
        super::algosvc::NS,
        algo.name(),
        triples.join(" ; ")
    );
    Ok((q, selected))
}

/// What a write-back run did.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    /// the IRI of the run record
    pub run: String,
    pub algorithm: String,
    /// the commit the results were computed at
    pub source_commit: u64,
    pub rows: u64,
    pub triples: u64,
    pub commits: u64,
    /// `complete`
    pub status: String,
}

type Group = (NamedOrBlankNode, Vec<(NamedNode, Term)>);

fn subject_of(t: &Term) -> Result<NamedOrBlankNode> {
    match t {
        Term::NamedNode(n) => Ok(NamedOrBlankNode::NamedNode(n.clone())),
        Term::BlankNode(b) => Ok(NamedOrBlankNode::BlankNode(b.clone())),
        _ => Err(invalid("a node of the result is not a resource")),
    }
}

fn spk(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{SPK}{local}"))
}

fn int(n: u64) -> Term {
    Term::Literal(Literal::new_typed_literal(n.to_string(), xsd::INTEGER))
}

/// The triples of the rows, grouped by node: every node's values together.
fn groups(
    req: &AlgoRequest,
    w: &WriteSpec,
    vars: &[String],
    rows: &[Vec<Option<Term>>],
    run: &str,
    prefixes: &[(String, String)],
    control: &WriteOptions,
) -> Result<(Vec<Group>, Vec<NamedNode>)> {
    let col = |name: &str| vars.iter().position(|v| v == name);
    let mut preds: BTreeMap<&str, NamedNode> = BTreeMap::new();
    check_write_outputs(req, w)?;
    for (out, p) in &w.predicates {
        control.check()?;
        preds.insert(
            out.as_str(),
            NamedNode::new(expand_iri(p, prefixes)?).map_err(|e| invalid(e.to_string()))?,
        );
    }
    if preds.is_empty() {
        return Err(invalid("write.predicates names no output to write"));
    }
    let node = col("node").ok_or_else(|| invalid("the run has no nodes"))?;
    let hierarchical = matches!(req.algorithm.as_str(), "louvain" | "leiden");
    let mut out: Vec<Group> = Vec::new();
    let mut index: std::collections::HashMap<Term, usize> = Default::default();
    let mut resources: BTreeMap<(i64, i64), Option<(i64, i64)>> = BTreeMap::new();
    let level_pred = preds.get("level").cloned();
    // the community of each node at each level, to link a community to its parent
    let mut last: std::collections::HashMap<(Term, i64), i64> = Default::default();
    for row in rows {
        control.check()?;
        let Some(n) = &row[node] else { continue };
        let subject = subject_of(n)?;
        let at = *index.entry(n.clone()).or_insert_with(|| {
            out.push((subject, Vec::new()));
            out.len() - 1
        });
        for (name, pred) in &preds {
            control.check()?;
            if *name == "level" {
                continue;
            }
            let Some(c) = col(name) else { continue };
            let Some(v) = &row[c] else { continue };
            if hierarchical && *name == "community" {
                let level = col("level")
                    .and_then(|l| row[l].as_ref())
                    .and_then(|t| match t {
                        Term::Literal(l) => l.value().parse::<i64>().ok(),
                        _ => None,
                    })
                    .unwrap_or(0);
                let id = match v {
                    Term::Literal(l) => l.value().parse::<i64>().unwrap_or(0),
                    _ => 0,
                };
                let res = NamedNode::new_unchecked(format!("{SPK}algo:{run}:{level}:{id}"));
                out[at].1.push((pred.clone(), Term::NamedNode(res)));
                last.insert((n.clone(), level), id);
                resources.entry((level, id)).or_insert(None);
            } else {
                out[at].1.push((pred.clone(), v.clone()));
            }
        }
    }
    if hierarchical {
        // a community's parent is the community of the next level that holds its nodes
        for ((n, level), id) in &last {
            control.check()?;
            if let Some(parent) = last.get(&(n.clone(), level + 1)) {
                resources.insert((*level, *id), Some((level + 1, *parent)));
            }
        }
        let broader = NamedNode::new_unchecked(SKOS_BROADER);
        let name = |l: i64, i: i64| {
            Term::NamedNode(NamedNode::new_unchecked(format!("{SPK}algo:{run}:{l}:{i}")))
        };
        for ((level, id), parent) in &resources {
            control.check()?;
            let mut po: Vec<(NamedNode, Term)> = Vec::new();
            if let Some(lp) = &level_pred {
                po.push((lp.clone(), int(*level as u64)));
            }
            if let Some((pl, pi)) = parent {
                po.push((broader.clone(), name(*pl, *pi)));
            }
            if !po.is_empty() {
                out.push((
                    NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(format!(
                        "{SPK}algo:{run}:{level}:{id}"
                    ))),
                    po,
                ));
            }
        }
    }
    let node_preds: Vec<NamedNode> = {
        let mut v: Vec<NamedNode> = preds
            .iter()
            .filter(|(n, _)| **n != "level")
            .map(|(_, p)| p.clone())
            .collect();
        v.dedup();
        v
    };
    Ok((out, node_preds))
}

fn now() -> Term {
    Term::Literal(Literal::new_typed_literal(
        crate::guard::config::now_rfc3339(),
        xsd::DATE_TIME,
    ))
}

/// Run a request and write its rows to the graph of its `write` part, in batches. The
/// results are computed on the snapshot at the start. `author` is recorded with the
/// changes, and `opts` limit and control the computation.
pub fn run_write(
    store: &Store,
    req: &AlgoRequest,
    prefixes: &[(String, String)],
    opts: &QueryOptions,
    author: Option<Arc<str>>,
    ctl: &Control,
) -> Result<RunReport> {
    let entered = std::time::Instant::now();
    let deadline = super::earlier_deadline(opts.effective_deadline(entered)?, ctl.deadline);
    let flags = crate::task::Cancellation::from_flags(
        opts.cancel.iter().cloned().chain([ctl.cancel.flag()]),
    );
    let cancellation = opts
        .cancellation
        .as_ref()
        .map_or_else(|| flags.clone(), |c| c.combine(&flags));
    let operation = WriteOptions {
        deadline,
        cancellation: Some(cancellation.clone()),
        ..Default::default()
    };

    let w = req
        .write
        .as_ref()
        .ok_or_else(|| invalid("the request has no write part"))?;
    let graph =
        NamedNode::new(expand_iri(&w.graph, prefixes)?).map_err(|e| invalid(e.to_string()))?;
    let (q, _) = query_text(req, prefixes, false)?;
    check_write_outputs(req, w)?;
    let snap = store.snapshot();
    let source_commit = snap.commit;
    let started = now();
    operation.check()?;
    ctl.progress.report(0.0, "computing");
    operation.check()?;
    let mut o = opts.clone();
    o.timeout = None;
    o.deadline = deadline;
    o.cancellation = Some(cancellation.clone());
    let result = query(snap, &q, &o)?;
    operation.check()?;
    let mut rows = Vec::with_capacity(result.len());
    for i in 0..result.table.len() {
        operation.check()?;
        let mut row = Vec::with_capacity(result.table.cols.len());
        for col in &result.table.cols {
            operation.check()?;
            row.push(result.term(col[i]));
        }
        rows.push(row);
    }
    let run = uuid::Uuid::new_v4().to_string();
    let run_iri = NamedNode::new_unchecked(format!("urn:uuid:{run}"));
    let (groups, node_preds) = groups(req, w, &result.vars, &rows, &run, prefixes, &operation)?;
    let mut total_triples = 0;
    for group in &groups {
        operation.check()?;
        total_triples += group.1.len() as u64;
    }
    // batches of whole groups
    let size = w.batch_size.unwrap_or(DEFAULT_BATCH).max(1);
    let mut batches: Vec<std::ops::Range<usize>> = Vec::new();
    if w.atomic || groups.is_empty() {
        batches.push(0..groups.len());
    } else {
        let (mut start, mut n) = (0usize, 0usize);
        for (i, g) in groups.iter().enumerate() {
            operation.check()?;
            n += g.1.len();
            if n >= size {
                batches.push(start..i + 1);
                start = i + 1;
                n = 0;
            }
        }
        if start < groups.len() {
            batches.push(start..groups.len());
        }
    }
    let params_json = serde_json::to_string(&serde_json::json!({
        "params": req.params,
    }))
    .unwrap_or_default();
    let projection_text = match &req.projection {
        Some(ProjectionRef::Named(n)) => n.clone(),
        Some(ProjectionRef::Inline(d)) => serde_json::to_string(d).unwrap_or_default(),
        None => String::new(),
    };
    let lit = |s: &str| Term::Literal(Literal::new_simple_literal(s));
    let record = |t: &mut Vec<(NamedNode, Term)>| {
        t.push((rdf::TYPE.into_owned(), Term::NamedNode(spk("AlgorithmRun"))));
        t.push((spk("algorithm"), lit(&req.algorithm)));
        t.push((spk("parameters"), lit(&params_json)));
        if !projection_text.is_empty() {
            t.push((spk("projection"), lit(&projection_text)));
        }
        t.push((spk("sourceCommit"), int(source_commit)));
        t.push((
            NamedNode::new_unchecked(format!("{PROV}startedAtTime")),
            started.clone(),
        ));
        t.push((spk("triplesPlanned"), int(total_triples)));
        t.push((spk("batchesPlanned"), int(batches.len() as u64)));
    };
    let (mut written, mut commits) = (0u64, 0u64);
    let n_batches = batches.len();
    let outcome = (|| -> Result<()> {
        for (bi, range) in batches.iter().enumerate() {
            operation.check()?;
            operation.check()?;
            ctl.progress.report(
                0.1 + 0.9 * bi as f32 / n_batches.max(1) as f32,
                &format!("writing batch {} of {n_batches}", bi + 1),
            );
            let wo = WriteOptions {
                cancel: Some(ctl.cancel.flag()),
                cancellation: Some(cancellation.clone()),
                deadline,
                author: author.clone(),
                message: Some(Arc::from(format!(
                    "algo run {run} batch {}/{n_batches}",
                    bi + 1
                ))),
                graphs: opts.graphs.clone(),
                ..Default::default()
            };
            let mut txn = store.try_write_with(CommitKind::Transaction, wo)?;
            let g = txn.intern(&Term::NamedNode(graph.clone()))?;
            let rec = txn.intern(&Term::NamedNode(run_iri.clone()))?;
            let put = |txn: &mut crate::store::WriteTxn<'_>,
                       s: Id,
                       p: &NamedNode,
                       o: &Term|
             -> Result<()> {
                let p = txn.intern(&Term::NamedNode(p.clone()))?;
                let o = txn.intern(o)?;
                txn.insert([s, p, o, g])?;
                Ok(())
            };
            if bi == 0 {
                if w.mode == WriteMode::Replace {
                    let view = txn.view();
                    let mut keys = Vec::new();
                    view.scan(Perm::Gspo, &[g.0], |c| {
                        operation.check()?;
                        match c {
                            Chunk::Block(b, s, e) => keys.extend((s..e).map(|i| b.key(i))),
                            Chunk::Row(k) => keys.push(k),
                        }
                        Ok(true)
                    })?;
                    for k in keys {
                        operation.check()?;
                        txn.delete(Perm::Gspo.to_quad(&k))?;
                    }
                }
                let mut t = Vec::new();
                record(&mut t);
                for (p, o) in &t {
                    operation.check()?;
                    put(&mut txn, rec, p, o)?;
                }
            }
            for (s, po) in &groups[range.clone()] {
                operation.check()?;
                let sid = txn.intern(&match s {
                    NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n.clone()),
                    NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.clone()),
                })?;
                if w.mode == WriteMode::Merge {
                    // read in this transaction: the values another writer added are replaced too
                    for p in &node_preds {
                        operation.check()?;
                        let pid = txn.intern(&Term::NamedNode(p.clone()))?;
                        let view = txn.view();
                        let mut keys = Vec::new();
                        view.scan(Perm::Spo, &[sid.0, pid.0], |c| {
                            operation.check()?;
                            match c {
                                Chunk::Block(b, s, e) => keys.extend((s..e).map(|i| b.key(i))),
                                Chunk::Row(k) => keys.push(k),
                            }
                            Ok(true)
                        })?;
                        for k in keys {
                            operation.check()?;
                            let q = Perm::Spo.to_quad(&k);
                            if q[3] == g {
                                txn.delete(q)?;
                            }
                        }
                    }
                }
                for (p, o) in po {
                    operation.check()?;
                    put(&mut txn, sid, p, o)?;
                    written += 1;
                }
            }
            // the counters of the record, with the triples they count
            let counters = [
                ("batchesWritten", bi as u64 + 1),
                ("triplesWritten", written),
            ];
            for (name, v) in counters {
                operation.check()?;
                let p = txn.intern(&Term::NamedNode(spk(name)))?;
                let view = txn.view();
                let mut keys = Vec::new();
                view.scan(Perm::Spo, &[rec.0, p.0], |c| {
                    operation.check()?;
                    match c {
                        Chunk::Block(b, s, e) => keys.extend((s..e).map(|i| b.key(i))),
                        Chunk::Row(k) => keys.push(k),
                    }
                    Ok(true)
                })?;
                for k in keys {
                    operation.check()?;
                    let q = Perm::Spo.to_quad(&k);
                    if q[3] == g {
                        txn.delete(q)?;
                    }
                }
                put(&mut txn, rec, &spk(name), &int(v))?;
            }
            if bi + 1 == n_batches {
                put(
                    &mut txn,
                    rec,
                    &NamedNode::new_unchecked(format!("{PROV}endedAtTime")),
                    &now(),
                )?;
                put(&mut txn, rec, &spk("status"), &lit("complete"))?;
            }
            operation.check()?;
            txn.commit()?;
            commits += 1;
            // The next batch starts with its own check. After the last commit the run is
            // finished and its record says `complete`, so a late cancel or deadline must
            // not turn it into an error.
        }
        ctl.progress.report(1.0, "done");
        Ok(())
    })();
    if let Err(e) = outcome {
        // best effort: say how the run ended, in a commit of its own
        let status = if matches!(e, Error::Cancelled) {
            "cancelled"
        } else {
            "failed"
        };
        if commits > 0 && commits < n_batches as u64 {
            // One cooperative cleanup budget; never wait for a competing writer.
            // No failure in this optional record may replace the operation's error.
            let _ = (|| -> Result<()> {
                let deadline = std::time::Instant::now()
                    .checked_add(std::time::Duration::from_millis(100))
                    .ok_or_else(|| invalid("cleanup deadline is too large"))?;
                let mut txn = store.try_write_with(
                    CommitKind::Transaction,
                    WriteOptions {
                        no_wait: true,
                        deadline: Some(deadline),
                        author: author.clone(),
                        graphs: opts.graphs.clone(),
                        message: Some(Arc::from(format!("algo run {run} {status}"))),
                        ..Default::default()
                    },
                )?;
                let g = txn.intern(&Term::NamedNode(graph.clone()))?;
                let rec = txn.intern(&Term::NamedNode(run_iri.clone()))?;
                for (p, o) in [
                    (format!("{PROV}endedAtTime"), now()),
                    (format!("{SPK}status"), lit(status)),
                ] {
                    let p = txn.intern(&Term::NamedNode(NamedNode::new_unchecked(p)))?;
                    let o = txn.intern(&o)?;
                    txn.insert([rec, p, o, g])?;
                }
                txn.commit()?;
                Ok(())
            })();
        }
        return Err(e);
    }
    Ok(RunReport {
        run: run_iri.as_str().to_string(),
        algorithm: req.algorithm.clone(),
        source_commit,
        rows: rows.len() as u64,
        triples: written,
        commits,
        status: "complete".into(),
    })
}

/// The inline projection of a request as predicates, for callers that build requests.
pub fn inline(predicates: &[&str]) -> ProjectionRef {
    ProjectionRef::Inline(ProjectionDef {
        predicates: Some(
            predicates
                .iter()
                .map(|p| PredicateDef::new(*p, None))
                .collect(),
        ),
        ..Default::default()
    })
}
