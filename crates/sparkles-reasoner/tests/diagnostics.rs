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
