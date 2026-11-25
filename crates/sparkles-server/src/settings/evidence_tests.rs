//! Declared evidence settings (spec C25 §7, C19): `serve --settings` declarations,
//! locks, drift refusal across typed evidence operations, protection invariants and
//! reconciliation.

use super::*;
use crate::state::DbType;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

const UPDATE: &str = "application/sparql-update";

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let res = app.clone().oneshot(req).await.unwrap();
    let s = res.status();
    let etag = res
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let b = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null), etag)
}

fn req(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    let b = Request::builder().method(method).uri(uri);
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

fn req_if_match(method: &str, uri: &str, etag: &str, body: Option<Value>) -> Request<Body> {
    let b = Request::builder()
        .method(method)
        .uri(uri)
        .header("if-match", etag);
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

fn sparql_update(uri: &str, query: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", UPDATE)
        .body(Body::from(query.to_string()))
        .unwrap()
}

fn state_with_settings(dir: &std::path::Path, settings: Option<&std::path::Path>) -> Arc<AppState> {
    let mut st = AppState::new(
        dir,
        sparkles::store::StoreOptions::default(),
        std::time::Duration::from_secs(30),
    )
    .unwrap();
    if let Some(p) = settings {
        st.settings = Settings::load(p, None).unwrap();
    }
    Arc::new(st)
}

fn full_settings() -> Value {
    json!({
        "format": 1,
        "profile": 1,
        "enabled": true,
        "sourceRecords": ["urn:ev:sources"],
        "proposals": ["urn:ev:claims"],
        "decisions": ["urn:ev:decisions"],
        "supports": ["urn:ev:supports"],
        "identity": ["urn:ev:identity"],
        "projections": ["urn:ev:accepted", "urn:ev:people"],
        "policies": {
            "advisories": {
                "version": 1,
                "admittedKinds": ["reported"],
                "conflicts": "refuse",
                "boundary": ["urn:ev:sources", "urn:ev:claims"],
                "decisions": "urn:ev:decisions",
                "supports": "urn:ev:supports",
                "output": "urn:ev:accepted"
            }
        },
        "matchingPolicies": {
            "people": {
                "version": 1,
                "boundary": ["urn:ev:identity", "urn:ev:claims"],
                "decisions": "urn:ev:decisions",
                "maps": "urn:ev:identity",
                "output": "urn:ev:people"
            }
        }
    })
}

fn sample_proposal() -> Value {
    json!({
        "revisions": [{
            "key": "rev",
            "source": "urn:bulletin",
            "sourceKey": "bulletin",
            "content": { "digest": format!("sha256:{}", "0".repeat(64)) }
        }],
        "claims": [{
            "key": "claim",
            "occurrenceKey": "occ-1",
            "revision": { "key": "rev" },
            "proposition": "<urn:release> <urn:affectedBy> <urn:cve>",
            "kind": "reported",
            "polarity": "positive",
            "author": "urn:publisher"
        }]
    })
}

#[test]
fn declared_evidence_settings_file_validation() {
    let ok = |v: Value| Declared::parse(&v.to_string(), Providers::Unchecked).unwrap();
    let bad = |v: Value| Declared::parse(&v.to_string(), Providers::Unchecked).unwrap_err();

    // Valid defaults + per-dataset override + every valid lock shape.
    let d = ok(json!({
        "defaults": {
            "evidence": {
                "decisions": ["urn:ev:decisions"],
                "supports": ["urn:ev:supports"]
            },
            "locked": [
                "evidence.enabled",
                "evidence.sourceRecords",
                "evidence.policies.advisories",
                "evidence.policies.advisories.conflicts",
                "evidence.matchingPolicies.people.output"
            ]
        },
        "datasets": {
            "ev": {
                "evidence": full_settings(),
                "locked": ["evidence"]
            }
        }
    }));
    let r = resolve(&EVIDENCE, &d, "ev", json!({}), Providers::Unchecked);
    assert_eq!(r.status, Ok(()));
    assert_eq!(r.effective["enabled"], true);
    assert_eq!(r.sources[&vec!["enabled".into()]], "locked");

    // Order-insensitive canonical normalization of graph arrays.
    let d_order = ok(json!({
        "datasets": {
            "ev": {
                "evidence": {
                    "enabled": true,
                    "sourceRecords": ["urn:ev:z", "urn:ev:a"],
                    "proposals": ["urn:ev:p"]
                }
            }
        }
    }));
    let r = resolve(&EVIDENCE, &d_order, "ev", json!({}), Providers::Unchecked);
    assert_eq!(r.status, Ok(()));
    assert_eq!(
        r.effective["sourceRecords"],
        json!(["urn:ev:a", "urn:ev:z"])
    );

    // Invalid evidence declarations fail parse.
    assert!(
        bad(json!({"datasets": {"ev": {"evidence": {"enabled": true}}}}))
            .contains("datasets.ev.evidence")
    );
    assert!(
        bad(json!({"datasets": {"ev": {"evidence": {"format": 2}}}}))
            .contains("unsupported-profile")
    );
    assert!(
        bad(json!({"datasets": {"ev": {"evidence": {"profile": 2}}}}))
            .contains("unsupported-profile")
    );
    assert!(
        bad(json!({"datasets": {"ev": {"evidence": {"futureField": 1}}}}))
            .contains("datasets.ev.evidence")
    );
    assert!(
        bad(json!({"datasets": {"ev": {"evidence": {
            "enabled": true,
            "sourceRecords": ["not an iri"],
            "proposals": ["urn:ev:p"]
        }}}}))
        .contains("datasets.ev.evidence")
    );
    assert!(
        bad(json!({"datasets": {"ev": {"evidence": {
            "enabled": true,
            "sourceRecords": ["urn:ev:s"],
            "proposals": ["urn:ev:p"],
            "projections": ["urn:ev:orphan"]
        }}}}))
        .contains("not the output of any policy")
    );
    assert!(
        bad(json!({"datasets": {"ev": {"evidence": {"apiKey": "secret"}}}})).contains("apiKey")
    );

    // Invalid locks fail parse.
    assert!(bad(json!({"defaults": {"locked": ["evidence.unknown"]}})).contains("no member"));
    assert!(
        bad(json!({"defaults": {"locked": ["evidence.enabled.sub"]}})).contains("single field")
    );
    assert!(
        bad(json!({"defaults": {"locked": ["evidence.sourceRecords.0"]}})).contains("single field")
    );
    assert!(
        bad(json!({"defaults": {"locked": ["evidence.policies.BAD_NAME"]}}))
            .contains("not a valid policy name")
    );
    assert!(
        bad(json!({"defaults": {"locked": ["evidence.policies.advisories.bad"]}}))
            .contains("no member")
    );
    assert!(
        bad(json!({"defaults": {"locked": ["evidence.matchingPolicies.people.supports"]}}))
            .contains("no member")
    );
    assert!(
        bad(json!({"defaults": {"locked": ["evidence.policies.advisories.version.extra"]}}))
            .contains("no nested member")
    );
}

#[tokio::test]
async fn declared_evidence_drift_refuses_all_typed_operations_until_reconciled() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    std::fs::write(
        &f,
        json!({
            "datasets": {
                "ev": { "evidence": full_settings() }
            }
        })
        .to_string(),
    )
    .unwrap();
    let st = state_with_settings(&dir.path().join("data"), Some(&f));
    let ds = st.attach("ev", DbType::Persistent, None).unwrap();
    // Insert one ordinary triple so storedFact resolution has a real commit to target.
    ds.store
        .load(&[sparkles::io::Source::from_bytes(
            b"<urn:s> <urn:p> \"ok\" .\n".to_vec(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let head = ds.store.head_commit().seq;
    let id = ds.store.dataset_id();
    let app = crate::http::router(st.clone());

    // GET /$/settings lists "evidence" among dataset kinds.
    let (s, v, _) = send(&app, req("GET", "/$/settings", None)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        v["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "evidence"),
        "{v}"
    );

    // Before reconciliation, GET /$/settings/ev/evidence reports drift: true.
    let (s, v, _) = send(&app, req("GET", "/$/settings/ev/evidence", None)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["kind"], "evidence");
    assert_eq!(v["effective"]["enabled"], true);
    assert_eq!(v["recorded"]["drift"], true);
    assert_eq!(v["recorded"]["digest"], Value::Null);
    assert_eq!(v["sources"]["enabled"], "declared");

    // Ordinary SPARQL update on an ordinary graph still works during drift.
    let (s, _, _) = send(
        &app,
        sparql_update(
            "/ev/update",
            "INSERT DATA { GRAPH <urn:ordinary> { <urn:a> <urn:b> 1 } }",
        ),
    )
    .await;
    assert!(s.is_success(), "{s}");

    // All six typed evidence operations refuse with 409 evidence-settings-drift.
    let resolve_body = json!({
        "references": [{
            "key": "f",
            "storedFact": {
                "datasetId": id,
                "branchId": id,
                "commit": head,
                "triple": "<urn:s> <urn:p> \"ok\""
            }
        }]
    });
    let review_body = json!({
        "policy": "advisories",
        "policyVersion": 1,
        "ifHead": head,
        "decisions": []
    });
    let withdraw_body = json!({ "withdrawals": [] });
    let identities_body = json!({ "policy": "people", "entities": ["urn:e1"] });
    let dependents_body = json!({ "changed": ["urn:c1"] });

    for (path, body) in [
        ("/ev/claims", sample_proposal()),
        ("/ev/claims/resolve", resolve_body.clone()),
        ("/ev/claims/review", review_body),
        ("/ev/claims/withdraw", withdraw_body),
        ("/ev/claims/identities", identities_body.clone()),
        ("/ev/claims/dependents", dependents_body.clone()),
    ] {
        let (s, v, _) = send(&app, req("POST", path, Some(body))).await;
        assert_eq!(s, StatusCode::CONFLICT, "{path}: {v}");
        assert_eq!(v["code"], "evidence-settings-drift", "{path}: {v}");
        // Non-disclosing error message must not leak graph IRIs or policy names.
        let err_msg = v["error"].as_str().unwrap_or("");
        assert!(!err_msg.contains("urn:ev:"), "{path}: {err_msg}");
        assert!(!err_msg.contains("advisories"), "{path}: {err_msg}");
    }

    // Reconcile via PUT /$/evidence/ev/settings (with reversed projection order to check canonicalization).
    let mut put_body = full_settings();
    put_body["projections"] = json!(["urn:ev:people", "urn:ev:accepted"]);
    let (s, v, _) = send(&app, req("PUT", "/$/evidence/ev/settings", Some(put_body))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["revision"], 1);
    assert_eq!(v["managed"].as_array().unwrap().len(), 7);

    let registry = ds.store.managed_snapshot().unwrap().unwrap();
    assert!(
        registry.graphs.owners[&sparkles::managed::OwnerKind::Evidence]
            .evidence_overrides
            .is_none()
    );

    // GET /$/settings/ev/evidence now reports drift: false.
    let (s, v, _) = send(&app, req("GET", "/$/settings/ev/evidence", None)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["recorded"]["drift"], false);
    assert!(v["recorded"]["digest"].is_string());
    assert_eq!(v["runtime"], json!({}));

    // Protected graphs refuse ordinary SPARQL updates.
    let (s, v, _) = send(
        &app,
        sparql_update(
            "/ev/update",
            "INSERT DATA { GRAPH <urn:ev:accepted> { <urn:x> <urn:p> 1 } }",
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "profile-managed-graph");

    // Typed evidence operations now succeed.
    let (s, v, _) = send(&app, req("POST", "/ev/claims", Some(sample_proposal()))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["committed"], true);
    let claim_iri = v["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "Claim")
        .unwrap()["iri"]
        .as_str()
        .unwrap()
        .to_string();
    let claim_digest = v["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "Claim")
        .unwrap()["recordDigest"]
        .as_str()
        .unwrap()
        .to_string();

    let (s, v, _) = send(&app, req("POST", "/ev/claims/resolve", Some(resolve_body))).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let (s, v, _) = send(
        &app,
        req("POST", "/ev/claims/identities", Some(identities_body)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let (s, v, _) = send(
        &app,
        req("POST", "/ev/claims/dependents", Some(dependents_body)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let head = ds.store.head_commit().seq;
    let (s, v, _) = send(
        &app,
        req(
            "POST",
            "/ev/claims/review",
            Some(json!({
                "policy": "advisories",
                "policyVersion": 1,
                "ifHead": head,
                "decisions": [{
                    "key": "d1",
                    "target": claim_iri,
                    "targetDigest": claim_digest,
                    "disposition": "accepted"
                }]
            })),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["committed"], true);

    let (s, v, _) = send(
        &app,
        req(
            "POST",
            "/ev/claims/withdraw",
            Some(json!({
                "withdrawals": [{
                    "key": "w1",
                    "target": claim_iri,
                    "targetDigest": claim_digest
                }]
            })),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["committed"], true);
}

#[tokio::test]
async fn locks_prevent_conflicting_mutations_and_allow_unlocked_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    std::fs::write(
        &f,
        json!({
            "datasets": {
                "ev": {
                    "evidence": full_settings(),
                    "locked": [
                        "evidence.enabled",
                        "evidence.sourceRecords",
                        "evidence.policies.advisories.conflicts"
                    ]
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let st = state_with_settings(&dir.path().join("data"), Some(&f));
    st.attach("ev", DbType::Persistent, None).unwrap();
    let app = crate::http::router(st.clone());

    // Reconcile to declared settings.
    let (s, v, _) = send(
        &app,
        req("PUT", "/$/evidence/ev/settings", Some(full_settings())),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // PUT /$/evidence/ev/settings changing locked sourceRecords -> 409 locked-by-config.
    let mut bad_src = full_settings();
    bad_src["sourceRecords"] = json!(["urn:ev:sources", "urn:ev:extra"]);
    let (s, v, _) = send(&app, req("PUT", "/$/evidence/ev/settings", Some(bad_src))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "locked-by-config");
    assert_eq!(v["fields"], json!(["sourceRecords"]));

    // PUT /$/evidence/ev/settings changing locked policy field -> 409 locked-by-config.
    let mut bad_pol = full_settings();
    bad_pol["policies"]["advisories"]["conflicts"] = json!("admitLabelled");
    let (s, v, _) = send(&app, req("PUT", "/$/evidence/ev/settings", Some(bad_pol))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "locked-by-config");
    assert_eq!(v["fields"], json!(["policies.advisories.conflicts"]));

    // PATCH /$/settings/ev/evidence changing locked enabled -> 409 locked-by-config.
    let (s, v, _) = send(
        &app,
        req(
            "PATCH",
            "/$/settings/ev/evidence",
            Some(json!({"enabled": false})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "locked-by-config");
    assert_eq!(v["fields"], json!(["enabled"]));

    // POST /$/evidence/ev/detach on a graph required by locked sourceRecords (with enabled locked true) -> 409 locked-by-config.
    let (s, v, _) = send(
        &app,
        req(
            "POST",
            "/$/evidence/ev/detach",
            Some(json!({"graph": "urn:ev:sources"})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "locked-by-config");
    assert_eq!(v["fields"], json!(["sourceRecords"]));

    // Unlocked mutation via PUT /$/evidence/ev/settings succeeds and keeps drift == false.
    let mut extra_prop = full_settings();
    extra_prop["proposals"] = json!(["urn:ev:claims", "urn:ev:claims2"]);
    let (s, v, _) = send(
        &app,
        req("PUT", "/$/evidence/ev/settings", Some(extra_prop)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        v["managed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g == "urn:ev:claims2")
    );

    let (s, v, etag) = send(&app, req("GET", "/$/settings/ev/evidence", None)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let etag = etag.unwrap();
    assert_eq!(v["recorded"]["drift"], false);
    assert_eq!(v["sources"]["proposals"], "runtime");
    assert_eq!(v["sources"]["sourceRecords"], "locked");
    assert_eq!(v["overrides"].as_array().unwrap().len(), 1);
    assert_eq!(v["overrides"][0]["path"], "proposals");

    // Direct write to newly added proposal graph is refused as profile-managed-graph.
    let (s, v, _) = send(
        &app,
        sparql_update(
            "/ev/update",
            "INSERT DATA { GRAPH <urn:ev:claims2> { <urn:x> <urn:p> 1 } }",
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "profile-managed-graph");

    // Typed proposal succeeds with the unlocked override in place.
    let mut prop = sample_proposal();
    prop["proposalGraph"] = json!("urn:ev:claims2");
    let (s, v, _) = send(&app, req("POST", "/ev/claims", Some(prop))).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // Stale If-Match on PATCH /$/settings/ev/evidence returns 412 precondition-failed.
    let (s, v, _) = send(
        &app,
        req_if_match(
            "PATCH",
            "/$/settings/ev/evidence",
            "\"stale\"",
            Some(json!({"identity": ["urn:ev:identity", "urn:ev:identity2"]})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED, "{v}");
    assert_eq!(v["code"], "precondition-failed");

    // Matching If-Match PATCH succeeds and protects the added identity graph.
    let (s, v, _) = send(
        &app,
        req_if_match(
            "PATCH",
            "/$/settings/ev/evidence",
            &etag,
            Some(json!({"identity": ["urn:ev:identity", "urn:ev:identity2"]})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["recorded"]["drift"], false);
    assert!(
        v["recorded"]["managed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g == "urn:ev:identity2")
    );
}

#[tokio::test]
async fn reload_drift_preserves_protection_and_reconciles_after_explicit_detach() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    let initial = json!({
        "datasets": {
            "ev": {
                "evidence": {
                    "enabled": true,
                    "sourceRecords": ["urn:ev:s1", "urn:ev:s2"],
                    "proposals": ["urn:ev:p1"]
                },
                "locked": ["evidence"]
            }
        }
    });
    std::fs::write(&f, initial.to_string()).unwrap();
    let st = state_with_settings(&dir.path().join("data"), Some(&f));
    let ds = st.attach("ev", DbType::Persistent, None).unwrap();
    let app = crate::http::router(st.clone());

    // Reconcile fresh dataset via DELETE /$/settings/ev/evidence (resets to declared and records).
    let (s, v, _) = send(&app, req("DELETE", "/$/settings/ev/evidence", None)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["recorded"]["drift"], false);
    assert_eq!(
        v["recorded"]["managed"],
        json!(["urn:ev:p1", "urn:ev:s1", "urn:ev:s2"])
    );

    // Whole-kind lock refuses detaching urn:ev:s2 while declared still requires it.
    let (s, v, _) = send(
        &app,
        req(
            "POST",
            "/$/evidence/ev/detach",
            Some(json!({"graph": "urn:ev:s2"})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "locked-by-config");
    assert_eq!(v["fields"], json!([""]));

    let fence = evidence::DriftFence::capture(&st, &ds).unwrap();
    // Operator removes urn:ev:s2 from declared settings and reloads.
    let narrowed = json!({
        "datasets": {
            "ev": {
                "evidence": {
                    "enabled": true,
                    "sourceRecords": ["urn:ev:s1"],
                    "proposals": ["urn:ev:p1"]
                },
                "locked": ["evidence"]
            }
        }
    });
    std::fs::write(&f, narrowed.to_string()).unwrap();
    st.settings.reload(None).unwrap();

    {
        let _guard = st.settings.hold_evidence_declarations();
        assert!(matches!(
            fence.check(&st),
            Err(sparkles::Error::Managed(e)) if e.code == "evidence-settings-drift"
        ));
    }

    // Drift is detected and typed operations are refused.
    let (s, v, _) = send(&app, req("GET", "/$/settings/ev/evidence", None)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["recorded"]["drift"], true);
    let (s, v, _) = send(&app, req("POST", "/ev/claims", Some(sample_proposal()))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "evidence-settings-drift");

    // Protection is NOT weakened by the reload: urn:ev:s2 still refuses ordinary SPARQL updates.
    let (s, v, _) = send(
        &app,
        sparql_update(
            "/ev/update",
            "INSERT DATA { GRAPH <urn:ev:s2> { <urn:x> <urn:p> 1 } }",
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "profile-managed-graph");

    // Reconciling without detaching urn:ev:s2 first is refused (no silent weakening).
    let narrowed_body = json!({
        "format": 1,
        "profile": 1,
        "enabled": true,
        "sourceRecords": ["urn:ev:s1"],
        "proposals": ["urn:ev:p1"]
    });
    let (s, v, _) = send(
        &app,
        req(
            "PUT",
            "/$/evidence/ev/settings",
            Some(narrowed_body.clone()),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("detach it first"),
        "{v}"
    );
    let (s, v, _) = send(&app, req("DELETE", "/$/settings/ev/evidence", None)).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("detach it first"),
        "{v}"
    );

    // Detaching urn:ev:s1 (still required by locked settings) is STILL refused.
    let (s, v, _) = send(
        &app,
        req(
            "POST",
            "/$/evidence/ev/detach",
            Some(json!({"graph": "urn:ev:s1"})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "locked-by-config");

    // Detaching urn:ev:s2 (no longer in declared settings) succeeds, then PUT reconciles.
    let (s, v, _) = send(
        &app,
        req(
            "POST",
            "/$/evidence/ev/detach",
            Some(json!({"graph": "urn:ev:s2"})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["detached"], json!(["urn:ev:s2"]));

    let (s, v, _) = send(
        &app,
        req("PUT", "/$/evidence/ev/settings", Some(narrowed_body)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["managed"], json!(["urn:ev:p1", "urn:ev:s1"]));

    // Drift is cleared and typed operations resume.
    let (s, v, _) = send(&app, req("GET", "/$/settings/ev/evidence", None)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["recorded"]["drift"], false);
    let (s, v, _) = send(&app, req("POST", "/ev/claims", Some(sample_proposal()))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["committed"], true);
}

#[tokio::test]
async fn unlocked_reload_and_rust_api_divergence_cause_drift_including_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    let v1 = json!({
        "datasets": {
            "mem": {
                "evidence": {
                    "enabled": true,
                    "sourceRecords": ["urn:ev:s1"],
                    "proposals": ["urn:ev:p1"]
                }
            }
        }
    });
    std::fs::write(&f, v1.to_string()).unwrap();
    let st = state_with_settings(&dir.path().join("data"), Some(&f));
    let ds = st.attach("mem", DbType::Mem, None).unwrap();
    let app = crate::http::router(st.clone());

    // Initial drift on an in-memory dataset.
    let (s, v, _) = send(&app, req("POST", "/mem/claims", Some(sample_proposal()))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "evidence-settings-drift");

    // Reconcile via PUT /$/settings/mem/evidence (omitting format/profile to verify C19 defaults).
    let (s, v, _) = send(
        &app,
        req(
            "PUT",
            "/$/settings/mem/evidence",
            Some(json!({
                "enabled": true,
                "sourceRecords": ["urn:ev:s1"],
                "proposals": ["urn:ev:p1"]
            })),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["recorded"]["drift"], false);

    let (s, v, _) = send(&app, req("POST", "/mem/claims", Some(sample_proposal()))).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // Unlocked declared change on reload causes drift (no spurious runtime override).
    let v2 = json!({
        "datasets": {
            "mem": {
                "evidence": {
                    "enabled": true,
                    "sourceRecords": ["urn:ev:s1", "urn:ev:s2"],
                    "proposals": ["urn:ev:p1"]
                }
            }
        }
    });
    std::fs::write(&f, v2.to_string()).unwrap();
    st.settings.reload(None).unwrap();

    let (s, v, _) = send(&app, req("POST", "/mem/claims", Some(sample_proposal()))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "evidence-settings-drift");

    // Reconcile to v2.
    let (s, v, _) = send(&app, req("DELETE", "/$/settings/mem/evidence", None)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["recorded"]["drift"], false);

    // Out-of-band Rust API mutation diverging from declared settings also triggers drift.
    let diverged = sparkles::evidence::Settings {
        enabled: true,
        source_records: vec!["urn:ev:s1".into(), "urn:ev:s2".into()],
        proposals: vec!["urn:ev:p1".into(), "urn:ev:p2".into()],
        ..Default::default()
    };
    sparkles::evidence::configure(&ds.dataset, &diverged, &sparkles::task::Control::none())
        .unwrap();
    let (s, v, _) = send(&app, req("POST", "/mem/claims", Some(sample_proposal()))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "evidence-settings-drift");
}

fn declared_evidence(f: &std::path::Path, evidence: Value, locked: &[&str]) {
    let settings = json!({"datasets":{"ev":{"evidence":evidence,"locked":locked}}});
    std::fs::write(f, settings.to_string()).unwrap();
}

#[tokio::test]
async fn invalid_runtime_can_detach_a_retired_locked_graph() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    let mut initial = full_settings();
    initial["sourceRecords"] = json!(["urn:ev:sources", "urn:ev:retired"]);
    let locks = ["evidence.enabled", "evidence.sourceRecords"];
    declared_evidence(&f, initial.clone(), &locks);
    let st = state_with_settings(&dir.path().join("data"), Some(&f));
    st.attach("ev", DbType::Memory, None).unwrap();
    let app = crate::http::router(st.clone());
    let put = "/$/settings/ev/evidence";
    let (s, v, _) = send(&app, req("DELETE", put, None)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let patch = json!({"projections":["urn:ev:accepted", "urn:ev:people"]});
    let (s, v, _) = send(&app, req("PATCH", put, Some(patch))).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let mut narrowed = full_settings();
    narrowed["projections"] = json!(["urn:ev:accepted", "urn:ev:people", "urn:ev:second"]);
    narrowed["policies"]["second"] = narrowed["policies"]["advisories"].clone();
    narrowed["policies"]["second"]["output"] = json!("urn:ev:second");
    declared_evidence(&f, narrowed, &locks);
    st.settings.reload(None).unwrap();
    let (_, v, _) = send(&app, req("GET", put, None)).await;
    assert_eq!(v["status"]["valid"], false, "{v}");

    let detach = "/$/evidence/ev/detach";
    let (_, v, _) = send(
        &app,
        req("POST", detach, Some(json!({"graph":"urn:ev:sources"}))),
    )
    .await;
    assert_eq!(v["code"], "locked-by-config", "{v}");
    let (s, v, _) = send(
        &app,
        req("POST", detach, Some(json!({"graph":"urn:ev:retired"}))),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v, _) = send(&app, req("DELETE", put, None)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["recorded"]["drift"], false, "{v}");
}

#[tokio::test]
async fn disabled_declared_template_needs_no_recording() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    std::fs::write(&f, json!({
        "defaults":{"evidence":{"decisions":["urn:ev:decisions"]},"locked":["evidence.decisions"]},
        "datasets":{"ev":{"evidence":{"supports":["urn:ev:supports"]}}}
    }).to_string()).unwrap();
    let st = state_with_settings(&dir.path().join("data"), Some(&f));
    for name in ["ev", "other"] {
        st.attach(name, DbType::Memory, None).unwrap();
    }
    let app = crate::http::router(st);
    for name in ["ev", "other"] {
        let path = format!("/$/settings/{name}/evidence");
        for method in ["GET", "DELETE"] {
            let (s, v, _) = send(&app, req(method, &path, None)).await;
            assert_eq!(s, StatusCode::OK, "{v}");
            assert_eq!(v["recorded"]["drift"], false, "{v}");
            assert!(v["recorded"]["digest"].is_null(), "{v}");
            assert_eq!(v["recorded"]["managed"], json!([]));
        }
    }
}

#[tokio::test]
async fn policy_null_resets_to_declaration() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    let declared = full_settings();
    declared_evidence(&f, declared, &[]);
    let st = state_with_settings(&dir.path().join("data"), Some(&f));
    st.attach("ev", DbType::Memory, None).unwrap();
    let app = crate::http::router(st);
    let path = "/$/settings/ev/evidence";
    let patch = json!({"policies":{"advisories":{"admittedKinds":["reported", "observation"]}}});
    let (s, v, _) = send(&app, req("PATCH", path, Some(patch))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let patch = json!({"policies":{"advisories":null}});
    let (s, v, _) = send(&app, req("PATCH", path, Some(patch))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["effective"]["policies"]["advisories"]["admittedKinds"],
        json!(["reported"])
    );
    assert_eq!(v["runtime"], json!({}));
    assert_eq!(v["recorded"]["drift"], false);
}

#[tokio::test]
async fn version_and_nested_policy_lock_refusals() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    let mut declared = full_settings();
    declared_evidence(
        &f,
        declared.clone(),
        &["evidence.policies.advisories.conflicts"],
    );
    let st = state_with_settings(&dir.path().join("data"), Some(&f));
    st.attach("ev", DbType::Memory, None).unwrap();
    let app = crate::http::router(st);
    let path = "/$/settings/ev/evidence";
    for field in ["format", "profile"] {
        for method in ["PUT", "PATCH"] {
            let mut body = full_settings();
            body[field] = json!(2);
            let (s, v, _) = send(&app, req(method, path, Some(body))).await;
            assert_eq!(s, StatusCode::CONFLICT, "{method} {v}");
            assert_eq!(v["code"], "unsupported-profile", "{method} {v}");
        }
    }
    declared["policies"]
        .as_object_mut()
        .unwrap()
        .remove("advisories");
    declared["projections"] = json!(["urn:ev:people"]);
    let (s, v, _) = send(&app, req("PUT", path, Some(declared))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "locked-by-config", "{v}");
    assert_eq!(v["fields"], json!(["policies.advisories.conflicts"]));
    let mut same = full_settings();
    same["policies"]["advisories"]
        .as_object_mut()
        .unwrap()
        .remove("conflicts");
    let (s, v, _) = send(&app, req("PUT", path, Some(same))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn restored_evidence_overrides_hold_for_both_capture_paths() {
    for kind in [DbType::Persistent, DbType::Memory] {
        restore_with_overrides(kind).await;
    }
}

async fn restore_with_overrides(kind: DbType) {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    declared_evidence(&f, full_settings(), &[]);
    let st = state_with_settings(&dir.path().join("data"), Some(&f));
    let ds = st.attach("ev", kind, None).unwrap();
    let app = crate::http::router(st);
    let path = "/$/settings/ev/evidence";
    let patch = json!({"proposals":["urn:ev:claims","urn:ev:extra"]});
    let (s, v, _) = send(&app, req("PATCH", path, Some(patch))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let restored = dir.path().join("restore");
    let capture = if kind == DbType::Memory {
        ds.store.memory_backup_capture(
            "evidence-overrides",
            &sparkles::store::MemoryCaptureOptions {
                tmp_dir: dir.path().join("scratch"),
                ..Default::default()
            },
        )
    } else {
        ds.store.backup_capture("evidence-overrides")
    };
    capture.unwrap().write_to(&restored).unwrap();

    let st = state_with_settings(&dir.path().join("restored-data"), Some(&f));
    st.attach("ev", DbType::Persistent, Some(&restored))
        .unwrap();
    let app = crate::http::router(st);
    let (s, v, _) = send(&app, req("GET", path, None)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["recorded"]["drift"], false, "{v}");
    assert_eq!(v["effective"], v["recorded"]["settings"]);
    assert_eq!(
        v["runtime"]["proposals"],
        json!(["urn:ev:claims", "urn:ev:extra"])
    );
    let (s, v, _) = send(
        &app,
        sparql_update(
            "/ev/update",
            "INSERT DATA { GRAPH <urn:ev:extra> { <urn:x> <urn:p> 1 } }",
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "profile-managed-graph");
    let mut proposal = sample_proposal();
    proposal["proposalGraph"] = json!("urn:ev:extra");
    let (s, v, _) = send(&app, req("POST", "/ev/claims", Some(proposal))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["committed"], true);
}

#[tokio::test]
async fn interruption_after_registry_commit_and_reset_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    declared_evidence(&f, full_settings(), &[]);
    let data = dir.path().join("data");
    {
        let st = state_with_settings(&data, Some(&f));
        let ds = st.attach("ev", DbType::Persistent, None).unwrap();
        let app = crate::http::router(st.clone());
        let (s, v, _) = send(&app, req("DELETE", "/$/settings/ev/evidence", None)).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        ds.store.set_failpoint(
            "managed-published",
            Some(Arc::new(|_| {
                panic!("interrupt after the durable registry commit")
            })),
        );
        let mut changed = full_settings();
        changed["policies"]["advisories"]["admittedKinds"] = json!(["reported", "observation"]);
        let (s, v, _) = send(&app, req("PUT", "/$/evidence/ev/settings", Some(changed))).await;
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{v}");
    }

    for reset in [true, false] {
        let st = state_with_settings(&data, Some(&f));
        st.attach("ev", DbType::Persistent, None).unwrap();
        let app = crate::http::router(st);
        let path = "/$/settings/ev/evidence";
        let (_, v, _) = send(&app, req("GET", path, None)).await;
        assert_eq!(v["recorded"]["drift"], false, "{v}");
        assert_eq!(v["effective"], v["recorded"]["settings"]);
        assert_eq!(v["recorded"]["managed"].as_array().unwrap().len(), 7);
        if reset {
            assert_ne!(v["runtime"], json!({}), "{v}");
            let (s, v, _) = send(&app, req("DELETE", path, None)).await;
            assert_eq!(s, StatusCode::OK, "{v}");
        } else {
            assert_eq!(v["runtime"], json!({}), "{v}");
        }
    }
}

#[tokio::test]
async fn fenced_proposal_refuses_recording_after_admission() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    declared_evidence(&f, full_settings(), &[]);
    let st = state_with_settings(&dir.path().join("data"), Some(&f));
    let ds = st.attach("ev", DbType::Memory, None).unwrap();
    let app = crate::http::router(st.clone());
    let (s, v, _) = send(&app, req("DELETE", "/$/settings/ev/evidence", None)).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let mut changed = full_settings();
    changed["proposals"] = json!(["urn:ev:claims", "urn:ev:extra"]);
    let dataset = ds.dataset.clone();
    ds.store.set_failpoint(
        "evidence-admitted",
        Some(Arc::new(move |_| {
            let control = sparkles::task::Control::none();
            let settings =
                sparkles::evidence::parse_settings(changed.to_string().as_bytes(), &control)
                    .unwrap();
            sparkles::evidence::configure(&dataset, &settings, &control).unwrap();
        })),
    );
    let head = ds.store.head_commit().seq;
    let mut proposal = sample_proposal();
    proposal["proposalGraph"] = json!("urn:ev:extra");
    let (s, v, _) = send(&app, req("POST", "/ev/claims", Some(proposal))).await;
    ds.store.set_failpoint("evidence-admitted", None);
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "evidence-settings-drift", "{v}");
    assert_eq!(ds.store.head_commit().seq, head);
}

#[tokio::test]
async fn the_override_envelope_has_its_own_version() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    declared_evidence(&f, full_settings(), &[]);
    let data = dir.path().join("data");
    let root = {
        let st = state_with_settings(&data, Some(&f));
        let ds = st.attach("ev", DbType::Persistent, None).unwrap();
        let app = crate::http::router(st);
        let (s, v, _) = send(
            &app,
            req(
                "PATCH",
                "/$/settings/ev/evidence",
                Some(json!({"format":1})),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["runtime"], json!({"format":1}));
        ds.store.root().unwrap().to_path_buf()
    };

    let path = root.join(sparkles::managed::FILE);
    let mut registry: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let envelope = &mut registry["owners"]["evidence"]["evidenceOverrides"];
    assert_eq!(*envelope, json!({"overridesFormat":1,"patch":{"format":1}}));
    envelope["overridesFormat"] = json!(2);
    std::fs::write(&path, registry.to_string()).unwrap();
    let st = state_with_settings(&data, Some(&f));
    st.attach("ev", DbType::Persistent, None).unwrap();
    let app = crate::http::router(st);
    let (s, v, _) = send(&app, req("GET", "/$/settings/ev/evidence", None)).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["code"], "managed-unavailable");
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("evidence overrides format 2")
    );
}
