//! The memory of one run, against [`crate::ReasonOptions::max_memory`].
//!
//! A run has one [`Budget`], shared by its threads and by every structure the run owns:
//! the derived tier and its indexes, the local term table, the candidate buffers and
//! caches of rule evaluation, the merge's working buffers, the previous closure of an
//! incremental run and its maintenance state, the buffers that write the inferred
//! graph, and the closure kept for the next run. The store's block cache, shared with
//! queries, is not part of it.
//!
//! Structures are charged by their allocated capacity, worked out from the sizes of
//! their elements and the growth rules of `Vec` and of hashbrown's tables
//! ([`table_bytes`]), not by counting allocator calls, so the figures are estimates of
//! the reasoner's own heap. Growth is charged before it is allocated, the old and the
//! new allocation together while a buffer or table is resized, and released after the
//! old one is freed. A charge that would pass the limit fails the run at that point.
//! Memory that is already allocated when it is found, such as a term read from the
//! store, is charged at once and checked at the next charge that can fail.
//!
//! The peak is the highest total charged at any time, which the run reports as
//! `peakBytes`.
//!
//! Each holder takes its charges from the budget in steps, of 16 KiB or a 16,384th of
//! the limit when that is smaller, and keeps up to two steps of what it frees. The
//! steps held unused count against the limit and in the peak.

use parking_lot::Mutex;
use sparkles_budget::{DomainId, MemoryLease, MemoryParent, ParentRefused, Scope};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A run went over its memory limit.
#[derive(Debug, Clone, thiserror::Error)]
#[error("reasoning exceeded the memory limit of {limit} bytes{context}")]
pub struct MemoryExceeded {
    pub limit: usize,
    /// what was being done, for the message (with a leading space), or empty
    pub context: String,
}

impl MemoryExceeded {
    pub fn with_context(mut self, context: String) -> MemoryExceeded {
        if self.context.is_empty() {
            self.context = context;
        }
        self
    }
}

/// Private failure preserves the public local error's downcast and the exact
/// parent scope. It deliberately has no StdError implementation: conversion into
/// anyhow unwraps Local instead of hiding it behind a new public root error.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
struct ParentFailureContext(String);

#[derive(Debug, Clone)]
pub(crate) enum BudgetFailure {
    Local(MemoryExceeded),
    Parent {
        refusal: ParentRefused,
        context: String,
    },
}
impl BudgetFailure {
    pub fn local(limit: usize, context: String) -> Self {
        Self::Local(MemoryExceeded { limit, context })
    }
    pub fn with_context(mut self, context: String) -> Self {
        match &mut self {
            Self::Local(error) => {
                if error.context.is_empty() {
                    error.context = context;
                }
            }
            Self::Parent { context: first, .. } => {
                if first.is_empty() {
                    *first = context;
                }
            }
        }
        self
    }

    pub fn parent(refusal: ParentRefused) -> Self {
        Self::Parent {
            refusal,
            context: String::new(),
        }
    }

    pub fn core_error(&self) -> Option<sparkles_core::Error> {
        let Self::Parent {
            refusal:
                ParentRefused::Limit {
                    limit,
                    requested,
                    scope,
                },
            ..
        } = self
        else {
            return None;
        };
        Some(sparkles_core::Error::BudgetExceeded(
            sparkles_core::Budget {
                kind: match scope {
                    Scope::Local => sparkles_core::BudgetKind::Memory,
                    Scope::Work | Scope::Retained => sparkles_core::BudgetKind::ServerMemory,
                },
                limit: *limit,
                requested: *requested,
            },
        ))
    }

    /// Recover a typed budget error from an anyhow chain without string matching.
    pub fn from_anyhow(error: &anyhow::Error) -> Option<Self> {
        if let Some(local) = error.downcast_ref::<MemoryExceeded>() {
            return Some(Self::Local(local.clone()));
        }
        error
            .downcast_ref::<ParentRefused>()
            .copied()
            .map(|refusal| Self::Parent {
                refusal,
                context: error
                    .downcast_ref::<ParentFailureContext>()
                    .map_or_else(String::new, |c| c.0.clone()),
            })
    }
}
impl From<MemoryExceeded> for BudgetFailure {
    fn from(error: MemoryExceeded) -> Self {
        Self::Local(error)
    }
}
impl std::fmt::Display for BudgetFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(error) => error.fmt(f),
            Self::Parent { refusal, context } => write!(f, "{refusal}{context}"),
        }
    }
}
impl From<BudgetFailure> for anyhow::Error {
    fn from(error: BudgetFailure) -> Self {
        let core = error.core_error();
        match error {
            BudgetFailure::Local(local) => anyhow::Error::new(local),
            BudgetFailure::Parent { refusal, context } => {
                let mut error = anyhow::Error::new(refusal);
                if let Some(core) = core {
                    error = error.context(core);
                }
                if !context.is_empty() {
                    error = error.context(ParentFailureContext(context));
                }
                error
            }
        }
    }
}

