//! Point-in-time reads and named snapshots: free history in the current generation,
//! pins that keep generations across compaction and bulk commits, retention, garbage
//! collection and its crash safety.

use sparkles_core::Error;
use sparkles_core::history::{At, HistoryOptions, Retention};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

/// A clock well after the root commit's wall-clock time: `upd` sets it to T0 + 1000·s
/// for commit s.
const T0: i64 = 4_000_000_000_000;

thread_local! {
    static NOW: Arc<AtomicI64> = Arc::new(AtomicI64::new(T0));
}

fn now() -> Arc<AtomicI64> {
    NOW.with(|n| n.clone())
}

fn open(root: &Path, opts: StoreOptions) -> Store {
    let s = Store::open(root, opts).unwrap();
    let n = now();
    s.set_clock(Arc::new(move || n.load(Ordering::SeqCst)));
    s
}

fn upd(s: &Store, u: &str) {
    now().store(
        T0 + 1000 * (s.head_commit().seq as i64 + 1),
        Ordering::SeqCst,
    );
    update(s, u, &QueryOptions::default()).unwrap();
}

fn time(ms: i64) -> String {
    format!("time:{}", sparkles_core::commit::rfc3339_ms(T0 + ms))
}

fn at(s: &Store, a: &str) -> sparkles_core::Result<Vec<String>> {
    let (snap, _) = s.snapshot_at(&a.parse::<At>()?, &HistoryOptions::default())?;
    let r = query(
        snap,
        "SELECT ?s WHERE { ?s ?p ?o } ORDER BY ?s",
        &QueryOptions::default(),
    )?;
    Ok(r.rows()?
        .into_iter()
        .map(|row| row.values()[0].as_ref().unwrap().to_string())
        .collect())
}

/// The setup of the spec: commits 1–3 (+a, +b, −a) in the first generation.
fn setup(root: &Path, opts: StoreOptions) -> Store {
    let s = open(root, opts);
    upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 }");
    upd(&s, "INSERT DATA { <urn:b> <urn:p> 2 }");
    upd(&s, "DELETE DATA { <urn:a> <urn:p> 1 }");
    assert_eq!(s.head_commit().seq, 3);
    s
}

fn gens(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("gen-"))
        .collect();
    v.sort();
    v
}

#[test]
fn free_history_and_selectors() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(&dir.path().join("db"), StoreOptions::default());
    assert_eq!(at(&s, "commit:1").unwrap(), ["<urn:a>"]);
    assert_eq!(at(&s, "2").unwrap(), ["<urn:a>", "<urn:b>"]);
    assert!(at(&s, "0").unwrap().is_empty());
    assert_eq!(at(&s, "head").unwrap(), ["<urn:b>"]);
    // the live snapshot for the head
    let (snap, r) = s.snapshot_at(&At::Commit(3), &Default::default()).unwrap();
    assert!(!snap.historical && !r.historical);
    let (snap, r) = s.snapshot_at(&At::Commit(1), &Default::default()).unwrap();
    assert!(snap.historical && r.historical && r.head == 3);
    // time: the last commit at or before the instant
    assert_eq!(at(&s, &time(2500)).unwrap().len(), 2);
    assert_eq!(at(&s, &time(2000)).unwrap().len(), 2);
    assert_eq!(at(&s, &time(1999)).unwrap(), ["<urn:a>"]);
    assert_eq!(at(&s, &time(1_000_000)).unwrap(), ["<urn:b>"]);
    assert!(matches!(
        at(&s, "time:1970-01-01T00:00:00Z"),
        Err(Error::NotFound(_))
    ));
    // errors
    assert!(matches!(at(&s, "99"), Err(Error::NotFound(_))));
    assert!(matches!(at(&s, "snapshot:nope"), Err(Error::NotFound(_))));
    assert!(matches!(at(&s, "abc"), Err(Error::Invalid(_))));
    let h = s.history();
    assert_eq!(h.reconstructable, [(0, 3)]);
    assert_eq!(h.bytes, 0);
    // a second read of a past state is served from the cache
    at(&s, "1").unwrap();
    assert!(s.history().hits >= 1);
}

