//! Join ordering on cost summaries.
//!
//! [`order`] chooses how to join the triple patterns and other inputs of a group that
//! are connected by shared variables. It runs the planner's dynamic program (QLever's),
//! which keeps the cheapest plan per subset of the inputs and first sort variable, on
//! summaries of plans instead of plan trees. A summary holds a plan's cost, estimated
//! rows and first sort variable, and the distinct-value estimates of the variables that
//! later joins read. The tree is built once, for the chosen joins only.
//!
//! Three rules keep the program small. A greedy plan comes first, and its cost bounds
//! the search. A join costs at least as much as each of its inputs, except a pattern
//! that an index join probes, so a plan of several inputs that costs more than the
//! greedy plan cannot be part of a cheaper complete plan, and is dropped. When no
//! complete plan costs at most as much as the greedy plan, the greedy plan is taken.
//! Second, a plan whose sort variable no later join or filter reads is dropped when
//! another plan of the same inputs has the same estimated rows and distinct-value
//! estimates at no more cost. Every plan made from it has a counterpart made from the
//! other that costs no more, but that counterpart can lose its place to a cheaper plan
//! with more rows, which a filter later makes dearer. This rule alone can therefore
//! change the result, rarely and by little. Third, subsets whose inputs are not
//! connected by shared variables have no plan and are skipped.
//!
//! A group whose connected subsets have more splits than the program enumerates in
//! about a millisecond is planned in rounds. Each round plans the subsets up to the
//! largest size that fits, and the cheapest plan of that size joins its inputs into one
//! for the next round. The result of the rounds can cost more than the exhaustive
//! program's, but never more than the greedy plan. A group of more than 16 inputs, or
//! one whose inputs share more than 128 variables, keeps the greedy plan.

use super::ctx::Ctx;
use super::expr::Expr;
use super::indexjoin::{self, ProbeSide};
use super::keyprobe::Side;
use super::plan::{self, JoinModel, Node, Planner};
use super::table::VarId;
use crate::access::{PreparationText, Reservation};
use crate::error::{Error, Result};
use rustc_hash::FxHashMap;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// Groups with more inputs keep the greedy plan.
const DP_MAX_ITEMS: usize = 16;

/// The splits a round of the program enumerates at most. Every group of up to ten
/// inputs fits (a star of ten patterns, where every subset is connected, has about
/// 28,500), and so do chains of 16.
const DP_MAX_SPLITS: u64 = 1 << 15;

/// The splits a round enumerates at most when the group is planned in rounds.
const ROUND_SPLITS: u64 = 1 << 13;

/// The join variables the program tracks per subset fit in one `u128`.
const DP_MAX_JOIN_VARS: usize = 128;

/// A plan's key and the keys a fused star would probe for, when it is an index join of
/// star patterns (see [`indexjoin::chain_keys`]).
type Chain = Option<(VarId, f64)>;

/// What the join ordering knows about a plan.
#[derive(Clone, Copy, Debug)]
struct Sum {
    cost: f64,
    est: f64,
    /// the cost of sorting it
    sc: f64,
    /// the first sort variable
    sorted: Option<VarId>,
    chain: Chain,
}

impl Sum {
    fn new(cost: f64, est: f64, sorted: Option<VarId>) -> Sum {
        Sum {
            cost,
            est,
            sc: plan::sort_cost(est),
            sorted,
            chain: None,
        }
    }

    fn of(n: &Node) -> Sum {
        Sum {
            chain: indexjoin::chain_of(n),
            ..Sum::new(n.cost, n.est, n.sorted.first().copied())
        }
    }
}

/// How two plans are joined.
#[derive(Clone, Copy, Debug, PartialEq)]
enum How {
    /// merge join on the variable, sorting either input that is not sorted on it
    Merge(VarId),
    Hash,
    /// the left input's keys probe the right input's pattern (an index join)
    ProbeRight,
    /// the right input's keys probe the left input's pattern
    ProbeLeft,
}

/// A variable that the two inputs of a join share, with its distinct-value estimate on
/// each side and whether each side always binds it.
#[derive(Clone, Copy)]
struct Shared {
    v: VarId,
    /// its join variable index
    j: u32,
    da: f64,
    db: f64,
    /// each side's domain of the variable (see [`plan::domain`]), 0 when none
    doma: f64,
    domb: f64,
    ca: bool,
    cb: bool,
}

/// One way of joining two plans: its cost and first sort variable.
#[derive(Clone, Copy)]
struct Cand {
    cost: f64,
    sorted: Option<VarId>,
    how: How,
    chain: Chain,
}

/// The estimated rows of joining `a` and `b`, the same for every way of joining them:
/// as `model` tells, else from the distinct values, with several keys combined as
/// [`plan::keys_denom`] does when `backoff`.
fn pair_est(
    a: &Sum,
    b: &Sum,
    shared: &[Shared],
    model: Option<JoinModel>,
    backoff: bool,
    capped: bool,
) -> f64 {
    if let Some(m) = model {
        return m.est(a.est, b.est);
    }
    let cap = capped.then_some((a.est, b.est));
    let denom = plan::keys_denom(
        shared.iter().map(|s| (s.da.max(s.doma), s.db.max(s.domb))),
        backoff,
        cap,
    );
    plan::join_est_from(a.est, b.est, denom)
}

/// A bound below the cost of every way of joining `a` and `b` into `est` rows, given
/// which of them an index join can probe: each way costs at least its input or inputs,
/// their rows and the output rows.
fn pair_floor(a: &Sum, probe_a: bool, b: &Sum, probe_b: bool, est: f64) -> f64 {
    let mut floor = a.cost + b.cost + a.est + b.est + est;
    // an index join costs at least what it would if probing were free
    if probe_b {
        floor = floor.min(indexjoin::join_cost(a.cost, a.est, 0.0, est));
    }
    if probe_a {
        floor = floor.min(indexjoin::join_cost(b.cost, b.est, 0.0, est));
    }
    // sums in another order may round lower
    floor * (1.0 - 1e-9)
}

/// The ways of joining `a` and `b` into `est` rows, in the order the planner offers
/// them: merge joins on each shared variable both always bind (in `shared` order), the
/// hash join, then the index joins with `a` and with `b` as the input. `pa` and `pb`
/// describe the sides that are patterns an index join can probe. An index join is
/// costed only when `keep` accepts a bound below its cost and its sort variable.
#[allow(clippy::too_many_arguments)]
fn joins(
    a: &Sum,
    pa: Option<&ProbeSide>,
    b: &Sum,
    pb: Option<&ProbeSide>,
    shared: &[Shared],
    est: f64,
    batched: plan::Costing,
    keep: &mut impl FnMut(f64, Option<VarId>) -> bool,
    out: &mut Vec<Cand>,
) {
    out.clear();
    for s in shared {
        if !(s.ca && s.cb) {
            continue;
        }
        let (sa, sb) = (a.sorted == Some(s.v), b.sorted == Some(s.v));
        let extra = if sa { 0.0 } else { a.sc } + if sb { 0.0 } else { b.sc };
        if plan::merge_worth(extra, a.est, b.est, sa && sb) {
            let x = if sa { a.cost } else { a.cost + a.est + a.sc };
            let y = if sb { b.cost } else { b.cost + b.est + b.sc };
            out.push(Cand {
                cost: x + y + (a.est + b.est) * plan::merge_row_cost() + est,
                sorted: Some(s.v),
                how: How::Merge(s.v),
                chain: None,
            });
        }
    }
    let base = plan::hash_base(batched.hash_build, a.est, b.est);
    out.push(Cand {
        cost: a.cost + b.cost + base + est,
        sorted: if a.est >= b.est { a.sorted } else { b.sorted },
        how: How::Hash,
        chain: None,
    });
    if batched.index_joins {
        if let Some(p) = pb
            && let Some((cost, sorted, chain)) = probe(a, b, p, shared, est, false, keep)
        {
            out.push(Cand {
                cost,
                sorted,
                how: How::ProbeRight,
                chain,
            });
        }
        if let Some(p) = pa
            && let Some((cost, sorted, chain)) = probe(b, a, p, shared, est, true, keep)
        {
            out.push(Cand {
                cost,
                sorted,
                how: How::ProbeLeft,
                chain,
            });
        }
    }
}

/// The cost, first sort variable and star chain (see [`Sum::chain`]) of the index join
/// of `drive` into the pattern `pat`, if one is offered and `keep` accepts it.
/// `drive_is_b` tells which side of `shared` the input is.
fn probe(
    drive: &Sum,
    pat: &Sum,
    p: &ProbeSide,
    shared: &[Shared],
    est: f64,
    drive_is_b: bool,
    keep: &mut impl FnMut(f64, Option<VarId>) -> bool,
) -> Option<(f64, Option<VarId>, Chain)> {
    let s = shared.iter().find(|s| s.v == p.key)?;
    let (d, certain) = if drive_is_b {
        (s.db, s.cb)
    } else {
        (s.da, s.ca)
    };
    if !certain {
        return None;
    }
    // a shared variable the input is sorted on, other than the key, takes the
    // pattern's values in the output
    let sorted = match drive.sorted {
        Some(v) if v != p.key && shared.iter().any(|s| s.v == v) => None,
        s => s,
    };
    // the cost if probing were free bounds the cost from below
    let floor = indexjoin::join_cost(drive.cost, drive.est, 0.0, est);
    if !keep(floor, sorted) {
        return None;
    }
    // a fused star probes every pattern for the keys of its input
    let keys = match drive.chain {
        Some((k, base)) if p.star && k == p.key => base,
        _ => d.min(drive.est).max(1.0),
    };
    let probe_cost = p.cost(keys, pat.cost)?;
    let cost = indexjoin::join_cost(drive.cost, drive.est, probe_cost, est);
    Some((cost, sorted, p.star.then_some((p.key, keys))))
}

/// The cost and rows of a plan of `cost` and `est` rows under a FILTER of selectivity
/// `sel`, one of whose conjuncts sorts its input when `unsorted` (see [`plan::filter`]).
fn filtered(cost: f64, est: f64, sel: f64, unsorted: bool) -> (f64, f64) {
    let cost = cost + est + if unsorted { est * 0.5 } else { 0.0 };
    (cost, (est * sel).max(if est > 0.0 { 1.0 } else { 0.0 }))
}

/// The variables of `a` that `b` also has, in `a`'s order.
fn shared_vars<'a>(a: &'a Node, b: &'a Node) -> impl Iterator<Item = VarId> + 'a {
    a.vars.iter().filter(|v| b.vars.contains(v)).copied()
}

/// The most that [`build_join`] of `a` and `b` the given way allocates at once, counting
/// what the join keeps and the hash join made instead of an index join no longer offered.
fn join_build_bound(
    layout: &mut super::checked_layout::Layout<'_>,
    a: &Node,
    b: &Node,
    how: How,
    ctx: &Ctx,
) -> Result<()> {
    let keys = shared_vars(a, b).count();
    layout.slots::<VarId>(keys)?;
    match how {
        How::Merge(v) => plan::merge_join_bound(layout, a, b, v, keys, ctx),
        How::Hash => plan::hash_join_bound(layout, a, b, shared_vars(a, b), ctx),
        How::ProbeRight | How::ProbeLeft => {
            let (drive, probe) = match how {
                How::ProbeRight => (a, b),
                _ => (b, a),
            };
            indexjoin::index_join_bound(layout, drive, probe, ctx)?;
            plan::hash_join_bound(layout, a, b, shared_vars(a, b), ctx)
        }
    }
}

/// Keep, of `held`'s credit, what `n` and the sorts that made it (`sorts`, per child)
/// keep, and add it to the branch's plan storage.
fn keep_built(pl: &Planner, mut held: Reservation, n: &Node, sorts: [bool; 2]) -> Result<()> {
    let kept = {
        let mut layout = super::checked_layout::Layout::retained(held.preparation());
        layout.shallow(n)?;
        for (child, sorted) in n.children.iter().zip(sorts) {
            if sorted {
                layout.shallow(child)?;
            }
        }
        layout.bytes()
    };
    debug_assert!(
        kept <= held.bytes(),
        "the plan keeps {kept} bytes of the {} admitted",
        held.bytes()
    );
    if kept <= held.bytes() {
        held.release_to(kept);
    } else {
        held.grow_by(kept - held.bytes())?;
    }
    #[cfg(test)]
    crate::test_alloc::observe_release();
    pl.retain_planning(held);
    Ok(())
}

/// The filter credit that [`place_masked`] hands back for a filtered input copy in
/// [`Dp::run`], dropped after the copy. Unit tests settle its drop with the observer.
#[cfg(test)]
fn leaf_kept(held: Option<Reservation>) -> crate::test_alloc::Settled {
    crate::test_alloc::Settled::new(crate::test_alloc::Origin::handed_up(), held)
}
#[cfg(not(test))]
fn leaf_kept(held: Option<Reservation>) -> Option<Reservation> {
    held
}

