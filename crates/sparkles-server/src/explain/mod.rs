//! Explaining a query (spec C18 §6.6): the notes on a plan's operators and the template
//! description of what the query asks, computed from the plan alone, and the checks on
//! the prose that the `explain` role writes from them.
//!
//! Everything here reads a plan as JSON, in the form `/{ds}/explain`, the
//! `application/x-sparkles+json` result format and its error body write it: a
//! `PlanNode` tree, or a streaming `CursorPlan` tree. Each node is named by its id, the
//! path of child indexes from the root (`0`, `0.1`, `0.1.2`), which this module derives
//! from the tree's shape. The explanation never reads result rows.

pub mod http;
#[cfg(test)]
mod http_tests;
pub mod model;
#[cfg(test)]
mod tests;

use serde::Serialize;
use serde_json::{Value, json};

/// The most nodes a plan given to `/{ds}/sparql/explain` may have.
pub const MAX_NODES: usize = 10_000;
/// The deepest a given plan may nest.
const MAX_DEPTH: usize = 256;
/// The most notes shown before **More** (§6.6.2). The rest follow in order.
pub const SHOWN_NOTES: usize = 12;

/// One operator of a plan, with the numbers the notes and the model may cite.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeFacts {
    pub id: String,
    pub operator: String,
    /// the planner's description without the runtime notes in brackets
    pub description: String,
    pub columns: Vec<String>,
    /// `None` when the caller's view hides estimates (`-1`)
    pub estimated_rows: Option<f64>,
    pub estimated_cost: Option<f64>,
    /// `None` when the node did not run or the plan was not executed
    pub actual_rows: Option<i64>,
    /// inclusive of its children
    pub time_ms: f64,
    /// its own time: its time less its children's
    pub self_ms: f64,
    /// whether its counts cover its whole run (false when a budget or a `LIMIT`
    /// stopped it)
    pub complete: bool,
    /// its counts are partial because a failure stopped the query
    pub partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stopped_early: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pushed_filters: Vec<String>,
    #[serde(skip)]
    pub children: Vec<usize>,
    #[serde(skip)]
    pub parent: Option<usize>,
    /// the description as the plan gave it, runtime notes included
    #[serde(skip)]
    pub full_description: String,
}

impl NodeFacts {
    /// The rows of its inputs, when every child ran.
    fn input_rows(&self, nodes: &[NodeFacts]) -> Option<i64> {
        if self.children.is_empty() {
            return None;
        }
        self.children
            .iter()
            .map(|&c| nodes[c].actual_rows)
            .try_fold(0i64, |a, r| r.map(|r| a.saturating_add(r)))
    }

    /// The operator's name in a sentence, such as "The regex filter" or "HashJoin on
    /// ?team".
    pub fn label(&self) -> String {
        let d = short(&self.description, 48);
        if d.is_empty() {
            self.operator.clone()
        } else {
            format!("{} {d}", self.operator)
        }
    }
}

/// A plan as facts, in tree order.
#[derive(Clone, Debug)]
pub struct Plan {
    pub nodes: Vec<NodeFacts>,
    /// whether any node ran (actual rows or time)
    pub executed: bool,
    /// whether the caller's view hides estimates
    pub hidden: bool,
    /// the planner's warnings of the root
    pub warnings: Vec<(String, String)>,
}

impl Plan {
    /// Read a `PlanNode` or `CursorPlan` tree. A given plan is data: anything that is
    /// not a plan is refused with the reason, and text is never interpreted.
    pub fn read(v: &Value) -> Result<Plan, String> {
        let mut nodes = Vec::new();
        read_node(v, "0".to_string(), None, 0, &mut nodes)?;
        let executed = nodes
            .iter()
            .any(|n| n.actual_rows.is_some() || n.time_ms > 0.0);
        let hidden = nodes.iter().all(|n| n.estimated_rows.is_none());
        // a streamed plan keeps its warnings on the root's operator
        let w = if v.get("materializes").is_some() {
            &v["operator"]["warnings"]
        } else {
            &v["warnings"]
        };
        let warnings = w
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| {
                Some((
                    w["code"].as_str()?.to_string(),
                    w["message"].as_str()?.to_string(),
                ))
            })
            .collect();
        Ok(Plan {
            nodes,
            executed,
            hidden,
            warnings,
        })
    }

    pub fn get(&self, id: &str) -> Option<&NodeFacts> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn has(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    fn total_ms(&self) -> f64 {
        self.nodes.first().map_or(0.0, |n| n.time_ms)
    }

    /// The facts of every node, for a response's `nodes`.
    pub fn facts_json(&self) -> Value {
        serde_json::to_value(&self.nodes).unwrap_or_default()
    }
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite())
}

