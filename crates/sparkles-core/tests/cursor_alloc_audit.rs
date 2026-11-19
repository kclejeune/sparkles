//! An audit of the cursor's memory ledger against the allocator. A counting allocator
//! measures the peak of the heap bytes that a streamed query allocates and still holds,
//! and the test compares that peak with the peak the query's ledger reports. A charge
//! that a formula sizes apart from the allocation it stands for can fall short of it
//! without any answer changing, and this comparison is the check that does not read the
//! formula.
//!
//! The allocator counts a block only when a counted thread allocates it inside a
//! measured window, and it remembers the block's address, so a free lowers the count
//! only for a block that the window counted, whichever thread frees it. Memory that
//! another thread allocates or frees, and memory from before the window, cannot move
//! the reading in either direction. The counted threads are the one thread of the Rayon
//! pool that runs each query, which also takes the query's parallel work, and the
//! engine's block decoders, which a query waits for. Work that a query hands to any
//! other thread is outside the audit. Each test holds a lock for its whole body, since
//! the window is one for the process.

use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, ExecutionMode, Optimizations, QueryExecution, QueryOptions, query_execution,
};
use sparkles_core::spill::{Spill, SpillOptions};
use sparkles_core::store::{Store, StoreOptions};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Counting;

thread_local! {
    /// Whether this thread's allocations count. It has no destructor and no lazy
    /// initializer, so the allocator may read it.
    static COUNTED: Cell<bool> = const { Cell::new(false) };
}

fn counted() -> bool {
    COUNTED.try_with(Cell::get).unwrap_or(false)
}

/// The counted blocks that are live, as an open-addressed table of addresses and sizes
/// that the allocator can keep without allocating. `LOCK` guards the table and the
/// counters beside it.
const SLOTS: usize = 1 << 17;
static ADDRESS: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
static SIZE: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
static LOCK: AtomicBool = AtomicBool::new(false);
/// Whether a window is open, the blocks in the table, their bytes and the most bytes
/// they reached, and whether the table ever filled, which voids the reading.
static OPEN: AtomicBool = AtomicBool::new(false);
static BLOCKS: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static FULL: AtomicBool = AtomicBool::new(false);

fn locked<T>(f: impl FnOnce() -> T) -> T {
    while LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        std::hint::spin_loop();
    }
    let r = f();
    LOCK.store(false, Ordering::Release);
    r
}

fn slot(address: usize) -> usize {
    (address >> 3).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (usize::BITS - SLOTS.trailing_zeros())
}

/// Count a block that a counted thread allocated inside the window.
fn insert(address: usize, size: usize) {
    if !OPEN.load(Ordering::Relaxed) || !counted() {
        return;
    }
    locked(|| {
        if BLOCKS.load(Ordering::Relaxed) >= SLOTS / 2 {
            FULL.store(true, Ordering::Relaxed);
            return;
        }
        let mut at = slot(address);
        while ADDRESS[at].load(Ordering::Relaxed) != 0 {
            at = (at + 1) % SLOTS;
        }
        ADDRESS[at].store(address, Ordering::Relaxed);
        SIZE[at].store(size, Ordering::Relaxed);
        BLOCKS.fetch_add(1, Ordering::Relaxed);
        let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
        PEAK.fetch_max(live, Ordering::Relaxed);
    });
}