#[test]
fn compaction_ends_unpinned_history() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = setup(&root, StoreOptions::default());
    upd(&s, "INSERT DATA { <urn:c> <urn:p> 3 }");
    s.compact().unwrap();
    assert_eq!(gens(&root), ["gen-0002"]);
    let Err(Error::HistoryGone(g)) = at(&s, "1") else {
        panic!("gone")
    };
    assert_eq!((g.seq, g.head), (Some(1), 4));
    assert_eq!(g.reconstructable, [(4, 4)]);
    assert_eq!(g.metadata.map(|m| m.seq), Some(1));
    assert_eq!(at(&s, "4").unwrap().len(), 2);
}

#[test]
fn pins_keep_their_generation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = setup(&root, StoreOptions::default());
    let (v1, created) = s
        .create_snapshot("v1", &At::Commit(1), Some("before".into()))
        .unwrap();
    assert!(created && v1.seq == 1 && v1.reconstructable);
    // idempotent, and a conflict at another commit
    assert!(!s.create_snapshot("v1", &At::Commit(1), None).unwrap().1);
    assert!(matches!(
        s.create_snapshot("v1", &At::Commit(2), None),
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        s.create_snapshot("a/b", &At::Head, None),
        Err(Error::Invalid(_))
    ));
    s.compact().unwrap();
    assert_eq!(gens(&root), ["gen-0001", "gen-0002"]);
    let h = s.history();
    let g1 = h.generations.iter().find(|g| g.name == "gen-0001").unwrap();
    assert!(g1.bytes > 0);
    assert_eq!(
        g1.held_by,
        [sparkles_core::history::Hold::Snapshot("v1".into())]
    );
    assert_eq!(at(&s, "snapshot:v1").unwrap(), ["<urn:a>"]);
    // the same generation serves its other commits too
    assert_eq!(at(&s, "2").unwrap().len(), 2);
    // across a restart
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.snapshots()[0].name, "v1");
    assert_eq!(s.snapshots()[0].note.as_deref(), Some("before"));
    assert_eq!(at(&s, "snapshot:v1").unwrap(), ["<urn:a>"]);
    // deleting the pin collects the generation
    assert!(s.delete_snapshot("v1").unwrap());
    assert!(!s.delete_snapshot("v1").unwrap());
    assert_eq!(gens(&root), ["gen-0002"]);
    assert!(matches!(at(&s, "1"), Err(Error::HistoryGone(_))));
}

#[test]
fn pinning_the_head_before_compaction_is_free() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = setup(&root, StoreOptions::default());
    s.create_snapshot("rel", &At::Head, None).unwrap();
    s.compact().unwrap();
    assert_eq!(gens(&root), ["gen-0002"]);
    assert_eq!(s.history().bytes, 0);
    assert_eq!(at(&s, "snapshot:rel").unwrap(), ["<urn:b>"]);
}

#[test]
fn limits() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(
        &dir.path().join("db"),
        StoreOptions {
            max_snapshots: 1,
            ..Default::default()
        },
    );
    s.create_snapshot("one", &At::Head, None).unwrap();
    let Err(Error::Conflict(m)) = s.create_snapshot("two", &At::Head, None) else {
        panic!("limit")
    };
    assert!(m.contains("history-limit"));
}

