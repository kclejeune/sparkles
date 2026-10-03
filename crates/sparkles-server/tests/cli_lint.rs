//! `sparkles lint` as the real binary (spec X03): text and JSON reports, exit statuses,
//! `--fix` on files and stdin, `--rule` and the config file's `[lint]` table, directory
//! walks and `--list-rules`.

#![cfg(feature = "fmt")]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn run_stdin(dir: &Path, args: &[&str], input: &str) -> Output {
    let mut c = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    c.wait_with_output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const QUERY: &str = "PREFIX ex: <http://example.org/>\nPREFIX unused: <http://u/>\nSELECT ?s ?name { ?s ex:name ?n FILTER(?n = \"x\"@en-us) }\n";

const TURTLE: &str = "@prefix ex: <http://example.org/> .\nex:a ex:b \"1.5\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n";

fn setup() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("q")).unwrap();
    std::fs::write(d.path().join("q/a.rq"), QUERY).unwrap();
    std::fs::write(d.path().join("q/b.ttl"), TURTLE).unwrap();
    std::fs::write(d.path().join("q/c.nt"), "<a> <b> <c> .\n").unwrap();
    d
}

#[test]
fn reports_and_exit_statuses() {
    let d = setup();
    let dir = d.path();
    // warnings only: exit 0; the walk skips the N-Triples file
    let o = run(dir, &["lint", "q"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    assert!(
        text.contains(
            "q/a.rq:2:1: warning [unused-prefix] the prefix unused: is declared but never used"
        ),
        "{text}"
    );
    assert!(
        text.contains("q/a.rq:3:11: warning [unbound-variable]"),
        "{text}"
    );
    assert!(text.contains("[language-tag-case]"), "{text}");
    assert!(
        text.contains("q/b.ttl:2:11: warning [suspicious-datatype]"),
        "{text}"
    );
    assert!(!text.contains("c.nt"), "{text}");
    assert!(err(&o).contains("problems ("), "{}", err(&o));
    // --strict fails on warnings, and --rule raises one to an error
    assert_eq!(run(dir, &["lint", "--strict", "q"]).status.code(), Some(1));
    let o = run(dir, &["lint", "--rule", "unused-prefix=error", "q/a.rq"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("error [unused-prefix]"));
    // a syntax error is an error
    let o = run_stdin(dir, &["lint", "--language", "sparql"], "SELECT * {");
    assert_eq!(o.status.code(), Some(1));
    assert!(
        out(&o).starts_with("<stdin>:1:11: error [syntax]"),
        "{}",
        out(&o)
    );
    // files lint does not take, and bad flags
    let o = run(dir, &["lint", "q/c.nt"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("N-Triples is not linted"), "{}", err(&o));
    assert_eq!(
        run(dir, &["lint", "--rule", "nope=off", "q"]).status.code(),
        Some(2)
    );
    assert_eq!(
        run(dir, &["lint", "--format", "xml", "q"]).status.code(),
        Some(2)
    );
}

#[test]
fn json_report() {
    let d = setup();
    let o = run(d.path(), &["lint", "--format", "json", "q/a.rq"]);
    assert_eq!(o.status.code(), Some(0));
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let file = &doc["files"][0];
    assert_eq!(file["path"], "q/a.rq");
    assert_eq!(file["language"], "sparql");
    let first = &file["diagnostics"][0];
    assert_eq!(first["rule"], "unused-prefix");
    assert_eq!(first["severity"], "warning");
    assert_eq!(
        (first["line"].as_u64(), first["column"].as_u64()),
        (Some(2), Some(1))
    );
    assert_eq!(first["fixable"], true);
    assert_eq!(doc["summary"]["warnings"], 3, "{doc}");
}

#[test]
fn fixes_in_place_and_on_stdin() {
    let d = setup();
    let dir = d.path();
    let o = run(dir, &["lint", "--fix", "q/a.rq"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        err(&o).contains("fixed 2 problems in 1 file"),
        "{}",
        err(&o)
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("q/a.rq")).unwrap(),
        "PREFIX ex: <http://example.org/>\nSELECT ?s ?name { ?s ex:name ?n FILTER(?n = \"x\"@en-US) }\n"
    );
    // what is left is reported
    assert!(out(&o).contains("[unbound-variable]"), "{}", out(&o));
    // stdin: the fixed text on stdout, the findings on stderr
    let o = run_stdin(
        dir,
        &["lint", "--fix", "--stdin-filepath", "x.ttl"],
        "@prefix ex: <http://e/> .\n@prefix u: <http://u/> .\nex:a ex:b \"c\"^^<http://www.w3.org/2001/XMLSchema#string> .\n",
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "@prefix ex: <http://e/> .\nex:a ex:b \"c\" .\n");
}

#[test]
fn the_config_file_sets_levels_and_fmt_ignores_them() {
    let d = setup();
    let dir = d.path();
    std::fs::write(
        dir.join("q/.sparklesfmt.toml"),
        "indent-width = 2\n\n[lint]\nunused-prefix = \"off\"\nsuspicious-datatype = \"error\"\n",
    )
    .unwrap();
    let o = run(dir, &["lint", "q"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(!out(&o).contains("unused-prefix"), "{}", out(&o));
    assert!(
        out(&o).contains("error [suspicious-datatype]"),
        "{}",
        out(&o)
    );
    // the formatter reads the same file
    let o = run(dir, &["fmt", "q/b.ttl"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    // a bad level names the file and the key
    std::fs::write(
        dir.join("q/.sparklesfmt.toml"),
        "[lint]\nunused-prefix = \"loud\"\n",
    )
    .unwrap();
    let o = run(dir, &["lint", "q"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("lint.unused-prefix"), "{}", err(&o));
}

#[test]
fn list_rules() {
    let d = tempfile::tempdir().unwrap();
    let o = run(d.path(), &["lint", "--list-rules"]);
    assert_eq!(o.status.code(), Some(0));
    let text = out(&o);
    assert!(text.contains("cartesian-product"), "{text}");
    assert!(
        text.lines()
            .any(|l| l.starts_with("unused-prefix") && l.contains("yes")),
        "{text}"
    );
}
