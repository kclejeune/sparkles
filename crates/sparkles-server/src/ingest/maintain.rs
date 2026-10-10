//! The maintenance of agent memory (spec C18 Phase 5), as tasks of the ingestion
//! runtime with its progress, usage and cancellation:
//!
//! - **Consolidation** (§8.3) asserts each fact that several sessions asserted once in
//!   the consolidated graph, on a review branch, with a reifier derived from the session
//!   reifiers. It lists the duplicate entities that `link_entities` finds and the
//!   conflicts that `recall` reports, for a person, and never asserts `owl:sameAs`. In
//!   `auto` mode it merges the branch when every fact passed its checks and the merge
//!   preview is clean, as `auto` ingestion does.
//! - **Retention** (§8.4) deletes, with one Graph Store `DELETE` each, the session
//!   graphs whose newest fact is older than the dataset's `retention.after` and, by
//!   default, whose facts a reviewed graph asserts too.
//!
//! `POST /$/memory/{ds}/consolidate` and `POST /$/memory/{ds}/retention` start a task as
//! the caller, and every write goes through the tools with the caller's grants. The
//! server also starts them on the schedules of `memory.json` (`consolidation.every`,
//! `retention.every`), as the server itself. `GET /$/memory/{ds}/maintenance` reports
//! the settings, the last scheduled runs and the next. The task routes of
//! `/$/ingest/{ds}/{task}` read and cancel these tasks.

use super::pipeline::{Ctx, Failed, Run, Usage, expand_iri, merge_if, numbered_branch};
use super::{Status, Task};
use crate::assist::{ConsolidationMode, memory_settings};
use crate::auth::{Level, Principal};
use crate::http::{AdminBody, ApiResult, err, err_code};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The facts one `assert_facts` call of a consolidation writes.
const BATCH: usize = 200;
/// The dry runs that drop failing facts before a batch is written.
const FIX_PASSES: usize = 3;
/// The `link_entities` calls of the duplicate check, 20 entities each.
const LINK_CALLS: usize = 10;
/// The `recall` calls of the conflict check, 20 subjects each.
const RECALL_CALLS: usize = 10;
/// The longest a maintenance task runs by default.
const DEADLINE: Duration = Duration::from_secs(3600);
/// How often the scheduler looks for due tasks.
const TICK: Duration = Duration::from_secs(60);
/// The default schedule of retention.
const RETENTION_EVERY: &str = "1d";
/// The state of the schedules, per dataset.
const STATE_FILE: &str = "maintenance.json";

/// What a consolidation pass does.
#[derive(Clone, Debug, Default)]
pub struct ConsolidateRequest {
    pub dataset: String,
    pub mode: ConsolidationMode,
    /// the distinct sources a fact needs (default: the dataset's, else 2)
    pub min_sources: Option<u32>,
    /// report what the pass would write, and write nothing
    pub dry_run: bool,
    pub message: Option<String>,
    /// started by the schedule: a pass whose earlier branch still waits for review
    /// writes nothing
    pub scheduled: bool,
}

/// What a retention pass does.
#[derive(Clone, Debug, Default)]
pub struct RetentionRequest {
    pub dataset: String,
    /// the dataset's `retention` members, overridden by these
    pub after: Option<String>,
    pub graphs: Option<Vec<String>>,
    pub require_consolidated: Option<bool>,
    pub dry_run: bool,
    /// started by the schedule
    pub scheduled: bool,
}

fn failed(code: &str, status: u16, message: impl Into<String>) -> Failed {
    Failed(json!({ "code": code, "status": status, "message": message.into() }))
}

/// Run one consolidation pass. The value is the result; the usage is reported either
/// way.
pub fn consolidate(ctx: &Ctx, req: &ConsolidateRequest) -> (Result<Value, Value>, Value) {
    let started = Instant::now();
    let mut r = Run {
        ctx,
        dataset: req.dataset.clone(),
        usage: Usage::default(),
    };
    let out = consolidate_inner(&mut r, req).map_err(|f| f.0);
    (out, r.usage.json(started))
}

