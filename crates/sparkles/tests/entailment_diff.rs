#![cfg(feature = "reasoning")]

use oxrdf::{NamedNode, Term};
use sparkles::io::RdfFormat;
use sparkles::reasoning::analysis::{AnalysisOptions, AnalysisProgram};
use sparkles::reasoning::diff::DiffRequest;
use sparkles::reasoning::reasoner::{Extras, Inputs, Profile};
use sparkles::{Dataset, Error};
use std::sync::{Arc, atomic::AtomicBool};

const PRE: &str = "PREFIX ex: <http://ex.org/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX owl: <http://www.w3.org/2002/07/owl#> ";
fn ex(name: &str) -> Term {
    NamedNode::new(format!("http://ex.org/{name}"))
        .unwrap()
        .into()
}
fn open() -> (tempfile::TempDir, Dataset) {
    let dir = tempfile::tempdir().unwrap();
    let dataset = Dataset::open(dir.path().join("db")).unwrap();
    (dir, dataset)
}
fn request(from: impl ToString, to: impl ToString) -> DiffRequest {
    let mut request = DiffRequest::new(from.to_string(), to.to_string());
    request.analysis.program = Some(AnalysisProgram {
        profile: Profile::RdfsSimple,
        extras: Extras::default(),
        inputs: Inputs::default(),
    });
    request
}
fn update(dataset: &Dataset, text: &str) {
    dataset.update(&format!("{PRE}{text}")).unwrap();
}

#[test]
fn inferred_additions_removals_and_serialized_complete_delta() {
    let (_dir, dataset) = open();
    update(
        &dataset,
        "INSERT DATA { ex:Manager rdfs:subClassOf ex:Employee }",
    );
    let from = dataset.store().snapshot().commit;
    update(&dataset, "INSERT DATA { ex:alice a ex:Manager }");
    let to = dataset.store().snapshot().commit;
    let mut req = request(from, to);
    req.all_changes = true;
    let diff = dataset.reasoning().diff(&req).unwrap();
    assert_eq!((diff.added, diff.removed), (1, 0));
    assert_eq!(diff.added_triples[0][2], ex("Employee"));
    assert_eq!(diff.by_class["http://ex.org/Employee"].added, 1);
    assert!(diff.to_trig().unwrap().contains("spk:added"));
    assert!(diff.to_patch().unwrap().starts_with("A "));
    let reverse = dataset.reasoning().diff(&request(to, from)).unwrap();
    assert_eq!((reverse.added, reverse.removed), (0, 1));
    assert_eq!(dataset.store().snapshot().commit, to);
}

#[test]
fn moving_unchanged_fact_between_asserted_and_derived_is_not_a_delta() {
    let (_dir, dataset) = open();
    update(
        &dataset,
        "INSERT DATA { ex:Manager rdfs:subClassOf ex:Employee . ex:alice a ex:Manager, ex:Employee }",
    );
    let a = dataset.store().snapshot().commit;
    update(&dataset, "DELETE DATA { ex:alice a ex:Employee }");
    let b = dataset.store().snapshot().commit;
    for (from, to) in [(a, b), (b, a)] {
        let diff = dataset.reasoning().diff(&request(from, to)).unwrap();
        assert_eq!((diff.added, diff.removed), (0, 0));
    }
}

#[test]
fn asserted_conflict_is_reported_even_when_there_is_no_inferred_delta() {
    let (_dir, dataset) = open();
    update(
        &dataset,
        "INSERT DATA { ex:Cat owl:disjointWith ex:Dog . ex:tom a ex:Cat }",
    );
    let from = dataset.store().snapshot().commit;
    update(&dataset, "INSERT DATA { ex:tom a ex:Dog }");
    let diff = dataset.reasoning().diff(&request(from, "head")).unwrap();
    assert_eq!((diff.added, diff.removed), (0, 0));
    assert!(diff.findings_complete);
    assert_eq!(diff.new_findings.len(), 1);
    assert_eq!(diff.new_findings[0].check, "disjoint-classes");
    assert_eq!(diff.new_findings[0].focus, ex("tom"));
}

