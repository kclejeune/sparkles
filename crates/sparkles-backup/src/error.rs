//! Errors of backup operations: a stable machine-readable code (the `code` of the HTTP
//! error body `{error, code}`), its HTTP status, and a message for people.

use serde_json::Value as J;

/// Every error code of the backup API. [`Code::as_str`] is the wire form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    /// a repository, backup or policy name outside its grammar (400)
    InvalidName,
    /// a repository or policy configuration that does not validate (400)
    InvalidConfig,
    /// a malformed request body or parameter (400)
    InvalidRequest,
    /// a policy schedule or time zone that does not parse (400)
    InvalidSchedule,
    /// the server is `--read-only` (403)
    ServerReadOnly,
    NoSuchRepository,
    NoSuchBackup,
    NoSuchPolicy,
    /// a second name for one repository id, or a name taken by the API or config (409)
    RepositoryExists,
    PolicyExists,
    /// a non-empty location without a repository marker (409)
    NotARepository,
    /// `PUT /$/repositories/{repo}` changed the type, path, bucket, prefix or endpoint
    LocationImmutable,
    /// `DELETE /$/repositories/{repo}` while a task or policy uses it (409)
    RepositoryInUse,
    /// a change through the API to a repository or policy of the config file (409)
    ReadOnlyConfig,
    BackupExists,
    /// another backup of the same dataset to the same repository runs (409, `task`)
    BackupInProgress,
    /// a restore or verify of this backup runs on this server (409)
    BackupBusy,
    /// a write to a `readonly` repository (409)
    RepositoryReadOnly,
    /// a non-stale conflicting lock outlived `lockWait` (409, `holder`)
    RepositoryLocked,
    DatasetExists,
    /// an in-place restore could not drain requests to the dataset in time (409)
    DatasetBusy,
    /// an in-place restore of a `--loc`-attached dataset (409)
    NotManaged,
    /// `identity: "keep"` with the dataset id in use, or re-issuing commits in place (409)
    DuplicateDatasetId,
    /// a repository marker of a newer format, or with encryption (422)
    IncompatibleRepository,
    /// a backup whose `indexFormat` this build cannot read (422)
    IncompatibleFormat,
    /// a manifest that fails validation (422, names the field)
    InvalidBackup,
    /// not enough free disk space for a restore (507)
    InsufficientStorage,
    /// e.g. a backup of an in-memory dataset (501)
    BackupUnsupported,
    /// an operation this build does not implement yet (501)
    NotImplemented,
    /// a storage backend error after retries (502; the message has no URL query strings)
    RepositoryUnavailable,
    /// the commit catalog could not be flushed at capture (503, retryable)
    CatalogLagging,
    /// the operation was cancelled (a task ends `cancelled`)
    Cancelled,
    /// a restored store whose head or quad count differs from the manifest (500)
    RestoreMismatch,
    /// anything else (500)
    Internal,
}

