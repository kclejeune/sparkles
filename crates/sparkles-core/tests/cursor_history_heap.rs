//! An audit of the history reader's memory ledger against the allocator (item 829). A
//! counting allocator measures the live heap bytes of a streamed history query at each
//! pull, above what was live before the query, and the test compares them with what the
//! query's grant has charged at that moment. A record whose decoded form is larger than
//! the budget is refused before it is decoded, and the peak heap of the refused query
//! stays within the budget. The allocator counts every thread, so each test holds a lock
//! for its whole body.

use sparkles_core::sparql::pool::QueryPool;
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{CursorOptions, FallbackPolicy, QueryOptions, select_cursor};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let now = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // The old and the new block can be live together while the bytes move.
        let now = LIVE.fetch_add(new, Ordering::Relaxed) + new;
        PEAK.fetch_max(now, Ordering::Relaxed);
        let q = unsafe { System.realloc(p, l, new) };
        LIVE.fetch_sub(if q.is_null() { new } else { l.size() }, Ordering::Relaxed);
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

const P: &str = "PREFIX hist: <urn:x-sparkles:history#> ";
const ALL: &str = "SELECT ?s ?p ?o ?op ?c ?m { SERVICE hist:changes { << ?s ?p ?o >> hist:op ?op ; hist:commit ?c ; hist:message ?m } }";

/// What a query may hold outside its ledger: the parsed query and plan, the cursor and
/// its handles, and the change log's segment indexes that a scan reads.
const ALLOWANCE: u64 = 64 << 10;

fn store(dir: &std::path::Path, persistent: bool) -> Store {
    let o = StoreOptions {
        change_log_segment_bytes: 1 << 20,
        ..Default::default()
    };
    match persistent {
        true => Store::open(dir, o).unwrap(),
        false => Store::in_memory(o),
    }
}

fn commit(s: &Store, u: &str, message: Option<String>) {
    let mut o = QueryOptions::default();
    o.write.message = message.map(Into::into);
    update(s, u, &o).unwrap_or_else(|e| panic!("{e}"));
    s.flush_change_log().unwrap();
}

/// Commits of 200 quads each, 9,000 changes in all.
fn many(s: &Store) {
    for i in 0..45 {
        let body: String = (0..200)
            .map(|j| {
                format!(
                    "<urn:s{i}x{j}> <urn:p{}> \"{}{j}\" . ",
                    j % 5,
                    "w".repeat(j % 40)
                )
            })
            .collect();
        commit(
            s,
            &format!("INSERT DATA {{ {body} }}"),
            Some(format!("commit {i}")),
        );
    }
}

fn options(
    budget: u64,
    grant: Option<Arc<sparkles_core::sparql::pool::PoolGrant>>,
) -> QueryOptions {
    QueryOptions {
        no_cache: true,
        max_memory_bytes: Some(budget),
        grant,
        ..Default::default()
    }
}

fn strict(rows: usize) -> CursorOptions {
    CursorOptions {
        batch_rows: rows,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    }
}

/// Pull `q` `rows` at a time and check the live heap against the grant's charge after
/// every pull. Returns the rows and the largest excess of the heap over the charge.
fn audit(store: &Store, q: &str, budget: u64, rows: usize) -> (usize, i64) {
    let pool = QueryPool::new(1 << 40);
    let grant = Arc::new(pool.try_grant(0).unwrap());
    let opts = options(budget, Some(grant.clone()));
    let base = LIVE.load(Ordering::Relaxed) as i64;
    let mut c = select_cursor(store.snapshot(), &format!("{P}{q}"), &opts, &strict(rows)).unwrap();
    let (mut n, mut worst) = (0, i64::MIN);
    while let Some(b) = c.next_batch().unwrap() {
        n += b.len();
        drop(b);
        let heap = LIVE.load(Ordering::Relaxed) as i64 - base;
        worst = worst.max(heap - grant.used() as i64);
    }
    (n, worst)
}

