//! The PCA of a projection (spec U11 §3.3) stays within the working memory that
//! `working_bytes` admits for it, on many threads: its accumulators are one per thread,
//! not one per split of a parallel fold. A counting allocator measures the peak.

use sparkles_core::vector::projection::{BLOCK, principal};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static NOW: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let now = NOW.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(now, Ordering::SeqCst);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        NOW.fetch_sub(layout.size(), Ordering::SeqCst);
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

#[test]
fn the_pca_holds_one_accumulator_per_thread() {
    let (m, dim, threads) = (512usize, 2048usize, 16usize);
    let sample: Vec<f64> = (0..m * dim).map(|i| ((i as f64) * 0.618).sin()).collect();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    // the pool's threads and their stacks are not the PCA's
    pool.install(|| ());
    let base = NOW.load(Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    pool.install(|| principal(&sample, dim, 1, &|| Ok(())).unwrap());
    let used = PEAK.load(Ordering::SeqCst) - base;
    // the bound `working_bytes` gives the PCA: a block of directions per thread and four
    // more, and the small eigenproblem
    let k = BLOCK.min(dim);
    let bound = (threads + 4) * k * dim * 8 + 64 * 64 * 8;
    assert!(
        used <= bound,
        "the PCA held {used} bytes, more than {bound}"
    );
}
