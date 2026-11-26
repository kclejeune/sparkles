//! End-to-end memory-credit tests for MCP transports, session replay, and tasks (item 1305).

use super::adapter::Adapter;
use super::http::{HttpConf, route_with_session_config};
use super::*;
use crate::auth::{Peer, Principal};
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use rmcp::transport::streamable_http_server::session::local::SessionConfig;
use serde_json::{Value, json};
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::pool::QueryPool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// Build a dataset with `rows` triples whose literal values are `payload_len` ASCII bytes each.
fn big_turtle(rows: usize, payload_len: usize) -> String {
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    let pad = "a".repeat(payload_len);
    for i in 0..rows {
        ttl.push_str(&format!("ex:r{i} ex:v \"{i:04}-{pad}\" .\n"));
    }
    ttl
}

fn pooled_state(
    pool_bytes: u64,
    ttl: &str,
    mcp_cfg: McpConfig,
    max_sessions: usize,
) -> (tempfile::TempDir, Arc<AppState>, Arc<QueryPool>) {
    pooled_state_with_admission_wait(
        pool_bytes,
        ttl,
        mcp_cfg,
        max_sessions,
        Duration::from_secs(2),
    )
}

fn pooled_state_with_admission_wait(
    pool_bytes: u64,
    ttl: &str,
    mcp_cfg: McpConfig,
    max_sessions: usize,
    admission_wait: Duration,
) -> (tempfile::TempDir, Arc<AppState>, Arc<QueryPool>) {
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        mcp_cfg.max_timeout,
        Some(pool_bytes),
    )
    .unwrap();
    st.read_only = true;
    st.admission.wait = admission_wait;
    st.limits.query_memory_bytes = mcp_cfg.query_memory_bytes;
    st.limits.max_result_bytes = None;
    st.mcp = Some(Arc::new(HttpConf {
        cfg: mcp_cfg,
        max_sessions,
        shutdown: CancellationToken::new(),
        require_auth: false,
    }));
    let state = Arc::new(st);
    let ds = state.attach("t", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            ttl.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let pool = state.catalog.pool().expect("pooled state").clone();
    (dir, state, pool)
}

async fn wait_for(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    pred()
}

fn modern_meta(with_tasks: bool) -> Value {
    let caps = if with_tasks {
        json!({ "extensions": { "io.modelcontextprotocol/tasks": {} } })
    } else {
        json!({})
    };
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": caps
    })
}

fn large_query_args(max_bytes: usize) -> Value {
    json!({
        "dataset": "t",
        "query": "PREFIX ex: <http://ex.org/> SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ?s",
        "format": "json",
        "maxRows": 500,
        "maxBytes": max_bytes,
        "maxTermChars": 2000
    })
}

fn mcp_http_router(
    state: Arc<AppState>,
    session_config: SessionConfig,
    task_ttl: Option<Duration>,
) -> Router {
    let route = route_with_session_config(&state, session_config, task_ttl).unwrap();
    Router::new()
        .route("/$/mcp", route)
        .layer(axum::middleware::from_fn(
            |mut req: Request<Body>, next: axum::middleware::Next| async move {
                req.extensions_mut().insert(Principal::local());
                next.run(req).await
            },
        ))
        .with_state(state)
}

fn http_req(method: &str, headers: &[(&str, &str)], body: &str) -> Request<Body> {
    let mut req = Request::builder()
        .method(method)
        .uri("/$/mcp")
        .extension(ConnectInfo(Peer::Tcp("127.0.0.2:1".parse().unwrap())));
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    req.body(Body::from(body.to_string())).unwrap()
}

const ACCEPT: (&str, &str) = ("accept", "application/json, text/event-stream");
const JSON_CT: (&str, &str) = ("content-type", "application/json");