#[test]
fn bulk_commits_and_the_commit_window() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = open(
        &root,
        StoreOptions {
            bulk_threshold: 10,
            ..Default::default()
        },
    );
    s.set_retention(Retention {
        keep_commits: Some(10),
        keep_age_ms: None,
        max_bytes: None,
    })
    .unwrap();
    upd(&s, "INSERT DATA { <urn:x1> <urn:p> 1 }");
    upd(&s, "INSERT DATA { <urn:x2> <urn:p> 2 }");
    let nt: String = (0..100)
        .map(|i| format!("<urn:n{i}> <urn:p> \"{i}\" .\n"))
        .collect();
    s.load(&[Source::from_bytes(
        nt.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    assert_eq!(s.head_commit().seq, 3);
    assert_eq!(gens(&root), ["gen-0001", "gen-0002"]);
    assert_eq!(at(&s, "2").unwrap().len(), 2);
    assert_eq!(at(&s, "3").unwrap().len(), 102);
    let h = s.history();
    let g1 = h.generations.iter().find(|g| g.name == "gen-0001").unwrap();
    assert_eq!(g1.held_by, [sparkles_core::history::Hold::Retention]);
    // turning the window off collects it
    s.set_retention(Retention::default()).unwrap();
    assert_eq!(gens(&root), ["gen-0002"]);
}

#[test]
fn the_age_window() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = setup(&root, StoreOptions::default());
    s.set_retention(Retention {
        keep_commits: None,
        keep_age_ms: Some(1_500),
        max_bytes: None,
    })
    .unwrap();
    // now 3500: the cutoff is 2000, so commit 2 (the head until 3000) stays readable
    now().store(T0 + 3500, Ordering::SeqCst);
    s.compact().unwrap();
    assert_eq!(gens(&root), ["gen-0001", "gen-0002"]);
    assert_eq!(at(&s, "2").unwrap().len(), 2);
}

#[test]
fn interrupted_collections_and_rebuilds_are_finished_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = setup(&root, StoreOptions::default());
        s.create_snapshot("v1", &At::Commit(1), None).unwrap();
        s.compact().unwrap();
        assert_eq!(gens(&root), ["gen-0001", "gen-0002"]);
    }
    // a half-deleted generation, an interrupted rebuild, and one whose files a power loss
    // emptied: every generation above CURRENT is an interrupted rebuild
    std::fs::create_dir(root.join("gen-0009.deleting")).unwrap();
    std::fs::write(root.join("gen-0009.deleting/x"), b"x").unwrap();
    copy_dir(&root.join("gen-0002"), &root.join("gen-0003"));
    std::fs::create_dir(root.join("gen-0007")).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(gens(&root), ["gen-0001", "gen-0002"]);
    assert_eq!(at(&s, "snapshot:v1").unwrap(), ["<urn:a>"]);
    drop(s);
    // a generation left behind by a crash after the switch is collected unless pinned
    std::fs::remove_file(root.join("history.json")).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(gens(&root), ["gen-0002"]);
    assert!(s.snapshots().is_empty());
}

#[test]
fn damage_after_the_target_commit_does_not_matter() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = setup(&root, StoreOptions::default());
        s.create_snapshot("v1", &At::Commit(1), None).unwrap();
        s.compact().unwrap();
    }
    // flip a byte in the second transaction of the sealed generation's log: commit 1
    // replays, commit 2 (the damaged one) is corrupt
    let wal = root.join("gen-0001/wal.log");
    let mut b = std::fs::read(&wal).unwrap();
    let rec = b.len() / 6; // six records: three single-quad transactions
    b[rec * 3 - 2] ^= 0xff;
    std::fs::write(&wal, b).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(at(&s, "1").unwrap(), ["<urn:a>"]);
    assert!(
        matches!(at(&s, "2"), Err(Error::Corrupt(_))),
        "{:?}",
        at(&s, "2")
    );
}

#[test]
fn in_memory_stores_keep_only_what_pins_and_the_window_hold() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 }");
    upd(&s, "INSERT DATA { <urn:b> <urn:p> 1 }");
    assert!(matches!(
        s.snapshot_at(&At::Commit(1), &Default::default()),
        Err(Error::HistoryGone(_))
    ));
    assert!(s.snapshot_at(&At::Head, &Default::default()).is_ok());
    s.set_retention(Retention {
        keep_commits: Some(2),
        ..Default::default()
    })
    .unwrap();
    upd(&s, "INSERT DATA { <urn:c> <urn:p> 1 }");
    upd(&s, "INSERT DATA { <urn:d> <urn:p> 1 }");
    // commits 3 and 4 are the last two: 3 is kept as a past state
    assert_eq!(at(&s, "3").unwrap().len(), 3);
    assert!(matches!(at(&s, "2"), Err(Error::HistoryGone(_))));
    assert_eq!(s.history().reconstructable, [(3, 4)]);
}

#[test]
fn result_cache_keeps_states_apart() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(
        &dir.path().join("db"),
        StoreOptions {
            result_cache_min_ms: 0.0,
            ..Default::default()
        },
    );
    for _ in 0..2 {
        assert_eq!(at(&s, "1").unwrap(), ["<urn:a>"]);
        assert_eq!(at(&s, "2").unwrap().len(), 2);
        assert_eq!(at(&s, "head").unwrap(), ["<urn:b>"]);
    }
}

#[test]
fn dumps_at_a_commit() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(&dir.path().join("db"), StoreOptions::default());
    let mut out = Vec::new();
    assert_eq!(s.dump_nquads_at(&At::Commit(2), &mut out).unwrap(), 2);
}

