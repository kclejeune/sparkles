//! The wiring of the datatype checker into the EL and Horn tiers (F24, "The EL and Horn
//! tiers"): which data restrictions become the checker's atoms, what the saturation
//! derives from them, and what a run reports.

use super::*;

fn axioms(body: &str) -> Vec<AxiomKind> {
    crate::ofn_read::parse(&format!(
        "Prefix(:=<urn:dt:>)\nPrefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n\
         Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\nPrefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\nOntology(\n{body}\n)"
    ))
    .expect("test ontology")
    .axioms
    .into_iter()
    .map(|a| a.kind)
    .collect()
}

fn run(body: &str) -> (Saturation, Vec<AxiomKind>) {
    run_on(body, 1)
}

fn run_on(body: &str, threads: usize) -> (Saturation, Vec<AxiomKind>) {
    let (sat, ax, _) = run_consistent(body, threads);
    (sat, ax)
}

fn run_consistent(body: &str, threads: usize) -> (Saturation, Vec<AxiomKind>, bool) {
    let ax = axioms(body);
    let mut sat = Saturation::new();
    let all: Vec<&AxiomKind> = ax.iter().collect();
    let out = sat
        .saturate(
            &all,
            None,
            &Limits {
                threads,
                ..Limits::default()
            },
        )
        .unwrap();
    (sat, ax, out.consistent)
}

/// Whether the last run says `a ⊑ b` for two named classes.
fn sub(sat: &Saturation, a: &str, b: &str) -> bool {
    let d = sat
        .describe(&Class::new(format!("urn:dt:{a}")))
        .expect("a saturated run");
    !d.satisfiable
        || d.superclasses
            .iter()
            .any(|c| c.as_str() == format!("urn:dt:{b}"))
}

fn gap_of<'a>(sat: &'a Saturation, k: &AxiomKind) -> Option<&'a Gap> {
    sat.excluded()
        .iter()
        .find(|e| &e.axiom == k)
        .map(|e| &e.gap)
}

#[test]
fn el_data_builds_no_atoms() {
    let (sat, _) = run("SubClassOf(:A DataSomeValuesFrom(:d xsd:integer))\n\
         SubClassOf(DataSomeValuesFrom(:d xsd:decimal) :B)\n\
         FunctionalDataProperty(:d)");
    assert!(sat.interner.dt.is_empty());
    assert!(sat.interner.dt_run.is_none());
    assert!(sat.excluded().is_empty());
}

#[test]
fn a_checker_component_takes_its_in_profile_axioms() {
    let (sat, _) = run("SubClassOf(:A DataSomeValuesFrom(:d xsd:integer))\n\
         SubClassOf(:B DataAllValuesFrom(:d xsd:string))\n\
         DataPropertyAssertion(:d :a \"1\"^^xsd:integer)\n\
         NegativeDataPropertyAssertion(:d :a \"2\"^^xsd:integer)\n\
         DataPropertyDomain(:d :C)\n\
         DataPropertyRange(:d xsd:integer)\n\
         FunctionalDataProperty(:d)\n\
         SubClassOf(:A DataSomeValuesFrom(:e xsd:integer))");
    // the run's components are dropped after it
    assert!(sat.interner.dt_run.is_none());
    assert!(!sat.interner.dt.is_empty());
    // the saturation reads every atom, so nothing is left out
    assert!(sat.excluded().is_empty(), "{:?}", sat.excluded());
    assert!(sat.interner.dt.atoms.iter().all(|a| {
        matches!(&sat.interner.roles[a.role as usize], index::RoleName::Data(d) if d.as_str() == "urn:dt:d")
    }));
}

#[test]
fn a_zero_minimum_and_an_empty_range_fold() {
    let (sat, _) = run("SubClassOf(:A DataMinCardinality(0 :d xsd:integer))\n\
         SubClassOf(:B DataMaxCardinality(3 :d DataComplementOf(rdfs:Literal)))\n\
         SubClassOf(:C DataSomeValuesFrom(:d xsd:int))");
    // `≥ 0` and `≤ 3` of the empty range always hold, so only `≥ 1 d.int` is an atom
    assert_eq!(sat.interner.dt.atoms.len(), 1);
}

#[test]
fn restrictions_over_the_top_data_property_are_left_out_beside_the_checker() {
    let (sat, ax) = run(
        "SubClassOf(:A DataSomeValuesFrom(owl:topDataProperty xsd:integer))\n\
         SubClassOf(:B DataAllValuesFrom(:d xsd:int))",
    );
    assert_eq!(gap_of(&sat, &ax[0]), Some(&Gap::Partial(index::DT_TOP)));
    // without a checker component it stays with the lattice
    let (sat, ax) = run("SubClassOf(:A DataSomeValuesFrom(owl:topDataProperty xsd:integer))");
    assert!(gap_of(&sat, &ax[0]).is_none());
}

#[test]
fn a_range_the_checker_refuses_leaves_its_axiom_out() {
    let (sat, ax) = run(
        "SubClassOf(:A DataSomeValuesFrom(:d DatatypeRestriction(xsd:string xsd:pattern \"a*\")))\n\
         SubClassOf(:B DataAllValuesFrom(:d xsd:int))",
    );
    assert!(gap_of(&sat, &ax[0]).is_some());
}

#[test]
fn a_faceted_definition_outside_the_profile_reaches_the_checker() {
    let (sat, _) = run(
        "DatatypeDefinition(:small DatatypeRestriction(xsd:integer xsd:maxInclusive \"5\"^^xsd:integer))\n\
         SubClassOf(:A DataSomeValuesFrom(:d :small))\n\
         SubClassOf(:B DataSomeValuesFrom(:d xsd:integer))",
    );
    // the definition makes `d` a checker property, so the in-profile axiom is an atom
    assert!(!sat.interner.dt.is_empty());
}

#[test]
fn a_parallel_run_reports_what_one_thread_reports() {
    // role axioms over a checker property intern no data value, so that only the
    // checker components keep the run from its chunks
    let mut body = String::from(
        "FunctionalDataProperty(:d)\n\
         SubDataPropertyOf(:d owl:bottomDataProperty)\n\
         HasKey(:K () (:d))\n\
         SubClassOf(:B DataAllValuesFrom(:d xsd:int))\n",
    );
    for i in 0..4 * PARALLEL_AXIOMS {
        body.push_str(&format!("SubClassOf(:C{i} :D)\n"));
    }
    let (one, _) = run_on(&body, 1);
    let (many, _) = run_on(&body, 4);
    assert_eq!(one.excluded(), many.excluded());
    // the bottom edge and the key; the functional property and the universal are atoms
    assert_eq!(one.excluded().len(), 2);
}

#[test]
fn a_key_over_a_checker_property_is_used_in_part() {
    let (sat, ax) = run("HasKey(:K () (:d))\n\
         SubClassOf(:B DataAllValuesFrom(:d xsd:int))\n\
         HasKey(:K () (:e))");
    assert_eq!(gap_of(&sat, &ax[0]), Some(&Gap::Partial(index::DT_KEY)));
    assert!(gap_of(&sat, &ax[2]).is_none());
}

const BOOL: &str = "\"true\"^^xsd:boolean";
const NOT_BOOL: &str = "\"false\"^^xsd:boolean";

#[test]
fn both_values_of_a_boolean_give_their_class() {
    let (sat, _) = run(&format!(
        "SubClassOf(DataHasValue(:d {BOOL}) :C)\n\
         SubClassOf(DataHasValue(:d {NOT_BOOL}) :C)\n\
         SubClassOf(:A DataSomeValuesFrom(:d xsd:boolean))"
    ));
    assert!(sub(&sat, "A", "C"));
    assert!(sat.excluded().is_empty(), "{:?}", sat.excluded());
}

#[test]
fn a_cover_over_two_classes_is_used_in_part() {
    let (sat, ax) = run(&format!(
        "SubClassOf(DataHasValue(:d {BOOL}) :C)\n\
         SubClassOf(DataHasValue(:d {NOT_BOOL}) :D)\n\
         SubClassOf(:A DataSomeValuesFrom(:d xsd:boolean))\n\
         SubClassOf(:A :E)"
    ));
    assert!(!sub(&sat, "A", "C") && !sub(&sat, "A", "D"));
    for k in &ax[..3] {
        assert!(
            matches!(gap_of(&sat, k), Some(Gap::Partial(_))),
            "{k:?}: {:?}",
            sat.excluded()
        );
    }
    assert!(gap_of(&sat, &ax[3]).is_none());
}

#[test]
fn an_entailed_atom_gives_its_told_subsumer() {
    // `≥ 1 d.int[≥ 5]` entails `≥ 1 d.int[≥ 1]`, and the domain gives E
    let (sat, _) = run(
        "SubClassOf(:A DataSomeValuesFrom(:d DatatypeRestriction(xsd:int xsd:minInclusive \"5\"^^xsd:int)))\n\
         SubClassOf(DataSomeValuesFrom(:d DatatypeRestriction(xsd:int xsd:minInclusive \"1\"^^xsd:int)) :C)\n\
         SubClassOf(DataSomeValuesFrom(:d DatatypeRestriction(xsd:int xsd:maxInclusive \"1\"^^xsd:int)) :D)\n\
         DataPropertyDomain(:d :E)",
    );
    assert!(sub(&sat, "A", "C"));
    assert!(sub(&sat, "A", "E"));
    assert!(!sub(&sat, "A", "D"));
    assert!(sat.excluded().is_empty(), "{:?}", sat.excluded());
}

#[test]
fn a_conflict_makes_the_class_unsatisfiable() {
    let (sat, _) = run("SubClassOf(:A DataSomeValuesFrom(:d xsd:int))\n\
         SubClassOf(:A DataAllValuesFrom(:d xsd:string))\n\
         SubClassOf(:B DataMinCardinality(2 :d xsd:boolean))\n\
         FunctionalDataProperty(:d)");
    for c in ["A", "B"] {
        let d = sat.describe(&Class::new(format!("urn:dt:{c}"))).unwrap();
        assert!(!d.satisfiable, "{c}");
    }
    assert!(sat.excluded().is_empty(), "{:?}", sat.excluded());
}

#[test]
fn literals_with_individuals_decide_consistency() {
    // 972: a value outside a DL datatype's value space, a negative assertion that
    // contradicts a positive one, and two values of a functional property
    for (body, consistent) in [
        (
            "DataPropertyRange(:d xsd:int)\nDataPropertyAssertion(:d :a \"3000000000\"^^xsd:integer)",
            false,
        ),
        (
            "DataPropertyRange(:d xsd:int)\nDataPropertyAssertion(:d :a \"30\"^^xsd:integer)",
            true,
        ),
        (
            "DataPropertyRange(:d xsd:int)\nDataPropertyAssertion(:d :a \"3\"^^xsd:int)\n\
             NegativeDataPropertyAssertion(:d :a \"3\"^^xsd:integer)",
            false,
        ),
        (
            "FunctionalDataProperty(:d)\nDataPropertyRange(:d xsd:int)\n\
             DataPropertyAssertion(:d :a \"3\"^^xsd:int)\nDataPropertyAssertion(:d :a \"4\"^^xsd:int)",
            false,
        ),
        (
            "FunctionalDataProperty(:d)\nDataPropertyRange(:d xsd:int)\n\
             DataPropertyAssertion(:d :a \"3\"^^xsd:int)\nDataPropertyAssertion(:d :a \"03\"^^xsd:int)",
            true,
        ),
    ] {
        let (sat, _, c) = run_consistent(body, 1);
        assert_eq!(c, consistent, "{body}");
        assert!(sat.excluded().is_empty(), "{body}: {:?}", sat.excluded());
    }
}

#[test]
fn a_told_subsumer_held_before_or_after_the_atom_frees_it() {
    // `∃d.{true}` gives C and D, `∃d.{false}` gives C. With D held, `∃d.{true}` gives
    // only C more, so both values give C, whichever of D and the atom comes first.
    for d in [
        "SubClassOf(:A :D)",
        "SubClassOf(:A :B)\nSubClassOf(:B :B2)\nSubClassOf(:B2 :D)",
    ] {
        let (sat, _) = run(&format!(
            "SubClassOf(DataHasValue(:d {BOOL}) :C)\n\
             SubClassOf(DataHasValue(:d {BOOL}) :D)\n\
             SubClassOf(DataHasValue(:d {NOT_BOOL}) :C)\n\
             SubClassOf(:A DataSomeValuesFrom(:d xsd:boolean))\n{d}"
        ));
        assert!(sub(&sat, "A", "C"), "{d}");
        assert!(sat.excluded().is_empty(), "{d}: {:?}", sat.excluded());
    }
}

#[test]
fn a_cancellation_polled_once_stops_a_checker_run_wherever_it_comes() {
    let mut body = String::from(
        "SubClassOf(DataSomeValuesFrom(:d DatatypeRestriction(xsd:int xsd:minInclusive \"100\"^^xsd:int)) :G)\n",
    );
    for i in 0..100 {
        body.push_str(&format!(
            "SubClassOf(:C{i} DataSomeValuesFrom(:d DatatypeRestriction(xsd:int xsd:minInclusive \"{i}\"^^xsd:int)))\n"
        ));
    }
    let ax = axioms(&body);
    let all: Vec<&AxiomKind> = ax.iter().collect();
    let counting = |stop_at: usize| {
        let n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let m = n.clone();
        let limits = Limits {
            threads: 1,
            cancelled: Some(std::sync::Arc::new(move || {
                m.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1 == stop_at
            })),
            ..Limits::default()
        };
        (n, limits)
    };
    let (n, limits) = counting(0);
    let mut sat = Saturation::new();
    sat.saturate(&all, None, &limits).unwrap();
    assert!(sat.excluded().is_empty(), "{:?}", sat.excluded());
    let polls = n.load(std::sync::atomic::Ordering::Relaxed);
    assert!(polls > 2);
    for k in 1..=polls {
        let (_, limits) = counting(k);
        let r = Saturation::new().saturate(&all, None, &limits);
        assert!(
            matches!(r, Err(Error::Cancelled)),
            "poll {k} of {polls}: {:?}",
            r.err()
        );
    }
}

#[test]
fn a_disjointness_of_checker_properties_is_taken() {
    // `DisjointDataProperties` makes its properties one checker component, and the rule
    // reads its pairs: `A` has the value 1 by both. The second body adds a disjunctive
    // axiom, which Tier D does not take beside data, so the Horn rules run.
    for extra in ["", "SubClassOf(:X ObjectUnionOf(:Y :Z))"] {
        let (sat, ax) = run(&format!(
            "DisjointDataProperties(:d :e)\n\
             SubClassOf(:A DataHasValue(:d \"1\"^^xsd:integer))\n\
             SubClassOf(:A DataHasValue(:e \"1\"^^xsd:integer))\n\
             SubClassOf(:B DataHasValue(:d \"1\"^^xsd:integer))\n\
             SubClassOf(:B DataHasValue(:e \"2\"^^xsd:integer))\n{extra}"
        ));
        assert!(!sat.interner.dt.is_empty(), "{extra}");
        assert!(sub(&sat, "A", "Nothing"), "{extra}");
        assert!(!sub(&sat, "B", "Nothing"), "{extra}");
        assert!(
            gap_of(&sat, &ax[0]).is_none(),
            "{extra}: {:?}",
            sat.excluded()
        );
    }
}

#[test]
fn a_disjointness_reaches_the_subproperties() {
    let (sat, _) = run("DisjointDataProperties(:d :e)\n\
         SubDataPropertyOf(:d2 :d)\n\
         SubDataPropertyOf(:e2 :e)\n\
         SubClassOf(:A DataHasValue(:d2 \"1\"^^xsd:integer))\n\
         SubClassOf(:A DataHasValue(:e2 \"1\"^^xsd:integer))");
    assert!(sub(&sat, "A", "Nothing"));
    assert!(sat.excluded().is_empty(), "{:?}", sat.excluded());
}

#[test]
fn a_disjointness_with_assertions_decides_consistency() {
    for (v, consistent) in [("1", false), ("2", true)] {
        let (_, _, c) = run_consistent(
            &format!(
                "DisjointDataProperties(:d :e)\n\
                 DataPropertyAssertion(:d :a \"1\"^^xsd:integer)\n\
                 DataPropertyAssertion(:e :a \"{v}\"^^xsd:integer)"
            ),
            1,
        );
        assert_eq!(c, consistent, "{v}");
    }
}

