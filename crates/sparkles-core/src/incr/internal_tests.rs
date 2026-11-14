//! Internal circuits (spec F13 §3.6): the accumulator's counts against the change
//! feed and against evaluations at every commit, recovery, and the boundary that keeps
//! internal circuits away from callers.

use super::internal::{InternalAccess, InternalSpec};
use super::runtime::{InternalCircuit, OpenHost, Runtime, RuntimeOptions};
use super::views::ViewDef;
use crate::io::RdfFormat;
use crate::sparql::viewsvc::ViewRead;
use crate::sparql::{self, QueryOptions};
use crate::store::{ChangesOptions, Store, StoreOptions};
use oxrdf::{NamedOrBlankNode, Term};
use std::collections::BTreeMap;
use std::sync::Arc;

const DATA: &str = r#"
@prefix : <http://ex/> .
@prefix org: <http://www.w3.org/ns/org#> .
:a org:memberOf :t1 . :b org:memberOf :t1 . :c org:memberOf :t2 .
:a :age 30 . :b :age 40 . :c :age 50 .
"#;
const PREFIX: &str = "PREFIX : <http://ex/> PREFIX org: <http://www.w3.org/ns/org#> ";
/// Every fact of a subject, keyed by the subject: the counter of C22's refresh.
const FACTS: &str = "SELECT ?s ?p ?o WHERE { ?s ?p ?o }";
/// Every fact of a team's members, keyed by the team.
const TEAM_FACTS: &str = "SELECT ?t ?m ?p ?o WHERE { ?m <http://www.w3.org/ns/org#memberOf> ?t . \
                          ?m ?p ?o }";

