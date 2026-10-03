//! Matching a neighbourhood against a shape's triple expression: candidates per arc from
//! the typing, per-constraint counting for flat shapes, and, for the others,
//! distributions of the arcs over their candidate constraints, each resulting count
//! vector tested for membership in the expression and charged to the partition budget.
//! Semantic actions are enumerated within a matching vector under the same budget.
//!
//! **Membership.** Every triple constraint occurs once in a shape's expression (after
//! `&include` expansion), so a count vector `c` matches the expression iff it can be
//! cut into iterations of every group. For an expression `e` with cardinality `{m,n}`,
//! the set of `j` such that `c` (restricted to `e`'s constraints) splits into `j` parts
//! that each match `e` is an interval:
//!
//! * a triple constraint with `k` arcs: `{ j : j·m ≤ k ≤ j·n }`;
//! * a group: its body's iteration counts `B` are the intersection of the children's
//!   intervals for EachOf (every iteration takes a part of each child) and their sum for
//!   OneOf (every iteration takes one branch); then `{ j : [j·m, j·n] ∩ B ≠ ∅ }`.
//!
//! The vector matches iff the root's interval holds 1. Why this is exact: the children
//! of a group have disjoint constraints, so how one child's arcs are cut into
//! iterations never constrains another's; `j` iterations of an EachOf exist iff every
//! child splits into `j` parts, and of a OneOf iff the branches' part counts add up to
//! `j`. Parts are bags, so `I` body iterations group into `j` iterations of `{m,n}`
//! iff `j·m ≤ I ≤ j·n`. Sums and intersections of integer intervals are intervals, so
//! nothing is lost by keeping only the bounds. The cost is linear in the expression
//! whatever the counts (`(a|b){2}` matches `{a, b}` because the two iterations take
//! different branches); a brute-force enumeration of the partitions the definition
//! describes is the property test's oracle. Bag derivatives by `k` copies of a symbol
//! would compute the same, but they copy group bodies when the copies spread over
//! iterations.
//!
//! **Three-valued reads.** During discovery the typing is unknown. An arc whose value
//! may satisfy a constraint may be assigned to it; an EXTRA arc that satisfies nothing
//! for sure may also stay unmatched. The result is `False` when no choice matches
//! (whatever the unknown values turn out to be), `Unknown` when one does and some read
//! was unknown, and `True` only when every read was known.

use crate::ast::SemAct;
use crate::ir::{Dir, ShapeClass, ShapeIr, TcId, Te, TeId, Tri};
use crate::neigh::Neigh;
use crate::semact::ActCtx;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use sparkles_core::id::Id;
use sparkles_core::{Budget as Exceeded, BudgetKind};

/// The partitions one match may still try ([`crate::ValidateOptions::max_partitions`]).
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// `None`: unlimited
    pub limit: Option<u64>,
    pub used: u64,
}

impl Budget {
    pub fn new(limit: Option<u64>) -> Budget {
        Budget { limit, used: 0 }
    }

    /// Count `n` more partitions; [`sparkles_core::Error::BudgetExceeded`] with the
    /// `validation-work` budget once past the limit.
    pub fn charge(&mut self, n: u64) -> sparkles_core::Result<()> {
        self.used = self.used.saturating_add(n);
        match self.limit {
            Some(limit) if self.used > limit => {
                Err(sparkles_core::Error::BudgetExceeded(Exceeded {
                    kind: BudgetKind::ValidationWork,
                    limit,
                    requested: self.used,
                }))
            }
            _ => Ok(()),
        }
    }
}

/// Does `neigh` match `shape`? `read(value, tc)` tells whether an arc's value satisfies
/// a triple constraint's value expression under the current typing (`Unknown` during
/// discovery); it is not called for constraints without a value expression. Budget
/// errors are [`sparkles_core::Error::BudgetExceeded`].
pub fn matches(
    shape: &ShapeIr,
    neigh: &Neigh,
    read: &dyn Fn(Id, TcId) -> Tri,
    budget: &mut Budget,
    acts: &mut ActCtx<'_>,
) -> anyhow::Result<Tri> {
    matches_with(shape, neigh, read, budget, acts)
}

/// What matching needs of semantic actions: the registry through an [`ActCtx`], or a
/// recorder in tests.
pub(crate) trait Acts {
    fn can_fail(&self, acts: &[SemAct]) -> bool;
    fn on_tc(&mut self, acts: &[SemAct], triple: [Id; 3]) -> anyhow::Result<bool>;
    fn on_group(&mut self, acts: &[SemAct], triples: &[[Id; 3]]) -> anyhow::Result<bool>;
    fn on_shape(&mut self, acts: &[SemAct], focus: Id) -> anyhow::Result<bool>;
    /// Forget what the actions run since [`Acts::mark`] recorded (an assignment whose
    /// actions failed).
    fn mark(&self) -> ActMark;
    fn rollback(&mut self, mark: ActMark);
}

/// A position in what semantic actions recorded.
pub(crate) struct ActMark {
    prints: usize,
    unknown: FxHashMap<String, usize>,
}

impl Acts for ActCtx<'_> {
    fn can_fail(&self, acts: &[SemAct]) -> bool {
        self.registry.can_fail(acts)
    }
    fn on_tc(&mut self, acts: &[SemAct], triple: [Id; 3]) -> anyhow::Result<bool> {
        let r = self.registry;
        r.on_tc(acts, triple, self)
    }
    fn on_group(&mut self, acts: &[SemAct], triples: &[[Id; 3]]) -> anyhow::Result<bool> {
        let r = self.registry;
        r.on_group(acts, triples, self)
    }
    fn on_shape(&mut self, acts: &[SemAct], focus: Id) -> anyhow::Result<bool> {
        let r = self.registry;
        r.on_shape(acts, focus, self)
    }
    fn mark(&self) -> ActMark {
        ActMark {
            prints: self.prints.len(),
            unknown: self.unknown.clone(),
        }
    }
    fn rollback(&mut self, mark: ActMark) {
        self.prints.truncate(mark.prints);
        self.unknown = mark.unknown;
    }
}

/// Unbounded, in counts and intervals.
const INF: u64 = u64::MAX;

/// An arc: its `preds` entry and its value.
type ArcAt = (u32, Id);

/// Arcs with the same choices: the constraints they may be assigned to (and `drop`, the
/// shape's constraint count, for an EXTRA arc that may stay unmatched).
struct Class {
    opts: SmallVec<[u32; 4]>,
    arcs: Vec<ArcAt>,
}