#[test]
fn a_disjointness_that_no_tier_takes_is_reported() {
    let body = "DisjointDataProperties(:d :e)\n\
         SubClassOf(:A DataHasValue(:d \"1\"^^xsd:integer))\n\
         SubClassOf(:A DataHasValue(:e \"1\"^^xsd:integer))";
    let ax = axioms(body);
    let all: Vec<&AxiomKind> = ax.iter().collect();
    let mut sat = Saturation::new();
    sat.saturate(
        &all,
        None,
        &Limits {
            el_only: true,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(gap_of(&sat, &ax[0]).is_some(), "{:?}", sat.excluded());
    // with the top data property the axiom has no component, and stays out
    let (sat, ax) = run("DisjointDataProperties(:d owl:topDataProperty)\n\
         SubClassOf(:A DataHasValue(:d \"1\"^^xsd:integer))\n\
         SubClassOf(:B DataAllValuesFrom(:d xsd:int))");
    assert!(gap_of(&sat, &ax[0]).is_some(), "{:?}", sat.excluded());
    // beside literals of datatypes outside OWL 2 EL and a disjunctive axiom, which Tier
    // D can take (row 1303), the axiom is used or reported, never dropped
    let (sat, ax) = run("DisjointDataProperties(:d :e)\n\
         SubClassOf(:A DataHasValue(:d \"1\"^^xsd:int))\n\
         SubClassOf(:A DataHasValue(:e \"1\"^^xsd:int))\n\
         SubClassOf(:X ObjectUnionOf(:Y :Z))");
    assert!(
        sub(&sat, "A", "Nothing") || gap_of(&sat, &ax[0]).is_some(),
        "{:?}",
        sat.excluded()
    );
}

#[test]
fn a_definition_the_checker_cannot_convert_is_reported() {
    for def in [
        "DatatypeDefinition(:t DatatypeRestriction(xsd:string xsd:pattern \"a*\"))",
        "DatatypeDefinition(:t DataUnionOf(:t xsd:int))",
    ] {
        let (sat, ax) = run(&format!(
            "{def}\nSubClassOf(:A DataSomeValuesFrom(:d :t))\n\
             SubClassOf(:B DataAllValuesFrom(:d xsd:int))"
        ));
        assert!(
            gap_of(&sat, &ax[0]).is_some(),
            "{def}: {:?}",
            sat.excluded()
        );
    }
    // of two definitions of one datatype the atoms read one, and the other is reported;
    // a definition of a reserved name is reported too
    for defs in [
        "DatatypeDefinition(:t xsd:int)\nDatatypeDefinition(:t DataUnionOf(xsd:boolean xsd:int))",
        "DatatypeDefinition(xsd:integer DataUnionOf(xsd:boolean xsd:int))",
    ] {
        let (sat, ax) = run(&format!(
            "{defs}\nSubClassOf(:A DataSomeValuesFrom(:d :t))\n\
             SubClassOf(:B DataAllValuesFrom(:d xsd:int))"
        ));
        let defs: Vec<&AxiomKind> = ax
            .iter()
            .filter(|k| matches!(k, AxiomKind::DatatypeDefinition(..)))
            .collect();
        assert!(
            defs.iter().any(|k| gap_of(&sat, k).is_some()),
            "{defs:?}: {:?}",
            sat.excluded()
        );
    }
    // an unused definition beside the checker is taken: it says nothing of a class
    let (sat, ax) = run("DatatypeDefinition(:t DataUnionOf(xsd:boolean xsd:int))\n\
         SubClassOf(:B DataAllValuesFrom(:d xsd:int))");
    assert!(gap_of(&sat, &ax[0]).is_none(), "{:?}", sat.excluded());
}

#[test]
fn an_added_definition_that_makes_a_checker_component_starts_again() {
    // `:small` is unknown at first, so its axiom is left out and the run has no checker
    // component. The added definition gives it a facet, which makes `d` the checker's.
    let first = axioms(
        "SubClassOf(:A DataSomeValuesFrom(:d :small))\n\
         SubClassOf(DataSomeValuesFrom(:d xsd:integer) :B)\n\
         SubClassOf(:C :D)",
    );
    let def = axioms(
        "DatatypeDefinition(:small DatatypeRestriction(xsd:integer xsd:maxInclusive \"5\"^^xsd:integer))",
    );
    assert!(def.iter().all(needs_full_run));
    let before: Vec<&AxiomKind> = first.iter().collect();
    let added: Vec<&AxiomKind> = def.iter().collect();
    let all: Vec<&AxiomKind> = before.iter().chain(&added).copied().collect();
    let mut sat = Saturation::new();
    sat.classify(&before, None, &Limits::default()).unwrap();
    assert!(sat.interner.dt.is_empty());
    assert!(gap_of(&sat, &first[0]).is_some());
    let out = sat
        .classify(
            &all,
            Some(Changes {
                added: &added,
                removed: &[],
            }),
            &Limits::default(),
        )
        .unwrap();
    assert!(!out.stats.incremental);
    assert!(!sat.interner.dt.is_empty());
    let mut fresh = Saturation::new();
    let want = fresh.classify(&all, None, &Limits::default()).unwrap();
    assert_eq!(out.taxonomy, want.taxonomy);
    assert_eq!(sat.excluded(), fresh.excluded());
    assert!(sub(&sat, "A", "B"));
}

#[test]
fn a_cover_before_a_conflict_leaves_nothing_out() {
    // A's context finds the cover of `∃d.boolean` before the chain to owl:Nothing ends,
    // and an unsatisfiable class has every subsumer
    let (sat, _) = run(&format!(
        "SubClassOf(DataHasValue(:d {BOOL}) :C)\n\
         SubClassOf(DataHasValue(:d {NOT_BOOL}) :D)\n\
         SubClassOf(:A DataSomeValuesFrom(:d xsd:boolean))\n\
         SubClassOf(:A :E)\nSubClassOf(:E :F)\nSubClassOf(:F :G)\nSubClassOf(:G owl:Nothing)"
    ));
    assert!(sub(&sat, "A", "C") && sub(&sat, "A", "D"));
    assert!(sat.excluded().is_empty(), "{:?}", sat.excluded());
}

#[test]
fn a_parallel_run_with_covers_reports_what_one_thread_reports() {
    let mut body = format!(
        "SubClassOf(DataHasValue(:d {BOOL}) :C)\n\
         SubClassOf(DataHasValue(:d {NOT_BOOL}) :D)\n\
         SubClassOf(DataHasValue(:d {BOOL}) :H)\n\
         SubClassOf(DataHasValue(:d {NOT_BOOL}) :H)\n\
         SubClassOf(:A DataSomeValuesFrom(:d xsd:boolean))\n"
    );
    for i in 0..4 * PARALLEL_AXIOMS {
        body.push_str(&format!("SubClassOf(:X{i} :A)\nSubClassOf(:Y{i} :X{i})\n"));
    }
    let ax = axioms(&body);
    let all: Vec<&AxiomKind> = ax.iter().collect();
    let on = |threads: usize| {
        let mut sat = Saturation::new();
        let out = sat
            .classify(
                &all,
                None,
                &Limits {
                    threads,
                    ..Limits::default()
                },
            )
            .unwrap();
        (out.taxonomy, sat.excluded().to_vec())
    };
    let one = on(1);
    let many = on(4);
    assert_eq!(one, many);
    // the cover over C and D is left in part; both values give H
    assert!(!one.1.is_empty());
}

/// Ontologies whose only axioms outside OWL 2 EL are data restrictions that the checker
/// takes as one atom each, however many values or disjuncts their ranges have.
#[test]
fn data_disjunctions_inside_atoms_stay_with_the_horn_tier() {
    for (body, a, c) in [
        (
            "SubClassOf(:A DataSomeValuesFrom(:d DataUnionOf(xsd:boolean xsd:int)))\n\
             SubClassOf(DataSomeValuesFrom(:d DataUnionOf(xsd:boolean xsd:int xsd:string)) :C)",
            "A",
            "C",
        ),
        (
            "SubClassOf(:A DataMaxCardinality(2 :d xsd:boolean))\n\
             SubClassOf(:A DataSomeValuesFrom(:d xsd:boolean))\n\
             SubClassOf(DataSomeValuesFrom(:d xsd:boolean) :C)",
            "A",
            "C",
        ),
        (
            "DatatypeDefinition(:bi DataUnionOf(xsd:boolean xsd:int))\n\
             SubClassOf(:A DataSomeValuesFrom(:d xsd:int))\n\
             SubClassOf(DataSomeValuesFrom(:d :bi) :C)",
            "A",
            "C",
        ),
        (
            "SubClassOf(:A DataSomeValuesFrom(:d DataUnionOf(xsd:boolean xsd:int)))\n\
             SubClassOf(DataSomeValuesFrom(:d DataUnionOf(xsd:boolean xsd:int xsd:string)) :C)\n\
             SubClassOf(:P ObjectAllValuesFrom(:r :Q))\nSubClassOf(:P2 ObjectSomeValuesFrom(:r :P3))",
            "A",
            "C",
        ),
        (
            "SubClassOf(:A DataSomeValuesFrom(:d DataOneOf(\"1\"^^xsd:int \"2\"^^xsd:int)))\n\
             SubClassOf(DataSomeValuesFrom(:d xsd:int) :C)",
            "A",
            "C",
        ),
    ] {
        let (sat, _) = run(body);
        assert!(!sat.interner.dt.is_empty(), "{body}");
        assert!(sub(&sat, a, c), "{body}");
        assert!(sat.excluded().is_empty(), "{body}: {:?}", sat.excluded());
    }
}

fn int(v: u32) -> String {
    format!("\"{v}\"^^xsd:int")
}

/// Two classes that hold only of `a`, and a class C whose instances reach both, so
/// that `a` has the atoms of both only in C's world.
fn witnesses(d1: &str, d2: &str) -> String {
    format!(
        "SubClassOf(:D1 ObjectOneOf(:a))\nSubClassOf(:D2 ObjectOneOf(:a))\n\
         SubClassOf(:D1 {d1})\nSubClassOf(:D2 {d2})\n\
         SubClassOf(:C ObjectSomeValuesFrom(:r :D1))\nSubClassOf(:C ObjectSomeValuesFrom(:s :D2))\n"
    )
}

#[test]
fn a_world_joins_the_atoms_of_an_individual_and_two_witnesses() {
    // `a` has the value 1, D1 allows one value and D2 has the value 2: each pair is
    // satisfiable, the three are not, and only C's world holds them together
    let body = format!(
        "DataPropertyAssertion(:d :a {})\n{}",
        int(1),
        witnesses(
            "DataMaxCardinality(1 :d)",
            &format!("DataHasValue(:d {})", int(2))
        )
    );
    let (sat, _) = run(&body);
    assert!(!sat.interner.dt.is_empty());
    for (c, satisfiable) in [("C", false), ("D1", true), ("D2", true)] {
        let d = sat.describe(&Class::new(format!("urn:dt:{c}"))).unwrap();
        assert_eq!(d.satisfiable, satisfiable, "{c}");
    }
    assert!(sat.excluded().is_empty(), "{:?}", sat.excluded());
}

/// D1 gives `a` a value in {1, 2, 3} and D2 takes 3 away, so in C's world `a` has 1 or 2,
/// which give E or F, both G, and C ⊑ H follows by a case analysis that the Horn tier
/// does not make. No context of the global saturation holds both atoms.
fn world_cover() -> String {
    format!(
        "{}SubClassOf(DataHasValue(:d {}) :E)\nSubClassOf(DataHasValue(:d {}) :F)\n\
         SubClassOf(:E :G)\nSubClassOf(:F :G)\n\
         SubClassOf(ObjectSomeValuesFrom(:r :G) :H)\n",
        witnesses(
            &format!(
                "DataSomeValuesFrom(:d DataOneOf({} {} {}))",
                int(1),
                int(2),
                int(3)
            ),
            &format!("DataMaxCardinality(0 :d DataOneOf({}))", int(3))
        ),
        int(1),
        int(2)
    )
}

pub(super) fn world_cover_axioms() -> Vec<AxiomKind> {
    axioms(&world_cover())
}

#[test]
fn a_cover_found_only_in_a_world_is_used_in_part() {
    let (sat, ax) = run(&world_cover());
    assert!(!sub(&sat, "C", "H"));
    // the axioms of d's component, and none of the others
    for (i, k) in ax.iter().enumerate() {
        let touched = matches!(i, 2 | 3 | 6 | 7);
        assert_eq!(
            matches!(gap_of(&sat, k), Some(Gap::Partial(_))),
            touched,
            "{k:?}: {:?}",
            sat.excluded()
        );
    }
}

fn classify_checker_gaps(body: &str, reason: &'static str, expected: &[usize]) -> Classification {
    let kinds = axioms(&format!("{body}\nSubClassOf(:Untouched :Separate)"));
    let mut wanted = expected
        .iter()
        .map(|&i| Excluded {
            axiom: kinds[i].clone(),
            gap: Gap::Partial(reason),
        })
        .collect::<Vec<_>>();
    wanted.sort();
    let axioms: Vec<_> = kinds.into_iter().map(Axiom::new).collect();
    let mut classifier = Classifier::new();
    classifier.add(&axioms);
    let limits = Limits {
        threads: 1,
        ..Limits::default()
    };
    let out = classifier.classify(&limits).unwrap();
    assert!(out.complete_for_taken && !out.complete);
    let excluded = classifier.excluded();
    assert_eq!(excluded, wanted);

    let mut strict = Classifier::new();
    strict.add(&axioms);
    let Err(Error::Incomplete(refused)) = strict.classify(&Limits {
        strict: true,
        ..limits
    }) else {
        panic!("strict classification must refuse the checker gaps");
    };
    assert_eq!(refused, excluded);
    out
}

#[test]
fn late_main_checker_cover_reaches_public_coverage() {
    let cover = format!(
        "SubClassOf(DataHasValue(:d {BOOL}) :C)
         SubClassOf(DataHasValue(:d {NOT_BOOL}) :D)
         SubClassOf(:A DataSomeValuesFrom(:d xsd:boolean))"
    );
    let reason = dt_sat::reason(dt_sat::COVER);
    let out = classify_checker_gaps(&cover, reason, &[0, 1, 2]);
    assert!(out.consistent);
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.incomplete.contains(&Class::new("urn:dt:A")));
    assert!(!out.incomplete.contains(&Class::new("urn:dt:Untouched")));

    // The global assertions put the component in both empty-signature modules.
    let out = classify_checker_gaps(
        &format!(
            "{cover}
             DataPropertyAssertion(:d :a {BOOL})
             NegativeDataPropertyAssertion(:d :a {NOT_BOOL})"
        ),
        reason,
        &[0, 1, 2, 3, 4],
    );
    assert!(out.consistent);
    assert_eq!(out.consistency, Consistency::Unknown);
    assert!(out.incomplete.contains(&Class::new("urn:dt:Untouched")));

    let out = classify_checker_gaps(
        &format!("{cover}\nClassAssertion(owl:Nothing :a)"),
        reason,
        &[0, 1, 2],
    );
    assert!(!out.consistent);
    assert_eq!(out.consistency, Consistency::Inconsistent);
    assert!(out.incomplete.is_empty());
}

#[test]
fn late_world_checker_cover_reaches_public_coverage() {
    let out = classify_checker_gaps(&world_cover(), dt_sat::reason(dt_sat::COVER), &[2, 3, 6, 7]);
    assert!(out.consistent);
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.incomplete.contains(&Class::new("urn:dt:C")));
    assert!(!out.incomplete.contains(&Class::new("urn:dt:Untouched")));
}

#[test]
fn checker_key_gap_reaches_public_coverage() {
    let body = r#"HasKey(:K () (:d))
        ClassAssertion(:K :a)
        ClassAssertion(:K :b)
        DataPropertyAssertion(:d :a "true"^^xsd:boolean)
        DataPropertyAssertion(:d :b "true"^^xsd:boolean)
        DifferentIndividuals(:a :b)
        SubClassOf(:Data DataAllValuesFrom(:d xsd:boolean))"#;
    let out = classify_checker_gaps(body, index::DT_KEY, &[0]);
    assert!(out.consistent);
    assert_eq!(out.consistency, Consistency::Unknown);
    assert!(out.incomplete.contains(&Class::new("urn:dt:Untouched")));
}

#[test]
fn a_cover_in_a_world_without_an_instance_leaves_nothing_out() {
    // D3 makes every value of `a` a string, so C's world has a clash after, or before,
    // its cover, and C is unsatisfiable
    let body = format!(
        "{}SubClassOf(:D3 ObjectOneOf(:a))\nSubClassOf(:D3 DataAllValuesFrom(:d xsd:string))\n\
         SubClassOf(:C ObjectSomeValuesFrom(:u :D3))\n",
        world_cover()
    );
    let (sat, _) = run(&body);
    for (c, satisfiable) in [("C", false), ("D1", true), ("D2", true), ("D3", true)] {
        let d = sat.describe(&Class::new(format!("urn:dt:{c}"))).unwrap();
        assert_eq!(d.satisfiable, satisfiable, "{c}");
    }
    assert!(sat.excluded().is_empty(), "{:?}", sat.excluded());
}

