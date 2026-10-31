//! `sparkles fmt` on SHACL Compact Syntax, run as the real binary: extension detection,
//! `--language`, `--check` exit codes and the errors with their positions.

#![cfg(feature = "fmt")]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const DOC: &str = "prefix ex: <http://example.org/>\nshape   ex:S -> ex:C {ex:p IRI [1..1].}\n";

const FORMATTED: &str =
    "PREFIX ex: <http://example.org/>\n\nshape ex:S -> ex:C {\n  ex:p IRI [1..1] .\n}\n";

fn fmt_in(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(BIN)
        .arg("fmt")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(stdin.unwrap_or("").as_bytes()).unwrap();
    drop(input);
    child.wait_with_output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn formats_by_extension_and_by_flag() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("s.shaclc"), DOC).unwrap();
    let o = fmt_in(dir.path(), &["s.shaclc"], None);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), FORMATTED);
    // the same text on stdin, named by flag and by --stdin-filepath
    let o = fmt_in(dir.path(), &["--language", "shaclc"], Some(DOC));
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), FORMATTED);
    let o = fmt_in(dir.path(), &["--stdin-filepath", "x/y.shc"], Some(DOC));
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), FORMATTED);
    // the output is a fixpoint
    let o = fmt_in(dir.path(), &["--language", "shaclc"], Some(FORMATTED));
    assert_eq!(stdout(&o), FORMATTED);
}

#[test]
fn check_write_and_walks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("shapes")).unwrap();
    std::fs::write(dir.path().join("shapes/a.shaclc"), DOC).unwrap();
    std::fs::write(dir.path().join("shapes/b.shaclc"), FORMATTED).unwrap();
    let o = fmt_in(dir.path(), &["--check", "shapes"], None);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).contains("a.shaclc"), "{}", stderr(&o));
    assert!(!stderr(&o).contains("b.shaclc"), "{}", stderr(&o));
    let o = fmt_in(dir.path(), &["--write", "shapes"], None);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let written = std::fs::read_to_string(dir.path().join("shapes/a.shaclc")).unwrap();
    assert_eq!(written, FORMATTED);
    let o = fmt_in(dir.path(), &["--check", "shapes"], None);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
}

#[test]
fn options_apply() {
    let dir = tempfile::tempdir().unwrap();
    let doc = "PREFIX ex: <http://example.org/>\nPREFIX unused: <http://unused.example/>\n\nshape ex:S -> ex:C {\n  ex:p IRI [1..1] .\n}\n";
    let o = fmt_in(
        dir.path(),
        &["--language", "shaclc", "--prune-prefixes"],
        Some(doc),
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(!stdout(&o).contains("unused"), "{}", stdout(&o));
}

#[test]
fn syntax_errors_have_positions() {
    let dir = tempfile::tempdir().unwrap();
    let o = fmt_in(
        dir.path(),
        &["--language", "shaclc"],
        Some("PREFIX ex: <http://example.org/>\nshape ex:S {\n"),
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains(": error:"), "{}", stderr(&o));
    assert!(stdout(&o).is_empty());
}
