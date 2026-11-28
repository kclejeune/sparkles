//! The ontology routes (F21) under the caller's grants: a reader limited to one graph
//! sees the axioms of that graph only, also through the imports of another, a reader
//! cannot edit, and a writer limited to one graph cannot write another.

use super::*;
use serde_json::json;

#[tokio::test]
async fn kept_classification_reads_do_not_disclose_shared_state_and_scoped_frame_stays_explicit() {
    let (_dir, app) = server();
    let uri = format!("/$/ontology/ont/classification?{OPEN}&about=http%3A%2F%2Fex%2FA");
    let before = as_user(&app, "oopen", "GET", &uri, &json!(null)).await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.text());
    assert_eq!(before.json()["reason"], "unsupportedScope");
    let run = as_user(
        &app,
        "ofull",
        "POST",
        "/$/ontology/ont/classify",
        &json!({"graph":"http://ex/open"}),
    )
    .await;
    assert_eq!(run.status, StatusCode::OK, "{}", run.text());
    let after = as_user(&app, "oopen", "GET", &uri, &json!(null)).await;
    assert_eq!(after.json(), before.json());
    assert_eq!(after.headers[header::CACHE_CONTROL], "no-store");
    let scoped = as_user(
        &app,
        "oopen",
        "POST",
        "/$/ontology/ont/classify",
        &json!({"graph":"http://ex/open","projection":"frame","about":"http://ex/A","limit":1}),
    )
    .await;
    assert_eq!(scoped.status, StatusCode::OK, "{}", scoped.text());
    let j = scoped.json();
    assert_eq!(j["frame"]["retention"]["kept"], false);
    assert_eq!(
        j["frame"]["parents"]["items"][0]["iri"],
        "http://www.w3.org/2002/07/owl#Thing"
    );
    assert_eq!(j["frame"]["result"]["sourceCounts"]["resolvedGraphs"], 1);
    assert!(!scoped.text().contains("http://ex/Hidden"));
    let denied = call(&app, "GET", &uri, &[], "").await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
    assert_eq!(denied.headers[header::CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn kept_entity_and_axiom_reads_never_disclose_shared_state_to_a_restricted_reader() {
    let (_dir, app) = server();
    let run = as_user(
        &app,
        "ofull",
        "POST",
        "/$/ontology/ont/classify",
        &json!({"graph":"http://ex/open"}),
    )
    .await;
    assert_eq!(run.status, StatusCode::OK, "{}", run.text());
    for uri in [
        format!("/$/ontology/ont/axioms?{OPEN}&classification=kept"),
        format!("/$/ontology/ont/entity?{OPEN}&iri=http%3A%2F%2Fex%2FA&classification=kept"),
    ] {
        let r = as_user(&app, "oopen", "GET", &uri, &json!(null)).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        assert_eq!(r.headers[header::CACHE_CONTROL], "no-store");
        let j = r.json();
        let kept = j.get("classification").unwrap_or(&j);
        assert_eq!(kept["reason"], "unsupportedScope", "{uri}: {j}");
        assert!(kept["result"].is_null());
        assert!(!r.text().contains("http://ex/Hidden"), "{uri}: {j}");
    }
    let full = as_user(
        &app,
        "ofull",
        "GET",
        &format!("/$/ontology/ont/axioms?{OPEN}&classification=kept"),
        &json!(null),
    )
    .await;
    assert_eq!(full.json()["status"], "available", "{}", full.text());
}

#[tokio::test]
async fn authentication_refusals_of_kept_reads_are_never_cached() {
    let (_dir, app) = server();
    // a dataset the reader has no grant on answers as missing before the route runs
    for uri in [
        "/$/ontology/other/axioms?classification=kept",
        "/$/ontology/other/entity?iri=http%3A%2F%2Fex%2FA&classification=kept",
        "/$/ontology/other/classification",
    ] {
        let r = as_user(&app, "oread", "GET", uri, &json!(null)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}: {}", r.text());
        assert_eq!(r.headers[header::CACHE_CONTROL], "no-store", "{uri}");
    }
    // an anonymous kept read is refused before the route too
    let denied = call(
        &app,
        "GET",
        "/$/ontology/ont/axioms?classification=kept",
        &[],
        "",
    )
    .await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
    assert_eq!(denied.headers[header::CACHE_CONTROL], "no-store");
    // the asserted reads keep their own caching
    for uri in [
        "/$/ontology/other/axioms",
        "/$/ontology/other/entity?iri=http%3A%2F%2Fex%2FA",
    ] {
        let r = as_user(&app, "oread", "GET", uri, &json!(null)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}: {}", r.text());
        assert!(
            r.headers
                .get(header::CACHE_CONTROL)
                .is_none_or(|v| v.as_bytes() != b"no-store"),
            "{uri}"
        );
    }
}

#[tokio::test]
async fn branch_selection_refusals_of_kept_reads_are_never_cached() {
    let (_dir, app) = server();
    // branch selection refuses a conflicting or invalid branch before routing
    for branch in ["branch=one&branch=two", "branch=a%20b"] {
        for uri in [
            format!("/$/ontology/ont/axioms?classification=kept&{branch}"),
            format!("/$/ontology/ont/entity?iri=http%3A%2F%2Fex%2FA&classification=kept&{branch}"),
            format!("/$/ontology/ont/classification?{branch}"),
        ] {
            let r = as_user(&app, "oread", "GET", &uri, &json!(null)).await;
            assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uri}: {}", r.text());
            assert_eq!(r.json()["code"], "invalid-branch", "{uri}");
            assert_eq!(r.headers[header::CACHE_CONTROL], "no-store", "{uri}");
        }
        // the asserted reads keep their own caching
        for uri in [
            format!("/$/ontology/ont/axioms?{branch}"),
            format!("/$/ontology/ont/entity?iri=http%3A%2F%2Fex%2FA&{branch}"),
        ] {
            let r = as_user(&app, "oread", "GET", &uri, &json!(null)).await;
            assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uri}: {}", r.text());
            assert_eq!(r.json()["code"], "invalid-branch", "{uri}");
            assert!(
                r.headers
                    .get(header::CACHE_CONTROL)
                    .is_none_or(|v| v.as_bytes() != b"no-store"),
                "{uri}"
            );
        }
    }
}

