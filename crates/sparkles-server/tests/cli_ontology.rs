//! `sparkles ontology` run as the real binary: the summary, the axioms, edits and
//! profile checks of a database, and the check of a file, whose exit status says
//! whether the ontology is in the profiles asked for.

#![cfg(feature = "ontology")]

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const DATA: &str = r#"@prefix : <http://ex.org/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
:o a owl:Ontology .
:Professor a owl:Class . :Course a owl:Class .
:teaches a owl:ObjectProperty .
:Professor rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :teaches ; owl:someValuesFrom :Course ] .
"#;

fn sparkles(dir: &Path, args: &[&str]) -> Output {
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

#[test]
fn summary_axioms_edit_and_check_a_database() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("onto.ttl"), DATA).unwrap();
    let o = sparkles(dir, &["load", "--loc", "db", "onto.ttl"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));

    let o = sparkles(dir, &["ontology", "summary", "--loc", "db"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("ontology:  http://ex.org/o"), "{s}");
    assert!(s.contains("SubClassOf"), "{s}");

    let o = sparkles(
        dir,
        &["ontology", "axioms", "--loc", "db", "--type", "SubClassOf"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("SubClassOf(:Professor ObjectSomeValuesFrom(:teaches :Course))"),
        "{}",
        out(&o)
    );

    // RL forbids the existential superclass: exit status 1
    let o = sparkles(
        dir,
        &["ontology", "check", "--loc", "db", "--profile", "el,rl"],
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("OWL 2 EL: conforms"), "{s}");
    assert!(s.contains("OWL 2 RL: 1 violations"), "{s}");

    // remove the axiom and the RL check passes
    let axiom = r#"{"type": "SubClassOf", "subClass": "http://ex.org/Professor",
        "superClass": {"type": "ObjectSomeValuesFrom", "property": "http://ex.org/teaches", "class": "http://ex.org/Course"}}"#;
    std::fs::write(dir.join("remove.json"), axiom).unwrap();
    let o = sparkles(
        dir,
        &[
            "ontology",
            "edit",
            "--loc",
            "db",
            "--remove",
            "remove.json",
            "--format",
            "json",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["removed"], 1, "{j}");
    assert_eq!(j["deleted"], 4, "{j}");
    let o = sparkles(
        dir,
        &[
            "ontology",
            "check",
            "--loc",
            "db",
            "--profile",
            "rl",
            "--format",
            "json",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["conforms"], true, "{j}");
}

#[test]
fn check_a_file_offline() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("onto.ttl"), DATA).unwrap();
    let o = sparkles(dir, &["ontology", "check", "onto.ttl", "--profile", "all"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("OWL 2 DL: conforms"), "{s}");
    assert!(s.contains("OWL 2 QL: conforms"), "{s}");
    // without a header the ontology is outside OWL 2 DL
    std::fs::write(
        dir.join("bare.ttl"),
        DATA.replace(":o a owl:Ontology .", ""),
    )
    .unwrap();
    let o = sparkles(dir, &["ontology", "check", "bare.ttl"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(out(&o).contains("§3.1.2"), "{}", out(&o));
    // a strict read leaves the axioms of an undeclared property unmapped
    std::fs::write(
        dir.join("loose.ttl"),
        DATA.replace(":teaches a owl:ObjectProperty .", ""),
    )
    .unwrap();
    let o = sparkles(dir, &["ontology", "check", "loose.ttl", "--mode", "strict"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(out(&o).contains("maps to no axiom"), "{}", out(&o));
    // a lenient read takes it, and the DL check reports the missing declaration
    let o = sparkles(dir, &["ontology", "check", "loose.ttl"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(out(&o).contains("§5.8.1"), "{}", out(&o));
    // a missing target is an error, not a check
    let o = sparkles(dir, &["ontology", "summary"]);
    assert_ne!(o.status.code(), Some(0));
    assert!(err(&o).contains("--loc"), "{}", err(&o));
}
