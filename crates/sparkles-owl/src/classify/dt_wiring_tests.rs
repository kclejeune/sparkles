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
fn a_disjointness_of_checker_properties_is_reported() {
    // `DisjointDataProperties` makes its properties one checker component. No tier takes
    // the axiom, so the run must report it rather than drop it: `A` would be
    // unsatisfiable with it. The second body adds a disjunctive axiom, which Tier D does
    // not take beside data, so the Horn rules run and leave it out as well.
    for extra in ["", "SubClassOf(:X ObjectUnionOf(:Y :Z))"] {
        let (sat, ax) = run(&format!(
            "DisjointDataProperties(:d :e)\n\
             SubClassOf(:A DataHasValue(:d \"1\"^^xsd:integer))\n\
             SubClassOf(:A DataHasValue(:e \"1\"^^xsd:integer))\n{extra}"
        ));
        assert!(!sat.interner.dt.is_empty(), "{extra}");
        assert!(
            matches!(gap_of(&sat, &ax[0]), Some(Gap::Outside { .. })),
            "{extra}: {:?}",
            sat.excluded()
        );
    }
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
