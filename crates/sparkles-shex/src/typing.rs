//! The typing: (node, pair kind) pairs discovered breadth first from the fixed map, with
//! a three-valued local pre-check that keeps definitely failing pairs from expanding,
//! then refined per stratum to the greatest fixed point in parallel waves.
//!
//! **Discovery.** A pair is evaluated with every undecided reference read as unknown
//! (Kleene logic). A definite answer is final: `false` pairs are not expanded, which
//! prunes most of the graph when data are locally invalid. Otherwise the pairs it read
//! (the same node under another label, the values of its arcs under their constraints'
//! value expressions) become its edges and join the next frontier.
//!
//! **Refinement.** Strata are refined in increasing order; within one, every reference
//! is positive, so evaluation is monotone in the stratum's pairs. All undecided pairs
//! start alive (`true`); each wave re-evaluates the dirty ones against the current
//! typing, the failures become `false`, and the alive pairs with an edge to a failure
//! are the next wave's dirty set. What stays alive is the greatest fixed point. Lower
//! strata are final before a stratum starts, so `NOT` and EXTRA read final values.
//!
//! Nothing recurses through the data: one evaluation reads the typing and never
//! evaluates another pair, so a reference chain of any length costs no stack.

use crate::ir::{Ir, PairKind, Se, SeId, TcId, Tri};
use crate::matcher::{self, Budget};
use crate::nc::NcPlan;
use crate::neigh::{self, FetchOutcome, Neigh, SnapPlan};
use crate::semact::{ActCtx, Registry};
use oxrdf::Term;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use sparkles::id::{Id, Tag};
use sparkles::store::Snapshot;
use sparkles::validation::DataGraph;
use sparkles::{Budget as Exceeded, BudgetKind};
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Instant;

/// Pair states in the typing.
const UNDECIDED: u8 = 0;
const ALIVE: u8 = 1;
const TRUE: u8 = 2;
const FALSE: u8 = 3;

/// Evaluations between checks of the timeout and cancellation.
const CHECK_EVERY: usize = 256;
/// Pairs per parallel task, and the least pairs of a wave that run in parallel.
const CHUNK: usize = 256;
const PARALLEL_MIN: usize = 512;

/// The timeout and cancellation of a validation.
#[derive(Clone, Default)]
pub struct Limits {
    pub deadline: Option<Instant>,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Limits {
    /// [`sparkles::Error::Cancelled`] or [`sparkles::Error::Timeout`] once due.
    pub fn check(&self) -> sparkles::Result<()> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            return Err(sparkles::Error::Cancelled);
        }
        if self.deadline.is_some_and(|d| Instant::now() > d) {
            return Err(sparkles::Error::Timeout);
        }
        Ok(())
    }
}

/// What evaluating pairs needs: the compiled schema resolved against a snapshot and a
/// data graph, and the limits.
pub struct Env<'a> {
    pub ir: &'a Ir,
    pub snap: &'a Arc<Snapshot>,
    pub data: &'a DataGraph,
    pub plan: SnapPlan,
    pub nc: NcPlan,
    /// the semantic-action handlers while typing (no `print` output kept)
    pub registry: Registry,
    /// map nodes that are not in the store, by the payload of their local id
    pub absent: Vec<Term>,
    pub max_partitions: Option<u64>,
    pub max_pairs: Option<usize>,
    pub limits: Limits,
    pub parallel: bool,
    pub pool: Option<Arc<rayon::ThreadPool>>,
    /// per shape expression: no reference or shape below it, so it is evaluated at the
    /// value instead of through a pair
    pure: Vec<bool>,
}

/// Per-thread scratch: a neighbourhood buffer and the semantic-action state.
pub struct Worker<'a> {
    pub neigh: Neigh,
    pub cx: ActCtx<'a>,
    evals: usize,
}

impl<'a> Worker<'a> {
    pub fn new(registry: &'a Registry, snap: &'a Snapshot) -> Worker<'a> {
        Worker {
            neigh: Neigh::default(),
            cx: ActCtx::new(registry, snap),
            evals: 0,
        }
    }
}

