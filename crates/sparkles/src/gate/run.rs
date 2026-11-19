//! The runner: each check of a suite on a candidate state, with inputs from the state the
//! change starts from.

use super::{Check, CheckKind, DataGraph, Query, Severity, Shapes, Suite};
use crate::store::Snapshot;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The most messages one result keeps.
pub const MAX_MESSAGES: usize = 20;

/// What a check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// it ran and found nothing
    Passed,
    /// it ran and found something
    Failed,
    /// it could not run: an input is missing or invalid, the build lacks the checker, or
    /// it timed out. An error never passes.
    Error,
}

impl Status {
    /// `passed`, `failed` or `error`.
    pub fn name(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Error => "error",
        }
    }
}

/// The result of one check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckResult {
    pub check: oxrdf::NamedNode,
    /// the check type's local name, such as `ShaclCheck`
    pub kind: &'static str,
    pub severity: Severity,
    pub status: Status,
    /// what it found: results, nonconformant nodes, findings or solutions
    pub findings: u64,
    /// why it could not run: `check-unavailable`, `input-missing`, `input-invalid`,
    /// `check-timeout` or `budget-exceeded`
    pub code: Option<&'static str>,
    /// at most [`MAX_MESSAGES`] messages
    pub messages: Vec<String>,
    /// what the check reports beside its findings, each with its own severity. A note
    /// never fails the suite. No check of this build reports one.
    pub notes: Vec<Note>,
    pub millis: u64,
}

/// Something a check reports beside its findings, with its own severity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub severity: Severity,
    /// a stable code
    pub code: &'static str,
    pub count: u64,
    pub message: String,
}

impl CheckResult {
    /// Whether the result fails its suite: a check of severity `sh:Violation` that did not
    /// pass.
    pub fn blocks(&self) -> bool {
        self.severity == Severity::Violation && self.status != Status::Passed
    }
}

/// The results of a suite on one candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuiteReport {
    pub suite: oxrdf::NamedNode,
    pub revision: String,
    pub results: Vec<CheckResult>,
}

impl SuiteReport {
    /// Whether no result blocks.
    pub fn passed(&self) -> bool {
        !self.results.iter().any(CheckResult::blocks)
    }

    /// The results that block.
    pub fn failed(&self) -> impl Iterator<Item = &CheckResult> {
        self.results.iter().filter(|r| r.blocks())
    }

    /// The report as JSON.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "suite": self.suite.as_str(),
            "revision": self.revision,
            "passed": self.passed(),
            "results": self.results.iter().map(|r| serde_json::json!({
                "check": r.check.as_str(),
                "type": r.kind,
                "severity": r.severity.name(),
                "status": r.status.name(),
                "blocks": r.blocks(),
                "findings": r.findings,
                "code": r.code,
                "messages": r.messages,
                "notes": r.notes.iter().map(|n| serde_json::json!({
                    "severity": n.severity.name(),
                    "code": n.code,
                    "count": n.count,
                    "message": n.message,
                })).collect::<Vec<_>>(),
                "millis": r.millis,
            })).collect::<Vec<_>>(),
        })
    }
}

/// What a suite runs against.
pub struct Inputs<'a> {
    /// the state the change starts from, which holds the shapes graphs
    pub before: &'a Arc<Snapshot>,
    /// the candidate state the checks judge
    pub after: &'a Arc<Snapshot>,
    /// the dataset's stored queries (`None`: a stored-query check cannot run)
    pub stored: Option<&'a crate::stored::Catalog>,
    /// the bound on each check, from its start (`None`: none). It is cooperative: a check
    /// that is still running when it passes ends at its next check of the deadline, and a
    /// check that ends after it never passes.
    pub timeout: Option<Duration>,
}

/// Run every check of `suite`, in its order. A check that cannot run is an
/// [`Status::Error`] result, never skipped.
pub fn run(suite: &Suite, inputs: &Inputs<'_>) -> SuiteReport {
    SuiteReport {
        suite: suite.iri().clone(),
        revision: suite.revision(),
        results: suite
            .checks()
            .iter()
            .map(|c| run_check(c, inputs))
            .collect(),
    }
}

/// What a checker answers: the findings, their messages and its notes, or why it could
/// not run.
type Outcome = Result<(u64, Vec<String>, Vec<Note>), (&'static str, String)>;

/// One check's bound: its deadline, and what remains of it for the checkers that take a
/// relative timeout.
#[derive(Clone, Copy)]
pub(super) struct Bound {
    deadline: Option<Instant>,
}

impl Bound {
    fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
    }

    fn passed(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }
}

