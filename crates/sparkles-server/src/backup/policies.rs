//! Lifecycle policies on the server: `policies.json` (API policies), `policy-state.json`
//! (`lastScheduledFor`, `lastSuccess`, `consecutiveFailures`, and when each repository
//! was last collected after retention), `runs.json` (a ring of the last 1000
//! `PolicyRun`s), all written with `state::write_file_atomic` under `<data>/backup/`;
//! the `backup-policy` task (back up the selected datasets one after another, apply
//! retention, optionally start GC); and the `/$/backup-policies` routes (all
//! `server-admin`). A policy may not be named `preview` (that path is the preview
//! route).
//!
//! The policies themselves live in the registry (`Registry::policies`, shared with the
//! config file's). What a run does to repositories and datasets goes through an
//! [`Engine`], so tests drive the scheduler and the routes without a repository.

use super::BackupState;
use super::http::error_response;
use super::registry::{PolicyEntry, Registry};
use super::scheduler::{Clock, SystemClock};
use crate::state::{AppState, DbType, Task, TaskHandle, write_file_atomic};
use anyhow::Context as _;
use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sparkles_backup::policy::{self, NameCtx, Schedule, Tz};
use sparkles_backup::{
    BackupError, BackupSummary, Code, ConfigSource, CreateOptions, Ctl, DatasetRunResult,
    GcOptions, ListFilter, Policy, PolicyConfig, PolicyList, PolicyRun, PolicyRunDataset,
    PolicyRunList, PolicyState, PreviewRequest, PreviewResponse, RetentionResponse, RunGc,
    RunResult, RunRetention, RunTrigger,
};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

/// `runs.json` keeps this many runs (all policies together).
pub const MAX_RUNS: usize = 1000;
/// `gcAfterRetention` collects a repository at most this often.
const GC_INTERVAL: TimeDelta = TimeDelta::hours(24);

fn fmt_time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.to_utc())
}

// ------------------------------------------------------------------------ engine ------

/// A dataset a policy may back up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatasetInfo {
    pub name: String,
    /// `mem` datasets are skipped (reported)
    pub in_memory: bool,
    pub id: Uuid,
    /// the head commit
    pub head: u64,
}

/// What policy runs and retention do outside the policy module: list datasets, create,
/// list and delete backups, start GC. [`ServerEngine`] is the real one; tests use
/// fakes.
pub trait Engine: Send + Sync {
    fn datasets(&self, st: &Arc<AppState>) -> Vec<DatasetInfo>;
    /// The backups of `policy` in repository `repo`.
    fn list(
        &self,
        st: &Arc<AppState>,
        repo: &str,
        policy: &str,
    ) -> Result<Vec<BackupSummary>, BackupError>;
    /// Back up `dataset` to `repo` (`409 backup-exists` when the name is taken).
    fn create(
        &self,
        st: &Arc<AppState>,
        repo: &str,
        dataset: &str,
        o: CreateOptions,
    ) -> Result<BackupSummary, BackupError>;
    fn delete(&self, st: &Arc<AppState>, repo: &str, name: &str) -> Result<bool, BackupError>;
    /// Backups of `repo` a restore or verify on this server uses (retention keeps them).
    fn busy(&self, st: &Arc<AppState>, repo: &str) -> HashSet<String>;
    /// Start a `backup-gc` task on `repo`; its id.
    fn start_gc(&self, st: &Arc<AppState>, repo: &str) -> Result<String, BackupError>;
}

/// The engine of a running server: the registry's repositories and the server's
/// datasets.
pub struct ServerEngine;

fn backup_state(st: &AppState) -> Result<&Arc<BackupState>, BackupError> {
    st.backup.as_ref().ok_or_else(disabled)
}

/// Drive an engine future from a std thread: on the server's runtime, or on a
/// throwaway one before `backup::start` (tests, tools).
pub fn block_on<F: std::future::Future>(b: &BackupState, f: F) -> F::Output {
    match b.handle() {
        Some(h) => h.block_on(f),
        None => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime")
            .block_on(f),
    }
}

impl Engine for ServerEngine {
    fn datasets(&self, st: &Arc<AppState>) -> Vec<DatasetInfo> {
        st.datasets
            .read()
            .values()
            .map(|d| DatasetInfo {
                name: d.name.clone(),
                in_memory: d.kind == DbType::Mem,
                id: d.store.dataset_id(),
                head: d.store.head_commit().seq,
            })
            .collect()
    }

    fn list(
        &self,
        st: &Arc<AppState>,
        repo: &str,
        policy: &str,
    ) -> Result<Vec<BackupSummary>, BackupError> {
        let b = backup_state(st)?;
        let r = b.repo(repo)?;
        let filter = ListFilter {
            policy: Some(policy.to_string()),
            ..Default::default()
        };
        block_on(b, r.list(&filter))
    }

    // TODO(merge): route through the server's backup task slots and its per-(dataset,
    // repository) exclusion once they exist, so policy backups share them.
    fn create(
        &self,
        st: &Arc<AppState>,
        repo: &str,
        dataset: &str,
        o: CreateOptions,
    ) -> Result<BackupSummary, BackupError> {
        let b = backup_state(st)?;
        let ds = st.get(dataset).ok_or_else(|| {
            BackupError::new(Code::InvalidRequest, format!("no such dataset: {dataset}"))
        })?;
        if ds.kind == DbType::Mem {
            return Err(BackupError::new(
                Code::BackupUnsupported,
                "in-memory datasets cannot be backed up",
            ));
        }
        let r = b.repo(repo)?;
        let capture = ds.store.backup_capture(&o.name)?;
        drop(ds);
        block_on(b, r.create(capture.into(), &o))
    }

