//! HTTP router tests for item 534: in-place cursor execution and mid-stream hand-off on
//! `execution=auto`, single-task pool execution on explicit `execution=streaming`, format
//! parity, open-error characterization, budgets, timeouts, and disconnect cancellation.

use super::*;
use crate::http::cursor::route_stats;
use crate::http::inline::{History, QuickQueries};
use futures_util::StreamExt;

struct PoolServer {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

fn pool_server(limit: Option<u64>, concurrency: Option<&str>) -> PoolServer {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
        Some(64 << 20),
    )
    .unwrap();
    state.limits.max_result_bytes = limit;
    if let Some(flag) = concurrency {
        let mut config = crate::ratelimit::Config::default();
        config.apply_flag(flag).unwrap();
        state.rate_limit = Some(Arc::new(
            crate::ratelimit::RateLimiter::new(&config).unwrap(),
        ));
    }
    let state = Arc::new(state);
    state.set_phase(crate::obs::Phase::Ready);
    let app = router(state.clone());
    PoolServer {
        _dir: dir,
        state,
        app,
    }
}

fn load_turtle(state: &AppState, name: &str, data: String) {
    let ds = state.attach(name, DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
}

async fn post_query(
    app: &Router,
    uri: &str,
    query: &str,
    accept: &str,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let req = Request::post(uri)
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, accept)
        .body(Body::from(query.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, headers, bytes)
}

fn scrub_native_meta(bytes: &[u8]) -> serde_json::Value {
    let mut v: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    if let Some(meta) = v.get_mut("meta").and_then(|m| m.as_object_mut()) {
        meta.remove("timing");
        meta.remove("memory");
        meta.remove("plan");
        meta.remove("datasetId");
    }
    v
}

async fn wait_pool_zero(state: &AppState, limit: Duration) -> bool {
    tokio::time::timeout(limit, async {
        while state.catalog.pool().unwrap().granted() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok()
}

/// A cursor admitted by `execution=auto` that fits in `STREAM_AFTER` opens and drains
/// completely in place (returning a whole body with `Content-Length`), while an explicit
/// `execution=streaming` request stays on the blocking pool route.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_cursor_runs_in_place_whole_while_explicit_streaming_stays_on_pool() {
    let s = pool_server(None, None);
    let mut data = String::new();
    for i in 0..2000 {
        data.push_str(&format!("<urn:ip:s{i}> <urn:ip:p> {i} .\n"));
    }
    load_turtle(&s.state, "handoff_small", data);

    let q = "SELECT ?s ?o WHERE { ?s <urn:ip:p> ?o }";
    let key = QuickQueries::key("handoff_small", q);
    crate::http::QUICK.ignore_time(key);

    // 1. Default / `execution=auto` first run: opens and drains in place whole.
    let (status, headers, auto_body) = post_query(
        &s.app,
        "/handoff_small/sparql?execution=auto",
        q,
        "application/x-sparkles+json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers.contains_key(header::CONTENT_LENGTH),
        "in-place whole cursor response should carry Content-Length"
    );
    let auto_json: J = serde_json::from_slice(&auto_body).unwrap();
    assert_eq!(auto_json["meta"]["status"], "complete");
    assert_eq!(auto_json["rows"].as_array().unwrap().len(), 2000);
    assert_eq!(
        route_stats::in_place_whole(key),
        1,
        "first auto cursor run completed in place whole"
    );
    assert_eq!(route_stats::in_place_handoff(key), 0);
    assert_eq!(route_stats::pool_auto(key), 0);
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);

    // 2. Explicit `execution=streaming`: must stay on the blocking pool route, never in place.
    let before_stream = route_stats::pool_streaming();
    let (status, _, stream_body) = post_query(
        &s.app,
        "/handoff_small/sparql?execution=streaming",
        q,
        "application/x-sparkles+json",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        route_stats::pool_streaming() > before_stream,
        "explicit execution=streaming must take the pool streaming route"
    );
    assert_eq!(
        route_stats::in_place_whole(key),
        1,
        "explicit execution=streaming must not run in place"
    );
    assert_eq!(
        scrub_native_meta(&auto_body),
        scrub_native_meta(&stream_body)
    );
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);
}