fn consolidate_inner(r: &mut Run, req: &ConsolidateRequest) -> Result<Value, Failed> {
    let ctx = r.ctx;
    let st = &ctx.server.state;
    let ds = ctx
        .server
        .dataset(&ctx.principal.clone().on_branch(None), Some(&req.dataset))
        .map_err(Failed::from)?;
    let memory = memory_settings(st, &ds);
    if memory.agent_graphs.is_empty() {
        return Err(failed(
            "no-agent-graphs",
            400,
            "the dataset's memory settings name no agentGraphs, so there is nothing to consolidate",
        ));
    }
    let Some(target) = memory.consolidated_graph.clone() else {
        return Err(failed(
            "no-consolidated-graph",
            400,
            "the dataset's memory settings name no consolidatedGraph to write to",
        ));
    };
    if req.mode == ConsolidationMode::Auto && !ctx.principal.can(&ds.name, Level::Admin) {
        return Err(failed(
            "forbidden",
            403,
            "only an admin of the dataset may consolidate in auto mode",
        ));
    }
    let min = req
        .min_sources
        .or(memory.consolidation.as_ref().map(|c| c.min_sources))
        .unwrap_or(2);
    // a scheduled pass waits until the person has reviewed the last one
    if req.scheduled
        && let Some(open) = ds.store.branches().ok().and_then(|bs| {
            bs.into_iter()
                .find(|b| b.name.starts_with("consolidation.") && b.ahead > 0)
        })
    {
        return Ok(json!({
            "outcome": "pending-review",
            "branch": open.name,
            "message": "an earlier consolidation waits for review; this pass wrote nothing",
        }));
    }
    // 1. the repeated facts, the entities and the subjects of agent memory
    ctx.progress
        .status(Status::Scanning, 0.05, Some("reading agent memory".into()));
    let scan = r.tool(
        "memory_consolidation_scan",
        json!({ "minSources": min }),
        None,
    )?;
    r.check()?;
    // 2. duplicate entities: an exact label and a matching type (§8.3)
    ctx.progress.status(
        Status::Linking,
        0.3,
        Some("looking for duplicate entities".into()),
    );
    let duplicates = duplicates(r, &scan)?;
    // 3. conflicts that recall reports
    let conflicts = conflicts(r, &scan)?;
    let repeated: Vec<Value> = scan["repeated"].as_array().cloned().unwrap_or_default();
    let mut out = json!({
        "outcome": "proposed",
        "mode": match req.mode { ConsolidationMode::Branch => "branch", ConsolidationMode::Auto => "auto" },
        "target": target,
        "minSources": min,
        "scanned": scan["scanned"],
        "truncated": scan["truncated"],
        "repeated": repeated.len(),
        "duplicates": duplicates,
        "conflicts": conflicts,
    });
    if repeated.is_empty() || req.dry_run {
        out["outcome"] = if repeated.is_empty() {
            "nothing-to-consolidate"
        } else {
            "dry-run"
        }
        .into();
        out["facts"] = repeated
            .iter()
            .take(100)
            .cloned()
            .collect::<Vec<_>>()
            .into();
        return Ok(out);
    }
    r.check()?;
    // 4. the branch and the facts, in batches, each checked by a dry run first
    let date = chrono::Utc::now().format("%Y%m%d").to_string();
    let branch = numbered_branch(
        r,
        &ds,
        &|agent| match agent {
            Some(a) => format!("proposals.{a}.consolidate-{date}"),
            None => format!("consolidation.{date}"),
        },
        "Consolidation of agent memory",
    )?;
    out["branch"] = branch.clone().into();
    out["review"] = format!(
        "/ui/datasets/{}/review/{}",
        super::http::urlencode(&ds.name),
        super::http::urlencode(&branch)
    )
    .into();
    let message = req.message.clone().unwrap_or_else(|| {
        format!(
            "Consolidate memory: {} facts that {min} or more sources assert",
            repeated.len()
        )
    });
    let mut failed_facts: Vec<Value> = Vec::new();
    let mut commits: Vec<Value> = Vec::new();
    let mut proposed = 0usize;
    let mut guard_findings = false;
    let batches = repeated.len().div_ceil(BATCH);
    for (bi, batch) in repeated.chunks(BATCH).enumerate() {
        r.check()?;
        ctx.progress.status(
            Status::Writing,
            0.5 + 0.45 * bi as f32 / batches.max(1) as f32,
            Some(format!("batch {} of {batches}", bi + 1)),
        );
        let mut facts: Vec<Value> = batch
            .iter()
            .map(|f| {
                let from: Vec<String> = f["derivedFrom"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .take(20)
                    .map(|x| format!("<{x}>"))
                    .collect();
                let mut j = json!({ "s": f["s"], "p": f["p"], "o": f["o"] });
                if !from.is_empty() {
                    j["derivedFrom"] = from.into();
                }
                j
            })
            .collect();
        let args = |facts: &[Value], dry: bool| {
            let mut m = Map::new();
            m.insert("graph".into(), format!("<{target}>").into());
            m.insert("facts".into(), facts.to_vec().into());
            m.insert("allowUnknownIris".into(), true.into());
            m.insert("agent".into(), json!({ "name": "sparkles-consolidation" }));
            m.insert("message".into(), message.clone().into());
            if dry {
                m.insert("dryRun".into(), true.into());
            } else {
                let key =
                    crate::mcp::memory::ingest::digest(&format!("{}\0{branch}\0{bi}", ds.name));
                m.insert(
                    "idempotencyKey".into(),
                    format!(
                        "consolidate:{}",
                        key.trim_start_matches("sha256:")
                            .chars()
                            .take(48)
                            .collect::<String>()
                    )
                    .into(),
                );
            }
            Value::Object(m)
        };
        let mut ok = false;
        for _ in 0..FIX_PASSES {
            if facts.is_empty() {
                break;
            }
            match r.tool("assert_facts", args(&facts, true), Some(&branch)) {
                Ok(v) => {
                    if v.get("validation").is_some_and(|x| x["conforms"] == false) {
                        guard_findings = true;
                    }
                    ok = true;
                    break;
                }
                Err(e) => {
                    let errors: Vec<Value> = e
                        .data
                        .as_ref()
                        .and_then(|d| d["errors"].as_array().cloned())
                        .unwrap_or_default();
                    let drop: BTreeSet<usize> = errors
                        .iter()
                        .filter_map(|x| fact_index(x["at"].as_str().unwrap_or("")))
                        .filter(|i| *i < facts.len())
                        .collect();
                    if drop.is_empty() {
                        return Err(e.into());
                    }
                    for x in &errors {
                        if let Some(i) = fact_index(x["at"].as_str().unwrap_or(""))
                            && let Some(f) = facts.get(i)
                        {
                            failed_facts.push(json!({
                                "s": f["s"], "p": f["p"], "o": f["o"],
                                "code": x["code"], "message": x["message"],
                            }));
                        }
                    }
                    let mut i = 0;
                    facts.retain(|_| {
                        let keep = !drop.contains(&i);
                        i += 1;
                        keep
                    });
                }
            }
        }
        if facts.is_empty() {
            continue;
        }
        if !ok {
            return Err(failed(
                "invalid-facts",
                422,
                "the consolidated facts still failed their checks after the failing ones were dropped",
            ));
        }
        let v = r.tool("assert_facts", args(&facts, false), Some(&branch))?;
        if v.get("validation").is_some_and(|x| x["conforms"] == false) {
            guard_findings = true;
        }
        if let Some(c) = v.get("commit") {
            commits.push(c.clone());
        }
        proposed += facts.len();
    }
    out["proposed"] = proposed.into();
    out["failed"] = failed_facts.clone().into();
    out["commits"] = commits.into();
    if proposed == 0 {
        out["outcome"] = "no-facts".into();
        let _ = r.tool(
            "delete_branch",
            json!({ "name": branch, "force": true }),
            None,
        );
        if let Some(o) = out.as_object_mut() {
            o.remove("branch");
            o.remove("review");
        }
        return Ok(out);
    }
    if req.mode == ConsolidationMode::Auto {
        let why = if !failed_facts.is_empty() {
            Some("some facts failed their checks".to_string())
        } else if guard_findings {
            Some("the dataset's guard reported findings".to_string())
        } else {
            None
        };
        merge_if(r, &branch, &message, why, &mut out)?;
    }
    ctx.progress.status(Status::Writing, 0.99, None);
    Ok(out)
}

/// `facts[i]…` → `i`.
fn fact_index(at: &str) -> Option<usize> {
    let rest = at.strip_prefix("facts[")?;
    rest[..rest.find(']')?].parse().ok()
}

/// Pairs of entities with the same label and a matching type, from `link_entities` over
/// the labelled entities of agent memory.
fn duplicates(r: &Run, scan: &Value) -> Result<Vec<Value>, Failed> {
    let entities: Vec<&Value> = scan["entities"].as_array().into_iter().flatten().collect();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut out = Vec::new();
    for batch in entities.chunks(20).take(LINK_CALLS) {
        r.check()?;
        let mentions: Vec<Value> = batch
            .iter()
            .map(|e| json!({ "text": e["label"], "types": [e["type"]] }))
            .collect();
        let v = r.tool(
            "link_entities",
            json!({ "mentions": mentions, "k": 5 }),
            None,
        )?;
        let prefixes = v["prefixes"].clone();
        for (e, m) in batch
            .iter()
            .zip(v["mentions"].as_array().into_iter().flatten())
        {
            let me = e["iri"].as_str().unwrap_or("");
            for c in m["candidates"].as_array().into_iter().flatten() {
                let iri = expand_iri(c["iri"].as_str().unwrap_or(""), &prefixes);
                let exact = c["matchedBy"]
                    .as_array()
                    .is_some_and(|l| l.iter().any(|x| x == "exact" || x == "normalized"));
                if iri == me || iri.is_empty() || c["typeMatch"] != true || !exact {
                    continue;
                }
                let pair = if me < iri.as_str() {
                    (me.to_string(), iri.clone())
                } else {
                    (iri.clone(), me.to_string())
                };
                if seen.insert(pair.clone()) {
                    out.push(json!({
                        "label": e["label"],
                        "type": e["type"],
                        "entities": [pair.0, pair.1],
                    }));
                }
            }
        }
    }
    Ok(out)
}

/// The conflicts `recall` reports for the subjects of agent memory: two values of a
/// predicate the shapes allow once, from two graphs.
fn conflicts(r: &Run, scan: &Value) -> Result<Vec<Value>, Failed> {
    let subjects: Vec<&str> = scan["subjects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut out = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for batch in subjects.chunks(20).take(RECALL_CALLS) {
        r.check()?;
        let seeds: Vec<String> = batch.iter().map(|s| format!("<{s}>")).collect();
        let v = r.tool(
            "recall",
            json!({ "seeds": seeds, "hops": 0, "format": "json", "maxTriples": 1000 }),
            None,
        )?;
        let prefixes = v["prefixes"].clone();
        let graph_of = |id: &Value| -> Option<String> {
            v["citations"]
                .as_array()?
                .iter()
                .find(|c| c["id"] == *id)
                .and_then(|c| c["graph"].as_str())
                .map(|g| expand_iri(g, &prefixes))
        };
        for c in v["conflicts"].as_array().into_iter().flatten() {
            let s = expand_iri(c["s"].as_str().unwrap_or(""), &prefixes);
            let p = expand_iri(c["p"].as_str().unwrap_or(""), &prefixes);
            if !seen.insert(format!("{s} {p}")) {
                continue;
            }
            let values: Vec<Value> = c["values"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|x| json!({ "o": x["o"], "graph": graph_of(&x["citation"]) }))
                .collect();
            out.push(json!({ "s": s, "p": p, "values": values }));
        }
    }
    Ok(out)
}

/// Run one retention pass.
pub fn retention(ctx: &Ctx, req: &RetentionRequest) -> (Result<Value, Value>, Value) {
    let started = Instant::now();
    let mut r = Run {
        ctx,
        dataset: req.dataset.clone(),
        usage: Usage::default(),
    };
    let out = retention_inner(&mut r, req).map_err(|f| f.0);
    (out, r.usage.json(started))
}

fn retention_inner(r: &mut Run, req: &RetentionRequest) -> Result<Value, Failed> {
    let ctx = r.ctx;
    let st = &ctx.server.state;
    let ds = ctx
        .server
        .dataset(&ctx.principal.clone().on_branch(None), Some(&req.dataset))
        .map_err(Failed::from)?;
    if !ctx.principal.can(&ds.name, Level::Admin) {
        return Err(failed(
            "forbidden",
            403,
            "only an admin of the dataset applies retention",
        ));
    }
    let memory = memory_settings(st, &ds);
    let rule = memory.retention.clone();
    let Some(after) = req
        .after
        .clone()
        .or_else(|| rule.as_ref().map(|x| x.after.clone()))
    else {
        return Err(failed(
            "no-retention",
            400,
            "the dataset's memory settings have no retention; pass after to apply one once",
        ));
    };
    let graphs = req
        .graphs
        .clone()
        .or_else(|| rule.as_ref().map(|x| x.graphs.clone()))
        .unwrap_or_default();
    let require = req
        .require_consolidated
        .or(rule.as_ref().map(|x| x.require_consolidated))
        .unwrap_or(true);
    ctx.progress.status(
        Status::Scanning,
        0.05,
        Some("reading session graphs".into()),
    );
    let scan = r.tool(
        "memory_retention_scan",
        json!({ "after": after, "graphs": graphs, "requireConsolidated": require }),
        None,
    )?;
    let delete: Vec<String> = scan["delete"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let mut out = json!({
        "outcome": if req.dry_run { "dry-run" } else { "deleted" },
        "after": after,
        "requireConsolidated": require,
        "graphs": scan["graphs"],
        "delete": delete,
    });
    if req.dry_run || delete.is_empty() {
        if delete.is_empty() {
            out["outcome"] = "nothing-to-delete".into();
        }
        return Ok(out);
    }
    let mut deleted = Vec::new();
    let mut errors = Vec::new();
    let n = delete.len();
    for (i, g) in delete.iter().enumerate() {
        r.check()?;
        ctx.progress.status(
            Status::Writing,
            0.2 + 0.79 * i as f32 / n as f32,
            Some(format!("graph {} of {n}", i + 1)),
        );
        let u = json!({
            "update": format!("DROP SILENT GRAPH <{g}>"),
            "message": format!("Retention: delete the session graph <{g}>, older than {after}"),
        });
        match r.tool("sparql_update", u, None) {
            Ok(v) => deleted.push(json!({ "graph": g, "commit": v["commit"] })),
            Err(e) => errors.push(json!({ "graph": g, "code": e.code, "message": e.message })),
        }
    }
    out["deleted"] = deleted.into();
    if !errors.is_empty() {
        out["errors"] = errors.into();
    }
    Ok(out)
}

// --- tasks ----------------------------------------------------------------------------

/// A maintenance request.
#[derive(Clone, Debug)]
pub enum Job {
    Consolidate(ConsolidateRequest),
    Retention(RetentionRequest),
}

impl Job {
    fn kind(&self) -> &'static str {
        match self {
            Job::Consolidate(_) => "consolidation",
            Job::Retention(_) => "retention",
        }
    }

    fn dataset(&self) -> &str {
        match self {
            Job::Consolidate(c) => &c.dataset,
            Job::Retention(r) => &r.dataset,
        }
    }

    fn input(&self) -> Value {
        match self {
            Job::Consolidate(c) => json!({
                "kind": "consolidation",
                "mode": match c.mode { ConsolidationMode::Branch => "branch", ConsolidationMode::Auto => "auto" },
                "minSources": c.min_sources,
                "dryRun": c.dry_run,
                "scheduled": c.scheduled,
            }),
            Job::Retention(r) => json!({
                "kind": "retention",
                "after": r.after,
                "dryRun": r.dry_run,
                "scheduled": r.scheduled,
            }),
        }
    }
}

/// Add the task of `job` to the runtime and run it on a thread of its own.
pub fn start(
    st: &Arc<AppState>,
    p: Principal,
    job: Job,
    deadline: Duration,
    request_id: String,
) -> Result<Arc<Task>, String> {
    let task = Arc::new(Task::new(job.dataset(), &p.id(), job.input()));
    st.ingest.add(task.clone())?;
    let st2 = st.clone();
    let t = task.clone();
    let span = tracing::Span::current();
    let run = move || {
        let _e = span.enter();
        let cfg = crate::mcp::rest::config(&st2, crate::mcp::rest::Mode::Write);
        let server = crate::mcp::McpServer::new(st2.clone(), cfg);
        let ctx = Ctx {
            server: &server,
            models: None,
            principal: &p,
            progress: t.as_ref(),
            deadline: Instant::now() + deadline,
            cancel: t.cancel.clone(),
            pdf: st2.ingest.pdf.clone(),
            request_id,
        };
        let (out, usage) = match &job {
            Job::Consolidate(c) => consolidate(&ctx, c),
            Job::Retention(r) => retention(&ctx, r),
        };
        if let Err(e) = &out {
            tracing::info!(task = %t.id, kind = job.kind(), code = %e["code"], "maintenance failed");
        }
        t.finish(out, usage);
    };
    if let Err(e) = std::thread::Builder::new()
        .name("maintenance".into())
        .spawn(run)
    {
        task.finish(
            Err(json!({ "code": "internal", "message": format!("no thread for the task: {e}") })),
            json!({}),
        );
    }
    Ok(task)
}

// --- the schedule ---------------------------------------------------------------------

/// When each scheduled task of a dataset last started.
#[derive(Default, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Schedule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    consolidation: Option<LastRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retention: Option<LastRun>,
}

