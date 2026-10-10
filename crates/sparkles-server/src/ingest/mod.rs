//! Ingestion inside the server (spec C18 Phase 4): a document becomes a registered source
//! and proposed facts on a review branch, with no agent in between.
//!
//! - [`convert`] turns Markdown, HTML and PDF into the text of a rendition (§7.1), with
//!   PDF through pdf-inspector behind the `pdf` feature and OCR behind `pdf-ocr`
//!   (§7.1.1).
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

/// `serve` flags of ingestion.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct IngestServeArgs {
    /// How many PDF conversions may run at once; a conversion that overruns its
    /// deadline keeps its slot until it ends (spec C18 §7.1.1)
    #[arg(long, value_name = "N", default_value_t = DEFAULT_PDF_WORKERS)]
    pub pdf_workers: usize,
    #[command(flatten)]
    pub ocr: OcrArgs,
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
}

/// One running PDF conversion's worker slot.
#[cfg(feature = "pdf")]
pub struct PdfSlot(Arc<PdfRuntime>);

#[cfg(feature = "pdf")]
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
        }
    }

    /// A worker slot, waiting until `deadline` for one.
    #[cfg(feature = "pdf")]
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

    /// Whether this server converts PDFs, and reads scanned pages by OCR.
    pub fn capabilities(&self) -> Value {
        json!({ "pdf": cfg!(feature = "pdf"), "ocr": self.ocr.is_some() })
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
}

impl Runtime {
    pub fn new(args: &IngestServeArgs) -> Runtime {
        Runtime {
            pdf: Arc::new(PdfRuntime::new(args.pdf_workers, args.ocr.config())),
            tasks: Mutex::default(),
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
