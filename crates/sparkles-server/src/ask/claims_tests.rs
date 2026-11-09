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
    assert!(
        reqs.iter()
            .filter(|r| !is_summary(r))
            .all(|r| !r.body.to_string().contains("Claim occurrences"))
    );
    let plain = ask(&app, json!({ "question": question })).await;
    assert_eq!(plain["outcome"], "answered", "{plain}");
    assert!(plain.get("claims").is_none() && plain["summary"].get("claimCitations").is_none());
}

/// An unknown policy and the global mode are refused before any model call.
#[tokio::test(flavor = "multi_thread")]
async fn refused_policies_cost_no_model_call() {
    let m = mock(Arc::new(OnceLock::new()));
    let (_, app) = app(&m.url()).await;
    let out = ask_with(
        &app,
        json!({ "question": "Which teams does Acme have?", "reviewPolicy": "nothing" }),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(out["outcome"], "error", "{out}");
    assert_eq!(out["error"]["code"], "bad-argument");
    let out = ask_with(
        &app,
        json!({ "question": "Which teams does Acme have?", "reviewPolicy": "main", "mode": "global" }),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(out["error"]["code"], "bad-argument", "{out}");
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
