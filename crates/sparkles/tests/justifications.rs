#![cfg(feature = "reasoning")]
use oxrdf::NamedNode;
use sparkles::reasoning::analysis::AnalysisProgram;
use sparkles::reasoning::justify::{JustifyRequest, JustifyTarget};
use sparkles::reasoning::reasoner::diagnostics::DiagnoseOptions;
use sparkles::reasoning::reasoner::{Extras, GraphRef, Inputs, Profile};
use sparkles::{Dataset, io::RdfFormat};

const PRE: &str = "@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . @prefix owl: <http://www.w3.org/2002/07/owl#> . ";
fn options(target: JustifyTarget) -> JustifyRequest {
    let mut request = JustifyRequest::new(target);
    request.analysis.program = Some(AnalysisProgram {
        profile: Profile::OwlRl,
        extras: Extras::default(),
        inputs: Inputs::default(),
    });
    request
}
fn open(text: &str) -> (tempfile::TempDir, Dataset) {
    let dir = tempfile::tempdir().unwrap();
    let dataset = Dataset::open(dir.path().join("db")).unwrap();
    dataset
        .load_str(&format!("{PRE}{text}"), RdfFormat::TriG)
        .unwrap();
    (dir, dataset)
}
fn finding(dataset: &Dataset) -> sparkles::reasoning::reasoner::diagnostics::Finding {
    dataset
        .reasoning()
        .diagnostics(&DiagnoseOptions {
            checks: vec!["disjoint-classes".into()],
            ..Default::default()
        })
        .unwrap()
        .report
        .findings
        .remove(0)
}
#[test]
fn finding_support_is_asserted_sufficient_and_single_deletion_minimal() {
    let (_dir, dataset) =
        open("ex:Cat owl:disjointWith ex:Dog . ex:tom a ex:Cat, ex:Dog . ex:unrelated ex:p 1 .");
    let head = dataset.store().snapshot().commit;
    let report = dataset
        .reasoning()
        .justify(&options(JustifyTarget::Finding(finding(&dataset))))
        .unwrap();
    assert!(report.complete);
    let result = &report.results[0];
    assert!(result.holds);
    assert_eq!(result.justifications.len(), 1);
    assert_eq!(result.justifications[0].facts.len(), 3);
    for removed in 0..3 {
        let test = Dataset::memory();
        for (index, fact) in result.justifications[0].facts.iter().enumerate() {
            if index != removed {
                test.insert(fact.quad.as_ref()).unwrap();
            }
        }
        assert!(
            test.reasoning()
                .diagnostics(&DiagnoseOptions {
                    checks: vec!["disjoint-classes".into()],
                    ..Default::default()
                })
                .unwrap()
                .report
                .findings
                .is_empty()
        );
    }
    assert_eq!(dataset.store().snapshot().commit, head);
}
#[test]
fn alternate_supports_in_distinct_graphs_remain_distinct_justifications() {
    let (_dir, dataset) = open(
        "ex:g1 { ex:Cat owl:disjointWith ex:Dog . ex:tom a ex:Cat, ex:Dog } ex:g2 { ex:tom a ex:Dog }",
    );
    let target = dataset
        .reasoning()
        .diagnostics(&DiagnoseOptions {
            checks: vec!["disjoint-classes".into()],
            graphs: vec!["http://ex.org/g1".into(), "http://ex.org/g2".into()],
            ..Default::default()
        })
        .unwrap()
        .report
        .findings
        .remove(0);
    let mut request = options(JustifyTarget::Finding(target));
    request.analysis.program.as_mut().unwrap().inputs = Inputs {
        data_graphs: vec![
            GraphRef::Named("http://ex.org/g1".into()),
            GraphRef::Named("http://ex.org/g2".into()),
        ],
        ..Default::default()
    };
    let report = dataset.reasoning().justify(&request).unwrap();
    assert!(report.complete);
    assert_eq!(report.results[0].justifications.len(), 2);
    assert!(
        report.results[0]
            .justifications
            .iter()
            .all(|support| support.facts.len() == 3)
    );
}
#[test]
fn class_probe_finds_disjoint_superclasses_and_ignores_unrelated_conflict() {
    let (_dir, dataset) = open(
        "ex:CatDog rdfs:subClassOf ex:Cat, ex:Dog . ex:Cat owl:disjointWith ex:Dog . ex:unrelated a ex:Cat, ex:Dog .",
    );
    for (class, holds) in [("CatDog", true), ("Harmless", false)] {
        let request = options(JustifyTarget::Class(
            NamedNode::new(format!("http://ex.org/{class}")).unwrap(),
        ));
        let report = dataset.reasoning().justify(&request).unwrap();
        assert!(report.complete);
        assert_eq!(report.results[0].holds, holds);
        if holds {
            assert_eq!(report.results[0].justifications[0].facts.len(), 3);
            assert!(
                report.results[0].justifications[0]
                    .facts
                    .iter()
                    .all(|fact| !fact.quad.to_string().contains("class-probe"))
            );
        }
    }
}
#[test]
fn class_probe_is_visible_to_named_only_input_program() {
    let (_dir, dataset) = open(
        "ex:ontology {ex:CatDog rdfs:subClassOf ex:Cat, ex:Dog . ex:Cat owl:disjointWith ex:Dog }",
    );
    let mut request = options(JustifyTarget::Class(
        NamedNode::new("http://ex.org/CatDog").unwrap(),
    ));
    request.analysis.program.as_mut().unwrap().inputs = Inputs {
        data_graphs: vec![GraphRef::Named("http://ex.org/ontology".into())],
        ..Default::default()
    };
    let report = dataset.reasoning().justify(&request).unwrap();
    assert!(report.results[0].holds);
    assert_eq!(report.results[0].justifications[0].facts.len(), 3);
}
#[test]
fn exhausted_check_budget_cannot_claim_minimality() {
    let (_dir, dataset) = open("ex:Cat owl:disjointWith ex:Dog . ex:tom a ex:Cat, ex:Dog .");
    let mut request = options(JustifyTarget::Finding(finding(&dataset)));
    request.max_checks = 1;
    let report = dataset.reasoning().justify(&request).unwrap();
    assert!(!report.complete);
    assert!(report.results[0].justifications.is_empty());
}
#[test]
fn nonmonotone_exhaustion_checks_smaller_sets_even_when_whole_candidate_fails() {
    let (_dir, dataset) = open(
        "ex:a ex:p 1 . ex:a ex:blocked 1 . ex:Cat owl:disjointWith ex:Dog . ex:tom a ex:Cat .",
    );
    let (_other, target_ds) = open("ex:Cat owl:disjointWith ex:Dog . ex:tom a ex:Cat, ex:Dog .");
    let mut request = options(JustifyTarget::Finding(finding(&target_ds)));
    request.analysis.program.as_mut().unwrap().profile=Profile::Rules("[r: (<http://ex.org/a> <http://ex.org/p> 1), noValue(<http://ex.org/a>, <http://ex.org/blocked>) -> (<http://ex.org/tom> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Dog>)]".into());
    let report = dataset.reasoning().justify(&request).unwrap();
    assert!(report.complete);
    assert!(report.results[0].holds);
    assert_eq!(report.results[0].justifications[0].facts.len(), 3);
}