#[tokio::test]
async fn branch_path_rewrite_refusals_of_kept_reads_are_never_cached() {
    let (_dir, app) = server();
    // /$@main/ontology/… rewrites to /$/ontology/…; a conflicting branch selector
    // must still return no-store on kept routes
    for uri in [
        "/$@main/ontology/ont/axioms?classification=kept&branch=other",
        "/$@main/ontology/ont/entity?iri=http%3A%2F%2Fex%2FA&classification=kept&branch=other",
        "/$@main/ontology/ont/classification?branch=other",
    ] {
        let r = as_user(&app, "oread", "GET", uri, &json!(null)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uri}: {}", r.text());
        assert_eq!(r.json()["code"], "invalid-branch", "{uri}");
        assert_eq!(r.headers[header::CACHE_CONTROL], "no-store", "{uri}");
    }
    // an asserted read through the alias keeps its existing caching
    for uri in [
        "/$@main/ontology/ont/axioms?branch=other",
        "/$@main/ontology/ont/entity?iri=http%3A%2F%2Fex%2FA&branch=other",
    ] {
        let r = as_user(&app, "oread", "GET", uri, &json!(null)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uri}: {}", r.text());
        assert_eq!(r.json()["code"], "invalid-branch", "{uri}");
        assert!(
            r.headers
                .get(header::CACHE_CONTROL)
                .is_none_or(|v| v.as_bytes() != b"no-store"),
            "{uri}"
        );
    }
}

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
    let (dir, st, app) = server_state();
    drop(st);
    (dir, app)
}

fn server_state() -> (tempfile::TempDir, Arc<AppState>, Router) {
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
    (dir, st.clone(), router(st))
}

