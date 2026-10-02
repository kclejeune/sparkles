//! Write-time SHACL validation: the store's commit guard.

use sparkles::Error;
use sparkles::guard::{GuardMode, GuardStatus, Severity};
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::update::update;
use sparkles::sparql::{QueryOptions, query};
use sparkles::store::{Store, StoreOptions};
use sparkles_shacl::guard::{self, DataGraphSel, SetOutcome, ShapesSource, ValidationConfig};

const SHAPES: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ; sh:datatype xsd:string ] ;
  sh:property [ sh:path ex:age ; sh:maxInclusive 150 ; sh:severity sh:Warning ] .
"#;
const P: &str = "PREFIX ex: <http://ex.org/> ";

fn cfg(mode: GuardMode) -> ValidationConfig {
    ValidationConfig {
        format: 1,
        language: None,
        mode,
        shapes: ShapesSource {
            graphs: Some(vec!["urn:shapes".into()]),
            ..Default::default()
        },
        data_graph: DataGraphSel::Named("default".into()),
        include_inferences: false,
        threshold: Severity::Violation,
        baseline: Default::default(),
        timeout_seconds: 10.0,
        report_limit: 100,
        updated: None,
    }
}

fn store_with_shapes(opts: StoreOptions) -> Store {
    let s = Store::in_memory(opts);
    s.load(&[Source::from_bytes(
        SHAPES.as_bytes().to_vec(),
        RdfFormat::Turtle,
        Some(oxrdf::NamedNode::new("urn:shapes").unwrap()),
    )])
    .unwrap();
    s
}

fn upd(s: &Store, u: &str) -> sparkles::Result<sparkles::sparql::update::UpdateStats> {
    update(s, &format!("{P}{u}"), &QueryOptions::default())
}

fn ask(s: &Store, q: &str) -> bool {
    query(
        s.snapshot(),
        &format!("{P}ASK {{ {q} }}"),
        &QueryOptions::default(),
    )
    .unwrap()
    .boolean
}

fn enable(s: &Store, c: ValidationConfig) {
    match guard::set_config(s, Some(c)).unwrap() {
        SetOutcome::Installed(..) => {}
        SetOutcome::NotConforming(sum) => panic!("not conforming: {:?}", sum.results),
        SetOutcome::Removed => panic!("removed"),
    }
}

#[test]
fn reject_mode_keeps_the_data_conforming() {
    let s = store_with_shapes(StoreOptions::default());
    enable(&s, cfg(GuardMode::Reject));
    // A2: a conforming write passes, and its receipt says so
    let st = upd(&s, "INSERT DATA { ex:a a ex:Person ; ex:name \"A\" }").unwrap();
    let v = st.commit.as_ref().unwrap().validation.clone().unwrap();
    assert_eq!(v.status, GuardStatus::Passed);
    let head = s.head_commit().seq;
    // A3: a violating write is rejected, and no commit number is used
    let Err(Error::Rejected(r)) = upd(&s, "INSERT DATA { ex:b a ex:Person }") else {
        panic!("rejected")
    };
    assert_eq!(r.summary.blocking, 1);
    assert_eq!(r.head, head);
    assert!(r.summary.results[0]["focusNode"]["value"] == "http://ex.org/b");
    assert!(
        r.summary
            .report_turtle
            .as_deref()
            .unwrap()
            .contains("ValidationReport")
    );
    assert!(!ask(&s, "ex:b ?p ?o"));
    assert_eq!(s.head_commit().seq, head);
    // A4: validated once per request, on the final state
    upd(
        &s,
        "INSERT DATA { ex:c a ex:Person } ; INSERT DATA { ex:c ex:name \"C\" }",
    )
    .unwrap();
    assert!(matches!(
        upd(
            &s,
            "DELETE DATA { ex:a ex:name \"A\" } ; INSERT DATA { ex:x ex:p 1 }"
        ),
        Err(Error::Rejected(_))
    ));
    assert!(ask(&s, "ex:a ex:name \"A\"") && !ask(&s, "ex:x ex:p 1"));
    // warnings do not block at the violation threshold
    let st = upd(&s, "INSERT DATA { ex:a ex:age 200 }").unwrap();
    let v = st.commit.unwrap().validation.unwrap();
    assert_eq!(
        (v.status, v.blocking, v.by_severity.warning),
        (GuardStatus::Passed, 0, 1)
    );
}

