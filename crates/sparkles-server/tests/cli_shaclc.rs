//! SHACLC on the command line, run as the real binary: `sparkles shacl --shapes
//! file.shaclc`, `sparkles validation --shapes file.shaclc` (stored as Turtle) and
//! `sparkles schema --draft-shapes --format shaclc`.

#![cfg(feature = "shacl")]

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const DATA: &str = r#"@prefix ex: <http://ex.org/> . @prefix foaf: <http://xmlns.com/foaf/0.1/> .
ex:alice a ex:Person ; foaf:name "Alice" ; foaf:age 30 .
ex:bob a ex:Person ; foaf:name "Bob" .
ex:carol a ex:Person ; foaf:age 200 .
"#;

const SHAPES: &str = "PREFIX ex: <http://ex.org/>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
shape ex:PersonShape -> ex:Person {
    foaf:name xsd:string [1..1] .
    foaf:age xsd:integer [0..1] maxInclusive=150 .
}
";

fn sparkles(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn shaclc_files_on_the_command_line() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("data.ttl"), DATA).unwrap();
    std::fs::write(dir.join("shapes.shaclc"), SHAPES).unwrap();
    std::fs::write(dir.join("bad.shc"), "shape {").unwrap();

    // validation of files in memory: carol has no name and is too old
    let o = sparkles(
        dir,
        &[
            "shacl",
            "--data",
            "data.ttl",
            "--shapes",
            "shapes.shaclc",
            "--format",
            "json",
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let report: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(report["results"].as_array().unwrap().len(), 2, "{report}");
    let o = sparkles(dir, &["shacl", "--data", "data.ttl", "--shapes", "bad.shc"]);
    assert_ne!(o.status.code(), Some(0));
    assert!(text(&o).contains("SHACLC syntax error"), "{}", text(&o));

    // write-time validation from a SHACLC file: refused while carol does not conform
    let o = sparkles(dir, &["load", "--loc", "db", "data.ttl"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let enable = [
        "validation",
        "--loc",
        "db",
        "--shapes",
        "shapes.shaclc",
        "--mode",
        "reject",
    ];
    let o = sparkles(dir, &enable);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let fix = "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> \
               DELETE DATA { ex:carol foaf:age 200 } ; INSERT DATA { ex:carol foaf:name \"Carol\" }";
    let o = sparkles(dir, &["update", "--loc", "db", fix]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let o = sparkles(dir, &enable);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    // stored as Turtle
    let stored = std::fs::read_to_string(dir.join("db/validation-shapes.ttl")).unwrap();
    assert!(stored.contains("@prefix"), "{stored}");
    let o = sparkles(
        dir,
        &[
            "update",
            "--loc",
            "db",
            "PREFIX ex: <http://ex.org/> INSERT DATA { ex:dave a ex:Person }",
        ],
    );
    assert_eq!(o.status.code(), Some(3), "{}", text(&o));

    // drafted shapes as SHACLC
    let o = sparkles(
        dir,
        &[
            "schema",
            "--loc",
            "db",
            "--draft-shapes",
            "--format",
            "shaclc",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let draft = String::from_utf8(o.stdout).unwrap();
    assert!(
        draft.contains("shape shape:PersonShape -> ex:Person {"),
        "{draft}"
    );
    sparkles_shacl::compact::parse(&draft, None).unwrap_or_else(|e| panic!("{e}\n{draft}"));
}
