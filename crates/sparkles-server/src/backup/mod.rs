//! Backup repositories on the server: the registry of repositories and policies
//! (`<data>/backup/repositories.json`, `policies.json`, and `serve --backup-config`),
//! the task slots of backup, restore, verify and GC tasks, the HTTP routes
//! (`/$/repositories`, `/$/backups/{ds}`, `/$/backup-policies`), the policy scheduler,
//! in-place restores, startup recovery, metrics, and the `sparkles repo` and
//! `sparkles backup` subcommands. The repository engine is the `sparkles-backup` crate.
//!
//! Server tasks run on std threads (as every task) and drive the async engine with
//! `Handle::block_on` on the server's runtime ([`BackupState::handle`]).
//!
//! What the policy scheduler builds on:
//! * [`BackupState::registry`]: the policies (`Registry::policies`) and the
//!   repositories;
//! * [`ops::create_for_policy`] (the one-backup-per-dataset-and-repository rule and
//!   a task slot per dataset), [`ops::delete`], [`ops::start_gc`] (the GC task of
//!   `POST /$/repositories/{repo}/gc`) and [`BackupState::busy_backups`] (what
//!   retention must keep).

pub mod cli;
pub mod config;
pub mod http;
pub mod metrics;
pub mod ops;
pub mod policies;
pub mod recover;
pub mod registry;
// the scheduler's clock is used once it schedules
#[allow(dead_code)]
pub mod scheduler;
pub mod swap;
#[cfg(test)]
mod tests;

use crate::state::{AppState, TaskHandle, task_state};
use anyhow::Context;
use parking_lot::{Condvar, Mutex, RwLock};
use sparkles::outbound::OutboundPolicy;
use sparkles_backup::object_store::ObjectStore;
pub use sparkles_backup::{BackupError, Repository};
use sparkles_backup::{Code, ConfigSource, Credentials, OpenEnv, RepoConfig, RepoType};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::runtime::Handle;

/// Told once a task has either started or queued for a slot (the HTTP handler answers
/// with the task in that state).
pub type Started = tokio::sync::oneshot::Sender<()>;

/// Everything the server keeps about backups (`AppState::backup`).
pub struct BackupState {
    /// `<data>/backup`: `repositories.json`, `policies.json`, `policy-state.json`,
    /// `runs.json`, `verify.json`, `cache/<repository id>/`
    pub dir: PathBuf,
    /// the server's data directory (`fs` repositories may not lie inside it)
    pub data_dir: PathBuf,
    /// directories no `fs` repository may lie in: the data directory, and the
    /// directories of the server's config files
    pub forbid: Vec<PathBuf>,
    /// where repositories registered through the API may connect (the server's
    /// `--outbound-*` policy, set by [`start`])
    pub outbound: RwLock<OutboundPolicy>,
    /// backup tasks admitted and not finished (running or waiting for a slot)
    admitted: Mutex<usize>,
    /// `serve --backup-config`
    pub config_path: Option<PathBuf>,
    /// `serve --backup-max-tasks`: backup, restore, verify and GC tasks running at once
    pub max_tasks: usize,
    /// the server's runtime, set by [`start`] once it exists
    handle: OnceLock<Handle>,
    pub registry: registry::Registry,
    /// the `--backup-max-tasks` slots
    pub slots: Slots,
    claims: Mutex<Claims>,
    /// the last verification of each backup on this server (`verify.json`)
    pub verified: registry::VerifyHistory,
    pub metrics: metrics::BackupMetrics,
    /// stores to use instead of building one from a repository's configuration, by
    /// repository name (tests: a shared `InMemory`, or a store that fails on purpose)
    pub stores: Mutex<BTreeMap<String, Arc<dyn ObjectStore>>>,
    /// `holder.server` of this server's locks: a hash of the data directory
    server_id: String,
    /// policy scheduler state, run history, and the running policy tasks
    pub policies: policies::Policies,
}

