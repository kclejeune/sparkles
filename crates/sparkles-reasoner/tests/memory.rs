//! The memory limit of a run (F16 review findings F3, F4, F7 and F8): every buffer a run
//! allocates is charged before it grows, so that a run over its limit stops near it, an
//! incremental run is charged for the closure it updates, and a run that succeeds
//! reports a peak within its limit.

use oxrdf::Term;
use sparkles_core::index::Perm;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::{
    Cache, Extras, INFERRED_GRAPH, Incremental, Method, Profile, ReasonOptions, ReasonReport,
    infer, materialize_incremental, materialize_with,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The system allocator, counting the bytes allocated now and their peak.
struct Counting;

static NOW: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let n = NOW.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(n, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        NOW.fetch_sub(l.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, size) };
        if !q.is_null() {
            if size > l.size() {
                let n = NOW.fetch_add(size - l.size(), Ordering::Relaxed) + size - l.size();
                PEAK.fetch_max(n, Ordering::Relaxed);
            } else {
                NOW.fetch_sub(l.size() - size, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// The tests one at a time: one measures the heap of the whole process.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

const PREFIXES: &str = "@prefix ex: <http://ex.org/> . \
     @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n";

fn load(s: &Store, text: &str) {
    s.load(&[Source::from_bytes(
        format!("{PREFIXES}{text}").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
}

/// The inferred graph, as text.
fn inferred(s: &Store) -> BTreeSet<String> {
    let snap = s.snapshot();
    let Some(g) = snap.lookup_iri(INFERRED_GRAPH) else {
        return BTreeSet::new();
    };
    snap.scan_keys(Perm::Gspo, &[g.0])
        .unwrap()
        .iter()
        .map(|k| {
            let q = snap.quad_to_terms(&Perm::Gspo.to_quad(k)).unwrap();
            format!("{} {} {}", q.subject, q.predicate, q.object)
        })
        .collect()
}

/// What a full run would write now.
fn expected(s: &Store, p: &Profile) -> BTreeSet<String> {
    let (ts, _) = infer(s.snapshot(), p, &ReasonOptions::default()).unwrap();
    ts.iter()
        .map(|t| format!("{} {} {}", t.subject, t.predicate, t.object))
        .collect()
}

fn limited(max_memory: Option<usize>) -> ReasonOptions {
    ReasonOptions {
        max_memory,
        threads: Some(1),
        ..Default::default()
    }
}

/// A literal made by a rule is charged before it is built, and interned once.
#[test]
fn f16_local_term_budget() {
    let _one = one_at_a_time();
    const MIB: usize = 1 << 20;
    let big = "a".repeat(4 * MIB);
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/> .
         [twice: (?x ex:p ?l) strConcat(?l, ?l, ?y) -> (?x ex:q ?y)]"
            .into(),
    );
    let s = Store::in_memory(StoreOptions::default());
    load(&s, &format!("ex:a ex:p \"{big}\" ."));

    // the doubled literal does not fit a limit of 1 MiB: the run fails, nothing written
    let e = materialize_with(&s, &rules, &Extras::default(), &limited(Some(MIB))).unwrap_err();
    assert!(e.to_string().contains("memory limit"), "{e:#}");
    assert!(
        inferred(&s).is_empty(),
        "a failed run wrote the inferred graph"
    );

    // with room, the literal is exact
    let (ts, one) = infer(s.snapshot(), &rules, &limited(Some(64 * MIB))).unwrap();
    assert_eq!(ts.len(), 1);
    let Term::Literal(l) = &ts[0].object else {
        panic!("{:?}", ts[0].object);
    };
    assert_eq!(l.value().len(), 8 * MIB);
    assert!(l.value().bytes().all(|b| b == b'a'));
    assert!(
        one.peak_bytes as usize >= 8 * MIB && one.peak_bytes as usize <= 64 * MIB,
        "peak {}",
        one.peak_bytes
    );

    // a second subject with the same literal makes the same term: interned once
    load(&s, &format!("ex:b ex:p \"{big}\" ."));
    let (ts, two) = infer(s.snapshot(), &rules, &limited(Some(64 * MIB))).unwrap();
    assert_eq!(ts.len(), 2);
    assert!(
        (two.peak_bytes as usize) < one.peak_bytes as usize + 4 * MIB,
        "the same literal charged twice: {} after {}",
        two.peak_bytes,
        one.peak_bytes
    );
}

/// Runs on many threads that go over a small limit stop near it: the candidates of
/// every thread are charged to one budget before they are buffered.
#[test]
fn f16_shared_candidate_budget() {
    let _one = one_at_a_time();
    const LIMIT: usize = 48_000;
    // the store's blocks a scan decodes, the threads' evaluation state and the error:
    // 20 runs allocated at most 185,663 bytes beyond what they started with, and the
    // slack is that less the limit, 1.5 times over, rounded up to 64 KiB
    const SLACK: usize = 256 << 10;
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from("ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:C .\n");
    for i in 0..200_000 {
        ttl.push_str(&format!("ex:i{i} a ex:A .\n"));
    }
    load(&s, &ttl);
    // the store's change log writer encodes the load on a thread of its own, shortly
    // after the commit: about 3 MB for these triples, which a busy machine can delay
    // into the measured run, so it is written out first
    s.flush_change_log().unwrap();
    let opts = ReasonOptions {
        max_memory: Some(LIMIT),
        threads: Some(16),
        ..Default::default()
    };
    let snap = s.snapshot();
    // a first run warms the store's caches and starts the threads
    let _ = infer(snap.clone(), &Profile::RdfsSimple, &opts);
    let before = NOW.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    let e = infer(snap, &Profile::RdfsSimple, &opts).unwrap_err();
    let over = PEAK.load(Ordering::SeqCst) - before;
    eprintln!("the failed run allocated {over} bytes beyond what it started with");
    assert!(e.to_string().contains("memory limit"), "{e:#}");
    assert!(
        over <= LIMIT + SLACK,
        "the failed run allocated {over} bytes for a limit of {LIMIT}"
    );
}

/// A small dataset, materialized in full with a cache, and the commit of that run.
fn kept(persistent: Option<&std::path::Path>) -> (Store, Cache, u64) {
    let s = match persistent {
        Some(p) => Store::open(p, StoreOptions::default()).unwrap(),
        None => Store::in_memory(StoreOptions::default()),
    };
    let mut ttl = String::from("ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:C .\n");
    for i in 0..2_000 {
        ttl.push_str(&format!("ex:i{i} a ex:A .\n"));
    }
    load(&s, &ttl);
    let cache = Cache::default();
    let r = materialize_incremental(
        &s,
        &Profile::Rdfs,
        &Extras::default(),
        Incremental {
            since: None,
            cache: Some(&cache),
        },
        &ReasonOptions::default(),
    )
    .unwrap();
    let since = r.receipt.unwrap().commit.seq;
    (s, cache, since)
}

/// A change of the default graph: nothing, a deletion or an insert of a triple the
/// graph already has.
fn apply(s: &Store, case: &str) {
    match case {
        "no-op" => {}
        "deletion" => {
            let mut txn = s.write();
            let mut labels = Default::default();
            let q = oxrdf::Quad::new(
                oxrdf::NamedNode::new_unchecked("http://ex.org/i7"),
                oxrdf::NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
                oxrdf::NamedNode::new_unchecked("http://ex.org/A"),
                oxrdf::GraphName::DefaultGraph,
            );
            let q = txn.encode_quad(&q, &mut labels).unwrap();
            txn.delete(q).unwrap();
            txn.commit().unwrap();
        }
        "duplicate insert" => load(s, "ex:i7 a ex:A ."),
        _ => unreachable!(),
    }
}

fn update(
    s: &Store,
    cache: Option<&Cache>,
    since: u64,
    max_memory: Option<usize>,
) -> anyhow::Result<ReasonReport> {
    materialize_incremental(
        s,
        &Profile::Rdfs,
        &Extras::default(),
        Incremental {
            since: Some(since),
            cache,
        },
        &limited(max_memory),
    )
}

/// An incremental run is charged for the closure it updates, whether it comes from
/// memory or from the dataset: under a tiny limit it fails before it writes, the dataset
/// and the kept closure stay as they were, and the next run with room is incremental and
/// equals a full run.
#[test]
fn f16_incremental_runs_are_charged() {
    let _one = one_at_a_time();
    for case in ["no-op", "deletion", "duplicate insert"] {
        // the closure kept in memory
        let (s, cache, since) = kept(None);
        let before = inferred(&s);
        apply(&s, case);
        let e = update(&s, Some(&cache), since, Some(1024)).unwrap_err();
        assert!(e.to_string().contains("memory limit"), "{case}: {e:#}");
        assert_eq!(inferred(&s), before, "{case}: the failed run wrote");
        assert_eq!(
            cache.kept().map(|k| k.1),
            Some(since),
            "{case}: the kept closure was lost"
        );
        let r = update(&s, Some(&cache), since, None).unwrap();
        assert_eq!(r.method, Method::Incremental, "{case}: {:?}", r.fallback);
        assert_eq!(r.changes.as_ref().unwrap().source, "memory", "{case}");
        assert_eq!(inferred(&s), expected(&s, &Profile::Rdfs), "{case}");

        // the closure read from the dataset
        let dir = tempfile::tempdir().unwrap();
        let (s, _cache, since) = kept(Some(&dir.path().join("db")));
        let before = inferred(&s);
        apply(&s, case);
        let e = update(&s, None, since, Some(1024)).unwrap_err();
        assert!(e.to_string().contains("memory limit"), "{case}: {e:#}");
        assert_eq!(inferred(&s), before, "{case}: the failed run wrote");
        let r = update(&s, None, since, None).unwrap();
        assert_eq!(r.method, Method::Incremental, "{case}: {:?}", r.fallback);
        assert_eq!(r.changes.as_ref().unwrap().source, "store", "{case}");
        assert_eq!(inferred(&s), expected(&s, &Profile::Rdfs), "{case}");
    }

    // a limit between the closure and the full need: the closure is taken and the run
    // fails later, after it changed the closure, so the next run does not find it
    let insert = |s: &Store| {
        let mut ttl = String::new();
        for i in 0..600 {
            ttl.push_str(&format!("ex:n{i} a ex:A .\n"));
        }
        load(s, &ttl);
    };
    let (s, cache, since) = kept(None);
    insert(&s);
    let free = update(&s, Some(&cache), since, None).unwrap().peak_bytes as usize;
    let (s, cache, since) = kept(None);
    insert(&s);
    let before = inferred(&s);
    let e = update(&s, Some(&cache), since, Some(free * 17 / 20)).unwrap_err();
    assert!(e.to_string().contains("memory limit"), "{e:#}");
    assert!(
        !format!("{e:#}").contains("closure kept in memory"),
        "the closure did not fit: {e:#}"
    );
    assert_eq!(inferred(&s), before, "the failed run wrote");
    assert_eq!(
        cache.kept(),
        None,
        "a run that failed later kept the closure"
    );
    let r = update(&s, Some(&cache), since, None).unwrap();
    assert_eq!(r.method, Method::Full);
    assert!(
        r.fallback
            .as_deref()
            .is_some_and(|f| f.contains("no closure of the previous run is kept in memory")),
        "{:?}",
        r.fallback
    );
    assert_eq!(inferred(&s), expected(&s, &Profile::Rdfs));
}

/// A closure kept for another dataset is dropped by a run that cannot use it, and does
/// not make that run fail for memory.
#[test]
fn f16_stale_kept_closure_is_ignored() {
    let _one = one_at_a_time();
    // the closure of store A, in memory
    let mut ttl = String::from("ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:C .\n");
    for i in 0..20_000 {
        ttl.push_str(&format!("ex:i{i} a ex:A .\n"));
    }
    let a = Store::in_memory(StoreOptions::default());
    load(&a, &ttl);
    // a second commit, so that the commit of A's closure is not B's
    load(&a, "ex:extra a ex:A .");
    let cache = Cache::default();
    let r = materialize_incremental(
        &a,
        &Profile::Rdfs,
        &Extras::default(),
        Incremental {
            since: None,
            cache: Some(&cache),
        },
        &ReasonOptions::default(),
    )
    .unwrap();
    let a_commit = r.receipt.unwrap().commit.seq;
    assert_eq!(cache.kept().map(|k| k.1), Some(a_commit));

    // store B is persistent and small
    let setup = |dir: &std::path::Path| {
        let b = Store::open(dir, StoreOptions::default()).unwrap();
        let mut ttl = String::from("ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:C .\n");
        for i in 0..100 {
            ttl.push_str(&format!("ex:i{i} a ex:A .\n"));
        }
        load(&b, &ttl);
        let r = materialize_with(&b, &Profile::Rdfs, &Extras::default(), &limited(None)).unwrap();
        let since = r.receipt.unwrap().commit.seq;
        load(&b, "ex:new a ex:A .");
        (b, since)
    };
    // the need of the update without a closure kept
    let twin_dir = tempfile::tempdir().unwrap();
    let (twin, twin_since) = setup(&twin_dir.path().join("db"));
    let free = update(&twin, None, twin_since, None).unwrap().peak_bytes as usize;

    let dir = tempfile::tempdir().unwrap();
    let (b, since) = setup(&dir.path().join("db"));
    assert_ne!(a_commit, since, "the setups share a commit number");
    let r = update(&b, Some(&cache), since, Some(free * 2))
        .unwrap_or_else(|e| panic!("a stale closure made the run fail: {e:#}"));
    assert_eq!(r.changes.as_ref().unwrap().source, "store");
    assert_ne!(
        cache.kept().map(|k| k.1),
        Some(a_commit),
        "the closure of the other dataset is still kept"
    );
}

/// A run that succeeds reports a peak within its limit, and a limit of the peak an
/// unlimited run reports is enough for the same run.
#[test]
fn f16_incremental_reports_peak() {
    let _one = one_at_a_time();
    for case in ["no-op", "deletion", "duplicate insert"] {
        let (s, cache, since) = kept(None);
        apply(&s, case);
        let free = update(&s, Some(&cache), since, None).unwrap();
        assert!(free.peak_bytes > 0, "{case}");
        assert_eq!(
            free.method,
            Method::Incremental,
            "{case}: {:?}",
            free.fallback
        );
        assert_eq!(free.changes.as_ref().unwrap().source, "memory", "{case}");

        let (s, cache, since) = kept(None);
        apply(&s, case);
        // the closures of two setups differ by a few bytes: their local terms hold the
        // time of their first run
        let peak = free.peak_bytes as usize + 256;
        let r = update(&s, Some(&cache), since, Some(peak))
            .unwrap_or_else(|e| panic!("{case}: a limit of the reported peak {peak}: {e:#}"));
        assert_eq!(r.method, Method::Incremental, "{case}");
        assert!(
            r.peak_bytes as usize <= peak,
            "{case}: {} over {peak}",
            r.peak_bytes
        );

        // under it, a run either fails or stays within its limit
        let (s, cache, since) = kept(None);
        apply(&s, case);
        let half = peak / 2;
        match update(&s, Some(&cache), since, Some(half)) {
            Ok(r) => assert!(
                r.peak_bytes as usize <= half,
                "{case}: {} over {half}",
                r.peak_bytes
            ),
            Err(e) => assert!(e.to_string().contains("memory limit"), "{case}: {e:#}"),
        }
    }
    // the closure read from the dataset: a run that reads it, applies the change and
    // writes reports the same
    for case in ["no-op", "deletion", "duplicate insert"] {
        let dir = tempfile::tempdir().unwrap();
        let (s, _cache, since) = kept(Some(&dir.path().join("db")));
        apply(&s, case);
        let free = update(&s, None, since, None).unwrap();
        assert!(free.peak_bytes > 0, "{case}");
        assert_eq!(free.changes.as_ref().unwrap().source, "store", "{case}");

        let dir = tempfile::tempdir().unwrap();
        let (s, _cache, since) = kept(Some(&dir.path().join("db")));
        apply(&s, case);
        let peak = free.peak_bytes as usize + 256;
        let r = update(&s, None, since, Some(peak))
            .unwrap_or_else(|e| panic!("{case}: a limit of the reported peak {peak}: {e:#}"));
        assert!(
            r.peak_bytes as usize <= peak,
            "store {case}: {} over {peak}",
            r.peak_bytes
        );
    }
    // many candidates that collapse in the merge: each rule has its own evaluations and
    // its own set of emitted facts, so 4 rules buffer 4 x 20,000 candidates for 20,000
    // new rows. The run is charged for the buffered candidates and for the merge's
    // buffers sized for all of them, and a limit at the reported peak suffices.
    const N: usize = 20_000;
    const K: usize = 4;
    let mut text = String::from(
        "@prefix ex: <http://ex.org/> .
         @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n",
    );
    for k in 0..K {
        text.push_str(&format!(
            "[r{k}: (?x rdf:type ex:A) -> (?x rdf:type ex:B)]\n"
        ));
    }
    let rules = Profile::Rules(text);
    let collapse = || {
        let s = Store::in_memory(StoreOptions::default());
        let mut ttl = String::new();
        for i in 0..N {
            ttl.push_str(&format!("ex:i{i} a ex:A .\n"));
        }
        load(&s, &ttl);
        s
    };
    let free = materialize_with(&collapse(), &rules, &Extras::default(), &limited(None)).unwrap();
    assert_eq!(free.inferred, N as u64);
    // the candidate buffers (24 bytes each) and the merge's names of them (2 x 8 bytes)
    // are held at once
    let c = K * N;
    assert!(
        free.peak_bytes as usize >= c * 40,
        "peak {} for {c} candidates collapsing to {N} rows",
        free.peak_bytes
    );
    let peak = free.peak_bytes as usize + 256;
    let r = materialize_with(
        &collapse(),
        &rules,
        &Extras::default(),
        &limited(Some(peak)),
    )
    .unwrap_or_else(|e| panic!("collapse: a limit of the reported peak {peak}: {e:#}"));
    assert_eq!(r.inferred, N as u64);
    assert!(
        r.peak_bytes as usize <= peak,
        "collapse: {} over {peak}",
        r.peak_bytes
    );
    // under it, a run either fails or stays within its limit
    let half = peak / 2;
    match materialize_with(
        &collapse(),
        &rules,
        &Extras::default(),
        &limited(Some(half)),
    ) {
        Ok(r) => assert!(
            r.peak_bytes as usize <= half,
            "collapse: {} over {half}",
            r.peak_bytes
        ),
        Err(e) => assert!(e.to_string().contains("memory limit"), "collapse: {e:#}"),
    }
    // a full run likewise
    let (s, _, _) = kept(None);
    let free = materialize_with(&s, &Profile::Rdfs, &Extras::default(), &limited(None)).unwrap();
    let peak = free.peak_bytes as usize + 256;
    let r = materialize_with(&s, &Profile::Rdfs, &Extras::default(), &limited(Some(peak))).unwrap();
    assert!(
        r.peak_bytes as usize <= peak,
        "full: {} over {peak}",
        r.peak_bytes
    );
}

/// RDFS data of `n` triples for the thread-count test: a class chain, a property with a
/// domain, a range and a super-property, and instances that use them.
fn rdfs_store(n: usize) -> Store {
    let mut ttl = String::from(
        "ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:C .\n\
         ex:p rdfs:domain ex:D . ex:p rdfs:range ex:E . ex:p rdfs:subPropertyOf ex:q .\n",
    );
    for i in 0..n {
        if i % 2 == 0 {
            ttl.push_str(&format!("ex:i{i} a ex:A .\n"));
        } else {
            ttl.push_str(&format!("ex:i{i} ex:p ex:i{} .\n", i - 1));
        }
    }
    let s = Store::in_memory(StoreOptions::default());
    load(&s, &ttl);
    s
}

/// A limit just above the peak of a run on one thread holds on 4 and 16 threads: the
/// steps that holders keep unused scale with the limit, so on small data they do not
/// by themselves push a run over a limit that is close to its need.
#[test]
fn f16_limit_holds_on_many_threads() {
    let _one = one_at_a_time();
    for (n, exact_limit) in [(300usize, 1usize << 20), (3_000, 16 << 20)] {
        let s = rdfs_store(n);
        let snap = s.snapshot();
        let with = |threads: usize, max_memory: usize| ReasonOptions {
            max_memory: Some(max_memory),
            threads: Some(threads),
            ..Default::default()
        };
        // the limit of the first run gives a step of 64 or 1,024 bytes, so its peak is
        // within a few KB of the need
        let (_, one) = infer(snap.clone(), &Profile::Rdfs, &with(1, exact_limit)).unwrap();
        let p1 = one.peak_bytes as usize;
        let limit = p1 + p1 / 50;
        let mut worst = 0usize;
        for threads in [1usize, 4, 16] {
            for run in 0..10 {
                let (_, r) = infer(snap.clone(), &Profile::Rdfs, &with(threads, limit))
                    .unwrap_or_else(|e| {
                        panic!("n {n}, {threads} threads, run {run}, limit {limit}: {e:#}")
                    });
                let peak = r.peak_bytes as usize;
                assert!(
                    peak <= limit,
                    "n {n}, {threads} threads: peak {peak} over {limit}"
                );
                if threads == 16 {
                    worst = worst.max(peak);
                }
            }
        }
        eprintln!(
            "n {n}: 1-thread peak {p1}, limit {limit}, largest 16-thread peak {worst} ({:.3} of the 1-thread peak)",
            worst as f64 / p1 as f64
        );
    }
}

/// Keeping the closure after the commit is charged strictly: a run that succeeds reports
/// a peak within its limit whether or not the closure fits, and a closure that does not
/// fit is not kept.
#[test]
fn f16_keeping_after_commit_stays_within_limit() {
    let _one = one_at_a_time();
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/> .
         [dash: (?x ex:p ?l) strConcat(?l, \"-x\", ?y) -> (?x ex:q ?y)]"
            .into(),
    );
    let fresh = || {
        let s = Store::in_memory(StoreOptions::default());
        let mut ttl = String::new();
        for i in 0..3_000 {
            ttl.push_str(&format!("ex:s{i} ex:p \"literal number {i}\" .\n"));
        }
        load(&s, &ttl);
        s
    };
    let run = |max_memory: Option<usize>, cache: &Cache| {
        materialize_incremental(
            &fresh(),
            &rules,
            &Extras::default(),
            Incremental {
                since: None,
                cache: Some(cache),
            },
            &limited(max_memory),
        )
    };
    let lo = materialize_with(&fresh(), &rules, &Extras::default(), &limited(None))
        .unwrap()
        .peak_bytes as usize;
    let hi_cache = Cache::default();
    let hi = run(None, &hi_cache).unwrap().peak_bytes as usize;
    assert!(hi_cache.kept().is_some());
    assert!(hi > lo, "keeping the closure costs nothing: {lo} and {hi}");

    // limits evenly between the two, a grid dense enough to meet the narrow ranges in
    // which a step of the keeping work passes the limit
    const GRID: usize = 160;
    let mut kept_from = None;
    for k in 0..=GRID {
        let limit = lo + (hi - lo) * k / GRID;
        let cache = Cache::default();
        let r = run(Some(limit), &cache)
            .unwrap_or_else(|e| panic!("limit {limit} ({k} of {GRID} from {lo} to {hi}): {e:#}"));
        assert!(
            r.peak_bytes as usize <= limit,
            "limit {limit} ({k} of {GRID}): peak {}",
            r.peak_bytes
        );
        match (kept_from, cache.kept().is_some()) {
            (None, true) => kept_from = Some(k),
            (Some(from), false) => panic!("kept at {from} of {GRID} and not at {k}"),
            _ => {}
        }
    }
    assert!(
        kept_from.is_some(),
        "the largest limit does not keep the closure"
    );
}
