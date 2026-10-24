//! Live queries (spec F13 §2.3): a query whose result a subscriber receives once and
//! then as the change of each commit that changes it.
//!
//! A [`LiveQuery`] is a subscription to a circuit that the [`Runtime`](super::Runtime)
//! keeps, shared with every view and live query that asks the same question with the
//! same access. The runtime's passes leave events in the subscription's queue, and
//! [`LiveQuery::next`] or [`LiveQuery::recv`] takes them out. The rows of an event are
//! decoded once per pass and shared by every subscriber.

use super::compile::Kind;
use oxrdf::Term;
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// A row of a live query's result with its multiplicity in an event.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LiveRow {
    /// the row's terms in the order of the query's columns (`None`: unbound)
    pub terms: Vec<Option<Term>>,
    /// how many times the row was added or removed
    pub count: u64,
}

/// An event of a live query.
#[derive(Clone, Debug)]
pub enum LiveEvent {
    /// The whole result at `commit`. `reset` is set when it replaces a copy the
    /// subscriber already holds, after a change the runtime could not follow
    /// commit by commit. An ASK query's result is `value`.
    Snapshot {
        commit: u64,
        vars: Arc<[String]>,
        rows: Arc<[LiveRow]>,
        value: Option<bool>,
        reset: bool,
    },
    /// The change of the result from commit `from` to commit `commit`.
    Diff {
        commit: u64,
        from: u64,
        removed: Arc<[LiveRow]>,
        added: Arc<[LiveRow]>,
    },
    /// An ASK query's value changed between commit `from` and commit `commit`.
    Value { commit: u64, from: u64, value: bool },
    /// The subscription ended with this error (`over-budget`, `pass-timeout`,
    /// `view-failed`, `forbidden`).
    Error { code: String, message: String },
}

impl LiveEvent {
    /// The commit the event brings the subscriber to.
    pub fn commit(&self) -> Option<u64> {
        match self {
            LiveEvent::Snapshot { commit, .. }
            | LiveEvent::Diff { commit, .. }
            | LiveEvent::Value { commit, .. } => Some(*commit),
            LiveEvent::Error { .. } => None,
        }
    }

    /// The rows the event carries, for queue bounds.
    fn rows(&self) -> usize {
        match self {
            LiveEvent::Snapshot { rows, .. } => rows.len(),
            LiveEvent::Diff { removed, added, .. } => removed.len() + added.len(),
            _ => 1,
        }
    }

    /// One event with the effect of `self` followed by `next` (spec F13 §2.3,
    /// `coalesceMs`), or `None` when they do not merge (an error, or a change that does
    /// not follow on).
    pub fn merge(&self, next: &LiveEvent) -> Option<LiveEvent> {
        match (self, next) {
            (
                LiveEvent::Diff {
                    from,
                    commit,
                    removed,
                    added,
                },
                LiveEvent::Diff {
                    from: f2,
                    commit: c2,
                    removed: r2,
                    added: a2,
                },
            ) if commit == f2 => {
                let mut net: rustc_hash::FxHashMap<&Vec<Option<Term>>, i64> = Default::default();
                let mut order: Vec<&Vec<Option<Term>>> = Vec::new();
                for (rows, sign) in [(removed, -1i64), (added, 1), (r2, -1), (a2, 1)] {
                    for r in rows.iter() {
                        let n = net.entry(&r.terms).or_insert_with(|| {
                            order.push(&r.terms);
                            0
                        });
                        *n += sign * r.count as i64;
                    }
                }
                let (mut rem, mut add) = (Vec::new(), Vec::new());
                for terms in order {
                    let n = net[terms];
                    let row = LiveRow {
                        terms: terms.clone(),
                        count: n.unsigned_abs(),
                    };
                    match n.cmp(&0) {
                        std::cmp::Ordering::Less => rem.push(row),
                        std::cmp::Ordering::Greater => add.push(row),
                        std::cmp::Ordering::Equal => {}
                    }
                }
                Some(LiveEvent::Diff {
                    commit: *c2,
                    from: *from,
                    removed: rem.into(),
                    added: add.into(),
                })
            }
            (
                LiveEvent::Value { from, commit, .. },
                LiveEvent::Value {
                    from: f2,
                    commit: c2,
                    value,
                },
            ) if commit == f2 => Some(LiveEvent::Value {
                commit: *c2,
                from: *from,
                value: *value,
            }),
            (
                LiveEvent::Snapshot {
                    vars, value: None, ..
                },
                LiveEvent::Diff { .. },
            ) => {
                let LiveEvent::Snapshot {
                    commit,
                    rows,
                    reset,
                    ..
                } = self
                else {
                    unreachable!()
                };
                let as_diff = LiveEvent::Diff {
                    commit: *commit,
                    from: *commit,
                    removed: Arc::from(Vec::new()),
                    added: rows.clone(),
                };
                match as_diff.merge(next)? {
                    LiveEvent::Diff {
                        commit,
                        added,
                        removed,
                        ..
                    } if removed.is_empty() => Some(LiveEvent::Snapshot {
                        commit,
                        vars: vars.clone(),
                        rows: added,
                        value: None,
                        reset: *reset,
                    }),
                    _ => None,
                }
            }
            (
                LiveEvent::Snapshot {
                    vars,
                    value: Some(_),
                    reset,
                    ..
                },
                LiveEvent::Value { commit, value, .. },
            ) => Some(LiveEvent::Snapshot {
                commit: *commit,
                vars: vars.clone(),
                rows: Arc::from(Vec::new()),
                value: Some(*value),
                reset: *reset,
            }),
            _ => None,
        }
    }
}