/// Join plans `a` and `b` as [`plan::join`] does: as a cross product when they share no
/// variable, or one of them when the other is the one empty solution. On the checked
/// route the join is admitted as [`build_join`] admits one.
pub(super) fn join_admitted(a: Node, b: Node, pl: &Planner) -> Result<Node> {
    #[cfg(test)]
    let _stage = crate::test_alloc::Stage::enter(7);
    let ctx = pl.ctx;
    let mut keys = 0;
    let held = refused(pl.planning_stage(JOIN_BUILD_PHASE).and_then(|()| {
        pl.planning_scratch(|layout| {
            keys = shared_vars(&a, &b).count();
            plan::join_bound(layout, &a, &b)?;
            if keys > 0 {
                // the planner's join candidates, each made from copies of both plans
                for _ in 0..keys + 3 {
                    layout.node(&a)?;
                    layout.node(&b)?;
                }
                for v in shared_vars(&a, &b) {
                    plan::merge_join_bound(layout, &a, &b, v, keys, ctx)?;
                }
                plan::hash_join_bound(layout, &a, &b, shared_vars(&a, &b), ctx)?;
                indexjoin::index_join_bound(layout, &a, &b, ctx)?;
                indexjoin::index_join_bound(layout, &b, &a, ctx)?;
            }
            Ok(())
        })
    }))?;
    let keeps_input = held.is_some() && plan::join_keeps_input(&a, &b);
    let n = plan::join(a, b, ctx);
    if let Some(held) = held {
        if keeps_input {
            drop(held);
        } else {
            keep_built(pl, held, &n, [keys > 0; 2])?;
        }
    }
    Ok(n)
}

/// Join `a` and `b` the given way (the same nodes as the planner's join candidates). On
/// the checked route the most the join allocates is admitted before it is made, and what
/// the plan keeps of it stays with the branch's plan storage.
fn build_join(a: Node, b: Node, how: How, pl: &Planner) -> Result<Node> {
    #[cfg(test)]
    let _stage = crate::test_alloc::Stage::enter(7);
    let ctx = pl.ctx;
    let held =
        refused(pl.planning_stage(JOIN_BUILD_PHASE).and_then(|()| {
            pl.planning_scratch(|layout| join_build_bound(layout, &a, &b, how, ctx))
        }))?;
    let keys: Vec<VarId> = match &held {
        Some(_) => {
            let mut keys = Vec::with_capacity(shared_vars(&a, &b).count());
            keys.extend(shared_vars(&a, &b));
            keys
        }
        None => shared_vars(&a, &b).collect(),
    };
    let sorts = match how {
        How::Merge(v) => [a.sorted.first() != Some(&v), b.sorted.first() != Some(&v)],
        _ => [false; 2],
    };
    let n = match how {
        How::Merge(v) => plan::merge_join(a, b, v, &keys, ctx),
        How::Hash => plan::hash_join(a, b, keys, ctx),
        How::ProbeRight => match indexjoin::index_join(a, &b, ctx) {
            Ok(n) => n,
            Err(a) => {
                debug_assert!(false, "index join not offered when rebuilt");
                plan::hash_join(*a, b, keys, ctx)
            }
        },
        How::ProbeLeft => match indexjoin::index_join(b, &a, ctx) {
            Ok(n) => n,
            Err(b) => {
                debug_assert!(false, "index join not offered when rebuilt");
                plan::hash_join(a, *b, keys, ctx)
            }
        },
    };
    if let Some(held) = held {
        keep_built(pl, held, &n, sorts)?;
    }
    Ok(n)
}

/// What the ordering needs to know about a FILTER conjunct.
struct FilterInfo {
    /// it can be placed: no EXISTS, at least one variable, and every variable bound by
    /// some input
    placeable: bool,
    /// its variables, as local indices
    vars: Vec<u32>,
    /// the variable an expression cache would read, as a local index, if the inputs
    /// bind it
    cached: Option<u32>,
    /// its selectivity measured on a sample (see [`super::sample`])
    sel: Option<f64>,
}

/// The inputs' variables and the filters, indexed for the ordering.
struct Group {
    /// every variable of the inputs (local index → variable)
    vars: Vec<VarId>,
    /// variable → local index
    idx: FxHashMap<VarId, u32>,
    /// local index of a variable → its index among the join variables (those of two or
    /// more inputs), or `u32::MAX`
    join: Vec<u32>,
    /// join variable index → local index
    jvars: Vec<u32>,
    /// per input, its variables as local indices
    item_vars: Vec<Vec<u32>>,
    filters: Vec<FilterInfo>,
    /// how the joins are estimated when not from distinct values
    models: Models,
    /// the checked route's credit for the above, dropped after them
    _held: Option<Reservation>,
}

/// What the inputs of a group hold that estimates a join other than from distinct values.
#[derive(Clone, Default)]
struct Models {
    /// per input, the star masks of its patterns by join variable index (see
    /// [`super::charsets`])
    stars: Vec<Vec<(u32, u64)>>,
    /// per input, how it holds the leaves on each probed join variable (see
    /// [`super::keyprobe`])
    sides: Vec<Vec<(u32, Side)>>,
    /// some input has a star mask or a side
    starred: bool,
    probed: bool,
    /// per input, the join variable indexes, sorted, of each of its inputs that binds two
    /// or more, when several keys back off and two inputs could each bind several keys
    /// (see [`plan::keys_denom`])
    inputs: Vec<Vec<Vec<u32>>>,
}

/// What the inputs of one side of a join hold on a join variable: their star mask and
/// how they hold the leaves on it if it was probed.
#[derive(Clone, Copy, Default)]
struct Held {
    star: u64,
    side: Side,
}

impl Held {
    fn merge(self, o: Held) -> Held {
        Held {
            star: super::charsets::merge(self.star, o.star),
            side: self.side.merge(o.side),
        }
    }
}

impl Models {
    /// Measure the storage of a copy of these models.
    fn measure(&self, layout: &mut super::checked_layout::Layout<'_>) -> Result<()> {
        layout.slice(&self.stars)?;
        for stars in &self.stars {
            layout.slice(stars)?;
        }
        layout.slice(&self.sides)?;
        for sides in &self.sides {
            layout.slice(sides)?;
        }
        layout.slice(&self.inputs)?;
        for inputs in &self.inputs {
            layout.slice(inputs)?;
            for vars in inputs {
                layout.slice(vars)?;
            }
        }
        Ok(())
    }

    /// Whether one input of the inputs `ins` binds every key of `shared`, as
    /// [`plan::one_input_binds`] finds in the plan of them.
    fn one_binds<I: Iterator<Item = usize>>(&self, mut ins: I, shared: &[Shared]) -> bool {
        ins.any(|i| {
            self.inputs.get(i).is_some_and(|xs| {
                xs.iter()
                    .any(|x| shared.iter().all(|s| x.binary_search(&s.j).is_ok()))
            })
        })
    }

    /// What the inputs `ins` hold on join variable `j`.
    fn held<I: Iterator<Item = usize>>(&self, j: u32, ins: I) -> Held {
        let mut h = Held::default();
        for i in ins {
            if self.starred {
                for &(x, s) in &self.stars[i] {
                    if x == j {
                        h.star = super::charsets::merge(h.star, s);
                    }
                }
            }
            if self.probed {
                for &(x, t) in &self.sides[i] {
                    if x == j {
                        h.side = h.side.merge(t);
                    }
                }
            }
        }
        h
    }

    /// What input `i` holds on each join variable, by join variable index.
    fn held_by(&self, i: usize) -> Vec<(u32, Held)> {
        let stars = self.stars.get(i).map_or(&[][..], Vec::as_slice);
        let sides = self.sides.get(i).map_or(&[][..], Vec::as_slice);
        let mut js: Vec<u32> = stars.iter().map(|x| x.0).collect();
        js.extend(sides.iter().map(|x| x.0));
        js.sort_unstable();
        js.dedup();
        js.into_iter()
            .map(|j| (j, self.held(j, std::iter::once(i))))
            .collect()
    }

    /// The most join variables that [`Models::held_by`] lists for input `i`.
    fn held_by_bound(&self, i: usize) -> usize {
        self.stars.get(i).map_or(0, Vec::len) + self.sides.get(i).map_or(0, Vec::len)
    }

    /// [`Models::held_by`] in storage of [`Models::held_by_bound`] variables.
    fn held_by_within(&self, i: usize) -> Vec<(u32, Held)> {
        let stars = self.stars.get(i).map_or(&[][..], Vec::as_slice);
        let sides = self.sides.get(i).map_or(&[][..], Vec::as_slice);
        let mut js: Vec<u32> = Vec::with_capacity(stars.len() + sides.len());
        js.extend(stars.iter().map(|x| x.0));
        js.extend(sides.iter().map(|x| x.0));
        js.sort_unstable();
        js.dedup();
        let mut out = Vec::with_capacity(js.len());
        out.extend(js.iter().map(|&j| (j, self.held(j, std::iter::once(i)))));
        out
    }

    /// How a join on join variable `j` (variable `v`) of the inputs `a` and `b` is
    /// estimated, the same way as [`plan::join_est_with`] estimates it.
    fn of<I: Iterator<Item = usize> + Clone>(
        &self,
        ctx: &Ctx,
        v: VarId,
        j: u32,
        a: I,
        b: I,
    ) -> Option<JoinModel> {
        self.of_held(ctx, v, self.held(j, a), self.held(j, b))
    }

    /// [`Models::of`] for sides that hold `a` and `b` on the variable.
    fn of_held(&self, ctx: &Ctx, v: VarId, a: Held, b: Held) -> Option<JoinModel> {
        let probe = || {
            if !self.probed {
                return None;
            }
            super::keyprobe::applies(ctx, v, a.side, b.side)
                .map(|(m, a_src)| JoinModel::Probe { m, a_src })
        };
        let first = self.probed && super::keyprobe::first(ctx, v);
        if first && let Some(m) = probe() {
            return Some(m);
        }
        if self.starred
            && a.star != 0
            && b.star != 0
            && let Some((rr, rd)) = super::charsets::factor(ctx, v, a.star, b.star)
        {
            return Some(JoinModel::Star { rr, rd });
        }
        if first {
            return None;
        }
        probe()
    }
}

/// The merge of two lists of domains by join variable, each sorted by it: the larger
/// where both have the variable.
fn merge_dom(a: &[(u32, f64)], b: &[(u32, f64)]) -> Vec<(u32, f64)> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut k) = (0, 0);
    while i < a.len() || k < b.len() {
        match (a.get(i), b.get(k)) {
            (Some(&(ja, x)), Some(&(jb, y))) if ja == jb => {
                out.push((ja, x.max(y)));
                i += 1;
                k += 1;
            }
            (Some(&(ja, x)), Some(&(jb, _))) if ja < jb => {
                out.push((ja, x));
                i += 1;
            }
            (Some(&(ja, x)), None) => {
                out.push((ja, x));
                i += 1;
            }
            (_, Some(&(jb, y))) => {
                out.push((jb, y));
                k += 1;
            }
            (None, None) => unreachable!(),
        }
    }
    out
}

/// The merge of two lists of what plans hold by join variable, each sorted by it.
fn merge_held(a: &[(u32, Held)], b: &[(u32, Held)]) -> Vec<(u32, Held)> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut k) = (0, 0);
    while i < a.len() || k < b.len() {
        match (a.get(i), b.get(k)) {
            (Some(&(ja, ha)), Some(&(jb, hb))) if ja == jb => {
                out.push((ja, ha.merge(hb)));
                i += 1;
                k += 1;
            }
            (Some(&x), Some(&(jb, _))) if x.0 < jb => {
                out.push(x);
                i += 1;
            }
            (Some(&x), None) => {
                out.push(x);
                i += 1;
            }
            (_, Some(&y)) => {
                out.push(y);
                k += 1;
            }
            (None, None) => unreachable!(),
        }
    }
    out
}

