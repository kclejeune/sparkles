//! Cost-based rule-body planning for materialization (spec F23, roadmap item 487).
//!
//! The greedy planner of [`crate::engine::plan`] places, at each step, the atom with the
//! fewest estimated matches, and estimates every atom independently from per-predicate
//! averages. This planner keeps the greedy planner's builtin placement and its estimates
//! for atoms with variables, and changes three things.
//!
//! * An atom with a constant subject or object is probed: its matches are counted in the
//!   in-memory lists and in the store's index, exactly when the index estimates few
//!   matches, instead of taken as the predicate's average per subject or object.
//! * A join of `?s p1 ?a`, `?s p2 ?b` or `?s rdf:type <class>` atoms on one subject over
//!   the base tier is estimated from the characteristic sets of the store's statistics,
//!   the same estimate the SPARQL planner uses for subject stars
//!   ([`sparkles_core::sparql::charsets::star_estimate`]).
//! * Bodies of up to [`MAX_DP`] atoms are ordered by dynamic programming over the subsets
//!   of their atoms, so that the order minimizes the estimated rows of every prefix, not
//!   only the next step's. Longer bodies keep the greedy order with the estimates above.
//!
//! The order replaces the greedy planner's only when these estimates put its cost at
//! most half of the greedy order's, and when it needs no index by subject or object that
//! the greedy order does not. Such an index covers the whole graph, and building it for
//! `cls-svf1` added about 60 MiB to LUBM(20) under OWL 2 RL.
//!
//! A semi-naive iteration plans each rule once per atom that reads the delta. The planner
//! keeps each such plan with the row counts of its atoms' ranges, and plans again only
//! when one of them changed by more than a factor of [`REPLAN_RATIO`] since.

use crate::engine::{Atom, CRule, Slot, Step};
use crate::graph::Graph;
use rustc_hash::FxHashMap;
use sparkles_core::id::{Id, Tag};
use sparkles_core::index::Perm;

/// Bodies with at most this many atoms are ordered by dynamic programming.
pub(crate) const MAX_DP: usize = 6;

/// Prefixes the store estimates at no more than this many quads are counted exactly.
const EXACT_BELOW: u64 = 8192;

/// This planner's order replaces the greedy planner's only when its estimated cost is at
/// most the greedy order's divided by this.
pub(crate) const SAVING: f64 = 2.0;

/// A kept plan is used again while every atom's row count stays within this factor.
pub(crate) const REPLAN_RATIO: f64 = 2.0;

/// The in-memory share of a predicate's rows below which a base-tier star estimate is
/// used for it.
const STAR_MEMORY_SHARE: f64 = 0.1;

/// A probed pattern `(s, p, o)` with its row range.
type Probed = (Option<u64>, u64, Option<u64>, u32, u32);

struct Kept {
    counts: Vec<f64>,
    order: Vec<usize>,
}

/// The plans and star estimates kept across the iterations of one run.
#[derive(Default)]
pub(crate) struct Planner {
    kept: FxHashMap<(usize, usize), Kept>,
    stars: FxHashMap<Vec<(u64, Option<u64>)>, Option<f64>>,
    /// the counts of probed patterns by row range
    probes: FxHashMap<Probed, u64>,
    rdf_type: Option<u64>,
    /// plans made, plans used again and plans that differ from the greedy order, for the
    /// log
    pub made: usize,
    pub reused: usize,
    pub changed: usize,
}

#[inline]
fn bound(s: Slot, b: &[bool]) -> bool {
    match s {
        Slot::Const(_) => true,
        Slot::Var(v) => b[v],
        Slot::Any => false,
    }
}

