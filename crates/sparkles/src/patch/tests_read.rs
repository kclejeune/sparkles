//! The patch reader: the text and binary forms, round trips with the writer, and the
//! patches of Apache Jena's `jena-rdfpatch` tests (`TestPatchIO_Text`,
//! `AbstractTestPatchIO`, `testing/files/syntax-1.rdfp`; Apache-2.0), ported.

use super::*;
use oxrdf::{BlankNode, Literal, NamedNode, Triple};

fn n(s: &str) -> NamedNode {
    NamedNode::new_unchecked(s)
}

fn text(s: &str) -> crate::Result<Vec<PatchRow>> {
    PatchReader::text(s.as_bytes()).collect()
}

fn binary(b: &[u8]) -> crate::Result<Vec<PatchRow>> {
    PatchReader::binary(b).collect()
}

fn patch_error(e: crate::Error) -> PatchError {
    match e {
        crate::Error::Patch(p) => *p,
        e => panic!("not a patch error: {e}"),
    }
}

/// Write rows in one form and read them back.
fn round_trip(rows: &[PatchRow], bin: bool) -> Vec<PatchRow> {
    let mut w = PatchWriter::new(Vec::new(), bin);
    for r in rows {
        w.row(r).unwrap();
    }
    PatchReader::new(&w.into_inner()[..], bin)
        .collect::<crate::Result<Vec<_>>>()
        .unwrap()
}

fn q(s: impl Into<NamedOrBlankNode>, o: impl Into<Term>, g: GraphName) -> Quad {
    Quad::new(s, n("http://example/p"), o, g)
}

#[test]
fn every_row_round_trips_in_both_forms() {
    let g1 = GraphName::NamedNode(n("http://example/g1"));
    let g2 = GraphName::BlankNode(BlankNode::new_unchecked("g2"));
    let b = BlankNode::new_unchecked("b");
    let tt = Term::Triple(Box::new(Triple::new(
        b.clone(),
        n("http://example/prop"),
        Term::Triple(Box::new(Triple::new(
            b.clone(),
            n("http://example/q"),
            b.clone(),
        ))),
    )));
    let rows = vec![
        PatchRow::Header("id".into(), n("urn:uuid:x#commit:2").into()),
        PatchRow::Header(
            "message".into(),
            Literal::new_simple_literal("a \"b\"").into(),
        ),
        PatchRow::Begin,
        // AbstractTestPatchIO.write_read_01
        PatchRow::Add(q(
            n("http://example/s1"),
            n("http://example/o1"),
            g1.clone(),
        )),
        PatchRow::Delete(q(
            n("http://example/s1"),
            n("http://example/o1"),
            g1.clone(),
        )),
        // write_read_02
        PatchRow::Add(q(
            BlankNode::new_unchecked("s2"),
            Literal::new_typed_literal("123", oxrdf::vocab::xsd::INTEGER),
            g2.clone(),
        )),
        // write_read_04, with the triple terms as objects: a subject cannot be one
        PatchRow::Add(q(b.clone(), tt.clone(), g2.clone())),
        PatchRow::Add(q(
            n("http://example/s"),
            Literal::new_directional_language_tagged_literal("x", "ar", oxrdf::BaseDirection::Rtl)
                .unwrap(),
            GraphName::DefaultGraph,
        )),
        PatchRow::Add(q(
            n("http://example/s"),
            Literal::new_language_tagged_literal("tab\there\nnewline", "en-GB").unwrap(),
            GraphName::DefaultGraph,
        )),
        PatchRow::PrefixSet("ex".into(), "http://example/".into()),
        PatchRow::PrefixSet("".into(), "http://example/empty#".into()),
        PatchRow::PrefixRemove("ex".into()),
        PatchRow::Segment,
        PatchRow::Commit,
        PatchRow::Abort,
    ];
    assert_eq!(round_trip(&rows, false), rows);
    assert_eq!(round_trip(&rows, true), rows);
}

