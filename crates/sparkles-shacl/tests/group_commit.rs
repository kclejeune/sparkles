//! Write-time SHACL validation under group commit (spec X09 section 6): commits are
//! checked against the speculative head, a rejection is reported only once the state it
//! was judged against is durable, and a failed fence is reported as poisoning.

use sparkles_core::Error;
use sparkles_core::guard::{GuardMode, Severity};
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions, SyncPath};
use sparkles_shacl::guard::{self, DataGraphSel, SetOutcome, ShapesSource, ValidationConfig};
use std::sync::Arc;

const SHAPES: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ; sh:datatype xsd:string ] .
"#;
const P: &str = "PREFIX ex: <http://ex.org/> ";

fn options() -> StoreOptions {
    StoreOptions {
        experimental_group_commit: true,
        group_commit_idle_direct: false,
        ..Default::default()
    }
}

fn cfg() -> ValidationConfig {
    ValidationConfig {
        format: 1,
        language: None,
        mode: GuardMode::Reject,
        shapes: ShapesSource {
            inline: Some(SHAPES.into()),
            ..Default::default()
        },
        data_graph: DataGraphSel::Named("default".into()),
        include_inferences: false,
        threshold: Severity::Violation,
        baseline: Default::default(),
        timeout_seconds: 60.0,
        report_limit: 100,
        updated: None,
    }
}

fn upd(s: &Store, u: &str) -> sparkles_core::Result<sparkles_core::sparql::update::UpdateStats> {
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

fn guarded(root: &std::path::Path) -> Arc<Store> {
    let s = Arc::new(Store::open(root, options()).unwrap());
    match guard::set_config(&s, Some(cfg())).unwrap() {
        SetOutcome::Installed(..) => {}
        _ => panic!("the shapes are not installed"),
    }
    s
}

/// Writers run at once against a guard. The guard judges each write against the state
/// the writes before it left, whether or not their fence is done, so exactly the
/// conforming ones are kept, whatever the interleaving.
#[test]
fn concurrent_writes_are_judged_against_the_state_before_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = guarded(&root);
    let workers: Vec<_> = (0..8)
        .map(|t| {
            let s = s.clone();
            std::thread::spawn(move || {
                let (mut ok, mut rejected) = (0, 0);
                for k in 0..12 {
                    // every third write names no one, which the shape refuses
                    let u = if k % 3 == 2 {
                        format!("INSERT DATA {{ ex:p{t}_{k} a ex:Person }}")
                    } else {
                        format!("INSERT DATA {{ ex:p{t}_{k} a ex:Person ; ex:name \"n\" }}")
                    };
                    match upd(&s, &u) {
                        Ok(_) => ok += 1,
                        Err(Error::Rejected(_)) => rejected += 1,
                        Err(e) => panic!("{e}"),
                    }
                }
                (ok, rejected)
            })
        })
        .collect();
    let (mut ok, mut rejected) = (0, 0);
    for w in workers {
        let (o, r) = w.join().unwrap();
        ok += o;
        rejected += r;
    }
    assert_eq!((ok, rejected), (64, 32));
    assert_eq!(s.snapshot().commit, 64);
    assert!(!ask(
        &s,
        "?p a ex:Person FILTER NOT EXISTS { ?p ex:name ?n }"
    ));
    let m = s.commit_metrics();
    assert_eq!(m.fallbacks.iter().sum::<u64>(), 0, "{:?}", m.fallbacks);
    let fences = m.sync[SyncPath::Group as usize].count();
    assert!((1..=64).contains(&fences), "{fences} fences");
    drop(s);
    let s = Store::open(&root, Default::default()).unwrap();
    assert_eq!(s.snapshot().commit, 64);
    assert!(!ask(
        &s,
        "?p a ex:Person FILTER NOT EXISTS { ?p ex:name ?n }"
    ));
}

/// A change of the shapes reaches the checks of the writes after it, though its fence may
/// not be done yet when they are judged.
#[test]
fn a_shapes_change_in_flight_applies_to_the_next_write() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Arc::new(Store::open(&root, options()).unwrap());
    // shapes kept in a graph, so that a write can change them
    let mut c = cfg();
    c.shapes = ShapesSource {
        graphs: Some(vec!["urn:shapes".into()]),
        ..Default::default()
    };
    s.load(&[sparkles_core::io::Source::from_bytes(
        SHAPES.as_bytes().to_vec(),
        sparkles_core::io::RdfFormat::Turtle,
        Some(oxrdf::NamedNode::new("urn:shapes").unwrap()),
    )])
    .unwrap();
    match guard::set_config(&s, Some(c)).unwrap() {
        SetOutcome::Installed(..) => {}
        _ => panic!("the shapes are not installed"),
    }
    upd(&s, "INSERT DATA { ex:a a ex:Person ; ex:name \"A\" }").unwrap();
    // a second property: every person needs an email
    upd(
        &s,
        "INSERT DATA { GRAPH <urn:shapes> { ex:PersonShape <http://www.w3.org/ns/shacl#property> [ \
         <http://www.w3.org/ns/shacl#path> ex:email ; <http://www.w3.org/ns/shacl#minCount> 1 ] } }",
    )
    .expect_err("the data has no email yet");
    upd(&s, "INSERT DATA { ex:a ex:email \"a@ex.org\" }").unwrap();
    upd(
        &s,
        "INSERT DATA { GRAPH <urn:shapes> { ex:PersonShape <http://www.w3.org/ns/shacl#property> [ \
         <http://www.w3.org/ns/shacl#path> ex:email ; <http://www.w3.org/ns/shacl#minCount> 1 ] } }",
    )
    .unwrap();
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:b a ex:Person ; ex:name \"B\" }"),
        Err(Error::Rejected(_))
    ));
    upd(
        &s,
        "INSERT DATA { ex:b a ex:Person ; ex:name \"B\" ; ex:email \"b@ex.org\" }",
    )
    .unwrap();
}
