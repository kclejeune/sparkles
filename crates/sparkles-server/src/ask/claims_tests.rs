//! `POST /{ds}/ask` with a review policy (spec C25, "Recall and Ask with claims"):
//! the summary reads and cites claim occurrences, the policies are checked before any
//! model call, and a change before publication withholds the answer.

use super::tests::{ORG, draft, is_summary};
use crate::models::mock::{self, MockModel};
use crate::models::{Models, ModelsConfig};
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use oxrdf::{NamedNode, Triple};
use serde_json::{Value, json};
use sparkles::evidence::{
    self, ClaimInput, ClaimKind, ConflictMode, DecisionInput, Disposition, EvidenceInput,
    IdentityInput, IdentityRelation, MatchingPolicy, MethodRef, Polarity, Policy, ProposeOptions,
    RecordKey, RecordRef, RecordVersion, ReviewOptions, Settings, Sha256Digest, SourceContent,
    SourceRevisionInput, WithdrawalInput,
};
use sparkles::io::{RdfFormat, Source};
use sparkles::task::Control;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tower::ServiceExt;

const EASY: &str = "SELECT ?t WHERE { ?t a ex:Team }";
const SUMMARY: &str = "Acme has two teams [1] [2] and is based in Lisbon [c1]. Nothing else [c7].";

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn all() -> Option<Arc<sparkles::access::GraphAccess>> {
    Some(Arc::new(sparkles::access::GraphAccess::all()))
}

/// What a summary request runs before it answers, such as a withdrawal.
type Hook = OnceLock<Box<dyn Fn() + Send + Sync>>;

/// A mock that drafts `EASY` and summarizes citing two rows and two claims.
fn mock(hook: Arc<Hook>) -> MockModel {
    MockModel::start(move |r, _| {
        if is_summary(r) {
            if let Some(h) = hook.get() {
                h();
            }
            return (
                200,
                mock::openai(
                    &json!({ "text": SUMMARY, "citations": [1, 2], "claimCitations": ["c1", "c7"] })
                        .to_string(),
                ),
            );
        }
        (200, mock::openai(&draft(EASY)))
    })
}

