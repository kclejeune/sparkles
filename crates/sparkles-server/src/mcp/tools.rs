//! The tools: argument parsing, snapshot resolution and execution. Everything here runs
//! on a blocking thread.

use super::errors::{ErrorContext, ToolError};
use super::render::{self, Prefixes, QueryPage, RowSource, Terms};
use super::{Call, McpServer, Outcome};
use crate::http::INFERRED_GRAPH;
use crate::state::Dataset;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use oxrdf::{BlankNode, Literal, NamedNode, Term};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use spargebra::algebra::{GraphPattern, PropertyPathExpression};
use spargebra::term::{NamedNodePattern, TermPattern};
use spargebra::{Query, SparqlParser};
use sparkles::commit::CommitRange;
use sparkles::error::Error;
use sparkles::history::{At, HistoryOptions};
use sparkles::schema::{ClassEntry, GraphSelection, PredicateEntry, SchemaOptions, SchemaReport};
use sparkles::sparql::{self, PlanInfo, QueryKind, QueryOptions};
use sparkles::store::Snapshot;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub fn run(
    server: &McpServer,
    name: &str,
    args: Map<String, Value>,
    call: &Call,
) -> Result<Outcome, ToolError> {
    let t = Tools { server, call };
    if call.cancel.load(Ordering::Relaxed) {
        return Err(t.ctx(&[], 0.0).engine(Error::Cancelled));
    }
    match name {
        "list_datasets" => t.list_datasets(args),
        "describe_schema" => t.describe_schema(args),
        "draft_shapes" => t.draft_shapes(args),
        "diff_schema" => t.diff_schema(args),
        "sparql_query" => t.sparql_query(args),
        "explain_query" => t.explain_query(args),
        "describe_resource" => t.describe_resource(args),
        "find_paths" => t.find_paths(args),
        "list_commits" => t.list_commits(args),
        "list_changes" => t.list_changes(args),
        #[cfg(feature = "text")]
        "search_text" => t.search_text(args),
        "similar_entities" => t.similar_entities(args),
        #[cfg(feature = "shacl")]
        "validate_shacl" => t.validate_shacl(args),
        #[cfg(feature = "shex")]
        "validate_shex" => t.validate_shex(args),
        #[cfg(feature = "fmt")]
        "format" => t.format(args),
        #[cfg(feature = "graphql")]
        "graphql_query" => t.graphql_query(args),
        "check_query" => t.check_query(args),
        "similar_queries" => t.similar_queries(args),
        "link_entities" => t.link_entities(args),
        "recall" => t.recall(args),
        "why_empty" => t.why_empty(args),
        "share_query" => t.share_query(args),
        "sparql_update" => t.sparql_update(args),
        // a stored query of a dataset (`<dataset>__<query>`)
        name => t.stored_query(name, args),
    }
}

pub(super) struct Tools<'a> {
    pub(super) server: &'a McpServer,
    pub(super) call: &'a Call,
}

/// Arguments as a typed struct; schema violations are `bad-argument`.
pub(super) fn parse<T: DeserializeOwned>(args: Map<String, Value>) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(args))
        .map_err(|e| ToolError::bad_argument(format!("invalid arguments: {e}")))
}

/// `name` within `min..=max` (default `default`).
pub(super) fn bounded(
    name: &str,
    v: Option<u64>,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, ToolError> {
    let v = v.unwrap_or(default);
    if v < min {
        return Err(ToolError::bad_argument(format!("{name} must be ≥ {min}")));
    }
    if v > max {
        return Err(ToolError::bad_argument(format!("{name} must be ≤ {max}")));
    }
    Ok(v)
}

fn query_text(q: &str) -> Result<(), ToolError> {
    if q.trim().is_empty() {
        return Err(ToolError::bad_argument("query must not be empty"));
    }
    if q.chars().count() > 65536 {
        return Err(ToolError::bad_argument(
            "query must be at most 65536 characters",
        ));
    }
    Ok(())
}

/// The most named snapshots `list_commits` lists, newest first.
const MAX_SNAPSHOTS_LISTED: usize = 20;

/// The state a call selects: `atCommit` (a commit), or `at` (a commit as a number or as
/// `commit:N`, `time:<RFC 3339>`, `snapshot:<name>` or `head`), but not both. `None`
/// reads the head.
pub(super) fn selector(
    at_commit: Option<u64>,
    at: Option<&Value>,
) -> Result<Option<At>, ToolError> {
    const FORMS: &str =
        "at is a commit number, or a string: commit:N, time:<RFC 3339>, snapshot:<name> or head";
    match (at_commit, at) {
        (Some(_), Some(_)) => Err(ToolError::bad_argument("give at or atCommit, not both")),
        (Some(c), None) => Ok(Some(At::Commit(c))),
        (None, None) => Ok(None),
        (None, Some(Value::Number(n))) => n
            .as_u64()
            .map(|c| Some(At::Commit(c)))
            .ok_or_else(|| ToolError::bad_argument(FORMS)),
        (None, Some(Value::String(s))) => s
            .trim()
            .parse::<At>()
            .map(Some)
            .map_err(|e| ToolError::bad_argument(e.to_string())),
        (None, Some(_)) => Err(ToolError::bad_argument(FORMS)),
    }
}

/// The remaining time until `deadline` (`Error::Timeout` once it has passed).
pub(super) fn remaining(deadline: Instant) -> Result<Duration, Error> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(Error::Timeout)
}

/// The prefixes of a dataset: the well-known ones (as `/$/prefixes/{ds}`) and those
/// seen at load or set on the dataset, which win.
pub(crate) fn dataset_prefixes(ds: &Dataset) -> BTreeMap<String, String> {
    let mut p = sparkles::io::standard_prefixes();
    p.extend(ds.store.prefixes());
    p
}

fn prefix_vec(p: &BTreeMap<String, String>) -> Vec<(String, String)> {
    p.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// An IRI argument: `<iri>`, a full IRI, a prefixed name, or (when allowed) a blank node
/// label of this store (`_:b…`).
pub(super) fn parse_iri(
    s: &str,
    prefixes: &BTreeMap<String, String>,
    allow_bnode: bool,
) -> Result<Term, ToolError> {
    let s = s.trim();
    let bad =
        |e: &dyn std::fmt::Display| ToolError::bad_argument(format!("invalid IRI '{s}': {e}"));
    if let Some(label) = s.strip_prefix("_:") {
        if !allow_bnode {
            return Err(bad(&"blank nodes are not allowed here"));
        }
        if sparkles::store::parse_bnode_label(label).is_none() {
            return Err(bad(
                &"not a blank node label of this dataset (use one from an earlier result)",
            ));
        }
        return BlankNode::new(label)
            .map(Term::BlankNode)
            .map_err(|e| bad(&e));
    }
    if let Some(inner) = s.strip_prefix('<').and_then(|i| i.strip_suffix('>')) {
        return NamedNode::new(inner)
            .map(Term::NamedNode)
            .map_err(|e| bad(&e));
    }
    if let Some((pfx, local)) = s.split_once(':')
        && !local.starts_with("//")
        && let Some(ns) = prefixes.get(pfx)
    {
        return NamedNode::new(format!("{ns}{local}"))
            .map(Term::NamedNode)
            .map_err(|e| bad(&e));
    }
    NamedNode::new(s).map(Term::NamedNode).map_err(|e| bad(&e))
}

/// Whether `q` is SPARQL Update rather than a query.
fn is_update(q: &str, prefixes: &[(String, String)]) -> bool {
    let mut p = SparqlParser::new();
    for (k, v) in prefixes {
        match p.with_prefix(k, v) {
            Ok(next) => p = next,
            Err(_) => return false,
        }
    }
    p.parse_update(q).is_ok()
}

fn query_type(q: &Query) -> &'static str {
    match q {
        Query::Select { .. } => "SELECT",
        Query::Ask { .. } => "ASK",
        Query::Construct { .. } => "CONSTRUCT",
        Query::Describe { .. } => "DESCRIBE",
    }
}

