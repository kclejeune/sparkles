//! Ingestion inside the server (spec C18 Phase 4): a document becomes a registered source
//! and proposed facts on a review branch, with no agent in between.
//!
//! - [`convert`] turns Markdown, HTML and PDF into the text of a rendition (§7.1), with
//!   PDF through pdf-inspector behind the `pdf` feature and OCR behind `pdf-ocr`
//!   (§7.1.1), and Word, PowerPoint and OpenDocument files through `sparkles-office`
//!   behind `office`; spreadsheets are mapped sheet by sheet (§7.8).
//! - [`pipeline`] runs one ingestion as its caller: conversion, `register_source` on the
//!   review branch, the cost estimate and its confirmation (§7.9), extraction through the
//!   `extract` role with escalation along its list (§7.4), linking (§7.5) and
//!   `assert_facts` with a span for every fact (§7.6), in the review modes of §7.7.
//! - [`csv`] drafts a C05 mapping for a table from its header and a sample (§7.8).
//! - [`http`] serves `POST /$/ingest/{ds}` as a task with progress, usage and a result,
//!   and `sparkles ingest` ([`cli`]) runs the same pipeline on a database directory.
//!
//! Every tool runs through the MCP server as the caller, over the caller's view, so an
//! ingestion can write nothing that the caller could not write with `register_source` and
//! `assert_facts` itself.

pub mod cli;
pub mod convert;
mod csv;
mod extract;
mod html;
pub mod http;
#[cfg(any(feature = "pdf", feature = "office"))]
pub mod isolate;
mod link;
pub mod maintain;
#[cfg(feature = "office")]
pub(crate) mod office;
#[cfg(feature = "pdf")]
mod pdf;
pub mod pipeline;

#[cfg(test)]
#[cfg_attr(not(feature = "pdf"), allow(dead_code))]
mod fixtures;
#[cfg(test)]
mod tests;

pub use cli::IngestArgs;

use parking_lot::{Condvar, Mutex};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// The default number of PDF conversions that may run at once (`--pdf-workers`).
pub const DEFAULT_PDF_WORKERS: usize = 2;
/// How long a finished task stays readable, which is how long a preview waits for its
/// approval (§7.7).
pub const KEEP_FINISHED: Duration = Duration::from_secs(7 * 86_400);
/// How long a task waits for the confirmation of its estimate.
pub const CONFIRM_WAIT: Duration = Duration::from_secs(86_400);
/// The most tasks the server keeps, finished ones included.
pub const MAX_TASKS: usize = 500;
/// The most tasks that may be active at once.
pub const MAX_ACTIVE: usize = 32;

/// The default `--convert-memory`, in MiB.
pub const DEFAULT_CONVERT_MEMORY_MIB: u64 = 2048;

/// `serve` flags of ingestion.
#[derive(clap::Args, Clone, Debug)]
pub struct IngestServeArgs {
    /// How many PDF and Office conversions may run at once (spec C18 §7.1.1)
    #[arg(long, value_name = "N", default_value_t = DEFAULT_PDF_WORKERS)]
    pub pdf_workers: usize,
    /// The address-space limit of each PDF or Office conversion's child process, in MiB
    /// (at least 256)
    #[arg(long, value_name = "MIB", default_value_t = DEFAULT_CONVERT_MEMORY_MIB)]
    pub convert_memory: u64,
    /// Convert PDF and Office files on threads of the server instead of in child
    /// processes; a conversion that overruns its deadline then keeps its slot until it
    /// ends, and its memory is bounded only by the converters' own limits
    #[arg(long)]
    pub convert_in_process: bool,
    #[command(flatten)]
    pub ocr: OcrArgs,
}

impl Default for IngestServeArgs {
    fn default() -> IngestServeArgs {
        IngestServeArgs {
            pdf_workers: DEFAULT_PDF_WORKERS,
            convert_memory: DEFAULT_CONVERT_MEMORY_MIB,
            convert_in_process: false,
            ocr: OcrArgs::default(),
        }
    }
}

