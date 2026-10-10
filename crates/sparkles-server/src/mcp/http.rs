//! `/$/mcp`: the MCP tools over the Streamable HTTP transport (`sparkles serve --mcp`).
//!
//! rmcp's `StreamableHttpService` speaks the transport: one JSON-RPC message per POST,
//! answered with `application/json` (or an SSE stream when a legacy session needs one),
//! the `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` header checks, and for
//! clients of the `initialize` era, sessions named by `Mcp-Session-Id` (GET opens a
//! session's SSE stream and DELETE ends the session). Modern `2026-07-28` requests are
//! stateless.
//!
//! This layer sits between the router and rmcp, inside the server's own middleware, so
//! the request has already passed the Host and Origin checks, authentication and the
//! route's permission (any caller). It then:
//!
//! * answers `401` with the server's challenges to an anonymous caller that can read no
//!   dataset, so that clients know to sign in;
//! * binds each legacy session to the principal that opened it: another caller's
//!   requests with that session id get `404`, like an unknown session;
//! * bounds the number of legacy sessions;
//! * charges the rate limit of the message's class on its dataset, as the SPARQL
//!   endpoints do (a tool call is a `query`, `sparql_update` an `update`), and keeps
//!   the concurrency permits until the tool's work ends.
//!
//! Each tool call then runs as the request's principal (`adapter.rs`).

use super::adapter::{Adapter, Transport};
use super::{McpConfig, McpServer};
use crate::auth::Principal;
use crate::ratelimit::Class;
use crate::state::AppState;
use anyhow::{Result, bail};
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use parking_lot::Mutex;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The largest JSON-RPC message (a `sparql_update` of 1 Mi characters, escaped).
const MAX_BODY_BYTES: usize = 4 << 20;

const SESSION_HEADER: &str = "mcp-session-id";

/// `sparkles serve` flags of the MCP endpoint.
#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// Serve the MCP tools over Streamable HTTP at /$/mcp. Each call runs as the
    /// request's caller and sees only the datasets it may read
    #[arg(long)]
    pub mcp: bool,
    /// Offer the sparql_update tool at /$/mcp to callers that may write to a dataset
    /// (ignored, with a warning, on a --read-only server)
    #[arg(long)]
    pub mcp_allow_update: bool,
    /// Do not offer the datasets' stored queries as tools at /$/mcp
    #[arg(long)]
    pub mcp_no_stored_queries: bool,
    /// Allow federated SERVICE calls in MCP queries (--no-service still wins, and with
    /// auth the caller needs the `federate` permission)
    #[arg(long)]
    pub mcp_allow_service: bool,
    /// Datasets the MCP tools may see, by name or `*` pattern (repeatable; default all).
    /// Permissions still apply within them
    #[arg(long, value_name = "PATTERN")]
    pub mcp_dataset: Vec<String>,
    /// Largest `maxRows` an MCP sparql_query call may request
    #[arg(long, value_name = "N", default_value_t = 1000)]
    pub mcp_max_rows: usize,
    /// Largest `maxBytes` an MCP sparql_query call may request
    #[arg(long, value_name = "N", default_value_t = 1 << 20)]
    pub mcp_max_bytes: usize,
    /// Memory budget of an MCP call's queries, in MiB, capped by --query-memory-mb (0:
    /// that cap alone)
    #[arg(long, value_name = "N", default_value_t = 2048)]
    pub mcp_query_memory_mb: u64,
    /// MCP tool calls that run at once; further calls wait for a slot
    #[arg(long, value_name = "N", default_value_t = 4)]
    pub mcp_max_concurrent: usize,
    /// Do not offer this MCP tool (repeatable)
    #[arg(long, value_name = "NAME")]
    pub mcp_disable_tool: Vec<String>,
    /// MCP sessions of clients that use the `initialize` handshake, at once (0: no
    /// sessions; such clients are served without one)
    #[arg(long, value_name = "N", default_value_t = 256)]
    pub mcp_max_sessions: usize,
    /// How long an MCP tool call of a client that supports tasks runs before it becomes
    /// a task that the client polls, in milliseconds
    #[arg(long, value_name = "MS", default_value_t = 2000)]
    pub mcp_task_after_ms: u64,
}

/// The endpoint's settings (`AppState::mcp`).
pub struct HttpConf {
    pub cfg: McpConfig,
    /// legacy sessions at once (0: legacy clients are served statelessly)
    pub max_sessions: usize,
    /// ends the SSE streams and sessions when the server shuts down
    pub shutdown: CancellationToken,
}