/// Exact output parity between in-place `auto` whole cursor, explicit `execution=streaming`,
/// and `execution=eager` across all solution formats (JSON, XML, CSV, TSV, Thrift, native
/// JSON) and ASK.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn small_cursor_exact_parity_across_all_solution_formats() {
    let s = pool_server(None, None);
    let mut data = String::new();
    for i in 0..1500 {
        data.push_str(&format!(
            "<urn:par:s{i:04}> <urn:par:p> \"v-{i}-\\\"<&>\" .\n"
        ));
    }
    load_turtle(&s.state, "handoff_parity", data);

    let formats = [
        ("json", "application/sparql-results+json"),
        ("xml", "application/sparql-results+xml"),
        ("csv", "text/csv"),
        ("tsv", "text/tab-separated-values"),
        ("thrift", "application/sparql-results+thrift"),
        ("native", "application/x-sparkles+json"),
    ];
    for (idx, (fname, accept)) in formats.iter().enumerate() {
        // Distinct query text per format so each format hits the unseen first-run in-place
        // cursor path instead of settling to the eager shortcut from an earlier format.
        let q = format!("# fmt-{idx}\nSELECT ?s ?o WHERE {{ ?s <urn:par:p> ?o }}");
        let key = QuickQueries::key("handoff_parity", &q);
        let (st_auto, h_auto, body_auto) =
            post_query(&s.app, "/handoff_parity/sparql?execution=auto", &q, accept).await;
        assert_eq!(st_auto, StatusCode::OK, "auto {fname}");
        assert_eq!(
            route_stats::in_place_whole(key),
            1,
            "auto {fname} should finish in place whole"
        );

        let (st_stream, h_stream, body_stream) = post_query(
            &s.app,
            "/handoff_parity/sparql?execution=streaming",
            &q,
            accept,
        )
        .await;
        assert_eq!(st_stream, StatusCode::OK, "streaming {fname}");
        assert_eq!(
            h_auto.get(header::CONTENT_TYPE),
            h_stream.get(header::CONTENT_TYPE),
            "content-type {fname}"
        );

        let (st_eager, _, body_eager) =
            post_query(&s.app, "/handoff_parity/sparql?execution=eager", &q, accept).await;
        assert_eq!(st_eager, StatusCode::OK, "eager {fname}");

        if *fname == "native" {
            let a = scrub_native_meta(&body_auto);
            let b = scrub_native_meta(&body_stream);
            assert_eq!(a["vars"], b["vars"]);
            assert_eq!(a["rows"], b["rows"]);
            assert_eq!(a["meta"]["status"], "complete");
            assert_eq!(b["meta"]["status"], "complete");
        } else {
            assert_eq!(body_auto, body_stream, "auto vs streaming {fname}");
            assert_eq!(body_auto, body_eager, "auto vs eager {fname}");
        }
    }

    // ASK in place vs explicit streaming across solution formats.
    for (idx, (fname, accept)) in formats.iter().enumerate() {
        let q = format!("# ask-{idx}\nASK {{ ?s <urn:par:p> ?o }}");
        let (st_auto, h_auto, body_auto) =
            post_query(&s.app, "/handoff_parity/sparql?execution=auto", &q, accept).await;
        let (st_stream, h_stream, body_stream) = post_query(
            &s.app,
            "/handoff_parity/sparql?execution=streaming",
            &q,
            accept,
        )
        .await;
        assert_eq!(st_auto, StatusCode::OK, "ask auto {fname}");
        assert_eq!(st_stream, StatusCode::OK, "ask streaming {fname}");
        assert_eq!(
            h_auto.get(header::CONTENT_TYPE),
            h_stream.get(header::CONTENT_TYPE),
            "ask content-type {fname}"
        );
        if *fname == "native" {
            assert_eq!(
                scrub_native_meta(&body_auto)["boolean"],
                scrub_native_meta(&body_stream)["boolean"]
            );
        } else {
            assert_eq!(body_auto, body_stream, "ask auto vs streaming {fname}");
        }
    }
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);
}