/// How evaluation reads the typing: the pair (node, kind).
pub type Read<'r> = dyn Fn(Id, PairKind) -> Tri + 'r;

impl<'a> Env<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ir: &'a Ir,
        snap: &'a Arc<Snapshot>,
        data: &'a DataGraph,
        prefixes: &crate::PrefixMap,
        absent: Vec<Term>,
        opts: &crate::ValidateOptions,
    ) -> Env<'a> {
        let mut pure = vec![None; ir.ses.len()];
        for se in 0..ir.ses.len() {
            is_pure(ir, SeId(se as u32), &mut pure);
        }
        Env {
            ir,
            snap,
            data,
            plan: SnapPlan::new(snap, ir),
            nc: NcPlan::new(snap, &ir.ncs).with_prefixes(prefixes),
            registry: Registry::new(false),
            absent,
            max_partitions: opts.max_partitions,
            max_pairs: opts.max_pairs,
            limits: Limits {
                deadline: opts.timeout.map(|t| Instant::now() + t),
                cancel: opts.cancel.clone(),
            },
            parallel: opts.parallel,
            pool: opts.pool.clone(),
            pure: pure.into_iter().map(|p| p.unwrap_or(false)).collect(),
        }
    }

    /// The term of a node id (map nodes not in the store included).
    pub fn term(&self, id: Id) -> Term {
        if id.tag() == Tag::Local
            && let Some(t) = self.absent.get(id.payload() as usize)
        {
            return t.clone();
        }
        self.snap
            .term(id)
            .unwrap_or_else(|| oxrdf::NamedNode::new_unchecked("urn:x-sparkles:unknown").into())
    }

    /// Is `se` evaluated at the value, without a pair?
    pub fn is_pure(&self, se: SeId) -> bool {
        self.pure[se.index()]
    }

    /// Does `node` satisfy node constraint `nc`?
    pub fn nc_check(&self, nc: crate::ir::NcId, node: Id) -> anyhow::Result<bool> {
        if node.tag() == Tag::Local
            && let Some(t) = self.absent.get(node.payload() as usize)
        {
            return self.nc.check_term(nc, t);
        }
        self.nc.check(nc, node)
    }

    /// A pure shape expression at a node.
    pub fn eval_pure(&self, se: SeId, node: Id) -> anyhow::Result<bool> {
        Ok(match &self.ir.ses[se.index()] {
            Se::Nc(n) => self.nc_check(*n, node)?,
            Se::And(xs) => {
                for &x in xs {
                    if !self.eval_pure(x, node)? {
                        return Ok(false);
                    }
                }
                true
            }
            Se::Or(xs) => {
                for &x in xs {
                    if self.eval_pure(x, node)? {
                        return Ok(true);
                    }
                }
                false
            }
            Se::Not(x) => !self.eval_pure(*x, node)?,
            Se::Ref(_) | Se::Shape(_) | Se::External => {
                anyhow::bail!("internal: shape expression evaluated without the typing")
            }
        })
    }

    /// Does `node` satisfy `se` under the typing `read`?
    pub fn eval(
        &self,
        se: SeId,
        node: Id,
        read: &Read<'_>,
        w: &mut Worker<'_>,
    ) -> anyhow::Result<Tri> {
        Ok(match &self.ir.ses[se.index()] {
            Se::And(xs) => {
                let mut r = Tri::True;
                for &x in xs {
                    r = r.and(self.eval(x, node, read, w)?);
                    if r == Tri::False {
                        break;
                    }
                }
                r
            }
            Se::Or(xs) => {
                let mut r = Tri::False;
                for &x in xs {
                    r = r.or(self.eval(x, node, read, w)?);
                    if r == Tri::True {
                        break;
                    }
                }
                r
            }
            Se::Not(x) => !self.eval(*x, node, read, w)?,
            Se::Ref(k) => read(node, *k),
            Se::Nc(n) => Tri::from(self.nc_check(*n, node)?),
            Se::Shape(s) => self.shape(*s, node, read, w)?,
            Se::External => anyhow::bail!("an external shape has no definition"),
        })
    }

    /// Does `node` match shape `sid` under the typing `read`?
    fn shape(
        &self,
        sid: crate::ir::ShapeId,
        node: Id,
        read: &Read<'_>,
        w: &mut Worker<'_>,
    ) -> anyhow::Result<Tri> {
        let Worker { neigh, cx, .. } = w;
        if neigh::fetch(self.data, &self.plan, node, sid, neigh)? == FetchOutcome::FailFast {
            return Ok(Tri::False);
        }
        let shape = &self.ir.shapes[sid.index()];
        let error = RefCell::new(None);
        let value = |v: Id, tc: TcId| self.value(shape, tc, v, read, &error);
        let mut budget = Budget::new(self.max_partitions);
        let r = matcher::matches(shape, neigh, &value, &mut budget, cx)?;
        match error.into_inner() {
            Some(e) => Err(e),
            None => Ok(r),
        }
    }

    /// Does value `v` satisfy the value expression of constraint `tc`: at the value for a
    /// pure expression, through the pair otherwise.
    pub fn value(
        &self,
        shape: &crate::ir::ShapeIr,
        tc: TcId,
        v: Id,
        read: &Read<'_>,
        error: &RefCell<Option<anyhow::Error>>,
    ) -> Tri {
        let t = &shape.tcs[tc.index()];
        let Some(se) = t.value else {
            return Tri::True;
        };
        if self.is_pure(se) {
            return match self.eval_pure(se, v) {
                Ok(b) => Tri::from(b),
                Err(e) => {
                    error.borrow_mut().get_or_insert(e);
                    Tri::False
                }
            };
        }
        match t.pair {
            Some(k) => read(v, k),
            None => {
                let e = anyhow::anyhow!("internal: a value expression without a pair kind");
                error.borrow_mut().get_or_insert(e);
                Tri::False
            }
        }
    }

    /// Run `f` on each item with per-task scratch: in parallel chunks for large inputs,
    /// checking the limits every [`CHECK_EVERY`] evaluations.
    pub fn par_map<T, F>(&self, registry: &Registry, items: &[u32], f: F) -> anyhow::Result<Vec<T>>
    where
        T: Send,
        F: Fn(&mut Worker<'_>, u32) -> anyhow::Result<T> + Sync,
    {
        let chunk = |xs: &[u32]| -> anyhow::Result<Vec<T>> {
            let mut w = Worker::new(registry, self.snap);
            let mut out = Vec::with_capacity(xs.len());
            for &x in xs {
                if w.evals.is_multiple_of(CHECK_EVERY) {
                    self.limits.check()?;
                }
                w.evals += 1;
                out.push(f(&mut w, x)?);
            }
            Ok(out)
        };
        if !self.parallel || items.len() < PARALLEL_MIN {
            return chunk(items);
        }
        let run =
            || -> Vec<anyhow::Result<Vec<T>>> { items.par_chunks(CHUNK).map(chunk).collect() };
        let parts = match &self.pool {
            Some(pool) => pool.install(run),
            None => run(),
        };
        let mut out = Vec::with_capacity(items.len());
        for p in parts {
            out.extend(p?);
        }
        Ok(out)
    }
}