fn settings() -> Settings {
    Settings {
        enabled: true,
        source_records: vec!["urn:ev:sources".into()],
        proposals: vec!["urn:ev:claims".into()],
        decisions: vec!["urn:ev:decisions".into()],
        supports: vec!["urn:ev:supports".into()],
        identity: vec!["urn:ev:identities".into(), "urn:ev:maps".into()],
        projections: vec!["urn:ev:accepted".into(), "urn:ev:map".into()],
        policies: [(
            "main".into(),
            Policy {
                version: 1,
                admitted_kinds: vec![ClaimKind::Reported],
                required_scope: vec![],
                conflicts: ConflictMode::AdmitLabelled,
                boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
                decisions: "urn:ev:decisions".into(),
                supports: "urn:ev:supports".into(),
                output: "urn:ev:accepted".into(),
            },
        )]
        .into_iter()
        .collect(),
        matching_policies: [(
            "people".into(),
            MatchingPolicy {
                version: 1,
                boundary: vec!["urn:ev:identities".into()],
                decisions: "urn:ev:decisions".into(),
                maps: "urn:ev:maps".into(),
                output: "urn:ev:map".into(),
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    }
}

/// The organisation graph as `org`, with configuration history activated, evidence
/// configured and an assistant whose summary may receive rows.
async fn app(url: &str) -> (Arc<AppState>, Router) {
    let pair = json!({ "provider": "p", "model": "m" });
    let cfg = json!({
        "providers": { "p": { "kind": "openai", "endpoint": url, "requestTimeoutSecs": 5, "allowedModels": ["m"] } },
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
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    st.models = arc_swap::ArcSwapOption::from_pointee(models);
    st.limits.max_result_bytes = None;
    let st = Arc::new(st);
    let ds = st.attach("org", DbType::Mem, None).unwrap();
    let p = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    ds.dataset
        .store()
        .activate_memory_configuration_history(&p)
        .unwrap();
    ds.store
        .load(&[Source::from_bytes(
            ORG.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    evidence::configure(&ds.dataset, &settings(), &Control::none()).unwrap();
    let router = crate::http::router(st.clone());
    let (s, b) = send(
        &router,
        Request::builder()
            .method("PUT")
            .uri("/$/assistant/org")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "enabled": true, "send": "rows" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    (st, router)
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let res = app.clone().oneshot(req).await.unwrap();
    let s = res.status();
    let b = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (s, String::from_utf8_lossy(&b).into_owned())
}

async fn ask(app: &Router, body: Value) -> Value {
    ask_with(app, body, StatusCode::OK).await
}

async fn ask_with(app: &Router, body: Value, status: StatusCode) -> Value {
    let r = Request::post("/org/ask")
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (s, text) = send(app, r).await;
    assert_eq!(s, status, "{text}");
    serde_json::from_str(&text).unwrap()
}

/// The events of a server-sent event stream, in order.
async fn ask_sse(app: &Router, body: Value) -> Vec<(String, Value)> {
    let r = Request::post("/org/ask")
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (s, text) = send(app, r).await;
    assert_eq!(s, StatusCode::OK, "{text}");
    text.split("\n\n")
        .filter_map(|block| {
            let mut name = None;
            let mut data = String::new();
            for line in block.lines() {
                if let Some(e) = line.strip_prefix("event:") {
                    name = Some(e.trim().to_string());
                } else if let Some(d) = line.strip_prefix("data:") {
                    data.push_str(d.trim_start());
                }
            }
            Some((name?, serde_json::from_str(&data).unwrap_or(Value::Null)))
        })
        .collect()
}

/// An accepted claim that Acme is based in Lisbon, and its identity.
fn claim(ds: &sparkles::Dataset) -> (NamedNode, Sha256Digest) {
    let key = |k: &str| RecordKey::new(k).unwrap();
    let input = EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("rev"),
            source: iri("urn:source:registry"),
            source_key: key("registry"),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: Sha256Digest::of_bytes(b"registry", &Control::none()).unwrap(),
            },
            metadata: Default::default(),
            times: Default::default(),
            lineage: Default::default(),
        }],
        claims: vec![ClaimInput {
            version: RecordVersion::V1,
            key: key("claim"),
            occurrence_key: key("c"),
            revision: RecordRef::Local(key("rev")),
            proposition: Triple::new(
                iri("http://example.org/resource/acme"),
                iri("http://example.org/ontology#basedIn"),
                iri("http://example.org/resource/lisbon"),
            ),
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: iri("urn:registry"),
            scope: Default::default(),
            confidence: Default::default(),
            validity: None,
        }],
        ..Default::default()
    };
    let mut o = ProposeOptions::new(iri("urn:principal:importer"));
    o.proposal_graph = Some(iri("urn:ev:claims"));
    let p = evidence::propose(
        None,
        ds,
        ds,
        &input,
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let r = p.records.iter().find(|r| r.key == "claim").unwrap();
    let c = (r.iri.clone(), r.digest);
    let mut ro = ReviewOptions::new(iri("urn:principal:reviewer"));
    ro.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        ds,
        ds,
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
    c
}

fn dataset(st: &AppState) -> sparkles::Dataset {
    st.get("org").unwrap().dataset.clone()
}

/// The summary cites only listed claims, carries them, and only the summary step sees
/// them. Without a policy nothing changes.
#[tokio::test(flavor = "multi_thread")]
async fn the_summary_reads_and_cites_claims() {
    let m = mock(Arc::new(OnceLock::new()));
    let (st, app) = app(&m.url()).await;
    claim(&dataset(&st));
    let question = "Which teams does Acme have?";
    let out = ask(
        &app,
        json!({ "question": question, "reviewPolicy": "main" }),
    )
    .await;
    assert_eq!(out["outcome"], "answered", "{out}");
    assert_eq!(out["summary"]["claimCitations"], json!(["c1"]), "{out}");
    assert_eq!(out["claims"]["version"], 1);
    assert_eq!(out["claims"]["policy"]["name"], "main");
    let occurrences = out["claims"]["occurrences"].as_array().unwrap();
    assert_eq!(occurrences.len(), 1, "{out}");
    assert_eq!(occurrences[0]["state"], "accepted");
    let reqs = m.requests();
    let summaries: Vec<String> = reqs
        .iter()
        .filter(|r| is_summary(r))
        .map(|r| r.body.to_string())
        .collect();
    assert_eq!(summaries.len(), 1);
    assert!(summaries[0].contains("Claim occurrences") && summaries[0].contains("lisbon"));
    // the system prompt lets the answer use the listed occurrences
    assert!(summaries[0].contains("claim occurrences listed with them"));
    // an unlisted marker leaves the text, and every marker left names a listed claim
    let text = out["summary"]["text"].as_str().unwrap();
    assert!(!text.contains("[c7]"), "{text}");
    assert_eq!(super::prompts::claim_markers(text), vec!["c1".to_string()]);
    assert!(
        reqs.iter()
            .filter(|r| !is_summary(r))
            .all(|r| !r.body.to_string().contains("Claim occurrences"))
    );
    let plain = ask(&app, json!({ "question": question })).await;
    assert_eq!(plain["outcome"], "answered", "{plain}");
    let last = m.requests().into_iter().rev().find(is_summary).unwrap();
    assert!(
        !last
            .body
            .to_string()
            .contains("claim occurrences listed with them")
    );
    assert!(plain.get("claims").is_none() && plain["summary"].get("claimCitations").is_none());
}

/// An unknown policy, wrong policy kind, and the global mode are refused before any
/// model call.
#[tokio::test(flavor = "multi_thread")]
async fn refused_policies_cost_no_model_call() {
    let m = mock(Arc::new(OnceLock::new()));
    let (_, app) = app(&m.url()).await;
    for body in [
        json!({ "question": "Which teams does Acme have?", "reviewPolicy": "nothing" }),
        json!({ "question": "Which teams does Acme have?", "matchingPolicy": "nothing" }),
        json!({ "question": "Which teams does Acme have?", "reviewPolicy": "people" }),
        json!({ "question": "Which teams does Acme have?", "matchingPolicy": "main" }),
        json!({ "question": "Which teams does Acme have?", "reviewPolicy": "main", "mode": "global" }),
        json!({ "question": "Which teams does Acme have?", "matchingPolicy": "people", "mode": "global" }),
    ] {
        let out = ask_with(&app, body.clone(), StatusCode::BAD_REQUEST).await;
        assert_eq!(out["outcome"], "error", "{body}: {out}");
        assert_eq!(out["error"]["code"], "bad-argument", "{body}: {out}");
    }
    assert!(m.requests().is_empty());
}

/// Matching policy alone reports its resolution map without adding null claims or
/// claim-citation prompts; combined with a review policy it expands linked entities
/// across clusters so claims on an alias reach the summary.
#[tokio::test(flavor = "multi_thread")]
async fn matching_policy_resolves_cluster_claims_and_alone_omits_claims() {
    let m = MockModel::start(|r, _| {
        if is_summary(r) {
            if r.body.to_string().contains("Claim occurrences") {
                return (
                    200,
                    mock::openai(
                        &json!({
                            "text": "Acme has two teams [1] [2] and is based in Porto [c1].",
                            "citations": [1, 2],
                            "claimCitations": ["c1"],
                        })
                        .to_string(),
                    ),
                );
            }
            return (
                200,
                mock::openai(
                    &json!({
                        "text": "Acme has two teams [1] [2]. Hallucinated [c1].",
                        "citations": [1, 2],
                    })
                    .to_string(),
                ),
            );
        }
        (200, mock::openai(&draft(EASY)))
    });
    let (st, app) = app(&m.url()).await;
    let ds = dataset(&st);
    let key = |k: &str| RecordKey::new(k).unwrap();
    let alias = iri("http://example.org/resource/acme-alias");
    let mut po = ProposeOptions::new(iri("urn:principal:matcher"));
    po.identity_graph = Some(iri("urn:ev:identities"));
    let proposed = evidence::propose(
        None,
        &ds,
        &ds,
        &EvidenceInput {
            identities: vec![IdentityInput {
                version: RecordVersion::V1,
                key: key("id"),
                occurrence_key: key("m1"),
                entities: vec![iri("http://example.org/resource/acme"), alias.clone()],
                relation: IdentityRelation::PossibleMatch,
                preferred: Some(iri("http://example.org/resource/acme")),
                attributed_author: iri("urn:matcher"),
                method: Some(MethodRef {
                    method: iri("urn:method:names"),
                    version: key("1"),
                }),
            }],
            ..Default::default()
        },
        &po,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let id_rec = proposed.records.iter().find(|r| r.key == "id").unwrap();
    let mut ro = ReviewOptions::new(iri("urn:principal:reviewer"));
    ro.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        &ds,
        &ds,
        "people",
        1,
        &[DecisionInput {
            key: "dm".into(),
            target: id_rec.iri.clone(),
            target_digest: id_rec.digest,
            disposition: Disposition::Accepted,
            supersedes: None,
        }],
        &ro,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let mut co = ProposeOptions::new(iri("urn:principal:importer"));
    co.proposal_graph = Some(iri("urn:ev:claims"));
    let alias_claim = evidence::propose(
        None,
        &ds,
        &ds,
        &EvidenceInput {
            revisions: vec![SourceRevisionInput {
                version: RecordVersion::V1,
                key: key("rev-alias"),
                source: iri("urn:source:alias-reg"),
                source_key: key("alias-reg"),
                upstream_version: None,
                content: SourceContent::MetadataOnly {
                    digest: Sha256Digest::of_bytes(b"alias-reg", &Control::none()).unwrap(),
                },
                metadata: Default::default(),
                times: Default::default(),
                lineage: Default::default(),
            }],
            claims: vec![ClaimInput {
                version: RecordVersion::V1,
                key: key("claim-alias"),
                occurrence_key: key("ca"),
                revision: RecordRef::Local(key("rev-alias")),
                proposition: Triple::new(
                    alias,
                    iri("http://example.org/ontology#basedIn"),
                    iri("http://example.org/resource/porto"),
                ),
                kind: ClaimKind::Reported,
                polarity: Polarity::Positive,
                attributed_author: iri("urn:registry"),
                scope: Default::default(),
                confidence: Default::default(),
                validity: None,
            }],
            ..Default::default()
        },
        &co,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let cr = alias_claim
        .records
        .iter()
        .find(|r| r.key == "claim-alias")
        .unwrap();
    let mut ro = ReviewOptions::new(iri("urn:principal:reviewer"));
    ro.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        &ds,
        &ds,
        "main",
        1,
        &[DecisionInput {
            key: "da".into(),
            target: cr.iri.clone(),
            target_digest: cr.digest,
            disposition: Disposition::Accepted,
            supersedes: None,
        }],
        &ro,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();

    // matchingPolicy alone: reports resolution, omits claims and claimCitations
    let only_match = ask(
        &app,
        json!({ "question": "Which teams does Acme have?", "matchingPolicy": "people" }),
    )
    .await;
    assert_eq!(only_match["outcome"], "answered", "{only_match}");
    assert_eq!(only_match["resolution"]["policy"], "people", "{only_match}");
    assert_eq!(only_match["resolution"]["version"], 1, "{only_match}");
    assert_eq!(only_match["resolution"]["number"], 1, "{only_match}");
    assert!(only_match["resolution"]["revision"].is_string());
    assert!(only_match.get("claims").is_none(), "{only_match}");
    assert!(
        only_match["summary"].get("claimCitations").is_none(),
        "{only_match}"
    );
    assert!(
        only_match["summary"].get("resolution").is_none(),
        "{only_match}"
    );
    let only_text = only_match["summary"]["text"].as_str().unwrap();
    assert!(!only_text.contains("[c1]"), "{only_text}");
    let summary_req = m.requests().into_iter().rev().find(is_summary).unwrap();
    assert!(
        !summary_req
            .body
            .to_string()
            .contains("claim occurrences listed with them")
    );

    // HTTP SSE with matchingPolicy alone: summary event carries resolution, strips [c1],
    // and omits claims / claimCitations; with summary: false, a terminal policy event
    // carries resolution before usage.
    let sse_match = ask_sse(
        &app,
        json!({ "question": "Which teams does Acme have?", "matchingPolicy": "people" }),
    )
    .await;
    let sum_ev = &sse_match.iter().find(|(n, _)| n == "summary").unwrap().1;
    assert_eq!(sum_ev["resolution"], only_match["resolution"], "{sum_ev}");
    assert!(sum_ev.get("claims").is_none(), "{sum_ev}");
    assert!(sum_ev.get("claimCitations").is_none(), "{sum_ev}");
    assert!(
        !sum_ev["text"].as_str().unwrap().contains("[c1]"),
        "{sum_ev}"
    );
    assert!(
        sse_match.iter().all(|(n, _)| n != "policy"),
        "summary already carries resolution: {sse_match:?}"
    );

    let sse_match_nosum = ask_sse(
        &app,
        json!({
            "question": "Which teams does Acme have?",
            "matchingPolicy": "people",
            "summary": false,
        }),
    )
    .await;
    assert!(
        sse_match_nosum.iter().all(|(n, _)| n != "summary"),
        "{sse_match_nosum:?}"
    );
    let pol_ev = &sse_match_nosum
        .iter()
        .find(|(n, _)| n == "policy")
        .unwrap()
        .1;
    assert_eq!(pol_ev["resolution"], only_match["resolution"], "{pol_ev}");
    assert!(pol_ev.get("claims").is_none(), "{pol_ev}");

    // reviewPolicy + matchingPolicy: cluster expansion pulls the alias claim into summary
    let both = ask(
        &app,
        json!({
            "question": "Which teams does Acme have?",
            "reviewPolicy": "main",
            "matchingPolicy": "people",
        }),
    )
    .await;
    assert_eq!(both["outcome"], "answered", "{both}");
    assert_eq!(both["resolution"]["policy"], "people", "{both}");
    assert_eq!(both["resolution"]["number"], 1, "{both}");
    assert_eq!(both["claims"]["policy"]["name"], "main", "{both}");
    assert_eq!(both["summary"]["claimCitations"], json!(["c1"]), "{both}");
    assert!(both["summary"].get("resolution").is_none(), "{both}");
    let occs = both["claims"]["occurrences"].as_array().unwrap();
    assert_eq!(occs.len(), 1, "{both}");
    assert!(
        occs[0]["proposition"]["o"]
            .as_str()
            .unwrap()
            .contains("porto"),
        "{both}"
    );

    // HTTP SSE with both policies carries claims and resolution on summary, and with
    // summary: false carries both on the terminal policy event.
    let sse_both = ask_sse(
        &app,
        json!({
            "question": "Which teams does Acme have?",
            "reviewPolicy": "main",
            "matchingPolicy": "people",
        }),
    )
    .await;
    let sum_both = &sse_both.iter().find(|(n, _)| n == "summary").unwrap().1;
    assert_eq!(sum_both["claims"], both["claims"], "{sum_both}");
    assert_eq!(sum_both["resolution"], both["resolution"], "{sum_both}");
    assert_eq!(sum_both["claimCitations"], json!(["c1"]), "{sum_both}");

    let sse_rev_nosum = ask_sse(
        &app,
        json!({
            "question": "Which teams does Acme have?",
            "reviewPolicy": "main",
            "matchingPolicy": "people",
            "summary": false,
        }),
    )
    .await;
    assert!(
        sse_rev_nosum.iter().all(|(n, _)| n != "summary"),
        "{sse_rev_nosum:?}"
    );
    let pol_both = &sse_rev_nosum.iter().find(|(n, _)| n == "policy").unwrap().1;
    assert_eq!(pol_both["claims"], both["claims"], "{pol_both}");
    assert_eq!(pol_both["resolution"], both["resolution"], "{pol_both}");
}

/// A claim withdrawn while the summary is written withholds the answer.
#[tokio::test(flavor = "multi_thread")]
async fn a_change_before_publication_withholds_the_answer() {
    let hook: Arc<Hook> = Arc::new(OnceLock::new());
    let m = mock(hook.clone());
    let (st, app) = app(&m.url()).await;
    let ds = dataset(&st);
    let c = claim(&ds);
    let done = std::sync::atomic::AtomicBool::new(false);
    let target = c.clone();
    let _ = hook.set(Box::new(move || {
        if !done.swap(true, std::sync::atomic::Ordering::SeqCst) {
            evidence::withdraw(
                &ds,
                &ds,
                &[WithdrawalInput {
                    key: "w".into(),
                    target: target.0.clone(),
                    target_digest: target.1,
                }],
                &ReviewOptions::new(iri("urn:principal:reviewer")),
                &mut |_, _| all(),
                &Control::none(),
            )
            .unwrap();
        }
    }));
    let out = ask_with(
        &app,
        json!({ "question": "Which teams does Acme have?", "reviewPolicy": "main" }),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(out["outcome"], "error", "{out}");
    assert_eq!(out["error"]["code"], "forbidden");
    assert!(
        out.get("claims").is_none() && out.get("summary").is_none(),
        "{out}"
    );
}

/// A matching-map change while a matchingPolicy-only summary is in flight withholds the
/// answer in both JSON and SSE responses.
#[tokio::test(flavor = "multi_thread")]
async fn matching_policy_change_before_publication_withholds_the_answer() {
    let hook: Arc<Hook> = Arc::new(OnceLock::new());
    let h = hook.clone();
    let m = MockModel::start(move |r, _| {
        if is_summary(r) {
            if let Some(f) = h.get() {
                f();
            }
            return (
                200,
                mock::openai(
                    &json!({ "text": "Acme has two teams [1] [2].", "citations": [1, 2] })
                        .to_string(),
                ),
            );
        }
        (200, mock::openai(&draft(EASY)))
    });
    let (st, app) = app(&m.url()).await;
    let ds = dataset(&st);
    let key = |k: &str| RecordKey::new(k).unwrap();
    let mut po = ProposeOptions::new(iri("urn:principal:matcher"));
    po.identity_graph = Some(iri("urn:ev:identities"));
    let proposed = evidence::propose(
        None,
        &ds,
        &ds,
        &EvidenceInput {
            identities: vec![
                IdentityInput {
                    version: RecordVersion::V1,
                    key: key("id1"),
                    occurrence_key: key("m1"),
                    entities: vec![
                        iri("http://example.org/resource/acme"),
                        iri("http://example.org/resource/acme-a"),
                    ],
                    relation: IdentityRelation::PossibleMatch,
                    preferred: Some(iri("http://example.org/resource/acme")),
                    attributed_author: iri("urn:matcher"),
                    method: Some(MethodRef {
                        method: iri("urn:method:names"),
                        version: key("1"),
                    }),
                },
                IdentityInput {
                    version: RecordVersion::V1,
                    key: key("id2"),
                    occurrence_key: key("m2"),
                    entities: vec![
                        iri("http://example.org/resource/acme"),
                        iri("http://example.org/resource/acme-b"),
                    ],
                    relation: IdentityRelation::PossibleMatch,
                    preferred: Some(iri("http://example.org/resource/acme")),
                    attributed_author: iri("urn:matcher"),
                    method: Some(MethodRef {
                        method: iri("urn:method:names"),
                        version: key("1"),
                    }),
                },
            ],
            ..Default::default()
        },
        &po,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let id1 = proposed
        .records
        .iter()
        .find(|r| r.key == "id1")
        .unwrap()
        .clone();
    let id2 = proposed
        .records
        .iter()
        .find(|r| r.key == "id2")
        .unwrap()
        .clone();
    let step = std::sync::atomic::AtomicUsize::new(0);
    let _ = hook.set(Box::new(move || {
        let n = step.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (k, target) = match n {
            0 => ("dm1", &id1),
            1 => ("dm2", &id2),
            _ => return,
        };
        let mut ro = ReviewOptions::new(iri("urn:principal:reviewer"));
        ro.if_head = Some(ds.store().head_commit().seq);
        evidence::review(
            None,
            &ds,
            &ds,
            "people",
            1,
            &[DecisionInput {
                key: k.into(),
                target: target.iri.clone(),
                target_digest: target.digest,
                disposition: Disposition::Accepted,
                supersedes: None,
            }],
            &ro,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
    }));

    let out = ask_with(
        &app,
        json!({ "question": "Which teams does Acme have?", "matchingPolicy": "people" }),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(out["outcome"], "error", "{out}");
    assert_eq!(out["error"]["code"], "forbidden", "{out}");
    assert!(
        out.get("resolution").is_none()
            && out.get("claims").is_none()
            && out.get("summary").is_none(),
        "{out}"
    );

    let sse = ask_sse(
        &app,
        json!({ "question": "Which teams does Acme have?", "matchingPolicy": "people" }),
    )
    .await;
    assert!(
        sse.iter().all(|(n, _)| n != "summary" && n != "policy"),
        "withheld SSE must not publish summary or policy: {sse:?}"
    );
    let err_ev = &sse.iter().find(|(n, _)| n == "error").unwrap().1;
    assert_eq!(err_ev["code"], "forbidden", "{err_ev}");
}

#[test]
fn claim_markers_read_only_claim_identifiers() {
    assert_eq!(
        super::prompts::claim_markers("a [c1] b [1] c [c12] [c] [c1] [x2]"),
        vec!["c1".to_string(), "c12".to_string()]
    );
}

/// A kept answer stores the occurrences its summary cites and no other.
#[test]
fn a_kept_answer_stores_its_cited_claims() {
    let out = json!({
        "result": {},
        "summary": { "text": "Based in Lisbon [c1].", "citations": [], "claimCitations": ["c1"] },
        "claims": {
            "version": 1,
            "policy": { "name": "main", "version": 1 },
            "occurrences": [ { "id": "c1", "state": "accepted" }, { "id": "c2", "state": "rejected" } ],
            "truncated": false,
        },
    });
    let a = crate::assistant::stored_answer(&out, &Value::Null).unwrap();
    assert_eq!(a["summary"]["claimCitations"], json!(["c1"]));
    assert_eq!(
        a["claims"],
        json!({ "version": 1, "policy": { "name": "main", "version": 1 },
                "occurrences": [ { "id": "c1", "state": "accepted" } ] })
    );
    let plain = crate::assistant::stored_answer(
        &json!({ "result": {}, "summary": { "text": "Two [1].", "citations": [1] } }),
        &Value::Null,
    )
    .unwrap();
    assert!(plain.get("claims").is_none());
}

/// A supplied query has no linked entities, so no claim reaches the summary, whatever
/// the claims name.
#[tokio::test(flavor = "multi_thread")]
async fn a_supplied_query_reads_no_claims() {
    let m = mock(Arc::new(OnceLock::new()));
    let (st, app) = app(&m.url()).await;
    let ds = dataset(&st);
    claim(&ds);
    // a claim about the IRI an earlier preflight used as a stand-in seed
    let key = |k: &str| RecordKey::new(k).unwrap();
    let mut o = ProposeOptions::new(iri("urn:principal:importer"));
    o.proposal_graph = Some(iri("urn:ev:claims"));
    evidence::propose(
        None,
        &ds,
        &ds,
        &EvidenceInput {
            revisions: vec![SourceRevisionInput {
                version: RecordVersion::V1,
                key: key("rev"),
                source: iri("urn:source:other"),
                source_key: key("other"),
                upstream_version: None,
                content: SourceContent::MetadataOnly {
                    digest: Sha256Digest::of_bytes(b"other", &Control::none()).unwrap(),
                },
                metadata: Default::default(),
                times: Default::default(),
                lineage: Default::default(),
            }],
            claims: vec![ClaimInput {
                version: RecordVersion::V1,
                key: key("claim"),
                occurrence_key: key("c"),
                revision: RecordRef::Local(key("rev")),
                proposition: Triple::new(
                    iri("urn:x-sparkles:evidence:ask-preflight"),
                    iri("urn:says"),
                    iri("urn:anything"),
                ),
                kind: ClaimKind::Reported,
                polarity: Polarity::Positive,
                attributed_author: iri("urn:other"),
                scope: Default::default(),
                confidence: Default::default(),
                validity: None,
            }],
            ..Default::default()
        },
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let out = ask(
        &app,
        json!({ "question": "Which teams does Acme have?", "query": EASY, "reviewPolicy": "main" }),
    )
    .await;
    assert_eq!(out["outcome"], "answered", "{out}");
    assert_eq!(out["claims"]["occurrences"], json!([]), "{out}");
    assert_eq!(out["claims"]["policy"]["name"], "main", "{out}");
    let summary = m.requests().into_iter().find(is_summary).unwrap();
    assert!(!summary.body.to_string().contains("anything"));
}

/// An evidence refusal from recall keeps recall's status in the JSON form.
#[test]
fn evidence_refusals_keep_their_status() {
    use axum::http::StatusCode as S;
    let status = super::http::status_of;
    assert_eq!(status("evidence-unavailable"), S::UNPROCESSABLE_ENTITY);
    assert_eq!(status("incomplete-evidence"), S::UNPROCESSABLE_ENTITY);
    assert_eq!(status("projection-unavailable"), S::SERVICE_UNAVAILABLE);
    assert_eq!(status("configuration-history-inactive"), S::CONFLICT);
    assert_eq!(status("forbidden"), S::FORBIDDEN);
    assert_eq!(status("something-else"), S::INTERNAL_SERVER_ERROR);
}
