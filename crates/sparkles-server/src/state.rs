//! Server state: dataset registry (persisted like Fuseki's `configuration/`), async tasks.

use anyhow::{Context, Result, bail};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sparkles::store::{Store, StoreOptions};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbType {
    Persistent,
    Mem,
}

/// The recorded reasoning status (`reasoning.json`, also embedded in the registry).
/// Fields after `at` are absent from files written by older versions.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningInfo {
    #[serde(default)]
    pub reasoning_format: u32,
    pub profile: String,
    pub inferred: u64,
    pub at: String,
    /// commit (`seq`) at which the inferences were materialized
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<u64>,
    /// what `commit` counts: `"commit"` (the commit sequence)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_source: Option<String>,
    /// the dataset `commit` belongs to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_id: Option<String>,
    /// rule text of profile `rules`, for re-runs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub millis: Option<u64>,
    /// copied from a clone source whose inferences were already stale
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inherited_stale: bool,
}

pub struct Dataset {
    pub name: String,
    pub kind: DbType,
    pub store: Store,
    pub reasoning: RwLock<Option<ReasoningInfo>>,
    /// not part of the persisted registry (e.g. `--loc` on the command line)
    pub ephemeral: bool,
    /// the last schema report served (`/$/schema/{ds}`), kept for its pagination cursors
    pub schema_cache: Mutex<Option<SchemaCacheEntry>>,
    /// write-time SHACL validation, when configured
    pub validation: RwLock<Option<Arc<Validation>>>,
    /// write-time validation counters (the store's guard observer)
    pub validation_metrics: Arc<crate::obs::ValidationMetrics>,
}

#[cfg(feature = "shacl")]
pub type Validation = sparkles_shacl::guard::ShaclGuard;
/// Placeholder: built without SHACL validation.
#[cfg(not(feature = "shacl"))]
pub struct Validation;

/// Install a store's write-time validation from its `validation.json`. A configuration
/// that cannot be loaded leaves the dataset refusing writes (the store fails closed).
fn install_validation(store: &Store) -> Option<Arc<Validation>> {
    #[cfg(feature = "shacl")]
    match sparkles_shacl::guard::install(store) {
        Ok(g) => return g,
        Err(e) => tracing::error!(
            "write-time validation of {}: {e:#}; writes are refused until it is fixed",
            store
                .root()
                .map_or("(memory)".into(), |r| r.display().to_string())
        ),
    }
    #[cfg(not(feature = "shacl"))]
    let _ = store;
    None
}

/// A computed schema report and what it was computed for.
pub struct SchemaCacheEntry {
    /// snapshot identity (`sparkles::schema::snapshot_identity`)
    pub identity: u64,
    /// hash of the selection parameters
    pub selection: u64,
    pub report: Arc<sparkles::schema::SchemaReport>,
}

#[derive(Serialize, Deserialize, Default)]
struct Registry {
    datasets: Vec<RegistryEntry>,
}

#[derive(Serialize, Deserialize)]
struct RegistryEntry {
    name: String,
    #[serde(rename = "type")]
    kind: DbType,
    #[serde(default)]
    reasoning: Option<ReasoningInfo>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub kind: String,
    pub dataset: String,
    /// the dataset a task creates (clone)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub state: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    /// `DELETE /$/tasks/{id}` may cancel it (now)
    pub cancellable: bool,
    /// the task's typed result (a backup summary, a verify or GC report, a policy run)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
    /// set by a cancel request; the work checks it
    #[serde(skip)]
    pub cancel: Arc<AtomicBool>,
}

/// Task states (`Task::state`).
pub mod task_state {
    /// waiting for a task slot
    pub const QUEUED: &str = "queued";
    pub const RUNNING: &str = "running";
    pub const DONE: &str = "done";
    pub const FAILED: &str = "failed";
    /// ended by a cancel request
    pub const CANCELLED: &str = "cancelled";
}

impl Task {
    /// Still queued or running.
    pub fn active(&self) -> bool {
        matches!(
            self.state.as_str(),
            task_state::QUEUED | task_state::RUNNING
        )
    }
}

/// Whether a task's error comes from a cancellation (`sparkles::Error::Cancelled`, or
/// a backup operation's `cancelled`) anywhere in its chain.
fn rooted_in_cancel(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        if matches!(
            c.downcast_ref::<sparkles::Error>(),
            Some(sparkles::Error::Cancelled)
        ) {
            return true;
        }
        #[cfg(feature = "backup")]
        if c.downcast_ref::<sparkles_backup::BackupError>()
            .is_some_and(|b| b.is_cancelled())
        {
            return true;
        }
        false
    })
}

