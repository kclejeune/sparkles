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
pub(crate) fn models_for(url: &str, pair: &Value) -> Models {
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
            limits: Default::default(),
        })
    }

    pub(crate) fn rows(&self, q: &str) -> Vec<Vec<String>> {
        let ds = self.st.datasets().get("org").cloned().unwrap();
        let mut v: Vec<Vec<String>> =
            sparql::query(ds.store.snapshot(), q, &QueryOptions::default())
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
                limits: Default::default(),
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
                limits: Default::default(),
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
            limits: Default::default(),
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

/// With prompt caching, every attempt reserves its input at the write price of the
/// provider's TTL, so the estimate counts the input at that price too, and an estimate
/// under the cap is never refused at its first reservation.
#[tokio::test(flavor = "multi_thread")]
async fn the_estimate_counts_the_cache_write_price() {
    let f = fixture_with(
        3,
        |r, _| (200, mock::openai(&answer(r))),
        json!({ "provider": "p", "model": "m" }),
    );
    f.allow_rows().await;
    let url = f.mock.url();
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
            limits: Default::default(),
        };
        let estimate = |caching: Value| {
            let cfg = json!({
                "providers": { "p": { "kind": "anthropic", "endpoint": url,
                    "allowedModels": ["m"], "structuredOutput": "json-schema",
                    "promptCaching": caching } },
                "roles": { "community": [{ "provider": "p", "model": "m" }] }
            });
            f.st.set_models(Some(Arc::new(Models::new(
                ModelsConfig::parse(&cfg.to_string()).unwrap(),
                Default::default(),
                sparkles::outbound::OutboundPolicy {
                    allow_private: true,
                    ..Default::default()
                },
            ))));
            let (models, pairs, p) = r.admit().unwrap();
            let prep = r.prepare(&p, &BuildState::default()).unwrap();
            let plans = r.plan(&prep, &BuildState::default());
            r.estimate(&models, &pairs, &prep, &plans)
        };
        let plain = estimate(json!({ "enabled": false }));
        let hour = estimate(json!({ "ttl": "1h" }));
        let n = |v: &Value, k: &str| v[k].as_u64().unwrap();
        let out = n(&plain, "outputLimit") * n(&plain, "requests");
        assert!(n(&plain, "requests") > 0, "{plain}");
        assert_eq!(
            n(&hour, "tokens"),
            2 * (n(&plain, "tokens") - out) + out,
            "{hour} {plain}"
        );
        let five = estimate(json!({}));
        assert!(n(&five, "tokens") > n(&plain, "tokens"), "{five}");
        assert!(n(&five, "tokens") < n(&hour, "tokens"), "{five}");
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
            limits: Default::default(),
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

/// A person's edit chain of any length leads back to the report it edits, and a chain
/// through a generated report or around a loop does not (item 652).
#[test]
fn edit_chains_of_any_length_lead_back() {
    let mut revs: HashMap<String, Revision> = HashMap::new();
    revs.insert(
        "urn:r:0".into(),
        Revision {
            prev: Some("urn:r:old".into()),
            generated: true,
            policy: None,
        },
    );
    for i in 1..=100 {
        revs.insert(
            format!("urn:r:{i}"),
            Revision {
                prev: Some(format!("urn:r:{}", i - 1)),
                generated: false,
                policy: None,
            },
        );
    }
    assert!(edit_of(&revs, "urn:r:100", "urn:r:0"));
    assert!(edit_of(&revs, "urn:r:64", "urn:r:0"));
    assert!(edit_of(&revs, "urn:r:0", "urn:r:0"));
    // a generated report does not lead back through what it replaced
    assert!(!edit_of(&revs, "urn:r:100", "urn:r:old"));
    // a loop ends
    revs.insert(
        "urn:r:a".into(),
        Revision {
            prev: Some("urn:r:b".into()),
            generated: false,
            policy: None,
        },
    );
    revs.insert(
        "urn:r:b".into(),
        Revision {
            prev: Some("urn:r:a".into()),
            generated: false,
            policy: None,
        },
    );
    assert!(!edit_of(&revs, "urn:r:a", "urn:r:0"));
}

/// The heap bytes a quad owns besides its slot, which the run's charges must bound.
fn owned(q: &oxrdf::Quad) -> u64 {
    let object = match &q.object {
        Term::NamedNode(n) => n.as_str().len(),
        Term::Literal(l) => {
            l.value().len() + l.datatype().as_str().len() + l.language().map_or(0, str::len)
        }
        t => t.to_string().len(),
    };
    let graph = match &q.graph_name {
        oxrdf::GraphName::NamedNode(n) => n.as_str().len(),
        _ => 0,
    };
    (q.subject.to_string().len() + q.predicate.as_str().len() + object + graph) as u64
}

fn long_quad(graph: &str, value: &str) -> oxrdf::Quad {
    oxrdf::Quad::new(
        oxrdf::NamedNode::new_unchecked("urn:s"),
        oxrdf::NamedNode::new_unchecked("urn:p"),
        oxrdf::Literal::new_simple_literal(value),
        oxrdf::GraphName::NamedNode(oxrdf::NamedNode::new_unchecked(graph)),
    )
}

#[test]
fn commit_charges_bound_the_whole_update() {
    let reports = format!("urn:g:{}", "r".repeat(5_000));
    let controls: String = (0u8..0x20).map(char::from).collect();
    let deleted: Vec<String> = (0..1000)
        .map(|i| format!("urn:uuid:00000000-0000-0000-0000-{i:012}"))
        .collect();
    let cases: Vec<(Vec<String>, Vec<oxrdf::Quad>)> = vec![
        // an empty hierarchy deletes many communities and writes almost nothing
        (deleted.clone(), Vec::new()),
        (deleted, vec![long_quad(&reports, &controls)]),
        (
            Vec::new(),
            vec![
                long_quad(&reports, &controls.repeat(50)),
                long_quad("urn:a/b", "x"),
                long_quad("urn:a", "y"),
                long_quad(&reports, "z"),
            ],
        ),
        (Vec::new(), Vec::new()),
    ];
    for (delete, quads) in cases {
        let bound = commit_bytes(&reports, &delete, &quads);
        let text = commit_text(&reports, &delete, &quads);
        assert_eq!(bound, text.len() as u64);
        // the buffer never grew past its first allocation
        assert_eq!(text.capacity(), text.len());
        // the text is the one the statements build on their own
        let mut parts: Vec<String> = Vec::new();
        parts.extend(store::delete_subjects(&reports, &delete));
        if !quads.is_empty() {
            let mut by_graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for q in &quads {
                by_graph
                    .entry(q.graph_name.to_string())
                    .or_default()
                    .push(format!("{} {} {} .", q.subject, q.predicate, q.object));
            }
            let mut body = String::new();
            for (g, ts) in by_graph {
                body.push_str(&format!("GRAPH {g} {{\n{}\n}}\n", ts.join("\n")));
            }
            parts.push(format!("INSERT DATA {{\n{body}}}"));
        }
        parts.push(store::delete_unreachable_reports(&reports));
        parts.push(store::delete_unlinked_footprints(&reports));
        assert_eq!(text, parts.join(" ;\n"));
    }
}

#[test]
fn report_charges_bound_what_the_report_builds() {
    use super::super::report::{check, render_leaf};
    use super::super::store::ReportEvidence;
    let graph = format!("urn:g:{}", "r".repeat(10_000));
    let slot = std::mem::size_of::<oxrdf::Quad>() as u64;
    let string = std::mem::size_of::<String>() as u64;
    let vec = std::mem::size_of::<Vec<(String, String)>>() as u64;
    // enough pairs that the report's vector of quads crosses capacity boundaries
    for n in [1usize, 3, 37, 400, 1300] {
        let members: BTreeSet<String> = ["<http://ex/a>", "<http://ex/b>"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let fact = |s: &str, o: &str| Fact {
            s: format!("<http://ex/{s}>"),
            p: "<http://ex/knows>".into(),
            o: o.into(),
            reifier: None,
        };
        // a cited literal of 1 MiB, whose escaped quote makes the parser's buffer grow
        let big = format!("\"{}\\\"\"", "v".repeat(1 << 20));
        let facts = vec![fact("a", "<http://ex/b>"), fact("b", &big)];
        let input = render_leaf(&members, &facts, &HashMap::<String, EntityInfo>::new());
        let findings: Vec<Value> = (0..12)
            .map(|i| json!({"text": format!("Finding {i}."), "entities": [1], "cites": [1, 2]}))
            .collect();
        let out = json!({"title": "T", "summary": "S", "rating": 3, "findings": findings});
        let checked = check(&out, &input, &|_| None).unwrap();
        let pairs: Vec<(String, String)> = (0..n)
            .map(|i| {
                (
                    format!("urn:claim:{i}:{}", "c".repeat(100)),
                    format!("urn:decision:{i}:{}", "d".repeat(100)),
                )
            })
            .collect();
        let half = (n / 2) as u32;
        // two items with pairs, and two child items with none
        let read = Read {
            items: vec![
                (0..half).collect(),
                (half..n as u32).collect(),
                Vec::new(),
                Vec::new(),
            ],
            own: (0..n as u32).collect(),
            children: vec![7],
            all: None,
        };
        let child = format!("urn:child:{}", "x".repeat(1000));
        let current: HashMap<usize, String> = [(7, child.clone())].into_iter().collect();
        let iri = "urn:uuid:00000000-0000-0000-0000-000000000001";
        let charged = report_bytes(
            |p| &pairs[p as usize],
            &read,
            &checked,
            &input,
            iri,
            &graph,
            &current,
        );
        let pair = |p: &u32| pairs[*p as usize].clone();
        let evidence = ReportEvidence {
            policy: "urn:policy".into(),
            version: 1,
            configuration: "l/1".into(),
            items: read
                .items
                .iter()
                .map(|x| x.iter().map(pair).collect())
                .collect(),
            used: read.own.iter().map(pair).collect(),
            children: vec![child],
        };
        let strings: u64 = evidence.items.len() as u64 * vec
            + evidence
                .items
                .iter()
                .flatten()
                .chain(&evidence.used)
                .map(|(c, d)| (c.len() + d.len()) as u64 + 2 * string)
                .sum::<u64>()
            + evidence
                .children
                .iter()
                .map(|c| c.len() as u64 + string)
                .sum::<u64>();
        let quads = report_quads(
            &NewReport {
                iri: iri.into(),
                community: "urn:c".into(),
                status: Status::Unreviewed,
                source_commit: 1,
                footprint: "urn:fp".into(),
                language: "en".into(),
                activity: Some(Activity {
                    model: "p/m".into(),
                    input_tokens: 1,
                    output_tokens: 1,
                    started_at: "2026-11-09T00:00:00Z".into(),
                }),
                revision_of: None,
                body: store::Body::Written { checked, input },
                evidence: Some(evidence),
            },
            &graph,
        );
        // the report's vector with its spare capacity, and the level's at twice the
        // report's length with the buffer it grew from
        let built: u64 = quads.capacity() as u64 * slot
            + 3 * quads.len() as u64 * slot
            + quads.iter().map(owned).sum::<u64>();
        assert!(
            charged >= strings + built,
            "{n}: {charged} < {strings} + {built}"
        );
    }
}

/// End-to-end automatic refresh on the internal F13 accumulator circuit:
/// - attaches after build and detaches when switched to manual or disabled,
/// - counts insertions, deletions and updates independently across two communities,
/// - rewrites only the community whose count crosses `minChangedFraction`,
/// - triggers refresh when an entity joins a community (in either direction),
/// - replaces the circuit cleanly when projection settings change.
#[tokio::test(flavor = "multi_thread")]
async fn circuit_refresh_handles_insertions_deletions_updates_and_joining_across_communities() {
    use crate::graphrag::refresh::{Watch, tick_once};
    let f = fixture(5);
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let s = settings(json!({
            "refresh": { "mode": "auto", "debounceSeconds": 1, "minChangedFraction": 0.15 }
        }));
        f.run(s, Mode::Build).unwrap();
        let ds = f.st.datasets().get("org").cloned().unwrap();
        assert_eq!(
            ds.dataset.inner_state().views.internal_count(),
            1,
            "build in auto mode attaches the internal refresh circuit"
        );
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
        let upd = |u: &str| {
            sparkles::sparql::update::update(&ds.store, u, &Default::default()).unwrap();
        };
        let sec = Duration::from_secs(1);
        let mut w: HashMap<String, Watch> = HashMap::new();
        let mut now = Instant::now();

        // First sight waits one debounce and starts nothing.
        tick_once(&f.st, &mut w, now);
        now += sec;
        tick_once(&f.st, &mut w, now);
        assert!(tasks(&f.st).is_empty());

        // Insertions + deletions + replacements in community x only; community y gets 1
        // small change below threshold.
        let before = f.reports();
        upd(&format!(
            "INSERT DATA {{ GRAPH <{ORG}> {{ <http://ex/x0> <http://ex/n> \"1\", \"2\", \"3\", \"4\" . \
             <http://ex/y0> <http://ex/n> \"small\" }} }}"
        ));
        upd(&format!(
            "DELETE DATA {{ GRAPH <{ORG}> {{ <http://ex/x0> <http://ex/n> \"1\" }} }}"
        ));
        upd(&format!(
            "DELETE {{ GRAPH <{ORG}> {{ <http://ex/x0> <http://ex/n> \"2\" }} }} \
             INSERT {{ GRAPH <{ORG}> {{ <http://ex/x0> <http://ex/n> \"2b\" }} }} WHERE {{}}"
        ));
        // 4 inserts + 1 delete + 2 (replace) = 7 gross changes (3 net facts out of 16 >= 15%)
        // on community x; community y has only 1 change (< 15%).
        now += sec;
        tick_once(&f.st, &mut w, now);
        now += sec;
        tick_once(&f.st, &mut w, now);
        assert_eq!(tasks(&f.st).len(), 1);
        wait(&f.st);
        assert_eq!(tasks(&f.st)[0].state, "done", "{:?}", tasks(&f.st)[0]);
        let after = f.reports();
        assert_eq!(
            after.iter().filter(|r| !before.contains(r)).count(),
            1,
            "only the changed community's report is rewritten"
        );

        // Next ticks see no further refresh because `after_build` discarded prior counts.
        for _ in 0..3 {
            now += sec;
            tick_once(&f.st, &mut w, now);
        }
        assert_eq!(tasks(&f.st).len(), 1);

        // An entity joining a community (`new -> member`) triggers refresh even on 1 edge.
        upd(&format!(
            "INSERT DATA {{ GRAPH <{ORG}> {{ <http://ex/newcomer> <http://ex/knows> <http://ex/y1> }} }}"
        ));
        now += sec;
        tick_once(&f.st, &mut w, now);
        now += sec;
        tick_once(&f.st, &mut w, now);
        assert_eq!(tasks(&f.st).len(), 2, "{:?}", tasks(&f.st));
        wait(&f.st);
        assert_eq!(tasks(&f.st)[1].state, "done", "{:?}", tasks(&f.st)[1]);
        assert!(
            tasks(&f.st)[1]
                .message
                .as_deref()
                .is_some_and(|m| m.contains("newcomer") && m.contains("joined")),
            "{:?}",
            tasks(&f.st)[1].message
        );

        // Changing projection settings replaces the circuit cleanly without conflict.
        let wider = settings(json!({
            "projection": { "graphs": [ORG, "urn:g:extra"] },
            "refresh": { "mode": "auto", "debounceSeconds": 1, "minChangedFraction": 0.15 }
        }));
        store_settings(&f.st, &ds, &wider);
        now += sec;
        tick_once(&f.st, &mut w, now);
        assert_eq!(ds.dataset.inner_state().views.internal_count(), 1);

        // Switching to manual detaches and collects the internal circuit.
        let manual = settings(json!({ "refresh": { "mode": "manual" } }));
        store_settings(&f.st, &ds, &manual);
        now += sec;
        tick_once(&f.st, &mut w, now);
        ds.dataset
            .inner_state()
            .views
            .settle(Duration::from_secs(10));
        assert_eq!(
            ds.dataset.inner_state().views.internal_count(),
            0,
            "manual mode detaches the internal circuit"
        );
    })
    .await
    .unwrap();
}