impl Group {
    /// The group of `items` and `filters`. On the checked route the storage is sized
    /// from the inputs and the filters before any of it is allocated, and admitted in
    /// two steps, the variables and filters first and the join models once the join
    /// variables are known. The group holds the credit until it drops.
    fn new(items: &[Vec<Node>], filters: &[Expr], pl: &Planner) -> Result<Group> {
        #[cfg(test)]
        let _stage = crate::test_alloc::Stage::enter(0);
        let ctx = pl.ctx;
        let total: usize = items.iter().map(|it| it[0].vars.len()).sum();
        first_storage(|| total * std::mem::size_of::<VarId>());
        let mut held = refused(pl.planning_stage(GROUP_PHASE).and_then(|()| {
            pl.planning_scratch(|layout| {
                layout.slots::<VarId>(total)?;
                layout.map::<VarId, u32>(total)?;
                // count, join and jvars
                layout.slots::<u32>(3 * total)?;
                layout.slots::<Vec<u32>>(items.len())?;
                for it in items {
                    layout.slots::<u32>(it[0].vars.len())?;
                }
                layout.slots::<FilterInfo>(filters.len())?;
                let mut most = 0;
                for f in filters {
                    let c = var_count(f);
                    layout.slots::<u32>(c)?;
                    most = most.max(c);
                }
                // one filter's variable set and local indices at a time
                layout.slots::<VarId>(most)?;
                layout.slots::<Option<u32>>(most)
            })
        }))?;
        let checked = held.is_some();
        let cap = if checked { total } else { 0 };
        let mut vars = Vec::with_capacity(cap);
        let mut idx: FxHashMap<VarId, u32> =
            FxHashMap::with_capacity_and_hasher(cap, Default::default());
        let mut count: Vec<u32> = Vec::with_capacity(cap);
        let mut item_vars = Vec::with_capacity(items.len());
        for it in items {
            let mut iv = Vec::with_capacity(it[0].vars.len());
            for &v in &it[0].vars {
                let l = *idx.entry(v).or_insert_with(|| {
                    vars.push(v);
                    count.push(0);
                    vars.len() as u32 - 1
                });
                if !iv.contains(&l) {
                    iv.push(l);
                    count[l as usize] += 1;
                }
            }
            item_vars.push(iv);
        }
        let mut join = vec![u32::MAX; vars.len()];
        let mut jvars = Vec::with_capacity(cap);
        for (l, &c) in count.iter().enumerate() {
            if c >= 2 {
                join[l] = jvars.len() as u32;
                jvars.push(l as u32);
            }
        }
        // a conjunct is looked up by its text only when some selectivity was sampled
        let sampling = ctx.has_sampled();
        let mut infos = Vec::with_capacity(filters.len());
        for f in filters {
            let vs = if checked {
                // the same variables as `var_set`, in storage of the counted size
                let mut vs = Vec::with_capacity(var_count(f));
                f.vars(&mut vs);
                vs.sort_unstable();
                vs.dedup();
                vs
            } else {
                f.var_set()
            };
            let local: Vec<Option<u32>> = vs.iter().map(|v| idx.get(v).copied()).collect();
            let placeable = !f.has_exists() && !vs.is_empty() && local.iter().all(Option::is_some);
            let cached = super::exprcache::eligible(&[f])
                .ok()
                .flatten()
                .and_then(|v| idx.get(&v).copied());
            let sel = match (&held, sampling) {
                (_, false) => None,
                (Some(credit), true) => {
                    let mut text = PreparationText::new(credit.preparation())?;
                    let written = f.write_description(ctx, &mut text);
                    let (text, credit) = text.finish()?;
                    if written.is_err() {
                        return Err(Error::invalid("could not write a filter description"));
                    }
                    let sel = ctx.sampled(&text).map(|s| s.sel);
                    // the text goes before its credit
                    drop(text);
                    drop(credit);
                    sel
                }
                (None, true) => ctx.sampled(&f.display(ctx)).map(|s| s.sel),
            };
            let vars = if checked {
                let mut out = Vec::with_capacity(local.iter().flatten().count());
                out.extend(local.iter().flatten().copied());
                out
            } else {
                local.into_iter().flatten().collect()
            };
            infos.push(FilterInfo {
                placeable,
                vars,
                cached,
                sel,
            });
        }
        let filters = infos;
        // the inputs that bind several keys, needed only when two inputs could each bind
        // several keys of one join
        let several =
            |iv: &Vec<u32>| iv.iter().filter(|&&l| join[l as usize] != u32::MAX).count() > 1;
        let backoff = ctx.opt.key_backoff && item_vars.iter().filter(|iv| several(iv)).count() > 1;
        // The models' credit, with the scratch of one input at a time, which is released
        // once they are built.
        let mut scratch = 0;
        if let Some(credit) = held.as_mut() {
            first_storage(|| items.len() * std::mem::size_of::<Vec<(u32, u64)>>());
            let models = refused(pl.planning_stage(MODELS_PHASE).and_then(|()| {
                pl.planning_scratch(|layout| {
                    let mut most = super::checked_layout::Layout::new(credit.preparation());
                    layout.slots::<Vec<(u32, u64)>>(items.len())?;
                    layout.slots::<Vec<(u32, Side)>>(items.len())?;
                    for (i, it) in items.iter().enumerate() {
                        let mut one = super::checked_layout::Layout::new(credit.preparation());
                        if ctx.opt.characteristic_sets {
                            let bound = super::charsets::of_node_bound(ctx, &it[0]);
                            layout.slots::<(u32, u64)>(bound)?;
                            one.slots::<(VarId, u64)>(bound)?;
                            one.map::<VarId, usize>(bound)?;
                        }
                        if ctx.opt.probed_keys {
                            let own = own_keys(&item_vars[i], &join).count();
                            layout.slots::<(u32, Side)>(own)?;
                            one.slots::<u32>(own)?;
                        }
                        if one.bytes() > most.bytes() {
                            most = one;
                        }
                    }
                    if backoff {
                        layout.slots::<Vec<Vec<u32>>>(items.len())?;
                        for it in items {
                            let (mut inputs, mut keys) = (0usize, 0usize);
                            plan::input_vars(&it[0], &mut |vs| {
                                let k = keys_in(vs, &idx, &join).count();
                                if k > 1 {
                                    inputs += 1;
                                    keys += k;
                                }
                            });
                            layout.slots::<Vec<u32>>(inputs)?;
                            layout.slots::<u32>(keys)?;
                        }
                    }
                    scratch = most.bytes();
                    layout.add(
                        usize::try_from(scratch).map_err(|_| credit.preparation().exhausted())?,
                    )
                })
            }))?;
            if let Some(models) = models {
                credit.merge(models);
            }
        }
        let checked_cap = |n: usize| if checked { n } else { 0 };
        let mut models = Models {
            stars: Vec::with_capacity(checked_cap(items.len())),
            sides: Vec::with_capacity(checked_cap(items.len())),
            ..Models::default()
        };
        for (i, it) in items.iter().enumerate() {
            ctx.check()?;
            let mut masks = Vec::new();
            if ctx.opt.characteristic_sets {
                if checked {
                    let bound = super::charsets::of_node_bound(ctx, &it[0]);
                    super::charsets::of_node_within(ctx, &it[0], &mut masks, bound);
                } else {
                    super::charsets::of_node(ctx, &it[0], &mut masks);
                }
            }
            let star = |(v, m): (VarId, u64)| {
                let j = join[*idx.get(&v)? as usize];
                (j != u32::MAX).then_some((j, m))
            };
            models.stars.push(if checked {
                let kept = masks.iter().copied().filter_map(star).count();
                let mut stars = Vec::with_capacity(kept);
                stars.extend(masks.iter().copied().filter_map(star));
                drop(masks);
                stars
            } else {
                masks.into_iter().filter_map(star).collect()
            });
            let mut sides = if checked && ctx.opt.probed_keys {
                Vec::with_capacity(own_keys(&item_vars[i], &join).count())
            } else {
                Vec::new()
            };
            if ctx.opt.probed_keys {
                // an input without the variable holds no leaf on it, and its side merges
                // as nothing
                let mut own: Vec<u32> = if checked {
                    let mut own = Vec::with_capacity(own_keys(&item_vars[i], &join).count());
                    own.extend(own_keys(&item_vars[i], &join));
                    own
                } else {
                    own_keys(&item_vars[i], &join).collect()
                };
                own.sort_unstable();
                for j in own {
                    let v = vars[jvars[j as usize] as usize];
                    if let Some(s) = super::keyprobe::side(ctx, &it[0], v) {
                        sides.push((j, s));
                    }
                }
            }
            models.sides.push(sides);
        }
        if backoff {
            models.inputs.reserve_exact(checked_cap(items.len()));
            for it in items {
                let mut inputs = Vec::new();
                if checked {
                    let mut count = 0;
                    plan::input_vars(&it[0], &mut |vs| {
                        count += usize::from(keys_in(vs, &idx, &join).count() > 1);
                    });
                    inputs.reserve_exact(count);
                }
                plan::input_vars(&it[0], &mut |vs| {
                    let mut x: Vec<u32> = if checked {
                        let mut x = Vec::with_capacity(keys_in(vs, &idx, &join).count());
                        x.extend(keys_in(vs, &idx, &join));
                        x
                    } else {
                        keys_in(vs, &idx, &join).collect()
                    };
                    if x.len() > 1 {
                        x.sort_unstable();
                        x.dedup();
                        inputs.push(x);
                    }
                });
                models.inputs.push(inputs);
            }
        }
        models.starred = models.stars.iter().any(|s| !s.is_empty());
        models.probed = models.sides.iter().any(|s| !s.is_empty());
        if let Some(credit) = held.as_mut() {
            credit.release_to(credit.bytes() - scratch);
        }
        Ok(Group {
            vars,
            idx,
            join,
            jvars,
            item_vars,
            filters,
            models,
            _held: held,
        })
    }
}

/// The join variable indexes of the variables `vs`, unsorted.
fn keys_in<'a>(
    vs: &'a [VarId],
    idx: &'a FxHashMap<VarId, u32>,
    join: &'a [u32],
) -> impl Iterator<Item = u32> + 'a {
    vs.iter()
        .filter_map(|v| idx.get(v).map(|&l| join[l as usize]))
        .filter(|&j| j != u32::MAX)
}

/// The join variable indexes of the local variables `iv`.
fn own_keys<'a>(iv: &'a [u32], join: &'a [u32]) -> impl Iterator<Item = u32> + 'a {
    iv.iter()
        .map(|&l| join[l as usize])
        .filter(|&j| j != u32::MAX)
}

/// The occurrences of variables in `f`, which bound its variable set.
fn var_count(f: &Expr) -> usize {
    let mut n = 0usize;
    let Ok(()) = f.visit_vars(&mut |_| {
        n += 1;
        Ok::<_, std::convert::Infallible>(())
    });
    n
}

#[cfg(test)]
thread_local! {
    /// Refusals and cancellations of the program's admissions on this thread, for the
    /// tests of their admission.
    pub(super) static DP_REFUSALS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The bytes of the first storage that the program's latest admission covers: the
    /// table it last asked to grow to, or the first buffer of another admission.
    pub(super) static DP_GROWTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The rounds the program planned on this thread.
    pub(super) static DP_ROUNDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The scratch credit the last checked greedy ordering released when it finished,
    /// the credit it kept, and the storage it keeps (see `Greedy::storage`).
    pub(super) static GREEDY_RELEASED: std::cell::Cell<(usize, usize, usize)> =
        const { std::cell::Cell::new((0, 0, 0)) };
}

/// The set bits of `m`.
fn bits_of(mut m: u32) -> impl Iterator<Item = usize> + Clone {
    std::iter::from_fn(move || {
        (m != 0).then(|| {
            let i = m.trailing_zeros() as usize;
            m &= m - 1;
            i
        })
    })
}

/// The plan for a connected group of inputs (each with its access paths), placing the
/// filters it can and removing them from `filters`. A group that the program cannot
/// track, with more than 16 inputs or more than 128 variables shared between inputs,
/// keeps the greedy plan.
pub(super) fn order(
    pl: &Planner,
    mut items: Vec<Vec<Node>>,
    filters: &mut Vec<Expr>,
) -> Result<Node> {
    let g = Group::new(&items, filters, pl)?;
    let dp = Dp::prepare(&g, &items, pl)?;
    let greedy = Greedy::run(&g, &items, filters, pl)?;
    if let Some(mut dp) = dp {
        let bound = greedy.cost();
        if let Some(best) = dp.run(&items, filters, bound, pl)? {
            let (node, applied) = dp.build(best, &mut items, filters, pl)?;
            debug_assert!(
                node.cost <= bound.unwrap_or(f64::INFINITY),
                "plan dearer than the bound"
            );
            // the program places only the first 64 filters (see `place_masked`)
            let mut i = 0;
            filters.retain(|_| {
                let keep = i >= 64 || applied & (1u64 << i) == 0;
                i += 1;
                keep
            });
            return Ok(node);
        }
    }
    greedy.build(items, filters, pl)
}

/// Place on `n` the filters among the first 64 of `filters` that `applied` does not
/// mark and whose variables `n` binds, and return `n` with `applied` and those filters
/// marked. `fvars` holds each filter's variables, with an impossible variable for one
/// that holds an EXISTS, which is never placed here.
/// On the checked route the FILTER is admitted before the filters are copied, and the
/// credit returned holds what the plan keeps of it.
pub(super) fn place_masked(
    pl: &Planner,
    n: Node,
    applied: u64,
    filters: &[Expr],
    fvars: &[Vec<VarId>],
) -> Result<(Node, u64, Option<Reservation>)> {
    #[cfg(test)]
    let _stage = crate::test_alloc::Stage::within(6);
    let mut mask = applied;
    let mut count = 0;
    for (i, fv) in fvars.iter().enumerate().take(filters.len().min(64)) {
        if mask & (1 << i) == 0 && !fv.is_empty() && fv.iter().all(|v| n.vars.contains(v)) {
            mask |= 1 << i;
            count += 1;
        }
    }
    if count == 0 {
        return Ok((n, mask, None));
    }
    let placed = mask & !applied;
    let now = || {
        filters
            .iter()
            .take(64)
            .enumerate()
            .filter(move |&(i, _)| placed & (1 << i) != 0)
            .map(|(_, f)| f)
    };
    let scratch = refused(pl.filter_scratch(&n, now(), true))?;
    let mut copies = Vec::with_capacity(count);
    copies.extend(now().cloned());
    let (n, kept) = pl.filter_held(scratch, n, copies, 0)?;
    Ok((n, mask, kept))
}

// ------------------------------------------------------------------------------
// greedy
// ------------------------------------------------------------------------------

/// A set of local variable indices.
#[derive(Clone)]
struct Bits(Vec<u64>);

impl Bits {
    fn new(n: usize) -> Bits {
        Bits(vec![0; n.div_ceil(64)])
    }
    fn set(&mut self, i: u32) {
        self.0[i as usize / 64] |= 1 << (i % 64);
    }
    fn get(&self, i: u32) -> bool {
        self.0[i as usize / 64] & (1 << (i % 64)) != 0
    }
    fn or(&self, o: &Bits) -> Bits {
        Bits(self.0.iter().zip(&o.0).map(|(a, b)| a | b).collect())
    }
    fn intersects(&self, o: &Bits) -> bool {
        self.0.iter().zip(&o.0).any(|(a, b)| a & b != 0)
    }
    fn ones(&self) -> impl Iterator<Item = usize> + Clone + '_ {
        self.0.iter().enumerate().flat_map(|(w, &x)| {
            let mut x = x;
            std::iter::from_fn(move || {
                (x != 0).then(|| {
                    let i = x.trailing_zeros() as usize;
                    x &= x - 1;
                    w * 64 + i
                })
            })
        })
    }
}

enum From {
    Leaf,
    Join(usize, usize, How),
}

/// A plan of the greedy ordering.
struct Unit {
    sum: Sum,
    /// the inputs it joins
    inputs: Bits,
    /// variables in the order of the plan's columns, as local indices
    vars: Vec<u32>,
    has: Bits,
    certain: Bits,
    /// distinct-value estimates of the join variables, by join variable index
    d: Vec<(u32, f64)>,
    /// the domains of the join variables (see [`plan::domain`]), by join variable index
    dom: Vec<(u32, f64)>,
    probe: Option<ProbeSide>,
    from: From,
    /// the filters placed on top, by index
    filters: Vec<usize>,
    /// what its inputs hold on the join variables, by join variable index, when the
    /// group has star masks or probed variables (see [`Models`])
    held: Vec<(u32, Held)>,
}

