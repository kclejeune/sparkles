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
//! program's, but never more than the greedy plan. A group of more than 16 inputs keeps
//! the greedy plan.

use super::ctx::Ctx;
use super::expr::Expr;
use super::indexjoin::{self, ProbeSide};
use super::plan::{self, Node, Planner};
use super::table::VarId;
use crate::error::Result;
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

/// What the join ordering knows about a plan.
#[derive(Clone, Copy, Debug)]
struct Sum {
    cost: f64,
    est: f64,
    /// the cost of sorting it
    sc: f64,
    /// the first sort variable
    sorted: Option<VarId>,
}

impl Sum {
    fn new(cost: f64, est: f64, sorted: Option<VarId>) -> Sum {
        Sum {
            cost,
            est,
            sc: plan::sort_cost(est),
            sorted,
        }
    }

    fn of(n: &Node) -> Sum {
        Sum::new(n.cost, n.est, n.sorted.first().copied())
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
    da: f64,
    db: f64,
    ca: bool,
    cb: bool,
}

/// One way of joining two plans: its cost and first sort variable.
#[derive(Clone, Copy)]
struct Cand {
    cost: f64,
    sorted: Option<VarId>,
    how: How,
}

/// The estimated rows of joining `a` and `b`, the same for every way of joining them.
fn pair_est(a: &Sum, b: &Sum, shared: &[Shared]) -> f64 {
    let denom = shared.iter().map(|s| s.da.max(s.db)).fold(1.0f64, f64::max);
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
                cost: x + y + (a.est + b.est) + est,
                sorted: Some(s.v),
                how: How::Merge(s.v),
            });
        }
    }
    let base = plan::hash_base(batched.hash_build, a.est, b.est);
    out.push(Cand {
        cost: a.cost + b.cost + base + est,
        sorted: if a.est >= b.est { a.sorted } else { b.sorted },
        how: How::Hash,
    });
    if batched.index_joins {
        if let Some(p) = pb
            && let Some((cost, sorted)) = probe(a, b, p, shared, est, false, keep)
        {
            out.push(Cand {
                cost,
                sorted,
                how: How::ProbeRight,
            });
        }
        if let Some(p) = pa
            && let Some((cost, sorted)) = probe(b, a, p, shared, est, true, keep)
        {
            out.push(Cand {
                cost,
                sorted,
                how: How::ProbeLeft,
            });
        }
    }
}

/// The cost and first sort variable of the index join of `drive` into the pattern
/// `pat`, if one is offered and `keep` accepts it. `drive_is_b` tells which side of
/// `shared` the input is.
fn probe(
    drive: &Sum,
    pat: &Sum,
    p: &ProbeSide,
    shared: &[Shared],
    est: f64,
    drive_is_b: bool,
    keep: &mut impl FnMut(f64, Option<VarId>) -> bool,
) -> Option<(f64, Option<VarId>)> {
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
    let probe_cost = p.cost(d.min(drive.est).max(1.0), pat.cost)?;
    let cost = indexjoin::join_cost(drive.cost, drive.est, probe_cost, est);
    Some((cost, sorted))
}

/// The cost and rows of a plan of `cost` and `est` rows under a FILTER of `count`
/// conjuncts, one of which sorts its input when `unsorted` (see [`plan::filter`]).
fn filtered(cost: f64, est: f64, count: u32, unsorted: bool) -> (f64, f64) {
    let cost = cost + est + if unsorted { est * 0.5 } else { 0.0 };
    let sel = plan::FILTER_SELECTIVITY.powi(count as i32);
    (cost, (est * sel).max(if est > 0.0 { 1.0 } else { 0.0 }))
}

/// Join `a` and `b` the given way (the same nodes as the planner's join candidates).
fn build_join(a: Node, b: Node, how: How, ctx: &Ctx) -> Node {
    let keys: Vec<VarId> = a
        .vars
        .iter()
        .filter(|v| b.vars.contains(v))
        .copied()
        .collect();
    match how {
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
    }
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
}

impl Group {
    fn new(items: &[Vec<Node>], filters: &[Expr]) -> Group {
        let mut idx: FxHashMap<VarId, u32> = FxHashMap::default();
        let mut vars = Vec::new();
        let mut count: Vec<u32> = Vec::new();
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
        let mut jvars = Vec::new();
        for (l, &c) in count.iter().enumerate() {
            if c >= 2 {
                join[l] = jvars.len() as u32;
                jvars.push(l as u32);
            }
        }
        let filters = filters
            .iter()
            .map(|f| {
                let vs = f.var_set();
                let local: Vec<Option<u32>> = vs.iter().map(|v| idx.get(v).copied()).collect();
                let placeable =
                    !f.has_exists() && !vs.is_empty() && local.iter().all(Option::is_some);
                let cached = super::exprcache::eligible(&[f])
                    .ok()
                    .flatten()
                    .and_then(|v| idx.get(&v).copied());
                FilterInfo {
                    placeable,
                    vars: local.into_iter().flatten().collect(),
                    cached,
                }
            })
            .collect();
        Group {
            vars,
            idx,
            join,
            jvars,
            item_vars,
            filters,
        }
    }
}

