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
