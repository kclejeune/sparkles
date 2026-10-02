//! Tests of the policy routes, runs and scheduler, with a fake engine (no repository)
//! and a clock the tests move.

use super::*;
use crate::backup::registry::RepoEntry;
use crate::backup::scheduler::{self, Scheduler};
use axum::body::Body;
use axum::http::Request;
use serde_json::{Value as J, json};
use sparkles::store::StoreOptions;
use sparkles_backup::{CommitRef, DatasetRef, RepoConfig};
use std::time::Duration;
use tower::ServiceExt;

fn t(s: &str) -> DateTime<Utc> {
    parse_time(s).unwrap()
}

/// A clock that only moves when the test says so. `sleep` blocks until the test
/// advances the time past it ([`FakeClock::wait_sleep`], [`FakeClock::advance`]).
struct FakeClock {
    now: Mutex<DateTime<Utc>>,
    pending: Mutex<Option<Duration>>,
    cv: parking_lot::Condvar,
}

impl FakeClock {
    fn at(s: &str) -> Arc<FakeClock> {
        Arc::new(FakeClock {
            now: Mutex::new(t(s)),
            pending: Mutex::new(None),
            cv: parking_lot::Condvar::new(),
        })
    }

    fn set(&self, s: &str) {
        *self.now.lock() = t(s);
    }

    /// The duration the scheduler thread sleeps for, once it does.
    fn wait_sleep(&self) -> Duration {
        let mut p = self.pending.lock();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(d) = *p {
                return d;
            }
            assert!(
                !self.cv.wait_until(&mut p, deadline).timed_out(),
                "the scheduler never slept"
            );
        }
    }

    /// Move the time forward by `d` and wake the sleeper.
    fn advance(&self, d: Duration) {
        *self.now.lock() += TimeDelta::from_std(d).unwrap();
        *self.pending.lock() = None;
        self.cv.notify_all();
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock()
    }

    fn sleep(&self, d: Duration) {
        let mut p = self.pending.lock();
        *p = Some(d);
        self.cv.notify_all();
        while p.is_some() {
            self.cv.wait(&mut p);
        }
    }
}

/// One repository in memory, and two datasets (`ds` persistent, `mem1` in memory, which
/// policies back up like any other).
struct Fake {
    clock: Arc<FakeClock>,
    datasets: Mutex<Vec<DatasetInfo>>,
    backups: Mutex<Vec<BackupSummary>>,
    /// datasets whose backups fail
    failing: Mutex<HashSet<String>>,
    busy: Mutex<HashSet<String>>,
    gcs: Mutex<Vec<String>>,
    /// while true, backups wait
    hold: Mutex<bool>,
    released: parking_lot::Condvar,
    /// backups started
    entered: Mutex<usize>,
}

const DS_ID: u128 = 0x3f1c_9a2e_0000_4000_8000_0000_0000_0003;

impl Fake {
    fn new(clock: Arc<FakeClock>) -> Arc<Fake> {
        Arc::new(Fake {
            clock,
            datasets: Mutex::new(vec![
                DatasetInfo {
                    name: "ds".into(),
                    id: Uuid::from_u128(DS_ID),
                    head: 7,
                },
                DatasetInfo {
                    name: "mem1".into(),
                    id: Uuid::from_u128(9),
                    head: 1,
                },
            ]),
            backups: Mutex::default(),
            failing: Mutex::default(),
            busy: Mutex::default(),
            gcs: Mutex::default(),
            hold: Mutex::new(false),
            released: parking_lot::Condvar::new(),
            entered: Mutex::new(0),
        })
    }

    fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.backups.lock().iter().map(|b| b.name.clone()).collect();
        v.sort();
        v
    }

    /// Wait until a backup has started (and waits, while held).
    fn wait_entered(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while *self.entered.lock() == 0 {
            assert!(std::time::Instant::now() < deadline, "no backup started");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn release(&self) {
        *self.hold.lock() = false;
        self.released.notify_all();
    }
}

fn summary(
    name: &str,
    dataset: &str,
    id: Uuid,
    seq: u64,
    policy: Option<&str>,
    completed: DateTime<Utc>,
) -> BackupSummary {
    BackupSummary {
        name: name.into(),
        repository: "local".into(),
        dataset: DatasetRef {
            name: dataset.into(),
            id,
            kind: "persistent".into(),
        },
        commit: CommitRef {
            seq,
            timestamp: fmt_time(completed),
            quads: 3,
            reference: format!("commit:{seq}"),
        },
        created: fmt_time(completed),
        completed: fmt_time(completed),
        millis: 5,
        logical_bytes: 300,
        added_bytes: 70,
        policy: policy.map(str::to_string),
        run: None,
        note: None,
        same_lineage: None,
        verified: None,
    }
}

impl Engine for Fake {
    fn datasets(&self, _: &Arc<AppState>) -> Vec<DatasetInfo> {
        self.datasets.lock().clone()
    }

    fn list(
        &self,
        _: &Arc<AppState>,
        repo: &str,
        policy: &str,
    ) -> Result<Vec<BackupSummary>, BackupError> {
        assert_eq!(repo, "local");
        Ok(self
            .backups
            .lock()
            .iter()
            .filter(|b| b.policy.as_deref() == Some(policy))
            .cloned()
            .collect())
    }

    fn create(
        &self,
        _: &Arc<AppState>,
        repo: &str,
        dataset: &str,
        o: CreateOptions,
    ) -> Result<BackupSummary, BackupError> {
        assert_eq!(repo, "local");
        *self.entered.lock() += 1;
        {
            let mut h = self.hold.lock();
            while *h {
                self.released.wait(&mut h);
            }
        }
        if self.failing.lock().contains(dataset) {
            return Err(BackupError::new(
                Code::RepositoryUnavailable,
                "the bucket is on fire",
            ));
        }
        let mut backups = self.backups.lock();
        if backups.iter().any(|b| b.name == o.name) {
            return Err(BackupError::new(Code::BackupExists, "exists"));
        }
        let ds = self
            .datasets
            .lock()
            .iter()
            .find(|d| d.name == dataset)
            .cloned()
            .unwrap();
        let mut s = summary(
            &o.name,
            dataset,
            ds.id,
            ds.head,
            o.policy.as_ref().map(|p| p.0.as_str()),
            self.clock.now(),
        );
        s.run = o.policy.map(|p| p.1);
        backups.push(s.clone());
        Ok(s)
    }

    fn delete(&self, _: &Arc<AppState>, _: &str, name: &str) -> Result<bool, BackupError> {
        let mut b = self.backups.lock();
        let n = b.len();
        b.retain(|x| x.name != name);
        Ok(b.len() < n)
    }

    fn busy(&self, _: &Arc<AppState>, _: &str) -> HashSet<String> {
        self.busy.lock().clone()
    }

    fn start_gc(&self, _: &Arc<AppState>, repo: &str) -> Result<String, BackupError> {
        let mut g = self.gcs.lock();
        g.push(repo.to_string());
        Ok(format!("gc-{}", g.len()))
    }
}

struct T {
    dir: tempfile::TempDir,
    st: Arc<AppState>,
    app: Router,
    clock: Arc<FakeClock>,
    fake: Arc<Fake>,
}

impl T {
    fn b(&self) -> &Arc<BackupState> {
        self.st.backup.as_ref().unwrap()
    }

    fn backup_dir(&self) -> PathBuf {
        self.dir.path().join("backup")
    }

    async fn call(&self, method: &str, uri: &str, body: Option<J>) -> (StatusCode, J) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(match body {
                Some(b) => Body::from(b.to_string()),
                None => Body::empty(),
            })
            .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let j = if bytes.is_empty() {
            J::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, j)
    }

    /// Wait for task `id` to end; its state.
    fn wait(&self, id: &str) -> Task {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let task = self
                .st
                .tasks
                .lock()
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .unwrap();
            let target = task.target.clone().unwrap_or_default();
            if !task.active() && self.b().policies.running_task(&target).as_deref() != Some(id) {
                return task;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "task {id} hangs: {task:?} running {:?}",
                self.b().policies.running_task(&target)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Wait for the latest task of policy `p` to end.
    fn wait_policy(&self, p: &str) -> Task {
        let id = self
            .st
            .tasks
            .lock()
            .iter()
            .rev()
            .find(|t| t.kind == "backup-policy" && t.target.as_deref() == Some(p))
            .map(|t| t.id.clone())
            .expect("a policy task");
        self.wait(&id)
    }

    fn runs(&self, p: &str) -> Vec<PolicyRun> {
        self.b().policies.runs(p, 100)
    }
}

/// A server with the backup state (repositories `local`, and `archive` read-only), a
/// fake engine and a fake clock at `now`. Files under `<dir>/backup` written before
/// (`files`) are loaded as at a restart.
fn setup_with(now: &str, dir: tempfile::TempDir, read_only: bool) -> T {
    let mut state =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    state.read_only = read_only;
    let b = BackupState::new(dir.path(), None, 2).unwrap();
    for (name, readonly) in [("local", false), ("archive", true)] {
        b.registry.repos.write().insert(
            name.into(),
            RepoEntry::new(
                RepoConfig {
                    name: name.into(),
                    path: Some(format!("/srv/{name}")),
                    readonly,
                    ..Default::default()
                },
                ConfigSource::Api,
            ),
        );
    }
    let clock = FakeClock::at(now);
    let fake = Fake::new(clock.clone());
    b.policies.set_clock(clock.clone());
    b.policies.set_engine(fake.clone());
    state.backup = Some(Arc::new(b));
    let st = Arc::new(state);
    let app = crate::http::router(st.clone());
    T {
        dir,
        st,
        app,
        clock,
        fake,
    }
}

fn setup(now: &str) -> T {
    setup_with(now, tempfile::tempdir().unwrap(), false)
}

fn nightly() -> J {
    json!({
        "name": "nightly", "repository": "local", "schedule": "30 2 * * *",
        "timezone": "Europe/Berlin", "nameTemplate": "{policy}-{dataset}-{date:%Y%m%d}",
        "retention": {"expireAfter": "30d", "minCount": 7, "maxCount": 60},
        "gcAfterRetention": true
    })
}

#[tokio::test]
async fn policies_are_created_shown_changed_and_removed() {
    let s = setup("2026-09-30T12:05:00Z");
    let (st, j) = s.call("POST", "/$/backup-policies", Some(nightly())).await;
    assert_eq!(st, StatusCode::CREATED, "{j}");
    assert_eq!(j["name"], "nightly");
    assert_eq!(j["source"], "api");
    assert_eq!(j["datasets"], json!(["*"]));
    assert_eq!(j["catchUp"], "one");
    // 02:30 CEST on 2026-10-01
    assert_eq!(j["state"]["nextRun"], "2026-10-01T00:30:00.000Z");
    assert_eq!(j["state"]["lastScheduledFor"], "2026-09-30T00:30:00.000Z");
    assert!(j["state"]["lastRun"].is_null() && j["state"]["runningTask"].is_null());
    // persisted
    let saved = read_api_policies(&s.backup_dir()).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].retention.min_count, 7);
    assert!(s.backup_dir().join("policy-state.json").exists());

    let (st, j) = s.call("GET", "/$/backup-policies", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(j["policies"][0]["name"], "nightly");
    let (st, j) = s.call("GET", "/$/backup-policies/nightly", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(j["retention"]["maxCount"], 60);

    // PUT takes the whole policy; the name may be left out but cannot change
    let mut changed = nightly();
    changed.as_object_mut().unwrap().remove("name");
    changed["schedule"] = "every 6h".into();
    changed["enabled"] = false.into();
    let (st, j) = s
        .call("PUT", "/$/backup-policies/nightly", Some(changed.clone()))
        .await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(j["schedule"], "every 6h");
    assert!(j["state"]["nextRun"].is_null(), "disabled: {j}");
    assert_eq!(j["state"]["lastScheduledFor"], "2026-09-30T12:00:00.000Z");
    changed["name"] = "other".into();
    let (st, j) = s
        .call("PUT", "/$/backup-policies/nightly", Some(changed))
        .await;
    assert_eq!(
        (st, j["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid-name"))
    );
    assert_eq!(
        read_api_policies(&s.backup_dir()).unwrap()[0].schedule,
        "every 6h"
    );

    let (st, _) = s.call("DELETE", "/$/backup-policies/nightly", None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, j) = s.call("GET", "/$/backup-policies/nightly", None).await;
    assert_eq!(
        (st, j["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("no-such-policy"))
    );
    assert!(read_api_policies(&s.backup_dir()).unwrap().is_empty());
    for (m, path) in [
        ("PUT", "/$/backup-policies/nope"),
        ("DELETE", "/$/backup-policies/nope"),
        ("POST", "/$/backup-policies/nope/run"),
        ("POST", "/$/backup-policies/nope/retention"),
        ("GET", "/$/backup-policies/nope/runs"),
    ] {
        let (st, j) = s.call(m, path, Some(nightly())).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{m} {path}");
        assert_eq!(j["code"], "no-such-policy", "{m} {path}");
    }
}

#[tokio::test]
async fn bad_policies_are_refused() {
    let s = setup("2026-09-30T12:05:00Z");
    let cases: Vec<(J, StatusCode, &str)> = vec![
        (
            json!({"name": "preview"}),
            StatusCode::BAD_REQUEST,
            "invalid-name",
        ),
        (
            json!({"name": "Nightly"}),
            StatusCode::BAD_REQUEST,
            "invalid-name",
        ),
        (
            json!({"schedule": "tonight"}),
            StatusCode::BAD_REQUEST,
            "invalid-schedule",
        ),
        (
            json!({"schedule": "every 10s"}),
            StatusCode::BAD_REQUEST,
            "invalid-schedule",
        ),
        (
            json!({"timezone": "Mars/Base"}),
            StatusCode::BAD_REQUEST,
            "invalid-schedule",
        ),
        (
            json!({"nameTemplate": "{policy}/{dataset}"}),
            StatusCode::BAD_REQUEST,
            "invalid-config",
        ),
        (
            json!({"nameTemplate": "{when}"}),
            StatusCode::BAD_REQUEST,
            "invalid-config",
        ),
        (
            json!({"retention": {"expireAfter": "a while"}}),
            StatusCode::BAD_REQUEST,
            "invalid-config",
        ),
        (
            json!({"retention": {"maxCount": 0}}),
            StatusCode::BAD_REQUEST,
            "invalid-config",
        ),
        (
            json!({"repository": "nowhere"}),
            StatusCode::NOT_FOUND,
            "no-such-repository",
        ),
        (
            json!({"repository": "archive"}),
            StatusCode::CONFLICT,
            "repository-read-only",
        ),
        (
            json!({"name": "local"}),
            StatusCode::CONFLICT,
            "policy-exists",
        ),
        (
            json!({"retention": "forever"}),
            StatusCode::BAD_REQUEST,
            "invalid-request",
        ),
    ];
    for (patch, status, code) in cases {
        let mut p = nightly();
        for (k, v) in patch.as_object().unwrap() {
            p[k] = v.clone();
        }
        let (st, j) = s.call("POST", "/$/backup-policies", Some(p.clone())).await;
        assert_eq!((st, j["code"].as_str()), (status, Some(code)), "{p}: {j}");
    }
    let (st, _) = s.call("POST", "/$/backup-policies", Some(nightly())).await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, j) = s.call("POST", "/$/backup-policies", Some(nightly())).await;
    assert_eq!(
        (st, j["code"].as_str()),
        (StatusCode::CONFLICT, Some("policy-exists"))
    );
}

#[tokio::test]
async fn config_file_policies_are_read_only() {
    let s = setup("2026-09-30T12:05:00Z");
    let config: PolicyConfig = serde_json::from_value(nightly()).unwrap();
    s.b().registry.policies.write().insert(
        "nightly".into(),
        PolicyEntry {
            config,
            source: ConfigSource::Config,
        },
    );
    let (st, j) = s.call("GET", "/$/backup-policies/nightly", None).await;
    assert_eq!((st, j["source"].as_str()), (StatusCode::OK, Some("config")));
    for m in ["PUT", "DELETE"] {
        let (st, j) = s
            .call(m, "/$/backup-policies/nightly", Some(nightly()))
            .await;
        assert_eq!(
            (st, j["code"].as_str()),
            (StatusCode::CONFLICT, Some("read-only-config")),
            "{m}"
        );
    }
    // and never written to policies.json
    s.call(
        "POST",
        "/$/backup-policies",
        Some(json!({"name": "hourly", "repository": "local", "schedule": "0 * * * *"})),
    )
    .await;
    let saved = read_api_policies(&s.backup_dir()).unwrap();
    assert_eq!(
        saved.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["hourly"]
    );
}

#[tokio::test]
async fn read_only_servers_refuse_policy_changes() {
    let s = setup_with("2026-09-30T12:05:00Z", tempfile::tempdir().unwrap(), true);
    let (st, j) = s.call("POST", "/$/backup-policies", Some(nightly())).await;
    assert_eq!(
        (st, j["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("server-read-only"))
    );
    // previews still work
    let (st, _) = s
        .call(
            "POST",
            "/$/backup-policies/preview",
            Some(json!({"schedule": "every 6h"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
}

// Time zones and DST in the preview over HTTP, and the preview's sample and errors
#[tokio::test]
async fn preview() {
    let s = setup("2026-10-24T00:00:00Z");
    let (st, j) = s
        .call(
            "POST",
            "/$/backup-policies/preview",
            Some(json!({"schedule": "30 2 * * *", "timezone": "Europe/Berlin", "count": 3,
                        "nameTemplate": "{policy}-{dataset}-{date:%Y%m%d-%H%M}", "dataset": "foaf"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(
        j["next"],
        json!([
            "2026-10-24T00:30:00.000Z",
            "2026-10-25T00:30:00.000Z",
            "2026-10-26T01:30:00.000Z"
        ])
    );
    assert_eq!(j["description"], "Every day at 02:30 (Europe/Berlin)");
    assert_eq!(j["sample"], "policy-foaf-20261024-0230");
    s.clock.set("2027-03-27T12:00:00Z");
    let (_, j) = s
        .call(
            "POST",
            "/$/backup-policies/preview",
            Some(json!({"schedule": "30 2 * * *", "timezone": "Europe/Berlin", "count": 2})),
        )
        .await;
    assert_eq!(
        j["next"],
        json!(["2027-03-28T01:00:00.000Z", "2027-03-29T00:30:00.000Z"])
    );
    assert!(j.get("sample").is_none());
    s.clock.set("2026-10-24T13:47:12Z");
    let (_, j) = s
        .call(
            "POST",
            "/$/backup-policies/preview",
            Some(json!({"schedule": "every 6h", "timezone": "America/New_York"})),
        )
        .await;
    assert_eq!(j["next"].as_array().unwrap().len(), 5);
    assert_eq!(j["next"][0], "2026-10-24T18:00:00.000Z");
    assert_eq!(j["next"][1], "2026-10-25T00:00:00.000Z");
    assert_eq!(j["description"], "Every 6 hours, counted from 00:00 UTC");
    for (body, code) in [
        (json!({"schedule": "30 2 * *"}), "invalid-schedule"),
        (
            json!({"schedule": "30 2 * * *", "timezone": "Europe/Atlantis"}),
            "invalid-schedule",
        ),
        (
            json!({"schedule": "30 2 * * *", "nameTemplate": "{nope}"}),
            "invalid-config",
        ),
        (
            json!({"schedule": "30 2 * * *", "nameTemplate": "a b"}),
            "invalid-config",
        ),
        (json!({"timezone": "UTC"}), "invalid-request"),
    ] {
        let (st, j) = s
            .call("POST", "/$/backup-policies/preview", Some(body.clone()))
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(j["code"], code, "{body}: {j}");
    }
}

// `*/10` in UTC; at 12:05 nextRun is 12:10; at 12:10 a scheduled run; a manual run
// at 12:12 does not move the schedule
#[tokio::test]
async fn scheduled_and_manual_runs() {
    let s = setup("2026-09-30T12:05:00Z");
    let (st, j) = s
        .call(
            "POST",
            "/$/backup-policies",
            Some(json!({"name": "p", "repository": "local", "schedule": "*/10 * * * *"})),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{j}");
    assert_eq!(j["state"]["nextRun"], "2026-09-30T12:10:00.000Z");
    let mut sched = Scheduler::default();
    // nothing is due at 12:05: sleep until 12:10 (capped at 60 s)
    assert_eq!(sched.step(&s.st, s.clock.now()), scheduler::MAX_SLEEP);
    s.clock.set("2026-09-30T12:09:30Z");
    assert_eq!(sched.step(&s.st, s.clock.now()), Duration::from_secs(30));
    assert!(s.runs("p").is_empty());

    s.clock.set("2026-09-30T12:10:00Z");
    sched.step(&s.st, s.clock.now());
    let task = s.wait_policy("p");
    assert_eq!(task.kind, "backup-policy");
    assert_eq!(task.dataset, "");
    assert_eq!(task.target.as_deref(), Some("p"));
    assert_eq!(task.state, "done", "{:?}", task.message);
    assert_eq!(task.message.as_deref(), Some("2/2 datasets backed up"));
    let detail = task.detail.unwrap();
    assert_eq!(detail["trigger"], "schedule");
    assert_eq!(detail["scheduledFor"], "2026-09-30T12:10:00.000Z");
    // in-memory datasets are backed up like the others
    assert_eq!(
        s.fake.names(),
        ["p-ds-20260930t121000z", "p-mem1-20260930t121000z"]
    );
    let runs = s.runs("p");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].trigger, RunTrigger::Schedule);
    assert_eq!(runs[0].result, RunResult::Ok);
    assert_eq!(
        runs[0].datasets[0].backup.as_deref(),
        Some("p-ds-20260930t121000z")
    );
    assert_eq!(runs[0].datasets[0].added_bytes, Some(70));
    assert_eq!(runs[0].datasets[1].result, DatasetRunResult::Ok);
    assert_eq!(
        runs[0].datasets[1].backup.as_deref(),
        Some("p-mem1-20260930t121000z")
    );
    assert_eq!(runs[0].retention, Some(RunRetention::default()));
    // the backup names this policy and run
    let b = s.fake.backups.lock()[0].clone();
    assert_eq!(b.policy.as_deref(), Some("p"));
    assert_eq!(b.run.as_deref(), Some(runs[0].id.as_str()));

    // a later step in the same window does not run it again
    sched.step(&s.st, t("2026-09-30T12:10:40Z"));
    assert!(s.b().policies.running_task("p").is_none());

    s.clock.set("2026-09-30T12:12:00Z");
    let (st, task) = s.call("POST", "/$/backup-policies/p/run", None).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["kind"], "backup-policy");
    assert_eq!(task["cancellable"], true);
    let task = s.wait(task["id"].as_str().unwrap());
    let detail = task.detail.unwrap();
    assert_eq!(detail["trigger"], "manual");
    assert!(detail["scheduledFor"].is_null());
    // `{time}` of a manual run is its start
    assert!(
        s.fake
            .names()
            .contains(&"p-ds-20260930t121200z".to_string())
    );
    let (_, j) = s.call("GET", "/$/backup-policies/p", None).await;
    assert_eq!(j["state"]["nextRun"], "2026-09-30T12:20:00.000Z");
    assert_eq!(j["state"]["lastScheduledFor"], "2026-09-30T12:10:00.000Z");
    assert_eq!(j["state"]["lastRun"]["trigger"], "manual");
    assert_eq!(j["state"]["lastSuccess"], "2026-09-30T12:12:00.000Z");
    let (_, j) = s
        .call("GET", "/$/backup-policies/p/runs?limit=1", None)
        .await;
    assert_eq!(j["runs"].as_array().unwrap().len(), 1);
    assert_eq!(j["runs"][0]["trigger"], "manual");
    let (_, j) = s.call("GET", "/$/backup-policies/p/runs", None).await;
    assert_eq!(j["runs"][1]["trigger"], "schedule");
}

fn write_state(dir: &FsPath, policy: J, last_scheduled_for: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("policies.json"),
        json!({"policies": [policy]}).to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("policy-state.json"),
        json!({"policies": {"hourly": {"lastScheduledFor": last_scheduled_for}}}).to_string(),
    )
    .unwrap();
}

// An hourly policy that last ran at 09:00, back at 12:40: 60 s later one catch-up
// run for 12:00, then the 13:00 run (the scheduler thread with a fake clock)
#[tokio::test]
async fn one_catch_up_run_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    write_state(
        &dir.path().join("backup"),
        json!({"name": "hourly", "repository": "local", "schedule": "0 * * * *"}),
        "2026-09-30T09:00:00.000Z",
    );
    let s = setup_with("2026-09-30T12:40:00Z", dir, false);
    scheduler::spawn(s.st.clone(), s.clock.clone());
    assert_eq!(s.clock.wait_sleep(), scheduler::STARTUP_DELAY);
    assert!(s.runs("hourly").is_empty());
    s.clock.advance(scheduler::STARTUP_DELAY);
    // 12:41: catch-up
    let d = s.clock.wait_sleep();
    assert_eq!(d, scheduler::MAX_SLEEP);
    s.wait_policy("hourly");
    let runs = s.runs("hourly");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].trigger, RunTrigger::CatchUp);
    assert_eq!(
        runs[0].scheduled_for.as_deref(),
        Some("2026-09-30T12:00:00.000Z")
    );
    assert_eq!(runs[0].started, "2026-09-30T12:41:00.000Z");
    assert_eq!(
        s.fake.names(),
        ["hourly-ds-20260930t120000z", "hourly-mem1-20260930t120000z"]
    );
    // on to 13:00, a minute at a time
    let mut d = d;
    while s.clock.now() < t("2026-09-30T13:00:00Z") {
        s.clock.advance(d);
        d = s.clock.wait_sleep();
    }
    assert_eq!(s.clock.now(), t("2026-09-30T13:00:00Z"));
    s.wait_policy("hourly");
    let runs = s.runs("hourly");
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].trigger, RunTrigger::Schedule);
    assert_eq!(
        runs[0].scheduled_for.as_deref(),
        Some("2026-09-30T13:00:00.000Z")
    );
    // the state survives a restart
    let again = Policies::load(&s.backup_dir(), &Registry::default()).unwrap();
    assert_eq!(again.runs("hourly", 10).len(), 2);
    assert_eq!(
        again.inner.lock().state.policies["hourly"]
            .last_scheduled_for
            .as_deref(),
        Some("2026-09-30T13:00:00.000Z")
    );
}

// Missed runs with `catchUp: "none"`: the missed 12:00 is recorded skipped; the next run is 13:00
#[tokio::test]
async fn missed_runs_skipped_without_catch_up() {
    let dir = tempfile::tempdir().unwrap();
    write_state(
        &dir.path().join("backup"),
        json!({"name": "hourly", "repository": "local", "schedule": "0 * * * *",
               "catchUp": "none"}),
        "2026-09-30T09:00:00.000Z",
    );
    let s = setup_with("2026-09-30T12:41:00Z", dir, false);
    let mut sched = Scheduler::default();
    sched.step(&s.st, s.clock.now());
    assert!(s.b().policies.running_task("hourly").is_none());
    let runs = s.runs("hourly");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].result, RunResult::Skipped);
    assert_eq!(runs[0].trigger, RunTrigger::CatchUp);
    assert_eq!(
        runs[0].scheduled_for.as_deref(),
        Some("2026-09-30T12:00:00.000Z")
    );
    sched.step(&s.st, t("2026-09-30T12:59:59Z"));
    assert_eq!(s.runs("hourly").len(), 1);
    s.clock.set("2026-09-30T13:00:00Z");
    sched.step(&s.st, s.clock.now());
    s.wait_policy("hourly");
    let runs = s.runs("hourly");
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].trigger, RunTrigger::Schedule);
    assert_eq!(runs[0].result, RunResult::Ok);
    // a clock that goes back re-runs nothing
    sched.step(&s.st, t("2026-09-30T11:00:00Z"));
    sched.step(&s.st, t("2026-09-30T13:00:30Z"));
    assert_eq!(s.runs("hourly").len(), 2);
    assert!(s.b().policies.running_task("hourly").is_none());
}

#[tokio::test]
async fn overlapping_runs_are_skipped() {
    let s = setup("2026-09-30T12:05:00Z");
    s.call(
        "POST",
        "/$/backup-policies",
        Some(json!({"name": "p", "repository": "local", "schedule": "*/10 * * * *"})),
    )
    .await;
    *s.fake.hold.lock() = true;
    let (st, task) = s.call("POST", "/$/backup-policies/p/run", None).await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let id = task["id"].as_str().unwrap().to_string();
    let (st, j) = s.call("POST", "/$/backup-policies/p/run", None).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(j["code"], "policy-running");
    assert_eq!(j["task"], id.as_str());
    let (_, j) = s.call("GET", "/$/backup-policies/p", None).await;
    assert_eq!(j["state"]["runningTask"], id.as_str());
    // the 12:10 instant comes while it runs
    s.clock.set("2026-09-30T12:10:00Z");
    Scheduler::default().step(&s.st, s.clock.now());
    let runs = s.runs("p");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].result, RunResult::Skipped);
    assert_eq!(runs[0].trigger, RunTrigger::Schedule);
    assert_eq!(
        runs[0].reason.as_deref(),
        Some("the previous run is still running")
    );
    s.fake.release();
    let task = s.wait(&id);
    assert_eq!(task.state, "done");
    assert_eq!(s.runs("p").len(), 2);
    let (_, j) = s.call("GET", "/$/backup-policies/p", None).await;
    assert!(j["state"]["runningTask"].is_null());
    // skipped runs do not count as failures
    assert_eq!(j["state"]["consecutiveFailures"], 0);
}

