//! The change log and history queries: every commit's recorded changes equal the
//! difference of the states before and after it, as a replay of the write-ahead log
//! materializes them, across compactions, bulk commits, restarts and crashes; lookups by
//! term; retention; access control; in-memory stores.

use oxrdf::{GraphName, NamedNode, Term};
use sparkles::Error;
use sparkles::annotations::nquads_line;
use sparkles::history::{At, HistoryOptions};
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::QueryOptions;
use sparkles::sparql::update::update;
use sparkles::store::{
    ChangeLogSettings, DiffOp, HistoryBound, HistoryQuery, Store, StoreOptions, UnrecordedReason,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

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

/// One quad of a small universe: IRIs, inline integers, strings, language-tagged
/// strings, a few graphs.
fn quad(r: &mut Rng) -> String {
    let s = format!("<urn:s{}>", r.below(6));
    let p = format!("<urn:p{}>", r.below(3));
    let o = match r.below(4) {
        0 => format!("<urn:o{}>", r.below(4)),
        1 => format!("{}", r.below(5)),
        2 => format!("\"v{}\"", r.below(4)),
        _ => format!("\"w{}\"@en", r.below(3)),
    };
    match r.below(3) {
        0 => format!("{s} {p} {o} ."),
        g => format!("GRAPH <urn:g{g}> {{ {s} {p} {o} }}"),
    }
}

fn upd(s: &Store, u: &str) {
    update(s, u, &QueryOptions::default()).unwrap_or_else(|e| panic!("{u}: {e}"));
}

/// The quads of the live state, as canonical N-Quads lines.
fn live(s: &Store) -> BTreeSet<String> {
    lines(&s.snapshot())
}

fn lines(snap: &sparkles::store::Snapshot) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    snap.for_each_quad(|q| {
        out.insert(nquads_line(&snap.quad_to_terms(q).unwrap()));
        Ok(())
    })
    .unwrap();
    out
}

/// The net changes between two states: (removed, added).
fn delta(a: &BTreeSet<String>, b: &BTreeSet<String>) -> (BTreeSet<String>, BTreeSet<String>) {
    (
        a.difference(b).cloned().collect(),
        b.difference(a).cloned().collect(),
    )
}

/// The recorded changes of commit `seq`: (removed, added).
fn recorded(s: &Store, seq: u64) -> (BTreeSet<String>, BTreeSet<String>) {
    let r = s
        .history_changes(&HistoryQuery {
            from: Some(HistoryBound::Commit(seq)),
            to: Some(HistoryBound::Commit(seq)),
            ..Default::default()
        })
        .unwrap();
    assert!(r.unrecorded.is_empty(), "commit {seq}: {:?}", r.unrecorded);
    let (mut rem, mut add) = (BTreeSet::new(), BTreeSet::new());
    for c in &r.changes {
        assert_eq!(c.commit.seq, seq);
        let line = nquads_line(&c.quad);
        match c.op {
            DiffOp::Add => assert!(add.insert(line)),
            DiffOp::Remove => assert!(rem.insert(line)),
        }
    }
    (rem, add)
}

/// Run a random history with compactions, a bulk commit, transactions that change a
/// quad back, deletes of base quads and blank nodes; returns the state after every
/// commit.
fn churn(s: &Store, seed: u64, commits: usize) -> BTreeMap<u64, BTreeSet<String>> {
    let mut r = Rng(seed);
    let mut states = BTreeMap::new();
    states.insert(s.head_commit().seq, live(s));
    for i in 0..commits {
        match r.below(12) {
            0 if i > 5 => {
                s.compact().unwrap();
                continue;
            }
            1 => {
                // a quad inserted and deleted in one transaction cancels out
                let q = quad(&mut r);
                upd(
                    s,
                    &format!(
                        "INSERT DATA {{ {q} }} ; DELETE DATA {{ {q} }} ; INSERT DATA {{ <urn:t{i}> <urn:p0> {i} }}"
                    ),
                );
            }
            2 => upd(
                s,
                &format!("INSERT DATA {{ _:b <urn:p1> {i} . _:b <urn:p2> \"b{i}\" }}"),
            ),
            3 | 4 => {
                // delete what is there, base quads included
                let cur: Vec<String> = live(s).into_iter().collect();
                if cur.is_empty() {
                    continue;
                }
                let mut del = String::new();
                for _ in 0..1 + r.below(3) {
                    let l = &cur[r.below(cur.len() as u64) as usize];
                    let l = l.trim_end_matches(" .");
                    if l.contains("_:") {
                        continue;
                    }
                    // N-Quads line to a DELETE DATA quad
                    let parts: Vec<&str> = l.rsplitn(2, ' ').collect();
                    if parts[0].starts_with("<urn:g") && l.matches('<').count() >= 3 {
                        del.push_str(&format!("GRAPH {} {{ {} }} ", parts[0], parts[1]));
                    } else {
                        del.push_str(&format!("{l} . "));
                    }
                }
                if del.is_empty() {
                    continue;
                }
                upd(s, &format!("DELETE DATA {{ {del} }}"));
            }
            _ => {
                let mut ins = String::new();
                for _ in 0..1 + r.below(4) {
                    ins.push_str(&quad(&mut r));
                    ins.push(' ');
                }
                upd(s, &format!("INSERT DATA {{ {ins} }}"));
            }
        }
        let head = s.head_commit().seq;
        states.insert(head, live(s));
    }
    states
}

/// Every commit's recorded changes equal the difference of its states.
fn check_all(s: &Store, states: &BTreeMap<u64, BTreeSet<String>>, skip: &BTreeSet<u64>) {
    let seqs: Vec<u64> = states.keys().copied().collect();
    for w in seqs.windows(2) {
        let (a, b) = (w[0], w[1]);
        assert_eq!(b, a + 1, "commits are consecutive");
        if skip.contains(&b) {
            continue;
        }
        assert_eq!(
            recorded(s, b),
            delta(&states[&a], &states[&b]),
            "commit {b}"
        );
    }
}

