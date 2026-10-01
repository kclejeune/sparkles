//! The geometric operations behind the `geof:` functions, over parsed geometries.

pub mod accessors;
pub mod aeqd;
pub mod construct;
pub mod distance;
pub mod measure;
pub mod overlay;
pub mod relate;

/// Why an operation has no result. Both are SPARQL type errors for the function call.
#[derive(Clone, Debug, PartialEq)]
pub enum OpError {
    /// the arguments do not fit the operation (types, CRSs, units, …)
    Type(String),
    /// the inputs have more vertices than `maxOpVertices` allows
    TooLarge(u64),
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpError::Type(m) => f.write_str(m),
            OpError::TooLarge(n) => write!(f, "geometry operation too large ({n} vertices)"),
        }
    }
}

impl std::error::Error for OpError {}

/// The answer of an operation that is not implemented yet.
pub(crate) fn not_yet() -> OpError {
    OpError::Type("not supported yet".into())
}