/// Policy runs are admitted like other backup tasks: a manual run beyond the queue is
/// `503 too-many-tasks`, a scheduled one is recorded skipped, and the GC after
/// retention is not started.
#[tokio::test]
async fn runs_beyond_the_task_queue_are_refused_or_skipped() {
    let s = setup("2026-09-30T12:05:00Z");
    s.call(
        "POST",
        "/$/backup-policies",
        Some(json!({"name": "p", "repository": "local", "schedule": "*/10 * * * *"})),
    )
    .await;
    let b = s.b();
    let held: Vec<_> = (0..b.max_tasks * (1 + crate::backup::QUEUE_PER_SLOT))
        .map(|_| b.admit().unwrap())
        .collect();
    let (st, j) = s.call("POST", "/$/backup-policies/p/run", None).await;
    assert_eq!(
        (st, j["code"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("too-many-tasks")),
        "{j}"
    );
    s.clock.set("2026-09-30T12:10:00Z");
    Scheduler::default().step(&s.st, s.clock.now());
    let runs = s.runs("p");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].result, RunResult::Skipped);
    assert!(
        runs[0]
            .reason
            .as_deref()
            .is_some_and(|r| r.starts_with("too many backup tasks")),
        "{:?}",
        runs[0].reason
    );
    assert!(s.b().policies.running_task("p").is_none());
    let e = ServerEngine.start_gc(&s.st, "local").unwrap_err();
    assert_eq!(e.code(), Code::TooManyTasks);
    // with room again, the next instant runs
    drop(held);
    s.clock.set("2026-09-30T12:20:00Z");
    Scheduler::default().step(&s.st, s.clock.now());
    let task = s.wait_policy("p");
    assert_eq!(task.state, "done", "{:?}", task.message);
}