fn pattern_mut(q: &mut Query) -> &mut GraphPattern {
    match q {
        Query::Select { pattern, .. }
        | Query::Ask { pattern, .. }
        | Query::Construct { pattern, .. }
        | Query::Describe { pattern, .. } => pattern,
    }
}

pub(super) fn pattern(q: &Query) -> &GraphPattern {
    match q {
        Query::Select { pattern, .. }
        | Query::Ask { pattern, .. }
        | Query::Construct { pattern, .. }
        | Query::Describe { pattern, .. } => pattern,
    }
}

/// FNV-1a (the same selection hash as `/$/schema`, so the two share the cached report).
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

const BUILTIN_NAMESPACES: [&str; 5] = [
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "http://www.w3.org/2000/01/rdf-schema#",
    "http://www.w3.org/2002/07/owl#",
    "http://www.w3.org/2001/XMLSchema#",
    "http://www.w3.org/ns/shacl#",
];

fn builtin(iri: &str) -> bool {
    BUILTIN_NAMESPACES.iter().any(|ns| iri.starts_with(ns))
}

/// `1234` → `1234`, `12_345` → `12k`, `1_234_567` → `1.2M`, `80_000_000` → `80M`.
fn approx(n: f64) -> String {
    let n = n.max(0.0);
    let (v, unit) = if n >= 1e9 {
        (n / 1e9, "B")
    } else if n >= 1e6 {
        (n / 1e6, "M")
    } else if n >= 1e4 {
        (n / 1e3, "k")
    } else {
        return format!("{}", n.round() as u64);
    };
    if v < 10.0 {
        let s = format!("{v:.1}");
        format!("{}{unit}", s.strip_suffix(".0").unwrap_or(&s))
    } else {
        format!("{}{unit}", v.round() as u64)
    }
}

