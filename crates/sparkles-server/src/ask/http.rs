//! `POST /{ds}/ask` (spec C18 §5): the asking pipeline for the UI's Ask bar, as
//! server-sent events or, with `Accept: application/json`, as one object.
//!
//! The pipeline runs as the caller over the caller's view, with the dataset's role
//! lists and limits from `assistant.json`. Each event of §5.2 is sent as it happens, and
//! a client that goes away cancels the pipeline at its next step. The finished ask is
//! counted for the budgets and `GET /$/models/usage`, and kept in the caller's history
//! when the dataset keeps one.

use super::{AskOptions, Turn, ask};
use crate::assistant::{self, Send};
use crate::auth::Principal;
use crate::http::{AdminBody, ApiResult, err_code};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/{ds}/ask", post(ask_route))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TurnBody {
    question: String,
    query: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Clarification {
    Answer {
        /// the clarify event's id, which the answer may repeat
        #[serde(default, rename = "id")]
        _id: Option<String>,
        value: String,
    },
    Text(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AskBody {
    question: String,
    #[serde(default)]
    context: Vec<TurnBody>,
    clarification: Option<Clarification>,
    at: Option<Value>,
    branch: Option<String>,
    reasoning: Option<bool>,
    run: Option<bool>,
    summary: Option<bool>,
    max_rows: Option<usize>,
    try_harder: Option<String>,
    #[serde(default)]
    reviewed_only: bool,
    query: Option<String>,
}

fn bad(m: impl Into<String>) -> crate::http::ApiError {
    err_code(StatusCode::BAD_REQUEST, "bad-argument", m)
}

/// The HTTP status of an error event's code, for the JSON form.
fn status_of(code: &str) -> StatusCode {
    match code {
        "bad-argument" => StatusCode::BAD_REQUEST,
        "unknown-dataset" | "no-model" => StatusCode::NOT_FOUND,
        "budget-exceeded" => StatusCode::TOO_MANY_REQUESTS,
        "provider-unavailable" => StatusCode::BAD_GATEWAY,
        "timeout" => StatusCode::GATEWAY_TIMEOUT,
        "cancelled" => StatusCode::SERVICE_UNAVAILABLE,
        "no-later-pair" => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn ask_route(
    State(st): State<Arc<AppState>>,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult<Response> {
    // the main dataset's settings; a branch is read through the tools' `branch`
    let ds = st.get(&name).ok_or_else(|| {
        err_code(
            StatusCode::NOT_FOUND,
            "unknown-dataset",
            format!("no such dataset: /{name}"),
        )
    })?;
    let b: AskBody = serde_json::from_slice(&body).map_err(|e| bad(e.to_string()))?;
    let question = b.question.trim().to_string();
    if question.is_empty() || question.chars().count() > 2000 {
        return Err(bad("question must hold 1 to 2000 characters"));
    }
    if b.context.len() > super::MAX_CONTEXT {
        return Err(bad(format!(
            "context holds at most {} earlier turns",
            super::MAX_CONTEXT
        )));
    }
    if b.context
        .iter()
        .any(|t| t.question.chars().count() > 2000 || t.query.chars().count() > 65536)
    {
        return Err(bad("an earlier turn's question or query is too long"));
    }
    if b.query
        .as_ref()
        .is_some_and(|q| q.trim().is_empty() || q.chars().count() > 65536)
    {
        return Err(bad("query must hold 1 to 65536 characters"));
    }
    if b.query.is_some() && b.try_harder.is_some() {
        return Err(bad("query and tryHarder cannot be combined"));
    }
    let settings = assistant::settings(&st, &ds);
    if let Err(why) = assistant::ask_status(&st, &settings) {
        return Err(err_code(StatusCode::NOT_FOUND, "no-assistant", why));
    }
    let Some(models) = st.models.clone() else {
        return Err(err_code(
            StatusCode::NOT_FOUND,
            "no-assistant",
            "this server has no model providers",
        ));
    };
    let me = p.id();
    // §3.5: a request that would start over a daily cap is refused before any call
    let over = |used: u64, cap: Option<u64>| cap.is_some_and(|c| used >= c);
    if over(
        st.asks.tokens_today(&ds.name, None),
        settings.budget.per_dataset_per_day,
    ) || over(
        st.asks.tokens_today(&ds.name, Some(&me)),
        settings.budget.per_principal_per_day,
    ) {
        let reset = (chrono::Utc::now() + chrono::Duration::days(1))
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|t| {
                t.and_utc()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            });
        let mut e = json!({
            "error": "this dataset's question budget for today is used up",
            "code": "budget-exceeded",
        });
        if let Some(r) = reset {
            e["resetAt"] = r.into();
        }
        return Err(crate::http::err_body(StatusCode::TOO_MANY_REQUESTS, e));
    }
    // Try harder: the pair after the one that drafted the earlier ask of this principal
    let draft_start = match &b.try_harder {
        None => None,
        Some(id) => {
            let Some(r) = st
                .asks
                .recent(id)
                .filter(|r| r.principal == me && r.dataset == ds.name)
            else {
                return Err(err_code(
                    StatusCode::NOT_FOUND,
                    "unknown-ask",
                    format!("no recent ask {id:?} of yours"),
                ));
            };
            Some(r.draft_pair + 1)
        }
    };
    let lists = assistant::lists(&models, &settings);
    if let Some(k) = draft_start
        && k >= lists.get(&crate::models::Role::Draft).map_or(0, Vec::len)
    {
        return Err(err_code(
            StatusCode::CONFLICT,
            "no-later-pair",
            "the draft role has no pair after the one that answered",
        ));
    }
    // Try harder rejects the earlier answer (§5.5)
    if let Some(id) = &b.try_harder {
        assistant::feedback(&st, &ds, &me, id, "rejected", None);
    }
    // reviewedOnly: the query does not see agent memory that nobody reviewed (§8.8)
    let principal = if b.reviewed_only {
        let m = crate::assist::memory_settings(&st, &ds);
        p.clone().hiding_graphs(&m.agent_graphs)
    } else {
        p.clone()
    };
    let branch = b
        .branch
        .clone()
        .or_else(|| crate::http::branches::current().filter(|b| b != sparkles::branch::MAIN));
    let cfg = crate::mcp::rest::config(&st);
    let max_rows = b.max_rows.unwrap_or(1000).clamp(1, cfg.max_rows.max(1));
    let rows_allowed = settings.send >= Send::Rows;
    let id = uuid::Uuid::new_v4().simple().to_string()[..16].to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    let routing = settings.routing.clone().unwrap_or_default();
    let model_routing = models.config.routing.clone().unwrap_or_default();
    let o = AskOptions {
        dataset: ds.name.clone(),
        question,
        id: Some(id.clone()),
        lists: Some(lists),
        draft_start,
        context: b
            .context
            .into_iter()
            .map(|t| Turn {
                question: t.question,
                query: t.query,
            })
            .collect(),
        clarification: b.clarification.map(|c| match c {
            Clarification::Answer { value, .. } | Clarification::Text(value) => value,
        }),
        query: b.query,
        run: b.run.unwrap_or(true),
        summary: rows_allowed && b.summary.unwrap_or(true),
        max_rows,
        rows_for_summary: settings.rows_for_summary(),
        deadline: Duration::from_secs_f64(
            settings
                .deadline_secs
                .unwrap_or(assistant::DEFAULT_DEADLINE_SECS),
        ),
        max_tokens: settings
            .budget
            .per_request
            .unwrap_or(assistant::DEFAULT_TOKENS_PER_REQUEST),
        complexity_threshold: routing
            .complexity_threshold
            .or(model_routing.complexity_threshold)
            .unwrap_or(super::DEFAULT_COMPLEXITY_THRESHOLD),
        example_score: routing
            .example_score
            .or(model_routing.example_score)
            .unwrap_or(super::DEFAULT_EXAMPLE_SCORE),
        at: b.at,
        branch,
        reasoning: b.reasoning,
        full_results: true,
        cancel: Some(cancel.clone()),
        pairs: Vec::new(),
    };
    let server = crate::mcp::McpServer::new(st.clone(), cfg);
    let finish = {
        let st = st.clone();
        let ds = ds.clone();
        let id = id.clone();
        let me = me.clone();
        move |out: &Value| {
            let tokens = out["usage"]["inputTokens"].as_u64().unwrap_or(0)
                + out["usage"]["outputTokens"].as_u64().unwrap_or(0);
            st.asks.add_tokens(&ds.name, &me, tokens);
            st.asks
                .record(&id, assistant::recent_of(&ds.name, &me, out), &out["usage"]);
            assistant::remember(&st, &ds, assistant::record_of(&id, &me, out));
        }
    };
    let sse = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|a| a.contains("text/event-stream") || !a.contains("application/json"));
    let held = crate::ratelimit::hold();
    if !sse {
        let span = tracing::Span::current();
        let out = tokio::task::spawn_blocking(move || {
            let _held = held;
            span.in_scope(|| {
                let mut on = |_: &str, _: &Value| {};
                let out = ask(&server, &models, &principal, &o, &mut on);
                finish(&out);
                out
            })
        })
        .await
        .map_err(|e| err_code(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
        let status = match out["error"]["code"].as_str() {
            Some(c) => status_of(c),
            None => StatusCode::OK,
        };
        return Ok((status, Json(out)).into_response());
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || {
        let _held = held;
        span.in_scope(|| {
            let mut on = |event: &str, data: &Value| {
                let e = Event::default().event(event).data(data.to_string());
                if tx.send(e).is_err() {
                    // the client went away: stop at the next step
                    cancel.store(true, Ordering::Relaxed);
                }
            };
            let out = ask(&server, &models, &principal, &o, &mut on);
            finish(&out);
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
