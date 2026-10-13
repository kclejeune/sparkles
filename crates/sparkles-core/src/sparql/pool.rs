//! The query memory pool (spec X08 §4).
//!
//! A [`QueryPool`] bounds the estimated memory of all running queries together. Each
//! query holds a [`PoolGrant`], a number of bytes reserved from the pool in granules of
//! 4 MiB. The query's own accounting (`Ctx::charge`) stays authoritative and checks its
//! own budget first. The grant only follows it: a charge that passes the grant asks the
//! pool for more granules, and when the pool cannot give them the charge fails with
//! [`BudgetKind::ServerMemory`]. A query never waits for memory while it holds a grant,
//! so the pool cannot deadlock. The only wait is admission, where a request that holds
//! nothing queues in arrival order for its initial grant.

use crate::error::{Budget, BudgetKind, Error};
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The size in which grants grow and shrink.
pub const GRANULE: u64 = 4 << 20;

/// What a request asks for at admission.
pub const INITIAL_GRANT: u64 = 16 << 20;

/// A grant gives back memory only when the query holds this much more than it uses.
const SHRINK_SLACK: u64 = 16 << 20;

/// How often a smaller waiter may pass the one at the head of the queue before the head
/// is served first.
const MAX_BYPASS: u32 = 8;

fn round_up(bytes: u64) -> u64 {
    bytes.div_ceil(GRANULE).saturating_mul(GRANULE)
}

type Waker = Arc<dyn Fn() + Send + Sync>;

struct Waiter {
    ticket: u64,
    bytes: u64,
    wake: Option<Waker>,
}

#[derive(Default)]
struct Queue {
    next_ticket: u64,
    waiters: VecDeque<Waiter>,
    /// how many times in a row a waiter behind the head was served first
    bypassed: u32,
}

/// A byte budget for the queries of one process or catalog.
pub struct QueryPool {
    capacity: u64,
    granted: AtomicU64,
    /// requests waiting for admission, read without the queue's lock
    waiting: AtomicUsize,
    queue: Mutex<Queue>,
    changed: Condvar,
    admitted: AtomicU64,
    rejected: AtomicU64,
}

impl std::fmt::Debug for QueryPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryPool")
            .field("capacity", &self.capacity)
            .field("granted", &self.granted())
            .finish()
    }
}

/// A snapshot of the pool for `/$/server/memory` and the metrics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolStats {
    pub capacity: u64,
    pub granted: u64,
    pub waiting: usize,
    pub admitted: u64,
    pub rejected: u64,
}

impl QueryPool {
    pub fn new(capacity: u64) -> Arc<QueryPool> {
        Arc::new(QueryPool {
            capacity,
            granted: AtomicU64::new(0),
            waiting: AtomicUsize::new(0),
            queue: Mutex::new(Queue::default()),
            changed: Condvar::new(),
            admitted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        })
    }

