//! Update requests: operations separated by `;` and a blank line, `WITH`/`USING`
//! lines, `DELETE {`/`INSERT {`/`WHERE {` blocks, quad data, one-line operations.
//!
//! Every keyword and graph reference of an operation is printed as written (in the
//! grammar's spelling): `SILENT`, `INTO GRAPH`, `DEFAULT`, `NAMED`, `ALL` and the
//! optional `GRAPH` of `ADD`/`MOVE`/`COPY` are never added or dropped.

use super::Ctx;
use crate::doc::DocId;
use crate::lex::TokenKind;
use crate::tree::{Element, NodeId};

/// `UpdateUnit`: the file header, then the prologues and operations with a blank line
/// between each two, an operation's `;` right after its last token (`};`), the
/// comments at the end of the file and one final newline. A `;` that ends the request
/// is dropped.
pub fn update_unit(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let mut parts = Vec::new();
    parts.extend(cx.header());
    let children = cx.children(n);
    let mut any = false;
    for (i, &e) in children.iter().enumerate() {
        match e {
            Element::Token(t) if cx.tree.token_kind(t) == TokenKind::Semicolon => {
                // a separator only when something follows it
                if i + 1 < children.len() {
                    parts.push(cx.tok(t));
                }
            }
            e => {
                if any {
                    parts.push(cx.empty_line());
                }
                parts.push(cx.element(e));
                any = true;
            }
        }
    }
    parts.extend(cx.dangling(n, any));
    parts.push(cx.hard_line());
    cx.concat(parts)
}

/// `LoadOp`: `LOAD [SILENT] <x> [INTO GRAPH <g>]`.
pub fn load_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `ClearOp`: `CLEAR [SILENT] GRAPH <g>`, `CLEAR DEFAULT|NAMED|ALL`.
pub fn clear_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `DropOp`: as [`clear_op`].
pub fn drop_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `CreateOp`: `CREATE [SILENT] GRAPH <g>`.
pub fn create_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `AddOp`: `ADD [SILENT] a TO b`.
pub fn add_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `MoveOp`: as [`add_op`].
pub fn move_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `CopyOp`: as [`add_op`].
pub fn copy_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `InsertDataOp`: `INSERT DATA {`, the quads, `}`.
pub fn insert_data_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `DeleteDataOp`: `DELETE DATA {`, the quads, `}`.
pub fn delete_data_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `DeleteWhereOp`: `DELETE WHERE {`, the quads, `}`.
pub fn delete_where_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `ModifyOp`: `WITH <g>`, the `DELETE {` and `INSERT {` blocks, each `USING [NAMED]
/// <g>` and the `WHERE {` block, each starting a line.
pub fn modify_op(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let mut parts = Vec::new();
    for c in cx.child_nodes(n) {
        if !parts.is_empty() {
            parts.push(cx.hard_line());
        }
        parts.push(cx.node(c));
    }
    cx.concat(parts)
}

/// `WithClause`: `WITH <g>`.
pub fn with_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `DeleteClause`: `DELETE {`, the quads, `}`.
pub fn delete_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `InsertClause`: `INSERT {`, the quads, `}`.
pub fn insert_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `UsingClause`: `USING [NAMED] <g>`.
pub fn using_clause(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    cx.words(n)
}

/// `QuadPattern`: always expanded, `{`, the triples statements and `GRAPH` blocks one
/// per line, `}`; `{}` when empty. A `.` after a `GRAPH` block is dropped.
pub fn quad_pattern(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    braced(cx, n)
}

/// `QuadsGraph`: `GRAPH <g> {`, the triples statements, `}`.
pub fn quads_graph(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let mut head = Vec::new();
    for e in cx.children(n) {
        match e {
            Element::Token(t) if cx.tree.token_kind(t) == TokenKind::LBrace => break,
            e => head.push(cx.element(e)),
        }
    }
    let block = braced(cx, n);
    head.push(block);
    cx.spaced(head)
}

/// The `{ … }` of `n`: its child nodes as an always expanded block.
fn braced(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    let open = cx.child_token(n, TokenKind::LBrace);
    let close = cx.child_token(n, TokenKind::RBrace);
    let (Some(open), Some(close)) = (open, close) else {
        return cx.verbatim(n);
    };
    let items: Vec<(NodeId, DocId)> = cx
        .child_nodes(n)
        .into_iter()
        .map(|c| (c, cx.node(c)))
        .collect();
    let open = cx.tok(open);
    let close = cx.tok(close);
    cx.block(n, open, &items, close)
}

#[cfg(test)]
mod tests {
    use crate::{Language, Options, format};

    fn fmt(src: &str) -> String {
        format(src, Language::Sparql, &Options::default())
            .unwrap_or_else(|e| panic!("{src}: {e}"))
            .text
    }

    #[test]
    fn one_line_operations_keep_their_keywords() {
        assert_eq!(
            fmt("load silent <x> into graph <g>; clear  default ;drop all;\
                 create silent graph <g>; add silent default to graph <g>;\
                 move <a> to default; copy graph <a> to <b> ;"),
            "LOAD SILENT <x> INTO GRAPH <g>;

CLEAR DEFAULT;

DROP ALL;

CREATE SILENT GRAPH <g>;

ADD SILENT DEFAULT TO GRAPH <g>;

MOVE <a> TO DEFAULT;

COPY GRAPH <a> TO <b>
"
        );
    }

    #[test]
    fn quad_blocks_are_expanded() {
        assert_eq!(
            fmt("insert data { <a> <b> <c> . graph <g> { <a> <b> <c> . } . graph <h> {} }"),
            "INSERT DATA {
  <a> <b> <c> .
  GRAPH <g> {
    <a> <b> <c> .
  }
  GRAPH <h> {}
}
"
        );
        assert_eq!(fmt("DELETE DATA{}"), "DELETE DATA {}\n");
        assert_eq!(
            fmt("delete where {?s ?p ?o .}"),
            "DELETE WHERE {\n  ?s ?p ?o .\n}\n"
        );
    }

    #[test]
    fn modify_clauses_start_lines() {
        assert_eq!(
            fmt(
                "with <g> delete { ?s ?p ?o . } insert { ?s ?p 1 . } using <u> using named <v> WHERE {\n  ?s ?p ?o .\n}"
            ),
            "WITH <g>
DELETE {
  ?s ?p ?o .
}
INSERT {
  ?s ?p 1 .
}
USING <u>
USING NAMED <v>
WHERE {
  ?s ?p ?o .
}
"
        );
    }

    #[test]
    fn prologues_and_separators() {
        assert_eq!(
            fmt("prefix ex: <x> insert data {} ; insert data { ex:a ex:b ex:c . }"),
            "PREFIX ex: <x>

INSERT DATA {};

INSERT DATA {
  ex:a ex:b ex:c .
}
"
        );
        // a trailing comment after the `;` stays after it
        assert_eq!(
            fmt("CLEAR ALL ; # c\nDROP ALL ; # d\n"),
            "CLEAR ALL; # c\n\nDROP ALL # d\n"
        );
        // the header and the comments at the end
        assert_eq!(
            fmt("# head\n\nPREFIX ex: <x>\n# about clearing\nCLEAR ALL\n# end\n"),
            "# head\n\nPREFIX ex: <x>\n\n# about clearing\nCLEAR ALL\n# end\n"
        );
    }
}