/// Is `se` free of references and shapes (memoized in `memo`)?
fn is_pure(ir: &Ir, se: SeId, memo: &mut [Option<bool>]) -> bool {
    if let Some(p) = memo[se.index()] {
        return p;
    }
    // a provisional answer stops cycles, which only go through references
    memo[se.index()] = Some(false);
    let p = match &ir.ses[se.index()] {
        Se::Nc(_) => true,
        Se::And(xs) | Se::Or(xs) => xs.iter().all(|&x| is_pure(ir, x, memo)),
        Se::Not(x) => is_pure(ir, *x, memo),
        Se::Ref(_) | Se::Shape(_) | Se::External => false,
    };
    memo[se.index()] = Some(p);
    p
}

/// The typing graph: the discovered pairs, their edges and their states.
pub struct Typing {
    index: FxHashMap<(Id, PairKind), u32>,
    pairs: Vec<(Id, PairKind)>,
    state: Vec<AtomicU8>,
    /// the pairs each pair read (forward edges), as offsets into `edges`
    starts: Vec<u32>,
    edges: Vec<u32>,
    /// refinement waves per stratum
    pub waves: Vec<usize>,
    /// pair evaluations
    pub evaluations: u64,
}

impl Typing {
    /// The number of discovered pairs.
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// The index of a pair, if discovered.
    pub fn get(&self, node: Id, kind: PairKind) -> Option<u32> {
        self.index.get(&(node, kind)).copied()
    }