/// The steps of the atoms in `order`, with each builtin placed as soon as its inputs are
/// bound, as [`crate::engine::plan_with`] places them. `None` when a builtin's inputs are
/// never bound.
pub(crate) fn steps_in_order(
    rule: &CRule,
    ranges: &[(u32, u32)],
    order: &[usize],
) -> Option<Vec<Step>> {
    let mut b = vec![false; rule.nvars];
    let mut steps = Vec::with_capacity(rule.atoms.len() + rule.builtins.len());
    let mut left: Vec<usize> = (0..rule.builtins.len()).collect();
    let place = |b: &mut Vec<bool>, left: &mut Vec<usize>, steps: &mut Vec<Step>| {
        let mut progress = true;
        while progress {
            progress = false;
            left.retain(|&j| {
                let f = &rule.builtins[j];
                if f.inputs.iter().all(|&v| b[v]) {
                    steps.push(Step::Builtin(j));
                    for &v in &f.outputs {
                        b[v] = true;
                    }
                    progress = true;
                    false
                } else {
                    true
                }
            });
        }
    };
    for &i in order {
        place(&mut b, &mut left, &mut steps);
        let a = &rule.atoms[i];
        for s in [a.s, a.p, a.o] {
            if let Slot::Var(v) = s {
                b[v] = true;
            }
        }
        steps.push(Step::Atom {
            atom: i,
            lo: ranges[i].0,
            hi: ranges[i].1,
        });
    }
    place(&mut b, &mut left, &mut steps);
    left.is_empty().then_some(steps)
}

impl Planner {
    pub(crate) fn new(rdf_type: Option<u64>) -> Planner {
        Planner {
            rdf_type,
            ..Planner::default()
        }
    }

    /// The plan of rule `ri` over `ranges`, kept under `key` (the atom that reads the
    /// delta, or `usize::MAX` for a plan over every row).
    pub(crate) fn plan(
        &mut self,
        g: &Graph,
        ri: usize,
        key: usize,
        rule: &CRule,
        ranges: &[(u32, u32)],
    ) -> Option<Vec<Step>> {
        let counts: Vec<f64> = rule
            .atoms
            .iter()
            .zip(ranges)
            .map(|(a, &(lo, hi))| match a.p {
                Slot::Const(p) => g.count_p(p, lo, hi) as f64,
                _ => hi.saturating_sub(lo) as f64,
            })
            .collect();
        if let Some(k) = self.kept.get(&(ri, key))
            && k.counts.iter().zip(&counts).all(|(&old, &new)| {
                let r = (new + 1.0) / (old + 1.0);
                (1.0 / REPLAN_RATIO..=REPLAN_RATIO).contains(&r)
            })
        {
            let order = k.order.clone();
            self.reused += 1;
            return steps_in_order(rule, ranges, &order);
        }
        self.made += 1;
        // the greedy planner's order, which this one replaces only when its estimates
        // promise a clear saving
        let greedy_steps = crate::engine::plan(rule, ranges, Some(g))?;
        let greedy: Vec<usize> = greedy_steps
            .iter()
            .filter_map(|s| match s {
                Step::Atom { atom, .. } => Some(*atom),
                Step::Builtin(_) => None,
            })
            .collect();
        let mine = if rule.atoms.len() <= MAX_DP {
            self.dp(g, rule, ranges)
        } else {
            self.greedy(g, rule, ranges)
        };
        // an order that reads an atom of a variable predicate by subject or object alone
        // builds an index of the whole graph by that position, which the estimates do
        // not price, so it is taken only when the greedy order needs the same indexes
        let needs = |steps: &[Step]| crate::engine::index_needs_with(rule, steps, &[]);
        let order = if mine != greedy
            && steps_in_order(rule, ranges, &mine).is_some_and(|m| {
                let (ms, mo) = needs(&m);
                let (gs, go) = needs(&greedy_steps);
                (!ms || gs) && (!mo || go)
            })
            && self.cost(g, rule, ranges, &mine) * SAVING < self.cost(g, rule, ranges, &greedy)
        {
            self.changed += 1;
            mine
        } else {
            greedy
        };
        let steps = steps_in_order(rule, ranges, &order)?;
        tracing::trace!(rule = %rule.name, key, ?order, ?counts, "cost-based plan");
        self.kept.insert((ri, key), Kept { counts, order });
        Some(steps)
    }