#[cfg(feature = "text")]
#[test]
fn historical_text_search_reverses_inserts_and_deletes_under_the_query_budget() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(&dir.path().join("db"), StoreOptions::default());
    upd(&s, r#"INSERT DATA { <urn:old> <urn:label> "fox" }"#);
    let old = s.snapshot().commit;
    s.enable_text(Default::default()).unwrap();
    upd(
        &s,
        r#"DELETE DATA { <urn:old> <urn:label> "fox" };
        INSERT DATA { <urn:new> <urn:label> "fox fox fox" }"#,
    );
    let q = r#"SELECT ?s { (?s ?score) <http://jena.apache.org/text#query> ("fox" 1) }"#;
    let subject = |at| {
        let (snap, _) = s.snapshot_at(&at, &Default::default()).unwrap();
        let rows = query(snap, q, &QueryOptions::default())
            .unwrap()
            .rows()
            .unwrap();
        assert_eq!(rows.len(), 1);
        rows.iter().next().unwrap().values()[0]
            .as_ref()
            .unwrap()
            .to_string()
    };
    assert_eq!(subject(At::Head), "<urn:new>");
    assert_eq!(subject(At::Commit(old)), "<urn:old>");
    let (snap, _) = s
        .snapshot_at(&At::Commit(old), &Default::default())
        .unwrap();
    let memory = QueryOptions {
        max_memory_bytes: Some(1 << 20),
        ..Default::default()
    };
    assert!(matches!(
        query(snap.clone(), q, &memory),
        Err(Error::BudgetExceeded(b))
            if b.kind == sparkles_core::BudgetKind::Memory && b.requested > b.limit
    ));
    let stopped = sparkles_core::task::Cancel::new();
    stopped.cancel();
    let cancelled = QueryOptions {
        cancel: Some(stopped.flag()),
        ..Default::default()
    };
    assert!(matches!(
        query(snap.clone(), q, &cancelled),
        Err(Error::Cancelled)
    ));
    let expired = QueryOptions {
        deadline: Some(std::time::Instant::now()),
        ..Default::default()
    };
    assert!(matches!(query(snap, q, &expired), Err(Error::Timeout)));
}

#[cfg(feature = "text")]
#[test]
fn creating_an_indexed_snapshot_reports_an_existing_pin_without_text() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path(), StoreOptions::default());
    s.enable_text(Default::default()).unwrap();
    let (pin, _) = s.create_snapshot("search", &At::Head, None).unwrap();
    let Err(Error::Conflict(message)) =
        s.create_snapshot_with_text("search", &At::Head, &Default::default())
    else {
        panic!("creating an indexed pin over an unindexed pin must conflict");
    };
    assert!(message.contains(&format!(
        "already pins commit {} without a full-text index",
        pin.seq
    )));
}

#[cfg(feature = "text")]
#[test]
fn named_snapshot_text_survives_compaction_reopen_and_live_disable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let q = r#"SELECT ?s { ?s <http://jena.apache.org/text#query> "fox" }"#;
    let s = open(&root, StoreOptions::default());
    upd(&s, r#"INSERT DATA { <urn:old> <urn:label> "fox" }"#);
    s.enable_text(Default::default()).unwrap();
    let old = s.snapshot().commit;
    let pin = s
        .create_snapshot_with_text("search", &At::Head, &Default::default())
        .unwrap()
        .0;
    assert!(pin.text_bytes > 0);
    let index = snapshot_text_dir(&root);
    assert!(s.history().bytes >= pin.text_bytes);
    upd(
        &s,
        r#"DELETE DATA { <urn:old> <urn:label> "fox" };
        INSERT DATA { <urn:new> <urn:label> "fox fox fox" }"#,
    );
    s.compact_with(&Default::default()).unwrap();
    s.disable_text().unwrap();
    drop(s);

    let s = open(&root, StoreOptions::default());
    let pin = s.named_snapshot("search").unwrap();
    assert!(pin.generation.is_some());
    assert!(pin.text_bytes > 0);
    let (snapshot, _) = s
        .snapshot_at(&At::Snapshot("search".into()), &Default::default())
        .unwrap();
    let rows = query(snapshot.clone(), q, &Default::default())
        .unwrap()
        .rows()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows.iter().next().unwrap().values()[0]
            .as_ref()
            .unwrap()
            .to_string(),
        "<urn:old>"
    );
    let (past, _) = s
        .snapshot_at(&At::Commit(old), &Default::default())
        .unwrap();
    assert!(matches!(
        query(past, q, &Default::default()),
        Err(Error::HistoryUnsupported(_))
    ));
    assert!(s.delete_snapshot("search").unwrap());
    assert!(!index.exists());
    // A reader that already opened the index keeps its mapped files through deletion.
    assert_eq!(query(snapshot, q, &Default::default()).unwrap().len(), 1);
}

