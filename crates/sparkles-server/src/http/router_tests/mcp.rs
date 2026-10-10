//! `/$/mcp` (`serve --mcp`): the Streamable HTTP transport through the whole router, and
//! the MCP tools as each caller sees them (permissions, hidden datasets, rate limits,
//! budgets and the write tool's gate).

use super::*;
use crate::auth::Peer;
use crate::mcp::http::{HttpConf, ServeArgs};
use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use clap::Parser;
use serde_json::json;

mod ingest;
mod memory_writes;

const FIXTURE: &str = r#"@prefix ex:   <http://ex.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:Person rdfs:label "Person"@en .
ex:alice a ex:Person ; rdfs:label "Alice"@en ; ex:knows ex:bob .
ex:bob   a ex:Person ; rdfs:label "Bob" .
"#;

#[derive(Parser)]
struct Flags {
    #[command(flatten)]
    mcp: ServeArgs,
}

/// The endpoint's settings from `serve` flags (`--mcp` is implied).
fn conf(st: &AppState, flags: &[&str]) -> Arc<HttpConf> {
    let args = ["serve", "--mcp"].into_iter().chain(flags.iter().copied());
    Arc::new(Flags::parse_from(args).mcp.conf(st).unwrap().unwrap())
}

fn load(st: &AppState, name: &str, ttl: &str) {
    let ds = st.attach(name, DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            ttl.as_bytes().to_vec(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
}

/// An open server with dataset `t` and the MCP flags `flags`.
fn open(flags: &[&str], read_only: bool) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.read_only = read_only;
    st.mcp = Some(conf(&st, flags));
    let state = Arc::new(st);
    load(&state, "t", FIXTURE);
    let app = router(state.clone());
    Server {
        _dir: dir,
        state,
        app,
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    /// The JSON-RPC message of a JSON body, or the last one of an SSE stream.
    fn rpc(&self) -> J {
        if self.content_type().starts_with("text/event-stream") {
            let last = self
                .body
                .lines()
                .filter_map(|l| l.strip_prefix("data: "))
                .rfind(|d| d.starts_with('{'))
                .unwrap_or_else(|| panic!("no message in {}", self.body));
            return serde_json::from_str(last).unwrap();
        }
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
    fn content_type(&self) -> String {
        self.header("content-type")
    }
    fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default()
    }
}

async fn http(app: &Router, method: &str, headers: &[(&str, &str)], body: &str) -> Reply {
    let mut req = Request::builder()
        .method(method)
        .uri("/$/mcp")
        .extension(ConnectInfo(Peer::Tcp("127.0.0.2:1".parse().unwrap())));
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

const ACCEPT: (&str, &str) = ("accept", "application/json, text/event-stream");
const JSON_CT: (&str, &str) = ("content-type", "application/json");

/// `_meta` of a modern request.
fn m() -> J {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

/// A modern request with the headers the transport requires, plus `extra`.
async fn modern(app: &Router, method: &str, mut params: J, extra: &[(&str, &str)]) -> Reply {
    if params.get("_meta").is_none() {
        params["_meta"] = m();
    }
    let name = params
        .get("name")
        .or_else(|| params.get("uri"))
        .or_else(|| params.get("taskId"))
        .and_then(J::as_str)
        .map(str::to_string);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let mut headers = vec![
        ACCEPT,
        JSON_CT,
        ("mcp-protocol-version", "2026-07-28"),
        ("mcp-method", method),
    ];
    if let Some(n) = &name {
        headers.push(("mcp-name", n.as_str()));
    }
    headers.extend_from_slice(extra);
    http(app, "POST", &headers, &body.to_string()).await
}

/// A modern `tools/call`; returns `result`.
async fn tool(app: &Router, name: &str, args: J, extra: &[(&str, &str)]) -> J {
    let r = modern(
        app,
        "tools/call",
        json!({"name": name, "arguments": args}),
        extra,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let rpc = r.rpc();
    assert!(rpc.get("error").is_none(), "{rpc}");
    rpc["result"].clone()
}

/// The error code of a failed tool call.
fn tool_error(result: &J) -> String {
    assert_eq!(result["isError"], true, "{result}");
    result["_meta"]["io.github.kclejeune.sparkles/error"]["code"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The tool names of `tools/list`.
async fn tool_names(app: &Router, extra: &[(&str, &str)]) -> Vec<String> {
    let r = modern(app, "tools/list", json!({}), extra).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    r.rpc()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

/// The dataset names of `list_datasets`.
async fn dataset_names(app: &Router, extra: &[(&str, &str)]) -> Vec<String> {
    let r = tool(app, "list_datasets", json!({}), extra).await;
    r["structuredContent"]["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect()
}

/// A legacy `initialize`; returns the session id (empty without one) and the reply.
async fn initialize(app: &Router, extra: &[(&str, &str)]) -> (String, Reply) {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": {"name": "t", "version": "1"}}});
    let mut headers = vec![ACCEPT, JSON_CT];
    headers.extend_from_slice(extra);
    let r = http(app, "POST", &headers, &body.to_string()).await;
    (r.header("mcp-session-id"), r)
}

/// A legacy message in session `sid`.
async fn legacy(app: &Router, sid: &str, body: J, extra: &[(&str, &str)]) -> Reply {
    let mut headers = vec![
        ACCEPT,
        JSON_CT,
        ("mcp-protocol-version", "2025-11-25"),
        ("mcp-session-id", sid),
    ];
    headers.extend_from_slice(extra);
    http(app, "POST", &headers, &body.to_string()).await
}

// ------------------------------------------------------------------ transport ------

/// `subscriptions/listen` over HTTP: the answer is an SSE stream that carries the
/// acknowledgment, then a notification when a stored query is added.
#[tokio::test(flavor = "multi_thread")]
async fn subscriptions_stream_over_http() {
    use http_body_util::BodyExt;
    let s = open(&[], true);
    let body = json!({"jsonrpc": "2.0", "id": 7, "method": "subscriptions/listen",
        "params": {"_meta": m(), "notifications": {"toolsListChanged": true}}});
    let mut req = Request::builder()
        .method("POST")
        .uri("/$/mcp")
        .extension(ConnectInfo(Peer::Tcp("127.0.0.2:1".parse().unwrap())));
    for (k, v) in [
        ACCEPT,
        JSON_CT,
        ("mcp-protocol-version", "2026-07-28"),
        ("mcp-method", "subscriptions/listen"),
    ] {
        req = req.header(k, v);
    }
    let res = s
        .app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res.headers()["content-type"].to_str().unwrap().to_string();
    assert!(ct.starts_with("text/event-stream"), "{ct}");
    let mut stream = res.into_body();
    let mut seen = String::new();
    let mut added = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !seen.contains("notifications/tools/list_changed") {
        let frame = tokio::time::timeout_at(deadline, stream.frame())
            .await
            .expect("no notification")
            .expect("the stream ended")
            .unwrap();
        if let Ok(data) = frame.into_data() {
            seen.push_str(&String::from_utf8_lossy(&data));
        }
        if !added && seen.contains("notifications/subscriptions/acknowledged") {
            // the subscription looks every 2 s; change the tools after its first look
            tokio::time::sleep(Duration::from_millis(500)).await;
            let ds = s.state.get("t").unwrap();
            let d: sparkles::stored::Definition =
                serde_json::from_value(json!({"query": "ASK { ?s ?p ?o }"})).unwrap();
            ds.queries
                .put("anything", d, sparkles::stored::Change::default())
                .unwrap();
            added = true;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a22_modern_call_returns_json() {
    let s = open(&[], false);
    let r = modern(
        &s.app,
        "tools/call",
        json!({"name": "sparql_query", "arguments": {"dataset": "t", "query": "ASK { ex:alice ex:knows ex:bob }"}}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.content_type().starts_with("application/json"),
        "{}",
        r.content_type()
    );
    let rpc = r.rpc();
    assert_eq!(rpc["id"], 1);
    let text = rpc["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("# ASK · commit 1"), "{text}");
    assert!(text.ends_with("true"), "{text}");
    assert_eq!(rpc["result"]["resultType"], "complete");
    // modern requests are stateless: no session
    assert!(r.header("mcp-session-id").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a23_transport_validation() {
    let s = open(&[], false);
    let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "_meta": m(), "name": "sparql_query", "arguments": {"query": "ASK {}"}}})
    .to_string();
    let base = [
        ACCEPT,
        JSON_CT,
        ("mcp-protocol-version", "2026-07-28"),
        ("mcp-method", "tools/call"),
    ];
    // Mcp-Name that does not match the body
    let mut h = base.to_vec();
    h.push(("mcp-name", "list_datasets"));
    let r = http(&s.app, "POST", &h, &call).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.rpc()["error"]["code"], -32020, "{}", r.body);
    // a modern body without MCP-Protocol-Version
    let h = [
        ACCEPT,
        JSON_CT,
        ("mcp-method", "tools/call"),
        ("mcp-name", "sparql_query"),
    ];
    let r = http(&s.app, "POST", &h, &call).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    // an unknown method
    let r = modern(&s.app, "nope/nope", json!({}), &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    assert_eq!(r.rpc()["error"]["code"], -32601);
    // an unknown tool is a protocol error
    let r = modern(
        &s.app,
        "tools/call",
        json!({"name": "drop_everything", "arguments": {}}),
        &[],
    )
    .await;
    assert_eq!(r.rpc()["error"]["code"], -32602, "{}", r.body);
    // the transport needs both media types in Accept
    let h = [("accept", "application/json"), JSON_CT];
    let r = http(&s.app, "POST", &h, &call).await;
    assert_eq!(r.status, StatusCode::NOT_ACCEPTABLE);
    // not JSON
    let r = http(&s.app, "POST", &[ACCEPT, JSON_CT], "{nope").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    assert_eq!(r.rpc()["error"]["code"], -32700);
    assert_eq!(r.rpc()["id"], J::Null);
    // a notification is accepted with no body
    let r = http(
        &s.app,
        "POST",
        &[
            ACCEPT,
            JSON_CT,
            ("mcp-protocol-version", "2026-07-28"),
            ("mcp-method", "notifications/cancelled"),
        ],
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":99}}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
    assert!(r.body.is_empty());
    // GET without a session, and other methods
    let r = http(&s.app, "GET", &[("accept", "text/event-stream")], "").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = http(&s.app, "PUT", &[], "").await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    // Origin: a foreign page is refused, the server's own pages are not
    let mut h = base.to_vec();
    h.push(("mcp-name", "sparql_query"));
    h.push(("host", "127.0.0.1:3030"));
    let mut evil = h.clone();
    evil.push(("origin", "https://evil.example"));
    let r = http(&s.app, "POST", &evil, &call).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    let mut own = h.clone();
    own.push(("origin", "http://127.0.0.1:3030"));
    let r = http(&s.app, "POST", &own, &call).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    // DNS rebinding: a Host the open server does not answer to
    let mut rebound = h.clone();
    rebound.retain(|(k, _)| *k != "host");
    rebound.push(("host", "attacker.example"));
    let r = http(&s.app, "POST", &rebound, &call).await;
    assert_eq!(r.status, StatusCode::MISDIRECTED_REQUEST, "{}", r.body);
}

#[tokio::test(flavor = "multi_thread")]
async fn endpoint_needs_the_flag() {
    let s = server();
    let r = http(&s.app, "POST", &[ACCEPT, JSON_CT], "{}").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_sessions() {
    let s = open(&[], false);
    let (sid, r) = initialize(&s.app, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!sid.is_empty(), "no session id");
    assert!(r.content_type().starts_with("text/event-stream"));
    let init = r.rpc();
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        init["result"]["capabilities"],
        json!({"completions": {}, "extensions": {"io.modelcontextprotocol/tasks": {}}, "prompts": {}, "resources": {"listChanged": true}, "tools": {"listChanged": true}})
    );
    let r = legacy(
        &s.app,
        &sid,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let r = legacy(
        &s.app,
        &sid,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "list_datasets", "arguments": {}}}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let rpc = r.rpc();
    assert_eq!(rpc["id"], 2);
    assert_eq!(
        rpc["result"]["structuredContent"]["datasets"][0]["name"],
        "t"
    );
    assert!(rpc["result"].get("resultType").is_none(), "{rpc}");
    // an unknown session
    let r = legacy(
        &s.app,
        "no-such-session",
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // DELETE ends the session
    let r = http(
        &s.app,
        "DELETE",
        &[
            ("mcp-session-id", &sid),
            ("mcp-protocol-version", "2025-11-25"),
        ],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
    let r = legacy(
        &s.app,
        &sid,
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list"}),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_are_bounded_or_off() {
    let s = open(&["--mcp-max-sessions", "1"], false);
    let (first, _) = initialize(&s.app, &[]).await;
    assert!(!first.is_empty());
    let (second, r) = initialize(&s.app, &[]).await;
    assert!(second.is_empty());
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.header("retry-after"), "5");
    // without sessions, legacy clients get JSON and no session id
    let s = open(&["--mcp-max-sessions", "0"], false);
    let (sid, r) = initialize(&s.app, &[]).await;
    assert!(sid.is_empty());
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.content_type().starts_with("application/json"));
    assert_eq!(r.rpc()["result"]["protocolVersion"], "2025-11-25");
    let r = http(
        &s.app,
        "POST",
        &[ACCEPT, JSON_CT, ("mcp-protocol-version", "2025-11-25")],
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "list_datasets", "arguments": {}}}).to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        r.rpc()["result"]["structuredContent"]["datasets"][0]["name"],
        "t"
    );
}

// ------------------------------------------------------------- the write tool ------

#[tokio::test(flavor = "multi_thread")]
async fn a27_update_is_opt_in() {
    // off by default
    let s = open(&[], false);
    assert!(
        !tool_names(&s.app, &[])
            .await
            .contains(&"sparql_update".into())
    );
    let r = modern(
        &s.app,
        "tools/call",
        json!({"name": "sparql_update", "arguments": {"update": "CLEAR ALL"}}),
        &[],
    )
    .await;
    assert_eq!(r.rpc()["error"]["code"], -32602);
    // never on a read-only server
    let s = open(&["--mcp-allow-update"], true);
    assert!(
        !tool_names(&s.app, &[])
            .await
            .contains(&"sparql_update".into())
    );
    // on when enabled
    let s = open(&["--mcp-allow-update"], false);
    let r = modern(&s.app, "tools/list", json!({}), &[]).await;
    let tools = r.rpc()["result"]["tools"].as_array().unwrap().clone();
    let last = tools.iter().find(|t| t["name"] == "sparql_update").unwrap();
    assert_eq!(last["annotations"]["destructiveHint"], true);
    assert_eq!(last["annotations"]["readOnlyHint"], false);
    let r = tool(
        &s.app,
        "sparql_update",
        json!({"update": "INSERT DATA { ex:carol a ex:Person }", "message": "add carol"}),
        &[],
    )
    .await;
    let out = &r["structuredContent"];
    assert_eq!(out["dataset"], "t");
    assert_eq!(out["committed"], true);
    assert_eq!(out["commit"], 2);
    assert_eq!(out["inserted"], 1);
    assert_eq!(out["deleted"], 0);
    assert_eq!(out["message"], "add carol");
    assert_eq!(s.state.get("t").unwrap().store.head_commit().seq, 2);
    // the commit message header applies when the call names none
    let r = tool(
        &s.app,
        "sparql_update",
        json!({"update": "INSERT DATA { ex:dave a ex:Person }"}),
        &[("sparkles-commit-message", "add dave")],
    )
    .await;
    assert_eq!(r["structuredContent"]["message"], "add dave");
    let commits = send(
        &s.app,
        Request::get("/$/commits/t?limit=1")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .json();
    assert_eq!(commits["commits"][0]["message"], "add dave", "{commits}");
    // LOAD is refused and nothing is written
    let r = tool(
        &s.app,
        "sparql_update",
        json!({"update": "LOAD <file:///etc/hosts>"}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "load-disabled");
    let r = tool(
        &s.app,
        "sparql_update",
        json!({"update": "LOAD <http://127.0.0.1:9/x.ttl> INTO GRAPH <urn:g>"}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "load-disabled");
    // a query is not an update
    let r = tool(&s.app, "sparql_update", json!({"update": "ASK {}"}), &[]).await;
    assert_eq!(tool_error(&r), "not-an-update");
    let r = tool(
        &s.app,
        "sparql_update",
        json!({"update": "INSERT DATA {"}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "syntax");
    let r = tool(
        &s.app,
        "sparql_update",
        json!({"update": "INSERT DATA { ex:x ex:y ex:z }", "message": "a\u{7}b"}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "bad-argument");
    assert_eq!(s.state.get("t").unwrap().store.head_commit().seq, 3);
    // queries see the write, and list_datasets says the dataset is writable
    let r = tool(&s.app, "list_datasets", json!({}), &[]).await;
    assert_eq!(r["structuredContent"]["datasets"][0]["writable"], true);
    assert_eq!(r["structuredContent"]["limits"]["updates"], true);
}

#[cfg(feature = "shacl")]
#[tokio::test(flavor = "multi_thread")]
async fn updates_pass_write_time_validation() {
    let s = open(&["--mcp-allow-update"], false);
    let shapes = "@prefix sh: <http://www.w3.org/ns/shacl#> .\n@prefix ex: <http://ex.org/> .\n@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\nex:S a sh:NodeShape ; sh:targetClass ex:Person ; sh:property [ sh:path rdfs:label ; sh:minCount 1 ] .\n";
    let cfg = json!({"mode": "reject", "shapes": {"inline": shapes}});
    let put = Request::put("/$/validation/t")
        .header("content-type", "application/json")
        .body(Body::from(cfg.to_string()))
        .unwrap();
    let r = send(&s.app, put).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let head = s.state.get("t").unwrap().store.head_commit().seq;
    // a person without a label is rejected, and nothing is written
    let r = tool(
        &s.app,
        "sparql_update",
        json!({"update": "INSERT DATA { ex:eve a ex:Person }"}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "validation-failed");
    assert_eq!(
        r["_meta"]["io.github.kclejeune.sparkles/error"]["status"],
        422
    );
    assert_eq!(s.state.get("t").unwrap().store.head_commit().seq, head);
    // a conforming write passes and reports the validation
    let r = tool(
        &s.app,
        "sparql_update",
        json!({"update": "INSERT DATA { ex:eve a ex:Person ; rdfs:label \"Eve\" }"}),
        &[],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert!(r["structuredContent"]["validation"].is_object(), "{r}");
}

#[tokio::test(flavor = "multi_thread")]
async fn dry_runs_preview_updates() {
    let s = open(&["--mcp-allow-update"], false);
    let head = s.state.get("t").unwrap().store.head_commit().seq;
    let r = tool(
        &s.app,
        "sparql_update",
        json!({"update": "INSERT DATA { ex:carol a ex:Person . GRAPH ex:g { ex:carol ex:p 1 } }",
               "dryRun": true, "changes": 1, "message": "add carol"}),
        &[],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let out = &r["structuredContent"];
    assert_eq!(
        (
            &out["dryRun"],
            &out["committed"],
            &out["wouldCommit"],
            &out["outcome"]
        ),
        (&json!(true), &json!(false), &json!(true), &json!("commit"))
    );
    assert_eq!(
        (&out["head"], &out["commit"]),
        (&json!(head), &json!(head + 1))
    );
    assert_eq!((&out["inserted"], &out["deleted"]), (&json!(2), &json!(0)));
    assert_eq!(out["graphs"].as_array().unwrap().len(), 2, "{out}");
    assert_eq!(out["changes"]["total"], 2);
    assert_eq!(out["changes"]["truncated"], true);
    assert!(
        out["changes"]["quads"][0]
            .as_str()
            .unwrap()
            .starts_with("+ "),
        "{out}"
    );
    assert_eq!(out["message"], "add carol");
    assert_eq!(s.state.get("t").unwrap().store.head_commit().seq, head);
    // changes needs a dry run, and is bounded
    let r = tool(
        &s.app,
        "sparql_update",
        json!({"update": "INSERT DATA { ex:x ex:y ex:z }", "changes": 1}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "bad-argument");
    let r = tool(
        &s.app,
        "sparql_update",
        json!({"update": "INSERT DATA { ex:x ex:y ex:z }", "dryRun": true, "changes": 101}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "bad-argument");
    assert_eq!(s.state.get("t").unwrap().store.head_commit().seq, head);
}

#[cfg(feature = "shacl")]
#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_dry_run_is_a_result() {
    let s = open(&["--mcp-allow-update"], false);
    let shapes = "@prefix sh: <http://www.w3.org/ns/shacl#> .\n@prefix ex: <http://ex.org/> .\n@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\nex:S a sh:NodeShape ; sh:targetClass ex:Person ; sh:property [ sh:path rdfs:label ; sh:minCount 1 ] .\n";
    let cfg = json!({"mode": "reject", "shapes": {"inline": shapes}});
    let put = Request::put("/$/validation/t")
        .header("content-type", "application/json")
        .body(Body::from(cfg.to_string()))
        .unwrap();
    assert_eq!(send(&s.app, put).await.status, StatusCode::OK);
    let r = tool(
        &s.app,
        "sparql_update",
        json!({"update": "INSERT DATA { ex:eve a ex:Person }", "dryRun": true}),
        &[],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let out = &r["structuredContent"];
    assert_eq!(out["outcome"], "rejected");
    assert_eq!(out["validation"]["status"], "rejected");
    assert!(
        out["error"].as_str().unwrap().contains("blocking result"),
        "{out}"
    );
}

// --------------------------------------------------------- resources, prompts ------

#[tokio::test(flavor = "multi_thread")]
async fn a28_resources_and_prompts() {
    let s = open(&[], false);
    let r = modern(&s.app, "resources/list", json!({}), &[]).await;
    let list = r.rpc()["result"].clone();
    let uris: Vec<&str> = list["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert_eq!(uris, ["sparkles://t/prefixes", "sparkles://t/schema"]);
    assert_eq!(list["cacheScope"], "private");
    let r = modern(&s.app, "resources/templates/list", json!({}), &[]).await;
    let templates = r.rpc()["result"]["resourceTemplates"].clone();
    assert_eq!(templates.as_array().unwrap().len(), 3, "{templates}");
    // the schema resource is the describe_schema summary
    let r = modern(
        &s.app,
        "resources/read",
        json!({"uri": "sparkles://t/schema"}),
        &[],
    )
    .await;
    let res = r.rpc()["result"].clone();
    assert_eq!(res["ttlMs"], 30000);
    assert_eq!(res["cacheScope"], "private");
    let contents = &res["contents"][0];
    assert_eq!(contents["mimeType"], "application/json");
    let schema: J = serde_json::from_str(contents["text"].as_str().unwrap()).unwrap();
    let summary = tool(&s.app, "describe_schema", json!({}), &[]).await;
    assert_eq!(schema, summary["structuredContent"]);
    let r = modern(
        &s.app,
        "resources/read",
        json!({"uri": "sparkles://t/prefixes"}),
        &[],
    )
    .await;
    let text = r.rpc()["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("PREFIX ex: <http://ex.org/>\n"), "{text}");
    for uri in ["sparkles://nope/schema", "sparkles://t/nope", "http://x/"] {
        let r = modern(&s.app, "resources/read", json!({"uri": uri}), &[]).await;
        assert_eq!(r.rpc()["error"]["code"], -32602, "{uri}: {}", r.body);
    }
    // prompts
    let r = modern(&s.app, "prompts/list", json!({}), &[]).await;
    let names: Vec<String> = r.rpc()["result"]["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        [
            "explore_dataset",
            "answer_question",
            "run_stored_query",
            "ask_graph",
            "explain_term",
            "agent_memory",
            "ingest_document",
            "consolidate_memory"
        ]
    );
    let r = modern(
        &s.app,
        "prompts/get",
        json!({"name": "explore_dataset", "arguments": {"dataset": "t"}}),
        &[],
    )
    .await;
    let msg = r.rpc()["result"]["messages"][0].clone();
    assert_eq!(msg["role"], "user");
    let text = msg["content"]["text"].as_str().unwrap();
    assert!(text.contains("PREFIX ex: <http://ex.org/>"), "{text}");
    assert!(
        text.ends_with("Start by calling describe_schema for dataset t."),
        "{text}"
    );
    let r = modern(
        &s.app,
        "prompts/get",
        json!({"name": "answer_question", "arguments": {"dataset": "t", "question": "Who knows Bob?"}}),
        &[],
    )
    .await;
    let text = r.rpc()["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        text.starts_with("Answer the question using dataset t: Who knows Bob?"),
        "{text}"
    );
    for args in [
        json!({}),
        json!({"dataset": "nope", "question": "?"}),
        json!({"dataset": "t"}),
    ] {
        let r = modern(
            &s.app,
            "prompts/get",
            json!({"name": "answer_question", "arguments": args}),
            &[],
        )
        .await;
        assert_eq!(r.rpc()["error"]["code"], -32602, "{args}: {}", r.body);
    }
}

// -------------------------------------------------------------------- budgets ------

#[tokio::test(flavor = "multi_thread")]
async fn server_budgets_cap_mcp_calls() {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    // `--query-memory-mb 1` caps the MCP budget of 2 GiB
    st.limits.query_memory_bytes = Some(1 << 20);
    st.mcp = Some(conf(&st, &[]));
    let state = Arc::new(st);
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..3000 {
        ttl.push_str(&format!("ex:s{i} ex:v {i} .\n"));
    }
    load(&state, "t", &ttl);
    let app = router(state.clone());
    let r = tool(
        &app,
        "sparql_query",
        json!({"query": "SELECT * WHERE { ?a ex:v ?x . ?b ex:v ?y }"}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "budget-memory");
    // and the server's default timeout bounds timeoutSeconds
    let r = tool(
        &app,
        "sparql_query",
        json!({"query": "ASK {}", "timeoutSeconds": 31}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "bad-argument");
}

/// A24: closing the connection of a stateless call stops its query, so the call's slot
/// is free for the next one long before the first call's timeout.
#[tokio::test(flavor = "multi_thread")]
async fn a24_disconnect_cancels_the_call() {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.limits.query_memory_bytes = None;
    st.mcp = Some(conf(&st, &["--mcp-max-concurrent", "1"]));
    let state = Arc::new(st);
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..5000 {
        ttl.push_str(&format!("ex:s{i} ex:v {i} .\n"));
    }
    load(&state, "t", &ttl);
    let app = router(state.clone());
    let slow = {
        let app = app.clone();
        tokio::spawn(async move {
            tool(
                &app,
                "sparql_query",
                json!({"query": "SELECT ?a ?b WHERE { ?a ex:v ?x . ?b ex:v ?y }", "timeoutSeconds": 30}),
                &[],
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!slow.is_finished(), "the query is running");
    slow.abort();
    let t = std::time::Instant::now();
    let r = tool(&app, "sparql_query", json!({"query": "ASK {}"}), &[]).await;
    assert_eq!(r["isError"], false, "{r}");
    assert!(
        t.elapsed() < Duration::from_secs(10),
        "the slot was held for {:?}",
        t.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dataset_flag_limits_what_mcp_sees() {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.mcp = Some(conf(&st, &["--mcp-dataset", "pub*"]));
    let state = Arc::new(st);
    load(&state, "public", FIXTURE);
    load(&state, "private", FIXTURE);
    let app = router(state.clone());
    assert_eq!(dataset_names(&app, &[]).await, ["public"]);
    let r = tool(
        &app,
        "sparql_query",
        json!({"dataset": "private", "query": "ASK {}"}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "unknown-dataset");
    // the only visible dataset may be left out
    let r = tool(&app, "sparql_query", json!({"query": "ASK {}"}), &[]).await;
    assert_eq!(r["isError"], false, "{r}");
}

// ----------------------------------------------------------------------- auth ------

#[cfg(feature = "auth")]
mod auth {
    use super::*;
    use crate::auth::Auth;
    use crate::http::router_tests::auth::{Fixture, b, bearer, config_text, tok};
    use crate::ratelimit::{RateLimiter, Sources};

    /// The auth fixture (anonymous reads `public`; alice is server-admin; bob writes
    /// `wiki` and reads `team-*`; token `etl` writes `wiki`), with MCP flags `flags` and
    /// rate-limit flags `limits`.
    fn authed(flags: &[&str], limits: &[&str], extra: &str) -> Server {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("auth.toml");
        let f = Fixture {
            extra: extra.to_string(),
            ..Default::default()
        };
        std::fs::write(&config, config_text(&f)).unwrap();
        let mut st =
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
        st.auth = Some(Arc::new(Auth::open(&config, dir.path()).unwrap().0));
        let sources = Sources {
            flags: limits.iter().map(|s| s.to_string()).collect(),
            auth: true,
            ..Default::default()
        };
        st.rate_limit = sources.load().unwrap().map(|cfg| {
            Arc::new(
                RateLimiter::new(&cfg)
                    .unwrap()
                    .with_keyer(Arc::new(crate::auth::PrincipalKeyer)),
            )
        });
        st.mcp = Some(conf(&st, flags));
        let state = Arc::new(st);
        for name in ["wiki", "team-a", "secret", "public"] {
            load(&state, name, FIXTURE);
        }
        state.set_phase(crate::obs::Phase::Ready);
        let app = router(state.clone());
        Server {
            _dir: dir,
            state,
            app,
        }
    }

    fn etl() -> String {
        bearer(&tok('A'))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn callers_see_their_datasets() {
        let s = authed(&["--mcp-allow-update"], &[], "");
        let bob = b("bob");
        let alice = b("alice");
        let anon = dataset_names(&s.app, &[]).await;
        assert_eq!(anon, ["public"]);
        let mut bobs = dataset_names(&s.app, &[("authorization", &bob)]).await;
        bobs.sort();
        assert!(
            bobs.contains(&"wiki".into()) && bobs.contains(&"team-a".into()),
            "{bobs:?}"
        );
        assert!(!bobs.contains(&"secret".into()), "{bobs:?}");
        let alices = dataset_names(&s.app, &[("authorization", &alice)]).await;
        assert_eq!(alices, ["public", "secret", "team-a", "wiki"]);
        // a hidden dataset is reported like a missing one
        let r = tool(
            &s.app,
            "sparql_query",
            json!({"dataset": "secret", "query": "ASK {}"}),
            &[("authorization", &bob)],
        )
        .await;
        assert_eq!(tool_error(&r), "unknown-dataset");
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(
            !text.contains("secret,") && !text.contains(", secret"),
            "{text}"
        );
        // resources and prompts follow the same rules
        let r = modern(&s.app, "resources/list", json!({}), &[]).await;
        assert_eq!(r.rpc()["result"]["resources"].as_array().unwrap().len(), 2);
        let r = modern(
            &s.app,
            "resources/read",
            json!({"uri": "sparkles://secret/prefixes"}),
            &[("authorization", &bob)],
        )
        .await;
        assert_eq!(r.rpc()["error"]["code"], -32602);
        let r = modern(
            &s.app,
            "prompts/get",
            json!({"name": "explore_dataset", "arguments": {"dataset": "secret"}}),
            &[],
        )
        .await;
        assert_eq!(r.rpc()["error"]["code"], -32602);
        // listings vary by caller: clients must not share them
        let r = modern(&s.app, "tools/list", json!({}), &[]).await;
        assert_eq!(r.rpc()["result"]["cacheScope"], "private");
    }

    /// Completions list only what the caller sees, and a task answers only its caller.
    #[tokio::test(flavor = "multi_thread")]
    async fn completions_and_tasks_follow_the_caller() {
        let s = authed(&["--mcp-task-after-ms", "0"], &[], "");
        let (bob, alice) = (b("bob"), b("alice"));
        let datasets = |auth: String| {
            let app = s.app.clone();
            async move {
                let r = modern(
                    &app,
                    "completion/complete",
                    json!({"ref": {"type": "ref/prompt", "name": "explore_dataset"},
                           "argument": {"name": "dataset", "value": ""}}),
                    &[("authorization", &auth)],
                )
                .await;
                r.rpc()["result"]["completion"]["values"].clone()
            }
        };
        assert_eq!(
            datasets(alice.clone()).await,
            json!(["public", "secret", "team-a", "wiki"])
        );
        let bobs = datasets(bob.clone()).await;
        assert!(
            !bobs.as_array().unwrap().contains(&json!("secret")),
            "{bobs}"
        );
        // alice's slow query becomes a task
        let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
        for i in 0..3000 {
            data.push_str(&format!("ex:s{i} ex:v {i} .\n"));
        }
        load(&s.state, "slow", &data);
        let tasks_meta = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {
                "extensions": {"io.modelcontextprotocol/tasks": {}}
            }
        });
        let r = modern(
            &s.app,
            "tools/call",
            json!({"_meta": tasks_meta, "name": "sparql_query", "arguments": {"dataset": "slow",
                   "query": "SELECT (COUNT(*) AS ?n) WHERE { ?a ex:v ?x . ?b ex:v ?y }"}}),
            &[("authorization", &alice)],
        )
        .await;
        let res = r.rpc()["result"].clone();
        assert_eq!(res["resultType"], "task", "{}", r.body);
        let id = res["taskId"].as_str().unwrap().to_string();
        let get = |auth: String| {
            let (app, meta, id) = (s.app.clone(), tasks_meta.clone(), id.clone());
            async move {
                modern(
                    &app,
                    "tasks/get",
                    json!({"_meta": meta, "taskId": id}),
                    &[("authorization", &auth)],
                )
                .await
                .rpc()
            }
        };
        // bob cannot see it
        let r = get(bob.clone()).await;
        assert_eq!(r["error"]["code"], -32602, "{r}");
        // alice polls it to its end
        let mut task = J::Null;
        for _ in 0..600 {
            task = get(alice.clone()).await["result"].clone();
            if task["status"] != "working" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(task["status"], "completed", "{task}");
        let text = task["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.ends_with("9000000"), "{text}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_follows_write_permission() {
        let s = authed(&["--mcp-allow-update"], &[], "");
        let bob = b("bob");
        let etl = etl();
        let has_update = |names: Vec<String>| names.contains(&"sparql_update".to_string());
        // anonymous reads only: no write tool
        assert!(!has_update(tool_names(&s.app, &[]).await));
        let r = tool(
            &s.app,
            "sparql_update",
            json!({"dataset": "public", "update": "INSERT DATA { ex:x ex:y ex:z }"}),
            &[],
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden");
        assert!(has_update(
            tool_names(&s.app, &[("authorization", &bob)]).await
        ));
        let before = s.state.get("team-a").unwrap().store.head_commit().seq;
        // bob reads team-a but may not write it
        let r = tool(
            &s.app,
            "sparql_update",
            json!({"dataset": "team-a", "update": "INSERT DATA { ex:x ex:y ex:z }"}),
            &[("authorization", &bob)],
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden");
        assert_eq!(
            r["_meta"]["io.github.kclejeune.sparkles/error"]["status"],
            403
        );
        assert_eq!(
            s.state.get("team-a").unwrap().store.head_commit().seq,
            before
        );
        // and writes wiki
        let r = tool(
            &s.app,
            "sparql_update",
            json!({"dataset": "wiki", "update": "INSERT DATA { ex:x ex:y ex:z }"}),
            &[("authorization", &bob)],
        )
        .await;
        assert_eq!(r["structuredContent"]["committed"], true, "{r}");
        // a token works the same way
        let r = tool(
            &s.app,
            "sparql_update",
            json!({"dataset": "wiki", "update": "INSERT DATA { ex:x2 ex:y ex:z }"}),
            &[("authorization", &etl)],
        )
        .await;
        assert_eq!(r["structuredContent"]["committed"], true, "{r}");
        // list_datasets tells writable datasets apart
        let r = tool(
            &s.app,
            "list_datasets",
            json!({}),
            &[("authorization", &bob)],
        )
        .await;
        for d in r["structuredContent"]["datasets"].as_array().unwrap() {
            assert_eq!(d["writable"], d["name"] == "wiki", "{d}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sign_in_is_required_when_anonymous_sees_nothing() {
        // the fixture's anonymous principal reads `public` only
        let s = authed(&["--mcp-dataset", "wiki"], &[], "");
        let r = modern(&s.app, "tools/list", json!({}), &[]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.body);
        assert!(
            r.header("www-authenticate").contains("Bearer"),
            "{:?}",
            r.headers
        );
        let r = modern(
            &s.app,
            "tools/list",
            json!({}),
            &[("authorization", &b("bob"))],
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        // bad credentials are refused by the auth layer
        let bad = bearer(&tok('Z'));
        let r = modern(&s.app, "tools/list", json!({}), &[("authorization", &bad)]).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        let r = modern(
            &s.app,
            "tools/list",
            json!({}),
            &[("authorization", &b("nobody"))],
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sessions_belong_to_their_caller() {
        let s = authed(&[], &[], "");
        let bob = b("bob");
        let alice = b("alice");
        let (sid, r) = initialize(&s.app, &[("authorization", &bob)]).await;
        assert_eq!(r.status, StatusCode::OK);
        assert!(!sid.is_empty());
        let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "list_datasets", "arguments": {}}});
        let r = legacy(&s.app, &sid, list.clone(), &[("authorization", &bob)]).await;
        assert_eq!(r.status, StatusCode::OK);
        // another caller with bob's session id
        let r = legacy(&s.app, &sid, list.clone(), &[("authorization", &alice)]).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
        let r = legacy(&s.app, &sid, list.clone(), &[]).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
        let r = http(
            &s.app,
            "DELETE",
            &[
                ("mcp-session-id", &sid),
                ("mcp-protocol-version", "2025-11-25"),
                ("authorization", &alice),
            ],
            "",
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
        // each request still runs as its own caller: bob's session shows bob's datasets
        let r = legacy(&s.app, &sid, list, &[("authorization", &bob)]).await;
        let names: Vec<String> = r.rpc()["result"]["structuredContent"]["datasets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"wiki".into()), "{names:?}");
        assert!(!names.contains(&"secret".into()), "{names:?}");
        // and a call without credentials in bob's session is refused, not run as bob
        let r = tool(&s.app, "list_datasets", json!({}), &[]).await;
        assert_eq!(r["structuredContent"]["datasets"][0]["name"], "public");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rate_limits_apply_per_class() {
        let s = authed(
            &["--mcp-allow-update"],
            &["query=2/min", "update=1/min"],
            "",
        );
        let bob = b("bob");
        let h = [("authorization", bob.as_str())];
        let ask = json!({"dataset": "wiki", "query": "ASK {}"});
        for _ in 0..2 {
            let r = tool(&s.app, "sparql_query", ask.clone(), &h).await;
            assert_eq!(r["isError"], false, "{r}");
        }
        let r = modern(
            &s.app,
            "tools/call",
            json!({"name": "sparql_query", "arguments": ask}),
            &h,
        )
        .await;
        assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.body);
        assert!(!r.header("retry-after").is_empty());
        // listings are not charged
        let r = modern(&s.app, "tools/list", json!({}), &h).await;
        assert_eq!(r.status, StatusCode::OK);
        // the update class has its own budget
        let up = json!({"dataset": "wiki", "update": "INSERT DATA { ex:x ex:y ex:z }"});
        let r = tool(&s.app, "sparql_update", up.clone(), &h).await;
        assert_eq!(r["isError"], false, "{r}");
        let r = modern(
            &s.app,
            "tools/call",
            json!({"name": "sparql_update", "arguments": up}),
            &h,
        )
        .await;
        assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.body);
        // a SPARQL query of the same caller shares the query budget
        let r = call_get(&s.app, "/wiki/sparql?query=ASK%7B%7D", &bob).await;
        assert_eq!(r, StatusCode::TOO_MANY_REQUESTS);
        // another caller has a budget of its own
        let alice = b("alice");
        let r = tool(
            &s.app,
            "sparql_query",
            json!({"dataset": "wiki", "query": "ASK {}"}),
            &[("authorization", &alice)],
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
    }

    async fn call_get(app: &Router, uri: &str, auth: &str) -> StatusCode {
        let req = Request::get(uri)
            .header("authorization", auth)
            .extension(ConnectInfo(Peer::Tcp("127.0.0.2:1".parse().unwrap())))
            .body(Body::empty())
            .unwrap();
        app.clone().oneshot(req).await.unwrap().status()
    }

    /// MCP tools through grants limited to some graphs (C12): queries and schemas see
    /// the view, listings leave out counts, validation is refused, and updates write the
    /// write graphs only.
    #[tokio::test(flavor = "multi_thread")]
    async fn tools_follow_graph_grants() {
        use crate::http::router_tests::auth::graphs::{DATA, users};
        let s = authed(&["--mcp-allow-update"], &[], &users());
        let ds = s.state.attach("graphs", DbType::Mem, None).unwrap();
        ds.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::TriG,
                None,
            )])
            .unwrap();
        let q = |user: &str| {
            (
                json!({"dataset": "graphs", "query": "SELECT ?s { GRAPH ?g { ?s ?p ?o } }"}),
                b(user),
            )
        };
        let text = |r: &J| {
            r["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        let (args, auth) = q("gfull");
        let full = text(&tool(&s.app, "sparql_query", args, &[("authorization", &auth)]).await);
        assert!(full.contains("b1"), "{full}");
        let (args, auth) = q("gra");
        let r = tool(&s.app, "sparql_query", args, &[("authorization", &auth)]).await;
        assert_eq!(r["isError"], false, "{r}");
        let mine = text(&r);
        assert!(mine.contains("a1") && !mine.contains("b1"), "{mine}");
        // an endpoint the grants do not name
        let (args, auth) = q("gep");
        let r = tool(&s.app, "sparql_query", args, &[("authorization", &auth)]).await;
        assert_eq!(tool_error(&r), "forbidden");
        // listings without counts, schemas of the view, commits without counts
        let gra = b("gra");
        let r = tool(
            &s.app,
            "list_datasets",
            json!({}),
            &[("authorization", &gra)],
        )
        .await;
        let d = r["structuredContent"]["datasets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == "graphs")
            .unwrap()
            .clone();
        assert_eq!(d["quads"], 3, "{d}");
        let r = tool(
            &s.app,
            "describe_schema",
            json!({"dataset": "graphs", "graph": "union"}),
            &[("authorization", &gra)],
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        let r = tool(
            &s.app,
            "list_commits",
            json!({"dataset": "graphs"}),
            &[("authorization", &gra)],
        )
        .await;
        let c = &r["structuredContent"]["commits"][0];
        assert!(c["seq"].is_u64() && c.get("quads").is_none(), "{r}");
        // path search reads the caller's view: b/1 holds the only a1 → b1 edge
        let paths = |user: &str| {
            let auth = b(user);
            let app = s.app.clone();
            async move {
                tool(
                    &app,
                    "find_paths",
                    json!({"dataset": "graphs", "source": "http://ex/a1", "target": "http://ex/b1", "graph": "http://ex/b/1"}),
                    &[("authorization", &auth)],
                )
                .await
            }
        };
        let r = paths("gfull").await;
        assert_eq!(
            r["structuredContent"]["paths"][0]["edges"],
            json!(["ex:a1 ex:q ex:b1"]),
            "{r}"
        );
        let r = paths("gra").await;
        assert_eq!(r["structuredContent"]["paths"], json!([]), "{r}");
        // the recorded history leaves out the graphs the caller may not read
        let r = tool(
            &s.app,
            "list_changes",
            json!({"dataset": "graphs"}),
            &[("authorization", &gra)],
        )
        .await;
        let quads: Vec<String> = r["structuredContent"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["quad"].as_str().unwrap().to_string())
            .collect();
        assert!(!quads.is_empty(), "{r}");
        assert!(
            quads
                .iter()
                .all(|q| !q.contains("b/1") && !q.contains("ex:b1")),
            "{quads:?}"
        );
        // validation reads every graph
        let r = tool(
            &s.app,
            "validate_shacl",
            json!({"dataset": "graphs", "shapes": "@prefix sh: <http://www.w3.org/ns/shacl#> ."}),
            &[("authorization", &gra)],
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden");
        // updates: the write graphs only
        let grad = b("grad");
        let up = |u: &str| json!({"dataset": "graphs", "update": u});
        let r = tool(
            &s.app,
            "sparql_update",
            up("INSERT DATA { GRAPH <http://ex/a/1> { <http://ex/n> <http://ex/p> 1 } }"),
            &[("authorization", &grad)],
        )
        .await;
        assert_eq!(r["structuredContent"]["committed"], true, "{r}");
        let r = tool(
            &s.app,
            "sparql_update",
            up("INSERT DATA { GRAPH <http://ex/b/1> { <http://ex/n> <http://ex/p> 1 } }"),
            &[("authorization", &grad)],
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden");
    }

    /// MCP tools under protections of triples (C12 Phase 2): queries, listings and
    /// updates see and write what the caller's protections leave.
    #[tokio::test(flavor = "multi_thread")]
    async fn tools_follow_triple_protections() {
        use crate::http::router_tests::auth::triples::{DATA, users};
        let s = authed(&["--mcp-allow-update"], &[], &users());
        let ds = s.state.attach("hr", DbType::Mem, None).unwrap();
        ds.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let text = |r: &J| {
            r["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        let q = json!({"dataset": "hr", "query": "SELECT ?s ?v { ?s <http://ex/salary> ?v }"});
        let r = tool(
            &s.app,
            "sparql_query",
            q.clone(),
            &[("authorization", &b("thr"))],
        )
        .await;
        assert!(text(&r).contains("alice"), "{r}");
        let r = tool(
            &s.app,
            "sparql_query",
            q,
            &[("authorization", &b("tstaff"))],
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        assert!(!text(&r).contains("alice"), "{r}");
        let r = tool(
            &s.app,
            "describe_resource",
            json!({"dataset": "hr", "iri": "http://ex/alice"}),
            &[("authorization", &b("tstaff"))],
        )
        .await;
        assert!(!r.to_string().contains("salary"), "{r}");
        let r = tool(
            &s.app,
            "list_datasets",
            json!({}),
            &[("authorization", &b("tstaff"))],
        )
        .await;
        let d = r["structuredContent"]["datasets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == "hr")
            .unwrap()
            .clone();
        assert_eq!(d["quads"], 3, "{d}");
        let up = |u: &str| json!({"dataset": "hr", "update": u});
        let r = tool(
            &s.app,
            "sparql_update",
            up("INSERT DATA { <http://ex/x> <http://ex/salary> 1 }"),
            &[("authorization", &b("tstaff"))],
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden");
        let r = tool(
            &s.app,
            "sparql_update",
            up("INSERT DATA { <http://ex/x> <http://ex/salary> 1 }"),
            &[("authorization", &b("thr"))],
        )
        .await;
        assert_eq!(r["structuredContent"]["committed"], true, "{r}");
    }

    /// A memory tool's result as text: the text block, or the structured content.
    #[cfg(feature = "text")]
    fn result_text(r: &J) -> String {
        match r.get("structuredContent") {
            Some(s) => s.to_string(),
            None => r["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        }
    }

    /// explain_query through graph grants (C12): a term that occurs only in a hidden
    /// graph is reported as an absent one is, and a view without estimates shows none
    /// and still warns about a missing LIMIT.
    #[tokio::test(flavor = "multi_thread")]
    async fn explain_query_follows_graph_grants() {
        use crate::http::router_tests::auth::graphs::{DATA, users};
        let s = authed(&[], &[], &users());
        let ds = s.state.attach("graphs", DbType::Mem, None).unwrap();
        ds.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::TriG,
                None,
            )])
            .unwrap();
        let explain = |user: &'static str, query: String| {
            let app = s.app.clone();
            async move {
                tool(
                    &app,
                    "explain_query",
                    json!({"dataset": "graphs", "query": query}),
                    &[("authorization", &b(user))],
                )
                .await
            }
        };
        // ex:b1 occurs only in http://ex/b/1
        let term = |t: &str| format!("SELECT ?s WHERE {{ ?s <http://ex/q> <{t}> }} LIMIT 5");
        let full = explain("gfull", term("http://ex/b1")).await;
        assert_eq!(full["structuredContent"]["warnings"], json!([]), "{full}");
        let hidden = explain("gra", term("http://ex/b1")).await;
        let absent = explain("gra", term("http://ex/zz")).await;
        let warning = |r: &J, t: &str| {
            let w = &r["structuredContent"]["warnings"];
            assert_eq!(w.as_array().map(Vec::len), Some(1), "{r}");
            assert_eq!(w[0]["code"], "unknown-term", "{r}");
            w[0]["message"].as_str().unwrap().replace(t, "T")
        };
        assert_eq!(warning(&hidden, "ex:b1"), warning(&absent, "ex:zz"));
        // estimates: a number for the full view, null for the restricted one, which
        // still hears about the missing LIMIT
        let q = "SELECT ?s ?o WHERE { ?s <http://ex/p> ?o }".to_string();
        let full = explain("gfull", q.clone()).await;
        assert!(
            full["structuredContent"]["estimatedRows"].is_u64(),
            "{full}"
        );
        let r = explain("gra", q).await;
        let sc = &r["structuredContent"];
        assert!(sc["estimatedRows"].is_null(), "{r}");
        assert!(sc["plan"].as_str().unwrap().contains("est=?"), "{r}");
        let codes: Vec<&str> = sc["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap())
            .collect();
        assert_eq!(codes, ["no-limit"], "{r}");
    }

    /// The memory tools of C17 §6 through graph grants (C12): checks, candidates,
    /// facts and citations come from the caller's view only, and a term that exists
    /// only in a hidden graph is reported as an absent one is.
    #[cfg(feature = "text")]
    #[tokio::test(flavor = "multi_thread")]
    async fn memory_tools_follow_graph_grants() {
        use crate::http::router_tests::auth::graphs::{DATA, users};
        let s = authed(&[], &[], &users());
        let ds = s.state.attach("graphs", DbType::Mem, None).unwrap();
        ds.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::TriG,
                None,
            )])
            .unwrap();
        ds.store
            .enable_text(sparkles::text::TextConfig::default())
            .unwrap();
        let def: sparkles::stored::Definition = serde_json::from_value(json!({
            "query": "SELECT ?s WHERE { ?s <http://ex/p> ?label } LIMIT 10",
            "description": "Things with a fox label",
            "questions": ["Which foxes are there?"]
        }))
        .unwrap();
        ds.queries
            .put("foxes", def, sparkles::stored::Change::default())
            .unwrap();
        let call = |user: &'static str, name: &'static str, args: J| {
            let app = s.app.clone();
            async move { tool(&app, name, args, &[("authorization", &b(user))]).await }
        };

        // check_query: ex:b1 occurs only in http://ex/b/1
        let check = |user: &'static str, term: &'static str| {
            call(
                user,
                "check_query",
                json!({"dataset": "graphs", "query": format!("SELECT ?s WHERE {{ ?s <http://ex/q> <{term}> }} LIMIT 5")}),
            )
        };
        let full = check("gfull", "http://ex/b1").await;
        assert_eq!(full["structuredContent"]["issues"], json!([]), "{full}");
        let hidden = check("gra", "http://ex/b1").await;
        let absent = check("gra", "http://ex/zz").await;
        let issue = |r: &J, term: &str| {
            let mut i = r["structuredContent"]["issues"][0].clone();
            assert_eq!(i["code"], "unknown-term", "{r}");
            i["term"] = J::Null;
            i["message"] = i["message"].as_str().unwrap().replace(term, "T").into();
            i
        };
        assert_eq!(issue(&hidden, "ex:b1"), issue(&absent, "ex:zz"));

        // similar_queries: only callers who may run queries see stored ones
        let r = call(
            "gra",
            "similar_queries",
            json!({"dataset": "graphs", "question": "which foxes"}),
        )
        .await;
        assert_eq!(r["structuredContent"]["queries"][0]["name"], "foxes", "{r}");
        let r = call(
            "gep",
            "similar_queries",
            json!({"dataset": "graphs", "question": "which foxes"}),
        )
        .await;
        assert_eq!(r["structuredContent"]["queries"], json!([]), "{r}");
        // check_query and recall need the query endpoint
        let r = call(
            "gep",
            "check_query",
            json!({"dataset": "graphs", "query": "ASK {}"}),
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden");

        // link_entities: a hidden entity is never a candidate
        let link = |user: &'static str| {
            call(
                user,
                "link_entities",
                json!({"dataset": "graphs", "labelPredicates": ["http://ex/p"],
                       "mentions": [{"text": "secret fox"}, {"text": "fox"}]}),
            )
        };
        let r = link("gfull").await;
        let m = &r["structuredContent"]["mentions"];
        assert_eq!(m[0]["verdict"], "exact", "{r}");
        assert_eq!(m[0]["candidates"][0]["iri"], "ex:b1", "{r}");
        let r = link("gra").await;
        let m = &r["structuredContent"]["mentions"];
        // the words match a visible entity, never the hidden one
        assert_eq!(m[0]["verdict"], "candidates", "{r}");
        let fox: Vec<&str> = m[1]["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["iri"].as_str().unwrap())
            .collect();
        assert_eq!(fox, ["ex:a1"], "{r}");
        assert!(!result_text(&r).contains("b1"), "{r}");

        // recall: facts and citations of the view only
        for args in [
            json!({"dataset": "graphs", "seeds": ["http://ex/a1"], "hops": 2}),
            json!({"dataset": "graphs", "query": "fox", "includeSuperseded": true}),
        ] {
            let r = call("gfull", "recall", args.clone()).await;
            let t = result_text(&r);
            assert!(
                t.contains("ex:b1") && t.contains("graph=<http://ex/b/1>"),
                "{t}"
            );
            let r = call("gra", "recall", args.clone()).await;
            assert_eq!(r["isError"], false, "{r}");
            let t = result_text(&r);
            assert!(t.contains("ex:a1"), "{t}");
            for hidden in ["b1", "http://ex/b/1", "default fox", "graph=default"] {
                assert!(!t.contains(hidden), "{hidden} in {t}");
            }
        }
        let r = call(
            "gra",
            "recall",
            json!({"dataset": "graphs", "seeds": ["http://ex/b1"]}),
        )
        .await;
        let t = result_text(&r);
        assert!(t.starts_with("# dataset=graphs "), "{t}");
        assert!(!t.contains("secret") && !t.contains("ex:a1"), "{t}");
    }

    /// The memory tools under protections of triples (C12b): a protected predicate is
    /// unknown to `check_query`, never suggested, and its facts never reach `recall`; a
    /// protected entity is never a candidate.
    #[cfg(feature = "text")]
    #[tokio::test(flavor = "multi_thread")]
    async fn memory_tools_follow_triple_protections() {
        use crate::http::router_tests::auth::triples::{DATA, users};
        let s = authed(&[], &[], &users());
        let ds = s.state.attach("hr", DbType::Mem, None).unwrap();
        ds.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        ds.store
            .enable_text(sparkles::text::TextConfig::default())
            .unwrap();
        let call = |user: &'static str, name: &'static str, args: J| {
            let app = s.app.clone();
            async move { tool(&app, name, args, &[("authorization", &b(user))]).await }
        };
        let check = |user: &'static str, p: &'static str| {
            call(
                user,
                "check_query",
                json!({"dataset": "hr", "query": format!("SELECT ?v WHERE {{ ?s <{p}> ?v }} LIMIT 5")}),
            )
        };
        let r = check("thr", "http://ex/salary").await;
        assert_eq!(r["structuredContent"]["ok"], true, "{r}");
        let hidden = check("tstaff", "http://ex/salary").await;
        let absent = check("tstaff", "http://ex/wage").await;
        let issue = |r: &J, term: &str| {
            let mut i = r["structuredContent"]["issues"][0].clone();
            assert_eq!(i["code"], "unknown-predicate", "{r}");
            i["term"] = J::Null;
            i["message"] = i["message"].as_str().unwrap().replace(term, "T").into();
            i
        };
        assert_eq!(issue(&hidden, "ex:salary"), issue(&absent, "ex:wage"));
        // a misspelling suggests the predicate only to those who may see it
        let r = check("thr", "http://ex/salry").await;
        assert!(result_text(&r).contains("salary"), "{r}");
        let r = check("tstaff", "http://ex/salry").await;
        assert!(!result_text(&r).contains("salary\""), "{r}");
        assert!(
            r["structuredContent"]["issues"][0]["suggestions"]
                .as_array()
                .is_none_or(|s| s.iter().all(|s| s["term"] != "ex:salary")),
            "{r}"
        );

        // link_entities: Bob is a patient
        let link = |user: &'static str| {
            call(
                user,
                "link_entities",
                json!({"dataset": "hr", "mentions": [{"text": "bob fox"}, {"text": "fox"}]}),
            )
        };
        let r = link("tdoc").await;
        assert_eq!(
            r["structuredContent"]["mentions"][0]["verdict"], "exact",
            "{r}"
        );
        let r = link("tstaff").await;
        assert_ne!(
            r["structuredContent"]["mentions"][0]["verdict"], "exact",
            "{r}"
        );
        assert!(!result_text(&r).contains("ex:bob"), "{r}");

        // recall: no salary without the role
        let args = json!({"dataset": "hr", "seeds": ["http://ex/alice"], "format": "json"});
        let r = call("thr", "recall", args.clone()).await;
        assert!(result_text(&r).contains("salary"), "{r}");
        let r = call("tstaff", "recall", args).await;
        let t = result_text(&r);
        assert!(t.contains("ex:alice") && !t.contains("salary"), "{t}");
        let r = call(
            "tstaff",
            "recall",
            json!({"dataset": "hr", "query": "fox", "hops": 2}),
        )
        .await;
        let t = result_text(&r);
        assert!(!t.contains("salary") && !t.contains("ex:bob"), "{t}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn service_needs_federate() {
        let s = authed(&["--mcp-allow-service"], &[], "");
        let q = json!({"dataset": "wiki", "query": "SELECT * WHERE { SERVICE <http://127.0.0.1:9/sparql> { ?s ?p ?o } }"});
        let r = tool(&s.app, "sparql_query", q, &[("authorization", &b("bob"))]).await;
        let code = tool_error(&r);
        assert!(
            code == "forbidden" || code == "service-disabled",
            "{code}: {r}"
        );
    }
}
