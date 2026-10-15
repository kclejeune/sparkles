//! node2vec embeddings (spec F14 §4; Grover and Leskovec, KDD 2016).
//!
//! Biased second-order random walks feed a skip-gram model with negative sampling. A walk
//! at node `v` that came from `t` picks a neighbour `x` with probability proportional to
//! the edge weight times `1/p` when `x` is `t`, 1 when `x` is a neighbour of `t`, and
//! `1/q` otherwise. Training is sequential over the walks in a fixed order, so a seed gives
//! one result. The `concurrency` parameter does not apply to it.

use super::{CHECK_EVERY, Control, Graph, Rng, bad};
use crate::error::Result;

#[cfg(test)]
mod probe {
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone, Copy, Debug)]
    pub(super) enum Point {
        Work(&'static str),
        Allocation(&'static str, usize),
        Reserved(&'static str, usize),
    }
    type Callback = Rc<dyn Fn(Point)>;
    thread_local! {
        static HOOK: RefCell<Option<Callback>> = const { RefCell::new(None) };
        static FAIL: RefCell<Option<&'static str>> = const { RefCell::new(None) };
    }
    pub(super) struct Failure(Option<&'static str>);
    pub(super) fn fail(name: &'static str) -> Failure {
        Failure(FAIL.with(|f| f.replace(Some(name))))
    }
    impl Drop for Failure {
        fn drop(&mut self) {
            FAIL.with(|f| *f.borrow_mut() = self.0.take());
        }
    }
    pub(super) fn allocation_fails(name: &'static str) -> bool {
        FAIL.with(|f| *f.borrow() == Some(name))
    }
    pub(super) struct Scope(Option<Callback>);
    pub(super) fn install(f: impl Fn(Point) + 'static) -> Scope {
        Scope(HOOK.with(|h| h.replace(Some(Rc::new(f)))))
    }
    impl Drop for Scope {
        fn drop(&mut self) {
            HOOK.with(|h| *h.borrow_mut() = self.0.take());
        }
    }
    pub(super) fn at(point: Point) {
        let hook = HOOK.with(|h| h.borrow().clone());
        if let Some(hook) = hook {
            hook(point);
        }
    }
}

#[derive(Clone, Debug)]
pub struct Params {
    pub dimension: usize,
    pub walk_length: usize,
    pub walks_per_node: usize,
    pub window: usize,
    /// p: the weight of returning to the previous node is `1 / return_factor`
    pub return_factor: f64,
    /// q: the weight of moving away is `1 / in_out_factor`
    pub in_out_factor: f64,
    pub negative_samples: usize,
    pub epochs: usize,
    pub learning_rate: f64,
    pub seed: u64,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            dimension: 128,
            walk_length: 80,
            walks_per_node: 10,
            window: 10,
            return_factor: 1.0,
            in_out_factor: 1.0,
            negative_samples: 5,
            epochs: 1,
            learning_rate: 0.025,
            seed: 0,
        }
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x.clamp(-30.0, 30.0)).exp())
}

// One shared countdown bounds work across all loops, including initialization and
// branches that skip training. Binary searches are bounded by usize::BITS comparisons
// and count as one operation. Checks do not draw random numbers or reorder arithmetic.
struct Work<'a> {
    ctl: &'a Control,
    left: usize,
}

impl<'a> Work<'a> {
    fn new(ctl: &'a Control) -> Result<Self> {
        ctl.check()?;
        Ok(Self {
            ctl,
            left: CHECK_EVERY,
        })
    }

    #[inline]
    fn tick(&mut self, _phase: &'static str) -> Result<()> {
        #[cfg(test)]
        probe::at(probe::Point::Work(_phase));
        self.left -= 1;
        if self.left == 0 {
            self.ctl.check()?;
            self.left = CHECK_EVERY;
        }
        Ok(())
    }
}

fn bytes<T>(capacity: usize) -> Result<u64> {
    capacity
        .checked_mul(std::mem::size_of::<T>())
        .filter(|&n| n <= isize::MAX as usize)
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| bad("node2vec", "buffer capacity overflows"))
}

struct Charge<'a> {
    ctl: &'a Control,
    bytes: u64,
}

impl Drop for Charge<'_> {
    fn drop(&mut self) {
        self.ctl.release(self.bytes);
    }
}