#[test]
fn generated_blank_focus_is_matched_across_isolated_oracle_runs() {
    use sparkles::reasoning::ReasonRequest;
    let (_dir, dataset) = open("ex:Cat owl:disjointWith ex:Dog . ex:trigger ex:p 1 .");
    let profile=Profile::Rules("[generated: (<http://ex.org/trigger> <http://ex.org/p> 1), makeTemp(?x) -> (?x <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Cat>), (?x <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Dog>)]".into());
    dataset
        .reasoning()
        .run(&ReasonRequest {
            profile: profile.clone(),
            ..Default::default()
        })
        .unwrap();
    let target = dataset
        .reasoning()
        .diagnostics(&DiagnoseOptions {
            checks: vec!["disjoint-classes".into()],
            inferences: true,
            ..Default::default()
        })
        .unwrap()
        .report
        .findings
        .remove(0);
    let mut request = options(JustifyTarget::Finding(target));
    request.analysis.program.as_mut().unwrap().profile = profile;
    let report = dataset.reasoning().justify(&request).unwrap();
    assert!(report.results[0].holds);
    assert!(report.complete);
    assert_eq!(report.results[0].justifications[0].facts.len(), 2);
}

#[test]
fn justified_diagnostics_preserve_findings_and_share_the_check_allowance() {
    use sparkles::reasoning::analysis::AnalysisOptions;
    let (_dir, dataset) = open(
        "ex:Cat owl:disjointWith ex:Dog . ex:tom a ex:Cat, ex:Dog . ex:rex a ex:Cat, ex:Dog .",
    );
    let options = DiagnoseOptions {
        checks: vec!["disjoint-classes".into()],
        ..Default::default()
    };
    let report = dataset
        .reasoning()
        .diagnostics_justified(&options, &AnalysisOptions::default(), 500)
        .unwrap();
    assert_eq!(report.diagnostics.report.findings.len(), 2);
    assert!(report.supports.complete);
    assert!(
        report
            .supports
            .results
            .iter()
            .all(|result| result.justifications[0].facts.len() == 3)
    );
    let mut value = report.diagnostics.report.to_json();
    report.attach(&mut value);
    assert_eq!(value["findings"][0]["justificationComplete"], true);
    let limited = dataset
        .reasoning()
        .diagnostics_justified(&options, &AnalysisOptions::default(), 1)
        .unwrap();
    assert!(!limited.supports.complete);
    assert_eq!(limited.supports.checks, 1);
    assert!(
        limited
            .supports
            .results
            .iter()
            .all(|result| result.justifications.is_empty())
    );
    let turtle = report.supports.to_turtle();
    for quad in oxrdfio::RdfParser::from_format(RdfFormat::Turtle).for_slice(turtle.as_bytes()) {
        quad.unwrap();
    }
}