/// A read-only server runs no policies: manual runs are `403 server-read-only` and the
/// scheduler lets their instants pass.
#[tokio::test]
async fn read_only_servers_run_no_policies() {
    let s = setup_with("2026-09-30T12:05:00Z", tempfile::tempdir().unwrap(), true);
    let mut p: PolicyConfig = serde_json::from_value(
        json!({"name": "p", "repository": "local", "schedule": "*/10 * * * *"}),
    )
    .unwrap();
    p.datasets = vec!["*".into()];
    s.b().registry.policies.write().insert(
        "p".into(),
        PolicyEntry {
            config: p,
            source: ConfigSource::Config,
        },
    );
    let (st, j) = s.call("POST", "/$/backup-policies/p/run", None).await;
    assert_eq!(
        (st, j["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("server-read-only")),
        "{j}"
    );
    let mut sched = Scheduler::default();
    for now in ["2026-09-30T12:10:00Z", "2026-09-30T12:20:00Z"] {
        s.clock.set(now);
        assert_eq!(sched.step(&s.st, s.clock.now()), scheduler::MAX_SLEEP);
    }
    assert!(s.runs("p").is_empty());
    assert!(s.st.tasks.lock().is_empty());
}

// Retention through the route, with a dry run first
#[tokio::test]
async fn retention_route() {
    let s = setup("2026-09-30T12:00:00Z");
    s.call(
        "POST",
        "/$/backup-policies",
        Some(
            json!({"name": "p", "repository": "local", "schedule": "0 3 * * *",
                    "retention": {"minCount": 2, "maxCount": 3, "expireAfter": "2d"}}),
        ),
    )
    .await;
    let now = s.clock.now();
    let id = Uuid::from_u128(DS_ID);
    for (name, hours, policy) in [
        ("d0.5", 12, Some("p")),
        ("d1", 24, Some("p")),
        ("d3", 72, Some("p")),
        ("d4", 96, Some("p")),
        ("d5", 120, Some("p")),
        ("manual", 240, None),
    ] {
        s.fake.backups.lock().push(summary(
            name,
            "ds",
            id,
            1,
            policy,
            now - TimeDelta::hours(hours),
        ));
    }
    let names = |j: &J| -> Vec<String> {
        j.as_array()
            .unwrap()
            .iter()
            .map(|b| b["name"].as_str().unwrap().to_string())
            .collect()
    };
    let (st, j) = s
        .call("POST", "/$/backup-policies/p/retention?dryRun=true", None)
        .await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(j["dryRun"], true);
    assert_eq!(names(&j["delete"]), ["d3", "d4", "d5"]);
    assert_eq!(names(&j["keep"]), ["d0.5", "d1"]);
    assert!(j.get("errors").is_none());
    assert_eq!(s.fake.names().len(), 6);
    // a backup a restore uses is kept this time
    s.fake.busy.lock().insert("d5".into());
    let (st, j) = s.call("POST", "/$/backup-policies/p/retention", None).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(j["dryRun"], false);
    assert_eq!(names(&j["delete"]), ["d3", "d4"]);
    assert_eq!(s.fake.names(), ["d0.5", "d1", "d5", "manual"]);
    s.fake.busy.lock().clear();
    let (_, j) = s.call("POST", "/$/backup-policies/p/retention", None).await;
    assert_eq!(names(&j["delete"]), ["d5"]);
    assert_eq!(s.fake.names(), ["d0.5", "d1", "manual"]);
}

#[tokio::test]
async fn runs_apply_retention_and_collect_once_a_day() {
    let s = setup("2026-09-30T12:00:00Z");
    s.call(
        "POST",
        "/$/backup-policies",
        Some(
            json!({"name": "p", "repository": "local", "schedule": "0 3 * * *",
                    "datasets": ["d*"], "retention": {"minCount": 1, "maxCount": 1},
                    "gcAfterRetention": true}),
        ),
    )
    .await;
    let run = |s: &T| {
        let st = s.st.clone();
        let task = start_run(&st, "p".into(), RunTrigger::Manual, None)
            .ok()
            .unwrap();
        s.wait(&task.id);
        s.runs("p").remove(0)
    };
    let first = run(&s);
    assert_eq!(first.datasets.len(), 1, "mem1 does not match d*");
    assert_eq!(first.gc, None, "retention deleted nothing: no GC");
    s.clock.set("2026-09-30T13:00:00Z");
    let second = run(&s);
    assert_eq!(second.retention.unwrap().deleted, ["p-ds-20260930t120000z"]);
    assert_eq!(
        second.gc,
        Some(RunGc {
            task: "gc-1".into()
        })
    );
    s.clock.set("2026-09-30T14:00:00Z");
    let third = run(&s);
    assert_eq!(third.retention.unwrap().deleted, ["p-ds-20260930t130000z"]);
    assert_eq!(third.gc, None, "one GC per repository per 24 h");
    s.clock.set("2026-10-01T13:00:01Z");
    let fourth = run(&s);
    assert_eq!(
        fourth.gc,
        Some(RunGc {
            task: "gc-2".into()
        })
    );
    assert_eq!(s.fake.names(), ["p-ds-20261001t130001z"]);
}

#[tokio::test]
async fn failures_names_and_unchanged_datasets() {
    let s = setup("2026-09-30T12:00:00Z");
    s.call(
        "POST",
        "/$/backup-policies",
        Some(
            json!({"name": "p", "repository": "local", "schedule": "0 3 * * *",
                    "nameTemplate": "{policy}-{dataset}", "skipUnchanged": true}),
        ),
    )
    .await;
    s.fake.datasets.lock().push(DatasetInfo {
        name: "ds2".into(),
        id: Uuid::from_u128(2),
        head: 3,
    });
    // a name already taken gets `-2`; a failing dataset makes the run partial
    s.fake.backups.lock().push(summary(
        "p-ds",
        "ds",
        Uuid::from_u128(DS_ID),
        6,
        None,
        s.clock.now(),
    ));
    s.fake.failing.lock().insert("ds2".into());
    let task = start_run(&s.st, "p".into(), RunTrigger::Manual, None)
        .ok()
        .unwrap();
    let task = s.wait(&task.id);
    assert_eq!(task.state, "done");
    let run = &s.runs("p")[0];
    assert_eq!(run.result, RunResult::Partial);
    let by = |n: &str| {
        run.datasets
            .iter()
            .find(|d| d.dataset == n)
            .unwrap()
            .clone()
    };
    assert_eq!(by("ds").backup.as_deref(), Some("p-ds-2"));
    assert_eq!(by("ds2").result, DatasetRunResult::Failed);
    assert_eq!(by("ds2").reason.as_deref(), Some("the bucket is on fire"));
    let (_, j) = s.call("GET", "/$/backup-policies/p", None).await;
    assert_eq!(j["state"]["consecutiveFailures"], 1);
    assert!(j["state"]["lastSuccess"].is_null());

    let mut metrics = String::new();
    render_metrics(&s.st, &mut metrics);
    assert!(
        metrics.contains("sparkles_backup_policy_runs_total{policy=\"p\",result=\"partial\"} 1"),
        "{metrics}"
    );
    assert!(metrics.contains("sparkles_backup_policy_consecutive_failures{policy=\"p\"} 1"));
    assert!(metrics.contains("sparkles_backup_policy_next_run_timestamp_seconds{policy=\"p\"} "));
    assert!(!metrics.contains("sparkles_backup_policy_last_success_timestamp_seconds{"));

    // ds is unchanged since p-ds-2 (head 7): skipped; ds2 works now
    s.fake.failing.lock().clear();
    s.clock.set("2026-09-30T12:30:00Z");
    let task = start_run(&s.st, "p".into(), RunTrigger::Manual, None)
        .ok()
        .unwrap();
    s.wait(&task.id);
    let run = &s.runs("p")[0];
    assert_eq!(run.result, RunResult::Ok);
    let ds = run.datasets.iter().find(|d| d.dataset == "ds").unwrap();
    assert_eq!(ds.reason.as_deref(), Some("unchanged"));
    let (_, j) = s.call("GET", "/$/backup-policies/p", None).await;
    assert_eq!(j["state"]["consecutiveFailures"], 0);
    assert_eq!(j["state"]["lastSuccess"], "2026-09-30T12:30:00.000Z");
    let mut metrics = String::new();
    render_metrics(&s.st, &mut metrics);
    let ts = t("2026-09-30T12:30:00Z").timestamp();
    assert!(metrics.contains(&format!(
        "sparkles_backup_policy_last_success_timestamp_seconds{{policy=\"p\"}} {ts}"
    )));
}

#[tokio::test]
async fn cancelling_a_run() {
    let s = setup("2026-09-30T12:00:00Z");
    s.call(
        "POST",
        "/$/backup-policies",
        Some(json!({"name": "p", "repository": "local", "schedule": "0 3 * * *"})),
    )
    .await;
    s.fake.datasets.lock().insert(
        0,
        DatasetInfo {
            name: "a".into(),
            id: Uuid::from_u128(5),
            head: 1,
        },
    );
    *s.fake.hold.lock() = true;
    let task = start_run(&s.st, "p".into(), RunTrigger::Manual, None)
        .ok()
        .unwrap();
    s.fake.wait_entered();
    assert!(s.st.cancel_task(&task.id).unwrap().is_ok());
    s.fake.release();
    let task = s.wait(&task.id);
    assert_eq!(task.state, "cancelled");
    let run = &s.runs("p")[0];
    // the backup in flight finished (the fake ignores the flag); the rest were skipped
    assert_eq!(run.datasets[0].result, DatasetRunResult::Ok);
    assert_eq!(run.datasets[1].reason.as_deref(), Some("cancelled"));
    assert_eq!(run.result, RunResult::Partial);
    assert!(run.retention.is_none());
}

#[tokio::test]
async fn a_policy_disabled_mid_run_stops() {
    let s = setup("2026-09-30T12:00:00Z");
    s.call(
        "POST",
        "/$/backup-policies",
        Some(json!({"name": "p", "repository": "local", "schedule": "0 3 * * *"})),
    )
    .await;
    s.fake.datasets.lock().insert(
        0,
        DatasetInfo {
            name: "a".into(),
            id: Uuid::from_u128(5),
            head: 1,
        },
    );
    *s.fake.hold.lock() = true;
    let task = start_run(&s.st, "p".into(), RunTrigger::Schedule, Some(s.clock.now()))
        .ok()
        .unwrap();
    s.fake.wait_entered();
    s.b()
        .registry
        .policies
        .write()
        .get_mut("p")
        .unwrap()
        .config
        .enabled = false;
    s.fake.release();
    s.wait(&task.id);
    let run = &s.runs("p")[0];
    assert_eq!(run.result, RunResult::Skipped);
    assert_eq!(run.datasets[1].reason.as_deref(), Some("policy disabled"));
}

#[test]
fn the_run_history_is_a_ring() {
    let dir = tempfile::tempdir().unwrap();
    let p = Policies::load(dir.path(), &Registry::default()).unwrap();
    {
        let mut inner = p.inner.lock();
        let run = |i: usize| PolicyRun {
            id: i.to_string(),
            policy: "p".into(),
            trigger: RunTrigger::Schedule,
            scheduled_for: None,
            started: "2026-09-30T12:00:00.000Z".into(),
            finished: None,
            result: RunResult::Ok,
            reason: None,
            datasets: vec![],
            retention: None,
            gc: None,
        };
        for i in 0..MAX_RUNS + 4 {
            inner.runs.push_front(run(i));
        }
        p.record(&mut inner, run(MAX_RUNS + 4));
    }
    let again = Policies::load(dir.path(), &Registry::default()).unwrap();
    let runs = again.runs("p", usize::MAX);
    assert_eq!(runs.len(), MAX_RUNS);
    assert_eq!(runs[0].id, (MAX_RUNS + 4).to_string());
}

#[test]
fn invalid_config_file_policies_stop_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::default();
    let mut config: PolicyConfig = serde_json::from_value(nightly()).unwrap();
    config.schedule = "sometimes".into();
    registry.policies.write().insert(
        "nightly".into(),
        PolicyEntry {
            config,
            source: ConfigSource::Config,
        },
    );
    let e = Policies::load(dir.path(), &registry).err().unwrap();
    assert!(format!("{e:#}").contains("backup policy nightly"), "{e:#}");
}

/// A real policy run against an `fs` repository registered over HTTP.
#[tokio::test]
async fn end_to_end_policy_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut state =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    state.backup = Some(Arc::new(BackupState::new(dir.path(), None, 2).unwrap()));
    let st = Arc::new(state);
    st.create("ds", crate::state::DbType::Persistent).unwrap();
    let app = crate::http::router(st.clone());
    let call = |method: &str, uri: &str, body: J| {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let app = app.clone();
        async move {
            let res = app.oneshot(req).await.unwrap();
            let status = res.status();
            let b = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, serde_json::from_slice::<J>(&b).unwrap_or(J::Null))
        }
    };
    let repo = tempfile::tempdir().unwrap();
    let (st_, j) = call(
        "POST",
        "/$/repositories",
        json!({"name": "mem", "type": "fs", "path": repo.path()}),
    )
    .await;
    assert_eq!(st_, StatusCode::CREATED, "{j}");
    let (st_, j) = call(
        "POST",
        "/$/backup-policies",
        json!({"name": "p", "repository": "mem", "schedule": "every 1h",
               "retention": {"maxCount": 1}}),
    )
    .await;
    assert_eq!(st_, StatusCode::CREATED, "{j}");
    for _ in 0..2 {
        let (st_, task) = call("POST", "/$/backup-policies/p/run", J::Null).await;
        assert_eq!(st_, StatusCode::ACCEPTED, "{task}");
        let id = task["id"].as_str().unwrap().to_string();
        loop {
            let t = st
                .tasks
                .lock()
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .unwrap();
            if !t.active() {
                assert_eq!(t.state, "done", "{:?}", t.message);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(1100));
    }
    let (_, j) = call("GET", "/$/repositories/mem/backups?policy=p", J::Null).await;
    assert_eq!(j["backups"].as_array().unwrap().len(), 1, "{j}");
}
