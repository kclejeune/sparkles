//! Compaction in the background: commits made during the build are carried over, and
//! crashes, bulk commits, cancellation, history, quotas, backups and the free-space
//! reserve are handled.

use super::*;
use crate::history::{At, HistoryOptions, Retention};
use crate::io::RdfFormat;
use crate::sparql::QueryOptions;
use crate::sparql::update::update;
use std::sync::atomic::AtomicI64;

/// `n` subjects from `off`, each with a type and two values, in a few predicates.
fn nt(n: usize, off: usize) -> Source {
    let mut s = String::new();
    for i in off..off + n {
        s.push_str(&format!(
            "<urn:s{i}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:C{}> .\n<urn:s{i}> <urn:p{}> \"v{i}\" .\n<urn:s{i}> <urn:q> {} .\n",
            i % 5,
            i % 7,
            i % 100
        ));
    }
    Source::from_bytes(s.into_bytes(), RdfFormat::Turtle, None)
}

/// A commit and the dump of its state.
type State = (u64, Vec<String>);

fn upd(s: &Store, u: &str) {
    update(s, u, &QueryOptions::default()).unwrap_or_else(|e| panic!("{u}: {e}"));
}

fn lines(snap: &Snapshot) -> Vec<String> {
    let mut b = Vec::new();
    dump_snapshot(snap, &mut b).unwrap();
    let mut v: Vec<String> = String::from_utf8(b)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

fn dump(s: &Store) -> Vec<String> {
    lines(&s.snapshot())
}

fn dump_at(s: &Store, seq: u64) -> Vec<String> {
    let (snap, _) = s
        .snapshot_at(&At::Commit(seq), &HistoryOptions::default())
        .unwrap();
    lines(&snap)
}

/// Write `i` of a sequence that inserts new terms, deletes base quads, deletes quads
/// it inserted, makes blank nodes and changes several quads in one commit.
fn churn(s: &Store, i: usize) {
    let u = match i % 6 {
        0 => format!("INSERT DATA {{ <urn:n{i}> <urn:new> \"new {i}\" }}"),
        1 => format!("DELETE DATA {{ <urn:s{i}> <urn:q> {} }}", i % 100),
        2 => format!(
            "INSERT DATA {{ <urn:n{i}> <urn:p1> {i} . <urn:s{i}> <urn:q> 1000 }} ; DELETE DATA {{ <urn:s{}> <urn:p{}> \"v{}\" }}",
            i + 1,
            (i + 1) % 7,
            i + 1
        ),
        3 => format!(
            "DELETE DATA {{ <urn:n{}> <urn:new> \"new {}\" }}",
            i - 3,
            i - 3
        ),
        4 => {
            format!("INSERT DATA {{ _:b <urn:p1> {i} . GRAPH <urn:g> {{ _:b <urn:q> \"g{i}\" }} }}")
        }
        _ => format!(
            "INSERT DATA {{ <urn:s{}> <urn:q> {} }}",
            i - 4,
            (i - 4) % 100
        ),
    };
    upd(s, &u);
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), &t).unwrap();
        }
    }
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

