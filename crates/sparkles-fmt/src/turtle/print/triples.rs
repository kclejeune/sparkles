//! Statements, predicate-object entries, objects, blank node property lists, annotation
//! blocks, collections and the RDF 1.2 terms.
//!
//! - A statement is **flat**, `s p o .`, when it has exactly one entry with one object,
//!   no comments inside it (its own leading and trailing comments are outside), and it
//!   fits. Otherwise it is **expanded**: the subject alone on its line, each entry on
//!   its own line one level deeper ending with ` ;`, and a lone `.` at the subject's
//!   indentation. A subject that is a blank node property list or a reified triple with
//!   no entries prints as itself and ` .`.
//! - The `a` entries come first in a subject block or a `[ … ]` block (N14,
//!   `type-shorthand`), each run between detached comment blocks on its own; the others
//!   keep their order.
//! - An object list stays on its entry's line when it fits; otherwise the predicate
//!   stays alone and the objects go one per line one level deeper, `,` after each but
//!   the last. Several `[ … ]` objects hug: `], [`.
//! - `[ … ]` and `{| … |}` stay inline only with a single entry with a single object that
//!   fits; otherwise the bracket ends the owning line, the entries follow one level
//!   deeper, each ending with ` ;`, and the closing bracket comes back to the owning
//!   line's indentation. A collection stays inline when it fits; otherwise one item per
//!   line.
//! - Reified triples, triple terms and reifiers print inline with single spaces.

use super::{Tx, element, node};
use crate::doc::DocId;
use crate::lex::TokenKind;
use crate::normalize;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeId};
use crate::trivia;

/// Whether an entry ends with ` ;`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Semi {
    /// in an expanded statement or block
    Always,
    /// in a flat statement
    Never,
    /// in a `[ … ]` or `{| … |}` with one entry: when the block breaks
    IfBreak,
}

/// `TriplesStmt` at `indent` columns, with its comments: its document and whether it
/// prints on several lines.
pub fn statement(tx: &mut Tx<'_, '_>, n: NodeId, indent: usize) -> (DocId, bool) {
    let dot = tx.child_token(n, TokenKind::Dot);
    if tx.comments.ignored(n) {
        let mut doc = tx.verbatim(n);
        if dot.is_none() {
            let d = tx.text(" .");
            doc = tx.concat([doc, d]);
        }
        let multi = tx.tree.text(n).contains(['\n', '\r']);
        return (tx.wrap(n, doc), multi);
    }
    let (doc, multi) = statement_body(tx, n, indent, dot);
    (tx.wrap(n, doc), multi)
}

fn statement_body(
    tx: &mut Tx<'_, '_>,
    n: NodeId,
    indent: usize,
    dot: Option<crate::tree::TokenId>,
) -> (DocId, bool) {
    let entries = nodes_of(tx, n, NodeKind::PropertyListEntry);
    let subject = tx.children(n).first().copied();
    let dot_doc = |tx: &mut Tx<'_, '_>| match dot {
        Some(t) => tx.tok(t),
        None => tx.text("."),
    };
    let single = match entries.as_slice() {
        [] => true,
        [e] => nodes_of(tx, *e, NodeKind::Object).len() == 1,
        _ => false,
    };
    if single && !tx.inner_comments(n) {
        let mut parts = Vec::new();
        if let Some(s) = subject {
            parts.push(element(tx, s));
        }
        if let Some(&e) = entries.first() {
            parts.push(entry(tx, e, Semi::Never));
        }
        parts.push(dot_doc(tx));
        let flat = tx.spaced(parts);
        let fits = tx
            .flat_width(flat)
            .filter(|&(w, _)| indent + w <= usize::from(tx.opts.line_width));
        match fits {
            Some((_, newline)) => return (flat, newline),
            // a subject alone breaks by itself
            None if entries.is_empty() => return (flat, true),
            None => {}
        }
    }
    let subject = match subject {
        Some(s) => element(tx, s),
        None => tx.nil(),
    };
    let lines = match entries.is_empty() {
        true => None,
        false => Some(entry_lines(tx, &entries, true)),
    };
    let dot = dot_doc(tx);
    if lines.is_none() && tx.comments.dangling(n).is_empty() {
        let sp = tx.space();
        return (tx.concat([subject, sp, dot]), true);
    }
    // the subject, then the entries one level deeper, with the lone `.` as the closer
    (block(tx, n, subject, lines, dot), true)
}

