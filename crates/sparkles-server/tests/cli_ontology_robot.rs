//! `sparkles ontology reason`, `reduce` and `relax` run as the real binary, alone and in
//! chains: the outputs, the chain against the same commands run through files, ROBOT's
//! errors and the exit statuses.

#![cfg(feature = "ontology")]

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const ONTO: &str = r#"Prefix(:=<http://ex.org/>)
Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)
Ontology(<http://ex.org/o>
Declaration(Class(:Neuron))
Declaration(Class(:CerebellarNeuron))
Declaration(Class(:HindbrainNeuron))
Declaration(Class(:Cerebellum))
Declaration(Class(:Brain))
Declaration(Class(:Obsolete))
Declaration(ObjectProperty(:partOf))
AnnotationAssertion(rdfs:label :Neuron "neuron")
EquivalentClasses(:CerebellarNeuron ObjectIntersectionOf(:Neuron ObjectSomeValuesFrom(:partOf :Cerebellum)))
EquivalentClasses(:HindbrainNeuron ObjectIntersectionOf(:Neuron ObjectSomeValuesFrom(:partOf :Brain)))
SubClassOf(:Cerebellum ObjectSomeValuesFrom(:partOf :Brain))
TransitiveObjectProperty(:partOf)
)
"#;

const UNSAT: &str = r#"Prefix(:=<http://ex.org/>)
Ontology(
Declaration(Class(:A))
Declaration(Class(:B))
Declaration(Class(:C))
Declaration(Class(:X))
SubClassOf(:A :B)
SubClassOf(:A :C)
DisjointClasses(:B :C)
SubClassOf(:X :B)
)
"#;

fn sparkles(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env_remove("SPARKLES_SERVER")
        .output()
        .unwrap()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The axiom lines of a functional-style document.
fn axioms(path: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("Prefix(") && !l.starts_with("Ontology(") && l.contains('('))
        .map(str::to_string)
        .collect()
}

const ABOX: &str = r#"Prefix(:=<http://ex.org/>)
Ontology(
Declaration(Class(:A))
Declaration(Class(:C))
Declaration(NamedIndividual(:a))
DisjointClasses(:A :C)
ClassAssertion(:A :a)
ClassAssertion(:C :a)
)
"#;

fn setup() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("abox.ofn"), ABOX).unwrap();
    std::fs::write(d.path().join("o.ofn"), ONTO).unwrap();
    std::fs::write(d.path().join("unsat.ofn"), UNSAT).unwrap();
    d
}

