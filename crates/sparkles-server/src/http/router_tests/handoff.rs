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

/// Graph formats (Turtle, N-Triples, JSON-LD, N-Quads, TriG, RDF/XML, RDF Thrift, RDF
/// Protobuf, RDF/JSON, TriX, native JSON) keep parity between `execution=auto` and
/// explicit `execution=streaming` below the streaming threshold.
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
        ("jsonld", "application/ld+json"),
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
            // Native graph JSON encodes triples as 3-term arrays in `triples` and quads
            // as 4-slot arrays in `quads` (with `null` in slot 3 for the default graph).
            let stmts = |v: &J, label: &str| -> Vec<(J, J, J)> {
                let mut out = Vec::new();
                for (field, arity) in [("triples", 3usize), ("quads", 4usize)] {
                    if let Some(arr) = v[field].as_array() {
                        for (row_idx, row) in arr.iter().enumerate() {
                            let cells = row.as_array().unwrap_or_else(|| {
                                panic!("{label} {field}[{row_idx}] must be an array: {row}")
                            });
                            assert_eq!(
                                cells.len(),
                                arity,
                                "{label} {field}[{row_idx}] arity mismatch: {row}"
                            );
                            if arity == 4 {
                                assert!(
                                    cells[3].is_null(),
                                    "{label} {field}[{row_idx}] default-graph slot must be null: {row}"
                                );
                            }
                            for (slot, term) in cells[..3].iter().enumerate() {
                                let obj = term.as_object().unwrap_or_else(|| {
                                    panic!(
                                        "{label} {field}[{row_idx}][{slot}] must be a term object: {term}"
                                    )
                                });
                                assert!(
                                    obj.get("type")
                                        .and_then(J::as_str)
                                        .is_some_and(|s| !s.is_empty()),
                                    "{label} {field}[{row_idx}][{slot}] missing non-empty type: {term}"
                                );
                                assert!(
                                    obj.get("value")
                                        .and_then(J::as_str)
                                        .is_some_and(|s| !s.is_empty()),
                                    "{label} {field}[{row_idx}][{slot}] missing non-empty value: {term}"
                                );
                            }
                            out.push((cells[0].clone(), cells[1].clone(), cells[2].clone()));
                        }
                    }
                }
                out
            };
            let a_stmts = stmts(&a, "auto");
            let b_stmts = stmts(&b, "streaming");
            let expected: Vec<(J, J, J)> = (0..300)
                .map(|i| {
                    (
                        serde_json::json!({ "type": "uri", "value": format!("urn:gr:s{i:04}") }),
                        serde_json::json!({ "type": "uri", "value": "urn:gr:p" }),
                        serde_json::json!({ "type": "literal", "value": format!("graph-val-{i}-\"<&>") }),
                    )
                })
                .collect();
            assert_eq!(a_stmts.len(), 300);
            assert_eq!(a_stmts, expected, "native auto statements");
            assert_eq!(a_stmts, b_stmts, "native statement parity");
        } else if *fname == "rt" || *fname == "pb" {
            // Jena binary formats write triple messages on eager and quad messages on cursor;
            // decode both payloads and verify they contain the exact same 300 statements.
            let jena_fmt = if *fname == "rt" {
                sparkles::jena_formats::JenaFormat::Thrift
            } else {
                sparkles::jena_formats::JenaFormat::Protobuf
            };
            let mut quads_auto = Vec::new();
            sparkles::jena_formats::read(jena_fmt, &body_auto[..], None, &mut |q| {
                quads_auto.push(q);
                Ok(())
            })
            .unwrap_or_else(|e| panic!("decode auto {fname}: {e}"));
            let mut quads_stream = Vec::new();
            sparkles::jena_formats::read(jena_fmt, &body_stream[..], None, &mut |q| {
                quads_stream.push(q);
                Ok(())
            })
            .unwrap_or_else(|e| panic!("decode streaming {fname}: {e}"));
            assert_eq!(quads_auto.len(), 300, "graph auto {fname} count");
            assert_eq!(
                quads_auto, quads_stream,
                "graph decoded statement parity {fname}"
            );
        } else {
            assert_eq!(body_auto, body_stream, "graph parity {fname}");
        }
    }
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);
}

async fn read_metrics(app: &Router) -> String {
    let resp = app
        .clone()
        .oneshot(Request::get("/$/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn metric_sample(text: &str, series: &str) -> Option<f64> {
    text.lines()
        .find_map(|l| l.strip_prefix(series)?.strip_prefix(' '))
        .map(|v| v.parse().unwrap())
}

async fn wait_metric(app: &Router, series: &str, want: f64) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let m = read_metrics(app).await;
        if metric_sample(&m, series) == Some(want) {
            return m;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {series} == {want}:\n{m}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A single row larger than `STREAM_AFTER` hands off without buffering the entire row as
/// one oversized initial chunk; the initial chunk stays `<= STREAM_AFTER + 1` and every
/// subsequent chunk stays `<= CHUNK`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_oversized_row_hands_off_with_bounded_initial_chunk() {
    let s = pool_server(None, None);
    let huge = "H".repeat(crate::http::stream::STREAM_AFTER + 200_000);
    // Load 2,000 tiny triples (~150 KiB total, well below STREAM_AFTER) plus one >1.2 MiB
    // literal so `execution=auto` chooses a SELECT cursor and the single oversized row is
    // what crosses STREAM_AFTER.
    let mut data = String::new();
    for i in 0..2000 {
        data.push_str(&format!("<urn:huge:s{i:04}> <urn:huge:p> \"s{i}\" .\n"));
    }
    data.push_str(&format!("<urn:huge:big> <urn:huge:p> \"{huge}\" .\n"));
    load_turtle(&s.state, "big_row", data);

    for (idx, accept) in [
        "application/sparql-results+json",
        "text/tab-separated-values",
    ]
    .iter()
    .enumerate()
    {
        let q = format!("# huge-row-{idx}\nSELECT ?s ?o WHERE {{ ?s <urn:huge:p> ?o }}");
        let key = QuickQueries::key("big_row", &q);
        let req = Request::post("/big_row/sparql?execution=auto")
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .header(header::ACCEPT, *accept)
            .body(Body::from(q.clone()))
            .unwrap();
        let resp = s.app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers().get(header::CONTENT_LENGTH).is_none(),
            "oversized single row must stream, not return a whole body"
        );
        assert_eq!(route_stats::in_place_handoff(key), 1);
        assert_eq!(route_stats::in_place_whole(key), 0);

        let mut stream = resp.into_body().into_data_stream();
        let mut chunks = Vec::new();
        let mut body = Vec::new();
        while let Some(item) = stream.next().await {
            let chunk = item.unwrap();
            chunks.push(chunk.len());
            body.extend_from_slice(&chunk);
        }
        assert!(
            chunks.len() >= 2,
            "expected multiple chunks, got {chunks:?}"
        );
        assert!(
            chunks[0] <= crate::http::stream::STREAM_AFTER + 1,
            "initial chunk {} exceeded STREAM_AFTER + 1",
            chunks[0]
        );
        for (i, &len) in chunks.iter().enumerate().skip(1) {
            assert!(
                len <= crate::http::stream::CHUNK,
                "chunk {i} ({len} bytes) exceeded CHUNK"
            );
        }

        let (st_stream, _, expected) =
            post_query(&s.app, "/big_row/sparql?execution=streaming", &q, accept).await;
        assert_eq!(st_stream, StatusCode::OK);
        assert_eq!(body, expected);
    }
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);
}