/// Why `chk:UnsatisfiableClassesCheck` does not run in any build (roadmap item 1207).
pub(super) const UNSATISFIABLE_DEFERRED: &str = "chk:UnsatisfiableClassesCheck is deferred, \
     so no build runs it: the OWL reader cannot yet read a terminology with the guarantee \
     that no axiom is lost";

const TIMED_OUT: (&str, &str) = ("check-timeout", "the check did not finish within its bound");

fn run_check(c: &Check, inputs: &Inputs<'_>) -> CheckResult {
    let t0 = Instant::now();
    // the deferral comes before the bound, so no timeout can replace its code
    if matches!(c.kind, CheckKind::UnsatisfiableClasses { .. }) {
        let deferred = Err(("check-unavailable", UNSATISFIABLE_DEFERRED.to_string()));
        return result(c, deferred, t0);
    }
    let outcome = match inputs.timeout.map(|t| t0.checked_add(t)) {
        Some(None) => Err((
            "input-invalid",
            "the check's bound is too large".to_string(),
        )),
        Some(Some(deadline)) => dispatch(
            c,
            inputs,
            Bound {
                deadline: Some(deadline),
            },
        ),
        None => dispatch(c, inputs, Bound { deadline: None }),
    };
    result(c, outcome, t0)
}

fn dispatch(c: &Check, inputs: &Inputs<'_>, bound: Bound) -> Outcome {
    if bound.passed() {
        return Err((TIMED_OUT.0, TIMED_OUT.1.to_string()));
    }
    let outcome = match &c.kind {
        // (run_check answers it before its bound; this keeps dispatch total)
        CheckKind::UnsatisfiableClasses { .. } => {
            Err(("check-unavailable", UNSATISFIABLE_DEFERRED.to_string()))
        }
        kind if !kind.available() => Err((
            "check-unavailable",
            format!(
                "this build cannot run chk:{}: its cargo feature is not compiled in",
                kind.type_name()
            ),
        )),
        CheckKind::Shacl {
            shapes,
            data_graph,
            inferences,
        } => shacl(shapes, data_graph, *inferences, inputs, bound),
        CheckKind::Shex { schema, shape_map } => shex(schema, shape_map, inputs, bound),
        CheckKind::Diagnostics {
            diagnostics,
            inferences,
        } => diagnostics_check(diagnostics.clone(), *inferences, inputs, bound),
        CheckKind::Query(q) => query(q, inputs, bound),
    };
    // a check that ends after its deadline never passes, and its error is the timeout
    if bound.passed() && !matches!(outcome, Ok((n, _, _)) if n > 0) {
        return Err((TIMED_OUT.0, TIMED_OUT.1.to_string()));
    }
    outcome
}

fn result(c: &Check, outcome: Outcome, t0: Instant) -> CheckResult {
    let (status, findings, code, mut messages, notes) = match outcome {
        Ok((0, m, notes)) => (Status::Passed, 0, None, m, notes),
        Ok((n, m, notes)) => (Status::Failed, n, None, m, notes),
        Err((code, m)) => (Status::Error, 0, Some(code), vec![m], Vec::new()),
    };
    messages.truncate(MAX_MESSAGES);
    CheckResult {
        check: c.iri.clone(),
        kind: c.kind.type_name(),
        severity: c.severity,
        status,
        findings,
        code,
        messages,
        notes,
        millis: t0.elapsed().as_millis() as u64,
    }
}

/// A checker's error: a timeout, or an input it could not use.
fn failure(e: &anyhow::Error) -> (&'static str, String) {
    let timeout = e.chain().any(|c| {
        matches!(
            c.downcast_ref::<crate::error::Error>(),
            Some(crate::error::Error::Timeout)
        )
    });
    let budget = e.chain().any(|c| {
        matches!(
            c.downcast_ref::<crate::error::Error>(),
            Some(crate::error::Error::BudgetExceeded(_))
        )
    });
    let code = if timeout {
        "check-timeout"
    } else if budget {
        "budget-exceeded"
    } else {
        "input-invalid"
    };
    (code, format!("{e:#}"))
}

