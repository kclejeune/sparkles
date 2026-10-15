//! Graph-algorithm runs as requests, and write-back in batches (spec F14 §2.5, A11).

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::algorun::{
    AlgoRequest, ProjectionRef, WriteMode, WriteSpec, inline, query_text, run_write,
};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::task::{Control, Progress};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const DATA: &str = "@prefix ex: <http://ex/> .
ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:a . ex:c ex:p ex:d .
ex:d ex:p ex:e . ex:e ex:p ex:d . ex:f ex:p ex:g . ex:h a ex:N .";

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn prefixes() -> Vec<(String, String)> {
    vec![("ex".into(), "http://ex/".into())]
}

fn request(
    algorithm: &str,
    predicates: BTreeMap<String, String>,
    batch: Option<usize>,
) -> AlgoRequest {
    AlgoRequest {
        algorithm: algorithm.into(),
        projection: Some(inline(&["ex:p"])),
        params: Default::default(),
        write: Some(WriteSpec {
            graph: "urn:graph:out".into(),
            mode: WriteMode::Replace,
            predicates,
            batch_size: batch,
            atomic: false,
        }),
    }
}

fn select(s: &Store, q: &str) -> Vec<Vec<String>> {
    let r = query(
        s.snapshot(),
        &format!(
            "PREFIX ex: <http://ex/> PREFIX spk: <urn:x-sparkles:> PREFIX prov: <http://www.w3.org/ns/prov#> {q}"
        ),
        &QueryOptions::default(),
    )
    .unwrap();
    r.rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| match t {
                    Some(oxrdf::Term::Literal(l)) => l.value().to_string(),
                    Some(oxrdf::Term::NamedNode(n)) => n.as_str().to_string(),
                    Some(t) => t.to_string(),
                    None => "-".into(),
                })
                .collect()
        })
        .collect()
}

fn destination_only() -> QueryOptions {
    use sparkles_core::access::{GraphAccess, GraphRule, Graphs};
    QueryOptions {
        graphs: Some(Arc::new(GraphAccess {
            read: Graphs::All,
            write: Graphs::Only(GraphRule::new(["urn:graph:out"], &[])),
            triples: None,
        })),
        ..Default::default()
    }
}

fn batch_record(s: &Store) -> Vec<Vec<String>> {
    select(
        s,
        "SELECT ?run ?b ?t (COUNT(?n) AS ?nodes) WHERE { GRAPH <urn:graph:out> { \
         ?run a spk:AlgorithmRun ; spk:batchesWritten ?b ; spk:triplesWritten ?t . \
         ?n ex:rank ?rank } } GROUP BY ?run ?b ?t",
    )
}

