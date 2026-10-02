//! The prologue: `VERSION` first, then `BASE` and the `PREFIX` runs, sorted and
//! grouped (N5, `prefix-groups`), one declaration per line; with `prune-prefixes`
//! without the declarations nothing uses.

use super::Ctx;
use crate::doc::DocId;
use crate::normalize::{self, PrefixDecl, prune};
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeId, TokenId};
use crate::trivia;
use std::collections::{HashMap, HashSet};

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

    let pruned = match cx.opts.prune_prefixes {
        true => pruned(cx, &decls),
        false => Pruned::default(),
    };
    // the prefix declarations that are printed, for the run planning
    let mut prefixes: Vec<PrefixDecl> = Vec::new();
    let mut after_other = false;
    for (i, &d) in decls.iter().enumerate() {
        if cx.tree.kind(d) != NodeKind::PrefixDecl {
            after_other = i > 0;
            continue;
        }
        if pruned.nodes.contains(&d) {
            continue;
        }
        let (label, iri) = prefix_parts(cx, d);
        prefixes.push(PrefixDecl {
            node: d,
            label,
            iri,
            // a detached block stays at the start of the run, so it does not count
            has_comments: !cx.comments.leading(d).is_empty() || !cx.comments.trailing(d).is_empty(),
            barrier_before: after_other
                || !cx.comments.detached_before(d).is_empty()
                || pruned.blocks.contains_key(&d),
            blank_before: cx.comments.blank_before(d) || pruned.blank.contains(&d),
        });
        after_other = false;
    }
    if let Some(first) = under_header(cx, &decls, &prefixes) {
        prefixes[first].has_comments = true;
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
            let mut parts = comment_blocks(cx, pruned.blocks.get(&d).map_or(&[], Vec::as_slice));
            parts.push(cx.node(d));
            let doc = cx.concat(parts);
            let blank = cx.comments.blank_before(d) || pruned.blank.contains(&d);
            items.push((d, doc, blank));
            continue;
        }
        if prefixes.get(next_prefix).map(|p| p.node) != Some(d) {
            continue;
        }
        let Some(run) = runs.next() else { continue };
        let len = run.order.len() + run.drop.len();
        // the run starts where its first declaration was written, and the detached
        // comment blocks before it (a section header, and those of the dropped
        // declarations before it) stay there, whatever moves first
        let lead_blank = prefixes[next_prefix].blank_before;
        let mut blocks = pruned.blocks.get(&d).cloned().unwrap_or_default();
        blocks.extend(cx.comments.detached_before(d).iter().cloned());
        let mut detached = comment_blocks(cx, &blocks);
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
    // every declaration dropped: their detached blocks are all the prologue prints
    if !pruned.end_blocks.is_empty() {
        let mut parts = comment_blocks(cx, &pruned.end_blocks);
        parts.pop();
        let doc = cx.concat(parts);
        items.push((n, doc, true));
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

/// Comment blocks, each followed by a blank line (marked printed).
fn comment_blocks(cx: &mut Ctx<'_, '_>, blocks: &[Vec<TokenId>]) -> Vec<DocId> {
    let mut parts = Vec::new();
    for block in blocks {
        for &c in block {
            cx.comments.mark_printed(c);
        }
        parts.push(trivia::comment_lines(&mut cx.arena, cx.comments, block));
        parts.push(cx.empty_line());
    }
    parts
}

/// The declarations of a prologue that `prune-prefixes` drops, and what passes from them
/// to the next item printed.
#[derive(Default)]
struct Pruned {
    nodes: HashSet<NodeId>,
    /// the items after dropped declarations that had a blank line before them
    blank: HashSet<NodeId>,
    /// the detached comment blocks of dropped declarations, printed before the next
    /// item (which starts a run)
    blocks: HashMap<NodeId, Vec<Vec<TokenId>>>,
    /// those with no item after them
    end_blocks: Vec<Vec<TokenId>>,
}

/// The declarations of the prologue `decls` that `prune-prefixes` drops: those nothing
/// uses that have no comment of their own (leading or trailing). Detached blocks before
/// one are a section's, not its own: they stay, before the next item.
///
/// The output must prune to itself, so a declaration stays when the second run would
/// keep it. One printed first in the file right under the header comments has them as
/// its leading block the second time (the header is every comment before the first
/// token): the first declaration of the source stays when the header touches it and
/// it would be printed first.
fn pruned(cx: &mut Ctx<'_, '_>, decls: &[NodeId]) -> Pruned {
    // an update request without operations stays as written: pruning could leave only
    // comments, which the second run reads as a file header
    let root = cx.tree.root();
    if cx.tree.kind(root) == NodeKind::UpdateUnit
        && cx
            .tree
            .child_nodes(root)
            .all(|c| cx.tree.kind(c) == NodeKind::Prologue)
    {
        return Pruned::default();
    }
    if cx.unused_prefixes.is_none() {
        let comments = cx.comments;
        let unused =
            prune::unused_declarations_with(cx.tree, &cx.scope, cx.opts, |n| comments.ignored(n));
        cx.unused_prefixes = Some(unused);
    }
    let unused = cx.unused_prefixes.as_ref().expect("worked out above");
    let comments = cx.comments;
    let mut out = Pruned {
        nodes: decls
            .iter()
            .copied()
            .filter(|&d| {
                cx.tree.kind(d) == NodeKind::PrefixDecl
                    && unused.contains(&d)
                    && comments.leading(d).is_empty()
                    && comments.trailing(d).is_empty()
            })
            .collect(),
        ..Pruned::default()
    };
    if let Some(&first) = decls.first()
        && out.nodes.contains(&first)
        && cx.tree.first_token(first) == cx.tree.first_token(cx.tree.root())
        && !comments.header().is_empty()
        && !comments.blank_before(first)
        && printed_first(cx, decls, &out.nodes)
    {
        out.nodes.remove(&first);
    }
    out.pass_on(cx, decls);
    // blocks with no item after them: the last declaration holding one stays when
    // something else of the prologue is printed (the second run sees the blocks before
    // it as detached, and nothing after it either); else they print alone, and they are
    // the file header the second time
    let printed = decls.iter().any(|d| !out.nodes.contains(d));
    if !out.end_blocks.is_empty()
        && printed
        && let Some(&last) = decls
            .iter()
            .rev()
            .find(|&&d| out.nodes.contains(&d) && !comments.detached_before(d).is_empty())
    {
        out.nodes.remove(&last);
        out.pass_on(cx, decls);
    }
    out
}

impl Pruned {
    /// What passes on from the dropped declarations: blank lines and detached blocks,
    /// to the next item but a `VERSION`, or to the end.
    fn pass_on(&mut self, cx: &Ctx<'_, '_>, decls: &[NodeId]) {
        self.blank.clear();
        self.blocks.clear();
        let mut blank = false;
        let mut blocks: Vec<Vec<TokenId>> = Vec::new();
        for &d in decls {
            if cx.tree.kind(d) == NodeKind::VersionDecl {
                continue;
            }
            if self.nodes.contains(&d) {
                blank |= cx.comments.blank_before(d);
                blocks.extend(cx.comments.detached_before(d).iter().cloned());
                continue;
            }
            if std::mem::take(&mut blank) {
                self.blank.insert(d);
            }
            if !blocks.is_empty() {
                self.blocks.insert(d, std::mem::take(&mut blocks));
            }
        }
        self.end_blocks = blocks;
    }
}

/// Whether the first declaration of `decls` is printed first when the declarations in
/// `dropped` (it among them) go: no `VERSION`, and it sorts first in its run.
fn printed_first(cx: &Ctx<'_, '_>, decls: &[NodeId], dropped: &HashSet<NodeId>) -> bool {
    let kind = |d: NodeId| cx.tree.kind(d);
    if decls.iter().any(|&d| kind(d) == NodeKind::VersionDecl) {
        return false;
    }
    let groups = &cx.opts.prefix_groups;
    // its run: up to a `BASE`, a detached block, or (without groups) a blank line; a
    // dropped declaration passes those on to the next one, which ends the run the same
    let run: Vec<(String, String)> = std::iter::once(decls[0])
        .chain(decls[1..].iter().copied().take_while(|&d| {
            kind(d) == NodeKind::PrefixDecl
                && cx.comments.detached_before(d).is_empty()
                && (!groups.is_empty() || !cx.comments.blank_before(d))
        }))
        .enumerate()
        .filter(|&(i, d)| i == 0 || !dropped.contains(&d))
        .map(|(_, d)| prefix_parts(cx, d))
        .collect();
    let conflict = run
        .iter()
        .any(|a| run.iter().any(|b| a.0 == b.0 && a.1 != b.1));
    let group_of = |label: &str| {
        groups
            .iter()
            .position(|g| g.iter().any(|l| l == label))
            .unwrap_or(groups.len())
    };
    let key = |p: &(String, String)| (group_of(&p.0), p.0.clone());
    conflict || run[1..].iter().all(|p| key(&run[0]) <= key(p))
}

/// The declaration (an index into `prefixes`) whose comments the file header (every
/// comment before the first token) counts as, when it touches the first declaration:
/// the one printed first, right under the header. The second run reads the header as
/// that declaration's leading block, and a declaration printed first under comments of
/// its own has them as the header the second time; counting them the same both times
/// keeps the run from dropping it as a plain duplicate once and keeping it the other.
fn under_header(cx: &Ctx<'_, '_>, decls: &[NodeId], prefixes: &[PrefixDecl]) -> Option<usize> {
    let &d = decls.first()?;
    let touches = cx.tree.kind(d) == NodeKind::PrefixDecl
        && prefixes.first().map(|p| p.node) == Some(d)
        && !decls
            .iter()
            .any(|&v| cx.tree.kind(v) == NodeKind::VersionDecl)
        && cx.tree.first_token(d) == cx.tree.first_token(cx.tree.root())
        && !cx.comments.header().is_empty()
        && !cx.comments.blank_before(d)
        && cx.comments.detached_before(d).is_empty();
    if !touches {
        return None;
    }
    // the first run, sorted as `normalize::plan_runs` sorts it (one that binds a label
    // twice stays as written)
    let groups = &cx.opts.prefix_groups;
    let end = (1..prefixes.len())
        .find(|&i| prefixes[i].barrier_before || (groups.is_empty() && prefixes[i].blank_before))
        .unwrap_or(prefixes.len());
    let run = &prefixes[..end];
    if run
        .iter()
        .any(|a| run.iter().any(|b| a.label == b.label && a.iri != b.iri))
    {
        return Some(0);
    }
    let group_of = |label: &str| {
        groups
            .iter()
            .position(|g| g.iter().any(|l| l == label))
            .unwrap_or(groups.len())
    };
    (0..end).min_by_key(|&i| (group_of(&run[i].label), run[i].label.as_str(), i))
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

#[cfg(test)]
mod tests {
    use crate::{Language, Options, format};

    /// `src` formatted with `prune-prefixes` (through every check, to a fixpoint).
    fn pruned(src: &str) -> String {
        let opts = Options {
            prune_prefixes: true,
            ..Options::default()
        };
        let out = format(src, Language::Sparql, &opts).unwrap_or_else(|e| panic!("{src:?}: {e}"));
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        let again = format(&out.text, Language::Sparql, &opts).unwrap();
        assert_eq!(again.text, out.text, "not a fixpoint");
        out.text
    }

    #[test]
    fn prune_drops_unused_declarations_without_comments() {
        assert_eq!(
            pruned("PREFIX a: <http://e/a#> PREFIX b: <http://e/b#>\nSELECT * { ?s a:p ?o }"),
            "PREFIX a: <http://e/a#>\n\nSELECT *\nWHERE {\n  ?s a:p ?o .\n}\n"
        );
        // nothing left: no prologue, no blank line
        assert_eq!(pruned("PREFIX a: <http://e/a#>\nASK {}"), "ASK {}\n");
        assert_eq!(
            pruned("# header\n\nPREFIX a: <http://e/a#>\nBASE <http://e/>\nASK {}"),
            "# header\n\nBASE <http://e/>\n\nASK {}\n"
        );
        // a comment keeps a declaration
        assert_eq!(
            pruned("PREFIX a: <http://e/a#> # kept\n# kept too\nPREFIX b: <http://e/b#>\nASK {}"),
            "PREFIX a: <http://e/a#> # kept\n# kept too\nPREFIX b: <http://e/b#>\n\nASK {}\n"
        );
    }

    #[test]
    fn prune_keeps_the_runs_and_their_blank_lines() {
        // the blank line before a dropped declaration starts the next kept one's run
        assert_eq!(
            pruned(
                "PREFIX z: <http://e/z#>\n\nPREFIX x: <http://e/x#>\nPREFIX b: <http://e/b#>\nPREFIX a: <http://e/a#>\nSELECT * { ?s z:p a:o , b:o }"
            ),
            "PREFIX z: <http://e/z#>\n\nPREFIX a: <http://e/a#>\nPREFIX b: <http://e/b#>\n\nSELECT *\nWHERE {\n  ?s z:p a:o, b:o .\n}\n"
        );
        // a BASE still ends a run when the declaration after it goes
        assert_eq!(
            pruned(
                "PREFIX z: <http://e/z#>\nBASE <http://e/>\nPREFIX x: <http://e/x#>\nPREFIX a: <http://e/a#>\nSELECT * { ?s z:p a:o }"
            ),
            "PREFIX z: <http://e/z#>\nBASE <http://e/>\nPREFIX a: <http://e/a#>\n\nSELECT *\nWHERE {\n  ?s z:p a:o .\n}\n"
        );
        // the section comments of a dropped declaration stay, before the next item
        assert_eq!(
            pruned(
                "PREFIX x: <http://e/x#>\n\n# section\n\nPREFIX y: <http://e/y#>\nPREFIX a: <http://e/a#>\nSELECT * { ?s a:p ?o }"
            ),
            "# section\n\nPREFIX a: <http://e/a#>\n\nSELECT *\nWHERE {\n  ?s a:p ?o .\n}\n"
        );
        assert_eq!(
            pruned(
                "PREFIX a: <http://e/a#>\n\n# one\n\n# two\nPREFIX y: <http://e/y#>\nBASE <http://e/>\nSELECT * { ?s a:p ?o }"
            ),
            "PREFIX a: <http://e/a#>\n\n# one\n\n# two\nPREFIX y: <http://e/y#>\nBASE <http://e/>\n\nSELECT *\nWHERE {\n  ?s a:p ?o .\n}\n"
        );
        assert_eq!(
            pruned(
                "PREFIX a: <http://e/a#>\n\n# one\n\nPREFIX y: <http://e/y#>\nBASE <http://e/>\nSELECT * { ?s a:p ?o }"
            ),
            "PREFIX a: <http://e/a#>\n\n# one\n\nBASE <http://e/>\n\nSELECT *\nWHERE {\n  ?s a:p ?o .\n}\n"
        );
        assert_eq!(
            pruned("PREFIX a: <http://e/a#>\n\n# one\n\nPREFIX y: <http://e/y#>\nASK {}"),
            "# one\n\nASK {}\n"
        );
        assert_eq!(
            pruned(
                "PREFIX a: <http://e/a#>\nPREFIX b: <http://e/b#>\n\n# one\n\nPREFIX y: <http://e/y#>\nASK { ?s a:p ?o }"
            ),
            "PREFIX a: <http://e/a#>\n\n# one\n\nPREFIX y: <http://e/y#>\n\nASK {\n  ?s a:p ?o .\n}\n"
        );
        // under a header, the declaration printed first has the header as its leading
        // comments in the output, so it stays when it is printed first anyway
        assert_eq!(
            pruned("# about\nPREFIX a: <http://e/a#>\nPREFIX b: <http://e/b#>\nASK {}"),
            "# about\nPREFIX a: <http://e/a#>\n\nASK {}\n"
        );
        assert_eq!(
            pruned("# about\nPREFIX b: <http://e/b#>\nPREFIX a: <http://e/a#>\nASK { ?s a:p ?o }"),
            "# about\nPREFIX a: <http://e/a#>\n\nASK {\n  ?s a:p ?o .\n}\n"
        );
        assert_eq!(
            pruned("# about\n\nPREFIX a: <http://e/a#>\nASK {}"),
            "# about\n\nASK {}\n"
        );
        // an update without operations stays as written
        let src = "PREFIX a: <http://e/a#>\n\n# one\n\nPREFIX b: <http://e/b#>\n# the end\n";
        assert_eq!(pruned(src), src);
        // the dropped declarations go, operations follow
        assert_eq!(
            pruned("# about\nPREFIX a: <http://e/a#>\n\nPREFIX b: <http://e/b#>\nCLEAR ALL"),
            "# about\nPREFIX a: <http://e/a#>\n\nCLEAR ALL\n"
        );
        assert_eq!(
            pruned("# about\n\nPREFIX a: <http://e/a#>\nCLEAR ALL ; CLEAR DEFAULT # the end"),
            "# about\n\nCLEAR ALL;\n\nCLEAR DEFAULT # the end\n"
        );
        // of two identical declarations the one in scope at the uses stays
        assert_eq!(
            pruned("PREFIX a: <http://e/a#>\nPREFIX a: <http://e/a#>\nSELECT * { ?s a:p ?o }"),
            "PREFIX a: <http://e/a#>\n\nSELECT *\nWHERE {\n  ?s a:p ?o .\n}\n"
        );
        // a label bound twice: the shadowed binding goes, so the run sorts again
        assert_eq!(
            pruned(
                "PREFIX b: <http://e/1#>\nPREFIX a: <http://e/a#>\nPREFIX b: <http://e/2#>\nSELECT * { ?s b:p a:o }"
            ),
            "PREFIX a: <http://e/a#>\nPREFIX b: <http://e/2#>\n\nSELECT *\nWHERE {\n  ?s b:p a:o .\n}\n"
        );
    }
}
