//! The prologue: `VERSION` first, then `BASE` and the `PREFIX` runs, sorted and
//! grouped (N5, `prefix-groups`), one declaration per line.

use super::Ctx;
use crate::doc::DocId;
use crate::tree::NodeId;

/// `Prologue`.
///
/// TODO: the layout (stub: as written).
pub fn prologue(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `BaseDecl`.
///
/// TODO: the layout (stub: as written).
pub fn base_decl(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `PrefixDecl`.
///
/// TODO: the layout (stub: as written).
pub fn prefix_decl(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}

/// `VersionDecl`.
///
/// TODO: the layout (stub: as written).
pub fn version_decl(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.verbatim(n)
}