impl Unit {
    fn d(&self, j: u32) -> f64 {
        let i = self.d.binary_search_by_key(&j, |x| x.0).unwrap();
        self.d[i].1
    }

    fn dom(&self, j: u32) -> f64 {
        self.dom
            .binary_search_by_key(&j, |x| x.0)
            .map_or(0.0, |i| self.dom[i].1)
    }

    fn held_on(&self, j: u32) -> Held {
        self.held
            .binary_search_by_key(&j, |x| x.0)
            .map_or_else(|_| Held::default(), |i| self.held[i].1)
    }
}

/// A join the greedy ordering may take next, ordered so that the heap's top is the one
/// with the fewest estimated rows, then the cheapest, then the earliest pair.
struct Pair {
    est: f64,
    cost: f64,
    a: u32,
    b: u32,
    how: How,
    sorted: Option<VarId>,
    /// the join variable with how the join was estimated, when not from distinct values
    model: Option<(u32, JoinModel)>,
    chain: Chain,
}

impl PartialEq for Pair {
    fn eq(&self, o: &Pair) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Pair {}
impl PartialOrd for Pair {
    fn partial_cmp(&self, o: &Pair) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Pair {
    fn cmp(&self, o: &Pair) -> Ordering {
        o.est
            .total_cmp(&self.est)
            .then(o.cost.total_cmp(&self.cost))
            .then(o.a.cmp(&self.a))
            .then(o.b.cmp(&self.b))
    }
}

/// The planner's greedy ordering: each input takes its cheapest access path and the
/// filters it binds, then the join with the fewest estimated rows (the cheapest among
/// equal ones, the earliest pair among equal costs) is taken until one plan is left.
/// Plans are numbered in the order they are made, which is the order of the planner's
/// list of plans, so ties break the same way.
struct Greedy {
    units: Vec<Unit>,
    /// per input, its cheapest access path, and the plan with the filters on it when it
    /// has any (an input without filters is moved out of the inputs when the plan is
    /// built, not copied, since it can be a whole subtree)
    leaves: Vec<(usize, Option<Node>)>,
    /// the plans left, in order
    alive: Vec<usize>,
    /// filters placed, by index
    placed: Vec<bool>,
    /// the checked route's credit for the copies of inputs that filters are placed on,
    /// which the plan keeps when it is built from this ordering
    leaf_held: Option<Reservation>,
    /// the checked route's credit for the above, dropped after them
    held: Option<Reservation>,
}

/// The storage of a set of `n` indices.
fn bits_bytes(n: usize) -> usize {
    n.div_ceil(64) * std::mem::size_of::<u64>()
}

impl Greedy {
    /// The greedy ordering of `items`. On the checked route the tables, whose sizes
    /// follow from the group, are admitted before they are allocated, and each plan's
    /// storage, and the heap's growth, as the ordering makes them.
    fn run(g: &Group, items: &[Vec<Node>], filters: &[Expr], pl: &Planner) -> Result<Greedy> {
        #[cfg(test)]
        let _stage = crate::test_alloc::Stage::enter(1);
        let ctx = pl.ctx;
        let (n, nv, jv) = (items.len(), g.vars.len(), g.jvars.len());
        let held_vars: usize = g.item_vars.iter().map(Vec::len).sum();
        first_storage(|| 2 * n * std::mem::size_of::<Unit>());
        let mut held = refused(pl.planning_stage(GREEDY_PHASE).and_then(|()| {
            pl.planning_scratch(|layout| {
                layout.slots::<bool>(filters.len())?;
                layout.slots::<Unit>(2 * n)?;
                // the inputs, variables and certain variables of each input's plan
                layout.add(n * (2 * bits_bytes(nv) + bits_bytes(n)))?;
                layout.slots::<(usize, Option<Node>)>(n)?;
                // alive and the plans paired with each new one
                layout.slots::<usize>(2 * n)?;
                // seen and dead
                layout.slots::<usize>(2 * n)?;
                layout.slots::<bool>(2 * n)?;
                // the holders of each variable, with their counts while they are sized
                layout.slots::<Vec<usize>>(nv)?;
                layout.slots::<usize>(held_vars)?;
                layout.slots::<u32>(nv)?;
                layout.slots::<Shared>(jv)?;
                layout.slots::<Cand>(jv + 3)
            })
        }))?;
        let checked = held.is_some();
        let cap = |k: usize| if checked { k } else { 0 };
        let mut leaf_held = match &held {
            Some(credit) => Some(credit.preparation().reserve(0)?),
            None => None,
        };
        let batched = plan::Costing::of(ctx);
        let mut placed = vec![false; filters.len()];
        let mut units: Vec<Unit> = Vec::with_capacity(2 * n);
        let mut leaves = Vec::with_capacity(n);
        // the credit of the leaves' transient lists, released when the ordering ends
        let mut listed = 0;
        for (i, opts) in items.iter().enumerate() {
            ctx.check()?;
            let mut at = 0;
            for (k, o) in opts.iter().enumerate().skip(1) {
                if o.cost.total_cmp(&opts[at].cost) == Ordering::Less {
                    at = k;
                }
            }
            let best = &opts[at];
            let mut has = Bits::new(nv);
            for &l in &g.item_vars[i] {
                has.set(l);
            }
            let binds = |f: usize| {
                !placed[f]
                    && g.filters[f].placeable
                    && g.filters[f].vars.iter().all(|&l| has.get(l))
            };
            let placing = if checked {
                (0..filters.len()).filter(|&f| binds(f)).count()
            } else {
                0
            };
            // filters keep the variables of their input
            let keys = if checked {
                best.vars
                    .iter()
                    .filter(|v| g.join[g.idx[*v] as usize] != u32::MAX)
                    .count()
            } else {
                0
            };
            let starred = g.models.starred || g.models.probed;
            if let Some(credit) = held.as_mut() {
                // the plan's variables, estimates, domains and what it holds, with the
                // filters it places while they are listed
                let mut bytes = best.vars.len() * std::mem::size_of::<u32>();
                bytes += 2 * keys * std::mem::size_of::<(u32, f64)>();
                bytes += placing * std::mem::size_of::<usize>();
                if starred {
                    bytes += g.models.held_by_bound(i)
                        * (std::mem::size_of::<u32>() + std::mem::size_of::<(u32, Held)>());
                }
                first_storage(|| bytes);
                let preparation = credit.preparation().clone();
                refused(preparation.stage(GREEDY_UNIT_PHASE))?;
                refused(credit.grow_by(bytes as u64))?;
                // the filter list and the variables `held_by_within` lists them from
                listed += placing * std::mem::size_of::<usize>();
                if starred {
                    listed += g.models.held_by_bound(i) * std::mem::size_of::<u32>();
                }
            }
            if let Some(credit) = leaf_held.as_mut()
                && placing > 0
            {
                // the copy of the input, under the filters placed on it, whose copies the
                // FILTER admits
                let preparation = credit.preparation().clone();
                refused(preparation.stage(GREEDY_LEAF_PHASE))?;
                let mut layout = super::checked_layout::Layout::new(&preparation);
                layout.node(best)?;
                first_storage(|| layout.bytes() as usize);
                refused(credit.grow_by(layout.bytes()))?;
            }
            let mut now = Vec::with_capacity(placing);
            now.extend((0..filters.len()).filter(|&f| binds(f)));
            let filtered = if now.is_empty() {
                None
            } else {
                for &f in &now {
                    placed[f] = true;
                }
                let copy = best.clone();
                #[cfg(test)]
                let _stage = crate::test_alloc::Stage::within(6);
                // the copy of the input is admitted with the leaf, and the FILTER with the
                // copies of its filters now, before they are made
                let scratch =
                    refused(pl.filter_scratch(&copy, now.iter().map(|&f| &filters[f]), true))?;
                let conjuncts: Vec<Expr> = now.iter().map(|&f| filters[f].clone()).collect();
                let (node, kept) = pl.filter_held(scratch, copy, conjuncts, 0)?;
                match (leaf_held.as_mut(), kept) {
                    (Some(credit), Some(kept)) => credit.merge(kept),
                    (None, Some(kept)) => pl.retain_planning(kept),
                    _ => {}
                }
                Some(node)
            };
            drop(now);
            let node = filtered.as_ref().unwrap_or(best);
            debug_assert_eq!(node.vars.len(), best.vars.len());
            let mut certain = Bits::new(nv);
            let mut d = Vec::with_capacity(cap(keys));
            let mut dom = Vec::with_capacity(cap(keys));
            let vars: Vec<u32> = node.vars.iter().map(|v| g.idx[v]).collect();
            for (&l, &v) in vars.iter().zip(&node.vars) {
                if node.certain.contains(&v) {
                    certain.set(l);
                }
                let j = g.join[l as usize];
                if j != u32::MAX {
                    d.push((j, node.d(v)));
                    dom.push((j, plan::domain(ctx, node, v)));
                }
            }
            d.sort_unstable_by_key(|x| x.0);
            dom.sort_unstable_by_key(|x| x.0);
            let mut inputs = Bits::new(n);
            inputs.set(i as u32);
            units.push(Unit {
                sum: Sum::of(node),
                inputs,
                vars,
                has,
                certain,
                d,
                dom,
                probe: if batched.index_joins {
                    indexjoin::probe_side(node, ctx)
                } else {
                    None
                },
                from: From::Leaf,
                filters: Vec::new(),
                held: match (starred, checked) {
                    (false, _) => Vec::new(),
                    (true, false) => g.models.held_by(i),
                    (true, true) => g.models.held_by_within(i),
                },
            });
            leaves.push((at, filtered));
        }
        let mut gr = Greedy {
            units,
            leaves,
            alive: (0..n).collect(),
            placed,
            leaf_held,
            held,
        };
        let mut heap = BinaryHeap::new();
        let mut cands = Vec::with_capacity(cap(jv + 3));
        let mut shared = Vec::with_capacity(cap(jv));
        // the plans left that bind each variable, so that only plans sharing a variable
        // are paired: testing every pair costs the square of the inputs at each step. A
        // join replaces one or two holders of each of its variables with itself, so the
        // checked route sizes each list once from the inputs.
        let mut holders: Vec<Vec<usize>> = if checked {
            let mut count = vec![0u32; nv];
            for iv in &g.item_vars {
                for &l in iv {
                    count[l as usize] += 1;
                }
            }
            let holders = count
                .iter()
                .map(|&c| Vec::with_capacity(c as usize))
                .collect();
            drop(count);
            holders
        } else {
            vec![Vec::new(); nv]
        };
        for (u, unit) in gr.units.iter().enumerate() {
            for l in unit.has.ones() {
                holders[l].push(u);
            }
        }
        // when each plan was last paired with the plan being paired
        let mut seen = vec![usize::MAX; 2 * gr.units.len()];
        let mut with = Vec::with_capacity(cap(n));
        for b in 0..gr.units.len() {
            ctx.check()?;
            with.clear();
            for l in gr.units[b].has.ones() {
                for &a in &holders[l] {
                    if a < b && seen[a] != b {
                        seen[a] = b;
                        with.push(a);
                    }
                }
            }
            gr.heap_room(&mut heap, with.len())?;
            for &a in &with {
                gr.push_pair(g, a, b, batched, ctx, &mut heap, &mut cands, &mut shared);
            }
        }
        let mut dead = Vec::with_capacity(cap(2 * n));
        dead.resize(gr.units.len(), false);
        while gr.alive.len() > 1 {
            ctx.check()?;
            let Some(p) = std::iter::from_fn(|| heap.pop())
                .find(|p: &Pair| !dead[p.a as usize] && !dead[p.b as usize])
            else {
                break;
            };
            let w = gr.join(g, &p, filters)?;
            dead[p.a as usize] = true;
            dead[p.b as usize] = true;
            dead.push(false);
            gr.alive.retain(|&u| u != p.a as usize && u != p.b as usize);
            if seen.len() <= w {
                seen.resize(w + 1, usize::MAX);
            }
            with.clear();
            for l in gr.units[w].has.ones() {
                let h = &mut holders[l];
                h.retain(|&u| u != p.a as usize && u != p.b as usize);
                for &u in h.iter() {
                    if seen[u] != w {
                        seen[u] = w;
                        with.push(u);
                    }
                }
                h.push(w);
            }
            gr.heap_room(&mut heap, with.len())?;
            for &u in &with {
                gr.push_pair(g, u, w, batched, ctx, &mut heap, &mut cands, &mut shared);
            }
            gr.alive.push(w);
        }
        // The ordering keeps the plans, the leaves, the plans left and the placed
        // filters. The heap and the working lists are dropped here, and with them
        // their credit, sized as above.
        let heap_bytes = heap.capacity() * std::mem::size_of::<Pair>();
        drop((heap, cands, shared, holders, seen, with, dead));
        if let Some(credit) = gr.held.as_mut() {
            let scratch = heap_bytes
                + listed
                + n * std::mem::size_of::<usize>()
                + 2 * n * (std::mem::size_of::<usize>() + std::mem::size_of::<bool>())
                + nv * (std::mem::size_of::<Vec<usize>>() + std::mem::size_of::<u32>())
                + held_vars * std::mem::size_of::<usize>()
                + jv * std::mem::size_of::<Shared>()
                + (jv + 3) * std::mem::size_of::<Cand>();
            credit.release_to(credit.bytes() - scratch as u64);
            #[cfg(test)]
            {
                let (kept, storage) = (credit.bytes() as usize, gr.storage());
                GREEDY_RELEASED.with(|r| r.set((scratch, kept, storage)));
            }
        }
        Ok(gr)
    }