/// What a history reader holds stays within what its grant has charged plus a fixed
/// allowance at every pull, over pages of whole commits from a file-backed log and from
/// one in memory, both ways through the log.
#[test]
fn a_history_reader_holds_no_more_heap_than_its_ledger_at_every_pull() {
    let _serial = serial();
    for persistent in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), persistent);
        many(&s);
        let descending = ALL.replace(
            "hist:message ?m",
            "hist:message ?m ; hist:order hist:descending",
        );
        for q in [ALL, descending.as_str()] {
            for rows in [1, 100] {
                // one run warms what the store and the allocator keep between queries
                audit(&s, q, 16 << 20, rows);
                let (n, worst) = audit(&s, q, 16 << 20, rows);
                println!("AUDIT persistent={persistent} rows={rows} n={n} worst={worst} q={q}");
                assert_eq!(n, 9000);
                assert!(
                    worst <= ALLOWANCE as i64,
                    "the heap passed the ledger by {worst} bytes: persistent={persistent} {q}"
                );
            }
        }
    }
}

/// Run `q` to its refusal under `budget` and return the error and the peak heap above
/// what was live before it.
fn refused(store: &Store, q: &str, budget: u64) -> (Error, u64) {
    let run = || {
        let mut c = select_cursor(
            store.snapshot(),
            &format!("{P}{q}"),
            &options(budget, None),
            &strict(64),
        )?;
        while c.next_batch()?.is_some() {}
        Ok::<_, Error>(())
    };
    // one run warms what the store and the allocator keep between queries
    let _ = run();
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let e = run().expect_err("refused");
    (e, (PEAK.load(Ordering::Relaxed) - base) as u64)
}

/// Each part of a record's decoded form is admitted before it is allocated: a compact
/// record of many changes over a few short terms, a record of more than 65,536 distinct
/// short terms, and a page of commits whose messages it holds with them. Each budget
/// holds the records' bytes but not their decoded form, and the query is refused with a
/// peak heap within the budget.
#[test]
fn a_record_larger_than_the_budget_is_refused_before_it_is_decoded() {
    let _serial = serial();
    for persistent in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), persistent);
        // 27,000 changes over 90 terms
        let body: String = (0..27_000)
            .map(|n| {
                format!(
                    "<urn:a{}> <urn:b{}> <urn:c{}> . ",
                    n % 30,
                    n / 30 % 30,
                    n / 900
                )
            })
            .collect();
        commit(&s, &format!("INSERT DATA {{ {body} }}"), None);
        let compact =
            "SELECT ?s { SERVICE hist:changes { << ?s ?p ?o >> hist:from 1 ; hist:to 1 } }";
        // 70,000 changes over as many distinct terms
        let body: String = (0..70_000)
            .map(|n| format!("<urn:t{n}> <urn:p> 1 . "))
            .collect();
        commit(&s, &format!("INSERT DATA {{ {body} }}"), None);
        let terms = "SELECT ?s { SERVICE hist:changes { << ?s ?p ?o >> hist:from 2 ; hist:to 2 } }";
        // 2,000 commits of one quad, each with a message of 1,000 bytes, the longest a
        // message may be: a page holds every commit's message
        for i in 0..2000 {
            commit(
                &s,
                &format!("INSERT DATA {{ <urn:m{i}> <urn:p> 1 }}"),
                Some(format!("{i:04}{}", "m".repeat(996))),
            );
        }
        let message = "SELECT ?s { SERVICE hist:changes { << ?s ?p ?o >> hist:from 3 } }";
        for (q, budget) in [(compact, 1u64 << 20), (terms, 2 << 20), (message, 3 << 19)] {
            let (e, peak) = refused(&s, q, budget);
            println!("REFUSED persistent={persistent} budget={budget} peak={peak} {e} q={q}");
            assert!(
                matches!(&e, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory),
                "{q}: {e}"
            );
            assert!(
                peak <= budget + ALLOWANCE,
                "peak {peak} passed the budget {budget}: persistent={persistent} {q}"
            );
        }
    }
}
