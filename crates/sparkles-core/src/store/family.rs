//! Runtime ownership and admission shared by one opening and all its branches.
//!
//! The allocation is the identity; a dataset UUID cannot prove that two stores
//! belong to the same opening. It owns no Store or BranchSet, so retaining the
//! root process lock through detached members cannot create an ownership cycle.
//! Future admission providers must reuse this gate. Exclusive close admission refuses active writers and pending publication.

use crate::guard::WriteOptions;
use crate::{Error, Result};
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// A single published version of a family's managed-graph registry, pairing its monotonic
/// publication generation with the installed state so readers observe both coherently.
pub(crate) struct ManagedVersion {
    pub(crate) generation: u64,
    pub(crate) state: Option<Arc<crate::managed::ManagedState>>,
}

/// Lock-free reader / serialized writer slot for a family's versioned managed-graph registry.
/// Keeping the inner `ArcSwap` private guarantees every publication advances `generation`.
pub(crate) struct ManagedSlot {
    inner: arc_swap::ArcSwap<ManagedVersion>,
    write_lock: Mutex<u64>,
}

impl ManagedSlot {
    pub(crate) fn empty() -> Self {
        Self {
            inner: arc_swap::ArcSwap::from_pointee(ManagedVersion {
                generation: 0,
                state: None,
            }),
            write_lock: Mutex::new(0),
        }
    }

    pub(crate) fn store(&self, state: Option<Arc<crate::managed::ManagedState>>) {
        let mut generation = self.write_lock.lock();
        *generation = generation
            .checked_add(1)
            .expect("managed generation overflow");
        self.inner.store(Arc::new(ManagedVersion {
            generation: *generation,
            state,
        }));
    }

    pub(crate) fn load(&self) -> arc_swap::Guard<Arc<ManagedVersion>> {
        self.inner.load()
    }

    pub(crate) fn load_version(&self) -> Arc<ManagedVersion> {
        self.inner.load_full()
    }

    pub(crate) fn load_full(&self) -> Option<Arc<crate::managed::ManagedState>> {
        self.inner.load().state.clone()
    }

    pub(crate) fn generation(&self) -> u64 {
        self.inner.load().generation
    }
}

/// Retained last by each store and branch set, after their owned resources.
pub(crate) struct Family {
    pub(crate) gate: Arc<FamilyGate>,
    /// The managed-graph registry every member admits its commits against (see
    /// [`crate::managed`]). One registry serves main and every branch, including those
    /// created before it was enabled, so they cannot diverge. Shared with snapshots via
    /// `Arc` without retaining `Family` or its root process lock.
    pub(crate) managed: Arc<ManagedSlot>,
    /// Serializes managed-registry changes end-to-end (including their post-install
    /// `managed_barrier`) so a subsequent registry change never inspects branches while
    /// a writer admitted under a superseded registry is still in flight.
    pub(crate) managed_change: Mutex<()>,
    /// The durable source holds of main and every branch (see [`crate::holds`]).
    pub(crate) holds: crate::holds::FamilyHolds,
    /// Selection made under this opening's actual root process exclusion. This
    /// scalar is not a proof of an imported physical base or WAL interval.
    origin: OnceLock<Option<crate::evidence::Sha256Digest>>,
    origin_format: OnceLock<Option<u32>>,
    _owner_lock: Option<Arc<super::ProcessLock>>,
}

impl Family {
    pub(crate) fn new(owner_lock: Option<Arc<super::ProcessLock>>) -> Arc<Self> {
        let origin = OnceLock::new();
        let origin_format = OnceLock::new();
        if owner_lock.is_none() {
            // Memory families have no imported on-disk namespace.
            origin.set(None).expect("new memory family selection");
            origin_format
                .set(None)
                .expect("new memory family format selection");
        }
        Arc::new(Self {
            gate: Arc::new(FamilyGate::default()),
            managed: Arc::new(ManagedSlot::empty()),
            managed_change: Mutex::new(()),
            holds: Default::default(),
            origin,
            origin_format,
            _owner_lock: owner_lock,
        })
    }

    pub(crate) fn store_managed(&self, state: Option<Arc<crate::managed::ManagedState>>) {
        self.managed.store(state);
    }

    pub(crate) fn managed_generation(&self) -> u64 {
        self.managed.generation()
    }

