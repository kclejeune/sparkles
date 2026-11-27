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

/// `POST /$/evidence/{ds}/identities/rebuild` requires unrestricted `admin` on `{ds}`:
/// anonymous callers get `401`, non-admin writers and readers get `403`, and an admin
/// succeeds.
#[tokio::test]
async fn identity_map_rebuild_requires_admin() {
    let s = proposal_server();
    let alice = b("alice");
    let evw = basic("evw", "evw-pw");
    let bob = b("bob");
    let body = serde_json::json!({
        "format": 1, "profile": 1, "enabled": true,
        "sourceRecords": ["urn:ev:sources"], "proposals": ["urn:ev:claims"],
        "decisions": ["urn:ev:decisions"],
        "identity": ["urn:ev:identity", "urn:ev:maps"],
        "projections": ["urn:ev:map"],
        "matchingPolicies": {"people": {
            "version": 1,
            "boundary": ["urn:ev:identity", "urn:ev:claims"],
            "decisions": "urn:ev:decisions",
            "maps": "urn:ev:maps",
            "output": "urn:ev:map"
        }}
    })
    .to_string();
    let r = call(
        &s.app,
        "PUT",
        "/$/evidence/ev/settings",
        &[
            ("authorization", &alice),
            ("content-type", "application/json"),
        ],
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let req_body = serde_json::json!({"policy": "people"}).to_string();
    // Anonymous -> 401
    let r = call(
        &s.app,
        "POST",
        "/$/evidence/ev/identities/rebuild",
        &[("content-type", "application/json")],
        &req_body,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());

    // Writer (non-admin) -> 403
    let r = call(
        &s.app,
        "POST",
        "/$/evidence/ev/identities/rebuild",
        &[
            ("authorization", &evw),
            ("content-type", "application/json"),
        ],
        &req_body,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    // Reader/unrelated user -> 403 or 404
    let r = call(
        &s.app,
        "POST",
        "/$/evidence/ev/identities/rebuild",
        &[
            ("authorization", &bob),
            ("content-type", "application/json"),
        ],
        &req_body,
    )
    .await;
    assert!(
        matches!(r.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{}: {}",
        r.status,
        r.text()
    );

    // Unrestricted admin -> 200
    let r = call(
        &s.app,
        "POST",
        "/$/evidence/ev/identities/rebuild",
        &[
            ("authorization", &alice),
            ("content-type", "application/json"),
        ],
        &req_body,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["committed"], true);
    assert_eq!(r.json()["number"], 1);
}

/// `POST /$/evidence/{ds}/identities/rebuild` refuses restricted admins (including
/// branch-scoped error formatting), allows branch-scoped unrestricted admins only on their
/// branch, and refuses on a read-only server.
#[tokio::test]
async fn identity_map_rebuild_enforces_restricted_branch_and_read_only_admin() {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    let s = build(Fixture {
        extra: format!(
            r#"
[[users]]
name = "devadmin"
password = "{devadmin}"
[[users.grants]]
dataset = "ev"
level = "admin"
branches = ["dev"]
"#,
            devadmin = h("devadmin-pw"),
        ),
        ..Default::default()
    });
    let alice = b("alice");
    let devadmin = basic("devadmin", "devadmin-pw");
    s.state.attach("ev", DbType::Persistent, None).unwrap();
    let settings_body = serde_json::json!({
        "format": 1, "profile": 1, "enabled": true,
        "sourceRecords": ["urn:ev:sources"], "proposals": ["urn:ev:claims"],
        "decisions": ["urn:ev:decisions"],
        "identity": ["urn:ev:identity", "urn:ev:maps"],
        "projections": ["urn:ev:map"],
        "matchingPolicies": {"people": {
            "version": 1,
            "boundary": ["urn:ev:identity", "urn:ev:claims"],
            "decisions": "urn:ev:decisions",
            "maps": "urn:ev:maps",
            "output": "urn:ev:map"
        }}
    })
    .to_string();
    let r = call(
        &s.app,
        "PUT",
        "/$/evidence/ev/settings",
        &[
            ("authorization", &alice),
            ("content-type", "application/json"),
        ],
        &settings_body,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = call(
        &s.app,
        "POST",
        "/$/branches/ev",
        &[
            ("authorization", &alice),
            ("content-type", "application/json"),
        ],
        r#"{"name":"dev"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    let req_body = serde_json::json!({"policy": "people"}).to_string();
    // Restricted admin (hiding graphs) -> 403 on main and 403 naming `/ev@dev` on branch.
    let mut restricted_admin = crate::auth::Principal::local();
    restricted_admin.kind = crate::auth::Kind::User;
    restricted_admin.name = "radmin".into();
    restricted_admin = restricted_admin.hiding_graphs(&["urn:ev:private".into()]);
    let exec = |branch: &str| crate::evidence::Execution {
        branch: branch.into(),
        cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        deadline: None,
        grant: None,
    };
    let main_err = crate::evidence::execute(
        "rebuild-identities",
        s.state.clone(),
        "ev".into(),
        restricted_admin.clone(),
        axum::body::Bytes::from(req_body.clone()),
        exec(sparkles::branch::MAIN),
    )
    .unwrap_err();
    assert_eq!(main_err.0, StatusCode::FORBIDDEN, "{:?}", main_err.1);
    let dev_err = crate::evidence::execute(
        "rebuild-identities",
        s.state.clone(),
        "ev".into(),
        restricted_admin.on_branch(Some("dev")),
        axum::body::Bytes::from(req_body.clone()),
        exec("dev"),
    )
    .unwrap_err();
    assert_eq!(dev_err.0, StatusCode::FORBIDDEN, "{:?}", dev_err.1);
    assert!(
        dev_err.1.to_string().contains("/ev@dev"),
        "expected branch name in error: {}",
        dev_err.1
    );

    // Branch-scoped unrestricted admin -> refused on main, succeeds on ?branch=dev.
    let r = call(
        &s.app,
        "POST",
        "/$/evidence/ev/identities/rebuild",
        &[
            ("authorization", &devadmin),
            ("content-type", "application/json"),
        ],
        &req_body,
    )
    .await;
    assert!(
        matches!(r.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{}: {}",
        r.status,
        r.text()
    );
    let r = call(
        &s.app,
        "POST",
        "/$/evidence/ev/identities/rebuild?branch=dev",
        &[
            ("authorization", &devadmin),
            ("content-type", "application/json"),
        ],
        &req_body,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["committed"], true);

    // Read-only server refuses rebuild with 403 even for an unrestricted admin.
    let ro_dir = tempfile::tempdir().unwrap();
    let mut ro_state = AppState::new(
        ro_dir.path(),
        sparkles::store::StoreOptions::default(),
        std::time::Duration::from_secs(30),
    )
    .unwrap();
    ro_state.attach("ev", DbType::Persistent, None).unwrap();
    let ro_ds = ro_state.get("ev").unwrap();
    let ev_settings = sparkles::evidence::Settings {
        enabled: true,
        source_records: vec!["urn:ev:sources".into()],
        proposals: vec!["urn:ev:claims".into()],
        decisions: vec!["urn:ev:decisions".into()],
        identity: vec!["urn:ev:identity".into(), "urn:ev:maps".into()],
        projections: vec!["urn:ev:map".into()],
        matching_policies: [(
            "people".to_string(),
            sparkles::evidence::MatchingPolicy {
                version: 1,
                boundary: vec!["urn:ev:identity".into(), "urn:ev:claims".into()],
                decisions: "urn:ev:decisions".into(),
                maps: "urn:ev:maps".into(),
                output: "urn:ev:map".into(),
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    sparkles::evidence::configure(
        &ro_ds.dataset,
        &ev_settings,
        &sparkles::task::Control::none(),
    )
    .unwrap();
    ro_state.read_only = true;
    let ro_app = crate::http::router(Arc::new(ro_state));
    let r = call(
        &ro_app,
        "POST",
        "/$/evidence/ev/identities/rebuild",
        &[("content-type", "application/json")],
        &req_body,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
}

#[tokio::test]
async fn evidence_reauthenticate_requires_admin_and_respects_drift() {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    let s = build(Fixture {
        extra: format!(
            r#"
[[users]]
name = "evw"
password = "{evw}"
datasets = {{ ev = "write" }}

[[users]]
name = "main_admin"
password = "{ma}"
[[users.grants]]
dataset = "ev"
branches = ["main"]
level = "admin"
"#,
            evw = h("evw-pw"),
            ma = h("ma-pw"),
        ),
        ..Default::default()
    });
    // Create persistent dataset `ev` and activate configuration history before attaching.
    let ev_path = s.dir.path().join("databases").join("ev");
    {
        let _ds = sparkles::Dataset::open(&ev_path).unwrap();
        drop(_ds);
        let prep =
            sparkles::access::Preparation::new(sparkles::task::Control::none(), 64 << 20).unwrap();
        sparkles::migrate::activate_configuration_history(&ev_path, false, &prep).unwrap();
    }
    let ds = s
        .state
        .attach("ev", DbType::Persistent, Some(&ev_path))
        .unwrap();
    let settings = sparkles::evidence::Settings {
        enabled: true,
        source_records: vec!["urn:ev:sources".into()],
        proposals: vec!["urn:ev:claims".into()],
        ..Default::default()
    };
    sparkles::evidence::configure(&ds.dataset, &settings, &sparkles::task::Control::none())
        .unwrap();
    let alice = b("alice");
    let evw = basic("evw", "evw-pw");
    let main_admin = basic("main_admin", "ma-pw");
    let bob = b("bob");

    // Propose a claim as writer `evw`, then fork branch `dev`.
    let r = propose(&s, &evw, &revision_and_claim("reauth-auth")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let claim_iri = r.json()["records"][1]["iri"].as_str().unwrap().to_string();
    ds.dataset
        .create_branch("dev", &Default::default())
        .unwrap();

    // Admin detaches and re-protects `urn:ev:claims`.
    let r = call(
        &s.app,
        "POST",
        "/$/evidence/ev/detach",
        &[
            ("authorization", &alice),
            ("content-type", "application/json"),
        ],
        &serde_json::json!({ "graph": "urn:ev:claims" }).to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = call(
        &s.app,
        "PUT",
        "/$/evidence/ev/settings",
        &[
            ("authorization", &alice),
            ("content-type", "application/json"),
        ],
        &serde_json::json!({
            "format": 1,
            "profile": 1,
            "enabled": true,
            "sourceRecords": ["urn:ev:sources"],
            "proposals": ["urn:ev:claims"]
        })
        .to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let body = serde_json::json!({ "graph": "urn:ev:claims" }).to_string();

    // Anonymous caller -> 401.
    let r = call(
        &s.app,
        "POST",
        "/$/evidence/ev/reauthenticate",
        &[("content-type", "application/json")],
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());

    // Writer `evw` (non-admin) -> 403.
    let r = call(
        &s.app,
        "POST",
        "/$/evidence/ev/reauthenticate",
        &[
            ("authorization", &evw),
            ("content-type", "application/json"),
        ],
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    // Branch-scoped admin (`main_admin` has admin on `main` only) -> 403 on `branch: "dev"`.
    let r = call(
        &s.app,
        "POST",
        "/$/evidence/ev/reauthenticate",
        &[
            ("authorization", &main_admin),
            ("content-type", "application/json"),
        ],
        &serde_json::json!({ "graph": "urn:ev:claims", "branch": "dev" }).to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    // Unrelated user `bob` -> 403/404.
    let r = call(
        &s.app,
        "POST",
        "/$/evidence/ev/reauthenticate",
        &[
            ("authorization", &bob),
            ("content-type", "application/json"),
        ],
        &body,
    )
    .await;
    assert!(
        matches!(r.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{}: {}",
        r.status,
        r.text()
    );

    // While declared evidence settings drift, even admin gets 409 evidence-settings-drift.
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("settings.json");
    std::fs::write(
        &f,
        serde_json::json!({
            "datasets": {
                "ev": {
                    "evidence": {
                        "enabled": true,
                        "sourceRecords": ["urn:ev:sources", "urn:ev:extra-sources"],
                        "proposals": ["urn:ev:claims"]
                    }
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let drifted = crate::settings::Settings::load(&f, None).unwrap();
    let orig_declared = s.state.settings.declared();
    s.state
        .settings
        .replace_declared_for_test(drifted.declared());
    let r = call(
        &s.app,
        "POST",
        "/$/evidence/ev/reauthenticate",
        &[
            ("authorization", &alice),
            ("content-type", "application/json"),
        ],
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["code"], "evidence-settings-drift");
    s.state.settings.replace_declared_for_test(orig_declared);

    // Admin `alice` succeeds once reconciled and restores resolution for `evw`.
    let r = call(
        &s.app,
        "POST",
        "/$/evidence/ev/reauthenticate",
        &[
            ("authorization", &alice),
            ("content-type", "application/json"),
        ],
        &body,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["committed"], true);

    let r = call(
        &s.app,
        "POST",
        "/ev/claims/resolve",
        &[
            ("authorization", &evw),
            ("content-type", "application/json"),
        ],
        &serde_json::json!({
            "references": [{ "key": "c", "record": { "iri": claim_iri } }]
        })
        .to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    drop(dir);
}

#[tokio::test]
async fn proposals_selecting_existing_records_verify_over_http() {
    let s = proposal_server();
    let ds = s.state.get("ev").unwrap();
    let sha = |text: &str| {
        sparkles::evidence::Sha256Digest::of_bytes(
            text.as_bytes(),
            &sparkles::task::Control::none(),
        )
        .unwrap()
        .to_string()
    };
    let text = "naïve café release 1.2 is affected.";
    let json_doc = r#"{"affected":["pkg-1","pkg-2"],"score":1.000}"#;
    let plain_doc = "plain non-JSON bulletin";

    // Seed revisions, a normalized-text rendition, a metadata-only revision, a plain-text
    // revision, and an initial passage.
    let seed = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "revisions": [
                {
                    "key": "revText",
                    "source": "urn:src:text",
                    "sourceKey": "src-text",
                    "content": {"text": text}
                },
                {
                    "key": "revJson",
                    "source": "urn:src:json",
                    "sourceKey": "src-json",
                    "content": {"text": json_doc}
                },
                {
                    "key": "revPlain",
                    "source": "urn:src:plain",
                    "sourceKey": "src-plain",
                    "content": {"text": plain_doc}
                },
                {
                    "key": "revMeta",
                    "source": "urn:src:meta",
                    "sourceKey": "src-meta",
                    "content": {"digest": sha(json_doc)}
                }
            ],
            "renditions": [
                {
                    "key": "rendText",
                    "revision": {"key": "revText"},
                    "occurrenceKey": "rend-text",
                    "conversion": {"method": "urn:method:identity", "version": "1"},
                    "mediaType": "text/plain",
                    "content": {"text": text}
                }
            ],
            "passages": [
                {
                    "key": "pass1",
                    "revision": {"key": "revText"},
                    "rendition": {"key": "rendText"},
                    "occurrenceKey": "pass-1",
                    "start": 6,
                    "end": 10,
                    "textDigest": sha("café"),
                    "quote": "café"
                }
            ]
        }),
    )
    .await;
    assert_eq!(seed.status, StatusCode::OK, "{}", seed.text());
    let find_iri = |r: &R, key: &str| {
        r.json()["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|rec| rec["key"] == key)
            .unwrap()["iri"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let rev_text = find_iri(&seed, "revText");
    let rev_json = find_iri(&seed, "revJson");
    let rev_plain = find_iri(&seed, "revPlain");
    let rev_meta = find_iri(&seed, "revMeta");
    let rend_text = find_iri(&seed, "rendText");
    let pass1 = find_iri(&seed, "pass1");

    // 1. Propose a passage against the existing rendition: matching succeeds.
    let ok_passage = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "idempotencyKey": "http-existing-passage",
            "passages": [{
                "key": "p2",
                "revision": {"iri": rev_text},
                "rendition": {"iri": rend_text},
                "occurrenceKey": "pass-2",
                "start": 11,
                "end": 22,
                "textDigest": sha("release 1.2"),
                "quote": "release 1.2"
            }]
        }),
    )
    .await;
    assert_eq!(ok_passage.status, StatusCode::OK, "{}", ok_passage.text());
    assert_eq!(ok_passage.json()["committed"], true);
    // Replay returns identical receipt.
    let replay_passage = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "idempotencyKey": "http-existing-passage",
            "passages": [{
                "key": "p2",
                "revision": {"iri": rev_text},
                "rendition": {"iri": rend_text},
                "occurrenceKey": "pass-2",
                "start": 11,
                "end": 22,
                "textDigest": sha("release 1.2"),
                "quote": "release 1.2"
            }]
        }),
    )
    .await;
    assert_eq!(replay_passage.status, StatusCode::OK);
    assert_eq!(replay_passage.json()["replayed"], true);
    assert_eq!(
        replay_passage.json()["records"],
        ok_passage.json()["records"]
    );

    // Mismatched quote/span -> 422 span-mismatch; mismatched revision -> 400 invalid-proposal.
    let bad_span = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "passages": [{
                "key": "p-bad",
                "revision": {"iri": rev_text},
                "rendition": {"iri": rend_text},
                "occurrenceKey": "p-bad",
                "start": 11,
                "end": 22,
                "textDigest": sha("release 1.2"),
                "quote": "wrong quote"
            }]
        }),
    )
    .await;
    assert_eq!(
        bad_span.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        bad_span.text()
    );
    assert_eq!(bad_span.json()["code"], "span-mismatch");

    let bad_rev = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "passages": [{
                "key": "p-rev",
                "revision": {"iri": rev_json},
                "rendition": {"iri": rend_text},
                "occurrenceKey": "p-rev",
                "start": 11,
                "end": 22,
                "textDigest": sha("release 1.2"),
                "quote": "release 1.2"
            }]
        }),
    )
    .await;
    assert_eq!(
        bad_rev.status,
        StatusCode::BAD_REQUEST,
        "{}",
        bad_rev.text()
    );
    assert_eq!(bad_rev.json()["code"], "invalid-proposal");

    // 2. Reference an existing passage: matching succeeds, mismatched revision -> 400,
    // unauthorized evp gets identical 422 evidence-unavailable as for missing.
    let ok_ref = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "references": [{
                "key": "refPass",
                "occurrenceKey": "ref-pass-1",
                "passage": {
                    "revision": {"iri": rev_text},
                    "rendition": {"iri": rend_text},
                    "passage": {"iri": pass1}
                }
            }]
        }),
    )
    .await;
    assert_eq!(ok_ref.status, StatusCode::OK, "{}", ok_ref.text());

    let bad_pass_ref = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "references": [{
                "key": "refBad",
                "occurrenceKey": "ref-bad",
                "passage": {
                    "revision": {"iri": rev_json},
                    "rendition": {"iri": rend_text},
                    "passage": {"iri": pass1}
                }
            }]
        }),
    )
    .await;
    assert_eq!(
        bad_pass_ref.status,
        StatusCode::BAD_REQUEST,
        "{}",
        bad_pass_ref.text()
    );
    assert_eq!(bad_pass_ref.json()["code"], "invalid-proposal");

    let missing_iri = "urn:uuid:00000000-0000-4000-8000-000000000000";
    let unauth_pass_ref = |p_iri: &str| {
        serde_json::json!({
            "references": [{
                "key": "refHidden",
                "occurrenceKey": "ref-hidden",
                "passage": {
                    "revision": {"iri": rev_text},
                    "rendition": {"iri": rend_text},
                    "passage": {"iri": p_iri}
                }
            }]
        })
    };
    let hidden_p = propose(&s, &b("evp"), &unauth_pass_ref(&pass1)).await;
    let missing_p = propose(&s, &b("evp"), &unauth_pass_ref(missing_iri)).await;
    assert_eq!(
        hidden_p.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        hidden_p.text()
    );
    assert_eq!(hidden_p.json()["code"], "evidence-unavailable");
    let scrub = |r: &R, iri: &str| {
        let mut v = r.json();
        v.as_object_mut().unwrap().remove("requestId");
        serde_json::to_string(&v)
            .unwrap()
            .replace(&rev_text, "REV")
            .replace(&rend_text, "REND")
            .replace(iri, "TARGET")
    };
    assert_eq!(scrub(&hidden_p, &pass1), scrub(&missing_p, missing_iri));

    // 3. Structured values against existing JSON, non-JSON, metadata-only and
    // unauthorized revisions, plus unsupported normalization method.
    let norm = serde_json::json!({
        "method": sparkles::evidence::JSON_LEXICAL_UTF8_V1,
        "version": sparkles::evidence::JSON_LEXICAL_UTF8_VERSION
    });
    let ok_struct = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "references": [
                {
                    "key": "s1",
                    "occurrenceKey": "s1",
                    "structured": {
                        "revision": {"iri": rev_json},
                        "pointer": "/affected/1",
                        "method": norm,
                        "valueDigest": sha("\"pkg-2\"")
                    }
                },
                {
                    "key": "s2",
                    "occurrenceKey": "s2",
                    "structured": {
                        "revision": {"iri": rev_json},
                        "pointer": "/score",
                        "method": norm,
                        "valueDigest": sha("1.000")
                    }
                }
            ]
        }),
    )
    .await;
    assert_eq!(ok_struct.status, StatusCode::OK, "{}", ok_struct.text());

    let head = ds.store.head_commit().seq;
    // Wrong valueDigest -> 422 span-mismatch.
    let bad_digest = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "references": [{
                "key": "s-bad",
                "occurrenceKey": "s-bad",
                "structured": {
                    "revision": {"iri": rev_json},
                    "pointer": "/score",
                    "method": norm,
                    "valueDigest": sha("1")
                }
            }]
        }),
    )
    .await;
    assert_eq!(
        bad_digest.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        bad_digest.text()
    );
    assert_eq!(bad_digest.json()["code"], "span-mismatch");

    // Existing non-JSON revision -> 422 span-mismatch.
    let non_json = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "references": [{
                "key": "s-plain",
                "occurrenceKey": "s-plain",
                "structured": {
                    "revision": {"iri": rev_plain},
                    "pointer": "",
                    "method": norm,
                    "valueDigest": sha(plain_doc)
                }
            }]
        }),
    )
    .await;
    assert_eq!(
        non_json.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        non_json.text()
    );
    assert_eq!(non_json.json()["code"], "span-mismatch");

    // Existing metadata-only revision -> 422 evidence-unavailable, matching missing byte for byte.
    let meta_only = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "references": [{
                "key": "s-meta",
                "occurrenceKey": "s-meta",
                "structured": {
                    "revision": {"iri": rev_meta},
                    "pointer": "/score",
                    "method": norm,
                    "valueDigest": sha("1.000")
                }
            }]
        }),
    )
    .await;
    assert_eq!(
        meta_only.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        meta_only.text()
    );
    assert_eq!(meta_only.json()["code"], "evidence-unavailable");
    let missing_meta = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "references": [{
                "key": "s-meta",
                "occurrenceKey": "s-meta",
                "structured": {
                    "revision": {"iri": missing_iri},
                    "pointer": "/score",
                    "method": norm,
                    "valueDigest": sha("1.000")
                }
            }]
        }),
    )
    .await;
    assert_eq!(
        scrub(&meta_only, &rev_meta),
        scrub(&missing_meta, missing_iri)
    );

    // Malformed RFC 6901 pointer against an existing JSON revision -> 400 invalid-proposal
    // (never 422 span-mismatch).
    let bad_ptr = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "references": [{
                "key": "s-bad-ptr",
                "occurrenceKey": "s-bad-ptr",
                "structured": {
                    "revision": {"iri": rev_json},
                    "pointer": "/bad~2",
                    "method": norm,
                    "valueDigest": sha("1.000")
                }
            }]
        }),
    )
    .await;
    assert_eq!(
        bad_ptr.status,
        StatusCode::BAD_REQUEST,
        "{}",
        bad_ptr.text()
    );
    assert_eq!(bad_ptr.json()["code"], "invalid-proposal");

    // Unsupported normalization method -> 422 incomplete-evidence.
    let unsupported = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "references": [{
                "key": "s-unsup",
                "occurrenceKey": "s-unsup",
                "structured": {
                    "revision": {"iri": rev_json},
                    "pointer": "/score",
                    "method": {"method": "urn:example:custom-norm", "version": "1"},
                    "valueDigest": sha("1.000")
                }
            }]
        }),
    )
    .await;
    assert_eq!(
        unsupported.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        unsupported.text()
    );
    assert_eq!(unsupported.json()["code"], "incomplete-evidence");

    // Unauthorized caller on existing JSON revision matches missing byte for byte.
    let unauth_struct = |r_iri: &str| {
        serde_json::json!({
            "references": [{
                "key": "s-unauth",
                "occurrenceKey": "s-unauth",
                "structured": {
                    "revision": {"iri": r_iri},
                    "pointer": "/score",
                    "method": norm,
                    "valueDigest": sha("1.000")
                }
            }]
        })
    };
    let hidden_s = propose(&s, &b("evp"), &unauth_struct(&rev_json)).await;
    let missing_s = propose(&s, &b("evp"), &unauth_struct(missing_iri)).await;
    assert_eq!(
        hidden_s.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        hidden_s.text()
    );
    assert_eq!(hidden_s.json()["code"], "evidence-unavailable");
    assert_eq!(scrub(&hidden_s, &rev_json), scrub(&missing_s, missing_iri));
    assert_eq!(ds.store.head_commit().seq, head);

    // Detaching and re-protecting invalidates the existing passage -> 422 origin-not-reproduced.
    sparkles::evidence::detach(&ds.dataset, "urn:ev:sources").unwrap();
    let settings = sparkles::evidence::Settings {
        enabled: true,
        source_records: vec!["urn:ev:sources".into()],
        proposals: vec!["urn:ev:claims".into()],
        ..Default::default()
    };
    sparkles::evidence::configure(&ds.dataset, &settings, &sparkles::task::Control::none())
        .unwrap();
    let stale_pass = propose(
        &s,
        &b("evw"),
        &serde_json::json!({
            "references": [{
                "key": "refStale",
                "occurrenceKey": "ref-stale",
                "passage": {
                    "revision": {"iri": rev_text},
                    "rendition": {"iri": rend_text},
                    "passage": {"iri": pass1}
                }
            }]
        }),
    )
    .await;
    assert_eq!(
        stale_pass.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        stale_pass.text()
    );
    assert_eq!(stale_pass.json()["code"], "origin-not-reproduced");
}

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

