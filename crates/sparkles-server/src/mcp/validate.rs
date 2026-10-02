//! The validation tools: `validate_shacl` (a SHACL shapes graph in Turtle or SHACLC) and
//! `validate_shex` (a ShEx schema with a shape map). Both validate one snapshot of a
//! dataset's data graph, as `/{ds}/shacl` and `/{ds}/shex` do, and answer counts plus
//! the first `maxResults` results in compact terms. Neither writes, fetches imports or
//! runs SERVICE (not in SHACL-SPARQL constraints, not in ShEx `SPARQL` selectors).

use super::Outcome;
use super::errors::{ErrorContext, ToolError, secs};
use super::render::{Prefixes, Terms};
use super::tools::{Tools, bounded, dataset_prefixes, parse, parse_iri, remaining};
use crate::http::INFERRED_GRAPH;
use crate::state::Dataset;
use crate::validation_common::{self, GraphParam, ValidationInputs};
use oxrdf::Term;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::error::Error;
use sparkles::store::Snapshot;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Largest shapes graph or schema, in characters.
const MAX_SCHEMA_CHARS: usize = 1 << 20;
/// Largest shape map, in characters.
#[cfg(feature = "shex")]
const MAX_MAP_CHARS: usize = 65536;
/// Results of a call that does not set `maxResults` (capped by the server's `maxRows`).
const DEFAULT_MAX_RESULTS: usize = 20;
/// Terms and reasons in results are cut to this many characters.
const TERM_CHARS: usize = 500;

const VALIDATION_HINT: &str = "validate one named graph (graph), fewer focus nodes (narrower targets or shape map), or simpler shapes";

/// What both tools share: the snapshot, the data graph and the call's deadline.
struct Target {
    ds: Arc<Dataset>,
    snap: Arc<Snapshot>,
    inputs: ValidationInputs,
    reasoning: bool,
    deadline: Instant,
    timeout: Duration,
    prefixes: BTreeMap<String, String>,
}

/// Fail on an empty or oversized text argument.
fn text_arg(name: &str, s: &str, max: usize) -> Result<(), ToolError> {
    if s.trim().is_empty() {
        return Err(ToolError::bad_argument(format!("{name} must not be empty")));
    }
    if s.chars().count() > max {
        return Err(ToolError::bad_argument(format!(
            "{name} must be at most {max} characters"
        )));
    }
    Ok(())
}

/// The results that fit both `max_results` and `max_bytes` (of compact JSON), and
/// whether any were left out.
fn bounded_results(
    items: impl Iterator<Item = Value>,
    total: usize,
    max_results: usize,
    max_bytes: usize,
) -> (Vec<Value>, bool) {
    let mut out = Vec::new();
    let mut bytes = 0;
    for v in items.take(max_results) {
        let n = v.to_string().len() + 1;
        if bytes + n > max_bytes && !out.is_empty() {
            break;
        }
        bytes += n;
        out.push(v);
    }
    let truncated = out.len() < total;
    (out, truncated)
}