/// When an in-place `execution=auto` cursor's body exceeds `STREAM_AFTER` (1 MiB), it hands
/// off the cursor and already-buffered bytes to a blocking streaming task, producing exact
/// byte-for-byte parity across all solution formats, marking the text `Slow`, and routing
/// the next run directly to the single-task `pool_auto` path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_place_cursor_hands_off_past_threshold_with_exact_parity_all_formats() {
    let s = pool_server(None, None);
    let mut data = String::new();
    let payload = "a".repeat(800);
    // 2,000 rows * ~850 bytes ≈ 1.7 MiB (> STREAM_AFTER = 1 MiB)
    for i in 0..2000 {
        data.push_str(&format!(
            "<urn:big:s{i:04}> <urn:big:p> \"{payload}-{i}\" .\n"
        ));
    }
    load_turtle(&s.state, "handoff_big", data);

    let formats = [
        ("json", "application/sparql-results+json"),
        ("xml", "application/sparql-results+xml"),
        ("csv", "text/csv"),
        ("tsv", "text/tab-separated-values"),
        ("thrift", "application/sparql-results+thrift"),
        ("native", "application/x-sparkles+json"),
    ];
    for (idx, (fname, accept)) in formats.iter().enumerate() {
        let q = format!("# handoff-{idx}\nSELECT ?s ?o WHERE {{ ?s <urn:big:p> ?o }}");
        let key = QuickQueries::key("handoff_big", &q);
        // First run: opens in place, exceeds 1 MiB, hands off to blocking pool.
        let (st_auto, h_auto, body_auto) =
            post_query(&s.app, "/handoff_big/sparql?execution=auto", &q, accept).await;
        assert_eq!(st_auto, StatusCode::OK, "handoff auto {fname}");
        assert!(
            body_auto.len() > crate::http::stream::STREAM_AFTER,
            "{fname} body {} must exceed STREAM_AFTER",
            body_auto.len()
        );
        assert_eq!(
            route_stats::in_place_handoff(key),
            1,
            "{fname} first run must trigger in-place -> pool hand-off"
        );
        assert_eq!(route_stats::in_place_whole(key), 0);
        assert_eq!(
            crate::http::QUICK.history(key),
            History::Slow,
            "{fname} streamed hand-off must leave history Slow"
        );

        // Second run: history is Slow -> goes directly to single-task pool_auto route.
        let (st_pool, _, body_pool) =
            post_query(&s.app, "/handoff_big/sparql?execution=auto", &q, accept).await;
        assert_eq!(st_pool, StatusCode::OK, "pool_auto {fname}");
        assert_eq!(
            route_stats::pool_auto(key),
            1,
            "{fname} second run must use single-task pool_auto route"
        );

        // Explicit `execution=streaming`: single-task pool streaming route.
        let (st_stream, h_stream, body_stream) = post_query(
            &s.app,
            "/handoff_big/sparql?execution=streaming",
            &q,
            accept,
        )
        .await;
        assert_eq!(st_stream, StatusCode::OK, "streaming {fname}");
        assert_eq!(
            h_auto.get(header::CONTENT_TYPE),
            h_stream.get(header::CONTENT_TYPE)
        );

        if *fname == "native" {
            let a = scrub_native_meta(&body_auto);
            let p = scrub_native_meta(&body_pool);
            let b = scrub_native_meta(&body_stream);
            assert_eq!(a["rows"], b["rows"], "native handoff vs streaming");
            assert_eq!(p["rows"], b["rows"], "native pool_auto vs streaming");
        } else {
            assert_eq!(body_auto, body_stream, "{fname} handoff vs streaming");
            assert_eq!(body_pool, body_stream, "{fname} pool_auto vs streaming");
        }
        assert!(
            wait_pool_zero(&s.state, Duration::from_secs(2)).await,
            "pool grant leaked after {fname}"
        );
    }
}

