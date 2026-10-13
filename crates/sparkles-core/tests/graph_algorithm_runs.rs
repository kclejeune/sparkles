//! Graph-algorithm runs as requests, and write-back in batches (spec F14 §2.5, A11).

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::algorun::{
    AlgoRequest, ProjectionRef, WriteMode, WriteSpec, inline, query_text, run_write,
};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::task::{Control, Progress};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
