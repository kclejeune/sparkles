//! The inspection of a run's input state (`ReasonOptions::inspect`): it runs under the
//! writer lock, after a progress report that tells a watcher the run holds the lock, and
//! its warnings join the report's.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::{Extras, Incremental, Profile, ReasonOptions, materialize_incremental};
use std::sync::{Arc, Mutex};

#[test]
fn the_inspection_follows_a_progress_report_and_warns() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
          ex:A rdfs:subClassOf ex:B . ex:a a ex:A ."
            .to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let (p, i) = (events.clone(), events.clone());
    let head = s.snapshot().commit;
    let opts = ReasonOptions {
        progress: Some(Arc::new(move |_, msg: &str| {
            p.lock().unwrap().push(format!("progress {msg}"))
        })),
        inspect: Some(Arc::new(move |snap| {
            i.lock().unwrap().push("inspect".into());
            assert_eq!(
                snap.commit, head,
                "the inspection reads the locked input state"
            );
            Ok(vec!["checked".into()])
        })),
        ..ReasonOptions::default()
    };
    let r = materialize_incremental(
        &s,
        &Profile::RdfsSimple,
        &Extras::default(),
        Incremental {
            since: None,
            cache: None,
        },
        &opts,
    )
    .unwrap();
    let events = events.lock().unwrap();
    let at = events
        .iter()
        .position(|e| e == "inspect")
        .expect("inspected");
    assert!(at > 0 && events[0].starts_with("progress"), "{events:?}");
    assert!(
        r.warnings.contains(&"checked".to_string()),
        "{:?}",
        r.warnings
    );
}
