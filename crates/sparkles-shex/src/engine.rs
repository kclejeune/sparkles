//! Validation: expand the shape map, discover the typing graph, refine it stratum by
//! stratum, and explain the nonconformant results.

use crate::error::todo;
use crate::{CompiledSchema, ResultMap, ShapeLabel, ShapeMap, ShapeResult, ValidateOptions};
use oxrdf::Term;
use sparkles::store::Snapshot;
use std::sync::Arc;

/// See [`crate::validate`].
pub fn validate(
    snap: &Arc<Snapshot>,
    schema: &CompiledSchema,
    map: &ShapeMap,
    opts: &ValidateOptions,
) -> anyhow::Result<ResultMap> {
    let _ = (snap, schema, map, opts);
    Err(todo("validation"))
}

/// See [`crate::validate_node`].
pub fn validate_node(
    snap: &Arc<Snapshot>,
    schema: &CompiledSchema,
    node: &Term,
    shape: &ShapeLabel,
    opts: &ValidateOptions,
) -> anyhow::Result<ShapeResult> {
    let _ = (snap, schema, node, shape, opts);
    Err(todo("validation"))
}
