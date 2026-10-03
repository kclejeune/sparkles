//! `sparkles mcp --url URL`: a stdio-to-HTTP bridge for MCP hosts that launch stdio
//! servers, to a running `sparkles serve --mcp`. Each JSON-RPC line on stdin is sent as
//! one POST to `URL/$/mcp`, with the headers the Streamable HTTP transport requires, and
//! each message of the answer (a JSON body or an SSE stream) is written to stdout as one
//! line. The server does the work and its permissions apply: the bridge sends the bearer
//! token of `--token`, of `SPARKLES_TOKEN`, or of the saved `sparkles auth login`.
//!
//! Requests run concurrently. `notifications/cancelled` drops the HTTP request it names,
//! which cancels the call on the server, as a closed connection does. A session of the
//! `initialize` era is kept by its `Mcp-Session-Id`, its notifications stream is opened
//! after `notifications/initialized`, and the session is ended when stdin closes.

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::task::AbortHandle;

const PROTOCOL_VERSION: &str = "mcp-protocol-version";
const SESSION: &str = "mcp-session-id";
const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";

/// The `Mcp-Name` of a request: its `name`, `uri` or `taskId`.
fn mcp_name(method: &str, params: Option<&Value>) -> Option<String> {
    let key = match method {
        "tools/call" | "prompts/get" => "name",
        "resources/read" | "resources/subscribe" | "resources/unsubscribe" => "uri",
        "tasks/get" | "tasks/update" | "tasks/cancel" => "taskId",
        _ => return None,
    };
    let v = params?.get(key)?.as_str()?;
    // a value that is not plain visible ASCII travels in base64
    let plain = !v.starts_with([' ', '\t'])
        && !v.ends_with([' ', '\t'])
        && v.chars().all(|c| (' '..='~').contains(&c))
        && !(v.starts_with("=?base64?") && v.ends_with("?="));
    Some(if plain {
        v.to_string()
    } else {
        format!("=?base64?{}?=", STANDARD.encode(v))
    })
}

/// The bridge's state, shared by the requests in flight.
struct Bridge<W> {
    client: reqwest::Client,
    endpoint: String,
    base: String,
    token: Option<String>,
    /// the legacy session and its negotiated protocol version
    session: Mutex<Option<String>>,
    version: Mutex<Option<String>>,
    out: tokio::sync::Mutex<W>,
    /// requests in flight by their JSON-RPC id (as JSON text)
    inflight: Mutex<HashMap<String, AbortHandle>>,
}

impl<W: AsyncWrite + Unpin + Send + 'static> Bridge<W> {
    /// Write one message as one line.
    async fn emit(&self, msg: &Value) {
        let mut line = msg.to_string();
        line.push('\n');
        let mut out = self.out.lock().await;
        if out.write_all(line.as_bytes()).await.is_err() || out.flush().await.is_err() {
            tracing::warn!("MCP bridge: stdout is closed");
        }
    }

    /// A JSON-RPC error for request `id`, when the server answered with something else.
    async fn fail(&self, id: Option<&Value>, code: i64, message: String) {
        match id {
            Some(id) => {
                self.emit(&json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": code, "message": message}}))
                    .await;
            }
            None => tracing::warn!("MCP bridge: {message}"),
        }
    }

    fn request(&self, method: reqwest::Method) -> reqwest::RequestBuilder {
        let mut r = self.client.request(method, &self.endpoint).header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        );
        if let Some(t) = &self.token {
            r = r.bearer_auth(t);
        }
        if let Some(s) = self.session.lock().clone() {
            r = r.header(SESSION, s);
        }
        r
    }

    /// Send one message and write the messages of its answer.
    async fn forward(self: Arc<Self>, msg: Value, raw: String) {
        let id = msg
            .get("id")
            .filter(|_| msg.get("method").is_some())
            .cloned();
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = msg.get("params");
        let mut r = self
            .request(reqwest::Method::POST)
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        // the protocol version: the request's own, the one `initialize` asks for, or the
        // one the session negotiated
        let version = params
            .and_then(|p| p.get("_meta"))
            .and_then(|m| m.get(META_VERSION))
            .or_else(|| {
                (method == "initialize")
                    .then(|| params.and_then(|p| p.get("protocolVersion")))
                    .flatten()
            })
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| self.version.lock().clone());
        if let Some(v) = &version {
            r = r.header(PROTOCOL_VERSION, v);
        }
        if !method.is_empty() {
            r = r.header("mcp-method", &method);
        }
        if let Some(n) = mcp_name(&method, params) {
            r = r.header("mcp-name", n);
        }
        let resp = match r.body(raw).send().await {
            Ok(resp) => resp,
            Err(e) => {
                let m = format!("cannot reach {}: {e}", self.base);
                return self.fail(id.as_ref(), -32603, m).await;
            }
        };
        let status = resp.status();
        if method == "initialize"
            && let Some(s) = resp.headers().get(SESSION).and_then(|v| v.to_str().ok())
        {
            *self.session.lock() = Some(s.to_string());
        }
        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if ct.starts_with("text/event-stream") {
            self.events(resp, &method).await;
            return;
        }
        let body = resp.bytes().await.unwrap_or_default();
        match serde_json::from_slice::<Value>(&body) {
            // a JSON-RPC message (a result, or the error of the transport)
            Ok(v) if v.get("jsonrpc").is_some() => {
                self.note_version(&method, &v);
                self.emit(&v).await;
            }
            _ if status.is_success() => {}
            Ok(v) => {
                let m = v
                    .get("error")
                    .and_then(Value::as_str)
                    .map_or_else(|| status.to_string(), str::to_string);
                self.fail(id.as_ref(), -32603, self.explain(status, m))
                    .await;
            }
            Err(_) => {
                let text = String::from_utf8_lossy(&body).trim().to_string();
                let m = if text.is_empty() {
                    status.to_string()
                } else {
                    text
                };
                self.fail(id.as_ref(), -32603, self.explain(status, m))
                    .await;
            }
        }
    }

    /// The message of an HTTP error, with the remedy for a missing login.
    fn explain(&self, status: reqwest::StatusCode, m: String) -> String {
        if status == reqwest::StatusCode::UNAUTHORIZED {
            format!(
                "{m}: not logged in to {base} (run: sparkles auth login --server {base}, or pass --token)",
                base = self.base
            )
        } else {
            format!("HTTP {}: {m}", status.as_u16())
        }
    }

    /// Remember the version a legacy `initialize` negotiated.
    fn note_version(&self, method: &str, v: &Value) {
        if method == "initialize"
            && let Some(pv) = v["result"]["protocolVersion"].as_str()
        {
            *self.version.lock() = Some(pv.to_string());
        }
    }

    /// Write the messages of an SSE stream as they come.
    async fn events(&self, mut resp: reqwest::Response, method: &str) {
        let mut buf = String::new();
        let mut data = String::new();
        loop {
            let chunk = match resp.chunk().await {
                Ok(Some(c)) => c,
                Ok(None) | Err(_) => break,
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(i) = buf.find('\n') {
                let line = buf[..i].trim_end_matches('\r').to_string();
                buf.drain(..=i);
                if line.is_empty() {
                    // the end of an event
                    if let Ok(v) = serde_json::from_str::<Value>(&data) {
                        self.note_version(method, &v);
                        self.emit(&v).await;
                    }
                    data.clear();
                } else if let Some(d) = line.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(d.strip_prefix(' ').unwrap_or(d));
                }
            }
        }
    }

    /// The notification stream of a legacy session (`GET /$/mcp`), until it ends.
    async fn session_stream(self: Arc<Self>) {
        let mut r = self.request(reqwest::Method::GET);
        if let Some(v) = self.version.lock().clone() {
            r = r.header(PROTOCOL_VERSION, v);
        }
        let Ok(resp) = r.send().await else { return };
        if resp.status().is_success() {
            self.events(resp, "").await;
        }
    }
}