impl ServeArgs {
    /// The endpoint's settings, or `None` without `--mcp`. `st` holds the server's
    /// limits, read-only switch and SERVICE setting.
    pub fn conf(&self, st: &AppState) -> Result<Option<HttpConf>> {
        if !self.mcp {
            return Ok(None);
        }
        if self.mcp_max_rows == 0 {
            bail!("--mcp-max-rows must be at least 1");
        }
        if self.mcp_max_bytes < 1024 {
            bail!("--mcp-max-bytes must be at least 1024");
        }
        if self.mcp_max_concurrent == 0 {
            bail!("--mcp-max-concurrent must be at least 1");
        }
        for t in &self.mcp_disable_tool {
            if !super::schemas::all_tools().contains(&t.as_str()) {
                bail!(
                    "--mcp-disable-tool: unknown tool '{t}' (tools: {})",
                    super::schemas::all_tools().join(", ")
                );
            }
        }
        for d in &self.mcp_dataset {
            if d.is_empty()
                || !d
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-' | b'*'))
            {
                bail!("--mcp-dataset '{d}': expected a dataset name or a `*` pattern");
            }
        }
        if self.mcp_allow_update && st.read_only {
            tracing::warn!("--mcp-allow-update has no effect on a --read-only server");
        }
        if self.mcp_allow_service && !st.allow_service {
            tracing::warn!("--mcp-allow-service has no effect with --no-service");
        }
        let mcp_memory = (self.mcp_query_memory_mb > 0).then_some(self.mcp_query_memory_mb << 20);
        let query_memory_bytes = match (mcp_memory, st.limits.query_memory_bytes) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Ok(Some(HttpConf {
            cfg: McpConfig {
                max_rows: self.mcp_max_rows,
                max_bytes: self.mcp_max_bytes,
                // calls may ask for up to the server's default query timeout
                max_timeout: st.default_timeout,
                query_memory_bytes,
                allow_service: self.mcp_allow_service && st.allow_service,
                allow_update: self.mcp_allow_update && !st.read_only,
                max_concurrent: self.mcp_max_concurrent,
                disabled: self
                    .mcp_disable_tool
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
                datasets: self.mcp_dataset.clone(),
                stored_queries: !self.mcp_no_stored_queries,
                task_after: std::time::Duration::from_millis(self.mcp_task_after_ms),
                http: true,
                ..McpConfig::default()
            },
            max_sessions: self.mcp_max_sessions,
            shutdown: CancellationToken::new(),
        }))
    }
}

