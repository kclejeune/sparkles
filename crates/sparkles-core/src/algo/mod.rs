//! Whole-graph algorithms on a projection of the dataset (spec F14).
//!
//! The library works on dense `u32` node indexes and knows nothing of SPARQL. A
//! [`Graph`] holds compressed sparse rows built from the edges of a dataset (see
//! [`crate::sparql::algosvc`] for the projection), and each algorithm module takes a
//! graph, its parameters, a [`Par`] for parallel loops and a [`Control`] that carries the
//! cancel flag, the deadline and the memory budget.
//!
//! Every algorithm gives the same result on any number of threads, except where its
//! module says otherwise, because its parallel updates are synchronous or do not depend
//! on their order, and its floating-point sums add fixed-size blocks in index order.

// the reference implementations in the tests index several arrays by one counter
#![cfg_attr(test, allow(clippy::needless_range_loop))]

pub mod betweenness;
pub mod cache;
pub mod catalog;
pub mod community;
pub mod components;
pub mod fastrp;
pub mod graph;
pub mod kcore;
pub mod labelprop;
pub(crate) mod memory;
pub mod node2vec;
pub mod pagerank;
pub mod pool;
pub mod similarity;
pub mod traverse;
pub mod triangles;

pub use graph::{Aggregate, Csr, Graph, RawEdges};
pub use pool::Par;

use crate::error::{Budget, BudgetKind, Error, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// The cancel flag, the deadline and the memory budget of one algorithm call. Cheap to
/// clone, and shared by the worker threads of the call.
#[derive(Clone)]
pub struct Control {
    inner: Arc<Inner>,
}

struct Inner {
    cancel: Arc<AtomicBool>,
    cancellation: Option<crate::task::Cancellation>,
    deadline: Option<Instant>,
    limit: u64,
    used: AtomicU64,
    peak: AtomicU64,
    query: Option<Arc<ServiceMemory>>,
}

pub(crate) struct ServiceMemory {
    pub(crate) ledger: Arc<crate::sparql::query_memory::QueryMemoryLedger>,
    pub(crate) owner: Arc<crate::sparql::query_memory::MemoryOwner>,
    pub(crate) query_limit: u64,
    pub(crate) limit: u64,
    pub(crate) used: AtomicU64,
    pub(crate) peak: AtomicU64,
    pub(crate) frames: std::sync::atomic::AtomicUsize,
    pub(crate) closed: AtomicBool,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(query) = &self.query {
            // Plain Vec outputs and immutable projection data do not retain Control.
            // The call's residual ownership ends only after its other owners drop.
            query.ledger.release_service(
                query,
                self.used.load(Ordering::Relaxed),
                Some(&self.used),
            );
            query.ledger.end_service_frame(query);
        }
    }
}

/// A test-only observation of a native control check at its actual call site.
#[cfg(feature = "failpoints")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    Before,
    Passed,
    Cancelled,
    Timeout,
}