/// When the rows themselves end at or just below `STREAM_AFTER` and the JSON footer is what
/// first pushes total size past `STREAM_AFTER`, `execution=auto` still hands off to the
/// streaming producer with a bounded initial chunk rather than returning an oversized whole
/// body.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn footer_crossing_threshold_hands_off_with_bounded_initial_chunk() {
    let s = pool_server(None, None);
    let base_q = "SELECT ?o WHERE { ?s <urn:ft:p> ?o }";
    // Load 2,000 rows with no sort barrier so `execution=auto` selects a SELECT cursor,
    // then pad row 0 so the full JSON document is `STREAM_AFTER + 2` (total length is
    // order-independent: all 2,000 rows + header end at `STREAM_AFTER - 1`, and only the
    // 3-byte JSON footer `]}` crosses `STREAM_AFTER`).
    let mut probe_data = String::from("<urn:ft:s0000> <urn:ft:p> \"a\" .\n");
    for i in 1..2000 {
        probe_data.push_str(&format!("<urn:ft:s{i:04}> <urn:ft:p> \"v\" .\n"));
    }
    load_turtle(&s.state, "ft_probe", probe_data);
    let (_, _, probe_body) = post_query(
        &s.app,
        "/ft_probe/sparql?execution=streaming",
        base_q,
        "application/sparql-results+json",
    )
    .await;
    let target_total = crate::http::stream::STREAM_AFTER + 2;
    let pad = target_total - probe_body.len();
    let padded = "a".repeat(1 + pad);
    let mut exact_data = format!("<urn:ft:s0000> <urn:ft:p> \"{padded}\" .\n");
    for i in 1..2000 {
        exact_data.push_str(&format!("<urn:ft:s{i:04}> <urn:ft:p> \"v\" .\n"));
    }
    load_turtle(&s.state, "ft_exact", exact_data);

    let key = QuickQueries::key("ft_exact", base_q);
    let req = Request::post("/ft_exact/sparql?execution=auto")
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "application/sparql-results+json")
        .body(Body::from(base_q))
        .unwrap();
    let resp = s.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get(header::CONTENT_LENGTH).is_none(),
        "body crossing STREAM_AFTER on its footer must stream, not return whole"
    );
    assert_eq!(route_stats::in_place_handoff(key), 1);
    assert_eq!(route_stats::in_place_whole(key), 0);

    let mut stream = resp.into_body().into_data_stream();
    let mut chunks = Vec::new();
    let mut body = Vec::new();
    while let Some(item) = stream.next().await {
        let chunk = item.unwrap();
        chunks.push(chunk.len());
        body.extend_from_slice(&chunk);
    }
    assert_eq!(body.len(), target_total);
    assert!(
        chunks[0] <= crate::http::stream::STREAM_AFTER + 1,
        "initial chunk {} exceeded STREAM_AFTER + 1",
        chunks[0]
    );
    let parsed: J = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        parsed["results"]["bindings"].as_array().unwrap().len(),
        2000
    );
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);
}

/// Dropping an unread whole in-place cursor response records `cancelled` (499), matching
/// the pool route's body-aware observation contract (X05 §6).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_unread_in_place_whole_cursor_body_records_cancelled() {
    let s = pool_server(None, None);
    let mut data = String::new();
    for i in 0..2000 {
        data.push_str(&format!("<urn:drop:s{i}> <urn:drop:p> {i} .\n"));
    }
    load_turtle(&s.state, "drop_whole", data);

    let q = "SELECT ?s ?o WHERE { ?s <urn:drop:p> ?o }";
    let key = QuickQueries::key("drop_whole", q);
    let req = Request::post("/drop_whole/sparql?execution=auto")
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "text/tab-separated-values")
        .body(Body::from(q))
        .unwrap();
    let resp = s.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().contains_key(header::CONTENT_LENGTH));
    assert_eq!(route_stats::in_place_whole(key), 1);

    // Drop the whole cursor response without reading any body bytes.
    drop(resp);

    let m = wait_metric(
        &s.app,
        r#"sparkles_requests_total{dataset="drop_whole",operation="query",outcome="cancelled"}"#,
        1.0,
    )
    .await;
    assert_eq!(
        metric_sample(
            &m,
            r#"sparkles_requests_total{dataset="drop_whole",operation="query",outcome="ok"}"#
        )
        .unwrap_or(0.0),
        0.0,
        "dropping an unread whole cursor body must not record ok:\n{m}"
    );
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);
}

