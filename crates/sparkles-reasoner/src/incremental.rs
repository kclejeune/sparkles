//! Incremental maintenance of a materialization with the backward/forward algorithm
//! (Motik, Nenov, Piro, Horrocks: *Incremental Update of Datalog Materialisation: the
//! Backward/Forward Algorithm*, AAAI 2015).
//!
//! The closure of the previous run is a [`Closure`]: the input graphs of a snapshot as
//! its base tier, and every derived fact, the generalized ones included, in memory as its
//! derived tier. An update reads the input graphs of the snapshot it brings the closure
//! to, so the inserted facts are in the base tier from its start and the deleted ones are
//! not. Given the explicit facts deleted and inserted since, an update
//!
//! 1. **deletes** what no longer follows. A deleted explicit fact is a candidate, held
//!    in the derived tier while the search decides whether it still follows. Before a
//!    candidate is deleted, a backward search looks for another proof of it among the
//!    facts not deleted, and keeps it if one exists. Only facts without a proof are
//!    deleted, and the heads of the rule instances that used them become candidates.
//!    DRed (Gupta, Mumick, Subrahmanian 1993) would delete every consequence first and
//!    derive the survivors again; with RDFS that deletes most of the closure for a single
//!    `rdf:type` triple, through `rdfs:Resource` and the class axioms;
//! 2. **inserts** the new explicit facts and runs the rules semi-naively from them, with
//!    the closure that remains as the old facts. The inserted facts are delta rows of the
//!    derived tier, flagged explicit, for the length of the run, and leave it at its
//!    end, since the base tier holds them.
//!
//! The backward search decides provability exactly. It explores the rule instances that
//! derive a fact, depth first, and counts for each instance the body facts not proved
//! yet. Explicit facts and facts proved earlier in the run are proved. When the search
//! finds no proof and has explored every instance it reached, none of the facts it
//! reached has a proof, and they are all remembered as unprovable.

use crate::budget::{Budget, Held, MemoryExceeded, table_bytes, table_capacity, table_grown};
use crate::engine::{self, CHead, CRule, Eval, First, Limits, Mode, Slot, Step};
use crate::facts::Base;
use crate::graph::{Cands, Graph, Triple};
use crate::terms::Terms;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_core::id::Id;
use sparkles_core::store::Snapshot;
use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A run that cannot be maintained incrementally and must materialize in full.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub(crate) struct Fallback(pub String);

/// A materialization: the input graphs of a snapshot as the base tier of its graph (see
/// [`crate::facts`]), and the derived facts in memory as its derived tier.
///
/// A fact is explicit when the base tier holds it. The derived tier holds the facts the
/// base tier does not, and a flag marks those of them that count as explicit too: the
/// GeoSPARQL default geometries, and for the length of an update the inserted facts.
/// Whether a fact of the closure is explicit therefore needs no read of the store: it
/// is explicit when the derived tier does not hold it, or holds it flagged.
pub(crate) struct Closure {
    pub graph: Graph,
    pub terms: Terms,
    /// one bit per row of the derived tier, from the graph's offset on: the row holds a
    /// fact that counts as explicit
    flags: Vec<u64>,
    /// the heap of `flags`, charged to the budget of the graph
    flags_held: Held,
    /// the GeoSPARQL default geometries the closure holds as explicit facts, when they
    /// are known (see [`crate::default_geometry_changes`])
    pub defaults: Option<Defaults>,
}

/// The GeoSPARQL default geometries of a closure: each feature's default geometry, and
/// the id of `geo:hasDefaultGeometry`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Defaults {
    pub pred: u64,
    pub of: FxHashMap<u64, u64>,
}

impl Defaults {
    /// Whether `t` is one of the default geometries.
    pub fn contains(&self, t: &Triple) -> bool {
        t[1] == self.pred && self.of.get(&t[0]) == Some(&t[2])
    }

    pub fn facts(&self) -> impl Iterator<Item = Triple> + '_ {
        self.of.iter().map(|(f, g)| [*f, self.pred, *g])
    }

    /// The heap of the map.
    pub fn heap_bytes(&self) -> usize {
        table_bytes(self.of.capacity(), size_of::<(u64, u64)>())
    }

    /// Replace ids that moved. The new map is charged before it is built, beside the old
    /// one (which stays charged to the closure until it settles), failing past the limit
    /// when `strict`.
    fn remap(
        &mut self,
        moved: &FxHashMap<u64, u64>,
        budget: &Arc<Budget>,
        strict: bool,
    ) -> Result<(), MemoryExceeded> {
        let m = |x: u64| moved.get(&x).copied().unwrap_or(x);
        self.pred = m(self.pred);
        if self
            .of
            .iter()
            .any(|(f, g)| moved.contains_key(f) || moved.contains_key(g))
        {
            let mut held = Held::new(budget);
            let bytes = table_bytes(table_capacity(self.of.len()), size_of::<(u64, u64)>());
            if strict {
                held.try_grow(bytes)?;
            } else {
                held.grow(bytes);
            }
            self.of = self.of.iter().map(|(f, g)| (m(*f), m(*g))).collect();
        }
        Ok(())
    }
}

impl Closure {
    /// The closure of `graph`, whose base tier holds the explicit facts and whose
    /// derived tier holds the derived ones, none of them flagged.
    pub fn new(graph: Graph, terms: Terms) -> Closure {
        let flags_held = Held::new(graph.budget());
        Closure {
            graph,
            terms,
            flags: Vec::new(),
            flags_held,
            defaults: None,
        }
    }

    /// The heap of the closure: its derived tier, its local terms, the flags and the
    /// default geometries. The base tier is the store's.
    pub fn heap_bytes(&self) -> usize {
        self.graph.heap_bytes() + self.terms.heap_bytes() + self.own_bytes()
    }

    fn own_bytes(&self) -> usize {
        self.flags.capacity() * size_of::<u64>()
            + self.defaults.as_ref().map_or(0, Defaults::heap_bytes)
    }

    /// Charge the closure to `budget` from now on, or fail when it does not fit.
    pub fn set_budget(&mut self, budget: &Arc<Budget>) -> Result<(), MemoryExceeded> {
        let held = Held::try_new(budget, self.own_bytes())?;
        self.graph.set_budget(budget)?;
        self.terms.set_budget(budget)?;
        self.flags_held = held;
        Ok(())
    }

    /// Charge the change of the closure's own buffers (the flags and the default
    /// geometries) since the last call.
    pub fn settle(&mut self) {
        let own = self.own_bytes();
        self.flags_held.set(own);
        self.graph.settle();
    }

    /// [`settle`](Self::settle), failing instead of charging past the limit.
    pub fn try_settle(&mut self) -> Result<(), MemoryExceeded> {
        let own = self.own_bytes();
        self.flags_held.try_set(own)?;
        self.graph.try_settle()
    }

