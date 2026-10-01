//! Statements, predicate-object entries, objects, blank node property lists, annotation
//! blocks, collections and the RDF 1.2 terms.
//!
//! - A statement is **flat**, `s p o .`, when it has exactly one entry with one object,
//!   no comments inside it (its own leading and trailing comments are outside), and it
//!   fits. Otherwise it is **expanded**: the subject alone on its line, each entry on
//!   its own line one level deeper ending with ` ;`, and a lone `.` at the subject's
//!   indentation. A subject that is a blank node property list or a reified triple with
//!   no entries prints as itself and ` .`.
//! - With `turtle-layout = "conventional"` an expanded statement takes SPARQL's compact
//!   form instead: the subject and the first entry share a line, the other entries
//!   follow one level deeper, ` ;` ends every entry but the last and ` .` the last one.
//!   Comments before the `.` keep the lone `.` (and every ` ;`), and so does a trailing
//!   comment of the last entry when the statement has one too.
//! - The `a` entries come first in a subject block or a `[ … ]` block (N14,
//!   `type-shorthand`), each run between detached comment blocks on its own; the others
//!   keep their order. With `sort` the other entries and the objects of each list are
//!   sorted by printed form ([`crate::turtle::sort`]). When the statement has a trailing
//!   comment, the entry written last stays last rather than give way to one with a
//!   trailing comment: a comment after `; .` would not trail the statement any more.
//! - An object list stays on its entry's line when it fits; otherwise the predicate
//!   stays alone and the objects go one per line one level deeper, `,` after each but
//!   the last. Several `[ … ]` objects hug: `], [`.
//! - `[ … ]` and `{| … |}` stay inline only with a single entry with a single object that
//!   fits; otherwise the bracket ends the owning line, the entries follow one level
//!   deeper, each ending with ` ;` (but the last in the conventional layout), and the
//!   closing bracket comes back to the owning line's indentation. A collection stays
//!   inline when it fits; otherwise one item per line.
//! - Reified triples, triple terms and reifiers print inline with single spaces.

use super::{Tx, element, node, run_opening};
use crate::TurtleLayout;
use crate::doc::DocId;
use crate::lex::TokenKind;
use crate::normalize;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeId};
use crate::turtle::sort::{self, Placed};

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
    if entries.is_empty() {
        let dot = dot_doc(tx);
        if tx.comments.dangling(n).is_empty() {
            let sp = tx.space();
            return (tx.concat([subject, sp, dot]), true);
        }
        return (block(tx, n, subject, None, dot), true);
    }
    let order = entry_order(tx, &entries, true, has_trailing(tx, n));
    let last = order.last().map(|p| p.node);
    // the conventional layout ends the last entry with ` .`, unless comments must come
    // between them: dangling ones, or the last entry's trailing comment when the
    // statement has one too (both would end the line)
    let conventional = tx.opts.turtle_layout == TurtleLayout::Conventional
        && !tx
            .comments
            .dangling(n)
            .iter()
            .any(|&c| !tx.comments.copied(c))
        && !(last.is_some_and(|e| has_trailing(tx, e)) && has_trailing(tx, n));
    if !conventional {
        // the subject, then the entries one level deeper, with the lone `.` as the closer
        let items = entry_docs(tx, &order, Semi::Always);
        let lines = join(tx, &items, false);
        let dot = dot_doc(tx);
        return (block(tx, n, subject, Some(lines), dot), true);
    }
    // the subject and the first entry on one line (unless a comment comes before the
    // entry), the others one level deeper, then ` .`
    let items = entry_docs(tx, &order, Semi::Never);
    let (first, _, first_leading) = items[0];
    let head = match first_leading {
        true => {
            let hl = tx.hard_line();
            let d = tx.concat([hl, first]);
            tx.indent(d)
        }
        false => {
            let sp = tx.space();
            tx.concat([sp, first])
        }
    };
    let mut parts = vec![subject, head];
    if items.len() > 1 {
        let rest = join(tx, &items[1..], true);
        parts.push(tx.indent(rest));
    }
    parts.push(tx.space());
    parts.push(dot_doc(tx));
    let doc = tx.concat(parts);
    let multi = items.len() > 1 || first_leading || prints_on_lines(tx, doc, indent);
    (doc, multi)
}

