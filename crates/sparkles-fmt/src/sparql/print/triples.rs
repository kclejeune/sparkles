//! Triples: subject blocks in the compact form, object lists, `[ … ]` blocks,
//! collections, and the RDF 1.2 reified triples, triple terms, reifiers and annotation
//! blocks.
//!
//! - A statement with one predicate prints on one line when it fits: `?s ex:p ?o .`.
//!   With more, the subject and the first entry share the first line, each further
//!   entry goes on its own line one level deeper, `;` ends every entry but the last and
//!   ` .` the statement. A trailing or doubled `;` in the input is dropped.
//! - An object list stays on the entry's line when it fits; otherwise the objects go one
//!   per line two levels deeper than the entry, each but the last followed by `,`.
//!   Several `[ … ]` objects hug instead: `], [`.
//! - `[ … ]` and `{| … |}` stay inline only with a single entry with a single object
//!   that fits; otherwise the bracket ends the owning line, the entries follow one level
//!   deeper, and the closing bracket comes back to the owning line's indentation.
//! - A collection stays inline when it fits; otherwise one item per line.
//! - Reified triples, triple terms and reifiers print inline with single spaces.

use super::{Ctx, term};
use crate::doc::DocId;
use crate::lex::TokenKind;
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeId};

/// `TriplesStmt`: the subject, its entries in the compact form, then ` .`.
pub fn triples_stmt(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let children = cx.children(n);
    let entries = nodes_of(cx, n, NodeKind::PropertyListEntry);
    let subject = match children.first() {
        Some(&e) => term::element(cx, e),
        None => cx.nil(),
    };
    let dot = match cx.child_token(n, TokenKind::Dot) {
        Some(t) => cx.tok(t),
        None => cx.text("."),
    };
    let sp = cx.space();
    let Some((&first, rest)) = entries.split_first() else {
        // a blank node property list, a collection or a reified triple alone
        return cx.concat([subject, sp, dot]);
    };
    let first_leading = cx.has_leading(first);
    let first = cx.node(first);
    let head = if first_leading {
        let hl = cx.hard_line();
        let d = cx.concat([hl, first]);
        cx.indent(d)
    } else {
        let sp = cx.space();
        cx.concat([sp, first])
    };
    if rest.is_empty() {
        let line = cx.concat([subject, head, sp, dot]);
        return cx.group(line);
    }
    let items: Vec<(NodeId, DocId)> = rest.iter().map(|&e| (e, cx.node(e))).collect();
    let hl = cx.hard_line();
    let stack = cx.stack(&items);
    let more = cx.concat([hl, stack]);
    let more = cx.indent(more);
    cx.concat([subject, head, more, sp, dot])
}

/// `PropertyListEntry`: the verb and the object list, then ` ;` when another entry
/// follows in the same list.
pub fn property_list_entry(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let verb = match cx.children(n).first() {
        Some(&Element::Token(t)) => cx.verb(t),
        Some(&Element::Node(v)) if cx.tree.kind(v) != NodeKind::Object => cx.node(v),
        _ => cx.nil(),
    };
    let objects = nodes_of(cx, n, NodeKind::Object);
    let list = object_list(cx, &objects);
    let mut parts = vec![verb, list];
    if let Some(semi) = cx.child_token(n, TokenKind::Semicolon)
        && has_later_entry(cx, n)
    {
        parts.push(cx.space());
        parts.push(cx.tok(semi));
    }
    cx.concat(parts)
}

/// The objects after a verb: inline when they fit, else one per line two levels deeper;
/// several `[ … ]` objects hug (`], [`), and a single object follows the verb directly.
fn object_list(cx: &mut Ctx<'_, '_>, objects: &[NodeId]) -> DocId {
    let leading = objects.iter().any(|&o| cx.has_leading(o));
    let hug = objects.len() > 1
        && objects.iter().all(|&o| {
            matches!(cx.children(o).first(), Some(&Element::Node(b))
                if cx.tree.kind(b) == NodeKind::BNodePropertyList)
        });
    let docs: Vec<DocId> = objects.iter().map(|&o| cx.node(o)).collect();
    if (docs.len() == 1 && !leading) || (hug && !leading) {
        let mut parts = Vec::new();
        for d in docs {
            parts.push(cx.space());
            parts.push(d);
        }
        return cx.concat(parts);
    }
    let mut parts = Vec::new();
    for d in docs {
        parts.push(cx.line());
        parts.push(d);
    }
    let inner = cx.concat(parts);
    let inner = cx.indent(inner);
    let inner = cx.indent(inner);
    cx.group(inner)
}