#[test]
fn classes_all_follows_the_relevant_superclass_cone() {
    let (_dir, dataset) = open(
        "ex:CatDog rdfs:subClassOf ex:Cat, ex:Dog . ex:Deeper rdfs:subClassOf ex:CatDog . ex:Cat owl:disjointWith ex:Dog . ex:Harmless a owl:Class .",
    );
    let mut request = options(JustifyTarget::AllClasses);
    request.max_checks = 2000;
    let report = dataset.reasoning().justify(&request).unwrap();
    assert!(report.complete);
    let classes: std::collections::BTreeSet<_> = report
        .results
        .iter()
        .map(|result| result.class.as_ref().unwrap().as_str())
        .collect();
    assert!(classes.contains("http://ex.org/CatDog"));
    assert!(classes.contains("http://ex.org/Deeper"));
    assert!(!classes.contains("http://ex.org/Harmless"));
    assert!(
        report
            .results
            .iter()
            .filter(|result| matches!(
                result.class.as_ref().unwrap().as_str(),
                "http://ex.org/CatDog" | "http://ex.org/Deeper"
            ))
            .all(|result| result.holds)
    );
}

#[test]
fn justified_diagnostics_cannot_disclose_an_unreadable_graph() {
    use sparkles::access::{GraphAccess, GraphRule, Graphs};
    use sparkles::reasoning::analysis::AnalysisOptions;
    let (_dir, dataset) = open(
        "ex:hidden {ex:Cat owl:disjointWith ex:Dog . ex:secret a ex:Cat, ex:Dog} ex:visible {ex:s ex:p 1}",
    );
    let access = std::sync::Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["http://ex.org/visible"], &[])),
        Graphs::none(),
    ));
    let report = dataset
        .reasoning()
        .diagnostics_justified(
            &DiagnoseOptions {
                checks: vec!["disjoint-classes".into()],
                graphs: vec!["http://ex.org/hidden".into()],
                ..Default::default()
            },
            &AnalysisOptions {
                access: Some(access),
                ..Default::default()
            },
            500,
        )
        .unwrap();
    assert!(
        report.diagnostics.report.findings.is_empty(),
        "an unreadable graph disclosed its findings"
    );
}

#[test]
fn nothing_member_support_accepts_requested_type_with_additional_inferred_types() {
    let (_dir, dataset) = open("ex:C rdfs:subClassOf owl:Nothing . ex:x a ex:C .");
    let target = dataset
        .reasoning()
        .diagnostics(&DiagnoseOptions {
            checks: vec!["nothing-member".into()],
            ..Default::default()
        })
        .unwrap()
        .report
        .findings
        .remove(0);
    let mut request = options(JustifyTarget::Finding(target));
    request.analysis.program.as_mut().unwrap().profile = Profile::RdfsSimple;
    let report = dataset.reasoning().justify(&request).unwrap();
    assert!(
        report.results[0].holds,
        "requested evidence was hidden by a larger oracle evidence group"
    );
    assert!(report.complete);
    assert_eq!(report.results[0].justifications[0].facts.len(), 2);
}

