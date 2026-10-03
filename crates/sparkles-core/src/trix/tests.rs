//! The TriX reader and writer against Apache Jena's TriX tests (`TestTriXReader`,
//! `TestTriXBad` and `TestTriXWriter` in jena-arq), whose files are vendored in
//! `testsuite/trix/jena` under the Apache License 2.0.

use super::*;
use oxrdf::Dataset;
use oxrdf::graph::CanonicalizationAlgorithm;
use oxrdfio::{RdfFormat, RdfParser};
use std::path::PathBuf;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testsuite/trix/jena")
}

fn read_trix(bytes: &[u8]) -> Result<Vec<Quad>, TrixError> {
    let mut out = Vec::new();
    parse(bytes, None, |q| {
        out.push(q);
        Ok::<(), TrixError>(())
    })?;
    Ok(out)
}

fn read_file(name: &str) -> Result<Vec<Quad>, TrixError> {
    read_trix(&std::fs::read(dir().join(name)).unwrap())
}

fn read_nquads(name: &str) -> Vec<Quad> {
    let bytes = std::fs::read(dir().join(name)).unwrap();
    RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(&bytes)
        .collect::<Result<_, _>>()
        .unwrap()
}

fn canonical(quads: &[Quad]) -> Dataset {
    let mut d: Dataset = quads.iter().collect();
    d.canonicalize(CanonicalizationAlgorithm::Unstable);
    d
}

fn write(quads: &[Quad]) -> String {
    let mut w = TrixWriter::new(Vec::new());
    for q in quads {
        w.quad(q).unwrap();
    }
    String::from_utf8(w.finish().unwrap()).unwrap()
}

/// TestTriXReader: each document against its N-Quads, where Jena gives one.
const GOOD: &[(&str, Option<&str>)] = &[
    ("trix-01.trix", Some("trix-01.nq")),
    ("trix-02.trix", Some("trix-02.nq")),
    ("trix-03.trix", Some("trix-03.nq")),
    ("trix-04.trix", Some("trix-04.nq")),
    ("trix-05.trix", Some("trix-05.nq")),
    ("trix-06.trix", Some("trix-06.nq")),
    ("trix-10.trix", Some("trix-10.nq")),
    ("trix-11.trix", Some("trix-11.nq")),
    ("trix-12.trix", Some("trix-12.nq")),
    ("trix-13.trix", Some("trix-13.nq")),
    ("trix-14.trix", Some("trix-14.nq")),
    ("trix-15.trix", Some("trix-15.nq")),
    ("trix-ns-1.trix", Some("trix-ns-1.nq")),
    ("trix-ns-2.trix", Some("trix-ns-2.nq")),
    // the examples of HPL-2004-56 (Jena leaves out example 2, which uses <integer>)
    ("trix-ex-1.trix", None),
    ("trix-ex-3.trix", None),
    ("trix-ex-4.trix", None),
    ("trix-ex-5.trix", None),
    // the element names of the W3C DTD
    ("trix-w3c-1.trix", Some("trix-w3c-1.nq")),
    ("trix-w3c-2.trix", Some("trix-w3c-2.nq")),
    ("trix-star-1.trix", Some("trix-star-1.nq")),
    ("trix-star-2.trix", Some("trix-star-2.nq")),
    // not in Jena's lists of bad files: these two are well-formed
    ("trix-bad-00.trix", None),
    ("trix-bad-10.trix", None),
];

/// TestTriXBad, and example 2 of HPL-2004-56, whose `<integer>` Jena does not read.
const BAD: &[&str] = &[
    "trix-bad-01.trix",
    "trix-bad-02.trix",
    "trix-bad-03.trix",
    "trix-bad-04.trix",
    "trix-bad-05.trix",
    "trix-bad-06.trix",
    "trix-bad-07.trix",
    "trix-bad-08.trix",
    "trix-bad-09.trix",
    "trix-star-bad-triple-term-1.trix",
    "trix-star-bad-triple-term-2.trix",
    "trix-star-bad-triple-term-3.trix",
    "trix-star-bad-triple-term-4.trix",
    "trix-ex-2.trix",
];

#[test]
fn jena_reader_tests() {
    for (trix, nq) in GOOD {
        let got = read_file(trix).unwrap_or_else(|e| panic!("{trix}: {e}"));
        if let Some(nq) = nq {
            assert_eq!(
                canonical(&got),
                canonical(&read_nquads(nq)),
                "{trix} against {nq}"
            );
        }
    }
}

#[test]
fn jena_bad_tests() {
    for bad in BAD {
        assert!(read_file(bad).is_err(), "{bad} should not parse");
    }
}

/// TestTriXWriter: N-Quads written as TriX read back to the same dataset.
#[test]
fn jena_writer_tests() {
    for (_, nq) in GOOD {
        let Some(nq) = nq else { continue };
        let quads = read_nquads(nq);
        let doc = write(&quads);
        let back = read_trix(doc.as_bytes()).unwrap_or_else(|e| panic!("{nq}: {e}\n{doc}"));
        assert_eq!(canonical(&back), canonical(&quads), "{nq}:\n{doc}");
    }
}

