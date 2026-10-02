//! `prune-prefixes` for Turtle and TriG: the prefix declarations of a directive block
//! that nothing uses in their scope ([`crate::normalize::prune`]) and have no comment of
//! their own are not printed. Their detached comment blocks (a section header) and the
//! blank line before them pass on to the next item printed in the block, or are printed
//! at the block's end. A block that prints nothing at all is no sort barrier either, so
//! the statements around it sort together, as they will the next time.
//!
//! The output must prune to itself, as with the SPARQL prologue: a document of
//! directives alone stays as written (only comments could be left, the file header the
//! next time); the first declaration of the file, right under the header comments, stays
//! when it would be printed first (those comments would lead it the next time); and an
//! unused redefinition of a label stays when the declaration of that label printed last
//! before it binds a different namespace (without the redefinition, that binding would
//! cover its scope, and the IRIs there would be compacted with it the next time). An
//! unused declaration whose label is redefined later goes like any other: its scope ends
//! at the redefinition. Detached blocks left with no item after them in their block are
//! printed at its end: the next time they are detached before the statement that follows,
//! which prints them the same way (and they are a sort barrier there, as the block was).

use super::Tx;
use crate::normalize::prune;
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeId, TokenId};
use crate::trivia;
use std::collections::{HashMap, HashSet};

/// The declarations of a directive block that `prune-prefixes` drops, and what passes
/// from them to the next item printed.
#[derive(Debug, Default)]
pub struct Pruned {
    pub nodes: HashSet<NodeId>,
    /// the items after dropped declarations that had a blank line before them
    pub blank: HashSet<NodeId>,
    /// the detached comment blocks of dropped declarations, printed before the next
    /// item (which starts a run)
    pub blocks: HashMap<NodeId, Vec<Vec<TokenId>>>,
    /// those with no item after them
    pub end_blocks: Vec<Vec<TokenId>>,
}

impl Pruned {
    /// Whether the block `decls` prints nothing.
    pub fn prints_nothing(&self, decls: &[NodeId]) -> bool {
        self.end_blocks.is_empty() && decls.iter().all(|d| self.nodes.contains(d))
    }

