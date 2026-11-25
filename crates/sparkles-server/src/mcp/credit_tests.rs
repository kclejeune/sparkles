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
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        mcp_cfg.max_timeout,
        Some(pool_bytes),
    )
    .unwrap();
    st.read_only = true;
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
    let tool_res = &completed["result"]["result"];
    assert_eq!(tool_res["isError"], false, "{completed}");
    let parsed: Value =
        serde_json::from_str(tool_res["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(parsed["rows"].as_array().unwrap().len(), 180);

    // While the completed task stays retained in `Tasks`, QueryPool credit MUST remain held.
    assert!(
        pool.granted() > 0,
        "completed task should retain QueryPool credit until TTL expiry"
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