    /// Replace the base tier by the input graphs `graphs` of `snap`, a later snapshot of
    /// the same generation, in the order the run resolved them.
    pub fn set_inputs(&mut self, snap: Arc<Snapshot>, graphs: &[Id]) -> anyhow::Result<()> {
        let base = Base::new(snap, graphs)?;
        self.graph.set_base(Arc::new(base));
        Ok(())
    }

    /// Whether row `row` of the derived tier is flagged explicit.
    #[inline]
    pub fn flagged(&self, row: u32) -> bool {
        let Some(i) = row.checked_sub(self.graph.offset()) else {
            return false;
        };
        self.flags
            .get(i as usize / 64)
            .is_some_and(|w| w & (1 << (i % 64)) != 0)
    }

    pub fn set_flag(&mut self, row: u32, on: bool) {
        let done = self.set_flag_with(row, on, false);
        debug_assert!(done.is_ok(), "charges that may pass the limit do not fail");
    }

    /// [`set_flag`](Self::set_flag), charging the growth of the flags before it is
    /// allocated, and with `strict` failing when it does not fit.
    pub fn set_flag_with(
        &mut self,
        row: u32,
        on: bool,
        strict: bool,
    ) -> Result<(), MemoryExceeded> {
        let i = row - self.graph.offset();
        let w = i as usize / 64;
        if self.flags.len() <= w {
            if !on {
                return Ok(());
            }
            let cap = self.flags.capacity();
            if w + 1 > cap {
                let new = (cap * 2).max(w + 1).max(4);
                let bytes = (new - cap) * size_of::<u64>();
                if strict {
                    self.flags_held.try_grow(bytes)?;
                } else {
                    self.flags_held.grow(bytes);
                }
                self.flags.reserve_exact(new - self.flags.len());
            }
            self.flags.resize(w + 1, 0);
        }
        if on {
            self.flags[w] |= 1 << (i % 64);
        } else {
            self.flags[w] &= !(1 << (i % 64));
        }
        Ok(())
    }

    /// Whether a fact of the closure is explicit: the base tier holds it when the
    /// derived tier does not, and a fact of the derived tier is explicit when it is
    /// flagged. A triple that is not in the closure counts as explicit, so the caller
    /// asks only about facts it read from the closure.
    #[inline]
    pub fn explicit_held(&self, t: &Triple) -> bool {
        self.graph.position(t).is_none_or(|r| self.flagged(r))
    }

    /// The live facts of the derived tier, in row order, with their flags.
    pub fn live(&self) -> impl Iterator<Item = (Triple, bool)> + '_ {
        let g = &self.graph;
        let rows = (g.offset()..g.len()).filter(|&r| g.alive(r));
        rows.map(|r| (g.triple(r), self.flagged(r)))
    }

    /// Replace local ids by store ids (`moved`) in every live row.
    pub fn remap(&mut self, moved: &FxHashMap<u64, u64>) {
        let done = self.remap_with(moved, false);
        debug_assert!(done.is_ok(), "charges that may pass the limit do not fail");
    }

    /// [`remap`](Self::remap), charging the work beside the closure before it is
    /// allocated and failing past the limit. The closure is unusable after an error.
    pub fn try_remap(&mut self, moved: &FxHashMap<u64, u64>) -> Result<(), MemoryExceeded> {
        self.remap_with(moved, true)
    }

    fn remap_with(
        &mut self,
        moved: &FxHashMap<u64, u64>,
        strict: bool,
    ) -> Result<(), MemoryExceeded> {
        if moved.is_empty() {
            return Ok(());
        }
        let budget = self.graph.budget().clone();
        if let Some(d) = &mut self.defaults {
            d.remap(moved, &budget, strict)?;
        }
        let m = |x: u64| moved.get(&x).copied().unwrap_or(x);
        // the facts that move, in a buffer charged before each growth
        let fact = size_of::<(Triple, bool)>();
        let mut held = Held::new(&budget);
        let mut hit: Vec<(Triple, bool)> = Vec::new();
        for x in self
            .live()
            .filter(|(t, _)| t.iter().any(|x| moved.contains_key(x)))
        {
            if hit.len() == hit.capacity() {
                let cap = hit.capacity();
                let new = (cap * 2).max(4);
                if strict {
                    held.try_grow(new * fact)?;
                } else {
                    held.grow(new * fact);
                }
                hit.reserve_exact(new - hit.len());
                held.shrink(cap * fact);
            }
            hit.push(x);
        }
        for (t, _) in &hit {
            if let Some(r) = self.graph.position(t) {
                self.set_flag(r, false);
            }
            self.graph.kill(t);
        }
        for (t, flag) in hit {
            let n = [m(t[0]), m(t[1]), m(t[2])];
            match self.graph.position(&n) {
                Some(r) => {
                    if flag {
                        self.set_flag_with(r, true, strict)?;
                    }
                }
                None => {
                    if strict {
                        self.graph.try_add_batch([n])?;
                    } else {
                        self.graph.add_batch([n]);
                    }
                    let r = self.graph.len() - 1;
                    self.set_flag_with(r, flag, strict)?;
                }
            }
        }
        Ok(())
    }

    /// Rebuild the derived tier without its dead rows when they are a quarter of it or
    /// more. The new tier is built beside the old one, charged as it grows; with
    /// `strict`, failing past the limit (the closure is unusable then).
    pub fn compact_if_needed(&mut self, strict: bool) -> Result<(), MemoryExceeded> {
        let g = &self.graph;
        let rows = (g.len() - g.offset()) as usize;
        if (g.dead_rows() as usize) * 4 < rows {
            return Ok(());
        }
        let budget = g.budget().clone();
        let n = g.live();
        let mut held = Held::new(&budget);
        let list = n * size_of::<(Triple, bool)>() + n.div_ceil(64) * size_of::<u64>();
        if strict {
            held.try_grow(list)?;
        } else {
            held.grow(list);
        }
        let live: Vec<(Triple, bool)> = self.live().collect();
        let mut graph = g.try_like(live.len(), &budget, strict)?;
        if strict {
            graph.try_add_batch(live.iter().map(|x| x.0))?;
        } else {
            graph.add_batch(live.iter().map(|x| x.0));
        }
        let mut flags = vec![0u64; live.len().div_ceil(64)];
        for (r, (_, b)) in live.iter().enumerate() {
            if *b {
                flags[r / 64] |= 1 << (r % 64);
            }
        }
        self.graph = graph;
        self.flags = flags;
        drop(live);
        drop(held);
        self.settle();
        Ok(())
    }
}

/// What an update changed.
#[derive(Debug, Default)]
pub(crate) struct Update {
    /// facts removed from the closure (an insertion may have derived some of them again)
    pub deleted: Vec<Triple>,
    /// rows from this one on were added
    pub added_from: u32,
    /// facts whose provability was searched for
    pub checked: u64,
    /// semi-naive iterations of the insertion
    pub iterations: usize,
    /// the charge of the update's buffers that remain, `deleted` among them
    pub held: Option<Held>,
}