/// When the client disconnects after the hand-off signal commits headers but before the
/// initial chunk enqueues, metrics record 0 response bytes, 0 result rows, and a cancelled
/// outcome, while releasing both query memory and the HTTP concurrency permit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_initial_handoff_send_reports_zero_bytes_zero_rows_and_releases_resources() {
    use crate::http::algo::probe::{CURRENT, Gate, Probe, Release};

    let s = pool_server(None, Some("query=concurrency=1"));
    let payload = "p".repeat(800);
    let mut data = String::new();
    for i in 0..2000 {
        data.push_str(&format!(
            "<urn:fail0:s{i:04}> <urn:fail0:p> \"{payload}-{i}\" .\n"
        ));
    }
    load_turtle(&s.state, "fail_init", data);

    let q = "SELECT ?s ?o WHERE { ?s <urn:fail0:p> ?o }";
    let key = QuickQueries::key("fail_init", q);
    let gate = Arc::new(Gate::default());
    let _release = Release(gate.clone());
    let probe = Probe {
        producer: Some(gate.clone()),
        ..Default::default()
    };
    let req = Request::post("/fail_init/sparql?execution=auto")
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "text/tab-separated-values")
        .body(Body::from(q))
        .unwrap();

    let entered = gate.entered.notified();
    tokio::pin!(entered);
    entered.as_mut().enable();
    let resp = CURRENT
        .scope(probe, s.app.clone().oneshot(req))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(route_stats::in_place_handoff(key), 1);
    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .unwrap();

    // Drop response before the first chunk enqueues, then release the producer gate.
    drop(resp);
    let done = gate.done.notified();
    tokio::pin!(done);
    done.as_mut().enable();
    gate.release();
    tokio::time::timeout(Duration::from_secs(5), done)
        .await
        .unwrap();

    let m = wait_metric(
        &s.app,
        r#"sparkles_requests_total{dataset="fail_init",operation="query",outcome="cancelled"}"#,
        1.0,
    )
    .await;
    assert_eq!(
        metric_sample(
            &m,
            r#"sparkles_response_bytes_total{dataset="fail_init",operation="query"}"#
        )
        .unwrap_or(0.0),
        0.0,
        "failed initial send must report 0 response bytes:\n{m}"
    );
    assert_eq!(
        metric_sample(&m, r#"sparkles_result_rows_total{dataset="fail_init"}"#).unwrap_or(0.0),
        0.0,
        "failed initial send must report 0 result rows:\n{m}"
    );
    assert!(wait_pool_zero(&s.state, Duration::from_secs(5)).await);

    // Concurrency slot must be released promptly.
    let (st, _, _) = post_query(
        &s.app,
        "/fail_init/sparql?execution=auto",
        "ASK { ?s <urn:fail0:p> ?o }",
        "application/sparql-results+json",
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

/// Real >1 MiB `GraphCursor` in-place + hand-off across N-Triples, Turtle, deferred
/// RDF/JSON, native JSON, DESCRIBE, and `max_result_bytes`, plus >1 MiB HTTP router parity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_cursor_over_threshold_in_place_handoff_and_http_parity() {
    use crate::http::cursor::{AutoOutcome, AutoRequest, run_in_place_with_mode};
    use crate::http::{OutFormat, Params, stream};
    use sparkles::sparql::ExecutionMode;
    use sparkles::sparql::results::SolutionsFormat;

    let s = pool_server(None, None);
    let payload = "g".repeat(650);
    let mut data = String::new();
    // 2,200 statements * ~700 bytes ≈ 1.54 MiB (> STREAM_AFTER = 1 MiB)
    for i in 0..2200 {
        data.push_str(&format!(
            "<urn:gbig:s{i:04}> <urn:gbig:p> \"{payload}-{i}\" .\n"
        ));
    }
    load_turtle(&s.state, "gbig_ds", data);
    let ds = s.state.get("gbig_ds").unwrap();

    let cases = [
        (
            "nt",
            OutFormat::Rdf(oxrdfio::RdfFormat::NTriples),
            false,
            "application/n-triples",
        ),
        (
            "ttl",
            OutFormat::Rdf(oxrdfio::RdfFormat::Turtle),
            false,
            "text/turtle",
        ),
        (
            "rj",
            OutFormat::Jena(sparkles::jena_formats::JenaFormat::RdfJson),
            false,
            "application/rdf+json",
        ),
        (
            "native",
            OutFormat::Rdf(oxrdfio::RdfFormat::NTriples),
            true,
            "application/x-sparkles+json",
        ),
    ];
    for (idx, (name, rdf_format, native_graph, accept)) in cases.iter().enumerate() {
        let q = format!(
            "# gbig-{idx}\nCONSTRUCT {{ ?s <urn:gbig:p> ?o }} WHERE {{ ?s <urn:gbig:p> ?o }} ORDER BY ?s ?o"
        );
        let key = QuickQueries::key("gbig_ds", &q);
        let params = Params(Default::default());
        let uri: axum::http::Uri = "/gbig_ds/sparql?execution=auto".parse().unwrap();
        let (options, guard) = crate::http::query_options_for_test(&s.state, &ds);
        let outcome = run_in_place_with_mode(
            ds.clone(),
            AutoRequest {
                timeouts: stream::Timeouts::default(),
                query: q.clone(),
                options,
                at: None,
                params: &params,
                uri: &uri,
                guard,
                limit: None,
                format: if *native_graph {
                    SolutionsFormat::Sparkles
                } else {
                    SolutionsFormat::Json
                },
                thrift: false,
                rdf_format: *rdf_format,
                native_graph: *native_graph,
                prefixes: Default::default(),
                io: None,
                quick_key: key,
                auto: true,
                trial: false,
                claim: None,
            },
            ExecutionMode::Streaming,
        )
        .await
        .unwrap_or_else(|e| panic!("graph handoff {name} failed: {} {}", e.0, e.1));
        let AutoOutcome::Responded(resp) = outcome else {
            panic!("expected GraphCursor response for {name}");
        };
        assert_eq!(resp.status(), StatusCode::OK, "graph handoff {name}");
        assert_eq!(
            route_stats::in_place_handoff(key),
            1,
            "graph {name} >1 MiB must trigger in-place hand-off"
        );
        assert_eq!(route_stats::in_place_whole(key), 0);

        let mut stream = resp.into_body().into_data_stream();
        let mut chunks = Vec::new();
        let mut body_handoff = Vec::new();
        while let Some(item) = stream.next().await {
            let chunk = item.unwrap();
            chunks.push(chunk.len());
            body_handoff.extend_from_slice(&chunk);
        }
        assert!(
            body_handoff.len() > stream::STREAM_AFTER,
            "{name} body {} must exceed STREAM_AFTER",
            body_handoff.len()
        );
        assert!(
            chunks[0] <= stream::STREAM_AFTER + 1,
            "{name} initial chunk {} exceeded STREAM_AFTER + 1",
            chunks[0]
        );
        for (i, &len) in chunks.iter().enumerate().skip(1) {
            assert!(
                len <= stream::CHUNK,
                "{name} chunk {i} ({len}) exceeded CHUNK"
            );
        }

        // Compare byte-for-byte with explicit HTTP `execution=streaming`.
        let (st_stream, _, body_stream) =
            post_query(&s.app, "/gbig_ds/sparql?execution=streaming", &q, accept).await;
        assert_eq!(st_stream, StatusCode::OK, "streaming {name}");
        if *native_graph {
            assert_eq!(
                scrub_native_meta(&body_handoff)["quads"],
                scrub_native_meta(&body_stream)["quads"]
            );
        } else {
            assert_eq!(body_handoff, body_stream, "graph handoff parity {name}");
        }
    }

    // DESCRIBE over >1 MiB via GraphCursor in-place hand-off.
    let desc_q = "DESCRIBE ?s WHERE { ?s <urn:gbig:p> ?o }";
    let desc_key = QuickQueries::key("gbig_ds", desc_q);
    let params = Params(Default::default());
    let uri: axum::http::Uri = "/gbig_ds/sparql?execution=auto".parse().unwrap();
    let (options, guard) = crate::http::query_options_for_test(&s.state, &ds);
    let AutoOutcome::Responded(resp) = run_in_place_with_mode(
        ds.clone(),
        AutoRequest {
            timeouts: stream::Timeouts::default(),
            query: desc_q.into(),
            options,
            at: None,
            params: &params,
            uri: &uri,
            guard,
            limit: None,
            format: SolutionsFormat::Json,
            thrift: false,
            rdf_format: OutFormat::Rdf(oxrdfio::RdfFormat::NTriples),
            native_graph: false,
            prefixes: Default::default(),
            io: None,
            quick_key: desc_key,
            auto: true,
            trial: false,
            claim: None,
        },
        ExecutionMode::Streaming,
    )
    .await
    .unwrap_or_else(|e| panic!("DESCRIBE handoff failed: {} {}", e.0, e.1)) else {
        panic!("expected GraphCursor response for DESCRIBE");
    };
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(route_stats::in_place_handoff(desc_key), 1);
    let desc_body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(desc_body.len() > stream::STREAM_AFTER);

    // max_result_bytes on GraphCursor hand-off: 1.2 MiB limit aborts cleanly <= limit.
    let limit = 1_200_000u64;
    let lim_q = "CONSTRUCT { ?s <urn:gbig:p> ?o } WHERE { ?s <urn:gbig:p> ?o }";
    let lim_key = QuickQueries::key("gbig_ds", lim_q);
    let (options, guard) = crate::http::query_options_for_test(&s.state, &ds);
    let AutoOutcome::Responded(resp) = run_in_place_with_mode(
        ds.clone(),
        AutoRequest {
            timeouts: stream::Timeouts::default(),
            query: lim_q.into(),
            options,
            at: None,
            params: &params,
            uri: &uri,
            guard,
            limit: Some(limit),
            format: SolutionsFormat::Json,
            thrift: false,
            rdf_format: OutFormat::Rdf(oxrdfio::RdfFormat::NTriples),
            native_graph: false,
            prefixes: Default::default(),
            io: None,
            quick_key: lim_key,
            auto: true,
            trial: false,
            claim: None,
        },
        ExecutionMode::Streaming,
    )
    .await
    .unwrap_or_else(|e| panic!("max_result_bytes graph handoff failed: {} {}", e.0, e.1)) else {
        panic!("expected GraphCursor response for max_result_bytes");
    };
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(route_stats::in_place_handoff(lim_key), 1);
    let mut stream = resp.into_body().into_data_stream();
    let mut received = 0u64;
    let mut saw_err = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(c) => received += c.len() as u64,
            Err(_) => {
                saw_err = true;
                break;
            }
        }
    }
    assert!(saw_err, "graph handoff must abort past max_result_bytes");
    assert!(received <= limit, "received {received} > limit {limit}");
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);
}

