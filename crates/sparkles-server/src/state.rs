//! Server state: dataset registry (persisted like Fuseki's `configuration/`), async tasks.

use anyhow::Result;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use sparkles::store::StoreOptions;
use std::collections::{BTreeMap, BTreeSet};
#[cfg(feature = "backup")]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Instant;

pub use sparkles::catalog::{DatasetKind as DbType, Reservation, valid_name};

#[cfg(feature = "reasoning")]
pub use sparkles::reasoning::write_record as write_reasoning_file;
pub use sparkles::reasoning::{
    AutoSetting, ReasoningRecord as ReasoningInfo, read_record as read_reasoning_file,
};

/// What [`AppState::lookup`] finds.
pub enum Lookup {
    Open(Arc<Dataset>),
    Unavailable(Unavailable),
    Missing,
}

/// Why a declared dataset does not serve (spec C01 §4.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// still opening at startup; a retry can pass
    Opening,
    /// failed to open, or closed for recovery, with the reason
    Failed(sparkles::catalog::Refusal),
}

impl Unavailable {
    pub fn state(&self) -> &'static str {
        match self {
            Unavailable::Opening => "opening",
            Unavailable::Failed(_) => "failed",
        }
    }
}

/// What a caller that is not a server administrator reads as the reason a dataset is
/// refused, since the full message names the data directory.
pub fn redacted_refusal(kind: &str) -> &'static str {
    match kind {
        "unsupported-format" => {
            "this release does not read the dataset's files; a server administrator can see why in the server log or in GET /$/server"
        }
        _ => {
            "the dataset could not be opened; a server administrator can see why in the server log or in GET /$/server"
        }
    }
}

/// What Fuseki's direct Graph Store naming allows on a dataset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectMode {
    /// reads only (`fuseki:gsp-direct-r`): a write is a `405`
    Read,
    /// reads and writes (`fuseki:gsp-direct-rw`)
    ReadWrite,
}

/// The datasets that serve direct Graph Store naming (`serve --gsp-direct-naming`), and
/// whether they take writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GspDirectNaming {
    /// the mode of the datasets that name none
    all: Option<DirectMode>,
    datasets: BTreeMap<String, DirectMode>,
}

impl GspDirectNaming {
    /// The values of `--gsp-direct-naming`. An empty value turns direct naming on for
    /// every dataset, `NAME` for one dataset, and `NAME=read` or `NAME=read-write` sets
    /// what the dataset allows. Without a mode a dataset is read-write.
    pub fn parse(values: &[String]) -> Result<GspDirectNaming, String> {
        let mut out = GspDirectNaming::default();
        for v in values {
            if v.is_empty() {
                out.all = Some(DirectMode::ReadWrite);
                continue;
            }
            let (name, mode) = match v.split_once('=') {
                None => (v.as_str(), DirectMode::ReadWrite),
                Some((n, "read")) => (n, DirectMode::Read),
                Some((n, "read-write")) => (n, DirectMode::ReadWrite),
                Some((_, m)) => {
                    return Err(format!(
                        "--gsp-direct-naming: the mode of a dataset is read or read-write, not '{m}'"
                    ));
                }
            };
            if name.is_empty() {
                return Err("--gsp-direct-naming: a dataset name is empty".into());
            }
            out.datasets.insert(name.to_string(), mode);
        }
        Ok(out)
    }

    /// No dataset serves direct naming.
    #[cfg(test)]
    pub const OFF: GspDirectNaming = GspDirectNaming {
        all: None,
        datasets: BTreeMap::new(),
    };

    /// The values of a Fuseki configuration, then those of the command line, which win
    /// for the datasets both name.
    pub fn layered(config: &[String], cli: &[String]) -> Result<GspDirectNaming, String> {
        let all: Vec<String> = config.iter().chain(cli).cloned().collect();
        GspDirectNaming::parse(&all)
    }

    /// Direct naming on every dataset that no entry names, with read and write.
    #[cfg(test)]
    pub fn everywhere() -> GspDirectNaming {
        GspDirectNaming {
            all: Some(DirectMode::ReadWrite),
            datasets: BTreeMap::new(),
        }
    }

    /// What dataset `name` allows, if it serves direct naming.
    pub fn mode(&self, name: &str) -> Option<DirectMode> {
        self.datasets.get(name).copied().or(self.all)
    }

    /// Whether any dataset serves direct naming.
    #[cfg(test)]
    pub fn is_on(&self) -> bool {
        self.all.is_some() || !self.datasets.is_empty()
    }
}

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

/// Default of `serve --reason-max-inferred`, the reasoner's own default of 50 million
/// derived triples (`sparkles_reasoner::DEFAULT_MAX_INFERRED`).
pub const DEFAULT_REASON_MAX_INFERRED: usize = 50_000_000;
/// The facts one query may derive with query-time reasoning by default (spec F17 §8).
pub const DEFAULT_REASON_MAX_DERIVED: usize = 1_000_000;

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

/// Removes a dataset name from [`AppState`]'s leaving set when dropped.
struct Leaving<'a> {
    st: &'a AppState,
    name: String,
}

impl Drop for Leaving<'_> {
    fn drop(&mut self) {
        self.st.leaving.lock().remove(&self.name);
    }
}

