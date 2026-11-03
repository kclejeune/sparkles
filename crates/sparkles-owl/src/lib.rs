//! The OWL 2 axiom model of Sparkles (spec F21).
//!
//! [`model`] holds the structural specification as Rust values. [`read`] maps RDF
//! triples to axioms by the reverse mapping of the OWL 2 Mapping to RDF Graphs, in a
//! lenient or a strict mode, and [`write`] maps axioms to triples by the forward
//! mapping. [`profiles`] checks an ontology against OWL 2 EL, QL, RL and the OWL 2 DL
//! restrictions. [`ofn`] writes the functional-style syntax, [`manchester`] reads and
//! writes the Manchester syntax, and [`json`] writes the JSON form of axioms that the
//! HTTP API uses.
//!
//! The crate depends on `oxrdf` and not on the store, so it reads files as well as
//! datasets.
//!
//! ```
//! use sparkles_owl::{read, model::*, profiles::{self, Profile}};
//! use oxrdf::{NamedNode, Triple, BlankNode, Term};
//!
//! let n = |s: &str| NamedNode::new_unchecked(s);
//! let ex = |s: &str| n(&format!("http://ex.org/{s}"));
//! let rdf_type = n(sparkles_owl::vocab::RDF_TYPE);
//! let owl = |s: &str| n(&format!("http://www.w3.org/2002/07/owl#{s}"));
//! let r = BlankNode::new_unchecked("r");
//! let triples = vec![
//!     Triple::new(ex("university"), rdf_type.clone(), owl("Ontology")),
//!     Triple::new(ex("Professor"), rdf_type.clone(), owl("Class")),
//!     Triple::new(ex("Course"), rdf_type.clone(), owl("Class")),
//!     Triple::new(ex("teaches"), rdf_type.clone(), owl("ObjectProperty")),
//!     Triple::new(ex("Professor"), n(sparkles_owl::vocab::RDFS_SUB_CLASS_OF), r.clone()),
//!     Triple::new(r.clone(), rdf_type.clone(), owl("Restriction")),
//!     Triple::new(r.clone(), owl("onProperty"), ex("teaches")),
//!     Triple::new(r.clone(), owl("someValuesFrom"), ex("Course")),
//! ];
//! let o = read::read_graph(triples, read::ReadOptions::default());
//! assert!(o.unmapped.is_empty());
//! let expected = Axiom::sub_class_of(
//!     Class::new("http://ex.org/Professor"),
//!     ClassExpression::some(ObjectProperty::new("http://ex.org/teaches"), Class::new("http://ex.org/Course")),
//! );
//! assert!(o.contains(&expected));
//! let el = &profiles::check(&o, &[Profile::El], Default::default())[0];
//! assert!(el.conforms);
//! ```

pub mod classify;
pub mod datatypes;
pub mod entities;
pub mod json;
pub mod manchester;
pub mod model;
pub mod ofn;
pub mod ofn_read;
pub mod profiles;
pub mod read;
pub mod validate;
pub mod vocab;
pub mod write;

pub use model::*;
pub use read::{Mode, Ontology, ReadOptions, is_terminological};