#[tokio::test(flavor = "multi_thread")]
async fn stdio_backpressure_holds_pool_credit_until_frame_is_read() {
    let ttl = big_turtle(180, 900);
    let cfg = McpConfig {
        max_bytes: 1 << 20,
        ..Default::default()
    };
    let (_dir, state, pool) = pooled_state(32 << 20, &ttl, cfg.clone(), 0);
    let server = McpServer::new(state, cfg);

    // Small duplex buffer (4 KiB) forces the ~180 KiB NDJSON response frame to block
    // mid-write inside `CreditedStdioTransport::send`.
    let (client, srv) = tokio::io::duplex(4096);
    let (sr, sw) = tokio::io::split(srv);
    let serve_handle = tokio::spawn(super::adapter::serve(Adapter::new(server), sr, sw));
    let (cr, mut cw) = tokio::io::split(client);

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "_meta": modern_meta(false),
            "name": "sparql_query",
            "arguments": large_query_args(512 << 10)
        }
    });
    let mut line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    cw.write_all(line.as_bytes()).await.unwrap();
    cw.flush().await.unwrap();

    // While the client has not read from the pipe, the response frame is blocked in
    // `write_all` and MUST hold its QueryPool grant.
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() > 0).await,
        "expected QueryPool credit to stay held while stdio response is backpressured"
    );
    // Give the query a brief moment to finish executing and confirm credit remains held
    // purely by the blocked transport write.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        pool.granted() > 0,
        "credit dropped before stdio client read the response"
    );

    let mut lines = BufReader::new(cr).lines();
    let reply_line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .expect("JSON-RPC reply");
    let reply: Value = serde_json::from_str(&reply_line).unwrap();
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let body: Value =
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["rows"].as_array().unwrap().len(), 180);

    // Once the frame is written to the pipe, QueryPool credit must drop back to 0.
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() == 0).await,
        "expected QueryPool credit to return to 0 after stdio write, still granted={}",
        pool.granted()
    );

    drop(cw);
    drop(lines);
    let _ = serve_handle.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stdio_disconnect_and_cancel_release_pool_credit_without_leak() {
    let ttl = big_turtle(180, 900);
    let cfg = McpConfig {
        max_bytes: 1 << 20,
        ..Default::default()
    };
    let (_dir, state, pool) = pooled_state(32 << 20, &ttl, cfg.clone(), 0);
    let server = McpServer::new(state, cfg);

    // Subcase 1: client drops pipe while large response is blocked mid-write.
    let (client, srv) = tokio::io::duplex(4096);
    let (sr, sw) = tokio::io::split(srv);
    let serve_handle = tokio::spawn(super::adapter::serve(Adapter::new(server.clone()), sr, sw));
    let (cr, mut cw) = tokio::io::split(client);

    let req = json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "tools/call",
        "params": {
            "_meta": modern_meta(false),
            "name": "sparql_query",
            "arguments": large_query_args(512 << 10)
        }
    });
    let mut line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    cw.write_all(line.as_bytes()).await.unwrap();
    cw.flush().await.unwrap();

    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() > 0).await,
        "expected credit held before disconnect"
    );
    drop(cr);
    drop(cw);
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() == 0).await,
        "credit leaked after stdio disconnect: granted={}",
        pool.granted()
    );
    let _ = serve_handle.await;

    // Subcase 2: client sends `notifications/cancelled` for an in-flight request.
    let (client, srv) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(srv);
    let serve_handle = tokio::spawn(super::adapter::serve(Adapter::new(server), sr, sw));
    let (cr, mut cw) = tokio::io::split(client);
    let req2 = json!({
        "jsonrpc": "2.0",
        "id": 12,
        "method": "tools/call",
        "params": {
            "_meta": modern_meta(false),
            "name": "sparql_query",
            "arguments": large_query_args(512 << 10)
        }
    });
    let cancel = json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": { "requestId": 12 }
    });
    let batch = format!(
        "{}\n{}\n",
        serde_json::to_string(&req2).unwrap(),
        serde_json::to_string(&cancel).unwrap()
    );
    cw.write_all(batch.as_bytes()).await.unwrap();
    cw.flush().await.unwrap();
    // Close client so session terminates cleanly after handling cancellation.
    drop(cw);
    drop(cr);
    let _ = serve_handle.await;
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() == 0).await,
        "credit leaked after notifications/cancelled: granted={}",
        pool.granted()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stdio_and_http_memory_budget_refusal_returns_clean_error_with_zero_leak() {
    let ttl = big_turtle(180, 900);
    // 96 KiB per-query memory cap cannot hold the ~180 KiB rendered result + wire frame.
    let cfg = McpConfig {
        max_bytes: 1 << 20,
        query_memory_bytes: Some(96 << 10),
        ..Default::default()
    };
    let (_dir, state, pool) = pooled_state(32 << 20, &ttl, cfg.clone(), 0);

    // 1. Over stdio
    {
        let server = McpServer::new(state.clone(), cfg.clone());
        let (client, srv) = tokio::io::duplex(1 << 20);
        let (sr, sw) = tokio::io::split(srv);
        let serve_handle = tokio::spawn(super::adapter::serve(Adapter::new(server), sr, sw));
        let (cr, mut cw) = tokio::io::split(client);
        let req = json!({
            "jsonrpc": "2.0",
            "id": 21,
            "method": "tools/call",
            "params": {
                "_meta": modern_meta(false),
                "name": "sparql_query",
                "arguments": large_query_args(512 << 10)
            }
        });
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        cw.write_all(line.as_bytes()).await.unwrap();
        cw.flush().await.unwrap();
        let mut lines = BufReader::new(cr).lines();
        let reply_line = lines.next_line().await.unwrap().unwrap();
        let reply: Value = serde_json::from_str(&reply_line).unwrap();
        assert_eq!(reply["id"], 21);
        assert_eq!(reply["result"]["isError"], true, "{reply}");
        assert_eq!(
            reply["result"]["_meta"]["io.github.kclejeune.sparkles/error"]["code"], "budget-memory",
            "{reply}"
        );
        drop(cw);
        drop(lines);
        let _ = serve_handle.await;
        assert!(
            wait_for(Duration::from_secs(5), || pool.granted() == 0).await,
            "stdio budget refusal leaked credit: granted={}",
            pool.granted()
        );
    }

    // 2. Over stateless HTTP
    {
        let app = mcp_http_router(state.clone(), SessionConfig::default(), None);
        let body = json!({
            "jsonrpc": "2.0",
            "id": 22,
            "method": "tools/call",
            "params": {
                "_meta": modern_meta(false),
                "name": "sparql_query",
                "arguments": large_query_args(512 << 10)
            }
        });
        let req = http_req(
            "POST",
            &[
                ACCEPT,
                JSON_CT,
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", "tools/call"),
                ("mcp-name", "sparql_query"),
            ],
            &body.to_string(),
        );
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let raw = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let reply: Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(reply["id"], 22);
        assert_eq!(reply["result"]["isError"], true, "{reply}");
        assert_eq!(
            reply["result"]["_meta"]["io.github.kclejeune.sparkles/error"]["code"], "budget-memory",
            "{reply}"
        );
        drop(raw);
        assert!(
            wait_for(Duration::from_secs(5), || pool.granted() == 0).await,
            "HTTP budget refusal leaked credit: granted={}",
            pool.granted()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn http_stateless_response_holds_credit_until_body_drained_or_dropped() {
    let ttl = big_turtle(180, 900);
    let cfg = McpConfig {
        max_bytes: 1 << 20,
        http: true,
        ..Default::default()
    };
    let (_dir, state, pool) = pooled_state(32 << 20, &ttl, cfg, 0);
    let app = mcp_http_router(state, SessionConfig::default(), None);

    let make_req = |id: u64| {
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "_meta": modern_meta(false),
                "name": "sparql_query",
                "arguments": large_query_args(512 << 10)
            }
        });
        http_req(
            "POST",
            &[
                ACCEPT,
                JSON_CT,
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", "tools/call"),
                ("mcp-name", "sparql_query"),
            ],
            &body.to_string(),
        )
    };

    // Subcase 1: Response body sits unread -> credit held; drained & dropped -> credit 0.
    let resp = app.clone().oneshot(make_req(31)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        pool.granted() > 0,
        "stateless HTTP response body must keep QueryPool credit until read"
    );
    let raw = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let reply: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(reply["id"], 31);
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let parsed: Value =
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(parsed["rows"].as_array().unwrap().len(), 180);
    drop(raw);
    assert_eq!(
        pool.granted(),
        0,
        "stateless HTTP response leaked credit after body drain"
    );

    // Subcase 2: Client disconnects and drops Response<Body> unread -> credit returns to 0.
    let resp = app.clone().oneshot(make_req(32)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        pool.granted() > 0,
        "expected credit held before client drop"
    );
    drop(resp);
    assert_eq!(
        pool.granted(),
        0,
        "stateless HTTP response leaked credit when dropped unread"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn http_legacy_session_replay_holds_credit_and_releases_on_delivery_ttl_and_delete() {
    let ttl = big_turtle(180, 900);
    let cfg = McpConfig {
        max_bytes: 1 << 20,
        http: true,
        ..Default::default()
    };
    let (_dir, state, pool) = pooled_state(32 << 20, &ttl, cfg, 16);
    let mut session_cfg = SessionConfig::default();
    session_cfg.completed_cache_ttl = Duration::from_millis(350);
    let app = mcp_http_router(state, session_cfg, None);

    // 1. Open legacy session via `initialize`.
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "credit-test", "version": "1"}
        }
    });
    let init_resp = app
        .clone()
        .oneshot(http_req("POST", &[ACCEPT, JSON_CT], &init.to_string()))
        .await
        .unwrap();
    assert_eq!(init_resp.status(), StatusCode::OK);
    let sid = init_resp
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let _ = axum::body::to_bytes(init_resp.into_body(), usize::MAX)
        .await
        .unwrap();

    // Send `notifications/initialized`.
    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    let init_ack = app
        .clone()
        .oneshot(http_req(
            "POST",
            &[
                ACCEPT,
                JSON_CT,
                ("mcp-protocol-version", "2025-11-25"),
                ("mcp-session-id", sid.as_str()),
            ],
            &initialized.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(init_ack.status(), StatusCode::ACCEPTED);

    let post_large = |id: u64| {
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "sparql_query",
                "arguments": large_query_args(512 << 10)
            }
        });
        http_req(
            "POST",
            &[
                ACCEPT,
                JSON_CT,
                ("mcp-protocol-version", "2025-11-25"),
                ("mcp-session-id", sid.as_str()),
            ],
            &body.to_string(),
        )
    };

    // Helper: extract the priming event `id:` from the first SSE frame, then drop the body
    // before reading the terminal response so the worker retains it in the replay cache.
    async fn start_and_disconnect(app: &Router, req: Request<Body>) -> String {
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let mut body = resp.into_body();
        let first = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .unwrap()
            .expect("priming frame")
            .unwrap()
            .into_data()
            .unwrap();
        let text = String::from_utf8_lossy(&first);
        let event_id = text
            .lines()
            .find_map(|l| l.strip_prefix("id: "))
            .unwrap_or_else(|| panic!("expected priming id in {text:?}"))
            .to_string();
        drop(body);
        event_id
    }

    // 2. Disconnect after priming, verify credit stays held in replay cache, then replay via
    //    `GET /$/mcp` with `Last-Event-ID` and verify credit returns to 0 after delivery.
    let priming_id = start_and_disconnect(&app, post_large(41)).await;
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() > 0).await,
        "expected credit held in session replay cache after client disconnect"
    );
    // Let the query finish into the replay cache before replaying.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        pool.granted() > 0,
        "replay cache dropped credit before Last-Event-ID reconnect"
    );

    let replay_resp = app
        .clone()
        .oneshot(http_req(
            "GET",
            &[
                ("accept", "text/event-stream"),
                ("mcp-protocol-version", "2025-11-25"),
                ("mcp-session-id", sid.as_str()),
                ("last-event-id", priming_id.as_str()),
            ],
            "",
        ))
        .await
        .unwrap();
    assert_eq!(replay_resp.status(), StatusCode::OK);
    let replay_bytes = axum::body::to_bytes(replay_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let replay_text = String::from_utf8_lossy(&replay_bytes);
    let payload_line = replay_text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .find(|d| d.starts_with('{'))
        .unwrap_or_else(|| panic!("expected replayed JSON-RPC payload in {replay_text:?}"));
    let reply: Value = serde_json::from_str(payload_line).unwrap();
    assert_eq!(reply["id"], 41);
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let parsed: Value =
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(parsed["rows"].as_array().unwrap().len(), 180);
    drop(replay_bytes);
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() == 0).await,
        "replay cache did not release credit after delivery: granted={}",
        pool.granted()
    );

    // 3. Disconnect with NO reconnect -> `completed_cache_ttl` (350 ms) evicts cache and
    //    releases QueryPool credit to 0 automatically.
    let _ = start_and_disconnect(&app, post_large(42)).await;
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() > 0).await,
        "expected credit held before replay TTL expiry"
    );
    assert!(
        wait_for(Duration::from_secs(3), || pool.granted() == 0).await,
        "replay cache did not expire after completed_cache_ttl: granted={}",
        pool.granted()
    );

    // 4. Disconnect with NO reconnect -> `DELETE /$/mcp` closes session and releases credit
    //    immediately.
    let _ = start_and_disconnect(&app, post_large(43)).await;
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() > 0).await,
        "expected credit held before DELETE"
    );
    let del = app
        .clone()
        .oneshot(http_req(
            "DELETE",
            &[
                ("mcp-protocol-version", "2025-11-25"),
                ("mcp-session-id", sid.as_str()),
            ],
            "",
        ))
        .await
        .unwrap();
    assert!(del.status().is_success(), "DELETE status {}", del.status());
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() == 0).await,
        "session DELETE leaked replay credit: granted={}",
        pool.granted()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_task_retains_credit_across_get_task_and_releases_on_ttl() {
    let ttl = big_turtle(180, 900);
    // 16 MiB pool equals one INITIAL_GRANT: proves `tasks/get` reuses the retained result's
    // PoolGrant instead of deadlocking on a second grant admission!
    let cfg = McpConfig {
        max_bytes: 1 << 20,
        task_after: Duration::ZERO,
        ..Default::default()
    };
    let (_dir, state, pool) = pooled_state(16 << 20, &ttl, cfg.clone(), 0);
    let server = McpServer::new_with_task_ttl(state, cfg, Duration::from_millis(450));

    let (client, srv) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(srv);
    let serve_handle = tokio::spawn(super::adapter::serve(Adapter::new(server), sr, sw));
    let (cr, mut cw) = tokio::io::split(client);
    let mut lines = BufReader::new(cr).lines();

    async fn send_recv(
        cw: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>,
        lines: &mut tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
        msg: Value,
    ) -> Value {
        let mut s = serde_json::to_string(&msg).unwrap();
        s.push('\n');
        cw.write_all(s.as_bytes()).await.unwrap();
        cw.flush().await.unwrap();
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    }

    // Start tool call that immediately becomes a background task (`task_after = 0`).
    let start_reply = send_recv(
        &mut cw,
        &mut lines,
        json!({
            "jsonrpc": "2.0",
            "id": 51,
            "method": "tools/call",
            "params": {
                "_meta": modern_meta(true),
                "name": "sparql_query",
                "arguments": large_query_args(512 << 10)
            }
        }),
    )
    .await;
    let task_id = match start_reply["result"]["taskId"].as_str() {
        Some(id) => id.to_string(),
        None => {
            panic!("expected task creation with task_after=0, got: {start_reply}");
        }
    };

    // Poll `tasks/get` until completed.
    let mut completed = None;
    for poll_id in 52..100 {
        let r = send_recv(
            &mut cw,
            &mut lines,
            json!({
                "jsonrpc": "2.0",
                "id": poll_id,
                "method": "tasks/get",
                "params": {
                    "_meta": modern_meta(true),
                    "taskId": task_id
                }
            }),
        )
        .await;
        if r["result"]["status"] == "completed" {
            completed = Some(r);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let completed = completed.expect("task should complete");
    assert_eq!(
        completed["result"]["resultType"], "complete",
        "GetTaskResult must carry outer resultType=complete: {completed}"
    );
    assert_eq!(completed["result"]["taskId"], task_id);
    assert_eq!(completed["result"]["status"], "completed");
    let tool_res = &completed["result"]["result"];
    assert_eq!(tool_res["resultType"], "complete", "{completed}");
    assert_eq!(tool_res["isError"], false, "{completed}");
    let parsed: Value =
        serde_json::from_str(tool_res["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(parsed["rows"].as_array().unwrap().len(), 180);

    // While the completed task stays retained in `Tasks`, QueryPool credit MUST remain held,
    // trimmed down from the 16 MiB INITIAL_GRANT slack to the actual retained payload size.
    let held = pool.granted();
    assert!(
        held > 0 && held < (4 << 20),
        "completed task should retain trimmed QueryPool credit (0 < held < 4 MiB), got {held}"
    );

    // A second `tasks/get` poll on the 16 MiB pool must succeed without deadlocking.
    let again = send_recv(
        &mut cw,
        &mut lines,
        json!({
            "jsonrpc": "2.0",
            "id": 200,
            "method": "tasks/get",
            "params": {
                "_meta": modern_meta(true),
                "taskId": task_id
            }
        }),
    )
    .await;
    assert_eq!(again["result"]["resultType"], "complete", "{again}");
    assert_eq!(again["result"]["status"], "completed", "{again}");
    assert_eq!(again["result"]["result"]["isError"], false);

    // Wait past the 450 ms task TTL: automatic eviction timer must drop the retained result
    // and return QueryPool credit to 0 without any further client calls.
    assert!(
        wait_for(Duration::from_secs(3), || pool.granted() == 0).await,
        "expired task leaked QueryPool credit: granted={}",
        pool.granted()
    );

    drop(cw);
    drop(lines);
    let _ = serve_handle.await;
}

/// F1: `explain_query` with `notes: true` under a tight `query_memory_bytes` ceiling must
/// return a bounded `budget-memory` error rather than bypassing admission uncredited.
#[tokio::test(flavor = "multi_thread")]
async fn explain_query_under_tight_budget_returns_bounded_error_without_bypass() {
    let ttl = big_turtle(40, 64);
    let cfg = McpConfig {
        max_bytes: 1 << 20,
        query_memory_bytes: Some(1536),
        ..Default::default()
    };
    let (_dir, state, pool) = pooled_state(32 << 20, &ttl, cfg.clone(), 0);
    let server = McpServer::new(state, cfg);

    let (client, srv) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(srv);
    let serve_handle = tokio::spawn(super::adapter::serve(Adapter::new(server), sr, sw));
    let (cr, mut cw) = tokio::io::split(client);
    let mut lines = BufReader::new(cr).lines();

    let req = json!({
        "jsonrpc": "2.0",
        "id": 61,
        "method": "tools/call",
        "params": {
            "_meta": modern_meta(false),
            "name": "explain_query",
            "arguments": {
                "dataset": "t",
                "query": "PREFIX ex: <http://ex.org/> SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ?s",
                "notes": true
            }
        }
    });
    let mut line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    cw.write_all(line.as_bytes()).await.unwrap();
    cw.flush().await.unwrap();

    let reply_line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let reply: Value = serde_json::from_str(&reply_line).unwrap();
    assert_eq!(reply["id"], 61);
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert_eq!(
        reply["result"]["_meta"]["io.github.kclejeune.sparkles/error"]["code"], "budget-memory",
        "{reply}"
    );
    assert!(
        reply["result"].get("structuredContent").is_none(),
        "oversized explain_query payload must not escape uncredited: {reply}"
    );
    drop((cw, lines));
    let _ = serve_handle.await;
    assert!(wait_for(Duration::from_secs(5), || pool.granted() == 0).await);
}

/// Custom `AsyncWrite` that accepts at most `chunk` bytes per `poll_write` and yields
/// `Poll::Pending` on alternate polls to stress partial writes and cancel-safety.
struct ShortWriter<W> {
    inner: W,
    chunk: usize,
    yield_next: bool,
}

impl<W: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for ShortWriter<W> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.yield_next {
            self.yield_next = false;
            cx.waker().wake_by_ref();
            return std::task::Poll::Pending;
        }
        self.yield_next = true;
        let n = buf.len().min(self.chunk);
        std::pin::Pin::new(&mut self.inner).poll_write(cx, &buf[..n])
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// F3, F8, F9:
/// - `notifications/cancelled` drops a queued large stdio response and releases its credit
///   even while stdout remains open and backpressured;
/// - duplicate in-flight `RequestId` is rejected with `-32600` without corrupting the
///   original request;
/// - interleaved protocol-error writes on a short-writing sink never tear NDJSON lines.
#[tokio::test(flavor = "multi_thread")]
async fn stdio_cancel_under_open_backpressure_dup_id_and_short_write_framing() {
    let ttl = big_turtle(180, 900);
    let cfg = McpConfig {
        max_bytes: 1 << 20,
        ..Default::default()
    };
    let (_dir, state, pool) = pooled_state(64 << 20, &ttl, cfg.clone(), 0);
    let server = McpServer::new(state, cfg);

    // Subcase 1 (F9 + F8): open 4 KiB backpressured pipe — cancel queued req 2 while req 1
    // blocks the pipe, and reject a duplicate in-flight req 1.
    {
        let (client, srv) = tokio::io::duplex(4096);
        let (sr, sw) = tokio::io::split(srv);
        let serve_handle =
            tokio::spawn(super::adapter::serve(Adapter::new(server.clone()), sr, sw));
        let (cr, mut cw) = tokio::io::split(client);

        let make_call = |id: u64| {
            let req = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "_meta": modern_meta(false),
                    "name": "sparql_query",
                    "arguments": large_query_args(512 << 10)
                }
            });
            format!("{}\n", serde_json::to_string(&req).unwrap())
        };

        // Send req 1 so its ~180 KiB response blocks on the 4 KiB duplex pipe.
        cw.write_all(make_call(101).as_bytes()).await.unwrap();
        cw.flush().await.unwrap();
        // Wait for req 1 to finish executing, enter `StdioQueue`, trim its INITIAL_GRANT
        // down to the ~180 KiB blocked frame, and stall mid-write on the 4 KiB pipe.
        assert!(
            wait_for(Duration::from_secs(10), || {
                let g = pool.granted();
                g > 0 && g < (4 << 20)
            })
            .await,
            "expected req 101 to trim and block inside StdioQueue: granted={}",
            pool.granted()
        );
        let held_by_one = pool.granted();
        assert!(held_by_one > 0);

        // Send req 2: it executes and queues behind req 1 in `StdioQueue`.
        cw.write_all(make_call(102).as_bytes()).await.unwrap();
        cw.flush().await.unwrap();
        assert!(
            wait_for(Duration::from_secs(5), || pool.granted() > held_by_one).await,
            "expected req 2 queued behind backpressured req 1 to add credit"
        );

        // Cancel req 2 while the pipe stays OPEN and backpressured on req 1!
        let cancel_2 = json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": 102 }
        });
        cw.write_all(format!("{}\n", serde_json::to_string(&cancel_2).unwrap()).as_bytes())
            .await
            .unwrap();
        cw.flush().await.unwrap();
        // Req 2's credit MUST drop immediately while req 1 is still blocked on the open pipe.
        assert!(
            wait_for(Duration::from_secs(5), || pool.granted() <= held_by_one).await,
            "cancelled queued stdio frame kept credit under open backpressure: granted={}, held_by_one={held_by_one}",
            pool.granted()
        );

        // Send a duplicate request with id 101 WHILE original 101's response is already
        // queued/writing inside `StdioQueue` under open backpressure: must still be rejected
        // with -32600 rather than admitted as a second concurrent request! Follow it with
        // a distinct query (id 104) and wait for its credit to appear BEFORE reading stdout,
        // deterministically proving `receive()` already rejected `dup_101` while `101` stayed
        // blocked on the 4 KiB pipe.
        let dup_101 = json!({
            "jsonrpc": "2.0",
            "id": 101,
            "method": "tools/list",
            "params": { "_meta": modern_meta(false) }
        });
        let probe_batch = format!(
            "{}\n{}",
            serde_json::to_string(&dup_101).unwrap(),
            make_call(104)
        );
        cw.write_all(probe_batch.as_bytes()).await.unwrap();
        cw.flush().await.unwrap();
        assert!(
            wait_for(Duration::from_secs(5), || pool.granted() > held_by_one).await,
            "expected req 104 after dup_101 to queue behind backpressured req 101"
        );

        // Now drain stdout (pipe still open) and verify req 101, the duplicate-ID -32600
        // rejection, req 104, and a subsequent tools/list work cleanly with no stray response
        // for cancelled req 2 and pool.granted() == 0!
        let mut lines = BufReader::new(cr).lines();
        let line1 = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let reply1: Value = serde_json::from_str(&line1).unwrap();
        assert_eq!(reply1["id"], 101);
        assert_eq!(reply1["result"]["isError"], false);

        let line_dup = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let reply_dup: Value = serde_json::from_str(&line_dup).unwrap();
        assert_eq!(reply_dup["id"], 101);
        assert_eq!(
            reply_dup["error"]["code"], -32600,
            "duplicate ID while response is in StdioQueue must return -32600: {reply_dup}"
        );

        let line_104 = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let reply_104: Value = serde_json::from_str(&line_104).unwrap();
        assert_eq!(reply_104["id"], 104);
        assert_eq!(reply_104["result"]["isError"], false);

        let list = json!({
            "jsonrpc": "2.0",
            "id": 103,
            "method": "tools/list",
            "params": { "_meta": modern_meta(false) }
        });
        cw.write_all(format!("{}\n", serde_json::to_string(&list).unwrap()).as_bytes())
            .await
            .unwrap();
        cw.flush().await.unwrap();
        let line_list = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let reply_list: Value = serde_json::from_str(&line_list).unwrap();
        assert_eq!(
            reply_list["id"], 103,
            "cancelled req 102 must not emit a response before req 103: {reply_list}"
        );
        assert!(
            wait_for(Duration::from_secs(5), || pool.granted() == 0).await,
            "credit leaked on open stdio pipe: granted={}",
            pool.granted()
        );

        drop((cw, lines));
        let _ = serve_handle.await;
    }

    // Subcase 2 (F8): duplicate in-flight RequestId on stdio is rejected with -32600 while
    // the original request completes intact.
    {
        let (client, srv) = tokio::io::duplex(1 << 20);
        let (sr, sw) = tokio::io::split(srv);
        let serve_handle =
            tokio::spawn(super::adapter::serve(Adapter::new(server.clone()), sr, sw));
        let (cr, mut cw) = tokio::io::split(client);
        let mut lines = BufReader::new(cr).lines();

        let req_a = json!({
            "jsonrpc": "2.0",
            "id": 77,
            "method": "tools/call",
            "params": {
                "_meta": modern_meta(false),
                "name": "sparql_query",
                "arguments": large_query_args(512 << 10)
            }
        });
        let req_dup = json!({
            "jsonrpc": "2.0",
            "id": 77,
            "method": "tools/list",
            "params": { "_meta": modern_meta(false) }
        });
        let batch = format!(
            "{}\n{}\n",
            serde_json::to_string(&req_a).unwrap(),
            serde_json::to_string(&req_dup).unwrap()
        );
        cw.write_all(batch.as_bytes()).await.unwrap();
        cw.flush().await.unwrap();

        let mut replies = Vec::new();
        for _ in 0..2 {
            let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            replies.push(serde_json::from_str::<Value>(&line).unwrap());
        }
        assert!(
            replies
                .iter()
                .any(|r| r["id"] == 77 && r["error"]["code"] == -32600),
            "expected -32600 duplicate ID rejection in {replies:?}"
        );
        assert!(
            replies
                .iter()
                .any(|r| r["id"] == 77 && r["result"]["isError"] == false),
            "expected original request 77 to succeed intact in {replies:?}"
        );
        drop((cw, lines));
        let _ = serve_handle.await;
        assert!(wait_for(Duration::from_secs(5), || pool.granted() == 0).await);
    }

    // Subcase 3 (F3): interleaved Invalid Request protocol errors and responses over a
    // short-writing 7-byte sink produce intact NDJSON lines with zero tearing.
    {
        let (client, srv) = tokio::io::duplex(256 << 10);
        let (sr, sw) = tokio::io::split(srv);
        let short_sw = ShortWriter {
            inner: sw,
            chunk: 7,
            yield_next: false,
        };
        let serve_handle = tokio::spawn(super::adapter::serve(
            Adapter::new(server.clone()),
            sr,
            short_sw,
        ));
        let (cr, mut cw) = tokio::io::split(client);

        let mut batch = String::new();
        for i in 0..6u64 {
            let valid = json!({
                "jsonrpc": "2.0",
                "id": 200 + i,
                "method": "tools/list",
                "params": { "_meta": modern_meta(false) }
            });
            batch.push_str(&serde_json::to_string(&valid).unwrap());
            batch.push('\n');
            // Triggers serde_json Category::Data inside CreditedStdioTransport::receive.
            batch.push_str("{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":123}\n");
        }
        cw.write_all(batch.as_bytes()).await.unwrap();
        cw.flush().await.unwrap();

        let mut lines = BufReader::new(cr).lines();
        let mut valid_count = 0usize;
        let mut err_count = 0usize;
        for _ in 0..12 {
            let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .expect("intact NDJSON line");
            let v: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("torn NDJSON frame: {e}: {line:?}"));
            if v.get("error").is_some() {
                assert_eq!(v["error"]["code"], -32600);
                err_count += 1;
            } else {
                valid_count += 1;
            }
        }
        assert_eq!(valid_count, 6);
        assert_eq!(err_count, 6);
        drop((cw, lines));
        let _ = serve_handle.await;
    }

    // Subcase 4 (R2-8): stdin half-closes immediately after protocol errors over a
    // short-writing 7-byte sink while stdout stays open — every accepted -32600 error line
    // must drain intact before EOF.
    {
        let (client, srv) = tokio::io::duplex(256 << 10);
        let (sr, sw) = tokio::io::split(srv);
        let short_sw = ShortWriter {
            inner: sw,
            chunk: 7,
            yield_next: false,
        };
        let serve_handle = tokio::spawn(super::adapter::serve(Adapter::new(server), sr, short_sw));
        let (cr, mut cw) = tokio::io::split(client);

        let mut batch = String::new();
        for _ in 0..4 {
            batch.push_str("{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":123}\n");
        }
        cw.write_all(batch.as_bytes()).await.unwrap();
        cw.flush().await.unwrap();
        // Half-close stdin immediately while keeping stdout (`cr`) open to read.
        let eof_start = Instant::now();
        cw.shutdown().await.unwrap();
        drop(cw);

        let mut lines = BufReader::new(cr).lines();
        let mut err_count = 0usize;
        while let Some(line) = tokio::time::timeout(Duration::from_secs(4), lines.next_line())
            .await
            .unwrap()
            .unwrap()
        {
            let v: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("truncated NDJSON error frame on EOF: {e}: {line:?}"));
            assert_eq!(v["error"]["code"], -32600);
            err_count += 1;
        }
        assert_eq!(
            err_count, 4,
            "all accepted protocol errors must drain before EOF"
        );
        serve_handle.await.unwrap().unwrap();
        assert!(
            eof_start.elapsed() < Duration::from_secs(2),
            "clean stdin EOF drain must exit promptly without hitting 5s timeout: {:?}",
            eof_start.elapsed()
        );
    }
}