    /// The storage the ordering keeps, besides the copies of its leaves, measured from
    /// its vectors.
    #[cfg(test)]
    fn storage(&self) -> usize {
        use std::mem::size_of;
        let bits = |b: &Bits| b.0.capacity() * size_of::<u64>();
        let units: usize = self
            .units
            .iter()
            .map(|u| {
                bits(&u.inputs)
                    + bits(&u.has)
                    + bits(&u.certain)
                    + u.vars.capacity() * size_of::<u32>()
                    + (u.d.capacity() + u.dom.capacity()) * size_of::<(u32, f64)>()
                    + u.filters.capacity() * size_of::<usize>()
                    + u.held.capacity() * size_of::<(u32, Held)>()
            })
            .sum();
        units
            + self.units.capacity() * size_of::<Unit>()
            + self.leaves.capacity() * size_of::<(usize, Option<Node>)>()
            + self.alive.capacity() * size_of::<usize>()
            + self.placed.capacity() * size_of::<bool>()
    }

    /// Cost the joins of plans `a` and `b` (`a` made first) and queue the best.
    #[allow(clippy::too_many_arguments)]
    fn push_pair(
        &self,
        g: &Group,
        a: usize,
        b: usize,
        batched: plan::Costing,
        ctx: &Ctx,
        heap: &mut BinaryHeap<Pair>,
        cands: &mut Vec<Cand>,
        shared: &mut Vec<Shared>,
    ) {
        let (ua, ub) = (&self.units[a], &self.units[b]);
        if !ua.has.intersects(&ub.has) {
            return;
        }
        shared.clear();
        for &l in &ua.vars {
            if ub.has.get(l) {
                let j = g.join[l as usize];
                shared.push(Shared {
                    v: g.vars[l as usize],
                    j,
                    da: ua.d(j),
                    db: ub.d(j),
                    doma: ua.dom(j),
                    domb: ub.dom(j),
                    ca: ua.certain.get(l),
                    cb: ub.certain.get(l),
                });
            }
        }
        let model = match shared.as_slice() {
            [s] => {
                #[cfg(test)]
                let _stage = crate::test_alloc::Stage::enter(8);
                let j = g.join[g.idx[&s.v] as usize];
                g.models
                    .of_held(ctx, s.v, ua.held_on(j), ub.held_on(j))
                    .map(|m| (j, m))
            }
            _ => None,
        };
        let capped = ctx.opt.key_backoff
            && shared.len() > 1
            && g.models.one_binds(ua.inputs.ones(), shared)
            && g.models.one_binds(ub.inputs.ones(), shared);
        let est = pair_est(
            &ua.sum,
            &ub.sum,
            shared,
            model.map(|(_, m)| m),
            ctx.opt.key_backoff,
            capped,
        );
        joins(
            &ua.sum,
            ua.probe.as_ref(),
            &ub.sum,
            ub.probe.as_ref(),
            shared,
            est,
            batched,
            &mut |_, _| true,
            cands,
        );
        let mut best = cands[0];
        for c in &cands[1..] {
            if c.cost < best.cost {
                best = *c;
            }
        }
        heap.push(Pair {
            est,
            cost: best.cost,
            a: a as u32,
            b: b as u32,
            how: best.how,
            sorted: best.sorted,
            model,
            chain: best.chain,
        });
    }

    /// Make room in `heap` for `need` more joins. On the checked route the larger
    /// storage is admitted before it is allocated.
    fn heap_room(&mut self, heap: &mut BinaryHeap<Pair>, need: usize) -> Result<()> {
        match self.held.as_mut() {
            Some(credit) if heap.capacity() - heap.len() < need => {
                grow_admitted_in(credit, heap, need, GREEDY_GROW_PHASE)
            }
            _ => Ok(()),
        }
    }

    /// Make the plan joining `p.a` and `p.b`, with the filters it binds on top. On the
    /// checked route its storage is admitted before it is allocated.
    fn join(&mut self, g: &Group, p: &Pair, filters: &[Expr]) -> Result<usize> {
        let (a, b) = (&self.units[p.a as usize], &self.units[p.b as usize]);
        let (drive, other) = if p.how == How::ProbeLeft {
            (b, a)
        } else {
            (a, b)
        };
        let binds = |f: usize| {
            !self.placed[f]
                && g.filters[f].placeable
                && g.filters[f]
                    .vars
                    .iter()
                    .all(|&l| a.has.get(l) || b.has.get(l))
        };
        let checked = self.held.is_some();
        let (width, placing) = if checked {
            let rest = other.vars.iter().filter(|&&l| !drive.has.get(l)).count();
            (
                drive.vars.len() + rest,
                (0..filters.len()).filter(|&f| binds(f)).count(),
            )
        } else {
            (0, 0)
        };
        if let Some(credit) = self.held.as_mut() {
            let mut bytes = width * std::mem::size_of::<u32>();
            bytes += (2 * a.has.0.len() + a.inputs.0.len()) * std::mem::size_of::<u64>();
            bytes += (a.d.len() + b.d.len() + a.dom.len() + b.dom.len())
                * std::mem::size_of::<(u32, f64)>();
            bytes += placing * std::mem::size_of::<usize>();
            bytes += (a.held.len() + b.held.len()) * std::mem::size_of::<(u32, Held)>();
            first_storage(|| bytes);
            let preparation = credit.preparation().clone();
            refused(preparation.stage(GREEDY_UNIT_PHASE))?;
            refused(credit.grow_by(bytes as u64))?;
        }
        let mut vars = if checked {
            let mut vars = Vec::with_capacity(width);
            vars.extend_from_slice(&drive.vars);
            vars
        } else {
            drive.vars.clone()
        };
        vars.extend(other.vars.iter().filter(|&&l| !drive.has.get(l)));
        let has = a.has.or(&b.has);
        let certain = match p.how {
            How::ProbeRight => a.certain.or(&b.has),
            How::ProbeLeft => b.certain.or(&a.has),
            _ => a.certain.or(&b.certain),
        };
        let mut d = Vec::with_capacity(a.d.len() + b.d.len());
        let (mut i, mut k) = (0, 0);
        while i < a.d.len() || k < b.d.len() {
            let x = match (a.d.get(i), b.d.get(k)) {
                (Some(&(ja, da)), Some(&(jb, db))) if ja == jb => {
                    i += 1;
                    k += 1;
                    match p.model {
                        Some((j, m)) if j == ja => (ja, m.distinct(da, db)),
                        _ => (ja, da.min(db)),
                    }
                }
                (Some(&(ja, da)), Some(&(jb, _))) if ja < jb => {
                    i += 1;
                    (ja, da)
                }
                (Some(&(ja, da)), None) => {
                    i += 1;
                    (ja, da)
                }
                (_, Some(&(jb, db))) => {
                    k += 1;
                    (jb, db)
                }
                (None, None) => unreachable!(),
            };
            d.push((x.0, x.1.min(p.est).max(1.0)));
        }
        let dom = merge_dom(&a.dom, &b.dom);
        let inputs = a.inputs.or(&b.inputs);
        let mut sum = Sum {
            chain: p.chain,
            ..Sum::new(p.cost, p.est, p.sorted)
        };
        let now: Vec<usize> = if checked {
            let mut now = Vec::with_capacity(placing);
            now.extend((0..filters.len()).filter(|&f| binds(f)));
            now
        } else {
            (0..filters.len())
                .filter(|&f| {
                    !self.placed[f]
                        && g.filters[f].placeable
                        && g.filters[f].vars.iter().all(|&l| has.get(l))
                })
                .collect()
        };
        if !now.is_empty() {
            let unsorted = now.iter().any(|&f| {
                g.filters[f]
                    .cached
                    .is_some_and(|l| has.get(l) && sum.sorted != Some(g.vars[l as usize]))
            });
            let sel = super::sample::combine(now.iter().map(|&f| g.filters[f].sel));
            let (cost, est) = filtered(sum.cost, sum.est, sel, unsorted);
            sum = Sum {
                chain: sum.chain,
                ..Sum::new(cost, est, sum.sorted)
            };
            for x in &mut d {
                x.1 = x.1.min(sum.est.max(1.0));
            }
            for &f in &now {
                self.placed[f] = true;
            }
        }
        self.units.push(Unit {
            sum,
            inputs,
            vars,
            has,
            certain,
            d,
            dom,
            probe: None,
            from: From::Join(p.a as usize, p.b as usize, p.how),
            filters: now,
            held: merge_held(&a.held, &b.held),
        });
        Ok(self.units.len() - 1)
    }

    /// The cost of the plan, when the inputs became one plan.
    fn cost(&self) -> Option<f64> {
        match self.alive.as_slice() {
            [u] => Some(self.units[*u].sum.cost),
            _ => None,
        }
    }

