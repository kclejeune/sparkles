//! What the server keeps in memory about GraphRAG builds (spec C22 §2.2, §8): the
//! confirmations builds wait for, and the figures the metrics report.

use parking_lot::{Condvar, Mutex};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

/// A build waiting for a person to confirm its estimate.
#[derive(Default)]
pub struct Waiting {
    /// `Some(true)` confirmed, `Some(false)` refused
    answer: Mutex<Option<bool>>,
    cv: Condvar,
}

impl Waiting {
    /// Wait up to `timeout` for an answer, or until `cancelled` returns true. `false`
    /// unless confirmed.
    pub fn wait(&self, timeout: Duration, cancelled: &dyn Fn() -> bool) -> bool {
        let end = std::time::Instant::now() + timeout;
        let mut a = self.answer.lock();
        loop {
            if let Some(v) = *a {
                return v;
            }
            if cancelled() || std::time::Instant::now() >= end {
                return false;
            }
            // wake regularly to see a cancellation
            self.cv.wait_for(&mut a, Duration::from_millis(200));
        }
    }

    fn answer(&self, v: bool) {
        *self.answer.lock() = Some(v);
        self.cv.notify_all();
    }
}

/// The figures of one dataset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Figures {
    /// reports by status, as of the last build or review
    pub reports: BTreeMap<&'static str, u64>,
    /// tokens used by phase (`build`, `refresh`)
    pub tokens: BTreeMap<&'static str, u64>,
    pub stale: u64,
    pub anomalies: u64,
}

/// The server's GraphRAG state in memory.
#[derive(Default)]
pub struct Runtime {
    /// builds waiting for confirmation, by task id, with their dataset
    waiting: Mutex<HashMap<String, (String, Arc<Waiting>)>>,
    figures: Mutex<BTreeMap<String, Figures>>,
    /// the build state and the daily ledger of datasets without a directory
    states: Mutex<HashMap<String, super::state::BuildState>>,
    days: Mutex<HashMap<String, Arc<Mutex<super::budget::DayLedger>>>>,
    /// one lock per dataset, so that builds and reviews never interleave their commits
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl Runtime {
    /// Register a build that waits for confirmation.
    pub fn wait_for(&self, task: &str, dataset: &str) -> Arc<Waiting> {
        let w = Arc::new(Waiting::default());
        self.waiting
            .lock()
            .insert(task.to_string(), (dataset.to_string(), w.clone()));
        w
    }

    /// Forget a build's confirmation.
    pub fn done_waiting(&self, task: &str) {
        self.waiting.lock().remove(task);
    }

    /// Confirm or refuse the waiting build `task` of `dataset`. `false` when no build
    /// of that dataset waits under that id.
    pub fn answer(&self, dataset: &str, task: &str, confirm: bool) -> bool {
        let w = self.waiting.lock();
        match w.get(task) {
            Some((d, waiting)) if d == dataset => {
                waiting.answer(confirm);
                true
            }
            _ => false,
        }
    }

    /// Change the figures of a dataset.
    pub fn update(&self, dataset: &str, f: impl FnOnce(&mut Figures)) {
        f(self.figures.lock().entry(dataset.to_string()).or_default());
    }

    /// The build state of an in-memory dataset.
    pub fn state(&self, dataset: &str) -> super::state::BuildState {
        self.states.lock().get(dataset).cloned().unwrap_or_default()
    }

    pub fn set_state(&self, dataset: &str, s: super::state::BuildState) {
        self.states.lock().insert(dataset.to_string(), s);
    }

    /// The daily ledger of an in-memory dataset.
    pub fn day(&self, dataset: &str) -> Arc<Mutex<super::budget::DayLedger>> {
        self.days
            .lock()
            .entry(dataset.to_string())
            .or_default()
            .clone()
    }

    /// The lock that orders a dataset's GraphRAG commits.
    pub fn lock(&self, dataset: &str) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .entry(dataset.to_string())
            .or_default()
            .clone()
    }

    pub fn figures(&self) -> BTreeMap<String, Figures> {
        self.figures.lock().clone()
    }
}
