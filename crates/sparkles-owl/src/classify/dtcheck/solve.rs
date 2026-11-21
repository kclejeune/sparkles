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
    pub fn negate(self) -> Atom {
        match self {
            Atom::AtLeast(d, 0, _) => Atom::AtLeast(d, 1, Ranges::EMPTY),
            Atom::AtLeast(d, n, r) => Atom::AtMost(d, n - 1, r),
            Atom::AtMost(d, n, r) => Atom::AtLeast(d, n + 1, r),
        }
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
}

/// Whether the integer program has a solution, by preprocessing and a bounded search.
fn solve(nvars: usize, mut cons: Vec<Lin>, nodes: u64) -> Run {
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
    let mut val = vec![0u128; nvars];
    let mut assigned = vec![false; nvars];
    let mut budget = nodes;
    match search(&vars, 0, &cons, &ub, &mut val, &mut assigned, &mut budget) {
        Some(true) => Run::Sat,
        Some(false) => Run::Unsat,
        None => Run::Bound,
    }
}

/// Whether a partial assignment can still satisfy every constraint.
fn feasible(cons: &[Lin], ub: &[u128], val: &[u128], assigned: &[bool]) -> bool {
    cons.iter().all(|c| {
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

fn search(
    vars: &[usize],
    i: usize,
    cons: &[Lin],
    ub: &[u128],
    val: &mut [u128],
    assigned: &mut [bool],
    budget: &mut u64,
) -> Option<bool> {
    if *budget == 0 {
        return None;
    }
    *budget -= 1;
    if !feasible(cons, ub, val, assigned) {
        return Some(false);
    }
    let Some(&v) = vars.get(i) else {
        return Some(true);
    };
    assigned[v] = true;
    let mut x = ub[v];
    let mut out = Some(false);
    loop {
        val[v] = x;
        match search(vars, i + 1, cons, ub, val, assigned, budget) {
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
        if x == 0 {
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
    let np = p.props.len();
    if np > b.props {
        return Answer::Unknown(Why::Bound);
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
    // the cells of every range involved
    let mut ranges: Vec<Rid> = p
        .props
        .iter()
        .flat_map(|q| q.ranges.iter().copied())
        .collect();
    for a in &p.atoms {
        let (Atom::AtLeast(_, _, x) | Atom::AtMost(_, _, x)) = *a;
        ranges.push(x);
    }
    let mut cells = vec![Ranges::FULL];
    for &x in &ranges {
        let nx = r.not(x);
        let mut next = Vec::with_capacity(cells.len() * 2);
        for &c in &cells {
            for part in [r.meet(c, x), r.meet(c, nx)] {
                if r.is_empty(part) != Some(true) && !next.contains(&part) {
                    next.push(part);
                }
            }
        }
        cells = next;
        if cells.len() > b.types {
            return Answer::Unknown(Why::Bound);
        }
    }
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
    let mut types = Vec::new();
    for (ci, &c) in cells.iter().enumerate() {
        for &s in &sets {
            let ok = (0..np)
                .filter(|i| s & (1 << i) != 0)
                .all(|i| p.props[i].ranges.iter().all(|&x| r.meet(c, x) == c));
            if ok {
                types.push((ci, s));
            }
            if types.len() > b.types {
                return Answer::Unknown(Why::Bound);
            }
        }
    }
    let mut cons = Vec::new();
    for a in &p.atoms {
        let (Atom::AtLeast(d, n, x) | Atom::AtMost(d, n, x)) = *a;
        let vars = types
            .iter()
            .enumerate()
            .filter(|(_, (ci, s))| s & (1 << d) != 0 && r.meet(cells[*ci], x) == cells[*ci])
            .map(|(t, _)| t)
            .collect();
        cons.push(Lin {
            vars,
            at_least: matches!(a, Atom::AtLeast(..)),
            n: u128::from(n),
        });
    }
    let sizes: Vec<_> = cells.iter().map(|&c| r.size(c)).collect();
    let exact = sizes.iter().all(|s| s.is_exact());
    let with_caps = |cap: &dyn Fn(usize) -> Option<u128>| {
        let mut all = cons.clone();
        for ci in 0..cells.len() {
            if let Some(n) = cap(ci) {
                all.push(Lin {
                    vars: types
                        .iter()
                        .enumerate()
                        .filter(|(_, (c, _))| *c == ci)
                        .map(|(t, _)| t)
                        .collect(),
                    at_least: false,
                    n,
                });
            }
        }
        solve(types.len(), all, b.nodes)
    };
    // at most the upper bound of each cell: unsatisfiable here is unsatisfiable
    let opt = with_caps(&|ci| sizes[ci].hi);
    if let Run::Unsat = opt {
        return Answer::Unsat;
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
        (Run::Bound, _) | (_, Run::Bound) => Answer::Unknown(Why::Bound),
        _ => Answer::Unknown(Why::Uncounted),
    }
}
