//! Structural checks of a closed schema: references and inclusions resolve, no label
//! reaches itself without passing through a shape, `&include` names a triple
//! expression, no negated reference cycle (through NOT or EXTRA), unique labels, and the
//! facet rules; plus the strata of the label dependency graph.

use crate::ast::{NodeConstraint, Schema};
use crate::error::{SchemaError, schema_todo};

/// What the checks found that compilation needs.
#[derive(Clone, Debug, Default)]
pub struct Checked {
    /// the stratum of each declaration of [`Schema::shapes`], in order
    pub strata: Vec<u32>,
}

/// Check a closed schema (imports merged, see [`crate::resolve::close`]).
pub fn check(schema: &Schema) -> Result<Checked, SchemaError> {
    let _ = schema;
    Err(schema_todo("schema checks"))
}

/// The facet rules of a node constraint (both syntaxes): no two string-length facets of
/// the same kind, no numeric facet on a non-numeric or unknown datatype, numeric bounds
/// that are numbers. The error is the message.
pub fn check_facets(nc: &NodeConstraint) -> Result<(), String> {
    // accepts everything until the rules are written
    let _ = nc;
    Ok(())
}