/// The plan for a connected group of inputs (each with its access paths), placing the
/// filters it can and removing them from `filters`. The inputs come back unplanned when
/// they are 12 or fewer and the program on summaries cannot track them (more than 128
/// variables shared between inputs), for the exhaustive program to plan.
pub(super) fn order(
    pl: &Planner,
    items: Vec<Vec<Node>>,
    filters: &mut Vec<Expr>,
) -> Result<std::result::Result<Node, Vec<Vec<Node>>>> {
    let g = Group::new(&items, filters);
    let dp = Dp::prepare(&g, &items);
    if dp.is_none() && items.len() <= 12 {
        return Ok(Err(items));
    }
    let greedy = Greedy::run(&g, &items, filters, pl.ctx)?;
    if let Some(mut dp) = dp {
        let bound = greedy.cost();
        if let Some(best) = dp.run(&items, filters, bound, pl)? {
            let (node, applied) = dp.build(best, &items, filters, pl);
            debug_assert!(
                node.cost <= bound.unwrap_or(f64::INFINITY),
                "plan dearer than the bound"
            );
            // the program places only the first 64 filters (see `Planner::dp`)
            let mut i = 0;
            filters.retain(|_| {
                let keep = i >= 64 || applied & (1u64 << i) == 0;
                i += 1;
                keep
            });
            return Ok(Ok(node));
        }
    }
    Ok(Ok(greedy.build(filters, pl)))
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
}

enum From {
    Leaf,
    Join(usize, usize, How),
}

/// A plan of the greedy ordering.
struct Unit {
    sum: Sum,
    /// variables in the order of the plan's columns, as local indices
    vars: Vec<u32>,
    has: Bits,
    certain: Bits,
    /// distinct-value estimates of the join variables, by join variable index
    d: Vec<(u32, f64)>,
    probe: Option<ProbeSide>,
    from: From,
    /// the filters placed on top, by index
    filters: Vec<usize>,
}

impl Unit {
    fn d(&self, j: u32) -> f64 {
        let i = self.d.binary_search_by_key(&j, |x| x.0).unwrap();
        self.d[i].1
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
    leaves: Vec<Node>,
    /// the plans left, in order
    alive: Vec<usize>,
    /// filters placed, by index
    placed: Vec<bool>,
}

impl Greedy {
    fn run(g: &Group, items: &[Vec<Node>], filters: &[Expr], ctx: &Ctx) -> Result<Greedy> {
        let nv = g.vars.len();
        let batched = plan::Costing::of(ctx);
        let mut placed = vec![false; filters.len()];
        let mut units: Vec<Unit> = Vec::with_capacity(2 * items.len());
        let mut leaves = Vec::with_capacity(items.len());
        for (i, opts) in items.iter().enumerate() {
            let mut best = &opts[0];
            for o in &opts[1..] {
                if o.cost.total_cmp(&best.cost) == Ordering::Less {
                    best = o;
                }
            }
            let mut has = Bits::new(nv);
            for &l in &g.item_vars[i] {
                has.set(l);
            }
            let now: Vec<usize> = (0..filters.len())
                .filter(|&f| {
                    !placed[f]
                        && g.filters[f].placeable
                        && g.filters[f].vars.iter().all(|&l| has.get(l))
                })
                .collect();
            let node = if now.is_empty() {
                best.clone()
            } else {
                for &f in &now {
                    placed[f] = true;
                }
                plan::filter(
                    best.clone(),
                    now.iter().map(|&f| filters[f].clone()).collect(),
                    ctx,
                )
            };
            let mut certain = Bits::new(nv);
            let mut d = Vec::new();
            let vars: Vec<u32> = node.vars.iter().map(|v| g.idx[v]).collect();
            for (&l, &v) in vars.iter().zip(&node.vars) {
                if node.certain.contains(&v) {
                    certain.set(l);
                }
                let j = g.join[l as usize];
                if j != u32::MAX {
                    d.push((j, node.d(v)));
                }
            }
            d.sort_unstable_by_key(|x| x.0);
            units.push(Unit {
                sum: Sum::of(&node),
                vars,
                has,
                certain,
                d,
                probe: if batched.index_joins {
                    indexjoin::probe_side(&node, ctx)
                } else {
                    None
                },
                from: From::Leaf,
                filters: Vec::new(),
            });
            leaves.push(node);
        }
        let mut gr = Greedy {
            units,
            leaves,
            alive: (0..items.len()).collect(),
            placed,
        };
        let mut heap = BinaryHeap::new();
        let mut cands = Vec::new();
        let mut shared = Vec::new();
        for b in 0..gr.units.len() {
            for a in 0..b {
                gr.push_pair(g, a, b, batched, &mut heap, &mut cands, &mut shared);
            }
        }
        let mut dead = vec![false; gr.units.len()];
        while gr.alive.len() > 1 {
            ctx.check()?;
            let Some(p) = std::iter::from_fn(|| heap.pop())
                .find(|p: &Pair| !dead[p.a as usize] && !dead[p.b as usize])
            else {
                break;
            };
            let w = gr.join(g, &p, filters);
            dead[p.a as usize] = true;
            dead[p.b as usize] = true;
            dead.push(false);
            gr.alive.retain(|&u| u != p.a as usize && u != p.b as usize);
            for i in 0..gr.alive.len() {
                let u = gr.alive[i];
                gr.push_pair(g, u, w, batched, &mut heap, &mut cands, &mut shared);
            }
            gr.alive.push(w);
        }
        Ok(gr)
    }