/// Stop counting a block, if the window counted it. Later blocks of its probe run move
/// back over the gap, so a search still ends at the first empty slot.
fn remove(address: usize) {
    if BLOCKS.load(Ordering::Relaxed) == 0 {
        return;
    }
    locked(|| {
        let mut at = slot(address);
        loop {
            match ADDRESS[at].load(Ordering::Relaxed) {
                0 => return,
                a if a == address => break,
                _ => at = (at + 1) % SLOTS,
            }
        }
        LIVE.fetch_sub(SIZE[at].load(Ordering::Relaxed), Ordering::Relaxed);
        BLOCKS.fetch_sub(1, Ordering::Relaxed);
        let mut gap = at;
        let mut next = (at + 1) % SLOTS;
        loop {
            let a = ADDRESS[next].load(Ordering::Relaxed);
            if a == 0 {
                break;
            }
            // The block stays if its home slot lies after the gap and up to its place.
            let home = slot(a);
            let stays = if gap <= next {
                gap < home && home <= next
            } else {
                gap < home || home <= next
            };
            if !stays {
                ADDRESS[gap].store(a, Ordering::Relaxed);
                SIZE[gap].store(SIZE[next].load(Ordering::Relaxed), Ordering::Relaxed);
                gap = next;
            }
            next = (next + 1) % SLOTS;
        }
        ADDRESS[gap].store(0, Ordering::Relaxed);
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            insert(p as usize, l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        remove(p as usize);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // The old and the new block can be live together while the bytes move, so the
        // new size is counted in full before the old block leaves the count.
        let old = locked(|| LIVE.load(Ordering::Relaxed));
        let grows = OPEN.load(Ordering::Relaxed) && counted();
        if grows {
            PEAK.fetch_max(old + new, Ordering::Relaxed);
        }
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            remove(p as usize);
            insert(q as usize, new);
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The engine's start hook for its long-lived threads. A query waits for the block
/// decoders, so they count, and the store's other workers do not.
fn engine_thread() {
    if std::thread::current()
        .name()
        .is_some_and(|name| name.starts_with("sparkles-decode"))
    {
        COUNTED.set(true);
    }
}

/// The one-thread pool whose thread is counted. A query that runs in it keeps its
/// parallel work on that thread.
fn pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        sparkles_core::threads::set_start_hook(engine_thread);
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .start_handler(|_| COUNTED.set(true))
            .build()
            .unwrap()
    })
}

/// The most bytes that `f`, run on the counted thread, held of what it allocated.
fn peak<T: Send>(f: impl FnOnce() -> T + Send) -> (T, usize) {
    pool().install(|| {
        locked(|| {
            for address in &ADDRESS {
                address.store(0, Ordering::Relaxed);
            }
            BLOCKS.store(0, Ordering::Relaxed);
            LIVE.store(0, Ordering::Relaxed);
            PEAK.store(0, Ordering::Relaxed);
        });
        OPEN.store(true, Ordering::Relaxed);
        let r = f();
        OPEN.store(false, Ordering::Relaxed);
        assert!(!FULL.load(Ordering::Relaxed), "the block table filled");
        (r, PEAK.load(Ordering::Relaxed))
    })
}

/// The meter reads what the counted thread allocates and holds, whatever is freed or
/// allocated around it.
#[test]
fn the_meter_counts_the_window_of_the_counted_thread_alone() {
    let _serial = serial();
    const MIB: usize = 1 << 20;
    // A block from before the window, freed inside it, does not lower the reading.
    let before = pool().install(|| Vec::<u8>::with_capacity(4 * MIB));
    let (_, heap) = peak(move || {
        drop(before);
        let held = Vec::<u8>::with_capacity(MIB);
        // Another thread's blocks do not raise it.
        std::thread::spawn(|| drop(std::hint::black_box(Vec::<u8>::with_capacity(8 * MIB))))
            .join()
            .unwrap();
        let mut grown = Vec::<u8>::with_capacity(MIB);
        grown.reserve_exact(2 * MIB);
        drop((held, grown));
    });
    // The held block, and the old and the new block of the one that grew.
    assert!((4 * MIB..5 * MIB).contains(&heap), "{heap}");
    // A block that the window counted and another thread frees leaves the count.
    let (_, heap) = peak(|| {
        let block = Vec::<u8>::with_capacity(2 * MIB);
        std::thread::spawn(move || drop(block)).join().unwrap();
        drop(std::hint::black_box(Vec::<u8>::with_capacity(MIB)));
    });
    assert!((2 * MIB..3 * MIB).contains(&heap), "{heap}");
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

/// What one streamed run of a query read: its rows and batches, the largest batch, the
/// batches under [`SHORT`] rows before the last, the ledger's peak and the plan.
struct Reading {
    rows: usize,
    batches: usize,
    largest: usize,
    short: usize,
    ledger: u64,
    plan: String,
}

/// Run `q` as a cursor on the counted thread and read the heap it held at its peak.
fn measure(s: &Store, q: &str, opts: &QueryOptions, cursor: &CursorOptions) -> (Reading, usize) {
    peak(|| {
        let QueryExecution::Select(mut c) =
            query_execution(s.snapshot(), q, opts, cursor, ExecutionMode::Streaming).unwrap()
        else {
            panic!("{q} did not stream");
        };
        // The lengths are counted without a list, which would be heap of the test's
        // own that grows with the batches.
        let (mut rows, mut batches, mut largest, mut short) = (0, 0, 0, 0);
        let mut last = 0;
        while let Some(batch) = c.next_batch().unwrap() {
            let len = batch.len();
            // The batch before this one was not the last.
            short += usize::from(batches > 0 && last < SHORT);
            (rows, batches, largest, last) = (rows + len, batches + 1, largest.max(len), len);
        }
        let ledger = c.stats().mem_peak_bytes;
        // The plan's text is the test's own heap, so it is built outside the window.
        OPEN.store(false, Ordering::Relaxed);
        let plan = c.plan_json().unwrap();
        Reading {
            rows,
            batches,
            largest,
            short,
            ledger,
            plan,
        }
    })
}

/// Whether the cursor ran the plan's join or OPTIONAL as a merge on `?s`.
fn merges(plan: &str) -> bool {
    plan.contains("[merge on ?s]")
        || plan.contains(r#""operator":"MergeJoin","description":"on ?s""#)
}

/// The heap a streamed merge holds stays within what its ledger reports plus a fixed
/// allowance, whichever way the merge tests an OPTIONAL's condition and however many
/// rows pass through it. Two runs warm what the store keeps between queries, such as
/// decoded blocks, and each of the three runs after them must pass. A conditional
/// OPTIONAL that tests its pairs before it copies them also fills its batches, so at
/// most one batch before the last is short.
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
                for run in 0..5 {
                    let (r, heap) = measure(&s, q, &opts, &cursor);
                    assert!(merges(&r.plan), "{q}: {}", r.plan);
                    assert!(r.rows >= subjects);
                    if run < 2 {
                        continue;
                    }
                    println!(
                        "AUDIT subjects={subjects} pairs={pairs} run={run} rows={} batches={} \
                         largest={} short={} heap={heap} ledger={} q={q}",
                        r.rows, r.batches, r.largest, r.short, r.ledger
                    );
                    if heap as u64 > r.ledger + ALLOWANCE {
                        over.push(format!(
                            "heap {heap} over ledger {}: {subjects} subjects, pairs {pairs}, \
                             run {run}, {q}",
                            r.ledger
                        ));
                    }
                    if pairs && q.contains("FILTER") && r.short > 1 {
                        over.push(format!(
                            "{} of {} batches are short: {subjects} subjects, {q}",
                            r.short, r.batches
                        ));
                    }
                }
            }
        }
    }
    assert!(over.is_empty(), "{}", over.join("\n"));
}