/// The memory a run has charged, its peak and its limit.
pub(crate) struct Budget {
    limit: usize,
    /// the bytes a holder takes at once, see [`MAX_STEP`]
    step: usize,
    used: AtomicUsize,
    peak: AtomicUsize,
    parent: Option<Backing>,
}

/// One owned parent reservation. The atomic is the fast-path published coverage;
/// the mutex serializes extensions and detaching retired credit, never its release.
struct Backing {
    parent: Arc<dyn MemoryParent>,
    domain: DomainId,
    reserved: AtomicUsize,
    lease: Mutex<MemoryLease>,
}
const PARENT_STEP: usize = 64 << 10;
const PARENT_SLACK: usize = 1 << 20;

impl std::fmt::Debug for Budget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Budget")
            .field("limit", &self.limit)
            .field("used", &self.used())
            .field("peak", &self.peak())
            .field("domain", &self.parent.as_ref().map(|p| &p.domain))
            .finish()
    }
}

impl Budget {
    pub fn new(limit: Option<usize>) -> Arc<Budget> {
        Self::with_parent(limit, None)
    }

    pub fn with_parent(limit: Option<usize>, parent: Option<Arc<dyn MemoryParent>>) -> Arc<Budget> {
        Arc::new(Budget {
            limit: limit.unwrap_or(usize::MAX),
            step: limit.map_or(MAX_STEP, |l| (l >> STEP_SHIFT).min(MAX_STEP)),
            used: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            parent: parent.map(|parent| Backing {
                domain: parent.domain(),
                // All MemoryParent implementations are sealed; zero never invokes
                // a backend and cannot refuse or allocate reservation credit.
                lease: Mutex::new(parent.clone().reserve(0).expect("zero reservation")),
                reserved: AtomicUsize::new(0),
                parent,
            }),
        })
    }