fn counters_stay_in_the_destination(mode: WriteMode, restricted: bool) {
    let s = Arc::new(store());
    let before = s.snapshot().commit;
    let (start_tx, start_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let writer_store = s.clone();
    let writer = std::thread::spawn(move || -> Result<String, String> {
        start_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|e| format!("batch boundary not reached: {e}"))?;
        // One snapshot witnesses both counters and their data after batch 1 commits.
        let first = batch_record(&writer_store);
        if first.len() != 1 || first[0][1..] != ["1", "2", "2"] {
            return Err(format!("batch 1 was not atomically published: {first:?}"));
        }
        let run = first[0][0].clone();
        let result = sparkles_core::sparql::update::update(
            &writer_store,
            &format!(
                "INSERT DATA {{ GRAPH <urn:graph:other> {{ <{run}> \
                 <urn:x-sparkles:batchesWritten> 777 ; \
                 <urn:x-sparkles:triplesWritten> 888 }} }}"
            ),
            &QueryOptions {
                write: sparkles_core::guard::WriteOptions {
                    deadline: Some(Instant::now() + Duration::from_secs(10)),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .map(|_| ())
        .map_err(|e| e.to_string());
        // Sending only follows the competing writer's actual committed update.
        let _ = done_tx.send(result.clone());
        result?;
        Ok(run)
    });
    let callback_error = Arc::new(Mutex::new(None));
    let callback_error2 = callback_error.clone();
    let done_rx = Mutex::new(done_rx);
    let ctl = Control::none();
    let cancel = ctl.cancel.clone();
    let ctl = Control {
        deadline: Some(Instant::now() + Duration::from_secs(20)),
        progress: Progress::new(move |_, message| {
            if !message.starts_with("writing batch 2 ") {
                return;
            }
            let result = start_tx.send(()).map_err(|e| e.to_string()).and_then(|()| {
                done_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|e| e.to_string())?
            });
            if let Err(error) = result {
                *callback_error2.lock().unwrap() = Some(error);
                cancel.cancel();
            }
        }),
        ..ctl
    };
    let mut req = request(
        "pageRank",
        [("score".into(), "ex:rank".into())].into(),
        Some(2),
    );
    req.write.as_mut().unwrap().mode = mode;
    let opts = if restricted {
        destination_only()
    } else {
        QueryOptions::default()
    };
    let outcome = run_write(&s, &req, &prefixes(), &opts, None, &ctl);
    drop(ctl);
    // All handshake guards are released and the competing writer joined before assertions.
    let inserted = writer.join();
    assert_eq!(*callback_error.lock().unwrap(), None);
    let run = inserted.expect("competing writer panicked").unwrap();
    let rep = outcome.expect("destination-only write must not delete another graph");
    assert_eq!(rep.run, run);
    let unrelated = select(
        &s,
        &format!(
            "SELECT ?b ?t WHERE {{ GRAPH <urn:graph:other> {{ <{run}> \
             spk:batchesWritten ?b ; spk:triplesWritten ?t }} }}"
        ),
    );
    assert_eq!(unrelated, vec![vec!["777", "888"]]);
    assert_eq!(rep.triples, 7);
    assert_eq!(rep.commits, 4);
    assert_eq!(rep.source_commit, before);
    assert_eq!(s.snapshot().commit, before + rep.commits + 1);
    let final_record = batch_record(&s);
    assert_eq!(
        final_record,
        vec![vec![run, "4".into(), "7".into(), "7".into()]]
    );
}

#[test]
fn replace_counter_updates_preserve_another_graph() {
    counters_stay_in_the_destination(WriteMode::Replace, false);
}

#[test]
fn merge_counter_updates_preserve_another_graph() {
    counters_stay_in_the_destination(WriteMode::Merge, false);
}

#[test]
fn replace_counter_updates_need_only_destination_write_grants() {
    counters_stay_in_the_destination(WriteMode::Replace, true);
}

#[test]
fn merge_counter_updates_need_only_destination_write_grants() {
    counters_stay_in_the_destination(WriteMode::Merge, true);
}

fn atomic_counters_stay_in_the_destination(mode: WriteMode) {
    let s = store();
    sparkles_core::sparql::update::update(
        &s,
        "INSERT DATA { GRAPH <urn:graph:other> { <urn:uuid:other-run> \
         <urn:x-sparkles:batchesWritten> 777 ; <urn:x-sparkles:triplesWritten> 888 } }",
        &QueryOptions::default(),
    )
    .unwrap();
    let before = s.snapshot().commit;
    let mut req = request(
        "pageRank",
        [("score".into(), "ex:rank".into())].into(),
        Some(2),
    );
    let write = req.write.as_mut().unwrap();
    write.mode = mode;
    write.atomic = true;
    let rep = run_write(
        &s,
        &req,
        &prefixes(),
        &destination_only(),
        None,
        &Control::none(),
    )
    .unwrap();
    assert_eq!(rep.commits, 1);
    assert_eq!(rep.triples, 7);
    assert_eq!(s.snapshot().commit, before + 1);
    assert_eq!(
        batch_record(&s),
        vec![vec![rep.run, "1".into(), "7".into(), "7".into()]]
    );
    assert_eq!(
        select(
            &s,
            "SELECT ?b ?t WHERE { GRAPH <urn:graph:other> { <urn:uuid:other-run> \
             spk:batchesWritten ?b ; spk:triplesWritten ?t } }",
        ),
        vec![vec!["777", "888"]]
    );
}

#[test]
fn atomic_replace_counters_need_only_destination_grants() {
    atomic_counters_stay_in_the_destination(WriteMode::Replace);
}

#[test]
fn atomic_merge_counters_need_only_destination_grants() {
    atomic_counters_stay_in_the_destination(WriteMode::Merge);
}

#[test]
fn the_query_a_request_stands_for() {
    let mut r = request("pageRank", BTreeMap::new(), None);
    r.params.insert("maxIterations".into(), 50.into());
    r.params.insert("source".into(), "ex:f".into());
    let (q, outs) = query_text(&r, &prefixes(), false).unwrap();
    assert!(q.contains("SERVICE algo:pageRank"), "{q}");
    assert!(q.contains("algo:source <http://ex/f>"), "{q}");
    assert_eq!(outs, ["node", "score"]);
    r.params.insert("bogus".into(), 1.into());
    assert!(query_text(&r, &prefixes(), false).is_err());
    assert!(query_text(&request("nope", BTreeMap::new(), None), &prefixes(), false).is_err());
}

fn graphalytics_request() -> AlgoRequest {
    AlgoRequest {
        algorithm: "clustering".into(),
        projection: Some(inline(&["ex:p"])),
        params: [("graphalytics".into(), true.into())].into_iter().collect(),
        write: None,
    }
}

#[test]
fn graphalytics_request_selects_only_available_rows_and_summary() {
    let s = store();
    let req = graphalytics_request();
    let (q, vars) = query_text(&req, &prefixes(), false).unwrap();
    let result = query(s.snapshot(), &q, &QueryOptions::default()).unwrap();
    assert_eq!(vars, ["node", "coefficient"]);
    assert_eq!(result.rows().len(), 7);
    let (q, vars) = query_text(&req, &prefixes(), true).unwrap();
    assert_eq!(vars, ["summary"]);
    let result = query(s.snapshot(), &q, &QueryOptions::default()).unwrap();
    assert_eq!(result.rows().len(), 1);
    let mut ordinary = req.clone();
    ordinary.params.insert("graphalytics".into(), false.into());
    assert_eq!(
        query_text(&ordinary, &prefixes(), false).unwrap().1,
        ["node", "coefficient", "triangles"]
    );
    ordinary.params.insert("graphalytics".into(), "true".into());
    assert!(
        query_text(&ordinary, &prefixes(), false)
            .unwrap_err()
            .to_string()
            .contains("malformed parameter 'graphalytics'")
    );
}

#[test]
fn graphalytics_write_selects_coefficient_and_refuses_triangles_before_compute() {
    let s = store();
    let mut req = graphalytics_request();
    req.write = request(
        "clustering",
        [("triangles".into(), "ex:triangles".into())].into(),
        None,
    )
    .write;
    let before = s.snapshot().commit;
    let computing = Arc::new(AtomicUsize::new(0));
    let observed = computing.clone();
    let ctl = Control {
        progress: Progress::new(move |_, m| {
            if m == "computing" {
                observed.fetch_add(1, Ordering::Relaxed);
            }
        }),
        ..Control::none()
    };
    let error = run_write(&s, &req, &prefixes(), &QueryOptions::default(), None, &ctl).unwrap_err();
    assert_eq!(computing.load(Ordering::Relaxed), 0, "{error}");
    assert!(error.to_string().contains("not an output"), "{error}");
    assert_eq!(s.snapshot().commit, before);
    req.write.as_mut().unwrap().predicates =
        [("coefficient".into(), "ex:coefficient".into())].into();
    let report = run_write(
        &s,
        &req,
        &prefixes(),
        &QueryOptions::default(),
        None,
        &Control::none(),
    )
    .unwrap();
    assert_eq!(report.triples, 7);
    assert_eq!(report.commits, 1);
    req.params.insert("graphalytics".into(), false.into());
    req.write.as_mut().unwrap().predicates = [("triangles".into(), "ex:triangles".into())].into();
    assert_eq!(
        run_write(
            &s,
            &req,
            &prefixes(),
            &QueryOptions::default(),
            None,
            &Control::none()
        )
        .unwrap()
        .triples,
        7
    );
}

#[test]
fn graphalytics_write_preserves_invalid_input_precedence() {
    let s = store();
    let mut req = graphalytics_request();
    req.params.insert("graphalytics".into(), "true".into());
    req.write = request(
        "clustering",
        [("triangles".into(), "ex:triangles".into())].into(),
        None,
    )
    .write;
    let before = s.snapshot().commit;
    let error = run_write(
        &s,
        &req,
        &prefixes(),
        &QueryOptions::default(),
        None,
        &Control::none(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("malformed parameter 'graphalytics'"),
        "{error}"
    );
    req.write.as_mut().unwrap().graph = "invalid graph".into();
    let error = run_write(
        &s,
        &req,
        &prefixes(),
        &QueryOptions::default(),
        None,
        &Control::none(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("not an absolute IRI"), "{error}");
    assert_eq!(s.snapshot().commit, before);
}

#[test]
fn a11_batches_records_and_replace() {
    let s = store();
    let preds: BTreeMap<String, String> = [("score".to_string(), "ex:rank".to_string())].into();
    let before = s.snapshot().commit;
    let rep = run_write(
        &s,
        &request("pageRank", preds.clone(), Some(2)),
        &prefixes(),
        &QueryOptions::default(),
        None,
        &Control::none(),
    )
    .unwrap();
    // seven nodes in batches of two triples
    assert_eq!(rep.triples, 7);
    assert_eq!(rep.commits, 4);
    assert_eq!(rep.source_commit, before);
    assert_eq!(s.snapshot().commit, before + 4);
    let ranks = select(
        &s,
        "SELECT ?n ?r WHERE { GRAPH <urn:graph:out> { ?n ex:rank ?r } }",
    );
    assert_eq!(ranks.len(), 7);
    let rec = select(
        &s,
        "SELECT ?c ?b ?t ?status ?end WHERE { GRAPH <urn:graph:out> { ?run a spk:AlgorithmRun ; \
         spk:sourceCommit ?c ; spk:batchesWritten ?b ; spk:triplesWritten ?t ; spk:status ?status ; \
         prov:endedAtTime ?end } }",
    );
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0][0], before.to_string());
    assert_eq!(rec[0][1], "4");
    assert_eq!(rec[0][2], "7");
    assert_eq!(rec[0][3], "complete");
    // a second run in replace mode leaves no stale value or record
    run_write(
        &s,
        &request("pageRank", preds, Some(2)),
        &prefixes(),
        &QueryOptions::default(),
        None,
        &Control::none(),
    )
    .unwrap();
    let ranks = select(
        &s,
        "SELECT ?n ?r WHERE { GRAPH <urn:graph:out> { ?n ex:rank ?r } }",
    );
    assert_eq!(ranks.len(), 7);
    let recs = select(
        &s,
        "SELECT ?run WHERE { GRAPH <urn:graph:out> { ?run a spk:AlgorithmRun } }",
    );
    assert_eq!(recs.len(), 1);
}

#[test]
fn atomic_runs_commit_once() {
    let s = store();
    let mut r = request(
        "kCore",
        [("core".to_string(), "ex:core".to_string())].into(),
        Some(1),
    );
    r.write.as_mut().unwrap().atomic = true;
    let before = s.snapshot().commit;
    let rep = run_write(
        &s,
        &r,
        &prefixes(),
        &QueryOptions::default(),
        None,
        &Control::none(),
    )
    .unwrap();
    assert_eq!(rep.commits, 1);
    assert_eq!(s.snapshot().commit, before + 1);
}

#[test]
fn a_cancelled_run_says_how_far_it_got() {
    let s = store();
    let preds: BTreeMap<String, String> = [("score".to_string(), "ex:rank".to_string())].into();
    let ctl = Control::none();
    let flag = ctl.cancel.clone();
    let seen = Arc::new(AtomicUsize::new(0));
    let seen2 = seen.clone();
    let ctl = Control {
        progress: Progress::new(move |_, m| {
            // cancel when the third batch is about to start
            if m.starts_with("writing batch 3") {
                seen2.fetch_add(1, Ordering::Relaxed);
                flag.cancel();
            }
        }),
        ..ctl
    };
    let e = run_write(
        &s,
        &request("pageRank", preds, Some(2)),
        &prefixes(),
        &QueryOptions::default(),
        None,
        &ctl,
    )
    .unwrap_err();
    assert!(matches!(e, sparkles_core::Error::Cancelled), "{e}");
    assert_eq!(seen.load(Ordering::Relaxed), 1);
    let rec = select(
        &s,
        "SELECT ?b ?t ?status ?end WHERE { GRAPH <urn:graph:out> { ?run a spk:AlgorithmRun ; \
         spk:batchesWritten ?b ; spk:triplesWritten ?t ; spk:status ?status ; prov:endedAtTime ?end } }",
    );
    assert_eq!(rec.len(), 1, "{rec:?}");
    assert_eq!(rec[0][0], "2");
    assert_eq!(rec[0][1], "4");
    assert_eq!(rec[0][2], "cancelled");
    let ranks = select(
        &s,
        "SELECT ?n WHERE { GRAPH <urn:graph:out> { ?n ex:rank ?r } }",
    );
    assert_eq!(ranks.len(), 4, "the batches already committed stay");
}

#[test]
fn merge_replaces_only_the_written_values() {
    let s = store();
    sparkles_core::sparql::update::update(
        &s,
        "PREFIX ex: <http://ex/> INSERT DATA { GRAPH <urn:graph:out> { ex:a ex:rank 99 ; ex:keep 1 . ex:zzz ex:rank 5 } }",
        &QueryOptions::default(),
    )
    .unwrap();
    let mut r = request(
        "pageRank",
        [("score".to_string(), "ex:rank".to_string())].into(),
        None,
    );
    r.write.as_mut().unwrap().mode = WriteMode::Merge;
    run_write(
        &s,
        &r,
        &prefixes(),
        &QueryOptions::default(),
        None,
        &Control::none(),
    )
    .unwrap();
    let a = select(
        &s,
        "SELECT ?r WHERE { GRAPH <urn:graph:out> { ex:a ex:rank ?r } }",
    );
    assert_eq!(a.len(), 1, "the old value is replaced: {a:?}");
    assert_ne!(a[0][0], "99");
    let keep = select(
        &s,
        "SELECT ?r WHERE { GRAPH <urn:graph:out> { ex:a ex:keep ?r } }",
    );
    assert_eq!(keep.len(), 1);
    // a node outside the result keeps its value
    let z = select(
        &s,
        "SELECT ?r WHERE { GRAPH <urn:graph:out> { ex:zzz ex:rank ?r } }",
    );
    assert_eq!(z[0][0], "5");
}

#[test]
fn communities_are_resources_with_levels() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"@prefix ex: <http://ex/> . ex:a1 ex:p ex:a2, ex:a3 . ex:a2 ex:p ex:a3 . \
          ex:b1 ex:p ex:b2, ex:b3 . ex:b2 ex:p ex:b3 . ex:a3 ex:p ex:b1 ."
            .to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let mut r = request(
        "leiden",
        [
            ("community".to_string(), "ex:community".to_string()),
            ("level".to_string(), "ex:level".to_string()),
        ]
        .into(),
        None,
    );
    r.params.insert("seed".into(), 1.into());
    run_write(
        &s,
        &r,
        &prefixes(),
        &QueryOptions::default(),
        None,
        &Control::none(),
    )
    .unwrap();
    let rows = select(
        &s,
        "SELECT ?n ?c ?l WHERE { GRAPH <urn:graph:out> { ?n ex:community ?c . ?c ex:level ?l } }",
    );
    assert!(rows.len() >= 6);
    assert!(
        rows.iter()
            .all(|r| r[1].starts_with("urn:x-sparkles:algo:"))
    );
    let same = |a: &str, b: &str, level: &str| {
        let find = |n: &str| {
            rows.iter()
                .find(|r| r[0] == format!("http://ex/{n}") && r[2] == level)
                .map(|r| r[1].clone())
        };
        find(a) == find(b)
    };
    assert!(same("a1", "a2", "0"));
    assert!(!same("a1", "b1", "0"));
    let _ = ProjectionRef::Named(String::new());
}