impl BackupState {
    /// Load the registry (`<data>/backup/*.json`) and the config file. A config file
    /// that does not parse or validate stops the server (errors name the line and
    /// column). Does not touch any repository (they are opened lazily, and reachability
    /// is checked in the background by [`start`]).
    pub fn new(
        data_dir: &Path,
        config_path: Option<PathBuf>,
        max_tasks: usize,
    ) -> anyhow::Result<BackupState> {
        let dir = data_dir.join("backup");
        let file = match &config_path {
            Some(p) => Some(config::load(p)?),
            None => None,
        };
        if let (Some(f), Some(p)) = (&file, &config_path) {
            validate_file(f, data_dir).with_context(|| format!("backup config {}", p.display()))?;
        }
        let registry = registry::Registry::load(&dir, file.as_ref())?;
        let max_tasks = max_tasks.max(1);
        let policies = policies::Policies::load(&dir, &registry)?;
        let mut forbid = vec![data_dir.to_path_buf()];
        if let Some(d) = config_path.as_deref().and_then(config_dir) {
            forbid.push(d);
        }
        Ok(BackupState {
            verified: registry::VerifyHistory::load(dir.join(registry::VERIFY_FILE)),
            dir,
            data_dir: data_dir.to_path_buf(),
            forbid,
            outbound: RwLock::new(OutboundPolicy::default()),
            admitted: Mutex::new(0),
            config_path,
            max_tasks,
            handle: OnceLock::new(),
            registry,
            slots: Slots::new(max_tasks),
            claims: Mutex::new(Claims::default()),
            metrics: metrics::BackupMetrics::default(),
            stores: Mutex::new(BTreeMap::new()),
            server_id: server_id(data_dir),
            policies,
        })
    }

    /// The server's runtime (`None` before [`start`], e.g. in router tests that did not
    /// call it).
    pub fn handle(&self) -> Option<Handle> {
        self.handle.get().cloned()
    }

