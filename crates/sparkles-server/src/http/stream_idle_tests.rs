//! An unread body must end independently of its query's computation timeout.
use super::router;
use crate::obs::Phase;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

struct Server {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
        Some(64 << 20),
    )
    .unwrap();
    state.stream_idle_timeout = Duration::from_millis(20);
    assert!(state.stream_transfer_timeout.is_none());
    let mut config = crate::ratelimit::Config::default();
    config.apply_flag("query=concurrency=1").unwrap();
    state.rate_limit = Some(Arc::new(
        crate::ratelimit::RateLimiter::new(&config).unwrap(),
    ));
    let state = Arc::new(state);
    let ds = state.attach("ds", DbType::Mem, None).unwrap();
    let payload = "x".repeat(3 << 20);
    ds.store
        .load(&[Source::from_bytes(
            format!("<urn:large> <urn:p> \"{payload}\" .\n").into_bytes(),
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

struct HeldBody {
    response: Option<axum::response::Response>,
    gate: Arc<super::algo::probe::Gate>,
    release: Option<super::algo::probe::Release>,
}

impl Drop for HeldBody {
    fn drop(&mut self) {
        drop(self.response.take());
        // These tests use two runtime workers so the owned timer can join even if
        // an assertion unwinds through this synchronous producer cleanup guard.
        drop(self.release.take());
    }
}

async fn response(s: &Server) -> HeldBody {
    let gate = Arc::new(super::algo::probe::Gate::default());
    let release = super::algo::probe::Release(gate.clone());
    let probe = super::algo::probe::Probe {
        producer: Some(gate.clone()),
        ..Default::default()
    };
    let response = super::algo::probe::CURRENT.scope(probe, s.app.clone().oneshot(
        Request::get("/ds/sparql?query=SELECT%20%3Fo%20WHERE%20%7B%3Curn%3Alarge%3E%20%3Curn%3Ap%3E%20%3Fo%7D&execution=streaming&format=tsv&timeout=30")
            .body(Body::empty()).unwrap(),
    )).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    HeldBody {
        response: Some(response),
        gate,
        release: Some(release),
    }
}

async fn grant_released(s: &Server, limit: Duration) -> bool {
    tokio::time::timeout(limit, async {
        while s.state.catalog.pool().unwrap().granted() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok()
}

async fn second_status(s: &Server) -> StatusCode {
    s.app
        .clone()
        .oneshot(
            Request::get("/ds/sparql?query=ASK%7B%7D")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_idle_releases_query_grant_without_polling_body() {
    let s = server();
    let first = response(&s).await;
    assert_eq!(first.response.as_ref().unwrap().status(), StatusCode::OK);
    assert!(s.state.catalog.pool().unwrap().granted() > 0);
    first.gate.release();
    let released_while_body_held = grant_released(&s, Duration::from_secs(1)).await;
    // Always close the old/no-op producer before asserting the intended red.
    drop(first);
    assert!(
        grant_released(&s, Duration::from_secs(5)).await,
        "owned producer did not stop after its body dropped"
    );
    assert!(
        released_while_body_held,
        "independent idle limit left the unread producer's query grant live"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_idle_releases_query_slot_without_polling_body() {
    let s = server();
    let first = response(&s).await;
    assert_eq!(first.response.as_ref().unwrap().status(), StatusCode::OK);
    let refused_while_live = second_status(&s).await;
    first.gate.release();
    let _ = grant_released(&s, Duration::from_secs(1)).await;
    let after_idle = second_status(&s).await;
    // Preserve real cleanup even on the baseline's expected missing timer/lease behavior.
    drop(first);
    assert!(
        grant_released(&s, Duration::from_secs(5)).await,
        "owned producer did not stop after its body dropped"
    );
    assert_eq!(refused_while_live, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        after_idle,
        StatusCode::OK,
        "idle-aborted body retained its HTTP query concurrency slot"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logical_abort_retains_real_busy_producer_admission_until_quiescence() {
    let s = server();
    let gate = Arc::new(super::algo::probe::Gate::default());
    let release = super::algo::probe::Release(gate.clone());
    let probe = super::algo::probe::Probe {
        queued_producer: Some(gate.clone()),
        ..Default::default()
    };
    let response = super::algo::probe::CURRENT.scope(probe, s.app.clone().oneshot(
        Request::get("/ds/sparql?query=SELECT%20%3Fo%20WHERE%20%7B%3Curn%3Alarge%3E%20%3Curn%3Ap%3E%20%3Fo%7D&execution=streaming&format=tsv&timeout=30")
            .body(Body::empty()).unwrap(),
    )).await.unwrap();
    let first = HeldBody {
        response: Some(response),
        gate: gate.clone(),
        release: Some(release),
    };
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    let abort = first
        .response
        .as_ref()
        .unwrap()
        .extensions()
        .get::<super::StreamAbort>()
        .unwrap()
        .clone();
    let independently_aborted = tokio::time::timeout(Duration::from_secs(2), async {
        while !abort.is_aborted() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    let busy_grant = s.state.catalog.pool().unwrap().granted();
    let busy_status = second_status(&s).await;
    gate.release();
    let released = grant_released(&s, Duration::from_secs(2)).await;
    let after = second_status(&s).await;
    drop(first);
    assert!(
        independently_aborted,
        "timer must run independently of the gated producer and body polling"
    );
    assert!(
        busy_grant > 0,
        "logical abort must retain the actual busy engine's grant"
    );
    assert_eq!(busy_status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(released);
    assert_eq!(after, StatusCode::OK);
    assert!(!gate.timed_out.load(std::sync::atomic::Ordering::Relaxed));
}

async fn unread_transfer_fails(s: &Server, uri: &str, accept: &str, encoding: &str) {
    let response = s
        .app
        .clone()
        .oneshot(
            Request::get(uri)
                .header("accept", accept)
                .header("accept-encoding", encoding)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let abort = response.extensions().get::<super::StreamAbort>().cloned();
    let aborted = match abort {
        Some(abort) => tokio::time::timeout(Duration::from_secs(2), async {
            while !abort.is_aborted() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok(),
        None => false,
    };
    let failed = matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            axum::body::to_bytes(response.into_body(), usize::MAX)
        )
        .await,
        Ok(Err(_))
    );
    let released = grant_released(s, Duration::from_secs(5)).await;
    assert_eq!(status, StatusCode::OK, "{uri}");
    assert!(
        aborted,
        "{uri} {accept} {encoding}: unread application body did not independently abort"
    );
    assert!(
        failed,
        "{uri} {accept} {encoding}: failed stream must not have clean or compressed EOF"
    );
    assert!(released);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_query_mode_and_select_format_aborts_unread_plain_and_gzip_body() {
    let s = server();
    for encoding in ["identity", "gzip"] {
        for mode in ["eager", "streaming", "auto"] {
            for format in ["json", "xml", "csv", "tsv", "sparkles"] {
                let uri = format!(
                    "/ds/sparql?query=SELECT%20%3Fo%20WHERE%20%7B%3Curn%3Alarge%3E%20%3Curn%3Ap%3E%20%3Fo%7D&execution={mode}&format={format}&timeout=30"
                );
                unread_transfer_fails(&s, &uri, "*/*", encoding).await;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finite_graph_diff_and_changes_aborts_unread_plain_and_gzip_body() {
    let s = server();
    for encoding in ["identity", "gzip"] {
        for (uri, accept) in [
            ("/ds/data?default", "application/n-triples"),
            ("/ds/diff?from=0&quads=true", "application/json"),
            ("/ds/changes?after=0", "application/json"),
        ] {
            unread_transfer_fails(&s, uri, accept, encoding).await;
        }
    }
}