pub struct AppState {
    pub data_dir: PathBuf,
    pub datasets: RwLock<BTreeMap<String, Arc<Dataset>>>,
    pub tasks: Mutex<Vec<Task>>,
    task_counter: AtomicU64,
    pub started: Instant,
    pub started_at: String,
    pub store_opts: StoreOptions,
    pub default_timeout: std::time::Duration,
    pub read_only: bool,
    /// honor `validate=false` on writes (skips write-time validation)
    pub allow_unvalidated_writes: bool,
    /// response compression
    pub http_compression: crate::compress::HttpCompression,
    pub allow_service: bool,
    /// where SERVICE and `LOAD <http…>` may connect (`serve --outbound-*`)
    pub outbound: sparkles::outbound::OutboundPolicy,
    /// which files `LOAD <file:…>` may read (`serve --load-dir`; none without it)
    pub file_loads: sparkles::sparql::FileLoads,
    /// cap on the classes, and separately the predicates, of one schema report
    pub schema_max_entries: usize,
    /// Per-request budgets.
    pub limits: Limits,
    /// Emit one `sparkles::access` event per request.
    pub access_log: bool,
    pub metrics: crate::obs::Metrics,
    /// rate and concurrency limits (`None`: no limits, the default)
    pub rate_limit: Option<Arc<crate::ratelimit::RateLimiter>>,
    phase: AtomicU8,
    /// authentication and authorization (`serve --auth-config`); `None`: open
    pub auth: Option<Arc<crate::auth::Auth>>,
    /// browser origins that may call the API cross-origin, without credentials
    /// (`serve --cors-origin`; with auth, besides the configuration's `cors.origins`)
    pub cors_origins: Vec<String>,
    /// the `Host` names answered without auth (`serve --public-host`)
    pub hosts: crate::exposure::Hosts,
    /// automatic re-materialization of stale inferences (`serve --auto-reason`)
    pub auto_reason: Option<crate::reasoning::AutoReason>,
    /// dataset names being created by a task (clone), with the task id
    reserved: Mutex<BTreeMap<String, String>>,
    /// datasets being replaced in place (an in-place restore), with the task id: every
    /// request naming one of them gets `503` + `Retry-After` (the router's restoring
    /// layer)
    pub restoring: Mutex<BTreeMap<String, String>>,
    /// backup repositories and policies (`serve`; `None` for embedded use)
    #[cfg(feature = "backup")]
    pub backup: Option<Arc<crate::backup::BackupState>>,
    /// Serializes dataset management (create / attach / delete / registry saves) so a
    /// name is reserved atomically and an older registry snapshot can never overwrite
    /// a newer one.
    manage: Mutex<()>,
    /// background task slots (`serve --max-tasks`)
    pub task_queue: TaskQueue,
}

/// Reasoning status is kept inside the database directory (`reasoning.json`) so it
/// survives restarts however the database is attached (registry, `--loc`, CLI `infer`).
pub fn read_reasoning_file(root: &Path) -> Option<ReasoningInfo> {
    serde_json::from_slice(&std::fs::read(root.join("reasoning.json")).ok()?).ok()
}

/// Write (or remove) `reasoning.json` durably: temporary file, sync, rename, directory
/// sync, so a crash leaves the old status or the new one, never a torn file.
pub fn write_reasoning_file(root: &Path, info: Option<&ReasoningInfo>) -> Result<()> {
    let path = root.join("reasoning.json");
    match info {
        Some(i) => {
            let mut i = i.clone();
            i.reasoning_format = 2;
            write_file_atomic(&path, &serde_json::to_vec_pretty(&i)?)?;
        }
        None => match std::fs::remove_file(&path) {
            Ok(()) => sync_dir(root)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        },
    }
    Ok(())
}

/// Replace `path` durably (temporary file, sync, rename, directory sync).
pub fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    sync_dir(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )
}

/// Flush a directory's entries to stable storage (a no-op where directories cannot be
/// opened for syncing).
pub fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

impl Dataset {
    /// Update the reasoning status in memory and in the database directory.
    pub fn set_reasoning(&self, info: Option<ReasoningInfo>) -> Result<()> {
        if let Some(root) = self.store.root() {
            write_reasoning_file(root, info.as_ref())?;
        }
        *self.reasoning.write() = info;
        Ok(())
    }
}

pub fn now() -> String {
    sparkles::builder::now_rfc3339()
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "ui"
        && name != "$"
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && !name.starts_with('.')
}

