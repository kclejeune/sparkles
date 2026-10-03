//! Diffs between commits: exact against the materialized states, for random commit
//! sequences with deletes and re-inserts, across compactions, bulk commits, collected
//! generations and graphs, in persistent and in-memory stores; budgets; history for
//! in-memory stores; pin expiry and schedules.

use oxrdf::{GraphName, NamedNode};
use sparkles_core::Error;
use sparkles_core::annotations::nquads_line;
use sparkles_core::history::{At, HistoryOptions, Retention, Schedule};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{ChangesOptions, DiffMethod, DiffOp, DiffOptions, Store, StoreOptions};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// One quad of a small universe: few subjects, predicates and graphs, objects that are
/// IRIs, inline integers, strings (vocabulary terms) and language-tagged strings.
fn quad(r: &mut Rng) -> String {
    let s = format!("<urn:s{}>", r.below(5));
    let p = format!("<urn:p{}>", r.below(2));
    let o = match r.below(4) {
        0 => format!("<urn:o{}>", r.below(4)),
        1 => format!("{}", r.below(4)),
        2 => format!("\"v{}\"", r.below(4)),
        _ => format!("\"w{}\"@en", r.below(3)),
    };
    match r.below(3) {
        0 => format!("{s} {p} {o} ."),
        g => format!("GRAPH <urn:g{g}> {{ {s} {p} {o} }}"),
    }
}

/// The quads of a state, as canonical N-Quads lines.
fn state(s: &Store, seq: u64) -> BTreeSet<String> {
    let (snap, _) = s
        .snapshot_at(&At::Commit(seq), &HistoryOptions::default())
        .unwrap();
    let mut out = BTreeSet::new();
    snap.for_each_quad(|q| {
        out.insert(nquads_line(&snap.quad_to_terms(q).unwrap()));
        Ok(())
    })
    .unwrap();
    out
}

fn in_graph(line: &str, g: &Option<GraphName>) -> bool {
    match g {
        None => true,
        Some(GraphName::DefaultGraph) => !line.contains("<urn:g"),
        Some(GraphName::NamedNode(n)) => line.ends_with(&format!("{n} .")),
        Some(GraphName::BlankNode(_)) => false,
    }
}

/// Check `diff(a, b)` against the two materialized states; returns its method.
fn check(s: &Store, a: u64, b: u64, graph: Option<GraphName>) -> DiffMethod {
    let o = DiffOptions {
        graph: graph.clone(),
        ..Default::default()
    };
    let d = s.diff(&At::Commit(a), &At::Commit(b), &o).unwrap();
    let (sa, sb) = (state(s, a), state(s, b));
    let want_add: BTreeSet<String> = sb
        .difference(&sa)
        .filter(|l| in_graph(l, &graph))
        .cloned()
        .collect();
    let want_rem: BTreeSet<String> = sa
        .difference(&sb)
        .filter(|l| in_graph(l, &graph))
        .cloned()
        .collect();
    let mut add = BTreeSet::new();
    let mut rem = BTreeSet::new();
    let mut seen_add = false;
    for (op, q) in d.iter() {
        let line = nquads_line(&q);
        match op {
            DiffOp::Add => {
                seen_add = true;
                assert!(add.insert(line), "a quad added twice");
            }
            DiffOp::Remove => {
                assert!(!seen_add, "removals come before additions");
                assert!(rem.insert(line), "a quad removed twice");
            }
        }
    }
    assert_eq!(add, want_add, "added, diff({a}, {b}) in {graph:?}");
    assert_eq!(rem, want_rem, "removed, diff({a}, {b}) in {graph:?}");
    assert_eq!(d.added as usize, add.len());
    assert_eq!(d.removed as usize, rem.len());
    assert_eq!((d.from.commit.seq, d.to.commit.seq), (a, b));
    d.method
}

fn graphs() -> [Option<GraphName>; 3] {
    [
        None,
        Some(GraphName::DefaultGraph),
        Some(GraphName::NamedNode(NamedNode::new_unchecked("urn:g1"))),
    ]
}

/// What a random step did.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Did {
    Update,
    Bulk,
    Compact,
}