/// Whether `d`, printed at `indent` columns, takes more than one line.
fn prints_on_lines(tx: &Tx<'_, '_>, d: DocId, indent: usize) -> bool {
    let width = usize::from(tx.opts.line_width).saturating_sub(indent);
    let width = u16::try_from(width).unwrap_or(u16::MAX);
    match crate::doc::print(&tx.arena, d, tx.tree.src, width, tx.opts.indent_width, None) {
        Ok(p) => p.text.contains(['\n', '\r']),
        Err(_) => true,
    }
}

/// The entries of a subject block or a `[ … ]` or `{| … |}` block in printing order,
/// within each run between detached comment blocks: the `a` (or `rdf:type`) entries
/// first and the others as written (N14, as written with `type-shorthand = false` or
/// `reorder` off); with `sort`, the `a` entries first and the others by printed
/// predicate, in every block. `closer_trailed`: the statement has a trailing comment,
/// which trails only while the entry before its `.` has none (both would end one line),
/// so the entry written last stays last when one with a trailing comment would end it.
fn entry_order(
    tx: &mut Tx<'_, '_>,
    entries: &[NodeId],
    reorder: bool,
    closer_trailed: bool,
) -> Vec<Placed> {
    let sorting = tx.opts.sort;
    let keys: Option<Vec<(bool, String)>> = match (sorting, reorder && tx.opts.type_shorthand) {
        (true, _) => Some(
            entries
                .iter()
                .map(|&e| (!is_type(tx, e), sort::printed_verb(tx, e)))
                .collect(),
        ),
        (false, true) => Some(
            entries
                .iter()
                .map(|&e| (!is_type(tx, e), String::new()))
                .collect(),
        ),
        (false, false) => None,
    };
    let order = sort::order(tx.comments, entries, keys.as_deref(), sorting, false);
    match closer_trailed && ends_with_moved_trailing(tx, &order, entries) {
        true => sort::order(tx.comments, entries, keys.as_deref(), sorting, true),
        false => order,
    }
}

/// The entries' documents in printing order, each ending with ` ;` but the last, which
/// ends as `last` says; with whether a blank line goes before each and whether it starts
/// with a comment. A run's detached comments and blank line stay at its start whichever
/// entry moves there; other blank lines are kept as written (not inside a sorted run).
fn entry_docs(tx: &mut Tx<'_, '_>, order: &[Placed], last: Semi) -> Vec<(DocId, bool, bool)> {
    let mut items = Vec::with_capacity(order.len());
    for (i, p) in order.iter().enumerate() {
        let opening = run_opening(tx, p);
        let leading = opening.is_some() || tx.has_leading(p.node);
        let semi = match i + 1 == order.len() {
            true => last,
            false => Semi::Always,
        };
        let mut doc = entry(tx, p.node, semi);
        if let Some(o) = opening {
            doc = tx.concat([o, doc]);
        }
        items.push((doc, p.blank, leading));
    }
    items
}

