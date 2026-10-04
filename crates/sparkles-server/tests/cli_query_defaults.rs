//! `sparkles query --loc` and `sparkles queries run` run with the dataset's query
//! defaults, as a server and the library do: RDFS on read from the database's
//! `rdfs.json`, and the materialized inferences as part of the default graph.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");
const ASK: &str = "ASK { <urn:s> a <urn:parent> }";

#[track_caller]
fn run(args: &[&str]) -> String {
    let o = Command::new(BIN).args(args).output().unwrap();
    assert!(
        o.status.success(),
        "{args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[track_caller]
fn ask(db: &str) -> bool {
    let out = run(&["query", "--loc", db, "--results", "json", ASK]);
    let j: serde_json::Value = serde_json::from_str(&out).unwrap();
    j["boolean"].as_bool().unwrap()
}

fn database() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data.ttl");
    std::fs::write(
        &data,
        "<urn:child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:parent> .
         <urn:s> a <urn:child> .",
    )
    .unwrap();
    let db = dir.path().join("db").to_str().unwrap().to_string();
    run(&["load", "--loc", &db, data.to_str().unwrap()]);
    (dir, db)
}

#[test]
fn query_reads_the_configured_rdfs() {
    let (_dir, db) = database();
    assert!(!ask(&db));
    std::fs::write(
        std::path::Path::new(&db).join("rdfs.json"),
        r#"{"rdfsFormat": 1, "graph": "default"}"#,
    )
    .unwrap();
    assert!(ask(&db));
}

/// The answer of the stored query `parent`.
#[cfg(feature = "reasoning")]
#[track_caller]
fn stored_ask(db: &str) -> bool {
    let out = run(&["queries", "run", "--loc", db, "parent", "--results", "json"]);
    let j: serde_json::Value = serde_json::from_str(&out).unwrap();
    j["boolean"].as_bool().unwrap()
}

#[cfg(feature = "reasoning")]
fn store_query(dir: &std::path::Path, db: &str) {
    let q = dir.join("parent.rq");
    std::fs::write(&q, ASK).unwrap();
    run(&[
        "queries",
        "put",
        "--loc",
        db,
        "parent",
        "--query",
        q.to_str().unwrap(),
    ]);
}

#[cfg(feature = "reasoning")]
#[test]
fn query_reads_the_materialized_inferences() {
    let (dir, db) = database();
    store_query(dir.path(), &db);
    assert!(!ask(&db));
    assert!(!stored_ask(&db));
    run(&["infer", "--loc", &db, "--profile", "rdfs"]);
    assert!(ask(&db));
    assert!(stored_ask(&db));
}