    fn delete(&self, st: &Arc<AppState>, repo: &str, name: &str) -> Result<bool, BackupError> {
        let b = backup_state(st)?;
        let r = b.repo(repo)?;
        block_on(b, r.delete(name))
    }

    // TODO(merge): the server's busy set of backups a restore or verify uses
    fn busy(&self, _st: &Arc<AppState>, _repo: &str) -> HashSet<String> {
        HashSet::new()
    }

    // TODO(merge): share the GC task of `POST /$/repositories/{repo}/gc`
    fn start_gc(&self, st: &Arc<AppState>, repo: &str) -> Result<String, BackupError> {
        let b = backup_state(st)?.clone();
        let r = b.repo(repo)?;
        let name = repo.to_string();
        let task = st.start_task_opts(
            st.next_task_id(),
            "backup-gc",
            "",
            Some(repo),
            true,
            move |h| {
                let opts = GcOptions {
                    ctl: Ctl::with_cancel(h.cancel_flag()),
                    ..Default::default()
                };
                h.progress(0.05, "marking");
                let report = block_on(&b, r.gc(&opts))?;
                h.set_detail(serde_json::to_value(&report)?);
                tracing::info!(target: "sparkles::backup", repository = name.as_str(),
                    deleted = report.deleted, deleted_bytes = report.deleted_bytes, "gc after retention");
                Ok(format!(
                    "deleted {} blobs ({} bytes)",
                    report.deleted, report.deleted_bytes
                ))
            },
        );
        Ok(task.id)
    }
}

// ------------------------------------------------------------------------- state ------

/// The persisted scheduler state of one policy.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Persisted {
    #[serde(default)]
    last_scheduled_for: Option<String>,
    #[serde(default)]
    last_success: Option<String>,
    #[serde(default)]
    consecutive_failures: u32,
}

/// `policy-state.json`
#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    policies: BTreeMap<String, Persisted>,
    /// repository → the last `gcAfterRetention` start
    #[serde(default)]
    gc: BTreeMap<String, String>,
}

/// `policies.json`
#[derive(Debug, Default, Serialize, Deserialize)]
struct PoliciesFile {
    #[serde(default)]
    policies: Vec<PolicyConfig>,
}

