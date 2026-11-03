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
//! Automatic execution may stream a query as a cursor that eager execution answers
//! faster, since eager execution spreads more of its work over the query pool. A quick run
//! of either kind makes the next run eager and a slow run sends the next one to the
//! blocking pool with automatic execution, so without more history such a text settles
//! into whichever mode its first quick or slow run leaves it in. On forge, one server process answered
//! DBpedia's `births-by-decade` in 3 ms eagerly for its whole life and the next in 15 ms as
//! a cursor (roadmap item 449), and a text whose eager runs were slow but whose cursor runs
//! were quick alternated between them. So the history keeps the fastest eager run and the
//! fastest whole cursor run of each text, and a text runs in the faster of the two modes
//! once both are known. A cursor run that answered whole in [`QUICK_QUERY_MS`] to
//! [`EAGER_TRIAL_MS`] has its next run tried eagerly on the blocking pool when no eager run
//! of the text is known. The fastest runs, not the last ones, decide, so that one slow run
//! of the faster mode, as a busy moment gives, does not hold the text in the slower mode.
//!
//! The first run of a text in each mode is cold, since it meets that mode's code and data
//! for the first time. On atlas the first cursor run of the synthetic `expr-bind-group`
//! took 0.94 to 1.16 ms of server time and the later ones 0.58 ms, and the first eager
//! run 1.07 to 1.18 ms and the later ones 0.77 ms. Comparing the cold cursor run with the
//! eager run after it left about one server process in two answering that query eagerly
//! for its whole life. A text whose two modes are both quick therefore settles only once
//! the slower mode has run twice, and until then its next run goes to the mode that lost
//! with a single run. That costs a text at most two runs in its slower mode.
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

/// A cursor run that answered whole in less than this, but not in [`QUICK_QUERY_MS`],
/// has the next run of its text tried eagerly.
pub const EAGER_TRIAL_MS: f64 = 50.0;

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
        tokio::task::block_in_place(|| {
            crate::cpus::place_request();
            f()
        })
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
    /// The last run was an automatic cursor that answered whole but not quickly: the next
    /// run is eager, on the blocking pool.
    TryEager,
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

/// The kind of a run that [`QuickQueries`] records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Run {
    Eager,
    /// an eager trial that outgrew its memory budget
    EagerFailed,
    Ask,
    Cursor,
}

/// How a quick query may execute on its next automatic request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickExecution {
    Eager,
    /// ASK may be quick because its native path avoids an over-budget eager product.
    PreserveAuto,
}

/// What is remembered of one text: its [`History`], its fastest eager run and its fastest
/// whole cursor run in milliseconds (infinite: none yet), how many timed runs of each
/// kind it has had, up to two, and the token of the request that holds its eager trial
/// (0: none).
#[derive(Clone, Copy, Debug)]
struct Seen {
    history: History,
    eager_best: f64,
    cursor_best: f64,
    eager_runs: u8,
    cursor_runs: u8,
    trial: u64,
}

/// The runs of each mode that a text whose two modes are both quick needs before it
/// settles in the faster one, so that a cold first run does not decide it alone.
const SETTLING_RUNS: u8 = 2;

/// The query texts (per dataset) whose last run is remembered, quick or not.
#[derive(Default)]
pub struct QuickQueries {
    quick: Mutex<HashMap<u64, Seen>>,
    /// the last token that [`Self::claim_trial`] gave out
    trials: std::sync::atomic::AtomicU64,
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
            History::Unseen | History::Slow | History::TryEager => None,
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
            .map_or(History::Unseen, |seen| seen.history)
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

    /// Claim the eager trial of a text whose history is [`History::TryEager`]. One caller
    /// gets a token, which holds the trial until it passes the token to
    /// [`Self::end_trial`], and requests that overlap the trial keep automatic execution,
    /// whatever their own runs record.
    pub fn claim_trial(&self, key: u64) -> Option<u64> {
        let mut set = self.quick.lock();
        match set.get_mut(&key) {
            Some(seen) if seen.history == History::TryEager && seen.trial == 0 => {
                let token = self
                    .trials
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1;
                seen.trial = token;
                Some(token)
            }
            _ => None,
        }
    }

    /// Release the eager trial that [`Self::claim_trial`] gave out as `token`. A trial
    /// claimed since by another request, after the text was forgotten and seen again,
    /// stays held.
    pub fn end_trial(&self, key: u64, token: u64) {
        if let Some(seen) = self.quick.lock().get_mut(&key)
            && seen.trial == token
        {
            seen.trial = 0;
        }
    }

