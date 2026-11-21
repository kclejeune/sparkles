//! The local problem: whether data restrictions can hold of one element together.
//!
//! The checker counts values by type. A type is a cell, a nonempty intersection of each
//! range involved or its complement, with an upward-closed set of the data properties
//! involved, the properties that relate the element to the values of that type. Each
//! type has an unknown count of distinct values. A minimum or maximum bounds the sum of
//! the types whose cell is within its range and whose set holds its property, a range
//! of a property and a disjointness forbid types, and the types of one cell sum to at
//! most the cell's size. The counts assume nothing about which witnesses are the same
//! value, so the answer is exact when the search finishes within its bound.

use super::range::{Ranges, Rid};
use super::sets::CAP;
use rustc_hash::FxHashMap;

/// A data property of the problem: the indices of its superproperties, which need not
/// be closed, and its ranges.
#[derive(Clone, Debug, Default)]
pub struct Prop {
    pub supers: Vec<usize>,
    pub ranges: Vec<Rid>,
}

/// A data restriction on the element, over one property of the problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Atom {
    /// `≥ n d.R`: an existential is `≥ 1`, a value `≥ 1` of its point range
    AtLeast(usize, u64, Rid),
    /// `≤ n d.R`: a universal `∀d.R` is `≤ 0 d.¬R`
    AtMost(usize, u64, Rid),
}

