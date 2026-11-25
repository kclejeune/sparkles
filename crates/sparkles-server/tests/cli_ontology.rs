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

/// Classifies the cases with `--partial` under deadlines that find the saturation, checks
/// each answer and its exit status, and returns the partial answers' consistencies. The
/// deadlines rise by 8 percent from 2 ms until one of them gives an answer instead of a
/// timeout, which has run out in the saturation, since that is the first part of a run
/// that can stop with an answer. The step may be longer than the saturation, so the
/// milliseconds between the last timeout and that answer are halved until they are one
/// apart. The deadlines go on until twenty have answered. A ladder fixed from the time of
/// a full run misses a saturation of some milliseconds on a loaded machine. The command
/// has no memory budget to stop a run at a fixed place, so the deadline decides.
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
    let limit = ((took * 20000.0) as u64).max(2000);
    let seen = std::cell::RefCell::new(Vec::new());
    let answered = std::cell::Cell::new(0);
    // whether the command answered under a deadline of `ms` milliseconds
    let run = |ms: u64| -> bool {
        let t = format!("{}", ms as f64 / 1000.0);
        let o = sparkles(dir, &[&args[..], &["--partial", "--timeout", &t]].concat());
        let Ok(j) = serde_json::from_slice::<serde_json::Value>(&o.stdout) else {
            // a stop before the saturation fails as before
            assert_eq!(o.status.code(), Some(1), "{}", err(&o));
            return false;
        };
        answered.set(answered.get() + 1);
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
            return true;
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
        seen.borrow_mut().push(c);
        true
    };
    let after = |ms: u64| ms + (ms * 8 / 100).max(1);
    let (mut lo, mut ms) = (0, 2);
    while ms < limit && !run(ms) {
        lo = ms;
        ms = after(ms);
    }
    let mut hi = ms;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if run(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    while answered.get() < 20 && ms < limit {
        ms = after(ms);
        run(ms);
    }
    seen.into_inner()
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
    assert!(out(&o).contains("full disjunctive run on "), "{}", out(&o));
    assert!(!out(&o).contains("partial"), "{}", out(&o));

    // A stopped partial run in `--format text` prints `partial:`, the tier, and `incomplete`.
    let big = dir.join("dl-big.ofn");
    std::fs::write(
        &big,
        cases(if cfg!(debug_assertions) { 120 } else { 400 }, false),
    )
    .unwrap();
    let full = sparkles(
        dir,
        &[
            "ontology",
            "classify",
            "dl-big.ofn",
            "--threads",
            "1",
            "--format",
            "json",
        ],
    );
    assert_eq!(full.status.code(), Some(0), "{}", err(&full));
    let fj: serde_json::Value = serde_json::from_slice(&full.stdout).unwrap();
    let s = &fj["stats"];
    let took = (s["indexMs"].as_f64().unwrap()
        + s["saturationMs"].as_f64().unwrap()
        + s["taxonomyMs"].as_f64().unwrap())
        / 1000.0;
    let limit = ((took * 20000.0) as u64).max(2000);
    let saw_partial_text = std::cell::Cell::new(false);
    let answered = std::cell::Cell::new(0usize);
    let run_text = |ms: u64| -> bool {
        let timeout = format!("{}", ms as f64 / 1000.0);
        let o = sparkles(
            dir,
            &[
                "ontology",
                "classify",
                "dl-big.ofn",
                "--partial",
                "--threads",
                "1",
                "--timeout",
                &timeout,
                "--about",
                "http://ex.org/C0",
                "--format",
                "text",
            ],
        );
        if o.status.code() == Some(2) {
            let text = out(&o);
            assert!(text.contains("partial:"), "{text}");
            assert!(text.contains("incomplete "), "{text}");
            assert!(text.contains("disjunctive run on "), "{text}");
            if text.contains("incomplete http://ex.org/C0") {
                assert!(
                    text.contains("  incomplete   yes (superclasses may be missing)"),
                    "{text}"
                );
            }
            answered.set(answered.get() + 1);
            saw_partial_text.set(true);
            return true;
        }
        if o.status.code() == Some(0) {
            answered.set(answered.get() + 1);
            return true;
        }
        false
    };
    let after = |ms: u64| ms + (ms * 8 / 100).max(1);
    let (mut lo, mut ms) = (0u64, 2u64);
    while ms < limit && !run_text(ms) {
        lo = ms;
        ms = after(ms);
    }
    let mut hi = ms;
    while hi - lo > 1 && !saw_partial_text.get() {
        let mid = (lo + hi) / 2;
        if run_text(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    while !saw_partial_text.get() && answered.get() < 20 && ms < limit {
        ms = after(ms);
        run_text(ms);
    }
    assert!(
        saw_partial_text.get(),
        "no deadline gave a partial --format text answer"
    );
}

#[test]
fn classify_leaves_the_consistency_unknown_when_stopped_early() {
    let seen = partial_on_the_command_line(true);
    assert!(seen.iter().any(|c| c == "unknown"), "{seen:?}");
}

/// An ontology where a run with its assertion and without the rooted rules leaves the union
/// out and misses K ⊑ Ans (roadmap item 1271). ROBOT's commands classify the assertions too.
const IRL1: &str = "Prefix(:=<http://example.org/irl#>)
Ontology(<http://example.org/irl#irl1>
Declaration(Class(:K)) Declaration(Class(:S)) Declaration(Class(:D1)) Declaration(Class(:D2)) Declaration(Class(:Ans))
Declaration(ObjectProperty(:r)) Declaration(ObjectProperty(:s)) Declaration(NamedIndividual(:a))
SubObjectPropertyOf(:r :s)
SubClassOf(:K ObjectSomeValuesFrom(:r :S))
SubClassOf(:S ObjectUnionOf(:D1 :D2))
SubClassOf(:D1 ObjectAllValuesFrom(ObjectInverseOf(:s) :Ans))
SubClassOf(:D2 ObjectAllValuesFrom(ObjectInverseOf(:s) :Ans))
ClassAssertion(:K :a)
)
";

#[test]
fn a_classification_that_leaves_an_axiom_out_is_incomplete_or_refused() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("irl1.ofn"), IRL1).unwrap();
    let o = sparkles(
        dir,
        &[
            "ontology",
            "classify",
            "irl1.ofn",
            "--assertions",
            "--format",
            "json",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["complete"], false, "{j}");
    assert_eq!(j["completeForTaken"], true, "{j}");
    assert_eq!(j["partial"], false, "{j}");
    assert_eq!(j["consistency"], "consistent", "{j}");
    assert_eq!(
        j["incomplete"],
        serde_json::json!(["http://example.org/irl#K", "http://example.org/irl#S"]),
        "{j}"
    );
    assert!(err(&o).contains("incomplete answer"), "{}", err(&o));
    let o = sparkles(
        dir,
        &[
            "ontology",
            "classify",
            "irl1.ofn",
            "--assertions",
            "--about",
            "http://example.org/irl#K",
            "--format",
            "text",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("incomplete http://example.org/irl#K"),
        "{}",
        out(&o)
    );
    assert!(
        out(&o).contains("  incomplete   yes (superclasses may be missing)"),
        "{}",
        out(&o)
    );
    // strict: no answer, and the error names the axiom and the rule
    let o = sparkles(
        dir,
        &[
            "ontology",
            "classify",
            "irl1.ofn",
            "--assertions",
            "--strict",
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(out(&o).is_empty(), "{}", out(&o));
    let e = err(&o);
    assert!(e.contains("would not be complete"), "{e}");
    assert!(e.contains("ObjectUnionOf"), "{e}");
    assert!(e.contains("OWL 2 Profiles"), "{e}");
    // ROBOT's reason with --strict true fails as ROBOT does, and writes nothing
    let o = sparkles(
        dir,
        &[
            "ontology", "reason", "--input", "irl1.ofn", "--strict", "true", "--output", "out.ofn",
        ],
    );
    assert_ne!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        err(&o).contains("INCOMPLETE REASONING ERROR"),
        "{}",
        err(&o)
    );
    assert!(!dir.join("out.ofn").exists());
    let o = sparkles(
        dir,
        &[
            "ontology", "reason", "--input", "irl1.ofn", "--output", "out.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("--strict true refuses"), "{}", err(&o));
    std::fs::remove_file(dir.join("out.ofn")).unwrap();
    // HermiT and JFact are strict by default, and --strict false answers as before.
    // `reduce` classifies ROBOT's subset of the subclass axioms and the property
    // characteristics, which in irl1 the classifier takes whole, so its input holds a
    // maximum of two, which needs counting.
    std::fs::write(
        dir.join("count.ofn"),
        "Prefix(:=<http://example.org/c#>)
Ontology(<http://example.org/c>
Declaration(Class(:A)) Declaration(Class(:B)) Declaration(ObjectProperty(:r))
SubClassOf(:A ObjectMaxCardinality(2 :r :B))
)
",
    )
    .unwrap();
    for (cmd, r, input) in [
        ("reason", "hermit", "irl1.ofn"),
        ("reduce", "JFact", "count.ofn"),
        ("materialize", "hermit", "irl1.ofn"),
    ] {
        let o = sparkles(
            dir,
            &[
                "ontology",
                cmd,
                "--input",
                input,
                "--reasoner",
                r,
                "--output",
                "out.ofn",
            ],
        );
        assert_ne!(o.status.code(), Some(0), "{cmd} {r}: {}", err(&o));
        assert!(
            err(&o).contains("INCOMPLETE REASONING ERROR"),
            "{cmd} {r}: {}",
            err(&o)
        );
        assert!(!dir.join("out.ofn").exists(), "{cmd} {r}");
        let o = sparkles(
            dir,
            &[
                "ontology",
                cmd,
                "--input",
                input,
                "--reasoner",
                r,
                "--strict",
                "false",
                "--output",
                "out.ofn",
            ],
        );
        assert_eq!(o.status.code(), Some(0), "{cmd} {r}: {}", err(&o));
        assert!(
            err(&o).contains("--strict true refuses"),
            "{cmd} {r}: {}",
            err(&o)
        );
        std::fs::remove_file(dir.join("out.ofn")).unwrap();
    }
    // ELK stays a warning
    let o = sparkles(
        dir,
        &[
            "ontology",
            "reason",
            "--input",
            "irl1.ofn",
            "--reasoner",
            "ELK",
            "--output",
            "out.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("--strict true refuses"), "{}", err(&o));
    // an ontology the classifier takes whole is complete, strict or not
    std::fs::write(
        dir.join("el.ofn"),
        "Prefix(:=<http://ex.org/>)\nOntology(SubClassOf(:A :B))\n",
    )
    .unwrap();
    let o = sparkles(
        dir,
        &[
            "ontology", "classify", "el.ofn", "--strict", "--format", "json",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["complete"], true, "{j}");
}

const DL663_TRAILING_SUPERROLES: &str = include_str!(
    "../../sparkles-owl/tests/fixtures/dl663_top_chain_default/03-trailing-superroles.ofn"
);

fn dl663_write_input(dir: &Path) {
    // A ⊑ E is direct in ELK and redundant through D in the default reasoner.
    let input = DL663_TRAILING_SUPERROLES.replacen(
        "Ontology(",
        "Ontology(
         Declaration(Class(:A)) Declaration(Class(:B)) Declaration(Class(:D))
         Declaration(Class(:E))
         Declaration(ObjectProperty(:r)) Declaration(ObjectProperty(:s))
         Declaration(ObjectProperty(:t)) Declaration(ObjectProperty(:g))
         SubClassOf(:A :E) SubClassOf(:D :E)
         ",
        1,
    );
    std::fs::write(dir.join("chain.ofn"), input).unwrap();
}

fn dl663_transform(
    dir: &Path,
    command: &str,
    output: &str,
    extra: &[&str],
) -> Vec<sparkles::owl::Axiom> {
    let mut args = vec![
        "ontology",
        command,
        "--input",
        "chain.ofn",
        "--output",
        output,
    ];
    args.extend_from_slice(extra);
    let o = sparkles(dir, &args);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let doc = std::fs::read_to_string(dir.join(output)).unwrap();
    sparkles::owl::ofn_read::parse(&doc).unwrap().axioms
}

fn dl663_subclass(sub: &str, sup: &str) -> sparkles::owl::Axiom {
    sparkles::owl::Axiom::sub_class_of(
        sparkles::owl::Class::new(format!("urn:top-domain:{sub}")),
        sparkles::owl::Class::new(format!("urn:top-domain:{sup}")),
    )
}

#[test]
fn dl663_cli_file_classify_uses_the_default_reasoner() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    dl663_write_input(dir);
    let o = sparkles(
        dir,
        &[
            "ontology",
            "classify",
            "chain.ofn",
            "--about",
            "urn:top-domain:A",
            "--format",
            "json",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(j["complete"], false, "{j}");
    assert_eq!(j["completeForTaken"], true, "{j}");
    assert_eq!(j["excludedCount"], 1, "{j}");
    assert_eq!(
        j["excluded"][0]["partial"], "property chains through the top object property",
        "{j}",
    );
    assert!(
        j["class"]["superClasses"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("urn:top-domain:D")),
        "dl663 missing: CLI file classification domain",
    );
    println!("dl663 ok: CLI file classification domain");
}

#[test]
fn dl663_cli_default_reason_writes_the_named_consequence() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    dl663_write_input(dir);
    let axioms = dl663_transform(dir, "reason", "reasoned.ofn", &[]);
    assert!(axioms.contains(&dl663_subclass("D", "E")));
    assert!(!axioms.contains(&dl663_subclass("C", "D")));
    assert!(
        axioms.contains(&dl663_subclass("A", "D")),
        "dl663 missing: CLI default reason domain",
    );
    println!("dl663 ok: CLI default reason domain");
}

#[test]
fn dl663_cli_default_reduce_removes_the_now_redundant_axiom() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    dl663_write_input(dir);
    let axioms = dl663_transform(
        dir,
        "reduce",
        "reduced.ofn",
        &["--named-classes-only", "true"],
    );
    assert!(axioms.contains(&dl663_subclass("D", "E")));
    assert!(
        !axioms.contains(&dl663_subclass("A", "E")),
        "dl663 missing: CLI default reduce redundancy",
    );
    println!("dl663 ok: CLI default reduce redundancy");
}

#[test]
fn dl663_cli_explicit_elk_and_emr_keep_legacy_answers() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    dl663_write_input(dir);
    for reasoner in ["ELK", "emr"] {
        let axioms = dl663_transform(dir, "reason", "legacy.ofn", &["--reasoner", reasoner]);
        assert!(axioms.contains(&dl663_subclass("A", "E")), "{reasoner}");
        assert!(!axioms.contains(&dl663_subclass("A", "D")), "{reasoner}");
    }
    let axioms = dl663_transform(
        dir,
        "reduce",
        "reduced.ofn",
        &["--named-classes-only", "true", "--reasoner", "ELK"],
    );
    assert!(axioms.contains(&dl663_subclass("A", "E")));
    assert!(axioms.contains(&dl663_subclass("D", "E")));
    println!("dl663 ok: CLI explicit ELK and emr parity");
}

#[test]
fn dl663_cli_materialize_defaults_to_elk() {
    use sparkles::owl::{Axiom, Class, ClassExpression, ObjectProperty};

    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    dl663_write_input(dir);
    let args = ["--term", "urn:top-domain:r"];
    let default = dl663_transform(dir, "materialize", "default.ofn", &args);
    let explicit = dl663_transform(
        dir,
        "materialize",
        "elk.ofn",
        &[&args[..], &["--reasoner", "ELK"]].concat(),
    );
    let ordinary = Axiom::sub_class_of(
        Class::new("urn:top-domain:A"),
        ClassExpression::some(
            ObjectProperty::new("urn:top-domain:r"),
            Class::new("urn:top-domain:B"),
        ),
    );
    assert!(default.contains(&ordinary));
    assert!(!default.contains(&dl663_subclass("A", "D")));
    assert_eq!(default, explicit);
    println!("dl663 ok: CLI omitted materialize reasoner is ELK");
}
