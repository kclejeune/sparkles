//! Server state: dataset registry (persisted like Fuseki's `configuration/`), async tasks.

use anyhow::{Context, Result, bail};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sparkles::store::{Store, StoreOptions};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbType {
    Persistent,
    Mem,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReasoningInfo {
    pub profile: String,
    pub inferred: u64,
    pub at: String,
}

pub struct Dataset {
    pub name: String,
    pub kind: DbType,
    pub store: Store,
    pub reasoning: RwLock<Option<ReasoningInfo>>,
    /// not part of the persisted registry (e.g. `--loc` on the command line)
    pub ephemeral: bool,
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
    pub state: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
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
    pub allow_service: bool,
    /// Per-request budgets.
    pub limits: Limits,
    /// Emit one `sparkles::access` event per request.
    pub access_log: bool,
    pub metrics: crate::obs::Metrics,
    phase: AtomicU8,
    /// Serializes dataset management (create / attach / delete / registry saves) so a
    /// name is reserved atomically and an older registry snapshot can never overwrite
    /// a newer one.
    manage: Mutex<()>,
}

/// Reasoning status is kept inside the database directory (`reasoning.json`) so it
/// survives restarts however the database is attached (registry, `--loc`, CLI `infer`).
pub fn read_reasoning_file(root: &Path) -> Option<ReasoningInfo> {
    serde_json::from_slice(&std::fs::read(root.join("reasoning.json")).ok()?).ok()
}

pub fn write_reasoning_file(root: &Path, info: Option<&ReasoningInfo>) -> Result<()> {
    let path = root.join("reasoning.json");
    match info {
        Some(i) => std::fs::write(&path, serde_json::to_vec_pretty(i)?)?,
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
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
    /// serialized body of query and Graph Store GET responses
    pub max_result_bytes: Option<u64>,
    /// rows of any intermediate result
    pub max_rows: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            query_memory_bytes: Some(8 << 30),
            max_result_bytes: Some(1 << 30),
            max_rows: 200_000_000,
        }
    }
}

impl Limits {
    /// `{timeoutSeconds, queryMemoryBytes, maxResultBytes, maxRows}`; 0 means unlimited.
    pub fn json(&self, timeout: std::time::Duration) -> serde_json::Value {
        serde_json::json!({
            "timeoutSeconds": timeout.as_secs_f64(),
            "queryMemoryBytes": self.query_memory_bytes.unwrap_or(0),
            "maxResultBytes": self.max_result_bytes.unwrap_or(0),
            "maxRows": self.max_rows,
        })
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
            limits: Limits::default(),
            access_log: true,
            metrics: crate::obs::Metrics::new(true, 100),
            phase: AtomicU8::new(crate::obs::Phase::Starting as u8),
            manage: Mutex::new(()),
        };
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
        Ok(Arc::new(Dataset {
            name: name.to_string(),
            kind,
            store,
            reasoning: RwLock::new(reasoning),
            ephemeral: loc.is_some(),
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

    // ----------------------------------------------------------------- tasks ------

    pub fn start_task(
        self: &Arc<Self>,
        kind: &str,
        dataset: &str,
        work: impl FnOnce(&TaskHandle) -> Result<String> + Send + 'static,
    ) -> Task {
        let id = self
            .task_counter
            .fetch_add(1, Ordering::Relaxed)
            .to_string();
        let task = Task {
            id: id.clone(),
            kind: kind.to_string(),
            dataset: dataset.to_string(),
            state: "running".into(),
            started_at: now(),
            finished_at: None,
            message: None,
            progress: Some(0.0),
        };
        {
            let mut tasks = self.tasks.lock();
            tasks.push(task.clone());
            let n = tasks.len();
            if n > 200 {
                tasks.drain(..n - 200);
            }
        }
        let state = self.clone();
        std::thread::spawn(move || {
            let handle = TaskHandle {
                state: state.clone(),
                id: id.clone(),
            };
            let r = work(&handle);
            let mut tasks = state.tasks.lock();
            if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
                t.finished_at = Some(now());
                t.progress = Some(1.0);
                match r {
                    Ok(msg) => {
                        t.state = "done".into();
                        t.message = Some(msg);
                    }
                    Err(e) => {
                        t.state = "failed".into();
                        t.message = Some(format!("{e:#}"));
                    }
                }
            }
        });
        task
    }
}

#[derive(Clone)]
pub struct TaskHandle {
    state: Arc<AppState>,
    id: String,
}

impl TaskHandle {
    pub fn progress(&self, p: f32, msg: &str) {
        if let Some(t) = self.state.tasks.lock().iter_mut().find(|t| t.id == self.id) {
            t.progress = Some(p.clamp(0.0, 1.0));
            t.message = Some(msg.to_string());
        }
    }
}

pub fn uptime_secs(state: &AppState) -> u64 {
    state.started.elapsed().as_secs()
}