/// Per-request budgets of the server (`None`: unlimited).
#[derive(Clone, Debug)]
pub struct Limits {
    /// estimated memory of a query's (or update WHERE clause's) intermediate results
    pub query_memory_bytes: Option<u64>,
    /// serialized body of a query response
    pub max_result_bytes: Option<u64>,
    /// serialized body of a Graph Store GET (a graph or whole-dataset export)
    pub max_export_bytes: Option<u64>,
    /// rows of any intermediate result
    pub max_rows: usize,
    /// SPARQL updates without a `timeout` parameter (`None`: no limit, the default)
    pub update_timeout: Option<std::time::Duration>,
    /// decompressed size of a compressed request body or uploaded file
    pub max_decompressed_bytes: Option<u64>,
    /// request body of a SPARQL query (also `/{ds}/explain` and `/{ds}/shacl`)
    pub max_query_body_bytes: Option<u64>,
    /// request body of a SPARQL update
    pub max_update_body_bytes: Option<u64>,
    /// request body of an admin (`/$/…`) or prefix change
    pub max_admin_body_bytes: Option<u64>,
    /// streamed request body of a Graph Store write or upload (decompressed)
    pub max_upload_bytes: Option<u64>,
    /// free space a spooled request body must leave in the temporary directory
    pub min_free_disk_bytes: Option<u64>,
    /// the largest `timeout` a request may ask for (never below the server's default)
    pub max_timeout: Option<std::time::Duration>,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            query_memory_bytes: Some(8 << 30),
            max_result_bytes: Some(1 << 30),
            max_export_bytes: None,
            max_rows: 200_000_000,
            update_timeout: None,
            max_decompressed_bytes: Some(64 << 30),
            max_query_body_bytes: Some(16 << 20),
            max_update_body_bytes: Some(256 << 20),
            max_admin_body_bytes: Some(16 << 20),
            max_upload_bytes: Some(4 << 30),
            min_free_disk_bytes: Some(1 << 30),
            max_timeout: Some(std::time::Duration::from_secs(1800)),
        }
    }
}

impl Limits {
    /// `{timeoutSeconds, updateTimeoutSeconds, maxTimeoutSeconds, queryMemoryBytes,
    /// maxResultBytes, maxExportBytes, maxRows, max…BodyBytes, maxUploadBytes}`; 0 means
    /// unlimited.
    pub fn json(&self, timeout: std::time::Duration) -> serde_json::Value {
        let secs = |t: Option<std::time::Duration>| t.map_or(0.0, |t| t.as_secs_f64());
        serde_json::json!({
            "timeoutSeconds": timeout.as_secs_f64(),
            "updateTimeoutSeconds": secs(self.update_timeout),
            "maxTimeoutSeconds": secs(self.max_timeout.map(|m| m.max(timeout))),
            "queryMemoryBytes": self.query_memory_bytes.unwrap_or(0),
            "maxResultBytes": self.max_result_bytes.unwrap_or(0),
            "maxExportBytes": self.max_export_bytes.unwrap_or(0),
            "maxRows": self.max_rows,
            "maxQueryBodyBytes": self.max_query_body_bytes.unwrap_or(0),
            "maxUpdateBodyBytes": self.max_update_body_bytes.unwrap_or(0),
            "maxAdminBodyBytes": self.max_admin_body_bytes.unwrap_or(0),
            "maxUploadBytes": self.max_upload_bytes.unwrap_or(0),
        })
    }

    /// A requested timeout, capped at `max_timeout` but never below `floor` (the
    /// server's own default, which a client may always ask for).
    pub fn cap_timeout(
        &self,
        requested: std::time::Duration,
        floor: Option<std::time::Duration>,
    ) -> std::time::Duration {
        match self.max_timeout {
            Some(max) => requested.min(floor.map_or(max, |f| max.max(f))),
            None => requested,
        }
    }
}

