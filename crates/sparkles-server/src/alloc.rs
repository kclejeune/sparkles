//! Global allocator selection and release of retained heap memory when the server is idle.
//!
//! Query execution allocates and frees many large column buffers. After a burst of
//! concurrent requests, the allocator keeps much of that freed memory resident: glibc
//! raises its mmap threshold and cannot trim a fragmented heap from the top, and mimalloc
//! purges freed pages only when later allocations run. Once no request has been active
//! for a short while, [`start_idle_release`] hands the free memory back to the OS.
//!
//! The release wakes every query worker thread, and the memory it returns has to be
//! faulted in again by the next request, so it waits for a whole quiet interval: requests
//! that arrive one after another, even with gaps between them, never meet it.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: Mimalloc = Mimalloc;

/// mimalloc, called through its plain entry points for word alignment.
///
/// `mimalloc::MiMalloc` sends every request to `mi_malloc_aligned`. In mimalloc 3 an
/// aligned request of 64 KiB or more reserves extra room to align it, so a buffer whose
/// size is a power of two lands in the next size class: a full decoded block column of
/// 256 KiB takes 320 KiB. `mi_malloc` returns blocks aligned to the 8-byte word for every
/// size, so requests aligned to 8 or less use it and the rest keep the aligned calls.
#[cfg(feature = "mimalloc")]
struct Mimalloc;

/// The alignment of every block `mi_malloc` returns.
#[cfg(feature = "mimalloc")]
const WORD: usize = 8;