/// R3-1: cancelling a task-creation request whose `CreateTaskResult` is already queued in
/// `StdioQueue` behind a backpressured response must retire the detached task immediately
/// instead of stranding its entry and retained credit until TTL.
#[tokio::test(flavor = "multi_thread")]
async fn stdio_cancelling_queued_task_creation_retires_detached_task() {
    let ttl = big_turtle(180, 900);
    let cfg = McpConfig {
        max_bytes: 1 << 20,
        task_after: Duration::ZERO,
        ..Default::default()
    };
    let (_dir, state, pool) = pooled_state(64 << 20, &ttl, cfg.clone(), 0);
    let server = McpServer::new_with_task_ttl(state, cfg, Duration::from_secs(60));

    let (client, srv) = tokio::io::duplex(4096);
    let (sr, sw) = tokio::io::split(srv);
    let (writer_probe, serve_handle) =
        super::adapter::spawn_serve_with_probe(Adapter::new(server.clone()), sr, sw);
    let (cr, mut cw) = tokio::io::split(client);

    // 1. Block the 4 KiB stdout pipe with a synchronous (~180 KiB) tool response.
    let req_block = json!({
        "jsonrpc": "2.0",
        "id": 501,
        "method": "tools/call",
        "params": {
            "_meta": modern_meta(false),
            "name": "sparql_query",
            "arguments": large_query_args(512 << 10)
        }
    });
    cw.write_all(format!("{}\n", serde_json::to_string(&req_block).unwrap()).as_bytes())
        .await
        .unwrap();
    cw.flush().await.unwrap();
    assert!(
        wait_for(Duration::from_secs(10), || {
            let g = pool.granted();
            g > 0 && g < (4 << 20)
        })
        .await,
        "expected req 501 to trim and block inside StdioQueue: granted={}",
        pool.granted()
    );
    let held_by_one = pool.granted();

    // 2. Send a task-capable call (`task_after = 0`). It detaches into `server.shared.tasks`
    //    and queues its `CreateTaskResult` reply behind blocked req 501 in `StdioQueue`.
    let req_task = json!({
        "jsonrpc": "2.0",
        "id": 502,
        "method": "tools/call",
        "params": {
            "_meta": modern_meta(true),
            "name": "sparql_query",
            "arguments": large_query_args(512 << 10)
        }
    });
    cw.write_all(format!("{}\n", serde_json::to_string(&req_task).unwrap()).as_bytes())
        .await
        .unwrap();
    cw.flush().await.unwrap();
    // Wait until `CreateTaskResult` for request 502 is observably queued inside
    // `StdioQueue` behind blocked req 501 AND the detached task has finished and trimmed
    // its retained result, ensuring `notifications/cancelled` exercises queued writer
    // cancellation rather than `pending` slot cancellation.
    let req_502_id = rmcp::model::RequestId::Number(502);
    assert!(
        wait_for(Duration::from_secs(10), || {
            let g = pool.granted();
            writer_probe.is_queued(&req_502_id)
                && server.shared.tasks.len() == 1
                && g > held_by_one
                && g < held_by_one + (4 << 20)
        })
        .await,
        "detached task reply 502 did not enqueue behind blocked req 501: queued={}, tasks={}, granted={}, held_by_one={held_by_one}",
        writer_probe.is_queued(&req_502_id),
        server.shared.tasks.len(),
        pool.granted()
    );

    // 3. Cancel req 502 while stdout remains blocked on req 501.
    let cancel_502 = json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": { "requestId": 502 }
    });
    cw.write_all(format!("{}\n", serde_json::to_string(&cancel_502).unwrap()).as_bytes())
        .await
        .unwrap();
    cw.flush().await.unwrap();
    assert!(
        wait_for(Duration::from_secs(5), || {
            server.shared.tasks.len() == 0 && pool.granted() <= held_by_one
        })
        .await,
        "cancelling queued task-creation response left detached task stranded: tasks={}, granted={}, held_by_one={held_by_one}",
        server.shared.tasks.len(),
        pool.granted()
    );

    // 4. Drain stdout: req 501 and a subsequent tools/list arrive with no CreateTaskResult
    //    for cancelled req 502, and pool credit drops to 0.
    let list = json!({
        "jsonrpc": "2.0",
        "id": 503,
        "method": "tools/list",
        "params": { "_meta": modern_meta(false) }
    });
    cw.write_all(format!("{}\n", serde_json::to_string(&list).unwrap()).as_bytes())
        .await
        .unwrap();
    cw.flush().await.unwrap();

    let mut lines = BufReader::new(cr).lines();
    let line1 = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let reply1: Value = serde_json::from_str(&line1).unwrap();
    assert_eq!(reply1["id"], 501);
    assert_eq!(reply1["result"]["isError"], false);

    let line2 = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let reply2: Value = serde_json::from_str(&line2).unwrap();
    assert_eq!(
        reply2["id"], 503,
        "cancelled queued CreateTaskResult 502 must not appear on stdout: {reply2}"
    );
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() == 0).await,
        "credit leaked after draining stdout: granted={}",
        pool.granted()
    );

    drop((cw, lines));
    serve_handle.await.unwrap().unwrap();
}