#[tokio::test]
async fn strict_materialized_browser_never_reveals_hidden_import_inputs() {
    let (_dir, state, app) = server_state();
    let ds = state.get("ont").unwrap();
    ds.dataset
        .reasoning()
        .run(&sparkles::reasoning::ReasonRequest {
            profile: sparkles_reasoner::Profile::Rdfs,
            inputs: sparkles_reasoner::inputs::Inputs {
                data_graphs: vec![sparkles_reasoner::inputs::GraphRef::Named(
                    "http://ex/open".into(),
                )],
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
    let commit = ds.store.head_commit().seq;
    let selection = format!("{OPEN}&at=commit:{commit}&inferences=true");
    let full = as_user(
        &app,
        "ofull",
        "GET",
        &format!("/$/ontology/ont/hierarchy?{selection}"),
        &json!(null),
    )
    .await;
    assert_eq!(full.status, StatusCode::OK, "{}", full.text());
    assert!(full.text().contains("http://ex/secret"));
    let pin = full.json()["inferenceSource"]["sourceId"]
        .as_str()
        .unwrap()
        .to_owned();
    for route in ["hierarchy", "individuals", "term"] {
        let extra = if route == "term" {
            "&iri=http%3A%2F%2Fex%2FvisibleIndividual"
        } else {
            ""
        };
        let uri = format!("/$/ontology/ont/{route}?{selection}&sourceId={pin}{extra}");
        let denied = as_user(&app, "oopen", "GET", &uri, &json!(null)).await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN, "{}", denied.text());
        assert_eq!(denied.headers[header::CACHE_CONTROL], "no-store");
        for hidden in ["http://ex/secret", "Hidden", "sourceId", "sha256:"] {
            assert!(!denied.text().contains(hidden), "{}", denied.text());
        }
    }
    assert_eq!(ds.store.head_commit().seq, commit);
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

/// The import map (spec U12): administrators change it, readers see only the entries whose
/// graph they can read, and a map entry never resolves an import to a hidden graph.
#[tokio::test]
async fn the_import_map_needs_admin_and_shows_readers_their_graphs() {
    let (_dir, app) = server();
    // http://ex/empty holds no triples on this branch, so only administrators see its entry
    let set = json!({"set": {"http://ex/x": "http://ex/secret", "http://ex/y": "http://ex/open", "http://ex/secret": "http://ex/secret", "http://ex/z": "http://ex/empty"}});
    for user in ["ofull", "oopen", "oread"] {
        let r = as_user(&app, user, "PATCH", "/$/ontology/ont/imports", &set).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{user}: {}", r.text());
        let r = as_user(
            &app,
            user,
            "POST",
            "/$/ontology/ont/imports/fetch",
            &json!({"iri": "http://ex/x"}),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{user}: {}", r.text());
        let r = as_user(
            &app,
            user,
            "POST",
            "/$/ontology/ont/imports/catalog",
            &json!({}),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{user}: {}", r.text());
    }
    let r = as_user(
        &app,
        "alice",
        "GET",
        "/$/ontology/ont/imports",
        &json!(null),
    )
    .await;
    let revision = r.json()["revision"].as_str().unwrap().to_string();
    let r = call(
        &app,
        "PATCH",
        "/$/ontology/ont/imports",
        &[
            ("authorization", &b("alice")),
            ("content-type", "application/json"),
            ("if-match", &revision),
        ],
        &set.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["entries"].as_array().unwrap().len(), 4);
    let iris = |r: &R| -> Vec<String> {
        r.json()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["iri"].as_str().unwrap().to_string())
            .collect()
    };
    let r = as_user(
        &app,
        "oread",
        "GET",
        "/$/ontology/ont/imports",
        &json!(null),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["admin"], false);
    assert_eq!(iris(&r).len(), 3, "{}", r.text());
    assert!(!r.text().contains("http://ex/empty"), "{}", r.text());
    let r = as_user(
        &app,
        "oopen",
        "GET",
        "/$/ontology/ont/imports",
        &json!(null),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(iris(&r), ["http://ex/y"]);
    assert!(!r.text().contains("secret"), "{}", r.text());
    // the open graph's import of the secret graph stays unresolved for this caller
    let r = as_user(
        &app,
        "oopen",
        "GET",
        &format!("/$/ontology/ont?{OPEN}"),
        &json!(null),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["graphs"],
        json!(["http://ex/open"]),
        "{}",
        r.text()
    );
    let closure = r.json()["closure"].clone();
    assert_eq!(closure[0]["iri"], "http://ex/secret");
    assert_eq!(closure[0]["graphs"], json!([]), "{closure}");
    let r = as_user(
        &app,
        "ofull",
        "GET",
        &format!("/$/ontology/ont?{OPEN}"),
        &json!(null),
    )
    .await;
    assert_eq!(
        r.json()["closure"][0]["graphs"],
        json!(["http://ex/secret"])
    );
    assert_eq!(r.json()["closure"][0]["via"], "map");
}

/// Pattern-based axiom generation (spec U12, item 623): readers can preview patterns on
/// graphs they can read, while applying requires dataset write access and graph write grants.
#[tokio::test]
async fn ontology_patterns_preview_reads_and_apply_enforces_write_grants() {
    let (_dir, app) = server();
    let payload = json!({
        "graph": "http://ex/open",
        "target": "http://ex/open",
        "table": {
            "data": "ID,Label,Parent\nID,LABEL,SC %\nhttp://ex/Generated,Generated class,http://ex/A\n"
        },
        "spec": {
            "mode": "robot",
            "headerTemplateRow": true
        }
    });

    // Anonymous gets 401 on both preview and apply.
    let r = call(
        &app,
        "POST",
        "/$/ontology/ont/patterns/preview",
        &[("content-type", "application/json")],
        &payload.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
    let r = call(
        &app,
        "POST",
        "/$/ontology/ont/patterns/apply",
        &[("content-type", "application/json")],
        &payload.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());

    // Reader can preview, but apply is 403 Forbidden.
    let prev = as_user(
        &app,
        "oread",
        "POST",
        "/$/ontology/ont/patterns/preview",
        &payload,
    )
    .await;
    assert_eq!(prev.status, StatusCode::OK, "{}", prev.text());
    assert_eq!(prev.json()["valid"], true);
    let deny = as_user(
        &app,
        "oread",
        "POST",
        "/$/ontology/ont/patterns/apply",
        &payload,
    )
    .await;
    assert_eq!(deny.status, StatusCode::FORBIDDEN, "{}", deny.text());

    // Graph-restricted writer cannot apply to a graph outside its write grant.
    let mut secret_payload = payload.clone();
    secret_payload["graph"] = json!("http://ex/secret");
    secret_payload["target"] = json!("http://ex/secret");
    let deny_graph = as_user(
        &app,
        "oopen",
        "POST",
        "/$/ontology/ont/patterns/apply",
        &secret_payload,
    )
    .await;
    assert_eq!(
        deny_graph.status,
        StatusCode::FORBIDDEN,
        "{}",
        deny_graph.text()
    );

    // Graph-restricted writer cannot apply to a graph outside its write grant, even when
    // the pattern itself has row errors (403 must precede 422 pattern-errors).
    let mut secret_bad_payload = payload.clone();
    secret_bad_payload["graph"] = json!("http://ex/open");
    secret_bad_payload["target"] = json!("http://ex/secret");
    secret_bad_payload["table"] =
        json!({ "data": "ID,Parent\nID,SC %\nhttp://ex/X,not valid (\n" });
    let deny_graph_bad = as_user(
        &app,
        "oopen",
        "POST",
        "/$/ontology/ont/patterns/apply",
        &secret_bad_payload,
    )
    .await;
    assert_eq!(
        deny_graph_bad.status,
        StatusCode::FORBIDDEN,
        "target graph grant must refuse before 422 pattern-errors: {}",
        deny_graph_bad.text()
    );

    // Full writer can apply to the open graph in one commit.
    let ok = as_user(
        &app,
        "ofull",
        "POST",
        "/$/ontology/ont/patterns/apply",
        &payload,
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());
    assert_eq!(ok.json()["committed"], true);
}