fn load(s: &Store) {
    s.load(&[crate::io::Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .expect("load");
}

fn upd(s: &Store, u: &str) {
    sparql::update::update(s, &format!("{PREFIX}{u}"), &QueryOptions::default()).expect("update");
}

fn runtime(s: &Store, root: Option<std::path::PathBuf>) -> Arc<Runtime> {
    let rt = Runtime::open(root, s.dataset_id(), RuntimeOptions::default());
    rt.set_host(Arc::new(OpenHost));
    rt
}

fn persistent(dir: &std::path::Path, change_log: bool) -> Store {
    Store::open(
        dir,
        StoreOptions {
            change_log,
            history_max_generations: if change_log { 8 } else { 1 },
            ..Default::default()
        },
    )
    .expect("open")
}

fn name(t: &Option<Term>) -> String {
    match t {
        Some(Term::NamedNode(n)) => n.as_str().trim_start_matches("http://ex/").to_string(),
        Some(t) => t.to_string(),
        None => "UNDEF".into(),
    }
}

fn attach(rt: &Arc<Runtime>, s: &Store, id: &str, query: &str, key: &str) -> InternalCircuit {
    rt.attach_internal(
        s,
        InternalSpec::accumulator(id, query, [key]),
        InternalAccess::everything(),
    )
    .expect("attach")
}

/// The counts after `after` by key name, which must cover every commit.
fn counts(c: &InternalCircuit, after: u64) -> BTreeMap<String, u64> {
    let n = c.counts(after).expect("counts");
    assert!(n.complete(), "{n:?}");
    n.keys
        .iter()
        .map(|(k, n)| (k.iter().map(name).collect::<Vec<_>>().join(" "), *n))
        .collect()
}

/// The feed-based counter: every quad a commit added or removed counts once for its
/// subject.
fn feed_counts(s: &Store, after: u64) -> BTreeMap<String, u64> {
    let page = s.changes(after, &ChangesOptions::default()).expect("feed");
    let mut out = BTreeMap::new();
    for c in &page.commits {
        assert!(c.complete());
        for (_, q) in c.iter() {
            let NamedOrBlankNode::NamedNode(n) = &q.subject else {
                continue;
            };
            let k = n.as_str().trim_start_matches("http://ex/").to_string();
            *out.entry(k).or_insert(0) += 1;
        }
    }
    out
}

/// The query's rows at the head by key, with their multiplicities.
fn rows_at_head(s: &Store, query: &str) -> BTreeMap<Vec<String>, i64> {
    let r = sparql::query(
        s.snapshot(),
        query,
        &QueryOptions {
            no_cache: true,
            ..Default::default()
        },
    )
    .expect("oracle");
    let mut out = BTreeMap::new();
    for row in r.test_application_rows() {
        *out.entry(row.iter().map(name).collect::<Vec<_>>())
            .or_insert(0) += 1;
    }
    out
}

/// The absolute change of the rows from `before` to `after`, summed per key (the
/// first column).
fn churn(
    before: &BTreeMap<Vec<String>, i64>,
    after: &BTreeMap<Vec<String>, i64>,
    into: &mut BTreeMap<String, u64>,
) {
    let mut seen = std::collections::BTreeSet::new();
    for row in before.keys().chain(after.keys()) {
        if !seen.insert(row.clone()) {
            continue;
        }
        let d = after.get(row).copied().unwrap_or(0) - before.get(row).copied().unwrap_or(0);
        if d != 0 {
            *into.entry(row[0].clone()).or_insert(0) += d.unsigned_abs();
        }
    }
}

const WORKLOAD: &[&str] = &[
    // churn across commits: an addition, then its removal
    "INSERT DATA { :a :likes :x }",
    "DELETE DATA { :a :likes :x }",
    // a replacement within one commit
    "DELETE { :b :age 40 } INSERT { :b :age 41 } WHERE {}",
    // a deletion
    "DELETE DATA { :c :age 50 }",
    // a commit that adds and removes the same fact changes nothing
    "INSERT DATA { :d :age 1 } ; DELETE DATA { :d :age 1 }",
    // a member who joins a team and leaves it, and changes in one commit
    "INSERT DATA { :d org:memberOf :t2 . :d :age 20 }",
    "DELETE DATA { :d org:memberOf :t2 } ; INSERT DATA { :d org:memberOf :t1 }",
    "DELETE DATA { :a :age 30 }",
];

/// The accumulator counts what the feed-based counter counts: an addition and a later
/// removal count 2, a replacement within one commit 2 and a deletion 1, whether the
/// runtime keeps up with each commit or applies a run of them.
#[test]
fn accumulator_counts_match_the_feed_counter() {
    for per_commit in [true, false] {
        let s = Store::in_memory(StoreOptions::default());
        load(&s);
        let rt = runtime(&s, None);
        let c = attach(&rt, &s, "facts", FACTS, "s");
        let start = s.snapshot().commit;
        assert_eq!(c.commit(), start);
        for u in WORKLOAD {
            upd(&s, u);
            if per_commit {
                rt.round(&s);
            }
        }
        rt.round(&s);
        assert_eq!(c.commit(), s.snapshot().commit);
        let got = counts(&c, start);
        assert_eq!(got, feed_counts(&s, start), "per commit: {per_commit}");
        assert_eq!(got["a"], 3, "added, removed, and a deletion");
        assert_eq!(got["b"], 2, "a replacement in one commit");
        assert_eq!(got["c"], 1, "a deletion");
        // the counts after any later commit are exact as well
        for after in start..=s.snapshot().commit {
            assert_eq!(counts(&c, after), feed_counts(&s, after), "after {after}");
        }
    }
}

/// A key from a join counts the changes of the circuit's rows at each commit, also when
/// a run of commits is applied at once, as evaluations of the query at every commit
/// give them.
#[test]
fn joined_keys_count_each_commit_of_a_run() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let c = attach(&rt, &s, "teams", TEAM_FACTS, "t");
    let start = s.snapshot().commit;
    let mut expected = BTreeMap::new();
    let mut before = rows_at_head(&s, TEAM_FACTS);
    for u in WORKLOAD {
        upd(&s, u);
        let now = rows_at_head(&s, TEAM_FACTS);
        churn(&before, &now, &mut expected);
        before = now;
    }
    // one round applies the whole run, one commit at a time
    rt.round(&s);
    assert_eq!(counts(&c, start), expected);
    // t2: :c's deletion, then :d joining with two facts and leaving with them
    assert_eq!(expected["t2"], 5, "{expected:?}");
    assert_eq!(expected["t1"], 7, "{expected:?}");
}

/// Discarding counts drops the commits server code acted on, and reads before them are
/// no longer complete.
#[test]
fn discarded_counts_are_gone() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let c = attach(&rt, &s, "facts", FACTS, "s");
    let start = s.snapshot().commit;
    upd(&s, WORKLOAD[0]);
    rt.round(&s);
    let mid = s.snapshot().commit;
    upd(&s, WORKLOAD[1]);
    rt.round(&s);
    c.discard_through(mid);
    assert!(!c.counts(start).unwrap().complete());
    assert_eq!(counts(&c, mid), feed_counts(&s, mid));
}