/// Persistent restart recovery: accumulator counts checkpointed on clean close are
/// reloaded on reopen, offline commits are replayed from the change log, and an
/// unbridgeable gap without a change log falls back to a refresh cleanly.
#[tokio::test(flavor = "multi_thread")]
async fn circuit_refresh_recovers_checkpoint_across_restart_and_handles_gap() {
    use crate::graphrag::refresh::{CIRCUIT_ID, Watch, tick_once};
    for change_log in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("org");
        let mock = MockModel::start(|r, _| (200, mock::openai(&answer(r))));
        let pair = json!({ "provider": "p", "model": "m" });
        let opts = sparkles::store::StoreOptions {
            change_log,
            history_max_generations: if change_log { 8 } else { 1 },
            ..Default::default()
        };
        let s = settings(json!({
            "refresh": { "mode": "auto", "debounceSeconds": 1, "minChangedFraction": 0.15 }
        }));
        let mock_url = mock.url();
        let db_clone = db.clone();
        let s_clone = s.clone();
        tokio::task::spawn_blocking(move || {
            let open_state = |mock_url: &str| {
                let mut st = AppState::standalone(opts.clone(), Duration::from_secs(30));
                st.models = arc_swap::ArcSwapOption::from_pointee(models_for(mock_url, &pair));
                let st = Arc::new(st);
                let ds = st.attach("org", DbType::Persistent, Some(&db_clone)).unwrap();
                (st, ds)
            };
            // Phase 1: initial build + 2 facts (below threshold), then clean close.
            {
                let (st, ds) = open_state(&mock_url);
                ds.store
                    .load(&[Source::from_bytes(
                        data(5).into_bytes(),
                        RdfFormat::TriG,
                        None,
                    )])
                    .unwrap();
                let mut assistant: crate::assistant::AssistantSettings =
                    crate::settings::effective(&st, &ds, &crate::settings::ASSISTANT);
                assistant.send = crate::assistant::Send::Rows;
                crate::settings::store_runtime(
                    &st,
                    &ds,
                    &crate::settings::ASSISTANT,
                    &serde_json::to_value(assistant).unwrap(),
                )
                .unwrap();
                store_settings(&st, &ds, &s_clone);
                run(&Run {
                    st: &st,
                    ds: ds.clone(),
                    settings: s_clone.clone(),
                    mode: Mode::Build,
                    confirmed: true,
                    task: None,
                    task_id: String::new(),
                    cancel: Default::default(),
                    limits: Default::default(),
                })
                .unwrap();
                // Add 2 facts (below 15% threshold of ~20 facts) and wait for circuit.
                sparkles::sparql::update::update(
                    &ds.store,
                    &format!(
                        "INSERT DATA {{ GRAPH <{ORG}> {{ <http://ex/x0> <http://ex/n> \"pre1\", \"pre2\" }} }}"
                    ),
                    &Default::default(),
                )
                .unwrap();
                let mut w = HashMap::new();
                let now = Instant::now();
                tick_once(&st, &mut w, now);
                tick_once(&st, &mut w, now + Duration::from_secs(2));
                assert!(
                    st.tasks
                        .lock()
                        .iter()
                        .all(|t| t.kind != TASK_KIND),
                    "2 facts stay below threshold before restart"
                );
                // Clean close writes the internal circuit checkpoint.
                st.graphrag.detach_circuit("org");
                ds.dataset.inner_state().views.close(&ds.store);
            }
            let ckpt_dir = db_clone.join("circuits").join(CIRCUIT_ID).join("main");
            assert!(
                ckpt_dir.exists()
                    && std::fs::read_dir(&ckpt_dir)
                        .unwrap()
                        .next()
                        .is_some(),
                "clean close wrote internal circuit checkpoint at {}",
                ckpt_dir.display()
            );

            // Phase 2: offline commit adds 4 more facts while server is stopped
            // (2 pre-restart + 4 offline = 6 >= 15% threshold). Without change_log,
            // compacting discards the old generation so the gap cannot be bridged.
            {
                let store = sparkles::store::Store::open(&db_clone, opts.clone()).unwrap();
                sparkles::sparql::update::update(
                    &store,
                    &format!(
                        "INSERT DATA {{ GRAPH <{ORG}> {{ <http://ex/x0> <http://ex/n> \"off1\", \"off2\", \"off3\", \"off4\" }} }}"
                    ),
                    &Default::default(),
                )
                .unwrap();
                if !change_log {
                    store.compact().unwrap();
                }
            }

            // Phase 3: reopen server and tick: loads checkpoint, replays or falls back,
            // and triggers the refresh cleanly.
            let (st, ds) = open_state(&mock_url);
            let mut w: HashMap<String, Watch> = HashMap::new();
            let now = Instant::now();
            tick_once(&st, &mut w, now);
            assert_eq!(
                ds.dataset.inner_state().views.stats(&ds.store).loaded,
                1,
                "change_log={change_log}: checkpoint must be loaded on reopen"
            );
            tick_once(&st, &mut w, now + Duration::from_secs(2));
            let started: Vec<_> = st
                .tasks
                .lock()
                .iter()
                .filter(|t| t.kind == TASK_KIND)
                .cloned()
                .collect();
            assert_eq!(
                started.len(),
                1,
                "change_log={change_log}: expected refresh after restart: {started:?}"
            );
            let end = Instant::now() + Duration::from_secs(60);
            while st.active_task(TASK_KIND, "org").is_some() {
                assert!(Instant::now() < end, "refresh did not finish");
                std::thread::sleep(Duration::from_millis(20));
            }
            let done = st
                .tasks
                .lock()
                .iter()
                .find(|t| t.kind == TASK_KIND)
                .cloned()
                .unwrap();
            assert_eq!(done.state, "done", "change_log={change_log}: {done:?}");
            let msg = done.message.unwrap_or_default();
            if change_log {
                assert!(
                    msg.contains("facts of <"),
                    "with change_log, exact count triggers refresh: {msg}"
                );
            } else {
                assert!(
                    msg.contains("cannot count every commit"),
                    "without change_log, unbridgeable gap triggers fallback refresh: {msg}"
                );
            }
        })
        .await
        .unwrap();
    }
}

