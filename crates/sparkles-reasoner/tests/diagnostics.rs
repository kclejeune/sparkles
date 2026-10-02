//! Inconsistency diagnostics over data and materialized inferences.

use serde_json::{Value as J, json};
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::QueryOptions;
use sparkles::store::{Store, StoreOptions};
use sparkles_reasoner::diagnostics::{
    Basis, CheckStatus, Closure, DiagnoseOptions, DiagnosticsReport, ReportStatus, diagnose,
};
use sparkles_reasoner::{Profile, ReasonOptions, materialize};

const PREFIXES: &str = "@prefix ex: <http://ex.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
";

fn store(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("{PREFIXES}{ttl}").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn opts() -> DiagnoseOptions {
    DiagnoseOptions {
        prefixes: vec![("ex".into(), "http://ex.org/".into())],
        ..Default::default()
    }
}

fn check(s: &Store, o: &DiagnoseOptions) -> DiagnosticsReport {
    diagnose(s.snapshot(), o).unwrap_or_else(|e| panic!("{e:#}"))
}

fn findings(r: &DiagnosticsReport, id: &str) -> Vec<J> {
    r.to_json()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["check"] == id)
        .cloned()
        .collect()
}

fn outcome(r: &DiagnosticsReport, id: &str) -> CheckStatus {
    r.checks.iter().find(|c| c.id == id).unwrap().status
}

fn uri(s: &str) -> J {
    json!({"type": "uri", "value": format!("http://ex.org/{s}")})
}

fn update(s: &Store, u: &str) {
    let text = format!("PREFIX ex: <http://ex.org/>\n{u}");
    sparkles::sparql::update::update(s, &text, &QueryOptions::default()).unwrap();
}

#[test]
fn disjoint_classes_follow_the_subclass_closure() {
    let s = store(
        "ex:Cat owl:disjointWith ex:Dog . ex:Kitten rdfs:subClassOf ex:Cat .
         ex:tom a ex:Kitten, ex:Dog .",
    );
    let r = check(&s, &opts());
    assert_eq!(r.status, ReportStatus::ViolationsFound);
    assert_eq!(r.findings.len(), 1);
    let f = &findings(&r, "disjoint-classes")[0];
    assert_eq!(f["rule"], "cax-dw");
    assert_eq!(f["severity"], "inconsistency");
    assert_eq!(f["focus"], uri("tom"));
    assert_eq!(f["evidence"]["classes"], json!([uri("Cat"), uri("Dog")]));
    assert_eq!(f["basis"], "asserted");
    assert_eq!(
        f["message"],
        "ex:tom is an instance of the disjoint classes ex:Cat and ex:Dog"
    );
    // only stated types: nothing
    let r = check(
        &s,
        &DiagnoseOptions {
            closure: Closure::None,
            ..opts()
        },
    );
    assert_eq!(r.status, ReportStatus::NoneFound);
    assert_eq!(r.to_json()["scope"]["closure"], "none");
}

#[test]
fn owl_nothing_members_and_unsatisfiable_classes() {
    let s = store("ex:x a owl:Nothing .");
    let r = check(&s, &opts());
    let f = findings(&r, "nothing-member");
    assert_eq!(f.len(), 1);
    assert_eq!(f[0]["rule"], "cls-nothing2");
    assert_eq!(r.status, ReportStatus::ViolationsFound);

    let s = store("ex:E rdfs:subClassOf ex:F . ex:F rdfs:subClassOf owl:Nothing .");
    let r = check(&s, &opts());
    assert_eq!(r.status, ReportStatus::NoneFound, "warnings never violate");
    let w = findings(&r, "unsatisfiable-class");
    assert_eq!(w.len(), 2);
    let e = w.iter().find(|f| f["focus"] == uri("E")).unwrap();
    assert_eq!(e["severity"], "warning");
    assert_eq!(
        e["evidence"]["path"],
        json!([
            uri("E"),
            uri("F"),
            {"type": "uri", "value": "http://www.w3.org/2002/07/owl#Nothing"}
        ])
    );

    update(&s, "INSERT DATA { ex:e a ex:E }");
    let r = check(&s, &opts());
    assert_eq!(r.status, ReportStatus::ViolationsFound);
    let f = findings(&r, "nothing-member");
    assert_eq!(f.len(), 1);
    assert_eq!(f[0]["focus"], uri("e"));
    assert_eq!(f[0]["evidence"]["type"], uri("E"));
    // E has a member now; F still has one through E under the closure
    assert!(findings(&r, "unsatisfiable-class").is_empty());
}

