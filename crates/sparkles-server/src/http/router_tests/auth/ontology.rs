//! The ontology routes (F21) under the caller's grants: a reader limited to one graph
//! sees the axioms of that graph only, also through the imports of another, a reader
//! cannot edit, and a writer limited to one graph cannot write another.

use super::*;
use serde_json::json;

const DATA: &str = r#"@prefix : <http://ex/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://ex/open> {
  <http://ex/open> a owl:Ontology ; owl:imports <http://ex/secret> .
  :A a owl:Class . :B a owl:Class .
  :visibleIndividual a :A .
}
<http://ex/secret> {
  <http://ex/secret> a owl:Ontology .
  :Hidden a owl:Class . :A rdfs:subClassOf :Hidden .
  :secretIndividual a :Hidden .
}
"#;

fn users() -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    format!(
        r#"
[[users]]
name = "ofull"
password = "{ofull}"
datasets = {{ ont = "write" }}

[[users]]
name = "oopen"
password = "{oopen}"
roles = []
[[users.grants]]
dataset = "ont"
level = "write"
graphs = ["http://ex/open"]

[[users]]
name = "oread"
password = "{oread}"
datasets = {{ ont = "read" }}
"#,
        ofull = h("ofull-pw"),
        oopen = h("oopen-pw"),
        oread = h("oread-pw"),
    )
}

fn server() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture {
        extra: users(),
        ..Default::default()
    };
    let config = dir.path().join("auth.toml");
    std::fs::write(&config, config_text(&f)).unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.auth = Some(Arc::new(Auth::open(&config, dir.path()).unwrap().0));
    let st = Arc::new(st);
    let ds = st.attach("ont", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    st.set_phase(crate::obs::Phase::Ready);
    (dir, router(st))
}

const OPEN: &str = "graph=http%3A%2F%2Fex%2Fopen";

async fn as_user(app: &Router, user: &str, method: &str, uri: &str, body: &J) -> R {
    call(
        app,
        method,
        uri,
        &[
            ("authorization", &b(user)),
            ("content-type", "application/json"),
        ],
        &body.to_string(),
    )
    .await
}

