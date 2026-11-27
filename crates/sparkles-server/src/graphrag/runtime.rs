//! What the server keeps in memory about GraphRAG builds (spec C22 §2.2, §8): the
//! confirmations builds wait for, and the figures the metrics report.

use parking_lot::{Condvar, Mutex};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

/// An attached internal F13 accumulator circuit for one dataset's GraphRAG automatic
/// refresh (spec C22 §5, F13 §3.6).
pub(crate) struct RefreshCircuit {
    pub(crate) dataset_id: uuid::Uuid,
    pub(crate) runtime: Weak<sparkles::incr::Runtime>,
    pub(crate) settings: super::settings::GraphragSettings,
    pub(crate) access_epoch: u64,
    pub(crate) spec: sparkles::incr::InternalSpec,
    pub(crate) access_fp: u64,
    pub(crate) handle: sparkles::incr::InternalCircuit,
}

impl RefreshCircuit {
    pub(crate) fn new(
        ds: &crate::state::Dataset,
        settings: super::settings::GraphragSettings,
        access_epoch: u64,
        spec: sparkles::incr::InternalSpec,
        access_fp: u64,
        handle: sparkles::incr::InternalCircuit,
    ) -> Self {
        Self {
            dataset_id: ds.store.dataset_id(),
            runtime: Arc::downgrade(&ds.dataset.inner_state().views),
            settings,
            access_epoch,
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

    pub(crate) fn matches_attached(
        &self,
        ds: &crate::state::Dataset,
        settings: &super::settings::GraphragSettings,
        access_epoch: u64,
    ) -> bool {
        self.same_store(ds) && self.access_epoch == access_epoch && self.settings == *settings
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

/// Canonical dataset UUID for `ds` (a branch shares its `main` dataset's identity).
pub(crate) fn owner_id(ds: &crate::state::Dataset) -> uuid::Uuid {
    ds.main()
        .map_or_else(|| ds.store.dataset_id(), |m| m.store.dataset_id())
}

fn is_registered_owner(st: &crate::state::AppState, name: &str, dataset_id: uuid::Uuid) -> bool {
    st.catalog
        .get(name)
        .is_some_and(|cur| cur.dataset_id() == dataset_id)
}

/// Removes a private check's cancellation handle when the check leaves preparation.
pub(crate) struct PrivateCheckGuard {
    checks: Arc<Mutex<HashMap<String, Vec<Arc<AtomicBool>>>>>,
    dataset: String,
    cancel: Arc<AtomicBool>,
}

impl Drop for PrivateCheckGuard {
    fn drop(&mut self) {
        let mut map = self.checks.lock();
        if let Some(list) = map.get_mut(&self.dataset) {
            list.retain(|c| !Arc::ptr_eq(c, &self.cancel));
            if list.is_empty() {
                map.remove(&self.dataset);
            }
        }
    }
}

/// The server's GraphRAG state in memory.
#[derive(Default)]
pub struct Runtime {
    /// builds waiting for confirmation, by task id, with their dataset
    waiting: Mutex<HashMap<String, (String, Arc<Waiting>)>>,
    /// per-dataset figures keyed by dataset UUID so renames carry figures atomically and
    /// delayed cleanup never overwrites or removes another owner's figures
    figures: Mutex<HashMap<uuid::Uuid, Figures>>,
    /// the build state and the daily ledger of datasets without a directory, keyed by
    /// dataset UUID so renames carry state atomically and deletes never cross owners
    states: Mutex<HashMap<uuid::Uuid, super::state::BuildState>>,
    days: Mutex<HashMap<uuid::Uuid, Arc<Mutex<super::budget::DayLedger>>>>,
    /// the accounting anomalies of datasets without a directory, keyed by dataset UUID
    anomalies: Mutex<HashMap<uuid::Uuid, Vec<super::budget::Anomaly>>>,
    /// one lock per dataset, so that builds and reviews never interleave their commits
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// one lock per dataset ordering refresh circuit attach, replace and detach transitions
    /// outside `circuits`, so settling or building one dataset never blocks others
    circuit_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// internal F13 accumulator circuits for automatic refresh, one per dataset
    circuits: Mutex<HashMap<String, RefreshCircuit>>,
    /// active automatic refresh private checks per dataset, cancelled if a restore reserves it
    private_checks: Arc<Mutex<HashMap<String, Vec<Arc<AtomicBool>>>>>,
    /// active automatic refresh task id per dataset, cancelled if an in-place restore begins
    auto_tasks: Mutex<HashMap<String, String>>,
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

    /// Change the figures of a dataset only while `(dataset, dataset_id)` is still the
    /// live registered instance in the catalog.
    pub fn update_if_live(
        &self,
        st: &crate::state::AppState,
        dataset: &str,
        dataset_id: uuid::Uuid,
        f: impl FnOnce(&mut Figures),
    ) {
        let mut map = self.figures.lock();
        if !is_registered_owner(st, dataset, dataset_id) {
            return;
        }
        f(map.entry(dataset_id).or_default());
    }

    /// The build state of an in-memory dataset instance.
    pub fn state(&self, dataset_id: uuid::Uuid) -> super::state::BuildState {
        self.states
            .lock()
            .get(&dataset_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Overwrite an in-memory dataset's build state directly (tests).
    #[cfg(test)]
    pub fn set_state(&self, dataset_id: uuid::Uuid, s: super::state::BuildState) {
        self.states.lock().insert(dataset_id, s);
    }

    /// Save an in-memory dataset's build state only while `ds` is still the live
    /// registered dataset instance under `ds.name`.
    pub fn set_state_if_live(
        &self,
        st: &crate::state::AppState,
        ds: &crate::state::Dataset,
        s: super::state::BuildState,
    ) {
        let id = owner_id(ds);
        let mut map = self.states.lock();
        if !is_registered_owner(st, &ds.name, id) {
            return;
        }
        map.insert(id, s);
    }

    /// A snapshot of an in-memory dataset's daily ledger without allocating an entry.
    pub fn day_snapshot(&self, dataset_id: uuid::Uuid) -> super::budget::DayLedger {
        self.days
            .lock()
            .get(&dataset_id)
            .map(|l| *l.lock())
            .unwrap_or_default()
    }

    /// The daily ledger of an in-memory dataset, stored only while `owner` is still
    /// registered in the catalog.
    pub fn day_if_live(
        &self,
        st: &crate::state::AppState,
        owner: &crate::state::Dataset,
    ) -> Arc<Mutex<super::budget::DayLedger>> {
        let id = owner.store.dataset_id();
        let mut map = self.days.lock();
        if !is_registered_owner(st, &owner.name, id) {
            return Arc::new(Mutex::new(super::budget::DayLedger::default()));
        }
        map.entry(id).or_default().clone()
    }

    /// The lock that orders a dataset's GraphRAG commits.
    pub fn lock(&self, dataset: &str) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .entry(dataset.to_string())
            .or_default()
            .clone()
    }

    /// Keep an anomaly of a dataset without a directory, and count it (§3.5), only while
    /// `ds` is still the registered instance under `ds.name`.
    pub fn record_anomaly_if_live(
        &self,
        st: &crate::state::AppState,
        ds: &crate::state::Dataset,
        a: super::budget::Anomaly,
    ) {
        let id = owner_id(ds);
        {
            let mut all = self.anomalies.lock();
            if !is_registered_owner(st, &ds.name, id) {
                return;
            }
            let list = all.entry(id).or_default();
            // the latest ones, as the dataset ledger's file keeps them
            list.push(a);
            let over = list.len().saturating_sub(super::ledger::MAX_ANOMALIES);
            list.drain(..over);
        }
        self.update_if_live(st, &ds.name, id, |f| f.anomalies += 1);
    }

    /// The anomalies of a dataset without a directory.
    pub fn anomalies(&self, dataset_id: uuid::Uuid) -> Vec<super::budget::Anomaly> {
        self.anomalies
            .lock()
            .get(&dataset_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn figures(&self, st: &crate::state::AppState) -> BTreeMap<String, Figures> {
        let by_id = self.figures.lock().clone();
        if by_id.is_empty() {
            return BTreeMap::new();
        }
        let mut out = BTreeMap::new();
        for info in st.catalog.list() {
            if let Some(f) = by_id.get(&info.id) {
                out.insert(info.name, f.clone());
            }
        }
        out
    }

    /// Register an active private check's cancel flag for `dataset` until the guard drops.
    /// Caller holds `AppState::restore_admission` so registration is ordered with restore
    /// reservations.
    pub(crate) fn register_private_check(
        &self,
        dataset: &str,
        cancel: Arc<AtomicBool>,
    ) -> PrivateCheckGuard {
        self.private_checks
            .lock()
            .entry(dataset.to_string())
            .or_default()
            .push(cancel.clone());
        PrivateCheckGuard {
            checks: self.private_checks.clone(),
            dataset: dataset.to_string(),
            cancel,
        }
    }

    /// Cancel any in-flight private checks for `dataset` when a restore reserves it.
    #[cfg_attr(not(any(test, feature = "backup")), allow(dead_code))]
    pub(crate) fn cancel_private_checks(&self, dataset: &str) {
        if let Some(list) = self.private_checks.lock().get(dataset) {
            for c in list {
                c.store(true, Ordering::Release);
            }
        }
    }

    /// Record the currently admitted automatic refresh task for `dataset`.
    pub(crate) fn set_auto_task(&self, dataset: &str, task_id: &str) {
        self.auto_tasks
            .lock()
            .insert(dataset.to_string(), task_id.to_string());
    }

    /// Clear the recorded automatic refresh task for `dataset` if it still matches `task_id`.
    pub(crate) fn clear_auto_task(&self, dataset: &str, task_id: &str) {
        let mut map = self.auto_tasks.lock();
        if map.get(dataset).is_some_and(|id| id == task_id) {
            map.remove(dataset);
        }
    }

    /// Cancel an active automatic refresh task for `dataset` when an in-place restore begins.
    #[cfg_attr(not(any(test, feature = "backup")), allow(dead_code))]
    pub(crate) fn cancel_auto_refresh(&self, st: &crate::state::AppState, dataset: &str) {
        let id = self.auto_tasks.lock().get(dataset).cloned();
        if let Some(id) = id {
            let _ = st.cancel_task(&id);
        }
    }

    /// The lock that serializes a dataset's refresh circuit lifecycle transitions.
    pub(crate) fn circuit_lock(&self, dataset: &str) -> Arc<Mutex<()>> {
        self.circuit_locks
            .lock()
            .entry(dataset.to_string())
            .or_default()
            .clone()
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

    /// Return the dataset's attached refresh circuit when it matches `(ds, spec, access_fp)`,
    /// refreshing its cached `(settings, access_epoch)` in place so non-spec changes
    /// (such as debounce or threshold tweaks) reuse the live circuit. Caller must hold
    /// [`Self::circuit_lock`].
    pub(crate) fn refresh_circuit_sync_locked(
        &self,
        dataset: &str,
        ds: &crate::state::Dataset,
        settings: &super::settings::GraphragSettings,
        access_epoch: u64,
        spec: &sparkles::incr::InternalSpec,
        access_fp: u64,
    ) -> Option<sparkles::incr::InternalCircuit> {
        let mut map = self.circuits.lock();
        let c = map.get_mut(dataset)?;
        if !c.matches(ds, spec, access_fp) {
            return None;
        }
        c.settings = settings.clone();
        c.access_epoch = access_epoch;
        Some(c.handle.clone())
    }

    /// Whether `dataset` currently holds a refresh circuit attached to `ds`.
    pub(crate) fn has_refresh_circuit(&self, dataset: &str, ds: &crate::state::Dataset) -> bool {
        self.circuits
            .lock()
            .get(dataset)
            .is_some_and(|c| c.same_store(ds))
    }

    /// Whether `dataset` currently holds a refresh circuit attached to `ds` whose
    /// settings and access epoch match `(settings, access_epoch)`.
    pub(crate) fn has_matching_circuit(
        &self,
        dataset: &str,
        ds: &crate::state::Dataset,
        settings: &super::settings::GraphragSettings,
        access_epoch: u64,
    ) -> bool {
        self.circuits
            .lock()
            .get(dataset)
            .is_some_and(|c| c.matches_attached(ds, settings, access_epoch))
    }

    /// Install a refresh circuit for `dataset`, returning any displaced circuit so the
    /// caller drops it outside the lock. Caller must hold [`Self::circuit_lock`].
    pub(crate) fn set_refresh_circuit_locked(
        &self,
        dataset: &str,
        circuit: RefreshCircuit,
    ) -> Option<RefreshCircuit> {
        self.circuits.lock().insert(dataset.to_string(), circuit)
    }

    /// Remove and return the refresh circuit of `dataset`, if any. Caller must hold
    /// [`Self::circuit_lock`].
    pub(crate) fn take_refresh_circuit_locked(&self, dataset: &str) -> Option<RefreshCircuit> {
        self.circuits.lock().remove(dataset)
    }

    /// Drop any refresh circuit attached for `dataset`, outside the map lock. Caller must
    /// hold [`Self::circuit_lock`].
    pub(crate) fn detach_circuit_locked(&self, dataset: &str) {
        let old = self.circuits.lock().remove(dataset);
        drop(old);
    }

    /// Drop the refresh circuit of `dataset` only when it belongs to `ds`, outside the
    /// map lock. Caller must hold [`Self::circuit_lock`].
    pub(crate) fn detach_circuit_for_locked(&self, dataset: &str, ds: &crate::state::Dataset) {
        let old = {
            let mut map = self.circuits.lock();
            if map.get(dataset).is_some_and(|c| c.same_store(ds)) {
                map.remove(dataset)
            } else {
                None
            }
        };
        drop(old);
    }

    /// Drop any refresh circuit attached for `dataset`, serialized per dataset.
    pub(crate) fn detach_circuit(&self, dataset: &str) {
        let lock = self.circuit_lock(dataset);
        let _held = lock.lock();
        self.detach_circuit_locked(dataset);
    }

    /// Drop the refresh circuit of `dataset` under `circuit_lock` when its owner is no
    /// longer live according to `is_live`. Releases `circuits` before invoking `is_live`
    /// and drops any removed handle outside `circuits`.
    pub(crate) fn detach_if_not_live_owner(
        &self,
        dataset: &str,
        is_live: impl Fn(&str, uuid::Uuid, &Weak<sparkles::incr::Runtime>) -> bool,
    ) {
        let lock = self.circuit_lock(dataset);
        let _held = lock.lock();
        let owner = self
            .circuits
            .lock()
            .get(dataset)
            .map(|c| (c.dataset_id, c.runtime.clone()));
        if let Some((dataset_id, runtime)) = owner
            && !is_live(dataset, dataset_id, &runtime)
        {
            let old = self.circuits.lock().remove(dataset);
            drop(old);
        }
    }

    /// Drop the refresh circuit of `dataset` only when it belongs to `dataset_id`,
    /// serialized per dataset.
    pub(crate) fn detach_circuit_for_id(&self, dataset: &str, dataset_id: uuid::Uuid) {
        let lock = self.circuit_lock(dataset);
        let _held = lock.lock();
        let old = {
            let mut map = self.circuits.lock();
            if map.get(dataset).is_some_and(|c| c.dataset_id == dataset_id) {
                map.remove(dataset)
            } else {
                None
            }
        };
        drop(old);
    }

    /// Drop the refresh circuit of `dataset` only when it belongs to `dataset_id` and
    /// settle its views runtime when it stays open (as on an in-memory dataset rename),
    /// so a re-attach under a new name does not collide with an uncollected handle and a
    /// replacement dataset created under the old name is never touched.
    pub(crate) fn detach_circuit_for_id_and_settle(&self, dataset: &str, dataset_id: uuid::Uuid) {
        let lock = self.circuit_lock(dataset);
        let _held = lock.lock();
        let old = {
            let mut map = self.circuits.lock();
            if map.get(dataset).is_some_and(|c| c.dataset_id == dataset_id) {
                map.remove(dataset)
            } else {
                None
            }
        };
        if let Some(old) = old {
            let rt = old.runtime.upgrade();
            drop(old);
            if let Some(rt) = rt
                && !rt.is_closed()
            {
                rt.settle(Duration::from_secs(5));
            }
        }
    }

    /// Detach and settle the refresh circuit of `from` belonging to `dataset_id` after a
    /// dataset rename. Build state, daily ledgers, anomalies and figures are keyed by
    /// `dataset_id` and are already visible under the destination name as soon as the
    /// catalog publishes the rename.
    pub(crate) fn rename(&self, from: &str, dataset_id: uuid::Uuid) {
        self.detach_circuit_for_id_and_settle(from, dataset_id);
    }

    /// Detach the refresh circuit of `dataset` and clear its in-memory state after
    /// deletion, scoped strictly to `dataset_id` so delayed cleanup after `leaving` drops
    /// never clears a replacement dataset created under the same name.
    pub(crate) fn forget(&self, dataset: &str, dataset_id: Option<uuid::Uuid>) {
        let Some(id) = dataset_id else {
            return;
        };
        self.detach_circuit_for_id(dataset, id);
        self.states.lock().remove(&id);
        self.days.lock().remove(&id);
        self.anomalies.lock().remove(&id);
        self.figures.lock().remove(&id);
    }

    /// Keep only refresh circuits for datasets that remain live, dropping removed handles
    /// outside the map lock. `in_snapshot` selects candidates outside per-dataset locks;
    /// `is_live` re-validates each candidate's owner under its circuit lock without
    /// holding `circuits`.
    pub(crate) fn retain_circuits(
        &self,
        in_snapshot: impl Fn(&str) -> bool,
        is_live: impl Fn(&str, uuid::Uuid, &Weak<sparkles::incr::Runtime>) -> bool,
    ) {
        let gone: Vec<String> = self
            .circuits
            .lock()
            .keys()
            .filter(|k| !in_snapshot(k))
            .cloned()
            .collect();
        for k in gone {
            self.detach_if_not_live_owner(&k, &is_live);
        }
    }
}
