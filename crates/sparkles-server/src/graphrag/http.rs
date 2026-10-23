//! The GraphRAG routes (spec C22 §2.2, §2.3, §6.4).
//!
//! - `POST /$/graphrag/{ds}/build` and `POST /$/graphrag/{ds}/refresh` start a run as a
//!   task in `/$/tasks`. Without `"confirm": true` the run waits for
//!   `POST /$/graphrag/{ds}/confirm`.
//! - `GET /$/graphrag/{ds}` reports the state.
//! - `GET /$/graphrag/{ds}/communities` lists the communities, and
//!   `GET /$/graphrag/{ds}/community?iri=` shows one with its report.
//! - `POST /$/graphrag/{ds}/review` sets a report's status, or replaces its title and
//!   summary with a person's edit.
//!
//! Everything derived from a reports graph is read through the caller's view bound to
//! the head snapshot, so a caller for whom the graph is not proven sees no title, member,
//! count or status of it (§6.4). Such a caller gets `available: false`.

use super::build::{self, Mode, Run, RunError};
use super::guard::gr;
use super::settings::GraphragSettings;
use super::store::{self, Status};
use crate::auth::{Endpoint, Level, Principal};
use crate::http::{AdminBody, ApiResult, blocking, dataset, err, err_code};
use crate::state::{AppState, Dataset};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use oxrdf::Term;
use serde::Deserialize;
use serde_json::{Value, json};
use sparkles::sparql::{self, QueryOptions};
use std::collections::BTreeMap;
use std::sync::Arc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/graphrag/{ds}", get(get_status))
        .route("/$/graphrag/{ds}/build", post(post_build))
        .route("/$/graphrag/{ds}/refresh", post(post_refresh))
        .route("/$/graphrag/{ds}/confirm", post(post_confirm))
        .route("/$/graphrag/{ds}/communities", get(get_communities))
        .route("/$/graphrag/{ds}/community", get(get_community))
        .route("/$/graphrag/{ds}/review", post(post_review))
}

fn bad(m: impl Into<String>) -> crate::http::ApiError {
    err_code(StatusCode::BAD_REQUEST, "bad-argument", m)
}

fn body<T: Default + serde::de::DeserializeOwned>(b: &[u8]) -> ApiResult<T> {
    if b.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(b).map_err(|e| bad(format!("the body: {e}")))
}

fn settings_of(st: &AppState, ds: &Dataset) -> GraphragSettings {
    crate::settings::effective(st, ds, &crate::settings::GRAPHRAG)
}

/// The caller's query options on `ds`.
fn caller_options(ds: &Dataset, p: &Principal) -> QueryOptions {
    let mut opts = ds.dataset.query_options();
    #[cfg(feature = "auth")]
    crate::auth::restrict(&mut opts, p, &ds.name, Endpoint::Query);
    #[cfg(not(feature = "auth"))]
    {
        opts.graphs = p.view(&ds.name, Endpoint::Query);
    }
    opts
}

/// Whether the caller may read the reports graph at the head commit (§6.3).
fn proven(ds: &Dataset, p: &Principal, graph: &str) -> bool {
    match p.view(&ds.name, Endpoint::Query) {
        None => true,
        Some(v) => v.bound(&ds.store.snapshot()).read.allows_iri(graph),
    }
}