/// A request's share of its rate-limit permits, carried to the tool's blocking work.
#[derive(Clone)]
pub struct HeldShare(#[allow(dead_code)] Arc<crate::ratelimit::Held>);

struct Endpoint {
    state: Arc<AppState>,
    server: McpServer,
    service: StreamableHttpService<Adapter, LocalSessionManager>,
    sessions: Arc<LocalSessionManager>,
    /// the principal (`Principal::id`) that opened each legacy session
    owners: Mutex<HashMap<String, String>>,
    max_sessions: usize,
}

/// The `/$/mcp` route, or `None` when the endpoint is off.
pub fn route(state: &Arc<AppState>) -> Option<MethodRouter<Arc<AppState>>> {
    let conf = state.mcp.clone()?;
    let server = McpServer::new(state.clone(), conf.cfg.clone());
    let adapter = Adapter::with_transport(server.clone(), Transport::Http);
    let sessions = Arc::new(LocalSessionManager::default());
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(conf.max_sessions > 0)
        .with_json_response(true)
        .with_cancellation_token(conf.shutdown.clone())
        .with_max_request_body_bytes(MAX_BODY_BYTES)
        // the server's own Host and Origin checks ran before this layer (`auth`), as for
        // every route; rmcp's would refuse every name but localhost
        .disable_allowed_hosts()
        .disable_allowed_origins();
    let service = StreamableHttpService::new(move || Ok(adapter.clone()), sessions.clone(), config);
    let ep = Arc::new(Endpoint {
        state: state.clone(),
        server,
        service,
        sessions,
        owners: Mutex::new(HashMap::new()),
        max_sessions: conf.max_sessions,
    });
    Some(axum::routing::any(move |req: Request| {
        let ep = ep.clone();
        async move { ep.handle(req).await }
    }))
}

/// A JSON error body, like the server's other errors.
fn error(status: StatusCode, msg: &str) -> Response {
    let body = json!({ "error": msg });
    let mut r = (status, axum::Json(body.clone())).into_response();
    r.extensions_mut().insert(crate::http::ErrorJson(body));
    r
}

/// What a message costs: its rate-limit class and dataset, and whether it opens a
/// session.
#[derive(Debug, Default, PartialEq)]
struct Charge {
    class: Option<Class>,
    dataset: Option<String>,
    initialize: bool,
}

fn charge_of(message: &Value) -> Charge {
    let Value::Object(m) = message else {
        return Charge::default();
    };
    let params = m.get("params");
    let str_at = |v: Option<&Value>, k: &str| {
        v.and_then(|v| v.get(k))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    match m.get("method").and_then(Value::as_str) {
        Some("tools/call") => {
            let name = str_at(params, "name");
            let class = if name.as_deref() == Some("sparql_update") {
                Class::Update
            } else {
                Class::Query
            };
            // a stored query's tool names its dataset (`<dataset>__<query>`)
            let dataset =
                str_at(params.and_then(|p| p.get("arguments")), "dataset").or_else(|| {
                    name.as_deref()
                        .and_then(|n| n.split_once("__"))
                        .map(|(ds, _)| ds.to_string())
                });
            Charge {
                class: Some(class),
                dataset,
                initialize: false,
            }
        }
        Some("resources/read") => Charge {
            class: Some(Class::Query),
            dataset: str_at(params, "uri").and_then(|u| {
                u.strip_prefix("sparkles://")
                    .and_then(|r| r.split_once('/'))
                    .map(|(ds, _)| ds.to_string())
            }),
            initialize: false,
        },
        Some("initialize") => Charge {
            initialize: true,
            ..Charge::default()
        },
        _ => Charge::default(),
    }
}

impl Endpoint {
    async fn handle(&self, req: Request) -> Response {
        // the auth layer names every caller (the local principal without auth)
        let Some(p) = req.extensions().get::<Principal>().cloned() else {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "request without a caller",
            );
        };
        // an anonymous caller that can read nothing is asked to sign in
        if self.state.auth.is_some() && p.is_anonymous() && self.server.visible(&p).is_empty() {
            return crate::auth::authentication_required(&self.state, &p, req.headers());
        }
        let session = req
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        // a session belongs to the caller that opened it
        if let Some(s) = &session
            && self
                .owners
                .lock()
                .get(s)
                .is_some_and(|owner| *owner != p.id())
        {
            return error(StatusCode::NOT_FOUND, "Not Found: Session not found");
        }
        if req.method() != Method::POST {
            return self.forward(req, &p).await;
        }
        let (parts, body) = req.into_parts();
        let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY_BYTES).await else {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "an MCP message may be at most 4 MiB",
            );
        };
        let Ok(message) = serde_json::from_slice::<Value>(&bytes) else {
            // JSON-RPC's parse error, without an id
            let body = json!({"jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": "Parse error: the body is not JSON"}});
            return (StatusCode::BAD_REQUEST, axum::Json(body)).into_response();
        };
        let mut charge = charge_of(&message);
        if charge.initialize && session.is_none() && self.max_sessions > 0 {
            let open = self.sessions.sessions.read().await.len();
            if open >= self.max_sessions {
                let mut r = error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "too many MCP sessions; retry later",
                );
                r.headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
                return r;
            }
        }
        // a call without `dataset` reads the caller's only dataset
        if charge.class.is_some() && charge.dataset.is_none() {
            let visible = self.server.visible(&p);
            if let [only] = visible.as_slice() {
                charge.dataset = Some(only.name.clone());
            }
        }
        let req = Request::from_parts(parts, Body::from(bytes));
        match (&self.state.rate_limit, charge.class) {
            (Some(rl), Some(class)) => {
                crate::ratelimit::limit_as(rl, class, charge.dataset, req, |req| {
                    self.forward(req, &p)
                })
                .await
            }
            _ => self.forward(req, &p).await,
        }
    }

    /// Hand the request to rmcp, and record the owner of a session it opens.
    async fn forward(&self, mut req: Request, p: &Principal) -> Response {
        // inside the rate limiter's scope: the tool keeps the permits while it works
        req.extensions_mut()
            .insert(HeldShare(Arc::new(crate::ratelimit::hold())));
        // rmcp reads the Host even with its own check off; the server's check ran
        // already, and a request may come without one (HTTP/1.0, some proxies)
        if !req.headers().contains_key(header::HOST) && req.uri().authority().is_none() {
            req.headers_mut()
                .insert(header::HOST, HeaderValue::from_static("localhost"));
        }
        let resp = self.service.handle(req).await;
        if let Some(id) = resp
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
        {
            let open = self.sessions.sessions.read().await;
            let mut owners = self.owners.lock();
            owners.retain(|s, _| open.keys().any(|k| k.as_ref() == s.as_str()));
            owners.insert(id.to_string(), p.id());
        }
        resp.map(Body::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges() {
        let c = |s: &str| charge_of(&serde_json::from_str(s).unwrap());
        assert_eq!(
            c(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"sparql_query","arguments":{"dataset":"wiki","query":"ASK{}"}}}"#
            ),
            Charge {
                class: Some(Class::Query),
                dataset: Some("wiki".into()),
                initialize: false
            }
        );
        assert_eq!(
            c(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"sparql_update","arguments":{"update":"CLEAR ALL"}}}"#
            )
            .class,
            Some(Class::Update)
        );
        assert_eq!(
            c(
                r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"sparkles://t/schema"}}"#
            )
            .dataset
            .as_deref(),
            Some("t")
        );
        assert!(c(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#).initialize);
        assert_eq!(
            c(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#),
            Charge::default()
        );
        assert_eq!(c("[1,2]"), Charge::default());
    }
}
