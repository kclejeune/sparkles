//! `sparkles schema` run as the real binary: subject classes per predicate and the
//! SHACL constraints layer, from a shapes graph and from the write-time validation.

#![cfg(feature = "shacl")]

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const DATA: &str = r#"@prefix ex: <http://ex.org/> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:a a ex:Person ; ex:name "A" .
ex:o a ex:Org ; ex:name "O" .
ex:shapes {
  ex:PersonShape a sh:NodeShape ;
    sh:targetClass ex:Person ;
    sh:property [ sh:path ex:name ; sh:minCount 1 ; sh:datatype xsd:string ] .
}
"#;

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

#[test]
fn subject_classes_and_constraints() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("data.trig"), DATA).unwrap();
    let o = sparkles(dir, &["load", "--loc", "db", "data.trig"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));

    // no write-time validation: no constraints layer
    let o = sparkles(dir, &["schema", "--loc", "db", "--format", "json"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(j.get("constraints").is_none(), "{j}");

    let o = sparkles(
        dir,
        &[
            "schema",
            "--loc",
            "db",
            "--subject-classes",
            "--shapes",
            "http://ex.org/shapes",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let s = out(&o);
    assert!(
        s.contains("    subjects: <http://ex.org/Org> 1, <http://ex.org/Person> 1\n"),
        "{s}"
    );
    assert!(
        s.contains("constraints from shapes graphs, validated on request: <http://ex.org/shapes>"),
        "{s}"
    );
    assert!(
        s.contains(
            "    <http://ex.org/name>  min 1 · datatype xsd:string  [validated-on-request]\n"
        ),
        "{s}"
    );

    // the write-time validation's shapes are the layer by default
    let o = sparkles(
        dir,
        &[
            "validation",
            "--loc",
            "db",
            "--mode",
            "reject",
            "--shapes-graph",
            "http://ex.org/shapes",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    let o = sparkles(dir, &["schema", "--loc", "db", "--format", "json"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let src = &j["constraints"]["sources"][0];
    assert_eq!(src["kind"], "guard", "{j}");
    assert_eq!(
        src["classes"][0]["properties"][0]["enforcement"],
        "reject-on-write"
    );
    let o = sparkles(dir, &["schema", "--loc", "db", "--shapes", "none"]);
    assert!(!out(&o).contains("constraints from"), "{}", out(&o));

    for bad in [
        &["--shapes", "union"][..],
        &["--shapes", "http://ex.org/missing"],
        &["--shapes", "none", "--shapes", "guard"],
    ] {
        let mut args = vec!["schema", "--loc", "db"];
        args.extend_from_slice(bad);
        let o = sparkles(dir, &args);
        assert_eq!(o.status.code(), Some(1), "{bad:?}: {}", out(&o));
    }
}

#[test]
fn profiles_and_diffs() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("data.trig"), DATA).unwrap();
    let o = sparkles(dir, &["load", "--loc", "db", "data.trig"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = sparkles(
        dir,
        &[
            "update",
            "--loc",
            "db",
            "PREFIX ex: <http://ex.org/> INSERT DATA { ex:b a ex:Person ; ex:name \"B\", \"Bee\" ; ex:worksFor ex:o }",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));

    let o = sparkles(dir, &["schema", "--loc", "db", "--profiles"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    assert!(
        text.contains("<http://ex.org/Person>  instances 2"),
        "{text}"
    );
    assert!(
        text.contains("<http://ex.org/name>  instances 2/2  triples 3  values 1..2  string 3"),
        "{text}"
    );
    assert!(
        text.contains("<http://ex.org/worksFor>  instances 1/2"),
        "{text}"
    );
    assert!(text.contains("→ Org 1"), "{text}");
    assert!(
        text.contains("← <http://ex.org/worksFor>  triples 1  instances 1"),
        "{text}"
    );
    let o = sparkles(
        dir,
        &[
            "schema",
            "--loc",
            "db",
            "--profiles",
            "--class",
            "http://ex.org/Org",
            "--format",
            "json",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["classes"].as_array().unwrap().len(), 1);
    assert_eq!(j["classes"][0]["class"], "http://ex.org/Org");

    // the load is commit 1, the update commit 2
    let o = sparkles(dir, &["schema", "--loc", "db", "--diff", "1"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    assert!(
        text.contains("schema diff from commit 1 to commit 2"),
        "{text}"
    );
    assert!(
        text.contains("+ predicate http://ex.org/worksFor (triples 1)"),
        "{text}"
    );
    assert!(
        text.contains("~ class http://ex.org/Person: observed.instances 1 -> 2"),
        "{text}"
    );
    let o = sparkles(
        dir,
        &[
            "schema", "--loc", "db", "--diff", "commit:2", "--to", "2", "--format", "json",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["counts"]["classesChanged"], 0);
    // --class needs --profiles or --draft-shapes
    let o = sparkles(
        dir,
        &["schema", "--loc", "db", "--class", "http://ex.org/Org"],
    );
    assert_ne!(o.status.code(), Some(0));
}
