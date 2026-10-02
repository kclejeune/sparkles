//! Write guards: checks that run on a transaction's post-state under the writer lock,
//! before anything is written, and may reject the commit. `sparkles-shacl` and
//! `sparkles-shex` provide write-time SHACL and ShEx validation as guards; the store only
//! knows this interface (and reads `mode` from [`config::CONFIG_FILE`]).

pub mod config;

use crate::commit::CommitKind;
use crate::error::Result;
use crate::id::Id;
use crate::store::Snapshot;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// A check run before every commit of a store.
pub trait CommitGuard: Send + Sync {
    /// Validate the candidate state. A summary with status [`GuardStatus::Rejected`]
    /// aborts the commit (the store turns it into [`crate::Error::Rejected`]); an error
    /// aborts it too.
    fn check(&self, c: &Candidate<'_>) -> Result<ValidationSummary>;
    /// Called after a commit the guard saw was published (with its sequence number).
    fn committed(&self, _seq: u64) {}
    /// Called when a write bypassed the guard.
    fn bypassed(&self) {}
    /// A short description, for errors and status.
    fn describe(&self) -> String;
    /// The shape language this guard validates with.
    fn language(&self) -> GuardLanguage {
        GuardLanguage::Shacl
    }
}

/// Sees the outcome of every guard decision of a store (for metrics and logs): the
/// summary of a check (including rejections) or of a bypass, or the error that aborted
/// the check, with the time the check took. `language` is the installed guard's (SHACL
/// when a required guard is missing).
pub trait GuardObserver: Send + Sync {
    fn observe(
        &self,
        language: GuardLanguage,
        kind: CommitKind,
        outcome: std::result::Result<&ValidationSummary, &crate::Error>,
        elapsed: Duration,
    );
}

/// A commit about to happen.
pub struct Candidate<'a> {
    /// the committed state the transaction started from
    pub base: &'a Snapshot,
    /// the state after the transaction (uncommitted)
    pub view: Arc<Snapshot>,
    pub kind: CommitKind,
    pub changes: Changes<'a>,
    pub opts: &'a WriteOptions,
}

/// What a transaction changed.
pub enum Changes<'a> {
    /// effective inserts (`1`) and deletes (`2`) of the WAL path
    Log(&'a [(u8, [Id; 4])]),
    /// a transaction committed by a rebuild: its small changes plus a bulk batch
    Rebuilt {
        log: &'a [(u8, [Id; 4])],
        bulk: &'a [[Id; 4]],
    },
    /// a rebuild from sources (bulk loads): anything may have changed
    Unknown,
}

impl Changes<'_> {
    /// The graph ids touched, or `None` when unknown.
    pub fn graphs(&self) -> Option<Vec<u64>> {
        let mut g: Vec<u64> = match self {
            Changes::Log(log) => log.iter().map(|(_, q)| q[3].0).collect(),
            Changes::Rebuilt { log, bulk } => log
                .iter()
                .map(|(_, q)| q[3].0)
                .chain(bulk.iter().map(|q| q[3].0))
                .collect(),
            Changes::Unknown => return None,
        };
        g.sort_unstable();
        g.dedup();
        Some(g)
    }
}

/// Per-write options for guards.
#[derive(Clone, Debug, Default)]
pub struct WriteOptions {
    /// skip the guard (counted and logged; callers decide who may)
    pub bypass_validation: bool,
    /// the request's deadline (a guard also applies its own budget)
    pub deadline: Option<Instant>,
    pub cancel: Option<Arc<AtomicBool>>,
    /// results carried in a summary (the guard's default if `None`)
    pub report_limit: Option<usize>,
    /// the message recorded with the commit (see
    /// [`annotations::validate_message`](crate::annotations::validate_message))
    pub message: Option<Arc<str>>,
    /// checked once the writer lock is held, before anything is written
    pub precondition: Option<Precondition>,
    /// fail with [`Error::WriterBusy`](crate::Error::WriterBusy) instead of waiting
    /// when another write holds the writer lock
    pub no_wait: bool,
}