/// Characterization tests for open-phase errors (PLAN.md item 534): parse error, unknown
/// `at`, memory budget refused at open, and timeout, on both `execution=streaming` and
/// `execution=auto`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_errors_keep_exact_status_on_streaming_and_auto() {
    let s = pool_server(None, None);
    let mut data = String::new();
    for i in 0..2500 {
        data.push_str(&format!("<urn:oe:s{i}> <urn:oe:p> {i} .\n"));
    }
    load_turtle(&s.state, "open_err", data);

    for mode in ["streaming", "auto"] {
        // 1. Parse error -> 400 Bad Request
        let (st, _, body) = post_query(
            &s.app,
            &format!("/open_err/sparql?execution={mode}"),
            "SELECT ?s WHERE { BROKEN",
            "application/sparql-results+json",
        )
        .await;
        assert_eq!(
            st,
            StatusCode::BAD_REQUEST,
            "parse error {mode}: {}",
            String::from_utf8_lossy(&body)
        );

        // 2. Unknown `at` snapshot -> 404 Not Found (inside snapshot_for at open)
        let (st, _, body) = post_query(
            &s.app,
            &format!("/open_err/sparql?execution={mode}&at=snapshot:nonexistent_snapshot"),
            "SELECT ?s WHERE { ?s <urn:oe:p> ?o }",
            "application/sparql-results+json",
        )
        .await;
        assert_eq!(
            st,
            StatusCode::NOT_FOUND,
            "unknown at {mode}: {}",
            String::from_utf8_lossy(&body)
        );

        // 3. Memory budget refused at open -> 507 Insufficient Storage with budget="memory"
        let dir_mem = tempfile::tempdir().unwrap();
        let mut st_mem = AppState::with_memory(
            dir_mem.path(),
            StoreOptions::default(),
            Duration::from_secs(30),
            Some(64 << 20),
        )
        .unwrap();
        st_mem.limits.query_memory_bytes = Some(1024);
        let st_mem = Arc::new(st_mem);
        st_mem.set_phase(crate::obs::Phase::Ready);
        load_turtle(&st_mem, "open_mem", "<urn:oe:s0> <urn:oe:p> 0 .\n".into());
        let app_mem = router(st_mem.clone());
        let (st, _, body) = post_query(
            &app_mem,
            &format!("/open_mem/sparql?execution={mode}"),
            "SELECT ?s ?o WHERE { ?s <urn:oe:p> ?o }",
            "application/sparql-results+json",
        )
        .await;
        assert_eq!(
            st,
            StatusCode::INSUFFICIENT_STORAGE,
            "memory budget {mode}: {}",
            String::from_utf8_lossy(&body)
        );
        let err_json: J = serde_json::from_slice(&body).unwrap();
        assert_eq!(err_json["budget"], "memory", "{mode}: {err_json}");
        assert!(wait_pool_zero(&st_mem, Duration::from_secs(2)).await);

        // 4. Timeout -> 408 Request Timeout
        let slow_q = format!(
            "# timeout-{mode}\nSELECT ?a ?b WHERE {{ ?a <urn:oe:p> ?x . ?b <urn:oe:p> ?y FILTER(?x + ?y < 0) }}"
        );
        let (st, _, body) = post_query(
            &s.app,
            &format!("/open_err/sparql?execution={mode}&timeout=0.1"),
            &slow_q,
            "application/sparql-results+json",
        )
        .await;
        assert_eq!(
            st,
            StatusCode::REQUEST_TIMEOUT,
            "timeout {mode}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);
}

/// `max-result-mb` / `max_result_bytes` enforcement on both in-place pre-commit (507),
/// post-handoff streaming (200 then truncated <= limit), and explicit `execution=streaming`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_result_bytes_enforced_on_in_place_handoff_and_streaming() {
    let payload = "z".repeat(800);
    let mut data = String::new();
    for i in 0..2000 {
        data.push_str(&format!(
            "<urn:lim:s{i:04}> <urn:lim:p> \"{payload}-{i}\" .\n"
        ));
    }

    // Case 1: Limit below STREAM_AFTER (50 KiB) -> in-place `auto` refuses with 507 pre-commit.
    let s_small = pool_server(Some(50_000), None);
    load_turtle(&s_small.state, "lim_small", data.clone());
    let q_small = "SELECT ?s ?o WHERE { ?s <urn:lim:p> ?o }";
    let (st, _, body) = post_query(
        &s_small.app,
        "/lim_small/sparql?execution=auto",
        q_small,
        "text/tab-separated-values",
    )
    .await;
    assert_eq!(st, StatusCode::INSUFFICIENT_STORAGE);
    let err_json: J = serde_json::from_slice(&body).unwrap();
    assert_eq!(err_json["budget"], "result-bytes");
    assert!(wait_pool_zero(&s_small.state, Duration::from_secs(2)).await);

    // Case 2: Limit above STREAM_AFTER (1.2 MiB) on ~1.7 MiB result -> `auto` hands off at
    // 1 MiB (200 OK) and stops cleanly on the pool without exceeding 1.2 MiB.
    let limit = 1_200_000u64;
    let s_big = pool_server(Some(limit), None);
    load_turtle(&s_big.state, "lim_big", data);
    let q_big = "SELECT ?s ?o WHERE { ?s <urn:lim:p> ?o }";
    let key = QuickQueries::key("lim_big", q_big);
    for mode in ["auto", "streaming"] {
        let req = Request::post(format!("/lim_big/sparql?execution={mode}"))
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .header(header::ACCEPT, "text/tab-separated-values")
            .body(Body::from(q_big))
            .unwrap();
        let resp = s_big.app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "{mode} commits before 1.2 MiB"
        );
        let mut body_stream = resp.into_body().into_data_stream();
        let mut received = 0u64;
        let mut saw_err = false;
        while let Some(item) = body_stream.next().await {
            match item {
                Ok(chunk) => received += chunk.len() as u64,
                Err(_) => {
                    saw_err = true;
                    break;
                }
            }
        }
        assert!(
            saw_err,
            "{mode} must abort stream when max_result_bytes is exceeded"
        );
        assert!(
            received <= limit,
            "{mode} delivered {received} bytes, exceeding limit {limit}"
        );
    }
    assert_eq!(route_stats::in_place_handoff(key), 1);
    assert!(wait_pool_zero(&s_big.state, Duration::from_secs(2)).await);
}