    pub fn unlimited() -> Arc<Budget> {
        Budget::new(None)
    }
    pub fn used(&self) -> usize {
        self.used.load(Ordering::SeqCst)
    }
    pub fn available(&self) -> usize {
        self.limit.saturating_sub(self.used())
    }
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }
    pub fn parent(&self) -> Option<Arc<dyn MemoryParent>> {
        self.parent.as_ref().map(|p| p.parent.clone())
    }
    pub fn domain(&self) -> DomainId {
        self.parent.as_ref().map_or_else(
            || sparkles_budget::MemoryDomain::unpooled().id(),
            |p| p.domain.clone(),
        )
    }
    fn exceeded(&self) -> BudgetFailure {
        BudgetFailure::Local(MemoryExceeded {
            limit: self.limit,
            context: String::new(),
        })
    }
    fn overflow(&self) -> BudgetFailure {
        if self.parent.is_some() {
            BudgetFailure::parent(ParentRefused::Overflow)
        } else {
            self.exceeded()
        }
    }
    fn add_usage(&self, n: usize, strict: bool) -> Result<usize, BudgetFailure> {
        let mut cur = self.used.load(Ordering::SeqCst);
        loop {
            let next = cur.checked_add(n).ok_or_else(|| self.overflow())?;
            if strict && next > self.limit {
                return Err(self.exceeded());
            }
            match self
                .used
                .compare_exchange_weak(cur, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return Ok(next),
                Err(now) => cur = now,
            }
        }
    }
    /// Charge before allocation. Parent-backed usage is covered before acceptance.
    pub fn try_charge(&self, n: usize) -> Result<(), BudgetFailure> {
        if n == 0 {
            return Ok(());
        }
        let next = self.add_usage(n, true)?;
        if let Some(parent) = &self.parent {
            if next > parent.reserved.load(Ordering::SeqCst) {
                self.used.fetch_sub(n, Ordering::SeqCst);
                let mut lease = parent.lease.lock();
                let next = self.add_usage(n, true)?;
                if let Err(error) = self.grow_locked(parent, &mut lease, next) {
                    self.used.fetch_sub(n, Ordering::SeqCst);
                    return Err(BudgetFailure::parent(error));
                }
                self.peak.fetch_max(next, Ordering::Relaxed);
                return Ok(());
            }
        }
        self.peak.fetch_max(next, Ordering::Relaxed);
        Ok(())
    }
    fn grow_locked(
        &self,
        parent: &Backing,
        lease: &mut MemoryLease,
        need: usize,
    ) -> Result<(), ParentRefused> {
        let need = need.max(self.used.load(Ordering::SeqCst));
        let cur = parent.reserved.load(Ordering::SeqCst);
        if need <= cur {
            return Ok(());
        }
        let ahead = need.checked_next_multiple_of(PARENT_STEP).unwrap_or(need);
        let grow = |lease: &mut MemoryLease, target: usize| {
            lease.try_grow(u64::try_from(target - cur).map_err(|_| ParentRefused::Overflow)?)
        };
        let target = match grow(lease, ahead) {
            Ok(()) => ahead,
            Err(ParentRefused::Limit { .. } | ParentRefused::Overflow) if ahead > need => {
                grow(lease, need)?;
                need
            }
            Err(error) => return Err(error),
        };
        parent.reserved.store(target, Ordering::SeqCst);
        Ok(())
    }
    /// Compatibility accounting for already allocated unpooled state. A parent
    /// always requires strict pre-admission; its refusal must be propagated.
    pub fn charge(&self, n: usize) -> Result<(), BudgetFailure> {
        if self.parent.is_some() {
            return self.try_charge(n);
        }
        let next = self.add_usage(n, false)?;
        self.peak.fetch_max(next, Ordering::Relaxed);
        Ok(())
    }
    pub fn release(&self, n: usize) {
        if n == 0 {
            return;
        }
        let prior = self.used.fetch_sub(n, Ordering::SeqCst);
        assert!(prior >= n, "release only owned reasoning bytes");
        if let Some(parent) = &self.parent {
            if parent.reserved.load(Ordering::SeqCst)
                > (prior - n).saturating_add(PARENT_SLACK + PARENT_STEP)
            {
                self.trim(parent);
            }
        }
    }
    fn trim(&self, parent: &Backing) {
        let mut lease = parent.lease.lock();
        let cur = parent.reserved.load(Ordering::SeqCst);
        let keep = |used: usize| {
            used.checked_next_multiple_of(PARENT_STEP)
                .unwrap_or(cur)
                .min(cur)
        };
        let first = keep(self.used.load(Ordering::SeqCst));
        if first >= cur {
            return;
        }
        // Sequentially consistent publication/read pairing with fast chargers.
        parent.reserved.store(first, Ordering::SeqCst);
        let final_keep = keep(self.used.load(Ordering::SeqCst)).max(first);
        parent.reserved.store(final_keep, Ordering::SeqCst);
        let retired = lease
            .split(u64::try_from(cur - final_keep).expect("backed credit fits u64"))
            .expect("split only owned parent credit");
        drop(lease);
        drop(retired);
    }
    pub fn check(&self) -> Result<(), BudgetFailure> {
        if self.used() > self.limit {
            return Err(self.exceeded());
        }
        Ok(())
    }
    pub fn check_peak(&self) -> Result<(), BudgetFailure> {
        if self.peak() > self.limit {
            return Err(self.exceeded());
        }
        Ok(())
    }
}

/// Bytes charged to a budget, released on drop.
pub(crate) struct Held {
    budget: Arc<Budget>,
    bytes: usize,
    /// charged to the budget and not used yet, see [`MAX_STEP`]
    credit: usize,
}

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Held({})", self.bytes)
    }
}

/// The bytes a holder takes from the budget at once when a charge does not fit what
/// it took before: 16 KiB, or a 16,384th of the limit when that is smaller. Small
/// charges then do not each update the counter all threads share. A holder keeps at
/// most two steps unused.
const MAX_STEP: usize = 16 << 10;