#[derive(Clone, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LastRun {
    started: String,
    task: String,
}

fn schedule_of(st: &AppState, ds: &crate::state::Dataset) -> Schedule {
    crate::assist::read_file(st, ds, STATE_FILE)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// The time after which a task that last started at `last` is due again.
fn next_run(last: Option<&LastRun>, every_days: f64) -> Option<chrono::DateTime<chrono::Utc>> {
    let l = chrono::DateTime::parse_from_rfc3339(&last?.started).ok()?;
    Some(l.with_timezone(&chrono::Utc) + chrono::Duration::seconds((every_days * 86_400.0) as i64))
}

/// Start the scheduled tasks that are due, as the server itself. A task of the same
/// kind that still runs for the dataset delays the next.
pub fn tick(st: &Arc<AppState>) {
    let now = chrono::Utc::now();
    for ds in st.datasets().values() {
        let memory = memory_settings(st, ds);
        let mut sched = schedule_of(st, ds);
        let mut changed = false;
        let running = |kind: &str| {
            st.ingest
                .list(&ds.name)
                .iter()
                .any(|t| t.input["kind"] == kind && t.state.lock().status.active())
        };
        if let Some(c) = &memory.consolidation
            && let Some(every) = c.every.as_deref().and_then(crate::assist::duration_days)
            && next_run(sched.consolidation.as_ref(), every).is_none_or(|n| n <= now)
            && !running("consolidation")
        {
            let job = Job::Consolidate(ConsolidateRequest {
                dataset: ds.name.clone(),
                mode: c.mode,
                min_sources: Some(c.min_sources),
                scheduled: true,
                ..Default::default()
            });
            match start(st, Principal::local(), job, DEADLINE, String::new()) {
                Ok(t) => {
                    sched.consolidation = Some(LastRun {
                        started: now.to_rfc3339(),
                        task: t.id.clone(),
                    });
                    changed = true;
                }
                Err(e) => tracing::warn!(dataset = %ds.name, "scheduled consolidation: {e}"),
            }
        }
        if let Some(rule) = &memory.retention {
            let every =
                crate::assist::duration_days(rule.every.as_deref().unwrap_or(RETENTION_EVERY))
                    .unwrap_or(1.0);
            if next_run(sched.retention.as_ref(), every).is_none_or(|n| n <= now)
                && !running("retention")
            {
                let job = Job::Retention(RetentionRequest {
                    dataset: ds.name.clone(),
                    scheduled: true,
                    ..Default::default()
                });
                match start(st, Principal::local(), job, DEADLINE, String::new()) {
                    Ok(t) => {
                        sched.retention = Some(LastRun {
                            started: now.to_rfc3339(),
                            task: t.id.clone(),
                        });
                        changed = true;
                    }
                    Err(e) => tracing::warn!(dataset = %ds.name, "scheduled retention: {e}"),
                }
            }
        }
        if changed
            && let Err(e) = crate::assist::write_file(
                st,
                ds,
                STATE_FILE,
                &serde_json::to_value(&sched).unwrap_or_default(),
            )
        {
            tracing::warn!(dataset = %ds.name, "{e:#}");
        }
    }
}

/// Look for due maintenance once a minute (from `serve`), and count what waits for
/// review in each dataset's inbox. A read-only server counts and starts nothing.
pub fn spawn_schedule(st: Arc<AppState>) {
    tokio::spawn(async move {
        let mut t = tokio::time::interval(TICK);
        loop {
            t.tick().await;
            let st = st.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if !st.read_only {
                    tick(&st);
                }
                crate::mcp::memory::pending::refresh_all(&st);
            })
            .await;
        }
    });
}

