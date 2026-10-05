//! The scheduler of automatic compaction, its status, settings and metrics.

use super::*;
use crate::http::router;
use crate::state::DbType;
use axum::body::Body;
use axum::extract::Request;
use axum::http::header;
use sparkles::io::{RdfFormat, Source};
use sparkles::store::StoreOptions;
use tower::ServiceExt;

async fn send(app: &Router, method: &str, uri: &str, body: &str) -> (StatusCode, J) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let b = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let j = serde_json::from_slice(&b)
        .unwrap_or_else(|_| J::String(String::from_utf8_lossy(&b).into_owned()));
    (status, j)
}

async fn text(app: &Router, uri: &str) -> String {
    let res = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let b = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&b).into_owned()
}

/// A server whose automatic compaction fires at 100 delta quads, with nothing else.
fn server(set: impl FnOnce(&mut AppState)) -> (tempfile::TempDir, Arc<AppState>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.compaction.policy = CompactionPolicy {
        min_delta_quads: 100,
        delta_ratio: 0.0,
        idle_seconds: 0,
        max_age_seconds: 0,
        min_interval_seconds: 0,
        ..Default::default()
    };
    set(&mut st);
    let st = Arc::new(st);
    st.create("p", DbType::Persistent).unwrap();
    // the first load into an empty dataset builds its base
    insert(&st, "p", 10, "seed");
    let app = router(st.clone());
    (dir, st, app)
}

fn insert(st: &AppState, ds: &str, n: usize, tag: &str) {
    let nt: String = (0..n)
        .map(|i| format!("<urn:{tag}{i}> <urn:p> \"{tag} {i}\" .\n"))
        .collect();
    st.get(ds)
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
}

