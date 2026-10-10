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
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