// --- routes ---------------------------------------------------------------------------

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/memory/{ds}/consolidate", post(post_consolidate))
        .route("/$/memory/{ds}/retention", post(post_retention))
        .route("/$/memory/{ds}/maintenance", get(get_maintenance))
}

fn bad(m: impl Into<String>) -> crate::http::ApiError {
    err_code(StatusCode::BAD_REQUEST, "bad-argument", m)
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConsolidateBody {
    mode: Option<ConsolidationMode>,
    min_sources: Option<u32>,
    dry_run: Option<bool>,
    message: Option<String>,
    deadline_seconds: Option<f64>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RetentionBody {
    after: Option<String>,
    graphs: Option<Vec<String>>,
    require_consolidated: Option<bool>,
    dry_run: Option<bool>,
    deadline_seconds: Option<f64>,
}

fn body<T: Default + serde::de::DeserializeOwned>(b: &[u8]) -> ApiResult<T> {
    if b.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(b).map_err(|e| bad(format!("the body: {e}")))
}

fn deadline(s: Option<f64>) -> ApiResult<Duration> {
    match s {
        Some(s) if !(1.0..=86_400.0).contains(&s) => {
            Err(bad("deadlineSeconds is between 1 and 86400"))
        }
        Some(s) => Ok(Duration::from_secs_f64(s)),
        None => Ok(DEADLINE),
    }
}

fn main_only() -> ApiResult<()> {
    if let Some(b) = crate::http::branches::current()
        && b != sparkles::branch::MAIN
    {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "invalid-branch",
            "maintenance works on the dataset's main branch",
        ));
    }
    Ok(())
}