// These buffers start empty and never grow after try_reserve_exact. With the pinned
// Rust Global allocator, reserve_exact retains precisely the requested capacity. The
// charge precedes allocation; this accounts for Vec capacity, not allocator overhead.
// Field order frees the Vec before releasing its reservation on every exit.
struct Buffer<'a, T> {
    values: Vec<T>,
    charge: Charge<'a>,
}

impl<'a, T> Buffer<'a, T> {
    fn new(_name: &'static str, capacity: usize, ctl: &'a Control) -> Result<Self> {
        ctl.check()?;
        let bytes = bytes::<T>(capacity)?;
        ctl.charge(bytes)?;
        let charge = Charge { ctl, bytes };
        #[cfg(test)]
        probe::at(probe::Point::Allocation(_name, capacity));
        // Test-only allocation-error adapter; it never attempts a large allocation.
        #[cfg(test)]
        if probe::allocation_fails(_name) {
            return Err(bad("node2vec", "injected allocation failure"));
        }
        let mut values = Vec::new();
        values
            .try_reserve_exact(capacity)
            .map_err(|e| bad("node2vec", format!("buffer allocation failed: {e}")))?;
        if values.capacity() != capacity {
            return Err(bad(
                "node2vec",
                "allocator returned an unsupported capacity",
            ));
        }
        #[cfg(test)]
        probe::at(probe::Point::Reserved(_name, values.capacity()));
        Ok(Self { values, charge })
    }

    fn push(&mut self, value: T) -> Result<()> {
        if self.values.len() == self.values.capacity() {
            return Err(bad("node2vec", "fixed buffer capacity exceeded"));
        }
        self.values.push(value);
        Ok(())
    }

    fn clear(&mut self) {
        self.values.clear();
    }

    fn filled(mut self, value: T, work: &mut Work<'_>, phase: &'static str) -> Result<Self>
    where
        T: Copy,
    {
        while self.values.len() < self.values.capacity() {
            work.tick(phase)?;
            self.push(value)?;
        }
        Ok(self)
    }

    fn into_output(mut self) -> Vec<T> {
        let values = std::mem::take(&mut self.values);
        // The call's Control keeps the returned output charged, as for other native
        // result Vecs. Only temporary owners release their reservations here.
        self.charge.bytes = 0;
        values
    }
}

impl<T> std::ops::Deref for Buffer<'_, T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.values
    }
}
impl<T> std::ops::DerefMut for Buffer<'_, T> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.values
    }
}

/// One step of a walk from `cur`, which it reached from `prev` (`None` at the start).
fn step(
    g: &Graph,
    p: &Params,
    prev: Option<u32>,
    cur: u32,
    rng: &mut Rng,
    buf: &mut Buffer<'_, f64>,
    work: &mut Work<'_>,
) -> Result<Option<u32>> {
    let row = g.out.row(cur);
    if row.is_empty() {
        return Ok(None);
    }
    let w = g.out.wrow(cur);
    buf.clear();
    let mut total = 0.0;
    for (i, &x) in row.iter().enumerate() {
        work.tick("neighbor")?;
        let mut a = w.map_or(1.0, |w| w[i] as f64);
        if let Some(t) = prev {
            a *= if x == t {
                1.0 / p.return_factor
            } else if g.out.has(t, x) {
                1.0
            } else {
                1.0 / p.in_out_factor
            };
        }
        total += a;
        buf.push(total)?;
    }
    if total <= 0.0 {
        return Ok(None);
    }
    let r = rng.next_f64() * total;
    let i = buf.partition_point(|&c| c <= r).min(row.len() - 1);
    Ok(Some(row[i]))
}

