//! `sparkles infer` updates the previous materialization incrementally, run as the real
//! binary: each run is a new process, so the closure comes from the database.
#![cfg(feature = "reasoning")]

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
        &[
            "infer",
            "--loc",
            &db,
            "--profile",
            "rdfs-simple",
            "--full",
            "--threads",
            "2",
        ],
        0,
    );
    let e = stderr(&o);
    assert!(e.contains("full, ") && e.contains("+0 -0"), "{e}");
    let after = expect(&["query", "--loc", &db, "--results", "csv", q], 0).stdout;
    assert_eq!(before, after);
    let o = expect(&["infer", "--loc", &db, "--status", "--format", "json"], 0);
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(s["run"]["threads"], 2, "{s}");
    assert!(s["run"]["peakBytes"].as_u64().unwrap_or(0) > 0, "{s}");
    // other rules: in full, with the reason
    let o = expect(&["infer", "--loc", &db, "--profile", "rdfs"], 0);
    assert!(
        stderr(&o).contains("because the rules changed"),
        "{}",
        stderr(&o)
    );
}

/// `--max-inferred` sets the limit of a run, a run past it fails with the limit and how to
/// raise it, and 0 sets no limit.
#[test]
fn infer_stops_at_the_limit_of_inferred_triples() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data.ttl");
    std::fs::write(
        &data,
        "@prefix ex: <http://ex.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
         ex:p a owl:TransitiveProperty .
         ex:a0 ex:p ex:a1 . ex:a1 ex:p ex:a2 . ex:a2 ex:p ex:a3 . ex:a3 ex:p ex:a4 .",
    )
    .unwrap();
    let db = dir.path().join("db").to_str().unwrap().to_string();
    expect(&["load", "--loc", &db, data.to_str().unwrap()], 0);
    let o = expect(
        &[
            "infer",
            "--loc",
            &db,
            "--profile",
            "owl-rl",
            "--max-inferred",
            "3",
        ],
        1,
    );
    let e = stderr(&o);
    assert!(
        e.contains("exceeded the limit of 3 inferred triples") && e.contains("--max-inferred"),
        "{e}"
    );
    // nothing was recorded
    let o = expect(&["infer", "--loc", &db, "--status", "--format", "json"], 0);
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(s.get("run").is_none_or(|r| r.is_null()), "{s}");
    let o = expect(
        &[
            "infer",
            "--loc",
            &db,
            "--profile",
            "owl-rl",
            "--max-inferred",
            "0",
        ],
        0,
    );
    assert!(stderr(&o).contains("inferred triples"), "{}", stderr(&o));
}

#[test]
fn infer_why_prints_the_derivation() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data.ttl");
    std::fs::write(
        &data,
        "@prefix ex: <http://ex.org/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:Cat rdfs:subClassOf ex:Animal . ex:tom a ex:Cat .",
    )
    .unwrap();
    let db = dir.path().join("db").to_str().unwrap().to_string();
    expect(&["load", "--loc", &db, data.to_str().unwrap()], 0);
    expect(&["infer", "--loc", &db, "--profile", "rdfs"], 0);
    let why = "<http://ex.org/tom> a <http://ex.org/Animal>";
    let o = expect(&["infer", "--loc", &db, "--why", why], 0);
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(text.contains("derived by rdfs9 (rdfs)"), "{text}");
    assert!(text.contains("asserted in graph default"), "{text}");
    let o = expect(
        &["infer", "--loc", &db, "--why", why, "--format", "json"],
        0,
    );
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["trees"][0]["rule"]["name"], "rdfs9", "{j}");
    // not entailed: exit 1; a malformed triple: an error
    expect(
        &[
            "infer",
            "--loc",
            &db,
            "--why",
            "<http://ex.org/tom> a <http://ex.org/Plant>",
        ],
        1,
    );
    expect(
        &["infer", "--loc", &db, "--why", "<http://ex.org/tom> a"],
        1,
    );
    expect(
        &["infer", "--loc", &db, "--why", why, "--max-trees", "11"],
        2,
    );
}

#[test]
fn infer_why_not_returns_verified_missing_facts_without_materializing() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db").to_str().unwrap().to_string();
    let data = dir.path().join("data.ttl");
    std::fs::write(&data, "@prefix ex: <http://ex.org/> . ex:tom a ex:Cat .").unwrap();
    expect(&["load", "--loc", &db, data.to_str().unwrap()], 0);
    let output = expect(
        &[
            "infer",
            "--loc",
            &db,
            "--why-not",
            "<http://ex.org/tom> a <http://ex.org/Animal>",
            "--profile",
            "rdfs",
            "--max-depth",
            "2",
            "--max-hypotheses",
            "3",
            "--format",
            "json",
        ],
        0,
    );
    let j: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(j["mode"], "whyNot");
    assert_eq!(j["entailed"], false);
    assert!(!j["hypotheses"].as_array().unwrap().is_empty(), "{j}");
    let output = expect(&["infer", "--loc", &db, "--status", "--format", "json"], 0);
    assert!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["reasoning"].is_null()
    );
}

#[test]
fn infer_diff_compares_retained_states_without_materializing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let dataset = sparkles::Dataset::open(&path).unwrap();
    dataset.update("INSERT DATA { <urn:Manager> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:Employee> }").unwrap();
    let from = dataset.head_commit().seq;
    dataset
        .update("INSERT DATA { <urn:alice> a <urn:Manager> }")
        .unwrap();
    let head = dataset.head_commit().seq;
    drop(dataset);
    let range = format!("{from}..head");
    let result = expect(
        &[
            "infer",
            "--loc",
            path.to_str().unwrap(),
            "--diff",
            &range,
            "--profile",
            "rdfs-simple",
            "--format",
            "json",
        ],
        0,
    );
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["added"], 1);
    assert_eq!(value["removed"], 0);
    let dataset = sparkles::Dataset::open(&path).unwrap();
    assert_eq!(dataset.head_commit().seq, head);
    assert!(dataset.reasoning().record().is_none());
}

#[test]
fn infer_check_justify_returns_first_minimal_support_under_one_bound() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let dataset = sparkles::Dataset::open(&path).unwrap();
    dataset.update("INSERT DATA { <urn:Cat> <http://www.w3.org/2002/07/owl#disjointWith> <urn:Dog> . <urn:tom> a <urn:Cat>, <urn:Dog> }").unwrap();
    let head = dataset.head_commit().seq;
    drop(dataset);
    let result = expect(
        &[
            "infer",
            "--loc",
            path.to_str().unwrap(),
            "--check",
            "--justify",
            "--format",
            "json",
        ],
        1,
    );
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        value["findings"][0]["justification"]["facts"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let result = expect(
        &[
            "infer",
            "--loc",
            path.to_str().unwrap(),
            "--check",
            "--justify",
            "--max-checks",
            "1",
            "--format",
            "json",
        ],
        2,
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap()["justificationsComplete"],
        false
    );
    assert_eq!(
        sparkles::Dataset::open(&path).unwrap().head_commit().seq,
        head
    );
}