fn current(root: &Path) -> String {
    std::fs::read_to_string(root.join("CURRENT"))
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn commits_made_during_the_build_are_carried_over() {
    for persistent in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = match persistent {
            true => Store::open(&root, StoreOptions::default()).unwrap(),
            false => Store::in_memory(StoreOptions::default()),
        };
        let control = Arc::new(Store::in_memory(StoreOptions::default()));
        for x in [&s, &*control] {
            x.load(&[nt(3000, 0)]).unwrap();
        }
        s.compact().unwrap();
        for i in 0..60 {
            churn(&s, i);
            churn(&control, i);
        }
        let head0 = s.head_commit().seq;
        let seen: Arc<Mutex<Vec<State>>> = Default::default();
        s.set_failpoint(
            "compact-built",
            Some(Arc::new({
                let (control, seen) = (control.clone(), seen.clone());
                move |st: &Store| {
                    for i in 60..360 {
                        churn(st, i);
                        churn(&control, i);
                        if i % 61 == 0 {
                            seen.lock().push((st.head_commit().seq, dump(st)));
                        }
                    }
                }
            })),
        );
        let r = s.compact_with(&CompactOptions::default()).unwrap();
        s.set_failpoint("compact-built", None);
        assert_eq!(r.abandoned, None);
        assert_eq!(r.base_commit, head0);
        assert_eq!(r.caught_up_commits, s.head_commit().seq - head0);
        assert!(r.caught_up_commits >= 290, "{r:?}");
        assert_eq!(dump(&s), dump(&control), "persistent: {persistent}");
        assert_eq!(s.snapshot().commit, s.head_commit().seq);
        assert!(!s.snapshot().delta.is_empty());
        assert!(s.writer.lock().tap.is_none());
        if persistent {
            assert_eq!(gens(&root), vec![current(&root)]);
            // the commits of the build read the same from the new generation's log
            for (seq, d) in seen.lock().iter() {
                assert!(*seq > head0);
                assert_eq!(&dump_at(&s, *seq), d, "commit {seq}");
            }
            assert_eq!(dump_at(&s, head0).len(), {
                let (snap, _) = s
                    .snapshot_at(&At::Commit(head0), &Default::default())
                    .unwrap();
                snap.len() as usize
            });
        }
        // later commits, and another compaction with nothing to carry over
        for i in 360..380 {
            churn(&s, i);
            churn(&control, i);
        }
        let r = s.compact_with(&CompactOptions::default()).unwrap();
        assert_eq!(r.caught_up_commits, 0);
        assert!(s.snapshot().delta.is_empty());
        assert_eq!(dump(&s), dump(&control));
        if persistent {
            let head = s.head_commit();
            drop(s);
            let s = Store::open(&root, StoreOptions::default()).unwrap();
            assert_eq!(s.head_commit(), head);
            assert_eq!(dump(&s), dump(&control));
        }
    }
}

#[test]
fn a_reopen_replays_the_carried_commits() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.load(&[nt(1000, 0)]).unwrap();
    s.set_failpoint(
        "compact-built",
        Some(Arc::new(|st: &Store| {
            for i in 0..100 {
                churn(st, i);
            }
        })),
    );
    s.compact().unwrap();
    let (head, d) = (s.head_commit(), dump(&s));
    let delta = s.snapshot().delta.inserts() + s.snapshot().delta.deletes();
    assert!(delta > 0);
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.head_commit(), head);
    assert_eq!(dump(&s), d);
    assert_eq!(
        s.snapshot().delta.inserts() + s.snapshot().delta.deletes(),
        delta
    );
    // and goes on taking commits in the new generation's log
    churn(&s, 100);
    let d = dump(&s);
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(dump(&s), d);
    // the integrity check finds nothing wrong, next to the open store too
    let r = crate::check::check(&root, &Default::default()).unwrap();
    assert_eq!(r.errors, 0, "{}", r.to_text());
    assert_eq!(r.head, Some(s.head_commit().seq));
    // nor with a pinned generation whose carried-over commits are its last ones
    s.create_snapshot("p", &At::Commit(head.seq - 1), None)
        .unwrap();
    s.set_failpoint("compact-built", Some(Arc::new(|st: &Store| churn(st, 101))));
    s.compact().unwrap();
    drop(s);
    let r = crate::check::check(&root, &Default::default()).unwrap();
    assert_eq!(r.errors, 0, "{}", r.to_text());
    let readable = crate::history::reconstructable_offline(
        &root,
        Store::open(&root, StoreOptions::default())
            .unwrap()
            .dataset_id(),
    )
    .unwrap();
    assert!(
        readable
            .iter()
            .any(|&(a, b)| a <= head.seq - 1 && head.seq + 1 <= b),
        "{readable:?}"
    );
}