/// `s` cut to `max` characters with a `…(+N chars)` marker.
fn cut_marked(s: String, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s,
        Some((i, _)) => format!("{}…(+{} chars)", &s[..i], s[i..].chars().count()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Section {
    Summary,
    Classes,
    Predicates,
    Constraints,
    Profiles,
}

impl Section {
    fn name(self) -> &'static str {
        match self {
            Section::Summary => "summary",
            Section::Classes => "classes",
            Section::Predicates => "predicates",
            Section::Constraints => "constraints",
            Section::Profiles => "profiles",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DescribeSchemaArgs {
    dataset: Option<String>,
    section: Option<Section>,
    graph: Option<String>,
    reasoning: Option<bool>,
    include_builtin: Option<bool>,
    limit: Option<u64>,
    cursor: Option<String>,
    at_commit: Option<u64>,
    at: Option<Value>,
    subject_classes: Option<bool>,
    shapes: Option<Vec<String>>,
    classes: Option<Vec<String>>,
}

/// describe_schema continuation: base64url JSON.
#[derive(Serialize, Deserialize)]
struct Cursor {
    /// commit of the report
    c: u64,
    /// selection hash
    h: u64,
    /// section
    s: String,
    /// last IRI served
    a: String,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Format {
    Table,
    Json,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct SparqlQueryArgs {
    dataset: Option<String>,
    query: String,
    format: Option<Format>,
    max_rows: Option<u64>,
    max_bytes: Option<u64>,
    max_term_chars: Option<u64>,
    offset: Option<u64>,
    exact_total: Option<bool>,
    timeout_seconds: Option<f64>,
    reasoning: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExplainArgs {
    dataset: Option<String>,
    query: String,
    include_algebra: Option<bool>,
    reasoning: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Direction {
    Both,
    Outgoing,
    Incoming,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DescribeResourceArgs {
    dataset: Option<String>,
    iri: String,
    direction: Option<Direction>,
    max_triples: Option<u64>,
    lang: Option<String>,
    reasoning: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
    mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ListCommitsArgs {
    dataset: Option<String>,
    limit: Option<u64>,
    before: Option<u64>,
}

impl Tools<'_> {
    pub(super) fn cfg(&self) -> &super::McpConfig {
        self.server.cfg()
    }

    /// [`ToolError`] 403 unless the caller's grants reach the `info` endpoint of `ds`.
    pub(super) fn info_endpoint(&self, ds: &str) -> Result<(), ToolError> {
        if self
            .call
            .principal
            .can_at(ds, crate::auth::Endpoint::Info, crate::auth::Level::Read)
        {
            Ok(())
        } else {
            Err(ToolError::new(
                "forbidden",
                403,
                format!("the info endpoint of /{ds} is not allowed"),
            ))
        }
    }

    /// The dataset the call names, among those its principal may read.
    pub(super) fn dataset(&self, name: Option<&str>) -> Result<Arc<Dataset>, ToolError> {
        self.server.dataset(&self.call.principal, name)
    }

    /// The snapshot a call reads: the state `atCommit` or `at` selects (see
    /// [`selector`]), else the head. A past state that the pin table does not hold is
    /// read from the dataset's retained history, within `deadline`.
    pub(super) fn snapshot(
        &self,
        ds: &Arc<Dataset>,
        at_commit: Option<u64>,
        at: Option<&Value>,
        deadline: Instant,
    ) -> Result<Arc<Snapshot>, ToolError> {
        let sel = selector(at_commit, at)?;
        let ho = HistoryOptions {
            cancel: Some(self.call.cancel.clone()),
            deadline: Some(deadline),
        };
        let secs = deadline
            .saturating_duration_since(self.call.arrived)
            .as_secs_f64();
        let ctx = self.ctx(&[], secs);
        self.server
            .shared
            .pins
            .resolve(ds, sel.as_ref(), &ho, &|e| ctx.engine(e))
    }

    pub(super) fn ctx<'n>(
        &'n self,
        prefix_names: &'n [&'n str],
        timeout_secs: f64,
    ) -> ErrorContext<'n> {
        ErrorContext {
            timeout_secs,
            max_timeout_secs: self.cfg().max_timeout.as_secs_f64(),
            prefix_names,
            request_id: &self.call.request_id,
        }
    }

    pub(super) fn timeout(&self, secs: Option<f64>) -> Result<Duration, ToolError> {
        let max = self.cfg().max_timeout;
        match secs {
            None => Ok(self.cfg().default_timeout()),
            Some(s) if s.is_finite() && s > 0.0 && s <= max.as_secs_f64() => {
                Ok(Duration::from_secs_f64(s))
            }
            Some(_) => Err(ToolError::bad_argument(format!(
                "timeoutSeconds must be > 0 and ≤ {}",
                super::errors::secs(max.as_secs_f64())
            ))),
        }
    }

    /// Whether a call reads the materialized inferences (as the HTTP `reasoning`
    /// parameter: when the dataset has them and the argument is not false).
    pub(super) fn reasoning(ds: &Dataset, arg: Option<bool>) -> bool {
        arg != Some(false) && ds.reasoning.read().is_some()
    }

    pub(super) fn query_options(
        &self,
        ds: &str,
        endpoint: crate::auth::Endpoint,
        reasoning: bool,
        deadline: Instant,
        prefixes: &BTreeMap<String, String>,
    ) -> Result<QueryOptions, Error> {
        // a grant limited to other endpoints does not reach this tool
        if !self
            .call
            .principal
            .can_at(ds, endpoint, crate::auth::Level::Read)
        {
            return Err(Error::NotPermitted(format!(
                "the {} endpoint of /{ds} is not allowed",
                endpoint.as_str()
            )));
        }
        // the dataset's query defaults (RDFS on read, the inferred overlay and DESCRIBE),
        // as over HTTP
        let mut defaults = self
            .server
            .state
            .datasets()
            .get(ds)
            .map(|d| d.dataset.query_options())
            .unwrap_or_default();
        if !reasoning {
            defaults.default_graph_extra.clear();
        }
        let mut opts = QueryOptions {
            timeout: Some(remaining(deadline)?),
            max_rows: Some(self.server.state.limits.max_rows),
            max_memory_bytes: self.cfg().query_memory_bytes,
            max_rows_produced: self.server.state.limits.max_rows_produced,
            allow_service: self.cfg().allow_service,
            outbound: self.server.state.outbound.clone(),
            cancel: Some(self.call.cancel.clone()),
            prefixes: prefix_vec(prefixes),
            ..defaults
        };
        // SERVICE and LOAD are the principal's server permissions, and the graphs its
        // grants cover, as over HTTP
        crate::auth::restrict(&mut opts, &self.call.principal, ds, endpoint);
        Ok(opts)
    }

    /// Parse a query, telling SPARQL Update apart from a syntax error.
    pub(crate) fn parse_query(
        &self,
        q: &str,
        prefixes: &BTreeMap<String, String>,
        ctx: &ErrorContext,
    ) -> Result<Query, ToolError> {
        let pv = prefix_vec(prefixes);
        sparql::parse_query(q, None, &pv).map_err(|e| {
            if is_update(q, &pv) {
                let hint = if self.server.offers("sparql_update") {
                    "use sparql_update"
                } else {
                    "updates are disabled on this server; the tools here only read"
                };
                ToolError::new("not-a-query", 400, "this is SPARQL Update").hint(hint)
            } else {
                ctx.engine(e)
            }
        })
    }

    // ------------------------------------------------------------ list_datasets ------

    fn list_datasets(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let NoArgs {} = parse(args)?;
        let cfg = self.cfg();
        let p = &self.call.principal;
        let updates = self.server.may_update(p);
        let datasets = self.server.visible(p);
        let list: Vec<Value> = datasets
            .iter()
            .map(|ds| {
                let snap = ds.store.snapshot();
                let head = ds.store.head_commit();
                let modified = ds.store.commit(snap.commit).unwrap_or(head).timestamp();
                let reasoning = ds.reasoning.read().as_ref().map_or(Value::Null, |info| {
                    let f = crate::reasoning::freshness(info, &ds.store, snap.commit);
                    json!({ "profile": info.profile, "stale": f.stale })
                });
                // the quads the caller can read
                let quads = match p.view(&ds.name, crate::auth::Endpoint::Info) {
                    Some(v) => v.masked(&snap).and_then(|m| v.visible_quads(&m)).ok(),
                    None => Some(snap.len()),
                };
                let mut j = json!({
                    "name": ds.name,
                    "quads": quads,
                    "commit": snap.commit,
                    "modified": modified,
                    "reasoning": reasoning,
                    "textSearch": ds.dataset.indexes().text().enabled(),
                    "writable": updates && p.can(&ds.name, crate::auth::Level::Write),
                });
                // graphql_query reads this dataset
                if self.server.offers("graphql_query") && super::graphql_on(p, ds) {
                    j["graphql"] = true.into();
                }
                j
            })
            .collect();
        Ok(Outcome::Structured(json!({
            "datasets": list,
            "limits": {
                "defaultMaxRows": 100.min(cfg.max_rows),
                "maxRows": cfg.max_rows,
                "defaultMaxBytes": 65536.min(cfg.max_bytes),
                "maxBytes": cfg.max_bytes,
                "defaultTimeoutSeconds": cfg.default_timeout_secs(),
                "maxTimeoutSeconds": cfg.max_timeout_secs(),
                "service": cfg.allow_service,
                "updates": updates,
            }
        })))
    }

    // ----------------------------------------------------------- describe_schema ------

    fn describe_schema(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: DescribeSchemaArgs = parse(args)?;
        let section = a.section.unwrap_or(Section::Summary);
        let limit = bounded(
            "limit",
            a.limit,
            if section == Section::Summary { 25 } else { 100 },
            1,
            500,
        )? as usize;
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let timeout = self.cfg().max_timeout;
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let graph = match a.graph.as_deref().map(str::trim) {
            None | Some("default") => GraphSelection::Default,
            Some("union") => GraphSelection::Union,
            Some(g) => match parse_iri(g, &prefix_map, false)? {
                Term::NamedNode(n) => {
                    GraphSelection::parse(n.as_str()).map_err(ToolError::bad_argument)?
                }
                _ => {
                    return Err(ToolError::bad_argument(
                        "graph must be default, union or a graph IRI",
                    ));
                }
            },
        };
        let reasoning = Self::reasoning(&ds, a.reasoning);
        let subject_classes = a.subject_classes.unwrap_or(false);
        let mut selection = format!("{}\n{}\n{reasoning}\nfalse", graph.name(), graph.name());
        if subject_classes {
            selection.push_str("\nsubjectClasses");
        }
        let selection = fnv(&selection);
        // the constraints layer's sources: graph IRIs may be prefixed names
        let mut shapes: Vec<String> = Vec::new();
        for v in a.shapes.iter().flatten() {
            match v.trim() {
                v @ ("guard" | "none" | "default" | "union") => shapes.push(v.to_string()),
                v => match parse_iri(v, &prefix_map, false)? {
                    Term::NamedNode(n) => shapes.push(n.into_string()),
                    _ => {
                        return Err(ToolError::bad_argument(
                            "shapes are guard, none, default or graph IRIs",
                        ));
                    }
                },
            }
        }
        let shapes =
            crate::http::ShapesRequest::from_values(&shapes).map_err(ToolError::bad_argument)?;
        if a.shapes.is_some() && section != Section::Constraints {
            return Err(ToolError::bad_argument(
                "shapes applies to section=constraints",
            ));
        }
        if a.classes.is_some() && section != Section::Profiles {
            return Err(ToolError::bad_argument(
                "classes applies to section=profiles",
            ));
        }
        let mut profile_classes: Vec<String> = Vec::new();
        for c in a.classes.iter().flatten() {
            match parse_iri(c, &prefix_map, false)? {
                Term::NamedNode(n) => profile_classes.push(n.into_string()),
                _ => return Err(ToolError::bad_argument("classes must be IRIs")),
            }
        }
        let stale = |status: u16, msg: &str| {
            ToolError::new("stale-cursor", status, msg.to_string()).hint("restart without cursor")
        };
        let (snap, after) = match a.cursor.as_deref() {
            Some(c) => {
                if matches!(
                    section,
                    Section::Summary | Section::Constraints | Section::Profiles
                ) {
                    return Err(ToolError::bad_argument(
                        "cursor applies to section=classes or section=predicates",
                    ));
                }
                let cur: Cursor = URL_SAFE_NO_PAD
                    .decode(c.trim())
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .ok_or_else(|| stale(400, "malformed cursor"))?;
                if cur.h != selection || cur.s != section.name() {
                    return Err(stale(
                        400,
                        "the cursor belongs to another section, graph or reasoning setting",
                    ));
                }
                if a.at_commit.is_some_and(|at| at != cur.c) {
                    return Err(ToolError::bad_argument(
                        "atCommit differs from the cursor's commit",
                    ));
                }
                if a.at.is_some() {
                    return Err(ToolError::bad_argument(
                        "at does not apply with cursor: the cursor names its commit",
                    ));
                }
                let snap = self
                    .snapshot(&ds, Some(cur.c), None, self.call.arrived + timeout)
                    .map_err(|_| stale(409, "the schema changed since the first page"))?;
                (snap, Some(cur.a))
            }
            None => (
                self.snapshot(&ds, a.at_commit, a.at.as_ref(), self.call.arrived + timeout)?,
                None,
            ),
        };
        let report = self.schema_report(
            &ds,
            &snap,
            graph.clone(),
            reasoning,
            subject_classes,
            timeout,
            &ctx,
        )?;
        let include_builtin = a.include_builtin.unwrap_or(false);
        let mut terms = Terms::new(&prefixes, 500);
        let classes: Vec<&ClassEntry> = report
            .classes
            .iter()
            .filter(|c| include_builtin || !c.builtin)
            .collect();
        let hidden = report.classes.len() - classes.len();
        let mut out = json!({
            "dataset": ds.name,
            "commit": snap.commit,
            "graph": graph.name(),
            "reasoning": reasoning,
            "section": section.name(),
            "totals": {
                "triples": report.totals.triples,
                "classes": report.totals.classes,
                "predicates": report.totals.predicates,
            },
            "builtinClassesHidden": hidden,
        });
        let mut next = Value::Null;
        match section {
            Section::Summary => {
                let ontology: Vec<Value> = report
                    .ontology
                    .iter()
                    .map(|o| {
                        let mut e = json!({ "iri": terms.iri(&o.iri) });
                        if let Some(l) = render::choose(
                            o.labels
                                .iter()
                                .map(|l| (l.value.as_str(), l.lang.as_deref())),
                            "en",
                        ) {
                            e["label"] = l.into();
                        }
                        if let Some(v) = o.version_info.first() {
                            e["versionInfo"] = render::label_text(&v.value).into();
                        }
                        e
                    })
                    .collect();
                let roots: Vec<String> = report
                    .hierarchy
                    .roots
                    .iter()
                    .filter(|r| include_builtin || !builtin(r))
                    .take(25)
                    .map(|r| terms.iri(r))
                    .collect();
                let mut top_classes = classes.clone();
                top_classes.sort_by(|a, b| {
                    b.observed
                        .instances
                        .cmp(&a.observed.instances)
                        .then_with(|| a.iri.cmp(&b.iri))
                });
                let mut top_preds: Vec<&PredicateEntry> = report.predicates.iter().collect();
                top_preds.sort_by(|a, b| {
                    b.observed
                        .triples
                        .cmp(&a.observed.triples)
                        .then_with(|| a.iri.cmp(&b.iri))
                });
                out["ontology"] = ontology.into();
                out["roots"] = roots.into();
                out["classes"] = top_classes
                    .iter()
                    .take(limit)
                    .map(|c| class_json(c, &mut terms))
                    .collect();
                out["predicates"] = top_preds
                    .iter()
                    .take(limit)
                    .map(|p| predicate_json(p, &mut terms))
                    .collect();
            }
            Section::Profiles => {
                let schema = SchemaOptions {
                    graph: graph.clone(),
                    inferred_graph: Some(INFERRED_GRAPH.to_string()),
                    include_inferred: reasoning,
                    deadline: Some(self.call.arrived + timeout),
                    ..Default::default()
                };
                let p = self.class_profiles(&ds, &snap, schema, profile_classes, &ctx)?;
                out["profiles"] = super::schema_history::profiles_json(&p, limit, &mut terms);
            }
            Section::Constraints => {
                let view = self
                    .call
                    .principal
                    .view(&ds.name, crate::auth::Endpoint::Info);
                let layer = ds
                    .dataset
                    .schema()
                    .constraints_at(&snap, &shapes, view.as_deref())
                    .map_err(|e| match e {
                        Error::NotFound(m) => ToolError::new("unknown-graph", 404, m),
                        Error::Unsupported(m) => ToolError::new("unsupported", 501, m),
                        Error::Invalid(m) => ToolError::new("invalid-shapes", 400, m),
                        e => ctx.engine(e),
                    })?
                    .unwrap_or_default();
                out["constraints"] = layer
                    .sources
                    .iter()
                    .map(|src| constraints_json(src, &mut terms))
                    .collect();
            }
            Section::Classes | Section::Predicates => {
                let iris: Vec<&str> = if section == Section::Classes {
                    classes.iter().map(|c| c.iri.as_str()).collect()
                } else {
                    report.predicates.iter().map(|p| p.iri.as_str()).collect()
                };
                let start = after
                    .as_deref()
                    .map_or(0, |a| iris.partition_point(|x| *x <= a));
                let end = start.saturating_add(limit).min(iris.len());
                if end < iris.len() {
                    next = URL_SAFE_NO_PAD
                        .encode(
                            serde_json::to_vec(&Cursor {
                                c: snap.commit,
                                h: selection,
                                s: section.name().into(),
                                a: iris[end - 1].to_string(),
                            })
                            .unwrap_or_default(),
                        )
                        .into();
                }
                if section == Section::Classes {
                    out["classes"] = classes[start..end]
                        .iter()
                        .map(|c| class_json(c, &mut terms))
                        .collect();
                } else {
                    out["predicates"] = report.predicates[start..end]
                        .iter()
                        .map(|p| predicate_json(p, &mut terms))
                        .collect();
                }
            }
        }
        out["next"] = next;
        out["prefixes"] = json!(terms.used());
        Ok(Outcome::Structured(out))
    }

    /// The schema report of `snap`: the dataset's cached one when it matches, else a
    /// fresh one (which replaces the cache, as `/$/schema` does).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn schema_report(
        &self,
        ds: &Dataset,
        snap: &Arc<Snapshot>,
        graph: GraphSelection,
        reasoning: bool,
        subject_classes: bool,
        timeout: Duration,
        ctx: &ErrorContext,
    ) -> Result<Arc<SchemaReport>, ToolError> {
        self.info_endpoint(&ds.name)?;
        // a caller limited to some graphs gets a report of those, outside the cache
        let graphs = self
            .call
            .principal
            .view(&ds.name, crate::auth::Endpoint::Info);
        let deadline = self.call.arrived + timeout;
        let opts = SchemaOptions {
            graph,
            declared_graph: None,
            inferred_graph: Some(INFERRED_GRAPH.to_string()),
            include_inferred: reasoning,
            declared_from_inferred: false,
            deadline: Some(deadline),
            cancel: Some(self.call.cancel.clone()),
            max_entries: self.server.state.schema_max_entries,
            term_totals: false,
            graphs,
            subject_classes,
        };
        // the dataset's kept report, brought up to date, or a new one
        let (report, _) = ds
            .dataset
            .schema()
            .report_at(snap, &opts)
            .map_err(|e| ctx.schema_call(e))?;
        Ok(report)
    }

    // -------------------------------------------------------------- sparql_query ------

    fn sparql_query(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: SparqlQueryArgs = parse(args)?;
        self.run_sparql(a, Vec::new())
    }

    /// `sparql_query` with `bindings` as initial bindings (stored queries).
    pub(super) fn run_sparql(
        &self,
        a: SparqlQueryArgs,
        bindings: Vec<(String, Term)>,
    ) -> Result<Outcome, ToolError> {
        let cfg = self.cfg();
        query_text(&a.query)?;
        let max_rows = bounded(
            "maxRows",
            a.max_rows,
            100.min(cfg.max_rows as u64),
            1,
            cfg.max_rows as u64,
        )? as usize;
        let max_bytes = bounded(
            "maxBytes",
            a.max_bytes,
            65536.min(cfg.max_bytes as u64),
            1024,
            cfg.max_bytes as u64,
        )? as usize;
        let max_chars = bounded("maxTermChars", a.max_term_chars, 500, 16, 100_000)? as usize;
        let offset = bounded("offset", a.offset, 0, 0, u64::MAX >> 1)? as usize;
        let exact_total = a.exact_total.unwrap_or(true);
        let format = a.format.unwrap_or(Format::Table);
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let snap = self.snapshot(&ds, a.at_commit, a.at.as_ref(), self.call.arrived + timeout)?;
        let reasoning = Self::reasoning(&ds, a.reasoning);
        let t0 = Instant::now();
        let mut parsed = self.parse_query(&a.query, &prefix_map, &ctx)?;
        let parse_ms = t0.elapsed().as_secs_f64() * 1000.0;
        // exactTotal=false: stop after offset+maxRows+1 solutions
        let capped = !exact_total && matches!(parsed, Query::Select { .. });
        if capped {
            let p = pattern_mut(&mut parsed);
            let inner = std::mem::replace(
                p,
                GraphPattern::Bgp {
                    patterns: Vec::new(),
                },
            );
            *p = GraphPattern::Slice {
                inner: Box::new(inner),
                start: 0,
                length: Some(offset.saturating_add(max_rows).saturating_add(1)),
            };
        }
        let deadline = self.call.arrived + timeout;
        let mut opts = self
            .query_options(
                &ds.name,
                crate::auth::Endpoint::Query,
                reasoning,
                deadline,
                &prefix_map,
            )
            .map_err(|e| ctx.engine(e))?;
        opts.initial_bindings = bindings;
        let mut r = sparql::execute_query(snap.clone(), &parsed, &opts, parse_ms)
            .map_err(|e| ctx.engine(e))?;
        if r.kind == QueryKind::Select {
            sparql::select_star_order(&a.query, &mut r);
        }
        let elapsed_ms = (t0.elapsed().as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0;
        let text = match r.kind {
            QueryKind::Ask => match format {
                Format::Table => render::ask_table(snap.commit, r.boolean),
                Format::Json => render::ask_json(&ds.name, snap.commit, r.boolean, elapsed_ms),
            },
            kind => {
                let len = r.len();
                let start = offset.min(len);
                let end = start.saturating_add(max_rows).min(len);
                let (vars, rows) = if kind == QueryKind::Select {
                    let rows = (start..end)
                        .map(|i| r.table.cols.iter().map(|c| r.term(c[i])).collect())
                        .collect();
                    (Some(r.vars.clone()), RowSource::Solutions(rows))
                } else {
                    (None, RowSource::Triples(&r.triples[start..end]))
                };
                let page = QueryPage {
                    dataset: &ds.name,
                    commit: snap.commit,
                    query_type: query_type(&parsed),
                    vars,
                    rows,
                    more_beyond: len > end,
                    total: Some(len),
                    total_is_lower_bound: capped,
                    offset,
                    max_rows,
                    max_bytes,
                    max_chars,
                    elapsed_ms,
                };
                match format {
                    Format::Table => render::table(&page, &prefixes),
                    Format::Json => render::json(&page, &prefixes),
                }
            }
        };
        Ok(Outcome::Text(text))
    }

    // ------------------------------------------------------------- explain_query ------

    fn explain_query(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: ExplainArgs = parse(args)?;
        query_text(&a.query)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let timeout = self.cfg().default_timeout();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let snap = self.snapshot(&ds, a.at_commit, a.at.as_ref(), self.call.arrived + timeout)?;
        let reasoning = Self::reasoning(&ds, a.reasoning);
        let parsed = self.parse_query(&a.query, &prefix_map, &ctx)?;
        let opts = self
            .query_options(
                &ds.name,
                crate::auth::Endpoint::Query,
                reasoning,
                self.call.arrived + timeout,
                &prefix_map,
            )
            .map_err(|e| ctx.engine(e))?;
        let (algebra, plan) =
            sparql::explain(snap.clone(), &a.query, &opts).map_err(|e| ctx.engine(e))?;
        // A caller whose grants or protections hide some data must not learn from the
        // dictionary that a term occurs there, so its terms are looked up in its view.
        let view = self
            .call
            .principal
            .view(&ds.name, crate::auth::Endpoint::Query)
            .is_some()
            .then(|| {
                let mut opts = opts.clone();
                opts.union_default_graph = Some(false);
                opts.default_graph_extra.clear();
                super::memory::Reader {
                    snap: snap.clone(),
                    opts,
                    deadline: self.call.arrived + timeout,
                    reasoning,
                }
            });
        let mut lines = String::new();
        plan_lines(&plan, 0, &mut lines);
        // the planner's own notes first (spatial filters not pushed down, …)
        let mut warnings: Vec<Value> = plan
            .warnings
            .iter()
            .map(|w| json!({"code": w.code, "message": w.message}))
            .collect();
        // unknown constant terms
        let mut consts = Vec::new();
        constant_terms(pattern(&parsed), &mut consts);
        let mut seen = BTreeSet::new();
        let mut terms = Terms::new(&prefixes, 200);
        let mut lookups = 0;
        for t in consts {
            if warnings.len() >= 10 {
                break;
            }
            if !seen.insert(t.to_string()) {
                continue;
            }
            let known = match (snap.lookup_term(&t), &view) {
                (None, _) => false,
                (Some(_), None) => true,
                // past the lookups, an unchecked term gets no warning
                (Some(_), Some(_)) if lookups >= 20 => true,
                (Some(_), Some(r)) => {
                    lookups += 1;
                    r.ask(
                        &super::memory::exists_term(r),
                        vec![("t".into(), t.clone())],
                    )
                    .map_err(|e| ctx.engine(e))?
                }
            };
            if known {
                continue;
            }
            warnings.push(json!({
                "code": "unknown-term",
                "message": format!(
                    "{} does not occur in dataset {}; patterns using it match nothing. Check spelling with describe_schema.",
                    terms.term(&t),
                    ds.name
                ),
            }));
        }
        let root = plan.estimated_rows;
        let kind = query_type(&parsed);
        for (code, message) in plan_warnings(&plan, &parsed, 100.min(self.cfg().max_rows)) {
            warnings.push(json!({ "code": code, "message": message }));
        }
        let service =
            self.cfg().allow_service && self.call.principal.has(crate::auth::ServerPerm::Federate);
        if !service && has_service(pattern(&parsed)) {
            warnings.push(json!({
                "code": "service-disabled",
                "message": "SERVICE is disabled for MCP calls.",
            }));
        }
        let mut out = json!({
            "dataset": ds.name,
            "commit": snap.commit,
            "queryType": kind,
            // null when the caller's view hides the estimates
            "estimatedRows": (root >= 0.0).then(|| root.round() as u64),
            "plan": lines.trim_end(),
            "warnings": warnings,
        });
        if a.include_algebra.unwrap_or(false) {
            out["algebra"] = cut_marked(algebra, 8192).into();
        }
        Ok(Outcome::Structured(out))
    }

    // --------------------------------------------------------- describe_resource ------

    /// The `DESCRIBE` of the resource in `mode` (`cbd`, `scbd` or `outgoing`, as the
    /// dataset's DESCRIBE setting and `?describe=` name them), at most `max` triples,
    /// rendered `s p o` with the compact terms.
    fn description(
        &self,
        q: &Queries,
        mode: &str,
        max: usize,
        terms: &mut Terms,
        ctx: &ErrorContext,
    ) -> Result<Value, ToolError> {
        let mode = sparql::describe::DescribeMode::parse(mode)
            .map_err(|_| ToolError::bad_argument("mode must be cbd, scbd or outgoing"))?;
        let mut opts = q.opts.clone();
        opts.timeout = Some(remaining(q.deadline).map_err(|e| ctx.engine(e))?);
        opts.initial_bindings = vec![("r".to_string(), q.resource.clone())];
        // one triple more than shown tells whether the description goes on
        opts.describe.mode = mode;
        opts.describe.labels = false;
        opts.describe.max_triples = Some(max as u64 + 1);
        // the resource comes in as a binding (a blank node has no syntax in DESCRIBE)
        let r = sparql::query(
            q.snap.clone(),
            "DESCRIBE ?d WHERE { BIND(?r AS ?d) }",
            &opts,
        )
        .map_err(|e| ctx.engine(e))?;
        let triples: Vec<Value> = r
            .triples
            .iter()
            .take(max)
            .map(|t| {
                json!(format!(
                    "{} {} {}",
                    terms.term(&t.subject.clone().into()),
                    terms.term(&Term::NamedNode(t.predicate.clone())),
                    terms.term(&t.object)
                ))
            })
            .collect();
        Ok(json!({
            "mode": mode.name(),
            "triples": triples,
            "truncated": r.triples.len() > max,
        }))
    }

    fn describe_resource(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: DescribeResourceArgs = parse(args)?;
        let max_triples = bounded("maxTriples", a.max_triples, 50, 1, 500)? as usize;
        let direction = a.direction.unwrap_or(Direction::Both);
        let lang = a.lang.unwrap_or_else(|| "en".to_string());
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let timeout = self.cfg().default_timeout();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let resource = parse_iri(&a.iri, &prefix_map, true)?;
        let snap = self.snapshot(&ds, a.at_commit, a.at.as_ref(), self.call.arrived + timeout)?;
        let reasoning = Self::reasoning(&ds, a.reasoning);
        let deadline = self.call.arrived + timeout;
        let opts = self
            .query_options(
                &ds.name,
                crate::auth::Endpoint::Query,
                reasoning,
                deadline,
                &BTreeMap::new(),
            )
            .map_err(|e| ctx.engine(e))?;
        let q = Queries {
            snap: &snap,
            opts,
            deadline,
            resource: resource.clone(),
        };
        let mut terms = Terms::new(&prefixes, 200);
        let known = snap.lookup_term(&resource).is_some();
        // a term the dataset does not know has no triples: nothing to query
        let side = |outgoing: bool| -> Result<Side, ToolError> {
            if known {
                q.side(outgoing, max_triples).map_err(|e| ctx.engine(e))
            } else {
                Ok(Side::default())
            }
        };
        let out_side = (direction != Direction::Incoming)
            .then(|| side(true))
            .transpose()?;
        let in_side = (direction != Direction::Outgoing)
            .then(|| side(false))
            .transpose()?;
        let exists = known
            && match (&out_side, &in_side) {
                (Some(o), Some(i)) => o.total + i.total > 0,
                (Some(o), None) => o.total > 0 || q.ask(false).map_err(|e| ctx.engine(e))?,
                (None, Some(i)) => i.total > 0 || q.ask(true).map_err(|e| ctx.engine(e))?,
                (None, None) => false,
            };
        let types = if known {
            q.types().map_err(|e| ctx.engine(e))?
        } else {
            Vec::new()
        };
        // labels of the resource, its types and the sampled neighbours
        let mut wanted: Vec<&NamedNode> = Vec::new();
        if let Term::NamedNode(n) = &resource {
            wanted.push(n);
        }
        for t in &types {
            if let Term::NamedNode(n) = t {
                wanted.push(n);
            }
        }
        for side in [&out_side, &in_side].into_iter().flatten() {
            for (_, t) in &side.triples {
                if let Term::NamedNode(n) = t {
                    wanted.push(n);
                }
            }
        }
        let mut labels = if known {
            q.labels(&wanted).map_err(|e| ctx.engine(e))?
        } else {
            HashMap::new()
        };
        if known && let Term::BlankNode(_) = &resource {
            let own = q.own_labels().map_err(|e| ctx.engine(e))?;
            labels.insert(resource.to_string(), own);
        }
        let label_of = |t: &Term| label_of(&labels, t, &lang);
        let mut out = json!({
            "dataset": ds.name,
            "commit": snap.commit,
            "iri": terms.term(&resource),
            "exists": exists,
            "types": types.iter().map(|t| terms.term(t)).collect::<Vec<_>>(),
        });
        if let Some(l) = label_of(&resource) {
            out["label"] = l.into();
        }
        if let Some(side) = &out_side {
            let triples: Vec<Value> = side
                .triples
                .iter()
                .map(|(p, o)| {
                    let mut e = json!({ "p": terms.iri(p.as_str()), "o": terms.term(o) });
                    if let Some(l) = label_of(o) {
                        e["oLabel"] = l.into();
                    }
                    e
                })
                .collect();
            out["outgoing"] = side.json(triples, &mut terms);
        }
        if let Some(side) = &in_side {
            let triples: Vec<Value> = side
                .triples
                .iter()
                .map(|(p, s)| {
                    let mut e = json!({ "s": terms.term(s) });
                    if let Some(l) = label_of(s) {
                        e["sLabel"] = l.into();
                    }
                    e["p"] = terms.iri(p.as_str()).into();
                    e
                })
                .collect();
            out["incoming"] = side.json(triples, &mut terms);
        }
        if let Some(mode) = a.mode.as_deref() {
            out["description"] = self.description(&q, mode, max_triples, &mut terms, &ctx)?;
        }
        out["prefixes"] = json!(terms.used());
        Ok(Outcome::Structured(out))
    }

    // -------------------------------------------------------------- list_commits ------

    fn list_commits(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: ListCommitsArgs = parse(args)?;
        let limit = bounded("limit", a.limit, 10, 1, 100)? as usize;
        let ds = self.dataset(a.dataset.as_deref())?;
        self.info_endpoint(&ds.name)?;
        let restricted = self.call.principal.restricted(&ds.name);
        let range = a.before.map_or(CommitRange::Latest, CommitRange::Before);
        let head = ds.store.head_commit().seq;
        let page = ds.store.commits(range, limit);
        let next = page
            .commits
            .last()
            .filter(|c| c.seq > page.first_retained && page.commits.len() == limit)
            .map_or(Value::Null, |c| json!({ "before": c.seq }));
        let commits: Vec<Value> = page
            .commits
            .iter()
            .map(|c| {
                let mut j = json!({
                    "seq": c.seq,
                    "timestamp": c.timestamp(),
                    "kind": c.kind.name(),
                    "inserted": c.inserted,
                    "deleted": c.deleted,
                    "quads": c.quads,
                });
                // the counts cover every graph
                if restricted {
                    crate::http::redact_commit_json(&mut j);
                }
                j
            })
            .collect();
        // the past states `at` and `atCommit` can read beyond the pins of this server
        let readable: Vec<Value> = ds
            .dataset
            .history()
            .status()
            .reconstructable
            .iter()
            .map(|&(from, to)| json!({ "from": from, "to": to.min(head) }))
            .collect();
        let mut snapshots = ds.dataset.snapshots().list();
        snapshots.sort_by(|a, b| b.seq.cmp(&a.seq).then_with(|| a.name.cmp(&b.name)));
        let snapshots: Vec<Value> = snapshots
            .iter()
            .take(MAX_SNAPSHOTS_LISTED)
            .map(|s| json!({ "name": s.name, "commit": s.seq }))
            .collect();
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "head": head,
            "firstRetained": page.first_retained,
            "complete": page.complete,
            "commits": commits,
            "next": next,
            "readable": readable,
            "snapshots": snapshots,
        })))
    }
}

fn class_json(c: &ClassEntry, terms: &mut Terms) -> Value {
    let mut e = json!({
        "iri": terms.iri(&c.iri),
        "instances": c.observed.instances,
        "declared": c.declared.types.iter().map(|t| terms.iri(t)).collect::<Vec<_>>(),
    });
    if let Some(l) = render::choose(
        c.declared
            .labels
            .iter()
            .map(|l| (l.value.as_str(), l.lang.as_deref())),
        "en",
    ) {
        e["label"] = l.into();
    }
    if !c.declared.super_classes.is_empty() {
        e["superClasses"] = c
            .declared
            .super_classes
            .iter()
            .map(|t| terms.iri(t))
            .collect::<Vec<_>>()
            .into();
    }
    let exprs: Vec<String> = c
        .declared
        .super_class_expressions
        .iter()
        .map(|x| class_expression(x, terms))
        .collect();
    if !exprs.is_empty() {
        e["superClassExpressions"] = exprs.into();
    }
    e
}

/// A class expression (Manchester Syntax, IRIs in angle brackets) with its IRIs written
/// as the result's other IRIs are.
fn class_expression(x: &str, terms: &mut Terms) -> String {
    let mut out = String::with_capacity(x.len());
    let mut rest = x;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        match rest[i + 1..].find('>') {
            Some(j) if !rest[i + 1..i + 1 + j].contains([' ', '<']) => {
                out.push_str(&terms.iri(&rest[i + 1..i + 1 + j]));
                rest = &rest[i + 2 + j..];
            }
            _ => {
                out.push('<');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// One source of the constraints layer: one line per property shape.
fn constraints_json(
    src: &sparkles::schema::constraints::ConstraintSource,
    terms: &mut Terms,
) -> Value {
    let mut e = json!({
        "source": src.kind,
        "graphs": src.graphs.iter().map(|g| if g == "default" { g.clone() } else { terms.iri(g) }).collect::<Vec<_>>(),
        "classes": src.classes.iter().map(|c| {
            let mut ce = json!({
                "class": terms.iri(&c.class),
                "properties": c.properties.iter().map(|p| json!({
                    "path": terms.iri(&p.path),
                    "constraints": p.summary(|i| terms.iri(i)),
                    "enforcement": p.enforcement,
                })).collect::<Vec<_>>(),
            });
            if c.closed {
                ce["closed"] = true.into();
            }
            if c.other_paths > 0 {
                ce["otherPaths"] = c.other_paths.into();
            }
            ce
        }).collect::<Vec<_>>(),
    });
    if let Some(m) = &src.mode {
        e["mode"] = m.as_str().into();
    }
    if let Some(t) = &src.threshold {
        e["threshold"] = t.as_str().into();
    }
    if src.other_targets > 0 {
        e["otherTargets"] = src.other_targets.into();
    }
    e
}

fn predicate_json(p: &PredicateEntry, terms: &mut Terms) -> Value {
    let o = &p.observed;
    let mut kinds: Vec<(String, u64)> = Vec::new();
    if let Some(k) = &o.objects.iri {
        kinds.push(("iri".into(), k.triples));
    }
    if let Some(k) = &o.objects.blank {
        kinds.push(("blank".into(), k.triples));
    }
    if let Some(k) = &o.objects.triple_term {
        kinds.push(("triple".into(), k.triples));
    }
    let mut vector = false;
    for g in &o.objects.literals {
        vector |= sparkles::vector::is_datatype(&g.datatype);
        let mut name = terms.iri(&g.datatype);
        if let Some(langs) = g.languages.as_ref().filter(|l| !l.is_empty()) {
            let tags: Vec<String> = langs
                .iter()
                .map(|l| match &l.direction {
                    Some(d) => format!("{}--{d}", l.lang),
                    None => l.lang.clone(),
                })
                .collect();
            name = format!("{name}@{}", tags.join(","));
        }
        kinds.push((name, g.triples));
    }
    kinds.sort_by_key(|k| std::cmp::Reverse(k.1));
    let mut e = json!({
        "iri": terms.iri(&p.iri),
        "triples": o.triples,
        "distinctSubjects": o.distinct_subjects,
        "distinctObjects": o.distinct_objects,
        "maxPerSubject": o.max_per_subject,
        "objects": kinds.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>(),
    });
    if let Some(l) = render::choose(
        p.declared
            .labels
            .iter()
            .map(|l| (l.value.as_str(), l.lang.as_deref())),
        "en",
    ) {
        e["label"] = l.into();
    }
    if !p.declared.domains.is_empty() {
        e["domains"] = p
            .declared
            .domains
            .iter()
            .map(|t| terms.iri(t))
            .collect::<Vec<_>>()
            .into();
    }
    if !p.declared.ranges.is_empty() {
        e["ranges"] = p
            .declared
            .ranges
            .iter()
            .map(|t| terms.iri(t))
            .collect::<Vec<_>>()
            .into();
    }
    if vector {
        e["vector"] = true.into();
    }
    // the ten classes with the most triples, then the untyped subjects
    if let Some(classes) = &o.subject_classes {
        let mut top: Vec<_> = classes.iter().collect();
        top.sort_by(|a, b| {
            b.triples
                .cmp(&a.triples)
                .then_with(|| a.class.cmp(&b.class))
        });
        let mut list: Vec<String> = top
            .iter()
            .take(10)
            .map(|c| format!("{} {}", terms.iri(&c.class), c.triples))
            .collect();
        if top.len() > 10 {
            list.push(format!("+{} more classes", top.len() - 10));
        }
        if let Some(u) = o.untyped_subjects.filter(|u| u.triples > 0) {
            list.push(format!("untyped {}", u.triples));
        }
        e["subjectClasses"] = list.into();
    }
    e
}

/// `<operator> <description> est=<rows> [<columns>]`, two spaces per depth.
fn plan_lines(p: &PlanInfo, depth: usize, out: &mut String) {
    out.push_str(&"  ".repeat(depth));
    out.push_str(&p.operator);
    if !p.description.is_empty() {
        out.push(' ');
        out.push_str(&p.description.replace(['\n', '\r'], " "));
    }
    // a view restricted by grants sees no estimates (-1)
    if p.estimated_rows < 0.0 {
        out.push_str(" est=?");
    } else {
        out.push_str(&format!(" est={}", p.estimated_rows.round() as u64));
    }
    let cols: Vec<String> = p.columns.iter().map(|c| format!("?{c}")).collect();
    out.push_str(&format!(" [{}]\n", cols.join(" ")));
    for c in &p.children {
        plan_lines(c, depth + 1, out);
    }
}

/// The `no-limit` and `large-estimate` warnings of a query's plan.
pub(super) fn plan_warnings(
    plan: &PlanInfo,
    parsed: &Query,
    default_rows: usize,
) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let root = plan.estimated_rows;
    // a view restricted by grants sees no estimates (-1): any query without a LIMIT
    // may be large
    let hidden = root < 0.0;
    if matches!(query_type(parsed), "SELECT" | "CONSTRUCT")
        && !has_limit(pattern(parsed))
        && (hidden || root > 10_000.0)
    {
        let size = if hidden {
            "No LIMIT, and no estimate is available for this view".to_string()
        } else {
            format!("No LIMIT and about {} result rows estimated", approx(root))
        };
        out.push((
            "no-limit",
            format!(
                "{size}; sparql_query returns only the first {default_rows}. Add LIMIT or aggregate with COUNT/GROUP BY."
            ),
        ));
    }
    let largest = max_estimate(plan);
    if largest > 50_000_000.0 {
        out.push((
            "large-estimate",
            format!(
                "An intermediate result of about {} rows is estimated; the query may exceed the timeout or memory budget. Add selective patterns (a class, a constant) first.",
                approx(largest)
            ),
        ));
    }
    out
}

fn max_estimate(p: &PlanInfo) -> f64 {
    p.children
        .iter()
        .map(max_estimate)
        .fold(p.estimated_rows, f64::max)
}

/// A top-level LIMIT (below the projection modifiers).
fn has_limit(p: &GraphPattern) -> bool {
    match p {
        GraphPattern::Slice {
            length: Some(_), ..
        } => true,
        GraphPattern::Slice { inner, .. }
        | GraphPattern::Project { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner } => has_limit(inner),
        _ => false,
    }
}

pub(super) fn children(p: &GraphPattern) -> Vec<&GraphPattern> {
    use GraphPattern as G;
    match p {
        G::Join { left, right }
        | G::Lateral { left, right }
        | G::LeftJoin { left, right, .. }
        | G::Union { left, right }
        | G::Minus { left, right }
        | G::SemiJoin { left, right }
        | G::AntiJoin { left, right } => vec![left, right],
        G::Filter { inner, .. }
        | G::Graph { inner, .. }
        | G::Extend { inner, .. }
        | G::Assign { inner, .. }
        | G::Unfold { inner, .. }
        | G::OrderBy { inner, .. }
        | G::Project { inner, .. }
        | G::Distinct { inner }
        | G::Reduced { inner }
        | G::Slice { inner, .. }
        | G::Group { inner, .. } => vec![inner],
        _ => Vec::new(),
    }
}

/// Whether the pattern calls a remote SERVICE (a path search runs locally).
fn has_service(p: &GraphPattern) -> bool {
    matches!(p, GraphPattern::Service { name, .. } if !sparkles::sparql::pathsearch::is_search(name))
        || children(p).into_iter().any(has_service)
}

/// Constant IRIs and literals of the triple patterns outside SERVICE.
pub(super) fn constant_terms(p: &GraphPattern, out: &mut Vec<Term>) {
    let add = |t: &TermPattern, out: &mut Vec<Term>| match t {
        TermPattern::NamedNode(n) => out.push(Term::NamedNode(n.clone())),
        TermPattern::Literal(l) => out.push(Term::Literal(l.clone())),
        _ => {}
    };
    match p {
        GraphPattern::Bgp { patterns } => {
            for tp in patterns {
                add(&tp.subject, out);
                if let NamedNodePattern::NamedNode(n) = &tp.predicate {
                    out.push(Term::NamedNode(n.clone()));
                }
                add(&tp.object, out);
            }
        }
        GraphPattern::Path {
            subject,
            path,
            object,
        } => {
            add(subject, out);
            if let PropertyPathExpression::NamedNode(n) = path {
                out.push(Term::NamedNode(n.clone()));
            }
            add(object, out);
        }
        GraphPattern::Service { .. } => {}
        p => {
            for c in children(p) {
                constant_terms(c, out);
            }
        }
    }
}

/// The internal queries of describe_resource: one snapshot, one deadline. The resource
/// and predicates are passed as pre-bound variables, never spliced into query text.
struct Queries<'a> {
    snap: &'a Arc<Snapshot>,
    opts: QueryOptions,
    deadline: Instant,
    resource: Term,
}

/// One direction of describe_resource.
#[derive(Default)]
struct Side {
    total: u64,
    /// (predicate, count), count desc then IRI
    predicates: Vec<(NamedNode, u64)>,
    /// (predicate, the other end)
    triples: Vec<(NamedNode, Term)>,
}

impl Side {
    fn json(&self, triples: Vec<Value>, terms: &mut Terms) -> Value {
        let listed: Vec<Value> = self
            .predicates
            .iter()
            .take(50)
            .map(|(p, n)| json!({ "p": terms.iri(p.as_str()), "count": n }))
            .collect();
        json!({
            "total": self.total,
            "predicates": listed,
            "predicatesTotal": self.predicates.len(),
            "triples": triples,
            "truncated": (self.triples.len() as u64) < self.total,
        })
    }
}

impl Queries<'_> {
    fn select(&self, q: &str, extra: Vec<(String, Term)>) -> Result<Vec<Vec<Option<Term>>>, Error> {
        let mut opts = self.opts.clone();
        opts.timeout = Some(remaining(self.deadline)?);
        opts.initial_bindings = vec![("r".to_string(), self.resource.clone())];
        opts.initial_bindings.extend(extra);
        Ok(sparql::query(self.snap.clone(), q, &opts)?.rows())
    }

    fn ask(&self, outgoing: bool) -> Result<bool, Error> {
        let mut opts = self.opts.clone();
        opts.timeout = Some(remaining(self.deadline)?);
        opts.initial_bindings = vec![("r".to_string(), self.resource.clone())];
        let q = if outgoing {
            "ASK { ?r ?p ?o }"
        } else {
            "ASK { ?s ?p ?r }"
        };
        Ok(sparql::query(self.snap.clone(), q, &opts)?.boolean)
    }

    fn side(&self, outgoing: bool, max_triples: usize) -> Result<Side, Error> {
        let q = if outgoing {
            "SELECT ?p (COUNT(*) AS ?n) WHERE { ?r ?p ?o } GROUP BY ?p"
        } else {
            "SELECT ?p (COUNT(*) AS ?n) WHERE { ?s ?p ?r } GROUP BY ?p"
        };
        let mut predicates: Vec<(NamedNode, u64)> = self
            .select(q, Vec::new())?
            .into_iter()
            .filter_map(|row| {
                let Some(Term::NamedNode(p)) = row.first().cloned().flatten() else {
                    return None;
                };
                let n = match row.get(1).cloned().flatten() {
                    Some(Term::Literal(l)) => l.value().parse::<u64>().unwrap_or(0),
                    _ => 0,
                };
                Some((p, n))
            })
            .collect();
        predicates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.as_str().cmp(b.0.as_str())));
        let total = predicates.iter().map(|(_, n)| n).sum();
        // round-robin quotas: an even share per listed predicate, then fill in order
        let listed = &predicates[..predicates.len().min(50)];
        let share = (max_triples / listed.len().max(1)).max(1) as u64;
        let mut quota: Vec<u64> = Vec::with_capacity(listed.len());
        let mut left = max_triples as u64;
        for (_, n) in listed {
            let q = share.min(*n).min(left);
            quota.push(q);
            left -= q;
        }
        for (i, (_, n)) in listed.iter().enumerate() {
            let extra = left.min(n - quota[i]);
            quota[i] += extra;
            left -= extra;
        }
        let mut triples = Vec::new();
        for ((p, _), k) in listed.iter().zip(quota) {
            if k == 0 {
                continue;
            }
            let q = if outgoing {
                format!("SELECT ?x WHERE {{ ?r ?pp ?x }} LIMIT {k}")
            } else {
                format!("SELECT ?x WHERE {{ ?x ?pp ?r }} LIMIT {k}")
            };
            for row in self.select(&q, vec![("pp".to_string(), Term::NamedNode(p.clone()))])? {
                if let Some(t) = row.into_iter().next().flatten() {
                    triples.push((p.clone(), t));
                }
            }
        }
        Ok(Side {
            total,
            predicates,
            triples,
        })
    }

    fn types(&self) -> Result<Vec<Term>, Error> {
        Ok(self
            .select("SELECT ?t WHERE { ?r a ?t } LIMIT 20", Vec::new())?
            .into_iter()
            .filter_map(|r| r.into_iter().next().flatten())
            .collect())
    }

    /// Label candidates of IRIs, keyed by IRI.
    fn labels(&self, iris: &[&NamedNode]) -> Result<Labels, Error> {
        labels_of(self.snap, &self.opts, self.deadline, iris)
    }

    /// Label candidates of the resource itself (for a blank node).
    fn own_labels(&self) -> Result<Vec<(usize, Literal)>, Error> {
        let q = format!(
            "SELECT ?lp ?l WHERE {{ VALUES ?lp {{ {} }} ?r ?lp ?l }} LIMIT 1000",
            label_values()
        );
        Ok(self
            .select(&q, Vec::new())?
            .into_iter()
            .filter_map(|row| match row.as_slice() {
                [Some(p), Some(Term::Literal(l))] => label_rank(p).map(|r| (r, l.clone())),
                _ => None,
            })
            .collect())
    }
}

