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
    assert!(producer_finished(&first.gate).await);
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
    assert!(producer_finished(&gate).await);
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

async fn producer_finished(gate: &super::algo::probe::Gate) -> bool {
    // A prior waiter can have consumed Notify's permit; finished is authoritative.
    let notified = gate.done.notified();
    if !gate.finished()
        && tokio::time::timeout(Duration::from_secs(5), notified)
            .await
            .is_err()
    {
        return false;
    }
    gate.finished()
}

async fn transfer_cleanup(s: &Server, completion: &super::algo::probe::Gate) -> bool {
    // Pool credit can return before the real producer releases HTTP admission.
    if !producer_finished(completion).await {
        return false;
    }
    grant_released(s, Duration::from_secs(5)).await
}

async fn require_stream_response(
    response: axum::response::Response,
    uri: &str,
    accept: &str,
    encoding: &str,
) -> axum::response::Response {
    if response.status() == StatusCode::OK {
        return response;
    }
    // A refusal before a producer exists cannot wait on its completion observer.
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 64 << 10).await;
    panic!("{uri} {accept} {encoding}: status={status}, headers={headers:?}, body={body:?}");
}

async fn unread_transfer_fails(s: &Server, uri: &str, accept: &str, encoding: &str) {
    let completion = Arc::new(super::algo::probe::Gate::default());
    let probe = super::algo::probe::Probe {
        completion: Some(completion.clone()),
        ..Default::default()
    };
    let response = super::algo::probe::CURRENT
        .scope(
            probe,
            s.app.clone().oneshot(
                Request::get(uri)
                    .header("accept", accept)
                    .header("accept-encoding", encoding)
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
    let response = require_stream_response(response, uri, accept, encoding).await;
    let abort = response.extensions().get::<super::StreamAbort>().cloned();
    let mut held = HeldBody {
        response: Some(response),
        gate: completion.clone(),
        release: Some(super::algo::probe::Release(completion.clone())),
    };
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
            axum::body::to_bytes(held.response.take().unwrap().into_body(), usize::MAX)
        )
        .await,
        Ok(Err(_))
    );
    let released = transfer_cleanup(s, &completion).await;
    assert!(
        aborted,
        "{uri} {accept} {encoding}: unread application body did not independently abort"
    );
    assert!(
        failed,
        "{uri} {accept} {encoding}: failed stream must not have clean or compressed EOF"
    );
    assert!(
        released,
        "{uri} {accept} {encoding}: transfer cleanup did not finish"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cleanup_waits_for_the_real_producer_not_only_its_released_pool_grant() {
    let s = server();
    let after_write = Arc::new(super::algo::probe::Gate::default());
    let completion = Arc::new(super::algo::probe::Gate::default());
    let probe = super::algo::probe::Probe {
        completion: Some(completion.clone()),
        after_write: Some(after_write.clone()),
        ..Default::default()
    };
    let uri = "/ds/sparql?query=SELECT%20%3Fo%20WHERE%20%7B%3Curn%3Alarge%3E%20%3Curn%3Ap%3E%20%3Fo%7D&execution=streaming&format=tsv&timeout=30";
    let response = super::algo::probe::CURRENT
        .scope(
            probe,
            s.app
                .clone()
                .oneshot(Request::get(uri).body(Body::empty()).unwrap()),
        )
        .await
        .unwrap();
    let response = require_stream_response(response, uri, "*/*", "identity").await;
    let abort = response
        .extensions()
        .get::<super::StreamAbort>()
        .unwrap()
        .clone();
    let mut held = HeldBody {
        response: Some(response),
        gate: after_write.clone(),
        release: Some(super::algo::probe::Release(after_write.clone())),
    };
    tokio::time::timeout(Duration::from_secs(5), after_write.entered.notified())
        .await
        .unwrap();
    assert!(
        abort.is_aborted(),
        "the real unread-body idle timer must have fired"
    );
    assert_eq!(s.state.catalog.pool().unwrap().granted(), 0);
    assert!(!completion.finished(), "the real producer is still gated");
    let busy = s
        .app
        .clone()
        .oneshot(
            Request::get("/ds/sparql?query=ASK%7B%7D")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let busy_status = busy.status();
    let busy_headers = busy.headers().clone();
    let busy_body = axum::body::to_bytes(busy.into_body(), 64 << 10)
        .await
        .unwrap();
    let busy_json: serde_json::Value = serde_json::from_slice(&busy_body).unwrap();
    assert_eq!(
        busy_status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{busy_headers:?} {busy_body:?}"
    );
    assert_eq!(busy_json["reason"], "concurrency");
    assert_eq!(busy_json["limitClass"], "query");
    eprintln!(
        "post-write pool_granted={} producer_finished={} busy_status={busy_status} headers={busy_headers:?} body={busy_body:?}",
        s.state.catalog.pool().unwrap().granted(),
        completion.finished()
    );
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            axum::body::to_bytes(held.response.take().unwrap().into_body(), usize::MAX)
        )
        .await,
        Ok(Err(_))
    ));
    let premature_cleanup = {
        let mut cleanup = Box::pin(transfer_cleanup(&s, &completion));
        match futures_util::poll!(cleanup.as_mut()) {
            std::task::Poll::Ready(released) => {
                assert!(released);
                true
            }
            std::task::Poll::Pending => false,
        }
    };
    // Drop the polled future before using the completion observer again.
    after_write.release();
    let finished = producer_finished(&completion).await;
    drop(held);
    let after = second_status(&s).await;
    assert!(finished);
    assert_eq!(after, StatusCode::OK);
    assert!(
        !after_write
            .timed_out
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    eprintln!(
        "cleanup_returned_before_release={premature_cleanup} producer_finished_after_release={finished} final_ask_status={after}"
    );
    assert!(
        !premature_cleanup,
        "pool-only cleanup returned before real producer completion"
    );
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