fn opts() -> StoreOptions {
    StoreOptions {
        // small segments: sealing, sealed indexes and several files are exercised
        change_log_segment_bytes: 4096,
        ..Default::default()
    }
}

#[test]
fn every_commit_equals_the_replayed_states() {
    for persistent in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let s = match persistent {
            true => Store::open(&dir.path().join("db"), opts()).unwrap(),
            false => Store::in_memory(opts()),
        };
        // a bulk load into the empty dataset is recorded in full
        let base: String = (0..40)
            .map(|i| format!("<urn:s{}> <urn:p{}> \"base{i}\" .\n", i % 6, i % 3))
            .collect();
        s.load(&[Source::from_bytes(
            base.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let states = churn(&s, 7, 300);
        check_all(&s, &states, &BTreeSet::new());
        // the commits of the current generation, against a replay of its log
        if persistent {
            let head = s.head_commit().seq;
            for seq in states.keys().copied().filter(|&q| q > 0) {
                let Ok((a, _)) = s.snapshot_at(&At::Commit(seq - 1), &HistoryOptions::default())
                else {
                    continue;
                };
                let (b, _) = s
                    .snapshot_at(&At::Commit(seq), &HistoryOptions::default())
                    .unwrap();
                assert_eq!(recorded(&s, seq), delta(&lines(&a), &lines(&b)), "{seq}");
                assert!(seq <= head);
            }
            // and after a restart, from the files alone
            let root = dir.path().join("db");
            drop(s);
            let s = Store::open(&root, opts()).unwrap();
            check_all(&s, &states, &BTreeSet::new());
            let st = s.change_log_status().unwrap();
            assert!(st.segments > 1, "{st:?}");
        }
    }
}

#[test]
fn lookups_by_term_find_every_matching_change() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), opts()).unwrap();
    let states = churn(&s, 11, 250);
    // brute force: every change of every commit
    let mut all: Vec<(u64, DiffOp, String)> = Vec::new();
    let seqs: Vec<u64> = states.keys().copied().collect();
    for w in seqs.windows(2) {
        let (rem, add) = delta(&states[&w[0]], &states[&w[1]]);
        all.extend(rem.into_iter().map(|l| (w[1], DiffOp::Remove, l)));
        all.extend(add.into_iter().map(|l| (w[1], DiffOp::Add, l)));
    }
    let iri = |s: &str| NamedNode::new(s).unwrap();
    for subj in ["urn:s0", "urn:s3", "urn:s5", "urn:nothing"] {
        for pred in [None, Some("urn:p1")] {
            let q = HistoryQuery {
                subjects: vec![Term::NamedNode(iri(subj))],
                predicates: pred.map(iri).into_iter().collect(),
                ..Default::default()
            };
            let r = s.history_changes(&q).unwrap();
            let got: BTreeSet<(u64, DiffOp, String)> = r
                .changes
                .iter()
                .map(|c| (c.commit.seq, c.op, nquads_line(&c.quad)))
                .collect();
            let want: BTreeSet<(u64, DiffOp, String)> = all
                .iter()
                .filter(|(_, _, l)| l.starts_with(&format!("<{subj}> ")))
                .filter(|(_, _, l)| pred.is_none_or(|p| l.contains(&format!(" <{p}> "))))
                .cloned()
                .collect();
            assert_eq!(got, want, "{subj} {pred:?}");
            // in commit order
            assert!(
                r.changes
                    .windows(2)
                    .all(|w| w[0].commit.seq <= w[1].commit.seq)
            );
        }
    }
    // an object (an inline integer), a graph, removals only, a range, newest first
    let q = HistoryQuery {
        objects: vec![Term::Literal(oxrdf::Literal::from(3i64))],
        graphs: vec![GraphName::NamedNode(iri("urn:g1"))],
        op: Some(DiffOp::Remove),
        from: Some(HistoryBound::Commit(50)),
        to: Some(HistoryBound::Commit(200)),
        descending: true,
        ..Default::default()
    };
    let r = s.history_changes(&q).unwrap();
    let want: Vec<(u64, String)> = all
        .iter()
        .filter(|(c, op, l)| {
            (50..=200).contains(c)
                && *op == DiffOp::Remove
                && l.ends_with(" <urn:g1> .")
                && l.contains(" \"3\"^^<http://www.w3.org/2001/XMLSchema#integer> ")
        })
        .map(|(c, _, l)| (*c, l.clone()))
        .collect();
    let mut got: Vec<(u64, String)> = r
        .changes
        .iter()
        .map(|c| (c.commit.seq, nquads_line(&c.quad)))
        .collect();
    assert!(got.windows(2).all(|w| w[0].0 >= w[1].0), "newest first");
    got.sort();
    assert_eq!(got, want);
    // the last commit that changed a subject: newest first, limit 1
    let q = HistoryQuery {
        subjects: vec![Term::NamedNode(iri("urn:s2"))],
        descending: true,
        limit: 1,
        ..Default::default()
    };
    let r = s.history_changes(&q).unwrap();
    let last = all
        .iter()
        .filter(|(_, _, l)| l.starts_with("<urn:s2> "))
        .map(|c| c.0)
        .max();
    assert_eq!(r.changes.first().map(|c| c.commit.seq), last);
    assert_eq!(
        r.truncated,
        all.iter()
            .filter(|(_, _, l)| l.starts_with("<urn:s2> "))
            .count()
            > 1
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &t);
        } else if e.file_name() != "sparkles.lock" {
            std::fs::copy(e.path(), &t).unwrap();
        }
    }
}

