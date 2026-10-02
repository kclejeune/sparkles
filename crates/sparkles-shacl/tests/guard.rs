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

/// A uniqueness constraint in SHACL-SPARQL is anchored at the focus node: a write
/// validates the nodes that share the changed value, not every node.
const UNIQUE_SPARQL: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
ex:KeyUnique a sh:NodeShape ; sh:targetSubjectsOf ex:key ;
  sh:sparql [ sh:select """
    SELECT $this ?value WHERE {
      $this <http://ex.org/key> ?value . ?other <http://ex.org/key> ?value .
      FILTER (?other != $this)
    }""" ] .
"#;

#[test]
fn anchored_sparql_constraints_are_validated_incrementally() {
    use sparkles::guard::Strategy;
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..300 {
        data.push_str(&format!("ex:n{i} ex:key \"k{i}\" .\n"));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let g = match guard::set_config(&s, Some(inline(GuardMode::Reject, UNIQUE_SPARQL))).unwrap() {
        SetOutcome::Installed(g, _) => g,
        _ => panic!("installed"),
    };
    let st = g.status();
    assert!(st.incremental.full_shapes.is_empty(), "{st:?}");
    let v = summary(upd(&s, "INSERT DATA { ex:m ex:key \"new\" }").unwrap());
    assert_eq!(
        (v.status, v.strategy, v.fallback.as_deref(), v.focus_nodes),
        (GuardStatus::Passed, Strategy::Incremental, None, Some(1))
    );
    // a duplicate key: both holders are validated, and both have a result
    let Err(Error::Rejected(r)) = upd(&s, "INSERT DATA { ex:z ex:key \"k7\" }") else {
        panic!("rejected")
    };
    assert_eq!(
        (
            r.summary.strategy,
            r.summary.blocking,
            r.summary.focus_nodes
        ),
        (Strategy::Incremental, 2, Some(2))
    );
    // a query that is not anchored is validated in full on every write
    let global = UNIQUE_SPARQL.replace("$this <http://ex.org/key> ?value .", "");
    let g = match guard::set_config(&s, Some(inline(GuardMode::Warn, &global))).unwrap() {
        SetOutcome::Installed(g, _) => g,
        _ => panic!("installed"),
    };
    assert_eq!(g.status().incremental.full_shapes[0].reason, "sparql");
}

#[test]
fn where_targets_select_the_nodes_that_conform() {
    use sparkles::guard::Strategy;
    let shapes = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
ex:AdultPerson a sh:NodeShape ;
  sh:targetWhere [ sh:class ex:Person ;
    sh:property [ sh:path ex:age ; sh:minInclusive 18 ; sh:minCount 1 ] ] ;
  sh:property [ sh:path ex:licence ; sh:minCount 1 ] .
"#;
    let parsed = sparkles_shacl::Shapes::parse(shapes, RdfFormat::Turtle, None).unwrap();
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA { ex:alice a ex:Person . ex:bob a ex:Person ; ex:age 21 ; ex:licence \"B\" . ex:carl a ex:Person ; ex:age 12 }",
    )
    .unwrap();
    // the specification's example: only bob is a focus node, and he conforms
    let report = sparkles_shacl::validate(&s.snapshot(), &parsed, &Default::default()).unwrap();
    assert!(report.conforms, "{:?}", report.results);
    enable(&s, inline(GuardMode::Reject, shapes));
    // carl turns 18 and becomes a focus node without a licence
    let Err(Error::Rejected(r)) = upd(
        &s,
        "DELETE DATA { ex:carl ex:age 12 } ; INSERT DATA { ex:carl ex:age 18 }",
    ) else {
        panic!("rejected")
    };
    assert_eq!(
        (r.summary.strategy, r.summary.blocking),
        (Strategy::Incremental, 1)
    );
    assert_eq!(
        r.summary.results[0]["focusNode"]["value"],
        "http://ex.org/carl"
    );
    // a where target that does not narrow the nodes reads every edge of the focus node:
    // the new nodes ex:x and "y" are validated, and the literals "B", 21, 12 and "y"
    // are not IRIs
    let open = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
ex:NotPerson a sh:NodeShape ; sh:targetWhere [ sh:not [ sh:class ex:Person ] ] ;
  sh:nodeKind sh:IRI .
"#;
    enable(&s, inline(GuardMode::Warn, open));
    let v = summary(upd(&s, "INSERT DATA { ex:x ex:other \"y\" }").unwrap());
    assert_eq!(
        (v.strategy, v.focus_nodes, v.blocking),
        (Strategy::Incremental, Some(2), 4)
    );
}

#[test]
fn subclass_changes_validate_the_instances_they_affect() {
    use sparkles::guard::Strategy;
    let shapes = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
ex:AgentShape a sh:NodeShape ; sh:targetClass ex:Agent ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ] .
"#;
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA { ex:a a ex:Robot . ex:b a ex:Robot ; ex:name \"B\" . ex:c a ex:Agent ; ex:name \"C\" }",
    )
    .unwrap();
    enable(&s, inline(GuardMode::Warn, shapes));
    // robots become agents: the two robots are validated, and one has no name
    let v = summary(
        upd(
            &s,
            "INSERT DATA { ex:Robot <http://www.w3.org/2000/01/rdf-schema#subClassOf> ex:Agent }",
        )
        .unwrap(),
    );
    assert_eq!(
        (v.strategy, v.fallback.as_deref(), v.blocking, v.focus_nodes),
        (Strategy::Incremental, None, 1, Some(2))
    );
}

