//! `POST /{ds}/sparql/explain` (C18 §6.6.6): a query's plan with its node ids, the
//! notes and the description, as server-sent events or, with `Accept:
//! application/json`, as one object.
//!
//! It runs as the caller over the caller's view, with the limits of the HTTP tools
//! (`crate::mcp::rest`). The `plan` and `notes` events come before the `explain` role
//! is called, so a person reads the notes while the model writes.

use crate::auth::Principal;
use crate::http::{AdminBody, ApiResult, err_code};
use crate::mcp::explain::{ExplainBody, sparql_explain};
use crate::mcp::{Call, McpServer};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// The largest plan a client may give, as JSON.
pub const MAX_PLAN_BYTES: usize = 2 << 20;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/{ds}/sparql/explain", post(explain_route))
}

fn bad(m: impl Into<String>) -> crate::http::ApiError {
    err_code(StatusCode::BAD_REQUEST, "bad-argument", m)
}

async fn explain_route(
    State(st): State<Arc<AppState>>,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult<Response> {
    let mut b: ExplainBody = serde_json::from_slice(&body).map_err(|e| bad(e.to_string()))?;
    if b.dataset.is_some() {
        return Err(bad(
            "the path names the dataset; leave dataset out of the body",
        ));
    }
    if b.query.trim().is_empty() || b.query.chars().count() > 65536 {
        return Err(bad("query must hold 1 to 65536 characters"));
    }
    if let Some(plan) = &b.plan
        && serde_json::to_vec(plan).map_or(0, |v| v.len()) > MAX_PLAN_BYTES
    {
        return Err(err_code(
            StatusCode::PAYLOAD_TOO_LARGE,
            "bad-argument",
            format!("plan is larger than {} MiB", MAX_PLAN_BYTES >> 20),
        ));
    }
    b.dataset = Some(name);
    // the branch of the body, else the request's
    let branch = b
        .branch
        .take()
        .map(|s| s.trim().to_string())
        .or_else(crate::http::branches::current)
        .filter(|b| b != sparkles::branch::MAIN);
    if let Some(br) = &branch
        && !sparkles::branch::valid_name(br)
    {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "invalid-branch",
            format!("invalid branch name '{br}'"),
        ));
    }
    let request_id = headers
        .get(&crate::obs::X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    let call = Call {
        arrived: Instant::now(),
        cancel: cancel.clone(),
        request_id,
        principal: p.on_branch(branch.as_deref()),
        headers: None,
        held: None,
    };
    let server = McpServer::new(
        st.clone(),
        crate::mcp::rest::config(&st, crate::mcp::rest::Mode::Read),
    );
    let sse = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|a| a.contains("text/event-stream") || !a.contains("application/json"));
    let held = crate::ratelimit::hold();
    let span = tracing::Span::current();
    if !sse {
        let out = tokio::task::spawn_blocking(move || {
            let _held = held;
            span.in_scope(|| {
                let mut out = Map::new();
                let mut on = |event: &str, data: Value| {
                    match event {
                        "plan" | "notes" => {
                            if let Value::Object(m) = data {
                                out.extend(m);
                            }
                        }
                        e => {
                            out.insert(e.into(), data);
                        }
                    };
                };
                sparql_explain(&server, &call, b, &mut on).map(|()| Value::Object(out))
            })
        })
        .await
        .map_err(|e| err_code(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
        return match out {
            Ok(v) => Ok(Json(v).into_response()),
            Err(e) => Err(crate::mcp::rest::tool_error(&e)),
        };
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    tokio::task::spawn_blocking(move || {
        let _held = held;
        span.in_scope(|| {
            let mut on = |event: &str, data: Value| {
                let e = Event::default().event(event).data(data.to_string());
                if tx.send(e).is_err() {
                    // the client went away
                    cancel.store(true, Ordering::Relaxed);
                }
            };
            if let Err(e) = sparql_explain(&server, &call, b, &mut on) {
                let mut body = json!({ "error": e.message, "code": e.code, "status": e.status });
                if let Some(h) = &e.hint {
                    body["hint"] = h.clone().into();
                }
                on("error", body);
            }
        })
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|e| (Ok::<_, std::convert::Infallible>(e), rx))
    });
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}
