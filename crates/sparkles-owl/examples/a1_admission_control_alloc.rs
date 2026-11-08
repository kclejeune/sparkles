//! Separate requested-allocation pass. Never use this binary for throughput.
use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use serde_json::{Value, json};

mod a1_r4;

#[global_allocator]
static GLOBAL: Counting = Counting;
struct Counting;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static REQUESTS: AtomicU64 = AtomicU64::new(0);
static REQUESTED: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK_LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK_REQUEST_OVERLAP: AtomicI64 = AtomicI64::new(0);
static FAILURES: AtomicU64 = AtomicU64::new(0);

fn requested(size: usize) {
    REQUESTS.fetch_add(1, Ordering::Relaxed);
    REQUESTED.fetch_add(size as u64, Ordering::Relaxed);
    // For realloc this includes old+requested new storage even if mimalloc can
    // resize in place. This is a requested replacement overlap, not physical RSS.
    PEAK_REQUEST_OVERLAP.fetch_max(LIVE.load(Ordering::Relaxed) + size as i64, Ordering::Relaxed);
}

fn held(delta: i64) {
    let n = LIVE.fetch_add(delta, Ordering::Relaxed) + delta;
    PEAK_LIVE.fetch_max(n, Ordering::Relaxed);
}

// SAFETY: the allocator forwards every pointer, size, alignment and operation to
// the same existing server allocator unchanged. Atomics inspect layouts/results.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let active = ACTIVE.load(Ordering::Relaxed);
        if active { requested(layout.size()); }
        let p = unsafe { sparkles_alloc::Mimalloc.alloc(layout) };
        if active {
            if p.is_null() { FAILURES.fetch_add(1, Ordering::Relaxed); } else { held(layout.size() as i64); }
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let active = ACTIVE.load(Ordering::Relaxed);
        if active { requested(layout.size()); }
        let p = unsafe { sparkles_alloc::Mimalloc.alloc_zeroed(layout) };
        if active {
            if p.is_null() { FAILURES.fetch_add(1, Ordering::Relaxed); } else { held(layout.size() as i64); }
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        if ACTIVE.load(Ordering::Relaxed) { held(-(layout.size() as i64)); }
        unsafe { sparkles_alloc::Mimalloc.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let active = ACTIVE.load(Ordering::Relaxed);
        if active { requested(size); }
        let replacement = unsafe { sparkles_alloc::Mimalloc.realloc(p, layout, size) };
        if active {
            if replacement.is_null() { FAILURES.fetch_add(1, Ordering::Relaxed); }
            else { held(size as i64 - layout.size() as i64); }
        }
        replacement
    }
}

struct Allocations;
impl a1_r4::Meter for Allocations {
    const NAME: &'static str = "requested-allocation-mimalloc";
    fn begin() {
        assert!(!ACTIVE.load(Ordering::Relaxed));
        REQUESTS.store(0, Ordering::Relaxed); REQUESTED.store(0, Ordering::Relaxed);
        LIVE.store(0, Ordering::Relaxed); PEAK_LIVE.store(0, Ordering::Relaxed);
        PEAK_REQUEST_OVERLAP.store(0, Ordering::Relaxed); FAILURES.store(0, Ordering::Relaxed);
        ACTIVE.store(true, Ordering::Relaxed);
    }
    fn snapshot() -> Value {
        // Receipt JSON construction must not count itself. It stays alive beyond
        // end(), so its later destruction also remains outside the counted window.
        let active = ACTIVE.swap(false, Ordering::Relaxed);
        let record = json!({"requested_events": REQUESTS.load(Ordering::Relaxed),
            "requested_bytes": REQUESTED.load(Ordering::Relaxed),
            "live_delta_bytes": LIVE.load(Ordering::Relaxed), "peak_live_delta_bytes": PEAK_LIVE.load(Ordering::Relaxed),
            "peak_requested_replacement_overlap_bytes": PEAK_REQUEST_OVERLAP.load(Ordering::Relaxed),
            "allocation_failures": FAILURES.load(Ordering::Relaxed), "physical_overlap_measured": false});
        ACTIVE.store(active, Ordering::Relaxed);
        record
    }
    fn end() -> Value { ACTIVE.store(false, Ordering::Relaxed); Self::snapshot() }
}

fn main() { a1_r4::main::<Allocations>(); }