#[test]
fn a_parallel_run_with_worlds_reports_what_one_thread_reports() {
    let mut body = world_cover();
    // a second component whose cover is decided: both values give K
    body.push_str(&format!(
        "SubClassOf(:B1 ObjectOneOf(:b))\nSubClassOf(:B2 ObjectOneOf(:b))\n\
         SubClassOf(:B1 DataSomeValuesFrom(:e DataOneOf({} {} {})))\n\
         SubClassOf(:B2 DataMaxCardinality(0 :e DataOneOf({})))\n\
         SubClassOf(DataHasValue(:e {}) :K)\nSubClassOf(DataHasValue(:e {}) :K)\n\
         SubClassOf(:C ObjectSomeValuesFrom(:r :B1))\nSubClassOf(:C ObjectSomeValuesFrom(:s :B2))\n",
        int(1),
        int(2),
        int(3),
        int(3),
        int(1),
        int(2)
    ));
    for i in 0..2 * MIN_CLASSES {
        body.push_str(&format!("SubClassOf(:C{i} :C)\n"));
    }
    let ax = axioms(&body);
    let all: Vec<&AxiomKind> = ax.iter().collect();
    let on = |threads: usize| {
        let mut sat = Saturation::new();
        let out = sat
            .classify(
                &all,
                None,
                &Limits {
                    threads,
                    ..Limits::default()
                },
            )
            .unwrap();
        (out.taxonomy, sat.excluded().to_vec())
    };
    let one = on(1);
    assert_eq!(one, on(4));
    assert!(!one.1.is_empty());
    // e's axioms are decided
    assert!(
        one.1
            .iter()
            .all(|x| !format!("{:?}", x.axiom).contains("urn:dt:e")),
        "{:?}",
        one.1
    );
}

#[test]
fn a_functional_link_joins_the_atoms_of_its_targets() {
    // the Horn analogue of the counting examples of F24 Completeness: C's two r
    // successors are one, which has the atoms of both
    let pre = "FunctionalObjectProperty(:r)\n\
               SubClassOf(:C ObjectSomeValuesFrom(:r :D1))\n\
               SubClassOf(:C ObjectSomeValuesFrom(:r :D2))\n";
    let conflict = format!(
        "{pre}SubClassOf(:D1 DataHasValue(:d {}))\nSubClassOf(:D2 DataAllValuesFrom(:d xsd:string))",
        int(1)
    );
    let (sat, _) = run(&conflict);
    assert!(!sat.describe(&Class::new("urn:dt:C")).unwrap().satisfiable);
    assert!(sat.describe(&Class::new("urn:dt:D1")).unwrap().satisfiable);
    assert!(sat.excluded().is_empty(), "{:?}", sat.excluded());

    let entailed = format!(
        "{pre}SubClassOf(:D1 DataSomeValuesFrom(:d DataOneOf({} {})))\n\
         SubClassOf(:D2 DataMaxCardinality(0 :d DataOneOf({})))\n\
         SubClassOf(DataHasValue(:d {}) :E)\nSubClassOf(ObjectSomeValuesFrom(:r :E) :H)",
        int(1),
        int(2),
        int(2),
        int(1)
    );
    let (sat, _) = run(&entailed);
    assert!(sub(&sat, "C", "H") && !sub(&sat, "D1", "E"));
    assert!(sat.excluded().is_empty(), "{:?}", sat.excluded());

    let cover = format!(
        "{pre}SubClassOf(:D1 DataSomeValuesFrom(:d DataOneOf({} {} {})))\n\
         SubClassOf(:D2 DataMaxCardinality(0 :d DataOneOf({})))\n\
         SubClassOf(DataHasValue(:d {}) :E)\nSubClassOf(DataHasValue(:d {}) :F)",
        int(1),
        int(2),
        int(3),
        int(3),
        int(1),
        int(2)
    );
    let (sat, ax) = run(&cover);
    assert!(!sub(&sat, "C", "E") && !sub(&sat, "C", "F"));
    for k in &ax[3..] {
        assert!(
            matches!(gap_of(&sat, k), Some(Gap::Partial(_))),
            "{k:?}: {:?}",
            sat.excluded()
        );
    }

    // with individuals: x's two r values are one individual, which has 1 and not 1
    let (_, _, consistent) = run_consistent(
        &format!(
            "FunctionalObjectProperty(:r)\nObjectPropertyAssertion(:r :x :b)\n\
             ObjectPropertyAssertion(:r :x :c)\nDataPropertyAssertion(:d :b {})\n\
             NegativeDataPropertyAssertion(:d :c {})",
            int(1),
            int(1)
        ),
        1,
    );
    assert!(!consistent);
}

#[test]
fn a_zero_minimum_in_a_body_holds_of_every_class() {
    let (sat, ax) = run("SubClassOf(DataMinCardinality(0 :d xsd:int) :E)\n\
         SubClassOf(:A DataAllValuesFrom(:d xsd:int))\n\
         SubClassOf(:B :Z)");
    assert!(sub(&sat, "A", "E") && sub(&sat, "B", "E"));
    assert!(gap_of(&sat, &ax[0]).is_none(), "{:?}", sat.excluded());
    // an exact zero in a body is an at-most, which the Horn tier leaves out
    let (sat, ax) = run("SubClassOf(DataExactCardinality(0 :d xsd:int) :E)\n\
         SubClassOf(:A DataAllValuesFrom(:d xsd:int))");
    assert!(gap_of(&sat, &ax[0]).is_some(), "{:?}", sat.excluded());
    assert!(!sub(&sat, "A", "E"));
}

#[test]
fn doubles_and_booleans_decide_consistency() {
    // xsd:double has a value space of its own, apart from the numbers of owl:real, and
    // "1.0" and "1" are one double; "1" is a lexical form of true
    for (body, consistent) in [
        (
            "FunctionalDataProperty(:d)\nDataPropertyAssertion(:d :a \"1.5\"^^xsd:double)\n\
             DataPropertyAssertion(:d :a \"2.5\"^^xsd:double)",
            false,
        ),
        (
            "FunctionalDataProperty(:d)\nDataPropertyAssertion(:d :a \"1.0\"^^xsd:double)\n\
             DataPropertyAssertion(:d :a \"1\"^^xsd:double)",
            true,
        ),
        (
            "FunctionalDataProperty(:d)\nDataPropertyAssertion(:d :a \"1\"^^xsd:double)\n\
             DataPropertyAssertion(:d :a \"1\"^^xsd:decimal)",
            false,
        ),
        (
            "DataPropertyRange(:d xsd:boolean)\nDataPropertyAssertion(:d :a \"abc\"^^xsd:string)",
            false,
        ),
        (
            "FunctionalDataProperty(:d)\nDataPropertyRange(:d xsd:boolean)\n\
             DataPropertyAssertion(:d :a \"true\"^^xsd:boolean)\n\
             DataPropertyAssertion(:d :a \"1\"^^xsd:boolean)",
            true,
        ),
    ] {
        let (sat, _, c) = run_consistent(body, 1);
        assert!(
            sat.interner.dt_run.is_none() && !sat.interner.dt.is_empty(),
            "{body}"
        );
        assert_eq!(c, consistent, "{body}");
        assert!(sat.excluded().is_empty(), "{body}: {:?}", sat.excluded());
    }
}

#[test]
fn what_a_world_adds_to_the_checker_cache_stays_charged_after_it() {
    use std::sync::atomic::{AtomicIsize, Ordering};
    let (mut sat, _) = run(&world_cover());
    let c = sat.interner.class(&Class::new("urn:dt:C"));
    let ix = sat.index.as_ref().expect("a saturated run");
    let dt = ix.dt.as_ref().expect("the checker's tables");
    // the run filled the cache, so the world below solves again only once it is empty
    dt.forget();
    let held = AtomicIsize::new(0);
    let bound = |delta: isize| {
        held.fetch_add(delta, Ordering::Relaxed);
        true
    };
    let world = super::world::World::new(ix, &sat.ctx, None, &[], &[], &bound);
    world.classify(c).unwrap();
    // the world's copies are released when it goes, and the cache entries it made stay
    assert!(held.load(Ordering::Relaxed) > 0);
}

#[test]
fn a_bounded_world_refuses_checker_growth_and_retires_only_its_own_bytes() {
    use super::admission_test::{Point, set_hook};
    use parking_lot::Mutex;
    use std::sync::{Arc, atomic::AtomicBool};

    const BASE: usize = 4096;
    let run_world = |cap: usize| {
        let (mut sat, _) = run(&world_cover());
        let c = sat.interner.class(&Class::new("urn:dt:C"));
        let ix = sat.index.as_ref().unwrap();
        let dt = ix.dt.as_ref().unwrap();
        dt.forget();
        let before = dt.bytes();
        let reserving = Arc::new(AtomicBool::new(false));
        let marked = reserving.clone();
        let mut totals = saturate::Totals {
            peak: BASE,
            paid: BASE,
            ..Default::default()
        };
        let limits = Limits::default();
        let first_checker_charge = Mutex::new(None);
        let refused = Mutex::new(None);
        let paid = AtomicUsize::new(BASE);
        let charge = |growth| {
            let before = paid.load(Ordering::Relaxed);
            if reserving.load(Ordering::Relaxed) {
                first_checker_charge.lock().get_or_insert((before, growth));
            }
            let used = before + growth;
            if used > cap {
                *refused.lock() = Some((before, growth));
                return false;
            }
            paid.store(used, Ordering::Relaxed);
            true
        };
        let budget = WorldBudget::start(&limits, Some(&charge), BASE, &mut totals).unwrap();
        let bound = |delta| {
            let ok = budget.tell(delta);
            reserving.store(false, Ordering::Relaxed);
            ok
        };
        set_hook(move |point| {
            if point == Point::WorldDataReserve {
                marked.store(true, Ordering::Relaxed);
            }
        });
        let result = world::World::new(ix, &sat.ctx, None, &[], &[], &bound).classify(c);
        set_hook(|_| {});
        let retired = budget.held.load(Ordering::Relaxed);
        assert_eq!(usize::try_from(retired).unwrap(), dt.bytes() - before);
        budget.finish(&mut totals);
        assert_eq!(totals.paid, paid.load(Ordering::Relaxed));
        let checkpoint = first_checker_charge.lock().expect("a checker pool request");
        if cap == usize::MAX {
            assert!(result.is_ok());
            assert!(retired > 0, "the cache must still own its admitted bytes");
            assert_eq!(budget.stopped(), Ok(()));
            assert_eq!(totals.peak, totals.paid);
        } else {
            assert_eq!(*refused.lock(), Some(checkpoint));
            assert_eq!(checkpoint.0, cap);
            assert!(matches!(result, Err(index::Halt::Stopped)));
            let used = checkpoint.0 + checkpoint.1;
            assert_eq!(
                budget.stopped(),
                Err(Error::Memory {
                    limit: usize::MAX,
                    used
                })
            );
            assert_eq!((totals.peak, totals.paid), (used, cap));
        }
        checkpoint
    };
    let (cap, growth) = run_world(usize::MAX);
    assert!(growth > 0);
    assert_eq!(run_world(cap), (cap, growth));
}

#[test]
fn el_only_does_not_activate_checker_or_use_excluded_definitions() {
    // 1. EL HasKey + owl:topDataProperty beside an outside-EL DataAllValuesFrom
    let body = "HasKey(:K () (:d))\n\
        ClassAssertion(:K :a)\n\
        ClassAssertion(:K :b)\n\
        DataPropertyAssertion(:d :a \"1\"^^xsd:integer)\n\
        DataPropertyAssertion(:d :b \"1\"^^xsd:integer)\n\
        DifferentIndividuals(:a :b)\n\
        SubClassOf(:X DataAllValuesFrom(:d xsd:string))\n\
        SubClassOf(:Y DataSomeValuesFrom(owl:topDataProperty xsd:integer))";
    let ax = axioms(body);
    let refs: Vec<&AxiomKind> = ax.iter().collect();
    let limits = Limits {
        el_only: true,
        threads: 1,
        ..Limits::default()
    };
    let mut sat = Saturation::new();
    let out = sat.saturate(&refs, None, &limits).unwrap();
    assert!(!out.consistent, "EL HasKey must still detect inconsistency");
    assert!(sat.interner.dt.is_unallocated());
    assert_eq!(sat.interner.dt.bytes(), 0);
    // Only the outside-EL DataAllValuesFrom is excluded; neither HasKey nor topDataProperty
    // should be downgraded to Partial.
    assert_eq!(sat.excluded().len(), 1, "{:?}", sat.excluded());
    assert!(matches!(sat.excluded()[0].gap, Gap::Outside { .. }));

    // 2. Excluded faceted DatatypeDefinition must not leak into EL axioms under el_only
    let body2 = "DatatypeDefinition(:small DatatypeRestriction(xsd:integer xsd:maxInclusive \"5\"^^xsd:integer))\n\
        DataPropertyRange(:d :small)\n\
        DataPropertyAssertion(:d :a \"6\"^^xsd:integer)";
    let ax2 = axioms(body2);
    let refs2: Vec<&AxiomKind> = ax2.iter().collect();
    let mut sat2 = Saturation::new();
    let out2 = sat2.saturate(&refs2, None, &limits).unwrap();
    assert!(
        out2.consistent,
        "excluded definition must not make el_only run inconsistent"
    );
    assert!(sat2.interner.dt.is_unallocated());
    assert!(
        sat2.excluded()
            .iter()
            .any(|e| matches!(e.axiom, AxiomKind::DatatypeDefinition(..)))
    );

    // 3. Same isolation when the Horn-to-EL fallback triggers under a tight memory cap
    let mut horn_body = String::from(body2);
    horn_body.push_str(
        "\nTransitiveObjectProperty(:part_of)\n\
         InverseObjectProperties(:part_of :has_part)\n\
         SubClassOf(:FilopodiumMembrane :Membrane)\n\
         SubClassOf(:FilopodiumMembrane ObjectSomeValuesFrom(:part_of :Filopodium))\n\
         SubClassOf(:Filopodium ObjectSomeValuesFrom(:part_of :Cytoskeleton))\n\
         SubClassOf(:Cytoskeleton :Organelle)\n\
         EquivalentClasses(:MembraneBounded ObjectIntersectionOf(:Organelle ObjectSomeValuesFrom(:has_part :Membrane)))\n\
         EquivalentClasses(:OrganelleMembrane ObjectIntersectionOf(:Membrane ObjectSomeValuesFrom(:part_of :MembraneBounded)))",
    );
    let ax3 = axioms(&horn_body);
    let refs3: Vec<&AxiomKind> = ax3.iter().collect();
    let mut el_sat = Saturation::new();
    let el_res = el_sat.saturate(&refs3, None, &limits).unwrap();
    let mut horn_sat = Saturation::new();
    let horn_res = horn_sat
        .saturate(
            &refs3,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    assert!(el_res.consistent);
    assert!(!horn_res.consistent);
    assert!(
        horn_res.stats.peak_memory > el_res.stats.peak_memory,
        "{} vs {}",
        horn_res.stats.peak_memory,
        el_res.stats.peak_memory
    );
    let mut fb_sat = Saturation::new();
    let fb = fb_sat
        .saturate(
            &refs3,
            None,
            &Limits {
                threads: 1,
                max_memory: Some(horn_res.stats.peak_memory - 1),
                ..Limits::default()
            },
        )
        .unwrap();
    assert!(fb.stats.horn_over_budget);
    assert!(
        fb.consistent,
        "Horn-to-EL fallback must match EL-only consistency"
    );
    assert!(fb_sat.interner.dt.is_unallocated());
}

#[test]
fn reserved_datatype_definition_cannot_shadow_builtin() {
    let body = "DatatypeDefinition(xsd:int xsd:string)\n\
        SubClassOf(:A DataHasValue(:d \"1\"^^xsd:int))\n\
        SubClassOf(:A DataAllValuesFrom(:d xsd:int))";
    let (sat, _) = run(body);
    let a = sat
        .describe(&Class::new("urn:dt:A"))
        .expect("a saturated run");
    assert!(a.satisfiable, "xsd:int must not be shadowed by xsd:string");
    assert!(
        sat.excluded()
            .iter()
            .any(|e| matches!(e.axiom, AxiomKind::DatatypeDefinition(..))),
        "{:?}",
        sat.excluded()
    );
}

#[test]
fn superseded_el_datatype_definition_is_reported() {
    let body = "DatatypeDefinition(:t xsd:integer)\n\
        DatatypeDefinition(:t xsd:int)\n\
        SubClassOf(:A DataSomeValuesFrom(:d :t))";
    let (sat, ax) = run(body);
    assert!(
        gap_of(&sat, &ax[0]).is_some(),
        "superseded EL DatatypeDefinition must be reported: {:?}",
        sat.excluded()
    );
}

#[test]
fn refused_universal_and_top_disjoint_keep_el_haskey() {
    for extra in [
        "SubClassOf(:X DataAllValuesFrom(:d DatatypeRestriction(xsd:string xsd:pattern \"a*\")))",
        "SubClassOf(:X DataMinCardinality(2 :d <urn:dt:unknown>))",
        "DisjointDataProperties(:d owl:topDataProperty)",
    ] {
        let body = format!(
            "HasKey(:K () (:d))\n\
             ClassAssertion(:K :a)\n\
             ClassAssertion(:K :b)\n\
             DataPropertyAssertion(:d :a \"1\"^^xsd:integer)\n\
             DataPropertyAssertion(:d :b \"1\"^^xsd:integer)\n\
             DifferentIndividuals(:a :b)\n\
             {extra}"
        );
        let (sat, _, consistent) = run_consistent(&body, 1);
        assert!(!consistent, "EL HasKey must stay active for {extra}");
        assert!(sat.interner.dt.is_unallocated(), "{extra}");
        assert_eq!(sat.interner.dt.bytes(), 0, "{extra}");
    }
}

#[test]
fn non_checker_runs_allocate_no_checker_tables() {
    assert!(super::index::Interner::new().dt.is_unallocated());
    assert_eq!(super::index::Interner::new().dt.bytes(), 0);
    for body in [
        "SubClassOf(:A :B)",
        "SubClassOf(:A DataSomeValuesFrom(:d xsd:integer))\n\
         SubClassOf(:B DataHasValue(:d \"1\"^^xsd:integer))\n\
         FunctionalDataProperty(:d)",
    ] {
        let (sat, _) = run(body);
        assert!(sat.interner.dt.is_unallocated(), "{body}");
        assert_eq!(sat.interner.dt.bytes(), 0, "{body}");
    }
}

#[test]
fn incremental_one_shot_cancellation_is_not_swallowed() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Test both a data-free EL incremental edit and an edit adding an outside-EL axiom.
    for (base_body, added_body) in [
        ("SubClassOf(:A :B)", "SubClassOf(:B :C)"),
        (
            "SubClassOf(:A :B)",
            "SubClassOf(:A DataSomeValuesFrom(:d xsd:int))",
        ),
    ] {
        let base_ax = axioms(base_body);
        let add_ax = axioms(added_body);
        let mut all = base_ax.clone();
        all.extend(add_ax.clone());
        let base_refs: Vec<&AxiomKind> = base_ax.iter().collect();
        let all_refs: Vec<&AxiomKind> = all.iter().collect();
        let add_refs: Vec<&AxiomKind> = add_ax.iter().collect();

        // Count polls on an uncancelled incremental run, then fire a one-shot cancel at
        // every poll point.
        let total = Arc::new(AtomicUsize::new(0));
        {
            let mut sat = Saturation::new();
            sat.saturate(
                &base_refs,
                None,
                &Limits {
                    threads: 1,
                    ..Limits::default()
                },
            )
            .unwrap();
            let c = total.clone();
            let limits = Limits {
                threads: 1,
                cancelled: Some(Arc::new(move || {
                    c.fetch_add(1, Ordering::Relaxed);
                    false
                })),
                ..Limits::default()
            };
            sat.saturate(
                &all_refs,
                Some(Changes {
                    added: &add_refs,
                    removed: &[],
                }),
                &limits,
            )
            .unwrap();
        }
        let n = total.load(Ordering::Relaxed);
        assert!(n >= 2, "expected at least 2 poll checkpoints, got {n}");
        for k in 1..=n {
            let mut sat = Saturation::new();
            sat.saturate(
                &base_refs,
                None,
                &Limits {
                    threads: 1,
                    ..Limits::default()
                },
            )
            .unwrap();
            let count = Arc::new(AtomicUsize::new(0));
            let c = count.clone();
            let limits = Limits {
                threads: 1,
                cancelled: Some(Arc::new(move || c.fetch_add(1, Ordering::Relaxed) + 1 == k)),
                ..Limits::default()
            };
            let res = sat.saturate(
                &all_refs,
                Some(Changes {
                    added: &add_refs,
                    removed: &[],
                }),
                &limits,
            );
            assert!(
                matches!(res, Err(Error::Cancelled)),
                "one-shot cancel at poll {k}/{n} for ({base_body} + {added_body}) returned {res:?}"
            );
        }
    }
}

