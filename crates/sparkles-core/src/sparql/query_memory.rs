//! Private query ownership. Payloads never enter this ledger or its locks.
use super::pool::PoolGrant;
use crate::error::{Budget, BudgetKind, Error, Result};
use parking_lot::{Condvar, Mutex};
use sparkles_budget::{MemoryDomain, MemoryLease, ParentHandle, ParentRefused, Scope};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub(crate) struct QueryMemoryLedger {
    pub(crate) live: AtomicU64,
    pub(crate) peak: AtomicU64,
    /// Total cursor decode + optional prefetch envelope bytes currently admitted on
    /// this ledger. Updated only while holding `state`.
    envelope_reserved: AtomicU64,
    /// Optional prefetch excess bytes currently admitted on this ledger. Updated only
    /// while holding `state`.
    prefetch_bytes: AtomicU64,
    /// Number of active cursor prefetch scopes currently relying on the envelope.
    prefetch_active: AtomicUsize,
    /// Number of detached optional-prefetch backing releases currently running
    /// outside `state`. Updated only while holding `state`.
    reclaim_in_flight: AtomicUsize,
    /// Monotonic count of completed optional-prefetch reclamations (after any
    /// backing return finishes). Updated only while holding `state`.
    reclaim_epoch: AtomicU64,
    #[cfg(test)]
    /// Number of threads currently waiting in `wait_in_flight_prefetch_releases`.
    reclaim_waiters: AtomicUsize,
    #[cfg(test)]
    /// Total entries into `wait_in_flight_prefetch_releases` that observed an
    /// in-flight release and waited on `reclaim_done`.
    reclaim_waits_total: AtomicUsize,
    reclaim_done: Condvar,
    #[cfg(test)]
    /// Number of threads currently waiting in `wait_for_upstream_growth`.
    upstream_waiters: AtomicUsize,
    #[cfg(test)]
    /// Total entries into `wait_for_upstream_growth` that waited on `upstream_done`.
    upstream_waits_total: AtomicUsize,
    /// Monotonic count of non-empty staged upstream leases returned back to the
    /// parent outside `state`, incremented after the parent return finishes so
    /// refusal revalidation detects freed upstream capacity even when the local
    /// minimum delta did not fall.
    upstream_return_epoch: AtomicU64,
    upstream_done: Condvar,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    owners: Vec<Account>,
    next: u64,
    cursor: bool,
    calls: usize,
    upstream: Option<ParentHandle>,
    decode_total: u64,
    prefetch_total: u64,
    prefetch_growth_in_flight: usize,
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
    /// Optional prefetch scratch bytes currently admitted on this account,
    /// reclaimable by required charges whenever no prefetch scope is active.
    prefetch: u64,
    /// Nested contexts own actual upstream credit instead of charging their grant
    /// metadata again. Drop or split only after leaving the ledger lock.
    lease: Option<MemoryLease>,
    /// Number of unlocked upstream growth calls currently in flight on this
    /// non-exact account, allowing concurrent callers to wait for staged credit.
    upstream_in_flight: usize,
}

impl Account {
    /// Grow a live account's reservation to cover `own_live`, a step ahead when the
    /// grant can give the step and exactly otherwise. A refused grant changes nothing.
    fn reserve_live(&mut self, grant: &PoolGrant, own_live: u64) -> Result<()> {
        if own_live <= self.eager {
            return Ok(());
        }
        #[cfg(test)]
        let _ = LIVE_GROWS.try_with(|n| n.set(n.get() + 1));
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

struct UpstreamRequest {
    parent: ParentHandle,
    min_delta: u64,
    pref_delta: u64,
}

enum UpstreamPlan {
    Applied,
    Wait,
    Acquire(UpstreamRequest),
}

enum AdmitStep {
    Done,
    WaitUpstream,
    NeedUpstream(UpstreamRequest),
}

#[derive(Clone, Copy)]
enum RefusalDemand {
    Bytes { bytes: u64, decode: bool },
    Prefetch { need: u64 },
}

/// Staged upstream credit held outside `QueryMemoryLedger::state` while an
/// admission retries. Any excess or uncommitted lease returns inside
/// `ActiveUpstreamOp` so synchronous parent return callbacks never wait on the
/// caller's still-active `UpstreamGrowthGuard`.
struct StagedCredit<'a> {
    ledger: &'a QueryMemoryLedger,
    lease: Option<MemoryLease>,
}

impl<'a> StagedCredit<'a> {
    fn new(ledger: &'a QueryMemoryLedger) -> Self {
        Self {
            ledger,
            lease: None,
        }
    }

    fn is_none(&self) -> bool {
        self.lease.is_none()
    }

    fn is_some(&self) -> bool {
        self.lease.is_some()
    }

    fn bytes(&self) -> u64 {
        self.lease.as_ref().map_or(0, MemoryLease::bytes)
    }

    fn clear(&mut self) {
        if let Some(lease) = self.lease.take() {
            let returned = lease.bytes() != 0;
            {
                let _active = ActiveUpstreamOp::enter();
                drop(lease);
            }
            if returned {
                UpstreamReturnScope::record_return(self.ledger);
                self.ledger
                    .upstream_return_epoch
                    .fetch_add(1, Ordering::Release);
            }
        }
    }
}

impl Drop for StagedCredit<'_> {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Tracks an in-flight unlocked upstream growth on a non-exact account (and
/// optionally the ledger's prefetch envelope) so concurrent callers wait for
/// staged credit rather than racing upstream. Held after `StagedCredit` in
/// `UpstreamAdmission` so struct field drop order returns unused upstream
/// credit before waking waiters.
struct UpstreamGrowthGuard<'a> {
    ledger: &'a QueryMemoryLedger,
    owner_id: Option<u64>,
    prefetch: bool,
}

impl<'a> UpstreamGrowthGuard<'a> {
    fn new(ledger: &'a QueryMemoryLedger) -> Self {
        Self {
            ledger,
            owner_id: None,
            prefetch: false,
        }
    }

    fn is_account_grower(&self) -> bool {
        self.owner_id.is_some()
    }

    fn is_prefetch_grower(&self) -> bool {
        self.prefetch
    }

    fn activate_account(&mut self, account: &mut Account) {
        if !account.exact && self.owner_id.is_none() {
            account.upstream_in_flight += 1;
            self.owner_id = Some(account.owner.id);
        }
    }

    fn activate_prefetch(&mut self, state: &mut State) {
        if !self.prefetch {
            state.prefetch_growth_in_flight += 1;
            self.prefetch = true;
        }
    }

    fn finish_locked(&mut self, state: &mut State) {
        let mut changed = false;
        if let Some(id) = self.owner_id.take() {
            let account = state
                .owners
                .iter_mut()
                .find(|a| a.owner.id == id)
                .expect("growing account");
            debug_assert!(account.upstream_in_flight > 0, "account upstream growth");
            account.upstream_in_flight -= 1;
            changed = true;
        }
        if self.prefetch {
            self.prefetch = false;
            debug_assert!(
                state.prefetch_growth_in_flight > 0,
                "prefetch upstream growth"
            );
            state.prefetch_growth_in_flight -= 1;
            changed = true;
        }
        if changed {
            self.ledger.upstream_done.notify_all();
        }
    }
}

impl Drop for UpstreamGrowthGuard<'_> {
    fn drop(&mut self) {
        if self.owner_id.is_some() || self.prefetch {
            let mut state = self.ledger.lock();
            self.finish_locked(&mut state);
        }
    }
}

/// Bundles staged upstream credit with its in-flight growth guard. Field drop
/// order guarantees `credit` returns any unused or excess upstream lease outside
/// `QueryMemoryLedger::state` (inside `ActiveUpstreamOp`) before `growth` wakes
/// waiting callers.
struct UpstreamAdmission<'a> {
    credit: StagedCredit<'a>,
    growth: UpstreamGrowthGuard<'a>,
}

impl<'a> UpstreamAdmission<'a> {
    fn new(ledger: &'a QueryMemoryLedger) -> Self {
        Self {
            credit: StagedCredit::new(ledger),
            growth: UpstreamGrowthGuard::new(ledger),
        }
    }

    fn finish_locked(&mut self, state: &mut State) {
        if self.credit.is_none() {
            self.growth.finish_locked(state);
        }
    }
}

/// Apply already-held upstream `credit` to cover `own_live` on `account`, wait for
/// an overlapping growth on the same non-exact account when permitted, or return
/// the additional upstream credit to acquire after unlocking `state`.
/// Never calls `ParentHandle::reserve` or drops a `MemoryLease` while locked.
fn plan_or_apply_upstream(
    account: &mut Account,
    upstream: &Option<ParentHandle>,
    own_live: u64,
    admission: &mut UpstreamAdmission<'_>,
    allow_wait: bool,
) -> UpstreamPlan {
    if own_live <= account.eager {
        return UpstreamPlan::Applied;
    }
    let need_more = own_live - account.eager;
    let target = if account.exact {
        own_live
    } else {
        own_live
            .checked_next_multiple_of(LIVE_STEP)
            .unwrap_or(own_live)
    };
    let pref_total = target - account.eager;
    let held = admission.credit.bytes();
    if held < need_more {
        if !account.exact
            && !admission.growth.is_account_grower()
            && account.upstream_in_flight > 0
            && allow_wait
        {
            return UpstreamPlan::Wait;
        }
        admission.growth.activate_account(account);
        let parent = upstream.as_ref().expect("upstream parent").clone();
        return UpstreamPlan::Acquire(UpstreamRequest {
            parent,
            min_delta: need_more - held,
            pref_delta: pref_total - held,
        });
    }
    let use_bytes = held.min(pref_total);
    debug_assert!(use_bytes >= need_more);
    let lease = account.lease.as_mut().expect("upstream account");
    let held_lease = admission
        .credit
        .lease
        .as_mut()
        .expect("held upstream credit");
    if use_bytes == held {
        lease.try_merge(held_lease).expect("same upstream parent");
        admission.credit.lease = None;
    } else {
        let mut part = held_lease
            .split(use_bytes)
            .expect("held credit covers use_bytes");
        lease.try_merge(&mut part).expect("same upstream parent");
    }
    account.eager = account
        .eager
        .checked_add(use_bytes)
        .expect("admitted upstream credit");
    UpstreamPlan::Applied
}

/// Acquire upstream credit outside `QueryMemoryLedger::state`, stepping ahead
/// when the parent allows it and falling back to the exact delta otherwise.
fn reserve_more_credit(
    credit: &mut StagedCredit<'_>,
    req: &UpstreamRequest,
) -> std::result::Result<(), ParentRefused> {
    debug_assert!(req.min_delta > 0 && req.pref_delta >= req.min_delta);
    let _active = ActiveUpstreamOp::enter();
    let mut extra = match req.parent.reserve(req.pref_delta) {
        Ok(lease) => lease,
        Err(ParentRefused::Limit { .. } | ParentRefused::Overflow)
            if req.pref_delta > req.min_delta =>
        {
            req.parent.reserve(req.min_delta)?
        }
        Err(error) => return Err(error),
    };
    match &mut credit.lease {
        Some(existing) => existing.try_merge(&mut extra)?,
        None => credit.lease = Some(extra),
    }
    Ok(())
}

const INLINE_RETURN_SCOPES: usize = 8;

static NEXT_RETURN_SCOPE_TID: AtomicU64 = AtomicU64::new(1);

struct OverflowReturnScope {
    tid: u64,
    index: usize,
    ledger_addr: usize,
    count: u64,
}

static OVERFLOW_RETURN_SCOPES: Mutex<Vec<OverflowReturnScope>> = Mutex::new(Vec::new());

/// Inline, `!Drop` per-thread stack of active upstream return scopes. Because it
/// is `const`-initialized and needs no destructor, native TLS never destroys it
/// before application TLS drop guards run.
struct InlineReturnScopes {
    len: std::cell::Cell<usize>,
    tid: std::cell::Cell<u64>,
    slots: [std::cell::Cell<(*const QueryMemoryLedger, u64)>; INLINE_RETURN_SCOPES],
}

impl InlineReturnScopes {
    const fn new() -> Self {
        Self {
            len: std::cell::Cell::new(0),
            tid: std::cell::Cell::new(0),
            slots: [const { std::cell::Cell::new((std::ptr::null(), 0)) }; INLINE_RETURN_SCOPES],
        }
    }

    fn thread_id(&self) -> u64 {
        let tid = self.tid.get();
        if tid != 0 {
            return tid;
        }
        let tid = NEXT_RETURN_SCOPE_TID.fetch_add(1, Ordering::Relaxed);
        debug_assert_ne!(tid, 0, "return scope thread id overflow");
        self.tid.set(tid);
        tid
    }
}

const _: () = {
    assert!(!std::mem::needs_drop::<std::cell::Cell<usize>>());
    assert!(!std::mem::needs_drop::<InlineReturnScopes>());
};

thread_local! {
    /// Depth of optional-prefetch backing releases (`Release::finish`) currently
    /// executing on this thread, preventing self- or cross-ledger deadlock if a
    /// grant waker or parent return callback re-enters this or another ledger.
    static ACTIVE_PREFETCH_RELEASES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Depth of upstream `ParentHandle::reserve` or staged-credit drops currently
    /// executing on this thread, preventing self-deadlock if a parent callback
    /// re-enters a ledger while an upstream growth is in flight.
    static ACTIVE_UPSTREAM_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Active upstream admission attempts on this thread, keyed by target ledger
    /// pointer, counting same-thread `StagedCredit::clear` returns on that ledger
    /// so refusal revalidation ignores returns induced by the attempt itself
    /// (including synchronous parent callbacks and its own staged clear) while
    /// still observing concurrent returns from other threads and avoiding
    /// cross-ledger misattribution.
    static ACTIVE_UPSTREAM_RETURN_SCOPES: InlineReturnScopes = const { InlineReturnScopes::new() };
}

struct ActiveUpstreamOp {
    tracked: bool,
}

impl ActiveUpstreamOp {
    fn enter() -> Self {
        let tracked = ACTIVE_UPSTREAM_OPS
            .try_with(|depth| depth.set(depth.get() + 1))
            .is_ok();
        Self { tracked }
    }
}

impl Drop for ActiveUpstreamOp {
    fn drop(&mut self) {
        if self.tracked {
            let _ = ACTIVE_UPSTREAM_OPS.try_with(|depth| {
                let cur = depth.get();
                debug_assert!(cur > 0, "active upstream op depth");
                depth.set(cur - 1);
            });
        }
    }
}

/// Tracks same-thread staged-credit returns on `ledger` across one unlocked
/// upstream acquisition attempt and its refusal revalidation. Entered after
/// snapshotting `upstream_return_epoch` under `state` and dropping the lock so
/// thread-local bookkeeping never allocates while holding `state`.
struct UpstreamReturnScope<'a> {
    ledger: &'a QueryMemoryLedger,
    return_epoch: u64,
    index: Option<usize>,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl<'a> UpstreamReturnScope<'a> {
    fn enter(ledger: &'a QueryMemoryLedger, return_epoch: u64) -> Self {
        let ledger_ptr = std::ptr::from_ref(ledger);
        let index = ACTIVE_UPSTREAM_RETURN_SCOPES
            .try_with(|scopes| {
                let index = scopes.len.get();
                if index < INLINE_RETURN_SCOPES {
                    scopes.slots[index].set((ledger_ptr, 0));
                } else {
                    let tid = scopes.thread_id();
                    OVERFLOW_RETURN_SCOPES.lock().push(OverflowReturnScope {
                        tid,
                        index,
                        ledger_addr: ledger_ptr as usize,
                        count: 0,
                    });
                }
                scopes.len.set(index + 1);
                index
            })
            .ok();
        Self {
            ledger,
            return_epoch,
            index,
            _not_send: std::marker::PhantomData,
        }
    }

    fn record_return(ledger: &QueryMemoryLedger) {
        let ledger_ptr = std::ptr::from_ref(ledger);
        let _ = ACTIVE_UPSTREAM_RETURN_SCOPES.try_with(|scopes| {
            let len = scopes.len.get();
            if len == 0 {
                return;
            }
            for slot in &scopes.slots[..len.min(INLINE_RETURN_SCOPES)] {
                let (ptr, count) = slot.get();
                if std::ptr::eq(ptr, ledger_ptr) {
                    slot.set((ptr, count.wrapping_add(1)));
                }
            }
            if len > INLINE_RETURN_SCOPES {
                let tid = scopes.tid.get();
                debug_assert_ne!(tid, 0, "overflow return scopes have thread id");
                let ledger_addr = ledger_ptr as usize;
                let mut overflow = OVERFLOW_RETURN_SCOPES.lock();
                for entry in overflow.iter_mut() {
                    if entry.tid == tid && entry.ledger_addr == ledger_addr {
                        entry.count = entry.count.wrapping_add(1);
                    }
                }
            }
        });
    }

    fn other_returned(&self) -> bool {
        let Some(index) = self.index else {
            return false;
        };
        let Ok(self_returns) = ACTIVE_UPSTREAM_RETURN_SCOPES.try_with(|scopes| {
            if index < INLINE_RETURN_SCOPES {
                scopes.slots[index].get().1
            } else {
                let tid = scopes.tid.get();
                OVERFLOW_RETURN_SCOPES
                    .lock()
                    .iter()
                    .rev()
                    .find(|entry| entry.tid == tid && entry.index == index)
                    .expect("active overflow upstream return scope")
                    .count
            }
        }) else {
            return false;
        };
        self.ledger
            .upstream_return_epoch
            .load(Ordering::Acquire)
            .wrapping_sub(self.return_epoch)
            > self_returns
    }
}

