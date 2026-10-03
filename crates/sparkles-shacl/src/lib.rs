//! SHACL validation for Sparkles (Apache Jena `jena-shacl` equivalent).
//!
//! * **SHACL Core**: node and property shapes, all targets (including implicit class
//!   targets), `sh:deactivated`, `sh:severity`, `sh:message`, all property path forms
//!   and every core constraint component.
//! * **SHACL 1.2 Core** list constraints (`sh:memberShape`, `sh:minListLength`,
//!   `sh:maxListLength`, `sh:uniqueMembers`) and `sh:targetWhere`.
//! * **SHACL Compact Syntax** (SHACLC): shapes are read from it and written to it
//!   ([`compact`], [`ShapesSyntax`]).
//! * **SHACL-SPARQL**: `sh:sparql` SELECT constraints and SPARQL-based constraint
//!   components (ASK / SELECT validators), with pre-binding of `$this`, `$value`,
//!   `$currentShape`, `$shapesGraph` and parameters.
//!
//! The data graph is read directly from a store [`Snapshot`](sparkles_core::store::Snapshot)
//! through index scans; nothing is copied into memory.
//!
//! ```no_run
//! # use sparkles_core::store::{Store, StoreOptions};
//! # use sparkles_core::io::RdfFormat;
//! # fn main() -> anyhow::Result<()> {
//! let store = Store::in_memory(StoreOptions::default());
//! let shapes = sparkles_shacl::Shapes::parse(
//!     "@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
//!      ex:S a sh:NodeShape ; sh:targetClass ex:Person ;
//!          sh:property [ sh:path ex:name ; sh:minCount 1 ] .",
//!     RdfFormat::Turtle,
//!     None,
//! )?;
//! let report = sparkles_shacl::validate(&store.snapshot(), &shapes, &Default::default())?;
//! println!("{}", report.to_turtle());
//! # Ok(()) }
//! ```

pub mod compact;
pub mod constraints;
mod data;
pub mod guard;
pub mod incremental;
mod localize;
pub mod path;
pub mod report;
pub mod shapes;
pub mod sparql;
pub mod syntax;
mod validate;
pub mod vocab;

pub use path::PropertyPath;
pub use report::{ValidationReport, ValidationResult};
pub use shapes::{Constraint, NodeKind, Shape, Shapes, Target};
pub use sparkles_core::io::RdfFormat;
pub use syntax::ShapesSyntax;
pub use validate::{TooManyResults, ValidateOptions, validate, validate_node};

/// Lexical validity of an XSD literal (as used by `sh:datatype`; see [`sparkles_core::xsd`]).
pub use sparkles_core::xsd::is_valid as is_valid_literal;
