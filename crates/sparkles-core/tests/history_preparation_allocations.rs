//! A bounded read must refuse large transactions before their buffer allocation.
use sparkles_core::{
    Error,
    history::{At, HistoryOptions},
    sparql::{QueryOptions, update::update},
    store::{Store, StoreOptions},
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

struct Counting;
static ENABLED: AtomicBool = AtomicBool::new(false);
static LARGEST: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static MEASURE_LOCK: Mutex<()> = Mutex::new(());

// SAFETY: every allocation, reallocation and deallocation is delegated unchanged
// to System. The counters only observe sizes and never inspect or change a pointer.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            if ENABLED.load(Ordering::SeqCst) {
                LARGEST.fetch_max(layout.size(), Ordering::SeqCst);
                PEAK.fetch_max(live, Ordering::SeqCst);
            }
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, layout, size) };
        if !pointer.is_null() {
            let live = if size >= layout.size() {
                LIVE.fetch_add(size - layout.size(), Ordering::SeqCst) + size - layout.size()
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::SeqCst) - (layout.size() - size)
            };
            if ENABLED.load(Ordering::SeqCst) {
                LARGEST.fetch_max(size, Ordering::SeqCst);
                PEAK.fetch_max(live, Ordering::SeqCst);
            }
        }
        pointer
    }
}

fn measure<T>(operation: impl FnOnce() -> T) -> (T, usize, usize) {
    let base = LIVE.load(Ordering::SeqCst);
    LARGEST.store(0, Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    ENABLED.store(true, Ordering::SeqCst);
    let result = operation();
    ENABLED.store(false, Ordering::SeqCst);
    (
        result,
        LARGEST.load(Ordering::SeqCst),
        PEAK.load(Ordering::SeqCst).saturating_sub(base),
    )
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;
#[test]
fn cold_replay_refuses_transaction_growth_before_allocating_the_transaction() {
    let _lock = MEASURE_LOCK.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreOptions::default()).unwrap();
    let data = (0..12_000)
        .map(|n| format!("<urn:s{n}> <urn:p> 1 ."))
        .collect::<String>();
    update(
        &store,
        &format!("INSERT DATA {{ {data} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    update(
        &store,
        "INSERT DATA { <urn:later> <urn:p> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let cap = 64 << 10;
    let (result, largest, peak) =
        measure(|| store.snapshot_at_limited(&At::Commit(1), &HistoryOptions::default(), cap));
    assert!(matches!(result, Err(Error::BudgetExceeded(_))));
    eprintln!("bounded WAL refusal: largest={largest}, peak={peak}, cap={cap}");
    assert!(
        largest <= cap as usize,
        "whole transaction was allocated first: {largest}"
    );
    assert!(
        peak <= cap as usize,
        "temporary allocation peak exceeded cap: {peak}"
    );
    // The refused attempt must not poison the history cache or select head.
    let (exact, resolved) = store
        .snapshot_at_limited(&At::Commit(1), &HistoryOptions::default(), 64 << 20)
        .unwrap();
    assert_eq!(resolved.commit.seq, 1);
    assert_eq!(exact.commit, 1);
}
