//! In-process tests of the ontology routes; the caller's grants are tested in
//! `router_tests::auth::ontology`.

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

const DATA: &str = "@prefix : <http://ex.org/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
:o a owl:Ontology ; rdfs:label \"Teaching\" .
:Professor a owl:Class . :Course a owl:Class . :Person a owl:Class .
:teaches a owl:ObjectProperty .
:Professor rdfs:subClassOf :Person ,
    [ a owl:Restriction ; owl:onProperty :teaches ; owl:someValuesFrom :Course ] .
:ada a :Professor ; :teaches :logic .
";

struct Server {
    _dir: tempfile::TempDir,
    _state: Arc<AppState>,
    app: Router,
}

fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let ds = state.attach("t", DbType::Mem, None).unwrap();
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
        _state: state,
        app,
    }
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> J {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.text()))
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

async fn send(app: &Router, method: &str, uri: &str, headers: &[(&str, &str)], body: &str) -> Resp {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Resp {
        status,
        headers,
        body,
    }
}

async fn get(app: &Router, uri: &str) -> Resp {
    send(app, "GET", uri, &[], "").await
}

async fn post(app: &Router, uri: &str, body: J) -> Resp {
    send(
        app,
        "POST",
        uri,
        &[(header::CONTENT_TYPE.as_str(), "application/json")],
        &body.to_string(),
    )
    .await
}

fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn sub(a: &str, b: J) -> J {
    json!({"type": "SubClassOf", "subClass": a, "superClass": b})
}

