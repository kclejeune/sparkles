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
/// The values of the `metric` parameter of node similarity.
const METRICS: &[&str] = &["jaccard", "overlap"];
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
        if oxiri::Iri::parse(full.as_str()).is_err() {
            return Err(not_an_iri(s));
        }
        return Ok(full);
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
        Some(ProjectionRef::Named(n)) => {
            // only a catalog name reaches the text, so its quoting never matters
            if !crate::algo::catalog::valid_name(n) {
                return Err(invalid(format!("'{n}' is not a valid projection name")));
            }
            triples.push(format!("algo:projection \"{n}\""))
        }
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
                // a known word only, written as the algo: name of the metric
                if !METRICS.contains(&s.as_str()) {
                    return Err(invalid(format!(
                        "metric is one of {}, not '{s}'",
                        METRICS.join(", ")
                    )));
                }
                triples.push(format!("algo:metric algo:{s}"))
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

/// The integer of a literal of the result, or 0.
fn integer_of(t: Option<Term>) -> i64 {
    match t {
        Some(Term::Literal(l)) => l.value().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

#[cfg(test)]
thread_local! {
    /// Terms of the result decoded so far on this thread, and how many were decoded when
    /// the first batch committed.
    static DECODED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static DECODED_AT_FIRST_COMMIT: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// One term of the result, decoded when a batch needs it.
fn decode(result: &super::QueryResult, id: Id) -> Option<Term> {
    #[cfg(test)]
    DECODED.with(|d| d.set(d.get() + 1));
    result.term(id)
}

/// How the rows of a result become triples: the column of the node, and for each
/// written output its column and predicate, in the order of the output names.
struct Layout {
    node: usize,
    /// column, predicate, and whether the column holds the community of a hierarchical
    /// run, which is written as a community resource
    outputs: Vec<(usize, NamedNode, bool)>,
    level: Option<usize>,
    level_pred: Option<NamedNode>,
    /// the predicates written for each node, which merge mode replaces
    node_preds: Vec<NamedNode>,
}

fn layout(
    req: &AlgoRequest,
    w: &WriteSpec,
    vars: &[String],
    prefixes: &[(String, String)],
    control: &WriteOptions,
) -> Result<Layout> {
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
    let mut outputs = Vec::new();
    for (name, pred) in &preds {
        if *name == "level" {
            continue;
        }
        if let Some(c) = col(name) {
            outputs.push((c, pred.clone(), hierarchical && *name == "community"));
        }
    }
    let mut node_preds: Vec<NamedNode> = preds
        .iter()
        .filter(|(n, _)| **n != "level")
        .map(|(_, p)| p.clone())
        .collect();
    node_preds.dedup();
    Ok(Layout {
        node,
        outputs,
        level: col("level").filter(|_| hierarchical),
        level_pred: preds.get("level").cloned().filter(|_| hierarchical),
        node_preds,
    })
}

/// A community of a hierarchical run: its level and number, and the community of the
/// next level that holds its nodes.
type Communities = BTreeMap<(i64, i64), Option<(i64, i64)>>;

/// The plan of a write-back, made in one pass over the result's identifiers without
/// decoding the result: the rows of each node together, the communities of a
/// hierarchical run, and where each batch ends.
struct Batches {
    /// the rows with a node, ordered by node, keeping the order of a node's rows
    order: Vec<usize>,
    communities: Communities,
    /// the groups (nodes, then community resources) written up to the end of each batch
    ends: Vec<usize>,
    triples: u64,
}

impl Layout {
    /// The triples of one row.
    fn triples(&self, table: &super::table::Table, i: usize) -> usize {
        self.outputs
            .iter()
            .filter(|(c, _, _)| !table.cols[*c][i].is_undef())
            .count()
    }

    /// The triples of a community resource: its level and its parent.
    fn resource_triples(&self, parent: &Option<(i64, i64)>) -> usize {
        usize::from(self.level_pred.is_some()) + usize::from(parent.is_some())
    }

    fn community_column(&self) -> Option<usize> {
        self.outputs.iter().find(|o| o.2).map(|o| o.0)
    }

    fn batches(
        &self,
        result: &super::QueryResult,
        w: &WriteSpec,
        control: &WriteOptions,
    ) -> Result<Batches> {
        let table = &result.table;
        let nodes = &table.cols[self.node];
        let mut order = Vec::new();
        order
            .try_reserve_exact(table.len())
            .map_err(|e| invalid(format!("the run's row order does not fit: {e}")))?;
        for (i, node) in nodes.iter().enumerate().take(table.len()) {
            if i % CHECK_ROWS == 0 {
                control.check()?;
            }
            if !node.is_undef() {
                order.push(i);
            }
        }
        control.check()?;
        // stable: a node's rows keep their order
        order.sort_by_key(|&i| nodes[i]);
        control.check()?;
        let size = w.batch_size.unwrap_or(DEFAULT_BATCH).max(1);
        let mut communities: Communities = BTreeMap::new();
        let mut ends = Vec::new();
        let (mut groups, mut pending, mut triples) = (0usize, 0usize, 0u64);
        let cut = |groups: usize, n: usize, pending: &mut usize, ends: &mut Vec<usize>| {
            *pending += n;
            if !w.atomic && *pending >= size {
                ends.push(groups);
                *pending = 0;
            }
        };
        let community = self.community_column();
        let mut levels: Vec<(i64, i64)> = Vec::new();
        let mut start = 0;
        while start < order.len() {
            control.check()?;
            let id = nodes[order[start]];
            let mut stop = start;
            let mut n = 0;
            levels.clear();
            while stop < order.len() && nodes[order[stop]] == id {
                let i = order[stop];
                n += self.triples(table, i);
                if let Some(c) = community
                    && !table.cols[c][i].is_undef()
                {
                    // the level and number of the node's community in this row
                    let level = self.level.map_or(0, |l| {
                        if table.cols[l][i].is_undef() {
                            0
                        } else {
                            integer_of(decode(result, table.cols[l][i]))
                        }
                    });
                    levels.push((level, integer_of(decode(result, table.cols[c][i]))));
                }
                stop += 1;
            }
            // a community's parent is the community of the next level that holds its
            // nodes; the last row of a node at a level counts
            for &key in &levels {
                communities.entry(key).or_insert(None);
            }
            levels.sort_by_key(|l| l.0);
            let mut kept = 0;
            for k in 0..levels.len() {
                if k + 1 == levels.len() || levels[k + 1].0 != levels[k].0 {
                    levels[kept] = levels[k];
                    kept += 1;
                }
            }
            levels.truncate(kept);
            for (k, &(level, id)) in levels.iter().enumerate() {
                if let Some(&next) = levels.get(k + 1)
                    && next.0 == level + 1
                {
                    communities.insert((level, id), Some(next));
                }
            }
            groups += 1;
            triples += n as u64;
            cut(groups, n, &mut pending, &mut ends);
            start = stop;
        }
        for parent in communities.values() {
            let n = self.resource_triples(parent);
            if n > 0 {
                groups += 1;
                triples += n as u64;
                cut(groups, n, &mut pending, &mut ends);
            }
        }
        if ends.last() != Some(&groups) || ends.is_empty() {
            ends.push(groups);
        }
        Ok(Batches {
            order,
            communities,
            ends,
            triples,
        })
    }
}

/// How often a pass over the rows checks the cancel flag and the deadline.
const CHECK_ROWS: usize = 1 << 12;

/// The groups of a write-back in order, each built when its batch is written: the
/// triples of a node, then those of each community resource.
struct Groups<'a> {
    result: &'a super::QueryResult,
    layout: &'a Layout,
    plan: &'a Batches,
    run: &'a str,
    /// the next row of `plan.order`
    at: usize,
    communities: std::collections::btree_map::Iter<'a, (i64, i64), Option<(i64, i64)>>,
}

impl Groups<'_> {
    /// The next group: its subject and its predicates and objects in `po`.
    fn next(&mut self, po: &mut Vec<(NamedNode, Term)>) -> Result<Option<NamedOrBlankNode>> {
        po.clear();
        let table = &self.result.table;
        let nodes = &table.cols[self.layout.node];
        let order = &self.plan.order;
        if self.at < order.len() {
            let id = nodes[order[self.at]];
            let subject = subject_of(
                &decode(self.result, id)
                    .ok_or_else(|| invalid("a node of the result has no term"))?,
            )?;
            while self.at < order.len() && nodes[order[self.at]] == id {
                let i = order[self.at];
                for (c, pred, community) in &self.layout.outputs {
                    let v = table.cols[*c][i];
                    if v.is_undef() {
                        continue;
                    }
                    let Some(term) = decode(self.result, v) else {
                        continue;
                    };
                    if *community {
                        let level = self.layout.level.map_or(0, |l| {
                            if table.cols[l][i].is_undef() {
                                0
                            } else {
                                integer_of(decode(self.result, table.cols[l][i]))
                            }
                        });
                        let number = integer_of(Some(term));
                        let res = format!("{SPK}algo:{}:{level}:{number}", self.run);
                        po.push((pred.clone(), Term::NamedNode(NamedNode::new_unchecked(res))));
                    } else {
                        po.push((pred.clone(), term));
                    }
                }
                self.at += 1;
            }
            return Ok(Some(subject));
        }
        for ((level, id), parent) in self.communities.by_ref() {
            if self.layout.resource_triples(parent) == 0 {
                continue;
            }
            if let Some(lp) = &self.layout.level_pred {
                po.push((lp.clone(), int(*level as u64)));
            }
            if let Some((pl, pi)) = parent {
                po.push((
                    NamedNode::new_unchecked(SKOS_BROADER),
                    Term::NamedNode(NamedNode::new_unchecked(format!(
                        "{SPK}algo:{}:{pl}:{pi}",
                        self.run
                    ))),
                ));
            }
            return Ok(Some(NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(
                format!("{SPK}algo:{}:{level}:{id}", self.run),
            ))));
        }
        Ok(None)
    }
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
    let run = uuid::Uuid::new_v4().to_string();
    let run_iri = NamedNode::new_unchecked(format!("urn:uuid:{run}"));
    // The result stays as identifiers. Each batch decodes the terms of its own nodes
    // when it is written, so the memory of a run is the result and one batch.
    let layout = layout(req, w, &result.vars, prefixes, &operation)?;
    let plan = layout.batches(&result, w, &operation)?;
    let total_triples = plan.triples;
    let mut groups = Groups {
        result: &result,
        layout: &layout,
        plan: &plan,
        run: &run,
        at: 0,
        communities: plan.communities.iter(),
    };
    let node_preds = &layout.node_preds;
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
        t.push((spk("batchesPlanned"), int(plan.ends.len() as u64)));
    };
    let (mut written, mut commits) = (0u64, 0u64);
    let n_batches = plan.ends.len();
    let outcome = (|| -> Result<()> {
        let mut group = 0usize;
        for (bi, &end) in plan.ends.iter().enumerate() {
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
            let mut po = Vec::new();
            while group < end {
                operation.check()?;
                let Some(s) = groups.next(&mut po)? else {
                    return Err(invalid("the run has fewer groups than it planned"));
                };
                group += 1;
                let sid = txn.intern(&match s {
                    NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n),
                    NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b),
                })?;
                if w.mode == WriteMode::Merge {
                    // read in this transaction: the values another writer added are replaced too
                    for p in node_preds {
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
                for (p, o) in &po {
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
            #[cfg(test)]
            if bi == 0 {
                DECODED_AT_FIRST_COMMIT.with(|d| d.set(Some(DECODED.with(|n| n.get()))));
            }
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
        rows: result.table.len() as u64,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{RdfFormat, Source};
    use crate::store::StoreOptions;

    fn chain(n: usize) -> Store {
        let s = Store::in_memory(StoreOptions::default());
        let mut data = String::from("@prefix ex: <http://ex/> .\n");
        for i in 0..n {
            data.push_str(&format!("ex:n{i} ex:p ex:n{} .\n", i + 1));
        }
        s.load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        s
    }

    fn request(algorithm: &str, outputs: serde_json::Value, batch: usize) -> AlgoRequest {
        AlgoRequest::from_json(serde_json::json!({
            "algorithm": algorithm,
            "projection": {"predicates": ["http://ex/p"]},
            "write": {"graph": "http://ex/out", "predicates": outputs, "batchSize": batch}
        }))
        .unwrap()
    }

    /// A write-back decodes the terms of a batch when it writes the batch. Before its
    /// first commit it has decoded the first batch's nodes, not the whole result.
    #[test]
    fn write_back_decodes_one_batch_at_a_time() {
        let s = chain(300);
        let req = request("wcc", serde_json::json!({"component": "http://ex/c"}), 10);
        DECODED.with(|d| d.set(0));
        DECODED_AT_FIRST_COMMIT.with(|d| d.set(None));
        let rep = run_write(
            &s,
            &req,
            &[],
            &QueryOptions::default(),
            None,
            &Control::none(),
        )
        .unwrap();
        assert_eq!((rep.rows, rep.triples, rep.commits), (301, 301, 31));
        let first = DECODED_AT_FIRST_COMMIT.with(|d| d.get()).unwrap();
        // ten nodes, each with its term and its component's
        assert!(
            first <= 2 * 10,
            "{first} terms decoded before the first commit"
        );
        assert_eq!(DECODED.with(|d| d.get()), 2 * 301);
    }

    /// Hierarchical communities are written as resources after the nodes, with their
    /// level and the community of the next level, as before the batches streamed.
    #[test]
    fn hierarchical_write_back_links_each_community_to_its_parent() {
        let s = Store::in_memory(StoreOptions::default());
        let mut data = String::from("@prefix ex: <http://ex/> .\n");
        // two cliques of five joined by one edge, then a pair far away
        for (base, size) in [(0, 5), (5, 5)] {
            for a in 0..size {
                for b in a + 1..size {
                    data.push_str(&format!("ex:n{} ex:p ex:n{} .\n", base + a, base + b));
                }
            }
        }
        data.push_str("ex:n4 ex:p ex:n5 .\n");
        s.load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        let req = request(
            "louvain",
            serde_json::json!({"community": "http://ex/c", "level": "http://ex/l"}),
            3,
        );
        let rep = run_write(
            &s,
            &req,
            &[],
            &QueryOptions::default(),
            None,
            &Control::none(),
        )
        .unwrap();
        let q = "SELECT ?n ?c WHERE { GRAPH <http://ex/out> { ?n <http://ex/c> ?c } }";
        let r = query(s.snapshot(), q, &QueryOptions::default()).unwrap();
        // every node has one community resource for each level
        let levels = r.len() / 10;
        assert!(levels >= 1 && r.len() == 10 * levels, "{}", r.len());
        let q = "SELECT ?c ?l WHERE { GRAPH <http://ex/out> { ?c <http://ex/l> ?l } }";
        let communities = query(s.snapshot(), q, &QueryOptions::default())
            .unwrap()
            .len();
        let q = "SELECT ?c ?p WHERE { GRAPH <http://ex/out> { ?c <http://www.w3.org/2004/02/skos/core#broader> ?p } }";
        let links = query(s.snapshot(), q, &QueryOptions::default())
            .unwrap()
            .len();
        // every community but those of the top level has a parent
        assert!(communities >= 2, "{communities}");
        if levels > 1 {
            assert!(links > 0 && links < communities, "{links} of {communities}");
        }
        assert_eq!(
            rep.triples,
            (r.len() + communities + links) as u64,
            "{rep:?}"
        );
    }
}
