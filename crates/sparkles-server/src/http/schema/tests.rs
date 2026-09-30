//! In-process tests of the schema discovery endpoints.

use crate::http::router;
use crate::state::{AppState, DbType, ReasoningInfo};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::Value as J;
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const PREFIXES: &str = "@prefix ex: <http://ex.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
";

struct Server {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

fn server_with(trig: &str, configure: impl FnOnce(&mut AppState)) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut state =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    configure(&mut state);
    let state = Arc::new(state);
    let ds = state.attach("t", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            format!("{PREFIXES}{trig}").into_bytes(),
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

fn server(trig: &str) -> Server {
    server_with(trig, |_| {})
}

async fn get(app: &Router, path: &str) -> (StatusCode, J) {
    let res = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&body)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&body)));
    (status, json)
}

async fn ok(app: &Router, path: &str) -> J {
    let (status, j) = get(app, path).await;
    assert_eq!(status, StatusCode::OK, "{path}: {j}");
    j
}

async fn update(app: &Router, u: &str) {
    let res = app
        .clone()
        .oneshot(
            Request::post("/t/update")
                .header(header::CONTENT_TYPE, "application/sparql-update")
                .body(Body::from(format!("PREFIX ex: <http://ex.org/>\n{u}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(res.status().is_success(), "{}", res.status());
}

fn iris(page: &J) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["iri"].as_str().unwrap().to_string())
        .collect()
}

fn find<'a>(page: &'a J, iri: &str) -> &'a J {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["iri"] == iri)
        .unwrap_or_else(|| panic!("{iri} not in {page}"))
}

fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[tokio::test]
async fn summary_shape() {
    let s = server(
        r#"ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:A . ex:C rdfs:subClassOf ex:A .
           ex:D rdfs:subClassOf ex:D . ex:x a ex:C ; ex:name "x" .
           ex:O a owl:Ontology ; rdfs:label "Test" ."#,
    );
    let j = ok(&s.app, "/$/schema/t").await;
    assert_eq!(j["schemaFormat"], 1);
    assert_eq!(j["dataset"], "t");
    assert!(j["snapshot"]["version"].is_u64());
    assert!(j["snapshot"]["generation"].is_string());
    assert!(j["snapshot"]["computedAt"].as_str().unwrap().ends_with('Z'));
    assert_eq!(
        j["selection"],
        serde_json::json!({"graph": "default", "declaredGraph": "default", "reasoning": false, "declared": "asserted"})
    );
    assert_eq!(
        j["hierarchy"]["roots"],
        serde_json::json!([
            "http://ex.org/A",
            "http://ex.org/D",
            "http://www.w3.org/2002/07/owl#Ontology"
        ])
    );
    assert_eq!(
        j["hierarchy"]["cycles"],
        serde_json::json!([["http://ex.org/A", "http://ex.org/B"]])
    );
    assert_eq!(j["totals"]["classes"], 5);
    assert_eq!(j["classes"]["total"], 5);
    assert!(j["classes"]["next"].is_null());
    assert_eq!(
        find(&j["classes"], "http://ex.org/C")["observed"]["instances"],
        1
    );
    let owl = find(&j["classes"], "http://www.w3.org/2002/07/owl#Ontology");
    assert_eq!(owl["builtin"], true);
    let name = find(&j["predicates"], "http://ex.org/name");
    assert_eq!(name["observed"]["maxPerSubject"], 1);
    assert_eq!(j["ontology"][0]["labels"][0]["value"], "Test");

    // served from the cached report until the data changes
    let again = ok(&s.app, "/$/schema/t").await;
    assert_eq!(again["snapshot"]["computedAt"], j["snapshot"]["computedAt"]);
    update(&s.app, "INSERT DATA { ex:y a ex:C }").await;
    let after = ok(&s.app, "/$/schema/t").await;
    assert!(after["snapshot"]["version"].as_u64() > j["snapshot"]["version"].as_u64());
    assert_eq!(
        find(&after["classes"], "http://ex.org/C")["observed"]["instances"],
        2
    );
}

