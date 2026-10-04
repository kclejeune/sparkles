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

#[test]
fn shacl_parse_converts_between_syntaxes() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("shapes.shaclc"), SHAPES).unwrap();

    // SHACLC to Turtle, and the Turtle back to SHACLC, read to the same graph
    let o = sparkles(dir, &["shacl", "parse", "shapes.shaclc", "--out", "turtle"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let ttl = String::from_utf8(o.stdout).unwrap();
    assert!(ttl.contains("sh:targetClass ex:Person"), "{ttl}");
    std::fs::write(dir.join("shapes.ttl"), &ttl).unwrap();
    let o = sparkles(dir, &["shacl", "p", "shapes.ttl"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let shc = String::from_utf8(o.stdout).unwrap();
    assert!(shc.contains("shape ex:PersonShape -> ex:Person {"), "{shc}");
    let a = sparkles_shacl::compact::parse(SHAPES, None).unwrap().graph;
    let b = sparkles_shacl::compact::parse(&shc, None).unwrap().graph;
    assert_eq!(canonical(&a), canonical(&b), "{shc}");

    // several files each get a header; stdin needs --in for SHACLC
    let o = sparkles(
        dir,
        &[
            "shacl",
            "parse",
            "shapes.ttl",
            "shapes.shaclc",
            "--out",
            "nt",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let nt = String::from_utf8(o.stdout).unwrap();
    assert!(nt.starts_with("# shapes.ttl\n"), "{nt}");
    assert!(nt.contains("\n# shapes.shaclc\n"), "{nt}");
    let mut child = Command::new(BIN)
        .args(["shacl", "parse", "-", "--in", "shaclc", "--out", "ttl"])
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(SHAPES.as_bytes()).unwrap();
    }
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).contains("sh:maxInclusive 150"));

    // a graph with triples SHACLC cannot express, ill-formed SHACL, an unknown syntax
    std::fs::write(
        dir.join("extra.ttl"),
        format!("{ttl}\nex:PersonShape <http://ex.org/note> \"n\" .\n"),
    )
    .unwrap();
    let o = sparkles(dir, &["shacl", "parse", "extra.ttl"]);
    assert_ne!(o.status.code(), Some(0));
    assert!(text(&o).contains("has no SHACLC form"), "{}", text(&o));
    std::fs::write(
        dir.join("bad.ttl"),
        "@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:s> a sh:NodeShape ; sh:minCount \"x\" .",
    )
    .unwrap();
    let o = sparkles(dir, &["shacl", "parse", "bad.ttl", "--out", "turtle"]);
    assert_ne!(o.status.code(), Some(0), "{}", text(&o));
    let o = sparkles(dir, &["shacl", "parse", "shapes.ttl", "--out", "yaml"]);
    assert_ne!(o.status.code(), Some(0));
    assert!(text(&o).contains("unknown output syntax"), "{}", text(&o));
    // validation still needs --shapes, and its flags do not mix with parse
    let o = sparkles(dir, &["shacl", "--data", "x.ttl"]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    let o = sparkles(dir, &["shacl", "--data", "x.ttl", "parse", "shapes.ttl"]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
}

/// The triples of a graph without blank nodes, sorted, and the number of all triples.
fn canonical(g: &oxrdf::Graph) -> (Vec<String>, usize) {
    let mut v: Vec<String> = g
        .iter()
        .filter(|t| {
            !matches!(t.subject, oxrdf::NamedOrBlankNodeRef::BlankNode(_))
                && !matches!(t.object, oxrdf::TermRef::BlankNode(_))
        })
        .map(|t| t.to_string())
        .collect();
    v.sort();
    (v, g.len())
}