/// `Object`: the term, its reifiers and annotation blocks one space apart, then the `,`
/// when another object follows.
pub fn object(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let mut parts = Vec::new();
    let mut comma = None;
    for e in cx.children(n) {
        match e {
            Element::Token(t) if cx.tree.token_kind(t) == TokenKind::Comma => {
                comma = Some(cx.tok(t));
            }
            e => parts.push(term::element(cx, e)),
        }
    }
    let d = cx.spaced(parts);
    match comma {
        Some(c) => cx.concat([d, c]),
        None => d,
    }
}

/// `BNodePropertyList`: `[ p o ]` when it fits with a single entry with a single object,
/// otherwise expanded.
pub fn bnode_property_list(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    property_block(cx, n, TokenKind::LBracket, TokenKind::RBracket)
}

/// `AnnotationBlock`: `{| p o |}`, as [`bnode_property_list`].
pub fn annotation_block(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    property_block(cx, n, TokenKind::LBracePipe, TokenKind::PipeRBrace)
}

fn property_block(cx: &mut Ctx<'_, '_>, n: NodeId, open: TokenKind, close: TokenKind) -> DocId {
    let entries = nodes_of(cx, n, NodeKind::PropertyListEntry);
    let open = bracket(cx, n, open);
    let close = bracket(cx, n, close);
    let single = entries.len() == 1 && nodes_of(cx, entries[0], NodeKind::Object).len() == 1;
    if single {
        let d = cx.node(entries[0]);
        return cx.delimited(n, open, &[d], close, true);
    }
    let items: Vec<(NodeId, DocId)> = entries.iter().map(|&e| (e, cx.node(e))).collect();
    cx.block(n, open, &items, close)
}

/// `Collection`: `( a b c )` when it fits, otherwise one item per line.
pub fn collection(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let items: Vec<DocId> = nodes_of(cx, n, NodeKind::CollectionItem)
        .into_iter()
        .map(|i| cx.node(i))
        .collect();
    let open = bracket(cx, n, TokenKind::LParen);
    let close = bracket(cx, n, TokenKind::RParen);
    cx.delimited(n, open, &items, close, true)
}

/// `CollectionItem`: its term.
pub fn collection_item(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    spaced(cx, n)
}

/// `ReifiedTriple`: `<< s p o ~ r >>`.
pub fn reified_triple(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    spaced(cx, n)
}

/// `TripleTerm`: `<<( s p o )>>`.
pub fn triple_term(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    spaced(cx, n)
}

/// `Reifier`: `~ r`, or `~` alone.
pub fn reifier(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    spaced(cx, n)
}

/// The children one space apart.
fn spaced(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let parts: Vec<DocId> = cx
        .children(n)
        .into_iter()
        .map(|e| term::element(cx, e))
        .collect();
    cx.spaced(parts)
}

/// `n`'s child nodes of `kind`.
fn nodes_of(cx: &Ctx<'_, '_>, n: NodeId, kind: NodeKind) -> Vec<NodeId> {
    cx.tree
        .child_nodes(n)
        .filter(|&c| cx.tree.kind(c) == kind)
        .collect()
}

/// `n`'s bracket token of `kind` (the parser always has it).
fn bracket(cx: &mut Ctx<'_, '_>, n: NodeId, kind: TokenKind) -> DocId {
    match cx.child_token(n, kind) {
        Some(t) => cx.tok(t),
        None => cx.nil(),
    }
}

/// Whether another entry follows `entry` in its list.
fn has_later_entry(cx: &Ctx<'_, '_>, entry: NodeId) -> bool {
    let Some(parent) = cx.tree.parent(entry) else {
        return false;
    };
    let siblings: Vec<NodeId> = nodes_of(cx, parent, NodeKind::PropertyListEntry);
    siblings.last() != Some(&entry)
}

#[cfg(test)]
mod tests {
    use super::super::pattern::tests::{P, print, print_with};
    use crate::Options;
    use crate::syntax::NodeKind;

    fn stmt_with(triples: &str, opts: &Options) -> String {
        let src = format!(
            "{P}PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
             PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT * {{ {triples} }}"
        );
        print_with(&src, NodeKind::TriplesStmt, opts)
    }

    fn stmt(triples: &str) -> String {
        stmt_with(triples, &Options::default())
    }

    fn narrow() -> Options {
        Options {
            line_width: 40,
            ..Options::default()
        }
    }