/// Tight memory budget on the refresh accumulator keeps memory bounded, triggers a clean
/// fallback refresh on eviction, and resumes exact counting after refresh discards counts.
#[tokio::test(flavor = "multi_thread")]
async fn circuit_refresh_enforces_memory_bounds_and_recovers_after_eviction() {
    use crate::graphrag::refresh::{Watch, tick_once};
    let mock = MockModel::start(|r, _| (200, mock::openai(&answer(r))));
    let pair = json!({ "provider": "p", "model": "m" });
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    st.models = arc_swap::ArcSwapOption::from_pointee(models_for(&mock.url(), &pair));
    st.views_options.views_memory = Some(16 << 10);
    let st = Arc::new(st);
    let ds = st.attach("org", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            data(5).into_bytes(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let app = crate::http::router(st.clone());
    let f = Fixture { st, app, mock };
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let s = settings(json!({
            "refresh": { "mode": "auto", "debounceSeconds": 1, "minChangedFraction": 0.5 }
        }));
        f.run(s, Mode::Build).unwrap();
        let ds = f.st.datasets().get("org").cloned().unwrap();
        // Push many distinct large subjects (unrelated to stored communities) in separate
        // commits so the 16 KiB tally cap evicts earlier commits and records `unknown`.
        for i in 0..80 {
            let big = "k".repeat(300);
            sparkles::sparql::update::update(
                &ds.store,
                &format!(
                    "INSERT DATA {{ GRAPH <{ORG}> {{ <http://ex/big/{big}/{i}> <http://ex/n> \"v\" }} }}"
                ),
                &Default::default(),
            )
            .unwrap();
        }
        let sec = Duration::from_secs(1);
        let mut w: HashMap<String, Watch> = HashMap::new();
        let mut now = Instant::now();
        tick_once(&f.st, &mut w, now);
        now += sec;
        tick_once(&f.st, &mut w, now);
        let started: Vec<_> = f
            .st
            .tasks
            .lock()
            .iter()
            .filter(|t| t.kind == TASK_KIND)
            .cloned()
            .collect();
        assert_eq!(started.len(), 1, "eviction triggers fallback refresh: {started:?}");
        let end = Instant::now() + Duration::from_secs(60);
        while f.st.active_task(TASK_KIND, "org").is_some() {
            assert!(Instant::now() < end, "refresh did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
        let done = f
            .st
            .tasks
            .lock()
            .iter()
            .find(|t| t.kind == TASK_KIND)
            .cloned()
            .unwrap();
        assert_eq!(done.state, "done", "{done:?}");
        assert!(
            done.message
                .as_deref()
                .is_some_and(|m| m.contains("cannot count every commit")),
            "{:?}",
            done.message
        );
        // After refresh discards counts, a small change is counted cleanly and stays below 50%.
        sparkles::sparql::update::update(
            &ds.store,
            &format!("INSERT DATA {{ GRAPH <{ORG}> {{ <http://ex/x0> <http://ex/n> \"tiny\" }} }}"),
            &Default::default(),
        )
        .unwrap();
        for _ in 0..3 {
            now += sec;
            tick_once(&f.st, &mut w, now);
        }
        let all: Vec<_> = f
            .st
            .tasks
            .lock()
            .iter()
            .filter(|t| t.kind == TASK_KIND)
            .cloned()
            .collect();
        assert_eq!(
            all.len(),
            1,
            "exact counting resumes after refresh discards evicted range: {all:?}"
        );
    })
    .await
    .unwrap();
}

/// Deleting a dataset detaches its internal refresh circuit cleanly without pinning it.
#[tokio::test(flavor = "multi_thread")]
async fn circuit_refresh_detaches_on_dataset_delete() {
    let f = fixture(4);
    f.allow_rows().await;
    tokio::task::spawn_blocking(move || {
        let s = settings(json!({
            "refresh": { "mode": "auto", "debounceSeconds": 1 }
        }));
        f.run(s, Mode::Build).unwrap();
        let ds = f.st.datasets().get("org").cloned().unwrap();
        assert!(f.st.graphrag.has_refresh_circuit("org", &ds));
        drop(ds);
        assert!(f.st.delete("org").unwrap());
    })
    .await
    .unwrap();
}