    /// Build the plan tree from the inputs, removing the placed filters from `filters`.
    fn build(self, items: Vec<Vec<Node>>, filters: &mut Vec<Expr>, pl: &Planner) -> Result<Node> {
        #[cfg(test)]
        let _stage = crate::test_alloc::Stage::enter(5);
        let ctx = pl.ctx;
        first_storage(|| self.units.len() * std::mem::size_of::<Option<Node>>());
        let _nodes_held = match &self.held {
            Some(_) => refused(pl.planning_stage(GREEDY_BUILD_PHASE).and_then(|()| {
                pl.planning_scratch(|layout| layout.slots::<Option<Node>>(self.units.len()))
            }))?,
            None => None,
        };
        // the plan keeps the copies of the inputs that filters are placed on
        if let Some(leaf_held) = self.leaf_held {
            pl.retain_planning(leaf_held);
        }
        let mut nodes: Vec<Option<Node>> = Vec::with_capacity(self.units.len());
        let mut leaves = self.leaves.into_iter().zip(items);
        for u in &self.units {
            ctx.check()?;
            let n = match u.from {
                From::Leaf => match leaves.next().unwrap() {
                    ((_, Some(filtered)), _) => filtered,
                    ((at, None), mut opts) => opts.swap_remove(at),
                },
                From::Join(a, b, how) => {
                    let (na, nb) = (nodes[a].take().unwrap(), nodes[b].take().unwrap());
                    let n = build_join(na, nb, how, pl)?;
                    if u.filters.is_empty() {
                        n
                    } else {
                        #[cfg(test)]
                        let _stage = crate::test_alloc::Stage::within(6);
                        let scratch = refused(pl.filter_scratch(
                            &n,
                            u.filters.iter().map(|&f| &filters[f]),
                            true,
                        ))?;
                        let mut copies = Vec::with_capacity(u.filters.len());
                        copies.extend(u.filters.iter().map(|&f| filters[f].clone()));
                        let (n, kept) = pl.filter_held(scratch, n, copies, 0)?;
                        if let Some(kept) = kept {
                            pl.retain_planning(kept);
                        }
                        n
                    }
                }
            };
            debug_assert!(
                n.cost.to_bits() == u.sum.cost.to_bits() && n.est.to_bits() == u.sum.est.to_bits(),
                "greedy summary {:?} differs from its plan ({}, {})",
                u.sum,
                n.cost,
                n.est
            );
            nodes.push(Some(n));
        }
        let mut i = 0;
        filters.retain(|_| {
            let keep = !self.placed[i];
            i += 1;
            keep
        });
        let mut it = self.alive.into_iter();
        let mut acc = nodes[it.next().unwrap()].take().unwrap();
        for u in it {
            let n = nodes[u].take().unwrap();
            debug_assert_eq!(
                shared_vars(&acc, &n).count(),
                0,
                "the plans left by the greedy order share no variable"
            );
            acc = pl.place_filters(join_admitted(acc, n, pl)?, filters)?;
        }
        Ok(acc)
    }
}

// ------------------------------------------------------------------------------
// dynamic program
// ------------------------------------------------------------------------------

/// A plan of the dynamic program: its summary, the inputs it joins (a mask), where its
/// distinct-value estimates start in `dval`, and how it was made (`a` and `b` are the
/// entries it joins, or the input and access path of a leaf).
#[derive(Clone, Copy)]
struct Ent {
    sum: Sum,
    mask: u32,
    d: u32,
    how: How,
    a: u32,
    b: u32,
    probe: Option<ProbeSide>,
}

/// The best plan per first sort variable found so far for a subset.
#[derive(Clone, Copy)]
struct Best {
    cost: f64,
    rows: f64,
    sorted: Option<VarId>,
    /// rows before the filters on top
    est: f64,
    filtered: bool,
    how: How,
    a: u32,
    b: u32,
    /// the join variable with how the join was estimated, when not from distinct values
    model: Option<(u32, JoinModel)>,
    chain: Chain,
}

/// What the program plans with: an input, or a plan of several inputs fixed by an
/// earlier round, with its plans per sort variable.
#[derive(Clone)]
struct Part {
    inputs: u32,
    ents: Vec<u32>,
}

/// The dynamic program over subsets of the inputs.
///
/// A round plans every connected subset of the current parts up to a size. When the
/// whole group fits the budget of splits, one round plans it whole. Otherwise a round
/// plans subsets up to the largest size that fits, the cheapest plan of that size
/// becomes a part, and the next round goes on with fewer parts (iterative dynamic
/// programming, as Kossmann and Stocker describe it).
struct Dp {
    n: usize,
    /// per subset of the inputs: its join variables, the ones every plan of it binds,
    /// the filters it covers (the first 64)
    jm: Vec<u128>,
    cm: Vec<u128>,
    fcov: Vec<u64>,
    /// join variable index → variable
    jvar: Vec<VarId>,
    /// variable → join variable index
    jidx: FxHashMap<VarId, u32>,
    /// filter (the first 64) → the variable an expression cache reads and the inputs
    /// binding it
    cached: Vec<Option<(VarId, u32)>>,
    /// filter (the first 64) → its selectivity measured on a sample
    fsel: Vec<Option<f64>>,
    /// how the joins are estimated when not from distinct values
    models: Models,
    ents: Vec<Ent>,
    dval: Vec<f64>,
    /// beside `dval`, each entry's domain of each join variable it binds (see
    /// [`plan::domain`]), the larger of its inputs'
    ddom: Vec<f64>,
    /// the checked route's credit for the tables over the subsets and the storage of
    /// `ents`, `dval` and `ddom`, dropped after them
    held: Option<Reservation>,
}

/// One round's tables, per subset of the parts.
struct Round {
    parts: Vec<Part>,
    /// the inputs of each subset
    inputs: Vec<u32>,
    conn: Vec<bool>,
    /// the subset is a single input (an index join can probe it)
    leaf: Vec<bool>,
    /// its entries, in `list`
    slot: Vec<(u32, u32)>,
    list: Vec<u32>,
    minc: Vec<f64>,
    /// splits per subset size
    splits: Vec<u64>,
    /// the checked route's credit for the round's tables and the storage of `list`,
    /// dropped after them
    held: Option<Reservation>,
}

#[inline]
fn rank(set: u128, bit: u32) -> u32 {
    (set & ((1u128 << bit) - 1)).count_ones()
}

/// Whether the parts `m` are connected: the parts reached from the lowest one along
/// `nb` (the neighbours of each subset) are all of `m`.
fn connected(m: u32, nb: &[u32]) -> bool {
    let mut c = m.isolate_lowest_one();
    loop {
        let next = (c | nb[c as usize]) & m;
        if next == c {
            return c == m;
        }
        c = next;
    }
}

impl Dp {
    /// The tables over the inputs' subsets, or `None` when the program cannot track
    /// the group (too many inputs or join variables).
    fn prepare(g: &Group, items: &[Vec<Node>], pl: &Planner) -> Result<Option<Dp>> {
        #[cfg(test)]
        let _stage = crate::test_alloc::Stage::enter(2);
        let n = items.len();
        if n > DP_MAX_ITEMS || g.jvars.len() > DP_MAX_JOIN_VARS {
            return Ok(None);
        }
        let full: u32 = (1u32 << n) - 1;
        let size = 1usize << n;
        let fcount = g.filters.len().min(64);
        // The tables below are sized from the group before any is allocated.
        let held = pl.planning_stage(PREPARE_PHASE).and_then(|()| {
            pl.planning_scratch(|layout| {
                layout.slots::<VarId>(g.jvars.len())?;
                layout.slots::<u32>(g.vars.len())?;
                layout.slots::<u128>(2 * n)?;
                layout.slots::<&FilterInfo>(fcount)?;
                layout.slots::<u128>(2 * size)?;
                layout.slots::<u64>(size)?;
                layout.slots::<Option<(VarId, u32)>>(fcount)?;
                layout.map::<VarId, u32>(g.jvars.len())?;
                layout.slots::<Option<f64>>(fcount)?;
                g.models.measure(layout)
            })
        });
        let held = refused(held)?;
        let jvar: Vec<VarId> = g.jvars.iter().map(|&l| g.vars[l as usize]).collect();
        let mut var_items = vec![0u32; g.vars.len()];
        let mut jb = vec![0u128; n];
        let mut cb = vec![0u128; n];
        for (i, iv) in g.item_vars.iter().enumerate() {
            for &l in iv {
                var_items[l as usize] |= 1 << i;
                let j = g.join[l as usize];
                if j != u32::MAX {
                    jb[i] |= 1u128 << j;
                    if items[i][0].certain.contains(&g.vars[l as usize]) {
                        cb[i] |= 1u128 << j;
                    }
                }
            }
        }
        let fl: Vec<&FilterInfo> = g.filters.iter().take(64).collect();
        let mut jm = vec![0u128; size];
        let mut cm = vec![0u128; size];
        let mut fcov = vec![0u64; size];
        for m in 1..=full {
            let low = m.isolate_lowest_one();
            let i = low.trailing_zeros() as usize;
            let r = (m ^ low) as usize;
            jm[m as usize] = jm[r] | jb[i];
            cm[m as usize] = cm[r] | cb[i];
            let mut f = 0u64;
            for (x, fi) in fl.iter().enumerate() {
                if fi.placeable && fi.vars.iter().all(|&l| var_items[l as usize] & m != 0) {
                    f |= 1 << x;
                }
            }
            fcov[m as usize] = f;
        }
        let cached = fl
            .iter()
            .map(|fi| {
                fi.cached
                    .map(|l| (g.vars[l as usize], var_items[l as usize]))
            })
            .collect();
        let jidx = jvar
            .iter()
            .enumerate()
            .map(|(j, &v)| (v, j as u32))
            .collect();
        let fsel = fl.iter().map(|fi| fi.sel).collect();
        Ok(Some(Dp {
            n,
            jm,
            cm,
            fcov,
            jvar,
            jidx,
            cached,
            fsel,
            models: g.models.clone(),
            ents: Vec::new(),
            dval: Vec::new(),
            ddom: Vec::new(),
            held,
        }))
    }

    /// The join variables of the inputs `m` that inputs outside them share.
    fn bnd(&self, m: u32) -> u128 {
        let full = (1u32 << self.n) - 1;
        self.jm[m as usize] & self.jm[(full ^ m) as usize]
    }

    /// The tables of a round over `parts`.
    fn round(&self, parts: Vec<Part>, pl: &Planner) -> Result<Round> {
        #[cfg(test)]
        DP_ROUNDS.with(|n| n.set(n.get() + 1));
        let k = parts.len();
        let size = 1usize << k;
        let listed: usize = parts.iter().map(|p| p.ents.len()).sum();
        first_storage(|| 4 * size);
        // The tables below are sized from the parts before any is allocated. `list`
        // starts with the parts' entries and grows as the round plans subsets.
        let mut held = refused(pl.planning_stage(ROUND_PHASE).and_then(|()| {
            pl.planning_scratch(|layout| {
                layout.slots::<u32>(2 * size + k + listed)?;
                layout.slots::<bool>(2 * size)?;
                layout.slots::<u64>(k + 1)?;
                layout.slots::<(u32, u32)>(size)?;
                layout.slots::<f64>(size)
            })
        }))?;
        let mut inputs = vec![0u32; size];
        let mut nb = vec![0u32; size];
        let mut conn = vec![false; size];
        let mut leaf = vec![false; size];
        let mut splits = vec![0u64; k + 1];
        let adj: Vec<u32> = (0..k)
            .map(|i| {
                let ji = self.jm[parts[i].inputs as usize];
                (0..k)
                    .filter(|&j| j != i && ji & self.jm[parts[j].inputs as usize] != 0)
                    .fold(0, |a, j| a | 1 << j)
            })
            .collect();
        for m in 1..size as u32 {
            let low = m.isolate_lowest_one();
            let i = low.trailing_zeros() as usize;
            let r = (m ^ low) as usize;
            inputs[m as usize] = inputs[r] | parts[i].inputs;
            nb[m as usize] = nb[r] | adj[i];
            if connected(m, &nb) {
                conn[m as usize] = true;
                let c = m.count_ones();
                splits[c as usize] += (1u64 << (c - 1)) - 1;
            }
        }
        // The neighbours serve only to find the connected subsets, so their storage
        // and its credit go before the round is planned.
        drop((nb, adj));
        if let Some(held) = held.as_mut() {
            held.release_to(held.bytes() - (4 * (size + k)) as u64);
        }
        let mut list = Vec::with_capacity(listed);
        let mut slot = vec![(0u32, 0u32); size];
        let mut minc = vec![f64::INFINITY; size];
        for (i, p) in parts.iter().enumerate() {
            let m = 1usize << i;
            leaf[m] = p.inputs.is_power_of_two();
            slot[m] = (list.len() as u32, p.ents.len() as u32);
            list.extend(&p.ents);
            for &e in &p.ents {
                minc[m] = minc[m].min(self.ents[e as usize].sum.cost);
            }
        }
        Ok(Round {
            parts,
            inputs,
            conn,
            leaf,
            slot,
            list,
            minc,
            splits,
            held,
        })
    }

    /// The entry among `ents` of least cost (the first of equal ones).
    fn cheapest(&self, ents: &[u32]) -> Option<u32> {
        let mut best: Option<u32> = None;
        for &e in ents {
            if best.is_none_or(|b| self.ents[e as usize].sum.cost < self.ents[b as usize].sum.cost)
            {
                best = Some(e);
            }
        }
        best
    }

    /// Run the program, dropping plans that cost more than `bound`. Returns the entry of
    /// the cheapest complete plan, if one costs at most `bound`.
    fn run(
        &mut self,
        items: &[Vec<Node>],
        filters: &[Expr],
        bound: Option<f64>,
        pl: &Planner,
    ) -> Result<Option<u32>> {
        let ctx = pl.ctx;
        #[cfg(test)]
        let _stage = crate::test_alloc::Stage::enter(3);
        let ub = bound.unwrap_or(f64::INFINITY);
        let batched = plan::Costing::of(ctx);
        let fvars = dp_filter_vars(filters, pl)?;
        let fvars = &fvars.vars[..];
        let leaves: usize = items.iter().map(Vec::len).sum();
        // On the checked route, the parts' storage is admitted before it is allocated,
        // and the credit is dropped after the parts.
        first_storage(|| items.len() * std::mem::size_of::<Part>());
        let mut parts_held = refused(pl.planning_stage(PARTS_PHASE).and_then(|()| {
            pl.planning_scratch(|layout| {
                layout.slots::<Part>(items.len())?;
                layout.slots::<u32>(leaves)?;
                layout.slots::<bool>(items.iter().map(Vec::len).max().unwrap_or(0))
            })
        }))?;
        if self.held.is_some() {
            let vals: usize = (0..items.len())
                .map(|i| items[i].len() * self.bnd(1 << i).count_ones() as usize)
                .sum();
            make_room(self.held.as_mut(), &mut self.ents, leaves)?;
            make_room(self.held.as_mut(), &mut self.dval, vals)?;
            make_room(self.held.as_mut(), &mut self.ddom, vals)?;
        }
        // Every plan's first sort variable is a join variable or a leaf's (see
        // [`joins`]), which bounds the plans kept per subset.
        let keys = 1 + self.jvar.len() + leaves;
        let mut keep = Vec::with_capacity(if parts_held.is_some() {
            items.iter().map(Vec::len).max().unwrap_or(0)
        } else {
            0
        });
        // the inputs: every access path with the filters it binds
        let mut parts = Vec::with_capacity(items.len());
        for (i, opts) in items.iter().enumerate() {
            let m = 1u32 << i;
            let bnd = self.bnd(m);
            let mut ents = Vec::with_capacity(opts.len());
            for (k, o) in opts.iter().enumerate() {
                // copied only to place filters on, since an input can be a whole subtree.
                // On the checked route the copy is admitted before it is made, and the
                // credit is dropped after it.
                let _copy;
                let _kept;
                let filtered;
                let o = if fvars
                    .iter()
                    .take(64)
                    .any(|fv| !fv.is_empty() && fv.iter().all(|v| o.vars.contains(v)))
                {
                    _copy = if parts_held.is_some() {
                        refused(
                            pl.planning_stage(LEAF_PHASE)
                                .and_then(|()| pl.planning_scratch(|layout| layout.node(o))),
                        )?
                    } else {
                        None
                    };
                    let placed = place_masked(pl, o.clone(), 0, filters, fvars)?;
                    _kept = leaf_kept(placed.2);
                    filtered = placed.0;
                    &filtered
                } else {
                    o
                };
                let d = self.dval.len() as u32;
                let mut rest = bnd;
                while rest != 0 {
                    let j = rest.trailing_zeros();
                    rest &= rest - 1;
                    self.dval.push(o.d(self.jvar[j as usize]));
                    self.ddom.push(plan::domain(ctx, o, self.jvar[j as usize]));
                }
                ents.push(self.ents.len() as u32);
                self.ents.push(Ent {
                    sum: Sum::of(o),
                    mask: m,
                    d,
                    how: How::Hash,
                    a: i as u32,
                    b: k as u32,
                    probe: if batched.index_joins {
                        indexjoin::probe_side(o, ctx)
                    } else {
                        None
                    },
                });
            }
            self.drop_dominated(m, &mut ents, &mut keep);
            parts.push(Part { inputs: m, ents });
        }
        debug_assert!(self.ents.len() <= leaves);
        drop(keep);
        let mut rounds = false;
        loop {
            let mut r = self.round(parts, pl)?;
            let k = r.parts.len();
            let total: u64 = r.splits.iter().sum();
            if !rounds && total > DP_MAX_SPLITS {
                // planned in rounds from here on, where the parts keep only the plans
                // that can matter later
                rounds = true;
                if let Some(held) = parts_held.as_mut() {
                    first_storage(|| part_bytes(k) as usize);
                    refused(pl.planning_stage(PARTS_PHASE))?;
                    refused(held.grow_by(part_bytes(k)))?;
                }
                parts = Vec::with_capacity(k);
                parts.extend(r.parts.into_iter().map(|p| self.useful(p)));
                continue;
            }
            let budget = if rounds { ROUND_SPLITS } else { DP_MAX_SPLITS };
            // the largest subsets whose splits fit the budget
            let mut size = k;
            if total > budget {
                let mut sum = 0;
                size = 2;
                for (s, &w) in r.splits.iter().enumerate().skip(2) {
                    sum += w;
                    if sum > budget {
                        break;
                    }
                    size = s;
                }
            }
            self.plan_round(&mut r, size, keys, ub, batched, ctx)?;
            if size == k {
                let (s, l) = r.slot[(1usize << k) - 1];
                return Ok(self.cheapest(&r.list[s as usize..(s + l) as usize]));
            }
            // the cheapest plan of `size` parts becomes a part
            let mut pick: Option<(u32, u32)> = None;
            for m in 1..(1u32 << k) {
                if m.count_ones() as usize != size || !r.conn[m as usize] {
                    continue;
                }
                let (s, l) = r.slot[m as usize];
                if let Some(e) = self.cheapest(&r.list[s as usize..(s + l) as usize])
                    && pick.is_none_or(|(_, b)| {
                        self.ents[e as usize].sum.cost < self.ents[b as usize].sum.cost
                    })
                {
                    pick = Some((m, e));
                }
            }
            let Some((m, _)) = pick else {
                return Ok(None);
            };
            let (s, l) = r.slot[m as usize];
            let rest = k - size + 1;
            if let Some(held) = parts_held.as_mut() {
                // the merged part's entries are copied first
                first_storage(|| 4 * l as usize);
                refused(pl.planning_stage(PARTS_PHASE))?;
                refused(held.grow_by(part_bytes(rest) + 4 * l as u64))?;
            }
            let merged = self.useful(Part {
                inputs: r.inputs[m as usize],
                ents: r.list[s as usize..(s + l) as usize].to_vec(),
            });
            parts = Vec::with_capacity(rest);
            parts.extend(
                r.parts
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| m & (1 << i) == 0)
                    .map(|(_, p)| p),
            );
            parts.push(merged);
        }
    }

