//! The ShExC pretty printer: prefixed names from the schema's prefixes, annotations and
//! semantic actions kept.

use crate::ast::Schema;
use crate::error::NOT_IMPLEMENTED;

/// Write a schema as ShExC (see [`Schema::to_shexc`]).
pub fn write(schema: &Schema) -> String {
    let _ = schema;
    unimplemented!("ShExC writer: {NOT_IMPLEMENTED}")
}