/// A check of the committed state that a write depends on (an HTTP `If-Match`, say).
/// The store runs it with the writer lock held, on the head snapshot, so no other
/// commit can come between the check and the write. An `Err` (usually
/// [`Error::PreconditionFailed`](crate::Error::PreconditionFailed)) stops the write
/// before anything is written.
#[derive(Clone)]
pub struct Precondition(pub Arc<PreconditionFn>);

/// The check a [`Precondition`] runs.
pub type PreconditionFn = dyn Fn(&Snapshot) -> Result<()> + Send + Sync;

impl Precondition {
    pub fn new(f: impl Fn(&Snapshot) -> Result<()> + Send + Sync + 'static) -> Precondition {
        Precondition(Arc::new(f))
    }

    pub fn check(&self, head: &Snapshot) -> Result<()> {
        (self.0)(head)
    }
}

impl std::fmt::Debug for Precondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Precondition")
    }
}

impl WriteOptions {
    /// [`Error::Cancelled`](crate::Error::Cancelled) once `cancel` is set,
    /// [`Error::Timeout`](crate::Error::Timeout) past the deadline: loads, replaces and
    /// the wait for the writer lock stop there, before anything is published.
    pub fn check(&self) -> Result<()> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        {
            return Err(crate::Error::Cancelled);
        }
        if self.deadline.is_some_and(|d| Instant::now() > d) {
            return Err(crate::Error::Timeout);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GuardStatus {
    Passed,
    Warned,
    Rejected,
    Skipped,
    Bypassed,
}

impl GuardStatus {
    pub fn name(self) -> &'static str {
        match self {
            GuardStatus::Passed => "passed",
            GuardStatus::Warned => "warned",
            GuardStatus::Rejected => "rejected",
            GuardStatus::Skipped => "skipped",
            GuardStatus::Bypassed => "bypassed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuardMode {
    Reject,
    Warn,
    Off,
}

/// The shape language of a dataset's write-time validation (`language` in
/// `validation.json`; SHACL when absent).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuardLanguage {
    #[default]
    Shacl,
    Shex,
}

impl GuardLanguage {
    pub const ALL: [GuardLanguage; 2] = [GuardLanguage::Shacl, GuardLanguage::Shex];

    /// `shacl` or `shex` (the `language` label of the validation metrics).
    pub fn name(self) -> &'static str {
        match self {
            GuardLanguage::Shacl => "shacl",
            GuardLanguage::Shex => "shex",
        }
    }

    /// `SHACL` or `ShEx`, for messages.
    pub fn title(self) -> &'static str {
        match self {
            GuardLanguage::Shacl => "SHACL",
            GuardLanguage::Shex => "ShEx",
        }
    }

    /// The position in [`GuardLanguage::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    Full,
    Incremental,
    None,
}

/// A result severity, ranked `Violation` > `Warning` > `Info`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Violation,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SeverityCounts {
    pub violation: u64,
    pub warning: u64,
    pub info: u64,
}

/// What a guard found about one candidate commit.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationSummary {
    /// the guard's language; a ShEx guard's results are result-map entries, one per
    /// nonconformant association, each counted as a violation
    pub language: GuardLanguage,
    pub status: GuardStatus,
    pub mode: GuardMode,
    pub strategy: Strategy,
    pub threshold: Severity,
    /// `sh:conforms`: no results at all
    pub conforms: bool,
    /// results at or above the threshold
    pub blocking: u64,
    pub total: u64,
    pub by_severity: SeverityCounts,
    pub limit: usize,
    pub truncated: bool,
    pub millis: u64,
    /// the first `limit` results (blocking first), as JSON
    pub results: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shapes_error: Option<String>,
    /// the same results as a Turtle `sh:ValidationReport`, for rejections
    #[serde(skip)]
    pub report_turtle: Option<String>,
}