#[cfg(feature = "failpoints")]
pub type CheckObserver =
    Arc<dyn Fn(&'static str, u32, CheckOutcome, Option<Instant>) + Send + Sync>;

#[cfg(feature = "failpoints")]
thread_local! {
    static CHECK_OBSERVER: std::cell::RefCell<Option<CheckObserver>> = const { std::cell::RefCell::new(None) };
}

/// Install a native check observer on this thread only, for bounded control tests.
#[cfg(feature = "failpoints")]
pub fn set_check_observer(observer: Option<CheckObserver>) {
    CHECK_OBSERVER.with(|slot| *slot.borrow_mut() = observer);
}

#[cfg(feature = "failpoints")]
fn observe_check(
    at: &'static std::panic::Location<'static>,
    outcome: CheckOutcome,
    deadline: Option<Instant>,
) {
    let observer = CHECK_OBSERVER.with(|slot| slot.borrow().clone());
    if let Some(observer) = observer {
        observer(at.file(), at.line(), outcome, deadline);
    }
}

/// Bounded actual projection allocation/scan observations for regression tests only.
#[cfg(any(test, feature = "failpoints"))]
#[derive(Clone, Copy, Debug)]
pub enum ProjectionEvent {
    Allocation(&'static str, usize, u64, u64),
    Materialized(&'static str, usize),
}
#[cfg(any(test, feature = "failpoints"))]
type ProjectionObserver = Arc<dyn Fn(ProjectionEvent) + Send + Sync>;
#[cfg(any(test, feature = "failpoints"))]
thread_local! {
    static PROJECTION_OBSERVER: std::cell::RefCell<Option<ProjectionObserver>> = const { std::cell::RefCell::new(None) };
}
#[cfg(any(test, feature = "failpoints"))]
pub fn set_projection_observer(observer: Option<ProjectionObserver>) {
    PROJECTION_OBSERVER.with(|s| *s.borrow_mut() = observer);
}
#[cfg(any(test, feature = "failpoints"))]
pub(crate) fn observe_projection(event: ProjectionEvent) {
    let f = PROJECTION_OBSERVER.with(|s| s.borrow().clone());
    if let Some(f) = f {
        f(event);
    }
}

impl Control {
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    #[cfg(any(test, feature = "failpoints"))]
    pub(crate) fn observed_used(&self) -> u64 {
        self.inner.query.as_ref().map_or_else(
            || self.inner.used.load(Ordering::Relaxed),
            |q| q.used.load(Ordering::Relaxed),
        )
    }

    /// A call that stops when `cancel` is set or the deadline passes, and holds at most
    /// `mem_limit` bytes of estimated memory (`u64::MAX`: no limit).
    pub fn new(cancel: Arc<AtomicBool>, deadline: Option<Instant>, mem_limit: u64) -> Control {
        Self::with_cancellation(cancel, None, deadline, mem_limit)
    }

    pub(crate) fn with_cancellation(
        cancel: Arc<AtomicBool>,
        cancellation: Option<crate::task::Cancellation>,
        deadline: Option<Instant>,
        mem_limit: u64,
    ) -> Control {
        Control {
            inner: Arc::new(Inner {
                cancel,
                cancellation,
                deadline,
                limit: mem_limit,
                used: AtomicU64::new(0),
                peak: AtomicU64::new(0),
                query: None,
            }),
        }
    }

    pub(crate) fn for_query(ctx: &crate::sparql::Ctx, limit: u64) -> Result<Self> {
        let ledger = ctx.memory().clone();
        let owner = ledger.owner(ctx.grant())?;
        let mut control = Self::with_cancellation(
            ctx.cancel.clone(),
            ctx.cancellation.clone(),
            ctx.deadline,
            limit,
        );
        ledger.start_call();
        Arc::get_mut(&mut control.inner).expect("new call").query = Some(Arc::new(ServiceMemory {
            ledger,
            owner,
            query_limit: ctx.mem_limit,
            limit,
            used: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            frames: std::sync::atomic::AtomicUsize::new(1),
            closed: AtomicBool::new(false),
        }));
        Ok(control)
    }

    /// A lexical graph/workspace frame. SERVICE limits include all prior output
    /// leases, while this frame releases only its residual native storage on drop.
    pub(crate) fn query_frame(&self) -> Result<Self> {
        self.check()?;
        let query = self
            .inner
            .query
            .as_ref()
            .ok_or_else(|| Error::invalid("query frame needs a SERVICE owner"))?;
        query.ledger.start_service_frame(query)?;
        Ok(Self {
            inner: Arc::new(Inner {
                cancel: self.inner.cancel.clone(),
                cancellation: self.inner.cancellation.clone(),
                deadline: self.inner.deadline,
                limit: query.limit,
                used: AtomicU64::new(0),
                peak: AtomicU64::new(0),
                query: Some(query.clone()),
            }),
        })
    }

    pub(crate) fn output_lease(
        &self,
        bytes: u64,
    ) -> Result<Option<crate::sparql::query_memory::QueryMemoryLease>> {
        self.inner
            .query
            .as_ref()
            .map(|query| query.ledger.transfer_native(query, bytes, &self.inner.used))
            .transpose()
    }

    /// A call without a deadline, cancel flag or memory limit.
    pub fn unlimited() -> Control {
        Control::new(Arc::new(AtomicBool::new(false)), None, u64::MAX)
    }

    /// Fail when the call is cancelled or past its deadline.
    #[inline]
    #[cfg_attr(feature = "failpoints", track_caller)]
    pub fn check(&self) -> Result<()> {
        #[cfg(feature = "failpoints")]
        let at = std::panic::Location::caller();
        #[cfg(feature = "failpoints")]
        observe_check(at, CheckOutcome::Before, self.inner.deadline);
        if self.inner.cancel.load(Ordering::Relaxed)
            || self
                .inner
                .cancellation
                .as_ref()
                .is_some_and(|c| c.check().is_err())
        {
            #[cfg(feature = "failpoints")]
            observe_check(at, CheckOutcome::Cancelled, self.inner.deadline);
            return Err(Error::Cancelled);
        }
        if let Some(d) = self.inner.deadline
            && Instant::now() >= d
        {
            #[cfg(feature = "failpoints")]
            observe_check(at, CheckOutcome::Timeout, self.inner.deadline);
            return Err(Error::Timeout);
        }
        #[cfg(feature = "failpoints")]
        observe_check(at, CheckOutcome::Passed, self.inner.deadline);
        Ok(())
    }

    /// Hold `bytes` more estimated memory. Fails, holding nothing more, when that would
    /// pass the limit. Call it before the allocation it accounts for.
    pub fn charge(&self, bytes: u64) -> Result<()> {
        let i = &self.inner;
        if let Some(query) = &i.query {
            return query.ledger.reserve_service(query, bytes, Some(&i.used));
        }
        let prior = i
            .used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |u| {
                u.checked_add(bytes).filter(|n| *n <= i.limit)
            })
            .map_err(|u| {
                Error::BudgetExceeded(Budget {
                    kind: BudgetKind::Memory,
                    limit: i.limit,
                    requested: u.saturating_add(bytes),
                })
            })?;
        i.peak.fetch_max(prior + bytes, Ordering::Relaxed);
        Ok(())
    }

    /// Fail when `bytes` more would pass the limit, without holding them.
    pub fn require(&self, bytes: u64) -> Result<()> {
        let i = &self.inner;
        if let Some(query) = &i.query {
            return query.ledger.require_service(query, bytes);
        }
        let used = i.used.load(Ordering::Relaxed);
        if used.saturating_add(bytes) > i.limit {
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                limit: i.limit,
                requested: used.saturating_add(bytes),
            }));
        }
        Ok(())
    }

    /// Give back `bytes` of the memory charged.
    pub fn release(&self, bytes: u64) {
        if let Some(query) = &self.inner.query {
            query
                .ledger
                .release_service(query, bytes, Some(&self.inner.used));
            return;
        }
        let _ = self
            .inner
            .used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |u| {
                Some(u.saturating_sub(bytes))
            });
    }

    /// The most estimated memory the call held at once.
    pub fn peak(&self) -> u64 {
        self.inner.query.as_ref().map_or_else(
            || self.inner.peak.load(Ordering::Relaxed),
            |q| q.peak.load(Ordering::Relaxed),
        )
    }

    /// The memory the call may still charge.
    pub fn remaining(&self) -> u64 {
        self.inner.query.as_ref().map_or_else(
            || {
                self.inner
                    .limit
                    .saturating_sub(self.inner.used.load(Ordering::Relaxed))
            },
            |q| q.limit.saturating_sub(q.used.load(Ordering::Relaxed)),
        )
    }
}