/// The OCR flags of `serve` and `sparkles ingest` (feature `pdf-ocr`).
#[cfg(feature = "pdf-ocr")]
#[derive(clap::Args, Clone, Debug, Default)]
pub struct OcrArgs {
    /// The directory of the PP-OCR models that OCR reads; without it a PDF page that
    /// needs OCR is refused with `needs-ocr`. Models are never downloaded
    #[arg(long, value_name = "DIR")]
    pub pdf_ocr_models: Option<std::path::PathBuf>,
    /// The PDFium shared library that renders pages for OCR (default: PDFium's own
    /// discovery, such as PDFIUM_LIB_PATH)
    #[arg(long, value_name = "PATH", requires = "pdf_ocr_models")]
    pub pdfium_lib: Option<std::path::PathBuf>,
    /// The ONNX Runtime shared library that runs the OCR models (default: the platform's
    /// search path, or ORT_DYLIB_PATH)
    #[arg(long, value_name = "PATH", requires = "pdf_ocr_models")]
    pub onnxruntime_lib: Option<std::path::PathBuf>,
}

/// Without the `pdf-ocr` feature there are no OCR flags.
#[cfg(not(feature = "pdf-ocr"))]
#[derive(clap::Args, Clone, Debug, Default)]
pub struct OcrArgs {}

/// Where OCR finds its models and libraries.
#[cfg(feature = "pdf-ocr")]
#[derive(Clone, Debug)]
pub struct OcrConfig {
    pub models: std::path::PathBuf,
    pub pdfium: Option<std::path::PathBuf>,
}

/// A build without `pdf-ocr` has no OCR configuration.
#[cfg(not(feature = "pdf-ocr"))]
#[derive(Clone, Debug)]
pub enum OcrConfig {}

impl OcrArgs {
    /// The OCR configuration, when OCR is configured. With an ONNX Runtime path, this
    /// sets `ORT_DYLIB_PATH`, which ONNX Runtime's loader reads; call it before the
    /// server starts its threads.
    pub fn config(&self) -> Option<OcrConfig> {
        #[cfg(feature = "pdf-ocr")]
        {
            let models = self.pdf_ocr_models.clone()?;
            if let Some(lib) = &self.onnxruntime_lib {
                // SAFETY: called while the process starts, before the runtime and the
                // other threads that could read the environment exist
                unsafe { std::env::set_var("ORT_DYLIB_PATH", lib) };
            }
            Some(OcrConfig {
                models,
                pdfium: self.pdfium_lib.clone(),
            })
        }
        #[cfg(not(feature = "pdf-ocr"))]
        None
    }
}

/// The PDF workers and the OCR configuration.
#[derive(Debug)]
// the workers and OCR serve PDF conversion only
#[cfg_attr(not(feature = "pdf"), allow(dead_code))]
pub struct PdfRuntime {
    max: usize,
    used: Mutex<usize>,
    cv: Condvar,
    ocr: Option<OcrConfig>,
    /// the child processes that conversions run in, when the binary provides them
    #[cfg(any(feature = "pdf", feature = "office"))]
    isolation: Option<isolate::Isolation>,
    /// why conversions are refused: the child processes could not start safely
    #[cfg(any(feature = "pdf", feature = "office"))]
    broken: Option<String>,
}

/// One running PDF or Office conversion's worker slot.
#[cfg(any(feature = "pdf", feature = "office"))]
pub struct PdfSlot(Arc<PdfRuntime>);

#[cfg(any(feature = "pdf", feature = "office"))]
impl Drop for PdfSlot {
    fn drop(&mut self) {
        *self.0.used.lock() -= 1;
        self.0.cv.notify_one();
    }
}

impl PdfRuntime {
    pub fn new(workers: usize, ocr: Option<OcrConfig>) -> PdfRuntime {
        PdfRuntime {
            max: workers.max(1),
            used: Mutex::new(0),
            cv: Condvar::new(),
            ocr,
            #[cfg(any(feature = "pdf", feature = "office"))]
            isolation: None,
            #[cfg(any(feature = "pdf", feature = "office"))]
            broken: None,
        }
    }

