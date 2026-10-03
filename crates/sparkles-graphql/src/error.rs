//! GraphQL errors with the codes of the spec (§9): request errors that fail the whole
//! request, and execution errors of one field.

use serde_json::{Map, Value as J, json};

/// The `extensions.code` of an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    ParseFailed,
    ValidationFailed,
    BadUserInput,
    TooComplex,
    PersistedQueryRequired,
    CursorInvalid,
    CursorExpired,
    BudgetExceeded,
    Timeout,
    Cancelled,
    MultipleValues,
    MissingValue,
    InvalidValue,
    UnresolvedType,
    /// A malformed HTTP request (no document, a body that is not JSON).
    BadRequest,
    /// A mutation sent with `GET`.
    MethodNotAllowed,
    Internal,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::ParseFailed => "GRAPHQL_PARSE_FAILED",
            Code::ValidationFailed => "GRAPHQL_VALIDATION_FAILED",
            Code::BadUserInput => "BAD_USER_INPUT",
            Code::TooComplex => "QUERY_TOO_COMPLEX",
            Code::PersistedQueryRequired => "PERSISTED_QUERY_REQUIRED",
            Code::CursorInvalid => "CURSOR_INVALID",
            Code::CursorExpired => "CURSOR_EXPIRED",
            Code::BudgetExceeded => "BUDGET_EXCEEDED",
            Code::Timeout => "TIMEOUT",
            Code::Cancelled => "CANCELLED",
            Code::MultipleValues => "MULTIPLE_VALUES",
            Code::MissingValue => "MISSING_VALUE",
            Code::InvalidValue => "INVALID_VALUE",
            Code::UnresolvedType => "UNRESOLVED_TYPE",
            Code::BadRequest => "BAD_REQUEST",
            Code::MethodNotAllowed => "METHOD_NOT_ALLOWED",
            Code::Internal => "INTERNAL_SERVER_ERROR",
        }
    }
}

/// One error of a response.
#[derive(Clone, Debug)]
pub struct GqlError {
    pub code: Code,
    pub message: String,
    /// `(line, column)`, 1-based
    pub locations: Vec<(usize, usize)>,
    /// response keys and list indexes
    pub path: Vec<J>,
    /// extensions besides `code`
    pub extensions: Map<String, J>,
}

impl GqlError {
    pub fn new(code: Code, message: impl Into<String>) -> GqlError {
        GqlError {
            code,
            message: message.into(),
            locations: Vec::new(),
            path: Vec::new(),
            extensions: Map::new(),
        }
    }

    pub fn with(mut self, k: &str, v: impl Into<J>) -> GqlError {
        self.extensions.insert(k.into(), v.into());
        self
    }

    pub fn at(mut self, loc: Option<(usize, usize)>) -> GqlError {
        self.locations.extend(loc);
        self
    }

    pub fn to_json(&self) -> J {
        let mut o = Map::new();
        o.insert("message".into(), self.message.clone().into());
        if !self.locations.is_empty() {
            o.insert(
                "locations".into(),
                self.locations
                    .iter()
                    .map(|(l, c)| json!({ "line": l, "column": c }))
                    .collect(),
            );
        }
        if !self.path.is_empty() {
            o.insert("path".into(), J::Array(self.path.clone()));
        }
        let mut ext = Map::new();
        ext.insert("code".into(), self.code.as_str().into());
        ext.extend(self.extensions.clone());
        o.insert("extensions".into(), J::Object(ext));
        J::Object(o)
    }

    /// The error of an engine failure that fails a whole request.
    pub fn from_engine(e: sparkles::Error) -> GqlError {
        match e {
            sparkles::Error::BudgetExceeded(b) => {
                GqlError::new(Code::BudgetExceeded, b.to_string())
                    .with("budget", serde_json::to_value(b.kind).unwrap_or_default())
                    .with("limit", b.limit)
                    .with("requested", b.requested)
            }
            sparkles::Error::Timeout => GqlError::new(Code::Timeout, "the request timed out"),
            sparkles::Error::Cancelled => {
                GqlError::new(Code::Cancelled, "the request was cancelled")
            }
            // a value the engine refuses, such as a regex pattern
            sparkles::Error::Invalid(m) => GqlError::new(Code::BadUserInput, m),
            e => GqlError::new(Code::Internal, e.to_string()),
        }
    }
}

/// How a request ended, for the HTTP status (§9.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Executed, with or without execution errors.
    Executed,
    /// A malformed HTTP request.
    BadRequest,
    /// The document does not parse.
    ParseFailed,
    /// The document does not validate, or a limit of §7.1, or a bad variable.
    Invalid,
    /// A mutation sent with `GET`.
    MethodNotAllowed,
    Budget,
    Timeout,
    Cancelled,
    /// An error the server did not expect.
    Internal,
}

impl Outcome {
    /// The outcome of a request error with this code.
    pub fn of(code: Code) -> Outcome {
        match code {
            Code::ParseFailed => Outcome::ParseFailed,
            Code::BadRequest => Outcome::BadRequest,
            Code::MethodNotAllowed => Outcome::MethodNotAllowed,
            Code::BudgetExceeded => Outcome::Budget,
            Code::Timeout => Outcome::Timeout,
            Code::Cancelled => Outcome::Cancelled,
            Code::Internal => Outcome::Internal,
            _ => Outcome::Invalid,
        }
    }

    /// The HTTP status code: `graphql_response` is whether the response is
    /// `application/graphql-response+json` (else `application/json`).
    pub fn status(self, graphql_response: bool) -> u16 {
        match self {
            Outcome::Executed => 200,
            Outcome::BadRequest => 400,
            Outcome::ParseFailed if graphql_response => 400,
            Outcome::Invalid if graphql_response => 422,
            Outcome::ParseFailed | Outcome::Invalid => 200,
            Outcome::MethodNotAllowed => 405,
            Outcome::Budget => 507,
            Outcome::Timeout => 408,
            Outcome::Cancelled => 503,
            Outcome::Internal => 500,
        }
    }
}
