//! Point-in-time reads and named snapshots: free history in the current generation,
//! pins that keep generations across compaction and bulk commits, retention, garbage
//! collection and its crash safety.

use sparkles::Error;
use sparkles::history::{At, HistoryOptions, Retention};
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::update::update;
use sparkles::sparql::{QueryOptions, query};
use sparkles::store::{Store, StoreOptions};
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
    format!("time:{}", sparkles::commit::rfc3339_ms(T0 + ms))
}

fn at(s: &Store, a: &str) -> sparkles::Result<Vec<String>> {
    let (snap, _) = s.snapshot_at(&a.parse::<At>()?, &HistoryOptions::default())?;
    let r = query(
        snap,
        "SELECT ?s WHERE { ?s ?p ?o } ORDER BY ?s",
        &QueryOptions::default(),
    )?;
    Ok(r.rows()
        .into_iter()
        .map(|row| row[0].as_ref().unwrap().to_string())
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
    assert_eq!((g.seq, g.head), (1, 4));
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
    assert_eq!(g1.held_by, [sparkles::history::Hold::Snapshot("v1".into())]);
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
    assert_eq!(g1.held_by, [sparkles::history::Hold::Retention]);
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
    // a half-deleted generation, an interrupted rebuild, and a foreign directory
    std::fs::create_dir(root.join("gen-0009.deleting")).unwrap();
    std::fs::write(root.join("gen-0009.deleting/x"), b"x").unwrap();
    copy_dir(&root.join("gen-0002"), &root.join("gen-0003"));
    std::fs::create_dir(root.join("gen-0007")).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(gens(&root), ["gen-0001", "gen-0002", "gen-0007"]);
    assert_eq!(at(&s, "snapshot:v1").unwrap(), ["<urn:a>"]);
    drop(s);
    // a generation left behind by a crash after the switch is collected unless pinned
    std::fs::remove_file(root.join("history.json")).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(gens(&root), ["gen-0002", "gen-0007"]);
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
fn in_memory_stores_have_no_past() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 }");
    upd(&s, "INSERT DATA { <urn:b> <urn:p> 1 }");
    assert!(matches!(
        s.snapshot_at(&At::Commit(1), &Default::default()),
        Err(Error::HistoryUnsupported(_))
    ));
    assert!(s.snapshot_at(&At::Head, &Default::default()).is_ok());
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
fn full_text_search_only_at_the_head() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(&dir.path().join("db"), StoreOptions::default());
    upd(&s, "INSERT DATA { <urn:t> <urn:label> \"red fox\" }");
    s.enable_text(Default::default()).unwrap();
    let q = "SELECT ?s { ?s <http://jena.apache.org/text#query> \"fox\" }";
    let (snap, _) = s.snapshot_at(&At::Head, &Default::default()).unwrap();
    assert_eq!(query(snap, q, &QueryOptions::default()).unwrap().len(), 1);
    let (snap, _) = s.snapshot_at(&At::Commit(1), &Default::default()).unwrap();
    assert!(matches!(
        query(snap, q, &QueryOptions::default()),
        Err(Error::HistoryUnsupported(_))
    ));
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
        let r = sparkles::check::check(root, &Default::default()).unwrap();
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