/// A step is the limit shifted right by this many bits, when that is under
/// [`MAX_STEP`].
const STEP_SHIFT: u32 = 14;

impl Held {
    pub fn new(budget: &Arc<Budget>) -> Held {
        Held {
            budget: budget.clone(),
            bytes: 0,
            credit: 0,
        }
    }

    /// `n` bytes charged, or the error when they do not fit.
    pub fn try_new(budget: &Arc<Budget>, n: usize) -> Result<Held, BudgetFailure> {
        let mut h = Held::new(budget);
        h.try_grow(n)?;
        Ok(h)
    }

    /// Exact prepared ownership, without adding a holder-credit layer. Used to
    /// prepare a complete migration or publication before touching its old owner.
    pub fn try_new_exact(budget: &Arc<Budget>, n: usize) -> Result<Held, BudgetFailure> {
        Self::new_exact_mode(budget, n, true)
    }

    pub fn new_exact_mode(
        budget: &Arc<Budget>,
        n: usize,
        strict: bool,
    ) -> Result<Held, BudgetFailure> {
        if strict {
            budget.try_charge(n)?;
        } else {
            budget.charge(n)?;
        }
        Ok(Held {
            budget: budget.clone(),
            bytes: n,
            credit: 0,
        })
    }

    /// Hand already charged bytes to another counter of the identical budget.
    /// Call only after publication, outside locks if unused credit can return.
    pub fn into_bytes(mut self) -> usize {
        self.budget.release(std::mem::take(&mut self.credit));
        std::mem::take(&mut self.bytes)
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn budget(&self) -> &Arc<Budget> {
        &self.budget
    }

    pub fn try_grow(&mut self, n: usize) -> Result<(), BudgetFailure> {
        let next = self
            .bytes
            .checked_add(n)
            .ok_or_else(|| self.budget.overflow())?;
        if n > self.credit {
            take(&self.budget, n, &mut self.credit)?;
        }
        self.credit -= n;
        self.bytes = next;
        Ok(())
    }

    pub fn grow(&mut self, n: usize) -> Result<(), BudgetFailure> {
        let next = self
            .bytes
            .checked_add(n)
            .ok_or_else(|| self.budget.overflow())?;
        if n > self.credit {
            self.budget.charge(n - self.credit)?;
            self.credit = n;
        }
        self.credit -= n;
        self.bytes = next;
        Ok(())
    }

    pub fn shrink(&mut self, n: usize) {
        let n = n.min(self.bytes);
        self.bytes -= n;
        self.credit += n;
        give_back(&self.budget, &mut self.credit);
    }

    /// Hold `n` bytes: grow (even past the limit) or shrink to it.
    pub fn set(&mut self, n: usize) -> Result<(), BudgetFailure> {
        if n > self.bytes {
            self.grow(n - self.bytes)?;
        } else {
            self.shrink(self.bytes - n);
        }
        Ok(())
    }

    /// [`set`](Self::set), failing without growing when the growth does not fit.
    pub fn try_set(&mut self, n: usize) -> Result<(), BudgetFailure> {
        if n > self.bytes {
            self.try_grow(n - self.bytes)
        } else {
            self.shrink(self.bytes - n);
            Ok(())
        }
    }

    /// Move `n` of these bytes into a new holder.
    pub fn split(&mut self, n: usize) -> Held {
        let n = n.min(self.bytes);
        self.bytes -= n;
        Held {
            budget: self.budget.clone(),
            bytes: n,
            credit: 0,
        }
    }

    /// Transfer actual owned bytes between holders of the identical budget.
    /// Failure leaves both holders unchanged; their payloads still have credit.
    pub fn try_merge(&mut self, other: &mut Held) -> Result<(), BudgetFailure> {
        if !Arc::ptr_eq(&self.budget, &other.budget) {
            return Err(BudgetFailure::parent(ParentRefused::DifferentParent));
        }
        let total = self
            .bytes
            .checked_add(other.bytes)
            .ok_or_else(|| self.budget.overflow())?;
        self.bytes = total;
        other.bytes = 0;
        Ok(())
    }

    /// Take the bytes of `other` over.
    pub fn absorb(&mut self, mut other: Held) {
        self.bytes += std::mem::take(&mut other.bytes);
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.budget.release(self.bytes + self.credit);
    }
}

/// Charge `n` bytes more than `credit` holds, with a step beyond when that fits, and
/// add them to `credit`.
#[inline]
fn take(budget: &Budget, n: usize, credit: &mut usize) -> Result<(), BudgetFailure> {
    let need = n - *credit;
    let want = need.checked_add(budget.step).unwrap_or(need);
    if budget.try_charge(want).is_ok() {
        *credit += want;
    } else {
        budget.try_charge(need)?;
        *credit += need;
    }
    Ok(())
}

/// Give back what `credit` holds beyond two steps.
#[inline]
fn give_back(budget: &Budget, credit: &mut usize) {
    if *credit > 2 * budget.step {
        budget.release(*credit - budget.step);
        *credit = budget.step;
    }
}

impl Held {
    /// Take over `n` bytes already charged to this holder's budget (by a [`Meter`]).
    pub fn adopt(&mut self, n: usize) {
        self.bytes += n;
    }
}

/// The charges of one task, as a count, for the threads of a parallel step: a
/// [`Held`] without its own reference to the budget. What it holds when dropped is
/// released, unless [`into_bytes`](Self::into_bytes) hands it to a [`Held`].
pub(crate) struct Meter<'b> {
    budget: &'b Budget,
    /// charges fail past the limit; otherwise they are made anyway
    strict: bool,
    bytes: usize,
    /// charged to the budget and not used yet, see [`MAX_STEP`]
    credit: usize,
}

