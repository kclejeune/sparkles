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
async fn only_default_graph_commits_make_inferences_stale() {
    let dir = tempfile::tempdir().unwrap();
    let auto = || Some(AutoReason::new(Duration::ZERO, None));
    let st = open(dir.path(), auto(), false);
    st.create("t", DbType::Persistent).unwrap();
    load(&st, "t", "ex:C rdfs:subClassOf ex:B . ex:x a ex:C .");
    let app = router(st.clone());
    post_json(&app, "/$/reason/t", r#"{"profile":"rdfs"}"#).await;
    wait_tasks(&st).await;
    let c = get_json(&app, "/$/reason/t").await["commit"]
        .as_u64()
        .unwrap();

    // a named graph, and the inferred graph itself: still fresh
    update(&app, "t", "INSERT DATA { GRAPH ex:g { ex:y a ex:C } }").await;
    update(
        &app,
        "t",
        "INSERT DATA { GRAPH <urn:x-sparkles:inferred> { ex:z a ex:B } }",
    )
    .await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["stale"], false, "{s}");
    assert_eq!(s["commitsSince"], 2);
    assert_eq!(s["head"], c + 2);
    assert!(s.get("staleReason").is_none());
    let (_, hdr) = b_instances(&app, "t").await;
    assert_eq!(hdr, None);
    assert_eq!(
        get_json(&app, "/$/datasets/t").await["reasoning"]["stale"],
        false
    );
    // so automatic mode has nothing to do
    crate::reasoning::auto_reason_tick(&st, Instant::now() + Duration::from_secs(60));
    assert_eq!(st.tasks.lock().len(), 1);

    // the same after a restart, when the flags come from the WAL and the catalog
    drop(app);
    drop(st);
    let st = open(dir.path(), auto(), false);
    let app = router(st.clone());
    assert_eq!(get_json(&app, "/$/reason/t").await["stale"], false);

    // a change to the default graph makes them stale, counting every commit since
    update(&app, "t", "INSERT DATA { ex:w a ex:C }").await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["stale"], true);
    assert_eq!(s["commitsSince"], 3);
    let (_, hdr) = b_instances(&app, "t").await;
    assert_eq!(hdr.as_deref(), Some("stale; commits-since=3"));
    crate::reasoning::auto_reason_tick(&st, Instant::now() + Duration::from_secs(60));
    wait_tasks(&st).await;
    assert_eq!(st.tasks.lock().len(), 1, "an automatic run");
    assert_eq!(get_json(&app, "/$/reason/t").await["stale"], false);
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
    // the automatic run updated the closure the dataset keeps in memory
    assert_eq!(tasks[1].detail.as_ref().unwrap()["method"], "incremental");
    assert_eq!(s["run"]["changes"]["source"], "memory", "{s}");
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

/// The last task's detail and message.
fn last_task(st: &AppState) -> (J, String) {
    let t = st.tasks.lock().last().unwrap().clone();
    (t.detail.unwrap(), t.message.unwrap())
}

