//! Update requests: operations separated by `;` and a blank line, `WITH`/`USING`
//! lines, `DELETE {`/`INSERT {`/`WHERE {` blocks, quad data, one-line operations.

use super::Ctx;
use crate::doc::DocId;
use crate::tree::NodeId;

/// `UpdateUnit`.
///
/// TODO: the layout (stub: as written).
pub fn update_unit(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `LoadOp`.
///
/// TODO: the layout (stub: as written).
pub fn load_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `ClearOp`.
///
/// TODO: the layout (stub: as written).
pub fn clear_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `DropOp`.
///
/// TODO: the layout (stub: as written).
pub fn drop_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `CreateOp`.
///
/// TODO: the layout (stub: as written).
pub fn create_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `AddOp`.
///
/// TODO: the layout (stub: as written).
pub fn add_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `MoveOp`.
///
/// TODO: the layout (stub: as written).
pub fn move_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `CopyOp`.
///
/// TODO: the layout (stub: as written).
pub fn copy_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `InsertDataOp`.
///
/// TODO: the layout (stub: as written).
pub fn insert_data_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `DeleteDataOp`.
///
/// TODO: the layout (stub: as written).
pub fn delete_data_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `DeleteWhereOp`.
///
/// TODO: the layout (stub: as written).
pub fn delete_where_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `ModifyOp`.
///
/// TODO: the layout (stub: as written).
pub fn modify_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `WithClause`.
///
/// TODO: the layout (stub: as written).
pub fn with_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `DeleteClause`.
///
/// TODO: the layout (stub: as written).
pub fn delete_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `InsertClause`.
///
/// TODO: the layout (stub: as written).
pub fn insert_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `UsingClause`.
///
/// TODO: the layout (stub: as written).
pub fn using_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `QuadPattern`.
///
/// TODO: the layout (stub: as written).
pub fn quad_pattern(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `QuadsGraph`.
///
/// TODO: the layout (stub: as written).
pub fn quads_graph(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}