/// A circuit that keeps its rows reads as a one-off query does.
#[test]
fn a_rows_circuit_keeps_its_rows() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let c = rt
        .attach_internal(
            &s,
            InternalSpec::rows("teams", TEAM_FACTS),
            InternalAccess::everything(),
        )
        .unwrap();
    assert!(c.counts(0).is_err(), "a rows circuit has no counts");
    for u in WORKLOAD {
        upd(&s, u);
        rt.round(&s);
        let r = c.rows().unwrap();
        assert_eq!(r.commit, s.snapshot().commit);
        let mut got = BTreeMap::new();
        for (row, n) in &r.rows {
            got.insert(row.iter().map(name).collect::<Vec<_>>(), *n as i64);
        }
        assert_eq!(got, rows_at_head(&s, TEAM_FACTS), "after {u}");
    }
}

/// Specs that an accumulator cannot count are refused, and an id names one circuit.
#[test]
fn attach_checks_its_spec() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let refused = |spec: InternalSpec| {
        rt.attach_internal(&s, spec, InternalAccess::everything())
            .unwrap_err()
    };
    let grouped = "SELECT ?s (COUNT(*) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?s";
    assert!(
        refused(InternalSpec::accumulator("g", grouped, ["s"]))
            .to_string()
            .contains("grouped")
    );
    assert!(
        refused(InternalSpec::accumulator("k", FACTS, ["x"]))
            .to_string()
            .contains("?x")
    );
    assert!(
        refused(InternalSpec::accumulator(
            "w",
            "SELECT ?s WHERE { ?s ?p ?o }",
            ["s"]
        ))
        .to_string()
        .contains("fact")
    );
    assert!(
        refused(InternalSpec::accumulator("a", "ASK { ?s ?p ?o }", ["s"]))
            .to_string()
            .contains("SELECT")
    );
    assert!(
        refused(InternalSpec::rows("../x", FACTS))
            .to_string()
            .contains("cannot name")
    );
    // a state over its budget fails the circuit
    let small = refused(InternalSpec::rows("small", FACTS).with_memory(16));
    assert!(
        matches!(&small, crate::Error::Component(c) if c.code == "over-budget"),
        "{small}"
    );
    assert_eq!(
        rt.internal_count(),
        0,
        "a circuit that failed its build is not attached"
    );
    let a = attach(&rt, &s, "facts", FACTS, "s");
    // the same spec joins the circuit, another one under the same id is refused
    let b = attach(&rt, &s, "facts", FACTS, "s");
    assert_eq!(rt.internal_count(), 1);
    assert!(
        refused(InternalSpec::accumulator("facts", TEAM_FACTS, ["t"]))
            .to_string()
            .contains("another internal circuit")
    );
    drop(a);
    rt.round(&s);
    assert_eq!(rt.internal_count(), 1, "a handle remains");
    drop(b);
    rt.round(&s);
    assert_eq!(rt.internal_count(), 0, "collected after its last handle");
}

