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
fn classify_el_and_horn_partial_under_deadline() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();

    let mut el_ofn = String::from(
        "Prefix(:=<http://ex.org/>)\nOntology(\n\
         TransitiveObjectProperty(:part)\n\
         SubObjectPropertyOf(ObjectPropertyChain(:r1 :r2) :s)\n",
    );
    let en = if cfg!(debug_assertions) { 100 } else { 300 };
    for i in 0..en {
        let next = (i + 1) % en;
        el_ofn.push_str(&format!(
            "SubClassOf(:A{i} :A{next})\n\
             SubClassOf(:A{i} ObjectSomeValuesFrom(:r1 :B{i}))\n\
             SubClassOf(:B{i} ObjectSomeValuesFrom(:r2 :C{next}))\n\
             SubClassOf(:C{i} ObjectSomeValuesFrom(:part :P{next}))\n\
             SubClassOf(:P{i} ObjectSomeValuesFrom(:part :P{next}))\n\
             SubClassOf(ObjectSomeValuesFrom(:s :C{i}) :D{i})\n",
        ));
    }
    el_ofn.push_str(")\n");

    let mut horn_ofn = String::from(
        "Prefix(:=<http://ex.org/>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n\
         InverseObjectProperties(:r :rinv)\n",
    );
    let hn = if cfg!(debug_assertions) { 90 } else { 300 };
    for i in 0..hn {
        let b = (7 * i + 3) % hn;
        let d_idx = (11 * i + 5) % hn;
        horn_ofn.push_str(&format!(
            "SubClassOf(:H{i} ObjectSomeValuesFrom(:r :M{i}))\n\
             SubClassOf(:H{i} ObjectAllValuesFrom(:r :F{b}))\n\
             SubClassOf(:H{i} ObjectMaxCardinality(1 :r owl:Thing))\n\
             SubClassOf(:H{i} ObjectSomeValuesFrom(:r :G{d_idx}))\n\
             SubClassOf(:M{i} ObjectAllValuesFrom(:rinv :Back{i}))\n",
        ));
    }
    horn_ofn.push_str(")\n");

    for (tier, file, src) in [("el", "el.ofn", el_ofn), ("horn", "horn.ofn", horn_ofn)] {
        std::fs::write(dir.join(file), src).unwrap();
        let args = [
            "ontology",
            "classify",
            file,
            "--threads",
            "1",
            "--format",
            "json",
        ];
        let o = sparkles(dir, &args);
        assert_eq!(o.status.code(), Some(0), "{tier}: {}", err(&o));
        let full: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(full["tier"], tier, "{tier}: {full}");
        assert_eq!(full["partial"], false, "{tier}: {full}");
        let s = &full["stats"];
        let took = (s["indexMs"].as_f64().unwrap()
            + s["saturationMs"].as_f64().unwrap()
            + s["taxonomyMs"].as_f64().unwrap())
            / 1000.0;
        let limit = ((took * 20000.0) as u64).max(2000);
        let saw_partial = std::cell::Cell::new(false);
        let answered = std::cell::Cell::new(0usize);
        let run = |ms: u64| -> bool {
            let t = format!("{}", ms as f64 / 1000.0);
            let o = sparkles(dir, &[&args[..], &["--partial", "--timeout", &t]].concat());
            let Ok(j) = serde_json::from_slice::<serde_json::Value>(&o.stdout) else {
                assert_eq!(o.status.code(), Some(1), "{tier}: {}", err(&o));
                return false;
            };
            answered.set(answered.get() + 1);
            assert_eq!(j["tier"], tier, "{tier}: {j}");
            if j["partial"] == true {
                assert_eq!(j["complete"], false, "{tier}: {j}");
                assert_eq!(o.status.code(), Some(2), "{tier}: {}", err(&o));
                assert!(err(&o).contains("partial answer"), "{tier}: {}", err(&o));
                saw_partial.set(true);
            }
            true
        };
        let after = |ms: u64| ms + (ms * 8 / 100).max(1);
        let (mut lo, mut ms) = (0, 2);
        while ms < limit && !run(ms) {
            lo = ms;
            ms = after(ms);
        }
        let mut hi = ms;
        while hi - lo > 1 && !saw_partial.get() {
            let mid = (lo + hi) / 2;
            if run(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        while !saw_partial.get() && answered.get() < 20 && ms < limit {
            ms = after(ms);
            run(ms);
        }
        assert!(
            saw_partial.get(),
            "{tier}: no deadline gave a partial CLI answer"
        );
    }
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

const WRITER_ONTO: &str = r#"Prefix(:=<http://ex.org/>)
Prefix(obo:=<http://purl.obolibrary.org/obo/>)
Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)
Ontology(<http://purl.obolibrary.org/obo/demo.owl>
Declaration(Class(obo:GO_0000001))
Declaration(Class(obo:GO_0000002))
Declaration(Class(obo:GO_0000003))
Declaration(Class(obo:GO_0000004))
Declaration(ObjectProperty(obo:BFO_0000050))
AnnotationAssertion(rdfs:label obo:GO_0000001 "cerebellar neuron")
AnnotationAssertion(rdfs:label obo:GO_0000002 "hindbrain neuron")
AnnotationAssertion(rdfs:label obo:GO_0000003 "neuron")
AnnotationAssertion(rdfs:label obo:GO_0000004 "cerebellum")
EquivalentClasses(obo:GO_0000001 ObjectIntersectionOf(obo:GO_0000003 ObjectSomeValuesFrom(obo:BFO_0000050 obo:GO_0000004)))
SubClassOf(obo:GO_0000004 obo:GO_0000002)
SubClassOf(obo:GO_0000001 obo:GO_0000003)
TransitiveObjectProperty(obo:BFO_0000050)
)
"#;

#[test]
fn ontology_writers_owx_omn_obo_and_obographs_cli() {
    use sparkles::owl::{
        Annotation, AnnotationProperty, AnnotationValue, Axiom, Class, ClassExpression, Literal,
        ObjectProperty,
    };
    use std::collections::BTreeSet;
    use std::io::Read as _;

    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("demo.ofn"), WRITER_ONTO).unwrap();

    // Write all four new formats (plus .gz) in a single reason/relax/reduce chain
    let o = sparkles(
        dir,
        &[
            "ontology",
            "reason",
            "--input",
            "demo.ofn",
            "-a",
            "true",
            "-o",
            "reasoned.ofn",
            "-o",
            "reasoned.owx",
            "-o",
            "reasoned.omn",
            "-o",
            "reasoned.obo",
            "-o",
            "reasoned.json",
            "-o",
            "reasoned.owx.gz",
            "-o",
            "reasoned.obo.gz",
            "-o",
            "reasoned.json.gz",
            "relax",
            "-o",
            "relaxed.owx",
            "reduce",
            "-o",
            "reduced.omn",
            "-o",
            "reduced.obo",
            "-o",
            "reduced.json",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));

    // Reference axioms from .ofn
    let ofn_doc =
        sparkles::owl::ofn_read::parse(&std::fs::read_to_string(dir.join("reasoned.ofn")).unwrap())
            .unwrap();
    let ofn_set: BTreeSet<Axiom> = ofn_doc.axioms.into_iter().collect();

    // OWL/XML round-trips identically to .ofn
    let owx_text = std::fs::read_to_string(dir.join("reasoned.owx")).unwrap();
    let owx_doc = sparkles::owl::owx::parse(&owx_text).unwrap();
    assert_eq!(owx_doc.header, ofn_doc.header);
    assert_eq!(owx_doc.axioms.into_iter().collect::<BTreeSet<_>>(), ofn_set);

    // Gzipped OWL/XML matches plain OWL/XML byte-for-byte
    let mut gz_buf = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(dir.join("reasoned.owx.gz")).unwrap())
        .read_to_string(&mut gz_buf)
        .unwrap();
    assert_eq!(gz_buf, owx_text);

    // Manchester round-trips identically to .ofn
    let omn_text = std::fs::read_to_string(dir.join("reasoned.omn")).unwrap();
    let omn_doc = sparkles::owl::manchester::parse_document(&omn_text).unwrap();
    assert_eq!(omn_doc.header, ofn_doc.header);
    assert_eq!(omn_doc.axioms.into_iter().collect::<BTreeSet<_>>(), ofn_set);

    // OBO 1.4 contains OBO PURL IDs, trailing is_inferred qualifier, and parses back with expected axioms
    let obo_text = std::fs::read_to_string(dir.join("reasoned.obo")).unwrap();
    assert!(obo_text.contains("format-version: 1.4\n"), "{obo_text}");
    assert!(obo_text.contains("id: GO:0000001\n"), "{obo_text}");
    assert!(obo_text.contains("id: BFO:0000050\n"), "{obo_text}");
    assert!(
        obo_text.contains("is_a: GO:0000003 {is_inferred=\"true\"}\n"),
        "{obo_text}"
    );
    let obo_doc = sparkles::owl::obo::parse(&obo_text).unwrap();
    let go1 = Class::new("http://purl.obolibrary.org/obo/GO_0000001");
    let go3 = Class::new("http://purl.obolibrary.org/obo/GO_0000003");
    let go4 = Class::new("http://purl.obolibrary.org/obo/GO_0000004");
    let bfo50 = ObjectProperty::new("http://purl.obolibrary.org/obo/BFO_0000050");
    let inferred_sub = Axiom::sub_class_of(go1.clone(), go3.clone()).annotated(Annotation::new(
        AnnotationProperty::new(sparkles::owl::obo::OIO_IS_INFERRED),
        AnnotationValue::Literal(Literal::simple("true")),
    ));
    assert!(obo_doc.axioms.contains(&inferred_sub));

    // Gzipped OBO matches plain OBO
    let mut obo_gz = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(dir.join("reasoned.obo.gz")).unwrap())
        .read_to_string(&mut obo_gz)
        .unwrap();
    assert_eq!(obo_gz, obo_text);

    // OBO Graphs JSON is valid JSON, reports skipped TransitiveObjectProperty warning, and
    // preserves classes, edges and logical definitions
    assert!(
        err(&o).contains("reasoned.json") && err(&o).contains("omitted"),
        "lossy OBO Graphs JSON write must warn on stderr: {}",
        err(&o)
    );
    let json_text = std::fs::read_to_string(dir.join("reasoned.json")).unwrap();
    let json_doc = sparkles::owl::obographs::parse(&json_text).unwrap();
    assert!(json_doc.axioms.contains(&inferred_sub));
    let mut json_gz = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(dir.join("reasoned.json.gz")).unwrap())
        .read_to_string(&mut json_gz)
        .unwrap();
    assert_eq!(json_gz, json_text);

    // Relax output in .owx and reduce output in .obo/.json carry the relaxed existential
    let relaxed_owx =
        sparkles::owl::owx::parse(&std::fs::read_to_string(dir.join("relaxed.owx")).unwrap())
            .unwrap();
    let ex_ax = Axiom::sub_class_of(
        go1.clone(),
        ClassExpression::some(bfo50.clone(), go4.clone()),
    );
    assert!(relaxed_owx.axioms.contains(&ex_ax));
    let reduced_obo =
        sparkles::owl::obo::parse(&std::fs::read_to_string(dir.join("reduced.obo")).unwrap())
            .unwrap();
    assert!(reduced_obo.axioms.contains(&ex_ax));
    let reduced_json = sparkles::owl::obographs::parse(
        &std::fs::read_to_string(dir.join("reduced.json")).unwrap(),
    )
    .unwrap();
    assert!(reduced_json.axioms.contains(&ex_ax));

    // Materialize also writes all four formats
    let o = sparkles(
        dir,
        &[
            "ontology",
            "materialize",
            "-i",
            "demo.ofn",
            "--term",
            "BFO:0000050",
            "-o",
            "mat.owx",
            "-o",
            "mat.omn",
            "-o",
            "mat.obo",
            "-o",
            "mat.json",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    for f in ["mat.owx", "mat.omn", "mat.obo", "mat.json"] {
        assert!(dir.join(f).exists(), "{f}");
    }
    let mat_owx =
        sparkles::owl::owx::parse(&std::fs::read_to_string(dir.join("mat.owx")).unwrap()).unwrap();
    assert!(mat_owx.axioms.contains(&ex_ax));

    // Explicit --format flags on `classify` and `axioms`
    for fmt in ["owx", "omn", "obo", "obographs"] {
        let o = sparkles(dir, &["ontology", "classify", "demo.ofn", "--format", fmt]);
        assert_eq!(o.status.code(), Some(0), "{fmt}: {}", err(&o));
        let body = out(&o);
        match fmt {
            "owx" => assert!(sparkles::owl::owx::parse(&body).is_ok(), "{body}"),
            "omn" => assert!(
                sparkles::owl::manchester::parse_document(&body).is_ok(),
                "{body}"
            ),
            "obo" => assert!(sparkles::owl::obo::parse(&body).is_ok(), "{body}"),
            "obographs" => assert!(sparkles::owl::obographs::parse(&body).is_ok(), "{body}"),
            _ => unreachable!(),
        }
        // --partial refuses every document syntax format
        let p = sparkles(
            dir,
            &[
                "ontology",
                "classify",
                "demo.ofn",
                "--partial",
                "--format",
                fmt,
            ],
        );
        assert_ne!(p.status.code(), Some(0), "{fmt}");
        assert!(err(&p).contains("partial"), "{fmt}: {}", err(&p));
    }

    // Load into a local database and test `sparkles ontology axioms --format owx|omn|obo|obographs`
    let o = sparkles(dir, &["load", "--loc", "db", "demo.ofn"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    for fmt in ["owx", "omn", "obo", "obographs"] {
        let o = sparkles(dir, &["ontology", "axioms", "--loc", "db", "--format", fmt]);
        assert_eq!(o.status.code(), Some(0), "{fmt}: {}", err(&o));
        let body = out(&o);
        match fmt {
            "owx" => assert!(sparkles::owl::owx::parse(&body).is_ok(), "{body}"),
            "omn" => assert!(
                sparkles::owl::manchester::parse_document(&body).is_ok(),
                "{body}"
            ),
            "obo" => assert!(sparkles::owl::obo::parse(&body).is_ok(), "{body}"),
            "obographs" => assert!(sparkles::owl::obographs::parse(&body).is_ok(), "{body}"),
            _ => unreachable!(),
        }
    }
}

#[test]
fn ontology_writers_atomic_error_omn_gz_chained_classify_and_header_loss_cli() {
    use std::io::Read as _;

    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("demo.ofn"), WRITER_ONTO).unwrap();

    // 1. .omn.gz matches plain .omn byte-for-byte
    let o = sparkles(
        dir,
        &[
            "ontology",
            "reason",
            "-i",
            "demo.ofn",
            "-o",
            "out.omn",
            "-o",
            "out.omn.gz",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let plain_omn = std::fs::read_to_string(dir.join("out.omn")).unwrap();
    let mut gz_omn = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(dir.join("out.omn.gz")).unwrap())
        .read_to_string(&mut gz_omn)
        .unwrap();
    assert_eq!(gz_omn, plain_omn);

    // 2. Chained `reason ... classify --format owx|omn|obo|obographs`
    for fmt in ["owx", "omn", "obo", "obographs"] {
        let o = sparkles(
            dir,
            &[
                "ontology", "reason", "-i", "demo.ofn", "classify", "--format", fmt,
            ],
        );
        assert_eq!(o.status.code(), Some(0), "{fmt}: {}", err(&o));
        let body = out(&o);
        match fmt {
            "owx" => assert!(sparkles::owl::owx::parse(&body).is_ok(), "{body}"),
            "omn" => assert!(
                sparkles::owl::manchester::parse_document(&body).is_ok(),
                "{body}"
            ),
            "obo" => assert!(sparkles::owl::obo::parse(&body).is_ok(), "{body}"),
            "obographs" => assert!(sparkles::owl::obographs::parse(&body).is_ok(), "{body}"),
            _ => unreachable!(),
        }
    }

    // 3. Chained `classify --format bogus` fails fast before writing `-o`
    let o = sparkles(
        dir,
        &[
            "ontology",
            "reason",
            "-i",
            "demo.ofn",
            "classify",
            "--format",
            "bogus",
            "-o",
            "should_not_exist.owx",
        ],
    );
    assert_ne!(o.status.code(), Some(0));
    assert!(
        !dir.join("should_not_exist.owx").exists(),
        "invalid --format must fail before writing -o"
    );

    // 4. Atomic write: writer error (XML 1.0 illegal NUL in literal) must not truncate pre-existing file
    let bad_ofn = "Prefix(:=<http://ex.org/>)\nPrefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\nOntology(<http://ex.org/bad>\nDeclaration(Class(:A))\nAnnotationAssertion(rdfs:label :A \"bad\0nul\")\n)\n";
    std::fs::write(dir.join("bad.ofn"), bad_ofn).unwrap();
    let existing = dir.join("existing.owx");
    std::fs::write(&existing, "SENTINEL_KEEP_ME").unwrap();
    let o = sparkles(
        dir,
        &["ontology", "relax", "-i", "bad.ofn", "-o", "existing.owx"],
    );
    assert_ne!(o.status.code(), Some(0), "NUL in OWL/XML must fail");
    let owx_err = err(&o);
    assert!(
        owx_err.contains("OWL/XML") && owx_err.contains("U+0000"),
        "expected OWL/XML U+0000 serialization diagnostic, got: {owx_err}"
    );
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap(),
        "SENTINEL_KEEP_ME",
        "writer error must not truncate or corrupt existing destination file"
    );

    // 4b. Atomic success preserves permissions on existing regular file and updates symlink target
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let shared = dir.join("shared.owx");
        std::fs::write(&shared, "OLD").unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o644)).unwrap();
        let target = dir.join("target.owx");
        std::fs::write(&target, "STALE").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.join("link.owx");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let o = sparkles(
            dir,
            &[
                "ontology",
                "relax",
                "-i",
                "demo.ofn",
                "-o",
                "shared.owx",
                "-o",
                "link.owx",
            ],
        );
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert_eq!(
            std::fs::metadata(&shared).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert!(sparkles::owl::owx::parse(&std::fs::read_to_string(&target).unwrap()).is_ok());
    }

    // 5. Header-only loss logs stderr warning on lossy format
    let hdr_loss_ofn = "Prefix(:=<http://ex.org/>)\nPrefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\nOntology(<http://ex.org/hdr>\nAnnotation(rdfs:label \"bonjour\"@fr)\nDeclaration(Class(:A))\n)\n";
    std::fs::write(dir.join("hdr_loss.ofn"), hdr_loss_ofn).unwrap();
    let o = sparkles(
        dir,
        &[
            "ontology",
            "relax",
            "-i",
            "hdr_loss.ofn",
            "-o",
            "hdr_loss.json",
            "-o",
            "hdr_loss.obo",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let stderr = err(&o);
    assert!(
        stderr.contains("hdr_loss.json") && stderr.contains("header annotations"),
        "expected header-loss warning for .json: {stderr}"
    );
    assert!(
        stderr.contains("hdr_loss.obo") && stderr.contains("header annotations"),
        "expected header-loss warning for .obo: {stderr}"
    );
}

#[test]
fn ontology_writers_remote_server_stdout_formats() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("demo.ofn"), WRITER_ONTO).unwrap();

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut server = Kill(
        Command::new(BIN)
            .current_dir(dir)
            .args([
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--data",
                "data",
            ])
            .env_remove("SPARKLES_AUTH_CONFIG")
            .env_remove("SPARKLES_BACKUP_CONFIG")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let start = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(server.0.try_wait().unwrap().is_none(), "server exited");
        assert!(start.elapsed() < Duration::from_secs(30), "server timeout");
        std::thread::sleep(Duration::from_millis(20));
    }
    let url = format!("http://127.0.0.1:{port}");
    // Wait until /$/ping responds 200
    loop {
        if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            use std::io::{Read as _, Write as _};
            let _ = stream
                .write_all(b"GET /$/ping HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
            let mut resp = String::new();
            let _ = stream.read_to_string(&mut resp);
            if resp.starts_with("HTTP/1.1 200") {
                break;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "server not ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let o = sparkles(dir, &["dataset", "create", "onto", "--server", &url]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = sparkles(
        dir,
        &["ontology", "relax", "-i", "demo.ofn", "-o", "demo.ttl"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = sparkles(
        dir,
        &["load", "--server", &url, "--dataset", "onto", "demo.ttl"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));

    for cmd in ["axioms", "classify"] {
        for fmt in ["owx", "omn", "obo", "obographs"] {
            let o = sparkles(
                dir,
                &[
                    "ontology",
                    cmd,
                    "--server",
                    &url,
                    "--dataset",
                    "onto",
                    "--format",
                    fmt,
                ],
            );
            assert_eq!(o.status.code(), Some(0), "{cmd} {fmt}: {}", err(&o));
            let body = out(&o);
            match fmt {
                "owx" => assert!(sparkles::owl::owx::parse(&body).is_ok(), "{cmd}: {body}"),
                "omn" => assert!(
                    sparkles::owl::manchester::parse_document(&body).is_ok(),
                    "{cmd}: {body}"
                ),
                "obo" => assert!(sparkles::owl::obo::parse(&body).is_ok(), "{cmd}: {body}"),
                "obographs" => assert!(
                    sparkles::owl::obographs::parse(&body).is_ok(),
                    "{cmd}: {body}"
                ),
                _ => unreachable!(),
            }
        }
    }
}
