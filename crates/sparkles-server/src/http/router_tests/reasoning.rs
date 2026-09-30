//! Freshness of materialized inferences, re-runs, automatic re-materialization and the
//! diagnostics endpoint.

use super::*;
use crate::reasoning::AutoReason;
use std::path::Path;
use std::time::Instant;

const EX: &str = "@prefix ex: <http://ex.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
";

fn open(dir: &Path, auto: Option<AutoReason>, read_only: bool) -> Arc<AppState> {
    let mut st = AppState::new(dir, StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.auto_reason = auto;
    st.read_only = read_only;
    Arc::new(st)
}

fn load(st: &AppState, ds: &str, ttl: &str) {
    st.get(ds)
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            format!("{EX}{ttl}").into_bytes(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
}

async fn wait_tasks(st: &AppState) {
    let t0 = Instant::now();
    while st.tasks.lock().iter().any(|t| t.state == "running") {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "tasks did not finish"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for t in st.tasks.lock().iter() {
        assert_eq!(t.state, "done", "{t:?}");
    }
}

async fn get_json(app: &Router, path: &str) -> J {
    let r = send(app, Request::get(path).body(Body::empty()).unwrap()).await;
    assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text());
    r.json()
}

async fn post_json(app: &Router, path: &str, body: &str) -> Resp {
    send(
        app,
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

async fn update(app: &Router, ds: &str, text: &str) {
    let r = send(
        app,
        Request::post(format!("/{ds}/update"))
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(Body::from(format!("PREFIX ex: <http://ex.org/>\n{text}")))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// Instances of `ex:B` and the `Sparkles-Inferences` header.
async fn b_instances(app: &Router, ds: &str) -> (Vec<String>, Option<String>) {
    let (r, h) = send_h(
        app,
        Request::post(format!("/{ds}/sparql"))
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .body(Body::from(
                "SELECT ?s { ?s a <http://ex.org/B> } ORDER BY ?s",
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = r.json()["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["s"]["value"].as_str().unwrap().to_string())
        .collect();
    let hdr = h
        .get("sparkles-inferences")
        .map(|v| v.to_str().unwrap().to_string());
    (rows, hdr)
}

#[tokio::test]
async fn inference_freshness_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let st = open(dir.path(), None, false);
    st.create("t", DbType::Persistent).unwrap();
    load(&st, "t", "ex:C rdfs:subClassOf ex:B . ex:x a ex:C .");
    let app = router(st.clone());

    // nothing materialized yet
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["reasoning"], J::Null);
    assert_eq!(s["head"], 1);
    // a re-run needs a recorded status
    let r = post_json(&app, "/$/reason/t", r#"{"rerun":true}"#).await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    let r = post_json(&app, "/$/reason/t", r#"{"profile":"rdfs"}"#).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    let c = s["commit"].as_u64().unwrap();
    assert_eq!(
        (s["stale"].clone(), s["commitsSince"].clone()),
        (J::Bool(false), 0.into())
    );
    assert_eq!(s["head"], c);
    assert_eq!(s["profile"], "rdfs");
    assert_eq!(s["auto"]["enabled"], false);
    let (rows, hdr) = b_instances(&app, "t").await;
    assert_eq!(rows, ["http://ex.org/x"]);
    assert_eq!(hdr, None, "fresh inferences send no header");

    update(&app, "t", "INSERT DATA { ex:y a ex:C }").await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["stale"], true);
    assert_eq!(s["commitsSince"], 1);
    assert_eq!(s["head"], c + 1);
    assert_eq!(s["staleReason"], "1 commit since materialization");
    let (_, hdr) = b_instances(&app, "t").await;
    assert_eq!(hdr.as_deref(), Some("stale; commits-since=1"));
    // without inferences there is no header
    let (_, h) = send_h(
        &app,
        Request::get("/t/sparql?reasoning=false&query=ASK%7B%7D")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(h.get("sparkles-inferences").is_none());
    // the dataset info and the statistics carry the status
    let info = get_json(&app, "/$/datasets/t").await;
    assert_eq!(info["reasoning"]["stale"], true);
    assert_eq!(info["reasoning"]["commitsSince"], 1);
    assert_eq!(info["reasoning"]["commit"], c);
    let stats = get_json(&app, "/$/stats/t").await;
    assert_eq!(stats["reasoning"]["commitsSince"], 1);

    // compaction does not change the head
    let r = send(
        &app,
        Request::post("/$/compact/t").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    wait_tasks(&st).await;
    assert_eq!(get_json(&app, "/$/reason/t").await["commitsSince"], 1);

    // nor does a restart
    drop(app);
    drop(st);
    let st = open(dir.path(), None, false);
    let app = router(st.clone());
    assert_eq!(get_json(&app, "/$/reason/t").await["commitsSince"], 1);

    let r = post_json(&app, "/$/reason/t", r#"{"rerun":true}"#).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["stale"], false);
    assert_eq!(s["commit"], c + 2);
    let commit = get_json(&app, &format!("/$/commits/t/{}", c + 2)).await;
    assert_eq!(commit["commit"]["kind"], "reason");
    let (rows, hdr) = b_instances(&app, "t").await;
    assert_eq!(rows, ["http://ex.org/x", "http://ex.org/y"]);
    assert_eq!(hdr, None);

    // the status file is complete JSON with the recorded position
    let file: J = serde_json::from_slice(
        &std::fs::read(dir.path().join("databases/t/reasoning.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(file["reasoningFormat"], 2);
    assert_eq!(file["positionSource"], "commit");
    assert_eq!(file["commit"], c + 2);
}

#[tokio::test]
async fn legacy_or_foreign_status_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    {
        let st = open(dir.path(), None, false);
        st.create("t", DbType::Persistent).unwrap();
        load(&st, "t", "ex:C rdfs:subClassOf ex:B . ex:x a ex:C .");
    }
    let root = dir.path().join("databases/t");
    // written by an older version: no position
    std::fs::write(
        root.join("reasoning.json"),
        r#"{"profile":"rdfs","inferred":1,"at":"2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    let auto = AutoReason::new(Duration::ZERO, None);
    let st = open(dir.path(), Some(auto), false);
    let app = router(st.clone());
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(
        (s["stale"].clone(), s["commitsSince"].clone()),
        (J::Null, J::Null)
    );
    assert_eq!(s["auto"]["enabled"], true);
    let (_, hdr) = b_instances(&app, "t").await;
    assert_eq!(hdr.as_deref(), Some("unknown"));
    // automatic runs never guess
    crate::reasoning::auto_reason_tick(&st, Instant::now());
    assert!(st.tasks.lock().is_empty());

    // a status recorded for another dataset id
    drop(app);
    drop(st);
    std::fs::write(
        root.join("reasoning.json"),
        r#"{"profile":"rdfs","inferred":1,"at":"2026-01-01T00:00:00Z","commit":1,
            "positionSource":"commit","datasetId":"00000000-0000-4000-8000-000000000000"}"#,
    )
    .unwrap();
    let st = open(dir.path(), None, false);
    let s = get_json(&router(st), "/$/reason/t").await;
    assert_eq!(s["stale"], J::Null);
    assert_eq!(s["staleReason"], "recorded for another dataset");
}

#[tokio::test]
async fn automatic_runs_after_the_debounce() {
    let dir = tempfile::tempdir().unwrap();
    let st = open(
        dir.path(),
        Some(AutoReason::new(
            Duration::from_secs(5),
            Some(Duration::from_secs(20)),
        )),
        false,
    );
    st.attach("t", DbType::Mem, None).unwrap();
    load(&st, "t", "ex:C rdfs:subClassOf ex:B . ex:x a ex:C .");
    let app = router(st.clone());
    post_json(&app, "/$/reason/t", r#"{"profile":"rdfs"}"#).await;
    wait_tasks(&st).await;
    let t0 = Instant::now();
    // fresh: nothing to do
    crate::reasoning::auto_reason_tick(&st, t0);
    assert_eq!(st.tasks.lock().len(), 1);

    update(&app, "t", "INSERT DATA { ex:y a ex:C }").await;
    crate::reasoning::auto_reason_tick(&st, t0);
    assert_eq!(st.tasks.lock().len(), 1, "within the debounce");
    let s = get_json(&app, "/$/reason/t").await;
    assert!(s["auto"]["scheduledAt"].is_string(), "{s}");
    // writes keep coming: the head changed, the debounce restarts
    update(&app, "t", "INSERT DATA { ex:z a ex:C }").await;
    crate::reasoning::auto_reason_tick(&st, t0 + Duration::from_secs(4));
    crate::reasoning::auto_reason_tick(&st, t0 + Duration::from_secs(8));
    assert_eq!(st.tasks.lock().len(), 1, "the head changed 4 s ago");
    crate::reasoning::auto_reason_tick(&st, t0 + Duration::from_secs(10));
    wait_tasks(&st).await;
    let tasks = st.tasks.lock().clone();
    assert_eq!(tasks.len(), 2);
    assert!(
        tasks[1].message.as_deref().unwrap().starts_with("auto: "),
        "{tasks:?}"
    );
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["stale"], false);
    let (rows, _) = b_instances(&app, "t").await;
    assert_eq!(rows.len(), 3);

    // the maximum delay forces a run while writes never pause
    update(&app, "t", "INSERT DATA { ex:w a ex:C }").await;
    let t1 = t0 + Duration::from_secs(100);
    crate::reasoning::auto_reason_tick(&st, t1);
    for i in 1..=19 {
        update(&app, "t", &format!("INSERT DATA {{ ex:v{i} a ex:C }}")).await;
        crate::reasoning::auto_reason_tick(&st, t1 + Duration::from_secs(i));
    }
    assert_eq!(st.tasks.lock().len(), 2);
    update(&app, "t", "INSERT DATA { ex:u a ex:C }").await;
    crate::reasoning::auto_reason_tick(&st, t1 + Duration::from_secs(20));
    wait_tasks(&st).await;
    assert_eq!(st.tasks.lock().len(), 3);
}

#[tokio::test]
async fn no_automatic_runs_on_a_read_only_server() {
    let dir = tempfile::tempdir().unwrap();
    let st = open(
        dir.path(),
        Some(AutoReason::new(Duration::ZERO, None)),
        true,
    );
    let ds = st.attach("t", DbType::Mem, None).unwrap();
    load(&st, "t", "ex:C rdfs:subClassOf ex:B . ex:x a ex:C .");
    let rep = sparkles_reasoner::materialize(
        &ds.store,
        &sparkles_reasoner::Profile::Rdfs,
        &Default::default(),
    )
    .unwrap();
    ds.set_reasoning(Some(crate::reasoning::recorded(
        &sparkles_reasoner::Profile::Rdfs,
        &rep,
        &ds.store,
    )))
    .unwrap();
    load(&st, "t", "ex:y a ex:C .");
    crate::reasoning::auto_reason_tick(&st, Instant::now() + Duration::from_secs(60));
    assert!(st.tasks.lock().is_empty());
    let s = get_json(&router(st.clone()), "/$/reason/t").await;
    assert_eq!(s["stale"], true);
    assert_eq!(s["auto"]["enabled"], false);
}

#[tokio::test]
async fn diagnostics_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let st = open(dir.path(), None, false);
    st.attach("t", DbType::Mem, None).unwrap();
    load(
        &st,
        "t",
        "ex:Cat owl:disjointWith ex:Dog . ex:Kitten rdfs:subClassOf ex:Cat .
         ex:tom a ex:Kitten, ex:Dog .",
    );
    let app = router(st.clone());
    let r = get_json(&app, "/$/reason/t/diagnostics").await;
    assert_eq!(r["diagnosticsFormat"], 1);
    assert_eq!(r["dataset"], "t");
    assert_eq!(r["status"], "violations-found");
    assert_eq!(r["scope"]["inferences"]["included"], false);
    assert_eq!(r["checks"].as_array().unwrap().len(), 7);
    assert_eq!(r["findings"].as_array().unwrap().len(), 1);
    let f = &r["findings"][0];
    assert_eq!(f["check"], "disjoint-classes");
    assert_eq!(f["rule"], "cax-dw");
    assert_eq!(f["focus"]["value"], "http://ex.org/tom");
    assert_eq!(
        f["message"],
        "ex:tom is an instance of the disjoint classes ex:Cat and ex:Dog"
    );
    let r = get_json(&app, "/$/reason/t/diagnostics?closure=none").await;
    assert_eq!(r["status"], "none-found");
    let r = get_json(
        &app,
        "/$/reason/t/diagnostics?checks=same-different,thing-empty",
    )
    .await;
    assert_eq!(r["checks"].as_array().unwrap().len(), 2);

    // with (stale) inferences: the scope says so
    post_json(&app, "/$/reason/t", r#"{"profile":"rdfs"}"#).await;
    wait_tasks(&st).await;
    update(&app, "t", "INSERT DATA { ex:felix a ex:Cat }").await;
    let r = get_json(&app, "/$/reason/t/diagnostics").await;
    let inf = &r["scope"]["inferences"];
    assert_eq!(inf["included"], true);
    assert_eq!(inf["profile"], "rdfs");
    assert_eq!(inf["stale"], true);
    assert_eq!(inf["commitsSince"], 1);
    let r = get_json(&app, "/$/reason/t/diagnostics?reasoning=false").await;
    assert_eq!(r["scope"]["inferences"]["included"], false);

    for bad in [
        "checks=bogus",
        "limit=0",
        "limit=10001",
        "limit=x",
        "closure=full",
    ] {
        let r = send(
            &app,
            Request::get(format!("/$/reason/t/diagnostics?{bad}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let r = send(
        &app,
        Request::get("/$/reason/nope/diagnostics")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}
