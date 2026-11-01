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
    v["format"] = 3.into();
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

/// `GET`/`POST /$/evidence/{ds}/holds`, `DELETE /$/evidence/{ds}/holds/{id}` and
/// `releaseHolds` on a branch deletion (spec C25 §3 and §7).
#[tokio::test]
async fn evidence_holds_are_acquired_listed_and_released_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("d", DbType::Persistent, None).unwrap();
    let app = router(state.clone());
    for i in 0..3 {
        let (status, _) = call(
            &app,
            "POST",
            "/d/update",
            Some(UPDATE),
            &format!("INSERT DATA {{ <urn:x> <urn:p> {i} }}"),
        )
        .await;
        assert!(status.is_success());
    }
    let ds = state.get("d").unwrap();
    let dev = ds
        .store
        .create_branch("dev", &sparkles::branch::BranchOptions::default())
        .unwrap();
    let id = "00000000-0000-0000-0000-000000000001";
    let body = json!({ "id": id, "commits": [{ "commit": 1 }, { "branch": "main", "commit": 2 }] });
    let (status, j) = call(
        &app,
        "POST",
        "/$/evidence/d/holds",
        Some("application/json"),
        &body.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{j}");
    assert_eq!(j["owner"], "evidence");
    assert_eq!(j["commits"][0]["branch"], "main");
    assert_eq!(j["commits"][1]["commit"], 2);
    let (status, _) = call(
        &app,
        "POST",
        "/$/evidence/d/holds",
        Some("application/json"),
        &body.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let on_dev = json!({ "commits": [{ "branchId": dev.id, "commit": 3 }] });
    let (status, j) = call(
        &app,
        "POST",
        "/$/evidence/d/holds",
        Some("application/json"),
        &on_dev.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{j}");
    assert_eq!(j["commits"][0]["branch"], "dev");
    let dev_hold = j["id"].as_str().unwrap().to_string();
    let (status, j) = call(&app, "GET", "/$/evidence/d/holds", None, "").await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert_eq!(j["holds"].as_array().unwrap().len(), 2);
    assert_eq!(j["heldCommits"], 3);
    assert_eq!(j["maxCommits"], 65536);
    let (status, j) = call(
        &app,
        "GET",
        "/$/evidence/d/holds?onBranch=dev&limit=1",
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert_eq!(j["holds"][0]["id"], dev_hold.as_str());
    assert_eq!(j["next"], J::Null);
    let (status, j) = call(&app, "GET", "/$/evidence/d/holds?limit=1", None, "").await;
    assert_eq!(status, StatusCode::OK, "{j}");
    let after = j["next"].as_str().unwrap().to_string();
    let (_, j) = call(
        &app,
        "GET",
        &format!("/$/evidence/d/holds?limit=1&after={after}"),
        None,
        "",
    )
    .await;
    assert_eq!(j["holds"].as_array().unwrap().len(), 1);
    assert_eq!(j["next"], J::Null);

    // the held branch is deleted only with its holds released
    let (status, j) = call(&app, "DELETE", "/$/branches/d/dev", None, "").await;
    assert_eq!(status, StatusCode::CONFLICT, "{j}");
    assert_eq!(j["code"], "branch-held");
    let (status, j) = call(
        &app,
        "DELETE",
        "/$/branches/d/dev?releaseHolds=true",
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{j}");
    let (_, j) = call(&app, "GET", "/$/evidence/d/holds", None, "").await;
    assert_eq!(j["holds"].as_array().unwrap().len(), 1);

    // refusals
    for (body, expected, code) in [
        (
            json!({ "id": id, "commits": [{ "commit": 1 }] }),
            StatusCode::CONFLICT,
            Some("hold-conflict"),
        ),
        (
            json!({ "commits": [{ "commit": 99 }] }),
            StatusCode::GONE,
            Some("history-gone"),
        ),
        (
            json!({ "commits": [{ "branch": "nope", "commit": 1 }] }),
            StatusCode::NOT_FOUND,
            None,
        ),
        (
            json!({ "commits": [{ "branch": "main", "branchId": dev.id, "commit": 1 }] }),
            StatusCode::BAD_REQUEST,
            None,
        ),
        (json!({ "commits": [] }), StatusCode::BAD_REQUEST, None),
        (
            json!({ "commits": [{ "commit": 1 }], "note": "x" }),
            StatusCode::BAD_REQUEST,
            None,
        ),
    ] {
        let (status, j) = call(
            &app,
            "POST",
            "/$/evidence/d/holds",
            Some("application/json"),
            &body.to_string(),
        )
        .await;
        assert_eq!(status, expected, "{body}: {j}");
        if let Some(code) = code {
            assert_eq!(j["code"], code, "{body}");
        }
    }
    let (status, _) = call(&app, "GET", "/$/evidence/d/holds?limit=0", None, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/$/evidence/d/holds/{id}"),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/$/evidence/d/holds/{id}"),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "DELETE", "/$/evidence/d/holds/not-a-uuid", None, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// `POST /{ds}/claims/resolve` verifies stored facts at their exact commits and
/// authenticated records, answers the first reference that does not verify with its
/// code, and refuses malformed references field by field.
#[tokio::test]
async fn evidence_references_are_resolved_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("d", DbType::Persistent, None).unwrap();
    let app = router(state.clone());
    let ds = state.get("d").unwrap();
    let id = ds.store.dataset_id();
    let (status, _) = call(
        &app,
        "PUT",
        "/$/evidence/d/settings",
        Some("application/json"),
        r#"{"format":1,"profile":1,"enabled":true,"sourceRecords":["urn:ev:sources"],"proposals":["urn:ev:claims"]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "POST",
        "/d/update",
        Some(UPDATE),
        "INSERT DATA { GRAPH <urn:g> { <urn:a> <urn:p> \"v\"@en } }",
    )
    .await;
    assert!(status.is_success());
    let fact_commit = ds.store.head_commit().seq;
    let (status, _) = call(
        &app,
        "POST",
        "/d/update",
        Some(UPDATE),
        "DELETE DATA { GRAPH <urn:g> { <urn:a> <urn:p> \"v\"@en } }",
    )
    .await;
    assert!(status.is_success());
    // a typed record with its receipt
    let mut t = ds.store.write();
    t.set_managed(
        ds.store
            .managed_writer(OwnerKind::Evidence, uuid::Uuid::new_v4())
            .unwrap(),
    )
    .unwrap();
    let (seq, _) = t.managed_stamp().unwrap();
    // the receipt carries the operation marker of the graph's current protection
    let nonce = ds.store.managed_graphs().unwrap().unwrap().owners[&OwnerKind::Evidence]
        .protections["urn:ev:sources"];
    let trig = format!(
        "PREFIX ev: <urn:x-sparkles:evidence:>
         GRAPH <urn:ev:sources> {{
           <urn:rev> a ev:SourceRevision ; ev:availabilityReceipt <urn:receipt> ;
             ev:graphProtection \"{nonce}\" .
           <urn:receipt> a ev:Operation ; ev:graphProtection \"{nonce}\" ;
             ev:datasetId \"{id}\" ; ev:branchId \"{id}\" ; ev:commit {seq} .
         }}"
    );
    let mut labels = std::collections::HashMap::new();
    for q in oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::TriG).for_slice(trig.as_bytes()) {
        let q = t.encode_quad(&q.unwrap(), &mut labels).unwrap();
        t.insert(q).unwrap();
    }
    t.commit().unwrap();
    let fact = |commit: u64| {
        json!({"key": "f", "storedFact": {
            "datasetId": id, "branchId": id, "commit": commit,
            "graph": "urn:g", "triple": "<urn:a> <urn:p> \"v\"@en",
        }})
    };
    let record = |kinds: J| json!({"key": "rec", "record": {"iri": "urn:rev", "kinds": kinds}});
    let resolve = |refs: J| {
        let app = app.clone();
        async move {
            call(
                &app,
                "POST",
                "/d/claims/resolve",
                Some("application/json"),
                &json!({ "references": refs }).to_string(),
            )
            .await
        }
    };
    let (status, body) = resolve(json!([
        fact(fact_commit),
        record(json!(["SourceRevision"]))
    ]))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["resolved"][0]["storedFact"]["servedBy"], json!(id));
    assert_eq!(
        body["resolved"][0]["storedFact"]["commit"],
        json!(fact_commit)
    );
    assert_eq!(body["resolved"][1]["record"]["kind"], "SourceRevision");
    assert_eq!(
        body["resolved"][1]["record"]["receipt"]["commit"],
        json!(seq)
    );
    // the fact is gone at a later commit, which is never replaced by another
    let (status, body) = resolve(json!([fact(fact_commit + 1)])).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        (body["code"].as_str(), body["reference"].as_str()),
        (Some("evidence-unavailable"), Some("f"))
    );
    // the first failing reference is answered; a kind conflict is a 409
    let (status, body) = resolve(json!([fact(fact_commit), record(json!(["Claim"]))])).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        (body["code"].as_str(), body["reference"].as_str()),
        (Some("immutable-record-conflict"), Some("rec"))
    );
    // a blank node without a reifier
    let (status, body) = resolve(json!([{"key": "b", "storedFact": {
        "datasetId": id, "branchId": id, "commit": fact_commit,
        "triple": "_:x <urn:p> \"v\"",
    }}]))
    .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("unanchored-evidence")
        )
    );
    // malformed references are field-specific 400s
    for (refs, field) in [
        (json!([{"key": "x"}]), "references[0]"),
        (
            json!([{"key": "", "record": {"iri": "urn:rev"}}]),
            "references[0].key",
        ),
        (
            json!([{"key": "x", "record": {"iri": "urn:rev", "kinds": ["Nope"]}}]),
            "references[0].record.kinds[0]",
        ),
        (
            json!([{"key": "x", "record": {"iri": "not an iri"}}]),
            "references[0].record.iri",
        ),
        (
            json!([{"key": "x", "storedFact": {"datasetId": id, "branchId": id, "commit": 1, "triple": "<urn:a> <urn:p>"}}]),
            "references[0].storedFact.triple",
        ),
        (
            json!([{"key": "x", "record": {"iri": "urn:a"}}, {"key": "x", "record": {"iri": "urn:b"}}]),
            "references[1].key",
        ),
    ] {
        let (status, body) = resolve(refs).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap().starts_with(field), "{body}");
    }
    // records of a branch resolve at the branch's state, where main's record is inherited
    ds.store.create_branch("dev", &Default::default()).unwrap();
    let (status, body) = call(
        &app,
        "POST",
        "/d/claims/resolve?branch=dev",
        Some("application/json"),
        &json!({ "references": [record(json!([]))] }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// `POST /{ds}/claims` publishes one batch as one commit of the evidence owner with
/// server-owned receipts, replays a repeated key, and answers each refusal with its
/// status and code, writing nothing.
#[tokio::test]
async fn claims_are_proposed_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("d", DbType::Persistent, None).unwrap();
    let app = router(state.clone());
    let ds = state.get("d").unwrap();
    let id = ds.store.dataset_id();
    // without the profile
    let small = |occurrence: &str, object: &str| {
        json!({
            "revisions": [{"key": "rev", "source": "urn:bulletin", "sourceKey": "bulletin",
                           "content": {"digest": format!("sha256:{}", "0".repeat(64))}}],
            "claims": [{"key": "claim", "occurrenceKey": occurrence, "revision": {"key": "rev"},
                        "proposition": format!("<urn:release> <urn:affectedBy> <{object}>"),
                        "kind": "reported", "polarity": "positive", "author": "urn:publisher"}],
        })
    };
    let post = |body: J| {
        let app = app.clone();
        async move {
            call(
                &app,
                "POST",
                "/d/claims",
                Some("application/json"),
                &body.to_string(),
            )
            .await
        }
    };
    let (status, j) = post(small("c1", "urn:cve")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{j}");
    assert_eq!(j["code"], "unsupported-profile");
    let (status, _) = call(
        &app,
        "PUT",
        "/$/evidence/d/settings",
        Some("application/json"),
        r#"{"format":1,"profile":1,"enabled":true,"sourceRecords":["urn:ev:sources"],"proposals":["urn:ev:claims"]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "POST",
        "/d/update",
        Some(UPDATE),
        "INSERT DATA { GRAPH <urn:g> { <urn:release> <urn:affectedBy> <urn:cve> } }",
    )
    .await;
    assert!(status.is_success());
    let fact_commit = ds.store.head_commit().seq;
    let text = "Release 1.2 is affected by CVE-1.";
    let selected = sparkles::evidence::Sha256Digest::of_bytes(
        &text.as_bytes()[0..11],
        &sparkles::task::Control::none(),
    )
    .unwrap()
    .to_string();
    let full = json!({
        "idempotencyKey": "k1",
        "message": "import bulletin",
        "revisions": [{"key": "rev", "source": "urn:bulletin", "sourceKey": "bulletin",
                       "content": {"text": text}, "publishedAt": "2026-01-01T10:00:00Z",
                       "locators": ["https://example.org/bulletin"]}],
        "renditions": [{"key": "text", "revision": {"key": "rev"}, "occurrenceKey": "text",
                        "conversion": {"method": "urn:method:identity", "version": "1"},
                        "mediaType": "text/plain", "language": "en", "content": {"text": text}}],
        "passages": [{"key": "span", "revision": {"key": "rev"}, "rendition": {"key": "text"},
                      "occurrenceKey": "span", "start": 0, "end": 11, "textDigest": selected}],
        "references": [
            {"key": "fact", "occurrenceKey": "fact", "storedFact": {
                "datasetId": id, "branchId": id, "commit": fact_commit,
                "graph": "urn:g", "triple": "<urn:release> <urn:affectedBy> <urn:cve>"}},
            {"key": "pref", "occurrenceKey": "pref", "passage": {
                "revision": {"key": "rev"}, "rendition": {"key": "text"}, "passage": {"key": "span"}}},
        ],
        "claims": [{"key": "claim", "occurrenceKey": "c1", "revision": {"key": "rev"},
                    "proposition": "<urn:release> <urn:affectedBy> <urn:cve>",
                    "kind": "reported", "polarity": "positive", "author": "urn:publisher",
                    "scope": {"urn:scope:release": ["<urn:release>"]},
                    "confidence": {"source": {"scale": "urn:x-sparkles:evidence:ZeroToOne",
                                              "value": "0.8", "evaluator": "urn:publisher"}}}],
        "links": [{"key": "link", "occurrenceKey": "l1", "target": {"key": "claim"},
                   "evidence": {"key": "pref"}, "relation": "supports", "author": "urn:importer"}],
        "assessments": [{"key": "a", "occurrenceKey": "a1", "target": {"key": "claim"},
                         "conclusion": "<urn:conclusion:plausible>",
                         "method": {"method": "urn:method:review", "version": "1"},
                         "configurationDigest": selected, "evaluatedAt": "2026-01-02T00:00:00Z",
                         "evaluator": "urn:analyst"}],
    });
    // a dry run writes nothing
    let mut dry = full.clone();
    dry["dryRun"] = true.into();
    let head = ds.store.head_commit().seq;
    let (status, j) = post(dry).await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert_eq!(j["committed"], false);
    assert_eq!(j["dryRun"]["wouldCommit"], true, "{j}");
    assert_eq!(ds.store.head_commit().seq, head);
    let (status, first) = post(full.clone()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["committed"], true);
    assert_eq!(first["commit"], head + 1);
    assert_eq!(first["records"].as_array().unwrap().len(), 8);
    assert!(first["hold"].is_string(), "{first}");
    assert!(first["firstAvailableAt"].is_string());
    // the claim resolves as an authenticated record
    let claim = first["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "claim")
        .unwrap()["iri"]
        .clone();
    let (status, j) = call(
        &app,
        "POST",
        "/d/claims/resolve",
        Some("application/json"),
        &json!({"references": [{"key": "c", "record": {"iri": claim, "kinds": ["Claim"]}}]})
            .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert_eq!(
        j["resolved"][0]["record"]["receipt"]["iri"],
        first["operation"]
    );
    // a repeated key returns the first receipt and writes nothing
    let head = ds.store.head_commit().seq;
    let (status, again) = post(full.clone()).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["replayed"], true);
    assert_eq!(again["commit"], first["commit"]);
    assert_eq!(again["records"], first["records"]);
    assert_eq!(ds.store.head_commit().seq, head);
    // refusals write nothing
    let mut changed = full.clone();
    changed["claims"][0]["polarity"] = "negative".into();
    let mut stale = small("c2", "urn:cve");
    stale["ifHead"] = (head - 1).into();
    let mut unknown_kind = small("c3", "urn:cve");
    unknown_kind["claims"][0]["kind"] = "rumor".into();
    let mut missing = small("c4", "urn:cve");
    missing["revisions"] = json!([]);
    missing["claims"][0]["revision"] =
        json!({"iri": "urn:uuid:00000000-0000-4000-8000-000000000000"});
    let mut both = small("c5", "urn:cve");
    both["claims"][0]["revision"] = json!({"key": "rev", "iri": "urn:x"});
    let mut unknown_field = small("c6", "urn:cve");
    unknown_field["firstAvailableAt"] = "2020-01-01T00:00:00Z".into();
    for (body, want, code) in [
        (changed, StatusCode::CONFLICT, "idempotency-conflict"),
        (stale, StatusCode::CONFLICT, "head-changed"),
        (
            missing,
            StatusCode::UNPROCESSABLE_ENTITY,
            "evidence-unavailable",
        ),
        (unknown_kind, StatusCode::BAD_REQUEST, "invalid-reference"),
        (both, StatusCode::BAD_REQUEST, "invalid-reference"),
        (unknown_field, StatusCode::BAD_REQUEST, "invalid-proposal"),
        (json!({}), StatusCode::BAD_REQUEST, ""),
    ] {
        let (status, j) = post(body.clone()).await;
        assert_eq!(status, want, "{body}: {j}");
        if !code.is_empty() {
            assert_eq!(j["code"], code, "{body}: {j}");
        }
        assert_eq!(ds.store.head_commit().seq, head, "{body}");
    }
    // the same identity with other content
    let (status, j) = post(small("c1", "urn:cve")).await;
    assert_eq!(status, StatusCode::OK, "{j}");
    let head = ds.store.head_commit().seq;
    let (status, j) = post(small("c1", "urn:other")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{j}");
    assert_eq!(j["code"], "immutable-record-conflict");
    assert_eq!(ds.store.head_commit().seq, head);
    // a direct write of a typed record's graph stays refused
    let (status, j) = call(
        &app,
        "POST",
        "/d/update",
        Some(UPDATE),
        "INSERT DATA { GRAPH <urn:ev:claims> { <urn:x> <urn:y> <urn:z> } }",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{j}");
    // clients discover the operation
    let (_, server) = call(&app, "GET", "/$/server", None, "").await;
    assert_eq!(server["capabilities"]["claims"]["profiles"], json!([1]));
    assert_eq!(server["capabilities"]["claims"]["propose"], "/{ds}/claims");
}