    /// Run `f` to completion on the server's runtime, from a task thread (before
    /// [`start`], on a runtime of its own: tests and tools).
    pub fn block_on<F: std::future::Future>(&self, f: F) -> Result<F::Output, BackupError> {
        let Some(h) = self.handle() else {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| BackupError::internal(format!("a runtime for backups: {e}")))?;
            return Ok(rt.block_on(f));
        };
        Ok(match Handle::try_current() {
            // a runtime thread (a test calling a task body directly) may not block on
            // its own runtime
            Ok(_) => tokio::task::block_in_place(|| h.block_on(f)),
            Err(_) => h.block_on(f),
        })
    }

    /// The opened repository registered as `name`: `404 no-such-repository`; a
    /// repository that cannot be opened is `502 repository-unavailable` (and marked
    /// unreachable). Opening is lazy and cached; a config reload drops the cache of
    /// changed entries. Blocks: for task threads (handlers use [`open_repo`](Self::open_repo)).
    pub fn repo(&self, name: &str) -> Result<Arc<Repository>, BackupError> {
        if let Some(r) = self.registry.repo(name)? {
            return Ok(r);
        }
        self.block_on(self.open_repo(name))?
    }

    /// Also keep `fs` repositories out of the directory of config file `file` (the
    /// auth config).
    pub fn forbid_config_dir(&mut self, file: &Path) {
        if let Some(d) = config_dir(file) {
            self.forbid.push(d);
        }
    }

    /// How repository `cfg` is opened on this server; `init` creates the marker of an
    /// empty location. Repositories registered through the API connect only where the
    /// server's outbound policy allows.
    pub fn open_env(&self, cfg: &RepoConfig, source: ConfigSource, init: bool) -> OpenEnv {
        OpenEnv {
            cache_dir: Some(self.dir.join("cache")),
            forbid_under: self.forbid.clone(),
            init,
            server_id: self.server_id.clone(),
            store: self.stores.lock().get(&cfg.name).cloned(),
            outbound: (source == ConfigSource::Api).then(|| self.outbound.read().clone()),
            ..Default::default()
        }
    }

    /// The configuration to open repository `cfg` with: checked against what the
    /// config file allows API registrations ([`check_api`](Self::check_api)), and
    /// with a named credential source replaced by its definition (`400 invalid-config`
    /// if there is none).
    pub fn prepare(
        &self,
        cfg: &RepoConfig,
        source: ConfigSource,
    ) -> Result<RepoConfig, BackupError> {
        if source == ConfigSource::Api {
            self.check_api(cfg)?;
        }
        let mut cfg = cfg.clone();
        if let Credentials::Named { name } = &cfg.credentials {
            cfg.credentials = self
                .registry
                .api
                .read()
                .credentials
                .get(name)
                .cloned()
                .ok_or_else(|| no_credential_source(name))?;
        }
        Ok(cfg)
    }

    /// What a repository registered through the API may be: `fs` under one of
    /// `[api] fs_roots` (when set); `s3` with a credential source the config file
    /// names, never environment variables, files or the default provider chain of the
    /// caller's choosing (they would send the server's secrets, or read its files, for
    /// an endpoint the caller picked); not `gcs` or `azure`, which use the server's
    /// ambient credentials (config file only). `400 invalid-config` naming the field.
    pub fn check_api(&self, cfg: &RepoConfig) -> Result<(), BackupError> {
        let api = self.registry.api.read();
        let refuse = |field: &str, msg: String| {
            Err(
                BackupError::new(Code::InvalidConfig, format!("{field}: {msg}"))
                    .with("field", field),
            )
        };
        match cfg.kind {
            RepoType::Fs => {
                let path = Path::new(cfg.path.as_deref().unwrap_or(""));
                if !api.fs_roots.is_empty()
                    && !api
                        .fs_roots
                        .iter()
                        .any(|root| sparkles_backup::repo::is_within(path, root))
                {
                    let roots: Vec<String> = api
                        .fs_roots
                        .iter()
                        .map(|r| r.display().to_string())
                        .collect();
                    return refuse(
                        "path",
                        format!(
                            "repositories registered through the API must lie under {}",
                            roots.join(" or ")
                        ),
                    );
                }
            }
            RepoType::S3 => match &cfg.credentials {
                Credentials::Named { name } if api.credentials.contains_key(name) => {}
                Credentials::Named { name } => return Err(no_credential_source(name)),
                _ => {
                    return refuse(
                        "credentials",
                        "repositories registered through the API use a credential source \
                         defined in the server's backup config file: {\"source\": \"named\", \
                         \"name\": …}"
                            .into(),
                    );
                }
            },
            RepoType::Gcs | RepoType::Azure => {
                return refuse(
                    "type",
                    format!(
                        "{} repositories use the server's own credentials and are defined in its backup config file",
                        cfg.kind.as_str()
                    ),
                );
            }
            RepoType::Memory => {}
        }
        Ok(())
    }

    /// Admit a backup task (create, restore, verify or GC) started by a request: at
    /// most [`QUEUE_PER_SLOT`] wait per `--backup-max-tasks` slot, beyond those that
    /// run; `503 too-many-tasks` otherwise. Move the admission into the task.
    pub fn admit(self: &Arc<Self>) -> Result<Admission, BackupError> {
        let mut n = self.admitted.lock();
        let limit = self.max_tasks * (1 + QUEUE_PER_SLOT);
        if *n >= limit {
            return Err(BackupError::new(
                Code::TooManyTasks,
                format!("{limit} backup tasks run or wait already; try again later"),
            ));
        }
        *n += 1;
        Ok(Admission { b: self.clone() })
    }

    /// Open (or return the opened) repository `name`, updating its status. A
    /// registered repository is initialized only if its id was never seen (so a
    /// location that lost its marker is reported, not re-created), and its marker must
    /// keep the id first seen.
    pub async fn open_repo(&self, name: &str) -> Result<Arc<Repository>, BackupError> {
        let (cfg, known, source) = {
            let repos = self.registry.repos.read();
            let e = repos
                .get(name)
                .ok_or_else(|| registry::no_such_repository(name))?;
            if let Some(r) = &e.opened {
                return Ok(r.clone());
            }
            (e.config.clone(), e.id, e.source)
        };
        let env = self.open_env(&cfg, source, known.is_none() && !cfg.readonly);
        let opened = match self.prepare(&cfg, source) {
            Ok(c) => Repository::open(&c, &env).await,
            Err(e) => Err(e),
        };
        let r = match opened {
            Ok(r) if known.is_some_and(|k| k != r.id()) => Err(BackupError::new(
                Code::RepositoryUnavailable,
                format!(
                    "{} now holds repository {}, not {} as registered",
                    cfg.location(),
                    r.id(),
                    known.unwrap_or_default()
                ),
            )),
            r => r,
        };
        let r = r.map(Arc::new);
        let learned = self.registry.update(name, |e| {
            // a change of the settings in the meantime wins
            if e.config != cfg {
                return false;
            }
            match &r {
                Ok(repo) => {
                    let new = e.id.is_none();
                    e.id = Some(repo.id());
                    e.opened = Some(repo.clone());
                    e.mark_reachable(None);
                    new && e.source == sparkles_backup::ConfigSource::Api
                }
                Err(err) => {
                    e.mark_unreachable(err.message());
                    false
                }
            }
        });
        if learned == Some(true)
            && let Err(e) = self.registry.save_repositories(&self.dir)
        {
            tracing::warn!(target: "sparkles::backup", "saving the repository registry: {e:#}");
        }
        r
    }

    /// Re-read what is known about repository `name`: reachability, totals (`stats`)
    /// and the last GC (`gc/last.json`). Problems are recorded, not returned.
    pub async fn refresh(&self, name: &str) {
        let Ok(repo) = self.open_repo(name).await else {
            return;
        };
        let stats = repo.stats().await;
        let last_gc = repo.last_gc().await;
        self.registry.update(name, |e| {
            match stats {
                Ok(s) => e.stats = Some(s),
                Err(err) if err.code() == Code::RepositoryUnavailable => {
                    e.mark_unreachable(err.message())
                }
                Err(err) => {
                    tracing::debug!(target: "sparkles::backup", "statistics of {name}: {err}")
                }
            }
            match last_gc {
                Ok(Some(g)) => e.last_gc = Some(g),
                Ok(None) => {}
                Err(err) => {
                    tracing::debug!(target: "sparkles::backup", "last GC of {name}: {err}")
                }
            }
        });
    }

    /// [`refresh`](Self::refresh) in the background (a no-op before [`start`]).
    pub fn refresh_later(self: &Arc<Self>, name: &str) {
        if let Some(h) = self.handle() {
            let b = self.clone();
            let name = name.to_string();
            h.spawn(async move { b.refresh(&name).await });
        }
    }

    /// Re-read `--backup-config` (SIGHUP): the new file's repositories and policies
    /// replace the old ones; API entries stay. A file that does not load leaves
    /// everything as it was.
    pub fn reload(&self) -> anyhow::Result<()> {
        let Some(p) = &self.config_path else {
            return Ok(());
        };
        let f = config::load(p)?;
        validate_file(&f, &self.data_dir)
            .with_context(|| format!("backup config {}", p.display()))?;
        self.registry.replace_config(&f)
    }

    // ------------------------------------------------------------- claims ------

    /// Claim what task `task` works on, all or nothing: `409 backup-in-progress` (with
    /// `task`) when a backup of the same dataset into the same repository runs,
    /// `409 dataset-busy` when a restore into `c.target` runs. Released when the
    /// returned [`Claim`] is dropped (move it into the task).
    pub fn claim(self: &Arc<Self>, task: &str, c: ClaimSpec) -> Result<Claim, BackupError> {
        let mut claims = self.claims.lock();
        if let Some((ds, repo)) = &c.create
            && let Some(t) = claims.creates.get(&(ds.clone(), repo.clone()))
        {
            return Err(BackupError::new(
                Code::BackupInProgress,
                format!("a backup of /{ds} into {repo} is running (task {t})"),
            )
            .with("task", t.as_str()));
        }
        if let Some(ds) = &c.target
            && let Some(t) = claims.targets.get(ds)
        {
            return Err(BackupError::new(
                Code::DatasetBusy,
                format!("task {t} restores into /{ds}"),
            )
            .with("task", t.as_str()));
        }
        if let Some(k) = &c.create {
            claims.creates.insert(k.clone(), task.to_string());
        }
        if let Some(k) = &c.backup {
            claims
                .backups
                .entry(k.clone())
                .or_default()
                .push(task.to_string());
        }
        if let Some(r) = &c.repo {
            claims
                .repos
                .entry(r.clone())
                .or_default()
                .push(task.to_string());
        }
        if let Some(ds) = &c.target {
            claims.targets.insert(ds.clone(), task.to_string());
        }
        Ok(Claim {
            b: self.clone(),
            task: task.to_string(),
            spec: c,
        })
    }

    /// The task backing up dataset `ds` into repository `repo`, if one runs.
    pub fn create_running(&self, ds: &str, repo: &str) -> Option<String> {
        self.claims
            .lock()
            .creates
            .get(&(ds.to_string(), repo.to_string()))
            .cloned()
    }

    /// A task restoring or verifying backup `backup` of repository `repo`, if one runs
    /// (the backup may not be deleted meanwhile: `409 backup-busy`).
    pub fn backup_busy(&self, repo: &str, backup: &str) -> Option<String> {
        self.claims
            .lock()
            .backups
            .get(&(repo.to_string(), backup.to_string()))
            .and_then(|v| v.first().cloned())
    }

    /// The backups of repository `repo` that a restore or verification uses now
    /// (retention keeps them until its next evaluation).
    #[cfg_attr(not(test), allow(dead_code))] // policy retention
    pub fn busy_backups(&self, repo: &str) -> BTreeSet<String> {
        self.claims
            .lock()
            .backups
            .iter()
            .filter(|((r, _), v)| r == repo && !v.is_empty())
            .map(|((_, b), _)| b.clone())
            .collect()
    }

    /// A task that uses repository `repo`, if one runs.
    pub fn repo_task(&self, repo: &str) -> Option<String> {
        self.claims
            .lock()
            .repos
            .get(repo)
            .and_then(|v| v.first().cloned())
    }

    /// A backup task working on dataset `ds` (backing it up, or restoring into it), if
    /// one runs.
    pub fn dataset_task(&self, ds: &str) -> Option<String> {
        let c = self.claims.lock();
        c.targets.get(ds).cloned().or_else(|| {
            c.creates
                .iter()
                .find(|((d, _), _)| d == ds)
                .map(|(_, t)| t.clone())
        })
    }

    /// The task restoring into dataset `ds`, if one runs.
    pub fn target_busy(&self, ds: &str) -> Option<String> {
        self.claims.lock().targets.get(ds).cloned()
    }
}