/// The documents one per line, a blank line before those that ask for one; before the
/// first only with `gap_first` (which also starts with its line break).
fn join(tx: &mut Tx<'_, '_>, items: &[(DocId, bool, bool)], gap_first: bool) -> DocId {
    let mut parts = Vec::with_capacity(items.len() * 2);
    for (i, &(doc, blank, _)) in items.iter().enumerate() {
        if i > 0 || gap_first {
            parts.push(match blank {
                true => tx.empty_line(),
                false => tx.hard_line(),
            });
        }
        parts.push(doc);
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
/// level deeper; several `[ … ]` objects hug (`], [`). With `sort`, by printed form.
fn object_list(tx: &mut Tx<'_, '_>, objects: &[NodeId]) -> DocId {
    let order = object_order(tx, objects);
    let moved = order.iter().zip(objects).any(|(p, &o)| p.node != o);
    let leading = order.iter().any(|p| tx.has_leading(p.node));
    // not when a comment ends an object before the last: it would end the line after
    // the next `[`, inside that block
    let hug = order.len() > 1
        && order.iter().enumerate().all(|(i, p)| {
            let o = p.node;
            matches!(tx.tree.children(o).first(), Some(&Element::Node(b))
                if tx.tree.kind(b) == NodeKind::BNodePropertyList
                    && (i + 1 == order.len() || !(has_trailing(tx, o) || has_trailing(tx, b))))
        });
    let mut docs = Vec::with_capacity(order.len());
    for (i, p) in order.iter().enumerate() {
        let opening = run_opening(tx, p);
        let doc = match moved {
            false => node(tx, p.node),
            true => moved_object(tx, p.node, i + 1 < order.len()),
        };
        docs.push(match opening {
            Some(o) => tx.concat([o, doc]),
            None => doc,
        });
    }
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

/// The objects in printing order: as written, or with `sort` by printed form within the
/// runs between detached comment blocks. A list holding an object under the ignore
/// pragma stays as written (its copied text holds its `,`). The object written last stays
/// last when another one with a trailing comment would end the list: printed before the
/// `;` or the `.` that follows the list, that comment would belong to the entry or the
/// statement the next time.
fn object_order(tx: &mut Tx<'_, '_>, objects: &[NodeId]) -> Vec<Placed> {
    if !tx.opts.sort || objects.len() < 2 || objects.iter().any(|&o| tx.comments.ignored(o)) {
        return sort::order::<()>(tx.comments, objects, None, false, false);
    }
    let keys: Vec<String> = objects
        .iter()
        .map(|&o| sort::printed_object(tx, o))
        .collect();
    let order = sort::order(tx.comments, objects, Some(&keys), true, false);
    match ends_with_moved_trailing(tx, &order, objects) {
        true => sort::order(tx.comments, objects, Some(&keys), true, true),
        false => order,
    }
}

/// Whether `order` ends with a node other than the last of `nodes` that has a trailing
/// comment.
fn ends_with_moved_trailing(tx: &Tx<'_, '_>, order: &[Placed], nodes: &[NodeId]) -> bool {
    order
        .last()
        .is_some_and(|p| nodes.last() != Some(&p.node) && has_trailing(tx, p.node))
}

/// An `Object` that moved, with its comments: the term, its reifiers and annotation
/// blocks, then a `,` when another object follows.
fn moved_object(tx: &mut Tx<'_, '_>, n: NodeId, comma: bool) -> DocId {
    let mut parts = Vec::new();
    let mut source_comma = None;
    for e in tx.children(n) {
        match e {
            Element::Token(t) if tx.tree.token_kind(t) == TokenKind::Comma => {
                source_comma = Some(t);
            }
            e => parts.push(element(tx, e)),
        }
    }
    let mut d = tx.spaced(parts);
    if comma {
        let c = match source_comma {
            Some(t) => tx.tok(t),
            None => tx.text(","),
        };
        d = tx.concat([d, c]);
    }
    tx.wrap(n, d)
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
    // the conventional layout drops the last entry's `;`
    let last = match tx.opts.turtle_layout {
        TurtleLayout::Diff => Semi::Always,
        TurtleLayout::Conventional => Semi::Never,
    };
    match entries.as_slice() {
        [] => block(tx, n, open, None, close),
        [e] if nodes_of(tx, *e, NodeKind::Object).len() == 1 => {
            let semi = match last {
                Semi::Always => Semi::IfBreak,
                _ => Semi::Never,
            };
            let d = entry(tx, *e, semi);
            tx.delimited(n, open, &[d], close, true)
        }
        _ => {
            let reorder = tx.tree.kind(n) == NodeKind::BNodePropertyList;
            let order = entry_order(tx, &entries, reorder, false);
            let items = entry_docs(tx, &order, last);
            let lines = join(tx, &items, false);
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