    /// The estimated cost of an order: the rows read and made at every step.
    fn cost(&mut self, g: &Graph, rule: &CRule, ranges: &[(u32, u32)], order: &[usize]) -> f64 {
        let (mut mask, mut rows, mut cost) = (0u32, 1.0f64, 0.0f64);
        for &i in order {
            let b = Self::bound_after(rule, mask);
            let out = rows * self.fanout(g, rule, ranges, mask, i, &b);
            cost += rows + out;
            rows = out;
            mask |= 1 << i;
        }
        cost
    }

    /// The variables bound after the atoms of `mask` and the builtins they enable.
    fn bound_after(rule: &CRule, mask: u32) -> Vec<bool> {
        let mut b = vec![false; rule.nvars];
        for (i, a) in rule.atoms.iter().enumerate() {
            if mask & (1 << i) != 0 {
                for s in [a.s, a.p, a.o] {
                    if let Slot::Var(v) = s {
                        b[v] = true;
                    }
                }
            }
        }
        let mut progress = true;
        let mut done = vec![false; rule.builtins.len()];
        while progress {
            progress = false;
            for (j, f) in rule.builtins.iter().enumerate() {
                if !done[j] && f.inputs.iter().all(|&v| b[v]) {
                    done[j] = true;
                    for &v in &f.outputs {
                        if !b[v] {
                            b[v] = true;
                            progress = true;
                        }
                    }
                }
            }
        }
        b
    }

    /// The order of the atoms that minimizes the estimated rows of every prefix.
    fn dp(&mut self, g: &Graph, rule: &CRule, ranges: &[(u32, u32)]) -> Vec<usize> {
        let n = rule.atoms.len();
        let full = (1u32 << n) - 1;
        // per subset: (cost, rows, last atom, previous subset)
        let mut best: Vec<Option<(f64, f64, usize, u32)>> = vec![None; 1 << n];
        best[0] = Some((0.0, 1.0, usize::MAX, 0));
        let mut by_size: Vec<u32> = (0..=full).collect();
        by_size.sort_by_key(|m| (m.count_ones(), *m));
        for &mask in &by_size {
            let Some((cost, rows, _, _)) = best[mask as usize] else {
                continue;
            };
            if mask == full {
                continue;
            }
            let b = Self::bound_after(rule, mask);
            for i in 0..n {
                if mask & (1 << i) != 0 {
                    continue;
                }
                let f = self.fanout(g, rule, ranges, mask, i, &b);
                let out = rows * f;
                let c = cost + rows + out;
                let next = (mask | (1 << i)) as usize;
                if best[next].is_none_or(|(bc, _, _, _)| c < bc) {
                    best[next] = Some((c, out, i, mask));
                }
            }
        }
        let mut order = Vec::with_capacity(n);
        let mut m = full;
        while m != 0 {
            let (_, _, i, prev) = best[m as usize].expect("every subset is reached");
            order.push(i);
            m = prev;
        }
        order.reverse();
        order
    }

    /// The greedy order with this planner's estimates.
    fn greedy(&mut self, g: &Graph, rule: &CRule, ranges: &[(u32, u32)]) -> Vec<usize> {
        let n = rule.atoms.len();
        let mut mask = 0u32;
        let mut order = Vec::with_capacity(n);
        while order.len() < n {
            let b = Self::bound_after(rule, mask);
            let i = (0..n)
                .filter(|&i| mask & (1 << i) == 0)
                .map(|i| (i, self.fanout(g, rule, ranges, mask, i, &b)))
                .min_by(|x, y| x.1.total_cmp(&y.1))
                .map(|x| x.0)
                .expect("an atom is left");
            mask |= 1 << i;
            order.push(i);
        }
        order
    }