/// The embeddings, `dimension` values for each node, in node order.
pub fn run(g: &Graph, p: &Params, ctl: &Control) -> Result<Vec<f32>> {
    let d = p.dimension;
    if d == 0 {
        return Err(bad("node2vec", "dimension must be above 0"));
    }
    if p.return_factor <= 0.0 || p.in_out_factor <= 0.0 {
        return Err(bad(
            "node2vec",
            "returnFactor and inOutFactor must be above 0",
        ));
    }
    if p.walk_length < 2 {
        return Err(bad("node2vec", "walkLength must be at least 2"));
    }
    let mut work = Work::new(ctl)?;
    let n = g.n();
    if n == 0 {
        return Ok(Vec::new());
    }
    let nd = n
        .checked_mul(d)
        .ok_or_else(|| bad("node2vec", "dimension overflows"))?;
    let steps = p
        .epochs
        .checked_mul(p.walks_per_node)
        .and_then(|v| v.checked_mul(n))
        .filter(|&v| u64::try_from(v).is_ok())
        .ok_or_else(|| bad("node2vec", "walk count overflows"))?;
    let samples = p
        .negative_samples
        .checked_add(1)
        .ok_or_else(|| bad("node2vec", "negativeSamples overflows"))?;
    let mut degree = 0;
    for u in 0..n as u32 {
        work.tick("degree")?;
        degree = degree.max(g.out.deg(u));
    }
    // Check every capacity and their sum before allocating any of the buffers.
    let capacities = [
        bytes::<f32>(nd)?,
        bytes::<f32>(nd)?,
        bytes::<f64>(n)?,
        bytes::<u32>(n)?,
        bytes::<u32>(p.walk_length)?,
        bytes::<f64>(degree)?,
        bytes::<f32>(d)?,
    ];
    capacities
        .into_iter()
        .try_fold(0u64, |sum, v| sum.checked_add(v))
        .ok_or_else(|| bad("node2vec", "working memory overflows"))?;
    let mut syn0 = Buffer::new("syn0", nd, ctl)?;
    for u in 0..n {
        let mut rng = Rng::for_item(p.seed, u as u64);
        for _ in 0..d {
            work.tick("initialize")?;
            syn0.push(((rng.next_f64() - 0.5) / d as f64) as f32)?;
        }
    }
    let mut syn1 = Buffer::new("syn1", nd, ctl)?.filled(0f32, &mut work, "initialize-output")?;
    // Incoming CSR ownership is the separate graph/transpose accounting contract.
    // Acquire it once; do not duplicate that reservation among these own buffers.
    let inn = g.inn();
    let mut cum = Buffer::new("noise", n, ctl)?;
    let mut acc = 0.0;
    for u in 0..n as u32 {
        work.tick("noise")?;
        acc += ((g.out.deg(u) + if g.symmetric { 0 } else { inn.deg(u) }) as f64 + 1.0).powf(0.75);
        cum.push(acc)?;
    }
    let total_steps = steps.max(1);
    let mut done = 0usize;
    let mut walk = Buffer::new("walk", p.walk_length, ctl)?;
    let mut buf = Buffer::new("sampling", degree, ctl)?;
    let mut neu1e =
        Buffer::new("gradient", d, ctl)?.filled(0f32, &mut work, "initialize-gradient")?;
    let mut order = Buffer::new("order", n, ctl)?;
    for u in 0..n as u32 {
        work.tick("order")?;
        order.push(u)?;
    }
    let mut shuffle = Rng::new(p.seed ^ 0xABCD);
    for epoch in 0..p.epochs {
        work.tick("epoch")?;
        for rep in 0..p.walks_per_node {
            work.tick("pass")?;
            for i in (1..n).rev() {
                work.tick("shuffle")?;
                order.swap(i, shuffle.below(i + 1));
            }
            for &start in order.iter() {
                work.tick("start")?;
                let item = ((epoch * p.walks_per_node + rep) * n) as u64 + start as u64;
                let mut rng = Rng::for_item(p.seed ^ 0x5555, item);
                walk.clear();
                walk.push(start)?;
                let mut prev = None;
                let mut cur = start;
                while walk.len() < p.walk_length {
                    work.tick("walk")?;
                    match step(g, p, prev, cur, &mut rng, &mut buf, &mut work)? {
                        Some(next) => {
                            walk.push(next)?;
                            prev = Some(cur);
                            cur = next;
                        }
                        None => break,
                    }
                }
                if walk.len() < 2 {
                    done += 1;
                    continue;
                }
                let alpha = (p.learning_rate * (1.0 - done as f64 / total_steps as f64))
                    .max(p.learning_rate * 1e-4) as f32;
                done += 1;
                for i in 0..walk.len() {
                    work.tick("center")?;
                    let center = walk[i] as usize;
                    let b = rng.below(p.window.max(1));
                    let reach = p.window.max(1) - b;
                    let lo = i.saturating_sub(reach);
                    let hi = i.saturating_add(reach).saturating_add(1).min(walk.len());
                    for (j, &ctx_node) in walk.iter().enumerate().take(hi).skip(lo) {
                        work.tick("context")?;
                        if j == i {
                            continue;
                        }
                        let l1 = center * d;
                        for value in neu1e.iter_mut() {
                            work.tick("clear-gradient")?;
                            *value = 0.0;
                        }
                        for s in 0..samples {
                            work.tick("negative")?;
                            let (target, label) = if s == 0 {
                                (ctx_node as usize, 1.0f32)
                            } else {
                                let r = rng.next_f64() * acc;
                                let t = cum.partition_point(|&c| c <= r).min(n - 1);
                                if t == ctx_node as usize {
                                    continue;
                                }
                                (t, 0.0)
                            };
                            let l2 = target * d;
                            let mut dot = 0f32;
                            for c in 0..d {
                                work.tick("dot")?;
                                dot += syn0[l1 + c] * syn1[l2 + c];
                            }
                            let gr = (label - sigmoid(dot)) * alpha;
                            for c in 0..d {
                                work.tick("update-output")?;
                                neu1e[c] += gr * syn1[l2 + c];
                                syn1[l2 + c] += gr * syn0[l1 + c];
                            }
                        }
                        for c in 0..d {
                            work.tick("update-input")?;
                            syn0[l1 + c] += neu1e[c];
                        }
                    }
                }
            }
        }
    }
    ctl.check()?;
    Ok(syn0.into_output())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    #[test]
    fn r3_r4_seeded_bits_before_and_after() {
        let g = ugraph(2, &[(0, 1)]);
        let p = Params {
            dimension: 2,
            walk_length: 6,
            walks_per_node: 2,
            window: 2,
            negative_samples: 1,
            epochs: 1,
            seed: 17,
            ..Params::default()
        };
        let values = run(&g, &p, &Control::unlimited()).unwrap();
        let bits: Vec<_> = values.iter().map(|v| v.to_bits()).collect();
        assert_eq!(bits, [1048538886, 3183028222, 3194169835, 1009158302]);
    }

    #[test]
    fn r3_r4_cancel_inside_long_walk() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        use std::{cell::Cell, rc::Rc};
        let g = ugraph(2, &[(0, 1)]);
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        let count = Rc::new(Cell::new(0usize));
        let observed = count.clone();
        let _hook = probe::install(move |point| {
            if let probe::Point::Work("walk") = point {
                observed.set(observed.get() + 1);
                f.store(true, Ordering::Relaxed);
            }
        });
        let p = Params {
            dimension: 1,
            walk_length: 70_000,
            walks_per_node: 1,
            window: 1,
            negative_samples: 0,
            epochs: 1,
            ..Params::default()
        };
        let result = run(&g, &p, &Control::new(flag, None, u64::MAX));
        println!(
            "F14_NODE2VEC_CANCEL walk_operations={} result={:?}",
            count.get(),
            result.as_ref().map(|v| v.len())
        );
        assert!(matches!(result, Err(crate::Error::Cancelled)), "{result:?}");
        assert!(count.get() > 0 && count.get() <= CHECK_EVERY);
    }

    #[test]
    fn r3_r4_walk_budget_before_allocation() {
        use std::{
            cell::Cell,
            rc::Rc,
            sync::{Arc, atomic::AtomicBool},
        };
        let g = ugraph(2, &[(0, 1)]);
        let allocated = Rc::new(Cell::new(false));
        let observed = allocated.clone();
        let _hook = probe::install(move |point| {
            if let probe::Point::Allocation("walk", 1000) = point {
                observed.set(true);
            }
        });
        let p = Params {
            dimension: 1,
            walk_length: 1000,
            walks_per_node: 1,
            window: 1,
            negative_samples: 0,
            epochs: 1,
            ..Params::default()
        };
        let result = run(
            &g,
            &p,
            &Control::new(Arc::new(AtomicBool::new(false)), None, 32),
        );
        println!(
            "F14_NODE2VEC_BUDGET walk_allocation_reached={} result={:?}",
            allocated.get(),
            result.as_ref().map(|v| v.len())
        );
        assert!(
            matches!(result, Err(crate::Error::BudgetExceeded(_))),
            "{result:?}"
        );
        assert!(!allocated.get());
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        dot / (na * nb)
    }

    #[test]
    fn deterministic_and_separates_two_cliques() {
        let mut e = Vec::new();
        for a in 0..6u32 {
            for b in a + 1..6 {
                e.push((a, b));
                e.push((a + 6, b + 6));
            }
        }
        e.push((5, 6));
        let g = ugraph(12, &e);
        let p = Params {
            dimension: 16,
            walk_length: 20,
            walks_per_node: 20,
            window: 4,
            epochs: 2,
            seed: 5,
            ..Params::default()
        };
        let a = run(&g, &p, &Control::unlimited()).unwrap();
        let b = run(&g, &p, &Control::unlimited()).unwrap();
        assert_eq!(a, b);
        let v = |i: usize| &a[i * 16..(i + 1) * 16];
        let within =
            (cosine(v(0), v(1)) + cosine(v(2), v(3)) + cosine(v(8), v(9)) + cosine(v(10), v(11)))
                / 4.0;
        let across =
            (cosine(v(0), v(11)) + cosine(v(1), v(10)) + cosine(v(2), v(9)) + cosine(v(3), v(8)))
                / 4.0;
        assert!(within > across, "within {within} across {across}");
    }

    #[test]
    fn seeded_branching_fixture() {
        let g = ugraph(4, &[(0, 1), (0, 2), (1, 2), (2, 3)]);
        let p = Params {
            dimension: 3,
            walk_length: 7,
            walks_per_node: 2,
            window: 3,
            negative_samples: 3,
            epochs: 2,
            seed: 1729,
            return_factor: 2.0,
            in_out_factor: 4.0,
            ..Params::default()
        };
        let values = run(&g, &p, &Control::unlimited()).unwrap();
        assert_eq!(
            values.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            [
                3180703203, 1043649593, 1006374875, 1043180213, 1034735564, 1041152539, 1045586290,
                1025171187, 3190191322, 1042132246, 1032796098, 3179680921
            ]
        );
    }

    fn tiny_params() -> Params {
        Params {
            dimension: 3,
            walk_length: 8,
            walks_per_node: 2,
            window: 4,
            negative_samples: 2,
            epochs: 2,
            ..Params::default()
        }
    }

    #[test]
    fn cancellation_checks_every_phase_and_releases_all_buffers() {
        use std::{
            cell::Cell,
            rc::Rc,
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };
        let g = ugraph(3, &[(0, 1), (1, 2), (2, 0)]);
        for phase in [
            "degree",
            "initialize",
            "initialize-output",
            "noise",
            "initialize-gradient",
            "order",
            "epoch",
            "pass",
            "shuffle",
            "start",
            "walk",
            "neighbor",
            "center",
            "context",
            "clear-gradient",
            "negative",
            "dot",
            "update-output",
            "update-input",
        ] {
            let flag = Arc::new(AtomicBool::new(false));
            let f = flag.clone();
            let seen = Rc::new(Cell::new(false));
            let s = seen.clone();
            let since = Rc::new(Cell::new(0usize));
            let count = since.clone();
            let _hook = probe::install(move |point| {
                if let probe::Point::Work(p) = point {
                    if p == phase {
                        s.set(true);
                        f.store(true, Ordering::Relaxed);
                    }
                    if s.get() {
                        count.set(count.get() + 1);
                    }
                }
            });
            let ctl = Control::new(flag, None, 1_000_000);
            let result = run(&g, &tiny_params(), &ctl);
            assert!(seen.get(), "phase {phase} was not exercised");
            assert!(
                matches!(result, Err(crate::Error::Cancelled)),
                "{phase}: {result:?}"
            );
            assert!(since.get() <= CHECK_EVERY, "{phase}: {}", since.get());
            assert_eq!(ctl.remaining(), 1_000_000, "leaked {phase} reservation");
        }
    }

    #[test]
    fn large_inner_loops_stop_within_the_shared_work_bound() {
        use std::{
            cell::Cell,
            rc::Rc,
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };
        let small = ugraph(2, &[(0, 1)]);
        let edges: Vec<_> = (1..CHECK_EVERY as u32 + 3).map(|v| (0, v)).collect();
        let star = ugraph(CHECK_EVERY as u32 + 3, &edges);
        for (phase, g, p) in [
            (
                "neighbor",
                &star,
                Params {
                    dimension: 1,
                    walk_length: 2,
                    walks_per_node: 1,
                    negative_samples: 0,
                    epochs: 1,
                    ..Params::default()
                },
            ),
            (
                "dot",
                &small,
                Params {
                    dimension: CHECK_EVERY + 3,
                    walk_length: 2,
                    walks_per_node: 1,
                    negative_samples: 0,
                    epochs: 1,
                    ..Params::default()
                },
            ),
            (
                "negative",
                &small,
                Params {
                    dimension: 1,
                    walk_length: 2,
                    walks_per_node: 1,
                    negative_samples: CHECK_EVERY + 3,
                    epochs: 1,
                    ..Params::default()
                },
            ),
            (
                "context",
                &small,
                Params {
                    dimension: 1,
                    walk_length: 300,
                    window: usize::MAX,
                    walks_per_node: 1,
                    negative_samples: 0,
                    epochs: 1,
                    ..Params::default()
                },
            ),
            (
                "epoch",
                &small,
                Params {
                    epochs: usize::MAX,
                    walks_per_node: 0,
                    ..tiny_params()
                },
            ),
        ] {
            let flag = Arc::new(AtomicBool::new(false));
            let f = flag.clone();
            let seen = Rc::new(Cell::new(false));
            let s = seen.clone();
            let since = Rc::new(Cell::new(0usize));
            let c = since.clone();
            let _hook = probe::install(move |point| {
                if let probe::Point::Work(p) = point {
                    if p == phase {
                        s.set(true);
                        f.store(true, Ordering::Relaxed);
                    }
                    if s.get() {
                        c.set(c.get() + 1);
                    }
                }
            });
            let ctl = Control::new(flag, None, 32_000_000);
            assert!(
                matches!(run(g, &p, &ctl), Err(crate::Error::Cancelled)),
                "{phase}"
            );
            assert!(seen.get(), "{phase}");
            assert!(since.get() <= CHECK_EVERY, "{phase}: {}", since.get());
            assert_eq!(ctl.remaining(), 32_000_000);
        }
    }

    #[test]
    fn deadlines_and_already_cancelled_controls() {
        use std::{
            sync::{Arc, atomic::AtomicBool},
            time::{Duration, Instant},
        };
        let g = ugraph(2, &[(0, 1)]);
        let cancelled = Control::new(Arc::new(AtomicBool::new(true)), None, 0);
        assert!(matches!(
            run(&g, &tiny_params(), &cancelled),
            Err(crate::Error::Cancelled)
        ));
        let timeout = Control::new(Arc::new(AtomicBool::new(false)), Some(Instant::now()), 0);
        assert!(matches!(
            run(&g, &tiny_params(), &timeout),
            Err(crate::Error::Timeout)
        ));
        let valid = Control::new(
            Arc::new(AtomicBool::new(false)),
            Some(Instant::now() + Duration::from_secs(30)),
            1_000_000,
        );
        assert_eq!(run(&g, &tiny_params(), &valid).unwrap().len(), 6);
    }

    #[test]
    fn capacity_accounting_has_exact_peak_and_retains_only_output() {
        use std::{
            cell::RefCell,
            rc::Rc,
            sync::{Arc, atomic::AtomicBool},
        };
        let g = ugraph(2, &[(0, 1)]);
        let p = Params {
            dimension: 1,
            walk_length: 1000,
            walks_per_node: 1,
            window: 1,
            negative_samples: 0,
            epochs: 1,
            ..Params::default()
        };
        // Independent live-capacity oracle: two n*d*f32, n*f64, n*u32,
        // walk_length*u32, max_degree*f64, d*f32. No graph allocation is included.
        let expected = 2 * 2 * 4 + 2 * 8 + 2 * 4 + 1000 * 4 + 8 + 4;
        assert_eq!(expected, 4052);
        let ctl = Control::new(Arc::new(AtomicBool::new(false)), None, expected);
        let c = ctl.clone();
        let observations = Rc::new(RefCell::new(Vec::new()));
        let observed = observations.clone();
        let _hook = probe::install(move |point| {
            if let probe::Point::Reserved(name, capacity) = point {
                observed
                    .borrow_mut()
                    .push((name, capacity, expected - c.remaining()));
            }
        });
        let output = run(&g, &p, &ctl).unwrap();
        assert_eq!(
            &*observations.borrow(),
            &[
                ("syn0", 2, 8),
                ("syn1", 2, 16),
                ("noise", 2, 32),
                ("walk", 1000, 4032),
                ("sampling", 1, 4040),
                ("gradient", 1, 4044),
                ("order", 2, 4052)
            ]
        );
        assert_eq!(ctl.peak(), expected);
        assert_eq!(ctl.remaining(), expected - 8);
        assert_eq!(output.len(), 2);
        drop(output);
        // Control holds the returned Vec's charge for the call, not a Vec drop hook.
        assert_eq!(ctl.remaining(), expected - 8);
        let short = Control::new(Arc::new(AtomicBool::new(false)), None, expected - 1);
        assert!(matches!(
            run(&g, &p, &short),
            Err(crate::Error::BudgetExceeded(_))
        ));
        assert_eq!(short.remaining(), expected - 1);
    }

    #[test]
    fn every_allocation_failure_and_budget_refusal_rolls_back() {
        use std::sync::{Arc, atomic::AtomicBool};
        let g = ugraph(2, &[(0, 1)]);
        let p = tiny_params();
        for name in [
            "syn0", "syn1", "noise", "walk", "sampling", "gradient", "order",
        ] {
            let ctl = Control::new(Arc::new(AtomicBool::new(false)), None, 1_000_000);
            // Explicit caller-path adapter models fallible reserve, without OOM.
            let failure = probe::fail(name);
            assert!(
                matches!(run(&g, &p, &ctl), Err(crate::Error::Invalid(_))),
                "{name}"
            );
            drop(failure);
            assert_eq!(ctl.remaining(), 1_000_000, "{name}");
        }
        for limit in [0, 23, 47, 63, 95, 103, 115, 123] {
            let ctl = Control::new(Arc::new(AtomicBool::new(false)), None, limit);
            assert!(
                matches!(run(&g, &p, &ctl), Err(crate::Error::BudgetExceeded(_))),
                "{limit}"
            );
            assert_eq!(ctl.remaining(), limit);
        }
    }

    #[test]
    fn overflowing_capacities_refuse_before_allocation_and_extreme_windows_work() {
        use std::{cell::Cell, rc::Rc};
        let g = ugraph(2, &[(0, 1)]);
        for p in [
            Params {
                dimension: usize::MAX,
                ..tiny_params()
            },
            Params {
                walk_length: usize::MAX,
                ..tiny_params()
            },
            Params {
                negative_samples: usize::MAX,
                ..tiny_params()
            },
            Params {
                epochs: usize::MAX,
                walks_per_node: 2,
                ..tiny_params()
            },
            Params {
                walks_per_node: usize::MAX,
                epochs: 1,
                ..tiny_params()
            },
        ] {
            let allocated = Rc::new(Cell::new(false));
            let a = allocated.clone();
            let _hook = probe::install(move |point| {
                if matches!(point, probe::Point::Allocation(..)) {
                    a.set(true);
                }
            });
            assert!(matches!(
                run(&g, &p, &Control::unlimited()),
                Err(crate::Error::Invalid(_))
            ));
            assert!(!allocated.get());
        }
        for window in [0, usize::MAX] {
            assert_eq!(
                run(
                    &g,
                    &Params {
                        window,
                        ..tiny_params()
                    },
                    &Control::unlimited()
                )
                .unwrap()
                .len(),
                6
            );
        }
    }

    #[test]
    fn empty_and_zero_training_calls_keep_parameter_and_output_contracts() {
        let empty = ugraph(0, &[]);
        assert!(
            run(&empty, &tiny_params(), &Control::unlimited())
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            run(
                &empty,
                &Params {
                    dimension: 0,
                    ..tiny_params()
                },
                &Control::unlimited()
            ),
            Err(crate::Error::Invalid(_))
        ));
        assert!(matches!(
            run(
                &empty,
                &Params {
                    walk_length: 1,
                    ..tiny_params()
                },
                &Control::unlimited()
            ),
            Err(crate::Error::Invalid(_))
        ));
        let g = ugraph(2, &[]);
        let no_epochs = run(
            &g,
            &Params {
                epochs: 0,
                ..tiny_params()
            },
            &Control::unlimited(),
        )
        .unwrap();
        let no_walks = run(
            &g,
            &Params {
                walks_per_node: 0,
                ..tiny_params()
            },
            &Control::unlimited(),
        )
        .unwrap();
        assert_eq!(no_epochs, no_walks);
        assert_eq!(no_epochs.len(), 6);
        assert!(no_epochs.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn reservation_unwind_drops_every_temporary() {
        use std::sync::{Arc, atomic::AtomicBool};
        let g = ugraph(2, &[(0, 1)]);
        let ctl = Control::new(Arc::new(AtomicBool::new(false)), None, 1_000_000);
        let _hook = probe::install(|point| {
            if let probe::Point::Reserved("order", _) = point {
                panic!("intentional node2vec reservation unwind");
            }
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(&g, &tiny_params(), &ctl)
        }));
        assert!(result.is_err());
        assert_eq!(ctl.remaining(), 1_000_000);
    }
}