pub(crate) fn matches_with<A: Acts>(
    shape: &ShapeIr,
    neigh: &Neigh,
    read: &dyn Fn(Id, TcId) -> Tri,
    budget: &mut Budget,
    acts: &mut A,
) -> anyhow::Result<Tri> {
    if neigh.closed_violation.is_some() {
        return Ok(Tri::False);
    }
    let arc_acts = shape.tcs.iter().any(|tc| !tc.sem_acts.is_empty())
        || shape.te.iter().any(|t| match t {
            Te::Tc(_) => false,
            Te::EachOf { acts, .. } | Te::OneOf { acts, .. } => !acts.is_empty(),
        });
    let verdict = if shape.class == ShapeClass::Flat && !arc_acts && is_flat(shape) {
        flat(shape, neigh, read)
    } else {
        general(shape, neigh, read, budget, acts, arc_acts)?
    };
    if verdict == Tri::True
        && !shape.sem_acts.is_empty()
        && !acts.on_shape(&shape.sem_acts, neigh.node)?
    {
        return Ok(Tri::False);
    }
    Ok(verdict)
}

/// Is the shape what the counting path assumes: a triple constraint, or an EachOf of
/// triple constraints matched once, with one constraint per (predicate, direction)?
fn is_flat(shape: &ShapeIr) -> bool {
    let tcs_ok = |kids: &[TeId]| {
        kids.iter()
            .all(|k| matches!(shape.te[k.index()], Te::Tc(_)))
    };
    let root_ok = match shape.root {
        None => true,
        Some(r) => match &shape.te[r.index()] {
            Te::Tc(_) => true,
            Te::EachOf {
                kids,
                min: 1,
                max: Some(1),
                ..
            } => tcs_ok(kids),
            _ => false,
        },
    };
    root_ok && shape.preds.iter().all(|(_, _, tcs)| tcs.len() == 1)
}

/// The value of an arc for a constraint: `True` without a value expression.
#[inline]
fn cand(shape: &ShapeIr, read: &dyn Fn(Id, TcId) -> Tri, v: Id, tc: TcId) -> Tri {
    if shape.tcs[tc.index()].value.is_none() {
        Tri::True
    } else {
        read(v, tc)
    }
}

/// Flat shapes: count per constraint while reading the arcs, and stop at the first arc
/// that settles the answer.
fn flat(shape: &ShapeIr, neigh: &Neigh, read: &dyn Fn(Id, TcId) -> Tri) -> Tri {
    let mut unknown = false;
    for (e, (pred, _, tcs)) in shape.preds.iter().enumerate() {
        let tc = tcs[0];
        let t = &shape.tcs[tc.index()];
        let max = t.max.map_or(INF, u64::from);
        let extra = shape.extra.iter().any(|x| x == pred);
        // arcs that must be assigned to tc, and EXTRA arcs that may be
        let (mut sure, mut maybe) = (0u64, 0u64);
        for &v in neigh.values.get(e).map_or(&[][..], |v| v) {
            match cand(shape, read, v, tc) {
                Tri::True => sure += 1,
                Tri::Unknown => {
                    unknown = true;
                    if extra { maybe += 1 } else { sure += 1 }
                }
                Tri::False if extra => {}
                Tri::False => return Tri::False,
            }
            if sure > max {
                return Tri::False;
            }
        }
        if sure + maybe < u64::from(t.min) {
            return Tri::False;
        }
    }
    if unknown { Tri::Unknown } else { Tri::True }
}

/// Shapes in general: candidates per arc, classes of arcs with the same candidates,
/// and distributions of each class over its candidates.
fn general<A: Acts>(
    shape: &ShapeIr,
    neigh: &Neigh,
    read: &dyn Fn(Id, TcId) -> Tri,
    budget: &mut Budget,
    acts: &mut A,
    arc_acts: bool,
) -> anyhow::Result<Tri> {
    let ntc = shape.tcs.len();
    let drop = ntc as u32;
    // arcs with a single choice are counted (and kept when actions need them)
    let mut fixed = vec![0u64; ntc];
    let mut fixed_arcs: Vec<Vec<ArcAt>> = if arc_acts {
        vec![Vec::new(); ntc]
    } else {
        Vec::new()
    };
    let mut classes: Vec<Class> = Vec::new();
    let mut unknown = false;
    for (e, (pred, _, tcs)) in shape.preds.iter().enumerate() {
        let extra = shape.extra.iter().any(|x| x == pred);
        for &v in neigh.values.get(e).map_or(&[][..], |v| v) {
            let mut opts: SmallVec<[u32; 4]> = SmallVec::new();
            let mut sure = false;
            for &tc in tcs {
                match cand(shape, read, v, tc) {
                    Tri::True => sure = true,
                    Tri::Unknown => unknown = true,
                    Tri::False => continue,
                }
                opts.push(tc.0);
            }
            if opts.is_empty() {
                if extra {
                    continue;
                }
                return Ok(Tri::False);
            }
            if extra && !sure {
                opts.push(drop);
            }
            if let [tc] = opts[..] {
                fixed[tc as usize] += 1;
                if arc_acts {
                    fixed_arcs[tc as usize].push((e as u32, v));
                }
            } else {
                match classes.iter_mut().find(|c| c.opts == opts) {
                    Some(c) => c.arcs.push((e as u32, v)),
                    None => classes.push(Class {
                        opts,
                        arcs: vec![(e as u32, v)],
                    }),
                }
            }
        }
    }
    let Some(root) = shape.root else {
        // no expression: no `preds`, so no arcs
        return Ok(if unknown { Tri::Unknown } else { Tri::True });
    };
    // caps (the drop choice is unbounded) and the minimum counts every match needs
    let mut cap: Vec<u64> = shape.max_occ.iter().map(|m| m.unwrap_or(INF)).collect();
    cap.resize(ntc, INF);
    cap.push(INF);
    let mut need = vec![0u64; ntc];
    min_need(shape, root, 1, &mut need);
    if (0..ntc).any(|t| fixed[t] > cap[t]) {
        return Ok(Tri::False);
    }
    // avail[ci * ntc + t]: the arcs of classes ci.. that may go to t
    let mut avail = vec![0u64; (classes.len() + 1) * ntc];
    for ci in (0..classes.len()).rev() {
        let (head, tail) = avail.split_at_mut((ci + 1) * ntc);
        head[ci * ntc..].copy_from_slice(&tail[..ntc]);
        for &o in &classes[ci].opts {
            if o != drop {
                head[ci * ntc + o as usize] += classes[ci].arcs.len() as u64;
            }
        }
    }
    if (0..ntc).any(|t| fixed[t] + avail[t] < need[t]) {
        return Ok(Tri::False);
    }
    let mut counts = fixed.clone();
    counts.push(0);
    let mut search = Search {
        shape,
        root,
        ntc,
        cap,
        need,
        avail,
        classes: &classes,
        counts,
        dist: classes.iter().map(|c| vec![0; c.opts.len()]).collect(),
        memo: FxHashMap::default(),
        found: false,
        act_check: (arc_acts && !unknown).then_some(ActCheck {
            neigh,
            fixed_arcs: &fixed_arcs,
        }),
    };
    search.class(0, budget, acts)?;
    Ok(match (search.found, unknown) {
        (false, _) => Tri::False,
        (true, true) => Tri::Unknown,
        (true, false) => Tri::True,
    })
}

