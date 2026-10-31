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
