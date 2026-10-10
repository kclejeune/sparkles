//! Local text embeddings with Candle (spec F12).
//!
//! An [`Embedder`] reads a sentence-transformers snapshot from a directory (an
//! operator's directory or a snapshot of [`sparkles_modelstore`]), checks its
//! configuration at once, and loads the weights the first time it embeds. A dispatcher
//! thread runs the batches on a small dedicated thread pool at a lower scheduling
//! priority, serves queries before documents, and drops the weights after an idle time.
//!
//! ```no_run
//! use sparkles_embed::{Embedder, Kind, ModelSpec, Options};
//! let e = Embedder::new(ModelSpec::new("/var/lib/sparkles/models/org/model/rev"), Options::default())?;
//! let v = e.embed(&["rivers of southern France"], Kind::Query)?;
//! # Ok::<(), sparkles_embed::Error>(())
//! ```

mod model;
pub mod pooling;
mod qwen3;
pub mod snapshot;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};

pub use pooling::Pooling;
pub use snapshot::{Arch, SnapshotConfig, hub_select};

use model::{Loaded, Settings};

/// Errors of the embedding runtime.
#[derive(Debug, thiserror::Error, Clone)]
pub enum Error {
    /// The snapshot's files are missing or malformed.
    #[error("model snapshot: {0}")]
    Snapshot(String),
    /// The snapshot needs something this crate does not implement.
    #[error("unsupported model: {0}")]
    Unsupported(String),
    /// A setting is out of range.
    #[error("invalid model setting: {0}")]
    Invalid(String),
    /// Loading or running the model failed.
    #[error("embedding model: {0}")]
    Model(String),
    /// The queue holds too many texts.
    #[error("the embedding queue is full")]
    Busy,
    /// The runtime is shutting down.
    #[error("the embedding runtime stopped")]
    Stopped,
}

pub type Result<T> = std::result::Result<T, Error>;

/// The element type of the weights in memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dtype {
    #[default]
    F32,
    Bf16,
}

/// Whether a text is a search query or a stored document. The two may get different
/// prompts (instructions).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Query,
    Document,
}

/// A model to run: a snapshot directory and overrides of what it declares.
#[derive(Clone, Debug)]
pub struct ModelSpec {
    pub dir: PathBuf,
    pub dtype: Dtype,
    /// Matryoshka truncation to this many components (renormalized when the model
    /// normalizes).
    pub dimension: Option<usize>,
    /// Token limit per text, at most the model's own.
    pub max_tokens: Option<usize>,
    pub pooling: Option<Pooling>,
    pub normalize: Option<bool>,
    /// Text put before queries, replacing the snapshot's `query` prompt.
    pub query_prompt: Option<String>,
    /// Text put before documents, replacing the snapshot's `document` prompt.
    pub document_prompt: Option<String>,
}

impl ModelSpec {
    pub fn new(dir: impl Into<PathBuf>) -> ModelSpec {
        ModelSpec {
            dir: dir.into(),
            dtype: Dtype::F32,
            dimension: None,
            max_tokens: None,
            pooling: None,
            normalize: None,
            query_prompt: None,
            document_prompt: None,
        }
    }
}

/// How the runtime uses the machine.
#[derive(Clone, Debug)]
pub struct Options {
    /// Threads of the inference pool.
    pub threads: usize,
    /// Drop the weights after this long without work; `None` keeps them.
    pub idle_unload: Option<Duration>,
    /// Texts that may wait per kind before [`Error::Busy`].
    pub max_queued: usize,
    /// Documents per forward pass. Queries wait at most one such pass.
    pub micro_batch: usize,
    /// Added to the nice value of the inference threads (Linux); 0 leaves it.
    pub nice: i32,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            threads: 2,
            idle_unload: Some(Duration::from_secs(600)),
            max_queued: 4096,
            micro_batch: 16,
            nice: 10,
        }
    }
}

/// What a model produces, resolved from the snapshot and the spec.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub arch: &'static str,
    pub dimension: usize,
    pub hidden_size: usize,
    pub pooling: Pooling,
    pub normalize: bool,
    pub max_tokens: usize,
    pub dtype: Dtype,
    pub query_prompt: String,
    pub document_prompt: String,
}

/// The state of the weights.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Unloaded,
    Loading,
    Loaded,
}

