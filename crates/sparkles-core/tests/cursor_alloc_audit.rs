//! An audit of the cursor's memory ledger against the allocator. A counting allocator
//! measures the peak of live heap bytes while a streamed query runs, above what was
//! live before it, and the test compares that peak with the peak the query's ledger
//! reports. A charge that a formula sizes apart from the allocation it stands for can
//! fall short of it without any answer changing, and this comparison is the check that
//! does not read the formula.
//! The allocator counts every thread, so each test holds a lock for its whole body.

use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, ExecutionMode, Optimizations, QueryExecution, QueryOptions, query_execution,
};
use sparkles_core::store::{Store, StoreOptions};
use std::alloc::{GlobalAlloc, Layout, System};
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

/// Peak bytes allocated while `f` runs, above what was live before it.
fn peak<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let r = f();
    (r, PEAK.load(Ordering::Relaxed).saturating_sub(base))
}

/// `subjects` subjects with `p`, each with one to four `q` objects of which a third
/// have `r`.
fn store(subjects: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..subjects {
        data.push_str(&format!(
            "<urn:s:{i}> <urn:p> \"{}\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n",
            i % 97
        ));
        for j in 0..(1 + i % 4) {
            data.push_str(&format!(
                "<urn:s:{i}> <urn:q> <urn:o:{}> .\n",
                (i * 7 + j * 331) % 3000
            ));
        }
    }
    for o in 0..3000 {
        if o % 3 == 0 {
            data.push_str(&format!("<urn:o:{o}> <urn:r> \"kept\" .\n"));
        }
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s
}

/// Merges on `?s` with one to four pairs for each left row: three conditional
/// OPTIONALs, an OPTIONAL without a condition and a join.
const QUERIES: &[&str] = &[
    "SELECT ?s ?v ?o { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o FILTER(?o = <urn:o:7>) } }",
    "SELECT ?s ?v ?o { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o FILTER EXISTS { ?o <urn:r> ?k } } }",
    "SELECT ?s ?v ?o { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o \
     FILTER(?v = 5 || NOT EXISTS { ?o <urn:r> ?k }) } }",
    "SELECT ?s ?v ?o { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o } }",
    "SELECT ?s ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o }",
];

/// The heap a streamed merge holds stays within what its ledger reports plus a fixed
/// allowance, whichever way the merge tests an OPTIONAL's condition and however many
/// rows pass through it. A conditional OPTIONAL that tests its pairs before it copies
/// them also ends a batch only when the next left row's pairs do not fit, so at most
/// one batch before the last is under half the largest.
#[test]
fn a_streamed_merge_holds_no_more_heap_than_its_ledger_reports() {
    let _serial = serial();
    let mut over = Vec::new();
    for subjects in [20_000, 80_000] {
        let s = store(subjects);
        for q in QUERIES {
            for pairs in [true, false] {
                let opts = QueryOptions {
                    max_memory_bytes: Some(64 << 20),
                    no_cache: true,
                    optimizations: Some(Optimizations {
                        merge_pair_conditions: pairs,
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let cursor = CursorOptions::default();
                // Two runs warm what the store and the allocator keep between queries.
                // The allocator counts every thread, so a run beside work the store
                // finishes after a load reads high, and the smallest of a few runs is
                // the query's own heap.
                let mut best: Option<(usize, usize, usize, usize, u64, usize)> = None;
                for run in 0..40 {
                    let ((rows, batches, largest, short, ledger), heap) = peak(|| {
                        let QueryExecution::Select(mut c) = query_execution(
                            s.snapshot(),
                            q,
                            &opts,
                            &cursor,
                            ExecutionMode::Streaming,
                        )
                        .unwrap() else {
                            panic!("{q} did not stream");
                        };
                        // The lengths are counted without a list, which would be heap
                        // of the test's own that grows with the batches.
                        let (mut rows, mut batches, mut largest, mut short) = (0, 0, 0, 0);
                        let mut last = 0;
                        while let Some(batch) = c.next_batch().unwrap() {
                            let len = batch.len();
                            // The batch before this one was not the last.
                            short += usize::from(batches > 0 && last < SHORT);
                            (rows, batches, largest, last) =
                                (rows + len, batches + 1, largest.max(len), len);
                        }
                        (rows, batches, largest, short, c.stats().mem_peak_bytes)
                    });
                    if run < 2 {
                        continue;
                    }
                    if best.is_none_or(|b| heap < b.5) {
                        best = Some((rows, batches, largest, short, ledger, heap));
                    }
                    if heap as u64 <= ledger + ALLOWANCE {
                        break;
                    }
                }
                {
                    let (rows, batches, largest, short, ledger, heap) = best.unwrap();
                    println!(
                        "AUDIT subjects={subjects} pairs={pairs} rows={rows} batches={batches} \
                         largest={largest} short={short} heap={heap} ledger={ledger} q={q}"
                    );
                    assert!(rows >= subjects);
                    if heap as u64 > ledger + ALLOWANCE {
                        over.push(format!(
                            "heap {heap} over ledger {ledger}: {subjects} subjects, pairs {pairs}, {q}"
                        ));
                    }
                    if pairs && q.contains("FILTER") && short > 1 {
                        over.push(format!(
                            "{short} of {batches} batches are short: {subjects} subjects, {q}"
                        ));
                    }
                }
            }
        }
    }
    assert!(over.is_empty(), "{}", over.join("\n"));
}

/// A batch under this many rows is short, at the default batch size of 4096 rows.
const SHORT: usize = 512;

/// What a query may hold outside its ledger: the parsed query and plan, the cursor and
/// its handles, and allocator rounding. The queries here measured up to 14 KiB.
const ALLOWANCE: u64 = 32 << 10;
