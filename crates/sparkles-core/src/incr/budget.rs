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

pub(crate) type ReclaimHook = Arc<dyn Fn(u64) -> u64 + Send + Sync>;

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
    /// Transient recovery scratch (e.g. checkpoint file bytes and unshrunk decoded tally)
    /// that is pre-admitted on the parent `QueryPool` without enforcing the new attach's
    /// narrower `local_limit` before `shrink_to` runs.
    pool_scratch_bytes: u64,
    child_bytes: u64,
    peak: u64,
    parent: Option<ParentHandle>,
    credit: Option<LiveCredit>,
    retired: Vec<MemoryLease>,
    /// Optional leaf reclaim callback (e.g. evicting `Slot::history`) invoked when an
    /// active reservation hits `local_limit` or the parent pool. Must never lock
    /// `BudgetState`, `Runtime::reg`, or `Cell::state`.
    reclaim_external: Option<ReclaimHook>,
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
            pool_scratch_bytes: 0,
            child_bytes: 0,
            peak: 0,
            parent,
            credit,
            retired: Vec::new(),
            reclaim_external: None,
        }
    }

    #[inline]
    fn local_live(&self) -> u64 {
        self.state_bytes
            .saturating_add(self.external_bytes)
            .saturating_add(self.pass_bytes)
            .saturating_add(self.scratch_bytes)
            .saturating_add(self.child_bytes)
    }

    #[inline]
    fn total_live(&self) -> u64 {
        self.local_live().saturating_add(self.pool_scratch_bytes)
    }

    fn reclaim(&mut self, needed: u64) -> u64 {
        if needed == 0 || self.external_bytes == 0 {
            return 0;
        }
        let Some(hook) = self.reclaim_external.as_ref() else {
            return 0;
        };
        let freed = hook(needed).min(self.external_bytes);
        self.external_bytes -= freed;
        freed
    }

    fn reserve_raw_inner(
        &mut self,
        delta: u64,
        allow_reclaim: bool,
        enforce_local: bool,
    ) -> std::result::Result<(), ParentRefused> {
        if delta == 0 {
            return Ok(());
        }
        let mut freed_total = 0u64;
        if enforce_local {
            let next_local = self
                .local_live()
                .checked_add(delta)
                .ok_or(ParentRefused::Overflow)?;
            if next_local > self.local_limit && allow_reclaim {
                let freed = self.reclaim(next_local - self.local_limit);
                freed_total = freed_total.saturating_add(freed);
            }
            let next_local = self
                .local_live()
                .checked_add(delta)
                .ok_or(ParentRefused::Overflow)?;
            if next_local > self.local_limit {
                if freed_total > 0 {
                    self.release_raw(freed_total);
                    if self.total_live() == 0 {
                        self.trim_raw();
                    }
                }
                return Err(ParentRefused::Limit {
                    limit: self.local_limit,
                    requested: next_local,
                    scope: Scope::Local,
                });
            }
        }
        if freed_total >= delta {
            let excess = freed_total - delta;
            if excess > 0 {
                self.release_raw(excess);
            }
            self.peak = self.peak.max(self.total_live().saturating_add(delta));
            return Ok(());
        }
        let mut net_add = delta - freed_total;
        if let Some(credit) = &mut self.credit {
            match credit.try_add(net_add) {
                Ok(()) => {}
                Err(err) if allow_reclaim && self.external_bytes > 0 => {
                    let more = self.reclaim(net_add);
                    freed_total = freed_total.saturating_add(more);
                    if more == 0 {
                        if freed_total > 0 {
                            self.release_raw(freed_total);
                            if self.total_live() == 0 {
                                self.trim_raw();
                            }
                        }
                        return Err(err);
                    }
                    if more >= net_add {
                        let excess = more - net_add;
                        if excess > 0 {
                            self.release_raw(excess);
                        }
                    } else {
                        net_add -= more;
                        if let Err(err2) = self.credit.as_mut().unwrap().try_add(net_add) {
                            self.release_raw(freed_total);
                            if self.total_live() == 0 {
                                self.trim_raw();
                            }
                            return Err(err2);
                        }
                    }
                }
                Err(err) => {
                    if freed_total > 0 {
                        self.release_raw(freed_total);
                        if self.total_live() == 0 {
                            self.trim_raw();
                        }
                    }
                    return Err(err);
                }
            }
        }
        let final_total = self.total_live().saturating_add(delta);
        self.peak = self.peak.max(final_total);
        Ok(())
    }

    fn reserve_raw(&mut self, delta: u64) -> std::result::Result<(), ParentRefused> {
        self.reserve_raw_inner(delta, true, true)
    }

    fn reserve_child_raw(&mut self, delta: u64) -> std::result::Result<(), ParentRefused> {
        if delta == 0 {
            return Ok(());
        }
        let next_child = self
            .child_bytes
            .checked_add(delta)
            .ok_or(ParentRefused::Overflow)?;
        self.reserve_raw(delta)?;
        self.child_bytes = next_child;
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

    fn check_local_limit_after_reclaim(&mut self) -> Result<()> {
        let cur = self.local_live();
        if cur > self.local_limit {
            let freed = self.reclaim(cur - self.local_limit);
            if freed > 0 {
                self.release_raw(freed);
                if self.total_live() == 0 {
                    self.trim_raw();
                }
            }
            let after = self.local_live();
            if after > self.local_limit {
                return Err(Error::BudgetExceeded(Budget {
                    kind: BudgetKind::Memory,
                    limit: self.local_limit,
                    requested: after,
                }));
            }
        }
        Ok(())
    }
}

