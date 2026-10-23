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
//! future that notices is the one doing the work), so only requests that are short or
//! bounded run there. Those are a query whose last run with the same text took less
//! than [`QUICK_QUERY_MS`], the first run of a query text the server has not seen when
//! the request has a timeout and the server has been idle for [`FIRST_RUN_QUIET`], and a
//! small update that only inserts or deletes data and
//! finds the writer lock free. A first run gives up cancellation on disconnect, but its
//! timeout still stops it. A text whose run was slow or failed uses the blocking pool
//! until a later run of it is quick again, and so does a text that may call a SERVICE.
//! Timeouts apply either way.
//!
//! The blocking pool costs a first request about 2 ms more on an idle machine. On forge
//! at 10.5M triples, a fresh server answered its first `path-plus` in a median of 6.5 ms
//! through the pool and 4.3 ms in place, and a text it had not seen in 0.72 and 0.50 ms.
//! A woken pool thread may have to leave a deep sleep state, may land on a slower core,
//! and starts with a cold allocator heap, while the worker that read the request is
//! already running. Under steady load the opposite held: the BSBM explore driver, whose
//! texts are all new, ran 10% to 28% slower per query with every first run in place,
//! so a first run goes in place only on an idle server.

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

/// What the server remembers of a query text, which decides where its next run goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum History {
    /// No run of the text is remembered.
    Unseen,
    /// The last run was quick.
    Quick(QuickExecution),
    /// The last run was slow, streamed past one body or failed.
    Slow,
}

/// How long the server must have been without requests for the first run of a text it
/// has not seen to run in place. A run under steady load stays on the blocking pool,
/// whose threads are then awake.
pub const FIRST_RUN_QUIET: std::time::Duration = std::time::Duration::from_millis(10);

/// Whether the server is idle enough for a first run in place ([`FIRST_RUN_QUIET`]).
/// Router tests run many servers in one process and send requests back to back, so they
/// skip this condition, and `alloc`'s tests cover it.
pub fn idle_for_first_run() -> bool {
    cfg!(test) || crate::alloc::idle_besides_this(FIRST_RUN_QUIET)
}

/// Whether the first run of `query`, a text the server has not seen, may run in place
/// under a timeout. A SERVICE call may wait on another server or run a graph algorithm,
/// whose work does not always check the deadline, so a text that may contain one,
/// including through a `\u` escape of the keyword, runs on the blocking pool.
pub fn first_run_in_place(query: &str) -> bool {
    !query.contains("\\u")
        && !query.contains("\\U")
        && !query
            .as_bytes()
            .windows(7)
            .any(|w| w.eq_ignore_ascii_case(b"service"))
}

/// How a quick query may execute on its next automatic request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickExecution {
    Eager,
    /// ASK may be quick because its native path avoids an over-budget eager product.
    PreserveAuto,
}

/// The query texts (per dataset) whose last run is remembered, quick or not.
#[derive(Default)]
pub struct QuickQueries {
    quick: Mutex<HashMap<u64, History>>,
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

    /// The execution hint of a quick text (for tests; requests read [`Self::history`]).
    #[cfg(test)]
    pub fn execution(&self, key: u64) -> Option<QuickExecution> {
        match self.history(key) {
            History::Quick(execution) => Some(execution),
            History::Unseen | History::Slow => None,
        }
    }

    /// What is remembered of the text with this key, in one snapshot. A failed
    /// concurrent request can change it, but cannot turn this hint into a different
    /// policy.
    pub fn history(&self, key: u64) -> History {
        #[cfg(test)]
        {
            *self.reads.lock().entry(key).or_default() += 1;
        }
        self.quick
            .lock()
            .get(&key)
            .copied()
            .unwrap_or(History::Unseen)
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
        if set.len() >= REMEMBERED && !set.contains_key(&key) {
            set.clear();
        }
        let seen = if ms < QUICK_QUERY_MS {
            History::Quick(execution)
        } else {
            History::Slow
        };
        set.insert(key, seen);
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
