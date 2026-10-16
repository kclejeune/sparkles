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

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A run went over its memory limit.
#[derive(Debug, Clone, thiserror::Error)]
#[error("reasoning exceeded the memory limit of {limit} bytes{context}")]
pub(crate) struct MemoryExceeded {
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

/// The memory a run has charged, its peak and its limit.
pub(crate) struct Budget {
    limit: usize,
    used: AtomicUsize,
    peak: AtomicUsize,
}

impl std::fmt::Debug for Budget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Budget")
            .field("limit", &self.limit)
            .field("used", &self.used())
            .field("peak", &self.peak())
            .finish()
    }
}

impl Budget {
    pub fn new(limit: Option<usize>) -> Arc<Budget> {
        Arc::new(Budget {
            limit: limit.unwrap_or(usize::MAX),
            used: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        })
    }

    pub fn unlimited() -> Arc<Budget> {
        Budget::new(None)
    }

    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }

    fn exceeded(&self) -> MemoryExceeded {
        MemoryExceeded {
            limit: self.limit,
            context: String::new(),
        }
    }

    /// Charge `n` bytes, or fail without charging when that passes the limit.
    pub fn try_charge(&self, n: usize) -> Result<(), MemoryExceeded> {
        if n == 0 {
            return Ok(());
        }
        let mut cur = self.used.load(Ordering::Relaxed);
        loop {
            let next = match cur.checked_add(n) {
                Some(x) if x <= self.limit => x,
                _ => return Err(self.exceeded()),
            };
            match self
                .used
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => {
                    self.peak.fetch_max(next, Ordering::Relaxed);
                    return Ok(());
                }
                Err(x) => cur = x,
            }
        }
    }

    /// Charge `n` bytes that are already allocated, even past the limit.
    pub fn charge(&self, n: usize) {
        if n == 0 {
            return;
        }
        let next = self.used.fetch_add(n, Ordering::AcqRel).saturating_add(n);
        self.peak.fetch_max(next, Ordering::Relaxed);
    }

    pub fn release(&self, n: usize) {
        if n > 0 {
            self.used.fetch_sub(n, Ordering::AcqRel);
        }
    }

    /// Fail when the charges passed the limit (through [`charge`](Self::charge)).
    pub fn check(&self) -> Result<(), MemoryExceeded> {
        if self.used() > self.limit {
            return Err(self.exceeded());
        }
        Ok(())
    }

    /// Fail when the charges ever passed the limit, also when they are back under it:
    /// a run that reports success reports a peak within its limit.
    pub fn check_peak(&self) -> Result<(), MemoryExceeded> {
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
}

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Held({})", self.bytes)
    }
}

impl Held {
    pub fn new(budget: &Arc<Budget>) -> Held {
        Held {
            budget: budget.clone(),
            bytes: 0,
        }
    }

    /// `n` bytes charged, or the error when they do not fit.
    pub fn try_new(budget: &Arc<Budget>, n: usize) -> Result<Held, MemoryExceeded> {
        let mut h = Held::new(budget);
        h.try_grow(n)?;
        Ok(h)
    }

    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn budget(&self) -> &Arc<Budget> {
        &self.budget
    }

    pub fn try_grow(&mut self, n: usize) -> Result<(), MemoryExceeded> {
        self.budget.try_charge(n)?;
        self.bytes += n;
        Ok(())
    }

    pub fn grow(&mut self, n: usize) {
        self.budget.charge(n);
        self.bytes += n;
    }

    pub fn shrink(&mut self, n: usize) {
        let n = n.min(self.bytes);
        self.budget.release(n);
        self.bytes -= n;
    }

    /// Hold `n` bytes: grow (even past the limit) or shrink to it.
    pub fn set(&mut self, n: usize) {
        if n > self.bytes {
            self.grow(n - self.bytes);
        } else {
            self.shrink(self.bytes - n);
        }
    }

    /// Move `n` of these bytes into a new holder.
    pub fn split(&mut self, n: usize) -> Held {
        let n = n.min(self.bytes);
        self.bytes -= n;
        Held {
            budget: self.budget.clone(),
            bytes: n,
        }
    }

    /// Take the bytes of `other` over.
    pub fn absorb(&mut self, mut other: Held) {
        self.bytes += std::mem::take(&mut other.bytes);
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
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
}

impl<'b> Meter<'b> {
    pub fn new(budget: &'b Budget, strict: bool) -> Meter<'b> {
        Meter {
            budget,
            strict,
            bytes: 0,
        }
    }

    pub fn add(&mut self, n: usize) -> Result<(), MemoryExceeded> {
        if self.strict {
            self.budget.try_charge(n)?;
        } else {
            self.budget.charge(n);
        }
        self.bytes += n;
        Ok(())
    }

    pub fn sub(&mut self, n: usize) {
        let n = n.min(self.bytes);
        self.budget.release(n);
        self.bytes -= n;
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

    /// The bytes charged, for a [`Held::adopt`] of the same budget.
    pub fn into_bytes(mut self) -> usize {
        std::mem::take(&mut self.bytes)
    }
}

impl Drop for Meter<'_> {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
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
        h.shrink(90);
        assert_eq!(b.used(), 10);
        b.charge(200);
        assert!(b.check().is_err());
        b.release(200);
        drop(h);
        assert_eq!((b.used(), b.peak()), (0, 210));
    }
}
