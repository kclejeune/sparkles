//! The wiring of the datatype checker into the EL and Horn tiers (F24, "The EL and Horn
//! tiers"): which data restrictions become the checker's atoms, and what a run reports
//! while the saturation does not read them.

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
    let ax = axioms(body);
    let mut sat = Saturation::new();
    let all: Vec<&AxiomKind> = ax.iter().collect();
    sat.saturate(
        &all,
        None,
        &Limits {
            threads: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    (sat, ax)
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
    let (sat, ax) = run("SubClassOf(:A DataSomeValuesFrom(:d xsd:integer))\n\
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
    for k in &ax[..7] {
        assert!(gap_of(&sat, k).is_some(), "{k:?}");
    }
    for k in &ax[..7] {
        if !matches!(
            k,
            AxiomKind::SubClassOf(_, ClassExpression::DataAllValuesFrom(..))
        ) {
            assert_eq!(
                gap_of(&sat, k),
                Some(&Gap::Partial(index::DT_UNWIRED)),
                "{k:?}"
            );
        }
    }
    // the other component stays with the lattice
    assert!(gap_of(&sat, &ax[7]).is_none());
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
