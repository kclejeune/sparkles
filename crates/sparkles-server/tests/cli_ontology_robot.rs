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
    // a chain from a database starts with reason, reduce, relax or materialize
    let o = sparkles(dir, &["ontology", "classify", "--loc", "db", "relax"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(
        err(&o).contains("starts with reason, reduce, relax or materialize"),
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
    // a graph the file does not have selects no axioms, as it does alone
    let o = sparkles(
        dir,
        &[
            "ontology",
            "relax",
            "-i",
            "n.ofn",
            "--graph",
            "http://ex.org/missing",
            "-o",
            "g.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let g = axioms(&dir.join("g.ofn"));
    assert!(!g.iter().any(|l| l.starts_with("SubClassOf")), "{g:?}");
}

/// GO's pattern: part of is transitive and the inverse of has part, which OWL 2 EL lacks.
const INVERSE: &str = r#"Prefix(:=<http://ex.org/>)
Ontology(<http://ex.org/go>
Declaration(Class(:FilopodiumMembrane))
Declaration(Class(:Membrane))
Declaration(Class(:Filopodium))
Declaration(Class(:Cytoskeleton))
Declaration(Class(:Organelle))
Declaration(Class(:MembraneBounded))
Declaration(Class(:OrganelleMembrane))
Declaration(ObjectProperty(:part_of))
Declaration(ObjectProperty(:has_part))
TransitiveObjectProperty(:part_of)
InverseObjectProperties(:part_of :has_part)
SubClassOf(:FilopodiumMembrane :Membrane)
SubClassOf(:FilopodiumMembrane ObjectSomeValuesFrom(:part_of :Filopodium))
SubClassOf(:Filopodium ObjectSomeValuesFrom(:part_of :Cytoskeleton))
SubClassOf(:Cytoskeleton :Organelle)
EquivalentClasses(:MembraneBounded ObjectIntersectionOf(:Organelle ObjectSomeValuesFrom(:has_part :Membrane)))
EquivalentClasses(:OrganelleMembrane ObjectIntersectionOf(:Membrane ObjectSomeValuesFrom(:part_of :MembraneBounded)))
)
"#;

#[test]
fn reasoner_elk_runs_the_el_rules_and_the_default_the_horn_rules() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("go.ofn"), INVERSE).unwrap();
    // the same ontology without the inverse, which the EL rules alone classify
    let el: String = INVERSE
        .lines()
        .filter(|l| !l.starts_with("InverseObjectProperties("))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(dir.join("el.ofn"), el).unwrap();
    let inferred = "SubClassOf(:FilopodiumMembrane :OrganelleMembrane)";
    for (cmd, out_name) in [("reason", "r"), ("reduce", "d")] {
        let elk = format!("{out_name}.elk.ofn");
        let o = sparkles(
            dir,
            &["ontology", cmd, "-i", "go.ofn", "-r", "ELK", "-o", &elk],
        );
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        let base = format!("{out_name}.el.ofn");
        let o = sparkles(dir, &["ontology", cmd, "-i", "el.ofn", "-o", &base]);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        let mut elk_axioms = axioms(&dir.join(&elk));
        elk_axioms.retain(|l| !l.starts_with("InverseObjectProperties("));
        assert_eq!(elk_axioms, axioms(&dir.join(&base)), "{cmd}");
        let def = format!("{out_name}.default.ofn");
        let o = sparkles(dir, &["ontology", cmd, "-i", "go.ofn", "-o", &def]);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        if cmd == "reason" {
            assert!(!axioms(&dir.join(&elk)).contains(inferred));
            assert!(axioms(&dir.join(&def)).contains(inferred), "{}", err(&o));
            let o = sparkles(
                dir,
                &[
                    "ontology", cmd, "-i", "go.ofn", "-r", "hermit", "-o", "h.ofn",
                ],
            );
            assert_eq!(o.status.code(), Some(0), "{}", err(&o));
            assert!(axioms(&dir.join("h.ofn")).contains(inferred));
        }
    }
}

#[test]
fn materialize_takes_elk_by_default_and_runs_the_el_rules() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("go.ofn"), INVERSE).unwrap();
    let el: String = INVERSE
        .lines()
        .filter(|l| !l.starts_with("InverseObjectProperties("))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(dir.join("el.ofn"), el).unwrap();
    let run = |args: &[&str]| {
        let mut a = vec!["ontology", "materialize"];
        a.extend_from_slice(args);
        let o = sparkles(dir, &a);
        assert_eq!(o.status.code(), Some(0), "{args:?}: {}", err(&o));
    };
    run(&["-i", "go.ofn", "-r", "ELK", "-o", "elk.ofn"]);
    run(&["-i", "go.ofn", "-o", "default.ofn"]);
    run(&["-i", "el.ofn", "-o", "el.out.ofn"]);
    run(&["-i", "go.ofn", "-r", "hermit", "-o", "h.ofn"]);
    // ELK, the default, gives the EL answer: that of the ontology without the inverse
    let mut elk = axioms(&dir.join("elk.ofn"));
    assert_eq!(elk, axioms(&dir.join("default.ofn")));
    elk.retain(|l| !l.starts_with("InverseObjectProperties("));
    assert_eq!(elk, axioms(&dir.join("el.out.ofn")));
    // a DL reasoner's name takes the Horn rules, which add the inverse's subsumption
    let inferred = "SubClassOf(:FilopodiumMembrane :OrganelleMembrane)";
    assert!(!elk.contains(inferred));
    assert!(axioms(&dir.join("h.ofn")).contains(inferred));
}

