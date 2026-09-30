//! In-process tests of the HTTP router (SHACL endpoint, result cache controls).

use super::router;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::Value as J;
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

#[cfg(feature = "reasoning")]
mod reasoning;

const DATA: &str = r#"
@prefix ex: <http://example.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

ex:Student rdfs:subClassOf ex:Person .
ex:worksFor rdfs:domain ex:Employee .
ex:alice a ex:Student ; foaf:name "Alice" ; foaf:age 30 .
ex:bob a ex:Person ; foaf:name "Bob" ; foaf:age 200 .
ex:carol ex:worksFor ex:acme .

GRAPH ex:g1 {
  ex:dave a ex:Person ; foaf:age "unknown" .
}
"#;

#[cfg(feature = "shacl")]
const PERSON_SHAPES: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix ex: <http://example.org/> .

ex:PersonShape a sh:NodeShape ;
  sh:targetClass ex:Person ;
  sh:property [ sh:path foaf:name ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ;
  sh:property [ sh:path foaf:age ; sh:maxCount 1 ; sh:datatype xsd:integer ;
                sh:minInclusive 0 ; sh:maxInclusive 150 ] .
"#;

/// Conforms on the default graph; `ex:dave` in `ex:g1` has no name.
#[cfg(feature = "shacl")]
const NAME_SHAPES: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix ex: <http://example.org/> .
ex:NameShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path foaf:name ; sh:minCount 1 ] .
"#;

/// Everyone who works for something must be an `ex:Employee`: only true with RDFS
/// inferences (`rdfs:domain`).
#[cfg(all(feature = "reasoning", feature = "shacl"))]
const EMPLOYEE_SHAPES: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ex: <http://example.org/> .
ex:EmployeeShape a sh:NodeShape ; sh:targetSubjectsOf ex:worksFor ; sh:class ex:Employee .
"#;

struct Server {
    _dir: tempfile::TempDir,
    #[cfg_attr(not(all(feature = "reasoning", feature = "shacl")), allow(dead_code))]
    state: Arc<AppState>,
    app: Router,
}

fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let opts = StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    };
    let state = Arc::new(AppState::new(dir.path(), opts, Duration::from_secs(30)).unwrap());
    let ds = state.attach("ds", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
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
    #[cfg_attr(not(feature = "shacl"), allow(dead_code))]
    content_type: String,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> J {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Resp {
        status,
        content_type,
        body,
    }
}

#[cfg(feature = "shacl")]
async fn validate(app: &Router, query: &str, shapes: &str, accept: Option<&str>) -> Resp {
    let mut req =
        Request::post(format!("/ds/shacl{query}")).header(header::CONTENT_TYPE, "text/turtle");
    if let Some(a) = accept {
        req = req.header(header::ACCEPT, a);
    }
    send(app, req.body(Body::from(shapes.to_string())).unwrap()).await
}

#[cfg(feature = "shacl")]
async fn validate_json(app: &Router, query: &str, shapes: &str) -> J {
    let r = validate(app, query, shapes, Some("application/json")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.content_type, "application/json");
    r.json()
}

#[cfg(feature = "shacl")]
fn focus_nodes(report: &J) -> Vec<String> {
    let mut v: Vec<String> = report["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["focusNode"]["value"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v.dedup();
    v
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn shacl_conforming_and_json_report() {
    let s = server();
    let ok = validate_json(&s.app, "", NAME_SHAPES).await;
    assert_eq!(ok["conforms"], true);
    assert_eq!(ok["results"].as_array().unwrap().len(), 0);

    let r = validate_json(&s.app, "?graph=default", PERSON_SHAPES).await;
    assert_eq!(r["conforms"], false);
    let results = r["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{r:#}");
    let v = &results[0];
    assert_eq!(
        v["focusNode"],
        serde_json::json!({"type": "uri", "value": "http://example.org/bob"})
    );
    assert_eq!(v["resultPath"]["type"], "uri");
    assert_eq!(v["resultPath"]["value"], "http://xmlns.com/foaf/0.1/age");
    assert_eq!(v["value"]["value"], "200");
    assert_eq!(
        v["value"]["datatype"],
        "http://www.w3.org/2001/XMLSchema#integer"
    );
    assert_eq!(v["sourceShape"]["type"], "bnode");
    assert_eq!(
        v["sourceConstraintComponent"]["value"],
        "http://www.w3.org/ns/shacl#MaxInclusiveConstraintComponent"
    );
    assert_eq!(
        v["severity"]["value"],
        "http://www.w3.org/ns/shacl#Violation"
    );
    assert!(!v["messages"].as_array().unwrap().is_empty());
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn shacl_rdf_reports() {
    let s = server();
    // Turtle is the default
    let r = validate(&s.app, "", PERSON_SHAPES, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.content_type.starts_with("text/turtle"),
        "{}",
        r.content_type
    );
    let mut g = oxrdf::Graph::new();
    for t in oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::Turtle).for_slice(&r.body) {
        let q = t.unwrap();
        g.insert(oxrdf::TripleRef::new(&q.subject, &q.predicate, &q.object));
    }
    let report = sparkles_shacl::ValidationReport::from_rdf(&g, None).unwrap();
    assert!(!report.conforms);
    assert_eq!(report.results.len(), 1);
    assert_eq!(
        report.results[0].focus_node.to_string(),
        "<http://example.org/bob>"
    );

    let r = validate(&s.app, "", NAME_SHAPES, Some("text/turtle")).await;
    assert!(r.text().contains("sh:conforms true"), "{}", r.text());

    let r = validate(&s.app, "", PERSON_SHAPES, Some("application/n-triples")).await;
    assert_eq!(r.content_type, "application/n-triples");
    assert!(
        r.text()
            .contains("<http://www.w3.org/ns/shacl#focusNode> <http://example.org/bob>")
    );

    let r = validate(&s.app, "", PERSON_SHAPES, Some("application/ld+json")).await;
    assert_eq!(r.content_type, "application/ld+json");
    assert!(
        r.text()
            .contains("http://www.w3.org/ns/shacl#ValidationReport")
    );

    // format= overrides Accept
    let r = validate(&s.app, "?format=json", PERSON_SHAPES, Some("text/turtle")).await;
    assert_eq!(r.content_type, "application/json");
    assert_eq!(r.json()["conforms"], false);
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn shacl_graph_selection() {
    let s = server();
    let union = validate_json(&s.app, "?graph=union", NAME_SHAPES).await;
    assert_eq!(union["conforms"], false);
    assert_eq!(focus_nodes(&union), ["http://example.org/dave"]);

    // the same via the Jena special graph IRI
    let q = format!(
        "?graph={}",
        percent_encoding::utf8_percent_encode(
            "urn:x-arq:UnionGraph",
            percent_encoding::NON_ALPHANUMERIC
        )
    );
    let special = validate_json(&s.app, &q, NAME_SHAPES).await;
    assert_eq!(focus_nodes(&special), focus_nodes(&union));
    assert_eq!(special["results"].as_array().unwrap().len(), 1);

    let union = validate_json(&s.app, "?graph=union", PERSON_SHAPES).await;
    assert_eq!(
        focus_nodes(&union),
        ["http://example.org/bob", "http://example.org/dave"]
    );

    // a named graph on its own: dave is a Person there
    let g1 = validate_json(
        &s.app,
        "?graph=http%3A%2F%2Fexample.org%2Fg1",
        PERSON_SHAPES,
    )
    .await;
    assert_eq!(focus_nodes(&g1), ["http://example.org/dave"]);
    let comps: Vec<&str> = g1["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["sourceConstraintComponent"]["value"].as_str().unwrap())
        .collect();
    assert!(comps.contains(&"http://www.w3.org/ns/shacl#MinCountConstraintComponent"));
    assert!(comps.contains(&"http://www.w3.org/ns/shacl#DatatypeConstraintComponent"));

    let missing = validate(
        &s.app,
        "?graph=http%3A%2F%2Fexample.org%2Fnope",
        NAME_SHAPES,
        None,
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let bad = validate(&s.app, "?graph=not%20an%20iri", NAME_SHAPES, None).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn shacl_errors() {
    let s = server();
    let r = validate(&s.app, "", "this is not turtle", None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.json()["error"].as_str().unwrap().contains("shapes"));

    let r = send(
        &s.app,
        Request::post("/nope/shacl")
            .body(Body::from(NAME_SHAPES))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = send(
        &s.app,
        Request::get("/ds/shacl").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);

    // any non-RDF content type (curl's default form encoding) means Turtle
    let r = send(
        &s.app,
        Request::post("/ds/shacl")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ACCEPT, "application/json")
            .body(Body::from(NAME_SHAPES))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["conforms"], true);

    // N-Triples shapes, no Content-Type charset issues
    let nt = "<http://example.org/S> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/ns/shacl#NodeShape> .\n\
              <http://example.org/S> <http://www.w3.org/ns/shacl#targetNode> <http://example.org/alice> .\n\
              <http://example.org/S> <http://www.w3.org/ns/shacl#class> <http://example.org/Robot> .\n";
    let r = send(
        &s.app,
        Request::post("/ds/shacl")
            .header(header::CONTENT_TYPE, "application/n-triples; charset=utf-8")
            .header(header::ACCEPT, "application/json")
            .body(Body::from(nt))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(focus_nodes(&r.json()), ["http://example.org/alice"]);
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn dataset_info_lists_shacl_endpoint() {
    let s = server();
    let r = send(
        &s.app,
        Request::get("/$/datasets/ds").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["endpoints"]["shacl"], "/ds/shacl");
}

#[cfg(all(feature = "reasoning", feature = "shacl"))]
#[tokio::test]
async fn shacl_with_inferences() {
    let s = server();
    // without materialized inferences carol is not an Employee
    let r = validate_json(&s.app, "", EMPLOYEE_SHAPES).await;
    assert_eq!(focus_nodes(&r), ["http://example.org/carol"]);

    let ds = s.state.get("ds").unwrap();
    let rep = sparkles_reasoner::materialize(
        &ds.store,
        &sparkles_reasoner::Profile::Rdfs,
        &Default::default(),
    )
    .unwrap();
    assert!(rep.inferred > 0);
    *ds.reasoning.write() = Some(crate::reasoning::recorded(
        &sparkles_reasoner::Profile::Rdfs,
        &rep,
        &ds.store,
    ));

    // data ∪ inferred by default
    for q in ["", "?graph=default", "?reasoning=true", "?graph=union"] {
        let r = validate_json(&s.app, q, EMPLOYEE_SHAPES).await;
        assert_eq!(r["conforms"], true, "{q}: {r:#}");
    }
    // reasoning=false leaves the inferred graph out, also of the union of all graphs
    for q in ["?reasoning=false", "?graph=union&reasoning=false"] {
        let r = validate_json(&s.app, q, EMPLOYEE_SHAPES).await;
        assert_eq!(focus_nodes(&r), ["http://example.org/carol"], "{q}");
    }
    // the union without inferences still has the named graphs
    let r = validate_json(&s.app, "?graph=union&reasoning=false", NAME_SHAPES).await;
    assert_eq!(focus_nodes(&r), ["http://example.org/dave"]);
}

// ------------------------------------------------------------- result cache ------

async fn stats(app: &Router) -> J {
    let r = send(
        app,
        Request::get("/$/stats/ds").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    r.json()["resultCache"].clone()
}

async fn query(app: &Router, params: &str) -> J {
    let q = "PREFIX foaf: <http://xmlns.com/foaf/0.1/> \
             SELECT ?t (COUNT(?p) AS ?c) WHERE { ?p a ?t . ?p foaf:name ?n } GROUP BY ?t";
    let r = send(
        app,
        Request::post(format!("/ds/sparql{params}"))
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .header(header::ACCEPT, "application/x-sparkles+json")
            .body(Body::from(q))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

fn plan_cached(p: &J) -> bool {
    p["cached"] == true
        || p["children"]
            .as_array()
            .is_some_and(|c| c.iter().any(plan_cached))
}

#[tokio::test]
async fn result_cache_nocache_and_clear() {
    let s = server();
    let c0 = stats(&s.app).await;
    assert_eq!(c0["enabled"], true);
    assert_eq!(c0["entries"], 0);

    let a = query(&s.app, "").await;
    assert!(!plan_cached(&a["meta"]["plan"]));
    let c1 = stats(&s.app).await;
    assert!(c1["entries"].as_u64().unwrap() > 0, "{c1}");
    assert!(c1["bytes"].as_u64().unwrap() > 0, "{c1}");

    let b = query(&s.app, "").await;
    assert!(plan_cached(&b["meta"]["plan"]));
    assert_eq!(a["rows"], b["rows"]);
    let c2 = stats(&s.app).await;
    assert!(c2["hits"].as_u64().unwrap() > c1["hits"].as_u64().unwrap());

    // nocache=true neither reads nor fills the cache
    let n = query(&s.app, "?nocache=true").await;
    assert!(!plan_cached(&n["meta"]["plan"]));
    assert_eq!(n["rows"], a["rows"]);
    let c3 = stats(&s.app).await;
    assert_eq!(
        (c3["hits"].clone(), c3["misses"].clone()),
        (c2["hits"].clone(), c2["misses"].clone())
    );

    let r = send(
        &s.app,
        Request::post("/$/cache/clear/ds")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["cleared"], c3["entries"]);
    assert_eq!(stats(&s.app).await["entries"], 0);
    let d = query(&s.app, "").await;
    assert!(!plan_cached(&d["meta"]["plan"]));

    let r = send(
        &s.app,
        Request::post("/$/cache/clear/nope")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------------------
// regressions from the implementation review
// ---------------------------------------------------------------------------------------

async fn select_rows(app: &Router, query: &str) -> usize {
    let r = send(
        app,
        Request::post("/ds/sparql")
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .header(header::ACCEPT, "application/sparql-results+json")
            .body(Body::from(query.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["results"]["bindings"].as_array().unwrap().len()
}

#[tokio::test]
async fn graph_store_put_keeps_the_graph_on_invalid_input() {
    let s = server();
    let put = |body: &'static str| {
        Request::put("/ds/data?graph=http://example.org/g1")
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(body))
            .unwrap()
    };
    let count = "SELECT * WHERE { GRAPH <http://example.org/g1> { ?s ?p ?o } }";
    assert_eq!(select_rows(&s.app, count).await, 2);
    let r = send(
        &s.app,
        put("<http://example.org/n> <http://example.org/p> 1 . this is not turtle"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(
        select_rows(&s.app, count).await,
        2,
        "graph must be unchanged"
    );
    let r = send(
        &s.app,
        put("<http://example.org/n> <http://example.org/p> 1 ."),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(select_rows(&s.app, count).await, 1);
}

#[tokio::test]
async fn read_only_server_rejects_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let mut state =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    state.read_only = true;
    let state = Arc::new(state);
    state.attach("ds", DbType::Mem, None).unwrap();
    let app = router(state.clone());
    let r = send(
        &app,
        Request::post("/$/compact/ds").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(state.tasks.lock().is_empty(), "no task may be scheduled");
}

#[test]
fn concurrent_dataset_management_is_serialized() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    // distinct names: every creation reported successful survives a restart
    let handles: Vec<_> = (0..30)
        .map(|i| {
            let st = state.clone();
            std::thread::spawn(move || st.create(&format!("d{i}"), DbType::Mem).is_ok())
        })
        .collect();
    assert!(handles.into_iter().all(|h| h.join().unwrap()));
    // the same name: exactly one creation wins
    let handles: Vec<_> = (0..12)
        .map(|_| {
            let st = state.clone();
            std::thread::spawn(move || st.create("same", DbType::Mem).is_ok())
        })
        .collect();
    let won = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|ok| *ok)
        .count();
    assert_eq!(won, 1);
    drop(state);
    let reopened =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    let names: Vec<String> = reopened.datasets.read().keys().cloned().collect();
    assert_eq!(names.len(), 31, "{names:?}");
}

// ------------------------------------------------------------------ commits ------

/// Send a request and return the response with its headers.
async fn send_h(app: &Router, req: Request<Body>) -> (Resp, axum::http::HeaderMap) {
    let res = app.clone().oneshot(req).await.unwrap();
    let headers = res.headers().clone();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    (
        Resp {
            status,
            content_type,
            body,
        },
        headers,
    )
}

fn commit_header(h: &axum::http::HeaderMap) -> u64 {
    h.get("sparkles-commit")
        .expect("Sparkles-Commit header")
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn sparql_update(
    app: &Router,
    text: &str,
    accept: Option<&str>,
) -> (Resp, axum::http::HeaderMap) {
    let mut req =
        Request::post("/ds/update").header(header::CONTENT_TYPE, "application/sparql-update");
    if let Some(a) = accept {
        req = req.header(header::ACCEPT, a);
    }
    send_h(app, req.body(Body::from(text.to_string())).unwrap()).await
}

#[tokio::test]
async fn writes_return_commits_and_reads_name_them() {
    let s = server();
    // the fixture's bulk load into the empty in-memory dataset was commit 1
    let list = send(
        &s.app,
        Request::get("/$/commits/ds").body(Body::empty()).unwrap(),
    )
    .await
    .json();
    assert_eq!(list["head"], 1);
    assert_eq!(list["commits"][0]["kind"], "load");
    assert_eq!(list["commits"][0]["bulk"], true);
    assert_eq!(list["commits"][1]["kind"], "create");
    assert_eq!(list["commits"][1]["parent"], J::Null);
    let id = list["datasetId"].as_str().unwrap().to_string();

    // a receipt when asked for
    let (r, h) = sparql_update(
        &s.app,
        "INSERT DATA { <urn:a> <urn:p> 1 . <urn:b> <urn:p> 2 }",
        Some("application/x-sparkles+json"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(commit_header(&h), 2);
    assert_eq!(h["sparkles-dataset-id"], id.as_str());
    let j = r.json();
    assert_eq!(
        (j["inserted"].as_u64(), j["operations"].as_u64()),
        (Some(2), Some(1))
    );
    assert_eq!(j["committed"], true);
    assert_eq!(j["dataset"], "ds");
    assert_eq!(j["commit"]["seq"], 2);
    assert_eq!(j["commit"]["parent"], 1);
    assert_eq!(j["commit"]["kind"], "update");
    assert_eq!(j["commit"]["inserted"], 2);

    // the default body is unchanged
    let (r, h) = sparql_update(&s.app, "INSERT DATA { <urn:c> <urn:p> 3 }", None).await;
    assert_eq!(r.content_type, "application/json");
    let mut keys: Vec<String> = r.json().as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["deleted", "inserted", "operations", "timing"]);
    assert_eq!(commit_header(&h), 3);

    // no change: no commit
    let (r, h) = sparql_update(
        &s.app,
        "INSERT DATA { <urn:a> <urn:p> 1 }",
        Some("application/x-sparkles+json"),
    )
    .await;
    assert_eq!(r.json()["committed"], false);
    assert_eq!(r.json()["commit"]["seq"], 3);
    assert_eq!(commit_header(&h), 3);

    // Graph Store Protocol
    let (r, h) = send_h(
        &s.app,
        Request::put("/ds/data?graph=urn:g&receipt=true")
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from("<urn:s> <urn:p> 1, 2, 3 ."))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["count"], 3);
    assert_eq!(j["commit"]["kind"], "gsp-put");
    assert_eq!(j["commit"]["seq"], 4);
    assert_eq!(commit_header(&h), 4);
    let (r, h) = send_h(
        &s.app,
        Request::delete("/ds/data?graph=urn:g")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(commit_header(&h), 5);
    assert_eq!(
        s.state.get("ds").unwrap().store.head_commit().kind.name(),
        "gsp-delete"
    );

    // reads carry the commit they were evaluated against
    let (_, h) = send_h(
        &s.app,
        Request::get("/ds/sparql?query=ASK%7B%7D")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(commit_header(&h), 5);
    assert_eq!(h["sparkles-dataset-id"], id.as_str());
    let (_, h) = send_h(
        &s.app,
        Request::get("/ds/sparql?query=ASK%7B%7D")
            .header(header::ORIGIN, "http://elsewhere.example")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let exposed = h[header::ACCESS_CONTROL_EXPOSE_HEADERS]
        .to_str()
        .unwrap()
        .to_lowercase();
    assert!(
        exposed.contains("sparkles-commit") && exposed.contains("sparkles-dataset-id"),
        "{exposed}"
    );

    // dataset info
    let info = send(
        &s.app,
        Request::get("/$/datasets/ds").body(Body::empty()).unwrap(),
    )
    .await
    .json();
    assert_eq!(info["head"], 5);
    assert_eq!(info["id"], id.as_str());
}

#[tokio::test]
async fn the_commit_catalog_pages_and_rejects_bad_ranges() {
    let s = server();
    for i in 0..7 {
        sparql_update(
            &s.app,
            &format!("INSERT DATA {{ <urn:x{i}> <urn:p> {i} }}"),
            None,
        )
        .await;
    }
    // head is 8 (root 0, fixture load 1, then 2..8)
    let get = |q: &str| {
        Request::get(format!("/$/commits/ds{q}"))
            .body(Body::empty())
            .unwrap()
    };
    let seqs = |j: &J| -> Vec<u64> {
        j["commits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["seq"].as_u64().unwrap())
            .collect()
    };
    let j = send(&s.app, get("?limit=3")).await.json();
    assert_eq!(seqs(&j), [8, 7, 6]);
    assert_eq!(j["next"], "/$/commits/ds?before=6&limit=3");
    assert_eq!(
        seqs(&send(&s.app, get("?before=6&limit=3")).await.json()),
        [5, 4, 3]
    );
    assert_eq!(
        seqs(&send(&s.app, get("?after=5&limit=2")).await.json()),
        [6, 7]
    );
    let last = send(&s.app, get("?before=2&limit=5")).await.json();
    assert_eq!(seqs(&last), [1, 0]);
    assert_eq!(last["next"], J::Null);
    for bad in ["?before=x", "?before=3&after=1", "?limit=0"] {
        assert_eq!(
            send(&s.app, get(bad)).await.status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    let nope = Request::get("/$/commits/nope").body(Body::empty()).unwrap();
    assert_eq!(send(&s.app, nope).await.status, StatusCode::NOT_FOUND);
    let r = send(&s.app, get("/99")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(
        r.text().contains("no commit 99 in dataset ds (head is 8)"),
        "{}",
        r.text()
    );
    assert_eq!(
        send(&s.app, get("/commit:2")).await.json()["commit"]["seq"],
        2
    );
    assert_eq!(send(&s.app, get("/head")).await.json()["commit"]["seq"], 8);
    assert_eq!(
        send(&s.app, get("/x")).await.status,
        StatusCode::BAD_REQUEST
    );
}