#[cfg(feature = "text")]
#[test]
fn rdf_query_at_an_indexed_pin_defers_opening_its_corrupt_index() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = open(root, StoreOptions::default());
    upd(&store, r#"INSERT DATA { <urn:old> <urn:label> "fox" }"#);
    store.enable_text(Default::default()).unwrap();
    store
        .create_snapshot_with_text("search", &At::Head, &Default::default())
        .unwrap();
    upd(&store, r#"DELETE DATA { <urn:old> <urn:label> "fox" }"#);
    let manifest = snapshot_text_dir(root).join("text/meta.json");
    let bytes = std::fs::read(&manifest).unwrap();
    std::fs::write(&manifest, b"invalid").unwrap();

    let (snapshot, _) = store
        .snapshot_at(&At::Snapshot("search".into()), &Default::default())
        .unwrap();
    let rdf = query(
        snapshot.clone(),
        "SELECT ?s { ?s <urn:label> ?o }",
        &Default::default(),
    )
    .unwrap();
    assert_eq!(rdf.len(), 1);
    // The same snapshot can search after the file is repaired because the RDF read
    // neither tried opening the malformed manifest nor cached a failure.
    std::fs::write(manifest, bytes).unwrap();
    let text = r#"SELECT ?s { ?s <http://jena.apache.org/text#query> "fox" }"#;
    assert_eq!(query(snapshot, text, &Default::default()).unwrap().len(), 1);
}

#[cfg(feature = "text")]
#[test]
fn rebuilding_snapshot_text_charges_only_the_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let store = open(root, StoreOptions::default());
    upd(&store, r#"INSERT DATA { <urn:old> <urn:label> "fox" }"#);
    store.enable_text(Default::default()).unwrap();
    let (pin, _) = store
        .create_snapshot_with_text("search", &At::Head, &Default::default())
        .unwrap();
    let old = snapshot_text_dir(root);
    let quota = store.quota().used_bytes + 512;
    assert!(pin.text_bytes > 512);
    store.set_quota(Some(quota)).unwrap();

    let rebuilt = store
        .rebuild_snapshot_text("search", &Default::default())
        .unwrap();
    assert_eq!(rebuilt.seq, pin.seq);
    assert!(rebuilt.text_bytes > 512);
    assert!(store.quota().used_bytes <= quota);
    assert!(!old.exists());
    assert_ne!(old, snapshot_text_dir(root));
}

#[cfg(feature = "text")]
#[test]
fn adjusted_and_head_text_search_report_the_full_hit_count() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(dir.path(), StoreOptions::default());
    upd(
        &store,
        r#"INSERT DATA {
        <urn:a> <urn:label> "fox" . <urn:b> <urn:label> "fox" .
        <urn:c> <urn:label> "fox" . <urn:d> <urn:label> "fox"
    }"#,
    );
    store
        .enable_text(sparkles_core::text::TextConfig {
            max_hits: 2,
            ..Default::default()
        })
        .unwrap();
    let seq = store.snapshot().commit;
    store
        .create_snapshot_with_text("search", &At::Head, &Default::default())
        .unwrap();
    upd(&store, r#"INSERT DATA { <urn:e> <urn:label> "hen" }"#);
    let q = r#"SELECT ?s { ?s <http://jena.apache.org/text#query> "fox" }"#;
    for at in [At::Head, At::Commit(seq), At::Snapshot("search".into())] {
        let (snap, _) = store.snapshot_at(&at, &Default::default()).unwrap();
        assert!(matches!(
            query(snap, q, &Default::default()),
            Err(Error::BudgetExceeded(b)) if b.kind == sparkles_core::BudgetKind::Rows
                && b.limit == 2 && b.requested == 4
        ));
    }
}

#[cfg(feature = "text")]
#[test]
fn expiration_releases_snapshot_text_files() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path(), StoreOptions::default());
    s.enable_text(Default::default()).unwrap();
    s.create_snapshot_with_text(
        "short",
        &At::Head,
        &sparkles_core::history::SnapshotOptions {
            expires_ms: Some(T0 + 2_000),
            ..Default::default()
        },
    )
    .unwrap();
    let index = snapshot_text_dir(dir.path());
    now().store(T0 + 3_000, Ordering::SeqCst);
    s.history_tick().unwrap();
    assert!(s.named_snapshot("short").is_none());
    assert!(!index.exists());
}

