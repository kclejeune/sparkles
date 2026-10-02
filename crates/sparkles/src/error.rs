use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
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
}

impl BudgetKind {
    pub const ALL: [BudgetKind; 6] = [
        BudgetKind::Rows,
        BudgetKind::Memory,
        BudgetKind::ResultBytes,
        BudgetKind::DecompressedBytes,
        BudgetKind::OutboundBytes,
        BudgetKind::ValidationWork,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            BudgetKind::Rows => "rows",
            BudgetKind::Memory => "memory",
            BudgetKind::ResultBytes => "result-bytes",
            BudgetKind::DecompressedBytes => "decompressed-bytes",
            BudgetKind::OutboundBytes => "outbound-bytes",
            BudgetKind::ValidationWork => "validation-work",
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
    pub fn invalid(s: impl Into<String>) -> Error {
        Error::Invalid(s.into())
    }
    pub fn unsupported(s: impl Into<String>) -> Error {
        Error::Unsupported(s.into())
    }
}
