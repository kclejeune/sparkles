//! Opt-in sorting of Turtle and TriG (`sort`): statements by printed subject within the
//! runs between sort barriers (detached comment blocks and directives), the entries of a
//! block by printed predicate after the `a` entries, objects by printed form; TriG graph
//! blocks by printed name, the default graph first. A node moves with its leading
//! comments and its trailing comment.
//!
//! - **Printed form.** A node's key is its text as the printer prints it, on one line
//!   and without comments: a second printer over the same tree with no comments builds
//!   the node's document (with every normalization and the sorting inside it), and
//!   [`flat_text`] reads it flat. A node under `# sparkles-fmt: ignore` sorts by its
//!   source text. Strings compare by codepoint; equal keys keep their order (a stable
//!   sort), so the output sorts to itself.
//! - **Runs.** A detached comment block starts a run and stays at its start whichever
//!   node sorts first there; directives end a run and never move. Inside a sorted run the
//!   formatter owns the blank lines: only the "multi-line neighbor" rule adds one.
//! - **Commas.** An object list holding an object under the ignore pragma keeps its
//!   order (the copied text holds its comma), and so does one that would end with an
//!   object carrying a trailing comment: printed before the entry's `;` or the
//!   statement's `.`, that comment would belong to the entry or the statement the next
//!   time.

use super::print::{Tx, element};
use crate::doc::{Doc, DocId};
use crate::lex::TokenKind;
use crate::tree::{Element, NodeId};
use crate::trivia::Comments;

/// Whether `sort` acts on Turtle and TriG.
pub const IMPLEMENTED: bool = true;

/// A node in printing order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub node: NodeId,
    /// For the first node of a run: the run's first node as written, whose detached
    /// comment blocks open the run (it may have moved further down).
    pub run_head: Option<NodeId>,
    /// Whether a blank line goes before it: for the first node of a run, as written
    /// before the run; later, as written before the node unless the run is sorted or the
    /// node moved.
    pub blank: bool,
}

/// `nodes` in printing order: cut into runs at each node with a detached comment block,
/// each run ordered by `keys` (a stable sort; `None` keeps the order as written), the
/// last node staying last with `pin_last`. `sorted`: the formatter owns the blank lines
/// inside a run.
pub fn order<K: Ord>(
    comments: &Comments,
    nodes: &[NodeId],
    keys: Option<&[K]>,
    sorted: bool,
    pin_last: bool,
) -> Vec<Placed> {
    let mut out = Vec::with_capacity(nodes.len());
    let mut start = 0;
    while start < nodes.len() {
        let mut end = start + 1;
        while end < nodes.len() && comments.detached_before(nodes[end]).is_empty() {
            end += 1;
        }
        let mut run: Vec<usize> = (start..end).collect();
        if let Some(keys) = keys {
            let moving = match pin_last && end == nodes.len() {
                true => &mut run[..end - start - 1],
                false => &mut run[..],
            };
            moving.sort_by(|&a, &b| keys[a].cmp(&keys[b]));
        }
        let head = nodes[start];
        for (k, &i) in run.iter().enumerate() {
            let node = nodes[i];
            out.push(match k {
                0 => Placed {
                    node,
                    run_head: Some(head),
                    blank: comments.blank_before(head),
                },
                _ => Placed {
                    node,
                    run_head: None,
                    blank: !sorted && node != head && comments.blank_before(node),
                },
            });
        }
        start = end;
    }
    out
}

/// The printed form of `e`, for sorting: its document on one line, without comments.
pub fn printed(tx: &mut Tx<'_, '_>, e: Element) -> String {
    if let Element::Node(n) = e
        && tx.comments.ignored(n)
    {
        return tx.tree.text(n).to_string();
    }
    match tx.keys.as_deref_mut() {
        Some(k) => {
            let d = element(k, e);
            flat_text(k, d)
        }
        None => {
            let d = element(tx, e);
            flat_text(tx, d)
        }
    }
}

/// The printed form of an `Object` without its `,`.
pub fn printed_object(tx: &mut Tx<'_, '_>, o: NodeId) -> String {
    if tx.comments.ignored(o) {
        return tx.tree.text(o).to_string();
    }
    let k: &mut Tx<'_, '_> = match tx.keys.as_deref_mut() {
        Some(k) => k,
        None => tx,
    };
    let tree = k.tree;
    let parts: Vec<DocId> = k
        .children(o)
        .into_iter()
        .filter(|e| !matches!(*e, Element::Token(t) if tree.token_kind(t) == TokenKind::Comma))
        .map(|e| element(k, e))
        .collect();
    let d = k.spaced(parts);
    flat_text(k, d)
}