impl Drop for UpstreamReturnScope<'_> {
    fn drop(&mut self) {
        let Some(index) = self.index else {
            return;
        };
        let ledger_ptr = std::ptr::from_ref(self.ledger);
        let _ = ACTIVE_UPSTREAM_RETURN_SCOPES.try_with(|scopes| {
            let len = scopes.len.get();
            debug_assert_eq!(len, index + 1, "LIFO upstream return scope");
            if index < INLINE_RETURN_SCOPES {
                let (ptr, _) = scopes.slots[index].replace((std::ptr::null(), 0));
                debug_assert!(
                    std::ptr::eq(ptr, ledger_ptr),
                    "matching upstream return scope ledger"
                );
            } else {
                let tid = scopes.tid.get();
                let mut overflow = OVERFLOW_RETURN_SCOPES.lock();
                let pos = overflow
                    .iter()
                    .rposition(|entry| entry.tid == tid && entry.index == index)
                    .expect("active overflow upstream return scope");
                let popped = overflow.swap_remove(pos);
                debug_assert_eq!(
                    popped.ledger_addr, ledger_ptr as usize,
                    "matching upstream return scope ledger"
                );
                if overflow.is_empty() && overflow.capacity() > 64 {
                    overflow.shrink_to_fit();
                }
            }
            scopes.len.set(index);
        });
    }
}

#[cfg(test)]
struct WaiterGuard<'a>(&'a AtomicUsize);

#[cfg(test)]
impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

/// Panic-safe guard for an optional-prefetch backing release executing outside
/// `QueryMemoryLedger::state`. Publishes `reclaim_epoch` before decrementing
/// `reclaim_in_flight` and wakes waiters on drop.
struct InFlightPrefetchRelease<'a> {
    ledger: &'a QueryMemoryLedger,
    tracked: bool,
}

impl<'a> InFlightPrefetchRelease<'a> {
    fn enter(ledger: &'a QueryMemoryLedger) -> Self {
        let tracked = ACTIVE_PREFETCH_RELEASES
            .try_with(|depth| depth.set(depth.get() + 1))
            .is_ok();
        Self { ledger, tracked }
    }

    fn finish_with(self, release: Release) {
        release.finish();
    }
}

