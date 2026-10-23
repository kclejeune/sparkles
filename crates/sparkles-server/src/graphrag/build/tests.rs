//! Runs of the build against stub model providers (C22 §3, §5, §11).

use super::*;
use crate::models::mock::{self, MockModel, Received};
use crate::models::{Models, ModelsConfig};
use crate::state::DbType;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sparkles::io::{RdfFormat, Source};
use tower::ServiceExt;

pub(crate) const ORG: &str = "urn:g:org";

/// Two clusters of `n` people each, every pair of a cluster knowing each other, with
/// one edge between the clusters and a few literals per person.
pub(crate) fn data(n: usize) -> String {
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
pub(crate) fn answer(r: &Received) -> String {
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

pub(crate) struct Fixture {
    pub(crate) st: Arc<AppState>,
    pub(crate) app: axum::Router,
    pub(crate) mock: MockModel,
}

pub(crate) fn fixture(n: usize) -> Fixture {
    fixture_with(
        n,
        |r, _| (200, mock::openai(&answer(r))),
        json!({ "provider": "p", "model": "m" }),
    )
}

/// A fixture whose provider answers with `handler`, and whose community role is `pair`.
pub(crate) fn fixture_with(
    n: usize,
    handler: impl Fn(&Received, usize) -> (u16, Value) + Send + Sync + 'static,
    pair: Value,
) -> Fixture {
    let mock = MockModel::start(handler);
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    st.models = arc_swap::ArcSwapOption::from_pointee(models_for(&mock.url(), &pair));
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

/// The model configuration of a fixture whose provider listens at `url`.
fn models_for(url: &str, pair: &Value) -> Models {
    let cfg = json!({
        "providers": { "p": { "kind": "openai", "endpoint": url, "requestTimeoutSecs": 5,
                              "allowedModels": ["m"], "structuredOutput": "json-schema" } },
        "roles": { "community": [pair] }
    });
    Models::new(
        ModelsConfig::parse(&cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    )
}

pub(crate) async fn patch(app: &axum::Router, uri: &str, body: Value) -> StatusCode {
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

pub(crate) fn settings(extra: Value) -> GraphragSettings {
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
    pub(crate) async fn allow_rows(&self) {
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

    pub(crate) fn run(&self, settings: GraphragSettings, mode: Mode) -> Result<Outcome, RunError> {
        let ds = self.st.datasets().get("org").cloned().unwrap();
        store_settings(&self.st, &ds, &settings);
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

    pub(crate) fn rows(&self, q: &str) -> Vec<Vec<String>> {
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

    pub(crate) fn reports(&self) -> Vec<Vec<String>> {
        self.rows(&format!(
            "SELECT ?c ?r ?s WHERE {{ GRAPH <urn:x-sparkles:graphrag:reports> {{ ?c a <{}> ; <{}> ?r . ?r <{}> ?s }} }}",
            super::super::guard::gr("Community"),
            super::super::guard::gr("report"),
            super::super::guard::gr("status"),
        ))
    }
}

/// Make `s` the dataset's live settings, which a run checks against its own.
pub(crate) fn store_settings(st: &AppState, ds: &Dataset, s: &GraphragSettings) {
    crate::settings::store_runtime(
        st,
        ds,
        &crate::settings::GRAPHRAG,
        &serde_json::to_value(s).unwrap(),
    )
    .unwrap();
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

/// Phase 3 and A4 with `refresh.mode: "auto"`: the loop starts no refresh while the
/// changes since the build stay below the fraction, starts one once they pass it after
/// the debounce, and its own commit of reports starts none.
#[tokio::test(flavor = "multi_thread")]
async fn an_automatic_refresh_follows_the_change_feed() {
    use crate::graphrag::refresh::{Watch, tick_once};
    let f = fixture(5);
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let s = settings(json!({
            "refresh": { "mode": "auto", "debounceSeconds": 1, "minChangedFraction": 0.1 }
        }));
        f.run(s, Mode::Build).unwrap();
        let before = f.reports();
        let ds = f.st.datasets().get("org").cloned().unwrap();
        let add = |k: usize| {
            let mut t = format!("<{ORG}> {{\n");
            for i in 0..k {
                t.push_str(&format!(
                    "<http://ex/x1> <http://ex/note> \"auto {k} {i}\" .\n"
                ));
            }
            t.push_str("}\n");
            ds.store
                .load(&[Source::from_bytes(t.into_bytes(), RdfFormat::TriG, None)])
                .unwrap();
        };
        let tasks = |st: &AppState| -> Vec<crate::state::Task> {
            st.tasks
                .lock()
                .iter()
                .filter(|t| t.kind == TASK_KIND)
                .cloned()
                .collect()
        };
        let wait = |st: &AppState| {
            let end = Instant::now() + Duration::from_secs(60);
            while st.active_task(TASK_KIND, "org").is_some() {
                assert!(Instant::now() < end, "the refresh did not finish");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let sec = Duration::from_secs(1);
        let mut w: HashMap<String, Watch> = HashMap::new();
        let mut now = Instant::now();
        // first sight waits a debounce, then finds nothing changed
        tick_once(&f.st, &mut w, now);
        now += sec;
        tick_once(&f.st, &mut w, now);
        assert!(tasks(&f.st).is_empty());
        // one fact is below the fraction
        add(1);
        now += sec;
        tick_once(&f.st, &mut w, now);
        now += sec;
        tick_once(&f.st, &mut w, now);
        assert!(tasks(&f.st).is_empty());
        let requests = f.mock.requests().len();
        // seven more pass it: not before the debounce, then a refresh
        add(7);
        now += sec;
        tick_once(&f.st, &mut w, now);
        assert!(tasks(&f.st).is_empty());
        now += sec;
        tick_once(&f.st, &mut w, now);
        let started = tasks(&f.st);
        assert_eq!(started.len(), 1, "{started:?}");
        wait(&f.st);
        let done = tasks(&f.st);
        assert_eq!(done[0].state, "done", "{:?}", done[0]);
        let msg = done[0].message.clone().unwrap_or_default();
        assert!(msg.starts_with("automatic refresh ("), "{msg}");
        assert!(f.mock.requests().len() > requests);
        let after = f.reports();
        assert_eq!(
            after.iter().filter(|r| !before.contains(r)).count(),
            1,
            "{before:?} {after:?}"
        );
        // the refresh's own commit changes no fact of the projection
        for _ in 0..3 {
            now += sec;
            tick_once(&f.st, &mut w, now);
        }
        assert_eq!(tasks(&f.st).len(), 1);
        // manual mode stops the loop
        let manual = settings(json!({ "refresh": { "mode": "manual" } }));
        store_settings(&f.st, &ds, &manual);
        add(20);
        for _ in 0..3 {
            now += sec;
            tick_once(&f.st, &mut w, now);
        }
        assert_eq!(tasks(&f.st).len(), 1);
    })
    .await
    .unwrap();
}

/// Phase 3, round 2: a pending retry whose refresh failed is tried again after the
/// backoff, on the same day and without a commit.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_pending_retry_runs_again_after_the_backoff() {
    use crate::graphrag::refresh::{Watch, tick_once};
    let f = fixture(4);
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        // every community is left pending by a budget that fits no request
        let s = settings(json!({
            "budget": { "tokensPerBuild": 10 },
            "refresh": { "mode": "auto", "debounceSeconds": 1 }
        }));
        let o = f.run(s, Mode::Build).unwrap();
        assert!(o.pending > 0, "{o:?}");
        let tasks = |st: &AppState| -> Vec<crate::state::Task> {
            st.tasks
                .lock()
                .iter()
                .filter(|t| t.kind == TASK_KIND)
                .cloned()
                .collect()
        };
        let wait = |st: &AppState| {
            let end = Instant::now() + Duration::from_secs(60);
            while st.active_task(TASK_KIND, "org").is_some() {
                assert!(Instant::now() < end, "the refresh did not finish");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let models = f.st.models.load_full();
        f.st.models.store(None);
        let sec = Duration::from_secs(1);
        let mut w: HashMap<String, Watch> = HashMap::new();
        let mut now = Instant::now();
        tick_once(&f.st, &mut w, now);
        now += sec;
        tick_once(&f.st, &mut w, now);
        assert_eq!(tasks(&f.st).len(), 1);
        wait(&f.st);
        assert_eq!(tasks(&f.st)[0].state, "failed", "{:?}", tasks(&f.st));
        // the next tick sees the failure; the backoff of two periods holds
        now += sec;
        tick_once(&f.st, &mut w, now);
        now += sec;
        tick_once(&f.st, &mut w, now);
        assert_eq!(tasks(&f.st).len(), 1);
        f.st.models.store(models);
        now += sec * 2;
        tick_once(&f.st, &mut w, now);
        let all = tasks(&f.st);
        assert_eq!(all.len(), 2, "{all:?}");
        wait(&f.st);
        let all = tasks(&f.st);
        assert!(
            all.iter().any(|t| t.state == "done"
                && t.message.as_deref().is_some_and(|m| m.contains("pending"))),
            "{all:?}"
        );
    })
    .await
    .unwrap();
}

/// Phase 3, round 1 finding 4: a dataset admits one GraphRAG task at a time, whichever
/// path starts it, so an automatic refresh cannot run beside a build waiting for its
/// confirmation.
#[tokio::test(flavor = "multi_thread")]
async fn one_graphrag_task_runs_per_dataset() {
    let f = fixture(3);
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let s = settings(json!({}));
        let ds = f.st.datasets().get("org").cloned().unwrap();
        store_settings(&f.st, &ds, &s);
        let first = spawn(&f.st, ds.clone(), s.clone(), Mode::Build, false, None).unwrap();
        let end = Instant::now() + Duration::from_secs(30);
        while !f.st.graphrag.is_waiting("org", &first.id) {
            assert!(
                Instant::now() < end,
                "the build never waited for confirmation"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let second = spawn(
            &f.st,
            ds.clone(),
            s,
            Mode::Refresh,
            true,
            Some("test".into()),
        );
        assert_eq!(second.unwrap_err(), first.id);
        assert!(f.st.graphrag.answer("org", &first.id, false));
        while f.st.active_task(TASK_KIND, "org").is_some() {
            assert!(Instant::now() < end, "the build did not end");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(f.mock.requests().is_empty());
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
            store_settings(&st2, &ds, &settings(json!({})));
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

fn gr(n: &str) -> String {
    super::super::guard::gr(n)
}

const REPORTS: &str = "urn:x-sparkles:graphrag:reports";

impl Fixture {
    fn ds(&self) -> Arc<Dataset> {
        self.st.datasets().get("org").cloned().unwrap()
    }

    fn load(&self, trig: String) {
        self.ds()
            .store
            .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
            .unwrap();
    }

    /// Add `k` new literal facts about `subject`.
    fn add_notes(&self, subject: &str, from: usize, k: usize) {
        let mut t = format!("<{ORG}> {{\n");
        for i in from..from + k {
            t.push_str(&format!("<{subject}> <http://ex/note> \"note {i}\" .\n"));
        }
        t.push_str("}\n");
        self.load(t);
    }

    /// Start a run that waits for its confirmation, and return it with its task id.
    fn waiting(
        &self,
        s: GraphragSettings,
        mode: Mode,
        id: &str,
    ) -> std::thread::JoinHandle<Result<Outcome, RunError>> {
        let st = self.st.clone();
        let id2 = id.to_string();
        store_settings(&st, &self.ds(), &s);
        let h = std::thread::spawn(move || {
            let ds = st.datasets().get("org").cloned().unwrap();
            run(&Run {
                st: &st,
                ds,
                settings: s,
                mode,
                confirmed: false,
                task: None,
                task_id: id2,
                cancel: Arc::new(AtomicBool::new(false)),
            })
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        // probe with a refusal that is never sent: wait until the run registered
        while !self.st.graphrag.is_waiting("org", id) {
            assert!(Instant::now() < deadline, "the run never waited");
            std::thread::sleep(Duration::from_millis(20));
        }
        h
    }
}

/// Every community links the footprint of the run that wrote its counts and members,
/// so a run that keeps every report keeps its own footprint (round 6, finding 1).
#[tokio::test(flavor = "multi_thread")]
async fn community_records_keep_the_footprint_that_read_them() {
    let f = fixture(5);
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let s = settings(json!({}));
        f.run(s.clone(), Mode::Build).unwrap();
        f.add_notes("http://ex/x1", 0, 1);
        let o = f.run(s, Mode::Refresh).unwrap();
        assert_eq!(o.written, 0, "{o:?}");
        let missing = f.rows(&format!(
            "SELECT ?c ?f WHERE {{ GRAPH <{REPORTS}> {{ ?c a <{}> ; <{}> ?f }} FILTER NOT EXISTS {{ GRAPH <{}> {{ ?f <{}> <{REPORTS}> }} }} }}",
            gr("Community"),
            gr("disclosure"),
            super::super::guard::DISCLOSURE_GRAPH,
            gr("covers"),
        ));
        assert!(missing.is_empty(), "{missing:?}");
        let linked = f.rows(&format!(
            "SELECT DISTINCT ?f WHERE {{ GRAPH <{REPORTS}> {{ ?c a <{}> ; <{}> ?f }} }}",
            gr("Community"),
            gr("disclosure"),
        ));
        assert_eq!(linked.len(), 1, "{linked:?}");
    })
    .await
    .unwrap();
}

/// Settings changed while a run waits for its confirmation refuse the run before it
/// sends anything (round 6, finding 5).
#[tokio::test(flavor = "multi_thread")]
async fn settings_changed_during_the_wait_refuse_the_run() {
    let f = fixture(3);
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let h = f.waiting(settings(json!({})), Mode::Build, "w1");
        store_settings(&f.st, &f.ds(), &settings(json!({ "language": "de" })));
        assert!(f.st.graphrag.answer("org", "w1", true));
        let r = h.join().unwrap();
        assert!(
            matches!(&r, Err(RunError::Refused(m)) if m.contains("settings changed")),
            "{r:?}"
        );
        assert!(f.mock.requests().is_empty());
    })
    .await
    .unwrap();
}

/// A model configuration replaced while a run waits refuses the run, even when its role
/// pairs stay the same, since a provider's endpoint or limits may have changed (round 7,
/// finding 3).
#[tokio::test(flavor = "multi_thread")]
async fn models_replaced_during_the_wait_refuse_the_run() {
    let f = fixture(3);
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let h = f.waiting(settings(json!({})), Mode::Build, "w1");
        let pair = json!({ "provider": "p", "model": "m" });
        f.st.models
            .store(Some(Arc::new(models_for(&f.mock.url(), &pair))));
        assert!(f.st.graphrag.answer("org", "w1", true));
        let r = h.join().unwrap();
        assert!(
            matches!(&r, Err(RunError::Refused(m)) if m.contains("model configuration changed")),
            "{r:?}"
        );
        assert!(f.mock.requests().is_empty());
    })
    .await
    .unwrap();
}

/// The live check runs before every request, so a retry after a failed attempt does not
/// send the facts once the configuration has changed (round 7, finding 2).
#[tokio::test(flavor = "multi_thread")]
async fn a_retry_rechecks_the_live_configuration() {
    let slot: Arc<std::sync::OnceLock<(Arc<AppState>, String)>> = Default::default();
    let slot2 = slot.clone();
    let f = fixture_with(
        3,
        move |r, i| {
            if i == 0 {
                // the administrator changes the configuration after the first attempt
                if let Some((st, url)) = slot2.get() {
                    let pair = json!({ "provider": "p", "model": "m" });
                    st.models.store(Some(Arc::new(models_for(url, &pair))));
                }
                return (503, json!({ "error": { "message": "busy" } }));
            }
            (200, mock::openai(&answer(r)))
        },
        json!({ "provider": "p", "model": "m" }),
    );
    slot.set((f.st.clone(), f.mock.url())).ok().unwrap();
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let r = f.run(settings(json!({})), Mode::Build);
        assert!(matches!(&r, Err(RunError::Refused(_))), "{r:?}");
        assert_eq!(f.mock.requests().len(), 1);
    })
    .await
    .unwrap();
}

/// A review that changes a report link while a refresh waits fails the refresh, which
/// would otherwise relink the old report (round 6, finding 7).
#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_does_not_undo_a_concurrent_edit() {
    let f = fixture(4);
    f.allow_rows().await;
    let f = tokio::task::spawn_blocking(move || {
        f.run(settings(json!({})), Mode::Build).unwrap();
        f
    })
    .await
    .unwrap();
    let rep = f.reports()[0][1].trim_matches(['<', '>']).to_string();
    let f = Arc::new(f);
    let f2 = f.clone();
    let h =
        tokio::task::spawn_blocking(move || f2.waiting(settings(json!({})), Mode::Refresh, "w2"))
            .await
            .unwrap();
    // a person edits the report while the refresh waits
    let res = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/$/graphrag/org/review")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "report": rep, "title": "Edited" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let f3 = f.clone();
    tokio::task::spawn_blocking(move || {
        assert!(f3.st.graphrag.answer("org", "w2", true));
        let r = h.join().unwrap();
        assert!(
            matches!(&r, Err(RunError::Failed(m)) if m.contains("changed during the run")),
            "{r:?}"
        );
        let titles = f3.rows(&format!(
            "SELECT ?t WHERE {{ GRAPH <{REPORTS}> {{ ?c <{}> ?r . ?r <{}> ?t }} }}",
            gr("report"),
            store::RDFS_LABEL
        ));
        assert!(titles.iter().any(|t| t[0].contains("Edited")), "{titles:?}");
    })
    .await
    .unwrap();
}

/// Small changes add up: a kept report keeps the baseline it was written from, so two
/// refreshes that each change less than the fraction rewrite it together (round 6,
/// finding 9).
#[tokio::test(flavor = "multi_thread")]
async fn small_changes_add_up_across_refreshes() {
    let f = fixture(5);
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let s = settings(json!({ "refresh": { "minChangedFraction": 0.1 } }));
        f.run(s.clone(), Mode::Build).unwrap();
        f.add_notes("http://ex/x1", 0, 1);
        assert_eq!(f.run(s.clone(), Mode::Refresh).unwrap().written, 0);
        f.add_notes("http://ex/x1", 1, 1);
        let o = f.run(s, Mode::Refresh).unwrap();
        assert_eq!(o.written, 1, "{o:?}");
    })
    .await
    .unwrap();
}

/// A refresh that finds no community removes the old ones (round 6, finding 10).
#[tokio::test(flavor = "multi_thread")]
async fn an_empty_hierarchy_removes_the_old_communities() {
    let f = fixture(3);
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let s = settings(json!({}));
        f.run(s.clone(), Mode::Build).unwrap();
        assert_eq!(f.reports().len(), 2);
        let ds = f.ds();
        sparql::update::update(
            &ds.store,
            &format!("CLEAR GRAPH <{ORG}>"),
            &QueryOptions::default(),
        )
        .unwrap();
        let o = f.run(s, Mode::Refresh).unwrap();
        assert_eq!(o.communities, 0, "{o:?}");
        let left = f.rows(&format!(
            "SELECT ?s WHERE {{ GRAPH <{REPORTS}> {{ ?s ?p ?o }} }}"
        ));
        assert!(left.is_empty(), "{left:?}");
    })
    .await
    .unwrap();
}

/// A cited fact that has a reifier is cited with its reifier too (round 6, finding 13).
#[tokio::test(flavor = "multi_thread")]
async fn cited_facts_carry_their_reifiers() {
    let f = fixture(3);
    f.allow_rows().await;
    // every fact of the two clusters gets a reifier
    let mut t = format!("<{ORG}> {{\n");
    for (k, line) in data(3)
        .lines()
        .filter(|l| l.starts_with("<http"))
        .enumerate()
    {
        let triple = line.trim_end_matches(" .");
        t.push_str(&format!(
            "<http://ex/rf{k}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> <<( {triple} )>> .\n"
        ));
    }
    t.push_str("}\n");
    f.load(t);
    tokio::task::spawn_blocking(move || {
        f.run(settings(json!({})), Mode::Build).unwrap();
        let cites = f.rows(&format!(
            "SELECT ?f ?x WHERE {{ GRAPH <{REPORTS}> {{ ?f <{}> ?x }} }}",
            gr("cites")
        ));
        let terms = cites.iter().filter(|c| c[1].starts_with("<<(")).count();
        let reifiers = cites
            .iter()
            .filter(|c| c[1].starts_with("<http://ex/rf"))
            .count();
        assert!(terms > 0 && terms == reifiers, "{cites:?}");
    })
    .await
    .unwrap();
}

/// The estimate counts the output limit the requests will carry: the pair's own
/// (round 6, finding 14).
#[tokio::test(flavor = "multi_thread")]
async fn the_estimate_uses_the_pairs_output_limit() {
    let f = fixture_with(
        3,
        |r, _| (200, mock::openai(&answer(r))),
        json!({ "provider": "p", "model": "m", "maxOutputTokens": 4096 }),
    );
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let ds = f.ds();
        let s = settings(json!({}));
        store_settings(&f.st, &ds, &s);
        let r = Run {
            st: &f.st,
            ds,
            settings: s,
            mode: Mode::Build,
            confirmed: true,
            task: None,
            task_id: "e".into(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let (models, pairs, p) = r.admit().unwrap();
        let prep = r.prepare(&p, &BuildState::default()).unwrap();
        let plans = r.plan(&prep, &BuildState::default());
        let e = r.estimate(&models, &pairs, &prep, &plans);
        assert_eq!(e["outputLimit"], 4096, "{e}");
    })
    .await
    .unwrap();
}

/// An accounting anomaly stops the run: the remaining communities stay pending rather
/// than failed, and a dataset without a directory keeps the anomaly (round 6, findings
/// 15 and 17).
#[tokio::test(flavor = "multi_thread")]
async fn an_anomaly_leaves_the_rest_pending() {
    let f = fixture_with(
        5,
        |r, _| {
            let mut v = mock::openai(&answer(r));
            v["usage"] = json!({ "prompt_tokens": 50_000_000, "completion_tokens": 5 });
            (200, v)
        },
        json!({ "provider": "p", "model": "m" }),
    );
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let o = f.run(settings(json!({})), Mode::Build).unwrap();
        assert_eq!(o.failed, 0, "{o:?}");
        assert!(o.pending >= 1, "{o:?}");
        assert!(
            o.stopped.as_deref().is_some_and(|m| m.contains("anomaly")),
            "{o:?}"
        );
        assert_eq!(f.mock.requests().len(), 1);
        assert_eq!(f.st.graphrag.anomalies("org").len(), 1);
        assert_eq!(f.st.graphrag.figures()["org"].anomalies, 1);
    })
    .await
    .unwrap();
}

