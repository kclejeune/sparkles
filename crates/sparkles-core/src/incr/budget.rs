//! Hierarchical memory accounting for incremental view circuits (spec F13 / X14).
//!
//! Each circuit owns a [`ViewBudget`] that accounts for its persistent state,
//! external slot buffers (`Slot::history` or `InternalSlot::tally`), pass-time
//! growth, scratch buffers, and nested SPARQL evaluation against both a local byte
//! limit and an optional upstream [`ParentHandle`] (such as
//! [`QueryPool::work_parent`] or a reading query's [`Ctx::memory_parent`]).
//!
//! Excess credit returned during shrinks is detached into [`MemoryLease`]s via
//! [`LiveCredit::remove_detached`] and [`LiveCredit::trim_detached`] rather than
//! dropped inline, so callers can drop retired leases strictly outside `Runtime::reg`,
//! `Cell::state`, and `InternalCell::state` mutexes and avoid deadlocking with
//! synchronous [`QueryPool`] waiter wake-ups.

use crate::error::{Budget, BudgetKind, Error, Result};
use crate::sparql::pool::{PoolGrant, QueryPool};
use parking_lot::Mutex;
use sparkles_budget::{LiveCredit, MemoryDomain, MemoryLease, ParentHandle, ParentRefused, Scope};
use std::sync::Arc;

pub(crate) fn budget_error(err: ParentRefused) -> Error {
    match err {
        ParentRefused::Limit {
            limit,
            requested,
            scope: Scope::Local,
        } => Error::BudgetExceeded(Budget {
            kind: BudgetKind::Memory,
            limit,
            requested,
        }),
        ParentRefused::Limit {
            limit,
            requested,
            scope: Scope::Work | Scope::Retained,
        } => Error::BudgetExceeded(Budget {
            kind: BudgetKind::ServerMemory,
            limit,
            requested,
        }),
        ParentRefused::Overflow => Error::BudgetExceeded(Budget {
            kind: BudgetKind::Memory,
            limit: u64::MAX,
            requested: u64::MAX,
        }),
        other => Error::Component(Box::new(
            crate::error::ComponentError::new(
                "views",
                "over-budget",
                format!("memory reservation refused: {other}"),
            )
            .with_source(other),
        )),
    }
}

struct BudgetState {
    local_limit: u64,
    state_bytes: u64,
    external_bytes: u64,
    pass_bytes: u64,
    scratch_bytes: u64,
    child_bytes: u64,
    peak: u64,
    parent: Option<ParentHandle>,
    credit: Option<LiveCredit>,
    retired: Vec<MemoryLease>,
}

impl BudgetState {
    fn new(local_limit: Option<u64>, parent: Option<ParentHandle>) -> Self {
        let credit = parent.clone().map(LiveCredit::new);
        Self {
            local_limit: local_limit.unwrap_or(u64::MAX),
            state_bytes: 0,
            external_bytes: 0,
            pass_bytes: 0,
            scratch_bytes: 0,
            child_bytes: 0,
            peak: 0,
            parent,
            credit,
            retired: Vec::new(),
        }
    }

    #[inline]
    fn total_live(&self) -> u64 {
        self.state_bytes
            .saturating_add(self.external_bytes)
            .saturating_add(self.pass_bytes)
            .saturating_add(self.scratch_bytes)
            .saturating_add(self.child_bytes)
    }

    /// Bytes counted against `local_limit`. A nested `QueryMemoryLedger` pre-reserves
    /// up to one [`sparkles_budget::LIVE_STEP`] chunk ahead of its own live bytes
    /// (while enforcing `remaining_local` at exact byte granularity via `Ctx::mem_limit`),
    /// so subtracting at most one `LIVE_STEP` prevents a tight local limit such as
    /// 64 KiB from failing on a tiny nested evaluation while `s.credit` still charges
    /// 100% of `child_bytes` to the upstream `QueryPool`.
    #[inline]
    fn local_live(&self) -> u64 {
        self.state_bytes
            .saturating_add(self.external_bytes)
            .saturating_add(self.pass_bytes)
            .saturating_add(self.scratch_bytes)
            .saturating_add(self.child_bytes.saturating_sub(sparkles_budget::LIVE_STEP))
    }

    fn reserve_raw(&mut self, delta: u64) -> std::result::Result<(), ParentRefused> {
        if delta == 0 {
            return Ok(());
        }
        let next_local = self
            .local_live()
            .checked_add(delta)
            .ok_or(ParentRefused::Overflow)?;
        if next_local > self.local_limit {
            return Err(ParentRefused::Limit {
                limit: self.local_limit,
                requested: next_local,
                scope: Scope::Local,
            });
        }
        let next_total = self
            .total_live()
            .checked_add(delta)
            .ok_or(ParentRefused::Overflow)?;
        if let Some(credit) = &mut self.credit {
            credit.try_add(delta)?;
        }
        self.peak = self.peak.max(next_total);
        Ok(())
    }