#[cfg(feature = "mimalloc")]
// SAFETY: every block comes from mimalloc and goes back to it through `mi_free` or a
// reallocation. Blocks from the plain calls are aligned to `WORD`, which these calls only
// serve when the layout asks for no more.
unsafe impl std::alloc::GlobalAlloc for Mimalloc {
    #[inline]
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        use libmimalloc_sys::{mi_malloc, mi_malloc_aligned};
        if layout.align() <= WORD {
            unsafe { mi_malloc(layout.size()) as *mut u8 }
        } else {
            unsafe { mi_malloc_aligned(layout.size(), layout.align()) as *mut u8 }
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        use libmimalloc_sys::{mi_zalloc, mi_zalloc_aligned};
        if layout.align() <= WORD {
            unsafe { mi_zalloc(layout.size()) as *mut u8 }
        } else {
            unsafe { mi_zalloc_aligned(layout.size(), layout.align()) as *mut u8 }
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: std::alloc::Layout) {
        unsafe { libmimalloc_sys::mi_free(ptr as *mut std::ffi::c_void) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        use libmimalloc_sys::{mi_realloc, mi_realloc_aligned};
        let ptr = ptr as *mut std::ffi::c_void;
        if layout.align() <= WORD {
            unsafe { mi_realloc(ptr, new_size) as *mut u8 }
        } else {
            unsafe { mi_realloc_aligned(ptr, new_size, layout.align()) as *mut u8 }
        }
    }
}

static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static DIRTY: AtomicBool = AtomicBool::new(false);
/// When the last request ended, in milliseconds since [`epoch`].
static LAST_END_MS: AtomicU64 = AtomicU64::new(0);

fn epoch() -> Instant {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64
}

/// Middleware counting in-flight requests.
pub async fn track(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    ACTIVE.fetch_add(1, Ordering::SeqCst);
    DIRTY.store(true, Ordering::SeqCst);
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            LAST_END_MS.store(now_ms(), Ordering::SeqCst);
            ACTIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _done = Done;
    next.run(req).await
}

/// Release free heap memory to the OS once requests have run and then none has been
/// active for `interval`.
pub fn start_idle_release(interval: Duration) {
    if interval.is_zero() {
        return;
    }
    epoch();
    std::thread::Builder::new()
        .name("idle-release".into())
        .spawn(move || {
            let mut wait = interval;
            loop {
                std::thread::sleep(wait);
                wait = interval;
                if !DIRTY.load(Ordering::SeqCst) {
                    continue;
                }
                if ACTIVE.load(Ordering::SeqCst) != 0 {
                    continue;
                }
                let quiet = Duration::from_millis(
                    now_ms().saturating_sub(LAST_END_MS.load(Ordering::SeqCst)),
                );
                if quiet < interval {
                    // a request ended recently: look again when it has been quiet long
                    // enough
                    wait = interval - quiet;
                    continue;
                }
                if DIRTY.swap(false, Ordering::SeqCst) {
                    let t = Instant::now();
                    release();
                    tracing::debug!("released idle heap memory in {:?}", t.elapsed());
                }
            }
        })
        .expect("spawning the idle-release thread");
}

/// Return the calling thread's free heap memory to the system: what a local embedding
/// model runs on each of its threads after it drops its weights (spec F12).
#[cfg_attr(not(feature = "embed-local"), allow(dead_code))]
pub fn release_current_thread() {
    #[cfg(feature = "mimalloc")]
    // SAFETY: mi_collect only returns free memory to the OS.
    unsafe {
        libmimalloc_sys::mi_collect(true)
    };
    #[cfg(all(target_os = "linux", target_env = "gnu", not(feature = "mimalloc")))]
    // SAFETY: malloc_trim only returns free memory to the OS.
    unsafe {
        libc::malloc_trim(0);
    }
}

fn release() {
    #[cfg(feature = "mimalloc")]
    {
        // freed pages sit in per-thread heaps: collect on every query worker thread
        rayon::broadcast(|_| unsafe { libmimalloc_sys::mi_collect(true) });
        unsafe { libmimalloc_sys::mi_collect(true) };
    }
    #[cfg(all(target_os = "linux", target_env = "gnu", not(feature = "mimalloc")))]
    // SAFETY: malloc_trim only returns free memory to the OS.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(all(test, feature = "mimalloc"))]
mod tests {
    use super::Mimalloc;
    use std::alloc::{GlobalAlloc, Layout};

    fn usable(p: *mut u8) -> usize {
        // SAFETY: `p` is a live block from mimalloc.
        unsafe { libmimalloc_sys::mi_usable_size(p as *const std::ffi::c_void) }
    }

    /// A word-aligned buffer of a power-of-two size keeps its size class. Through the
    /// aligned calls, a full block column of 256 KiB took 320 KiB.
    #[test]
    fn word_aligned_powers_of_two_keep_their_size_class() {
        for size in [64 << 10, 128 << 10, 256 << 10, 1 << 20] {
            let layout = Layout::from_size_align(size, 8).unwrap();
            // SAFETY: the layout has a non-zero size, and each block is freed with the
            // layout it has then.
            unsafe {
                let p = Mimalloc.alloc(layout);
                assert_eq!(usable(p), size, "alloc of {size}");
                Mimalloc.dealloc(p, layout);
                let z = Mimalloc.alloc_zeroed(layout);
                assert_eq!(usable(z), size, "alloc_zeroed of {size}");
                assert!(std::slice::from_raw_parts(z, size).iter().all(|&b| b == 0));
                let r = Mimalloc.realloc(z, layout, size * 2);
                assert_eq!(usable(r), size * 2, "realloc to {}", size * 2);
                Mimalloc.dealloc(r, Layout::from_size_align(size * 2, 8).unwrap());
            }
        }
    }

    /// Larger alignments still go through the aligned calls, and every block is aligned
    /// as its layout asks.
    #[test]
    fn every_block_has_its_alignment() {
        for align in [1, 2, 4, 8, 16, 64, 4096] {
            for size in [1, 3, 8, 24, 100, 4096, 70_000, 300_000] {
                let layout = Layout::from_size_align(size, align).unwrap();
                let grown = Layout::from_size_align(size + 1000, align).unwrap();
                // SAFETY: the layout has a non-zero size, and each block is freed with the
                // layout it has then.
                unsafe {
                    let p = Mimalloc.alloc(layout);
                    assert_eq!(p as usize % align, 0, "alloc {size} aligned to {align}");
                    let p = Mimalloc.realloc(p, layout, grown.size());
                    assert_eq!(p as usize % align, 0, "realloc {size} aligned to {align}");
                    Mimalloc.dealloc(p, grown);
                    let z = Mimalloc.alloc_zeroed(layout);
                    assert_eq!(z as usize % align, 0, "zeroed {size} aligned to {align}");
                    Mimalloc.dealloc(z, layout);
                }
            }
        }
    }
}