#[cfg(feature = "shacl")]
fn shacl(
    shapes: &Shapes,
    data_graph: &DataGraph,
    inferences: bool,
    inputs: &Inputs<'_>,
    bound: Bound,
) -> Outcome {
    use sparkles_shacl::{ShapesSyntax, ValidateOptions};
    let mut exclude = vec![super::RUNS_GRAPH.to_string()];
    let shapes = match shapes {
        Shapes::Graph(g) => {
            if !holds_data(inputs.before, g.as_str()).map_err(|e| failure(&e.into()))? {
                return Err((
                    "input-missing",
                    format!(
                        "the shapes graph <{}> is empty in the state the change starts from",
                        g.as_str()
                    ),
                ));
            }
            exclude.push(g.as_str().to_string());
            sparkles_shacl::Shapes::from_store(inputs.before, Some(g.as_str()))
                .map_err(|e| failure(&e))?
        }
        Shapes::Inline(text) => {
            sparkles_shacl::Shapes::parse(text, ShapesSyntax::Rdf(oxrdfio::RdfFormat::Turtle), None)
                .map_err(|e| failure(&e))?
        }
    };
    // a shape that `sh:deactivated true` turns off selects no focus node
    if !shapes.targeted().any(|shape| !shape.deactivated) {
        return Err((
            "input-invalid",
            "the shapes have no active shape with a target, so the check would validate nothing"
                .to_string(),
        ));
    }
    if let DataGraph::Graphs(gs) = data_graph {
        for g in gs {
            if !holds_data(inputs.after, g).map_err(|e| failure(&e.into()))? {
                return Err((
                    "input-missing",
                    format!("the data graph <{g}> holds no data in the candidate"),
                ));
            }
        }
    }
    let Some(graphs) = data_graph
        .selection()
        .graphs(inputs.after, inferences, &exclude)
    else {
        return Err((
            "input-missing",
            "no data graph of the check holds data in the candidate".to_string(),
        ));
    };
    if bound.passed() {
        return Err((TIMED_OUT.0, TIMED_OUT.1.to_string()));
    }
    let opts = ValidateOptions {
        data_graph: graphs.data_graph,
        extra_graphs: graphs.extra_graphs,
        exclude_graphs: graphs.exclude_graphs,
        timeout: bound.remaining(),
        deadline: bound.deadline,
        ..Default::default()
    };
    let report = sparkles_shacl::validate(inputs.after, &shapes, &opts).map_err(|e| failure(&e))?;
    if report.conforms {
        return Ok((0, Vec::new(), Vec::new()));
    }
    let messages = report
        .results
        .iter()
        .take(MAX_MESSAGES)
        .map(|r| {
            let sev = r.severity.as_str().rsplit('#').next().unwrap_or_default();
            match r.message() {
                Some(m) => format!("{sev} at {}: {m}", r.focus_node),
                None => format!(
                    "{sev} at {}: {}",
                    r.focus_node,
                    r.source_constraint_component.as_str()
                ),
            }
        })
        .collect();
    Ok((report.results.len().max(1) as u64, messages, Vec::new()))
}

#[cfg(not(feature = "shacl"))]
fn shacl(_: &Shapes, _: &DataGraph, _: bool, _: &Inputs<'_>, _: Bound) -> Outcome {
    unreachable!("an unavailable check does not run")
}

#[cfg(feature = "shex")]
fn shex(schema: &str, map: &str, inputs: &Inputs<'_>, bound: Bound) -> Outcome {
    let invalid = |what: &str, e: &dyn std::fmt::Display| ("input-invalid", format!("{what}: {e}"));
    let parsed =
        sparkles_shex::Schema::parse_shexc(schema, None).map_err(|e| invalid("the schema", &e))?;
    let compiled = sparkles_shex::compile(&parsed, &sparkles_shex::NoImports)
        .map_err(|e| invalid("the schema", &e))?;
    let map = sparkles_shex::ShapeMap::parse(map, compiled.prefixes(), compiled.base())
        .map_err(|e| invalid("the shape map", &e))?;
    if bound.passed() {
        return Err((TIMED_OUT.0, TIMED_OUT.1.to_string()));
    }
    let opts = sparkles_shex::ValidateOptions {
        timeout: bound.remaining(),
        ..Default::default()
    };
    let results =
        sparkles_shex::validate(inputs.after, &compiled, &map, &opts).map_err(|e| failure(&e))?;
    if results.conforms {
        return Ok((0, Vec::new(), Vec::new()));
    }
    let messages = results
        .results
        .iter()
        .filter(|r| r.status == sparkles_shex::Status::Nonconformant)
        .take(MAX_MESSAGES)
        .map(|r| {
            format!(
                "{} does not conform to {}: {}",
                r.node,
                sparkles_shex::report::label_str(&r.shape),
                r.reason.as_deref().unwrap_or("nonconformant")
            )
        })
        .collect();
    Ok((results.nonconformant.max(1) as u64, messages, Vec::new()))
}

#[cfg(not(feature = "shex"))]
fn shex(_: &str, _: &str, _: &Inputs<'_>, _: Bound) -> Outcome {
    unreachable!("an unavailable check does not run")
}

