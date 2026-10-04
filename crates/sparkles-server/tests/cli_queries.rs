//! `sparkles queries` run as the real binary: storing, listing, running and deleting a
//! stored query, a run that follows the database's DESCRIBE setting and RDFS on read as
//! the server does, and a `queries.json` that cannot be read.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const DATA: &str = r#"@prefix ex: <http://ex.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:Student rdfs:subClassOf ex:Person .
ex:al a ex:Student ; ex:age 41 .
ex:bo ex:knows ex:al .
"#;

fn sparkles(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

/// Run and check the exit code; the output.
fn expect(dir: &Path, args: &[&str], code: i32) -> Output {
    let o = sparkles(dir, args);
    assert_eq!(
        o.status.code(),
        Some(code),
        "sparkles {args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("data.ttl"), DATA).unwrap();
    expect(dir.path(), &["load", "--loc", "db", "data.ttl"], 0);
    dir
}

#[test]
fn stored_queries_from_the_command_line() {
    let dir = setup();
    let d = dir.path();
    std::fs::write(
        d.join("q.rq"),
        "PREFIX ex: <http://ex.org/> SELECT ?who WHERE { ?who ex:age ?age FILTER(?age >= ?min) }",
    )
    .unwrap();
    let o = expect(
        d,
        &[
            "queries",
            "put",
            "--loc",
            "db",
            "adults",
            "--query",
            "q.rq",
            "--param",
            "min:integer=18",
        ],
        0,
    );
    assert_eq!(out(&o).trim(), "adults: version 1");
    let o = expect(
        d,
        &[
            "queries",
            "put",
            "--loc",
            "db",
            "adults",
            "--query",
            "q.rq",
            "--param",
            "min:integer=18",
        ],
        0,
    );
    assert_eq!(out(&o).trim(), "adults: unchanged at version 1");
    let o = expect(d, &["queries", "list", "--loc", "db"], 0);
    assert!(out(&o).starts_with("adults  v1  SELECT"), "{}", out(&o));
    let o = expect(
        d,
        &["queries", "list", "--loc", "db", "--format", "json"],
        0,
    );
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j[0]["name"], "adults");
    let o = expect(d, &["queries", "get", "--loc", "db", "adults"], 0);
    assert!(out(&o).contains("?min: integer = 18"), "{}", out(&o));
    expect(d, &["queries", "versions", "--loc", "db", "adults"], 0);

    let o = expect(
        d,
        &[
            "queries",
            "run",
            "--loc",
            "db",
            "adults",
            "--results",
            "csv",
        ],
        0,
    );
    assert!(out(&o).contains("http://ex.org/al"), "{}", out(&o));
    let o = expect(
        d,
        &[
            "queries",
            "run",
            "--loc",
            "db",
            "adults",
            "--set",
            "min=50",
            "--results",
            "csv",
        ],
        0,
    );
    assert!(!out(&o).contains("http://ex.org/al"), "{}", out(&o));
    expect(
        d,
        &["queries", "run", "--loc", "db", "adults", "--version", "7"],
        1,
    );
    expect(d, &["queries", "run", "--loc", "db", "missing"], 1);
    expect(d, &["queries", "run", "--loc", "nowhere", "adults"], 1);
    assert!(!d.join("nowhere").exists());

    expect(d, &["queries", "delete", "--loc", "db", "adults"], 0);
    expect(d, &["queries", "delete", "--loc", "db", "adults"], 1);
}

/// A stored query runs with the dataset's DESCRIBE setting and RDFS on read, as
/// `/{ds}/queries/{name}` does.
#[test]
fn runs_follow_the_dataset_settings() {
    let dir = setup();
    let d = dir.path();
    std::fs::write(d.join("describe.rq"), "DESCRIBE <http://ex.org/al>").unwrap();
    std::fs::write(
        d.join("people.rq"),
        "SELECT ?p WHERE { ?p a <http://ex.org/Person> }",
    )
    .unwrap();
    for (name, file) in [("al", "describe.rq"), ("people", "people.rq")] {
        expect(
            d,
            &["queries", "put", "--loc", "db", name, "--query", file],
            0,
        );
    }

    // the symmetric description adds the triples that point at the resource
    let o = expect(
        d,
        &["queries", "run", "--loc", "db", "al", "--results", "nt"],
        0,
    );
    assert!(!out(&o).contains("http://ex.org/bo"), "{}", out(&o));
    expect(
        d,
        &["describe-settings", "--loc", "db", "--set", "mode=scbd"],
        0,
    );
    let o = expect(
        d,
        &["queries", "run", "--loc", "db", "al", "--results", "nt"],
        0,
    );
    assert!(out(&o).contains("http://ex.org/bo"), "{}", out(&o));

    // RDFS on read with the schema in the default graph
    let o = expect(
        d,
        &[
            "queries",
            "run",
            "--loc",
            "db",
            "people",
            "--results",
            "csv",
        ],
        0,
    );
    assert!(!out(&o).contains("http://ex.org/al"), "{}", out(&o));
    std::fs::write(
        d.join("db/rdfs.json"),
        r#"{"rdfsFormat": 1, "graph": "default"}"#,
    )
    .unwrap();
    let o = expect(
        d,
        &[
            "queries",
            "run",
            "--loc",
            "db",
            "people",
            "--results",
            "csv",
        ],
        0,
    );
    assert!(out(&o).contains("http://ex.org/al"), "{}", out(&o));
}

#[test]
fn a_catalog_that_cannot_be_read_is_an_error() {
    let dir = setup();
    let d = dir.path();
    std::fs::write(d.join("db/queries.json"), "{ not json").unwrap();
    std::fs::write(d.join("q.rq"), "SELECT * WHERE { ?s ?p ?o }").unwrap();
    for args in [
        &["queries", "list", "--loc", "db"][..],
        &["queries", "put", "--loc", "db", "all", "--query", "q.rq"],
        &["queries", "delete", "--loc", "db", "all"],
        &["queries", "run", "--loc", "db", "all"],
    ] {
        let o = expect(d, args, 1);
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("queries.json"),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    // the file is left as it was
    assert_eq!(
        std::fs::read_to_string(d.join("db/queries.json")).unwrap(),
        "{ not json"
    );
}
