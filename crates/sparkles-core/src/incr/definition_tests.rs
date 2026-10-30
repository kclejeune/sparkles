//! View definitions captured with the data by backups and clones (roadmap item 220,
//! spec F05 "Recovery of managed definitions").

use super::runtime::{OpenHost, Runtime, RuntimeOptions};
use super::views::{ViewDef, ViewState};
use crate::sparql::QueryOptions;
use crate::sparql::update::update;
use crate::store::definitions::{self, DefinitionSource};
use crate::store::{Store, StoreOptions};
use std::sync::Arc;

fn query(i: u64) -> String {
    format!("SELECT ?s WHERE {{ ?s <urn:p> ?o FILTER(?o != {i}) }}")
}

/// The `i` of a captured definition of [`query`].
fn step_of(bytes: &[u8]) -> u64 {
    let v: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    let q = v["query"].as_str().unwrap();
    let tail = q.rsplit("!= ").next().unwrap();
    tail.trim_end_matches(") }").trim().parse().unwrap()
}

fn open(dir: &std::path::Path) -> (Arc<Store>, Arc<Runtime>) {
    let s = Arc::new(Store::open(dir, StoreOptions::default()).unwrap());
    let rt = Runtime::open(
        Some(dir.to_path_buf()),
        s.dataset_id(),
        RuntimeOptions::default(),
    );
    rt.set_host(Arc::new(OpenHost));
    let source: Arc<dyn DefinitionSource> = rt.clone();
    s.register_definitions(Arc::downgrade(&source));
    (s, rt)
}

fn upd(s: &Store, u: &str) {
    update(s, u, &QueryOptions::default()).unwrap();
}

/// The capture reads the definitions while it holds the writer lock. A definition
/// changed while that lock is held, or after it, is not the captured one, and a change
/// of the definitions never waits for the writer lock, so neither side deadlocks.
#[test]
fn the_capture_records_the_definitions_of_its_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let (s, rt) = open(tmp.path());
    upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 }");
    rt.put(&s, "v", ViewDef::table(query(1))).unwrap();
    let racer = rt.clone();
    s.set_failpoint(
        "backup-capture-locked",
        Some(Arc::new(move |s: &Store| {
            racer.put(s, "v", ViewDef::table(query(2))).unwrap();
        })),
    );
    let racer = rt.clone();
    s.set_failpoint(
        "backup-capture-unlocked",
        Some(Arc::new(move |s: &Store| {
            racer.put(s, "w", ViewDef::table(query(3))).unwrap();
        })),
    );
    let cap = s.backup_capture("b").unwrap();
    s.set_failpoint("backup-capture-locked", None);
    s.set_failpoint("backup-capture-unlocked", None);
    assert_eq!(cap.definitions.len(), 1, "{:?}", cap.definitions);
    let d = &cap.definitions[0];
    assert_eq!(
        (d.kind, d.path.as_str(), d.format),
        ("view", "views/v/view.json", 1)
    );
    assert_eq!(step_of(&d.bytes), 1);
    // the bytes are those a definition file holds
    assert_eq!(
        definitions::check(
            crate::formats::definition_kind("view").unwrap(),
            &d.path,
            &d.bytes
        )
        .unwrap(),
        1
    );
    // the changes made during the capture are the runtime's current definitions
    let now = rt.capture_definitions().unwrap();
    assert_eq!(now.len(), 2);
    assert_eq!(step_of(&now[0].bytes), 2);
    // a copy of the capture holds the captured definition as a file
    drop(cap);
    let cap = s.backup_capture("c").unwrap();
    let copy = tmp.path().join("copy");
    cap.write_to(&copy).unwrap();
    let kind = crate::formats::definition_kind("view").unwrap();
    assert_eq!(definitions::read_dir(&copy, kind).unwrap(), now);
}

