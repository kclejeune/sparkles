//! Short requests run on the thread that received them.
//!
//! A request handed to the blocking pool crosses threads twice (there and back), and a
//! query crosses twice more to serialize its result. On a busy machine each crossing
//! costs microseconds; on an idle one the woken thread may sit on a core in a deep sleep
//! state or at its lowest clock, and a request that computes for a fraction of a
//! millisecond can wait several milliseconds for the wake-ups. Short requests therefore
//! run where they arrived, in `block_in_place`, which hands the worker's other tasks to
//! another thread first, so nothing else waits behind them.
//!
//! A request running in place cannot be cancelled when its client disconnects (the
//! future that notices is the one doing the work), so only requests known to be short
//! run there: a query whose last run with the same text took less than
//! [`QUICK_QUERY_MS`], and a small update that only inserts or deletes data and finds
//! the writer lock free. Everything else uses the blocking pool as before. Timeouts
//! apply either way.

use parking_lot::Mutex;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

/// A query whose previous run (parse, plan, execution and serialization) took less
/// than this runs in place.
pub const QUICK_QUERY_MS: f64 = 10.0;

/// Results with up to this many cells (rows × variables) are serialized in place.
pub const QUICK_RESULT_CELLS: usize = 1 << 14;

/// Updates up to this size (bytes of text) are checked for running in place.
pub const QUICK_UPDATE_BYTES: usize = 64 << 10;

/// Most query texts remembered as quick; the set starts over when it is full.
const REMEMBERED: usize = 4096;

/// Whether this thread can run blocking work in place: a worker of a multi-threaded
/// runtime (a current-thread runtime has no other thread to hand its tasks to).
pub fn available() -> bool {
    tokio::runtime::Handle::try_current()
        .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
}

/// Requests run in place so far (for tests).
#[cfg(test)]
pub static RAN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Run `f` on this thread, after handing this worker's other tasks to another thread.
/// A panic becomes a `500` response, as it does for work on the blocking pool.
pub fn run<T>(f: impl FnOnce() -> T) -> super::ApiResult<T> {
    #[cfg(test)]
    RAN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tokio::task::block_in_place(f)
    }))
    .map_err(|p| {
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        super::err(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("the request panicked: {msg}"),
        )
    })
}

/// The query texts (per dataset) whose last run was quick.
#[derive(Default)]
pub struct QuickQueries(Mutex<HashSet<u64>>);

impl QuickQueries {
    pub fn key(dataset: &str, query: &str) -> u64 {
        let mut h = std::hash::DefaultHasher::new();
        dataset.hash(&mut h);
        query.hash(&mut h);
        h.finish()
    }

    pub fn is_quick(&self, key: u64) -> bool {
        self.0.lock().contains(&key)
    }

    /// Record how long a run of the query took, in milliseconds of work.
    pub fn record(&self, key: u64, ms: f64) {
        let mut set = self.0.lock();
        if ms < QUICK_QUERY_MS {
            if set.len() >= REMEMBERED && !set.contains(&key) {
                set.clear();
            }
            set.insert(key);
        } else {
            set.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_quick_queries_per_dataset() {
        let q = QuickQueries::default();
        let a = QuickQueries::key("a", "ASK {}");
        let b = QuickQueries::key("b", "ASK {}");
        assert_ne!(a, b);
        assert!(!q.is_quick(a));
        q.record(a, 0.5);
        assert!(q.is_quick(a));
        assert!(!q.is_quick(b));
        // a slow run makes it run on the blocking pool again
        q.record(a, QUICK_QUERY_MS * 2.0);
        assert!(!q.is_quick(a));
    }

    #[test]
    fn starts_over_when_full() {
        let q = QuickQueries::default();
        for i in 0..REMEMBERED as u64 {
            q.record(i, 0.1);
        }
        assert!(q.is_quick(0));
        q.record(u64::MAX, 0.1);
        assert!(!q.is_quick(0));
        assert!(q.is_quick(u64::MAX));
    }

    #[test]
    fn a_panic_in_place_becomes_an_error() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .unwrap();
        let r = rt.block_on(async {
            tokio::spawn(async { run(|| -> u32 { panic!("boom") }) })
                .await
                .unwrap()
        });
        let e = r.expect_err("an error");
        assert_eq!(e.0, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert!(e.1["error"].as_str().unwrap().contains("boom"));
        // the runtime keeps working
        assert_eq!(
            rt.block_on(async { tokio::spawn(async { 1 }).await.unwrap() }),
            1
        );
    }

    #[test]
    fn not_available_outside_a_multi_threaded_runtime() {
        assert!(!available());
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert!(!rt.block_on(async { available() }));
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .unwrap();
        assert!(rt.block_on(async { tokio::spawn(async { available() }).await.unwrap() }));
    }
}
