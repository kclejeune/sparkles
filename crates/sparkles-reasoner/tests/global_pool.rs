//! A run called on the only worker of the global rayon pool. The run moves off that
//! worker, which parks until the run ends, so no step of the run may hand work to the
//! global pool. This file holds one test, so that it can size the global pool of its
//! own process.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::{Extras, Incremental, Profile, ReasonOptions, materialize_incremental};
use std::sync::mpsc;
use std::time::Duration;

fn load(s: &Store, ttl: String) {
    s.load(&[Source::from_bytes(
        ttl.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
}

/// An incremental run without a kept closure rebuilds the previous closure from the
/// store, probing its inferred triples against the input graphs. With 128 or more of
/// them the probes run in parallel, and they must run on the run's own threads, not on
/// the global pool whose only worker waits for the run.
#[test]
fn reconstruction_on_the_only_global_worker_ends() {
    rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build_global()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    let mut ttl = String::from(
        "@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:C .\n",
    );
    for i in 0..300 {
        ttl.push_str(&format!("ex:i{i} a ex:A .\n"));
    }
    load(&s, ttl);
    let rdfs = Profile::RdfsSimple;
    let first = materialize_incremental(
        &s,
        &rdfs,
        &Extras::default(),
        Incremental {
            since: None,
            cache: None,
        },
        &ReasonOptions::default(),
    )
    .unwrap();
    assert!(first.inferred >= 128, "{} inferred", first.inferred);
    let since = first.receipt.unwrap().commit.seq;
    load(
        &s,
        "@prefix ex: <http://ex.org/> . ex:extra a ex:A .\n".to_string(),
    );
    let (tx, rx) = mpsc::channel();
    let s = std::sync::Arc::new(s);
    {
        let s = s.clone();
        rayon::spawn(move || {
            let inc = Incremental {
                since: Some(since),
                cache: None,
            };
            let r = materialize_incremental(
                &s,
                &rdfs,
                &Extras::default(),
                inc,
                &ReasonOptions::default(),
            );
            let _ =
                tx.send(r.map(|r| (r.inferred, r.changes.map(|c| c.source).unwrap_or_default())));
        });
    }
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(r) => {
            let (inferred, source) = r.unwrap();
            assert_eq!(source, "store");
            assert_eq!(inferred, first.inferred + 2);
        }
        Err(e) => panic!("the run on the only global worker did not end: {e}"),
    }
}
