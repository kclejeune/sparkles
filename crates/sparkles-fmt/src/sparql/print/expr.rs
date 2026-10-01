//! Expressions: single spaces around binary operators, tight unary operators and
//! calls, `||`/`&&` chains that break one operand per line with the operator leading
//! or trailing (`operator-position`), argument lists, `IN`, `EXISTS`, N12 (`?v+1` →
//! `?v + 1`).

use super::Ctx;
use crate::doc::DocId;
use crate::tree::NodeId;

/// `OrChain`.
///
/// TODO: the layout (stub: as written).
pub fn or_chain(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `AndChain`.
///
/// TODO: the layout (stub: as written).
pub fn and_chain(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `ChainOperand`.
///
/// TODO: the layout (stub: as written).
pub fn chain_operand(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Binary`.
///
/// TODO: the layout (stub: as written).
pub fn binary(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Unary`.
///
/// TODO: the layout (stub: as written).
pub fn unary(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Bracketed`.
///
/// TODO: the layout (stub: as written).
pub fn bracketed(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Call`.
///
/// TODO: the layout (stub: as written).
pub fn call(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `ArgList`.
///
/// TODO: the layout (stub: as written).
pub fn arg_list(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Arg`.
///
/// TODO: the layout (stub: as written).
pub fn arg(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Aggregate`.
///
/// TODO: the layout (stub: as written).
pub fn aggregate(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `InList`.
///
/// TODO: the layout (stub: as written).
pub fn in_list(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `Exists`.
///
/// TODO: the layout (stub: as written).
pub fn exists(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `NotExists`.
///
/// TODO: the layout (stub: as written).
pub fn not_exists(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}
