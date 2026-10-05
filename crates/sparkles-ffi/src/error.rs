//! The one error type that crosses the boundary. The Kotlin side maps each kind to the
//! Jena exception that Jena raises in the same situation (P04 §4.3).

use crate::encode::Malformed;

/// What went wrong, as the Kotlin side tells the cases apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ErrorKind {
    SparqlSyntax,
    RdfParse,
    Timeout,
    Cancelled,
    BudgetExceeded,
    Unsupported,
    Invalid,
    Conflict,
    WriterBusy,
    PreconditionFailed,
    Rejected,
    GuardMissing,
    NotFound,
    HistoryGone,
    HistoryUnsupported,
    NotPermitted,
    Service,
    Corrupt,
    Poisoned,
    StorageFull,
    Io,
    /// the database directory is locked by another process
    Locked,
    /// a write transaction was aborted by an earlier failure, or has ended
    TransactionEnded,
    /// a batch that does not follow the encoding (a bug in the binding)
    Malformed,
    Other,
}

#[derive(Debug, uniffi::Error)]
pub enum FfiError {
    Engine {
        kind: ErrorKind,
        /// the engine's message (not `message`, which the Kotlin exception has already)
        detail: String,
        /// for `BudgetExceeded`: which budget (`rows`, `memory`, …), its limit and the
        /// amount asked for
        budget: Option<String>,
        limit: Option<u64>,
        requested: Option<u64>,
    },
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let FfiError::Engine { kind, detail, .. } = self;
        write!(f, "{kind:?}: {detail}")
    }
}

impl std::error::Error for FfiError {}

impl FfiError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> FfiError {
        FfiError::Engine {
            kind,
            detail: message.into(),
            budget: None,
            limit: None,
            requested: None,
        }
    }

    pub fn kind(&self) -> ErrorKind {
        let FfiError::Engine { kind, .. } = self;
        *kind
    }
}

impl From<Malformed> for FfiError {
    fn from(m: Malformed) -> FfiError {
        FfiError::new(ErrorKind::Malformed, m.to_string())
    }
}

impl From<sparkles::Error> for FfiError {
    fn from(e: sparkles::Error) -> FfiError {
        use sparkles::Error as E;
        let message = e.to_string();
        let kind = match &e {
            E::Io(_) => ErrorKind::Io,
            E::SparqlSyntax(_) => ErrorKind::SparqlSyntax,
            E::RdfParse(_) => ErrorKind::RdfParse,
            E::Timeout => ErrorKind::Timeout,
            E::Cancelled => ErrorKind::Cancelled,
            E::BudgetExceeded(b) => {
                return FfiError::Engine {
                    kind: ErrorKind::BudgetExceeded,
                    detail: message,
                    budget: Some(b.kind.as_str().to_string()),
                    limit: Some(b.limit),
                    requested: Some(b.requested),
                };
            }
            E::Unsupported(_) => ErrorKind::Unsupported,
            E::Locked { .. } => ErrorKind::Locked,
            E::Invalid(_) => ErrorKind::Invalid,
            E::Corrupt(_) => ErrorKind::Corrupt,
            E::Service(_) => ErrorKind::Service,
            E::Poisoned => ErrorKind::Poisoned,
            E::TextUnavailable(_) => ErrorKind::Unsupported,
            E::NotFound(_) => ErrorKind::NotFound,
            E::HistoryGone(_) => ErrorKind::HistoryGone,
            E::HistoryUnsupported(_) => ErrorKind::HistoryUnsupported,
            E::Conflict(_) => ErrorKind::Conflict,
            E::Rejected(_) => ErrorKind::Rejected,
            E::GuardMissing(_) => ErrorKind::GuardMissing,
            E::NotPermitted(_) => ErrorKind::NotPermitted,
            E::StorageFull(_) => ErrorKind::StorageFull,
            E::PreconditionFailed(_) => ErrorKind::PreconditionFailed,
            E::WriterBusy => ErrorKind::WriterBusy,
            E::Patch(p) => match p.kind {
                sparkles::patch::PatchErrorKind::Syntax => ErrorKind::RdfParse,
                sparkles::patch::PatchErrorKind::Term => ErrorKind::Invalid,
                sparkles::patch::PatchErrorKind::PrevMismatch => ErrorKind::Conflict,
            },
            _ => ErrorKind::Other,
        };
        FfiError::new(kind, message)
    }
}

pub type FfiResult<T> = Result<T, FfiError>;