struct Inner {
    state: StateFile,
    /// newest first
    runs: VecDeque<PolicyRun>,
    /// policy → its running `backup-policy` task
    running: BTreeMap<String, String>,
    /// `sparkles_backup_policy_runs_total` by (policy, result)
    counts: BTreeMap<(String, &'static str), u64>,
}

/// Policy scheduler state, run history, and the running policy tasks
/// (`BackupState::policies`).
pub struct Policies {
    dir: PathBuf,
    inner: Mutex<Inner>,
    clock: RwLock<Arc<dyn Clock>>,
    engine: RwLock<Arc<dyn Engine>>,
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &FsPath) -> anyhow::Result<T> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_json<T: Serialize>(dir: &FsPath, file: &str, v: &T) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(file);
    write_file_atomic(&path, &serde_json::to_vec_pretty(v)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// The API policies of `<dir>/policies.json` (none when it does not exist).
pub fn read_api_policies(dir: &FsPath) -> anyhow::Result<Vec<PolicyConfig>> {
    Ok(read_json::<PoliciesFile>(&dir.join("policies.json"))?.policies)
}

/// Write the registry's API policies to `<dir>/policies.json`.
pub fn write_api_policies(dir: &FsPath, registry: &Registry) -> anyhow::Result<()> {
    let policies = registry
        .policies
        .read()
        .values()
        .filter(|e| e.source == ConfigSource::Api)
        .map(|e| e.config.clone())
        .collect();
    write_json(dir, "policies.json", &PoliciesFile { policies })
}

impl Policies {
    /// Read `policy-state.json` and `runs.json` under `dir` (`<data>/backup`), add the
    /// API policies of `policies.json` the registry does not have yet, and check every
    /// policy (a config-file policy that does not validate stops the server).
    pub fn load(dir: &FsPath, registry: &Registry) -> anyhow::Result<Policies> {
        {
            let mut map = registry.policies.write();
            for p in read_api_policies(dir)? {
                map.entry(p.name.clone()).or_insert(PolicyEntry {
                    config: p,
                    source: ConfigSource::Api,
                });
            }
            for (name, e) in map.iter() {
                policy::check_policy(&e.config)
                    .map_err(|err| anyhow::anyhow!("backup policy {name}: {}", err.message()))?;
            }
        }
        let state: StateFile = read_json(&dir.join("policy-state.json"))?;
        let runs: PolicyRunList = read_json(&dir.join("runs.json"))?;
        Ok(Policies {
            dir: dir.to_path_buf(),
            inner: Mutex::new(Inner {
                state,
                runs: runs.runs.into_iter().take(MAX_RUNS).collect(),
                running: BTreeMap::new(),
                counts: BTreeMap::new(),
            }),
            clock: RwLock::new(Arc::new(SystemClock)),
            engine: RwLock::new(Arc::new(ServerEngine)),
        })
    }

    /// The time source of schedules, run times and `nextRun` (the scheduler's).
    pub fn set_clock(&self, clock: Arc<dyn Clock>) {
        *self.clock.write() = clock;
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.read().now()
    }

    /// Replace the engine (tests).
    pub fn set_engine(&self, engine: Arc<dyn Engine>) {
        *self.engine.write() = engine;
    }

    fn engine(&self) -> Arc<dyn Engine> {
        self.engine.read().clone()
    }

    fn save_state(&self, inner: &Inner) {
        if let Err(e) = write_json(&self.dir, "policy-state.json", &inner.state) {
            tracing::error!(target: "sparkles::backup", "{e:#}");
        }
    }

    fn save_runs(&self, inner: &Inner) {
        let list = PolicyRunList {
            runs: inner.runs.iter().cloned().collect(),
        };
        if let Err(e) = write_json(&self.dir, "runs.json", &list) {
            tracing::error!(target: "sparkles::backup", "{e:#}");
        }
    }

    /// Add a finished (or skipped) run to the history and count it.
    fn record(&self, inner: &mut Inner, run: PolicyRun) {
        *inner
            .counts
            .entry((run.policy.clone(), result_str(run.result)))
            .or_default() += 1;
        inner.runs.push_front(run);
        inner.runs.truncate(MAX_RUNS);
        self.save_runs(inner);
    }

    /// The task running policy `name`, if any.
    pub fn running_task(&self, name: &str) -> Option<String> {
        self.inner.lock().running.get(name).cloned()
    }

    /// The runs of `name`, newest first.
    pub fn runs(&self, name: &str, limit: usize) -> Vec<PolicyRun> {
        self.inner
            .lock()
            .runs
            .iter()
            .filter(|r| r.policy == name)
            .take(limit)
            .cloned()
            .collect()
    }

    /// The API view of a policy: its settings with the scheduler state.
    pub fn view(&self, e: &PolicyEntry) -> Policy {
        let now = self.now();
        let inner = self.inner.lock();
        let p = inner
            .state
            .policies
            .get(&e.config.name)
            .cloned()
            .unwrap_or_default();
        let next_run = e
            .config
            .enabled
            .then(|| policy::check_policy(&e.config).ok())
            .flatten()
            .and_then(|(s, tz)| policy::next_runs(&s, tz, now, 1).pop())
            .map(fmt_time);
        let last_run = inner
            .runs
            .iter()
            .find(|r| r.policy == e.config.name)
            .cloned()
            .map(Box::new);
        Policy {
            config: e.config.clone(),
            source: e.source,
            state: PolicyState {
                next_run,
                last_scheduled_for: p.last_scheduled_for,
                last_run,
                last_success: p.last_success,
                consecutive_failures: p.consecutive_failures,
                running_task: inner.running.get(&e.config.name).cloned(),
            },
        }
    }

    /// A new or rescheduled policy starts from its latest instant at or before now, so
    /// it waits for its next one (no catch-up run for instants before it existed).
    fn reset_schedule(&self, name: &str, s: &Schedule, tz: Tz) {
        let last = policy::latest_due(s, tz, self.now(), None).map(fmt_time);
        let mut inner = self.inner.lock();
        inner
            .state
            .policies
            .entry(name.to_string())
            .or_default()
            .last_scheduled_for = last;
        self.save_state(&inner);
    }

    fn forget(&self, name: &str) {
        let mut inner = self.inner.lock();
        if inner.state.policies.remove(name).is_some() {
            self.save_state(&inner);
        }
    }

    /// Scheduler step: start (or record as skipped) a run of every due policy at `now`
    /// and return the earliest next instant of the enabled policies.
    pub fn evaluate_all(&self, st: &Arc<AppState>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let Some(b) = &st.backup else { return None };
        let entries: Vec<PolicyConfig> = b
            .registry
            .policies
            .read()
            .values()
            .map(|e| e.config.clone())
            .collect();
        entries
            .iter()
            .filter_map(|p| self.evaluate(st, p, now))
            .min()
    }

    /// Scheduler step of one policy (see [`evaluate_all`](Self::evaluate_all)).
    fn evaluate(
        &self,
        st: &Arc<AppState>,
        p: &PolicyConfig,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        let (s, tz) = policy::check_policy(p).ok()?;
        let mut inner = self.inner.lock();
        let since = inner
            .state
            .policies
            .get(&p.name)
            .and_then(|x| x.last_scheduled_for.as_deref())
            .and_then(parse_time);
        let due = policy::latest_due(&s, tz, now, since);
        if let Some(due) = due {
            // persisted before anything runs, and only ever forward
            inner
                .state
                .policies
                .entry(p.name.clone())
                .or_default()
                .last_scheduled_for = Some(fmt_time(due));
            self.save_state(&inner);
        }
        let next = policy::next_runs(&s, tz, now, 1).pop();
        // a policy seen for the first time waits for its next instant; a disabled one
        // lets its instants pass
        let (Some(since), Some(due), true) = (since, due, p.enabled) else {
            return next.filter(|_| p.enabled);
        };
        let missed = policy::missed_before(&s, tz, since, due);
        let trigger = if missed {
            RunTrigger::CatchUp
        } else {
            RunTrigger::Schedule
        };
        let skipped = |why: &str| {
            tracing::info!(target: "sparkles::backup", policy = p.name.as_str(),
                scheduled_for = %fmt_time(due), "policy run skipped: {why}");
            PolicyRun {
                id: Uuid::new_v4().to_string(),
                policy: p.name.clone(),
                trigger,
                scheduled_for: Some(fmt_time(due)),
                started: fmt_time(now),
                finished: Some(fmt_time(now)),
                result: RunResult::Skipped,
                datasets: Vec::new(),
                retention: None,
                gc: None,
            }
        };
        if missed && p.catch_up == sparkles_backup::CatchUp::None {
            let run = skipped("missed while the server was down (catchUp: none)");
            self.record(&mut inner, run);
        } else if inner.running.contains_key(&p.name) {
            let run = skipped("the previous run is still running");
            self.record(&mut inner, run);
        } else {
            drop(inner);
            if let Err(e) = start_run(st, p.name.clone(), trigger, Some(due)) {
                tracing::warn!(target: "sparkles::backup", policy = p.name.as_str(),
                    "policy run not started: {}", e.message());
            }
        }
        next
    }
}

fn result_str(r: RunResult) -> &'static str {
    match r {
        RunResult::Ok => "ok",
        RunResult::Partial => "partial",
        RunResult::Failed => "failed",
        RunResult::Skipped => "skipped",
    }
}

// -------------------------------------------------------------------------- runs ------

/// Why a run could not start.
pub enum StartError {
    /// `409 policy-running`, with the running task
    Running(String),
    Other(BackupError),
}

impl StartError {
    fn message(&self) -> String {
        match self {
            StartError::Running(t) => format!("the policy is running (task {t})"),
            StartError::Other(e) => e.message().to_string(),
        }
    }