/// Counters and state for status pages.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub state: State,
    pub loads: u64,
    pub unloads: u64,
    pub texts: u64,
    pub batches: u64,
    pub failures: u64,
    /// Bytes of the weights at the chosen type while loaded.
    pub weight_bytes: Option<u64>,
    pub last_load_ms: Option<u64>,
    /// Seconds since the last batch.
    pub idle_secs: Option<u64>,
    pub queued_queries: usize,
    pub queued_documents: usize,
    pub last_error: Option<String>,
}

struct Job {
    texts: Vec<String>,
    /// indices of `texts`, longest first, so a micro-batch holds texts of similar length
    order: Vec<usize>,
    done: usize,
    out: Vec<Vec<f32>>,
    kind: Kind,
    reply: mpsc::SyncSender<Result<Vec<Vec<f32>>>>,
}

#[derive(Default)]
struct Queues {
    queries: VecDeque<Job>,
    docs: VecDeque<Job>,
    queued_q: usize,
    queued_d: usize,
    shutdown: bool,
    unload: bool,
}

struct Shared {
    q: Mutex<Queues>,
    cv: Condvar,
    status: Mutex<Status>,
    last_used: Mutex<Option<Instant>>,
}

/// A local embedding model with its own dispatcher thread and inference pool.
pub struct Embedder {
    shared: Arc<Shared>,
    info: Info,
    opts: Options,
    thread: Option<JoinHandle<()>>,
}

fn resolve(cfg: &SnapshotConfig, spec: &ModelSpec) -> Result<Settings> {
    let pooling = spec.pooling.or(cfg.pooling).ok_or_else(|| {
        Error::Snapshot(
            "the snapshot has no modules.json with a Pooling module; set the pooling explicitly"
                .into(),
        )
    })?;
    let normalize = spec.normalize.or(cfg.normalize).unwrap_or(true);
    let model_max = match (cfg.max_seq_length, cfg.max_position_embeddings) {
        (Some(m), _) => m,
        // RoBERTa offsets positions by the padding index plus one
        (None, Some(p)) if cfg.arch == Arch::XlmRoberta => p.saturating_sub(2),
        (None, Some(p)) => p.min(8192),
        (None, None) => 512,
    };
    let max_tokens = spec.max_tokens.unwrap_or(model_max);
    if max_tokens < 2 || max_tokens > model_max.max(2) {
        return Err(Error::Invalid(format!(
            "maxTokens {max_tokens} is outside 2..={model_max}"
        )));
    }
    let dimension = spec.dimension.unwrap_or(cfg.hidden_size);
    if dimension == 0 || dimension > cfg.hidden_size {
        return Err(Error::Invalid(format!(
            "dimension {dimension} is outside 1..={}",
            cfg.hidden_size
        )));
    }
    let query_prompt = spec
        .query_prompt
        .clone()
        .or_else(|| cfg.query_prompt.clone())
        .unwrap_or_default();
    let document_prompt = spec
        .document_prompt
        .clone()
        .or_else(|| cfg.document_prompt.clone())
        .unwrap_or_default();
    if !cfg.include_prompt
        && matches!(pooling, Pooling::Mean | Pooling::Max | Pooling::MeanSqrtLen)
        && (!query_prompt.is_empty() || !document_prompt.is_empty())
    {
        return Err(Error::Unsupported(
            "pooling that excludes prompt tokens (include_prompt: false) with a prompt".into(),
        ));
    }
    let append_eos = if cfg.arch.is_decoder() && pooling == Pooling::LastToken {
        Some(cfg.eos_token_id.ok_or_else(|| {
            Error::Snapshot("last-token pooling needs eos_token_id in config.json".into())
        })?)
    } else {
        None
    };
    Ok(Settings {
        pooling,
        normalize,
        max_tokens,
        dimension,
        query_prompt,
        document_prompt,
        left_padding: cfg.left_padding.unwrap_or(cfg.arch.is_decoder()),
        append_eos,
        pad_id: cfg.pad_token_id.or(cfg.eos_token_id).unwrap_or(0),
        lowercase: cfg.do_lower_case,
    })
}