impl AppState {
    pub fn new(
        data_dir: &Path,
        store_opts: StoreOptions,
        default_timeout: std::time::Duration,
    ) -> Result<AppState> {
        std::fs::create_dir_all(data_dir.join("databases"))?;
        let state = AppState {
            data_dir: data_dir.to_path_buf(),
            datasets: RwLock::new(BTreeMap::new()),
            tasks: Mutex::new(Vec::new()),
            task_counter: AtomicU64::new(1),
            started: Instant::now(),
            started_at: now(),
            store_opts,
            default_timeout,
            read_only: false,
            allow_service: true,
            outbound: Default::default(),
            file_loads: sparkles::sparql::FileLoads::Disabled,
            schema_max_entries: sparkles::schema::DEFAULT_MAX_ENTRIES,
            limits: Limits::default(),
            access_log: true,
            metrics: crate::obs::Metrics::new(true, 100),
            rate_limit: None,
            phase: AtomicU8::new(crate::obs::Phase::Starting as u8),
            auth: None,
            cors_origins: Vec::new(),
            hosts: crate::exposure::Hosts::default(),
            allow_unvalidated_writes: false,
            http_compression: Default::default(),
            auto_reason: None,
            reserved: Mutex::new(BTreeMap::new()),
            restoring: Mutex::new(BTreeMap::new()),
            #[cfg(feature = "backup")]
            backup: None,
            manage: Mutex::new(()),
            task_queue: TaskQueue::new(DEFAULT_MAX_TASKS),
        };
        // clones that were being built when the server stopped are never registered
        for e in std::fs::read_dir(data_dir.join("databases"))?.flatten() {
            if e.file_name().to_string_lossy().starts_with(".clone-") {
                tracing::info!("removing unfinished clone {}", e.path().display());
                std::fs::remove_dir_all(e.path())
                    .with_context(|| format!("removing {}", e.path().display()))?;
            }
        }
        let reg_path = data_dir.join("config.json");
        if reg_path.exists() {
            let reg: Registry = serde_json::from_slice(&std::fs::read(&reg_path)?)
                .with_context(|| format!("reading {}", reg_path.display()))?;
            for e in reg.datasets {
                let ds = state.open_dataset(&e.name, e.kind, None)?;
                // older registries kept the reasoning status only here
                if ds.reasoning.read().is_none() {
                    *ds.reasoning.write() = e.reasoning;
                }
                state.datasets.write().insert(e.name.clone(), ds);
                tracing::info!("opened dataset /{} ({:?})", e.name, e.kind);
            }
        }
        Ok(state)
    }

    /// State without a data directory or registry, for embedded use (`sparkles mcp`):
    /// datasets are only [`attach`](Self::attach)ed, and nothing is ever written besides
    /// the datasets themselves.
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    pub fn standalone(store_opts: StoreOptions, default_timeout: std::time::Duration) -> AppState {
        AppState {
            data_dir: PathBuf::new(),
            datasets: RwLock::new(BTreeMap::new()),
            tasks: Mutex::new(Vec::new()),
            task_counter: AtomicU64::new(1),
            started: Instant::now(),
            started_at: now(),
            store_opts,
            default_timeout,
            read_only: false,
            allow_service: false,
            outbound: Default::default(),
            file_loads: sparkles::sparql::FileLoads::Disabled,
            schema_max_entries: sparkles::schema::DEFAULT_MAX_ENTRIES,
            limits: Limits::default(),
            access_log: false,
            metrics: crate::obs::Metrics::new(false, 100),
            phase: AtomicU8::new(crate::obs::Phase::Ready as u8),
            auto_reason: None,
            reserved: Mutex::new(BTreeMap::new()),
            restoring: Mutex::new(BTreeMap::new()),
            #[cfg(feature = "backup")]
            backup: None,
            manage: Mutex::new(()),
            task_queue: TaskQueue::new(DEFAULT_MAX_TASKS),
            rate_limit: None,
            auth: None,
            cors_origins: Vec::new(),
            hosts: crate::exposure::Hosts::default(),
            allow_unvalidated_writes: false,
            http_compression: Default::default(),
        }
    }

