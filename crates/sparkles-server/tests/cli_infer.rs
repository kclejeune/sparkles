//! `sparkles infer` updates the previous materialization incrementally, run as the real
//! binary: each run is a new process, so the closure comes from the database.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

#[track_caller]
fn expect(args: &[&str], code: i32) -> Output {
    let o = Command::new(BIN).args(args).output().unwrap();
    assert_eq!(
        o.status.code(),
        Some(code),
        "{args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[cfg(feature = "reasoning")]
#[test]
fn infer_runs_incrementally() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data.ttl");
    std::fs::write(
        &data,
        "@prefix ex: <http://ex.org/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:C rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:A . ex:x a ex:C .",
    )
    .unwrap();
    let db = dir.path().join("db").to_str().unwrap().to_string();
    expect(&["load", "--loc", &db, data.to_str().unwrap()], 0);
    let o = expect(&["infer", "--loc", &db, "--profile", "rdfs-simple"], 0);
    assert!(stderr(&o).contains("full, "), "{}", stderr(&o));
    expect(
        &[
            "update",
            "--loc",
            &db,
            "PREFIX ex: <http://ex.org/> DELETE DATA { ex:x a ex:C } ; INSERT DATA { ex:y a ex:B }",
        ],
        0,
    );
    let o = expect(&["infer", "--loc", &db, "--profile", "rdfs-simple"], 0);
    let e = stderr(&o);
    assert!(
        e.contains("incremental: 1 explicit triples added, 1 removed") && e.contains("-2"),
        "{e}"
    );
    let o = expect(&["infer", "--loc", &db, "--status", "--format", "json"], 0);
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(s["run"]["method"], "incremental", "{s}");
    assert_eq!(s["run"]["changes"]["source"], "store", "{s}");
    // the result is what a full run gives
    let q = "SELECT ?s ?o { GRAPH <urn:x-sparkles:inferred> { ?s a ?o } } ORDER BY ?s ?o";
    let before = expect(&["query", "--loc", &db, "--results", "csv", q], 0).stdout;
    let o = expect(
        &["infer", "--loc", &db, "--profile", "rdfs-simple", "--full"],
        0,
    );
    let e = stderr(&o);
    assert!(e.contains("full, ") && e.contains("+0 -0"), "{e}");
    let after = expect(&["query", "--loc", &db, "--results", "csv", q], 0).stdout;
    assert_eq!(before, after);
    // other rules: in full, with the reason
    let o = expect(&["infer", "--loc", &db, "--profile", "rdfs"], 0);
    assert!(
        stderr(&o).contains("because the rules changed"),
        "{}",
        stderr(&o)
    );
}