impl Atom {
    /// The negation of the atom, which the at-least and at-most kinds are closed under.
    /// The negation of `≥ 0 d.R`, which always holds, is `≥ 1 d.∅`, which never does.
    /// The negation of `≤ u64::MAX d.R` is past the counts an atom holds, so it is `None`.
    pub fn negate(self) -> Option<Atom> {
        Some(match self {
            Atom::AtLeast(d, 0, _) => Atom::AtLeast(d, 1, Ranges::EMPTY),
            Atom::AtLeast(d, n, r) => Atom::AtMost(d, n - 1, r),
            Atom::AtMost(d, n, r) => Atom::AtLeast(d, n.checked_add(1)?, r),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct Problem {
    pub props: Vec<Prop>,
    pub disjoint: Vec<(usize, usize)>,
    pub atoms: Vec<Atom>,
}

/// Why the checker gave no answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// the search passed its bound on types or nodes
    Bound,
    /// the answer turns on the size of a cell the checker has not counted exactly
    Uncounted,
    /// the caller's stop flag was set: its deadline, cancellation or memory budget
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Sat,
    Unsat,
    Unknown(Why),
}

#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    /// the most properties a problem may involve, since sets of them are enumerated
    pub props: usize,
    /// the most types
    pub types: usize,
    /// the most search nodes for one solve
    pub nodes: u64,
}

impl Default for Bounds {
    fn default() -> Bounds {
        Bounds {
            props: 12,
            types: 4096,
            nodes: 100_000,
        }
    }
}

/// A constraint on a sum of counts: at least or at most `n`.
#[derive(Clone, Debug)]
struct Lin {
    vars: Vec<usize>,
    at_least: bool,
    n: u128,
}

enum Run {
    Sat,
    Unsat,
    Bound,
    Stopped,
}

/// How many search nodes pass between two reads of the stop flag.
const POLL: u64 = 1024;

/// Whether the integer program has a solution, by preprocessing and a bounded search.
fn solve(nvars: usize, mut cons: Vec<Lin>, nodes: u64, stop: &dyn Fn() -> bool) -> Run {
    const INF: u128 = u128::MAX;
    // a count fixed at zero by a maximum of zero
    let mut ub = vec![INF; nvars];
    for c in &cons {
        if !c.at_least {
            for &v in &c.vars {
                ub[v] = ub[v].min(c.n);
            }
        }
    }
    // a count that no maximum bounds can be as large as any minimum needs
    let mut live = vec![true; nvars];
    loop {
        let mut changed = false;
        let mut in_min = vec![false; nvars];
        for c in &cons {
            if c.at_least {
                for &v in &c.vars {
                    in_min[v] = true;
                }
            }
        }
        for v in 0..nvars {
            if live[v] && (!in_min[v] || ub[v] == 0) {
                live[v] = false;
                changed = true;
            }
        }
        let free: Vec<bool> = (0..nvars).map(|v| live[v] && ub[v] == INF).collect();
        let before = cons.len();
        cons.retain(|c| !(c.at_least && c.vars.iter().any(|&v| free[v])));
        for v in 0..nvars {
            if free[v] {
                live[v] = false;
            }
        }
        changed |= cons.len() != before;
        for c in &mut cons {
            let n = c.vars.len();
            c.vars.retain(|&v| live[v]);
            changed |= c.vars.len() != n;
        }
        cons.retain(|c| !(c.at_least && c.n == 0) && !(!c.at_least && c.vars.is_empty()));
        if cons.iter().any(|c| c.at_least && c.vars.is_empty()) {
            return Run::Unsat;
        }
        if !changed {
            break;
        }
    }
    let vars: Vec<usize> = (0..nvars).filter(|&v| live[v]).collect();
    for &v in &vars {
        let most = cons
            .iter()
            .filter(|c| c.at_least && c.vars.contains(&v))
            .map(|c| c.n)
            .max()
            .unwrap_or(0);
        ub[v] = ub[v].min(most);
    }
    // each minimum whose counts are all among those of a maximum
    let mut pairs = Vec::new();
    for (a, ca) in cons.iter().enumerate().filter(|(_, c)| c.at_least) {
        for (b, cb) in cons.iter().enumerate().filter(|(_, c)| !c.at_least) {
            if ca.vars.iter().all(|v| cb.vars.contains(v)) {
                pairs.push((a, b));
            }
        }
    }
    let mut val = vec![0u128; nvars];
    let mut assigned = vec![false; nvars];
    let mut budget = nodes;
    match search(
        &vars,
        0,
        &cons,
        &pairs,
        &ub,
        &mut val,
        &mut assigned,
        &mut budget,
        stop,
    ) {
        Some(true) => Run::Sat,
        Some(false) => Run::Unsat,
        None if stop() => Run::Stopped,
        None => Run::Bound,
    }
}

/// Whether a partial assignment can still satisfy every constraint, each alone and each
/// minimum together with a maximum over a superset of its counts.
fn feasible(
    cons: &[Lin],
    pairs: &[(usize, usize)],
    ub: &[u128],
    val: &[u128],
    assigned: &[bool],
) -> bool {
    // a minimum `a` within a maximum `b`: `a`'s sum is at most `b`'s bound less the
    // assigned counts of `b` outside `a`
    let paired = pairs.iter().all(|&(a, b)| {
        let outside: u128 = cons[b]
            .vars
            .iter()
            .filter(|&&v| assigned[v] && !cons[a].vars.contains(&v))
            .map(|&v| val[v])
            .sum();
        cons[a].n <= cons[b].n.saturating_sub(outside)
    });
    paired
        && cons.iter().all(|c| {
            let mut sum: u128 = 0;
            let mut most: u128 = 0;
            for &v in &c.vars {
                if assigned[v] {
                    sum += val[v];
                    most += val[v];
                } else {
                    most = most.saturating_add(ub[v]);
                }
            }
            if c.at_least { most >= c.n } else { sum <= c.n }
        })
}

#[allow(clippy::too_many_arguments)]
fn search(
    vars: &[usize],
    i: usize,
    cons: &[Lin],
    pairs: &[(usize, usize)],
    ub: &[u128],
    val: &mut [u128],
    assigned: &mut [bool],
    budget: &mut u64,
    stop: &dyn Fn() -> bool,
) -> Option<bool> {
    if *budget == 0 || (budget.is_multiple_of(POLL) && stop()) {
        return None;
    }
    *budget -= 1;
    if !feasible(cons, pairs, ub, val, assigned) {
        return Some(false);
    }
    let Some(&v) = vars.get(i) else {
        return Some(true);
    };
    // the counts of `v` that the constraints allow given the others: at most what each
    // maximum has left, and at least what each minimum still needs beyond the most the
    // other unassigned counts give
    let mut hi = ub[v];
    let mut lo: u128 = 0;
    for c in cons.iter().filter(|c| c.vars.contains(&v)) {
        let mut sum: u128 = 0;
        let mut rest: u128 = 0;
        for &w in c.vars.iter().filter(|&&w| w != v) {
            if assigned[w] {
                sum += val[w];
            } else {
                rest = rest.saturating_add(ub[w]);
            }
        }
        if c.at_least {
            lo = lo.max(c.n.saturating_sub(sum.saturating_add(rest)));
        } else {
            hi = hi.min(c.n.saturating_sub(sum));
        }
    }
    let last = i + 1 == vars.len();
    assigned[v] = true;
    let mut out = Some(false);
    let mut x = hi;
    while x >= lo && hi >= lo {
        val[v] = x;
        match search(vars, i + 1, cons, pairs, ub, val, assigned, budget, stop) {
            Some(true) => {
                out = Some(true);
                break;
            }
            None => {
                out = None;
                break;
            }
            Some(false) => {}
        }
        // with every other count fixed, the bounds above are exact, so one value decides
        if last || x == 0 {
            break;
        }
        x -= 1;
    }
    assigned[v] = false;
    val[v] = 0;
    out
}

/// Decides the problem.
pub fn check(r: &mut Ranges, p: &Problem, b: &Bounds) -> Answer {
    check_stopping(r, p, b, &|| false)
}

/// Decides the problem, answering [`Why::Stopped`] once `stop` returns true. The flag is
/// read between cells and every [`POLL`] search nodes.
pub fn check_stopping(r: &mut Ranges, p: &Problem, b: &Bounds, stop: &dyn Fn() -> bool) -> Answer {
    if stop() {
        return Answer::Unknown(Why::Stopped);
    }
    let np = p.props.len();
    // sets of properties are bit sets of a u64
    if np > b.props.min(63) {
        return Answer::Unknown(Why::Bound);
    }
    // Sizes saturate at 2^63, which is exact only while the counts asked for stay below
    // it. The model's cardinalities are 32-bit, so only a caller's own numbers reach it.
    let demand: u128 = p
        .atoms
        .iter()
        .map(|a| match *a {
            Atom::AtLeast(_, n, _) | Atom::AtMost(_, n, _) => u128::from(n) + 1,
        })
        .sum();
    if demand >= CAP {
        return Answer::Unknown(Why::Uncounted);
    }
    // upward closure of the hierarchy
    let mut up: Vec<u64> = (0..np).map(|i| 1u64 << i).collect();
    loop {
        let mut changed = false;
        for i in 0..np {
            let mut m = up[i];
            for &s in &p.props[i].supers {
                m |= up[s];
            }
            if m != up[i] {
                up[i] = m;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // the distinct ranges involved, by index
    let mut ranges: Vec<Rid> = Vec::new();
    let mut index: FxHashMap<Rid, usize> = FxHashMap::default();
    let atom_ranges = p.atoms.iter().map(|a| {
        let (Atom::AtLeast(_, _, x) | Atom::AtMost(_, _, x)) = *a;
        x
    });
    for x in p
        .props
        .iter()
        .flat_map(|q| q.ranges.iter().copied())
        .chain(atom_ranges)
    {
        index.entry(x).or_insert_with(|| {
            ranges.push(x);
            ranges.len() - 1
        });
    }
    // The cells, each with the side of every range it lies on, which its construction
    // decides: whether a cell is within a range never rests on comparing ids. Cells of
    // different parents are disjoint, so no two are the same set.
    let mut cells: Vec<(Rid, Vec<bool>)> = vec![(Ranges::FULL, Vec::new())];
    for &x in &ranges {
        if stop() {
            return Answer::Unknown(Why::Stopped);
        }
        let nx = r.not(x);
        let mut next = Vec::with_capacity(cells.len() * 2);
        for (c, sides) in &cells {
            for (part, inside) in [(r.meet(*c, x), true), (r.meet(*c, nx), false)] {
                if r.is_empty(part) != Some(true) {
                    let mut s = sides.clone();
                    s.push(inside);
                    next.push((part, s));
                    if next.len() > b.types {
                        return Answer::Unknown(Why::Bound);
                    }
                }
            }
        }
        cells = next;
    }
    let within = |ci: usize, x: Rid| cells[ci].1[index[&x]];
    // the upward-closed sets of properties without a disjoint pair
    let mut sets = Vec::new();
    for s in 1u64..(1 << np) {
        let closed = (0..np).all(|i| s & (1 << i) == 0 || s & up[i] == up[i]);
        let apart = p
            .disjoint
            .iter()
            .all(|&(x, y)| s & (1 << x) == 0 || s & (1 << y) == 0);
        if closed && apart {
            sets.push(s);
        }
    }
    if cells.len().saturating_mul(sets.len()) > b.types.saturating_mul(64) {
        return Answer::Unknown(Why::Bound);
    }
    let mut types = Vec::new();
    for ci in 0..cells.len() {
        for &s in &sets {
            let ok = (0..np)
                .filter(|i| s & (1 << i) != 0)
                .all(|i| p.props[i].ranges.iter().all(|&x| within(ci, x)));
            if ok {
                types.push((ci, s));
                if types.len() > b.types {
                    return Answer::Unknown(Why::Bound);
                }
            }
        }
    }
    let mut cons = Vec::new();
    for a in &p.atoms {
        let (Atom::AtLeast(d, n, x) | Atom::AtMost(d, n, x)) = *a;
        let vars = types
            .iter()
            .enumerate()
            .filter(|(_, (ci, s))| s & (1 << d) != 0 && within(*ci, x))
            .map(|(t, _)| t)
            .collect();
        cons.push(Lin {
            vars,
            at_least: matches!(a, Atom::AtLeast(..)),
            n: u128::from(n),
        });
    }
    let mut by_cell: Vec<Vec<usize>> = vec![Vec::new(); cells.len()];
    for (t, (ci, _)) in types.iter().enumerate() {
        by_cell[*ci].push(t);
    }
    let cells: Vec<Rid> = cells.into_iter().map(|(c, _)| c).collect();
    let sizes: Vec<_> = cells.iter().map(|&c| r.size(c)).collect();
    let exact = sizes.iter().all(|s| s.is_exact());
    let with_caps = |cap: &dyn Fn(usize) -> Option<u128>| {
        let mut all = cons.clone();
        for (ci, vars) in by_cell.iter().enumerate() {
            if let Some(n) = cap(ci) {
                all.push(Lin {
                    vars: vars.clone(),
                    at_least: false,
                    n,
                });
            }
        }
        solve(types.len(), all, b.nodes, stop)
    };
    // at most the upper bound of each cell: unsatisfiable here is unsatisfiable
    let opt = with_caps(&|ci| sizes[ci].hi);
    match opt {
        Run::Unsat => return Answer::Unsat,
        Run::Stopped => return Answer::Unknown(Why::Stopped),
        _ => {}
    }
    if exact {
        return match opt {
            Run::Sat => Answer::Sat,
            _ => Answer::Unknown(Why::Bound),
        };
    }
    // at most the lower bound: satisfiable here is satisfiable
    let pes = with_caps(&|ci| (sizes[ci].lo < CAP).then_some(sizes[ci].lo));
    match (opt, pes) {
        (_, Run::Sat) => Answer::Sat,
        (_, Run::Stopped) => Answer::Unknown(Why::Stopped),
        (Run::Bound, _) | (_, Run::Bound) => Answer::Unknown(Why::Bound),
        _ => Answer::Unknown(Why::Uncounted),
    }
}

#[cfg(test)]
mod tests {
    use super::{Lin, Run, solve};
    use proptest::prelude::*;

    fn brute(nvars: usize, cons: &[Lin], most: u128) -> bool {
        let mut x = vec![0u128; nvars];
        loop {
            let ok = cons.iter().all(|c| {
                let s: u128 = c.vars.iter().map(|&v| x[v]).sum();
                if c.at_least { s >= c.n } else { s <= c.n }
            });
            if ok {
                return true;
            }
            let mut i = 0;
            loop {
                if i == nvars {
                    return false;
                }
                if x[i] < most {
                    x[i] += 1;
                    break;
                }
                x[i] = 0;
                i += 1;
            }
        }
    }

    fn arb_lin(nvars: usize) -> impl Strategy<Value = Lin> {
        (
            proptest::collection::btree_set(0..nvars, 0..=nvars),
            any::<bool>(),
            0u128..=6,
        )
            .prop_map(|(vars, at_least, n)| Lin {
                vars: vars.into_iter().collect(),
                at_least,
                n,
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// The search with its pruning against enumeration. A solution, if any, has one
        /// whose counts are each at most the largest minimum, since lowering a count that
        /// no tight minimum needs keeps every constraint.
        #[test]
        fn solve_matches_enumeration(
            (nvars, cons) in (1usize..=4).prop_flat_map(|n| (Just(n), proptest::collection::vec(arb_lin(n), 0..=5)))
        ) {
            let most = cons.iter().filter(|c| c.at_least).map(|c| c.n).max().unwrap_or(0);
            let want = brute(nvars, &cons, most);
            let got = match solve(nvars, cons.clone(), 1_000_000, &|| false) {
                Run::Sat => true,
                Run::Unsat => false,
                Run::Bound | Run::Stopped => panic!("bound on a small system"),
            };
            prop_assert_eq!(got, want);
        }
    }
}
