//! Tasks: cancellation (`DELETE /$/tasks/{id}`), the `queued`/`cancelled` states and
//! `detail`, server-scoped tasks, and the restoring layer.

use super::*;
use crate::state::task_state;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// A task that runs until cancelled or until `stop` is set; it reports `queued`
/// first when `queued`.
pub(super) fn spin(
    st: &Arc<AppState>,
    dataset: &str,
    cancellable: bool,
    stop: Arc<AtomicBool>,
) -> String {
    let id = st.next_task_id();
    st.start_task_opts(id.clone(), "test", dataset, None, cancellable, move |h| {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(30) && !stop.load(Ordering::Relaxed) {
            if h.is_cancelled() {
                return Err(sparkles::Error::Cancelled.into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok("stopped".into())
    });
    id
}

pub(super) async fn wait_done(st: &AppState, id: &str) -> crate::state::Task {
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
            t0.elapsed() < Duration::from_secs(30),
            "task {id} did not end"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn delete(app: &Router, path: &str) -> Resp {
    send(app, Request::delete(path).body(Body::empty()).unwrap()).await
}

async fn get(app: &Router, path: &str) -> Resp {
    send(app, Request::get(path).body(Body::empty()).unwrap()).await
}

#[tokio::test]
async fn cancel_ends_a_task_cancelled() {
    let s = server();
    let id = spin(&s.state, "ds", true, Arc::default());
    let t = get(&s.app, &format!("/$/tasks/{id}")).await.json();
    assert_eq!(t["state"], "running");
    assert_eq!(t["cancellable"], true);
    assert!(t.get("detail").is_none(), "{t}");

    let r = delete(&s.app, &format!("/$/tasks/{id}")).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    assert_eq!(r.json()["id"], id.as_str());
    assert_eq!(r.json()["message"], "cancelling");

    let t = wait_done(&s.state, &id).await;
    assert_eq!(t.state, task_state::CANCELLED);
    assert_eq!(t.message.as_deref(), Some("cancelled"));
    assert!(!t.cancellable);
    // a finished task no longer accepts it
    let r = delete(&s.app, &format!("/$/tasks/{id}")).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json()["code"], "not-cancellable");
}

#[tokio::test]
async fn tasks_that_refuse_cancellation() {
    let s = server();
    let stop = Arc::new(AtomicBool::new(false));
    let id = spin(&s.state, "ds", false, stop.clone());
    let r = delete(&s.app, &format!("/$/tasks/{id}")).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json()["code"], "not-cancellable");
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("does not accept")
    );
    stop.store(true, Ordering::Relaxed);
    let t = wait_done(&s.state, &id).await;
    assert_eq!(t.state, task_state::DONE);
    assert_eq!(t.message.as_deref(), Some("stopped"));

    let r = delete(&s.app, "/$/tasks/nope").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn queued_state_detail_and_other_errors() {
    let s = server();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let id = s.state.next_task_id();
    s.state
        .start_task_opts(id.clone(), "test", "ds", None, true, move |h| {
            h.set_state(task_state::QUEUED);
            h.set_detail(serde_json::json!({"level": "exists"}));
            while !stop2.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(2));
            }
            h.set_state(task_state::RUNNING);
            h.set_cancellable(false);
            anyhow::bail!("broken")
        });
    let t0 = Instant::now();
    let t = loop {
        let t = get(&s.app, &format!("/$/tasks/{id}")).await.json();
        if t["state"] == "queued" || t0.elapsed() > Duration::from_secs(10) {
            break t;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    };
    assert_eq!(t["state"], "queued");
    assert_eq!(t["detail"]["level"], "exists");
    stop.store(true, Ordering::Relaxed);
    let t = wait_done(&s.state, &id).await;
    // an error that is not a cancellation fails the task
    assert_eq!(t.state, task_state::FAILED);
    assert_eq!(t.message.as_deref(), Some("broken"));
    assert_eq!(t.detail.unwrap()["level"], "exists");
}

#[tokio::test]
async fn server_scoped_tasks_are_listed_for_server_admins() {
    let s = server();
    let stop = Arc::new(AtomicBool::new(false));
    let id = spin(&s.state, "", true, stop.clone());
    // without auth, the caller is the local server admin
    let list = get(&s.app, "/$/tasks").await.json();
    let t = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id.as_str())
        .unwrap();
    assert_eq!(t["dataset"], "");
    let r = delete(&s.app, &format!("/$/tasks/{id}")).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    assert_eq!(wait_done(&s.state, &id).await.state, task_state::CANCELLED);
}

#[tokio::test]
async fn restoring_datasets_answer_503() {
    let s = server();
    let ask = |app: Router| async move {
        send(
            &app,
            Request::get("/ds/sparql?query=ASK%7B%7D")
                .body(Body::empty())
                .unwrap(),
        )
        .await
    };
    assert_eq!(ask(s.app.clone()).await.status, StatusCode::OK);
    let restoring = s
        .state
        .catalog
        .reserve("ds", sparkles::catalog::ReservationKind::Restore, "7")
        .unwrap();
    let r = ask(s.app.clone()).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    let j = r.json();
    assert_eq!(j["code"], "dataset-restoring");
    assert!(j["error"].as_str().unwrap().contains("task 7"), "{j}");
    let res = s
        .app
        .clone()
        .oneshot(Request::get("/$/datasets/ds").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.headers()[header::RETRY_AFTER], "5");
    // routes without that dataset are not affected
    assert_eq!(get(&s.app, "/$/tasks").await.status, StatusCode::OK);
    assert_eq!(get(&s.app, "/$/datasets").await.status, StatusCode::OK);
    drop(restoring);
    assert_eq!(ask(s.app.clone()).await.status, StatusCode::OK);
}

#[tokio::test]
async fn detach_and_reattach_for_a_swap() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    st.create("p", DbType::Persistent).unwrap();
    st.attach("m", DbType::Mem, None).unwrap();
    let app = router(st.clone());
    // only managed persistent datasets can be swapped
    assert!(st.detach_for_swap("m").is_none());
    assert!(st.detach_for_swap("nope").is_none());
    let ds = st.detach_for_swap("p").unwrap();
    assert_eq!(
        get(&app, "/$/datasets/p").await.status,
        StatusCode::NOT_FOUND
    );
    // the database stays locked while the old store is alive
    assert!(st.reattach("p").is_err());
    drop(ds);
    st.reattach("p").unwrap();
    assert_eq!(get(&app, "/$/datasets/p").await.status, StatusCode::OK);
    assert!(st.reattach("p").is_err(), "already registered");
    // the persisted registry never lost it
    drop(app);
    drop(st);
    let st = AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    assert!(st.get("p").is_some());
}