#[test]
fn enabling_reject_needs_a_conforming_head_and_warn_commits() {
    let s = store_with_shapes(StoreOptions::default());
    upd(&s, "INSERT DATA { ex:d a ex:Person }").unwrap();
    // A6: the head does not conform
    assert!(matches!(
        guard::set_config(&s, Some(cfg(GuardMode::Reject))).unwrap(),
        SetOutcome::NotConforming(_)
    ));
    // A7: warn mode commits and reports
    enable(&s, cfg(GuardMode::Warn));
    let st = upd(&s, "INSERT DATA { ex:e a ex:Person }").unwrap();
    let v = st.commit.unwrap().validation.unwrap();
    assert_eq!((v.status, v.blocking), (GuardStatus::Warned, 2));
    assert!(ask(&s, "ex:e a ex:Person"));
    // off removes the guard
    assert!(matches!(
        guard::set_config(&s, None).unwrap(),
        SetOutcome::Removed
    ));
    let st = upd(&s, "INSERT DATA { ex:f a ex:Person }").unwrap();
    assert!(st.commit.unwrap().validation.is_none());
}

#[test]
fn shapes_changes_are_validated_and_unrelated_graphs_skipped() {
    let s = store_with_shapes(StoreOptions::default());
    upd(&s, "INSERT DATA { ex:a a ex:Person ; ex:name \"A\" }").unwrap();
    enable(&s, cfg(GuardMode::Reject));
    // A10: a new constraint the data violates is rejected
    let add = "INSERT DATA { GRAPH <urn:shapes> { ex:PersonShape <http://www.w3.org/ns/shacl#property> [ <http://www.w3.org/ns/shacl#path> ex:email ; <http://www.w3.org/ns/shacl#minCount> 1 ] } }";
    assert!(matches!(upd(&s, add), Err(Error::Rejected(_))));
    // a malformed shape is rejected with the parse error
    let bad = "INSERT DATA { GRAPH <urn:shapes> { ex:S a <http://www.w3.org/ns/shacl#NodeShape> ; <http://www.w3.org/ns/shacl#targetClass> ex:Person ; <http://www.w3.org/ns/shacl#property> [ <http://www.w3.org/ns/shacl#path> \"not a path\" ; <http://www.w3.org/ns/shacl#minCount> 1 ] } }";
    let Err(Error::Rejected(r)) = upd(&s, bad) else {
        panic!("rejected")
    };
    assert!(r.summary.shapes_error.is_some(), "{r}");
    // A11: a graph outside the data graph is skipped
    let st = upd(&s, "INSERT DATA { GRAPH <urn:other> { ex:z a ex:Person } }").unwrap();
    assert_eq!(
        st.commit.unwrap().validation.unwrap().status,
        GuardStatus::Skipped
    );
    // dropping the shapes is allowed
    upd(&s, "DROP GRAPH <urn:shapes>").unwrap();
    upd(&s, "INSERT DATA { ex:q a ex:Person }").unwrap();
}