    /// The final value of a discovered pair.
    pub fn value(&self, idx: u32) -> bool {
        self.state[idx as usize].load(Ordering::Relaxed) == TRUE
    }

    /// The typing as evaluation reads it: decided and alive pairs, `Unknown` for the
    /// rest.
    pub fn read(&self, node: Id, kind: PairKind) -> Tri {
        match self.get(node, kind) {
            Some(i) => match self.state[i as usize].load(Ordering::Relaxed) {
                ALIVE | TRUE => Tri::True,
                FALSE => Tri::False,
                _ => Tri::Unknown,
            },
            None => Tri::Unknown,
        }
    }

    /// The value of a pair decided for good, if it is.
    fn decided(&self, node: Id, kind: PairKind) -> Option<bool> {
        match self.state[self.get(node, kind)? as usize].load(Ordering::Relaxed) {
            TRUE => Some(true),
            FALSE => Some(false),
            _ => None,
        }
    }

    fn intern(&mut self, pair: (Id, PairKind), max_pairs: Option<usize>) -> anyhow::Result<u32> {
        if let Some(&i) = self.index.get(&pair) {
            return Ok(i);
        }
        let i = self.pairs.len() as u32;
        if let Some(limit) = max_pairs
            && self.pairs.len() >= limit
        {
            return Err(sparkles::Error::BudgetExceeded(Exceeded {
                kind: BudgetKind::ValidationWork,
                limit: limit as u64,
                requested: limit as u64 + 1,
            })
            .into());
        }
        self.index.insert(pair, i);
        self.pairs.push(pair);
        self.state.push(AtomicU8::new(UNDECIDED));
        Ok(i)
    }
}

/// Compute the typing of the pairs reachable from `seeds`.
pub fn run(env: &Env<'_>, seeds: &[(Id, PairKind)]) -> anyhow::Result<Typing> {
    let mut t = Typing {
        index: FxHashMap::default(),
        pairs: Vec::new(),
        state: Vec::new(),
        starts: vec![0],
        edges: Vec::new(),
        waves: Vec::new(),
        evaluations: 0,
    };
    for &s in seeds {
        t.intern(s, env.max_pairs)?;
    }
    discover(env, &mut t)?;
    refine(env, &mut t)?;
    Ok(t)
}