/// The open segment of a log directory.
fn open_segment(root: &Path) -> std::path::PathBuf {
    let mut v: Vec<_> = std::fs::read_dir(root.join("changes"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "log"))
        .collect();
    v.sort();
    v.pop().unwrap()
}

#[test]
fn a_crash_loses_no_recorded_change() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, opts()).unwrap();
    let mut states = churn(&s, 3, 60);
    let log = s.change_log().unwrap().clone();
    // the background writer stops: later commits stay queued, as after a crash before
    // it ran
    log.set_background(false);
    for i in 0..40 {
        upd(
            &s,
            &format!("INSERT DATA {{ <urn:s{}> <urn:i{i}> {i} . }}", i % 6),
        );
        states.insert(s.head_commit().seq, live(&s));
    }
    assert!(log.status().pending > 0);
    // a crash: the files as they are now
    let crash = dir.path().join("crash");
    copy_dir(&root, &crash);
    {
        let c = Store::open(&crash, opts()).unwrap();
        check_all(&c, &states, &BTreeSet::new());
    }
    // a torn tail: the last record cut in half, and garbage after it
    let crash2 = dir.path().join("crash2");
    copy_dir(&root, &crash2);
    let seg = open_segment(&crash2);
    // the records end where the zeros written ahead of them begin
    let bytes = std::fs::read(&seg).unwrap();
    let end = bytes.iter().rposition(|&b| b != 0).unwrap() + 1;
    let f = std::fs::OpenOptions::new().write(true).open(&seg).unwrap();
    f.set_len(end as u64 - 7).unwrap();
    drop(f);
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&seg).unwrap();
        f.write_all(&[0x55; 40]).unwrap();
    }
    {
        let c = Store::open(&crash2, opts()).unwrap();
        check_all(&c, &states, &BTreeSet::new());
    }
    // a sealed segment whose index was never written is indexed again
    let crash3 = dir.path().join("crash3");
    copy_dir(&root, &crash3);
    let idx: Vec<_> = std::fs::read_dir(crash3.join("changes"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "idx"))
        .collect();
    assert!(!idx.is_empty());
    std::fs::remove_file(&idx[0]).unwrap();
    {
        let c = Store::open(&crash3, opts()).unwrap();
        check_all(&c, &states, &BTreeSet::new());
        assert!(idx[0].exists());
    }
    // the original keeps working, and closing it writes the queue
    log.set_background(true);
    drop(s);
    let s = Store::open(&root, opts()).unwrap();
    check_all(&s, &states, &BTreeSet::new());
}

#[test]
fn history_reaches_back_past_compactions_and_collected_generations() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, opts()).unwrap();
    upd(&s, "INSERT DATA { <urn:a> <urn:name> \"Ann\" }");
    upd(
        &s,
        "DELETE DATA { <urn:a> <urn:name> \"Ann\" } ; INSERT DATA { <urn:a> <urn:name> \"Anna\" }",
    );
    s.compact().unwrap();
    upd(&s, "INSERT DATA { <urn:b> <urn:name> \"Bo\" }");
    s.compact().unwrap();
    upd(
        &s,
        "DELETE DATA { <urn:a> <urn:name> \"Anna\" } ; INSERT DATA { <urn:a> <urn:name> \"Annie\" }",
    );
    // commit 1 can no longer be read, but its changes are recorded
    assert!(
        s.snapshot_at(&At::Commit(1), &HistoryOptions::default())
            .is_err()
    );
    let r = s
        .history_changes(&HistoryQuery {
            subjects: vec![Term::NamedNode(NamedNode::new("urn:a").unwrap())],
            ..Default::default()
        })
        .unwrap();
    let got: Vec<(u64, DiffOp, String)> = r
        .changes
        .iter()
        .map(|c| match &c.quad.object {
            Term::Literal(l) => (c.commit.seq, c.op, l.value().to_string()),
            t => panic!("{t}"),
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (1, DiffOp::Add, "Ann".into()),
            (2, DiffOp::Remove, "Ann".into()),
            (2, DiffOp::Add, "Anna".into()),
            (4, DiffOp::Remove, "Anna".into()),
            (4, DiffOp::Add, "Annie".into()),
        ]
    );
    // a diff across the collected generations reads the change log
    let d = s
        .diff(&At::Commit(1), &At::Head, &Default::default())
        .unwrap();
    assert_eq!((d.added, d.removed), (2, 1));
}

#[test]
fn bulk_commits_are_recorded_or_summarized() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(
        &dir.path().join("db"),
        StoreOptions {
            bulk_threshold: 10,
            change_log_bulk_max_quads: 50,
            ..opts()
        },
    )
    .unwrap();
    let nt = |n: usize, off: usize| {
        let s: String = (off..off + n)
            .map(|i| format!("<urn:s{i}> <urn:p> {i} .\n"))
            .collect();
        Source::from_bytes(s.into_bytes(), RdfFormat::Turtle, None)
    };
    s.load(&[nt(20, 0)]).unwrap(); // commit 1: bulk into an empty dataset
    // commit 2: bulk (about 80 bytes a quad), both states hold 80 > 50 quads
    s.load(&[nt(60, 20)]).unwrap();
    upd(&s, "INSERT DATA { <urn:x> <urn:p> 1 }"); // commit 3
    let r = s.history_changes(&HistoryQuery::default()).unwrap();
    assert_eq!(r.changes.iter().filter(|c| c.commit.seq == 1).count(), 20);
    assert_eq!(r.changes.iter().filter(|c| c.commit.seq == 2).count(), 0);
    assert_eq!(r.changes.iter().filter(|c| c.commit.seq == 3).count(), 1);
    assert_eq!(r.unrecorded.len(), 1);
    assert_eq!(
        (
            r.unrecorded[0].from,
            r.unrecorded[0].to,
            r.unrecorded[0].reason
        ),
        (2, 2, UnrecordedReason::Bulk)
    );
    let commit2 = r.changes.iter().find(|c| c.commit.seq == 1).unwrap();
    assert!(commit2.commit.bulk);
}

