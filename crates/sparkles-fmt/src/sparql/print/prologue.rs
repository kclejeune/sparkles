//! The prologue: `VERSION` first, then `BASE` and the `PREFIX` runs, sorted and
//! grouped (N5, `prefix-groups`), one declaration per line.

use super::Ctx;
use crate::doc::DocId;
use crate::tree::NodeId;

/// `Prologue`: one declaration per line, blank lines kept as written.
///
/// TODO: `VERSION` first, and the `PREFIX` runs sorted, deduplicated and grouped.
pub fn prologue(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let decls = cx.child_nodes(n);
    cx.lines(&decls)
}

/// `BaseDecl`: `BASE <…>`.
pub fn base_decl(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `PrefixDecl`: `PREFIX ex: <…>`, one space after the label.
pub fn prefix_decl(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `VersionDecl`: `VERSION "1.2"`.
///
/// TODO: the version string in the configured quote style.
pub fn version_decl(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}