    pub fn capacity(&self) -> u64 {
        self.capacity
    }
    pub fn granted(&self) -> u64 {
        self.granted.load(Ordering::Relaxed)
    }
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::Relaxed)
    }
    pub fn stats(&self) -> PoolStats {
        PoolStats {
            capacity: self.capacity,
            granted: self.granted(),
            waiting: self.waiting(),
            admitted: self.admitted.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
        }
    }

    /// Count a request that gave up waiting (the server answers it `503`).
    pub fn note_rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    /// Take `bytes` from the pool if `limit` allows it. `limit` is the most the pool
    /// may have granted after the call.
    fn reserve(&self, bytes: u64, limit: u64) -> bool {
        self.granted
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |g| {
                g.checked_add(bytes).filter(|n| *n <= limit)
            })
            .is_ok()
    }

    fn release(self: &Arc<Self>, bytes: u64) {
        if bytes == 0 {
            return;
        }
        self.granted.fetch_sub(bytes, Ordering::AcqRel);
        if self.waiting() > 0 {
            self.wake();
        }
    }

    fn wake(&self) {
        let q = self.queue.lock();
        for w in &q.waiters {
            if let Some(f) = &w.wake {
                f();
            }
        }
        drop(q);
        self.changed.notify_all();
    }

    /// A grant of `bytes` if the pool has them and nobody is queued ahead.
    pub fn try_grant(self: &Arc<Self>, bytes: u64) -> Option<PoolGrant> {
        let bytes = round_up(bytes);
        if self.waiting() == 0 && self.reserve(bytes, self.capacity) {
            self.admitted.fetch_add(1, Ordering::Relaxed);
            return Some(PoolGrant::with(self.clone(), bytes));
        }
        None
    }

    /// An empty grant, which grows as the query's charges need.
    pub fn empty_grant(self: &Arc<Self>) -> PoolGrant {
        PoolGrant::with(self.clone(), 0)
    }

    /// Join the admission queue for an initial grant of `bytes`. `wake` is called when
    /// the pool's state changes, so that an async caller can poll again.
    pub fn enter(self: &Arc<Self>, bytes: u64, wake: Option<Waker>) -> Ticket {
        let mut q = self.queue.lock();
        let ticket = q.next_ticket;
        q.next_ticket += 1;
        q.waiters.push_back(Waiter {
            ticket,
            bytes: round_up(bytes),
            wake,
        });
        self.waiting.store(q.waiters.len(), Ordering::Relaxed);
        Ticket {
            pool: self.clone(),
            ticket,
            bytes: round_up(bytes),
            done: false,
        }
    }

    /// Wait up to `wait` for an initial grant of `bytes`, or until `cancelled` returns
    /// true (it is checked at least every 50 ms). For callers that may block a thread.
    pub fn admit_blocking(
        self: &Arc<Self>,
        bytes: u64,
        wait: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<PoolGrant, Error> {
        if let Some(g) = self.try_grant(bytes) {
            return Ok(g);
        }
        let mut t = self.enter(bytes, None);
        let end = Instant::now() + wait;
        loop {
            if let Some(g) = t.poll() {
                return Ok(g);
            }
            let now = Instant::now();
            if cancelled() {
                return Err(Error::Cancelled);
            }
            if now >= end {
                self.note_rejected();
                return Err(self.exhausted(bytes));
            }
            let mut q = self.queue.lock();
            self.changed
                .wait_for(&mut q, (end - now).min(Duration::from_millis(50)));
        }
    }

    /// The error of a request the pool could not admit or a query it could not grow.
    pub fn exhausted(&self, requested: u64) -> Error {
        Error::BudgetExceeded(Budget {
            kind: BudgetKind::ServerMemory,
            limit: self.capacity,
            requested,
        })
    }
}

/// A place in the admission queue. Dropping it leaves the queue.
pub struct Ticket {
    pool: Arc<QueryPool>,
    ticket: u64,
    bytes: u64,
    done: bool,
}

impl Ticket {
    /// The grant, if this request can be served now. The head of the queue is served
    /// first. A request behind it may pass when it needs less than the head does, at
    /// most eight times in a row.
    pub fn poll(&mut self) -> Option<PoolGrant> {
        if self.done {
            return None;
        }
        let pool = self.pool.clone();
        let mut q = pool.queue.lock();
        let pos = q.waiters.iter().position(|w| w.ticket == self.ticket)?;
        let head_bytes = q.waiters.front().map_or(0, |w| w.bytes);
        let may_pass = pos == 0 || (self.bytes < head_bytes && q.bypassed < MAX_BYPASS);
        if !may_pass || !pool.reserve(self.bytes, pool.capacity) {
            return None;
        }
        q.waiters.remove(pos);
        q.bypassed = if pos == 0 { 0 } else { q.bypassed + 1 };
        pool.waiting.store(q.waiters.len(), Ordering::Relaxed);
        pool.admitted.fetch_add(1, Ordering::Relaxed);
        self.done = true;
        // the next waiter may fit now
        let next: Vec<Waker> = q.waiters.iter().filter_map(|w| w.wake.clone()).collect();
        drop(q);
        for f in next {
            f();
        }
        pool.changed.notify_all();
        Some(PoolGrant::with(pool.clone(), self.bytes))
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut q = self.pool.queue.lock();
        q.waiters.retain(|w| w.ticket != self.ticket);
        self.pool.waiting.store(q.waiters.len(), Ordering::Relaxed);
        drop(q);
        // a smaller request behind this one may fit now
        self.pool.wake();
    }
}

