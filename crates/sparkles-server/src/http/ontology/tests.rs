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
    // the functional-style form answers a classification without a write
    let r = post(
        &s.app,
        "/$/ontology/t/classify?format=ofn",
        json!({"target": "http://ex.org/tax"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
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

#[tokio::test]
async fn kept_classification_get_is_fresh_read_only_and_pins_pages() {
    let s = server();
    let uri = "/$/ontology/t/classification";
    let first = get(&s.app, uri).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text());
    assert_eq!(first.headers[header::CACHE_CONTROL], "no-store");
    assert!(!first.headers.contains_key(header::ETAG));
    assert_eq!(first.json()["status"], "unavailable");
    assert_eq!(first.json()["result"], J::Null);
    let classified = post(&s.app, "/$/ontology/t/classify", json!({})).await;
    assert_eq!(classified.status, StatusCode::OK, "{}", classified.text());
    // A read miss must not have warmed the classifier. Availability changes
    // without a commit and must not be hidden by conditional GET.
    assert_eq!(classified.json()["reused"], false);
    let read = send(&s.app, "GET", uri, &[("if-none-match", "*")], "").await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.text());
    assert_eq!(read.headers[header::CACHE_CONTROL], "no-store");
    let j = read.json();
    assert_eq!(j["status"], "available");
    assert_eq!(j["outcome"]["consistency"], "consistent");
    let pin = format!(
        "at=commit:{}&resultId={}",
        j["commit"],
        j["result"]["resultId"].as_str().unwrap()
    );
    let parents = get(
        &s.app,
        &format!(
            "{uri}?{pin}&part=parents&about={}&limit=1",
            enc("http://ex.org/Professor")
        ),
    )
    .await;
    assert_eq!(parents.status, StatusCode::OK, "{}", parents.text());
    assert_eq!(
        parents.json()["page"]["items"][0]["iri"],
        "http://ex.org/Person"
    );
    let sources = get(&s.app, &format!("{uri}?{pin}&part=sources&limit=1")).await;
    assert_eq!(sources.json()["page"]["items"][0]["kind"], "requestedGraph");
    let next = sources.json()["page"]["nextOffset"].as_u64().unwrap();
    let tail = get(
        &s.app,
        &format!("{uri}?{pin}&part=sources&limit=1&offset={next}"),
    )
    .await;
    assert_eq!(tail.status, StatusCode::OK, "{}", tail.text());
    assert_eq!(tail.json()["page"]["items"][0]["kind"], "resolvedGraph");
    assert_eq!(tail.json()["result"]["resultId"], j["result"]["resultId"]);
    let missing = get(
        &s.app,
        &format!("{uri}?about={}", enc("http://ex.org/Missing")),
    )
    .await;
    assert_eq!(missing.json()["entity"]["present"], false);
}

#[tokio::test]
async fn kept_classification_http_selectors_refuse_ambiguous_or_computing_inputs() {
    let s = server();
    let dataset =
        crate::http::dataset(&s._state, "t").unwrap_or_else(|_| panic!("fixture dataset"));
    let head = dataset.store.snapshot().commit;
    for query in [
        "inferences=false",
        "assertions=false",
        "part=status&part=status",
        "limit=0",
        "limit=18446744073709551616",
        "offset=1&part=sources",
        "part=parents",
        "resultId=bogus",
        "unknown=x",
    ] {
        let r = get(&s.app, &format!("/$/ontology/t/classification?{query}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{query}: {}", r.text());
        assert_eq!(r.headers[header::CACHE_CONTROL], "no-store");
    }
    let ct = [(header::CONTENT_TYPE.as_str(), "application/json")];
    for (query, body) in [
        (
            "?nodes=ture",
            r#"{"projection":"frame","about":"http://ex.org/Professor"}"#,
        ),
        (
            "?assertions=maybe",
            r#"{"projection":"frame","about":"http://ex.org/Professor"}"#,
        ),
        (
            "?partial=maybe",
            r#"{"projection":"frame","about":"http://ex.org/Professor"}"#,
        ),
        (
            "?inferences=maybe",
            r#"{"projection":"frame","about":"http://ex.org/Professor"}"#,
        ),
        (
            "?projection=frame&imports=false&imports=true",
            r#"{"about":"http://ex.org/Professor"}"#,
        ),
        (
            "?imports=false",
            r#"{"projection":"frame","imports":true,"about":"http://ex.org/Professor"}"#,
        ),
        (
            "?mode=strict",
            r#"{"projection":"frame","mode":"lenient","about":"http://ex.org/Professor"}"#,
        ),
        (
            "?assertions=false&assertions=false",
            r#"{"projection":"frame","about":"http://ex.org/Professor"}"#,
        ),
        (
            "?inferences=false&inferences=false",
            r#"{"projection":"frame","about":"http://ex.org/Professor"}"#,
        ),
        (
            "?unknown=ignored",
            r#"{"projection":"frame","about":"http://ex.org/Professor"}"#,
        ),
        (
            "?graph=urn:query",
            r#"{"projection":"frame","graph":"urn:body","about":"http://ex.org/Professor"}"#,
        ),
        ("", "null"),
        (
            "",
            r#"{"projection":"frame","projection":"frame","about":"http://ex.org/Professor"}"#,
        ),
        (
            "?projection=frame",
            r#"{"about":"http://ex.org/Professor","limit":1,"limit":2}"#,
        ),
        (
            "?projection=frame&projection=frame",
            r#"{"about":"http://ex.org/Professor"}"#,
        ),
        (
            "?projection=frame",
            r#"{"projection":"frame","about":"http://ex.org/Professor"}"#,
        ),
        (
            "",
            r#"{"projection":"unknown","about":"http://ex.org/Professor"}"#,
        ),
        (
            "",
            r#"{"projection":"frame","about":"http://ex.org/Professor","nodes":true}"#,
        ),
        (
            "",
            r#"{"projection":"frame","about":"http://ex.org/Professor","target":"http://ex.org/out"}"#,
        ),
        (
            "?format=ofn",
            r#"{"projection":"frame","about":"http://ex.org/Professor"}"#,
        ),
    ] {
        let r = send(
            &s.app,
            "POST",
            &format!("/$/ontology/t/classify{query}"),
            &ct,
            body,
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::BAD_REQUEST,
            "{query} {body}: {}",
            r.text()
        );
    }
    assert_eq!(
        get(&s.app, "/$/ontology/t/classification").await.json()["status"],
        "unavailable"
    );
    assert_eq!(dataset.store.snapshot().commit, head);
    assert!(
        !dataset
            .dataset
            .ontology()
            .open(&Default::default(), &Default::default())
            .unwrap()
            .cached,
        "invalid selectors must not map or warm the ontology"
    );
    let r = post(&s.app, "/$/ontology/t/classify", json!({"projection":"frame", "about":"http://ex.org/Professor", "nodes":false,"offset":0,"target":null,"limit":1,"lang":["en"]})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert!(j.get("nodes").is_none() && j.get("class").is_none(), "{j}");
    assert_eq!(
        j["frame"]["parents"]["items"][0]["iri"],
        "http://ex.org/Person"
    );
    assert_eq!(j["frame"]["retention"]["kept"], true);
}

#[tokio::test]
async fn kept_classification_http_branch_and_retained_history_are_exact() {
    let s = persistent();
    let ds = crate::http::dataset(&s._state, "h").unwrap_or_else(|_| panic!("fixture dataset"));
    ds.dataset
        .store()
        .create_branch("kept-dev", &Default::default())
        .unwrap();
    let run = post(&s.app, "/$/ontology/h/classify?branch=kept-dev", json!({})).await;
    assert_eq!(run.status, StatusCode::OK, "{}", run.text());
    let read = get(&s.app, "/$/ontology/h/classification?branch=kept-dev").await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.text());
    assert_eq!(read.headers["sparkles-branch"], "kept-dev");
    let j = read.json();
    assert_eq!(j["status"], "available");
    assert_eq!(
        get(&s.app, "/$/ontology/h/classification").await.json()["status"],
        "unavailable"
    );
    let edit = post(
        &s.app,
        "/$/ontology/h/axioms?branch=kept-dev",
        json!({"add":[sub("http://ex.org/Lecturer", json!("http://ex.org/Person"))]}),
    )
    .await;
    assert_eq!(edit.status, StatusCode::OK, "{}", edit.text());
    let current = get(&s.app, "/$/ontology/h/classification?branch=kept-dev").await;
    assert_eq!(current.json()["status"], "mismatch");
    let pinned = get(
        &s.app,
        &format!(
            "/$/ontology/h/classification?branch=kept-dev&at=commit:{}&resultId={}",
            j["commit"],
            j["result"]["resultId"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(pinned.status, StatusCode::OK, "{}", pinned.text());
    assert_eq!(pinned.json()["status"], "available");
    assert_eq!(pinned.json()["result"], j["result"]);
}

/// Item 603's cases in Turtle, as the `sparkles` crate's tests write them: a classification
/// that the disjunctive rules take, long enough that a deadline stops it part way.
fn cases(n: usize, inconsistent: bool) -> String {
    let mut s = String::from(
        "@prefix : <http://ex.org/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
:r a owl:ObjectProperty . :s a owl:ObjectProperty .\n",
    );
    let some = |p: &str, c: &str| {
        format!("[ a owl:Restriction ; owl:onProperty {p} ; owl:someValuesFrom {c} ]")
    };
    for i in 0..n {
        let (b, d) = ((7 * i + 3) % n, (13 * i + 5) % n);
        s.push_str(&format!(
            ":C{i} a owl:Class ; rdfs:subClassOf [ owl:unionOf ( :B{i} :E{i} ) ] .
             :B{i} rdfs:subClassOf :K{i} , :C{b} . :E{i} rdfs:subClassOf :K{i} .
             :K{i} rdfs:subClassOf {} .
             :E{i} rdfs:subClassOf [ a owl:Restriction ;
                 owl:onProperty [ owl:inverseOf :r ] ; owl:allValuesFrom :F{d} ] .
             {} rdfs:subClassOf :G{b} .\n",
            some(":r", &format!(":C{d}")),
            some(":r", &format!(":F{i}")),
        ));
    }
    if inconsistent {
        s.push_str(&format!(
            "owl:Thing rdfs:subClassOf {} .
             :N1 rdfs:subClassOf owl:Nothing . :N2 rdfs:subClassOf :N3 . :N3 owl:disjointWith :N2 .\n",
            some(":s", "[ owl:unionOf ( :N1 :N2 ) ]"),
        ));
    }
    s
}

/// A server whose dataset `t` holds `data` in Turtle.
fn server_of(data: &str) -> Server {
    server_with_memory(data, None)
}

/// A server whose dataset `t` holds `data` in Turtle, and whose queries have a memory budget
/// of `memory` bytes in place of the default one when it is given.
fn server_with_memory(data: &str, memory: Option<u64>) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    if memory.is_some() {
        st.limits.query_memory_bytes = memory;
    }
    let state = Arc::new(st);
    let ds = state.attach("t", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            data.as_bytes().to_vec(),
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

/// The size of the cases that a debug build takes.
fn case_count() -> usize {
    if cfg!(debug_assertions) { 120 } else { 400 }
}

/// The full answer to the cases on one thread, from a server of its own, and the time of
/// the call.
async fn whole_over_http(inconsistent: bool) -> (serde_json::Value, f64) {
    let body = json!({"partial": true, "threads": 1, "limit": 10000});
    let t = std::time::Instant::now();
    let full = post(
        &server_of(&cases(case_count(), inconsistent)).app,
        "/$/ontology/t/classify",
        body,
    )
    .await
    .json();
    let took = t.elapsed().as_secs_f64();
    assert_eq!(full["complete"], true, "{full}");
    assert_eq!(full["partial"], false, "{full}");
    assert_eq!(full["tier"], "disjunctive", "{full}");
    let want = if inconsistent {
        "inconsistent"
    } else {
        "consistent"
    };
    assert_eq!(full["consistency"], want, "{full}");
    assert_eq!(full["consistent"], !inconsistent, "{full}");
    (full, took)
}

/// Classifies the cases on one thread on a server of its own, which keeps a complete
/// answer for the next call, with the query string `query` and the memory budget
/// `memory`. Checks the answer against the full one and returns the consistency of a
/// partial answer, `Some(None)` for a complete one, and `None` when the call ended with the
/// status `refused`.
async fn stopped_over_http(
    inconsistent: bool,
    full: &serde_json::Value,
    query: &str,
    memory: Option<u64>,
    refused: StatusCode,
) -> Option<Option<String>> {
    let s = server_with_memory(&cases(case_count(), inconsistent), memory);
    let r = post(
        &s.app,
        &format!("/$/ontology/t/classify{query}"),
        json!({"partial": true, "threads": 1, "limit": 10000}),
    )
    .await;
    if r.status == refused {
        // a stop before the saturation fails as before: a deadline times out, and a
        // budget under the size of the ontology and its index is refused
        return None;
    }
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let want = if inconsistent {
        "inconsistent"
    } else {
        "consistent"
    };
    let j = r.json();
    assert_eq!(j["classes"], full["classes"], "{j}");
    if j["partial"] == false {
        if j["complete"] == false {
            // a budget past the one of the disjunctive rules runs the EL rules instead,
            // whose answer leaves the disjunctive axioms out
            assert!(j["excludedCount"].as_u64() > Some(0), "{j}");
            return None;
        }
        assert_eq!(j["consistency"], want, "{j}");
        assert_eq!(j["incompleteCount"], 0, "{j}");
        return Some(None);
    }
    assert_eq!(
        j["complete"], false,
        "a partial answer is presented as complete: {j}"
    );
    let open = j["incomplete"].as_array().unwrap();
    assert_eq!(
        j["incompleteCount"].as_u64(),
        Some(open.len() as u64),
        "{j}"
    );
    let c = j["consistency"].as_str().unwrap().to_string();
    assert!(c == want || c == "unknown", "{j}");
    if c == "unknown" {
        // every class but owl:Thing and owl:Nothing is open
        assert!(
            open.len() as u64 + 2 >= j["classes"].as_u64().unwrap(),
            "{j}"
        );
    }
    Some(Some(c))
}

/// Classifies the cases over HTTP under deadlines that find the saturation, checks each
/// answer and returns the consistencies of the partial ones. The deadlines rise by 8
/// percent from 2 ms until one of them gives an answer instead of a timeout, which has
/// run out in the saturation, since that is the first part of a run that can stop with an
/// answer. The step may be longer than the saturation, so the milliseconds between the
/// last timeout and that answer are halved until they are one apart. The deadlines go on
/// until twenty have answered. A ladder fixed from the time of a full run misses a
/// saturation of some milliseconds on a loaded machine.
async fn partial_over_http(inconsistent: bool) -> Vec<String> {
    let (full, took) = whole_over_http(inconsistent).await;
    let limit = ((took * 20000.0) as u64).max(2000);
    let after = |ms: u64| ms + (ms * 8 / 100).max(1);
    let mut seen = Vec::new();
    let mut answered = 0;
    // whether a classification under a deadline of `ms` milliseconds answered
    async fn run(
        inconsistent: bool,
        full: &serde_json::Value,
        ms: u64,
        answered: &mut usize,
        seen: &mut Vec<String>,
    ) -> bool {
        let query = format!("?timeout={}", ms as f64 / 1000.0);
        let r = stopped_over_http(
            inconsistent,
            full,
            &query,
            None,
            StatusCode::REQUEST_TIMEOUT,
        )
        .await;
        if let Some(c) = &r {
            *answered += 1;
            seen.extend(c.clone());
        }
        r.is_some()
    }
    let (mut lo, mut ms) = (0, 2);
    while ms < limit && !run(inconsistent, &full, ms, &mut answered, &mut seen).await {
        lo = ms;
        ms = after(ms);
    }
    let mut hi = ms;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if run(inconsistent, &full, mid, &mut answered, &mut seen).await {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    while answered < 20 && ms < limit {
        ms = after(ms);
        run(inconsistent, &full, ms, &mut answered, &mut seen).await;
    }
    seen
}

/// Classifies the cases over HTTP under the server's memory budget, from a twentieth of
/// the full run's estimate to nearly all of it, checks each answer and returns the
/// consistencies of the partial ones. A run stops at the same place under the same budget
/// on every machine, where a deadline stops it at a place that depends on the speed of
/// the machine and its load.
async fn partial_over_http_under_budgets(inconsistent: bool) -> Vec<String> {
    let (full, _) = whole_over_http(inconsistent).await;
    let estimate = full["stats"]["estimatedBytes"].as_u64().unwrap();
    let mut seen = Vec::new();
    for k in 1..20 {
        let r = stopped_over_http(
            inconsistent,
            &full,
            "",
            Some(estimate * k / 20),
            StatusCode::INSUFFICIENT_STORAGE,
        )
        .await;
        seen.extend(r.flatten());
    }
    seen
}

#[tokio::test]
async fn classify_answers_in_part_under_a_deadline() {
    let seen = partial_over_http(false).await;
    assert!(!seen.is_empty(), "no deadline gave a partial answer");
    let seen = partial_over_http_under_budgets(false).await;
    assert!(!seen.is_empty(), "no budget gave a partial answer");
    let s = server();
    // a partial answer cannot be written or answered as a functional-style document
    let r = post(
        &s.app,
        "/$/ontology/t/classify",
        json!({"partial": true, "target": "http://ex.org/tax"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = post(
        &s.app,
        "/$/ontology/t/classify?format=ofn&partial=true",
        json!({}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = post(
        &s.app,
        "/$/ontology/t/classify",
        json!({"expansion": "greedy"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // a complete run of an EL ontology, with either strategy
    for e in ["cautious", "eager"] {
        let j = post(
            &s.app,
            "/$/ontology/t/classify",
            json!({"partial": true, "expansion": e}),
        )
        .await
        .json();
        assert_eq!(
            (
                &j["complete"],
                &j["partial"],
                &j["consistency"],
                &j["tier"],
                &j["incomplete"]
            ),
            (
                &json!(true),
                &json!(false),
                &json!("consistent"),
                &json!("el"),
                &json!([])
            ),
            "{j}"
        );
    }
}

#[tokio::test]
async fn classify_leaves_the_consistency_unknown_when_stopped_early() {
    let seen = partial_over_http_under_budgets(true).await;
    assert!(
        seen.iter().any(|c| c == "unknown"),
        "no budget left the consistency unknown: {seen:?}"
    );
    // the deadlines check every answer, and a stop of the clock may or may not come
    // before the inconsistency is found
    partial_over_http(true).await;
}

/// Every quad of dataset `t`, as sorted N-Quads lines.
async fn quads(app: &Router, ds: &str) -> Vec<String> {
    let r = send(
        app,
        "GET",
        &format!("/{ds}/data"),
        &[("accept", "application/n-quads")],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mut l: Vec<String> = r
        .text()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    l.sort();
    l
}

/// The inverse of an edit's answer: add back what it removed, remove what it wrote.
fn inverse(j: &J) -> J {
    let axioms = |k: &str| -> Vec<J> {
        j[k].as_array()
            .unwrap()
            .iter()
            .map(|a| a["axiom"].clone())
            .collect()
    };
    json!({"add": axioms("removedAxioms"), "remove": axioms("addedAxioms"), "declare": false})
}

const UNRELATED: &str = "@prefix : <http://ex.org/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix dct: <http://purl.org/dc/terms/> .
:o dct:creator \"Ada\" .
:Professor rdfs:comment \"teaches\"@en ; :note [ :about :Course ] .
:logic :credits 6 .
[] a owl:Restriction ; owl:onProperty :teaches .
";

#[tokio::test]
async fn edits_list_what_they_wrote_and_their_inverse_restores_the_graph() {
    let s = server();
    let r = send(
        &s.app,
        "POST",
        "/t/data?default",
        &[("content-type", "text/turtle")],
        UNRELATED,
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let before = quads(&s.app, "t").await;
    // a new class under Person: the subclass axiom and the declaration it needs
    let added = sub("http://ex.org/Lecturer", json!("http://ex.org/Person"));
    let present = sub("http://ex.org/Professor", json!("http://ex.org/Person"));
    let r = post(
        &s.app,
        "/$/ontology/t/axioms",
        json!({"add": [added, present]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let types: Vec<&str> = j["addedAxioms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["SubClassOf", "Declaration"], "{j}");
    assert_eq!(j["present"], 1, "{j}");
    assert_eq!(j["removedAxioms"], json!([]));
    assert_eq!(j["axiomsTruncated"], false);
    assert_eq!(j["addedAxioms"][0]["ofn"], "SubClassOf(:Lecturer :Person)");
    // the inverse removes the two axioms and nothing else
    let r = post(&s.app, "/$/ontology/t/axioms", inverse(&j)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["removed"], 2, "{}", r.text());
    assert_eq!(quads(&s.app, "t").await, before);

    // annotating an axiom replaces it: the plain axiom out, the annotated one in
    let annotated = json!({"type": "SubClassOf", "subClass": "http://ex.org/Professor",
        "superClass": "http://ex.org/Person",
        "annotations": [{"property": "http://www.w3.org/2000/01/rdf-schema#comment",
                         "value": {"@value": "by definition", "@language": "en"}}]});
    let r = post(
        &s.app,
        "/$/ontology/t/axioms",
        json!({"remove": [present], "add": [annotated]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["removedAxioms"][0]["axiom"], present, "{j}");
    assert_eq!(
        j["addedAxioms"][0]["axiom"]["annotations"][0]["value"]["@language"],
        "en"
    );
    // every quad from before is still there, and the new ones describe the owl:Axiom node
    let after = quads(&s.app, "t").await;
    for q in &before {
        assert!(after.contains(q), "lost {q}");
    }
    let new: Vec<&String> = after.iter().filter(|q| !before.contains(q)).collect();
    assert_eq!(new.len(), 5, "{new:?}");
    let node = new[0].split(' ').next().unwrap();
    assert!(node.starts_with("_:"), "{new:?}");
    assert!(new.iter().all(|q| q.starts_with(node)), "{new:?}");
    // and its inverse restores the graph exactly
    let r = post(&s.app, "/$/ontology/t/axioms", inverse(&j)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(quads(&s.app, "t").await, before);
}

#[tokio::test]
async fn entities_are_removed_with_their_axioms() {
    let s = server();
    let course = "/$/ontology/t/entity?iri=http%3A%2F%2Fex.org%2FCourse";
    let before = quads(&s.app, "t").await;
    let r = send(
        &s.app,
        "DELETE",
        &format!("{course}&dryRun=true&changes=20"),
        &[],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["dryRun"], true, "{}", r.text());
    assert!(
        r.json()["changes"]["total"].as_u64().unwrap() > 0,
        "{}",
        r.text()
    );
    assert_eq!(quads(&s.app, "t").await, before);
    // a stale tag removes nothing
    let r = send(
        &s.app,
        "DELETE",
        course,
        &[(
            "if-match",
            "W/\"00000000-0000-0000-0000-000000000000:0:owl\"",
        )],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED, "{}", r.text());
    let tag = get(&s.app, "/$/ontology/t").await.headers[header::ETAG]
        .to_str()
        .unwrap()
        .to_string();
    let r = send(
        &s.app,
        "DELETE",
        &format!("{course}&message=drop%20courses"),
        &[("if-match", &tag)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let removed: Vec<&str> = j["removedAxioms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["ofn"].as_str().unwrap())
        .collect();
    assert!(removed.contains(&"Declaration(Class(:Course))"), "{j}");
    assert!(
        removed.contains(&"SubClassOf(:Professor ObjectSomeValuesFrom(:teaches :Course))"),
        "{j}"
    );
    assert_eq!(get(&s.app, course).await.status, StatusCode::NOT_FOUND);
    let c = get(&s.app, "/$/commits/t?limit=1").await.json();
    assert_eq!(c["commits"][0]["message"], "drop courses", "{c}");
    // adding back what it removed restores the graph's axioms
    let back = json!({"add": j["removedAxioms"].as_array().unwrap().iter().map(|a| a["axiom"].clone()).collect::<Vec<_>>(), "declare": false});
    let r = post(&s.app, "/$/ontology/t/axioms", back).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(get(&s.app, course).await.status, StatusCode::OK);
    assert_eq!(quads(&s.app, "t").await.len(), before.len());
    // an IRI the ontology does not mention removes nothing
    let r = send(
        &s.app,
        "DELETE",
        "/$/ontology/t/entity?iri=http%3A%2F%2Fex.org%2FNone",
        &[],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["removed"], 0);
    assert_eq!(r.json()["committed"], false);
    let r = send(&s.app, "DELETE", "/$/ontology/t/entity", &[], "").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = send(
        &s.app,
        "DELETE",
        &format!("{course}&inferences=true"),
        &[],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn the_ontology_routes_work_on_a_branch() {
    let s = persistent();
    s._state
        .get("h")
        .unwrap()
        .dataset
        .create_branch("dev", &Default::default())
        .unwrap();
    let main_tag = get(&s.app, "/$/ontology/h").await.headers[header::ETAG]
        .to_str()
        .unwrap()
        .to_string();
    let r = get(&s.app, "/$/ontology/h?branch=dev").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.headers["sparkles-branch"], "dev");
    let dev_tag = r.headers[header::ETAG].to_str().unwrap().to_string();
    assert_ne!(dev_tag, main_tag);
    let body =
        json!({"add": [sub("http://ex.org/Lecturer", json!("http://ex.org/Person"))]}).to_string();
    let ct = (header::CONTENT_TYPE.as_str(), "application/json");
    // main's tag does not name the branch's head
    let r = send(
        &s.app,
        "POST",
        "/$/ontology/h/axioms?branch=dev",
        &[ct, ("if-match", &main_tag)],
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED, "{}", r.text());
    let r = send(
        &s.app,
        "POST",
        "/$/ontology/h/axioms?branch=dev",
        &[ct, ("if-match", &dev_tag)],
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let lecturer = "iri=http%3A%2F%2Fex.org%2FLecturer";
    let r = get(
        &s.app,
        &format!("/$/ontology/h/entity?branch=dev&{lecturer}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        get(&s.app, &format!("/$/ontology/h/entity?{lecturer}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    for uri in [
        "/$/ontology/h/entities?branch=dev&kind=Class",
        "/$/ontology/h/axioms?branch=dev&type=SubClassOf",
    ] {
        let r = get(&s.app, uri).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        assert!(r.text().contains("Lecturer"), "{uri}: {}", r.text());
    }
    let r = post(
        &s.app,
        "/$/ontology/h/classify?branch=dev",
        json!({"about": "http://ex.org/Lecturer"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = post(&s.app, "/$/ontology/h/check?branch=dev", json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = send(
        &s.app,
        "DELETE",
        &format!("/$/ontology/h/entity?branch=dev&{lecturer}"),
        &[],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["removed"], 2, "{}", r.text());
    // a protected branch takes changes through merges only
    s._state
        .get("h")
        .unwrap()
        .dataset
        .set_branch_protected("dev", true)
        .unwrap();
    let r = send(
        &s.app,
        "POST",
        "/$/ontology/h/axioms?branch=dev",
        &[ct],
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.json()["code"], "branch-protected", "{}", r.text());
    let r = get(&s.app, "/$/ontology/h?branch=nope").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
}

#[tokio::test]
async fn edits_take_functional_syntax() {
    let s = server();
    let r = post(
        &s.app,
        "/$/ontology/t/axioms",
        json!({"add": ["SubClassOf(<http://ex.org/Tutor> ObjectSomeValuesFrom(<http://ex.org/teaches> <http://ex.org/Course>))"]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["addedAxioms"][0]["type"],
        "SubClassOf",
        "{}",
        r.text()
    );
    let r = post(
        &s.app,
        "/$/ontology/t/axioms",
        json!({"add": ["SubClassOf(<http://ex.org/Tutor>"]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(r.json()["code"], "invalid-axiom", "{}", r.text());
}

#[tokio::test]
async fn parse_reads_manchester_and_reports_the_span() {
    let s = server();
    let r = post(
        &s.app,
        "/$/ontology/t/parse",
        json!({"text": "Person and teaches some Course"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["as"], "classExpression", "{j}");
    // the dataset's prefixes write the names, which read back as they are written
    assert_eq!(
        j["manchester"], ":Person and (:teaches some :Course)",
        "{j}"
    );
    assert_eq!(j["value"]["type"], "ObjectIntersectionOf", "{j}");
    assert_eq!(
        j["ofn"], "ObjectIntersectionOf(:Person ObjectSomeValuesFrom(:teaches :Course))",
        "{j}"
    );
    let r = post(
        &s.app,
        "/$/ontology/t/parse",
        json!({"text": "Professor SubClassOf Person", "as": "axiom"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["value"]["type"], "SubClassOf");
    // the span counts UTF-16 code units: the emoji before the name takes two
    let r = post(
        &s.app,
        "/$/ontology/t/parse",
        json!({"text": "'😀' or Robot"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let j = r.json();
    assert_eq!(j["code"], "invalid-manchester", "{j}");
    assert_eq!(j["reason"], "unknown", "{j}");
    assert_eq!(
        (j["start"].as_u64(), j["end"].as_u64()),
        (Some(0), Some(4)),
        "{j}"
    );
    let r = post(
        &s.app,
        "/$/ontology/t/parse",
        json!({"text": "x", "as": "frame"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = post(&s.app, "/$/ontology/t/parse", json!({"txt": "x"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn dl_query_answers_with_the_reasoning_used() {
    let s = server();
    let r = post(
        &s.app,
        "/$/ontology/t/dl-query",
        json!({"expression": "teaches some Course"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.headers.contains_key("sparkles-commit"), "{:?}", r.headers);
    let j = r.json();
    assert_eq!(j["reasoning"]["engine"], "el", "{j}");
    assert_eq!(j["reasoning"]["complete"], true, "{j}");
    assert_eq!(j["subclasses"], json!(["http://ex.org/Professor"]), "{j}");
    assert_eq!(
        j["directSubclasses"],
        json!(["http://ex.org/Professor"]),
        "{j}"
    );
    assert_eq!(j["instances"], json!(["http://ex.org/ada"]), "{j}");
    assert_eq!(
        j["superclasses"],
        json!(["http://www.w3.org/2002/07/owl#Thing"]),
        "{j}"
    );
    assert_eq!(j["counts"]["subclasses"], 1, "{j}");
    // the functional-style and JSON forms, and the RL engine by name
    let r = post(
        &s.app,
        "/$/ontology/t/dl-query",
        json!({
            "expression": "ObjectSomeValuesFrom(<http://ex.org/teaches> <http://ex.org/Course>)",
            "syntax": "functional",
            "reasoning": "rl",
            "answers": ["subclasses", "instances"],
        }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["reasoning"]["engine"], "rl", "{j}");
    assert_eq!(j["instances"], json!(["http://ex.org/ada"]), "{j}");
    assert!(j.get("superclasses").is_none(), "{j}");
    let r = post(
        &s.app,
        "/$/ontology/t/dl-query",
        json!({"expression": {"type": "ObjectSomeValuesFrom", "property": "http://ex.org/teaches", "class": "http://ex.org/Course"}, "answers": "subclasses"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // an expression that does not read, an answer and an engine the route does not know
    let r = post(
        &s.app,
        "/$/ontology/t/dl-query",
        json!({"expression": "teaches some"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(r.json()["code"], "invalid-manchester", "{}", r.text());
    for body in [
        json!({"expression": "Course", "answers": ["cousins"]}),
        json!({"expression": "Course", "reasoning": "dl"}),
        json!({}),
    ] {
        let r = post(&s.app, "/$/ontology/t/dl-query", body).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    }
}

#[tokio::test]
async fn the_entity_search_ranks_typeahead_hits() {
    let s = server();
    let r = get(&s.app, "/$/ontology/t/search?q=prof").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.headers.contains_key("sparkles-commit"));
    let j = r.json();
    assert_eq!(j["results"][0]["iri"], "http://ex.org/Professor", "{j}");
    assert_eq!(j["results"][0]["type"], "entity");
    assert_eq!(j["results"][0]["kinds"], json!(["class"]));
    assert_eq!(j["results"][0]["match"]["field"], "shortForm");
    assert_eq!(j["results"][0]["match"]["highlight"], json!([[0, 4]]));
    assert_eq!(j["index"], "built");
    // a typo, a kind filter, a page and the languages of Accept-Language
    let j = get(&s.app, "/$/ontology/t/search?q=Profesor").await.json();
    assert_eq!(j["results"][0]["match"]["kind"], "fuzzy", "{j}");
    assert_eq!(j["index"], "current");
    let j = get(&s.app, "/$/ontology/t/search?q=teach&kind=property")
        .await
        .json();
    assert_eq!(j["results"][0]["iri"], "http://ex.org/teaches", "{j}");
    assert_eq!(
        get(&s.app, "/$/ontology/t/search?q=teach&kind=class")
            .await
            .json()["results"],
        json!([])
    );
    let r = send(
        &s.app,
        "GET",
        "/$/ontology/t/search?q=teaching",
        &[("accept-language", "fr-CH, fr;q=0.9, en;q=0.5, *;q=0.1")],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // a commit is searched at once
    let r = send(
        &s.app,
        "POST",
        "/t/update",
        &[(header::CONTENT_TYPE.as_str(), "application/sparql-update")],
        "INSERT DATA { <http://ex.org/Lecturer> a <http://www.w3.org/2002/07/owl#Class> ; <http://www.w3.org/2000/01/rdf-schema#label> \"lecturer\"@en }",
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let j = get(&s.app, "/$/ontology/t/search?q=lect&limit=1")
        .await
        .json();
    assert_eq!(j["results"][0]["label"], "lecturer", "{j}");
    assert_eq!(j["results"][0]["labelLang"], "en", "{j}");
    assert_ne!(j["index"], "current", "{j}");
    // bad requests
    for uri in [
        "/$/ontology/t/search",
        "/$/ontology/t/search?q=x&limit=0",
        "/$/ontology/t/search?q=x&limit=101",
        "/$/ontology/t/search?q=x&kind=planet",
        "/$/ontology/t/search?q=x&lang=not_a_tag!",
        "/$/ontology/t/search?q=x&at=1",
        "/$/ontology/t/search?q=x&offset=10000",
    ] {
        let r = get(&s.app, uri).await;
        assert!(
            r.status.is_client_error(),
            "{uri}: {} {}",
            r.status,
            r.text()
        );
    }
}

#[tokio::test]
async fn the_entity_search_reads_the_branch_asked_for() {
    let s = persistent();
    s._state
        .get("h")
        .unwrap()
        .dataset
        .create_branch("dev", &Default::default())
        .unwrap();
    let r = post(
        &s.app,
        "/$/ontology/h/axioms?branch=dev",
        json!({"add": [sub("http://ex.org/Lecturer", json!("http://ex.org/Person"))]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = get(&s.app, "/$/ontology/h/search?q=lectur&branch=dev").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.headers["sparkles-branch"], "dev");
    assert_eq!(r.json()["results"][0]["iri"], "http://ex.org/Lecturer");
    let j = get(&s.app, "/$/ontology/h/search?q=lectur").await.json();
    assert_eq!(j["results"], json!([]), "{j}");
}

#[tokio::test]
async fn the_hierarchy_answers_one_level_at_a_time() {
    let s = server();
    let r = get(&s.app, "/$/ontology/t/hierarchy").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.headers.contains_key("sparkles-commit"));
    let tag = r.headers[header::ETAG].to_str().unwrap().to_string();
    let j = r.json();
    assert_eq!(j["kind"], "class");
    assert_eq!(j["roots"], "computed");
    let roots: Vec<&str> = j["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["iri"].as_str().unwrap())
        .collect();
    assert_eq!(
        roots,
        ["http://ex.org/Course", "http://ex.org/Person"],
        "{j}"
    );
    assert_eq!(j["children"][1]["children"], 1);
    // the same commit is not sent again
    let r = send(
        &s.app,
        "GET",
        "/$/ontology/t/hierarchy",
        &[("if-none-match", &tag)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    // each route's own tag is a valid precondition of a read
    for uri in [
        "/$/ontology/t/hierarchy".to_string(),
        format!("/$/ontology/t/term?iri={}", enc("http://ex.org/Person")),
    ] {
        let tag = get(&s.app, &uri).await.headers[header::ETAG]
            .to_str()
            .unwrap()
            .to_string();
        let r = send(&s.app, "GET", &uri, &[("if-match", &tag)], "").await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
    }
    let j = get(
        &s.app,
        &format!(
            "/$/ontology/t/hierarchy?iri={}&ancestors=true",
            enc("http://ex.org/Professor")
        ),
    )
    .await
    .json();
    assert_eq!(j["node"]["parents"], 1, "{j}");
    assert_eq!(j["children"], json!([]));
    assert_eq!(j["ancestors"]["nodes"][0]["iri"], "http://ex.org/Person");
    assert_eq!(
        j["ancestors"]["edges"],
        json!([["http://ex.org/Professor", "http://ex.org/Person"]])
    );
    let j = get(&s.app, "/$/ontology/t/hierarchy?kind=objectProperty")
        .await
        .json();
    assert_eq!(j["children"][0]["iri"], "http://ex.org/teaches", "{j}");
    for uri in [
        "/$/ontology/t/hierarchy?kind=planet",
        "/$/ontology/t/hierarchy?limit=0",
        "/$/ontology/t/hierarchy?limit=x",
        "/$/ontology/t/hierarchy?inferences=true",
    ] {
        let r = get(&s.app, uri).await;
        assert!(
            r.status.is_client_error(),
            "{uri}: {} {}",
            r.status,
            r.text()
        );
    }
    let r = get(
        &s.app,
        &format!("/$/ontology/t/hierarchy?iri={}", enc("http://ex.org/none")),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
}

#[tokio::test]
async fn the_term_route_describes_one_entity_and_downloads_it() {
    let s = server();
    let iri = enc("http://ex.org/Professor");
    let r = get(&s.app, &format!("/$/ontology/t/term?iri={iri}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["iri"], "http://ex.org/Professor");
    assert_eq!(j["kinds"], json!(["class"]));
    assert_eq!(j["ontologies"][0]["ontology"], "http://ex.org/o");
    let types: Vec<&str> = j["axioms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["type"].as_str().unwrap())
        .collect();
    assert!(types.contains(&"SubClassOf"), "{j}");
    let instances = j["incoming"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["property"] == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type")
        .unwrap();
    assert_eq!(instances["subjects"], json!(["http://ex.org/ada"]));
    assert!(j["names"]["http://ex.org/teaches"].is_object(), "{j}");
    // the course is mentioned by the professor's restriction
    let j = get(
        &s.app,
        &format!("/$/ontology/t/term?iri={}", enc("http://ex.org/Course")),
    )
    .await
    .json();
    assert_eq!(j["mentions"]["total"], 1, "{j}");
    assert_eq!(j["mentions"]["axioms"][0]["type"], "SubClassOf");
    // downloads
    for (f, mime, needle) in [
        ("ttl", "text/turtle", "subClassOf"),
        ("nt", "application/n-triples", "<http://ex.org/Professor>"),
        ("rdfxml", "application/rdf+xml", "rdf:RDF"),
        ("jsonld", "application/ld+json", "@id"),
        ("ofn", "text/owl-functional", "SubClassOf("),
    ] {
        let r = get(&s.app, &format!("/$/ontology/t/term?iri={iri}&format={f}")).await;
        assert_eq!(r.status, StatusCode::OK, "{f}: {}", r.text());
        assert!(
            r.headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with(mime),
            "{f}"
        );
        assert!(
            r.headers[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .contains("attachment"),
            "{f}"
        );
        assert!(r.text().contains(needle), "{f}: {}", r.text());
    }
    for uri in [
        "/$/ontology/t/term".to_string(),
        format!("/$/ontology/t/term?iri={iri}&format=xls"),
        format!("/$/ontology/t/term?iri={iri}&mentions=999999"),
    ] {
        let r = get(&s.app, &uri).await;
        assert!(
            r.status.is_client_error(),
            "{uri}: {} {}",
            r.status,
            r.text()
        );
    }
    let r = get(
        &s.app,
        &format!("/$/ontology/t/term?iri={}", enc("http://ex.org/none")),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_browser_routes_read_the_branch_asked_for() {
    let s = persistent();
    s._state
        .get("h")
        .unwrap()
        .dataset
        .create_branch("dev", &Default::default())
        .unwrap();
    let r = post(
        &s.app,
        "/$/ontology/h/axioms?branch=dev",
        json!({"add": [sub("http://ex.org/Lecturer", json!("http://ex.org/Person"))]}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let person = enc("http://ex.org/Person");
    let r = get(
        &s.app,
        &format!("/$/ontology/h/hierarchy?iri={person}&branch=dev"),
    )
    .await;
    assert_eq!(r.headers["sparkles-branch"], "dev");
    let kids: Vec<String> = r.json()["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["iri"].as_str().unwrap().to_string())
        .collect();
    assert!(
        kids.contains(&"http://ex.org/Lecturer".to_string()),
        "{kids:?}"
    );
    let j = get(&s.app, &format!("/$/ontology/h/hierarchy?iri={person}"))
        .await
        .json();
    assert!(
        !j["children"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["iri"] == "http://ex.org/Lecturer"),
        "{j}"
    );
    let r = get(
        &s.app,
        &format!(
            "/$/ontology/h/term?iri={}&branch=dev",
            enc("http://ex.org/Lecturer")
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = get(
        &s.app,
        &format!("/$/ontology/h/term?iri={}", enc("http://ex.org/Lecturer")),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn metrics_include_assertions_at_the_requested_revision_and_have_their_own_tag() {
    let s = persistent();
    let ordinary = get(&s.app, "/$/ontology/h").await;
    let commit = ordinary.json()["commit"].as_u64().unwrap();
    let tag = ordinary.headers[header::ETAG].to_str().unwrap();
    let r = send(
        &s.app,
        "GET",
        "/$/ontology/h?metrics=true",
        &[("if-none-match", tag)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["metrics"]["commit"], commit);
    assert_eq!(j["metrics"]["entities"]["NamedIndividual"], 2);
    assert_eq!(j["metrics"]["assertionOccurrences"]["ClassAssertion"], 1);
    assert_eq!(j["metrics"]["reports"].as_array().unwrap().len(), 4);
    assert_ne!(r.headers[header::ETAG], ordinary.headers[header::ETAG]);
    let update = send(
        &s.app,
        "POST",
        "/h/update",
        &[("content-type", "application/sparql-update")],
        "INSERT DATA { <http://ex.org/new> a <http://www.w3.org/2002/07/owl#Class> }",
    )
    .await;
    assert!(update.status.is_success(), "{}", update.text());
    let pinned = get(
        &s.app,
        &format!("/$/ontology/h?metrics=true&at=commit:{commit}"),
    )
    .await;
    assert_eq!(pinned.status, StatusCode::OK, "{}", pinned.text());
    assert_eq!(pinned.json()["metrics"], j["metrics"]);
    assert_eq!(
        get(&s.app, "/$/ontology/h?metrics=maybe").await.status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn individuals_route_pages_and_locates_with_revision_receipts() {
    let s = server();
    let root = get(&s.app, "/$/ontology/t/individuals").await;
    assert_eq!(root.status, StatusCode::OK, "{}", root.text());
    assert!(root.json()["individuals"].as_array().unwrap().is_empty());
    assert_eq!(root.json()["groups"][0]["type"], J::Null); // :logic only occurs as an object
    assert_eq!(
        root.json()["groups"][1]["type"]["iri"],
        "http://ex.org/Professor"
    );
    let uri = format!(
        "/$/ontology/t/individuals?type={}&limit=1",
        enc("http://ex.org/Professor")
    );
    let r = get(&s.app, &uri).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["individuals"][0]["iri"], "http://ex.org/ada");
    assert_eq!(r.json()["next"], J::Null);
    assert_eq!(
        r.headers["sparkles-commit"],
        root.headers["sparkles-commit"]
    );
    let located = get(
        &s.app,
        &format!("/$/ontology/t/individuals?iri={}", enc("http://ex.org/ada")),
    )
    .await;
    assert_eq!(
        located.json()["groups"][0]["type"]["iri"],
        "http://ex.org/Professor"
    );
    let tag = r.headers[header::ETAG].to_str().unwrap();
    let cached = send(&s.app, "GET", &uri, &[("If-None-Match", tag)], "").await;
    assert_eq!(cached.status, StatusCode::NOT_MODIFIED);
    for bad in [
        "limit=0",
        "limit=1001",
        "limit=x",
        "untyped=true&type=http%3A%2F%2Fex.org%2FProfessor",
        "inferences=true",
    ] {
        let r = get(&s.app, &format!("/$/ontology/t/individuals?{bad}")).await;
        assert!(
            r.status.is_client_error(),
            "{bad}: {} {}",
            r.status,
            r.text()
        );
    }
    let absent = get(
        &s.app,
        &format!(
            "/$/ontology/t/individuals?iri={}",
            enc("http://ex.org/Professor")
        ),
    )
    .await;
    assert_eq!(absent.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn individual_pages_read_only_the_selected_branch_and_retained_revision() {
    let s = persistent();
    s._state
        .get("h")
        .unwrap()
        .dataset
        .create_branch("individual-dev", &Default::default())
        .unwrap();
    let before = get(&s.app, "/$/ontology/h/individuals?branch=individual-dev")
        .await
        .json()["commit"]
        .as_u64()
        .unwrap();
    let r = post(&s.app, "/$/ontology/h/axioms?branch=individual-dev", json!({"add": [{"type":"ClassAssertion", "class":"http://ex.org/Person", "individual":"http://ex.org/new-person"}]})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let base = format!(
        "/$/ontology/h/individuals?type={}",
        enc("http://ex.org/Person")
    );
    let dev = get(&s.app, &format!("{base}&branch=individual-dev")).await;
    assert_eq!(dev.headers["sparkles-branch"], "individual-dev");
    assert!(dev.text().contains("new-person"), "{}", dev.text());
    let main = get(&s.app, &base).await;
    assert!(!main.text().contains("new-person"), "{}", main.text());
    let old = get(
        &s.app,
        &format!("{base}&branch=individual-dev&at=commit:{before}"),
    )
    .await;
    assert_eq!(old.status, StatusCode::OK, "{}", old.text());
    assert!(!old.text().contains("new-person"));
    assert_eq!(old.json()["commit"], before);
}

#[tokio::test]
async fn inferred_browse_materialization_sources_are_read_only_pinned_and_never_commit_cached() {
    let s = server();
    let ds = s._state.get("t").unwrap();
    let outcome = ds
        .dataset
        .reasoning()
        .run(&sparkles::reasoning::ReasonRequest {
            profile: sparkles_reasoner::Profile::Rdfs,
            ..Default::default()
        })
        .unwrap();
    let _ = outcome;
    let commit = ds.store.head_commit().seq;
    let uri = format!(
        "/$/ontology/t/hierarchy?inferences=true&at=commit:{commit}&iri={}",
        enc("http://ex.org/Person")
    );
    let page = send(&s.app, "GET", &uri, &[("if-none-match", "*")], "").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text());
    assert_eq!(page.headers[header::CACHE_CONTROL], "no-store");
    assert!(page.headers.get(header::ETAG).is_none());
    let source = page.json()["inferenceSource"].clone();
    assert_eq!(source["kind"], "materialization");
    assert_eq!(source["profile"], "rdfs");
    let pin = enc(source["sourceId"].as_str().unwrap());
    let term = get(
        &s.app,
        &format!(
            "/$/ontology/t/term?inferences=true&at=commit:{commit}&sourceId={pin}&iri={}",
            enc("http://ex.org/ada")
        ),
    )
    .await;
    assert_eq!(term.status, StatusCode::OK, "{}", term.text());
    assert_eq!(term.json()["inferenceSource"], source);
    assert_eq!(term.headers[header::CACHE_CONTROL], "no-store");
    let kept = get(
        &s.app,
        &format!("/$/ontology/t/classification?at=commit:{commit}"),
    )
    .await;
    assert_eq!(kept.json()["status"], "unavailable");
    assert_eq!(ds.store.head_commit().seq, commit);
    ds.dataset.reasoning().clear().unwrap();
    let missing = get(&s.app, &uri).await;
    assert_ne!(missing.status, StatusCode::OK);
    assert_eq!(missing.headers[header::CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn inferred_browse_rejects_ambiguous_and_non_json_selectors_before_reading() {
    let s = server();
    for uri in [
        "/$/ontology/t/hierarchy?inferences=false&inferences=true&at=commit:1",
        "/$/ontology/t/hierarchy?inferences=true&at=commit:1&at=commit:2",
        "/$/ontology/t/hierarchy?inferences=true&at=commit:1&unknown=true",
        "/$/ontology/t/term?inferences=true&at=commit:1&iri=urn:A&format=ttl",
    ] {
        let response = get(&s.app, uri).await;
        assert_eq!(
            response.status,
            StatusCode::BAD_REQUEST,
            "{}",
            response.text()
        );
        assert_eq!(response.headers[header::CACHE_CONTROL], "no-store");
    }
}