#[cfg(feature = "reasoning")]
fn diagnostics_check(
    checks: Vec<String>,
    inferences: bool,
    inputs: &Inputs<'_>,
    bound: Bound,
) -> Outcome {
    use sparkles_reasoner::diagnostics::{CheckStatus, DiagnoseOptions, diagnose};
    let opts = DiagnoseOptions {
        checks,
        inferences,
        timeout: bound.remaining(),
        ..Default::default()
    };
    let report = diagnose(inputs.after.clone(), &opts).map_err(|e| failure(&e))?;
    for o in &report.checks {
        match o.status {
            CheckStatus::Timeout => {
                return Err(("check-timeout", format!("diagnostic {} timed out", o.id)));
            }
            CheckStatus::Error => {
                return Err((
                    "input-invalid",
                    format!(
                        "diagnostic {}: {}",
                        o.id,
                        o.error.as_deref().unwrap_or("failed")
                    ),
                ));
            }
            CheckStatus::Violations | CheckStatus::None | CheckStatus::Truncated => {}
        }
    }
    let total: usize = report.checks.iter().map(|o| o.findings).sum();
    let truncated = report
        .checks
        .iter()
        .any(|o| o.status == CheckStatus::Truncated);
    let messages = report
        .findings
        .iter()
        .take(MAX_MESSAGES)
        .map(|f| format!("{}: {}", f.check, f.message))
        .collect();
    Ok((
        (total.max(usize::from(truncated))) as u64,
        messages,
        Vec::new(),
    ))
}

#[cfg(not(feature = "reasoning"))]
fn diagnostics_check(_: Vec<String>, _: bool, _: &Inputs<'_>, _: Bound) -> Outcome {
    unreachable!("an unavailable check does not run")
}

fn query(q: &Query, inputs: &Inputs<'_>, bound: Bound) -> Outcome {
    let text = match q {
        Query::Inline(t) => t.clone(),
        Query::Stored {
            name,
            version,
            digest,
        } => {
            let Some(digest) = digest else {
                return Err((
                    "input-invalid",
                    format!(
                        "the stored query {name:?} is not pinned by chk:storedQueryDigest, so its text could change under the same version"
                    ),
                ));
            };
            let catalog = inputs.stored.ok_or_else(|| {
                (
                    "input-missing",
                    "the dataset's stored queries are not available".to_string(),
                )
            })?;
            let s = catalog.get(name, Some(*version)).ok_or_else(|| {
                (
                    "input-missing",
                    format!("no version {version} of the stored query {name:?}"),
                )
            })?;
            if !s.definition.parameters.is_empty() {
                return Err((
                    "input-invalid",
                    format!(
                        "the stored query {name:?} takes parameters, which a check cannot give"
                    ),
                ));
            }
            let found = sparkles_core::managed::settings_digest(s.definition.query.as_bytes());
            if *digest != found {
                return Err((
                    "input-invalid",
                    format!(
                        "version {version} of the stored query {name:?} is not the query the suite pinned"
                    ),
                ));
            }
            s.definition.query
        }
    };
    let opts = crate::sparql::QueryOptions {
        timeout: bound.remaining(),
        deadline: bound.deadline,
        ..Default::default()
    };
    let r = crate::sparql::query(inputs.after.clone(), &text, &opts)
        .map_err(|e| failure(&anyhow::Error::new(e)))?;
    use crate::sparql::QueryKind;
    let n = match r.kind {
        QueryKind::Ask => u64::from(r.boolean),
        _ => r.len() as u64,
    };
    let messages = match (n, r.kind) {
        (0, _) => Vec::new(),
        (_, QueryKind::Ask) => vec!["the query answered true".to_string()],
        (n, QueryKind::Select) => vec![format!("the query returned {n} solutions")],
        (n, _) => vec![format!("the query returned {n} triples")],
    };
    Ok((n, messages, Vec::new()))
}

/// Whether the graph `iri` holds a quad in `snap`: the named graph of that IRI, or the
/// default graph for `urn:x-arq:DefaultGraph`.
#[cfg(feature = "shacl")]
fn holds_data(snap: &Arc<Snapshot>, iri: &str) -> sparkles_core::Result<bool> {
    let id = if iri == sparkles_core::sparql::ctx::DEFAULT_GRAPH_IRI {
        crate::id::Id::DEFAULT_GRAPH
    } else {
        match snap.lookup_iri(iri) {
            Some(id) => id,
            None => return Ok(false),
        }
    };
    Ok(snap.count(crate::index::Perm::Gspo, &[id.0])? > 0)
}
