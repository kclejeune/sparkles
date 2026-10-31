//! Every syntax Sparkles reads or writes works at every embedded entry point: `load_str`,
//! `load_str_into`, `load_bytes`, `load_file`, `load_files`, `load_file_into`, `dump`,
//! `dump_graph` and SPARQL `LOAD`. No entry point refuses a syntax another accepts.

use oxrdf::{GraphNameRef, NamedNode};
use sparkles::codec::{Codec, Level};
use sparkles::io::{RdfFormat, RdfSyntax};
use sparkles::jena_formats::JenaFormat;
use sparkles::{Dataset, Error};
use std::io::Write;

const TRIG: &str = "@prefix ex: <http://ex.org/> . \
    ex:a ex:p 1, \"x\"@en, \"plain\" . ex:a ex:q [ ex:r ex:b ] . \
    ex:g { ex:c ex:p ex:a . ex:c ex:p \"in g\" }";

const JENA: [JenaFormat; 4] = [
    JenaFormat::TriX,
    JenaFormat::Thrift,
    JenaFormat::Protobuf,
    JenaFormat::RdfJson,
];

const W3C: [RdfFormat; 5] = [
    RdfFormat::NQuads,
    RdfFormat::TriG,
    RdfFormat::Turtle,
    RdfFormat::NTriples,
    RdfFormat::RdfXml,
];

fn every_syntax() -> Vec<RdfSyntax> {
    W3C.iter()
        .map(|f| RdfSyntax::from(*f))
        .chain(JENA.iter().map(|j| RdfSyntax::from(*j)))
        .collect()
}

fn fixture() -> Dataset {
    let ds = Dataset::memory();
    ds.load_str(TRIG, RdfFormat::TriG).unwrap();
    ds
}

/// The default graph only, for syntaxes that hold no named graph.
fn default_graph_of(ds: &Dataset) -> Dataset {
    let mut nt = Vec::new();
    ds.dump(&mut nt, RdfFormat::NTriples).unwrap();
    let out = Dataset::memory();
    out.load_str(std::str::from_utf8(&nt).unwrap(), RdfFormat::NTriples)
        .unwrap();
    out
}