impl Update {
    /// The facts added to the closure.
    pub fn added<'a>(&self, c: &'a Closure) -> impl Iterator<Item = Triple> + 'a {
        let g = &c.graph;
        (self.added_from..g.len())
            .filter(|&r| g.alive(r))
            .map(|r| g.triple(r))
    }
}

fn cancelled(limits: &Limits) -> bool {
    limits
        .cancel
        .as_ref()
        .is_some_and(|c| c.load(Ordering::Relaxed))
}

/// Bring `c` from the closure of the old explicit facts to that of the new ones:
/// `removed` explicit facts are no longer explicit, `inserted` ones are. The base tier of
/// `c` holds the new explicit facts already: the inserted ones and not the removed ones,
/// except for explicit facts the store does not hold, such as default geometries, which
/// the derived tier holds flagged.
///
/// Returns [`Fallback`] (as the error) when the rules read RDF lists and a list fact is
/// derived, removed or added: what list builtins see then depends on the order of
/// derivation. The closure is unusable after any error.
pub(crate) fn update(
    c: &mut Closure,
    rules: &[CRule],
    limits: &Limits,
    inserted: &[Triple],
    removed: &[Triple],
) -> anyhow::Result<Update> {
    let lists = rules.iter().any(engine::reads_lists);
    let (first, rest) = (c.terms.rdf_first, c.terms.rdf_rest);
    let is_list = |t: &Triple| t[1] == first || t[1] == rest;
    if lists {
        if inserted.iter().chain(removed).any(is_list) {
            return Err(Fallback("an RDF list changed and the rules read lists".into()).into());
        }
        let g = &c.graph;
        for p in [first, rest] {
            let rows = g.cands(None, Some(p), None, 0, g.len());
            if (0..rows.len())
                .map(|i| rows.get(i))
                .any(|r| g.alive(r) && !c.flagged(r))
            {
                return Err(Fallback("the rules read RDF lists that are derived".into()).into());
            }
        }
    }
    let mut out = Update::default();
    let budget = &limits.budget;
    // the closure, the run's changes and the maintenance state are charged to the run
    // before they are used, also when nothing is inserted
    budget.check()?;
    let mut held = Held::new(budget);

    // the inserted facts the derived tier holds are explicit from the start, as the base
    // tier's copies of them are
    for t in inserted {
        if let Some(r) = c.graph.position(t) {
            c.set_flag_with(r, true, true)?;
        }
    }

    // ---- deletion
    held.try_grow(3 * removed.len() * size_of::<Triple>())?;
    let mut queue: Vec<Triple> = Vec::new();
    // the removed facts that were in the base tier: they join the derived tier as
    // candidates, and stay there when they still follow
    let mut back: Vec<Triple> = Vec::new();
    for t in removed {
        match c.graph.position(t) {
            Some(r) => c.set_flag(r, false),
            None => back.push(*t),
        }
        queue.push(*t);
    }
    c.graph.try_add_batch(back)?;
    if !queue.is_empty() {
        let plans = Plans::new(&mut c.graph, rules)?;
        held.try_grow(plans.bytes)?;
        let mut search = Search::new(budget);
        let abort = AtomicBool::new(false);
        // the facts removed so far, charged as their buffer grows, and the queue
        let mut deleted = Held::new(budget);
        let mut queue_held = Held::new(budget);
        while !queue.is_empty() {
            if cancelled(limits) {
                anyhow::bail!("reasoning cancelled");
            }
            let mut dead: Vec<Triple> = Vec::new();
            let mut step = Held::try_new(
                budget,
                queue.len() * size_of::<Triple>()
                    + table_bytes(table_grown(0, 0, queue.len()), size_of::<Triple>()),
            )?;
            let mut seen: FxHashSet<Triple> =
                FxHashSet::with_capacity_and_hasher(queue.len(), Default::default());
            {
                let cx = Cx {
                    c: &*c,
                    rules,
                    plans: &plans,
                    abort: &abort,
                    cancel: limits.cancel.as_deref(),
                    budget,
                };
                for t in queue.drain(..) {
                    if seen.insert(t) && !search.prove(&cx, t) {
                        dead.push(t);
                    }
                    if let Some(e) = search.err.take() {
                        return Err(e);
                    }
                }
            }
            if dead.is_empty() {
                break;
            }
            let (next, next_held) = consequences(c, rules, &plans, &dead, limits)?;
            queue = next;
            step.absorb(next_held);
            for t in &dead {
                c.graph.kill(t);
            }
            queue.retain(|t| c.graph.position(t).is_some() && !search.proved.contains(t));
            // the new buffer is charged before it is allocated, and the old one is
            // released once it holds the facts
            let (len, cap) = (out.deleted.len(), out.deleted.capacity());
            let need = len + dead.len();
            if need > cap {
                let new = need.max(cap * 2);
                deleted.try_grow(new * size_of::<Triple>())?;
                out.deleted.reserve_exact(new - len);
                deleted.shrink(cap * size_of::<Triple>());
            }
            out.deleted.extend(dead);
            drop(seen);
            // the next queue keeps its charge, the previous one's is released
            queue_held = step.split(queue.capacity() * size_of::<Triple>());
            drop(step);
        }
        drop(queue_held);
        out.checked = search.checked;
        held.absorb(deleted);
        if cancelled(limits) {
            anyhow::bail!("reasoning cancelled");
        }
        if lists && out.deleted.iter().any(is_list) {
            return Err(Fallback("a derived RDF list fact was removed".into()).into());
        }
    }

    // ---- insertion
    let start = c.graph.len();
    out.added_from = start;
    held.try_grow(std::mem::size_of_val(inserted))?;
    let fresh: Vec<Triple> = inserted
        .iter()
        .filter(|t| c.graph.position(t).is_none())
        .copied()
        .collect();
    c.graph.try_add_batch(fresh)?;
    for r in start..c.graph.len() {
        c.set_flag_with(r, true, true)?;
    }
    budget.check()?;
    if c.graph.len() > start {
        let o = engine::run(&mut c.graph, rules, &c.terms, limits, Some(start))?;
        out.iterations = o.iterations;
        if lists && out.added(c).any(|t| is_list(&t) && !c.explicit_held(&t)) {
            return Err(Fallback("an RDF list fact was derived".into()).into());
        }
    }
    // the inserted facts that the base tier holds leave the derived tier, found with one
    // sorted pass over the base tier
    held.try_grow(std::mem::size_of_val(inserted) + inserted.len())?;
    let mut ins: Vec<Triple> = inserted.to_vec();
    ins.sort_unstable();
    ins.dedup();
    let in_base = match c.graph.base() {
        Some(b) => b.contains_sorted(&ins)?,
        None => vec![false; ins.len()],
    };
    for (t, _) in ins.iter().zip(in_base).filter(|x| x.1) {
        if let Some(r) = c.graph.position(t) {
            c.set_flag(r, false);
            c.graph.kill(t);
        }
    }
    drop(ins);
    // the removed facts stay with the update
    out.held = Some(held);
    Ok(out)
}