    #[test]
    fn subject_blocks() {
        assert_eq!(stmt("?s ex:p ?o"), "?s ex:p ?o .");
        assert_eq!(
            stmt("?s a ex:C; ex:p 1 ;; ex:q 2 ;"),
            "?s a ex:C ;\n  ex:p 1 ;\n  ex:q 2 ."
        );
        assert_eq!(stmt("?s ex:p 1, 2,3"), "?s ex:p 1, 2, 3 .");
        assert_eq!(
            stmt_with("?s ex:p ex:objectNumberOne, ex:objectNumberTwo", &narrow()),
            "?s ex:p\n    ex:objectNumberOne,\n    ex:objectNumberTwo ."
        );
        assert_eq!(
            stmt_with(
                "?s ex:a 1 ; ex:p ex:objectNumberOne, ex:objectNumberTwo",
                &narrow()
            ),
            "?s ex:a 1 ;\n  ex:p\n      ex:objectNumberOne,\n      ex:objectNumberTwo ."
        );
        assert_eq!(stmt("$this ex:p ?o"), "$this ex:p ?o .");
    }

    #[test]
    fn comments_in_subject_blocks() {
        assert_eq!(
            stmt("?s ex:p 1 ; # one\n # two\n ex:q 2, # a\n 3 ."),
            "?s ex:p 1 ; # one\n  # two\n  ex:q\n      2, # a\n      3 ."
        );
    }

    #[test]
    fn normalized_terms() {
        assert_eq!(
            stmt(
                "<http://example.org/s> rdf:type <http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"
            ),
            "ex:s a rdf:type ."
        );
        assert_eq!(
            stmt_with(
                "?s rdf:type ?o",
                &Options {
                    type_shorthand: false,
                    compact_iris: false,
                    ..Options::default()
                }
            ),
            "?s rdf:type ?o ."
        );
        assert_eq!(
            stmt("?s ex:p \"1\"^^xsd:integer, '2.5'^^xsd:decimal, \"1\"^^xsd:decimal, \"x\"@EN-gb"),
            "?s ex:p 1, 2.5, \"1\"^^xsd:decimal, \"x\"@EN-gb ."
        );
        assert_eq!(stmt("?s ex:p ( ), [ ]"), "?s ex:p (), [] .");
    }

    #[test]
    fn brackets() {
        assert_eq!(stmt("?s ex:p [ ex:q 1 ]"), "?s ex:p [ ex:q 1 ] .");
        assert_eq!(
            stmt("?s ex:p [ ex:q 1 ; ex:r 2 ]"),
            "?s ex:p [\n  ex:q 1 ;\n  ex:r 2\n] ."
        );
        assert_eq!(
            stmt("?s ex:p [ ex:q 1 ; ex:r 2 ], [ ex:t 3 ]"),
            "?s ex:p [\n  ex:q 1 ;\n  ex:r 2\n], [ ex:t 3 ] ."
        );
        assert_eq!(stmt("[ ex:q 1 ]"), "[ ex:q 1 ] .");
        assert_eq!(stmt("( 1 ?x ) ex:p (2)"), "( 1 ?x ) ex:p ( 2 ) .");
        assert_eq!(
            stmt_with("?s ex:p ( ex:itemNumberOne ex:itemNumberTwo )", &narrow()),
            "?s ex:p (\n  ex:itemNumberOne\n  ex:itemNumberTwo\n) ."
        );
    }

    #[test]
    fn rdf_12_terms() {
        assert_eq!(
            stmt("?s ex:p ?o ~?r {|ex:a 1|}"),
            "?s ex:p ?o ~ ?r {| ex:a 1 |} ."
        );
        assert_eq!(
            stmt("<<?s ex:p ?o ~ ex:r>> ex:q <<( ex:a ex:b ex:c )>>"),
            "<< ?s ex:p ?o ~ ex:r >> ex:q <<( ex:a ex:b ex:c )>> ."
        );
        assert_eq!(stmt("<< ex:a ex:b ex:c >>"), "<< ex:a ex:b ex:c >> .");
        assert_eq!(
            stmt("?s ex:p ?o ~ {| ex:a 1; ex:b 2 |}"),
            "?s ex:p ?o ~ {|\n  ex:a 1 ;\n  ex:b 2\n|} ."
        );
    }

    #[test]
    fn paths_are_tight() {
        let src = format!(
            "{P}SELECT * {{ ?s ( ex:a | ^<http://example.org/b> ) + / ! ( a | ^ ex:c ) ? ?o }}"
        );
        assert_eq!(
            print(&src, NodeKind::TriplesStmt),
            "?s (ex:a|^ex:b)+/!(a|^ex:c)? ?o ."
        );
    }
}