impl Tools<'_> {
    /// Resolve the dataset, snapshot, data graph and deadline of a validation call.
    fn target(
        &self,
        dataset: Option<&str>,
        graph: Option<&str>,
        reasoning: Option<bool>,
        timeout_seconds: Option<f64>,
        at_commit: Option<u64>,
    ) -> Result<Target, ToolError> {
        let timeout = self.timeout(timeout_seconds)?;
        let ds = self.dataset(dataset)?;
        // validation reads every graph a shape reaches, and its report quotes them
        if self.call.principal.restricted(&ds.name) {
            return Err(ToolError::new(
                "forbidden",
                403,
                format!(
                    "validation covers every graph of /{}, and your access is limited to some graphs",
                    ds.name
                ),
            ));
        }
        let prefixes = dataset_prefixes(&ds);
        let graph = match graph.map(str::trim) {
            None | Some("default") => GraphParam::Default,
            Some("union") => GraphParam::Union,
            Some(g) => match parse_iri(g, &prefixes, false)? {
                Term::NamedNode(n) => GraphParam::parse(n.as_str())
                    .map_err(|e| ToolError::bad_argument(format!("{e:#}")))?,
                _ => {
                    return Err(ToolError::bad_argument(
                        "graph must be default, union or a graph IRI",
                    ));
                }
            },
        };
        let snap = self.server.shared.pins.resolve(&ds, at_commit)?;
        if let GraphParam::Named(iri) = &graph
            && !validation_common::graph_exists(&snap, iri)
        {
            return Err(
                ToolError::new("unknown-graph", 404, format!("no graph <{iri}>"))
                    .hint("graph=union covers all graphs"),
            );
        }
        let reasoning = Self::reasoning(&ds, reasoning);
        let has_inferred = ds.reasoning.read().is_some();
        let inferred = has_inferred.then_some(INFERRED_GRAPH);
        let inputs = validation_common::inputs(&snap, &graph, inferred, reasoning)
            .map_err(|e| ToolError::bad_argument(format!("{e:#}")))?;
        Ok(Target {
            ds,
            snap,
            inputs,
            reasoning,
            deadline: self.call.arrived + timeout,
            timeout,
            prefixes,
        })
    }

    /// `maxResults`: 20 by default, at most the server's `maxRows`.
    fn max_results(&self, arg: Option<u64>) -> Result<usize, ToolError> {
        let max = self.cfg().max_rows as u64;
        Ok(bounded(
            "maxResults",
            arg,
            (DEFAULT_MAX_RESULTS as u64).min(max),
            1,
            max,
        )? as usize)
    }

    /// An error of a validator (an `anyhow::Error` around the engine's errors).
    fn validation_error(
        &self,
        e: anyhow::Error,
        timeout: Duration,
        ctx: &ErrorContext,
    ) -> ToolError {
        let timed_out = || {
            ToolError::new(
                "timeout",
                408,
                format!(
                    "validation exceeded the {} s timeout",
                    secs(timeout.as_secs_f64())
                ),
            )
            .hint(format!(
                "{VALIDATION_HINT}, or raise timeoutSeconds (max {})",
                secs(self.cfg().max_timeout.as_secs_f64())
            ))
        };
        #[cfg(feature = "shacl")]
        if let Some(r) = e.downcast_ref::<sparkles_shacl::TooManyResults>() {
            return too_many(r.limit);
        }
        #[cfg(feature = "shex")]
        if let Some(r) = e.downcast_ref::<sparkles_shex::TooManyResults>() {
            return too_many(r.limit);
        }
        #[cfg(feature = "shex")]
        if let Some(s) = e.downcast_ref::<sparkles_shex::SchemaError>() {
            return ToolError::new("invalid-schema", 400, s.message.clone());
        }
        let msg = format!("{e:#}");
        match e.downcast::<Error>() {
            Ok(Error::Timeout) => timed_out(),
            Ok(e @ Error::BudgetExceeded(_)) => {
                let mut t = ctx.engine(e);
                t.hint = Some(VALIDATION_HINT.to_string());
                t
            }
            Ok(e) => ctx.engine(e),
            Err(_) if msg.contains("timed out") => timed_out(),
            Err(_) if msg.contains("cancelled") => ctx.engine(Error::Cancelled),
            Err(_) => ToolError::new("invalid-shapes", 400, msg),
        }
    }
}

/// A report over the memory budget's result count.
fn too_many(limit: usize) -> ToolError {
    let mut t = ToolError::new(
        "budget-memory",
        507,
        format!("the validation report would hold more than {limit} results (memory budget)"),
    )
    .hint(VALIDATION_HINT);
    t.budget = Some("memory");
    t
}

// ------------------------------------------------------------------ SHACL ------

#[cfg(feature = "shacl")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ShaclArgs {
    dataset: Option<String>,
    shapes: String,
    shapes_format: Option<ShapesFormat>,
    graph: Option<String>,
    reasoning: Option<bool>,
    max_results: Option<u64>,
    timeout_seconds: Option<f64>,
    at_commit: Option<u64>,
}

/// The syntax of `validate_shacl`'s shapes.
#[cfg(feature = "shacl")]
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ShapesFormat {
    Turtle,
    Shaclc,
}