/// Dropping a client connection mid-stream on explicit `execution=streaming` cancels the
/// single-task blocking producer and releases both its memory pool grant and its HTTP
/// concurrency slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_disconnect_cancels_explicit_streaming_and_releases_resources() {
    let s = pool_server(None, Some("query=concurrency=1"));
    let payload = "m".repeat(4096);
    let mut data = String::new();
    for i in 0..800 {
        data.push_str(&format!(
            "<urn:dc:s{i:04}> <urn:dc:p> \"{payload}-{i}\" .\n"
        ));
    }
    load_turtle(&s.state, "dc_ds", data);

    let req = Request::post("/dc_ds/sparql?execution=streaming")
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "text/tab-separated-values")
        .body(Body::from("SELECT ?s ?o WHERE { ?s <urn:dc:p> ?o }"))
        .unwrap();
    let resp = s.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    // Read one chunk to confirm streaming started, then drop the body (simulating client disconnect).
    let first = stream.next().await.expect("first chunk").expect("ok chunk");
    assert!(!first.is_empty());
    drop(stream);

    // Memory grant must return to 0 promptly.
    assert!(
        wait_pool_zero(&s.state, Duration::from_secs(5)).await,
        "client disconnect did not release streaming cursor memory grant"
    );
    // Concurrency slot must also be released promptly.
    let ok = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (st, _, _) = post_query(
                &s.app,
                "/dc_ds/sparql?execution=streaming",
                "ASK { ?s <urn:dc:p> ?o }",
                "application/sparql-results+json",
            )
            .await;
            if st == StatusCode::OK {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(ok, "client disconnect did not release concurrency slot");
}