/// N-Quads with blank node labels renamed in order of appearance, sorted: isomorphic
/// datasets that hold a blank node per label agree.
fn canonical(ds: &Dataset) -> Vec<String> {
    let mut buf = Vec::new();
    ds.dump(&mut buf, RdfFormat::NQuads).unwrap();
    let mut labels = std::collections::BTreeMap::new();
    let mut lines: Vec<String> = String::from_utf8(buf)
        .unwrap()
        .lines()
        .map(|l| {
            l.split(' ')
                .map(|t| match t.strip_prefix("_:") {
                    Some(label) => {
                        let next = labels.len();
                        format!("_:b{}", labels.entry(label.to_owned()).or_insert(next))
                    }
                    None => t.to_owned(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    lines.sort();
    lines
}

fn written(ds: &Dataset, syntax: RdfSyntax) -> Vec<u8> {
    let mut out = Vec::new();
    ds.dump(&mut out, syntax).unwrap();
    out
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Codec::Gzip.writer(&mut out, Some(Level(1)), 1).unwrap();
    w.write_all(data).unwrap();
    w.finish().unwrap();
    out
}

/// The part of the fixture a syntax can hold.
fn expected_for(ds: &Dataset, syntax: RdfSyntax) -> Vec<String> {
    if syntax.supports_datasets() {
        canonical(ds)
    } else {
        canonical(&default_graph_of(ds))
    }
}

#[test]
fn load_bytes_and_dump_round_trip_every_syntax() {
    let ds = fixture();
    for syntax in every_syntax() {
        let bytes = written(&ds, syntax);
        let back = Dataset::memory();
        back.load_bytes(&bytes, syntax, None, None).unwrap();
        assert_eq!(
            canonical(&back),
            expected_for(&ds, syntax),
            "{}",
            syntax.name()
        );
        // a compressed body is read as a compressed file is
        let back = Dataset::memory();
        back.load_bytes(&gzip(&bytes), syntax, None, None).unwrap();
        assert_eq!(
            canonical(&back),
            expected_for(&ds, syntax),
            "{} gz",
            syntax.name()
        );
    }
}

#[test]
fn load_str_and_load_str_into_take_every_text_syntax() {
    let ds = fixture();
    for syntax in every_syntax() {
        // the binary syntaxes are not text
        if matches!(
            syntax,
            RdfSyntax::Jena(JenaFormat::Thrift | JenaFormat::Protobuf)
        ) {
            continue;
        }
        let text = String::from_utf8(written(&ds, syntax)).unwrap();
        let back = Dataset::memory();
        back.load_str(&text, syntax).unwrap();
        assert_eq!(
            canonical(&back),
            expected_for(&ds, syntax),
            "{}",
            syntax.name()
        );
        let into = Dataset::memory();
        into.load_str_into(&text, syntax, "http://ex.org/t")
            .unwrap();
        assert!(
            canonical(&into)
                .iter()
                .any(|l| l.ends_with("\"plain\" <http://ex.org/t> .")),
            "{}",
            syntax.name()
        );
    }
}

#[test]
fn load_bytes_resolves_relative_iris_against_the_base() {
    let trix = "<trix xmlns=\"http://www.w3.org/2004/03/trix/trix-1/\"><graph><triple>\
        <uri>s</uri><uri>p</uri><plainLiteral>v</plainLiteral></triple></graph></trix>";
    let ds = Dataset::memory();
    ds.load_bytes(
        trix.as_bytes(),
        JenaFormat::TriX,
        Some("http://ex.org/dir/"),
        None,
    )
    .unwrap();
    assert_eq!(
        canonical(&ds),
        ["<http://ex.org/dir/s> <http://ex.org/dir/p> \"v\" ."]
    );
    let e = Dataset::memory()
        .load_bytes(trix.as_bytes(), JenaFormat::TriX, None, None)
        .unwrap_err();
    assert!(matches!(e, Error::RdfParse(_)), "{e}");
}

#[test]
fn files_of_every_syntax_load_by_extension_plain_and_compressed() {
    let ds = fixture();
    let dir = tempfile::tempdir().unwrap();
    let ext = |s: RdfSyntax| match s {
        RdfSyntax::Rdf(f) => f.file_extension(),
        RdfSyntax::Jena(j) => j.file_extension(),
    };
    for syntax in every_syntax() {
        let bytes = written(&ds, syntax);
        let plain = dir.path().join(format!("p.{}", ext(syntax)));
        let zipped = dir.path().join(format!("z.{}.gz", ext(syntax)));
        std::fs::write(&plain, &bytes).unwrap();
        std::fs::write(&zipped, gzip(&bytes)).unwrap();
        let want = expected_for(&ds, syntax);
        for path in [&plain, &zipped] {
            let back = Dataset::memory();
            back.load_file(path).unwrap();
            assert_eq!(
                canonical(&back),
                want,
                "{} {}",
                syntax.name(),
                path.display()
            );
        }
        let both = Dataset::memory();
        both.load_files([&plain, &zipped]).unwrap();
        // the two blank node triples of the second file hold other blank nodes
        let bnodes = want.iter().filter(|l| l.contains("_:b")).count();
        assert_eq!(
            both.len(),
            (want.len() + bnodes) as u64,
            "{}",
            syntax.name()
        );
    }
}

#[test]
fn dump_graph_writes_one_graphs_triples_in_every_syntax() {
    let ds = fixture();
    let g = NamedNode::new("http://ex.org/g").unwrap();
    for syntax in every_syntax() {
        let mut out = Vec::new();
        let n = ds
            .dump_graph(GraphNameRef::NamedNode(g.as_ref()), &mut out, syntax)
            .unwrap();
        assert_eq!(n, 2, "{}", syntax.name());
        // the triples read back into the default graph
        let back = Dataset::memory();
        back.load_bytes(&out, syntax, None, None).unwrap();
        assert_eq!(
            canonical(&back),
            vec![
                "<http://ex.org/c> <http://ex.org/p> \"in g\" .".to_string(),
                "<http://ex.org/c> <http://ex.org/p> <http://ex.org/a> .".to_string(),
            ],
            "{}",
            syntax.name()
        );
        // and the default graph
        let mut out = Vec::new();
        let n = ds
            .dump_graph(GraphNameRef::DefaultGraph, &mut out, syntax)
            .unwrap();
        assert_eq!(n, 5, "{}", syntax.name());
    }
}

#[test]
fn sparql_load_reads_every_syntax_from_a_file() {
    let ds = fixture();
    let dir = tempfile::tempdir().unwrap();
    for syntax in every_syntax() {
        let ext = match syntax {
            RdfSyntax::Rdf(f) => f.file_extension(),
            RdfSyntax::Jena(j) => j.file_extension(),
        };
        let path = dir.path().join(format!("l.{ext}"));
        std::fs::write(&path, written(&ds, syntax)).unwrap();
        let back = Dataset::memory();
        back.update(&format!("LOAD <file://{}>", path.display()))
            .unwrap();
        assert_eq!(
            canonical(&back),
            expected_for(&ds, syntax),
            "{}",
            syntax.name()
        );
    }
}

#[test]
fn a_malformed_file_of_each_jena_syntax_names_the_file_and_the_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let ds = fixture();
    for j in JENA {
        let mut bytes = written(&ds, j.into());
        match j {
            JenaFormat::Thrift | JenaFormat::Protobuf => bytes.truncate(bytes.len() - 7),
            JenaFormat::TriX => bytes.truncate(bytes.len() / 2),
            JenaFormat::RdfJson => bytes.truncate(bytes.len() / 2),
        }
        let path = dir.path().join(format!("bad.{}", j.file_extension()));
        std::fs::write(&path, bytes).unwrap();
        let e = Dataset::memory().load_file(&path).unwrap_err();
        assert!(
            matches!(&e, Error::RdfParse(m) if m.contains("bad.") && m.contains(j.name())),
            "{j:?}: {e}"
        );
        // and a failed load leaves the dataset as it was
        let target = fixture();
        let before = target.len();
        assert!(target.load_file(&path).is_err());
        assert_eq!(target.len(), before, "{j:?}");
    }
}

#[test]
fn syntaxes_are_found_by_name_media_type_and_extension() {
    for (name, want) in [
        ("trix", RdfSyntax::from(JenaFormat::TriX)),
        ("application/trix+xml", JenaFormat::TriX.into()),
        ("thrift", JenaFormat::Thrift.into()),
        ("rt", JenaFormat::Thrift.into()),
        ("application/rdf+protobuf", JenaFormat::Protobuf.into()),
        ("rdf/json", JenaFormat::RdfJson.into()),
        ("ttl", RdfFormat::Turtle.into()),
        ("text/turtle", RdfFormat::Turtle.into()),
        ("nq", RdfFormat::NQuads.into()),
        ("application/trig; charset=utf-8", RdfFormat::TriG.into()),
    ] {
        assert_eq!(RdfSyntax::from_name(name), Some(want), "{name}");
    }
    assert_eq!(
        RdfSyntax::from_extension(".pbrdf"),
        Some(JenaFormat::Protobuf.into())
    );
    assert_eq!(
        RdfSyntax::from_extension("rdf"),
        Some(RdfFormat::RdfXml.into())
    );
    assert_eq!(RdfSyntax::from_name("no-such-syntax"), None);
}

fn isomorphic_dataset(ds: &Dataset) -> oxrdf::Dataset {
    let mut nq = Vec::new();
    ds.dump(&mut nq, RdfFormat::NQuads).unwrap();
    let mut out: oxrdf::Dataset = oxrdfio::RdfParser::from_format(RdfFormat::NQuads)
        .for_slice(&nq)
        .map(Result::unwrap)
        .collect();
    out.canonicalize(oxrdf::graph::CanonicalizationAlgorithm::Unstable);
    out
}

#[test]
fn jenas_trix_files_load_like_their_nquads_and_round_trip_through_every_syntax() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testsuite/trix/jena");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let trix = entry.unwrap().path();
        if trix.extension().is_none_or(|e| e != "trix") {
            continue;
        }
        let nq = trix.with_extension("nq");
        if !nq.exists() {
            continue;
        }
        let from_trix = Dataset::memory();
        from_trix.load_file(&trix).unwrap();
        let from_nq = Dataset::memory();
        from_nq.load_file(&nq).unwrap();
        let want = isomorphic_dataset(&from_nq);
        assert_eq!(isomorphic_dataset(&from_trix), want, "{}", trix.display());
        // and through every syntax that holds the dataset
        for j in [JenaFormat::TriX, JenaFormat::Thrift, JenaFormat::Protobuf] {
            let bytes = written(&from_trix, j.into());
            let back = Dataset::memory();
            back.load_bytes(&bytes, j, None, None).unwrap();
            assert_eq!(
                isomorphic_dataset(&back),
                want,
                "{} via {}",
                trix.display(),
                j.name()
            );
        }
        checked += 1;
    }
    assert!(checked >= 12, "{checked} files");
}

#[test]
fn jenas_bad_trix_files_are_refused_with_the_file_name() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testsuite/trix/jena");
    let mut refused = 0;
    for n in 1..=9 {
        let path = dir.join(format!("trix-bad-{n:02}.trix"));
        if !path.exists() {
            continue;
        }
        let e = Dataset::memory().load_file(&path).unwrap_err();
        assert!(
            matches!(&e, Error::RdfParse(m) if m.contains(&format!("trix-bad-{n:02}")) && m.contains("TriX")),
            "{e}"
        );
        refused += 1;
    }
    assert!(refused >= 5, "{refused} files");
}
