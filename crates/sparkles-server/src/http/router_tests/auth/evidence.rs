//! Evidence resolution does not disclose datasets, branches or facts a caller cannot
//! read: an unreadable reference answers exactly as a missing one.
use super::*;

async fn resolve(s: &AuthServer, ds: &str, user: &str, references: serde_json::Value) -> R {
    call(
        &s.app,
        "POST",
        &format!("/{ds}/claims/resolve"),
        &[
            ("authorization", &b(user)),
            ("content-type", "application/json"),
        ],
        &serde_json::json!({ "references": references }).to_string(),
    )
    .await
}

fn reference(st: &AppState, ds: &str, triple: &str) -> serde_json::Value {
    let d = st.get(ds).unwrap();
    serde_json::json!({
        "key": "r",
        "storedFact": {
            "datasetId": d.store.dataset_id(),
            "branchId": d.store.dataset_id(),
            "commit": d.store.head_commit().seq,
            "triple": triple,
        },
    })
}

#[tokio::test]
async fn unreadable_references_answer_as_missing_ones() {
    let s = auth_server();
    let team = "<http://ex.org/team-a> <http://ex.org/p> \"1\"";
    let secret = "<http://ex.org/secret> <http://ex.org/p> \"1\"";
    // bob reads team-a from a dataset he writes
    let r = resolve(
        &s,
        "wiki",
        "bob",
        serde_json::json!([reference(&s.state, "team-a", team)]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(v["resolved"][0]["key"], "r");
    // the secret dataset exists and holds the fact, but bob cannot read it
    let hidden = resolve(
        &s,
        "wiki",
        "bob",
        serde_json::json!([reference(&s.state, "secret", secret)]),
    )
    .await;
    assert_eq!(
        hidden.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        hidden.text()
    );
    let mut missing = reference(&s.state, "secret", secret);
    missing["storedFact"]["datasetId"] = serde_json::json!(uuid::Uuid::new_v4());
    let missing = resolve(&s, "wiki", "bob", serde_json::json!([missing])).await;
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(hidden.body, missing.body);
    let v: serde_json::Value = serde_json::from_slice(&missing.body).unwrap();
    assert_eq!(v["code"], "evidence-unavailable");
    assert_eq!(v["reference"], "r");
    // a branch identity the dataset does not list answers the same
    let mut branch = reference(&s.state, "team-a", team);
    branch["storedFact"]["branchId"] = serde_json::json!(uuid::Uuid::new_v4());
    let branch = resolve(&s, "wiki", "bob", serde_json::json!([branch])).await;
    assert_eq!(branch.body, missing.body);
    // alice, a server administrator, reads the secret dataset
    let r = resolve(
        &s,
        "wiki",
        "alice",
        serde_json::json!([reference(&s.state, "secret", secret)]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // the route itself needs read on its dataset
    let r = resolve(&s, "secret", "bob", serde_json::json!([])).await;
    assert!(
        matches!(r.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{}",
        r.status
    );
}

#[tokio::test]
async fn anchors_and_graph_grants_do_not_disclose_hidden_data() {
    let s = build(Fixture {
        extra: super::graphs::users(),
        ..Default::default()
    });
    let ds = s.state.attach("graphs", DbType::Persistent, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            super::graphs::DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let commit = ds.store.head_commit().seq;
    let id = ds.store.dataset_id();
    let fact = |key: &str, graph: &str, triple: &str| {
        serde_json::json!({ "key": key, "storedFact": {
            "datasetId": id, "branchId": id, "commit": commit,
            "graph": graph, "triple": triple,
        }})
    };
    // gra reads http://ex/a/*: two references to one commit share its view
    let r = resolve(
        &s,
        "graphs",
        "gra",
        serde_json::json!([
            fact(
                "a1",
                "http://ex/a/1",
                "<http://ex/a1> <http://ex/p> \"alpha fox\""
            ),
            fact(
                "a2",
                "http://ex/a/2",
                "<http://ex/a2> <http://ex/p> \"alpha two\""
            ),
        ]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // a fact in an unreadable graph answers as a missing one
    let hidden = resolve(
        &s,
        "graphs",
        "gra",
        serde_json::json!([fact(
            "r",
            "http://ex/b/1",
            "<http://ex/b1> <http://ex/p> \"secret fox\""
        )]),
    )
    .await;
    let missing = resolve(
        &s,
        "graphs",
        "gra",
        serde_json::json!([fact(
            "r",
            "http://ex/b/1",
            "<http://ex/b1> <http://ex/p> \"no fox\""
        )]),
    )
    .await;
    assert_eq!(
        hidden.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        hidden.text()
    );
    assert_eq!(hidden.body, missing.body);
    // an unanchored blank node answers the same for an unreadable and a missing dataset
    let secret = s.state.get("secret").unwrap().store.dataset_id();
    let blank = |dataset: uuid::Uuid| {
        serde_json::json!([{ "key": "b", "storedFact": {
            "datasetId": dataset, "branchId": dataset, "commit": 1,
            "triple": "_:x <http://ex.org/p> \"1\"",
        }}])
    };
    let unreadable = resolve(&s, "wiki", "bob", blank(secret)).await;
    let absent = resolve(&s, "wiki", "bob", blank(uuid::Uuid::new_v4())).await;
    assert_eq!(unreadable.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(unreadable.body, absent.body);
}

// ------------------------------------------------------------------ proposals ------

/// A server with a persistent dataset `ev` whose evidence profile is configured, a
/// writer of all of it (`evw`) and a writer of its claims graph alone (`evp`).
fn proposal_server() -> AuthServer {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    let s = build(Fixture {
        extra: format!(
            r#"
[[users]]
name = "evw"
password = "{evw}"
datasets = {{ ev = "write" }}

[[users]]
name = "evp"
password = "{evp}"
[[users.grants]]
dataset = "ev"
level = "write"
graphs = ["urn:ev:claims"]
"#,
            evw = h("evw-pw"),
            evp = h("evp-pw"),
        ),
        ..Default::default()
    });
    let ds = s.state.attach("ev", DbType::Persistent, None).unwrap();
    let settings = sparkles::evidence::Settings {
        enabled: true,
        source_records: vec!["urn:ev:sources".into()],
        proposals: vec!["urn:ev:claims".into()],
        ..Default::default()
    };
    sparkles::evidence::configure(&ds.dataset, &settings, &sparkles::task::Control::none())
        .unwrap();
    s
}

async fn propose(s: &AuthServer, creds: &str, body: &serde_json::Value) -> R {
    call(
        &s.app,
        "POST",
        "/ev/claims",
        &[
            ("authorization", creds),
            ("content-type", "application/json"),
        ],
        &body.to_string(),
    )
    .await
}

fn revision_and_claim(occurrence: &str) -> serde_json::Value {
    serde_json::json!({
        "revisions": [{"key": "rev", "source": "urn:bulletin", "sourceKey": "bulletin",
                       "content": {"digest": format!("sha256:{}", "0".repeat(64))}}],
        "claims": [claim(occurrence, serde_json::json!({"key": "rev"}))],
    })
}

fn claim(occurrence: &str, revision: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"key": "claim", "occurrenceKey": occurrence, "revision": revision,
        "proposition": "<urn:release> <urn:affectedBy> <urn:cve>",
        "kind": "reported", "polarity": "positive", "author": "urn:publisher"})
}

#[tokio::test]
async fn proposals_need_update_grants_on_every_written_graph() {
    let s = proposal_server();
    let head = s.state.get("ev").unwrap().store.head_commit().seq;
    // evp may write claims but not the source records the batch also writes
    let r = propose(&s, &b("evp"), &revision_and_claim("c1")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(s.state.get("ev").unwrap().store.head_commit().seq, head);
    // evw may write both, and records the authenticated principal
    let r = propose(&s, &b("evw"), &revision_and_claim("c1")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let ds = s.state.get("ev").unwrap();
    assert!(
        ds.dataset
            .ask(&format!(
                "ASK {{ GRAPH <urn:ev:claims> {{ <{}> <http://www.w3.org/ns/prov#wasAssociatedWith> <urn:x-sparkles:principal:user:evw> }} }}",
                r.json()["operation"].as_str().unwrap()
            ))
            .unwrap()
    );
    // a reader without write is refused by the route
    let r = propose(&s, &b("bob"), &revision_and_claim("c2")).await;
    assert!(
        matches!(r.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{}",
        r.status
    );
}

#[tokio::test]
async fn a_proposal_does_not_disclose_records_its_caller_cannot_read() {
    let s = proposal_server();
    let r = propose(&s, &b("evw"), &revision_and_claim("c1")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rev = r.json()["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "rev")
        .unwrap()["iri"]
        .as_str()
        .unwrap()
        .to_string();
    // evp cannot read the source-record graph that holds the revision
    let only_claim = |revision: &str| serde_json::json!({ "claims": [claim("c9", serde_json::json!({"iri": revision}))] });
    let missing_iri = "urn:uuid:00000000-0000-4000-8000-000000000000";
    let hidden = propose(&s, &b("evp"), &only_claim(&rev)).await;
    let missing = propose(&s, &b("evp"), &only_claim(missing_iri)).await;
    assert_eq!(
        hidden.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        hidden.text()
    );
    assert_eq!(hidden.json()["code"], "evidence-unavailable");
    // the answers are equal byte for byte apart from the IRI and the request id
    let scrub = |r: &R, iri: &str| {
        let mut v = r.json();
        v.as_object_mut().unwrap().remove("requestId");
        serde_json::to_string(&v).unwrap().replace(iri, "IRI")
    };
    assert_eq!(scrub(&hidden, &rev), scrub(&missing, missing_iri));
    // evw reads it, so the same claim publishes
    let r = propose(&s, &b("evw"), &only_claim(&rev)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

#[tokio::test]
async fn a_token_revoked_before_publication_writes_nothing() {
    let s = proposal_server();
    let (cookie, csrf) = password_session(&s, "evw").await;
    let r = super::tokens::mint_as(
        &s.app,
        &[("cookie", &cookie), ("x-sparkles-csrf", &csrf)],
        r#"{"name":"importer","datasets":{"ev":"write"},"expiresIn":"7d"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let token = r.json()["token"].as_str().unwrap().to_string();
    let id = r.json()["id"].as_str().unwrap().to_string();
    let ds = s.state.get("ev").unwrap();
    // a fact the proposal references, so that it holds a commit before it writes
    ds.dataset
        .update("INSERT DATA { GRAPH <urn:g> { <urn:a> <urn:p> <urn:o> } }")
        .unwrap();
    let mut body = revision_and_claim("c1");
    body["references"] = serde_json::json!([{"key": "f", "occurrenceKey": "f", "storedFact": {
        "datasetId": ds.store.dataset_id(), "branchId": ds.store.dataset_id(),
        "commit": ds.store.head_commit().seq, "graph": "urn:g",
        "triple": "<urn:a> <urn:p> <urn:o>"}}]);
    // the token is revoked after the request was authorized and its hold acquired
    let auth = s.auth();
    let revoked = id.clone();
    ds.store.set_failpoint(
        "evidence-held",
        Some(Arc::new(move |_| {
            assert_eq!(
                auth.tokens.remove(std::slice::from_ref(&revoked)).unwrap(),
                1
            );
        })),
    );
    let head = ds.store.head_commit().seq;
    let r = propose(&s, &bearer(&token), &body).await;
    ds.store.set_failpoint("evidence-held", None);
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.json()["code"], "authority-changed");
    assert_eq!(ds.store.head_commit().seq, head, "nothing was written");
    // the orphaned hold reconciles once its grace has passed
    assert_eq!(
        sparkles::evidence::reconcile_operation_holds(&ds.dataset, 0)
            .unwrap()
            .len(),
        1
    );
    // the revoked token is refused at authentication from now on
    let r = propose(&s, &bearer(&token), &body).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
}

/// A claims ingestion (spec C25, "Ingestion into claim proposals") needs `write` on the
/// update endpoint and update grants on the evidence graphs it writes, and a caller
/// without them is refused before any model call.
#[tokio::test]
async fn a_claims_ingestion_needs_evidence_write_before_any_model_call() {
    use crate::models::mock::{self, MockModel};
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    let s = build(Fixture {
        extra: format!(
            r#"
[[users]]
name = "gsrc"
password = "{gsrc}"
[[users.grants]]
dataset = "wiki"
level = "write"
graphs = ["http://ex.org/src/*"]
"#,
            gsrc = h("gsrc-pw")
        ),
        ..Default::default()
    });
    let m = MockModel::start(|_, _| (200, mock::openai(r#"{"mentions":[],"facts":[]}"#)));
    let cfg = serde_json::json!({ "providers": { "cheap": {
            "kind": "openai", "endpoint": m.url(), "requestTimeoutSecs": 5,
            "allowedModels": ["m"], "structuredOutput": "json-schema" } },
        "roles": { "extract": [{ "provider": "cheap", "model": "m" }] } });
    s.state
        .models
        .store(Some(Arc::new(crate::models::Models::new(
            crate::models::ModelsConfig::parse(&cfg.to_string()).unwrap(),
            Default::default(),
            sparkles::outbound::OutboundPolicy {
                allow_private: true,
                ..Default::default()
            },
        ))));
    let json = |user: &str| {
        [
            ("authorization", b(user)),
            ("content-type", "application/json".to_string()),
        ]
    };
    let put = |path: String, body: serde_json::Value| {
        let app = s.app.clone();
        async move {
            let headers = json("alice");
            let hs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            call(&app, "PUT", &path, &hs, &body.to_string()).await
        }
    };
    for ds in ["wiki", "team-a"] {
        let r = put(
            format!("/$/assistant/{ds}"),
            serde_json::json!({ "enabled": true, "ingest": true, "send": "documents" }),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let r = put(
            format!("/$/evidence/{ds}/settings"),
            serde_json::json!({"format": 1, "profile": 1, "enabled": true,
                "sourceRecords": ["urn:ev:sources"], "proposals": ["urn:ev:claims"]}),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let ingest = |user: &'static str, ds: &'static str, graph: &'static str| {
        let app = s.app.clone();
        async move {
            let headers = json(user);
            let hs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let body = serde_json::json!({ "text": "# Notes\n\nThe wiki has one page.\n",
                "format": "text/markdown", "mode": "claims", "graph": graph });
            let r = call(
                &app,
                "POST",
                &format!("/$/ingest/{ds}"),
                &hs,
                &body.to_string(),
            )
            .await;
            if r.status != StatusCode::ACCEPTED {
                return r.json();
            }
            let id = r.json()["id"].as_str().unwrap().to_string();
            let w = call(
                &app,
                "GET",
                &format!("/$/ingest/{ds}/{id}?wait=30"),
                &hs,
                "",
            )
            .await;
            assert_eq!(w.status, StatusCode::OK, "{}", w.text());
            w.json()
        }
    };
    let head = |ds: &str| s.state.get(ds).unwrap().store.head_commit().seq;
    // bob only reads team-a
    let at = head("team-a");
    let v = ingest("bob", "team-a", "http://ex.org/doc").await;
    assert_eq!(v["error"]["code"], "forbidden", "{v}");
    assert_eq!(head("team-a"), at);
    // gsrc writes one graph of wiki, but not the evidence graphs
    let at = head("wiki");
    let v = ingest("gsrc", "wiki", "http://ex.org/src/doc").await;
    assert_eq!(v["error"]["code"], "forbidden", "{v}");
    assert!(
        v["error"]["message"].as_str().unwrap().contains("urn:ev:"),
        "{v}"
    );
    assert_eq!(head("wiki"), at);
    assert!(
        m.requests().is_empty(),
        "no model call before the grants pass"
    );
    // bob writes wiki: the grants pass and the extraction runs
    let v = ingest("bob", "wiki", "http://ex.org/doc").await;
    assert_eq!(v["status"], "done", "{v}");
    assert_eq!(v["result"]["mode"], "claims", "{v}");
    assert!(!m.requests().is_empty());
    assert_eq!(head("wiki"), at + 1);
}