#[test]
fn same_as_and_different_from() {
    let s = store("ex:a owl:sameAs ex:b . ex:b owl:differentFrom ex:a .");
    assert_eq!(findings(&check(&s, &opts()), "same-different").len(), 1);
    let s = store("ex:c owl:differentFrom ex:c .");
    let f = findings(&check(&s, &opts()), "same-different");
    assert_eq!(f.len(), 1);
    assert_eq!(f[0]["message"], "ex:c is declared different from itself");
    let s = store(
        "ex:d owl:sameAs ex:e . ex:e owl:sameAs ex:f . ex:d owl:differentFrom ex:f .
         ex:g owl:differentFrom ex:h .",
    );
    let f = findings(&check(&s, &opts()), "same-different");
    assert_eq!(f.len(), 1);
    assert_eq!(f[0]["focus"], uri("d"));
    assert_eq!(f[0]["evidence"]["other"], uri("f"));
}

#[test]
fn functional_literal_conflicts_compare_values() {
    let s = store(
        r#"ex:age a owl:FunctionalProperty .
        ex:p ex:age 30, "30"^^xsd:decimal, 31 .
        ex:q ex:age ex:v1, ex:v2 .
        ex:r ex:age "1"^^xsd:integer, "1" .
        ex:u ex:age "a"^^ex:dt, "b"^^ex:dt ."#,
    );
    let r = check(&s, &opts());
    let mut f = findings(&r, "functional-literal-conflict");
    assert_eq!(f.len(), 3, "{f:#?}");
    // an integer and a string are different values (disjoint value spaces); literals of
    // an unknown datatype are never compared
    let r_at = f.iter().position(|x| x["focus"] == uri("r")).unwrap();
    f.remove(r_at);
    for x in &f {
        assert_eq!(x["focus"], uri("p"));
        assert_eq!(x["evidence"]["property"], uri("age"));
        let vals: Vec<&str> = x["evidence"]["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["value"].as_str().unwrap())
            .collect();
        assert!(vals.contains(&"31"), "{vals:?}");
    }
}

#[test]
fn all_disjoint_classes_and_empty_thing() {
    let s = store(
        "[] a owl:AllDisjointClasses ; owl:members (ex:A ex:B ex:C) .
         ex:z a ex:A, ex:C .",
    );
    let f = findings(&check(&s, &opts()), "all-disjoint-classes");
    assert_eq!(f.len(), 1);
    assert_eq!(f[0]["evidence"]["classes"], json!([uri("A"), uri("C")]));

    let s = store("owl:Thing rdfs:subClassOf owl:Nothing .");
    let r = check(&s, &opts());
    assert_eq!(r.status, ReportStatus::ViolationsFound);
    let f = findings(&r, "thing-empty");
    assert_eq!(f.len(), 1);
    assert_eq!(f[0]["evidence"]["axiom"]["type"], "triple");
}

#[test]
fn stale_inferences_are_marked_by_basis() {
    let s = store(
        "ex:K rdfs:subClassOf ex:Cat . ex:Cat owl:disjointWith ex:Dog .
         ex:k a ex:K, ex:Dog .",
    );
    materialize(&s, &Profile::Rdfs, &ReasonOptions::default()).unwrap();
    let with_inf = DiagnoseOptions {
        closure: Closure::None,
        inferences: true,
        ..opts()
    };
    let r = check(&s, &with_inf);
    let f = findings(&r, "disjoint-classes");
    assert_eq!(f.len(), 1);
    // only the inferred `ex:k a ex:Cat` makes it a finding without the closure
    assert_eq!(f[0]["basis"], "uses-inferences", "{f:#?}");
    let r = check(
        &s,
        &DiagnoseOptions {
            closure: Closure::Subclass,
            ..with_inf.clone()
        },
    );
    assert_eq!(r.findings[0].basis, Basis::Asserted);
    assert_eq!(r.to_json()["scope"]["inferences"]["included"], true);

    // the stale inferred type keeps the finding alive
    update(&s, "DELETE DATA { ex:k a ex:K }");
    let r = check(&s, &with_inf);
    assert_eq!(r.findings.len(), 1);
    assert_eq!(r.findings[0].basis, Basis::UsesInferences);
    // without the stale inferences there is nothing
    let r = check(
        &s,
        &DiagnoseOptions {
            inferences: false,
            ..with_inf.clone()
        },
    );
    assert_eq!(r.status, ReportStatus::NoneFound);
    materialize(&s, &Profile::Rdfs, &ReasonOptions::default()).unwrap();
    assert_eq!(check(&s, &with_inf).status, ReportStatus::NoneFound);
}

#[test]
fn truncation_and_limits() {
    let mut ttl = String::from("ex:Cat owl:disjointWith ex:Dog .\n");
    for i in 0..150 {
        ttl.push_str(&format!("ex:i{i} a ex:Cat, ex:Dog .\n"));
    }
    let s = store(&ttl);
    let r = check(&s, &opts());
    assert_eq!(outcome(&r, "disjoint-classes"), CheckStatus::Truncated);
    assert_eq!(
        r.checks
            .iter()
            .find(|c| c.id == "disjoint-classes")
            .unwrap()
            .findings,
        100
    );
    assert_eq!(r.status, ReportStatus::ViolationsFound);
    let r = check(
        &s,
        &DiagnoseOptions {
            limit: 1000,
            ..opts()
        },
    );
    assert_eq!(outcome(&r, "disjoint-classes"), CheckStatus::Violations);
    assert_eq!(r.findings.len(), 150);
}

#[test]
fn check_selection_and_timeouts() {
    let s = store("ex:x a owl:Nothing .");
    let r = check(
        &s,
        &DiagnoseOptions {
            checks: vec!["same-different".into()],
            ..opts()
        },
    );
    assert_eq!(r.checks.len(), 1);
    assert_eq!(r.status, ReportStatus::NoneFound);
    assert!(
        diagnose(
            s.snapshot(),
            &DiagnoseOptions {
                checks: vec!["bogus".into()],
                ..opts()
            }
        )
        .is_err()
    );
    // an expired deadline marks every check as timed out: incomplete, never "none-found"
    let r = check(
        &s,
        &DiagnoseOptions {
            timeout: Some(std::time::Duration::ZERO),
            checks: vec!["same-different".into()],
            ..opts()
        },
    );
    assert_eq!(r.status, ReportStatus::Incomplete);
    assert_eq!(outcome(&r, "same-different"), CheckStatus::Timeout);
    let clean = store("ex:a a ex:B .");
    assert_eq!(check(&clean, &opts()).status, ReportStatus::NoneFound);
}

#[test]
fn turtle_rendering_uses_shacl_result_properties() {
    use sparkles_reasoner::diagnostics::ReportContext;
    let s = store(
        "ex:Cat owl:disjointWith ex:Dog . ex:tom a ex:Cat, ex:Dog .
         owl:Thing rdfs:subClassOf owl:Nothing .",
    );
    let r = check(&s, &opts());
    let prefixes = vec![("ex".to_string(), "http://ex.org/".to_string())];
    let ttl = r.to_turtle(&ReportContext {
        dataset: Some("t"),
        prefixes: &prefixes,
        ..Default::default()
    });
    assert!(ttl.contains("@prefix spk: <urn:x-sparkles:>"), "{ttl}");
    assert!(ttl.contains("sh:focusNode ex:tom"), "{ttl}");
    assert!(!ttl.contains("conforms"), "never claims conformance: {ttl}");
    // it parses, and the evidence lists keep their order
    let triples: Vec<oxrdf::Triple> = oxttl::TurtleParser::new()
        .for_slice(ttl.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("{e}\n{ttl}"));
    let has = |p: &str, o: &str| {
        triples
            .iter()
            .any(|t| t.predicate.as_str() == p && t.object.to_string() == o)
    };
    assert!(has("urn:x-sparkles:dataset", "\"t\""));
    assert!(has(
        "http://www.w3.org/ns/shacl#sourceConstraintComponent",
        "<urn:x-sparkles:check:disjoint-classes>"
    ));
    assert!(has(
        "http://www.w3.org/ns/shacl#resultSeverity",
        "<http://www.w3.org/ns/shacl#Violation>"
    ));
    assert!(has("urn:x-sparkles:rule", "\"cax-dw\""));
    assert!(has(
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
        "<http://ex.org/Cat>"
    ));
    // the thing-empty axiom is a triple term
    assert!(
        triples
            .iter()
            .any(|t| t.predicate.as_str() == "urn:x-sparkles:axiom"
                && matches!(t.object, oxrdf::Term::Triple(_))),
        "{ttl}"
    );
    let reports = triples
        .iter()
        .filter(|t| t.object.to_string() == "<urn:x-sparkles:DiagnosticsReport>")
        .count();
    assert_eq!(reports, 1);
}

// ------------------------------------------------- OWL 2 RL rules, one by one ------

/// The findings of one check on `ttl`, which must be the only check with findings.
fn only(ttl: &str, id: &str) -> Vec<J> {
    let r = check(&store(ttl), &opts());
    let all = r.to_json()["findings"].as_array().unwrap().clone();
    let f = findings(&r, id);
    assert_eq!(all.len(), f.len(), "other checks found something: {all:#?}");
    if !f.is_empty() {
        assert_eq!(r.status, ReportStatus::ViolationsFound);
        assert!(f.iter().all(|x| x["severity"] == "inconsistency"));
    }
    f
}

fn one(ttl: &str, id: &str, rule: &str) -> J {
    let f = only(ttl, id);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0]["rule"], rule);
    f[0].clone()
}

