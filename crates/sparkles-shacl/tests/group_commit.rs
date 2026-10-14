//! Write-time SHACL validation under group commit (spec X09 section 6): commits are
//! checked against the speculative head, so each write is judged against the writes
//! before it whether or not their fence is done, and a change of the shapes in flight
//! applies to the writes after it.

use sparkles_core::Error;
use sparkles_core::guard::{GuardMode, Severity};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_shacl::guard::{self, DataGraphSel, SetOutcome, ShapesSource, ValidationConfig};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// A hub that may have at most three links, a limit that holds across writes.
const HUB: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
ex:HubShape a sh:NodeShape ; sh:targetNode ex:hub ;
  sh:property [ sh:path ex:link ; sh:maxCount 3 ] .
"#;
const PERSON: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ] .
"#;
const P: &str = "PREFIX ex: <http://ex.org/> ";

fn options() -> StoreOptions {
    StoreOptions {
        experimental_group_commit: true,
        group_commit_idle_direct: false,
        ..Default::default()
    }
}

fn cfg(shapes: ShapesSource) -> ValidationConfig {
    ValidationConfig {
        format: 1,
        language: None,
        mode: GuardMode::Reject,
        shapes,
        data_graph: DataGraphSel::Named("default".into()),
        include_inferences: false,
        threshold: Severity::Violation,
        baseline: Default::default(),
        timeout_seconds: 60.0,
        report_limit: 100,
        updated: None,
    }
}

fn inline(shapes: &str) -> ShapesSource {
    ShapesSource {
        inline: Some(shapes.into()),
        ..Default::default()
    }
}

fn upd(s: &Store, u: &str) -> sparkles_core::Result<sparkles_core::sparql::update::UpdateStats> {
    update(s, &format!("{P}{u}"), &QueryOptions::default())
}

fn count(s: &Store, q: &str) -> usize {
    query(
        s.snapshot(),
        &format!("{P}SELECT * {{ {q} }}"),
        &QueryOptions::default(),
    )
    .unwrap()
    .len()
}

fn install(s: &Store, shapes: ShapesSource) {
    match guard::set_config(s, Some(cfg(shapes))).unwrap() {
        SetOutcome::Installed(..) => {}
        _ => panic!("the shapes are not installed"),
    }
}

/// Eight writers add links to one hub that may hold three. Whatever the interleaving and
/// however the fences fall, exactly three writes are kept, so each was judged against the
/// state the writes before it left.
#[test]
fn a_limit_across_concurrent_writes_holds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Arc::new(Store::open(&root, options()).unwrap());
    install(&s, inline(HUB));
    let workers: Vec<_> = (0..8)
        .map(|t| {
            let s = s.clone();
            std::thread::spawn(move || {
                let (mut ok, mut rejected) = (0, 0);
                for k in 0..4 {
                    match upd(&s, &format!("INSERT DATA {{ ex:hub ex:link ex:x{t}_{k} }}")) {
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
    assert_eq!((ok, rejected), (3, 29));
    assert_eq!(s.snapshot().commit, 3);
    assert_eq!(count(&s, "ex:hub ex:link ?x"), 3);
    let m = s.commit_metrics();
    assert_eq!(m.fallbacks.iter().sum::<u64>(), 0, "{:?}", m.fallbacks);
    drop(s);
    let s = Store::open(&root, Default::default()).unwrap();
    assert_eq!(count(&s, "ex:hub ex:link ?x"), 3);
}

/// A write that tightens the shapes is in flight, its fence paused, when the next write is
/// checked. The next write meets the new shapes.
#[test]
fn a_shapes_change_in_flight_applies_to_the_next_write() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Arc::new(Store::open(&root, options()).unwrap());
    s.load(&[Source::from_bytes(
        PERSON.as_bytes().to_vec(),
        RdfFormat::Turtle,
        Some(oxrdf::NamedNode::new("urn:shapes").unwrap()),
    )])
    .unwrap();
    install(
        &s,
        ShapesSource {
            graphs: Some(vec!["urn:shapes".into()]),
            ..Default::default()
        },
    );
    upd(
        &s,
        "INSERT DATA { ex:a a ex:Person ; ex:name \"A\" ; ex:email \"a@ex.org\" }",
    )
    .unwrap();
    // the first write's fence waits
    let (hit_tx, hit) = mpsc::channel();
    let (go_tx, go) = mpsc::channel::<()>();
    let go = std::sync::Mutex::new(go);
    let first = AtomicBool::new(true);
    s.set_failpoint(
        "group-sealed",
        Some(Arc::new(move |_| {
            if first.swap(false, Ordering::SeqCst) {
                hit_tx.send(()).unwrap();
                let _ = go.lock().unwrap().recv_timeout(Duration::from_secs(10));
            }
        })),
    );
    let held = {
        let s = s.clone();
        std::thread::spawn(move || {
            upd(
                &s,
                "INSERT DATA { ex:c a ex:Person ; ex:name \"C\" ; ex:email \"c@ex.org\" }",
            )
        })
    };
    hit.recv_timeout(Duration::from_secs(10)).unwrap();
    // every person needs an email now, which the data satisfies
    let tighten = {
        let s = s.clone();
        std::thread::spawn(move || {
            upd(
                &s,
                "INSERT DATA { GRAPH <urn:shapes> { ex:PersonShape <http://www.w3.org/ns/shacl#property> \
                 [ <http://www.w3.org/ns/shacl#path> ex:email ; <http://www.w3.org/ns/shacl#minCount> 1 ] } }",
            )
        })
    };
    std::thread::sleep(Duration::from_millis(300));
    // checked against the shapes of the write in flight, and refused once its base is durable
    let late = {
        let s = s.clone();
        std::thread::spawn(move || upd(&s, "INSERT DATA { ex:b a ex:Person ; ex:name \"B\" }"))
    };
    std::thread::sleep(Duration::from_millis(300));
    go_tx.send(()).unwrap();
    held.join().unwrap().unwrap();
    tighten.join().unwrap().unwrap();
    assert!(matches!(late.join().unwrap(), Err(Error::Rejected(_))));
    s.set_failpoint("group-sealed", None);
    assert_eq!(count(&s, "ex:b ?p ?o"), 0);
    upd(
        &s,
        "INSERT DATA { ex:b a ex:Person ; ex:name \"B\" ; ex:email \"b@ex.org\" }",
    )
    .unwrap();
}