impl ValidationSummary {
    /// A summary with no findings (of a SHACL guard; set `language` for another).
    pub fn empty(status: GuardStatus, mode: GuardMode, threshold: Severity) -> ValidationSummary {
        ValidationSummary {
            language: GuardLanguage::Shacl,
            status,
            mode,
            strategy: Strategy::None,
            threshold,
            conforms: true,
            blocking: 0,
            total: 0,
            by_severity: SeverityCounts::default(),
            limit: 0,
            truncated: false,
            millis: 0,
            results: Vec::new(),
            shapes_error: None,
            report_turtle: None,
        }
    }

    /// `Sparkles-Validation` header value (an RFC 9651 Dictionary). A ShEx guard's adds
    /// `lang=shex` after `strategy`; a SHACL guard's has no `lang`, as before format 2.
    pub fn header(&self) -> String {
        let mode = match self.mode {
            GuardMode::Reject => "reject",
            GuardMode::Warn => "warn",
            GuardMode::Off => "off",
        };
        let strategy = match self.strategy {
            Strategy::Full => "full",
            Strategy::Incremental => "incremental",
            Strategy::None => "none",
        };
        let lang = match self.language {
            GuardLanguage::Shacl => "",
            GuardLanguage::Shex => ", lang=shex",
        };
        format!(
            "status={}, mode={}, strategy={}{lang}, blocking={}, total={}, violations={}, warnings={}, infos={}, ms={}",
            self.status.name(),
            mode,
            strategy,
            self.blocking,
            self.total,
            self.by_severity.violation,
            self.by_severity.warning,
            self.by_severity.info,
            self.millis
        )
    }
}

/// A commit a guard rejected: nothing was written.
#[derive(Clone, Debug)]
pub struct Rejection {
    pub summary: ValidationSummary,
    /// the head, unchanged
    pub head: u64,
    pub kind: CommitKind,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.summary.language == GuardLanguage::Shex {
            let n = self.summary.blocking;
            return match &self.summary.shapes_error {
                Some(e) => write!(
                    f,
                    "ShEx validation failed: the schema cannot be used ({e}); nothing was committed"
                ),
                None => write!(
                    f,
                    "ShEx validation failed: {n} nonconformant association{}; nothing was committed",
                    if n == 1 { "" } else { "s" }
                ),
            };
        }
        match &self.summary.shapes_error {
            Some(e) => write!(
                f,
                "SHACL validation failed: the shapes graph cannot be read ({e}); nothing was committed"
            ),
            None => write!(
                f,
                "SHACL validation failed: {} blocking result{} (threshold {}); nothing was committed",
                self.summary.blocking,
                if self.summary.blocking == 1 { "" } else { "s" },
                match self.summary.threshold {
                    Severity::Violation => "violation",
                    Severity::Warning => "warning",
                    Severity::Info => "info",
                }
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shex_summaries() {
        let mut s = ValidationSummary::empty(
            GuardStatus::Rejected,
            GuardMode::Reject,
            Severity::Violation,
        );
        s.strategy = Strategy::Full;
        s.blocking = 3;
        assert!(
            s.header()
                .starts_with("status=rejected, mode=reject, strategy=full, blocking=3,")
        );
        s.language = GuardLanguage::Shex;
        assert!(
            s.header()
                .starts_with("status=rejected, mode=reject, strategy=full, lang=shex, blocking=3,"),
            "{}",
            s.header()
        );
        let r = Rejection {
            summary: s,
            head: 1,
            kind: CommitKind::Transaction,
        };
        assert_eq!(
            r.to_string(),
            "ShEx validation failed: 3 nonconformant associations; nothing was committed"
        );
        assert_eq!(
            serde_json::to_value(&r.summary).unwrap()["language"],
            "shex"
        );
    }
}
