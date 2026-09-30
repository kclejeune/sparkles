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
        mode,
        shapes: ShapesSource {
            graphs: Some(vec!["urn:shapes".into()]),
            ..Default::default()
        },
        data_graph: DataGraphSel::Named("default".into()),
        include_inferences: false,
        threshold: Severity::Violation,
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
    assert_eq!(r.validation.unwrap().status, GuardStatus::Passed);
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