/// Run `n` random steps: updates (inserts, deletes, re-inserts), compactions and bulk
/// loads. Returns what each commit was.
fn random_history(s: &Store, r: &mut Rng, n: usize, bulk_threshold: u64) -> Vec<(u64, Did)> {
    let mut log = Vec::new();
    let mut live: Vec<String> = Vec::new();
    for _ in 0..n {
        match r.below(10) {
            0 => {
                s.compact().unwrap();
                log.push((s.head_commit().seq, Did::Compact));
            }
            1 => {
                // a bulk load (with some quads already present)
                // over `bulk_threshold` by the size estimate (80 bytes a quad)
                let nt: String = (0..bulk_threshold * 3)
                    .map(|i| format!("<urn:b{}> <urn:p0> \"{i}\" <urn:g1> .\n", r.below(30)))
                    .collect();
                s.load(&[Source::from_bytes(nt.into_bytes(), RdfFormat::NQuads, None)])
                    .unwrap();
                let c = s.head_commit();
                log.push((c.seq, if c.bulk { Did::Bulk } else { Did::Update }));
            }
            _ => {
                let mut ins = Vec::new();
                let mut del = Vec::new();
                for _ in 0..1 + r.below(4) {
                    if !live.is_empty() && r.below(3) == 0 {
                        let i = r.below(live.len() as u64) as usize;
                        del.push(live.swap_remove(i));
                    } else {
                        let q = quad(r);
                        live.push(q.clone());
                        ins.push(q);
                    }
                }
                let u = format!(
                    "DELETE DATA {{ {} }} ; INSERT DATA {{ {} }}",
                    del.join(" "),
                    ins.join(" ")
                );
                let before = s.head_commit().seq;
                update(s, &u, &QueryOptions::default()).unwrap();
                if s.head_commit().seq != before {
                    log.push((s.head_commit().seq, Did::Update));
                }
            }
        }
    }
    log
}

fn all_pairs(s: &Store, r: &mut Rng, head: u64, samples: usize) {
    for _ in 0..samples {
        let a = r.below(head + 1);
        let b = r.below(head + 1);
        for g in graphs() {
            check(s, a, b, g);
        }
    }
}

fn keep_all() -> Retention {
    Retention {
        keep_commits: Some(100_000),
        ..Default::default()
    }
}

#[test]
fn diffs_within_one_generation_read_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    let mut r = Rng(7);
    for _ in 0..40 {
        let u = format!("INSERT DATA {{ {} }}", quad(&mut r));
        update(&s, &u, &QueryOptions::default()).unwrap();
        let u = format!("DELETE DATA {{ {} }}", quad(&mut r));
        update(&s, &u, &QueryOptions::default()).unwrap();
    }
    let head = s.head_commit().seq;
    for (a, b) in [(0, head), (head, 0), (3, 3), (1, 2), (10, head - 1)] {
        for g in graphs() {
            let m = check(&s, a, b, g);
            assert_eq!(
                m,
                if a == b {
                    DiffMethod::Same
                } else {
                    DiffMethod::Log
                }
            );
        }
    }
    all_pairs(&s, &mut r, head, 30);
}

#[test]
fn a_quad_deleted_and_reinserted_is_no_change() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    let u = |q: &str| update(&s, q, &QueryOptions::default()).unwrap();
    u("INSERT DATA { <urn:a> <urn:p> 1 . <urn:b> <urn:p> 2 }");
    u("DELETE DATA { <urn:a> <urn:p> 1 }");
    u("INSERT DATA { <urn:a> <urn:p> 1 }");
    u("DELETE DATA { <urn:b> <urn:p> 2 }");
    let d = s
        .diff(&At::Commit(1), &At::Head, &Default::default())
        .unwrap();
    let lines: Vec<String> = d
        .iter()
        .map(|(op, q)| format!("{} {}", op.sign(), nquads_line(&q)))
        .collect();
    assert_eq!(
        lines,
        ["- <urn:b> <urn:p> \"2\"^^<http://www.w3.org/2001/XMLSchema#integer> ."]
    );
    assert_eq!(d.log_changes, 3);
    // the reverse direction swaps the signs
    let d = s
        .diff(&At::Head, &At::Commit(1), &Default::default())
        .unwrap();
    assert_eq!((d.added, d.removed), (1, 0));
}

