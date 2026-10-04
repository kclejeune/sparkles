//! The Sparkles library. The engine lives in `sparkles-core`, and this crate re-exports
//! its modules under their own names, so `sparkles::store::Store` and
//! `sparkles::sparql::QueryOptions` are the engine's types. The embedded [`Dataset`] API
//! and the [`querybuilder`] are defined here.

pub use sparkles_core::{
    access, annotations, branch, builder, check, codec, commit, disk, error, geo, guard, history,
    id, index, io, jena_formats, nesting, outbound, patch, preview, schema, sparql, store, stored,
    tabular, text, trix, validation, vector, vocab, xsd,
};

pub mod dataset;
pub mod querybuilder;

pub use dataset::{Dataset, GraphView, QuadIter, Solution, Solutions, Transaction};
pub use sparkles_core::{Budget, BudgetKind, Error, Result};
