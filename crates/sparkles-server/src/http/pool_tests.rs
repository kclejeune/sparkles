//! Admission to the query memory pool (spec X08 §4): queueing, `503` with `Retry-After`,
//! grant release, `/$/server/memory` and the metrics.

use super::router;
use crate::obs::Phase;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::Value as J;
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const MB: u64 = 1 << 20;
const Q: &str = "/ds/sparql?query=SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D";

struct Server {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

fn server(pool_mb: u64, wait: Duration) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
        Some(pool_mb * MB),
    )
    .unwrap();
    state.admission.wait = wait;
    let state = Arc::new(state);
    let ds = state.attach("ds", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            b"<urn:a> <urn:p> <urn:b> .\n".to_vec(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    state.set_phase(Phase::Ready);
    let app = router(state.clone());
    Server {
        _dir: dir,
        state,
        app,
    }
}

async fn get(app: &Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let res = app
        .clone()
        .oneshot(
            Request::get(uri)
                .header(header::ACCEPT, "application/x-sparkles+json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

fn pool(s: &Server) -> &Arc<sparkles::sparql::pool::QueryPool> {
    s.state.catalog.pool().expect("a pool")
}

#[tokio::test]
async fn a_full_pool_answers_503_after_the_wait_and_serves_again_when_it_frees() {
    let s = server(64, Duration::from_millis(200));
    // other queries hold 56 of the 64 MiB, so the 16 MiB a request starts with is short
    let held = pool(&s).try_grant(56 * MB).unwrap();
    let started = std::time::Instant::now();
    let (status, headers, body) = get(&s.app, Q).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert_eq!(headers[header::RETRY_AFTER], "1");
    let j: J = serde_json::from_str(&body).unwrap();
    assert_eq!(j["budget"], "server-memory");
    assert_eq!(pool(&s).stats().rejected, 1);
    assert_eq!(pool(&s).waiting(), 0);
    let (_, _, m) = get(&s.app, "/$/metrics").await;
    assert!(
        m.contains("sparkles_query_admission_rejected_total 1"),
        "{m}"
    );
    assert!(m.contains("sparkles_query_pool_capacity_bytes 67108864"));
    drop(held);
    let (status, _, body) = get(&s.app, Q).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // the request's grant went back to the pool with the request
    assert_eq!(pool(&s).granted(), 0);
}

#[tokio::test]
async fn a_request_waits_for_memory_in_the_queue() {
    let s = server(64, Duration::from_secs(10));
    let held = pool(&s).try_grant(56 * MB).unwrap();
    let app = s.app.clone();
    let waiting = tokio::spawn(async move { get(&app, Q).await });
    for _ in 0..200 {
        if pool(&s).waiting() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(pool(&s).waiting(), 1);
    let (_, _, m) = get(&s.app, "/$/metrics").await;
    assert!(m.contains("sparkles_query_admission_waiting 1"), "{m}");
    let (_, _, mem) = get(&s.app, "/$/server/memory").await;
    let j: J = serde_json::from_str(&mem).unwrap();
    assert_eq!(j["pool"]["waiting"], 1);
    assert_eq!(j["pool"]["grantedBytes"], 56 * MB);
    drop(held);
    let (status, _, body) = waiting.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(pool(&s).waiting(), 0);
    assert_eq!(pool(&s).granted(), 0);
}

#[tokio::test]
async fn a_query_reports_the_memory_it_was_granted() {
    let s = server(256, Duration::from_secs(2));
    let (status, _, body) = get(&s.app, Q).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let j: J = serde_json::from_str(&body).unwrap();
    assert_eq!(j["meta"]["memory"]["grantedBytes"], 16 * MB, "{j}");
}

#[tokio::test]
async fn a_server_without_a_pool_reports_none() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.set_phase(Phase::Ready);
    let app = router(state);
    let (status, _, body) = get(&app, "/$/server/memory").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let j: J = serde_json::from_str(&body).unwrap();
    assert_eq!(j["pool"]["enabled"], false);
    assert_eq!(j["caches"]["block"]["capacityBytes"], 1u64 << 30);
    let (_, _, m) = get(&app, "/$/metrics").await;
    assert!(!m.contains("sparkles_query_pool"));
}

#[tokio::test]
async fn server_memory_lists_the_caches_per_dataset() {
    let s = server(256, Duration::from_secs(2));
    s.state.attach("other", DbType::Mem, None).unwrap();
    let (status, _, body) = get(&s.app, "/$/server/memory").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let j: J = serde_json::from_str(&body).unwrap();
    assert_eq!(j["pool"]["enabled"], true);
    assert_eq!(j["pool"]["capacityBytes"], 256 * MB);
    assert_eq!(j["caches"]["shared"], true);
    let names: Vec<_> = j["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["ds", "other"]);
}

async fn post_update(app: &Router, body: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let res = app
        .clone()
        .oneshot(
            Request::post("/ds/update")
                .header(header::CONTENT_TYPE, "application/sparql-update")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn an_update_outgrowing_its_admission_grant_answers_503() {
    let s = server(64, Duration::from_millis(200));
    let mut data = String::new();
    for i in 0..1_000 {
        data.push_str(&format!("<urn:s{i}> <urn:p> \"{i}\" .\n"));
    }
    s.state
        .get("ds")
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    const U: &str = "DELETE { ?a <urn:none> ?b } WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y }";
    // other requests hold 40 of the 64 MiB, so the update cannot grow past its 16 MiB
    let held = pool(&s).try_grant(40 * MB).unwrap();
    let (status, headers, body) = post_update(&s.app, U).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(headers[header::RETRY_AFTER], "1");
    let j: J = serde_json::from_str(&body).unwrap();
    assert_eq!(j["budget"], "server-memory", "{body}");
    assert_eq!(pool(&s).granted(), 40 * MB);
    drop(held);
    let (status, _, body) = post_update(&s.app, U).await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(pool(&s).granted(), 0);
}

// X07 metadata and X08 grants must survive the same serialization paths.
#[tokio::test]
async fn native_io_and_grant_metadata_survive_eager_streaming_and_early_stop() {
    let s = server(256, Duration::from_secs(2));
    for (mode, send, status, rows) in [
        ("eager", "", "complete", 1),
        ("streaming", "", "complete", 1),
        ("streaming", "&send=0", "stopped", 0),
    ] {
        let (code, _, body) = get(
            &s.app,
            &format!("{Q}&profile=io&format=sparkles&execution={mode}{send}"),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{body}");
        let j: J = serde_json::from_str(&body).unwrap();
        if mode == "streaming" {
            assert_eq!(j["meta"]["status"], status, "{j}");
        } else {
            // Eager metadata has no cursor status field.
            assert!(j["meta"].get("status").is_none(), "{j}");
            assert_eq!(j["meta"]["totalRows"], 1, "{j}");
        }
        assert_eq!(j["meta"]["sentRows"], rows, "{j}");
        assert_eq!(j["meta"]["memory"]["grantedBytes"], 16 * MB, "{j}");
        assert!(j["meta"]["io"].is_object(), "{j}");
        assert!(j["meta"]["io"]["pagesTruncated"].is_boolean(), "{j}");
        assert!(j["meta"]["io"]["minorFaults"].is_number(), "{j}");
        assert_eq!(pool(&s).granted(), 0);
    }
}

#[tokio::test]
async fn dropping_profiled_streaming_body_releases_its_query_grant() {
    let s = server(256, Duration::from_secs(2));
    // A large literal makes one row exceed the serialization threshold. The producer
    // cannot finish through the bounded channel while this response remains unpolled.
    let payload = "x".repeat(super::stream::STREAM_AFTER + 16 * super::stream::CHUNK);
    s.state
        .get("ds")
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            format!("<urn:large> <urn:p> \"{payload}\" .\n").into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let response = s
        .app
        .clone()
        .oneshot(
            Request::get(format!(
                "{Q}&profile=io&format=sparkles&execution=streaming"
            ))
            .header(header::ACCEPT, "application/x-sparkles+json")
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        pool(&s).granted() >= 16 * MB,
        "unconsumed producer retains its grant"
    );
    drop(response);
    tokio::time::timeout(Duration::from_secs(5), async {
        while pool(&s).granted() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled serializer releases its grant");
}

#[tokio::test]
async fn dropping_a_default_streamed_body_releases_its_query_grant() {
    let s = server(256, Duration::from_secs(2));
    // a million-row scan is what automatic execution streams, and the bounded channel
    // holds the producer while this response remains unpolled
    let mut data = String::with_capacity(32 << 20);
    for i in 0..1_000_000 {
        data.push_str(&format!("<urn:s{i}> <urn:q> {i} .\n"));
    }
    s.state
        .get("ds")
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let response = s
        .app
        .clone()
        .oneshot(
            Request::get("/ds/sparql?query=SELECT%20%3Fs%20%3Fo%20WHERE%20%7B%3Fs%20%3Curn%3Aq%3E%20%3Fo%7D&format=tsv")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        pool(&s).granted() > 0,
        "a default stream that nobody reads retains its grant"
    );
    drop(response);
    tokio::time::timeout(Duration::from_secs(5), async {
        while pool(&s).granted() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled serializer releases its grant");
}

async fn algorithm(app: &Router, body: &str) -> (StatusCode, String) {
    let res = app
        .clone()
        .oneshot(
            Request::post("/$/algo/ds/run")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/sparql-results+json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn wait_task(st: &AppState, id: &str) -> crate::state::Task {
    let t0 = std::time::Instant::now();
    loop {
        let t = st
            .tasks
            .lock()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .unwrap();
        if t.state != "running" {
            return t;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "task {id} did not finish"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn task_id(body: &str) -> String {
    serde_json::from_str::<J>(body).unwrap()["taskId"]
        .as_str()
        .unwrap()
        .to_string()
}

/// An algorithm run selects its class from its body, so the admission layer cannot
/// classify it by route. A read run waits for its initial grant as a query does, and a
/// write-back run takes its grant when its task starts.
#[tokio::test]
async fn algorithm_runs_wait_for_the_memory_pool() {
    const READ: &str = r#"{"algorithm":"wcc","projection":{"predicates":["urn:p"]}}"#;
    const WRITE: &str = r#"{"algorithm":"wcc","projection":{"predicates":["urn:p"]},"write":{"graph":"urn:out","predicates":{"component":"urn:component"}}}"#;
    let s = server(64, Duration::from_millis(200));
    let held = pool(&s).try_grant(56 * MB).unwrap();
    let (status, body) = algorithm(&s.app, READ).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let j: J = serde_json::from_str(&body).unwrap();
    assert_eq!(j["budget"], "server-memory");
    assert_eq!(pool(&s).stats().rejected, 1);
    let (status, body) = algorithm(&s.app, WRITE).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let task = wait_task(&s.state, &task_id(&body)).await;
    assert_eq!(task.state, "failed", "{:?}", task.message);
    assert_eq!(pool(&s).stats().rejected, 2);
    drop(held);
    let (status, body) = algorithm(&s.app, READ).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = algorithm(&s.app, WRITE).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let task = wait_task(&s.state, &task_id(&body)).await;
    assert_eq!(task.state, "done", "{:?}", task.message);
    assert_eq!(pool(&s).granted(), 0);
}

#[tokio::test]
async fn a_query_is_admitted_after_its_body_arrives() {
    let s = server(64, Duration::from_secs(10));
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(4);
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|chunk| (chunk, rx))
    });
    let app = s.app.clone();
    let sent = tokio::spawn(async move {
        let res = app
            .oneshot(
                Request::post("/ds/sparql")
                    .header(header::CONTENT_TYPE, "application/sparql-query")
                    .header(header::ACCEPT, "application/x-sparkles+json")
                    .body(Body::from_stream(stream))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    });
    tx.send(Ok("SELECT * WHERE ".into())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    // a client that is still sending holds no pool memory and no place in the queue
    assert_eq!(pool(&s).granted(), 0);
    assert_eq!(pool(&s).waiting(), 0);
    assert!(!sent.is_finished());
    tx.send(Ok("{ ?s ?p ?o }".into())).await.unwrap();
    drop(tx);
    let (status, body) = sent.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("urn:a"), "{body}");
    assert_eq!(pool(&s).granted(), 0);
}

#[tokio::test]
async fn uploads_and_graph_store_writes_take_no_initial_grant() {
    let s = server(64, Duration::from_millis(200));
    // the whole pool is held, so a request that asks for an initial grant would answer 503
    let held = pool(&s).try_grant(64 * MB).unwrap();
    let send = |method: &str, uri: &str, ct: &str| {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, ct)
            .body(Body::from("<urn:c> <urn:p> <urn:d> .\n"))
            .unwrap();
        let app = s.app.clone();
        async move {
            let res = app.oneshot(req).await.unwrap();
            let status = res.status();
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, String::from_utf8_lossy(&body).into_owned())
        }
    };
    for (method, uri) in [
        ("PUT", "/ds/data?graph=urn:g"),
        ("POST", "/ds/data?default"),
        ("POST", "/ds"),
    ] {
        let (status, body) = send(method, uri, "application/n-triples").await;
        assert!(status.is_success(), "{method} {uri}: {status} {body}");
    }
    assert_eq!(pool(&s).stats().rejected, 0);
    // a SPARQL update still asks for its initial grant and waits for it
    let (status, _, body) = post_update(&s.app, "INSERT DATA { <urn:e> <urn:p> <urn:f> }").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    drop(held);
    assert_eq!(pool(&s).granted(), 0);
}

#[tokio::test]
async fn a_body_read_ahead_is_not_copied_again() {
    // admission hands a body that ended within its limit on as one frame, which the
    // body extractor keeps instead of copying, so the body is not held twice
    let bytes = axum::body::Bytes::from(vec![b'x'; 1 << 20]);
    let at = bytes.as_ptr();
    let read = super::read_body(Body::from(bytes), Some(2 << 20), "--max-query-body-mb")
        .await
        .ok()
        .unwrap();
    assert_eq!(read.as_ptr(), at);
    assert_eq!(read.len(), 1 << 20);
    // a body of several frames is still joined, and its limit still holds
    let frames = futures_util::stream::iter(
        ["ab", "cd", "ef"].map(|s| Ok::<_, std::io::Error>(axum::body::Bytes::from(s))),
    );
    let read = super::read_body(Body::from_stream(frames), Some(6), "x")
        .await
        .ok()
        .unwrap();
    assert_eq!(&read[..], b"abcdef");
    let frames = futures_util::stream::iter(
        ["ab", "cd", "ef"].map(|s| Ok::<_, std::io::Error>(axum::body::Bytes::from(s))),
    );
    assert!(
        super::read_body(Body::from_stream(frames), Some(5), "x")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_query_larger_than_the_whole_pool_answers_507_without_retry_after() {
    let s = server(64, Duration::from_millis(200));
    let ds = s.state.get("ds").unwrap();
    let nt: String = (0..3000)
        .map(|i| format!("<urn:s{i}> <urn:q> <urn:o{i}> .\n"))
        .collect();
    ds.store
        .load(&[Source::from_bytes(
            nt.into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    // an eager product of 9 million rows needs far more than the 64 MiB pool
    let q = "SELECT * WHERE { ?a <urn:q> ?b . ?c <urn:q> ?d }";
    let uri = format!(
        "/ds/sparql?execution=eager&query={}",
        percent_encoding::utf8_percent_encode(q, percent_encoding::NON_ALPHANUMERIC)
    );
    let (status, headers, body) = get(&s.app, &uri).await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE, "{body}");
    assert!(headers.get(header::RETRY_AFTER).is_none(), "{headers:?}");
    let j: J = serde_json::from_str(&body).unwrap();
    assert_eq!(j["budget"], "server-memory");
    assert!(
        j["error"]
            .as_str()
            .unwrap()
            .contains("more than the server's whole query memory pool"),
        "{body}"
    );
    assert_eq!(pool(&s).granted(), 0);
}
