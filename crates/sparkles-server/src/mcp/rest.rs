//! The memory tools over plain HTTP (C18 §10): `POST /{ds}/check` (`check_query`),
//! `POST /{ds}/recall` (`recall` in its JSON format) and `POST /{ds}/sparql/diagnose`
//! (`why_empty`).
//!
//! Each route runs the MCP tool itself, as the request's principal and over the
//! principal's view, with the limits the MCP endpoint would apply, so HTTP and MCP keep
//! one code path. The JSON body is the tool's arguments without `dataset`, which the
//! path names. A tool error is answered with its HTTP status and `{error, code,
//! hint?}`. The tools read `main` only, so another branch is refused.
//!
//! Phase 3m-a adds `POST /{ds}/facts` (`assert_facts`, which may name a `branch` in its
//! body as the tool does) and `POST /{ds}/memory/brief` (the brief of §8.10.9, a
//! `Tools` method that MCP does not offer as a tool). Phase 3m-b adds `POST /{ds}/sources`
//! (`register_source`) and `GET /{ds}/sources` (`list_sources`, with its arguments in
//! the query string).

use super::{Call, McpConfig, McpServer, Outcome};
use crate::auth::Principal;
use crate::http::{AdminBody, ApiResult, err_body, err_code};
use crate::state::AppState;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Router};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/{ds}/check", post(check))
        .route("/{ds}/recall", post(recall))
        .route("/{ds}/sparql/diagnose", post(diagnose))
        .route("/{ds}/facts", post(facts))
        .route("/{ds}/memory/brief", post(brief))
        .route("/{ds}/sources", post(register).get(sources))
}

/// How a route runs its tool.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// an offered read tool
    Read,
    /// `assert_facts`: the route's own grants decide, not `--mcp-allow-update`
    Write,
    /// a `Tools` method that MCP does not offer
    Internal,
}

/// The tools' limits over HTTP: those of `/$/mcp` when the server runs it, else the
/// server's own query limits.
pub(crate) fn config(st: &AppState, mode: Mode) -> McpConfig {
    let mut cfg = match &st.mcp {
        Some(conf) => conf.cfg.clone(),
        None => McpConfig {
            max_timeout: st.default_timeout,
            query_memory_bytes: st.limits.query_memory_bytes,
            ..McpConfig::default()
        },
    };
    // grants decide what a caller sees here; the MCP endpoint's own dataset and tool
    // switches do not apply to these routes
    cfg.datasets.clear();
    cfg.disabled.clear();
    // `POST /{ds}/facts` needs `write` on the graphs it writes, which the tool checks,
    // and a read-only server writes nothing
    cfg.allow_update = mode == Mode::Write && !st.read_only;
    cfg.allow_service = false;
    cfg.stored_queries = false;
    cfg.http = true;
    cfg
}

async fn check(
    st: State<Arc<AppState>>,
    ds: Path<String>,
    p: Extension<Principal>,
    headers: HeaderMap,
    body: AdminBody,
) -> ApiResult<Response> {
    run("check_query", st, ds, p, headers, body, None, Mode::Read).await
}

async fn facts(
    st: State<Arc<AppState>>,
    ds: Path<String>,
    p: Extension<Principal>,
    headers: HeaderMap,
    body: AdminBody,
) -> ApiResult<Response> {
    run("assert_facts", st, ds, p, headers, body, None, Mode::Write).await
}

async fn register(
    st: State<Arc<AppState>>,
    ds: Path<String>,
    p: Extension<Principal>,
    headers: HeaderMap,
    body: AdminBody,
) -> ApiResult<Response> {
    run(
        "register_source",
        st,
        ds,
        p,
        headers,
        body,
        None,
        Mode::Write,
    )
    .await
}

/// `GET /{ds}/sources?graph=…&graphPrefix=…&needsExtraction=true&limit=N`: the query
/// string as `list_sources`' arguments.
async fn sources(
    st: State<Arc<AppState>>,
    ds: Path<String>,
    p: Extension<Principal>,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
) -> ApiResult<Response> {
    let mut args = Map::new();
    let mut graphs: Vec<Value> = Vec::new();
    for (k, v) in form_urlencoded::parse(q.unwrap_or_default().as_bytes()) {
        let bad = |what: &str| {
            err_code(
                StatusCode::BAD_REQUEST,
                "bad-argument",
                format!("{k}: {what}"),
            )
        };
        match k.as_ref() {
            "graph" => graphs.push(v.into_owned().into()),
            "graphPrefix" => {
                args.insert("graphPrefix".into(), v.into_owned().into());
            }
            "needsExtraction" => {
                let b = match v.as_ref() {
                    "true" | "1" => true,
                    "false" | "0" => false,
                    _ => return Err(bad("true or false")),
                };
                args.insert("needsExtraction".into(), b.into());
            }
            "limit" | "atCommit" => {
                let n: u64 = v.parse().map_err(|_| bad("a whole number"))?;
                args.insert(k.clone().into_owned(), n.into());
            }
            _ => return Err(bad("unknown parameter")),
        }
    }
    if !graphs.is_empty() {
        args.insert("graphs".into(), graphs.into());
    }
    let body = serde_json::to_vec(&Value::Object(args)).unwrap_or_default();
    run(
        "list_sources",
        st,
        ds,
        p,
        headers,
        AdminBody(body.into()),
        None,
        Mode::Read,
    )
    .await
}