/// The dataset's own secret patterns apply to generated reports (round 6, finding 16).
#[tokio::test(flavor = "multi_thread")]
async fn configured_secret_patterns_apply_to_reports() {
    let f = fixture_with(
        3,
        |r, _| {
            let text = answer(r).replace("People who know each other.", "Their key is ACME-12345.");
            (200, mock::openai(&text))
        },
        json!({ "provider": "p", "model": "m" }),
    );
    f.allow_rows().await;
    assert_eq!(
        patch(
            &f.app,
            "/$/settings/org/memory",
            json!({ "agentGraphs": ["urn:x-sparkles:import/*"], "imports": { "base": "urn:x-sparkles:import/", "secretPatterns": [{ "name": "acme", "regex": "ACME-[0-9]+" }] } })
        )
        .await,
        StatusCode::OK
    );
    tokio::task::spawn_blocking(move || {
        let o = f.run(settings(json!({})), Mode::Build).unwrap();
        assert_eq!(o.written, 0, "{o:?}");
        assert_eq!(o.failed, 2, "{o:?}");
    })
    .await
    .unwrap();
}

/// Every branch of a dataset spends the dataset's one daily ledger (round 6, finding 4).
#[tokio::test(flavor = "multi_thread")]
async fn branches_share_the_daily_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(3);
    f.allow_rows().await;
    let main =
        f.st.attach("disk", DbType::Persistent, Some(&dir.path().join("db")))
            .unwrap();
    main.store
        .load(&[Source::from_bytes(
            data(3).into_bytes(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    assert_eq!(
        patch(
            &f.app,
            "/$/settings/disk/assistant",
            json!({ "send": "rows" })
        )
        .await,
        StatusCode::OK
    );
    tokio::task::spawn_blocking(move || {
        main.store
            .create_branch("dev", &Default::default())
            .unwrap();
        let dev = f.st.branch_dataset(&main, "dev").unwrap();
        let s = settings(json!({}));
        store_settings(&f.st, &dev, &s);
        let o = run(&Run {
            st: &f.st,
            ds: dev.clone(),
            settings: s,
            mode: Mode::Build,
            confirmed: true,
            task: None,
            task_id: "b".into(),
            cancel: Arc::new(AtomicBool::new(false)),
        })
        .unwrap();
        assert!(o.tokens > 0, "{o:?}");
        let root = main.store.root().unwrap();
        let used = super::super::ledger::DatasetLedger::open(root)
            .unwrap()
            .day
            .lock()
            .used;
        assert_eq!(used, o.tokens);
        assert_ne!(dev.store.root(), main.store.root());
    })
    .await
    .unwrap();
}
