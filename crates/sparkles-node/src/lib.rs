//! Node-API bridge. Shared globals contain Rust-only values, never JavaScript handles.
mod admin;
#[cfg(feature = "backup")]
mod backups;
mod catalog;
mod environment;
mod rdf;
mod streams;
mod terms;
mod utilities;
pub use utilities::utility;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use parking_lot::Mutex;
use serde_json::{Value, json};
use sparkles::embed::{GraphMatch, QuadPattern, TxnWorker};
use sparkles::guard::WriteOptions;
use sparkles::sparql::{QueryKind, QueryOptions, QueryResult};
use sparkles::{Dataset, Error as EngineError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

fn err(e: EngineError) -> napi::Error {
    let kind = match &e {
        EngineError::SparqlSyntax(_) => "SparqlSyntaxError",
        EngineError::RdfParse(_) => "RdfSyntaxError",
        EngineError::Invalid(_) => "InvalidInputError",
        EngineError::Unsupported(_) => "UnsupportedError",
        EngineError::Timeout => "QueryTimeoutError",
        EngineError::Cancelled => "CancelledError",
        EngineError::Conflict(_) | EngineError::WriterBusy | EngineError::PreconditionFailed(_) => {
            "ConflictError"
        }
        EngineError::NotFound(_) | EngineError::HistoryGone(_) => "NotFoundError",
        EngineError::NotPermitted(_) => "PermissionDeniedError",
        EngineError::BudgetExceeded(_) => "BudgetExceededError",
        EngineError::Locked { .. } => "DatasetLockedError",
        EngineError::Rejected(_) | EngineError::GuardMissing(_) => "WriteRejectedError",
        EngineError::Service(_) => "ServiceError",
        _ => "StorageError",
    };
    let details = match &e {
        EngineError::Rejected(r) => json!({ "validation": r.summary, "head": r.head.to_string() }),
        EngineError::BudgetExceeded(b) => json!({
            "budget": format!("{:?}", b.kind),
            "limit": b.limit.to_string(),
            "requested": b.requested.to_string(),
        }),
        EngineError::Locked { path, pid } => json!({ "path": path, "pid": pid }),
        EngineError::Io(io) => json!({
            "errno": io.raw_os_error(),
            "code": if io.kind() == std::io::ErrorKind::NotFound {"ENOENT"} else {"EIO"},
        }),
        _ => json!({}),
    };
    coded(kind, &e.to_string(), details)
}
/// A JavaScript error of `kind`, which `nativeError` turns into its class.
fn coded(kind: &str, message: &str, details: Value) -> napi::Error {
    napi::Error::new(
        Status::GenericFailure,
        json!({ "kind": kind, "message": message, "details": details }).to_string(),
    )
}
fn invalid(m: impl Into<String>) -> napi::Error {
    err(EngineError::invalid(m))
}
/// A failure inside the addon, such as a panic in a native task, rather than in the input.
fn internal(m: impl std::fmt::Display) -> napi::Error {
    coded("InternalError", &m.to_string(), json!({}))
}
fn parse(s: &str) -> napi::Result<Value> {
    serde_json::from_str(s).map_err(|e| invalid(e.to_string()))
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> sparkles::Result<T> + Send + 'static,
) -> napi::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| internal(format!("a native task failed: {e}")))?
        .map_err(err)
}
// Facade backup methods intentionally reject calls entered in a Tokio runtime.
// A dedicated Rust thread keeps those blocking bridges outside the addon runtime.
#[cfg(feature = "backup")]
async fn off_runtime<T: Send + 'static>(
    f: impl FnOnce() -> sparkles::Result<T> + Send + 'static,
) -> napi::Result<T> {
    let (send, recv) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("sparkles-node-backup".into())
        .spawn(move || {
            let _ = send.send(f());
        })
        .map_err(|e| err(e.into()))?;
    recv.await
        .map_err(|_| internal("the backup worker ended without a result"))?
        .map_err(err)
}
fn lossless(mut v: Value) -> Value {
    match &mut v {
        Value::Number(n) => {
            if let Some(n) = n.as_u64() {
                return Value::String(n.to_string());
            }
        }
        Value::Array(a) => {
            for item in a {
                *item = lossless(item.take());
            }
        }
        Value::Object(o) => {
            for item in o.values_mut() {
                *item = lossless(item.take());
            }
        }
        _ => {}
    }
    v
}
fn receipt(r: &sparkles::commit::Receipt) -> Value {
    lossless(serde_json::to_value(r).expect("receipt serializes"))
}

#[napi]
pub struct Cancellation {
    flag: Arc<AtomicBool>,
    resources: Arc<environment::Resources>,
}
#[napi]
impl Cancellation {
    #[napi(constructor)]
    pub fn new(env: napi::Env) -> napi::Result<Self> {
        let flag = Arc::new(AtomicBool::new(false));
        let resources = environment::resources(env)?;
        resources.flag(&flag);
        Ok(Self { flag, resources })
    }
    #[napi]
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }
}