/// What `materialize --term partOf` adds to `o.ofn`: the direct superclasses of the two
/// defined classes, of which `∃partOf.Brain` is not direct for CerebellarNeuron through
/// the transitive partOf, and nothing for Cerebellum, whose existential is asserted, or
/// for Neuron, Brain and Obsolete, which have no class axiom.
const MATERIALIZED: [&str; 4] = [
    "SubClassOf(:CerebellarNeuron :HindbrainNeuron)",
    "SubClassOf(:CerebellarNeuron ObjectSomeValuesFrom(:partOf :Cerebellum))",
    "SubClassOf(:HindbrainNeuron :Neuron)",
    "SubClassOf(:HindbrainNeuron ObjectSomeValuesFrom(:partOf :Brain))",
];

#[test]
fn materialize_by_term_term_file_and_every_property() {
    let d = setup();
    let dir = d.path();
    let input = axioms(&dir.join("o.ofn"));
    let added = |file: &str| -> BTreeSet<String> {
        axioms(&dir.join(file))
            .difference(&input)
            .cloned()
            .collect()
    };
    let want: BTreeSet<String> = MATERIALIZED.iter().map(|s| s.to_string()).collect();
    let o = sparkles(
        dir,
        &[
            "ontology",
            "materialize",
            "--input",
            "o.ofn",
            "--term",
            "http://ex.org/partOf",
            "--output",
            "m.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(added("m.ofn"), want);
    std::fs::write(
        dir.join("terms.txt"),
        "# the properties\nhttp://ex.org/partOf # part of\n\n",
    )
    .unwrap();
    let o = sparkles(
        dir,
        &[
            "ontology",
            "materialize",
            "-i",
            "o.ofn",
            "-T",
            "terms.txt",
            "-o",
            "t.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(added("t.ofn"), want);
    // no term, or none that is an IRI, takes every object property, as ROBOT does
    let o = sparkles(
        dir,
        &["ontology", "materialize", "-i", "o.ofn", "-o", "a.ofn"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(added("a.ofn"), want);
    let o = sparkles(
        dir,
        &[
            "ontology",
            "materialize",
            "-i",
            "o.ofn",
            "-t",
            "ex:partOf",
            "-o",
            "b.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        err(&o).contains("ex:partOf is not a valid IRI."),
        "{}",
        err(&o)
    );
    assert!(err(&o).contains("every object property"), "{}", err(&o));
    assert_eq!(added("b.ofn"), want);
    // a full IRI that java.net.URL rejects, here for its port, is dropped as ROBOT drops it
    let o = sparkles(
        dir,
        &[
            "ontology",
            "materialize",
            "-i",
            "o.ofn",
            "-t",
            "http://ex.org:bad/partOf",
            "-o",
            "c.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        err(&o).contains("http://ex.org:bad/partOf is not a valid IRI."),
        "{}",
        err(&o)
    );
    assert_eq!(added("c.ofn"), want);
    // an OBO CURIE expands, and a property the ontology lacks adds the named superclasses
    let o = sparkles(
        dir,
        &[
            "ontology",
            "materialize",
            "-i",
            "o.ofn",
            "-t",
            "BFO:0000050",
            "-r",
            "HermiT",
            "-s",
            "false",
            "-a",
            "true",
            "-o",
            "c.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        err(&o).contains("--reasoner HermiT is ignored"),
        "{}",
        err(&o)
    );
    assert_eq!(
        added("c.ofn"),
        want.iter()
            .filter(|s| !s.contains("ObjectSomeValuesFrom"))
            .cloned()
            .collect()
    );
    // --create-new-ontology true saves the ontology without them, as ROBOT does
    let o = sparkles(
        dir,
        &[
            "ontology",
            "materialize",
            "-i",
            "o.ofn",
            "-n",
            "TRUE",
            "-o",
            "n.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(added("n.ofn").is_empty());
}

#[test]
fn materialize_in_a_chain_from_a_file_and_a_database() {
    let d = setup();
    let dir = d.path();
    let o = sparkles(
        dir,
        &[
            "ontology",
            "reason",
            "--input",
            "o.ofn",
            "materialize",
            "--term",
            "http://ex.org/partOf",
            "reduce",
            "--output",
            "chain.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    for (cmd, from, to) in [
        ("reason", "o.ofn", "1.ofn"),
        ("materialize", "1.ofn", "2.ofn"),
        ("reduce", "2.ofn", "3.ofn"),
    ] {
        let mut args = vec!["ontology", cmd, "--input", from, "--output", to];
        if cmd == "materialize" {
            args.extend(["--term", "http://ex.org/partOf"]);
        }
        let o = sparkles(dir, &args);
        assert_eq!(o.status.code(), Some(0), "{cmd}: {}", err(&o));
    }
    let chain = axioms(&dir.join("chain.ofn"));
    assert_eq!(chain, axioms(&dir.join("3.ofn")));
    assert!(
        chain.contains("SubClassOf(:CerebellarNeuron ObjectSomeValuesFrom(:partOf :Cerebellum))"),
        "{chain:?}"
    );
    // from a database
    let o = sparkles(dir, &["load", "--loc", "db", "o.ofn"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = sparkles(
        dir,
        &[
            "ontology",
            "materialize",
            "--loc",
            "db",
            "-t",
            "http://ex.org/partOf",
            "-o",
            "db.ofn",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let a = axioms(&dir.join("db.ofn"));
    assert!(
        a.iter().any(|l| l.starts_with("SubClassOf(")
            && l.contains("CerebellarNeuron")
            && l.contains("ObjectSomeValuesFrom(")
            && l.contains("Cerebellum))")),
        "{a:?}"
    );
}

#[test]
fn materialize_errors_and_exit_statuses() {
    let d = setup();
    let dir = d.path();
    let cases: &[(&[&str], &str)] = &[
        (
            &["materialize", "-i", "unsat.ofn", "-o", "y.ofn"],
            "unsatisfiable: http://ex.org/A",
        ),
        (
            &["materialize", "-i", "o.ofn", "-r", "emr"],
            "INVALID REASONER ERROR",
        ),
        (
            &["materialize", "-i", "o.ofn", "-r", "nonsense"],
            "INVALID REASONER ERROR",
        ),
        (
            &["materialize", "-i", "o.ofn", "-T", "missing.txt"],
            "cannot read the term file",
        ),
        (
            &["materialize", "-i", "abox.ofn", "-o", "y.ofn"],
            "INCONSISTENT ONTOLOGY ERROR",
        ),
    ];
    for (args, message) in cases {
        let mut a = vec!["ontology"];
        a.extend_from_slice(args);
        let o = sparkles(dir, &a);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {}", err(&o));
        assert!(err(&o).contains(message), "{args:?}: {}", err(&o));
    }
    assert!(!dir.join("y.ofn").exists());
}
