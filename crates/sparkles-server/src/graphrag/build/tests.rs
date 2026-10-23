//! Runs of the build against stub model providers (C22 §3, §5, §11).

use super::*;
use crate::models::mock::{self, MockModel, Received};
use crate::models::{Models, ModelsConfig};
use crate::state::DbType;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sparkles::io::{RdfFormat, Source};
use tower::ServiceExt;

const ORG: &str = "urn:g:org";

/// Two clusters of `n` people each, every pair of a cluster knowing each other, with
/// one edge between the clusters and a few literals per person.
fn data(n: usize) -> String {
    let mut t = format!("<{ORG}> {{\n");
    for c in ["x", "y"] {
        for i in 0..n {
            t.push_str(&format!(
                "<http://ex/{c}{i}> <http://www.w3.org/2000/01/rdf-schema#label> \"{c} person {i}\" .\n"
            ));
            for j in 0..n {
                if i < j {
                    t.push_str(&format!(
                        "<http://ex/{c}{i}> <http://ex/knows> <http://ex/{c}{j}> .\n"
                    ));
                }
            }
        }
    }
    t.push_str("<http://ex/x0> <http://ex/knows> <http://ex/y0> .\n}\n");
    t
}

/// The report a stub model writes: one finding that cites item 1 and the entity that
/// item names.
fn answer(r: &Received) -> String {
    let prompt = mock::prompt_of(r);
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
    json!({
        "title": "A community",
        "summary": "People who know each other.",
        "findings": [{ "text": "They know each other.", "entities": [entity], "cites": [1] }],
        "rating": 5
    })
    .to_string()
}

struct Fixture {
    st: Arc<AppState>,
    app: axum::Router,
    mock: MockModel,
}

