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

const OFN: &str = "Prefix(:=<http://ex.org/>)
Ontology(<http://ex.org/o>
  Declaration(Class(:Professor))
  Declaration(Class(:Course))
  Declaration(ObjectProperty(:teaches))
  SubClassOf(:Professor ObjectSomeValuesFrom(:teaches :Course))
)
";

#[test]
fn functional_syntax_files_check_and_load() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("onto.ofn"), OFN).unwrap();
    let o = sparkles(
        dir,
        &["ontology", "check", "onto.ofn", "--profile", "el,dl"],
    );
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    assert!(out(&o).contains("OWL 2 EL: conforms"), "{}", out(&o));
    // the file loads as the triples of its axioms, which read back as the same axioms
    let o = sparkles(dir, &["load", "--loc", "db", "onto.ofn"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = sparkles(
        dir,
        &["ontology", "axioms", "--loc", "db", "--type", "SubClassOf"],
    );
    assert!(
        out(&o).contains("SubClassOf(:Professor ObjectSomeValuesFrom(:teaches :Course))"),
        "{}",
        out(&o)
    );
    // a syntax error names the file and the position
    std::fs::write(dir.join("bad.ofn"), "Ontology(\n  SubClassOf(:A))").unwrap();
    let o = sparkles(dir, &["ontology", "check", "bad.ofn"]);
    assert_ne!(o.status.code(), Some(0));
    assert!(
        err(&o).contains("bad.ofn") && err(&o).contains("line 2"),
        "{}",
        err(&o)
    );
}

const EL: &str = "Prefix(:=<http://ex.org/>)
Ontology(<http://ex.org/el>
  Declaration(Class(:Professor))
  Declaration(Class(:Course))
  Declaration(Class(:Teacher))
  Declaration(Class(:Rock))
  Declaration(ObjectProperty(:teaches))
  SubClassOf(:Professor ObjectSomeValuesFrom(:teaches :Course))
  EquivalentClasses(:Teacher ObjectSomeValuesFrom(:teaches :Course))
  SubClassOf(:Rock :Course)
  DisjointClasses(:Rock :Course)
)
";

#[test]
fn classify_a_file_and_a_database() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("el.ofn"), EL).unwrap();
    // an unsatisfiable class: exit status 1, unless allowed
    let o = sparkles(dir, &["ontology", "classify", "el.ofn", "--format", "text"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("unsatisfiable http://ex.org/Rock"), "{s}");
    let o = sparkles(
        dir,
        &["ontology", "classify", "el.ofn", "--allow-unsatisfiable"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("SubClassOf(:Professor :Teacher)"), "{s}");
    let o = sparkles(
        dir,
        &[
            "ontology",
            "classify",
            "el.ofn",
            "--allow-unsatisfiable",
            "--format",
            "json",
            "--about",
            "http://ex.org/Professor",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["unsatisfiableCount"], 1, "{j}");
    assert!(
        j["class"]["superClasses"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("http://ex.org/Teacher")),
        "{j}"
    );
    // a file classifies without a database, so it takes no target
    let o = sparkles(
        dir,
        &[
            "ontology",
            "classify",
            "el.ofn",
            "--target",
            "http://ex.org/t",
        ],
    );
    assert_ne!(o.status.code(), Some(0));
    // a database: classify, then write the taxonomy to a named graph
    let o = sparkles(dir, &["load", "--loc", "db", "el.ofn"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = sparkles(
        dir,
        &[
            "ontology",
            "classify",
            "--loc",
            "db",
            "--allow-unsatisfiable",
            "--target",
            "http://ex.org/taxonomy",
            "--message",
            "classified",
            "--format",
            "json",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["written"]["committed"], true, "{j}");
    let o = sparkles(
        dir,
        &[
            "ontology",
            "axioms",
            "--loc",
            "db",
            "--graph",
            "http://ex.org/taxonomy",
            "--type",
            "SubClassOf",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("SubClassOf(:Professor :Teacher)"),
        "{}",
        out(&o)
    );
}

/// Item 603's cases in functional-style syntax, as the `sparkles` crate's tests write
/// them: a classification that the disjunctive rules take, long enough that a deadline
/// stops it part way.
fn cases(n: usize, inconsistent: bool) -> String {
    let mut s = String::from(
        "Prefix(:=<http://ex.org/>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n",
    );
    for i in 0..n {
        let (b, d) = ((7 * i + 3) % n, (13 * i + 5) % n);
        s.push_str(&format!(
            "SubClassOf(:C{i} ObjectUnionOf(:B{i} :E{i}))
             SubClassOf(:B{i} :K{i}) SubClassOf(:B{i} :C{b}) SubClassOf(:E{i} :K{i})
             SubClassOf(:K{i} ObjectSomeValuesFrom(:r :C{d}))
             SubClassOf(:E{i} ObjectAllValuesFrom(ObjectInverseOf(:r) :F{d}))
             SubClassOf(ObjectSomeValuesFrom(:r :F{i}) :G{b})\n"
        ));
    }
    if inconsistent {
        s.push_str(
            "SubClassOf(owl:Thing ObjectSomeValuesFrom(:s ObjectUnionOf(:N1 :N2)))
             SubClassOf(:N1 owl:Nothing) SubClassOf(:N2 :N3) DisjointClasses(:N3 :N2)\n",
        );
    }
    s.push_str(")\n");
    s
}

/// Classifies the cases with `--partial` under deadlines from a fraction of a full run's
/// time upwards, checks each answer and its exit status, and returns the partial
/// answers' consistencies.
fn partial_on_the_command_line(inconsistent: bool) -> Vec<String> {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("dl.ofn"), cases(300, inconsistent)).unwrap();
    let args = [
        "ontology",
        "classify",
        "dl.ofn",
        "--threads",
        "1",
        "--format",
        "json",
    ];
    let o = sparkles(dir, &args);
    let full: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        o.status.code(),
        Some(if inconsistent { 1 } else { 0 }),
        "{}",
        err(&o)
    );
    assert_eq!(full["partial"], false, "{full}");
    assert_eq!(full["tier"], "disjunctive", "{full}");
    let want = if inconsistent {
        "inconsistent"
    } else {
        "consistent"
    };
    assert_eq!(full["consistency"], want, "{full}");
    let s = &full["stats"];
    let took = (s["indexMs"].as_f64().unwrap()
        + s["saturationMs"].as_f64().unwrap()
        + s["taxonomyMs"].as_f64().unwrap())
        / 1000.0;
    let mut seen = Vec::new();
    let mut f = 0.02;
    while f < 1.5 {
        let t = format!("{}", (took * f).max(1e-6));
        f *= 1.25;
        let o = sparkles(dir, &[&args[..], &["--partial", "--timeout", &t]].concat());
        let Ok(j) = serde_json::from_slice::<serde_json::Value>(&o.stdout) else {
            // a stop before the saturation fails as before
            assert_eq!(o.status.code(), Some(1), "{}", err(&o));
            continue;
        };
        assert_eq!(j["classes"], full["classes"], "{j}");
        let clean = j["consistent"] == true && j["unsatisfiableCount"] == 0;
        if j["partial"] == false {
            assert_eq!(j["complete"], true, "{j}");
            assert_eq!(
                o.status.code(),
                Some(if clean { 0 } else { 1 }),
                "{}",
                err(&o)
            );
            continue;
        }
        assert_eq!(
            j["complete"], false,
            "a partial answer is presented as complete: {j}"
        );
        assert_eq!(
            o.status.code(),
            Some(if clean { 2 } else { 1 }),
            "{}",
            err(&o)
        );
        assert!(err(&o).contains("partial answer"), "{}", err(&o));
        let c = j["consistency"].as_str().unwrap().to_string();
        assert!(c == want || c == "unknown", "{j}");
        assert_eq!(
            j["incompleteCount"].as_u64(),
            Some(j["incomplete"].as_array().unwrap().len() as u64),
            "{j}"
        );
        seen.push(c);
    }
    seen
}

#[test]
fn classify_answers_in_part_under_a_deadline() {
    assert!(
        !partial_on_the_command_line(false).is_empty(),
        "no deadline gave a partial answer"
    );
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("dl.ofn"), cases(3, false)).unwrap();
    // a functional-style answer, a write and a chain cannot say that they are partial
    for extra in [
        &["--format", "ofn"][..],
        &["--format", "json", "--target", "http://ex.org/t"][..],
        &["--format", "json", "relax"][..],
    ] {
        let o = sparkles(
            dir,
            &[&["ontology", "classify", "dl.ofn", "--partial"][..], extra].concat(),
        );
        assert_ne!(o.status.code(), Some(0), "{extra:?}");
    }
    let o = sparkles(
        dir,
        &["ontology", "classify", "dl.ofn", "--expansion", "greedy"],
    );
    assert_ne!(o.status.code(), Some(0));
    // a run that finishes is complete, with either strategy, and its text says so
    let o = sparkles(
        dir,
        &[
            "ontology",
            "classify",
            "dl.ofn",
            "--partial",
            "--expansion",
            "eager",
            "--format",
            "text",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).starts_with("consistent:"), "{}", out(&o));
    assert!(!out(&o).contains("partial"), "{}", out(&o));
}

#[test]
fn classify_leaves_the_consistency_unknown_when_stopped_early() {
    let seen = partial_on_the_command_line(true);
    assert!(seen.iter().any(|c| c == "unknown"), "{seen:?}");
}