#[test]
fn writes_and_reads_go_on_during_a_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Arc::new(Store::open(&root, StoreOptions::default()).unwrap());
    let control = Arc::new(Store::in_memory(StoreOptions::default()));
    for x in [&*s, &*control] {
        x.load(&[nt(4000, 0)]).unwrap();
    }
    s.compact().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = std::thread::spawn({
        let (s, control, stop) = (s.clone(), control.clone(), stop.clone());
        move || {
            let mut i = 0;
            while !stop.load(Ordering::Relaxed) || i < 50 {
                churn(&s, i);
                churn(&control, i);
                i += 1;
            }
            i
        }
    });
    let reader = std::thread::spawn({
        let (s, stop) = (s.clone(), stop.clone());
        move || {
            let mut n = 0;
            while !stop.load(Ordering::Relaxed) {
                let snap = s.snapshot();
                let r = crate::sparql::query(
                    snap.clone(),
                    "SELECT ?p (COUNT(*) AS ?n) { ?s ?p ?o } GROUP BY ?p",
                    &QueryOptions::default(),
                )
                .unwrap();
                assert!(!r.rows().is_empty());
                let mut all = 0;
                snap.for_each_quad(|_| {
                    all += 1;
                    Ok(())
                })
                .unwrap();
                assert_eq!(all, snap.len());
                n += 1;
            }
            n
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    let r = s
        .compact_with(&CompactOptions {
            threads: Some(2),
            low_priority: true,
            ..Default::default()
        })
        .unwrap();
    stop.store(true, Ordering::Relaxed);
    let writes = writer.join().unwrap();
    let reads = reader.join().unwrap();
    assert!(writes >= 50 && reads > 0);
    assert_eq!(r.abandoned, None);
    assert_eq!(dump(&s), dump(&control));
    let head = s.head_commit();
    drop(Arc::into_inner(s).unwrap());
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.head_commit(), head);
    assert_eq!(dump(&s), dump(&control));
}

#[test]
fn queries_answer_the_same_after_a_compaction() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[nt(2000, 0)]).unwrap();
    s.compact().unwrap();
    for i in 0..400 {
        churn(&s, i);
    }
    let qs = [
        "SELECT ?c (COUNT(?s) AS ?n) { ?s a ?c } GROUP BY ?c ORDER BY ?c",
        "SELECT ?p (COUNT(*) AS ?n) { ?s ?p ?o } GROUP BY ?p ORDER BY ?p",
        "SELECT (COUNT(DISTINCT ?o) AS ?n) { ?s ?p ?o }",
        "SELECT (COUNT(*) AS ?n) { ?s <urn:q> ?o FILTER(?o > 50) }",
        "SELECT ?s ?o { ?s <urn:p1> ?o } ORDER BY ?s ?o",
    ];
    let answers = |s: &Store| -> Vec<String> {
        qs.iter()
            .map(|q| {
                let r = crate::sparql::query(s.snapshot(), q, &QueryOptions::default()).unwrap();
                format!("{:?}", r.rows())
            })
            .collect()
    };
    let before = answers(&s);
    s.set_failpoint(
        "compact-built",
        Some(Arc::new(|st: &Store| {
            for i in 400..420 {
                churn(st, i);
            }
        })),
    );
    s.compact().unwrap();
    s.set_failpoint("compact-built", None);
    let control = Store::in_memory(StoreOptions::default());
    control.load(&[nt(2000, 0)]).unwrap();
    for i in 0..420 {
        churn(&control, i);
    }
    assert_eq!(answers(&s), answers(&control));
    // and with nothing carried over, the answers do not change at all
    s.compact().unwrap();
    assert!(s.snapshot().delta.is_empty());
    assert_eq!(answers(&s), answers(&control));
    assert_ne!(before, answers(&s));
}