    /// The estimated matches of atom `i` per binding of the atoms of `mask`.
    fn fanout(
        &mut self,
        g: &Graph,
        rule: &CRule,
        ranges: &[(u32, u32)],
        mask: u32,
        i: usize,
        b: &[bool],
    ) -> f64 {
        let a = &rule.atoms[i];
        let (lo, hi) = ranges[i];
        if hi <= lo {
            return 0.0;
        }
        if let Slot::Const(p) = a.p {
            // probes: a constant subject or object is counted in the indexes
            let probe = match (a.s, a.o) {
                (Slot::Const(s), Slot::Const(o)) => {
                    Some(self.probe(g, Some(s), p, Some(o), lo, hi).min(1.0))
                }
                (Slot::Const(s), o) => {
                    let c = self.probe(g, Some(s), p, None, lo, hi);
                    Some(if bound(o, b) { c.min(1.0) } else { c })
                }
                (s, Slot::Const(o)) => {
                    let c = self.probe(g, None, p, Some(o), lo, hi);
                    Some(if bound(s, b) { c.min(1.0) } else { c })
                }
                _ => None,
            };
            if let Some(c) = probe {
                return c;
            }
            if let Some(f) = self.star_fanout(g, rule, ranges, mask, i, b) {
                return f;
            }
        }
        crate::engine::estimate_atom(a, b, (lo, hi), Some(g))
    }

    /// The fanout of a star atom `?s p ?o` (or `?s rdf:type <class>`) on a subject that
    /// star atoms of `mask` bind, from the characteristic sets: rows(Q + a) / rows(Q).
    fn star_fanout(
        &mut self,
        g: &Graph,
        rule: &CRule,
        ranges: &[(u32, u32)],
        mask: u32,
        i: usize,
        b: &[bool],
    ) -> Option<f64> {
        let base = g.base()?;
        let a = &rule.atoms[i];
        let Slot::Var(x) = a.s else {
            return None;
        };
        if !b[x] {
            return None;
        }
        let item = |a: &Atom, (lo, hi): (u32, u32)| -> Option<(u64, Option<u64>)> {
            let Slot::Const(p) = a.p else { return None };
            if lo > 0 {
                return None;
            }
            let o = match a.o {
                Slot::Const(o) if Some(p) == self.rdf_type => Some(o),
                Slot::Const(_) => return None,
                _ => None,
            };
            // mostly base-tier rows, which the sets describe
            let memory = g.count_p(p, g.offset(), hi) as f64;
            let stored = base.count_p(p) as f64;
            (memory <= STAR_MEMORY_SHARE * stored.max(1.0)).then_some((p, o))
        };
        let mine = item(a, ranges[i])?;
        let mut q: Vec<(u64, Option<u64>)> = Vec::new();
        for (j, other) in rule.atoms.iter().enumerate() {
            if mask & (1 << j) != 0
                && other.s == Slot::Var(x)
                && let Some(it) = item(other, ranges[j])
            {
                q.push(it);
            }
        }
        if q.is_empty() || q.contains(&mine) {
            return None;
        }
        q.sort_unstable();
        let before = self.star_rows(base, &q)?;
        let mut with = q.clone();
        with.push(mine);
        with.sort_unstable();
        let after = self.star_rows(base, &with)?;
        (before > 0.0).then(|| after / before)
    }

    /// [`probe_count`], once per pattern and range in a run.
    fn probe(
        &mut self,
        g: &Graph,
        s: Option<u64>,
        p: u64,
        o: Option<u64>,
        lo: u32,
        hi: u32,
    ) -> f64 {
        *self
            .probes
            .entry((s, p, o, lo, hi))
            .or_insert_with(|| probe_count(g, s, p, o, lo, hi)) as f64
    }

    fn star_rows(
        &mut self,
        base: &crate::facts::Base,
        items: &[(u64, Option<u64>)],
    ) -> Option<f64> {
        if let Some(r) = self.stars.get(items) {
            return *r;
        }
        let r = sparkles_core::sparql::charsets::star_estimate(&base.snap.generation, items)
            .map(|(rows, _)| rows);
        self.stars.insert(items.to_vec(), r);
        r
    }
}