/// The least number of arcs of each constraint in any match: the product of the
/// minimum cardinalities from the constraint up to the root, 0 below a OneOf with
/// several branches.
fn min_need(shape: &ShapeIr, te: TeId, factor: u64, need: &mut [u64]) {
    match &shape.te[te.index()] {
        Te::Tc(tc) => {
            need[tc.index()] = factor.saturating_mul(u64::from(shape.tcs[tc.index()].min))
        }
        Te::EachOf { kids, min, .. } => {
            for &k in kids {
                min_need(shape, k, factor.saturating_mul(u64::from(*min)), need);
            }
        }
        Te::OneOf { kids, min, .. } => {
            let f = if kids.len() == 1 {
                factor.saturating_mul(u64::from(*min))
            } else {
                0
            };
            for &k in kids {
                min_need(shape, k, f, need);
            }
        }
    }
}

/// Does the count vector match the expression once?
pub(crate) fn member(shape: &ShapeIr, root: TeId, counts: &[u64]) -> bool {
    iters(shape, root, counts).is_some_and(|(lo, hi)| lo <= 1 && 1 <= hi)
}

/// The numbers of parts `counts` (restricted to `te`'s constraints) splits into, each
/// matching `te` with its cardinality: an interval, `None` when empty.
fn iters(shape: &ShapeIr, te: TeId, counts: &[u64]) -> Option<(u64, u64)> {
    let (body, min, max) = match &shape.te[te.index()] {
        Te::Tc(tc) => {
            let t = &shape.tcs[tc.index()];
            // each part holds between min and max arcs
            return split(
                counts[tc.index()],
                u64::from(t.min),
                t.max.map_or(INF, u64::from),
            );
        }
        Te::EachOf { kids, min, max, .. } => {
            let mut b = (0, INF);
            for &k in kids {
                let (lo, hi) = iters(shape, k, counts)?;
                b = (b.0.max(lo), b.1.min(hi));
                if b.0 > b.1 {
                    return None;
                }
            }
            (b, min, max)
        }
        Te::OneOf { kids, min, max, .. } => {
            let mut b = (0u64, 0u64);
            for &k in kids {
                let (lo, hi) = iters(shape, k, counts)?;
                b = (b.0.saturating_add(lo), b.1.saturating_add(hi));
            }
            (b, min, max)
        }
    };
    // j parts of m..n iterations each: [j·m, j·n] meets the body's iteration counts
    let (m, n) = (u64::from(*min), max.map_or(INF, u64::from));
    let (blo, bhi) = body;
    let lo = if blo == 0 {
        0
    } else if n == 0 {
        return None;
    } else {
        blo.div_ceil(n)
    };
    let hi = if m == 0 || bhi == INF { INF } else { bhi / m };
    (lo <= hi).then_some((lo, hi))
}

/// `{ j : j·m ≤ k ≤ j·n }` for `k` arcs in parts of `m..=n` arcs.
fn split(k: u64, m: u64, n: u64) -> Option<(u64, u64)> {
    if k == 0 {
        return Some((0, if m == 0 { INF } else { 0 }));
    }
    if n == 0 {
        return None;
    }
    let lo = k.div_ceil(n).max(1);
    let hi = k.checked_div(m).unwrap_or(INF);
    (lo <= hi).then_some((lo, hi))
}

/// What checking semantic actions on an assignment needs.
struct ActCheck<'a> {
    neigh: &'a Neigh,
    fixed_arcs: &'a [Vec<ArcAt>],
}

/// The depth-first enumeration of distributions, class by class.
struct Search<'a> {
    shape: &'a ShapeIr,
    root: TeId,
    ntc: usize,
    cap: Vec<u64>,
    need: Vec<u64>,
    avail: Vec<u64>,
    classes: &'a [Class],
    /// the counts so far (the drop choice last)
    counts: Vec<u64>,
    /// per class, the arcs given to each choice
    dist: Vec<Vec<u64>>,
    /// the membership of the count vectors tested so far
    memo: FxHashMap<Vec<u64>, bool>,
    found: bool,
    act_check: Option<ActCheck<'a>>,
}

impl Search<'_> {
    /// Distribute the classes from `ci` on; stops once `found`.
    fn class<A: Acts>(
        &mut self,
        ci: usize,
        budget: &mut Budget,
        acts: &mut A,
    ) -> anyhow::Result<()> {
        if ci == self.classes.len() {
            return self.leaf(budget, acts);
        }
        let n = self.classes[ci].arcs.len() as u64;
        self.choice(ci, 0, n, budget, acts)
    }

    /// Give `left` arcs of class `ci` to its choices from `oi` on.
    fn choice<A: Acts>(
        &mut self,
        ci: usize,
        oi: usize,
        left: u64,
        budget: &mut Budget,
        acts: &mut A,
    ) -> anyhow::Result<()> {
        let opts = &self.classes[ci].opts;
        if oi == opts.len() {
            if left > 0 {
                return Ok(());
            }
            // the constraints of this class can still reach their minimum
            let base = (ci + 1) * self.ntc;
            let reachable = opts.iter().all(|&o| {
                let o = o as usize;
                o == self.ntc || self.counts[o] + self.avail[base + o] >= self.need[o]
            });
            if reachable {
                self.class(ci + 1, budget, acts)?;
            }
            return Ok(());
        }
        let o = opts[oi] as usize;
        let room = |s: &Self, o: usize| s.cap[o].saturating_sub(s.counts[o]);
        let later: u64 = opts[oi + 1..]
            .iter()
            .fold(0u64, |a, &o| a.saturating_add(room(self, o as usize)));
        let lo = left.saturating_sub(later);
        let hi = left.min(room(self, o));
        for x in lo..=hi {
            self.counts[o] += x;
            self.dist[ci][oi] = x;
            let r = self.choice(ci, oi + 1, left - x, budget, acts);
            self.counts[o] -= x;
            r?;
            if self.found {
                break;
            }
        }
        Ok(())
    }

    /// A complete count vector: test it, then the semantic actions.
    fn leaf<A: Acts>(&mut self, budget: &mut Budget, acts: &mut A) -> anyhow::Result<()> {
        let key = &self.counts[..self.ntc];
        let member = match self.memo.get(key) {
            Some(&m) => m,
            None => {
                budget.charge(1)?;
                let m = member(self.shape, self.root, key);
                self.memo.insert(key.to_vec(), m);
                m
            }
        };
        if member {
            self.found = match &self.act_check {
                None => true,
                Some(check) => assign(self.shape, self.classes, &self.dist, check, budget, acts)?,
            };
        }
        Ok(())
    }
}