#[test]
fn a_crash_at_any_point_recovers() {
    for point in [
        "compact-started",
        "compact-built",
        "compact-caught-up",
        "compact-before-current",
        "after",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let crash = dir.path().join("crash");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        s.load(&[nt(1500, 0)]).unwrap();
        s.compact().unwrap();
        for i in 0..30 {
            churn(&s, i);
        }
        s.set_failpoint(
            "compact-built",
            Some(Arc::new(|st: &Store| {
                for i in 30..130 {
                    churn(st, i);
                }
            })),
        );
        let state: Arc<Mutex<Option<State>>> = Default::default();
        if point != "after" && point != "compact-built" {
            s.set_failpoint(
                point,
                Some(Arc::new({
                    let (crash, state) = (crash.clone(), state.clone());
                    move |st: &Store| {
                        *state.lock() = Some((st.snapshot().commit, dump(st)));
                        copy_dir(st.root.as_ref().unwrap(), &crash);
                    }
                })),
            );
        }
        if point == "compact-built" {
            s.set_failpoint(
                point,
                Some(Arc::new({
                    let (crash, state) = (crash.clone(), state.clone());
                    move |st: &Store| {
                        for i in 30..130 {
                            churn(st, i);
                        }
                        *state.lock() = Some((st.snapshot().commit, dump(st)));
                        copy_dir(st.root.as_ref().unwrap(), &crash);
                    }
                })),
            );
        }
        s.compact().unwrap();
        if point == "after" {
            *state.lock() = Some((s.head_commit().seq, dump(&s)));
            copy_dir(&root, &crash);
        }
        let (head, d) = state.lock().clone().unwrap();
        let c = Store::open(&crash, StoreOptions::default()).unwrap();
        assert_eq!(c.head_commit().seq, head, "{point}");
        assert_eq!(dump(&c), d, "{point}");
        assert_eq!(gens(&crash), vec![current(&crash)], "{point}");
        // the recovered store takes writes and compacts
        churn(&c, 130);
        c.compact().unwrap();
        assert_eq!(gens(&crash), vec![current(&crash)], "{point}");
    }
}

#[test]
fn a_bulk_commit_during_the_build_makes_it_moot() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let opts = StoreOptions {
        bulk_threshold: 1000,
        ..Default::default()
    };
    let s = Store::open(&root, opts.clone()).unwrap();
    s.load(&[nt(500, 0)]).unwrap();
    for i in 0..10 {
        churn(&s, i);
    }
    let reserved: Arc<Mutex<String>> = Default::default();
    s.set_failpoint(
        "compact-built",
        Some(Arc::new({
            let reserved = reserved.clone();
            move |st: &Store| {
                *reserved.lock() =
                    format!("gen-{:04}", st.compaction.reserved.load(Ordering::Relaxed));
                churn(st, 10);
                st.load(&[nt(2000, 10_000)]).unwrap();
                churn(st, 11);
            }
        })),
    );
    let before = current(&root);
    let r = s.compact().map(|_| ());
    assert!(r.is_ok());
    s.set_failpoint("compact-built", None);
    let reserved = reserved.lock().clone();
    let now = current(&root);
    assert_ne!(now, before);
    assert_ne!(now, reserved, "the bulk commit took a number of its own");
    assert!(!root.join(&reserved).exists());
    assert_eq!(gens(&root), vec![now.clone()]);
    assert!(s.writer.lock().tap.is_none());
    let d = dump(&s);
    // the report says so
    s.set_failpoint(
        "compact-built",
        Some(Arc::new(|st: &Store| {
            st.load(&[nt(2000, 20_000)]).unwrap();
        })),
    );
    let r = s.compact_with(&CompactOptions::default()).unwrap();
    s.set_failpoint("compact-built", None);
    assert!(r.abandoned.is_some(), "{r:?}");
    assert!(dump(&s).len() > d.len());
    // a compaction afterwards works
    let r = s.compact_with(&CompactOptions::default()).unwrap();
    assert_eq!(r.abandoned, None);
    assert_eq!(gens(&root), vec![current(&root)]);
    drop(s);
    let s = Store::open(&root, opts).unwrap();
    assert_eq!(gens(&root), vec![current(&root)]);
    assert!(dump(&s).len() > d.len());
}