pub struct AppState {
    pub data_dir: PathBuf,
    pub catalog: sparkles::Catalog,
    /// the disclosure guard of reports graphs (spec C22 §6), which only authenticated
    /// principals carry
    #[cfg_attr(not(feature = "auth"), allow(dead_code))]
    pub graphrag_guard: Arc<crate::graphrag::guard::Guard>,
    /// the vector index projections the server keeps (spec U11 §3.3)
    pub projections: crate::projection::Projections,
    /// GraphRAG builds waiting for confirmation, and the figures of the metrics
    #[cfg(feature = "mcp")]
    pub graphrag: Arc<crate::graphrag::runtime::Runtime>,
    /// HTTP routing state and observers, keyed by dataset name. Only map lookups and
    /// changes happen under this lock, never catalog work or file system work.
    routing: Mutex<BTreeMap<String, Arc<Dataset>>>,
    /// Datasets being deleted or renamed. Lookups do not find them, so no request can
    /// take a handle that would make the delete or rename fail.
    leaving: Mutex<BTreeSet<String>>,
    /// Kept independently so completed workers release server ownership before
    /// publishing their terminal task status.
    pub tasks: Arc<Mutex<Vec<Task>>>,
    task_counter: AtomicU64,
    pub started: Instant,
    pub started_at: String,
    pub store_opts: StoreOptions,
    /// the memory profile and the admission wait of the query memory pool
    pub admission: crate::server_memory::Admission,
    pub default_timeout: std::time::Duration,
    /// how a query runs when its request names no `execution` (`serve --default-execution`)
    pub default_execution: sparkles::sparql::ExecutionMode,
    pub read_only: bool,
    /// honor `validate=false` on writes (skips write-time validation)
    pub allow_unvalidated_writes: bool,
    /// response compression
    pub http_compression: crate::compress::HttpCompression,
    pub allow_service: bool,
    /// where SERVICE and `LOAD <http…>` may connect (`serve --outbound-*`)
    pub outbound: sparkles::outbound::OutboundPolicy,
    /// where cursor queries write their spill files (`serve --spill`; none when off)
    pub spill: Option<Arc<sparkles::spill::Spill>>,
    /// which files `LOAD <file:…>` may read (`serve --load-dir`; none without it)
    pub file_loads: sparkles::sparql::FileLoads,
    /// cap on the classes, and separately the predicates, of one schema report
    pub schema_max_entries: usize,
    /// the store size above which the schema reports of the tools come from the index
    /// statistics (`--schema-statistics-above`; `None`: always scan)
    pub schema_statistics_above: Option<u64>,
    /// what the imports of one ShEx schema may read (`--shex-max-imports` and the like)
    #[cfg(feature = "shex")]
    pub shex_import_limits: sparkles_shex::resolve::ImportLimits,
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
    /// whether a server with a dataset that failed to open still reports ready
    /// (`serve --ready-ignore-failed`; spec C01 §4.1)
    pub ready_ignore_failed: bool,
    /// the datasets whose startup has not finished: those of `serve --mem`, `--loc` and
    /// `--fuseki-config` until they are attached, and those that startup configures
    /// (`--text`, `--geo`, `--validate`, `--rdfs`, data files) until it has; requests
    /// find them opening meanwhile (see [`crate::startup`])
    pub startup_pending: Mutex<BTreeMap<String, DbType>>,
    /// whether `startup_pending` may hold names, so that lookups skip its lock after
    /// startup
    holding: AtomicBool,
    /// authentication and authorization (`serve --auth-config`); `None`: open
    pub auth: Option<Arc<crate::auth::Auth>>,
    /// browser origins that may call the API cross-origin, without credentials
    /// (`serve --cors-origin`; with auth, besides the configuration's `cors.origins`)
    pub cors_origins: Vec<String>,
    /// the `Host` names answered without auth (`serve --public-host`)
    pub hosts: crate::exposure::Hosts,
    /// the TCP listener `serve` bound, once it has, for recognising SERVICE calls to
    /// this server (see [`crate::http::self_service`])
    pub listener: std::sync::OnceLock<crate::http::self_service::Listener>,
    /// the MapLibre style of the UI's maps (`serve --map-style-url`); `None`: the
    /// bundled basemap
    pub map_style_url: Option<String>,
    /// Fuseki's Graph Store direct naming (`serve --gsp-direct-naming`): `/{ds}/{path}`
    /// names the graph whose IRI is the request URL
    pub gsp_direct_naming: GspDirectNaming,
    /// automatic re-materialization of stale inferences (`serve --auto-reason`)
    pub auto_reason: Option<crate::reasoning::AutoReason>,
    /// the largest closure a dataset keeps in memory for incremental reasoning, in
    /// derived triples (`serve --reason-cache-triples`)
    pub reason_cache_triples: usize,
    /// the settings of incremental views and live queries (`serve --max-live-queries`,
    /// `--view-pass-timeout`, …)
    pub views_options: sparkles::incr::RuntimeOptions,
    /// the derived triples a materialization may produce before it fails
    /// (`serve --reason-max-inferred`, 0 on the command line for no limit, which is
    /// `usize::MAX` here); a request may ask for less with `maxInferred`
    pub reason_max_inferred: usize,
    /// the facts a query's query-time reasoning may derive (`serve --reason-max-derived`,
    /// 0 on the command line for no limit); a request may ask for less with `maxDerived`
    pub reason_max_derived: usize,
    /// automatic compaction (`serve --auto-compact-*`) and what is known of each
    /// dataset's compactions
    pub compaction: crate::compaction::AutoCompact,
    /// backup repositories and policies (`serve`; `None` for embedded use)
    #[cfg(feature = "backup")]
    pub backup: Option<Arc<crate::backup::BackupState>>,
    /// background task slots (`serve --max-tasks`)
    pub task_queue: TaskQueue,
    /// `POST /$/format` (`serve --format-*`)
    #[cfg(feature = "fmt")]
    pub format: FormatConf,
    /// the MCP endpoint `/$/mcp` (`serve --mcp`); `None`: not mounted
    #[cfg(feature = "mcp")]
    pub mcp: Option<Arc<crate::mcp::http::HttpConf>>,
    /// the C18 files of in-memory datasets (`assist`)
    pub volatile: crate::assist::Volatile,
    /// the model providers and role lists (`serve --model-config`, spec C18 §3.4),
    /// replaced on SIGHUP (spec C19 §7); read them with [`models`](Self::models)
    pub models: arc_swap::ArcSwapOption<crate::models::Models>,
    /// the model store and the local embedding models (`serve --models-dir`, spec F12)
    pub local_models: Option<Arc<crate::model_store::LocalModels>>,
    /// the declared settings file (`serve --settings`, spec C19 §5)
    pub settings: crate::settings::Settings,
    /// recent asks, usage counts and daily token counts (spec C18 §5.5)
    pub asks: crate::assistant::Runtime,
    /// the queue, the conditions and the delivery status of outbound notifications
    /// (spec C21)
    pub notifier: crate::notify::Notifier,
    /// ingestion tasks and the PDF workers (spec C18 Phase 4)
    #[cfg(feature = "mcp")]
    pub ingest: crate::ingest::Runtime,
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
#[cfg(feature = "backup")]
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
#[cfg(any(feature = "backup", feature = "auth"))]
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
    /// the disk one query's spill files may reserve (`--query-spill-mb`)
    pub query_spill_bytes: Option<u64>,
    /// the largest `timeout` a request may ask for (never below the server's default)
    pub max_timeout: Option<std::time::Duration>,
    /// commits one request of `GET /$/commits/{ds}/timeline` reads
    pub max_timeline_commits: u64,
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
            query_spill_bytes: Some(16 << 30),
            max_timeout: Some(std::time::Duration::from_secs(1800)),
            max_timeline_commits: 10_000_000,
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

/// The disclosure guard of a catalog's datasets, which finds a dataset's directory
/// through the catalog.
impl AppState {
    /// Moves whenever the access policy is reloaded or another change can alter the
    /// rights of more than one credential (spec C22 §6.3). Issuing a credential and
    /// revoking one credential leave it unchanged.
    pub fn access_epoch(&self) -> u64 {
        #[cfg(feature = "auth")]
        return self.auth.as_ref().map_or(0, |a| a.epoch());
        #[cfg(not(feature = "auth"))]
        0
    }

