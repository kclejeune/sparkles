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

/// C27 A4 over HTTP (spec C27 §4.3): an authenticated proposal's availability is the
/// server's commit time, the as-known state before it excludes the import whatever its
/// publication date, and a client cannot backdate availability through the proposal
/// body, SPARQL Update or the Graph Store.
#[tokio::test]
async fn authenticated_availability_is_server_owned_and_cannot_be_backdated() {
    // commit times never decrease from the root commit's wall clock, so the test
    // clock runs in 2100
    const JANUARY: i64 = 4_103_654_400_000;
    const MARCH: i64 = 4_108_752_000_000;
    let s = proposal_server();
    let ds = s.state.get("ev").unwrap();
    let now = Arc::new(std::sync::atomic::AtomicI64::new(JANUARY));
    let read = now.clone();
    ds.store.set_clock(Arc::new(move || {
        read.load(std::sync::atomic::Ordering::SeqCst)
    }));
    ds.dataset
        .update("INSERT DATA { GRAPH <urn:g> { <urn:a> <urn:p> <urn:o> } }")
        .unwrap();
    now.store(MARCH, std::sync::atomic::Ordering::SeqCst);
    let mut body = revision_and_claim("c1");
    body["revisions"][0]["publishedAt"] = "2100-01-05T00:00:00Z".into();
    // a client-supplied availability is refused while the body is read
    let head = ds.store.head_commit();
    for forged in [
        {
            let mut b = body.clone();
            b["revisions"][0]["firstAvailableAt"] = "2100-01-15T00:00:00Z".into();
            b
        },
        {
            let mut b = body.clone();
            b["firstAvailableAt"] = "2100-01-15T00:00:00Z".into();
            b
        },
    ] {
        let r = propose(&s, &b("evw"), &forged).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
        assert_eq!(r.json()["code"], "invalid-proposal");
        assert_eq!(ds.store.head_commit(), head, "nothing was written");
    }
    let r = propose(&s, &b("evw"), &body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let v = r.json();
    let commit = v["commit"].as_u64().unwrap();
    assert_eq!(ds.store.commit(commit).unwrap().timestamp_ms, MARCH);
    assert_eq!(
        v["firstAvailableAt"],
        sparkles::commit::rfc3339_ms(MARCH).as_str()
    );
    let rev = v["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "rev")
        .unwrap()["iri"]
        .as_str()
        .unwrap()
        .to_string();
    // the receipt names the authenticated principal
    assert!(
        ds.dataset
            .ask(&format!(
                "ASK {{ GRAPH <urn:ev:sources> {{ <{rev}> <urn:x-sparkles:evidence:availabilityReceipt> ?op . ?op <http://www.w3.org/ns/prov#wasAssociatedWith> <urn:x-sparkles:principal:user:evw> }} }}"
            ))
            .unwrap()
    );
    // the as-known February state lacks the January-published revision
    let ask = |at: &'static str| {
        let s = &s;
        let rev = rev.clone();
        async move {
            let r = call(
                &s.app,
                "POST",
                &format!("/ev/sparql{at}"),
                &[
                    ("authorization", &b("evw")),
                    ("content-type", "application/sparql-query"),
                    ("accept", "application/sparql-results+json"),
                ],
                &format!("ASK {{ GRAPH ?g {{ <{rev}> ?p ?o }} }}"),
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()["boolean"].as_bool().unwrap()
        }
    };
    assert!(ask("").await);
    assert!(!ask("?at=time:2100-02-15T00:00:00Z").await);
    // direct writes of availability are refused before they publish
    let head = ds.store.head_commit();
    let early = "\"2100-01-15T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime>";
    let update = format!(
        "INSERT DATA {{ GRAPH <urn:ev:sources> {{ <{rev}> <urn:x-sparkles:evidence:firstAvailableAt> {early} }} }}"
    );
    let r = call(
        &s.app,
        "POST",
        "/ev/update",
        &[
            ("authorization", &b("evw")),
            ("content-type", "application/sparql-update"),
        ],
        &update,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["code"], "profile-managed-graph");
    let r = call(
        &s.app,
        "POST",
        "/ev/data?graph=urn:ev:sources",
        &[
            ("authorization", &b("evw")),
            ("content-type", "application/n-triples"),
        ],
        &format!("<{rev}> <urn:x-sparkles:evidence:firstAvailableAt> {early} .\n"),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["code"], "profile-managed-graph");
    assert_eq!(ds.store.head_commit(), head, "nothing was written");
    assert_eq!(
        ds.dataset
            .query(&format!(
                "SELECT ?t {{ GRAPH <urn:ev:sources> {{ <{rev}> <urn:x-sparkles:evidence:firstAvailableAt> ?t }} }}"
            ))
            .unwrap()
            .rows()
            .unwrap()
            .len(),
        1
    );
}

/// A claim's validity record over HTTP (spec C27 §4.4): written with the claim, and
/// refused before anything is written when malformed or sent through open scope.
#[tokio::test]
async fn claim_validity_records_are_written_with_the_claim_over_http() {
    let s = proposal_server();
    let ds = s.state.get("ev").unwrap();
    let validity = serde_json::json!({
        "start": {"kind": "known", "earliest": "2026-01-01T01:00:00+01:00",
                  "latest": "2026-01-01T01:00:00+01:00",
                  "precision": "urn:x-sparkles:valid-time:Day", "sourceValue": "1 January 2026"},
        "end": {"kind": "unknown", "sourceValue": "until further notice"},
        "normalization": {"status": "normalized", "method": "urn:method:dates",
                          "methodVersion": "1", "calendar": "urn:x-sparkles:valid-time:Gregorian"},
    });
    let head = ds.store.head_commit();
    let mut refused = Vec::new();
    for (path, value) in [
        ("/start/kind", serde_json::json!("sometimes")),
        ("/start/precision", serde_json::Value::Null),
        ("/end/earliest", serde_json::json!("2026-02-01T00:00:00Z")),
        ("/normalization/status", serde_json::json!("guessed")),
        ("/normalization/foldChoice", serde_json::json!("earlier")),
        ("/start/earliest", serde_json::json!("2026-01-01T00:00:00")),
        ("/extra", serde_json::json!(1)),
    ] {
        let mut v = validity.clone();
        let (parent, leaf) = path.rsplit_once('/').unwrap();
        let target = if parent.is_empty() {
            &mut v
        } else {
            v.pointer_mut(parent).unwrap()
        };
        if value.is_null() {
            target.as_object_mut().unwrap().remove(leaf);
        } else {
            target[leaf] = value;
        }
        let mut body = revision_and_claim("c1");
        body["claims"][0]["validity"] = v;
        let r = propose(&s, &b("evw"), &body).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{path}: {}", r.text());
        refused.push(r.json()["code"].clone());
    }
    let mut scoped = revision_and_claim("c1");
    scoped["claims"][0]["scope"] =
        serde_json::json!({"urn:x-sparkles:valid-time:validDuring": ["<urn:period>"]});
    let r = propose(&s, &b("evw"), &scoped).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    refused.push(r.json()["code"].clone());
    // every refusal carries the proposal API's code
    assert!(
        refused.iter().all(|c| c == "invalid-proposal"),
        "{refused:?}"
    );
    assert_eq!(ds.store.head_commit(), head, "nothing was written");
    // a valid record is written with the claim, its instants in UTC
    let mut body = revision_and_claim("c1");
    body["claims"][0]["validity"] = validity;
    let r = propose(&s, &b("evw"), &body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let claim = r.json()["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "claim")
        .unwrap()["iri"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        ds.dataset
            .ask(&format!(
                "PREFIX vt: <urn:x-sparkles:valid-time:>
                 ASK {{ GRAPH <urn:ev:claims> {{
                   <{claim}> vt:validDuring ?i . ?i a vt:Interval ; vt:normalization/vt:status vt:Normalized ;
                     vt:start [ vt:kind vt:Known ; vt:earliest \"2026-01-01T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime> ] ;
                     vt:end [ vt:kind vt:Unknown ; vt:sourceValue \"until further notice\" ] }} }}"
            ))
            .unwrap()
    );
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

#[tokio::test]
async fn evidence_settings_require_admin_and_drift_does_not_disclose() {
    let s = proposal_server();
    // Replace declared settings on `s.state` with a settings file that drifts from
    // what `proposal_server()` recorded (adds a second source graph).
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    std::fs::write(
        &f,
        serde_json::json!({
            "datasets": {
                "ev": {
                    "evidence": {
                        "enabled": true,
                        "sourceRecords": ["urn:ev:sources", "urn:ev:secret-sources"],
                        "proposals": ["urn:ev:claims"]
                    },
                    "locked": ["evidence.sourceRecords"]
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let settings = crate::settings::Settings::load(&f, None).unwrap();
    let d = settings.declared();
    // Swap the declared settings directly onto the existing server state.
    s.state.settings.replace_declared_for_test(d);

    let evw = basic("evw", "evw-pw");
    let alice = b("alice");

    // Non-admin writer cannot read GET /$/settings/ev/evidence (C25 decision 7).
    let r = call(
        &s.app,
        "GET",
        "/$/settings/ev/evidence",
        &[("authorization", &evw)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    // Non-admin GET /$/settings/ev omits the evidence kind.
    let r = call(
        &s.app,
        "GET",
        "/$/settings/ev",
        &[("authorization", &evw)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["kinds"].get("evidence").is_none(), "{}", r.text());

    // Admin alice reads GET /$/settings/ev/evidence and sees drift: true.
    let r = call(
        &s.app,
        "GET",
        "/$/settings/ev/evidence",
        &[("authorization", &alice)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["recorded"]["drift"], true);

    // Admin GET /$/settings/ev includes evidence.
    let r = call(
        &s.app,
        "GET",
        "/$/settings/ev",
        &[("authorization", &alice)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["kinds"].get("evidence").is_some(), "{}", r.text());

    // Writer evw gets 409 evidence-settings-drift without leaking secret graph IRIs.
    let r = propose(&s, &evw, &revision_and_claim("occ-drift")).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["code"], "evidence-settings-drift");
    assert!(!r.text().contains("urn:ev:secret-sources"), "{}", r.text());
    assert!(!r.text().contains("urn:ev:sources"), "{}", r.text());

    // Unauthorized user bob still gets 403/404, not evidence-settings-drift.
    let r = propose(&s, &b("bob"), &revision_and_claim("occ-unauth")).await;
    assert!(
        matches!(r.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{}: {}",
        r.status,
        r.text()
    );

    // Keep tempdir alive until end of test.
    drop(dir);
}

#[tokio::test]
async fn evidence_settings_reads_use_admin_middleware() {
    let s = build(super::reader_fixture());
    let admin = b("alice");
    let reader = bearer(&tok('R'));
    for path in [
        "/$/settings/public/evidence",
        "/$/settings/public/%65vidence",
    ] {
        for method in ["GET", "HEAD"] {
            let r = call(&s.app, method, path, &[], "").await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
            assert!(!r.all("www-authenticate").is_empty());
            let r = call(&s.app, method, path, &[("authorization", &reader)], "").await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
            assert!(
                r.all("www-authenticate")
                    .iter()
                    .any(|h| h.contains("insufficient_scope"))
            );
            let r = call(
                &s.app,
                method,
                path,
                &[("authorization", &admin), ("sec-fetch-site", "cross-site")],
                "",
            )
            .await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
        }
    }
}

// -------------------------------------------------- support-aware projection visibility --

#[tokio::test(flavor = "multi_thread")]
async fn projected_triples_enforce_support_visibility_across_sparql_gsp_recall_and_ask() {
    use crate::models::mock::{self, MockModel};
    use crate::models::{Models, ModelsConfig};
    use oxrdf::{NamedNode, Triple};
    use sparkles::evidence::{
        self, ClaimInput, ClaimKind, ConflictMode, DecisionInput, Disposition, EvidenceInput,
        Polarity, Policy, ProposeOptions, RecordKey, RecordRef, RecordVersion, ReviewOptions,
        Settings, Sha256Digest, SourceContent, SourceRevisionInput, WithdrawalInput,
    };
    use sparkles::task::Control;

    const SOURCES_A: &str = "urn:ev:sources-a";
    const SOURCES_B: &str = "urn:ev:sources-b";
    const CLAIMS_A: &str = "urn:ev:claims-a";
    const CLAIMS_B: &str = "urn:ev:claims-b";
    const DECISIONS: &str = "urn:ev:decisions";
    const SUPPORTS: &str = "urn:ev:supports";
    const ACCEPTED: &str = "urn:ev:accepted";
    const PUBLIC: &str = "urn:ev:public";

    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    let s = build(Fixture {
        extra: format!(
            r#"
[[users]]
name = "ra"
password = "{ra}"
[[users.grants]]
dataset = "ev"
level = "read"
graphs = ["{PUBLIC}", "{ACCEPTED}", "{DECISIONS}", "{SUPPORTS}", "{CLAIMS_A}", "{SOURCES_A}"]

[[users]]
name = "rb"
password = "{rb}"
[[users.grants]]
dataset = "ev"
level = "read"
graphs = ["{PUBLIC}", "{ACCEPTED}", "{DECISIONS}", "{SUPPORTS}", "{CLAIMS_B}", "{SOURCES_B}"]

[[users]]
name = "rmeta"
password = "{rmeta}"
[[users.grants]]
dataset = "ev"
level = "read"
graphs = ["{PUBLIC}", "{ACCEPTED}", "{DECISIONS}", "{SUPPORTS}"]

[[users]]
name = "rout"
password = "{rout}"
[[users.grants]]
dataset = "ev"
level = "read"
graphs = ["{PUBLIC}", "{ACCEPTED}"]

[[users]]
name = "rall"
password = "{rall}"
datasets = {{ ev = "read" }}
"#,
            ra = h("ra-pw"),
            rb = h("rb-pw"),
            rmeta = h("rmeta-pw"),
            rout = h("rout-pw"),
            rall = h("rall-pw"),
        ),
        ..Default::default()
    });

    // Wire a mock model so we can verify POST /ev/ask as well.
    let mock_model = MockModel::start(|req, _| {
        let body = req.body.to_string();
        if body.contains("summarize") || body.contains("Question:") {
            return (
                200,
                mock::openai(
                    &serde_json::json!({
                        "text": "Checked the release status.",
                        "citations": [],
                    })
                    .to_string(),
                ),
            );
        }
        (
            200,
            mock::openai(
                &serde_json::json!({
                    "sparql": format!(
                        "SELECT ?cve WHERE {{ GRAPH <{ACCEPTED}> {{ <urn:release> <urn:affectedBy> ?cve }} }}"
                    ),
                    "reason": "check release vulnerabilities"
                })
                .to_string(),
            ),
        )
    });
    let pair = serde_json::json!({ "provider": "p", "model": "m" });
    let cfg = serde_json::json!({
        "providers": {
            "p": {
                "kind": "openai",
                "endpoint": mock_model.url(),
                "requestTimeoutSecs": 5,
                "allowedModels": ["m"]
            }
        },
        "roles": { "draft": [pair], "summarize": [pair] },
    });
    let models = Models::new(
        ModelsConfig::parse(&cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    );
    s.state.models.store(Some(Arc::new(models)));

    let ds = s.state.attach("ev", DbType::Memory, None).unwrap();
    ds.store
        .set_retention(sparkles::history::Retention {
            keep_commits: Some(64),
            ..Default::default()
        })
        .unwrap();
    let prep = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    ds.dataset
        .store()
        .activate_memory_configuration_history(&prep)
        .unwrap();
    let settings = Settings {
        enabled: true,
        source_records: vec![SOURCES_A.into(), SOURCES_B.into()],
        proposals: vec![CLAIMS_A.into(), CLAIMS_B.into()],
        decisions: vec![DECISIONS.into()],
        supports: vec![SUPPORTS.into()],
        projections: vec![ACCEPTED.into()],
        policies: [(
            "main".into(),
            Policy {
                version: 1,
                admitted_kinds: vec![ClaimKind::Reported],
                required_scope: vec![],
                conflicts: ConflictMode::AdmitLabelled,
                boundary: vec![
                    SOURCES_A.into(),
                    SOURCES_B.into(),
                    CLAIMS_A.into(),
                    CLAIMS_B.into(),
                ],
                decisions: DECISIONS.into(),
                supports: SUPPORTS.into(),
                output: ACCEPTED.into(),
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    evidence::configure(&ds.dataset, &settings, &Control::none()).unwrap();
    // Add an ordinary public triple and property declaration so Recall/Ask always have a
    // readable entity anchor and known predicate without disclosing hidden projections.
    ds.dataset
        .update(&format!(
            "INSERT DATA {{ GRAPH <{PUBLIC}> {{ <urn:release> <http://www.w3.org/2000/01/rdf-schema#label> \"Release 1\" . <urn:affectedBy> a <http://www.w3.org/1999/02/22-rdf-syntax-ns#Property> }} }}"
        ))
        .unwrap();
    // Enable assistant on `ev` as server admin `alice`.
    let r = call(
        &s.app,
        "PUT",
        "/$/assistant/ev",
        &[
            ("authorization", &b("alice")),
            ("content-type", "application/json"),
        ],
        &serde_json::json!({ "enabled": true, "send": "rows" }).to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let iri = |u: &str| NamedNode::new(u).unwrap();
    let all = || Some(Arc::new(sparkles::access::GraphAccess::all()));
    let propose_claim = |src_g: &str, clm_g: &str, src_k: &str, occ: &str, cve: &str| {
        let input = EvidenceInput {
            revisions: vec![SourceRevisionInput {
                version: RecordVersion::V1,
                key: RecordKey::new("rev").unwrap(),
                source: iri(&format!("urn:source:{src_k}")),
                source_key: RecordKey::new(src_k).unwrap(),
                upstream_version: None,
                content: SourceContent::MetadataOnly {
                    digest: Sha256Digest::of_bytes(src_k.as_bytes(), &Control::none()).unwrap(),
                },
                metadata: Default::default(),
                times: Default::default(),
                lineage: Default::default(),
            }],
            claims: vec![ClaimInput {
                version: RecordVersion::V1,
                key: RecordKey::new("claim").unwrap(),
                occurrence_key: RecordKey::new(occ).unwrap(),
                revision: RecordRef::Local(RecordKey::new("rev").unwrap()),
                proposition: Triple::new(iri("urn:release"), iri("urn:affectedBy"), iri(cve)),
                kind: ClaimKind::Reported,
                polarity: Polarity::Positive,
                attributed_author: iri("urn:publisher"),
                scope: Default::default(),
                confidence: Default::default(),
                validity: None,
            }],
            ..Default::default()
        };
        let mut o = ProposeOptions::new(iri("urn:principal:importer"));
        o.source_graph = Some(iri(src_g));
        o.proposal_graph = Some(iri(clm_g));
        let p = evidence::propose(
            None,
            &ds.dataset,
            &ds.dataset,
            &input,
            &o,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        let rec = p.records.iter().find(|r| r.key == "claim").unwrap();
        (rec.iri.clone(), rec.digest)
    };
    let accept_claim = |c: &(NamedNode, Sha256Digest)| {
        let mut ro = ReviewOptions::new(iri("urn:principal:reviewer"));
        ro.if_head = Some(ds.store.head_commit().seq);
        evidence::review(
            None,
            &ds.dataset,
            &ds.dataset,
            "main",
            1,
            &[DecisionInput {
                key: "d".into(),
                target: c.0.clone(),
                target_digest: c.1,
                disposition: Disposition::Accepted,
                supersedes: None,
            }],
            &ro,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
    };

    // Both Claim A and Claim B support cve-1; only Claim B2 supports cve-2.
    let ca = propose_claim(SOURCES_A, CLAIMS_A, "src-a", "a1", "urn:cve-1");
    let cb = propose_claim(SOURCES_B, CLAIMS_B, "src-b", "b1", "urn:cve-1");
    let cb2 = propose_claim(SOURCES_B, CLAIMS_B, "src-b2", "b2", "urn:cve-2");
    accept_claim(&ca);
    accept_claim(&cb);
    accept_claim(&cb2);
    let commit_before_withdraw = ds.store.head_commit().seq;

    let sparql_objects = |user: &'static str, at: Option<u64>| {
        let app = s.app.clone();
        async move {
            let path = match at {
                Some(seq) => format!("/ev/sparql?at=commit:{seq}"),
                None => "/ev/sparql".to_string(),
            };
            let q = format!(
                "SELECT ?o WHERE {{ GRAPH <{ACCEPTED}> {{ <urn:release> <urn:affectedBy> ?o }} }} ORDER BY ?o"
            );
            let r = call(
                &app,
                "POST",
                &path,
                &[
                    ("authorization", &b(user)),
                    ("content-type", "application/sparql-query"),
                    ("accept", "application/sparql-results+json"),
                ],
                &q,
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()["results"]["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["o"]["value"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };

    let gsp_get = |user: &'static str| {
        let app = s.app.clone();
        async move {
            call(
                &app,
                "GET",
                &format!("/ev/data?graph={ACCEPTED}"),
                &[
                    ("authorization", &b(user)),
                    ("accept", "application/n-triples"),
                ],
                "",
            )
            .await
        }
    };

    let recall_accepted_objects = |user: &'static str, policy: Option<&'static str>| {
        let app = s.app.clone();
        async move {
            let mut body = serde_json::json!({
                "seeds": ["urn:release"],
                "format": "json"
            });
            if let Some(p) = policy {
                body["reviewPolicy"] = serde_json::json!(p);
            }
            let r = call(
                &app,
                "POST",
                "/ev/recall",
                &[
                    ("authorization", &b(user)),
                    ("content-type", "application/json"),
                ],
                &body.to_string(),
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            let v = r.json();
            let mut objs = Vec::new();
            for e in v["entities"].as_array().unwrap() {
                for f in e["facts"].as_array().unwrap() {
                    if f["p"].as_str().unwrap().contains("affectedBy") {
                        objs.push(f["o"].as_str().unwrap().to_string());
                    }
                }
            }
            objs.sort();
            (objs, v)
        }
    };

    // 1. SPARQL before withdrawal
    assert_eq!(sparql_objects("ra", None).await, vec!["urn:cve-1"]);
    assert_eq!(
        sparql_objects("rb", None).await,
        vec!["urn:cve-1", "urn:cve-2"]
    );
    assert!(sparql_objects("rmeta", None).await.is_empty());
    assert!(sparql_objects("rout", None).await.is_empty());
    assert_eq!(
        sparql_objects("rall", None).await,
        vec!["urn:cve-1", "urn:cve-2"]
    );

    // 2. Graph Store GET before withdrawal (non-disclosing 404 for readers with no readable support)
    let r = gsp_get("ra").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("urn:cve-1") && !r.text().contains("urn:cve-2"));
    let r = gsp_get("rb").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("urn:cve-1") && r.text().contains("urn:cve-2"));
    assert_eq!(gsp_get("rmeta").await.status, StatusCode::NOT_FOUND);
    assert_eq!(gsp_get("rout").await.status, StatusCode::NOT_FOUND);

    // 3. Recall (without and with reviewPolicy) before withdrawal
    assert_eq!(
        recall_accepted_objects("ra", None).await.0,
        vec!["<urn:cve-1>"]
    );
    assert_eq!(
        recall_accepted_objects("rb", None).await.0,
        vec!["<urn:cve-1>", "<urn:cve-2>"]
    );
    assert!(recall_accepted_objects("rmeta", None).await.0.is_empty());
    assert!(recall_accepted_objects("rout", None).await.0.is_empty());

    let (ra_pol_objs, ra_pol_json) = recall_accepted_objects("ra", Some("main")).await;
    assert_eq!(ra_pol_objs, vec!["<urn:cve-1>"]);
    assert_eq!(
        ra_pol_json["claims"]["occurrences"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (rmeta_pol_objs, rmeta_pol_json) = recall_accepted_objects("rmeta", Some("main")).await;
    assert!(rmeta_pol_objs.is_empty());
    assert!(
        rmeta_pol_json["claims"]["occurrences"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // 4. Ask before withdrawal: ra gets cve-1, rmeta gets 0 rows and no leaked cve in model prompt.
    let ask_req = |user: &'static str| {
        let app = s.app.clone();
        async move {
            let r = call(
                &app,
                "POST",
                "/ev/ask",
                &[
                    ("authorization", &b(user)),
                    ("content-type", "application/json"),
                    ("accept", "application/json"),
                ],
                &serde_json::json!({
                    "question": "Which CVEs affect Release 1?",
                    "query": format!(
                        "SELECT ?cve WHERE {{ GRAPH <{ACCEPTED}> {{ <urn:release> <urn:affectedBy> ?cve }} }} ORDER BY ?cve"
                    )
                })
                .to_string(),
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()
        }
    };
    let ask_ra = ask_req("ra").await;
    assert_eq!(ask_ra["outcome"], "answered", "{ask_ra}");
    let ra_res_str = ask_ra["result"]["results"].to_string();
    assert!(
        ra_res_str.contains("urn:cve-1") && !ra_res_str.contains("urn:cve-2"),
        "{ask_ra}"
    );
    let before_rmeta_reqs = mock_model.requests().len();
    let ask_rmeta = ask_req("rmeta").await;
    assert_eq!(ask_rmeta["outcome"], "empty", "{ask_rmeta}");
    assert!(
        !ask_rmeta.to_string().contains("cve-1") && !ask_rmeta.to_string().contains("cve-2"),
        "{ask_rmeta}"
    );
    for req in &mock_model.requests()[before_rmeta_reqs..] {
        let txt = req.body.to_string();
        assert!(!txt.contains("cve-1") && !txt.contains("cve-2"), "{txt}");
    }

    // 5. Withdraw Claim A: cve-1 remains projected via Claim B, so ra loses visibility across
    //    SPARQL, GSP, Recall and Ask while rb keeps both triples.
    evidence::withdraw(
        &ds.dataset,
        &ds.dataset,
        &[WithdrawalInput {
            key: "w".into(),
            target: ca.0.clone(),
            target_digest: ca.1,
        }],
        &ReviewOptions::new(iri("urn:principal:reviewer")),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();

    assert!(sparql_objects("ra", None).await.is_empty());
    assert_eq!(
        sparql_objects("rb", None).await,
        vec!["urn:cve-1", "urn:cve-2"]
    );
    assert_eq!(gsp_get("ra").await.status, StatusCode::NOT_FOUND);
    assert_eq!(gsp_get("rb").await.status, StatusCode::OK);
    assert!(recall_accepted_objects("ra", None).await.0.is_empty());
    assert!(
        recall_accepted_objects("ra", Some("main"))
            .await
            .0
            .is_empty()
    );
    assert_eq!(
        recall_accepted_objects("rb", None).await.0,
        vec!["<urn:cve-1>", "<urn:cve-2>"]
    );
    let before_ra_after_withdraw = mock_model.requests().len();
    let ask_ra_after = ask_req("ra").await;
    assert_eq!(ask_ra_after["outcome"], "empty", "{ask_ra_after}");
    assert!(
        !ask_ra_after.to_string().contains("cve-1") && !ask_ra_after.to_string().contains("cve-2"),
        "{ask_ra_after}"
    );
    for req in &mock_model.requests()[before_ra_after_withdraw..] {
        let txt = req.body.to_string();
        assert!(!txt.contains("cve-1") && !txt.contains("cve-2"), "{txt}");
    }
    let ask_rb_after = ask_req("rb").await;
    assert_eq!(ask_rb_after["outcome"], "answered", "{ask_rb_after}");
    let rb_res_str = ask_rb_after["result"]["results"].to_string();
    assert!(
        rb_res_str.contains("urn:cve-1") && rb_res_str.contains("urn:cve-2"),
        "{ask_rb_after}"
    );

    // 6. Historical SPARQL at the commit before withdrawal still shows cve-1 to ra
    //    and hides it from rmeta.
    assert_eq!(
        sparql_objects("ra", Some(commit_before_withdraw)).await,
        vec!["urn:cve-1"]
    );
    assert!(
        sparql_objects("rmeta", Some(commit_before_withdraw))
            .await
            .is_empty()
    );
}