/// Internal circuits are not views: no listing, read, rebuild, removal, backup capture
/// or metric names one, a view or live query of the same query and access has a
/// circuit of its own, and what callers do to views leaves internal circuits alone
/// (spec F13 §3.6).
#[test]
fn internal_circuits_are_invisible_to_callers() {
    use crate::store::DefinitionSource;
    let dir = tempfile::tempdir().unwrap();
    let s = persistent(dir.path(), true);
    load(&s);
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    let c = attach(&rt, &s, "secret", FACTS, "s");
    let start = s.snapshot().commit;
    rt.round(&s);
    assert!(rt.list(&s).is_empty());
    let code = |e: crate::Error| match e {
        crate::Error::Component(c) => c.code.to_string(),
        e => e.to_string(),
    };
    assert_eq!(code(rt.get(&s, "secret").unwrap_err()), "view-not-found");
    assert_eq!(code(rt.rebuild("secret").unwrap_err()), "view-not-found");
    assert_eq!(code(rt.delete("secret").unwrap_err()), "view-not-found");
    let snap = s.snapshot();
    let read = rt.read(&ViewRead {
        name: "secret",
        at: snap.commit,
        snap: Some(&snap),
        allow_stale: false,
        caller: None,
        deadline: None,
    });
    assert_eq!(code(read.err().expect("refused")), "view-not-found");
    assert!(!rt.readable_by("secret", None));
    assert!(rt.capture_definitions().unwrap().is_empty());
    let st = rt.stats(&s);
    assert!(st.views.is_empty());
    assert_eq!((st.circuits, st.internal_circuits), (0, 1));
    // a view and a live query of the same question have circuits of their own
    rt.put(&s, "secret", ViewDef::table(FACTS)).unwrap();
    let live = rt
        .live(&s, FACTS, None, &super::live::LiveOptions::default())
        .unwrap();
    rt.round(&s);
    assert_eq!(
        rt.stats(&s).circuits,
        1,
        "the view and the live query share one"
    );
    upd(&s, WORKLOAD[2]);
    rt.round(&s);
    // removing and rebuilding the view leaves the internal circuit counting
    rt.rebuild("secret").unwrap();
    rt.delete("secret").unwrap();
    drop(live);
    rt.round(&s);
    upd(&s, WORKLOAD[3]);
    rt.round(&s);
    assert_eq!(counts(&c, start), feed_counts(&s, start));
    assert_eq!(rt.stats(&s).circuits, 0);
    // the checkpoints live outside the views' directory
    rt.checkpoint(&s);
    assert!(dir.path().join("circuits/secret/main").is_dir());
    assert!(!dir.path().join("views/secret").exists());
    rt.close(&s);
    // a reopened runtime lists no view from them
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    rt.round(&s);
    assert!(rt.list(&s).is_empty());
}

/// A restart loads the newest checkpoint and counts the commits made since from the
/// change log, so the counts after the first commit stay exact; without the log the
/// gap is reported as unknown.
#[test]
fn restarts_keep_the_counts() {
    let dir = tempfile::tempdir().unwrap();
    let start;
    {
        let s = persistent(dir.path(), true);
        load(&s);
        let rt = runtime(&s, Some(dir.path().to_path_buf()));
        let c = attach(&rt, &s, "facts", FACTS, "s");
        start = c.commit();
        for u in &WORKLOAD[..3] {
            upd(&s, u);
            rt.round(&s);
        }
        rt.close(&s);
    }
    {
        let s = persistent(dir.path(), true);
        for u in &WORKLOAD[3..] {
            upd(&s, u);
        }
    }
    let s = persistent(dir.path(), true);
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    let c = attach(&rt, &s, "facts", FACTS, "s");
    assert_eq!(rt.stats(&s).loaded, 1, "the checkpoint was loaded");
    rt.round(&s);
    assert_eq!(c.commit(), s.snapshot().commit);
    assert_eq!(counts(&c, start), feed_counts(&s, start));
    rt.close(&s);
}

#[test]
fn a_gap_the_log_cannot_bridge_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let start;
    {
        let s = persistent(dir.path(), false);
        load(&s);
        let rt = runtime(&s, Some(dir.path().to_path_buf()));
        let c = attach(&rt, &s, "facts", FACTS, "s");
        start = c.commit();
        upd(&s, WORKLOAD[0]);
        rt.round(&s);
        rt.close(&s);
    }
    let mid;
    {
        let s = persistent(dir.path(), false);
        mid = s.snapshot().commit;
        upd(&s, WORKLOAD[1]);
        // a compaction leaves no history to list the commit from
        s.compact().unwrap();
    }
    let s = persistent(dir.path(), false);
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    let c = attach(&rt, &s, "facts", FACTS, "s");
    assert_eq!(rt.stats(&s).loaded, 1);
    rt.round(&s);
    let n = c.counts(start).unwrap();
    assert!(n.unknown.is_some(), "{n:?}");
    // the commits before the gap keep their counts
    let before = c.counts(start).unwrap();
    assert!(
        before
            .keys
            .iter()
            .any(|(k, n)| name(&k[0]) == "a" && *n == 1)
    );
    // and the commits after it are exact again
    let head = s.snapshot().commit;
    assert!(head > mid);
    upd(&s, WORKLOAD[2]);
    rt.round(&s);
    assert_eq!(counts(&c, head), BTreeMap::from([("b".to_string(), 2)]));
}