/// The rank of a SHACL severity (2 violation, 1 warning, 0 info) and its short name.
/// SHACL 1.2's `sh:Debug` and `sh:Trace` count as info; any other IRI as a violation.
#[cfg(feature = "shacl")]
fn severity(iri: &str) -> (u8, Option<&'static str>) {
    match iri.strip_prefix(sparkles_shacl::vocab::SH_NS) {
        Some("Violation") => (2, Some("Violation")),
        Some("Warning") => (1, Some("Warning")),
        Some("Info") => (0, Some("Info")),
        Some("Debug") => (0, Some("Debug")),
        Some("Trace") => (0, Some("Trace")),
        _ => (2, None),
    }
}

#[cfg(feature = "shacl")]
fn shacl_result(r: &sparkles_shacl::ValidationResult, terms: &mut Terms) -> Value {
    use sparkles_shacl::PropertyPath;
    let mut o = json!({ "focus": terms.term(&r.focus_node) });
    match &r.result_path {
        Some(PropertyPath::Predicate(p)) => o["path"] = terms.iri(p.as_str()).into(),
        Some(p) => o["path"] = cut(p.to_string()).into(),
        None => {}
    }
    if let Some(v) = &r.value {
        o["value"] = terms.term(v).into();
    }
    o["shape"] = terms.term(&r.source_shape).into();
    o["constraint"] = terms.iri(r.source_constraint_component.as_str()).into();
    o["severity"] = match severity(r.severity.as_str()) {
        (_, Some(s)) => s.into(),
        (_, None) => terms.iri(r.severity.as_str()).into(),
    };
    if let Some(m) = r.message() {
        o["message"] = cut(m.to_string()).into();
    }
    o
}

/// `s` cut to [`TERM_CHARS`] characters, the cut marked.
fn cut(s: String) -> String {
    match s.char_indices().nth(TERM_CHARS) {
        None => s,
        Some((i, _)) => format!("{}…(+{} chars)", &s[..i], s[i..].chars().count()),
    }
}

#[cfg(feature = "shacl")]
impl Tools<'_> {
    pub(super) fn validate_shacl(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: ShaclArgs = parse(args)?;
        text_arg("shapes", &a.shapes, MAX_SCHEMA_CHARS)?;
        let max_results = self.max_results(a.max_results)?;
        let t = self.target(
            a.dataset.as_deref(),
            a.graph.as_deref(),
            a.reasoning,
            a.timeout_seconds,
            a.at_commit,
        )?;
        let prefixes = Prefixes::new(&t.prefixes);
        let names = prefixes.names();
        let ctx = self.ctx(&names, t.timeout.as_secs_f64());
        let syntax = match a.shapes_format.unwrap_or(ShapesFormat::Turtle) {
            ShapesFormat::Turtle => sparkles_shacl::ShapesSyntax::default(),
            ShapesFormat::Shaclc => sparkles_shacl::ShapesSyntax::Compact,
        };
        let shapes = sparkles_shacl::Shapes::parse(&a.shapes, syntax, None)
            .map_err(|e| ToolError::new("syntax", 400, format!("shapes: {e:#}")))?;
        let opts = sparkles_shacl::ValidateOptions {
            data_graph: t.inputs.data_graph.clone(),
            extra_graphs: t.inputs.extra_graphs.clone(),
            exclude_graphs: t.inputs.exclude_graphs.clone(),
            pool: validation_common::validation_pool(),
            timeout: Some(
                remaining(t.deadline)
                    .map_err(|e| self.validation_error(e.into(), t.timeout, &ctx))?,
            ),
            cancel: Some(self.call.cancel.clone()),
            max_results: validation_common::max_results(&self.server.state.limits),
            ..Default::default()
        };
        let report = sparkles_shacl::validate(&t.snap, &shapes, &opts)
            .map_err(|e| self.validation_error(e, t.timeout, &ctx))?;
        // most severe first, then by shape and focus node (as write-time validation)
        let mut ranked: Vec<(u8, &sparkles_shacl::ValidationResult)> = report
            .results
            .iter()
            .map(|r| (severity(r.severity.as_str()).0, r))
            .collect();
        ranked.sort_by(|(a, x), (b, y)| {
            b.cmp(a)
                .then_with(|| x.source_shape.to_string().cmp(&y.source_shape.to_string()))
                .then_with(|| x.focus_node.to_string().cmp(&y.focus_node.to_string()))
        });
        let count = |rank| ranked.iter().filter(|(s, _)| *s == rank).count();
        let by_severity = json!({"violation": count(2), "warning": count(1), "info": count(0)});
        let mut terms = Terms::new(&prefixes, TERM_CHARS);
        let (results, truncated) = bounded_results(
            ranked.iter().map(|(_, r)| shacl_result(r, &mut terms)),
            ranked.len(),
            max_results,
            self.cfg().max_bytes,
        );
        Ok(Outcome::Structured(json!({
            "dataset": t.ds.name,
            "commit": t.snap.commit,
            "reasoning": t.reasoning,
            "conforms": report.conforms,
            "total": ranked.len(),
            "bySeverity": by_severity,
            "results": results,
            "truncated": truncated,
            "prefixes": terms.used(),
        })))
    }
}