#[test]
fn prepass_and_saturation_admit_memory_before_growing() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let make_pool = |cap: usize| {
        let granted = Arc::new(AtomicUsize::new(0));
        let g = granted.clone();
        let limits = Limits {
            threads: 1,
            max_memory: Some(cap),
            charge: Some(Arc::new(move |bytes| {
                let prev = g.fetch_add(bytes, Ordering::Relaxed);
                if prev.saturating_add(bytes) > cap {
                    g.fetch_sub(bytes, Ordering::Relaxed);
                    false
                } else {
                    true
                }
            })),
            ..Limits::default()
        };
        (granted, limits)
    };

    // 1. Pre-pass on a large string DataOneOf under a tight max_memory + charge callback
    let lits: String = (0..300)
        .map(|i| format!("\"{i:0>200}\"^^xsd:string"))
        .collect::<Vec<_>>()
        .join(" ");
    let body = format!("SubClassOf(:A DataSomeValuesFrom(:d DataOneOf({lits})))");
    let ax = axioms(&body);
    let refs: Vec<&AxiomKind> = ax.iter().collect();
    let cap = 24 * 1024usize;
    let (granted, limits) = make_pool(cap);
    let mut sat = Saturation::new();
    let res = sat.saturate(&refs, None, &limits);
    assert!(
        matches!(res, Err(Error::Memory { .. })),
        "expected Error::Memory, got {res:?}"
    );
    assert!(granted.load(Ordering::Relaxed) <= cap);
    assert!(sat.interner.dt.is_unallocated());

    // 2. Large DatatypeDefinition cloning is pre-admitted by AST size before copying
    let def_body = format!(
        "DatatypeDefinition(:big DataOneOf({lits}))\nSubClassOf(:A DataSomeValuesFrom(:d :big))"
    );
    let def_ax = axioms(&def_body);
    let def_refs: Vec<&AxiomKind> = def_ax.iter().collect();
    let (granted2, limits2) = make_pool(cap);
    let mut sat2 = Saturation::new();
    let res2 = sat2.saturate(&def_refs, None, &limits2);
    assert!(
        matches!(res2, Err(Error::Memory { .. })),
        "expected Error::Memory for huge DatatypeDefinition, got {res2:?}"
    );
    assert!(granted2.load(Ordering::Relaxed) <= cap);

    // 3. Superrole fan-out in dt_sat::solve admits memory before expanding superrole constraints
    let mut fan_body = String::from("SubClassOf(:A DataSomeValuesFrom(:d0 xsd:int))\n");
    for i in 1..=200 {
        fan_body.push_str(&format!(
            "SubDataPropertyOf(:d0 :s{i})\n\
             DataPropertyRange(:s{i} DatatypeRestriction(xsd:int xsd:minInclusive \"{i}\"^^xsd:int))\n"
        ));
    }
    let fan_ax = axioms(&fan_body);
    let fan_refs: Vec<&AxiomKind> = fan_ax.iter().collect();
    let mut uncap = Saturation::new();
    let full = uncap
        .saturate(
            &fan_refs,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    let tight = full.stats.peak_memory / 2;
    let (granted3, limits3) = make_pool(tight);
    let mut sat3 = Saturation::new();
    let res3 = sat3.saturate(&fan_refs, None, &limits3);
    assert!(
        matches!(res3, Err(Error::Memory { .. }))
            || matches!(res3, Ok(ref o) if o.stats.horn_over_budget),
        "expected Error::Memory or horn_over_budget under tight cap {tight}, got {res3:?}"
    );
    assert!(granted3.load(Ordering::Relaxed) <= tight);
    assert!(sat3.interner.dt.is_unallocated());
}

#[test]
fn lang_string_range_reaches_checker_fresh_and_incrementally() {
    // rdf:langString is accepted by profiles::el_axiom (outside is empty), yet Walk::el_range
    // routes it to the checker, which decides inconsistency with an xsd:string value.
    let base_ax = axioms("DataPropertyAssertion(:d :a \"x\"^^xsd:string)");
    let add_ax =
        axioms("DataPropertyRange(:d <http://www.w3.org/1999/02/22-rdf-syntax-ns#langString>)");
    let mut all = base_ax.clone();
    all.extend(add_ax.clone());
    let base_refs: Vec<&AxiomKind> = base_ax.iter().collect();
    let all_refs: Vec<&AxiomKind> = all.iter().collect();
    let add_refs: Vec<&AxiomKind> = add_ax.iter().collect();
    let limits = Limits {
        threads: 1,
        ..Limits::default()
    };

    let mut fresh = Saturation::new();
    let out_fresh = fresh.saturate(&all_refs, None, &limits).unwrap();
    assert!(!out_fresh.consistent);
    assert!(fresh.excluded().is_empty(), "{:?}", fresh.excluded());

    let mut inc = Saturation::new();
    let out0 = inc.saturate(&base_refs, None, &limits).unwrap();
    assert!(out0.consistent);
    let out_inc = inc
        .saturate(
            &all_refs,
            Some(Changes {
                added: &add_refs,
                removed: &[],
            }),
            &limits,
        )
        .unwrap();
    assert!(!out_inc.consistent);
    assert!(inc.excluded().is_empty(), "{:?}", inc.excluded());
}

#[test]
fn retained_bytes_counts_interner_dt_and_large_cached_answers() {
    // Many nested at-least body restrictions entailed by one positive restriction
    let mut body = String::from(
        "SubClassOf(:A DataSomeValuesFrom(:d DatatypeRestriction(xsd:int xsd:minInclusive \"100\"^^xsd:int)))\n",
    );
    for i in 1..=64 {
        body.push_str(&format!(
            "SubClassOf(DataSomeValuesFrom(:d DatatypeRestriction(xsd:int xsd:minInclusive \"{i}\"^^xsd:int)) :C{i})\n"
        ));
    }
    let (sat, _) = run(&body);
    assert!(sat.interner.dt.bytes() > 0);
    assert!(sat.interner.data_bytes() >= sat.interner.dt.bytes());
    let ix = sat.index.as_ref().expect("completed index");
    let dt = ix.dt.as_ref().expect("checker tables");
    assert!(dt.bytes() >= dt.comps.bytes() + 64 * std::mem::size_of::<usize>());
}

#[test]
fn tier_d_with_excluded_data_keeps_incremental_reuse() {
    let base = axioms(
        "SubClassOf(:X ObjectUnionOf(:Y :Z))\n\
         SubClassOf(:Y :U)\n\
         SubClassOf(:Z :U)\n\
         SubClassOf(:A DataHasValue(:d \"1\"^^xsd:int))",
    );
    let add = axioms("SubClassOf(:W :V)");
    let mut all = base.clone();
    all.extend(add.clone());
    let base_refs: Vec<&AxiomKind> = base.iter().collect();
    let all_refs: Vec<&AxiomKind> = all.iter().collect();
    let add_refs: Vec<&AxiomKind> = add.iter().collect();
    let limits = Limits {
        threads: 1,
        ..Limits::default()
    };
    let mut sat = Saturation::new();
    let before = sat.classify(&base_refs, None, &limits).unwrap();
    assert!(before.complete_for_taken && !before.complete);
    assert!(sub(&sat, "X", "U"));
    assert!(matches!(gap_of(&sat, &base[3]), Some(Gap::Outside { .. })));
    assert!(
        sat.index.as_ref().unwrap().dt.is_none(),
        "Tier D run must not build ix.dt"
    );
    let out = sat
        .saturate(
            &all_refs,
            Some(Changes {
                added: &add_refs,
                removed: &[],
            }),
            &limits,
        )
        .unwrap();
    assert!(out.stats.incremental, "Tier D run should stay incremental");
}