#[test]
fn jena_syntax_1() {
    let rows = text(include_str!("../../tests/patch/jena-syntax-1.rdfp")).unwrap();
    let codes: Vec<_> = rows.iter().map(PatchRow::code).collect();
    assert_eq!(codes, ["H", "TX", "PA", "PD", "PA", "PD", "A", "D", "TC"]);
    assert_eq!(
        rows[0],
        PatchRow::Header(
            "id".into(),
            n("uuid:bbe2edae-325e-11ec-abcc-a70bbba0dfb1").into()
        )
    );
    assert_eq!(
        rows[4],
        PatchRow::PrefixSet("".into(), "http://example".into())
    );
}

#[test]
fn triple_terms_and_replacement_characters() {
    // TestPatchIO_Text.read_tripleTerm_01
    let rows = text("A <http://example/s1> <http://example/p1> <<( <http://example/s> <http://example/p> <http://example/o> )>> .").unwrap();
    let PatchRow::Add(quad) = &rows[0] else {
        panic!()
    };
    assert!(matches!(quad.object, Term::Triple(_)));
    // read_warning_01 and read_no_warning_01: a raw U+FFFD and its escape read the same
    let raw =
        text("A <http://example/s1> <http://example/p1> 'abc\u{FFFD}def' <http://example/g1> .")
            .unwrap();
    let esc =
        text("A <http://example/s1> <http://example/p1> 'abc\\uFFFDdef' <http://example/g1> .")
            .unwrap();
    assert_eq!(raw, esc);
    // a triple term as a subject is a term error
    let e = patch_error(text("A <<( <urn:s> <urn:p> <urn:o> )>> <urn:p> <urn:o> .").unwrap_err());
    assert_eq!(e.kind, PatchErrorKind::Term);
}