/// Enumerate the assignments of the class arcs to choices that a distribution allows,
/// until the semantic actions of one succeed. Each assignment costs one partition.
fn assign<A: Acts>(
    shape: &ShapeIr,
    classes: &[Class],
    dist: &[Vec<u64>],
    check: &ActCheck<'_>,
    budget: &mut Budget,
    acts: &mut A,
) -> anyhow::Result<bool> {
    let can_fail = shape.tcs.iter().any(|t| acts.can_fail(&t.sem_acts))
        || shape.te.iter().any(|t| match t {
            Te::Tc(_) => false,
            Te::EachOf { acts: a, .. } | Te::OneOf { acts: a, .. } => acts.can_fail(a),
        });
    // the class arcs in order, each with its class
    let arcs: Vec<(usize, ArcAt)> = classes
        .iter()
        .enumerate()
        .flat_map(|(ci, c)| c.arcs.iter().map(move |&a| (ci, a)))
        .collect();
    let mut quota: Vec<Vec<u64>> = dist.to_vec();
    let mut choice = vec![0usize; arcs.len()];
    let (mut i, mut next) = (0usize, 0usize);
    loop {
        if i == arcs.len() {
            budget.charge(1)?;
            let mark = can_fail.then(|| acts.mark());
            if run_acts(shape, classes, &arcs, &choice, check, acts)? {
                return Ok(true);
            }
            if let Some(mark) = mark {
                acts.rollback(mark);
            }
            if !can_fail || arcs.is_empty() {
                return Ok(false);
            }
            i -= 1;
            quota[arcs[i].0][choice[i]] += 1;
            next = choice[i] + 1;
            continue;
        }
        let ci = arcs[i].0;
        let e = classes[ci].opts.len();
        let mut o = next;
        while o < e && quota[ci][o] == 0 {
            o += 1;
        }
        if o < e {
            quota[ci][o] -= 1;
            choice[i] = o;
            i += 1;
            next = 0;
        } else {
            if i == 0 {
                return Ok(false);
            }
            i -= 1;
            quota[arcs[i].0][choice[i]] += 1;
            next = choice[i] + 1;
        }
    }
}

