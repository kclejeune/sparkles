//! Item 652: community reports read under a review policy's accepted projection, with
//! stub providers and the real router (spec C25, "Reports on accepted evidence").

use super::*;
use crate::graphrag::build::{self, Mode, Outcome, Run, RunError};
use crate::graphrag::evidence::{Evidence, Limits};
use crate::graphrag::guard::gr;
use crate::graphrag::settings::GraphragSettings;
use crate::models::mock::{self, MockModel, Received};
use oxrdf::{Literal, NamedNode, Triple};
use sparkles::evidence::{
    self, ClaimInput, ClaimKind, ConflictMode, DecisionInput, Disposition, EvidenceInput,
    MatchingPolicy, Polarity, Policy, ProposeOptions, RecordKey, RecordRef, RecordVersion,
    ReviewOptions, Settings, Sha256Digest, SourceContent, SourceRevisionInput, WithdrawalInput,
};
use sparkles::task::Control;

const ORG: &str = "urn:g:org";
const SOURCES: &str = "urn:ev:sources";
const CLAIMS: &str = "urn:ev:claims";
const HIDDEN: &str = "urn:ev:hidden-claims";
const DECISIONS: &str = "urn:ev:decisions";
const SUPPORTS: &str = "urn:ev:supports";
const ACCEPTED: &str = "urn:ev:accepted";
const IDENTITIES: &str = "urn:ev:identities";
const MAPS: &str = "urn:ev:maps";
const MAP: &str = "urn:ev:map";
const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
const LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const PROV: &str = "http://www.w3.org/ns/prov#";

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn ex(local: &str) -> NamedNode {
    iri(&format!("http://ex/{local}"))
}

