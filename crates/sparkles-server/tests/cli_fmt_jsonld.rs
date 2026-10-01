//! `sparkles fmt` on JSON-LD, run as the real binary: walks pick `.jsonld` files,
//! `--check` exit codes, `--sort` from the command line and from `.sparklesfmt.toml`,
//! and the errors JSON-LD has beyond syntax (duplicate keys, comments) with their
//! positions.

#![cfg(feature = "fmt")]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const DOC: &str = r#"{"name":"Alice","@id":"http://example.org/alice","@context":{"name":"http://schema.org/name","age":"http://schema.org/age"},"age":42}"#;

const PRETTY: &str = r#"{
  "@context": {
    "name": "http://schema.org/name",
    "age": "http://schema.org/age"
  },
  "@id": "http://example.org/alice",
  "name": "Alice",
  "age": 42
}
"#;

const SORTED: &str = r#"{
  "@context": {
    "age": "http://schema.org/age",
    "name": "http://schema.org/name"
  },
  "@id": "http://example.org/alice",
  "age": 42,
  "name": "Alice"
}
"#;

/// `sparkles fmt ARGS` in `dir` with stdin, and the home and config directories inside it.
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

#[track_caller]
fn expect(dir: &Path, args: &[&str], stdin: Option<&str>, code: i32) -> Output {
    let o = fmt_in(dir, args, stdin);
    assert_eq!(
        o.status.code(),
        Some(code),
        "{args:?}\nstdout: {}\nstderr: {}",
        stdout(&o),
        stderr(&o)
    );
    o
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn walks_check_and_sort() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    write(dir, "w/a.jsonld", DOC);
    write(dir, "w/b.jsonld", PRETTY);
    // plain .json is never JSON-LD in a walk
    write(dir, "w/c.json", DOC);
    let o = expect(dir, &["-l", "w"], None, 1);
    assert_eq!(stdout(&o), "w/a.jsonld\n");
    expect(dir, &["--check", "w/b.jsonld"], None, 0);
    expect(dir, &["--check", "w/a.jsonld"], None, 1);

    let o = expect(dir, &["w/a.jsonld"], None, 0);
    assert_eq!(stdout(&o), PRETTY);
    let o = expect(dir, &["--sort", "w/a.jsonld"], None, 0);
    assert_eq!(stdout(&o), SORTED);
    let o = expect(dir, &["--language", "jsonld"], Some(DOC), 0);
    assert_eq!(stdout(&o), PRETTY);

    write(dir, "w/.sparklesfmt.toml", "sort = true\n");
    expect(dir, &["--write", "w"], None, 0);
    assert_eq!(
        std::fs::read_to_string(dir.join("w/a.jsonld")).unwrap(),
        SORTED
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("w/b.jsonld")).unwrap(),
        SORTED
    );
    assert_eq!(std::fs::read_to_string(dir.join("w/c.json")).unwrap(), DOC);
}

#[test]
fn duplicate_keys_and_comments_are_errors() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let dup = "{\n  \"@id\": \"ex:a\",\n  \"name\": \"A\",\n  \"name\": \"B\"\n}\n";
    write(dir, "dup.jsonld", dup);
    let o = expect(dir, &["dup.jsonld"], None, 2);
    assert!(
        stderr(&o).starts_with("dup.jsonld:4:3: error: "),
        "{}",
        stderr(&o)
    );
    assert!(
        stderr(&o).contains("duplicate key \"name\""),
        "{}",
        stderr(&o)
    );

    let commented = "{\n  // who\n  \"@id\": \"ex:a\"\n}\n";
    write(dir, "c.jsonld", commented);
    let o = expect(dir, &["--write", "c.jsonld"], None, 2);
    assert!(
        stderr(&o).starts_with("c.jsonld:2:3: error: "),
        "{}",
        stderr(&o)
    );
    assert!(
        stderr(&o).contains("comments are not allowed in JSON"),
        "{}",
        stderr(&o)
    );
    // left untouched
    assert_eq!(
        std::fs::read_to_string(dir.join("c.jsonld")).unwrap(),
        commented
    );
}