#[tokio::test]
async fn pagination() {
    let data: String = (0..2500)
        .map(|i| format!("ex:i{i} a ex:C{i:04} .\n"))
        .collect();
    let s = server(&data);
    let p1 = ok(&s.app, "/$/schema/t/classes?limit=1000").await;
    assert_eq!((iris(&p1).len(), p1["total"].as_u64()), (1000, Some(2500)));
    let next = p1["next"].as_str().unwrap().to_string();
    let p2 = ok(
        &s.app,
        &format!("/$/schema/t/classes?limit=1000&cursor={next}"),
    )
    .await;
    let p3 = ok(
        &s.app,
        &format!(
            "/$/schema/t/classes?limit=1000&cursor={}",
            p2["next"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(iris(&p3).len(), 500);
    assert!(p3["next"].is_null());
    let all: Vec<String> = [&p1, &p2, &p3].iter().flat_map(|p| iris(p)).collect();
    let expected: Vec<String> = (0..2500)
        .map(|i| format!("http://ex.org/C{i:04}"))
        .collect();
    assert_eq!(all, expected);

    // the summary's first pages use the same limit
    let sum = ok(&s.app, "/$/schema/t?limit=3").await;
    assert_eq!(iris(&sum["classes"]).len(), 3);
    assert!(sum["predicates"]["next"].is_null());

    // a write after the first page: the old cursor is served from the cached report
    update(&s.app, "INSERT DATA { ex:q a ex:C9999 }").await;
    let old = ok(
        &s.app,
        &format!("/$/schema/t/classes?limit=1000&cursor={next}"),
    )
    .await;
    assert_eq!(old["total"], 2500);
    assert_eq!(iris(&old), iris(&p2));
    // once a report of the new snapshot replaces it, the old cursor is stale
    let fresh = ok(&s.app, "/$/schema/t/classes?limit=1").await;
    assert_eq!(fresh["total"], 2501);
    let (status, j) = get(
        &s.app,
        &format!("/$/schema/t/classes?limit=1000&cursor={next}"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{j}");
    assert!(
        j["error"]
            .as_str()
            .unwrap()
            .contains("restart from the first page")
    );

    // a cursor only fits the selection it was issued for
    let (status, _) = get(
        &s.app,
        &format!(
            "/$/schema/t/classes?graph=union&cursor={}",
            fresh["next"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn errors() {
    let data: String = (0..3000)
        .map(|i| format!("ex:i{i} a ex:C{i} ; ex:p{} {i} .\n", i % 50))
        .collect();
    let s = server(&data);
    let cases = [
        ("/$/schema/nope", StatusCode::NOT_FOUND),
        (
            "/$/schema/t?graph=http%3A%2F%2Fex.org%2Fmissing",
            StatusCode::NOT_FOUND,
        ),
        (
            "/$/schema/t?declaredGraph=http%3A%2F%2Fex.org%2Fmissing",
            StatusCode::NOT_FOUND,
        ),
        ("/$/schema/t/classes?limit=0", StatusCode::BAD_REQUEST),
        ("/$/schema/t/classes?limit=10001", StatusCode::BAD_REQUEST),
        ("/$/schema/t/classes?limit=ten", StatusCode::BAD_REQUEST),
        ("/$/schema/t?graph=not%20an%20iri", StatusCode::BAD_REQUEST),
        ("/$/schema/t?reasoning=maybe", StatusCode::BAD_REQUEST),
        ("/$/schema/t?declared=some", StatusCode::BAD_REQUEST),
        ("/$/schema/t?timeout=-1", StatusCode::BAD_REQUEST),
        ("/$/schema/t/predicates?cursor=%%%", StatusCode::BAD_REQUEST),
        (
            "/$/schema/t/predicates?cursor=eyJ4IjoxfQ",
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (path, want) in cases {
        let (status, j) = get(&s.app, path).await;
        assert_eq!(status, want, "{path}: {j}");
        assert!(j["error"].is_string(), "{path}: {j}");
    }
    let (status, j) = get(&s.app, "/$/schema/t?timeout=0.000001").await;
    assert_eq!(status, StatusCode::REQUEST_TIMEOUT, "{j}");
    let msg = j["error"].as_str().unwrap();
    assert!(
        msg.contains("exceeded") && msg.contains("timeout="),
        "{msg}"
    );
    // named graphs by IRI, Jena's special names
    let (status, _) = get(
        &s.app,
        &format!("/$/schema/t?graph={}", enc("urn:x-arq:UnionGraph")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn entry_cap() {
    let data: String = (0..11).map(|i| format!("ex:i a ex:C{i} .\n")).collect();
    let s = server_with(&data, |st| st.schema_max_entries = 10);
    let (status, j) = get(&s.app, "/$/schema/t").await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{j}");
    assert_eq!(j["error"], "dataset has 11 classes (limit 10)");
}

#[tokio::test]
async fn named_graphs_and_read_only() {
    let s = server_with(
        "ex:x a ex:P . ex:g1 { ex:y a ex:P . ex:x a ex:P . } ex:onto { ex:P a owl:Class ; rdfs:label \"Person\" . }",
        |st| st.read_only = true,
    );
    let j = ok(&s.app, "/$/schema/t/classes").await;
    let p = find(&j, "http://ex.org/P");
    assert_eq!(p["observed"]["instances"], 1);
    assert_eq!(p["declared"]["types"], serde_json::json!([]));
    let j = ok(
        &s.app,
        &format!(
            "/$/schema/t/classes?graph=union&declaredGraph={}",
            enc("http://ex.org/onto")
        ),
    )
    .await;
    let p = find(&j, "http://ex.org/P");
    assert_eq!(p["observed"]["instances"], 2);
    assert_eq!(
        p["declared"]["types"],
        serde_json::json!(["http://www.w3.org/2002/07/owl#Class"])
    );
    assert_eq!(p["declared"]["labels"][0]["value"], "Person");
    let j = ok(
        &s.app,
        &format!("/$/schema/t/classes?graph={}", enc("<http://ex.org/g1>")),
    )
    .await;
    assert_eq!(find(&j, "http://ex.org/P")["observed"]["instances"], 2);
}

#[tokio::test]
async fn inferences_follow_the_reasoning_parameter() {
    let s = server(
        "ex:C rdfs:subClassOf ex:B . ex:x a ex:C .
         <urn:x-sparkles:inferred> { ex:x a ex:B . ex:C rdfs:subClassOf rdfs:Resource . }",
    );
    // no reasoning status yet: inferences are off by default
    let j = ok(&s.app, "/$/schema/t").await;
    assert_eq!(j["selection"]["reasoning"], false);
    assert_eq!(
        find(&j["classes"], "http://ex.org/B")["observed"]["instances"],
        0
    );
    *s.state.get("t").unwrap().reasoning.write() = Some(ReasoningInfo {
        profile: "rdfs".into(),
        inferred: 2,
        at: crate::state::now(),
        ..Default::default()
    });
    let j = ok(&s.app, "/$/schema/t").await;
    assert_eq!(j["selection"]["reasoning"], true);
    assert_eq!(
        find(&j["classes"], "http://ex.org/B")["observed"]["instances"],
        1
    );
    let c = find(&j["classes"], "http://ex.org/C");
    assert_eq!(
        c["declared"]["superClasses"],
        serde_json::json!(["http://ex.org/B"])
    );
    let j = ok(&s.app, "/$/schema/t?reasoning=false").await;
    assert_eq!(
        find(&j["classes"], "http://ex.org/B")["observed"]["instances"],
        0
    );
    let j = ok(&s.app, "/$/schema/t?declared=all").await;
    let c = find(&j["classes"], "http://ex.org/C");
    assert_eq!(c["declared"]["superClasses"].as_array().unwrap().len(), 2);
}