// ------------------------------------------------------------------- ShEx ------

#[cfg(feature = "shex")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ShexArgs {
    dataset: Option<String>,
    schema: String,
    shape_map: String,
    graph: Option<String>,
    reasoning: Option<bool>,
    only_nonconformant: Option<bool>,
    max_results: Option<u64>,
    timeout_seconds: Option<f64>,
    at_commit: Option<u64>,
}

/// A syntax error with its line and column.
#[cfg(feature = "shex")]
fn syntax(what: &str, e: &sparkles_shex::ParseError) -> ToolError {
    ToolError::new(
        "syntax",
        400,
        format!(
            "{what} syntax error at line {}, column {}: {}",
            e.line, e.column, e.message
        ),
    )
}

#[cfg(feature = "shex")]
fn shape_label(l: &sparkles_shex::ShapeLabel, terms: &mut Terms) -> String {
    use sparkles_shex::ShapeLabel;
    match l {
        ShapeLabel::Iri(i) => terms.iri(i),
        ShapeLabel::BNode(b) => format!("_:{b}"),
        ShapeLabel::Start => "START".to_string(),
    }
}

/// One failure in compact terms: the `kind` and fields of the JSON report, with the
/// value as a term and the predicate as a (prefixed) IRI.
#[cfg(feature = "shex")]
fn shex_failure(f: &sparkles_shex::ShexFailure, terms: &mut Terms) -> Value {
    use sparkles_shex::ShexFailure as F;
    let mut v = serde_json::to_value(f).unwrap_or(Value::Null);
    let value = match f {
        F::NodeKind { value, .. }
        | F::Datatype { value, .. }
        | F::Facet { value, .. }
        | F::ValueSet { value, .. }
        | F::Closed { value, .. }
        | F::Extra { value, .. }
        | F::Reference { value, .. } => Some(value),
        _ => None,
    };
    if let Some(t) = value {
        v["value"] = terms.term(t).into();
    }
    if let Some(p) = v
        .get("predicate")
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        v["predicate"] = terms.iri(&p).into();
    }
    v
}

#[cfg(feature = "shex")]
fn shex_result(r: &sparkles_shex::ShapeResult, terms: &mut Terms) -> Value {
    let mut o = json!({
        "node": terms.term(&r.node),
        "shape": shape_label(&r.shape, terms),
        "status": match r.status {
            sparkles_shex::Status::Conformant => "conformant",
            sparkles_shex::Status::Nonconformant => "nonconformant",
        },
    });
    if let Some(reason) = &r.reason {
        o["reason"] = cut(reason.clone()).into();
    }
    if !r.failures.is_empty() {
        o["failures"] = r.failures.iter().map(|f| shex_failure(f, terms)).collect();
    }
    o
}

