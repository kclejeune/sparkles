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
static GLOBAL: sparkles_alloc::Mimalloc = sparkles_alloc::Mimalloc;

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

/// Start the clock that request activity is measured on. The server calls it at start,
/// so that a first request after a quiet start counts the time since then.
pub fn start_clock() {
    epoch();
}

/// Whether the request calling this is the only one in flight and no other request has
/// ended for `quiet`: the server's threads have been idle, so a wake-up of another
/// thread would be slow. A request counts until its response body has ended.
pub fn idle_besides_this(quiet: Duration) -> bool {
    idle(
        ACTIVE.load(Ordering::SeqCst),
        now_ms().saturating_sub(LAST_END_MS.load(Ordering::SeqCst)),
        quiet,
    )
}

/// [`idle_besides_this`] for `active` requests in flight, including the caller, the last
/// of the others having ended `since_end_ms` ago.
fn idle(active: usize, since_end_ms: u64, quiet: Duration) -> bool {
    active <= 1 && since_end_ms >= quiet.as_millis() as u64
}

/// One request in flight, counted in `active` until it drops, when it records its end.
struct Activity {
    active: &'static AtomicUsize,
    last_end_ms: &'static AtomicU64,
}

impl Activity {
    fn start(active: &'static AtomicUsize, last_end_ms: &'static AtomicU64) -> Self {
        active.fetch_add(1, Ordering::SeqCst);
        Activity {
            active,
            last_end_ms,
        }
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        self.last_end_ms.store(now_ms(), Ordering::SeqCst);
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A response body that keeps its request counted as active until the body has been
/// sent or dropped, since a streamed answer is still being produced after its headers.
struct Counted {
    inner: axum::body::Body,
    _activity: Activity,
}

impl http_body::Body for Counted {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        std::pin::Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

/// Middleware counting in-flight requests, each until its response body ends.
pub async fn track(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let activity = Activity::start(&ACTIVE, &LAST_END_MS);
    DIRTY.store(true, Ordering::SeqCst);
    next.run(req).await.map(|inner| {
        axum::body::Body::new(Counted {
            inner,
            _activity: activity,
        })
    })
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
    sparkles_alloc::collect();
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
        rayon::broadcast(|_| sparkles_alloc::collect());
        sparkles_alloc::collect();
    }
    #[cfg(all(target_os = "linux", target_env = "gnu", not(feature = "mimalloc")))]
    // SAFETY: malloc_trim only returns free memory to the OS.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(test)]
mod activity_tests {
    use super::*;

    #[test]
    fn a_server_is_idle_for_a_first_run_only_alone_and_after_the_quiet_time() {
        let quiet = Duration::from_millis(10);
        assert!(idle(1, 10, quiet));
        assert!(idle(1, 5_000, quiet));
        assert!(!idle(1, 9, quiet), "a request ended 9 ms ago");
        assert!(!idle(2, 5_000, quiet), "another request is in flight");
    }

    #[tokio::test]
    async fn a_request_counts_until_its_body_ends() {
        use http_body_util::BodyExt;
        static ACTIVE: AtomicUsize = AtomicUsize::new(0);
        static END: AtomicU64 = AtomicU64::new(u64::MAX);
        let (tx, rx) = tokio::sync::mpsc::channel::<axum::body::Bytes>(1);
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv()
                .await
                .map(|b| (Ok::<_, std::convert::Infallible>(b), rx))
        });
        let body = Counted {
            inner: axum::body::Body::from_stream(stream),
            _activity: Activity::start(&ACTIVE, &END),
        };
        assert_eq!(ACTIVE.load(Ordering::SeqCst), 1);
        tx.send("part".into()).await.unwrap();
        let mut body = Box::pin(body);
        let frame = body.frame().await.unwrap().unwrap();
        assert_eq!(frame.into_data().unwrap(), "part");
        // the headers and a first part are out, and the body goes on
        assert_eq!(ACTIVE.load(Ordering::SeqCst), 1);
        assert_eq!(END.load(Ordering::SeqCst), u64::MAX);
        drop(tx);
        assert!(body.frame().await.is_none());
        drop(body);
        assert_eq!(ACTIVE.load(Ordering::SeqCst), 0);
        assert_ne!(END.load(Ordering::SeqCst), u64::MAX);
    }
}
