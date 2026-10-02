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

/// Status, content type and body of a GET with an `Accept` header.
async fn get_accept(app: &Router, path: &str, accept: &str) -> (StatusCode, String, String) {
    let res = app
        .clone()
        .oneshot(
            Request::get(path)
                .header(header::ACCEPT, accept)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let ct = res
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, ct, String::from_utf8(body.to_vec()).unwrap())
}

const VOID: &str = "http://rdfs.org/ns/void#";

/// The VoID dataset node and the integer value of each of its `void:` properties.
fn void_numbers(
    text: &str,
    format: oxrdfio::RdfFormat,
) -> (Vec<oxrdf::Triple>, Vec<(String, u64)>) {
    let g: Vec<oxrdf::Triple> = oxrdfio::RdfParser::from_format(format)
        .for_slice(text.as_bytes())
        .map(|q| oxrdf::Triple::from(q.unwrap()))
        .collect();
    let dataset = g
        .iter()
        .find(|t| t.object.to_string() == format!("<{VOID}Dataset>"))
        .unwrap_or_else(|| panic!("no void:Dataset in\n{text}"))
        .subject
        .clone();
    let mut numbers: Vec<(String, u64)> = g
        .iter()
        .filter(|t| t.subject == dataset)
        .filter_map(|t| {
            let p = t.predicate.as_str().strip_prefix(VOID)?;
            match &t.object {
                oxrdf::Term::Literal(l) => Some((p.to_string(), l.value().parse().ok()?)),
                _ => None,
            }
        })
        .collect();
    numbers.sort();
    (g, numbers)
}

#[tokio::test]
async fn void_by_content_negotiation() {
    let s = server(
        r#"ex:Person a owl:Class ; rdfs:label "Person" .
           ex:alice a ex:Person ; ex:knows ex:bob .
           ex:bob a ex:Person ."#,
    );
    // JSON first: the cached report has no term totals, and VoID computes them
    let j = ok(&s.app, "/$/schema/t").await;
    assert!(j["totals"].get("distinctSubjects").is_none());
    let (status, ct, text) = get_accept(&s.app, "/$/schema/t", "text/turtle").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(ct.starts_with("text/turtle"), "{ct}");
    assert!(text.contains("@prefix void:"), "{text}");
    let (g, numbers) = void_numbers(&text, oxrdfio::RdfFormat::Turtle);
    let expected = [
        ("classes", 2),
        ("distinctObjects", 4),
        ("distinctSubjects", 3),
        ("entities", 3),
        ("properties", 3),
        ("triples", 5),
    ]
    .map(|(p, n)| (p.to_string(), n));
    assert_eq!(numbers, expected, "{text}");
    let parts = |key: &str| {
        g.iter()
            .filter(|t| t.predicate.as_str() == format!("{VOID}{key}"))
            .count()
    };
    assert_eq!(
        (parts("classPartition"), parts("propertyPartition")),
        (2, 3)
    );
    // the declarations follow, unless left out
    let label = "http://www.w3.org/2000/01/rdf-schema#label";
    assert!(g.iter().any(|t| t.predicate.as_str() == label), "{text}");
    let (_, _, text) = get_accept(&s.app, "/$/schema/t?declarations=false", "text/turtle").await;
    let (g, numbers) = void_numbers(&text, oxrdfio::RdfFormat::Turtle);
    assert_eq!(numbers, expected);
    assert!(!g.iter().any(|t| t.predicate.as_str() == label), "{text}");
    // the other syntaxes, by Accept or format=
    for (path, accept, ct, format) in [
        (
            "/$/schema/t",
            "application/n-triples",
            "application/n-triples",
            oxrdfio::RdfFormat::NTriples,
        ),
        (
            "/$/schema/t",
            "application/rdf+xml;q=0.9, application/json;q=0.5",
            "application/rdf+xml",
            oxrdfio::RdfFormat::RdfXml,
        ),
        (
            "/$/schema/t?format=nquads",
            "*/*",
            "application/n-quads",
            oxrdfio::RdfFormat::NQuads,
        ),
    ] {
        let (status, got, text) = get_accept(&s.app, path, accept).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(got, ct);
        assert_eq!(void_numbers(&text, format).1, expected, "{accept}");
    }
    // JSON stays the default, and the JSON document has no term totals
    for (path, accept) in [
        ("/$/schema/t", "*/*"),
        ("/$/schema/t", "application/json"),
        ("/$/schema/t?format=json", "text/turtle"),
    ] {
        let (status, ct, text) = get_accept(&s.app, path, accept).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, "application/json", "{path} {accept}");
        let j: J = serde_json::from_str(&text).unwrap();
        assert_eq!(j["totals"]["triples"], 5);
        assert!(j["totals"].get("distinctSubjects").is_none(), "{text}");
    }
    for path in [
        "/$/schema/t?format=csv",
        "/$/schema/t?format=turtle&declarations=maybe",
    ] {
        let (status, _, text) = get_accept(&s.app, path, "*/*").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {text}");
    }
}