/// Backup tasks that may wait per `--backup-max-tasks` slot (see [`BackupState::admit`]).
pub const QUEUE_PER_SLOT: usize = 4;

/// An admitted backup task; released on drop.
pub struct Admission {
    b: Arc<BackupState>,
}

impl Drop for Admission {
    fn drop(&mut self) {
        *self.b.admitted.lock() -= 1;
    }
}

fn no_credential_source(name: &str) -> BackupError {
    BackupError::new(
        Code::InvalidConfig,
        format!(
            "credentials.name: no credential source {name:?} in the server's backup config file"
        ),
    )
    .with("field", "credentials.name")
}

/// The directory of a config file, canonicalized when it exists.
fn config_dir(file: &Path) -> Option<PathBuf> {
    let dir = std::path::absolute(file).ok()?.parent()?.to_path_buf();
    Some(std::fs::canonicalize(&dir).unwrap_or(dir))
}

/// What a task claims (see [`BackupState::claim`]).
#[derive(Clone, Debug, Default)]
pub struct ClaimSpec {
    /// a backup of `(dataset, repository)`: one at a time
    pub create: Option<(String, String)>,
    /// a restore or verification of `(repository, backup)`: the backup is busy
    pub backup: Option<(String, String)>,
    /// the repository it uses (it may not be unregistered meanwhile)
    pub repo: Option<String>,
    /// the dataset a restore creates or replaces: one at a time
    pub target: Option<String>,
}

