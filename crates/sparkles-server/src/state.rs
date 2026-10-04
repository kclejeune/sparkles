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

pub use sparkles::reasoning::{
    AutoSetting, ReasoningRecord as ReasoningInfo, read_record as read_reasoning_file,
    write_record as write_reasoning_file,
};
#[cfg(feature = "reasoning")]
pub use sparkles::reasoning::{RunChanges, RunInfo};

/// A dataset of the server: the library's [`sparkles::Dataset`], which holds the store
/// and the state its directory configures (the write guard, RDFS on read, the stored
/// queries, the GraphQL configuration, the reasoning record, the closure cache, the
/// schema cache and the clone origin), and what only the server keeps. Its fields read
/// through to the library's state, so `ds.store` and `ds.reasoning` are the library's.
pub struct Dataset {
    pub name: String,
    pub kind: DbType,
    /// the branch this dataset object serves (`None`: `main`, the dataset itself)
    pub branch: Option<BranchOf>,
    /// the dataset objects of the branches other than `main`, opened on first use
    pub branches: Mutex<BTreeMap<String, Arc<Dataset>>>,
    /// not part of the persisted registry (e.g. `--loc` on the command line)
    pub ephemeral: bool,
    /// write-time validation counters (the store's guard observer)
    pub validation_metrics: Arc<crate::obs::ValidationMetrics>,
    /// taken offline by `POST /$/datasets/{ds}?state=offline` (Fuseki): its services
    /// answer `503` until `?state=active`; not persisted
    pub offline: AtomicBool,
    /// the library's dataset
    pub dataset: sparkles::Dataset,
}

impl std::ops::Deref for Dataset {
    type Target = sparkles::dataset::DatasetState;

    fn deref(&self) -> &Self::Target {
        self.dataset.state()
    }
}

/// Which branch a branch's dataset object serves.
pub struct BranchOf {
    /// the branch's name, which a rename changes
    pub name: RwLock<String>,
    /// the dataset (its `main`)
    pub main: std::sync::Weak<Dataset>,
}

impl Dataset {
    /// The branch this object serves (`main` for the dataset itself).
    pub fn branch_name(&self) -> String {
        self.branch
            .as_ref()
            .map_or(sparkles::branch::MAIN.to_string(), |b| {
                b.name.read().clone()
            })
    }

    /// The dataset's own object (`main`), for a branch's.
    pub fn main(&self) -> Option<Arc<Dataset>> {
        self.branch.as_ref().and_then(|b| b.main.upgrade())
    }

    /// The name the server's per-dataset state (compaction, tasks) keeps it under:
    /// the dataset's name, and `name@branch` for a branch other than `main`.
    pub fn key(&self) -> String {
        match &self.branch {
            Some(b) => format!("{}@{}", self.name, b.name.read()),
            None => self.name.clone(),
        }
    }
}

// the guard type of the validators; builds without one do not name it
#[allow(unused_imports)]
pub use crate::write_validation::Validation;

/// Default of `serve --reason-cache-triples`.
pub const DEFAULT_REASON_CACHE_TRIPLES: usize = sparkles::reasoning::DEFAULT_CLOSURE_CACHE_TRIPLES;

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

/// A background task. Its JSON form also carries Fuseki's names for its fields
/// (`taskId`, `task`, `started`, `finished`, `success`; see [`Task::serialize`]).
#[derive(Clone, Debug)]
pub struct Task {
    pub id: String,
    pub kind: String,
    pub dataset: String,
    /// the dataset a task creates (clone)
    pub target: Option<String>,
    pub state: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub message: Option<String>,
    pub progress: Option<f32>,
    /// `DELETE /$/tasks/{id}` may cancel it (now)
    pub cancellable: bool,
    /// the task's typed result (a backup summary, a verify or GC report, a policy run)
    pub detail: Option<serde_json::Value>,
    /// set by a cancel request; the work checks it
    pub cancel: Arc<AtomicBool>,
}