    fn response(&self, name: &str) -> Response {
        match self {
            StartError::Running(task) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": format!("policy {name} is running (task {task})"),
                    "code": "policy-running",
                    "task": task,
                })),
            )
                .into_response(),
            StartError::Other(e) => error_response(e),
        }
    }
}

impl From<BackupError> for StartError {
    fn from(e: BackupError) -> StartError {
        StartError::Other(e)
    }
}

/// Start a `backup-policy` task for policy `name` (server-scoped: `dataset: ""`,
/// `target` the policy). `scheduled_for` is the instant a scheduled or catch-up run is
/// for (`{time}` of the names; the start time for a manual run).
pub fn start_run(
    st: &Arc<AppState>,
    name: String,
    trigger: RunTrigger,
    scheduled_for: Option<DateTime<Utc>>,
) -> Result<Task, StartError> {
    let b = backup_state(st)?.clone();
    let config = b
        .registry
        .policies
        .read()
        .get(&name)
        .map(|e| e.config.clone())
        .ok_or_else(|| no_such_policy(&name))?;
    let (_, tz) = policy::check_policy(&config)?;
    let id = st.next_task_id();
    {
        let mut inner = b.policies.inner.lock();
        if let Some(t) = inner.running.get(&name) {
            return Err(StartError::Running(t.clone()));
        }
        inner.running.insert(name.clone(), id.clone());
    }
    let started = b.policies.now();
    let st2 = st.clone();
    let (task_id, target) = (id.clone(), name.clone());
    let task = st.start_task_opts(id, "backup-policy", "", Some(&target), true, move |h| {
        let run = Run {
            st: &st2,
            b: &b,
            h,
            config,
            tz,
            trigger,
            scheduled_for,
            started,
        };
        let r = run.run();
        let mut inner = b.policies.inner.lock();
        if inner.running.get(&name) == Some(&task_id) {
            inner.running.remove(&name);
        }
        drop(inner);
        r
    });
    Ok(task)
}

/// One `backup-policy` task.
struct Run<'a> {
    st: &'a Arc<AppState>,
    b: &'a Arc<BackupState>,
    h: &'a TaskHandle,
    config: PolicyConfig,
    tz: Tz,
    trigger: RunTrigger,
    scheduled_for: Option<DateTime<Utc>>,
    started: DateTime<Utc>,
}

