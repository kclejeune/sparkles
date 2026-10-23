//! The limit of inferred triples over HTTP: the server's `--reason-max-inferred` bounds
//! every run, and a request's `maxInferred` may only lower it.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

/// A transitive chain of 5 links, whose OWL 2 RL closure has 6 more `ex:p` triples.
const CHAIN: &str = "PREFIX ex: <http://ex.org/> PREFIX owl: <http://www.w3.org/2002/07/owl#>
INSERT DATA { ex:p a owl:TransitiveProperty .
  ex:a0 ex:p ex:a1 . ex:a1 ex:p ex:a2 . ex:a2 ex:p ex:a3 . ex:a3 ex:p ex:a4 }";

fn server(limit: usize) -> (tempfile::TempDir, Arc<AppState>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.reason_max_inferred = limit;
    let st = Arc::new(st);
    st.create("r", DbType::Persistent).unwrap();
    let app = router(st.clone());
    (dir, st, app)
}

async fn post(app: &Router, uri: &str, ct: &str, body: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// Start a run and wait for its task to end.
async fn reason(st: &AppState, app: &Router, uri: &str, body: &str) -> crate::state::Task {
    let before = st.tasks.lock().len();
    let ct = if body.starts_with('{') {
        "application/json"
    } else {
        "application/x-www-form-urlencoded"
    };
    let (status, text) = post(app, uri, ct, body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    let t0 = std::time::Instant::now();
    loop {
        let t = {
            let tasks = st.tasks.lock();
            assert!(tasks.len() > before);
            tasks.last().cloned().unwrap()
        };
        if !t.active() {
            return t;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "task did not finish"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_server_limit_bounds_runs_and_requests_may_lower_it() {
    let (_d, st, app) = server(1000);
    let (status, text) = post(&app, "/r/update", "application/sparql-update", CHAIN).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    // a request above the server's limit is refused before any work
    let (status, text) = post(
        &app,
        "/$/reason/r",
        "application/json",
        r#"{"profile":"owl-rl","maxInferred":1001}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert!(text.contains("--reason-max-inferred"), "{text}");
    for bad in [r#"{"maxInferred":0}"#, r#"{"maxInferred":"9"}"#] {
        let (status, text) = post(&app, "/$/reason/r", "application/json", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {text}");
    }
    let (status, text) = post(&app, "/$/reason/r?maxInferred=-1", "application/json", "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    // a request's lower limit fails the run with the limit and how to raise it
    let head = st.get("r").unwrap().store.snapshot().commit;
    let t = reason(
        &st,
        &app,
        "/$/reason/r",
        r#"{"profile":"owl-rl","maxInferred":3}"#,
    )
    .await;
    assert_eq!(t.state, "failed", "{t:?}");
    let msg = t.message.clone().unwrap_or_default();
    assert!(
        msg.contains("exceeded the limit of 3 inferred triples") && msg.contains("maxInferred"),
        "{msg}"
    );
    let detail = t.detail.clone().unwrap_or_default();
    assert_eq!(detail["code"], "inferred-limit", "{detail}");
    assert_eq!(detail["maxInferred"], 3, "{detail}");
    assert_eq!(st.get("r").unwrap().store.snapshot().commit, head);
    // a form field and a query parameter set it as well
    let t = reason(&st, &app, "/$/reason/r", "profile=owl-rl&maxInferred=2").await;
    assert_eq!(t.state, "failed", "{t:?}");
    assert!(
        t.message
            .unwrap_or_default()
            .contains("limit of 2 inferred"),
        "the form field applies"
    );
    let t = reason(&st, &app, "/$/reason/r?maxInferred=1", "profile=owl-rl").await;
    assert_eq!(t.state, "failed", "{t:?}");
    assert!(
        t.message
            .unwrap_or_default()
            .contains("limit of 1 inferred"),
        "the query parameter applies"
    );
    // within the server's limit the run succeeds
    let t = reason(&st, &app, "/$/reason/r", r#"{"profile":"owl-rl"}"#).await;
    assert_eq!(t.state, "done", "{t:?}");
}

#[tokio::test]
async fn the_server_limit_fails_a_run_without_a_request_limit() {
    let (_d, st, app) = server(4);
    let (status, text) = post(&app, "/r/update", "application/sparql-update", CHAIN).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let t = reason(&st, &app, "/$/reason/r", r#"{"profile":"owl-rl"}"#).await;
    assert_eq!(t.state, "failed", "{t:?}");
    assert!(
        t.message
            .unwrap_or_default()
            .contains("exceeded the limit of 4 inferred triples"),
        "the server's limit applies"
    );
    assert_eq!(t.detail.unwrap_or_default()["maxInferred"], 4);
}

#[test]
fn the_server_default_is_the_reasoner_default() {
    assert_eq!(
        crate::state::DEFAULT_REASON_MAX_INFERRED,
        sparkles_reasoner::DEFAULT_MAX_INFERRED
    );
}