impl<'b> Meter<'b> {
    pub fn new(budget: &'b Budget, strict: bool) -> Meter<'b> {
        Meter {
            budget,
            strict,
            bytes: 0,
            credit: 0,
        }
    }

    #[inline]
    pub fn add(&mut self, n: usize) -> Result<(), BudgetFailure> {
        let next = self
            .bytes
            .checked_add(n)
            .ok_or_else(|| self.budget.overflow())?;
        if n > self.credit {
            if self.strict {
                take(self.budget, n, &mut self.credit)?;
            } else {
                self.budget.charge(n - self.credit)?;
                self.credit = n;
            }
        }
        self.credit -= n;
        self.bytes = next;
        Ok(())
    }

    pub fn sub(&mut self, n: usize) {
        let n = n.min(self.bytes);
        self.bytes -= n;
        self.credit += n;
        give_back(self.budget, &mut self.credit);
    }

    pub fn budget(&self) -> &'b Budget {
        self.budget
    }

    pub fn strict(&self) -> bool {
        self.strict
    }

    /// Take over `n` bytes already charged to the same budget.
    pub fn adopt(&mut self, n: usize) {
        self.bytes += n;
    }

    /// The bytes charged, for a [`Held::adopt`] of the same budget; what it took
    /// beyond them is given back.
    pub fn into_bytes(mut self) -> usize {
        self.budget.release(std::mem::take(&mut self.credit));
        std::mem::take(&mut self.bytes)
    }
}

impl Drop for Meter<'_> {
    fn drop(&mut self) {
        self.budget.release(self.bytes + self.credit);
    }
}

// ------------------------------------------------------------ allocation sizes ----

/// The buckets hashbrown allocates for a table that can hold `cap` items without
/// growing (its `capacity()`).
fn buckets(cap: usize) -> usize {
    if cap == 0 {
        0
    } else if cap < 8 {
        // tables of up to 8 buckets hold one item less than their buckets
        (cap + 1).next_power_of_two()
    } else {
        // larger tables hold 7/8 of their buckets
        (cap.saturating_mul(8) / 7).next_power_of_two()
    }
}

/// The heap of a hashbrown table (`HashMap`, `HashSet` or `HashTable`) of capacity
/// `cap` with elements (keys with values) of `elem` bytes: one element and one control
/// byte per bucket, and a group of control bytes more.
pub(crate) fn table_bytes(cap: usize, elem: usize) -> usize {
    match buckets(cap) {
        0 => 0,
        b => b.saturating_mul(elem + 1).saturating_add(16),
    }
}

/// The capacity a table of capacity `cap` and `len` items has after room for `add`
/// more is reserved: unchanged when they fit, otherwise that of the smallest table
/// holding `len + add` items and more than `cap` (hashbrown's growth rule).
pub(crate) fn table_grown(cap: usize, len: usize, add: usize) -> usize {
    let want = len.saturating_add(add);
    if want <= cap {
        return cap;
    }
    table_capacity(want.max(cap + 1))
}