    /// Cost the joins of plans `a` and `b` (`a` made first) and queue the best.
    #[allow(clippy::too_many_arguments)]
    fn push_pair(
        &self,
        g: &Group,
        a: usize,
        b: usize,
        batched: plan::Costing,
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
                    da: ua.d(j),
                    db: ub.d(j),
                    ca: ua.certain.get(l),
                    cb: ub.certain.get(l),
                });
            }
        }
        let est = pair_est(&ua.sum, &ub.sum, shared);
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
        });
    }

    /// Make the plan joining `p.a` and `p.b`, with the filters it binds on top.
    fn join(&mut self, g: &Group, p: &Pair, filters: &[Expr]) -> usize {
        let (a, b) = (&self.units[p.a as usize], &self.units[p.b as usize]);
        let (drive, other) = if p.how == How::ProbeLeft {
            (b, a)
        } else {
            (a, b)
        };
        let mut vars = drive.vars.clone();
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
                    (ja, da.min(db))
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
        let mut sum = Sum::new(p.cost, p.est, p.sorted);
        let now: Vec<usize> = (0..filters.len())
            .filter(|&f| {
                !self.placed[f]
                    && g.filters[f].placeable
                    && g.filters[f].vars.iter().all(|&l| has.get(l))
            })
            .collect();
        if !now.is_empty() {
            let unsorted = now.iter().any(|&f| {
                g.filters[f]
                    .cached
                    .is_some_and(|l| has.get(l) && sum.sorted != Some(g.vars[l as usize]))
            });
            let (cost, est) = filtered(sum.cost, sum.est, now.len() as u32, unsorted);
            sum = Sum::new(cost, est, sum.sorted);
            for x in &mut d {
                x.1 = x.1.min(sum.est.max(1.0));
            }
            for &f in &now {
                self.placed[f] = true;
            }
        }
        self.units.push(Unit {
            sum,
            vars,
            has,
            certain,
            d,
            probe: None,
            from: From::Join(p.a as usize, p.b as usize, p.how),
            filters: now,
        });
        self.units.len() - 1
    }

    /// The cost of the plan, when the inputs became one plan.
    fn cost(&self) -> Option<f64> {
        match self.alive.as_slice() {
            [u] => Some(self.units[*u].sum.cost),
            _ => None,
        }
    }

    /// Build the plan tree, removing the placed filters from `filters`.
    fn build(self, filters: &mut Vec<Expr>, pl: &Planner) -> Node {
        let ctx = pl.ctx;
        let mut nodes: Vec<Option<Node>> = Vec::with_capacity(self.units.len());
        let mut leaves = self.leaves.into_iter();
        for u in &self.units {
            let n = match u.from {
                From::Leaf => leaves.next().unwrap(),
                From::Join(a, b, how) => {
                    let (na, nb) = (nodes[a].take().unwrap(), nodes[b].take().unwrap());
                    let n = build_join(na, nb, how, ctx);
                    if u.filters.is_empty() {
                        n
                    } else {
                        plan::filter(
                            n,
                            u.filters.iter().map(|&f| filters[f].clone()).collect(),
                            ctx,
                        )
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
            acc = pl.place_filters(plan::join(acc, n, ctx), filters);
        }
        acc
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
    ents: Vec<Ent>,
    dval: Vec<f64>,
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
    fn prepare(g: &Group, items: &[Vec<Node>]) -> Option<Dp> {
        let n = items.len();
        if n > DP_MAX_ITEMS || g.jvars.len() > DP_MAX_JOIN_VARS {
            return None;
        }
        let full: u32 = (1u32 << n) - 1;
        let size = 1usize << n;
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
        Some(Dp {
            n,
            jm,
            cm,
            fcov,
            jvar,
            jidx,
            cached,
            ents: Vec::new(),
            dval: Vec::new(),
        })
    }

    /// The join variables of the inputs `m` that inputs outside them share.
    fn bnd(&self, m: u32) -> u128 {
        let full = (1u32 << self.n) - 1;
        self.jm[m as usize] & self.jm[(full ^ m) as usize]
    }

    /// The tables of a round over `parts`.
    fn round(&self, parts: Vec<Part>) -> Round {
        let k = parts.len();
        let size = 1usize << k;
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
        let mut list = Vec::new();
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
        Round {
            parts,
            inputs,
            conn,
            leaf,
            slot,
            list,
            minc,
            splits,
        }
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
        let ub = bound.unwrap_or(f64::INFINITY);
        let batched = plan::Costing::of(ctx);
        let fvars = dp_filter_vars(filters);
        // the inputs: every access path with the filters it binds
        let mut parts = Vec::with_capacity(items.len());
        for (i, opts) in items.iter().enumerate() {
            let m = 1u32 << i;
            let bnd = self.bnd(m);
            let mut ents = Vec::with_capacity(opts.len());
            for (k, o) in opts.iter().enumerate() {
                let (o, _) = pl.apply_dp_filters(o.clone(), 0, filters, &fvars);
                let d = self.dval.len() as u32;
                let mut rest = bnd;
                while rest != 0 {
                    let j = rest.trailing_zeros();
                    rest &= rest - 1;
                    self.dval.push(o.d(self.jvar[j as usize]));
                }
                ents.push(self.ents.len() as u32);
                self.ents.push(Ent {
                    sum: Sum::of(&o),
                    mask: m,
                    d,
                    how: How::Hash,
                    a: i as u32,
                    b: k as u32,
                    probe: if batched.index_joins {
                        indexjoin::probe_side(&o, ctx)
                    } else {
                        None
                    },
                });
            }
            self.drop_dominated(m, &mut ents);
            parts.push(Part { inputs: m, ents });
        }
        let mut rounds = false;
        loop {
            let mut r = self.round(parts);
            let k = r.parts.len();
            let total: u64 = r.splits.iter().sum();
            if !rounds && total > DP_MAX_SPLITS {
                // planned in rounds from here on, where the parts keep only the plans
                // that can matter later
                rounds = true;
                parts = r.parts.into_iter().map(|p| self.useful(p)).collect();
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
            self.plan_round(&mut r, size, ub, batched, ctx)?;
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
            let merged = self.useful(Part {
                inputs: r.inputs[m as usize],
                ents: r.list[s as usize..(s + l) as usize].to_vec(),
            });
            parts = r
                .parts
                .into_iter()
                .enumerate()
                .filter(|(i, _)| m & (1 << i) == 0)
                .map(|(_, p)| p)
                .collect();
            parts.push(merged);
        }
    }

    /// The part with its cheapest plan and those sorted on a variable that later joins
    /// read.
    fn useful(&self, p: Part) -> Part {
        let cheapest = self.cheapest(&p.ents);
        let bnd = self.bnd(p.inputs);
        let ents = p
            .ents
            .iter()
            .copied()
            .filter(|&e| {
                Some(e) == cheapest
                    || self.ents[e as usize].sum.sorted.is_some_and(|v| {
                        self.jvar
                            .iter()
                            .position(|&x| x == v)
                            .is_some_and(|j| bnd >> j & 1 == 1)
                    })
            })
            .collect();
        Part {
            inputs: p.inputs,
            ents,
        }
    }

    /// Plan every connected subset of the round's parts of at most `size` parts.
    fn plan_round(
        &mut self,
        r: &mut Round,
        size: usize,
        ub: f64,
        batched: plan::Costing,
        ctx: &Ctx,
    ) -> Result<()> {
        let k = r.parts.len();
        let mut cur: Vec<Best> = Vec::new();
        let mut scratch = Scratch::default();
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
                self.split(r, m, sub, m ^ sub, ub, batched, &mut cur, &mut scratch);
                sub = (sub - 1) & low;
            }
            self.finish(r, m, &mut cur);
        }
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
                da: 0.0,
                db: 0.0,
                ca: ca >> j & 1 == 1,
                cb: cb >> j & 1 == 1,
            });
            sc.pos.push((rank(ba, j), rank(bb, j)));
        }
        let newf = self.fcov[im as usize] & !(self.fcov[ia as usize] | self.fcov[ib as usize]);
        let nf = newf.count_ones();
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
        let Scratch {
            shared,
            pos,
            cached,
            cands,
        } = sc;
        for &ea in &r.list[sa as usize..(sa + la) as usize] {
            let a = &self.ents[ea as usize];
            for &eb in &r.list[sb as usize..(sb + lb) as usize] {
                let b = &self.ents[eb as usize];
                for (s, &(pa, pb)) in shared.iter_mut().zip(pos.iter()) {
                    s.da = self.dval[(a.d + pa) as usize];
                    s.db = self.dval[(b.d + pb) as usize];
                }
                let est = pair_est(&a.sum, &b.sum, shared);
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
                        filtered(cost, est, nf, unsorted(key))
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
    fn finish(&mut self, r: &mut Round, m: u32, cur: &mut [Best]) {
        cur.sort_unstable_by_key(|b| b.sorted);
        let start = r.list.len() as u32;
        let bnd = self.bnd(r.inputs[m as usize]);
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
                    (Some(x), Some(y)) => x.min(y),
                    (Some(x), None) | (None, Some(x)) => x,
                    (None, None) => unreachable!("a join variable of neither input"),
                };
                let mut x = x.min(b.est).max(1.0);
                if b.filtered {
                    x = x.min(b.rows.max(1.0));
                }
                self.dval.push(x);
            }
            self.ents.push(Ent {
                sum: Sum::new(b.cost, b.rows, b.sorted),
                mask: ea.mask | eb.mask,
                d,
                how: b.how,
                a: b.a,
                b: b.b,
                probe: None,
            });
        }
        let first = self.ents.len() as u32 - cur.len() as u32;
        let mut ids: Vec<u32> = (first..self.ents.len() as u32).collect();
        self.drop_dominated(r.inputs[m as usize], &mut ids);
        for &e in &ids {
            r.minc[m as usize] = r.minc[m as usize].min(self.ents[e as usize].sum.cost);
        }
        r.list.extend(&ids);
        r.slot[m as usize] = (start, ids.len() as u32);
    }

    /// Drop from `ids`, plans of the inputs `inputs`, those whose sort variable no later
    /// join or filter reads and that another plan matches in rows and distinct-value
    /// estimates at no more cost. Every plan made from a dropped one has a counterpart
    /// made from the other that costs no more, with the same estimates.
    fn drop_dominated(&self, inputs: u32, ids: &mut Vec<u32>) {
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
                && (0..len).all(|i| {
                    self.dval[(x.d + i) as usize].to_bits()
                        == self.dval[(y.d + i) as usize].to_bits()
                })
        };
        let mut keep = vec![true; ids.len()];
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
    fn build(&self, root: u32, items: &[Vec<Node>], filters: &[Expr], pl: &Planner) -> (Node, u64) {
        let ctx = pl.ctx;
        let fvars = dp_filter_vars(filters);
        // the entries of the plan, children before parents
        let mut need = vec![root];
        let mut i = 0;
        while i < need.len() {
            let e = self.ents[need[i] as usize];
            if !e.mask.is_power_of_two() {
                need.push(e.a);
                need.push(e.b);
            }
            i += 1;
        }
        need.sort_unstable();
        let mut built: FxHashMap<u32, (Node, u64)> = FxHashMap::default();
        for &x in &need {
            let e = self.ents[x as usize];
            let (n, f) = if e.mask.is_power_of_two() {
                pl.apply_dp_filters(
                    items[e.a as usize][e.b as usize].clone(),
                    0,
                    filters,
                    &fvars,
                )
            } else {
                let (na, fa) = built.remove(&e.a).unwrap();
                let (nb, fb) = built.remove(&e.b).unwrap();
                pl.apply_dp_filters(build_join(na, nb, e.how, ctx), fa | fb, filters, &fvars)
            };
            debug_assert!(
                n.cost.to_bits() == e.sum.cost.to_bits() && n.est.to_bits() == e.sum.est.to_bits(),
                "summary {:?} differs from its plan ({}, {})",
                e.sum,
                n.cost,
                n.est
            );
            built.insert(x, (n, f));
        }
        built.remove(&root).unwrap()
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
}

/// The variables of each filter as the exhaustive program reads them (EXISTS never
/// placed).
fn dp_filter_vars(filters: &[Expr]) -> Vec<Vec<VarId>> {
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
}
