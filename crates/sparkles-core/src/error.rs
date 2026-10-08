use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// An exclusive database or catalog lock is held by another opener.
    #[error("{}", locked_message(path, *pid))]
    Locked {
        path: std::path::PathBuf,
        pid: Option<u32>,
    },
    #[error("SPARQL syntax error: {0}")]
    SparqlSyntax(#[from] spargebra::SparqlSyntaxError),
    #[error("RDF parse error: {0}")]
    RdfParse(String),
    #[error("query timed out")]
    Timeout,
    #[error("query cancelled")]
    Cancelled,
    /// A per-request budget (intermediate rows, estimated memory, response size) was
    /// exceeded.
    #[error("{0}")]
    BudgetExceeded(Budget),
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Invalid(String),
    #[error("corrupt index: {0}")]
    Corrupt(String),
    #[error("SERVICE error: {0}")]
    Service(String),
    /// A write-ahead log or generation write failed after a commit started; later writes
    /// are refused until the store is reopened (reads continue).
    #[error("write-ahead log failed; restart the server to recover")]
    Poisoned,
    /// The dataset's full-text index is not at the queried snapshot (stale or rebuilding).
    #[error("{0}")]
    TextUnavailable(String),
    /// A commit, snapshot or other named thing that does not exist.
    #[error("{0}")]
    NotFound(String),
    /// A commit that existed but whose data is no longer kept.
    #[error("{0}")]
    HistoryGone(Box<crate::history::HistoryGone>),
    /// A past-state read that this dataset or feature cannot serve (e.g. in memory).
    #[error("{0}")]
    HistoryUnsupported(String),
    /// A request that conflicts with the current state (e.g. a name already in use).
    #[error("{0}")]
    Conflict(String),
    /// A write guard (write-time validation) rejected the commit; nothing was written.
    #[error("{0}")]
    Rejected(Box<crate::guard::Rejection>),
    /// The dataset requires a write guard that this process has not installed.
    #[error("{0}")]
    GuardMissing(String),
    /// The caller lacks a permission the operation needs (outbound SERVICE or LOAD,
    /// `LOAD <file:…>`); raised before any connection or file is opened.
    #[error("{0}")]
    NotPermitted(String),
    /// A commit refused before anything was written: it would leave less free disk space
    /// than the store keeps, or grow an in-memory store past its size limit.
    #[error("{0}")]
    StorageFull(String),
    /// A write's precondition ([`WriteOptions::precondition`](crate::guard::WriteOptions))
    /// did not hold; nothing was written.
    #[error("{0}")]
    PreconditionFailed(String),
    /// A write asked not to wait ([`WriteOptions::no_wait`](crate::guard::WriteOptions))
    /// found the writer lock taken; nothing was written.
    #[error("another write is in progress")]
    WriterBusy,
    /// A dry run ([`WriteOptions::dry_run`](crate::guard::WriteOptions)) stopped where
    /// the write would have committed, with what the commit would be; nothing was
    /// written. [`preview::catch`](crate::preview::catch) turns it into an `Ok`.
    #[error("dry run: nothing was committed")]
    DryRun(Box<crate::preview::Preview>),
    /// An RDF Patch that cannot be read or applied: a syntax error, a term the store
    /// cannot hold, or a `prev` header that names a commit other than the head. Nothing
    /// was written.
    #[error("{0}")]
    Patch(Box<crate::patch::PatchError>),
    /// A branch or merge request that failed (see [`crate::branch::BranchError`] for
    /// its code and status); nothing was written.
    #[error("{0}")]
    Branch(Box<crate::branch::BranchError>),
    /// An error of a component outside the engine (a backup repository, the reasoner,
    /// the GraphQL adapter), with the component's own stable code. Its `source` is the
    /// component's error, which a program can downcast for the details.
    #[error("{0}")]
    Component(Box<ComponentError>),
}

/// The error of a component outside the engine (see [`Error::Component`]).
#[derive(Debug)]
pub struct ComponentError {
    /// `backup`, `reasoner`, `graphql`, …
    pub component: &'static str,
    /// the component's stable kebab-case code, such as `repository-locked`
    pub code: String,
    pub message: String,
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ComponentError {
    pub fn new(
        component: &'static str,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> ComponentError {
        ComponentError {
            component,
            code: code.into(),
            message: message.into(),
            source: None,
        }
    }

    /// The same error with the component's original error as its source.
    pub fn with_source(mut self, e: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(e));
        self
    }
}

impl std::fmt::Display for ComponentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ComponentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

/// Which budget a request exceeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BudgetKind {
    /// rows of one intermediate result
    Rows,
    /// estimated bytes of the intermediate results alive at once
    Memory,
    /// bytes of the serialized response
    ResultBytes,
    /// bytes of a decompressed input (a compressed request body or upload)
    DecompressedBytes,
    /// bytes received by the outbound calls (SERVICE, `LOAD <http…>`) of one SPARQL
    /// request, summed over its calls
    OutboundBytes,
    /// the work of one validation: the partitions tried to match a node's neighbourhood
    /// to a shape, or the (node, shape) pairs of a ShEx typing
    ValidationWork,
    /// rows produced by all the operators of one query (or of one update's WHERE
    /// clauses), summed
    RowsProduced,
    /// on-disk bytes of a persistent dataset (its storage quota)
    DatasetBytes,
    /// quads a caller's triple-level access rules hide at one commit
    HiddenQuads,
}