#[test]
fn reason_writes_the_ontology_with_the_inferred_axioms() {
    let d = setup();
    let dir = d.path();
    let o = sparkles(
        dir,
        &[
            "ontology", "reason", "--input", "o.ofn", "--output", "r.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let a = axioms(&dir.join("r.ofn"));
    assert!(
        a.contains("SubClassOf(:CerebellarNeuron :HindbrainNeuron)"),
        "{a:?}"
    );
    assert!(a.contains("SubClassOf(:Obsolete owl:Thing)"), "{a:?}");
    assert!(
        a.contains("AnnotationAssertion(rdfs:label :Neuron \"neuron\")"),
        "{a:?}"
    );
    // the options of ROBOT, by its short names too
    let o = sparkles(
        dir,
        &[
            "ontology", "reason", "-i", "o.ofn", "-x", "true", "-a", "true", "-o", "x.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let a = axioms(&dir.join("x.ofn"));
    assert!(!a.iter().any(|l| l.contains("owl:Thing)")), "{a:?}");
    assert!(
        a.iter().any(|l| l.contains("oboInOwl:is_inferred")
            || l.contains("<http://www.geneontology.org/formats/oboInOwl#is_inferred>")),
        "{a:?}"
    );
}

#[test]
fn a_chain_equals_its_commands_through_files() {
    let d = setup();
    let dir = d.path();
    let o = sparkles(
        dir,
        &[
            "ontology",
            "reason",
            "--input",
            "o.ofn",
            "relax",
            "reduce",
            "--output",
            "chain.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    for (cmd, from, to) in [
        ("reason", "o.ofn", "1.ofn"),
        ("relax", "1.ofn", "2.ofn"),
        ("reduce", "2.ofn", "3.ofn"),
    ] {
        let o = sparkles(dir, &["ontology", cmd, "--input", from, "--output", to]);
        assert_eq!(o.status.code(), Some(0), "{cmd}: {}", err(&o));
    }
    let chain = axioms(&dir.join("chain.ofn"));
    assert_eq!(chain, axioms(&dir.join("3.ofn")));
    assert!(
        chain.contains("SubClassOf(:CerebellarNeuron ObjectSomeValuesFrom(:partOf :Cerebellum))")
    );
    assert!(!chain.contains("SubClassOf(:CerebellarNeuron :Neuron)"));
    // an --output on each command writes the ontology after it
    let o = sparkles(
        dir,
        &[
            "ontology", "reason", "-i", "o.ofn", "-o", "a.ofn", "relax", "-o", "b.ttl", "reduce",
            "-o", "c.ofn.gz",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(axioms(&dir.join("a.ofn")), axioms(&dir.join("1.ofn")));
    assert!(dir.join("b.ttl").exists() && dir.join("c.ofn.gz").exists());
    // the Turtle output reads back
    let o = sparkles(dir, &["ontology", "classify", "b.ttl", "--format", "text"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).starts_with("consistent"), "{}", out(&o));
}

#[test]
fn classify_and_check_in_a_chain() {
    let d = setup();
    let dir = d.path();
    let o = sparkles(
        dir,
        &[
            "ontology",
            "reason",
            "--input",
            "o.ofn",
            "check",
            "--profile",
            "el",
            "classify",
            "--format",
            "text",
            "relax",
            "--output",
            "y.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("OWL 2 EL: conforms"), "{s}");
    assert!(s.contains("consistent: "), "{s}");
    assert!(dir.join("y.ofn").exists());
    // a chain can start from classify FILE, and the -o after relax is relax's
    let o = sparkles(
        dir,
        &[
            "ontology", "classify", "o.ofn", "--format", "text", "relax", "-o", "z.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).starts_with("consistent"), "{}", out(&o));
    let z = axioms(&dir.join("z.ofn"));
    assert!(
        z.contains("SubClassOf(:CerebellarNeuron ObjectSomeValuesFrom(:partOf :Cerebellum))"),
        "{z:?}"
    );
    // and from check FILE, whose diagnostics are those of check alone
    let o = sparkles(
        dir,
        &[
            "ontology",
            "check",
            "o.ofn",
            "--profile",
            "el",
            "-o",
            "c.ofn",
            "relax",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let alone = sparkles(dir, &["ontology", "check", "o.ofn", "--profile", "el"]);
    assert_eq!(out(&o), out(&alone));
    assert_eq!(axioms(&dir.join("c.ofn")), axioms(&dir.join("o.ofn")));
    // a chain from a database starts with reason, reduce or relax
    let o = sparkles(dir, &["ontology", "classify", "--loc", "db", "relax"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(
        err(&o).contains("starts with reason, reduce or relax"),
        "{}",
        err(&o)
    );
    // a classify later in a chain classifies the assertions only with --assertions, as
    // classify alone does: the assertions of abox.ofn are inconsistent
    let first = |args: &[&str]| {
        let mut a = vec!["ontology"];
        a.extend_from_slice(args);
        let o = sparkles(dir, &a);
        let s = out(&o);
        (o.status.code(), s.lines().next().unwrap_or("").to_string())
    };
    let without = first(&["relax", "-i", "abox.ofn", "classify", "--format", "text"]);
    let with = first(&[
        "relax",
        "-i",
        "abox.ofn",
        "classify",
        "--assertions",
        "--format",
        "text",
    ]);
    assert_eq!(
        without,
        first(&["classify", "abox.ofn", "--format", "text"])
    );
    assert_eq!(
        with,
        first(&["classify", "abox.ofn", "--assertions", "--format", "text"])
    );
    assert_eq!(without.0, Some(0), "{without:?}");
    assert_eq!(with.0, Some(1), "{with:?}");
    assert!(with.1.starts_with("inconsistent"), "{with:?}");
    // a failing classification ends the chain with status 1
    let o = sparkles(
        dir,
        &[
            "ontology",
            "relax",
            "--input",
            "unsat.ofn",
            "classify",
            "relax",
            "-o",
            "n.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(!dir.join("n.ofn").exists());
}

#[test]
fn robot_errors_and_exit_statuses() {
    let d = setup();
    let dir = d.path();
    let o = sparkles(
        dir,
        &[
            "ontology", "reason", "--input", "o.ofn", "relax", "--input", "o.ofn", "--output",
            "y.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("CHAINED INPUT ERROR"), "{}", err(&o));
    let o = sparkles(
        dir,
        &[
            "ontology",
            "reason",
            "--input",
            "unsat.ofn",
            "-D",
            "dump.ofn",
            "-o",
            "y.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("unsatisfiable: http://ex.org/A"),
        "{}",
        err(&o)
    );
    let dump = axioms(&dir.join("dump.ofn"));
    assert!(dump.contains("DisjointClasses(:B :C)"), "{dump:?}");
    assert!(!dump.contains("SubClassOf(:X :B)"), "{dump:?}");
    assert!(!dir.join("y.ofn").exists());
    // reduce and relax do not refuse an incoherent ontology
    let o = sparkles(
        dir,
        &["ontology", "reduce", "-i", "unsat.ofn", "-o", "y.ofn"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let cases: &[(&[&str], &str)] = &[
        (
            &["relax", "-i", "o.ofn", "--enforce-obo-format", "yes"],
            "BOOLEAN VALUE ERROR",
        ),
        (
            &["reason", "-i", "o.ofn", "-r", "nonsense"],
            "INVALID REASONER ERROR",
        ),
        (
            &["reduce", "-i", "o.ofn", "-r", "emr"],
            "INVALID REASONER ERROR",
        ),
        (
            &["reason", "-i", "o.ofn", "-A", "subclass bogus"],
            "UNKNOWN AXIOM GENERATOR",
        ),
        (
            &["reason", "-i", "o.ofn", "-e", "some"],
            "EQUIVALENT CLASSES OPTION ERROR",
        ),
        (
            &["reason", "-i", "o.ofn", "-n", "true", "-m", "true"],
            "CREATE ONTOLOGY ERROR",
        ),
        (
            &["reason", "-i", "o.ofn", "-o", "y.owx"],
            "no writer for .owx",
        ),
        (
            &["reason", "-i", "o.ofn", "-o", "y.xyz"],
            "INVALID FORMAT ERROR",
        ),
        (&["reason", "-o", "y.ofn"], "--input FILE"),
    ];
    for (args, message) in cases {
        let mut a = vec!["ontology"];
        a.extend_from_slice(args);
        let o = sparkles(dir, &a);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {}", err(&o));
        assert!(err(&o).contains(message), "{args:?}: {}", err(&o));
    }
    let o = sparkles(
        dir,
        &[
            "ontology", "reason", "-i", "o.ofn", "-r", "HermiT", "-o", "h.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        err(&o).contains("--reasoner HermiT is ignored"),
        "{}",
        err(&o)
    );
}

#[test]
fn reason_from_rdf_xml_and_a_database() {
    let d = setup();
    let dir = d.path();
    let o = sparkles(dir, &["ontology", "reason", "-i", "o.ofn", "-o", "r.owl"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = sparkles(dir, &["ontology", "reduce", "-i", "r.owl", "-o", "r2.ofn"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    // RDF/XML keeps no empty prefix, so the IRIs are written in full
    assert!(
        axioms(&dir.join("r2.ofn")).contains(
            "SubClassOf(<http://ex.org/CerebellarNeuron> <http://ex.org/HindbrainNeuron>)"
        )
    );
    let o = sparkles(dir, &["load", "--loc", "db", "r.owl"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = sparkles(dir, &["ontology", "relax", "--loc", "db", "-o", "r3.ofn"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        axioms(&dir.join("r3.ofn"))
            .contains("SubClassOf(<http://ex.org/CerebellarNeuron> <http://ex.org/Neuron>)")
    );
}

#[test]
fn functional_style_files_keep_n_ary_and_annotated_axioms() {
    let d = setup();
    let dir = d.path();
    std::fs::write(
        dir.join("n.ofn"),
        r#"Prefix(:=<http://ex.org/>)
Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)
Ontology(
EquivalentClasses(:A :B :C)
SubClassOf(:A :D)
SubClassOf(Annotation(rdfs:comment "x") :A :D)
)
"#,
    )
    .unwrap();
    // through a gzipped functional-style file and back
    let o = sparkles(dir, &["ontology", "relax", "-i", "n.ofn", "-o", "n.ofn.gz"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = sparkles(
        dir,
        &["ontology", "reduce", "-i", "n.ofn.gz", "-o", "m.ofn"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let a = axioms(&dir.join("m.ofn"));
    for ax in [
        "EquivalentClasses(:A :B :C)",
        "SubClassOf(:A :D)",
        "SubClassOf(Annotation(rdfs:comment \"x\") :A :D)",
    ] {
        assert!(a.contains(ax), "{ax}: {a:?}");
    }
}
