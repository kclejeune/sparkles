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
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Retained last by each store and branch set, after their owned resources.
pub(crate) struct Family {
    pub(crate) gate: Arc<FamilyGate>,
    /// The managed-graph registry every member admits its commits against (see
    /// [`crate::managed`]). One registry serves main and every branch, including those
    /// created before it was enabled, so they cannot diverge.
    pub(crate) managed: arc_swap::ArcSwapOption<crate::managed::ManagedState>,
    _owner_lock: Option<Arc<super::ProcessLock>>,
}

impl Family {
    pub(crate) fn new(owner_lock: Option<Arc<super::ProcessLock>>) -> Arc<Self> {
        Arc::new(Self {
            gate: Arc::new(FamilyGate::default()),
            managed: arc_swap::ArcSwapOption::empty(),
            _owner_lock: owner_lock,
        })
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
}

/// Held from before branch-writer acquisition through durable publication or refusal.
/// This is owned and Send so a pending group commit can retain it after WriteTxn ends.
pub(crate) struct FamilyPermit {
    gate: Arc<FamilyGate>,
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
                return Ok(FamilyPermit { gate: self.clone() });
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
    /// It must be bounded and must not acquire a family permit or a branch writer.
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
        drop(state);
        if drained {
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