async fn wait(st: &AppState, id: &str) -> Task {
    let t0 = Instant::now();
    loop {
        let t = st
            .tasks
            .lock()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .unwrap();
        if !t.active() {
            return t;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "task {id} did not end"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn compactions(st: &AppState) -> Vec<Task> {
    st.tasks
        .lock()
        .iter()
        .filter(|t| t.kind == TASK)
        .cloned()
        .collect()
}

#[tokio::test]
async fn the_scheduler_compacts_when_due_and_not_before() {
    let (_d, st, app) = server(|_| {});
    insert(&st, "p", 99, "a");
    tick(&st, Instant::now());
    assert!(compactions(&st).is_empty());
    let (_, j) = send(&app, "GET", "/$/compaction/p", "").await;
    assert_eq!(j["state"], "idle", "{j}");
    assert_eq!(j["measures"]["deltaQuads"], 99);
    assert_eq!(j["measures"]["threshold"], 100);
    insert(&st, "p", 1, "b");
    let (_, j) = send(&app, "GET", "/$/compaction/p", "").await;
    assert_eq!(j["state"], "due", "{j}");
    assert_eq!(j["triggerKind"], "ratio");
    tick(&st, Instant::now());
    let tasks = compactions(&st);
    assert_eq!(tasks.len(), 1);
    let t = wait(&st, &tasks[0].id).await;
    assert_eq!(t.state, "done", "{t:?}");
    let msg = t.message.unwrap();
    assert!(msg.starts_with("auto: compacted to gen-0003"), "{msg}");
    assert!(msg.contains("writer lock"), "{msg}");
    let (_, j) = send(&app, "GET", "/$/compaction/p", "").await;
    assert_eq!(j["policy"]["partial"], "auto", "{j}");
    assert!(
        matches!(j["last"]["mode"].as_str(), Some("full" | "partial")),
        "{j}"
    );
    let ds = st.get("p").unwrap();
    assert!(ds.store.snapshot().delta.is_empty());
    tick(&st, Instant::now());
    let (_, j) = send(&app, "GET", "/$/compaction/p", "").await;
    assert_eq!(j["state"], "idle", "{j}");
    assert_eq!(j["last"]["automatic"], true);
    assert_eq!(j["last"]["outcome"], "done");
    assert_eq!(j["last"]["generation"], "gen-0003");
    assert!(
        j["last"]["trigger"]
            .as_str()
            .unwrap()
            .contains("reached 100")
    );
    assert_eq!(j["automaticRuns"], 1);
    // in the dataset statistics and the metrics too
    let (_, s) = send(&app, "GET", "/$/stats/p", "").await;
    assert_eq!(s["compaction"]["last"]["outcome"], "done", "{s}");
    let m = text(&app, "/$/metrics").await;
    assert!(
        m.contains("sparkles_compactions_total{dataset=\"p\",mode=\"auto\",outcome=\"done\"} 1"),
        "{m}"
    );
    assert!(m.contains("sparkles_compaction_lock_seconds_count{dataset=\"p\"} 1"));
    assert!(m.contains("sparkles_compaction_due{dataset=\"p\"} 0"));
    // nothing more to do
    tick(&st, Instant::now());
    assert_eq!(compactions(&st).len(), 1);
}

#[tokio::test]
async fn due_compactions_wait_for_what_needs_the_dataset() {
    let (_d, st, app) = server(|st| st.compaction.policy.min_interval_seconds = 3600);
    insert(&st, "p", 100, "a");
    // the first compaction has no previous one to wait for
    tick(&st, Instant::now());
    let id = compactions(&st)[0].id.clone();
    wait(&st, &id).await;
    insert(&st, "p", 100, "b");
    tick(&st, Instant::now());
    let (_, j) = send(&app, "GET", "/$/compaction/p", "").await;
    assert_eq!(j["state"], "deferred", "{j}");
    assert_eq!(j["deferred"], "min-interval");
    assert_eq!(compactions(&st).len(), 1);
    // an hour later it goes ahead, unless something else needs the dataset
    let later = Instant::now() + Duration::from_secs(3601);
    let restoring = st
        .catalog
        .reserve("p", sparkles::catalog::ReservationKind::Restore, "9")
        .unwrap();
    tick(&st, later);
    // requests to the dataset wait for the restore: ask the status directly
    let j = status_json(&st, &st.get("p").unwrap());
    assert_eq!(j["deferred"], "restore", "{j}");
    drop(restoring);
    // a load task of the dataset
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let load = st.start_task("load", "p", move |_| {
        let _ = rx.recv();
        Ok("loaded".into())
    });
    tick(&st, later);
    let (_, j) = send(&app, "GET", "/$/compaction/p", "").await;
    assert_eq!(j["deferred"], "bulk-load", "{j}");
    // no free task slot: an automatic compaction never queues
    st.task_queue.set_max(1);
    let other = st.attach("q", DbType::Mem, None).unwrap();
    drop(other);
    tx.send(()).unwrap();
    wait(&st, &load.id).await;
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let busy = st.start_task("clone", "q", move |_| {
        let _ = rx.recv();
        Ok("done".into())
    });
    tick(&st, later);
    let (_, j) = send(&app, "GET", "/$/compaction/p", "").await;
    assert_eq!(j["deferred"], "slots", "{j}");
    assert_eq!(compactions(&st).len(), 1);
    tx.send(()).unwrap();
    wait(&st, &busy.id).await;
    tick(&st, later);
    assert_eq!(compactions(&st).len(), 2);
    let t = compactions(&st)[1].clone();
    assert_eq!(wait(&st, &t.id).await.state, "done");
}

#[tokio::test]
async fn the_switches_turn_it_off() {
    let (_d, st, app) = server(|st| st.compaction.enabled = false);
    insert(&st, "p", 200, "a");
    tick(&st, Instant::now());
    assert!(compactions(&st).is_empty());
    let (_, j) = send(&app, "GET", "/$/compaction/p", "").await;
    assert_eq!(
        (j["state"].as_str(), j["serverEnabled"].as_bool()),
        (Some("off"), Some(false))
    );
    // the dataset's own switch
    let (_d, st, app) = server(|_| {});
    insert(&st, "p", 200, "a");
    let (s, j) = send(&app, "PUT", "/$/compaction/p", r#"{"enabled": false}"#).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["state"], "off");
    tick(&st, Instant::now());
    assert!(compactions(&st).is_empty());
    // a manual compaction still works, and is recorded as one
    let (s, t) = send(&app, "POST", "/$/compact/p", "").await;
    assert_eq!(s, StatusCode::ACCEPTED, "{t}");
    let t = wait(&st, t["id"].as_str().unwrap()).await;
    assert!(t.message.unwrap().starts_with("compacted to gen-0003"));
    let (_, j) = send(&app, "GET", "/$/compaction/p", "").await;
    assert_eq!(j["last"]["automatic"], false, "{j}");
    assert_eq!(j["automaticRuns"], 0);
    let m = text(&app, "/$/metrics").await;
    assert!(m.contains("mode=\"manual\",outcome=\"done\"} 1"), "{m}");
}

#[tokio::test]
async fn settings_are_set_per_dataset_and_kept() {
    let (dir, st, app) = server(|_| {});
    let (s, j) = send(&app, "GET", "/$/compaction/p", "").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["own"], json!({}));
    assert_eq!(j["policy"]["minDeltaQuads"], 100);
    let (s, j) = send(
        &app,
        "PUT",
        "/$/compaction/p",
        r#"{"deltaRatio": 0.02, "minDeltaQuads": 5000}"#,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["own"], json!({"deltaRatio": 0.02, "minDeltaQuads": 5000}));
    assert_eq!(j["policy"]["deltaRatio"], 0.02);
    assert_eq!(j["policy"]["minDeltaQuads"], 5000);
    // unknown settings, bad values and bad JSON are refused, and change nothing
    for body in [
        r#"{"ratio": 1}"#,
        r#"{"deltaRatio": -1}"#,
        r#"{"idleSeconds": "x"}"#,
        "[]",
        "{",
    ] {
        let (s, j) = send(&app, "PUT", "/$/compaction/p", body).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body}: {j}");
    }
    let (_, j) = send(&app, "GET", "/$/compaction/p", "").await;
    assert_eq!(j["own"]["deltaRatio"], 0.02);
    // kept across a restart
    drop(app);
    drop(st);
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let app = router(st.clone());
    let (_, j) = send(&app, "GET", "/$/compaction/p", "").await;
    assert_eq!(
        j["own"],
        json!({"deltaRatio": 0.02, "minDeltaQuads": 5000}),
        "{j}"
    );
    let (s, j) = send(&app, "DELETE", "/$/compaction/p", "").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["own"], json!({}));
    assert_eq!(j["policy"]["deltaRatio"], 0.05);
    let (s, _) = send(&app, "GET", "/$/compaction/nope", "").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_read_only_server_neither_compacts_nor_changes_settings() {
    let (_d, st, app) = server(|st| st.read_only = true);
    insert(&st, "p", 200, "a");
    tick(&st, Instant::now());
    assert!(compactions(&st).is_empty());
    let (_, j) = send(&app, "GET", "/$/compaction/p", "").await;
    assert_eq!(j["state"], "off");
    let (s, _) = send(&app, "PUT", "/$/compaction/p", r#"{"enabled": true}"#).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = send(&app, "DELETE", "/$/compaction/p", "").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_restore_cancels_a_running_compaction() {
    let (_d, st, _app) = server(|_| {});
    let t = st.start_task_opts(st.next_task_id(), TASK, "p", None, true, |h| {
        while !h.is_cancelled() {
            std::thread::sleep(Duration::from_millis(2));
        }
        Err(sparkles::Error::Cancelled.into())
    });
    cancel(&st, "p");
    assert_eq!(wait(&st, &t.id).await.state, "cancelled");
}

#[test]
fn the_flags_make_the_policy() {
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        a: AutoCompactArgs,
    }
    let c = Cli::parse_from(["x"]).a.state().unwrap();
    assert!(c.enabled);
    assert_eq!(c.policy, CompactionPolicy::default());
    assert_eq!(c.max_running, 1);
    assert!(c.threads >= 1 && c.io_bytes_per_sec.is_none());
    let c = Cli::parse_from([
        "x",
        "--no-auto-compact",
        "--auto-compact-ratio",
        "0.2",
        "--auto-compact-idle",
        "0",
        "--auto-compact-threads",
        "3",
        "--auto-compact-io-mb",
        "50",
        "--auto-compact-partial",
        "off",
    ])
    .a
    .state()
    .unwrap();
    assert!(!c.enabled);
    assert_eq!(c.policy.partial, sparkles::store::PartialMode::Off);
    assert!(Cli::try_parse_from(["x", "--auto-compact-partial", "sometimes"]).is_err());
    assert_eq!((c.policy.delta_ratio, c.policy.idle_seconds), (0.2, 0));
    assert_eq!((c.threads, c.io_bytes_per_sec), (3, Some(50 << 20)));
    let bad = Cli::parse_from(["x", "--auto-compact-ratio", "NaN"])
        .a
        .state();
    assert!(bad.is_err());
}