/// How a live query starts.
#[derive(Clone, Debug, Default)]
pub struct LiveOptions {
    /// resume after this commit (`Last-Event-ID`): the first event is the change from
    /// it to the circuit's commit when the runtime can compute it, and a `reset`
    /// otherwise
    pub after: Option<u64>,
    /// the budget of the circuit's state (the runtime's default when `None`)
    pub max_memory_bytes: Option<u64>,
    /// the principal the per-principal limit counts against
    pub principal: Option<String>,
}

/// The most rows a subscription's queue holds before its events are replaced by a
/// `reset` taken when the subscriber reads again.
const QUEUE_ROWS: usize = if cfg!(test) { 1_000 } else { 1_000_000 };

#[derive(Default)]
struct Queue {
    events: VecDeque<LiveEvent>,
    rows: usize,
    /// the queue overflowed: the next read takes a fresh `reset`
    overflow: bool,
    closed: bool,
}

/// What a pass and a subscriber share.
#[derive(Default)]
pub(crate) struct Shared {
    queue: Mutex<Queue>,
    cv: Condvar,
    notify: tokio::sync::Notify,
}

impl Shared {
    pub(crate) fn push(&self, e: LiveEvent) {
        let mut q = self.queue.lock();
        if q.closed {
            return;
        }
        let end = matches!(e, LiveEvent::Error { .. });
        if !q.overflow || end {
            q.rows += e.rows();
            q.events.push_back(e);
            if q.rows > QUEUE_ROWS && !end {
                q.events.clear();
                q.rows = 0;
                q.overflow = true;
            }
        }
        if end {
            q.closed = true;
        }
        drop(q);
        self.cv.notify_all();
        self.notify.notify_one();
    }

    pub(crate) fn close(&self) {
        self.queue.lock().closed = true;
        self.wake_all();
    }

    /// Wake every waiting reader, blocking or asynchronous, and the next one to wait.
    fn wake_all(&self) {
        self.cv.notify_all();
        self.notify.notify_waiters();
        self.notify.notify_one();
    }
}

/// The live queries of each principal.
pub(crate) type PerPrincipal = Arc<Mutex<rustc_hash::FxHashMap<String, usize>>>;

/// A subscriber's count against the limits, released when the subscription drops.
pub(crate) struct Count {
    pub(crate) total: Arc<AtomicUsize>,
    pub(crate) principal: Option<(PerPrincipal, String)>,
}

impl Drop for Count {
    fn drop(&mut self) {
        self.total.fetch_sub(1, Ordering::AcqRel);
        if let Some((m, p)) = &self.principal {
            let mut m = m.lock();
            if let Some(n) = m.get_mut(p) {
                *n -= 1;
                if *n == 0 {
                    m.remove(p);
                }
            }
        }
    }
}

