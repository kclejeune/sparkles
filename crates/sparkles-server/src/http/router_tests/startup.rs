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

fn eventually(what: &str, mut f: impl FnMut() -> bool) {
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
        |_, _| Ok(()),
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
        |_, _| Ok(()),
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
        |_, _| panic!("the later steps ran"),
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
        |_, _| anyhow::bail!("--text: no dataset nope"),
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

/// The production ordering of a shutdown: the server begins it (`begin_shutdown`, as
/// the shutdown future does) while a long request holds the drain open. Startup opens
/// no more datasets from that moment, not only once the drain has finished, and the
/// datasets left unopened stay registered.
#[tokio::test(flavor = "multi_thread")]
async fn a_shutdown_stops_the_opening_before_requests_drain() {
    let dir = data_dir(&["a", "b", "c"], false);
    let (st, pending) = deferred(dir.path(), false);
    let gate = Gate::holding(&["a", "b", "c"]);
    let opening = startup::spawn(
        st.clone(),
        pending,
        options(Some(&gate), 1),
        |_, _| panic!("the later steps ran after a shutdown"),
        |e| panic!("fatal: {e}"),
    );
    let stop = opening.stop_flag();
    let gate_a = gate.clone();
    tokio::task::spawn_blocking(move || gate_a.wait_entered("a"))
        .await
        .unwrap();

    // a request that holds the drain open until the test lets it finish
    let entered = Arc::new(tokio::sync::Notify::new());
    let finish = Arc::new(tokio::sync::Notify::new());
    let (e2, f2) = (entered.clone(), finish.clone());
    let app = Router::new()
        .route(
            "/test/hold",
            axum::routing::get(move || {
                let (e, f) = (e2.clone(), f2.clone());
                async move {
                    e.notify_one();
                    f.notified().await;
                    "done"
                }
            }),
        )
        .merge(router(st.clone()));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let st2 = st.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            l,
            app.into_make_service_with_connect_info::<crate::auth::Peer>(),
        )
        .with_graceful_shutdown(async move {
            let _ = rx.await;
            startup::begin_shutdown(&st2, &stop);
        })
        .await
        .unwrap();
    });
    let held = tokio::spawn(async move {
        reqwest::get(format!("{base}/test/hold"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    });
    entered.notified().await;
    tx.send(()).unwrap();
    let st3 = st.clone();
    tokio::task::spawn_blocking(move || {
        eventually("draining", || st3.phase() == crate::obs::Phase::Draining)
    })
    .await
    .unwrap();
    assert!(
        !server.is_finished(),
        "the request no longer holds the drain"
    );

    // /a finishes opening, and no other dataset starts while the drain goes on
    gate.release("a");
    let opening = tokio::task::spawn_blocking(move || {
        eventually("startup gives up", || opening.is_finished());
        opening
    })
    .await
    .unwrap();
    assert!(!gate.entered("b") && !gate.entered("c"));
    assert!(st.get("a").is_some());
    assert_eq!(st.catalog.opening(), ["b", "c"]);
    assert!(!server.is_finished());

    finish.notify_one();
    assert_eq!(held.await.unwrap(), "done");
    server.await.unwrap();
    opening.stop_and_join();
    assert_eq!(st.phase(), crate::obs::Phase::Draining);
    drop(st);
    // the next start opens all of them
    let st = AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    for n in ["a", "b", "c"] {
        assert_eq!(st.get(n).unwrap().store.snapshot().len(), 1, "{n}");
    }
}

/// A dataset that startup configures (here with `--validate`) answers as opening until
/// the configuration is installed, so no write reaches it unvalidated while another
/// dataset still opens.
#[cfg(feature = "shacl")]
#[tokio::test(flavor = "multi_thread")]
async fn startup_configuration_is_installed_before_a_dataset_serves() {
    let dir = data_dir(&["cfg", "slow"], false);
    let conf = dir.path().join("reject.json");
    std::fs::write(
        dir.path().join("shapes.ttl"),
        "<urn:S> a <http://www.w3.org/ns/shacl#NodeShape> ; \
         <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/P> ; \
         <http://www.w3.org/ns/shacl#property> [ \
         <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ; \
         <http://www.w3.org/ns/shacl#minCount> 1 ] .",
    )
    .unwrap();
    std::fs::write(
        &conf,
        r#"{"mode": "reject", "shapes": {"source": "shapes.ttl"}}"#,
    )
    .unwrap();
    let (st, pending) = deferred(dir.path(), false);
    startup::hold(&st, &[], &["cfg".to_string()]).unwrap();
    let app = router(st.clone());
    let gate = Gate::holding(&["slow"]);
    let spec = format!("cfg={}", conf.display());
    let opening = startup::spawn(
        st.clone(),
        pending,
        options(Some(&gate), 2),
        move |st, _| crate::write_validation::validate_at_startup(st, &spec).map(drop),
        |e| panic!("fatal: {e}"),
    );
    let (st2, gate2) = (st.clone(), gate.clone());
    tokio::task::spawn_blocking(move || {
        gate2.wait_entered("slow");
        eventually("cfg opens", || st2.catalog.get("cfg").is_some());
    })
    .await
    .unwrap();
    let invalid = "INSERT DATA { <urn:x> a <http://ex.org/P> }";
    let update = |body: &str| {
        Request::post("/cfg/update")
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    // open in the catalog, but not served before its validation is installed
    let r = send(&app, update(invalid)).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.json()["code"], "dataset-opening");
    let r = send(
        &app,
        Request::get("/$/ready/cfg").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.json()["datasets"][0]["state"], "opening", "{}", r.text());
    assert_eq!(st.catalog.get("cfg").unwrap().store().snapshot().len(), 1);

    gate.release("slow");
    let st2 = st.clone();
    tokio::task::spawn_blocking(move || {
        eventually("ready", || st2.phase() == crate::obs::Phase::Ready)
    })
    .await
    .unwrap();
    let r = send(&app, update(invalid)).await;
    assert!(r.status.is_client_error(), "{} {}", r.status, r.text());
    let r = send(
        &app,
        update("INSERT DATA { <urn:y> a <http://ex.org/P> ; <http://ex.org/name> \"y\" }"),
    )
    .await;
    assert!(r.status.is_success(), "{} {}", r.status, r.text());
    opening.stop_and_join();
}

/// The names that startup attaches (`--mem`, `--loc`, `--fuseki-config`) are taken
/// before requests arrive: an administrator cannot create or rename a dataset into
/// one, and startup still attaches it.
#[tokio::test(flavor = "multi_thread")]
async fn names_that_startup_attaches_are_reserved() {
    let dir = data_dir(&["a", "slow"], false);
    let (st, pending) = deferred(dir.path(), false);
    startup::hold(&st, &[("scratch".to_string(), DbType::Mem)], &[]).unwrap();
    // a registered name cannot be attached as well
    assert!(startup::hold(&st, &[("a".to_string(), DbType::Mem)], &[]).is_err());
    let app = router(st.clone());
    let gate = Gate::holding(&["slow"]);
    let opening = startup::spawn(
        st.clone(),
        pending,
        options(Some(&gate), 2),
        |st, _| st.attach_expected("scratch", DbType::Mem, None).map(drop),
        |e| panic!("fatal: {e}"),
    );
    let (st2, gate2) = (st.clone(), gate.clone());
    tokio::task::spawn_blocking(move || {
        gate2.wait_entered("slow");
        eventually("a opens", || st2.get("a").is_some());
    })
    .await
    .unwrap();
    let r = send(
        &app,
        Request::post("/$/datasets?dbName=scratch&dbType=mem")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r = send(
        &app,
        Request::post("/$/datasets/a/rename")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"name": "scratch"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r = send(
        &app,
        Request::get("/scratch/sparql?query=ASK%7B%7D")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.json()["code"], "dataset-opening");

    gate.release("slow");
    let st2 = st.clone();
    tokio::task::spawn_blocking(move || {
        eventually("ready", || st2.phase() == crate::obs::Phase::Ready)
    })
    .await
    .unwrap();
    let r = send(
        &app,
        Request::get("/scratch/sparql?query=ASK%7B%7D")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(st.get("a").is_some());
    opening.stop_and_join();
}

/// A lookup that misses a dataset just before startup publishes it still finds it:
/// the request gets the dataset, never a missing one.
#[test]
fn a_lookup_that_straddles_publication_finds_the_dataset() {
    let dir = data_dir(&["straddle"], false);
    let (st, pending) = deferred(dir.path(), false);
    let gate = Gate::holding(&["straddle"]);
    let opening = startup::spawn(
        st.clone(),
        pending,
        options(Some(&gate), 1),
        |_, _| Ok(()),
        |e| panic!("fatal: {e}"),
    );
    gate.wait_entered("straddle");
    let once = std::sync::atomic::AtomicBool::new(false);
    let (st2, gate2) = (st.clone(), gate.clone());
    startup::LOOKUP_HOOKS.lock().insert(
        "straddle".into(),
        Arc::new(move || {
            if once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            // the first lookup missed: the dataset is published, and leaves the
            // opening state, before the lookup goes on
            gate2.release("straddle");
            eventually("published", || {
                st2.catalog.get("straddle").is_some() && !st2.catalog.is_opening("straddle")
            });
        }),
    );
    let found = st.lookup("straddle");
    startup::LOOKUP_HOOKS.lock().remove("straddle");
    assert!(
        matches!(found, crate::state::Lookup::Open(_)),
        "the straddling lookup missed the dataset"
    );
    opening.stop_and_join();
}

/// A shutdown that starts during a later startup step, here the first data load of
/// `--fuseki-config`, starts no further step: the second load never runs, the error
/// it leaves is not fatal, and the held dataset is never released to requests.
#[test]
fn a_shutdown_during_a_later_step_starts_no_further_step() {
    let dir = data_dir(&[], false);
    let (first, second) = (dir.path().join("first.nt"), dir.path().join("second.nt"));
    std::fs::write(&first, "<urn:first> <urn:p> \"1\" .\n").unwrap();
    std::fs::write(&second, "<urn:second> <urn:p> \"2\" .\n").unwrap();
    let (st, pending) = deferred(dir.path(), false);
    startup::hold(&st, &[("stepstop".to_string(), DbType::Mem)], &[]).unwrap();
    let add = crate::fuseki_config::ServeAdditions {
        data: vec![
            ("stepstop".into(), first, None),
            ("stepstop".into(), second, None),
        ],
        ..Default::default()
    };
    // the first load ends with a shutdown, as one that a signal starts meanwhile
    let stop = Arc::new(parking_lot::Mutex::new(None::<startup::StopFlag>));
    let (st2, stop2) = (st.clone(), stop.clone());
    startup::STEP_HOOKS.lock().insert(
        "stepstop".into(),
        Arc::new(move || {
            let flag = stop2.lock().clone().expect("the stop flag");
            startup::begin_shutdown(&st2, &flag);
        }),
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let opening = startup::spawn(
        st.clone(),
        pending,
        options(None, 1),
        move |st, stop| {
            ready_rx.recv().unwrap();
            st.attach_expected("stepstop", DbType::Mem, None)?;
            crate::fuseki_config::start(st, &add, stop)
        },
        move |e| tx.send(format!("{e:#}")).unwrap(),
    );
    *stop.lock() = Some(opening.stop_flag());
    ready_tx.send(()).unwrap();
    eventually("startup gives up", || opening.is_finished());
    opening.stop_and_join();
    startup::STEP_HOOKS.lock().remove("stepstop");
    assert!(rx.try_recv().is_err(), "a shutdown made the stop fatal");
    // the first load finished, and the second never started
    let held = st.catalog.get("stepstop").unwrap();
    assert_eq!(held.store().snapshot().len(), 1);
    assert!(st.get("stepstop").is_none(), "startup released the dataset");
    assert_eq!(st.phase(), crate::obs::Phase::Draining);
}

/// Each declared dataset counts once in `sparkles_datasets`, by the state a request
/// sees: one that startup holds back counts as opening, also once the catalog has it.
#[tokio::test(flavor = "multi_thread")]
async fn dataset_states_count_each_dataset_once() {
    let dir = data_dir(&["cfg", "slow"], false);
    let (st, pending) = deferred(dir.path(), false);
    startup::hold(&st, &[], &["cfg".to_string()]).unwrap();
    let app = router(st.clone());
    let states = |app: Router| async move {
        let r = send(
            &app,
            Request::get("/$/metrics").body(Body::empty()).unwrap(),
        )
        .await;
        let text = r.text();
        let n = |state: &str| -> u64 {
            let key = format!("sparkles_datasets{{state=\"{state}\"}} ");
            text.lines()
                .find_map(|l| l.strip_prefix(&key))
                .unwrap_or_else(|| panic!("no {key} in metrics"))
                .parse()
                .unwrap()
        };
        let prom = (n("open"), n("opening"), n("failed"));
        let r = send(
            &app,
            Request::get("/$/metrics?format=json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let j = r.json()["datasetStates"].clone();
        let json = (
            j["open"].as_u64().unwrap(),
            j["opening"].as_u64().unwrap(),
            j["failed"].as_u64().unwrap(),
        );
        assert_eq!(prom, json, "the Prometheus and JSON counts differ");
        prom
    };
    // before either dataset is published
    assert_eq!(states(app.clone()).await, (0, 2, 0));

    let gate = Gate::holding(&["slow"]);
    let opening = startup::spawn(
        st.clone(),
        pending,
        options(Some(&gate), 2),
        |_, _| Ok(()),
        |e| panic!("fatal: {e}"),
    );
    let (st2, gate2) = (st.clone(), gate.clone());
    tokio::task::spawn_blocking(move || {
        gate2.wait_entered("slow");
        eventually("cfg is published", || st2.catalog.get("cfg").is_some());
    })
    .await
    .unwrap();
    // /cfg is in the catalog but held back, and /slow still opens
    assert_eq!(states(app.clone()).await, (0, 2, 0));

    gate.release("slow");
    let st2 = st.clone();
    tokio::task::spawn_blocking(move || {
        eventually("ready", || st2.phase() == crate::obs::Phase::Ready)
    })
    .await
    .unwrap();
    assert_eq!(states(app.clone()).await, (2, 0, 0));
    opening.stop_and_join();
}