/// Shared handle into a [`ViewBudget`]'s state for pass-time operators, checkpoint
/// decoders, and fixed-point stores. Releases detach excess [`MemoryLease`]s into
/// `retired` rather than dropping them inline.
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

    pub(crate) fn try_reserve_pool_scratch(&self, delta: u64) -> Result<()> {
        if delta == 0 {
            return Ok(());
        }
        let mut s = self.inner.lock();
        s.reserve_raw_inner(delta, true, false)
            .map_err(budget_error)?;
        s.pool_scratch_bytes = s
            .pool_scratch_bytes
            .checked_add(delta)
            .ok_or_else(|| budget_error(ParentRefused::Overflow))?;
        Ok(())
    }

    pub(crate) fn release_pool_scratch(&self, delta: u64) {
        if delta == 0 {
            return;
        }
        let mut s = self.inner.lock();
        let take = delta.min(s.pool_scratch_bytes);
        s.pool_scratch_bytes -= take;
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
    /// `Pass::evaluate`. Returns `Ok(None)` when this budget has no upstream parent, in
    /// which case `Ctx::mem_limit` / `QueryOptions::max_memory_bytes` enforces the
    /// local limit directly at byte granularity without a nested ledger.
    pub(crate) fn child_parent(
        &self,
        pool: Option<&Arc<QueryPool>>,
    ) -> Result<Option<(ParentHandle, Option<Arc<PoolGrant>>)>> {
        let parent_domain = {
            let s = self.inner.lock();
            match s.parent.as_ref().map(ParentHandle::domain) {
                Some(d) => d,
                None => return Ok(None),
            }
        };
        let (domain, grant): (&MemoryDomain, Option<Arc<PoolGrant>>) = match pool {
            Some(p) if p.domain() == parent_domain => {
                (p.memory_domain(), Some(Arc::new(p.empty_grant())))
            }
            _ if parent_domain == MemoryDomain::unpooled().id() => (MemoryDomain::unpooled(), None),
            _ => {
                return Err(Error::invalid(
                    "pooled view parent requires its matching QueryPool",
                ));
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
        Ok(Some((handle, grant)))
    }
}

/// RAII lease for transient scratch bytes reserved on a [`BudgetHandle`].
///
/// Guarantees that scratch reservations are released on every exit path (normal return,
/// early `?` error, or panic unwind). Always drop the guarded allocation before
/// dropping its [`ScratchLease`].
#[derive(Default, Debug)]
pub(crate) struct ScratchLease {
    budget: Option<BudgetHandle>,
    bytes: u64,
    pool_only: bool,
}

impl ScratchLease {
    pub(crate) fn new(budget: Option<BudgetHandle>) -> Self {
        Self {
            budget,
            bytes: 0,
            pool_only: false,
        }
    }

    /// Scratch lease that enforces parent `QueryPool` admission without enforcing the
    /// view's `local_limit` (used for checkpoint file reads and unshrunk tally recovery
    /// before shrinking to the new attach's local budget).
    pub(crate) fn new_pool_only(budget: Option<BudgetHandle>) -> Self {
        Self {
            budget,
            bytes: 0,
            pool_only: true,
        }
    }

    pub(crate) fn from_reserved(budget: Option<BudgetHandle>, bytes: u64) -> Self {
        Self {
            budget,
            bytes,
            pool_only: false,
        }
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) fn try_reserve(&mut self, delta: u64) -> Result<()> {
        if delta == 0 {
            return Ok(());
        }
        if let Some(budget) = &self.budget {
            if self.pool_only {
                budget.try_reserve_pool_scratch(delta)?;
            } else {
                budget.try_reserve_scratch(delta)?;
            }
        }
        self.bytes = self.bytes.saturating_add(delta);
        Ok(())
    }

    pub(crate) fn try_set(&mut self, next: u64) -> Result<()> {
        if next > self.bytes {
            self.try_reserve(next - self.bytes)?;
        } else if next < self.bytes {
            self.release(self.bytes - next);
        }
        Ok(())
    }

    pub(crate) fn release(&mut self, delta: u64) {
        let take = delta.min(self.bytes);
        if take == 0 {
            return;
        }
        self.bytes -= take;
        if let Some(budget) = &self.budget {
            if self.pool_only {
                budget.release_pool_scratch(take);
            } else {
                budget.release_scratch(take);
            }
        }
    }

    /// Disarm this lease and return its reserved byte count for atomic handoff into
    /// persistent state accounting.
    pub(crate) fn take_bytes(&mut self) -> u64 {
        std::mem::take(&mut self.bytes)
    }
}

impl Drop for ScratchLease {
    fn drop(&mut self) {
        if self.bytes > 0 {
            if let Some(budget) = &self.budget {
                if self.pool_only {
                    budget.release_pool_scratch(self.bytes);
                } else {
                    budget.release_scratch(self.bytes);
                }
            }
            self.bytes = 0;
        }
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

    pub(crate) fn set_reclaim_external(&self, hook: Option<ReclaimHook>) {
        self.inner.lock().reclaim_external = hook;
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

    pub(crate) fn local_limit(&self) -> Option<u64> {
        let limit = self.inner.lock().local_limit;
        (limit != u64::MAX).then_some(limit)
    }

    pub(crate) fn scratch_lease(&self) -> ScratchLease {
        ScratchLease::new(Some(self.handle()))
    }

    /// Atomically move `bytes` from persistent `state_bytes` to a [`ScratchLease`],
    /// reserving only any shortfall if `state_bytes < bytes`. Avoids a 2× peak during
    /// `Circuit::reset`.
    pub(crate) fn move_state_to_scratch(&self, bytes: u64) -> Result<ScratchLease> {
        if bytes == 0 {
            return Ok(ScratchLease::new(Some(self.handle())));
        }
        let mut s = self.inner.lock();
        let from_state = bytes.min(s.state_bytes);
        let extra = bytes - from_state;
        if extra > 0 {
            s.reserve_raw(extra).map_err(budget_error)?;
        }
        s.state_bytes -= from_state;
        s.scratch_bytes = s
            .scratch_bytes
            .checked_add(bytes)
            .ok_or_else(|| budget_error(ParentRefused::Overflow))?;
        drop(s);
        Ok(ScratchLease::from_reserved(Some(self.handle()), bytes))
    }

    /// Reconcile persistent state bytes outside a pass.
    pub(crate) fn try_set_state(&self, next_state_bytes: u64) -> Result<()> {
        let mut s = self.inner.lock();
        if next_state_bytes > s.state_bytes {
            let delta = next_state_bytes - s.state_bytes;
            s.reserve_raw(delta).map_err(budget_error)?;
        } else if next_state_bytes < s.state_bytes {
            let delta = s.state_bytes - next_state_bytes;
            s.release_raw(delta);
        }
        s.state_bytes = next_state_bytes;
        s.check_local_limit_after_reclaim()?;
        if s.total_live() == 0 {
            s.trim_raw();
        }
        Ok(())
    }

    /// Lower persistent state accounting to `next_state_bytes` if it shrank (used when
    /// clearing state before a rebuild or transferring a circuit's budget across an
    /// inference switch after old operators are detached).
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

    /// Atomically commit pass-time charges (`pass_bytes`) and an optional transient
    /// output [`ScratchLease`] together with the circuit's new persistent
    /// `next_state_bytes`, avoiding any transient 2× peak or uncharged gap.
    pub(crate) fn commit_pass_with_scratch(
        &self,
        next_state_bytes: u64,
        out_lease: &mut ScratchLease,
    ) -> Result<()> {
        let mut s = self.inner.lock();
        let release_scratch = out_lease.bytes().min(s.scratch_bytes);
        let prev_combined = s
            .state_bytes
            .saturating_add(s.pass_bytes)
            .saturating_add(release_scratch);
        if next_state_bytes > prev_combined {
            let delta = next_state_bytes - prev_combined;
            s.reserve_raw(delta).map_err(budget_error)?;
        } else if next_state_bytes < prev_combined {
            let delta = prev_combined - next_state_bytes;
            s.release_raw(delta);
        }
        s.state_bytes = next_state_bytes;
        s.pass_bytes = 0;
        s.scratch_bytes -= release_scratch;
        let _ = out_lease.take_bytes();
        s.check_local_limit_after_reclaim()?;
        if s.total_live() == 0 {
            s.trim_raw();
        }
        Ok(())
    }

    pub(crate) fn commit_pass(&self, next_state_bytes: u64) -> Result<()> {
        let mut empty = ScratchLease::default();
        self.commit_pass_with_scratch(next_state_bytes, &mut empty)
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
        if next_external_bytes > s.external_bytes {
            let delta = next_external_bytes - s.external_bytes;
            s.reserve_raw_inner(delta, false, true)
                .map_err(budget_error)?;
        } else if next_external_bytes < s.external_bytes {
            let delta = s.external_bytes - next_external_bytes;
            s.release_raw(delta);
        }
        s.external_bytes = next_external_bytes;
        let cur = s.local_live();
        if cur > s.local_limit {
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                limit: s.local_limit,
                requested: cur,
            }));
        }
        if s.total_live() == 0 {
            s.trim_raw();
        }
        Ok(())
    }

    /// Atomically move `lease` (e.g. a decoded `Tally` held in scratch) into
    /// `external_bytes`, reconciling to `next_external_bytes` without a 2× peak.
    pub(crate) fn commit_external_from_scratch(
        &self,
        next_external_bytes: u64,
        lease: &mut ScratchLease,
    ) -> Result<()> {
        let mut s = self.inner.lock();
        let taken = if lease.pool_only {
            lease.bytes().min(s.pool_scratch_bytes)
        } else {
            lease.bytes().min(s.scratch_bytes)
        };
        let prev = s.external_bytes.saturating_add(taken);
        if next_external_bytes > prev {
            let delta = next_external_bytes - prev;
            s.reserve_raw_inner(delta, false, true)
                .map_err(budget_error)?;
        } else if next_external_bytes < prev {
            let delta = prev - next_external_bytes;
            s.release_raw(delta);
        }
        s.external_bytes = next_external_bytes;
        if lease.pool_only {
            s.pool_scratch_bytes -= taken;
        } else {
            s.scratch_bytes -= taken;
        }
        let _ = lease.take_bytes();
        let cur = s.local_live();
        if cur > s.local_limit {
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                limit: s.local_limit,
                requested: cur,
            }));
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incr::circuit::Circuit;
    use crate::incr::compile::compile;
    use crate::incr::runtime::DeferredDrops;
    use crate::sparql::parse_query;
    use crate::store::{Store, StoreOptions};
    use crate::task::Control;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Proves that `Circuit` frees its snapshot/payloads strictly before its
    /// `ViewBudget` returns credit to the parent, and that `DeferredDrops` drops
    /// detached circuits strictly before detached retired leases on both normal exit
    /// and early-`?` error paths.
    #[test]
    fn circuit_and_deferred_drops_free_payloads_before_parent_release() {
        let store = Store::in_memory(StoreOptions::default());
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:a> <urn:p> \"hello\" . <urn:b> <urn:p> \"world\" }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let snap = store.snapshot();
        // Advance the store past `snap` so `circuit` is the sole owner of `snap`.
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:b> <urn:p> \"2\" }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let weak_snap = Arc::downgrade(&snap);

        let domain = MemoryDomain::unpooled();
        let release_saw_dead_snap = Arc::new(AtomicBool::new(false));
        let release_count = Arc::new(AtomicUsize::new(0));
        let check_snap = weak_snap.clone();
        let flag = release_saw_dead_snap.clone();
        let count = release_count.clone();
        let parent = domain.parent(
            |_| Ok(()),
            move |bytes| {
                if bytes > 0 {
                    count.fetch_add(1, Ordering::SeqCst);
                    // When the circuit drops, its `snap` field must already have dropped
                    // before `budget` returns credit to the parent.
                    if check_snap.strong_count() == 0 {
                        flag.store(true, Ordering::SeqCst);
                    }
                }
            },
        );

        let q = parse_query("SELECT ?s ?o WHERE { ?s <urn:p> ?o }", None, &[]).unwrap();
        let mut circuit = Circuit::new(compile(&q, None).unwrap(), None);
        circuit.set_pool(None, Some(parent.clone()));
        circuit.build(snap, &Control::none()).unwrap();
        assert_eq!(weak_snap.strong_count(), 1);
        drop(circuit);
        assert!(
            release_count.load(Ordering::SeqCst) >= 1,
            "dropping charged circuit must release parent credit"
        );
        assert!(
            release_saw_dead_snap.load(Ordering::SeqCst),
            "circuit payloads (snap) must drop strictly before parent release callback runs"
        );

        // Now verify `DeferredDrops` drop order on an early `?` error exit:
        // detached circuit payloads must drop before detached retired leases.
        let snap2 = store.snapshot();
        drop(store);
        let weak_snap2 = Arc::downgrade(&snap2);
        let err_flag = Arc::new(AtomicBool::new(false));
        let err_count = Arc::new(AtomicUsize::new(0));
        let check_snap2 = weak_snap2.clone();
        let flag2 = err_flag.clone();
        let count2 = err_count.clone();
        let parent2 = domain.parent(
            |_| Ok(()),
            move |bytes| {
                if bytes > 0 {
                    count2.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(
                        check_snap2.strong_count(),
                        0,
                        "detached circuit payload must drop strictly before retired lease releases credit"
                    );
                    flag2.store(true, Ordering::SeqCst);
                }
            },
        );

        let mut taken_budget = None;
        let early_err = || -> Result<()> {
            let mut deferred = DeferredDrops::default();
            let mut old = Circuit::new(compile(&q, None).unwrap(), None);
            old.set_pool(None, Some(parent2.clone()));
            old.build(snap2, &Control::none())?;
            let lease = parent2.reserve(4096).unwrap();
            let (_, b) = old.take_budget();
            // Simulate transferring the budget onto the live replacement circuit while
            // `old` and `lease` are queued in `DeferredDrops`.
            taken_budget = Some(b);
            deferred.retired.push(lease);
            deferred.circuits.push(old);
            // Simulate an early `?` return (e.g. `reg.view(name)?` after `sync_inference`).
            Err(crate::error::Error::NotFound("simulated".into()))
        };
        assert!(early_err().is_err());
        assert!(err_flag.load(Ordering::SeqCst));
        assert!(err_count.load(Ordering::SeqCst) >= 1);
        drop(taken_budget);
    }

    /// Proves that `local_limit` is enforced without a 64 KiB `LIVE_STEP` slack overshoot
    /// when a nested child reserves alongside operator pass bytes.
    #[test]
    fn local_limit_enforces_child_bytes_without_live_step_overshoot() {
        let budget = ViewBudget::new(
            Some(32 << 10),
            Some(MemoryDomain::unpooled().parent(|_| Ok(()), |_| {})),
        );
        let handle = budget.handle();
        handle.try_reserve_pass(24 << 10).unwrap();
        let (child, _grant) = handle.child_parent(None).unwrap().unwrap();
        // Remaining local room is 8 KiB; requesting 16 KiB from the child must fail
        // rather than being masked by subtracting LIVE_STEP (64 KiB).
        let err = child.reserve(16 << 10).unwrap_err();
        assert!(
            matches!(
                err,
                ParentRefused::Limit {
                    scope: Scope::Local,
                    ..
                }
            ),
            "expected Scope::Local refusal without 64 KiB overshoot, got {err:?}"
        );
        // Exact remaining 8 KiB still succeeds.
        let ok = child.reserve(8 << 10).unwrap();
        drop(ok);
        handle.release_pass(24 << 10);
    }
}