/// A definition change racing many captures: each capture holds either the definition
/// that was current when its commit was made or the one that followed it, never one
/// from after a later commit.
#[test]
fn racing_definition_changes_stay_consistent_with_the_captured_data() {
    let tmp = tempfile::tempdir().unwrap();
    let (s, rt) = open(tmp.path());
    rt.put(&s, "v", ViewDef::table(query(0))).unwrap();
    upd(&s, "INSERT DATA { <urn:step> <urn:n> 0 }");
    let start = s.head_commit().seq;
    const STEPS: u64 = 150;
    let writer = {
        let (s, rt) = (s.clone(), rt.clone());
        std::thread::spawn(move || {
            let mut seqs = vec![(start, 0u64)];
            for i in 1..=STEPS {
                // the definition of step i, then the data of step i
                rt.put(&s, "v", ViewDef::table(query(i))).unwrap();
                upd(
                    &s,
                    &format!(
                        "DELETE WHERE {{ <urn:step> <urn:n> ?x }} ; INSERT DATA {{ <urn:step> <urn:n> {i} }}"
                    ),
                );
                seqs.push((s.head_commit().seq, i));
            }
            seqs
        })
    };
    let mut seen = Vec::new();
    while !writer.is_finished() {
        let cap = s.backup_capture("race").unwrap();
        let def = step_of(&cap.definitions[0].bytes);
        seen.push((cap.commit.seq, def));
    }
    let seqs = writer.join().unwrap();
    assert!(!seen.is_empty());
    for (seq, def) in seen {
        let step = seqs
            .iter()
            .filter(|(c, _)| *c <= seq)
            .map(|(_, i)| *i)
            .max()
            .unwrap();
        assert!(
            def == step || def == step + 1,
            "commit {seq} holds the data of step {step}, but the definition of step {def}"
        );
    }
}

/// Without a live source, such as a stopped database opened for an offline backup, the
/// capture reads the definitions from the directory. Unreadable definitions refuse the
/// capture rather than leaving them out.
#[test]
fn directory_definitions_are_captured_and_broken_ones_refuse() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let (s, rt) = open(tmp.path());
        rt.put(&s, "v", ViewDef::table(query(7))).unwrap();
    }
    let s = Store::open(tmp.path(), StoreOptions::default()).unwrap();
    let cap = s.backup_capture("b").unwrap();
    assert_eq!(cap.definitions.len(), 1);
    assert_eq!(step_of(&cap.definitions[0].bytes), 7);
    drop(cap);
    drop(s);
    std::fs::write(
        tmp.path().join("views/v/view.json"),
        br#"{"format": 9, "query": "ASK {}"}"#,
    )
    .unwrap();
    let s = Store::open(tmp.path(), StoreOptions::default()).unwrap();
    let e = s.backup_capture("b").unwrap_err();
    assert!(matches!(e, crate::Error::Unsupported(_)), "{e}");
    drop(s);
    // a runtime that could not read them refuses too
    let (s, _rt) = open(tmp.path());
    let e = s.backup_capture("b").unwrap_err();
    assert!(e.to_string().contains("cannot be captured"), "{e}");
}

/// A clone carries the definitions paused, and a paused view attaches only once it is
/// resumed, which clears the marker.
#[test]
fn a_clone_carries_paused_definitions_until_resumed() {
    let tmp = tempfile::tempdir().unwrap();
    let (s, rt) = open(&tmp.path().join("src"));
    upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 . <urn:b> <urn:p> 2 }");
    rt.put(&s, "v", ViewDef::table(query(1))).unwrap();
    let dst = tmp.path().join("clone");
    s.clone_to(&dst, &Default::default()).unwrap();
    let kind = crate::formats::definition_kind("view").unwrap();
    assert_eq!(
        definitions::read_dir(&dst, kind).unwrap(),
        rt.capture_definitions().unwrap()
    );
    assert_eq!(
        definitions::read_paused(&dst, "views/v/view.json")
            .unwrap()
            .reason,
        "cloned"
    );
    let (c, crt) = open(&dst);
    crt.round(&c);
    let v = crt.get(&c, "v").unwrap();
    assert_eq!(
        v.state,
        ViewState::Suspended {
            reason: "cloned".into()
        }
    );
    assert!(v.plan.is_none(), "a paused view has no circuit");
    let Err(e) = crt.read(&crate::sparql::viewsvc::ViewRead {
        name: "v",
        at: c.snapshot().commit,
        snap: None,
        allow_stale: false,
        caller: None,
        deadline: None,
    }) else {
        panic!("a paused view is read")
    };
    assert!(e.to_string().contains("suspended (cloned)"), "{e}");
    // the clone's capture records its own definitions, still paused on restore
    assert_eq!(c.backup_capture("b").unwrap().definitions.len(), 1);
    crt.rebuild("v").unwrap();
    assert!(definitions::read_paused(&dst, "views/v/view.json").is_none());
    crt.round(&c);
    let v = crt
        .wait_built("v", std::time::Duration::from_secs(10))
        .unwrap();
    assert_eq!(v.rows, 1, "{v:?}");
}