fn rows(ds: &Dataset, opts: &QueryOptions, q: &str) -> ApiResult<Vec<Vec<Option<Term>>>> {
    sparql::query(ds.store.snapshot(), q, opts)
        .map(|r| r.rows())
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn lit(t: &Option<Term>) -> Option<String> {
    match t {
        Some(Term::Literal(l)) => Some(l.value().to_string()),
        _ => None,
    }
}

fn iri(t: &Option<Term>) -> Option<String> {
    match t {
        Some(Term::NamedNode(n)) => Some(n.as_str().to_string()),
        _ => None,
    }
}

fn int(t: &Option<Term>) -> Option<u64> {
    lit(t).and_then(|v| v.parse().ok())
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BuildBody {
    /// start without waiting for confirmation of the estimate
    #[serde(default)]
    confirm: bool,
}

async fn post_build(
    st: State<Arc<AppState>>,
    path: Path<String>,
    p: Extension<Principal>,
    b: AdminBody,
) -> ApiResult<Response> {
    start(st, path, p, b, Mode::Build).await
}

async fn post_refresh(
    st: State<Arc<AppState>>,
    path: Path<String>,
    p: Extension<Principal>,
    b: AdminBody,
) -> ApiResult<Response> {
    start(st, path, p, b, Mode::Refresh).await
}

async fn start(
    State(st): State<Arc<AppState>>,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(b): AdminBody,
    mode: Mode,
) -> ApiResult<Response> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    if !p.can(&name, Level::Admin) {
        return Err(err(StatusCode::FORBIDDEN, "dataset admin required"));
    }
    let ds = dataset(&st, &name)?;
    let b: BuildBody = body(&b)?;
    let settings = settings_of(&st, &ds);
    if !settings.enabled {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "not-enabled",
            "GraphRAG is not enabled for this dataset; set enabled in its graphrag settings",
        ));
    }
    settings
        .check()
        .map_err(|m| err_code(StatusCode::BAD_REQUEST, "invalid-settings", m))?;
    crate::http::task_start_check(&st, Some(build::TASK_KIND), &ds.name)?;
    let id = st.next_task_id();
    let task_id = id.clone();
    let st2 = st.clone();
    let ds_name = ds.name.clone();
    let task = st.start_task_opts(id, build::TASK_KIND, &ds_name, None, true, move |h| {
        let cancel = h.cancel_flag();
        let r = Run {
            st: &st2,
            ds: ds.clone(),
            settings,
            mode,
            confirmed: b.confirm,
            task: Some(h),
            task_id,
            cancel: cancel.clone(),
        };
        match build::run(&r) {
            Ok(o) => {
                h.set_detail(serde_json::to_value(&o).unwrap_or(Value::Null));
                Ok(match &o.stopped {
                    Some(why) => format!(
                        "{} reports written, {} pending: {why}",
                        o.written, o.pending
                    ),
                    None => format!(
                        "{} reports written, {} kept, {} failed",
                        o.written, o.kept, o.failed
                    ),
                })
            }
            Err(RunError::Cancelled) => Err(sparkles::Error::Cancelled.into()),
            Err(e) => Err(anyhow::anyhow!(e.to_string())),
        }
    });
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConfirmBody {
    task: String,
    #[serde(default = "yes")]
    confirm: bool,
}

fn yes() -> bool {
    true
}

async fn post_confirm(
    State(st): State<Arc<AppState>>,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(b): AdminBody,
) -> ApiResult<Response> {
    if !p.can(&name, Level::Admin) {
        return Err(err(StatusCode::FORBIDDEN, "dataset admin required"));
    }
    let ds = dataset(&st, &name)?;
    let b: ConfirmBody = body(&b)?;
    if b.task.is_empty() {
        return Err(bad("task names the waiting build"));
    }
    if !st.graphrag.answer(&ds.name, &b.task, b.confirm) {
        return Err(err_code(
            StatusCode::CONFLICT,
            "not-waiting",
            "no build of this dataset waits for confirmation under that task",
        ));
    }
    Ok(Json(json!({ "task": b.task, "confirmed": b.confirm })).into_response())
}

/// The reports of a graph by status, as the caller reads them.
fn statuses(ds: &Dataset, opts: &QueryOptions, graph: &str) -> ApiResult<BTreeMap<String, u64>> {
    let q = format!(
        "SELECT ?s (COUNT(?r) AS ?n) WHERE {{ GRAPH <{graph}> {{ ?c <{}> ?r . ?r <{}> ?s }} }} GROUP BY ?s",
        gr("report"),
        gr("status")
    );
    let mut out: BTreeMap<String, u64> = Status::ALL
        .iter()
        .map(|s| (s.as_str().to_string(), 0))
        .collect();
    for r in rows(ds, opts, &q)? {
        if let (Some(s), Some(n)) = (lit(&r[0]), int(&r[1])) {
            out.insert(s, n);
        }
    }
    Ok(out)
}

