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
        self.waiting.load(Ordering::SeqCst)
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
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |g| {
                g.checked_add(bytes).filter(|n| *n <= limit)
            })
            .is_ok()
    }

    fn release(self: &Arc<Self>, bytes: u64) {
        if bytes == 0 {
            return;
        }
        // `granted` is written before `waiting` is read, and a waiter writes `waiting`
        // before it reads `granted`. Sequential consistency on both makes sure that at
        // least one side sees the other, so a release never skips the wake-up of a
        // waiter that did not see the freed memory.
        self.granted.fetch_sub(bytes, Ordering::SeqCst);
        if self.waiting.load(Ordering::SeqCst) > 0 {
            self.wake();
        }
    }

    fn wake(&self) {
        // the callback runs outside the lock, so that it may poll its ticket
        let next = self.next_to_serve(&self.queue.lock());
        if let Some((_, Some(f))) = next {
            f();
        }
        self.changed.notify_all();
    }

    /// The wake-up of the first waiter that [`Ticket::poll`] would serve now, if it has
    /// one. Only that waiter is woken. When it takes its grant it wakes the next one that
    /// fits, so a release wakes the waiters that fit one after another instead of all of
    /// them. A waiter that was woken but finds the memory taken again passes the wake-up
    /// on in the same way. Blocking waiters have no callback and wait on `changed`.
    fn next_to_serve(&self, q: &Queue) -> Option<(u64, Option<Waker>)> {
        let granted = self.granted.load(Ordering::SeqCst);
        (0..q.waiters.len())
            .find(|&pos| {
                let w = &q.waiters[pos];
                granted.saturating_add(w.bytes) <= self.capacity
                    && may_pass(q, pos, granted, self.capacity)
            })
            .map(|pos| (q.waiters[pos].ticket, q.waiters[pos].wake.clone()))
    }

    /// The most a request can be granted at admission: a pool smaller than the usual
    /// initial grant still serves requests.
    fn clamp(&self, bytes: u64) -> u64 {
        round_up(bytes).min(self.capacity)
    }

    /// A grant of `bytes` if the pool has them and nobody is queued ahead.
    pub fn try_grant(self: &Arc<Self>, bytes: u64) -> Option<PoolGrant> {
        let bytes = self.clamp(bytes);
        // under the queue's lock, so that a request cannot pass one that is queued
        let q = self.queue.lock();
        if q.waiters.is_empty() && self.reserve(bytes, self.capacity) {
            drop(q);
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
        let bytes = self.clamp(bytes);
        let mut q = self.queue.lock();
        let ticket = q.next_ticket;
        q.next_ticket += 1;
        q.waiters.push_back(Waiter {
            ticket,
            bytes,
            wake,
        });
        self.waiting.store(q.waiters.len(), Ordering::SeqCst);
        Ticket {
            pool: self.clone(),
            ticket,
            bytes,
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

/// Whether the waiter at `pos` may be served before the waiters ahead of it. The head
/// may. A waiter behind it may pass only waiters that are larger and wait for memory
/// (they do not fit now), and only a few times in a row.
fn may_pass(q: &Queue, pos: usize, granted: u64, capacity: u64) -> bool {
    let me = q.waiters[pos].bytes;
    pos == 0
        || (q.bypassed < MAX_BYPASS
            && q.waiters
                .iter()
                .take(pos)
                .all(|w| w.bytes > me && granted.saturating_add(w.bytes) > capacity))
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
        let granted = pool.granted.load(Ordering::SeqCst);
        if !may_pass(&q, pos, granted, pool.capacity) || !pool.reserve(self.bytes, pool.capacity) {
            // This waiter may have been the one a release woke, and a running grant may
            // have grown into the memory since. Another waiter may fit in what is left,
            // so the wake-up goes on to it rather than ending here.
            let next = pool.next_to_serve(&q);
            drop(q);
            if let Some((ticket, Some(f))) = next
                && ticket != self.ticket
            {
                f();
            }
            return None;
        }
        q.waiters.remove(pos);
        q.bypassed = if pos == 0 { 0 } else { q.bypassed + 1 };
        pool.waiting.store(q.waiters.len(), Ordering::SeqCst);
        pool.admitted.fetch_add(1, Ordering::Relaxed);
        self.done = true;
        // the next waiter may fit in what is left
        let next = pool.next_to_serve(&q);
        drop(q);
        if let Some((_, Some(f))) = next {
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
        if q.waiters.front().is_some_and(|w| w.ticket == self.ticket) {
            // the next head starts with no bypasses behind it
            q.bypassed = 0;
        }
        q.waiters.retain(|w| w.ticket != self.ticket);
        self.pool.waiting.store(q.waiters.len(), Ordering::SeqCst);
        drop(q);
        // a smaller request behind this one may fit now
        self.pool.wake();
    }
}

/// Bytes reserved from a [`QueryPool`] for one query, returned when the last owner drops.
///
/// The grant adds every eager context's admitted peak to live cursor and direct
/// charges. Contexts sharing one request grant retain independent ownership.
pub struct PoolGrant {
    pool: Arc<QueryPool>,
    granted: AtomicU64,
    /// Sum of eager context contributions, live cursor charges and direct charges.
    used: AtomicU64,
    /// the most the grant has held
    peak: AtomicU64,
    /// serializes growth and trimming of this grant
    resize: Mutex<()>,
}

impl std::fmt::Debug for PoolGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolGrant")
            .field("granted", &self.granted())
            .field("used", &self.used())
            .finish()
    }
}

impl PoolGrant {
    fn with(pool: Arc<QueryPool>, bytes: u64) -> PoolGrant {
        PoolGrant {
            pool,
            granted: AtomicU64::new(bytes),
            used: AtomicU64::new(0),
            peak: AtomicU64::new(bytes),
            resize: Mutex::new(()),
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

    /// The bytes charged against this grant now.
    #[inline]
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::SeqCst)
    }

    /// The most bytes this grant has held at once.
    pub fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }

    /// Account for `bytes` more and make the grant cover them. Fails, accounting for
    /// nothing, when the pool cannot give the granules. While requests wait for
    /// admission, one query may not hold more than half the pool.
    #[inline]
    pub fn charge(&self, bytes: u64) -> Result<(), Error> {
        let used = self.add_usage(bytes)?;
        if used > self.granted.load(Ordering::SeqCst) {
            return self.charge_slow(bytes);
        }
        Ok(())
    }

    /// Add a representable contribution without wrapping another caller's ownership.
    #[inline]
    fn add_usage(&self, bytes: u64) -> Result<u64, Error> {
        self.used
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                used.checked_add(bytes)
            })
            .map(|prior| prior + bytes)
            .map_err(|_| self.pool.exhausted(u64::MAX))
    }

    /// Take a speculative charge out before waiting for the growth lock. Under the
    /// lock, publish its contribution before publishing more grant capacity, so fast
    /// chargers cannot consume capacity meant for this charge. A rejected growth
    /// removes exactly this contribution and leaves concurrent ownership intact.
    #[cold]
    fn charge_slow(&self, bytes: u64) -> Result<(), Error> {
        self.used.fetch_sub(bytes, Ordering::SeqCst);
        let _lock = self.resize.lock();
        let need = self.add_usage(bytes)?;
        if let Err(error) = self.grow_locked(need) {
            self.used.fetch_sub(bytes, Ordering::SeqCst);
            return Err(error);
        }
        Ok(())
    }

    /// Cover a cursor operator's checked output of `bytes` for now. The coverage goes
    /// back with the next trim, since cursor operators charge what they keep.
    pub fn cover_transient(&self, bytes: u64) -> Result<(), Error> {
        let _lock = self.resize.lock();
        self.grow_locked(bytes)
    }

    /// Account for `bytes` less, and give back granules the query no longer needs.
    #[inline]
    pub fn release(&self, bytes: u64) {
        let prior = self.used.fetch_sub(bytes, Ordering::SeqCst);
        debug_assert!(prior >= bytes, "grant released {bytes} of {prior} bytes");
        let used = prior.wrapping_sub(bytes);
        if self.granted.load(Ordering::Relaxed) > used.saturating_add(SHRINK_SLACK + GRANULE) {
            self.trim_slow();
        }
    }

    /// Make the grant cover `need` bytes, reserving the granules from the pool. Runs
    /// under the grant's lock, so a second thread that needs the same granules sees the
    /// new total instead of competing for them.
    fn grow_locked(&self, need: u64) -> Result<(), Error> {
        let capacity = self.pool.capacity;
        let need = need.max(self.used.load(Ordering::SeqCst));
        let cur = self.granted.load(Ordering::SeqCst);
        if cur >= need {
            return Ok(());
        }
        if need > capacity {
            return Err(self.pool.exhausted(need));
        }
        let target = round_up(need).min(capacity);
        let share = if self.pool.waiting() > 0 {
            capacity / 2
        } else {
            capacity
        };
        if target > share.max(INITIAL_GRANT.min(capacity)) {
            return Err(self.pool.exhausted(need));
        }
        if !self.pool.reserve(target - cur, capacity) {
            return Err(self.pool.exhausted(need));
        }
        self.granted.store(target, Ordering::SeqCst);
        self.peak.fetch_max(target, Ordering::Relaxed);
        Ok(())
    }

    /// Give back granules when the query holds well over what it uses. The pool wakes
    /// waiting requests while the grant's lock is held, so a waker must never charge or
    /// release this grant.
    #[cold]
    fn trim_slow(&self) {
        let _lock = self.resize.lock();
        let cur = self.granted.load(Ordering::SeqCst);
        let keep = |used: u64| round_up(used).max(INITIAL_GRANT.min(cur)).min(cur);
        let t1 = keep(self.used.load(Ordering::SeqCst));
        if t1 >= cur {
            return;
        }
        // A charger writes `used` and then reads `granted`. The trimmer writes
        // `granted` and then reads `used`. Both are sequentially consistent, so either
        // the trimmer sees the charge and keeps its bytes, or the charger sees the lower
        // grant and goes to `grow`, which waits for the lock. The bytes go back to the
        // pool only after the second read. A slow charger also publishes its own
        // usage before growing the grant under this lock, so a newly published
        // granule cannot be accepted twice.
        self.granted.store(t1, Ordering::SeqCst);
        let t2 = keep(self.used.load(Ordering::SeqCst)).max(t1);
        if t2 != t1 {
            self.granted.store(t2, Ordering::SeqCst);
        }
        self.pool.release(cur - t2);
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
        g.charge(9 * MB).unwrap();
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
        match b.charge(20 * MB) {
            Err(Error::BudgetExceeded(x)) => {
                assert_eq!(x.kind, BudgetKind::ServerMemory);
                assert_eq!(x.limit, 32 * MB);
            }
            other => panic!("{other:?}"),
        }
        // the failed growth took nothing
        assert_eq!(pool.granted(), 24 * MB);
        assert_eq!(b.used(), 0);
        drop(a);
        b.charge(20 * MB).unwrap();
    }

    #[test]
    fn grants_shrink_when_the_query_releases_a_lot() {
        let pool = QueryPool::new(256 * MB);
        let g = pool.empty_grant();
        g.charge(100 * MB).unwrap();
        g.release(10 * MB);
        assert_eq!(g.granted(), 100 * MB);
        // a grant keeps its first 16 MiB, so that small queries do not churn the pool
        g.release(80 * MB);
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
    fn a_pool_smaller_than_the_initial_grant_still_admits() {
        let pool = QueryPool::new(8 * MB);
        let g = pool.try_grant(INITIAL_GRANT).unwrap();
        assert_eq!(g.granted(), 8 * MB);
        assert!(pool.try_grant(MB).is_none());
        drop(g);
        let g = pool.empty_grant();
        g.charge(5 * MB).unwrap();
        assert_eq!(g.granted(), 8 * MB);
        assert!(g.charge(9 * MB).is_err());
    }

    #[test]
    fn a_waiter_does_not_pass_a_head_that_fits() {
        let pool = QueryPool::new(32 * MB);
        let a = pool.try_grant(16 * MB).unwrap();
        // the head wants 24 and waits. a smaller request behind it may pass
        let mut head = pool.enter(24 * MB, None);
        assert!(head.poll().is_none());
        let mut small = pool.enter(8 * MB, None);
        let s = small.poll().unwrap();
        drop(s);
        drop(a);
        // the head now fits, so a request behind it waits its turn
        let mut late = pool.enter(8 * MB, None);
        assert!(late.poll().is_none());
        assert!(head.poll().is_some());
    }

    #[test]
    fn growth_is_limited_to_half_the_pool_while_requests_wait() {
        let pool = QueryPool::new(64 * MB);
        let g = pool.empty_grant();
        g.charge(40 * MB).unwrap();
        let other = pool.try_grant(24 * MB).unwrap();
        let mut waiter = pool.enter(16 * MB, None);
        assert!(waiter.poll().is_none());
        assert!(g.charge(4 * MB).is_err());
        drop(other);
        assert!(waiter.poll().is_some());
    }

    /// A6: many clients that each want 40% of the pool end with a result or a
    /// server-memory failure, none hangs, and the pool returns to zero.
    #[test]
    fn many_large_queries_never_deadlock() {
        let pool = QueryPool::new(100 * MB);
        let outcomes = std::sync::Mutex::new((0u32, 0u32));
        std::thread::scope(|s| {
            for _ in 0..64 {
                s.spawn(|| {
                    for _ in 0..5 {
                        match pool.admit_blocking(16 * MB, Duration::from_millis(300), || false) {
                            Ok(g) => {
                                let r = g.charge(40 * MB);
                                std::thread::sleep(Duration::from_millis(2));
                                let mut o = outcomes.lock().unwrap();
                                if r.is_ok() {
                                    o.0 += 1;
                                } else {
                                    o.1 += 1;
                                }
                            }
                            Err(e) => {
                                assert!(matches!(
                                    e,
                                    Error::BudgetExceeded(b) if b.kind == BudgetKind::ServerMemory
                                ));
                                outcomes.lock().unwrap().1 += 1;
                            }
                        }
                    }
                });
            }
        });
        let o = outcomes.lock().unwrap();
        assert_eq!(o.0 + o.1, 320);
        assert!(o.0 > 0);
        assert_eq!(pool.granted(), 0);
        assert_eq!(pool.waiting(), 0);
    }

    #[test]
    fn slow_growth_never_accepts_more_than_capacity() {
        let pool = QueryPool::new(20 * MB);
        let barrier = std::sync::Barrier::new(3);
        let slot: Mutex<Option<Arc<PoolGrant>>> = Mutex::new(None);
        let accepted = AtomicUsize::new(0);
        let mut unbacked = 0;
        std::thread::scope(|scope| {
            for (bit, bytes) in [(1, MB), (2, 4 * MB)] {
                let (barrier, slot, accepted) = (&barrier, &slot, &accepted);
                scope.spawn(move || {
                    for _ in 0..100_000 {
                        barrier.wait();
                        let grant = slot.lock().clone().unwrap();
                        let charged = grant.charge(bytes).is_ok();
                        if charged {
                            accepted.fetch_or(bit, Ordering::SeqCst);
                        }
                        barrier.wait();
                        barrier.wait();
                        if charged {
                            grant.release(bytes);
                        }
                        drop(grant);
                        barrier.wait();
                    }
                });
            }
            for _ in 0..100_000 {
                let grant = Arc::new(pool.try_grant(16 * MB).unwrap());
                grant.charge(16 * MB).unwrap();
                *slot.lock() = Some(grant.clone());
                accepted.store(0, Ordering::SeqCst);
                barrier.wait();
                barrier.wait();
                let bits = accepted.load(Ordering::SeqCst);
                let bytes = 16 * MB
                    + if bits & 1 != 0 { MB } else { 0 }
                    + if bits & 2 != 0 { 4 * MB } else { 0 };
                if bits == 3 || bytes > pool.capacity() || grant.granted() < bytes {
                    unbacked += 1;
                }
                assert_eq!(grant.used(), bytes);
                barrier.wait();
                barrier.wait();
                grant.release(16 * MB);
                assert_eq!(grant.used(), 0);
                slot.lock().take();
                drop(grant);
                assert_eq!(pool.granted(), 0);
            }
        });
        assert_eq!(unbacked, 0, "rounds accepting usage beyond the grant");
    }

    #[test]
    fn failed_slow_growth_leaves_the_grant_reusable() {
        let pool = QueryPool::new(20 * MB);
        let grant = pool.try_grant(16 * MB).unwrap();
        grant.charge(17 * MB).unwrap();
        assert!(matches!(grant.charge(4 * MB), Err(Error::BudgetExceeded(b))
            if b.kind == BudgetKind::ServerMemory));
        assert_eq!(grant.used(), 17 * MB);
        assert_eq!(grant.granted(), 20 * MB);
        grant.release(17 * MB);
        grant.charge(20 * MB).unwrap();
        assert_eq!(grant.used(), 20 * MB);
        grant.release(20 * MB);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn overflowing_charge_preserves_existing_ownership() {
        let pool = QueryPool::new(u64::MAX);
        let grant = pool.empty_grant();
        grant.charge(u64::MAX - 1).unwrap();
        let before = (grant.used(), grant.granted(), pool.granted());
        assert!(matches!(grant.charge(2), Err(Error::BudgetExceeded(b))
            if b.kind == BudgetKind::ServerMemory));
        assert_eq!((grant.used(), grant.granted(), pool.granted()), before);
        grant.release(u64::MAX - 1);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn growth_of_one_grant_from_two_threads_never_fails_spuriously() {
        let pool = QueryPool::new(20 * MB);
        let barrier = std::sync::Barrier::new(2);
        let failures = AtomicU64::new(0);
        let slot: Mutex<Option<Arc<PoolGrant>>> = Mutex::new(None);
        std::thread::scope(|s| {
            for bytes in [MB, 2 * MB] {
                let (pool, barrier, failures, slot) = (&pool, &barrier, &failures, &slot);
                s.spawn(move || {
                    for _ in 0..20_000 {
                        barrier.wait();
                        if bytes == MB {
                            let g = pool.try_grant(16 * MB).unwrap();
                            g.charge(16 * MB).unwrap();
                            *slot.lock() = Some(Arc::new(g));
                        }
                        barrier.wait();
                        let g = slot.lock().clone().unwrap();
                        barrier.wait();
                        // used goes from 16 to 19 MiB, which needs the same granule
                        if g.charge(bytes).is_err() {
                            failures.fetch_add(1, Ordering::SeqCst);
                        } else {
                            g.release(bytes);
                        }
                        barrier.wait();
                        if bytes == MB {
                            slot.lock().take();
                        }
                        drop(g);
                    }
                });
            }
        });
        assert_eq!(failures.load(Ordering::SeqCst), 0);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn trim_rereads_usage_before_returning_memory() {
        let pool = QueryPool::new(64 * MB);
        let g = pool.empty_grant();
        g.charge(48 * MB).unwrap();
        // a trim that starts after a charge must keep the charge covered
        g.trim_slow();
        assert!(g.granted() >= 48 * MB, "{}", g.granted());
        assert!(pool.granted() >= 48 * MB, "{}", pool.granted());
        g.release(48 * MB);
        assert_eq!(g.granted(), 16 * MB);
        assert_eq!(pool.granted(), 16 * MB);
        assert_eq!(g.used(), 0);
    }

    #[test]
    fn an_empty_grant_never_trims_and_zero_releases_change_nothing() {
        let pool = QueryPool::new(64 * MB);
        let g = pool.empty_grant();
        g.release(0);
        assert_eq!((g.granted(), g.used()), (0, 0));
        g.charge(0).unwrap();
        assert_eq!((g.granted(), g.used()), (0, 0));
    }

    #[test]
    fn a_release_wakes_only_the_waiters_that_fit() {
        use std::sync::atomic::AtomicUsize;
        let pool = QueryPool::new(64 * MB);
        let first = pool.try_grant(32 * MB).unwrap();
        let second = pool.try_grant(32 * MB).unwrap();
        let woken: Arc<[AtomicUsize; 3]> = Arc::new(Default::default());
        let mut tickets: Vec<Ticket> = (0..3)
            .map(|i| {
                let w = woken.clone();
                pool.enter(
                    16 * MB,
                    Some(Arc::new(move || {
                        w[i].fetch_add(1, Ordering::SeqCst);
                    })),
                )
            })
            .collect();
        let counts = || {
            woken
                .iter()
                .map(|c| c.load(Ordering::SeqCst))
                .collect::<Vec<_>>()
        };
        // 32 MiB come back: only the head fits first
        drop(first);
        assert_eq!(counts(), [1, 0, 0]);
        let a = tickets[0].poll().unwrap();
        // the head took 16 MiB, so the second waiter fits in the rest
        assert_eq!(counts(), [1, 1, 0]);
        let b = tickets[1].poll().unwrap();
        // nothing is left for the third
        assert_eq!(counts(), [1, 1, 0]);
        assert!(tickets[2].poll().is_none());
        // a growth that fails wakes nobody, and the next release wakes the third
        b.charge(20 * MB).unwrap_err();
        assert_eq!(counts(), [1, 1, 0]);
        drop(a);
        assert_eq!(counts(), [1, 1, 1]);
        assert!(tickets[2].poll().is_some());
        drop((b, second, tickets));
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn a_woken_waiter_that_no_longer_fits_wakes_the_next() {
        use std::sync::atomic::AtomicUsize;
        let pool = QueryPool::new(64 * MB);
        let freed = pool.try_grant(32 * MB).unwrap();
        let grows = pool.try_grant(8 * MB).unwrap();
        let _other = pool.try_grant(24 * MB).unwrap();
        let woken: Arc<[AtomicUsize; 2]> = Arc::new(Default::default());
        let mut tickets: Vec<Ticket> = [32 * MB, 16 * MB]
            .into_iter()
            .enumerate()
            .map(|(i, bytes)| {
                let w = woken.clone();
                pool.enter(
                    bytes,
                    Some(Arc::new(move || {
                        w[i].fetch_add(1, Ordering::SeqCst);
                    })),
                )
            })
            .collect();
        let counts = || {
            woken
                .iter()
                .map(|c| c.load(Ordering::SeqCst))
                .collect::<Vec<_>>()
        };
        drop(freed);
        assert_eq!(counts(), [1, 0]);
        // a running grant takes 16 of the 32 MiB before the head polls
        grows.charge(24 * MB).unwrap();
        assert!(tickets[0].poll().is_none());
        // the smaller waiter fits in the 16 MiB left and may pass the head
        assert_eq!(counts(), [1, 1]);
        assert!(tickets[1].poll().is_some());
        drop(tickets.remove(0));
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
                        let bytes = ((t + i) % 40 + 1) * MB;
                        if g.charge(bytes).is_ok() {
                            g.release(bytes);
                        }
                    }
                });
            }
        });
        assert_eq!(pool.granted(), 0);
    }
}