#[cfg(feature = "shex")]
impl Tools<'_> {
    pub(super) fn validate_shex(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        use sparkles_shex::ShapeMap;
        let a: ShexArgs = parse(args)?;
        text_arg("schema", &a.schema, MAX_SCHEMA_CHARS)?;
        text_arg("shapeMap", &a.shape_map, MAX_MAP_CHARS)?;
        let max_results = self.max_results(a.max_results)?;
        let only_nonconformant = a.only_nonconformant.unwrap_or(true);
        let t = self.target(
            a.dataset.as_deref(),
            a.graph.as_deref(),
            a.reasoning,
            a.timeout_seconds,
            a.at_commit,
        )?;
        let names: Vec<String> = t.prefixes.keys().cloned().collect();
        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let ctx = self.ctx(&name_refs, t.timeout.as_secs_f64());
        let schema =
            sparkles_shex::parse_schema(&a.schema, None, None).map_err(|e| syntax("schema", &e))?;
        if let Some(iri) = schema.imports.first() {
            return Err(ToolError::bad_argument(format!(
                "validate_shex does not resolve imports (IMPORT <{iri}>)"
            ))
            .hint("put the imported shapes into the schema itself"));
        }
        let compiled = sparkles_shex::compile(&schema, &sparkles_shex::NoImports)
            .map_err(|e| ToolError::new("invalid-schema", 400, e.message))?;
        // the map may use the schema's prefixes, then the dataset's
        let mut map_prefixes = compiled.prefixes().clone();
        for (k, v) in &t.prefixes {
            if !map_prefixes.iter().any(|(p, _)| p == k) {
                map_prefixes.push((k.clone(), v.clone()));
            }
        }
        let map = ShapeMap::parse(&a.shape_map, &map_prefixes, compiled.base())
            .map_err(|e| syntax("shape map", &e))?;
        // the result terms use the dataset's prefixes, then the schema's
        let mut result_prefixes = t.prefixes.clone();
        for (k, v) in compiled.prefixes() {
            result_prefixes
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        let prefixes = Prefixes::new(&result_prefixes);
        let limits = &self.server.state.limits;
        // the typing's pairs within the memory budget, at an estimated 64 bytes each
        let max_pairs = limits
            .query_memory_bytes
            .map_or(sparkles_shex::DEFAULT_MAX_PAIRS, |m| {
                usize::try_from(m / 64)
                    .unwrap_or(usize::MAX)
                    .min(sparkles_shex::DEFAULT_MAX_PAIRS)
            });
        let timeout =
            remaining(t.deadline).map_err(|e| self.validation_error(e.into(), t.timeout, &ctx))?;
        let opts = sparkles_shex::ValidateOptions {
            data_graph: t.inputs.data_graph.clone(),
            extra_graphs: t.inputs.extra_graphs.clone(),
            exclude_graphs: t.inputs.exclude_graphs.clone(),
            pool: validation_common::validation_pool(),
            timeout: Some(timeout),
            cancel: Some(self.call.cancel.clone()),
            max_results: validation_common::max_results(limits),
            max_pairs: Some(max_pairs),
            only_nonconformant,
            // SPARQL selectors: the call's row and memory budgets, never SERVICE (their
            // queries take no prefixes but their own)
            selector_query: Some(sparkles::sparql::QueryOptions {
                max_rows: Some(limits.max_rows),
                max_memory_bytes: self.cfg().query_memory_bytes,
                max_rows_produced: limits.max_rows_produced,
                forbid_service: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let results = sparkles_shex::validate(&t.snap, &compiled, &map, &opts)
            .map_err(|e| self.validation_error(e, t.timeout, &ctx))?;
        let mut terms = Terms::new(&prefixes, TERM_CHARS);
        let (out, truncated) = bounded_results(
            results.results.iter().map(|r| shex_result(r, &mut terms)),
            results.results.len(),
            max_results,
            self.cfg().max_bytes,
        );
        Ok(Outcome::Structured(json!({
            "dataset": t.ds.name,
            "commit": t.snap.commit,
            "reasoning": t.reasoning,
            "conforms": results.conforms,
            "counts": {
                "conformant": results.conformant,
                "nonconformant": results.nonconformant,
            },
            "results": out,
            "truncated": truncated,
            "warnings": results.warnings,
            "prefixes": terms.used(),
        })))
    }
}
