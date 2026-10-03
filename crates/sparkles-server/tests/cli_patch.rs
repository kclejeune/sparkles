//! `sparkles patch` and `sparkles rdfpatch` as the real binary (spec F10 P15): applying
//! patch files to a local database and through a server, and printing their rows.

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

const P: &str = "TX .\nA <urn:a> <urn:p> \"1\" .\nA <urn:b> <urn:p> <urn:c> <urn:g> .\nTC .\n";

fn jena(name: &str) -> std::path::PathBuf {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../sparkles-core/tests/patch"
    ))
    .join(name)
}

#[test]
fn patch_applies_files_to_a_local_database() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("p.rdfp"), P).unwrap();
    let o = run(dir, &["patch", "--loc", "db", "p.rdfp"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        err(&o).starts_with("inserted 2 · deleted 0 · commit 1"),
        "{}",
        err(&o)
    );
    // the same patch again changes nothing
    let o = run(dir, &["patch", "--loc", "db", "p.rdfp"]);
    assert!(err(&o).contains("no change · head 1"), "{}", err(&o));
    // the binary form by its extension, one commit per file, with a message
    std::fs::copy(jena("jena-1.trp"), dir.join("j.trp")).unwrap();
    std::fs::write(dir.join("d.rdfp"), "D <urn:a> <urn:p> \"1\" .").unwrap();
    let o = run(
        dir,
        &[
            "patch",
            "--loc",
            "db",
            "--message",
            "two files",
            "j.trp",
            "d.rdfp",
        ],
    );
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        err(&o).contains("j.trp: inserted 10 · deleted 1 · commit 2"),
        "{}",
        err(&o)
    );
    assert!(
        err(&o).contains("d.rdfp: inserted 0 · deleted 1 · commit 3"),
        "{}",
        err(&o)
    );
    let o = run(dir, &["log", "--loc", "db", "--format", "json"]);
    assert!(out(&o).contains("\"kind\": \"patch\"") || out(&o).contains("\"kind\":\"patch\""));
    assert!(out(&o).contains("two files"), "{}", out(&o));
    // a syntax error names the file, the line and the column, and applies nothing
    std::fs::write(dir.join("bad.rdfp"), "A <urn:z> <urn:p> 1 .\nA <urn:z> .\n").unwrap();
    let o = run(dir, &["patch", "--loc", "db", "bad.rdfp"]);
    assert!(!o.status.success());
    assert!(
        err(&o).contains("bad.rdfp: RDF Patch syntax error at line 2"),
        "{}",
        err(&o)
    );
    let o = run(dir, &["query", "--loc", "db", "ASK { <urn:z> ?p ?o }"]);
    assert!(
        out(&o).contains("no") || out(&o).contains("false"),
        "{}",
        out(&o)
    );
}

#[test]
fn rdfpatch_prints_rows_and_counts() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("p.rdfp"), P).unwrap();
    let o = run(dir, &["rdfpatch", "p.rdfp"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), P);
    assert!(
        err(&o).contains("# Data:     Adds=2 Deletes=0"),
        "{}",
        err(&o)
    );
    assert!(
        err(&o).contains("# Txn:      TX=1, TC=1, TA=0"),
        "{}",
        err(&o)
    );
    // Jena's binary patch reads as its text form
    let o = run(dir, &["rdfpatch", jena("jena-1.trp").to_str().unwrap()]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("PA \"foaf\" <http://xmlns.com/foaf/0.1/> ."),
        "{}",
        out(&o)
    );
    assert!(
        err(&o).contains("# Prefixes: Adds=2 Deletes=1"),
        "{}",
        err(&o)
    );
    let o = run(dir, &["rdfpatch", "--format", "binary", "p.rdfp"]);
    assert_eq!(o.status.code(), Some(1));
}

/// A free port in 5520–5539.
#[cfg(feature = "auth")]
fn port() -> u16 {
    (5520..5540)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("a free port in 5520-5539")
}

#[cfg(feature = "auth")]
#[test]
fn patch_through_a_server() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("p.rdfp"), P).unwrap();
    let port = port();
    let mut child = Command::new(BIN)
        .args(["serve", "--port", &port.to_string(), "--mem", "ds"])
        .arg("--data")
        .arg(dir.join("data"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let t0 = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let o = run(
        dir,
        &["patch", "--server", &base, "--dataset", "ds", "p.rdfp"],
    );
    let _ = child.kill();
    let _ = child.wait();
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        err(&o).starts_with("inserted 2 · deleted 0 · commit 1"),
        "{}",
        err(&o)
    );
}
