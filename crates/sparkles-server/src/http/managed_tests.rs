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

/// `POST /{ds}/claims/review` and `POST /{ds}/claims/withdraw` record decisions and
/// withdrawals, project accepted claims into the policy's output graph and answer each
/// refusal with its status and code, writing nothing.
#[tokio::test]
async fn claims_are_reviewed_and_withdrawn_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("d", DbType::Persistent, None).unwrap();
    let app = router(state.clone());
    let ds = state.get("d").unwrap();
    let post = |path: &'static str, body: J| {
        let app = app.clone();
        async move {
            call(
                &app,
                "POST",
                path,
                Some("application/json"),
                &body.to_string(),
            )
            .await
        }
    };
    let (status, j) = call(
        &app,
        "PUT",
        "/$/evidence/d/settings",
        Some("application/json"),
        &json!({"format": 1, "profile": 1, "enabled": true,
                "sourceRecords": ["urn:ev:sources"], "proposals": ["urn:ev:claims"],
                "decisions": ["urn:ev:decisions"], "supports": ["urn:ev:supports"],
                "projections": ["urn:ev:accepted"],
                "policies": {"main": {"version": 1, "admittedKinds": ["reported"],
                    "requiredScope": [], "conflicts": "refuse",
                    "boundary": ["urn:ev:sources", "urn:ev:claims"],
                    "decisions": "urn:ev:decisions", "supports": "urn:ev:supports",
                    "output": "urn:ev:accepted"}}})
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{j}");
    let propose = |source: &str| {
        json!({
            "revisions": [{"key": "rev", "source": format!("urn:{source}"), "sourceKey": source,
                           "content": {"digest": format!("sha256:{}", "0".repeat(64))}}],
            "claims": [{"key": "claim", "occurrenceKey": "c", "revision": {"key": "rev"},
                        "proposition": "<urn:release> <urn:affectedBy> <urn:cve>",
                        "kind": "reported", "polarity": "positive", "author": "urn:publisher"}],
        })
    };
    let (status, p) = post("/d/claims", propose("advisory")).await;
    assert_eq!(status, StatusCode::OK, "{p}");
    let claim = p["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "claim")
        .unwrap()
        .clone();
    let head = ds.store.head_commit().seq;
    let accept = |if_head: u64, supersedes: J| {
        json!({"policy": "main", "policyVersion": 1, "ifHead": if_head,
               "decisions": [{"key": "a", "target": claim["iri"], "targetDigest": claim["recordDigest"],
                              "disposition": "accepted", "supersedes": supersedes}]})
    };
    // a dry run writes nothing
    let mut dry = accept(head, J::Null);
    dry["dryRun"] = true.into();
    let (status, j) = post("/d/claims/review", dry).await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert_eq!(j["dryRun"]["wouldCommit"], true, "{j}");
    assert_eq!(ds.store.head_commit().seq, head);
    let (status, r) = post("/d/claims/review", accept(head, J::Null)).await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["committed"], true);
    assert!(r["decisions"][0]["support"].is_string(), "{r}");
    assert_eq!(r["changed"]["inserted"][0]["output"], "urn:ev:accepted");
    let ask = |q: &str| ds.dataset.ask(q).unwrap();
    assert!(ask(
        "ASK { GRAPH <urn:ev:accepted> { <urn:release> <urn:affectedBy> <urn:cve> } }"
    ));
    // refusals write nothing
    let head = ds.store.head_commit().seq;
    let mut changed_digest = accept(head, r["decisions"][0]["iri"].clone());
    changed_digest["decisions"][0]["targetDigest"] = format!("sha256:{}", "1".repeat(64)).into();
    let mut version = accept(head, r["decisions"][0]["iri"].clone());
    version["policyVersion"] = 2.into();
    let mut unknown = accept(head, J::Null);
    unknown["policy"] = "nope".into();
    let mut disposition = accept(head, J::Null);
    disposition["decisions"][0]["disposition"] = "maybe".into();
    for (body, want, code) in [
        (
            accept(head - 1, J::Null),
            StatusCode::CONFLICT,
            "head-changed",
        ),
        (
            accept(head, J::Null),
            StatusCode::CONFLICT,
            "decision-superseded",
        ),
        (changed_digest, StatusCode::CONFLICT, "target-changed"),
        (version, StatusCode::CONFLICT, "policy-changed"),
        (unknown, StatusCode::BAD_REQUEST, ""),
        (disposition, StatusCode::BAD_REQUEST, "invalid-reference"),
        (
            json!({"policy": "main"}),
            StatusCode::BAD_REQUEST,
            "invalid-proposal",
        ),
    ] {
        let (status, j) = post("/d/claims/review", body.clone()).await;
        assert_eq!(status, want, "{body}: {j}");
        if !code.is_empty() {
            assert_eq!(j["code"], code, "{body}: {j}");
        }
        assert_eq!(ds.store.head_commit().seq, head, "{body}");
    }
    // a withdrawal removes the last live support's triple
    let (status, w) = post(
        "/d/claims/withdraw",
        json!({"idempotencyKey": "w1", "withdrawals": [{"key": "w", "target": claim["iri"],
               "targetDigest": claim["recordDigest"]}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{w}");
    assert_eq!(w["withdrawals"][0]["existing"], false);
    assert_eq!(w["changed"]["removed"].as_array().unwrap().len(), 1, "{w}");
    assert!(!ask(
        "ASK { GRAPH ?g { <urn:release> <urn:affectedBy> <urn:cve> } }"
    ));
    let (status, again) = post(
        "/d/claims/withdraw",
        json!({"idempotencyKey": "w1", "withdrawals": [{"key": "w", "target": claim["iri"],
               "targetDigest": claim["recordDigest"]}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["replayed"], true);
    // an acceptance of the withdrawn claim
    let head = ds.store.head_commit().seq;
    let (status, j) = post(
        "/d/claims/review",
        accept(head, r["decisions"][0]["iri"].clone()),
    )
    .await;
    assert_eq!(
        (status, j["code"].as_str()),
        (StatusCode::CONFLICT, Some("target-withdrawn"))
    );
    // clients discover the operations
    let (_, server) = call(&app, "GET", "/$/server", None, "").await;
    assert_eq!(
        server["capabilities"]["claims"]["review"],
        "/{ds}/claims/review"
    );
    assert_eq!(
        server["capabilities"]["claims"]["withdraw"],
        "/{ds}/claims/withdraw"
    );
}

/// Identity proposals through `POST /{ds}/claims`, their reviews under a matching
/// policy through `POST /{ds}/claims/review` and the resolution read through `POST
/// /{ds}/claims/identities`, with each refusal's status and code.
#[tokio::test]
async fn identities_are_proposed_reviewed_and_resolved_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("d", DbType::Persistent, None).unwrap();
    let app = router(state.clone());
    let ds = state.get("d").unwrap();
    let post = |path: &'static str, body: J| {
        let app = app.clone();
        async move {
            call(
                &app,
                "POST",
                path,
                Some("application/json"),
                &body.to_string(),
            )
            .await
        }
    };
    let (status, j) = call(
        &app,
        "PUT",
        "/$/evidence/d/settings",
        Some("application/json"),
        &json!({"format": 1, "profile": 1, "enabled": true,
                "sourceRecords": ["urn:ev:sources"], "proposals": ["urn:ev:claims"],
                "decisions": ["urn:ev:decisions"],
                "identity": ["urn:ev:identity", "urn:ev:maps"],
                "projections": ["urn:ev:map"],
                "matchingPolicies": {"people": {"version": 1,
                    "boundary": ["urn:ev:identity", "urn:ev:claims"],
                    "decisions": "urn:ev:decisions", "maps": "urn:ev:maps",
                    "output": "urn:ev:map"}}})
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{j}");
    assert!(
        j["settings"]["matchingPolicies"]["people"].is_object(),
        "{j}"
    );
    let propose = |occurrence: &str, entities: J, relation: &str| {
        json!({"identityGraph": "urn:ev:identity",
               "identities": [{"key": "p", "occurrenceKey": occurrence, "entities": entities,
                               "relation": relation, "author": "urn:matcher",
                               "method": {"method": "urn:method:names", "version": "1"}}]})
    };
    let (status, p) = post(
        "/d/claims",
        propose("ab", json!(["urn:a", "urn:b"]), "possibleMatch"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{p}");
    let ab = p["records"][0].clone();
    assert_eq!(ab["kind"], "IdentityProposal", "{p}");
    assert_eq!(ab["graph"], "urn:ev:identity");
    let (status, j) = post(
        "/d/claims",
        propose("bad", json!(["urn:a"]), "possibleMatch"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{j}");
    let (status, j) = post(
        "/d/claims",
        propose("bad", json!(["urn:a", "urn:b"]), "sameAs"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{j}");
    let (_, d) = post(
        "/d/claims",
        propose("ab-apart", json!(["urn:a", "urn:b"]), "distinctEntities"),
    )
    .await;
    let apart = d["records"][0].clone();
    let review = |target: &J, disposition: &str, supersedes: J| {
        json!({"policy": "people", "policyVersion": 1, "ifHead": ds.store.head_commit().seq,
               "decisions": [{"key": "x", "target": target["iri"],
                              "targetDigest": target["recordDigest"],
                              "disposition": disposition, "supersedes": supersedes}]})
    };
    let (status, r) = post("/d/claims/review", review(&ab, "accepted", J::Null)).await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["changed"]["map"]["number"], 1, "{r}");
    assert_eq!(r["mapRevision"], r["changed"]["map"]["revision"]);
    assert_eq!(r["changed"]["inserted"].as_array().unwrap().len(), 2, "{r}");
    assert!(ds.dataset.ask(
        "ASK { GRAPH <urn:ev:map> { <urn:b> <urn:x-sparkles:evidence:resolvesTo> <urn:a> } }"
    ).unwrap());
    // a separation of a cluster conflicts and writes nothing
    let head = ds.store.head_commit().seq;
    let (status, j) = post("/d/claims/review", review(&apart, "accepted", J::Null)).await;
    assert_eq!(
        (status, j["code"].as_str()),
        (StatusCode::CONFLICT, Some("identity-conflict")),
        "{j}"
    );
    assert_eq!(ds.store.head_commit().seq, head);
    // the read
    let (status, read) = post(
        "/d/claims/identities",
        json!({"policy": "people", "entities": ["urn:b", "urn:z"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(read["number"], 1);
    assert_eq!(read["entities"][0]["representative"], "urn:a");
    assert_eq!(read["entities"][0]["members"], json!(["urn:a", "urn:b"]));
    assert_eq!(
        read["entities"][0]["decisions"],
        json!([r["decisions"][0]["iri"]])
    );
    assert_eq!(read["entities"][1]["representative"], "urn:z");
    let (status, _) = post(
        "/d/claims/identities",
        json!({"policy": "nobody", "entities": ["urn:b"]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = post(
        "/d/claims/identities",
        json!({"policy": "people", "entities": ["urn:b"], "extra": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // an analysis that used the map's revision
    let (status, a) = post(
        "/d/claims",
        json!({"proposalGraph": "urn:ev:claims",
               "assessments": [{"key": "a", "occurrenceKey": "a1", "target": {"iri": ab["iri"]},
                                "conclusion": "<urn:conclusion:plausible>",
                                "method": {"method": "urn:method:review", "version": "1"},
                                "configurationDigest": format!("sha256:{}", "0".repeat(64)),
                                "evaluatedAt": "2026-01-02T00:00:00Z",
                                "evaluator": "urn:analyst",
                                "inputs": [{"iri": r["mapRevision"]}]}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{a}");
    let analysis = a["records"][0]["iri"].clone();
    // a split reverses the match, and the map is empty again
    let (status, s) = post(
        "/d/claims/review",
        review(&ab, "rejected", r["decisions"][0]["iri"].clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{s}");
    assert_eq!(s["changed"]["map"]["previous"], r["mapRevision"], "{s}");
    assert_eq!(s["changed"]["removed"].as_array().unwrap().len(), 2, "{s}");
    assert!(
        !ds.dataset
            .ask("ASK { GRAPH <urn:ev:map> { ?s ?p ?o } }")
            .unwrap()
    );
    // the split's predecessor finds the analysis that used it
    let (status, found) = post(
        "/d/claims/dependents",
        json!({"changed": [s["changed"]["map"]["previous"]], "transitive": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(
        found["dependents"],
        json!([{"assessment": analysis, "graph": "urn:ev:claims",
                "on": r["mapRevision"], "dependence": "used", "depth": 0}]),
        "{found}"
    );
    for bad in [
        json!({"changed": []}),
        json!({"changed": ["not an iri"]}),
        json!({"changed": ["urn:a"], "extra": 1}),
    ] {
        let (status, j) = post("/d/claims/dependents", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{j}");
    }
    let (_, server) = call(&app, "GET", "/$/server", None, "").await;
    assert_eq!(
        server["capabilities"]["claims"]["identities"],
        "/{ds}/claims/identities"
    );
    assert_eq!(
        server["capabilities"]["claims"]["dependents"],
        "/{ds}/claims/dependents"
    );
}

#[tokio::test]
async fn cross_dataset_claims_proposal_holds_referenced_dataset_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("ev", DbType::Persistent, None).unwrap();
    state.attach("inv", DbType::Persistent, None).unwrap();
    let ev = state.get("ev").unwrap();
    let inv = state.get("inv").unwrap();
    let app = router(state.clone());
    // configure evidence only on ev; inv stays an ordinary dataset
    let (status, j) = call(
        &app,
        "PUT",
        "/$/evidence/ev/settings",
        Some("application/json"),
        &json!({
            "format": 1,
            "profile": 1,
            "enabled": true,
            "sourceRecords": ["urn:ev:sources"],
            "proposals": ["urn:ev:claims"],
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{j}");
    // Record a fact at a past commit before tightening retention.
    inv.dataset
        .update("INSERT DATA { GRAPH <urn:inv> { <urn:host> <urn:runs> <urn:release> } }")
        .unwrap();
    let fact_commit = inv.store.head_commit().seq;
    inv.dataset
        .update("DELETE DATA { GRAPH <urn:inv> { <urn:host> <urn:runs> <urn:release> } }")
        .unwrap();
    let body = foreign_claims_body(inv.store.dataset_id(), fact_commit);
    use crate::auth::{Access, Endpoint, Grants, Kind, Level, Principal, Restricted, Scheme};
    let principal = Principal::new(
        Kind::User,
        "proposer",
        Scheme::Basic,
        Access::of(Grants {
            datasets: vec![("ev".into(), Level::Write)],
            restricted: vec![Restricted {
                dataset: "inv".into(),
                level: Level::Read,
                endpoints: Some(vec![Endpoint::Query]),
                graphs: None,
                lifts: Vec::new(),
                branches: None,
                server_models: false,
            }],
            ..Default::default()
        }),
    );
    let inv_root = inv.store.root().unwrap();
    let metadata = std::fs::read(inv_root.join("dataset.json")).unwrap();
    let origin_head = ev.store.head_commit().seq;
    let denied = crate::evidence::execute(
        "propose",
        state.clone(),
        "ev".into(),
        principal,
        body.to_string().into(),
        crate::evidence::Execution {
            branch: sparkles::branch::MAIN.into(),
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            deadline: None,
            grant: None,
        },
    )
    .expect_err("query read does not grant authority to create a foreign hold");
    assert_eq!(denied.into_parts().0, StatusCode::FORBIDDEN);
    assert_eq!(ev.store.head_commit().seq, origin_head);
    assert_eq!(
        std::fs::read(inv_root.join("dataset.json")).unwrap(),
        metadata
    );
    assert!(!inv_root.join(sparkles::holds::FILE).exists());
    let (status, proposed) = call(
        &app,
        "POST",
        "/ev/claims",
        Some("application/json"),
        &body.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{proposed}");
    assert_eq!(proposed["committed"], true);
    let hold_id = proposed["hold"].as_str().expect("hold uuid").to_string();
    // GET /$/evidence/inv/holds lists the cross-dataset hold with forDatasetId = ev
    let (status, listed) = call(&app, "GET", "/$/evidence/inv/holds", None, "").await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let holds = listed["holds"].as_array().unwrap();
    assert_eq!(holds.len(), 1);
    assert_eq!(holds[0]["id"], hold_id);
    assert_eq!(holds[0]["forDatasetId"], ev.store.dataset_id().to_string());
    inv.store
        .set_retention(sparkles::history::Retention {
            keep_commits: Some(1),
            keep_age_ms: None,
            max_bytes: Some(1),
        })
        .unwrap();
    // compact inv repeatedly; the held fact stays resolvable
    for i in 0..3 {
        inv.dataset
            .update(&format!("INSERT DATA {{ <urn:n> <urn:v> {i} }}"))
            .unwrap();
        inv.store.compact().unwrap();
    }
    inv.store.history_tick().unwrap();
    let (status, resolved) = call(
        &app,
        "POST",
        "/ev/claims/resolve",
        Some("application/json"),
        &json!({"references": [{"key": "f", "storedFact": {
            "datasetId": inv.store.dataset_id(),
            "branchId": inv.store.dataset_id(),
            "commit": fact_commit,
            "graph": "urn:inv",
            "triple": "<urn:host> <urn:runs> <urn:release>",
        }}]})
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/$/evidence/inv/holds/{hold_id}"),
        None,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    inv.store.compact().unwrap();
    inv.store.history_tick().unwrap();
    let (status, gone) = call(
        &app,
        "POST",
        "/ev/claims/resolve",
        Some("application/json"),
        &json!({"references": [{"key": "f", "storedFact": {
            "datasetId": inv.store.dataset_id(),
            "branchId": inv.store.dataset_id(),
            "commit": fact_commit,
            "graph": "urn:inv",
            "triple": "<urn:host> <urn:runs> <urn:release>",
        }}]})
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{gone}");
    assert_eq!(gone["code"], "evidence-unavailable");
}

fn foreign_claims_body(dataset_id: uuid::Uuid, fact_commit: u64) -> serde_json::Value {
    let digest =
        sparkles::evidence::Sha256Digest::of_bytes(b"note", &sparkles::task::Control::none())
            .unwrap()
            .to_string();
    json!({
        "revisions": [{"key": "rev", "source": "urn:src", "sourceKey": "s1",
                       "content": {"digest": digest}}],
        "references": [{"key": "ref", "occurrenceKey": "r1", "storedFact": {
            "datasetId": dataset_id,
            "branchId": dataset_id,
            "commit": fact_commit,
            "graph": "urn:inv",
            "triple": "<urn:host> <urn:runs> <urn:release>",
        }}],
        "claims": [{"key": "c", "occurrenceKey": "c1", "revision": {"key": "rev"},
                    "proposition": "<urn:host> <urn:runs> <urn:release>",
                    "kind": "reported", "polarity": "positive", "author": "urn:a"}],
        "links": [{"key": "l", "occurrenceKey": "l1", "target": {"key": "c"},
                   "evidence": {"key": "ref"}, "relation": "supports", "author": "urn:a"}],
    })
}

#[test]
fn cross_dataset_proposal_rejects_a_source_renamed_before_acquisition() {
    memory_source_rename_refuses("evidence-sources-resolved", 0);
}

#[test]
fn cross_dataset_proposal_rejects_a_source_renamed_before_publication() {
    memory_source_rename_refuses("evidence-held", 1);
}

fn memory_source_rename_refuses(point: &'static str, expected_holds: usize) {
    use crate::auth::{Access, Grants, Kind, Level, Principal, Scheme};
    use sparkles::task::Control;
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let ev = state.create("ev", DbType::Memory).unwrap();
    let inv = state.create("inv", DbType::Memory).unwrap();
    sparkles::evidence::configure(
        &ev.dataset,
        &sparkles::evidence::Settings {
            enabled: true,
            source_records: vec!["urn:ev:sources".into()],
            proposals: vec!["urn:ev:claims".into()],
            ..Default::default()
        },
        &Control::none(),
    )
    .unwrap();
    inv.dataset
        .update("INSERT DATA { GRAPH <urn:inv> { <urn:host> <urn:runs> <urn:release> } }")
        .unwrap();
    let body = foreign_claims_body(inv.store.dataset_id(), inv.store.head_commit().seq);
    let principal = Principal::new(
        Kind::User,
        "proposer",
        Scheme::Basic,
        Access::of(Grants {
            datasets: vec![
                ("ev".into(), Level::Write),
                ("inv".into(), Level::Admin),
                ("renamed".into(), Level::Read),
            ],
            ..Default::default()
        }),
    );
    assert!(principal.admin_everywhere("inv"));
    assert!(!principal.admin_everywhere("renamed"));
    let catalog = state.catalog.clone();
    ev.store.set_failpoint(
        point,
        Some(Arc::new(move |_| {
            catalog.rename("inv", "renamed").unwrap();
        })),
    );
    let origin_head = ev.store.head_commit().seq;
    let refused = crate::evidence::execute(
        "propose",
        state.clone(),
        "ev".into(),
        principal,
        body.to_string().into(),
        crate::evidence::Execution {
            branch: sparkles::branch::MAIN.into(),
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            deadline: None,
            grant: None,
        },
    );
    ev.store.set_failpoint(point, None);
    let refused = refused.expect_err("the authorized source name changed");
    assert_eq!(refused.into_parts().0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(ev.store.head_commit().seq, origin_head);
    let renamed = state.catalog.get("renamed").unwrap();
    assert!(std::ptr::eq(inv.dataset.store(), renamed.store()));
    assert_eq!(
        sparkles::evidence::holds(&renamed).unwrap().len(),
        expected_holds
    );
}

/// `POST /$/evidence/{ds}/identities/rebuild` (item 643): rebuilds a matching policy's
/// resolution map after a boundary removal or detach-and-reprotect, supports dry-run,
/// idempotency replay, branches and restart durability, and restores reviews and reads.
#[tokio::test]
async fn identity_map_rebuild_over_http_restores_reviews_reads_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let settings_json = |boundary: &[&str]| {
        json!({
            "format": 1, "profile": 1, "enabled": true,
            "sourceRecords": ["urn:ev:sources"], "proposals": ["urn:ev:claims"],
            "decisions": ["urn:ev:decisions"],
            "identity": ["urn:ev:identity", "urn:ev:identity2", "urn:ev:maps"],
            "projections": ["urn:ev:map"],
            "matchingPolicies": {"people": {
                "version": 1,
                "boundary": boundary,
                "decisions": "urn:ev:decisions",
                "maps": "urn:ev:maps",
                "output": "urn:ev:map"
            }}
        })
    };
    let rev3_iri;
    {
        let state = Arc::new(
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
        );
        state.attach("d", DbType::Persistent, None).unwrap();
        let app = router(state.clone());
        let ds = state.get("d").unwrap();
        let post = |path: &str, body: J| {
            let app = app.clone();
            let path = path.to_string();
            async move {
                call(
                    &app,
                    "POST",
                    &path,
                    Some("application/json"),
                    &body.to_string(),
                )
                .await
            }
        };
        let (status, j) = call(
            &app,
            "PUT",
            "/$/evidence/d/settings",
            Some("application/json"),
            &settings_json(&["urn:ev:identity", "urn:ev:identity2", "urn:ev:claims"]).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{j}");

        let propose = |graph: &str, occurrence: &str, entities: J| {
            json!({
                "identityGraph": graph,
                "identities": [{
                    "key": "p", "occurrenceKey": occurrence, "entities": entities,
                    "relation": "possibleMatch", "author": "urn:matcher",
                    "method": {"method": "urn:method:names", "version": "1"}
                }]
            })
        };
        let (s, p1) = post(
            "/d/claims",
            propose("urn:ev:identity2", "ab", json!(["urn:a", "urn:b"])),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{p1}");
        let ab = p1["records"][0].clone();
        let (s, p2) = post(
            "/d/claims",
            propose("urn:ev:identity", "cd", json!(["urn:c", "urn:d"])),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{p2}");
        let cd = p2["records"][0].clone();

        let review_body = |target: &J| {
            json!({
                "policy": "people", "policyVersion": 1, "ifHead": ds.store.head_commit().seq,
                "decisions": [{
                    "key": "x", "target": target["iri"], "targetDigest": target["recordDigest"],
                    "disposition": "accepted"
                }]
            })
        };
        let (s, r1) = post("/d/claims/review", review_body(&ab)).await;
        assert_eq!(s, StatusCode::OK, "{r1}");
        let dab = r1["decisions"][0]["iri"].clone();
        let (s, r2) = post("/d/claims/review", review_body(&cd)).await;
        assert_eq!(s, StatusCode::OK, "{r2}");
        let rev2 = r2["mapRevision"].clone();

        // Create branch `dev` before the boundary change.
        let (s, b) = call(
            &app,
            "POST",
            "/$/branches/d",
            Some("application/json"),
            &json!({"name": "dev"}).to_string(),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{b}");

        // Remove urn:ev:identity2 from the boundary -> projection-unavailable.
        let (s, j) = call(
            &app,
            "PUT",
            "/$/evidence/d/settings",
            Some("application/json"),
            &settings_json(&["urn:ev:identity", "urn:ev:claims"]).to_string(),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{j}");
        let (s, j) = post(
            "/d/claims/identities",
            json!({"policy": "people", "entities": ["urn:a", "urn:c"]}),
        )
        .await;
        assert_eq!(
            (s, j["code"].as_str()),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Some("projection-unavailable")
            ),
            "{j}"
        );

        // Malformed / invalid preconditions.
        let (s, _) = post(
            "/$/evidence/d/identities/rebuild",
            json!({"policy": "nobody"}),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _) = post(
            "/$/evidence/d/identities/rebuild",
            json!({"policy": "people", "extra": 1}),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, j) = post(
            "/$/evidence/d/identities/rebuild",
            json!({"policy": "people", "policyVersion": 9}),
        )
        .await;
        assert_eq!(
            (s, j["code"].as_str()),
            (StatusCode::CONFLICT, Some("policy-changed")),
            "{j}"
        );
        let (s, j) = post(
            "/$/evidence/d/identities/rebuild",
            json!({"policy": "people", "ifHead": 0}),
        )
        .await;
        assert_eq!(
            (s, j["code"].as_str()),
            (StatusCode::CONFLICT, Some("head-changed")),
            "{j}"
        );

        // Dry run does not commit.
        let head = ds.store.head_commit().seq;
        let (s, dry) = post(
            "/$/evidence/d/identities/rebuild",
            json!({"policy": "people", "policyVersion": 1, "ifHead": head, "dryRun": true}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{dry}");
        assert_eq!(dry["committed"], false);
        assert_eq!(dry["dryRun"]["wouldCommit"], true);
        assert_eq!(ds.store.head_commit().seq, head);

        // Commit rebuild with idempotency key.
        let (s, rebuilt) = post(
            "/$/evidence/d/identities/rebuild",
            json!({"policy": "people", "policyVersion": 1, "ifHead": head, "idempotencyKey": "rb-1"}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{rebuilt}");
        assert_eq!(rebuilt["committed"], true);
        assert_eq!(rebuilt["replayed"], false);
        assert_eq!(rebuilt["number"], 3);
        assert_eq!(rebuilt["previous"], rev2);
        assert_eq!(rebuilt["boundary"], json!(["urn:ev:identity"]));
        assert_eq!(rebuilt["changed"]["map"]["decisionsEnded"], json!([dab]));
        rev3_iri = rebuilt["revision"].clone();

        // Replay returns first receipt; changed payload conflicts.
        let (s, replay) = post(
            "/$/evidence/d/identities/rebuild",
            json!({"policy": "people", "policyVersion": 1, "ifHead": head, "idempotencyKey": "rb-1"}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{replay}");
        assert_eq!(replay["committed"], false);
        assert_eq!(replay["replayed"], true);
        assert_eq!(replay["revision"], rev3_iri);
        assert_eq!(replay["boundary"], json!(["urn:ev:identity"]));
        let (s, conflict) = post(
            "/$/evidence/d/identities/rebuild",
            json!({"policy": "people", "idempotencyKey": "rb-1"}),
        )
        .await;
        assert_eq!(
            (s, conflict["code"].as_str()),
            (StatusCode::CONFLICT, Some("idempotency-conflict")),
            "{conflict}"
        );

        // Replay of rb-1 (policyVersion: 1) still returns the original receipt after bumping
        // policy version to 2, while a fresh key with policyVersion: 1 gets policy-changed.
        let mut bumped = settings_json(&["urn:ev:identity", "urn:ev:claims"]);
        bumped["matchingPolicies"]["people"]["version"] = json!(2);
        let (s, j) = call(
            &app,
            "PUT",
            "/$/evidence/d/settings",
            Some("application/json"),
            &bumped.to_string(),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{j}");
        let (s, replay_after_bump) = post(
            "/$/evidence/d/identities/rebuild",
            json!({"policy": "people", "policyVersion": 1, "ifHead": head, "idempotencyKey": "rb-1"}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{replay_after_bump}");
        assert_eq!(replay_after_bump["replayed"], true);
        assert_eq!(replay_after_bump["committed"], false);
        assert_eq!(replay_after_bump["policyVersion"], 1);
        assert_eq!(replay_after_bump["revision"], rev3_iri);
        let (s, stale_v) = post(
            "/$/evidence/d/identities/rebuild",
            json!({"policy": "people", "policyVersion": 1, "idempotencyKey": "rb-fresh"}),
        )
        .await;
        assert_eq!(
            (s, stale_v["code"].as_str()),
            (StatusCode::CONFLICT, Some("policy-changed")),
            "{stale_v}"
        );
        // Restore version 1 for subsequent steps in this fixture.
        let (s, j) = call(
            &app,
            "PUT",
            "/$/evidence/d/settings",
            Some("application/json"),
            &settings_json(&["urn:ev:identity", "urn:ev:claims"]).to_string(),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{j}");

        // Reads and reviews succeed again on main.
        let (s, read) = post(
            "/d/claims/identities",
            json!({"policy": "people", "entities": ["urn:a", "urn:d"]}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{read}");
        assert_eq!(read["revision"], rev3_iri);
        assert_eq!(read["entities"][0]["representative"], "urn:a");
        assert_eq!(read["entities"][1]["representative"], "urn:c");

        // Rebuild also works on a branch (?branch=dev).
        let (s, dev_rb) = post(
            "/$/evidence/d/identities/rebuild?branch=dev",
            json!({"policy": "people"}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{dev_rb}");
        assert_eq!(dev_rb["committed"], true);
        let (s, dev_read) = post(
            "/d/claims/identities?branch=dev",
            json!({"policy": "people", "entities": ["urn:a", "urn:d"]}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{dev_read}");
        assert_eq!(dev_read["entities"][0]["representative"], "urn:a");
        assert_eq!(dev_read["entities"][1]["representative"], "urn:c");

        // Detach and re-protect the remaining identity graph -> projection-unavailable -> rebuild -> empty map.
        let (s, _) = post("/$/evidence/d/detach", json!({"graph": "urn:ev:identity"})).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = call(
            &app,
            "PUT",
            "/$/evidence/d/settings",
            Some("application/json"),
            &settings_json(&["urn:ev:identity", "urn:ev:claims"]).to_string(),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (s, j) = post(
            "/d/claims/identities",
            json!({"policy": "people", "entities": ["urn:c"]}),
        )
        .await;
        assert_eq!(
            (s, j["code"].as_str()),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Some("projection-unavailable")
            ),
            "{j}"
        );
        let (s, rebuilt2) = post(
            "/$/evidence/d/identities/rebuild",
            json!({"policy": "people"}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{rebuilt2}");
        assert_eq!(rebuilt2["number"], 4);
        assert_eq!(rebuilt2["previous"], rev3_iri);
    }

    // Reopen persistent server state and verify rebuilt map survives restart.
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    if state.get("d").is_none() {
        state.attach("d", DbType::Persistent, None).unwrap();
    }
    let app = router(state.clone());
    let (s, read) = call(
        &app,
        "POST",
        "/d/claims/identities",
        Some("application/json"),
        &json!({"policy": "people", "entities": ["urn:c"]}).to_string(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{read}");
    assert_eq!(read["number"], 4);
    assert_eq!(read["entities"][0]["representative"], "urn:c");
    assert_eq!(read["entities"][0]["members"], json!(["urn:c"]));
    let (s, p3) = call(
        &app,
        "POST",
        "/d/claims",
        Some("application/json"),
        &json!({
            "identityGraph": "urn:ev:identity",
            "identities": [{
                "key": "p", "occurrenceKey": "xy", "entities": ["urn:x", "urn:y"],
                "relation": "possibleMatch", "author": "urn:matcher",
                "method": {"method": "urn:method:names", "version": "1"}
            }]
        })
        .to_string(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p3}");
    let ds = state.get("d").unwrap();
    let xy = &p3["records"][0];
    let (s, r) = call(
        &app,
        "POST",
        "/d/claims/review",
        Some("application/json"),
        &json!({
            "policy": "people", "policyVersion": 1, "ifHead": ds.store.head_commit().seq,
            "decisions": [{
                "key": "x", "target": xy["iri"], "targetDigest": xy["recordDigest"],
                "disposition": "accepted"
            }]
        })
        .to_string(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["changed"]["map"]["number"], 5);
}

/// `POST /$/evidence/{ds}/reauthenticate` (spec C25, roadmap item 552): verifies records
/// written under an earlier protection nonce after a detach + re-protect, refuses
/// tampered records cleanly, restores resolution and proposals, and matches OpenAPI.
#[tokio::test]
async fn evidence_reauthenticate_http_route_and_openapi_contract() {
    let dir = tempfile::tempdir().unwrap();
    // Initialize persistent dataset and activate configuration history before opening.
    let ds_path = dir.path().join("databases").join("d");
    {
        let _ds = sparkles::Dataset::open(&ds_path).unwrap();
        drop(_ds);
        let prep =
            sparkles::access::Preparation::new(sparkles::task::Control::none(), 64 << 20).unwrap();
        sparkles::migrate::activate_configuration_history(&ds_path, false, &prep).unwrap();
    }
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state
        .attach("d", DbType::Persistent, Some(&ds_path))
        .unwrap();
    // Attach a second unactivated dataset to verify `409 configuration-history-inactive`.
    state.attach("legacy", DbType::Memory, None).unwrap();
    let app = router(state.clone());
    let post = |uri: &'static str, body: J| {
        let app = app.clone();
        async move {
            call(
                &app,
                "POST",
                uri,
                Some("application/json"),
                &body.to_string(),
            )
            .await
        }
    };
    let settings = json!({
        "format": 1,
        "profile": 1,
        "enabled": true,
        "sourceRecords": ["urn:ev:sources"],
        "proposals": ["urn:ev:claims"],
        "decisions": ["urn:ev:decisions"],
        "supports": ["urn:ev:supports"],
        "projections": ["urn:ev:accepted"],
        "policies": {
            "advisories": {
                "version": 1,
                "admittedKinds": ["reported"],
                "requiredScope": [],
                "conflicts": "refuse",
                "boundary": ["urn:ev:sources", "urn:ev:claims"],
                "decisions": "urn:ev:decisions",
                "supports": "urn:ev:supports",
                "output": "urn:ev:accepted"
            }
        }
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

    let batch = json!({
        "revisions": [{
            "key": "rev",
            "source": "https://example.org/src",
            "sourceKey": "src-1",
            "content": { "text": "hello world" }
        }],
        "claims": [{
            "key": "c1",
            "occurrenceKey": "c1",
            "revision": { "key": "rev" },
            "proposition": "<https://example.org/a> <https://example.org/p> <https://example.org/b>",
            "kind": "reported",
            "polarity": "positive",
            "author": "https://example.org/author"
        }]
    });
    let (status, proposed) = post("/d/claims", batch.clone()).await;
    assert_eq!(status, StatusCode::OK, "{proposed}");
    let rev_iri = proposed["records"][0]["iri"].as_str().unwrap().to_string();
    let claim_iri = proposed["records"][1]["iri"].as_str().unwrap().to_string();
    let claim_digest = proposed["records"][1]["recordDigest"]
        .as_str()
        .unwrap()
        .to_string();

    // Detach and re-protect `urn:ev:claims`.
    let (status, _) = post("/$/evidence/d/detach", json!({ "graph": "urn:ev:claims" })).await;
    assert_eq!(status, StatusCode::OK);
    // Refuses while still detached.
    let (status, j) = post(
        "/$/evidence/d/reauthenticate",
        json!({ "graph": "urn:ev:claims" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{j}");
    assert_eq!(j["code"], "unsupported-profile");

    // Re-protect.
    let (status, _) = call(
        &app,
        "PUT",
        "/$/evidence/d/settings",
        Some("application/json"),
        &settings.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Resolution fails with `origin-not-reproduced`, and re-proposal fails with
    // `immutable-record-conflict`.
    let (status, j) = post(
        "/d/claims/resolve",
        json!({ "references": [{ "key": "k", "record": { "iri": claim_iri } }] }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{j}");
    assert_eq!(j["code"], "origin-not-reproduced");
    let (status, j) = post("/d/claims", batch.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{j}");
    assert_eq!(j["code"], "immutable-record-conflict");

    // Dry-run preview works without committing.
    let (status, dry) = post(
        "/$/evidence/d/reauthenticate",
        json!({ "graph": "urn:ev:claims", "dryRun": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{dry}");
    assert_eq!(dry["committed"], false);
    assert_eq!(dry["dryRun"]["wouldCommit"], true);

    // Real reconciliation with idempotency key and pinned recordDigest succeeds.
    let req_body = json!({
        "graph": "urn:ev:claims",
        "idempotencyKey": "http-reauth-1",
        "records": [{ "iri": claim_iri, "recordDigest": claim_digest }],
        "message": "re-authenticate claims after maintenance"
    });
    let (status, res) = post("/$/evidence/d/reauthenticate", req_body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{res}");
    assert_eq!(res["committed"], true);
    assert_eq!(res["replayed"], false);
    assert_eq!(res["records"][0]["iri"], claim_iri);
    assert_eq!(res["records"][0]["kind"], "Claim");
    assert_eq!(res["records"][0]["alreadyAuthenticated"], false);

    // Replay returns the original receipt cleanly.
    let (status, replay) = post("/$/evidence/d/reauthenticate", req_body).await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["committed"], false);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["operation"], res["operation"]);
    assert_eq!(replay["commit"], res["commit"]);

    // Same key with a different payload returns 409 idempotency-conflict.
    let (status, conflict) = post(
        "/$/evidence/d/reauthenticate",
        json!({ "graph": "urn:ev:claims", "idempotencyKey": "http-reauth-1" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["code"], "idempotency-conflict");

    // Resolution and new proposals referencing the records now succeed.
    let (status, resolved) = post(
        "/d/claims/resolve",
        json!({ "references": [{ "key": "k", "record": { "iri": claim_iri } }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    assert_eq!(resolved["resolved"][0]["record"]["iri"], claim_iri);
    let (status, followup) = post(
        "/d/claims",
        json!({
            "claims": [{
                "key": "c2",
                "occurrenceKey": "c2",
                "revision": { "iri": rev_iri },
                "proposition": "<https://example.org/a> <https://example.org/p> <https://example.org/c>",
                "kind": "observation",
                "polarity": "positive",
                "author": "https://example.org/author"
            }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{followup}");

    // Tampering while detached causes 409 immutable-record-conflict over HTTP.
    let (status, _) = post("/$/evidence/d/detach", json!({ "graph": "urn:ev:claims" })).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "POST",
        "/d/update",
        Some(UPDATE),
        &format!("INSERT DATA {{ GRAPH <urn:ev:claims> {{ <{claim_iri}> <https://example.org/bad> \"1\" }} }}"),
    )
    .await;
    assert!(status.is_success());
    let (status, _) = call(
        &app,
        "PUT",
        "/$/evidence/d/settings",
        Some("application/json"),
        &settings.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, tampered) = post(
        "/$/evidence/d/reauthenticate",
        json!({ "graph": "urn:ev:claims" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{tampered}");
    assert_eq!(tampered["code"], "immutable-record-conflict");

    // Unactivated dataset returns 409 configuration-history-inactive.
    let (status, _) = call(
        &app,
        "PUT",
        "/$/evidence/legacy/settings",
        Some("application/json"),
        &settings.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, inactive) = post(
        "/$/evidence/legacy/reauthenticate",
        json!({ "graph": "urn:ev:claims" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{inactive}");
    assert_eq!(inactive["code"], "configuration-history-inactive");

    // Projection graphs and malformed requests are rejected with consistent plain 400
    // errors (never mixing `invalid-reference` with plain 400).
    for bad in [
        json!({ "graph": "urn:ev:accepted" }),
        json!({ "graph": "urn:ev:claims", "records": [] }),
        json!({ "graph": "not an iri" }),
        json!({ "graph": "urn:ev:claims", "idempotencyKey": "" }),
        json!({ "graph": "urn:ev:claims", "extra": 1 }),
    ] {
        let (status, j) = post("/$/evidence/d/reauthenticate", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{j}");
        assert!(j.get("code").is_none(), "unexpected error code on 400: {j}");
    }

    // Response matches the published OpenAPI schema.
    let doc = crate::openapi::build("0.0.0");
    let schema = doc["paths"]["/$/evidence/{ds}/reauthenticate"]["post"]["responses"]["200"]
        ["content"]["application/json"]["schema"]
        .clone();
    assert_eq!(
        crate::openapi::contract_tests::mismatch(&doc, &schema, &res, "$"),
        None
    );

    // Chunked streaming body above 4 MiB is rejected with 413 at extraction time even
    // when the general admin ceiling is much larger (16 MiB default).
    {
        use axum::body::{Body, Bytes};
        use axum::http::Request;
        use std::sync::Arc as StdArc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tower::ServiceExt as _;

        let yielded = StdArc::new(AtomicUsize::new(0));
        let yielded_stream = yielded.clone();
        // 16 chunks x 512 KiB = 8 MiB total, streamed with no Content-Length.
        let chunk = Bytes::from(vec![b' '; 512 * 1024]);
        let stream = futures_util::stream::iter((0..16).map(move |_| {
            yielded_stream.fetch_add(1, Ordering::SeqCst);
            Ok::<_, std::io::Error>(chunk.clone())
        }));
        let req = Request::builder()
            .method("POST")
            .uri("/$/evidence/d/reauthenticate")
            .header("content-type", "application/json")
            .body(Body::from_stream(stream))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        // Stopped streaming right after crossing 4 MiB (<= 10 chunks, not all 16).
        assert!(
            yielded.load(Ordering::SeqCst) <= 10,
            "stream was buffered past 4 MiB: yielded {} chunks",
            yielded.load(Ordering::SeqCst)
        );
    }

    // Read-only server rejects re-authentication with 403.
    {
        let ro_dir = tempfile::tempdir().unwrap();
        let mut ro_state = AppState::new(
            ro_dir.path(),
            StoreOptions::default(),
            Duration::from_secs(30),
        )
        .unwrap();
        ro_state.read_only = true;
        let ro_state = Arc::new(ro_state);
        ro_state.attach("d", DbType::Memory, None).unwrap();
        let ro_app = router(ro_state);
        let (status, j) = call(
            &ro_app,
            "POST",
            "/$/evidence/d/reauthenticate",
            Some("application/json"),
            &json!({ "graph": "urn:ev:claims" }).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{j}");
    }

    // Local preparation budget exhaustion during whole-graph discovery must return
    // HTTP 422 `incomplete-evidence` (never HTTP 507 Insufficient Storage).
    {
        let (status, _) = call(
            &app,
            "POST",
            "/$/evidence/d/detach",
            Some("application/json"),
            &json!({ "graph": "urn:ev:claims" }).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        {
            let ds = state.datasets().remove("d").unwrap();
            let mut t = ds.dataset.store().write();
            let mut labels = std::collections::HashMap::new();
            let gn = oxrdf::GraphName::NamedNode(oxrdf::NamedNode::new_unchecked("urn:ev:claims"));
            let rdf_type =
                oxrdf::NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
            let claim_cls = oxrdf::NamedNode::new_unchecked("urn:x-sparkles:evidence:Claim");
            // 520 subjects x ~132 KiB IRI ≈ 68 MiB > 64 MiB local preparation budget.
            let pad = "a".repeat(132 * 1024);
            for i in 0..520 {
                let subj = oxrdf::NamedNode::new_unchecked(format!("urn:long:{i}:{pad}"));
                let q = oxrdf::Quad::new(subj, rdf_type.clone(), claim_cls.clone(), gn.clone());
                let ids = t.encode_quad(&q, &mut labels).unwrap();
                t.insert(ids).unwrap();
            }
            t.commit().unwrap();
        }
        let (status, _) = call(
            &app,
            "PUT",
            "/$/evidence/d/settings",
            Some("application/json"),
            &settings.to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, budget_err) = post(
            "/$/evidence/d/reauthenticate",
            json!({ "graph": "urn:ev:claims" }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{budget_err}");
        assert_eq!(budget_err["code"], "incomplete-evidence");
    }
}