#[tokio::test]
async fn incremental_runs() {
    let dir = tempfile::tempdir().unwrap();
    let st = open(dir.path(), None, false);
    st.create("t", DbType::Persistent).unwrap();
    load(&st, "t", "ex:C rdfs:subClassOf ex:B . ex:x a ex:C .");
    let app = router(st.clone());
    post_json(&app, "/$/reason/t", r#"{"profile":"rdfs"}"#).await;
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["run"]["method"], "full", "{s}");
    assert_eq!(s["run"].get("fallback"), None);

    update(
        &app,
        "t",
        "DELETE DATA { ex:x a ex:C } ; INSERT DATA { ex:y a ex:C }",
    )
    .await;
    post_json(&app, "/$/reason/t", r#"{"rerun":true}"#).await;
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["stale"], false);
    assert_eq!(s["run"]["method"], "incremental", "{s}");
    let c = &s["run"]["changes"];
    assert_eq!(
        (&c["explicitAdded"], &c["explicitRemoved"], &c["source"]),
        (
            &serde_json::json!(1),
            &serde_json::json!(1),
            &serde_json::json!("memory")
        ),
        "{s}"
    );
    let (detail, message) = last_task(&st);
    assert_eq!(detail["method"], "incremental");
    assert!(
        message.contains("incremental: 1 explicit triples added, 1 removed"),
        "{message}"
    );
    let (rows, _) = b_instances(&app, "t").await;
    assert_eq!(rows, ["http://ex.org/y"]);

    // asked for in full
    update(&app, "t", "INSERT DATA { ex:z a ex:C }").await;
    post_json(&app, "/$/reason/t", r#"{"rerun":true,"full":true}"#).await;
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(
        (&s["run"]["method"], &s["stale"]),
        (&serde_json::json!("full"), &serde_json::json!(false))
    );

    // after a restart the closure comes from the dataset
    drop(app);
    drop(st);
    let st = open(dir.path(), None, false);
    let app = router(st.clone());
    update(&app, "t", "DELETE DATA { ex:y a ex:C }").await;
    post_json(&app, "/$/reason/t", r#"{"rerun":true}"#).await;
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["run"]["method"], "incremental", "{s}");
    assert_eq!(s["run"]["changes"]["source"], "store", "{s}");
    let (rows, _) = b_instances(&app, "t").await;
    assert_eq!(rows, ["http://ex.org/z"]);

    // another profile runs in full, and says why
    post_json(&app, "/$/reason/t", r#"{"profile":"rdfs-simple"}"#).await;
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["run"]["method"], "full");
    assert_eq!(
        s["run"]["fallback"], "the rules changed since the previous run",
        "{s}"
    );
}

/// An ontology in a named graph and an import: what the status records, which commits
/// make the inferences stale, and re-runs over the recorded graphs.
#[tokio::test]
async fn input_graphs_and_imports() {
    let dir = tempfile::tempdir().unwrap();
    let auto = || Some(AutoReason::per_dataset());
    let st = open(dir.path(), auto(), false);
    st.create("t", DbType::Persistent).unwrap();
    load(&st, "t", "<urn:o> owl:imports ex:extra . ex:x a ex:C .");
    let app = router(st.clone());
    update(
        &app,
        "t",
        "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA { GRAPH ex:onto { ex:C rdfs:subClassOf ex:A } }",
    )
    .await;
    let r = post_json(
        &app,
        "/$/reason/t",
        r#"{"profile":"rdfs","ontologyGraphs":["http://ex.org/onto"]}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(
        s["inputGraphs"],
        serde_json::json!(["default", "http://ex.org/onto"]),
        "{s}"
    );
    // the import that did not resolve is watched
    assert_eq!(
        s["watchedGraphs"],
        serde_json::json!(["default", "http://ex.org/extra", "http://ex.org/onto"]),
        "{s}"
    );
    assert_eq!(s["imports"][0]["iri"], "http://ex.org/extra");
    assert_eq!(s["inputs"]["ontologyGraphs"][0], "http://ex.org/onto");
    assert!(
        s["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("did not resolve")),
        "{s}"
    );

    // a graph the run does not read: still fresh
    update(&app, "t", "INSERT DATA { GRAPH ex:other { ex:y a ex:C } }").await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(
        (&s["stale"], &s["commitsSince"]),
        (&J::Bool(false), &J::from(1))
    );
    // the ontology graph: stale, and a re-run reads the recorded graphs incrementally
    update(
        &app,
        "t",
        "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA { GRAPH ex:onto { ex:A rdfs:subClassOf ex:B } }",
    )
    .await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["stale"], true, "{s}");
    let (_, hdr) = b_instances(&app, "t").await;
    assert_eq!(hdr.as_deref(), Some("stale; commits-since=2"));
    post_json(&app, "/$/reason/t", r#"{"rerun":true}"#).await;
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["stale"], false, "{s}");
    assert_eq!(s["run"]["method"], "incremental", "{s}");
    let (rows, hdr) = b_instances(&app, "t").await;
    assert_eq!((rows, hdr), (vec!["http://ex.org/x".to_string()], None));

    // the missing import appears: stale, and the re-run reads it, in full
    update(&app, "t", "INSERT DATA { GRAPH ex:extra { ex:z a ex:C } }").await;
    assert_eq!(get_json(&app, "/$/reason/t").await["stale"], true);
    // after a restart, the staleness of named graphs comes from the commit diff
    drop(app);
    drop(st);
    let st = open(dir.path(), auto(), false);
    let app = router(st.clone());
    assert_eq!(get_json(&app, "/$/reason/t").await["stale"], true);
    post_json(&app, "/$/reason/t", r#"{"rerun":true}"#).await;
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(
        s["run"]["fallback"], "the input graphs changed since the previous run",
        "{s}"
    );
    assert_eq!(s["imports"][0]["graph"], "http://ex.org/extra", "{s}");
    let (rows, _) = b_instances(&app, "t").await;
    assert_eq!(rows, ["http://ex.org/x", "http://ex.org/z"]);

    // bad input graphs
    let r = post_json(
        &app,
        "/$/reason/t",
        r#"{"profile":"rdfs","dataGraphs":["urn:x-sparkles:inferred"]}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = post_json(&app, "/$/reason/t", r#"{"imports":"sometimes"}"#).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

/// Imports fetched from files under `--load-dir`, kept in the dataset, and refreshed.
#[tokio::test]
async fn fetched_imports() {
    let dir = tempfile::tempdir().unwrap();
    let docs = tempfile::tempdir().unwrap();
    std::fs::write(
        docs.path().join("onto.ttl"),
        "<http://ex.org/C> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex.org/B> .",
    )
    .unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.file_loads = sparkles::sparql::FileLoads::under(docs.path()).unwrap();
    let st = Arc::new(st);
    st.create("t", DbType::Persistent).unwrap();
    load(&st, "t", "<urn:o> owl:imports ex:onto . ex:x a ex:C .");
    let app = router(st.clone());
    let body = serde_json::json!({
        "profile": "rdfs",
        "imports": "fetch",
        "locationMapping": [{
            "name": "http://ex.org/onto",
            "altName": format!("file://{}/onto.ttl", docs.path().canonicalize().unwrap().display()),
        }],
    });
    let r = post_json(&app, "/$/reason/t", &body.to_string()).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(
        s["fetchedImports"],
        serde_json::json!(["http://ex.org/onto"]),
        "{s}"
    );
    let (rows, _) = b_instances(&app, "t").await;
    assert_eq!(rows, ["http://ex.org/x"]);
    // the copy is a graph of the dataset
    let r = send(
        &app,
        Request::get("/t/data?graph=http%3A%2F%2Fex.org%2Fonto")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(r.text().contains("subClassOf"), "{}", r.text());

    // a refresh loads the changed document again
    std::fs::write(
        docs.path().join("onto.ttl"),
        "<http://ex.org/C> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex.org/D> .",
    )
    .unwrap();
    post_json(&app, "/$/reason/t", r#"{"rerun":true}"#).await;
    wait_tasks(&st).await;
    assert_eq!(b_instances(&app, "t").await.0, ["http://ex.org/x"]);
    post_json(
        &app,
        "/$/reason/t",
        r#"{"rerun":true,"refreshImports":true}"#,
    )
    .await;
    wait_tasks(&st).await;
    assert!(b_instances(&app, "t").await.0.is_empty());
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
        &Default::default(),
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
async fn diagnostics_over_chosen_graphs() {
    let dir = tempfile::tempdir().unwrap();
    let st = open(dir.path(), None, false);
    st.attach("t", DbType::Mem, None).unwrap();
    load(&st, "t", "ex:Cat owl:disjointWith ex:Dog .");
    let app = router(st.clone());
    update(
        &app,
        "t",
        "INSERT DATA { GRAPH ex:data { ex:tom a ex:Cat, ex:Dog } }",
    )
    .await;
    let r = get_json(&app, "/$/reason/t/diagnostics").await;
    assert_eq!(r["status"], "none-found");
    let q = "/$/reason/t/diagnostics?graph=default&graph=http%3A%2F%2Fex.org%2Fdata";
    let r = get_json(&app, q).await;
    assert_eq!(r["status"], "violations-found", "{r}");
    assert_eq!(r["scope"]["graph"], "graphs");
    assert_eq!(
        r["scope"]["graphs"],
        serde_json::json!(["default", "http://ex.org/data"])
    );
    let r = send(
        &app,
        Request::get("/$/reason/t/diagnostics?graph=nope")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
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
    assert_eq!(r["checks"].as_array().unwrap().len(), 16);
    assert_eq!(r["findings"].as_array().unwrap().len(), 1);
    let f = &r["findings"][0];
    assert_eq!(f["check"], "disjoint-classes");
    assert_eq!(f["rule"], "cax-dw");
    assert_eq!(f["focus"]["value"], "http://ex.org/tom");
    assert_eq!(
        f["message"],
        "ex:tom is an instance of the disjoint classes ex:Cat and ex:Dog"
    );
    // Turtle, by Accept or by format
    for (path, accept) in [
        ("/$/reason/t/diagnostics", "text/turtle"),
        ("/$/reason/t/diagnostics?format=turtle", "*/*"),
    ] {
        let r = send(
            &app,
            Request::get(path)
                .header(header::ACCEPT, accept)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        let ttl = r.text();
        assert!(ttl.contains("spk:DiagnosticsReport"), "{ttl}");
        assert!(ttl.contains("sh:focusNode ex:tom"), "{ttl}");
        assert!(ttl.contains("spk:dataset \"t\""), "{ttl}");
    }
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
        "format=xml",
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

/// Instances of `geo:Geometry` and the features' default geometries, with inferences.
async fn geometries(app: &Router, ds: &str) -> Vec<String> {
    let r = send(
        app,
        Request::post(format!("/{ds}/sparql"))
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .body(Body::from(
                "PREFIX geo: <http://www.opengis.net/ont/geosparql#>
                 SELECT ?s ?k { { ?s a geo:Geometry BIND(\"geometry\" AS ?k) }
                   UNION { ?s geo:hasDefaultGeometry ?g BIND(\"default\" AS ?k) } }
                 ORDER BY ?k ?s",
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            format!(
                "{} {}",
                b["k"]["value"].as_str().unwrap(),
                b["s"]["value"].as_str().unwrap()
            )
        })
        .collect()
}

#[tokio::test]
async fn geosparql_vocabulary_and_default_geometries() {
    let dir = tempfile::tempdir().unwrap();
    let st = open(dir.path(), None, false);
    st.create("t", DbType::Persistent).unwrap();
    load(
        &st,
        "t",
        "@prefix geo: <http://www.opengis.net/ont/geosparql#> .
         @prefix sf: <http://www.opengis.net/ont/sf#> .
         ex:gA a sf:Polygon .
         ex:p1 geo:hasGeometry ex:g1 .
         ex:p2 geo:hasGeometry ex:g2a , ex:g2b .",
    );
    let app = router(st.clone());
    // an unknown vocabulary is refused, in JSON and in a form
    let r = post_json(&app, "/$/reason/t", r#"{"vocabularies":["dublin-core"]}"#).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.text().contains("unknown vocabulary"), "{}", r.text());
    let r = send(
        &app,
        Request::post("/$/reason/t")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("profile=rdfs&vocabulary=nope"))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = post_json(
        &app,
        "/$/reason/t",
        r#"{"profile":"rdfs","vocabularies":["geosparql"],"geoDefaultGeometry":true}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["vocabularies"], serde_json::json!(["geosparql"]));
    assert_eq!(s["geoDefaultGeometry"], true);
    let inferred = s["inferred"].as_u64().unwrap();
    let want = [
        "default http://ex.org/p1",
        "geometry http://ex.org/g1",
        "geometry http://ex.org/g2a",
        "geometry http://ex.org/g2b",
        "geometry http://ex.org/gA",
    ];
    assert_eq!(geometries(&app, "t").await, want);
    // the vocabulary's axioms are inferences, not data of the default graph
    let r = send(
        &app,
        Request::get(
            "/t/sparql?reasoning=false&query=ASK%7B%3Fs%20%3Chttp%3A%2F%2Fwww.w3.org%2F2000%2F01%2Frdf-schema%23subClassOf%3E%20%3Fo%7D",
        )
        .header(header::ACCEPT, "application/sparql-results+json")
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(r.json()["boolean"], false);

    // a re-run repeats the extras
    update(
        &app,
        "t",
        "INSERT DATA { ex:p3 <http://www.opengis.net/ont/geosparql#hasGeometry> ex:g3 }",
    )
    .await;
    let r = post_json(&app, "/$/reason/t", r#"{"rerun":true}"#).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["vocabularies"], serde_json::json!(["geosparql"]));
    assert_eq!(s["geoDefaultGeometry"], true);
    assert!(s["inferred"].as_u64().unwrap() > inferred);
    let g = geometries(&app, "t").await;
    assert!(g.contains(&"default http://ex.org/p3".to_string()), "{g:?}");

    // without the extras the vocabulary and the default geometries go away
    let r = post_json(&app, "/$/reason/t", r#"{"profile":"rdfs"}"#).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert!(s.get("vocabularies").is_none() && s.get("geoDefaultGeometry").is_none());
    assert_eq!(geometries(&app, "t").await, Vec::<String>::new());
}

async fn put_auto(app: &Router, ds: &str, body: &str) -> Resp {
    send(
        app,
        Request::put(format!("/$/reason/{ds}/auto"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn per_dataset_automatic_runs() {
    let dir = tempfile::tempdir().unwrap();
    let st = open(dir.path(), Some(AutoReason::per_dataset()), false);
    st.create("t", DbType::Persistent).unwrap();
    load(&st, "t", "ex:C rdfs:subClassOf ex:B . ex:x a ex:C .");
    let app = router(st.clone());
    // the setting lives with a recorded status
    let r = put_auto(&app, "t", r#"{"enabled":true}"#).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    post_json(&app, "/$/reason/t", r#"{"profile":"rdfs"}"#).await;
    wait_tasks(&st).await;
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(
        s["auto"],
        serde_json::json!({"enabled": false, "source": "server"})
    );

    // off without a server-wide setting: a stale dataset waits
    update(&app, "t", "INSERT DATA { ex:y a ex:C }").await;
    crate::reasoning::auto_reason_tick(&st, Instant::now() + Duration::from_secs(60));
    assert_eq!(st.tasks.lock().len(), 1);

    for bad in [
        r#"{"enabled":"yes"}"#,
        r#"{"enabled":true,"debounceSeconds":-1}"#,
        r#"{"enabled":true,"maxDelaySeconds":"soon"}"#,
        "nope",
    ] {
        assert_eq!(
            put_auto(&app, "t", bad).await.status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    let r = put_auto(&app, "t", r#"{"enabled":true,"debounceSeconds":2}"#).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let auto = &r.json()["auto"];
    assert_eq!(auto["enabled"], true);
    assert_eq!(auto["source"], "dataset");
    assert_eq!(auto["debounceSeconds"], 2.0);
    assert_eq!(auto["maxDelaySeconds"], 24.0);
    let file: J = serde_json::from_slice(
        &std::fs::read(dir.path().join("databases/t/reasoning.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        file["auto"],
        serde_json::json!({"enabled": true, "debounceSeconds": 2.0})
    );

    // the dataset's debounce applies, and the run keeps the setting
    let t0 = Instant::now();
    crate::reasoning::auto_reason_tick(&st, t0);
    crate::reasoning::auto_reason_tick(&st, t0 + Duration::from_secs(1));
    assert_eq!(st.tasks.lock().len(), 1, "within the dataset's debounce");
    crate::reasoning::auto_reason_tick(&st, t0 + Duration::from_secs(3));
    wait_tasks(&st).await;
    assert_eq!(st.tasks.lock().len(), 2);
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(s["stale"], false);
    assert_eq!(s["auto"]["source"], "dataset");

    // the setting survives a restart; switched off, it overrides the server's
    drop(app);
    drop(st);
    let st = open(
        dir.path(),
        Some(AutoReason::new(Duration::ZERO, None)),
        false,
    );
    let app = router(st.clone());
    assert_eq!(
        get_json(&app, "/$/reason/t").await["auto"]["debounceSeconds"],
        2.0
    );
    let r = put_auto(&app, "t", r#"{"enabled":false}"#).await;
    assert_eq!(
        r.json()["auto"],
        serde_json::json!({"enabled": false, "source": "dataset"})
    );
    update(&app, "t", "INSERT DATA { ex:z a ex:C }").await;
    crate::reasoning::auto_reason_tick(&st, Instant::now() + Duration::from_secs(60));
    assert!(st.tasks.lock().is_empty());
    // removed, the server's setting applies again
    let r = send(
        &app,
        Request::delete("/$/reason/t/auto")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["auto"]["source"], "server");
    assert_eq!(r.json()["auto"]["enabled"], true);
    crate::reasoning::auto_reason_tick(&st, Instant::now() + Duration::from_secs(60));
    wait_tasks(&st).await;
    assert_eq!(st.tasks.lock().len(), 1);

    // never on a read-only server
    drop(app);
    drop(st);
    let st = open(dir.path(), Some(AutoReason::per_dataset()), true);
    let r = put_auto(&router(st), "t", r#"{"enabled":true}"#).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_waiting_write_supersedes_an_automatic_run() {
    let dir = tempfile::tempdir().unwrap();
    let st = open(
        dir.path(),
        Some(AutoReason::new(Duration::ZERO, None)),
        false,
    );
    let ds = st.attach("t", DbType::Mem, None).unwrap();
    load(&st, "t", "ex:C0 rdfs:subClassOf ex:C1 .");
    let app = router(st.clone());
    post_json(&app, "/$/reason/t", r#"{"profile":"rdfs"}"#).await;
    wait_tasks(&st).await;
    // enough data that a run takes a while: a long subclass chain with many members
    let mut ttl = String::new();
    for i in 1..100 {
        ttl.push_str(&format!("ex:C{i} rdfs:subClassOf ex:C{} .\n", i + 1));
    }
    for i in 0..1000 {
        ttl.push_str(&format!("ex:i{i} a ex:C0 .\n"));
    }
    load(&st, "t", &ttl);
    crate::reasoning::auto_reason_tick(&st, Instant::now());
    let id = st.tasks.lock()[1].id.clone();
    let task = |st: &AppState| {
        st.tasks
            .lock()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .unwrap()
    };
    // once the run holds the writer lock, a write comes in
    let t0 = Instant::now();
    while !task(&st)
        .message
        .is_some_and(|m| m != "auto: loading triples")
    {
        assert!(t0.elapsed() < Duration::from_secs(30), "{:?}", task(&st));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let store_ds = ds.clone();
    let writer = std::thread::spawn(move || {
        let t = Instant::now();
        let r = sparkles::sparql::update::update(
            &store_ds.store,
            "INSERT DATA { <http://ex.org/w> a <http://ex.org/C0> }",
            &Default::default(),
        )
        .unwrap();
        (r.commit.unwrap().committed, t.elapsed())
    });
    let (committed, waited) = tokio::task::spawn_blocking(move || writer.join().unwrap())
        .await
        .unwrap();
    assert!(committed);
    while task(&st).active() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let t = task(&st);
    assert_eq!(t.state, "cancelled", "{t:?} (the write waited {waited:?})");
    // still stale: the next pass starts a new run
    assert_eq!(get_json(&app, "/$/reason/t").await["stale"], true);
    crate::reasoning::auto_reason_tick(&st, Instant::now());
    assert_eq!(st.tasks.lock().len(), 3);
    let id = st.tasks.lock()[2].id.clone();
    let _ = st.cancel_task(&id);
}

/// A dry run of a write to the default graph leaves the inferences fresh, and gives
/// automatic mode nothing to do (C15 §4.2).
#[tokio::test]
async fn dry_runs_leave_inferences_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let st = open(
        dir.path(),
        Some(AutoReason::new(Duration::ZERO, None)),
        false,
    );
    st.create("t", DbType::Persistent).unwrap();
    load(&st, "t", "ex:C rdfs:subClassOf ex:B . ex:x a ex:C .");
    let app = router(st.clone());
    post_json(&app, "/$/reason/t", r#"{"profile":"rdfs"}"#).await;
    wait_tasks(&st).await;
    let before = get_json(&app, "/$/reason/t").await;
    assert_eq!(before["stale"], false);
    let r = send(
        &app,
        Request::post("/t/update?dryRun=true")
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(Body::from(
                "INSERT DATA { <http://ex.org/w> a <http://ex.org/C> }",
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["wouldCommit"], true);
    let s = get_json(&app, "/$/reason/t").await;
    assert_eq!(
        (&s["stale"], &s["commitsSince"]),
        (&J::Bool(false), &J::from(0))
    );
    assert_eq!(s["head"], before["head"]);
    crate::reasoning::auto_reason_tick(&st, Instant::now() + Duration::from_secs(60));
    assert_eq!(st.tasks.lock().len(), 1);
}

/// The service description names the entailment regime of the materialized
/// inferences, and leaves the default graph's count out, since the inferences are part
/// of it.
#[tokio::test]
async fn the_service_description_names_the_materialized_regime() {
    let dir = tempfile::tempdir().unwrap();
    let st = open(dir.path(), None, false);
    st.create("t", DbType::Persistent).unwrap();
    load(&st, "t", "ex:C rdfs:subClassOf ex:B . ex:x a ex:C .");
    let app = router(st.clone());
    let describe = || async {
        let r = send(
            &app,
            Request::get("/t/sparql")
                .header(header::ACCEPT, "application/n-triples")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r.text()
    };
    let regime = "<http://www.w3.org/ns/sparql-service-description#defaultEntailmentRegime>";
    let nt = describe().await;
    assert!(
        nt.contains(&format!(
            "{regime} <http://www.w3.org/ns/entailment/Simple>"
        )),
        "{nt}"
    );
    assert!(nt.contains("void#triples"), "{nt}");
    let r = post_json(&app, "/$/reason/t", r#"{"profile":"rdfs"}"#).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    wait_tasks(&st).await;
    let nt = describe().await;
    assert!(
        nt.contains(&format!("{regime} <http://www.w3.org/ns/entailment/RDFS>")),
        "{nt}"
    );
    assert!(
        nt.contains("materialized into <urn:x-sparkles:inferred>"),
        "{nt}"
    );
    // the inferred graph is a named graph with a count; the default graph has none
    assert!(
        nt.contains(
            "<http://www.w3.org/ns/sparql-service-description#name> <urn:x-sparkles:inferred>"
        ),
        "{nt}"
    );
    assert_eq!(nt.matches("void#triples").count(), 1, "{nt}");
}