#[test]
fn a_cancelled_compaction_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.load(&[nt(500, 0)]).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    s.set_failpoint(
        "compact-started",
        Some(Arc::new({
            let cancel = cancel.clone();
            move |_: &Store| cancel.store(true, Ordering::Relaxed)
        })),
    );
    let before = gens(&root);
    let e = s
        .compact_with(&CompactOptions {
            cancel: Some(cancel.clone()),
            ..Default::default()
        })
        .unwrap_err();
    assert!(matches!(e, Error::Cancelled), "{e}");
    assert_eq!(gens(&root), before);
    assert!(s.writer.lock().tap.is_none());
    assert!(!s.compaction.running.load(Ordering::Relaxed));
    churn(&s, 0);
    s.set_failpoint("compact-started", None);
    s.compact().unwrap();
    assert_ne!(gens(&root), before);
}

#[test]
fn only_one_compaction_runs_at_a_time() {
    let s = Arc::new(Store::in_memory(StoreOptions::default()));
    s.load(&[nt(200, 0)]).unwrap();
    let inner: Arc<Mutex<Option<String>>> = Default::default();
    s.set_failpoint(
        "compact-built",
        Some(Arc::new({
            let inner = inner.clone();
            move |st: &Store| {
                *inner.lock() = Some(st.compact().unwrap_err().to_string());
                assert_eq!(
                    st.compaction_blocker(false).map(|b| b.reason),
                    Some("running")
                );
            }
        })),
    );
    s.compact().unwrap();
    assert!(inner.lock().as_deref().unwrap().contains("already running"));
}

#[test]
fn named_snapshots_and_history_stay_readable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.load(&[nt(300, 0)]).unwrap();
    for i in 0..10 {
        churn(&s, i);
    }
    let mid = s.head_commit().seq - 5;
    let mid_dump = dump_at(&s, mid);
    s.create_snapshot("mid", &At::Commit(mid), None).unwrap();
    let during: Arc<Mutex<Option<State>>> = Default::default();
    s.set_failpoint(
        "compact-built",
        Some(Arc::new({
            let during = during.clone();
            move |st: &Store| {
                for i in 10..20 {
                    churn(st, i);
                }
                // a pin made during the build, at a commit the build does not hold
                let seq = st.head_commit().seq;
                st.create_snapshot("during", &At::Head, None).unwrap();
                *during.lock() = Some((seq, dump(st)));
                churn(st, 20);
            }
        })),
    );
    s.compact().unwrap();
    s.set_failpoint("compact-built", None);
    let (seq, d) = during.lock().clone().unwrap();
    assert_eq!(dump_at(&s, mid), mid_dump);
    assert_eq!(dump_at(&s, seq), d);
    // the old generation is kept for the pin in its middle
    assert_eq!(gens(&root).len(), 2);
    // after the next compaction, the pin made during the build still reads, from the
    // generation whose log holds it
    churn(&s, 21);
    s.compact().unwrap();
    assert_eq!(dump_at(&s, seq), d);
    assert_eq!(dump_at(&s, mid), mid_dump);
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(dump_at(&s, seq), d);
    assert_eq!(dump_at(&s, mid), mid_dump);
}

#[test]
fn the_retention_window_is_never_cut_short_by_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let opts = StoreOptions {
        history_max_generations: 2,
        ..Default::default()
    };
    let s = Store::open(&root, opts).unwrap();
    s.load(&[nt(100, 0)]).unwrap();
    s.set_retention(Retention {
        keep_commits: Some(1_000_000),
        ..Default::default()
    })
    .unwrap();
    let mut i = 0;
    let mut step = |s: &Store| {
        for _ in 0..3 {
            churn(s, i);
            i += 1;
        }
    };
    step(&s);
    assert_eq!(s.compaction_blocker(false), None);
    s.compact().unwrap();
    step(&s);
    assert_eq!(s.compaction_blocker(false), None);
    s.compact().unwrap();
    step(&s);
    // a third would make the window drop the first generation
    let b = s.compaction_blocker(false).unwrap();
    assert_eq!(b.reason, "history", "{b:?}");
    assert_eq!(gens(&root).len(), 3);
    // a manual compaction goes ahead
    s.compact().unwrap();
    assert_eq!(gens(&root).len(), 3);
    // without the window, nothing waits
    s.set_retention(Retention::default()).unwrap();
    step(&s);
    assert_eq!(s.compaction_blocker(false), None);
}