    fn reserve_child_raw(&mut self, delta: u64) -> std::result::Result<(), ParentRefused> {
        if delta == 0 {
            return Ok(());
        }
        let next_child = self
            .child_bytes
            .checked_add(delta)
            .ok_or(ParentRefused::Overflow)?;
        let next_local = self
            .state_bytes
            .checked_add(self.external_bytes)
            .and_then(|v| v.checked_add(self.pass_bytes))
            .and_then(|v| v.checked_add(self.scratch_bytes))
            .and_then(|v| v.checked_add(next_child.saturating_sub(sparkles_budget::LIVE_STEP)))
            .ok_or(ParentRefused::Overflow)?;
        if next_local > self.local_limit {
            return Err(ParentRefused::Limit {
                limit: self.local_limit,
                requested: next_local,
                scope: Scope::Local,
            });
        }
        let next_total = self
            .total_live()
            .checked_add(delta)
            .ok_or(ParentRefused::Overflow)?;
        if let Some(credit) = &mut self.credit {
            credit.try_add(delta)?;
        }
        self.child_bytes = next_child;
        self.peak = self.peak.max(next_total);
        Ok(())
    }

    fn release_raw(&mut self, delta: u64) {
        if delta == 0 {
            return;
        }
        if let Some(credit) = &mut self.credit
            && let Some(lease) = credit
                .remove_detached(delta)
                .expect("released view budget bytes cannot exceed live bytes")
        {
            self.retired.push(lease);
        }
    }

    fn trim_raw(&mut self) {
        if let Some(credit) = &mut self.credit
            && let Some(lease) = credit.trim_detached()
        {
            self.retired.push(lease);
        }
    }
}

/// Shared handle into a [`ViewBudget`]'s state for pass-time operators and fixed-point
/// stores. Releases detach excess [`MemoryLease`]s into `retired` rather than dropping
/// them inline.
#[derive(Clone)]
pub(crate) struct BudgetHandle {
    inner: Arc<Mutex<BudgetState>>,
}

impl std::fmt::Debug for BudgetHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BudgetHandle").finish_non_exhaustive()
    }
}

impl BudgetHandle {
    pub(crate) fn try_reserve_pass(&self, delta: u64) -> Result<()> {
        if delta == 0 {
            return Ok(());
        }
        let mut s = self.inner.lock();
        s.reserve_raw(delta).map_err(budget_error)?;
        s.pass_bytes = s
            .pass_bytes
            .checked_add(delta)
            .ok_or_else(|| budget_error(ParentRefused::Overflow))?;
        Ok(())
    }

    pub(crate) fn release_pass(&self, delta: u64) {
        if delta == 0 {
            return;
        }
        let mut s = self.inner.lock();
        let take = delta.min(s.pass_bytes);
        s.pass_bytes -= take;
        s.release_raw(take);
    }

    pub(crate) fn try_reserve_scratch(&self, delta: u64) -> Result<()> {
        if delta == 0 {
            return Ok(());
        }
        let mut s = self.inner.lock();
        s.reserve_raw(delta).map_err(budget_error)?;
        s.scratch_bytes = s
            .scratch_bytes
            .checked_add(delta)
            .ok_or_else(|| budget_error(ParentRefused::Overflow))?;
        Ok(())
    }

    pub(crate) fn release_scratch(&self, delta: u64) {
        if delta == 0 {
            return;
        }
        let mut s = self.inner.lock();
        let take = delta.min(s.scratch_bytes);
        s.scratch_bytes -= take;
        s.release_raw(take);
        if s.total_live() == 0 {
            s.trim_raw();
        }
    }

    /// Remaining room under the local limit for a nested SPARQL evaluation.
    pub(crate) fn remaining_local(&self) -> Option<u64> {
        let s = self.inner.lock();
        if s.local_limit == u64::MAX {
            return None;
        }
        Some(s.local_limit.saturating_sub(s.local_live()))
    }