#[test]
fn shapes_from_graphs_and_a_file_are_merged() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.load(&[Source::from_bytes(
        SHAPES.as_bytes().to_vec(),
        RdfFormat::Turtle,
        Some(oxrdf::NamedNode::new("urn:shapes").unwrap()),
    )])
    .unwrap();
    let mut c = cfg(GuardMode::Reject);
    c.shapes.inline = Some(UNIQUE.into());
    enable(&s, c);
    assert!(root.join(guard::SHAPES_FILE).exists());
    // the graph's shapes and the file's both apply
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:p a ex:Person }"),
        Err(Error::Rejected(_))
    ));
    upd(
        &s,
        "INSERT DATA { ex:p a ex:Person ; ex:name \"P\" ; ex:email \"e\" }",
    )
    .unwrap();
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:q ex:email \"e\" }"),
        Err(Error::Rejected(_))
    ));
    // a change to the shapes graph is read with the file's shapes
    let add = "INSERT DATA { GRAPH <urn:shapes> { ex:PersonShape <http://www.w3.org/ns/shacl#property> [ <http://www.w3.org/ns/shacl#path> ex:email ; <http://www.w3.org/ns/shacl#minCount> 1 ] } }";
    upd(&s, add).unwrap();
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:q ex:email \"e\" }"),
        Err(Error::Rejected(_))
    ));
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:r a ex:Person ; ex:name \"R\" }"),
        Err(Error::Rejected(_))
    ));
    drop(s);
    // the configuration names both, and reinstalls
    let stored = guard::read_config(&root).unwrap().unwrap();
    assert!(stored.shapes.graphs.is_some() && stored.shapes.file.is_some());
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    guard::install(&s).unwrap().unwrap();
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:q ex:email \"e\" }"),
        Err(Error::Rejected(_))
    ));
    upd(
        &s,
        "INSERT DATA { ex:r a ex:Person ; ex:name \"R\" ; ex:email \"r\" }",
    )
    .unwrap();
}

/// A write that bypasses validation is flagged in the commit catalog, which keeps the
/// flag across a restart and rebuilds it from the WAL.
#[test]
fn bypassed_writes_are_flagged_in_the_commit_log() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let catalog = |root: &std::path::Path| -> Vec<(u64, bool)> {
        let (_, cs) = sparkles::commit::read_catalog(&root.join("commits.bin"))
            .unwrap()
            .unwrap();
        cs.iter().map(|c| (c.seq, c.unvalidated)).collect()
    };
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        enable(&s, inline(GuardMode::Reject, SHAPES));
        let bypass = QueryOptions {
            write: sparkles::guard::WriteOptions {
                bypass_validation: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let st = update(
            &s,
            &format!("{P}INSERT DATA {{ ex:x a ex:Person }}"),
            &bypass,
        )
        .unwrap();
        let c = st.commit.unwrap();
        assert!(c.commit.unvalidated);
        assert_eq!(
            serde_json::to_value(&c).unwrap()["commit"]["unvalidated"],
            true
        );
        let st = upd(&s, "DELETE DATA { ex:x a ex:Person }").unwrap();
        assert!(!st.commit.unwrap().commit.unvalidated);
    }
    // a library write to a validated database without its guard is a bypass too
    {
        let s = Store::open(
            &root,
            StoreOptions {
                unvalidated_writes: true,
                ..Default::default()
            },
        )
        .unwrap();
        let st = upd(&s, "INSERT DATA { ex:y a ex:Person }").unwrap();
        let c = st.commit.unwrap();
        assert!(c.commit.unvalidated);
        assert_eq!(
            c.validation.unwrap().status,
            sparkles::guard::GuardStatus::Bypassed
        );
    }
    let flags = catalog(&root);
    let n = flags.len();
    assert_eq!(
        flags[n - 3..].iter().map(|x| x.1).collect::<Vec<_>>(),
        [true, false, true]
    );
    // the WAL records the flag: a rebuilt catalog has it
    std::fs::remove_file(root.join("commits.bin")).unwrap();
    drop(
        Store::open(
            &root,
            StoreOptions {
                unvalidated_writes: true,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    assert_eq!(catalog(&root), flags);
}