#[cfg(feature = "text")]
#[test]
fn snapshot_text_can_be_repaired_and_dropped_without_releasing_its_pin() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let q = r#"SELECT ?s { ?s <http://jena.apache.org/text#query> "fox" }"#;
    let s = open(root, StoreOptions::default());
    upd(&s, r#"INSERT DATA { <urn:old> <urn:label> "fox" }"#);
    s.enable_text(Default::default()).unwrap();
    let name = "repair";
    let at = At::Snapshot(name.into());
    let pin = s
        .create_snapshot_with_text(name, &At::Head, &Default::default())
        .unwrap()
        .0;
    let index = snapshot_text_dir(root);
    upd(&s, r#"DELETE DATA { <urn:old> <urn:label> "fox" }"#);
    s.compact().unwrap();
    s.disable_text().unwrap();
    std::fs::remove_dir_all(index.join("text")).unwrap();
    let (snapshot, _) = s.snapshot_at(&at, &Default::default()).unwrap();
    assert_eq!(snapshot.len(), 1);
    assert!(matches!(
        query(snapshot, q, &Default::default()),
        Err(Error::TextUnavailable(_))
    ));
    let repaired = s.rebuild_snapshot_text(name, &Default::default()).unwrap();
    assert_eq!(repaired.seq, pin.seq);
    assert!(!index.exists());
    let replacement = snapshot_text_dir(root);
    assert_ne!(replacement, index);
    let (snapshot, _) = s.snapshot_at(&at, &Default::default()).unwrap();
    assert_eq!(query(snapshot, q, &Default::default()).unwrap().len(), 1);
    assert!(s.drop_snapshot_text(name).unwrap());
    assert!(!replacement.exists());
    let pin = s.named_snapshot(name).unwrap();
    assert_eq!(pin.seq, repaired.seq);
    assert_eq!(pin.text_bytes, 0);
    let (snapshot, _) = s.snapshot_at(&at, &Default::default()).unwrap();
    assert_eq!(snapshot.len(), 1);
    assert!(matches!(
        query(snapshot, q, &Default::default()),
        Err(Error::HistoryUnsupported(_))
    ));
}

#[cfg(feature = "text")]
#[test]
fn a_failed_snapshot_text_publish_leaves_only_collectible_staging() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let store = open(root, StoreOptions::default());
    store.enable_text(Default::default()).unwrap();
    let blocker = root.join("history.tmp");
    std::fs::create_dir(&blocker).unwrap();
    assert!(
        store
            .create_snapshot_with_text("orphan", &At::Head, &Default::default())
            .is_err()
    );
    let orphan = snapshot_text_dir(root);
    assert!(store.named_snapshot("orphan").is_none());
    std::fs::remove_dir(blocker).unwrap();
    drop(store);
    let reopened = open(root, StoreOptions::default());
    assert!(!orphan.exists());
    assert!(reopened.named_snapshot("orphan").is_none());
}

#[cfg(feature = "text")]
fn snapshot_text_dir(root: &Path) -> std::path::PathBuf {
    let dirs = std::fs::read_dir(root.join("history-text"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(dirs.len(), 1);
    dirs.into_iter().next().unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to.join(e.file_name()));
        } else {
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}

#[test]
fn check_accepts_generations_kept_for_history() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = setup(&root, StoreOptions::default());
        s.create_snapshot("v1", &At::Commit(1), None).unwrap();
        s.compact().unwrap();
    }
    let leftovers = |root: &Path| -> Vec<String> {
        let r = sparkles_core::check::check(root, &Default::default()).unwrap();
        r.checks
            .iter()
            .flat_map(|c| c.issues.iter())
            .filter(|i| i.message.contains("leftover"))
            .filter_map(|i| i.file.clone())
            .collect()
    };
    assert!(leftovers(&root).is_empty(), "{:?}", leftovers(&root));
    // unpinned (hand-edited history.json gone), the old generation is a leftover again
    std::fs::remove_file(root.join("history.json")).unwrap();
    assert_eq!(leftovers(&root), ["gen-0001"]);
}