#[test]
fn random_histories_across_compactions_and_bulk_commits() {
    for seed in [1, 2, 3] {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(
            &dir.path().join("db"),
            StoreOptions {
                bulk_threshold: 6,
                history_max_generations: 64,
                // the write-ahead logs and state comparisons alone (the change log is
                // checked in history_log.rs)
                change_log: false,
                ..Default::default()
            },
        )
        .unwrap();
        s.set_retention(keep_all()).unwrap();
        let mut r = Rng(seed * 7919);
        let log = random_history(&s, &mut r, 60, 6);
        let head = s.head_commit().seq;
        assert_eq!(s.history().reconstructable, [(0, head)]);
        all_pairs(&s, &mut r, head, 40);
        // a range with no bulk commit is read from the logs, compactions included
        let bulks: Vec<u64> = log
            .iter()
            .filter(|l| l.1 == Did::Bulk)
            .map(|l| l.0)
            .collect();
        let (a, b) = (0..head)
            .flat_map(|a| (a + 1..=head).map(move |b| (a, b)))
            .filter(|&(a, b)| !bulks.iter().any(|&k| a < k && k <= b))
            .max_by_key(|&(a, b)| b - a)
            .unwrap();
        assert_eq!(check(&s, a, b, None), DiffMethod::Log, "{a}..{b}");
        let k = bulks.first().expect("a bulk commit");
        assert_eq!(check(&s, k - 1, *k, None), DiffMethod::Compare);
    }
}

/// Read the change feed from `after` to the head in pages; checks each commit's changes
/// against the diff of that one commit. Returns the commits read and how many were
/// listed without their changes.
fn follow(s: &Store, after: u64, o: &ChangesOptions) -> (Vec<u64>, usize) {
    let (mut seen, mut partial, mut at) = (Vec::new(), 0, after);
    loop {
        let page = s.changes(at, o).unwrap();
        assert!(page.commits.len() <= o.max_commits.max(1));
        if page.commits.is_empty() {
            assert_eq!(at, s.head_commit().seq);
            return (seen, partial);
        }
        for c in &page.commits {
            let seq = c.commit.seq;
            assert_eq!(seq, at + 1, "commits come in order");
            let d = s
                .diff(&At::Commit(seq - 1), &At::Commit(seq), &Default::default())
                .unwrap();
            assert_eq!(
                (c.added, c.removed),
                (d.added, d.removed),
                "counts of {seq}"
            );
            if c.complete() {
                let got: Vec<String> = c
                    .iter()
                    .map(|(op, q)| format!("{} {}", op.sign(), nquads_line(&q)))
                    .collect();
                let want: Vec<String> = d
                    .iter()
                    .map(|(op, q)| format!("{} {}", op.sign(), nquads_line(&q)))
                    .collect();
                assert_eq!(got, want, "changes of {seq}");
            } else {
                partial += 1;
                assert!(o.max_quads > 0 && c.added + c.removed > o.max_quads);
            }
            seen.push(seq);
            at = seq;
        }
        assert_eq!(page.next(), at);
    }
}

#[test]
fn the_change_feed_lists_each_commit_once() {
    for seed in [3, 4] {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(
            &dir.path().join("db"),
            StoreOptions {
                bulk_threshold: 6,
                history_max_generations: 64,
                ..Default::default()
            },
        )
        .unwrap();
        s.set_retention(keep_all()).unwrap();
        let mut r = Rng(seed * 104_729);
        let log = random_history(&s, &mut r, 60, 6);
        assert!(log.iter().any(|l| l.1 == Did::Bulk));
        let head = s.head_commit().seq;
        for (after, max_commits, max_quads) in [(0, 100, 0), (0, 3, 0), (5, 7, 4), (head, 10, 0)] {
            let o = ChangesOptions {
                max_commits,
                max_quads,
                ..Default::default()
            };
            let (seen, partial) = follow(&s, after, &o);
            assert_eq!(seen, (after + 1..=head).collect::<Vec<_>>());
            if max_quads == 0 {
                assert_eq!(partial, 0);
            }
        }
        // past the head
        assert!(matches!(
            s.changes(head + 1, &Default::default()),
            Err(Error::NotFound(_))
        ));
    }
}

