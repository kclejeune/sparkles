//! Write guards: checks that run on a transaction's post-state under the writer lock,
//! before anything is written, and may reject the commit. `sparkles-shacl` provides
//! write-time SHACL validation as a guard; the store only knows this interface.

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
}

/// Sees the outcome of every guard decision of a store (for metrics and logs): the
/// summary of a check (including rejections) or of a bypass, or the error that aborted
/// the check, with the time the check took.
pub trait GuardObserver: Send + Sync {
    fn observe(
        &self,
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
    /// A summary with no findings.
    pub fn empty(status: GuardStatus, mode: GuardMode, threshold: Severity) -> ValidationSummary {
        ValidationSummary {
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

    /// `Sparkles-Validation` header value (an RFC 9651 Dictionary).
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
        format!(
            "status={}, mode={}, strategy={}, blocking={}, total={}, violations={}, warnings={}, infos={}, ms={}",
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

/// Serialize an optional shared summary (for [`crate::commit::Receipt`]).
pub(crate) fn serialize_summary<S: serde::Serializer>(
    v: &Option<Arc<ValidationSummary>>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    match v {
        Some(v) => v.as_ref().serialize(s),
        None => s.serialize_none(),
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