impl Embedder {
    /// Read and check the snapshot's configuration and start the dispatcher. The weights
    /// are not read until the first [`embed`](Embedder::embed).
    pub fn new(spec: ModelSpec, opts: Options) -> Result<Embedder> {
        if opts.threads == 0 || opts.micro_batch == 0 || opts.max_queued == 0 {
            return Err(Error::Invalid(
                "threads, microBatch and maxQueued must be positive".into(),
            ));
        }
        let cfg = SnapshotConfig::read(&spec.dir)?;
        let settings = resolve(&cfg, &spec)?;
        if spec.dtype == Dtype::Bf16 && matches!(cfg.arch, Arch::Bert | Arch::XlmRoberta) {
            return Err(Error::Unsupported(format!(
                "{} runs in f32 only",
                cfg.arch.name()
            )));
        }
        let info = Info {
            arch: cfg.arch.name(),
            dimension: settings.dimension,
            hidden_size: cfg.hidden_size,
            pooling: settings.pooling,
            normalize: settings.normalize,
            max_tokens: settings.max_tokens,
            dtype: spec.dtype,
            query_prompt: settings.query_prompt.clone(),
            document_prompt: settings.document_prompt.clone(),
        };
        let shared = Arc::new(Shared {
            q: Mutex::new(Queues::default()),
            cv: Condvar::new(),
            status: Mutex::new(Status {
                state: State::Unloaded,
                loads: 0,
                unloads: 0,
                texts: 0,
                batches: 0,
                failures: 0,
                weight_bytes: None,
                last_load_ms: None,
                idle_secs: None,
                queued_queries: 0,
                queued_documents: 0,
                last_error: None,
            }),
            last_used: Mutex::new(None),
        });
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(opts.threads)
            .thread_name(|i| format!("sparkles-embed-{i}"))
            .start_handler({
                let nice = opts.nice;
                move |_| lower_priority(nice)
            })
            .build()
            .map_err(|e| Error::Model(format!("thread pool: {e}")))?;
        let th = {
            let shared = shared.clone();
            let opts = opts.clone();
            let dtype = spec.dtype;
            std::thread::Builder::new()
                .name("sparkles-embed".into())
                .spawn(move || dispatch(shared, pool, cfg, settings, dtype, opts))
                .map_err(|e| Error::Model(format!("dispatcher thread: {e}")))?
        };
        Ok(Embedder {
            shared,
            info,
            opts,
            thread: Some(th),
        })
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    pub fn options(&self) -> &Options {
        &self.opts
    }

    /// Embed `texts`, in order. Loads the weights first if they are not loaded. Fails
    /// with [`Error::Busy`] when more than `max_queued` texts of this kind would wait.
    pub fn embed(&self, texts: &[&str], kind: Kind) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let (tx, rx) = mpsc::sync_channel(1);
        let mut order: Vec<usize> = (0..texts.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(texts[i].len()));
        let job = Job {
            texts: texts.iter().map(|t| t.to_string()).collect(),
            order,
            done: 0,
            out: vec![Vec::new(); texts.len()],
            kind,
            reply: tx,
        };
        {
            let mut q = self.shared.q.lock();
            if q.shutdown {
                return Err(Error::Stopped);
            }
            let n = texts.len();
            match kind {
                Kind::Query => {
                    if q.queued_q + n > self.opts.max_queued {
                        return Err(Error::Busy);
                    }
                    q.queued_q += n;
                    q.queries.push_back(job);
                }
                Kind::Document => {
                    if q.queued_d + n > self.opts.max_queued {
                        return Err(Error::Busy);
                    }
                    q.queued_d += n;
                    q.docs.push_back(job);
                }
            }
            self.shared.cv.notify_all();
        }
        rx.recv().map_err(|_| Error::Stopped)?
    }

    /// Drop the weights now; the next embedding loads them again.
    pub fn unload(&self) {
        self.shared.q.lock().unload = true;
        self.shared.cv.notify_all();
    }

    pub fn status(&self) -> Status {
        let mut s = self.shared.status.lock().clone();
        {
            let q = self.shared.q.lock();
            s.queued_queries = q.queued_q;
            s.queued_documents = q.queued_d;
        }
        s.idle_secs = self.shared.last_used.lock().map(|t| t.elapsed().as_secs());
        s
    }
}

