//! The Sparkles library. The engine lives in `sparkles-core`, and this crate re-exports
//! its modules under their own names, so `sparkles::store::Store` and
//! `sparkles::sparql::QueryOptions` are the engine's types. The embedded [`Dataset`] API
//! and the [`querybuilder`] are defined here.

pub use sparkles_core::{
    access, algo, annotations, branch, builder, check, codec, commit, disk, error, explain,
    formats, geo, guard, history, id, incr, index, io, ioacct, jena_formats, mapping, migrate,
    nesting, outbound, patch, patterns, preview, schema, sparql, spill, store, stored,
    stored_mappings, tabular, task, text, trix, validation, vector, vocab, xsd,
};

#[cfg(feature = "backup")]
pub mod backup;
mod branches;
pub mod catalog;
pub mod cloning;
pub mod dataset;
pub mod embed;
pub mod evidence;
#[cfg(feature = "fmt")]
pub mod fmt;
pub mod forms;
pub mod handles;
pub mod managed_source;
pub mod overview;
pub mod querybuilder;
pub mod reasoning;
pub mod shared_entity;
pub mod stats;
pub mod syntax;
pub mod terms;
pub mod valid_time;
pub mod write_guard;

pub use catalog::{Catalog, CatalogOptions};
pub use dataset::{Dataset, DatasetOptions, GraphView, QuadIter, Solution, Solutions, Transaction};
pub use sparkles_core::{Budget, BudgetKind, Error, Result};
pub use syntax::RdfSyntax;
