//! Write previews (dry runs): a write that runs as it would, up to its commit, reports
//! what the commit would be, and rolls back.
//!
//! A write whose [`WriteOptions`](crate::guard::WriteOptions) carry
//! [`dry_run`](crate::guard::WriteOptions::dry_run) ends with
//! [`Error::DryRun`] holding a [`Preview`] where it would have committed. Every write path
//! of the store stops there, so nothing that follows a commit runs. [`catch`] turns that
//! result back into the preview. See `docs/specs/C15-write-previews.md`.

use crate::commit::{CommitInfo, CommitKind};
use crate::error::{Error, Result};
use crate::guard::{GuardStatus, Rejection, ValidationSummary};
use crate::store::DiffOp;
use oxrdf::{GraphName, Quad};
use std::sync::Arc;

/// The most changed quads a preview lists on request (`changes`).
pub const MAX_LISTED_CHANGES: usize = 10_000;

/// What a dry run reports beyond the commit and its counts.
#[derive(Clone, Debug, Default)]
pub struct DryRun {
    /// the changed quads to list (0: counts only)
    pub changes: usize,
    /// list every change (an RDF Patch); fails past `max_changes`
    pub all_changes: bool,
    /// the most changes a full listing, or the state comparison of a bulk write's
    /// listing, may hold (0: no limit): a `rows` budget error beyond it
    pub max_changes: u64,
}

/// The net change of one graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphChange {
    pub graph: GraphName,
    pub inserted: u64,
    pub deleted: u64,
}

/// Whether a write fits the dataset's storage limits.
#[derive(Debug, Default)]
pub struct StorageCheck {
    /// the error the write would get (the quota, the disk reserve, an in-memory limit)
    pub refused: Option<Error>,
    /// the quota (or an in-memory dataset's size limit) in bytes
    pub limit: Option<u64>,
    /// the bytes the dataset takes now (with a quota)
    pub used: Option<u64>,
    /// the bytes the check compared with the limit
    pub projected: Option<u64>,
}

/// How a previewed write would end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// it would create a commit
    Commit,
    /// it would succeed without a net change, and create no commit
    NoChange,
    /// `If-Match` or `If-None-Match` does not hold
    PreconditionFailed,
    /// the write guard would reject it
    Rejected,
    /// the quota, the disk reserve or an in-memory size limit would refuse it
    StorageRefused,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Outcome::Commit => "commit",
            Outcome::NoChange => "no-change",
            Outcome::PreconditionFailed => "precondition-failed",
            Outcome::Rejected => "rejected",
            Outcome::StorageRefused => "storage-refused",
        }
    }
}

/// What a write would do: the report of a dry run.
#[derive(Debug)]
pub struct Preview {
    pub dataset_id: uuid::Uuid,
    /// the head the write ran against (unchanged)
    pub head: CommitInfo,
    /// The commit the write would create (`None`: no net change). Its timestamp is the
    /// one it would get now; the real commit's may be later.
    pub commit: Option<CommitInfo>,
    /// the kind of the commit
    pub kind: CommitKind,
    /// the message the commit would carry
    pub message: Option<Arc<str>>,
    /// the graphs the write changes, the default graph first, then by name
    pub graphs: Vec<GraphChange>,
    /// the listed changes, removals first
    pub changes: Vec<(DiffOp, Quad)>,
    /// the changed quads in all (`None`: not counted, as no listing was asked for)
    pub changes_total: Option<u64>,
    /// what the write guard found; its status is `Rejected` when it would reject
    pub validation: Option<Arc<ValidationSummary>>,
    /// the write's precondition: `None` without one, else whether it holds
    pub precondition: Option<std::result::Result<(), String>>,
    pub storage: StorageCheck,
}

impl Preview {
    /// How the write would end: the first refusal it would meet, in the order the write
    /// checks them, else whether it would commit.
    pub fn outcome(&self) -> Outcome {
        if matches!(self.precondition, Some(Err(_))) {
            return Outcome::PreconditionFailed;
        }
        let Some(c) = &self.commit else {
            return Outcome::NoChange;
        };
        let rejected = self.rejected();
        let refused = self.storage.refused.is_some();
        // a write through the WAL validates before it checks the storage; a bulk write
        // measures the generation it built before validating it
        match (c.bulk, rejected, refused) {
            (false, true, _) | (true, true, false) => Outcome::Rejected,
            (_, _, true) => Outcome::StorageRefused,
            _ => Outcome::Commit,
        }
    }

    /// The guard would reject the write.
    pub fn rejected(&self) -> bool {
        self.validation
            .as_ref()
            .is_some_and(|v| v.status == GuardStatus::Rejected)
    }

    /// The rejection the write would get from its guard.
    pub fn rejection(&self) -> Option<Rejection> {
        self.validation
            .as_ref()
            .filter(|v| v.status == GuardStatus::Rejected)
            .map(|v| Rejection {
                summary: (**v).clone(),
                head: self.head.seq,
                kind: self.kind,
            })
    }

    /// The message of the error the write would get, when it would be refused.
    pub fn error(&self) -> Option<String> {
        match self.outcome() {
            Outcome::PreconditionFailed => match &self.precondition {
                Some(Err(m)) => Some(m.clone()),
                _ => None,
            },
            Outcome::Rejected => self.rejection().map(|r| r.to_string()),
            Outcome::StorageRefused => self.storage.refused.as_ref().map(|e| e.to_string()),
            Outcome::Commit | Outcome::NoChange => None,
        }
    }

    /// The commit a receipt of the write would name: the new commit, or the head.
    pub fn receipt_commit(&self) -> &CommitInfo {
        self.commit.as_ref().unwrap_or(&self.head)
    }
}

/// The preview a dry run ended with: `Err(Error::DryRun(p))` becomes `Ok(p)`. A write
/// that returned normally was not a dry run, which is an error here.
pub fn catch<T>(r: Result<T>) -> Result<Preview> {
    match r {
        Err(Error::DryRun(p)) => Ok(*p),
        Err(e) => Err(e),
        Ok(_) => Err(Error::invalid("the write was not a dry run")),
    }
}
