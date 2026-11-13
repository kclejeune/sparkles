//! Durable jobs over HTTP: the submission receipt and its `Location`, the claim's lease
//! token, publication with N-Quads, replay by operation id and the fencing of a finished
//! lease.

use crate::http::router;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use serde_json::{Value as J, json};
use sparkles::store::StoreOptions;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    lease: Option<&str>,
    body: Option<J>,
) -> (StatusCode, J, Option<String>) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = lease {
        req = req.header(super::LEASE_HEADER, t);
    }
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let location = res
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(J::Null),
        location,
    )
}

fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn settings() -> J {
    json!({
        "format": 1, "profile": 1, "enabled": true,
        "definitions": {"collect": {"kind": "quads", "graphs": ["urn:g"]}}
    })
}

#[tokio::test]
async fn submit_claim_publish_and_fence_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("d", DbType::Persistent, None).unwrap();
    let app = router(state.clone());

    let (s, j, _) = call(&app, "GET", "/$/jobs/d/settings", None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["settings"]["enabled"], false);
    let (s, j, _) = call(&app, "PUT", "/$/jobs/d/settings", None, Some(settings())).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["protected"], true);
    assert!(
        j["revisions"]["collect"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );

    // a newer request format is refused before anything is read
    let (s, j, _) = call(
        &app,
        "POST",
        "/$/jobs/d",
        None,
        Some(json!({"format": 2, "definition": "collect", "idempotencyKey": "k"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{j}");
    assert_eq!(j["code"], "unsupported-format");

    let submission = json!({
        "definition": "collect", "payload": {"page": 1}, "idempotencyKey": "adv-1",
        "checkpoint": "start"
    });
    let (s, j, location) = call(&app, "POST", "/$/jobs/d", None, Some(submission.clone())).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{j}");
    let run = j["run"].as_str().unwrap().to_string();
    assert_eq!(
        location.as_deref(),
        Some(format!("/$/jobs/d/{run}").as_str())
    );
    assert_eq!(j["replayed"], false);
    let (s, j, _) = call(&app, "POST", "/$/jobs/d", None, Some(submission)).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["replayed"], true);
    assert_eq!(j["run"], run);
    let (s, j, _) = call(
        &app,
        "POST",
        "/$/jobs/d",
        None,
        Some(json!({"definition": "collect", "payload": {"page": 2}, "idempotencyKey": "adv-1"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{j}");
    assert_eq!(j["code"], "idempotency-conflict");

    // the ledger graph leaves the query, Graph Store and listing views (C26 §2.3)
    let ledger = sparkles::jobs::LEDGER_GRAPH;
    let ask = format!("ASK {{ GRAPH <{ledger}> {{ ?s ?p ?o }} }}");
    let (s, j, _) = call(
        &app,
        "GET",
        &format!("/d/query?query={}", enc(&ask)),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["boolean"], false);
    let (s, j, _) = call(
        &app,
        "GET",
        &format!(
            "/d/query?query={}",
            enc("SELECT ?g { GRAPH ?g { ?s ?p ?o } }")
        ),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["results"]["bindings"].as_array().unwrap().len(), 0);
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/d/data?graph={}", enc(ledger)),
        None,
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "a hidden graph answers as a missing one"
    );
    let (s, j, _) = call(&app, "GET", &format!("/$/jobs/d/{run}"), None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["state"], "queued");
    assert_eq!(j["checkpoint"], "start");
    let (s, j, _) = call(&app, "GET", "/$/jobs/d?state=queued", None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["runs"].as_array().unwrap().len(), 1);
    // a static segment is never a run id, and an unknown id is not found
    let (s, _, _) = call(&app, "GET", "/$/jobs/d/not-a-uuid", None, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = call(
        &app,
        "GET",
        &format!("/$/jobs/d/{}", uuid::Uuid::new_v4()),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, j, _) = call(&app, "POST", "/$/jobs/d/claim", None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["run"], run);
    assert_eq!(j["payload"], json!({"page": 1}));
    assert_eq!(j["generation"], 1);
    let token = j["token"].as_str().unwrap().to_string();
    let version = j["version"].as_u64().unwrap();
    let (s, _, _) = call(&app, "POST", "/$/jobs/d/claim", None, None).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "nothing else is eligible");
    // the inspection never shows the token or its verifier
    let (_, j, _) = call(&app, "GET", &format!("/$/jobs/d/{run}"), None, None).await;
    assert_eq!(j["state"], "leased");
    assert!(j["attempt"].get("token").is_none());
    assert!(!j.to_string().contains(&token));

    let (s, j, _) = call(&app, "POST", &format!("/$/jobs/d/{run}/renew"), None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{j}");
    assert_eq!(j["code"], "lease-missing");
    let (s, j, _) = call(
        &app,
        "POST",
        &format!("/$/jobs/d/{run}/renew"),
        Some(&token),
        Some(json!({"extendMs": 10000, "expectedVersion": version})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["version"], version + 1);
    let (s, j, _) = call(
        &app,
        "POST",
        &format!("/$/jobs/d/{run}/renew"),
        Some("wrong"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{j}");
    assert_eq!(j["code"], "lease-lost");

    let op = uuid::Uuid::new_v4().to_string();
    let publication = json!({
        "operation": op,
        "insert": ["<urn:s> <urn:p> \"v\" <urn:g> ."],
        "checkpoint": "page-1-done"
    });
    let (s, j, _) = call(
        &app,
        "POST",
        &format!("/$/jobs/d/{run}/publish"),
        Some(&token),
        Some(json!({
            "operation": op,
            "insert": ["<urn:s> <urn:p> \"v\" <urn:elsewhere> ."]
        })),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "a graph outside the definition: {j}"
    );
    let (s, j, _) = call(
        &app,
        "POST",
        &format!("/$/jobs/d/{run}/publish"),
        Some(&token),
        Some(publication.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["replayed"], false);
    let commit = j["commit"].as_u64().unwrap();
    let ds = state.get("d").unwrap();
    assert!(
        ds.dataset
            .ask("ASK { GRAPH <urn:g> { <urn:s> <urn:p> \"v\" } }")
            .unwrap()
    );
    let (s, j, _) = call(
        &app,
        "POST",
        &format!("/$/jobs/d/{run}/publish"),
        Some(&token),
        Some(publication),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["replayed"], true);
    assert_eq!(j["commit"], commit);
    let (s, j, _) = call(
        &app,
        "POST",
        &format!("/$/jobs/d/{run}/publish"),
        Some(&token),
        Some(json!({"operation": op, "insert": ["<urn:s2> <urn:p> \"v\" <urn:g> ."]})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{j}");
    assert_eq!(j["code"], "publication-conflict");
    let (s, j, _) = call(
        &app,
        "POST",
        &format!("/$/jobs/d/{run}/fail"),
        Some(&token),
        Some(json!({"transient": true, "reason": "late"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{j}");
    assert_eq!(j["code"], "lease-lost");
    let (_, j, _) = call(&app, "GET", &format!("/$/jobs/d/{run}"), None, None).await;
    assert_eq!(j["state"], "succeeded");
    assert_eq!(j["checkpoint"], "page-1-done");
    assert_eq!(j["publication"]["commit"], commit);
    let (s, j, _) = call(&app, "POST", &format!("/$/jobs/d/{run}/cancel"), None, None).await;
    assert_eq!(s, StatusCode::CONFLICT, "a terminal run is immutable: {j}");
    let (s, j, location) = call(
        &app,
        "POST",
        &format!("/$/jobs/d/{run}/retry"),
        None,
        Some(json!({"idempotencyKey": "adv-1-again"})),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "only failed or cancelled runs retry: {j} {location:?}"
    );
}

/// A persistent dataset with jobs enabled and one submitted run, so that its only quads
/// are control records.
async fn seeded() -> (tempfile::TempDir, Arc<AppState>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("d", DbType::Persistent, None).unwrap();
    let app = router(state.clone());
    let (s, j, _) = call(&app, "PUT", "/$/jobs/d/settings", None, Some(settings())).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    let body = json!({"definition": "collect", "idempotencyKey": "k1", "payload": {"page": 1}});
    let (s, j, _) = call(&app, "POST", "/$/jobs/d", None, Some(body)).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{j}");
    (dir, state, app)
}

#[tokio::test]
async fn aggregates_and_listings_leave_the_hidden_ledger_out() {
    let (_dir, state, app) = seeded().await;
    let ledger = sparkles::jobs::LEDGER_GRAPH;
    assert!(!state.get("d").unwrap().dataset.snapshot().is_empty());
    // statistics
    let (s, j, _) = call(&app, "GET", "/$/stats/d", None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert!(!j.to_string().contains(ledger), "{j}");
    assert!(!j.to_string().contains("urn:x-sparkles:jobs:"), "{j}");
    assert_eq!(j["quads"], 0, "{j}");
    assert_eq!(j["graphs"].as_array().map(Vec::len), Some(0), "{j}");
    assert_eq!(j["classes"].as_array().map(Vec::len), Some(0), "{j}");
    // the global listing and the server document, which name no dataset in their path
    for uri in ["/$/datasets", "/$/server"] {
        let (s, j, _) = call(&app, "GET", uri, None, None).await;
        assert_eq!(s, StatusCode::OK, "{j}");
        let d = j["datasets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == "d")
            .unwrap()
            .clone();
        assert_eq!(d["quads"], 0, "{uri}: {d}");
        assert!(d.get("graphs").is_none(), "the grants are not limited: {d}");
    }
    let (s, j, _) = call(&app, "GET", "/$/datasets/d", None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["quads"], 0, "{j}");
    // history counts count every graph, so they are left out
    let (s, j, _) = call(&app, "GET", "/$/commits/d/head", None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert!(j["commit"].get("inserted").is_none(), "{j}");
    let (s, j, _) = call(&app, "GET", "/$/commits/d", None, None).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    let commits = j["commits"].as_array().unwrap();
    assert!(!commits.is_empty());
    assert!(commits.iter().all(|c| c.get("inserted").is_none()), "{j}");
    // readiness sizes count every graph: left out for the dataset that hides one
    state.attach("e", DbType::Mem, None).unwrap();
    let (_, j, _) = call(&app, "GET", "/$/ready", None, None).await;
    let ready = |name: &str| {
        j["datasets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == name)
            .unwrap()
            .clone()
    };
    assert!(ready("e").get("deltaQuads").is_some(), "{j}");
    assert!(ready("d").get("deltaQuads").is_none(), "{j}");
}

#[tokio::test]
async fn validation_never_reads_the_hidden_ledger() {
    let (_dir, _state, app) = seeded().await;
    let ledger = sparkles::jobs::LEDGER_GRAPH;
    // a shape over the run class would report every run, were the ledger read
    let shapes = "@prefix sh: <http://www.w3.org/ns/shacl#> .
        <urn:S> a sh:NodeShape ; sh:targetClass <urn:x-sparkles:jobs:Run> ;
          sh:property [ sh:path <urn:x-sparkles:jobs:state> ; sh:maxCount 0 ] .";
    for uri in ["/d/shacl?graph=union", "/d/shacl"] {
        let req = Request::post(uri)
            .header("content-type", "text/turtle")
            .header("accept", "application/json")
            .body(Body::from(shapes))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&bytes);
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert!(body.contains("\"conforms\":true"), "{uri}: {body}");
        assert!(!body.contains("urn:x-sparkles:jobs:run:"), "{uri}: {body}");
    }
    // the ledger named as the data graph is a graph that does not exist
    let req = Request::post(format!("/d/shacl?graph={}", enc(ledger)))
        .header("content-type", "text/turtle")
        .header("accept", "application/json")
        .body(Body::from(shapes))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn snapshot_creation_and_history_gone_leave_the_commit_counts_out() {
    let (_dir, state, app) = seeded().await;
    let (status, j, _) = call(
        &app,
        "POST",
        "/$/snapshots/d",
        None,
        Some(json!({"name": "s1", "at": "head"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{j}");
    assert!(j["commit"].get("inserted").is_none(), "{j}");
    assert!(j["commit"].get("quads").is_none(), "{j}");

    let ds = state.get("d").unwrap();
    let seq = ds.store.head_commit().seq;
    // without its pin, the state of the snapshot is collected
    ds.store.delete_snapshot("s1").unwrap();
    ds.store.set_retention(Default::default()).unwrap();
    ds.dataset
        .update("INSERT DATA { <urn:s> <urn:p> <urn:o> }")
        .unwrap();
    ds.dataset.compact().unwrap();
    let q = enc("ASK { ?s ?p ?o }");
    let (status, j, _) = call(
        &app,
        "GET",
        &format!("/d/query?at=commit:{seq}&query={q}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{j}");
    assert_eq!(j["metadata"]["seq"], seq, "{j}");
    for field in [
        "inserted",
        "deleted",
        "quads",
        "digest",
        "exact",
        "defaultGraph",
    ] {
        assert!(j["metadata"].get(field).is_none(), "{field}: {j}");
    }
}

#[tokio::test]
async fn metrics_count_only_the_visible_quads() {
    let (_dir, _state, app) = seeded().await;
    let (status, j, _) = call(&app, "GET", "/$/metrics?format=json", None, None).await;
    assert_eq!(status, StatusCode::OK, "{j}");
    let d = j["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "d")
        .unwrap();
    assert_eq!(d["quads"].as_u64().unwrap_or(0), 0, "{d}");
    let res = app
        .clone()
        .oneshot(Request::get("/$/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    for line in text.lines().filter(|l| {
        (l.starts_with("sparkles_dataset_quads") || l.starts_with("sparkles_branch_quads"))
            && l.contains("\"d\"")
    }) {
        assert!(line.ends_with(" 0"), "{line}");
    }
}

#[tokio::test]
async fn a_merge_preview_leaves_the_hidden_changes_out() {
    let (_dir, state, app) = seeded().await;
    let ds = state.get("d").unwrap();
    ds.dataset
        .update("INSERT DATA { <urn:s> <urn:p> <urn:base> }")
        .unwrap();
    ds.store.create_branch("b", &Default::default()).unwrap();
    ds.dataset
        .update("DELETE DATA { <urn:s> <urn:p> <urn:base> }; INSERT DATA { <urn:s> <urn:p> <urn:main> }")
        .unwrap();
    let b = state.branch_dataset(&ds, "b").unwrap();
    b.dataset
        .update("DELETE DATA { <urn:s> <urn:p> <urn:base> }; INSERT DATA { <urn:s> <urn:p> <urn:branch> }")
        .unwrap();
    let (status, j, _) = call(
        &app,
        "POST",
        "/$/jobs/d",
        None,
        Some(json!({"definition": "collect", "idempotencyKey": "k2"})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{j}");
    let (status, j, _) = call(&app, "GET", "/$/merge/d?source=main&target=b", None, None).await;
    assert_eq!(status, StatusCode::OK, "{j}");
    // the business cell conflicts, and the new run's records are not counted
    assert_eq!(j["changes"]["inserted"].as_u64().unwrap_or(0), 0, "{j}");
    assert_eq!(j["conflictCount"], 1, "{j}");
    assert!(!j.to_string().contains("urn:x-sparkles:jobs:"), "{j}");
}

#[tokio::test]
async fn a_local_service_call_keeps_the_target_ledger_hidden() {
    let (_dir, state, app) = seeded().await;
    state.attach("e", DbType::Mem, None).unwrap();
    state
        .listener
        .set(crate::http::self_service::Listener {
            scheme: "http",
            ip: "127.0.0.1".parse().unwrap(),
            port: 17739,
        })
        .unwrap();
    let q = format!(
        "SELECT ?s ?p ?o WHERE {{ SERVICE <http://127.0.0.1:17739/d/query> {{ GRAPH <{}> {{ ?s ?p ?o }} }} }}",
        sparkles::jobs::LEDGER_GRAPH
    );
    let (status, j, _) = call(
        &app,
        "GET",
        &format!("/e/query?query={}", enc(&q)),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert_eq!(j["results"]["bindings"].as_array().unwrap().len(), 0, "{j}");
}

#[tokio::test]
async fn a_resource_header_keeps_the_selected_dataset_ledger_hidden() {
    let (_dir, state, app) = seeded().await;
    let ds = state.get("d").unwrap();
    let n = |s: &str| oxrdf::NamedNode::new(s).unwrap();
    ds.dataset
        .reasoning()
        .rdfs()
        .set(sparkles::reasoning::rdfs::NewSchema::Triples(vec![
            oxrdf::Triple::new(
                n("urn:x-sparkles:jobs:Run"),
                n("http://www.w3.org/2000/01/rdf-schema#subClassOf"),
                n("http://www.w3.org/2002/07/owl#Ontology"),
            ),
            oxrdf::Triple::new(
                n("urn:x-sparkles:jobs:payload"),
                n("http://www.w3.org/2000/01/rdf-schema#subPropertyOf"),
                n("http://purl.org/dc/terms/title"),
            ),
        ]))
        .unwrap();
    let cfg = json!({
        "id": "ledger", "kind": "vocabulary", "title": "Test", "license": "CC-BY-4.0",
        "namespaces": [{"prefix": "test", "iri": "urn:test:"}],
        "graphs": [{"dataset": "d", "graph": sparkles::jobs::LEDGER_GRAPH}]
    });
    let (status, j, _) = call(&app, "POST", "/$/resources", None, Some(cfg)).await;
    assert_eq!(status, StatusCode::CREATED, "{j}");
    let (status, j, _) = call(&app, "GET", "/$/resources/ledger/header", None, None).await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert!(!j.to_string().contains("urn:x-sparkles:jobs:run:"), "{j}");
}

async fn text_of(app: &Router, req: Request) -> (StatusCode, String) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn shacl_guard_validation_never_reads_the_hidden_ledger() {
    let (_dir, _state, app) = seeded().await;
    let shapes = "@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:S> a sh:NodeShape ; sh:targetClass <urn:x-sparkles:jobs:Run> ; sh:property [ sh:path <urn:x-sparkles:jobs:state> ; sh:maxCount 0 ] .";
    let cfg = json!({"mode": "warn", "dataGraph": "union", "shapes": {"inline": shapes}});
    let (status, j, _) = call(&app, "PUT", "/$/validation/d", None, Some(cfg)).await;
    assert_eq!(status, StatusCode::OK, "{j}");
    let req = Request::post("/d/shacl?shapes=guard")
        .header("accept", "application/json")
        .body(Body::empty())
        .unwrap();
    let (status, body) = text_of(&app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("urn:x-sparkles:jobs:run:"), "{body}");
}

#[cfg(feature = "shex")]
#[tokio::test]
async fn shex_guard_validation_never_selects_a_ledger_focus() {
    let (_dir, state, app) = seeded().await;
    let ds = state.get("d").unwrap();
    let admin = sparkles::jobs::Caller::admin(oxrdf::NamedNode::new("urn:admin").unwrap());
    let runs = ds.dataset.jobs().list(&Default::default(), &admin).unwrap();
    let run = format!("urn:x-sparkles:jobs:run:{}", runs[0].id);
    let schema = "<urn:S> { <urn:x-sparkles:jobs:state> [<urn:never>] }";
    let cfg = json!({"language": "shex", "mode": "warn", "dataGraph": "union",
        "schema": {"inline": schema, "format": "shexc"},
        "shapeMap": "{FOCUS a <urn:x-sparkles:jobs:Run>}@<urn:S>"});
    let (status, j, _) = call(&app, "PUT", "/$/validation/d", None, Some(cfg)).await;
    assert_eq!(status, StatusCode::OK, "{j}");
    let req = Request::post(format!(
        "/d/shex?schema=guard&node={}",
        enc(&format!("<{run}>"))
    ))
    .header("content-type", "text/plain")
    .header("accept", "application/json")
    .body(Body::empty())
    .unwrap();
    let (status, body) = text_of(&app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let j: J = serde_json::from_str(&body).unwrap();
    assert_eq!(j["conforms"], true, "{j}");
    assert_eq!(j["counts"]["nonconformant"], 0, "{j}");
    assert_eq!(j["results"].as_array().map(Vec::len), Some(0), "{j}");
    assert!(!body.contains("urn:x-sparkles:jobs:run:"), "{body}");
}

/// A dataset `d` whose ledger graph held `quads` before jobs were enabled. With `guard`,
/// the dataset is persistent and its guard configuration names the ledger. Neither the
/// configuration route nor enablement admits that, so the file is written by hand
/// before the server opens the dataset, as a damaged or edited directory would be.
async fn ledger_written_before_jobs(
    quads: &str,
    guard: Option<J>,
) -> (tempfile::TempDir, Arc<AppState>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let ledger = sparkles::jobs::LEDGER_GRAPH;
    let data =
        format!("INSERT DATA {{ <urn:person> a <urn:Public> . GRAPH <{ledger}> {{ {quads} }} }}");
    let Some(mut cfg) = guard else {
        state.attach("d", DbType::Mem, None).unwrap();
        state.get("d").unwrap().dataset.update(&data).unwrap();
        let app = router(state.clone());
        let (s, j, _) = call(&app, "PUT", "/$/jobs/d/settings", None, Some(settings())).await;
        assert_eq!(s, StatusCode::OK, "{j}");
        return (dir, state, app);
    };
    let db = dir.path().join("prepared-d");
    {
        let ds = sparkles::Dataset::open(&db).unwrap();
        ds.update(&data).unwrap();
        ds.jobs()
            .configure(&serde_json::from_value(settings()).unwrap())
            .unwrap();
    }
    let shex = cfg["language"] == "shex";
    cfg["format"] = json!(if shex { 3 } else { 2 });
    if !shex {
        cfg["language"] = json!("shacl");
    }
    std::fs::write(
        db.join(sparkles::guard::config::CONFIG_FILE),
        serde_json::to_vec(&cfg).unwrap(),
    )
    .unwrap();
    state.attach("d", DbType::Persistent, Some(&db)).unwrap();
    let app = router(state.clone());
    (dir, state, app)
}

#[allow(dead_code)]
const LEDGER_SHAPES: &str = "<urn:SecretShape> a <http://www.w3.org/ns/shacl#NodeShape> ; <http://www.w3.org/ns/shacl#targetClass> <urn:Public> ; <http://www.w3.org/ns/shacl#property> <urn:SecretPropertyShape> . <urn:SecretPropertyShape> <http://www.w3.org/ns/shacl#path> <urn:secret-path> ; <http://www.w3.org/ns/shacl#minCount> 1";

#[allow(dead_code)]
const LEDGER_SCHEMA: &str = "<urn:schema> a <http://www.w3.org/ns/shex#Schema> ; <http://www.w3.org/ns/shex#shapes> <urn:S> . <urn:S> a <http://www.w3.org/ns/shex#Shape> ; <http://www.w3.org/ns/shex#expression> <urn:tc> . <urn:tc> a <http://www.w3.org/ns/shex#TripleConstraint> ; <http://www.w3.org/ns/shex#predicate> <urn:secret-path> ; <http://www.w3.org/ns/shex#min> 1 ; <http://www.w3.org/ns/shex#max> 1";

/// A guard whose shapes or schema are the ledger graph's, in `mode`.
#[allow(dead_code)]
fn ledger_guard(language: &str, mode: &str) -> J {
    let ledger = sparkles::jobs::LEDGER_GRAPH;
    match language {
        "shacl" => json!({"mode": mode, "dataGraph": "default", "shapes": {"graphs": [ledger]}}),
        _ => json!({"language": "shex", "mode": mode, "dataGraph": "default",
            "schema": {"graphs": [ledger]}, "shapeMap": "<urn:person>@<urn:S>"}),
    }
}

#[cfg(all(feature = "shacl", feature = "shex"))]
#[tokio::test]
async fn jobs_are_not_enabled_under_a_guard_that_reads_the_ledger() {
    for (language, quads) in [("shacl", LEDGER_SHAPES), ("shex", LEDGER_SCHEMA)] {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
        );
        state.attach("d", DbType::Mem, None).unwrap();
        let ledger = sparkles::jobs::LEDGER_GRAPH;
        state
            .get("d")
            .unwrap()
            .dataset
            .update(&format!("INSERT DATA {{ GRAPH <{ledger}> {{ {quads} }} }}"))
            .unwrap();
        let app = router(state.clone());
        let (status, j, _) = call(
            &app,
            "PUT",
            "/$/validation/d",
            None,
            Some(ledger_guard(language, "warn")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{language}: {j}");
        let (status, j, _) = call(&app, "PUT", "/$/jobs/d/settings", None, Some(settings())).await;
        assert_eq!(status, StatusCode::CONFLICT, "{language}: {j}");
        assert!(!j.to_string().contains("secret-path"), "{language}: {j}");
    }
}

#[cfg(all(feature = "shacl", feature = "shex"))]
#[tokio::test]
async fn a_guard_configuration_naming_the_ledger_is_answered_as_absent() {
    for (language, quads) in [("shacl", LEDGER_SHAPES), ("shex", LEDGER_SCHEMA)] {
        let (_dir, _state, app) = ledger_written_before_jobs(quads, None).await;
        let (status, j, _) = call(
            &app,
            "PUT",
            "/$/validation/d",
            None,
            Some(ledger_guard(language, "reject")),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{language}: {j}");
        assert!(
            !j.to_string().contains("secret-path") && !j.to_string().contains("urn:S>"),
            "{language}: {j}"
        );
    }
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn a_guard_whose_shapes_are_in_the_ledger_is_answered_as_absent() {
    let (_dir, _state, app) =
        ledger_written_before_jobs(LEDGER_SHAPES, Some(ledger_guard("shacl", "warn"))).await;
    let req = Request::post("/d/shacl?shapes=guard")
        .header("accept", "application/json")
        .body(Body::empty())
        .unwrap();
    let (status, body) = text_of(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(
        !body.contains("secret-path") && !body.contains("SecretPropertyShape"),
        "{body}"
    );
}

#[cfg(feature = "shex")]
#[tokio::test]
async fn a_guard_whose_schema_is_in_the_ledger_is_answered_as_absent() {
    let (_dir, _state, app) =
        ledger_written_before_jobs(LEDGER_SCHEMA, Some(ledger_guard("shex", "warn"))).await;
    let req = Request::post(format!("/d/shex?schema=guard&node={}", enc("<urn:person>")))
        .header("content-type", "text/plain")
        .header("accept", "application/json")
        .body(Body::empty())
        .unwrap();
    let (status, body) = text_of(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(!body.contains("secret-path"), "{body}");
}

#[tokio::test]
async fn a_public_kept_view_stays_readable_on_a_dataset_with_jobs() {
    let (_dir, state, app) = seeded().await;
    let ds = state.get("d").unwrap();
    ds.dataset
        .update("INSERT DATA { <urn:public> <urn:p> <urn:o> }")
        .unwrap();
    ds.dataset
        .views()
        .put(
            "public",
            sparkles::incr::ViewDef::table("SELECT ?s WHERE { ?s <urn:p> <urn:o> }"),
        )
        .unwrap();
    ds.dataset
        .views()
        .wait_built("public", Duration::from_secs(10))
        .unwrap();
    let q = "PREFIX view: <urn:x-sparkles:view#> SELECT ?s WHERE { SERVICE view:read { [] view:name \"public\" ; view:bind ?s } }";
    let (status, j, _) = call(
        &app,
        "GET",
        &format!("/d/query?query={}", enc(q)),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert_eq!(
        j["results"]["bindings"].as_array().map(Vec::len),
        Some(1),
        "{j}"
    );
}

/// `seeded` with a branch `work` that holds one public quad more than `main`.
async fn with_a_branch_to_merge() -> (tempfile::TempDir, Arc<AppState>, Router) {
    let (dir, state, app) = seeded().await;
    let ds = state.get("d").unwrap();
    ds.dataset
        .create_branch("work", &Default::default())
        .unwrap();
    ds.dataset
        .branch("work")
        .unwrap()
        .update("INSERT DATA { <urn:s> <urn:p> <urn:o> }")
        .unwrap();
    (dir, state, app)
}

#[tokio::test]
async fn a_merge_receipt_leaves_the_commit_counts_out() {
    let (_dir, _state, app) = with_a_branch_to_merge().await;
    let body = json!({"source": "work", "target": "main"});
    let (status, j, _) = call(&app, "POST", "/$/merge/d", None, Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert_eq!(j["changes"]["inserted"], 1, "{j}");
    assert!(j["commit"]["seq"].is_u64(), "{j}");
    for field in [
        "inserted",
        "deleted",
        "quads",
        "digest",
        "exact",
        "defaultGraph",
    ] {
        assert!(j["commit"].get(field).is_none(), "{field}: {j}");
    }
}

#[tokio::test]
async fn a_merge_task_detail_leaves_the_commit_counts_out() {
    let (_dir, state, app) = with_a_branch_to_merge().await;
    let req = Request::post("/$/merge/d")
        .header("content-type", "application/json")
        .header("prefer", "respond-async")
        .body(Body::from(json!({"source": "work"}).to_string()))
        .unwrap();
    let (status, body) = text_of(&app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = serde_json::from_str::<J>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let t0 = std::time::Instant::now();
    let task = loop {
        let t = state
            .tasks
            .lock()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .unwrap();
        if !t.active() {
            break t;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "the task did not end"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(task.state, "done", "{:?}", task.message);
    let d = task.detail.unwrap();
    assert!(d["commit"]["seq"].is_u64(), "{d}");
    for field in [
        "inserted",
        "deleted",
        "quads",
        "digest",
        "exact",
        "defaultGraph",
    ] {
        assert!(d["commit"].get(field).is_none(), "{field}: {d}");
    }
    let (status, j, _) = call(&app, "GET", &format!("/$/tasks/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert!(!j.to_string().contains("\"quads\""), "{j}");
}

#[tokio::test]
async fn a_merge_task_finished_before_jobs_were_enabled_leaves_the_commit_counts_out() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("d", DbType::Mem, None).unwrap();
    let ds = state.get("d").unwrap();
    let ledger = sparkles::jobs::LEDGER_GRAPH;
    ds.dataset
        .update(&format!(
            "INSERT DATA {{ GRAPH <{ledger}> {{ <urn:secret> <urn:p> 1 }} }}"
        ))
        .unwrap();
    ds.dataset
        .create_branch("work", &Default::default())
        .unwrap();
    ds.dataset
        .branch("work")
        .unwrap()
        .update("INSERT DATA { <urn:s> <urn:p> 2 }")
        .unwrap();
    let app = router(state.clone());
    let req = Request::post("/$/merge/d")
        .header("content-type", "application/json")
        .header("prefer", "respond-async")
        .body(Body::from(json!({"source": "work"}).to_string()))
        .unwrap();
    let (status, body) = text_of(&app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = serde_json::from_str::<J>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let t0 = std::time::Instant::now();
    while state
        .tasks
        .lock()
        .iter()
        .find(|t| t.id == id)
        .is_some_and(|t| t.active())
    {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "the task did not end"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (s, j, _) = call(&app, "PUT", "/$/jobs/d/settings", None, Some(settings())).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    for uri in [format!("/$/tasks/{id}"), "/$/tasks".to_string()] {
        let (status, j, _) = call(&app, "GET", &uri, None, None).await;
        assert_eq!(status, StatusCode::OK, "{j}");
        assert!(!j.to_string().contains("\"quads\""), "{uri}: {j}");
    }
}