/// An `ASK` query that calls a graph algorithm executes as a cursor under `execution=auto`
/// and must never be remembered as quick or scheduled in place on subsequent runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn algorithm_ask_is_never_remembered_as_quick_or_run_in_place() {
    let s = pool_server(None, None);
    load_turtle(
        &s.state,
        "algo_ask",
        "<urn:a> <urn:e> <urn:b> .\n<urn:b> <urn:e> <urn:c> .\n".into(),
    );
    let q = "PREFIX algo: <urn:x-sparkles:algo#> ASK { SERVICE algo:wcc { [] algo:node ?n ; algo:component ?c } }";
    let key = QuickQueries::key("algo_ask", q);
    crate::http::QUICK.ignore_time(key);

    for _ in 0..3 {
        let (st, _, body) = post_query(
            &s.app,
            "/algo_ask/sparql?execution=auto",
            q,
            "application/sparql-results+json",
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let parsed: J = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["boolean"], true);
    }
    assert_eq!(
        route_stats::pool_auto(key),
        3,
        "algorithm ASK must execute through the pool_auto cursor route on every run"
    );
    assert!(
        !crate::http::QUICK.is_quick(key),
        "algorithm ASK must never be remembered as quick"
    );
    assert_eq!(
        crate::http::inline::ran_by_key(key),
        0,
        "algorithm ASK must never run in place"
    );
    assert_eq!(route_stats::in_place_whole(key), 0);
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);
}