fn fixture(n: usize) -> Fixture {
    let mock = MockModel::start(|r, _| (200, mock::openai(&answer(r))));
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    let cfg = json!({
        "providers": { "p": { "kind": "openai", "endpoint": mock.url(), "requestTimeoutSecs": 5,
                              "allowedModels": ["m"], "structuredOutput": "json-schema" } },
        "roles": { "community": [{ "provider": "p", "model": "m" }] }
    });
    st.models = arc_swap::ArcSwapOption::from_pointee(Models::new(
        ModelsConfig::parse(&cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    ));
    let st = Arc::new(st);
    let ds = st.attach("org", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            data(n).into_bytes(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let app = crate::http::router(st.clone());
    Fixture { st, app, mock }
}

async fn patch(app: &axum::Router, uri: &str, body: Value) -> StatusCode {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    res.status()
}

fn settings(extra: Value) -> GraphragSettings {
    let mut v = json!({
        "enabled": true,
        "principal": "reports",
        "projection": { "graphs": [ORG] },
        "levels": 1,
        "minCommunitySize": 2
    });
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        for (k, x) in e {
            o.insert(k.clone(), x.clone());
        }
    }
    serde_json::from_value(v).unwrap()
}

impl Fixture {
    async fn allow_rows(&self) {
        assert_eq!(
            patch(
                &self.app,
                "/$/settings/org/assistant",
                json!({ "send": "rows" })
            )
            .await,
            StatusCode::OK
        );
    }

    fn run(&self, settings: GraphragSettings, mode: Mode) -> Result<Outcome, RunError> {
        let ds = self.st.datasets().get("org").cloned().unwrap();
        run(&Run {
            st: &self.st,
            ds,
            settings,
            mode,
            confirmed: true,
            task: None,
            task_id: "t1".into(),
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    fn rows(&self, q: &str) -> Vec<Vec<String>> {
        let ds = self.st.datasets().get("org").cloned().unwrap();
        let mut v: Vec<Vec<String>> =
            sparql::query(ds.store.snapshot(), q, &QueryOptions::default())
                .unwrap()
                .rows()
                .into_iter()
                .map(|r| {
                    r.into_iter()
                        .map(|t| t.map(|t| t.to_string()).unwrap_or_default())
                        .collect()
                })
                .collect();
        v.sort();
        v
    }

    fn reports(&self) -> Vec<Vec<String>> {
        self.rows(&format!(
            "SELECT ?c ?r ?s WHERE {{ GRAPH <urn:x-sparkles:graphrag:reports> {{ ?c a <{}> ; <{}> ?r . ?r <{}> ?s }} }}",
            super::super::guard::gr("Community"),
            super::super::guard::gr("report"),
            super::super::guard::gr("status"),
        ))
    }
}

/// A1: two clusters make two communities at the finest level, each with a report whose
/// finding cites a fact of its own cluster.
#[tokio::test(flavor = "multi_thread")]
async fn two_clusters_make_two_reported_communities() {
    let f = fixture(5);
    f.allow_rows().await;
    let (o, f) = tokio::task::spawn_blocking(move || (f.run(settings(json!({})), Mode::Build), f))
        .await
        .unwrap();
    let f2 = &f;
    let o = o.unwrap();
    assert_eq!(o.stopped, None, "{o:?}");
    assert!(o.written >= 2, "{o:?}");
    let reports = f2.reports();
    assert_eq!(reports.len(), 2, "{reports:?}");
    for r in &reports {
        assert_eq!(r[2], "\"unreviewed\"");
    }
    // each finding cites a fact whose subject is a member of its community
    let bad = f2.rows(&format!(
        "SELECT ?c ?s WHERE {{ GRAPH <urn:x-sparkles:graphrag:reports> {{ ?c <{report}> ?r . ?r <{finding}> ?f . ?f <{cites}> ?t . FILTER(isTRIPLE(?t)) BIND(SUBJECT(?t) AS ?s) FILTER NOT EXISTS {{ ?c <{member}> ?s }} }} }}",
        report = super::super::guard::gr("report"),
        finding = super::super::guard::gr("finding"),
        cites = super::super::guard::gr("cites"),
        member = super::super::guard::gr("member"),
    ));
    assert!(bad.is_empty(), "{bad:?}");
    // the footprint is in the same data, and the commits are of kind graphrag
    let fps = f2.rows(&format!(
        "SELECT ?f WHERE {{ GRAPH <{}> {{ ?f <{}> ?j }} }}",
        super::super::guard::DISCLOSURE_GRAPH,
        super::super::guard::gr("footprint")
    ));
    assert_eq!(fps.len(), 1);
    let ds = f2.st.datasets().get("org").cloned().unwrap();
    let c = ds.store.commit(*o.commits.last().unwrap()).unwrap();
    assert_eq!(c.kind, CommitKind::Graphrag);
    assert_eq!(f.mock.requests().len(), 2, "one request per community");
}

/// A3 and A4: a refresh keeps a report whose facts changed by less than the fraction,
/// and rewrites one whose facts changed by more, superseding the old report.
#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_rewrites_only_changed_communities() {
    let f = fixture(5);
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let s = settings(json!({ "refresh": { "minChangedFraction": 0.1 } }));
        f.run(s.clone(), Mode::Build).unwrap();
        let before = f.reports();
        let ds = f.st.datasets().get("org").cloned().unwrap();
        let add = |k: usize| {
            let mut t = format!("<{ORG}> {{\n");
            for i in 0..k {
                t.push_str(&format!("<http://ex/x1> <http://ex/note> \"note {i}\" .\n"));
            }
            t.push_str("}\n");
            ds.store
                .load(&[Source::from_bytes(t.into_bytes(), RdfFormat::TriG, None)])
                .unwrap();
        };
        // cluster x has 5 labels and 10 edges inside: one more fact is below 10%
        add(1);
        let o = f.run(s.clone(), Mode::Refresh).unwrap();
        assert_eq!(o.written, 0, "{o:?}");
        assert_eq!(f.reports(), before);
        // six more facts pass it, and only cluster x's report changes
        add(7);
        let o = f.run(s, Mode::Refresh).unwrap();
        assert!(o.written >= 1 && o.kept >= 1, "{o:?}");
        let after = f.reports();
        let changed: Vec<_> = after.iter().filter(|r| !before.contains(r)).collect();
        assert_eq!(changed.len(), 1, "{before:?} {after:?}");
        let rev = f.rows(&format!(
            "SELECT ?old WHERE {{ GRAPH <urn:x-sparkles:graphrag:reports> {{ {} <http://www.w3.org/ns/prov#wasRevisionOf> ?old }} }}",
            changed[0][1]
        ));
        assert_eq!(rev.len(), 1);
    })
    .await
    .unwrap();
}

/// A8: a budget smaller than the first reservation sends no request and leaves every
/// community pending.
#[tokio::test(flavor = "multi_thread")]
async fn a_budget_that_does_not_fit_sends_nothing() {
    let f = fixture(4);
    f.allow_rows().await;
    let (o, f) = tokio::task::spawn_blocking(move || {
        let o = f
            .run(
                settings(json!({ "budget": { "tokensPerBuild": 10 } })),
                Mode::Build,
            )
            .unwrap();
        (o, f)
    })
    .await
    .unwrap();
    assert!(o.stopped.is_some(), "{o:?}");
    assert_eq!(o.written, 0);
    assert_eq!(o.pending, o.communities);
    assert!(f.mock.requests().is_empty());
}

/// A10: a provider limited to the schema cannot serve the community role.
#[tokio::test(flavor = "multi_thread")]
async fn schema_only_providers_refuse_the_build() {
    let f = fixture(3);
    let e = f.run(settings(json!({})), Mode::Build).unwrap_err();
    assert!(
        matches!(&e, RunError::Refused(m) if m.contains("send")),
        "{e}"
    );
    assert!(f.mock.requests().is_empty());
}

/// A run without a prior confirmation waits for one, and a refusal ends it.
#[tokio::test(flavor = "multi_thread")]
async fn runs_wait_for_confirmation() {
    let f = fixture(3);
    f.allow_rows().await;
    let st = f.st.clone();
    for confirm in [false, true] {
        let st2 = st.clone();
        let h = std::thread::spawn(move || {
            let ds = st2.datasets().get("org").cloned().unwrap();
            run(&Run {
                st: &st2,
                ds,
                settings: settings(json!({})),
                mode: Mode::Build,
                confirmed: false,
                task: None,
                task_id: format!("w{confirm}"),
                cancel: Arc::new(AtomicBool::new(false)),
            })
        });
        let id = format!("w{confirm}");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !st.graphrag.answer("org", &id, confirm) {
            assert!(Instant::now() < deadline, "the run never waited");
            std::thread::sleep(Duration::from_millis(20));
        }
        let r = h.join().unwrap();
        if confirm {
            assert!(r.is_ok(), "{r:?}");
        } else {
            assert!(matches!(r, Err(RunError::NotConfirmed)), "{r:?}");
        }
    }
}

/// Four clusters in two pairs: the coarser level's reports are written from their
/// children's reports and cite them.
#[tokio::test(flavor = "multi_thread")]
async fn parents_are_written_from_their_children() {
    let f = fixture(2);
    f.allow_rows().await;
    let ds = f.st.datasets().get("org").cloned().unwrap();
    let mut t = format!("<{ORG}> {{\n");
    for c in ["a", "b", "c", "d"] {
        for i in 0..5 {
            for j in (i + 1)..5 {
                t.push_str(&format!(
                    "<http://ex/{c}{i}> <http://ex/knows> <http://ex/{c}{j}> .\n"
                ));
            }
        }
    }
    for (x, y) in [("a", "b"), ("c", "d")] {
        for i in 0..3 {
            t.push_str(&format!(
                "<http://ex/{x}{i}> <http://ex/near> <http://ex/{y}{i}> .\n"
            ));
        }
    }
    t.push_str("<http://ex/b4> <http://ex/far> <http://ex/c4> .\n}\n");
    ds.store
        .load(&[Source::from_bytes(t.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();
    let (o, f) = tokio::task::spawn_blocking(move || {
        (
            f.run(
                settings(json!({ "levels": 3, "resolution": 0.5 })),
                Mode::Build,
            ),
            f,
        )
    })
    .await
    .unwrap();
    let o = o.unwrap();
    let levels = f.rows(&format!(
        "SELECT DISTINCT ?l WHERE {{ GRAPH <urn:x-sparkles:graphrag:reports> {{ ?c <{}> ?l }} }}",
        super::super::guard::gr("level")
    ));
    assert!(levels.len() >= 2, "{o:?} {levels:?}");
    // every parent's report cites a report of one of its children
    let parents = f.rows(&format!(
        "SELECT DISTINCT ?p WHERE {{ GRAPH <urn:x-sparkles:graphrag:reports> {{ ?c <{broader}> ?p }} }}",
        broader = store::SKOS_BROADER
    ));
    assert!(!parents.is_empty());
    let uncited = f.rows(&format!(
        "SELECT ?p WHERE {{ GRAPH <urn:x-sparkles:graphrag:reports> {{ ?c <{broader}> ?p . ?p <{report}> ?pr FILTER NOT EXISTS {{ ?pr <{cites}> ?cr . ?c2 <{broader}> ?p ; <{report}> ?cr }} }} }}",
        broader = store::SKOS_BROADER,
        report = super::super::guard::gr("report"),
        cites = super::super::guard::gr("citesReport"),
    ));
    assert!(uncited.is_empty(), "{uncited:?}");
}