    /// Construct a child [`ParentHandle`] (and matching empty [`PoolGrant`] when in a
    /// [`QueryPool`] domain) for nested SPARQL evaluation inside `Pass::ctx` or
    /// `Pass::evaluate`. Returns `None` when this budget has no upstream parent, in
    /// which case `Ctx::mem_limit` / `QueryOptions::max_memory_bytes` enforces the
    /// local limit directly at byte granularity without a nested ledger.
    pub(crate) fn child_parent(
        &self,
        pool: Option<&Arc<QueryPool>>,
    ) -> Option<(ParentHandle, Option<Arc<PoolGrant>>)> {
        let parent_domain = {
            let s = self.inner.lock();
            s.parent.as_ref().map(ParentHandle::domain)?
        };
        let (domain, grant): (&MemoryDomain, Option<Arc<PoolGrant>>) = match pool {
            Some(p) if p.domain() == parent_domain => {
                (p.memory_domain(), Some(Arc::new(p.empty_grant())))
            }
            _ if parent_domain == MemoryDomain::unpooled().id() => (MemoryDomain::unpooled(), None),
            _ => {
                debug_assert!(
                    false,
                    "pooled view parent should be paired with its QueryPool"
                );
                return None;
            }
        };
        let take = self.inner.clone();
        let give = self.inner.clone();
        let handle = domain.parent(
            move |bytes| {
                let mut s = take.lock();
                s.reserve_child_raw(bytes)
            },
            move |bytes| {
                let mut s = give.lock();
                s.child_bytes = s
                    .child_bytes
                    .checked_sub(bytes)
                    .expect("nested view query release cannot exceed child_bytes");
                s.release_raw(bytes);
                if s.total_live() == 0 {
                    s.trim_raw();
                }
            },
        );
        Some((handle, grant))
    }
}

/// Hierarchical byte budget for one incremental circuit and its slot buffers.
///
/// Declare this as the last field of [`super::Circuit`] so all operator and result
/// payloads drop strictly before the budget's [`LiveCredit`] and retired
/// [`MemoryLease`]s return credit to the parent.
pub(crate) struct ViewBudget {
    inner: Arc<Mutex<BudgetState>>,
}