/// Breadth-first discovery with the three-valued pre-check.
fn discover(env: &Env<'_>, t: &mut Typing) -> anyhow::Result<()> {
    let mut lo = 0usize;
    while lo < t.pairs.len() {
        env.limits.check()?;
        let hi = t.pairs.len();
        let frontier: Vec<u32> = (lo as u32..hi as u32).collect();
        t.evaluations += frontier.len() as u64;
        let tr: &Typing = t;
        let results = env.par_map(&env.registry, &frontier, |w, i| {
            let (node, kind) = tr.pairs[i as usize];
            let reads = RefCell::new(Vec::new());
            // decided pairs are final; the others are unknown and become edges
            let read = |n: Id, k: PairKind| match tr.decided(n, k) {
                Some(b) => Tri::from(b),
                None => {
                    reads.borrow_mut().push((n, k));
                    Tri::Unknown
                }
            };
            let r = env.eval(env.ir.pairs[kind.index()].se, node, &read, w)?;
            let mut reads = reads.into_inner();
            if r != Tri::Unknown {
                reads.clear();
            }
            Ok((r, reads))
        })?;
        for (i, (r, mut reads)) in results.into_iter().enumerate() {
            let state = match r {
                Tri::True => TRUE,
                Tri::False => FALSE,
                Tri::Unknown => ALIVE,
            };
            t.state[lo + i].store(state, Ordering::Relaxed);
            reads.sort_unstable();
            reads.dedup();
            for p in reads {
                let j = t.intern(p, env.max_pairs)?;
                t.edges.push(j);
            }
            t.starts.push(t.edges.len() as u32);
        }
        lo = hi;
    }
    Ok(())
}

/// Per stratum, the greatest fixed point in waves.
fn refine(env: &Env<'_>, t: &mut Typing) -> anyhow::Result<()> {
    let n = t.pairs.len();
    // reverse edges
    let mut rstarts = vec![0u32; n + 1];
    for &j in &t.edges {
        rstarts[j as usize + 1] += 1;
    }
    for i in 0..n {
        rstarts[i + 1] += rstarts[i];
    }
    let mut fill = rstarts.clone();
    let mut redges = vec![0u32; t.edges.len()];
    for i in 0..n {
        for &j in &t.edges[t.starts[i] as usize..t.starts[i + 1] as usize] {
            redges[fill[j as usize] as usize] = i as u32;
            fill[j as usize] += 1;
        }
    }
    let stratum = |i: usize| env.ir.pairs[t.pairs[i].1.index()].stratum;
    let last = env.ir.strata.max(1) - 1;
    let mut by_stratum: Vec<Vec<u32>> = vec![Vec::new(); last as usize + 1];
    for i in 0..n {
        if t.state[i].load(Ordering::Relaxed) == ALIVE {
            by_stratum[stratum(i).min(last) as usize].push(i as u32);
        }
    }
    let mut mark = vec![usize::MAX; n];
    let mut waves = 0usize;
    let mut per_stratum = Vec::with_capacity(by_stratum.len());
    for (s, alive) in by_stratum.iter().enumerate() {
        let s = s as u32;
        let first = waves;
        let mut dirty = alive.clone();
        while !dirty.is_empty() {
            env.limits.check()?;
            waves += 1;
            t.evaluations += dirty.len() as u64;
            let tr: &Typing = t;
            let failed = env.par_map(&env.registry, &dirty, |w, i| {
                let (node, kind) = tr.pairs[i as usize];
                let read = |n: Id, k: PairKind| tr.read(n, k);
                match env.eval(env.ir.pairs[kind.index()].se, node, &read, w)? {
                    Tri::Unknown => anyhow::bail!("internal: a pair read an undiscovered pair"),
                    r => Ok(r == Tri::False),
                }
            })?;
            let failed: Vec<u32> = dirty
                .iter()
                .zip(failed)
                .filter_map(|(&i, f)| f.then_some(i))
                .collect();
            for &f in &failed {
                t.state[f as usize].store(FALSE, Ordering::Relaxed);
            }
            dirty.clear();
            for &f in &failed {
                let f = f as usize;
                for &q in &redges[rstarts[f] as usize..rstarts[f + 1] as usize] {
                    let qi = q as usize;
                    // readers in higher strata wait for their own stratum
                    if mark[qi] != waves
                        && t.state[qi].load(Ordering::Relaxed) == ALIVE
                        && stratum(qi).min(last) == s
                    {
                        mark[qi] = waves;
                        dirty.push(q);
                    }
                }
            }
            dirty.sort_unstable();
        }
        per_stratum.push(waves - first);
        for &i in alive {
            let _ = t.state[i as usize].compare_exchange(
                ALIVE,
                TRUE,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }
    t.waves = per_stratum;
    Ok(())
}
