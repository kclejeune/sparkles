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
    assert_eq!(j["control"], true);
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

    // the ledger is no graph, so the query, Graph Store and listing views have none
    // under the IRI the ledger graph once had (C26 §2.3)
    let ledger = "urn:x-sparkles:jobs:ledger";
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

/// The recovery route of a dataset that was not restored reports no recovery, and it
/// refuses to resume a run that no recovery paused (spec C26 §6 and §7.3).
#[tokio::test]
async fn the_recovery_route_resumes_only_paused_restored_work() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("d", DbType::Persistent, None).unwrap();
    let app = router(state.clone());
    let (s, j, _) = call(&app, "PUT", "/$/jobs/d/settings", None, Some(settings())).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    let (s, j, _) = call(&app, "POST", "/$/jobs/d/recover", None, Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j, json!({"recovery": null, "resumed": []}));
    let body = json!({"definition": "collect", "idempotencyKey": "k1", "payload": {}});
    let (s, j, _) = call(&app, "POST", "/$/jobs/d", None, Some(body)).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{j}");
    let run = j["run"].as_str().unwrap().to_string();
    let (s, j, _) = call(
        &app,
        "POST",
        "/$/jobs/d/recover",
        None,
        Some(json!({"resume": [run]})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{j}");
    let (s, _, _) = call(
        &app,
        "POST",
        "/$/jobs/d/recover",
        None,
        Some(json!({"resume": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}
