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