    /// The part with its cheapest plan and those sorted on a variable that later joins
    /// read.
    /// The entries are kept in place, in their order.
    fn useful(&self, mut p: Part) -> Part {
        let cheapest = self.cheapest(&p.ents);
        let bnd = self.bnd(p.inputs);
        p.ents.retain(|&e| {
            Some(e) == cheapest
                || self.ents[e as usize].sum.sorted.is_some_and(|v| {
                    self.jvar
                        .iter()
                        .position(|&x| x == v)
                        .is_some_and(|j| bnd >> j & 1 == 1)
                })
        });
        p
    }

    /// Plan every connected subset of the round's parts of at most `size` parts.
    /// `keys` bounds the plans kept per subset.
    fn plan_round(
        &mut self,
        r: &mut Round,
        size: usize,
        keys: usize,
        ub: f64,
        batched: plan::Costing,
        ctx: &Ctx,
    ) -> Result<()> {
        let k = r.parts.len();
        // On the checked route, the buffers reused across the round's subsets are
        // admitted at their bounds before they are allocated, and the credit is dropped
        // after them.
        let jv = self.jvar.len();
        let held = match self.held.as_ref() {
            Some(held) => {
                let preparation = held.preparation();
                first_storage(|| keys * std::mem::size_of::<Best>());
                let reserve = || {
                    preparation.stage(SCRATCH_PHASE)?;
                    let mut layout = super::checked_layout::Layout::new(preparation);
                    layout.slots::<Best>(keys)?;
                    Scratch::measure(&mut layout, jv, self.cached.len(), keys)?;
                    preparation.reserve(layout.bytes())
                };
                Some(refused(reserve())?)
            }
            None => None,
        };
        let (mut cur, mut scratch) = if held.is_some() {
            (
                Vec::with_capacity(keys),
                Scratch::with_capacity(jv, self.cached.len(), keys),
            )
        } else {
            (Vec::new(), Scratch::default())
        };
        for m in 1..(1u32 << k) {
            let c = m.count_ones() as usize;
            if c < 2 || c > size || !r.conn[m as usize] {
                continue;
            }
            ctx.check()?;
            cur.clear();
            let hb = 1u32 << (31 - m.leading_zeros());
            let low = m ^ hb;
            // splits (sub, rest) with sub < rest, in the exhaustive program's order
            let mut sub = low;
            while sub != 0 {
                self.split(r, m, sub, m ^ sub, ub, batched, ctx, &mut cur, &mut scratch);
                sub = (sub - 1) & low;
            }
            debug_assert!(held.is_none() || cur.capacity() == keys);
            self.finish(r, m, &mut cur, &mut scratch)?;
        }
        debug_assert!(held.is_none() || scratch.within(jv, self.cached.len(), keys));
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn split(
        &self,
        r: &Round,
        m: u32,
        sub: u32,
        rest: u32,
        ub: f64,
        batched: plan::Costing,
        ctx: &Ctx,
        cur: &mut Vec<Best>,
        sc: &mut Scratch,
    ) {
        let (sa, la) = r.slot[sub as usize];
        let (sb, lb) = r.slot[rest as usize];
        if la == 0 || lb == 0 {
            return;
        }
        let (ia, ib, im) = (
            r.inputs[sub as usize],
            r.inputs[rest as usize],
            r.inputs[m as usize],
        );
        let sh = self.jm[ia as usize] & self.jm[ib as usize];
        if sh == 0 {
            return;
        }
        let (leaf_a, leaf_b) = (r.leaf[sub as usize], r.leaf[rest as usize]);
        let floor = |leaf: bool, c: f64| if leaf { 0.0 } else { c };
        if floor(leaf_a, r.minc[sub as usize]) + floor(leaf_b, r.minc[rest as usize]) > ub {
            return;
        }
        // a join on one variable: from probed values or characteristic sets
        let model = if sh.count_ones() == 1 {
            let j = sh.trailing_zeros();
            #[cfg(test)]
            let _stage = crate::test_alloc::Stage::enter(8);
            self.models
                .of(ctx, self.jvar[j as usize], j, bits_of(ia), bits_of(ib))
                .map(|m| (j, m))
        } else {
            None
        };
        let (ba, bb) = (self.bnd(ia), self.bnd(ib));
        let (ca, cb) = (self.cm[ia as usize], self.cm[ib as usize]);
        sc.shared.clear();
        sc.pos.clear();
        let mut bits = sh;
        while bits != 0 {
            let j = bits.trailing_zeros();
            bits &= bits - 1;
            sc.shared.push(Shared {
                v: self.jvar[j as usize],
                j,
                da: 0.0,
                doma: 0.0,
                domb: 0.0,
                db: 0.0,
                ca: ca >> j & 1 == 1,
                cb: cb >> j & 1 == 1,
            });
            sc.pos.push((rank(ba, j), rank(bb, j)));
        }
        let newf = self.fcov[im as usize] & !(self.fcov[ia as usize] | self.fcov[ib as usize]);
        let nf = newf.count_ones();
        let mut bits = newf;
        let sel = super::sample::combine(std::iter::from_fn(|| {
            (bits != 0).then(|| {
                let f = bits.trailing_zeros();
                bits &= bits - 1;
                self.fsel[f as usize]
            })
        }));
        sc.cached.clear();
        let mut fb = newf;
        while fb != 0 {
            let f = fb.trailing_zeros();
            fb &= fb - 1;
            if let Some((v, items)) = self.cached[f as usize]
                && items & im != 0
            {
                sc.cached.push(v);
            }
        }
        let capped = ctx.opt.key_backoff
            && sc.shared.len() > 1
            && self.models.one_binds(bits_of(ia), &sc.shared)
            && self.models.one_binds(bits_of(ib), &sc.shared);
        let Scratch {
            shared,
            pos,
            cached,
            cands,
            ..
        } = sc;
        for &ea in &r.list[sa as usize..(sa + la) as usize] {
            let a = &self.ents[ea as usize];
            for &eb in &r.list[sb as usize..(sb + lb) as usize] {
                let b = &self.ents[eb as usize];
                for (s, &(pa, pb)) in shared.iter_mut().zip(pos.iter()) {
                    s.da = self.dval[(a.d + pa) as usize];
                    s.db = self.dval[(b.d + pb) as usize];
                    s.doma = self.ddom[(a.d + pa) as usize];
                    s.domb = self.ddom[(b.d + pb) as usize];
                }
                let est = pair_est(
                    &a.sum,
                    &b.sum,
                    shared,
                    model.map(|(_, m)| m),
                    ctx.opt.key_backoff,
                    capped,
                );
                let probe_a = batched.index_joins && a.probe.is_some();
                let probe_b = batched.index_joins && b.probe.is_some();
                let mut floor = pair_floor(&a.sum, probe_a, &b.sum, probe_b, est);
                if nf > 0 {
                    floor += est;
                }
                if floor > ub {
                    continue;
                }
                let unsorted = |key: Option<VarId>| cached.iter().any(|&v| key != Some(v));
                let after = |cost: f64, key: Option<VarId>| {
                    if nf > 0 {
                        filtered(cost, est, sel, unsorted(key))
                    } else {
                        (cost, est)
                    }
                };
                // a join can only replace a cheaper plan with its sort variable
                let mut keep = |lb: f64, key: Option<VarId>| {
                    let lb = after(lb, key).0;
                    lb <= ub
                        && cur
                            .iter()
                            .find(|x| x.sorted == key)
                            .is_none_or(|x| x.cost > lb)
                };
                joins(
                    &a.sum,
                    a.probe.as_ref(),
                    &b.sum,
                    b.probe.as_ref(),
                    shared,
                    est,
                    batched,
                    &mut keep,
                    cands,
                );
                for c in cands.iter() {
                    let (cost, e) = after(c.cost, c.sorted);
                    if cost > ub {
                        continue;
                    }
                    let new = Best {
                        cost,
                        rows: e,
                        sorted: c.sorted,
                        est,
                        filtered: nf > 0,
                        how: c.how,
                        a: ea,
                        b: eb,
                        model,
                        chain: c.chain,
                    };
                    match cur.iter_mut().find(|x| x.sorted == c.sorted) {
                        Some(x) if x.cost <= cost => {}
                        Some(x) => *x = new,
                        None => cur.push(new),
                    }
                }
            }
        }
    }

    /// Store the best plans of subset `m`, in order of their sort variable.
    fn finish(&mut self, r: &mut Round, m: u32, cur: &mut [Best], sc: &mut Scratch) -> Result<()> {
        cur.sort_unstable_by_key(|b| b.sorted);
        let start = r.list.len() as u32;
        let bnd = self.bnd(r.inputs[m as usize]);
        // On the checked route, the tables grow only by admitted steps.
        let vals = cur.len() * bnd.count_ones() as usize;
        make_room(self.held.as_mut(), &mut self.ents, cur.len())?;
        make_room(self.held.as_mut(), &mut self.dval, vals)?;
        make_room(self.held.as_mut(), &mut self.ddom, vals)?;
        make_room(r.held.as_mut(), &mut r.list, cur.len())?;
        for b in cur.iter() {
            let (ea, eb) = (self.ents[b.a as usize], self.ents[b.b as usize]);
            let (ja, jbm) = (self.jm[ea.mask as usize], self.jm[eb.mask as usize]);
            let (ba, bb) = (self.bnd(ea.mask), self.bnd(eb.mask));
            let d = self.dval.len() as u32;
            let mut rest = bnd;
            while rest != 0 {
                let j = rest.trailing_zeros();
                rest &= rest - 1;
                let da = (ja >> j & 1 == 1).then(|| self.dval[(ea.d + rank(ba, j)) as usize]);
                let db = (jbm >> j & 1 == 1).then(|| self.dval[(eb.d + rank(bb, j)) as usize]);
                let x = match (da, db) {
                    (Some(x), Some(y)) => match b.model {
                        Some((s, m)) if s == j => m.distinct(x, y),
                        _ => x.min(y),
                    },
                    (Some(x), None) | (None, Some(x)) => x,
                    (None, None) => unreachable!("a join variable of neither input"),
                };
                let mut x = x.min(b.est).max(1.0);
                if b.filtered {
                    x = x.min(b.rows.max(1.0));
                }
                self.dval.push(x);
                let doma = (ja >> j & 1 == 1).then(|| self.ddom[(ea.d + rank(ba, j)) as usize]);
                let domb = (jbm >> j & 1 == 1).then(|| self.ddom[(eb.d + rank(bb, j)) as usize]);
                self.ddom.push(doma.unwrap_or(0.0).max(domb.unwrap_or(0.0)));
            }
            self.ents.push(Ent {
                sum: Sum {
                    chain: b.chain,
                    ..Sum::new(b.cost, b.rows, b.sorted)
                },
                mask: ea.mask | eb.mask,
                d,
                how: b.how,
                a: b.a,
                b: b.b,
                probe: None,
            });
        }
        let first = self.ents.len() as u32 - cur.len() as u32;
        let Scratch { ids, keep, .. } = sc;
        ids.clear();
        ids.extend(first..self.ents.len() as u32);
        self.drop_dominated(r.inputs[m as usize], ids, keep);
        for &e in ids.iter() {
            r.minc[m as usize] = r.minc[m as usize].min(self.ents[e as usize].sum.cost);
        }
        r.list.extend(ids.iter());
        r.slot[m as usize] = (start, ids.len() as u32);
        Ok(())
    }

    /// Drop from `ids`, plans of the inputs `inputs`, those whose sort variable no later
    /// join or filter reads and that another plan matches in rows and distinct-value
    /// estimates at no more cost. Every plan made from a dropped one has a counterpart
    /// made from the other that costs no more, with the same estimates.
    /// `keep` is a buffer reused across calls.
    fn drop_dominated(&self, inputs: u32, ids: &mut Vec<u32>, keep: &mut Vec<bool>) {
        if ids.len() < 2 {
            return;
        }
        let bnd = self.bnd(inputs);
        let len = bnd.count_ones();
        let covered = self.fcov[inputs as usize];
        let dead = |key: Option<VarId>| match key {
            None => true,
            Some(v) => {
                !self.jidx.get(&v).is_some_and(|&j| bnd >> j & 1 == 1)
                    && !self
                        .cached
                        .iter()
                        .enumerate()
                        .any(|(f, c)| covered >> f & 1 == 0 && c.is_some_and(|(x, _)| x == v))
            }
        };
        let same = |x: &Ent, y: &Ent| {
            x.sum.est.to_bits() == y.sum.est.to_bits()
                && x.sum.chain.map(|(k, n)| (k, n.to_bits()))
                    == y.sum.chain.map(|(k, n)| (k, n.to_bits()))
                && (0..len).all(|i| {
                    self.dval[(x.d + i) as usize].to_bits()
                        == self.dval[(y.d + i) as usize].to_bits()
                })
        };
        keep.clear();
        keep.resize(ids.len(), true);
        for i in 0..ids.len() {
            let x = &self.ents[ids[i] as usize];
            if !dead(x.sum.sorted) {
                continue;
            }
            keep[i] = !(0..ids.len()).any(|j| {
                let y = &self.ents[ids[j] as usize];
                j != i
                    && keep[j]
                    && y.sum.cost <= x.sum.cost
                    && (y.sum.cost < x.sum.cost || j < i || !dead(y.sum.sorted))
                    && same(x, y)
            });
        }
        let mut k = 0;
        ids.retain(|_| {
            k += 1;
            keep[k - 1]
        });
    }

    /// Build the plan of entry `root`, with the mask of the filters it placed.
    /// Each input is used once, and is moved out of `items` rather than copied, since it
    /// can be a whole subtree.
    fn build(
        &self,
        root: u32,
        items: &mut [Vec<Node>],
        filters: &[Expr],
        pl: &Planner,
    ) -> Result<(Node, u64)> {
        #[cfg(test)]
        let _stage = crate::test_alloc::Stage::enter(4);
        let ctx = pl.ctx;
        let fvars = dp_filter_vars(filters, pl)?;
        let fvars = &fvars.vars[..];
        // The plan joins each of the n inputs once, so it has 2n - 1 entries, and at
        // most n built plans wait for their parent. On the checked route that storage is
        // admitted before it is allocated, and the credit is dropped after it.
        first_storage(|| (2 * self.n - 1) * std::mem::size_of::<u32>());
        let held = refused(pl.planning_stage(BUILD_PHASE).and_then(|()| {
            pl.planning_scratch(|layout| {
                layout.slots::<u32>(2 * self.n - 1)?;
                layout.map::<u32, (Node, u64)>(self.n)
            })
        }))?;
        // the entries of the plan, children before parents
        let mut need = Vec::with_capacity(if held.is_some() { 2 * self.n - 1 } else { 1 });
        need.push(root);
        let mut i = 0;
        while i < need.len() {
            let e = self.ents[need[i] as usize];
            if !e.mask.is_power_of_two() {
                need.push(e.a);
                need.push(e.b);
            }
            i += 1;
        }
        debug_assert!(need.len() < 2 * self.n);
        need.sort_unstable();
        let mut built: FxHashMap<u32, (Node, u64)> = if held.is_some() {
            FxHashMap::with_capacity_and_hasher(self.n, Default::default())
        } else {
            FxHashMap::default()
        };
        #[cfg(debug_assertions)]
        let capacity = built.capacity();
        for &x in &need {
            ctx.check()?;
            let e = self.ents[x as usize];
            let (n, f, kept) = if e.mask.is_power_of_two() {
                let leaf = std::mem::replace(&mut items[e.a as usize][e.b as usize], Node::unit());
                place_masked(pl, leaf, 0, filters, fvars)?
            } else {
                let (na, fa) = built.remove(&e.a).unwrap();
                let (nb, fb) = built.remove(&e.b).unwrap();
                place_masked(pl, build_join(na, nb, e.how, pl)?, fa | fb, filters, fvars)?
            };
            if let Some(kept) = kept {
                pl.retain_planning(kept);
            }
            debug_assert!(
                n.cost.to_bits() == e.sum.cost.to_bits() && n.est.to_bits() == e.sum.est.to_bits(),
                "summary {:?} differs from its plan ({}, {})",
                e.sum,
                n.cost,
                n.est
            );
            built.insert(x, (n, f));
        }
        #[cfg(debug_assertions)]
        debug_assert!(held.is_none() || built.capacity() == capacity);
        Ok(built.remove(&root).unwrap())
    }
}

/// Buffers reused across splits.
#[derive(Default)]
struct Scratch {
    shared: Vec<Shared>,
    /// each shared variable's place among each side's distinct-value estimates
    pos: Vec<(u32, u32)>,
    /// the variables of the filters placed on a join that an expression cache reads
    cached: Vec<VarId>,
    cands: Vec<Cand>,
    /// a subset's new entries, and which of them [`Dp::drop_dominated`] keeps
    ids: Vec<u32>,
    keep: Vec<bool>,
}

impl Scratch {
    /// The buffers' storage at their bounds: `jv` join variables, `cached` filters an
    /// expression cache reads, and `keys` plans per subset. A join offers at most a
    /// merge join per shared variable, a hash join and two index joins (see [`joins`]).
    fn measure(
        layout: &mut super::checked_layout::Layout<'_>,
        jv: usize,
        cached: usize,
        keys: usize,
    ) -> Result<()> {
        layout.slots::<Shared>(jv)?;
        layout.slots::<(u32, u32)>(jv)?;
        layout.slots::<VarId>(cached)?;
        layout.slots::<Cand>(jv + 3)?;
        layout.slots::<u32>(keys)?;
        layout.slots::<bool>(keys)
    }