#[test]
fn the_change_feed_stops_at_gaps() {
    let dir = tempfile::tempdir().unwrap();
    // without the change log, which would fill the gap (history_log.rs)
    let s = Store::open(
        &dir.path().join("db"),
        StoreOptions {
            change_log: false,
            ..Default::default()
        },
    )
    .unwrap();
    let u = |q: &str| update(&s, q, &QueryOptions::default()).unwrap();
    u("INSERT DATA { <urn:a> <urn:p> 1 }");
    u("INSERT DATA { <urn:b> <urn:p> 2 }");
    u("INSERT DATA { <urn:c> <urn:p> 3 }");
    s.create_snapshot("v", &At::Commit(1), None).unwrap();
    s.compact().unwrap(); // gen 2 from commit 3
    u("DELETE DATA { <urn:a> <urn:p> 1 }");
    u("INSERT DATA { <urn:a> <urn:p> 11 }");
    s.compact().unwrap(); // gen 3 from commit 5; gen 2 is collected
    u("INSERT DATA { <urn:d> <urn:p> 4 }");
    assert_eq!(s.history().reconstructable, [(0, 3), (5, 6)]);
    // up to the gap, then the commit that cannot be read
    let page = s.changes(1, &Default::default()).unwrap();
    assert_eq!(page.next(), 3);
    let e = s.changes(3, &Default::default()).err().unwrap();
    assert!(matches!(&e, Error::HistoryGone(g) if g.seq == 4), "{e}");
    let e = s.changes(4, &Default::default()).err().unwrap();
    assert!(matches!(&e, Error::HistoryGone(g) if g.seq == 4), "{e}");
    // and after it
    let (seen, _) = follow(&s, 5, &Default::default());
    assert_eq!(seen, [6]);
    // waiting for commits: the watch sees the next one
    let mut rx = s.subscribe_commits();
    u("INSERT DATA { <urn:e> <urn:p> 5 }");
    assert!(rx.has_changed().unwrap());
    assert_eq!(*rx.borrow_and_update(), 7);
}

#[test]
fn gaps_in_history_are_compared() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    // without the change log, which would read the gap's changes (history_log.rs)
    let s = Store::open(
        &root,
        StoreOptions {
            change_log: false,
            ..Default::default()
        },
    )
    .unwrap();
    let u = |q: &str| update(&s, q, &QueryOptions::default()).unwrap();
    u("INSERT DATA { <urn:a> <urn:p> 1 }");
    u("INSERT DATA { <urn:b> <urn:p> 2 }");
    u("INSERT DATA { <urn:c> <urn:p> 3 }");
    s.create_snapshot("v", &At::Commit(2), None).unwrap();
    s.compact().unwrap(); // gen 2 from commit 3
    u("DELETE DATA { <urn:a> <urn:p> 1 }");
    u("INSERT DATA { GRAPH <urn:g1> { <urn:d> <urn:p> 4 } }");
    s.compact().unwrap(); // gen 3 from commit 5; gen 2 holds nothing pinned
    u("INSERT DATA { <urn:a> <urn:p> 1 }");
    u("DELETE DATA { <urn:b> <urn:p> 2 }");
    assert_eq!(s.history().reconstructable, [(0, 3), (5, 7)]);
    for (a, b) in [(1, 7), (2, 6), (7, 0), (3, 5), (0, 5)] {
        for g in graphs() {
            check(&s, a, b, g);
        }
    }
    assert_eq!(check(&s, 1, 7, None), DiffMethod::Compare);
    assert_eq!(check(&s, 5, 7, None), DiffMethod::Log);
    // an end in the gap is gone
    let e = s
        .diff(&At::Commit(4), &At::Head, &Default::default())
        .err()
        .unwrap();
    assert!(matches!(e, Error::HistoryGone(_)), "{e}");
    // a pin as an end
    let d = s
        .diff(&At::Snapshot("v".into()), &At::Head, &Default::default())
        .unwrap();
    assert_eq!(d.from.commit.seq, 2);
}