/// The capacity of a table made for `items` items (hashbrown's `with_capacity`).
pub(crate) fn table_capacity(items: usize) -> usize {
    let b = match items {
        0 => return 0,
        1..4 => 4,
        4..8 => 8,
        n => (n.saturating_mul(8) / 7).next_power_of_two(),
    };
    if b < 8 { b - 1 } else { b / 8 * 7 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The estimate of a table's heap is what hashbrown allocates: checked against
    /// the capacities a `HashSet` reports as it grows.
    #[test]
    fn table_sizes_follow_hashbrown() {
        let mut s: rustc_hash::FxHashSet<u64> = Default::default();
        for i in 0..100_000u64 {
            let (cap, len) = (s.capacity(), s.len());
            s.insert(i);
            if s.capacity() != cap {
                assert_eq!(s.capacity(), table_grown(cap, len, 1), "at {i}");
            }
            assert!(buckets(s.capacity()) >= s.capacity());
        }
        assert_eq!(table_bytes(0, 8), 0);
        assert_eq!(table_bytes(3, 8), 4 * 9 + 16);
        assert_eq!(table_bytes(7, 8), 8 * 9 + 16);
        assert_eq!(table_bytes(14, 8), 16 * 9 + 16);
    }

    #[test]
    fn charges_fail_at_the_limit_and_track_the_peak() {
        let b = Budget::new(Some(100));
        let mut h = Held::try_new(&b, 60).unwrap();
        assert!(Held::try_new(&b, 41).is_err());
        assert_eq!(b.used(), 60);
        h.try_grow(40).unwrap();
        assert_eq!(b.peak(), 100);
        // a limit this small gives steps of 0 bytes: what a holder frees is released
        h.shrink(90);
        assert_eq!(b.used(), 10);
        h.try_grow(90).unwrap();
        assert_eq!(b.used(), 100);
        b.charge(200).unwrap();
        assert!(b.check().is_err());
        b.release(200);
        drop(h);
        assert_eq!((b.used(), b.peak()), (0, 300));
    }

    /// Small charges take a step from the budget when it fits, and a holder gives back
    /// what it does not use beyond two steps.
    #[test]
    fn charges_take_steps() {
        let b = Budget::unlimited();
        let mut h = Held::new(&b);
        h.try_grow(10).unwrap();
        assert_eq!(b.used(), 10 + MAX_STEP);
        h.try_grow(MAX_STEP).unwrap();
        assert_eq!(b.used(), 10 + MAX_STEP);
        h.try_grow(1).unwrap();
        assert_eq!(b.used(), 11 + 2 * MAX_STEP);
        // all of it freed: one step stays
        h.shrink(MAX_STEP + 11);
        assert_eq!(b.used(), MAX_STEP);
        let mut m = Meter::new(&b, true);
        m.add(5).unwrap();
        assert_eq!(b.used(), 2 * MAX_STEP + 5);
        assert_eq!(m.into_bytes(), 5);
        assert_eq!(b.used(), MAX_STEP + 5);
        b.release(5);
        drop(h);
        assert_eq!(b.used(), 0);
    }

    #[test]
    fn steps_scale_with_the_limit() {
        assert_eq!(Budget::new(Some(1 << 20)).step, 64);
        assert_eq!(Budget::new(Some(1 << 30)).step, 16 << 10);
        assert_eq!(Budget::new(None).step, 16 << 10);
        let b = Budget::new(Some(10_000));
        assert_eq!(b.step, 0);
        let mut h = Held::new(&b);
        h.try_grow(10).unwrap();
        assert_eq!(b.used(), 10);
        h.shrink(10);
        assert_eq!(b.used(), 0);
    }

    #[test]
    fn absurd_charges_fail() {
        let b = Budget::new(Some(1 << 20));
        assert!(Held::new(&b).try_grow(usize::MAX).is_err());
        assert_eq!(b.used(), 0);
        assert!(Meter::new(&b, true).add(usize::MAX).is_err());
        assert_eq!(b.used(), 0);
    }
}

#[cfg(test)]
mod parent_tests;

mod warnings;
pub(crate) use warnings::{WorkingWarnings, format_len};