#[test]
fn branch_selectors_apply_one_program_without_mutating_either_state() {
    let (_dir, dataset) = open();
    update(
        &dataset,
        "INSERT DATA { ex:Manager rdfs:subClassOf ex:Employee }",
    );
    let from = dataset.store().snapshot().commit;
    dataset
        .store()
        .create_branch("feature", &Default::default())
        .unwrap();
    let branch = dataset.store().branch("feature").unwrap();
    branch
        .load(&[sparkles::io::Source::from_bytes(
            b"<http://ex.org/alice> a <http://ex.org/Manager> .".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let to = branch.snapshot().commit;
    drop(branch);
    let diff = dataset
        .reasoning()
        .diff(&request("main", "feature"))
        .unwrap();
    assert_eq!((diff.added, diff.removed), (1, 0));
    let reverse = dataset
        .reasoning()
        .diff(&request(format!("feature@{to}"), format!("main@{from}")))
        .unwrap();
    assert_eq!((reverse.added, reverse.removed), (0, 1));
    assert_eq!(dataset.store().snapshot().commit, from);
}

#[test]
fn incomplete_samples_cannot_be_serialized_as_applicable_patch() {
    let (_dir, dataset) = open();
    update(
        &dataset,
        "INSERT DATA { ex:Manager rdfs:subClassOf ex:Employee }",
    );
    let from = dataset.store().snapshot().commit;
    update(&dataset, "INSERT DATA { ex:alice a ex:Manager }");
    let mut req = request(from, "head");
    req.sample_limit = 0;
    let diff = dataset.reasoning().diff(&req).unwrap();
    assert_eq!(diff.added, 1);
    assert!(diff.sample_truncated);
    assert!(diff.to_patch().is_err());
    assert!(diff.to_trig().is_err());
}

#[test]
fn cancellation_timeout_and_memory_are_shared_request_controls() {
    let (_dir, dataset) = open();
    update(
        &dataset,
        "INSERT DATA { ex:alice a ex:Manager . ex:Manager rdfs:subClassOf ex:Employee }",
    );
    let mut req = request("head", "head");
    req.analysis.cancel = Some(Arc::new(AtomicBool::new(true)));
    assert!(matches!(
        dataset.reasoning().diff(&req),
        Err(Error::Cancelled)
    ));
    req.analysis.cancel = None;
    req.analysis.timeout = Some(std::time::Duration::ZERO);
    assert!(dataset.reasoning().diff(&req).is_err());
    req.analysis = AnalysisOptions {
        max_memory: Some(1),
        program: req.analysis.program,
        ..Default::default()
    };
    assert!(dataset.reasoning().diff(&req).is_err());
}

#[test]
fn explicit_query_time_program_includes_backward_rules() {
    let (_dir, dataset) = open();
    let from = dataset.store().snapshot().commit;
    update(&dataset, "INSERT DATA { ex:alice ex:p ex:bob }");
    let mut req = request(from, "head");
    req.analysis.program.as_mut().unwrap().profile =
        Profile::Rules("[backward: (?x <http://ex.org/q> ?y) <- (?x <http://ex.org/p> ?y)]".into());
    let diff = dataset.reasoning().diff(&req).unwrap();
    assert_eq!((diff.added, diff.removed), (1, 0));
    assert_eq!(diff.added_triples[0], [ex("alice"), ex("q"), ex("bob")]);
}

#[test]
fn independent_state_graph_grants_hide_inputs_before_reasoning() {
    use sparkles::access::{GraphAccess, GraphRule, Graphs};
    let (_dir, dataset) = open();
    dataset.load_str("@prefix ex:<http://ex.org/>. @prefix rdfs:<http://www.w3.org/2000/01/rdf-schema#>. ex:visible {ex:Manager rdfs:subClassOf ex:Employee} ex:hidden {ex:alice a ex:Manager}",RdfFormat::TriG).unwrap();
    let from = dataset.store().snapshot().commit;
    update(
        &dataset,
        "INSERT DATA { GRAPH ex:hidden { ex:bob a ex:Manager }}",
    );
    let mut req = request(from, "head");
    req.analysis.program.as_mut().unwrap().inputs = Inputs {
        data_graphs: vec![
            sparkles::reasoning::reasoner::GraphRef::Named("http://ex.org/visible".into()),
            sparkles::reasoning::reasoner::GraphRef::Named("http://ex.org/hidden".into()),
        ],
        ..Default::default()
    };
    let access = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["http://ex.org/visible"], &[])),
        Graphs::none(),
    ));
    req.from_access = Some(access.clone());
    req.to_access = Some(access);
    let diff = dataset.reasoning().diff(&req).unwrap();
    assert_eq!((diff.added, diff.removed), (0, 0));
    assert!(diff.new_findings.is_empty());
}