#[test]
fn prp_irp_irreflexive_property() {
    let f = one(
        "ex:knows a owl:IrreflexiveProperty . ex:a ex:knows ex:a .",
        "irreflexive-property",
        "prp-irp",
    );
    assert_eq!(f["focus"], uri("a"));
    assert_eq!(f["evidence"]["property"], uri("knows"));
    assert_eq!(
        f["message"],
        "ex:a is related to itself by the irreflexive property ex:knows"
    );
    assert!(
        only(
            "ex:knows a owl:IrreflexiveProperty . ex:a ex:knows ex:b . ex:c ex:other ex:c .",
            "irreflexive-property"
        )
        .is_empty()
    );
}

#[test]
fn prp_asyp_asymmetric_property() {
    let f = one(
        "ex:parentOf a owl:AsymmetricProperty .
         ex:a ex:parentOf ex:b . ex:b ex:parentOf ex:a .",
        "asymmetric-property",
        "prp-asyp",
    );
    assert_eq!(f["focus"], uri("a"));
    assert_eq!(f["evidence"]["other"], uri("b"));
    assert_eq!(
        f["message"],
        "ex:a and ex:b are related in both directions by the asymmetric property ex:parentOf"
    );
    // a self-loop is a violation too
    one(
        "ex:parentOf a owl:AsymmetricProperty . ex:c ex:parentOf ex:c .",
        "asymmetric-property",
        "prp-asyp",
    );
    assert!(
        only(
            "ex:parentOf a owl:AsymmetricProperty .
             ex:a ex:parentOf ex:b . ex:b ex:parentOf ex:c .",
            "asymmetric-property"
        )
        .is_empty()
    );
}

