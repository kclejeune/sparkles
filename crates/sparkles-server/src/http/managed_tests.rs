//! The managed-graph boundary over HTTP (spec C25 §5): SPARQL Update, Graph Store
//! writes, uploads and RDF Patch get `409 profile-managed-graph` for a managed graph, and
//! an unenforceable registry gives `503 managed-unavailable` while reads continue.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::managed::{OwnerEntry, OwnerKind};
use sparkles::store::StoreOptions;
use tower::ServiceExt;

const UPDATE: &str = "application/sparql-update";
const NQ: &str = "application/n-quads";
const TTL: &str = "text/turtle";
const SETTINGS: &[u8] = br#"{"format":1,"profile":1}"#;

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    ct: Option<&str>,
    body: &str,
) -> (StatusCode, J) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(ct) = ct {
        req = req.header("content-type", ct);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(J::Null))
}

#[tokio::test]
async fn ordinary_writes_to_a_managed_graph_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("d", DbType::Persistent, None).unwrap();
    let ds = state.get("d").unwrap();
    ds.store
        .load(&[sparkles::io::Source::from_bytes(
            b"<urn:s> <urn:p> \"m\" <urn:m> .\n".to_vec(),
            sparkles::io::RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    ds.store
        .enable_managed(
            OwnerKind::Evidence,
            OwnerEntry::for_settings(1, vec!["urn:m".into()], SETTINGS),
            SETTINGS,
        )
        .unwrap();
    let app = router(state.clone());
    let head = ds.store.head_commit().seq;
    let refused: [(&str, &str, Option<&str>, &str); 8] = [
        (
            "POST",
            "/d/patch",
            Some("application/rdf-patch"),
            "A <urn:x> <urn:p> \"1\" <urn:m> .\n",
        ),
        (
            "POST",
            "/d/update",
            Some(UPDATE),
            "INSERT DATA { GRAPH <urn:m> { <urn:x> <urn:p> 1 } }",
        ),
        (
            "POST",
            "/d/update",
            Some(UPDATE),
            "DELETE WHERE { GRAPH <urn:m> { ?s ?p ?o } }",
        ),
        (
            "POST",
            "/d/update?dryRun",
            Some(UPDATE),
            "INSERT DATA { GRAPH <urn:m> { <urn:x> <urn:p> 1 } }",
        ),
        (
            "PUT",
            "/d/data?graph=urn:m",
            Some(TTL),
            "<urn:x> <urn:p> 1 .",
        ),
        (
            "POST",
            "/d/data?graph=urn:m",
            Some(TTL),
            "<urn:x> <urn:p> 1 .",
        ),
        ("DELETE", "/d/data?graph=urn:m", None, ""),
        (
            "POST",
            "/d/data",
            Some(NQ),
            "<urn:x> <urn:p> \"1\" <urn:m> .\n",
        ),
    ];
    for (method, uri, ct, body) in refused {
        let (status, j) = call(&app, method, uri, ct, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {uri}: {j}");
        assert_eq!(j["code"], "profile-managed-graph", "{method} {uri}: {j}");
    }
    assert_eq!(ds.store.head_commit().seq, head);
    // other graphs stay writable
    let (status, j) = call(
        &app,
        "PUT",
        "/d/data?graph=urn:free",
        Some(TTL),
        "<urn:x> <urn:p> 1 .",
    )
    .await;
    assert!(status.is_success(), "{status} {j}");
    // a registry that cannot be enforced refuses every write and keeps reads
    let root = ds.store.root().unwrap().to_path_buf();
    drop(app);
    drop(ds);
    drop(state);
    let path = root.join("managed.json");
    let mut v: J = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["format"] = 2.into();
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    if state.get("d").is_none() {
        state.attach("d", DbType::Persistent, None).unwrap();
    }
    let app = router(state.clone());
    let (status, j) = call(
        &app,
        "PUT",
        "/d/data?graph=urn:free",
        Some(TTL),
        "<urn:y> <urn:p> 1 .",
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{j}");
    assert_eq!(j["code"], "managed-unavailable");
    let (status, j) = call(
        &app,
        "POST",
        "/d/query",
        Some("application/sparql-query"),
        "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } }",
    )
    .await;
    assert!(status.is_success(), "{status} {j}");
}

/// `GET`/`PUT /$/evidence/{ds}/settings` and `POST /$/evidence/{ds}/detach` (spec C25
/// §7): configuration protects every role graph, never weakens protection, and unknown
/// profiles refuse as `unsupported-profile`.
#[tokio::test]
async fn evidence_settings_are_configured_and_detached_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("d", DbType::Persistent, None).unwrap();
    let app = router(state.clone());
    let (status, j) = call(&app, "GET", "/$/evidence/d/settings", None, "").await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert_eq!(j["settings"]["enabled"], false);
    assert_eq!(
        (j["digest"].clone(), j["revision"].clone()),
        (J::Null, 0.into())
    );

    let settings = json!({
        "format": 1, "profile": 1, "enabled": true,
        "sourceRecords": ["urn:ev:sources"], "proposals": ["urn:ev:claims"],
        "decisions": ["urn:ev:decisions"], "supports": ["urn:ev:supports"],
        "projections": ["urn:ev:accepted"],
        "policies": { "advisories": {
            "version": 1, "admittedKinds": ["reported"], "conflicts": "refuse",
            "boundary": ["urn:ev:sources", "urn:ev:claims"],
            "decisions": "urn:ev:decisions", "supports": "urn:ev:supports",
            "output": "urn:ev:accepted" } }
    });
    let (status, j) = call(
        &app,
        "PUT",
        "/$/evidence/d/settings",
        Some("application/json"),
        &settings.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert_eq!(j["revision"], 1);
    assert_eq!(j["managed"].as_array().unwrap().len(), 5);
    assert_eq!(j["settings"]["identity"], json!([]));
    let canonical = sparkles::evidence::parse_settings(
        settings.to_string().as_bytes(),
        &sparkles::task::Control::none(),
    )
    .unwrap()
    .canonical(&sparkles::task::Control::none())
    .unwrap();
    assert_eq!(
        j["digest"],
        sparkles::managed::settings_digest(canonical.bytes())
    );
    assert_eq!(j["settings"], *canonical.value());
    let (status, j) = call(
        &app,
        "POST",
        "/d/update",
        Some(UPDATE),
        "INSERT DATA { GRAPH <urn:ev:accepted> { <urn:x> <urn:p> 1 } }",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{j}");
    assert_eq!(j["code"], "profile-managed-graph");

    // dropping the projection role while it is protected
    let mut fewer = settings.clone();
    fewer["projections"] = json!([]);
    fewer["policies"] = json!({});
    let (status, j) = call(
        &app,
        "PUT",
        "/$/evidence/d/settings",
        Some("application/json"),
        &fewer.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{j}");
    // detach, then the narrower settings are accepted
    let (status, j) = call(
        &app,
        "POST",
        "/$/evidence/d/detach",
        Some("application/json"),
        r#"{"graph":"urn:ev:accepted"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert_eq!(j["detached"], json!(["urn:ev:accepted"]));
    let (status, j) = call(
        &app,
        "PUT",
        "/$/evidence/d/settings",
        Some("application/json"),
        &fewer.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{j}");
    let (status, _) = call(
        &app,
        "POST",
        "/d/update",
        Some(UPDATE),
        "INSERT DATA { GRAPH <urn:ev:accepted> { <urn:x> <urn:p> 1 } }",
    )
    .await;
    assert!(status.is_success(), "{status}");

    // invalid requests
    for (uri, body, expected, code) in [
        (
            "/$/evidence/d/settings",
            r#"{"format":1,"profile":2,"enabled":false}"#,
            StatusCode::CONFLICT,
            Some("unsupported-profile"),
        ),
        (
            "/$/evidence/d/settings",
            r#"{"format":1,"profile":1,"enabled":true,"sourceRecords":["urn:a"],"proposals":["urn:p"],"future":1}"#,
            StatusCode::BAD_REQUEST,
            None,
        ),
        (
            "/$/evidence/d/detach",
            r#"{"graph":"urn:not-managed"}"#,
            StatusCode::NOT_FOUND,
            None,
        ),
        (
            "/$/evidence/d/detach",
            r#"{"iri":"urn:ev:sources"}"#,
            StatusCode::BAD_REQUEST,
            None,
        ),
    ] {
        let method = if uri.ends_with("settings") {
            "PUT"
        } else {
            "POST"
        };
        let (status, j) = call(&app, method, uri, Some("application/json"), body).await;
        assert_eq!(status, expected, "{body}: {j}");
        if let Some(code) = code {
            assert_eq!(j["code"], code, "{body}");
        }
    }
    let (status, j) = call(&app, "GET", "/$/evidence/d/settings", None, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(j["settings"], fewer_canonical(&fewer));
}

fn fewer_canonical(v: &J) -> J {
    let s = sparkles::evidence::parse_settings(
        v.to_string().as_bytes(),
        &sparkles::task::Control::none(),
    )
    .unwrap();
    s.canonical(&sparkles::task::Control::none())
        .unwrap()
        .value()
        .clone()
}