/// The entries one per line, each ending with ` ;`, in printing order: within each run
/// between detached comment blocks, the `a` (or `rdf:type`) entries first, the others in
/// source order (N14, as written with `type-shorthand = false` or `reorder` off). A
/// run's detached comments and blank line stay at its start whichever entry moves there;
/// other blank lines are kept as written.
fn entry_lines(tx: &mut Tx<'_, '_>, entries: &[NodeId], reorder: bool) -> DocId {
    let reorder = reorder && tx.opts.type_shorthand;
    let mut runs: Vec<Vec<NodeId>> = Vec::new();
    for &e in entries {
        match runs.last_mut() {
            Some(run) if tx.comments.detached_before(e).is_empty() => run.push(e),
            _ => runs.push(vec![e]),
        }
    }
    let mut parts = Vec::new();
    for run in runs {
        let head = run[0];
        let order: Vec<NodeId> = match reorder {
            true => {
                let (mut types, rest): (Vec<NodeId>, Vec<NodeId>) =
                    run.iter().partition(|&&e| is_type(tx, e));
                types.extend(rest);
                types
            }
            false => run,
        };
        let mut detached = Vec::new();
        if order[0] != head {
            for block in tx.comments.detached_before(head).to_vec() {
                for &c in &block {
                    tx.comments.mark_printed(c);
                }
                let cx = &mut tx.cx;
                detached.push(trivia::comment_lines(&mut cx.arena, cx.comments, &block));
                detached.push(tx.empty_line());
            }
        }
        for (i, &e) in order.iter().enumerate() {
            let mut doc = entry(tx, e, Semi::Always);
            if i == 0 && !detached.is_empty() {
                detached.push(doc);
                doc = tx.concat(std::mem::take(&mut detached));
            }
            if !parts.is_empty() {
                let blank = match i {
                    0 => tx.comments.blank_before(head),
                    _ => e != head && tx.comments.blank_before(e),
                };
                parts.push(match blank {
                    true => tx.empty_line(),
                    false => tx.hard_line(),
                });
            }
            parts.push(doc);
        }
    }
    tx.concat(parts)
}

/// Whether an entry's verb is `a` or `rdf:type`.
fn is_type(tx: &Tx<'_, '_>, e: NodeId) -> bool {
    match tx.tree.children(e).first() {
        Some(&Element::Token(t)) => {
            tx.tree.token_kind(t) == TokenKind::Kw(Kw::A)
                || normalize::is_rdf_type(tx.tree, t, &tx.scope)
        }
        _ => false,
    }
}

/// `open`, the lines one level deeper with the container's dangling comments after them,
/// then `close` on its own line; `open` and `close` together when both are empty.
fn block(tx: &mut Tx<'_, '_>, n: NodeId, open: DocId, lines: Option<DocId>, close: DocId) -> DocId {
    let dangling = tx.dangling(n, lines.is_some());
    if lines.is_none() && dangling.is_none() {
        return tx.concat([open, close]);
    }
    let mut inner = Vec::new();
    if let Some(l) = lines {
        inner.push(tx.hard_line());
        inner.push(l);
    }
    inner.extend(dangling);
    let inner = tx.concat(inner);
    let inner = tx.indent(inner);
    let hl = tx.hard_line();
    tx.concat([open, inner, hl, close])
}

/// `PropertyListEntry`, with its comments: the verb (`a` for `rdf:type`), the object
/// list, and ` ;` as `semi` says. A `;` is printed before the entry's trailing comment.
pub fn entry(tx: &mut Tx<'_, '_>, n: NodeId, semi: Semi) -> DocId {
    let source_semi = tx.child_token(n, TokenKind::Semicolon);
    let mut parts = Vec::new();
    if tx.comments.ignored(n) {
        parts.push(tx.verbatim(n));
        if source_semi.is_none() && semi == Semi::Always {
            parts.push(tx.text(" ;"));
        }
    } else {
        let verb = match tx.children(n).first() {
            Some(&Element::Token(t)) => tx.verb(t),
            _ => tx.nil(),
        };
        parts.push(verb);
        let objects = nodes_of(tx, n, NodeKind::Object);
        if !objects.is_empty() {
            parts.push(object_list(tx, &objects));
        }
        if semi != Semi::Never {
            let sp = tx.space();
            let s = match source_semi {
                Some(t) => tx.tok(t),
                None => tx.text(";"),
            };
            let s = tx.concat([sp, s]);
            parts.push(match semi {
                Semi::IfBreak => {
                    let nil = tx.nil();
                    tx.if_break(s, nil, None)
                }
                _ => s,
            });
        }
    }
    let doc = tx.concat(parts);
    tx.wrap(n, doc)
}

