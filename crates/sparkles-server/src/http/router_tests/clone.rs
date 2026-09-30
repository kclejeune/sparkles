//! Cloning a dataset into a new persistent dataset (`POST /$/datasets/{ds}/clone`).

use super::*;
use std::path::Path;
use std::time::Instant;

const DATA: &str = "@prefix ex: <http://ex.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:C rdfs:subClassOf ex:B . ex:x a ex:C . ex:x ex:p _:b .
GRAPH ex:g { _:b ex:q 1 . ex:y ex:q 2 }
";

fn open(dir: &Path, read_only: bool) -> Arc<AppState> {
    let mut st = AppState::new(dir, StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.read_only = read_only;
    Arc::new(st)
}

/// A server with the persistent dataset `prod` (5 quads).
fn prod(dir: &Path) -> Arc<AppState> {
    let st = open(dir, false);
    let ds = st.create("prod", DbType::Persistent).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    st
}

async fn wait_task(st: &AppState, id: &str) -> crate::state::Task {
    let t0 = Instant::now();
    loop {
        let t = st
            .tasks
            .lock()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .unwrap();
        if t.state != "running" {
            return t;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "task {id} did not finish"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn post(app: &Router, path: &str) -> (Resp, axum::http::HeaderMap) {
    send_h(app, Request::post(path).body(Body::empty()).unwrap()).await
}

async fn info(app: &Router, name: &str) -> Resp {
    send(
        app,
        Request::get(format!("/$/datasets/{name}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// `COUNT(*)` of a pattern; `ds` may carry protocol parameters (`ds?reasoning=false`).
async fn count(app: &Router, ds: &str, q: &str) -> u64 {
    let (ds, params) = ds.split_once('?').unwrap_or((ds, ""));
    let r = send(
        app,
        Request::post(format!("/{ds}/sparql?{params}"))
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .body(Body::from(format!("SELECT (COUNT(*) AS ?n) {{ {q} }}")))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["results"]["bindings"][0]["n"]["value"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

const ALL: &str = "{ ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } }";

#[tokio::test]
async fn clone_a_dataset() {
    let dir = tempfile::tempdir().unwrap();
    let st = prod(dir.path());
    let app = router(st.clone());
    let (r, h) = post(&app, "/$/datasets/prod/clone?name=prod-sandbox").await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    assert_eq!(h["location"], "/$/datasets/prod-sandbox");
    let task = r.json();
    assert_eq!(task["kind"], "clone");
    assert_eq!(task["dataset"], "prod");
    assert_eq!(task["target"], "prod-sandbox");
    let t = wait_task(&st, task["id"].as_str().unwrap()).await;
    assert_eq!(t.state, "done", "{t:?}");
    assert_eq!(
        t.message.as_deref(),
        Some("cloned /prod at commit 1 (5 quads) into /prod-sandbox")
    );

    let src = info(&app, "prod").await.json();
    let c = info(&app, "prod-sandbox").await;
    assert_eq!(c.status, StatusCode::OK);
    let c = c.json();
    assert_eq!(c["quads"], 5);
    assert_eq!(c["type"], "persistent");
    assert_eq!(c["head"], 0);
    assert_ne!(c["id"], src["id"]);
    assert_eq!(c["forkedFrom"]["id"], src["id"]);
    assert_eq!(c["forkedFrom"]["seq"], 1);
    assert_eq!(c["origin"]["originFormat"], 1);
    assert_eq!(c["origin"]["source"]["name"], "prod");
    assert_eq!(c["origin"]["source"]["quads"], 5);
    assert_eq!(c["origin"]["inferences"], "copy");
    assert!(src.get("origin").is_none() && src.get("forkedFrom").is_none());

    // blank nodes keep their labels
    let bnode = |app: Router, ds: &'static str| async move {
        let r = send(
            &app,
            Request::post(format!("/{ds}/sparql"))
                .header(header::CONTENT_TYPE, "application/sparql-query")
                .body(Body::from(
                    "SELECT ?b { <http://ex.org/x> <http://ex.org/p> ?b }",
                ))
                .unwrap(),
        )
        .await;
        r.json()["results"]["bindings"][0]["b"]["value"].clone()
    };
    assert_eq!(
        bnode(app.clone(), "prod").await,
        bnode(app.clone(), "prod-sandbox").await
    );

    // independent: writes to the clone leave the source alone
    let r = send(
        &app,
        Request::post("/prod-sandbox/update")
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(Body::from("INSERT DATA { <urn:a> <urn:b> <urn:c> }"))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(count(&app, "prod-sandbox", ALL).await, 6);
    assert_eq!(count(&app, "prod", ALL).await, 5);

    // registered and persisted
    drop(app);
    drop(st);
    let st = open(dir.path(), false);
    let app = router(st);
    assert_eq!(count(&app, "prod-sandbox", ALL).await, 6);
    assert_eq!(
        info(&app, "prod-sandbox").await.json()["origin"]["source"]["name"],
        "prod"
    );
}

#[tokio::test]
async fn clone_names_conflict_and_are_validated() {
    let dir = tempfile::tempdir().unwrap();
    let st = prod(dir.path());
    st.attach("mem", DbType::Mem, None).unwrap();
    let app = router(st.clone());
    for (path, status) in [
        ("/$/datasets/prod/clone?name=mem", StatusCode::CONFLICT),
        ("/$/datasets/prod/clone?name=prod", StatusCode::CONFLICT),
        (
            "/$/datasets/prod/clone?name=.hidden",
            StatusCode::BAD_REQUEST,
        ),
        ("/$/datasets/prod/clone?name=a%2Fb", StatusCode::BAD_REQUEST),
        ("/$/datasets/prod/clone", StatusCode::BAD_REQUEST),
        (
            "/$/datasets/prod/clone?name=x&inferences=keep",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/$/datasets/prod/clone?name=x&type=mem",
            StatusCode::BAD_REQUEST,
        ),
        ("/$/datasets/nope/clone?name=x", StatusCode::NOT_FOUND),
    ] {
        let (r, _) = post(&app, path).await;
        assert_eq!(r.status, status, "{path}: {}", r.text());
    }

    // a name being created by a running task
    let held = st.reserve("busy", "42").unwrap();
    let (r, _) = post(&app, "/$/datasets/prod/clone?name=busy").await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(
        r.json()["error"],
        "dataset /busy is being created by task 42"
    );
    let r = send(
        &app,
        Request::post("/$/datasets")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"dbName":"busy","dbType":"mem"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(info(&app, "busy").await.status, StatusCode::NOT_FOUND);
    drop(held);
    assert!(st.reserved_by("busy").is_none());

    // a directory that is not a registered dataset is never adopted
    std::fs::create_dir(dir.path().join("databases/ghost")).unwrap();
    let (r, _) = post(&app, "/$/datasets/prod/clone?name=ghost").await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("exists but is not a registered dataset")
    );

    // JSON body
    let r = send(
        &app,
        Request::post("/$/datasets/prod/clone")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"name":"from-json","inferences":"drop"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let t = wait_task(&st, r.json()["id"].as_str().unwrap()).await;
    assert_eq!(t.state, "done", "{t:?}");
    assert_eq!(
        info(&app, "from-json").await.json()["origin"]["inferences"],
        "drop"
    );

    // read-only servers refuse
    let ro = open(tempfile::tempdir().unwrap().path(), true);
    ro.attach("m", DbType::Mem, None).unwrap();
    let (r, _) = post(&router(ro), "/$/datasets/m/clone?name=x").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn failed_clones_leave_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let st = prod(dir.path());
    let app = router(st.clone());
    let name = crate::clone::FAIL_BEFORE_RENAME;
    let (r, _) = post(&app, &format!("/$/datasets/prod/clone?name={name}")).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let t = wait_task(&st, r.json()["id"].as_str().unwrap()).await;
    assert_eq!(t.state, "failed");
    let left: Vec<String> = std::fs::read_dir(dir.path().join("databases"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["prod"]);
    assert!(st.reserved_by(name).is_none(), "the name is released");
    assert_eq!(info(&app, name).await.status, StatusCode::NOT_FOUND);
    drop(app);
    drop(st);

    // unfinished clones are swept at startup
    let orphan = dir.path().join("databases/.clone-x-9");
    std::fs::create_dir_all(orphan.join("gen-0001")).unwrap();
    let st = open(dir.path(), false);
    assert!(!orphan.exists());
    assert_eq!(st.datasets.read().len(), 1);
}

#[tokio::test]
async fn cancelled_clones_leave_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let st = prod(dir.path());
    let app = router(st.clone());
    // clone tasks accept cancellation
    let (r, _) = post(&app, "/$/datasets/prod/clone?name=c1").await;
    assert_eq!(r.json()["cancellable"], true);
    let t = wait_task(&st, r.json()["id"].as_str().unwrap()).await;
    assert_eq!(t.state, "done");
    assert!(!t.cancellable);

    // a cancelled clone stops before the rename
    let src = st.get("prod").unwrap();
    let databases = dir.path().join("databases");
    let (tmp, dst) = (databases.join(".clone-c2-1"), databases.join("c2"));
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let e = crate::clone::clone_into(
        &src.store,
        "prod",
        None,
        &tmp,
        &dst,
        crate::clone::Inferences::Copy,
        None,
        Some(cancel),
    )
    .unwrap_err();
    assert!(matches!(
        e.downcast_ref::<sparkles::Error>(),
        Some(sparkles::Error::Cancelled)
    ));
    assert!(!tmp.exists() && !dst.exists());
}

#[cfg(feature = "reasoning")]
#[tokio::test]
async fn clones_copy_or_drop_the_inferences() {
    let dir = tempfile::tempdir().unwrap();
    let st = prod(dir.path());
    let app = router(st.clone());
    let r = send(
        &app,
        Request::post("/$/reason/prod")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"profile":"rdfs"}"#))
            .unwrap(),
    )
    .await;
    let t = wait_task(&st, r.json()["id"].as_str().unwrap()).await;
    assert_eq!(t.state, "done");
    let inferred = "GRAPH <urn:x-sparkles:inferred> { ?s ?p ?o }";
    let n_inf = count(&app, "prod", inferred).await;
    assert!(n_inf > 0);
    // the stored quads (queries also see the inferences in the default graph)
    let total = count(&app, "prod?reasoning=false", ALL).await;

    for (name, mode) in [("with", "copy"), ("without", "drop")] {
        let (r, _) = post(
            &app,
            &format!("/$/datasets/prod/clone?name={name}&inferences={mode}"),
        )
        .await;
        let t = wait_task(&st, r.json()["id"].as_str().unwrap()).await;
        assert_eq!(t.state, "done", "{t:?}");
    }
    // copied: fresh at the clone's root commit
    let c = info(&app, "with").await.json();
    assert_eq!(c["reasoning"]["profile"], "rdfs");
    assert_eq!(c["reasoning"]["stale"], false);
    assert_eq!(c["reasoning"]["commit"], 0);
    assert_eq!(count(&app, "with", "?s a <http://ex.org/B>").await, 1);
    // dropped: no status, no inferred graph
    let c = info(&app, "without").await.json();
    assert_eq!(c["reasoning"], J::Null);
    assert_eq!(count(&app, "without", inferred).await, 0);
    assert_eq!(count(&app, "without", ALL).await, total - n_inf);

    // stale at the source: stale in the clone, for a reason it can tell
    let r = send(
        &app,
        Request::post("/prod/update")
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(Body::from("INSERT DATA { <urn:a> <urn:b> <urn:c> }"))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let (r, _) = post(&app, "/$/datasets/prod/clone?name=stale").await;
    wait_task(&st, r.json()["id"].as_str().unwrap()).await;
    let s = send(
        &app,
        Request::get("/$/reason/stale").body(Body::empty()).unwrap(),
    )
    .await
    .json();
    assert_eq!(s["stale"], true);
    assert_eq!(s["commitsSince"], J::Null);
    assert_eq!(s["staleReason"], "inherited from source at clone time");
}