impl Serialize for Task {
    /// The Sparkles fields, then Fuseki's: `taskId` (the id), `task` (Fuseki's name for
    /// a compaction or a backup, else the kind), `started`, and once the task has ended
    /// `finished` and `success`. Fuseki clients poll `/$/tasks/{taskId}` until
    /// `finished` appears.
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            id: &'a str,
            kind: &'a str,
            dataset: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            target: Option<&'a str>,
            state: &'a str,
            started_at: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            finished_at: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            message: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            progress: Option<f32>,
            cancellable: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            detail: Option<&'a serde_json::Value>,
            task_id: &'a str,
            task: &'a str,
            started: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            finished: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            success: Option<bool>,
        }
        let ended = !self.active();
        Wire {
            id: &self.id,
            kind: &self.kind,
            dataset: &self.dataset,
            target: self.target.as_deref(),
            state: &self.state,
            started_at: &self.started_at,
            finished_at: self.finished_at.as_deref(),
            message: self.message.as_deref(),
            progress: self.progress,
            cancellable: self.cancellable,
            detail: self.detail.as_ref(),
            task_id: &self.id,
            task: match self.kind.as_str() {
                "compact" => "Compact",
                "backup" => "Backup",
                k => k,
            },
            started: &self.started_at,
            finished: if ended {
                Some(self.finished_at.as_deref().unwrap_or(&self.started_at))
            } else {
                None
            },
            success: ended.then(|| self.state == task_state::DONE),
        }
        .serialize(s)
    }
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
    /// the ceilings of GraphQL requests (`--graphql-max-depth` and the like)
    #[cfg(feature = "graphql")]
    pub graphql_limits: sparkles_graphql::plan::Limits,
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
    /// the MapLibre style of the UI's maps (`serve --map-style-url`); `None`: the
    /// bundled basemap
    pub map_style_url: Option<String>,
    /// Fuseki's Graph Store direct naming (`serve --gsp-direct-naming`): `/{ds}/{path}`
    /// names the graph whose IRI is the request URL
    pub gsp_direct_naming: bool,
    /// automatic re-materialization of stale inferences (`serve --auto-reason`)
    pub auto_reason: Option<crate::reasoning::AutoReason>,
    /// the largest closure a dataset keeps in memory for incremental reasoning, in
    /// triples (`serve --reason-cache-triples`)
    pub reason_cache_triples: usize,
    /// automatic compaction (`serve --auto-compact-*`) and what is known of each
    /// dataset's compactions
    pub compaction: crate::compaction::AutoCompact,
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
    /// `POST /$/format` (`serve --format-*`)
    #[cfg(feature = "fmt")]
    pub format: FormatConf,
    /// the MCP endpoint `/$/mcp` (`serve --mcp`); `None`: not mounted
    #[cfg(feature = "mcp")]
    pub mcp: Option<Arc<crate::mcp::http::HttpConf>>,
}

/// Who may use `POST /$/format` (`serve --format-endpoint`).
#[cfg(feature = "fmt")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum FormatEndpoint {
    /// every caller the server admits, anonymous ones included
    #[default]
    On,
    /// every caller but the anonymous principal (401)
    Authenticated,
    /// nobody (404)
    Off,
}

/// The format endpoint's settings and its slots.
#[cfg(feature = "fmt")]
#[derive(Clone, Debug)]
pub struct FormatConf {
    pub endpoint: FormatEndpoint,
    /// the largest request body (`--format-max-mb`; `None`: unlimited)
    pub max_bytes: Option<u64>,
    /// how long a request may take, waiting for a slot included (`--format-timeout`)
    pub timeout: std::time::Duration,
    /// requests formatting at once (one per core); more wait until their deadline
    pub permits: Arc<tokio::sync::Semaphore>,
}