/// A merge whose right run of equal keys outgrows a small budget writes the run to a
/// spill file and joins from it, and the heap it holds then, with the frames it reads
/// back, stays within the ledger and the allowance for spooled runs.
#[test]
fn a_spooled_merge_run_holds_no_more_heap_than_its_ledger_reports() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let spill = Spill::open(&SpillOptions {
        dir: dir.path().to_path_buf(),
        max_bytes: 1 << 30,
        query_bytes: 1 << 30,
        min_free_bytes: 0,
        encrypted: false,
    })
    .unwrap();
    // 2,000 subjects with a value and an object, and one with 20 values and 40,000
    // objects, whose run of objects no batch holds.
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    let integer = "http://www.w3.org/2001/XMLSchema#integer";
    for i in 0..2000 {
        data.push_str(&format!(
            "<urn:s:{i}> <urn:p> \"{}\"^^<{integer}> .\n",
            i % 97
        ));
        data.push_str(&format!("<urn:s:{i}> <urn:q> <urn:o:{}> .\n", i % 500));
    }
    for v in 0..20 {
        data.push_str(&format!("<urn:hub> <urn:p> \"{v}\"^^<{integer}> .\n"));
    }
    for o in 0..40_000 {
        data.push_str(&format!("<urn:hub> <urn:q> <urn:o:{o}> .\n"));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let mut over = Vec::new();
    for q in [QUERIES[0], QUERIES[3], QUERIES[4]] {
        for pairs in [true, false] {
            let opts = QueryOptions {
                max_memory_bytes: Some(SPOOL_BUDGET),
                no_cache: true,
                spill: Some(spill.clone()),
                optimizations: Some(Optimizations {
                    merge_pair_conditions: pairs,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let cursor = CursorOptions::default();
            for run in 0..5 {
                let (r, heap) = measure(&s, q, &opts, &cursor);
                assert!(merges(&r.plan), "{q}: {}", r.plan);
                assert!(r.plan.contains("spooledRuns"), "{q}: {}", r.plan);
                assert!(r.rows >= 2000 + 20);
                if run < 2 {
                    continue;
                }
                println!(
                    "AUDIT spooled pairs={pairs} run={run} rows={} batches={} heap={heap} \
                     ledger={} q={q}",
                    r.rows, r.batches, r.ledger
                );
                if heap as u64 > r.ledger + ALLOWANCE {
                    over.push(format!(
                        "heap {heap} over ledger {}: spooled, pairs {pairs}, run {run}, {q}",
                        r.ledger
                    ));
                }
            }
        }
    }
    assert_eq!(spill.reserved_bytes(), 0);
    assert!(over.is_empty(), "{}", over.join("\n"));
}

/// A batch under this many rows is short, at the default batch size of 4096 rows.
const SHORT: usize = 512;

/// What a query may hold outside its ledger: the parsed query and plan, the cursor and
/// its handles, and allocator rounding. The queries here measured up to 14 KiB.
const ALLOWANCE: u64 = 32 << 10;

/// The budget under which the run of 40,000 objects spools.
const SPOOL_BUDGET: u64 = 2 << 20;