#[test]
fn a_backup_lease_and_too_little_disk_make_it_wait() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        s.load(&[nt(100, 0)]).unwrap();
        let cur = commit::generation_number(&s.snapshot().generation.name);
        let id = s
            .history
            .as_ref()
            .unwrap()
            .lock()
            .lease(cur, "backup:nightly");
        let b = s.compaction_blocker(false).unwrap();
        assert_eq!(b.reason, "backup");
        assert!(b.detail.contains("backup:nightly"), "{b:?}");
        s.history.as_ref().unwrap().lock().leases.remove(&id);
        assert_eq!(s.compaction_blocker(true), None);
        s.compaction.rebuilding.store(true, Ordering::Relaxed);
        assert_eq!(s.compaction_blocker(false).unwrap().reason, "bulk-load");
        s.compaction.rebuilding.store(false, Ordering::Relaxed);
    }
    let s = Store::open(
        &root,
        StoreOptions {
            min_free_disk_bytes: Some(u64::MAX / 4),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(s.compaction_blocker(true).unwrap().reason, "disk");
    assert_eq!(s.compaction_blocker(false), None);
}

#[test]
fn the_quota_does_not_count_the_generation_being_built() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.load(&[nt(3000, 0)]).unwrap();
    s.compact().unwrap();
    s.quota.invalidate();
    let used = s.quota.used();
    // room for small commits, not for a second copy of the index
    s.set_quota(Some(used + (64 << 10))).unwrap();
    let ok = Arc::new(AtomicUsize::new(0));
    s.set_failpoint(
        "compact-built",
        Some(Arc::new({
            let ok = ok.clone();
            move |st: &Store| {
                let building = st.root.as_ref().unwrap().join(format!(
                    "gen-{:04}",
                    st.compaction.reserved.load(Ordering::Relaxed)
                ));
                assert!(dir_size(&building) > 64 << 10);
                st.quota.invalidate();
                for i in 0..20 {
                    upd(st, &format!("INSERT DATA {{ <urn:x{i}> <urn:p> {i} }}"));
                    ok.fetch_add(1, Ordering::Relaxed);
                }
            }
        })),
    );
    s.compact().unwrap();
    assert_eq!(ok.load(Ordering::Relaxed), 20);
    // and the old generation is gone again
    s.quota.invalidate();
    assert!(s.quota.used() < used + (64 << 10));
}

#[test]
fn settings_are_kept_in_compaction_json() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert!(s.compaction_settings().is_empty());
    let mut own = CompactionSettings::default();
    own.set("deltaRatio", "0.02").unwrap();
    own.set("enabled", "false").unwrap();
    s.set_compaction_settings(Some(own.clone())).unwrap();
    let j: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join(COMPACTION_FILE)).unwrap()).unwrap();
    assert_eq!(
        j,
        serde_json::json!({"format": 1, "deltaRatio": 0.02, "enabled": false})
    );
    let bad = CompactionSettings {
        delta_ratio: Some(f64::NAN),
        ..Default::default()
    };
    assert!(s.set_compaction_settings(Some(bad)).is_err());
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.compaction_settings(), own);
    let p = CompactionPolicy::default().with(&s.compaction_settings());
    assert!(!p.enabled && p.delta_ratio == 0.02 && p.min_delta_quads == 10_000);
    s.set_compaction_settings(None).unwrap();
    assert!(!root.join(COMPACTION_FILE).exists());
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert!(s.compaction_settings().is_empty());
    // a damaged file fails the open rather than being ignored
    drop(s);
    std::fs::write(root.join(COMPACTION_FILE), b"{\"deltaRatio\": \"x\"}").unwrap();
    assert!(Store::open(&root, StoreOptions::default()).is_err());
}