/// F2, F6, F7, F8, F16:
/// - Mid-write disconnect after Hyper polls the terminal SSE frame to EOF stays replayable;
/// - Explicit `Last-Event-ID` ack past the terminal frame evicts replay credit immediately;
/// - Post-cache `notifications/cancelled` evicts replay credit AND discards unread body queues;
/// - `DELETE /$/mcp` releases credit even while a stalled SSE body sits unread;
/// - Duplicate in-flight `RequestId` in a legacy session returns `-32600` without breaking
///   the original request;
/// - `HttpConf.shutdown.cancel()` releases disconnected replay caches AND retained completed
///   tasks immediately while the router stays alive.
#[tokio::test(flavor = "multi_thread")]
async fn http_legacy_sse_midwrite_replay_post_cache_cancel_dup_id_and_shutdown() {
    let ttl = big_turtle(180, 900);
    let cfg = McpConfig {
        max_bytes: 1 << 20,
        http: true,
        task_after: Duration::ZERO,
        max_timeout: Duration::from_secs(60),
        ..Default::default()
    };
    let (_dir, state, pool) =
        pooled_state_with_admission_wait(64 << 20, &ttl, cfg, 16, Duration::from_secs(30));
    let mut session_cfg = SessionConfig::default();
    session_cfg.completed_cache_ttl = Duration::from_secs(60);
    let app = mcp_http_router(state.clone(), session_cfg, Some(Duration::from_secs(60)));

    // Open a legacy session.
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "r1-fix-test", "version": "1"}
        }
    });
    let init_resp = app
        .clone()
        .oneshot(http_req("POST", &[ACCEPT, JSON_CT], &init.to_string()))
        .await
        .unwrap();
    assert_eq!(init_resp.status(), StatusCode::OK);
    let sid = init_resp
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let _ = axum::body::to_bytes(init_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let init_ack = app
        .clone()
        .oneshot(http_req(
            "POST",
            &[
                ACCEPT,
                JSON_CT,
                ("mcp-protocol-version", "2025-11-25"),
                ("mcp-session-id", sid.as_str()),
            ],
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(init_ack.status(), StatusCode::ACCEPTED);

    let post_query = |id: u64| {
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "sparql_query",
                "arguments": large_query_args(512 << 10)
            }
        });
        http_req(
            "POST",
            &[
                ACCEPT,
                JSON_CT,
                ("mcp-protocol-version", "2025-11-25"),
                ("mcp-session-id", sid.as_str()),
            ],
            &body.to_string(),
        )
    };

    // 1. (F2) Poll the body ALL the way to EOF and drop it (simulating Hyper buffering the
    //    terminal frame before TCP drops mid-write). Reconnecting with priming `Last-Event-ID`
    //    MUST still replay the full terminal response!
    let resp = app.clone().oneshot(post_query(301)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let full = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let full_text = String::from_utf8_lossy(&full);
    let event_ids: Vec<&str> = full_text
        .lines()
        .filter_map(|l| l.strip_prefix("id: "))
        .collect();
    assert!(
        event_ids.len() >= 2,
        "expected priming + terminal event ids in {full_text:?}"
    );
    let priming_id = event_ids[0].to_string();
    let terminal_id = event_ids[1].to_string();
    drop(full);
    // Replay cache still retains the terminal frame (trimmed well below 16 MiB).
    assert!(pool.granted() > 0 && pool.granted() < (4 << 20));

    let replay = app
        .clone()
        .oneshot(http_req(
            "GET",
            &[
                ("accept", "text/event-stream"),
                ("mcp-protocol-version", "2025-11-25"),
                ("mcp-session-id", sid.as_str()),
                ("last-event-id", priming_id.as_str()),
            ],
            "",
        ))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let replay_bytes = axum::body::to_bytes(replay.into_body(), usize::MAX)
        .await
        .unwrap();
    let replay_text = String::from_utf8_lossy(&replay_bytes);
    let payload_line = replay_text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .find(|d| d.starts_with('{'))
        .expect("replayed terminal response after body EOF");
    let reply: Value = serde_json::from_str(payload_line).unwrap();
    assert_eq!(reply["id"], 301);
    assert_eq!(reply["result"]["isError"], false);
    drop(replay_bytes);

    // Explicit client ack at `terminal_id` evicts the replay entry immediately!
    let ack = app
        .clone()
        .oneshot(http_req(
            "GET",
            &[
                ("accept", "text/event-stream"),
                ("mcp-protocol-version", "2025-11-25"),
                ("mcp-session-id", sid.as_str()),
                ("last-event-id", terminal_id.as_str()),
            ],
            "",
        ))
        .await
        .unwrap();
    assert_eq!(ack.status(), StatusCode::OK);
    drop(ack);
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() == 0).await,
        "client Last-Event-ID ack at terminal frame did not evict replay credit: granted={}",
        pool.granted()
    );

    // 2. (F6) Hold an SSE response body completely UNREAD so its terminal frame sits in BOTH
    //    the body queue and the replay cache; post-cache `notifications/cancelled` must drop
    //    BOTH and return credit to 0 immediately while `unread_resp` is still alive!
    let unread_resp = app.clone().oneshot(post_query(302)).await.unwrap();
    assert_eq!(unread_resp.status(), StatusCode::OK);
    assert!(wait_for(Duration::from_secs(5), || pool.granted() > 0).await);
    // Give the handler time to finish caching the terminal frame.
    tokio::time::sleep(Duration::from_millis(80)).await;
    let cancel_302 = json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": { "requestId": 302 }
    });
    let cancel_resp = app
        .clone()
        .oneshot(http_req(
            "POST",
            &[
                ACCEPT,
                JSON_CT,
                ("mcp-protocol-version", "2025-11-25"),
                ("mcp-session-id", sid.as_str()),
            ],
            &cancel_302.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(cancel_resp.status(), StatusCode::ACCEPTED);
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() == 0).await,
        "post-cache notifications/cancelled failed to release unread body + replay credit: granted={}",
        pool.granted()
    );
    drop(unread_resp);

    // 2b. (F8 / R2-11 / R3-6) Duplicate in-flight RequestId on a legacy HTTP session returns
    //     -32600 on the duplicate SSE stream without breaking the original request.
    //     Use `prompts/get` (which bypasses `http.rs` pre-admission and acquires pool credit
    //     inside `Adapter::get_prompt` -> `admit_call`) while holding the entire QueryPool
    //     capacity so id 305 is deterministically registered in-flight and blocked until the
    //     duplicate's `-32600` response has been observed.
    {
        let gated_body = json!({
            "jsonrpc": "2.0",
            "id": 305,
            "method": "prompts/get",
            "params": {
                "name": "explore_dataset",
                "arguments": { "dataset": "t" }
            }
        });
        let dup_body = json!({
            "jsonrpc": "2.0",
            "id": 305,
            "method": "tools/list",
            "params": {}
        });
        let headers = [
            ACCEPT,
            JSON_CT,
            ("mcp-protocol-version", "2025-11-25"),
            ("mcp-session-id", sid.as_str()),
        ];
        let gate_grant = pool
            .try_grant(64 << 20)
            .expect("pool idle before gated duplicate subcase");
        // Bound the duplicate exchange to 5 s, well inside the 30 s admission wait and 60 s
        // call deadline, so the original request cannot time out admission before the
        // duplicate POST is processed.
        let (orig_resp, dup_bytes) = tokio::time::timeout(Duration::from_secs(5), async {
            // First POST returns the SSE response as soon as the session worker registers id 305,
            // while `gate_grant` blocks `Adapter::get_prompt` inside `admit_call`.
            let orig_resp = app
                .clone()
                .oneshot(http_req("POST", &headers, &gated_body.to_string()))
                .await
                .unwrap();
            assert_eq!(orig_resp.status(), StatusCode::OK);
            let dup_resp = app
                .clone()
                .oneshot(http_req("POST", &headers, &dup_body.to_string()))
                .await
                .unwrap();
            assert_eq!(dup_resp.status(), StatusCode::OK);
            let dup_bytes = axum::body::to_bytes(dup_resp.into_body(), usize::MAX)
                .await
                .unwrap();
            (orig_resp, dup_bytes)
        })
        .await
        .expect("duplicate HTTP exchange must finish well before the 30s admission deadline");
        let dup_text = String::from_utf8_lossy(&dup_bytes);
        let dup_payload = dup_text
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .find(|d| d.starts_with('{'))
            .expect("duplicate request must emit JSON-RPC error event");
        let dup_v: Value = serde_json::from_str(dup_payload).unwrap();
        assert_eq!(dup_v["id"], 305);
        assert_eq!(
            dup_v["error"]["code"], -32600,
            "expected -32600 duplicate ID rejection on legacy HTTP: {dup_v}"
        );

        // Release the pool gate so original id 305 unblocks and emits its terminal response.
        drop(gate_grant);
        let orig_bytes = axum::body::to_bytes(orig_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let orig_text = String::from_utf8_lossy(&orig_bytes);
        let orig_payload = orig_text
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .find(|d| d.starts_with('{'))
            .expect("original request must still emit its terminal response");
        let orig_v: Value = serde_json::from_str(orig_payload).unwrap();
        assert_eq!(orig_v["id"], 305);
        assert!(
            orig_v["result"]["messages"]
                .as_array()
                .is_some_and(|m| !m.is_empty()),
            "expected original prompts/get response for id 305: {orig_v}"
        );
        // Evict 305's replay cache via cancellation so subcase 3 starts from 0 credit.
        let cancel_305 = json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": 305 }
        });
        let _ = app
            .clone()
            .oneshot(http_req("POST", &headers, &cancel_305.to_string()))
            .await
            .unwrap();
        assert!(wait_for(Duration::from_secs(5), || pool.granted() == 0).await);
    }

    // 3. (F7 / R2-10) Create BOTH an unread/disconnected replay entry AND a retained
    //    completed task (with 60 s TTLs), verify both hold credit, then cancel
    //    `HttpConf.shutdown` WITHOUT dropping `app` -> credit MUST drop to 0 immediately!
    let held_sse = app.clone().oneshot(post_query(303)).await.unwrap();
    assert_eq!(held_sse.status(), StatusCode::OK);
    assert!(
        wait_for(Duration::from_secs(5), || {
            let g = pool.granted();
            g > 0 && g < (4 << 20)
        })
        .await,
        "legacy SSE response did not finish caching and trimming: granted={}",
        pool.granted()
    );
    let granted_with_sse = pool.granted();
    assert!(granted_with_sse > 0);
    let task_start = app
        .clone()
        .oneshot(http_req(
            "POST",
            &[
                ACCEPT,
                JSON_CT,
                ("mcp-protocol-version", "2026-07-28"),
                ("mcp-method", "tools/call"),
                ("mcp-name", "sparql_query"),
            ],
            &json!({
                "jsonrpc": "2.0",
                "id": 304,
                "method": "tools/call",
                "params": {
                    "_meta": modern_meta(true),
                    "name": "sparql_query",
                    "arguments": large_query_args(512 << 10)
                }
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(task_start.status(), StatusCode::OK);
    let start_raw = axum::body::to_bytes(task_start.into_body(), usize::MAX)
        .await
        .unwrap();
    let start_v: Value = serde_json::from_slice(&start_raw).unwrap();
    let task_id = start_v["result"]["taskId"].as_str().unwrap().to_string();
    // Wait until the task finishes and is retained.
    let mut task_completed = false;
    for poll in 0..100 {
        let poll_resp = app
            .clone()
            .oneshot(http_req(
                "POST",
                &[
                    ACCEPT,
                    JSON_CT,
                    ("mcp-protocol-version", "2026-07-28"),
                    ("mcp-method", "tasks/get"),
                    ("mcp-name", task_id.as_str()),
                ],
                &json!({
                    "jsonrpc": "2.0",
                    "id": 400 + poll,
                    "method": "tasks/get",
                    "params": { "_meta": modern_meta(true), "taskId": task_id }
                })
                .to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(poll_resp.status(), StatusCode::OK);
        let raw = axum::body::to_bytes(poll_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&raw).unwrap();
        if v["result"]["status"] == "completed" {
            assert_eq!(v["result"]["resultType"], "complete", "{v}");
            task_completed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        task_completed,
        "background task {task_id} did not reach completed state before shutdown check"
    );
    assert!(
        pool.granted() > granted_with_sse,
        "expected both legacy SSE replay and retained completed task to hold credit: granted={}, sse_only={granted_with_sse}",
        pool.granted()
    );

    // Fire HTTP shutdown while `app` and `held_sse` are still alive!
    state.mcp.as_ref().unwrap().shutdown.cancel();
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() == 0).await,
        "HttpConf.shutdown did not immediately release session replay + task credit: granted={}",
        pool.granted()
    );
    drop(held_sse);
    drop(app);
}

/// F10 & F12: 20 small completed background tasks on a 32 MiB pool trim their grants so they
/// hold < 1 MiB total (instead of 20 * 16 MiB = 320 MiB) and never starve new queries.
#[tokio::test(flavor = "multi_thread")]
async fn many_small_completed_tasks_trim_grants_and_never_starve_pool() {
    use super::errors::ErrorContext;
    use super::retained::RetainedCallResult;
    use super::tasks::{TaskOutcome, Tasks};
    use sparkles::access::Preparation;
    use sparkles::sparql::pool::INITIAL_GRANT;
    use sparkles::task::Control;
    use std::sync::atomic::AtomicBool;

    let pool = QueryPool::new(32 << 20);
    let tasks = Tasks::with_ttl(Duration::from_secs(60));
    let mut task_ids = Vec::new();
    for i in 0..20usize {
        // Every call starts with a full 16 MiB `INITIAL_GRANT` (20 * 16 MiB = 320 MiB >> 32 MiB).
        let grant =
            Arc::new(pool.try_grant(INITIAL_GRANT).unwrap_or_else(|| {
                panic!("pool starved before task {i}: granted={}", pool.granted())
            }));
        assert_eq!(grant.granted(), INITIAL_GRANT);
        let prep = Preparation::with_grant(Control::none(), 4 << 20, grant.clone()).unwrap();
        let err_ctx = ErrorContext {
            timeout_secs: 30.0,
            max_timeout_secs: 60.0,
            prefix_names: &[],
            request_id: "trim",
        };
        let retained = RetainedCallResult::from_outcome(
            Ok(Outcome::Text(format!("small-task-result-{i}"))),
            &prep,
            true,
            &err_ctx,
        )
        .unwrap();
        drop(prep);
        drop(grant);
        let work = tokio::spawn(async move { TaskOutcome::Retained(retained) });
        let task = tasks
            .try_start_retained("alice".into(), work, Arc::new(AtomicBool::new(false)), None)
            .ok()
            .expect("task admitted");
        // Wait for this task to complete and trim its grant before admitting the next one.
        assert!(
            wait_for(Duration::from_secs(5), || {
                tasks
                    .get("alice", &task.task_id)
                    .is_ok_and(|d| d.task.status == rmcp::model::TaskStatus::Completed)
            })
            .await,
            "task {i} did not complete"
        );
        task_ids.push(task.task_id);
    }

    // All 20 retained tasks together MUST hold far less than 1 MiB (not 20 * 16 MiB = 320 MiB!).
    let granted = pool.granted();
    assert!(
        granted > 0 && granted < (1 << 20),
        "20 small completed tasks held too much pool credit: {granted} bytes"
    );
    // A fresh 16 MiB query grant still admits immediately on the 32 MiB pool!
    let fresh_grant = pool
        .try_grant(INITIAL_GRANT)
        .expect("32 MiB pool must still admit a 16 MiB query beside 20 small completed tasks");
    drop(fresh_grant);

    // Shutdown releases every retained task and returns pool credit to 0.
    tasks.shutdown();
    assert!(
        wait_for(Duration::from_secs(5), || pool.granted() == 0).await,
        "tasks.shutdown() leaked credit: granted={}",
        pool.granted()
    );
    assert_eq!(task_ids.len(), 20);
}