fn accepted(ds: &str, task: &Task) -> Response {
    let loc = format!("/$/ingest/{}/{}", super::http::urlencode(ds), task.id);
    (
        StatusCode::ACCEPTED,
        [(header::LOCATION, loc)],
        Json(task.json()),
    )
        .into_response()
}

async fn post_consolidate(
    State(st): State<Arc<AppState>>,
    Path(ds): Path<String>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    AdminBody(b): AdminBody,
) -> ApiResult<Response> {
    main_only()?;
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let d = crate::http::dataset(&st, &ds)?;
    let b: ConsolidateBody = body(&b)?;
    if b.min_sources.is_some_and(|m| !(2..=100).contains(&m)) {
        return Err(bad("minSources is a number from 2 to 100"));
    }
    let mode = b.mode.unwrap_or_else(|| {
        memory_settings(&st, &d)
            .consolidation
            .map(|c| c.mode)
            .unwrap_or_default()
    });
    let job = Job::Consolidate(ConsolidateRequest {
        dataset: d.name.clone(),
        mode,
        min_sources: b.min_sources,
        dry_run: b.dry_run.unwrap_or(false),
        message: b.message,
        scheduled: false,
    });
    let task = start(
        &st,
        p,
        job,
        deadline(b.deadline_seconds)?,
        super::http::request_id(&headers),
    )
    .map_err(|m| err_code(StatusCode::TOO_MANY_REQUESTS, "too-many-tasks", m))?;
    Ok(accepted(&d.name, &task))
}

