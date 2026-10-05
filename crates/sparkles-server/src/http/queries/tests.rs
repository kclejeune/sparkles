//! Stored queries over HTTP: definitions, versions, preconditions and runs.

use crate::http::router;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value as J, json};
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const DATA: &str = r#"@prefix ex: <http://ex.org/> .
ex:a ex:name "Ann" ; ex:age 30 ; ex:knows ex:b .
ex:b ex:name "Bob" ; ex:age 17 .
ex:c ex:name "Cy" ; ex:age 45 ; ex:knows ex:a .
"#;

const ADULTS: &str = "PREFIX ex: <http://ex.org/>\nSELECT ?name WHERE { ?p ex:name ?name ; ex:age ?age FILTER(?age >= ?minAge) } ORDER BY ?name";

struct Server {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let ds = state.create("t", DbType::Persistent).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let app = router(state.clone());
    Server {
        _dir: dir,
        state,
        app,
    }
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

impl Resp {
    fn json(&self) -> J {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
    fn header(&self, h: &str) -> String {
        self.headers
            .get(h)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default()
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    Resp {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

async fn put(app: &Router, name: &str, def: J, extra: &[(&str, &str)]) -> Resp {
    let mut r = Request::put(format!("/$/queries/t/{name}"))
        .header(header::CONTENT_TYPE, "application/json");
    for (k, v) in extra {
        r = r.header(*k, *v);
    }
    send(app, r.body(Body::from(def.to_string())).unwrap()).await
}

async fn get(app: &Router, path: &str, accept: &str) -> Resp {
    send(
        app,
        Request::get(path)
            .header(header::ACCEPT, accept)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// The first column of a CSV result, without the header.
fn column(r: &Resp) -> Vec<String> {
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    r.body
        .lines()
        .skip(1)
        .map(|l| l.trim().to_string())
        .collect()
}

fn adults() -> J {
    json!({
        "query": ADULTS,
        "description": "People at least minAge years old",
        "parameters": { "minAge": { "type": "integer", "default": 18, "description": "Youngest age" } }
    })
}

#[tokio::test]
async fn define_and_run() {
    let s = server();
    let r = put(
        &s.app,
        "adults",
        adults(),
        &[("sparkles-commit-message", "first")],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let j = r.json();
    assert_eq!(j["version"]["version"], 1);
    assert_eq!(j["version"]["message"], "first");
    assert_eq!(j["version"]["datasetCommit"], 1);
    assert_eq!(j["kind"], "SELECT");
    assert_eq!(j["changed"], true);
    assert_eq!(r.header("etag"), "\"v1\"");
    // runs, with the default and with a value
    let r = get(&s.app, "/t/queries/adults", "text/csv").await;
    assert_eq!(column(&r), ["Ann", "Cy"]);
    assert_eq!(r.header("sparkles-query-version"), "1");
    assert!(!r.header("sparkles-commit").is_empty());
    let r = get(&s.app, "/t/queries/adults?minAge=40", "text/csv").await;
    assert_eq!(column(&r), ["Cy"]);
    let r = get(&s.app, "/t/queries/adults?%24minAge=0", "text/csv").await;
    assert_eq!(column(&r), ["Ann", "Bob", "Cy"]);
    // SPARQL JSON by default
    let r = get(&s.app, "/t/queries/adults", "*/*").await;
    assert_eq!(r.json()["results"]["bindings"].as_array().unwrap().len(), 2);
    // a form body and a JSON body
    let r = send(
        &s.app,
        Request::post("/t/queries/adults")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ACCEPT, "text/csv")
            .body(Body::from("minAge=40"))
            .unwrap(),
    )
    .await;
    assert_eq!(column(&r), ["Cy"]);
    let r = send(
        &s.app,
        Request::post("/t/queries/adults")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "text/csv")
            .body(Body::from(r#"{"minAge": 20}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(column(&r), ["Ann", "Cy"]);
    // the listing and the definition
    let l = get(&s.app, "/$/queries/t", "*/*").await.json();
    assert_eq!(l["queries"][0]["name"], "adults");
    assert_eq!(l["queries"][0]["parameters"]["minAge"]["type"], "integer");
    assert!(l["queries"][0].get("query").is_none());
    let d = get(&s.app, "/$/queries/t/adults", "*/*").await;
    assert_eq!(d.json()["query"], ADULTS);
    assert_eq!(d.header("etag"), "\"v1\"");
    // the file is in the database directory
    let root = s.state.datasets()["t"].store.root().unwrap().to_path_buf();
    assert!(root.join(sparkles::stored::FILE).exists());
}

#[tokio::test]
async fn values_are_checked_and_never_change_the_query() {
    let s = server();
    put(&s.app, "adults", adults(), &[]).await;
    for (q, want) in [
        ("minAge=old", "parameter 'minAge'"),
        ("minAge=1.5", "parameter 'minAge'"),
        ("nope=1", "unknown parameter 'nope'"),
        ("minAge=1&%24minAge=2", "more than once"),
    ] {
        let r = get(&s.app, &format!("/t/queries/adults?{q}"), "*/*").await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}: {}", r.body);
        assert!(r.body.contains(want), "{q}: {}", r.body);
    }
    let by_name = json!({
        "query": "PREFIX ex: <http://ex.org/>\nSELECT ?p WHERE { ?p ex:name ?name }",
        "parameters": { "name": { "type": "string" } }
    });
    assert_eq!(
        put(&s.app, "byname", by_name, &[]).await.status,
        StatusCode::CREATED
    );
    let r = get(&s.app, "/t/queries/byname?name=Ann", "text/csv").await;
    assert_eq!(column(&r), ["http://ex.org/a"]);
    let evil = "Ann\" } UNION { ?p ?q ?r } #";
    let r = get(
        &s.app,
        &format!(
            "/t/queries/byname?name={}",
            form_urlencoded::byte_serialize(evil.as_bytes()).collect::<String>()
        ),
        "text/csv",
    )
    .await;
    assert!(column(&r).is_empty(), "{}", r.body);
    let r = get(&s.app, "/t/queries/byname", "*/*").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.body.contains("missing parameter 'name'"), "{}", r.body);
    let r = get(&s.app, "/t/queries/missing", "*/*").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn versions_and_preconditions() {
    let s = server();
    let first = put(&s.app, "adults", adults(), &[]).await.json();
    let mut d2 = adults();
    d2["description"] = "Grown-ups".into();
    // only when absent
    let r = put(&s.app, "adults", d2.clone(), &[("if-none-match", "*")]).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED, "{}", r.body);
    let r = put(&s.app, "adults", d2.clone(), &[("if-match", "\"v1\"")]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let j = r.json();
    assert_eq!(j["version"]["version"], 2);
    assert_eq!(j["version"]["parent"], 1);
    assert_ne!(j["version"]["digest"], first["version"]["digest"]);
    // unchanged: no new version
    let r = put(&s.app, "adults", d2.clone(), &[]).await.json();
    assert_eq!(r["changed"], false);
    assert_eq!(r["version"]["version"], 2);
    // a stale tag
    let r = put(&s.app, "adults", adults(), &[("if-match", "\"v1\"")]).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    // a GET answer can be sent back
    let back = get(&s.app, "/$/queries/t/adults", "*/*").await.json();
    let r = put(&s.app, "adults", back, &[]).await.json();
    assert_eq!(r["changed"], false, "{r}");
    let v = get(&s.app, "/$/queries/t/adults/versions", "*/*")
        .await
        .json();
    let nums: Vec<u64> = v["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["version"].as_u64().unwrap())
        .collect();
    assert_eq!(nums, [2, 1]);
    let old = get(&s.app, "/$/queries/t/adults?version=1", "*/*")
        .await
        .json();
    assert_eq!(old["description"], "People at least minAge years old");
    let r = get(&s.app, "/t/queries/adults?version=1&minAge=40", "text/csv").await;
    assert_eq!(r.header("sparkles-query-version"), "1");
    // delete, conditionally
    let r = send(
        &s.app,
        Request::delete("/$/queries/t/adults")
            .header("if-match", "\"v1\"")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    let r = send(
        &s.app,
        Request::delete("/$/queries/t/adults")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        get(&s.app, "/$/queries/t/adults", "*/*").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn definitions_are_checked() {
    let s = server();
    for (def, want) in [
        (
            json!({"query": "INSERT DATA { <a:a> <a:b> <a:c> }"}),
            "updates",
        ),
        (
            json!({"query": "SELECT ?x WHERE { BIND(1 AS ?x) }", "parameters": {"x": {"type": "integer"}}}),
            "assigns",
        ),
        (
            json!({"query": ADULTS, "parameters": {"minAge": {"type": "number"}}}),
            "invalid definition",
        ),
        (json!({"query": ADULTS, "extra": 1}), "invalid definition"),
        (json!({"query": ADULTS, "results": "turtle"}), "results"),
    ] {
        let r = put(&s.app, "q", def, &[]).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{want}: {}", r.body);
        assert!(r.body.contains(want), "{want}: {}", r.body);
    }
    let r = put(&s.app, "bad%20name", adults(), &[]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
}

#[tokio::test]
async fn the_definition_names_the_default_format() {
    let s = server();
    let mut d = adults();
    d["results"] = "csv".into();
    put(&s.app, "adults", d, &[]).await;
    let r = get(&s.app, "/t/queries/adults", "*/*").await;
    assert!(
        r.header("content-type").starts_with("text/csv"),
        "{}",
        r.header("content-type")
    );
    let r = get(
        &s.app,
        "/t/queries/adults",
        "application/sparql-results+json",
    )
    .await;
    assert!(r.header("content-type").contains("json"));
    let r = get(&s.app, "/t/queries/adults?format=tsv", "*/*").await;
    assert!(
        r.header("content-type")
            .starts_with("text/tab-separated-values")
    );
    // a graph query
    let g = json!({
        "query": "PREFIX ex: <http://ex.org/>\nCONSTRUCT { ?p ex:knows ?q } WHERE { ?p ex:knows ?q FILTER(?p = ?who) }",
        "parameters": { "who": { "type": "iri" } },
        "results": "ntriples"
    });
    assert_eq!(
        put(&s.app, "knows", g, &[]).await.status,
        StatusCode::CREATED
    );
    let r = get(&s.app, "/t/queries/knows?who=ex:a", "*/*").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        r.body.trim(),
        "<http://ex.org/a> <http://ex.org/knows> <http://ex.org/b> ."
    );
}

#[tokio::test]
async fn clones_keep_the_queries() {
    let s = server();
    put(&s.app, "adults", adults(), &[]).await;
    let ds = s.state.datasets()["t"].clone();
    let out = tempfile::tempdir().unwrap();
    let dir = out.path().join("copy");
    ds.store
        .clone_to(&dir, &sparkles::store::CloneOptions::default())
        .unwrap();
    let copy = sparkles::stored::Catalog::open(Some(&dir)).unwrap();
    assert_eq!(copy.list()[0].0, "adults");
}