    /// Run conversions in child processes as `iso` says.
    #[cfg(any(feature = "pdf", feature = "office"))]
    pub fn with_isolation(mut self, iso: Option<isolate::Isolation>) -> PdfRuntime {
        self.isolation = iso;
        self
    }

    #[cfg(any(feature = "pdf", feature = "office"))]
    pub fn isolation(&self) -> Option<&isolate::Isolation> {
        self.isolation.as_ref()
    }

    /// Refuse every conversion with `why`, because its child process cannot start safely.
    #[cfg(any(feature = "pdf", feature = "office"))]
    pub fn with_broken(mut self, why: Option<String>) -> PdfRuntime {
        self.broken = why;
        self
    }

    /// The refusal of a conversion when the child processes cannot start safely.
    #[cfg(any(feature = "pdf", feature = "office"))]
    pub fn child_refusal(&self) -> Option<convert::Refusal> {
        self.broken.as_ref().map(|why| {
            convert::Refusal::new(
                "conversion-failed",
                500,
                format!(
                    "this server cannot convert PDF and Office files: its conversion child \
                     could not start safely ({why}); restart it with --convert-in-process to \
                     convert on threads without the child's limits"
                ),
            )
        })
    }

    /// The OCR configuration, which a conversion child is given.
    #[cfg(any(feature = "pdf", feature = "office"))]
    pub fn ocr_config(&self) -> Option<&OcrConfig> {
        self.ocr.as_ref()
    }

    /// A worker slot, waiting until `deadline` for one.
    #[cfg(any(feature = "pdf", feature = "office"))]
    pub fn acquire(this: &Arc<PdfRuntime>, deadline: Instant) -> Option<PdfSlot> {
        let mut used = this.used.lock();
        while *used >= this.max {
            if this.cv.wait_until(&mut used, deadline).timed_out() && *used >= this.max {
                return None;
            }
        }
        *used += 1;
        Some(PdfSlot(this.clone()))
    }

    #[cfg(feature = "pdf")]
    pub fn ocr(&self) -> Option<&OcrConfig> {
        self.ocr.as_ref()
    }

    /// Whether this server converts PDFs and Office documents, and reads scanned pages by OCR.
    pub fn capabilities(&self) -> Value {
        json!({ "pdf": cfg!(feature = "pdf"), "ocr": self.ocr.is_some(), "office": cfg!(feature = "office") })
    }
}

impl Default for PdfRuntime {
    fn default() -> PdfRuntime {
        PdfRuntime::new(DEFAULT_PDF_WORKERS, None)
    }
}

/// The state of a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Queued,
    /// a maintenance task reads the memory it works on
    Scanning,
    Converting,
    Registering,
    /// the estimate is above the dataset's threshold: waiting for `confirm`
    AwaitingConfirmation,
    Extracting,
    Linking,
    Writing,
    /// a preview whose proposals wait for `approve`
    AwaitingApproval,
    Done,
    Failed,
    Cancelled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Scanning => "scanning",
            Status::Converting => "converting",
            Status::Registering => "registering",
            Status::AwaitingConfirmation => "awaiting-confirmation",
            Status::Extracting => "extracting",
            Status::Linking => "linking",
            Status::Writing => "writing",
            Status::AwaitingApproval => "awaiting-approval",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
        }
    }

    /// Whether the task still runs (a preview waiting for approval has ended its run).
    pub fn active(self) -> bool {
        !matches!(
            self,
            Status::Done | Status::Failed | Status::Cancelled | Status::AwaitingApproval
        )
    }
}

