//! Private query ownership. Payloads never enter this ledger or its locks.
use super::pool::PoolGrant;
use crate::error::{Budget, BudgetKind, Error, Result};
use parking_lot::Mutex;
use sparkles_budget::{MemoryDomain, MemoryLease, ParentHandle, ParentRefused, Scope};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct QueryMemoryLedger {
    pub(crate) live: AtomicU64,
    pub(crate) peak: AtomicU64,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    owners: Vec<Account>,
    next: u64,
    cursor: bool,
    calls: usize,
    upstream: Option<ParentHandle>,
}
/// The owner id of an eager context's fast account, the first owner of its ledger.
pub(crate) const PRIMARY: u64 = 1;
/// The step in which a live account's reservation from its grant grows, so that a
/// cursor's charges take a pool operation only once per step.
const LIVE_STEP: u64 = 64 << 10;
/// How far a live account's reservation may stay above its live bytes before a release
/// gives the excess back to the grant.
const LIVE_SLACK: u64 = 1 << 20;
#[cfg(test)]
thread_local! {
    /// growths of a live account's reservation on this thread
    pub(crate) static LIVE_GROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
pub(crate) struct MemoryOwner {
    pub(crate) id: u64,
    grant: Option<Arc<PoolGrant>>,
}
struct Account {
    /// Supplied child credit is returned exactly, independently of executor mode.
    exact: bool,
    owner: Arc<MemoryOwner>,
    live: u64,
    /// The account's reservation from its grant. Under eager accounting it is the
    /// admitted high point, kept until the result drops. Under cursor accounting it
    /// follows `live` in steps of [`LIVE_STEP`] and never falls below it.
    eager: u64,
    decode: u64,
    /// Nested contexts own actual upstream credit instead of charging their grant
    /// metadata again. Drop or split only after leaving the ledger lock.
    lease: Option<MemoryLease>,
}

impl Account {
    /// Grow a live account's reservation to cover `own_live`, a step ahead when the
    /// grant can give the step and exactly otherwise. A refused grant changes nothing.
    fn reserve_live(&mut self, grant: &PoolGrant, own_live: u64) -> Result<()> {
        if own_live <= self.eager {
            return Ok(());
        }
        #[cfg(test)]
        LIVE_GROWS.with(|n| n.set(n.get() + 1));
        let ahead = own_live.next_multiple_of(LIVE_STEP).max(own_live);
        match grant.charge(ahead - self.eager) {
            Ok(()) => self.eager = ahead,
            Err(_) if ahead > own_live => {
                grant.charge(own_live - self.eager)?;
                self.eager = own_live;
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn reserve_upstream(&mut self, live: u64) -> std::result::Result<(), ParentRefused> {
        if live <= self.eager {
            return Ok(());
        }
        let lease = self.lease.as_mut().expect("upstream account");
        let ahead = if self.exact {
            live
        } else {
            live.checked_next_multiple_of(LIVE_STEP).unwrap_or(live)
        };
        let next = match lease.try_grow(ahead - self.eager) {
            Ok(()) => ahead,
            Err(ParentRefused::Limit { .. } | ParentRefused::Overflow) if ahead > live => {
                lease.try_grow(live - self.eager)?;
                live
            }
            Err(error) => return Err(error),
        };
        self.eager = next;
        Ok(())
    }

    /// Detach already accounted credit without running its return callback.
    fn retire(&mut self, bytes: u64) -> Release {
        match &mut self.lease {
            Some(lease) => Release {
                grant: None,
                bytes: 0,
                lease: Some(lease.split(bytes).expect("owned nested credit")),
            },
            None => Release {
                grant: self.owner.grant.clone(),
                bytes,
                lease: None,
            },
        }
    }

    /// The part of a live account's reservation to give back now that its live bytes
    /// fell: everything above the next step once the excess passes [`LIVE_SLACK`].
    fn shrink_live(&mut self) -> u64 {
        if self.eager - self.live <= LIVE_SLACK {
            return 0;
        }
        let keep = self.live.next_multiple_of(LIVE_STEP).min(self.eager);
        let bytes = self.eager - keep;
        self.eager = keep;
        bytes
    }
}
/// Captured by the two backend callbacks, with no path back from the ledger.
/// Its final drop follows the final owned lease's release and retires bookkeeping.
struct ChildAccount {
    ledger: Arc<QueryMemoryLedger>,
    owner: Arc<MemoryOwner>,
}
impl Drop for ChildAccount {
    fn drop(&mut self) {
        let retired = {
            let mut state = self.ledger.lock();
            let i = state
                .owners
                .iter()
                .position(|a| a.owner.id == self.owner.id)
                .expect("owned child account");
            let account = &state.owners[i];
            assert!(account.exact && account.live == 0 && account.eager == 0);
            state.owners.swap_remove(i)
        };
        drop(retired);
    }
}

struct Release {
    grant: Option<Arc<PoolGrant>>,
    bytes: u64,
    lease: Option<MemoryLease>,
}
impl Release {
    fn finish(self) {
        if let Some(grant) = self.grant {
            grant.release(self.bytes);
        }
        drop(self.lease);
    }
}
fn parent_error(error: ParentRefused) -> Error {
    match error {
        ParentRefused::Limit {
            limit,
            requested,
            scope,
        } => Error::BudgetExceeded(Budget {
            kind: if scope == Scope::Local {
                BudgetKind::Memory
            } else {
                BudgetKind::ServerMemory
            },
            limit,
            requested,
        }),
        error => Error::Component(Box::new(
            crate::error::ComponentError::new("query-memory", "ownership", error.to_string())
                .with_source(error),
        )),
    }
}
fn memory(limit: u64, requested: u64) -> Error {
    Error::BudgetExceeded(Budget {
        kind: BudgetKind::Memory,
        limit,
        requested,
    })
}
pub(super) fn same(a: Option<&Arc<PoolGrant>>, b: Option<&Arc<PoolGrant>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}
#[cfg(test)]
thread_local! {
    /// Ledger creations and lock acquisitions on this thread (for tests of the
    /// per-query cost of queries that never need the ledger). The counter is
    /// thread-local, so ledger work on other threads, such as rayon workers, is not
    /// counted.
    pub(crate) static LEDGER_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
impl QueryMemoryLedger {
    #[inline]
    fn lock(&self) -> parking_lot::MutexGuard<'_, State> {
        #[cfg(test)]
        LEDGER_OPS.with(|n| n.set(n.get() + 1));
        self.state.lock()
    }
    /// A ledger that takes over an eager context's fast account (see
    /// [`super::ctx::Ctx::memory`]). Owner [`PRIMARY`] is that account, with its live
    /// bytes and its admitted eager contribution to `grant`, so charges made before the
    /// ledger existed release to it and the ledger returns its contribution on drop.
    pub(crate) fn seeded(
        grant: Option<Arc<PoolGrant>>,
        live: u64,
        eager: u64,
        peak: u64,
    ) -> Arc<Self> {
        #[cfg(test)]
        LEDGER_OPS.with(|n| n.set(n.get() + 1));
        debug_assert!(grant.is_none() || eager >= live, "eager covers live");
        Arc::new(Self {
            live: AtomicU64::new(live),
            peak: AtomicU64::new(peak),
            state: Mutex::new(State {
                owners: vec![Account {
                    exact: false,
                    owner: Arc::new(MemoryOwner { id: PRIMARY, grant }),
                    live,
                    eager,
                    decode: 0,
                    lease: None,
                }],
                next: PRIMARY,
                cursor: false,
                calls: 0,
                upstream: None,
            }),
        })
    }
    pub(crate) fn nested(grant: Option<Arc<PoolGrant>>, parent: ParentHandle) -> Arc<Self> {
        let mut ledger = Self::seeded(grant, 0, 0, 0);
        let state = Arc::get_mut(&mut ledger)
            .expect("fresh ledger")
            .state
            .get_mut();
        state.owners[0].lease = Some(parent.reserve(0).expect("sealed zero reservation"));
        state.upstream = Some(parent);
        ledger
    }

    /// Prepare bookkeeping outside the admission lock. Failed and zero-only public
    /// reconfiguration cannot accumulate idle owner records.
    pub(crate) fn owner(&self, grant: Option<&Arc<PoolGrant>>) -> Result<Arc<MemoryOwner>> {
        self.new_owner(grant, false)
    }

    fn new_owner(&self, grant: Option<&Arc<PoolGrant>>, exact: bool) -> Result<Arc<MemoryOwner>> {
        let mut prepared = None;
        let mut storage = Vec::new();
        loop {
            let mut state = self.lock();
            let cursor = state.cursor;
            if let Some(i) = state.owners.iter().position(|a| {
                !a.exact
                    && a.live == 0
                    && (a.eager == 0 || cursor)
                    && Arc::strong_count(&a.owner) == 1
                    && !same(a.owner.grant.as_ref(), grant)
            }) {
                let retired = state.owners.swap_remove(i);
                drop(state);
                // A retired live account gives back the step it still reserved.
                if retired.lease.is_none()
                    && let Some(g) = &retired.owner.grant
                {
                    g.release(retired.eager);
                }
                drop(retired);
                continue;
            }
            if let Some(account) = state
                .owners
                .iter()
                .find(|a| !exact && !a.exact && same(a.owner.grant.as_ref(), grant))
            {
                let owner = account.owner.clone();
                drop(state);
                return Ok(owner);
            }
            if prepared.is_none() {
                drop(state);
                prepared = Some(Arc::new(MemoryOwner {
                    id: 0,
                    grant: grant.cloned(),
                }));
                continue;
            }
            if state.owners.len() == state.owners.capacity() {
                let need = state
                    .owners
                    .len()
                    .checked_add(1)
                    .ok_or_else(|| memory(u64::MAX, u64::MAX))?;
                if storage.capacity() < need {
                    drop(state);
                    storage
                        .try_reserve_exact(need)
                        .map_err(|_| memory(u64::MAX, u64::MAX))?;
                    continue;
                }
                storage.append(&mut state.owners);
                std::mem::swap(&mut state.owners, &mut storage);
            }
            let id = state
                .next
                .checked_add(1)
                .ok_or_else(|| memory(u64::MAX, u64::MAX))?;
            let mut owner = prepared.take().expect("prepared owner");
            Arc::get_mut(&mut owner).expect("unpublished owner").id = id;
            state.next = id;
            let lease = state
                .upstream
                .as_ref()
                .map(|parent| parent.reserve(0).expect("sealed zero reservation"));
            state.owners.push(Account {
                exact,
                owner: owner.clone(),
                live: 0,
                eager: 0,
                decode: 0,
                lease,
            });
            drop(state);
            return Ok(owner);
        }
    }
    /// An exact-credit child has a separate account even when the executor uses
    /// the same grant. Its caller amortizes reservations above this adapter.
    pub(crate) fn child_parent(
        self: &Arc<Self>,
        grant: Option<Arc<PoolGrant>>,
        limit: u64,
    ) -> Result<ParentHandle> {
        let factory = grant
            .as_ref()
            .map_or(MemoryDomain::unpooled(), |g| g.memory_domain());
        if self
            .lock()
            .upstream
            .as_ref()
            .is_some_and(|p| p.domain() != factory.id())
        {
            return Err(parent_error(ParentRefused::ForeignDomain));
        }
        let owner = self.new_owner(grant.as_ref(), true)?;
        let held = Arc::new(ChildAccount {
            ledger: self.clone(),
            owner,
        });
        let take = held.clone();
        let domain = grant
            .as_ref()
            .map_or(MemoryDomain::unpooled(), |g| g.memory_domain());
        Ok(domain.parent(
            move |bytes| take.ledger.reserve_child(&take.owner, limit, bytes),
            move |bytes| held.ledger.release(held.owner.id, bytes),
        ))
    }

    fn reserve_child(
        &self,
        owner: &MemoryOwner,
        limit: u64,
        bytes: u64,
    ) -> std::result::Result<(), ParentRefused> {
        if bytes == 0 {
            return Ok(());
        }
        let mut state = self.lock();
        let live = self
            .live
            .load(Ordering::Relaxed)
            .checked_add(bytes)
            .ok_or(ParentRefused::Overflow)?;
        if live > limit {
            return Err(ParentRefused::Limit {
                limit,
                requested: live,
                scope: Scope::Local,
            });
        }
        let account = state
            .owners
            .iter_mut()
            .find(|a| a.owner.id == owner.id)
            .expect("exact child account");
        assert!(account.exact && account.decode == 0);
        let next = account
            .live
            .checked_add(bytes)
            .ok_or(ParentRefused::Overflow)?;
        if account.lease.is_some() {
            account.reserve_upstream(next)?;
        } else {
            if let Some(grant) = &owner.grant {
                grant.charge(bytes).map_err(|error| {
                    let Error::BudgetExceeded(budget) = error else {
                        unreachable!("grant only refuses memory");
                    };
                    ParentRefused::Limit {
                        limit: budget.limit,
                        requested: budget.requested,
                        scope: Scope::Work,
                    }
                })?;
            }
            account.eager = next;
        }
        account.live = next;
        self.live.store(live, Ordering::Release);
        self.peak.fetch_max(live, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn owner_by_id(&self, id: u64) -> Arc<MemoryOwner> {
        self.lock()
            .owners
            .iter()
            .find(|a| a.owner.id == id)
            .expect("owned account")
            .owner
            .clone()
    }
    fn admit(
        &self,
        state: &mut State,
        owner: &MemoryOwner,
        limit: u64,
        bytes: u64,
        decode: bool,
    ) -> Result<()> {
        let prior = self.live.load(Ordering::Relaxed);
        let live = prior
            .checked_add(bytes)
            .filter(|n| *n <= limit)
            .ok_or_else(|| memory(limit, prior.saturating_add(bytes)))?;
        let cursor = state.cursor;
        let account = state
            .owners
            .iter_mut()
            .find(|a| a.owner.id == owner.id)
            .expect("live owner");
        let own_live = account
            .live
            .checked_add(bytes)
            .ok_or_else(|| memory(limit, u64::MAX))?;
        let own_decode = account
            .decode
            .checked_add(if decode { bytes } else { 0 })
            .ok_or_else(|| memory(limit, u64::MAX))?;
        if account.lease.is_some() {
            account.reserve_upstream(own_live).map_err(parent_error)?;
        } else if let Some(grant) = &owner.grant {
            if cursor && !account.exact {
                account.reserve_live(grant, own_live)?;
            } else if own_live > account.eager {
                grant.charge(own_live - account.eager)?;
                account.eager = own_live;
            }
        }
        account.live = own_live;
        account.decode = own_decode;
        self.live.store(live, Ordering::Release);
        self.peak.fetch_max(live, Ordering::Relaxed);
        Ok(())
    }
    pub(crate) fn reserve(
        &self,
        owner: &MemoryOwner,
        limit: u64,
        bytes: u64,
        decode: bool,
    ) -> Result<()> {
        if bytes == 0 {
            return Ok(());
        }
        self.admit(&mut self.lock(), owner, limit, bytes, decode)
    }
    /// A concrete mutable receiver adds an exact contribution alongside the
    /// executor's existing amortized allowance. Returning it cannot consume that
    /// allowance or strand its own bytes behind LIVE_SLACK.
    pub(crate) fn reserve_receiving(
        &self,
        owner: &MemoryOwner,
        limit: u64,
        bytes: u64,
    ) -> Result<()> {
        let mut state = self.lock();
        let prior = self.live.load(Ordering::Relaxed);
        let live = prior
            .checked_add(bytes)
            .filter(|n| *n <= limit)
            .ok_or_else(|| memory(limit, prior.saturating_add(bytes)))?;
        let account = state
            .owners
            .iter_mut()
            .find(|a| a.owner.id == owner.id)
            .expect("receiving owner");
        let own_live = account
            .live
            .checked_add(bytes)
            .ok_or_else(|| memory(limit, u64::MAX))?;
        let eager = account
            .eager
            .checked_add(bytes)
            .ok_or_else(|| memory(limit, u64::MAX))?;
        if let Some(lease) = &mut account.lease {
            lease.try_grow(bytes).map_err(parent_error)?;
        } else if let Some(grant) = &owner.grant {
            grant.charge(bytes)?;
        }
        account.live = own_live;
        account.eager = eager;
        self.live.store(live, Ordering::Release);
        self.peak.fetch_max(live, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn release_receiving(&self, id: u64, bytes: u64) {
        let release = {
            let mut state = self.lock();
            let account = state
                .owners
                .iter_mut()
                .find(|a| a.owner.id == id)
                .expect("receiving owner");
            assert!(
                bytes <= account.live - account.decode,
                "owned receiving bytes"
            );
            account.live -= bytes;
            account.eager = account
                .eager
                .checked_sub(bytes)
                .expect("receiving contribution");
            self.live.fetch_sub(bytes, Ordering::Release);
            account.retire(bytes)
        };
        // Pool wakeups and parent return callbacks may reenter the ledger.
        release.finish();
    }

    fn release_locked(&self, state: &mut State, id: u64, bytes: u64, decode: bool) -> Release {
        let cursor = state.cursor;
        let account = state
            .owners
            .iter_mut()
            .find(|a| a.owner.id == id)
            .expect("retained owner");
        if decode {
            account.decode = account
                .decode
                .checked_sub(bytes)
                .expect("owned decode bytes");
        } else {
            assert!(
                bytes <= account.live - account.decode,
                "release only owned nondecode bytes"
            );
        }
        account.live = account.live.checked_sub(bytes).expect("owned live bytes");
        let live = self
            .live
            .load(Ordering::Relaxed)
            .checked_sub(bytes)
            .expect("query live bytes");
        self.live.store(live, Ordering::Release);
        let returned = if account.exact {
            account.eager = account
                .eager
                .checked_sub(bytes)
                .expect("owned exact child credit");
            bytes
        } else if cursor && (account.owner.grant.is_some() || account.lease.is_some()) {
            // An eager account keeps its admitted high point until the result drops
            // (X14 stage 4), whether a grant or a parent's lease backs it. Only a
            // finished SELECT or ASK lowers it, through `shrink_eager`.
            account.shrink_live()
        } else {
            0
        };
        account.retire(returned)
    }
    pub(crate) fn release(&self, id: u64, bytes: u64) {
        let release = {
            let mut state = self.lock();
            self.release_locked(&mut state, id, bytes, false)
        };
        release.finish();
    }
    /// Let a cursor's grant give back granules it no longer needs. The accounts keep
    /// their reservations, which their releases shrink.
    pub(crate) fn trim_zero(&self, grant: Option<&Arc<PoolGrant>>) {
        let cursor = self.lock().cursor;
        if cursor && let Some(grant) = grant {
            grant.release(0);
        }
    }
    pub(crate) fn release_decode(&self) {
        // One original owner at a time; no allocation, grant wake or callback under lock.
        loop {
            let release = {
                let mut state = self.lock();
                let Some((id, bytes)) = state
                    .owners
                    .iter()
                    .find(|a| a.decode != 0)
                    .map(|a| (a.owner.id, a.decode))
                else {
                    break;
                };
                self.release_locked(&mut state, id, bytes, true)
            };
            release.finish();
        }
    }
    pub(crate) fn check_output(&self, owner: &MemoryOwner, limit: u64, bytes: u64) -> Result<()> {
        let mut state = self.lock();
        let need = self.live.load(Ordering::Relaxed).saturating_add(bytes);
        // Historical Ctx output peak is an attempted estimate, unlike admitted ownership.
        self.peak.fetch_max(need, Ordering::Relaxed);
        if need > limit {
            return Err(memory(limit, need));
        }
        let cursor = state.cursor;
        let account = state
            .owners
            .iter_mut()
            .find(|a| a.owner.id == owner.id)
            .expect("output owner");
        if account.lease.is_some() {
            let own = account
                .live
                .checked_add(bytes)
                .ok_or_else(|| parent_error(ParentRefused::Overflow))?;
            account.reserve_upstream(own).map_err(parent_error)?;
        } else if let Some(grant) = &owner.grant {
            if cursor {
                grant.cover_transient(grant.used().saturating_add(bytes))?;
            } else {
                let own = account
                    .live
                    .checked_add(bytes)
                    .ok_or_else(|| memory(limit, u64::MAX))?;
                if own > account.eager {
                    grant.charge(own - account.eager)?;
                    account.eager = own;
                }
            }
        }
        Ok(())
    }
    pub(crate) fn check_cursor_transition(&self) -> Result<()> {
        if self.lock().calls != 0 {
            return Err(Error::invalid(
                "algorithm call still owns preparation memory",
            ));
        }
        Ok(())
    }
    pub(crate) fn cursor_mode(&self) -> Result<()> {
        let mut releases = Vec::new();
        loop {
            let mut state = self.lock();
            if state.calls != 0 {
                return Err(Error::invalid(
                    "algorithm call still owns preparation memory",
                ));
            }
            if releases.capacity() < state.owners.len() {
                let n = state.owners.len();
                drop(state);
                releases
                    .try_reserve_exact(n)
                    .map_err(|_| memory(u64::MAX, u64::MAX))?;
                continue;
            }
            for account in &mut state.owners {
                if account.lease.is_some() || account.owner.grant.is_some() {
                    let bytes = account
                        .eager
                        .checked_sub(account.live)
                        .expect("backed live ownership");
                    releases.push(account.retire(bytes));
                }
                account.eager = account.live;
            }
            state.cursor = true;
            drop(state);
            break;
        }
        for release in releases {
            release.finish();
        }
        Ok(())
    }
    /// Lower the admitted eager contributions of a finished eager query to what its
    /// result keeps: the primary account to its live bytes plus `keep`, the bytes of
    /// output that no lease holds, and every other account to its live bytes. The excess returns to each
    /// account's grant. A cursor ledger keeps no eager contribution, and a running
    /// algorithm call may still need its own, so both are left as they are.
    pub(crate) fn shrink_eager(&self, keep: u64) -> Result<()> {
        let mut releases = Vec::new();
        loop {
            let mut state = self.lock();
            if state.cursor || state.calls != 0 {
                return Ok(());
            }
            if releases.capacity() < state.owners.len() {
                let n = state.owners.len();
                drop(state);
                releases
                    .try_reserve_exact(n)
                    .map_err(|_| memory(u64::MAX, u64::MAX))?;
                continue;
            }
            for account in &mut state.owners {
                if account.lease.is_none() && account.owner.grant.is_none() {
                    continue;
                }
                let floor = if account.owner.id == PRIMARY {
                    account.live.saturating_add(keep)
                } else {
                    account.live
                };
                if account.eager > floor {
                    let bytes = account.eager - floor;
                    account.eager = floor;
                    releases.push(account.retire(bytes));
                }
            }
            break;
        }
        for release in releases {
            release.finish();
        }
        Ok(())
    }
    pub(crate) fn start_call(&self) {
        let mut state = self.lock();
        state.calls = state.calls.checked_add(1).expect("bounded active calls");
    }
    pub(crate) fn start_service_frame(&self, service: &crate::algo::ServiceMemory) -> Result<()> {
        let mut state = self.lock();
        if service.closed.load(Ordering::Relaxed) {
            return Err(Error::invalid("SERVICE memory owner is closed"));
        }
        let frames = service
            .frames
            .load(Ordering::Relaxed)
            .checked_add(1)
            .ok_or_else(|| Error::invalid("SERVICE frame count overflows"))?;
        state.calls = state
            .calls
            .checked_add(1)
            .ok_or_else(|| Error::invalid("query frame count overflows"))?;
        service.frames.store(frames, Ordering::Relaxed);
        Ok(())
    }
    pub(crate) fn end_service_frame(&self, service: &crate::algo::ServiceMemory) {
        let mut state = self.lock();
        state.calls = state.calls.checked_sub(1).expect("active SERVICE frame");
        service.frames.store(
            service
                .frames
                .load(Ordering::Relaxed)
                .checked_sub(1)
                .expect("owned SERVICE frame"),
            Ordering::Relaxed,
        );
    }
    fn retire_service(&self, lease: &mut QueryMemoryLease) -> Result<()> {
        let Some(service) = lease.service.as_ref().cloned() else {
            return Ok(());
        };
        let state = self.lock();
        if !Arc::ptr_eq(&service.ledger, &lease.ledger)
            || service.owner.id != lease.owner.id
            || service.frames.load(Ordering::Relaxed) != 0
            || service.used.load(Ordering::Relaxed) != lease.bytes
            || service.closed.load(Ordering::Relaxed)
        {
            return Err(Error::invalid(
                "SERVICE output cannot retire before all native frames and scratch are gone",
            ));
        }
        service.used.store(0, Ordering::Relaxed);
        service.closed.store(true, Ordering::Release);
        drop(state);
        lease.service = None;
        Ok(())
    }
    /// SERVICE and query admission publish in one transaction. A residual frame is
    /// optional for transferred output leases; their shared SERVICE live stays owned.
    pub(crate) fn reserve_service(
        &self,
        service: &crate::algo::ServiceMemory,
        bytes: u64,
        frame: Option<&AtomicU64>,
    ) -> Result<()> {
        let mut state = self.lock();
        if service.closed.load(Ordering::Relaxed) {
            return Err(Error::invalid("SERVICE memory owner is closed"));
        }
        let prior = service.used.load(Ordering::Relaxed);
        let next = prior
            .checked_add(bytes)
            .filter(|n| *n <= service.limit)
            .ok_or_else(|| memory(service.limit, prior.saturating_add(bytes)))?;
        let residual = frame
            .map(|f| {
                f.load(Ordering::Relaxed)
                    .checked_add(bytes)
                    .ok_or_else(|| memory(service.limit, u64::MAX))
            })
            .transpose()?;
        if bytes != 0 {
            self.admit(
                &mut state,
                &service.owner,
                service.query_limit,
                bytes,
                false,
            )?;
        }
        service.used.store(next, Ordering::Relaxed);
        service.peak.fetch_max(next, Ordering::Relaxed);
        if let (Some(frame), Some(next)) = (frame, residual) {
            frame.store(next, Ordering::Relaxed);
        }
        Ok(())
    }
    pub(crate) fn require_service(
        &self,
        service: &crate::algo::ServiceMemory,
        bytes: u64,
    ) -> Result<()> {
        let _state = self.lock();
        if service.closed.load(Ordering::Relaxed) {
            return Err(Error::invalid("SERVICE memory owner is closed"));
        }
        let used = service.used.load(Ordering::Relaxed);
        used.checked_add(bytes)
            .filter(|n| *n <= service.limit)
            .ok_or_else(|| memory(service.limit, used.saturating_add(bytes)))?;
        let live = self.live.load(Ordering::Relaxed);
        live.checked_add(bytes)
            .filter(|n| *n <= service.query_limit)
            .map(|_| ())
            .ok_or_else(|| memory(service.query_limit, live.saturating_add(bytes)))
    }
    pub(crate) fn release_service(
        &self,
        service: &crate::algo::ServiceMemory,
        bytes: u64,
        frame: Option<&AtomicU64>,
    ) {
        let release = {
            let mut state = self.lock();
            let remaining = service
                .used
                .load(Ordering::Relaxed)
                .checked_sub(bytes)
                .expect("SERVICE owned bytes");
            let residual = frame.map(|f| {
                f.load(Ordering::Relaxed)
                    .checked_sub(bytes)
                    .expect("frame owned bytes")
            });
            let release = self.release_locked(&mut state, service.owner.id, bytes, false);
            service.used.store(remaining, Ordering::Relaxed);
            if let (Some(frame), Some(next)) = (frame, residual) {
                frame.store(next, Ordering::Relaxed);
            }
            release
        };
        release.finish();
    }
    pub(crate) fn transfer_native(
        self: &Arc<Self>,
        service: &Arc<crate::algo::ServiceMemory>,
        bytes: u64,
        frame: &AtomicU64,
    ) -> Result<QueryMemoryLease> {
        let _state = self.lock();
        let next = frame
            .load(Ordering::Relaxed)
            .checked_sub(bytes)
            .ok_or_else(|| Error::invalid("algorithm output exceeds its frame-owned capacity"))?;
        frame.store(next, Ordering::Relaxed);
        Ok(QueryMemoryLease {
            ledger: self.clone(),
            owner: service.owner.clone(),
            bytes,
            service: Some(service.clone()),
        })
    }
    #[cfg(test)]
    pub(crate) fn owners(&self) -> usize {
        self.lock().owners.len()
    }
}
impl Drop for QueryMemoryLedger {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        for account in &state.owners {
            if account.lease.is_none()
                && let Some(grant) = &account.owner.grant
            {
                // Both policies keep the reservation at or above the live bytes.
                grant.release(account.eager);
            }
        }
    }
}

/// An independently owned portion of one query ledger. No payload/cache/Ctx back edge.
pub(crate) struct QueryMemoryLease {
    pub(crate) ledger: Arc<QueryMemoryLedger>,
    pub(crate) owner: Arc<MemoryOwner>,
    pub(crate) bytes: u64,
    pub(crate) service: Option<Arc<crate::algo::ServiceMemory>>,
}
impl QueryMemoryLease {
    pub(crate) fn retire_service(&mut self) -> Result<()> {
        let ledger = self.ledger.clone();
        ledger.retire_service(self)
    }

    pub(crate) fn merge(&mut self, slot: &mut Option<Self>) -> Result<()> {
        let other = slot
            .as_ref()
            .ok_or_else(|| Error::invalid("missing algorithm column admission"))?;
        if !Arc::ptr_eq(&self.ledger, &other.ledger)
            || self.owner.id != other.owner.id
            || !match (&self.service, &other.service) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
        {
            return Err(Error::invalid(
                "algorithm outputs belong to different query owners",
            ));
        }
        self.bytes = self
            .bytes
            .checked_add(other.bytes)
            .ok_or_else(|| memory(u64::MAX, u64::MAX))?;
        slot.take().expect("admitted column").bytes = 0;
        Ok(())
    }

    pub(crate) fn resize(&mut self, limit: u64, bytes: u64) -> Result<()> {
        if let Some(service) = &self.service {
            if bytes > self.bytes {
                self.ledger
                    .reserve_service(service, bytes - self.bytes, None)?;
            } else {
                self.ledger
                    .release_service(service, self.bytes - bytes, None);
            }
        } else if bytes > self.bytes {
            self.ledger
                .reserve(&self.owner, limit, bytes - self.bytes, false)?;
        } else {
            self.ledger.release(self.owner.id, self.bytes - bytes);
        }
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for QueryMemoryLease {
    fn drop(&mut self) {
        if let Some(service) = &self.service {
            self.ledger.release_service(service, self.bytes, None);
        } else {
            self.ledger.release(self.owner.id, self.bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algo::{Control, memory::Buffer};
    use crate::sparql::Ctx;
    use crate::sparql::pool::QueryPool;
    use crate::store::{Store, StoreOptions};
    #[test]
    fn receiving_exact_release_returns_after_unlock_with_live_sibling() {
        let used = Arc::new(AtomicU64::new(0));
        let returned = Arc::new(AtomicU64::new(0));
        let callback_ledger =
            Arc::new(std::sync::OnceLock::<std::sync::Weak<QueryMemoryLedger>>::new());
        let take = used.clone();
        let give = used.clone();
        let returns = returned.clone();
        let inspect = callback_ledger.clone();
        let parent = MemoryDomain::unpooled().parent(
            move |n| {
                take.fetch_add(n, Ordering::Relaxed);
                Ok(())
            },
            move |n| {
                if n != 0 {
                    let ledger = inspect.get().unwrap().upgrade().unwrap();
                    assert!(
                        ledger.state.try_lock().is_some(),
                        "parent return may reenter ledger"
                    );
                    returns.fetch_add(n, Ordering::Relaxed);
                }
                give.fetch_sub(n, Ordering::Relaxed);
            },
        );
        let ledger = QueryMemoryLedger::nested(None, parent);
        callback_ledger.set(Arc::downgrade(&ledger)).unwrap();
        let owner = ledger.owner(None).unwrap();
        ledger.reserve(&owner, 256 << 10, 4096, false).unwrap();
        let base = used.load(Ordering::Relaxed);
        ledger.reserve_receiving(&owner, 256 << 10, 8192).unwrap();
        assert_eq!(used.load(Ordering::Relaxed), base + 8192);
        ledger.release_receiving(owner.id, 4096);
        ledger.release_receiving(owner.id, 4096);
        assert_eq!(returned.load(Ordering::Relaxed), 8192);
        assert_eq!(used.load(Ordering::Relaxed), base);
        assert_eq!(ledger.live.load(Ordering::Relaxed), 4096);
        ledger.release(owner.id, 4096);
        // No nonzero callback needs to upgrade a ledger which is already dropping.
        ledger.shrink_eager(0).unwrap();
        assert_eq!(used.load(Ordering::Relaxed), 0);
    }

    const MB: u64 = 1 << 20;
    fn context() -> Ctx {
        Ctx::new(Store::in_memory(StoreOptions::default()).snapshot())
    }

    /// Queries that call no graph algorithm and keep no lease take no ledger lock and
    /// create no ledger: the per-query cost F14 added must stay off their path. Updates
    /// run their WHERE clause on the same context and take no ledger either, so one is
    /// covered too. The counter is thread-local, so it sees only the calling thread.
    /// Ledger work on rayon workers is not counted, and this small dataset stays below
    /// the thresholds of the parallel kernels, so this test guards the serial paths.
    #[test]
    fn plain_queries_take_no_ledger_operations() {
        use crate::io::{RdfFormat, Source};
        use crate::sparql::{QueryOptions, query};
        let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
        for i in 0..300 {
            data.push_str(&format!(
                "ex:p{i} a ex:T{} ; ex:name \"n{i}\" ; ex:age {} ; ex:worksFor ex:o{} ; \
                 ex:knows ex:p{} ; ex:authorOf ex:d{i} .\nex:d{i} ex:title \"t{i}\" .\n",
                i % 3,
                i % 90,
                i % 7,
                (i * 7 + 1) % 300
            ));
        }
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let pool = QueryPool::new(64 * MB);
        let opts = QueryOptions {
            prefixes: vec![("ex".into(), "http://ex.org/".into())],
            pool: Some(pool.clone()),
            max_memory_bytes: Some(32 * MB),
            ..Default::default()
        };
        for q in [
            "SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }",
            "SELECT DISTINCT ?o WHERE { ?s ex:knows ?o }",
            "SELECT ?n WHERE { ?s ex:name ?n } ORDER BY ?n LIMIT 20",
            "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s ex:knows ?o }",
            "SELECT ?p (COUNT(*) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p",
            "SELECT ?t (COUNT(?s) AS ?c) WHERE { ?s a ?t } GROUP BY ?t ORDER BY DESC(?c)",
            "SELECT ?p ?n ?a ?k WHERE { VALUES ?p { ex:p1 ex:p10 ex:p100 } \
             ?p ex:name ?n ; ex:age ?a ; ex:knows ?k }",
            "SELECT ?p ?n ?a WHERE { ?p ex:worksFor ex:o3 ; ex:name ?n ; ex:age ?a }",
            "SELECT ?p ?d ?t WHERE { ?p ex:worksFor ex:o3 . ?p ex:authorOf ?d . ?d ex:title ?t }",
            "SELECT ?p ?a WHERE { ?p ex:name ?n OPTIONAL { ?p ex:age ?a FILTER(?a > 50) } }",
            "SELECT ?p ?x WHERE { ?p ex:knows+ ?x } LIMIT 500",
            "SELECT ?p ?x WHERE { ex:p5 (ex:knows|ex:worksFor)* ?x . BIND(ex:p5 AS ?p) }",
            "CONSTRUCT { ?p ex:friend ?o } WHERE { ?p ex:knows ?o . ?o ex:worksFor ex:o3 }",
            "DESCRIBE ex:p3 ex:p4",
        ] {
            let before = LEDGER_OPS.with(|n| n.get());
            let r = query(store.snapshot(), q, &opts).unwrap();
            assert!(
                !r.test_application_rows().is_empty() || !r.triples().is_empty(),
                "{q}"
            );
            let ops = LEDGER_OPS.with(|n| n.get()) - before;
            eprintln!("{ops} ledger operations for {q}");
            assert_eq!(ops, 0, "{ops} ledger operations for {q}");
        }
        let before = LEDGER_OPS.with(|n| n.get());
        crate::sparql::update::update(
            &store,
            "INSERT { ?p ex:colleague ?q } WHERE { ?p ex:worksFor ?o . ?q ex:worksFor ?o . \
             FILTER(?p != ?q) }",
            &opts,
        )
        .unwrap();
        assert_eq!(
            LEDGER_OPS.with(|n| n.get()),
            before,
            "ledger operations for an update"
        );
        assert_eq!(pool.granted(), 0);
    }
    /// A grant set after the first charge would leave that charge on the old grant,
    /// which debug builds refuse.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "the grant is set before the first charge")]
    fn set_grant_after_a_charge_is_refused() {
        let pool = QueryPool::new(64 * MB);
        let mut ctx = context();
        ctx.set_grant(Some(Arc::new(pool.empty_grant())));
        ctx.check_output((MB / 8) as usize, 1).unwrap();
        ctx.set_grant(None);
    }
    /// Charges made before the ledger exists move to its first owner: they grow and
    /// release there, the eager contribution stays admitted once, and the ledger
    /// returns it when the context drops.
    #[test]
    fn fast_account_hands_over_outstanding_charges_to_the_ledger() {
        let pool = QueryPool::new(64 * MB);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = context();
        ctx.reconfigure_grant(Some(grant.clone()));
        ctx.mem_limit = 8 * MB;
        let before = LEDGER_OPS.with(|n| n.get());
        let held = ctx.charge(3 * MB).unwrap();
        ctx.check_output((MB / 8) as usize, 1).unwrap();
        drop(ctx.charge(2 * MB).unwrap());
        assert!(matches!(ctx.check_output((6 * MB / 8) as usize, 1),
            Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory && b.requested == 9 * MB));
        assert_eq!(LEDGER_OPS.with(|n| n.get()), before, "no ledger yet");
        assert_eq!((grant.used(), ctx.mem_peak()), (5 * MB, 9 * MB));
        // An algorithm call makes the ledger, which takes over the account.
        let ctl = Control::for_query(&ctx, 8 * MB).unwrap();
        assert_eq!(ctx.memory().live.load(Ordering::Acquire), 3 * MB);
        assert_eq!(ctx.memory().owners(), 1);
        held.add(MB).unwrap();
        assert_eq!(
            (ctx.memory().live.load(Ordering::Acquire), grant.used()),
            (4 * MB, 5 * MB)
        );
        held.add(2 * MB).unwrap();
        assert_eq!(grant.used(), 6 * MB);
        assert!(
            matches!(held.add(3 * MB), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
        );
        drop(held);
        assert_eq!(ctx.memory().live.load(Ordering::Acquire), 0);
        drop(ctl);
        assert_eq!(ctx.mem_peak(), 9 * MB);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }
    /// Charges racing the hand-over to the ledger are counted once, on one side.
    #[test]
    fn concurrent_charges_survive_the_hand_over() {
        for round in 0..20 {
            let pool = QueryPool::new(1 << 40);
            let grant = Arc::new(pool.empty_grant());
            let mut ctx = context();
            ctx.reconfigure_grant(Some(grant.clone()));
            let ctx = Arc::new(ctx);
            let barrier = Arc::new(std::sync::Barrier::new(5));
            let threads: Vec<_> = (0..4)
                .map(|t| {
                    let ctx = ctx.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        for i in 0..2000u64 {
                            let c = ctx.charge(1 + (i + t) % 97).unwrap();
                            ctx.check_output(((i * 13) % 50) as usize, 2).unwrap();
                            c.add(5).unwrap();
                            drop(c);
                        }
                    })
                })
                .collect();
            barrier.wait();
            for _ in 0..round * 50 {
                std::hint::spin_loop();
            }
            let ledger = ctx.memory().clone();
            for t in threads {
                t.join().unwrap();
            }
            assert_eq!(ledger.live.load(Ordering::Acquire), 0);
            assert!(grant.used() > 0, "the eager contribution stays admitted");
            drop(ledger);
            drop(Arc::into_inner(ctx).unwrap());
            assert_eq!(grant.used(), 0, "round {round}");
        }
    }
    #[test]
    fn current_public_limit_and_each_original_grant_remain_authoritative() {
        let pool = QueryPool::new(64 * MB);
        let a = Arc::new(pool.empty_grant());
        let b = Arc::new(pool.empty_grant());
        let mut ctx = context();
        ctx.mem_limit = 16 * MB;
        ctx.reconfigure_grant(Some(a.clone()));
        std::mem::forget(ctx.charge(4 * MB).unwrap());
        ctx.reconfigure_grant(Some(b.clone()));
        drop(ctx.charge(2 * MB).unwrap());
        assert_eq!((a.used(), b.used()), (4 * MB, 2 * MB));
        ctx.mem_limit = 5 * MB;
        assert!(
            matches!(ctx.charge(2 * MB), Err(Error::BudgetExceeded(x)) if x.kind==BudgetKind::Memory && x.limit==5*MB)
        );
        assert_eq!((a.used(), b.used()), (4 * MB, 2 * MB));
        ctx.mem_limit = 16 * MB;
        ctx.reconfigure_grant(None);
        drop(ctx.charge(MB).unwrap());
        ctx.reconfigure_grant(Some(a.clone()));
        drop(ctx.charge(MB).unwrap());
        assert_eq!((a.used(), b.used()), (5 * MB, 2 * MB));
        let weak = Arc::downgrade(ctx.memory());
        drop(ctx);
        assert!(weak.upgrade().is_none());
        assert_eq!((a.used(), b.used()), (0, 0));
        drop((a, b));
        assert_eq!(pool.granted(), 0);
    }
    #[test]
    fn rejected_and_zero_configurations_do_not_accumulate_owners() {
        let pool = QueryPool::new(0);
        let mut ctx = context();
        for _ in 0..100 {
            ctx.reconfigure_grant(Some(Arc::new(pool.empty_grant())));
            assert!(
                matches!(ctx.charge(1),Err(Error::BudgetExceeded(x)) if x.kind==BudgetKind::ServerMemory)
            );
            drop(ctx.charge(0).unwrap());
            assert!(ctx.memory().owners() <= 1);
        }
        assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
        assert_eq!(ctx.mem_peak(), 0);
    }
    /// A finished eager query lowers each account's contribution to its live bytes,
    /// and the primary account's to what the result keeps, on the grant the account was
    /// admitted under. Live charges stay covered, and a cursor ledger is left alone.
    #[test]
    fn shrinking_a_finished_eager_query_keeps_live_and_result_bytes() {
        let pool = QueryPool::new(64 * MB);
        let a = Arc::new(pool.empty_grant());
        let b = Arc::new(pool.empty_grant());
        let mut ctx = context();
        ctx.set_grant(Some(a.clone()));
        drop(ctx.charge(8 * MB).unwrap());
        std::mem::forget(ctx.charge(MB).unwrap());
        assert!(a.used() >= 8 * MB);
        ctx.reconfigure_grant(Some(b.clone()));
        drop(ctx.charge(4 * MB).unwrap());
        std::mem::forget(ctx.charge(MB / 2).unwrap());
        assert!(b.used() >= 4 * MB);
        // the primary account keeps its live megabyte and the result's two, which no
        // lease holds, the other account its live half megabyte
        ctx.memory().shrink_eager(2 * MB).unwrap();
        assert_eq!((a.used(), b.used()), (3 * MB, MB / 2));
        ctx.memory().shrink_eager(0).unwrap();
        assert_eq!((a.used(), b.used()), (MB, MB / 2));
        // live charges grow the contribution again
        std::mem::forget(ctx.charge(MB).unwrap());
        assert_eq!(b.used(), MB / 2 + MB);
        drop(ctx);
        assert_eq!((a.used(), b.used()), (0, 0));

        let cursor = context();
        let mut cursor = cursor;
        cursor.set_grant(Some(a.clone()));
        cursor.enable_cursor_accounting().unwrap();
        let live = cursor.charge(3 * MB).unwrap();
        cursor.memory().shrink_eager(0).unwrap();
        assert_eq!(a.used(), 3 * MB);
        drop(live);
        drop(cursor);
        assert_eq!(a.used(), 0);
    }
    #[test]
    fn cursor_transition_and_decode_scratch_release_each_original_owner() {
        let pool = QueryPool::new(64 * MB);
        let a = Arc::new(pool.empty_grant());
        let b = Arc::new(pool.empty_grant());
        let mut ctx = context();
        ctx.reconfigure_grant(Some(a.clone()));
        drop(ctx.charge(4 * MB).unwrap());
        ctx.reconfigure_grant(Some(b.clone()));
        std::mem::forget(ctx.charge(2 * MB).unwrap());
        ctx.enable_cursor_accounting().unwrap();
        assert_eq!((a.used(), b.used()), (0, 2 * MB));
        assert!(ctx.reserve_decode(2000));
        let b_before = b.used();
        assert!(b_before > 2 * MB);
        ctx.reconfigure_grant(Some(a.clone()));
        assert!(ctx.reserve_decode(4000));
        assert!(a.used() > 0);
        let keep = ctx.charge(300).unwrap();
        ctx.release_cursor_work();
        // Each live account keeps the step that covers what it holds, within its slack.
        assert_eq!((a.used(), b.used()), (LIVE_STEP, 2 * MB + LIVE_STEP));
        drop(keep);
        drop(ctx);
        assert_eq!((a.used(), b.used()), (0, 0));
    }
    /// A cursor account reserves its grant in steps: small charges take a pool operation
    /// once per step, the reservation never falls below the live bytes and never stays
    /// more than the slack above them, and the context gives it all back when it drops.
    #[test]
    fn live_reservations_grow_in_steps_and_shrink_with_hysteresis() {
        let pool = QueryPool::new(256 * MB);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = context();
        ctx.set_grant(Some(grant.clone()));
        ctx.enable_cursor_accounting().unwrap();
        let before = LIVE_GROWS.with(|n| n.get());
        let held: Vec<_> = (0..4096).map(|_| ctx.charge(1000).unwrap()).collect();
        let live = 4096 * 1000;
        let grows = LIVE_GROWS.with(|n| n.get()) - before;
        assert!(grows as u64 <= live / LIVE_STEP + 1, "{grows} growths");
        assert!(grant.used() >= live && grant.used() < live + LIVE_STEP);
        let mut held = held;
        // Releases within the slack keep the reservation.
        held.truncate(4096 - 512);
        assert!(grant.used() >= live && grant.used() < live + LIVE_STEP);
        // Past the slack the reservation falls back to the step over the live bytes.
        held.truncate(1024);
        let live = 1024 * 1000;
        assert!(grant.used() >= live && grant.used() <= live + LIVE_SLACK + LIVE_STEP);
        drop(held);
        assert!(grant.used() <= LIVE_SLACK + LIVE_STEP);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    /// Seventeen cursors that charge a 256 MiB pool concurrently in small pieces never
    /// hold more than the pool, each keeps at most one step and the slack above its live
    /// bytes, and all of it comes back.
    #[test]
    fn concurrent_cursors_share_a_small_pool_within_their_steps() {
        let pool = QueryPool::new(256 * MB);
        let grants: Vec<_> = (0..17).map(|_| Arc::new(pool.empty_grant())).collect();
        std::thread::scope(|scope| {
            for grant in &grants {
                let pool = &pool;
                scope.spawn(move || {
                    let mut ctx = context();
                    ctx.set_grant(Some(grant.clone()));
                    ctx.enable_cursor_accounting().unwrap();
                    let mut held = Vec::new();
                    for round in 0..4 {
                        for _ in 0..2000 {
                            match ctx.charge(4096) {
                                Ok(charge) => held.push(charge),
                                Err(Error::BudgetExceeded(_)) => break,
                                Err(error) => panic!("{error:?}"),
                            }
                            assert!(pool.granted() <= pool.capacity());
                        }
                        let live = ctx.memory().live.load(Ordering::Acquire);
                        assert!(grant.used() >= live);
                        assert!(
                            grant.used() <= live + LIVE_SLACK + LIVE_STEP,
                            "round {round}"
                        );
                        held.truncate(held.len() / 4);
                    }
                    drop(held);
                    drop(ctx);
                    assert_eq!(grant.used(), 0);
                });
            }
        });
        drop(grants);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn native_call_captures_owner_limit_and_output_outlives_context() {
        let pool = QueryPool::new(64 * MB);
        let a = Arc::new(pool.empty_grant());
        let b = Arc::new(pool.empty_grant());
        let mut ctx = context();
        ctx.reconfigure_grant(Some(a.clone()));
        ctx.mem_limit = 4 * MB;
        let ctl = Control::for_query(&ctx, 8 * MB).unwrap();
        ctx.reconfigure_grant(Some(b.clone()));
        ctx.mem_limit = MB;
        let output = Buffer::<u8>::new("query-ledger-output", 2 * MB as usize, &ctl)
            .unwrap()
            .into_output();
        let lease = ctl.output_lease(output.capacity() as u64).unwrap().unwrap();
        assert_eq!((a.used(), b.used()), (2 * MB, 0));
        assert!(
            matches!(ctx.charge(1),Err(Error::BudgetExceeded(x)) if x.kind==BudgetKind::Memory)
        );
        drop(ctl);
        let weak = Arc::downgrade(ctx.memory());
        drop(ctx);
        assert!(weak.upgrade().is_some());
        assert_eq!(a.used(), 2 * MB);
        drop(output);
        drop(lease);
        assert!(weak.upgrade().is_none());
        assert_eq!((a.used(), b.used()), (0, 0));
    }
    #[test]
    fn cursor_transition_refuses_active_native_adapter_without_partial_transition() {
        let mut ctx = context();
        let ctl = Control::for_query(&ctx, u64::MAX).unwrap();
        assert!(ctx.enable_cursor_accounting().is_err());
        assert!(!ctx.streams());
        drop(ctl);
        ctx.enable_cursor_accounting().unwrap();
        assert!(ctx.streams());
    }
    #[test]
    fn shared_service_frames_transfer_retire_and_close_without_query_release() {
        let mut ctx = context();
        let pool = QueryPool::new(64 * MB);
        let grant = Arc::new(pool.empty_grant());
        ctx.reconfigure_grant(Some(grant.clone()));
        let root = Control::for_query(&ctx, 100).unwrap();
        let frame = root.query_frame().unwrap();
        let data = Buffer::<u8>::new("service-first", 80, &frame)
            .unwrap()
            .into_output();
        let mut lease = frame.output_lease(80).unwrap().unwrap();
        let service = lease.service.as_ref().unwrap().clone();
        assert_eq!((root.remaining(), root.peak()), (20, 80));
        assert!(lease.retire_service().is_err());
        drop(frame);
        let next = root.query_frame().unwrap();
        assert!(
            matches!(Buffer::<u8>::new("service-aggregate",21,&next),Err(Error::BudgetExceeded(b)) if b.kind==BudgetKind::Memory && b.limit==100 && b.requested==101)
        );
        assert_eq!((root.remaining(), root.peak()), (20, 80));
        drop(next);
        drop(root);
        let before = (
            ctx.memory().live.load(Ordering::Acquire),
            grant.used(),
            ctx.mem_peak(),
        );
        lease.retire_service().unwrap();
        assert!(lease.service.is_none());
        assert_eq!(
            before,
            (
                ctx.memory().live.load(Ordering::Acquire),
                grant.used(),
                ctx.mem_peak()
            )
        );
        assert_eq!(service.peak.load(Ordering::Relaxed), 80);
        assert!(service.closed.load(Ordering::Acquire));
        assert!(matches!(
            ctx.memory().reserve_service(&service, 1, None),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            ctx.memory().start_service_frame(&service),
            Err(Error::Invalid(_))
        ));
        // Later query-only parent/cursor growth is not constrained by completed maxMemory.
        lease.resize(ctx.mem_limit, 101).unwrap();
        assert_eq!(service.peak.load(Ordering::Relaxed), 80);
        drop(data);
        drop(lease);
        assert_eq!(ctx.memory().live.load(Ordering::Acquire), 0);
        drop(service);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }
    #[test]
    fn dead_service_frame_residuals_do_not_accumulate_and_exact_capacity_refuses_before_allocation()
    {
        let ctx = context();
        let root = Control::for_query(&ctx, 80).unwrap();
        for _ in 0..8 {
            let frame = root.query_frame().unwrap();
            let data = Buffer::<u8>::new("service-residual", 80, &frame)
                .unwrap()
                .into_output();
            assert_eq!(root.remaining(), 0);
            drop(data);
            drop(frame);
            assert_eq!(root.remaining(), 80);
        }
        assert_eq!(root.peak(), 80);
        let frame = root.query_frame().unwrap();
        assert!(
            matches!(Buffer::<u8>::new("service-one-over",81,&frame),Err(Error::BudgetExceeded(b)) if b.kind==BudgetKind::Memory && b.requested==81)
        );
        assert_eq!((root.remaining(), root.peak()), (80, 80));
        drop(frame);
        drop(root);
        assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn refused_cursor_transition_with_prepared_local_terms_changes_no_ownership() {
        let mut ctx = context();
        ctx.intern_term(&oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
            "local".repeat(1024),
        )));
        let ctl = Control::for_query(&ctx, u64::MAX).unwrap();
        let before = (
            ctx.memory().live.load(Ordering::Acquire),
            ctx.mem_peak(),
            ctx.local_len(),
        );
        assert!(ctx.enable_cursor_accounting().is_err());
        assert_eq!(
            before,
            (
                ctx.memory().live.load(Ordering::Acquire),
                ctx.mem_peak(),
                ctx.local_len()
            ),
            "refused mode change must not charge local terms"
        );
        drop(ctl);
        ctx.enable_cursor_accounting().unwrap();
        assert!(ctx.streams());
    }
    #[test]
    fn concurrent_native_admission_peak_excludes_rejected_pending_capacity() {
        use std::sync::mpsc;
        use std::time::Duration;
        let pool = QueryPool::new(4 * MB);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = context();
        ctx.reconfigure_grant(Some(grant.clone()));
        let controls = [
            Control::for_query(&ctx, 8 * MB).unwrap(),
            Control::for_query(&ctx, 8 * MB).unwrap(),
        ];
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let (send, receive) = mpsc::channel();
        let mut threads = Vec::new();
        let mut releases = Vec::new();
        for (i, ctl) in controls.iter().cloned().enumerate() {
            let (release, wait) = mpsc::channel();
            releases.push(release);
            let send = send.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move|| {
                barrier.wait();let allocation=Buffer::<u8>::new("query-ledger-concurrent",3*MB as usize,&ctl);
                send.send((i,allocation.as_ref().map(|b|b.capacity()).map_err(|e|matches!(e,Error::BudgetExceeded(x) if x.kind==BudgetKind::ServerMemory)))).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();drop(allocation);
            }));
        }
        drop(send);
        let first = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        let second = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        for release in releases {
            release.send(()).unwrap();
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(
            usize::from(first.1.is_ok()) + usize::from(second.1.is_ok()),
            1
        );
        assert!(
            first
                .1
                .as_ref()
                .err()
                .or(second.1.as_ref().err())
                .copied()
                .unwrap()
        );
        let peaks = [controls[0].peak(), controls[1].peak()];
        assert_eq!(peaks.iter().sum::<u64>(), 3 * MB);
        assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
        assert_eq!(ctx.mem_peak(), 3 * MB);
        assert_eq!(grant.used(), 3 * MB);
        drop(controls);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    /// A cursor that compares stored strings reserves their keys within its decoding
    /// envelope, which grows with the largest key and not with the number of
    /// comparisons, so 2,000 comparisons take the ledger about as often as a filter
    /// that compares nothing. The cursor's end releases the envelope to the pool.
    #[test]
    fn cursor_string_comparisons_take_a_bounded_number_of_ledger_operations() {
        use crate::io::{RdfFormat, Source};
        use crate::sparql::{CursorOptions, QueryOptions, select_cursor};
        let pool = QueryPool::new(64 * MB);
        let opts = QueryOptions {
            prefixes: vec![("ex".into(), "http://ex.org/".into())],
            pool: Some(pool.clone()),
            max_memory_bytes: Some(32 * MB),
            no_cache: true,
            ..Default::default()
        };
        let mut ops = Vec::new();
        let n = 2000;
        for filter in ["BOUND(?x)", "?x < ?y"] {
            let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
            for i in 0..n {
                data.push_str(&format!(
                    "ex:s{i} ex:p \"name {i:05}\" ; ex:q \"name {:05}\" .\n",
                    (i * 7) % n
                ));
            }
            let store = Store::in_memory(StoreOptions::default());
            store
                .load(&[Source::from_bytes(
                    data.into_bytes(),
                    RdfFormat::Turtle,
                    None,
                )])
                .unwrap();
            let before = LEDGER_OPS.with(|n| n.get());
            let mut cursor = select_cursor(
                store.snapshot(),
                &format!("SELECT ?s WHERE {{ ?s ex:p ?x ; ex:q ?y FILTER({filter}) }}"),
                &opts,
                &CursorOptions {
                    batch_rows: 64,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut rows = 0;
            while let Some(batch) = cursor.next_batch().unwrap() {
                rows += batch.len();
            }
            assert!(rows >= n / 4, "{filter}: {rows} of {n}");
            drop(cursor);
            ops.push(LEDGER_OPS.with(|n| n.get()) - before);
            assert_eq!(pool.granted(), 0, "the cursor released its envelope");
        }
        assert!(ops[1] <= ops[0] + 16, "{ops:?}");
    }
}

#[cfg(test)]
mod child_tests;