/// `QueryCursor::uses_algorithms`, `GraphCursor::uses_algorithms`, and the
/// `InPlaceWork::CursorWhole` algorithmic guard record algorithm-calling cursors as `Slow`
/// rather than quick.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn algorithm_select_and_construct_cursors_report_uses_algorithms_and_never_mark_quick() {
    use crate::http::cursor::{AutoOutcome, AutoRequest, run_in_place_with_mode};
    use crate::http::{OutFormat, Params, stream};
    use sparkles::sparql::results::SolutionsFormat;
    use sparkles::sparql::{
        CursorOptions, ExecutionMode, QueryExecution, QueryOptions, query_cursor,
    };

    let s = pool_server(None, None);
    load_turtle(
        &s.state,
        "algo_cur",
        "<urn:a> <urn:e> <urn:b> .\n<urn:b> <urn:e> <urn:c> .\n".into(),
    );
    let ds = s.state.get("algo_cur").unwrap();
    let snap = ds.store.snapshot();

    let algo_select = "PREFIX algo: <urn:x-sparkles:algo#> SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:node ?n ; algo:component ?c } }";
    let plain_select = "SELECT ?s ?o WHERE { ?s <urn:e> ?o }";
    let algo_construct = "PREFIX algo: <urn:x-sparkles:algo#> CONSTRUCT { ?n <urn:comp> ?c } WHERE { SERVICE algo:wcc { [] algo:node ?n ; algo:component ?c } }";
    let plain_construct = "CONSTRUCT { ?s <urn:e> ?o } WHERE { ?s <urn:e> ?o }";

    // Direct core accessor checks on QueryCursor, GraphCursor, and QueryExecution.
    let exec = query_cursor(
        snap.clone(),
        algo_select,
        &QueryOptions::default(),
        &CursorOptions::default(),
    )
    .unwrap();
    assert!(exec.uses_algorithms());
    match exec {
        QueryExecution::Select(c) => assert!(c.uses_algorithms()),
        _ => panic!("expected QueryExecution::Select"),
    }

    let exec = query_cursor(
        snap.clone(),
        plain_select,
        &QueryOptions::default(),
        &CursorOptions::default(),
    )
    .unwrap();
    assert!(!exec.uses_algorithms());
    match exec {
        QueryExecution::Select(c) => assert!(!c.uses_algorithms()),
        _ => panic!("expected QueryExecution::Select"),
    }

    let exec = query_cursor(
        snap.clone(),
        algo_construct,
        &QueryOptions::default(),
        &CursorOptions::default(),
    )
    .unwrap();
    assert!(exec.uses_algorithms());
    match exec {
        QueryExecution::Graph(c) => assert!(c.uses_algorithms()),
        _ => panic!("expected QueryExecution::Graph"),
    }

    let exec = query_cursor(
        snap,
        plain_construct,
        &QueryOptions::default(),
        &CursorOptions::default(),
    )
    .unwrap();
    assert!(!exec.uses_algorithms());
    match exec {
        QueryExecution::Graph(c) => assert!(!c.uses_algorithms()),
        _ => panic!("expected QueryExecution::Graph"),
    }

    // Direct `InPlaceWork::CursorWhole` guard check for algorithmic SELECT and CONSTRUCT.
    let params = Params(Default::default());
    let uri: axum::http::Uri = "/algo_cur/sparql?execution=auto".parse().unwrap();
    for (label, query) in [("select", algo_select), ("construct", algo_construct)] {
        let key = QuickQueries::key("algo_cur", query);
        crate::http::QUICK.ignore_time(key);
        let (options, guard) = crate::http::query_options_for_test(&s.state, &ds);
        let AutoOutcome::Responded(resp) = run_in_place_with_mode(
            ds.clone(),
            AutoRequest {
                timeouts: stream::Timeouts::default(),
                query: query.into(),
                options,
                at: None,
                params: &params,
                uri: &uri,
                guard,
                limit: None,
                format: SolutionsFormat::Json,
                thrift: false,
                rdf_format: OutFormat::Rdf(oxrdfio::RdfFormat::NTriples),
                native_graph: false,
                prefixes: Default::default(),
                io: None,
                quick_key: key,
                auto: true,
                trial: false,
                claim: None,
            },
            ExecutionMode::Streaming,
        )
        .await
        .unwrap_or_else(|e| panic!("algo in-place {label} failed: {} {}", e.0, e.1)) else {
            panic!("expected CursorWhole response for algo {label}");
        };
        assert_eq!(resp.status(), StatusCode::OK, "algo {label}");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(!body.is_empty(), "algo {label} body");
        assert_eq!(
            route_stats::in_place_whole(key),
            1,
            "algo {label} should finish through InPlaceWork::CursorWhole"
        );
        assert_eq!(
            crate::http::QUICK.history(key),
            History::Slow,
            "InPlaceWork::CursorWhole must record algorithmic {label} as Slow"
        );
        assert!(
            !crate::http::QUICK.is_quick(key),
            "algorithmic {label} must never be remembered as quick"
        );
    }
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);
}

/// A cursor whose history settles to `PreserveAuto` (for example after an eager memory
/// refusal) stays on the in-place whole cursor route on subsequent `execution=auto` runs
/// without waking the blocking pool, with exact parity against eager and streaming output.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settled_quick_cursor_stays_in_place_on_subsequent_runs() {
    use crate::http::inline::QuickExecution;

    let s = pool_server(None, None);
    let mut data = String::new();
    for i in 0..2000 {
        data.push_str(&format!("<urn:set:s{i:04}> <urn:set:p> {i} .\n"));
    }
    load_turtle(&s.state, "settled_cur", data);

    let q = "SELECT ?s ?o WHERE { ?s <urn:set:p> ?o }";
    let key = QuickQueries::key("settled_cur", q);
    crate::http::QUICK.ignore_time(key);
    // Record an eager memory failure so quick cursor runs settle in PreserveAuto.
    crate::http::QUICK.record_eager_failure(key);

    let (st1, _, body1) = post_query(
        &s.app,
        "/settled_cur/sparql?execution=auto",
        q,
        "text/tab-separated-values",
    )
    .await;
    assert_eq!(st1, StatusCode::OK);
    assert_eq!(
        crate::http::QUICK.history(key),
        History::Quick(QuickExecution::PreserveAuto),
        "quick cursor after eager failure must settle to PreserveAuto"
    );
    assert_eq!(route_stats::pool_auto(key), 1);
    assert_eq!(route_stats::in_place_whole(key), 0);

    // Subsequent runs with PreserveAuto must stay on the in-place whole cursor route.
    for expected_in_place in 1..=2 {
        let (st, headers, body) = post_query(
            &s.app,
            "/settled_cur/sparql?execution=auto",
            q,
            "text/tab-separated-values",
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(headers.contains_key(header::CONTENT_LENGTH));
        assert_eq!(body, body1);
        assert_eq!(route_stats::in_place_whole(key), expected_in_place);
        assert_eq!(route_stats::pool_auto(key), 1);
    }

    let (st_eager, _, body_eager) = post_query(
        &s.app,
        "/settled_cur/sparql?execution=eager",
        q,
        "text/tab-separated-values",
    )
    .await;
    assert_eq!(st_eager, StatusCode::OK);
    assert_eq!(body1, body_eager);

    let (st_stream, _, body_stream) = post_query(
        &s.app,
        "/settled_cur/sparql?execution=streaming",
        q,
        "text/tab-separated-values",
    )
    .await;
    assert_eq!(st_stream, StatusCode::OK);
    assert_eq!(body1, body_stream);
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);
}