/// The facts matching `(s p o)`, with `s` or `o` constant, in rows `[lo, hi)`: the
/// in-memory lists' rows and, when the range holds the base tier, the store's estimate
/// of the prefix, which reads no block.
fn probe_count(g: &Graph, s: Option<u64>, p: u64, o: Option<u64>, lo: u32, hi: u32) -> u64 {
    let memory = g.cands(s, Some(p), o, lo, hi).len() as u64;
    // a term the rules made has a local id, which no stored fact holds
    let local = |x: Option<u64>| x.is_some_and(|x| Id(x).tag() == Tag::Local);
    let stored = match g.base() {
        Some(_) if local(s) || local(o) || local(Some(p)) => 0,
        Some(b) if g.reads_base(lo, hi) => {
            // the index's estimate, which decodes no block, and an exact count through a
            // memo of the planner's own when the estimate is small
            let estimate = match (s, o) {
                (Some(s), Some(o)) => b.snap.estimate(Perm::Spo, &[s, p, o]),
                (Some(s), None) => b.snap.estimate(Perm::Pso, &[p, s]),
                (None, Some(o)) => b.snap.estimate(Perm::Pos, &[p, o]),
                (None, None) => b.count_p(p),
            };
            if (s.is_some() || o.is_some()) && estimate <= EXACT_BELOW {
                let mut memo = crate::facts::Memo::default();
                let mut n = 0u64;
                let done = b.for_each_with(&mut memo, s, Some(p), o, |_| {
                    n += 1;
                    n <= EXACT_BELOW
                });
                b.add_scans(memo.scans);
                if done.is_ok() { n } else { estimate }
            } else {
                estimate
            }
        }
        _ => 0,
    };
    memory + stored
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atom(s: Slot, p: u64, o: Slot) -> Atom {
        Atom {
            s,
            p: Slot::Const(p),
            o,
        }
    }

    /// `(?x 1 ?y), (?y 2 ?z), (?z 3 100)`: the average of predicate 3 per object is high,
    /// but object 100 has one fact, which the probe counts, so the plan starts there.
    #[test]
    fn probes_and_dp_start_at_the_selective_constant() {
        let mut g = Graph::default();
        for i in 0..200u64 {
            g.add([1000 + i, 1, 2000 + i]);
            g.add([2000 + i, 2, 3000 + i]);
            // predicate 3: object 100 once, object 101 for everything else
            g.add([3000 + i, 3, if i == 7 { 100 } else { 101 }]);
        }
        let (x, y, z) = (Slot::Var(0), Slot::Var(1), Slot::Var(2));
        let rule = CRule {
            name: "t".into(),
            nvars: 3,
            atoms: vec![atom(x, 1, y), atom(y, 2, z), atom(z, 3, Slot::Const(100))],
            builtins: Vec::new(),
            head: vec![crate::engine::CHead::Triple(x, Slot::Const(9), z)],
        };
        let ranges = vec![(0, g.len()); 3];
        let mut p = Planner::new(None);
        let steps = p.plan(&g, 0, usize::MAX, &rule, &ranges).unwrap();
        let order: Vec<usize> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Atom { atom, .. } => Some(*atom),
                _ => None,
            })
            .collect();
        assert_eq!(order, vec![2, 1, 0]);
        // the same counts reuse the kept order
        p.plan(&g, 0, usize::MAX, &rule, &ranges).unwrap();
        assert_eq!((p.made, p.reused), (1, 1));
    }

    /// Builtins go where the greedy planner puts them: as soon as their inputs are bound.
    #[test]
    fn steps_place_builtins_as_soon_as_their_inputs_are_bound() {
        use crate::builtins::BuiltinKind;
        let (x, y) = (Slot::Var(0), Slot::Var(1));
        let rule = CRule {
            name: "t".into(),
            nvars: 2,
            atoms: vec![atom(x, 1, y), atom(y, 2, x)],
            builtins: vec![crate::engine::CBuiltin {
                origin: 0,
                kind: BuiltinKind::NotEqual,
                args: vec![x, y],
                inputs: vec![0, 1],
                outputs: Vec::new(),
                regex: None,
            }],
            head: vec![crate::engine::CHead::Triple(x, Slot::Const(9), y)],
        };
        let steps = steps_in_order(&rule, &[(0, 1), (0, 1)], &[1, 0]).unwrap();
        assert!(matches!(
            steps.as_slice(),
            [
                Step::Atom { atom: 1, .. },
                Step::Builtin(0),
                Step::Atom { atom: 0, .. }
            ]
        ));
    }
}