impl Run<'_> {
    /// Whether the policy is still there and enabled (a manual run of a disabled
    /// policy runs anyway).
    fn still_enabled(&self) -> bool {
        self.b
            .registry
            .policies
            .read()
            .get(&self.config.name)
            .is_some_and(|e| e.config.enabled || self.trigger == RunTrigger::Manual)
    }

    fn run(&self) -> anyhow::Result<String> {
        let p = &self.config;
        let pols = &self.b.policies;
        let engine = pols.engine();
        let run_id = Uuid::new_v4();
        let at = self.scheduled_for.unwrap_or(self.started);
        tracing::info!(target: "sparkles::backup", policy = p.name.as_str(), run = %run_id,
            trigger = ?self.trigger, "policy run started");
        let mut run = PolicyRun {
            id: run_id.to_string(),
            policy: p.name.clone(),
            trigger: self.trigger,
            scheduled_for: self.scheduled_for.map(fmt_time),
            started: fmt_time(self.started),
            finished: None,
            result: RunResult::Ok,
            datasets: Vec::new(),
            retention: None,
            gc: None,
        };
        let selected: Vec<DatasetInfo> = engine
            .datasets(self.st)
            .into_iter()
            .filter(|d| p.datasets.iter().any(|g| crate::auth::glob(g, &d.name)))
            .collect();
        // this policy's latest backup per dataset id, for skipUnchanged
        let previous: Vec<BackupSummary> = if p.skip_unchanged {
            engine
                .list(self.st, &p.repository, &p.name)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let (mut cancelled, mut disabled) = (false, false);
        for (i, ds) in selected.iter().enumerate() {
            let skip = |why: &str| PolicyRunDataset {
                dataset: ds.name.clone(),
                backup: None,
                result: DatasetRunResult::Skipped,
                reason: Some(why.to_string()),
                added_bytes: None,
                millis: None,
            };
            if !cancelled && self.h.is_cancelled() {
                cancelled = true;
            }
            if !cancelled && !disabled && !self.still_enabled() {
                disabled = true;
            }
            if cancelled {
                run.datasets.push(skip("cancelled"));
                continue;
            }
            if disabled {
                run.datasets.push(skip("policy disabled"));
                continue;
            }
            self.h.progress(
                i as f32 / selected.len() as f32 * 0.9,
                &format!("dataset {}/{}: {}", i + 1, selected.len(), ds.name),
            );
            if ds.in_memory {
                run.datasets.push(skip("in-memory dataset"));
                continue;
            }
            let unchanged = previous
                .iter()
                .filter(|b| b.dataset.id == ds.id)
                .max_by(|a, b| a.completed.cmp(&b.completed))
                .is_some_and(|b| b.commit.seq == ds.head);
            if unchanged {
                run.datasets.push(skip("unchanged"));
                continue;
            }
            run.datasets.push(self.backup(&*engine, ds, run_id, at));
        }
        let (ok, failed) = run
            .datasets
            .iter()
            .fold((0, 0), |(o, f), d| match d.result {
                DatasetRunResult::Ok => (o + 1, f),
                DatasetRunResult::Failed => (o, f + 1),
                DatasetRunResult::Skipped => (o, f),
            });
        run.result = if disabled {
            RunResult::Skipped
        } else if failed == 0 && !cancelled {
            RunResult::Ok
        } else if ok == 0 {
            RunResult::Failed
        } else {
            RunResult::Partial
        };
        if !cancelled && !disabled {
            self.h.progress(0.95, "retention");
            run.retention = Some(self.retention(&*engine));
            if p.gc_after_retention {
                run.gc = self.gc(&*engine);
            }
        }
        let finished = pols.now();
        run.finished = Some(fmt_time(finished));
        if matches!(run.result, RunResult::Failed | RunResult::Partial) {
            tracing::warn!(target: "sparkles::backup", policy = p.name.as_str(), run = %run_id,
                result = result_str(run.result), failed, ok, "policy run failed");
        } else {
            tracing::info!(target: "sparkles::backup", policy = p.name.as_str(), run = %run_id,
                result = result_str(run.result), ok, "policy run finished");
        }
        {
            let mut inner = pols.inner.lock();
            let s = inner.state.policies.entry(p.name.clone()).or_default();
            match run.result {
                RunResult::Ok => {
                    s.last_success = Some(fmt_time(finished));
                    s.consecutive_failures = 0;
                }
                RunResult::Partial | RunResult::Failed => s.consecutive_failures += 1,
                RunResult::Skipped => {}
            }
            pols.save_state(&inner);
            pols.record(&mut inner, run.clone());
        }
        self.h.set_detail(serde_json::to_value(&run)?);
        if cancelled {
            return Err(BackupError::cancelled().into());
        }
        Ok(format!(
            "{ok}/{} datasets backed up{}",
            run.datasets.len(),
            match &run.retention {
                Some(r) if !r.deleted.is_empty() => format!(", {} expired", r.deleted.len()),
                _ => String::new(),
            }
        ))
    }

    /// Back up one dataset under the rendered name, appending `-2`, `-3`, … while the
    /// name is taken.
    fn backup(
        &self,
        engine: &dyn Engine,
        ds: &DatasetInfo,
        run_id: Uuid,
        at: DateTime<Utc>,
    ) -> PolicyRunDataset {
        let p = &self.config;
        let t0 = Instant::now();
        let failed = |e: &BackupError| {
            tracing::warn!(target: "sparkles::backup", policy = p.name.as_str(),
                dataset = ds.name.as_str(), code = e.code().as_str(), "policy backup failed: {}", e.message());
            PolicyRunDataset {
                dataset: ds.name.clone(),
                backup: None,
                result: DatasetRunResult::Failed,
                reason: Some(e.message().to_string()),
                added_bytes: None,
                millis: Some(t0.elapsed().as_millis() as u64),
            }
        };
        let base = match policy::render_name(
            &p.name_template,
            &NameCtx {
                policy: &p.name,
                dataset: &ds.name,
                seq: ds.head,
                run: run_id,
                time: at,
                tz: self.tz,
            },
        ) {
            Ok(n) => n,
            Err(e) => return failed(&e),
        };
        let mut k = 1;
        loop {
            let name = if k == 1 {
                base.clone()
            } else {
                let suffix = format!("-{k}");
                let keep = base.len().min(64 - suffix.len());
                format!("{}{suffix}", &base[..keep])
            };
            let o = CreateOptions {
                name: name.clone(),
                note: None,
                policy: Some((p.name.clone(), run_id.to_string())),
                dataset_name: ds.name.clone(),
                extra: Vec::new(),
                ctl: Ctl::with_cancel(self.h.cancel_flag()),
            };
            match engine.create(self.st, &p.repository, &ds.name, o) {
                Ok(s) => {
                    return PolicyRunDataset {
                        dataset: ds.name.clone(),
                        backup: Some(s.name),
                        result: DatasetRunResult::Ok,
                        reason: None,
                        added_bytes: Some(s.added_bytes),
                        millis: Some(t0.elapsed().as_millis() as u64),
                    };
                }
                Err(e) if e.code() == Code::BackupExists && k < 100 => k += 1,
                Err(e) => return failed(&e),
            }
        }
    }

    fn retention(&self, engine: &dyn Engine) -> RunRetention {
        match apply_retention(self.st, engine, &self.config, false) {
            Ok(r) => RunRetention {
                deleted: r.delete.into_iter().map(|b| b.name).collect(),
                error: r.errors.map(|e| e.join("; ")),
            },
            Err(e) => {
                tracing::warn!(target: "sparkles::backup", policy = self.config.name.as_str(),
                    "policy retention failed: {}", e.message());
                RunRetention {
                    deleted: Vec::new(),
                    error: Some(e.message().to_string()),
                }
            }
        }
    }

    /// Start GC unless this repository was collected after retention in the last 24 h.
    fn gc(&self, engine: &dyn Engine) -> Option<RunGc> {
        let pols = &self.b.policies;
        let repo = &self.config.repository;
        let now = pols.now();
        {
            let inner = pols.inner.lock();
            let recent = inner
                .state
                .gc
                .get(repo)
                .and_then(|t| parse_time(t))
                .is_some_and(|t| now - t < GC_INTERVAL && t <= now);
            if recent {
                return None;
            }
        }
        match engine.start_gc(self.st, repo) {
            Ok(task) => {
                let mut inner = pols.inner.lock();
                inner.state.gc.insert(repo.clone(), fmt_time(now));
                pols.save_state(&inner);
                Some(RunGc { task })
            }
            Err(e) => {
                tracing::warn!(target: "sparkles::backup", repository = repo.as_str(),
                    "gc after retention not started: {}", e.message());
                None
            }
        }
    }
}