/// Row budgets (`max-rows` and `max-rows-produced`) fail pre-commit with HTTP 507 (with
/// `plan` and `commit` for Sparkles native JSON) when tripped before `STREAM_AFTER`, and
/// abort cleanly mid-stream after hand-off commits `200 OK` headers when tripped later.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn row_budget_enforced_pre_commit_and_post_handoff() {
    use crate::http::cursor::{AutoOutcome, AutoRequest, run_in_place_with_mode};
    use crate::http::{OutFormat, Params, stream};
    use crate::obs::{DeferredReport, Outcome};
    use sparkles::BudgetKind;
    use sparkles::sparql::ExecutionMode;
    use sparkles::sparql::results::SolutionsFormat;

    let s = pool_server(None, None);
    let payload = "r".repeat(400);
    let mut data = String::new();
    // 5,000 rows * ~430 bytes ≈ 2.15 MiB (> STREAM_AFTER = 1 MiB) across two cursor batches
    // (batch 1 = 4,096 rows ≈ 1.76 MiB > STREAM_AFTER; batch 2 = 904 rows).
    for i in 0..5000 {
        data.push_str(&format!(
            "<urn:rb:s{i:04}> <urn:rb:p> \"{payload}-{i}\" .\n"
        ));
    }
    load_turtle(&s.state, "rb_ds", data);
    let ds = s.state.get("rb_ds").unwrap();

    // Case 1: Pre-commit `max-rows` refusal on `execution=auto` with native JSON includes
    // `budget = "rows"`, `plan`, and `commit`.
    let q_rows = "# rb-pre-rows\nSELECT ?s ?o WHERE { ?s <urn:rb:p> ?o }";
    let (st, _, body) = post_query(
        &s.app,
        "/rb_ds/sparql?execution=auto&max-rows=100",
        q_rows,
        "application/x-sparkles+json",
    )
    .await;
    assert_eq!(st, StatusCode::INSUFFICIENT_STORAGE);
    let err_json: J = serde_json::from_slice(&body).unwrap();
    assert_eq!(err_json["budget"], "rows", "{err_json}");
    assert!(
        err_json.get("plan").is_some_and(|v| !v.is_null()),
        "pre-commit sparkles error should include plan: {err_json}"
    );
    assert!(err_json.get("commit").is_some(), "{err_json}");

    // Case 2: Pre-commit `max-rows` refusal inside `run_in_place_with_mode` (cursor fails
    // before `STREAM_AFTER`, exercising `InPlaceWork::CursorFailed` with error plan).
    let q_ip_fail = "# rb-ip-pre\nSELECT ?s ?o WHERE { ?s <urn:rb:p> ?o }";
    let key_ip_fail = QuickQueries::key("rb_ds", q_ip_fail);
    let params = Params(Default::default());
    let uri: axum::http::Uri = "/rb_ds/sparql?execution=auto".parse().unwrap();
    let (mut options, guard) = crate::http::query_options_for_test(&s.state, &ds);
    options.max_rows = Some(100);
    let AutoOutcome::Responded(resp) = run_in_place_with_mode(
        ds.clone(),
        AutoRequest {
            timeouts: stream::Timeouts::default(),
            query: q_ip_fail.into(),
            options,
            at: None,
            params: &params,
            uri: &uri,
            guard,
            limit: None,
            format: SolutionsFormat::Sparkles,
            thrift: false,
            rdf_format: OutFormat::Rdf(oxrdfio::RdfFormat::NTriples),
            native_graph: false,
            prefixes: Default::default(),
            io: None,
            quick_key: key_ip_fail,
            auto: true,
            trial: false,
            claim: None,
        },
        ExecutionMode::Streaming,
    )
    .await
    .unwrap_or_else(|e| panic!("in-place pre-commit row budget failed: {} {}", e.0, e.1)) else {
        panic!("expected CursorFailed response");
    };
    assert_eq!(resp.status(), StatusCode::INSUFFICIENT_STORAGE);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let err_json: J = serde_json::from_slice(&body).unwrap();
    assert_eq!(err_json["budget"], "rows", "{err_json}");
    assert!(
        err_json.get("plan").is_some_and(|v| !v.is_null()),
        "in-place cursor pre-commit error should include plan: {err_json}"
    );
    assert!(err_json.get("commit").is_some(), "{err_json}");

    // Case 3: Pre-commit `max-rows-produced` refusal on `execution=auto`.
    let q_prod = "# rb-pre-prod\nSELECT ?s ?o WHERE { ?s <urn:rb:p> ?o }";
    let (st, _, body) = post_query(
        &s.app,
        "/rb_ds/sparql?execution=auto&max-rows-produced=100",
        q_prod,
        "text/tab-separated-values",
    )
    .await;
    assert_eq!(st, StatusCode::INSUFFICIENT_STORAGE);
    let err_json: J = serde_json::from_slice(&body).unwrap();
    assert_eq!(err_json["budget"], "rows-produced", "{err_json}");

    // Case 4: Post-handoff `max-rows=4500` lets batch 1 (4,096 rows ≈ 1.76 MiB) cross
    // STREAM_AFTER and commit 200 OK before batch 2 trips the row limit and aborts mid-stream
    // with the exact row-budget error and deferred budget outcome on both in-place hand-off
    // and explicit `execution=streaming`.
    let q_mid = "# rb-mid\nSELECT ?s ?o WHERE { ?s <urn:rb:p> ?o }";
    let key_mid = QuickQueries::key("rb_ds", q_mid);
    let (mut options, guard) = crate::http::query_options_for_test(&s.state, &ds);
    options.max_rows = Some(4500);
    let AutoOutcome::Responded(resp) = run_in_place_with_mode(
        ds.clone(),
        AutoRequest {
            timeouts: stream::Timeouts::default(),
            query: q_mid.into(),
            options,
            at: None,
            params: &params,
            uri: &uri,
            guard,
            limit: None,
            format: SolutionsFormat::Tsv,
            thrift: false,
            rdf_format: OutFormat::Rdf(oxrdfio::RdfFormat::NTriples),
            native_graph: false,
            prefixes: Default::default(),
            io: None,
            quick_key: key_mid,
            auto: true,
            trial: false,
            claim: None,
        },
        ExecutionMode::Streaming,
    )
    .await
    .unwrap_or_else(|e| panic!("in-place post-handoff row budget failed: {} {}", e.0, e.1)) else {
        panic!("expected CursorHandoff response");
    };
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "in-place hand-off must commit headers before hitting row 4500"
    );
    assert_eq!(route_stats::in_place_handoff(key_mid), 1);
    let deferred = resp
        .extensions()
        .get::<DeferredReport>()
        .cloned()
        .expect("in-place handoff deferred report");
    let mut stream = resp.into_body().into_data_stream();
    let mut received = 0usize;
    let mut err_msg = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => received += chunk.len(),
            Err(err) => {
                err_msg = Some(err.to_string());
                break;
            }
        }
    }
    assert!(
        received > 0,
        "handoff must deliver prefix bytes before abort"
    );
    let err_msg = err_msg.expect("handoff must abort stream on post-commit row budget");
    assert!(
        err_msg.contains("exceeds the limit of 4500"),
        "expected post-handoff max-rows error, got: {err_msg}"
    );
    assert_eq!(
        deferred.snapshot_outcome(),
        Some((Some(Outcome::Budget), Some(BudgetKind::Rows))),
        "in-place hand-off deferred report must record Budget(Rows)"
    );

    let req = Request::post("/rb_ds/sparql?execution=streaming&max-rows=4500")
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "text/tab-separated-values")
        .body(Body::from(q_mid))
        .unwrap();
    let resp = s.app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "streaming must commit headers before hitting row 4500"
    );
    let mut stream = resp.into_body().into_data_stream();
    let mut received = 0usize;
    let mut err_msg = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => received += chunk.len(),
            Err(err) => {
                err_msg = Some(err.to_string());
                break;
            }
        }
    }
    assert!(
        received > 0,
        "streaming must deliver prefix bytes before abort"
    );
    let err_msg = err_msg.expect("streaming must abort stream on post-commit row budget");
    assert!(
        err_msg.contains("exceeds the limit of 4500"),
        "expected streaming max-rows error, got: {err_msg}"
    );

    // Case 5: Post-handoff `max-rows-produced=9000` allows batch 1 (Scan + Project = 8,192
    // rows produced, ~1.76 MiB > STREAM_AFTER) to commit 200 OK before batch 2 (total 9,096+
    // rows produced) trips `BudgetKind::RowsProduced` on both in-place hand-off and explicit
    // `execution=streaming`.
    let q_mid_prod = "# rb-mid-prod\nSELECT ?s ?o WHERE { ?s <urn:rb:p> ?o }";
    let key_mid_prod = QuickQueries::key("rb_ds", q_mid_prod);
    let (mut options, guard) = crate::http::query_options_for_test(&s.state, &ds);
    options.max_rows_produced = Some(9000);
    let AutoOutcome::Responded(resp) = run_in_place_with_mode(
        ds.clone(),
        AutoRequest {
            timeouts: stream::Timeouts::default(),
            query: q_mid_prod.into(),
            options,
            at: None,
            params: &params,
            uri: &uri,
            guard,
            limit: None,
            format: SolutionsFormat::Tsv,
            thrift: false,
            rdf_format: OutFormat::Rdf(oxrdfio::RdfFormat::NTriples),
            native_graph: false,
            prefixes: Default::default(),
            io: None,
            quick_key: key_mid_prod,
            auto: true,
            trial: false,
            claim: None,
        },
        ExecutionMode::Streaming,
    )
    .await
    .unwrap_or_else(|e| {
        panic!(
            "in-place post-handoff rows-produced budget failed: {} {}",
            e.0, e.1
        )
    }) else {
        panic!("expected CursorHandoff response for max-rows-produced");
    };
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "in-place hand-off must commit headers before hitting 9000 rows produced"
    );
    assert_eq!(route_stats::in_place_handoff(key_mid_prod), 1);
    let deferred_prod = resp
        .extensions()
        .get::<DeferredReport>()
        .cloned()
        .expect("in-place handoff deferred report for max-rows-produced");
    let mut stream = resp.into_body().into_data_stream();
    let mut received = 0usize;
    let mut err_msg = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => received += chunk.len(),
            Err(err) => {
                err_msg = Some(err.to_string());
                break;
            }
        }
    }
    assert!(
        received > 0,
        "handoff must deliver prefix bytes before rows-produced abort"
    );
    let err_msg = err_msg.expect("handoff must abort stream on post-commit rows-produced budget");
    assert!(
        err_msg.contains("operators produced") && err_msg.contains("limit 9000"),
        "expected post-handoff rows-produced error, got: {err_msg}"
    );
    assert_eq!(
        deferred_prod.snapshot_outcome(),
        Some((Some(Outcome::Budget), Some(BudgetKind::RowsProduced))),
        "in-place hand-off deferred report must record Budget(RowsProduced)"
    );

    let req = Request::post("/rb_ds/sparql?execution=streaming&max-rows-produced=9000")
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "text/tab-separated-values")
        .body(Body::from(q_mid_prod))
        .unwrap();
    let resp = s.app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "streaming must commit headers before hitting 9000 rows produced"
    );
    let mut stream = resp.into_body().into_data_stream();
    let mut received = 0usize;
    let mut err_msg = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => received += chunk.len(),
            Err(err) => {
                err_msg = Some(err.to_string());
                break;
            }
        }
    }
    assert!(
        received > 0,
        "streaming must deliver prefix bytes before rows-produced abort"
    );
    let err_msg = err_msg.expect("streaming must abort stream on post-commit rows-produced budget");
    assert!(
        err_msg.contains("operators produced") && err_msg.contains("limit 9000"),
        "expected streaming rows-produced error, got: {err_msg}"
    );

    let m = wait_metric(
        &s.app,
        r#"sparkles_requests_total{dataset="rb_ds",operation="query",outcome="budget"}"#,
        4.0,
    )
    .await;
    assert_eq!(
        metric_sample(
            &m,
            r#"sparkles_budget_exceeded_total{dataset="rb_ds",budget="rows"}"#
        ),
        Some(2.0),
        "expected 2 routed rows-budget failures:\n{m}"
    );
    assert_eq!(
        metric_sample(
            &m,
            r#"sparkles_budget_exceeded_total{dataset="rb_ds",budget="rows-produced"}"#
        ),
        Some(2.0),
        "expected 2 routed rows-produced-budget failures:\n{m}"
    );
    assert!(wait_pool_zero(&s.state, Duration::from_secs(2)).await);
}