#[test]
fn budgets_and_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    let data: String = (0..50)
        .map(|i| format!("<urn:s{i}> <urn:p> {i} . "))
        .collect();
    update(
        &s,
        &format!("INSERT DATA {{ {data} }}"),
        &Default::default(),
    )
    .unwrap();
    let o = DiffOptions {
        max_quads: 10,
        ..Default::default()
    };
    match s.diff(&At::Commit(0), &At::Head, &o) {
        Err(Error::BudgetExceeded(b)) => {
            assert_eq!(b.kind, sparkles_core::BudgetKind::Rows);
            assert_eq!(b.limit, 10);
        }
        r => panic!("{:?}", r.map(|d| d.len())),
    }
    let o = DiffOptions {
        max_quads: 50,
        ..Default::default()
    };
    assert_eq!(s.diff(&At::Commit(0), &At::Head, &o).unwrap().added, 50);
    let o = DiffOptions {
        cancel: Some(Arc::new(true.into())),
        ..Default::default()
    };
    // a cancelled diff stops at its next check; a small one may finish first
    let _ = s.diff(&At::Commit(0), &At::Head, &o);
    // selector errors
    assert!(matches!(
        s.diff(&At::Commit(99), &At::Head, &Default::default()),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn in_memory_history_and_diffs() {
    let s = Store::in_memory(StoreOptions {
        bulk_threshold: 6,
        ..Default::default()
    });
    let u = |q: &str| update(&s, q, &QueryOptions::default()).unwrap();
    u("INSERT DATA { <urn:a> <urn:p> 1 }");
    // without retention or pins only the head is readable
    let e = s
        .snapshot_at(&At::Commit(0), &Default::default())
        .err()
        .unwrap();
    assert!(matches!(e, Error::HistoryGone(_)), "{e}");
    s.create_snapshot("one", &At::Head, None).unwrap();
    u("INSERT DATA { <urn:b> <urn:p> 2 }");
    let (snap, r) = s
        .snapshot_at(&At::Snapshot("one".into()), &Default::default())
        .unwrap();
    assert!(r.historical && snap.historical && snap.commit == 1);
    assert_eq!(snap.len(), 1);
    assert_eq!(s.history().reconstructable, [(1, 2)]);
    // the window keeps every state from now on
    s.set_retention(keep_all()).unwrap();
    let mut r = Rng(11);
    random_history(&s, &mut r, 50, 6);
    let head = s.head_commit().seq;
    assert_eq!(s.history().reconstructable, [(1, head)]);
    for _ in 0..40 {
        let a = 1 + r.below(head);
        let b = 1 + r.below(head);
        for g in graphs() {
            check(&s, a, b, g);
        }
    }
    // a shorter window drops the old states, pins stay
    s.set_retention(Retention {
        keep_commits: Some(3),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(s.history().reconstructable, [(1, 1), (head - 2, head)]);
    assert!(s.delete_snapshot("one").unwrap());
    assert_eq!(s.history().reconstructable, [(head - 2, head)]);
}

const T0: i64 = 4_000_000_000_000;

fn clocked(root: &Path) -> (Store, Arc<AtomicI64>) {
    let s = Store::open(root, StoreOptions::default()).unwrap();
    let now = Arc::new(AtomicI64::new(T0));
    let n = now.clone();
    s.set_clock(Arc::new(move || n.load(Ordering::SeqCst)));
    (s, now)
}

#[test]
fn pins_expire_at_the_tick() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let (s, now) = clocked(&root);
    update(&s, "INSERT DATA { <urn:a> <urn:p> 1 }", &Default::default()).unwrap();
    s.create_snapshot_with("short", &At::Head, None, Some(T0 + 1000))
        .unwrap();
    s.create_snapshot("long", &At::Head, None).unwrap();
    assert!(s.history_tick().unwrap().expired.is_empty());
    now.store(T0 + 1000, Ordering::SeqCst);
    assert_eq!(s.history_tick().unwrap().expired, ["short"]);
    let names: Vec<String> = s.snapshots().into_iter().map(|p| p.name).collect();
    assert_eq!(names, ["long"]);
    drop(s);
    // durable
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.snapshots().len(), 1);
}

#[test]
fn scheduled_pins_rotate() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let (s, now) = clocked(&root);
    s.set_schedules(vec![Schedule {
        prefix: "hourly-".into(),
        every_ms: 3_600_000,
        keep_last: 2,
    }])
    .unwrap();
    let mut made = Vec::new();
    for h in 0..4 {
        now.store(T0 + h * 3_600_000, Ordering::SeqCst);
        let q = format!("INSERT DATA {{ <urn:a> <urn:p> {h} }}");
        update(&s, &q, &Default::default()).unwrap();
        let t = s.history_tick().unwrap();
        assert_eq!(t.created.len(), 1, "{t:?}");
        made.extend(t.created);
        // not due again within the hour, nor without a change
        assert!(s.history_tick().unwrap().created.is_empty());
    }
    let names: Vec<String> = s.snapshots().into_iter().map(|p| p.name).collect();
    assert_eq!(names, made[2..]);
    // a quiet hour pins nothing
    now.store(T0 + 5 * 3_600_000, Ordering::SeqCst);
    assert!(s.history_tick().unwrap().created.is_empty());
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.schedules().len(), 1);
    // bad schedules
    for bad in [
        Schedule {
            prefix: "a/b".into(),
            every_ms: 3_600_000,
            keep_last: 1,
        },
        Schedule {
            prefix: "x".into(),
            every_ms: 1000,
            keep_last: 1,
        },
        Schedule {
            prefix: "x".into(),
            every_ms: 3_600_000,
            keep_last: 0,
        },
    ] {
        assert!(s.set_schedules(vec![bad]).is_err());
    }
}