#[test]
fn prp_pdw_disjoint_properties() {
    let f = one(
        "ex:likes owl:propertyDisjointWith ex:hates .
         ex:a ex:likes ex:b ; ex:hates ex:b .",
        "disjoint-properties",
        "prp-pdw",
    );
    assert_eq!(f["focus"], uri("a"));
    assert_eq!(
        f["evidence"]["properties"],
        json!([uri("hates"), uri("likes")])
    );
    assert_eq!(f["evidence"]["value"], uri("b"));
    assert!(
        only(
            "ex:likes owl:propertyDisjointWith ex:hates .
             ex:a ex:likes ex:b ; ex:hates ex:c .",
            "disjoint-properties"
        )
        .is_empty()
    );
}

#[test]
fn prp_adp_all_disjoint_properties() {
    let f = one(
        "[] a owl:AllDisjointProperties ; owl:members (ex:p ex:q ex:r) .
         ex:a ex:p 1 ; ex:r 1 ; ex:q 2 .",
        "all-disjoint-properties",
        "prp-adp",
    );
    assert_eq!(f["evidence"]["properties"], json!([uri("p"), uri("r")]));
    assert_eq!(f["evidence"]["value"]["value"], "1");
    assert_eq!(f["evidence"]["axiom"]["type"], "bnode");
    assert!(
        only(
            "[] a owl:AllDisjointProperties ; owl:members (ex:p ex:q) .
             ex:a ex:p 1 ; ex:q 2 . ex:b ex:q 1 .",
            "all-disjoint-properties"
        )
        .is_empty()
    );
}