impl BudgetKind {
    pub const ALL: [BudgetKind; 9] = [
        BudgetKind::Rows,
        BudgetKind::Memory,
        BudgetKind::ResultBytes,
        BudgetKind::DecompressedBytes,
        BudgetKind::OutboundBytes,
        BudgetKind::ValidationWork,
        BudgetKind::RowsProduced,
        BudgetKind::DatasetBytes,
        BudgetKind::HiddenQuads,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            BudgetKind::Rows => "rows",
            BudgetKind::Memory => "memory",
            BudgetKind::ResultBytes => "result-bytes",
            BudgetKind::DecompressedBytes => "decompressed-bytes",
            BudgetKind::OutboundBytes => "outbound-bytes",
            BudgetKind::ValidationWork => "validation-work",
            BudgetKind::RowsProduced => "rows-produced",
            BudgetKind::DatasetBytes => "dataset-bytes",
            BudgetKind::HiddenQuads => "hidden-quads",
        }
    }
}

/// An exceeded budget: its limit and what the request needed (rows or bytes, by kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub kind: BudgetKind,
    pub limit: u64,
    pub requested: u64,
}

impl std::fmt::Display for Budget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            BudgetKind::Rows => write!(
                f,
                "intermediate result of {} rows exceeds the limit of {}",
                self.requested, self.limit
            ),
            BudgetKind::Memory => write!(
                f,
                "query exceeds its memory budget: needs about {}, limit {}",
                human_bytes(self.requested),
                human_bytes(self.limit)
            ),
            BudgetKind::ResultBytes => write!(
                f,
                "response exceeds the result size budget of {}",
                human_bytes(self.limit)
            ),
            BudgetKind::DecompressedBytes => write!(
                f,
                "decompressed input exceeds the limit of {}",
                human_bytes(self.limit)
            ),
            BudgetKind::OutboundBytes => write!(
                f,
                "the outbound requests (SERVICE, LOAD) of this request exceed their total of {}",
                human_bytes(self.limit)
            ),
            BudgetKind::ValidationWork => write!(
                f,
                "validation exceeds its work budget of {} (partitions of one match, or typing pairs)",
                self.limit
            ),
            BudgetKind::RowsProduced => write!(
                f,
                "query exceeds its work budget: its operators produced {} rows, limit {}",
                self.requested, self.limit
            ),
            BudgetKind::DatasetBytes => write!(
                f,
                "the write would grow the dataset to about {} on disk, over its quota of {}",
                human_bytes(self.requested),
                human_bytes(self.limit)
            ),
            BudgetKind::HiddenQuads => write!(
                f,
                "the access rules of this request hide more than {} quads at this commit",
                self.limit
            ),
        }
    }
}

/// `512 B`, `3.8 KiB`, `1.6 GiB`.
pub fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if b < 1024 {
        return format!("{b} B");
    }
    let mut v = b as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// A stable kebab-case name of the error, the same in every binding: `cancelled`,
    /// `not-found`, `conflict`, `guard-missing` and so on, and a component error's own
    /// code. The server maps codes to HTTP statuses and the bindings to their exception
    /// classes.
    pub fn code(&self) -> &str {
        match self {
            Error::Io(_) => "io",
            Error::Locked { .. } => "locked",
            Error::SparqlSyntax(_) => "sparql-syntax",
            Error::RdfParse(_) => "rdf-parse",
            Error::Timeout => "timeout",
            Error::Cancelled => "cancelled",
            Error::BudgetExceeded(_) => "budget-exceeded",
            Error::Unsupported(_) => "unsupported",
            Error::Invalid(_) => "invalid",
            Error::Corrupt(_) => "corrupt",
            Error::Service(_) => "service",
            Error::Poisoned => "poisoned",
            Error::TextUnavailable(_) => "text-unavailable",
            Error::NotFound(_) => "not-found",
            Error::HistoryGone(_) => "history-gone",
            Error::HistoryUnsupported(_) => "history-unsupported",
            Error::Conflict(_) => "conflict",
            Error::Rejected(_) => "rejected",
            Error::GuardMissing(_) => "guard-missing",
            Error::NotPermitted(_) => "not-permitted",
            Error::StorageFull(_) => "storage-full",
            Error::PreconditionFailed(_) => "precondition-failed",
            Error::WriterBusy => "writer-busy",
            Error::DryRun(_) => "dry-run",
            Error::Patch(_) => "patch",
            Error::Component(c) => &c.code,
            Error::Branch(b) => b.code,
        }
    }

    pub fn invalid(s: impl Into<String>) -> Error {
        Error::Invalid(s.into())
    }
    pub fn unsupported(s: impl Into<String>) -> Error {
        Error::Unsupported(s.into())
    }
}

/// The message of [`Error::Locked`], which names the holder when the lock file records it.
fn locked_message(path: &std::path::Path, pid: Option<u32>) -> String {
    let path = path.display();
    match pid {
        Some(pid) if pid == std::process::id() => format!(
            "directory {path} is already open in this process (pid {pid}); reuse the open handle"
        ),
        Some(pid) => format!(
            "directory {path} is in use by another process (pid {pid}); stop it or talk to it over HTTP"
        ),
        None => {
            format!("directory {path} is in use by another opener; stop it or talk to it over HTTP")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_names_its_holder() {
        let locked = |pid| {
            Error::Locked {
                path: "/data".into(),
                pid,
            }
            .to_string()
        };
        assert_eq!(
            locked(Some(1)),
            "directory /data is in use by another process (pid 1); stop it or talk to it over HTTP"
        );
        let own = locked(Some(std::process::id()));
        assert!(own.contains("already open in this process"), "{own}");
        assert!(!own.contains("Some("), "{own}");
        let unknown = locked(None);
        assert!(
            unknown.contains("another opener") && !unknown.contains("None"),
            "{unknown}"
        );
    }
}
