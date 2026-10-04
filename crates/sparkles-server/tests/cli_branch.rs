//! `sparkles branch`, `sparkles merge` and the global `--branch` as the real binary
//! (spec F09, A22): branches of a local database, merges with and without conflicts,
//! and the branch listing of a database a server holds.

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
fn a22_branches_and_merges_on_a_local_database() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    ok(
        dir,
        &[
            "update",
            "--loc",
            "db",
            "INSERT DATA { <urn:a> <urn:age> 30 }",
        ],
    );
    let o = ok(dir, &["branch", "create", "--loc", "db", "dev"]);
    assert!(
        err(&o).contains("created branch dev from main@1"),
        "{}",
        err(&o)
    );
    let o = ok(
        dir,
        &[
            "update",
            "--loc",
            "db",
            "--branch",
            "dev",
            "INSERT DATA { <urn:c> <urn:p> 1 }",
        ],
    );
    assert!(err(&o).contains("commit 2"), "{}", err(&o));
    assert!(subjects(dir, "dev").contains("urn:c"));
    assert!(!subjects(dir, "main").contains("urn:c"));
    // the listing
    let o = ok(dir, &["branch", "list", "--loc", "db"]);
    let text = out(&o);
    assert!(text.starts_with("branch"), "{text}");
    assert!(text.contains("dev") && text.contains("main@1"), "{text}");
    // a fast-forward merge exits 0
    let o = run(dir, &["merge", "--loc", "db", "dev"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    assert!(out(&o).contains("merged (fast-forward)"), "{}", out(&o));
    assert!(subjects(dir, "main").contains("urn:c"));
    let o = run(dir, &["merge", "--loc", "db", "dev"]);
    assert!(out(&o).contains("up to date"), "{}", out(&o));

    // a conflict exits 2 and prints the cell
    ok(
        dir,
        &[
            "update",
            "--loc",
            "db",
            "DELETE DATA { <urn:a> <urn:age> 30 } ; INSERT DATA { <urn:a> <urn:age> 31 }",
        ],
    );
    ok(
        dir,
        &[
            "update",
            "--loc",
            "db",
            "--branch",
            "dev",
            "DELETE DATA { <urn:a> <urn:age> 30 } ; INSERT DATA { <urn:a> <urn:age> 32 }",
        ],
    );
    let o = run(dir, &["merge", "--loc", "db", "dev"]);
    assert_eq!(o.status.code(), Some(2), "{}{}", out(&o), err(&o));
    let text = out(&o);
    assert!(text.contains("CONFLICT  <urn:a> <urn:age>"), "{text}");
    assert!(text.contains("base    30"), "{text}");
    assert!(text.contains("ours    31   (main)"), "{text}");
    assert!(text.contains("theirs  32   (dev)"), "{text}");
    assert!(text.contains("1 conflict, nothing merged"), "{text}");
    // resolved by a file
    std::fs::write(
        dir.join("r.json"),
        r#"[{"graph": null, "subject": "<urn:a>", "predicate": "<urn:age>", "take": "theirs"}]"#,
    )
    .unwrap();
    let o = run(dir, &["merge", "--loc", "db", "dev", "--resolve", "r.json"]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    let o = ok(
        dir,
        &[
            "query",
            "--loc",
            "db",
            "--results",
            "csv",
            "SELECT ?o { <urn:a> <urn:age> ?o }",
        ],
    );
    assert!(
        out(&o).contains("32") && !out(&o).contains("31"),
        "{}",
        out(&o)
    );
    // the branch's own log, and a dump of the branch
    let o = ok(
        dir,
        &["log", "--loc", "db", "--branch", "dev", "--format", "json"],
    );
    assert!(out(&o).contains("\"seq\""), "{}", out(&o));
    let o = ok(dir, &["dump", "--loc", "db", "--branch", "dev"]);
    assert!(out(&o).contains("<urn:c>"), "{}", out(&o));
    // protection and deletion
    ok(dir, &["branch", "protect", "--loc", "db", "dev"]);
    let o = run(
        dir,
        &[
            "update",
            "--loc",
            "db",
            "--branch",
            "dev",
            "INSERT DATA { <urn:x> <urn:p> 1 }",
        ],
    );
    assert!(!o.status.success());
    assert!(err(&o).contains("protected"), "{}", err(&o));
    ok(dir, &["branch", "protect", "--loc", "db", "dev", "--off"]);
    ok(dir, &["branch", "delete", "--loc", "db", "dev"]);
    let o = run(dir, &["branch", "show", "--loc", "db", "dev"]);
    assert!(!o.status.success());
    // a command that takes no branch refuses one
    let o = run(dir, &["check", "--loc", "db", "--branch", "x"]);
    assert!(!o.status.success());
}

/// A free port in 5600–5619.
fn port() -> u16 {
    (5600..5620)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("a free port in 5600-5619")
}

#[test]
fn a22_the_listing_reads_the_files_while_a_server_holds_the_database() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    ok(
        dir,
        &["update", "--loc", "db", "INSERT DATA { <urn:a> <urn:p> 1 }"],
    );
    ok(
        dir,
        &["branch", "create", "--loc", "db", "dev", "--note", "a test"],
    );
    let port = port();
    let mut child = Command::new(BIN)
        .args(["serve", "--port", &port.to_string(), "--loc"])
        .arg(format!("ds={}", dir.join("db").display()))
        .arg("--data")
        .arg(dir.join("data"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let o = run(dir, &["branch", "list", "--loc", "db", "--format", "json"]);
    #[cfg(feature = "auth")]
    let remote = {
        let base = format!("http://127.0.0.1:{port}");
        run(
            dir,
            &[
                "merge",
                "--server",
                &base,
                "--dataset",
                "ds",
                "dev",
                "--dry-run",
            ],
        )
    };
    let _ = child.kill();
    let _ = child.wait();
    assert!(o.status.success(), "{}", err(&o));
    let list: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(list[0]["name"], "main");
    assert_eq!(list[1]["name"], "dev");
    assert_eq!(list[1]["note"], "a test");
    assert_eq!(list[1]["from"]["branch"], "main");
    #[cfg(feature = "auth")]
    {
        assert_eq!(
            remote.status.code(),
            Some(0),
            "{}{}",
            out(&remote),
            err(&remote)
        );
        assert!(out(&remote).contains("up to date"), "{}", out(&remote));
    }
}