/// The printed form of an entry's verb (`a` for `rdf:type`).
pub fn printed_verb(tx: &mut Tx<'_, '_>, e: NodeId) -> String {
    if tx.comments.ignored(e) {
        return tx.tree.text(e).to_string();
    }
    let k: &mut Tx<'_, '_> = match tx.keys.as_deref_mut() {
        Some(k) => k,
        None => tx,
    };
    let d = match k.children(e).first() {
        Some(&Element::Token(t)) => k.verb(t),
        _ => k.nil(),
    };
    flat_text(k, d)
}

/// Document `d` read flat: line breaks as single spaces, the flat branch of every
/// `IfBreak`, without line suffixes (trailing comments).
pub fn flat_text(tx: &Tx<'_, '_>, d: DocId) -> String {
    let arena = &tx.arena;
    let src = tx.tree.src;
    let mut out = String::new();
    let mut stack = vec![d];
    while let Some(d) = stack.pop() {
        match arena.get(d) {
            Doc::Nil | Doc::SoftLine | Doc::LineSuffix(_) | Doc::BreakParent => {}
            Doc::Line | Doc::HardLine | Doc::EmptyLine => out.push(' '),
            Doc::Text(s) => out.push_str(s),
            Doc::Token { id, printed } => out.push_str(match printed {
                Some(p) => p,
                None => arena.tokens()[id.0 as usize].text(src),
            }),
            Doc::Verbatim(r) => out.push_str(&src[r.clone()]),
            Doc::Indent(x) | Doc::Group { doc: x, .. } => stack.push(*x),
            Doc::IfBreak { flat, .. } => stack.push(*flat),
            Doc::Concat(ds) => stack.extend(ds.iter().rev()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::Turtle;
    use crate::{Options, TurtleLayout};

    fn sorted() -> Options {
        Options {
            sort: true,
            ..Options::default()
        }
    }

    fn fmt_with(src: &str, trig: bool, opts: &Options) -> String {
        let out = crate::check::run(&Turtle { trig }, src, opts)
            .unwrap_or_else(|e| panic!("{src:?}: {e}"))
            .text;
        let again = crate::check::run(&Turtle { trig }, &out, opts)
            .unwrap()
            .text;
        assert_eq!(again, out, "not a fixpoint");
        out
    }

    fn fmt(src: &str) -> String {
        fmt_with(src, false, &sorted())
    }

    const P: &str = "PREFIX ex: <http://example.org/>\n\n";

    #[test]
    fn statements_entries_and_objects() {
        assert_eq!(
            fmt(&format!(
                "{P}ex:c ex:p 1 .\nex:b ex:q 3, 1, 2 ; ex:p 2 ; a ex:C .\nex:a ex:p 1 ."
            )),
            format!(
                "{P}ex:a ex:p 1 .\n\nex:b\n  a ex:C ;\n  ex:p 2 ;\n  ex:q 1, 2, 3 ;\n.\n\nex:c ex:p 1 .\n"
            )
        );
        // equal subjects keep their order; blank lines inside a run are the formatter's
        assert_eq!(
            fmt(&format!("{P}ex:b ex:p 2 .\n\nex:a ex:p 2 .\nex:b ex:p 1 .")),
            format!("{P}ex:a ex:p 2 .\nex:b ex:p 2 .\nex:b ex:p 1 .\n")
        );
        // printed forms: compacted IRIs, `a`, literal shorthands
        assert_eq!(
            fmt(&format!(
                "{P}<http://example.org/z> ex:p 1 . ex:y ex:p '\"x' , \"1\"^^<http://www.w3.org/2001/XMLSchema#integer>, 0 ."
            )),
            format!("{P}ex:y\n  ex:p '\"x', 0, 1 ;\n.\n\nex:z ex:p 1 .\n")
        );
        // blank node property lists sort inside and by their printed form
        assert_eq!(
            fmt(&format!("{P}ex:s ex:p [ ex:b 1 ; ex:a 2 ], [ ex:a 1 ] .")),
            format!("{P}ex:s\n  ex:p [ ex:a 1 ], [\n    ex:a 2 ;\n    ex:b 1 ;\n  ] ;\n.\n")
        );
    }

    #[test]
    fn comments_move_with_their_nodes() {
        // leading and trailing comments travel; a detached block is a barrier
        assert_eq!(
            fmt(&format!(
                "{P}# zeta\nex:z ex:p 1 . # z\nex:y ex:p 1 .\n\n# section\n\nex:x ex:p 1 .\nex:w ex:p 1 ."
            )),
            format!(
                "{P}ex:y ex:p 1 .\n# zeta\nex:z ex:p 1 . # z\n\n# section\n\nex:w ex:p 1 .\nex:x ex:p 1 .\n"
            )
        );
        // an object list that would end with a commented object keeps its order
        assert_eq!(
            fmt(&format!("{P}ex:s ex:p 2, # two\n 1 .")),
            format!("{P}ex:s\n  ex:p\n    2, # two\n    1 ;\n.\n")
        );
        assert_eq!(
            fmt(&format!("{P}ex:s ex:p 3, 1, # one\n 2 .")),
            format!("{P}ex:s\n  ex:p\n    1, # one\n    2,\n    3 ;\n.\n")
        );
        // entries: the trailing comment stays with its entry
        assert_eq!(
            fmt(&format!("{P}ex:s ex:q 1 ; # q\n ex:p 2 .")),
            format!("{P}ex:s\n  ex:p 2 ;\n  ex:q 1 ; # q\n.\n")
        );
        // unless the statement has a trailing comment too: the entry before the `.`
        // stays last, or both comments would end its line
        assert_eq!(
            fmt(&format!(
                "{P}ex:s ex:r 1 ; # r\n ex:q 3 ; ex:p 2 ;\n. # end"
            )),
            format!("{P}ex:s\n  ex:q 3 ;\n  ex:r 1 ; # r\n  ex:p 2 ;\n. # end\n")
        );
        // the same for the `a` entries first, without sorting
        assert_eq!(
            fmt_with(
                &format!("{P}ex:s ex:p 1 ; # p\n a ex:C ;\n. # end"),
                false,
                &Options::default()
            ),
            format!("{P}ex:s\n  ex:p 1 ; # p\n  a ex:C ;\n. # end\n")
        );
    }

    #[test]
    fn ignored_nodes_sort_by_their_source_text() {
        assert_eq!(
            fmt(&format!(
                "{P}ex:b ex:p 1 .\n# sparkles-fmt: ignore\nex:a   ex:p   1 .\nex:a ex:p 2 ."
            )),
            format!("{P}ex:a ex:p 2 .\n# sparkles-fmt: ignore\nex:a   ex:p   1 .\nex:b ex:p 1 .\n")
        );
        // an object list with an ignored object keeps its order
        assert_eq!(
            fmt(&format!("{P}ex:s ex:p 2,\n  # sparkles-fmt: ignore\n  1 .")),
            format!("{P}ex:s\n  ex:p\n    2,\n    # sparkles-fmt: ignore\n    1 ;\n.\n")
        );
    }

    #[test]
    fn graph_blocks_sort_by_name_the_default_graph_first() {
        let src = "PREFIX ex: <http://example.org/>\nex:g2 { ex:b ex:p 1 . ex:a ex:p 1 } ex:z ex:p 1 . ex:g1 { ex:a ex:p 1 } { ex:c ex:p 1 } ex:y ex:p 1 .";
        assert_eq!(
            fmt_with(src, true, &sorted()),
            "PREFIX ex: <http://example.org/>\n\nex:y ex:p 1 .\nex:z ex:p 1 .\n\n{\n  ex:c ex:p 1 .\n}\n\nGRAPH ex:g1 {\n  ex:a ex:p 1 .\n}\n\nGRAPH ex:g2 {\n  ex:a ex:p 1 .\n  ex:b ex:p 1 .\n}\n"
        );
    }

    #[test]
    fn directives_are_barriers() {
        assert_eq!(
            fmt(
                "PREFIX ex: <http://example.org/>\nex:b ex:p 1 .\nex:a ex:p 1 .\nPREFIX ex: <http://example.com/>\nex:d ex:p 1 .\nex:c ex:p 1 ."
            ),
            "PREFIX ex: <http://example.org/>\n\nex:a ex:p 1 .\nex:b ex:p 1 .\nPREFIX ex: <http://example.com/>\n\nex:c ex:p 1 .\nex:d ex:p 1 .\n"
        );
    }

    #[test]
    fn with_the_conventional_layout() {
        let opts = Options {
            sort: true,
            turtle_layout: TurtleLayout::Conventional,
            ..Options::default()
        };
        assert_eq!(
            fmt_with(
                &format!(
                    "{P}ex:s ex:q 1 ; # q\n ex:p 2 ; a ex:C .\nex:r ex:p [ ex:b 1 ; ex:a 2 ] ."
                ),
                false,
                &opts
            ),
            format!(
                "{P}ex:r ex:p [\n  ex:a 2 ;\n  ex:b 1\n] .\n\nex:s a ex:C ;\n  ex:p 2 ;\n  ex:q 1 . # q\n"
            )
        );
    }
}
