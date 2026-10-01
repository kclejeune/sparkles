//! `sparkles shex` run as the real binary: exit statuses (0 conformant, 1 nonconformant,
//! 2 usage, parse and schema errors), report formats, imports relative to the schema
//! file, and `parse` (ShExC and ShExJ out, and back).

#![cfg(feature = "shex")]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const DATA: &str = r#"@prefix ex: <http://ex.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
ex:alice a foaf:Person ; foaf:name "Alice" ; foaf:knows ex:bob .
ex:bob a foaf:Person ; foaf:name "Bob" .
ex:carol a foaf:Person ; foaf:knows ex:alice .
"#;

const SCHEMA: &str = "PREFIX ex: <http://ex.org/>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
start = @ex:Person
ex:Person { foaf:name xsd:string ; foaf:knows @ex:Person * }
";

fn shex(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("shex")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn setup() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("data.ttl"), DATA).unwrap();
    std::fs::write(d.path().join("person.shex"), SCHEMA).unwrap();
    d
}

#[test]
fn exit_statuses() {
    let d = setup();
    let dir = d.path();
    // conformant: 0, Jena's OK
    let o = shex(
        dir,
        &[
            "validate",
            "-s",
            "person.shex",
            "-d",
            "data.ttl",
            "-n",
            "<http://ex.org/alice>",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "OK\n");
    // nonconformant: 1, one line per association
    let o = shex(
        dir,
        &[
            "v",
            "--shapes",
            "person.shex",
            "--datafile",
            "data.ttl",
            "--target",
            "<http://ex.org/carol>",
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(
        out(&o).starts_with(
            "<http://ex.org/carol> @ START :: Focus = <http://ex.org/carol>, Status = nonconformant"
        ),
        "{}",
        out(&o)
    );
    // a query map and the other formats
    let o = shex(
        dir,
        &[
            "validate",
            "-s",
            "person.shex",
            "-d",
            "data.ttl",
            "--shape-map",
            "{FOCUS a foaf:Person}@ex:Person",
            "--format",
            "smap",
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    let lines: Vec<String> = out(&o).lines().map(str::to_string).collect();
    assert_eq!(lines.len(), 3, "{}", out(&o));
    assert!(lines.contains(&"<http://ex.org/carol>@!<http://ex.org/Person>".to_string()));
    assert!(lines.contains(&"<http://ex.org/alice>@<http://ex.org/Person>".to_string()));
    let o = shex(
        dir,
        &[
            "validate",
            "-s",
            "person.shex",
            "-d",
            "data.ttl",
            "--shape-map",
            "{FOCUS a foaf:Person}@ex:Person",
            "--format",
            "json",
            "--only-nonconformant",
            "--stats",
        ],
    );
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["counts"]["conformant"], 2);
    assert_eq!(j["results"].as_array().unwrap().len(), 1);
    assert!(j["stats"]["pairs"].as_u64().unwrap() >= 3);
    // a JSON map file
    std::fs::write(
        dir.join("map.json"),
        r#"[{"node": "http://ex.org/bob", "shape": "http://ex.org/Person"}]"#,
    )
    .unwrap();
    let o = shex(
        dir,
        &[
            "validate",
            "-s",
            "person.shex",
            "-d",
            "data.ttl",
            "-m",
            "map.json",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
}

#[test]
fn usage_and_schema_errors_exit_2() {
    let d = setup();
    let dir = d.path();
    std::fs::write(
        dir.join("bad.shex"),
        "PREFIX ex: <http://ex.org/>\nex:S { ex:p @@ }",
    )
    .unwrap();
    std::fs::write(dir.join("nostart.shex"), "<http://ex.org/S> { }").unwrap();
    std::fs::write(dir.join("missing.shex"), "start = @<http://ex.org/Missing>").unwrap();
    for (args, needle) in [
        (
            vec!["validate", "-s", "bad.shex", "-d", "data.ttl", "-n", "<x>"],
            "line 2",
        ),
        (
            vec![
                "validate",
                "-s",
                "nostart.shex",
                "-d",
                "data.ttl",
                "-n",
                "<x>",
            ],
            "the schema has no start shape; give --shape",
        ),
        (
            vec![
                "validate",
                "-s",
                "missing.shex",
                "-d",
                "data.ttl",
                "-n",
                "<x>",
            ],
            "Missing",
        ),
        (
            vec![
                "validate",
                "-s",
                "person.shex",
                "-d",
                "data.ttl",
                "-n",
                "<x>",
                "--shape",
                "<http://ex.org/Nope>",
            ],
            "Nope",
        ),
        (
            vec![
                "validate",
                "-s",
                "person.shex",
                "-d",
                "data.ttl",
                "--shape-map",
                "<x>@@",
            ],
            "shape map",
        ),
        (
            vec![
                "validate",
                "-s",
                "person.shex",
                "-d",
                "data.ttl",
                "-n",
                "<x>",
                "--graph",
                "http://ex.org/none",
            ],
            "no such graph",
        ),
        (vec!["validate", "-s", "person.shex", "-d", "data.ttl"], ""),
    ] {
        let o = shex(dir, &args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
        assert!(err(&o).contains(needle), "{args:?}: {}", err(&o));
    }
}

#[test]
fn imports_resolve_against_the_schema_directory() {
    let d = setup();
    let dir = d.path();
    std::fs::create_dir(dir.join("schemas")).unwrap();
    std::fs::write(
        dir.join("schemas/common.shex"),
        "<http://ex.org/Named> { <http://xmlns.com/foaf/0.1/name> . }",
    )
    .unwrap();
    std::fs::write(
        dir.join("schemas/knower.shex"),
        "IMPORT <common>\n<http://ex.org/K> { <http://xmlns.com/foaf/0.1/knows> @<http://ex.org/Named> }",
    )
    .unwrap();
    let o = shex(
        dir,
        &[
            "validate",
            "-s",
            "schemas/knower.shex",
            "-d",
            "data.ttl",
            "-n",
            "<http://ex.org/carol>",
            "--shape",
            "<http://ex.org/K>",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
}

#[test]
fn parse_prints_a_structural_dump() {
    let d = setup();
    let o = shex(d.path(), &["parse", "person.shex", "--out", "text"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("http://ex.org/Person"), "{}", out(&o));
    let o = shex(d.path(), &["p", "nowhere.shex"]);
    assert_eq!(o.status.code(), Some(2));
}

/// `parse -` with `input` on stdin.
fn shex_stdin(dir: &Path, args: &[&str], input: &str) -> Output {
    let mut c = Command::new(BIN)
        .arg("shex")
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

/// ShExC → ShExJ → ShExC → ShExJ through the command line: the two ShExJ documents are
/// the same, and so is the ShExC written from each.
fn round_trip(dir: &Path, file: &str) {
    let j1 = shex(dir, &["parse", file, "--out", "shexj"]);
    assert_eq!(j1.status.code(), Some(0), "{}", err(&j1));
    let j1: serde_json::Value = serde_json::from_slice(&j1.stdout).unwrap();
    assert_eq!(j1["type"], "Schema");
    let c = shex(dir, &["parse", file, "--out", "shexc"]);
    assert_eq!(c.status.code(), Some(0), "{}", err(&c));
    let base = format!(
        "file://{}",
        std::path::absolute(dir.join(file)).unwrap().display()
    );
    // ShExJ on stdin (sniffed), out as ShExC
    let c2 = shex_stdin(
        dir,
        &["parse", "-", "--out", "shexc", "--base", &base],
        &j1.to_string(),
    );
    assert_eq!(c2.status.code(), Some(0), "{}", err(&c2));
    // that ShExC on stdin, out as ShExJ again
    let j2 = shex_stdin(
        dir,
        &["parse", "-", "--out", "shexj", "--base", &base],
        &out(&c2),
    );
    assert_eq!(j2.status.code(), Some(0), "{}\n{}", err(&j2), out(&c2));
    let j2: serde_json::Value = serde_json::from_slice(&j2.stdout).unwrap();
    assert_eq!(j1, j2, "\n{}", out(&c2));
}

#[test]
fn parse_writes_shexc_and_shexj() {
    let d = setup();
    let o = shex(d.path(), &["parse", "person.shex"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let c = out(&o);
    assert!(
        c.contains("PREFIX foaf: <http://xmlns.com/foaf/0.1/>"),
        "{c}"
    );
    assert!(c.contains("start = @ex:Person"), "{c}");
    assert!(c.contains("foaf:knows @ex:Person*"), "{c}");
    round_trip(d.path(), "person.shex");
    // annotations, semantic actions, value sets, facets and groups survive both forms
    std::fs::write(
        d.path().join("rich.shex"),
        r#"PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
        %ex:init{ go %}
        ex:S EXTRA a CLOSED {
          a [ex:T ex:U~] ;
          ex:n xsd:string /^[A-Z]\/x/i MAXLENGTH 9 // ex:note "name"@en %ex:act{ 50\% %} ;
          ( ex:p @ex:S * | ^ex:q [. - ex:x - ex:y~] {2,3} ) + ;
          ex:age xsd:integer MININCLUSIVE 0 MAXEXCLUSIVE 150.5 ?
        } // ex:label "S"
        ex:V IRI AND NOT @ex:S
        "#,
    )
    .unwrap();
    round_trip(d.path(), "rich.shex");
}

fn suite_schemas() -> Option<PathBuf> {
    let p = std::env::var("SPARKLES_SHEX_TESTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../apache/jena/jena-shex/src/test/files/spec")
        })
        .join("schemas");
    p.exists().then_some(p)
}

/// A schema of the shexTest suite (when it is there) through both forms.
#[test]
fn parse_round_trips_a_suite_schema() {
    let Some(dir) = suite_schemas() else {
        eprintln!("shexTest suite not found (set SPARKLES_SHEX_TESTS); skipped");
        return;
    };
    let d = tempfile::tempdir().unwrap();
    for f in [
        "kitchenSink.shex",
        "1literalPattern_with_all_punctuation.shex",
    ] {
        std::fs::copy(dir.join(f), d.path().join(f)).unwrap();
        round_trip(d.path(), f);
    }
}