async fn brief(
    st: State<Arc<AppState>>,
    ds: Path<String>,
    p: Extension<Principal>,
    headers: HeaderMap,
    body: AdminBody,
) -> ApiResult<Response> {
    run(
        "memory_brief",
        st,
        ds,
        p,
        headers,
        body,
        None,
        Mode::Internal,
    )
    .await
}

async fn recall(
    st: State<Arc<AppState>>,
    ds: Path<String>,
    p: Extension<Principal>,
    headers: HeaderMap,
    body: AdminBody,
) -> ApiResult<Response> {
    run(
        "recall",
        st,
        ds,
        p,
        headers,
        body,
        Some(("format", "json")),
        Mode::Read,
    )
    .await
}

async fn diagnose(
    st: State<Arc<AppState>>,
    ds: Path<String>,
    p: Extension<Principal>,
    headers: HeaderMap,
    body: AdminBody,
) -> ApiResult<Response> {
    run("why_empty", st, ds, p, headers, body, None, Mode::Read).await
}

/// The JSON body of a tool error.
pub(crate) fn tool_error(e: &super::errors::ToolError) -> crate::http::ApiError {
    let mut b = json!({ "error": e.message, "code": e.code });
    // the details, such as the failed checks of `assert_facts` or the pattern and offset
    // of `secret-detected`, beside the message
    if let Some(Value::Object(d)) = &e.data {
        for (k, v) in d {
            if k != "error" && k != "code" {
                b[k] = v.clone();
            }
        }
    }
    if let Some(h) = &e.hint {
        b["hint"] = h.clone().into();
    }
    if let Some(x) = e.budget {
        b["budget"] = x.into();
    }
    err_body(
        StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        b,
    )
}

/// Run `tool` with the body's arguments and `{ds}`.
#[allow(clippy::too_many_arguments)]
async fn run(
    tool: &str,
    State(st): State<Arc<AppState>>,
    Path(ds): Path<String>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
    fixed: Option<(&str, &str)>,
    mode: Mode,
) -> ApiResult<Response> {
    if let Some(b) = crate::http::branches::current()
        && b != sparkles::branch::MAIN
    {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "invalid-branch",
            "this route reads the main branch only",
        ));
    }
    let mut args: Map<String, Value> = if body.iter().all(u8::is_ascii_whitespace) {
        Map::new()
    } else {
        match serde_json::from_slice(&body) {
            Ok(Value::Object(m)) => m,
            Ok(_) => {
                return Err(err_code(
                    StatusCode::BAD_REQUEST,
                    "bad-argument",
                    "the body must be a JSON object",
                ));
            }
            Err(e) => {
                return Err(err_code(
                    StatusCode::BAD_REQUEST,
                    "bad-argument",
                    format!("the body is not JSON: {e}"),
                ));
            }
        }
    };
    if args.contains_key("dataset") {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "bad-argument",
            "the path names the dataset; leave dataset out of the body",
        ));
    }
    if let Some((k, v)) = fixed {
        if args.get(k).is_some_and(|x| x != v) {
            return Err(err_code(
                StatusCode::BAD_REQUEST,
                "bad-argument",
                format!("{k} is always {v} here"),
            ));
        }
        args.insert(k.into(), v.into());
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
    let server = McpServer::new(st.clone(), config(&st, mode));
    let out = if mode == Mode::Internal {
        let name = tool.to_string();
        let request_id = call.request_id.clone();
        tokio::task::spawn_blocking(move || server.run_now(&name, args, &call))
            .await
            .map_err(|e| {
                tracing::error!(request_id, "{tool} panicked: {e}");
                err_code(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error",
                )
            })
            .map(Ok)?
    } else {
        server.call(tool, args, call).await
    };
    match out {
        Err(_) => Err(err_code(
            StatusCode::NOT_FOUND,
            "unknown-tool",
            format!("{tool} is not available"),
        )),
        Ok(Err(e)) => Err(tool_error(&e)),
        Ok(Ok(Outcome::Structured(v))) => Ok(axum::Json(v).into_response()),
        Ok(Ok(Outcome::Text(t))) => {
            Ok(([(header::CONTENT_TYPE, "application/json")], t).into_response())
        }
    }
}
