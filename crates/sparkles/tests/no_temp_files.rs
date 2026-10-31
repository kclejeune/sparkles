//! Loading Jena's syntaxes creates no temporary file. The process's temporary directory
//! is made a path that cannot hold one, so any attempt to write a copy fails the load.
//! This is the only test of the binary, because it changes the process environment.

use oxrdf::{GraphName, NamedNode, Quad};
use sparkles::Dataset;
use sparkles::codec::{Codec, Level};
use sparkles::jena_formats::{JenaFormat, RdfWriter};
use std::io::Write;

const ALL: [JenaFormat; 4] = [
    JenaFormat::TriX,
    JenaFormat::Thrift,
    JenaFormat::Protobuf,
    JenaFormat::RdfJson,
];

fn n(s: &str) -> NamedNode {
    NamedNode::new(format!("http://ex.org/{s}")).unwrap()
}

fn encode(format: JenaFormat) -> Vec<u8> {
    let mut w = RdfWriter::new(format, Vec::new());
    for (i, graph) in [GraphName::DefaultGraph, GraphName::DefaultGraph]
        .into_iter()
        .enumerate()
    {
        let graph = if format.quads() && i == 1 {
            GraphName::NamedNode(n("g"))
        } else {
            graph
        };
        w.quad(&Quad::new(n("s"), n("p"), n(&format!("o{i}")), graph))
            .unwrap();
    }
    w.finish().unwrap()
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Codec::Gzip.writer(&mut out, Some(Level(1)), 1).unwrap();
    w.write_all(data).unwrap();
    w.finish().unwrap();
    out
}

#[test]
fn no_entry_point_writes_a_copy_of_a_jena_file() {
    let dir = tempfile::tempdir().unwrap();
    // an in-memory dataset builds its bulk loads in the temporary directory, so the
    // datasets are on disk, and they exist before the temporary directory becomes unusable
    let mut datasets: Vec<Dataset> = (0..ALL.len() * 5)
        .map(|i| Dataset::open(dir.path().join(format!("db{i}"))).unwrap())
        .collect();
    let mut files = Vec::new();
    for format in ALL {
        let bytes = encode(format);
        let plain = dir.path().join(format!("d.{}", format.file_extension()));
        let zipped = dir.path().join(format!("d.{}.gz", format.file_extension()));
        std::fs::write(&plain, &bytes).unwrap();
        std::fs::write(&zipped, gzip(&bytes)).unwrap();
        files.push((format, bytes, plain, zipped));
    }
    let missing = dir.path().join("no-such-temp-dir");
    // SAFETY: this test binary holds one test, and nothing else reads the environment
    // while it runs.
    unsafe { std::env::set_var("TMPDIR", &missing) };
    assert!(
        tempfile::NamedTempFile::new().is_err(),
        "the temporary directory is unusable"
    );

    let want = 2;
    for (format, bytes, plain, zipped) in files {
        let mut next = || datasets.pop().unwrap();
        assert_eq!(
            next().load_file(&plain).unwrap(),
            want,
            "{format:?} load_file"
        );
        assert_eq!(
            next().load_files([&plain, &zipped]).unwrap(),
            want,
            "{format:?} load_files"
        );
        let ds = next();
        ds.load_file_into(&zipped, "http://ex.org/t").unwrap();
        assert_eq!(ds.len(), want, "{format:?} load_file_into");
        assert_eq!(
            next().load_bytes(&bytes, format, None, None).unwrap(),
            want,
            "{format:?} load_bytes"
        );
        let stats = next()
            .update(&format!("LOAD <file://{}>", zipped.display()))
            .unwrap();
        assert_eq!(stats.inserted, want, "{format:?} LOAD");
    }
}