#[test]
fn cls_com_complement_classes() {
    let f = one(
        "ex:Dead owl:complementOf ex:Alive . ex:Zombie rdfs:subClassOf ex:Dead .
         ex:z a ex:Zombie, ex:Alive .",
        "complement-classes",
        "cls-com",
    );
    assert_eq!(f["focus"], uri("z"));
    assert_eq!(f["evidence"]["classes"], json!([uri("Alive"), uri("Dead")]));
    assert_eq!(
        f["message"],
        "ex:z is an instance of both ex:Dead and its complement ex:Alive"
    );
    assert!(
        only(
            "ex:Dead owl:complementOf ex:Alive . ex:z a ex:Dead . ex:y a ex:Alive .",
            "complement-classes"
        )
        .is_empty()
    );
}

#[test]
fn cls_maxc1_max_cardinality_zero() {
    let ttl = "ex:Childless owl:maxCardinality \"0\"^^xsd:nonNegativeInteger ;
                 owl:onProperty ex:hasChild .
               ex:a a ex:Childless .";
    let f = one(
        &format!("{ttl} ex:a ex:hasChild ex:c ."),
        "max-cardinality-zero",
        "cls-maxc1",
    );
    assert_eq!(f["focus"], uri("a"));
    assert_eq!(f["evidence"]["restriction"], uri("Childless"));
    assert_eq!(f["evidence"]["property"], uri("hasChild"));
    assert_eq!(f["evidence"]["value"], uri("c"));
    // a plain integer 0 is the same cardinality
    one(
        "[] owl:maxCardinality 0 ; owl:onProperty ex:p ; owl:equivalentClass ex:None .
         ex:R owl:maxCardinality 0 ; owl:onProperty ex:p . ex:b a ex:R ; ex:p 1 .",
        "max-cardinality-zero",
        "cls-maxc1",
    );
    assert!(
        only(
            &format!("{ttl} ex:b ex:hasChild ex:c ."),
            "max-cardinality-zero"
        )
        .is_empty()
    );
    assert!(
        only(
            "ex:One owl:maxCardinality 1 ; owl:onProperty ex:p . ex:a a ex:One ; ex:p 1 .",
            "max-cardinality-zero"
        )
        .is_empty()
    );
}

#[test]
fn cls_maxqc1_and_maxqc2_max_qualified_cardinality_zero() {
    let ttl = "ex:NoDogs owl:maxQualifiedCardinality \"0\"^^xsd:nonNegativeInteger ;
                 owl:onProperty ex:owns ; owl:onClass ex:Dog .
               ex:Puppy rdfs:subClassOf ex:Dog .
               ex:a a ex:NoDogs .";
    let f = one(
        &format!("{ttl} ex:a ex:owns ex:rex . ex:rex a ex:Puppy ."),
        "max-qualified-cardinality-zero",
        "cls-maxqc1",
    );
    assert_eq!(f["evidence"]["class"], uri("Dog"));
    assert_eq!(f["evidence"]["value"], uri("rex"));
    // a value outside the qualifying class is allowed
    assert!(
        only(
            &format!("{ttl} ex:a ex:owns ex:tom . ex:tom a ex:Cat ."),
            "max-qualified-cardinality-zero"
        )
        .is_empty()
    );
    // with owl:Thing any value counts, typed or not (cls-maxqc2)
    let thing = "ex:Nothingness owl:maxQualifiedCardinality 0 ;
                   owl:onProperty ex:owns ; owl:onClass owl:Thing .
                 ex:b a ex:Nothingness .";
    let f = one(
        &format!("{thing} ex:b ex:owns ex:x ."),
        "max-qualified-cardinality-zero",
        "cls-maxqc2",
    );
    assert_eq!(f["focus"], uri("b"));
    assert!(only(thing, "max-qualified-cardinality-zero").is_empty());
}