#[test]
fn turtle_terms() {
    let rows = text(
        "# a comment\n\
         H id <urn:x>.\n\
         TB .\n\
         A _:a <urn:p> 12 .\n\
         A <_:a> <urn:p> -1.5 .\n\
         A <urn:s> <urn:p> 1e3 .\n\
         A <urn:s> <urn:p> false .\n\
         A <urn:s> <urn:p> \"\"\"two\nlines\"\"\" .\n\
         A <urn:s> <urn:p> \"x\"^^<http://www.w3.org/2001/XMLSchema#string> .\n\
         A <urn:s> <urn:p> 'y'@en--ltr <urn:g> .\n\
         PA \"ex\" \"http://example/\" .\n\
         TC .",
    )
    .unwrap();
    let objects: Vec<String> = rows
        .iter()
        .filter_map(|r| match r {
            PatchRow::Add(q) => Some(q.object.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(
        objects,
        [
            "\"12\"^^<http://www.w3.org/2001/XMLSchema#integer>",
            "\"-1.5\"^^<http://www.w3.org/2001/XMLSchema#decimal>",
            "\"1e3\"^^<http://www.w3.org/2001/XMLSchema#double>",
            "\"false\"^^<http://www.w3.org/2001/XMLSchema#boolean>",
            "\"two\\nlines\"",
            "\"x\"",
            "\"y\"@en--ltr",
        ]
    );
    assert_eq!(rows[1], PatchRow::Begin);
    // `_:a` and `<_:a>` are the same label
    let subjects: Vec<_> = rows
        .iter()
        .filter_map(|r| match r {
            PatchRow::Add(q) => Some(q.subject.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(subjects[0], subjects[1]);
    assert_eq!(
        rows[rows.len() - 2],
        PatchRow::PrefixSet("ex".into(), "http://example/".into())
    );
}

#[test]
fn syntax_errors_name_line_and_column() {
    // F10 P11: a row one term short
    let e = patch_error(text("A <urn:a> <urn:p> .").unwrap_err());
    assert_eq!(e.kind, PatchErrorKind::Syntax);
    assert_eq!((e.line, e.column, e.row), (Some(1), Some(19), Some(1)));
    assert!(
        e.to_string()
            .starts_with("RDF Patch syntax error at line 1, column 19"),
        "{e}"
    );
    for (bad, line) in [
        ("TX .\nA <urn:a> <urn:p> <urn:o>\n", 3),
        ("TX .\nX .\n", 2),
        ("TX\n.\n.", 3),
        ("A ex:a <urn:p> <urn:o> .", 1),
        ("A <urn:a> <urn:p> \"open .", 1),
        ("A <urn:a> <urn:p> << <urn:a> <urn:b> <urn:c> >> .", 1),
    ] {
        let e = patch_error(text(bad).unwrap_err());
        assert_eq!(e.kind, PatchErrorKind::Syntax, "{bad}: {e}");
        assert_eq!(e.line, Some(line), "{bad}: {e}");
    }
    // rows before the error are returned first
    let mut r = PatchReader::text(&b"TX .\nA <urn:a> <urn:p> <urn:o> .\nA <urn:a> .\n"[..]);
    assert_eq!(r.next_row().unwrap(), Some(PatchRow::Begin));
    assert!(matches!(r.next_row().unwrap(), Some(PatchRow::Add(_))));
    assert!(r.next_row().is_err());
    assert_eq!(r.next_row().unwrap(), None);
}

#[test]
fn term_errors() {
    for bad in [
        "A <urn:a> \"p\" <urn:o> .",
        "A \"s\" <urn:p> <urn:o> .",
        "A <urn:a> <urn:p> <urn:o> \"g\" .",
        "A <urn:a> <urn:p> \"x\"@1-bad-tag .",
        "A <not an iri> <urn:p> <urn:o> .",
        "A <urn:a> <urn:p> \"x\"^^<http://www.w3.org/1999/02/22-rdf-syntax-ns#langString> .",
    ] {
        let e = patch_error(text(bad).unwrap_err());
        assert_eq!(e.kind, PatchErrorKind::Term, "{bad}: {e}");
        assert_eq!(e.line, Some(1), "{bad}");
    }
}

#[test]
fn jena_files_read_the_same_in_both_forms() {
    let t = text(include_str!("../../tests/patch/jena-1.rdfp")).unwrap();
    let b = binary(include_bytes!("../../tests/patch/jena-1.trp")).unwrap();
    assert_eq!(t.len(), 17);
    assert_eq!(t, b);
    assert_eq!(
        t[3],
        PatchRow::PrefixSet("foaf".into(), "http://xmlns.com/foaf/0.1/".into())
    );
}

#[test]
fn binary_errors() {
    // truncated inside a row
    let mut w = PatchWriter::new(Vec::new(), true);
    w.header("id", "urn:x").unwrap();
    let full = w.into_inner();
    let e = patch_error(binary(&full[..full.len() - 3]).unwrap_err());
    assert_eq!(e.kind, PatchErrorKind::Syntax);
    assert_eq!((e.row, e.offset), (Some(1), Some(0)));
    // a string length the body cannot hold fails at the end, without allocating it
    let e = patch_error(binary(&[0x1C, 0x18, 0xff, 0xff, 0xff, 0xff, 0x0f, b'a']).unwrap_err());
    assert_eq!(e.kind, PatchErrorKind::Syntax);
    // an unknown field of a row is skipped: field 7 (i32) = 1, then field 6 in the long
    // form (type, zigzag id) = TX
    assert_eq!(
        binary(&[0x75, 0x02, 0x05, 0x0C, 0x00, 0x00]).unwrap(),
        [PatchRow::Begin]
    );
    // a row of unknown fields only is empty
    assert!(binary(&[0x75, 0x02, 0x00]).is_err());
    // a prefixed name is a term error
    let e = patch_error(
        binary(&[
            0x1C, 0x18, 1, b'x', 0x1C, 0x4C, 0x18, 1, b'e', 0x18, 1, b'a', 0, 0, 0, 0,
        ])
        .unwrap_err(),
    );
    assert_eq!(e.kind, PatchErrorKind::Term, "{e}");
}

#[test]
fn file_extensions() {
    use std::path::Path;
    assert_eq!(binary_for_path(Path::new("a.rdfp")), Some(false));
    assert_eq!(binary_for_path(Path::new("a.TRP")), Some(true));
    assert_eq!(binary_for_path(Path::new("a.nq")), None);
}