#[test]
fn retention_drops_whole_segments_from_the_oldest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, opts()).unwrap();
    for i in 0..400 {
        upd(
            &s,
            &format!(
                "INSERT DATA {{ <urn:s{i}> <urn:p> \"a fairly long literal to fill segments {i}\" }}"
            ),
        );
    }
    let all = s.history_changes(&HistoryQuery::default()).unwrap();
    assert_eq!(all.changes.len(), 400);
    let before = s.change_log_status().unwrap();
    assert!(before.segments > 5, "{before:?}");
    // keep the last 100 commits: segments wholly older go, the one holding 300 stays
    s.set_change_log_settings(ChangeLogSettings {
        keep_commits: Some(100),
        ..Default::default()
    })
    .unwrap();
    let st = s.change_log_status().unwrap();
    assert!(st.first.unwrap() > 1 && st.first.unwrap() <= 301, "{st:?}");
    let r = s.history_changes(&HistoryQuery::default()).unwrap();
    assert_eq!(r.unrecorded[0].reason, UnrecordedReason::BeforeLog);
    assert_eq!(
        (r.unrecorded[0].from, r.unrecorded[0].to),
        (1, st.first.unwrap() - 1)
    );
    assert_eq!(r.changes.len() as u64, 400 - (st.first.unwrap() - 1));
    // a size limit
    s.set_change_log_settings(ChangeLogSettings {
        max_bytes: Some(3 * 4096),
        ..Default::default()
    })
    .unwrap();
    let st = s.change_log_status().unwrap();
    assert!(st.segments <= 4 && st.bytes <= 4 * 4096 + 4096, "{st:?}");
    // the settings survive a restart, and the files on disk match
    drop(s);
    let s = Store::open(&root, opts()).unwrap();
    assert_eq!(
        s.change_log_status().unwrap().first,
        Some(st.first.unwrap())
    );
    let files = std::fs::read_dir(root.join("changes")).unwrap().count();
    assert_eq!(
        files,
        2 * st.segments - 1,
        "a .log per segment and an .idx per sealed one"
    );
    // off removes the log; on again starts at the next commit
    s.set_change_log_settings(ChangeLogSettings {
        enabled: Some(false),
        ..Default::default()
    })
    .unwrap();
    assert!(!root.join("changes").exists());
    assert!(matches!(
        s.history_changes(&HistoryQuery::default()),
        Err(Error::HistoryUnsupported(_))
    ));
    s.set_change_log_settings(ChangeLogSettings::default())
        .unwrap();
    upd(&s, "INSERT DATA { <urn:after> <urn:p> 1 }");
    let r = s.history_changes(&HistoryQuery::default()).unwrap();
    assert_eq!(r.changes.len(), 1);
    assert_eq!(r.unrecorded[0].reason, UnrecordedReason::Gap);
    assert_eq!((r.unrecorded[0].from, r.unrecorded[0].to), (1, 400));
}

#[test]
fn retention_by_age_drops_old_segments_at_the_tick() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), opts()).unwrap();
    // commit times never go back: start after the root commit's
    let now = Arc::new(std::sync::atomic::AtomicI64::new(
        s.head_commit().timestamp_ms,
    ));
    let clock = now.clone();
    s.set_clock(Arc::new(move || {
        clock.load(std::sync::atomic::Ordering::SeqCst)
    }));
    for i in 0..300 {
        now.fetch_add(1000, std::sync::atomic::Ordering::SeqCst);
        upd(
            &s,
            &format!(
                "INSERT DATA {{ <urn:s{i}> <urn:p> \"a fairly long literal to fill segments {i}\" }}"
            ),
        );
    }
    s.set_change_log_settings(ChangeLogSettings {
        keep_age_ms: Some(50_000),
        ..Default::default()
    })
    .unwrap();
    // nothing goes before the tick, which knows the time
    assert_eq!(s.change_log_status().unwrap().first, Some(1));
    s.history_tick().unwrap();
    let first = s.change_log_status().unwrap().first.unwrap();
    // commit 250 was made 50 s before the last: every commit after it stays
    assert!(first > 1 && first <= 251, "{first}");
}

#[test]
fn history_respects_graph_views_and_triple_protections() {
    use sparkles::access::{
        Caller, GraphAccess, GraphRule, Graphs, Limits, Protection, Rule, TripleRules,
    };
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA { <urn:a> <urn:name> \"A\" . <urn:a> <urn:salary> 10 . GRAPH <urn:secret> { <urn:a> <urn:name> \"S\" } }",
    );
    upd(
        &s,
        "DELETE DATA { <urn:a> <urn:salary> 10 } ; INSERT DATA { <urn:a> <urn:salary> 11 }",
    );
    let default_only = Graphs::Only(GraphRule::new(["default"], &[]));
    let run = |a: GraphAccess| {
        s.history_changes(&HistoryQuery {
            access: Some(Arc::new(a)),
            ..Default::default()
        })
    };
    // a graph view: the secret graph's change is hidden
    let r = run(GraphAccess::graphs(default_only.clone(), Graphs::none())).unwrap();
    assert_eq!(r.changes.len(), 4);
    assert!(
        r.changes
            .iter()
            .all(|c| c.quad.graph_name.is_default_graph())
    );
    // a protection of a predicate: the salary's history is hidden too
    let protect = |classes: Option<Vec<String>>| TripleRules {
        rules: vec![Rule {
            protection: Arc::new(Protection {
                name: "pay".into(),
                predicates: Some(vec!["urn:salary".into()]),
                classes,
                subclasses: false,
                graphs: None,
                pattern: None,
                prefixes: Default::default(),
                hide_inferences: false,
            }),
            read: Graphs::none(),
            write: Graphs::none(),
        }],
        caller: Caller::default(),
        limits: Limits::default(),
    };
    let r = run(GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        protect(None),
    ))
    .unwrap();
    let names: Vec<String> = r
        .changes
        .iter()
        .map(|c| c.quad.predicate.to_string())
        .collect();
    assert_eq!(names, vec!["<urn:name>", "<urn:name>"]);
    // a protection that depends on the data is refused
    let e = run(GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        protect(Some(vec!["urn:Person".into()])),
    ))
    .unwrap_err();
    assert!(matches!(e, Error::NotPermitted(_)), "{e}");
}