fn read_node(
    v: &Value,
    id: String,
    parent: Option<usize>,
    depth: usize,
    out: &mut Vec<NodeFacts>,
) -> Result<usize, String> {
    if depth > MAX_DEPTH {
        return Err(format!("the plan nests deeper than {MAX_DEPTH} levels"));
    }
    if out.len() >= MAX_NODES {
        return Err(format!("the plan has more than {MAX_NODES} nodes"));
    }
    let Some(o) = v.as_object() else {
        return Err(format!("node {id} is not an object"));
    };
    // a CursorPlan holds its operator's numbers in `operator`
    let complete = o.get("complete").and_then(Value::as_bool).unwrap_or(true);
    let op = match o.get("operator") {
        Some(Value::Object(_)) => &o["operator"],
        Some(Value::String(_)) => v,
        _ => return Err(format!("node {id} has no operator")),
    };
    let operator = op["operator"]
        .as_str()
        .ok_or_else(|| format!("node {id} has no operator name"))?
        .chars()
        .take(64)
        .collect::<String>();
    let full: String = op["description"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(2000)
        .collect();
    let est = number(&op["estimatedRows"]).filter(|x| *x >= 0.0);
    let cost = number(&op["estimatedCost"]).filter(|x| *x >= 0.0);
    let act = op["actualRows"].as_i64().filter(|x| *x >= 0);
    let time = number(&op["timeMs"]).unwrap_or(0.0).max(0.0);
    let skipped = op["skipped"].as_str().map(|s| short(s, 200));
    let stopped = op["stoppedEarly"].as_bool().unwrap_or(false) || full.contains("[stopped early]");
    let me = out.len();
    out.push(NodeFacts {
        id: id.clone(),
        operator,
        description: clean(&full),
        columns: op["columns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c.as_str())
            .filter(|c| !is_generated(c))
            .take(64)
            .map(str::to_string)
            .collect(),
        estimated_rows: est,
        estimated_cost: cost,
        actual_rows: act,
        time_ms: time,
        self_ms: 0.0,
        complete: complete && op["complete"].as_bool() != Some(false),
        partial: false,
        skipped,
        stopped_early: stopped,
        cached: op["cached"].as_bool().unwrap_or(false),
        runs: op["runs"].as_u64().filter(|r| *r > 1),
        pushed_filters: op["pushedFilters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f.as_str())
            .take(32)
            .map(|f| short(f, 200))
            .collect(),
        children: Vec::new(),
        parent,
        full_description: full,
    });
    let kids = o
        .get("children")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut child_ids = Vec::with_capacity(kids.len());
    let mut child_ms = 0.0;
    for (i, c) in kids.iter().enumerate() {
        let at = read_node(c, format!("{id}.{i}"), Some(me), depth + 1, out)?;
        child_ms += out[at].time_ms;
        child_ids.push(at);
    }
    let n = &mut out[me];
    n.children = child_ids;
    n.self_ms = (n.time_ms - child_ms).max(0.0);
    Ok(me)
}

/// A generated variable name (aggregates, DESCRIBE): 32 hex digits.
fn is_generated(name: &str) -> bool {
    name.len() == 32 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A description without the runtime notes in square brackets and with generated
/// variable names shortened.
fn clean(d: &str) -> String {
    let mut out = String::with_capacity(d.len());
    let mut depth = 0usize;
    let mut in_iri = false;
    for c in d.chars() {
        match c {
            '<' if depth == 0 => {
                in_iri = true;
                out.push(c);
            }
            '>' if depth == 0 => {
                in_iri = false;
                out.push(c);
            }
            '[' if !in_iri => depth += 1,
            ']' if !in_iri && depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    shorten_generated(&out)
}

fn shorten_generated(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('?') {
        out.push_str(&rest[..=i]);
        let tail = &rest[i + 1..];
        if tail.len() >= 32 && is_generated(&tail[..32]) {
            out.push_str("agg");
            rest = &tail[32..];
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

/// `s` cut to `max` characters with an ellipsis.
pub fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

// ----------------------------------------------------------------- numbers ------

/// Milliseconds as a person reads them: `0.4 ms`, `12 ms`, `2.1 s`.
pub fn fmt_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        let s = ms / 1000.0;
        if s >= 10.0 {
            format!("{s:.0} s")
        } else {
            format!("{s:.1} s")
        }
    } else if ms >= 10.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{ms:.1} ms")
    }
}

/// A count with thousands separators, or with K and M from 100,000 on.
pub fn fmt_rows(n: f64) -> String {
    let n = n.max(0.0);
    if n >= 1e9 {
        format!("{:.1}G", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1}M", n / 1e6)
    } else if n >= 1e5 {
        format!("{:.0}K", n / 1e3)
    } else {
        let s = format!("{:.0}", n);
        let mut out = String::new();
        for (i, c) in s.chars().enumerate() {
            if i > 0 && (s.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        out
    }
}

// ------------------------------------------------------------------- notes ------

/// How much a note matters, which orders them and marks the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// the reason a query stopped or is slow
    High,
    /// a likely cause, such as a misestimate or a blowup
    Warning,
    /// context, such as a skipped node
    Info,
}

/// A note on an operator, or on the query when `node` is `None` (§6.6.2).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub node: Option<String>,
    pub code: String,
    pub severity: Severity,
    pub text: String,
    /// `explain`, `planner`, `lint` or `schema`
    pub source: &'static str,
    /// the editor range of a lint finding shown under **Query**
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Value>,
    /// the node's share of the time, which orders notes of one severity
    #[serde(skip)]
    pub share: f64,
}

/// The budget that stopped a query (§6.6.4).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stop {
    /// `timeout`, `memory`, `rows` or `rows-produced`
    pub budget: String,
    /// seconds for the timeout, bytes or rows for the others
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<f64>,
}

impl Stop {
    /// The budget of an error body of `/{ds}/sparql` (`{error, budget, limit}` or a
    /// `408` with `timeoutSeconds`), when one stopped the query.
    pub fn of_error_body(e: &Value) -> Option<Stop> {
        if let Some(b) = e["budget"].as_str() {
            return Some(Stop {
                budget: b.to_string(),
                limit: number(&e["limit"]),
                elapsed_ms: None,
            });
        }
        if e.get("timeoutSeconds").is_some() || e["code"] == "timeout" {
            return Some(Stop {
                budget: "timeout".into(),
                limit: number(&e["timeoutSeconds"]),
                elapsed_ms: None,
            });
        }
        None
    }

    /// The budget of an engine error, when one stopped the query.
    pub fn of_error(e: &sparkles::error::Error, timeout: Option<f64>) -> Option<Stop> {
        use sparkles::error::Error;
        match e {
            Error::Timeout => Some(Stop {
                budget: "timeout".into(),
                limit: timeout,
                elapsed_ms: None,
            }),
            Error::BudgetExceeded(b) => Some(Stop {
                budget: serde_json::to_value(b.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                limit: Some(b.limit as f64),
                elapsed_ms: None,
            }),
            _ => None,
        }
    }

    fn name(&self) -> String {
        match self.budget.as_str() {
            "timeout" => match self.limit {
                Some(s) => format!("{} timeout", fmt_ms(s * 1000.0)),
                None => "timeout".into(),
            },
            "memory" => match self.limit {
                Some(b) => format!("memory budget of {}", fmt_bytes(b)),
                None => "memory budget".into(),
            },
            "rows" => match self.limit {
                Some(r) => format!("limit of {} rows per result", fmt_rows(r)),
                None => "row limit".into(),
            },
            "rows-produced" => match self.limit {
                Some(r) => format!("budget of {} rows produced", fmt_rows(r)),
                None => "budget of rows produced".into(),
            },
            other => format!("{other} budget"),
        }
    }
}

fn fmt_bytes(b: f64) -> String {
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GiB", b / 1024.0 / 1024.0 / 1024.0)
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.0} MiB", b / 1024.0 / 1024.0)
    } else {
        format!("{:.0} KiB", b / 1024.0)
    }
}

/// Mark the counts that a failure cut short: a node that is not complete, did not
/// stop early and ran. Without a failure, nothing is partial.
pub fn mark_partial(plan: &mut Plan, failed: bool) {
    if !failed {
        return;
    }
    for n in &mut plan.nodes {
        n.partial = !n.complete && !n.stopped_early && n.skipped.is_none();
    }
}

fn joins(op: &str) -> bool {
    matches!(
        op,
        "HashJoin"
            | "MergeJoin"
            | "CartesianProduct"
            | "OptionalJoin"
            | "IndexJoin"
            | "StarJoin"
            | "SemiJoin"
            | "AntiJoin"
            | "Minus"
            | "SpatialJoin"
    )
}

/// The notes of a plan (§6.6.2), in order: by severity, then by the node's share of
/// the time, at most [`SHOWN_NOTES`] shown before the rest.
pub fn notes(plan: &Plan, stop: Option<&Stop>) -> Vec<Note> {
    let mut out = Vec::new();
    let total = plan.total_ms().max(1e-9);
    let nodes = &plan.nodes;
    let share = |n: &NodeFacts| n.self_ms / total;
    let partial = |n: &NodeFacts, rows: i64| -> String {
        if n.partial {
            format!("{} so far", fmt_rows(rows as f64))
        } else {
            fmt_rows(rows as f64)
        }
    };
    let mut note = |n: Option<&NodeFacts>, code: &str, sev: Severity, text: String| {
        out.push(Note {
            node: n.map(|n| n.id.clone()),
            code: code.to_string(),
            severity: sev,
            text,
            source: "explain",
            range: None,
            share: n.map_or(1.0, share),
        });
    };
    if let Some(s) = stop {
        let elapsed = s
            .elapsed_ms
            .or_else(|| plan.executed.then(|| plan.total_ms()))
            .map(|ms| format!(" after {}", fmt_ms(ms)))
            .unwrap_or_default();
        note(
            None,
            "budget",
            Severity::High,
            format!("The {} stopped the query{elapsed}.", s.name()),
        );
    }
    // dominant: own time for the timeout and for any run, rows for the row and memory
    // budgets
    let by_rows = stop.is_some_and(|s| s.budget != "timeout");
    if plan.executed {
        let best = if by_rows {
            nodes
                .iter()
                .filter(|n| n.actual_rows.is_some())
                .max_by_key(|n| n.actual_rows.unwrap_or(0))
        } else {
            nodes.iter().max_by(|a, b| a.self_ms.total_cmp(&b.self_ms))
        };
        if let Some(n) = best {
            if by_rows {
                if let Some(r) = n.actual_rows.filter(|r| *r > 0) {
                    note(
                        Some(n),
                        "dominant",
                        Severity::High,
                        format!("{} produced the most rows, {}.", n.label(), partial(n, r)),
                    );
                }
            } else if n.self_ms / total >= 0.2 && n.self_ms > 0.0 {
                let sev = if stop.is_some() || n.self_ms >= 100.0 {
                    Severity::High
                } else {
                    Severity::Info
                };
                note(
                    Some(n),
                    "dominant",
                    sev,
                    format!(
                        "{} took {} of the {}.",
                        n.label(),
                        fmt_ms(n.self_ms),
                        fmt_ms(plan.total_ms())
                    ),
                );
            }
        }
    } else if let Some(n) = nodes
        .iter()
        .filter(|n| n.estimated_cost.is_some())
        .max_by(|a, b| {
            own_cost(a, nodes)
                .partial_cmp(&own_cost(b, nodes))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    {
        let root = nodes[0].estimated_cost.unwrap_or(0.0).max(1e-9);
        let c = own_cost(n, nodes);
        if c / root >= 0.2 {
            note(
                Some(n),
                "dominant",
                Severity::Info,
                format!(
                    "{} has {:.0}% of the estimated cost.",
                    n.label(),
                    100.0 * c / root
                ),
            );
        }
    }
    for n in nodes {
        // a node whose parent was skipped repeats its parent's reason
        let parent_skipped = n.parent.is_some_and(|p| nodes[p].skipped.is_some());
        if let Some(why) = &n.skipped {
            if !parent_skipped {
                note(
                    Some(n),
                    "skipped",
                    Severity::Info,
                    format!("{} did not run because {why}.", n.label()),
                );
            }
            continue;
        }
        if let (Some(e), Some(a)) = (n.estimated_rows, n.actual_rows)
            && !n.stopped_early
            && !n.partial
        {
            let (e1, a1) = (e.max(1.0), (a as f64).max(1.0));
            if e1 / a1 >= 10.0 || a1 / e1 >= 10.0 {
                note(
                    Some(n),
                    "misestimate",
                    Severity::Warning,
                    format!(
                        "{} was estimated at {} rows and produced {}.",
                        n.label(),
                        fmt_rows(e),
                        fmt_rows(a as f64)
                    ),
                );
            }
        }
        if joins(&n.operator)
            && let Some(a) = n.actual_rows
            && n.children.len() >= 2
        {
            let ins: Vec<i64> = n
                .children
                .iter()
                .filter_map(|&c| nodes[c].actual_rows)
                .collect();
            if ins.len() == n.children.len() {
                let larger = ins.iter().copied().max().unwrap_or(0).max(1);
                if a >= 10 * larger && a >= 1000 {
                    let vars = if n.columns.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " on {}",
                            n.columns
                                .iter()
                                .take(4)
                                .map(|c| format!("?{c}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    note(
                        Some(n),
                        "blowup",
                        Severity::Warning,
                        format!(
                            "{}{vars} produced {} rows from inputs of {} and {}.",
                            n.operator,
                            partial(n, a),
                            fmt_rows(ins[0] as f64),
                            fmt_rows(ins[1] as f64)
                        ),
                    );
                }
            }
        }
        if n.operator == "Filter"
            && let (Some(a), Some(input)) = (n.actual_rows, n.input_rows(nodes))
            && input >= 10_000
            && (a as f64) < input as f64 * 0.01
        {
            note(
                Some(n),
                "late-filter",
                Severity::Warning,
                format!(
                    "{} kept {} of {} rows.",
                    n.label(),
                    partial(n, a),
                    fmt_rows(input as f64)
                ),
            );
        }
        if matches!(
            n.operator.as_str(),
            "Sort" | "OrderBy" | "TopK" | "GroupBy" | "Distinct"
        ) && let Some(input) = n.input_rows(nodes)
            && input > 1_000_000
        {
            note(
                Some(n),
                "large-sort",
                Severity::Warning,
                format!(
                    "{} read {} rows before its first output.",
                    n.label(),
                    fmt_rows(input as f64)
                ),
            );
        }
        if n.operator == "TransitivePath" && n.children.is_empty() && open_ends(&n.description) {
            note(
                Some(n),
                "open-path",
                Severity::Warning,
                format!(
                    "The path {} starts from every node.",
                    short(&n.description, 80)
                ),
            );
        }
        let parent_stopped = n.parent.is_some_and(|p| nodes[p].stopped_early);
        if n.stopped_early
            && !parent_stopped
            && let Some(a) = n.actual_rows
        {
            note(
                Some(n),
                "stopped-early",
                Severity::Info,
                format!(
                    "{} stopped after {} rows, which was enough.",
                    n.label(),
                    fmt_rows(a as f64)
                ),
            );
        }
    }
    if plan.hidden {
        note(
            nodes.first(),
            "hidden-estimates",
            Severity::Info,
            "Estimates are hidden for your view, so only actual counts are shown.".into(),
        );
    }
    for (code, message) in &plan.warnings {
        out.push(Note {
            node: None,
            code: code.clone(),
            severity: Severity::Warning,
            text: message.clone(),
            source: "planner",
            range: None,
            share: 0.0,
        });
    }
    sort_notes(&mut out);
    out
}

/// Order notes by severity, the budget first, then by the node's share of the time.
pub fn sort_notes(notes: &mut [Note]) {
    notes.sort_by(|a, b| {
        (a.code != "budget")
            .cmp(&(b.code != "budget"))
            .then(a.severity.cmp(&b.severity))
            .then(b.share.total_cmp(&a.share))
    });
}

fn own_cost(n: &NodeFacts, nodes: &[NodeFacts]) -> f64 {
    let kids: f64 = n
        .children
        .iter()
        .filter_map(|&c| nodes[c].estimated_cost)
        .sum();
    (n.estimated_cost.unwrap_or(0.0) - kids).max(0.0)
}

/// Whether a path's description has variables at both ends (`?x (<p>)+ ?y`).
fn open_ends(d: &str) -> bool {
    let words: Vec<&str> = d.split_whitespace().collect();
    words.len() >= 3
        && words[0].starts_with('?')
        && words.last().is_some_and(|w| w.starts_with('?'))
}

/// A lint finding as the explanation places it: on the node whose variables it names,
/// or under **Query** with its editor range.
#[derive(Clone, Debug)]
pub struct Finding {
    pub rule: String,
    pub severity: String,
    pub message: String,
    /// the variables the message names
    pub vars: Vec<String>,
    pub range: Value,
}

/// Add lint findings to `notes`, each on the node of its variables when one matches.
pub fn add_findings(plan: &Plan, notes: &mut Vec<Note>, findings: &[Finding]) {
    for f in findings {
        let node = (!f.vars.is_empty())
            .then(|| {
                plan.nodes.iter().find(|n| {
                    (f.rule != "cartesian-product" || n.operator == "CartesianProduct")
                        && f.vars.iter().all(|v| n.columns.iter().any(|c| c == v))
                })
            })
            .flatten();
        let node = match (f.rule.as_str(), node) {
            ("cartesian-product", None) => {
                plan.nodes.iter().find(|n| n.operator == "CartesianProduct")
            }
            (_, n) => n,
        };
        notes.push(Note {
            node: node.map(|n| n.id.clone()),
            code: f.rule.clone(),
            severity: match f.severity.as_str() {
                "error" => Severity::High,
                "warning" => Severity::Warning,
                _ => Severity::Info,
            },
            text: f.message.clone(),
            source: "lint",
            range: node.is_none().then(|| f.range.clone()),
            share: 0.0,
        });
    }
    sort_notes(notes);
}

/// The variables a message names, such as `?a` and `?b`.
pub fn vars_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars = text.char_indices().peekable();
    for (i, c) in chars {
        if c != '?' && c != '$' {
            continue;
        }
        let rest = &text[i + 1..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

// ------------------------------------------------------------- description ------

/// One sentence of **What it asks**, with the nodes it describes.
#[derive(Clone, Debug, Serialize)]
pub struct Sentence {
    pub text: String,
    pub nodes: Vec<String>,
}

/// The terms of a plan's descriptions as a person reads them: the label or prefixed
/// name of an IRI, a literal as written.
pub trait Labels {
    fn label(&self, iri: &str) -> String;
}

/// Labels from prefixes and local names.
pub struct LocalNames<'a> {
    pub prefixes: &'a [(String, String)],
    pub labels: std::collections::HashMap<String, String>,
}

impl Labels for LocalNames<'_> {
    fn label(&self, iri: &str) -> String {
        if let Some(l) = self.labels.get(iri) {
            return l.clone();
        }
        if iri == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type" {
            return "type".into();
        }
        let local = iri
            .rsplit(['#', '/', ':'])
            .find(|s| !s.is_empty())
            .unwrap_or(iri);
        if local.is_empty() || local.len() > 60 {
            for (p, ns) in self.prefixes {
                if let Some(rest) = iri.strip_prefix(ns.as_str()) {
                    return format!("{p}:{rest}");
                }
            }
            return format!("<{iri}>");
        }
        local.to_string()
    }
}

/// The terms of a description: `?v`, `<iri>`, `prefix:name`, literals and numbers.
fn words(d: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut in_iri, mut in_str) = (false, false);
    for c in d.chars() {
        match c {
            '<' if !in_str => {
                in_iri = true;
                cur.push(c);
            }
            '>' if in_iri => {
                in_iri = false;
                cur.push(c);
            }
            '"' if !in_iri => {
                in_str = !in_str;
                cur.push(c);
            }
            c if c.is_whitespace() && !in_iri && !in_str => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A term of a description as a person reads it.
fn term(w: &str, labels: &dyn Labels) -> String {
    if let Some(iri) = w.strip_prefix('<').and_then(|w| w.strip_suffix('>')) {
        return labels.label(iri);
    }
    if w == "rdf:type" || w == "a" {
        return "type".into();
    }
    // a typed literal reads as its value
    if let Some(i) = w.find("^^")
        && w.starts_with('"')
    {
        return w[..i].to_string();
    }
    w.to_string()
}

/// A triple pattern of a scan description (`POS ?p <age> ?a`) as text.
fn pattern_text(desc: &str, labels: &dyn Labels) -> Option<String> {
    let w = words(desc);
    let (perm, rest) = w.split_first()?;
    if perm.len() < 3 || !perm.chars().all(|c| "SPOG".contains(c)) || rest.len() < 3 {
        return None;
    }
    // the terms follow in subject, predicate, object order, whatever the permutation
    let (s, p, o) = (
        term(&rest[0], labels),
        term(&rest[1], labels),
        term(&rest[2], labels),
    );
    Some(if p == "type" {
        format!("{s} is a {o}")
    } else {
        format!("{s} {p} {o}")
    })
}

fn sentence_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
    }
}

/// The patterns under `at`, as text, with the ids of the nodes they come from. An
/// optional side is not followed.
fn patterns(
    plan: &Plan,
    at: usize,
    labels: &dyn Labels,
    texts: &mut Vec<String>,
    ids: &mut Vec<String>,
) {
    let n = &plan.nodes[at];
    if n.skipped.is_some() && n.parent.is_some_and(|p| plan.nodes[p].cached) {
        // the subtree of a cache hit still says what was computed
    }
    match n.operator.as_str() {
        "IndexScan" | "IndexRangeScan" | "CountFromIndex" | "IndexTopK" => {
            if let Some(t) = pattern_text(&n.description, labels)
                && !texts.contains(&t)
            {
                texts.push(t);
                ids.push(n.id.clone());
            }
            // a top-k's fallback plan describes the same patterns again
        }
        "TransitivePath" => {
            let d = words(&n.description)
                .iter()
                .map(|w| {
                    term(
                        w.trim_start_matches('(').trim_end_matches([')', '+', '*']),
                        labels,
                    )
                })
                .collect::<Vec<_>>();
            if d.len() >= 3 {
                let mark = if n.description.contains(")+") {
                    " at any depth"
                } else {
                    " at any depth, or itself"
                };
                texts.push(format!("{} {} {}{mark}", d[0], d[1], d[d.len() - 1]));
                ids.push(n.id.clone());
            }
            for &c in &n.children {
                patterns(plan, c, labels, texts, ids);
            }
        }
        "OptionalJoin" => {
            ids.push(n.id.clone());
            if let Some(&c) = n.children.first() {
                patterns(plan, c, labels, texts, ids);
            }
        }
        "Minus" | "AntiJoin" => {
            if let Some(&c) = n.children.first() {
                patterns(plan, c, labels, texts, ids);
            }
        }
        _ => {
            if joins(&n.operator) || n.operator == "Union" {
                ids.push(n.id.clone());
            }
            if n.operator != "IndexTopK" {
                for &c in &n.children {
                    patterns(plan, c, labels, texts, ids);
                }
            }
        }
    }
}

/// The template description of §6.6.3: one to four sentences, each naming the nodes it
/// describes. `kind` is the query form (`SELECT`, `ASK`, `CONSTRUCT` or `DESCRIBE`).
pub fn describe(plan: &Plan, kind: &str, labels: &dyn Labels) -> Vec<Sentence> {
    let mut out = Vec::new();
    let Some(root) = plan.nodes.first() else {
        return out;
    };
    // what it finds: the patterns, joined with "and"
    let mut texts = Vec::new();
    let mut ids = Vec::new();
    patterns(plan, 0, labels, &mut texts, &mut ids);
    let projected: Vec<String> = plan
        .nodes
        .iter()
        .find(|n| n.operator == "Project")
        .map(|n| n.columns.iter().map(|c| format!("?{c}")).collect())
        .unwrap_or_default();
    let verb = match kind {
        "ASK" => "Asks whether there is",
        "CONSTRUCT" => "Builds triples from",
        "DESCRIBE" => "Describes the resources of",
        _ => "Finds",
    };
    let what = if projected.is_empty() || kind != "SELECT" {
        String::new()
    } else {
        format!("{} ", list(&projected))
    };
    let first = if texts.is_empty() {
        format!("{verb} {what}from the values the query gives.")
    } else {
        let link = if what.is_empty() { "" } else { "where " };
        let any = if kind == "ASK" { "a match where " } else { "" };
        format!("{verb} {what}{link}{any}{}.", list(&texts))
    };
    if ids.is_empty() {
        ids.push(root.id.clone());
    }
    out.push(Sentence {
        text: sentence_case(first.trim()),
        nodes: ids,
    });
    // what it keeps: optional parts, filters, exclusions
    let mut keep = Vec::new();
    let mut keep_ids = Vec::new();
    for n in &plan.nodes {
        match n.operator.as_str() {
            "OptionalJoin" => {
                let mut t = Vec::new();
                let mut i = Vec::new();
                if let Some(&c) = n.children.get(1) {
                    patterns(plan, c, labels, &mut t, &mut i);
                }
                if !t.is_empty() {
                    keep.push(format!("with, when known, {}", list(&t)));
                    keep_ids.push(n.id.clone());
                }
            }
            "Filter" => {
                let d = filter_text(&n.description, labels);
                if !d.is_empty() {
                    keep.push(format!("keeping those where {d}"));
                    keep_ids.push(n.id.clone());
                }
            }
            "Minus" | "AntiJoin" => {
                let mut t = Vec::new();
                let mut i = Vec::new();
                if let Some(&c) = n.children.get(1) {
                    patterns(plan, c, labels, &mut t, &mut i);
                }
                if !t.is_empty() {
                    keep.push(format!("leaving out those where {}", list(&t)));
                    keep_ids.push(n.id.clone());
                }
            }
            _ => {}
        }
        for f in &n.pushed_filters {
            keep.push(format!("keeping those where {}", filter_text(f, labels)));
            keep_ids.push(n.id.clone());
        }
    }
    if !keep.is_empty() {
        out.push(Sentence {
            text: sentence_case(&format!("{}.", list(&keep))),
            nodes: keep_ids,
        });
    }
    // how it shapes the answer: grouping, order, duplicates, limit
    let mut shape = Vec::new();
    let mut shape_ids = Vec::new();
    for n in &plan.nodes {
        let d = &n.description;
        match n.operator.as_str() {
            "GroupBy" => {
                let by = d
                    .strip_prefix("by ")
                    .and_then(|r| r.split(" aggs ").next())
                    .unwrap_or("")
                    .trim();
                if by.is_empty() {
                    shape.push("counted or aggregated over all solutions".to_string());
                } else {
                    shape.push(format!("grouped by {by}"));
                }
                shape_ids.push(n.id.clone());
            }
            "OrderBy" | "TopK" => {
                let by = d.trim();
                if !by.is_empty() {
                    shape.push(format!("sorted by {}", by.replace("DESC(", "descending (")));
                    shape_ids.push(n.id.clone());
                }
            }
            "Distinct" => {
                shape.push("without duplicates".to_string());
                shape_ids.push(n.id.clone());
            }
            "Limit" => {
                let (mut offset, mut limit) = (None, None);
                let w: Vec<&str> = d.split_whitespace().collect();
                for p in w.windows(2) {
                    match p[0] {
                        "offset" => offset = p[1].parse::<u64>().ok().filter(|o| *o > 0),
                        "limit" => limit = p[1].parse::<u64>().ok(),
                        _ => {}
                    }
                }
                if kind == "ASK" {
                    continue;
                }
                match (offset, limit) {
                    (Some(o), Some(l)) => shape.push(format!("{l} of them after the first {o}")),
                    (None, Some(l)) => shape.push(format!("the first {l}")),
                    (Some(o), None) => shape.push(format!("all after the first {o}")),
                    (None, None) => continue,
                }
                shape_ids.push(n.id.clone());
            }
            _ => {}
        }
    }
    if !shape.is_empty() {
        out.push(Sentence {
            text: sentence_case(&format!("The answer is {}.", list(&shape))),
            nodes: shape_ids,
        });
    }
    // services and searches, which read outside the patterns
    let mut other = Vec::new();
    let mut other_ids = Vec::new();
    for n in &plan.nodes {
        let what = match n.operator.as_str() {
            "Service" => Some("asks a remote SPARQL service"),
            "TextSearch" => Some("searches the full-text index"),
            "VectorSearch" => Some("searches the vector index"),
            "HybridSearch" => Some("searches the text and vector indexes"),
            "SpatialScan" | "SpatialPf" | "SpatialJoin" | "SpatialKnn" | "SpatialRelate" => {
                Some("searches the spatial index")
            }
            _ => None,
        };
        if let Some(w) = what
            && !other.contains(&w.to_string())
        {
            other.push(w.to_string());
            other_ids.push(n.id.clone());
        }
    }
    if !other.is_empty() {
        out.push(Sentence {
            text: sentence_case(&format!("It also {}.", list(&other))),
            nodes: other_ids,
        });
    }
    out.truncate(4);
    out
}

/// A filter expression with its IRIs as labels.
fn filter_text(d: &str, labels: &dyn Labels) -> String {
    let w = words(d);
    let t: Vec<String> = w
        .iter()
        .map(|w| {
            if w.contains('<') {
                // an IRI inside a call, such as `STRSTARTS(STR(<iri>)`
                let mut out = String::new();
                let mut rest = w.as_str();
                while let Some(i) = rest.find('<') {
                    out.push_str(&rest[..i]);
                    match rest[i..].find('>') {
                        Some(j) => {
                            out.push_str(&labels.label(&rest[i + 1..i + j]));
                            rest = &rest[i + j + 1..];
                        }
                        None => {
                            out.push_str(&rest[i..]);
                            rest = "";
                        }
                    }
                }
                out.push_str(rest);
                out
            } else {
                w.clone()
            }
        })
        .collect();
    short(&t.join(" "), 160)
}

/// The deadline of a `run` profile when the call names none, in seconds.
pub const RUN_TIMEOUT_SECS: u64 = 30;

/// A plan with its notes and template description: what every explanation shows
/// before any model writes.
pub struct Built {
    pub plan: Plan,
    pub notes: Vec<Note>,
    pub asks: Vec<Sentence>,
}

/// Read `plan_json` and compute its notes, with the query's lint findings when the
/// `fmt` feature is built, and the template description. `failed` says the plan is of
/// a query that stopped, so its unfinished counts are partial.
pub fn build(
    plan_json: &Value,
    query: &str,
    kind: &str,
    prefixes: &[(String, String)],
    stop: Option<&Stop>,
    failed: bool,
) -> Result<Built, String> {
    let mut plan = Plan::read(plan_json)?;
    mark_partial(&mut plan, failed || stop.is_some());
    let mut notes = notes(&plan, stop);
    add_findings(&plan, &mut notes, &lint_findings(query));
    let labels = LocalNames {
        prefixes,
        labels: Default::default(),
    };
    let asks = describe(&plan, kind, &labels);
    Ok(Built { plan, notes, asks })
}

/// The lint findings of X04 that bear on a plan: errors and warnings.
#[cfg(feature = "fmt")]
fn lint_findings(query: &str) -> Vec<Finding> {
    use sparkles_fmt::lint::{self, LintOptions, Severity as S};
    let opts = LintOptions {
        deadline: Some(std::time::Instant::now() + std::time::Duration::from_millis(500)),
        ..Default::default()
    };
    let Ok(Ok(l)) =
        std::panic::catch_unwind(|| lint::lint(query, sparkles_fmt::Language::Sparql, &opts))
    else {
        return Vec::new();
    };
    l.diagnostics
        .iter()
        .filter(|d| matches!(d.severity, S::Error | S::Warning) && d.rule != "syntax")
        .take(20)
        .map(|d| Finding {
            rule: d.rule.to_string(),
            severity: d.severity.name().to_string(),
            message: d.message.clone(),
            vars: vars_in(&d.message),
            range: json!({
                "line": d.line,
                "column": d.column,
                "endLine": d.end_line,
                "endColumn": d.end_column,
            }),
        })
        .collect()
}

#[cfg(not(feature = "fmt"))]
fn lint_findings(_query: &str) -> Vec<Finding> {
    Vec::new()
}

/// The text plan of `explain_query` with notes (C18 §9.6): each line starts with the
/// node's id, and adds `act=` and `ms=` after a run.
pub fn text_lines(plan: &Plan) -> String {
    let mut out = String::new();
    for n in &plan.nodes {
        let depth = n.id.matches('.').count();
        out.push_str(&"  ".repeat(depth));
        out.push_str(&n.id);
        out.push(' ');
        out.push_str(&n.operator);
        if !n.full_description.is_empty() {
            out.push(' ');
            out.push_str(&n.full_description.replace(['\n', '\r'], " "));
        }
        match n.estimated_rows {
            Some(e) => out.push_str(&format!(" est={}", e.round() as u64)),
            None => out.push_str(" est=?"),
        }
        if plan.executed {
            match (n.actual_rows, &n.skipped) {
                (_, Some(_)) => out.push_str(" skipped"),
                (Some(a), _) => {
                    let mark = if n.partial { "+" } else { "" };
                    out.push_str(&format!(" act={a}{mark} ms={:.3}", n.time_ms));
                }
                (None, _) => out.push_str(" act=?"),
            }
        }
        let cols: Vec<String> = n.columns.iter().map(|c| format!("?{c}")).collect();
        out.push_str(&format!(" [{}]\n", cols.join(" ")));
    }
    out
}