fn write_options(v: &Value, flag: Arc<AtomicBool>) -> sparkles::Result<WriteOptions> {
    let mut o = WriteOptions {
        cancel: Some(flag),
        no_wait: v["noWait"].as_bool().unwrap_or(false),
        message: v["message"].as_str().map(Arc::from),
        ..Default::default()
    };
    if v["dryRun"] == true || v["dryRun"].is_object() {
        o.dry_run = Some(sparkles::preview::DryRun {
            changes: v["dryRun"]["changes"].as_u64().unwrap_or(0).min(10000) as usize,
            max_changes: v["dryRun"]["maxChanges"].as_u64().unwrap_or(10000),
            ..Default::default()
        });
    }
    if let Some(ms) = v["timeout"].as_u64() {
        o.deadline = Instant::now().checked_add(Duration::from_millis(ms));
    }
    if let Some(head) = v["ifHead"].as_str() {
        let head: u64 = head
            .parse()
            .map_err(|_| EngineError::invalid("invalid ifHead"))?;
        o.precondition = Some(sparkles::guard::Precondition::new(move |s| {
            if s.commit == head {
                Ok(())
            } else {
                Err(EngineError::PreconditionFailed(
                    "the dataset head changed".into(),
                ))
            }
        }));
    }
    Ok(o)
}
fn query_options(v: &Value, flag: Arc<AtomicBool>) -> sparkles::Result<QueryOptions> {
    let mut o = QueryOptions {
        cancel: Some(flag.clone()),
        write: write_options(v, flag)?,
        timeout: v["timeout"].as_u64().map(Duration::from_millis),
        union_default_graph: v["unionDefaultGraph"].as_bool(),
        no_cache: v["noCache"].as_bool().unwrap_or(false),
        base_iri: v["baseIri"].as_str().map(str::to_string),
        max_rows: v["maxRows"].as_u64().map(|n| n as usize),
        max_memory_bytes: v["maxMemoryBytes"].as_u64(),
        max_rows_produced: v["maxRowsProduced"].as_u64(),
        ..Default::default()
    };
    if !v["describe"].is_null() {
        o.describe = serde_json::from_value(v["describe"].clone())
            .map_err(|e| EngineError::invalid(e.to_string()))?;
    }
    if let Some(p) = v["prefixes"].as_object() {
        for (k, val) in p {
            o.prefixes.push((
                k.clone(),
                val.as_str()
                    .ok_or_else(|| EngineError::invalid("prefix must be a string"))?
                    .into(),
            ));
        }
    }
    if let Some(b) = v["bindings"].as_object() {
        for (k, val) in b {
            o.initial_bindings.push((k.clone(), terms::term(val)?));
        }
    }
    for (key, target) in [
        ("defaultGraph", &mut o.default_graph_uris),
        ("namedGraphs", &mut o.named_graph_uris),
    ] {
        if let Some(values) = v[key].as_array() {
            for value in values {
                target.push(
                    value
                        .as_str()
                        .ok_or_else(|| EngineError::invalid("graph must be a string"))?
                        .into(),
                );
            }
        }
    }
    if v["includeInferred"].as_bool() == Some(true) {
        o.default_graph_extra.push("urn:x-sparkles:inferred".into());
    }
    Ok(o)
}
fn open_query_cursor(
    ds: &Dataset,
    text: &str,
    v: &Value,
    flag: Arc<AtomicBool>,
) -> sparkles::Result<sparkles::sparql::QueryExecution> {
    let opts = query_options(v, flag)?;
    let mode = if v["execution"].as_str() == Some("auto") {
        sparkles::sparql::ExecutionMode::Auto
    } else {
        sparkles::sparql::ExecutionMode::Streaming
    };
    let cursor_opts = sparkles::sparql::CursorOptions {
        batch_rows: v["batchSize"]
            .as_u64()
            .unwrap_or(4096)
            .try_into()
            .map_err(|_| EngineError::invalid("batchSize is too large"))?,
        batch_bytes: v["batchBytes"]
            .as_u64()
            .unwrap_or(1 << 20)
            .try_into()
            .map_err(|_| EngineError::invalid("batchBytes is too large"))?,
        fallback: if v["allowMaterialization"].as_bool() == Some(false) {
            sparkles::sparql::FallbackPolicy::RejectMaterialization
        } else {
            sparkles::sparql::FallbackPolicy::AllowMaterialization
        },
    };
    if let Some(at) = v["at"].as_str() {
        let history = sparkles::history::HistoryOptions {
            cancel: opts.cancel.clone(),
            deadline: opts.timeout.and_then(|t| Instant::now().checked_add(t)),
        };
        let snapshot = ds.store().snapshot_at(&at.parse()?, &history)?.0;
        sparkles::sparql::query_execution(
            snapshot,
            text,
            &ds.with_query_defaults(&opts),
            &cursor_opts,
            mode,
        )
    } else {
        ds.query_execution_with(text, &opts, &cursor_opts, mode)
    }
}
fn normalize_store_options(mut v: Value) -> Value {
    let defaults = sparkles::store::StoreOptions::default();
    if v["unionDefaultGraph"].is_null() {
        v["unionDefaultGraph"] = json!(defaults.union_default_graph)
    }
    if v["cacheBytes"].is_null() {
        v["cacheBytes"] = json!(defaults.cache_bytes)
    }
    v
}
struct Shared {
    ds: Dataset,
    writers: Arc<Semaphore>,
    options: Value,
}
static STORES: LazyLock<Mutex<HashMap<PathBuf, Weak<Shared>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// The permits of running queries, one per query executing on a blocking thread. The
/// default is the number of cores, and `setMaxConcurrentQueries` replaces it. A streaming
/// cursor takes a permit only while it opens and while it computes a batch, so an idle
/// cursor never blocks other queries.
static READERS: LazyLock<Mutex<Arc<Semaphore>>> = LazyLock::new(|| {
    Mutex::new(Arc::new(Semaphore::new(
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(4),
    )))
});
fn readers() -> Arc<Semaphore> {
    READERS.lock().clone()
}
/// Set the number of queries that may run at once. Queries already running keep the
/// permits of the old limit until they finish, so for a moment both limits apply.
#[napi]
pub fn set_max_concurrent_queries(limit: u32) -> napi::Result<()> {
    if limit == 0 || limit as usize > Semaphore::MAX_PERMITS {
        return Err(invalid("maxConcurrentQueries must be a positive integer"));
    }
    *READERS.lock() = Arc::new(Semaphore::new(limit as usize));
    Ok(())
}
async fn permit(
    s: Arc<Semaphore>,
    v: &Value,
    flag: &AtomicBool,
) -> napi::Result<OwnedSemaphorePermit> {
    let started = Instant::now();
    let acquire = s.clone().acquire_owned();
    tokio::pin!(acquire);
    loop {
        if flag.load(Ordering::Relaxed) {
            return Err(err(EngineError::Cancelled));
        }
        if v["noWait"].as_bool() == Some(true) {
            return s
                .try_acquire_owned()
                .map_err(|_| err(EngineError::WriterBusy));
        }
        if v["timeout"]
            .as_u64()
            .is_some_and(|ms| started.elapsed() >= Duration::from_millis(ms))
        {
            return Err(err(EngineError::Timeout));
        }
        if let Ok(p) = tokio::time::timeout(Duration::from_millis(10), &mut acquire).await {
            return p.map_err(|e| invalid(e.to_string()));
        }
    }
}
#[napi]
pub struct NativeDataset {
    shared: Mutex<Option<Arc<Shared>>>,
    read_only: bool,
}
impl NativeDataset {
    fn get(&self, write: bool) -> napi::Result<Arc<Shared>> {
        if write && self.read_only {
            return Err(err(EngineError::NotPermitted(
                "dataset is read-only".into(),
            )));
        }
        self.shared
            .lock()
            .clone()
            .ok_or_else(|| invalid("dataset is closed"))
    }
}
#[napi]
impl NativeDataset {
    #[napi(factory)]
    pub fn memory(options: String) -> napi::Result<Self> {
        let v = parse(&options)?;
        let opts = sparkles::store::StoreOptions {
            union_default_graph: v["unionDefaultGraph"].as_bool().unwrap_or(false),
            ..Default::default()
        };
        let ds = Dataset::from_store(sparkles::store::Store::in_memory(opts));
        Ok(Self {
            read_only: v["readOnly"].as_bool().unwrap_or(false),
            shared: Mutex::new(Some(Arc::new(Shared {
                ds,
                writers: Arc::new(Semaphore::new(1)),
                options: v,
            }))),
        })
    }
    #[napi(factory)]
    pub async fn open(path: String, options: String) -> napi::Result<Self> {
        let v = normalize_store_options(parse(&options)?);
        let read_only = v["readOnly"].as_bool().unwrap_or(false);
        let shared = blocking(move || {
            let path = Path::new(&path);
            std::fs::create_dir_all(path)?;
            let key = std::fs::canonicalize(path)?;
            // Opening is single-flight. This Rust-only lock never invokes JavaScript.
            let mut stores = STORES.lock();
            stores.retain(|_, value| value.strong_count() > 0);
            if let Some(shared) = stores.get(&key).and_then(Weak::upgrade) {
                if shared.options["unionDefaultGraph"] != v["unionDefaultGraph"]
                    || shared.options["cacheBytes"] != v["cacheBytes"]
                {
                    return Err(EngineError::invalid(
                        "dataset open options differ from the existing handle",
                    ));
                }
                return Ok(shared);
            }
            if let Some(shared) = catalog::shared_for_path(&key, &v)? {
                stores.insert(key, Arc::downgrade(&shared));
                return Ok(shared);
            }
            let defaults = sparkles::store::StoreOptions::default();
            let o = sparkles::store::StoreOptions {
                union_default_graph: v["unionDefaultGraph"].as_bool().unwrap_or(false),
                cache_bytes: v["cacheBytes"].as_u64().unwrap_or(defaults.cache_bytes),
                ..defaults
            };
            let shared = Arc::new(Shared {
                ds: Dataset::open_with(&key, o)?,
                writers: Arc::new(Semaphore::new(1)),
                options: v,
            });
            stores.insert(key, Arc::downgrade(&shared));
            Ok(shared)
        })
        .await?;
        Ok(Self {
            shared: Mutex::new(Some(shared)),
            read_only,
        })
    }
    #[napi]
    pub fn identity(&self) -> napi::Result<String> {
        Ok(self.get(false)?.ds.dataset_id().to_string())
    }
    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        let shared = self.shared.lock().take();
        blocking(move || {
            drop(shared);
            Ok(())
        })
        .await
    }
    #[napi]
    pub async fn query(
        &self,
        text: String,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<NativeResult> {
        let shared = self.get(false)?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        match v["execution"].as_str().unwrap_or("eager") {
            "eager" => {}
            "streaming" | "auto" => {
                let readers = readers();
                let permit = permit(readers.clone(), &v, &flag).await?;
                let cancellation = flag.clone();
                let cursor =
                    blocking(move || open_query_cursor(&shared.ds, &text, &v, flag)).await?;
                drop(permit);
                return NativeResult::streaming(cursor, readers, cancellation).map_err(err);
            }
            _ => return Err(invalid("execution must be eager, streaming or auto")),
        }
        let _permit = permit(readers(), &v, &flag).await?;
        let result = blocking(move || {
            let opts = query_options(&v, flag)?;
            if let Some(at) = v["at"].as_str() {
                let (snapshot, _) = shared.ds.store().snapshot_at(
                    &at.parse()?,
                    &sparkles::history::HistoryOptions {
                        cancel: opts.cancel.clone(),
                        deadline: opts.timeout.and_then(|t| Instant::now().checked_add(t)),
                    },
                )?;
                sparkles::sparql::query(snapshot, &text, &shared.ds.with_query_defaults(&opts))
            } else {
                shared.ds.query_with(&text, &opts)
            }
        })
        .await?;
        Ok(NativeResult::query(result))
    }
    #[napi]
    pub fn begin<'env>(
        &self,
        env: &'env napi::Env,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<PromiseRaw<'env, NativeTransaction>> {
        let shared = self.get(true)?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        let resources = cancel.resources.clone();
        env.spawn_future(async move {
            let permit = permit(shared.writers.clone(), &v, &flag).await?;
            let worker =
                blocking(move || TxnWorker::begin_with(&shared.ds, None, write_options(&v, flag)?))
                    .await?
                    .ok_or_else(|| invalid("transaction precondition failed"))?;
            let inner = Arc::new(Mutex::new(Some(worker)));
            let weak = Arc::downgrade(&inner);
            let cleanup_marker = Arc::new(());
            resources.cleanup(&cleanup_marker, move || {
                if let Some(inner) = weak.upgrade() {
                    if let Some(mut guard) = inner.try_lock() {
                        guard.take();
                    } else {
                        let _ = std::thread::Builder::new()
                            .name("sparkles-node-cleanup".into())
                            .spawn(move || {
                                inner.lock().take();
                            });
                    }
                }
            });
            Ok(NativeTransaction {
                inner,
                permit: Arc::new(Mutex::new(Some(permit))),
                poisoned: Arc::new(AtomicBool::new(false)),
                _cleanup_marker: cleanup_marker,
            })
        })
    }
    #[napi]
    pub async fn update(
        &self,
        text: String,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<String> {
        let shared = self.get(true)?;
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        let _permit = permit(shared.writers.clone(), &v, &flag).await?;
        blocking(move || {
            let opts = query_options(&v, flag)?;
            let result = shared.ds.update_with(&text, &opts);
            if opts.write.dry_run.is_some() {
                let p = sparkles::preview::catch(result)?;
                let inserted = p.commit.as_ref().map_or(0, |c| c.inserted);
                let deleted = p.commit.as_ref().map_or(0, |c| c.deleted);
                let head = serde_json::to_value(p.head)
                    .map_err(|e| EngineError::invalid(e.to_string()))?;
                let commit = p
                    .commit
                    .as_ref()
                    .map(|c| lossless(serde_json::to_value(c).expect("commit serializes")));
                let changes: Vec<Value> = p
                    .changes
                    .iter()
                    .map(|(op, q)| {
                        json!({
                            "op": if *op == sparkles::store::DiffOp::Add {"add"} else {"remove"},
                            "quad": terms::encode_quad(q),
                        })
                    })
                    .collect();
                return Ok(json!({
                    "inserted": inserted.to_string(),
                    "deleted": deleted.to_string(),
                    "preview": {
                        "datasetId": p.dataset_id,
                        "head": lossless(head),
                        "commit": commit,
                        "outcome": p.outcome().name(),
                        "validation": p.validation.as_deref(),
                        "changes": changes,
                    },
                })
                .to_string());
            }
            let r = result?;
            Ok(json!({
                "inserted": r.inserted.to_string(),
                "deleted": r.deleted.to_string(),
                "receipt": r.commit.as_ref().map(receipt),
            })
            .to_string())
        })
        .await
    }
    #[napi]
    pub fn path(&self) -> napi::Result<Option<String>> {
        Ok(self
            .get(false)?
            .ds
            .store()
            .root()
            .map(|p| p.to_string_lossy().into_owned()))
    }
    #[napi]
    pub async fn count(&self) -> napi::Result<String> {
        let shared = self.get(false)?;
        blocking(move || Ok(shared.ds.len().to_string())).await
    }
    #[napi]
    pub fn wait_commit<'env>(
        &self,
        env: &'env napi::Env,
        after: String,
        cancel: &Cancellation,
    ) -> napi::Result<PromiseRaw<'env, String>> {
        let shared = self.get(false)?;
        let flag = cancel.flag.clone();
        let after: u64 = after
            .parse()
            .map_err(|_| invalid("invalid commit cursor"))?;
        env.spawn_future(async move {
            let mut rx = shared.ds.store().subscribe_commits();
            loop {
                if flag.load(Ordering::Relaxed) {
                    return Err(err(EngineError::Cancelled));
                }
                let head = *rx.borrow_and_update();
                if head > after {
                    return Ok(head.to_string());
                }
                if let Ok(result) =
                    tokio::time::timeout(Duration::from_millis(10), rx.changed()).await
                {
                    result.map_err(|_| invalid("dataset notifications ended"))?;
                }
            }
        })
    }
    #[napi]
    pub async fn count_pattern(&self, pattern: String) -> napi::Result<String> {
        let shared = self.get(false)?;
        let pattern = parse(&pattern)?;
        blocking(move || {
            Ok(
                sparkles::embed::count_in(&shared.ds.snapshot(), &quad_pattern(&pattern)?)?
                    .to_string(),
            )
        })
        .await
    }
    #[napi]
    pub async fn matched(&self, pattern: String) -> napi::Result<NativeResult> {
        let shared = self.get(false)?;
        let pattern = parse(&pattern)?;
        blocking(move || {
            Ok(NativeResult::scan(sparkles::embed::quads_in(
                shared.ds.snapshot(),
                &quad_pattern(&pattern)?,
            )))
        })
        .await
    }
}

