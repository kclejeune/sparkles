//! Compilation of a checked schema into the [`crate::ir`]: references resolved and
//! chains collapsed, `&include` expanded per occurrence, triple expressions flattened
//! per shape with their per-(predicate, direction) index and maximum occurrences, shapes
//! classified (flat, deterministic, ambiguous), and pair kinds for labels, START and
//! value expressions.

use crate::CompiledSchema;
use crate::ast::Schema;
use crate::check::Checked;
use crate::error::{SchemaError, schema_todo};

/// Compile a checked schema.
pub fn compile(schema: &Schema, checked: &Checked) -> Result<CompiledSchema, SchemaError> {
    let _ = (schema, checked);
    Err(schema_todo("schema compilation"))
}
