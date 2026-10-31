//! `sparkles load` reads Jena's TriX, RDF Thrift, RDF Protobuf and RDF/JSON files as it
//! reads the W3C syntaxes, with no rewritten copy. The process's temporary directory is a
//! path that does not exist, so a copy written there would fail the load. A malformed
//! file fails with its name, the syntax and a position.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const DATA: &str = "<http://ex.org/a> <http://ex.org/p> \"one\" .\n\
    <http://ex.org/a> <http://ex.org/p> \"deux\"@fr .\n\
    <http://ex.org/b> <http://ex.org/q> <http://ex.org/a> .\n";

fn run(tmp: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .env("TMPDIR", tmp.join("no-such-temp-dir"))
        .env("RUST_LOG", "error")
        .args(args)
        .output()
        .unwrap()
}

#[track_caller]
fn expect(tmp: &Path, args: &[&str], code: i32) -> Output {
    let o = run(tmp, args);
    assert_eq!(
        o.status.code(),
        Some(code),
        "{args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

#[test]
fn load_reads_each_jena_syntax_in_place_and_names_a_broken_file() {
    let dir = tempfile::tempdir().unwrap();
    let nt = dir.path().join("data.nt");
    std::fs::write(&nt, DATA).unwrap();
    for ext in ["trix", "rt", "rpb", "rj", "rt.gz", "trix.zst"] {
        let file = dir.path().join(format!("d.{ext}"));
        let file = file.to_str().unwrap();
        expect(
            dir.path(),
            &["convert", nt.to_str().unwrap(), "--output-file", file],
            0,
        );
        let db = dir.path().join(format!("db-{ext}"));
        let db = db.to_str().unwrap();
        let o = expect(dir.path(), &["load", "--loc", db, file], 0);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains("loaded 3 quads"), "{ext}: {err}");
        let o = expect(
            dir.path(),
            &[
                "query",
                "--loc",
                db,
                "--results",
                "csv",
                "SELECT ?o WHERE { <http://ex.org/b> ?p ?o }",
            ],
            0,
        );
        assert!(
            String::from_utf8_lossy(&o.stdout).contains("http://ex.org/a"),
            "{ext}"
        );
    }
    // a broken file, plain and compressed explicitly
    let good = std::fs::read(dir.path().join("d.rt")).unwrap();
    let bad = dir.path().join("bad.rt");
    std::fs::write(&bad, &good[..good.len() - 6]).unwrap();
    let db = dir.path().join("db-bad");
    let o = run(
        dir.path(),
        &["load", "--loc", db.to_str().unwrap(), bad.to_str().unwrap()],
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("bad.rt") && err.contains("RDF Thrift") && err.contains("after"),
        "{err}"
    );
}