async fn post_retention(
    State(st): State<Arc<AppState>>,
    Path(ds): Path<String>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    AdminBody(b): AdminBody,
) -> ApiResult<Response> {
    main_only()?;
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let d = crate::http::dataset(&st, &ds)?;
    let b: RetentionBody = body(&b)?;
    if let Some(a) = &b.after
        && crate::assist::duration_days(a).is_none_or(|x| x < 1.0)
    {
        return Err(bad("after is a duration of at least 1d, such as 365d"));
    }
    if b.after.is_none() && memory_settings(&st, &d).retention.is_none() {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "no-retention",
            "the dataset's memory settings have no retention; pass after to apply one once",
        ));
    }
    let job = Job::Retention(RetentionRequest {
        dataset: d.name.clone(),
        after: b.after,
        graphs: b.graphs,
        require_consolidated: b.require_consolidated,
        dry_run: b.dry_run.unwrap_or(false),
        scheduled: false,
    });
    let task = start(
        &st,
        p,
        job,
        deadline(b.deadline_seconds)?,
        super::http::request_id(&headers),
    )
    .map_err(|m| err_code(StatusCode::TOO_MANY_REQUESTS, "too-many-tasks", m))?;
    Ok(accepted(&d.name, &task))
}

async fn get_maintenance(
    State(st): State<Arc<AppState>>,
    Path(ds): Path<String>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<Value>> {
    main_only()?;
    let d = crate::http::dataset(&st, &ds)?;
    let memory = memory_settings(&st, &d);
    let sched = schedule_of(&st, &d);
    let entry = |every: Option<f64>, last: Option<&LastRun>, settings: Value| {
        let mut j = json!({ "settings": settings });
        if let Some(l) = last {
            j["lastRun"] = l.started.clone().into();
            j["lastTask"] = l.task.clone().into();
        }
        if let Some(e) = every {
            j["nextRun"] = match next_run(last, e) {
                Some(n) => n.to_rfc3339().into(),
                // never run: at the next look, within a minute
                None => "due".into(),
            };
        }
        j
    };
    let c = memory.consolidation.as_ref();
    let r = memory.retention.as_ref();
    // the open review items, for a caller who may review them
    let review = {
        let (st, d) = (st.clone(), d.clone());
        crate::http::blocking(move || Ok(crate::mcp::memory::pending::review_json(&st, &d, &p)))
            .await?
    };
    let mut out = json!({
        "dataset": d.name,
        "consolidation": entry(
            c.and_then(|c| c.every.as_deref()).and_then(crate::assist::duration_days),
            sched.consolidation.as_ref(),
            serde_json::to_value(c).unwrap_or_default(),
        ),
        "retention": entry(
            r.map(|r| r.every.as_deref().unwrap_or(RETENTION_EVERY)).and_then(crate::assist::duration_days),
            sched.retention.as_ref(),
            serde_json::to_value(r).unwrap_or_default(),
        ),
    });
    if let Some(v) = review {
        out["review"] = v;
    }
    Ok(Json(out))
}
