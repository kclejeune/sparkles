//! The prologue: `VERSION` first, then `BASE` and the `PREFIX` runs, sorted and
//! grouped (N5, `prefix-groups`), one declaration per line.

use super::Ctx;
use crate::doc::DocId;
use crate::normalize::{self, PrefixDecl};
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeId};
use crate::trivia;

/// `Prologue`: `VERSION` declarations first, then the others in source order, each
/// run of `PREFIX` declarations sorted by label, deduplicated and grouped as
/// `prefix-groups` says (a run that binds a label twice stays as written). A `BASE`, a
/// `VERSION` or a detached comment block ends a run; without groups a blank line does
/// too, and with groups the formatter owns the blank lines inside a run.
pub fn prologue(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let decls = cx.child_nodes(n);
    let (versions, rest): (Vec<NodeId>, Vec<NodeId>) = decls
        .iter()
        .partition(|&&d| cx.tree.kind(d) == NodeKind::VersionDecl);

    // the prefix declarations, for the run planning
    let mut prefixes: Vec<PrefixDecl> = Vec::new();
    let mut after_other = false;
    for (i, &d) in decls.iter().enumerate() {
        if cx.tree.kind(d) != NodeKind::PrefixDecl {
            after_other = i > 0;
            continue;
        }
        let (label, iri) = prefix_parts(cx, d);
        prefixes.push(PrefixDecl {
            node: d,
            label,
            iri,
            // a detached block stays at the start of the run, so it does not count
            has_comments: !cx.comments.leading(d).is_empty() || !cx.comments.trailing(d).is_empty(),
            barrier_before: after_other || !cx.comments.detached_before(d).is_empty(),
            blank_before: cx.comments.blank_before(d),
        });
        after_other = false;
    }
    let runs = normalize::plan_runs(&prefixes, &cx.opts.prefix_groups);

    // (node, its document, whether a blank line goes before it)
    let mut items: Vec<(NodeId, DocId, bool)> = Vec::new();
    for &v in &versions {
        let d = cx.node(v);
        items.push((v, d, cx.comments.blank_before(v)));
    }
    let mut next_prefix = 0;
    let mut runs = runs.into_iter();
    for &d in &rest {
        if cx.tree.kind(d) != NodeKind::PrefixDecl {
            let doc = cx.node(d);
            items.push((d, doc, cx.comments.blank_before(d)));
            continue;
        }
        if prefixes.get(next_prefix).map(|p| p.node) != Some(d) {
            continue;
        }
        let Some(run) = runs.next() else { continue };
        let len = run.order.len() + run.drop.len();
        // the run starts where its first declaration was written, and the detached
        // comment blocks before it (a section header) stay there, whatever moves first
        let lead_blank = cx.comments.blank_before(d);
        let mut detached = Vec::new();
        for block in cx.comments.detached_before(d).to_vec() {
            for &c in &block {
                cx.comments.mark_printed(c);
            }
            detached.push(trivia::comment_lines(&mut cx.arena, cx.comments, &block));
            detached.push(cx.empty_line());
        }
        for (k, &i) in run.order.iter().enumerate() {
            let node = prefixes[i].node;
            let mut doc = cx.node(node);
            if k == 0 && !detached.is_empty() {
                detached.push(doc);
                doc = cx.concat(std::mem::take(&mut detached));
            }
            let blank = match (k, run.sorted) {
                (0, _) => lead_blank,
                (_, true) => run.group_breaks.contains(&k),
                (_, false) => prefixes[i].blank_before,
            };
            items.push((node, doc, blank));
        }
        next_prefix += len;
    }

    let mut parts = Vec::new();
    for (k, &(_, doc, blank)) in items.iter().enumerate() {
        if k > 0 {
            parts.push(match blank {
                true => cx.empty_line(),
                false => cx.hard_line(),
            });
        }
        parts.push(doc);
    }
    cx.concat(parts)
}

/// A `PREFIX` declaration's label (without `:`) and namespace (without `<` `>`).
fn prefix_parts(cx: &Ctx<'_, '_>, d: NodeId) -> (String, String) {
    let (mut label, mut iri) = (String::new(), String::new());
    for e in cx.tree.children(d) {
        if let Element::Token(t) = *e {
            let text = cx.tree.token_text(t);
            match cx.tree.token_kind(t) {
                crate::lex::TokenKind::PnameNs => {
                    label = text.strip_suffix(':').unwrap_or(text).to_string()
                }
                crate::lex::TokenKind::IriRef => {
                    iri = text[1..text.len() - 1].to_string();
                }
                _ => {}
            }
        }
    }
    (label, iri)
}

/// The declaration's tokens one space apart, keywords in the grammar's spelling and
/// the IRIs as written (never compacted).
fn declaration(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let docs: Vec<DocId> = cx
        .children(n)
        .into_iter()
        .filter_map(|e| match e {
            Element::Token(t) => Some(cx.kw(t)),
            Element::Node(_) => None,
        })
        .collect();
    cx.spaced(docs)
}

/// `BaseDecl`: `BASE <…>`.
pub fn base_decl(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    declaration(cx, n)
}

/// `PrefixDecl`: `PREFIX ex: <…>`, one space after the label.
pub fn prefix_decl(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    declaration(cx, n)
}

/// `VersionDecl`: `VERSION "1.2"`, the string in the configured quote style.
pub fn version_decl(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}