#[test]
fn measures_follow_commits_and_compactions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let now = Arc::new(AtomicI64::new(5_000_000_000_000));
    let clock = {
        let now = now.clone();
        Arc::new(move || now.load(Ordering::SeqCst)) as Clock
    };
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.set_clock(clock.clone());
    s.load(&[nt(100, 0)]).unwrap();
    s.compact().unwrap();
    let m = s.compaction_measures();
    assert_eq!((m.base_seq, m.head, m.delta_quads), (1, 1, 0));
    assert_eq!(m.oldest_change_ms, None);
    assert_eq!(m.base_quads, 300);
    let p = CompactionPolicy::default();
    assert_eq!(p.verdict(&m), None);
    now.fetch_add(1000, Ordering::SeqCst);
    churn(&s, 0);
    now.fetch_add(5000, Ordering::SeqCst);
    churn(&s, 1);
    now.fetch_add(2000, Ordering::SeqCst);
    let m = s.compaction_measures();
    assert_eq!((m.base_seq, m.head, m.delta_quads), (1, 3, 2));
    assert_eq!(m.oldest_change_ms, Some(7000));
    assert_eq!(m.idle_ms, Some(2000));
    assert!(m.wal_bytes > 0 && m.delta_bytes > 0);
    // a day later the age trigger fires
    now.fetch_add(86_400_000, Ordering::SeqCst);
    let t = p.verdict(&s.compaction_measures()).unwrap();
    assert_eq!(t.kind, TriggerKind::Age);
    // a compaction with commits during its build starts the clock at the first of them
    s.set_failpoint(
        "compact-built",
        Some(Arc::new({
            let now = now.clone();
            move |st: &Store| {
                now.fetch_add(10, Ordering::SeqCst);
                churn(st, 2);
                now.fetch_add(10, Ordering::SeqCst);
                churn(st, 3);
            }
        })),
    );
    s.compact().unwrap();
    s.set_failpoint("compact-built", None);
    let m = s.compaction_measures();
    assert_eq!((m.base_seq, m.head), (3, 5));
    assert_eq!(m.oldest_change_ms, Some(10));
    assert_eq!(m.idle_ms, Some(0));
    drop(s);
    // a reopen finds the same
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.set_clock(clock);
    let m = s.compaction_measures();
    assert_eq!((m.base_seq, m.head), (3, 5));
    assert_eq!(m.oldest_change_ms, Some(10));
    assert_eq!(m.idle_ms, Some(0));
}

#[test]
fn a_build_can_be_held_to_a_write_rate() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[nt(2000, 0)]).unwrap();
    let t = std::time::Instant::now();
    let r = s
        .compact_with(&CompactOptions {
            threads: Some(1),
            io_bytes_per_sec: Some(200 << 10),
            ..Default::default()
        })
        .unwrap();
    // the generation is about 100 KB or more: at 200 KiB/s it takes a while
    let bytes = s.snapshot().generation.disk_bytes();
    let due = bytes as f64 / (200 << 10) as f64;
    assert!(t.elapsed().as_secs_f64() >= due * 0.5, "{r:?} {bytes}");
}

#[cfg(feature = "text")]
#[test]
fn full_text_search_sees_the_commits_carried_over() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.load(&[nt(300, 0)]).unwrap();
    s.enable_text(crate::text::TextConfig::default()).unwrap();
    s.set_failpoint(
        "compact-built",
        Some(Arc::new(|st: &Store| {
            upd(st, "INSERT DATA { <urn:t1> <urn:label> \"carried zebra\" }");
        })),
    );
    s.compact().unwrap();
    s.set_failpoint("compact-built", None);
    upd(&s, "INSERT DATA { <urn:t2> <urn:label> \"later zebra\" }");
    let hits = |s: &Store| {
        let r = crate::sparql::query(
            s.snapshot(),
            "PREFIX text: <http://jena.apache.org/text#> SELECT ?s { ?s text:query \"zebra\" } ORDER BY ?s",
            &QueryOptions::default(),
        )
        .unwrap();
        r.rows().len()
    };
    assert_eq!(hits(&s), 2);
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(hits(&s), 2);
}