#[cfg(feature = "fmt")]
impl Default for FormatConf {
    fn default() -> FormatConf {
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        FormatConf {
            endpoint: FormatEndpoint::On,
            max_bytes: Some(16 << 20),
            timeout: std::time::Duration::from_secs(10),
            permits: Arc::new(tokio::sync::Semaphore::new(cores)),
        }
    }
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
    /// rows produced by all the operators of one query, summed
    pub max_rows_produced: Option<u64>,
    /// the default storage quota of a persistent dataset (`--max-dataset-mb`; the
    /// stores enforce it, this copy is for display)
    pub max_dataset_bytes: Option<u64>,
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
            max_rows_produced: None,
            max_dataset_bytes: None,
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
    /// maxResultBytes, maxExportBytes, maxRows, maxRowsProduced, maxDatasetBytes,
    /// max…BodyBytes, maxUploadBytes}`; 0 means unlimited.
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
            "maxRowsProduced": self.max_rows_produced.unwrap_or(0),
            "maxDatasetBytes": self.max_dataset_bytes.unwrap_or(0),
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
            #[cfg(feature = "graphql")]
            graphql_limits: Default::default(),
            limits: Limits::default(),
            access_log: true,
            metrics: crate::obs::Metrics::new(true, 100),
            rate_limit: None,
            phase: AtomicU8::new(crate::obs::Phase::Starting as u8),
            auth: None,
            cors_origins: Vec::new(),
            hosts: crate::exposure::Hosts::default(),
            map_style_url: None,
            gsp_direct_naming: false,
            allow_unvalidated_writes: false,
            http_compression: Default::default(),
            auto_reason: None,
            reason_cache_triples: DEFAULT_REASON_CACHE_TRIPLES,
            compaction: Default::default(),
            reserved: Mutex::new(BTreeMap::new()),
            restoring: Mutex::new(BTreeMap::new()),
            #[cfg(feature = "backup")]
            backup: None,
            manage: Mutex::new(()),
            task_queue: TaskQueue::new(DEFAULT_MAX_TASKS),
            #[cfg(feature = "fmt")]
            format: FormatConf::default(),
            #[cfg(feature = "mcp")]
            mcp: None,
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
                let ds = state.open_dataset(&e.name, e.kind, None, false)?;
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
            #[cfg(feature = "graphql")]
            graphql_limits: Default::default(),
            limits: Limits::default(),
            access_log: false,
            metrics: crate::obs::Metrics::new(false, 100),
            phase: AtomicU8::new(crate::obs::Phase::Ready as u8),
            auto_reason: None,
            reason_cache_triples: DEFAULT_REASON_CACHE_TRIPLES,
            compaction: Default::default(),
            reserved: Mutex::new(BTreeMap::new()),
            restoring: Mutex::new(BTreeMap::new()),
            #[cfg(feature = "backup")]
            backup: None,
            manage: Mutex::new(()),
            task_queue: TaskQueue::new(DEFAULT_MAX_TASKS),
            #[cfg(feature = "fmt")]
            format: FormatConf::default(),
            #[cfg(feature = "mcp")]
            mcp: None,
            rate_limit: None,
            auth: None,
            cors_origins: Vec::new(),
            hosts: crate::exposure::Hosts::default(),
            map_style_url: None,
            gsp_direct_naming: false,
            allow_unvalidated_writes: false,
            http_compression: Default::default(),
        }
    }

    fn open_dataset(
        &self,
        name: &str,
        kind: DbType,
        loc: Option<&Path>,
        ephemeral: bool,
    ) -> Result<Arc<Dataset>> {
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
        Ok(self.dataset_of(name, kind, store, ephemeral, None))
    }

    /// The object of branch `branch` of dataset `main` (`main` itself for `main`),
    /// opened on first use and kept.
    pub fn branch_dataset(
        &self,
        main: &Arc<Dataset>,
        branch: &str,
    ) -> sparkles::Result<Arc<Dataset>> {
        if branch == sparkles::branch::MAIN {
            return Ok(main.clone());
        }
        let id = main.store.branch_id_of(branch)?;
        // a cached object of a branch deleted and made again under the same name is stale
        if let Some(d) = main.branches.lock().get(branch)
            && d.store.branch_id() == id
        {
            return Ok(d.clone());
        }
        let ds = self.branch_dataset_uncached(main, branch)?;
        main.branches.lock().insert(branch.to_string(), ds.clone());
        Ok(ds)
    }

    /// After branch `old` of `main` was renamed `new`: its dataset object, if one is
    /// open, serves the new name, and the old one names nothing.
    pub fn renamed_branch(&self, main: &Arc<Dataset>, old: &str, new: &str) {
        let mut m = main.branches.lock();
        if let Some(d) = m.remove(old) {
            if let Some(b) = &d.branch {
                *b.name.write() = new.to_string();
            }
            m.insert(new.to_string(), d);
        }
    }

    /// A branch's dataset object: the library opens the state the branch's directory
    /// configures, as for any dataset.
    fn branch_dataset_uncached(
        &self,
        main: &Arc<Dataset>,
        branch: &str,
    ) -> sparkles::Result<Arc<Dataset>> {
        let store = main
            .store
            .branch(branch)?
            .shared()
            .expect("a branch other than main has its own store");
        let mut ds = self.dataset_of(
            &main.name,
            main.kind,
            sparkles::dataset::StoreHandle::Branch(store),
            main.ephemeral,
            None,
        );
        Arc::get_mut(&mut ds).expect("just made").branch = Some(BranchOf {
            name: RwLock::new(branch.to_string()),
            main: Arc::downgrade(main),
        });
        Ok(ds)
    }

    /// The dataset `name` around `store`: the library opens the state the store's
    /// directory configures, and the server adds its validation metrics.
    fn dataset_of(
        &self,
        name: &str,
        kind: DbType,
        store: impl Into<sparkles::dataset::StoreHandle>,
        ephemeral: bool,
        origin: Option<serde_json::Value>,
    ) -> Arc<Dataset> {
        let dataset = sparkles::Dataset::from_store_with(
            store,
            sparkles::DatasetOptions {
                store: self.store_opts.clone(),
                name: Some(name.to_string()),
                closure_cache_triples: self.reason_cache_triples,
                origin,
            },
        );
        let validation_metrics = Arc::new(crate::obs::ValidationMetrics::new(name));
        dataset
            .store()
            .set_guard_observer(Some(validation_metrics.clone()));
        Arc::new(Dataset {
            name: name.to_string(),
            kind,
            branch: None,
            branches: Mutex::new(BTreeMap::new()),
            ephemeral,
            validation_metrics,
            offline: AtomicBool::new(false),
            dataset,
        })
    }

    #[cfg(any(feature = "reasoning", feature = "backup"))]
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

    /// Every dataset, and the open branches of each (for the background upkeep:
    /// compaction and history ticks).
    pub fn datasets_and_branches(&self) -> Vec<Arc<Dataset>> {
        let mains: Vec<Arc<Dataset>> = self.datasets.read().values().cloned().collect();
        let mut out = Vec::with_capacity(mains.len());
        for m in mains {
            let names: Vec<String> = m
                .store
                .branch_set()
                .map(|s| {
                    s.open_stores()
                        .iter()
                        .map(|b| b.branch_name().to_string())
                        .collect()
                })
                .unwrap_or_default();
            out.push(m.clone());
            for n in names {
                if let Ok(b) = self.branch_dataset(&m, &n) {
                    out.push(b);
                }
            }
        }
        out
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
        let ds = self.open_dataset(name, kind, None, false)?;
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
        let ds = self.open_dataset(name, kind, loc, true)?;
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
        let ds = self.open_dataset(name, DbType::Persistent, None, false)?;
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
        let ds = match self.open_dataset(&name, DbType::Persistent, None, false) {
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

    /// Register the in-memory store `store` (a clone) under a reserved name, with its
    /// reasoning status and origin, and persist the registry. Like every in-memory
    /// dataset, it is registered again after a restart, empty.
    pub fn adopt_memory(
        &self,
        reservation: Reservation,
        store: Store,
        reasoning: Option<ReasoningInfo>,
        origin: serde_json::Value,
    ) -> Result<Arc<Dataset>> {
        let name = reservation.name.clone();
        let _guard = self.manage.lock();
        let ds = self.dataset_of(&name, DbType::Mem, store, false, Some(origin));
        *ds.reasoning.write() = reasoning;
        self.datasets.write().insert(name.clone(), ds.clone());
        if let Err(e) = self.save_registry_locked() {
            self.datasets.write().remove(&name);
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
            let kind = kind.to_string();
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
                        state.task_queue.done(&kind);
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
        let slot = q.running < self.task_queue.max();
        let kind_slot = self.task_queue.kind_free(&q, kind);
        if slot && kind_slot {
            q.start(kind);
        } else {
            // a task that has not started may always be cancelled
            task.state = task_state::QUEUED.into();
            task.cancellable = true;
            task.message = Some(if slot {
                format!("waiting for a free {kind} slot (--max-{kind}s)")
            } else {
                "waiting for a free task slot (--max-tasks)".into()
            });
        }
        self.push_task(task.clone());
        if slot && kind_slot {
            drop(q);
            run(false);
        } else {
            q.queued
                .push_back((id, kind.to_string(), Box::new(move || run(true))));
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

/// Clones running at once by default (`serve --max-clones`), within `--max-tasks`.
pub const DEFAULT_MAX_CLONES: usize = 2;

/// The background task slots: at most `max` tasks run at once (each on its own
/// thread), and at most the limit of their kind for the kinds that have one (clones:
/// `--max-clones`). The others wait, `queued`, in start order, and a freed slot goes
/// to the first waiting task that may run. Backup tasks are not counted here: they
/// wait for the slots of `--backup-max-tasks`.
pub struct TaskQueue {
    max: std::sync::atomic::AtomicUsize,
    /// tasks of a kind that may run at once, for the kinds that have a limit
    kind_max: Mutex<BTreeMap<String, usize>>,
    inner: Mutex<QueueState>,
}

/// A queued task: its id, its kind and what starts it.
type Queued = (String, String, Box<dyn FnOnce() + Send>);

#[derive(Default)]
struct QueueState {
    running: usize,
    /// running tasks by kind
    kinds: BTreeMap<String, usize>,
    queued: std::collections::VecDeque<Queued>,
}

impl QueueState {
    /// A task of `kind` takes a slot.
    fn start(&mut self, kind: &str) {
        self.running += 1;
        *self.kinds.entry(kind.to_string()).or_default() += 1;
    }
}

impl TaskQueue {
    pub fn new(max: usize) -> TaskQueue {
        TaskQueue {
            max: std::sync::atomic::AtomicUsize::new(max),
            kind_max: Mutex::new(BTreeMap::from([("clone".to_string(), DEFAULT_MAX_CLONES)])),
            inner: Mutex::new(QueueState::default()),
        }
    }

    /// Tasks that may run at once (0: no limit).
    pub fn set_max(&self, max: usize) {
        self.max.store(max, Ordering::Relaxed);
    }

    /// Tasks of `kind` that may run at once (0: only the overall limit applies).
    pub fn set_kind_max(&self, kind: &str, max: usize) {
        let mut m = self.kind_max.lock();
        if max == 0 {
            m.remove(kind);
        } else {
            m.insert(kind.to_string(), max);
        }
    }

    fn max(&self) -> usize {
        match self.max.load(Ordering::Relaxed) {
            0 => usize::MAX,
            n => n,
        }
    }

    /// Whether one more task of `kind` stays within its kind's limit.
    fn kind_free(&self, q: &QueueState, kind: &str) -> bool {
        match self.kind_max.lock().get(kind) {
            Some(&max) => q.kinds.get(kind).copied().unwrap_or(0) < max,
            None => true,
        }
    }

    /// Whether a task without a kind limit started now would run at once rather than
    /// wait. A waiting task that may run takes the first free slot, so the only tasks
    /// that wait next to a free slot are held back by their kind's limit.
    pub fn has_free_slot(&self) -> bool {
        let q = self.inner.lock();
        q.running < self.max()
    }

    /// Tasks running now and waiting.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn counts(&self) -> (usize, usize) {
        let q = self.inner.lock();
        (q.running, q.queued.len())
    }

    /// A counted task of `kind` ended: the first waiting task that may run now takes
    /// its slot.
    fn done(&self, kind: &str) {
        let mut q = self.inner.lock();
        if let Some(n) = q.kinds.get_mut(kind) {
            *n -= 1;
            if *n == 0 {
                q.kinds.remove(kind);
            }
        }
        q.running -= 1;
        let next = (0..q.queued.len()).find(|&i| self.kind_free(&q, &q.queued[i].1));
        if let Some(i) = next
            && q.running < self.max()
        {
            let (_, k, start) = q.queued.remove(i).expect("in range");
            q.start(&k);
            drop(q);
            start();
        }
    }

    /// Take a queued task out; whether it was queued.
    fn remove(&self, id: &str) -> bool {
        let mut q = self.inner.lock();
        let Some(i) = q.queued.iter().position(|(t, _, _)| t == id) else {
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
    #[cfg(feature = "backup")]
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

    /// The task as a library [`Control`](sparkles::task::Control): its cancel flag is
    /// the one `DELETE /$/tasks/{id}` sets, and its progress reports become the task's
    /// `progress` and `message`.
    pub fn control(&self) -> sparkles::task::Control {
        let h = self.clone();
        sparkles::task::Control {
            cancel: sparkles::task::Cancel::from_flag(self.cancel.clone()),
            progress: sparkles::task::Progress::new(move |p, m| h.progress(p, m)),
            deadline: None,
        }
    }

    /// Set by `DELETE /$/tasks/{id}` (for a cancellable task).
    #[cfg_attr(not(feature = "backup"), allow(dead_code))]
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