/// The objects after a verb, starting with the space or line break after it: one object
/// follows the verb; several stay on its line when they fit, else go one per line one
/// level deeper; several `[ … ]` objects hug (`], [`).
fn object_list(tx: &mut Tx<'_, '_>, objects: &[NodeId]) -> DocId {
    let leading = objects.iter().any(|&o| tx.has_leading(o));
    // not when a comment ends an object before the last: it would end the line after
    // the next `[`, inside that block
    let hug = objects.len() > 1
        && objects.iter().enumerate().all(|(i, &o)| {
            matches!(tx.tree.children(o).first(), Some(&Element::Node(b))
                if tx.tree.kind(b) == NodeKind::BNodePropertyList
                    && (i + 1 == objects.len() || !(has_trailing(tx, o) || has_trailing(tx, b))))
        });
    let docs: Vec<DocId> = objects.iter().map(|&o| node(tx, o)).collect();
    if !leading && (docs.len() == 1 || hug) {
        let mut parts = Vec::new();
        for d in docs {
            parts.push(tx.space());
            parts.push(d);
        }
        return tx.concat(parts);
    }
    let mut parts = Vec::new();
    for d in docs {
        parts.push(tx.line());
        parts.push(d);
    }
    let inner = tx.concat(parts);
    let inner = tx.indent(inner);
    tx.group(inner)
}

/// `Object`: the term, its reifiers and annotation blocks one space apart, then the `,`
/// when another object follows.
pub fn object(tx: &mut Tx<'_, '_>, n: NodeId) -> DocId {
    let mut parts = Vec::new();
    let mut comma = None;
    for e in tx.children(n) {
        match e {
            Element::Token(t) if tx.tree.token_kind(t) == TokenKind::Comma => {
                comma = Some(tx.tok(t));
            }
            e => parts.push(element(tx, e)),
        }
    }
    let d = tx.spaced(parts);
    match comma {
        Some(c) => tx.concat([d, c]),
        None => d,
    }
}

/// `BNodePropertyList` and `AnnotationBlock`: inline when they fit with a single entry
/// with a single object, otherwise the entries one per line one level deeper, each
/// ending with ` ;`. `[ ]` with a comment inside holds only that comment.
pub fn property_block(tx: &mut Tx<'_, '_>, n: NodeId, open: TokenKind, close: TokenKind) -> DocId {
    let entries = nodes_of(tx, n, NodeKind::PropertyListEntry);
    let open = bracket(tx, n, open);
    let close = bracket(tx, n, close);
    match entries.as_slice() {
        [] => block(tx, n, open, None, close),
        [e] if nodes_of(tx, *e, NodeKind::Object).len() == 1 => {
            let d = entry(tx, *e, Semi::IfBreak);
            tx.delimited(n, open, &[d], close, true)
        }
        _ => {
            let reorder = tx.tree.kind(n) == NodeKind::BNodePropertyList;
            let lines = entry_lines(tx, &entries, reorder);
            block(tx, n, open, Some(lines), close)
        }
    }
}

/// `Collection`: `( a b c )` when it fits, otherwise one item per line.
pub fn collection(tx: &mut Tx<'_, '_>, n: NodeId) -> DocId {
    let items: Vec<DocId> = nodes_of(tx, n, NodeKind::CollectionItem)
        .into_iter()
        .map(|i| node(tx, i))
        .collect();
    let open = bracket(tx, n, TokenKind::LParen);
    let close = bracket(tx, n, TokenKind::RParen);
    match items.is_empty() {
        // `(` `)` with a comment between
        true => block(tx, n, open, None, close),
        false => tx.delimited(n, open, &items, close, true),
    }
}

/// The children one space apart: a collection item's term, `<< s p o ~ r >>`,
/// `<<( s p o )>>`, `~ r`.
pub fn spaced(tx: &mut Tx<'_, '_>, n: NodeId) -> DocId {
    let parts: Vec<DocId> = tx.children(n).into_iter().map(|e| element(tx, e)).collect();
    tx.spaced(parts)
}

/// `n`'s child nodes of `kind`.
fn nodes_of(tx: &Tx<'_, '_>, n: NodeId, kind: NodeKind) -> Vec<NodeId> {
    tx.tree
        .child_nodes(n)
        .filter(|&c| tx.tree.kind(c) == kind)
        .collect()
}

/// `n`'s bracket token of `kind` (the parser always has it).
fn bracket(tx: &mut Tx<'_, '_>, n: NodeId, kind: TokenKind) -> DocId {
    match tx.child_token(n, kind) {
        Some(t) => tx.tok(t),
        None => tx.nil(),
    }
}

/// Whether `n` has a trailing comment still to print.
fn has_trailing(tx: &Tx<'_, '_>, n: NodeId) -> bool {
    tx.comments
        .trailing(n)
        .iter()
        .any(|&c| !tx.comments.copied(c))
}