/// How many facts DRed would delete before deriving the survivors again: every fact with
/// a derivation that uses a removed explicit fact, transitively. For measurements; the
/// closure is unusable afterwards.
pub(crate) fn dred_overdeletion(
    c: &mut Closure,
    rules: &[CRule],
    limits: &Limits,
    removed: &[Triple],
) -> usize {
    // the removed facts of the base tier, as derived facts that are deleted first
    let back: Vec<Triple> = removed
        .iter()
        .filter(|t| c.graph.position(t).is_none())
        .copied()
        .collect();
    c.graph.add_batch(back);
    let plans = Plans::new(&mut c.graph, rules).expect("an unlimited budget");
    let mut gone: FxHashSet<Triple> = FxHashSet::default();
    let mut frontier: Vec<Triple> = removed
        .iter()
        .filter(|t| c.graph.position(t).is_some() && gone.insert(**t))
        .copied()
        .collect();
    while !frontier.is_empty() {
        let (next, _held) =
            consequences(c, rules, &plans, &frontier, limits).expect("an unlimited budget");
        for t in &frontier {
            c.graph.kill(t);
        }
        frontier = next
            .into_iter()
            .filter(|t| c.graph.position(t).is_some() && gone.insert(*t))
            .collect();
    }
    gone.len()
}

// ------------------------------------------------------------------ plans ----

/// A plan that starts with the variables of a rule head bound, and those variables.
type BackPlan = (Vec<Step>, Vec<usize>);

/// The plans an update evaluates with, made once per update.
struct Plans {
    /// per rule and head: the plan with the head's variables bound (those that some atom
    /// binds), and those variables
    back: Vec<Vec<Option<BackPlan>>>,
    /// heads by constant predicate: (rule, head)
    by_pred: FxHashMap<u64, Vec<(usize, usize)>>,
    /// heads with a variable predicate
    any_pred: Vec<(usize, usize)>,
    /// per rule and atom: the plan that starts with that atom
    forward: Vec<Vec<Option<Vec<Step>>>>,
    /// an estimate of the plans' heap
    bytes: usize,
}

impl Plans {
    fn new(g: &mut Graph, rules: &[CRule]) -> Result<Plans, MemoryExceeded> {
        let mut p = Plans {
            back: Vec::with_capacity(rules.len()),
            by_pred: FxHashMap::default(),
            any_pred: Vec::new(),
            forward: Vec::with_capacity(rules.len()),
            bytes: 0,
        };
        let all = |r: &CRule| vec![(0, g.len()); r.atoms.len()];
        for (ri, r) in rules.iter().enumerate() {
            let ranges = all(r);
            let mut in_atoms = vec![false; r.nvars];
            for a in &r.atoms {
                for s in [a.s, a.p, a.o] {
                    if let Slot::Var(v) = s {
                        in_atoms[v] = true;
                    }
                }
            }
            let mut heads = Vec::new();
            for (hi, h) in r.head.iter().enumerate() {
                let CHead::Triple(s, pr, o) = h else {
                    heads.push(None);
                    continue;
                };
                let mut pre: Vec<usize> = [s, pr, o]
                    .into_iter()
                    .filter_map(|x| match x {
                        Slot::Var(v) if in_atoms[*v] => Some(*v),
                        _ => None,
                    })
                    .collect();
                pre.sort_unstable();
                pre.dedup();
                let plan = engine::plan_with(r, &ranges, Some(g), None, &pre);
                if plan.is_some() {
                    match pr {
                        Slot::Const(c) => p.by_pred.entry(*c).or_default().push((ri, hi)),
                        _ => p.any_pred.push((ri, hi)),
                    }
                }
                heads.push(plan.map(|s| (s, pre)));
            }
            p.back.push(heads);
            p.forward.push(
                (0..r.atoms.len())
                    .map(|d| engine::plan_with(r, &ranges, Some(g), Some(d), &[]))
                    .collect(),
            );
        }
        // the indexes the plans use
        let (mut need_s, mut need_o) = (false, false);
        for (ri, r) in rules.iter().enumerate() {
            let plans = p.back[ri]
                .iter()
                .flatten()
                .map(|(s, pre)| (s, pre.as_slice()))
                .chain(p.forward[ri].iter().flatten().map(|s| (s, &[][..])));
            for (steps, pre) in plans {
                let (s, o) = engine::index_needs_with(r, steps, pre);
                need_s |= s;
                need_o |= o;
            }
        }
        // the plans' own heap: their steps and bound variables, a small part of a run
        p.bytes = p
            .back
            .iter()
            .flatten()
            .flatten()
            .map(|(s, pre)| s.len() * size_of::<Step>() + pre.len() * 8 + 64)
            .chain(
                p.forward
                    .iter()
                    .flatten()
                    .flatten()
                    .map(|s| s.len() * size_of::<Step>() + 32),
            )
            .sum::<usize>()
            + table_bytes(p.by_pred.capacity(), 32);
        if need_s {
            g.ensure_index(true, true)?;
        }
        if need_o {
            g.ensure_index(false, true)?;
        }
        Ok(p)
    }

    fn heads(&self, p: u64) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.by_pred
            .get(&p)
            .into_iter()
            .flatten()
            .chain(&self.any_pred)
            .copied()
    }
}

/// The heads (in the closure) of the rule instances with a body atom in `dead`, whose
/// rows are still live.
fn consequences(
    c: &Closure,
    rules: &[CRule],
    plans: &Plans,
    dead: &[Triple],
    limits: &Limits,
) -> anyhow::Result<(Vec<Triple>, Held)> {
    let g = &c.graph;
    let rows: Vec<u32> = dead.iter().filter_map(|t| g.position(t)).collect();
    let mut work: Vec<(usize, usize, Vec<u32>)> = Vec::new();
    for (ri, r) in rules.iter().enumerate() {
        for (d, a) in r.atoms.iter().enumerate() {
            if plans.forward[ri][d].is_none() {
                continue;
            }
            let fits = |x: Slot, v: u64| !matches!(x, Slot::Const(k) if k != v);
            let mine: Vec<u32> = rows
                .iter()
                .copied()
                .filter(|&row| {
                    let t = g.triple(row);
                    fits(a.s, t[0]) && fits(a.p, t[1]) && fits(a.o, t[2])
                })
                .collect();
            for ch in mine.chunks(256) {
                work.push((ri, d, ch.to_vec()));
            }
        }
    }
    let abort = AtomicBool::new(false);
    let mut held = Held::try_new(
        &limits.budget,
        work.iter()
            .map(|w| w.2.len() * 4 + size_of::<(usize, usize, Vec<u32>)>())
            .sum(),
    )?;
    type Heads = (Vec<Triple>, Held, Option<anyhow::Error>);
    let heads: Vec<Heads> = work
        .par_iter()
        .map(|(ri, d, rows)| {
            let r = &rules[*ri];
            let steps = plans.forward[*ri][*d].as_ref().unwrap();
            let mut ev = Eval::new(
                g,
                &c.terms,
                r,
                steps,
                g.len(),
                &abort,
                limits.cancel.as_deref(),
                usize::MAX,
                Mode::Existing,
                &limits.budget,
            );
            let mut b = vec![0u64; r.nvars];
            ev.run(0, &mut b, Some(First::Rows(Cands::Slice(rows))));
            let (out, h) = ev.take_out();
            (out, h, ev.err.take())
        })
        .collect();
    let n: usize = heads.iter().map(|h| h.0.len()).sum();
    // the heads, deduplicated
    held.try_grow(
        n * size_of::<Triple>() + table_bytes(table_grown(0, 0, n), size_of::<Triple>()),
    )?;
    let mut seen: FxHashSet<Triple> = FxHashSet::with_capacity_and_hasher(n, Default::default());
    let mut out = Vec::with_capacity(n);
    for (h, hh, err) in heads {
        if let Some(e) = err {
            return Err(e);
        }
        out.extend(h.into_iter().filter(|t| seen.insert(*t)));
        drop(hh);
    }
    drop(seen);
    let keep = held.split(out.capacity() * size_of::<Triple>());
    Ok((out, keep))
}