#[derive(Default)]
struct Claims {
    creates: BTreeMap<(String, String), String>,
    backups: BTreeMap<(String, String), Vec<String>>,
    repos: BTreeMap<String, Vec<String>>,
    targets: BTreeMap<String, String>,
}

/// Held claims of a task; released on drop.
pub struct Claim {
    b: Arc<BackupState>,
    task: String,
    spec: ClaimSpec,
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut c = self.b.claims.lock();
        let task = &self.task;
        if let Some(k) = &self.spec.create
            && c.creates.get(k) == Some(task)
        {
            c.creates.remove(k);
        }
        if let Some(k) = &self.spec.backup
            && let Some(v) = c.backups.get_mut(k)
        {
            v.retain(|t| t != task);
            if v.is_empty() {
                c.backups.remove(k);
            }
        }
        if let Some(k) = &self.spec.repo
            && let Some(v) = c.repos.get_mut(k)
        {
            v.retain(|t| t != task);
            if v.is_empty() {
                c.repos.remove(k);
            }
        }
        if let Some(k) = &self.spec.target
            && c.targets.get(k) == Some(task)
        {
            c.targets.remove(k);
        }
    }
}

// ------------------------------------------------------------------ slots ------

/// The `--backup-max-tasks` slots: backup, restore, verify and GC work holds one while
/// it runs; a task waiting for one shows `queued`.
pub struct Slots {
    max: usize,
    used: Mutex<usize>,
    freed: Condvar,
}