/// Dropping a client connection mid-stream after an in-place `execution=auto` cursor hands
/// off to the blocking pool cancels the live producer, records `outcome="cancelled"`, and
/// releases both memory and concurrency.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_disconnect_after_in_place_handoff_releases_resources() {
    use crate::http::algo::probe::{CURRENT, Gate, Probe, Release};

    let s = pool_server(None, Some("query=concurrency=1"));
    let payload = "d".repeat(800);
    let mut data = String::new();
    // 5,000 rows (~4.1 MiB, spanning two cursor batches) so substantial cursor work remains
    // un-pulled after hand-off.
    for i in 0..5000 {
        data.push_str(&format!(
            "<urn:dch:s{i:04}> <urn:dch:p> \"{payload}-{i}\" .\n"
        ));
    }
    load_turtle(&s.state, "dch_ds", data);

    let q = "SELECT ?s ?o WHERE { ?s <urn:dch:p> ?o }";
    let key = QuickQueries::key("dch_ds", q);
    let gate = Arc::new(Gate::default());
    let _release = Release(gate.clone());
    let probe = Probe {
        queued_producer: Some(gate.clone()),
        ..Default::default()
    };
    let req = Request::post("/dch_ds/sparql?execution=auto")
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "text/tab-separated-values")
        .body(Body::from(q))
        .unwrap();

    let entered = gate.entered.notified();
    tokio::pin!(entered);
    entered.as_mut().enable();
    let resp = CURRENT
        .scope(probe, s.app.clone().oneshot(req))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(route_stats::in_place_handoff(key), 1);
    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .unwrap();

    let mut stream = resp.into_body().into_data_stream();
    let first = stream.next().await.expect("first chunk").expect("ok chunk");
    assert!(!first.is_empty());

    // Prove the producer is still live mid-stream and holding both its cursor memory grant
    // and the HTTP concurrency permit before simulating client disconnect.
    assert!(
        !gate.finished(),
        "producer must still be paused mid-stream before disconnect"
    );
    assert!(
        s.state.catalog.pool().unwrap().granted() > 0,
        "cursor memory grant must be held while producer is paused mid-stream"
    );
    let (busy_status, _, _) = post_query(
        &s.app,
        "/dch_ds/sparql?execution=auto",
        "ASK { ?s <urn:dch:p> ?o }",
        "application/sparql-results+json",
    )
    .await;
    assert_eq!(
        busy_status,
        StatusCode::SERVICE_UNAVAILABLE,
        "HTTP concurrency permit must be held while producer is paused mid-stream"
    );

    let done = gate.done.notified();
    tokio::pin!(done);
    done.as_mut().enable();
    drop(stream);
    gate.release();
    tokio::time::timeout(Duration::from_secs(5), done)
        .await
        .unwrap();

    let m = wait_metric(
        &s.app,
        r#"sparkles_requests_total{dataset="dch_ds",operation="query",outcome="cancelled"}"#,
        1.0,
    )
    .await;
    assert_eq!(
        metric_sample(
            &m,
            r#"sparkles_requests_total{dataset="dch_ds",operation="query",outcome="ok"}"#
        )
        .unwrap_or(0.0),
        0.0,
        "cancelled hand-off stream must not record ok:\n{m}"
    );
    assert!(
        wait_pool_zero(&s.state, Duration::from_secs(5)).await,
        "disconnect after in-place hand-off leaked memory grant"
    );
    let (st, _, _) = post_query(
        &s.app,
        "/dch_ds/sparql?execution=auto",
        "ASK { ?s <urn:dch:p> ?o }",
        "application/sparql-results+json",
    )
    .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "disconnect after in-place hand-off did not release concurrency slot"
    );
}