// ------------------------------------------------------------------ proofs ----

/// How deep the search recurses before it queues the facts it reaches instead.
const MAX_DEPTH: u32 = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// reached, rule instances not explored yet
    Fresh,
    /// its instances are being explored
    Open,
    /// every instance explored, no proof found (yet)
    Explored,
    Proved,
}

struct Node {
    t: Triple,
    state: State,
    /// the instances with this fact in the body
    uses: Vec<u32>,
}

struct Inst {
    head: u32,
    /// body facts not proved yet
    open: u32,
}

/// What a search reads.
struct Cx<'a> {
    c: &'a Closure,
    rules: &'a [CRule],
    plans: &'a Plans,
    abort: &'a AtomicBool,
    cancel: Option<&'a AtomicBool>,
    budget: &'a Arc<Budget>,
}

/// Provability of facts among the live ones: what is known across the searches of one
/// update, and the state of the current search. Its buffers are charged to the run's
/// budget before they grow, and the first refusal stops the search.
struct Search {
    proved: FxHashSet<Triple>,
    unprovable: FxHashSet<Triple>,
    checked: u64,
    nodes: Vec<Node>,
    node_of: FxHashMap<Triple, u32>,
    insts: Vec<Inst>,
    queue: Vec<u32>,
    /// the buffers above and the nodes' lists of uses
    held: Held,
    /// of `held`: the nodes' lists of uses, freed when a search starts
    uses: usize,
    /// the first refusal, or the first error of an evaluation
    err: Option<anyhow::Error>,
}

/// The bytes of the allocation a vector of capacity `cap` with `len` elements of `elem`
/// bytes grows to when `add` more do not fit, or 0 when they fit (the standard
/// library's doubling rule).
fn vec_grown(cap: usize, len: usize, add: usize, elem: usize) -> usize {
    let want = len.saturating_add(add);
    if want <= cap {
        return 0;
    }
    let min = if elem == 1 {
        8
    } else if elem <= 1024 {
        4
    } else {
        1
    };
    want.max(cap.saturating_mul(2))
        .max(min)
        .saturating_mul(elem)
}

impl Search {
    fn new(budget: &Arc<Budget>) -> Search {
        Search {
            proved: FxHashSet::default(),
            unprovable: FxHashSet::default(),
            checked: 0,
            nodes: Vec::new(),
            node_of: FxHashMap::default(),
            insts: Vec::new(),
            queue: Vec::new(),
            held: Held::new(budget),
            uses: 0,
            err: None,
        }
    }

    /// Charge `n` bytes; false when they do not fit, and the search stops.
    fn charge(&mut self, n: usize) -> bool {
        if self.err.is_some() {
            return false;
        }
        match self.held.try_grow(n) {
            Ok(()) => true,
            Err(e) => {
                self.err = Some(e.into());
                false
            }
        }
    }

    /// The new allocation (charged) and the old one (to release once the elements are
    /// in) of a vector growing for `add` more elements: `Some((0, 0))` when they fit,
    /// `None` when the growth is refused.
    fn vec_growth(&mut self, cap: usize, len: usize, add: usize, elem: usize) -> Option<usize> {
        match vec_grown(cap, len, add, elem) {
            0 => Some(0),
            n if self.charge(n) => Some(cap * elem),
            _ => None,
        }
    }

    /// The same for a hash table with elements of `elem` bytes.
    fn table_growth(&mut self, cap: usize, len: usize, add: usize, elem: usize) -> Option<usize> {
        let to = table_grown(cap, len, add);
        if to == cap {
            return Some(0);
        }
        self.charge(table_bytes(to, elem))
            .then(|| table_bytes(cap, elem))
    }

    /// Whether a live fact follows from the explicit facts. A refusal leaves the answer
    /// false with `err` set, and the caller stops.
    fn prove(&mut self, cx: &Cx<'_>, t: Triple) -> bool {
        if self.err.is_some() {
            return false;
        }
        if self.proved.contains(&t) {
            return true;
        }
        if self.unprovable.contains(&t) {
            return false;
        }
        let Some(row) = cx.c.graph.position(&t) else {
            return false;
        };
        let fact = size_of::<Triple>();
        if cx.c.flagged(row) {
            let (cap, len) = (self.proved.capacity(), self.proved.len());
            let Some(old) = self.table_growth(cap, len, 1, fact) else {
                return false;
            };
            self.proved.insert(t);
            self.held.shrink(old);
            return true;
        }
        self.checked += 1;
        self.nodes.clear();
        self.node_of.clear();
        self.insts.clear();
        self.queue.clear();
        self.held.shrink(std::mem::take(&mut self.uses));
        let Some(root) = self.node(t) else {
            return false;
        };
        self.expand(cx, root, root, 0);
        while self.err.is_none()
            && self.nodes[root as usize].state != State::Proved
            && let Some(n) = self.queue.pop()
        {
            if self.nodes[n as usize].state == State::Fresh {
                self.expand(cx, n, root, 0);
            }
        }
        if self.err.is_some() {
            return false;
        }
        let proved = self.nodes[root as usize].state == State::Proved;
        // what the search learned, charged before it is kept
        let gained = self
            .nodes
            .iter()
            .filter(|n| n.state == State::Proved)
            .count();
        let lost = if proved { 0 } else { self.nodes.len() - gained };
        let (pc, pl) = (self.proved.capacity(), self.proved.len());
        let (uc, ul) = (self.unprovable.capacity(), self.unprovable.len());
        let (pt, ut) = (table_grown(pc, pl, gained), table_grown(uc, ul, lost));
        let new = if pt == pc { 0 } else { table_bytes(pt, fact) }
            + if ut == uc { 0 } else { table_bytes(ut, fact) };
        let old = if pt == pc { 0 } else { table_bytes(pc, fact) }
            + if ut == uc { 0 } else { table_bytes(uc, fact) };
        if !self.charge(new) {
            return false;
        }
        self.proved.reserve(gained);
        self.unprovable.reserve(lost);
        self.held.shrink(old);
        for n in &self.nodes {
            match n.state {
                State::Proved => {
                    self.proved.insert(n.t);
                }
                // with no proof of the root, the search explored everything it reached
                _ if !proved => {
                    self.unprovable.insert(n.t);
                }
                _ => {}
            }
        }
        proved
    }