    pub(super) fn select_opening_origin(
        &self,
        root: &std::path::Path,
        lock: &Arc<super::ProcessLock>,
        preparation: &crate::access::Preparation,
    ) -> Result<()> {
        preparation.check()?;
        if !self
            ._owner_lock
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(owner, lock))
            || self.origin.get().is_some()
        {
            return Err(Error::invalid(
                "origin selection needs the genuine unopened family",
            ));
        }
        super::verify_database_lock(root, lock)?;
        let selected = super::configuration_history::opening_origin_info(root, preparation)?;
        super::verify_database_lock(root, lock)?;
        preparation.check()?;
        let (digest, format) = match selected {
            Some((digest, format)) => (Some(digest), Some(format)),
            None => (None, None),
        };
        self.origin_format
            .set(format)
            .map_err(|_| Error::invalid("family origin format was already installed"))?;
        self.origin
            .set(digest)
            .map_err(|_| Error::invalid("family origin selection was already installed"))
    }

    /// Whether this opening has a format 2 destination import origin.
    pub(super) fn has_destination_origin(&self) -> bool {
        self.origin_format.get() == Some(&Some(2))
    }

    pub(super) fn verify_origin_lock(
        &self,
        root: &std::path::Path,
        lock: &Arc<super::ProcessLock>,
    ) -> Result<()> {
        if self.origin.get().is_none()
            || !self
                ._owner_lock
                .as_ref()
                .is_some_and(|owner| Arc::ptr_eq(owner, lock))
        {
            return Err(Error::invalid(
                "import context needs this family's actual root lock",
            ));
        }
        super::verify_database_lock(root, lock)
    }

    pub(super) fn check_origin_selection(
        &self,
        selected: Option<crate::evidence::Sha256Digest>,
    ) -> Result<()> {
        if self.origin.get() != Some(&selected) {
            return Err(super::managed_history::unavailable(
                "imported origin differs from the genuine live opening selection",
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct FamilyGate {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    writers: usize,
    admitted: bool,
    admissions: VecDeque<Arc<()>>,
    /// The cohort a new permit joins (see [`FamilyGate::wait_for_cohorts_before`]).
    epoch: u64,
    /// The live permits of each cohort that still has one.
    cohorts: std::collections::BTreeMap<u64, usize>,
}

/// Held from before branch-writer acquisition through durable publication or refusal.
/// This is owned and Send so a pending group commit can retain it after WriteTxn ends.
pub(crate) struct FamilyPermit {
    gate: Arc<FamilyGate>,
    epoch: u64,
}

/// Exclusive family admission. Dropping it allows the next queued admission or writers.
pub(crate) struct AdmissionPermit {
    gate: Arc<FamilyGate>,
}

#[cfg(test)]
impl AdmissionPermit {
    pub(crate) fn belongs_to(&self, gate: &Arc<FamilyGate>) -> bool {
        Arc::ptr_eq(&self.gate, gate)
    }
}

impl FamilyGate {
    /// Closing never waits or drains while catalog ownership is excluded.
    pub(crate) fn try_admit(self: &Arc<Self>) -> Result<AdmissionPermit> {
        let mut state = self.state.try_lock().ok_or(Error::WriterBusy)?;
        if state.admitted || state.writers != 0 || !state.admissions.is_empty() {
            return Err(Error::WriterBusy);
        }
        state.admitted = true;
        Ok(AdmissionPermit { gate: self.clone() })
    }

    #[cfg(test)]
    pub(crate) fn waiting_admissions(&self) -> usize {
        self.state.lock().admissions.len()
    }

    /// Hold the real admission mutex to exercise permit drop ordering and controls.
    #[cfg(test)]
    pub(super) fn with_locked_state(&self, f: impl FnOnce()) {
        let _state = self.state.lock();
        f();
    }

    pub(crate) fn enter(self: &Arc<Self>, opts: &WriteOptions) -> Result<FamilyPermit> {
        let mut state = self.lock(opts)?;
        loop {
            opts.check()?;
            if !state.admitted && state.admissions.is_empty() {
                state.writers += 1;
                let epoch = state.epoch;
                *state.cohorts.entry(epoch).or_default() += 1;
                return Ok(FamilyPermit {
                    gate: self.clone(),
                    epoch,
                });
            }
            if opts.no_wait {
                return Err(Error::WriterBusy);
            }
            self.wait(&mut state, opts);
        }
    }

    #[cfg(test)]
    pub(crate) fn admit(self: &Arc<Self>, opts: &WriteOptions) -> Result<AdmissionPermit> {
        self.admit_draining(opts, || Ok(()))
    }

    /// Settle known pending group publications while waiting for family admission.
    /// This hook is for the fixed internal coordinator drainer, never a user callback.
    /// It runs outside the gate mutex, before the caller takes any branch writer.
    /// It must be bounded and must not acquire a family permit or wait for a branch
    /// writer. It may try a member's writer without waiting, to drive that member's
    /// pending publications.
    #[cfg(test)]
    pub(crate) fn admit_draining(
        self: &Arc<Self>,
        opts: &WriteOptions,
        mut progress: impl FnMut() -> Result<()>,
    ) -> Result<AdmissionPermit> {
        // Declare this before the mutex guard so unwinding releases the mutex before
        // the waiter's Drop removes its queue entry.
        let mut waiter = AdmissionWaiter {
            gate: self.clone(),
            identity: Arc::new(()),
            queued: false,
        };
        let mut state = self.lock(opts)?;
        opts.check()?;
        if opts.no_wait && (state.admitted || state.writers != 0 || !state.admissions.is_empty()) {
            return Err(Error::WriterBusy);
        }
        state.admissions.push_back(waiter.identity.clone());
        waiter.queued = true;
        loop {
            opts.check()?;
            if !state.admitted
                && state.writers == 0
                && state
                    .admissions
                    .front()
                    .is_some_and(|head| Arc::ptr_eq(head, &waiter.identity))
            {
                state.admissions.pop_front();
                waiter.queued = false;
                state.admitted = true;
                return Ok(AdmissionPermit { gate: self.clone() });
            }
            // A pending group publication may hold the last writer permit after its
            // submitting thread has gone away. Give its coordinator a chance to
            // publish or refuse it, without holding the gate mutex across that work.
            drop(state);
            progress()?;
            state = self.lock(opts)?;
            opts.check()?;
            if state.admitted
                || state.writers != 0
                || !Arc::ptr_eq(
                    state.admissions.front().expect("queued admission"),
                    &waiter.identity,
                )
            {
                // Always use a bounded wait here, including with default options:
                // another progress cycle may be needed without a gate notification.
                self.changed.wait_for(&mut state, wait_slice(opts));
            }
        }
    }

    /// Start a new cohort and wait until every permit entered before it has dropped,
    /// without holding back new permits. A permit is held from before its writer is
    /// taken until its commit is published or refused, including by a pending group
    /// commit or a writer of a detached member, so this waits for every writer that
    /// could have seen a state the caller replaced before the call. Writers that enter
    /// afterwards, including nested operations of those waited for, proceed.
    /// `progress` runs outside the gate mutex between bounded waits, under the same
    /// rules as [`admit_draining`](Self::admit_draining)'s.
    pub(crate) fn wait_for_cohorts_before(
        &self,
        opts: &WriteOptions,
        mut progress: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        let mut state = self.lock(opts)?;
        state.epoch += 1;
        let target = state.epoch;
        loop {
            opts.check()?;
            if state.cohorts.range(..target).next().is_none() {
                return Ok(());
            }
            drop(state);
            progress()?;
            state = self.lock(opts)?;
            if state.cohorts.range(..target).next().is_some() {
                self.changed.wait_for(&mut state, wait_slice(opts));
            }
        }
    }

    fn lock(&self, opts: &WriteOptions) -> Result<MutexGuard<'_, State>> {
        opts.check()?;
        if opts.no_wait {
            return self.state.try_lock().ok_or(Error::WriterBusy);
        }
        if !interruptible(opts) {
            return Ok(self.state.lock());
        }
        loop {
            opts.check()?;
            if let Some(state) = self.state.try_lock_for(wait_slice(opts)) {
                return Ok(state);
            }
        }
    }

    fn wait(&self, state: &mut MutexGuard<'_, State>, opts: &WriteOptions) {
        if interruptible(opts) {
            self.changed.wait_for(state, wait_slice(opts));
        } else {
            self.changed.wait(state);
        }
    }
}

fn interruptible(opts: &WriteOptions) -> bool {
    opts.cancel.is_some() || opts.cancellation.is_some() || opts.deadline.is_some()
}

fn wait_slice(opts: &WriteOptions) -> Duration {
    let slice = Duration::from_millis(20);
    opts.deadline.map_or(slice, |deadline| {
        slice.min(deadline.saturating_duration_since(Instant::now()))
    })
}

impl Drop for FamilyPermit {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock();
        state.writers -= 1;
        let drained = state.writers == 0;
        let cohort = state.cohorts.get_mut(&self.epoch).expect("a live cohort");
        *cohort -= 1;
        let ended = *cohort == 0;
        if ended {
            state.cohorts.remove(&self.epoch);
        }
        drop(state);
        if drained || ended {
            self.gate.changed.notify_all();
        }
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        self.gate.state.lock().admitted = false;
        self.gate.changed.notify_all();
    }
}

#[cfg(test)]
struct AdmissionWaiter {
    gate: Arc<FamilyGate>,
    identity: Arc<()>,
    queued: bool,
}

#[cfg(test)]
impl Drop for AdmissionWaiter {
    fn drop(&mut self) {
        if self.queued {
            let mut state = self.gate.state.lock();
            state
                .admissions
                .retain(|entry| !Arc::ptr_eq(entry, &self.identity));
            drop(state);
            self.gate.changed.notify_all();
        }
    }
}
