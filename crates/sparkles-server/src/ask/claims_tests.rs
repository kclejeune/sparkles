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
            was_revision_of: None,
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

/// An unknown or wrong-kind policy is refused before any model call, in both local and
/// global modes.
#[tokio::test(flavor = "multi_thread")]
async fn refused_policies_cost_no_model_call() {
    let m = mock(Arc::new(OnceLock::new()));
    let (_, app) = app(&m.url()).await;
    for body in [
        json!({ "question": "Which teams does Acme have?", "reviewPolicy": "nothing" }),
        json!({ "question": "Which teams does Acme have?", "matchingPolicy": "nothing" }),
        json!({ "question": "Which teams does Acme have?", "reviewPolicy": "people" }),
        json!({ "question": "Which teams does Acme have?", "matchingPolicy": "main" }),
        json!({ "question": "Which teams does Acme have?", "reviewPolicy": "nothing", "mode": "global" }),
        json!({ "question": "Which teams does Acme have?", "matchingPolicy": "nothing", "mode": "global" }),
        json!({ "question": "Which teams does Acme have?", "matchingPolicy": "main", "mode": "global" }),
    ] {
        let out = ask_with(&app, body, StatusCode::BAD_REQUEST).await;
        assert_eq!(out["outcome"], "error", "{out}");
        assert_eq!(out["error"]["code"], "bad-argument", "{out}");
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

/// `stored_answer` persists `global.reviewPolicy` when present and omits it otherwise.
#[test]
fn stored_answer_persists_global_review_policy() {
    let out = json!({
        "mode": "global",
        "points": [{ "n": 1, "text": "Acme payments.", "reports": ["urn:r:main"] }],
        "reports": [{ "iri": "urn:r:main", "title": "Main Policy Community" }],
        "global": {
            "level": 0,
            "ranking": "rating",
            "reviewPolicy": {
                "name": "main",
                "iri": "urn:x-sparkles:evidence:policy:main",
                "version": 1,
            },
        },
        "summary": { "text": "Main summary [1].", "citations": [1] },
    });
    let kept = crate::assistant::stored_answer(&out, &Value::Null).unwrap();
    assert_eq!(
        kept["global"]["reviewPolicy"],
        json!({
            "name": "main",
            "iri": "urn:x-sparkles:evidence:policy:main",
            "version": 1,
        })
    );
    let unpolicied = crate::assistant::stored_answer(
        &json!({
            "mode": "global",
            "points": [{ "n": 1, "text": "Plain.", "reports": ["urn:r:plain"] }],
            "reports": [{ "iri": "urn:r:plain", "title": "Plain" }],
            "global": { "level": 0, "ranking": "rating" },
            "summary": { "text": "Plain [1].", "citations": [1] },
        }),
        &Value::Null,
    )
    .unwrap();
    assert!(unpolicied["global"].get("reviewPolicy").is_none());
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
                was_revision_of: None,
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

/// Global Ask with `reviewPolicy` and `matchingPolicy` (spec C25, item 811):
/// - `reviewPolicy` selects only reports built under that policy, excluding reports built
///   under another policy or no policy;
/// - requesting a policy with no reports excludes other policies' reports and falls back;
/// - `matchingPolicy` resolves identities referenced by the reports (and handles an empty
///   map revision cleanly);
/// - changing the matching map mid-flight withholds the global answer (`403 forbidden`).
#[tokio::test(flavor = "multi_thread")]
async fn global_answer_filters_by_review_policy_and_resolves_report_identities() {
    use sparkles::evidence::{IdentityInput, IdentityRelation, MatchingPolicy, MethodRef};

    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
    const IDENTITIES: &str = "urn:ev:identities";
    const MAPS: &str = "urn:ev:maps";
    const MAP: &str = "urn:ev:map";
    const EMPTY_MAPS: &str = "urn:ev:empty-maps";
    const EMPTY_MAP: &str = "urn:ev:empty-map";
    const OTHER_DEC: &str = "urn:ev:other-decisions";
    const OTHER_SUP: &str = "urn:ev:other-supports";
    const OTHER_OUT: &str = "urn:ev:other-accepted";
    const UNUSED_DEC: &str = "urn:ev:unused-decisions";
    const UNUSED_SUP: &str = "urn:ev:unused-supports";
    const UNUSED_OUT: &str = "urn:ev:unused-accepted";

    let hook: Arc<Hook> = Arc::new(OnceLock::new());
    let hook_c = hook.clone();
    let m = MockModel::start(move |r, _| {
        let body = r.body.to_string();
        if body.contains("\"Points\"") || body.contains("pick out what bears") {
            return (
                200,
                mock::openai(
                    &json!({
                        "points": [{
                            "text": "Acme payments community under main policy.",
                            "score": 90,
                            "reports": ["R1"],
                            "findings": ["F1.1"]
                        }]
                    })
                    .to_string(),
                ),
            );
        }
        if body.contains("\"GlobalAnswer\"") || body.contains("Points from the community reports") {
            if let Some(h) = hook_c.get() {
                h();
            }
            return (
                200,
                mock::openai(
                    &json!({
                        "text": "The main policy reports describe Acme payments [1].",
                        "citations": [1]
                    })
                    .to_string(),
                ),
            );
        }
        if is_summary(r) {
            return (
                200,
                mock::openai(
                    &json!({ "text": "Fallback local answer [1].", "citations": [1] }).to_string(),
                ),
            );
        }
        if body.contains("\"scope\"")
            || body.contains("Classify the question")
            || body.contains("scope") && body.contains("global")
        {
            return (
                200,
                mock::openai(
                    &json!({
                        "scope": "global",
                        "query": "",
                        "explanation": "Whole-dataset overview.",
                        "assumptions": [],
                        "clarify": { "question": "", "choices": [] },
                        "graph": { "subject": "", "predicate": "", "object": "" }
                    })
                    .to_string(),
                ),
            );
        }
        (200, mock::openai(&draft(EASY)))
    });

    let (st, app) = app(&m.url()).await;
    let ds = dataset(&st);

    // Configure three review policies ("main", "other", "unused") and two matching
    // policies ("people" with a merged cluster, "empty" with no map revision).
    let mut cfg = settings();
    cfg.decisions.extend([OTHER_DEC.into(), UNUSED_DEC.into()]);
    cfg.supports.extend([OTHER_SUP.into(), UNUSED_SUP.into()]);
    cfg.projections.extend([
        OTHER_OUT.into(),
        UNUSED_OUT.into(),
        MAP.into(),
        EMPTY_MAP.into(),
    ]);
    cfg.identity
        .extend([IDENTITIES.into(), MAPS.into(), EMPTY_MAPS.into()]);
    cfg.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: vec![],
            conflicts: ConflictMode::AdmitLabelled,
            boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
            decisions: OTHER_DEC.into(),
            supports: OTHER_SUP.into(),
            output: OTHER_OUT.into(),
        },
    );
    cfg.policies.insert(
        "unused".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: vec![],
            conflicts: ConflictMode::AdmitLabelled,
            boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
            decisions: UNUSED_DEC.into(),
            supports: UNUSED_SUP.into(),
            output: UNUSED_OUT.into(),
        },
    );
    cfg.matching_policies.insert(
        "people".into(),
        MatchingPolicy {
            version: 1,
            boundary: vec![IDENTITIES.into()],
            decisions: "urn:ev:decisions".into(),
            maps: MAPS.into(),
            output: MAP.into(),
        },
    );
    cfg.matching_policies.insert(
        "empty".into(),
        MatchingPolicy {
            version: 1,
            boundary: vec![IDENTITIES.into()],
            decisions: "urn:ev:decisions".into(),
            maps: EMPTY_MAPS.into(),
            output: EMPTY_MAP.into(),
        },
    );
    evidence::configure(&ds, &cfg, &Control::none()).unwrap();

    // Propose and accept a PossibleMatch merging alice_dup into alice under "people".
    let mut po = ProposeOptions::new(iri("urn:principal:matcher"));
    po.identity_graph = Some(iri(IDENTITIES));
    let id_prop = evidence::propose(
        None,
        &ds,
        &ds,
        &EvidenceInput {
            identities: vec![IdentityInput {
                version: RecordVersion::V1,
                key: RecordKey::new("m1").unwrap(),
                occurrence_key: RecordKey::new("occ-m1").unwrap(),
                entities: vec![
                    iri("http://example.org/resource/alice"),
                    iri("http://example.org/resource/alice_dup"),
                ],
                relation: IdentityRelation::PossibleMatch,
                preferred: Some(iri("http://example.org/resource/alice")),
                attributed_author: iri("urn:matcher"),
                method: Some(MethodRef {
                    method: iri("urn:method"),
                    version: RecordKey::new("1").unwrap(),
                }),
            }],
            ..Default::default()
        },
        &po,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let match_rec = &id_prop.records[0];
    let mut ro = ReviewOptions::new(iri("urn:principal:reviewer"));
    ro.if_head = Some(ds.store().head_commit().seq);
    let rev = evidence::review(
        None,
        &ds,
        &ds,
        "people",
        1,
        &[DecisionInput {
            key: "dm1".into(),
            target: match_rec.iri.clone(),
            target_digest: match_rec.digest,
            disposition: Disposition::Accepted,
            supersedes: None,
        }],
        &ro,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let first_decision = rev.decisions[0].iri.clone();

    // Populate mixed community reports in the reports graph:
    // - r-main: built under reviewPolicy "main" (references alice_dup)
    // - r-other: built under reviewPolicy "other" (secret marker OTHER_ONLY_TEXT)
    // - r-plain: built with no reviewPolicy (secret marker PLAIN_ONLY_TEXT)
    let gr = crate::graphrag::guard::gr;
    let main_policy_iri = sparkles::evidence::review::policy_iri("main");
    let other_policy_iri = sparkles::evidence::review::policy_iri("other");
    let trig = format!(
        r#"
<{REPORTS}> {{
  <urn:c:main> a <{community}> ; <{level}> 0 ; <{count}> 2 ; <{member}> <http://example.org/resource/acme> ; <{report}> <urn:r:main> .
  <urn:r:main> a <{report_cls}> ;
    <http://www.w3.org/2000/01/rdf-schema#label> "Main Policy Community" ;
    <{summary}> "Summary under main policy." ;
    <{status}> "reviewed" ;
    <{rating}> 8 ;
    <{rp}> <{main_iri}> ;
    <{pv}> 1 ;
    <{ec}> "lin/cfg" ;
    <{finding}> <urn:r:main#f1> .
  <urn:r:main#f1> <{order}> 1 ;
    <{text}> "MAIN_POLICY_FINDING for alice_dup." ;
    <{about}> <http://example.org/resource/alice_dup> ;
    <{cites}> <<( <http://example.org/resource/alice_dup> <http://example.org/ontology#leads> <http://example.org/resource/payments> )>> ;
    <{cc}> <urn:claim:main-1> ;
    <{cd}> <urn:decision:main-1> .

  <urn:c:other> a <{community}> ; <{level}> 0 ; <{count}> 2 ; <{member}> <http://example.org/resource/acme> ; <{report}> <urn:r:other> .
  <urn:r:other> a <{report_cls}> ;
    <http://www.w3.org/2000/01/rdf-schema#label> "Other Policy Community" ;
    <{summary}> "OTHER_ONLY_TEXT summary." ;
    <{status}> "reviewed" ;
    <{rating}> 10 ;
    <{rp}> <{other_iri}> ;
    <{pv}> 1 ;
    <{ec}> "lin/cfg" ;
    <{finding}> <urn:r:other#f1> .
  <urn:r:other#f1> <{order}> 1 ;
    <{text}> "OTHER_ONLY_TEXT finding." ;
    <{about}> <http://example.org/resource/bob> ;
    <{cites}> <<( <http://example.org/resource/bob> <http://example.org/ontology#leads> <http://example.org/resource/ops> )>> .

  <urn:c:plain> a <{community}> ; <{level}> 0 ; <{count}> 2 ; <{member}> <http://example.org/resource/acme> ; <{report}> <urn:r:plain> .
  <urn:r:plain> a <{report_cls}> ;
    <http://www.w3.org/2000/01/rdf-schema#label> "Unpolicied Community" ;
    <{summary}> "PLAIN_ONLY_TEXT summary." ;
    <{status}> "reviewed" ;
    <{rating}> 9 ;
    <{finding}> <urn:r:plain#f1> .
  <urn:r:plain#f1> <{order}> 1 ;
    <{text}> "PLAIN_ONLY_TEXT finding." ;
    <{about}> <http://example.org/resource/carol> ;
    <{cites}> <<( <http://example.org/resource/carol> <http://example.org/ontology#leads> <http://example.org/resource/sec> )>> .
}}
"#,
        community = gr("Community"),
        level = gr("level"),
        count = gr("memberCount"),
        member = gr("member"),
        report = gr("report"),
        report_cls = gr("Report"),
        summary = gr("summary"),
        status = gr("status"),
        rating = gr("rating"),
        rp = gr("reviewPolicy"),
        pv = gr("policyVersion"),
        ec = gr("evidenceConfiguration"),
        finding = gr("finding"),
        order = gr("order"),
        text = gr("text"),
        about = gr("about"),
        cites = gr("cites"),
        cc = gr("citesClaim"),
        cd = gr("citesDecision"),
        main_iri = main_policy_iri.as_str(),
        other_iri = other_policy_iri.as_str(),
    );
    ds.store()
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    // Enable GraphRAG on `org`.
    let state_ds = st.get("org").unwrap();
    crate::settings::store_runtime(
        &st,
        &state_ds,
        &crate::settings::GRAPHRAG,
        &json!({ "enabled": true, "principal": "local", "projection": { "graphs": ["urn:g:org"] } }),
    )
    .unwrap();

    // 1) Global answer with reviewPolicy="main" AND matchingPolicy="people":
    //    - selects ONLY urn:r:main (excludes urn:r:other and urn:r:plain)
    //    - resolves alice_dup -> alice and passes the identity link to the map prompt
    let before = m.requests().len();
    let out = ask(
        &app,
        json!({
            "question": "What are the main themes across the organisation?",
            "mode": "global",
            "reviewPolicy": "main",
            "matchingPolicy": "people",
        }),
    )
    .await;
    assert_eq!(out["outcome"], "answered", "{out}");
    assert_eq!(out["mode"], "global", "{out}");
    assert_eq!(out["global"]["selected"], 1, "{out}");
    assert_eq!(out["global"]["reviewPolicy"]["name"], "main", "{out}");
    assert_eq!(out["global"]["reviewPolicy"]["version"], 1, "{out}");
    let reps = out["reports"].as_array().unwrap();
    assert_eq!(reps.len(), 1, "{out}");
    assert_eq!(reps[0]["iri"], "urn:r:main", "{out}");
    assert_eq!(reps[0]["reviewPolicy"]["iri"], main_policy_iri.as_str());
    assert_eq!(
        reps[0]["findings"][0]["about"],
        json!(["http://example.org/resource/alice_dup"])
    );
    assert_eq!(
        reps[0]["identities"],
        json!([{
            "entity": "http://example.org/resource/alice_dup",
            "representative": "http://example.org/resource/alice",
            "members": [
                "http://example.org/resource/alice",
                "http://example.org/resource/alice_dup"
            ],
            "membersComplete": true,
        }]),
        "{out}"
    );
    assert_eq!(out["resolution"]["policy"], "people", "{out}");
    assert_eq!(out["resolution"]["version"], 1, "{out}");
    assert_eq!(out["resolution"]["number"], 1, "{out}");
    assert!(out["resolution"]["revision"].is_string(), "{out}");
    assert_eq!(
        out["resolution"]["entities"], reps[0]["identities"],
        "{out}"
    );
    // No other-policy or unpolicied report reached any LLM request, and the map prompt saw
    // the resolved identity link without rewriting source IRIs.
    let sent = &m.requests()[before..];
    assert_eq!(sent.len(), 2, "map + reduce only: {}", sent.len());
    let map_body = sent[0].body.to_string();
    assert!(map_body.contains("MAIN_POLICY_FINDING"), "{map_body}");
    assert!(!map_body.contains("OTHER_ONLY_TEXT"), "{map_body}");
    assert!(!map_body.contains("PLAIN_ONLY_TEXT"), "{map_body}");
    assert!(
        map_body.contains("resolvesTo <http://example.org/resource/alice>"),
        "{map_body}"
    );

    // 2) Global answer with an empty matching policy map reports revision: null, number: 0.
    let empty_map_out = ask(
        &app,
        json!({
            "question": "What are the main themes across the organisation?",
            "mode": "global",
            "reviewPolicy": "main",
            "matchingPolicy": "empty",
        }),
    )
    .await;
    assert_eq!(empty_map_out["outcome"], "answered", "{empty_map_out}");
    assert_eq!(empty_map_out["mode"], "global", "{empty_map_out}");
    assert_eq!(empty_map_out["resolution"]["policy"], "empty");
    assert_eq!(empty_map_out["resolution"]["revision"], Value::Null);
    assert_eq!(empty_map_out["resolution"]["number"], 0);
    assert_eq!(empty_map_out["resolution"]["entities"], json!([]));
    assert_eq!(empty_map_out["reports"][0]["identities"], json!([]));

    // 3) Requesting reviewPolicy="unused" (no reports built under it) excludes main/other/plain
    //    reports and falls back to answering from the data with a note explaining why.
    let before = m.requests().len();
    let unused_out = ask(
        &app,
        json!({
            "question": "Which teams does Acme have?",
            "mode": "global",
            "reviewPolicy": "unused",
        }),
    )
    .await;
    assert_eq!(unused_out["outcome"], "answered", "{unused_out}");
    assert_eq!(unused_out["mode"], "local", "{unused_out}");
    assert!(unused_out.get("reports").is_none(), "{unused_out}");
    let notes = unused_out["notes"].to_string();
    assert!(
        notes.contains("no community report exists under reviewPolicy \\\"unused\\\""),
        "{notes}"
    );
    for req in &m.requests()[before..] {
        let s = req.body.to_string();
        assert!(
            !s.contains("MAIN_POLICY_FINDING")
                && !s.contains("OTHER_ONLY_TEXT")
                && !s.contains("PLAIN_ONLY_TEXT"),
            "{s}"
        );
    }

    // 3b) Local mode with reviewPolicy="main" includes ONLY urn:r:main in the `ground`
    //     SSE event and prompt (excluding other/plain even though acme is a gr:member of
    //     all three), and reviewPolicy="unused" emits no community reports in `ground`.
    let sse_event = |event_name: &'static str, body: Value| {
        let router = app.clone();
        async move {
            let req = Request::post("/org/ask")
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .body(Body::from(body.to_string()))
                .unwrap();
            let (st, text) = send(&router, req).await;
            assert_eq!(st, StatusCode::OK, "{text}");
            let mut ev = None;
            let needle = format!("event: {event_name}");
            for block in text.split("\n\n") {
                if block.lines().any(|l| l == needle) {
                    for line in block.lines() {
                        if let Some(d) = line.strip_prefix("data: ") {
                            ev = Some(serde_json::from_str::<Value>(d).unwrap());
                        }
                    }
                }
            }
            ev.unwrap_or_else(|| panic!("{event_name} SSE event missing in:\n{text}"))
        }
    };
    let before = m.requests().len();
    let local_main_ground = sse_event(
        "ground",
        json!({
            "question": "Which teams does Acme have?",
            "mode": "local",
            "reviewPolicy": "main",
        }),
    )
    .await;
    assert_eq!(
        local_main_ground["communities"],
        json!(["<urn:r:main>"]),
        "expected only main community report in ground event: {local_main_ground}"
    );
    assert!(
        m.requests()[before..]
            .iter()
            .any(|req| req.body.to_string().contains("Summary under main policy.")),
        "expected matching-policy report summary in local draft prompt"
    );
    for req in &m.requests()[before..] {
        let s = req.body.to_string();
        assert!(
            !s.contains("OTHER_ONLY_TEXT") && !s.contains("PLAIN_ONLY_TEXT"),
            "excluded community leaked into local grounding: {s}"
        );
    }

    let before = m.requests().len();
    let local_unused_ground = sse_event(
        "ground",
        json!({
            "question": "Which teams does Acme have?",
            "mode": "local",
            "reviewPolicy": "unused",
        }),
    )
    .await;
    assert!(
        local_unused_ground.get("communities").is_none(),
        "excluded communities appeared in ground event for unused policy: {local_unused_ground}"
    );
    for req in &m.requests()[before..] {
        let s = req.body.to_string();
        assert!(
            !s.contains("Summary under main policy.")
                && !s.contains("OTHER_ONLY_TEXT")
                && !s.contains("PLAIN_ONLY_TEXT"),
            "excluded community leaked into local grounding: {s}"
        );
    }

    // 3c) mode="auto" routes global questions with reviewPolicy and matchingPolicy to global.
    let auto_global = ask(
        &app,
        json!({
            "question": "What are the main themes across the organisation?",
            "mode": "auto",
            "reviewPolicy": "main",
            "matchingPolicy": "people",
        }),
    )
    .await;
    assert_eq!(auto_global["outcome"], "answered", "{auto_global}");
    assert_eq!(auto_global["mode"], "global", "{auto_global}");
    assert_eq!(auto_global["global"]["reviewPolicy"]["name"], "main");
    assert_eq!(auto_global["resolution"]["policy"], "people");
    assert_eq!(auto_global["reports"][0]["iri"], "urn:r:main");

    // 3d) SSE `reports` event omits policy/resolution fields when neither request policy is
    //     given (even though urn:r:main records gr:reviewPolicy), and includes them when
    //     policies are requested.
    let sse_reports_event = |body: Value| {
        let router = app.clone();
        async move {
            let req = Request::post("/org/ask")
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .body(Body::from(body.to_string()))
                .unwrap();
            let (st, text) = send(&router, req).await;
            assert_eq!(st, StatusCode::OK, "{text}");
            let mut ev = None;
            for block in text.split("\n\n") {
                if block.contains("event: reports") {
                    for line in block.lines() {
                        if let Some(d) = line.strip_prefix("data: ") {
                            ev = Some(serde_json::from_str::<Value>(d).unwrap());
                        }
                    }
                }
            }
            ev.expect("reports SSE event")
        }
    };
    let unpolicied_ev = sse_reports_event(json!({
        "question": "What are the main themes across the organisation?",
        "mode": "global",
    }))
    .await;
    assert!(
        unpolicied_ev.get("reviewPolicy").is_none(),
        "{unpolicied_ev}"
    );
    assert!(unpolicied_ev.get("resolution").is_none(), "{unpolicied_ev}");
    for r in unpolicied_ev["reports"].as_array().unwrap() {
        assert!(r.get("reviewPolicy").is_none(), "{unpolicied_ev}");
    }
    let policied_ev = sse_reports_event(json!({
        "question": "What are the main themes across the organisation?",
        "mode": "global",
        "reviewPolicy": "main",
        "matchingPolicy": "people",
    }))
    .await;
    assert_eq!(policied_ev["reviewPolicy"]["name"], "main", "{policied_ev}");
    assert_eq!(
        policied_ev["resolution"]["policy"], "people",
        "{policied_ev}"
    );
    assert_eq!(
        policied_ev["reports"][0]["reviewPolicy"]["iri"],
        main_policy_iri.as_str(),
        "{policied_ev}"
    );

    // 4) Splitting the identity match while the reduce call runs withholds the global answer
    //    with 403 forbidden.
    let ds_hook = ds.clone();
    let match_iri = match_rec.iri.clone();
    let match_digest = match_rec.digest;
    let done = std::sync::atomic::AtomicBool::new(false);
    let _ = hook.set(Box::new(move || {
        if !done.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let mut ro = ReviewOptions::new(iri("urn:principal:reviewer"));
            ro.if_head = Some(ds_hook.store().head_commit().seq);
            evidence::review(
                None,
                &ds_hook,
                &ds_hook,
                "people",
                1,
                &[DecisionInput {
                    key: "dm2".into(),
                    target: match_iri.clone(),
                    target_digest: match_digest,
                    disposition: Disposition::Rejected,
                    supersedes: Some(first_decision.clone()),
                }],
                &ro,
                &mut |_, _| all(),
                &Control::none(),
            )
            .unwrap();
        }
    }));
    let withheld = ask_with(
        &app,
        json!({
            "question": "What are the main themes across the organisation?",
            "mode": "global",
            "reviewPolicy": "main",
            "matchingPolicy": "people",
        }),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(withheld["outcome"], "error", "{withheld}");
    assert_eq!(withheld["error"]["code"], "forbidden", "{withheld}");
    assert!(
        withheld.get("summary").is_none()
            && withheld.get("reports").is_none()
            && withheld.get("resolution").is_none(),
        "{withheld}"
    );
}

/// Run an MCP tool directly against `st` on dataset `org` as a local admin principal.
fn mcp_tool(
    st: &Arc<AppState>,
    name: &str,
    args: Value,
) -> Result<Value, crate::mcp::errors::ToolError> {
    mcp_tool_with_cfg(st, crate::mcp::McpConfig::default(), name, args)
}

fn mcp_tool_with_cfg(
    st: &Arc<AppState>,
    cfg: crate::mcp::McpConfig,
    name: &str,
    args: Value,
) -> Result<Value, crate::mcp::errors::ToolError> {
    let mut call = crate::mcp::Call {
        arrived: std::time::Instant::now(),
        cancel: Default::default(),
        request_id: "t".into(),
        principal: crate::auth::Principal::local(),
        headers: None,
        held: None,
        grant: None,
        preparation: None,
    };
    let Value::Object(mut m) = args else {
        panic!("arguments must be an object");
    };
    m.insert("dataset".into(), "org".into());
    let server = crate::mcp::McpServer::new(st.clone(), cfg);
    Ok(match server.run_now(name, m, &mut call)? {
        crate::mcp::Outcome::Structured(v) => v,
        crate::mcp::Outcome::Text(t) => serde_json::from_str(&t).unwrap_or(Value::String(t)),
    })
}

/// D1: `community_reports` applies `reviewPolicy` before search bounds and rank fusion,
/// so > 50 stronger off-policy hits cannot displace an eligible report or alter its score.
#[cfg(feature = "text")]
#[tokio::test(flavor = "multi_thread")]
async fn policy_scoped_search_excludes_off_policy_hits_from_slots_and_scores() {
    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
    const OTHER_DEC: &str = "urn:ev:other-decisions";
    const OTHER_SUP: &str = "urn:ev:other-supports";
    const OTHER_OUT: &str = "urn:ev:other-accepted";

    let m = mock(Arc::new(OnceLock::new()));
    let (st, _app) = app(&m.url()).await;
    let ds = dataset(&st);

    let mut cfg = settings();
    cfg.decisions.push(OTHER_DEC.into());
    cfg.supports.push(OTHER_SUP.into());
    cfg.projections.push(OTHER_OUT.into());
    cfg.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: vec![],
            conflicts: ConflictMode::AdmitLabelled,
            boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
            decisions: OTHER_DEC.into(),
            supports: OTHER_SUP.into(),
            output: OTHER_OUT.into(),
        },
    );
    evidence::configure(&ds, &cfg, &Control::none()).unwrap();
    ds.indexes()
        .text()
        .enable(sparkles::text::TextConfig::default())
        .unwrap();

    let gr = crate::graphrag::guard::gr;
    let main_iri = sparkles::evidence::review::policy_iri("main");
    let other_iri = sparkles::evidence::review::policy_iri("other");
    let mut trig = format!("<{REPORTS}> {{\n");
    // One low-rated eligible report under "main" matching "zephyr".
    trig.push_str(&format!(
        r#"
  <urn:c:main-z> a <{community}> ; <{level}> 0 ; <{count}> 2 ; <{report}> <urn:r:main-z> .
  <urn:r:main-z> a <{report_cls}> ;
    <http://www.w3.org/2000/01/rdf-schema#label> "Quiet Main Report" ;
    <{summary}> "Mentions zephyr once." ;
    <{status}> "reviewed" ;
    <{rating}> 1 ;
    <{rp}> <{main_iri}> ;
    <{pv}> 1 ;
    <{finding}> <urn:r:main-z#f1> .
  <urn:r:main-z#f1> <{order}> 1 ; <{text}> "Single zephyr note." .
"#,
        community = gr("Community"),
        level = gr("level"),
        count = gr("memberCount"),
        report = gr("report"),
        report_cls = gr("Report"),
        summary = gr("summary"),
        status = gr("status"),
        rating = gr("rating"),
        rp = gr("reviewPolicy"),
        pv = gr("policyVersion"),
        finding = gr("finding"),
        order = gr("order"),
        text = gr("text"),
        main_iri = main_iri.as_str(),
    ));
    // Fifteen high-rated eligible "main" reports that do NOT mention "zephyr".
    for i in 0..15 {
        trig.push_str(&format!(
            r#"
  <urn:c:main-fill-{i}> a <{community}> ; <{level}> 0 ; <{count}> 2 ; <{report}> <urn:r:main-fill-{i}> .
  <urn:r:main-fill-{i}> a <{report_cls}> ;
    <http://www.w3.org/2000/01/rdf-schema#label> "High Rated Unrelated {i}" ;
    <{summary}> "Unrelated operations overview." ;
    <{status}> "reviewed" ;
    <{rating}> 9 ;
    <{rp}> <{main_iri}> ;
    <{pv}> 1 .
"#,
            community = gr("Community"),
            level = gr("level"),
            count = gr("memberCount"),
            report = gr("report"),
            report_cls = gr("Report"),
            summary = gr("summary"),
            status = gr("status"),
            rating = gr("rating"),
            rp = gr("reviewPolicy"),
            pv = gr("policyVersion"),
            main_iri = main_iri.as_str(),
        ));
    }
    // Sixty stronger off-policy reports (> LIST_DEPTH=50) repeating "zephyr".
    for i in 0..60 {
        let policy_line = if i % 2 == 0 {
            format!(
                "<{}> <{}> ; <{}> 1 ;",
                gr("reviewPolicy"),
                other_iri.as_str(),
                gr("policyVersion")
            )
        } else {
            String::new()
        };
        trig.push_str(&format!(
            r#"
  <urn:c:off-{i}> a <{community}> ; <{level}> 0 ; <{count}> 2 ; <{report}> <urn:r:off-{i}> .
  <urn:r:off-{i}> a <{report_cls}> ;
    <http://www.w3.org/2000/01/rdf-schema#label> "zephyr zephyr zephyr off {i}" ;
    <{summary}> "zephyr zephyr zephyr zephyr summary {i}" ;
    <{status}> "reviewed" ;
    <{rating}> 10 ;
    {policy_line}
    <{finding}> <urn:r:off-{i}#f1> .
  <urn:r:off-{i}#f1> <{order}> 1 ; <{text}> "zephyr zephyr zephyr finding {i}" .
"#,
            community = gr("Community"),
            level = gr("level"),
            count = gr("memberCount"),
            report = gr("report"),
            report_cls = gr("Report"),
            summary = gr("summary"),
            status = gr("status"),
            rating = gr("rating"),
            finding = gr("finding"),
            order = gr("order"),
            text = gr("text"),
        ));
    }
    trig.push_str("}\n");
    ds.store()
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    let state_ds = st.get("org").unwrap();
    crate::settings::store_runtime(
        &st,
        &state_ds,
        &crate::settings::GRAPHRAG,
        &json!({ "enabled": true, "principal": "local", "projection": { "graphs": ["urn:g:org"] } }),
    )
    .unwrap();

    let first = mcp_tool(
        &st,
        "community_reports",
        json!({ "query": "zephyr", "reviewPolicy": "main", "limit": 5 }),
    )
    .unwrap();
    assert_eq!(first["available"], true, "{first}");
    assert_eq!(first["ranking"], "search", "{first}");
    assert_eq!(first["reports"][0]["iri"], "urn:r:main-z", "{first}");
    let score_before = first["reports"][0]["score"].as_f64().unwrap();

    // Add 20 more stronger off-policy "zephyr" reports: the eligible report's selection
    // and fused RRF score must stay identical.
    let mut extra = format!("<{REPORTS}> {{\n");
    for i in 60..80 {
        extra.push_str(&format!(
            r#"
  <urn:c:off-{i}> a <{community}> ; <{level}> 0 ; <{count}> 2 ; <{report}> <urn:r:off-{i}> .
  <urn:r:off-{i}> a <{report_cls}> ;
    <http://www.w3.org/2000/01/rdf-schema#label> "zephyr zephyr zephyr zephyr extra {i}" ;
    <{summary}> "zephyr zephyr zephyr zephyr extra summary {i}" ;
    <{status}> "reviewed" ;
    <{rating}> 10 ;
    <{rp}> <{other_iri}> ;
    <{pv}> 1 .
"#,
            community = gr("Community"),
            level = gr("level"),
            count = gr("memberCount"),
            report = gr("report"),
            report_cls = gr("Report"),
            summary = gr("summary"),
            status = gr("status"),
            rating = gr("rating"),
            rp = gr("reviewPolicy"),
            pv = gr("policyVersion"),
            other_iri = other_iri.as_str(),
        ));
    }
    extra.push_str("}\n");
    ds.store()
        .load(&[Source::from_bytes(
            extra.into_bytes(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();

    let second = mcp_tool(
        &st,
        "community_reports",
        json!({ "query": "zephyr", "reviewPolicy": "main", "limit": 5 }),
    )
    .unwrap();
    assert_eq!(second["ranking"], "search", "{second}");
    assert_eq!(second["reports"][0]["iri"], "urn:r:main-z", "{second}");
    assert_eq!(
        second["reports"][0]["score"].as_f64().unwrap(),
        score_before,
        "excluded hits altered eligible report score"
    );
}

/// D2 + D4 + L2:
/// - Reports exceeding 10,000 referenced entities refuse explicit `mode: "global"` with
///   `422 incomplete-evidence` (0 model calls), while exact-linked `mode: "auto"` still
///   answers locally.
/// - Replacing a selected report mid-flight withholds the global answer with `403`.
/// - Detaching a matching-map output graph refuses `community_reports` and Ask with
///   `503 projection-unavailable` before any model call.
#[tokio::test(flavor = "multi_thread")]
async fn auto_mode_ignores_report_entity_cap_for_local_and_enforces_global_limits() {
    use sparkles::evidence::{IdentityInput, IdentityRelation, MatchingPolicy, MethodRef};

    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
    const IDENTITIES: &str = "urn:ev:identities";
    const MAPS: &str = "urn:ev:maps";
    const MAP: &str = "urn:ev:map";

    let hook: Arc<Hook> = Arc::new(OnceLock::new());
    let hook_c = hook.clone();
    let m = MockModel::start(move |r, _| {
        let body = r.body.to_string();
        if body.contains("\"Points\"") || body.contains("pick out what bears") {
            return (
                200,
                mock::openai(
                    &json!({
                        "points": [{
                            "text": "Overview point.",
                            "score": 90,
                            "reports": ["R1"],
                            "findings": ["F1.1"]
                        }]
                    })
                    .to_string(),
                ),
            );
        }
        if body.contains("\"GlobalAnswer\"") || body.contains("Points from the community reports") {
            if let Some(h) = hook_c.get() {
                h();
            }
            return (
                200,
                mock::openai(
                    &json!({
                        "text": "Global summary [1].",
                        "citations": [1]
                    })
                    .to_string(),
                ),
            );
        }
        if is_summary(r) {
            return (
                200,
                mock::openai(
                    &json!({ "text": "Local Acme teams [1].", "citations": [1], "claimCitations": [] })
                        .to_string(),
                ),
            );
        }
        (200, mock::openai(&draft(EASY)))
    });

    let (st, app) = app(&m.url()).await;
    let ds = dataset(&st);

    let mut cfg = settings();
    cfg.projections.push(MAP.into());
    cfg.identity.extend([IDENTITIES.into(), MAPS.into()]);
    cfg.matching_policies.insert(
        "people".into(),
        MatchingPolicy {
            version: 1,
            boundary: vec![IDENTITIES.into()],
            decisions: "urn:ev:decisions".into(),
            maps: MAPS.into(),
            output: MAP.into(),
        },
    );
    evidence::configure(&ds, &cfg, &Control::none()).unwrap();

    // Accept one identity match so the matching map has a live revision to check/detach.
    let mut po = ProposeOptions::new(iri("urn:principal:matcher"));
    po.identity_graph = Some(iri(IDENTITIES));
    let prop = evidence::propose(
        None,
        &ds,
        &ds,
        &EvidenceInput {
            identities: vec![IdentityInput {
                version: RecordVersion::V1,
                key: RecordKey::new("m1").unwrap(),
                occurrence_key: RecordKey::new("occ-m1").unwrap(),
                entities: vec![
                    iri("http://example.org/resource/alice"),
                    iri("http://example.org/resource/alice_dup"),
                ],
                relation: IdentityRelation::PossibleMatch,
                preferred: Some(iri("http://example.org/resource/alice")),
                attributed_author: iri("urn:matcher"),
                method: Some(MethodRef {
                    method: iri("urn:method"),
                    version: RecordKey::new("1").unwrap(),
                }),
            }],
            ..Default::default()
        },
        &po,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let mut ro = ReviewOptions::new(iri("urn:principal:reviewer"));
    ro.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        &ds,
        &ds,
        "people",
        1,
        &[DecisionInput {
            key: "dm1".into(),
            target: prop.records[0].iri.clone(),
            target_digest: prop.records[0].digest,
            disposition: Disposition::Accepted,
            supersedes: None,
        }],
        &ro,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();

    let gr = crate::graphrag::guard::gr;
    let main_iri = sparkles::evidence::review::policy_iri("main");
    let mut trig = format!(
        r#"<{REPORTS}> {{
  <urn:c:huge> a <{community}> ; <{level}> 0 ; <{count}> 10005 ; <{report}> <urn:r:huge> .
  <urn:r:huge> a <{report_cls}> ;
    <http://www.w3.org/2000/01/rdf-schema#label> "Huge Community" ;
    <{summary}> "References over 10,000 entities." ;
    <{status}> "reviewed" ;
    <{rating}> 8 ;
    <{rp}> <{main_iri}> ;
    <{pv}> 1 ;
    <{finding}> <urn:r:huge#f1> .
  <urn:r:huge#f1> <{order}> 1 ; <{text}> "Huge finding." .
"#,
        community = gr("Community"),
        level = gr("level"),
        count = gr("memberCount"),
        report = gr("report"),
        report_cls = gr("Report"),
        summary = gr("summary"),
        status = gr("status"),
        rating = gr("rating"),
        rp = gr("reviewPolicy"),
        pv = gr("policyVersion"),
        finding = gr("finding"),
        order = gr("order"),
        text = gr("text"),
        main_iri = main_iri.as_str(),
    );
    let about_pred = gr("about");
    for i in 0..10_005 {
        trig.push_str(&format!(
            "  <urn:r:huge#f1> <{about_pred}> <http://example.org/ent/{i}> .\n"
        ));
    }
    trig.push_str("}\n");
    ds.store()
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    let state_ds = st.get("org").unwrap();
    crate::settings::store_runtime(
        &st,
        &state_ds,
        &crate::settings::GRAPHRAG,
        &json!({ "enabled": true, "principal": "local", "projection": { "graphs": ["urn:g:org"] } }),
    )
    .unwrap();

    // 1) Explicit global mode fails with 422 incomplete-evidence and costs 0 model calls.
    let before = m.requests().len();
    let err = ask_with(
        &app,
        json!({
            "question": "Which teams does Acme have?",
            "mode": "global",
            "reviewPolicy": "main",
            "matchingPolicy": "people",
        }),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(err["error"]["code"], "incomplete-evidence", "{err}");
    assert_eq!(m.requests().len(), before);

    // 2) Exact-linked mode="auto" on the exact same fixture succeeds locally!
    let auto_local = ask(
        &app,
        json!({
            "question": "Which teams does Acme have?",
            "mode": "auto",
            "reviewPolicy": "main",
            "matchingPolicy": "people",
        }),
    )
    .await;
    assert_eq!(auto_local["outcome"], "answered", "{auto_local}");
    assert_eq!(auto_local["mode"], "local", "{auto_local}");

    // 3) Replace the huge report with a small report, then mutate the report mid-flight
    //    during reduce -> 403 forbidden.
    ds.update(&format!("CLEAR GRAPH <{REPORTS}>")).unwrap();
    let small_trig = format!(
        r#"<{REPORTS}> {{
  <urn:c:small> a <{community}> ; <{level}> 0 ; <{count}> 2 ; <{report}> <urn:r:small> .
  <urn:r:small> a <{report_cls}> ;
    <http://www.w3.org/2000/01/rdf-schema#label> "Small Community" ;
    <{summary}> "Original summary." ;
    <{status}> "reviewed" ;
    <{rating}> 8 ;
    <{rp}> <{main_iri}> ;
    <{pv}> 1 ;
    <{finding}> <urn:r:small#f1> .
  <urn:r:small#f1> <{order}> 1 ;
    <{text}> "Original finding." ;
    <{about}> <http://example.org/resource/alice_dup> .
}}"#,
        community = gr("Community"),
        level = gr("level"),
        count = gr("memberCount"),
        report = gr("report"),
        report_cls = gr("Report"),
        summary = gr("summary"),
        status = gr("status"),
        rating = gr("rating"),
        rp = gr("reviewPolicy"),
        pv = gr("policyVersion"),
        finding = gr("finding"),
        order = gr("order"),
        text = gr("text"),
        about = gr("about"),
        main_iri = main_iri.as_str(),
    );
    ds.store()
        .load(&[Source::from_bytes(
            small_trig.into_bytes(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();

    let ds_mut = ds.clone();
    let mutated = std::sync::atomic::AtomicBool::new(false);
    let summary_pred = gr("summary");
    let _ = hook.set(Box::new(move || {
        if !mutated.swap(true, std::sync::atomic::Ordering::SeqCst) {
            ds_mut
                .update(&format!(
                    "DELETE DATA {{ GRAPH <{REPORTS}> {{ <urn:r:small> <{summary_pred}> \"Original summary.\" }} }}; \
                     INSERT DATA {{ GRAPH <{REPORTS}> {{ <urn:r:small> <{summary_pred}> \"Replaced summary.\" }} }}"
                ))
                .unwrap();
        }
    }));
    let replaced = ask_with(
        &app,
        json!({
            "question": "What are the main themes across the organisation?",
            "mode": "global",
            "reviewPolicy": "main",
            "matchingPolicy": "people",
        }),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(replaced["error"]["code"], "forbidden", "{replaced}");

    // 4) Detaching the matching map's output graph refuses community_reports and Ask
    //    with 503 projection-unavailable before any model call.
    evidence::detach(&ds, MAP).unwrap();
    let before = m.requests().len();
    let tool_err = mcp_tool(
        &st,
        "community_reports",
        json!({ "reviewPolicy": "main", "matchingPolicy": "people" }),
    )
    .unwrap_err();
    assert_eq!(tool_err.status, 503, "{tool_err:?}");
    assert_eq!(
        tool_err.code.as_ref(),
        "projection-unavailable",
        "{tool_err:?}"
    );

    let ask_err = ask_with(
        &app,
        json!({
            "question": "What are the main themes across the organisation?",
            "mode": "global",
            "reviewPolicy": "main",
            "matchingPolicy": "people",
        }),
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    assert_eq!(
        ask_err["error"]["code"], "projection-unavailable",
        "{ask_err}"
    );
    assert_eq!(m.requests().len(), before);
}

/// R2-2: Excluded hits never exhaust the text index `maxHits` budget either below or above
/// the 4,096-subject pushdown threshold, and broad eligible queries stay bounded without
/// returning `507 budget-rows`.
#[cfg(feature = "text")]
#[tokio::test(flavor = "multi_thread")]
async fn policy_scoped_text_search_ignores_excluded_hits_across_pushdown_threshold_and_max_hits() {
    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
    const OTHER_DEC: &str = "urn:ev:other-decisions";
    const OTHER_SUP: &str = "urn:ev:other-supports";
    const OTHER_OUT: &str = "urn:ev:other-accepted";

    let m = mock(Arc::new(OnceLock::new()));
    let (st, _app) = app(&m.url()).await;
    let ds = dataset(&st);

    let mut cfg = settings();
    cfg.decisions.push(OTHER_DEC.into());
    cfg.supports.push(OTHER_SUP.into());
    cfg.projections.push(OTHER_OUT.into());
    cfg.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: vec![],
            conflicts: ConflictMode::AdmitLabelled,
            boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
            decisions: OTHER_DEC.into(),
            supports: OTHER_SUP.into(),
            output: OTHER_OUT.into(),
        },
    );
    evidence::configure(&ds, &cfg, &Control::none()).unwrap();
    ds.indexes()
        .text()
        .enable(sparkles::text::TextConfig {
            max_hits: 100,
            ..sparkles::text::TextConfig::default()
        })
        .unwrap();

    let gr = crate::graphrag::guard::gr;
    let main_iri = sparkles::evidence::review::policy_iri("main");
    let other_iri = sparkles::evidence::review::policy_iri("other");
    let (c_cls, lv, cnt, rep, r_cls, sum, stat, rat, rp, pv, fnd, ord, txt) = (
        gr("Community"),
        gr("level"),
        gr("memberCount"),
        gr("report"),
        gr("Report"),
        gr("summary"),
        gr("status"),
        gr("rating"),
        gr("reviewPolicy"),
        gr("policyVersion"),
        gr("finding"),
        gr("order"),
        gr("text"),
    );

    // Phase 1 (below 4,096 eligible subjects): 1 eligible "zephyr" hit + 150 off-policy
    // "zephyr" hits (> maxHits = 100).
    let mut trig = format!("<{REPORTS}> {{\n");
    trig.push_str(&format!(
        r#"  <urn:c:main-hit> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:main-hit> .
  <urn:r:main-hit> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Quiet Main" ; <{sum}> "Mentions zephyr overview." ; <{stat}> "reviewed" ; <{rat}> 1 ; <{rp}> <{main}> ; <{pv}> 1 ; <{fnd}> <urn:r:main-hit#f1> .
  <urn:r:main-hit#f1> <{ord}> 1 ; <{txt}> "Single zephyr note." .
"#,
        main = main_iri.as_str(),
    ));
    for i in 0..150 {
        trig.push_str(&format!(
            r#"  <urn:c:off-{i}> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:off-{i}> .
  <urn:r:off-{i}> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Off {i}" ; <{sum}> "zephyr zephyr excluded overview {i}." ; <{stat}> "reviewed" ; <{rat}> 10 ; <{rp}> <{other}> ; <{pv}> 1 .
"#,
            other = other_iri.as_str(),
        ));
    }
    trig.push_str("}\n");
    ds.store()
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    let state_ds = st.get("org").unwrap();
    crate::settings::store_runtime(
        &st,
        &state_ds,
        &crate::settings::GRAPHRAG,
        &json!({ "enabled": true, "principal": "local", "projection": { "graphs": ["urn:g:org"] } }),
    )
    .unwrap();

    let below = mcp_tool(
        &st,
        "community_reports",
        json!({ "query": "zephyr", "reviewPolicy": "main", "level": 0, "limit": 5 }),
    )
    .unwrap();
    assert_eq!(below["ranking"], "search", "{below}");
    assert_eq!(below["reports"][0]["iri"], "urn:r:main-hit", "{below}");

    // Phase 2 (above 4,096 eligible subjects): add 2,050 eligible reports with 1 finding
    // each (4,102 eligible report + finding subjects > TEXT_PUSHDOWN_SUBJECTS = 4,096).
    let mut bulk = format!("<{REPORTS}> {{\n");
    for i in 0..2050 {
        bulk.push_str(&format!(
            r#"  <urn:c:fill-{i}> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:fill-{i}> .
  <urn:r:fill-{i}> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Fill {i}" ; <{sum}> "Unrelated overview {i}." ; <{stat}> "reviewed" ; <{rat}> 5 ; <{rp}> <{main}> ; <{pv}> 1 ; <{fnd}> <urn:r:fill-{i}#f1> .
  <urn:r:fill-{i}#f1> <{ord}> 1 ; <{txt}> "Background detail {i}." .
"#,
            main = main_iri.as_str(),
        ));
    }
    bulk.push_str("}\n");
    ds.store()
        .load(&[Source::from_bytes(bulk.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    let above = mcp_tool(
        &st,
        "community_reports",
        json!({ "query": "zephyr", "reviewPolicy": "main", "level": 0, "limit": 5 }),
    )
    .unwrap();
    assert_eq!(above["ranking"], "search", "{above}");
    assert_eq!(above["reports"][0]["iri"], "urn:r:main-hit", "{above}");

    // Phase 3 (broad query matching > maxHits = 100 eligible reports): succeeds on both
    // policied and unpolicied paths without `507 budget-rows`.
    let broad_policied = mcp_tool(
        &st,
        "community_reports",
        json!({ "query": "overview", "reviewPolicy": "main", "level": 0, "limit": 5 }),
    )
    .unwrap();
    assert_eq!(broad_policied["ranking"], "search", "{broad_policied}");
    assert_eq!(broad_policied["reports"].as_array().unwrap().len(), 5);

    let broad_unpolicied = mcp_tool(
        &st,
        "community_reports",
        json!({ "query": "overview", "level": 0, "limit": 5 }),
    )
    .unwrap();
    assert_eq!(broad_unpolicied["ranking"], "search", "{broad_unpolicied}");
    assert_eq!(broad_unpolicied["reports"].as_array().unwrap().len(), 5);
}

/// R2-3: Multi-term text ranking over eligible reports is independent of excluded reports,
/// so adding many off-policy reports matching one query term cannot break an eligible tie
/// or shift reciprocal-rank scores.
#[cfg(feature = "text")]
#[tokio::test(flavor = "multi_thread")]
async fn policy_scoped_multi_term_text_ranking_is_independent_of_excluded_reports() {
    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
    const OTHER_DEC: &str = "urn:ev:other-decisions";
    const OTHER_SUP: &str = "urn:ev:other-supports";
    const OTHER_OUT: &str = "urn:ev:other-accepted";

    let m = mock(Arc::new(OnceLock::new()));
    let (st, _app) = app(&m.url()).await;
    let ds = dataset(&st);

    let mut cfg = settings();
    cfg.decisions.push(OTHER_DEC.into());
    cfg.supports.push(OTHER_SUP.into());
    cfg.projections.push(OTHER_OUT.into());
    cfg.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: vec![],
            conflicts: ConflictMode::AdmitLabelled,
            boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
            decisions: OTHER_DEC.into(),
            supports: OTHER_SUP.into(),
            output: OTHER_OUT.into(),
        },
    );
    evidence::configure(&ds, &cfg, &Control::none()).unwrap();
    ds.indexes()
        .text()
        .enable(sparkles::text::TextConfig::default())
        .unwrap();

    let gr = crate::graphrag::guard::gr;
    let main_iri = sparkles::evidence::review::policy_iri("main");
    let other_iri = sparkles::evidence::review::policy_iri("other");
    let (c_cls, lv, cnt, rep, r_cls, sum, stat, rat, rp, pv) = (
        gr("Community"),
        gr("level"),
        gr("memberCount"),
        gr("report"),
        gr("Report"),
        gr("summary"),
        gr("status"),
        gr("rating"),
        gr("reviewPolicy"),
        gr("policyVersion"),
    );

    // Two equal-length eligible reports: one matches "zephyr", one matches "quartz".
    let trig = format!(
        r#"<{REPORTS}> {{
  <urn:c:main-z> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:main-z> .
  <urn:r:main-z> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Report Alpha" ; <{sum}> "Team zephyr overview." ; <{stat}> "reviewed" ; <{rat}> 5 ; <{rp}> <{main}> ; <{pv}> 1 .

  <urn:c:main-q> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:main-q> .
  <urn:r:main-q> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Report Bravo" ; <{sum}> "Team quartz overview." ; <{stat}> "reviewed" ; <{rat}> 5 ; <{rp}> <{main}> ; <{pv}> 1 .
}}"#,
        main = main_iri.as_str(),
    );
    ds.store()
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    let state_ds = st.get("org").unwrap();
    crate::settings::store_runtime(
        &st,
        &state_ds,
        &crate::settings::GRAPHRAG,
        &json!({ "enabled": true, "principal": "local", "projection": { "graphs": ["urn:g:org"] } }),
    )
    .unwrap();

    let before = mcp_tool(
        &st,
        "community_reports",
        json!({ "query": "zephyr quartz", "reviewPolicy": "main", "limit": 5 }),
    )
    .unwrap();
    assert_eq!(before["ranking"], "search", "{before}");
    let before_reports = before["reports"].as_array().unwrap();
    assert_eq!(before_reports.len(), 2, "{before}");
    let s0_before = before_reports[0]["score"].as_f64().unwrap();
    let s1_before = before_reports[1]["score"].as_f64().unwrap();
    assert_eq!(
        s0_before, s1_before,
        "equal-length single-term eligible matches should tie: {before}"
    );

    // Add 60 off-policy summaries matching ONLY "zephyr" to skew global corpus IDF.
    let mut extra = format!("<{REPORTS}> {{\n");
    for i in 0..60 {
        extra.push_str(&format!(
            r#"  <urn:c:off-{i}> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:off-{i}> .
  <urn:r:off-{i}> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Off {i}" ; <{sum}> "Team zephyr excluded {i}." ; <{stat}> "reviewed" ; <{rat}> 10 ; <{rp}> <{other}> ; <{pv}> 1 .
"#,
            other = other_iri.as_str(),
        ));
    }
    extra.push_str("}\n");
    ds.store()
        .load(&[Source::from_bytes(
            extra.into_bytes(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();

    let after = mcp_tool(
        &st,
        "community_reports",
        json!({ "query": "zephyr quartz", "reviewPolicy": "main", "limit": 5 }),
    )
    .unwrap();
    assert_eq!(after["ranking"], "search", "{after}");
    assert_eq!(
        after["reports"], before["reports"],
        "excluded reports altered eligible multi-term ranking or scores"
    );
}

/// R2-5: Draft-time report mutations during `mode: "auto"`:
/// 1. Unpolicied `auto` with `explore: false` where reports disappear during the scoped
///    draft runs an unscoped local draft and answers locally (C22 §7.2).
/// 2. Unpolicied `auto` where reports are replaced during the scoped draft delivers the
///    replacement report text to the first map prompt.
/// 3. Policy-bound `auto` where reports disappear during the scoped draft withholds with
///    `403 forbidden`.
#[tokio::test(flavor = "multi_thread")]
async fn auto_ask_handles_draft_time_report_disappearance_and_replacement() {
    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";

    type DraftHook = Arc<std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>>;
    let draft_hook: DraftHook = Arc::new(std::sync::Mutex::new(None));
    let draft_hook_c = draft_hook.clone();
    let m = MockModel::start(move |r, _| {
        let body = r.body.to_string();
        if body.contains("\"Points\"") || body.contains("pick out what bears") {
            return (
                200,
                mock::openai(
                    &json!({
                        "points": [{
                            "text": "Fresh replacement point.",
                            "score": 90,
                            "reports": ["R1"],
                            "findings": ["F1.1"]
                        }]
                    })
                    .to_string(),
                ),
            );
        }
        if body.contains("\"GlobalAnswer\"") || body.contains("Points from the community reports") {
            return (
                200,
                mock::openai(
                    &json!({
                        "text": "Global summary from fresh reports [1].",
                        "citations": [1]
                    })
                    .to_string(),
                ),
            );
        }
        if is_summary(r) {
            return (
                200,
                mock::openai(
                    &json!({
                        "text": "Local fallback answer [1].",
                        "citations": [1],
                        "claimCitations": []
                    })
                    .to_string(),
                ),
            );
        }
        // Scoped draft asks for `ScopedDraft`: fire the draft-time hook and return global
        // with an empty SPARQL query. Unscoped local draft returns `EASY`.
        if body.contains("ScopedDraft") {
            if let Some(h) = draft_hook_c.lock().unwrap().take() {
                h();
            }
            return (
                200,
                mock::openai(
                    &json!({
                        "scope": "global",
                        "query": "",
                        "explanation": "dataset-wide question",
                        "assumptions": [],
                        "clarify": { "question": "", "choices": [] },
                        "graph": { "subject": "", "predicate": "", "object": "" }
                    })
                    .to_string(),
                ),
            );
        }
        (200, mock::openai(&draft(EASY)))
    });

    let (st, app) = app(&m.url()).await;
    let ds = dataset(&st);
    let gr = crate::graphrag::guard::gr;
    let main_iri = sparkles::evidence::review::policy_iri("main");
    let load_report = |ds: &sparkles::Dataset, marker: &str| {
        let trig = format!(
            r#"<{REPORTS}> {{
  <urn:c:r1> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:r1> .
  <urn:r:r1> a <{r_cls}> ;
    <http://www.w3.org/2000/01/rdf-schema#label> "Community One" ;
    <{sum}> "{marker} summary." ;
    <{stat}> "reviewed" ;
    <{rat}> 8 ;
    <{rp}> <{main}> ;
    <{pv}> 1 ;
    <{fnd}> <urn:r:r1#f1> .
  <urn:r:r1#f1> <{ord}> 1 ; <{txt}> "{marker} finding." .
}}"#,
            c_cls = gr("Community"),
            lv = gr("level"),
            cnt = gr("memberCount"),
            rep = gr("report"),
            r_cls = gr("Report"),
            sum = gr("summary"),
            stat = gr("status"),
            rat = gr("rating"),
            rp = gr("reviewPolicy"),
            pv = gr("policyVersion"),
            fnd = gr("finding"),
            ord = gr("order"),
            txt = gr("text"),
            main = main_iri.as_str(),
        );
        ds.update(&format!("CLEAR SILENT GRAPH <{REPORTS}>"))
            .unwrap();
        ds.store()
            .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
            .unwrap();
    };

    let state_ds = st.get("org").unwrap();
    crate::settings::store_runtime(
        &st,
        &state_ds,
        &crate::settings::GRAPHRAG,
        &json!({ "enabled": true, "principal": "local", "projection": { "graphs": ["urn:g:org"] } }),
    )
    .unwrap();

    // 1) Unpolicied auto with `explore: false`: reports disappear during scoped draft ->
    //    runs unscoped local draft and answers locally rather than `unanswerable`.
    load_report(&ds, "ORIGINAL_BEFORE_DELETE");
    let ds_clear = ds.clone();
    *draft_hook.lock().unwrap() = Some(Box::new(move || {
        ds_clear
            .update(&format!("CLEAR GRAPH <{REPORTS}>"))
            .unwrap();
    }));
    let before = m.requests().len();
    let local_out = ask(
        &app,
        json!({
            "question": "What are the main themes across the organisation?",
            "mode": "auto",
            "explore": false,
        }),
    )
    .await;
    assert_eq!(local_out["outcome"], "answered", "{local_out}");
    assert_eq!(local_out["mode"], "local", "{local_out}");
    let reqs = &m.requests()[before..];
    assert!(
        reqs.len() >= 2
            && reqs[0].body.to_string().contains("ScopedDraft")
            && !reqs[1].body.to_string().contains("ScopedDraft"),
        "expected scoped draft followed by unscoped local draft, got {} requests",
        reqs.len()
    );

    // 2) Unpolicied auto: report text is replaced during scoped draft -> fresh selection
    //    reaches the first map prompt.
    load_report(&ds, "ORIGINAL_BEFORE_REPLACE");
    let ds_replace = ds.clone();
    let sum_pred = gr("summary");
    let txt_pred = gr("text");
    *draft_hook.lock().unwrap() = Some(Box::new(move || {
        ds_replace
            .update(&format!(
                "DELETE DATA {{ GRAPH <{REPORTS}> {{ <urn:r:r1> <{sum_pred}> \"ORIGINAL_BEFORE_REPLACE summary.\" . <urn:r:r1#f1> <{txt_pred}> \"ORIGINAL_BEFORE_REPLACE finding.\" }} }}; \
                 INSERT DATA {{ GRAPH <{REPORTS}> {{ <urn:r:r1> <{sum_pred}> \"REPLACED_DURING_DRAFT summary.\" . <urn:r:r1#f1> <{txt_pred}> \"REPLACED_DURING_DRAFT finding.\" }} }}"
            ))
            .unwrap();
    }));
    let before = m.requests().len();
    let replaced_out = ask(
        &app,
        json!({
            "question": "What are the main themes across the organisation?",
            "mode": "auto",
            "explore": false,
        }),
    )
    .await;
    assert_eq!(replaced_out["outcome"], "answered", "{replaced_out}");
    assert_eq!(replaced_out["mode"], "global", "{replaced_out}");
    let all_reqs = m.requests();
    let map_req = all_reqs[before..]
        .iter()
        .find(|r| {
            let s = r.body.to_string();
            s.contains("\"Points\"") || s.contains("pick out what bears")
        })
        .expect("map request");
    let map_body = map_req.body.to_string();
    assert!(
        map_body.contains("REPLACED_DURING_DRAFT"),
        "map prompt missed draft-time replacement: {map_body}"
    );
    assert!(
        !map_body.contains("ORIGINAL_BEFORE_REPLACE"),
        "map prompt used stale pre-draft report: {map_body}"
    );

    // 3) Policy-bound auto: reports disappear during scoped draft -> still withholds with
    //    403 forbidden.
    load_report(&ds, "POLICIED_BEFORE_DELETE");
    let ds_clear2 = ds.clone();
    *draft_hook.lock().unwrap() = Some(Box::new(move || {
        ds_clear2
            .update(&format!("CLEAR GRAPH <{REPORTS}>"))
            .unwrap();
    }));
    let withheld = ask_with(
        &app,
        json!({
            "question": "What are the main themes across the organisation?",
            "mode": "auto",
            "reviewPolicy": "main",
            "explore": false,
        }),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(withheld["outcome"], "error", "{withheld}");
    assert_eq!(withheld["error"]["code"], "forbidden", "{withheld}");
}

/// R2-7: End-to-end `community_reports` multi-report shared-cluster truncation across
/// `ResolutionDoc::read` chunks (`> MAX_RESOLVE = 100`), checking matching member prefixes
/// and `membersComplete: false` in both `resolution.entities` and every report's
/// `identities` array.
#[tokio::test(flavor = "multi_thread")]
async fn community_reports_bounds_shared_cluster_across_chunks_and_reports() {
    use sparkles::evidence::{IdentityInput, IdentityRelation, MatchingPolicy, MethodRef};

    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
    const IDENTITIES: &str = "urn:ev:identities";
    const MAPS: &str = "urn:ev:maps";
    const MAP: &str = "urn:ev:map";
    const CLUSTER_SIZE: usize = 105; // > MAX_RESOLVE (100) so ResolutionDoc::read spans 2 chunks

    let m = mock(Arc::new(OnceLock::new()));
    let (st, _app) = app(&m.url()).await;
    let ds = dataset(&st);

    let mut cfg = settings();
    cfg.projections.push(MAP.into());
    cfg.identity.extend([IDENTITIES.into(), MAPS.into()]);
    cfg.matching_policies.insert(
        "people".into(),
        MatchingPolicy {
            version: 1,
            boundary: vec![IDENTITIES.into()],
            decisions: "urn:ev:decisions".into(),
            maps: MAPS.into(),
            output: MAP.into(),
        },
    );
    evidence::configure(&ds, &cfg, &Control::none()).unwrap();

    // Propose and accept one 105-member cluster linked to m000.
    let rep_iri = iri("http://example.org/cluster/m000");
    let mut id_inputs = Vec::with_capacity(CLUSTER_SIZE - 1);
    for i in 1..CLUSTER_SIZE {
        id_inputs.push(IdentityInput {
            version: RecordVersion::V1,
            key: RecordKey::new(format!("id-{i:03}")).unwrap(),
            occurrence_key: RecordKey::new(format!("occ-{i:03}")).unwrap(),
            entities: vec![
                rep_iri.clone(),
                iri(&format!("http://example.org/cluster/m{i:03}")),
            ],
            relation: IdentityRelation::PossibleMatch,
            preferred: Some(rep_iri.clone()),
            attributed_author: iri("urn:matcher"),
            method: Some(MethodRef {
                method: iri("urn:method"),
                version: RecordKey::new("1").unwrap(),
            }),
        });
    }
    let mut po = ProposeOptions::new(iri("urn:principal:matcher"));
    po.identity_graph = Some(iri(IDENTITIES));
    let prop = evidence::propose(
        None,
        &ds,
        &ds,
        &EvidenceInput {
            identities: id_inputs,
            ..Default::default()
        },
        &po,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let decisions: Vec<DecisionInput> = prop
        .records
        .iter()
        .enumerate()
        .map(|(i, rec)| DecisionInput {
            key: format!("dec-{i:03}"),
            target: rec.iri.clone(),
            target_digest: rec.digest,
            disposition: Disposition::Accepted,
            supersedes: None,
        })
        .collect();
    let mut ro = ReviewOptions::new(iri("urn:principal:reviewer"));
    ro.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        &ds,
        &ds,
        "people",
        1,
        &decisions,
        &ro,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();

    // Two eligible reports each referencing all 105 members of the shared cluster:
    // 105 * (1 + 2 copies) * 105 members = 33,075 > 8,192 (MAX_IDENTITY_MEMBER_ENTRIES).
    let gr = crate::graphrag::guard::gr;
    let main_iri = sparkles::evidence::review::policy_iri("main");
    let mut trig = format!("<{REPORTS}> {{\n");
    for r_i in 0..2 {
        trig.push_str(&format!(
            r#"  <urn:c:shared-{r_i}> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> {CLUSTER_SIZE} ; <{rep}> <urn:r:shared-{r_i}> .
  <urn:r:shared-{r_i}> a <{r_cls}> ;
    <http://www.w3.org/2000/01/rdf-schema#label> "Shared Cluster {r_i}" ;
    <{sum}> "Shared cluster summary {r_i}." ;
    <{stat}> "reviewed" ;
    <{rat}> 8 ;
    <{rp}> <{main}> ;
    <{pv}> 1 ;
    <{fnd}> <urn:r:shared-{r_i}#f1> .
  <urn:r:shared-{r_i}#f1> <{ord}> 1 ; <{txt}> "Shared cluster finding {r_i}." .
"#,
            c_cls = gr("Community"),
            lv = gr("level"),
            cnt = gr("memberCount"),
            rep = gr("report"),
            r_cls = gr("Report"),
            sum = gr("summary"),
            stat = gr("status"),
            rat = gr("rating"),
            rp = gr("reviewPolicy"),
            pv = gr("policyVersion"),
            fnd = gr("finding"),
            ord = gr("order"),
            txt = gr("text"),
            main = main_iri.as_str(),
        ));
        let about = gr("about");
        for i in 0..CLUSTER_SIZE {
            trig.push_str(&format!(
                "  <urn:r:shared-{r_i}#f1> <{about}> <http://example.org/cluster/m{i:03}> .\n"
            ));
        }
    }
    trig.push_str("}\n");
    ds.store()
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    let state_ds = st.get("org").unwrap();
    crate::settings::store_runtime(
        &st,
        &state_ds,
        &crate::settings::GRAPHRAG,
        &json!({ "enabled": true, "principal": "local", "projection": { "graphs": ["urn:g:org"] } }),
    )
    .unwrap();

    let out = mcp_tool(
        &st,
        "community_reports",
        json!({ "reviewPolicy": "main", "matchingPolicy": "people" }),
    )
    .unwrap();
    assert_eq!(out["available"], true, "{out}");
    let top_entities = out["resolution"]["entities"].as_array().unwrap();
    assert_eq!(top_entities.len(), CLUSTER_SIZE);
    let reports = out["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 2);

    let cap = top_entities[0]["members"].as_array().unwrap().len();
    assert!(
        cap > 0 && cap < CLUSTER_SIZE,
        "expected truncation, cap={cap}"
    );
    let mut total_member_entries = 0usize;
    for ent in top_entities {
        let members = ent["members"].as_array().unwrap();
        assert_eq!(members.len(), cap);
        assert_eq!(ent["membersComplete"], false);
        assert_eq!(ent["representative"], rep_iri.as_str());
        total_member_entries += members.len();
    }
    for rep in reports {
        let ids = rep["identities"].as_array().unwrap();
        assert_eq!(ids.len(), CLUSTER_SIZE);
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(
                id, &top_entities[i],
                "per-report identity copy diverged from resolution.entities"
            );
            total_member_entries += id["members"].as_array().unwrap().len();
        }
    }
    assert!(
        total_member_entries <= 8_192,
        "aggregate member entries {total_member_entries} exceeded 8,192 budget"
    );
}

/// R3-1: Eligible text ranking streams in bounded report batches instead of materializing
/// all eligible literals at once, so sparse and broad queries succeed under tight
/// `max_rows` and `query_memory_bytes` budgets that reject a corpus-sized literal SELECT.
#[cfg(feature = "text")]
#[tokio::test(flavor = "multi_thread")]
async fn policy_scoped_text_search_streams_under_tight_row_and_memory_budgets() {
    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
    const OTHER_DEC: &str = "urn:ev:other-decisions";
    const OTHER_SUP: &str = "urn:ev:other-supports";
    const OTHER_OUT: &str = "urn:ev:other-accepted";

    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    // Above the 550 total reports (so `level_counts` and report enumeration fit), but far
    // below the 3,000+ eligible literal rows (6 literals per report) that an unbatched
    // `SELECT ?r ?lit` materializes.
    st.limits.max_rows = 1_000;
    st.limits.max_result_bytes = None;
    let st = Arc::new(st);
    let state_ds = st.attach("org", DbType::Mem, None).unwrap();
    let p = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    state_ds
        .dataset
        .store()
        .activate_memory_configuration_history(&p)
        .unwrap();
    state_ds
        .store
        .load(&[Source::from_bytes(
            ORG.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let ds = state_ds.dataset.clone();

    let mut cfg = settings();
    cfg.decisions.push(OTHER_DEC.into());
    cfg.supports.push(OTHER_SUP.into());
    cfg.projections.push(OTHER_OUT.into());
    cfg.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: vec![],
            conflicts: ConflictMode::AdmitLabelled,
            boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
            decisions: OTHER_DEC.into(),
            supports: OTHER_SUP.into(),
            output: OTHER_OUT.into(),
        },
    );
    evidence::configure(&ds, &cfg, &Control::none()).unwrap();
    ds.indexes()
        .text()
        .enable(sparkles::text::TextConfig::default())
        .unwrap();

    let gr = crate::graphrag::guard::gr;
    let main_iri = sparkles::evidence::review::policy_iri("main");
    let other_iri = sparkles::evidence::review::policy_iri("other");
    let (c_cls, lv, cnt, rep, r_cls, sum, stat, rat, rp, pv, fnd, ord, txt, abt, cts, cclm, cdec) = (
        gr("Community"),
        gr("level"),
        gr("memberCount"),
        gr("report"),
        gr("Report"),
        gr("summary"),
        gr("status"),
        gr("rating"),
        gr("reviewPolicy"),
        gr("policyVersion"),
        gr("finding"),
        gr("order"),
        gr("text"),
        gr("about"),
        gr("cites"),
        gr("citesClaim"),
        gr("citesDecision"),
    );

    // 500 eligible reports * (1 label + 1 summary + 4 findings) = 3,000 eligible literals,
    // with ~900-byte summaries and findings (~2.25 MiB of literal payloads), PLUS 2,199
    // `gr:about` citations across unselected reports (well above `max_rows = 1_000`) to
    // verify `with_findings_in` never scans citations of unselected reports (R4-2).
    let pad = "detail ".repeat(120);
    let mut trig = format!("<{REPORTS}> {{\n");
    trig.push_str(&format!(
        r#"  <urn:c:main-hit> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:main-hit> .
  <urn:r:main-hit> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Quiet Main" ; <{sum}> "Mentions zephyr overview {pad}" ; <{stat}> "reviewed" ; <{rat}> 1 ; <{rp}> <{main}> ; <{pv}> 1 ; <{fnd}> <urn:r:main-hit#f1>, <urn:r:main-hit#f2>, <urn:r:main-hit#f3>, <urn:r:main-hit#f4> .
  <urn:r:main-hit#f1> <{ord}> 1 ; <{txt}> "Single zephyr note {pad}" ; <{abt}> <urn:ent:main-1> ; <{cts}> <urn:fact:main-1> ; <{cclm}> <urn:claim:main-1> ; <{cdec}> <urn:dec:main-1> .
  <urn:r:main-hit#f2> <{ord}> 2 ; <{txt}> "Background note two {pad}" ; <{abt}> <urn:ent:main-2> .
  <urn:r:main-hit#f3> <{ord}> 3 ; <{txt}> "Background note three {pad}" ; <{abt}> <urn:ent:main-3> .
  <urn:r:main-hit#f4> <{ord}> 4 ; <{txt}> "Uncited background note four {pad}" .
"#,
        main = main_iri.as_str(),
    ));
    for i in 0..499 {
        trig.push_str(&format!(
            r#"  <urn:c:fill-{i}> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:fill-{i}> .
  <urn:r:fill-{i}> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Fill {i}" ; <{sum}> "Unrelated overview {i} {pad}" ; <{stat}> "reviewed" ; <{rat}> 5 ; <{rp}> <{main}> ; <{pv}> 1 ; <{fnd}> <urn:r:fill-{i}#f1>, <urn:r:fill-{i}#f2>, <urn:r:fill-{i}#f3>, <urn:r:fill-{i}#f4> .
  <urn:r:fill-{i}#f1> <{ord}> 1 ; <{txt}> "Background detail a {i} {pad}" ; <{abt}> <urn:ent:fill-{i}-1> .
  <urn:r:fill-{i}#f2> <{ord}> 2 ; <{txt}> "Background detail b {i} {pad}" ; <{abt}> <urn:ent:fill-{i}-2> .
  <urn:r:fill-{i}#f3> <{ord}> 3 ; <{txt}> "Background detail c {i} {pad}" ; <{abt}> <urn:ent:fill-{i}-3> .
  <urn:r:fill-{i}#f4> <{ord}> 4 ; <{txt}> "Background detail d {i} {pad}" ; <{abt}> <urn:ent:fill-{i}-4> .
"#,
            main = main_iri.as_str(),
        ));
    }
    for i in 0..50 {
        trig.push_str(&format!(
            r#"  <urn:c:off-{i}> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:off-{i}> .
  <urn:r:off-{i}> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Off {i}" ; <{sum}> "zephyr zephyr excluded overview {i} {pad}" ; <{stat}> "reviewed" ; <{rat}> 10 ; <{rp}> <{other}> ; <{pv}> 1 ; <{fnd}> <urn:r:off-{i}#f1>, <urn:r:off-{i}#f2>, <urn:r:off-{i}#f3>, <urn:r:off-{i}#f4> .
  <urn:r:off-{i}#f1> <{ord}> 1 ; <{txt}> "Excluded detail a {i} {pad}" ; <{abt}> <urn:ent:off-{i}-1> .
  <urn:r:off-{i}#f2> <{ord}> 2 ; <{txt}> "Excluded detail b {i} {pad}" ; <{abt}> <urn:ent:off-{i}-2> .
  <urn:r:off-{i}#f3> <{ord}> 3 ; <{txt}> "Excluded detail c {i} {pad}" ; <{abt}> <urn:ent:off-{i}-3> .
  <urn:r:off-{i}#f4> <{ord}> 4 ; <{txt}> "Excluded detail d {i} {pad}" ; <{abt}> <urn:ent:off-{i}-4> .
"#,
            other = other_iri.as_str(),
        ));
    }
    trig.push_str("}\n");
    ds.store()
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    crate::settings::store_runtime(
        &st,
        &state_ds,
        &crate::settings::GRAPHRAG,
        &json!({ "enabled": true, "principal": "local", "projection": { "graphs": ["urn:g:org"] } }),
    )
    .unwrap();

    // 1 MiB query memory budget: well below the ~2.25 MiB of eligible literal payloads
    // that full-corpus decoding would materialize.
    let tight_mcp = crate::mcp::McpConfig {
        query_memory_bytes: Some(1 << 20),
        ..crate::mcp::McpConfig::default()
    };

    let sparse_policied = mcp_tool_with_cfg(
        &st,
        tight_mcp.clone(),
        "community_reports",
        json!({ "query": "zephyr", "reviewPolicy": "main", "level": 0, "limit": 1 }),
    )
    .unwrap();
    assert_eq!(sparse_policied["ranking"], "search", "{sparse_policied}");
    assert_eq!(
        sparse_policied["reports"][0]["iri"], "urn:r:main-hit",
        "{sparse_policied}"
    );
    // R4-2: Selected report keeps all 4 findings in order (including f4 with zero
    // citations) and exact citations on f1, despite >2,000 citations on unselected reports
    // exceeding `max_rows = 1_000`.
    let hit_findings = sparse_policied["reports"][0]["findings"]
        .as_array()
        .unwrap();
    assert_eq!(hit_findings.len(), 4, "{sparse_policied}");
    assert_eq!(hit_findings[0]["iri"], "urn:r:main-hit#f1");
    assert_eq!(hit_findings[0]["cites"], json!(["<urn:fact:main-1>"]));
    assert_eq!(hit_findings[0]["claims"], json!(["urn:claim:main-1"]));
    assert_eq!(hit_findings[0]["decisions"], json!(["urn:dec:main-1"]));
    assert_eq!(hit_findings[1]["iri"], "urn:r:main-hit#f2");
    assert_eq!(hit_findings[2]["iri"], "urn:r:main-hit#f3");
    assert_eq!(hit_findings[3]["iri"], "urn:r:main-hit#f4");
    assert_eq!(hit_findings[3]["cites"], json!([]));
    assert_eq!(hit_findings[3]["claims"], json!([]));
    assert_eq!(hit_findings[3]["decisions"], json!([]));

    // Also verify `with_findings` directly preserves `gr:about` on cited findings and
    // keeps uncited findings under the same `max_rows = 1_000` budget (while >2,000
    // unselected `gr:about` triples exist in the reports graph).
    let snap = ds.store().snapshot();
    let tight_opts = sparkles::sparql::QueryOptions {
        max_rows: Some(1_000),
        max_memory_bytes: Some(1 << 20),
        ..sparkles::sparql::QueryOptions::default()
    };
    let mut selected = crate::graphrag::read::reports_named(
        &snap,
        &tight_opts,
        REPORTS,
        &["urn:r:main-hit".to_string()],
        crate::graphrag::read::Filter {
            reviewed_only: false,
            review_policy_iri: Some(main_iri.as_str()),
        },
    )
    .unwrap();
    crate::graphrag::read::with_findings(&snap, &tight_opts, REPORTS, &mut selected).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].findings.len(), 4);
    assert_eq!(selected[0].findings[0].about, vec!["urn:ent:main-1"]);
    assert_eq!(selected[0].findings[0].cites, vec!["<urn:fact:main-1>"]);
    assert_eq!(selected[0].findings[0].claims, vec!["urn:claim:main-1"]);
    assert_eq!(selected[0].findings[0].decisions, vec!["urn:dec:main-1"]);
    assert_eq!(selected[0].findings[3].iri, "urn:r:main-hit#f4");
    assert!(selected[0].findings[3].about.is_empty());
    assert!(selected[0].findings[3].cites.is_empty());
    assert!(selected[0].findings[3].claims.is_empty());
    assert!(selected[0].findings[3].decisions.is_empty());

    let broad_policied = mcp_tool_with_cfg(
        &st,
        tight_mcp.clone(),
        "community_reports",
        json!({ "query": "overview", "reviewPolicy": "main", "level": 0, "limit": 5 }),
    )
    .unwrap();
    assert_eq!(broad_policied["ranking"], "search", "{broad_policied}");
    assert_eq!(broad_policied["reports"].as_array().unwrap().len(), 5);

    let broad_unpolicied = mcp_tool_with_cfg(
        &st,
        tight_mcp,
        "community_reports",
        json!({ "query": "overview", "level": 0, "limit": 5 }),
    )
    .unwrap();
    assert_eq!(broad_unpolicied["ranking"], "search", "{broad_unpolicied}");
    assert_eq!(broad_unpolicied["reports"].as_array().unwrap().len(), 5);
}

/// R4-1: Bounded ordered report enumeration handles many small reports under tight memory
/// while preserving shared findings across reports, duplicate community links to one
/// report, and reports carrying multiple admitted statuses without duplicates.
#[cfg(feature = "text")]
#[tokio::test(flavor = "multi_thread")]
async fn policy_scoped_text_search_enumerates_many_small_reports_and_deduplicates_links() {
    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    st.limits.max_rows = 10_000;
    st.limits.max_result_bytes = None;
    let st = Arc::new(st);
    let state_ds = st.attach("org", DbType::Mem, None).unwrap();
    let p = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    state_ds
        .dataset
        .store()
        .activate_memory_configuration_history(&p)
        .unwrap();
    state_ds
        .store
        .load(&[Source::from_bytes(
            ORG.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let ds = state_ds.dataset.clone();
    evidence::configure(&ds, &settings(), &Control::none()).unwrap();
    ds.indexes()
        .text()
        .enable(sparkles::text::TextConfig::default())
        .unwrap();

    let gr = crate::graphrag::guard::gr;
    let main_iri = sparkles::evidence::review::policy_iri("main");
    let (c_cls, lv, cnt, rep, r_cls, sum, stat, rat, rp, pv, fnd, ord, txt) = (
        gr("Community"),
        gr("level"),
        gr("memberCount"),
        gr("report"),
        gr("Report"),
        gr("summary"),
        gr("status"),
        gr("rating"),
        gr("reviewPolicy"),
        gr("policyVersion"),
        gr("finding"),
        gr("order"),
        gr("text"),
    );

    let mut trig = format!("<{REPORTS}> {{\n");
    // 1) Duplicate community links pointing to the same report at level 0: 105 distinct
    //    communities (exceeding `DEFAULT_MAX_REPORTS = 100` and spanning multiple anchor
    //    batches), paired with an equal-length single-level-0-link peer (`urn:r:dup-peer`)
    //    that also carries a level-1 community link (`urn:c:zzzz-dup-peer-l1`).
    //    - Double-counting `marigold` in BM25 corpus stats lowers its IDF and flips the
    //      tie/ranking.
    //    - Applying a raw-row `LIMIT 100` in `reports_named` or `reports_at` before
    //      per-report deduplication lets `urn:r:dup-comm` (rating 9) fill all 100 rows
    //      and evict `urn:r:dup-peer` (rating 8).
    //    - Mapping `reports_named` without filtering by `level` lets the level-1 row
    //      overwrite the level-0 row for `urn:r:dup-peer`.
    trig.push_str(&format!(
        r#"  <urn:c:0000-dup-a> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:dup-comm> .
  <urn:c:zzzz-dup-b> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:dup-comm> .
  <urn:r:dup-comm> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Dup" ; <{sum}> "Unique marigold summary." ; <{stat}> "reviewed" ; <{rat}> 9 ; <{rp}> <{main}> ; <{pv}> 1 .
  <urn:c:0001-dup-peer> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:dup-peer> .
  <urn:c:zzzz-dup-peer-l1> a <{c_cls}> ; <{lv}> 1 ; <{cnt}> 2 ; <{rep}> <urn:r:dup-peer> .
  <urn:r:dup-peer> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Peer" ; <{sum}> "Unique verbena summary." ; <{stat}> "reviewed" ; <{rat}> 8 ; <{rp}> <{main}> ; <{pv}> 1 .
"#,
        main = main_iri.as_str(),
    ));
    for i in 0..103 {
        trig.push_str(&format!(
            "  <urn:c:0005-dup-extra-{i:03}> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:dup-comm> .\n"
        ));
    }
    // 2) Report carrying two admitted statuses ("reviewed" and "stale"), paired with an
    //    equal-length single-status peer (`urn:r:multi-peer`) so double-counting `saffron`
    //    across status scans would lower its IDF and break the exact tie.
    trig.push_str(&format!(
        r#"  <urn:c:multi-stat> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:multi-stat> .
  <urn:r:multi-stat> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Multi" ; <{sum}> "Unique saffron summary." ; <{stat}> "reviewed", "stale" ; <{rat}> 2 ; <{rp}> <{main}> ; <{pv}> 1 .
  <urn:c:multi-peer> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:multi-peer> .
  <urn:r:multi-peer> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Peer" ; <{sum}> "Unique ochre summary." ; <{stat}> "reviewed" ; <{rat}> 2 ; <{rp}> <{main}> ; <{pv}> 1 .
"#,
        main = main_iri.as_str(),
    ));
    // 3) Shared finding referenced by two distinct reports.
    trig.push_str(&format!(
        r#"  <urn:c:shared-a> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:shared-a> .
  <urn:c:shared-b> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:shared-b> .
  <urn:r:shared-a> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Shared A" ; <{sum}> "General overview." ; <{stat}> "reviewed" ; <{rat}> 1 ; <{rp}> <{main}> ; <{pv}> 1 ; <{fnd}> <urn:f:shared-cobalt> .
  <urn:r:shared-b> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Shared B" ; <{sum}> "General overview." ; <{stat}> "reviewed" ; <{rat}> 1 ; <{rp}> <{main}> ; <{pv}> 1 ; <{fnd}> <urn:f:shared-cobalt> .
  <urn:f:shared-cobalt> <{ord}> 1 ; <{txt}> "Shared cobalt finding across communities." .
"#,
        main = main_iri.as_str(),
    ));
    // 4) 1,500 small filler reports.
    for i in 0..1_500 {
        trig.push_str(&format!(
            r#"  <urn:c:small-{i:04}> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:small-{i:04}> .
  <urn:r:small-{i:04}> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "S{i}" ; <{sum}> "Small filler {i}" ; <{stat}> "reviewed" ; <{rat}> 5 ; <{rp}> <{main}> ; <{pv}> 1 .
"#,
            main = main_iri.as_str(),
        ));
    }
    trig.push_str("}\n");
    ds.store()
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    crate::settings::store_runtime(
        &st,
        &state_ds,
        &crate::settings::GRAPHRAG,
        &json!({ "enabled": true, "principal": "local", "projection": { "graphs": ["urn:g:org"] } }),
    )
    .unwrap();

    let tight_mcp = crate::mcp::McpConfig {
        query_memory_bytes: Some(512 << 10),
        ..crate::mcp::McpConfig::default()
    };

    // Shared finding must score both reports deterministically (policied and unpolicied).
    for extra in [json!({ "reviewPolicy": "main" }), json!({})] {
        let mut args = json!({ "query": "cobalt", "level": 0, "limit": 2 });
        if let Some(p) = extra.get("reviewPolicy") {
            args["reviewPolicy"] = p.clone();
        }
        let res = mcp_tool_with_cfg(&st, tight_mcp.clone(), "community_reports", args).unwrap();
        assert_eq!(res["ranking"], "search", "{res}");
        let iris: Vec<&str> = res["reports"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["iri"].as_str())
            .collect();
        assert_eq!(iris, vec!["urn:r:shared-a", "urn:r:shared-b"], "{res}");
        assert_eq!(res["reports"][0]["score"], res["reports"][1]["score"]);
    }

    // R5-2 & R6-2: 105 duplicate level-0 community links on `urn:r:dup-comm` (rating 9)
    // must neither double-count `marigold` in Pass 1 corpus stats nor evict
    // `urn:r:dup-peer` (rating 8) during named retrieval, and `urn:r:dup-peer`'s level-1
    // link must not overwrite its level-0 community row.
    let dup = mcp_tool_with_cfg(
        &st,
        tight_mcp.clone(),
        "community_reports",
        json!({ "query": "marigold verbena", "reviewPolicy": "main", "level": 0, "limit": 3 }),
    )
    .unwrap();
    assert_eq!(dup["ranking"], "search", "{dup}");
    assert_eq!(dup["reports"][0]["iri"], "urn:r:dup-comm", "{dup}");
    assert_eq!(dup["reports"][0]["community"], "urn:c:0000-dup-a", "{dup}");
    assert_eq!(dup["reports"][1]["iri"], "urn:r:dup-peer", "{dup}");
    assert_eq!(
        dup["reports"][1]["community"], "urn:c:0001-dup-peer",
        "{dup}"
    );
    assert_eq!(dup["reports"][1]["level"], 0, "{dup}");
    assert_eq!(
        dup["reports"][0]["score"], dup["reports"][1]["score"],
        "duplicate community link altered BM25 corpus statistics: {dup}"
    );
    assert!(dup["reports"][2]["score"].is_null(), "{dup}");

    // R6-2: Rating-ranked retrieval (`reports_at`) must also deduplicate per report IRI
    // before `LIMIT` so 105 community links on `urn:r:dup-comm` do not evict
    // `urn:r:dup-peer`.
    let by_rating = mcp_tool_with_cfg(
        &st,
        tight_mcp.clone(),
        "community_reports",
        json!({ "reviewPolicy": "main", "level": 0, "limit": 2 }),
    )
    .unwrap();
    assert_eq!(by_rating["ranking"], "rating", "{by_rating}");
    assert_eq!(
        by_rating["reports"][0]["iri"], "urn:r:dup-comm",
        "{by_rating}"
    );
    assert_eq!(
        by_rating["reports"][0]["community"], "urn:c:0000-dup-a",
        "{by_rating}"
    );
    assert_eq!(
        by_rating["reports"][1]["iri"], "urn:r:dup-peer",
        "{by_rating}"
    );
    assert_eq!(
        by_rating["reports"][1]["community"], "urn:c:0001-dup-peer",
        "{by_rating}"
    );

    // R5-2: Multi-status report must not double-count `saffron` across status scans;
    // otherwise `df(saffron) = 2 > df(ochre) = 1`, letting `urn:r:multi-peer` strictly
    // outscore `urn:r:multi-stat`.
    let multi = mcp_tool_with_cfg(
        &st,
        tight_mcp,
        "community_reports",
        json!({ "query": "saffron ochre", "level": 0, "limit": 3 }),
    )
    .unwrap();
    assert_eq!(multi["ranking"], "search", "{multi}");
    assert_eq!(multi["reports"][0]["iri"], "urn:r:multi-peer", "{multi}");
    assert_eq!(multi["reports"][1]["iri"], "urn:r:multi-stat", "{multi}");
    assert_eq!(
        multi["reports"][0]["score"], multi["reports"][1]["score"],
        "multi-status report altered BM25 corpus statistics: {multi}"
    );
    assert!(multi["reports"][2]["score"].is_null(), "{multi}");
}

/// R5-1: Retained non-current revisions (`gr:editOf` edit history), rejected/failed
/// reports, and other-level reports under the same `reviewPolicy` must not exhaust
/// `max_rows` when the eligible level-0 set is small.
#[cfg(feature = "text")]
#[tokio::test(flavor = "multi_thread")]
async fn policy_scoped_text_search_ignores_retained_revisions_under_tight_row_budget() {
    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    st.limits.max_rows = 1_000;
    st.limits.max_result_bytes = None;
    let st = Arc::new(st);
    let state_ds = st.attach("org", DbType::Mem, None).unwrap();
    let p = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    state_ds
        .dataset
        .store()
        .activate_memory_configuration_history(&p)
        .unwrap();
    state_ds
        .store
        .load(&[Source::from_bytes(
            ORG.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let ds = state_ds.dataset.clone();
    evidence::configure(&ds, &settings(), &Control::none()).unwrap();
    ds.indexes()
        .text()
        .enable(sparkles::text::TextConfig::default())
        .unwrap();

    let gr = crate::graphrag::guard::gr;
    let main_iri = sparkles::evidence::review::policy_iri("main");
    let (c_cls, lv, cnt, mem, rep, r_cls, sum, stat, rat, rp, pv, edit_of, cites_rep) = (
        gr("Community"),
        gr("level"),
        gr("memberCount"),
        gr("member"),
        gr("report"),
        gr("Report"),
        gr("summary"),
        gr("status"),
        gr("rating"),
        gr("reviewPolicy"),
        gr("policyVersion"),
        gr("editOf"),
        gr("citesReport"),
    );

    // 1 linked reviewed level-0 report under `main` (whose community carries 1,050
    // `gr:member` triples and whose report carries 1,050 `gr:citesReport` triples, both
    // exceeding `max_rows = 1_000`), plus 1,050 retained non-current revisions under
    // `main`, plus 200 rejected/failed same-policy reports and 10 level-1 same-policy
    // reports.
    let mut trig = format!("<{REPORTS}> {{\n");
    trig.push_str(&format!(
        r#"  <urn:c:live-0> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 1050 ; <{rep}> <urn:r:main-live> .
  <urn:r:main-live> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Live Main" ; <{sum}> "Current zephyr summary." ; <{stat}> "reviewed" ; <{rat}> 5 ; <{rp}> <{main}> ; <{pv}> 1 ; <{edit_of}> <urn:r:rev-0000> .
"#,
        main = main_iri.as_str(),
    ));
    for i in 0..1_050 {
        trig.push_str(&format!(
            "  <urn:c:live-0> <{mem}> <urn:ent:member-{i:04}> .\n  <urn:r:main-live> <{cites_rep}> <urn:r:cited-{i:04}> .\n"
        ));
    }
    for i in 0..1_050 {
        let next_edit = if i + 1 < 1_050 {
            format!(" ; <{edit_of}> <urn:r:rev-{:04}>", i + 1)
        } else {
            String::new()
        };
        trig.push_str(&format!(
            r#"  <urn:r:rev-{i:04}> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Rev {i}" ; <{sum}> "Superseded zephyr zephyr revision {i}." ; <{stat}> "reviewed" ; <{rat}> 10 ; <{rp}> <{main}> ; <{pv}> 1{next_edit} .
"#,
            main = main_iri.as_str(),
        ));
    }
    for i in 0..100 {
        trig.push_str(&format!(
            r#"  <urn:r:rej-{i:03}> a <{r_cls}> ; <{sum}> "Rejected zephyr {i}" ; <{stat}> "rejected" ; <{rp}> <{main}> ; <{pv}> 1 .
  <urn:r:fail-{i:03}> a <{r_cls}> ; <{sum}> "Failed zephyr {i}" ; <{stat}> "failed" ; <{rp}> <{main}> ; <{pv}> 1 .
"#,
            main = main_iri.as_str(),
        ));
    }
    for i in 0..10 {
        trig.push_str(&format!(
            r#"  <urn:c:lvl1-{i:03}> a <{c_cls}> ; <{lv}> 1 ; <{cnt}> 2 ; <{rep}> <urn:r:lvl1-{i:03}> .
  <urn:r:lvl1-{i:03}> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "L1 {i}" ; <{sum}> "Other level zephyr {i}" ; <{stat}> "reviewed" ; <{rat}> 8 ; <{rp}> <{main}> ; <{pv}> 1 .
"#,
            main = main_iri.as_str(),
        ));
    }
    trig.push_str("}\n");
    ds.store()
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    crate::settings::store_runtime(
        &st,
        &state_ds,
        &crate::settings::GRAPHRAG,
        &json!({ "enabled": true, "principal": "local", "projection": { "graphs": ["urn:g:org"] } }),
    )
    .unwrap();

    let tight_mcp = crate::mcp::McpConfig {
        query_memory_bytes: Some(1 << 20),
        ..crate::mcp::McpConfig::default()
    };

    for args in [
        json!({ "query": "zephyr", "reviewPolicy": "main", "level": 0, "limit": 1 }),
        json!({ "query": "zephyr", "level": 0, "limit": 1 }),
    ] {
        let res = mcp_tool_with_cfg(&st, tight_mcp.clone(), "community_reports", args).unwrap();
        assert_eq!(res["ranking"], "search", "{res}");
        let reps = res["reports"].as_array().unwrap();
        assert_eq!(reps.len(), 1, "{res}");
        assert_eq!(reps[0]["iri"], "urn:r:main-live", "{res}");
    }
}

/// R3-2: Scoped text search preserves the full-text index's default accent-insensitive
/// analyzer (`cafe` matches `café`, `naïve façade` matches `naive facade`) without a
/// vector index, and respects the index's configured `maxTextBytes` literal prefix.
#[cfg(feature = "text")]
#[tokio::test(flavor = "multi_thread")]
async fn text_search_preserves_accent_folding_and_max_text_bytes_truncation() {
    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
    const OTHER_DEC: &str = "urn:ev:other-decisions";
    const OTHER_SUP: &str = "urn:ev:other-supports";
    const OTHER_OUT: &str = "urn:ev:other-accepted";

    let m = mock(Arc::new(OnceLock::new()));
    let (st, _app) = app(&m.url()).await;
    let ds = dataset(&st);

    let mut cfg = settings();
    cfg.decisions.push(OTHER_DEC.into());
    cfg.supports.push(OTHER_SUP.into());
    cfg.projections.push(OTHER_OUT.into());
    cfg.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: vec![],
            conflicts: ConflictMode::AdmitLabelled,
            boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
            decisions: OTHER_DEC.into(),
            supports: OTHER_SUP.into(),
            output: OTHER_OUT.into(),
        },
    );
    evidence::configure(&ds, &cfg, &Control::none()).unwrap();
    ds.indexes()
        .text()
        .enable(sparkles::text::TextConfig::default())
        .unwrap();

    let gr = crate::graphrag::guard::gr;
    let main_iri = sparkles::evidence::review::policy_iri("main");
    let other_iri = sparkles::evidence::review::policy_iri("other");
    let (c_cls, lv, cnt, rep, r_cls, sum, stat, rat, rp, pv) = (
        gr("Community"),
        gr("level"),
        gr("memberCount"),
        gr("report"),
        gr("Report"),
        gr("summary"),
        gr("status"),
        gr("rating"),
        gr("reviewPolicy"),
        gr("policyVersion"),
    );

    let long_prefix = "x".repeat(80);
    let trig = format!(
        r#"<{REPORTS}> {{
  <urn:c:main-cafe> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:main-cafe> .
  <urn:r:main-cafe> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Hospitality" ; <{sum}> "Regional café expansion notes." ; <{stat}> "reviewed" ; <{rat}> 1 ; <{rp}> <{main}> ; <{pv}> 1 .

  <urn:c:main-naive> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:main-naive> .
  <urn:r:main-naive> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Architecture" ; <{sum}> "Naive facade review." ; <{stat}> "reviewed" ; <{rat}> 1 ; <{rp}> <{main}> ; <{pv}> 1 .

  <urn:c:main-high> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:main-high> .
  <urn:r:main-high> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Infrastructure" ; <{sum}> "Unrelated high-rated report." ; <{stat}> "reviewed" ; <{rat}> 10 ; <{rp}> <{main}> ; <{pv}> 1 .

  <urn:c:other-cafe> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:other-cafe> .
  <urn:r:other-cafe> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Excluded" ; <{sum}> "Excluded café café summary." ; <{stat}> "reviewed" ; <{rat}> 10 ; <{rp}> <{other}> ; <{pv}> 1 .
}}"#,
        main = main_iri.as_str(),
        other = other_iri.as_str(),
    );
    ds.store()
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    let state_ds = st.get("org").unwrap();
    crate::settings::store_runtime(
        &st,
        &state_ds,
        &crate::settings::GRAPHRAG,
        &json!({ "enabled": true, "principal": "local", "projection": { "graphs": ["urn:g:org"] } }),
    )
    .unwrap();

    // 1) ASCII query matches accented summary (policied and unpolicied), without a vector index.
    let accent_policied = mcp_tool(
        &st,
        "community_reports",
        json!({ "query": "cafe", "reviewPolicy": "main", "limit": 1 }),
    )
    .unwrap();
    assert_eq!(accent_policied["ranking"], "search", "{accent_policied}");
    assert_eq!(
        accent_policied["reports"][0]["iri"], "urn:r:main-cafe",
        "{accent_policied}"
    );

    let accent_unpolicied = mcp_tool(
        &st,
        "community_reports",
        json!({ "query": "cafe", "limit": 2 }),
    )
    .unwrap();
    assert_eq!(
        accent_unpolicied["ranking"], "search",
        "{accent_unpolicied}"
    );
    let unpolicied_iris: Vec<&str> = accent_unpolicied["reports"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["iri"].as_str())
        .collect();
    assert!(
        unpolicied_iris.contains(&"urn:r:main-cafe")
            && unpolicied_iris.contains(&"urn:r:other-cafe"),
        "expected both café reports to match before high-rated unrelated report: {accent_unpolicied}"
    );

    // Accented query matches ASCII summary as well.
    let reverse = mcp_tool(
        &st,
        "community_reports",
        json!({ "query": "naïve façade", "reviewPolicy": "main", "limit": 1 }),
    )
    .unwrap();
    assert_eq!(reverse["ranking"], "search", "{reverse}");
    assert_eq!(
        reverse["reports"][0]["iri"], "urn:r:main-naive",
        "{reverse}"
    );

    // 2) Configured `max_text_bytes` truncation: terms past the indexed byte prefix do
    // not rank reports.
    ds.indexes()
        .text()
        .enable(sparkles::text::TextConfig {
            max_text_bytes: 64,
            ..sparkles::text::TextConfig::default()
        })
        .unwrap();

    let trunc_trig = format!(
        r#"<{REPORTS}> {{
  <urn:c:in-prefix> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:in-prefix> .
  <urn:r:in-prefix> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Short" ; <{sum}> "Brief zephyr note inside indexed prefix." ; <{stat}> "reviewed" ; <{rat}> 1 ; <{rp}> <{main}> ; <{pv}> 1 .

  <urn:c:past-prefix> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:past-prefix> .
  <urn:r:past-prefix> a <{r_cls}> ; <http://www.w3.org/2000/01/rdf-schema#label> "Long" ; <{sum}> "{long_prefix} zephyr tailonly." ; <{stat}> "reviewed" ; <{rat}> 10 ; <{rp}> <{main}> ; <{pv}> 1 .
}}"#,
        main = main_iri.as_str(),
    );
    ds.store()
        .load(&[Source::from_bytes(
            trunc_trig.into_bytes(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();

    let prefix_hit = mcp_tool(
        &st,
        "community_reports",
        json!({ "query": "zephyr", "reviewPolicy": "main", "limit": 2 }),
    )
    .unwrap();
    assert_eq!(prefix_hit["ranking"], "search", "{prefix_hit}");
    assert_eq!(
        prefix_hit["reports"][0]["iri"], "urn:r:in-prefix",
        "{prefix_hit}"
    );
    assert!(
        prefix_hit["reports"][0]["score"].is_number(),
        "{prefix_hit}"
    );
    assert!(
        prefix_hit["reports"][1]["score"].is_null(),
        "report whose term sits past max_text_bytes should not be search-scored: {prefix_hit}"
    );

    let tail_only = mcp_tool(
        &st,
        "community_reports",
        json!({ "query": "tailonly", "reviewPolicy": "main", "limit": 2 }),
    )
    .unwrap();
    assert_eq!(
        tail_only["ranking"], "rating",
        "term past max_text_bytes must not trigger search ranking: {tail_only}"
    );
}

/// R7-1: Repeating a configured full-text predicate IRI in `PredicateSet::Only` must not
/// emit duplicate literal-lookup branches or alter BM25 corpus stats, scores, or ranking.
#[cfg(feature = "text")]
#[tokio::test(flavor = "multi_thread")]
async fn duplicate_configured_text_predicates_do_not_change_scores_or_ranking() {
    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";

    let m = mock(Arc::new(OnceLock::new()));
    let (st, _app) = app(&m.url()).await;
    let ds = dataset(&st);
    evidence::configure(&ds, &settings(), &Control::none()).unwrap();

    let gr = crate::graphrag::guard::gr;
    let main_iri = sparkles::evidence::review::policy_iri("main");
    let (c_cls, lv, cnt, rep, r_cls, sum, stat, rat, rp, pv) = (
        gr("Community"),
        gr("level"),
        gr("memberCount"),
        gr("report"),
        gr("Report"),
        gr("summary"),
        gr("status"),
        gr("rating"),
        gr("reviewPolicy"),
        gr("policyVersion"),
    );
    let label = crate::graphrag::store::RDFS_LABEL;

    // Equal-length title/summary pairs: `<urn:r:a-summary>` matches `marigold` in its
    // summary and sorts first by IRI; `<urn:r:b-title>` matches `verbena` in its title.
    let trig = format!(
        r#"<{REPORTS}> {{
  <urn:c:a-summary> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:a-summary> .
  <urn:r:a-summary> a <{r_cls}> ; <{label}> "alpha" ; <{sum}> "marigold" ; <{stat}> "reviewed" ; <{rat}> 5 ; <{rp}> <{main}> ; <{pv}> 1 .

  <urn:c:b-title> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:b-title> .
  <urn:r:b-title> a <{r_cls}> ; <{label}> "verbena" ; <{sum}> "omega" ; <{stat}> "reviewed" ; <{rat}> 5 ; <{rp}> <{main}> ; <{pv}> 1 .
}}
"#,
        main = main_iri.as_str(),
    );
    ds.store()
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    let state_ds = st.get("org").unwrap();
    crate::settings::store_runtime(
        &st,
        &state_ds,
        &crate::settings::GRAPHRAG,
        &json!({ "enabled": true, "principal": "local", "projection": { "graphs": ["urn:g:org"] } }),
    )
    .unwrap();

    let run_pair = |predicates: Vec<String>| -> Vec<(Vec<(String, f64)>, String)> {
        ds.indexes()
            .text()
            .enable(sparkles::text::TextConfig {
                predicates: sparkles::text::PredicateSet::Only(predicates),
                ..sparkles::text::TextConfig::default()
            })
            .unwrap();
        let mut results = Vec::new();
        for with_policy in [true, false] {
            let mut args2 = json!({ "query": "marigold verbena", "level": 0, "limit": 2 });
            let mut args1 = json!({ "query": "marigold verbena", "level": 0, "limit": 1 });
            if with_policy {
                args2["reviewPolicy"] = json!("main");
                args1["reviewPolicy"] = json!("main");
            }
            let r2 = mcp_tool(&st, "community_reports", args2).unwrap();
            assert_eq!(r2["ranking"], "search", "{r2}");
            let scored: Vec<(String, f64)> = r2["reports"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| {
                    (
                        item["iri"].as_str().unwrap().to_string(),
                        item["score"].as_f64().unwrap(),
                    )
                })
                .collect();
            let r1 = mcp_tool(&st, "community_reports", args1).unwrap();
            assert_eq!(r1["ranking"], "search", "{r1}");
            let top1 = r1["reports"][0]["iri"].as_str().unwrap().to_string();
            results.push((scored, top1));
        }
        results
    };

    let unique = run_pair(vec![sum.clone(), label.to_string()]);
    let repeated = run_pair(vec![sum.clone(), label.to_string(), sum.clone()]);

    for (u, r) in unique.iter().zip(repeated.iter()) {
        assert_eq!(u.0.len(), 2);
        assert_eq!(u.0[0].0, "urn:r:a-summary");
        assert_eq!(u.0[1].0, "urn:r:b-title");
        assert_eq!(u.0[0].1, u.0[1].1, "expected exact score tie: {u:?}");
        assert_eq!(u.1, "urn:r:a-summary");
        assert_eq!(
            r, u,
            "repeated configured text predicate changed scores or ranking: unique={u:?}, repeated={r:?}"
        );
    }
}

/// R7-2: A blank-node community link at the same level must not shadow a valid named
/// community during named hydration and drop a scored search hit.
#[cfg(feature = "text")]
#[tokio::test(flavor = "multi_thread")]
async fn blank_node_community_link_does_not_hide_named_search_hit() {
    const REPORTS: &str = "urn:x-sparkles:graphrag:reports";

    let m = mock(Arc::new(OnceLock::new()));
    let (st, _app) = app(&m.url()).await;
    let ds = dataset(&st);
    evidence::configure(&ds, &settings(), &Control::none()).unwrap();
    ds.indexes()
        .text()
        .enable(sparkles::text::TextConfig::default())
        .unwrap();

    let gr = crate::graphrag::guard::gr;
    let main_iri = sparkles::evidence::review::policy_iri("main");
    let (c_cls, lv, cnt, rep, r_cls, sum, stat, rat, rp, pv) = (
        gr("Community"),
        gr("level"),
        gr("memberCount"),
        gr("report"),
        gr("Report"),
        gr("summary"),
        gr("status"),
        gr("rating"),
        gr("reviewPolicy"),
        gr("policyVersion"),
    );
    let label = crate::graphrag::store::RDFS_LABEL;

    let trig = format!(
        r#"<{REPORTS}> {{
  _:bnode_shadow a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:bnode-shadow> .
  <urn:c:named-shadow> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 4 ; <{rep}> <urn:r:bnode-shadow> .
  <urn:r:bnode-shadow> a <{r_cls}> ; <{label}> "Larkspur" ; <{sum}> "Unique larkspur summary." ; <{stat}> "reviewed" ; <{rat}> 1 ; <{rp}> <{main}> ; <{pv}> 1 .

  <urn:c:high-peer> a <{c_cls}> ; <{lv}> 0 ; <{cnt}> 2 ; <{rep}> <urn:r:high-peer> .
  <urn:r:high-peer> a <{r_cls}> ; <{label}> "Peer" ; <{sum}> "Unrelated high-rated report." ; <{stat}> "reviewed" ; <{rat}> 9 ; <{rp}> <{main}> ; <{pv}> 1 .
}}
"#,
        main = main_iri.as_str(),
    );
    ds.store()
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    let state_ds = st.get("org").unwrap();
    crate::settings::store_runtime(
        &st,
        &state_ds,
        &crate::settings::GRAPHRAG,
        &json!({ "enabled": true, "principal": "local", "projection": { "graphs": ["urn:g:org"] } }),
    )
    .unwrap();

    for args in [
        json!({ "query": "larkspur", "reviewPolicy": "main", "level": 0, "limit": 1 }),
        json!({ "query": "larkspur", "level": 0, "limit": 1 }),
    ] {
        let res = mcp_tool(&st, "community_reports", args).unwrap();
        assert_eq!(res["ranking"], "search", "{res}");
        assert_eq!(res["reports"][0]["iri"], "urn:r:bnode-shadow", "{res}");
        assert_eq!(
            res["reports"][0]["community"], "urn:c:named-shadow",
            "{res}"
        );
        assert_eq!(res["reports"][0]["memberCount"], 4, "{res}");
        assert!(res["reports"][0]["score"].is_number(), "{res}");
    }
}