impl Drop for InFlightPrefetchRelease<'_> {
    fn drop(&mut self) {
        if self.tracked {
            let _ = ACTIVE_PREFETCH_RELEASES.try_with(|depth| {
                let cur = depth.get();
                debug_assert!(cur > 0, "active prefetch release depth");
                depth.set(cur - 1);
            });
        }
        let guard = self.ledger.lock();
        let next_epoch = self.ledger.reclaim_epoch.load(Ordering::Relaxed) + 1;
        // Publish the incremented epoch BEFORE decrementing `reclaim_in_flight` so a
        // concurrent Acquire load of `reclaim_in_flight == 0` always observes it.
        self.ledger
            .reclaim_epoch
            .store(next_epoch, Ordering::Release);
        let prev = self.ledger.reclaim_in_flight.load(Ordering::Relaxed);
        debug_assert!(prev > 0, "in-flight prefetch release");
        self.ledger
            .reclaim_in_flight
            .store(prev - 1, Ordering::Release);
        self.ledger.reclaim_done.notify_all();
        drop(guard);
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
        let _ = LEDGER_OPS.try_with(|n| n.set(n.get() + 1));
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
        let _ = LEDGER_OPS.try_with(|n| n.set(n.get() + 1));
        debug_assert!(grant.is_none() || eager >= live, "eager covers live");
        Arc::new(Self {
            live: AtomicU64::new(live),
            peak: AtomicU64::new(peak),
            envelope_reserved: AtomicU64::new(0),
            prefetch_bytes: AtomicU64::new(0),
            prefetch_active: AtomicUsize::new(0),
            reclaim_in_flight: AtomicUsize::new(0),
            reclaim_epoch: AtomicU64::new(0),
            #[cfg(test)]
            reclaim_waiters: AtomicUsize::new(0),
            #[cfg(test)]
            reclaim_waits_total: AtomicUsize::new(0),
            reclaim_done: Condvar::new(),
            #[cfg(test)]
            upstream_waiters: AtomicUsize::new(0),
            #[cfg(test)]
            upstream_waits_total: AtomicUsize::new(0),
            upstream_return_epoch: AtomicU64::new(0),
            upstream_done: Condvar::new(),
            state: Mutex::new(State {
                owners: vec![Account {
                    exact: false,
                    owner: Arc::new(MemoryOwner { id: PRIMARY, grant }),
                    live,
                    eager,
                    decode: 0,
                    prefetch: 0,
                    lease: None,
                    upstream_in_flight: 0,
                }],
                next: PRIMARY,
                cursor: false,
                calls: 0,
                upstream: None,
                decode_total: 0,
                prefetch_total: 0,
                prefetch_growth_in_flight: 0,
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
                    && a.upstream_in_flight == 0
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
                prefetch: 0,
                lease,
                upstream_in_flight: 0,
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
        loop {
            let epoch = self.reclaim_epoch.load(Ordering::Acquire);
            match self.reserve_child_once(owner, limit, bytes) {
                Ok(()) => return Ok(()),
                Err(ParentRefused::Limit { .. }) if self.reclaim_idle_prefetch_since(epoch) => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn reserve_child_once(
        &self,
        owner: &MemoryOwner,
        limit: u64,
        bytes: u64,
    ) -> std::result::Result<(), ParentRefused> {
        let mut admission = UpstreamAdmission::new(self);
        loop {
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
            let upstream = state.upstream.clone();
            let account = state
                .owners
                .iter_mut()
                .find(|a| a.owner.id == owner.id)
                .expect("exact child account");
            assert!(account.exact && account.decode == 0 && account.prefetch == 0);
            let next = account
                .live
                .checked_add(bytes)
                .ok_or(ParentRefused::Overflow)?;
            if account.lease.is_some() {
                match plan_or_apply_upstream(account, &upstream, next, &mut admission, false) {
                    UpstreamPlan::Applied => {}
                    UpstreamPlan::Wait => unreachable!("exact child account never waits"),
                    UpstreamPlan::Acquire(req) => {
                        drop(state);
                        reserve_more_credit(&mut admission.credit, &req)?;
                        continue;
                    }
                }
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
            return Ok(());
        }
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
    #[inline]
    pub(crate) fn envelope_reserved(&self) -> u64 {
        self.envelope_reserved.load(Ordering::SeqCst)
    }
    #[inline]
    pub(crate) fn has_envelope(&self) -> bool {
        self.envelope_reserved.load(Ordering::Acquire) != 0
    }
    #[inline]
    pub(crate) fn begin_prefetch(&self) {
        self.prefetch_active.fetch_add(1, Ordering::SeqCst);
    }
    #[inline]
    pub(crate) fn end_prefetch(&self) {
        let prev = self.prefetch_active.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(prev > 0, "active prefetch scope");
    }
    /// Admit optional cursor prefetch scratch up to `need` total envelope bytes,
    /// reusing any already-admitted decode or prefetch bytes, and pin the active
    /// prefetch scope before releasing the ledger lock.
    pub(crate) fn admit_prefetch(
        &self,
        grant: Option<&Arc<PoolGrant>>,
        limit: u64,
        need: u64,
    ) -> Result<()> {
        let owner = self.owner(grant)?;
        self.admit_prefetch_to(&owner, limit, need, true)
    }
    /// Grow the optional cursor prefetch envelope to `need` while the caller already
    /// holds an active prefetch pin.
    pub(crate) fn grow_active_prefetch(
        &self,
        grant: Option<&Arc<PoolGrant>>,
        limit: u64,
        need: u64,
    ) -> Result<()> {
        debug_assert!(
            self.prefetch_active.load(Ordering::Relaxed) > 0,
            "caller holds an active prefetch scope"
        );
        let owner = self.owner(grant)?;
        self.admit_prefetch_to(&owner, limit, need, false)
    }
    fn admit_prefetch_to(
        &self,
        owner: &MemoryOwner,
        limit: u64,
        need: u64,
        pin_active: bool,
    ) -> Result<()> {
        let mut admission = UpstreamAdmission::new(self);
        loop {
            let mut state = self.lock();
            let current = state.decode_total.saturating_add(state.prefetch_total);
            if need > current {
                if state.prefetch_growth_in_flight > 0
                    && !admission.growth.is_prefetch_grower()
                    && self.can_wait_for_upstream_growth()
                {
                    self.wait_for_upstream_growth(&mut state);
                    continue;
                }
                match self.admit_prefetch_locked(
                    &mut state,
                    owner,
                    limit,
                    need - current,
                    &mut admission,
                )? {
                    AdmitStep::Done => {}
                    AdmitStep::WaitUpstream => {
                        self.wait_for_upstream_growth(&mut state);
                        continue;
                    }
                    AdmitStep::NeedUpstream(req) => {
                        admission.growth.activate_prefetch(&mut state);
                        let return_epoch = self.upstream_return_epoch.load(Ordering::Acquire);
                        drop(state);
                        let return_scope = UpstreamReturnScope::enter(self, return_epoch);
                        if let Err(err) = reserve_more_credit(&mut admission.credit, &req)
                            && !self.revalidate_upstream_refusal(
                                owner.id,
                                RefusalDemand::Prefetch { need },
                                req.min_delta,
                                &return_scope,
                                &err,
                                &mut admission,
                            )
                        {
                            return Err(parent_error(err));
                        }
                        continue;
                    }
                }
            }
            if pin_active {
                self.prefetch_active.fetch_add(1, Ordering::SeqCst);
            }
            admission.finish_locked(&mut state);
            return Ok(());
        }
    }
    fn admit_prefetch_locked(
        &self,
        state: &mut State,
        owner: &MemoryOwner,
        limit: u64,
        bytes: u64,
        admission: &mut UpstreamAdmission<'_>,
    ) -> Result<AdmitStep> {
        let prior = self.live.load(Ordering::Relaxed);
        let live = prior
            .checked_add(bytes)
            .filter(|n| *n <= limit)
            .ok_or_else(|| memory(limit, prior.saturating_add(bytes)))?;
        let cursor = state.cursor;
        let upstream = state.upstream.clone();
        let allow_wait = self.can_wait_for_upstream_growth();
        let account = state
            .owners
            .iter_mut()
            .find(|a| a.owner.id == owner.id)
            .expect("live owner");
        let own_live = account
            .live
            .checked_add(bytes)
            .ok_or_else(|| memory(limit, u64::MAX))?;
        let own_prefetch = account
            .prefetch
            .checked_add(bytes)
            .ok_or_else(|| memory(limit, u64::MAX))?;
        let prefetch_total = state
            .prefetch_total
            .checked_add(bytes)
            .ok_or_else(|| memory(limit, u64::MAX))?;
        if account.lease.is_some() {
            match plan_or_apply_upstream(account, &upstream, own_live, admission, allow_wait) {
                UpstreamPlan::Applied => {}
                UpstreamPlan::Wait => return Ok(AdmitStep::WaitUpstream),
                UpstreamPlan::Acquire(req) => return Ok(AdmitStep::NeedUpstream(req)),
            }
        } else if let Some(grant) = &owner.grant {
            if cursor && !account.exact {
                account.reserve_live(grant, own_live)?;
            } else if own_live > account.eager {
                grant.charge(own_live - account.eager)?;
                account.eager = own_live;
            }
        }
        account.live = own_live;
        account.prefetch = own_prefetch;
        state.prefetch_total = prefetch_total;
        self.live.store(live, Ordering::Release);
        self.peak.fetch_max(live, Ordering::Relaxed);
        self.prefetch_bytes.store(prefetch_total, Ordering::Release);
        self.envelope_reserved.store(
            state.decode_total.saturating_add(prefetch_total),
            Ordering::Release,
        );
        Ok(AdmitStep::Done)
    }
    fn in_active_prefetch_release(&self) -> bool {
        ACTIVE_PREFETCH_RELEASES
            .try_with(|depth| depth.get() != 0)
            .unwrap_or(true)
    }

    fn can_wait_for_upstream_growth(&self) -> bool {
        ACTIVE_PREFETCH_RELEASES
            .try_with(|depth| depth.get() == 0)
            .unwrap_or(false)
            && ACTIVE_UPSTREAM_OPS
                .try_with(|depth| depth.get() == 0)
                .unwrap_or(false)
    }

    fn wait_for_upstream_growth(&self, guard: &mut parking_lot::MutexGuard<'_, State>) {
        #[cfg(test)]
        let _waiter = {
            self.upstream_waiters.fetch_add(1, Ordering::Release);
            self.upstream_waits_total.fetch_add(1, Ordering::Relaxed);
            WaiterGuard(&self.upstream_waiters)
        };
        self.upstream_done.wait(guard);
    }

    fn needed_upstream_delta(
        &self,
        state: &State,
        owner_id: u64,
        demand: RefusalDemand,
        held: u64,
    ) -> u64 {
        let account = state
            .owners
            .iter()
            .find(|a| a.owner.id == owner_id)
            .expect("live owner");
        let additional = match demand {
            RefusalDemand::Bytes { bytes, decode } => {
                if decode && self.prefetch_active.load(Ordering::SeqCst) == 0 {
                    bytes.saturating_sub(account.prefetch)
                } else {
                    bytes
                }
            }
            RefusalDemand::Prefetch { need } => {
                let current = state.decode_total.saturating_add(state.prefetch_total);
                need.saturating_sub(current)
            }
        };
        if additional == 0 {
            return 0;
        }
        let Some(own_live) = account.live.checked_add(additional) else {
            return u64::MAX;
        };
        own_live.saturating_sub(account.eager).saturating_sub(held)
    }

    fn revalidate_upstream_refusal(
        &self,
        owner_id: u64,
        demand: RefusalDemand,
        min_delta: u64,
        return_scope: &UpstreamReturnScope<'_>,
        err: &ParentRefused,
        admission: &mut UpstreamAdmission<'_>,
    ) -> bool {
        if !matches!(err, ParentRefused::Limit { .. } | ParentRefused::Overflow) {
            return false;
        }
        let for_prefetch = matches!(demand, RefusalDemand::Prefetch { .. });
        let held = admission.credit.bytes();
        let original_need_more = min_delta.saturating_add(held);
        let mut state = self.lock();
        if self.needed_upstream_delta(&state, owner_id, demand, held) < min_delta
            || return_scope.other_returned()
        {
            return true;
        }
        let other_in_flight = |state: &State, growth: &UpstreamGrowthGuard<'_>| {
            let account_in_flight = state
                .owners
                .iter()
                .map(|a| a.upstream_in_flight)
                .sum::<usize>()
                > usize::from(growth.is_account_grower());
            let prefetch_in_flight = for_prefetch
                && state.prefetch_growth_in_flight > usize::from(growth.is_prefetch_grower());
            account_in_flight || prefetch_in_flight
        };
        if !self.can_wait_for_upstream_growth() || !other_in_flight(&state, &admission.growth) {
            return false;
        }
        if admission.credit.is_some() {
            drop(state);
            admission.credit.clear();
            state = self.lock();
        }
        admission.growth.finish_locked(&mut state);
        while other_in_flight(&state, &admission.growth) {
            if self.needed_upstream_delta(&state, owner_id, demand, 0) < original_need_more
                || return_scope.other_returned()
            {
                return true;
            }
            self.wait_for_upstream_growth(&mut state);
        }
        self.needed_upstream_delta(&state, owner_id, demand, 0) < original_need_more
            || return_scope.other_returned()
    }

    fn wait_in_flight_prefetch_releases(&self, guard: &mut parking_lot::MutexGuard<'_, State>) {
        if self.in_active_prefetch_release() {
            return;
        }
        if self.reclaim_in_flight.load(Ordering::Relaxed) == 0 {
            return;
        }
        #[cfg(test)]
        let _waiter = {
            self.reclaim_waiters.fetch_add(1, Ordering::Release);
            self.reclaim_waits_total.fetch_add(1, Ordering::Relaxed);
            WaiterGuard(&self.reclaim_waiters)
        };
        while self.reclaim_in_flight.load(Ordering::Relaxed) != 0 {
            self.reclaim_done.wait(guard);
        }
    }

    /// Reclaim any idle optional prefetch reservation across all accounts, returning
    /// freed grant or parent-lease backing outside the ledger lock, and wait for any
    /// concurrent reclamation's backing return to complete. Returns `true` if idle
    /// prefetch was reclaimed or another caller completed a reclamation since
    /// `epoch_before`.
    fn reclaim_idle_prefetch_since(&self, epoch_before: u64) -> bool {
        // Read in reverse publication order:
        //   with backing return: in_flight += 1 -> prefetch_bytes = ... ->
        //                        [release.finish()] -> epoch += 1 -> in_flight -= 1
        //   without backing:     epoch += 1 -> prefetch_bytes = ...
        let prefetch = self.prefetch_bytes.load(Ordering::Acquire);
        let in_flight = self.reclaim_in_flight.load(Ordering::Acquire);
        let epoch = self.reclaim_epoch.load(Ordering::Acquire);
        if prefetch == 0 && in_flight == 0 {
            return epoch != epoch_before;
        }
        let mut progressed = epoch != epoch_before;
        loop {
            let step = {
                let mut guard = self.lock();
                let state = &mut *guard;
                if state.prefetch_total == 0 {
                    self.wait_in_flight_prefetch_releases(&mut guard);
                    if self.reclaim_epoch.load(Ordering::Relaxed) != epoch_before {
                        progressed = true;
                    }
                    break;
                }
                if self.prefetch_active.load(Ordering::SeqCst) != 0 {
                    self.wait_in_flight_prefetch_releases(&mut guard);
                    if self.reclaim_epoch.load(Ordering::Relaxed) != epoch_before {
                        progressed = true;
                    }
                    break;
                }
                // Dekker handshake with `begin_prefetch` + `envelope_reserved`:
                // publish the lowered envelope with SeqCst before re-checking
                // `prefetch_active`. Any concurrent prefetch either incremented
                // `prefetch_active` before this check (causing us to restore and abort)
                // or observes `decode_total` and takes the locked slow path.
                self.envelope_reserved
                    .store(state.decode_total, Ordering::SeqCst);
                if self.prefetch_active.load(Ordering::SeqCst) != 0 {
                    self.envelope_reserved.store(
                        state.decode_total.saturating_add(state.prefetch_total),
                        Ordering::SeqCst,
                    );
                    self.wait_in_flight_prefetch_releases(&mut guard);
                    if self.reclaim_epoch.load(Ordering::Relaxed) != epoch_before {
                        progressed = true;
                    }
                    break;
                }
                let Some(account) = state.owners.iter_mut().find(|a| a.prefetch != 0) else {
                    debug_assert_eq!(state.prefetch_total, 0);
                    self.wait_in_flight_prefetch_releases(&mut guard);
                    if self.reclaim_epoch.load(Ordering::Relaxed) != epoch_before {
                        progressed = true;
                    }
                    break;
                };
                let bytes = account.prefetch;
                account.prefetch = 0;
                account.live = account
                    .live
                    .checked_sub(bytes)
                    .expect("owned prefetch bytes");
                state.prefetch_total = state
                    .prefetch_total
                    .checked_sub(bytes)
                    .expect("query prefetch bytes");
                let live = self
                    .live
                    .load(Ordering::Relaxed)
                    .checked_sub(bytes)
                    .expect("query live bytes");
                self.live.store(live, Ordering::Release);
                let returned = if state.cursor
                    && !account.exact
                    && (account.owner.grant.is_some() || account.lease.is_some())
                    && account.eager > account.live
                {
                    let excess = account.eager - account.live;
                    account.eager = account.live;
                    excess
                } else {
                    0
                };
                let release = if returned != 0 {
                    let prev = self.reclaim_in_flight.load(Ordering::Relaxed);
                    self.reclaim_in_flight.store(prev + 1, Ordering::Release);
                    Some(account.retire(returned))
                } else {
                    let next_epoch = self.reclaim_epoch.load(Ordering::Relaxed) + 1;
                    self.reclaim_epoch.store(next_epoch, Ordering::Release);
                    None
                };
                self.prefetch_bytes
                    .store(state.prefetch_total, Ordering::Release);
                if state.prefetch_total != 0 {
                    self.envelope_reserved.store(
                        state.decode_total.saturating_add(state.prefetch_total),
                        Ordering::SeqCst,
                    );
                }
                progressed = true;
                release
            };
            if let Some(release) = step {
                InFlightPrefetchRelease::enter(self).finish_with(release);
            }
        }
        progressed
    }
    fn admit_with_credit(
        &self,
        state: &mut State,
        owner: &MemoryOwner,
        limit: u64,
        bytes: u64,
        decode: bool,
        admission: &mut UpstreamAdmission<'_>,
    ) -> Result<AdmitStep> {
        let prior = self.live.load(Ordering::Relaxed);
        let live = prior
            .checked_add(bytes)
            .filter(|n| *n <= limit)
            .ok_or_else(|| memory(limit, prior.saturating_add(bytes)))?;
        let cursor = state.cursor;
        let upstream = state.upstream.clone();
        let allow_wait = self.can_wait_for_upstream_growth();
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
        let decode_total = state
            .decode_total
            .checked_add(if decode { bytes } else { 0 })
            .ok_or_else(|| memory(limit, u64::MAX))?;
        if account.lease.is_some() {
            match plan_or_apply_upstream(account, &upstream, own_live, admission, allow_wait) {
                UpstreamPlan::Applied => {}
                UpstreamPlan::Wait => return Ok(AdmitStep::WaitUpstream),
                UpstreamPlan::Acquire(req) => return Ok(AdmitStep::NeedUpstream(req)),
            }
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
        state.decode_total = decode_total;
        self.live.store(live, Ordering::Release);
        self.peak.fetch_max(live, Ordering::Relaxed);
        if decode {
            self.envelope_reserved.store(
                decode_total.saturating_add(state.prefetch_total),
                Ordering::Release,
            );
        }
        Ok(AdmitStep::Done)
    }
    fn reserve_ordinary_once(&self, owner: &MemoryOwner, limit: u64, bytes: u64) -> Result<()> {
        let mut admission = UpstreamAdmission::new(self);
        loop {
            let mut state = self.lock();
            match self.admit_with_credit(&mut state, owner, limit, bytes, false, &mut admission)? {
                AdmitStep::Done => {
                    admission.finish_locked(&mut state);
                    return Ok(());
                }
                AdmitStep::WaitUpstream => {
                    self.wait_for_upstream_growth(&mut state);
                }
                AdmitStep::NeedUpstream(req) => {
                    let return_epoch = self.upstream_return_epoch.load(Ordering::Acquire);
                    drop(state);
                    let return_scope = UpstreamReturnScope::enter(self, return_epoch);
                    if let Err(err) = reserve_more_credit(&mut admission.credit, &req)
                        && !self.revalidate_upstream_refusal(
                            owner.id,
                            RefusalDemand::Bytes {
                                bytes,
                                decode: false,
                            },
                            req.min_delta,
                            &return_scope,
                            &err,
                            &mut admission,
                        )
                    {
                        return Err(parent_error(err));
                    }
                }
            }
        }
    }
    fn reserve_decode_once(&self, owner: &MemoryOwner, limit: u64, bytes: u64) -> Result<()> {
        let mut admission = UpstreamAdmission::new(self);
        loop {
            let mut guard = self.lock();
            let state = &mut *guard;
            // Promote idle optional prefetch bytes on the same owner into non-reclaimable
            // decode bytes so decoding does not double-charge or fail against idle prefetch.
            let promote = if self.prefetch_active.load(Ordering::SeqCst) == 0 {
                state
                    .owners
                    .iter()
                    .find(|a| a.owner.id == owner.id)
                    .expect("live owner")
                    .prefetch
                    .min(bytes)
            } else {
                0
            };
            let additional = bytes - promote;
            if additional > 0 {
                match self.admit_with_credit(
                    state,
                    owner,
                    limit,
                    additional,
                    true,
                    &mut admission,
                )? {
                    AdmitStep::Done => {}
                    AdmitStep::WaitUpstream => {
                        self.wait_for_upstream_growth(&mut guard);
                        continue;
                    }
                    AdmitStep::NeedUpstream(req) => {
                        let return_epoch = self.upstream_return_epoch.load(Ordering::Acquire);
                        drop(guard);
                        let return_scope = UpstreamReturnScope::enter(self, return_epoch);
                        if let Err(err) = reserve_more_credit(&mut admission.credit, &req)
                            && !self.revalidate_upstream_refusal(
                                owner.id,
                                RefusalDemand::Bytes {
                                    bytes,
                                    decode: true,
                                },
                                req.min_delta,
                                &return_scope,
                                &err,
                                &mut admission,
                            )
                        {
                            return Err(parent_error(err));
                        }
                        continue;
                    }
                }
            }
            if promote > 0 {
                let account = state
                    .owners
                    .iter_mut()
                    .find(|a| a.owner.id == owner.id)
                    .expect("live owner");
                account.prefetch -= promote;
                account.decode = account
                    .decode
                    .checked_add(promote)
                    .expect("promoted decode bytes fit live");
                state.prefetch_total -= promote;
                state.decode_total = state
                    .decode_total
                    .checked_add(promote)
                    .expect("promoted decode total fits live");
                self.prefetch_bytes
                    .store(state.prefetch_total, Ordering::Release);
                self.envelope_reserved.store(
                    state.decode_total.saturating_add(state.prefetch_total),
                    Ordering::Release,
                );
            }
            admission.finish_locked(&mut guard);
            return Ok(());
        }
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
        loop {
            let epoch = self.reclaim_epoch.load(Ordering::Acquire);
            let result = if decode {
                self.reserve_decode_once(owner, limit, bytes)
            } else {
                self.reserve_ordinary_once(owner, limit, bytes)
            };
            match result {
                Ok(()) => return Ok(()),
                Err(Error::BudgetExceeded(_)) if self.reclaim_idle_prefetch_since(epoch) => {}
                Err(error) => return Err(error),
            }
        }
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
        loop {
            let epoch = self.reclaim_epoch.load(Ordering::Acquire);
            match self.reserve_receiving_once(owner, limit, bytes) {
                Ok(()) => return Ok(()),
                Err(Error::BudgetExceeded(_)) if self.reclaim_idle_prefetch_since(epoch) => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn reserve_receiving_once(&self, owner: &MemoryOwner, limit: u64, bytes: u64) -> Result<()> {
        let mut credit = StagedCredit::new(self);
        loop {
            let mut state = self.lock();
            let prior = self.live.load(Ordering::Relaxed);
            let live = prior
                .checked_add(bytes)
                .filter(|n| *n <= limit)
                .ok_or_else(|| memory(limit, prior.saturating_add(bytes)))?;
            let upstream = state.upstream.clone();
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
                if bytes != 0 {
                    if let Some(mut held) = credit.lease.take() {
                        debug_assert_eq!(held.bytes(), bytes);
                        lease.try_merge(&mut held).expect("same upstream parent");
                    } else {
                        let parent = upstream.expect("upstream parent");
                        drop(state);
                        let _active = ActiveUpstreamOp::enter();
                        credit.lease = Some(parent.reserve(bytes).map_err(parent_error)?);
                        continue;
                    }
                }
            } else if let Some(grant) = &owner.grant {
                grant.charge(bytes)?;
            }
            account.live = own_live;
            account.eager = eager;
            self.live.store(live, Ordering::Release);
            self.peak.fetch_max(live, Ordering::Relaxed);
            return Ok(());
        }
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
                bytes <= account.live - account.decode - account.prefetch,
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

    fn release_locked(&self, state: &mut State, id: u64, bytes: u64) -> Release {
        let cursor = state.cursor;
        let account = state
            .owners
            .iter_mut()
            .find(|a| a.owner.id == id)
            .expect("retained owner");
        assert!(
            bytes <= account.live - account.decode - account.prefetch,
            "release only owned nondecode bytes"
        );
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
    fn release_envelope_locked(
        &self,
        state: &mut State,
        id: u64,
        decode_bytes: u64,
        prefetch_bytes: u64,
    ) -> (Release, bool) {
        let cursor = state.cursor;
        let account = state
            .owners
            .iter_mut()
            .find(|a| a.owner.id == id)
            .expect("retained owner");
        account.decode = account
            .decode
            .checked_sub(decode_bytes)
            .expect("owned decode bytes");
        account.prefetch = account
            .prefetch
            .checked_sub(prefetch_bytes)
            .expect("owned prefetch bytes");
        state.decode_total = state
            .decode_total
            .checked_sub(decode_bytes)
            .expect("query decode bytes");
        state.prefetch_total = state
            .prefetch_total
            .checked_sub(prefetch_bytes)
            .expect("query prefetch bytes");
        let total = decode_bytes
            .checked_add(prefetch_bytes)
            .expect("envelope bytes");
        account.live = account.live.checked_sub(total).expect("owned live bytes");
        let live = self
            .live
            .load(Ordering::Relaxed)
            .checked_sub(total)
            .expect("query live bytes");
        self.live.store(live, Ordering::Release);
        let returned = if account.exact {
            account.eager = account
                .eager
                .checked_sub(total)
                .expect("owned exact child credit");
            total
        } else if cursor && (account.owner.grant.is_some() || account.lease.is_some()) {
            // Optional prefetch backing must not be stranded behind LIVE_SLACK once
            // envelope teardown clears the prefetch record that later required charges
            // would otherwise reclaim.
            if prefetch_bytes != 0 && account.eager > account.live {
                let excess = account.eager - account.live;
                account.eager = account.live;
                excess
            } else {
                account.shrink_live()
            }
        } else {
            0
        };
        let track_in_flight = prefetch_bytes != 0 && returned != 0;
        if track_in_flight {
            let prev = self.reclaim_in_flight.load(Ordering::Relaxed);
            self.reclaim_in_flight.store(prev + 1, Ordering::Release);
        } else if prefetch_bytes != 0 {
            let next_epoch = self.reclaim_epoch.load(Ordering::Relaxed) + 1;
            self.reclaim_epoch.store(next_epoch, Ordering::Release);
        }
        self.prefetch_bytes
            .store(state.prefetch_total, Ordering::Release);
        self.envelope_reserved.store(
            state.decode_total.saturating_add(state.prefetch_total),
            Ordering::Release,
        );
        (account.retire(returned), track_in_flight)
    }
    pub(crate) fn release(&self, id: u64, bytes: u64) {
        let release = {
            let mut state = self.lock();
            self.release_locked(&mut state, id, bytes)
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
            let (release, track_in_flight) = {
                let mut state = self.lock();
                let Some((id, decode_bytes, prefetch_bytes)) = state
                    .owners
                    .iter()
                    .find(|a| a.decode != 0 || a.prefetch != 0)
                    .map(|a| (a.owner.id, a.decode, a.prefetch))
                else {
                    debug_assert_eq!(state.decode_total, 0);
                    debug_assert_eq!(state.prefetch_total, 0);
                    self.prefetch_bytes.store(0, Ordering::Release);
                    self.envelope_reserved.store(0, Ordering::Release);
                    break;
                };
                self.release_envelope_locked(&mut state, id, decode_bytes, prefetch_bytes)
            };
            if track_in_flight {
                InFlightPrefetchRelease::enter(self).finish_with(release);
            } else {
                release.finish();
            }
        }
    }
    pub(crate) fn check_output(&self, owner: &MemoryOwner, limit: u64, bytes: u64) -> Result<()> {
        loop {
            let epoch = self.reclaim_epoch.load(Ordering::Acquire);
            match self.check_output_once(owner, limit, bytes) {
                Ok(()) => return Ok(()),
                Err(Error::BudgetExceeded(_)) if self.reclaim_idle_prefetch_since(epoch) => {}
                Err(error) => {
                    if let Error::BudgetExceeded(budget) = &error {
                        // Historical Ctx output peak is an attempted estimate, unlike
                        // admitted ownership.
                        self.peak.fetch_max(budget.requested, Ordering::Relaxed);
                    }
                    return Err(error);
                }
            }
        }
    }
    fn check_output_once(&self, owner: &MemoryOwner, limit: u64, bytes: u64) -> Result<()> {
        let mut admission = UpstreamAdmission::new(self);
        loop {
            let mut state = self.lock();
            let need = self.live.load(Ordering::Relaxed).saturating_add(bytes);
            if need > limit {
                return Err(memory(limit, need));
            }
            let cursor = state.cursor;
            let upstream = state.upstream.clone();
            let allow_wait = self.can_wait_for_upstream_growth();
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
                match plan_or_apply_upstream(account, &upstream, own, &mut admission, allow_wait) {
                    UpstreamPlan::Applied => {}
                    UpstreamPlan::Wait => {
                        self.wait_for_upstream_growth(&mut state);
                        continue;
                    }
                    UpstreamPlan::Acquire(req) => {
                        let return_epoch = self.upstream_return_epoch.load(Ordering::Acquire);
                        drop(state);
                        let return_scope = UpstreamReturnScope::enter(self, return_epoch);
                        if let Err(err) = reserve_more_credit(&mut admission.credit, &req)
                            && !self.revalidate_upstream_refusal(
                                owner.id,
                                RefusalDemand::Bytes {
                                    bytes,
                                    decode: false,
                                },
                                req.min_delta,
                                &return_scope,
                                &err,
                                &mut admission,
                            )
                        {
                            return Err(parent_error(err));
                        }
                        continue;
                    }
                }
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
            self.peak.fetch_max(need, Ordering::Relaxed);
            admission.finish_locked(&mut state);
            return Ok(());
        }
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
        loop {
            let epoch = self.reclaim_epoch.load(Ordering::Acquire);
            match self.reserve_service_once(service, bytes, frame) {
                Ok(()) => return Ok(()),
                Err(Error::BudgetExceeded(_)) if self.reclaim_idle_prefetch_since(epoch) => {}
                Err(error) => return Err(error),
            }
        }
    }
    fn reserve_service_once(
        &self,
        service: &crate::algo::ServiceMemory,
        bytes: u64,
        frame: Option<&AtomicU64>,
    ) -> Result<()> {
        let mut admission = UpstreamAdmission::new(self);
        loop {
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
                match self.admit_with_credit(
                    &mut state,
                    &service.owner,
                    service.query_limit,
                    bytes,
                    false,
                    &mut admission,
                )? {
                    AdmitStep::Done => {}
                    AdmitStep::WaitUpstream => {
                        self.wait_for_upstream_growth(&mut state);
                        continue;
                    }
                    AdmitStep::NeedUpstream(req) => {
                        let return_epoch = self.upstream_return_epoch.load(Ordering::Acquire);
                        drop(state);
                        let return_scope = UpstreamReturnScope::enter(self, return_epoch);
                        if let Err(err) = reserve_more_credit(&mut admission.credit, &req)
                            && !self.revalidate_upstream_refusal(
                                service.owner.id,
                                RefusalDemand::Bytes {
                                    bytes,
                                    decode: false,
                                },
                                req.min_delta,
                                &return_scope,
                                &err,
                                &mut admission,
                            )
                        {
                            return Err(parent_error(err));
                        }
                        continue;
                    }
                }
            }
            service.used.store(next, Ordering::Relaxed);
            service.peak.fetch_max(next, Ordering::Relaxed);
            if let (Some(frame), Some(next)) = (frame, residual) {
                frame.store(next, Ordering::Relaxed);
            }
            admission.finish_locked(&mut state);
            return Ok(());
        }
    }
    pub(crate) fn require_service(
        &self,
        service: &crate::algo::ServiceMemory,
        bytes: u64,
    ) -> Result<()> {
        loop {
            let epoch = self.reclaim_epoch.load(Ordering::Acquire);
            match self.require_service_once(service, bytes) {
                Ok(()) => return Ok(()),
                Err(Error::BudgetExceeded(_)) if self.reclaim_idle_prefetch_since(epoch) => {}
                Err(error) => return Err(error),
            }
        }
    }
    fn require_service_once(&self, service: &crate::algo::ServiceMemory, bytes: u64) -> Result<()> {
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
            let release = self.release_locked(&mut state, service.owner.id, bytes);
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
            // a LIMIT over a scan reads it directly, while one over a path pulls the
            // path's cursor, whose batches the ledger charges (item 827)
            "SELECT ?p ?x WHERE { ?p ex:knows ?x } LIMIT 500",
            "SELECT ?p ?x WHERE { ?p ex:knows+ ?x }",
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
    /// A LIMIT over a path pulls the path's cursor, which takes the query's ledger, and
    /// a long narrow walk then charges a step at a time and checks its output each
    /// time it doubles, rather than taking the ledger's lock several times per level.
    #[test]
    fn a_pulled_path_limit_takes_the_ledger_once_per_step() {
        use crate::io::{RdfFormat, Source};
        use crate::sparql::{QueryOptions, query};
        let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
        // one cycle of 300 people, so every walk is 300 levels of one node
        for i in 0..300 {
            data.push_str(&format!("ex:p{i} ex:knows ex:p{} .\n", (i + 1) % 300));
        }
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let opts = QueryOptions {
            prefixes: vec![("ex".into(), "http://ex.org/".into())],
            pool: Some(QueryPool::new(64 * MB)),
            max_memory_bytes: Some(32 * MB),
            no_cache: true,
            ..Default::default()
        };
        let before = LEDGER_OPS.with(|n| n.get());
        let r = query(
            store.snapshot(),
            "SELECT ?p ?x WHERE { ?p ex:knows+ ?x } LIMIT 500",
            &opts,
        )
        .unwrap();
        let ops = LEDGER_OPS.with(|n| n.get()) - before;
        assert_eq!(r.len(), 500);
        // two walks of 300 levels took about 3,000 operations with several per level
        assert!(ops < 300, "{ops} ledger operations");
    }
    /// Decoding the rows of a pulled LIMIT charges its terms in steps rather than one
    /// ledger operation per term.
    #[test]
    fn decoded_rows_of_a_pulled_limit_take_the_ledger_in_steps() {
        use crate::io::{RdfFormat, Source};
        use crate::sparql::{QueryOptions, query};
        let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
        // more people than a cursor batch, so the LIMIT pulls its join
        for i in 0..5000 {
            data.push_str(&format!(
                "ex:p{i} ex:knows ex:p{} ; ex:name \"n{i}\" .\n",
                (i * 7 + 1) % 5000
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
        let opts = QueryOptions {
            prefixes: vec![("ex".into(), "http://ex.org/".into())],
            pool: Some(QueryPool::new(64 * MB)),
            max_memory_bytes: Some(32 * MB),
            no_cache: true,
            ..Default::default()
        };
        let r = query(
            store.snapshot(),
            "SELECT ?p ?n WHERE { ?p ex:knows ?x . ?x ex:name ?n } LIMIT 500",
            &opts,
        )
        .unwrap();
        assert_eq!(r.len(), 500);
        let before = LEDGER_OPS.with(|n| n.get());
        let rows = r.rows().unwrap();
        let ops = LEDGER_OPS.with(|n| n.get()) - before;
        assert_eq!(rows.len(), 500);
        // a charge per term took about 4,000 operations
        assert!(ops < 100, "{ops} ledger operations");
    }

    /// A FILTER over a product without a key, under an ASK, reads its input in batches
    /// that grow while no row passes, rather than one pair at a time with the ledger
    /// operations of a batch each.
    #[test]
    fn a_filter_whose_batches_come_back_empty_reads_larger_ones() {
        use crate::io::{RdfFormat, Source};
        use crate::sparql::{QueryOptions, query};
        let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
        for i in 0..300 {
            data.push_str(&format!("ex:p{i} ex:age {} .\n", i % 90));
        }
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let opts = QueryOptions {
            prefixes: vec![("ex".into(), "http://ex.org/".into())],
            no_cache: true,
            ..Default::default()
        };
        let before = LEDGER_OPS.with(|n| n.get());
        let r = query(
            store.snapshot(),
            "ASK { ?a ex:age ?x . ?b ex:age ?y FILTER(?x + ?y > 1000) }",
            &opts,
        )
        .unwrap();
        let ops = LEDGER_OPS.with(|n| n.get()) - before;
        assert!(!r.boolean);
        // one pair at a time took several operations for each of the 90,000 pairs
        assert!(ops < 5_000, "{ops} ledger operations");
    }

    /// A FILTER that reads ahead checks its grown demand against the row limit before
    /// each read, so it answers where reading at its consumer's demand answers (review
    /// round 8).
    #[test]
    fn a_grown_filter_demand_that_no_longer_fits_the_row_limit_shrinks() {
        use crate::sparql::{QueryOptions, query};
        let store = Store::in_memory(StoreOptions::default());
        let values: Vec<String> = (0..64).map(|i| i.to_string()).collect();
        let text = format!(
            "SELECT ?x WHERE {{ VALUES ?x {{ {} }} FILTER(?x >= 21) }} LIMIT 1",
            values.join(" ")
        );
        let opts = QueryOptions {
            max_rows: Some(32),
            no_cache: true,
            ..Default::default()
        };
        let r = query(store.snapshot(), &text, &opts).unwrap();
        assert_eq!(r.len(), 1);
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

    #[test]
    fn active_prefetch_is_not_reclaimed_and_idle_reclaim_releases_upstream_outside_lock() {
        let used = Arc::new(AtomicU64::new(0));
        let limit = 16_384u64;
        let callback_ledger =
            Arc::new(std::sync::OnceLock::<std::sync::Weak<QueryMemoryLedger>>::new());
        let take = used.clone();
        let give = used.clone();
        let inspect = callback_ledger.clone();
        let parent = MemoryDomain::unpooled().parent(
            move |n| {
                let mut cur = take.load(Ordering::Relaxed);
                loop {
                    let next = cur.checked_add(n).ok_or(ParentRefused::Overflow)?;
                    if next > limit {
                        return Err(ParentRefused::Limit {
                            limit,
                            requested: next,
                            scope: Scope::Work,
                        });
                    }
                    match take.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
                    {
                        Ok(_) => return Ok(()),
                        Err(actual) => cur = actual,
                    }
                }
            },
            move |n| {
                if n != 0 {
                    let ledger = inspect.get().unwrap().upgrade().unwrap();
                    assert!(
                        ledger.state.try_lock().is_some(),
                        "reclaim upstream return must run outside ledger lock"
                    );
                }
                give.fetch_sub(n, Ordering::AcqRel);
            },
        );
        let ledger = QueryMemoryLedger::nested(None, parent);
        callback_ledger.set(Arc::downgrade(&ledger)).unwrap();
        ledger.cursor_mode().unwrap();
        let owner = ledger.owner(None).unwrap();

        // Pin active optional prefetch for the entire parent budget.
        ledger.admit_prefetch(None, 1 << 20, limit).unwrap();
        assert_eq!(ledger.live.load(Ordering::Relaxed), limit);
        assert_eq!(used.load(Ordering::Relaxed), limit);

        // While prefetch is active, required charges must not reclaim it.
        assert!(matches!(
            ledger.reserve(&owner, 1 << 20, 4096, false),
            Err(Error::BudgetExceeded(_))
        ));
        assert!(matches!(
            ledger.reserve_receiving(&owner, 1 << 20, 4096),
            Err(Error::BudgetExceeded(_))
        ));

        // Once prefetch ends, the idle optional reservation is reclaimed—including
        // returning its upstream lease backing outside the ledger lock—so a required
        // receiving reservation succeeds under the tight parent budget.
        ledger.end_prefetch();
        ledger.reserve_receiving(&owner, 1 << 20, limit).unwrap();
        assert_eq!(ledger.live.load(Ordering::Relaxed), limit);
        assert_eq!(used.load(Ordering::Relaxed), limit);
        assert_eq!(ledger.envelope_reserved(), 0);

        ledger.release_receiving(owner.id, limit);
        assert_eq!(ledger.live.load(Ordering::Relaxed), 0);
        assert_eq!(used.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn release_decode_returns_optional_prefetch_backing_under_capped_parent_and_grant() {
        // 1. Capped upstream parent: admit full cap as optional prefetch, end prefetch,
        // release_decode, then reserve_receiving(1) and reserve_receiving(limit - 1).
        let used = Arc::new(AtomicU64::new(0));
        let limit = 16_384u64;
        let callback_ledger =
            Arc::new(std::sync::OnceLock::<std::sync::Weak<QueryMemoryLedger>>::new());
        let take = used.clone();
        let give = used.clone();
        let inspect = callback_ledger.clone();
        let parent = MemoryDomain::unpooled().parent(
            move |n| {
                let mut cur = take.load(Ordering::Relaxed);
                loop {
                    let next = cur.checked_add(n).ok_or(ParentRefused::Overflow)?;
                    if next > limit {
                        return Err(ParentRefused::Limit {
                            limit,
                            requested: next,
                            scope: Scope::Work,
                        });
                    }
                    match take.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
                    {
                        Ok(_) => return Ok(()),
                        Err(actual) => cur = actual,
                    }
                }
            },
            move |n| {
                if n != 0 {
                    let ledger = inspect.get().unwrap().upgrade().unwrap();
                    assert!(
                        ledger.state.try_lock().is_some(),
                        "teardown upstream return must run outside ledger lock"
                    );
                }
                give.fetch_sub(n, Ordering::AcqRel);
            },
        );
        let ledger = QueryMemoryLedger::nested(None, parent);
        callback_ledger.set(Arc::downgrade(&ledger)).unwrap();
        ledger.cursor_mode().unwrap();
        let owner = ledger.owner(None).unwrap();

        ledger.admit_prefetch(None, 1 << 20, limit).unwrap();
        assert_eq!(ledger.live.load(Ordering::Relaxed), limit);
        assert_eq!(used.load(Ordering::Relaxed), limit);
        ledger.end_prefetch();

        // Cursor teardown must return optional prefetch backing, not strand it behind
        // LIVE_SLACK after clearing the prefetch accounting record.
        ledger.release_decode();
        assert_eq!(ledger.live.load(Ordering::Relaxed), 0);
        assert_eq!(ledger.envelope_reserved(), 0);
        assert_eq!(used.load(Ordering::Relaxed), 0);

        ledger.reserve_receiving(&owner, 1 << 20, 1).unwrap();
        ledger
            .reserve_receiving(&owner, 1 << 20, limit - 1)
            .unwrap();
        assert_eq!(ledger.live.load(Ordering::Relaxed), limit);
        assert_eq!(used.load(Ordering::Relaxed), limit);
        ledger.release_receiving(owner.id, limit);
        assert_eq!(ledger.live.load(Ordering::Relaxed), 0);
        assert_eq!(used.load(Ordering::Relaxed), 0);

        // Mixed decode + optional prefetch on the same account also returns all excess
        // backing during teardown.
        ledger.reserve(&owner, 1 << 20, 4096, true).unwrap();
        ledger.admit_prefetch(None, 1 << 20, limit).unwrap();
        ledger.end_prefetch();
        ledger.release_decode();
        assert_eq!(ledger.live.load(Ordering::Relaxed), 0);
        assert_eq!(used.load(Ordering::Relaxed), 0);
        ledger.reserve_receiving(&owner, 1 << 20, limit).unwrap();
        ledger.release_receiving(owner.id, limit);
        assert_eq!(used.load(Ordering::Relaxed), 0);

        // 2. Grant-backed pool: optional prefetch admitted up to pool capacity is
        // returned to the grant on release_decode so a subsequent exact receiving
        // reservation succeeds.
        let pool = QueryPool::new(limit);
        let grant = Arc::new(pool.empty_grant());
        let grant_ledger = QueryMemoryLedger::seeded(Some(grant.clone()), 0, 0, 0);
        grant_ledger.cursor_mode().unwrap();
        let grant_owner = grant_ledger.owner(Some(&grant)).unwrap();
        grant_ledger
            .admit_prefetch(Some(&grant), 1 << 20, limit)
            .unwrap();
        assert_eq!(grant.used(), limit);
        grant_ledger.end_prefetch();
        grant_ledger.release_decode();
        assert_eq!(grant_ledger.live.load(Ordering::Relaxed), 0);
        assert_eq!(grant.used(), 0);
        grant_ledger
            .reserve_receiving(&grant_owner, 1 << 20, 1)
            .unwrap();
        grant_ledger.release_receiving(grant_owner.id, 1);
        assert_eq!(grant.used(), 0);
    }

    fn recv_bounded<T>(rx: &std::sync::mpsc::Receiver<T>, label: &str) -> T {
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap_or_else(|err| panic!("{label}: {err}"))
    }

    /// Wait with a bounded deadline until at least one caller is parked inside
    /// `ledger.reclaim_done.wait`. Because `reclaim_waiters` is incremented while
    /// holding `ledger.state` immediately before `reclaim_done.wait` atomically
    /// enqueues and unlocks `ledger.state`, observing `reclaim_waiters > 0` and
    /// then acquiring and dropping `ledger.state` guarantees the waiter is parked
    /// on the condition variable.
    fn wait_for_parked_reclaim_waiter(ledger: &QueryMemoryLedger) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while ledger.reclaim_waiters.load(Ordering::Acquire) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for caller to enter reclaim_done.wait"
            );
            std::thread::yield_now();
        }
        let guard = ledger
            .state
            .try_lock_until(deadline)
            .expect("timed out acquiring ledger.state after observing reclaim waiter");
        drop(guard);
    }

    /// Wait with a bounded deadline until at least one caller is parked inside
    /// `ledger.upstream_done.wait`.
    fn wait_for_parked_upstream_waiter(ledger: &QueryMemoryLedger) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while ledger.upstream_waiters.load(Ordering::Acquire) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for caller to enter upstream_done.wait"
            );
            std::thread::yield_now();
        }
        let guard = ledger
            .state
            .try_lock_until(deadline)
            .expect("timed out acquiring ledger.state after observing upstream waiter");
        drop(guard);
    }

    #[test]
    fn concurrent_required_reservation_waits_on_condvar_for_in_flight_reclaim() {
        use std::sync::mpsc;

        // Caller 1 detaches 16 KiB of idle prefetch under a 16 KiB capped parent and
        // pauses inside the parent's return callback (outside `ledger.state`, with
        // `prefetch_bytes == 0` and `reclaim_in_flight == 1`). Caller 2 runs
        // `reserve_receiving(4096)` on its own worker thread, fails against the
        // detached credit, and parks in `reclaim_done.wait`. Caller 1 waits until
        // Caller 2 is confirmed parked on the condition variable before completing
        // the backing return. Both workers report completion over bounded channels so
        // a lost notification fails promptly instead of hanging.
        let used = Arc::new(AtomicU64::new(0));
        let limit = 16_384u64;
        let callback_ledger =
            Arc::new(std::sync::OnceLock::<std::sync::Weak<QueryMemoryLedger>>::new());
        let (in_flight_tx, in_flight_rx) = mpsc::channel::<()>();
        let in_flight_tx = Arc::new(Mutex::new(Some(in_flight_tx)));

        let take_used = used.clone();
        let give_used = used.clone();
        let give_inspect = callback_ledger.clone();
        let give_in_flight_tx = in_flight_tx.clone();

        let parent = MemoryDomain::unpooled().parent(
            move |n| {
                let mut cur = take_used.load(Ordering::Relaxed);
                loop {
                    let next = cur.checked_add(n).ok_or(ParentRefused::Overflow)?;
                    if next > limit {
                        return Err(ParentRefused::Limit {
                            limit,
                            requested: next,
                            scope: Scope::Work,
                        });
                    }
                    match take_used.compare_exchange_weak(
                        cur,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return Ok(()),
                        Err(actual) => cur = actual,
                    }
                }
            },
            move |n| {
                if n != 0 {
                    let ledger = give_inspect.get().unwrap().upgrade().unwrap();
                    assert!(
                        ledger.state.try_lock().is_some(),
                        "reclaim upstream return must run outside ledger lock"
                    );
                    if let Some(tx) = give_in_flight_tx.lock().take() {
                        tx.send(()).unwrap();
                        wait_for_parked_reclaim_waiter(&ledger);
                    }
                }
                give_used.fetch_sub(n, Ordering::AcqRel);
            },
        );
        let ledger = QueryMemoryLedger::nested(None, parent);
        callback_ledger.set(Arc::downgrade(&ledger)).unwrap();
        ledger.cursor_mode().unwrap();
        let owner = ledger.owner(None).unwrap();

        ledger.admit_prefetch(None, 1 << 20, limit).unwrap();
        ledger.end_prefetch();

        let (r1_tx, r1_rx) = mpsc::channel();
        let (r2_tx, r2_rx) = mpsc::channel();
        let l1 = ledger.clone();
        let o1 = owner.clone();
        let t1 = std::thread::spawn(move || {
            let _ = r1_tx.send(l1.reserve_receiving(&o1, 1 << 20, 4096));
        });

        recv_bounded(
            &in_flight_rx,
            "caller 1 did not enter parent return callback",
        );
        assert_eq!(ledger.prefetch_bytes.load(Ordering::Acquire), 0);

        let l2 = ledger.clone();
        let o2 = owner.clone();
        let t2 = std::thread::spawn(move || {
            let _ = r2_tx.send(l2.reserve_receiving(&o2, 1 << 20, 4096));
        });

        let r1 = recv_bounded(&r1_rx, "caller 1 did not finish within deadline");
        let r2 = recv_bounded(&r2_rx, "caller 2 did not wake from reclaim_done");
        t1.join().unwrap();
        t2.join().unwrap();
        r1.expect("caller 1 must succeed after reclaiming idle prefetch");
        r2.expect("caller 2 must wake from reclaim_done and succeed");
        assert_eq!(
            ledger.reclaim_waits_total.load(Ordering::Acquire),
            1,
            "caller 2 must exercise the condition-variable wait branch"
        );
        assert_eq!(ledger.live.load(Ordering::Relaxed), 8192);
        assert_eq!(used.load(Ordering::Relaxed), 8192);
        ledger.release_receiving(owner.id, 8192);
        assert_eq!(ledger.live.load(Ordering::Relaxed), 0);
        assert_eq!(used.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn concurrent_required_reservation_retries_after_completed_reclaim_epoch() {
        use std::sync::atomic::AtomicBool;
        use std::sync::mpsc;
        use std::time::Duration;

        // Part 1: Deterministic parent-backed completed-epoch retry without waiting on
        // the condition variable. Caller 2 starts `reserve_receiving(4096)` while 16 KiB
        // idle prefetch fills the parent cap. Inside Caller 2's first parent refusal
        // callback (with no ledger lock held), Caller 2 pauses while Caller 1 completes
        // reclaim and advances `reclaim_epoch` to 1 with `reclaim_in_flight == 0`.
        // Caller 2 then observes `prefetch == 0 && in_flight == 0 && epoch != epoch_before`
        // and succeeds on retry with zero condvar waits.
        let used = Arc::new(AtomicU64::new(0));
        let limit = 16_384u64;
        let (caller2_refused_tx, caller2_refused_rx) = mpsc::channel::<()>();
        let (reclaim_finished_tx, reclaim_finished_rx) = mpsc::channel::<()>();
        let caller2_refused_tx = Arc::new(Mutex::new(Some(caller2_refused_tx)));
        let reclaim_finished_rx = Arc::new(Mutex::new(Some(reclaim_finished_rx)));
        let armed = Arc::new(AtomicBool::new(false));
        let paused_once = Arc::new(AtomicBool::new(false));

        let take_used = used.clone();
        let take_armed = armed.clone();
        let take_paused = paused_once.clone();
        let take_refused_tx = caller2_refused_tx.clone();
        let take_finished_rx = reclaim_finished_rx.clone();
        let give_used = used.clone();

        let parent = MemoryDomain::unpooled().parent(
            move |n| {
                let mut cur = take_used.load(Ordering::Relaxed);
                loop {
                    let next = cur.checked_add(n).ok_or(ParentRefused::Overflow)?;
                    if next > limit {
                        if take_armed.load(Ordering::Acquire)
                            && !take_paused.swap(true, Ordering::AcqRel)
                        {
                            let tx = take_refused_tx.lock().take().unwrap();
                            tx.send(()).unwrap();
                            let rx = take_finished_rx.lock().take().unwrap();
                            rx.recv_timeout(Duration::from_secs(5))
                                .expect("caller 1 did not finish reclaim before timeout");
                        }
                        return Err(ParentRefused::Limit {
                            limit,
                            requested: next,
                            scope: Scope::Work,
                        });
                    }
                    match take_used.compare_exchange_weak(
                        cur,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return Ok(()),
                        Err(actual) => cur = actual,
                    }
                }
            },
            move |n| {
                give_used.fetch_sub(n, Ordering::AcqRel);
            },
        );
        let ledger = QueryMemoryLedger::nested(None, parent);
        ledger.cursor_mode().unwrap();
        let owner = ledger.owner(None).unwrap();

        ledger.admit_prefetch(None, 1 << 20, limit).unwrap();
        ledger.end_prefetch();
        armed.store(true, Ordering::Release);

        let (r2_tx, r2_rx) = mpsc::channel();
        let l2 = ledger.clone();
        let o2 = owner.clone();
        let t2 = std::thread::spawn(move || {
            let _ = r2_tx.send(l2.reserve_receiving(&o2, 1 << 20, 4096));
        });

        recv_bounded(
            &caller2_refused_rx,
            "caller 2 did not reach parent refusal callback",
        );
        // Caller 2 has not yet returned from its first refusal and holds no ledger
        // lock; Caller 1 reclaims all idle prefetch to completion before Caller 2
        // reaches `reclaim_idle_prefetch_since`.
        ledger
            .reserve_receiving(&owner, 1 << 20, 4096)
            .expect("caller 1 must reclaim idle prefetch and succeed");
        assert_eq!(ledger.prefetch_bytes.load(Ordering::Acquire), 0);
        assert_eq!(ledger.reclaim_in_flight.load(Ordering::Acquire), 0);
        assert_eq!(ledger.reclaim_epoch.load(Ordering::Acquire), 1);

        reclaim_finished_tx.send(()).unwrap();
        let r2 = recv_bounded(&r2_rx, "caller 2 did not finish completed-epoch retry");
        t2.join().unwrap();
        r2.expect("caller 2 must retry via completed epoch and succeed");
        assert_eq!(
            ledger.reclaim_waits_total.load(Ordering::Acquire),
            0,
            "completed-epoch retry must not sleep on reclaim_done"
        );
        assert_eq!(ledger.reclaim_epoch.load(Ordering::Acquire), 1);
        assert_eq!(ledger.live.load(Ordering::Relaxed), 8192);
        assert_eq!(used.load(Ordering::Relaxed), 8192);
        ledger.release_receiving(owner.id, 8192);
        assert_eq!(ledger.live.load(Ordering::Relaxed), 0);
        assert_eq!(used.load(Ordering::Relaxed), 0);

        // Part 2: Deterministic unbacked local-limit completed-epoch retry.
        // Caller 2 snapshots `epoch_before` and fails its initial local admission while
        // idle prefetch fills the local limit. Caller 1 completes reclaim (incrementing
        // `reclaim_epoch` without any in-flight backing release). Caller 2's reclaim
        // check observes the completed epoch and retries successfully.
        let local_limit = 16_384u64;
        let local = QueryMemoryLedger::seeded(None, 0, 0, 0);
        local.cursor_mode().unwrap();
        let local_owner = local.owner(None).unwrap();
        local
            .admit_prefetch(None, local_limit, local_limit)
            .unwrap();
        local.end_prefetch();

        let epoch_before = local.reclaim_epoch.load(Ordering::Acquire);
        assert!(matches!(
            local.reserve_ordinary_once(&local_owner, local_limit, 4096),
            Err(Error::BudgetExceeded(_))
        ));
        local
            .reserve(&local_owner, local_limit, 4096, false)
            .expect("caller 1 must reclaim local idle prefetch and succeed");
        assert_eq!(local.prefetch_bytes.load(Ordering::Acquire), 0);
        assert_eq!(local.reclaim_in_flight.load(Ordering::Acquire), 0);
        assert_eq!(
            local.reclaim_epoch.load(Ordering::Acquire),
            epoch_before + 1
        );
        assert!(
            local.reclaim_idle_prefetch_since(epoch_before),
            "caller 2 must observe the completed local reclaim epoch"
        );
        assert_eq!(local.reclaim_waits_total.load(Ordering::Acquire), 0);
        local
            .reserve_ordinary_once(&local_owner, local_limit, 4096)
            .expect("caller 2 retry must succeed after local reclaim");
        assert_eq!(local.live.load(Ordering::Relaxed), 8192);
        local.release(local_owner.id, 8192);
        assert_eq!(local.live.load(Ordering::Relaxed), 0);

        // Part 3: A parent return callback that re-enters the ledger with an
        // unsatisfiable required charge must return BudgetExceeded without deadlocking
        // on its own in-flight reclaim.
        let re_used = Arc::new(AtomicU64::new(0));
        let re_ledger_cell =
            Arc::new(std::sync::OnceLock::<std::sync::Weak<QueryMemoryLedger>>::new());
        let re_owner_cell = Arc::new(std::sync::OnceLock::<Arc<MemoryOwner>>::new());
        let re_checked = Arc::new(AtomicBool::new(false));
        let re_take = re_used.clone();
        let re_give = re_used.clone();
        let re_inspect = re_ledger_cell.clone();
        let re_owner_ref = re_owner_cell.clone();
        let re_flag = re_checked.clone();
        let re_parent = MemoryDomain::unpooled().parent(
            move |n| {
                let mut cur = re_take.load(Ordering::Relaxed);
                loop {
                    let next = cur.checked_add(n).ok_or(ParentRefused::Overflow)?;
                    if next > limit {
                        return Err(ParentRefused::Limit {
                            limit,
                            requested: next,
                            scope: Scope::Work,
                        });
                    }
                    match re_take.compare_exchange_weak(
                        cur,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return Ok(()),
                        Err(actual) => cur = actual,
                    }
                }
            },
            move |n| {
                re_give.fetch_sub(n, Ordering::AcqRel);
                if n != 0 && !re_flag.swap(true, Ordering::AcqRel) {
                    let ledger = re_inspect.get().unwrap().upgrade().unwrap();
                    assert!(
                        ledger.state.try_lock().is_some(),
                        "same-ledger return callback must run outside ledger lock"
                    );
                    assert_eq!(
                        ledger.reclaim_in_flight.load(Ordering::Acquire),
                        1,
                        "tracked prefetch release must still be in flight during callback"
                    );
                    let owner = re_owner_ref.get().unwrap();
                    assert!(matches!(
                        ledger.reserve(owner, 1 << 20, limit * 2, false),
                        Err(Error::BudgetExceeded(_))
                    ));
                    assert_eq!(
                        ledger.reclaim_waits_total.load(Ordering::Acquire),
                        0,
                        "same-ledger callback must bypass waiting on its own reclaim_done"
                    );
                }
            },
        );
        let re_ledger = QueryMemoryLedger::nested(None, re_parent);
        re_ledger_cell.set(Arc::downgrade(&re_ledger)).unwrap();
        re_ledger.cursor_mode().unwrap();
        let re_owner = re_ledger.owner(None).unwrap();
        assert!(re_owner_cell.set(re_owner.clone()).is_ok());
        re_ledger.admit_prefetch(None, 1 << 20, limit).unwrap();
        re_ledger.end_prefetch();
        let (re_tx, re_rx) = mpsc::channel();
        let re_l = re_ledger.clone();
        let re_o = re_owner.clone();
        let re_thread = std::thread::spawn(move || {
            let _ = re_tx.send(re_l.reserve_receiving(&re_o, 1 << 20, limit));
        });
        let re_res = recv_bounded(
            &re_rx,
            "same-ledger callback hung waiting on its own in-flight reclaim",
        );
        re_thread.join().unwrap();
        re_res.expect("same-ledger reclaim callback must return without self-deadlock");
        assert!(re_checked.load(Ordering::Acquire));
        assert_eq!(
            re_ledger.reclaim_waits_total.load(Ordering::Acquire),
            0,
            "same-ledger callback must record zero reclaim_done waits"
        );
        re_ledger.release_receiving(re_owner.id, limit);
        assert_eq!(re_used.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn nested_admission_does_not_deadlock_with_outer_reclaim_waker() {
        use std::sync::atomic::AtomicBool;
        use std::sync::mpsc;

        // Part 1: Two-thread coordinated P1 regression.
        // `outer` holds 48 MiB of idle grant-backed prefetch (exceeding SHRINK_SLACK +
        // GRANULE so release trims granules and wakes the pool waiter). Thread A
        // detaches outer's prefetch and enters the pool waker while `reclaim_in_flight`
        // is still 1. Thread B attempts a nested receiving charge exceeding `outer`'s
        // remaining local limit; `nested` must drop its own lock before calling into
        // `outer`, so when Thread B parks on `outer.reclaim_done`, Thread A's waker can
        // reserve through `nested` without deadlocking and then wake Thread B.
        let pool = QueryPool::new(64 * MB);
        let outer_grant = Arc::new(pool.empty_grant());
        let outer_limit = 48 * MB;
        let outer = QueryMemoryLedger::seeded(Some(outer_grant.clone()), 0, 0, 0);
        outer.cursor_mode().unwrap();
        let outer_owner = outer.owner(Some(&outer_grant)).unwrap();
        let nested = QueryMemoryLedger::nested(
            Some(outer_grant.clone()),
            outer
                .child_parent(Some(outer_grant.clone()), outer_limit)
                .unwrap(),
        );
        let nested_owner = nested.owner(Some(&outer_grant)).unwrap();
        let nested_callback = nested
            .child_parent(Some(outer_grant.clone()), 64 * MB)
            .unwrap();

        outer
            .admit_prefetch(Some(&outer_grant), outer_limit, outer_limit)
            .unwrap();
        outer.end_prefetch();

        let (waker_entered_tx, waker_entered_rx) = mpsc::channel::<()>();
        let waker_entered_tx = Arc::new(Mutex::new(Some(waker_entered_tx)));
        let waker_ran = Arc::new(AtomicUsize::new(0));
        let coordinated_once = Arc::new(AtomicBool::new(false));
        let waker_outer = Arc::downgrade(&outer);
        let waker_nested = Arc::downgrade(&nested);
        let waker_entered_tx_cb = waker_entered_tx.clone();
        let waker_ran_cb = waker_ran.clone();
        let coordinated_once_cb = coordinated_once.clone();

        let waiter = pool.enter(
            32 * MB,
            Some(Arc::new(move || {
                if !coordinated_once_cb.swap(true, Ordering::AcqRel) {
                    if let Some(tx) = waker_entered_tx_cb.lock().take() {
                        tx.send(()).unwrap();
                    }
                    let outer = waker_outer.upgrade().expect("outer ledger live");
                    wait_for_parked_reclaim_waiter(&outer);
                    let nested = waker_nested.upgrade().expect("nested ledger live");
                    assert!(
                        nested.state.try_lock().is_some(),
                        "nested ledger lock must not be held while waiting on outer reclaim"
                    );
                }
                drop(nested_callback.clone().reserve(1).unwrap());
                waker_ran_cb.fetch_add(1, Ordering::SeqCst);
            })),
        );

        let (res_a_tx, res_a_rx) = mpsc::channel();
        let (res_b_tx, res_b_rx) = mpsc::channel();
        let outer_a = outer.clone();
        let outer_owner_a = outer_owner.clone();
        let thread_a = std::thread::spawn(move || {
            let _ = res_a_tx.send(outer_a.reserve_receiving(&outer_owner_a, outer_limit, 4096));
        });

        recv_bounded(&waker_entered_rx, "outer reclaim did not invoke pool waker");

        let nested_b = nested.clone();
        let nested_owner_b = nested_owner.clone();
        let thread_b = std::thread::spawn(move || {
            let _ = res_b_tx.send(nested_b.reserve_receiving(
                &nested_owner_b,
                64 * MB,
                outer_limit + 1,
            ));
        });

        let res_a = recv_bounded(&res_a_rx, "thread A did not finish outer reclaim");
        let res_b = recv_bounded(&res_b_rx, "thread B did not wake from outer reclaim");
        thread_a.join().unwrap();
        thread_b.join().unwrap();
        res_a.expect("thread A must finish outer reclaim and reserve");
        assert!(
            matches!(
                res_b,
                Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory
            ),
            "thread B must wake from outer reclaim and receive local memory refusal: {res_b:?}"
        );
        assert!(waker_ran.load(Ordering::SeqCst) > 0);
        assert!(
            outer.reclaim_waits_total.load(Ordering::Acquire) >= 1,
            "thread B must have waited on outer.reclaim_done while thread A ran the waker"
        );
        outer.release_receiving(outer_owner.id, 4096);
        drop(waiter);
        drop(nested_owner);
        drop(nested);
        drop(outer_owner);
        drop(outer);
        drop(outer_grant);
        assert_eq!(pool.granted(), 0);

        // Part 2: Single-thread nested admissions triggering outer prefetch reclaim
        // whose pool waker synchronously re-enters `nested` must not self-deadlock
        // across receiving, ordinary, decode, output, or prefetch admission paths.
        // Probe `nested.state.try_lock()` before re-entering so a lock-ordering
        // regression fails immediately instead of self-deadlocking.
        for mode in 0..5 {
            let pool = QueryPool::new(64 * MB);
            let outer_grant = Arc::new(pool.empty_grant());
            let outer_limit = 48 * MB;
            let outer = QueryMemoryLedger::seeded(Some(outer_grant.clone()), 0, 0, 0);
            outer.cursor_mode().unwrap();
            let nested = QueryMemoryLedger::nested(
                Some(outer_grant.clone()),
                outer
                    .child_parent(Some(outer_grant.clone()), outer_limit)
                    .unwrap(),
            );
            let nested_owner = nested.owner(Some(&outer_grant)).unwrap();
            let nested_callback = nested
                .child_parent(Some(outer_grant.clone()), 64 * MB)
                .unwrap();
            outer
                .admit_prefetch(Some(&outer_grant), outer_limit, outer_limit)
                .unwrap();
            outer.end_prefetch();

            let waker_nested = Arc::downgrade(&nested);
            let waker_ran = Arc::new(AtomicUsize::new(0));
            let waker_ran_cb = waker_ran.clone();
            let waiter = pool.enter(
                32 * MB,
                Some(Arc::new(move || {
                    let nested = waker_nested.upgrade().expect("nested ledger live");
                    assert!(
                        nested.state.try_lock().is_some(),
                        "mode {mode}: nested ledger lock must not be held during upstream admission"
                    );
                    drop(nested_callback.clone().reserve(1).unwrap());
                    waker_ran_cb.fetch_add(1, Ordering::SeqCst);
                })),
            );
            match mode {
                0 => {
                    nested
                        .reserve_receiving(&nested_owner, 64 * MB, 4096)
                        .unwrap();
                    nested.release_receiving(nested_owner.id, 4096);
                }
                1 => {
                    nested.reserve(&nested_owner, 64 * MB, 4096, false).unwrap();
                    nested.release(nested_owner.id, 4096);
                }
                2 => {
                    nested.reserve(&nested_owner, 64 * MB, 4096, true).unwrap();
                    nested.release_decode();
                }
                3 => {
                    nested.check_output(&nested_owner, 64 * MB, 4096).unwrap();
                }
                _ => {
                    nested
                        .admit_prefetch(Some(&outer_grant), 64 * MB, 4096)
                        .unwrap();
                    nested.end_prefetch();
                    nested.release_decode();
                }
            }
            assert!(waker_ran.load(Ordering::SeqCst) > 0, "mode {mode}");
            drop(waiter);
            drop(nested_owner);
            drop(nested);
            drop(outer);
            drop(outer_grant);
            assert_eq!(pool.granted(), 0, "mode {mode}");
        }
    }

    #[test]
    fn concurrent_nested_admissions_share_one_upstream_step() {
        use std::sync::mpsc;

        // Part 1: Capped outer child parent (`LIVE_STEP`). Hold Caller A between
        // upstream acquisition and local publication while Caller B requests one
        // ordinary byte. Both charges must succeed, B must wait on `upstream_done`,
        // and the parent must be charged only one `LIVE_STEP`.
        {
            let outer = QueryMemoryLedger::seeded(None, 0, 0, 0);
            let outer_parent = outer.child_parent(None, LIVE_STEP).unwrap();
            let held_leases: Arc<Mutex<Vec<MemoryLease>>> = Arc::new(Mutex::new(Vec::new()));
            let charge_count = Arc::new(AtomicUsize::new(0));
            let nested_cell =
                Arc::new(std::sync::OnceLock::<std::sync::Weak<QueryMemoryLedger>>::new());
            let (acquired_tx, acquired_rx) = mpsc::channel::<()>();
            let acquired_tx = Arc::new(Mutex::new(Some(acquired_tx)));

            let take_parent = outer_parent.clone();
            let take_leases = held_leases.clone();
            let take_count = charge_count.clone();
            let take_nested = nested_cell.clone();
            let take_acquired = acquired_tx.clone();
            let give_leases = held_leases.clone();

            let wrapper = MemoryDomain::unpooled().parent(
                move |n| {
                    let lease = take_parent.reserve(n)?;
                    take_leases.lock().push(lease);
                    let calls = take_count.fetch_add(1, Ordering::AcqRel);
                    if calls == 0
                        && let Some(tx) = take_acquired.lock().take()
                    {
                        tx.send(()).unwrap();
                        let nested = take_nested.get().unwrap().upgrade().unwrap();
                        wait_for_parked_upstream_waiter(&nested);
                    }
                    Ok(())
                },
                move |mut n| {
                    let mut leases = give_leases.lock();
                    while n > 0 {
                        let last = leases.last_mut().expect("tracked upstream lease");
                        let take = n.min(last.bytes());
                        if take == last.bytes() {
                            drop(leases.pop());
                        } else {
                            let part = last.split(take).unwrap();
                            drop(part);
                        }
                        n -= take;
                    }
                },
            );

            let nested = QueryMemoryLedger::nested(None, wrapper);
            nested_cell.set(Arc::downgrade(&nested)).unwrap();
            let owner = nested.owner(None).unwrap();

            let (ra_tx, ra_rx) = mpsc::channel();
            let (rb_tx, rb_rx) = mpsc::channel();
            let na = nested.clone();
            let oa = owner.clone();
            let ta = std::thread::spawn(move || {
                let _ = ra_tx.send(na.reserve(&oa, 1 << 20, 1, false));
            });

            recv_bounded(
                &acquired_rx,
                "caller A did not acquire upstream LIVE_STEP credit",
            );
            assert_eq!(outer.live.load(Ordering::Acquire), LIVE_STEP);

            let nb = nested.clone();
            let ob = owner.clone();
            let tb = std::thread::spawn(move || {
                let _ = rb_tx.send(nb.reserve(&ob, 1 << 20, 1, false));
            });

            let ra = recv_bounded(&ra_rx, "caller A did not publish staged upstream credit");
            let rb = recv_bounded(&rb_rx, "caller B did not reuse published upstream credit");
            ta.join().unwrap();
            tb.join().unwrap();
            ra.expect("caller A 1-byte reservation must succeed");
            rb.expect("caller B 1-byte reservation must succeed within A's LIVE_STEP");
            assert_eq!(
                charge_count.load(Ordering::Acquire),
                1,
                "parent must be charged only one LIVE_STEP"
            );
            assert!(
                nested.upstream_waits_total.load(Ordering::Acquire) >= 1,
                "caller B must coordinate on upstream_done"
            );
            assert_eq!(outer.live.load(Ordering::Acquire), LIVE_STEP);
            assert_eq!(nested.live.load(Ordering::Acquire), 2);
            nested.release(owner.id, 2);
            drop(owner);
            drop(nested);
            assert_eq!(outer.live.load(Ordering::Acquire), 0);
        }

        // Part 2: Two concurrent `admit_prefetch` callers racing to fill the same
        // envelope target under a capped parent reuse one upstream growth and admit
        // the envelope once.
        {
            let target = 16_384u64;
            let outer = QueryMemoryLedger::seeded(None, 0, 0, 0);
            let outer_parent = outer.child_parent(None, LIVE_STEP).unwrap();
            let held_leases: Arc<Mutex<Vec<MemoryLease>>> = Arc::new(Mutex::new(Vec::new()));
            let charge_count = Arc::new(AtomicUsize::new(0));
            let nested_cell =
                Arc::new(std::sync::OnceLock::<std::sync::Weak<QueryMemoryLedger>>::new());
            let (acquired_tx, acquired_rx) = mpsc::channel::<()>();
            let acquired_tx = Arc::new(Mutex::new(Some(acquired_tx)));

            let take_parent = outer_parent.clone();
            let take_leases = held_leases.clone();
            let take_count = charge_count.clone();
            let take_nested = nested_cell.clone();
            let take_acquired = acquired_tx.clone();
            let give_leases = held_leases.clone();

            let wrapper = MemoryDomain::unpooled().parent(
                move |n| {
                    let lease = take_parent.reserve(n)?;
                    take_leases.lock().push(lease);
                    let calls = take_count.fetch_add(1, Ordering::AcqRel);
                    if calls == 0
                        && let Some(tx) = take_acquired.lock().take()
                    {
                        tx.send(()).unwrap();
                        let nested = take_nested.get().unwrap().upgrade().unwrap();
                        wait_for_parked_upstream_waiter(&nested);
                    }
                    Ok(())
                },
                move |mut n| {
                    let mut leases = give_leases.lock();
                    while n > 0 {
                        let last = leases.last_mut().expect("tracked upstream lease");
                        let take = n.min(last.bytes());
                        if take == last.bytes() {
                            drop(leases.pop());
                        } else {
                            let part = last.split(take).unwrap();
                            drop(part);
                        }
                        n -= take;
                    }
                },
            );

            let nested = QueryMemoryLedger::nested(None, wrapper);
            nested_cell.set(Arc::downgrade(&nested)).unwrap();
            nested.cursor_mode().unwrap();

            let (ra_tx, ra_rx) = mpsc::channel();
            let (rb_tx, rb_rx) = mpsc::channel();
            let na = nested.clone();
            let ta = std::thread::spawn(move || {
                let _ = ra_tx.send(na.admit_prefetch(None, 1 << 20, target));
            });

            recv_bounded(
                &acquired_rx,
                "prefetch caller A did not acquire upstream credit",
            );

            let nb = nested.clone();
            let tb = std::thread::spawn(move || {
                let _ = rb_tx.send(nb.admit_prefetch(None, 1 << 20, target));
            });

            let ra = recv_bounded(&ra_rx, "prefetch caller A did not finish");
            let rb = recv_bounded(&rb_rx, "prefetch caller B did not finish");
            ta.join().unwrap();
            tb.join().unwrap();
            ra.expect("prefetch caller A must succeed");
            rb.expect("prefetch caller B must reuse A's admitted envelope");
            assert_eq!(
                charge_count.load(Ordering::Acquire),
                1,
                "concurrent prefetch callers must charge parent only once"
            );
            assert!(nested.upstream_waits_total.load(Ordering::Acquire) >= 1);
            assert_eq!(nested.envelope_reserved(), target);
            assert_eq!(nested.live.load(Ordering::Acquire), target);
            nested.end_prefetch();
            nested.end_prefetch();
            nested.release_decode();
            assert_eq!(nested.live.load(Ordering::Acquire), 0);
            drop(nested);
            assert_eq!(outer.live.load(Ordering::Acquire), 0);
        }

        // Part 3: A callback caller that bypasses `upstream_done` waiting hits an
        // upstream refusal while Caller A's staged `LIVE_STEP` is being installed,
        // then revalidates against current `account.eager` and succeeds locally.
        {
            let outer = QueryMemoryLedger::seeded(None, 0, 0, 0);
            let outer_parent = outer.child_parent(None, LIVE_STEP).unwrap();
            let held_leases: Arc<Mutex<Vec<MemoryLease>>> = Arc::new(Mutex::new(Vec::new()));
            let ok_charges = Arc::new(AtomicUsize::new(0));
            let (a_acquired_tx, a_acquired_rx) = mpsc::channel::<()>();
            let (let_a_publish_tx, let_a_publish_rx) = mpsc::channel::<()>();
            let (a_published_tx, a_published_rx) = mpsc::channel::<()>();
            let a_acquired_tx = Arc::new(Mutex::new(Some(a_acquired_tx)));
            let let_a_publish_tx = Arc::new(Mutex::new(Some(let_a_publish_tx)));
            let let_a_publish_rx = Arc::new(Mutex::new(Some(let_a_publish_rx)));
            let a_published_rx = Arc::new(Mutex::new(Some(a_published_rx)));

            let take_parent = outer_parent.clone();
            let take_leases = held_leases.clone();
            let take_ok = ok_charges.clone();
            let take_a_acquired = a_acquired_tx.clone();
            let take_let_a_publish_tx = let_a_publish_tx.clone();
            let take_let_a_publish_rx = let_a_publish_rx.clone();
            let take_a_published_rx = a_published_rx.clone();
            let give_leases = held_leases.clone();

            let wrapper = MemoryDomain::unpooled().parent(
                move |n| match take_parent.reserve(n) {
                    Ok(lease) => {
                        take_leases.lock().push(lease);
                        let count = take_ok.fetch_add(1, Ordering::AcqRel);
                        if count == 0
                            && let Some(tx) = take_a_acquired.lock().take()
                        {
                            tx.send(()).unwrap();
                            let rx = take_let_a_publish_rx.lock().take().unwrap();
                            recv_bounded(&rx, "caller B did not release caller A to publish");
                        }
                        Ok(())
                    }
                    Err(err) => {
                        // On B's exact 1-byte fallback refusal, release A to publish
                        // its `LIVE_STEP` and wait until A completes before returning
                        // the refusal to B, exercising refusal revalidation.
                        if n == 1
                            && let Some(tx) = take_let_a_publish_tx.lock().take()
                        {
                            tx.send(()).unwrap();
                            let rx = take_a_published_rx.lock().take().unwrap();
                            recv_bounded(&rx, "caller A did not finish publishing LIVE_STEP");
                        }
                        Err(err)
                    }
                },
                move |mut n| {
                    let mut leases = give_leases.lock();
                    while n > 0 {
                        let last = leases.last_mut().expect("tracked upstream lease");
                        let take = n.min(last.bytes());
                        if take == last.bytes() {
                            drop(leases.pop());
                        } else {
                            let part = last.split(take).unwrap();
                            drop(part);
                        }
                        n -= take;
                    }
                },
            );

            let nested = QueryMemoryLedger::nested(None, wrapper);
            let owner = nested.owner(None).unwrap();

            let (ra_tx, ra_rx) = mpsc::channel();
            let (rb_tx, rb_rx) = mpsc::channel();
            let na = nested.clone();
            let oa = owner.clone();
            let ta = std::thread::spawn(move || {
                let res = na.reserve(&oa, 1 << 20, 1, false);
                let _ = a_published_tx.send(());
                let _ = ra_tx.send(res);
            });

            recv_bounded(
                &a_acquired_rx,
                "caller A did not acquire upstream credit in refusal-revalidation test",
            );

            let nb = nested.clone();
            let ob = owner.clone();
            let tb = std::thread::spawn(move || {
                // Run inside an active upstream callback scope so B skips
                // `upstream_done.wait` and hits the refusal-revalidation path.
                let _active = ActiveUpstreamOp::enter();
                let _ = rb_tx.send(nb.reserve(&ob, 1 << 20, 1, false));
            });

            let ra = recv_bounded(&ra_rx, "caller A did not finish");
            let rb = recv_bounded(&rb_rx, "caller B did not finish after refusal revalidation");
            ta.join().unwrap();
            tb.join().unwrap();
            ra.expect("caller A must succeed");
            rb.expect("caller B must succeed after revalidating refusal against A's LIVE_STEP");
            assert_eq!(ok_charges.load(Ordering::Acquire), 1);
            assert_eq!(nested.upstream_waits_total.load(Ordering::Acquire), 0);
            assert_eq!(outer.live.load(Ordering::Acquire), LIVE_STEP);
            assert_eq!(nested.live.load(Ordering::Acquire), 2);
            nested.release(owner.id, 2);
            drop(owner);
            drop(nested);
            assert_eq!(outer.live.load(Ordering::Acquire), 0);
        }

        // Part 4: Capped parent (`LIVE_STEP + 1`) with local cap (`LIVE_STEP`).
        // Normal caller A (2 bytes) and reentrant callback caller B (`LIVE_STEP`
        // bytes, bypassing wait via `ActiveUpstreamOp`) both plan growth before
        // an exact 1-byte receiving reservation commits (`live = 1, eager = 1`).
        // B acquires `LIVE_STEP` filling the parent and pauses until A's preferred
        // and 2-byte fallback requests both fail and A parks on `upstream_done`.
        // B then fails its local cap check, returns its staged `LIVE_STEP` credit
        // (leaving A's local delta unchanged at 2), and wakes A. A must revalidate
        // upstream capacity and succeed, while a subsequent stable parent refusal
        // still terminates with `BudgetExceeded`.
        {
            let outer = QueryMemoryLedger::seeded(None, 0, 0, 0);
            let outer_parent = outer.child_parent(None, LIVE_STEP + 1).unwrap();
            let held_leases: Arc<Mutex<Vec<MemoryLease>>> = Arc::new(Mutex::new(Vec::new()));
            let reserve_calls = Arc::new(AtomicUsize::new(0));
            let nested_cell =
                Arc::new(std::sync::OnceLock::<std::sync::Weak<QueryMemoryLedger>>::new());
            let (a_planned_tx, a_planned_rx) = mpsc::channel::<()>();
            let (a_go_tx, a_go_rx) = mpsc::channel::<()>();
            let (b_planned_tx, b_planned_rx) = mpsc::channel::<()>();
            let (b_acquire_tx, b_acquire_rx) = mpsc::channel::<()>();
            let (b_acquired_tx, b_acquired_rx) = mpsc::channel::<()>();
            let a_planned_tx = Arc::new(Mutex::new(Some(a_planned_tx)));
            let a_go_rx = Arc::new(Mutex::new(Some(a_go_rx)));
            let b_planned_tx = Arc::new(Mutex::new(Some(b_planned_tx)));
            let b_acquire_rx = Arc::new(Mutex::new(Some(b_acquire_rx)));
            let b_acquired_tx = Arc::new(Mutex::new(Some(b_acquired_tx)));

            let take_parent = outer_parent.clone();
            let take_leases = held_leases.clone();
            let take_calls = reserve_calls.clone();
            let take_nested = nested_cell.clone();
            let take_a_planned = a_planned_tx.clone();
            let take_a_go = a_go_rx.clone();
            let take_b_planned = b_planned_tx.clone();
            let take_b_acquire = b_acquire_rx.clone();
            let take_b_acquired = b_acquired_tx.clone();
            let give_leases = held_leases.clone();

            let wrapper = MemoryDomain::unpooled().parent(
                move |n| {
                    let call = take_calls.fetch_add(1, Ordering::AcqRel);
                    if call == 0 {
                        assert_eq!(n, LIVE_STEP, "caller A must plan preferred LIVE_STEP");
                        take_a_planned.lock().take().unwrap().send(()).unwrap();
                        let rx = take_a_go.lock().take().unwrap();
                        recv_bounded(&rx, "caller A was not released to hit capped parent");
                    } else if call == 1 {
                        assert_eq!(n, LIVE_STEP, "caller B must plan LIVE_STEP");
                        take_b_planned.lock().take().unwrap().send(()).unwrap();
                        let rx = take_b_acquire.lock().take().unwrap();
                        recv_bounded(&rx, "caller B was not released to acquire LIVE_STEP");
                        let lease = take_parent.reserve(n)?;
                        take_leases.lock().push(lease);
                        take_b_acquired.lock().take().unwrap().send(()).unwrap();
                        let nested = take_nested.get().unwrap().upgrade().unwrap();
                        wait_for_parked_upstream_waiter(&nested);
                        return Ok(());
                    }
                    let lease = take_parent.reserve(n)?;
                    take_leases.lock().push(lease);
                    Ok(())
                },
                move |mut n| {
                    let mut leases = give_leases.lock();
                    while n > 0 {
                        let last = leases.last_mut().expect("tracked upstream lease");
                        let take = n.min(last.bytes());
                        if take == last.bytes() {
                            drop(leases.pop());
                        } else {
                            let part = last.split(take).unwrap();
                            drop(part);
                        }
                        n -= take;
                    }
                },
            );

            let nested = QueryMemoryLedger::nested(None, wrapper);
            nested_cell.set(Arc::downgrade(&nested)).unwrap();
            let owner = nested.owner(None).unwrap();

            let (ra_tx, ra_rx) = mpsc::channel();
            let na = nested.clone();
            let oa = owner.clone();
            let ta = std::thread::spawn(move || {
                let _ = ra_tx.send(na.reserve(&oa, LIVE_STEP, 2, false));
            });

            recv_bounded(&a_planned_rx, "caller A did not plan upstream growth");

            let (rb_tx, rb_rx) = mpsc::channel();
            let nb = nested.clone();
            let ob = owner.clone();
            let tb = std::thread::spawn(move || {
                let _active = ActiveUpstreamOp::enter();
                let _ = rb_tx.send(nb.reserve(&ob, LIVE_STEP, LIVE_STEP, false));
            });

            recv_bounded(
                &b_planned_rx,
                "caller B did not bypass wait and plan growth",
            );

            // Exact 1-byte receiving charge raises `live` and `eager` together.
            nested.reserve_receiving(&owner, LIVE_STEP, 1).unwrap();
            assert_eq!(nested.live.load(Ordering::Acquire), 1);
            assert_eq!(outer.live.load(Ordering::Acquire), 1);

            // Let B acquire `LIVE_STEP`, filling the `LIVE_STEP + 1` parent cap.
            b_acquire_tx.send(()).unwrap();
            recv_bounded(&b_acquired_rx, "caller B did not acquire LIVE_STEP");
            assert_eq!(outer.live.load(Ordering::Acquire), LIVE_STEP + 1);

            // Release A so its preferred + 2-byte fallback fail and it waits for B.
            a_go_tx.send(()).unwrap();

            let rb = recv_bounded(&rb_rx, "caller B did not finish local-limit failure");
            let ra = recv_bounded(&ra_rx, "caller A did not retry after B returned credit");
            ta.join().unwrap();
            tb.join().unwrap();
            assert!(
                matches!(
                    rb,
                    Err(Error::BudgetExceeded(b))
                        if b.kind == BudgetKind::Memory && b.limit == LIVE_STEP
                ),
                "caller B must fail against the local cap: {rb:?}"
            );
            ra.expect(
                "caller A must revalidate upstream capacity and succeed after B drops staged credit",
            );
            assert!(
                nested.upstream_waits_total.load(Ordering::Acquire) >= 1,
                "caller A must have waited on upstream_done while B held staged credit"
            );
            assert_eq!(
                nested.upstream_return_epoch.load(Ordering::Acquire),
                1,
                "caller B's dropped staged lease must advance upstream_return_epoch once"
            );
            assert_eq!(nested.live.load(Ordering::Acquire), 3);
            assert_eq!(outer.live.load(Ordering::Acquire), LIVE_STEP);

            // A stable parent refusal with no concurrent credit return must still
            // terminate promptly with `BudgetExceeded`.
            assert!(matches!(
                nested.reserve(&owner, 1 << 20, LIVE_STEP, false),
                Err(Error::BudgetExceeded(b))
                    if b.kind == BudgetKind::Memory && b.limit == LIVE_STEP + 1
            ));

            nested.release(owner.id, 2);
            nested.release_receiving(owner.id, 1);
            drop(owner);
            drop(nested);
            assert_eq!(outer.live.load(Ordering::Acquire), 0);
        }
    }

    #[test]
    fn reentrant_parent_staged_return_does_not_loop_stable_refusal() {
        use std::sync::atomic::AtomicBool;
        use std::sync::mpsc;

        // Bound synchronous reentry so a regression fails fast on attempt count
        // instead of spinning forever if the fix is absent, while `recv_bounded`
        // also bounds against any deadlock.
        const MAX_REENTRANT_ATTEMPTS: usize = 16;

        let outer = QueryMemoryLedger::seeded(None, 0, 0, 0);
        let outer_parent = outer.child_parent(None, LIVE_STEP + 1).unwrap();
        let held_leases: Arc<Mutex<Vec<MemoryLease>>> = Arc::new(Mutex::new(Vec::new()));
        let nested_cell =
            Arc::new(std::sync::OnceLock::<std::sync::Weak<QueryMemoryLedger>>::new());
        let owner_cell = Arc::new(std::sync::OnceLock::<Arc<MemoryOwner>>::new());
        let armed = Arc::new(AtomicBool::new(false));
        let in_reentrant_callback = Arc::new(AtomicBool::new(false));
        let a_attempts = Arc::new(AtomicUsize::new(0));
        let reentrant_receiving_calls = Arc::new(AtomicUsize::new(0));

        let take_parent = outer_parent.clone();
        let take_leases = held_leases.clone();
        let take_nested = nested_cell.clone();
        let take_owner = owner_cell.clone();
        let take_armed = armed.clone();
        let take_in_reentrant = in_reentrant_callback.clone();
        let take_a_attempts = a_attempts.clone();
        let take_reentrant_calls = reentrant_receiving_calls.clone();
        let give_leases = held_leases.clone();

        let wrapper = MemoryDomain::unpooled().parent(
            move |n| {
                if take_armed.load(Ordering::Acquire) {
                    if take_in_reentrant.load(Ordering::Acquire) {
                        assert_eq!(n, 1, "reentrant reserve_receiving must request 1 byte");
                        take_reentrant_calls.fetch_add(1, Ordering::AcqRel);
                        let lease = take_parent.reserve(1)?;
                        take_leases.lock().push(lease);
                        // Use already-backed slack on the same ledger so the
                        // reentrant receiving call exceeds its local cap of 2
                        // on recheck, drops its staged 1-byte lease, and
                        // advances `upstream_return_epoch`.
                        let nested = take_nested.get().unwrap().upgrade().unwrap();
                        let owner = take_owner.get().unwrap();
                        nested
                            .reserve(owner, 1 << 20, 1, false)
                            .expect("slack-backed temporary ordinary charge must succeed");
                        return Ok(());
                    }
                    let attempt = take_a_attempts.fetch_add(1, Ordering::AcqRel) + 1;
                    if attempt <= MAX_REENTRANT_ATTEMPTS {
                        let nested = take_nested.get().unwrap().upgrade().unwrap();
                        let owner = take_owner.get().unwrap();
                        take_in_reentrant.store(true, Ordering::Release);
                        let re_res = nested.reserve_receiving(owner, 2, 1);
                        take_in_reentrant.store(false, Ordering::Release);
                        assert!(
                            matches!(
                                re_res,
                                Err(Error::BudgetExceeded(b))
                                    if b.kind == BudgetKind::Memory && b.limit == 2
                            ),
                            "reentrant reserve_receiving must fail its local cap of 2: {re_res:?}"
                        );
                        // Restore ledger state before refusing A's request so
                        // ownership is unchanged on refusal.
                        nested.release(owner.id, 1);
                    }
                }
                let lease = take_parent.reserve(n)?;
                take_leases.lock().push(lease);
                Ok(())
            },
            move |mut n| {
                let mut leases = give_leases.lock();
                while n > 0 {
                    let last = leases.last_mut().expect("tracked upstream lease");
                    let take = n.min(last.bytes());
                    if take == last.bytes() {
                        drop(leases.pop());
                    } else {
                        let part = last.split(take).unwrap();
                        drop(part);
                    }
                    n -= take;
                }
            },
        );

        let nested = QueryMemoryLedger::nested(None, wrapper);
        nested_cell.set(Arc::downgrade(&nested)).unwrap();
        let owner = nested.owner(None).unwrap();
        assert!(owner_cell.set(owner.clone()).is_ok());

        // Ordinary 1-byte charge backed by a LIVE_STEP lease, leaving 1 spare
        // byte in the LIVE_STEP + 1 parent.
        nested.reserve(&owner, 1 << 20, 1, false).unwrap();
        assert_eq!(nested.live.load(Ordering::Acquire), 1);
        assert_eq!(outer.live.load(Ordering::Acquire), LIVE_STEP);
        assert_eq!(nested.upstream_return_epoch.load(Ordering::Acquire), 0);

        armed.store(true, Ordering::Release);

        let (ra_tx, ra_rx) = mpsc::channel();
        let na = nested.clone();
        let oa = owner.clone();
        let ta = std::thread::spawn(move || {
            let _ = ra_tx.send(na.reserve(&oa, 1 << 20, LIVE_STEP + 1, false));
        });

        let ra = recv_bounded(
            &ra_rx,
            "stable refusal hung under synchronous reentrant staged returns",
        );
        ta.join().unwrap();

        assert!(
            matches!(
                ra,
                Err(Error::BudgetExceeded(b))
                    if b.kind == BudgetKind::Memory
                        && b.limit == LIVE_STEP + 1
                        && b.requested == LIVE_STEP + 2
            ),
            "stable parent refusal must surface BudgetExceeded: {ra:?}"
        );
        assert_eq!(
            a_attempts.load(Ordering::Acquire),
            2,
            "stable refusal must run only preferred and minimum upstream attempts without retrying"
        );
        assert_eq!(
            reentrant_receiving_calls.load(Ordering::Acquire),
            2,
            "both upstream attempts must exercise the reentrant staged return"
        );
        assert_eq!(
            nested.upstream_return_epoch.load(Ordering::Acquire),
            2,
            "reentrant staged returns must have advanced upstream_return_epoch twice"
        );
        assert_eq!(nested.live.load(Ordering::Acquire), 1);
        assert_eq!(outer.live.load(Ordering::Acquire), LIVE_STEP);

        armed.store(false, Ordering::Release);
        nested.release(owner.id, 1);
        drop(owner);
        drop(nested);
        assert_eq!(outer.live.load(Ordering::Acquire), 0);
    }

    #[test]
    fn cross_ledger_callback_refusal_skips_target_reclaim_wait() {
        use std::sync::atomic::AtomicBool;
        use std::sync::mpsc;

        // `ledger_b` holds an in-flight tracked optional-prefetch release on Thread B
        // that stays open until `ledger_a`'s tracked optional-prefetch release callback
        // on Thread A has attempted an unsatisfiable reservation on `ledger_b` and
        // returned `BudgetExceeded`. Because Thread A is running inside a prefetch
        // release callback on a DIFFERENT ledger (`ledger_a`), the thread-local depth
        // counter (`ACTIVE_PREFETCH_RELEASES`) must suppress waiting on `ledger_b`'s
        // `reclaim_done` condvar and record zero waits in `ledger_b`.
        let limit = 16_384u64;
        let used_a = Arc::new(AtomicU64::new(0));
        let used_b = Arc::new(AtomicU64::new(0));

        let (b_in_flight_tx, b_in_flight_rx) = mpsc::channel::<()>();
        let (cb_returned_tx, cb_returned_rx) = mpsc::channel::<()>();
        let b_in_flight_tx = Arc::new(Mutex::new(Some(b_in_flight_tx)));
        let cb_returned_tx = Arc::new(Mutex::new(Some(cb_returned_tx)));
        let cb_returned_rx = Arc::new(Mutex::new(Some(cb_returned_rx)));
        let cb_checked = Arc::new(AtomicBool::new(false));

        let ledger_b_cell =
            Arc::new(std::sync::OnceLock::<std::sync::Weak<QueryMemoryLedger>>::new());
        let owner_b_cell = Arc::new(std::sync::OnceLock::<Arc<MemoryOwner>>::new());

        let take_b = used_b.clone();
        let give_b = used_b.clone();
        let give_b_ledger = ledger_b_cell.clone();
        let give_b_tx = b_in_flight_tx.clone();
        let give_b_rx = cb_returned_rx.clone();
        let parent_b = MemoryDomain::unpooled().parent(
            move |n| {
                let mut cur = take_b.load(Ordering::Relaxed);
                loop {
                    let next = cur.checked_add(n).ok_or(ParentRefused::Overflow)?;
                    if next > limit {
                        return Err(ParentRefused::Limit {
                            limit,
                            requested: next,
                            scope: Scope::Work,
                        });
                    }
                    match take_b.compare_exchange_weak(
                        cur,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return Ok(()),
                        Err(actual) => cur = actual,
                    }
                }
            },
            move |n| {
                if n != 0
                    && let Some(tx) = give_b_tx.lock().take()
                {
                    let ledger_b = give_b_ledger.get().unwrap().upgrade().unwrap();
                    assert!(
                        ledger_b.state.try_lock().is_some(),
                        "ledger_b return callback must run outside ledger_b lock"
                    );
                    tx.send(()).unwrap();
                    let rx = give_b_rx.lock().take().unwrap();
                    recv_bounded(
                        &rx,
                        "ledger_a callback did not return from ledger_b refusal while ledger_b release stayed in flight",
                    );
                }
                give_b.fetch_sub(n, Ordering::AcqRel);
            },
        );
        let ledger_b = QueryMemoryLedger::nested(None, parent_b);
        ledger_b_cell.set(Arc::downgrade(&ledger_b)).unwrap();
        ledger_b.cursor_mode().unwrap();
        let owner_b = ledger_b.owner(None).unwrap();
        assert!(owner_b_cell.set(owner_b.clone()).is_ok());

        let take_a = used_a.clone();
        let give_a = used_a.clone();
        let a_target_ledger = ledger_b_cell.clone();
        let a_target_owner = owner_b_cell.clone();
        let a_cb_checked = cb_checked.clone();
        let a_cb_returned_tx = cb_returned_tx.clone();
        let parent_a = MemoryDomain::unpooled().parent(
            move |n| {
                let mut cur = take_a.load(Ordering::Relaxed);
                loop {
                    let next = cur.checked_add(n).ok_or(ParentRefused::Overflow)?;
                    if next > limit {
                        return Err(ParentRefused::Limit {
                            limit,
                            requested: next,
                            scope: Scope::Work,
                        });
                    }
                    match take_a.compare_exchange_weak(
                        cur,
                        next,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return Ok(()),
                        Err(actual) => cur = actual,
                    }
                }
            },
            move |n| {
                give_a.fetch_sub(n, Ordering::AcqRel);
                if n != 0 && !a_cb_checked.swap(true, Ordering::AcqRel) {
                    let ledger_b = a_target_ledger.get().unwrap().upgrade().unwrap();
                    let owner_b = a_target_owner.get().unwrap();
                    assert!(
                        ledger_b.state.try_lock().is_some(),
                        "target ledger_b lock must be unlocked before cross-ledger callback admission"
                    );
                    assert_eq!(
                        ledger_b.reclaim_in_flight.load(Ordering::Acquire),
                        1,
                        "ledger_b must have its tracked release in flight"
                    );
                    assert!(matches!(
                        ledger_b.reserve(owner_b, 1 << 20, limit * 2, false),
                        Err(Error::BudgetExceeded(_))
                    ));
                    assert_eq!(
                        ledger_b.reclaim_waits_total.load(Ordering::Acquire),
                        0,
                        "cross-ledger callback must skip waiting on ledger_b.reclaim_done"
                    );
                    if let Some(tx) = a_cb_returned_tx.lock().take() {
                        tx.send(()).unwrap();
                    }
                }
            },
        );
        let ledger_a = QueryMemoryLedger::nested(None, parent_a);
        ledger_a.cursor_mode().unwrap();
        let owner_a = ledger_a.owner(None).unwrap();

        ledger_a.admit_prefetch(None, 1 << 20, limit).unwrap();
        ledger_a.end_prefetch();
        ledger_b.admit_prefetch(None, 1 << 20, limit).unwrap();
        ledger_b.end_prefetch();

        let (res_b_tx, res_b_rx) = mpsc::channel();
        let lb = ledger_b.clone();
        let ob = owner_b.clone();
        let tb = std::thread::spawn(move || {
            let _ = res_b_tx.send(lb.reserve_receiving(&ob, 1 << 20, 4096));
        });

        recv_bounded(
            &b_in_flight_rx,
            "thread B did not enter ledger_b tracked prefetch return callback",
        );
        assert_eq!(ledger_b.prefetch_bytes.load(Ordering::Acquire), 0);
        assert_eq!(ledger_b.reclaim_in_flight.load(Ordering::Acquire), 1);

        let (res_a_tx, res_a_rx) = mpsc::channel();
        let la = ledger_a.clone();
        let oa = owner_a.clone();
        let ta = std::thread::spawn(move || {
            let _ = res_a_tx.send(la.reserve_receiving(&oa, 1 << 20, 4096));
        });

        let res_a = recv_bounded(&res_a_rx, "thread A hung in cross-ledger callback refusal");
        let res_b = recv_bounded(&res_b_rx, "thread B did not finish ledger_b reclaim");
        ta.join().unwrap();
        tb.join().unwrap();
        res_a.expect("thread A must finish ledger_a reclaim and reserve");
        res_b.expect("thread B must finish ledger_b reclaim and reserve");
        assert!(cb_checked.load(Ordering::Acquire));
        assert_eq!(
            ledger_b.reclaim_waits_total.load(Ordering::Acquire),
            0,
            "target ledger must record zero condvar waits for cross-ledger callback refusal"
        );
        ledger_a.release_receiving(owner_a.id, 4096);
        ledger_b.release_receiving(owner_b.id, 4096);
        assert_eq!(used_a.load(Ordering::Relaxed), 0);
        assert_eq!(used_b.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn nested_upstream_growth_and_staged_return_in_tls_destructor() {
        use std::sync::atomic::AtomicBool;
        use std::sync::mpsc;

        const MAX_REENTRANT_ATTEMPTS: usize = 8;

        struct TlsGuard {
            action: Option<Box<dyn FnOnce() + Send>>,
            outcome_tx: mpsc::Sender<std::result::Result<(), String>>,
        }

        impl Drop for TlsGuard {
            fn drop(&mut self) {
                let Some(action) = self.action.take() else {
                    return;
                };
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action)).map_err(
                    |payload| {
                        payload
                            .downcast_ref::<&str>()
                            .map(|s| (*s).to_owned())
                            .or_else(|| payload.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "non-string panic payload".to_owned())
                    },
                );
                let _ = self.outcome_tx.send(res);
            }
        }

        thread_local! {
            static GUARD: std::cell::RefCell<Option<TlsGuard>> =
                const { std::cell::RefCell::new(None) };
        }

        // 1) Ample-budget upstream growth triggered from a TLS destructor
        // registered BEFORE the thread's first nested ledger reservation.
        let outer = QueryMemoryLedger::seeded(None, 0, 0, 0);
        let parent = outer.child_parent(None, 2 * LIVE_STEP).unwrap();
        let nested = QueryMemoryLedger::nested(None, parent);
        let owner = nested.owner(None).unwrap();

        // 2) Reentrant staged-return stable refusal also triggered inside the
        // same TLS destructor so both `UpstreamReturnScope::enter` and
        // `UpstreamReturnScope::record_return` are exercised during teardown
        // without looping.
        let ref_outer = QueryMemoryLedger::seeded(None, 0, 0, 0);
        let ref_outer_parent = ref_outer.child_parent(None, LIVE_STEP + 1).unwrap();
        let held_leases: Arc<Mutex<Vec<MemoryLease>>> = Arc::new(Mutex::new(Vec::new()));
        let ref_nested_cell =
            Arc::new(std::sync::OnceLock::<std::sync::Weak<QueryMemoryLedger>>::new());
        let ref_owner_cell = Arc::new(std::sync::OnceLock::<Arc<MemoryOwner>>::new());
        let armed = Arc::new(AtomicBool::new(false));
        let in_reentrant = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(AtomicUsize::new(0));

        let take_parent = ref_outer_parent.clone();
        let take_leases = held_leases.clone();
        let take_nested = ref_nested_cell.clone();
        let take_owner = ref_owner_cell.clone();
        let take_armed = armed.clone();
        let take_in_reentrant = in_reentrant.clone();
        let take_attempts = attempts.clone();
        let give_leases = held_leases.clone();

        let wrapper = MemoryDomain::unpooled().parent(
            move |n| {
                if take_armed.load(Ordering::Acquire) {
                    if take_in_reentrant.load(Ordering::Acquire) {
                        let lease = take_parent.reserve(n)?;
                        take_leases.lock().push(lease);
                        let nested = take_nested.get().unwrap().upgrade().unwrap();
                        let owner = take_owner.get().unwrap();
                        nested
                            .reserve(owner, 1 << 20, 1, false)
                            .expect("slack-backed temporary ordinary charge must succeed");
                        return Ok(());
                    }
                    let attempt = take_attempts.fetch_add(1, Ordering::AcqRel) + 1;
                    if attempt <= MAX_REENTRANT_ATTEMPTS {
                        let nested = take_nested.get().unwrap().upgrade().unwrap();
                        let owner = take_owner.get().unwrap();
                        take_in_reentrant.store(true, Ordering::Release);
                        let re_res = nested.reserve_receiving(owner, 2, 1);
                        take_in_reentrant.store(false, Ordering::Release);
                        assert!(matches!(
                            re_res,
                            Err(Error::BudgetExceeded(b))
                                if b.kind == BudgetKind::Memory && b.limit == 2
                        ));
                        nested.release(owner.id, 1);
                    }
                }
                let lease = take_parent.reserve(n)?;
                take_leases.lock().push(lease);
                Ok(())
            },
            move |mut n| {
                let mut leases = give_leases.lock();
                while n > 0 {
                    let last = leases.last_mut().expect("tracked upstream lease");
                    let take = n.min(last.bytes());
                    if take == last.bytes() {
                        drop(leases.pop());
                    } else {
                        let part = last.split(take).unwrap();
                        drop(part);
                    }
                    n -= take;
                }
            },
        );
        let ref_nested = QueryMemoryLedger::nested(None, wrapper);
        ref_nested_cell.set(Arc::downgrade(&ref_nested)).unwrap();
        let ref_owner = ref_nested.owner(None).unwrap();
        assert!(ref_owner_cell.set(ref_owner.clone()).is_ok());

        let (tx, rx) = mpsc::channel();
        let n_grow = nested.clone();
        let o_grow = owner.clone();
        let n_ref = ref_nested.clone();
        let o_ref = ref_owner.clone();
        let armed_for_drop = armed.clone();

        let t = std::thread::spawn(move || {
            let n_grow_drop = n_grow.clone();
            let o_grow_drop = o_grow.clone();
            let n_ref_drop = n_ref.clone();
            let o_ref_drop = o_ref.clone();

            // Register the application TLS drop guard BEFORE any nested ledger
            // reservation initializes ledger TLS on this thread.
            GUARD.with(|slot| {
                *slot.borrow_mut() = Some(TlsGuard {
                    action: Some(Box::new(move || {
                        // Upstream growth inside TLS destructor.
                        n_grow_drop
                            .reserve(&o_grow_drop, 2 * LIVE_STEP, LIVE_STEP, false)
                            .expect("upstream growth during TLS teardown must succeed");
                        assert_eq!(n_grow_drop.live.load(Ordering::Acquire), LIVE_STEP + 1);
                        n_grow_drop.release(o_grow_drop.id, LIVE_STEP + 1);

                        // Stable refusal with reentrant staged return inside TLS destructor.
                        armed_for_drop.store(true, Ordering::Release);
                        let refusal =
                            n_ref_drop.reserve(&o_ref_drop, 1 << 20, LIVE_STEP + 1, false);
                        armed_for_drop.store(false, Ordering::Release);
                        assert!(
                            matches!(
                                refusal,
                                Err(Error::BudgetExceeded(b))
                                    if b.kind == BudgetKind::Memory && b.limit == LIVE_STEP + 1
                            ),
                            "stable refusal during TLS teardown must return BudgetExceeded: {refusal:?}"
                        );
                        n_ref_drop.release(o_ref_drop.id, 1);
                    })),
                    outcome_tx: tx,
                });
            });

            // Initialize ledger upstream growth on this thread AFTER registering
            // the application TLS guard so any `Drop` ledger TLS key would be
            // destroyed before `GUARD` on glibc.
            n_grow
                .reserve(&o_grow, 2 * LIVE_STEP, 1, false)
                .expect("initial one-byte reservation must acquire one LIVE_STEP");
            n_ref
                .reserve(&o_ref, 1 << 20, 1, false)
                .expect("initial one-byte reservation on refusal ledger must succeed");
        });

        let outcome = recv_bounded(&rx, "TLS destructor guard did not run or hung");
        t.join().unwrap();
        outcome.expect("TLS destructor upstream growth and staged return must not panic");

        assert_eq!(
            attempts.load(Ordering::Acquire),
            2,
            "reentrant stable refusal during TLS teardown must not loop"
        );
        assert_eq!(ref_nested.upstream_return_epoch.load(Ordering::Acquire), 2);
        assert_eq!(nested.live.load(Ordering::Acquire), 0);
        assert_eq!(ref_nested.live.load(Ordering::Acquire), 0);

        drop(owner);
        drop(nested);
        drop(ref_owner);
        drop(ref_nested);
        assert_eq!(outer.live.load(Ordering::Acquire), 0);
        assert_eq!(ref_outer.live.load(Ordering::Acquire), 0);
    }

    #[test]
    fn upstream_return_scope_overflow_tracks_same_thread_vs_concurrent_returns() {
        let ledger_a = QueryMemoryLedger::seeded(None, 0, 0, 0);
        let ledger_b = QueryMemoryLedger::seeded(None, 0, 0, 0);

        let depth = INLINE_RETURN_SCOPES + 3;
        let mut scopes = Vec::with_capacity(depth);
        for i in 0..depth {
            let target = if i % 2 == 0 { &ledger_a } else { &ledger_b };
            let epoch = target.upstream_return_epoch.load(Ordering::Acquire);
            scopes.push(UpstreamReturnScope::enter(target, epoch));
        }

        // Same-thread staged return on `ledger_a` must be discounted by every
        // active `ledger_a` scope (both inline and overflow) and ignored by
        // `ledger_b` scopes.
        UpstreamReturnScope::record_return(&ledger_a);
        ledger_a
            .upstream_return_epoch
            .fetch_add(1, Ordering::Release);
        for scope in &scopes {
            assert!(
                !scope.other_returned(),
                "same-thread return must not trigger other_returned"
            );
        }

        // A concurrent return on `ledger_a` (advancing epoch without recording
        // on this thread) must be observed by both inline and overflow scopes
        // targeting `ledger_a`, but not by `ledger_b` scopes.
        ledger_a
            .upstream_return_epoch
            .fetch_add(1, Ordering::Release);
        for (i, scope) in scopes.iter().enumerate() {
            assert_eq!(
                scope.other_returned(),
                i % 2 == 0,
                "concurrent return on ledger_a must be observed only by ledger_a scopes (index {i})"
            );
        }

        // Drop in LIFO order and ensure overflow bookkeeping drains cleanly.
        while let Some(scope) = scopes.pop() {
            drop(scope);
        }
        ACTIVE_UPSTREAM_RETURN_SCOPES.with(|s| {
            assert_eq!(s.len.get(), 0, "inline scope stack must return to empty");
            let tid = s.tid.get();
            assert!(
                OVERFLOW_RETURN_SCOPES
                    .lock()
                    .iter()
                    .all(|entry| entry.tid != tid),
                "overflow scopes for this thread must be completely drained"
            );
        });
    }
}

#[cfg(test)]
mod child_tests;