fn quad_pattern(v: &Value) -> sparkles::Result<QuadPattern> {
    let node = |v: &Value| {
        if v.is_null() {
            Ok(None)
        } else {
            terms::node(v).map(Some)
        }
    };
    let graph = if v[3].is_null() {
        GraphMatch::Any
    } else if v[3]["termType"] == "DefaultGraph" {
        GraphMatch::Default
    } else {
        GraphMatch::Named(terms::node(&v[3])?)
    };
    let predicate = if v[1].is_null() {
        None
    } else {
        match terms::term(&v[1])? {
            oxrdf::Term::NamedNode(n) => Some(n),
            _ => return Err(EngineError::invalid("predicate must be an IRI")),
        }
    };
    Ok(QuadPattern {
        graph,
        subject: node(&v[0])?,
        predicate,
        object: if v[2].is_null() {
            None
        } else {
            Some(terms::term(&v[2])?)
        },
    })
}

fn query_result_row(r: &QueryResult, pos: usize) -> Option<Vec<Value>> {
    if r.kind == QueryKind::Select {
        if pos < r.table.len() {
            Some(
                r.table
                    .row(pos)
                    .iter()
                    .map(|id| {
                        r.term(*id)
                            .as_ref()
                            .map(terms::encode)
                            .unwrap_or(Value::Null)
                    })
                    .collect(),
            )
        } else {
            None
        }
    } else if pos < r.triples.len() {
        let t = &r.triples[pos];
        Some(vec![terms::encode_quad(&oxrdf::Quad::new(
            t.subject.clone(),
            t.predicate.clone(),
            t.object.clone(),
            oxrdf::GraphName::DefaultGraph,
        ))])
    } else {
        r.quads
            .get(pos - r.triples.len())
            .map(|q| vec![terms::encode_quad(q)])
    }
}