#[tokio::test]
async fn the_summary_counts_the_terminology() {
    let s = server();
    let r = get(&s.app, "/$/ontology/t").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["header"]["iri"], "http://ex.org/o");
    assert_eq!(
        j["header"]["annotations"][0],
        "Annotation(rdfs:label \"Teaching\")"
    );
    assert_eq!(j["axioms"]["SubClassOf"], 2);
    assert_eq!(j["entities"]["Class"], 3);
    assert_eq!(j["unmapped"], 0);
    assert_eq!(j["graphs"], json!(["default"]));
    assert_eq!(j["mode"], "lenient");
    let tag = r.headers[header::ETAG].to_str().unwrap();
    assert!(tag.ends_with(":owl\""), "{tag}");
    // a parameter that does not parse is a 400
    let r = get(&s.app, "/$/ontology/t?mode=loose").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = get(&s.app, "/$/ontology/missing").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn axioms_page_and_filter() {
    let s = server();
    let r = get(&s.app, "/$/ontology/t/axioms?type=SubClassOf").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["total"], 2);
    let ofn: Vec<&str> = j["axioms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["ofn"].as_str().unwrap())
        .collect();
    assert!(
        ofn.contains(&"SubClassOf(:Professor ObjectSomeValuesFrom(:teaches :Course))"),
        "{ofn:?}"
    );
    assert_eq!(j["axioms"][0]["graph"], "default");
    // pages
    let j = get(&s.app, "/$/ontology/t/axioms?limit=3").await.json();
    assert_eq!(j["axioms"].as_array().unwrap().len(), 3);
    assert_eq!(j["next"], 3);
    let total = j["total"].as_u64().unwrap();
    let j = get(
        &s.app,
        &format!("/$/ontology/t/axioms?limit=3&offset={}", total - 1),
    )
    .await
    .json();
    assert_eq!(j["axioms"].as_array().unwrap().len(), 1);
    assert!(j["next"].is_null());
    // about an entity, with the assertions the store holds
    let about = enc("http://ex.org/ada");
    let j = get(
        &s.app,
        &format!("/$/ontology/t/axioms?about={about}&assertions=true"),
    )
    .await
    .json();
    assert_eq!(j["total"], 2, "{j}");
    let unknown = enc("http://ex.org/nobody");
    let r = get(&s.app, &format!("/$/ontology/t/axioms?about={unknown}")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // the functional-style document
    let r = get(&s.app, "/$/ontology/t/axioms?format=ofn&type=SubClassOf").await;
    assert_eq!(r.status, StatusCode::OK);
    let text = r.text();
    assert!(text.contains("Ontology(<http://ex.org/o>"), "{text}");
    assert!(text.contains("SubClassOf(:Professor :Person)"), "{text}");
    assert!(get(&s.app, "/$/ontology/t/axioms?type=Nope").await.status == StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn checks_report_each_profile() {
    let s = server();
    let r = post(
        &s.app,
        "/$/ontology/t/check",
        json!({"profiles": ["el", "ql", "rl", "dl"]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let conforms: Vec<bool> = j["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["conforms"].as_bool().unwrap())
        .collect();
    assert_eq!(conforms, [true, true, false, true]);
    assert_eq!(j["conforms"], false);
    let rl = &j["reports"][2];
    assert_eq!(rl["profile"], "rl");
    assert!(
        rl["violations"][0]["rule"]
            .as_str()
            .unwrap()
            .contains("§4.2"),
        "{rl}"
    );
    assert!(
        rl["violations"][0]["axiom"]
            .as_str()
            .unwrap()
            .starts_with("SubClassOf(")
    );
    // the default is DL; a strict read refuses the undeclared individuals' property
    let r = post(&s.app, "/$/ontology/t/check", json!({"mode": "strict"})).await;
    assert_eq!(r.json()["reports"].as_array().unwrap().len(), 1);
    let r = post(&s.app, "/$/ontology/t/check", json!({"profiles": ["xl"]})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = post(&s.app, "/$/ontology/t/check", json!({"nonsense": 1})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn edits_add_and_remove_axioms() {
    let s = server();
    let added = sub("http://ex.org/Lecturer", json!("http://ex.org/Person"));
    let r = post(&s.app, "/$/ontology/t/axioms", json!({"add": [added]})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["added"], 2, "{j}");
    assert_eq!(j["declared"], json!(["Class(:Lecturer)"]));
    assert!(r.headers.contains_key("sparkles-commit"), "{:?}", r.headers);
    let j = get(&s.app, "/$/ontology/t/axioms?type=SubClassOf")
        .await
        .json();
    assert_eq!(j["total"], 3);
    // the restriction goes with its axiom
    let restriction = sub(
        "http://ex.org/Professor",
        json!({"type": "ObjectSomeValuesFrom", "property": "http://ex.org/teaches", "class": "http://ex.org/Course"}),
    );
    let r = post(
        &s.app,
        "/$/ontology/t/axioms",
        json!({"remove": [restriction, added]}),
    )
    .await;
    let j = r.json();
    assert_eq!(
        (j["removed"].as_u64(), j["deleted"].as_u64()),
        (Some(2), Some(5)),
        "{j}"
    );
    let j = get(&s.app, "/$/ontology/t").await.json();
    assert_eq!(j["axioms"]["SubClassOf"], 1);
    assert_eq!(j["unmapped"], 0);
    // removing what is not there is not an error
    let r = post(
        &s.app,
        "/$/ontology/t/axioms",
        json!({"remove": [sub("http://ex.org/A", json!("http://ex.org/B"))]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["absent"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn edits_refuse_bad_axioms_and_stale_tags() {
    let s = server();
    let bad = json!({"type": "DisjointClasses", "classes": ["http://ex.org/A"]});
    let r = post(&s.app, "/$/ontology/t/axioms", json!({"add": [bad]})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["code"], "invalid-axiom");
    let r = post(&s.app, "/$/ontology/t/axioms", json!({})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // If-Match names the summary's commit
    let tag = get(&s.app, "/$/ontology/t").await.headers[header::ETAG]
        .to_str()
        .unwrap()
        .to_string();
    let body = json!({"add": [sub("http://ex.org/A", json!("http://ex.org/B"))]}).to_string();
    let ct = (header::CONTENT_TYPE.as_str(), "application/json");
    let r = send(
        &s.app,
        "POST",
        "/$/ontology/t/axioms",
        &[ct, ("if-match", &tag)],
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = send(
        &s.app,
        "POST",
        "/$/ontology/t/axioms",
        &[ct, ("if-match", &tag)],
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED, "{}", r.text());
}

#[tokio::test]
async fn a_dry_run_commits_nothing() {
    let s = server();
    let body = json!({"add": [sub("http://ex.org/A", json!("http://ex.org/B"))]});
    let r = post(&s.app, "/$/ontology/t/axioms?dryRun=true", body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = get(&s.app, "/$/ontology/t/axioms?type=SubClassOf")
        .await
        .json();
    assert_eq!(j["total"], 2);
}

/// A server with a persistent dataset `h`, whose history a diff reads, loaded with
/// [`DATA`].
fn persistent() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let ds = state.attach("h", DbType::Persistent, None).unwrap();
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
        _state: state,
        app,
    }
}

#[tokio::test]
async fn diffs_the_axioms_of_two_commits() {
    let s = persistent();
    let start = s._state.get("h").unwrap().store.head_commit().seq;
    let r = post(
        &s.app,
        "/$/ontology/h/axioms",
        json!({
            "add": [sub("http://ex.org/Course", json!("http://www.w3.org/2002/07/owl#Thing"))],
            "remove": [sub("http://ex.org/Professor", json!("http://ex.org/Person"))],
        }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = get(&s.app, &format!("/$/ontology/h/diff?from={start}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["from"], start);
    assert_eq!(j["addedCount"], 1, "{j}");
    assert_eq!(j["removedCount"], 1, "{j}");
    assert!(
        j["added"][0]["ofn"].as_str().unwrap().contains("Course"),
        "{j}"
    );
    assert_eq!(j["added"][0]["type"], "SubClassOf");
    assert_eq!(j["truncated"], false);
    // as lines, in the other order
    let head = s._state.get("h").unwrap().store.head_commit().seq;
    let r = get(
        &s.app,
        &format!("/$/ontology/h/diff?from={head}&to={start}&format=text"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let t = r.text();
    assert!(
        t.lines()
            .any(|l| l.starts_with("- ") && l.contains("Course")),
        "{t}"
    );
    assert!(
        t.lines()
            .any(|l| l.starts_with("+ ") && l.contains("Person")),
        "{t}"
    );
    // assertions come from the changed triples
    s._state
        .get("h")
        .unwrap()
        .dataset
        .update("INSERT DATA { <http://ex.org/bob> a <http://ex.org/Professor> }")
        .unwrap();
    let r = get(
        &s.app,
        &format!("/$/ontology/h/diff?from={head}&assertions=true"),
    )
    .await;
    let j = r.json();
    assert_eq!(j["addedCount"], 1, "{j}");
    assert_eq!(j["added"][0]["type"], "ClassAssertion", "{j}");
    // from is required and at does not apply
    assert_eq!(
        get(&s.app, "/$/ontology/h/diff").await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(
            &s.app,
            &format!("/$/ontology/h/diff?from={start}&at={start}")
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn graph_store_writes_take_functional_syntax() {
    let s = server();
    let body = "Prefix(:=<http://ex.org/>)
Ontology(<http://ex.org/g-onto>
  Declaration(Class(:Lecture))
  SubClassOf(:Lecture ObjectSomeValuesFrom(:teaches :Course))
)";
    let r = send(
        &s.app,
        "PUT",
        "/t/data?graph=http%3A%2F%2Fex.org%2Fg",
        &[(header::CONTENT_TYPE.as_str(), "text/owl-functional")],
        body,
    )
    .await;
    assert!(r.status.is_success(), "{} {}", r.status, r.text());
    let r = get(
        &s.app,
        "/$/ontology/t/axioms?graph=http%3A%2F%2Fex.org%2Fg&type=SubClassOf",
    )
    .await;
    let j = r.json();
    assert_eq!(j["total"], 1, "{j}");
    // a syntax error is a 400 that names the position
    let r = send(
        &s.app,
        "PUT",
        "/t/data?graph=http%3A%2F%2Fex.org%2Fh",
        &[(header::CONTENT_TYPE.as_str(), "text/owl-functional")],
        "Ontology(\n SubClassOf(:A))",
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("line 2"), "{}", r.text());
}

#[tokio::test]
async fn inferred_views_say_what_they_read() {
    let s = server();
    // without a materialization the view reads the asserted axioms and says so
    let r = get(&s.app, "/$/ontology/t?inferences=true").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["inferences"], json!({"included": false}));
    assert!(
        get(&s.app, "/$/ontology/t")
            .await
            .json()
            .get("inferences")
            .is_none()
    );
    let r = post(
        &s.app,
        "/$/ontology/t/check",
        json!({"inferences": true, "profiles": ["rl"]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["inferences"]["included"], false);
    // an edit changes the asserted axioms only
    let r = post(
        &s.app,
        "/$/ontology/t/axioms?inferences=true",
        json!({"add": [sub("http://ex.org/Course", json!("http://ex.org/Thing"))]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn entity_routes_answer_the_accessors() {
    let s = server();
    let r = get(&s.app, "/$/ontology/t/entities?kind=Class").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["total"], 3, "{}", r.text());
    let r = get(&s.app, "/$/ontology/t/entities?kind=Thing").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let prof = enc("http://ex.org/Professor");
    let r = get(&s.app, &format!("/$/ontology/t/entity?iri={prof}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["kinds"], json!(["Class"]));
    assert_eq!(
        j["class"]["superClasses"].as_array().unwrap().len(),
        2,
        "{j}"
    );
    assert_eq!(j["class"]["instances"], json!([]));
    let r = get(
        &s.app,
        &format!("/$/ontology/t/entity?iri={prof}&assertions=true"),
    )
    .await;
    assert_eq!(
        r.json()["class"]["instances"][0]["expression"],
        "http://ex.org/ada"
    );
    let r = get(
        &s.app,
        &format!(
            "/$/ontology/t/entity?iri={}&assertions=true",
            enc("http://ex.org/ada")
        ),
    )
    .await;
    // an individual that only the stored assertions use is found by them
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["kinds"], json!(["NamedIndividual"]), "{j}");
    assert_eq!(
        j["namedIndividual"]["types"][0]["expression"],
        "http://ex.org/Professor"
    );
    assert_eq!(
        j["namedIndividual"]["assertions"][0]["type"],
        "ObjectPropertyAssertion"
    );
    let r = get(
        &s.app,
        &format!("/$/ontology/t/entity?iri={}", enc("http://ex.org/nobody")),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = get(
        &s.app,
        &format!("/$/ontology/t/entity?iri={prof}&kind=ObjectProperty"),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = get(&s.app, "/$/ontology/t/entity").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn classify_answers_the_taxonomy_and_writes_it() {
    let s = server();
    let r = post(
        &s.app,
        "/$/ontology/t/classify",
        json!({"about": "http://ex.org/Professor", "nodes": true, "limit": 2}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.headers.contains_key("sparkles-commit"), "{:?}", r.headers);
    let j = r.json();
    assert_eq!(
        (j["consistent"].as_bool(), j["complete"].as_bool()),
        (Some(true), Some(true)),
        "{j}"
    );
    assert_eq!(j["nodes"].as_array().unwrap().len(), 2, "{j}");
    assert_eq!(j["next"], 2, "{j}");
    assert!(
        j["class"]["superClasses"]
            .as_array()
            .unwrap()
            .contains(&json!("http://ex.org/Person")),
        "{j}"
    );
    // the kept classification answers again
    let j = post(&s.app, "/$/ontology/t/classify", json!({}))
        .await
        .json();
    assert_eq!(j["reused"], true, "{j}");
    // a class the ontology does not mention, a member the route does not take, inferences
    let r = post(
        &s.app,
        "/$/ontology/t/classify",
        json!({"about": "http://ex.org/Nobody"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = post(
        &s.app,
        "/$/ontology/t/classify",
        json!({"profiles": ["el"]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = post(
        &s.app,
        "/$/ontology/t/classify",
        json!({"inferences": true}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // the taxonomy in functional-style syntax
    let r = post(&s.app, "/$/ontology/t/classify?format=ofn", json!({})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/owl-functional"),
        "{:?}",
        r.headers
    );
    assert!(
        r.text().lines().any(|l| l.starts_with("SubClassOf(")
            && l.contains("Professor")
            && l.contains("Person")),
        "{}",
        r.text()
    );
    // a write commits the taxonomy to the target graph, and a dry run commits nothing
    let body = json!({"target": "http://ex.org/tax", "message": "classified"});
    let r = post(&s.app, "/$/ontology/t/classify?dryRun=true", body.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = get(
        &s.app,
        "/$/ontology/t/axioms?graph=http%3A%2F%2Fex.org%2Ftax",
    )
    .await
    .json();
    assert_eq!(j["total"], 0, "{j}");
    let r = post(&s.app, "/$/ontology/t/classify", body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["written"]["committed"], true, "{}", r.text());
    let j = get(
        &s.app,
        "/$/ontology/t/axioms?type=SubClassOf&graph=http%3A%2F%2Fex.org%2Ftax",
    )
    .await
    .json();
    assert!(j["total"].as_u64().unwrap() >= 1, "{j}");
}