#[test]
fn eq_diff2_and_eq_diff3_all_different() {
    let f = one(
        "[] a owl:AllDifferent ; owl:members (ex:a ex:b ex:c) . ex:a owl:sameAs ex:x .
         ex:c owl:sameAs ex:x .",
        "all-different",
        "eq-diff2",
    );
    assert_eq!(f["evidence"]["individuals"], json!([uri("a"), uri("c")]));
    let f = one(
        "[] a owl:AllDifferent ; owl:distinctMembers (ex:a ex:b) . ex:b owl:sameAs ex:a .",
        "all-different",
        "eq-diff3",
    );
    assert_eq!(f["focus"], uri("a"));
    // an individual listed twice
    let f = one(
        "[] a owl:AllDifferent ; owl:members (ex:a ex:b ex:a) .",
        "all-different",
        "eq-diff2",
    );
    assert_eq!(f["evidence"]["individuals"], json!([uri("a"), uri("a")]));
    assert!(
        only(
            "[] a owl:AllDifferent ; owl:members (ex:a ex:b ex:c) . ex:a owl:sameAs ex:x .
             [] a owl:AllDifferent ; owl:distinctMembers (ex:d ex:e) .",
            "all-different"
        )
        .is_empty()
    );
}

#[test]
fn prp_npa1_and_prp_npa2_negative_property_assertions() {
    let f = one(
        "ex:n1 owl:sourceIndividual ex:a ; owl:assertionProperty ex:knows ;
             owl:targetIndividual ex:b .
         ex:a ex:knows ex:b .",
        "negative-property-assertion",
        "prp-npa1",
    );
    assert_eq!(f["focus"], uri("a"));
    assert_eq!(f["evidence"]["axiom"], uri("n1"));
    assert_eq!(f["evidence"]["target"], uri("b"));
    assert_eq!(
        f["message"],
        "ex:a ex:knows ex:b is stated, but the negative property assertion ex:n1 denies it"
    );
    let f = one(
        "ex:n2 owl:sourceIndividual ex:a ; owl:assertionProperty ex:age ;
             owl:targetValue 30 .
         ex:a ex:age 30 .",
        "negative-property-assertion",
        "prp-npa2",
    );
    assert_eq!(f["evidence"]["target"]["value"], "30");
    assert!(
        only(
            "ex:n1 owl:sourceIndividual ex:a ; owl:assertionProperty ex:knows ;
                 owl:targetIndividual ex:b .
             ex:n2 owl:sourceIndividual ex:a ; owl:assertionProperty ex:age ;
                 owl:targetValue 30 .
             ex:a ex:knows ex:c ; ex:age 31 . ex:b ex:knows ex:a .",
            "negative-property-assertion"
        )
        .is_empty()
    );
}

#[test]
fn property_checks_see_inferred_assertions_with_their_basis() {
    // ex:p is a subproperty of an irreflexive property: only the inferences show it
    let s =
        store("ex:q a owl:IrreflexiveProperty . ex:p rdfs:subPropertyOf ex:q . ex:a ex:p ex:a .");
    assert!(findings(&check(&s, &opts()), "irreflexive-property").is_empty());
    materialize(&s, &Profile::Rdfs, &ReasonOptions::default()).unwrap();
    let r = check(
        &s,
        &DiagnoseOptions {
            inferences: true,
            ..opts()
        },
    );
    let f = findings(&r, "irreflexive-property");
    assert_eq!(f.len(), 1);
    assert_eq!(f[0]["basis"], "uses-inferences");
}