impl Drop for Embedder {
    fn drop(&mut self) {
        self.shared.q.lock().shutdown = true;
        self.shared.cv.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn dispatch(
    shared: Arc<Shared>,
    pool: rayon::ThreadPool,
    cfg: SnapshotConfig,
    settings: Settings,
    dtype: Dtype,
    opts: Options,
) {
    lower_priority(opts.nice);
    let mut model: Option<Loaded> = None;
    let unload = |model: &mut Option<Loaded>| {
        if model.take().is_some() {
            let mut s = shared.status.lock();
            s.state = State::Unloaded;
            s.unloads += 1;
            s.weight_bytes = None;
        }
    };
    loop {
        // wait for work, unloading when idle
        let mut job = {
            let mut q = shared.q.lock();
            loop {
                if q.shutdown {
                    let q = &mut *q;
                    for j in q.queries.drain(..).chain(q.docs.drain(..)) {
                        let _ = j.reply.send(Err(Error::Stopped));
                    }
                    return;
                }
                if q.unload {
                    q.unload = false;
                    unload(&mut model);
                }
                if let Some(j) = q.queries.pop_front() {
                    break j;
                }
                if let Some(j) = q.docs.pop_front() {
                    break j;
                }
                match (opts.idle_unload, model.is_some()) {
                    (Some(idle), true) => {
                        let since = shared
                            .last_used
                            .lock()
                            .map(|t| t.elapsed())
                            .unwrap_or_default();
                        if since >= idle {
                            unload(&mut model);
                        } else {
                            shared.cv.wait_for(&mut q, idle - since);
                        }
                    }
                    _ => shared.cv.wait(&mut q),
                }
            }
        };

        if model.is_none() {
            shared.status.lock().state = State::Loading;
            match pool.install(|| Loaded::load(&cfg, settings.clone(), dtype)) {
                Ok(m) => {
                    let mut s = shared.status.lock();
                    s.state = State::Loaded;
                    s.loads += 1;
                    s.weight_bytes = Some(m.weight_bytes);
                    s.last_load_ms = Some(m.load_ms);
                    model = Some(m);
                }
                Err(e) => {
                    {
                        let mut s = shared.status.lock();
                        s.state = State::Unloaded;
                        s.failures += 1;
                        s.last_error = Some(e.to_string());
                    }
                    finish(&shared, job, Err(e));
                    continue;
                }
            }
        }
        let m = model.as_ref().expect("loaded");

        // a query runs whole; a document job runs one micro-batch, then yields to queries
        let step = match job.kind {
            Kind::Query => job.texts.len() - job.done,
            Kind::Document => opts.micro_batch,
        };
        let mut failed = None;
        let end = (job.done + step).min(job.texts.len());
        while job.done < end {
            let to = (job.done + opts.micro_batch).min(end);
            let idx = &job.order[job.done..to];
            let texts: Vec<&str> = idx.iter().map(|&i| job.texts[i].as_str()).collect();
            match pool.install(|| m.embed(&texts, job.kind)) {
                Ok(vs) => {
                    for (&i, v) in idx.iter().zip(vs) {
                        job.out[i] = v;
                    }
                    let mut s = shared.status.lock();
                    s.texts += texts.len() as u64;
                    s.batches += 1;
                }
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
            job.done = to;
        }
        *shared.last_used.lock() = Some(Instant::now());
        if let Some(e) = failed {
            {
                let mut s = shared.status.lock();
                s.failures += 1;
                s.last_error = Some(e.to_string());
            }
            finish(&shared, job, Err(e));
        } else if job.done == job.texts.len() {
            let out = std::mem::take(&mut job.out);
            finish(&shared, job, Ok(out));
        } else {
            // back to the front of its queue
            shared.q.lock().docs.push_front(job);
        }
    }
}

fn finish(shared: &Shared, job: Job, result: Result<Vec<Vec<f32>>>) {
    {
        let mut q = shared.q.lock();
        match job.kind {
            Kind::Query => q.queued_q -= job.texts.len(),
            Kind::Document => q.queued_d -= job.texts.len(),
        }
    }
    let _ = job.reply.send(result);
}

/// Raise the calling thread's nice value by `n` (Linux, where nice is per thread). Other
/// platforms are left alone.
fn lower_priority(n: i32) {
    #[cfg(target_os = "linux")]
    if n > 0 {
        // SAFETY: plain system calls on the calling thread's id
        unsafe {
            let tid = libc::gettid() as libc::id_t;
            *libc::__errno_location() = 0;
            let cur = libc::getpriority(libc::PRIO_PROCESS, tid);
            let _ = libc::setpriority(libc::PRIO_PROCESS, tid, (cur + n).min(19));
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = n;
}
