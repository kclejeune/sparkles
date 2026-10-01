//! `VALUES`: one variable inline when it fits, one row per line otherwise, rows
//! `(v₁ v₂)` (aligned with `align-values`).

use super::Ctx;
use crate::doc::DocId;
use crate::tree::NodeId;

/// `ValuesClause`.
///
/// TODO: the layout (stub: as written).
pub fn values_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `InlineValues`.
///
/// TODO: the layout (stub: as written).
pub fn inline_values(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `ValuesRow`.
///
/// TODO: the layout (stub: as written).
pub fn values_row(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `DataValue`.
///
/// TODO: the layout (stub: as written).
pub fn data_value(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}
