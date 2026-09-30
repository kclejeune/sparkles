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
    #[error("memory limit exceeded: {0}")]
    MemoryLimit(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Invalid(String),
    #[error("corrupt index: {0}")]
    Corrupt(String),
    #[error("SERVICE error: {0}")]
    Service(String),
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