/// Bytes reserved from a [`QueryPool`] for one query, returned when the last owner drops.
pub struct PoolGrant {
    pool: Arc<QueryPool>,
    granted: AtomicU64,
}

impl std::fmt::Debug for PoolGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolGrant")
            .field("granted", &self.granted())
            .finish()
    }
}

impl PoolGrant {
    fn with(pool: Arc<QueryPool>, bytes: u64) -> PoolGrant {
        PoolGrant {
            pool,
            granted: AtomicU64::new(bytes),
        }
    }

    pub fn pool(&self) -> &Arc<QueryPool> {
        &self.pool
    }

    /// The bytes this grant holds now.
    #[inline]
    pub fn granted(&self) -> u64 {
        self.granted.load(Ordering::Relaxed)
    }

    /// Make the grant cover `live` bytes. Fails when the pool cannot give the granules.
    /// While requests wait for admission, one query may not hold more than half the
    /// pool.
    pub fn cover(&self, live: u64) -> Result<(), Error> {
        let target = round_up(live);
        loop {
            let cur = self.granted.load(Ordering::Acquire);
            if cur >= target {
                return Ok(());
            }
            let delta = target - cur;
            let share = if self.pool.waiting() > 0 {
                self.pool.capacity / 2
            } else {
                self.pool.capacity
            };
            if target > share.max(INITIAL_GRANT.min(self.pool.capacity)) {
                return Err(self.pool.exhausted(live));
            }
            if !self.pool.reserve(delta, self.pool.capacity) {
                return Err(self.pool.exhausted(live));
            }
            if self
                .granted
                .compare_exchange(cur, target, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(());
            }
            // another thread of the query grew the grant first
            self.pool.release(delta);
        }
    }

    /// Give back granules when the query holds well over what it uses.
    #[inline]
    pub fn trim(&self, live: u64) {
        let cur = self.granted();
        if cur > live.saturating_add(SHRINK_SLACK + GRANULE) {
            self.trim_slow(live);
        }
    }

    #[cold]
    fn trim_slow(&self, live: u64) {
        let target = round_up(live).max(INITIAL_GRANT.min(self.granted()));
        let cur = self.granted.load(Ordering::Acquire);
        if cur > target
            && self
                .granted
                .compare_exchange(cur, target, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            self.pool.release(cur - target);
        }
    }
}

