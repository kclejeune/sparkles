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

#[test]
fn item62_audit_local_branch_create_delete_records_actual_branch_identity() {
    let dir = tempfile::tempdir().unwrap();
    ok(
        dir.path(),
        &[
            "update",
            "--loc",
            "db",
            "INSERT DATA { <urn:local> <urn:p> 1 }",
        ],
    );
    let audited = |args: &[&str]| {
        let output = Command::new(BIN)
            .current_dir(dir.path())
            .args(["--log-format", "json"])
            .args(args)
            .env_remove("SPARKLES_SERVER")
            .env("RUST_LOG", "sparkles::audit=info")
            .env("USER", "audit-local")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|record| record["target"] == "sparkles::audit")
            .collect::<Vec<_>>()
    };
    let created = audited(&["branch", "create", "--loc", "db", "audit-review"]);
    let info = ok(
        dir.path(),
        &[
            "branch",
            "show",
            "--loc",
            "db",
            "audit-review",
            "--format",
            "json",
        ],
    );
    let info: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap();
    let deleted = audited(&["branch", "delete", "--loc", "db", "audit-review"]);
    for (name, events) in [("branch_created", created), ("branch_deleted", deleted)] {
        assert_eq!(
            events.len(),
            1,
            "missing or duplicate local {name} audit event: {events:?}"
        );
        assert_eq!(events[0]["fields"]["event"], name);
        assert_eq!(events[0]["fields"]["principal"], "local:audit-local");
        assert_eq!(events[0]["fields"]["branch"], "audit-review");
        if name == "branch_created" {
            assert_eq!(events[0]["fields"]["branch_id"], info["id"]);
        } else {
            assert!(events[0]["fields"].get("branch_id").is_none());
        }
    }
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
    wait_ready(port);
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

/// Wait until the server on `port` answers `GET /$/ready` with `200`: it listens before
/// its datasets open.
fn wait_ready(port: u16) {
    use std::io::{Read as _, Write as _};
    let t0 = std::time::Instant::now();
    loop {
        if let Ok(mut c) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            let _ = write!(c, "GET /$/ready HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
            let mut buf = String::new();
            let _ = c.read_to_string(&mut buf);
            if buf.starts_with("HTTP/1.0 200") || buf.starts_with("HTTP/1.1 200") {
                return;
            }
        }
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(60),
            "the server never became ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