fn evidence_settings(version: u32) -> Settings {
    Settings {
        enabled: true,
        source_records: vec![SOURCES.into()],
        proposals: vec![CLAIMS.into()],
        decisions: vec![DECISIONS.into()],
        supports: vec![SUPPORTS.into()],
        identity: vec![IDENTITIES.into(), MAPS.into()],
        projections: vec![ACCEPTED.into(), MAP.into()],
        policies: [(
            "main".into(),
            Policy {
                version,
                admitted_kinds: vec![ClaimKind::Reported],
                required_scope: vec![],
                conflicts: ConflictMode::AdmitLabelled,
                boundary: vec![SOURCES.into(), CLAIMS.into()],
                decisions: DECISIONS.into(),
                supports: SUPPORTS.into(),
                output: ACCEPTED.into(),
            },
        )]
        .into_iter()
        .collect(),
        matching_policies: [(
            "people".into(),
            MatchingPolicy {
                version: 1,
                boundary: vec![IDENTITIES.into()],
                decisions: DECISIONS.into(),
                maps: MAPS.into(),
                output: MAP.into(),
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    }
}

/// `rad` administers the dataset and `rp`, the principal the reports read as, reads it
/// all. `rnodec` cannot read the policy's decisions, `rnoclaims` reads everything
/// a report under the policy reads except the proposal graph, and `rhide` reads
/// everything but the proposal graph [`HIDDEN`].
fn users() -> String {
    let h = |u: &str| hash_password_with(&format!("{u}-pw"), 8, 1, 1).unwrap();
    format!(
        r#"
[[users]]
name = "rad"
password = "{}"
datasets = {{ ev = "admin" }}
[[users]]
name = "rp"
password = "{}"
datasets = {{ ev = "read" }}
[[users]]
name = "rnodec"
password = "{}"
[[users.grants]]
dataset = "ev"
level = "read"
graphs = ["{ORG}", "{CLAIMS}", "{SUPPORTS}", "{ACCEPTED}", "{REPORTS}"]
[[users]]
name = "rnoclaims"
password = "{}"
[[users.grants]]
dataset = "ev"
level = "read"
graphs = ["{ORG}", "{DECISIONS}", "{SUPPORTS}", "{ACCEPTED}", "{REPORTS}"]
[[users]]
name = "rhide"
password = "{}"
[[users.grants]]
dataset = "ev"
level = "read"
graphs = ["{ORG}", "{SOURCES}", "{CLAIMS}", "{DECISIONS}", "{SUPPORTS}", "{ACCEPTED}", "{REPORTS}"]
"#,
        h("rad"),
        h("rp"),
        h("rnodec"),
        h("rnoclaims"),
        h("rhide"),
    )
}

/// The report a stub model writes: one finding about the subject of the first item that
/// cites every item except a fact whose object is the label "Lead x2", so that the
/// claim behind that label is read and not cited.
fn answer_all(r: &Received) -> String {
    let prompt = mock::prompt_of(r);
    let cites: Vec<u64> = prompt
        .lines()
        .filter(|l| !l.contains("Lead x2"))
        .filter_map(|l| l.strip_prefix('[')?.split_once("] ")?.0.parse().ok())
        .take(32)
        .collect();
    let first = prompt
        .lines()
        .find_map(|l| l.strip_prefix("[1] "))
        .unwrap_or("");
    let subject = first.split(' ').next().unwrap_or("");
    let entity = prompt
        .lines()
        .filter_map(|l| l.strip_prefix('E'))
        .find_map(|l| {
            let (n, rest) = l.split_once(' ')?;
            (rest.split(' ').next() == Some(subject)).then(|| n.parse::<u64>().ok())?
        })
        .unwrap_or(1);
    serde_json::json!({
        "title": "A community",
        "summary": "People who work together.",
        "findings": [{ "text": "They work together.", "entities": [entity], "cites": cites }],
        "rating": 5
    })
    .to_string()
}

struct Env {
    s: AuthServer,
    mock: MockModel,
}

/// A server with dataset `ev`: two clusters of people in `urn:g:org`, configured for
/// evidence when `activate`, with a stub provider for the community role.
fn env(activate: bool) -> Env {
    let s = build(Fixture {
        extra: users(),
        ..Default::default()
    });
    let ds = s.state.attach("ev", DbType::Memory, None).unwrap();
    if activate {
        let p = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
        ds.dataset
            .store()
            .activate_memory_configuration_history(&p)
            .unwrap();
        evidence::configure(&ds.dataset, &evidence_settings(1), &Control::none()).unwrap();
    }
    let mut org = crate::graphrag::build::tests::data(4);
    org.push_str(&format!(
        "<{ORG}> {{ <http://ex/y0> <http://ex/owns> <http://ex/y1> . }}\n"
    ));
    ds.store
        .load(&[Source::from_bytes(
            org.into_bytes(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let mock = MockModel::start(|r, _| (200, mock::openai(&answer_all(r))));
    s.state
        .models
        .store(Some(Arc::new(crate::graphrag::build::tests::models_for(
            &mock.url(),
            &serde_json::json!({ "provider": "p", "model": "m" }),
        ))));
    crate::settings::store_runtime(
        &s.state,
        &ds,
        &crate::settings::ASSISTANT,
        &serde_json::json!({ "send": "rows" }),
    )
    .unwrap();
    Env { s, mock }
}

impl Env {
    fn ds(&self) -> Arc<crate::state::Dataset> {
        self.s.state.datasets().get("ev").cloned().unwrap()
    }

    fn dataset(&self) -> sparkles::Dataset {
        self.ds().dataset.clone()
    }

    fn run_with(
        &self,
        settings: GraphragSettings,
        mode: Mode,
        limits: Limits,
    ) -> Result<Outcome, RunError> {
        let ds = self.ds();
        crate::settings::store_runtime(
            &self.s.state,
            &ds,
            &crate::settings::GRAPHRAG,
            &serde_json::to_value(&settings).unwrap(),
        )
        .unwrap();
        build::run(&Run {
            st: &self.s.state,
            ds,
            settings,
            mode,
            confirmed: true,
            task: None,
            task_id: "t1".into(),
            cancel: Default::default(),
            limits,
        })
    }

    fn run(&self, settings: GraphragSettings, mode: Mode) -> Result<Outcome, RunError> {
        self.run_with(settings, mode, Limits::default())
    }

    fn rows(&self, q: &str) -> Vec<Vec<String>> {
        let mut v: Vec<Vec<String>> = sparkles::sparql::query(
            self.ds().store.snapshot(),
            q,
            &sparkles::sparql::QueryOptions::default(),
        )
        .unwrap()
        .rows()
        .unwrap()
        .into_iter()
        .map(|r| {
            r.values()
                .iter()
                .map(|t| t.as_ref().map(|t| t.to_string()).unwrap_or_default())
                .collect()
        })
        .collect();
        v.sort();
        v
    }

    /// The current report of the community that holds `member`.
    fn report_of(&self, member: &str) -> String {
        let r = self.rows(&format!(
            "SELECT ?r WHERE {{ GRAPH <{REPORTS}> {{ ?c <{}> <{member}> ; <{}> ?r }} }}",
            gr("member"),
            gr("report")
        ));
        assert_eq!(r.len(), 1, "{r:?}");
        r[0][0].trim_matches(['<', '>']).to_string()
    }

    fn community_of(&self, member: &str) -> String {
        let r = self.rows(&format!(
            "SELECT ?c WHERE {{ GRAPH <{REPORTS}> {{ ?c <{}> <{member}> }} }}",
            gr("member")
        ));
        r[0][0].trim_matches(['<', '>']).to_string()
    }
}

fn settings(extra: serde_json::Value) -> GraphragSettings {
    let mut v = serde_json::json!({
        "enabled": true,
        "principal": "rp",
        "projection": { "graphs": [ORG] },
        "levels": 1,
        "minCommunitySize": 2,
        "reviewPolicy": "main"
    });
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        for (k, x) in e {
            if x.is_null() {
                o.remove(k);
            } else {
                o.insert(k.clone(), x.clone());
            }
        }
    }
    serde_json::from_value(v).unwrap()
}

fn all() -> Option<Arc<sparkles::access::GraphAccess>> {
    Some(Arc::new(sparkles::access::GraphAccess::all()))
}

fn sha(t: &str) -> Sha256Digest {
    Sha256Digest::of_bytes(t.as_bytes(), &Control::none()).unwrap()
}

fn propose(ds: &sparkles::Dataset, source: &str, t: Triple) -> (NamedNode, Sha256Digest) {
    propose_in(ds, CLAIMS, source, t)
}

fn propose_in(
    ds: &sparkles::Dataset,
    graph: &str,
    source: &str,
    t: Triple,
) -> (NamedNode, Sha256Digest) {
    propose_by(ds, graph, source, t, iri("urn:publisher"))
}

fn propose_by(
    ds: &sparkles::Dataset,
    graph: &str,
    source: &str,
    t: Triple,
    author: NamedNode,
) -> (NamedNode, Sha256Digest) {
    let key = |k: &str| RecordKey::new(k).unwrap();
    let input = EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("rev"),
            source: iri(&format!("urn:source:{source}")),
            source_key: key(source),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: sha(source),
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
            proposition: t,
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: author,
            scope: Default::default(),
            confidence: Default::default(),
            validity: None,
        }],
        ..Default::default()
    };
    let mut o = ProposeOptions::new(iri("urn:principal:importer"));
    o.proposal_graph = Some(iri(graph));
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
    (r.iri.clone(), r.digest)
}

fn decide(ds: &sparkles::Dataset, c: &(NamedNode, Sha256Digest), d: Disposition, version: u32) {
    let mut o = ReviewOptions::new(iri("urn:principal:reviewer"));
    o.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        ds,
        ds,
        "main",
        version,
        &[DecisionInput {
            key: "d".into(),
            target: c.0.clone(),
            target_digest: c.1,
            disposition: d,
            supersedes: None,
        }],
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
}

fn withdraw(ds: &sparkles::Dataset, c: &(NamedNode, Sha256Digest)) {
    evidence::withdraw(
        ds,
        ds,
        &[WithdrawalInput {
            key: "w".into(),
            target: c.0.clone(),
            target_digest: c.1,
        }],
        &ReviewOptions::new(iri("urn:principal:reviewer")),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
}

fn leads() -> Triple {
    Triple::new(ex("x0"), ex("leads"), ex("x1"))
}

fn owns() -> Triple {
    Triple::new(ex("y0"), ex("owns"), ex("y1"))
}

fn label() -> Triple {
    Triple::new(ex("x2"), iri(LABEL), Literal::new_simple_literal("Lead x2"))
}

/// The IRIs a query's single column lists, without brackets.
fn column(rows: Vec<Vec<String>>) -> Vec<String> {
    rows.into_iter()
        .map(|r| r[0].trim_matches(['<', '>']).to_string())
        .collect()
}

/// Accepted facts cite their claims and decisions, the activity uses every claim behind
/// the input, a rejected proposition asserted by an ordinary graph enters without a
/// claim, the footprint lists the evidence graphs, the state records the pairs, and
/// `/community` and `community_reports` read the new members.
#[tokio::test(flavor = "multi_thread")]
async fn reports_under_a_policy_cite_claims_and_decisions() {
    let e = Arc::new(env(true));
    let ds = e.dataset();
    let a = propose(&ds, "advisory", leads());
    let l = propose(&ds, "badge", label());
    let r = propose(&ds, "rumour", owns());
    decide(&ds, &a, Disposition::Accepted, 1);
    decide(&ds, &l, Disposition::Accepted, 1);
    decide(&ds, &r, Disposition::Rejected, 1);
    let e2 = e.clone();
    let o =
        tokio::task::spawn_blocking(move || e2.run(settings(serde_json::json!({})), Mode::Build))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(o.stopped, None, "{o:?}");
    assert!(o.written >= 2, "{o:?}");
    let x = e.report_of("http://ex/x0");
    // the report names its policy, version and configuration
    let p = e.rows(&format!(
        "SELECT ?p ?v ?c WHERE {{ GRAPH <{REPORTS}> {{ <{x}> <{}> ?p ; <{}> ?v ; <{}> ?c }} }}",
        gr("reviewPolicy"),
        gr("policyVersion"),
        gr("evidenceConfiguration"),
    ));
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(p[0][1].starts_with("\"1\""), "{p:?}");
    assert!(p[0][2].contains('/'), "{p:?}");
    // the finding cites the accepted claim and its decision
    let claims = column(e.rows(&format!(
        "SELECT DISTINCT ?c WHERE {{ GRAPH <{REPORTS}> {{ <{x}> <{}> ?f . ?f <{}> ?c }} }}",
        gr("finding"),
        gr("citesClaim")
    )));
    assert_eq!(claims, vec![a.0.as_str().to_string()], "{claims:?}");
    let decisions = column(e.rows(&format!(
        "SELECT DISTINCT ?d WHERE {{ GRAPH <{REPORTS}> {{ <{x}> <{}> ?f . ?f <{}> ?d }} }}",
        gr("finding"),
        gr("citesDecision")
    )));
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    // the activity used the claim behind the label line as well, and its decision
    let used = column(e.rows(&format!(
        "SELECT ?u WHERE {{ GRAPH <{REPORTS}> {{ <{x}> <{PROV}wasGeneratedBy> ?a . ?a <{PROV}used> ?u }} }}"
    )));
    assert!(used.contains(&a.0.as_str().to_string()), "{used:?}");
    assert!(used.contains(&l.0.as_str().to_string()), "{used:?}");
    assert!(used.contains(&decisions[0]), "{used:?}");
    assert_eq!(used.len(), 4, "{used:?}");
    // the rejected claim is never cited, while the ordinary fact enters the input
    let y = e.report_of("http://ex/y0");
    let none = e.rows(&format!(
        "SELECT ?c WHERE {{ GRAPH <{REPORTS}> {{ ?f <{}> ?c }} }}",
        gr("citesClaim")
    ));
    assert!(
        !none.iter().any(|c| c[0].contains(r.0.as_str())),
        "{none:?}"
    );
    let owned = e.rows(&format!(
        "SELECT ?f WHERE {{ GRAPH <{REPORTS}> {{ <{y}> <{}> ?f . ?f <{}> <<( <http://ex/y0> <http://ex/owns> <http://ex/y1> )>> }} }}",
        gr("finding"),
        gr("cites")
    ));
    assert_eq!(owned.len(), 1, "{owned:?}");
    // no prompt names a record of the evidence graphs
    for q in e.mock.requests() {
        let p = mock::prompt_of(&q);
        assert!(!p.contains(a.0.as_str()) && !p.contains("urn:ev:"), "{p}");
    }
    // the footprint lists the decisions, supports and proposal graphs
    let fp = e.rows(&format!(
        "SELECT ?j WHERE {{ GRAPH <{}> {{ ?f <{}> ?j }} }}",
        crate::graphrag::guard::DISCLOSURE_GRAPH,
        gr("footprint")
    ));
    assert_eq!(fp.len(), 1, "{fp:?}");
    for g in [DECISIONS, SUPPORTS, CLAIMS, ACCEPTED] {
        assert!(fp[0][0].contains(g), "{g} missing from {}", fp[0][0]);
    }
    assert!(!fp[0][0].contains(SOURCES), "{}", fp[0][0]);
    // the state records the policy and the pairs of each report
    let st = e.s.state.graphrag.state(ds.store().dataset_id());
    let last = st.last_build.unwrap();
    assert_eq!(last.policy.as_ref().unwrap().name, "main");
    assert_eq!(last.pairs.len(), 2, "{:?}", last.pairs);
    for c in &last.communities {
        assert!(c.policy && c.pairs.is_some(), "{c:?}");
    }
    // `/community` reads the policy and the cited claims and decisions
    let c = e.community_of("http://ex/x0");
    let res = call(
        &e.s.app,
        "GET",
        &format!(
            "/$/graphrag/ev/community?iri={}",
            c.replace(':', "%3A").replace('/', "%2F")
        ),
        &[("authorization", &b("rad"))],
        "",
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let j = res.json();
    assert_eq!(j["report"]["reviewPolicy"]["version"], 1, "{j}");
    assert!(j["report"]["reviewPolicy"]["iri"].is_string(), "{j}");
    assert_eq!(
        j["report"]["findings"][0]["claims"],
        serde_json::json!([a.0.as_str()]),
        "{j}"
    );
    assert_eq!(
        j["report"]["findings"][0]["decisions"],
        serde_json::json!(decisions),
        "{j}"
    );
    // and so does `community_reports`
    let e3 = e.clone();
    let v = tokio::task::spawn_blocking(move || {
        tool_as(&e3.s, "rad", "community_reports", serde_json::json!({}))
    })
    .await
    .unwrap();
    let rep = v["reports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["iri"] == x)
        .cloned()
        .unwrap_or_else(|| panic!("{v}"));
    assert_eq!(rep["reviewPolicy"]["version"], 1, "{rep}");
    assert_eq!(
        rep["findings"][0]["claims"],
        serde_json::json!([a.0.as_str()]),
        "{rep}"
    );
}

/// A tool call as `user` on dataset `ev`.
fn tool_as(s: &AuthServer, user: &str, name: &str, args: serde_json::Value) -> serde_json::Value {
    let mut call = crate::mcp::Call {
        arrived: std::time::Instant::now(),
        cancel: Default::default(),
        request_id: "t".into(),
        principal: s
            .auth()
            .user_principal(user)
            .unwrap()
            .with_guard(s.state.graphrag_guard.clone(), s.state.access_epoch()),
        headers: None,
        held: None,
        grant: None,
        preparation: None,
    };
    let serde_json::Value::Object(mut m) = args else {
        panic!("arguments are an object")
    };
    m.insert("dataset".into(), "ev".into());
    let server = crate::mcp::McpServer::new(s.state.clone(), crate::mcp::McpConfig::default());
    match server.run_now(name, m, &mut call).unwrap() {
        crate::mcp::Outcome::Structured(v) => v,
        crate::mcp::Outcome::Text(t) => {
            serde_json::from_str(&t).unwrap_or(serde_json::Value::String(t))
        }
    }
}

fn refused(r: Result<Outcome, RunError>) -> String {
    match r {
        Err(RunError::Refused(m)) => m,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A policy on an unactivated store, an unknown policy, a matching policy and a
/// principal without the policy's grants are refused before any model call.
#[tokio::test(flavor = "multi_thread")]
async fn admission_refuses_before_any_model_call() {
    let plain = Arc::new(env(false));
    let p2 = plain.clone();
    let m = tokio::task::spawn_blocking(move || {
        refused(p2.run(settings(serde_json::json!({})), Mode::Build))
    })
    .await
    .unwrap();
    assert!(m.starts_with("configuration-history-inactive"), "{m}");
    assert!(plain.mock.requests().is_empty());
    let e = Arc::new(env(true));
    for (extra, why) in [
        (
            serde_json::json!({ "reviewPolicy": "nope" }),
            "an unknown policy",
        ),
        (
            serde_json::json!({ "reviewPolicy": "people" }),
            "a matching policy",
        ),
        (
            serde_json::json!({ "principal": "rnodec" }),
            "a principal without decisions",
        ),
    ] {
        let e2 = e.clone();
        let m = tokio::task::spawn_blocking(move || refused(e2.run(settings(extra), Mode::Build)))
            .await
            .unwrap();
        assert!(
            m.starts_with("reviewPolicy: no review policy") && !m.contains(DECISIONS),
            "{why}: {m}"
        );
    }
    assert!(e.mock.requests().is_empty());
    // a run without a policy on the unactivated store builds as before
    let p2 = plain.clone();
    let o = tokio::task::spawn_blocking(move || {
        p2.run(
            settings(serde_json::json!({ "reviewPolicy": null })),
            Mode::Build,
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert!(o.written >= 2, "{o:?}");
    let marks = plain.rows(&format!(
        "SELECT ?r WHERE {{ GRAPH <{REPORTS}> {{ ?r <{}> ?p }} }}",
        gr("reviewPolicy")
    ));
    assert!(marks.is_empty(), "{marks:?}");
    let used = plain.rows(&format!(
        "SELECT ?a WHERE {{ GRAPH <{REPORTS}> {{ ?a <{PROV}used> ?u }} }}"
    ));
    assert!(used.is_empty(), "{used:?}");
}

/// The projection never reads a proposal graph or another policy's output, even when it
/// names them, and a policy alone reads its output graph without any other graph.
#[tokio::test(flavor = "multi_thread")]
async fn other_evidence_graphs_are_never_read() {
    let e = Arc::new(env(true));
    let ds = e.dataset();
    let a = propose(&ds, "advisory", leads());
    decide(&ds, &a, Disposition::Accepted, 1);
    let e2 = e.clone();
    let o = tokio::task::spawn_blocking(move || {
        e2.run(
            settings(
                serde_json::json!({ "projection": { "graphs": [ORG, CLAIMS, MAP, "urn:ev:*"] } }),
            ),
            Mode::Build,
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert!(o.written >= 1, "{o:?}");
    for q in e.mock.requests() {
        let p = mock::prompt_of(&q);
        assert!(!p.contains(a.0.as_str()) && !p.contains("reifies"), "{p}");
    }
    // the accepted evidence alone: one community of the two people the claim links
    let e2 = e.clone();
    let o = tokio::task::spawn_blocking(move || {
        e2.run(
            settings(serde_json::json!({ "projection": { "graphs": [] } })),
            Mode::Build,
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(o.communities, 1, "{o:?}");
    let x = e.report_of("http://ex/x0");
    let claims = column(e.rows(&format!(
        "SELECT DISTINCT ?c WHERE {{ GRAPH <{REPORTS}> {{ <{x}> <{}> ?f . ?f <{}> ?c }} }}",
        gr("finding"),
        gr("citesClaim")
    )));
    assert_eq!(claims, vec![a.0.as_str().to_string()]);
}

/// A withdrawal rewrites a cited report below the fraction, while a new acceptance
/// follows the fraction; a fact with two supports keeps the surviving one.
#[tokio::test(flavor = "multi_thread")]
async fn withdrawals_rewrite_and_acceptances_follow_the_fraction() {
    let e = Arc::new(env(true));
    let ds = e.dataset();
    let a = propose(&ds, "advisory", leads());
    let m = propose(&ds, "mirror", leads());
    decide(&ds, &a, Disposition::Accepted, 1);
    decide(&ds, &m, Disposition::Accepted, 1);
    let s = settings(serde_json::json!({ "refresh": { "minChangedFraction": 1.0 } }));
    let (e2, s2) = (e.clone(), s.clone());
    tokio::task::spawn_blocking(move || e2.run(s2, Mode::Build).unwrap())
        .await
        .unwrap();
    let first = e.report_of("http://ex/x0");
    // a new acceptance of another fact of the community keeps the report
    let n = propose(
        &ds,
        "newsletter",
        Triple::new(ex("x1"), ex("leads"), ex("x2")),
    );
    decide(&ds, &n, Disposition::Accepted, 1);
    let (e2, s2) = (e.clone(), s.clone());
    let o = tokio::task::spawn_blocking(move || e2.run(s2, Mode::Refresh).unwrap())
        .await
        .unwrap();
    assert_eq!(o.written, 0, "{o:?}");
    assert_eq!(e.report_of("http://ex/x0"), first);
    // withdrawing one support leaves the fact, and rewrites the report that read it
    withdraw(&ds, &a);
    let (e2, s2) = (e.clone(), s.clone());
    let o = tokio::task::spawn_blocking(move || e2.run(s2, Mode::Refresh).unwrap())
        .await
        .unwrap();
    assert_eq!(o.written, 1, "{o:?}");
    let second = e.report_of("http://ex/x0");
    assert_ne!(second, first);
    let claims = column(e.rows(&format!(
        "SELECT DISTINCT ?c WHERE {{ GRAPH <{REPORTS}> {{ <{second}> <{}> ?f . ?f <{}> ?c }} }}",
        gr("finding"),
        gr("citesClaim")
    )));
    assert!(claims.contains(&m.0.as_str().to_string()), "{claims:?}");
    assert!(!claims.contains(&a.0.as_str().to_string()), "{claims:?}");
    // the other community's report stays
    assert_eq!(o.kept, o.communities - 1, "{o:?}");
}

/// A new policy version rewrites every report, and the automatic loop finds it due
/// without any data write.
#[tokio::test(flavor = "multi_thread")]
async fn a_policy_version_rewrites_every_report() {
    let e = Arc::new(env(true));
    let ds = e.dataset();
    let a = propose(&ds, "advisory", leads());
    decide(&ds, &a, Disposition::Accepted, 1);
    let s = settings(serde_json::json!({ "refresh": { "minChangedFraction": 1.0 } }));
    let (e2, s2) = (e.clone(), s.clone());
    tokio::task::spawn_blocking(move || e2.run(s2, Mode::Build).unwrap())
        .await
        .unwrap();
    let (e2, s2) = (e.clone(), s.clone());
    let due = tokio::task::spawn_blocking(move || {
        crate::graphrag::refresh::due_reason(&e2.s.state, &e2.ds(), &s2, &mut Default::default())
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(due, None);
    evidence::configure(&ds, &evidence_settings(2), &Control::none()).unwrap();
    let (e2, s2) = (e.clone(), s.clone());
    let due = tokio::task::spawn_blocking(move || {
        crate::graphrag::refresh::due_reason(&e2.s.state, &e2.ds(), &s2, &mut Default::default())
    })
    .await
    .unwrap()
    .unwrap();
    assert!(
        due.as_deref().is_some_and(|d| d.contains("version 2")),
        "{due:?}"
    );
    let (e2, s2) = (e.clone(), s.clone());
    let o = tokio::task::spawn_blocking(move || e2.run(s2, Mode::Refresh).unwrap())
        .await
        .unwrap();
    assert_eq!(o.written, o.communities, "{o:?}");
    // removing the policy rewrites them again, as reports without one
    let s3 = settings(
        serde_json::json!({ "reviewPolicy": null, "refresh": { "minChangedFraction": 1.0 } }),
    );
    let e2 = e.clone();
    let o = tokio::task::spawn_blocking(move || e2.run(s3, Mode::Refresh).unwrap())
        .await
        .unwrap();
    assert_eq!(o.written, o.communities, "{o:?}");
    let x = e.report_of("http://ex/x0");
    assert!(
        e.rows(&format!(
            "SELECT ?p WHERE {{ GRAPH <{REPORTS}> {{ <{x}> <{}> ?p }} }}",
            gr("reviewPolicy")
        ))
        .is_empty()
    );
}

/// A person's edit keeps the pairs of the report it revises, so an unchanged refresh
/// keeps it, and a withdrawal rewrites it; a report the state does not name is rewritten.
#[tokio::test(flavor = "multi_thread")]
async fn edits_keep_their_pairs_and_mismatches_are_rewritten() {
    let e = Arc::new(env(true));
    let ds = e.dataset();
    let a = propose(&ds, "advisory", leads());
    decide(&ds, &a, Disposition::Accepted, 1);
    let s = settings(serde_json::json!({ "refresh": { "minChangedFraction": 1.0 } }));
    let (e2, s2) = (e.clone(), s.clone());
    tokio::task::spawn_blocking(move || e2.run(s2, Mode::Build).unwrap())
        .await
        .unwrap();
    let first = e.report_of("http://ex/x0");
    let res = call(
        &e.s.app,
        "POST",
        "/$/graphrag/ev/review",
        &[
            ("authorization", &b("rad")),
            ("content-type", "application/json"),
        ],
        &serde_json::json!({ "report": first, "title": "Edited" }).to_string(),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let edit = e.report_of("http://ex/x0");
    assert_ne!(edit, first);
    // the edit copies the policy and the citations, and has no activity
    let claims = column(e.rows(&format!(
        "SELECT DISTINCT ?c WHERE {{ GRAPH <{REPORTS}> {{ <{edit}> <{}> ?p ; <{}> ?f . ?f <{}> ?c FILTER NOT EXISTS {{ <{edit}> <{PROV}wasGeneratedBy> ?a }} }} }}",
        gr("reviewPolicy"),
        gr("finding"),
        gr("citesClaim")
    )));
    assert_eq!(claims, vec![a.0.as_str().to_string()]);
    // and names the report it edits, which retention follows
    let of = column(e.rows(&format!(
        "SELECT ?o WHERE {{ GRAPH <{REPORTS}> {{ <{edit}> <{}> ?o }} }}",
        gr("editOf")
    )));
    assert_eq!(of, vec![first.clone()]);
    let (e2, s2) = (e.clone(), s.clone());
    let o = tokio::task::spawn_blocking(move || e2.run(s2, Mode::Refresh).unwrap())
        .await
        .unwrap();
    assert_eq!(o.written, 0, "{o:?}");
    assert_eq!(e.report_of("http://ex/x0"), edit);
    // the edited report and its activity stay through the refresh's clean-up
    let kept = e.rows(&format!(
        "SELECT ?u WHERE {{ GRAPH <{REPORTS}> {{ <{first}> <{PROV}wasGeneratedBy> ?a . ?a <{PROV}used> ?u }} }}"
    ));
    assert!(!kept.is_empty());
    withdraw(&ds, &a);
    let (e2, s2) = (e.clone(), s.clone());
    let o = tokio::task::spawn_blocking(move || e2.run(s2, Mode::Refresh).unwrap())
        .await
        .unwrap();
    assert_eq!(o.written, 1, "{o:?}");
    assert_ne!(e.report_of("http://ex/x0"), edit);
    // a state that names another report than the graph links, as after a crash between
    // two level commits, rewrites it
    let mut st = e.s.state.graphrag.state(ds.store().dataset_id());
    for c in &mut st.last_build.as_mut().unwrap().communities {
        c.report = Some("urn:uuid:00000000-0000-0000-0000-00000000dead".into());
    }
    e.s.state.graphrag.set_state(ds.store().dataset_id(), st);
    let (e2, s2) = (e.clone(), s.clone());
    let o = tokio::task::spawn_blocking(move || e2.run(s2, Mode::Refresh).unwrap())
        .await
        .unwrap();
    assert_eq!(o.written, o.communities, "{o:?}");
}

/// The bounds of a run's evidence refuse rather than truncate.
#[tokio::test(flavor = "multi_thread")]
async fn bounds_refuse_the_run() {
    let e = Arc::new(env(true));
    let ds = e.dataset();
    let a = propose(&ds, "advisory", leads());
    decide(&ds, &a, Disposition::Accepted, 1);
    for (limits, what) in [
        (
            Limits {
                input: 0,
                ..Limits::default()
            },
            "behind the input of community",
        ),
        (
            Limits {
                occurrences: 0,
                ..Limits::default()
            },
            "claim occurrences",
        ),
        (
            Limits {
                bytes: 16,
                ..Limits::default()
            },
            "bytes",
        ),
    ] {
        let e2 = e.clone();
        let m = tokio::task::spawn_blocking(move || {
            refused(e2.run_with(settings(serde_json::json!({})), Mode::Build, limits))
        })
        .await
        .unwrap();
        assert!(
            m.starts_with("incomplete-evidence") && m.contains(what),
            "{m}"
        );
    }
    assert!(e.mock.requests().is_empty());
    // the allowance also covers the level commits, which stop the run before they
    // commit
    let e2 = e.clone();
    let m = tokio::task::spawn_blocking(move || {
        refused(e2.run_with(
            settings(serde_json::json!({})),
            Mode::Build,
            Limits {
                bytes: 4096,
                ..Limits::default()
            },
        ))
    })
    .await
    .unwrap();
    assert!(m.starts_with("incomplete-evidence"), "{m}");
    assert!(
        e.rows(&format!(
            "SELECT ?r WHERE {{ GRAPH <{REPORTS}> {{ ?r a <{}> }} }}",
            gr("Report")
        ))
        .is_empty()
    );
}

/// A reader that may not read the proposal graph is admitted to reports built without a
/// policy and not to reports built under one.
#[tokio::test(flavor = "multi_thread")]
async fn readers_need_the_evidence_graphs() {
    let e = Arc::new(env(true));
    let ds = e.dataset();
    let a = propose(&ds, "advisory", leads());
    decide(&ds, &a, Disposition::Accepted, 1);
    let get = |e: Arc<Env>, user: &'static str| async move {
        let c = e.community_of("http://ex/x0");
        call(
            &e.s.app,
            "GET",
            &format!(
                "/$/graphrag/ev/community?iri={}",
                c.replace(':', "%3A").replace('/', "%2F")
            ),
            &[("authorization", &b(user))],
            "",
        )
        .await
        .status
    };
    let e2 = e.clone();
    tokio::task::spawn_blocking(move || {
        e2.run(
            settings(serde_json::json!({ "reviewPolicy": null })),
            Mode::Build,
        )
        .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(get(e.clone(), "rnoclaims").await, StatusCode::OK);
    let e2 = e.clone();
    tokio::task::spawn_blocking(move || {
        e2.run(settings(serde_json::json!({})), Mode::Build)
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(get(e.clone(), "rp").await, StatusCode::OK);
    assert_eq!(get(e.clone(), "rnoclaims").await, StatusCode::NOT_FOUND);
}

/// A change of the evidence settings after admission stops the run before it publishes.
#[tokio::test(flavor = "multi_thread")]
async fn a_changed_evidence_configuration_stops_the_run() {
    let e = Arc::new(env(true));
    let e2 = e.clone();
    tokio::task::spawn_blocking(move || {
        let ev = Evidence::admit(
            &e2.ds(),
            Some("main"),
            None,
            Default::default(),
            Limits::default(),
            &e2.ds().dataset.query_options(),
        )
        .unwrap()
        .unwrap();
        let check = ev.check();
        check.check().unwrap();
        evidence::configure(&e2.dataset(), &evidence_settings(2), &Control::none()).unwrap();
        match check.check() {
            Err(RunError::Refused(m)) => assert!(m.contains("evidence settings changed"), "{m}"),
            other => panic!("{other:?}"),
        }
    })
    .await
    .unwrap();
}

/// A run under a policy that committed its reports and stopped before it saved the
/// state leaves a state that says no report records a policy. A later run without the
/// policy still rewrites those reports, because it reads their policy from the graph.
#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_policy_run_is_rewritten_without_the_policy() {
    let e = Arc::new(env(true));
    let ds = e.dataset();
    let a = propose(&ds, "advisory", leads());
    decide(&ds, &a, Disposition::Accepted, 1);
    let plain = settings(
        serde_json::json!({ "reviewPolicy": null, "refresh": { "minChangedFraction": 1.0 } }),
    );
    let (e2, s2) = (e.clone(), plain.clone());
    tokio::task::spawn_blocking(move || e2.run(s2, Mode::Build).unwrap())
        .await
        .unwrap();
    let before = e.s.state.graphrag.state(ds.store().dataset_id());
    let e2 = e.clone();
    tokio::task::spawn_blocking(move || {
        e2.run(
            settings(serde_json::json!({ "refresh": { "minChangedFraction": 1.0 } })),
            Mode::Refresh,
        )
        .unwrap()
    })
    .await
    .unwrap();
    // the state as it was before the policy run saved its own
    e.s.state
        .graphrag
        .set_state(ds.store().dataset_id(), before);
    let (e2, s2) = (e.clone(), plain.clone());
    let o = tokio::task::spawn_blocking(move || e2.run(s2, Mode::Refresh).unwrap())
        .await
        .unwrap();
    assert_eq!(o.written, o.communities, "{o:?}");
    assert!(
        e.rows(&format!(
            "SELECT ?r WHERE {{ GRAPH <{REPORTS}> {{ ?c <{}> ?r . ?r <{}> ?p }} }}",
            gr("report"),
            gr("reviewPolicy")
        ))
        .is_empty()
    );
}

/// The private check of the automatic refresh (spec C25, "Refresh"). At server start,
/// after a withdrawal in a proposal graph the principal cannot read, after an unrelated
/// decision, after a settings-only change of the evidence configuration and after a
/// reload of the access policy, the check runs and leaves every report, status, state,
/// commit and task as it was. A withdrawal behind a cited claim and a state left behind
/// a crash start a refresh that names no record.
#[tokio::test(flavor = "multi_thread")]
async fn private_checks_start_refreshes_only_for_changed_inputs() {
    use crate::graphrag::refresh::{CHECK_REASON, Watch, tick_once};
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    let e = Arc::new(env(true));
    let e2 = e.clone();
    tokio::task::spawn_blocking(move || {
        let e = e2;
        let ds = e.dataset();
        let mut with_hidden = evidence_settings(1);
        with_hidden.proposals.push(HIDDEN.into());
        evidence::configure(&ds, &with_hidden, &Control::none()).unwrap();
        let a = propose(&ds, "advisory", leads());
        decide(&ds, &a, Disposition::Accepted, 1);
        let hidden = propose_in(&ds, HIDDEN, "whisper", leads());
        let unrelated = propose(&ds, "aside", Triple::new(ex("z0"), ex("knows"), ex("z1")));
        let s = settings(serde_json::json!({
            "principal": "rhide",
            "refresh": { "mode": "auto", "debounceSeconds": 1, "minChangedFraction": 1.0 }
        }));
        e.run(s, Mode::Build).unwrap();
        let tasks = || -> Vec<crate::state::Task> {
            e.s.state
                .tasks
                .lock()
                .iter()
                .filter(|t| t.kind == build::TASK_KIND)
                .cloned()
                .collect()
        };
        let wait = || {
            let end = Instant::now() + Duration::from_secs(60);
            while e.s.state.active_task(build::TASK_KIND, "ev").is_some() {
                assert!(Instant::now() < end, "the refresh did not finish");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let reports = || {
            e.rows(&format!(
                "SELECT ?s ?p ?o WHERE {{ GRAPH <{REPORTS}> {{ ?s ?p ?o }} }}"
            ))
        };
        let state = || e.s.state.graphrag.state(ds.store().dataset_id());
        let runs = |w: &HashMap<String, Watch>| w.get("ev").map_or(0, |w| w.private().runs);
        let sec = Duration::from_secs(1);
        let mut now = Instant::now();
        let mut w: HashMap<String, Watch> = HashMap::new();
        // everything a reader could see, which an empty check leaves alone
        let seen = |e: &Env| (reports(), state(), e.mock.requests().len());
        let before = seen(&e);
        let quiet = |w: &mut HashMap<String, Watch>, now: &mut Instant, ticks: usize| {
            for _ in 0..ticks {
                *now += sec;
                tick_once(&e.s.state, w, *now);
            }
        };
        // the first sight of the dataset, as at server start, checks it
        tick_once(&e.s.state, &mut w, now);
        assert_eq!(runs(&w), 1);
        quiet(&mut w, &mut now, 2);
        assert_eq!(runs(&w), 1, "an idle dataset is checked once");
        assert!(tasks().is_empty(), "{:?}", tasks());
        assert_eq!(seen(&e), before);
        // a withdrawal the principal cannot read, and a decision of an unrelated claim
        for change in 0..2 {
            let head = ds.store().head_commit().seq;
            if change == 0 {
                withdraw(&ds, &hidden);
            } else {
                decide(&ds, &unrelated, Disposition::Rejected, 1);
            }
            let after = ds.store().head_commit().seq;
            assert!(after > head);
            let n = runs(&w);
            quiet(&mut w, &mut now, 3);
            assert_eq!(runs(&w), n + 1, "change {change}");
            assert!(tasks().is_empty(), "change {change}: {:?}", tasks());
            assert_eq!(seen(&e), before, "change {change}");
            assert_eq!(ds.store().head_commit().seq, after, "change {change}");
        }
        // a settings-only change of the evidence configuration, and an access reload
        let mut wider = with_hidden.clone();
        wider.source_records.push("urn:ev:more-sources".into());
        evidence::configure(&ds, &wider, &Control::none()).unwrap();
        let n = runs(&w);
        quiet(&mut w, &mut now, 3);
        assert_eq!(runs(&w), n + 1);
        e.s.auth().reload().unwrap();
        quiet(&mut w, &mut now, 2);
        assert_eq!(runs(&w), n + 2);
        assert!(tasks().is_empty(), "{:?}", tasks());
        assert_eq!(seen(&e), before);
        // a withdrawal behind the cited claim ends its pair: the check starts a refresh
        let first = e.report_of("http://ex/x0");
        withdraw(&ds, &a);
        quiet(&mut w, &mut now, 3);
        let started = tasks();
        assert_eq!(started.len(), 1, "{started:?}");
        wait();
        let done = tasks();
        assert_eq!(done[0].state, "done", "{:?}", done[0]);
        let msg = done[0].message.clone().unwrap_or_default();
        assert!(msg.contains(CHECK_REASON), "{msg}");
        assert_ne!(e.report_of("http://ex/x0"), first);
        // the refresh cleared the check, and its own commit starts nothing
        quiet(&mut w, &mut now, 3);
        assert!(!w["ev"].private().due);
        assert_eq!(tasks().len(), 1);
        // a state left behind the reports graph, found when the server starts again
        let mut st = state();
        for c in &mut st.last_build.as_mut().unwrap().communities {
            c.report = Some("urn:uuid:00000000-0000-0000-0000-00000000dead".into());
        }
        e.s.state.graphrag.set_state(ds.store().dataset_id(), st);
        let mut w: HashMap<String, Watch> = HashMap::new();
        tick_once(&e.s.state, &mut w, now);
        assert_eq!(tasks().len(), 2, "{:?}", tasks());
        wait();
        assert!(tasks().iter().all(|t| t.state == "done"), "{:?}", tasks());
    })
    .await
    .unwrap();
}

/// The change feed counts only the graphs that a run under the policy reads: a claim in
/// a proposal graph that a wildcard of the projection names, attributed to a member,
/// joins no community and changes no report's input.
#[tokio::test(flavor = "multi_thread")]
async fn the_feed_counts_only_the_graphs_a_run_reads() {
    let e = Arc::new(env(true));
    let e2 = e.clone();
    tokio::task::spawn_blocking(move || {
        let e = e2;
        let ds = e.dataset();
        let a = propose(&ds, "advisory", leads());
        decide(&ds, &a, Disposition::Accepted, 1);
        let s = settings(serde_json::json!({
            "projection": { "graphs": [ORG, "urn:ev:*"] },
            "refresh": { "minChangedFraction": 1.0 }
        }));
        e.run(s.clone(), Mode::Build).unwrap();
        let due = |e: &Env| {
            crate::graphrag::refresh::due_reason(&e.s.state, &e.ds(), &s, &mut Default::default())
                .unwrap()
        };
        assert_eq!(due(&e), None);
        propose_by(
            &ds,
            CLAIMS,
            "memo",
            Triple::new(ex("z0"), ex("knows"), ex("z1")),
            ex("x0"),
        );
        assert_eq!(due(&e), None);
    })
    .await
    .unwrap();
}

/// Refreshes that private checks start and that keep failing back off further each
/// time: a check finds the withdrawal, and a refresh without a model provider fails.
#[tokio::test(flavor = "multi_thread")]
async fn failed_refreshes_after_checks_back_off() {
    use crate::graphrag::refresh::{Watch, tick_once};
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    let e = Arc::new(env(true));
    let e2 = e.clone();
    tokio::task::spawn_blocking(move || {
        let e = e2;
        let ds = e.dataset();
        let a = propose(&ds, "advisory", leads());
        let m = propose(&ds, "mirror", leads());
        decide(&ds, &a, Disposition::Accepted, 1);
        decide(&ds, &m, Disposition::Accepted, 1);
        let s = settings(serde_json::json!({
            "refresh": { "mode": "auto", "debounceSeconds": 1, "minChangedFraction": 1.0 }
        }));
        e.run(s, Mode::Build).unwrap();
        let tasks = || -> Vec<crate::state::Task> {
            e.s.state
                .tasks
                .lock()
                .iter()
                .filter(|t| t.kind == build::TASK_KIND)
                .cloned()
                .collect()
        };
        let wait = || {
            let end = Instant::now() + Duration::from_secs(60);
            while e.s.state.active_task(build::TASK_KIND, "ev").is_some() {
                assert!(Instant::now() < end, "the refresh did not finish");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let sec = Duration::from_secs(1);
        let mut now = Instant::now();
        let mut w: HashMap<String, Watch> = HashMap::new();
        tick_once(&e.s.state, &mut w, now);
        assert!(tasks().is_empty());
        e.s.state.models.store(None);
        withdraw(&ds, &a);
        // the first refresh starts after the debounce, and fails
        let mut started = Vec::new();
        for t in 1..=40u32 {
            now += sec;
            tick_once(&e.s.state, &mut w, now);
            if tasks().len() > started.len() {
                started.push(t);
                wait();
                assert_eq!(tasks().last().unwrap().state, "failed");
            }
            if started.len() == 4 {
                break;
            }
        }
        // each retry waits twice as long as the one before
        assert_eq!(started.len(), 4, "{started:?}");
        let gaps: Vec<u32> = started.windows(2).map(|p| p[1] - p[0]).collect();
        assert!(gaps.windows(2).all(|g| g[1] > g[0]), "{started:?}");
    })
    .await
    .unwrap();
}

/// A pooled server running a GraphRAG build under a review policy must share the
/// catalog's memory pool instead of failing with query-memory: foreign-domain.
#[tokio::test(flavor = "multi_thread")]
async fn pooled_server_evidence_build_succeeds_and_returns_pool_credit() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        crate::state::AppState::with_memory(
            dir.path(),
            sparkles::store::StoreOptions::default(),
            std::time::Duration::from_secs(30),
            Some(128 << 20),
        )
        .unwrap(),
    );
    let ds = st.attach("ev", DbType::Memory, None).unwrap();
    let p = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    ds.dataset
        .store()
        .activate_memory_configuration_history(&p)
        .unwrap();
    drop(p);
    evidence::configure(&ds.dataset, &evidence_settings(1), &Control::none()).unwrap();
    let mut org = crate::graphrag::build::tests::data(4);
    org.push_str(&format!(
        "<{ORG}> {{ <http://ex/y0> <http://ex/owns> <http://ex/y1> . }}\n"
    ));
    ds.store
        .load(&[Source::from_bytes(
            org.into_bytes(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let a = propose(&ds.dataset, "advisory", leads());
    decide(&ds.dataset, &a, Disposition::Accepted, 1);
    let mock = MockModel::start(|r, _| (200, mock::openai(&answer_all(r))));
    st.models
        .store(Some(Arc::new(crate::graphrag::build::tests::models_for(
            &mock.url(),
            &serde_json::json!({ "provider": "p", "model": "m" }),
        ))));
    crate::settings::store_runtime(
        &st,
        &ds,
        &crate::settings::ASSISTANT,
        &serde_json::json!({ "send": "rows" }),
    )
    .unwrap();
    let s = settings(serde_json::json!({}));
    crate::settings::store_runtime(
        &st,
        &ds,
        &crate::settings::GRAPHRAG,
        &serde_json::to_value(&s).unwrap(),
    )
    .unwrap();
    let st_bg = st.clone();
    let ds_bg = ds.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        build::run(&Run {
            st: &st_bg,
            ds: ds_bg,
            settings: s,
            mode: Mode::Build,
            confirmed: true,
            task: None,
            task_id: "pooled-1".into(),
            cancel: Default::default(),
            limits: Limits::default(),
        })
    })
    .await
    .unwrap()
    .unwrap();
    assert!(outcome.communities > 0);
    assert_eq!(st.catalog.pool().unwrap().granted(), 0);

    // Verify Evidence::admit keeps the 64 MiB selection ceiling distinct from the
    // configured query_memory_bytes (> 64 MiB and unbounded None) without grabbing
    // the entire pool during selection.
    let big_pool = sparkles::sparql::pool::QueryPool::new(128 << 20);
    {
        let big_grant = Arc::new(big_pool.empty_grant());
        let mut big_opts = ds.dataset.query_options();
        big_opts.grant = Some(big_grant.clone());
        big_opts.max_memory_bytes = Some(256 << 20);
        let ev_big = Evidence::admit(
            &ds,
            Some("main"),
            None,
            Default::default(),
            Limits::default(),
            &big_opts,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev_big.preparation_limit(),
            256 << 20,
            "root execution limit must match configured query cap, not cap + VIEW_BYTES"
        );
        assert_eq!(ev_big.selection_limit(), 64 << 20);
        assert!(
            ev_big.available_execution_bytes() >= (256 << 20) - 4096,
            "selection must not reduce usable execution headroom: available={}",
            ev_big.available_execution_bytes()
        );
        assert!(
            big_grant.peak() <= 64 << 20,
            "selection temporary reservation exceeded 64 MiB ceiling: peak={}",
            big_grant.peak()
        );
        let mut live_query_opts = big_opts.clone();
        live_query_opts.graphs = Some(Arc::new(sparkles::access::GraphAccess::all()));
        let live_res = sparkles::sparql::query_source(
            ev_big.read_source(),
            ds.dataset.store(),
            "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1",
            &live_query_opts,
        )
        .unwrap();
        drop(live_res);
        drop(ev_big);
        assert_eq!(big_grant.used(), 0);
    }
    {
        let unbounded_grant = Arc::new(big_pool.empty_grant());
        let mut unbounded_opts = ds.dataset.query_options();
        unbounded_opts.grant = Some(unbounded_grant.clone());
        unbounded_opts.max_memory_bytes = None;
        let ev_unbounded = Evidence::admit(
            &ds,
            Some("main"),
            None,
            Default::default(),
            Limits::default(),
            &unbounded_opts,
        )
        .unwrap()
        .unwrap();
        assert_eq!(ev_unbounded.preparation_limit(), u64::MAX);
        assert_eq!(ev_unbounded.selection_limit(), 64 << 20);
        assert!(
            unbounded_grant.peak() <= 64 << 20,
            "unbounded query selection must not reserve the entire pool: peak={}",
            unbounded_grant.peak()
        );
        drop(ev_unbounded);
        assert_eq!(unbounded_grant.used(), 0);
    }
    assert_eq!(big_pool.granted(), 0);
    assert_eq!(st.catalog.pool().unwrap().granted(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn declared_drift_refuses_policy_build_and_refresh_before_model_calls() {
    use crate::settings::{Declared, Providers};
    let e = Arc::new(env(true));
    let declared = Declared::parse(
        r#"{"datasets":{"ev":{"evidence":{"enabled":true,"sourceRecords":["urn:ev:changed"],"proposals":["urn:ev:claims"]}}}}"#,
        Providers::Unchecked,
    ).unwrap();
    e.s.state
        .settings
        .replace_declared_for_test(Arc::new(declared));

    let runner = e.clone();
    tokio::task::spawn_blocking(move || {
        for mode in [Mode::Build, Mode::Refresh] {
            let message = refused(runner.run(settings(serde_json::json!({})), mode));
            assert!(message.starts_with("evidence-settings-drift"), "{message}");
        }
    })
    .await
    .unwrap();
    assert!(e.mock.requests().is_empty());
}

/// With `reviewPolicy` and `refresh.mode: "auto"`, the internal F13 circuit excludes
/// non-output evidence graphs even when a projection wildcard matches them, so a claim
/// written to `CLAIMS` neither joins a community nor triggers an uncounted fallback.
#[tokio::test(flavor = "multi_thread")]
async fn auto_circuit_counts_only_policy_readable_graphs_without_uncounted_fallback() {
    use crate::graphrag::refresh::Watch;
    let e = Arc::new(env(true));
    let e2 = e.clone();
    tokio::task::spawn_blocking(move || {
        let e = e2;
        let ds = e.dataset();
        let a = propose(&ds, "advisory", leads());
        decide(&ds, &a, Disposition::Accepted, 1);
        let s = settings(serde_json::json!({
            "projection": { "graphs": [ORG, "urn:ev:*"] },
            "refresh": { "mode": "auto", "debounceSeconds": 1, "minChangedFraction": 1.0 }
        }));
        e.run(s.clone(), Mode::Build).unwrap();
        let srv_ds = e.ds();
        assert_eq!(
            srv_ds.dataset.inner_state().views.internal_count(),
            1,
            "auto mode under reviewPolicy attaches the internal circuit"
        );
        let mut w = Watch::default();
        // Prime `note_private` baseline so `due` starts false after first sight.
        assert_eq!(
            crate::graphrag::refresh::due_reason(&e.s.state, &srv_ds, &s, &mut w).unwrap(),
            None
        );
        // Clear initial first-sight private flag via `tick_once` pattern: we only check
        // whether `propose_by` in excluded CLAIMS triggers `due_reason` or `uncounted`.
        let mut w2 = Watch::default();
        // Run once to initialize private watch at current head.
        let _ = crate::graphrag::refresh::due_reason(&e.s.state, &srv_ds, &s, &mut w2).unwrap();
        // Simulate private check already cleared for the initial build.
        let mut watches = std::collections::HashMap::from([("ev".to_string(), w2)]);
        let now = std::time::Instant::now();
        crate::graphrag::refresh::tick_once(&e.s.state, &mut watches, now);
        crate::graphrag::refresh::tick_once(
            &e.s.state,
            &mut watches,
            now + std::time::Duration::from_secs(2),
        );
        assert!(!watches["ev"].private().due);
        // Writing to excluded CLAIMS (which matches `urn:ev:*` wildcard!) must be ignored
        // by the accumulator without triggering `uncounted` or joining a community.
        propose_by(
            &ds,
            CLAIMS,
            "memo",
            Triple::new(ex("z0"), ex("knows"), ex("z1")),
            ex("x0"),
        );
        let reason = crate::graphrag::refresh::due_reason(
            &e.s.state,
            &srv_ds,
            &s,
            watches.get_mut("ev").unwrap(),
        )
        .unwrap();
        assert_eq!(reason, None);
        assert!(
            !watches["ev"].private().due,
            "excluded CLAIMS write must be counted as 0 by the circuit, not fall back to uncounted"
        );
        // Writing an entity-joining edge into ORG IS counted by the circuit.
        sparkles::sparql::update::update(
            &srv_ds.store,
            &format!(
                "INSERT DATA {{ GRAPH <{ORG}> {{ <http://ex/x0> <http://ex/knows> <http://ex/fresh> }} }}"
            ),
            &Default::default(),
        )
        .unwrap();
        let reason = crate::graphrag::refresh::due_reason(
            &e.s.state,
            &srv_ds,
            &s,
            watches.get_mut("ev").unwrap(),
        )
        .unwrap();
        assert!(
            reason
                .as_deref()
                .is_some_and(|r| r.contains("http://ex/fresh") && r.contains("joined")),
            "{reason:?}"
        );

        // An attach-time failure under `reviewPolicy` must route through the C25 private
        // check fallback (`Ok(None)` + `private.due = true`) rather than returning `Err`.
        e.s.state.graphrag.detach_circuit("ev");
        let views = &srv_ds.dataset.inner_state().views;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while views.internal_count() > 0 && std::time::Instant::now() < deadline {
            views.settle(std::time::Duration::from_secs(2));
        }
        assert_eq!(
            views.internal_count(),
            0,
            "old refresh circuit must be collected before inducing C25 attach failure"
        );
        let prev_opts = views.options();
        let mut failing_opts = prev_opts.clone();
        failing_opts.pass_timeout = std::time::Duration::ZERO;
        views.set_options(failing_opts);
        // Write to ORG (not an evidence graph), while `watches["ev"].private().due` is
        // false, so only the attach failure can set `private.due = true`.
        assert!(!watches["ev"].private().due);
        sparkles::sparql::update::update(
            &srv_ds.store,
            &format!("INSERT DATA {{ GRAPH <{ORG}> {{ <http://ex/x0> <http://ex/note> \"af\" }} }}"),
            &Default::default(),
        )
        .unwrap();
        let r = crate::graphrag::refresh::due_reason(
            &e.s.state,
            &srv_ds,
            &s,
            watches.get_mut("ev").unwrap(),
        )
        .unwrap();
        assert_eq!(
            r, None,
            "attach failure under reviewPolicy must not surface as a direct refresh reason"
        );
        assert!(
            watches["ev"].private().due,
            "attach failure under reviewPolicy must schedule the private check"
        );
        assert_eq!(
            views.internal_count(),
            0,
            "failed attach must not leave a circuit attached"
        );
        assert!(!e.s.state.graphrag.has_refresh_circuit("ev", &srv_ds));
        views.set_options(prev_opts);

        // With `private.due == true`, acquiring a `Restore` reservation while `tick_once`
        // is already processing `ev` must preserve `NotLive` through `due_outcome` so
        // neither `private_check` nor `build::spawn` runs against the draining dataset.
        let held_res: Arc<parking_lot::Mutex<Option<sparkles::catalog::Reservation>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let private_hook_hit = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let held_hook = held_res.clone();
        let private_hit_hook = private_hook_hit.clone();
        let st_hook = e.s.state.clone();
        let _guard = crate::graphrag::refresh::set_tick_hook(move |point, name| {
            if point == crate::graphrag::refresh::TickHookPoint::BeforeDue && name == "ev" {
                let r = st_hook
                    .catalog
                    .reserve(
                        "ev",
                        sparkles::catalog::ReservationKind::Restore,
                        "c25-restore-mid-tick",
                    )
                    .unwrap();
                *held_hook.lock() = Some(r);
            } else if point == crate::graphrag::refresh::TickHookPoint::BeforePrivateCheck
                && name == "ev"
            {
                private_hit_hook.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        crate::graphrag::refresh::tick_once(
            &e.s.state,
            &mut watches,
            std::time::Instant::now() + std::time::Duration::from_secs(10),
        );
        drop(_guard);
        assert!(
            held_res.lock().is_some(),
            "test hook must have acquired restore reservation before due_outcome"
        );
        assert!(
            !private_hook_hit.load(std::sync::atomic::Ordering::SeqCst),
            "NotLive from due_outcome must skip private_check instead of falling through"
        );
        assert!(
            !watches.contains_key("ev"),
            "watch must be removed when due_outcome returns NotLive"
        );
        assert!(
            e.s.state.tasks.lock().is_empty(),
            "restore reservation mid-tick must not admit a C25 refresh task"
        );
        assert!(!e.s.state.graphrag.has_refresh_circuit("ev", &srv_ds));
        drop(held_res.lock().take());

        // Re-attach cleanly, then break `reviewPolicy` so `Evidence::admit` fails:
        // `tick_once` must detach the previously attached internal circuit.
        let now2 = std::time::Instant::now();
        crate::graphrag::refresh::tick_once(&e.s.state, &mut watches, now2);
        assert_eq!(views.internal_count(), 1);
        let broken = settings(serde_json::json!({
            "projection": { "graphs": [ORG, "urn:ev:*"] },
            "reviewPolicy": "missing-policy",
            "refresh": { "mode": "auto", "debounceSeconds": 1, "minChangedFraction": 1.0 }
        }));
        crate::settings::store_runtime(
            &e.s.state,
            &srv_ds,
            &crate::settings::GRAPHRAG,
            &serde_json::to_value(&broken).unwrap(),
        )
        .unwrap();
        crate::graphrag::refresh::tick_once(
            &e.s.state,
            &mut watches,
            now2 + std::time::Duration::from_secs(1),
        );
        views.settle(std::time::Duration::from_secs(10));
        assert!(
            !e.s.state.graphrag.has_refresh_circuit("ev", &srv_ds),
            "failing evidence admission must detach the stale internal circuit"
        );
        assert_eq!(views.internal_count(), 0);
    })
    .await
    .unwrap();
}

/// A `Restore` reservation acquired right before `Run::private_check`'s preparation
/// admission boundary prevents entering `prepare`, and a reservation acquired once
/// preparation has been admitted (outside `restore_admission`) does not block on the
/// scan and cancels the in-flight check promptly.
#[tokio::test(flavor = "multi_thread")]
async fn private_check_preparation_is_ordered_with_restore_reservation() {
    use crate::graphrag::build::{BuildHookPoint, set_build_hook};
    use crate::graphrag::refresh::{Watch, tick_once};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    let e = Arc::new(env(true));
    let e2 = e.clone();
    tokio::task::spawn_blocking(move || {
        let e = e2;
        let ds = e.dataset();
        let a = propose(&ds, "advisory", leads());
        decide(&ds, &a, Disposition::Accepted, 1);
        let s = settings(serde_json::json!({
            "projection": { "graphs": [ORG, "urn:ev:*"] },
            "refresh": { "mode": "auto", "debounceSeconds": 1, "minChangedFraction": 1.0 }
        }));
        e.run(s, Mode::Build).unwrap();

        // 1. Reservation acquired right before private-check preparation admission must
        // block entry into `prepare` and return `NotLive`.
        let held_res: Arc<parking_lot::Mutex<Option<sparkles::catalog::Reservation>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let prepare_entered = Arc::new(AtomicBool::new(false));
        let admit_hit = Arc::new(AtomicBool::new(false));
        let held_hook = held_res.clone();
        let prepare_hook = prepare_entered.clone();
        let admit_hook = admit_hit.clone();
        let st_hook = e.s.state.clone();
        let _guard = set_build_hook(move |point, name| {
            if point == BuildHookPoint::PrivateAdmit && name == "ev" {
                admit_hook.store(true, Ordering::SeqCst);
                let r = st_hook
                    .reserve_restore("ev", "c25-before-private-admit")
                    .unwrap();
                *held_hook.lock() = Some(r);
            } else if point == BuildHookPoint::PrivatePrepare && name == "ev" {
                prepare_hook.store(true, Ordering::SeqCst);
            }
        });
        let mut watches: HashMap<String, Watch> = HashMap::new();
        let now = Instant::now();
        tick_once(&e.s.state, &mut watches, now);
        drop(_guard);
        assert!(
            admit_hit.load(Ordering::SeqCst),
            "hook must fire right before private_check preparation admission"
        );
        assert!(
            !prepare_entered.load(Ordering::SeqCst),
            "restore reservation before private admission must prevent entering prepare"
        );
        assert!(
            !watches.contains_key("ev"),
            "watch must be removed when private check sees restore reservation"
        );
        assert!(e.s.state.tasks.lock().is_empty());
        drop(held_res.lock().take());

        // 2. Reservation acquired after private-check admission (before `prepare` scans)
        // must succeed immediately (scan is outside `restore_admission`), set the
        // registered cancellation flag via `cancel_private_checks`, and cause
        // `Run::private_check` itself to return `Err(RunError::Cancelled)` before any
        // outer liveness check runs.
        withdraw(&ds, &a);
        let state_ds = e.s.state.get("ev").unwrap();
        let eff_s: GraphragSettings =
            crate::settings::effective(&e.s.state, &state_ds, &crate::settings::GRAPHRAG);
        let direct_cancel = Arc::new(AtomicBool::new(false));
        let direct_cancel_hook = direct_cancel.clone();
        let direct_res: Arc<parking_lot::Mutex<Option<sparkles::catalog::Reservation>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let direct_res_hook = direct_res.clone();
        let direct_prepare_entered = Arc::new(AtomicBool::new(false));
        let direct_prepare_hook = direct_prepare_entered.clone();
        let st_direct_hook = e.s.state.clone();
        let _guard_direct = set_build_hook(move |point, name| {
            if point == BuildHookPoint::PrivatePrepare && name == "ev" {
                direct_prepare_hook.store(true, Ordering::SeqCst);
                assert!(
                    !direct_cancel_hook.load(Ordering::Acquire),
                    "private check cancel flag must be clear before reserve_restore"
                );
                // Must NOT deadlock: `restore_admission` has been released before `prepare`.
                let r = st_direct_hook
                    .reserve_restore("ev", "c25-during-private-prepare-direct")
                    .unwrap();
                assert!(
                    direct_cancel_hook.load(Ordering::Acquire),
                    "reserve_restore must set the registered private-check cancel flag"
                );
                *direct_res_hook.lock() = Some(r);
            }
        });
        let direct_run = Run {
            st: &e.s.state,
            ds: state_ds.clone(),
            settings: eff_s,
            mode: Mode::Refresh,
            confirmed: true,
            task: None,
            task_id: String::new(),
            cancel: direct_cancel.clone(),
            limits: Limits::default(),
        };
        let direct_outcome = direct_run.private_check();
        drop(_guard_direct);
        assert!(
            direct_prepare_entered.load(Ordering::SeqCst),
            "direct private_check must reach post-admission prepare hook"
        );
        assert!(
            direct_res.lock().is_some(),
            "reserve_restore must succeed without blocking on private_check preparation"
        );
        assert!(
            direct_cancel.load(Ordering::Acquire),
            "registered private-check cancel flag must remain set after reserve_restore"
        );
        assert!(
            matches!(direct_outcome, Err(RunError::Cancelled)),
            "Run::private_check must return Err(RunError::Cancelled) when cancelled by reserve_restore, got {direct_outcome:?}"
        );
        drop(direct_res.lock().take());

        // 3. End-to-end through `tick_once`: the same post-admission restore reservation
        // resolves as `NotLive`, drops the watch, and spawns no refresh task.
        let held_res2: Arc<parking_lot::Mutex<Option<sparkles::catalog::Reservation>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let prepare_entered2 = Arc::new(AtomicBool::new(false));
        let held_hook2 = held_res2.clone();
        let prepare_hook2 = prepare_entered2.clone();
        let st_hook2 = e.s.state.clone();
        let _guard2 = set_build_hook(move |point, name| {
            if point == BuildHookPoint::PrivatePrepare && name == "ev" {
                prepare_hook2.store(true, Ordering::SeqCst);
                let r = st_hook2
                    .reserve_restore("ev", "c25-during-private-prepare")
                    .unwrap();
                *held_hook2.lock() = Some(r);
            }
        });
        let mut watches2: HashMap<String, Watch> = HashMap::new();
        tick_once(&e.s.state, &mut watches2, now + Duration::from_secs(5));
        drop(_guard2);
        assert!(
            prepare_entered2.load(Ordering::SeqCst),
            "end-to-end tick_once must reach post-admission prepare hook"
        );
        assert!(
            held_res2.lock().is_some(),
            "reserve_restore must succeed without blocking on private_check preparation"
        );
        assert!(
            !watches2.contains_key("ev"),
            "cancelled private check under restore reservation must resolve as NotLive"
        );
        assert!(
            e.s.state.tasks.lock().is_empty(),
            "cancelled private check must not spawn a refresh task"
        );
        drop(held_res2.lock().take());
    })
    .await
    .unwrap();
}