#[test]
fn bulk_loads_are_validated_before_the_switch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(
        &root,
        StoreOptions {
            bulk_threshold: 10,
            ..Default::default()
        },
    )
    .unwrap();
    s.load(&[Source::from_bytes(
        SHAPES.as_bytes().to_vec(),
        RdfFormat::Turtle,
        Some(oxrdf::NamedNode::new("urn:shapes").unwrap()),
    )])
    .unwrap();
    enable(&s, cfg(GuardMode::Reject));
    let gens = || -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("gen-"))
            .collect();
        v.sort();
        v
    };
    let (head, len, before) = (s.head_commit().seq, s.snapshot().len(), gens());
    // A9: 100 persons, one without a name
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..100 {
        ttl.push_str(&format!("ex:p{i} a ex:Person"));
        ttl.push_str(if i == 50 {
            " .\n"
        } else {
            " ; ex:name \"n\" .\n"
        });
    }
    let src = |t: &str| Source::from_bytes(t.as_bytes().to_vec(), RdfFormat::Turtle, None);
    assert!(matches!(s.load(&[src(&ttl)]), Err(Error::Rejected(_))));
    assert_eq!(
        (s.head_commit().seq, s.snapshot().len(), gens()),
        (head, len, before)
    );
    let fixed = ttl.replace(
        "ex:p50 a ex:Person .",
        "ex:p50 a ex:Person ; ex:name \"n\" .",
    );
    let r = s
        .load_as(&[src(&fixed)], sparkles::commit::CommitKind::Load)
        .unwrap();
    assert!(r.commit.bulk);
    let v = r.validation.unwrap();
    // a rebuilt generation renumbers terms: validated in full
    assert_eq!(
        (v.status, v.fallback.as_deref()),
        (GuardStatus::Passed, Some("bulk"))
    );
}

#[test]
fn validated_databases_fail_closed_without_a_guard() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        s.load(&[Source::from_bytes(
            SHAPES.as_bytes().to_vec(),
            RdfFormat::Turtle,
            Some(oxrdf::NamedNode::new("urn:shapes").unwrap()),
        )])
        .unwrap();
        enable(&s, cfg(GuardMode::Reject));
    }
    assert!(root.join(guard::CONFIG_FILE).exists());
    // A14: no guard installed: writes fail, reads work
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:a a ex:Person ; ex:name \"A\" }"),
        Err(Error::GuardMissing(_))
    ));
    assert!(!ask(&s, "ex:a ?p ?o"));
    drop(s);
    let s = Store::open(
        &root,
        StoreOptions {
            unvalidated_writes: true,
            ..Default::default()
        },
    )
    .unwrap();
    upd(&s, "INSERT DATA { ex:a a ex:Person ; ex:name \"A\" }").unwrap();
    drop(s);
    // installed from validation.json: violations are rejected again
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert!(guard::install(&s).unwrap().is_some());
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:b a ex:Person }"),
        Err(Error::Rejected(_))
    ));
    // a bypass is allowed and counted
    let opts = QueryOptions {
        write: sparkles::guard::WriteOptions {
            bypass_validation: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let st = update(&s, &format!("{P}INSERT DATA {{ ex:b a ex:Person }}"), &opts).unwrap();
    assert_eq!(
        st.commit.unwrap().validation.unwrap().status,
        GuardStatus::Bypassed
    );
}

#[test]
fn inline_shapes_file_and_union_data_graph() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let mut c = cfg(GuardMode::Reject);
    c.shapes = ShapesSource {
        inline: Some(SHAPES.into()),
        ..Default::default()
    };
    c.data_graph = DataGraphSel::Named("union".into());
    enable(&s, c);
    assert!(root.join(guard::SHAPES_FILE).exists());
    let named = "INSERT DATA { GRAPH <urn:g1> { ex:b a ex:Person } }";
    assert!(matches!(upd(&s, named), Err(Error::Rejected(_))));
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    guard::install(&s).unwrap();
    assert!(matches!(upd(&s, named), Err(Error::Rejected(_))));
}