/// A held slot, released on drop.
pub struct Slot<'a> {
    slots: &'a Slots,
}

impl Slots {
    pub fn new(max: usize) -> Slots {
        Slots {
            max: max.max(1),
            used: Mutex::new(0),
            freed: Condvar::new(),
        }
    }

    /// Slots held now.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn in_use(&self) -> usize {
        *self.used.lock()
    }

    /// Take a slot for task `h`, waiting (state `queued`) while all are held;
    /// `started` is told once the task runs or queues. A cancel request while queued
    /// fails with `cancelled`.
    pub fn acquire(
        &self,
        h: &TaskHandle,
        started: Option<Started>,
    ) -> Result<Slot<'_>, BackupError> {
        let mut started = started;
        let waited = self.take(&h.cancel_flag(), || {
            h.set_state(task_state::QUEUED);
            h.progress(0.0, "waiting for a free backup task slot");
            if let Some(s) = started.take() {
                let _ = s.send(());
            }
        })?;
        if waited {
            h.set_state(task_state::RUNNING);
            h.progress(0.0, "starting");
        }
        if let Some(s) = started {
            let _ = s.send(());
        }
        Ok(Slot { slots: self })
    }

    /// Take a slot for work inside a task that keeps its own state (a policy run backs
    /// up its datasets one after another, each in a slot); `cancel` stops the wait.
    pub fn acquire_quietly(&self, cancel: &AtomicBool) -> Result<Slot<'_>, BackupError> {
        self.take(cancel, || {})?;
        Ok(Slot { slots: self })
    }

    /// Take a slot, calling `queued` first if all are held; whether it waited.
    fn take(&self, cancel: &AtomicBool, queued: impl FnOnce()) -> Result<bool, BackupError> {
        let mut used = self.used.lock();
        let waited = *used >= self.max;
        if waited {
            queued();
            while *used >= self.max {
                if cancel.load(Ordering::Relaxed) {
                    return Err(BackupError::cancelled());
                }
                self.freed.wait_for(&mut used, Duration::from_millis(100));
            }
        }
        *used += 1;
        Ok(waited)
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        *self.slots.used.lock() -= 1;
        self.slots.freed.notify_all();
    }
}

// ---------------------------------------------------------------- helpers ------

/// Check every repository of a config file (the checks of an API registration) and
/// every policy's repository.
fn validate_file(f: &config::ConfigFile, data_dir: &Path) -> anyhow::Result<()> {
    for r in f.repository_configs() {
        r.validate(&[data_dir.to_path_buf()])
            .map_err(|e| anyhow::anyhow!("repository {:?}: {e}", r.name))?;
    }
    for (name, c) in f.credential_sources() {
        // (checked as the credentials of an s3 repository)
        let probe = RepoConfig {
            name: name.clone(),
            kind: RepoType::S3,
            bucket: Some("b".into()),
            credentials: c,
            ..Default::default()
        };
        probe
            .validate(&[])
            .map_err(|e| anyhow::anyhow!("credentials {name:?}: {e}"))?;
    }
    Ok(())
}