/// A small seeded generator (splitmix64), so that a seed gives the same numbers in every
/// build of the library.
#[derive(Clone, Debug)]
pub(crate) struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    /// A generator for one item (a node, a walk) of a run, whatever order items run in.
    pub fn for_item(seed: u64, item: u64) -> Rng {
        let mut r = Rng::new(seed.wrapping_add(item.wrapping_mul(0xD1B5_4A32_D192_ED03)));
        r.next_u64();
        r
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `[0, 1)`.
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A number in `0..n` (`n` above zero).
    #[inline]
    pub fn below(&mut self, n: usize) -> usize {
        ((self.next_u64() as u128 * n as u128) >> 64) as usize
    }
}

/// An error for a parameter or an input of an algorithm.
pub(crate) fn bad(algo: &str, m: impl std::fmt::Display) -> Error {
    Error::invalid(format!("algo:{algo}: {m}"))
}

/// The statistics of a run, as the entries of the summary literal.
pub type Stats = serde_json::Map<String, serde_json::Value>;

/// How often a loop over nodes checks the cancel flag and the deadline.
pub(crate) const CHECK_EVERY: usize = 1 << 16;

/// The work of one block of a parallel loop over `n` nodes.
pub(crate) const BLOCK: usize = 4096;

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    /// A graph of `n` nodes with directed edges.
    pub fn digraph(n: u32, edges: &[(u32, u32)]) -> Graph {
        let raw = RawEdges {
            nodes: (0..n as u64).collect(),
            src: edges.iter().map(|e| e.0 as u64).collect(),
            dst: edges.iter().map(|e| e.1 as u64).collect(),
            w: None,
            symmetric: false,
        };
        Graph::build(raw, Aggregate::None, &Par::single(), &Control::unlimited()).unwrap()
    }

    /// A graph of `n` nodes that reads each edge both ways.
    pub fn ugraph(n: u32, edges: &[(u32, u32)]) -> Graph {
        let mut src = Vec::new();
        let mut dst = Vec::new();
        for &(a, b) in edges {
            src.push(a as u64);
            dst.push(b as u64);
            if a != b {
                src.push(b as u64);
                dst.push(a as u64);
            }
        }
        let raw = RawEdges {
            nodes: (0..n as u64).collect(),
            src,
            dst,
            w: None,
            symmetric: true,
        };
        Graph::build(raw, Aggregate::None, &Par::single(), &Control::unlimited()).unwrap()
    }

    /// Zachary's karate club (78 edges).
    pub fn karate() -> Vec<(u32, u32)> {
        let rows: [(u32, &[u32]); 26] = [
            (0, &[1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 17, 19, 21, 31]),
            (1, &[2, 3, 7, 13, 17, 19, 21, 30]),
            (2, &[3, 7, 8, 9, 13, 27, 28, 32]),
            (3, &[7, 12, 13]),
            (4, &[6, 10]),
            (5, &[6, 10, 16]),
            (6, &[16]),
            (8, &[30, 32, 33]),
            (9, &[33]),
            (13, &[33]),
            (14, &[32, 33]),
            (15, &[32, 33]),
            (18, &[32, 33]),
            (19, &[33]),
            (20, &[32, 33]),
            (22, &[32, 33]),
            (23, &[25, 27, 29, 32, 33]),
            (24, &[25, 27, 31]),
            (25, &[31]),
            (26, &[29, 33]),
            (27, &[33]),
            (28, &[31, 33]),
            (29, &[32, 33]),
            (30, &[32, 33]),
            (31, &[32, 33]),
            (32, &[33]),
        ];
        let mut e = Vec::new();
        for (a, bs) in rows {
            for &b in bs {
                e.push((a, b));
            }
        }
        e
    }
}