#[test]
fn class_supports_include_independent_probe_conflicts() {
    let (_dir, dataset) = open(
        "ex:C rdfs:subClassOf ex:A, ex:B, ex:D, ex:E . ex:A owl:disjointWith ex:B . ex:D owl:disjointWith ex:E .",
    );
    let mut request = options(JustifyTarget::Class(
        NamedNode::new("http://ex.org/C").unwrap(),
    ));
    request.max_checks = 3000;
    let report = dataset.reasoning().justify(&request).unwrap();
    assert!(report.complete);
    assert_eq!(
        report.results[0].justifications.len(),
        2,
        "one independent inconsistency was excluded from the class oracle"
    );
    assert!(
        report.results[0]
            .justifications
            .iter()
            .all(|support| support.facts.len() == 3)
    );
}

#[test]
fn relevant_class_cones_traverse_anonymous_disjoint_classes() {
    let (_dir, dataset) = open("ex:C rdfs:subClassOf _:a, _:b . _:a owl:disjointWith _:b .");
    let direct = dataset
        .reasoning()
        .justify(&options(JustifyTarget::Class(
            NamedNode::new("http://ex.org/C").unwrap(),
        )))
        .unwrap();
    assert!(direct.results[0].holds);
    let mut request = options(JustifyTarget::AllClasses);
    request.max_checks = 3000;
    let report = dataset.reasoning().justify(&request).unwrap();
    assert!(report.complete);
    assert!(
        report
            .results
            .iter()
            .any(
                |result| result.class.as_ref().unwrap().as_str() == "http://ex.org/C"
                    && result.holds
            ),
        "the named subclass disappeared behind anonymous superclass nodes"
    );
}

#[test]
fn asserted_findings_have_supports_without_a_recorded_or_explicit_program() {
    let (_dir, dataset) = open("ex:Cat owl:disjointWith ex:Dog . ex:tom a ex:Cat, ex:Dog .");
    let request = JustifyRequest::new(JustifyTarget::Finding(finding(&dataset)));
    let report = dataset.reasoning().justify(&request).unwrap();
    assert!(report.results[0].holds);
    assert_eq!(report.results[0].justifications[0].facts.len(), 3);
}

#[test]
fn classes_all_charges_the_shared_large_facts_in_retained_results() {
    let mut text = String::from("ex:Root owl:disjointWith ex:Other . ex:source ex:payload \"");
    text.push_str(&"x".repeat(320 * 1024));
    text.push_str("\" . ");
    for index in 0..64 {
        text.push_str(&format!("ex:C{index} rdfs:subClassOf ex:Root . "));
    }
    let (_dir, dataset) = open(&text);
    let mut request = options(JustifyTarget::AllClasses);
    request.analysis.program.as_mut().unwrap().profile=Profile::Rules("[support: (?x <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> ?c), (?c <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex.org/Root>), (<http://ex.org/source> <http://ex.org/payload> ?literal) -> (?x <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Other>)]".into());
    request.analysis.max_memory = Some(16 * 1024 * 1024);
    request.analysis.timeout = Some(std::time::Duration::from_secs(120));
    request.max_checks = 20_000;
    let result = dataset.reasoning().justify(&request);
    assert!(
        matches!(result,Err(sparkles::Error::Component(ref error)) if error.code=="reasoning-budget" && error.message.contains("memory")),
        "retained class results did not exhaust the shared memory grant: {}",
        match result {
            Ok(ref report) => format!(
                "{} results, complete={}",
                report.results.len(),
                report.complete
            ),
            Err(ref error) => error.to_string(),
        }
    );
}

#[test]
fn justified_diagnostics_cannot_disclose_a_denied_inferred_graph() {
    use sparkles::access::{GraphAccess, GraphRule, Graphs};
    use sparkles::reasoning::analysis::AnalysisOptions;
    let (_dir, dataset) =
        open("ex:Cat owl:disjointWith ex:Dog . ex:tom a ex:Cat . ex:tom ex:seed 1 .");
    dataset.reasoning().run(&sparkles::reasoning::ReasonRequest {profile:Profile::Rules("[derived: (<http://ex.org/tom> <http://ex.org/seed> 1) -> (<http://ex.org/tom> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Dog>)]".into()),..Default::default()}).unwrap();
    let access = std::sync::Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["default"], &[])),
        Graphs::none(),
    ));
    let report = dataset
        .reasoning()
        .diagnostics_justified(
            &DiagnoseOptions {
                checks: vec!["disjoint-classes".into()],
                inferences: true,
                ..Default::default()
            },
            &AnalysisOptions {
                access: Some(access),
                ..Default::default()
            },
            500,
        )
        .unwrap();
    assert!(
        report.diagnostics.report.findings.is_empty(),
        "a denied inferred graph disclosed a finding"
    );
}