    fn with_capacity(jv: usize, cached: usize, keys: usize) -> Scratch {
        Scratch {
            shared: Vec::with_capacity(jv),
            pos: Vec::with_capacity(jv),
            cached: Vec::with_capacity(cached),
            cands: Vec::with_capacity(jv + 3),
            ids: Vec::with_capacity(keys),
            keep: Vec::with_capacity(keys),
        }
    }

    /// Whether no buffer grew past the bounds it was made with.
    fn within(&self, jv: usize, cached: usize, keys: usize) -> bool {
        self.shared.capacity() == jv
            && self.pos.capacity() == jv
            && self.cached.capacity() == cached
            && self.cands.capacity() == jv + 3
            && self.ids.capacity() == keys
            && self.keep.capacity() == keys
    }
}

/// The phases a checked plan reports before it admits the program's tables, a round's
/// tables, and the growth of either.
pub(super) const PREPARE_PHASE: &str = "reserving join order tables";
/// The phases a checked plan reports before it admits the group's variables and
/// filters, and then its join models.
pub(super) const GROUP_PHASE: &str = "reserving join order group";
pub(super) const MODELS_PHASE: &str = "reserving join order models";
pub(super) const ROUND_PHASE: &str = "reserving join order round";
pub(super) const GROW_PHASE: &str = "growing join order table";
/// The phases a checked plan reports before it admits the filters' variables, the
/// parts, a round's reused buffers, the rebuilt plan's lists, and the copy of an input
/// that filters are placed on to cost it.
pub(super) const FILTER_VARS_PHASE: &str = "reserving join order filter variables";
pub(super) const PARTS_PHASE: &str = "reserving join order parts";
pub(super) const SCRATCH_PHASE: &str = "reserving join order round scratch";
pub(super) const BUILD_PHASE: &str = "reserving join order rebuild";
pub(super) const LEAF_PHASE: &str = "copying join order input";
/// The phases a checked plan reports before it admits the greedy ordering's tables,
/// each of its plans, the copy of an input that filters are placed on, the growth of
/// its heap of joins, and the list of plans it rebuilds.
pub(super) const GREEDY_PHASE: &str = "reserving greedy join order";
pub(super) const GREEDY_UNIT_PHASE: &str = "reserving greedy join order plan";
pub(super) const GREEDY_LEAF_PHASE: &str = "copying greedy join order input";
pub(super) const GREEDY_GROW_PHASE: &str = "growing greedy join order heap";
pub(super) const GREEDY_BUILD_PHASE: &str = "reserving greedy join order rebuild";
/// The phase a checked plan reports before it admits a join of its rebuilt plan.
pub(super) const JOIN_BUILD_PHASE: &str = "reserving join order join";

/// Record, for the tests of the program's admission, the bytes of the first storage
/// that the next admission covers.
#[inline]
fn first_storage(_bytes: impl FnOnce() -> usize) {
    #[cfg(test)]
    DP_GROWTH.with(|n| n.set(_bytes()));
}

/// The storage of `count` parts.
fn part_bytes(count: usize) -> u64 {
    (count * std::mem::size_of::<Part>()) as u64
}

/// `held`, counted on this thread when refused, for the tests of the program's
/// admission.
fn refused<T>(held: Result<T>) -> Result<T> {
    #[cfg(test)]
    if held.is_err() {
        DP_REFUSALS.with(|n| n.set(n.get() + 1));
    }
    held
}

/// Make room in `v` for `need` more values. On the checked route `credit`, which holds
/// `v`'s storage among others, admits the larger storage before it is allocated.
#[inline]
fn make_room<T>(credit: Option<&mut Reservation>, v: &mut Vec<T>, need: usize) -> Result<()> {
    match credit {
        Some(credit) if v.capacity() - v.len() < need => grow_admitted(credit, v, need),
        _ => Ok(()),
    }
}

/// Grow `v` to at least twice its storage, holding credit for the old storage and the
/// new while the values move, and then for the new storage only.
#[cold]
fn grow_admitted<T>(credit: &mut Reservation, v: &mut Vec<T>, need: usize) -> Result<()> {
    grow_admitted_in(credit, v, need, GROW_PHASE)
}

/// Storage that [`grow_admitted_in`] grows.
trait Room {
    type Item;
    fn len(&self) -> usize;
    fn capacity(&self) -> usize;
    fn try_reserve_exact(
        &mut self,
        more: usize,
    ) -> std::result::Result<(), std::collections::TryReserveError>;
}

impl<T> Room for Vec<T> {
    type Item = T;
    fn len(&self) -> usize {
        Vec::len(self)
    }
    fn capacity(&self) -> usize {
        Vec::capacity(self)
    }
    fn try_reserve_exact(
        &mut self,
        more: usize,
    ) -> std::result::Result<(), std::collections::TryReserveError> {
        Vec::try_reserve_exact(self, more)
    }
}

impl<T: Ord> Room for BinaryHeap<T> {
    type Item = T;
    fn len(&self) -> usize {
        BinaryHeap::len(self)
    }
    fn capacity(&self) -> usize {
        BinaryHeap::capacity(self)
    }
    fn try_reserve_exact(
        &mut self,
        more: usize,
    ) -> std::result::Result<(), std::collections::TryReserveError> {
        BinaryHeap::try_reserve_exact(self, more)
    }
}

/// [`grow_admitted`] for any storage, reporting `phase` before it admits the growth.
#[cold]
fn grow_admitted_in<S: Room>(
    credit: &mut Reservation,
    v: &mut S,
    need: usize,
    phase: &str,
) -> Result<()> {
    let preparation = credit.preparation().clone();
    let size = std::mem::size_of::<S::Item>();
    let bytes = |n: usize| {
        n.checked_mul(size)
            .and_then(|b| u64::try_from(b).ok())
            .ok_or_else(|| preparation.exhausted())
    };
    let want = v
        .len()
        .checked_add(need)
        .ok_or_else(|| preparation.exhausted())?;
    let capacity = want.max(v.capacity().saturating_mul(2)).max(4);
    let (old, new) = (bytes(v.capacity())?, bytes(capacity)?);
    #[cfg(test)]
    DP_GROWTH.with(|n| n.set(new as usize));
    refused(preparation.stage(phase))?;
    refused(credit.grow_by(new))?;
    v.try_reserve_exact(capacity - v.len())
        .map_err(|_| preparation.exhausted())?;
    if size != 0 && v.capacity() != capacity {
        return Err(preparation.exhausted());
    }
    credit.release_to(credit.bytes() - old);
    Ok(())
}

/// The variables of each filter as the exhaustive program reads them (EXISTS never
/// placed), with the checked route's credit for them, dropped after them.
struct FilterVars {
    vars: Vec<Vec<VarId>>,
    _held: Option<Reservation>,
}

/// The variables of each filter (see [`FilterVars`]). On the checked route their
/// storage is sized by a walk over the filters that allocates nothing, and admitted
/// before it is allocated.
fn dp_filter_vars(filters: &[Expr], pl: &Planner) -> Result<FilterVars> {
    let count = var_count;
    first_storage(|| filters.len() * std::mem::size_of::<Vec<VarId>>());
    let held = refused(pl.planning_stage(FILTER_VARS_PHASE).and_then(|()| {
        pl.planning_scratch(|layout| {
            layout.slots::<Vec<VarId>>(filters.len())?;
            for f in filters {
                layout.slots::<VarId>(if f.has_exists() { 1 } else { count(f) })?;
            }
            Ok(())
        })
    }))?;
    let vars = if held.is_some() {
        let mut vars = Vec::with_capacity(filters.len());
        for f in filters {
            vars.push(if f.has_exists() {
                vec![VarId::MAX]
            } else {
                // the same variables as `var_set`, in storage of the counted size
                let mut v = Vec::with_capacity(count(f));
                f.vars(&mut v);
                v.sort_unstable();
                v.dedup();
                v
            });
        }
        vars
    } else {
        filters
            .iter()
            .map(|f| {
                if f.has_exists() {
                    vec![VarId::MAX]
                } else {
                    f.var_set()
                }
            })
            .collect()
    };
    Ok(FilterVars { vars, _held: held })
}