/// Retention of policy `p` now: the plan, and (unless `dry_run`) its deletions. A
/// deletion that fails keeps the backup in `delete` and adds to `errors`.
fn apply_retention(
    st: &Arc<AppState>,
    engine: &dyn Engine,
    p: &PolicyConfig,
    dry_run: bool,
) -> Result<RetentionResponse, BackupError> {
    let b = backup_state(st)?;
    let list = engine.list(st, &p.repository, &p.name)?;
    let busy = engine.busy(st, &p.repository);
    let plan = policy::retention(&list, &p.name, &p.retention, b.policies.now(), &busy);
    let mut errors = Vec::new();
    let mut deleted = Vec::new();
    for s in plan.delete {
        if dry_run {
            deleted.push(s);
            continue;
        }
        match engine.delete(st, &p.repository, &s.name) {
            Ok(_) => deleted.push(s),
            Err(e) => errors.push(format!("{}: {}", s.name, e.message())),
        }
    }
    if !dry_run && !deleted.is_empty() {
        tracing::info!(target: "sparkles::backup", policy = p.name.as_str(),
            deleted = deleted.len(), "policy retention");
    }
    Ok(RetentionResponse {
        dry_run,
        delete: deleted,
        keep: plan.keep,
        errors: (!errors.is_empty()).then_some(errors),
    })
}

// ----------------------------------------------------------------------- metrics ------

/// The policy metric families (`sparkles_backup_policy_*`), appended by
/// `backup::metrics::render`.
pub fn render_metrics(st: &AppState, out: &mut String) {
    use std::fmt::Write as _;
    let Some(b) = &st.backup else { return };
    let views: Vec<Policy> = b
        .registry
        .policies
        .read()
        .values()
        .map(|e| b.policies.view(e))
        .collect();
    let counts = b.policies.inner.lock().counts.clone();
    if views.is_empty() && counts.is_empty() {
        return;
    }
    let esc = |v: &str| {
        v.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    };
    let secs = |t: &Option<String>| t.as_deref().and_then(parse_time).map(|t| t.timestamp());
    let family = |o: &mut String, name: &str, kind: &str, help: &str| {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} {kind}");
    };
    family(
        out,
        "sparkles_backup_policy_runs_total",
        "counter",
        "Backup policy runs since the server started, by result.",
    );
    for ((p, r), n) in &counts {
        let _ = writeln!(
            out,
            "sparkles_backup_policy_runs_total{{policy=\"{}\",result=\"{r}\"}} {n}",
            esc(p)
        );
    }
    family(
        out,
        "sparkles_backup_policy_last_success_timestamp_seconds",
        "gauge",
        "When the policy's last successful run finished (Unix seconds).",
    );
    for v in &views {
        if let Some(t) = secs(&v.state.last_success) {
            let _ = writeln!(
                out,
                "sparkles_backup_policy_last_success_timestamp_seconds{{policy=\"{}\"}} {t}",
                esc(&v.config.name)
            );
        }
    }
    family(
        out,
        "sparkles_backup_policy_next_run_timestamp_seconds",
        "gauge",
        "The policy's next scheduled run (Unix seconds).",
    );
    for v in &views {
        if let Some(t) = secs(&v.state.next_run) {
            let _ = writeln!(
                out,
                "sparkles_backup_policy_next_run_timestamp_seconds{{policy=\"{}\"}} {t}",
                esc(&v.config.name)
            );
        }
    }
    family(
        out,
        "sparkles_backup_policy_consecutive_failures",
        "gauge",
        "Failed or partial runs of the policy since its last successful one.",
    );
    for v in &views {
        let _ = writeln!(
            out,
            "sparkles_backup_policy_consecutive_failures{{policy=\"{}\"}} {}",
            esc(&v.config.name),
            v.state.consecutive_failures
        );
    }
}

// ------------------------------------------------------------------------ routes ------

/// The routes, merged by `backup::http::routes`.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/backup-policies", get(list_policies).post(create_policy))
        .route("/$/backup-policies/preview", post(preview))
        .route(
            "/$/backup-policies/{policy}",
            get(get_policy).put(put_policy).delete(remove_policy),
        )
        .route("/$/backup-policies/{policy}/run", post(run_policy))
        .route("/$/backup-policies/{policy}/retention", post(retention))
        .route("/$/backup-policies/{policy}/runs", get(list_runs))
}

type St = State<Arc<AppState>>;
type Res = Result<Response, ApiErr>;

