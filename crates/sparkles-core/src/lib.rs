#[cfg(test)]
pub(crate) mod test_alloc;

pub mod access;
pub mod algo;
pub mod annotations;
pub mod branch;
pub mod builder;
pub mod check;
pub mod codec;
pub mod commit;
pub mod disk;
pub mod duration;
pub mod error;
pub mod evidence;
pub mod explain;
pub mod formats;
pub mod fsutil;
pub mod geo;
pub mod guard;
pub mod history;
pub mod holds;
pub mod id;
pub mod incr;
pub mod index;
pub mod io;
pub mod ioacct;
pub mod jena_formats;
pub mod managed;
pub mod mapping;
pub mod migrate;
pub mod nesting;
pub mod outbound;
pub mod patch;
pub mod patterns;
pub mod preview;
pub mod rank;
pub mod schema;
pub mod sparql;
pub mod spill;
pub mod store;
pub mod stored;
pub mod stored_mappings;
pub mod syntaxes;
pub mod tabular;
pub mod task;
pub mod text;
pub mod threads;
pub mod trix;
pub mod validation;
pub mod vector;
pub mod vocab;
pub mod xsd;

pub use error::{Budget, BudgetKind, Error, Result};

/// The named graph of the reasoner's materialized inferences, which queries read as part
/// of the default graph when they ask for inferred data and merges leave out unless
/// asked to include it. Every crate names it through this constant.
pub const INFERRED_GRAPH: &str = "urn:x-sparkles:inferred";