fn now_rfc3339() -> String {
    chrono::DateTime::<chrono::Utc>::from(SystemTime::now())
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// What a task reports.
#[derive(Debug)]
pub struct TaskState {
    pub status: Status,
    pub progress: f32,
    pub message: Option<String>,
    pub updated: String,
    pub finished: Option<String>,
    pub finished_at: Option<Instant>,
    pub estimate: Option<Value>,
    pub usage: Value,
    pub result: Option<Value>,
    pub error: Option<Value>,
    pub confirmed: bool,
    /// what an approval of a preview writes
    pub approval: Option<pipeline::Approval>,
}

/// One ingestion task.
#[derive(Debug)]
pub struct Task {
    pub id: String,
    pub dataset: String,
    /// the principal that started it ([`crate::auth::Principal::id`])
    pub owner: String,
    pub created: String,
    /// what was ingested: name, format, bytes, title, URL
    pub input: Value,
    pub state: Mutex<TaskState>,
    cv: Condvar,
    pub cancel: Arc<AtomicBool>,
}

impl Task {
    pub fn new(dataset: &str, owner: &str, input: Value) -> Task {
        Task {
            id: uuid::Uuid::new_v4().simple().to_string()[..16].to_string(),
            dataset: dataset.to_string(),
            owner: owner.to_string(),
            created: now_rfc3339(),
            input,
            state: Mutex::new(TaskState {
                status: Status::Queued,
                progress: 0.0,
                message: None,
                updated: now_rfc3339(),
                finished: None,
                finished_at: None,
                estimate: None,
                usage: json!({}),
                result: None,
                error: None,
                confirmed: false,
                approval: None,
            }),
            cv: Condvar::new(),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Move to `status` at `progress` (0 to 1), with a message.
    pub fn set(&self, status: Status, progress: f32, message: Option<String>) {
        let mut s = self.state.lock();
        if !s.status.active() {
            return;
        }
        s.status = status;
        s.progress = progress.clamp(0.0, 1.0);
        s.message = message;
        s.updated = now_rfc3339();
        self.cv.notify_all();
    }

    pub fn update(&self, f: impl FnOnce(&mut TaskState)) {
        let mut s = self.state.lock();
        f(&mut s);
        s.updated = now_rfc3339();
        self.cv.notify_all();
    }

    /// End the task with its result, or with an error.
    pub fn finish(&self, out: Result<Value, Value>, usage: Value) {
        let mut s = self.state.lock();
        let cancelled = self.cancel.load(Ordering::Relaxed);
        match out {
            Ok(v) => {
                let preview = s.approval.is_some();
                s.status = if preview {
                    Status::AwaitingApproval
                } else {
                    Status::Done
                };
                s.progress = 1.0;
                s.message = None;
                s.result = Some(v);
            }
            Err(e) => {
                s.status = if cancelled || e["code"] == "cancelled" {
                    Status::Cancelled
                } else {
                    Status::Failed
                };
                s.message = e["message"].as_str().map(str::to_string);
                s.error = Some(e);
            }
        }
        s.usage = usage;
        s.finished = Some(now_rfc3339());
        s.finished_at = Some(Instant::now());
        s.updated = now_rfc3339();
        self.cv.notify_all();
    }

    /// Wait until the estimate is confirmed (`true`), or the task is cancelled or the
    /// wait expires (`false`).
    pub fn wait_confirmation(&self, wait: Duration) -> bool {
        let until = Instant::now() + wait;
        let mut s = self.state.lock();
        loop {
            if s.confirmed {
                return true;
            }
            if self.cancel.load(Ordering::Relaxed) {
                return false;
            }
            if self.cv.wait_until(&mut s, until).timed_out() {
                return s.confirmed;
            }
        }
    }

    /// Wait until the task has ended or waits for its caller, at most `wait`. A task
    /// that was cancelled while it waited for confirmation is about to end, so this
    /// waits for that.
    pub fn wait_settled(&self, wait: Duration) {
        let until = Instant::now() + wait;
        let mut s = self.state.lock();
        while s.status.active()
            && (s.status != Status::AwaitingConfirmation || self.cancel.load(Ordering::Relaxed))
        {
            if self.cv.wait_until(&mut s, until).timed_out() {
                return;
            }
        }
    }

    /// Wake a waiting task after a confirmation or a cancel.
    pub fn wake(&self) {
        self.cv.notify_all();
    }

    pub fn json(&self) -> Value {
        let s = self.state.lock();
        let mut j = json!({
            "id": self.id,
            "dataset": self.dataset,
            "status": s.status.as_str(),
            "progress": (f64::from(s.progress) * 1000.0).round() / 1000.0,
            "createdAt": self.created,
            "updatedAt": s.updated,
            "input": self.input,
            "usage": s.usage,
        });
        if let Some(m) = &s.message {
            j["message"] = m.clone().into();
        }
        if let Some(f) = &s.finished {
            j["finishedAt"] = f.clone().into();
        }
        if let Some(e) = &s.estimate {
            j["estimate"] = e.clone();
        }
        if let Some(r) = &s.result {
            j["result"] = r.clone();
        }
        if let Some(e) = &s.error {
            j["error"] = e.clone();
        }
        j
    }
}

/// The ingestion tasks of a server and its PDF workers.
#[derive(Default)]
pub struct Runtime {
    pub pdf: Arc<PdfRuntime>,
    tasks: Mutex<BTreeMap<String, Arc<Task>>>,
    /// the open items of each dataset's review inbox
    pub review: crate::mcp::memory::pending::Registry,
}

/// The PDF workers of `args`, with child processes when the binary provides them and
/// `--convert-in-process` is not given.
/// The PDF workers of `args`, with child processes when the binary provides them, the
/// platform is Linux and `--convert-in-process` is not given. A child is started once
/// here to check that it can set its limits. When it cannot, conversions are refused
/// with an error that names the reason, rather than run without the limits.
fn pdf_runtime(args: &IngestServeArgs) -> PdfRuntime {
    let rt = PdfRuntime::new(args.pdf_workers, args.ocr.config());
    #[cfg(any(feature = "pdf", feature = "office"))]
    let rt = {
        // RLIMIT_AS is enforced on Linux only: macOS accepts it and does not apply it
        let iso = isolate::child_exe()
            .filter(|_| !args.convert_in_process && cfg!(target_os = "linux"))
            .map(|exe| isolate::Isolation {
                exe,
                // below this the binary itself cannot start its threads
                memory: args.convert_memory.max(256).saturating_mul(1 << 20),
                prefix: Vec::new(),
            });
        let broken = iso.as_ref().and_then(|i| isolate::probe(i).err());
        if let Some(why) = &broken {
            tracing::error!(
                "PDF and Office conversions are refused: the conversion child could not start \
                 safely ({why}); --convert-in-process converts on threads instead"
            );
        }
        rt.with_isolation(iso).with_broken(broken)
    };
    rt
}

impl Runtime {
    pub fn new(args: &IngestServeArgs) -> Runtime {
        Runtime {
            pdf: Arc::new(pdf_runtime(args)),
            tasks: Mutex::default(),
            review: Default::default(),
        }
    }

    /// Add a task, dropping finished ones past their time or past [`MAX_TASKS`]:
    /// `Err` when [`MAX_ACTIVE`] tasks run already.
    pub fn add(&self, task: Arc<Task>) -> Result<(), String> {
        let mut tasks = self.tasks.lock();
        tasks.retain(|_, t| {
            let s = t.state.lock();
            s.status.active() || s.finished_at.is_none_or(|f| f.elapsed() < KEEP_FINISHED)
        });
        let active = tasks
            .values()
            .filter(|t| t.state.lock().status.active())
            .count();
        if active >= MAX_ACTIVE {
            return Err(format!(
                "{active} ingestions are running; try again when one has ended"
            ));
        }
        while tasks.len() >= MAX_TASKS {
            let oldest = tasks
                .iter()
                .filter(|(_, t)| !t.state.lock().status.active())
                .min_by(|a, b| a.1.created.cmp(&b.1.created))
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    tasks.remove(&k);
                }
                None => break,
            }
        }
        tasks.insert(task.id.clone(), task);
        Ok(())
    }

    pub fn remove(&self, id: &str) {
        self.tasks.lock().remove(id);
    }

    pub fn get(&self, id: &str) -> Option<Arc<Task>> {
        self.tasks.lock().get(id).cloned()
    }

    /// The tasks of `dataset`, newest first.
    pub fn list(&self, dataset: &str) -> Vec<Arc<Task>> {
        let mut v: Vec<Arc<Task>> = self
            .tasks
            .lock()
            .values()
            .filter(|t| t.dataset == dataset)
            .cloned()
            .collect();
        v.sort_by(|a, b| b.created.cmp(&a.created));
        v
    }
}