/// An error response (boxed: responses are large).
struct ApiErr(Box<Response>);

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        *self.0
    }
}

fn disabled() -> BackupError {
    BackupError::new(
        Code::NotImplemented,
        "backup repositories are not enabled on this server",
    )
}

fn no_such_policy(name: &str) -> BackupError {
    BackupError::new(Code::NoSuchPolicy, format!("no such policy: {name}"))
}

fn fail(e: BackupError) -> ApiErr {
    ApiErr(Box::new(error_response(&e)))
}

fn backups(st: &AppState) -> Result<Arc<BackupState>, ApiErr> {
    st.backup.clone().ok_or_else(|| fail(disabled()))
}

fn writable(st: &AppState) -> Result<(), ApiErr> {
    if st.read_only {
        return Err(fail(BackupError::new(
            Code::ServerReadOnly,
            "server is read-only",
        )));
    }
    Ok(())
}

fn body<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, ApiErr> {
    serde_json::from_slice(bytes).map_err(|e| {
        fail(BackupError::new(
            Code::InvalidRequest,
            format!("invalid request body: {e}"),
        ))
    })
}

fn entry(b: &BackupState, name: &str) -> Result<PolicyEntry, ApiErr> {
    b.registry
        .policies
        .read()
        .get(name)
        .map(|e| PolicyEntry {
            config: e.config.clone(),
            source: e.source,
        })
        .ok_or_else(|| fail(no_such_policy(name)))
}

fn audit(name: &str, action: &str) {
    tracing::info!(target: "sparkles::audit", event = "policy_changed", policy = name, action);
}

/// Check a policy body against the registry: `check_policy`, then its repository
/// (registered, writable). An empty `datasets` means all (`["*"]`).
fn validate(b: &BackupState, p: &mut PolicyConfig) -> Result<(Schedule, Tz), ApiErr> {
    if p.datasets.is_empty() {
        p.datasets = vec!["*".into()];
    }
    let checked = policy::check_policy(p).map_err(fail)?;
    match b.registry.repos.read().get(&p.repository) {
        None => Err(fail(BackupError::new(
            Code::NoSuchRepository,
            format!("no such repository: {}", p.repository),
        ))),
        Some(r) if r.config.readonly => Err(fail(BackupError::new(
            Code::RepositoryReadOnly,
            format!("repository {} is read-only", p.repository),
        ))),
        Some(_) => Ok(checked),
    }
}

fn save(b: &BackupState) -> Result<(), ApiErr> {
    write_api_policies(&b.dir, &b.registry).map_err(|e| {
        fail(BackupError::internal(format!(
            "saving the backup policies: {e:#}"
        )))
    })
}

/// `GET /$/backup-policies` → `PolicyList`
async fn list_policies(State(st): St) -> Res {
    let b = backups(&st)?;
    let policies = b
        .registry
        .policies
        .read()
        .values()
        .map(|e| b.policies.view(e))
        .collect();
    Ok(Json(PolicyList { policies }).into_response())
}

/// `POST /$/backup-policies` (body `PolicyConfig`) → `201` + `Policy`
async fn create_policy(State(st): St, bytes: Bytes) -> Res {
    let b = backups(&st)?;
    writable(&st)?;
    let mut p: PolicyConfig = body(&bytes)?;
    let (s, tz) = validate(&b, &mut p)?;
    let name = p.name.clone();
    {
        let mut map = b.registry.policies.write();
        if map.contains_key(&name) || b.registry.repos.read().contains_key(&name) {
            return Err(fail(BackupError::new(
                Code::PolicyExists,
                format!("a policy or repository named {name} exists"),
            )));
        }
        map.insert(
            name.clone(),
            PolicyEntry {
                config: p,
                source: ConfigSource::Api,
            },
        );
    }
    if let Err(r) = save(&b) {
        b.registry.policies.write().remove(&name);
        return Err(r);
    }
    b.policies.reset_schedule(&name, &s, tz);
    audit(&name, "created");
    let view = b.policies.view(&entry(&b, &name)?);
    let mut r = (StatusCode::CREATED, Json(view)).into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("/$/backup-policies/{name}")) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    Ok(r)
}

/// `POST /$/backup-policies/preview` (body `PreviewRequest`) → `PreviewResponse`;
/// `400 invalid-schedule` (also for an unknown time zone)
async fn preview(State(st): St, bytes: Bytes) -> Res {
    let b = backups(&st)?;
    let req: PreviewRequest = body(&bytes)?;
    let tz = policy::parse_timezone(&req.timezone).map_err(fail)?;
    let s = policy::parse_schedule(&req.schedule).map_err(fail)?;
    let now = b.policies.now();
    let next = policy::next_runs(&s, tz, now, req.count.unwrap_or(5).clamp(1, 20));
    let sample = match &req.name_template {
        Some(t) if !t.is_empty() => Some(
            policy::render_name(
                t,
                &NameCtx {
                    policy: "policy",
                    dataset: req.dataset.as_deref().unwrap_or("dataset"),
                    seq: 42,
                    run: Uuid::new_v4(),
                    time: next.first().copied().unwrap_or(now),
                    tz,
                },
            )
            .map_err(fail)?,
        ),
        _ => None,
    };
    Ok(Json(PreviewResponse {
        next: next.into_iter().map(fmt_time).collect(),
        description: policy::describe(&s, tz),
        sample,
    })
    .into_response())
}

/// `GET /$/backup-policies/{policy}` → `Policy`
async fn get_policy(State(st): St, Path(name): Path<String>) -> Res {
    let b = backups(&st)?;
    let e = entry(&b, &name)?;
    Ok(Json(b.policies.view(&e)).into_response())
}