#[tokio::test]
async fn graphrag_disclosure_mcp_pins_and_graph_cache_enforce_support_visibility() {
    use sparkles::evidence::{
        self as ev_api, ClaimInput, ClaimKind, ConflictMode, DecisionInput, Disposition,
        EvidenceInput, Polarity, Policy, ProposeOptions, RecordKey, RecordRef, RecordVersion,
        ReviewOptions, Settings, Sha256Digest, SourceContent, SourceRevisionInput,
    };
    use sparkles::task::Control;

    const SOURCES_A: &str = "urn:ev:sources-a";
    const CLAIMS_A: &str = "urn:ev:claims-a";
    const DECISIONS: &str = "urn:ev:decisions";
    const SUPPORTS: &str = "urn:ev:supports";
    const ACCEPTED: &str = "urn:ev:accepted";

    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    let s = build(Fixture {
        extra: format!(
            r#"
[[users]]
name = "rout"
password = "{rout}"
[[users.grants]]
dataset = "ev"
level = "read"
graphs = ["{ACCEPTED}", "urn:x-sparkles:graphrag:*"]

[[users]]
name = "ra"
password = "{ra}"
[[users.grants]]
dataset = "ev"
level = "read"
graphs = ["{ACCEPTED}", "{DECISIONS}", "{SUPPORTS}", "{CLAIMS_A}", "{SOURCES_A}", "urn:x-sparkles:graphrag:*"]
[[users.grants]]
dataset = "evlive"
level = "read"
graphs = ["{ACCEPTED}", "{DECISIONS}", "{SUPPORTS}", "{CLAIMS_A}", "{SOURCES_A}"]
"#,
            rout = h("rout-pw"),
            ra = h("ra-pw"),
        ),
        ..Default::default()
    });

    let ds = s.state.attach("ev", DbType::Memory, None).unwrap();
    ds.store
        .set_retention(sparkles::history::Retention {
            keep_commits: Some(64),
            ..Default::default()
        })
        .unwrap();
    ds.store
        .activate_memory_configuration_history(
            &sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap(),
        )
        .unwrap();

    let settings = Settings {
        enabled: true,
        source_records: vec![SOURCES_A.into()],
        proposals: vec![CLAIMS_A.into()],
        decisions: vec![DECISIONS.into()],
        supports: vec![SUPPORTS.into()],
        projections: vec![ACCEPTED.into()],
        policies: [(
            "main".to_string(),
            Policy {
                version: 1,
                admitted_kinds: vec![ClaimKind::Reported],
                required_scope: Vec::new(),
                conflicts: ConflictMode::AdmitLabelled,
                boundary: vec![SOURCES_A.into(), CLAIMS_A.into()],
                decisions: DECISIONS.into(),
                supports: SUPPORTS.into(),
                output: ACCEPTED.into(),
            },
        )]
        .into_iter()
        .collect(),
        ..Settings::default()
    };
    ev_api::configure(&ds.dataset, &settings, &Control::none()).unwrap();

    let iri = |u: &str| oxrdf::NamedNode::new(u).unwrap();
    let sha = |t: &str| Sha256Digest::of_bytes(t.as_bytes(), &Control::none()).unwrap();
    let all = || Some(Arc::new(sparkles::access::GraphAccess::all()));
    let prop = oxrdf::Triple::new(
        iri("urn:release"),
        iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        iri("urn:Release"),
    );
    let input = EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: RecordKey::new("rev-a").unwrap(),
            source: iri("urn:source:a"),
            source_key: RecordKey::new("src-a").unwrap(),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: sha("src-a"),
            },
            metadata: Default::default(),
            times: Default::default(),
            lineage: Default::default(),
        }],
        claims: vec![ClaimInput {
            version: RecordVersion::V1,
            key: RecordKey::new("claim-a").unwrap(),
            occurrence_key: RecordKey::new("c1").unwrap(),
            revision: RecordRef::Local(RecordKey::new("rev-a").unwrap()),
            proposition: prop,
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: iri("urn:author"),
            scope: Default::default(),
            confidence: Default::default(),
            validity: None,
        }],
        ..Default::default()
    };
    let mut po = ProposeOptions::new(iri("urn:importer"));
    po.source_graph = Some(iri(SOURCES_A));
    po.proposal_graph = Some(iri(CLAIMS_A));
    let p = ev_api::propose(
        None,
        &ds.dataset,
        &ds.dataset,
        &input,
        &po,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let rec = p.records.iter().find(|r| r.key == "claim-a").unwrap();
    let mut ro = ReviewOptions::new(iri("urn:reviewer"));
    ro.if_head = Some(ds.store.head_commit().seq);
    ev_api::review(
        None,
        &ds.dataset,
        &ds.dataset,
        "main",
        1,
        &[DecisionInput {
            key: "d1".into(),
            target: rec.iri.clone(),
            target_digest: rec.digest,
            disposition: Disposition::Accepted,
            supersedes: None,
        }],
        &ro,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();

    // 1. GraphRAG disclosure guard: write a persisted report & footprint covering ACCEPTED.
    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
    let snap = ds.store.snapshot();
    let fp = crate::graphrag::guard::capture(&crate::graphrag::guard::CaptureInput {
        dataset: "ev",
        branch: "main",
        source_commit: snap.commit,
        principal: "alice",
        caller: &sparkles::access::triples::Caller::default(),
        endpoint: "graphrag",
        view: None,
        graphs: &[ACCEPTED.to_string()],
        reports: &["urn:report:1".to_string()],
        snap: Some(snap.as_ref()),
        review_policy: false,
    })
    .unwrap();
    {
        let mut t = ds.store.write();
        let mut labels = std::collections::HashMap::new();
        for q in fp.quads(REPORTS) {
            let ids = t.encode_quad(&q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        }
        let rq = oxrdf::Quad::new(
            iri("urn:report:1"),
            iri(&crate::graphrag::guard::gr("summary")),
            oxrdf::Literal::new_simple_literal("secret projected release summary"),
            oxrdf::GraphName::NamedNode(iri(REPORTS)),
        );
        let dq = oxrdf::Quad::new(
            iri("urn:report:1"),
            iri(&crate::graphrag::guard::gr("disclosure")),
            iri(&fp.iri()),
            oxrdf::GraphName::NamedNode(iri(REPORTS)),
        );
        for q in [rq, dq] {
            let ids = t.encode_quad(&q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        }
        t.commit().unwrap();
    }

    // Admin (`alice`) sees the report; restricted `rout` (who has grant on ACCEPTED + reports) must NOT!
    let report_q = format!(
        "SELECT ?sum WHERE {{ GRAPH <{REPORTS}> {{ <urn:report:1> <{}> ?sum }} }}",
        crate::graphrag::guard::gr("summary")
    );
    let admin_rep = call(
        &s.app,
        "POST",
        "/ev/sparql",
        &[
            ("authorization", &b("alice")),
            ("content-type", "application/sparql-query"),
            ("accept", "application/sparql-results+json"),
        ],
        &report_q,
    )
    .await;
    assert_eq!(admin_rep.status, StatusCode::OK, "{}", admin_rep.text());
    assert!(
        admin_rep
            .text()
            .contains("secret projected release summary")
    );

    let rout_rep = call(
        &s.app,
        "POST",
        "/ev/sparql",
        &[
            ("authorization", &b("rout")),
            ("content-type", "application/sparql-query"),
            ("accept", "application/sparql-results+json"),
        ],
        &report_q,
    )
    .await;
    assert_eq!(rout_rep.status, StatusCode::OK, "{}", rout_rep.text());
    assert!(
        !rout_rep.text().contains("secret projected release summary"),
        "GraphRAG report over review-policy output graph leaked to restricted reader: {}",
        rout_rep.text()
    );

    // Controlled guard path must also refuse exemption for `rout`.
    let snap = ds.store.snapshot();
    let rout_view = sparkles::access::GraphAccess::graphs(
        sparkles::access::Graphs::Only(sparkles::access::GraphRule::new(
            [ACCEPTED, "urn:x-sparkles:graphrag:*"],
            &[],
        )),
        sparkles::access::Graphs::none(),
    );
    let guarded = crate::graphrag::guard::apply(rout_view, Some(&s.state.graphrag_guard));
    let prep = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    let prepared = guarded.masked_controlled(&snap, &prep).unwrap();
    assert!(
        !prepared.allows_read_iri(REPORTS),
        "controlled GraphRAG guard must not exempt reports over review-policy output graph"
    );

    // 2. HTTP /$/graph/{ds}/groups and /$/graph/{ds}/facets caches must invalidate after
    //    detach + re-configure at same commit.
    let groups_req = || {
        let app = s.app.clone();
        async move {
            let r = call(
                &app,
                "POST",
                &format!("/$/graph/ev/groups?default-graph-uri={ACCEPTED}"),
                &[
                    ("authorization", &b("ra")),
                    ("content-type", "application/json"),
                ],
                &serde_json::json!({
                    "within": { "class": "urn:Release" },
                })
                .to_string(),
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()
        }
    };
    let facets_req = || {
        let app = s.app.clone();
        async move {
            let r = call(
                &app,
                "POST",
                &format!("/$/graph/ev/facets?default-graph-uri={ACCEPTED}"),
                &[
                    ("authorization", &b("ra")),
                    ("content-type", "application/json"),
                ],
                &serde_json::json!({
                    "within": { "class": "urn:Release" },
                    "candidates": true,
                })
                .to_string(),
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()
        }
    };
    let g_before = groups_req().await;
    assert_eq!(
        g_before["within"]["size"], 1,
        "expected urn:release member before detach: {g_before}"
    );
    let f_before = facets_req().await;
    assert_eq!(
        f_before["size"], 1,
        "expected urn:release in facets before detach: {f_before}"
    );
    // Persisted Ask answer access captured while policy is active must be withheld from
    // restricted `rout` both before AND after policy removal/detach, while visible to `alice`.
    let ask_snap_before = ds.store.snapshot();
    let ask_access = crate::assistant::answer_access(
        Some(&ask_snap_before),
        Some(&Arc::new(sparkles::access::GraphAccess::graphs(
            sparkles::access::Graphs::Only(sparkles::access::GraphRule::new([ACCEPTED], &[])),
            sparkles::access::Graphs::none(),
        ))),
        &sparkles::access::triples::Caller::default(),
        "ev",
        None,
        "rout",
    );
    let rout_only_view = Arc::new(sparkles::access::GraphAccess::graphs(
        sparkles::access::Graphs::Only(sparkles::access::GraphRule::new([ACCEPTED], &[])),
        sparkles::access::Graphs::none(),
    ));
    assert!(
        !crate::assistant::answer_visible(
            Some(&ask_access),
            Some(&rout_only_view),
            &ask_snap_before
        ),
        "persisted Ask answer over review-policy output graph must be hidden from restricted reader"
    );
    assert!(
        crate::assistant::answer_visible(Some(&ask_access), None, &ask_snap_before),
        "unrestricted reader must still see persisted Ask answer"
    );

    // Detach and re-protect ACCEPTED without advancing commit sequence.
    ds.store
        .detach_managed(sparkles::managed::OwnerKind::Evidence, ACCEPTED)
        .unwrap();
    ev_api::configure(&ds.dataset, &settings, &Control::none()).unwrap();
    let g_after = groups_req().await;
    assert_eq!(
        g_after["within"]["size"], 0,
        "stale groups cache returned urn:release after detach at same commit: {g_after}"
    );
    let f_after = facets_req().await;
    assert_eq!(
        f_after["size"], 0,
        "stale facets cache returned urn:release after detach at same commit: {f_after}"
    );

    // 3. MCP Pins: warm vs cold explicit At::Commit and At::Snapshot preserve historical config
    //    even after the live policy is removed.
    let commit_with_policy = ds.store.head_commit().seq;
    let ev_view = ds
        .store
        .evidence_view(
            &sparkles::history::At::Commit(commit_with_policy),
            &sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap(),
        )
        .unwrap();
    ds.store
        .create_bound_snapshot("snap-policy", &ev_view, &Default::default())
        .unwrap();
    let pins = crate::mcp::pins::Pins::default();
    let ho = sparkles::history::HistoryOptions::default();
    let map_err =
        |e: sparkles::error::Error| crate::mcp::errors::ToolError::new("err", 500, e.to_string());
    // Warm the MCP pin table at `At::Head` AND at explicit `At::Commit(commit_with_policy)`.
    let _ = pins.resolve(&ds, None, &ho, &map_err).unwrap();
    let warm_commit = pins
        .resolve(
            &ds,
            Some(&sparkles::history::At::Commit(commit_with_policy)),
            &ho,
            &map_err,
        )
        .unwrap();

    // Remove the review policy from live settings (detaching ACCEPTED first).
    ds.store
        .detach_managed(sparkles::managed::OwnerKind::Evidence, ACCEPTED)
        .unwrap();
    let mut removed = settings.clone();
    removed.policies.clear();
    removed.projections.clear();
    ev_api::configure(&ds.dataset, &removed, &Control::none()).unwrap();

    // Even AFTER the source review policy is removed, the persisted GraphRAG report
    // captured under that policy (and persisted Ask answer) must remain hidden from `rout`!
    let rout_rep_after = call(
        &s.app,
        "POST",
        "/ev/sparql",
        &[
            ("authorization", &b("rout")),
            ("content-type", "application/sparql-query"),
            ("accept", "application/sparql-results+json"),
        ],
        &report_q,
    )
    .await;
    assert_eq!(
        rout_rep_after.status,
        StatusCode::OK,
        "{}",
        rout_rep_after.text()
    );
    assert!(
        !rout_rep_after
            .text()
            .contains("secret projected release summary"),
        "persisted GraphRAG report leaked to restricted reader after policy removal: {}",
        rout_rep_after.text()
    );
    let snap_after_removal = ds.store.snapshot();
    let prepared_after = guarded
        .masked_controlled(&snap_after_removal, &prep)
        .unwrap();
    assert!(
        !prepared_after.allows_read_iri(REPORTS),
        "controlled GraphRAG guard must keep report unproven after policy removal"
    );
    assert!(
        !crate::assistant::answer_visible(
            Some(&ask_access),
            Some(&rout_only_view),
            &snap_after_removal
        ),
        "persisted Ask answer must remain hidden from restricted reader after policy removal"
    );
    // Legacy v1 footprint (without evidence provenance) must also remain unproven for restricted
    // readers while still proven for unrestricted readers.
    let mut legacy_fp = fp.clone();
    legacy_fp.version = 1;
    legacy_fp.evidence_gated = false;
    for g in &mut legacy_fp.graphs {
        g.support_gated = false;
    }
    assert_eq!(
        crate::graphrag::guard::covers(Some(&rout_only_view), &legacy_fp),
        crate::graphrag::guard::Decision::Unproven
    );
    assert_eq!(
        crate::graphrag::guard::covers(None, &legacy_fp),
        crate::graphrag::guard::Decision::Proven
    );

    // Both warm and cold explicit At::Commit and At::Snapshot selections must still hide
    // ACCEPTED from `rout` (who has only ACCEPTED grant).
    let warm_commit_again = pins
        .resolve(
            &ds,
            Some(&sparkles::history::At::Commit(commit_with_policy)),
            &ho,
            &map_err,
        )
        .unwrap();
    let cold_pins = crate::mcp::pins::Pins::default();
    let cold_commit = cold_pins
        .resolve(
            &ds,
            Some(&sparkles::history::At::Commit(commit_with_policy)),
            &ho,
            &map_err,
        )
        .unwrap();
    let named_snap = pins
        .resolve(
            &ds,
            Some(&sparkles::history::At::Snapshot("snap-policy".into())),
            &ho,
            &map_err,
        )
        .unwrap();
    let rout_only = Arc::new(sparkles::access::GraphAccess::graphs(
        sparkles::access::Graphs::Only(sparkles::access::GraphRule::new([ACCEPTED], &[])),
        sparkles::access::Graphs::none(),
    ));
    for (label, snap) in [
        ("warm-first", warm_commit),
        ("warm-again", warm_commit_again),
        ("cold", cold_commit),
        ("named-pin", named_snap),
    ] {
        let res = sparkles::sparql::query(
            snap,
            &format!("ASK {{ GRAPH <{ACCEPTED}> {{ ?s ?p ?o }} }}"),
            &sparkles::sparql::QueryOptions {
                graphs: Some(rout_only.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            !res.boolean,
            "MCP pin selection ({label}) leaked projected triple after live policy removal"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn production_graphrag_build_ask_history_and_live_sse_evidence_visibility() {
    use crate::models::mock::{self, MockModel};
    use crate::models::{Models, ModelsConfig};
    use sparkles::evidence::{
        self as ev_api, ClaimInput, ClaimKind, ConflictMode, DecisionInput, Disposition,
        EvidenceInput, Polarity, Policy, ProposeOptions, RecordKey, RecordRef, RecordVersion,
        ReviewOptions, Settings, Sha256Digest, SourceContent, SourceRevisionInput,
    };
    use sparkles::task::Control;
    use std::sync::atomic::{AtomicBool, Ordering};

    const SOURCES_A: &str = "urn:ev:sources-a";
    const CLAIMS_A: &str = "urn:ev:claims-a";
    const DECISIONS: &str = "urn:ev:decisions";
    const SUPPORTS: &str = "urn:ev:supports";
    const ACCEPTED: &str = "urn:ev:accepted";
    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";

    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    let rout_hash = h("rout-pw");
    let ra_hash = h("ra-pw");
    let evnarrow_hash = h("evnarrow-pw");
    let make_extra = |narrow_evnarrow: bool| -> String {
        let evnarrow_graphs = if narrow_evnarrow {
            format!(
                r#"graphs = ["default", "{ACCEPTED}", "{DECISIONS}", "{SUPPORTS}", "{CLAIMS_A}", "{SOURCES_A}"]"#
            )
        } else {
            String::new()
        };
        format!(
            r#"
[[users]]
name = "rout"
password = "{rout_hash}"
[[users.grants]]
dataset = "ev"
level = "read"
graphs = ["{ACCEPTED}", "urn:x-sparkles:graphrag:*"]

[[users]]
name = "ra"
password = "{ra_hash}"
[[users.grants]]
dataset = "ev"
level = "read"
graphs = ["{ACCEPTED}", "{DECISIONS}", "{SUPPORTS}", "{CLAIMS_A}", "{SOURCES_A}", "urn:x-sparkles:graphrag:*"]
[[users.grants]]
dataset = "evask1"
level = "read"
graphs = ["{ACCEPTED}", "{DECISIONS}", "{SUPPORTS}", "{CLAIMS_A}", "{SOURCES_A}"]
[[users.grants]]
dataset = "evask2"
level = "read"
graphs = ["{ACCEPTED}", "{DECISIONS}", "{SUPPORTS}", "{CLAIMS_A}", "{SOURCES_A}"]
[[users.grants]]
dataset = "evlive"
level = "read"
graphs = ["{ACCEPTED}", "{DECISIONS}", "{SUPPORTS}", "{CLAIMS_A}", "{SOURCES_A}"]

[[users]]
name = "evnarrow"
password = "{evnarrow_hash}"
[[users.grants]]
dataset = "evask_hist"
level = "read"
{evnarrow_graphs}
"#
        )
    };
    let s = build(Fixture {
        extra: make_extra(false),
        ..Default::default()
    });
    let ds = s.state.attach("ev", DbType::Memory, None).unwrap();
    ds.store
        .activate_memory_configuration_history(
            &sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap(),
        )
        .unwrap();

    let settings = Settings {
        enabled: true,
        source_records: vec![SOURCES_A.into()],
        proposals: vec![CLAIMS_A.into()],
        decisions: vec![DECISIONS.into()],
        supports: vec![SUPPORTS.into()],
        projections: vec![ACCEPTED.into()],
        policies: [(
            "main".to_string(),
            Policy {
                version: 1,
                admitted_kinds: vec![ClaimKind::Reported],
                required_scope: Vec::new(),
                conflicts: ConflictMode::AdmitLabelled,
                boundary: vec![SOURCES_A.into(), CLAIMS_A.into()],
                decisions: DECISIONS.into(),
                supports: SUPPORTS.into(),
                output: ACCEPTED.into(),
            },
        )]
        .into_iter()
        .collect(),
        ..Settings::default()
    };
    let mut cleared = settings.clone();
    cleared.policies.clear();
    cleared.projections.clear();

    ev_api::configure(&ds.dataset, &settings, &Control::none()).unwrap();

    let iri = |u: &str| oxrdf::NamedNode::new(u).unwrap();
    let sha = |t: &str| Sha256Digest::of_bytes(t.as_bytes(), &Control::none()).unwrap();
    let all = || Some(Arc::new(sparkles::access::GraphAccess::all()));

    // Propose & accept 3 connected facts in ACCEPTED so GraphRAG community detection forms a community.
    let triples = [
        (
            "c1",
            oxrdf::Triple::new(
                iri("urn:release:a"),
                iri("http://ex/knows"),
                iri("urn:release:b"),
            ),
        ),
        (
            "c2",
            oxrdf::Triple::new(
                iri("urn:release:b"),
                iri("http://ex/knows"),
                iri("urn:release:c"),
            ),
        ),
        (
            "c3",
            oxrdf::Triple::new(
                iri("urn:release:c"),
                iri("http://ex/knows"),
                iri("urn:release:a"),
            ),
        ),
    ];
    let claims: Vec<ClaimInput> = triples
        .iter()
        .map(|(k, prop)| ClaimInput {
            version: RecordVersion::V1,
            key: RecordKey::new(format!("claim-{k}")).unwrap(),
            occurrence_key: RecordKey::new(*k).unwrap(),
            revision: RecordRef::Local(RecordKey::new("rev-a").unwrap()),
            proposition: prop.clone(),
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: iri("urn:author"),
            scope: Default::default(),
            confidence: Default::default(),
            validity: None,
        })
        .collect();
    let input = EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: RecordKey::new("rev-a").unwrap(),
            source: iri("urn:source:a"),
            source_key: RecordKey::new("src-a").unwrap(),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: sha("src-a"),
            },
            metadata: Default::default(),
            times: Default::default(),
            lineage: Default::default(),
        }],
        claims,
        ..Default::default()
    };
    let mut po = ProposeOptions::new(iri("urn:importer"));
    po.source_graph = Some(iri(SOURCES_A));
    po.proposal_graph = Some(iri(CLAIMS_A));
    let p = ev_api::propose(
        None,
        &ds.dataset,
        &ds.dataset,
        &input,
        &po,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let decisions: Vec<DecisionInput> = p
        .records
        .iter()
        .filter(|r| r.key.starts_with("claim-"))
        .enumerate()
        .map(|(i, rec)| DecisionInput {
            key: format!("d{i}"),
            target: rec.iri.clone(),
            target_digest: rec.digest,
            disposition: Disposition::Accepted,
            supersedes: None,
        })
        .collect();
    let mut ro = ReviewOptions::new(iri("urn:reviewer"));
    ro.if_head = Some(ds.store.head_commit().seq);
    ev_api::review(
        None,
        &ds.dataset,
        &ds.dataset,
        "main",
        1,
        &decisions,
        &ro,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();

    // Shared toggle + target dataset so the mock model can remove the active evidence policy
    // mid-operation (same commit!) on whichever dataset is under test.
    let remove_mid_call = Arc::new(AtomicBool::new(false));
    let remove_target: Arc<parking_lot::Mutex<Arc<crate::state::Dataset>>> =
        Arc::new(parking_lot::Mutex::new(ds.clone()));
    let target_hook = remove_target.clone();
    let cleared_hook = cleared.clone();
    let remove_flag = remove_mid_call.clone();
    let mock = MockModel::start(move |r, _| {
        let body = r.body.to_string();
        let is_summary = body.contains("rows of a query result");
        let is_community = body.contains("community") && body.contains("findings");
        if is_community {
            if remove_flag.swap(false, Ordering::AcqRel) {
                let target = target_hook.lock().clone();
                target
                    .store
                    .detach_managed(sparkles::managed::OwnerKind::Evidence, ACCEPTED)
                    .unwrap();
                ev_api::configure(&target.dataset, &cleared_hook, &Control::none()).unwrap();
            }
            return (
                200,
                mock::openai(
                    &serde_json::json!({
                        "title": "Release community",
                        "summary": "Secret projected graphrag build summary.",
                        "findings": [{ "text": "Connected releases.", "entities": [1], "cites": [1] }],
                        "rating": 5
                    })
                    .to_string(),
                ),
            );
        }
        if is_summary {
            if remove_flag.swap(false, Ordering::AcqRel) {
                let target = target_hook.lock().clone();
                target
                    .store
                    .detach_managed(sparkles::managed::OwnerKind::Evidence, ACCEPTED)
                    .unwrap();
                ev_api::configure(&target.dataset, &cleared_hook, &Control::none()).unwrap();
            }
            return (
                200,
                mock::openai(
                    &serde_json::json!({
                        "text": "Secret projected ask answer [1].",
                        "citations": [1]
                    })
                    .to_string(),
                ),
            );
        }
        let mut draft = serde_json::json!({
            "query": format!("SELECT ?s ?o WHERE {{ GRAPH <{ACCEPTED}> {{ ?s <http://ex/knows> ?o }} }}"),
            "explanation": "Lists known links in the accepted projection graph.",
            "assumptions": [],
            "clarify": { "question": "", "choices": [] },
            "graph": { "subject": "", "predicate": "", "object": "" }
        });
        if body.contains("ScopedDraft") {
            draft["scope"] = serde_json::json!("local");
        }
        (200, mock::openai(&draft.to_string()))
    });

    let cfg = serde_json::json!({
        "providers": {
            "p": {
                "kind": "openai",
                "endpoint": mock.url(),
                "requestTimeoutSecs": 5,
                "allowedModels": ["m"],
                "structuredOutput": "json-schema"
            }
        },
        "roles": {
            "community": [{ "provider": "p", "model": "m" }],
            "draft": [{ "provider": "p", "model": "m" }],
            "summarize": [{ "provider": "p", "model": "m" }]
        }
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

    // Enable assistant with send: "rows" so both GraphRAG builds and Ask summaries run.
    let assist_put = call(
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
    assert_eq!(assist_put.status, StatusCode::OK, "{}", assist_put.text());

    // 1. Production GraphRAG build over ACCEPTED (ordinary build without explicit reviewPolicy).
    let gr_settings: crate::graphrag::settings::GraphragSettings =
        serde_json::from_value(serde_json::json!({
            "enabled": true,
            "principal": "alice",
            "projection": { "graphs": [ACCEPTED] },
            "levels": 1,
            "minCommunitySize": 2
        }))
        .unwrap();
    crate::settings::store_runtime(
        &s.state,
        &ds,
        &crate::settings::GRAPHRAG,
        &serde_json::to_value(&gr_settings).unwrap(),
    )
    .unwrap();

    // 1a. Mid-build same-commit policy removal must cause the production build to refuse
    //     rather than publishing an unflagged footprint!
    remove_mid_call.store(true, Ordering::Release);
    let st_ref = s.state.clone();
    let ds_ref = ds.clone();
    let gs_ref = gr_settings.clone();
    let mid_res = tokio::task::spawn_blocking(move || {
        crate::graphrag::build::run(&crate::graphrag::build::Run {
            st: &st_ref,
            ds: ds_ref,
            settings: gs_ref,
            mode: crate::graphrag::build::Mode::Build,
            confirmed: true,
            task: None,
            task_id: "ev-mid-remove".into(),
            cancel: Arc::new(AtomicBool::new(false)),
            limits: Default::default(),
        })
    })
    .await
    .unwrap();
    assert!(
        matches!(mid_res, Err(crate::graphrag::build::RunError::Refused(_))),
        "same-commit policy removal during GraphRAG build must refuse publication, got {mid_res:?}"
    );

    // 1b. Re-enable policy and run a normal production build to completion: it persists a v2
    //     footprint with `evidence_gated = true` that stays hidden from `rout` even after
    //     subsequent policy removal.
    ev_api::configure(&ds.dataset, &settings, &Control::none()).unwrap();
    let st_ref = s.state.clone();
    let ds_ref = ds.clone();
    let gs_ref = gr_settings.clone();
    let ok_build = tokio::task::spawn_blocking(move || {
        crate::graphrag::build::run(&crate::graphrag::build::Run {
            st: &st_ref,
            ds: ds_ref,
            settings: gs_ref,
            mode: crate::graphrag::build::Mode::Build,
            confirmed: true,
            task: None,
            task_id: "ev-full-build".into(),
            cancel: Arc::new(AtomicBool::new(false)),
            limits: Default::default(),
        })
    })
    .await
    .unwrap()
    .unwrap();
    assert!(
        ok_build.written > 0,
        "expected written community report: {ok_build:?}"
    );

    // Remove the review policy AFTER the build, then query REPORTS as `alice` and `rout`.
    ds.store
        .detach_managed(sparkles::managed::OwnerKind::Evidence, ACCEPTED)
        .unwrap();
    ev_api::configure(&ds.dataset, &cleared, &Control::none()).unwrap();

    let report_q = format!("SELECT ?s ?o WHERE {{ GRAPH <{REPORTS}> {{ ?s ?p ?o }} }}");
    let alice_rep = call(
        &s.app,
        "POST",
        "/ev/sparql",
        &[
            ("authorization", &b("alice")),
            ("content-type", "application/sparql-query"),
            ("accept", "application/sparql-results+json"),
        ],
        &report_q,
    )
    .await;
    assert_eq!(alice_rep.status, StatusCode::OK, "{}", alice_rep.text());
    assert!(
        alice_rep
            .text()
            .contains("Secret projected graphrag build summary."),
        "admin must see built GraphRAG report: {}",
        alice_rep.text()
    );
    let rout_rep = call(
        &s.app,
        "POST",
        "/ev/sparql",
        &[
            ("authorization", &b("rout")),
            ("content-type", "application/sparql-query"),
            ("accept", "application/sparql-results+json"),
        ],
        &report_q,
    )
    .await;
    assert_eq!(rout_rep.status, StatusCode::OK, "{}", rout_rep.text());
    assert!(
        !rout_rep
            .text()
            .contains("Secret projected graphrag build summary."),
        "production GraphRAG build report leaked to restricted reader after policy removal: {}",
        rout_rep.text()
    );

    // 2. Production HTTP Ask + persisted `GET /$/asks/<ds>` retrieval.
    //    Give each Ask phase a fresh dataset with fresh authenticated acceptance receipts so
    //    restricted `ra`'s SELECT actually returns projected rows and invokes summarization.
    let setup_ask_ds = |name: &str| -> Arc<crate::state::Dataset> {
        let d = s.state.attach(name, DbType::Memory, None).unwrap();
        d.store
            .activate_memory_configuration_history(
                &sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap(),
            )
            .unwrap();
        ev_api::configure(&d.dataset, &settings, &Control::none()).unwrap();
        let p = ev_api::propose(
            None,
            &d.dataset,
            &d.dataset,
            &EvidenceInput {
                revisions: vec![SourceRevisionInput {
                    version: RecordVersion::V1,
                    key: RecordKey::new("rev-ask").unwrap(),
                    source: iri("urn:source:ask"),
                    source_key: RecordKey::new("src-ask").unwrap(),
                    upstream_version: None,
                    content: SourceContent::MetadataOnly {
                        digest: sha("src-ask"),
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                }],
                claims: vec![ClaimInput {
                    version: RecordVersion::V1,
                    key: RecordKey::new("claim-ask").unwrap(),
                    occurrence_key: RecordKey::new("cask").unwrap(),
                    revision: RecordRef::Local(RecordKey::new("rev-ask").unwrap()),
                    proposition: oxrdf::Triple::new(
                        iri("urn:ask:a"),
                        iri("http://ex/knows"),
                        iri("urn:ask:b"),
                    ),
                    kind: ClaimKind::Reported,
                    polarity: Polarity::Positive,
                    attributed_author: iri("urn:author"),
                    scope: Default::default(),
                    confidence: Default::default(),
                    validity: None,
                }],
                ..Default::default()
            },
            &po,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        let rec = p.records.iter().find(|r| r.key == "claim-ask").unwrap();
        let mut ro_ask = ReviewOptions::new(iri("urn:reviewer"));
        ro_ask.if_head = Some(d.store.head_commit().seq);
        ev_api::review(
            None,
            &d.dataset,
            &d.dataset,
            "main",
            1,
            &[DecisionInput {
                key: "dask".into(),
                target: rec.iri.clone(),
                target_digest: rec.digest,
                disposition: Disposition::Accepted,
                supersedes: None,
            }],
            &ro_ask,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        d
    };

    // 2a. Restricted `ra` asks while policy is active (and `alice` also asks), then remove policy
    //     AFTER Ask completes -> `GET /$/asks/evask1` for restricted `ra` must return
    //     `answerWithheld: true` with no answer text, while `alice` sees her stored answer.
    let ds_ask1 = setup_ask_ds("evask1");
    let assist_put1 = call(
        &s.app,
        "PUT",
        "/$/assistant/evask1",
        &[
            ("authorization", &b("alice")),
            ("content-type", "application/json"),
        ],
        &serde_json::json!({ "enabled": true, "send": "rows" }).to_string(),
    )
    .await;
    assert_eq!(assist_put1.status, StatusCode::OK, "{}", assist_put1.text());

    let ask_alice = call(
        &s.app,
        "POST",
        "/evask1/ask",
        &[
            ("authorization", &b("alice")),
            ("content-type", "application/json"),
            ("accept", "application/json"),
        ],
        &serde_json::json!({ "question": "What releases know each other?", "drafts": 1, "shapeCheck": false }).to_string(),
    )
    .await;
    assert_eq!(ask_alice.status, StatusCode::OK, "{}", ask_alice.text());
    assert!(
        ask_alice.text().contains("Secret projected ask answer"),
        "alice ask response must contain data-derived summary: {}",
        ask_alice.text()
    );
    let ask_ra = call(
        &s.app,
        "POST",
        "/evask1/ask",
        &[
            ("authorization", &b("ra")),
            ("content-type", "application/json"),
            ("accept", "application/json"),
        ],
        &serde_json::json!({ "question": "What releases know each other?", "drafts": 1, "shapeCheck": false }).to_string(),
    )
    .await;
    assert_eq!(ask_ra.status, StatusCode::OK, "{}", ask_ra.text());
    assert!(
        ask_ra.text().contains("Secret projected ask answer"),
        "restricted ra ask response must see authenticated rows and contain data-derived summary: {}",
        ask_ra.text()
    );

    ds_ask1
        .store
        .detach_managed(sparkles::managed::OwnerKind::Evidence, ACCEPTED)
        .unwrap();
    ev_api::configure(&ds_ask1.dataset, &cleared, &Control::none()).unwrap();

    let asks_alice = call(
        &s.app,
        "GET",
        "/$/asks/evask1",
        &[("authorization", &b("alice"))],
        "",
    )
    .await;
    assert_eq!(asks_alice.status, StatusCode::OK, "{}", asks_alice.text());
    assert!(
        asks_alice.text().contains("Secret projected ask answer"),
        "admin must see stored ask answer: {}",
        asks_alice.text()
    );
    let asks_ra = call(
        &s.app,
        "GET",
        "/$/asks/evask1",
        &[("authorization", &b("ra"))],
        "",
    )
    .await;
    assert_eq!(asks_ra.status, StatusCode::OK, "{}", asks_ra.text());
    assert!(
        !asks_ra.text().contains("Secret projected ask answer"),
        "stored ask answer leaked to restricted reader after policy removal: {}",
        asks_ra.text()
    );
    assert_eq!(
        asks_ra.json()["asks"][0]["answerWithheld"],
        serde_json::json!(true),
        "expected answerWithheld: true for ra: {}",
        asks_ra.text()
    );

    // 2b. Same-commit policy removal DURING Ask (after SPARQL reads ACCEPTED, before finish
    //     captures the footprint) must genuinely run the summarize callback, consume the
    //     toggle, advance managed_generation at the same RDF commit, and withhold the
    //     footprint so `ra` cannot see its stored answer.
    let ds_ask2 = setup_ask_ds("evask2");
    let assist_put2 = call(
        &s.app,
        "PUT",
        "/$/assistant/evask2",
        &[
            ("authorization", &b("alice")),
            ("content-type", "application/json"),
        ],
        &serde_json::json!({ "enabled": true, "send": "rows" }).to_string(),
    )
    .await;
    assert_eq!(assist_put2.status, StatusCode::OK, "{}", assist_put2.text());
    *remove_target.lock() = ds_ask2.clone();
    let commit_before_ask = ds_ask2.store.head_commit().seq;
    let gen_before_ask = ds_ask2.store.managed_generation();
    remove_mid_call.store(true, Ordering::Release);
    let ask_mid = call(
        &s.app,
        "POST",
        "/evask2/ask",
        &[
            ("authorization", &b("ra")),
            ("content-type", "application/json"),
            ("accept", "application/json"),
        ],
        &serde_json::json!({ "question": "Mid-ask policy removal check?", "drafts": 1, "shapeCheck": false }).to_string(),
    )
    .await;
    assert_eq!(ask_mid.status, StatusCode::OK, "{}", ask_mid.text());
    assert!(
        ask_mid.text().contains("Secret projected ask answer"),
        "mid-ask response must contain data-derived summary (proving non-empty SELECT): {}",
        ask_mid.text()
    );
    assert!(
        !remove_mid_call.load(Ordering::Acquire),
        "summarize callback must have consumed remove_mid_call toggle"
    );
    assert_eq!(
        ds_ask2.store.head_commit().seq,
        commit_before_ask,
        "policy removal during ask must be same-commit"
    );
    assert!(
        ds_ask2.store.managed_generation() > gen_before_ask,
        "mid-ask policy removal must advance managed_generation"
    );
    let asks_ra_mid = call(
        &s.app,
        "GET",
        "/$/asks/evask2",
        &[("authorization", &b("ra"))],
        "",
    )
    .await;
    assert_eq!(asks_ra_mid.status, StatusCode::OK, "{}", asks_ra_mid.text());
    let asks_list = asks_ra_mid.json()["asks"].as_array().unwrap().clone();
    assert_eq!(
        asks_list.len(),
        1,
        "expected ask record for ra: {asks_list:?}"
    );
    assert_eq!(
        asks_list[0]["answerWithheld"],
        serde_json::json!(true),
        "mid-flight policy removal must withhold footprint from ra: {}",
        asks_list[0]
    );
    assert!(asks_list[0].get("answer").is_none());

    // 2c. Explicit historical selector `at: commit:<head>` on an activated family where the
    //     commit's historical binding has an active policy while the live policy was removed
    //     at the same RDF commit BEFORE the Ask began: the stable unrestricted Ask must
    //     withhold its footprint so narrowing the SAME account's grants (while still
    //     covering every recorded graph) withholds the saved answer on `GET /$/asks`.
    let ds_hist = setup_ask_ds("evask_hist");
    let assist_put_h = call(
        &s.app,
        "PUT",
        "/$/assistant/evask_hist",
        &[
            ("authorization", &b("alice")),
            ("content-type", "application/json"),
        ],
        &serde_json::json!({ "enabled": true, "send": "rows" }).to_string(),
    )
    .await;
    assert_eq!(
        assist_put_h.status,
        StatusCode::OK,
        "{}",
        assist_put_h.text()
    );
    let hist_commit = ds_hist.store.head_commit().seq;
    // Clear live policy at the SAME RDF commit `hist_commit` before the Ask starts.
    ds_hist
        .store
        .detach_managed(sparkles::managed::OwnerKind::Evidence, ACCEPTED)
        .unwrap();
    ev_api::configure(&ds_hist.dataset, &cleared, &Control::none()).unwrap();
    assert_eq!(ds_hist.store.head_commit().seq, hist_commit);
    assert!(ds_hist.store.snapshot().evidence_visibility_key().is_none());

    let gen_hist_before = ds_hist.store.managed_generation();
    let ask_hist = call(
        &s.app,
        "POST",
        "/evask_hist/ask",
        &[
            ("authorization", &basic("evnarrow", "evnarrow-pw")),
            ("content-type", "application/json"),
            ("accept", "application/json"),
        ],
        &serde_json::json!({
            "question": "What releases know each other at head commit?",
            "at": format!("commit:{hist_commit}"),
            "drafts": 1,
            "shapeCheck": false
        })
        .to_string(),
    )
    .await;
    assert_eq!(ask_hist.status, StatusCode::OK, "{}", ask_hist.text());
    assert!(
        ask_hist.text().contains("Secret projected ask answer"),
        "unrestricted explicit-commit Ask must contain data-derived summary: {}",
        ask_hist.text()
    );
    assert_eq!(ds_hist.store.head_commit().seq, hist_commit);
    assert_eq!(ds_hist.store.managed_generation(), gen_hist_before);

    // Narrow `evnarrow` from unrestricted to restricted (still covering EVERY graph in
    // `evask_hist`) and verify `GET /$/asks/evask_hist` withholds the saved answer.
    s.write_config(&Fixture {
        extra: make_extra(true),
        ..Default::default()
    });
    s.state.auth.as_ref().unwrap().reload().unwrap();

    let asks_narrowed = call(
        &s.app,
        "GET",
        "/$/asks/evask_hist",
        &[("authorization", &basic("evnarrow", "evnarrow-pw"))],
        "",
    )
    .await;
    assert_eq!(
        asks_narrowed.status,
        StatusCode::OK,
        "{}",
        asks_narrowed.text()
    );
    assert!(
        !asks_narrowed.text().contains("Secret projected ask answer"),
        "explicit-commit Ask over historical active policy leaked after grants narrowed: {}",
        asks_narrowed.text()
    );
    assert_eq!(
        asks_narrowed.json()["asks"][0]["answerWithheld"],
        serde_json::json!(true),
        "expected answerWithheld: true after evnarrow's grants narrowed: {}",
        asks_narrowed.text()
    );

    // 3. HTTP SSE `GET /evlive/live` must terminate with `access-changed` on same-commit detach
    //    and return a fresh empty `reset` on reconnect with `Last-Event-ID`.
    let ds_live = s.state.attach("evlive", DbType::Memory, None).unwrap();
    ev_api::configure(&ds_live.dataset, &settings, &Control::none()).unwrap();
    let p2 = ev_api::propose(
        None,
        &ds_live.dataset,
        &ds_live.dataset,
        &EvidenceInput {
            revisions: vec![SourceRevisionInput {
                version: RecordVersion::V1,
                key: RecordKey::new("rev-live").unwrap(),
                source: iri("urn:source:live"),
                source_key: RecordKey::new("src-live").unwrap(),
                upstream_version: None,
                content: SourceContent::MetadataOnly {
                    digest: sha("src-live"),
                },
                metadata: Default::default(),
                times: Default::default(),
                lineage: Default::default(),
            }],
            claims: vec![ClaimInput {
                version: RecordVersion::V1,
                key: RecordKey::new("claim-live").unwrap(),
                occurrence_key: RecordKey::new("clive").unwrap(),
                revision: RecordRef::Local(RecordKey::new("rev-live").unwrap()),
                proposition: oxrdf::Triple::new(
                    iri("urn:live:s"),
                    iri("http://ex/knows"),
                    iri("urn:live:o"),
                ),
                kind: ClaimKind::Reported,
                polarity: Polarity::Positive,
                attributed_author: iri("urn:author"),
                scope: Default::default(),
                confidence: Default::default(),
                validity: None,
            }],
            ..Default::default()
        },
        &po,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let rec_live = p2.records.iter().find(|r| r.key == "claim-live").unwrap();
    let mut ro2 = ReviewOptions::new(iri("urn:reviewer"));
    ro2.if_head = Some(ds_live.store.head_commit().seq);
    ev_api::review(
        None,
        &ds_live.dataset,
        &ds_live.dataset,
        "main",
        1,
        &[DecisionInput {
            key: "dlive".into(),
            target: rec_live.iri.clone(),
            target_digest: rec_live.digest,
            disposition: Disposition::Accepted,
            supersedes: None,
        }],
        &ro2,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();

    let live_commit = ds_live.store.head_commit().seq;
    let live_q =
        format!("SELECT ?o WHERE {{ GRAPH <{ACCEPTED}> {{ <urn:live:s> <http://ex/knows> ?o }} }}");
    let live_uri = format!(
        "/evlive/live?query={}",
        url::form_urlencoded::byte_serialize(live_q.as_bytes()).collect::<String>()
    );
    let resp = s
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&live_uri)
                .header("authorization", b("ra"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body();
    use http_body_util::BodyExt;
    let first_chunk = tokio::time::timeout(Duration::from_secs(5), body.frame())
        .await
        .expect("timed out waiting for initial SSE frame")
        .expect("stream ended early")
        .unwrap()
        .into_data()
        .unwrap();
    let first_text = String::from_utf8_lossy(&first_chunk);
    assert!(
        first_text.contains("urn:live:o"),
        "expected initial live snapshot to include urn:live:o: {first_text}"
    );

    // Detach + re-configure at the SAME commit -> open SSE stream must emit `access-changed`!
    ds_live
        .store
        .detach_managed(sparkles::managed::OwnerKind::Evidence, ACCEPTED)
        .unwrap();
    ev_api::configure(&ds_live.dataset, &settings, &Control::none()).unwrap();
    assert_eq!(ds_live.store.head_commit().seq, live_commit);

    let mut saw_access_changed = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), body.frame()).await {
            Ok(Some(Ok(frame))) => {
                if let Ok(data) = frame.into_data() {
                    let txt = String::from_utf8_lossy(&data);
                    if txt.contains("access-changed") {
                        saw_access_changed = true;
                        break;
                    }
                }
            }
            Ok(None) => break,
            _ => {}
        }
    }
    assert!(
        saw_access_changed,
        "HTTP SSE stream must emit access-changed on same-commit evidence detach"
    );

    // Reconnecting with `Last-Event-ID: live_commit` must emit a `reset` with 0 rows!
    let resp2 = s
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&live_uri)
                .header("authorization", b("ra"))
                .header("last-event-id", live_commit.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let mut body2 = resp2.into_body();
    let reconnect_chunk = tokio::time::timeout(Duration::from_secs(5), body2.frame())
        .await
        .expect("timed out waiting for reconnect SSE frame")
        .expect("reconnect stream ended early")
        .unwrap()
        .into_data()
        .unwrap();
    let reconnect_text = String::from_utf8_lossy(&reconnect_chunk);
    assert!(
        reconnect_text.contains("event: reset") && !reconnect_text.contains("urn:live:o"),
        "reconnected SSE stream across same-commit detach must emit empty reset: {reconnect_text}"
    );
}