/// Run the triple-constraint and group actions on one assignment.
fn run_acts<A: Acts>(
    shape: &ShapeIr,
    classes: &[Class],
    arcs: &[(usize, ArcAt)],
    choice: &[usize],
    check: &ActCheck<'_>,
    acts: &mut A,
) -> anyhow::Result<bool> {
    let ntc = shape.tcs.len();
    // the arcs of each constraint: the fixed ones, then the assigned class arcs
    let mut by_tc: Vec<Vec<ArcAt>> = check.fixed_arcs.to_vec();
    by_tc.resize(ntc, Vec::new());
    for (&(ci, arc), &o) in arcs.iter().zip(choice) {
        let tc = classes[ci].opts[o] as usize;
        if tc < ntc {
            by_tc[tc].push(arc);
        }
    }
    let triple = |&(e, v): &ArcAt| {
        let dir: Dir = shape.preds[e as usize].1;
        check.neigh.triple(dir, e as usize, v)
    };
    for (t, tc) in shape.tcs.iter().enumerate() {
        if tc.sem_acts.is_empty() {
            continue;
        }
        for arc in &by_tc[t] {
            if !acts.on_tc(&tc.sem_acts, triple(arc))? {
                return Ok(false);
            }
        }
    }
    for (i, te) in shape.te.iter().enumerate() {
        let (Te::EachOf { acts: a, .. } | Te::OneOf { acts: a, .. }) = te else {
            continue;
        };
        if a.is_empty() {
            continue;
        }
        let mut tcs = Vec::new();
        tcs_under(shape, TeId(i as u32), &mut tcs);
        let triples: Vec<[Id; 3]> = tcs
            .iter()
            .flat_map(|t| by_tc[t.index()].iter().map(triple))
            .collect();
        if !acts.on_group(a, &triples)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The triple constraints of a triple expression, in order.
fn tcs_under(shape: &ShapeIr, te: TeId, out: &mut Vec<TcId>) {
    match &shape.te[te.index()] {
        Te::Tc(tc) => out.push(*tc),
        Te::EachOf { kids, .. } | Te::OneOf { kids, .. } => {
            for &k in kids {
                tcs_under(shape, k, out);
            }
        }
    }
}

/// Shapes built by hand, for the tests of the matcher and the typing.
#[cfg(test)]
pub(crate) mod build {
    use crate::ast::SemAct;
    use crate::ir::{Dir, SeId, ShapeClass, ShapeIr, TcId, TcIr, Te, TeId};
    use smallvec::SmallVec;

    #[derive(Default)]
    pub struct Builder {
        pub tcs: Vec<TcIr>,
        pub te: Vec<Te>,
    }

    impl Builder {
        /// A triple constraint on `pred` (`max`: `None` unbounded) whose value
        /// expression is `value` (`None`: any value).
        pub fn tc(
            &mut self,
            pred: &str,
            dir: Dir,
            value: Option<SeId>,
            min: u32,
            max: Option<u32>,
        ) -> TeId {
            let id = TcId(self.tcs.len() as u32);
            self.tcs.push(TcIr {
                pred: pred.to_string(),
                dir,
                value,
                pair: None,
                min,
                max,
                sem_acts: Vec::new(),
            });
            self.push(Te::Tc(id))
        }

        /// `tc` with a value expression, outgoing.
        pub fn p(&mut self, pred: &str, min: u32, max: Option<u32>) -> TeId {
            self.tc(pred, Dir::Out, Some(SeId(0)), min, max)
        }

        pub fn each(&mut self, kids: Vec<TeId>, min: u32, max: Option<u32>) -> TeId {
            self.push(Te::EachOf {
                kids,
                min,
                max,
                acts: Vec::new(),
            })
        }

        pub fn one(&mut self, kids: Vec<TeId>, min: u32, max: Option<u32>) -> TeId {
            self.push(Te::OneOf {
                kids,
                min,
                max,
                acts: Vec::new(),
            })
        }

        pub fn acts(&mut self, te: TeId, acts: Vec<SemAct>) {
            match &mut self.te[te.index()] {
                Te::Tc(tc) => self.tcs[tc.index()].sem_acts = acts,
                Te::EachOf { acts: a, .. } | Te::OneOf { acts: a, .. } => *a = acts,
            }
        }

        fn push(&mut self, te: Te) -> TeId {
            self.te.push(te);
            TeId(self.te.len() as u32 - 1)
        }

        /// The shape with expression `root`: `preds`, `max_occ` and the class as the
        /// compiler computes them.
        pub fn shape(self, root: Option<TeId>, extra: &[&str], closed: bool) -> ShapeIr {
            let mut preds: Vec<(String, Dir, SmallVec<[TcId; 2]>)> = Vec::new();
            for (i, tc) in self.tcs.iter().enumerate() {
                let id = TcId(i as u32);
                match preds
                    .iter_mut()
                    .find(|(p, d, _)| *p == tc.pred && *d == tc.dir)
                {
                    Some(e) => e.2.push(id),
                    None => preds.push((tc.pred.clone(), tc.dir, SmallVec::from_elem(id, 1))),
                }
            }
            let mut max_occ = vec![Some(0); self.tcs.len()];
            if let Some(r) = root {
                occ(&self, r, Some(1), &mut max_occ);
            }
            let det = preds.iter().all(|(_, _, t)| t.len() == 1);
            let flat_root = match root.map(|r| &self.te[r.index()]) {
                None | Some(Te::Tc(_)) => true,
                Some(Te::EachOf {
                    kids,
                    min: 1,
                    max: Some(1),
                    ..
                }) => kids.iter().all(|k| matches!(self.te[k.index()], Te::Tc(_))),
                _ => false,
            };
            let class = match (det, flat_root) {
                (true, true) => ShapeClass::Flat,
                (true, false) => ShapeClass::Deterministic,
                _ => ShapeClass::Ambiguous,
            };
            ShapeIr {
                closed,
                extra: extra.iter().map(|s| s.to_string()).collect(),
                tcs: self.tcs,
                te: self.te,
                root,
                preds,
                max_occ,
                class,
                sem_acts: Vec::new(),
            }
        }
    }

    fn occ(b: &Builder, te: TeId, factor: Option<u64>, out: &mut [Option<u64>]) {
        let mul = |f: Option<u64>, m: Option<u32>| match (f, m) {
            (Some(f), Some(m)) => Some(f * u64::from(m)),
            (Some(0), None) | (None, Some(0)) => Some(0),
            _ => None,
        };
        match &b.te[te.index()] {
            Te::Tc(tc) => out[tc.index()] = mul(factor, b.tcs[tc.index()].max),
            Te::EachOf { kids, max, .. } | Te::OneOf { kids, max, .. } => {
                for &k in kids {
                    occ(b, k, mul(factor, *max), out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::build::Builder;
    use super::*;
    use crate::ir::Dir;
    use proptest::prelude::*;
    use std::collections::HashMap;
    use std::time::Instant;

    /// Semantic actions for the tests: `reject N` fails on a triple whose object is
    /// `Id(N)`, `size N` fails unless a group matched N triples; every run is logged.
    #[derive(Default)]
    struct Recorder {
        log: Vec<String>,
    }

    impl Acts for Recorder {
        fn can_fail(&self, acts: &[SemAct]) -> bool {
            !acts.is_empty()
        }
        fn on_tc(&mut self, acts: &[SemAct], t: [Id; 3]) -> anyhow::Result<bool> {
            self.log.push(format!("tc {}", t[2].0));
            let o = t[2].0.to_string();
            Ok(acts.iter().all(|a| {
                a.code.as_deref().and_then(|c| c.strip_prefix("reject ")) != Some(o.as_str())
            }))
        }
        fn on_group(&mut self, acts: &[SemAct], ts: &[[Id; 3]]) -> anyhow::Result<bool> {
            self.log.push(format!("group {}", ts.len()));
            let n = ts.len().to_string();
            Ok(acts.iter().all(|a| {
                a.code
                    .as_deref()
                    .and_then(|c| c.strip_prefix("size "))
                    .is_none_or(|m| m == n)
            }))
        }
        fn on_shape(&mut self, _: &[SemAct], focus: Id) -> anyhow::Result<bool> {
            self.log.push(format!("shape {}", focus.0));
            Ok(true)
        }
        fn mark(&self) -> ActMark {
            ActMark {
                prints: self.log.len(),
                unknown: FxHashMap::default(),
            }
        }
        fn rollback(&mut self, mark: ActMark) {
            self.log.truncate(mark.prints);
        }
    }

    fn act(code: &str) -> Vec<SemAct> {
        vec![SemAct {
            name: "test".into(),
            code: Some(code.into()),
        }]
    }

    /// A neighbourhood of node `Id(7)` with the given values per `preds` entry.
    fn neigh(values: Vec<Vec<u64>>) -> Neigh {
        Neigh {
            node: Id(7),
            preds: (0..values.len() as u64).map(|i| Id(100 + i)).collect(),
            values: values
                .into_iter()
                .map(|v| v.into_iter().map(Id).collect())
                .collect(),
            closed_violation: None,
        }
    }

    fn run(shape: &ShapeIr, n: &Neigh, read: &dyn Fn(Id, TcId) -> Tri) -> Tri {
        let mut b = Budget::new(Some(100_000));
        matches_with(shape, n, read, &mut b, &mut Recorder::default()).unwrap()
    }

    fn all_ok(_: Id, _: TcId) -> Tri {
        Tri::True
    }

    #[test]
    fn budget_is_validation_work() {
        let mut b = Budget::new(Some(3));
        b.charge(3).unwrap();
        match b.charge(1) {
            Err(sparkles_core::Error::BudgetExceeded(e)) => {
                assert_eq!(
                    (e.kind, e.limit, e.requested),
                    (BudgetKind::ValidationWork, 3, 4)
                )
            }
            other => panic!("{other:?}"),
        }
        Budget::new(None).charge(u64::MAX).unwrap();
    }

    #[test]
    fn intervals() {
        assert_eq!(split(0, 1, 1), Some((0, 0)));
        assert_eq!(split(0, 0, 3), Some((0, INF)));
        assert_eq!(split(5, 2, 3), Some((2, 2)));
        assert_eq!(split(7, 2, 3), Some((3, 3)));
        assert_eq!(split(1, 2, 3), None);
        assert_eq!(split(4, 0, INF), Some((1, INF)));
        assert_eq!(split(3, 1, 0), None);
    }

    #[test]
    fn one_of_iterations_take_different_branches() {
        // (a | b){2} matches {a, b}
        let mut b = Builder::default();
        let (a, bb) = (b.p("a", 1, Some(1)), b.p("b", 1, Some(1)));
        let root = b.one(vec![a, bb], 2, Some(2));
        let s = b.shape(Some(root), &[], false);
        assert_eq!(s.class, ShapeClass::Deterministic);
        assert!(member(&s, root, &[1, 1]));
        assert!(member(&s, root, &[2, 0]));
        assert!(!member(&s, root, &[1, 0]));
        assert!(!member(&s, root, &[2, 1]));
        assert_eq!(run(&s, &neigh(vec![vec![1], vec![2]]), &all_ok), Tri::True);
        assert_eq!(run(&s, &neigh(vec![vec![1], vec![]]), &all_ok), Tri::False);
    }

    #[test]
    fn group_cardinality_spreads_counts() {
        // (a{1,2} ; b){1,3}
        let mut b = Builder::default();
        let (a, bb) = (b.p("a", 1, Some(2)), b.p("b", 1, Some(1)));
        let root = b.each(vec![a, bb], 1, Some(3));
        let s = b.shape(Some(root), &[], false);
        for (counts, ok) in [
            ([3, 2], true),
            ([3, 3], true),
            ([3, 1], false),
            ([6, 3], true),
            ([7, 3], false),
            ([0, 0], false),
        ] {
            assert_eq!(member(&s, root, &counts), ok, "{counts:?}");
        }
    }

    #[test]
    fn flat_counting() {
        // { a . ; b . ? ; c . * }
        let mut b = Builder::default();
        let kids = vec![
            b.p("a", 1, Some(1)),
            b.p("b", 0, Some(1)),
            b.p("c", 0, None),
        ];
        let root = b.each(kids, 1, Some(1));
        let s = b.shape(Some(root), &[], false);
        assert_eq!(s.class, ShapeClass::Flat);
        let ok = |v: Id, _| Tri::from(v.0 < 50);
        let n = |a: Vec<u64>, c: Vec<u64>| neigh(vec![a, vec![], c]);
        assert_eq!(run(&s, &n(vec![1], vec![2, 3]), &ok), Tri::True);
        assert_eq!(run(&s, &n(vec![], vec![]), &ok), Tri::False);
        assert_eq!(run(&s, &n(vec![1, 2], vec![]), &ok), Tri::False);
        // a failing value without EXTRA
        assert_eq!(run(&s, &n(vec![1], vec![60]), &ok), Tri::False);
        let unknown = |v: Id, _| if v.0 == 2 { Tri::Unknown } else { Tri::True };
        assert_eq!(run(&s, &n(vec![1], vec![2]), &unknown), Tri::Unknown);
        assert_eq!(run(&s, &n(vec![1, 2], vec![]), &unknown), Tri::False);
    }

    /// `EXTRA knows { knows @Person }` and the same without EXTRA, on a node that knows
    /// people of whom the even ones conform.
    #[test]
    fn extra() {
        let shape = |extra: &[&str]| {
            let mut b = Builder::default();
            let root = b.p("knows", 1, Some(1));
            b.shape(Some(root), extra, false)
        };
        let person = |v: Id, _| Tri::from(v.0.is_multiple_of(2));
        let (with, without) = (shape(&["knows"]), shape(&[]));
        // one conforming and one nonconforming person: the latter is an EXTRA arc
        let n = neigh(vec![vec![2, 3]]);
        assert_eq!(run(&with, &n, &person), Tri::True);
        assert_eq!(run(&without, &n, &person), Tri::False);
        // two conforming people: a matching arc can't stay in the remainder
        let n = neigh(vec![vec![2, 4]]);
        assert_eq!(run(&with, &n, &person), Tri::False);
        assert_eq!(run(&without, &n, &person), Tri::False);
        // undecided values: an EXTRA arc may or may not match
        let maybe = |v: Id, _| {
            if v.0 == 3 {
                Tri::Unknown
            } else {
                Tri::from(v.0.is_multiple_of(2))
            }
        };
        assert_eq!(run(&with, &neigh(vec![vec![2, 3]]), &maybe), Tri::Unknown);
        assert_eq!(run(&with, &neigh(vec![vec![2, 4, 3]]), &maybe), Tri::False);
    }

    #[test]
    fn inverse_cardinality() {
        // { ^parentOf @Person {1,2} } with three parents
        let mut b = Builder::default();
        let root = b.tc("parentOf", Dir::In, Some(crate::ir::SeId(0)), 1, Some(2));
        let s = b.shape(Some(root), &[], false);
        assert_eq!(run(&s, &neigh(vec![vec![1, 2, 3]]), &all_ok), Tri::False);
        assert_eq!(run(&s, &neigh(vec![vec![1, 2]]), &all_ok), Tri::True);
    }

    #[test]
    fn closed_violation_fails() {
        let mut b = Builder::default();
        let root = b.p("a", 0, None);
        let s = b.shape(Some(root), &[], true);
        let mut n = neigh(vec![vec![1]]);
        assert_eq!(run(&s, &n, &all_ok), Tri::True);
        n.closed_violation = Some((Id(9), Id(1)));
        assert_eq!(run(&s, &n, &all_ok), Tri::False);
        // `{}`: nothing to match
        let empty = Builder::default().shape(None, &[], false);
        assert_eq!(run(&empty, &neigh(vec![]), &all_ok), Tri::True);
    }

    #[test]
    fn ambiguous_distributions() {
        // { p [1] ; p @<A> ; p . } where values 1 and 2 conform to A
        let mut b = Builder::default();
        let kids = vec![
            b.p("p", 1, Some(1)),
            b.p("p", 1, Some(1)),
            b.p("p", 1, Some(1)),
        ];
        let root = b.each(kids, 1, Some(1));
        let s = b.shape(Some(root), &[], false);
        assert_eq!(s.class, ShapeClass::Ambiguous);
        let read = |v: Id, tc: TcId| {
            Tri::from(match tc.0 {
                0 => v.0 == 1,
                1 => v.0 <= 2,
                _ => true,
            })
        };
        assert_eq!(run(&s, &neigh(vec![vec![1, 2, 3]]), &read), Tri::True);
        assert_eq!(run(&s, &neigh(vec![vec![1, 3, 4]]), &read), Tri::False);
        assert_eq!(run(&s, &neigh(vec![vec![1, 2]]), &read), Tri::False);
    }

    /// `{ (p .{1,250} | p .{1,250} | p .{1,250}){2} }` on 1000 `p` arcs: two
    /// iterations hold at most 500 arcs, so no distribution matches, and the caps of 500
    /// arcs per constraint leave about 125k of them.
    #[test]
    fn pathological_shape_exceeds_the_budget_quickly() {
        let mut b = Builder::default();
        let kids = vec![
            b.tc("p", Dir::Out, None, 1, Some(250)),
            b.tc("p", Dir::Out, None, 1, Some(250)),
            b.tc("p", Dir::Out, None, 1, Some(250)),
        ];
        let root = b.one(kids, 2, Some(2));
        let s = b.shape(Some(root), &[], false);
        let n = neigh(vec![(1..=1000).collect()]);
        let t = Instant::now();
        let mut budget = Budget::new(Some(1000));
        let r = matches_with(&s, &n, &all_ok, &mut budget, &mut Recorder::default());
        assert!(t.elapsed().as_secs_f64() < 1.0, "{:?}", t.elapsed());
        match r.unwrap_err().downcast::<sparkles_core::Error>() {
            Ok(sparkles_core::Error::BudgetExceeded(e)) => {
                assert_eq!((e.kind, e.limit), (BudgetKind::ValidationWork, 1000))
            }
            other => panic!("{other:?}"),
        }
        // the ambiguity example `{ p [1 2] ; p [2 3] ; p . * }`: few arcs are
        // ambiguous, so it completes
        let mut b = Builder::default();
        let kids = vec![
            b.p("p", 1, Some(1)),
            b.p("p", 1, Some(1)),
            b.p("p", 0, None),
        ];
        let root = b.each(kids, 1, Some(1));
        let s = b.shape(Some(root), &[], false);
        let read = |v: Id, tc: TcId| {
            Tri::from(match tc.0 {
                0 => v.0 == 1 || v.0 == 2,
                1 => v.0 == 2 || v.0 == 3,
                _ => true,
            })
        };
        let n = neigh(vec![(1..=1000).collect()]);
        let mut budget = Budget::new(Some(1000));
        let r = matches_with(&s, &n, &read, &mut budget, &mut Recorder::default());
        assert_eq!(r.unwrap(), Tri::True);
    }

    #[test]
    fn semantic_actions_pick_an_assignment() {
        // { p . %reject 1% ; p . } on {1, 2}: only 2 may go to the first constraint
        let shape = |both: bool| {
            let mut b = Builder::default();
            let (x, y) = (b.p("p", 1, Some(1)), b.p("p", 1, Some(1)));
            b.acts(x, act("reject 1"));
            if both {
                b.acts(y, act("reject 1"));
            }
            let root = b.each(vec![x, y], 1, Some(1));
            b.shape(Some(root), &[], false)
        };
        let n = neigh(vec![vec![1, 2]]);
        let mut budget = Budget::new(None);
        let mut rec = Recorder::default();
        let r = matches_with(&shape(false), &n, &all_ok, &mut budget, &mut rec);
        assert_eq!(r.unwrap(), Tri::True);
        // the failed assignment's runs are forgotten
        assert_eq!(rec.log, vec!["tc 2"]);
        // value 1 rejected by both: no assignment
        let mut rec = Recorder::default();
        let r = matches_with(&shape(true), &n, &all_ok, &mut budget, &mut rec);
        assert_eq!(r.unwrap(), Tri::False);
        assert!(rec.log.is_empty());
    }

    #[test]
    fn group_actions_see_the_group_arcs() {
        // { (a . ; b .){2} %size 4% } runs once, on the four arcs
        let mut b = Builder::default();
        let kids = vec![b.p("a", 1, Some(1)), b.p("b", 1, Some(1))];
        let g = b.each(kids, 2, Some(2));
        b.acts(g, act("size 4"));
        let mut s = b.shape(Some(g), &[], false);
        s.sem_acts = act("shape");
        let n = neigh(vec![vec![1, 2], vec![3, 4]]);
        let mut budget = Budget::new(None);
        let mut rec = Recorder::default();
        let r = matches_with(&s, &n, &all_ok, &mut budget, &mut rec);
        assert_eq!(r.unwrap(), Tri::True);
        assert_eq!(rec.log, vec!["group 4", "shape 7"]);
        // undecided reads run nothing
        let mut rec = Recorder::default();
        let maybe = |_: Id, _: TcId| Tri::Unknown;
        let r = matches_with(&s, &n, &maybe, &mut budget, &mut rec);
        assert_eq!(r.unwrap(), Tri::Unknown);
        assert!(rec.log.is_empty());
    }

    // ------------------------------------------------ against brute force ------

    /// A generated triple expression: constraints on predicate 0 or 1.
    #[derive(Clone, Debug)]
    enum Gen {
        Tc(u32, Option<u32>, usize),
        Each(Vec<Gen>, u32, Option<u32>),
        One(Vec<Gen>, u32, Option<u32>),
    }

    fn card() -> impl Strategy<Value = (u32, Option<u32>)> {
        (0..3u32, prop_oneof![Just(None), (0..4u32).prop_map(Some)])
            .prop_map(|(min, max)| (min, max.map(|m| m.max(min))))
    }

    fn gen_te() -> impl Strategy<Value = Gen> {
        let leaf = (card(), 0..2usize).prop_map(|((min, max), p)| Gen::Tc(min, max, p));
        leaf.prop_recursive(3, 6, 3, |inner| {
            prop_oneof![
                (prop::collection::vec(inner.clone(), 1..4), card())
                    .prop_map(|(k, (min, max))| Gen::Each(k, min, max)),
                (prop::collection::vec(inner, 1..4), card())
                    .prop_map(|(k, (min, max))| Gen::One(k, min, max)),
            ]
        })
    }

    fn lower(b: &mut Builder, g: &Gen) -> TeId {
        match g {
            Gen::Tc(min, max, p) => b.p(["p", "q"][*p], *min, *max),
            Gen::Each(kids, min, max) => {
                let kids = kids.iter().map(|k| lower(b, k)).collect();
                b.each(kids, *min, *max)
            }
            Gen::One(kids, min, max) => {
                let kids = kids.iter().map(|k| lower(b, k)).collect();
                b.one(kids, *min, *max)
            }
        }
    }

    /// The constraints under a triple expression.
    fn syms(s: &ShapeIr, te: TeId) -> Vec<usize> {
        let mut out = Vec::new();
        tcs_under(s, te, &mut out);
        out.into_iter().map(|t| t.index()).collect()
    }

    type Memo = HashMap<(u32, Vec<u64>, u32, Option<u32>), bool>;

    /// The definition: does `bag` (zero outside `te`) match `te` with its cardinality?
    fn bf_once(s: &ShapeIr, te: TeId, bag: &[u64], memo: &mut Memo) -> bool {
        match &s.te[te.index()] {
            Te::Tc(tc) => {
                let t = &s.tcs[tc.index()];
                let k = bag[tc.index()];
                u64::from(t.min) <= k && t.max.is_none_or(|m| k <= u64::from(m))
            }
            Te::EachOf { min, max, .. } | Te::OneOf { min, max, .. } => {
                bf_rep(s, te, bag, *min, *max, memo)
            }
        }
    }

    /// Can `bag` be split into j ∈ [min, max] parts that each match `te`'s body once?
    fn bf_rep(
        s: &ShapeIr,
        te: TeId,
        bag: &[u64],
        min: u32,
        max: Option<u32>,
        memo: &mut Memo,
    ) -> bool {
        if bag.iter().all(|&c| c == 0) && min == 0 {
            return true;
        }
        if max == Some(0) {
            return false;
        }
        let key = (te.0, bag.to_vec(), min, max);
        if let Some(&r) = memo.get(&key) {
            return r;
        }
        // the first part: any sub-bag (the empty one only to reach min)
        let mut part = vec![0u64; bag.len()];
        let mut r = false;
        'parts: loop {
            let nonempty = part.iter().any(|&c| c > 0);
            if (nonempty || min > 0) && bf_body(s, te, &part, memo) {
                let rest: Vec<u64> = bag.iter().zip(&part).map(|(b, p)| b - p).collect();
                let max = max.map(|m| m - 1);
                if bf_rep(s, te, &rest, min.saturating_sub(1), max, memo) {
                    r = true;
                    break;
                }
            }
            for i in 0..part.len() {
                if part[i] < bag[i] {
                    part[i] += 1;
                    continue 'parts;
                }
                part[i] = 0;
            }
            break;
        }
        memo.insert(key, r);
        r
    }

    fn restrict(bag: &[u64], syms: &[usize]) -> Vec<u64> {
        (0..bag.len())
            .map(|i| if syms.contains(&i) { bag[i] } else { 0 })
            .collect()
    }

    fn bf_body(s: &ShapeIr, te: TeId, part: &[u64], memo: &mut Memo) -> bool {
        match &s.te[te.index()] {
            Te::Tc(_) => unreachable!(),
            Te::EachOf { kids, .. } => kids
                .iter()
                .all(|&k| bf_once(s, k, &restrict(part, &syms(s, k)), memo)),
            Te::OneOf { kids, .. } => kids.iter().any(|&k| {
                let sy = syms(s, k);
                part.iter()
                    .enumerate()
                    .all(|(i, &c)| c == 0 || sy.contains(&i))
                    && bf_once(s, k, part, memo)
            }),
        }
    }

    /// The definition on arcs: some assignment of each arc to a constraint its value
    /// satisfies (an EXTRA arc that satisfies none stays unmatched) matches.
    fn bf_matches(s: &ShapeIr, arcs: &[(usize, Id)], ok: &dyn Fn(Id, TcId) -> bool) -> bool {
        let root = s.root.unwrap();
        let mut choices: Vec<Vec<usize>> = Vec::new();
        for &(e, v) in arcs {
            let (pred, _, tcs) = &s.preds[e];
            let c: Vec<usize> = tcs
                .iter()
                .filter(|&&t| ok(v, t))
                .map(|t| t.index())
                .collect();
            if c.is_empty() {
                if s.extra.contains(pred) {
                    continue;
                }
                return false;
            }
            choices.push(c);
        }
        let mut memo = Memo::new();
        let mut pick = vec![0usize; choices.len()];
        loop {
            let mut bag = vec![0u64; s.tcs.len()];
            for (c, &i) in choices.iter().zip(&pick) {
                bag[c[i]] += 1;
            }
            if bf_once(s, root, &bag, &mut memo) {
                return true;
            }
            let mut i = 0;
            loop {
                if i == pick.len() {
                    return false;
                }
                pick[i] += 1;
                if pick[i] < choices[i].len() {
                    break;
                }
                pick[i] = 0;
                i += 1;
            }
        }
    }

    type Case = (Gen, Vec<usize>, Vec<Vec<u8>>, [bool; 2]);

    /// A generated case: the expression, the predicate of each arc, a read per (arc,
    /// constraint) (0 false, 1 true, 2 unknown) and which predicates are EXTRA.
    fn case() -> impl Strategy<Value = Case> {
        (
            gen_te(),
            prop::collection::vec(0..2usize, 0..=8),
            prop::collection::vec(prop::collection::vec(0..3u8, 16), 8),
            any::<[bool; 2]>(),
        )
    }

    /// The shape, the neighbourhood, and the arcs as (entry, value); arc i has value
    /// i + 1.
    fn setup(g: &Gen, arc_preds: &[usize], extra: [bool; 2]) -> (ShapeIr, Neigh, Vec<(usize, Id)>) {
        let mut b = Builder::default();
        let root = lower(&mut b, g);
        let names: Vec<&str> = ["p", "q"]
            .iter()
            .zip(extra)
            .filter_map(|(p, e)| e.then_some(*p))
            .collect();
        let s = b.shape(Some(root), &names, false);
        let mut values = vec![Vec::new(); s.preds.len()];
        let mut arcs = Vec::new();
        for (i, &p) in arc_preds.iter().enumerate() {
            let name = ["p", "q"][p];
            if let Some(e) = s.preds.iter().position(|(n, _, _)| n == name) {
                values[e].push(i as u64 + 1);
                arcs.push((e, Id(i as u64 + 1)));
            }
        }
        (s, neigh(values), arcs)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(500))]

        /// Known reads: the matcher agrees with the definition.
        #[test]
        fn matches_the_definition((g, arc_preds, reads, extra) in case()) {
            let (s, n, arcs) = setup(&g, &arc_preds, extra);
            prop_assume!(s.tcs.len() <= 16);
            let ok = |v: Id, tc: TcId| reads[v.0 as usize - 1][tc.index()] != 0;
            let read = |v: Id, tc: TcId| Tri::from(ok(v, tc));
            let want = bf_matches(&s, &arcs, &ok);
            let mut budget = Budget::new(None);
            let got = matches_with(&s, &n, &read, &mut budget, &mut Recorder::default()).unwrap();
            prop_assert_eq!(got, Tri::from(want), "{:?}", g);
        }

        /// Unknown reads: `True` and `False` hold whatever the unknown values are.
        #[test]
        fn three_valued_reads_are_sound((g, arc_preds, reads, extra) in case()) {
            let (s, n, arcs) = setup(&g, &arc_preds, extra);
            prop_assume!(s.tcs.len() <= 16);
            let tri = |v: Id, tc: TcId| match reads[v.0 as usize - 1][tc.index()] {
                0 => Tri::False,
                1 => Tri::True,
                _ => Tri::Unknown,
            };
            let unknowns: Vec<(Id, TcId)> = arcs
                .iter()
                .flat_map(|&(e, v)| s.preds[e].2.iter().map(move |&t| (v, t)))
                .filter(|&(v, t)| tri(v, t) == Tri::Unknown)
                .collect();
            prop_assume!(unknowns.len() <= 8);
            let mut budget = Budget::new(None);
            let got = matches_with(&s, &n, &tri, &mut budget, &mut Recorder::default()).unwrap();
            // the outcomes over every completion of the unknown reads
            let mut seen = [false; 2];
            for bits in 0u32..(1 << unknowns.len()) {
                let ok = |v: Id, t: TcId| match tri(v, t) {
                    Tri::Unknown => {
                        let i = unknowns.iter().position(|&u| u == (v, t)).unwrap();
                        bits & (1 << i) != 0
                    }
                    x => x == Tri::True,
                };
                seen[usize::from(bf_matches(&s, &arcs, &ok))] = true;
            }
            match got {
                Tri::True => prop_assert!(!seen[0] && unknowns.is_empty()),
                Tri::False => prop_assert!(!seen[1]),
                Tri::Unknown => prop_assert!(!unknowns.is_empty()),
            }
        }
    }
}