/// A checkpoint of a format this release does not read is left in place, and the
/// circuit is built from the data and counts from there.
#[test]
fn unknown_checkpoint_formats_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let s = persistent(dir.path(), true);
    load(&s);
    let state = dir.path().join("circuits/facts/main");
    std::fs::create_dir_all(&state).unwrap();
    let mut bytes = super::persist::encode_as(
        super::persist::INTERNAL,
        &super::persist::Header {
            dataset: s.dataset_id(),
            commit: s.snapshot().commit,
            fingerprint: 0,
        },
        b"",
    );
    bytes[8..12].copy_from_slice(&(super::persist::INTERNAL_FORMAT + 1).to_le_bytes());
    let file = state.join(super::persist::file_name(s.snapshot().commit));
    std::fs::write(&file, &bytes).unwrap();
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    let c = attach(&rt, &s, "facts", FACTS, "s");
    assert_eq!(rt.stats(&s).loaded, 0);
    let start = c.commit();
    // a checkpoint at the same commit does not write over the file
    rt.checkpoint(&s);
    assert_eq!(
        std::fs::read(&file).unwrap(),
        bytes,
        "left in place at its commit"
    );
    upd(&s, WORKLOAD[3]);
    rt.round(&s);
    assert_eq!(counts(&c, start), feed_counts(&s, start));
    rt.checkpoint(&s);
    assert_eq!(std::fs::read(&file).unwrap(), bytes, "left in place");
    // a view checkpoint is not read as an internal one, nor the reverse
    assert!(super::persist::decode_as(super::persist::VIEW, &bytes).is_err());
}

/// The env var that names the crash a child process runs, and its directory.
const CRASH_CHILD: &str = "SPARKLES_INTERNAL_CRASH_CHILD";
const CRASH_DIR: &str = "SPARKLES_INTERNAL_CRASH_DIR";
const CRASHED: i32 = 87;

fn crash(_: &Store) {
    std::process::exit(CRASHED);
}

/// The crash `name` of a child process. The child attaches the accumulator, counts
/// three commits and checkpoints, writes the commit it started from, then commits more
/// and stops at its crash.
fn crash_child(name: &str, dir: &std::path::Path) {
    let s = persistent(dir, true);
    load(&s);
    let rt = runtime(&s, Some(dir.to_path_buf()));
    let c = attach(&rt, &s, "facts", FACTS, "s");
    std::fs::write(dir.join("start"), c.commit().to_string()).unwrap();
    for u in &WORKLOAD[..3] {
        upd(&s, u);
        rt.round(&s);
    }
    rt.checkpoint(&s);
    for u in &WORKLOAD[3..] {
        upd(&s, u);
    }
    match name {
        "after-commit" => crash(&s),
        "pass-applied" => {
            s.set_failpoint("internal-pass-applied", Some(Arc::new(crash)));
            rt.round(&s);
        }
        "checkpoint-written" => {
            rt.round(&s);
            s.set_failpoint("internal-checkpoint-written", Some(Arc::new(crash)));
            rt.checkpoint(&s);
        }
        "pass-after-switch" => {
            s.compact().unwrap();
            upd(&s, "INSERT DATA { :e :age 9 }");
            s.set_failpoint("internal-pass-applied", Some(Arc::new(crash)));
            rt.round(&s);
        }
        other => panic!("no crash {other}"),
    }
    panic!("the crash {name} was not reached");
}

