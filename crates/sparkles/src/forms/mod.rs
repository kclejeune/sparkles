//! Editing forms generated from SHACL shapes (spec U02): the form model of a resource,
//! and checks and saves of the changes a form makes.
//!
//! A form lists the fields that the property shapes of the node shapes targeting a
//! resource describe (`sparkles_shacl::form`), with the resource's values read under
//! the caller's view, and the triples no property shape covers as read-only "other
//! properties". A change names a field by its id, the path in SPARQL syntax, with the
//! values to remove and to add. `Forms::check` previews the change as a dry run and
//! validates the nodes it touches against the form's shapes; `Forms::save` commits it
//! as one update when the values the form loaded are still the stored ones.
//!
//! The settings types are read by builds without the `shacl` feature too; the rest needs
//! it.

mod settings;
pub use settings::{FormSettings, SaveTo, ShapesSource};

#[cfg(feature = "shacl")]
mod model;
#[cfg(feature = "shacl")]
pub use model::*;
