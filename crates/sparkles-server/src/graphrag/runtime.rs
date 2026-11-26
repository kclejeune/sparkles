//! What the server keeps in memory about GraphRAG builds (spec C22 §2.2, §8): the
//! confirmations builds wait for, and the figures the metrics report.

use parking_lot::{Condvar, Mutex};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Weak};
use std::time::Duration;

/// An attached internal F13 accumulator circuit for one dataset's GraphRAG automatic
/// refresh (spec C22 §5, F13 §3.6).
pub(crate) struct RefreshCircuit {
    pub(crate) dataset_id: uuid::Uuid,
    pub(crate) runtime: Weak<sparkles::incr::Runtime>,
    pub(crate) spec: sparkles::incr::InternalSpec,
    pub(crate) access_fp: u64,
    pub(crate) handle: sparkles::incr::InternalCircuit,
}

impl RefreshCircuit {
    pub(crate) fn new(
        ds: &crate::state::Dataset,
        spec: sparkles::incr::InternalSpec,
        access_fp: u64,
        handle: sparkles::incr::InternalCircuit,
    ) -> Self {
        Self {
            dataset_id: ds.store.dataset_id(),
            runtime: Arc::downgrade(&ds.dataset.inner_state().views),
            spec,
            access_fp,
            handle,
        }
    }

    pub(crate) fn same_store(&self, ds: &crate::state::Dataset) -> bool {
        self.dataset_id == ds.store.dataset_id()
            && self.runtime.strong_count() > 0
            && Weak::ptr_eq(
                &self.runtime,
                &Arc::downgrade(&ds.dataset.inner_state().views),
            )
    }

    pub(crate) fn matches(
        &self,
        ds: &crate::state::Dataset,
        spec: &sparkles::incr::InternalSpec,
        access_fp: u64,
    ) -> bool {
        self.same_store(ds) && self.access_fp == access_fp && self.spec == *spec
    }
}

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
    /// the accounting anomalies of datasets without a directory
    anomalies: Mutex<HashMap<String, Vec<super::budget::Anomaly>>>,
    /// one lock per dataset, so that builds and reviews never interleave their commits
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// internal F13 accumulator circuits for automatic refresh, one per dataset
    circuits: Mutex<HashMap<String, RefreshCircuit>>,
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

    /// Whether a build of `dataset` waits for confirmation under `task`.
    pub fn is_waiting(&self, dataset: &str, task: &str) -> bool {
        self.waiting
            .lock()
            .get(task)
            .is_some_and(|(d, _)| d == dataset)
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

    /// Keep an anomaly of a dataset without a directory, and count it (§3.5).
    pub fn record_anomaly(&self, dataset: &str, a: super::budget::Anomaly) {
        let mut all = self.anomalies.lock();
        let list = all.entry(dataset.to_string()).or_default();
        // the latest ones, as the dataset ledger's file keeps them
        list.push(a);
        let over = list.len().saturating_sub(super::ledger::MAX_ANOMALIES);
        list.drain(..over);
        drop(all);
        self.update(dataset, |f| f.anomalies += 1);
    }

    /// The anomalies of a dataset without a directory.
    pub fn anomalies(&self, dataset: &str) -> Vec<super::budget::Anomaly> {
        self.anomalies
            .lock()
            .get(dataset)
            .cloned()
            .unwrap_or_default()
    }

    pub fn figures(&self) -> BTreeMap<String, Figures> {
        self.figures.lock().clone()
    }

    /// Return the dataset's attached refresh circuit when it still matches `ds`, `spec`
    /// and `access_fp`.
    pub(crate) fn refresh_circuit(
        &self,
        dataset: &str,
        ds: &crate::state::Dataset,
        spec: &sparkles::incr::InternalSpec,
        access_fp: u64,
    ) -> Option<sparkles::incr::InternalCircuit> {
        self.circuits
            .lock()
            .get(dataset)
            .filter(|c| c.matches(ds, spec, access_fp))
            .map(|c| c.handle.clone())
    }

    /// Whether `dataset` currently holds a refresh circuit attached to `ds`.
    pub(crate) fn has_refresh_circuit(&self, dataset: &str, ds: &crate::state::Dataset) -> bool {
        self.circuits
            .lock()
            .get(dataset)
            .is_some_and(|c| c.same_store(ds))
    }

    /// Install a refresh circuit for `dataset`, returning any displaced circuit so the
    /// caller drops it outside the lock.
    pub(crate) fn set_refresh_circuit(
        &self,
        dataset: &str,
        circuit: RefreshCircuit,
    ) -> Option<RefreshCircuit> {
        self.circuits.lock().insert(dataset.to_string(), circuit)
    }

    /// Remove and return the refresh circuit of `dataset`, if any.
    pub(crate) fn take_refresh_circuit(&self, dataset: &str) -> Option<RefreshCircuit> {
        self.circuits.lock().remove(dataset)
    }

    /// Drop any refresh circuit attached for `dataset`, outside the map lock.
    pub(crate) fn detach_circuit(&self, dataset: &str) {
        let old = self.circuits.lock().remove(dataset);
        drop(old);
    }

    /// Keep only refresh circuits for datasets satisfying `keep`, dropping removed
    /// handles outside the map lock.
    pub(crate) fn retain_circuits(&self, keep: impl Fn(&str) -> bool) {
        let mut removed = Vec::new();
        {
            let mut map = self.circuits.lock();
            let gone: Vec<String> = map.keys().filter(|k| !keep(k)).cloned().collect();
            for k in gone {
                if let Some(c) = map.remove(&k) {
                    removed.push(c);
                }
            }
        }
        drop(removed);
    }
}