const DRAFT_DATA: &str = r#"ex:Employee rdfs:subClassOf ex:Person .
    ex:a a ex:Person ; ex:name "A" ; ex:status "active" ; ex:knows ex:b .
    ex:b a ex:Person ; ex:name "B" ; ex:status "active" ; ex:knows ex:c .
    ex:c a ex:Employee ; ex:name "C", "Cee" ; ex:status "inactive" ; ex:knows ex:a .
    ex:d a ex:Person ; ex:name "D" ; ex:status "inactive" .
    ex:o a ex:Org ; ex:member ex:a, ex:c .
    GRAPH ex:g { ex:z a ex:Person ; ex:secret "s" }"#;

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, String, String) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let ct = res
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, ct, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn drafted_shapes() {
    let s = server(DRAFT_DATA);
    let j = ok(&s.app, "/$/schema/t/shapes").await;
    assert_eq!(j["draftFormat"], 1);
    assert_eq!(j["selection"]["graph"], "default");
    assert_eq!(j["options"]["support"], 1.0);
    let classes: Vec<&str> = j["shapes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["class"].as_str().unwrap())
        .collect();
    assert_eq!(
        classes,
        [
            "http://ex.org/Employee",
            "http://ex.org/Org",
            "http://ex.org/Person"
        ]
    );
    let person = &j["shapes"][2];
    assert_eq!(person["instances"], 4);
    assert_eq!(person["shape"], "urn:x-sparkles:shape:t:PersonShape");
    assert!(
        j["shacl"]
            .as_str()
            .unwrap()
            .contains("sh:targetClass ex:Person")
    );
    assert!(j["shex"].as_str().unwrap().contains("shape:PersonShape {"));
    assert!(!j["shacl"].as_str().unwrap().contains("secret"));
    // ex:c has two names: sh:maxCount 1 is rejected at support 1, drafted at 0.75
    let name = person["properties"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["path"] == "http://ex.org/name")
        .unwrap();
    let max = name["rejected"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["component"] == "maxCount")
        .unwrap();
    assert_eq!(max["excluded"], 1);
    let j = ok(&s.app, "/$/schema/t/shapes?support=0.75&closed=true").await;
    assert!(
        j["shacl"]
            .as_str()
            .unwrap()
            .contains("sh:maxCount 1 ;  # 3 of 4 instances, excludes 1"),
        "{}",
        j["shacl"]
    );
    assert!(j["shacl"].as_str().unwrap().contains("sh:closed true"));

    // Turtle, SHACLC and ShExC by format= or Accept
    for (path, accept, ct, needle) in [
        (
            "/$/schema/t/shapes?format=shaclc",
            "*/*",
            "text/shaclc",
            "shape shape:",
        ),
        (
            "/$/schema/t/shapes",
            "text/shaclc",
            "text/shaclc",
            "shape shape:",
        ),
        (
            "/$/schema/t/shapes?format=turtle",
            "*/*",
            "text/turtle",
            "a sh:NodeShape",
        ),
        (
            "/$/schema/t/shapes",
            "text/turtle",
            "text/turtle",
            "a sh:NodeShape",
        ),
        ("/$/schema/t/shapes", "text/shex", "text/shex", "PREFIX ex:"),
        (
            "/$/schema/t/shapes?format=shexc",
            "*/*",
            "text/shex",
            "PREFIX ex:",
        ),
    ] {
        let (status, got, text) = get_accept(&s.app, path, accept).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert!(got.starts_with(ct), "{got}");
        assert!(text.contains(needle), "{text}");
    }

    // the draft validates the data, and installs as a guard in warn mode
    let (_, _, turtle) = get_accept(&s.app, "/$/schema/t/shapes?format=turtle", "*/*").await;
    let (status, _, report) = send(
        &s.app,
        Request::post("/t/shacl")
            .header(header::CONTENT_TYPE, "text/turtle")
            .header(header::ACCEPT, "application/json")
            .body(Body::from(turtle.clone()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let report: J = serde_json::from_str(&report).unwrap();
    assert_eq!(report["conforms"], true, "{report}");
    let body = serde_json::json!({
        "language": "shacl", "mode": "warn", "shapes": { "inline": turtle }
    });
    let (status, _, text) = send(
        &s.app,
        Request::put("/$/validation/t")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let j: J = serde_json::from_str(&text).unwrap();
    assert_eq!(j["status"]["baseline"]["conforms"], true, "{j}");

    // and so does the SHACLC draft
    let (_, _, compact) = get_accept(&s.app, "/$/schema/t/shapes?format=shaclc", "*/*").await;
    let (status, _, report) = send(
        &s.app,
        Request::post("/t/shacl")
            .header(header::CONTENT_TYPE, "text/shaclc")
            .header(header::ACCEPT, "application/json")
            .body(Body::from(compact.clone()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let report: J = serde_json::from_str(&report).unwrap();
    assert_eq!(report["conforms"], true, "{report}");
    let body = serde_json::json!({
        "language": "shacl", "mode": "warn",
        "shapes": { "inline": compact, "format": "text/shaclc" }
    });
    let (status, _, text) = send(
        &s.app,
        Request::put("/$/validation/t")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let j: J = serde_json::from_str(&text).unwrap();
    assert_eq!(j["status"]["baseline"]["conforms"], true, "{j}");

    // the union graph includes the named graph's person
    let j = ok(&s.app, "/$/schema/t/shapes?graph=union").await;
    assert_eq!(j["shapes"][2]["instances"], 5);
    let j = ok(
        &s.app,
        &format!("/$/schema/t/shapes?class={}", enc("http://ex.org/Org")),
    )
    .await;
    assert_eq!(j["shapes"].as_array().unwrap().len(), 1);

    for path in [
        "/$/schema/t/shapes?support=0",
        "/$/schema/t/shapes?support=1.5",
        "/$/schema/t/shapes?support=x",
        "/$/schema/t/shapes?maxIn=x",
        "/$/schema/t/shapes?maxIn=65",
        "/$/schema/t/shapes?closed=maybe",
        "/$/schema/t/shapes?format=csv",
        "/$/schema/t/shapes?class=not%20an%20iri",
    ] {
        let (status, _, text) = get_accept(&s.app, path, "*/*").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {text}");
    }
    let (status, _, _) = get_accept(
        &s.app,
        &format!("/$/schema/t/shapes?graph={}", enc("http://ex.org/missing")),
        "*/*",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn subject_classes_on_request() {
    let s = server(
        r#"ex:a a ex:Person ; ex:name "A" . ex:b a ex:Person, ex:Agent ; ex:name "B", "B2" .
           ex:o a ex:Org ; ex:name "O" . ex:u ex:name "U" ."#,
    );
    let j = ok(&s.app, "/$/schema/t/predicates?detail=subjectClasses").await;
    let name = &find(&j, "http://ex.org/name")["observed"];
    assert_eq!(
        name["subjectClasses"],
        serde_json::json!([
            {"class": "http://ex.org/Agent", "triples": 2, "subjects": 1},
            {"class": "http://ex.org/Org", "triples": 1, "subjects": 1},
            {"class": "http://ex.org/Person", "triples": 3, "subjects": 2},
        ])
    );
    assert_eq!(
        name["untypedSubjects"],
        serde_json::json!({"triples": 1, "subjects": 1})
    );
    let j = ok(&s.app, "/$/schema/t?detail=subjectClasses").await;
    assert!(find(&j["predicates"], "http://ex.org/name")["observed"]["subjectClasses"].is_array());
    // without the detail, the report has no subject classes, even right after one with
    let j = ok(&s.app, "/$/schema/t/predicates").await;
    assert!(
        find(&j, "http://ex.org/name")["observed"]
            .get("subjectClasses")
            .is_none()
    );
    // a cursor of a listing with the detail does not continue one without
    let j = ok(
        &s.app,
        "/$/schema/t/predicates?detail=subjectClasses&limit=1",
    )
    .await;
    let next = j["next"].as_str().unwrap();
    let (status, _) = get(
        &s.app,
        &format!("/$/schema/t/predicates?limit=1&cursor={next}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = get(&s.app, "/$/schema/t?detail=everything").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[cfg(feature = "shacl")]
const SHAPES_DATA: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:a a ex:Person ; ex:name "A" .
ex:b a ex:Person ; ex:name "B" .
ex:shapes {
  ex:PersonShape a sh:NodeShape ;
    sh:targetClass ex:Person ;
    sh:property [ sh:path ex:name ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
                [ sh:path ex:nick ; sh:maxCount 1 ; sh:severity sh:Warning ] .
}
"#;

#[cfg(feature = "shacl")]
#[tokio::test]
async fn constraints_layer() {
    let s = server(SHAPES_DATA);
    let shapes = enc("http://ex.org/shapes");
    // no guard and no shapes= : no layer
    let j = ok(&s.app, "/$/schema/t").await;
    assert!(j.get("constraints").is_none(), "{j}");

    // a shapes graph, validated on request only
    let j = ok(&s.app, &format!("/$/schema/t?shapes={shapes}")).await;
    let src = &j["constraints"]["sources"][0];
    assert_eq!(src["kind"], "graphs");
    assert_eq!(src["graphs"], serde_json::json!(["http://ex.org/shapes"]));
    let person = &src["classes"][0];
    assert_eq!(person["class"], "http://ex.org/Person");
    assert_eq!(
        person["shapes"],
        serde_json::json!(["http://ex.org/PersonShape"])
    );
    let name = &person["properties"][0];
    assert_eq!(name["path"], "http://ex.org/name");
    assert_eq!(name["minCount"], 1);
    assert_eq!(name["maxCount"], 1);
    assert_eq!(name["datatype"], "http://www.w3.org/2001/XMLSchema#string");
    assert_eq!(name["enforcement"], "validated-on-request");
    // the observed layer is unchanged by it
    let observed = &find(&j["predicates"], "http://ex.org/name")["observed"];
    assert_eq!(observed["maxPerSubject"], 1);
    assert!(observed.get("minCount").is_none());
    // the layer alone
    let c = ok(&s.app, &format!("/$/schema/t/constraints?shapes={shapes}")).await;
    assert_eq!(c["constraints"], j["constraints"]);
    assert_eq!(c["dataset"], "t");
    let c = ok(&s.app, "/$/schema/t/constraints").await;
    assert_eq!(c["constraints"]["sources"], serde_json::json!([]));

    for (path, want) in [
        ("/$/schema/t?shapes=guard", StatusCode::NOT_FOUND),
        (
            "/$/schema/t/constraints?shapes=guard",
            StatusCode::NOT_FOUND,
        ),
        ("/$/schema/t?shapes=union", StatusCode::BAD_REQUEST),
        (
            "/$/schema/t?shapes=none&shapes=guard",
            StatusCode::BAD_REQUEST,
        ),
        ("/$/schema/t?shapes=not%20an%20iri", StatusCode::BAD_REQUEST),
        (
            "/$/schema/t?shapes=http%3A%2F%2Fex.org%2Fmissing",
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, j) = get(&s.app, path).await;
        assert_eq!(status, want, "{path}: {j}");
    }

    // the write-time guard's shapes are the layer by default
    let body = serde_json::json!({
        "language": "shacl", "mode": "reject",
        "shapes": { "graphs": ["http://ex.org/shapes"] }
    });
    let (status, _, text) = send(
        &s.app,
        Request::put("/$/validation/t")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let j = ok(&s.app, "/$/schema/t").await;
    let src = &j["constraints"]["sources"][0];
    assert_eq!(src["kind"], "guard");
    assert_eq!(src["mode"], "reject");
    assert_eq!(src["threshold"], "violation");
    let props = &src["classes"][0]["properties"];
    assert_eq!(props[0]["enforcement"], "reject-on-write");
    // a warning does not block a write at the default threshold
    assert_eq!(props[1]["path"], "http://ex.org/nick");
    assert_eq!(props[1]["enforcement"], "warn-on-write");
    // both sources, and none
    let j = ok(&s.app, &format!("/$/schema/t?shapes=guard&shapes={shapes}")).await;
    let kinds: Vec<&str> = j["constraints"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["guard", "graphs"]);
    let j = ok(&s.app, "/$/schema/t?shapes=none").await;
    assert!(j.get("constraints").is_none(), "{j}");
}

/// A summary, and how its report came about (`Sparkles-Schema-Report`).
async fn summary_with_header(app: &Router, path: &str) -> (J, String) {
    let res = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let how = res
        .headers()
        .get("sparkles-schema-report")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let mut j: J = serde_json::from_slice(&body).unwrap();
    j["snapshot"].as_object_mut().unwrap().remove("computedAt");
    (j, how)
}

#[tokio::test]
async fn reports_are_updated_from_changes() {
    for kind in [DbType::Mem, DbType::Persistent] {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
        );
        let ds = state.attach("t", kind, None).unwrap();
        let app = router(state.clone());
        update(
            &app,
            "INSERT DATA { ex:a a ex:C ; ex:p 1, \"x\"@en . ex:C <http://www.w3.org/2000/01/rdf-schema#label> \"C\" }",
        )
        .await;
        for path in ["/$/schema/t", "/$/schema/t?graph=union&reasoning=false"] {
            let (_, how) = summary_with_header(&app, path).await;
            assert_eq!(how, "full", "{kind:?} {path}");
            let (_, how) = summary_with_header(&app, path).await;
            assert_eq!(how, "cached");
            update(
                &app,
                "DELETE DATA { ex:a ex:p 1 } ; INSERT DATA { ex:b a ex:C, ex:D ; ex:p 2 . GRAPH ex:g { ex:b ex:q ex:a } }",
            )
            .await;
            let (updated, how) = summary_with_header(&app, path).await;
            assert_eq!(how, "updated; changes=5", "{kind:?} {path}");
            // the same report computed from scratch
            *ds.schema_cache.lock() = None;
            let (full, how) = summary_with_header(&app, path).await;
            assert_eq!(how, "full");
            assert_eq!(updated, full, "{kind:?} {path}");
            assert!(full["snapshot"]["commit"].as_u64().unwrap() >= 2);
            // undo, so the next selection starts from the same data
            update(
                &app,
                "INSERT DATA { ex:a ex:p 1 } ; DELETE DATA { ex:b a ex:C, ex:D ; ex:p 2 . GRAPH ex:g { ex:b ex:q ex:a } }",
            )
            .await;
        }
    }
}