/// A query timeout that expires after an in-place cursor hands off to the blocking pool
/// aborts the committed stream, records `outcome="timeout"`, and releases both memory and
/// the HTTP concurrency permit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mid_stream_timeout_after_handoff_aborts_and_releases_resources() {
    use crate::http::algo::probe::{CURRENT, Gate, Probe, Release};
    use std::sync::atomic::{AtomicU64, Ordering};

    let s = pool_server(None, Some("query=concurrency=1"));
    let payload = "t".repeat(400);
    let mut data = String::new();
    for i in 0..2000 {
        data.push_str(&format!(
            "<urn:to:s{i:04}> <urn:to:p> \"{payload}-{i}\" .\n"
        ));
    }
    load_turtle(&s.state, "to_ds", data);

    let q = "SELECT ?s ?o WHERE { ?s <urn:to:p> ?o }";
    let key = QuickQueries::key("to_ds", q);
    let gate = Arc::new(Gate::default());
    let _release = Release(gate.clone());
    let clock_advance_ns = Arc::new(AtomicU64::new(0));
    let probe = Probe {
        queued_producer: Some(gate.clone()),
        clock_advance_ns: Some(clock_advance_ns.clone()),
        ..Default::default()
    };
    // Give the pre-commit phase a 30 s deadline so it can never expire before hand-off,
    // then advance the producer's deadline clock deterministically once paused mid-stream.
    let req = Request::post("/to_ds/sparql?execution=auto&timeout=30")
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "text/tab-separated-values")
        .body(Body::from(q))
        .unwrap();

    let entered = gate.entered.notified();
    tokio::pin!(entered);
    entered.as_mut().enable();
    let resp = CURRENT
        .scope(probe, s.app.clone().oneshot(req))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(route_stats::in_place_handoff(key), 1);
    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .unwrap();

    let mut stream = resp.into_body().into_data_stream();
    let first = stream.next().await.expect("first chunk").expect("ok chunk");
    assert!(!first.is_empty());
    assert!(!gate.finished());
    assert!(s.state.catalog.pool().unwrap().granted() > 0);

    // Advance the test clock past the 30 s deadline while the producer is paused after its
    // initial send, then release it so the next post-handoff check trips the timeout.
    let done = gate.done.notified();
    tokio::pin!(done);
    done.as_mut().enable();
    clock_advance_ns.store(Duration::from_secs(60).as_nanos() as u64, Ordering::Relaxed);
    gate.release();

    let mut err_msg = None;
    while let Some(item) = stream.next().await {
        if let Err(err) = item {
            err_msg = Some(err.to_string());
            break;
        }
    }
    let err_msg = err_msg.expect("mid-stream timeout after hand-off must abort stream");
    assert!(
        err_msg.contains("timed out"),
        "expected mid-stream timeout error, got: {err_msg}"
    );
    tokio::time::timeout(Duration::from_secs(5), done)
        .await
        .unwrap();

    let _ = wait_metric(
        &s.app,
        r#"sparkles_requests_total{dataset="to_ds",operation="query",outcome="timeout"}"#,
        1.0,
    )
    .await;
    assert!(wait_pool_zero(&s.state, Duration::from_secs(5)).await);

    let (st, _, _) = post_query(
        &s.app,
        "/to_ds/sparql?execution=auto",
        "ASK { ?s <urn:to:p> ?o }",
        "application/sparql-results+json",
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}