    /// Whether the authority a principal's views were decided under still holds:
    /// the global generation is unchanged, and the session or token it was
    /// authenticated with has neither expired nor been revoked. Long-lived reads
    /// check it before they send more data. The expiry is the instant recorded when
    /// the principal was authenticated, including a session's idle timeout, so the
    /// check reads the clock rather than a store.
    pub fn access_current(&self, p: &crate::auth::Principal) -> bool {
        let Some(epoch) = p.access_epoch() else {
            return true;
        };
        if epoch != self.access_epoch() {
            return false;
        }
        #[cfg(feature = "auth")]
        if let Some(a) = &self.auth {
            return !a.credential_expired(p) && a.credential_live(p);
        }
        true
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(
        data_dir: &Path,
        store_opts: StoreOptions,
        default_timeout: std::time::Duration,
    ) -> Result<AppState> {
        AppState::with_memory(data_dir, store_opts, default_timeout, None)
    }

    /// [`new`](Self::new) with a query memory pool of `pool_bytes` for the datasets'
    /// queries (`None`: no pool).
    pub fn with_memory(
        data_dir: &Path,
        store_opts: StoreOptions,
        default_timeout: std::time::Duration,
        pool_bytes: Option<u64>,
    ) -> Result<AppState> {
        std::fs::create_dir_all(data_dir.join("databases"))?;
        let mut options: sparkles::CatalogOptions = store_opts.clone().into();
        options.memory.pool_bytes = pool_bytes;
        let catalog = sparkles::Catalog::open(data_dir, options)?;
        Ok(AppState::with_catalog(
            data_dir,
            catalog,
            store_opts,
            default_timeout,
        ))
    }

    /// [`with_memory`](Self::with_memory) without opening the registered persistent
    /// datasets, which are returned in registry order. The catalog lists them as
    /// opening until [`crate::startup`] opens them (spec C01 §4.1, Phase 2).
    pub fn deferred(
        data_dir: &Path,
        store_opts: StoreOptions,
        default_timeout: std::time::Duration,
        pool_bytes: Option<u64>,
    ) -> Result<(AppState, Vec<String>)> {
        std::fs::create_dir_all(data_dir.join("databases"))?;
        let mut options: sparkles::CatalogOptions = store_opts.clone().into();
        options.memory.pool_bytes = pool_bytes;
        let (catalog, pending) = sparkles::Catalog::open_deferred(data_dir, options)?;
        Ok((
            AppState::with_catalog(data_dir, catalog, store_opts, default_timeout),
            pending,
        ))
    }

    fn with_catalog(
        data_dir: &Path,
        catalog: sparkles::Catalog,
        store_opts: StoreOptions,
        default_timeout: std::time::Duration,
    ) -> AppState {
        AppState {
            data_dir: data_dir.to_path_buf(),
            graphrag_guard: Arc::new(crate::graphrag::guard::Guard::new()),
            projections: Default::default(),
            #[cfg(feature = "mcp")]
            graphrag: Default::default(),
            catalog,
            admission: Default::default(),
            routing: Mutex::new(BTreeMap::new()),
            leaving: Mutex::new(BTreeSet::new()),
            tasks: Arc::new(Mutex::new(Vec::new())),
            task_counter: AtomicU64::new(1),
            started: Instant::now(),
            started_at: now(),
            store_opts,
            default_timeout,
            default_execution: Default::default(),
            read_only: false,
            allow_service: true,
            outbound: Default::default(),
            spill: None,
            file_loads: sparkles::sparql::FileLoads::Disabled,
            schema_max_entries: sparkles::schema::DEFAULT_MAX_ENTRIES,
            schema_statistics_above: Some(sparkles::schema::DEFAULT_STATISTICS_ABOVE),
            #[cfg(feature = "shex")]
            shex_import_limits: Default::default(),
            #[cfg(feature = "graphql")]
            graphql_limits: Default::default(),
            limits: Limits::default(),
            access_log: true,
            metrics: crate::obs::Metrics::new(true, 100),
            rate_limit: None,
            phase: AtomicU8::new(crate::obs::Phase::Starting as u8),
            ready_ignore_failed: false,
            startup_pending: Default::default(),
            holding: AtomicBool::new(false),
            auth: None,
            cors_origins: Vec::new(),
            hosts: crate::exposure::Hosts::default(),
            listener: Default::default(),
            map_style_url: None,
            gsp_direct_naming: GspDirectNaming::default(),
            allow_unvalidated_writes: false,
            http_compression: Default::default(),
            auto_reason: None,
            reason_cache_triples: DEFAULT_REASON_CACHE_TRIPLES,
            views_options: Default::default(),
            reason_max_inferred: DEFAULT_REASON_MAX_INFERRED,
            reason_max_derived: DEFAULT_REASON_MAX_DERIVED,
            compaction: Default::default(),
            #[cfg(feature = "backup")]
            backup: None,
            task_queue: TaskQueue::new(DEFAULT_MAX_TASKS),
            #[cfg(feature = "fmt")]
            format: FormatConf::default(),
            #[cfg(feature = "mcp")]
            mcp: None,
            volatile: Default::default(),
            models: Default::default(),
            local_models: None,
            settings: crate::settings::Settings::in_dir(data_dir),
            asks: Default::default(),
            notifier: Default::default(),
            #[cfg(feature = "mcp")]
            ingest: Default::default(),
        }
    }

    /// State without a data directory or registry, for embedded use (`sparkles mcp`):
    /// datasets are only [`attach`](Self::attach)ed, and nothing is ever written besides
    /// the datasets themselves.
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    pub fn standalone(store_opts: StoreOptions, default_timeout: std::time::Duration) -> AppState {
        let catalog = sparkles::Catalog::memory(store_opts.clone().into());
        AppState {
            data_dir: PathBuf::new(),
            graphrag_guard: Arc::new(crate::graphrag::guard::Guard::new()),
            projections: Default::default(),
            #[cfg(feature = "mcp")]
            graphrag: Default::default(),
            catalog,
            admission: Default::default(),
            routing: Mutex::new(BTreeMap::new()),
            leaving: Mutex::new(BTreeSet::new()),
            tasks: Arc::new(Mutex::new(Vec::new())),
            task_counter: AtomicU64::new(1),
            started: Instant::now(),
            started_at: now(),
            store_opts,
            default_timeout,
            default_execution: Default::default(),
            read_only: false,
            allow_service: false,
            outbound: Default::default(),
            spill: None,
            file_loads: sparkles::sparql::FileLoads::Disabled,
            schema_max_entries: sparkles::schema::DEFAULT_MAX_ENTRIES,
            schema_statistics_above: Some(sparkles::schema::DEFAULT_STATISTICS_ABOVE),
            #[cfg(feature = "shex")]
            shex_import_limits: Default::default(),
            #[cfg(feature = "graphql")]
            graphql_limits: Default::default(),
            limits: Limits::default(),
            access_log: false,
            metrics: crate::obs::Metrics::new(false, 100),
            phase: AtomicU8::new(crate::obs::Phase::Ready as u8),
            ready_ignore_failed: false,
            startup_pending: Default::default(),
            holding: AtomicBool::new(false),
            auto_reason: None,
            reason_cache_triples: DEFAULT_REASON_CACHE_TRIPLES,
            views_options: Default::default(),
            reason_max_inferred: DEFAULT_REASON_MAX_INFERRED,
            reason_max_derived: DEFAULT_REASON_MAX_DERIVED,
            compaction: Default::default(),
            #[cfg(feature = "backup")]
            backup: None,
            task_queue: TaskQueue::new(DEFAULT_MAX_TASKS),
            #[cfg(feature = "fmt")]
            format: FormatConf::default(),
            #[cfg(feature = "mcp")]
            mcp: None,
            volatile: Default::default(),
            models: Default::default(),
            local_models: None,
            settings: Default::default(),
            asks: Default::default(),
            notifier: Default::default(),
            #[cfg(feature = "mcp")]
            ingest: Default::default(),
            rate_limit: None,
            auth: None,
            cors_origins: Vec::new(),
            hosts: crate::exposure::Hosts::default(),
            listener: Default::default(),
            map_style_url: None,
            gsp_direct_naming: GspDirectNaming::default(),
            allow_unvalidated_writes: false,
            http_compression: Default::default(),
        }
    }

    /// The model providers of this moment. A request keeps the ones it started with
    /// when a reload replaces them.
    pub fn models(&self) -> Option<Arc<crate::models::Models>> {
        self.models.load_full()
    }

    pub fn set_models(&self, m: Option<Arc<crate::models::Models>>) {
        self.models.store(m);
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
        let dataset = main.dataset.branch(branch)?;
        let mut ds = self.wrap_dataset(&main.name, main.kind, dataset, main.ephemeral);
        Arc::get_mut(&mut ds).expect("just made").branch = Some(BranchOf {
            name: RwLock::new(branch.to_string()),
            main: Arc::downgrade(main),
        });
        Ok(ds)
    }

    fn wrap_dataset(
        &self,
        name: &str,
        kind: DbType,
        dataset: sparkles::Dataset,
        ephemeral: bool,
    ) -> Arc<Dataset> {
        let validation_metrics = Arc::new(crate::obs::ValidationMetrics::new(name));
        dataset
            .store()
            .set_guard_observer(Some(validation_metrics.clone()));
        // loaded data does not bind the prefixes the settings file keeps (spec C20 §3.4)
        dataset
            .store()
            .set_prefix_filter(Some(self.settings.prefix_filter(name)));
        crate::http::start_views(self, name, &dataset);
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

    /// The routing objects of the datasets open now, without opening any.
    pub fn open_datasets(&self) -> Vec<(String, Arc<Dataset>)> {
        self.routing
            .lock()
            .iter()
            .map(|(n, ds)| (n.clone(), ds.clone()))
            .collect()
    }

    /// A snapshot of the server's routing objects for the catalog's datasets.
    pub fn datasets(&self) -> BTreeMap<String, Arc<Dataset>> {
        self.catalog
            .list()
            .into_iter()
            .filter_map(|d| self.get(&d.name).map(|ds| (d.name, ds)))
            .collect()
    }

    #[cfg(feature = "reasoning")]
    pub fn save_registry(&self) -> Result<()> {
        Ok(self.catalog.save()?)
    }

    pub fn phase(&self) -> crate::obs::Phase {
        crate::obs::Phase::from_u8(self.phase.load(Ordering::SeqCst))
    }

    pub fn set_phase(&self, p: crate::obs::Phase) {
        self.phase.store(p as u8, Ordering::SeqCst);
    }

    /// Move from `Starting` to `Ready`, unless the server began to drain meanwhile.
    pub fn mark_ready(&self) -> bool {
        self.phase
            .compare_exchange(
                crate::obs::Phase::Starting as u8,
                crate::obs::Phase::Ready as u8,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    /// Why the dataset `name`, which [`get`](Self::get) does not find, cannot serve:
    /// it is still opening, or it failed to open. `None` for a name nothing declares.
    /// The catalog records a dataset's final state before it stops listing it as
    /// opening, so the checks run in that order.
    pub fn unavailable(&self, name: &str) -> Option<Unavailable> {
        if self.catalog.is_opening(name) {
            return Some(Unavailable::Opening);
        }
        if let Some(r) = self.catalog.refusal(name) {
            return Some(Unavailable::Failed(r));
        }
        if self.catalog.is_expected(name) || self.held(name) {
            return Some(Unavailable::Opening);
        }
        None
    }

    /// The dataset `name` for a request: open, unavailable, or missing. A dataset that
    /// is published between the two lookups is found by a second one, so a request
    /// that straddles its opening never gets a missing dataset.
    pub fn lookup(&self, name: &str) -> Lookup {
        if let Some(ds) = self.get(name) {
            return Lookup::Open(ds);
        }
        #[cfg(test)]
        crate::startup::lookup_hook(name);
        if let Some(u) = self.unavailable(name) {
            return Lookup::Unavailable(u);
        }
        match self.get(name) {
            Some(ds) => Lookup::Open(ds),
            None => Lookup::Missing,
        }
    }

    /// Whether `name` is a dataset of this server, open or not.
    pub fn declared(&self, name: &str) -> bool {
        !matches!(self.lookup(name), Lookup::Missing)
    }

    /// Whether startup still holds `name` back from requests.
    fn held(&self, name: &str) -> bool {
        self.holding.load(Ordering::SeqCst) && self.startup_pending.lock().contains_key(name)
    }

    /// Hold `name` back from requests until [`release_startup`](Self::release_startup):
    /// it answers as opening.
    pub fn hold_for_startup(&self, name: &str, kind: DbType) {
        self.startup_pending.lock().insert(name.to_string(), kind);
        self.holding.store(true, Ordering::SeqCst);
    }

    /// Serve every dataset that startup held back.
    pub fn release_startup(&self) {
        self.startup_pending.lock().clear();
        self.holding.store(false, Ordering::SeqCst);
    }

    /// The datasets that do not serve yet, by name: the registered ones that wait to
    /// open, with those startup has not attached, and the ones that failed to open.
    pub fn unavailable_datasets(&self) -> Vec<(String, DbType, Unavailable)> {
        let mut out: Vec<(String, DbType, Unavailable)> = self
            .catalog
            .opening()
            .into_iter()
            .map(|n| (n, DbType::Persistent, Unavailable::Opening))
            .collect();
        let refusals = self.catalog.refusals();
        out.extend(
            self.startup_pending
                .lock()
                .iter()
                .filter(|(n, _)| {
                    !refusals.iter().any(|(r, _)| r == *n) && !self.catalog.is_opening(n)
                })
                .map(|(n, k)| (n.clone(), *k, Unavailable::Opening)),
        );
        out.extend(
            refusals
                .into_iter()
                .map(|(n, r)| (n, DbType::Persistent, Unavailable::Failed(r))),
        );
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn get(&self, name: &str) -> Option<Arc<Dataset>> {
        if self.leaving.lock().contains(name) {
            return None;
        }
        // startup's own steps see the datasets it holds back from requests
        if self.held(name) && !crate::startup::has_access() {
            return None;
        }
        let dataset = self.catalog.get(name)?;
        let info = self.catalog.info(name)?;
        let mut routing = self.routing.lock();
        if let Some(ds) = routing.get(name)
            && ds.store.dataset_id() == dataset.dataset_id()
        {
            return Some(ds.clone());
        }
        // A delete or rename may have started since the check above. Its wrapper is
        // not cached, and the catalog refuses the delete or rename while it lives.
        if self.leaving.lock().contains(name) {
            return None;
        }
        let ds = self.wrap_dataset(name, info.kind, dataset, info.attached);
        // A request that passed the restore guard can reach this lookup after the
        // routing cache was drained. Its temporary wrapper must not keep the old
        // library dataset alive after that request finishes.
        if self.catalog.restoring_by(name).is_none() {
            routing.insert(name.to_string(), ds.clone());
        }
        Some(ds)
    }

    #[cfg(feature = "backup")]
    pub fn forget_routing(&self, name: &str) {
        self.routing.lock().remove(name);
    }

    /// Every dataset, and the open branches of each (for the background upkeep:
    /// compaction and history ticks).
    pub fn datasets_and_branches(&self) -> Vec<Arc<Dataset>> {
        let mains: Vec<Arc<Dataset>> = self.datasets().values().cloned().collect();
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
        self.catalog.set_defaults(sparkles::DatasetOptions {
            store: self.store_opts.clone(),
            closure_cache_triples: self.reason_cache_triples,
            ..Default::default()
        });
        self.catalog.create(
            name,
            &sparkles::catalog::CreateDataset {
                kind,
                ..Default::default()
            },
        )?;
        Ok(self.get(name).expect("created"))
    }
    /// Startup attaches its datasets with [`attach_expected`](Self::attach_expected),
    /// so outside the tests only `sparkles ingest` (with `mcp`) attaches this way.
    #[cfg(any(test, feature = "mcp"))]
    pub fn attach(&self, name: &str, kind: DbType, loc: Option<&Path>) -> Result<Arc<Dataset>> {
        let source = match kind {
            DbType::Memory => sparkles::catalog::Attach::Memory,
            DbType::Persistent => sparkles::catalog::Attach::Directory(
                loc.map(Path::to_path_buf)
                    .unwrap_or_else(|| self.data_dir.join("databases").join(name)),
            ),
        };
        self.catalog.set_defaults(sparkles::DatasetOptions {
            store: self.store_opts.clone(),
            closure_cache_triples: self.reason_cache_triples,
            ..Default::default()
        });
        self.catalog.attach(name, source)?;
        Ok(self.get(name).expect("attached"))
    }
    /// [`attach`](Self::attach) under a name that startup took with
    /// [`sparkles::Catalog::expect`].
    pub fn attach_expected(
        &self,
        name: &str,
        kind: DbType,
        loc: Option<&Path>,
    ) -> Result<Arc<Dataset>> {
        let source = match kind {
            DbType::Memory => sparkles::catalog::Attach::Memory,
            DbType::Persistent => sparkles::catalog::Attach::Directory(
                loc.map(Path::to_path_buf)
                    .unwrap_or_else(|| self.data_dir.join("databases").join(name)),
            ),
        };
        self.catalog.set_defaults(sparkles::DatasetOptions {
            store: self.store_opts.clone(),
            closure_cache_triples: self.reason_cache_triples,
            ..Default::default()
        });
        self.catalog.attach_expected(name, source)?;
        Ok(self.get(name).expect("attached"))
    }
    /// Mark `name` as leaving until the guard drops. A second delete or rename of the
    /// same dataset is refused meanwhile.
    fn leave(&self, name: &str) -> Result<Leaving<'_>> {
        if !self.leaving.lock().insert(name.to_string()) {
            return Err(sparkles::Error::Conflict(format!(
                "dataset /{name} is already being deleted or renamed"
            ))
            .into());
        }
        Ok(Leaving {
            st: self,
            name: name.to_string(),
        })
    }

    /// Delete a dataset. Lookups stop finding it first, then its cached routing object
    /// is dropped and the catalog unregisters it and removes its files. The routing
    /// lock is held only for the map change, so other requests go on during a slow
    /// delete. Requests still holding the dataset make the delete fail with a conflict.
    pub fn delete(&self, name: &str) -> Result<bool> {
        self.delete_with_audit_identity(name)
            .map(|(deleted, _)| deleted)
    }

    /// Capture only scalar identity inside the existing leave boundary. An audit
    /// handle must neither keep the dataset in use nor sample racy deletion counts.
    pub(crate) fn delete_with_audit_identity(
        &self,
        name: &str,
    ) -> Result<(bool, Option<uuid::Uuid>)> {
        let leaving = self.leave(name)?;
        let cached = self.routing.lock().remove(name);
        // Only a managed persistent dataset has files that an old handle could write.
        if cached.as_ref().is_some_and(|ds| {
            ds.kind == DbType::Persistent && !ds.ephemeral && Arc::strong_count(ds) > 1
        }) {
            self.routing
                .lock()
                .insert(name.to_string(), cached.expect("checked"));
            return Err(sparkles::Error::Conflict(format!(
                "dataset /{name} still has live requests"
            ))
            .into());
        }
        let offline = cached
            .as_ref()
            .is_some_and(|ds| ds.offline.load(Ordering::Relaxed));
        let identity = cached
            .as_ref()
            .map(|ds| ds.store.dataset_id())
            .or_else(|| self.catalog.info(name).map(|info| info.id));
        drop(cached);
        #[cfg(test)]
        tests::delete_hook(name);
        let deleted = self.catalog.delete(name);
        drop(leaving);
        match deleted {
            Ok(deleted) => {
                if deleted {
                    self.metrics.forget(name);
                    self.volatile.forget(name);
                }
                Ok((deleted, deleted.then_some(identity).flatten()))
            }
            Err(e) => {
                if let Some(ds) = self.get(name) {
                    ds.offline.store(offline, Ordering::Relaxed);
                }
                Err(e.into())
            }
        }
    }

    /// The configuration that a rename of `from` would leave pointing at nothing, and
    /// which the server cannot rewrite: backup policies of the config file that name
    /// the dataset exactly.
    pub fn rename_blockers(&self, from: &str) -> Vec<String> {
        #[cfg(feature = "backup")]
        if let Some(b) = &self.backup {
            return crate::backup::policies::config_policies_naming(b, from)
                .into_iter()
                .map(|p| format!("backup policy {p} of the config file names /{from}"))
                .collect();
        }
        let _ = from;
        Vec::new()
    }

    /// Rename a dataset and carry the server's state kept under its name over to the
    /// new name: the request metrics, the compaction scheduler's state, a pending
    /// automatic reasoning run and the backup policies made through the API that name
    /// it exactly. Policies that select it with a glob are left as they are.
    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        let blockers = self.rename_blockers(from);
        if !blockers.is_empty() {
            return Err(sparkles::Error::Conflict(format!(
                "rename is blocked by {}; change it there first",
                blockers.join("; ")
            ))
            .into());
        }
        let leaving = self.leave(from)?;
        let cached = self.routing.lock().remove(from);
        if cached.as_ref().is_some_and(|ds| Arc::strong_count(ds) > 1) {
            self.routing
                .lock()
                .insert(from.to_string(), cached.expect("checked"));
            return Err(sparkles::Error::Conflict(format!(
                "dataset /{from} still has live requests"
            ))
            .into());
        }
        let offline = cached
            .as_ref()
            .is_some_and(|ds| ds.offline.load(Ordering::Relaxed));
        drop(cached);
        let result = self.catalog.rename(from, to);
        drop(leaving);
        match result {
            Ok(ds) => {
                drop(ds);
                self.metrics.rename(from, to);
                self.volatile.rename(from, to);
                self.compaction.rename(from, to);
                if let Some(auto) = &self.auto_reason {
                    auto.rename(from, to);
                }
                #[cfg(feature = "backup")]
                if let Some(b) = &self.backup
                    && let Err(e) = crate::backup::policies::rename_dataset(b, from, to)
                {
                    tracing::warn!("backup policies still name /{from}: {e:#}");
                }
                if let Some(ds) = self.get(to) {
                    ds.offline.store(offline, Ordering::Relaxed);
                }
                Ok(())
            }
            Err(e) => {
                if let Some(ds) = self.get(from) {
                    ds.offline.store(offline, Ordering::Relaxed);
                }
                Err(e.into())
            }
        }
    }
    pub fn reserved_by(&self, name: &str) -> Option<String> {
        self.catalog.reserved_by(name)
    }
    pub fn reserve(
        self: &Arc<Self>,
        name: &str,
        task: &str,
    ) -> std::result::Result<Reservation, String> {
        self.catalog
            .reserve(name, sparkles::catalog::ReservationKind::Clone, task)
            .map_err(|e| e.to_string())
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
                    #[cfg(test)]
                    tests::task_start_hook(&kind);
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
                        // A queued task cancelled as its worker starts never calls
                        // work, but its captures may still own the server or a name.
                        drop(work);
                        Err(sparkles::Error::Cancelled.into())
                    };
                    // Terminal status must outlive the worker's server ownership.
                    // Otherwise a caller can observe completion, close its last
                    // handle and still race this thread for the catalog lock.
                    let tasks = state.tasks.clone();
                    drop(handle);
                    if counted {
                        state.task_queue.done(&kind);
                    }
                    drop(state);
                    finish(&tasks, &id, r);
                    #[cfg(test)]
                    tests::task_retirement_hook(&kind);
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
    #[cfg(test)]
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
    #[cfg(test)]
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    type Hook = (String, Box<dyn FnOnce() + Send>);
    static DELETE_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
    type TaskHooks = Mutex<BTreeMap<String, Box<dyn FnOnce() + Send>>>;
    static TASK_RETIREMENT_HOOK: TaskHooks = Mutex::new(BTreeMap::new());
    static TASK_START_HOOK: TaskHooks = Mutex::new(BTreeMap::new());

    pub(crate) fn task_retirement_hook(kind: &str) {
        run_task_hook(&TASK_RETIREMENT_HOOK, kind);
    }

    pub(crate) fn task_start_hook(kind: &str) {
        run_task_hook(&TASK_START_HOOK, kind);
    }

    fn run_task_hook(hook: &TaskHooks, kind: &str) {
        let hook = hook.lock().remove(kind);
        if let Some(f) = hook {
            f();
        }
    }

    #[test]
    fn finished_tasks_release_server_ownership_before_worker_retirement() {
        let dir = tempfile::tempdir().unwrap();
        for (kind, expected) in [
            ("retirement-success", task_state::DONE),
            ("retirement-error", task_state::FAILED),
            ("retirement-cancelled", task_state::CANCELLED),
            ("backup-retirement", task_state::DONE),
        ] {
            let st = Arc::new(
                AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30))
                    .unwrap(),
            );
            if kind == "retirement-success" {
                drop(st.create("prod", DbType::Persistent).unwrap());
            }
            let tasks = st.tasks.clone();
            let (retired, retired_rx) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            TASK_RETIREMENT_HOOK.lock().insert(
                kind.to_string(),
                Box::new(move || {
                    retired.send(()).unwrap();
                    let _ = release_rx.recv_timeout(Duration::from_secs(30));
                }),
            );
            let task = st.start_task(kind, "", move |_| {
                match expected {
                    task_state::FAILED => anyhow::bail!("synthetic task failure"),
                    task_state::CANCELLED => return Err(sparkles::Error::Cancelled.into()),
                    _ => {}
                }
                Ok("completed".into())
            });
            retired_rx.recv_timeout(Duration::from_secs(30)).unwrap();
            let state = tasks
                .lock()
                .iter()
                .find(|t| t.id == task.id)
                .unwrap()
                .state
                .clone();
            assert_eq!(state, expected);
            let owners = Arc::strong_count(&st);
            drop(st);
            // Hold the retired worker open while proving the catalog is available.
            let reopened =
                AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30))
                    .expect("finished task must release the catalog before publishing completion");
            assert_eq!(
                owners, 1,
                "finished task must release every worker-owned server handle"
            );
            release.send(()).unwrap();
            drop(reopened);
        }
    }

