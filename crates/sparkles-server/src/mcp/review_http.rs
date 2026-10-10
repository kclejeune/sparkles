//! The review routes of C18 §7.10, §8.9 and §10: the inbox, the review of one branch,
//! the reviewer's actions, and the ingest profiles.
//!
//! - `GET /$/memory/{ds}/inbox` lists unreviewed session facts with their signals and
//!   the open review branches.
//! - `GET /$/memory/{ds}/review/{name}` lists what a branch proposes and retracts,
//!   its new entities and the text of the sources it cites.
//! - `POST /$/memory/{ds}/promote`, `/reject`, `/relink` and `/edit` are **Promote
//!   selected**, **Reject**, **Use existing** and **Edit value**.
//! - `GET /$/ingest/{ds}/profiles`, `PUT /$/ingest/{ds}/settings` and `GET`, `PUT` and
//!   `DELETE /$/ingest/{ds}/profiles/{name}` keep the ingest settings of §7.4.
//!
//! The memory routes need `read` on the dataset. Each action writes through
//! `assert_facts` or the guard as the request's principal, so the grants on the graphs
//! and branches it touches decide, as they do for an agent.

use super::memory::ingest::{INGEST_FILE, IngestSettings, ProfileSpec, ingest_settings};
use super::rest::{Mode, config, tool_error};
use super::{Call, McpServer, Outcome};
use crate::auth::Principal;
use crate::http::{AdminBody, ApiResult, blocking, err, err_code};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/memory/{ds}/inbox", get(inbox))
        .route("/$/memory/{ds}/review/{name}", get(review))
        .route("/$/memory/{ds}/promote", post(promote))
        .route("/$/memory/{ds}/reject", post(reject))
        .route("/$/memory/{ds}/relink", post(relink))
        .route("/$/memory/{ds}/edit", post(edit))
        .route("/$/ingest/{ds}/profiles", get(list_profiles))
        .route("/$/ingest/{ds}/settings", put(put_settings))
        .route(
            "/$/ingest/{ds}/profiles/{name}",
            get(get_profile).put(put_profile).delete(delete_profile),
        )
}

/// Run the `Tools` method `tool` with `args` and `{ds}` as the request's principal.
async fn run(
    st: Arc<AppState>,
    p: Principal,
    headers: HeaderMap,
    ds: String,
    tool: &'static str,
    mut args: Map<String, Value>,
    write: bool,
) -> ApiResult<Response> {
    if let Some(b) = crate::http::branches::current()
        && b != sparkles::branch::MAIN
    {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "invalid-branch",
            "name the branch in the path or body, not with ?branch=",
        ));
    }
    if write && st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    if args.contains_key("dataset") {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "bad-argument",
            "the path names the dataset; leave dataset out of the body",
        ));
    }
    args.insert("dataset".into(), ds.into());
    let request_id = headers
        .get(&crate::obs::X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let call = Call {
        arrived: Instant::now(),
        cancel: Arc::new(AtomicBool::new(false)),
        request_id,
        principal: p,
        headers: Some(headers),
        held: None,
    };
    let mode = if write { Mode::Write } else { Mode::Internal };
    let server = McpServer::new(st.clone(), config(&st, mode));
    let out = blocking(move || Ok(server.run_now(tool, args, &call))).await?;
    match out {
        Err(e) => Err(tool_error(&e)),
        Ok(Outcome::Structured(v)) => Ok(Json(v).into_response()),
        Ok(Outcome::Text(t)) => {
            Ok(([(axum::http::header::CONTENT_TYPE, "application/json")], t).into_response())
        }
    }
}