#[test]
fn configurations_are_written_as_format_2_and_format_1_still_reads() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let mut c = cfg(GuardMode::Reject);
    c.shapes = ShapesSource {
        inline: Some(SHAPES.into()),
        ..Default::default()
    };
    // given as format 1: written as format 2 with the language
    enable(&s, c);
    let j: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join(guard::CONFIG_FILE)).unwrap()).unwrap();
    assert_eq!(j["format"], 2);
    assert_eq!(j["language"], "shacl");
    drop(s);
    // a format 1 file (an older Sparkles wrote it) installs and rejects as before
    std::fs::write(
        root.join(guard::CONFIG_FILE),
        r#"{"format":1,"mode":"reject","shapes":{"file":"validation-shapes.ttl"}}"#,
    )
    .unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let g = guard::install(&s).unwrap().unwrap();
    assert_eq!((g.config().format, g.config().language), (1, None));
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:b a ex:Person }"),
        Err(Error::Rejected(_))
    ));
}

const UNIQUE: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
ex:EmailUnique a sh:NodeShape ; sh:targetObjectsOf ex:email ;
  sh:property [ sh:path [ sh:inversePath ex:email ] ; sh:maxCount 1 ] .
"#;

fn inline(mode: GuardMode, shapes: &str) -> ValidationConfig {
    let mut c = cfg(mode);
    c.shapes = ShapesSource {
        inline: Some(shapes.into()),
        ..Default::default()
    };
    c
}

fn summary(st: sparkles::sparql::update::UpdateStats) -> sparkles::guard::ValidationSummary {
    (*st.commit.unwrap().validation.unwrap()).clone()
}

#[test]
fn writes_validate_only_the_focus_nodes_they_affect() {
    use sparkles::guard::Strategy;
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..2000 {
        data.push_str(&format!("ex:p{i} ex:email \"p{i}@ex.org\" .\n"));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    enable(&s, inline(GuardMode::Reject, UNIQUE));
    // A16: one new email is one focus node, validated incrementally
    let v = summary(upd(&s, "INSERT DATA { ex:q ex:email \"q@ex.org\" }").unwrap());
    assert_eq!(
        (v.status, v.strategy, v.focus_nodes),
        (GuardStatus::Passed, Strategy::Incremental, Some(1))
    );
    // a duplicate is rejected, with the result at the shared value
    let Err(Error::Rejected(r)) = upd(&s, "INSERT DATA { ex:z ex:email \"p7@ex.org\" }") else {
        panic!("rejected")
    };
    assert_eq!(r.summary.strategy, Strategy::Incremental);
    assert_eq!(r.summary.results[0]["focusNode"]["value"], "p7@ex.org");
    // a write no shape reads is skipped
    let v = summary(upd(&s, "INSERT DATA { ex:q ex:name \"Q\" }").unwrap());
    assert_eq!(
        (v.status, v.strategy),
        (GuardStatus::Skipped, Strategy::None)
    );
    // deleting an email affects its value, which is no longer a focus node
    let v = summary(upd(&s, "DELETE DATA { ex:p3 ex:email \"p3@ex.org\" }").unwrap());
    assert_eq!(
        (v.strategy, v.focus_nodes),
        (Strategy::Incremental, Some(0))
    );
}

#[test]
fn warn_mode_keeps_exact_counts_across_writes() {
    use sparkles::guard::Strategy;
    let s = store_with_shapes(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA { ex:a a ex:Person . ex:b a ex:Person ; ex:age 200 }",
    )
    .unwrap();
    enable(&s, cfg(GuardMode::Warn));
    // two missing names and one warning; the new person adds a third violation
    let v = summary(upd(&s, "INSERT DATA { ex:c a ex:Person }").unwrap());
    assert_eq!(v.strategy, Strategy::Incremental);
    assert_eq!(
        (v.status, v.blocking, v.by_severity.warning, v.results.len()),
        (GuardStatus::Warned, 3, 1, 1)
    );
    // naming two of them leaves one
    let v = summary(
        upd(
            &s,
            "INSERT DATA { ex:a ex:name \"A\" . ex:c ex:name \"C\" }",
        )
        .unwrap(),
    );
    assert_eq!(
        (v.blocking, v.total, v.strategy),
        (1, 2, Strategy::Incremental)
    );
    let b = match guard::set_config(&s, Some(cfg(GuardMode::Warn))).unwrap() {
        SetOutcome::Installed(_, sum) => sum,
        _ => panic!("installed"),
    };
    assert_eq!((b.blocking, b.total), (1, 2));
}

#[test]
fn the_state_of_the_head_survives_a_restart() {
    use sparkles::guard::Strategy;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        enable(&s, inline(GuardMode::Warn, SHAPES));
        upd(&s, "INSERT DATA { ex:a a ex:Person }").unwrap();
    }
    assert!(root.join(guard::STATUS_FILE).exists());
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let g = guard::install(&s).unwrap().unwrap();
    let b = g.status().baseline.unwrap();
    assert_eq!((b.conforms, b.blocking), (Some(false), 1));
    // known: the first write after the restart is incremental
    let v = summary(upd(&s, "INSERT DATA { ex:b a ex:Person }").unwrap());
    assert_eq!((v.strategy, v.blocking), (Strategy::Incremental, 2));
    drop((g, s));
    // a commit the status file does not record leaves the state unknown
    let s = Store::open(
        &root,
        StoreOptions {
            unvalidated_writes: true,
            ..Default::default()
        },
    )
    .unwrap();
    upd(&s, "INSERT DATA { ex:c a ex:Person }").unwrap();
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let g = guard::install(&s).unwrap().unwrap();
    assert!(g.status().baseline.is_none());
    let v = summary(upd(&s, "INSERT DATA { ex:d a ex:Person }").unwrap());
    assert_eq!(
        (v.strategy, v.fallback.as_deref(), v.blocking),
        (Strategy::Full, Some("baseline"), 4)
    );
    // turning validation off removes the state
    guard::set_config(&s, None).unwrap();
    assert!(!root.join(guard::STATUS_FILE).exists());
}