/// The label predicates as a `VALUES` list.
fn label_values() -> String {
    render::LABEL_PREDICATES
        .iter()
        .map(|p| format!("<{p}>"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn label_rank(p: &Term) -> Option<usize> {
    match p {
        Term::NamedNode(n) => render::LABEL_PREDICATES
            .iter()
            .position(|x| *x == n.as_str()),
        _ => None,
    }
}

/// Label candidates keyed by IRI (or blank node label): `(predicate rank, literal)`.
pub(super) type Labels = HashMap<String, Vec<(usize, Literal)>>;

/// The label candidates of `iris`, in one `VALUES` query.
pub(super) fn labels_of(
    snap: &Arc<Snapshot>,
    opts: &QueryOptions,
    deadline: Instant,
    iris: &[&NamedNode],
) -> Result<Labels, Error> {
    let mut out = Labels::new();
    let unique: BTreeSet<&str> = iris.iter().map(|n| n.as_str()).collect();
    if unique.is_empty() {
        return Ok(out);
    }
    // IRIs are serialized by oxrdf (validated, `<…>`), never copied from input text
    let values: Vec<String> = unique
        .iter()
        .map(|i| NamedNode::new_unchecked(*i).to_string())
        .collect();
    let q = format!(
        "SELECT ?x ?lp ?l WHERE {{ VALUES ?lp {{ {} }} VALUES ?x {{ {} }} ?x ?lp ?l }} LIMIT 10000",
        label_values(),
        values.join(" ")
    );
    let mut opts = opts.clone();
    opts.timeout = Some(remaining(deadline)?);
    opts.initial_bindings = Vec::new();
    for row in sparql::query(snap.clone(), &q, &opts)?.rows() {
        if let [Some(Term::NamedNode(x)), Some(p), Some(Term::Literal(l))] = row.as_slice()
            && let Some(rank) = label_rank(p)
        {
            out.entry(x.as_str().to_string())
                .or_default()
                .push((rank, l.clone()));
        }
    }
    Ok(out)
}

/// The chosen label of `t` (§ labels: first predicate with a label, preferred language).
pub(super) fn label_of(labels: &Labels, t: &Term, lang: &str) -> Option<String> {
    let key = match t {
        Term::NamedNode(n) => n.as_str().to_string(),
        Term::BlankNode(_) => t.to_string(),
        _ => return None,
    };
    let v = labels.get(&key)?;
    let refs: Vec<(usize, &Literal)> = v.iter().map(|(r, l)| (*r, l)).collect();
    render::choose_ranked(&refs, lang)
}