    /// The node of `t`, made (and queued) when it is new; `None` when its room is
    /// refused.
    fn node(&mut self, t: Triple) -> Option<u32> {
        if let Some(&n) = self.node_of.get(&t) {
            return Some(n);
        }
        let n = self.nodes.len() as u32;
        let (nc, nl) = (self.nodes.capacity(), self.nodes.len());
        let a = self.vec_growth(nc, nl, 1, size_of::<Node>())?;
        let (qc, ql) = (self.queue.capacity(), self.queue.len());
        let b = self.vec_growth(qc, ql, 1, 4)?;
        let (mc, ml) = (self.node_of.capacity(), self.node_of.len());
        let c = self.table_growth(mc, ml, 1, size_of::<(Triple, u32)>())?;
        self.nodes.push(Node {
            t,
            state: State::Fresh,
            uses: Vec::new(),
        });
        self.node_of.insert(t, n);
        self.queue.push(n);
        self.held.shrink(a + b + c);
        Some(n)
    }

    /// Record that instance `inst` uses node `m`; false when the room is refused.
    fn used_by(&mut self, m: u32, inst: u32) -> bool {
        let u = &self.nodes[m as usize].uses;
        let (cap, len) = (u.capacity(), u.len());
        let grown = vec_grown(cap, len, 1, 4);
        if grown > 0 {
            if !self.charge(grown) {
                return false;
            }
            self.held.shrink(cap * 4);
            self.uses += grown - cap * 4;
        }
        self.nodes[m as usize].uses.push(inst);
        true
    }

    /// Room for one more instance; false when it is refused.
    fn room_for_instance(&mut self) -> bool {
        let (cap, len) = (self.insts.capacity(), self.insts.len());
        match self.vec_growth(cap, len, 1, size_of::<Inst>()) {
            Some(old) => {
                // the push follows at once
                self.held.shrink(old);
                true
            }
            None => false,
        }
    }

    fn mark_proved(&mut self, n: u32) {
        let mut stack = vec![n];
        while let Some(x) = stack.pop() {
            let node = &mut self.nodes[x as usize];
            if node.state == State::Proved {
                continue;
            }
            node.state = State::Proved;
            for i in std::mem::take(&mut node.uses) {
                let inst = &mut self.insts[i as usize];
                inst.open -= 1;
                if inst.open == 0 {
                    stack.push(inst.head);
                }
            }
        }
    }

    fn done(&self, n: u32, root: u32) -> bool {
        self.nodes[n as usize].state == State::Proved
            || self.nodes[root as usize].state == State::Proved
    }

    /// Explore the rule instances that derive node `n`, depth first.
    fn expand(&mut self, cx: &Cx<'_>, n: u32, root: u32, depth: u32) {
        self.nodes[n as usize].state = State::Open;
        let t = self.nodes[n as usize].t;
        let g = &cx.c.graph;
        for (ri, hi) in cx.plans.heads(t[1]) {
            let Some((steps, pre)) = &cx.plans.back[ri][hi] else {
                continue;
            };
            let rule = &cx.rules[ri];
            let Some(mut b) = bind_head(rule, hi, pre, t) else {
                continue;
            };
            let failed = {
                let mut sink = |body: &[Triple], implicit: &[Triple]| -> bool {
                    self.instance(cx, n, root, depth, body, implicit)
                };
                let mut ev = Eval::new(
                    g,
                    &cx.c.terms,
                    rule,
                    steps,
                    g.len(),
                    cx.abort,
                    cx.cancel,
                    usize::MAX,
                    Mode::Instances {
                        head: hi,
                        target: t,
                        sink: &mut sink,
                    },
                    cx.budget,
                );
                ev.run(0, &mut b, None);
                ev.err.take()
            };
            if let Some(e) = failed {
                self.err.get_or_insert(e);
            }
            if self.err.is_some() {
                break;
            }
            if self.done(n, root) || cx.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                break;
            }
        }
        let node = &mut self.nodes[n as usize];
        if node.state == State::Open {
            node.state = State::Explored;
        }
    }

    /// One instance deriving node `n`; returns false to stop exploring `n`.
    fn instance(
        &mut self,
        cx: &Cx<'_>,
        n: u32,
        root: u32,
        depth: u32,
        matched: &[Triple],
        implicit: &[Triple],
    ) -> bool {
        if self.err.is_some() {
            return false;
        }
        let t = self.nodes[n as usize].t;
        let body = matched.iter().chain(implicit).copied();
        let mut facts: Vec<Triple> = Vec::with_capacity(matched.len() + implicit.len());
        for u in body {
            // an instance that uses its own head or an unprovable fact proves nothing
            if u == t || self.unprovable.contains(&u) {
                return true;
            }
            // a body fact the derived tier does not hold is from the base tier
            if self.proved.contains(&u) || cx.c.explicit_held(&u) {
                continue;
            }
            facts.push(u);
        }
        if !self.room_for_instance() {
            return false;
        }
        let inst = self.insts.len() as u32;
        self.insts.push(Inst { head: n, open: 0 });
        let mut open = 0;
        let mut fresh: Vec<u32> = Vec::new();
        for u in facts {
            let known = self.node_of.contains_key(&u);
            let Some(m) = self.node(u) else {
                return false;
            };
            if self.nodes[m as usize].state == State::Proved {
                continue;
            }
            if !known {
                fresh.push(m);
            }
            open += 1;
            if !self.used_by(m, inst) {
                return false;
            }
        }
        self.insts[inst as usize].open = open;
        if open == 0 {
            self.mark_proved(n);
            return false;
        }
        if depth < MAX_DEPTH {
            for m in fresh {
                if self.nodes[m as usize].state == State::Fresh {
                    self.expand(cx, m, root, depth + 1);
                }
                if self.done(n, root) {
                    return false;
                }
            }
        }
        true
    }
}