/// A crash between a commit and its pass, after a pass and before its checkpoint,
/// while a checkpoint is written and after a generation switch, followed by an open:
/// every commit after the first is counted once.
#[test]
fn accumulators_recover_from_crashes() {
    const TEST: &str = "incr::internal_tests::accumulators_recover_from_crashes";
    if let Ok(name) = std::env::var(CRASH_CHILD) {
        let dir = std::env::var(CRASH_DIR).expect("the crash directory");
        crash_child(&name, std::path::Path::new(&dir));
        return;
    }
    for name in [
        "after-commit",
        "pass-applied",
        "checkpoint-written",
        "pass-after-switch",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CRASH_CHILD, name)
            .env(CRASH_DIR, dir.path())
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(CRASHED),
            "crash child {name}: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let start: u64 = std::fs::read_to_string(dir.path().join("start"))
            .unwrap()
            .parse()
            .unwrap();
        let s = persistent(dir.path(), true);
        let rt = runtime(&s, Some(dir.path().to_path_buf()));
        let c = attach(&rt, &s, "facts", FACTS, "s");
        assert_eq!(rt.stats(&s).loaded, 1, "{name}: the checkpoint was loaded");
        rt.round(&s);
        assert_eq!(counts(&c, start), feed_counts(&s, start), "{name}");
        upd(&s, "INSERT DATA { :f :age 1 }");
        rt.round(&s);
        assert_eq!(counts(&c, start), feed_counts(&s, start), "{name}: later");
        rt.checkpoint(&s);
        let names: Vec<String> = std::fs::read_dir(dir.path().join("circuits/facts/main"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 1, "{name}: {names:?}");
        rt.close(&s);
    }
}

/// An attach of the same id while the last handle's circuit writes its final checkpoint
/// joins that circuit and keeps its counts, rather than building a new circuit at the
/// head that never loads them.
#[test]
fn an_attach_during_collection_keeps_the_counts() {
    use parking_lot::Mutex;
    let dir = tempfile::tempdir().unwrap();
    let s = persistent(dir.path(), true);
    load(&s);
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    let c = attach(&rt, &s, "facts", FACTS, "s");
    let start = c.commit();
    for u in &WORKLOAD[..3] {
        upd(&s, u);
        rt.round(&s);
    }
    drop(c);
    // the final checkpoint is written and not yet renamed when the attach comes
    let joined: Arc<Mutex<Option<InternalCircuit>>> = Arc::default();
    let (rt2, slot) = (rt.clone(), joined.clone());
    s.set_failpoint(
        "internal-checkpoint-written",
        Some(Arc::new(move |s: &Store| {
            let mut slot = slot.lock();
            if slot.is_none() {
                *slot = Some(attach(&rt2, s, "facts", FACTS, "s"));
            }
        })),
    );
    rt.round(&s);
    s.set_failpoint("internal-checkpoint-written", None);
    let c = joined
        .lock()
        .take()
        .expect("attached during the collection");
    assert_eq!(rt.internal_count(), 1);
    upd(&s, WORKLOAD[3]);
    rt.round(&s);
    assert_eq!(counts(&c, start), feed_counts(&s, start));
}

/// An accumulator keeps the counts and not the rows its query matches.
#[test]
fn an_accumulator_keeps_no_rows() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    for i in 0..2000 {
        upd(&s, &format!("INSERT DATA {{ :p{i} :age {i} }}"));
    }
    let rt = runtime(&s, None);
    let rows = rt
        .attach_internal(
            &s,
            InternalSpec::rows("rows", FACTS),
            InternalAccess::everything(),
        )
        .unwrap();
    let acc = attach(&rt, &s, "acc", FACTS, "s");
    let start = acc.commit();
    assert!(
        acc.bytes() * 20 < rows.bytes(),
        "{} against {}",
        acc.bytes(),
        rows.bytes()
    );
    for u in WORKLOAD {
        upd(&s, u);
    }
    rt.round(&s);
    assert_eq!(counts(&acc, start), feed_counts(&s, start));
}