/// A warm pin's state is built when the pin is made and after a restart, and the
/// history cache keeps it while other states come and go.
#[test]
fn warm_pins_stay_materialized() {
    use sparkles_core::history::SnapshotOptions;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    // room for two past states of a few quads, not three
    let opts = StoreOptions {
        history_cache_bytes: 6_000,
        ..Default::default()
    };
    {
        let s = open(&root, opts.clone());
        for i in 0..6 {
            upd(&s, &format!("INSERT DATA {{ <urn:s{i}> <urn:p> 1, 2, 3 }}"));
        }
        let (p, created) = s
            .create_snapshot_opts(
                "hot",
                &At::Commit(2),
                &SnapshotOptions {
                    warm: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(created && p.warm);
        let h = s.history();
        assert_eq!((h.cache_entries, h.materializations), (1, 1));
        for seq in [3, 4, 5, 1] {
            s.snapshot_at(&At::Commit(seq), &HistoryOptions::default())
                .unwrap();
        }
        // the warm state was kept: reading it is a hit
        let misses = s.history().misses;
        s.snapshot_at(&At::Snapshot("hot".into()), &HistoryOptions::default())
            .unwrap();
        assert_eq!(s.history().misses, misses);
        assert!(s.history().cache_entries <= 2);
    }
    let s = open(&root, opts);
    assert!(s.named_snapshot("hot").unwrap().warm);
    assert_eq!(s.history().cache_entries, 0);
    assert_eq!(s.history_tick().unwrap().warmed, 1);
    assert_eq!(s.history().cache_entries, 1);
    assert_eq!(s.history_tick().unwrap().warmed, 0);
}

/// The quads of the live state, as sorted N-Quads lines.
fn dump_lines(s: &Store) -> Vec<String> {
    let mut out = Vec::new();
    s.dump_nquads(&mut out).unwrap();
    let mut v: Vec<String> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

/// The quads of the state at `seq`, as sorted N-Quads lines.
fn dump_lines_at(s: &Store, seq: u64) -> Vec<String> {
    let mut out = Vec::new();
    s.dump_nquads_at(&At::Commit(seq), &mut out).unwrap();
    let mut v: Vec<String> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

/// Past states are the same whichever known state they start from: the base, a cached
/// state before or after them, or the live state (replayed backward), across more
/// commits than the WAL index's spacing, with deletes of base quads, re-inserts and
/// changes undone within a transaction; in a sealed generation as well.
#[test]
fn past_states_agree_whatever_they_start_from() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let nt: String = (0..40)
        .map(|i| {
            format!(
                "<urn:s{}> <urn:p{}> \"{i}\" <urn:g{}> .\n",
                i % 7,
                i % 3,
                i % 2
            )
        })
        .collect();
    let mut states: Vec<(u64, Vec<String>)> = Vec::new();
    let mut r = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = |n: u64| {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        r % n
    };
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        s.load(&[Source::from_bytes(nt.into_bytes(), RdfFormat::NQuads, None)])
            .unwrap();
        s.compact().unwrap();
        states.push((s.head_commit().seq, dump_lines(&s)));
        while states.len() < 1_300 {
            let kind = rnd(4);
            let mut q = || {
                let i = rnd(60);
                format!(
                    "GRAPH <urn:g{}> {{ <urn:s{}> <urn:p{}> \"{i}\" }}",
                    i % 2,
                    i % 7,
                    i % 3
                )
            };
            let u = match kind {
                0 => format!("DELETE DATA {{ {} }}", q()),
                1 => format!("INSERT DATA {{ {} {} }}", q(), q()),
                2 => {
                    let a = q();
                    format!(
                        "INSERT DATA {{ {a} }} ; DELETE DATA {{ {a} }} ; INSERT DATA {{ {} }}",
                        q()
                    )
                }
                _ => format!("DELETE DATA {{ {} }} ; INSERT DATA {{ {} }}", q(), q()),
            };
            update(&s, &u, &QueryOptions::default()).unwrap();
            let head = s.head_commit().seq;
            if states.last().unwrap().0 != head {
                states.push((head, dump_lines(&s)));
            }
        }
    }
    let check = |s: &Store, order: &[usize]| {
        for &i in order {
            let (seq, want) = &states[i];
            assert_eq!(&dump_lines_at(s, *seq), want, "commit {seq}");
        }
    };
    let n = states.len();
    // a fresh store: the middle from the base or the live state, then neighbours of
    // cached states on either side, far jumps, and the base
    let order: Vec<usize> = [n / 2, n / 2 + 1, n / 2 - 1, n - 2, 1, n / 3, 2 * n / 3, 0]
        .into_iter()
        .chain((0..n).step_by(97))
        .chain((0..n).rev().step_by(89))
        .collect();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    check(&s, &order);
    // a cache that keeps one state at a time
    let small = StoreOptions {
        history_cache_bytes: 1,
        ..Default::default()
    };
    drop(s);
    let s = Store::open(&root, small.clone()).unwrap();
    let mut order: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        order.swap(i, rnd(i as u64 + 1) as usize);
    }
    check(&s, &order[..300]);
    // sealed: a pin keeps the generation through a compaction
    s.create_snapshot("first", &At::Commit(states[1].0), None)
        .unwrap();
    s.compact().unwrap();
    drop(s);
    let s = Store::open(&root, small).unwrap();
    check(&s, &order[..300]);
    let d = s
        .diff(
            &At::Commit(states[n / 2].0),
            &At::Commit(states[n / 2 + 1].0),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(d.method, sparkles_core::store::DiffMethod::Log);
}

/// Timings of point-in-time reads: `cargo test --release -p sparkles-core --test history --
/// --ignored --nocapture history_performance`. `HIST_BASE` quads (default 1,000,000) in
/// a bulk-built base, then `HIST_COMMITS` single-quad commits (default 50,000) in one
/// generation, as in the F06 benchmark at a smaller scale.
#[test]
#[ignore = "a benchmark"]
fn history_performance() {
    use oxrdf::{Literal, NamedNode, Term};
    use sparkles_core::id::Id;
    use std::time::Instant;
    let env = |k: &str, d: u64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let (base, commits) = (env("HIST_BASE", 1_000_000), env("HIST_COMMITS", 50_000));
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let nt: String = (0..base)
        .map(|i| format!("<urn:s{}> <urn:p{}> \"v{i}\" .\n", i / 10, i % 7))
        .collect();
    s.load(&[Source::from_bytes(
        nt.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
    let first = s.head_commit().seq;
    let t = Instant::now();
    for c in 0..commits {
        let mut w = s.write();
        let subj = w
            .intern(&Term::NamedNode(NamedNode::new_unchecked(format!(
                "urn:s{}",
                c * 7919 % (base / 10)
            ))))
            .unwrap();
        let pred = w
            .intern(&Term::NamedNode(NamedNode::new_unchecked("urn:p0")))
            .unwrap();
        let obj = w
            .intern(&Term::Literal(Literal::new_simple_literal(format!("n{c}"))))
            .unwrap();
        w.insert([subj, pred, obj, Id::DEFAULT_GRAPH]).unwrap();
        w.commit().unwrap();
    }
    eprintln!("{commits} single-quad commits: {:.2?}", t.elapsed());
    let head = s.head_commit().seq;
    let read = |s: &Store, what: &str, seq: u64| {
        let t = Instant::now();
        let (snap, _) = s
            .snapshot_at(&At::Commit(seq), &HistoryOptions::default())
            .unwrap();
        let took = t.elapsed();
        assert_eq!(snap.commit, seq);
        eprintln!("read {what} (commit {seq}): {took:.2?}");
    };
    let diff = |s: &Store, what: &str, a: u64, b: u64| {
        let t = Instant::now();
        let d = s
            .diff(&At::Commit(a), &At::Commit(b), &Default::default())
            .unwrap();
        eprintln!(
            "diff {what}: +{} -{} in {:.2?}",
            d.added,
            d.removed,
            t.elapsed()
        );
    };
    let mid = first + commits / 2;
    read(&s, "the middle, first", mid);
    read(&s, "the middle, again", mid);
    read(&s, "the middle + 1", mid + 1);
    read(&s, "the middle + 5,000", mid + 5_000);
    read(&s, "the middle - 3,000", mid - 3_000);
    read(&s, "the head - 100", head - 100);
    read(&s, "the base + 1,000", first + 1_000);
    diff(&s, "one commit at the end", head - 1, head);
    diff(&s, "the last 100 commits", head - 100, head);
    diff(&s, "one commit in the middle", mid, mid + 1);
    diff(&s, "every commit", first, head);
    drop(s);
    let t = Instant::now();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    eprintln!("reopen (replays {commits} commits): {:.2?}", t.elapsed());
    read(&s, "the middle after reopening", mid);
    read(&s, "the head - 10 after reopening", head - 10);
    diff(&s, "one commit at the end after reopening", head - 1, head);
}