#[tokio::test]
async fn the_ontology_holds_only_what_the_caller_reads() {
    let (_dir, app) = server();
    let uri = format!("/$/ontology/ont/axioms?{OPEN}&type=SubClassOf");
    let r = as_user(&app, "ofull", "GET", &uri, &json!(null)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["total"], 1);
    assert_eq!(
        r.json()["graphs"],
        json!(["http://ex/open", "http://ex/secret"])
    );
    // the import of a graph the caller cannot read is not followed
    let r = as_user(&app, "oopen", "GET", &uri, &json!(null)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["total"], 0, "{}", r.text());
    assert!(!r.text().contains("Hidden"), "{}", r.text());
    let r = as_user(
        &app,
        "oopen",
        "GET",
        &format!("/$/ontology/ont?{OPEN}"),
        &json!(null),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    // the open graph names its import, which does not resolve for this caller
    assert_eq!(
        r.json()["graphs"],
        json!(["http://ex/open"]),
        "{}",
        r.text()
    );
    assert!(!r.text().contains("Hidden"), "{}", r.text());
    let r = as_user(
        &app,
        "oopen",
        "POST",
        "/$/ontology/ont/check",
        &json!({"graph": "http://ex/open"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.text().contains("Hidden"), "{}", r.text());
    // the entity routes read through the same view
    let a = format!("/$/ontology/ont/entity?{OPEN}&iri=http%3A%2F%2Fex%2FA");
    let r = as_user(&app, "ofull", "GET", &a, &json!(null)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("Hidden"), "{}", r.text());
    let r = as_user(&app, "oopen", "GET", &a, &json!(null)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["class"]["superClasses"], json!([]), "{}", r.text());
    let r = as_user(
        &app,
        "oopen",
        "GET",
        &format!("/$/ontology/ont/entity?{OPEN}&iri=http%3A%2F%2Fex%2FHidden"),
        &json!(null),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = as_user(
        &app,
        "oopen",
        "GET",
        &format!("/$/ontology/ont/entities?{OPEN}"),
        &json!(null),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains("Hidden"), "{}", r.text());
}

#[tokio::test]
async fn edits_need_write_on_the_graph() {
    let (_dir, app) = server();
    let add = json!({"graph": "http://ex/open", "add": [{"type": "SubClassOf", "subClass": "http://ex/B", "superClass": "http://ex/A"}]});
    let r = as_user(&app, "oread", "POST", "/$/ontology/ont/axioms", &add).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = as_user(&app, "oopen", "POST", "/$/ontology/ont/axioms", &add).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // an axiom the readable graphs do not hold, sent to a graph the caller cannot write
    let other = json!({"graph": "http://ex/open", "target": "http://ex/secret", "add": [{"type": "SubClassOf", "subClass": "http://ex/A", "superClass": "http://ex/B"}]});
    let r = as_user(&app, "oopen", "POST", "/$/ontology/ont/axioms", &other).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let uri = "/$/ontology/ont/axioms?graph=http%3A%2F%2Fex%2Fsecret&type=SubClassOf&imports=false";
    let r = as_user(&app, "ofull", "GET", uri, &json!(null)).await;
    assert_eq!(r.json()["total"], 1, "{}", r.text());
}

#[tokio::test]
async fn removing_an_entity_needs_write_on_its_graphs() {
    let (_dir, app) = server();
    let b = format!("/$/ontology/ont/entity?{OPEN}&imports=false&iri=http%3A%2F%2Fex%2FB");
    let r = as_user(&app, "oread", "DELETE", &b, &json!(null)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = as_user(
        &app,
        "oread",
        "DELETE",
        &format!("{b}&dryRun=true"),
        &json!(null),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // a writer limited to the open graph removes the entity there
    let r = as_user(&app, "oopen", "DELETE", &b, &json!(null)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["removed"], 1, "{}", r.text());
    // but not where the removal would delete a triple of a graph it cannot write
    let a = "/$/ontology/ont/entity?graph=http%3A%2F%2Fex%2Fsecret&imports=false&iri=http%3A%2F%2Fex%2FA";
    let r = as_user(&app, "oopen", "DELETE", a, &json!(null)).await;
    assert!(
        r.status == StatusCode::FORBIDDEN || r.json()["removed"] == 0,
        "{}: {}",
        r.status,
        r.text()
    );
    let r = as_user(
        &app,
        "ofull",
        "GET",
        "/$/ontology/ont/axioms?graph=http%3A%2F%2Fex%2Fsecret&imports=false&type=SubClassOf",
        &json!(null),
    )
    .await;
    assert_eq!(r.json()["total"], 1, "{}", r.text());
}

#[tokio::test]
async fn classify_reads_the_callers_view_and_writes_need_write() {
    let (_dir, app) = server();
    let about = json!({"graph": "http://ex/open", "about": "http://ex/A"});
    let r = as_user(&app, "ofull", "POST", "/$/ontology/ont/classify", &about).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("Hidden"), "{}", r.text());
    // a reader limited to one graph classifies what it reads, without the import it cannot read
    let r = as_user(&app, "oopen", "POST", "/$/ontology/ont/classify", &about).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains("Hidden"), "{}", r.text());
    assert_eq!(r.json()["reused"], false, "{}", r.text());
    // a reader classifies but cannot write the taxonomy
    let r = as_user(
        &app,
        "oread",
        "POST",
        "/$/ontology/ont/classify",
        &json!({}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let write = json!({"graph": "http://ex/open", "target": "http://ex/tax"});
    let r = as_user(&app, "oread", "POST", "/$/ontology/ont/classify", &write).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // a writer limited to one graph cannot write another
    let r = as_user(&app, "oopen", "POST", "/$/ontology/ont/classify", &write).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = as_user(&app, "ofull", "POST", "/$/ontology/ont/classify", &write).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["written"]["committed"], true, "{}", r.text());
}

#[tokio::test]
async fn dl_query_and_parse_read_the_callers_view() {
    let (_dir, app) = server();
    let q = json!({"graph": "http://ex/open", "expression": "A", "answers": ["superclasses"]});
    let r = as_user(&app, "ofull", "POST", "/$/ontology/ont/dl-query", &q).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("Hidden"), "{}", r.text());
    // a reader limited to one graph queries what it reads, without the import it cannot read
    let r = as_user(&app, "oopen", "POST", "/$/ontology/ont/dl-query", &q).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains("Hidden"), "{}", r.text());
    // and the names it cannot read do not resolve
    let p = json!({"graph": "http://ex/open", "text": "Hidden"});
    let r = as_user(&app, "oopen", "POST", "/$/ontology/ont/parse", &p).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(r.json()["reason"], "unknown", "{}", r.text());
    let r = as_user(&app, "oread", "POST", "/$/ontology/ont/parse", &p).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

#[tokio::test]
async fn the_entity_search_holds_only_what_the_caller_reads() {
    let (_dir, app) = server();
    let uri = format!("/$/ontology/ont/search?{OPEN}&q=hidd");
    let r = as_user(&app, "ofull", "GET", &uri, &json!(null)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["results"][0]["iri"],
        "http://ex/Hidden",
        "{}",
        r.text()
    );
    // the import the caller cannot read is not searched
    let r = as_user(&app, "oopen", "GET", &uri, &json!(null)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["results"], json!([]), "{}", r.text());
    assert!(!r.text().contains("Hidden"), "{}", r.text());
    let r = as_user(
        &app,
        "oopen",
        "GET",
        &format!("/$/ontology/ont/search?{OPEN}&q=A"),
        &json!(null),
    )
    .await;
    assert_eq!(r.json()["results"][0]["iri"], "http://ex/A", "{}", r.text());
    // a reader may search, and a caller without credentials may not
    let r = as_user(&app, "oread", "GET", &uri, &json!(null)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = call(&app, "GET", &uri, &[], "").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
}

#[tokio::test]
async fn the_browser_routes_hold_only_what_the_caller_reads() {
    let (_dir, app) = server();
    let a = "iri=http%3A%2F%2Fex%2FA";
    let tree = format!("/$/ontology/ont/hierarchy?{OPEN}&{a}&ancestors=true");
    let r = as_user(&app, "ofull", "GET", &tree, &json!(null)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["ancestors"]["nodes"][0]["iri"],
        "http://ex/Hidden",
        "{}",
        r.text()
    );
    // the imported graph the caller cannot read gives no parent
    let r = as_user(&app, "oopen", "GET", &tree, &json!(null)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["ancestors"]["nodes"], json!([]), "{}", r.text());
    assert!(!r.text().contains("Hidden"), "{}", r.text());
    let term = format!("/$/ontology/ont/term?{OPEN}&{a}");
    let r = as_user(&app, "ofull", "GET", &term, &json!(null)).await;
    assert!(r.text().contains("Hidden"), "{}", r.text());
    let r = as_user(&app, "oopen", "GET", &term, &json!(null)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains("Hidden"), "{}", r.text());
    let r = as_user(
        &app,
        "oopen",
        "GET",
        &format!("{term}&format=ttl"),
        &json!(null),
    )
    .await;
    assert!(!r.text().contains("Hidden"), "{}", r.text());
    // an entity of the unreadable graph is not found
    let r = as_user(
        &app,
        "oopen",
        "GET",
        &format!("/$/ontology/ont/term?{OPEN}&iri=http%3A%2F%2Fex%2FHidden"),
        &json!(null),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = call(&app, "GET", &tree, &[], "").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
}

#[tokio::test]
async fn ontology_metrics_respect_graph_grants_and_remain_read_only() {
    let (_dir, app) = server();
    let uri = format!("/$/ontology/ont?{OPEN}&metrics=true");
    let full = as_user(&app, "oread", "GET", &uri, &json!(null)).await;
    assert_eq!(full.status, StatusCode::OK, "{}", full.text());
    assert_eq!(full.json()["metrics"]["entities"]["Class"], 3);
    let limited = as_user(&app, "oopen", "GET", &uri, &json!(null)).await;
    assert_eq!(limited.status, StatusCode::OK, "{}", limited.text());
    assert_eq!(limited.json()["metrics"]["entities"]["Class"], 2);
    assert_eq!(
        limited.json()["metrics"]["graphs"],
        json!(["http://ex/open"])
    );
    assert!(!limited.text().contains("Hidden"));
    assert_eq!(limited.json()["metrics"]["scopeComplete"], false);
    assert!(limited.json()["metrics"]["expressivity"]["notation"].is_null());
}

#[tokio::test]
async fn individual_pages_preserve_graph_grants_and_read_only_access() {
    let (_dir, app) = server();
    let uri = format!("/$/ontology/ont/individuals?{OPEN}");
    let full = as_user(&app, "oread", "GET", &uri, &json!(null)).await;
    assert_eq!(full.status, StatusCode::OK, "{}", full.text());
    assert_eq!(full.json()["groups"].as_array().unwrap().len(), 2);
    let limited = as_user(&app, "oopen", "GET", &uri, &json!(null)).await;
    assert_eq!(limited.status, StatusCode::OK, "{}", limited.text());
    assert_eq!(limited.json()["groups"].as_array().unwrap().len(), 1);
    assert!(!limited.text().contains("Hidden"));
    let hidden = format!("{uri}&iri=http%3A%2F%2Fex%2FsecretIndividual");
    let r = as_user(&app, "oopen", "GET", &hidden, &json!(null)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let public = format!("{uri}&type=http%3A%2F%2Fex%2FA");
    let r = as_user(&app, "oread", "GET", &public, &json!(null)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["individuals"][0]["iri"],
        "http://ex/visibleIndividual"
    );
    let denied = call(&app, "GET", &uri, &[], "").await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
}
