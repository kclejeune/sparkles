//! Global allocator selection and release of retained heap memory when the server is idle.
//!
//! Query execution allocates and frees many large column buffers. After a burst of
//! concurrent requests, the allocator keeps much of that freed memory resident: glibc
//! raises its mmap threshold and cannot trim a fragmented heap from the top, and mimalloc
//! purges freed pages only when later allocations run. Once no request has been active
//! for a short while, [`start_idle_release`] hands the free memory back to the OS.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static DIRTY: AtomicBool = AtomicBool::new(false);

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
            ACTIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _done = Done;
    next.run(req).await
}

/// Check every `interval` whether requests ran since the last release and none is active
/// now; if so, release free heap memory to the OS.
pub fn start_idle_release(interval: Duration) {
    if interval.is_zero() {
        return;
    }
    std::thread::Builder::new()
        .name("idle-release".into())
        .spawn(move || {
            loop {
                std::thread::sleep(interval);
                if ACTIVE.load(Ordering::SeqCst) == 0 && DIRTY.swap(false, Ordering::SeqCst) {
                    let t = std::time::Instant::now();
                    release();
                    tracing::debug!("released idle heap memory in {:?}", t.elapsed());
                }
            }
        })
        .expect("spawning the idle-release thread");
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