#[test]
fn rooted_reduction_declines_checker_data_and_uses_original_axioms() {
    let ax = axioms(
        "ObjectPropertyDomain(owl:topObjectProperty :Global)
         SubClassOf(:Data DataSomeValuesFrom(:d xsd:integer))
         SubClassOf(DataSomeValuesFrom(:d xsd:integer) :Handled)
         SubClassOf(:Data DataAllValuesFrom(:d xsd:int))
         SubClassOf(:X ObjectUnionOf(:Y :Z))",
    );
    let refs: Vec<_> = ax.iter().collect();
    assert!(dl_reduce::plan(&[&ax[0], &ax[4]], None).is_some());
    assert!(dl_reduce::plan(&refs, None).is_none());
    let mut sat = Saturation::new();
    let rooted = sat
        .classify(
            &refs,
            None,
            &Limits {
                threads: 1,
                rooted: true,
                ..Limits::default()
            },
        )
        .unwrap();
    assert!(!sat.reduced);
    let ix = sat.index.as_ref().unwrap();
    assert!(ix.dt.is_some() && ix.dl.is_none());
    assert!(sub(&sat, "Data", "Handled"));
    assert!(rooted.complete_for_taken && !rooted.complete);
    assert!(gap_of(&sat, &ax[4]).is_some());
    assert!(
        sat.interner
            .classes
            .iter()
            .all(|c| !c.as_str().starts_with(dl_reduce::PREFIX))
    );

    let mut plain = Saturation::new();
    let plain_out = plain
        .classify(
            &refs,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    assert_eq!(rooted.taxonomy, plain_out.taxonomy);
    assert_eq!(rooted.consistency, plain_out.consistency);
    assert_eq!(sat.excluded(), plain.excluded());
}

#[test]
fn rooted_tier_d_with_data_only_outside_el_keeps_original_exclusions() {
    let ax = axioms(
        r#"ClassAssertion(:Known :a)
         SubClassOf(:X ObjectUnionOf(:Y :Z))
         SubClassOf(:Y :U)
         SubClassOf(:Z :U)
         SubClassOf(:Data DataHasValue(:d "1"^^xsd:int))"#,
    );
    let refs: Vec<_> = ax.iter().collect();
    let mut sat = Saturation::new();
    let out = sat
        .classify(
            &refs,
            None,
            &Limits {
                threads: 1,
                rooted: true,
                ..Limits::default()
            },
        )
        .unwrap();
    assert!(!sat.reduced && sat.dl_rooted);
    let ix = sat.index.as_ref().unwrap();
    assert!(ix.dl.is_some() && ix.dt.is_none());
    assert!(out.complete_for_taken && !out.complete);
    assert!(sub(&sat, "X", "U"));
    assert_eq!(sat.excluded().len(), 1, "{:?}", sat.excluded());
    assert!(matches!(gap_of(&sat, &ax[4]), Some(Gap::Outside { .. })));
    assert!(
        out.taxonomy
            .classes()
            .all(|c| !c.as_str().starts_with(dl_reduce::PREFIX))
    );
}

#[test]
fn counted_tier_d_fallback_keeps_checker_data_outside_the_index() {
    use std::fmt::Write as _;
    use std::sync::Arc;

    let mut body = String::from(
        r#"SubClassOf(:Data DataHasValue(:d "1"^^xsd:int))
         SubClassOf(:Count ObjectMaxCardinality(1 :r))
         SubClassOf(:Y :U)
         SubClassOf(:Z :U)
         "#,
    );
    for i in 0..24 {
        writeln!(body, "SubClassOf(:X{i} ObjectUnionOf(:Y :Z))").unwrap();
    }
    let ax = axioms(&body);
    let refs: Vec<_> = ax.iter().collect();
    let default = Limits {
        threads: 1,
        ..Limits::default()
    };
    let paid = Arc::new(AtomicUsize::new(0));
    let charged = paid.clone();
    let limits = Limits {
        counting: true,
        dl_work: Some(1),
        charge: Some(Arc::new(move |bytes| {
            charged.fetch_add(bytes, Ordering::Relaxed);
            true
        })),
        ..default.clone()
    };
    let mut sat = Saturation::new();
    let out = sat.classify(&refs, None, &limits).unwrap();
    assert!(out.stats.dl_counting_over && out.stats.dl_over_budget);
    assert!(out.complete_for_taken && !out.complete);
    assert!(sat.index.as_ref().unwrap().dt.is_none() && !sat.dl_counted);
    assert!(sub(&sat, "X0", "U"));
    assert!(out.stats.memory <= paid.load(Ordering::Relaxed));
    assert!(matches!(gap_of(&sat, &ax[0]), Some(Gap::Outside { .. })));

    let mut plain = Saturation::new();
    let want = plain.classify(&refs, None, &default).unwrap();
    assert_eq!(out.taxonomy, want.taxonomy);
    assert_eq!(sat.excluded(), plain.excluded());
}

#[test]
fn shrinking_incremental_edit_into_checker_retries_fresh_and_honors_pool_cap() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let make_pool = |cap: usize| {
        let granted = Arc::new(AtomicUsize::new(0));
        let g = granted.clone();
        let limits = Limits {
            threads: 1,
            max_memory: Some(cap),
            charge: Some(Arc::new(move |bytes| {
                let prev = g.fetch_add(bytes, Ordering::Relaxed);
                if prev.saturating_add(bytes) > cap {
                    g.fetch_sub(bytes, Ordering::Relaxed);
                    false
                } else {
                    true
                }
            })),
            ..Limits::default()
        };
        (granted, limits)
    };

    // 1. Large kept EL ontology shrinks via Changes into a small checker ontology under a
    // budget that fits a fresh run of the small ontology but is smaller than the kept state.
    let mut big_body = String::new();
    for i in 0..1500 {
        big_body.push_str(&format!("SubClassOf(:K{i} :K{})\n", i + 1));
    }
    let big_ax = axioms(&big_body);
    let big_refs: Vec<&AxiomKind> = big_ax.iter().collect();
    let small_ax = axioms(
        "SubClassOf(:A DataHasValue(:d \"1\"^^xsd:int))\n\
         SubClassOf(:A DataAllValuesFrom(:d xsd:string))",
    );
    let small_refs: Vec<&AxiomKind> = small_ax.iter().collect();

    let mut fresh = Saturation::new();
    let fresh_out = fresh
        .saturate(
            &small_refs,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    assert!(!fresh.describe(&Class::new("urn:dt:A")).unwrap().satisfiable);

    // Test both same RuleKey (`el_only: false -> false`) and RuleKey change (`true -> false`).
    for initial_el_only in [false, true] {
        let mut sat = Saturation::new();
        let base_out = sat
            .saturate(
                &big_refs,
                None,
                &Limits {
                    threads: 1,
                    el_only: initial_el_only,
                    ..Limits::default()
                },
            )
            .unwrap();
        assert!(
            base_out.stats.memory > fresh_out.stats.peak_memory * 2,
            "base memory {} must exceed fresh peak {}",
            base_out.stats.memory,
            fresh_out.stats.peak_memory
        );
        let tight_cap = fresh_out.stats.peak_memory + 32 * 1024;
        assert!(tight_cap < base_out.stats.memory);
        let (granted, limits) = make_pool(tight_cap);
        let inc_out = sat
            .saturate(
                &small_refs,
                Some(Changes {
                    added: &small_refs,
                    removed: &big_refs,
                }),
                &limits,
            )
            .expect("shrinking edit must drop dead kept state and succeed fresh");
        assert!(!sat.describe(&Class::new("urn:dt:A")).unwrap().satisfiable);
        assert!(granted.load(Ordering::Relaxed) <= tight_cap);
        assert!(
            granted.load(Ordering::Relaxed) >= inc_out.stats.peak_memory,
            "pool grant {} must cover peak_memory {}",
            granted.load(Ordering::Relaxed),
            inc_out.stats.peak_memory
        );
    }

    // 2. Pool-cap saturation check: shrinking a large kept EL ontology into a checker
    // ontology whose saturation grows well past its build peak must NOT bypass the pool cap.
    let mut sat_heavy = String::from(
        "TransitiveObjectProperty(:part_of)\n\
         InverseObjectProperties(:part_of :has_part)\n\
         SubClassOf(:FilopodiumMembrane :Membrane)\n\
         SubClassOf(:FilopodiumMembrane ObjectSomeValuesFrom(:part_of :Filopodium))\n\
         SubClassOf(:Filopodium ObjectSomeValuesFrom(:part_of :Cytoskeleton))\n\
         SubClassOf(:Cytoskeleton :Organelle)\n\
         EquivalentClasses(:MembraneBounded ObjectIntersectionOf(:Organelle ObjectSomeValuesFrom(:has_part :Membrane)))\n\
         EquivalentClasses(:OrganelleMembrane ObjectIntersectionOf(:Membrane ObjectSomeValuesFrom(:part_of :MembraneBounded)))\n",
    );
    // 10 distinct small ranges combined across 45 classes produce many distinct dt_sat
    // cache keys and range meets during saturation while keeping build small.
    for i in 0..10 {
        sat_heavy.push_str(&format!(
            "SubClassOf(DataSomeValuesFrom(:d DatatypeRestriction(xsd:int xsd:minInclusive \"{i}\"^^xsd:int)) :G{i})\n"
        ));
    }
    for i in 0..10 {
        for j in (i + 1)..10 {
            sat_heavy.push_str(&format!(
                "SubClassOf(:P{i}_{j} DataSomeValuesFrom(:d DatatypeRestriction(xsd:int xsd:minInclusive \"{i}\"^^xsd:int)))\n\
                 SubClassOf(:P{i}_{j} DataAllValuesFrom(:d DatatypeRestriction(xsd:int xsd:maxInclusive \"{j}\"^^xsd:int)))\n"
            ));
        }
    }
    let fan_ax = axioms(&sat_heavy);
    let fan_refs: Vec<&AxiomKind> = fan_ax.iter().collect();
    let mut uncap_fan = Saturation::new();
    let full_fan = uncap_fan
        .saturate(
            &fan_refs,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    let mut el_fan = Saturation::new();
    let el_fan_out = el_fan
        .saturate(
            &fan_refs,
            None,
            &Limits {
                threads: 1,
                el_only: true,
                ..Limits::default()
            },
        )
        .unwrap();
    assert!(
        full_fan.stats.peak_memory > el_fan_out.stats.peak_memory * 2,
        "full peak {} vs el peak {}",
        full_fan.stats.peak_memory,
        el_fan_out.stats.peak_memory
    );
    let pool_cap = full_fan.stats.peak_memory - 1;
    let mut sat_pool = Saturation::new();
    sat_pool
        .saturate(
            &big_refs,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    let granted_pool = Arc::new(AtomicUsize::new(0));
    let gp = granted_pool.clone();
    let pool_only_limits = Limits {
        threads: 1,
        max_memory: None,
        charge: Some(Arc::new(move |bytes| {
            let prev = gp.fetch_add(bytes, Ordering::Relaxed);
            if prev.saturating_add(bytes) > pool_cap {
                gp.fetch_sub(bytes, Ordering::Relaxed);
                false
            } else {
                true
            }
        })),
        ..Limits::default()
    };
    let out_pool = sat_pool
        .saturate(
            &fan_refs,
            Some(Changes {
                added: &fan_refs,
                removed: &big_refs,
            }),
            &pool_only_limits,
        )
        .expect("Horn saturation over pool cap falls back to EL");
    assert!(out_pool.stats.horn_over_budget);
    assert!(granted_pool.load(Ordering::Relaxed) <= pool_cap);

    // 3. Kept EL ontology with a large exact integer literal: adding a refused data axiom
    // does not double-count the numeric ledger.
    let big_digits = "9".repeat(4000);
    let num_base = axioms(&format!(
        "SubClassOf(:A DataHasValue(:n \"{big_digits}\"^^xsd:integer))"
    ));
    let num_add = axioms(
        "SubClassOf(:X DataAllValuesFrom(:d DatatypeRestriction(xsd:string xsd:pattern \"a*\")))",
    );
    let mut num_all = num_base.clone();
    num_all.extend(num_add.clone());
    let num_base_refs: Vec<&AxiomKind> = num_base.iter().collect();
    let num_all_refs: Vec<&AxiomKind> = num_all.iter().collect();
    let num_add_refs: Vec<&AxiomKind> = num_add.iter().collect();

    let mut fresh_num = Saturation::new();
    let fresh_num_out = fresh_num
        .saturate(
            &num_all_refs,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    let mut inc_num = Saturation::new();
    inc_num
        .saturate(
            &num_base_refs,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    let (granted_num, limits_num) = make_pool(fresh_num_out.stats.peak_memory + 16 * 1024);
    let inc_num_out = inc_num
        .saturate(
            &num_all_refs,
            Some(Changes {
                added: &num_add_refs,
                removed: &[],
            }),
            &limits_num,
        )
        .expect("numeric ledger must not be counted twice");
    assert!(inc_num_out.consistent);
    assert!(granted_num.load(Ordering::Relaxed) <= fresh_num_out.stats.peak_memory + 16 * 1024);
}

#[test]
fn dt_index_copy_admits_memory_before_cloning() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Build an ontology with many distinct checker ranges so `it.dt.ranges()` is large.
    let mut body = String::new();
    for i in 0..80 {
        body.push_str(&format!(
            "SubClassOf(:A{i} DataSomeValuesFrom(:d DataOneOf(\"{i:0>120}\"^^xsd:string \"x{i:0>120}\"^^xsd:string)))\n"
        ));
    }
    let ax = axioms(&body);
    let refs: Vec<&AxiomKind> = ax.iter().collect();
    let mut uncap = Saturation::new();
    let full = uncap
        .saturate(
            &refs,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    let ix_dt_bytes = uncap.index.as_ref().unwrap().dt.as_ref().unwrap().bytes();
    assert!(ix_dt_bytes > 20_000);
    // Cap well below `full.stats.peak_memory` so `dt_index` refuses before copying.
    let cap = full.stats.peak_memory.saturating_sub(ix_dt_bytes / 2);
    let granted = Arc::new(AtomicUsize::new(0));
    let g = granted.clone();
    let limits = Limits {
        threads: 1,
        max_memory: Some(cap),
        charge: Some(Arc::new(move |bytes| {
            let prev = g.fetch_add(bytes, Ordering::Relaxed);
            if prev.saturating_add(bytes) > cap {
                g.fetch_sub(bytes, Ordering::Relaxed);
                false
            } else {
                true
            }
        })),
        ..Limits::default()
    };
    let mut sat = Saturation::new();
    let res = sat.saturate(&refs, None, &limits);
    assert!(
        matches!(res, Err(Error::Memory { .. }))
            || matches!(res, Ok(ref o) if o.stats.horn_over_budget),
        "expected memory refusal or Horn fallback under cap {cap}, got {res:?}"
    );
    assert!(granted.load(Ordering::Relaxed) <= cap);
}

#[test]
fn dt_index_star_hierarchy_upfront_admission_covers_per_role_vector_capacities() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // 256 child data properties under one root, each with a shared DataPropertyRange, and
    // a DataMinCardinality(2) on the root putting all 257 properties in one component.
    let mut ofn = String::from(
        "Prefix(:=<http://example.org/>)\nPrefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\nOntology(\n",
    );
    ofn.push_str("  Declaration(Class(:A))\n  Declaration(DataProperty(:root))\n");
    ofn.push_str("  SubClassOf(:A DataMinCardinality(2 :root xsd:integer))\n");
    for i in 0..256usize {
        ofn.push_str(&format!(
            "  Declaration(DataProperty(:p{i}))\n  SubDataPropertyOf(:p{i} :root)\n  DataPropertyRange(:p{i} xsd:integer)\n"
        ));
    }
    ofn.push_str(")\n");
    let doc = crate::ofn_read::parse(&ofn).unwrap();
    let refs: Vec<_> = doc.axioms.iter().map(|a| &a.kind).collect();

    // Uncapped run exercises `debug_assert!(total_est >= retained)` inside `dt_index`.
    let mut uncap = Saturation::new();
    let full = uncap
        .saturate(
            &refs,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    let dt = uncap.index.as_ref().unwrap().dt.as_ref().unwrap();
    assert_eq!(dt.roles.len(), 257);
    let ix_dt_bytes = dt.bytes();
    assert!(full.stats.peak_memory >= ix_dt_bytes);
    // Every child has exact 1-slot preallocated `supers` and min-4 `ranges` capacity.
    let mut children = 0usize;
    for ri in dt.roles.values() {
        if !ri.supers.is_empty() {
            children += 1;
            assert_eq!(ri.supers.len(), 1);
            assert_eq!(ri.supers.capacity(), 1);
            assert_eq!(ri.ranges.len(), 1);
            assert!(ri.ranges.capacity() >= 4);
        }
    }
    assert_eq!(children, 256);

    // Tight budget just below peak refuses cleanly without exceeding the pool cap.
    let cap = full.stats.peak_memory.saturating_sub(ix_dt_bytes / 4);
    let granted = Arc::new(AtomicUsize::new(0));
    let g = granted.clone();
    let limits = Limits {
        threads: 1,
        max_memory: Some(cap),
        charge: Some(Arc::new(move |bytes| {
            let prev = g.fetch_add(bytes, Ordering::Relaxed);
            if prev.saturating_add(bytes) > cap {
                g.fetch_sub(bytes, Ordering::Relaxed);
                false
            } else {
                true
            }
        })),
        ..Limits::default()
    };
    let mut sat = Saturation::new();
    let res = sat.saturate(&refs, None, &limits);
    assert!(
        matches!(res, Err(Error::Memory { .. }))
            || matches!(res, Ok(ref o) if o.stats.horn_over_budget),
        "expected memory refusal or Horn fallback under cap {cap}, got {res:?}"
    );
    assert!(granted.load(Ordering::Relaxed) <= cap);
}

#[test]
fn dt_index_absent_ranges_prices_default_without_allocating() {
    use super::dtcheck::range::Ranges;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Compile-time default table estimate matches a freshly constructed `Ranges::new()`,
    // and evaluating it in a `const` item prevents any lazy/allocating runtime fallback.
    const DEFAULT_BYTES: usize = Ranges::default_bytes();
    assert_eq!(DEFAULT_BYTES, Ranges::new().bytes());

    // Unqualified `DataMinCardinality(2 :d)` activates a checker component without
    // converting any data range, so `it.dt.ranges()` stays `None` when `dt_index` runs.
    let ofn = "Prefix(:=<http://example.org/>)\n\
               Ontology(\n\
                 Declaration(Class(:A))\n\
                 Declaration(DataProperty(:d))\n\
                 SubClassOf(:A DataMinCardinality(2 :d))\n\
               )\n";
    let doc = crate::ofn_read::parse(ofn).unwrap();
    let refs: Vec<_> = doc.axioms.iter().map(|a| &a.kind).collect();

    Ranges::reset_new_calls();
    let mut uncap = Saturation::new();
    let full = uncap
        .saturate(
            &refs,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    assert!(uncap.interner.dt.ranges().is_none());
    assert!(uncap.index.as_ref().unwrap().dt.is_some());
    // Exactly one `Ranges::new()` for the retained `DtIx`, never an extra pre-admission table.
    assert_eq!(Ranges::new_calls(), 1);
    assert!(full.stats.peak_memory >= DEFAULT_BYTES);

    // Under a cap large enough for pre-pass/normalization (~2-3 KiB) but strictly below
    // `DEFAULT_BYTES` (~10 KiB), `dt_index` must refuse inside `g.grow(total_est)`
    // before `Ranges::new()` is ever called.
    let cap = DEFAULT_BYTES.saturating_sub(1);
    let granted = Arc::new(AtomicUsize::new(0));
    let g = granted.clone();
    let limits = Limits {
        threads: 1,
        max_memory: Some(cap),
        charge: Some(Arc::new(move |bytes| {
            let prev = g.fetch_add(bytes, Ordering::Relaxed);
            if prev.saturating_add(bytes) > cap {
                g.fetch_sub(bytes, Ordering::Relaxed);
                false
            } else {
                true
            }
        })),
        ..Limits::default()
    };
    Ranges::reset_new_calls();
    let mut sat = Saturation::new();
    let res = sat.saturate(&refs, None, &limits);
    assert!(
        matches!(res, Err(Error::Memory { .. }))
            || matches!(res, Ok(ref o) if o.stats.horn_over_budget),
        "expected memory refusal or Horn fallback under cap {cap}, got {res:?}"
    );
    assert!(granted.load(Ordering::Relaxed) <= cap);
    assert_eq!(Ranges::new_calls(), 0);
}

#[test]
fn dtix_align_preserves_uncounted_and_renumbers_untouched_components() {
    // Component :u triggers UNCOUNTED (a finite but uncounted cell: xsd:token of length 2
    // with a 4e9 minimum cardinality, matching dtcheck::tests), while component :e is
    // edited and a new component :z is introduced ahead of :u to force component renumbering.
    let base = "SubClassOf(:U DataMinCardinality(4000000000 :u DatatypeRestriction(xsd:token xsd:length \"2\"^^xsd:integer)))
        SubClassOf(:E DataAllValuesFrom(:e xsd:integer))
        SubClassOf(:E DataSomeValuesFrom(:e xsd:integer))
        SubClassOf(:K :L)";
    let ax1 = axioms(base);
    let refs1: Vec<&AxiomKind> = ax1.iter().collect();
    let limits = Limits {
        threads: 1,
        ..Limits::default()
    };
    let mut sat = Saturation::new();
    let first = sat.classify(&refs1, None, &limits).unwrap();
    assert!(!first.stats.incremental);
    assert!(
        !sat.excluded().is_empty(),
        "initial run must report UNCOUNTED in excluded"
    );
    let old_dt = sat.index.as_ref().unwrap().dt.as_ref().unwrap();
    let old_u_comp = old_dt
        .comps
        .component(&DataProperty::new("urn:dt:u"))
        .expect("component :u");
    let u_ex = sat
        .interner
        .get(&index::Node::Class(
            sat.interner
                .classes
                .iter()
                .position(|c| c.as_str() == "urn:dt:U")
                .unwrap() as u32,
        ))
        .unwrap();
    let old_last = sat.ctx[u_ex as usize]
        .get()
        .unwrap()
        .st
        .lock()
        .extra
        .as_ref()
        .unwrap()
        .dt
        .iter()
        .find(|s| s.comp == old_u_comp)
        .unwrap()
        .last;
    assert_eq!(
        old_last,
        dt_sat::UNCOUNTED,
        "fixture must genuinely produce dt_sat::UNCOUNTED"
    );

    // Add a new component :z ahead of :u (shifting :u's component id) and edit :e;
    // :u must stay aligned, kept without re-evaluation, renumbered, and preserve UNCOUNTED.
    let added = axioms(
        "SubClassOf(:Z DataAllValuesFrom(:z xsd:string))
         SubClassOf(:E DataSomeValuesFrom(:e xsd:int))",
    );
    let mut ax2 = added.clone();
    ax2.extend(ax1.iter().cloned());
    let refs2: Vec<&AxiomKind> = ax2.iter().collect();
    let add_refs: Vec<&AxiomKind> = added.iter().collect();
    let second = sat
        .classify(
            &refs2,
            Some(Changes {
                added: &add_refs,
                removed: &[],
            }),
            &limits,
        )
        .unwrap();
    assert!(second.stats.incremental);
    let new_dt = sat.index.as_ref().unwrap().dt.as_ref().unwrap();
    let new_u_comp = new_dt
        .comps
        .component(&DataProperty::new("urn:dt:u"))
        .expect("component :u after edit");
    assert_ne!(
        old_u_comp, new_u_comp,
        "component :u must actually be renumbered by inserting :z ahead of it"
    );
    assert_eq!(
        new_dt.cache_entries_for(new_u_comp),
        0,
        "kept component :u must not be cleared and re-evaluated"
    );
    let kept_u = sat.ctx[u_ex as usize].get().unwrap().st.lock();
    assert!(kept_u.init, "context :U must remain initialized");
    let new_dt_st = kept_u
        .extra
        .as_ref()
        .unwrap()
        .dt
        .iter()
        .find(|s| s.comp == new_u_comp)
        .expect("kept context :U must be remapped to new component id");
    assert_eq!(new_dt_st.last, dt_sat::UNCOUNTED);
    drop(kept_u);
    assert!(!sat.excluded().is_empty());

    let mut fresh = Saturation::new();
    let want = fresh.classify(&refs2, None, &limits).unwrap();
    assert_eq!(second.taxonomy, want.taxonomy);
    assert_eq!(sat.excluded(), fresh.excluded());
}

#[test]
fn dtix_align_preserves_bound_and_renumbers_untouched_components() {
    // Component :b0..:b12 has 13 linked data properties (> Bounds::default().props == 12),
    // triggering dt_sat::BOUND on :B while component :e is edited and :z shifts IDs.
    let mut chain = String::new();
    for i in 0..12 {
        chain.push_str(&format!("SubDataPropertyOf(:b{i} :b{})\n", i + 1));
    }
    let base = format!(
        "{chain}
         SubClassOf(:B DataAllValuesFrom(:b0 xsd:integer))
         SubClassOf(:B DataSomeValuesFrom(:b0 xsd:integer))
         SubClassOf(:E DataAllValuesFrom(:e xsd:integer))
         SubClassOf(:E DataSomeValuesFrom(:e xsd:integer))
         SubClassOf(:K :L)"
    );
    let ax1 = axioms(&base);
    let refs1: Vec<&AxiomKind> = ax1.iter().collect();
    let limits = Limits {
        threads: 1,
        ..Limits::default()
    };
    let mut sat = Saturation::new();
    let first = sat.classify(&refs1, None, &limits).unwrap();
    assert!(!first.stats.incremental);
    assert!(
        !sat.excluded().is_empty(),
        "initial run must report BOUND in excluded"
    );
    let old_dt = sat.index.as_ref().unwrap().dt.as_ref().unwrap();
    let old_b_comp = old_dt
        .comps
        .component(&DataProperty::new("urn:dt:b0"))
        .expect("component :b0");
    let b_ex = sat
        .interner
        .get(&index::Node::Class(
            sat.interner
                .classes
                .iter()
                .position(|c| c.as_str() == "urn:dt:B")
                .unwrap() as u32,
        ))
        .unwrap();
    let old_last = sat.ctx[b_ex as usize]
        .get()
        .unwrap()
        .st
        .lock()
        .extra
        .as_ref()
        .unwrap()
        .dt
        .iter()
        .find(|s| s.comp == old_b_comp)
        .unwrap()
        .last;
    assert_eq!(
        old_last,
        dt_sat::BOUND,
        "13-property component must trigger dt_sat::BOUND"
    );

    let added = axioms(
        "SubClassOf(:Z DataAllValuesFrom(:z xsd:string))
         SubClassOf(:E DataSomeValuesFrom(:e xsd:int))",
    );
    let mut ax2 = added.clone();
    ax2.extend(ax1.iter().cloned());
    let refs2: Vec<&AxiomKind> = ax2.iter().collect();
    let add_refs: Vec<&AxiomKind> = added.iter().collect();
    let second = sat
        .classify(
            &refs2,
            Some(Changes {
                added: &add_refs,
                removed: &[],
            }),
            &limits,
        )
        .unwrap();
    assert!(second.stats.incremental);
    let new_dt = sat.index.as_ref().unwrap().dt.as_ref().unwrap();
    let new_b_comp = new_dt
        .comps
        .component(&DataProperty::new("urn:dt:b0"))
        .expect("component :b0 after edit");
    assert_ne!(old_b_comp, new_b_comp);
    assert_eq!(
        new_dt.cache_entries_for(new_b_comp),
        0,
        "kept BOUND component must not be cleared and re-evaluated"
    );
    let kept_b = sat.ctx[b_ex as usize].get().unwrap().st.lock();
    assert!(kept_b.init);
    let new_dt_st = kept_b
        .extra
        .as_ref()
        .unwrap()
        .dt
        .iter()
        .find(|s| s.comp == new_b_comp)
        .expect("kept context :B must be remapped to new component id");
    assert_eq!(new_dt_st.last, dt_sat::BOUND);
    drop(kept_b);
    assert!(!sat.excluded().is_empty());

    let mut fresh = Saturation::new();
    let want = fresh.classify(&refs2, None, &limits).unwrap();
    assert_eq!(second.taxonomy, want.taxonomy);
    assert_eq!(sat.excluded(), fresh.excluded());
}

#[test]
fn dtix_align_ignores_reordered_unchanged_body_axioms() {
    let u_body1 = axioms("SubClassOf(DataSomeValuesFrom(:u xsd:integer) :B1)");
    let u_body2 = axioms("SubClassOf(DataSomeValuesFrom(:u xsd:string) :B2)");
    let rest = axioms(
        "SubClassOf(:U DataSomeValuesFrom(:u DataUnionOf(xsd:integer xsd:string)))
         SubClassOf(:E DataAllValuesFrom(:e xsd:integer))
         SubClassOf(:E DataSomeValuesFrom(:e xsd:integer))",
    );
    let mut ax1 = Vec::new();
    ax1.extend(u_body1.iter().cloned());
    ax1.extend(u_body2.iter().cloned());
    ax1.extend(rest.iter().cloned());
    let refs1: Vec<&AxiomKind> = ax1.iter().collect();
    let limits = Limits {
        threads: 1,
        ..Limits::default()
    };
    let mut sat = Saturation::new();
    let first = sat.classify(&refs1, None, &limits).unwrap();
    assert!(!first.stats.incremental);

    // Reverse the two unchanged body axioms of :u while editing unrelated component :e.
    let added = axioms("SubClassOf(:E DataSomeValuesFrom(:e xsd:int))");
    let mut ax2 = Vec::new();
    ax2.extend(u_body2.iter().cloned());
    ax2.extend(u_body1.iter().cloned());
    ax2.extend(rest.iter().cloned());
    ax2.extend(added.iter().cloned());
    let refs2: Vec<&AxiomKind> = ax2.iter().collect();
    let add_refs: Vec<&AxiomKind> = added.iter().collect();
    let second = sat
        .classify(
            &refs2,
            Some(Changes {
                added: &add_refs,
                removed: &[],
            }),
            &limits,
        )
        .unwrap();
    assert!(second.stats.incremental);
    let new_dt = sat.index.as_ref().unwrap().dt.as_ref().unwrap();
    let new_u_comp = new_dt
        .comps
        .component(&DataProperty::new("urn:dt:u"))
        .expect("component :u");
    assert_eq!(
        new_dt.cache_entries_for(new_u_comp),
        0,
        "reordering unchanged body axioms of :u must not invalidate component :u"
    );

    let mut fresh = Saturation::new();
    let want = fresh.classify(&refs2, None, &limits).unwrap();
    assert_eq!(second.taxonomy, want.taxonomy);
    assert_eq!(sat.excluded(), fresh.excluded());
}

#[test]
fn push_bounded_never_exceeds_cap_limit() {
    for cap_limit in [1usize, 7, 8, 9, 4096, 4097, 4100, 8193] {
        let mut q = Vec::new();
        for i in 0..cap_limit as index::Ex {
            push_bounded(&mut q, i, cap_limit);
            assert!(
                q.capacity() <= cap_limit,
                "cap {} exceeded cap_limit {cap_limit} at len {}",
                q.capacity(),
                q.len()
            );
        }
        assert_eq!(q.len(), cap_limit);
    }
}

#[test]
fn neg_data_897_role_deletion_fits_changed_scratch_without_rehash() {
    let n = 897usize;
    let mut doc = String::with_capacity(n * 64 + 64);
    doc.push_str("SubClassOf(:A :B)\n");
    for i in 0..n {
        doc.push_str(&format!(
            "SubClassOf(DataSomeValuesFrom(:d{i} xsd:integer) :Q)\n"
        ));
    }
    let ax1 = axioms(&doc);
    let refs1: Vec<&AxiomKind> = ax1.iter().collect();
    let limits = Limits {
        threads: 1,
        ..Limits::default()
    };
    let mut sat_old = Saturation::new();
    sat_old.classify(&refs1, None, &limits).unwrap();
    let old_ix = sat_old.index.as_ref().unwrap();
    assert_eq!(old_ix.neg_data.len(), n);

    let mut sat = Saturation::new();
    let first = sat.classify(&refs1, None, &limits).unwrap();
    assert!(!first.stats.incremental);

    let kept = axioms("SubClassOf(:A :B)");
    let kept_refs: Vec<&AxiomKind> = kept.iter().collect();
    let removed_refs: Vec<&AxiomKind> = ax1[1..].iter().collect();
    index::reset_neg_role_set_stats();
    let second = sat
        .classify(
            &kept_refs,
            Some(Changes {
                added: &[],
                removed: &removed_refs,
            }),
            &limits,
        )
        .unwrap();
    assert!(second.stats.incremental);
    let (seen_init, seen_final, changed_init, changed_final) = index::neg_role_set_stats()
        .expect("changed_with_dt must record negative-role set capacities");
    assert!(
        seen_init >= n && seen_final == seen_init,
        "seen_roles must be pre-sized without rehash: init={seen_init}, final={seen_final}, n={n}"
    );
    assert!(
        changed_init >= n && changed_final == changed_init,
        "changed_neg_roles must be pre-sized without rehash: init={changed_init}, final={changed_final}, n={n}"
    );
    let new_ix = sat.index.as_ref().unwrap();
    assert!(new_ix.neg_data.is_empty());

    // Both `seen_roles` and `changed_neg_roles` are pre-sized to `max_roles = 897` with
    // zero rehashes; their combined bucket backing fits strictly inside `changed_scratch`.
    let buckets = dtcheck::range::map_buckets_for(n);
    let two_tables = buckets * (std::mem::size_of::<index::Role>() + 1) * 2 + 32;
    let scratch = new_ix.changed_scratch(old_ix);
    assert!(
        scratch >= old_ix.head_count() + two_tables,
        "changed_scratch {scratch} must cover expression marks + two pre-sized {n}-role sets ({two_tables})"
    );
    let mut fresh = Saturation::new();
    let fresh_out = fresh.classify(&kept_refs, None, &limits).unwrap();
    assert_eq!(second.taxonomy, fresh_out.taxonomy);
}

#[test]
fn same_tables_with_stop_polls_dense_role_closure_and_data_tables() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // 1. Pure EL ontology (no checker) with 100 roles in a linear chain -> 5,050 `sup`
    //    entries (> POLL_CHANGED = 4096) with only ~100 roles, so per-role ticks alone
    //    cannot trigger a poll without inner `sup` slice polling.
    let mut chain = String::new();
    for i in 0..99 {
        chain.push_str(&format!("SubObjectPropertyOf(:r{i} :r{})\n", i + 1));
    }
    chain.push_str("SubClassOf(:A :B)\n");
    let ax = axioms(&chain);
    let refs: Vec<&AxiomKind> = ax.iter().collect();
    let limits = Limits {
        threads: 1,
        ..Limits::default()
    };
    let mut sat = Saturation::new();
    sat.classify(&refs, None, &limits).unwrap();
    let ix = sat.index.as_ref().unwrap();
    assert!(ix.dt.is_none());
    let total_sup: usize = ix.roles.sup.iter().map(Vec::len).sum();
    assert!(
        total_sup > index::POLL_CHANGED,
        "expected dense closure > POLL_CHANGED, got {total_sup}"
    );
    let polls = AtomicUsize::new(0);
    assert_eq!(
        ix.same_tables_with_stop(ix, &|| {
            polls.fetch_add(1, Ordering::Relaxed);
            false
        }),
        Some(true)
    );
    assert!(
        polls.load(Ordering::Relaxed) >= 1,
        "same_tables_with_stop must poll inside dense role closure without a checker"
    );
    assert_eq!(
        ix.same_tables_with_stop(ix, &|| true),
        None,
        "same_tables_with_stop must stop on first poll"
    );

    // 2a. Isolate `data_range` polling by placing a controlled mismatch immediately after
    //     `data_range` at `functional[0]`. Both indexes have `any_functional = true` so
    //     `plain` matches and `same_tables_with_stop` traverses `sup`, `right`, `left`,
    //     `range`, and `data_range` before hitting `functional[0]`.
    let n = 4200usize;
    let mut base_dr = String::with_capacity(n * 48 + 64);
    for i in 0..n {
        base_dr.push_str(&format!("DataPropertyRange(:d{i} xsd:integer)\n"));
    }
    let ax_dr_a = axioms(&format!("{base_dr}FunctionalDataProperty(:d0)\n"));
    let ax_dr_b = axioms(&format!("{base_dr}FunctionalDataProperty(:d1)\n"));
    let refs_dr_a: Vec<&AxiomKind> = ax_dr_a.iter().collect();
    let refs_dr_b: Vec<&AxiomKind> = ax_dr_b.iter().collect();
    let mut sat_dr_a = Saturation::new();
    let mut sat_dr_b = Saturation::new();
    sat_dr_a.classify(&refs_dr_a, None, &limits).unwrap();
    sat_dr_b.classify(&refs_dr_b, None, &limits).unwrap();
    let ix_dr_a = sat_dr_a.index.as_ref().unwrap();
    let ix_dr_b = sat_dr_b.index.as_ref().unwrap();
    assert_eq!(ix_dr_a.roles.sup.len(), n);
    assert_eq!(ix_dr_b.roles.sup.len(), n);
    assert!(ix_dr_a.roles.any_functional && ix_dr_b.roles.any_functional);
    // Exact ticks before `data_range`: `sup` (2*n) + `right` (n) + `left` (n) + `range` (n) = 5*n.
    let pre_dr_polls = (5 * n) / index::POLL_CHANGED;
    let dr_polls = AtomicUsize::new(0);
    assert_eq!(
        ix_dr_b.same_tables_with_stop(ix_dr_a, &|| {
            dr_polls.fetch_add(1, Ordering::Relaxed);
            false
        }),
        Some(false)
    );
    assert!(
        dr_polls.load(Ordering::Relaxed) > pre_dr_polls,
        "data_range loop must trigger a poll beyond pre-data_range polls ({pre_dr_polls}), got {}",
        dr_polls.load(Ordering::Relaxed)
    );
    let cancel_dr = AtomicUsize::new(0);
    assert_eq!(
        ix_dr_b.same_tables_with_stop(ix_dr_a, &|| {
            cancel_dr.fetch_add(1, Ordering::Relaxed) == pre_dr_polls
        }),
        None,
        "cancellation inside data_range must fire before reaching functional[0] mismatch"
    );

    // 2b. Isolate `functional` (outer row ticks + inner `slice_eq_poll`) by placing a
    //     controlled mismatch after `functional` at `selfs[0]`.
    let mut base_fn = String::with_capacity(n * 80 + 128);
    base_fn.push_str(
        "SubClassOf(ObjectSomeValuesFrom(:r0 :A) :A)\n\
         SubClassOf(ObjectSomeValuesFrom(:r1 :A) :A)\n",
    );
    for i in 0..n {
        base_fn.push_str(&format!(
            "DataPropertyRange(:d{i} xsd:integer)\nFunctionalDataProperty(:d{i})\n"
        ));
    }
    let ax_fn_a = axioms(&format!("{base_fn}SubClassOf(ObjectHasSelf(:r0) :Q)\n"));
    let ax_fn_b = axioms(&format!("{base_fn}SubClassOf(ObjectHasSelf(:r1) :Q)\n"));
    let refs_fn_a: Vec<&AxiomKind> = ax_fn_a.iter().collect();
    let refs_fn_b: Vec<&AxiomKind> = ax_fn_b.iter().collect();
    let mut sat_fn_a = Saturation::new();
    let mut sat_fn_b = Saturation::new();
    sat_fn_a.classify(&refs_fn_a, None, &limits).unwrap();
    sat_fn_b.classify(&refs_fn_b, None, &limits).unwrap();
    let ix_fn_a = sat_fn_a.index.as_ref().unwrap();
    let ix_fn_b = sat_fn_b.index.as_ref().unwrap();
    let roles_fn = ix_fn_a.roles.sup.len();
    assert_eq!(roles_fn, n + 2);
    assert_eq!(ix_fn_b.roles.sup.len(), roles_fn);
    assert_eq!(ix_fn_a.selfs.len(), 1);
    assert_eq!(ix_fn_b.selfs.len(), 1);
    assert_ne!(ix_fn_a.selfs, ix_fn_b.selfs);
    // Without inner `functional` slice ticks, `sup` (2R) + `right`/`left`/`range`/`data_range`
    // (4R) + `functional` outer (R) + `empty` (R) = 8*R ticks; inner `functional` adds `n` ticks.
    let without_inner_func_polls = (8 * roles_fn) / index::POLL_CHANGED;
    let fn_polls = AtomicUsize::new(0);
    assert_eq!(
        ix_fn_b.same_tables_with_stop(ix_fn_a, &|| {
            fn_polls.fetch_add(1, Ordering::Relaxed);
            false
        }),
        Some(false)
    );
    assert!(
        fn_polls.load(Ordering::Relaxed) > without_inner_func_polls,
        "functional outer+inner polling must exceed non-inner poll bound ({without_inner_func_polls}), got {}",
        fn_polls.load(Ordering::Relaxed)
    );
    let cancel_fn = AtomicUsize::new(0);
    assert_eq!(
        ix_fn_b.same_tables_with_stop(ix_fn_a, &|| {
            cancel_fn.fetch_add(1, Ordering::Relaxed) == without_inner_func_polls
        }),
        None,
        "cancellation inside functional/empty must fire before reaching selfs mismatch"
    );

    // 2c. Cached `Roles::any_functional` makes `saturate::plain` O(1) and rejects a newly
    //     appended functional data role in 0 polls.
    let ax_plain = axioms(&base_dr);
    let refs_plain: Vec<&AxiomKind> = ax_plain.iter().collect();
    let mut sat_plain = Saturation::new();
    sat_plain.classify(&refs_plain, None, &limits).unwrap();
    let ix_plain = sat_plain.index.as_ref().unwrap();
    assert!(!ix_plain.roles.any_functional);
    let ax_appended = axioms(&format!("{base_dr}FunctionalDataProperty(:d_new)\n"));
    let refs_appended: Vec<&AxiomKind> = ax_appended.iter().collect();
    let mut sat_appended = Saturation::new();
    sat_appended
        .classify(&refs_appended, None, &limits)
        .unwrap();
    let ix_appended = sat_appended.index.as_ref().unwrap();
    assert!(ix_appended.roles.any_functional);
    let plain_polls = AtomicUsize::new(0);
    assert_eq!(
        ix_appended.same_tables_with_stop(ix_plain, &|| {
            plain_polls.fetch_add(1, Ordering::Relaxed);
            false
        }),
        Some(false)
    );
    assert_eq!(
        plain_polls.load(Ordering::Relaxed),
        0,
        "plain-mode mismatch from newly appended functional role must reject in O(1) before scanning"
    );
}

#[test]
fn dtix_align_polls_inside_large_body_told_slice() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Single checker property with 1 body atom whose `told` slice has 4,200 named
    // subsumers (> POLL_ALIGN = 4096). Without inner polling on `a.told == b.told`,
    // `DtIx::align` would only poll once at its final stop check.
    let n = 4200usize;
    let mut doc = String::with_capacity(n * 64 + 64);
    doc.push_str("SubClassOf(:A DataAllValuesFrom(:p xsd:integer))\n");
    for i in 0..n {
        doc.push_str(&format!(
            "SubClassOf(DataSomeValuesFrom(:p xsd:integer) :T{i})\n"
        ));
    }
    let ax = axioms(&doc);
    let refs: Vec<&AxiomKind> = ax.iter().collect();
    let mut sat = Saturation::new();
    sat.classify(
        &refs,
        None,
        &Limits {
            threads: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    let ix = sat.index.as_ref().unwrap();
    let dt = ix.dt.as_ref().unwrap();
    assert_eq!(dt.roles.len(), 1);
    assert_eq!(dt.body.len(), 1);
    assert_eq!(dt.body[0].len(), 1);
    assert_eq!(
        dt.body[0][0].told.as_deref().map(<[index::Ex]>::len),
        Some(n)
    );

    let polls = AtomicUsize::new(0);
    let outcome = dt.align(dt, ix.roles.sup.len(), &|| {
        polls.fetch_add(1, Ordering::Relaxed);
        false
    });
    assert!(matches!(outcome, dt_sat::DtAlignOutcome::Aligned(_)));
    assert!(
        polls.load(Ordering::Relaxed) >= 2,
        "DtIx::align must poll inside large body `told` slice before final stop check, got {}",
        polls.load(Ordering::Relaxed)
    );
    let stop_polls = AtomicUsize::new(0);
    let stopped = dt.align(dt, ix.roles.sup.len(), &|| {
        stop_polls.fetch_add(1, Ordering::Relaxed) == 0
    });
    assert!(matches!(stopped, dt_sat::DtAlignOutcome::Stopped));
    assert_eq!(stop_polls.load(Ordering::Relaxed), 1);
}

#[test]
fn high_fanout_refire_and_modes_compatible_poll_within_bound() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // 4,200 independent checker properties all telling two shared classes :Shared0 and
    // :Shared1 creates 8,400 refire membership entries (> 2 * POLL_CHANGED) and 4,200
    // checker roles (> POLL_ALIGN).
    let n = 4200usize;
    let mut doc = String::with_capacity(n * 170);
    for i in 0..n {
        doc.push_str(&format!(
            "SubClassOf(:A{i} DataAllValuesFrom(:p{i} xsd:integer))\n\
             SubClassOf(DataSomeValuesFrom(:p{i} xsd:integer) :Shared0)\n\
             SubClassOf(DataSomeValuesFrom(:p{i} xsd:integer) :Shared1)\n"
        ));
    }
    let ax1 = axioms(&doc);
    let refs1: Vec<&AxiomKind> = ax1.iter().collect();
    let limits = Limits {
        threads: 1,
        ..Limits::default()
    };
    let mut sat = Saturation::new();
    let first = sat.classify(&refs1, None, &limits).unwrap();
    assert!(!first.stats.incremental);

    let old_ix = sat.index.as_ref().unwrap();
    let old_dt = old_ix.dt.as_ref().unwrap();
    let shared0 = sat
        .interner
        .get(&index::Node::Class(
            sat.interner
                .classes
                .iter()
                .position(|c| c.as_str() == "urn:dt:Shared0")
                .unwrap() as u32,
        ))
        .unwrap();
    assert_eq!(
        old_dt.refire_of(shared0).map(<[u32]>::len),
        Some(n),
        "expected a high-fanout DtFire target with {n} components"
    );

    // 1. Direct modes_compatible_with_stop polling check.
    let polls = AtomicUsize::new(0);
    let res = old_dt.modes_compatible_with_stop(old_dt, old_ix.roles.sup.len(), &|| {
        polls.fetch_add(1, Ordering::Relaxed) >= 1
    });
    assert_eq!(
        res, None,
        "modes_compatible_with_stop must stop on cooperative poll"
    );
    assert!(polls.load(Ordering::Relaxed) >= 2);

    // 2. Differential poll count isolating the inner refire membership loop:
    //    `without_dt_polls` covers the per-expression scan + final stop; the extra polls
    //    with `Some(&aligned)` must strictly exceed the outer `for e in 0..n` tick bound
    //    `heads.len() / POLL_CHANGED`.
    let aligned = match old_dt.align(old_dt, old_ix.roles.sup.len(), &|| false) {
        dt_sat::DtAlignOutcome::Aligned(a) => a,
        _ => panic!("self-alignment must succeed"),
    };
    let without_dt = AtomicUsize::new(0);
    assert!(
        old_ix
            .changed_with_dt(old_ix, None, &|| {
                without_dt.fetch_add(1, Ordering::Relaxed);
                false
            })
            .is_some()
    );
    let with_dt = AtomicUsize::new(0);
    assert!(
        old_ix
            .changed_with_dt(old_ix, Some(&aligned), &|| {
                with_dt.fetch_add(1, Ordering::Relaxed);
                false
            })
            .is_some()
    );
    let base_polls = without_dt.load(Ordering::Relaxed);
    let total_polls = with_dt.load(Ordering::Relaxed);
    let refire_polls = total_polls - base_polls;
    let outer_only_max = old_ix.head_count() / index::POLL_CHANGED;
    assert!(
        refire_polls > outer_only_max,
        "inner refire scan must add polls beyond outer expression ticks: refire_polls={refire_polls}, outer_only_max={outer_only_max}"
    );
    // Cancelling strictly before the final `if stop()` must abort inside the refire scan.
    let cancel_at = base_polls + outer_only_max;
    assert!(cancel_at < total_polls - 1);
    let cpolls = AtomicUsize::new(0);
    let stopped = old_ix.changed_with_dt(old_ix, Some(&aligned), &|| {
        cpolls.fetch_add(1, Ordering::Relaxed) >= cancel_at
    });
    assert!(
        stopped.is_none(),
        "changed_with_dt must abort inside inner refire loop"
    );
    assert_eq!(cpolls.load(Ordering::Relaxed), cancel_at + 1);

    // 3. Single-role high-fanout `neg_data` comparison must poll inside the per-role scan.
    //    Use pure EL `DataHasValue` literals so `:d` stays on the EL data lattice (`neg_data`).
    let mut neg_doc = String::with_capacity(n * 64);
    for i in 0..n {
        neg_doc.push_str(&format!(
            "SubClassOf(DataHasValue(:d \"v{i}\"^^xsd:string) :C)\n"
        ));
    }
    let neg_ax = axioms(&neg_doc);
    let neg_refs: Vec<&AxiomKind> = neg_ax.iter().collect();
    let mut neg_sat = Saturation::new();
    neg_sat.classify(&neg_refs, None, &limits).unwrap();
    let neg_ix = neg_sat.index.as_ref().unwrap();
    assert_eq!(neg_ix.neg_data.len(), 1);
    assert_eq!(neg_ix.neg_data.values().next().map(Vec::len), Some(n));
    let neg_polls = AtomicUsize::new(0);
    assert!(
        neg_ix
            .changed_with_dt(neg_ix, None, &|| {
                neg_polls.fetch_add(1, Ordering::Relaxed);
                false
            })
            .is_some()
    );
    // Expression loop polls `1 + (heads.len() - 1) / POLL_CHANGED` times, plus 1 final
    // stop check; the inner single-role `neg_data` scan must add at least 1 poll.
    let expr_and_final = 1 + (neg_ix.head_count() - 1) / index::POLL_CHANGED + 1;
    assert!(
        neg_polls.load(Ordering::Relaxed) > expr_and_final,
        "expected inner neg_data poll beyond expression+final ({expr_and_final}), got {}",
        neg_polls.load(Ordering::Relaxed)
    );
}

#[test]
fn high_fanout_invalidation_and_orphan_prune_poll_within_bound() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // 1. Direct `invalidate_changed` + `orphan_prune` predecessor-cleanup test:
    //    200 sources x 50 object roles all link into one `:Hub` -> 10,000 incoming links
    //    in `:Hub`'s `st.back` while `total_slots` is only ~253 (`Exist(:r{j}, :Hub)` is
    //    shared across all 200 sources).
    let sources = 200usize;
    let roles = 50usize;
    let mut doc = String::with_capacity(sources * roles * 56 + 128);
    doc.push_str(
        "SubClassOf(:Hub DataAllValuesFrom(:d xsd:integer))\n\
         SubClassOf(:Hub DataSomeValuesFrom(:d xsd:integer))\n",
    );
    for i in 0..sources {
        for j in 0..roles {
            doc.push_str(&format!(
                "SubClassOf(:S{i} ObjectSomeValuesFrom(:r{j} :Hub))\n"
            ));
        }
    }
    let ax = axioms(&doc);
    let refs: Vec<&AxiomKind> = ax.iter().collect();
    let base_limits = Limits {
        threads: 1,
        ..Limits::default()
    };

    let build_hub_sat = || {
        let mut sat = Saturation::new();
        let out = sat.classify(&refs, None, &base_limits).unwrap();
        assert!(!out.stats.incremental);
        let hub = sat
            .interner
            .get(&index::Node::Class(
                sat.interner
                    .classes
                    .iter()
                    .position(|c| c.as_str() == "urn:dt:Hub")
                    .unwrap() as u32,
            ))
            .unwrap();
        let s0 = sat
            .interner
            .get(&index::Node::Class(
                sat.interner
                    .classes
                    .iter()
                    .position(|c| c.as_str() == "urn:dt:S0")
                    .unwrap() as u32,
            ))
            .unwrap();
        (sat, hub, s0)
    };

    // 1a. `invalidate_changed` with ONLY `:Hub` changed -> BFS walks `:Hub`'s 10,000
    //     `st.back` edges. Without inner edge polling, total ticks stay under 3 * POLL.
    let (sat_ref, hub, s0) = build_hub_sat();
    let ix = sat_ref.index.as_ref().unwrap();
    let roots = sat_ref.last_roots.clone();
    let (mut sat_bfs, _, _) = build_hub_sat();
    let total_slots = sat_bfs.ctx.len();
    assert!(total_slots < 500, "compact slot count, got {total_slots}");
    let mut changed_hub = vec![false; ix.head_count()];
    changed_hub[hub as usize] = true;
    let bfs_polls = Arc::new(AtomicUsize::new(0));
    let p = bfs_polls.clone();
    let counting = Limits {
        cancelled: Some(Arc::new(move || {
            p.fetch_add(1, Ordering::Relaxed);
            false
        })),
        ..base_limits.clone()
    };
    let mut peak = 0usize;
    let cleared = sat_bfs
        .invalidate_changed(
            &changed_hub,
            ix,
            None,
            None,
            &roots,
            &roots,
            false,
            &counting,
            0,
            &mut peak,
        )
        .unwrap();
    assert!(cleared > sources);
    let bfs_total = bfs_polls.load(Ordering::Relaxed);
    assert!(
        bfs_total >= 4,
        "invalidate_changed BFS must poll inside :Hub's 10,000 incoming edges, got {bfs_total}"
    );
    // Cancel inside the BFS edge walk (last poll) and verify Error::Cancelled.
    let (mut sat_bfs_cancel, _, _) = build_hub_sat();
    let seen = Arc::new(AtomicUsize::new(0));
    let s = seen.clone();
    let cancel_at = bfs_total - 1;
    let cancel_lim = Limits {
        cancelled: Some(Arc::new(move || {
            s.fetch_add(1, Ordering::Relaxed) >= cancel_at
        })),
        ..base_limits.clone()
    };
    let mut peak = 0usize;
    let err = sat_bfs_cancel.invalidate_changed(
        &changed_hub,
        ix,
        None,
        None,
        &roots,
        &roots,
        false,
        &cancel_lim,
        0,
        &mut peak,
    );
    assert!(matches!(err, Err(Error::Cancelled)));

    // 1b. `invalidate_changed` with ONLY `:S0` changed -> `:Hub` stays initialized and
    //     calls `drop_predecessors_where_poll` scanning all 10,000 incoming links.
    let (mut sat_clean, _, _) = build_hub_sat();
    let mut changed_s0 = vec![false; ix.head_count()];
    changed_s0[s0 as usize] = true;
    let clean_polls = Arc::new(AtomicUsize::new(0));
    let p = clean_polls.clone();
    let counting = Limits {
        cancelled: Some(Arc::new(move || {
            p.fetch_add(1, Ordering::Relaxed);
            false
        })),
        ..base_limits.clone()
    };
    let mut peak = 0usize;
    let cleared = sat_clean
        .invalidate_changed(
            &changed_s0,
            ix,
            None,
            None,
            &roots,
            &roots,
            false,
            &counting,
            0,
            &mut peak,
        )
        .unwrap();
    assert_eq!(cleared, 1);
    let clean_total = clean_polls.load(Ordering::Relaxed);
    // Initial scan over ~253 contexts + 10,000 subs contributes 2 polls; without inner
    // polling in `drop_predecessors_where_poll`, cleanup adds only 253 ticks (0 polls).
    assert!(
        clean_total >= 4,
        "invalidate_changed cleanup must poll inside drop_predecessors_where_poll, got {clean_total}"
    );
    let (mut sat_clean_cancel, _, _) = build_hub_sat();
    let seen = Arc::new(AtomicUsize::new(0));
    let s = seen.clone();
    let cancel_at = clean_total - 1;
    let cancel_lim = Limits {
        cancelled: Some(Arc::new(move || {
            s.fetch_add(1, Ordering::Relaxed) >= cancel_at
        })),
        ..base_limits.clone()
    };
    let mut peak = 0usize;
    let err = sat_clean_cancel.invalidate_changed(
        &changed_s0,
        ix,
        None,
        None,
        &roots,
        &roots,
        false,
        &cancel_lim,
        0,
        &mut peak,
    );
    assert!(matches!(err, Err(Error::Cancelled)));

    // 1c. Direct `orphan_prune` predecessor-cleanup differential test on `:Hub`:
    //     omitting `:S0` from `roots` prunes `:S0` and forces `:Hub` to run
    //     `drop_predecessors_where_poll` over 10,000 incoming links, adding >= 2 polls
    //     compared to keeping all roots (where cleanup is skipped).
    let (sat_no_prune, _, _) = build_hub_sat();
    let no_prune_polls = Arc::new(AtomicUsize::new(0));
    let p = no_prune_polls.clone();
    let counting = Limits {
        cancelled: Some(Arc::new(move || {
            p.fetch_add(1, Ordering::Relaxed);
            false
        })),
        ..base_limits.clone()
    };
    let mut totals = saturate::Totals::default();
    sat_no_prune
        .orphan_prune(ix, None, &roots, &counting, None, None, 0, &mut totals)
        .unwrap();
    let unpruned_total = no_prune_polls.load(Ordering::Relaxed);

    let roots_without_s0: Vec<index::Ex> = roots.iter().copied().filter(|&r| r != s0).collect();
    let (sat_prune, _, _) = build_hub_sat();
    let prune_polls = Arc::new(AtomicUsize::new(0));
    let p = prune_polls.clone();
    let counting = Limits {
        cancelled: Some(Arc::new(move || {
            p.fetch_add(1, Ordering::Relaxed);
            false
        })),
        ..base_limits.clone()
    };
    let mut totals = saturate::Totals::default();
    sat_prune
        .orphan_prune(
            ix,
            None,
            &roots_without_s0,
            &counting,
            None,
            None,
            0,
            &mut totals,
        )
        .unwrap();
    let pruned_total = prune_polls.load(Ordering::Relaxed);
    assert!(
        pruned_total >= unpruned_total + 2,
        "orphan_prune cleanup must poll inside drop_predecessors_where_poll: pruned={pruned_total}, unpruned={unpruned_total}"
    );
    let (sat_prune_cancel, _, _) = build_hub_sat();
    let seen = Arc::new(AtomicUsize::new(0));
    let s = seen.clone();
    let cancel_at = unpruned_total + 1;
    let cancel_lim = Limits {
        cancelled: Some(Arc::new(move || {
            s.fetch_add(1, Ordering::Relaxed) >= cancel_at
        })),
        ..base_limits.clone()
    };
    let mut totals = saturate::Totals::default();
    let err = sat_prune_cancel.orphan_prune(
        ix,
        None,
        &roots_without_s0,
        &cancel_lim,
        None,
        None,
        0,
        &mut totals,
    );
    assert!(matches!(err, Err(Error::Cancelled)));

    // 2. Direct `orphan_prune` wide outgoing traversal test:
    //    `:Root` has 5,000 outgoing `Exist` links (50 roles x 100 targets) and chains so
    //    both `st.subs` and `st.fwd.list` hold 5,000 entries. Without inner outgoing-edge
    //    polls, BFS + prune over `total_slots ~ 5,105` would poll only once.
    let targets = 100usize;
    let out_roles = 50usize;
    let mut out_doc = String::with_capacity(targets * out_roles * 64 + 2048);
    for j in 0..out_roles {
        out_doc.push_str(&format!(
            "SubObjectPropertyOf(ObjectPropertyChain(:p{j} :q) :u)\n"
        ));
    }
    for j in 0..out_roles {
        for i in 0..targets {
            out_doc.push_str(&format!(
                "SubClassOf(:Root ObjectSomeValuesFrom(:p{j} :T{i}))\n"
            ));
        }
    }
    let out_ax = axioms(&out_doc);
    let out_refs: Vec<&AxiomKind> = out_ax.iter().collect();
    let mut out_sat = Saturation::new();
    out_sat.classify(&out_refs, None, &base_limits).unwrap();
    let out_ix = out_sat.index.as_ref().unwrap();
    let root_ex = out_sat
        .interner
        .get(&index::Node::Class(
            out_sat
                .interner
                .classes
                .iter()
                .position(|c| c.as_str() == "urn:dt:Root")
                .unwrap() as u32,
        ))
        .unwrap();
    let out_polls = Arc::new(AtomicUsize::new(0));
    let p = out_polls.clone();
    let counting = Limits {
        cancelled: Some(Arc::new(move || {
            p.fetch_add(1, Ordering::Relaxed);
            false
        })),
        ..base_limits.clone()
    };
    let mut totals = saturate::Totals::default();
    out_sat
        .orphan_prune(
            out_ix,
            None,
            &[root_ex],
            &counting,
            None,
            None,
            0,
            &mut totals,
        )
        .unwrap();
    let out_total = out_polls.load(Ordering::Relaxed);
    assert!(
        out_total >= 3,
        "orphan_prune must poll across :Root's 5,000 outgoing subs/fwd edges, got {out_total}"
    );
    let seen = Arc::new(AtomicUsize::new(0));
    let s = seen.clone();
    let cancel_lim = Limits {
        cancelled: Some(Arc::new(move || s.fetch_add(1, Ordering::Relaxed) >= 1)),
        ..base_limits.clone()
    };
    let mut totals = saturate::Totals::default();
    let err = out_sat.orphan_prune(
        out_ix,
        None,
        &[root_ex],
        &cancel_lim,
        None,
        None,
        0,
        &mut totals,
    );
    assert!(matches!(err, Err(Error::Cancelled)));
}

#[test]
fn changed_with_dt_polls_skipped_new_neg_data_roles() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Adding 5,000 distinct `SubClassOf(DataSomeValuesFrom(:d_i xsd:integer) :Q)` axioms
    // on brand-new data roles to a tiny kept EL ontology produces 5,000 `self.neg_data`
    // keys that all satisfy `r >= old_roles`. The negative-role loop must tick before the
    // filter so cooperative stop polling still fires across the skipped keys.
    let base = axioms("SubClassOf(:A :B)");
    let base_refs: Vec<&AxiomKind> = base.iter().collect();
    let limits = Limits {
        threads: 1,
        ..Limits::default()
    };
    let mut sat_old = Saturation::new();
    sat_old.classify(&base_refs, None, &limits).unwrap();
    let old_ix = sat_old.index.as_ref().unwrap();
    assert!(old_ix.neg_data.is_empty());
    assert_eq!(old_ix.roles.sup.len(), 0);

    let n = 5000usize;
    let mut added_doc = String::with_capacity(n * 64 + 32);
    added_doc.push_str("SubClassOf(:A :B)\n");
    for i in 0..n {
        added_doc.push_str(&format!(
            "SubClassOf(DataSomeValuesFrom(:d{i} xsd:integer) :Q)\n"
        ));
    }
    let full = axioms(&added_doc);
    let full_refs: Vec<&AxiomKind> = full.iter().collect();
    let added_refs: Vec<&AxiomKind> = full[1..].iter().collect();
    let mut sat = Saturation::new();
    sat.classify(&base_refs, None, &limits).unwrap();
    let inc = sat
        .classify(
            &full_refs,
            Some(Changes {
                added: &added_refs,
                removed: &[],
            }),
            &limits,
        )
        .unwrap();
    assert!(inc.stats.incremental);
    let new_ix = sat.index.as_ref().unwrap();
    assert_eq!(new_ix.neg_data.len(), n);
    assert!(
        new_ix
            .neg_data
            .keys()
            .all(|&r| (r as usize) >= old_ix.roles.sup.len())
    );

    let polls = AtomicUsize::new(0);
    assert!(
        new_ix
            .changed_with_dt(old_ix, None, &|| {
                polls.fetch_add(1, Ordering::Relaxed);
                false
            })
            .is_some()
    );
    // Expression loop polls once (old_ix has < POLL_CHANGED heads) + 1 final stop check.
    let expr_and_final = 1 + (old_ix.head_count() - 1) / index::POLL_CHANGED + 1;
    assert!(
        polls.load(Ordering::Relaxed) >= expr_and_final + n / index::POLL_CHANGED,
        "skipped new neg_data keys must still poll: got {}, expected >= {}",
        polls.load(Ordering::Relaxed),
        expr_and_final + n / index::POLL_CHANGED
    );

    let stop_polls = AtomicUsize::new(0);
    assert!(
        new_ix
            .changed_with_dt(old_ix, None, &|| {
                stop_polls.fetch_add(1, Ordering::Relaxed) == 0
            })
            .is_none()
    );
    assert_eq!(stop_polls.load(Ordering::Relaxed), 1);
}

#[test]
fn dtix_align_polls_inside_single_large_checker_component() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Connect 9,000 subproperties to one superproperty with `DataAllValuesFrom` on the
    // superproperty so every property lands in a single checker component (> 2 * POLL_ALIGN).
    let n = 9000usize;
    let mut base_doc = String::with_capacity(n * 40 + 128);
    base_doc.push_str("SubClassOf(:A DataAllValuesFrom(:root xsd:integer))\n");
    for i in 0..n {
        base_doc.push_str(&format!("SubDataPropertyOf(:p{i} :root)\n"));
    }
    let ax_old = axioms(&base_doc);
    let refs_old: Vec<&AxiomKind> = ax_old.iter().collect();
    let limits = Limits {
        threads: 1,
        ..Limits::default()
    };

    let mut sat_old = Saturation::new();
    sat_old.classify(&refs_old, None, &limits).unwrap();
    let old_ix = sat_old.index.as_ref().unwrap();
    let old_dt = old_ix.dt.as_ref().unwrap();
    let old_roles_len = old_ix.roles.sup.len();
    assert_eq!(old_dt.body.len(), 1);
    assert_eq!(old_dt.roles.len(), n + 1);

    // Self-alignment must succeed and keep component 0 unchanged.
    match old_dt.align(old_dt, old_roles_len, &|| false) {
        dt_sat::DtAlignOutcome::Aligned(a) => {
            assert_eq!(a.comp_map, vec![Some(0)]);
            assert_eq!(a.changed_comps, vec![false]);
        }
        _ => panic!("self-alignment of single large component must succeed"),
    }

    // Variant A: add 1 subproperty -> cardinality mismatch skips the role-set membership walk.
    let extra_role = axioms("SubDataPropertyOf(:p_extra :root)");
    let mut ax_a = ax_old.clone();
    ax_a.extend(extra_role.iter().cloned());
    let refs_a: Vec<&AxiomKind> = ax_a.iter().collect();
    let added_a: Vec<&AxiomKind> = extra_role.iter().collect();
    let mut sat_a = Saturation::new();
    sat_a.classify(&refs_old, None, &limits).unwrap();
    sat_a
        .classify(
            &refs_a,
            Some(Changes {
                added: &added_a,
                removed: &[],
            }),
            &limits,
        )
        .unwrap();
    let dt_a = sat_a.index.as_ref().unwrap().dt.as_ref().unwrap();
    assert_eq!(dt_a.body.len(), 1);
    assert_eq!(dt_a.roles.len(), n + 2);

    // Variant B: same 9,001 roles, add 1 body atom -> runs the membership walk over all
    // 9,001 roles and then exits on `old_body.len() != new_body.len()` before per-role checks.
    let extra_body = axioms("SubClassOf(DataSomeValuesFrom(:root xsd:integer) :Extra)");
    let mut ax_b = ax_old.clone();
    ax_b.extend(extra_body.iter().cloned());
    let refs_b: Vec<&AxiomKind> = ax_b.iter().collect();
    let added_b: Vec<&AxiomKind> = extra_body.iter().collect();
    let mut sat_b = Saturation::new();
    sat_b.classify(&refs_old, None, &limits).unwrap();
    sat_b
        .classify(
            &refs_b,
            Some(Changes {
                added: &added_b,
                removed: &[],
            }),
            &limits,
        )
        .unwrap();
    let dt_b = sat_b.index.as_ref().unwrap().dt.as_ref().unwrap();
    assert_eq!(dt_b.body.len(), 1);
    assert_eq!(dt_b.roles.len(), n + 1);
    assert_ne!(dt_b.body[0].len(), old_dt.body[0].len());

    let polls_a = AtomicUsize::new(0);
    match dt_a.align(old_dt, old_roles_len, &|| {
        polls_a.fetch_add(1, Ordering::Relaxed);
        false
    }) {
        dt_sat::DtAlignOutcome::Aligned(a) => assert_eq!(a.changed_comps, vec![true]),
        _ => panic!("expected Aligned with changed_comps=[true] for variant A"),
    }
    let polls_b = AtomicUsize::new(0);
    match dt_b.align(old_dt, old_roles_len, &|| {
        polls_b.fetch_add(1, Ordering::Relaxed);
        false
    }) {
        dt_sat::DtAlignOutcome::Aligned(a) => assert_eq!(a.changed_comps, vec![true]),
        _ => panic!("expected Aligned with changed_comps=[true] for variant B"),
    }
    let count_a = polls_a.load(Ordering::Relaxed);
    let count_b = polls_b.load(Ordering::Relaxed);
    let min_walk_polls = (n + 1) / 4096;
    assert!(
        count_b >= count_a + min_walk_polls,
        "membership walk over {} roles must add >= {min_walk_polls} polls: count_a={count_a}, count_b={count_b}",
        n + 1
    );

    // Cancelling inside the membership walk (after cardinality-skip polls, before final stop)
    // must abort with `DtAlignOutcome::Stopped` rather than completing the component.
    let cancel_at = count_a;
    assert!(cancel_at < count_b - 1);
    let cpolls = AtomicUsize::new(0);
    let stopped = dt_b.align(old_dt, old_roles_len, &|| {
        cpolls.fetch_add(1, Ordering::Relaxed) >= cancel_at
    });
    assert!(
        matches!(stopped, dt_sat::DtAlignOutcome::Stopped),
        "expected DtAlignOutcome::Stopped inside single-component membership walk"
    );
    assert_eq!(cpolls.load(Ordering::Relaxed), cancel_at + 1);
}