#[test]
fn max_bytes_trims_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.set_retention(keep_all()).unwrap();
    for i in 0..3 {
        let q = format!("INSERT DATA {{ <urn:a{i}> <urn:p> {i} }}");
        update(&s, &q, &Default::default()).unwrap();
        s.compact().unwrap();
    }
    let h = s.history();
    let kept = h.generations.iter().filter(|g| !g.current).count();
    assert_eq!(kept, 3);
    let newest = h.generations[2].bytes;
    // room for one generation: the older ones go
    let st = s
        .set_retention(Retention {
            max_bytes: Some(newest + 1),
            ..keep_all()
        })
        .unwrap();
    let kept: Vec<&str> = st
        .generations
        .iter()
        .filter(|g| !g.current)
        .map(|g| g.name.as_str())
        .collect();
    assert_eq!(kept, ["gen-0003"]);
    assert!(st.bytes <= newest + 1);
}

/// Timings of the two diff paths: `cargo test --release -p sparkles-core --test diff --
/// --ignored --nocapture`. `DIFF_BASE` quads (default 1,000,000) in a bulk-built base,
/// then 20,000 commits of 5 inserts each, then a compaction and a bulk commit.
#[test]
#[ignore = "a benchmark"]
fn diff_performance() {
    use oxrdf::{Literal, Term};
    use sparkles_core::id::Id;
    use std::time::Instant;
    let base: usize = std::env::var("DIFF_BASE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(
        &dir.path().join("db"),
        StoreOptions {
            bulk_threshold: 10_000,
            ..Default::default()
        },
    )
    .unwrap();
    s.set_retention(keep_all()).unwrap();
    let nt: String = (0..base)
        .map(|i| format!("<urn:s{}> <urn:p{}> \"v{i}\" .\n", i / 10, i % 7))
        .collect();
    let t = Instant::now();
    s.load(&[Source::from_bytes(
        nt.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    eprintln!("load {base} quads: {:.2?}", t.elapsed());
    let first = s.head_commit().seq;
    let t = Instant::now();
    let mut r = Rng(5);
    for c in 0..20_000u64 {
        let mut w = s.write();
        for k in 0..5u64 {
            let subj = w
                .intern(&Term::NamedNode(NamedNode::new_unchecked(format!(
                    "urn:s{}",
                    r.below(base as u64 / 10)
                ))))
                .unwrap();
            let pred = w
                .intern(&Term::NamedNode(NamedNode::new_unchecked("urn:p0")))
                .unwrap();
            let obj = w
                .intern(&Term::Literal(Literal::new_simple_literal(format!(
                    "n{c}-{k}"
                ))))
                .unwrap();
            w.insert([subj, pred, obj, Id::DEFAULT_GRAPH]).unwrap();
        }
        w.commit().unwrap();
    }
    eprintln!("20,000 commits of 5 inserts: {:.2?}", t.elapsed());
    let head = s.head_commit().seq;
    let time = |what: &str, a: u64, b: u64| {
        let t = Instant::now();
        let d = s
            .diff(&At::Commit(a), &At::Commit(b), &Default::default())
            .unwrap();
        eprintln!(
            "{what}: commits {a}..{b}: +{} -{} by {} in {:.2?} ({} log changes, {} quads compared)",
            d.added,
            d.removed,
            d.method.as_str(),
            t.elapsed(),
            d.log_changes,
            d.compared
        );
    };
    time("log, every commit", first, head);
    time("log, the last 100 commits", head - 100, head);
    time("log, one commit", head - 1, head);
    s.compact().unwrap();
    for i in 0..3 {
        let q = format!("INSERT DATA {{ <urn:after{i}> <urn:p> {i} }}");
        update(&s, &q, &QueryOptions::default()).unwrap();
    }
    time("log across a compaction", head - 1_000, s.head_commit().seq);
    let before = s.head_commit().seq;
    // over the bulk threshold by the size estimate (80 bytes a quad)
    let nt: String = (0..50_000)
        .map(|i| format!("<urn:bulk{i}> <urn:p> \"b{i}\" .\n"))
        .collect();
    s.load(&[Source::from_bytes(
        nt.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    assert!(s.head_commit().bulk);
    time("compare across a bulk commit", before, s.head_commit().seq);
}