    fn open_dataset(&self, name: &str, kind: DbType, loc: Option<&Path>) -> Result<Arc<Dataset>> {
        let store = match kind {
            DbType::Mem => Store::in_memory(self.store_opts.clone()),
            DbType::Persistent => {
                let dir = loc
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.data_dir.join("databases").join(name));
                Store::open(&dir, self.store_opts.clone())
                    .with_context(|| format!("opening database {}", dir.display()))?
            }
        };
        let reasoning = store.root().and_then(read_reasoning_file);
        let validation = install_validation(&store);
        let validation_metrics = Arc::new(crate::obs::ValidationMetrics::new(name));
        store.set_guard_observer(Some(validation_metrics.clone()));
        Ok(Arc::new(Dataset {
            name: name.to_string(),
            kind,
            store,
            reasoning: RwLock::new(reasoning),
            ephemeral: loc.is_some(),
            schema_cache: Mutex::new(None),
            validation: RwLock::new(validation),
            validation_metrics,
        }))
    }

    /// Persist the registry of managed datasets.
    pub fn save_registry(&self) -> Result<()> {
        let _guard = self.manage.lock();
        self.save_registry_locked()
    }

    /// Write `config.json` durably (temporary file, sync, rename, directory sync). The
    /// caller holds `manage`, so the snapshot taken here is the newest one written.
    fn save_registry_locked(&self) -> Result<()> {
        if self.data_dir.as_os_str().is_empty() {
            // standalone state: no registry
            return Ok(());
        }
        let reg = Registry {
            datasets: self
                .datasets
                .read()
                .values()
                .filter(|d| !d.ephemeral)
                .map(|d| RegistryEntry {
                    name: d.name.clone(),
                    kind: d.kind,
                    reasoning: d.reasoning.read().clone(),
                })
                .collect(),
        };
        let path = self.data_dir.join("config.json");
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(&reg)?)?;
            f.sync_all()?;
        }
        std::fs::rename(tmp, path)?;
        #[cfg(unix)]
        std::fs::File::open(&self.data_dir)?.sync_all()?;
        Ok(())
    }

    pub fn phase(&self) -> crate::obs::Phase {
        crate::obs::Phase::from_u8(self.phase.load(Ordering::SeqCst))
    }

    pub fn set_phase(&self, p: crate::obs::Phase) {
        self.phase.store(p as u8, Ordering::SeqCst);
    }

    pub fn get(&self, name: &str) -> Option<Arc<Dataset>> {
        self.datasets.read().get(name).cloned()
    }

    pub fn create(&self, name: &str, kind: DbType) -> Result<Arc<Dataset>> {
        if !valid_name(name) {
            bail!("invalid dataset name '{name}'");
        }
        let _guard = self.manage.lock();
        if self.datasets.read().contains_key(name) {
            bail!("dataset '{name}' already exists");
        }
        if let Some(t) = self.reserved_by(name) {
            bail!("dataset /{name} is being created by task {t}");
        }
        if let Some(t) = self.restoring.lock().get(name) {
            bail!("dataset /{name} is being restored by task {t}");
        }
        let ds = self.open_dataset(name, kind, None)?;
        self.datasets.write().insert(name.to_string(), ds.clone());
        if let Err(e) = self.save_registry_locked() {
            // not reported as created, so it must not stay registered
            self.datasets.write().remove(name);
            return Err(e);
        }
        Ok(ds)
    }

    /// Register a dataset that is not persisted in the registry (`--mem`, `--loc`).
    pub fn attach(&self, name: &str, kind: DbType, loc: Option<&Path>) -> Result<Arc<Dataset>> {
        if !valid_name(name) {
            bail!("invalid dataset name '{name}'");
        }
        let _guard = self.manage.lock();
        if self.datasets.read().contains_key(name) {
            bail!("dataset '{name}' already exists");
        }
        if let Some(t) = self.reserved_by(name) {
            bail!("dataset /{name} is being created by task {t}");
        }
        if let Some(t) = self.restoring.lock().get(name) {
            bail!("dataset /{name} is being restored by task {t}");
        }
        let ds = self.open_dataset(name, kind, loc)?;
        let ds = Arc::new(Dataset {
            ephemeral: true,
            ..Arc::try_unwrap(ds).map_err(|_| anyhow::anyhow!("unexpected"))?
        });
        self.datasets.write().insert(name.to_string(), ds.clone());
        Ok(ds)
    }

    pub fn delete(&self, name: &str) -> Result<bool> {
        let _guard = self.manage.lock();
        let Some(ds) = self.datasets.write().remove(name) else {
            return Ok(false);
        };
        if let Err(e) = self.save_registry_locked() {
            self.datasets.write().insert(name.to_string(), ds);
            return Err(e);
        }
        self.metrics.forget(name);
        if ds.kind == DbType::Persistent
            && !ds.ephemeral
            && let Some(root) = ds.store.root()
        {
            let root = root.to_path_buf();
            drop(ds);
            std::fs::remove_dir_all(root)?;
        }
        Ok(true)
    }

    /// Take the registered persistent dataset `name` out of the map for an in-place
    /// replacement; the persisted registry keeps it (put it back with
    /// [`reattach`](Self::reattach)). `None` if there is no such dataset, or it is not a
    /// managed persistent one (`--loc`, `--mem`).
    #[cfg_attr(not(feature = "backup"), allow(dead_code))]
    pub fn detach_for_swap(&self, name: &str) -> Option<Arc<Dataset>> {
        let _guard = self.manage.lock();
        let mut map = self.datasets.write();
        match map.get(name) {
            Some(ds) if ds.kind == DbType::Persistent && !ds.ephemeral => map.remove(name),
            _ => None,
        }
    }

    /// Open `databases/<name>` and register it under `name` again (after a swap, or to
    /// roll one back). Fails if the name is registered.
    #[cfg_attr(not(feature = "backup"), allow(dead_code))]
    pub fn reattach(&self, name: &str) -> Result<Arc<Dataset>> {
        let _guard = self.manage.lock();
        if self.datasets.read().contains_key(name) {
            bail!("dataset '{name}' already exists");
        }
        let ds = self.open_dataset(name, DbType::Persistent, None)?;
        self.datasets.write().insert(name.to_string(), ds.clone());
        Ok(ds)
    }

    /// The task creating dataset `name`, if one is.
    pub fn reserved_by(&self, name: &str) -> Option<String> {
        self.reserved.lock().get(name).cloned()
    }

    /// Reserve the name of a dataset that task `task` will create. Fails (with the
    /// message for a `409`) when the name is registered, reserved, or its directory
    /// exists. The name is released when the reservation is dropped.
    pub fn reserve(self: &Arc<Self>, name: &str, task: &str) -> Result<Reservation, String> {
        let _guard = self.manage.lock();
        if self.datasets.read().contains_key(name) {
            return Err(format!("dataset /{name} already exists"));
        }
        let mut reserved = self.reserved.lock();
        if let Some(t) = reserved.get(name) {
            return Err(format!("dataset /{name} is being created by task {t}"));
        }
        if let Some(t) = self.restoring.lock().get(name) {
            return Err(format!("dataset /{name} is being restored by task {t}"));
        }
        if self.data_dir.join("databases").join(name).exists() {
            return Err(format!(
                "directory databases/{name} exists but is not a registered dataset; remove it first"
            ));
        }
        reserved.insert(name.to_string(), task.to_string());
        Ok(Reservation {
            state: self.clone(),
            name: name.to_string(),
        })
    }

    /// Register the persistent database now in `databases/{name}` under a reserved name
    /// and persist the registry. On failure the directory is removed.
    pub fn adopt(&self, reservation: Reservation) -> Result<Arc<Dataset>> {
        let name = reservation.name.clone();
        let _guard = self.manage.lock();
        let dir = self.data_dir.join("databases").join(&name);
        let ds = match self.open_dataset(&name, DbType::Persistent, None) {
            Ok(ds) => ds,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        self.datasets.write().insert(name.clone(), ds.clone());
        if let Err(e) = self.save_registry_locked() {
            self.datasets.write().remove(&name);
            drop(ds);
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        drop(reservation);
        Ok(ds)
    }

    // ----------------------------------------------------------------- tasks ------

    /// A new task id (for a task started with [`start_task_as`](Self::start_task_as)).
    pub fn next_task_id(&self) -> String {
        self.task_counter
            .fetch_add(1, Ordering::Relaxed)
            .to_string()
    }

    pub fn start_task(
        self: &Arc<Self>,
        kind: &str,
        dataset: &str,
        work: impl FnOnce(&TaskHandle) -> Result<String> + Send + 'static,
    ) -> Task {
        self.start_task_as(self.next_task_id(), kind, dataset, None, work)
    }

    /// Start a task with an id from [`next_task_id`](Self::next_task_id); `target` is the
    /// dataset it creates, if any.
    pub fn start_task_as(
        self: &Arc<Self>,
        id: String,
        kind: &str,
        dataset: &str,
        target: Option<&str>,
        work: impl FnOnce(&TaskHandle) -> Result<String> + Send + 'static,
    ) -> Task {
        self.start_task_opts(id, kind, dataset, target, false, work)
    }

    /// Start a task on its own thread. `dataset` is the dataset it works on (`""` for a
    /// server-scoped task, visible to `server-admin` only), `target` the dataset it
    /// creates, if any. A `cancellable` task accepts `DELETE /$/tasks/{id}`, which sets
    /// [`TaskHandle::cancel_flag`]; the work checks the flag and fails with
    /// `sparkles::Error::Cancelled` (or a backup `cancelled` error), which ends the task
    /// `cancelled` rather than `failed`.
    pub fn start_task_opts(
        self: &Arc<Self>,
        id: String,
        kind: &str,
        dataset: &str,
        target: Option<&str>,
        cancellable: bool,
        work: impl FnOnce(&TaskHandle) -> Result<String> + Send + 'static,
    ) -> Task {
        let mut task = Task {
            id: id.clone(),
            kind: kind.to_string(),
            dataset: dataset.to_string(),
            target: target.map(str::to_string),
            state: task_state::RUNNING.into(),
            started_at: now(),
            finished_at: None,
            message: None,
            progress: Some(0.0),
            cancellable,
            detail: None,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let cancel = task.cancel.clone();
        // backup tasks wait for their own slots (`--backup-max-tasks`)
        let counted = !kind.starts_with("backup-");
        let state = self.clone();
        let span = crate::otel::task_span(kind, &id, dataset);
        let run = {
            let id = id.clone();
            move |waited: bool| {
                std::thread::spawn(move || {
                    let handle = TaskHandle {
                        state: state.clone(),
                        id: id.clone(),
                        cancel,
                    };
                    // a task that waited runs unless it was cancelled meanwhile (checked
                    // and switched to running under the list's lock, as cancels are)
                    let mut start = !waited;
                    if waited {
                        handle.update(|t| {
                            if !t.cancel.load(Ordering::Relaxed) {
                                t.state = task_state::RUNNING.into();
                                t.cancellable = cancellable;
                                t.message = None;
                                start = true;
                            }
                        });
                    }
                    let r = if start {
                        span.in_scope(|| work(&handle))
                    } else {
                        Err(sparkles::Error::Cancelled.into())
                    };
                    finish(&state.tasks, &id, r);
                    if counted {
                        state.task_queue.done();
                    }
                });
            }
        };
        if !counted {
            self.push_task(task.clone());
            run(false);
            return task;
        }
        // decided and listed under the queue's lock, so the task is listed before a
        // finishing task can start it
        let mut q = self.task_queue.inner.lock();
        let now_running = q.running < self.task_queue.max();
        if now_running {
            q.running += 1;
        } else {
            // a task that has not started may always be cancelled
            task.state = task_state::QUEUED.into();
            task.cancellable = true;
            task.message = Some("waiting for a free task slot (--max-tasks)".into());
        }
        self.push_task(task.clone());
        if now_running {
            drop(q);
            run(false);
        } else {
            q.queued.push_back((id, Box::new(move || run(true))));
        }
        task
    }

    /// List a new task, dropping the oldest finished ones past [`FINISHED_TASKS_KEPT`]
    /// (queued and running tasks always stay listed).
    fn push_task(&self, task: Task) {
        let mut tasks = self.tasks.lock();
        tasks.push(task);
        let finished = tasks.iter().filter(|t| !t.active()).count();
        let mut excess = finished.saturating_sub(FINISHED_TASKS_KEPT);
        if excess > 0 {
            tasks.retain(|t| {
                if excess > 0 && !t.active() {
                    excess -= 1;
                    return false;
                }
                true
            });
        }
    }

    /// Whether another task may be queued: `Err` with the message for a `503` once
    /// [`MAX_QUEUED_TASKS`] wait.
    pub fn task_room(&self) -> std::result::Result<(), String> {
        let n = self.task_queue.inner.lock().queued.len();
        if n >= MAX_QUEUED_TASKS {
            return Err(format!(
                "{n} tasks are waiting for a task slot; try again later"
            ));
        }
        Ok(())
    }

    /// The queued or running task of `kind` on `dataset`, if there is one.
    pub fn active_task(&self, kind: &str, dataset: &str) -> Option<String> {
        self.tasks
            .lock()
            .iter()
            .find(|t| t.kind == kind && t.dataset == dataset && t.active())
            .map(|t| t.id.clone())
    }

    /// Ask task `id` to stop: `Ok` with the task when it accepted (the work stops at
    /// its next check), `Err` with it when it is finished or not cancellable, `None`
    /// when there is no such task.
    pub fn cancel_task(&self, id: &str) -> Option<Result<Task, Task>> {
        let mut tasks = self.tasks.lock();
        let t = tasks.iter_mut().find(|t| t.id == id)?;
        if !t.cancellable || !t.active() {
            return Some(Err(t.clone()));
        }
        t.cancel.store(true, Ordering::Relaxed);
        t.message = Some("cancelling".into());
        let t = t.clone();
        drop(tasks);
        // a task still waiting for its turn never starts
        if t.state == task_state::QUEUED && self.task_queue.remove(id) {
            finish(&self.tasks, id, Err(sparkles::Error::Cancelled.into()));
            let tasks = self.tasks.lock();
            return Some(Ok(tasks.iter().find(|t| t.id == id).cloned().unwrap_or(t)));
        }
        Some(Ok(t))
    }
}

/// Record how task `id` ended.
fn finish(tasks: &Mutex<Vec<Task>>, id: &str, r: Result<String>) {
    let mut tasks = tasks.lock();
    if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
        t.finished_at = Some(now());
        t.progress = Some(1.0);
        t.cancellable = false;
        match r {
            Ok(msg) => {
                t.state = task_state::DONE.into();
                t.message = Some(msg);
            }
            Err(e) if rooted_in_cancel(&e) => {
                t.state = task_state::CANCELLED.into();
                t.message = Some("cancelled".into());
            }
            Err(e) => {
                t.state = task_state::FAILED.into();
                t.message = Some(format!("{e:#}"));
            }
        }
    }
}

/// Finished tasks kept in the task list (`/$/tasks`); queued and running tasks are
/// never dropped from it.
pub const FINISHED_TASKS_KEPT: usize = 200;

/// Tasks that may wait for a slot at once; further starts are refused (`503`).
pub const MAX_QUEUED_TASKS: usize = 1000;

/// Background tasks running at once by default (`serve --max-tasks`).
pub const DEFAULT_MAX_TASKS: usize = 4;

/// The background task slots: at most `max` tasks run at once (each on its own
/// thread); the others wait, `queued`, in start order. Backup tasks are not counted
/// here: they wait for the slots of `--backup-max-tasks`.
pub struct TaskQueue {
    max: std::sync::atomic::AtomicUsize,
    inner: Mutex<QueueState>,
}

/// A queued task: its id and what starts it.
type Queued = (String, Box<dyn FnOnce() + Send>);

#[derive(Default)]
struct QueueState {
    running: usize,
    queued: std::collections::VecDeque<Queued>,
}

impl TaskQueue {
    pub fn new(max: usize) -> TaskQueue {
        TaskQueue {
            max: std::sync::atomic::AtomicUsize::new(max),
            inner: Mutex::new(QueueState::default()),
        }
    }

    /// Tasks that may run at once (0: no limit).
    pub fn set_max(&self, max: usize) {
        self.max.store(max, Ordering::Relaxed);
    }

    fn max(&self) -> usize {
        match self.max.load(Ordering::Relaxed) {
            0 => usize::MAX,
            n => n,
        }
    }

    /// Tasks running now and waiting.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn counts(&self) -> (usize, usize) {
        let q = self.inner.lock();
        (q.running, q.queued.len())
    }

    /// A counted task ended: start the next one in its slot.
    fn done(&self) {
        let mut q = self.inner.lock();
        match q.queued.pop_front() {
            Some((_, start)) => {
                drop(q);
                start();
            }
            None => q.running -= 1,
        }
    }

    /// Take a queued task out; whether it was queued.
    fn remove(&self, id: &str) -> bool {
        let mut q = self.inner.lock();
        let Some(i) = q.queued.iter().position(|(t, _)| t == id) else {
            return false;
        };
        let entry = q.queued.remove(i);
        drop(q);
        // its work (and whatever it holds, e.g. a name reservation) is dropped unrun
        drop(entry);
        true
    }
}