/// A stable hash of the data directory (`holder.server` of this server's locks).
fn server_id(data_dir: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let p = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// `restoredFrom` of a restored database (`{repository, backup, datasetId, seq}`): from
/// its `dataset.json`, else from its `restore.json`. For the dataset info.
pub fn restored_from(root: &Path) -> Option<serde_json::Value> {
    let from = match sparkles::commit::read_restored_from(root) {
        Ok(Some(f)) => f,
        _ => {
            let r: sparkles_backup::RestoreRecord =
                serde_json::from_slice(&std::fs::read(root.join("restore.json")).ok()?).ok()?;
            sparkles::commit::RestoredFrom {
                repository: r.repository.name,
                backup: r.backup,
                dataset_id: r.source.dataset_id,
                seq: r.source.seq,
            }
        }
    };
    serde_json::to_value(from).ok()
}

/// Lock the data directory for this server process (`<data>/sparkles-server.lock`, an
/// OS lock held while the returned file is open): a second server on the same data
/// directory, or an offline `sparkles backup restore --data`, is refused.
pub fn lock_data_dir(data_dir: &Path) -> anyhow::Result<std::fs::File> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("creating {}", data_dir.display()))?;
    let path = data_dir.join(DATA_LOCK);
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!(
            "data directory {} is in use by another sparkles server ({DATA_LOCK} is locked)",
            data_dir.display()
        ),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("locking {}", path.display()))
        }
    }
}

/// `<data>/sparkles-server.lock`
pub const DATA_LOCK: &str = "sparkles-server.lock";

/// Whether a server holds the data directory's lock now (for offline commands that
/// write into it).
#[cfg_attr(not(test), allow(dead_code))] // `sparkles backup restore --data`
pub fn data_dir_in_use(data_dir: &Path) -> bool {
    match std::fs::File::open(data_dir.join(DATA_LOCK)) {
        Ok(f) => matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        Err(_) => false,
    }
}

/// Start the background parts once the server's runtime exists: remember `h`, check
/// the repositories' reachability, start the policy scheduler, and re-read
/// `--backup-config` on SIGHUP (like the auth and rate-limit files). A no-op without
/// [`AppState::backup`].
pub fn start(st: &Arc<AppState>, h: &Handle) {
    let Some(b) = &st.backup else { return };
    let _ = b.handle.set(h.clone());
    *b.outbound.write() = st.outbound.clone();
    tracing::debug!(target: "sparkles::backup", max_tasks = b.max_tasks, "backup tasks enabled");
    let names: Vec<String> = b.registry.repos.read().keys().cloned().collect();
    for name in names {
        let b = b.clone();
        h.spawn(async move { b.refresh(&name).await });
    }
    #[cfg(unix)]
    if b.config_path.is_some() {
        let _g = h.enter();
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
            Ok(mut hup) => {
                let b = b.clone();
                h.spawn(async move {
                    while hup.recv().await.is_some() {
                        let b2 = b.clone();
                        match tokio::task::spawn_blocking(move || b2.reload()).await {
                            Ok(Ok(())) => {
                                tracing::info!(target: "sparkles::backup", "backup config reloaded");
                                let names: Vec<String> =
                                    b.registry.repos.read().keys().cloned().collect();
                                for n in names {
                                    b.refresh(&n).await;
                                }
                            }
                            Ok(Err(e)) => tracing::error!(
                                target: "sparkles::backup",
                                "backup config not reloaded: {e:#}"
                            ),
                            Err(e) => tracing::error!(
                                target: "sparkles::backup",
                                "backup config not reloaded: {e}"
                            ),
                        }
                    }
                });
            }
            Err(e) => tracing::warn!(
                target: "sparkles::backup",
                "cannot listen for SIGHUP: backup config reload disabled ({e})"
            ),
        }
    }
    scheduler::spawn(st.clone(), Arc::new(scheduler::SystemClock));
}