    /// Record how long an eager run of the query took, in milliseconds of work, or
    /// infinity for a run of either kind that failed or streamed past one body.
    pub fn record(&self, key: u64, ms: f64) {
        self.record_run(key, ms, Run::Eager);
    }

    /// A quick ASK can run in place, but still needs its automatic admission check.
    /// Its eager execution may exceed a lower budget on a later request.
    pub fn record_ask(&self, key: u64, ms: f64) {
        self.record_run(key, ms, Run::Ask);
    }

    /// Record how long a run of the query as an automatic cursor took that answered
    /// whole.
    pub fn record_cursor(&self, key: u64, ms: f64) {
        self.record_run(key, ms, Run::Cursor);
    }

    /// Record that an eager run of the query outgrew its memory budget, so that later runs
    /// keep automatic execution whatever eager runs took before.
    pub fn record_eager_failure(&self, key: u64) {
        self.record_run(key, f64::MAX, Run::EagerFailed);
    }

    fn record_run(&self, key: u64, ms: f64, run: Run) {
        #[cfg(test)]
        let ms = if run != Run::EagerFailed && ms.is_finite() && self.timeless.lock().contains(&key)
        {
            0.0
        } else {
            ms
        };
        let mut set = self.quick.lock();
        if set.len() >= REMEMBERED && !set.contains_key(&key) {
            set.clear();
        }
        let seen = set.get(&key).copied();
        let mut eager_best = seen.map_or(f64::INFINITY, |seen| seen.eager_best);
        let mut cursor_best = seen.map_or(f64::INFINITY, |seen| seen.cursor_best);
        let mut eager_runs = seen.map_or(0, |seen| seen.eager_runs);
        let mut cursor_runs = seen.map_or(0, |seen| seen.cursor_runs);
        let trial = seen.map_or(0, |seen| seen.trial);
        if ms.is_finite() {
            match run {
                Run::Eager => eager_runs = (eager_runs + 1).min(SETTLING_RUNS),
                Run::Cursor => cursor_runs = (cursor_runs + 1).min(SETTLING_RUNS),
                Run::Ask | Run::EagerFailed => {}
            }
        }
        let mut history = match run {
            _ if !ms.is_finite() => History::Slow,
            Run::Ask if ms < QUICK_QUERY_MS => History::Quick(QuickExecution::PreserveAuto),
            Run::Ask => History::Slow,
            Run::EagerFailed => {
                eager_best = ms;
                History::Slow
            }
            Run::Eager => {
                eager_best = eager_best.min(ms);
                if cursor_best < eager_best {
                    // the cursor is the faster mode
                    if cursor_best < QUICK_QUERY_MS {
                        History::Quick(QuickExecution::PreserveAuto)
                    } else {
                        History::Slow
                    }
                } else if ms < QUICK_QUERY_MS {
                    History::Quick(QuickExecution::Eager)
                } else if cursor_best.is_finite() {
                    // eager measured faster than the cursor, though not quick: eager on
                    // the pool
                    History::TryEager
                } else {
                    History::Slow
                }
            }
            Run::Cursor => {
                cursor_best = cursor_best.min(ms);
                if eager_best < cursor_best {
                    if eager_best < QUICK_QUERY_MS {
                        History::Quick(QuickExecution::Eager)
                    } else {
                        History::TryEager
                    }
                } else if eager_best.is_finite() {
                    // the cursor is the faster mode
                    if ms < QUICK_QUERY_MS {
                        History::Quick(QuickExecution::PreserveAuto)
                    } else {
                        History::Slow
                    }
                } else if ms < QUICK_QUERY_MS {
                    History::Quick(QuickExecution::Eager)
                } else if ms < EAGER_TRIAL_MS {
                    History::TryEager
                } else {
                    History::Slow
                }
            }
        };
        // A quick run of a text whose two modes are both quick goes on to the mode that
        // lost with fewer than SETTLING_RUNS runs, which a cold first run gives, and
        // otherwise to the faster mode as above.
        if matches!(run, Run::Eager | Run::Cursor)
            && ms < QUICK_QUERY_MS
            && eager_best < QUICK_QUERY_MS
            && cursor_best < QUICK_QUERY_MS
        {
            let cursor_faster = match run {
                Run::Eager => cursor_best < eager_best,
                _ => eager_best >= cursor_best,
            };
            history = if cursor_faster && eager_runs < SETTLING_RUNS {
                History::Quick(QuickExecution::Eager)
            } else if !cursor_faster && cursor_runs < SETTLING_RUNS {
                History::Quick(QuickExecution::PreserveAuto)
            } else {
                history
            };
        }
        set.insert(
            key,
            Seen {
                history,
                eager_best,
                cursor_best,
                eager_runs,
                cursor_runs,
                trial,
            },
        );
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

    /// A text that automatic execution streams settles in the faster mode whichever
    /// run comes first (item 449).
    #[test]
    fn a_text_runs_in_its_faster_mode() {
        let q = QuickQueries::default();
        // births-by-decade: a 15 ms cursor, a 3 ms eager run
        let a = 1;
        q.record_cursor(a, 15.0);
        assert_eq!(q.history(a), History::TryEager);
        q.record(a, 3.0);
        assert_eq!(q.history(a), History::Quick(QuickExecution::Eager));
        // one slow eager run, as a busy moment gives, keeps the text eager
        q.record(a, 10.4);
        assert_eq!(q.history(a), History::TryEager);
        q.record_cursor(a, 9.9);
        assert_eq!(q.history(a), History::Quick(QuickExecution::Eager));
        q.record(a, 3.0);
        assert_eq!(q.history(a), History::Quick(QuickExecution::Eager));
        // a quick cursor and a slow eager run: the cursor stays, in place
        let b = 2;
        q.record_cursor(b, 6.0);
        assert_eq!(q.history(b), History::Quick(QuickExecution::Eager));
        q.record(b, 14.0);
        assert_eq!(q.history(b), History::Quick(QuickExecution::PreserveAuto));
        q.record_cursor(b, 6.0);
        assert_eq!(q.history(b), History::Quick(QuickExecution::PreserveAuto));
        q.record_cursor(b, 9.0);
        assert_eq!(q.history(b), History::Quick(QuickExecution::PreserveAuto));
        // both slow: the cursor is tried eagerly once, then stays a cursor on the pool
        let c = 3;
        q.record_cursor(c, 20.0);
        assert_eq!(q.history(c), History::TryEager);
        q.record(c, 30.0);
        assert_eq!(q.history(c), History::Slow);
        q.record_cursor(c, 20.0);
        assert_eq!(q.history(c), History::Slow);
        // a slow eager run with no cursor measured leaves automatic execution
        let h = 8;
        q.record(h, 30.0);
        assert_eq!(q.history(h), History::Slow);
        // a quick eager run that a faster cursor beat keeps automatic execution
        let i = 9;
        q.record_cursor(i, 40.0);
        q.record(i, 20.0);
        assert_eq!(q.history(i), History::TryEager);
        q.record_cursor(i, 4.0);
        assert_eq!(q.history(i), History::Quick(QuickExecution::PreserveAuto));
        q.record(i, 8.0);
        assert_eq!(q.history(i), History::Quick(QuickExecution::PreserveAuto));
        // eager faster but not quick: the next runs stay eager, on the pool
        let f = 6;
        q.record_cursor(f, 40.0);
        q.record(f, 20.0);
        assert_eq!(q.history(f), History::TryEager);
        q.record_cursor(f, 45.0);
        assert_eq!(q.history(f), History::TryEager);
        // a long cursor run is not tried eagerly, and a failure never is
        let d = 4;
        q.record_cursor(d, EAGER_TRIAL_MS);
        assert_eq!(q.history(d), History::Slow);
        q.record_cursor(d, f64::INFINITY);
        assert_eq!(q.history(d), History::Slow);
        // an eager run that outgrew its budget keeps the cursor, whatever came before
        let g = 7;
        q.record_cursor(g, 15.0);
        q.record(g, 3.0);
        q.record_eager_failure(g);
        assert_eq!(q.history(g), History::Slow);
        q.record_cursor(g, 15.0);
        assert_eq!(q.history(g), History::Slow);
        q.record_cursor(g, 5.0);
        assert_eq!(q.history(g), History::Quick(QuickExecution::PreserveAuto));
        let e = 5;
        q.record_cursor(e, 15.0);
        q.record(e, f64::INFINITY);
        assert_eq!(q.history(e), History::Slow);
        q.record_cursor(e, 15.0);
        assert_eq!(
            q.history(e),
            History::TryEager,
            "a failure says nothing of eager time"
        );
        // one request holds the trial until it ends it, even when an overlapping cursor
        // run records another slow cursor
        let a = q.claim_trial(e).unwrap();
        assert_eq!(q.claim_trial(e), None);
        q.record_cursor(e, 15.0);
        assert_eq!(q.history(e), History::TryEager);
        assert_eq!(
            q.claim_trial(e),
            None,
            "an overlapping cursor run starts no second trial"
        );
        q.end_trial(e, a);
        let b = q.claim_trial(e).unwrap();
        assert_ne!(a, b);
        q.record(e, 3.0);
        q.end_trial(e, b);
        assert_eq!(q.history(e), History::Quick(QuickExecution::Eager));
        assert_eq!(q.claim_trial(e), None);
        assert_eq!(q.claim_trial(99), None, "an unseen text has no trial");
        q.end_trial(99, a);
        assert_eq!(q.history(99), History::Unseen);
    }

    /// A text whose two modes are both quick settles only once the slower mode has run
    /// twice, so that the cold first run of a mode does not decide it alone. The times
    /// are those of `expr-bind-group` on atlas, whose first cursor run took about as long
    /// as the first eager run and whose later cursor runs were the fastest.
    #[test]
    fn a_cold_first_run_does_not_settle_a_quick_text() {
        let q = QuickQueries::default();
        let a = 1;
        // the cold first cursor run, then the first eager run, slightly faster
        q.record_cursor(a, 1.12);
        assert_eq!(q.history(a), History::Quick(QuickExecution::Eager));
        q.record(a, 1.10);
        assert_eq!(
            q.history(a),
            History::Quick(QuickExecution::PreserveAuto),
            "the cursor, which lost with one run, runs again"
        );
        q.record_cursor(a, 0.58);
        assert_eq!(
            q.history(a),
            History::Quick(QuickExecution::Eager),
            "eager, which now lost with one run, runs again"
        );
        q.record(a, 0.77);
        assert_eq!(q.history(a), History::Quick(QuickExecution::PreserveAuto));
        // settled: a slower cursor run or a slow eager run changes nothing
        q.record_cursor(a, 0.9);
        assert_eq!(q.history(a), History::Quick(QuickExecution::PreserveAuto));
        q.record(a, 0.8);
        assert_eq!(q.history(a), History::Quick(QuickExecution::PreserveAuto));
        // a text whose eager runs are faster settles eagerly after a second cursor run
        let b = 2;
        q.record_cursor(b, 1.0);
        q.record(b, 0.5);
        assert_eq!(q.history(b), History::Quick(QuickExecution::PreserveAuto));
        q.record_cursor(b, 0.9);
        assert_eq!(q.history(b), History::Quick(QuickExecution::Eager));
        q.record(b, 0.45);
        assert_eq!(q.history(b), History::Quick(QuickExecution::Eager));
        // a slow mode needs no second run: the earlier rules decide
        let c = 3;
        q.record_cursor(c, 1.0);
        q.record(c, QUICK_QUERY_MS * 2.0);
        assert_eq!(q.history(c), History::Quick(QuickExecution::PreserveAuto));
        // and after a run that is not quick the earlier rules decide too
        let d = 4;
        q.record_cursor(d, 1.0);
        q.record(d, 0.5);
        q.record(d, QUICK_QUERY_MS * 2.0);
        assert_eq!(q.history(d), History::TryEager);
        q.record_cursor(d, QUICK_QUERY_MS * 2.0);
        assert_eq!(q.history(d), History::Quick(QuickExecution::Eager));
        // failed runs are not counted
        let e = 5;
        q.record_cursor(e, 1.0);
        q.record(e, f64::INFINITY);
        q.record(e, 0.5);
        assert_eq!(q.history(e), History::Quick(QuickExecution::PreserveAuto));
    }

    /// A trial whose text was forgotten and claimed again by another request stays held
    /// when the first request ends.
    #[test]
    fn ending_a_forgotten_trial_keeps_a_newer_claim() {
        let q = QuickQueries::default();
        q.record_cursor(1, 15.0);
        let a = q.claim_trial(1).unwrap();
        for k in 2..=REMEMBERED as u64 + 1 {
            q.record(k, 1.0);
        }
        assert_eq!(q.history(1), History::Unseen, "the text was forgotten");
        q.record_cursor(1, 15.0);
        let b = q.claim_trial(1).unwrap();
        q.end_trial(1, a);
        assert_eq!(q.claim_trial(1), None, "the newer claim stays held");
        q.end_trial(1, b);
        assert!(q.claim_trial(1).is_some());
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