impl Default for ViewBudget {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl ViewBudget {
    pub(crate) fn new(local_limit: Option<u64>, parent: Option<ParentHandle>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BudgetState::new(local_limit, parent))),
        }
    }

    pub(crate) fn handle(&self) -> BudgetHandle {
        BudgetHandle {
            inner: self.inner.clone(),
        }
    }

    /// Bind or replace the upstream parent before any live bytes are held.
    pub(crate) fn set_parent(&mut self, parent: Option<ParentHandle>) {
        let mut s = self.inner.lock();
        debug_assert_eq!(
            s.total_live(),
            0,
            "view budget parent must be set before charging"
        );
        if let Some(old) = s.credit.take() {
            let lease = old.into_lease();
            if lease.bytes() > 0 {
                s.retired.push(lease);
            }
        }
        s.credit = parent.clone().map(LiveCredit::new);
        s.parent = parent;
    }

    pub(crate) fn set_local_limit(&self, limit: Option<u64>) {
        self.inner.lock().local_limit = limit.unwrap_or(u64::MAX);
    }

    pub(crate) fn raise_local_limit(&self, limit: u64) {
        let mut s = self.inner.lock();
        if s.local_limit != u64::MAX {
            s.local_limit = s.local_limit.max(limit);
        }
    }

    pub(crate) fn try_reserve_scratch(&self, delta: u64) -> Result<()> {
        self.handle().try_reserve_scratch(delta)
    }

    pub(crate) fn release_scratch(&self, delta: u64) {
        self.handle().release_scratch(delta);
    }

    /// Reconcile persistent state bytes outside a pass (for example after checkpoint
    /// decoding).
    pub(crate) fn try_set_state(&self, next_state_bytes: u64) -> Result<()> {
        let mut s = self.inner.lock();
        let next_local = next_state_bytes
            .checked_add(s.external_bytes)
            .and_then(|v| v.checked_add(s.pass_bytes))
            .and_then(|v| v.checked_add(s.scratch_bytes))
            .and_then(|v| v.checked_add(s.child_bytes.saturating_sub(sparkles_budget::LIVE_STEP)))
            .ok_or_else(|| budget_error(ParentRefused::Overflow))?;
        if next_local > s.local_limit {
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                limit: s.local_limit,
                requested: next_local,
            }));
        }
        if next_state_bytes > s.state_bytes {
            let delta = next_state_bytes - s.state_bytes;
            s.reserve_raw(delta).map_err(budget_error)?;
        } else if next_state_bytes < s.state_bytes {
            let delta = s.state_bytes - next_state_bytes;
            s.release_raw(delta);
        }
        s.state_bytes = next_state_bytes;
        if s.total_live() == 0 {
            s.trim_raw();
        }
        Ok(())
    }

    /// Lower persistent state accounting to `next_state_bytes` if it shrank (used when
    /// transferring a circuit's budget across an inference switch after old operators
    /// are detached).
    pub(crate) fn shrink_state_to(&self, next_state_bytes: u64) {
        let mut s = self.inner.lock();
        if next_state_bytes < s.state_bytes {
            let delta = s.state_bytes - next_state_bytes;
            s.state_bytes = next_state_bytes;
            s.release_raw(delta);
            if s.total_live() == 0 {
                s.trim_raw();
            }
        }
    }

    /// Atomically commit pass-time charges (`pass_bytes`) together with the circuit's
    /// new persistent `next_state_bytes`, avoiding any transient 2× peak.
    pub(crate) fn commit_pass(&self, next_state_bytes: u64) -> Result<()> {
        let mut s = self.inner.lock();
        let next_local = next_state_bytes
            .checked_add(s.external_bytes)
            .and_then(|v| v.checked_add(s.scratch_bytes))
            .and_then(|v| v.checked_add(s.child_bytes.saturating_sub(sparkles_budget::LIVE_STEP)))
            .ok_or_else(|| budget_error(ParentRefused::Overflow))?;
        if next_local > s.local_limit {
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                limit: s.local_limit,
                requested: next_local,
            }));
        }
        let prev_combined = s.state_bytes.saturating_add(s.pass_bytes);
        if next_state_bytes > prev_combined {
            let delta = next_state_bytes - prev_combined;
            s.reserve_raw(delta).map_err(budget_error)?;
        } else if next_state_bytes < prev_combined {
            let delta = prev_combined - next_state_bytes;
            s.release_raw(delta);
        }
        s.state_bytes = next_state_bytes;
        s.pass_bytes = 0;
        if s.total_live() == 0 {
            s.trim_raw();
        }
        Ok(())
    }

    /// Reconcile pass-time charges on a failed pass without failing during cleanup.
    pub(crate) fn abort_pass(&self, current_state_bytes: u64) {
        let mut s = self.inner.lock();
        let prev_combined = s.state_bytes.saturating_add(s.pass_bytes);
        if current_state_bytes < prev_combined {
            s.release_raw(prev_combined - current_state_bytes);
            s.state_bytes = current_state_bytes;
            s.pass_bytes = 0;
        } else {
            s.state_bytes = prev_combined;
            s.pass_bytes = 0;
        }
        if s.total_live() == 0 {
            s.trim_raw();
        }
    }

    /// Release all persistent state and pass charges after the circuit's state has been
    /// cleared, detaching all excess credit down to zero when nothing else is live.
    pub(crate) fn clear_state(&self) {
        let mut s = self.inner.lock();
        let drop_bytes = s.state_bytes.saturating_add(s.pass_bytes);
        s.state_bytes = 0;
        s.pass_bytes = 0;
        s.release_raw(drop_bytes);
        if s.total_live() == 0 {
            s.trim_raw();
        }
    }

    /// Account for external slot buffers (`Slot::history` or `InternalSlot::tally`).
    pub(crate) fn try_set_external(&self, next_external_bytes: u64) -> Result<()> {
        let mut s = self.inner.lock();
        let next_local = s
            .state_bytes
            .checked_add(next_external_bytes)
            .and_then(|v| v.checked_add(s.pass_bytes))
            .and_then(|v| v.checked_add(s.scratch_bytes))
            .and_then(|v| v.checked_add(s.child_bytes.saturating_sub(sparkles_budget::LIVE_STEP)))
            .ok_or_else(|| budget_error(ParentRefused::Overflow))?;
        if next_local > s.local_limit {
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                limit: s.local_limit,
                requested: next_local,
            }));
        }
        if next_external_bytes > s.external_bytes {
            let delta = next_external_bytes - s.external_bytes;
            s.reserve_raw(delta).map_err(budget_error)?;
        } else if next_external_bytes < s.external_bytes {
            let delta = s.external_bytes - next_external_bytes;
            s.release_raw(delta);
        }
        s.external_bytes = next_external_bytes;
        if s.total_live() == 0 {
            s.trim_raw();
        }
        Ok(())
    }

    /// Lower external slot buffer accounting (never fails; used on clear/discard).
    pub(crate) fn set_external_best_effort(&self, next_external_bytes: u64) {
        let mut s = self.inner.lock();
        if next_external_bytes < s.external_bytes {
            let delta = s.external_bytes - next_external_bytes;
            s.external_bytes = next_external_bytes;
            s.release_raw(delta);
            if s.total_live() == 0 {
                s.trim_raw();
            }
        }
    }

    /// Drain all detached leases so the caller can drop them outside runtime and cell
    /// mutexes.
    pub(crate) fn drain_retired(&self) -> Vec<MemoryLease> {
        let mut s = self.inner.lock();
        std::mem::take(&mut s.retired)
    }
}
