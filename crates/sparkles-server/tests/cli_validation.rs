//! `sparkles validation` with ShEx run as the real binary: enabling `reject` (exit 1
//! while the data does not conform), a rejected `sparkles update` (exit 3, the
//! nonconformant associations on stderr), `--status` and `sparkles stats`, and the
//! flags of the other language.

#![cfg(feature = "shex")]

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const DATA: &str = r#"@prefix ex: <http://ex.org/> . @prefix foaf: <http://xmlns.com/foaf/0.1/> .
ex:alice a ex:Person ; foaf:name "Alice" ; foaf:age 30 ; foaf:knows ex:bob .
ex:bob a ex:Person ; foaf:name "Bob" ; foaf:knows ex:alice .
ex:carol a ex:Person ; foaf:age 200 .
"#;

const SCHEMA: &str = "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
ex:Person EXTRA a { a [ex:Person] ; foaf:name xsd:string ;
  foaf:age xsd:integer MAXINCLUSIVE 150 ? ; foaf:knows @ex:Person * }
";

const MAP: &str = "{FOCUS a ex:Person}@ex:Person";

fn sparkles(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
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

fn enable(dir: &Path, extra: &[&str]) -> Output {
    let mut args = vec![
        "validation",
        "--loc",
        "db",
        "--schema",
        "s.shex",
        "--shape-map",
        MAP,
        "--mode",
        "reject",
    ];
    args.extend_from_slice(extra);
    sparkles(dir, &args)
}

#[test]
fn shex_validation_from_the_command_line() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("data.ttl"), DATA).unwrap();
    std::fs::write(dir.join("s.shex"), SCHEMA).unwrap();
    let o = sparkles(dir, &["load", "--loc", "db", "data.ttl"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));

    // carol does not conform: exit 1, the summary on stdout
    let o = enable(dir, &["--lang", "shex"]);
    assert_eq!(o.status.code(), Some(1), "{}{}", out(&o), err(&o));
    assert!(
        err(&o).contains("1 nonconformant associations"),
        "{}",
        err(&o)
    );
    assert!(out(&o).contains("\"language\": \"shex\""), "{}", out(&o));

    let fix = "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> \
               DELETE DATA { ex:carol foaf:age 200 } ; INSERT DATA { ex:carol foaf:name \"Carol\" }";
    let o = sparkles(dir, &["update", "--loc", "db", fix]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    // --schema implies --lang shex
    let o = enable(dir, &[]);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    assert!(
        out(&o).starts_with("validation on: 3 associations (0 nonconformant)"),
        "{}",
        out(&o)
    );

    // a person without a name is rejected: exit 3, the association on stderr
    let o = sparkles(
        dir,
        &[
            "update",
            "--loc",
            "db",
            "INSERT DATA { <http://ex.org/dave> a <http://ex.org/Person> }",
        ],
    );
    assert_eq!(o.status.code(), Some(3), "{}{}", out(&o), err(&o));
    let e = err(&o);
    assert!(
        e.contains("ShEx validation failed: 1 nonconformant association"),
        "{e}"
    );
    assert!(
        e.contains("  http://ex.org/dave @ http://ex.org/Person: "),
        "{e}"
    );

    let o = sparkles(dir, &["validation", "--loc", "db", "--status"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("validation reject\nlanguage   shex\n"), "{s}");
    assert!(s.contains("shapes     1 shapes"), "{s}");
    let o = sparkles(
        dir,
        &["validation", "--loc", "db", "--status", "--format", "json"],
    );
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["language"], "shex");
    assert_eq!(j["config"]["schema"]["file"], "validation-schema.shex");

    let o = sparkles(dir, &["stats", "--loc", "db"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("validation      reject · ShEx · 1 shapes"),
        "{}",
        out(&o)
    );

    // off: writes are no longer validated
    let o = sparkles(dir, &["validation", "--loc", "db", "--off"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(!dir.join("db/validation-schema.shex").exists());
    let o = sparkles(
        dir,
        &[
            "update",
            "--loc",
            "db",
            "INSERT DATA { <http://ex.org/dave> a <http://ex.org/Person> }",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
}

#[test]
fn flags_of_the_other_language_are_usage_errors() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("s.shex"), SCHEMA).unwrap();
    for extra in [
        &["--threshold", "warning"][..],
        &["--lang", "shacl"][..],
        &["--shapes", "s.ttl"][..],
    ] {
        let o = enable(dir, extra);
        assert_ne!(o.status.code(), Some(0), "{extra:?}");
        assert!(err(&o).contains("SHACL"), "{extra:?}: {}", err(&o));
    }
}
