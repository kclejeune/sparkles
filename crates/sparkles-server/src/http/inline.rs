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
use std::collections::HashMap;
#[cfg(test)]
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
/// runtime (a current-thread runtime has no other thread to hand its tasks to). Setting
/// `SPARKLES_NO_IN_PLACE` in the server's environment sends every request to the blocking
/// pool, for comparing the two paths.
pub fn available() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    !*OFF.get_or_init(|| std::env::var_os("SPARKLES_NO_IN_PLACE").is_some())
        && tokio::runtime::Handle::try_current()
            .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
}

/// Requests run in place so far (for tests).
#[cfg(test)]
pub static RAN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Runs in place so far for each query key (for tests), so that a test counts the runs of
/// its own text while other tests run in parallel.
#[cfg(test)]
pub static RAN_BY_KEY: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<u64, u64>>,
> = std::sync::LazyLock::new(Default::default);

/// Count one run in place of the query with this key (a test build only).
pub fn note_in_place(key: u64) {
    #[cfg(test)]
    {
        *RAN_BY_KEY.lock().entry(key).or_default() += 1;
    }
    #[cfg(not(test))]
    let _ = key;
}

/// How many times the query with this key has run in place.
#[cfg(test)]
pub fn ran_by_key(key: u64) -> u64 {
    RAN_BY_KEY.lock().get(&key).copied().unwrap_or(0)
}

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

/// How a quick query may execute on its next automatic request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickExecution {
    Eager,
    /// ASK may be quick because its native path avoids an over-budget eager product.
    PreserveAuto,
}

/// The query texts (per dataset) whose last run was quick.
#[derive(Default)]
pub struct QuickQueries {
    quick: Mutex<HashMap<u64, QuickExecution>>,
    /// test hook: queries whose runs count as quick however long they took
    #[cfg(test)]
    timeless: Mutex<HashSet<u64>>,
    /// Test-only history reads, separated by key for parallel router tests.
    #[cfg(test)]
    reads: Mutex<HashMap<u64, u64>>,
}

impl QuickQueries {
    pub fn key(dataset: &str, query: &str) -> u64 {
        let mut h = std::hash::DefaultHasher::new();
        dataset.hash(&mut h);
        query.hash(&mut h);
        h.finish()
    }

    /// Read the scheduling and execution hint in one snapshot. A failed concurrent
    /// request can remove it, but cannot turn this hint into a different policy.
    pub fn execution(&self, key: u64) -> Option<QuickExecution> {
        #[cfg(test)]
        {
            *self.reads.lock().entry(key).or_default() += 1;
        }
        self.quick.lock().get(&key).copied()
    }

    #[cfg(test)]
    pub fn reads_by_key(&self, key: u64) -> u64 {
        self.reads.lock().get(&key).copied().unwrap_or(0)
    }

    #[cfg(test)]
    pub fn is_quick(&self, key: u64) -> bool {
        self.execution(key).is_some()
    }

    /// Test hook: a run of the query with this key that ends with a whole response
    /// counts as quick, however long it took. On a loaded machine a run that is quick
    /// on its own can take longer than [`QUICK_QUERY_MS`]. A failed or streamed run
    /// still makes the query not quick.
    #[cfg(test)]
    pub fn ignore_time(&self, key: u64) {
        self.timeless.lock().insert(key);
    }

    /// Record how long a run of the query took, in milliseconds of work.
    pub fn record(&self, key: u64, ms: f64) {
        self.record_execution(key, ms, QuickExecution::Eager);
    }

    /// A quick ASK can run in place, but still needs its automatic admission check.
    /// Its eager execution may exceed a lower budget on a later request.
    pub fn record_ask(&self, key: u64, ms: f64) {
        self.record_execution(key, ms, QuickExecution::PreserveAuto);
    }

    fn record_execution(&self, key: u64, ms: f64, execution: QuickExecution) {
        #[cfg(test)]
        let ms = if ms.is_finite() && self.timeless.lock().contains(&key) {
            0.0
        } else {
            ms
        };
        let mut set = self.quick.lock();
        if ms < QUICK_QUERY_MS {
            if set.len() >= REMEMBERED && !set.contains_key(&key) {
                set.clear();
            }
            set.insert(key, execution);
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
        // with the test hook, a slow run counts as quick, a failed one does not
        q.ignore_time(a);
        q.record(a, QUICK_QUERY_MS * 2.0);
        assert!(q.is_quick(a));
        q.record(a, f64::INFINITY);
        assert!(!q.is_quick(a));
    }

    #[test]
    fn quick_ask_preserves_auto_until_failure_or_eviction() {
        let q = QuickQueries::default();
        let key = u64::MAX;
        q.record(key, 0.1);
        assert_eq!(q.execution(key), Some(QuickExecution::Eager));
        q.record_ask(key, 0.1);
        assert_eq!(q.execution(key), Some(QuickExecution::PreserveAuto));
        q.record(key, f64::INFINITY);
        assert_eq!(q.execution(key), None);
        q.record_ask(key, 0.1);
        for i in 0..REMEMBERED as u64 {
            q.record(i, 0.1);
        }
        assert_eq!(q.execution(key), None);
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
