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
    self, ClaimInput, ClaimKind, ConflictMode, DecisionInput, Disposition, EvidenceInput, Polarity,
    Policy, ProposeOptions, RecordKey, RecordRef, RecordVersion, ReviewOptions, Settings,
    Sha256Digest, SourceContent, SourceRevisionInput, WithdrawalInput,
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
        projections: vec!["urn:ev:accepted".into()],
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

/// An unknown or wrong-kind policy is refused before any model call, in both local and
/// global modes.
#[tokio::test(flavor = "multi_thread")]
async fn refused_policies_cost_no_model_call() {
    let m = mock(Arc::new(OnceLock::new()));
    let (_, app) = app(&m.url()).await;
    for body in [
        json!({ "question": "Which teams does Acme have?", "reviewPolicy": "nothing" }),
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
  <urn:c:main> a <{community}> ; <{level}> 0 ; <{count}> 2 ; <{report}> <urn:r:main> .
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

  <urn:c:other> a <{community}> ; <{level}> 0 ; <{count}> 2 ; <{report}> <urn:r:other> .
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

  <urn:c:plain> a <{community}> ; <{level}> 0 ; <{count}> 2 ; <{report}> <urn:r:plain> .
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