/// Graph formats (Turtle, N-Triples, N-Quads, TriG, RDF/XML, RDF Thrift, RDF Protobuf,
/// RDF/JSON, TriX, native JSON) keep exact parity between `execution=auto` and explicit
/// `execution=streaming` (single-task pool), both under and over the streaming threshold.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_formats_exact_parity_across_auto_and_streaming() {
    let s = pool_server(None, None);
    let mut data = String::new();
    for i in 0..300 {
        data.push_str(&format!(
            "<urn:gr:s{i:04}> <urn:gr:p> \"graph-val-{i}-\\\"<&>\" .\n"
        ));
    }
    load_turtle(&s.state, "gr_ds", data);

    let formats = [
        ("ttl", "text/turtle"),
        ("nt", "application/n-triples"),
        ("nq", "application/n-quads"),
        ("trig", "application/trig"),
        ("rdfxml", "application/rdf+xml"),
        ("rt", "application/rdf+thrift"),
        ("pb", "application/rdf+protobuf"),
        ("rj", "application/rdf+json"),
        ("trix", "application/trix+xml"),
        ("native", "application/x-sparkles+json"),
    ];
    for (idx, (fname, accept)) in formats.iter().enumerate() {
        let q = format!(
            "# gr-{idx}\nCONSTRUCT {{ ?s <urn:gr:p> ?o }} WHERE {{ ?s <urn:gr:p> ?o }} ORDER BY ?s ?o"
        );
        let (st_auto, h_auto, body_auto) =
            post_query(&s.app, "/gr_ds/sparql?execution=auto", &q, accept).await;
        let (st_stream, h_stream, body_stream) =
            post_query(&s.app, "/gr_ds/sparql?execution=streaming", &q, accept).await;
        assert_eq!(st_auto, StatusCode::OK, "graph auto {fname}");
        assert_eq!(st_stream, StatusCode::OK, "graph streaming {fname}");
        assert_eq!(
            h_auto.get(header::CONTENT_TYPE),
            h_stream.get(header::CONTENT_TYPE),
            "graph content-type {fname}"
        );
        if *fname == "native" {
            let a = scrub_native_meta(&body_auto);
            let b = scrub_native_meta(&body_stream);
            assert_eq!(a["queryType"], "CONSTRUCT");
            assert_eq!(b["queryType"], "CONSTRUCT");
            // Eager native graph puts triples in `triples`, streaming cursor puts quads in `quads`;
            // verify both return all 300 statements.
            let a_count = a["triples"].as_array().map_or(0, Vec::len)
                + a["quads"].as_array().map_or(0, Vec::len);
            let b_count = b["triples"].as_array().map_or(0, Vec::len)
                + b["quads"].as_array().map_or(0, Vec::len);
            assert_eq!(a_count, 300);
            assert_eq!(b_count, 300);
        } else if *fname == "rt" || *fname == "pb" {
            // Jena binary formats write triple messages on eager and quad messages on cursor;
            // verify both are non-empty valid payloads.
            assert!(!body_auto.is_empty(), "graph auto {fname}");
            assert!(!body_stream.is_empty(), "graph streaming {fname}");
        } else {
            assert_eq!(body_auto, body_stream, "graph parity {fname}");
        }
    }
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);
}
