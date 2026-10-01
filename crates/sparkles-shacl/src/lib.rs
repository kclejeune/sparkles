//! SHACL validation for Sparkles (Apache Jena `jena-shacl` equivalent).
//!
//! * **SHACL Core**: node and property shapes, all targets (including implicit class
//!   targets), `sh:deactivated`, `sh:severity`, `sh:message`, all property path forms
//!   and every core constraint component.
//! * **SHACL-SPARQL**: `sh:sparql` SELECT constraints and SPARQL-based constraint
//!   components (ASK / SELECT validators), with pre-binding of `$this`, `$value`,
//!   `$currentShape`, `$shapesGraph` and parameters.
//!
//! The data graph is read directly from a store [`Snapshot`](sparkles::store::Snapshot)
//! through index scans; nothing is copied into memory.
//!
//! ```no_run
//! # use sparkles::store::{Store, StoreOptions};
//! # use sparkles::io::RdfFormat;
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

mod data;
pub mod guard;
pub mod path;
pub mod report;
pub mod shapes;
pub mod sparql;
mod validate;
pub mod vocab;

pub use path::PropertyPath;
pub use report::{ValidationReport, ValidationResult};
pub use shapes::{Constraint, NodeKind, Shape, Shapes, Target};
pub use sparkles::io::RdfFormat;
pub use validate::{TooManyResults, ValidateOptions, validate, validate_node};

/// Lexical validity of an XSD literal (as used by `sh:datatype`; see [`sparkles::xsd`]).
pub use sparkles::xsd::is_valid as is_valid_literal;