/// `PUT /$/backup-policies/{policy}` (body `PolicyConfig`, the name may be left out) →
/// `Policy`; `409 read-only-config` for a config-file policy
async fn put_policy(State(st): St, Path(name): Path<String>, bytes: Bytes) -> Res {
    let b = backups(&st)?;
    writable(&st)?;
    let old = entry(&b, &name)?;
    if old.source == ConfigSource::Config {
        return Err(fail(BackupError::new(
            Code::ReadOnlyConfig,
            format!("policy {name} comes from the config file; change it there"),
        )));
    }
    let mut v: serde_json::Value = body(&bytes)?;
    if let Some(o) = v.as_object_mut() {
        match o.get("name") {
            Some(n) if n.as_str() != Some(name.as_str()) => {
                return Err(fail(BackupError::new(
                    Code::InvalidName,
                    "a policy's name cannot change",
                )));
            }
            _ => {
                o.insert("name".into(), name.clone().into());
            }
        }
    }
    let mut p: PolicyConfig = serde_json::from_value(v).map_err(|e| {
        fail(BackupError::new(
            Code::InvalidRequest,
            format!("invalid request body: {e}"),
        ))
    })?;
    let (s, tz) = validate(&b, &mut p)?;
    let rescheduled = p.schedule != old.config.schedule || p.timezone != old.config.timezone;
    {
        let mut map = b.registry.policies.write();
        match map.get_mut(&name) {
            Some(e) => e.config = p,
            None => return Err(fail(no_such_policy(&name))),
        }
    }
    if let Err(r) = save(&b) {
        if let Some(e) = b.registry.policies.write().get_mut(&name) {
            e.config = old.config;
        }
        return Err(r);
    }
    if rescheduled {
        b.policies.reset_schedule(&name, &s, tz);
    }
    audit(&name, "changed");
    Ok(Json(b.policies.view(&entry(&b, &name)?)).into_response())
}

/// `DELETE /$/backup-policies/{policy}` → `204` (a running run stops before its next
/// dataset)
async fn remove_policy(State(st): St, Path(name): Path<String>) -> Res {
    let b = backups(&st)?;
    writable(&st)?;
    let old = entry(&b, &name)?;
    if old.source == ConfigSource::Config {
        return Err(fail(BackupError::new(
            Code::ReadOnlyConfig,
            format!("policy {name} comes from the config file; remove it there"),
        )));
    }
    b.registry.policies.write().remove(&name);
    if let Err(r) = save(&b) {
        b.registry.policies.write().insert(name, old);
        return Err(r);
    }
    b.policies.forget(&name);
    audit(&name, "removed");
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /$/backup-policies/{policy}/run` → `202` task `backup-policy` (server-scoped),
/// `detail: PolicyRun`; the schedule does not move. `409 policy-running` (with `task`)
async fn run_policy(State(st): St, Path(name): Path<String>) -> Res {
    let b = backups(&st)?;
    let e = entry(&b, &name)?;
    match b.registry.repos.read().get(&e.config.repository) {
        None => {
            return Err(fail(BackupError::new(
                Code::NoSuchRepository,
                format!("no such repository: {}", e.config.repository),
            )));
        }
        Some(r) if r.config.readonly => {
            return Err(fail(BackupError::new(
                Code::RepositoryReadOnly,
                format!("repository {} is read-only", e.config.repository),
            )));
        }
        Some(_) => {}
    }
    let task = start_run(&st, name.clone(), RunTrigger::Manual, None)
        .map_err(|e| ApiErr(Box::new(e.response(&name))))?;
    let mut r = (StatusCode::ACCEPTED, Json(&task)).into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("/$/tasks/{}", task.id)) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    Ok(r)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetentionQuery {
    #[serde(default)]
    dry_run: Option<String>,
}

/// `POST /$/backup-policies/{policy}/retention[?dryRun=true]` → `RetentionResponse`
async fn retention(
    State(st): St,
    Path(name): Path<String>,
    Query(q): Query<RetentionQuery>,
) -> Res {
    let b = backups(&st)?;
    let e = entry(&b, &name)?;
    let dry_run = matches!(q.dry_run.as_deref(), Some("true" | "1" | ""));
    if !dry_run
        && b.registry
            .repos
            .read()
            .get(&e.config.repository)
            .is_some_and(|r| r.config.readonly)
    {
        return Err(fail(BackupError::new(
            Code::RepositoryReadOnly,
            format!("repository {} is read-only", e.config.repository),
        )));
    }
    let engine = b.policies.engine();
    let r = tokio::task::spawn_blocking(move || apply_retention(&st, &*engine, &e.config, dry_run))
        .await
        .map_err(|e| fail(BackupError::internal(e.to_string())))?
        .map_err(fail)?;
    Ok(Json(r).into_response())
}

#[derive(Deserialize)]
struct RunsQuery {
    #[serde(default)]
    limit: Option<usize>,
}

/// `GET /$/backup-policies/{policy}/runs[?limit=]` → `PolicyRunList`, newest first
async fn list_runs(State(st): St, Path(name): Path<String>, Query(q): Query<RunsQuery>) -> Res {
    let b = backups(&st)?;
    entry(&b, &name)?;
    let runs = b
        .policies
        .runs(&name, q.limit.unwrap_or(50).clamp(1, MAX_RUNS));
    Ok(Json(PolicyRunList { runs }).into_response())
}

#[cfg(test)]
#[path = "policies_tests.rs"]
mod tests;