    /// Runs the hook installed for dataset `name`, where a delete starts its catalog
    /// work.
    pub(crate) fn delete_hook(name: &str) {
        let hook = {
            let mut h = DELETE_HOOK.lock();
            if h.as_ref().is_some_and(|(n, _)| n == name) {
                h.take()
            } else {
                None
            }
        };
        if let Some((_, f)) = hook {
            f();
        }
    }

    #[test]
    fn a_cancelled_start_drops_unrun_work_before_terminal_status() {
        let dir = tempfile::tempdir().unwrap();
        let st = Arc::new(
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
        );
        st.task_queue.set_max(1);
        let (working, working_rx) = mpsc::channel();
        let (finish_first, finish_first_rx) = mpsc::channel();
        let first = st.start_task("retirement-blocker", "", move |_| {
            working.send(()).unwrap();
            finish_first_rx
                .recv_timeout(Duration::from_secs(30))
                .unwrap();
            Ok("done".into())
        });
        working_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        let kind = "retirement-cancelled-start";
        let (starting, starting_rx) = mpsc::channel();
        let (start, start_rx) = mpsc::channel();
        TASK_START_HOOK.lock().insert(
            kind.to_string(),
            Box::new(move || {
                starting.send(()).unwrap();
                let _ = start_rx.recv_timeout(Duration::from_secs(30));
            }),
        );
        let (retired, retired_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        TASK_RETIREMENT_HOOK.lock().insert(
            kind.to_string(),
            Box::new(move || {
                retired.send(()).unwrap();
                let _ = release_rx.recv_timeout(Duration::from_secs(30));
            }),
        );
        let captured = st.clone();
        let task = st.start_task_opts(st.next_task_id(), kind, "", None, true, move |_| {
            // The unrun closure retains a real catalog-owning server handle.
            drop(captured);
            panic!("cancelled queued work must not run");
        });
        assert_eq!(task.state, task_state::QUEUED);
        finish_first.send(()).unwrap();
        starting_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while st
            .tasks
            .lock()
            .iter()
            .any(|t| t.id == first.id && t.active())
        {
            assert!(Instant::now() < deadline, "first task did not finish");
            std::thread::yield_now();
        }
        // The task left the queue, but its worker has not checked cancellation.
        st.cancel_task(&task.id).unwrap().unwrap();
        start.send(()).unwrap();
        retired_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(
            st.tasks
                .lock()
                .iter()
                .find(|t| t.id == task.id)
                .unwrap()
                .state,
            task_state::CANCELLED
        );
        assert_eq!(
            Arc::strong_count(&st),
            1,
            "cancelled unrun work must release its captured server handle"
        );
        drop(st);
        let reopened = AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30))
            .expect("cancelled unrun work must release its catalog before terminal status");
        release.send(()).unwrap();
        drop(reopened);
    }

    #[test]
    fn a_slow_delete_does_not_block_lookups_of_other_datasets() {
        let dir = tempfile::tempdir().unwrap();
        let st = Arc::new(
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
        );
        let target = "slow-delete-target";
        drop(st.create(target, DbType::Persistent).unwrap());
        drop(st.create("bystander", DbType::Persistent).unwrap());
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        *DELETE_HOOK.lock() = Some((
            target.to_string(),
            Box::new(move || {
                entered.send(()).unwrap();
                release_rx.recv().unwrap();
            }),
        ));
        let deleter = {
            let st = st.clone();
            std::thread::spawn(move || st.delete(target).unwrap())
        };
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let (found, found_rx) = mpsc::channel();
        {
            let st = st.clone();
            std::thread::spawn(move || found.send(st.get("bystander").is_some()));
        }
        let lookup = found_rx.recv_timeout(Duration::from_secs(5));
        // the dataset being deleted is not found while the delete runs
        let gone = st.get(target).is_none();
        release.send(()).unwrap();
        assert!(deleter.join().unwrap());
        assert_eq!(lookup, Ok(true), "a lookup waited for the delete");
        assert!(gone);
        assert!(st.get(target).is_none());
        assert!(!dir.path().join("databases").join(target).exists());
    }

    #[test]
    fn direct_naming_flags_win_over_the_fuseki_config() {
        use DirectMode::{Read, ReadWrite};
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let n = GspDirectNaming::layered(&v(&["ds=read-write"]), &v(&["ds=read"])).unwrap();
        assert_eq!(n.mode("ds"), Some(Read));
        let n = GspDirectNaming::layered(&v(&["ds=read"]), &v(&["ds=read-write"])).unwrap();
        assert_eq!(n.mode("ds"), Some(ReadWrite));
        // a bare flag covers the datasets no entry names
        let n = GspDirectNaming::layered(&v(&["ds=read"]), &v(&[""])).unwrap();
        assert_eq!(n.mode("ds"), Some(Read));
        assert_eq!(n.mode("other"), Some(ReadWrite));
        let n = GspDirectNaming::layered(&v(&["a=read"]), &v(&["b"])).unwrap();
        assert_eq!(
            (n.mode("a"), n.mode("b"), n.mode("c")),
            (Some(Read), Some(ReadWrite), None)
        );
    }
}

/// `--schema-statistics-above`: a number of quads, or `off`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatisticsAbove(pub Option<u64>);

impl Default for StatisticsAbove {
    fn default() -> Self {
        StatisticsAbove(Some(sparkles::schema::DEFAULT_STATISTICS_ABOVE))
    }
}

impl std::str::FromStr for StatisticsAbove {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim() {
            "off" => Ok(StatisticsAbove(None)),
            n => n
                .replace('_', "")
                .parse()
                .map(|n| StatisticsAbove(Some(n)))
                .map_err(|_| format!("expected a number of quads or off, not {s:?}")),
        }
    }
}

impl std::fmt::Display for StatisticsAbove {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(n) => write!(f, "{n}"),
            None => f.write_str("off"),
        }
    }
}