/// Bindings of rule `ri`'s head `hi` unified with `t` (only the variables in `pre`), or
/// `None` when a constant differs.
fn bind_head(rule: &CRule, hi: usize, pre: &[usize], t: Triple) -> Option<Vec<u64>> {
    let CHead::Triple(s, p, o) = &rule.head[hi] else {
        return None;
    };
    let mut b = vec![0u64; rule.nvars];
    for (slot, v) in [(s, t[0]), (p, t[1]), (o, t[2])] {
        match *slot {
            Slot::Const(c) if c != v => return None,
            Slot::Var(x) if pre.contains(&x) => {
                if b[x] != 0 && b[x] != v {
                    return None;
                }
                b[x] = v;
            }
            _ => {}
        }
    }
    Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::store::{Snapshot, Store, StoreOptions};
    use std::sync::Arc;

    const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
    const OWL: &str = "http://www.w3.org/2002/07/owl#";
    const EX: &str = "http://ex.org/";

    /// xorshift64*
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn pick<T: Copy>(&mut self, v: &[T]) -> T {
            v[self.below(v.len())]
        }
    }

    struct Vocab {
        /// the vocabulary in a named graph, and the explicit facts of a test in the
        /// default graph
        store: Store,
        snap: Arc<Snapshot>,
        classes: Vec<u64>,
        props: Vec<u64>,
        inds: Vec<u64>,
        lits: Vec<u64>,
        list_nodes: Vec<u64>,
        iri: FxHashMap<String, u64>,
    }

    impl Vocab {
        fn new() -> Vocab {
            let mut ttl = String::new();
            let mut names = Vec::new();
            for i in 0..6 {
                names.push(format!("{EX}C{i}"));
            }
            for i in 0..5 {
                names.push(format!("{EX}p{i}"));
            }
            for i in 0..8 {
                names.push(format!("{EX}i{i}"));
            }
            for i in 0..3 {
                names.push(format!("{EX}l{i}"));
            }
            for l in ["type", "first", "rest", "nil", "Property", "List"] {
                names.push(format!("{RDF}{l}"));
            }
            for l in [
                "subClassOf",
                "subPropertyOf",
                "domain",
                "range",
                "Class",
                "Resource",
                "Literal",
            ] {
                names.push(format!("{RDFS}{l}"));
            }
            for l in [
                "sameAs",
                "inverseOf",
                "TransitiveProperty",
                "SymmetricProperty",
                "FunctionalProperty",
                "InverseFunctionalProperty",
                "equivalentClass",
                "equivalentProperty",
                "intersectionOf",
                "unionOf",
                "hasValue",
                "onProperty",
                "someValuesFrom",
                "allValuesFrom",
                "Thing",
                "Class",
                "Restriction",
            ] {
                names.push(format!("{OWL}{l}"));
            }
            for n in &names {
                ttl.push_str(&format!("<urn:v> <urn:has> <{n}> <urn:vocab> .\n"));
            }
            for l in [
                "\"a\"",
                "\"b\"",
                "\"7\"^^<http://www.w3.org/2001/XMLSchema#integer>",
            ] {
                ttl.push_str(&format!("<urn:v> <urn:has> {l} <urn:vocab> .\n"));
            }
            let store = Store::in_memory(StoreOptions::default());
            store
                .load(&[Source::from_bytes(
                    ttl.into_bytes(),
                    RdfFormat::NQuads,
                    None,
                )])
                .unwrap();
            let snap = store.snapshot();
            let iri: FxHashMap<String, u64> = names
                .iter()
                .map(|n| (n.clone(), snap.lookup_iri(n).unwrap().0))
                .collect();
            let get = |p: &str| -> Vec<u64> {
                let mut v: Vec<(String, u64)> = iri
                    .iter()
                    .filter(|(k, _)| k.starts_with(&format!("{EX}{p}")))
                    .map(|(k, v)| (k.clone(), *v))
                    .collect();
                v.sort();
                v.into_iter().map(|x| x.1).collect()
            };
            let terms = Terms::new(snap.clone());
            let lits = ["\"a\"", "\"b\""]
                .iter()
                .map(|l| {
                    terms.id_for(&oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
                        &l[1..2],
                    )))
                })
                .chain([sparkles_core::id::Id::from_i64(7).unwrap().0])
                .collect();
            Vocab {
                store,
                classes: get("C"),
                props: get("p"),
                inds: get("i"),
                list_nodes: get("l"),
                lits,
                snap,
                iri,
            }
        }

        fn id(&self, ns: &str, l: &str) -> u64 {
            self.iri[&format!("{ns}{l}")]
        }

        /// A random triple: schema statements, typing, property assertions.
        fn triple(&self, r: &mut Rng, owl: bool) -> Triple {
            let rdf = |l| self.id(RDF, l);
            let rdfs = |l| self.id(RDFS, l);
            let o = |l| self.id(OWL, l);
            let (c, p, i) = (&self.classes, &self.props, &self.inds);
            let k = r.below(if owl { 20 } else { 10 });
            match k {
                0 => [r.pick(c), rdfs("subClassOf"), r.pick(c)],
                1 => [r.pick(p), rdfs("subPropertyOf"), r.pick(p)],
                2 => [r.pick(p), rdfs("domain"), r.pick(c)],
                3 => [r.pick(p), rdfs("range"), r.pick(c)],
                4 | 5 => [r.pick(i), rdf("type"), r.pick(c)],
                6..=8 => [r.pick(i), r.pick(p), r.pick(i)],
                9 => [r.pick(i), r.pick(p), r.pick(&self.lits)],
                10 => [r.pick(i), o("sameAs"), r.pick(i)],
                11 => [r.pick(p), o("inverseOf"), r.pick(p)],
                12 => [
                    r.pick(p),
                    rdf("type"),
                    r.pick(&[
                        o("TransitiveProperty"),
                        o("SymmetricProperty"),
                        o("FunctionalProperty"),
                        o("InverseFunctionalProperty"),
                    ]),
                ],
                13 => [r.pick(c), o("equivalentClass"), r.pick(c)],
                14 => [r.pick(c), o("hasValue"), r.pick(i)],
                15 => [r.pick(c), o("onProperty"), r.pick(p)],
                16 => [r.pick(c), o("someValuesFrom"), r.pick(c)],
                17 => [r.pick(c), o("allValuesFrom"), r.pick(c)],
                18 => [r.pick(p), o("equivalentProperty"), r.pick(p)],
                _ => [r.pick(c), rdf("type"), o("Class")],
            }
        }

        /// The triples of a two-member list for `owl:intersectionOf` or `owl:unionOf`.
        fn list(&self, r: &mut Rng) -> Vec<Triple> {
            let (first, rest, nil) = (
                self.id(RDF, "first"),
                self.id(RDF, "rest"),
                self.id(RDF, "nil"),
            );
            let (l0, l1) = (r.pick(&self.list_nodes), r.pick(&self.list_nodes));
            if l0 == l1 {
                return Vec::new();
            }
            let op = if r.below(2) == 0 {
                "intersectionOf"
            } else {
                "unionOf"
            };
            vec![
                [r.pick(&self.classes), self.id(OWL, op), l0],
                [l0, first, r.pick(&self.classes)],
                [l0, rest, l1],
                [l1, first, r.pick(&self.classes)],
                [l1, rest, nil],
            ]
        }
    }

    fn limits() -> Limits {
        Limits {
            max_iterations: 10_000,
            max_inferred: 10_000_000,
            budget: Budget::unlimited(),
            cancel: None,
            progress: None,
        }
    }

    fn compiled(text: &str, terms: &Terms) -> Vec<CRule> {
        let rules = crate::parse_rules(text).unwrap();
        let mut w = Vec::new();
        let c = engine::compile(&rules, terms, &mut w);
        assert!(
            c.iter()
                .all(|r| engine::incremental_blocker(r, terms).is_none())
        );
        c
    }

    /// Make the default graph hold exactly `base`, and return its snapshot.
    fn set_default_graph(v: &Vocab, base: &FxHashSet<Triple>) -> Arc<Snapshot> {
        let now = v.store.snapshot();
        let g = Id::DEFAULT_GRAPH;
        let mut txn = v.store.write();
        let have: Vec<[u64; 4]> = now
            .scan_keys(sparkles_core::index::Perm::Gspo, &[g.0])
            .unwrap()
            .iter()
            .map(|k| [k[1], k[2], k[3], k[0]])
            .collect();
        for q in &have {
            if !base.contains(&[q[0], q[1], q[2]]) {
                txn.delete([Id(q[0]), Id(q[1]), Id(q[2]), g]).unwrap();
            }
        }
        for t in base {
            assert!(
                t.iter()
                    .all(|&x| Id(x).tag() != sparkles_core::id::Tag::Local)
            );
            txn.insert([Id(t[0]), Id(t[1]), Id(t[2]), g]).unwrap();
        }
        txn.commit().unwrap();
        let snap = v.store.snapshot();
        assert!(Arc::ptr_eq(&snap.generation, &v.snap.generation));
        snap
    }

    fn full(v: &Vocab, text: &str, base: &FxHashSet<Triple>) -> Closure {
        let snap = set_default_graph(v, base);
        let terms = Terms::new(snap.clone());
        let rules = compiled(text, &terms);
        let b = Base::new(snap, &[Id::DEFAULT_GRAPH]).unwrap();
        let mut g = Graph::with_base(Arc::new(b));
        engine::run(&mut g, &rules, &terms, &limits(), None).unwrap();
        Closure::new(g, terms)
    }

    /// The facts of both tiers as text, explicit ones marked. A fact both tiers hold
    /// appears twice when the derived tier does not flag it.
    fn render(c: &Closure) -> std::collections::BTreeSet<String> {
        let s = |x: u64| c.terms.term(x).map_or(format!("?{x:x}"), |t| t.to_string());
        let line = |t: Triple, b: bool| {
            format!(
                "{} {} {}{}",
                s(t[0]),
                s(t[1]),
                s(t[2]),
                if b { " ." } else { "" }
            )
        };
        let mut out: std::collections::BTreeSet<String> =
            c.live().map(|(t, b)| line(t, b)).collect();
        c.graph
            .base()
            .unwrap()
            .for_each(None, None, None, |t| {
                out.insert(line(t, true));
                true
            })
            .unwrap();
        out
    }

    fn differential(text: &str, owl: bool, lists: bool, seed: u64, steps: usize) -> (usize, usize) {
        let v = Vocab::new();
        let mut r = Rng(seed);
        let mut base: FxHashSet<Triple> = FxHashSet::default();
        for _ in 0..25 {
            base.insert(v.triple(&mut r, owl));
        }
        let mut c = full(&v, text, &base);
        let (mut incremental, mut fallbacks) = (0, 0);
        for step in 0..steps {
            let mut removed = Vec::new();
            let mut inserted = Vec::new();
            let current: Vec<Triple> = {
                let mut x: Vec<Triple> = base.iter().copied().collect();
                x.sort_unstable();
                x
            };
            for _ in 0..r.below(4) {
                if !current.is_empty() {
                    let t = r.pick(&current);
                    if base.remove(&t) {
                        removed.push(t);
                    }
                }
            }
            for _ in 0..r.below(5) {
                let t = v.triple(&mut r, owl);
                if base.insert(t) {
                    inserted.push(t);
                }
            }
            if lists && r.below(6) == 0 {
                for t in v.list(&mut r) {
                    if base.insert(t) {
                        inserted.push(t);
                    }
                }
            }
            // the closure reads the input graphs of the new state, as an update does
            let snap = set_default_graph(&v, &base);
            let moved = c.terms.rebase(snap.clone());
            c.remap(&moved);
            c.set_inputs(snap, &[Id::DEFAULT_GRAPH]).unwrap();
            let rules = compiled(text, &c.terms);
            match update(&mut c, &rules, &limits(), &inserted, &removed) {
                Ok(_) => incremental += 1,
                Err(e) => {
                    assert!(e.downcast_ref::<Fallback>().is_some(), "{e:#}");
                    fallbacks += 1;
                    c = full(&v, text, &base);
                    continue;
                }
            }
            let want = full(&v, text, &base);
            let (a, b) = (render(&c), render(&want));
            if a != b {
                let extra: Vec<_> = a.difference(&b).collect();
                let missing: Vec<_> = b.difference(&a).collect();
                panic!(
                    "seed {seed} step {step}: removed {removed:?} inserted {inserted:?}\nextra {extra:#?}\nmissing {missing:#?}"
                );
            }
            c.compact_if_needed(false).unwrap();
        }
        (incremental, fallbacks)
    }

    #[test]
    fn rdfs_matches_full() {
        for seed in 1..25 {
            differential(crate::RDFS_RULES, false, false, seed, 25);
        }
    }

    #[test]
    fn rdfs_simple_matches_full() {
        for seed in 1..25 {
            differential(crate::RDFS_SIMPLE_RULES, false, false, seed, 25);
        }
    }

    #[test]
    fn owl_rl_matches_full() {
        let mut n = (0, 0);
        for seed in 1..25 {
            let (i, f) = differential(crate::OWL_RL_RULES, true, true, seed, 25);
            n = (n.0 + i, n.1 + f);
        }
        assert!(n.0 > n.1, "{n:?}");
    }

    #[test]
    fn jena_rules_match_full() {
        let rules = "@prefix ex: <http://ex.org/>.
            [chain: (?a ex:p0 ?b), (?b ex:p0 ?c), notEqual(?a, ?c) -> (?a ex:p1 ?c)]
            [lit: (?a ex:p2 ?v), isLiteral(?v), strConcat(?v, '!', ?w) -> (?a ex:p3 ?w)]
            [back: (?a ex:p3 ?w), regex(?w, '(.*)!', ?x) -> (?a ex:p4 ?x)]
            [num: (?a ex:p4 ?v), lessThan(?v, 10) -> (?a rdf:type ex:C0)]
            [sym: (?a ex:p1 ?b) -> (?b ex:p1 ?a)]
            [ty: (?a rdf:type ex:C0), (?a ?p ?b), notEqual(?p, rdf:type) -> (?b rdf:type ex:C1)]
            [any: (?a ?p ?b), (?p rdf:type owl:TransitiveProperty), (?b ?p ?c) -> (?a ?p ?c)]";
        for seed in 1..25 {
            differential(rules, true, false, seed, 25);
        }
    }
}