#[test]
fn preview_uses_the_complete_candidate_and_original_head_after_other_writes() {
    let (_dir, dataset) = open();
    update(
        &dataset,
        "INSERT DATA { ex:Manager rdfs:subClassOf ex:Employee }",
    );
    let before = dataset.store().snapshot().commit;
    let opts = sparkles::sparql::QueryOptions {
        write: sparkles::guard::WriteOptions {
            dry_run: Some(sparkles::preview::DryRun {
                retain_states: true,
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let preview = sparkles::preview::catch(dataset.update_with(
        &format!("{PRE}INSERT DATA {{ ex:alice a ex:Manager }}"),
        &opts,
    ))
    .unwrap();
    assert!(preview.changes.is_empty());
    update(&dataset, "INSERT DATA { ex:bob a ex:Manager }");
    let diff = dataset
        .reasoning()
        .diff_preview(&preview, &request("before", "candidate"))
        .unwrap();
    assert_eq!(diff.from.commit, before);
    assert_eq!((diff.added, diff.removed), (1, 0));
    assert_eq!(diff.added_triples[0][0], ex("alice"));
}

#[test]
fn randomized_deltas_match_independent_full_rematerializations() {
    use sparkles::reasoning::ReasonRequest;
    use std::collections::{BTreeMap, BTreeSet};
    fn facts(dataset: &Dataset) -> BTreeMap<String, [Term; 3]> {
        let snapshot = dataset.store().snapshot();
        let mut facts = BTreeMap::new();
        snapshot
            .for_each_quad(|quad| {
                let triple = [
                    snapshot.term(quad[0]).unwrap(),
                    snapshot.term(quad[1]).unwrap(),
                    snapshot.term(quad[2]).unwrap(),
                ];
                facts.insert(format!("{} {} {}", triple[0], triple[1], triple[2]), triple);
                Ok(())
            })
            .unwrap();
        facts
    }
    fn independent(text: &str) -> (BTreeMap<String, [Term; 3]>, BTreeMap<String, [Term; 3]>) {
        let dataset = Dataset::memory();
        dataset.load_str(text, RdfFormat::Turtle).unwrap();
        let asserted = facts(&dataset);
        dataset
            .reasoning()
            .run(&ReasonRequest {
                profile: Profile::RdfsSimple,
                ..Default::default()
            })
            .unwrap();
        (asserted, facts(&dataset))
    }
    let mut seed = 0x6f1742u64;
    for _ in 0..12 {
        let mut states = Vec::new();
        for _ in 0..2 {
            let mut text = String::from(
                "@prefix ex:<http://ex.org/>. @prefix rdfs:<http://www.w3.org/2000/01/rdf-schema#>. ",
            );
            for i in 0..5 {
                for j in 0..5 {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    if seed.is_multiple_of(5) {
                        text.push_str(&format!("ex:C{i} rdfs:subClassOf ex:C{j}. "));
                    }
                }
            }
            for i in 0..4 {
                text.push_str(&format!("ex:i{i} a ex:C{}. ", seed.rotate_left(i * 3) % 5));
            }
            states.push(text);
        }
        let (_dir, dataset) = open();
        dataset.load_str(&states[0], RdfFormat::Turtle).unwrap();
        let from = dataset.store().snapshot().commit;
        dataset
            .store()
            .create_snapshot("before", &sparkles::history::At::Commit(from), None)
            .unwrap();
        dataset.update("CLEAR ALL").unwrap();
        dataset.load_str(&states[1], RdfFormat::Turtle).unwrap();
        let mut req = request(from, "head");
        req.all_changes = true;
        req.sample_limit = 10_000;
        let diff = dataset.reasoning().diff(&req).unwrap();
        let (asserted_a, a) = independent(&states[0]);
        let (asserted_b, b) = independent(&states[1]);
        let expected = |having: &BTreeMap<String, [Term; 3]>,
                        other: &BTreeMap<String, [Term; 3]>,
                        asserted: &BTreeMap<String, [Term; 3]>| {
            having
                .keys()
                .filter(|key| !other.contains_key(*key) && !asserted.contains_key(*key))
                .cloned()
                .collect::<BTreeSet<_>>()
        };
        let actual = |rows: &Vec<[Term; 3]>| {
            rows.iter()
                .map(|row| format!("{} {} {}", row[0], row[1], row[2]))
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(actual(&diff.added_triples), expected(&b, &a, &asserted_b));
        assert_eq!(actual(&diff.removed_triples), expected(&a, &b, &asserted_a));
    }
}

#[test]
fn fresh_materialized_branches_compare_exact_stored_witness_terms() {
    let dataset = Dataset::memory();
    dataset
        .update("INSERT DATA {<urn:s> <urn:p> <urn:t>}")
        .unwrap();
    dataset
        .create_branch("feature", &Default::default())
        .unwrap();
    let feature = dataset.branch("feature").unwrap();
    let request = sparkles::reasoning::ReasonRequest {
        profile: Profile::Rules("[r: (?s <urn:p> <urn:t>) makeTemp(?w) -> (?s <urn:q> ?w)]".into()),
        ..Default::default()
    };
    dataset.reasoning().run(&request).unwrap();
    feature.reasoning().run(&request).unwrap();
    assert_eq!(
        dataset.reasoning().status().unwrap().freshness.stale,
        Some(false)
    );
    assert_eq!(
        feature.reasoning().status().unwrap().freshness.stale,
        Some(false)
    );
    let original = dataset.store().snapshot();
    let proposed = feature.store().snapshot();
    let original_witness = dataset
        .select("SELECT ?w FROM <urn:x-sparkles:inferred> WHERE {<urn:s> <urn:q> ?w}")
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .get("w")
        .unwrap()
        .clone();
    let proposed_witness = feature
        .select("SELECT ?w FROM <urn:x-sparkles:inferred> WHERE {<urn:s> <urn:q> ?w}")
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .get("w")
        .unwrap()
        .clone();
    assert_ne!(original_witness, proposed_witness);
    let mut diff_request = DiffRequest::new("head", "feature");
    diff_request.all_changes = true;
    let diff = dataset.reasoning().diff(&diff_request).unwrap();
    assert_eq!(
        (diff.added, diff.removed),
        (1, 1),
        "statuses {:?} / {:?}, cacheable {} / {}",
        dataset.reasoning().status(),
        feature.reasoning().status(),
        original.cacheable,
        proposed.cacheable
    );
    assert_eq!(diff.added_triples[0][2], proposed_witness);
    assert_eq!(diff.removed_triples[0][2], original_witness);
    assert_eq!(dataset.store().snapshot().commit, original.commit);
    assert_eq!(feature.store().snapshot().commit, proposed.commit);
}

fn clock_materializations(dataset: &Dataset) {
    dataset
        .update("INSERT DATA {<urn:anchor> <urn:anchor-p> <urn:anchor-o>}")
        .unwrap();
    dataset.create_branch("clock", &Default::default()).unwrap();
    let clock = dataset.branch("clock").unwrap();
    clock
        .update("INSERT DATA {<urn:trigger> <urn:p> <urn:o>}")
        .unwrap();
    let request = sparkles::reasoning::ReasonRequest {profile:Profile::Rules("[clock: (<urn:trigger> <urn:p> <urn:o>) now(?t) -> (<urn:clock> <urn:value> ?t) (?t <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2001/XMLSchema#integer>)]".into()),..Default::default()};
    dataset.reasoning().run(&request).unwrap();
    clock.reasoning().run(&request).unwrap();
}

#[test]
fn bounded_stored_typings_keep_the_actual_materialized_clock_values() {
    for persistent in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let dataset = if persistent {
            Dataset::open_with(
                dir.path().join("db"),
                sparkles::DatasetOptions {
                    closure_cache_triples: 0,
                    ..Default::default()
                },
            )
            .unwrap()
        } else {
            Dataset::memory()
        };
        clock_materializations(&dataset);
        let mut request = DiffRequest::new("head", "clock");
        request.analysis.max_memory = Some(32 * 1024 * 1024);
        let diff = dataset.reasoning().diff(&request).unwrap();
        assert!(
            diff.new_findings
                .iter()
                .any(|finding| finding.check == "literal-not-in-datatype"),
            "actual clock typing lost (persistent={persistent}): {:?}",
            diff.new_findings
        );
        assert!(diff.findings_complete);
    }
}

#[test]
fn unavailable_stored_generalized_summary_cannot_certify_absence() {
    let dataset = Dataset::from_store_with(
        sparkles::store::Store::in_memory(Default::default()),
        sparkles::DatasetOptions {
            closure_cache_triples: 0,
            ..Default::default()
        },
    );
    clock_materializations(&dataset);
    let mut request = DiffRequest::new("head", "clock");
    request.analysis.max_memory = Some(32 * 1024 * 1024);
    let diff = dataset.reasoning().diff(&request).unwrap();
    assert!(
        !diff.findings_complete,
        "an unavailable volatile summary was reported complete"
    );
    assert_eq!(diff.added, 1);
}

#[test]
fn pinned_branch_selectors_follow_inherited_history_by_owner_identity() {
    let dataset = Dataset::memory();
    dataset.update("INSERT DATA {<urn:Manager> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:Employee>}").unwrap();
    let base = dataset.store().snapshot().commit;
    dataset
        .create_branch("feature", &Default::default())
        .unwrap();
    let branch = dataset.branch("feature").unwrap();
    branch
        .update("INSERT DATA {<urn:alice> a <urn:Manager>}")
        .unwrap();
    let mut request = DiffRequest::new(format!("feature@{base}"), "feature");
    request.analysis.program = Some(AnalysisProgram {
        profile: Profile::RdfsSimple,
        extras: Default::default(),
        inputs: Default::default(),
    });
    let diff = dataset.reasoning().diff(&request).unwrap();
    assert_eq!((diff.added, diff.removed), (1, 0));
}
