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

/// The answer of [`ASK`] with `extra` arguments, or the error of a refused run.
#[cfg(feature = "reasoning")]
#[track_caller]
fn ask_with(db: &str, extra: &[&str]) -> Result<bool, String> {
    let mut args = vec!["query", "--loc", db, "--results", "json"];
    args.extend_from_slice(extra);
    args.push(ASK);
    let o = Command::new(BIN).args(&args).output().unwrap();
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).into_owned());
    }
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    Ok(j["boolean"].as_bool().unwrap())
}

/// `--reasoning` chooses which inferred data a query reads: the configured RDFS on read
/// and materialized inferences by default, the asserted data alone for `false`, and the
/// facts a query-time reasoner derives for `demand`.
#[cfg(feature = "reasoning")]
#[test]
fn query_reasoning_modes_choose_the_inferred_data() {
    let (_dir, db) = database();
    let refused = |extra: &[&str]| ask_with(&db, extra).unwrap_err();
    // without a reasoning record, demand reasoning needs a profile
    assert!(
        refused(&["--reasoning", "demand"]).contains("needs a profile"),
        "{}",
        refused(&["--reasoning", "demand"])
    );
    assert_eq!(
        ask_with(&db, &["--reasoning", "demand", "--profile", "rdfs"]),
        Ok(true)
    );
    assert_eq!(ask_with(&db, &["--reasoning", "false"]), Ok(false));
    // the profile and the limit belong to demand reasoning
    for mode in [&[][..], &["--reasoning", "materialized"][..]] {
        let mut extra = mode.to_vec();
        extra.extend(["--profile", "rdfs"]);
        assert!(refused(&extra).contains("need --reasoning demand"));
    }
    std::fs::write(
        std::path::Path::new(&db).join("rdfs.json"),
        r#"{"rdfsFormat": 1, "graph": "default"}"#,
    )
    .unwrap();
    for (extra, expected) in [
        (&[][..], true),
        (&["--reasoning", "true"][..], true),
        (&["--reasoning", "materialized"][..], true),
        (&["--reasoning", "false"][..], false),
        (&["--reasoning", "false", "--profile", "rdfs"][..], false),
    ] {
        assert_eq!(ask_with(&db, extra), Ok(expected), "{extra:?}");
    }
    // a limit of zero derived facts lifts the limit
    assert_eq!(
        ask_with(
            &db,
            &[
                "--reasoning",
                "demand",
                "--profile",
                "rdfs",
                "--max-derived",
                "0"
            ]
        ),
        Ok(true)
    );
}