async fn get_status(
    State(st): State<Arc<AppState>>,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Response> {
    let ds = dataset(&st, &name)?;
    let st2 = st.clone();
    let v = blocking(move || {
        let s = settings_of(&st2, &ds);
        let graph = s.reports_graph.clone();
        let mut out = json!({
            "dataset": ds.name,
            "enabled": s.enabled,
            "reportsGraph": graph,
            "refresh": s.refresh.mode,
        });
        if !proven(&ds, &p, &graph) {
            out["available"] = false.into();
            out["reason"] = "the reports are not available to this caller, whose access does not cover everything the build read".into();
            return Ok(out);
        }
        out["available"] = true.into();
        let opts = caller_options(&ds, &p);
        out["reports"] = json!(statuses(&ds, &opts, &graph)?);
        let q = format!(
            "SELECT ?l (COUNT(?c) AS ?n) WHERE {{ GRAPH <{graph}> {{ ?c a <{}> ; <{}> ?l }} }} GROUP BY ?l ORDER BY ?l",
            gr("Community"),
            gr("level")
        );
        let mut levels = Vec::new();
        for r in rows(&ds, &opts, &q)? {
            if let (Some(l), Some(n)) = (int(&r[0]), int(&r[1])) {
                levels.push(json!({ "level": l, "communities": n }));
            }
        }
        out["levels"] = levels.into();
        let state = match ds.store.root() {
            Some(db) => super::state::BuildState::load(db).ok(),
            None => Some(st2.graphrag.state(&ds.name)),
        };
        if let Some(b) = state.and_then(|s| s.last_build) {
            out["lastBuild"] = json!({
                "sourceCommit": b.source_commit,
                "branch": b.branch,
                "finishedAt": b.finished_at,
                "tokens": b.tokens,
                "pending": b.pending.len(),
            });
        }
        if p.can(&ds.name, Level::Admin) {
            let (day, anomalies) = match ds.store.root() {
                Some(db) => match super::ledger::DatasetLedger::open(db) {
                    Ok(l) => (*l.day.lock(), l.anomalies()),
                    Err(_) => (Default::default(), Vec::new()),
                },
                None => (*st2.graphrag.day(&ds.name).lock(), Vec::new()),
            };
            let used = if day.day == build::today() { day.used } else { 0 };
            out["budget"] = json!({
                "tokensPerBuild": s.budget.tokens_per_build,
                "tokensPerDay": s.budget.tokens_per_day,
                "usedToday": used,
            });
            out["anomalies"] = serde_json::to_value(anomalies).unwrap_or_default();
        }
        Ok(out)
    })
    .await?;
    Ok(Json(v).into_response())
}

async fn get_communities(
    State(st): State<Arc<AppState>>,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Response> {
    let ds = dataset(&st, &name)?;
    let st2 = st.clone();
    let v = blocking(move || {
        let s = settings_of(&st2, &ds);
        let graph = s.reports_graph;
        if !proven(&ds, &p, &graph) {
            return Ok(json!({ "available": false, "communities": [] }));
        }
        let opts = caller_options(&ds, &p);
        let q = format!(
            "SELECT ?c ?l ?parent ?n ?r ?title ?status ?rating WHERE {{ GRAPH <{graph}> {{ ?c a <{community}> ; <{level}> ?l ; <{count}> ?n . OPTIONAL {{ ?c <{broader}> ?parent }} OPTIONAL {{ ?c <{report}> ?r . OPTIONAL {{ ?r <{label}> ?title }} OPTIONAL {{ ?r <{status}> ?status }} OPTIONAL {{ ?r <{rating}> ?rating }} }} }} }} ORDER BY ?l DESC(?rating) ?c LIMIT 100000",
            community = gr("Community"),
            level = gr("level"),
            count = gr("memberCount"),
            broader = store::SKOS_BROADER,
            report = gr("report"),
            label = store::RDFS_LABEL,
            status = gr("status"),
            rating = gr("rating"),
        );
        let list: Vec<Value> = rows(&ds, &opts, &q)?
            .into_iter()
            .map(|r| {
                json!({
                    "iri": iri(&r[0]),
                    "level": int(&r[1]),
                    "parent": iri(&r[2]),
                    "memberCount": int(&r[3]),
                    "report": iri(&r[4]).map(|rep| json!({
                        "iri": rep,
                        "title": lit(&r[5]),
                        "status": lit(&r[6]),
                        "rating": int(&r[7]),
                    })),
                })
            })
            .collect();
        Ok(json!({ "available": true, "reportsGraph": graph, "communities": list }))
    })
    .await?;
    Ok(Json(v).into_response())
}

#[derive(Deserialize)]
struct CommunityQuery {
    iri: String,
}

/// The most members listed for one community.
const MAX_MEMBERS: usize = 500;

async fn get_community(
    State(st): State<Arc<AppState>>,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    Query(q): Query<CommunityQuery>,
) -> ApiResult<Response> {
    let ds = dataset(&st, &name)?;
    let c = oxrdf::NamedNode::new(q.iri.as_str()).map_err(|_| bad("iri is not an IRI"))?;
    let st2 = st.clone();
    let v = blocking(move || {
        let s = settings_of(&st2, &ds);
        let graph = s.reports_graph;
        let none = || err(StatusCode::NOT_FOUND, "no such community");
        if !proven(&ds, &p, &graph) {
            return Err(none());
        }
        let opts = caller_options(&ds, &p);
        let head = rows(
            &ds,
            &opts,
            &format!(
                "SELECT ?l ?parent ?n ?f ?r WHERE {{ GRAPH <{graph}> {{ {c} a <{}> ; <{}> ?l ; <{}> ?n ; <{}> ?f . OPTIONAL {{ {c} <{}> ?parent }} OPTIONAL {{ {c} <{}> ?r }} }} }}",
                gr("Community"),
                gr("level"),
                gr("memberCount"),
                gr("factCount"),
                store::SKOS_BROADER,
                gr("report"),
            ),
        )?;
        let Some(h) = head.into_iter().next() else {
            return Err(none());
        };
        let mut out = json!({
            "iri": c.as_str(),
            "level": int(&h[0]),
            "parent": iri(&h[1]),
            "memberCount": int(&h[2]),
            "factCount": int(&h[3]),
        });
        let members: Vec<Value> = rows(
            &ds,
            &opts,
            &format!(
                "SELECT ?m (SAMPLE(?label) AS ?l) WHERE {{ GRAPH <{graph}> {{ {c} <{}> ?m }} OPTIONAL {{ GRAPH ?g {{ ?m <{}> ?label }} }} }} GROUP BY ?m ORDER BY ?m LIMIT {MAX_MEMBERS}",
                gr("member"),
                store::RDFS_LABEL,
            ),
        )?
        .into_iter()
        .map(|r| json!({ "iri": iri(&r[0]), "label": lit(&r[1]) }))
        .collect();
        out["members"] = members.into();
        let children: Vec<Value> = rows(
            &ds,
            &opts,
            &format!(
                "SELECT ?k ?title WHERE {{ GRAPH <{graph}> {{ ?k <{}> {c} . OPTIONAL {{ ?k <{}> ?r . ?r <{}> ?title }} }} }} ORDER BY ?k",
                store::SKOS_BROADER,
                gr("report"),
                store::RDFS_LABEL,
            ),
        )?
        .into_iter()
        .map(|r| json!({ "iri": iri(&r[0]), "title": lit(&r[1]) }))
        .collect();
        out["children"] = children.into();
        if let Some(rep) = iri(&h[4]) {
            out["report"] = report_json(&ds, &opts, &graph, &rep)?;
        }
        Ok(out)
    })
    .await?;
    Ok(Json(v).into_response())
}

/// A report with its findings, citations and provenance.
fn report_json(ds: &Dataset, opts: &QueryOptions, graph: &str, rep: &str) -> ApiResult<Value> {
    let r = rows(
        ds,
        opts,
        &format!(
            "SELECT ?title ?summary ?status ?rating ?failure ?model ?in ?out ?at ?prev ?dc ?du ?commit WHERE {{ GRAPH <{graph}> {{ <{rep}> <{status}> ?status . OPTIONAL {{ <{rep}> <{label}> ?title }} OPTIONAL {{ <{rep}> <{summary}> ?summary }} OPTIONAL {{ <{rep}> <{rating}> ?rating }} OPTIONAL {{ <{rep}> <{failure}> ?failure }} OPTIONAL {{ <{rep}> <{prov}wasGeneratedBy> ?a . ?a <{spk}model> ?model ; <{spk}inputTokens> ?in ; <{spk}outputTokens> ?out ; <{prov}startedAtTime> ?at }} OPTIONAL {{ <{rep}> <{prov}wasRevisionOf> ?prev }} OPTIONAL {{ <{rep}> <{dc}> ?dc }} OPTIONAL {{ <{rep}> <{du}> ?du }} OPTIONAL {{ <{rep}> <{sc}> ?commit }} }} }}",
            status = gr("status"),
            label = store::RDFS_LABEL,
            summary = gr("summary"),
            rating = gr("rating"),
            failure = gr("failure"),
            prov = store::PROV,
            spk = store::SPK,
            dc = gr("droppedCitations"),
            du = gr("droppedUnsupported"),
            sc = gr("sourceCommit"),
        ),
    )?;
    let Some(r) = r.into_iter().next() else {
        return Ok(Value::Null);
    };
    let mut out = json!({
        "iri": rep,
        "title": lit(&r[0]),
        "summary": lit(&r[1]),
        "status": lit(&r[2]),
        "rating": int(&r[3]),
        "failure": lit(&r[4]),
        "revisionOf": iri(&r[9]),
        "droppedCitations": int(&r[10]),
        "droppedUnsupported": int(&r[11]),
        "sourceCommit": int(&r[12]),
    });
    if lit(&r[5]).is_some() {
        out["activity"] = json!({
            "model": lit(&r[5]),
            "inputTokens": int(&r[6]),
            "outputTokens": int(&r[7]),
            "startedAt": lit(&r[8]),
        });
    }
    let mut findings: BTreeMap<u64, Value> = BTreeMap::new();
    for f in rows(
        ds,
        opts,
        &format!(
            "SELECT ?n ?text ?p ?o WHERE {{ GRAPH <{graph}> {{ <{rep}> <{}> ?f . ?f <{}> ?n ; <{}> ?text . OPTIONAL {{ ?f ?p ?o FILTER(?p IN (<{}>, <{}>, <{}>)) }} }} }}",
            gr("finding"),
            gr("order"),
            gr("text"),
            gr("about"),
            gr("cites"),
            gr("unmatchedName"),
        ),
    )? {
        let (Some(n), Some(text)) = (int(&f[0]), lit(&f[1])) else {
            continue;
        };
        let e = findings.entry(n).or_insert_with(
            || json!({ "text": text, "about": [], "cites": [], "unmatchedNames": [] }),
        );
        let key = match iri(&f[2]).as_deref() {
            Some(p) if p == gr("about") => "about",
            Some(p) if p == gr("cites") => "cites",
            Some(p) if p == gr("unmatchedName") => "unmatchedNames",
            _ => continue,
        };
        let v = match &f[3] {
            Some(Term::Literal(l)) => Value::String(l.value().to_string()),
            Some(t) => Value::String(t.to_string()),
            None => continue,
        };
        if let Some(a) = e[key].as_array_mut()
            && !a.contains(&v)
        {
            a.push(v);
        }
    }
    out["findings"] = findings.into_values().collect::<Vec<_>>().into();
    Ok(out)
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReviewBody {
    report: String,
    /// `reviewed` or `rejected`
    #[serde(default)]
    status: Option<String>,
    /// a person's edit: a new report that supersedes this one, `reviewed`
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
}

/// The longest edited title and summary, in bytes.
const MAX_EDIT_BYTES: usize = 16 * 1024;

async fn post_review(
    State(st): State<Arc<AppState>>,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(b): AdminBody,
) -> ApiResult<Response> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    if !p.can(&name, Level::Write) {
        return Err(err(
            StatusCode::FORBIDDEN,
            "write access to the dataset required",
        ));
    }
    let ds = dataset(&st, &name)?;
    let b: ReviewBody = body(&b)?;
    let rep = oxrdf::NamedNode::new(b.report.as_str()).map_err(|_| bad("report is not an IRI"))?;
    let edit = b.title.is_some() || b.summary.is_some();
    let status = match b.status.as_deref() {
        None if edit => Status::Reviewed,
        None => {
            return Err(bad(
                "status is reviewed or rejected, or the body edits the report",
            ));
        }
        Some(s) => match Status::parse(s) {
            Some(s @ (Status::Reviewed | Status::Rejected)) => s,
            _ => return Err(bad("status is reviewed or rejected")),
        },
    };
    if edit && status != Status::Reviewed {
        return Err(bad("an edited report is reviewed"));
    }
    for t in [&b.title, &b.summary].into_iter().flatten() {
        if t.trim().is_empty() || t.len() > MAX_EDIT_BYTES {
            return Err(bad(
                "an edited title or summary is between 1 and 16384 bytes",
            ));
        }
    }
    let author = crate::http::author(&p);
    let st2 = st.clone();
    let v = blocking(move || {
        let s = settings_of(&st2, &ds);
        let graph = s.reports_graph.clone();
        let none = || err(StatusCode::NOT_FOUND, "no such report");
        if !proven(&ds, &p, &graph) {
            return Err(none());
        }
        // reviews and builds of the dataset commit one at a time, and the report is read
        // under the same lock
        let lock = st2.graphrag.lock(&ds.name);
        let _held = lock.lock();
        // the report as the caller reads it, and its community
        let opts = caller_options(&ds, &p);
        let found = rows(
            &ds,
            &opts,
            &format!(
                "SELECT ?c ?lang WHERE {{ GRAPH <{graph}> {{ ?c <{}> {rep} . OPTIONAL {{ {rep} <{}> ?summary BIND(LANG(?summary) AS ?lang) }} }} }}",
                gr("report"),
                gr("summary"),
            ),
        )?;
        let Some(f) = found.into_iter().next() else {
            return Err(none());
        };
        let community = iri(&f[0]).ok_or_else(none)?;
        let text = if edit {
            let lang = lit(&f[1]).filter(|l| !l.is_empty()).unwrap_or(s.language.clone());
            revision_text(&ds, &opts, &graph, rep.as_str(), &community, &lang, b.title.as_deref(), b.summary.as_deref())?
        } else {
            format!(
                "DELETE {{ GRAPH <{graph}> {{ {rep} <{status}> ?s }} }} INSERT {{ GRAPH <{graph}> {{ {rep} <{status}> \"{new}\" }} }} WHERE {{ GRAPH <{graph}> {{ {rep} <{status}> ?s }} }}",
                status = gr("status"),
                new = status.as_str(),
            )
        };
        let o = QueryOptions {
            write: sparkles::guard::WriteOptions {
                message: Some("GraphRAG review".into()),
                author: author.clone(),
                ..Default::default()
            },
            ..Default::default()
        };
        let stats =
            sparkles::sparql::update::update_as(&ds.store, &text, &o, sparkles::commit::CommitKind::Graphrag)
                .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let counts = statuses(&ds, &QueryOptions::default(), &graph)?;
        st2.graphrag.update(&ds.name, |f| {
            f.reports = Status::ALL
                .iter()
                .map(|s| (s.as_str(), counts.get(s.as_str()).copied().unwrap_or(0)))
                .collect();
        });
        Ok(json!({
            "report": b.report,
            "status": status.as_str(),
            "commit": stats.commit.map(|c| c.commit.seq),
        }))
    })
    .await?;
    Ok(Json(v).into_response())
}

/// The update that supersedes report `old` with a person's edit (§4). The new report
/// copies the old one's triples and those of its findings under new IRIs, with the
/// edited title or summary, status `reviewed` and `prov:wasRevisionOf` the old report.
/// It has no activity, since the commit's author is the person who wrote it. The old
/// report keeps its triples, and the community links to the new one.
#[allow(clippy::too_many_arguments)]
fn revision_text(
    ds: &Dataset,
    opts: &QueryOptions,
    graph: &str,
    old: &str,
    community: &str,
    lang: &str,
    title: Option<&str>,
    summary: Option<&str>,
) -> ApiResult<String> {
    use oxrdf::{Literal, NamedNode, NamedOrBlankNode, Triple};
    let new = store::new_report_iri();
    let moved = |t: &str| -> Option<String> {
        if t == old {
            Some(new.clone())
        } else {
            t.strip_prefix(old)
                .filter(|r| r.starts_with('#'))
                .map(|r| format!("{new}{r}"))
        }
    };
    let found = rows(
        ds,
        opts,
        &format!(
            "SELECT ?s ?p ?o WHERE {{ GRAPH <{graph}> {{ ?s ?p ?o FILTER(?s = <{old}> || STRSTARTS(STR(?s), \"{old}#\")) }} }}"
        ),
    )?;
    let skip = [
        store::RDFS_LABEL.to_string(),
        gr("summary"),
        gr("status"),
        format!("{}wasRevisionOf", store::PROV),
        format!("{}wasGeneratedBy", store::PROV),
    ];
    let activity = store::activity_iri(old);
    let mut triples: Vec<Triple> = Vec::new();
    let mut old_title = None;
    let mut old_summary = None;
    for r in found {
        let (Some(Term::NamedNode(s)), Some(Term::NamedNode(p)), Some(o)) =
            (r[0].clone(), r[1].clone(), r[2].clone())
        else {
            continue;
        };
        if s.as_str() == activity {
            continue;
        }
        if s.as_str() == old {
            if p.as_str() == store::RDFS_LABEL {
                old_title = lit(&Some(o.clone()));
            } else if p.as_str() == gr("summary") {
                old_summary = lit(&Some(o.clone()));
            }
            if skip.iter().any(|k| k == p.as_str()) {
                continue;
            }
        }
        let subject = moved(s.as_str()).unwrap_or_else(|| s.as_str().to_string());
        let object = match &o {
            Term::NamedNode(n) => match moved(n.as_str()) {
                Some(m) => Term::NamedNode(NamedNode::new_unchecked(m)),
                None => o,
            },
            _ => o,
        };
        triples.push(Triple::new(
            NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(subject)),
            p,
            object,
        ));
    }
    let text = |v: &str| {
        Term::Literal(
            Literal::new_language_tagged_literal(v, lang)
                .unwrap_or_else(|_| Literal::new_simple_literal(v)),
        )
    };
    let id = || NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(new.as_str()));
    let pred = |p: &str| NamedNode::new_unchecked(p);
    if let Some(t) = title.map(str::to_string).or(old_title) {
        triples.push(Triple::new(id(), pred(store::RDFS_LABEL), text(&t)));
    }
    if let Some(t) = summary.map(str::to_string).or(old_summary) {
        triples.push(Triple::new(id(), pred(&gr("summary")), text(&t)));
    }
    triples.push(Triple::new(
        id(),
        pred(&gr("status")),
        Term::Literal(Literal::new_simple_literal(Status::Reviewed.as_str())),
    ));
    triples.push(Triple::new(
        id(),
        pred(&format!("{}wasRevisionOf", store::PROV)),
        Term::NamedNode(NamedNode::new_unchecked(old)),
    ));
    triples.push(Triple::new(
        NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(community)),
        pred(&gr("report")),
        Term::NamedNode(NamedNode::new_unchecked(new.as_str())),
    ));
    let body: Vec<String> = triples
        .iter()
        .map(|t| format!("{} {} {} .", t.subject, t.predicate, t.object))
        .collect();
    Ok(format!(
        "DELETE DATA {{ GRAPH <{graph}> {{ <{community}> <{report}> <{old}> }} }} ;\nINSERT DATA {{ GRAPH <{graph}> {{\n{}\n}} }}",
        body.join("\n"),
        report = gr("report"),
    ))
}

#[cfg(test)]
mod tests;