/// A reserved dataset name (see [`AppState::reserve`]), released on drop.
pub struct Reservation {
    state: Arc<AppState>,
    name: String,
}

impl Reservation {
    /// The reserved dataset name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.state.reserved.lock().remove(&self.name);
    }
}

#[derive(Clone)]
pub struct TaskHandle {
    state: Arc<AppState>,
    id: String,
    cancel: Arc<AtomicBool>,
}

impl TaskHandle {
    pub fn progress(&self, p: f32, msg: &str) {
        self.update(|t| {
            t.progress = Some(p.clamp(0.0, 1.0));
            t.message = Some(msg.to_string());
        });
    }

    /// Set by `DELETE /$/tasks/{id}` (for a cancellable task).
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// Whether a cancel request came in.
    #[cfg_attr(not(any(test, feature = "backup")), allow(dead_code))]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// The task's typed result (`detail`), visible while it runs and after.
    #[cfg_attr(not(any(test, feature = "backup")), allow(dead_code))]
    pub fn set_detail(&self, detail: serde_json::Value) {
        self.update(|t| t.detail = Some(detail));
    }

    /// `queued` (waiting for a slot) or `running` (see [`task_state`]); the final
    /// state is set when the work returns.
    #[cfg_attr(not(any(test, feature = "backup")), allow(dead_code))]
    pub fn set_state(&self, state: &str) {
        self.update(|t| t.state = state.to_string());
    }

    /// Whether a cancel request is accepted from now on (e.g. no longer once a restore
    /// has started to swap directories).
    pub fn set_cancellable(&self, cancellable: bool) {
        self.update(|t| t.cancellable = cancellable);
    }

    fn update(&self, f: impl FnOnce(&mut Task)) {
        if let Some(t) = self.state.tasks.lock().iter_mut().find(|t| t.id == self.id) {
            f(t);
        }
    }
}

pub fn uptime_secs(state: &AppState) -> u64 {
    state.started.elapsed().as_secs()
}