#[test]
fn authors_and_messages_are_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, opts()).unwrap();
    let mut o = QueryOptions::default();
    o.write.author = Some("user:ann".into());
    o.write.message = Some("first".into());
    update(&s, "INSERT DATA { <urn:a> <urn:p> 1 }", &o).unwrap();
    upd(&s, "INSERT DATA { <urn:a> <urn:p> 2 }");
    let check = |s: &Store| {
        let r = s.history_changes(&HistoryQuery::default()).unwrap();
        let c1 = &r.changes[0].commit;
        assert_eq!(c1.author.as_deref(), Some("user:ann"));
        assert_eq!(c1.message.as_deref(), Some("first"));
        assert_eq!(r.changes[1].commit.author, None);
    };
    check(&s);
    drop(s);
    check(&Store::open(&root, opts()).unwrap());
}

#[test]
fn time_bounds_select_commits_by_their_timestamps() {
    let s = Store::in_memory(StoreOptions::default());
    // commit times never go back: count from the root commit's
    let t0 = s.head_commit().timestamp_ms;
    let now = Arc::new(std::sync::atomic::AtomicI64::new(t0));
    let clock = now.clone();
    s.set_clock(Arc::new(move || {
        clock.load(std::sync::atomic::Ordering::SeqCst)
    }));
    for i in 1..=5 {
        now.store(t0 + i * 1000, std::sync::atomic::Ordering::SeqCst);
        upd(&s, &format!("INSERT DATA {{ <urn:s> <urn:p> {i} }}"));
    }
    let r = s
        .history_changes(&HistoryQuery {
            from: Some(HistoryBound::Time(t0 + 1500)),
            to: Some(HistoryBound::At(At::Time(t0 + 4000))),
            ..Default::default()
        })
        .unwrap();
    let seqs: Vec<u64> = r.changes.iter().map(|c| c.commit.seq).collect();
    assert_eq!(seqs, vec![2, 3, 4]);
    assert_eq!((r.from, r.to), (2, 4));
}

