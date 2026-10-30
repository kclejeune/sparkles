//! Bind-before-open startup (spec C01 §4.1, Phase 2): the listener answers while the
//! datasets open, a dataset that does not open leaves the server degraded, and a
//! shutdown during the opening leaves every dataset registered.

use super::*;
use crate::startup::{self, gate::Gate};
use std::path::Path;

/// A data directory with the persistent datasets `names`, each holding one triple, and
/// `bad`, whose `dataset.json` is not JSON when `with_bad` is set.
fn data_dir(names: &[&str], with_bad: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let st = AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    for n in names.iter().copied().chain(with_bad.then_some("bad")) {
        let ds = st.create(n, DbType::Persistent).unwrap();
        ds.store
            .load(&[Source::from_bytes(
                format!("<urn:{n}> <urn:p> \"1\" .").into_bytes(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    drop(st);
    if with_bad {
        std::fs::write(dir.path().join("databases/bad/dataset.json"), b"not json").unwrap();
    }
    dir
}

fn deferred(dir: &Path, ignore_failed: bool) -> (Arc<AppState>, Vec<String>) {
    let (mut st, pending) =
        AppState::deferred(dir, StoreOptions::default(), Duration::from_secs(30), None).unwrap();
    st.ready_ignore_failed = ignore_failed;
    (Arc::new(st), pending)
}

fn options(gate: Option<&Arc<Gate>>, threads: usize) -> startup::Options {
    startup::Options {
        threads,
        require_all: false,
        before_open: gate.map(|g| g.hook()),
    }
}

fn eventually(what: &str, f: impl Fn() -> bool) {
    let t0 = std::time::Instant::now();
    while !f() {
        assert!(t0.elapsed() < Duration::from_secs(30), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A real listener on port 0 serving `app` until the returned sender fires.
async fn listen(app: Router) -> (String, tokio::sync::oneshot::Sender<()>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let service = app.into_make_service_with_connect_info::<crate::auth::Peer>();
    tokio::spawn(async move {
        axum::serve(l, service)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    (base, tx)
}

async fn fetch(c: &reqwest::Client, url: String) -> (StatusCode, reqwest::header::HeaderMap, J) {
    let r = c.get(url).send().await.unwrap();
    let status = StatusCode::from_u16(r.status().as_u16()).unwrap();
    let headers = r.headers().clone();
    let body = serde_json::from_slice(&r.bytes().await.unwrap()).unwrap_or(J::Null);
    (status, headers, body)
}

fn state_of(ready: &J, name: &str) -> String {
    ready["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == name)
        .unwrap_or_else(|| panic!("{name} not in {ready}"))["state"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_listener_answers_while_a_slow_dataset_opens_and_a_bad_one_degrades_the_server() {
    let dir = data_dir(&["fast", "slow"], true);
    let (st, pending) = deferred(dir.path(), false);
    assert_eq!(pending, ["bad", "fast", "slow"]);
    let (base, stop) = listen(router(st.clone())).await;
    let c = reqwest::Client::new();
    let ask = |ds: &str| format!("{base}/{ds}/sparql?query=ASK%7B%7D");

    // bound before anything opens: every dataset is opening, and none is a 404
    let (s, _, r) = fetch(&c, format!("{base}/$/ready")).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{r}");
    assert_eq!(r["status"], "starting", "{r}");
    for n in ["bad", "fast", "slow"] {
        assert_eq!(state_of(&r, n), "opening", "{r}");
    }
    let (s, h, r) = fetch(&c, ask("fast")).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{r}");
    assert_eq!(r["code"], "dataset-opening", "{r}");
    assert_eq!(h.get("retry-after").unwrap(), "5");
    let (s, _, _) = fetch(&c, ask("nosuch")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(fetch(&c, format!("{base}/$/ping")).await.0, StatusCode::OK);

    let gate = Gate::holding(&["slow"]);
    let opening = startup::spawn(
        st.clone(),
        pending,
        options(Some(&gate), 2),
        |_| Ok(()),
        |e| panic!("fatal: {e}"),
    );
    gate.wait_entered("slow");
    eventually("fast opens", || st.get("fast").is_some());
    eventually("bad fails", || st.catalog.refusal("bad").is_some());

    // the open dataset serves while another one still opens
    let (s, _, r) = fetch(&c, ask("fast")).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["boolean"], true);
    let (s, h, r) = fetch(&c, ask("slow")).await;
    assert_eq!(
        (s, r["code"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("dataset-opening"))
    );
    assert_eq!(h.get("retry-after").unwrap(), "5");
    let (s, h, r) = fetch(&c, ask("bad")).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{r}");
    assert_eq!(r["code"], "dataset-failed", "{r}");
    assert_eq!(r["kind"], "corrupt", "{r}");
    assert!(h.get("retry-after").is_none());
    let (s, _, r) = fetch(&c, format!("{base}/$/ready")).await;
    assert_eq!(
        (s, r["status"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("starting"))
    );
    assert_eq!(state_of(&r, "fast"), "open");
    assert_eq!(state_of(&r, "slow"), "opening");
    assert_eq!(state_of(&r, "bad"), "failed");
    let (s, _, r) = fetch(&c, format!("{base}/$/ready/slow")).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r["datasets"][0]["state"], "opening", "{r}");

    // admin operations that would race the opening are refused
    let r = c
        .delete(format!("{base}/$/datasets/slow"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 503);
    let r = c
        .post(format!("{base}/$/datasets/slow/rename?name=other"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 503);
    let r = c
        .post(format!("{base}/$/datasets?dbName=slow&dbType=tdb2"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 409, "{}", r.text().await.unwrap());
    let r = c
        .delete(format!("{base}/$/datasets/bad"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 503);

    gate.release("slow");
    eventually("ready phase", || st.phase() == crate::obs::Phase::Ready);
    let (s, _, r) = fetch(&c, ask("slow")).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    // a dataset that failed leaves the server degraded and not ready
    let (s, _, r) = fetch(&c, format!("{base}/$/ready")).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{r}");
    assert_eq!(r["status"], "degraded", "{r}");
    let bad = r["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "bad")
        .unwrap();
    assert_eq!(bad["errorKind"], "corrupt", "{r}");
    // without auth the caller administers the server and reads the whole reason
    assert!(
        bad["error"].as_str().unwrap().contains("is not served"),
        "{r}"
    );
    let m = c
        .get(format!("{base}/$/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    for line in [
        "sparkles_ready 0",
        "sparkles_datasets{state=\"open\"} 2",
        "sparkles_datasets{state=\"opening\"} 0",
        "sparkles_datasets{state=\"failed\"} 1",
    ] {
        assert!(m.lines().any(|l| l == line), "{line} not in metrics");
    }
    let (_, _, j) = fetch(&c, format!("{base}/$/metrics?format=json")).await;
    assert_eq!(j["status"], "degraded");
    assert_eq!(j["datasetStates"]["failed"], 1);
    let (_, _, srv) = fetch(&c, format!("{base}/$/server")).await;
    assert_eq!(srv["refusedDatasets"][0]["name"], "bad", "{srv}");
    assert_eq!(srv["refusedDatasets"][0]["kind"], "corrupt", "{srv}");
    opening.stop_and_join();
    let _ = stop.send(());
}

#[tokio::test(flavor = "multi_thread")]
async fn ready_ignore_failed_reports_ready_while_degraded() {
    let dir = data_dir(&["fast"], true);
    let (st, pending) = deferred(dir.path(), true);
    let app = router(st.clone());
    let opening = startup::spawn(
        st.clone(),
        pending,
        options(None, 2),
        |_| Ok(()),
        |e| panic!("fatal: {e}"),
    );
    eventually("ready phase", || st.phase() == crate::obs::Phase::Ready);
    opening.stop_and_join();
    let r = send(&app, Request::get("/$/ready").body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["status"], "degraded");
    let r = send(
        &app,
        Request::get("/$/metrics").body(Body::empty()).unwrap(),
    )
    .await;
    assert!(r.text().lines().any(|l| l == "sparkles_ready 1"));
}

#[test]
fn require_all_datasets_stops_startup_at_a_failure() {
    let dir = data_dir(&["fast"], true);
    let (st, pending) = deferred(dir.path(), false);
    let (tx, rx) = std::sync::mpsc::channel();
    let opening = startup::spawn(
        st.clone(),
        pending,
        startup::Options {
            require_all: true,
            ..options(None, 2)
        },
        |_| panic!("the later steps ran"),
        move |e| tx.send(format!("{e:#}")).unwrap(),
    );
    let e = rx.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(
        e.contains("--require-all-datasets") && e.contains("/bad"),
        "{e}"
    );
    opening.stop_and_join();
    assert_eq!(st.phase(), crate::obs::Phase::Starting);
}

#[test]
fn a_failed_later_step_is_fatal_and_the_server_never_becomes_ready() {
    let dir = data_dir(&["a"], false);
    let (st, pending) = deferred(dir.path(), false);
    let (tx, rx) = std::sync::mpsc::channel();
    let opening = startup::spawn(
        st.clone(),
        pending,
        options(None, 1),
        |_| anyhow::bail!("--text: no dataset nope"),
        move |e| tx.send(format!("{e:#}")).unwrap(),
    );
    assert!(
        rx.recv_timeout(Duration::from_secs(30))
            .unwrap()
            .contains("--text")
    );
    opening.stop_and_join();
    assert_eq!(st.phase(), crate::obs::Phase::Starting);
}

#[test]
fn a_shutdown_during_the_opening_waits_for_it_and_keeps_every_dataset_registered() {
    let dir = data_dir(&["a", "b", "c"], false);
    let (st, pending) = deferred(dir.path(), false);
    let gate = Gate::holding(&["a"]);
    let opening = startup::spawn(
        st.clone(),
        pending,
        options(Some(&gate), 1),
        |_| panic!("the later steps ran after a shutdown"),
        |e| panic!("fatal: {e}"),
    );
    gate.wait_entered("a");
    // the server drains, then stops the opening
    st.set_phase(crate::obs::Phase::Draining);
    let joined = std::thread::spawn(move || opening.stop_and_join());
    std::thread::sleep(Duration::from_millis(200));
    assert!(!joined.is_finished(), "the shutdown did not wait for /a");
    gate.release("a");
    joined.join().unwrap();
    // the dataset that was opening finished, and no other one started
    assert!(st.get("a").is_some());
    assert!(!gate.entered("b") && !gate.entered("c"));
    assert_eq!(st.catalog.opening(), ["b", "c"]);
    assert_eq!(st.phase(), crate::obs::Phase::Draining);
    drop(st);
    // the next start opens all of them
    let st = AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    for n in ["a", "b", "c"] {
        assert_eq!(st.get(n).unwrap().store.snapshot().len(), 1, "{n}");
    }
}