/// A checkpoint written under a larger budget is held to the budget of the attach that
/// loads it: the oldest counts are dropped, and the later ones stay exact.
#[test]
fn a_recovered_accumulator_keeps_its_new_budget() {
    let dir = tempfile::tempdir().unwrap();
    let s = persistent(dir.path(), true);
    load(&s);
    let start;
    {
        let rt = runtime(&s, Some(dir.path().to_path_buf()));
        let c = attach(&rt, &s, "facts", FACTS, "s");
        start = c.commit();
        for i in 0..200 {
            upd(&s, &format!("INSERT DATA {{ :q{i} :age {i} }}"));
            rt.round(&s);
        }
        rt.close(&s);
    }
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    let c = rt
        .attach_internal(
            &s,
            InternalSpec::accumulator("facts", FACTS, ["s"]).with_memory(8 << 10),
            InternalAccess::everything(),
        )
        .unwrap();
    assert_eq!(rt.stats(&s).loaded, 1);
    assert!(c.bytes() <= 8 << 10, "{}", c.bytes());
    let n = c.counts(start).unwrap();
    assert!(n.unknown.is_some(), "the oldest counts are gone: {n:?}");
    let head = s.snapshot().commit;
    upd(&s, WORKLOAD[2]);
    rt.round(&s);
    assert_eq!(counts(&c, head), feed_counts(&s, head));
}

/// A circuit built at commit 0 of a new store is checkpointed at a clean close, so a
/// reattach after later commits counts them from the change log.
#[test]
fn a_build_at_commit_zero_is_checkpointed() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = persistent(dir.path(), true);
        assert_eq!(s.snapshot().commit, 0);
        let rt = runtime(&s, Some(dir.path().to_path_buf()));
        let c = attach(&rt, &s, "facts", FACTS, "s");
        assert_eq!(c.commit(), 0);
        rt.close(&s);
    }
    {
        let s = persistent(dir.path(), true);
        load(&s);
        for u in WORKLOAD {
            upd(&s, u);
        }
    }
    let s = persistent(dir.path(), true);
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    let c = attach(&rt, &s, "facts", FACTS, "s");
    assert_eq!(
        rt.stats(&s).loaded,
        1,
        "the build at commit 0 was checkpointed"
    );
    rt.round(&s);
    assert_eq!(counts(&c, 0), feed_counts(&s, 0));
}

/// A run of commits that the worker applies at once is held to the budget after each
/// commit, so the counts never grow far past it before the run ends.
#[test]
fn a_long_run_keeps_to_the_budget_at_each_commit() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let rt = runtime(&s, None);
    let budget = 16 << 10;
    let c = rt
        .attach_internal(
            &s,
            InternalSpec::accumulator("facts", FACTS, ["s"]).with_memory(budget),
            InternalAccess::everything(),
        )
        .unwrap();
    for i in 0..2000 {
        upd(&s, &format!("INSERT DATA {{ :r{i} :age {i} }}"));
    }
    rt.round(&s);
    assert_eq!(c.commit(), s.snapshot().commit);
    assert!(c.bytes() <= budget, "{}", c.bytes());
    let (peak, drops) = c.tally_peak();
    assert!(
        peak <= budget + (4 << 10),
        "the counts reached {peak} bytes"
    );
    // counts at the budget are dropped in batches, not after every commit
    assert!((1..200).contains(&drops), "{drops} drops for 2,000 commits");
    let n = c.counts(0).unwrap();
    assert!(n.unknown.is_some(), "the oldest counts were dropped");
}

/// A checkpoint written while the default graph was not the union of the named graphs
/// is not loaded once it is, since the query then reads other quads.
#[test]
fn the_union_default_graph_setting_keeps_its_own_checkpoints() {
    let dir = tempfile::tempdir().unwrap();
    let open = |union: bool| {
        Store::open(
            dir.path(),
            StoreOptions {
                change_log: true,
                union_default_graph: union,
                ..Default::default()
            },
        )
        .expect("open")
    };
    {
        let s = open(false);
        upd(&s, "INSERT DATA { GRAPH :g { :a :p 1 } }");
        let rt = runtime(&s, Some(dir.path().to_path_buf()));
        let c = rt
            .attach_internal(
                &s,
                InternalSpec::rows("all", FACTS),
                InternalAccess::everything(),
            )
            .unwrap();
        assert!(c.rows().unwrap().rows.is_empty());
        rt.close(&s);
    }
    let s = open(true);
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    let c = rt
        .attach_internal(
            &s,
            InternalSpec::rows("all", FACTS),
            InternalAccess::everything(),
        )
        .unwrap();
    assert_eq!(rt.stats(&s).loaded, 0, "built again for the other setting");
    assert_eq!(c.rows().unwrap().rows.len(), 1);
}

