//! The branch operations of F09's Phase 2 as the real binary: `sparkles merge
//! --squash`, `sparkles revert`, `sparkles cherry-pick`, renames, deletions that
//! re-parent, and exempt predicates.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env_remove("SPARKLES_SERVER")
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let o = run(dir, args);
    assert!(o.status.success(), "{args:?}: {}{}", out(&o), err(&o));
    o
}

fn update(dir: &Path, branch: &str, text: &str) {
    ok(dir, &["update", "--loc", "db", "--branch", branch, text]);
}

fn subjects(dir: &Path, branch: &str) -> String {
    out(&ok(
        dir,
        &[
            "query",
            "--loc",
            "db",
            "--branch",
            branch,
            "--results",
            "csv",
            "SELECT ?s { ?s ?p ?o } ORDER BY ?s",
        ],
    ))
}

#[test]
fn a24_a25_squash_and_revert() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    update(dir, "main", "INSERT DATA { <urn:a> <urn:age> 30 }");
    ok(dir, &["branch", "create", "--loc", "db", "dev"]);
    update(dir, "dev", "INSERT DATA { <urn:c> <urn:p> 1 }");
    let o = ok(dir, &["merge", "--loc", "db", "dev", "--squash"]);
    assert!(out(&o).contains("merged (squash)"), "{}", out(&o));
    assert!(subjects(dir, "main").contains("urn:c"));
    // commit 2 of main is the squash; revert it
    let o = ok(dir, &["revert", "--loc", "db", "2"]);
    assert!(out(&o).contains("revert commit 2 on main"), "{}", out(&o));
    assert!(
        out(&o).contains("changed: +0 -1 as commit 3"),
        "{}",
        out(&o)
    );
    assert!(!subjects(dir, "main").contains("urn:c"));
    let o = ok(dir, &["revert", "--loc", "db", "2", "--format", "json"]);
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["upToDate"], true, "{j}");
    // a conflicting revert exits 2
    update(
        dir,
        "main",
        "DELETE DATA { <urn:a> <urn:age> 30 } ; INSERT DATA { <urn:a> <urn:age> 31 }",
    );
    let o = run(dir, &["revert", "--loc", "db", "1"]);
    assert_eq!(o.status.code(), Some(2), "{}{}", out(&o), err(&o));
    assert!(
        out(&o).contains("CONFLICT  <urn:a> <urn:age>"),
        "{}",
        out(&o)
    );
    ok(
        dir,
        &["revert", "--loc", "db", "1", "--on-conflict", "theirs"],
    );
    assert!(!subjects(dir, "main").contains("urn:a"));
    // on a branch, with the global --branch
    ok(dir, &["revert", "--loc", "db", "--branch", "dev", "2"]);
    assert!(!subjects(dir, "dev").contains("urn:c"));
}

#[test]
fn a26_cherry_pick() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    update(dir, "main", "INSERT DATA { <urn:a> <urn:age> 30 }");
    ok(dir, &["branch", "create", "--loc", "db", "dev"]);
    update(dir, "dev", "INSERT DATA { <urn:c> <urn:p> 1 }");
    update(dir, "dev", "INSERT DATA { <urn:d> <urn:p> 1 }");
    let o = ok(dir, &["cherry-pick", "--loc", "db", "dev", "3"]);
    assert!(
        out(&o).contains("cherry-pick commit 3 of dev into main"),
        "{}",
        out(&o)
    );
    assert!(
        out(&o).contains("changed: +1 -0 as commit 2"),
        "{}",
        out(&o)
    );
    let main = subjects(dir, "main");
    assert!(main.contains("urn:d") && !main.contains("urn:c"), "{main}");
    let o = ok(dir, &["cherry-pick", "--loc", "db", "dev", "3"]);
    assert!(out(&o).contains("nothing to change"), "{}", out(&o));
    // into another branch, with the global --branch
    ok(dir, &["branch", "create", "--loc", "db", "qa"]);
    ok(
        dir,
        &["cherry-pick", "--loc", "db", "--branch", "qa", "dev", "2"],
    );
    assert!(subjects(dir, "qa").contains("urn:c"));
}

#[test]
fn a27_replay() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    update(dir, "main", "INSERT DATA { <urn:a> <urn:age> 30 }");
    ok(dir, &["branch", "create", "--loc", "db", "dev"]);
    update(dir, "dev", "INSERT DATA { <urn:c> <urn:p> 1 }");
    update(dir, "dev", "INSERT DATA { <urn:d> <urn:p> 1 }");
    let o = ok(dir, &["merge", "--loc", "db", "dev", "--replay"]);
    assert!(
        out(&o).contains("merged (replay): +2 -0 as commit 3"),
        "{}",
        out(&o)
    );
    assert_eq!(subjects(dir, "main"), subjects(dir, "dev"));
    let o = ok(dir, &["log", "--loc", "db", "--format", "json"]);
    let text = out(&o);
    assert!(
        text.contains("\"seq\": 3") || text.contains("\"seq\":3"),
        "{text}"
    );
}