struct SelectCursor {
    cursor: sparkles::sparql::QueryCursor,
    batch: Option<sparkles::sparql::QueryBatch>,
    row: usize,
}
impl SelectCursor {
    fn stats_json(&self, sent: usize, closing: bool) -> sparkles::Result<String> {
        let mut stats = self.cursor.stats();
        if self
            .batch
            .as_ref()
            .is_some_and(|batch| self.row < batch.len())
            && stats.status == sparkles::sparql::CursorStatus::Complete
        {
            stats.status = if closing {
                sparkles::sparql::CursorStatus::Stopped
            } else {
                sparkles::sparql::CursorStatus::Open
            };
        }
        Ok(format!(
            "{{\"stats\":{},\"plan\":{},\"sentRows\":{sent}}}",
            serde_json::to_string(&stats).unwrap(),
            self.cursor.plan_json()?
        ))
    }
}
struct GraphStream {
    cursor: sparkles::sparql::GraphCursor,
    batch: Option<sparkles::sparql::GraphBatch>,
    row: usize,
}
impl GraphStream {
    fn stats_json(&self, sent: usize, closing: bool) -> sparkles::Result<String> {
        let mut stats = self.cursor.stats();
        if self.batch.as_ref().is_some_and(|b| self.row < b.len())
            && stats.status == sparkles::sparql::CursorStatus::Complete
        {
            stats.status = if closing {
                sparkles::sparql::CursorStatus::Stopped
            } else {
                sparkles::sparql::CursorStatus::Open
            };
        }
        Ok(format!(
            "{{\"stats\":{},\"plan\":{},\"sentRows\":{sent}}}",
            serde_json::to_string(&stats).unwrap(),
            self.cursor.plan_json()?
        ))
    }
}
enum Rows {
    Streaming(Box<SelectCursor>),
    Graph(Box<GraphStream>),
    Ask(Box<sparkles::sparql::AskResult>),
    Query(Box<QueryResult>),
    Collected(Box<sparkles::sparql::MaterializedResult>),
    Scan(Box<sparkles::QuadIter>),
}
struct Cursor {
    rows: Rows,
    pos: usize,
}
/// A query result that JavaScript pulls in batches. A streaming cursor keeps its snapshot
/// until it is drained, closed or collected, but it holds a reader permit only while a
/// batch is being computed.
#[napi]
pub struct NativeResult {
    cursor: Arc<Mutex<Option<Cursor>>>,
    metadata: String,
    statistics: Arc<Mutex<Option<String>>>,
    readers: Option<Arc<Semaphore>>,
    cancel: Option<Arc<AtomicBool>>,
}
impl NativeResult {
    fn query(result: QueryResult) -> Self {
        let kind = match result.kind {
            QueryKind::Select => "bindings",
            QueryKind::Ask => "boolean",
            _ => "quads",
        };
        let metadata = json!({
            "type": kind,
            "variables": result.vars,
            "size": result.len(),
            "value": result.boolean,
            "timing": result.timing,
            "plan": result.plan,
            "memoryPeakBytes": result.mem_peak_bytes.to_string(),
            "rowsProduced": result.rows_produced.to_string(),
        })
        .to_string();
        Self {
            cursor: Arc::new(Mutex::new(Some(Cursor {
                rows: Rows::Query(Box::new(result)),
                pos: 0,
            }))),
            metadata,
            statistics: Default::default(),
            readers: None,
            cancel: None,
        }
    }
    fn streaming(
        execution: sparkles::sparql::QueryExecution,
        readers: Arc<Semaphore>,
        cancel: Arc<AtomicBool>,
    ) -> sparkles::Result<Self> {
        use sparkles::sparql::QueryExecution;
        let (kind, variables, value) = match &execution {
            QueryExecution::Eager(r) => {
                let r = r.result();
                (
                    if r.kind == QueryKind::Select {
                        "bindings"
                    } else if r.kind == QueryKind::Ask {
                        "boolean"
                    } else {
                        "quads"
                    },
                    r.vars.clone(),
                    (r.kind == QueryKind::Ask).then_some(r.boolean),
                )
            }
            QueryExecution::Select(c) => ("bindings", c.variables().to_vec(), None),
            QueryExecution::Graph(_) => ("quads", Vec::new(), None),
            QueryExecution::Ask(r) => ("boolean", Vec::new(), Some(r.value())),
        };
        let chosen = if matches!(execution, QueryExecution::Eager(_)) {
            "eager"
        } else {
            "streaming"
        };
        let size = match &execution {
            QueryExecution::Eager(r) => format!(",\"size\":{}", r.result().len()),
            _ => String::new(),
        };
        let metadata = format!(
            "{{\"type\":{kind:?},\"variables\":{},\"value\":{},\"execution\":{chosen:?}{size},\"timing\":{},\"plan\":{}}}",
            serde_json::to_string(&variables).unwrap(),
            serde_json::to_string(&value).unwrap(),
            serde_json::to_string(&execution.stats().timing).unwrap(),
            execution.plan_json()?
        );
        let rows = match execution {
            QueryExecution::Eager(r) => Rows::Collected(r),
            QueryExecution::Select(cursor) => Rows::Streaming(Box::new(SelectCursor {
                cursor: *cursor,
                batch: None,
                row: 0,
            })),
            QueryExecution::Graph(cursor) => Rows::Graph(Box::new(GraphStream {
                cursor: *cursor,
                batch: None,
                row: 0,
            })),
            QueryExecution::Ask(result) => Rows::Ask(result),
        };
        Ok(Self {
            cursor: Arc::new(Mutex::new(Some(Cursor { rows, pos: 0 }))),
            metadata,
            statistics: Default::default(),
            readers: Some(readers),
            cancel: Some(cancel),
        })
    }
    fn scan(scan: sparkles::QuadIter) -> Self {
        Self {
            cursor: Arc::new(Mutex::new(Some(Cursor {
                rows: Rows::Scan(Box::new(scan)),
                pos: 0,
            }))),
            metadata: json!({ "type": "quads" }).to_string(),
            statistics: Default::default(),
            readers: None,
            cancel: None,
        }
    }
}
impl Drop for NativeResult {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}
#[napi]
impl NativeResult {
    #[napi]
    pub fn info(&self) -> String {
        self.metadata.clone()
    }
    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        let cursor = self.cursor.clone();
        let statistics = self.statistics.clone();
        blocking(move || {
            if let Some(mut cursor) = cursor.lock().take() {
                match &mut cursor.rows {
                    Rows::Streaming(state) => {
                        state.cursor.close();
                        *statistics.lock() = Some(state.stats_json(cursor.pos, true)?);
                    }
                    Rows::Graph(state) => {
                        state.cursor.close();
                        *statistics.lock() = Some(state.stats_json(cursor.pos, true)?);
                    }
                    _ => {}
                }
            }
            Ok(())
        })
        .await
    }
    #[napi]
    pub async fn stats(&self) -> napi::Result<String> {
        let cursor = self.cursor.clone();
        let statistics = self.statistics.clone();
        let metadata = self.metadata.clone();
        blocking(move || {
            if let Some(cursor) = cursor.lock().as_ref() {
                match &cursor.rows {
                    Rows::Streaming(state) => return state.stats_json(cursor.pos, false),
                    Rows::Graph(state) => return state.stats_json(cursor.pos, false),
                    _ => {}
                }
            }
            Ok(statistics.lock().as_ref().cloned().unwrap_or(metadata))
        })
        .await
    }
    #[napi]
    pub async fn next_batch(&self, max_rows: u32, max_bytes: u32) -> napi::Result<String> {
        let cursor = self.cursor.clone();
        let statistics = self.statistics.clone();
        let cancel = self.cancel.clone();
        // A streaming cursor computes its batch under a reader permit, which it gives back
        // when the batch is done. Waiting for the permit stops when the cursor is closed.
        let _permit = match (&self.readers, &cancel) {
            (Some(readers), Some(flag)) => Some(permit(readers.clone(), &Value::Null, flag).await?),
            _ => None,
        };
        blocking(move || {
            let mut lock = cursor.lock();
            let Some(c) = lock.as_mut() else {
                return Ok("null".into());
            };
            let mut terms = vec![Value::Null];
            let mut dictionary = HashMap::<String, u32>::new();
            let mut rows = Vec::new();
            let mut bytes = 0;
            while rows.len() < (max_rows.clamp(1, 65536) as usize)
                && bytes < (max_bytes.clamp(1024, 16 << 20) as usize)
            {
                if cancel
                    .as_ref()
                    .is_some_and(|flag| flag.load(Ordering::Relaxed))
                {
                    return Err(EngineError::Cancelled);
                }
                let row: Option<Vec<Value>> = match &mut c.rows {
                    Rows::Streaming(state) => {
                        if state
                            .batch
                            .as_ref()
                            .is_none_or(|batch| state.row == batch.len())
                        {
                            state.batch = None;
                            state.row = 0;
                            state.batch = state.cursor.next_batch()?;
                        }
                        match &state.batch {
                            None => None,
                            Some(batch) => {
                                let row = batch
                                    .row(state.row)?
                                    .into_iter()
                                    .map(|term| {
                                        term.as_ref().map(terms::encode).unwrap_or(Value::Null)
                                    })
                                    .collect();
                                state.row += 1;
                                Some(row)
                            }
                        }
                    }
                    Rows::Graph(state) => {
                        if state.batch.as_ref().is_none_or(|b| state.row == b.len()) {
                            state.batch = None;
                            state.row = 0;
                            state.batch = state.cursor.next_batch()?;
                        }
                        state.batch.as_ref().map(|batch| {
                            let value = terms::encode_quad(&batch.quads()[state.row]);
                            state.row += 1;
                            vec![value]
                        })
                    }
                    Rows::Ask(result) => {
                        debug_assert_eq!(result.result().kind, QueryKind::Ask);
                        None
                    }
                    Rows::Scan(scan) => scan
                        .next()
                        .transpose()?
                        .map(|q| vec![terms::encode_quad(&q)]),
                    Rows::Query(r) => query_result_row(r, c.pos),
                    Rows::Collected(r) => query_result_row(r.result(), c.pos),
                };
                let Some(row) = row else { break };
                c.pos += 1;
                let mut cells = Vec::new();
                for term in row {
                    if term.is_null() {
                        cells.push(0);
                        continue;
                    }
                    let key = term.to_string();
                    let id = if let Some(id) = dictionary.get(&key) {
                        *id
                    } else {
                        let id = terms.len() as u32;
                        bytes += key.len();
                        dictionary.insert(key, id);
                        terms.push(term);
                        id
                    };
                    cells.push(id);
                }
                bytes += cells.len() * 12 + 16;
                rows.push(cells);
            }
            if rows.is_empty() {
                match &c.rows {
                    Rows::Streaming(state) => {
                        *statistics.lock() = Some(state.stats_json(c.pos, false)?)
                    }
                    Rows::Graph(state) => {
                        *statistics.lock() = Some(state.stats_json(c.pos, false)?)
                    }
                    _ => {}
                }
                lock.take();
                return Ok("null".into());
            }
            Ok(json!({ "terms": terms, "rows": rows }).to_string())
        })
        .await
    }
}