/// A rows circuit with provenance checkpoints its annotations in format 2, reloads them
/// after a restart, resolves its fact keys, and is refused as an accumulator.
#[test]
fn provenance_survives_a_restart_in_format_2() {
    use super::prov::{Atom, FactKey, Provenance};
    let dir = tempfile::tempdir().unwrap();
    let query = format!("{PREFIX}SELECT ?t ?m WHERE {{ ?m org:memberOf ?t . ?m :age ?a }}");
    let spec = || InternalSpec::rows("members", query.clone()).with_provenance(Provenance::why());
    let before = {
        let s = persistent(dir.path(), true);
        load(&s);
        let rt = runtime(&s, Some(dir.path().to_path_buf()));
        let c = rt
            .attach_internal(&s, spec(), InternalAccess::everything())
            .expect("attach");
        upd(&s, "INSERT DATA { :d org:memberOf :t2 . :d :age 20 }");
        upd(&s, "DELETE DATA { :a :age 30 }");
        rt.round(&s);
        let rows = c.annotated_rows().expect("rows");
        assert_eq!(rows.rows.len(), 3);
        for (_, n, a) in &rows.rows {
            assert_eq!(
                (*n, a.count, a.witnesses.len(), a.truncated),
                (1, 1, 1, false)
            );
            assert_eq!(a.witnesses[0].len(), 2);
        }
        // the witness of :d resolves to its two quads
        let d = rows
            .rows
            .iter()
            .find(|(r, _, _)| name(&r[1]) == "d")
            .expect(":d");
        let keys: Vec<FactKey> = d.2.witnesses[0]
            .atoms()
            .iter()
            .filter_map(|a| match a {
                Atom::Fact(k) => Some(*k),
                Atom::Absent(_) => None,
            })
            .collect();
        let quads = c.resolve(&keys).expect("resolve");
        assert!(quads.iter().all(|(_, q)| {
            q.as_ref()
                .is_some_and(|(t, _)| name(&Some(t[0].clone())) == "d")
        }));
        rt.checkpoint(&s);
        let files = super::persist::list(&dir.path().join("circuits/members/main"));
        let bytes = std::fs::read(&files[0].1).unwrap();
        assert_eq!(&bytes[8..12], &2u32.to_le_bytes(), "format 2");
        assert!(super::persist::decode_as(super::persist::INTERNAL, &bytes).is_err());
        rows
    };
    let s = persistent(dir.path(), true);
    let rt = runtime(&s, Some(dir.path().to_path_buf()));
    let c = rt
        .attach_internal(&s, spec(), InternalAccess::everything())
        .expect("attach");
    assert_eq!(rt.stats(&s).loaded, 1, "the checkpoint loads");
    let mut a = c.annotated_rows().unwrap().rows;
    let mut b = before.rows;
    a.sort_by_key(|r| format!("{r:?}"));
    b.sort_by_key(|r| format!("{r:?}"));
    assert_eq!(a, b);
    // a circuit without provenance reads a checkpoint of its own format only
    drop(c);
    rt.round(&s);
    let plain = rt
        .attach_internal(
            &s,
            InternalSpec::rows("members", query.clone()),
            InternalAccess::everything(),
        )
        .expect("attach");
    assert_eq!(
        rt.stats(&s).loaded,
        1,
        "the annotated checkpoint is not loaded"
    );
    assert!(plain.annotated_rows().is_err());
    // an accumulator keeps no rows to annotate, and the bound is checked
    for bad in [
        InternalSpec::accumulator("acc", FACTS, ["s"]).with_provenance(Provenance::Count),
        InternalSpec::rows("w0", FACTS).with_provenance(Provenance::Why { max_witnesses: 0 }),
    ] {
        assert!(
            rt.attach_internal(&s, bad, InternalAccess::everything())
                .is_err()
        );
    }
}