impl Drop for PoolGrant {
    fn drop(&mut self) {
        self.pool.release(*self.granted.get_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1 << 20;

    #[test]
    fn grants_round_to_granules_and_return_on_drop() {
        let pool = QueryPool::new(64 * MB);
        let g = pool.try_grant(5 * MB).unwrap();
        assert_eq!(g.granted(), 8 * MB);
        assert_eq!(pool.granted(), 8 * MB);
        g.cover(9 * MB).unwrap();
        assert_eq!(g.granted(), 12 * MB);
        assert_eq!(pool.granted(), 12 * MB);
        drop(g);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn a_grant_that_the_pool_cannot_grow_fails_with_server_memory() {
        let pool = QueryPool::new(32 * MB);
        let a = pool.try_grant(16 * MB).unwrap();
        let b = pool.try_grant(8 * MB).unwrap();
        match b.cover(20 * MB) {
            Err(Error::BudgetExceeded(x)) => {
                assert_eq!(x.kind, BudgetKind::ServerMemory);
                assert_eq!(x.limit, 32 * MB);
            }
            other => panic!("{other:?}"),
        }
        // the failed growth took nothing
        assert_eq!(pool.granted(), 24 * MB);
        drop(a);
        b.cover(20 * MB).unwrap();
    }

    #[test]
    fn grants_shrink_when_the_query_releases_a_lot() {
        let pool = QueryPool::new(256 * MB);
        let g = pool.empty_grant();
        g.cover(100 * MB).unwrap();
        g.trim(90 * MB);
        assert_eq!(g.granted(), 100 * MB);
        // a grant keeps its first 16 MiB, so that small queries do not churn the pool
        g.trim(10 * MB);
        assert_eq!(g.granted(), 16 * MB);
        assert_eq!(pool.granted(), 16 * MB);
    }

    #[test]
    fn admission_is_first_come_first_served_and_times_out() {
        let pool = QueryPool::new(32 * MB);
        let a = pool.try_grant(24 * MB).unwrap();
        // 16 MiB does not fit beside 24
        assert!(pool.try_grant(16 * MB).is_none());
        let started = Instant::now();
        let e = pool
            .admit_blocking(16 * MB, Duration::from_millis(100), || false)
            .unwrap_err();
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(matches!(
            e,
            Error::BudgetExceeded(b) if b.kind == BudgetKind::ServerMemory
        ));
        assert_eq!(pool.waiting(), 0);
        assert_eq!(pool.stats().rejected, 1);
        // a waiter is served when memory returns
        let p2 = pool.clone();
        let h = std::thread::spawn(move || {
            p2.admit_blocking(16 * MB, Duration::from_secs(5), || false)
                .map(|g| g.granted())
        });
        while pool.waiting() == 0 {
            std::thread::yield_now();
        }
        // a newcomer queues behind it even though 8 MiB are free
        assert!(pool.try_grant(4 * MB).is_none());
        drop(a);
        assert_eq!(h.join().unwrap().unwrap(), 16 * MB);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn a_smaller_request_may_pass_a_large_waiter_a_few_times() {
        let pool = QueryPool::new(32 * MB);
        let a = pool.try_grant(24 * MB).unwrap();
        let mut big = pool.enter(16 * MB, None);
        assert!(big.poll().is_none());
        let mut grants = Vec::new();
        for i in 0..MAX_BYPASS {
            let mut small = pool.enter(4 * MB, None);
            // one granule of the 8 free fits at a time
            if let Some(g) = small.poll() {
                grants.push(g);
            } else {
                assert!(i >= 2, "{i}");
                break;
            }
        }
        assert_eq!(grants.len(), 2);
        drop(grants);
        drop(a);
        // with the pool free the head is served first
        assert!(big.poll().is_some());
    }

    #[test]
    fn growth_is_limited_to_half_the_pool_while_requests_wait() {
        let pool = QueryPool::new(64 * MB);
        let g = pool.empty_grant();
        g.cover(40 * MB).unwrap();
        let other = pool.try_grant(24 * MB).unwrap();
        let mut waiter = pool.enter(16 * MB, None);
        assert!(waiter.poll().is_none());
        assert!(g.cover(44 * MB).is_err());
        drop(other);
        assert!(waiter.poll().is_some());
    }

    #[test]
    fn concurrent_growth_and_release_balance_to_zero() {
        let pool = QueryPool::new(512 * MB);
        std::thread::scope(|s| {
            for t in 0..8u64 {
                let pool = pool.clone();
                s.spawn(move || {
                    for i in 0..200 {
                        let g = pool.empty_grant();
                        let _ = g.cover(((t + i) % 40 + 1) * MB);
                        g.trim(MB);
                    }
                });
            }
        });
        assert_eq!(pool.granted(), 0);
    }
}