#[test]
fn grandfather_mode_blocks_only_new_results() {
    use sparkles::guard::Strategy;
    use sparkles_shacl::guard::BaselinePolicy;
    let s = store_with_shapes(StoreOptions::default());
    upd(&s, "INSERT DATA { ex:a a ex:Person }").unwrap();
    let mut c = cfg(GuardMode::Reject);
    c.baseline = BaselinePolicy::Grandfather;
    // reject is enabled on a head that does not conform
    let sum = match guard::set_config(&s, Some(c)).unwrap() {
        SetOutcome::Installed(_, sum) => sum,
        _ => panic!("installed"),
    };
    assert_eq!(
        (sum.status, sum.blocking, sum.introduced),
        (GuardStatus::Passed, 1, Some(0))
    );
    // an unrelated write passes although ex:a still has no name
    let v = summary(upd(&s, "INSERT DATA { ex:b a ex:Person ; ex:name \"B\" }").unwrap());
    assert_eq!(
        (v.status, v.strategy, v.blocking, v.introduced),
        (GuardStatus::Passed, Strategy::Incremental, 1, Some(0))
    );
    // a write that adds a violation is rejected, and says so
    let Err(Error::Rejected(r)) = upd(&s, "INSERT DATA { ex:c a ex:Person }") else {
        panic!("rejected")
    };
    assert_eq!((r.summary.blocking, r.summary.introduced), (2, Some(1)));
    assert!(r.to_string().contains("1 new blocking result"), "{r}");
    assert_eq!(
        r.summary.results[0]["focusNode"]["value"],
        "http://ex.org/c"
    );
    // a shapes change compares with the results the old shapes gave
    let add = "INSERT DATA { GRAPH <urn:shapes> { ex:PersonShape <http://www.w3.org/ns/shacl#property> [ <http://www.w3.org/ns/shacl#path> ex:email ; <http://www.w3.org/ns/shacl#minCount> 1 ] } }";
    let Err(Error::Rejected(r)) = upd(&s, add) else {
        panic!("rejected")
    };
    assert_eq!(
        (r.summary.strategy, r.summary.introduced),
        (Strategy::Full, Some(2))
    );
}