/// A live query. Dropping it ends the subscription.
pub struct LiveQuery {
    pub(crate) shared: Arc<Shared>,
    pub(crate) slot: Arc<super::runtime::Cell>,
    pub(crate) kind: Kind,
    pub(crate) vars: Arc<[String]>,
    pub(crate) _count: Count,
}

impl std::fmt::Debug for LiveQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveQuery")
            .field("kind", &self.kind)
            .field("vars", &self.vars)
            .finish()
    }
}

impl LiveQuery {
    /// SELECT, ASK or CONSTRUCT.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The names of the result's columns (`subject`, `predicate` and `object` for
    /// CONSTRUCT, none for ASK).
    pub fn vars(&self) -> &[String] {
        &self.vars
    }

    fn take(&self, q: &mut MutexGuard<'_, Queue>) -> Option<LiveEvent> {
        if q.overflow {
            // passes hold the circuit's lock while they queue events, so the queue's
            // lock is let go and taken again after the circuit's
            let e = MutexGuard::unlocked(q, || {
                let mut slot = self.slot.state.lock();
                let mut q = self.shared.queue.lock();
                if !q.overflow {
                    return None;
                }
                q.overflow = false;
                q.rows = 0;
                // the reset covers the data events, but not an error that ends the stream
                q.events.retain(|e| matches!(e, LiveEvent::Error { .. }));
                Some(slot.resync(&self.shared))
            });
            if e.is_some() {
                return e;
            }
        }
        let e = q.events.pop_front()?;
        q.rows = q.rows.saturating_sub(e.rows());
        Some(e)
    }

    /// The next event, waiting until there is one. `None` once the subscription has
    /// ended (after an `error` event, or when the runtime closed it).
    pub fn next(&self) -> Option<LiveEvent> {
        self.next_until(None)
    }

    /// [`next`](Self::next), waiting at most `timeout`: `None` when it passes or the
    /// subscription has ended ([`is_closed`](Self::is_closed) tells which).
    pub fn next_timeout(&self, timeout: Duration) -> Option<LiveEvent> {
        self.next_until(Some(Instant::now() + timeout))
    }

    fn next_until(&self, until: Option<Instant>) -> Option<LiveEvent> {
        let mut q = self.shared.queue.lock();
        loop {
            if let Some(e) = self.take(&mut q) {
                return Some(e);
            }
            if q.closed {
                return None;
            }
            match until {
                None => self.shared.cv.wait(&mut q),
                Some(t) => {
                    if self.shared.cv.wait_until(&mut q, t).timed_out() {
                        return self.take(&mut q);
                    }
                }
            }
        }
    }

    /// The next event if one is waiting.
    pub fn try_next(&self) -> Option<LiveEvent> {
        let mut q = self.shared.queue.lock();
        self.take(&mut q)
    }

    /// The next event, awaiting it. `None` once the subscription has ended.
    pub async fn recv(&self) -> Option<LiveEvent> {
        loop {
            // registered before the queue is read, so that a wake in between is kept
            let mut notified = std::pin::pin!(self.shared.notify.notified());
            notified.as_mut().enable();
            {
                let mut q = self.shared.queue.lock();
                if let Some(e) = self.take(&mut q) {
                    return Some(e);
                }
                if q.closed {
                    return None;
                }
            }
            notified.await;
        }
    }

    /// End the subscription and wake every thread waiting for its next event.
    /// Events still queued are dropped.
    pub fn close(&self) {
        let mut q = self.shared.queue.lock();
        q.closed = true;
        q.events.clear();
        q.rows = 0;
        q.overflow = false;
        drop(q);
        self.shared.wake_all();
    }

    /// Whether the subscription has ended and its queue is empty.
    pub fn is_closed(&self) -> bool {
        let q = self.shared.queue.lock();
        q.closed && q.events.is_empty() && !q.overflow
    }
}

impl Drop for LiveQuery {
    fn drop(&mut self) {
        self.shared.close();
    }
}
