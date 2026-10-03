//! The client's error type.

use std::fmt;
use std::time::Duration;

/// Everything a call can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server answered with a status other than 2xx (or 304 where that is expected).
    #[error("{0}")]
    Status(Box<StatusError>),
    /// The request could not be sent or the response could not be read.
    #[error("cannot reach {url}: {source}")]
    Transport {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    /// The call's deadline passed.
    #[error("the call did not finish within {0:?}")]
    Deadline(Duration),
    /// The call's cancellation token was cancelled.
    #[error("the call was cancelled")]
    Cancelled,
    /// The response body is not valid in the format it claims.
    #[error("malformed {format} response: {message}")]
    Parse { format: String, message: String },
    /// The response has a media type the client cannot read.
    #[error("unsupported response media type '{0}'")]
    MediaType(String),
    /// A query returned another form of result than the method expects, such as a
    /// boolean for `select`.
    #[error("expected {expected} but the server returned {got}")]
    UnexpectedResults {
        expected: &'static str,
        got: &'static str,
    },
    /// The client was set up or called in a way that cannot work, such as a Sparkles-only
    /// option on a plain SPARQL endpoint.
    #[error("{0}")]
    Config(String),
    /// A local file could not be read.
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    /// The HTTP status of a [`Error::Status`].
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Status(s) => Some(s.status),
            _ => None,
        }
    }

    /// The server's machine-readable `code` (such as `precondition-failed`), if any.
    pub fn code(&self) -> Option<&str> {
        match self {
            Error::Status(s) => s.code.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn config(msg: impl Into<String>) -> Error {
        Error::Config(msg.into())
    }

    pub(crate) fn parse(format: impl fmt::Display, e: impl fmt::Display) -> Error {
        Error::Parse {
            format: format.to_string(),
            message: e.to_string(),
        }
    }
}

/// A non-2xx response, with the members of the server's error body
/// (`{"error", "code", "detail", "line", "column", "requestId"}`).
#[derive(Debug, Clone)]
pub struct StatusError {
    pub status: u16,
    /// The method and URL of the request.
    pub method: String,
    pub url: String,
    /// The server's message: `error` of a JSON body, else the start of the body.
    pub message: String,
    pub code: Option<String>,
    pub detail: Option<String>,
    pub line: Option<u64>,
    pub column: Option<u64>,
    /// `requestId` of the body, else the `X-Request-Id` header.
    pub request_id: Option<String>,
    /// The parsed JSON body, `Null` when the body was not JSON.
    pub body: serde_json::Value,
    /// The server's `Retry-After`, when it sent one.
    pub retry_after: Option<Duration>,
}

impl fmt::Display for StatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}: {}", self.status, self.url, self.message)?;
        if let Some(c) = &self.code {
            write!(f, " ({c})")?;
        }
        if let (Some(l), Some(c)) = (self.line, self.column) {
            write!(f, " at line {l}, column {c}")?;
        }
        Ok(())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