/// The solutions of a SELECT as strings, one row per line, cells separated by spaces
/// (`-` for unbound).
fn select(snap: Arc<sparkles::store::Snapshot>, q: &str, o: &QueryOptions) -> Vec<String> {
    let r = sparkles::sparql::query(snap, q, o).unwrap_or_else(|e| panic!("{q}: {e}"));
    r.rows()
        .into_iter()
        .map(|row| {
            row.iter()
                .map(|t| match t {
                    None => "-".to_string(),
                    Some(Term::Literal(l)) => l.value().to_string(),
                    Some(t) => t.to_string(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

const HIST: &str = "PREFIX hist: <urn:x-sparkles:history#> PREFIX : <urn:>";

#[test]
fn sparql_history_queries() {
    for persistent in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let s = match persistent {
            true => Store::open(&dir.path().join("db"), opts()).unwrap(),
            false => Store::in_memory(opts()),
        };
        let mut o = QueryOptions::default();
        o.write.author = Some("user:ann".into());
        o.write.message = Some("hello".into());
        update(
            &s,
            "INSERT DATA { <urn:a> <urn:name> \"Ann\" . <urn:b> <urn:name> \"Bo\" }",
            &o,
        )
        .unwrap(); // 1
        upd(
            &s,
            "DELETE DATA { <urn:a> <urn:name> \"Ann\" } ; INSERT DATA { <urn:a> <urn:name> \"Anna\" }",
        ); // 2
        if persistent {
            s.compact().unwrap();
        }
        upd(&s, "INSERT DATA { GRAPH <urn:g> { <urn:a> <urn:age> 30 } }"); // 3
        upd(
            &s,
            "DELETE DATA { GRAPH <urn:g> { <urn:a> <urn:age> 30 } } ; INSERT DATA { GRAPH <urn:g> { <urn:a> <urn:age> 31 } }",
        ); // 4
        let q = QueryOptions::default();
        let snap = s.snapshot();
        // the values of a predicate over time, with the commit metadata
        let rows = select(
            snap.clone(),
            &format!(
                "{HIST} SELECT ?c ?op ?v ?who ?msg ?kind WHERE {{ SERVICE hist:changes {{ << :a :name ?v >> hist:op ?op ; hist:commit ?c ; hist:author ?who ; hist:message ?msg ; hist:kind ?kind }} }} ORDER BY ?c ?op"
            ),
            &q,
        );
        assert_eq!(
            rows,
            vec![
                "1 add Ann user:ann hello update",
                "2 add Anna - - update",
                "2 remove Ann - - update",
            ],
            "{persistent}"
        );
        // the same with hist:subject and hist:predicate, and a graph
        let rows = select(
            snap.clone(),
            &format!(
                "{HIST} SELECT ?c ?op ?v ?g WHERE {{ SERVICE hist:changes {{ [] hist:subject :a ; hist:predicate :age ; hist:object ?v ; hist:graph ?g ; hist:op ?op ; hist:commit ?c }} }} ORDER BY ?c ?op"
            ),
            &q,
        );
        assert_eq!(
            rows,
            vec![
                "3 add 30 <urn:g>",
                "4 add 31 <urn:g>",
                "4 remove 30 <urn:g>"
            ]
        );
        // the default graph only, removals only
        let rows = select(
            snap.clone(),
            &format!(
                "{HIST} SELECT ?s ?v WHERE {{ SERVICE hist:changes {{ << ?s ?p ?v >> hist:op \"remove\" ; hist:graph hist:defaultGraph }} }}"
            ),
            &q,
        );
        assert_eq!(rows, vec!["<urn:a> Ann"]);
        // which commit last changed each subject: aggregate over the changes
        let rows = select(
            snap.clone(),
            &format!(
                "{HIST} SELECT ?s (MAX(?c) AS ?last) WHERE {{ SERVICE hist:changes {{ << ?s ?p ?o >> hist:commit ?c }} }} GROUP BY ?s ORDER BY ?s"
            ),
            &q,
        );
        assert_eq!(rows, vec!["<urn:a> 4", "<urn:b> 1"]);
        // newest first with a limit, a commit range, a join with the current state
        let rows = select(
            snap.clone(),
            &format!(
                "{HIST} SELECT ?c WHERE {{ SERVICE hist:changes {{ << :a ?p ?o >> hist:commit ?c ; hist:order hist:descending ; hist:limit 1 }} }}"
            ),
            &q,
        );
        assert_eq!(rows, vec!["4"]);
        let rows = select(
            snap.clone(),
            &format!(
                "{HIST} SELECT ?v ?c WHERE {{ ?x :name \"Anna\" . SERVICE hist:changes {{ << ?x :name ?v >> hist:op \"add\" ; hist:commit ?c ; hist:from 2 ; hist:to 3 }} }}"
            ),
            &q,
        );
        assert_eq!(rows, vec!["Anna 2"]);
        // time bounds: every commit is at or before then
        let rows = select(
            snap.clone(),
            &format!(
                "{HIST} SELECT (COUNT(*) AS ?n) WHERE {{ SERVICE hist:changes {{ << ?s ?p ?o >> hist:to \"2999-01-01T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime> }} }}"
            ),
            &q,
        );
        assert_eq!(rows, vec!["7"]);
        // the time output is an xsd:dateTime
        let rows = select(
            snap.clone(),
            &format!(
                "{HIST} SELECT ?ok WHERE {{ SERVICE hist:changes {{ << :b ?p ?o >> hist:time ?t }} BIND(datatype(?t) = <http://www.w3.org/2001/XMLSchema#dateTime> AS ?ok) }}"
            ),
            &q,
        );
        assert_eq!(rows, vec!["true"]);
        // a query of a past state sees the history up to it
        if persistent {
            let (past, _) = s
                .snapshot_at(&At::Commit(3), &HistoryOptions::default())
                .unwrap();
            let rows = select(
                past,
                &format!(
                    "{HIST} SELECT (MAX(?c) AS ?m) WHERE {{ SERVICE hist:changes {{ << ?s ?p ?o >> hist:commit ?c }} }}"
                ),
                &q,
            );
            assert_eq!(rows, vec!["3"]);
        }
        // a graph view hides the graph's history
        use sparkles::access::{GraphAccess, GraphRule, Graphs};
        let v = QueryOptions {
            graphs: Some(Arc::new(GraphAccess::graphs(
                Graphs::Only(GraphRule::new(["default"], &[])),
                Graphs::none(),
            ))),
            ..Default::default()
        };
        let rows = select(
            snap.clone(),
            &format!(
                "{HIST} SELECT (COUNT(*) AS ?n) WHERE {{ SERVICE hist:changes {{ << ?s ?p ?o >> hist:op ?op }} }}"
            ),
            &v,
        );
        assert_eq!(rows, vec!["4"]);
        // errors
        for bad in [
            "SERVICE hist:changes { << ?s ?p ?o >> hist:nope ?x }",
            "SERVICE hist:changes { << ?s ?p ?o >> hist:commit 3 }",
            "SERVICE hist:changes { << ?s ?p ?o >> hist:op \"maybe\" }",
            "SERVICE hist:changes { << ?s ?p ?o >> hist:from \"snapshot:v1\" }",
            "SERVICE hist:changes { << ?s ?p ?o >> hist:op ?a . << ?s ?p ?o >> hist:op ?b }",
        ] {
            let e = sparkles::sparql::query(
                snap.clone(),
                &format!("{HIST} SELECT * WHERE {{ {bad} }}"),
                &q,
            );
            assert!(e.is_err(), "{bad}");
        }
    }
}

#[test]
fn diffs_read_the_change_log_where_states_are_gone() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(
        &dir.path().join("db"),
        StoreOptions {
            bulk_threshold: 20,
            ..opts()
        },
    )
    .unwrap();
    let mut states = churn(&s, 5, 120);
    // a bulk commit in the middle, then more
    let body: String = (0..40)
        .map(|i| format!("<urn:bulk{i}> <urn:p0> {i} .\n"))
        .collect();
    s.load(&[Source::from_bytes(
        body.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    states.insert(s.head_commit().seq, live(&s));
    let more = churn(&s, 6, 60);
    states.extend(more);
    let head = s.head_commit().seq;
    let mut r = Rng(17);
    let mut via_log = 0;
    for _ in 0..60 {
        let a = r.below(head + 1);
        let b = r.below(head + 1);
        let d = s
            .diff(&At::Commit(a), &At::Commit(b), &Default::default())
            .unwrap_or_else(|e| panic!("{a}..{b}: {e}"));
        let (mut rem, mut add) = (BTreeSet::new(), BTreeSet::new());
        for (op, q) in d.iter() {
            match op {
                DiffOp::Add => add.insert(nquads_line(&q)),
                DiffOp::Remove => rem.insert(nquads_line(&q)),
            };
        }
        assert_eq!((rem, add), delta(&states[&a], &states[&b]), "{a}..{b}");
        let readable = s
            .snapshot_at(&At::Commit(a.min(b)), &HistoryOptions::default())
            .is_ok();
        if !readable && a != b {
            via_log += 1;
            assert_eq!(d.method, sparkles::store::DiffMethod::Log, "{a}..{b}");
        }
    }
    assert!(via_log > 0, "some diffs start at a collected state");
}

#[test]
fn the_change_feed_reads_the_change_log_where_generations_are_gone() {
    use sparkles::access::{GraphAccess, GraphRule, Graphs};
    use sparkles::store::ChangesOptions;
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(
        &dir.path().join("db"),
        StoreOptions {
            bulk_threshold: 20,
            change_log_bulk_max_quads: 30,
            ..opts()
        },
    )
    .unwrap();
    let mut states = churn(&s, 11, 120);
    // a bulk commit too large to record, then more commits and compactions
    let body: String = (0..40)
        .map(|i| format!("<urn:bulk{i}> <urn:p0> {i} .\n"))
        .collect();
    s.load(&[Source::from_bytes(
        body.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let bulk = s.head_commit().seq;
    states.insert(bulk, live(&s));
    states.extend(churn(&s, 12, 60));
    s.compact().unwrap();
    upd(&s, "INSERT DATA { <urn:last> <urn:p0> 1 }");
    states.insert(s.head_commit().seq, live(&s));
    let head = s.head_commit().seq;
    assert!(
        s.snapshot_at(&At::Commit(1), &HistoryOptions::default())
            .is_err(),
        "the first commits' generations are collected"
    );
    // follow the feed from the start in pages of several sizes
    for size in [1, 7, 100] {
        let o = ChangesOptions {
            max_commits: size,
            ..Default::default()
        };
        let mut after = 0;
        while after < head {
            let p = s
                .changes(after, &o)
                .unwrap_or_else(|e| panic!("after {after}: {e}"));
            assert!(!p.commits.is_empty(), "after {after}");
            for c in &p.commits {
                let seq = c.commit.seq;
                assert_eq!(seq, after + 1, "commits are consecutive");
                after = seq;
                if !c.complete() {
                    // a bulk commit the log has only the counts of, its states gone
                    assert_eq!(seq, bulk);
                    continue;
                }
                let (mut rem, mut add) = (BTreeSet::new(), BTreeSet::new());
                for (op, q) in c.iter() {
                    match op {
                        DiffOp::Add => add.insert(nquads_line(&q)),
                        DiffOp::Remove => rem.insert(nquads_line(&q)),
                    };
                }
                assert_eq!(
                    (rem, add),
                    delta(&states[&(seq - 1)], &states[&seq]),
                    "commit {seq}"
                );
            }
        }
    }
    // a graph view lists the changes of its graphs only, and every commit
    let o = ChangesOptions {
        max_commits: 1000,
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::Only(GraphRule::new(["default"], &[])),
            Graphs::none(),
        ))),
        ..Default::default()
    };
    let p = s.changes(0, &o).unwrap();
    assert!(p.commits.len() > 1);
    assert_eq!(p.commits[0].commit.seq, 1);
    for c in &p.commits {
        assert!(c.iter().all(|(_, q)| q.graph_name.is_default_graph()));
    }
    // without the change log the commits are gone
    s.set_change_log_settings(ChangeLogSettings {
        enabled: Some(false),
        ..Default::default()
    })
    .unwrap();
    let e = s.changes(0, &ChangesOptions::default()).err().unwrap();
    assert!(matches!(e, Error::HistoryGone(_)), "{e}");
}

#[test]
fn a_log_ahead_of_the_data_starts_again() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let old = dir.path().join("old");
    let s = Store::open(&root, opts()).unwrap();
    for i in 0..5 {
        upd(&s, &format!("INSERT DATA {{ <urn:s{i}> <urn:p> {i} }}"));
    }
    s.flush_change_log().unwrap();
    copy_dir(&root, &old);
    for i in 5..10 {
        upd(&s, &format!("INSERT DATA {{ <urn:s{i}> <urn:p> {i} }}"));
    }
    s.flush_change_log().unwrap();
    // the newer log next to the older data, as a hand-made copy would leave it
    std::fs::remove_dir_all(old.join("changes")).unwrap();
    copy_dir(&root.join("changes"), &old.join("changes"));
    let o = Store::open(&old, opts()).unwrap();
    assert_eq!(o.head_commit().seq, 5);
    let r = o.history_changes(&HistoryQuery::default()).unwrap();
    let seqs: Vec<u64> = r.changes.iter().map(|c| c.commit.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
    // and new commits are recorded
    upd(&o, "INSERT DATA { <urn:new> <urn:p> 1 }");
    let r = o.history_changes(&HistoryQuery::default()).unwrap();
    assert_eq!(
        r.changes.last().unwrap().quad.subject.to_string(),
        "<urn:new>"
    );
    assert_eq!(r.changes.last().unwrap().commit.seq, 6);
}

/// The directory benchmarks write to: `SPARKLES_BENCH_DIR`, or a temporary directory
/// (which may be in memory, where syncs cost nothing).
fn bench_dir() -> tempfile::TempDir {
    match std::env::var("SPARKLES_BENCH_DIR") {
        Ok(d) => {
            std::fs::create_dir_all(&d).unwrap();
            tempfile::tempdir_in(d).unwrap()
        }
        Err(_) => tempfile::tempdir().unwrap(),
    }
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// Single-triple commit latency with the change log on and off, interleaved, plus the
/// log's disk use, its write throughput and its recovery at open. Run with
/// `cargo test --release -p sparkles --test history_log commit_latency -- --ignored
/// --nocapture`, with `SPARKLES_BENCH_DIR` on a real disk.
#[test]
#[ignore]
fn commit_latency_with_and_without_the_change_log() {
    let commits: usize = std::env::var("COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    let rounds = 6;
    let dir = bench_dir();
    let base = || -> Source {
        // a base of 10,000 quads, so commits intern into a real vocabulary
        let b: String = (0..10_000)
            .map(|i| {
                format!(
                    "<http://example.org/s{i}> <http://example.org/p{}> \"value {i}\" .\n",
                    i % 20
                )
            })
            .collect();
        Source::from_bytes(b.into_bytes(), RdfFormat::NTriples, None)
    };
    let edit = |i: usize, tag: &str| {
        format!(
            "INSERT DATA {{ <http://example.org/s{}> <http://example.org/updated> \"edit {i} of {tag}\" }}",
            i % 10_000
        )
    };
    // latency alone, the two settings interleaved so that drift on a shared machine hits
    // both alike
    let mut lat: BTreeMap<bool, Vec<f64>> = BTreeMap::new();
    for round in 0..rounds {
        let order = if round % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        };
        for on in order {
            let root = dir.path().join(format!("r{round}-{on}"));
            let s = Store::open(
                &root,
                StoreOptions {
                    change_log: on,
                    ..Default::default()
                },
            )
            .unwrap();
            s.load(&[base()]).unwrap();
            s.flush_change_log().unwrap();
            let mut v = Vec::with_capacity(commits);
            for i in 0..commits {
                let u = edit(i, &format!("round {round}"));
                let t = std::time::Instant::now();
                update(&s, &u, &QueryOptions::default()).unwrap();
                v.push(t.elapsed().as_secs_f64() * 1e3);
            }
            let mean = v.iter().sum::<f64>() / v.len() as f64;
            let max = v.iter().cloned().fold(0.0, f64::max);
            println!(
                "round {round}, change log {}: mean {mean:.3} ms, p50 {:.3} ms, max {max:.1} ms",
                if on { "on " } else { "off" },
                percentile(&mut v.clone(), 0.5)
            );
            lat.entry(on).or_default().extend(v);
            drop(s);
            let _ = std::fs::remove_dir_all(&root);
        }
    }
    for (on, v) in &mut lat {
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        println!(
            "change log {}: {} commits, mean {mean:.3} ms, p50 {:.3} ms, p90 {:.3} ms, p99 {:.3} ms",
            if *on { "on " } else { "off" },
            v.len(),
            percentile(v, 0.5),
            percentile(v, 0.9),
            percentile(v, 0.99),
        );
    }
    // disk per commit, write throughput and recovery, on a store of its own
    let root = dir.path().join("extras");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.load(&[base()]).unwrap();
    s.flush_change_log().unwrap();
    let bytes0 = s.change_log_status().unwrap().bytes;
    let wal0 = s.wal_bytes();
    for i in 0..commits {
        update(&s, &edit(i, "size"), &QueryOptions::default()).unwrap();
    }
    s.flush_change_log().unwrap();
    let (bytes, wal) = (
        s.change_log_status().unwrap().bytes - bytes0,
        s.wal_bytes() - wal0,
    );
    println!(
        "{commits} single-triple commits: change log +{bytes} bytes ({:.0} per commit), write-ahead log +{wal} bytes ({:.0} per commit)",
        bytes as f64 / commits as f64,
        wal as f64 / commits as f64
    );
    // write throughput: queue the commits, then write them in one go
    s.change_log().unwrap().set_background(false);
    for i in 0..commits {
        update(&s, &edit(i, "queue"), &QueryOptions::default()).unwrap();
    }
    let t = std::time::Instant::now();
    s.flush_change_log().unwrap();
    let w = t.elapsed().as_secs_f64() * 1e3;
    // recovery: queued commits lost in a crash come back from the WAL
    for i in 0..commits {
        update(&s, &edit(i, "crash"), &QueryOptions::default()).unwrap();
    }
    let crash = dir.path().join("crash");
    copy_dir(&root, &crash);
    let t = std::time::Instant::now();
    let c = Store::open(&crash, StoreOptions::default()).unwrap();
    let open_ms = t.elapsed().as_secs_f64() * 1e3;
    assert_eq!(
        c.change_log_status().unwrap().last,
        Some(c.head_commit().seq)
    );
    drop(c);
    let t = std::time::Instant::now();
    drop(Store::open(&crash, StoreOptions::default()).unwrap());
    let reopen_ms = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "wrote {commits} queued commits in {w:.1} ms ({:.1} µs each); an open that recovered {commits} commits took {open_ms:.0} ms, the next open {reopen_ms:.0} ms",
        w * 1e3 / commits as f64
    );
    s.change_log().unwrap().set_background(true);
    // queries: one subject through the index, and every change
    let q = HistoryQuery {
        subjects: vec![Term::NamedNode(
            NamedNode::new("http://example.org/s42").unwrap(),
        )],
        ..Default::default()
    };
    s.history_changes(&q).unwrap();
    let t = std::time::Instant::now();
    let one = s.history_changes(&q).unwrap();
    let one_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = std::time::Instant::now();
    let all = s.history_changes(&HistoryQuery::default()).unwrap();
    let all_ms = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "history of one subject: {} changes in {one_ms:.2} ms; every change ({} over {} commits) in {all_ms:.1} ms",
        one.changes.len(),
        all.changes.len(),
        s.head_commit().seq
    );
    // what a sync of its own per commit would cost: append a record and fdatasync
    let f = dir.path().join("sync-probe");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&f)
        .unwrap();
    let mut v = Vec::new();
    for _ in 0..500 {
        use std::io::Write;
        let t = std::time::Instant::now();
        file.write_all(&[7u8; 160]).unwrap();
        file.sync_data().unwrap();
        v.push(t.elapsed().as_secs_f64() * 1e3);
    }
    println!(
        "a 160-byte append with fdatasync: p50 {:.3} ms, p90 {:.3} ms",
        percentile(&mut v, 0.5),
        percentile(&mut v, 0.9)
    );
}