#[napi]
pub struct NativeTransaction {
    inner: Arc<Mutex<Option<TxnWorker>>>,
    permit: Arc<Mutex<Option<OwnedSemaphorePermit>>>,
    poisoned: Arc<AtomicBool>,
    _cleanup_marker: Arc<()>,
}
#[napi]
impl NativeTransaction {
    #[napi]
    pub fn apply<'env>(
        &self,
        env: &'env napi::Env,
        operations: String,
    ) -> napi::Result<PromiseRaw<'env, String>> {
        if self.poisoned.load(Ordering::Relaxed) {
            return Err(invalid("transaction was aborted by an earlier failure"));
        }
        let poisoned = self.poisoned.clone();
        let values = parse(&operations)?;
        let worker = self.inner.clone();
        env.spawn_future(async move {
            let result = blocking(move || {
                let worker = worker.lock();
                let worker = worker
                    .as_ref()
                    .ok_or_else(|| EngineError::invalid("transaction ended"))?;
                worker.run(move |tx| {
                    let mut inserted = 0u64;
                    let mut deleted = 0u64;
                    let operations = values
                        .as_array()
                        .ok_or_else(|| EngineError::invalid("operations must be an array"))?
                        .iter()
                        .map(|v| {
                            Ok((
                                v[0].as_bool().ok_or_else(|| {
                                    EngineError::invalid("operation must be boolean")
                                })?,
                                terms::quad(&v[1])?,
                            ))
                        })
                        .collect::<sparkles::Result<Vec<_>>>()?;
                    for (insert, q) in operations {
                        if insert {
                            inserted += tx.insert(q.as_ref())? as u64
                        } else {
                            deleted += tx.remove(q.as_ref())? as u64
                        }
                    }
                    Ok(json!({
                        "inserted": inserted.to_string(),
                        "deleted": deleted.to_string(),
                    })
                    .to_string())
                })
            })
            .await;
            if result.is_err() {
                poisoned.store(true, Ordering::Relaxed)
            }
            result
        })
    }
    #[napi]
    pub async fn query(
        &self,
        text: String,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<NativeResult> {
        let worker = self.inner.clone();
        let v = parse(&options)?;
        if v["execution"].as_str().is_some_and(|mode| mode != "eager") {
            return Err(invalid(
                "finish the transaction before opening a streaming cursor",
            ));
        }
        let flag = cancel.flag.clone();
        blocking(move || {
            let worker = worker.lock();
            worker
                .as_ref()
                .ok_or_else(|| EngineError::invalid("transaction ended"))?
                .run(move |tx| tx.query_with(&text, &query_options(&v, flag)?))
                .map(NativeResult::query)
        })
        .await
    }
    #[napi]
    pub async fn update(
        &self,
        text: String,
        options: String,
        cancel: &Cancellation,
    ) -> napi::Result<String> {
        if self.poisoned.load(Ordering::Relaxed) {
            return Err(invalid("transaction was aborted by an earlier failure"));
        }
        let worker = self.inner.clone();
        let v = parse(&options)?;
        let flag = cancel.flag.clone();
        let result = blocking(move || {
            let worker = worker.lock();
            worker
                .as_ref()
                .ok_or_else(|| EngineError::invalid("transaction ended"))?
                .run(move |tx| {
                    let r = tx.update_with(&text, &query_options(&v, flag)?)?;
                    Ok(json!({
                        "inserted": r.inserted.to_string(),
                        "deleted": r.deleted.to_string(),
                    })
                    .to_string())
                })
        })
        .await;
        if result
            .as_ref()
            .is_err_and(|e| !e.reason.contains("SparqlSyntaxError"))
        {
            self.poisoned.store(true, Ordering::Relaxed)
        }
        result
    }
    #[napi]
    pub async fn matched(&self, pattern: String) -> napi::Result<NativeResult> {
        let worker = self.inner.clone();
        let v = parse(&pattern)?;
        blocking(move || {
            let worker = worker.lock();
            worker
                .as_ref()
                .ok_or_else(|| EngineError::invalid("transaction ended"))?
                .run(move |tx| {
                    Ok(NativeResult::scan(sparkles::embed::quads_in(
                        tx.snapshot(),
                        &quad_pattern(&v)?,
                    )))
                })
        })
        .await
    }
    #[napi]
    pub fn end<'env>(
        &self,
        env: &'env napi::Env,
        commit: bool,
    ) -> napi::Result<PromiseRaw<'env, String>> {
        let permit = self.permit.clone();
        let worker = self.inner.clone();
        let poisoned = self.poisoned.load(Ordering::Relaxed);
        env.spawn_future(async move {
            let r = blocking(move || {
                let worker = worker
                    .lock()
                    .take()
                    .ok_or_else(|| EngineError::invalid("transaction ended"))?;
                if commit && !poisoned {
                    worker.commit().map(|r| receipt(&r).to_string())
                } else {
                    worker.abort();
                    if commit {
                        Err(EngineError::invalid(
                            "transaction was aborted by an earlier failure",
                        ))
                    } else {
                        Ok("null".into())
                    }
                }
            })
            .await;
            permit.lock().take();
            r
        })
    }
}