impl Code {
    pub const ALL: [Code; 34] = [
        Code::InvalidName,
        Code::InvalidConfig,
        Code::InvalidRequest,
        Code::InvalidSchedule,
        Code::ServerReadOnly,
        Code::NoSuchRepository,
        Code::NoSuchBackup,
        Code::NoSuchPolicy,
        Code::RepositoryExists,
        Code::PolicyExists,
        Code::NotARepository,
        Code::LocationImmutable,
        Code::RepositoryInUse,
        Code::ReadOnlyConfig,
        Code::BackupExists,
        Code::BackupInProgress,
        Code::BackupBusy,
        Code::RepositoryReadOnly,
        Code::RepositoryLocked,
        Code::DatasetExists,
        Code::DatasetBusy,
        Code::NotManaged,
        Code::DuplicateDatasetId,
        Code::IncompatibleRepository,
        Code::IncompatibleFormat,
        Code::InvalidBackup,
        Code::InsufficientStorage,
        Code::BackupUnsupported,
        Code::NotImplemented,
        Code::RepositoryUnavailable,
        Code::CatalogLagging,
        Code::Cancelled,
        Code::RestoreMismatch,
        Code::Internal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Code::InvalidName => "invalid-name",
            Code::InvalidConfig => "invalid-config",
            Code::InvalidRequest => "invalid-request",
            Code::InvalidSchedule => "invalid-schedule",
            Code::ServerReadOnly => "server-read-only",
            Code::NoSuchRepository => "no-such-repository",
            Code::NoSuchBackup => "no-such-backup",
            Code::NoSuchPolicy => "no-such-policy",
            Code::RepositoryExists => "repository-exists",
            Code::PolicyExists => "policy-exists",
            Code::NotARepository => "not-a-repository",
            Code::LocationImmutable => "location-immutable",
            Code::RepositoryInUse => "repository-in-use",
            Code::ReadOnlyConfig => "read-only-config",
            Code::BackupExists => "backup-exists",
            Code::BackupInProgress => "backup-in-progress",
            Code::BackupBusy => "backup-busy",
            Code::RepositoryReadOnly => "repository-read-only",
            Code::RepositoryLocked => "repository-locked",
            Code::DatasetExists => "dataset-exists",
            Code::DatasetBusy => "dataset-busy",
            Code::NotManaged => "not-managed",
            Code::DuplicateDatasetId => "duplicate-dataset-id",
            Code::IncompatibleRepository => "incompatible-repository",
            Code::IncompatibleFormat => "incompatible-format",
            Code::InvalidBackup => "invalid-backup",
            Code::InsufficientStorage => "insufficient-storage",
            Code::BackupUnsupported => "backup-unsupported",
            Code::NotImplemented => "not-implemented",
            Code::RepositoryUnavailable => "repository-unavailable",
            Code::CatalogLagging => "catalog-lagging",
            Code::Cancelled => "cancelled",
            Code::RestoreMismatch => "restore-mismatch",
            Code::Internal => "internal",
        }
    }

    /// The HTTP status of a response with this code.
    pub fn http_status(self) -> u16 {
        match self {
            Code::InvalidName | Code::InvalidConfig | Code::InvalidRequest => 400,
            Code::InvalidSchedule => 400,
            Code::ServerReadOnly => 403,
            Code::NoSuchRepository | Code::NoSuchBackup | Code::NoSuchPolicy => 404,
            Code::RepositoryExists
            | Code::PolicyExists
            | Code::NotARepository
            | Code::LocationImmutable
            | Code::RepositoryInUse
            | Code::ReadOnlyConfig
            | Code::BackupExists
            | Code::BackupInProgress
            | Code::BackupBusy
            | Code::RepositoryReadOnly
            | Code::RepositoryLocked
            | Code::DatasetExists
            | Code::DatasetBusy
            | Code::NotManaged
            | Code::DuplicateDatasetId => 409,
            Code::IncompatibleRepository | Code::IncompatibleFormat | Code::InvalidBackup => 422,
            Code::InsufficientStorage => 507,
            Code::BackupUnsupported | Code::NotImplemented => 501,
            Code::RepositoryUnavailable => 502,
            // like the engine's `Cancelled` (a query cancelled by shutdown)
            Code::CatalogLagging | Code::Cancelled => 503,
            Code::RestoreMismatch | Code::Internal => 500,
        }
    }

    pub fn parse(s: &str) -> Option<Code> {
        Code::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error of a backup operation.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct BackupError {
    code: Code,
    message: String,
    /// extra fields of the error body (`task` of `backup-in-progress`, `holder` of
    /// `repository-locked`, `field` of `invalid-backup`)
    extra: Option<serde_json::Map<String, J>>,
}

pub type Result<T, E = BackupError> = std::result::Result<T, E>;

impl BackupError {
    pub fn new(code: Code, message: impl Into<String>) -> BackupError {
        BackupError {
            code,
            message: message.into(),
            extra: None,
        }
    }

    /// Add a field to the error body (`{error, code, <key>: value}`).
    pub fn with(mut self, key: &str, value: impl Into<J>) -> BackupError {
        self.extra
            .get_or_insert_with(Default::default)
            .insert(key.to_string(), value.into());
        self
    }

    /// `not-implemented`: the operation `what` is not built yet.
    pub fn unsupported(what: impl std::fmt::Display) -> BackupError {
        BackupError::new(
            Code::NotImplemented,
            format!("{what} is not implemented yet"),
        )
    }

    pub fn cancelled() -> BackupError {
        BackupError::new(Code::Cancelled, "cancelled")
    }

    pub fn internal(message: impl Into<String>) -> BackupError {
        BackupError::new(Code::Internal, message)
    }

    /// `invalid-backup` naming the manifest field at fault.
    pub fn invalid_backup(field: &str, message: impl std::fmt::Display) -> BackupError {
        BackupError::new(
            Code::InvalidBackup,
            format!("invalid backup: {field}: {message}"),
        )
        .with("field", field)
    }

    pub fn code(&self) -> Code {
        self.code
    }

    pub fn http_status(&self) -> u16 {
        self.code.http_status()
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn is_cancelled(&self) -> bool {
        self.code == Code::Cancelled
    }

    /// The HTTP error body: `{error, code, …extra}`.
    pub fn body(&self) -> J {
        let mut m = self.extra.clone().unwrap_or_default();
        m.insert("error".into(), self.message.clone().into());
        m.insert("code".into(), self.code.as_str().into());
        J::Object(m)
    }
}

/// Remove the query strings of URLs in a backend message (signed or presigned
/// parameters must never reach logs or clients).
pub fn redact_urls(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut rest = msg;
    while let Some(i) = rest.find("://") {
        // copy up to the end of the scheme separator, then the URL up to its query
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | '>' | ','))
            .unwrap_or(tail.len());
        let url = &tail[..end];
        match url.find(['?', '#']) {
            Some(q) => out.push_str(&url[..q]),
            None => out.push_str(url),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

impl From<object_store::Error> for BackupError {
    /// Backend errors are `repository-unavailable` (502), without URL query strings.
    /// Callers that expect `NotFound` or `AlreadyExists` match on the
    /// `object_store::Error` before converting.
    fn from(e: object_store::Error) -> BackupError {
        BackupError::new(
            Code::RepositoryUnavailable,
            format!("repository unavailable: {}", redact_urls(&e.to_string())),
        )
    }
}

impl From<sparkles::Error> for BackupError {
    fn from(e: sparkles::Error) -> BackupError {
        use sparkles::Error as E;
        let code = match &e {
            E::Cancelled => Code::Cancelled,
            E::Unsupported(_) => Code::BackupUnsupported,
            E::Conflict(m) if m.starts_with("catalog-lagging") => Code::CatalogLagging,
            E::Invalid(_) => Code::InvalidRequest,
            _ => Code::Internal,
        };
        BackupError::new(code, e.to_string())
    }
}

impl From<std::io::Error> for BackupError {
    fn from(e: std::io::Error) -> BackupError {
        BackupError::new(Code::Internal, format!("I/O error: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_with_statuses() {
        for c in Code::ALL {
            assert_eq!(Code::parse(c.as_str()), Some(c));
            assert!((400..600).contains(&c.http_status()), "{c}");
        }
        assert_eq!(Code::RepositoryLocked.http_status(), 409);
        assert_eq!(Code::InsufficientStorage.http_status(), 507);
        assert_eq!(Code::RepositoryUnavailable.http_status(), 502);
        assert_eq!(Code::InvalidBackup.http_status(), 422);
    }

    #[test]
    fn body_has_code_and_extras() {
        let e = BackupError::new(Code::BackupInProgress, "busy").with("task", "7");
        assert_eq!(
            e.body(),
            serde_json::json!({"error": "busy", "code": "backup-in-progress", "task": "7"})
        );
        let e = BackupError::invalid_backup("files[3].path", "not allowed");
        assert_eq!(e.body()["field"], "files[3].path");
        assert_eq!(e.http_status(), 422);
    }

    #[test]
    fn urls_lose_their_query() {
        assert_eq!(
            redact_urls("GET https://b.s3.amazonaws.com/k?X-Amz-Signature=abc failed"),
            "GET https://b.s3.amazonaws.com/k failed"
        );
        assert_eq!(
            redact_urls("(http://h:9000/a?x=1), and s3://b/p#f"),
            "(http://h:9000/a), and s3://b/p"
        );
        assert_eq!(redact_urls("no url"), "no url");
    }
}
