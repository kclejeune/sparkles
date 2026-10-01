//! Errors of parsing, compiling and validating.

/// A syntax error in a schema, a shape map or externs (ShExC or JSON), with the
/// 1-based line and column (in characters) where it was found.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line}, column {column}: {message}")]
pub struct ParseError {
    pub message: String,
    pub line: usize,
    pub column: usize,
}

impl ParseError {
    pub fn new(message: impl Into<String>, line: usize, column: usize) -> ParseError {
        ParseError {
            message: message.into(),
            line,
            column,
        }
    }
}

/// A schema that parses but cannot be used: an unresolved reference or import, a
/// negated reference cycle, an invalid `&include`, overlapping labels, an EXTERNAL shape
/// without a definition, or a shape-map label the schema does not define.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SchemaError {
    pub message: String,
}

impl SchemaError {
    pub fn new(message: impl Into<String>) -> SchemaError {
        SchemaError {
            message: message.into(),
        }
    }
}

/// A result map that would hold more than [`ValidateOptions::max_results`] results.
///
/// [`ValidateOptions::max_results`]: crate::ValidateOptions::max_results
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the validation report exceeds {limit} results")]
pub struct TooManyResults {
    pub limit: usize,
}

/// The message of a part that is not written yet; callers (the conformance harness)
/// skip what fails with it.
pub(crate) const NOT_IMPLEMENTED: &str = "not implemented";

pub(crate) fn todo(what: &str) -> anyhow::Error {
    anyhow::anyhow!("{what}: {NOT_IMPLEMENTED}")
}