fn body_object(body: &[u8]) -> ApiResult<Map<String, Value>> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice(body) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(err_code(
            StatusCode::BAD_REQUEST,
            "bad-argument",
            "the body must be a JSON object",
        )),
        Err(e) => Err(err_code(
            StatusCode::BAD_REQUEST,
            "bad-argument",
            format!("the body is not JSON: {e}"),
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ListQuery {
    limit: Option<u64>,
    timeout_seconds: Option<f64>,
    /// `?branch=` is read by the branch layer; accepted here so it is not unknown
    branch: Option<String>,
}

fn query_args(q: &ListQuery) -> Map<String, Value> {
    let mut m = Map::new();
    if let Some(l) = q.limit {
        m.insert("limit".into(), l.into());
    }
    if let Some(t) = q.timeout_seconds {
        m.insert("timeoutSeconds".into(), t.into());
    }
    let _ = &q.branch;
    m
}

async fn inbox(
    State(st): State<Arc<AppState>>,
    Path(ds): Path<String>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> ApiResult<Response> {
    run(st, p, headers, ds, "memory_inbox", query_args(&q), false).await
}

async fn review(
    State(st): State<Arc<AppState>>,
    Path((ds, branch)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> ApiResult<Response> {
    let mut args = query_args(&q);
    args.insert("branch".into(), branch.into());
    run(st, p, headers, ds, "memory_review", args, false).await
}

macro_rules! action {
    ($name:ident, $tool:literal) => {
        async fn $name(
            State(st): State<Arc<AppState>>,
            Path(ds): Path<String>,
            Extension(p): Extension<Principal>,
            headers: HeaderMap,
            AdminBody(body): AdminBody,
        ) -> ApiResult<Response> {
            let args = body_object(&body)?;
            run(st, p, headers, ds, $tool, args, true).await
        }
    };
}

action!(promote, "memory_promote");
action!(reject, "memory_reject");
action!(relink, "memory_relink");
action!(edit, "memory_edit");

// --- ingest settings ----------------------------------------------------------------

/// Serializes the read-modify-write of ingest settings.
static INGEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn store(st: &AppState, ds: &crate::state::Dataset, s: &IngestSettings) -> ApiResult<()> {
    s.validate().map_err(|m| err(StatusCode::BAD_REQUEST, m))?;
    let v = serde_json::to_value(s).unwrap_or_default();
    crate::assist::write_file(st, ds, INGEST_FILE, &v)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

fn main_dataset(st: &AppState, name: &str) -> ApiResult<Arc<crate::state::Dataset>> {
    if let Some(b) = crate::http::branches::current()
        && b != sparkles::branch::MAIN
    {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "invalid-branch",
            "ingest settings belong to the dataset, not to a branch",
        ));
    }
    crate::http::dataset(st, name)
}

async fn list_profiles(
    State(st): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    let ds = main_dataset(&st, &name)?;
    let s = ingest_settings(&st, &ds);
    Ok(Json(json!({
        "dataset": ds.name,
        "keepText": s.keep_text,
        "profiles": s.profiles,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SettingsBody {
    keep_text: bool,
}

async fn put_settings(
    State(st): State<Arc<AppState>>,
    Path(name): Path<String>,
    AdminBody(body): AdminBody,
) -> ApiResult<Json<Value>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = main_dataset(&st, &name)?;
    let b: SettingsBody =
        serde_json::from_slice(&body).map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    blocking(move || {
        let _g = INGEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = ingest_settings(&st, &ds);
        s.keep_text = b.keep_text;
        store(&st, &ds, &s)?;
        Ok(Json(json!({"dataset": ds.name, "keepText": s.keep_text})))
    })
    .await
}

fn profile_name(n: &str) -> ApiResult<()> {
    if super::memory::ingest::valid_profile_name(n) {
        Ok(())
    } else {
        Err(err(
            StatusCode::BAD_REQUEST,
            format!("invalid profile name {n:?}: use 1 to 64 letters, digits, _, . or -"),
        ))
    }
}

async fn get_profile(
    State(st): State<Arc<AppState>>,
    Path((name, profile)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let ds = main_dataset(&st, &name)?;
    profile_name(&profile)?;
    let s = ingest_settings(&st, &ds);
    match s.profiles.get(&profile) {
        Some(p) => Ok(Json(serde_json::to_value(p).unwrap_or_default())),
        None if profile == "default" => Ok(Json(json!({}))),
        None => Err(err(
            StatusCode::NOT_FOUND,
            format!("no profile {profile} in dataset {}", ds.name),
        )),
    }
}

async fn put_profile(
    State(st): State<Arc<AppState>>,
    Path((name, profile)): Path<(String, String)>,
    AdminBody(body): AdminBody,
) -> ApiResult<Json<Value>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = main_dataset(&st, &name)?;
    profile_name(&profile)?;
    let spec: ProfileSpec =
        serde_json::from_slice(&body).map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    spec.validate()
        .map_err(|m| err(StatusCode::BAD_REQUEST, format!("profile {profile}: {m}")))?;
    blocking(move || {
        let _g = INGEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = ingest_settings(&st, &ds);
        s.profiles.insert(profile, spec.clone());
        store(&st, &ds, &s)?;
        Ok(Json(serde_json::to_value(&spec).unwrap_or_default()))
    })
    .await
}

async fn delete_profile(
    State(st): State<Arc<AppState>>,
    Path((name, profile)): Path<(String, String)>,
) -> ApiResult<Response> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = main_dataset(&st, &name)?;
    profile_name(&profile)?;
    blocking(move || {
        let _g = INGEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = ingest_settings(&st, &ds);
        if s.profiles.remove(&profile).is_none() {
            return Err(err(
                StatusCode::NOT_FOUND,
                format!("no profile {profile} in dataset {}", ds.name),
            ));
        }
        store(&st, &ds, &s)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}