#[test]
fn writer_output_matches_jena() {
    let nq = "<http://example/s> <http://example/p> \"a<&>\" .\n\
              <http://example/s> <http://example/p> \"x\"@en <http://example/g> .\n\
              <http://example/s> <http://example/p> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> <http://example/g> .\n";
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(nq.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        write(&quads),
        r#"<trix xmlns="http://www.w3.org/2004/03/trix/trix-1/">
  <graph>
    <triple>
      <uri>http://example/s</uri>
      <uri>http://example/p</uri>
      <plainLiteral>a&lt;&amp;&gt;</plainLiteral>
    </triple>
  </graph>
  <graph>
    <uri>http://example/g</uri>
    <triple>
      <uri>http://example/s</uri>
      <uri>http://example/p</uri>
      <plainLiteral xml:lang="en">x</plainLiteral>
    </triple>
    <triple>
      <uri>http://example/s</uri>
      <uri>http://example/p</uri>
      <typedLiteral datatype="http://www.w3.org/2001/XMLSchema#integer">1</typedLiteral>
    </triple>
  </graph>
</trix>
"#
    );
    assert_eq!(
        write(&[]),
        "<trix xmlns=\"http://www.w3.org/2004/03/trix/trix-1/\">\n</trix>\n"
    );
}

#[test]
fn directional_strings_and_odd_text_round_trip() {
    let nq = "<http://example/s> <http://example/p> \"abc\"@ar--rtl .\n\
              <http://example/s> <http://example/p> \"line\\r\\nnext\\ttab\" .\n\
              <http://example/s> <http://example/p> \"<b>bold</b> &amp;\"^^<http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral> .\n\
              <http://example/s> <http://example/p> \"<unbalanced\"^^<http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral> .\n\
              _:b1 <http://example/p> <<( _:b1 <http://example/q> \"v\" )>> _:g .\n";
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(nq.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    let doc = write(&quads);
    assert!(
        doc.contains(r#"<plainLiteral xml:lang="ar--rtl">abc</plainLiteral>"#),
        "{doc}"
    );
    assert!(doc.contains("<b>bold</b> &amp;</typedLiteral>"), "{doc}");
    assert!(doc.contains("&lt;unbalanced</typedLiteral>"), "{doc}");
    let mut back = read_trix(doc.as_bytes()).unwrap();
    // an XML literal is read as the XML it was written as: one that is not well-formed
    // XML is written escaped, and so reads back escaped
    let ill_formed = back.remove(3);
    assert_eq!(
        ill_formed.object.to_string(),
        "\"&lt;unbalanced\"^^<http://www.w3.org/1999/02/22-rdf-syntax-ns#XMLLiteral>"
    );
    let mut want = quads.clone();
    want.remove(3);
    assert_eq!(canonical(&back), canonical(&want), "{doc}");
}

#[test]
fn unwritable_characters_fail() {
    let q = Quad::new(
        NamedNode::new_unchecked("http://example/s"),
        NamedNode::new_unchecked("http://example/p"),
        Literal::new_simple_literal("bell\u{7}"),
        GraphName::DefaultGraph,
    );
    let mut w = TrixWriter::new(Vec::new());
    assert!(w.quad(&q).is_err());
}

#[test]
fn reader_details() {
    // relative IRIs resolve against the base; blank node ids that are not N-Triples
    // labels are kept apart; xml:lang="" is a simple literal; CDATA is text
    let doc = br#"<trix xmlns="http://www.w3.org/2004/03/trix/trix-1/">
  <graph>
    <uri>g</uri>
    <triple>
      <id>a node</id>
      <uri>p</uri>
      <plainLiteral xml:lang="">x</plainLiteral>
    </triple>
    <triple>
      <id>a node</id>
      <uri>p</uri>
      <typedLiteral datatype="http://www.w3.org/2001/XMLSchema#integer"><![CDATA[7]]></typedLiteral>
    </triple>
  </graph>
</trix>"#;
    assert!(read_trix(doc).is_err(), "relative IRIs need a base");
    let mut quads = Vec::new();
    parse(&doc[..], Some("http://example/"), |q| {
        quads.push(q);
        Ok::<(), TrixError>(())
    })
    .unwrap();
    assert_eq!(quads.len(), 2);
    assert_eq!(quads[0].graph_name.to_string(), "<http://example/g>");
    assert_eq!(quads[0].predicate.as_str(), "http://example/p");
    assert_eq!(quads[0].subject, quads[1].subject);
    assert_eq!(quads[0].object.to_string(), "\"x\"");
    assert_eq!(
        quads[1].object.to_string(),
        "\"7\"^^<http://www.w3.org/2001/XMLSchema#integer>"
    );

    for (bad, why) in [
        (&b"<trix><graph><uri>http://g</uri><triple><uri>http://s</uri><uri>http://p</uri><uri>http://o</uri></triple><uri>http://h</uri></graph></trix>"[..], "a graph name after triples"),
        (b"<trix><graph><triple><uri>http://s</uri><uri>http://p</uri><typedLiteral datatype=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#langString\">x</typedLiteral></triple></graph></trix>", "rdf:langString"),
        (b"<trix><graph><triple><uri>http://s</uri><uri>http://p</uri><plainLiteral xml:lang=\"not a tag\">x</plainLiteral></triple></graph></trix>", "a bad language tag"),
        (b"<trix><graph><triple><uri>http://s</uri><qname>ex:p</qname><uri>http://o</uri></triple></graph></trix>", "an undefined prefix"),
        (b"<trix><graph></graph>", "an unclosed root"),
        (b"<trix></trix><trix></trix>", "two roots"),
        (b"", "an empty document"),
        (b"<trix><graph><triple><uri>http://s</uri><uri>http://p</uri><uri>http://o</uri></triple></graph></trax>", "mismatched tags"),
    ] {
        assert!(read_trix(bad).is_err(), "{why}");
    }
}