/// Serve the bridge on `input` and `output` until the input closes.
pub async fn serve<R, W>(
    client: reqwest::Client,
    base: String,
    token: Option<String>,
    input: R,
    output: W,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let bridge = Arc::new(Bridge {
        client,
        endpoint: format!("{base}/$/mcp"),
        base,
        token,
        session: Mutex::new(None),
        version: Mutex::new(None),
        out: tokio::sync::Mutex::new(output),
        inflight: Mutex::new(HashMap::new()),
    });
    let mut lines = BufReader::new(input).lines();
    let mut tasks = tokio::task::JoinSet::new();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        // a cancelled request: drop its HTTP request, which cancels it on the server
        if method == "notifications/cancelled"
            && let Some(id) = msg["params"].get("requestId")
            && let Some(h) = bridge.inflight.lock().remove(&id.to_string())
        {
            h.abort();
            if bridge.session.lock().is_none() {
                continue;
            }
        }
        let initialized = method == "notifications/initialized";
        let key = msg
            .get("id")
            .filter(|_| msg.get("method").is_some())
            .map(Value::to_string);
        let b = bridge.clone();
        let id = key.clone();
        let handle = tasks.spawn(async move {
            b.clone().forward(msg, line).await;
            if let Some(id) = id {
                let me = tokio::task::id();
                let mut inflight = b.inflight.lock();
                if inflight.get(&id).is_some_and(|h| h.id() == me) {
                    inflight.remove(&id);
                }
            }
        });
        if let Some(k) = key
            && !handle.is_finished()
        {
            bridge.inflight.lock().insert(k, handle);
        }
        // reap the requests that ended
        while tasks.try_join_next().is_some() {}
        if initialized && bridge.session.lock().is_some() {
            tasks.spawn(bridge.clone().session_stream());
        }
    }
    // stdin closed: stop what is in flight and end the session
    tasks.abort_all();
    if bridge.session.lock().is_some() {
        let _ = bridge.request(reqwest::Method::DELETE).send().await;
    }
    Ok(())
}

/// `sparkles mcp --url URL`.
pub fn run(url: &str, token: Option<String>, insecure: bool) -> Result<()> {
    let remote = crate::remote::Remote::open(Some(url), insecure)?;
    let token = token.filter(|t| !t.is_empty()).or(remote.token);
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .user_agent(concat!("sparkles/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("HTTP client")?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve(
        client,
        remote.base,
        token,
        tokio::io::stdin(),
        tokio::io::stdout(),
    ))
}
