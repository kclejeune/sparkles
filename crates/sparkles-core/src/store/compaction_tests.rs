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

/// Write `i` of a sequence like [`churn`]'s that uses only terms of the vocabulary of a
/// store loaded with [`nt`] (and integers, which are inline), so that a partial
/// compaction can fold it: new and deleted quads, types, a named graph, blank nodes and
/// several quads in one commit.
fn churn_known(s: &Store, i: usize) {
    let u = match i % 6 {
        0 => format!("INSERT DATA {{ <urn:s{i}> <urn:q> {} }}", 1000 + i),
        1 => format!("DELETE DATA {{ <urn:s{i}> <urn:q> {} }}", i % 100),
        2 => format!(
            "INSERT DATA {{ <urn:s{i}> a <urn:C{}> . GRAPH <urn:s2> {{ <urn:s{i}> <urn:q> 7 }} }} ; DELETE DATA {{ <urn:s{}> <urn:p{}> \"v{}\" }}",
            (i + 1) % 5,
            i + 1,
            (i + 1) % 7,
            i + 1
        ),
        3 => format!(
            "DELETE DATA {{ <urn:s{}> <urn:q> {} }}",
            i - 3,
            1000 + i - 3
        ),
        4 => format!(
            "INSERT DATA {{ _:b <urn:q> {i} . GRAPH <urn:s2> {{ _:b <urn:p1> \"v{i}\" }} }}"
        ),
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
        readable.iter().any(|&(a, b)| a < head.seq && head.seq < b),
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
    crash_at_each_point(churn, PartialMode::Off);
}

#[test]
fn a_crash_during_a_partial_compaction_recovers() {
    crash_at_each_point(churn_known, PartialMode::Always);
}

/// Crash a compaction at each of its points, with commits made by `churn` before and
/// during the build, and check what a reopen recovers.
fn crash_at_each_point(churn: fn(&Store, usize), mode: PartialMode) {
    for point in [
        "compact-started",
        "compact-built",
        "compact-indexed",
        "compact-caught-up",
        "compact-before-current",
        "after",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let crash = dir.path().join("crash");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        s.set_compaction_settings(Some(CompactionSettings {
            partial: Some(mode),
            ..Default::default()
        }))
        .unwrap();
        s.load(&[nt(1500, 0)]).unwrap();
        s.compact().unwrap();
        for i in 0..30 {
            churn(&s, i);
        }
        s.set_failpoint(
            "compact-built",
            Some(Arc::new(move |st: &Store| {
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
        let r = s.compact_with(&CompactOptions::default()).unwrap();
        let expected = if mode == PartialMode::Off {
            "full"
        } else {
            "partial"
        };
        assert_eq!(r.mode, expected, "{point}: {r:?}");
        assert_eq!(r.caught_up_commits, 100, "{point}");
        if point == "after" {
            *state.lock() = Some((s.head_commit().seq, dump(&s)));
            copy_dir(&root, &crash);
        }
        let (head, d) = state.lock().clone().unwrap();
        let c = Store::open(&crash, StoreOptions::default()).unwrap();
        assert_eq!(c.head_commit().seq, head, "{point}");
        assert_eq!(dump(&c), d, "{point}");
        // the change log lost nothing: the crashed copy records what the original did
        let changes = |s: &Store| -> Vec<(u64, DiffOp, String)> {
            let r = s
                .history_changes(&HistoryQuery {
                    to: Some(HistoryBound::Commit(head)),
                    ..Default::default()
                })
                .unwrap();
            assert!(
                r.unrecorded.iter().all(|u| u.to <= 1),
                "{point}: {:?}",
                r.unrecorded
            );
            r.changes
                .iter()
                .map(|x| (x.commit.seq, x.op, crate::annotations::nquads_line(&x.quad)))
                .collect()
        };
        assert_eq!(changes(&c), changes(&s), "{point}");
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
fn rebuilds_are_counted_by_reason() {
    let s = Store::in_memory(StoreOptions {
        bulk_threshold: 50,
        ..Default::default()
    });
    assert_eq!(s.rebuilds(RebuildReason::Bulk).count(), 0);
    // the first load and a large one are bulk commits; a small update goes to the delta
    s.load(&[nt(10, 0)]).unwrap();
    s.load(&[nt(100, 10)]).unwrap();
    upd(&s, "INSERT DATA { <urn:x> <urn:y> 1 }");
    let bulk = s.rebuilds(RebuildReason::Bulk);
    assert_eq!(bulk.count(), 2, "{bulk:?}");
    assert_eq!(s.rebuilds(RebuildReason::Compact).count(), 0);
    s.compact().unwrap();
    let c = s.rebuilds(RebuildReason::Compact);
    assert_eq!(c.count(), 1, "{c:?}");
    assert!(c.sum_seconds > 0.0);
    // the rebuild lands in exactly one bucket; which one depends on the machine's load
    assert_eq!(c.buckets.iter().filter(|&&n| n == 1).count(), 1, "{c:?}");
    // a dry run publishes nothing and counts nothing
    let dry = crate::guard::WriteOptions {
        dry_run: Some(crate::preview::DryRun::default()),
        ..Default::default()
    };
    match s.load_with(&[nt(100, 200)], CommitKind::Load, &dry) {
        Err(Error::DryRun(p)) => assert!(p.commit.unwrap().bulk),
        r => panic!("not a dry run: {r:?}"),
    }
    assert_eq!(s.rebuilds(RebuildReason::Bulk).count(), 2);
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

/// A store at `root` loaded with `n` subjects of [`nt`], whose compactions are partial as
/// `mode` says.
fn partial_store(root: &Path, n: usize, mode: PartialMode) -> Store {
    let s = Store::open(root, StoreOptions::default()).unwrap();
    s.set_compaction_settings(Some(CompactionSettings {
        partial: Some(mode),
        ..Default::default()
    }))
    .unwrap();
    s.load(&[nt(n, 0)]).unwrap();
    s
}

/// The statistics of the current generation with their ids written as terms, which
/// compares two generations whose vocabularies differ.
fn stats_terms(s: &Store) -> serde_json::Value {
    let snap = s.snapshot();
    let st = &snap.generation.stats;
    let t = |id: u64| {
        let id = if Id(id).tag() as u8 == 0xF {
            Id::vocab(id & crate::id::PAYLOAD_MASK)
        } else {
            Id(id)
        };
        snap.term(id)
            .map_or_else(|| format!("{id:?}"), |t| format!("class or term {t}"))
    };
    serde_json::json!({
        "quads": st.quads,
        "subjects": st.distinct_subjects,
        "predicates": st.distinct_predicates,
        "objects": st.distinct_objects,
        "perPredicate": st.predicates.iter().map(|p| (t(p.p), p.count, p.distinct_subjects, p.distinct_objects)).collect::<Vec<_>>(),
        "graphs": st.graphs.iter().map(|(g, n)| (t(*g), *n)).collect::<Vec<_>>(),
        "classes": st.classes.iter().map(|(c, n)| (t(*c), *n)).collect::<Vec<_>>(),
        "charsets": st.charsets.iter().map(|c| (c.preds.iter().map(|p| t(*p)).collect::<Vec<_>>(), c.subjects, c.triples.clone())).collect::<Vec<_>>(),
        "charsetOthers": st.charset_others,
    })
}

/// Queries whose answers must not depend on how a generation was written.
const DIFF_QUERIES: [&str; 8] = [
    "SELECT ?p (COUNT(*) AS ?n) { ?s ?p ?o } GROUP BY ?p ORDER BY ?p",
    "SELECT ?c (COUNT(DISTINCT ?s) AS ?n) { ?s a ?c } GROUP BY ?c ORDER BY ?c",
    "SELECT (COUNT(DISTINCT ?s) AS ?n) (COUNT(DISTINCT ?o) AS ?m) { ?s ?p ?o }",
    "SELECT ?g (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g ORDER BY ?g",
    "SELECT ?s ?o { ?s <urn:q> ?o FILTER(?o > 50 && ?o < 100060) } ORDER BY ?s ?o LIMIT 500",
    "SELECT ?s ?c ?v { ?s a ?c ; <urn:p3> ?v } ORDER BY DESC(?s) LIMIT 200",
    "SELECT (COUNT(*) AS ?n) { ?s <urn:q> ?o . ?s a <urn:C2> }",
    "ASK { <urn:s5> <urn:q> 100007 }",
];

fn diff_answers(s: &Store) -> Vec<String> {
    DIFF_QUERIES
        .iter()
        .map(|q| {
            let r = crate::sparql::query(s.snapshot(), q, &QueryOptions::default())
                .unwrap_or_else(|e| panic!("{q}: {e}"));
            format!("{:?}", r.rows())
        })
        .collect()
}

/// Compact `p` (partially) and `f` (in full), which took the same commits, and compare
/// them: the quads, query answers, statistics, `sparkles check` and a reopen.
fn compare_partial_and_full(p: Store, f: Store, round: &str) -> (Store, Store) {
    let rp = p.compact_with(&CompactOptions::default()).unwrap();
    let rf = f.compact_with(&CompactOptions::default()).unwrap();
    assert_eq!(rp.mode, "partial", "{round}: {rp:?}");
    assert_eq!(rf.mode, "full", "{round}: {rf:?}");
    assert_eq!(rp.quads, rf.quads, "{round}");
    assert!(p.snapshot().delta.is_empty());
    assert_eq!(dump(&p), dump(&f), "{round}");
    assert_eq!(diff_answers(&p), diff_answers(&f), "{round}");
    assert_eq!(stats_terms(&p), stats_terms(&f), "{round}");
    let mut out = Vec::new();
    for s in [p, f] {
        let root = s.root.clone().unwrap();
        let rep = crate::check::check(&root, &Default::default()).unwrap();
        assert_eq!(
            rep.status,
            crate::check::Status::Ok,
            "{round}: {}",
            rep.to_text()
        );
        let (head, d) = (s.head_commit(), dump(&s));
        drop(s);
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!((s.head_commit(), dump(&s)), (head, d), "{round}");
        out.push(s);
    }
    let f = out.pop().unwrap();
    (out.pop().unwrap(), f)
}

#[test]
fn a_partial_compaction_matches_a_full_one() {
    let dir = tempfile::tempdir().unwrap();
    // 90,000 quads: three blocks per permutation
    let n = 30_000;
    let mut p = partial_store(&dir.path().join("p"), n, PartialMode::Always);
    let mut f = partial_store(&dir.path().join("f"), n, PartialMode::Off);
    let both = |p: &Store, f: &Store, u: &str| {
        upd(p, u);
        upd(f, u);
    };
    let blocks = |s: &Store| -> Vec<usize> {
        let snap = s.snapshot();
        Perm::ALL
            .iter()
            .map(|&x| snap.perm(x).blocks.len())
            .collect()
    };
    // one subject gains 40,000 quads, so that its block splits; the subjects s10000 to
    // s19999 lose every quad, which empties blocks; types, a named graph and a blank
    // node change
    let mut ins = String::new();
    for j in 0..40_000 {
        ins += &format!("<urn:s5> <urn:q> {} . ", 100_000 + j);
    }
    both(&p, &f, &format!("INSERT DATA {{ {ins} }}"));
    both(
        &p,
        &f,
        "DELETE { ?s ?p ?o } WHERE { ?s ?p ?o FILTER(STRSTARTS(STR(?s), \"urn:s1\") && STRLEN(STR(?s)) = 10) }",
    );
    both(
        &p,
        &f,
        "INSERT DATA { <urn:s7> a <urn:C3> . <urn:s8> a <urn:C4> . _:x <urn:q> 3 . _:x a <urn:C1> . GRAPH <urn:s3> { <urn:s4> <urn:q> 1 . <urn:s5> <urn:p1> \"v6\" } } ; DELETE DATA { <urn:s9> a <urn:C4> }",
    );
    (p, f) = compare_partial_and_full(p, f, "concentrated");
    let b = blocks(&p);
    assert!(b[Perm::Spo.index()] >= 3, "{b:?}");
    // changes spread over every block, on top of the partial generation
    let mut ins = String::new();
    let mut del = String::new();
    for i in (0..n).filter(|i| !(10_000..20_000).contains(i)) {
        if i % 7 == 0 {
            ins += &format!("<urn:s{i}> <urn:q> {} . ", 200_000 + i);
        }
        if i % 11 == 0 {
            del += &format!("<urn:s{i}> <urn:p{}> \"v{i}\" . ", i % 7);
        }
    }
    both(
        &p,
        &f,
        &format!("INSERT DATA {{ {ins} }} ; DELETE DATA {{ {del} }}"),
    );
    (p, f) = compare_partial_and_full(p, f, "spread");
    // everything goes, then a few quads of known terms come back
    both(&p, &f, "DELETE WHERE { ?s ?p ?o }");
    both(&p, &f, "DELETE WHERE { GRAPH ?g { ?s ?p ?o } }");
    (p, f) = compare_partial_and_full(p, f, "emptied");
    assert_eq!(blocks(&p), vec![0; 7]);
    both(
        &p,
        &f,
        "INSERT DATA { <urn:s1> <urn:q> 5 . <urn:s2> a <urn:C0> . GRAPH <urn:s3> { <urn:s1> <urn:p1> \"v1\" } }",
    );
    let (p, _f) = compare_partial_and_full(p, f, "refilled");
    assert_eq!(p.snapshot().len(), 3);
}

#[test]
fn the_automatic_choice_and_its_reasons() {
    let dir = tempfile::tempdir().unwrap();
    // 300,000 quads: ten blocks per permutation
    let s = partial_store(&dir.path().join("db"), 100_000, PartialMode::Auto);
    // a few changes to known terms touch few blocks
    upd(
        &s,
        "INSERT DATA { <urn:s5> <urn:q> 100001 } ; DELETE DATA { <urn:s6> <urn:q> 6 }",
    );
    let r = s.compact_with(&CompactOptions::default()).unwrap();
    assert_eq!(
        (r.mode.as_str(), r.full_reason.as_deref()),
        ("partial", None)
    );
    assert!(
        r.blocks_rewritten > 0 && r.blocks_rewritten * 4 < r.blocks_copied,
        "{r:?}"
    );
    // a new term needs a full build
    upd(&s, "INSERT DATA { <urn:new> <urn:q> 1 }");
    let r = s.compact_with(&CompactOptions::default()).unwrap();
    assert_eq!(r.mode, "full");
    assert!(r.full_reason.unwrap().contains("not in the vocabulary"));
    // changes spread over most blocks still cost less than a full build
    let mut ins = String::new();
    for i in (0..100_000).step_by(1_000) {
        ins += &format!(
            "<urn:s{i}> <urn:q> {} . <urn:s{i}> a <urn:C{}> . ",
            500_000 + i,
            (i + 1) % 5
        );
    }
    upd(&s, &format!("INSERT DATA {{ {ins} }}"));
    let r = s.compact_with(&CompactOptions::default()).unwrap();
    assert_eq!(r.mode, "partial", "{r:?}");
    assert!(r.blocks_rewritten > r.blocks_copied, "{r:?}");
    upd(&s, &format!("DELETE DATA {{ {ins} }}"));
    let r = s
        .compact_with(&CompactOptions {
            partial: Some(PartialMode::Always),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.mode, "partial");
    // and never when it is off
    upd(&s, "INSERT DATA { <urn:s5> <urn:q> 100002 }");
    let r = s
        .compact_with(&CompactOptions {
            partial: Some(PartialMode::Off),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        (r.mode.as_str(), r.full_reason.as_deref()),
        ("full", Some("partial compaction is off"))
    );
}

#[test]
fn commits_during_a_partial_build_are_carried_over() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = partial_store(&root, 3000, PartialMode::Always);
    let control = Arc::new(Store::in_memory(StoreOptions::default()));
    control.load(&[nt(3000, 0)]).unwrap();
    for i in 0..60 {
        churn_known(&s, i);
        churn_known(&control, i);
    }
    let head0 = s.head_commit().seq;
    let seen: Arc<Mutex<Vec<State>>> = Default::default();
    s.set_failpoint(
        "compact-built",
        Some(Arc::new({
            let (control, seen) = (control.clone(), seen.clone());
            move |st: &Store| {
                for i in 60..360 {
                    // new terms too: the build read the delta before them
                    if i % 50 == 0 {
                        upd(
                            st,
                            &format!("INSERT DATA {{ <urn:n{i}> <urn:new> \"new {i}\" }}"),
                        );
                        upd(
                            &control,
                            &format!("INSERT DATA {{ <urn:n{i}> <urn:new> \"new {i}\" }}"),
                        );
                    }
                    churn_known(st, i);
                    churn_known(&control, i);
                    if i % 61 == 0 {
                        seen.lock().push((st.head_commit().seq, dump(st)));
                    }
                }
            }
        })),
    );
    let r = s.compact_with(&CompactOptions::default()).unwrap();
    s.set_failpoint("compact-built", None);
    assert_eq!(r.mode, "partial", "{r:?}");
    assert_eq!(r.base_commit, head0);
    assert_eq!(r.caught_up_commits, s.head_commit().seq - head0);
    assert_eq!(dump(&s), dump(&control));
    for (seq, d) in seen.lock().iter() {
        assert_eq!(&dump_at(&s, *seq), d, "commit {seq}");
    }
    let rep = crate::check::check(&root, &Default::default()).unwrap();
    assert_eq!(rep.status, crate::check::Status::Ok, "{}", rep.to_text());
    let head = s.head_commit();
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.head_commit(), head);
    assert_eq!(dump(&s), dump(&control));
    // the carried new terms make the next compaction a full one
    let r = s.compact_with(&CompactOptions::default()).unwrap();
    assert_eq!(r.mode, "full");
    assert_eq!(dump(&s), dump(&control));
}

/// Partial against full compaction of the benchmark's 10.5M-triple data set, with
/// deltas of 1,000, 10,000 and 100,000 quads that are concentrated in a few subjects or
/// spread over all of them. Each delta is 90% new `foaf:knows` links and 10% deleted
/// `foaf:age` quads, all of known terms. `SPARKLES_BENCH_NT` is the data (`data.nt` of
/// `mise run bench`) and `SPARKLES_BENCH_DIR` a directory on disk for the stores. Run
/// it in release mode: `cargo test --release -p sparkles-core --lib -- --ignored
/// --nocapture partial_compaction_at_scale`.
#[test]
#[ignore]
fn partial_compaction_at_scale() {
    let (Ok(nt), Ok(work)) = (
        std::env::var("SPARKLES_BENCH_NT"),
        std::env::var("SPARKLES_BENCH_DIR"),
    ) else {
        println!("set SPARKLES_BENCH_NT and SPARKLES_BENCH_DIR");
        return;
    };
    let work = PathBuf::from(work);
    let base = work.join("base");
    if !base.join("CURRENT").exists() {
        let _ = std::fs::remove_dir_all(&base);
        let t = std::time::Instant::now();
        let s = Store::open(&base, StoreOptions::default()).unwrap();
        s.load(&[Source::from_path(Path::new(&nt), None).unwrap()])
            .unwrap();
        println!(
            "loaded {} quads in {:.1} s",
            s.snapshot().len(),
            t.elapsed().as_secs_f64()
        );
    }
    let person = |n: u64| format!("<http://example.org/person/{n}>");
    let knows = "<http://xmlns.com/foaf/0.1/knows>";
    let age = "<http://xmlns.com/foaf/0.1/age>";
    println!(
        "{:<20} {:>7} {:>8} {:>9} {:>9} {:>9} {:>7}  auto",
        "delta", "forced", "mode", "rewritten", "build ms", "total ms", "lock ms"
    );
    // `SPARKLES_BENCH_CASES` limits the cases to those whose label contains it, and
    // `SPARKLES_BENCH_MODES` the modes (`always,off`)
    let cases = std::env::var("SPARKLES_BENCH_CASES").unwrap_or_default();
    let modes = std::env::var("SPARKLES_BENCH_MODES").unwrap_or_else(|_| "always,off".into());
    for size in [1_000u64, 10_000, 100_000] {
        for concentrated in [true, false] {
            let label = format!(
                "{size} {}",
                if concentrated {
                    "concentrated"
                } else {
                    "spread"
                }
            );
            if !label.contains(cases.as_str()) {
                continue;
            }
            for mode in [PartialMode::Always, PartialMode::Off] {
                if !modes.split(',').any(|m| m == mode.as_str()) {
                    continue;
                }
                let run = work.join("run");
                let _ = std::fs::remove_dir_all(&run);
                copy_dir(&base, &run);
                // the copy's dirty pages would otherwise be written back while the
                // compaction syncs its files
                // SAFETY: sync has no preconditions
                #[cfg(unix)]
                unsafe {
                    libc::sync()
                };
                let s = Store::open(&run, StoreOptions::default()).unwrap();
                let mut seed = 0x5eed_u64 + size;
                let mut rnd = |m: u64| {
                    seed = seed
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    (seed >> 33) % m
                };
                let (ins, del) = (size * 9 / 10, size / 10);
                let mut u = String::from("INSERT DATA { ");
                for j in 0..ins {
                    let (sub, obj) = if concentrated {
                        (500_000 + j % 10, 600_000 + j / 10)
                    } else {
                        (rnd(1_000_000), rnd(1_000_000))
                    };
                    u += &format!("{} {knows} {} . ", person(sub), person(obj));
                }
                u += "}";
                upd(&s, &u);
                let mut u = format!("DELETE {{ ?s {age} ?a }} WHERE {{ VALUES ?s {{ ");
                for j in 0..del {
                    let sub = if concentrated {
                        700_000 + j
                    } else {
                        rnd(1_000_000)
                    };
                    u += &person(sub);
                    u += " ";
                }
                u += &format!("}} ?s {age} ?a }}");
                upd(&s, &u);
                let snap = s.snapshot();
                let auto = match crate::store::partial::plan(&snap) {
                    Err(why) => format!("full: {why}"),
                    Ok(p) => {
                        let (pm, fm) = p.estimate_ms();
                        format!(
                            "{} ({:.1}% of blocks, {:.3}x, estimated {pm:.0} against {fm:.0} ms)",
                            if p.auto_refusal().is_some() {
                                "full"
                            } else {
                                "partial"
                            },
                            p.share() * 100.0,
                            p.fragmentation()
                        )
                    }
                };
                drop(snap);
                let r = s
                    .compact_with(&CompactOptions {
                        partial: Some(mode),
                        ..Default::default()
                    })
                    .unwrap();
                println!(
                    "{label:<20} {:>7} {:>8} {:>9} {:>9.0} {:>9.0} {:>7.1}  {auto}",
                    mode.as_str(),
                    r.mode,
                    format!(
                        "{}/{}",
                        r.blocks_rewritten,
                        r.blocks_rewritten + r.blocks_copied
                    ),
                    r.build_ms,
                    r.total_ms,
                    r.lock_ms
                );
                drop(s);
                let _ = std::fs::remove_dir_all(&run);
            }
        }
    }
}

/// A feature with a point geometry and a label: three quads.
#[cfg(feature = "geo")]
fn feature(i: usize) -> String {
    let geo = "http://www.opengis.net/ont/geosparql#";
    let (x, y) = (
        (i % 3600) as f64 / 10.0 - 180.0,
        (i / 3600 % 1800) as f64 / 10.0 - 90.0,
    );
    format!(
        "<urn:f{i}> <{geo}hasGeometry> <urn:g{i}> .\n<urn:g{i}> <{geo}asWKT> \"POINT({x} {y})\"^^<{geo}wktLiteral> .\n<urn:f{i}> <http://www.w3.org/2000/01/rdf-schema#label> \"feature {i}\" .\n"
    )
}

#[cfg(feature = "geo")]
fn insert_feature(s: &Store, i: usize) {
    upd(
        s,
        &format!("INSERT DATA {{ {} }}", feature(i).replace(" .\n", " . ")),
    );
}

/// How long the switch holds the writer lock with and without a spatial index, and with
/// the index's base built under the lock (as before it was built with the generation)
/// or beforehand. `SPARKLES_BENCH_GEOMS` sets the number of geometries (100,000). Run
/// it in release mode: `cargo test --release -p sparkles-core --features geo --lib --
/// --ignored --nocapture compaction_lock_with_a_spatial_index`.
#[cfg(feature = "geo")]
#[test]
#[ignore]
fn compaction_lock_with_a_spatial_index() {
    let n: usize = std::env::var("SPARKLES_BENCH_GEOMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    let mut data = String::new();
    for i in 0..n {
        data.push_str(&feature(i));
    }
    println!("{n} geometries, {} quads", n * 3);
    println!(
        "{:<28} {:>9} {:>9} {:>9} {:>8}",
        "case", "lock ms", "build ms", "total ms", "carried"
    );
    for (label, with_geo, at_switch) in [
        ("no spatial index", false, false),
        ("spatial base at the switch", true, true),
        ("spatial base with the build", true, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
        s.load(&[Source::from_bytes(
            data.clone().into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        s.compact().unwrap();
        if with_geo {
            let st = s.enable_geo(crate::geo::GeoConfig::default()).unwrap();
            assert_eq!(st.state, "ready", "{st:?}");
        }
        let mut next = n;
        let mut locks = Vec::new();
        for round in 0..3 {
            for _ in 0..500 {
                insert_feature(&s, next);
                next += 1;
            }
            let start = next;
            next += 100;
            s.set_failpoint(
                "compact-built",
                Some(Arc::new(move |st: &Store| {
                    for i in start..start + 100 {
                        insert_feature(st, i);
                    }
                })),
            );
            let r = s
                .compact_with(&CompactOptions {
                    geo_at_switch: at_switch,
                    ..Default::default()
                })
                .unwrap();
            s.set_failpoint("compact-built", None);
            assert!(r.abandoned.is_none());
            if with_geo {
                assert_eq!(s.geo_status().unwrap().state, "ready");
            }
            println!(
                "{:<28} {:>9.1} {:>9.0} {:>9.0} {:>8}   (round {round})",
                label, r.lock_ms, r.build_ms, r.total_ms, r.caught_up_commits
            );
            locks.push(r.lock_ms);
        }
        locks.sort_by(f64::total_cmp);
        println!("{label:<28} median lock {:.1} ms", locks[1]);
    }
}