    /// What passes on from the dropped declarations: blank lines and detached blocks,
    /// to the next item but a `VERSION`, or to the end.
    fn pass_on(&mut self, tx: &Tx<'_, '_>, decls: &[NodeId]) {
        self.blank.clear();
        self.blocks.clear();
        let mut blank = false;
        let mut blocks: Vec<Vec<TokenId>> = Vec::new();
        for &d in decls {
            if tx.tree.kind(d) == NodeKind::VersionDecl {
                continue;
            }
            if self.nodes.contains(&d) {
                blank |= tx.comments.blank_before(d);
                blocks.extend(tx.comments.detached_before(d).iter().cloned());
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

/// The declarations of directive block `decls` that `prune-prefixes` drops (none
/// without the key), of those the whole document drops ([`dropped`]).
pub fn pruned(tx: &mut Tx<'_, '_>, decls: &[NodeId]) -> Pruned {
    if !tx.opts.prune_prefixes {
        return Pruned::default();
    }
    if tx.dropped_prefixes.is_none() {
        let dropped = dropped(tx);
        tx.dropped_prefixes = Some(dropped);
    }
    let dropped = tx.dropped_prefixes.as_ref().expect("worked out above");
    let mut out = Pruned {
        nodes: decls
            .iter()
            .copied()
            .filter(|d| dropped.contains(d))
            .collect(),
        ..Pruned::default()
    };
    out.pass_on(tx, decls);
    out
}

/// The prefix declarations of the document that `prune-prefixes` drops: those nothing
/// uses that have no comment of their own (leading or trailing), but the first one under
/// the file header when it would be printed first, and those [`scoped`] keeps. None in a
/// document of directives alone.
fn dropped(tx: &Tx<'_, '_>) -> HashSet<NodeId> {
    let root = tx.tree.root();
    let directive = |d: NodeId| {
        matches!(
            tx.tree.kind(d),
            NodeKind::PrefixDecl | NodeKind::BaseDecl | NodeKind::VersionDecl
        )
    };
    let items: Vec<NodeId> = tx.tree.child_nodes(root).collect();
    if items.iter().all(|&d| directive(d)) {
        return HashSet::new();
    }
    let comments = tx.comments;
    let unused =
        prune::unused_declarations_with(tx.tree, &tx.scope, tx.opts, |n| comments.ignored(n));
    let decls: Vec<NodeId> = items
        .iter()
        .copied()
        .filter(|&d| tx.tree.kind(d) == NodeKind::PrefixDecl)
        .collect();
    let mut candidates: HashSet<NodeId> = decls
        .iter()
        .copied()
        .filter(|d| {
            unused.contains(d)
                && comments.leading(*d).is_empty()
                && comments.trailing(*d).is_empty()
        })
        .collect();
    let mut out = scoped(tx, &decls, &candidates);
    // the leading block: the directives the document starts with
    let leading: Vec<NodeId> = items
        .iter()
        .copied()
        .take_while(|&d| directive(d))
        .collect();
    if let Some(&first) = leading.first()
        && out.contains(&first)
        && tx.tree.first_token(first) == tx.tree.first_token(root)
        && !comments.header().is_empty()
        && !comments.blank_before(first)
        && printed_first(tx, &leading, &out)
    {
        candidates.remove(&first);
        out = scoped(tx, &decls, &candidates);
    }
    out
}

/// The `candidates` among the prefix declarations `decls` (in document order) that go.
/// A candidate stays when the last declaration of its label kept before it binds a
/// different namespace, since dropping it would extend that binding over its scope.
fn scoped(tx: &Tx<'_, '_>, decls: &[NodeId], candidates: &HashSet<NodeId>) -> HashSet<NodeId> {
    let mut in_force: HashMap<String, String> = HashMap::new();
    let mut out = HashSet::new();
    for &d in decls {
        let (label, iri) = prefix_parts(tx, d);
        if candidates.contains(&d) && in_force.get(&label).is_none_or(|kept| *kept == iri) {
            out.insert(d);
        } else {
            in_force.insert(label, iri);
        }
    }
    out
}

/// Comment blocks, each followed by a blank line (marked printed).
pub fn comment_blocks(tx: &mut Tx<'_, '_>, blocks: &[Vec<TokenId>]) -> Vec<crate::doc::DocId> {
    let mut parts = Vec::new();
    for block in blocks {
        for &c in block {
            tx.comments.mark_printed(c);
        }
        let cx = &mut tx.cx;
        parts.push(trivia::comment_lines(&mut cx.arena, cx.comments, block));
        parts.push(tx.empty_line());
    }
    parts
}

/// Whether the first declaration of the leading block `decls` is printed first when the
/// declarations in `dropped` (it among them) go: no `VERSION`, and it sorts first in its
/// run.
fn printed_first(tx: &Tx<'_, '_>, decls: &[NodeId], dropped: &HashSet<NodeId>) -> bool {
    let kind = |d: NodeId| tx.tree.kind(d);
    if decls.iter().any(|&d| kind(d) == NodeKind::VersionDecl) {
        return false;
    }
    let groups = &tx.opts.prefix_groups;
    // its run: up to a `BASE`, a detached block, or (without groups) a blank line; a
    // dropped declaration passes those on to the next one, which ends the run the same
    let run: Vec<(String, String)> = std::iter::once(decls[0])
        .chain(decls[1..].iter().copied().take_while(|&d| {
            kind(d) == NodeKind::PrefixDecl
                && tx.comments.detached_before(d).is_empty()
                && (!groups.is_empty() || !tx.comments.blank_before(d))
        }))
        .enumerate()
        .filter(|&(i, d)| i == 0 || !dropped.contains(&d))
        .map(|(_, d)| prefix_parts(tx, d))
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

/// A `PREFIX` declaration's label (without `:`) and namespace (without `<` `>`).
pub fn prefix_parts(tx: &Tx<'_, '_>, d: NodeId) -> (String, String) {
    let (mut label, mut iri) = (String::new(), String::new());
    for e in tx.tree.children(d) {
        if let Element::Token(t) = *e {
            let text = tx.tree.token_text(t);
            match tx.tree.token_kind(t) {
                crate::lex::TokenKind::PnameNs => {
                    label = text.strip_suffix(':').unwrap_or(text).to_string()
                }
                crate::lex::TokenKind::IriRef => iri = text[1..text.len() - 1].to_string(),
                _ => {}
            }
        }
    }
    (label, iri)
}

#[cfg(test)]
mod tests {
    use super::super::super::Turtle;
    use crate::{Options, check};

    fn opts() -> Options {
        Options {
            prune_prefixes: true,
            ..Options::default()
        }
    }

    /// `src` formatted with `prune-prefixes` (and `extra`), through every check, to a
    /// fixpoint.
    fn pruned_with(src: &str, trig: bool, opts: &Options) -> String {
        let out =
            check::run(&Turtle { trig }, src, opts).unwrap_or_else(|e| panic!("{src:?}: {e}"));
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        let again = check::run(&Turtle { trig }, &out.text, opts).unwrap();
        assert_eq!(again.text, out.text, "not a fixpoint");
        out.text
    }

    fn pruned(src: &str) -> String {
        pruned_with(src, false, &opts())
    }

    #[test]
    fn unused_declarations_go() {
        assert_eq!(
            pruned("@prefix b: <http://e/b#> .\n@prefix a: <http://e/a#> .\na:s a:p 1 ."),
            "PREFIX a: <http://e/a#>\n\na:s a:p 1 .\n"
        );
        // uses are counted in the output: `a` for rdf:type, numeric shorthands, compacted
        // IRIs
        assert_eq!(
            pruned(
                "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>\nPREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\nPREFIX e: <http://e/>\n<http://e/s> rdf:type <http://e/C> ; <http://e/p> \"1\"^^xsd:integer ."
            ),
            "PREFIX e: <http://e/>\n\ne:s\n  a e:C ;\n  e:p 1 ;\n.\n"
        );
        // a comment keeps a declaration; a section header stays
        assert_eq!(
            pruned(
                "PREFIX a: <http://e/a#> # kept\nPREFIX b: <http://e/b#>\n\n# section\n\nPREFIX c: <http://e/c#>\nPREFIX d: <http://e/d#>\nd:s d:p 1 ."
            ),
            "PREFIX a: <http://e/a#> # kept\n\n# section\n\nPREFIX d: <http://e/d#>\n\nd:s d:p 1 .\n"
        );
        // a graph name is a use
        assert_eq!(
            pruned_with(
                "PREFIX g: <http://e/g#>\nPREFIX x: <http://e/x#>\ng:g { <http://e/s> <http://e/p> 1 }",
                true,
                &opts()
            ),
            "PREFIX g: <http://e/g#>\n\nGRAPH g:g {\n  <http://e/s> <http://e/p> 1 .\n}\n"
        );
    }

    #[test]
    fn scopes_end_at_a_redefinition() {
        // a label bound twice to the same namespace: the unused binding goes
        assert_eq!(
            pruned(
                "PREFIX a: <http://e/1#>\na:s a:p 1 .\nPREFIX a: <http://e/1#>\nPREFIX b: <http://e/b#>\nb:s b:p 2 ."
            ),
            "PREFIX a: <http://e/1#>\n\na:s a:p 1 .\nPREFIX b: <http://e/b#>\n\nb:s b:p 2 .\n"
        );
        // to different namespaces, the first one printed: the unused redefinition stays
        // (without it, the first would compact the IRI below the next time)
        assert_eq!(
            pruned(
                "PREFIX a: <http://e/1#>\na:s a:p 1 .\nPREFIX a: <http://e/2#>\nPREFIX b: <http://e/b#>\nb:s b:p <http://e/1#o> ."
            ),
            "PREFIX a: <http://e/1#>\n\na:s a:p 1 .\nPREFIX a: <http://e/2#>\nPREFIX b: <http://e/b#>\n\nb:s b:p <http://e/1#o> .\n"
        );
        // the first one unused: its scope ends at the redefinition, and it goes; so does
        // the redefinition when nothing uses it either
        assert_eq!(
            pruned(
                "PREFIX a: <http://e/1#>\n<http://e/s> <http://e/p> 1 .\nPREFIX a: <http://e/2#>\na:s a:p 2 ."
            ),
            "<http://e/s> <http://e/p> 1 .\nPREFIX a: <http://e/2#>\n\na:s a:p 2 .\n"
        );
        assert_eq!(
            pruned(
                "PREFIX ex: <http://example.org/>\n PREFIX o: <http://example.org/o/> ex:s ex:p _:x . ex:s ex:p '''two\nlines''', '''two\nlines''' . @prefix o: <http://other.org/> . ex:s ex:p '''two\nlines''' .\n"
            ),
            "PREFIX ex: <http://example.org/>\n\nex:s ex:p _:x .\n\nex:s\n  ex:p \"\"\"two\nlines\"\"\", \"\"\"two\nlines\"\"\" ;\n.\n\nex:s ex:p \"\"\"two\nlines\"\"\" .\n"
        );
        // a redefinition dropped in between: the first binding is still the one printed
        // before the third
        assert_eq!(
            pruned(
                "PREFIX a: <http://e/1#>\na:s a:p 1 .\nPREFIX a: <http://e/1#>\n<http://e/s> <http://e/p> 2 .\nPREFIX a: <http://e/2#>\n<http://e/s> <http://e/p> <http://e/1#o> ."
            ),
            "PREFIX a: <http://e/1#>\n\na:s a:p 1 .\n<http://e/s> <http://e/p> 2 .\nPREFIX a: <http://e/2#>\n\n<http://e/s> <http://e/p> <http://e/1#o> .\n"
        );
        // a later block that prints nothing goes, and is no sort barrier any more
        let sorted = Options {
            sort: true,
            ..opts()
        };
        assert_eq!(
            pruned_with(
                "PREFIX a: <http://e/a#>\na:z a:p 1 .\nPREFIX x: <http://e/x#>\na:y a:p 1 .",
                false,
                &sorted
            ),
            "PREFIX a: <http://e/a#>\n\na:y a:p 1 .\na:z a:p 1 .\n"
        );
        // its section header stays, and stays a barrier
        assert_eq!(
            pruned_with(
                "PREFIX a: <http://e/a#>\n\n# vocabularies for later\n\nPREFIX x: <http://e/x#>\na:z a:p 1 .\na:y a:p 1 .",
                false,
                &sorted
            ),
            "PREFIX a: <http://e/a#>\n\n# vocabularies for later\n\na:y a:p 1 .\na:z a:p 1 .\n"
        );
        assert_eq!(
            pruned_with(
                "PREFIX a: <http://e/a#>\na:z a:p 1 .\n\n# later\n\nPREFIX x: <http://e/x#>\na:y a:p 1 .",
                false,
                &sorted
            ),
            "PREFIX a: <http://e/a#>\n\na:z a:p 1 .\n\n# later\n\na:y a:p 1 .\n"
        );
    }

    #[test]
    fn the_output_prunes_to_itself() {
        // only directives: as written
        let src = "PREFIX a: <http://e/a#>\n\n# one\n\nPREFIX b: <http://e/b#>\n# the end\n";
        assert_eq!(pruned(src), src);
        // the header over the declaration printed first
        assert_eq!(
            pruned(
                "# about\nPREFIX a: <http://e/a#>\nPREFIX b: <http://e/b#>\n<http://e/s> <http://e/p> 1 ."
            ),
            "# about\nPREFIX a: <http://e/a#>\n\n<http://e/s> <http://e/p> 1 .\n"
        );
        assert_eq!(
            pruned("# about\n\nPREFIX a: <http://e/a#>\n<http://e/s> <http://e/p> 1 ."),
            "# about\n\n<http://e/s> <http://e/p> 1 .\n"
        );
        // a section header left alone joins the file header
        assert_eq!(
            pruned(
                "# about\n\nPREFIX a: <http://e/a#>\n\n# section\n\nPREFIX b: <http://e/b#>\n<http://e/s> <http://e/p> 1 ."
            ),
            "# about\n\n# section\n\n<http://e/s> <http://e/p> 1 .\n"
        );
    }
}
