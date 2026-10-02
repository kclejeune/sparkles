//! Join estimates for subject stars from characteristic sets.
//!
//! The planner estimates a join from the distinct values of its variable on either side,
//! assuming that the values of the side with fewer are all among the other side's, less
//! QLever's correction of 0.7. For patterns on one subject, `?s p1 ?a . ?s p2 ?b`, that
//! holds when every subject with `p1` has `p2`, but often subjects have each predicate
//! independently of the others, and then the estimate is too high. The number of rows
//! each subject has for `p2` also differs between the subjects that have `p1` and those
//! that do not.
//!
//! The statistics count the characteristic sets of the subjects (Neumann and Moerkotte,
//! "Characteristic Sets: Accurate Cardinality Estimation for RDF Queries with Multiple
//! Joins", ICDE 2011): each set of predicates that some subject has exactly, with its
//! subjects and the triples of each predicate over them. The subjects that have every
//! predicate of `Q` are those of the sets that hold `Q`, and the rows of the star of `Q`
//! (one pattern `?s p ?o` per predicate) are, per set, its subjects times the average
//! triples per subject of each predicate. Both count over every graph.
//!
//! A join on a subject variable `?s` whose inputs hold patterns `?s p o` with constant
//! predicates, `Qa` on one side and `Qb` on the other, then keeps of the product of the
//! inputs' rows the share `rows(Qa ∪ Qb) / (rows(Qa) · rows(Qb))`, and of the product of
//! their distinct subjects the share `subjects(Qa ∪ Qb) / (subjects(Qa) · subjects(Qb))`.
//! For inputs that are exactly the stars this is the estimate from the sets. Whatever
//! else restricts an input (constant objects, filters, other joins) is taken to be
//! independent of the predicates its subjects have.
//!
//! The predicates of a query's stars are registered per subject variable before its
//! groups are ordered, and the sets are summed once per combination of them that some set
//! holds. A predicate is used only when the sets kept in the statistics hold nearly all of
//! its triples. Otherwise, and when a predicate occurs twice among the inputs, the join
//! keeps the estimate from distinct values.

use super::ctx::Ctx;
use super::plan::{Kind, Node, ScanSpec};
use super::table::VarId;
use crate::builder::Stats;
use crate::index::{O, P, S};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// The bit of a variable's star mask that marks predicates the sets cannot answer for:
/// unregistered, too rarely kept, or present twice.
pub(super) const BAD: u64 = 1 << 63;

/// Predicates registered per subject variable at most.
const MAX_PREDS: usize = 63;

/// A predicate is used when the sets kept hold at least this share of its triples.
const COVERAGE: f64 = 0.95;

/// The characteristic sets of a generation's statistics by predicate.
pub struct CharIndex {
    /// the subjects of each set kept
    subjects: Vec<f64>,
    /// predicate → the sets holding it, by index into the statistics' list, with its
    /// triples in each
    by_pred: FxHashMap<u64, Vec<(u32, f64)>>,
    /// predicate → its triples over the sets kept
    covered: FxHashMap<u64, u64>,
    /// the sets summed by the predicates of a list registered for a variable, kept for
    /// later queries
    groups: Mutex<FxHashMap<Vec<u64>, Arc<[Group]>>>,
}

impl CharIndex {
    /// Lists of predicates whose sums are kept before the cache starts over.
    const KEPT: usize = 1024;

    fn new(stats: &Stats) -> CharIndex {
        let mut by_pred: FxHashMap<u64, Vec<(u32, f64)>> = FxHashMap::default();
        let mut covered: FxHashMap<u64, u64> = FxHashMap::default();
        for (i, c) in stats.charsets.iter().enumerate() {
            for (&p, &t) in c.preds.iter().zip(&c.triples) {
                by_pred.entry(p).or_default().push((i as u32, t as f64));
                *covered.entry(p).or_default() += t;
            }
        }
        CharIndex {
            subjects: stats.charsets.iter().map(|c| c.subjects as f64).collect(),
            by_pred,
            covered,
            groups: Default::default(),
        }
    }

    /// Drop the sums kept for later queries (to time planning without them).
    #[cfg(test)]
    pub(super) fn forget(&self) {
        self.groups.lock().clear();
    }

    /// The sets summed by which of the predicates `preds` they hold, of those whose bit
    /// is in `usable`.
    fn groups(&self, preds: &[u64], usable: u64) -> Arc<[Group]> {
        if let Some(g) = self.groups.lock().get(preds) {
            return g.clone();
        }
        let none: &[(u32, f64)] = &[];
        let posting = |i: usize| -> &[(u32, f64)] {
            if usable & (1 << i) == 0 {
                return none;
            }
            self.by_pred.get(&preds[i]).map_or(none, |v| &v[..])
        };
        let mut mask = vec![0u64; self.subjects.len()];
        let mut touched: Vec<u32> = Vec::new();
        for i in 0..preds.len() {
            for &(c, _) in posting(i) {
                if mask[c as usize] == 0 {
                    touched.push(c);
                }
                mask[c as usize] |= 1 << i;
            }
        }
        touched.sort_unstable();
        let mut list: Vec<Group> = Vec::new();
        let mut of_mask: FxHashMap<u64, u32> = FxHashMap::default();
        let mut group_of = vec![u32::MAX; self.subjects.len()];
        for &c in &touched {
            let m = mask[c as usize];
            let g = *of_mask.entry(m).or_insert_with(|| {
                list.push(Group {
                    mask: m,
                    subjects: 0.0,
                    triples: vec![0.0; preds.len()],
                });
                list.len() as u32 - 1
            });
            group_of[c as usize] = g;
            list[g as usize].subjects += self.subjects[c as usize];
        }
        for i in 0..preds.len() {
            for &(c, t) in posting(i) {
                list[group_of[c as usize] as usize].triples[i] += t;
            }
        }
        list.sort_unstable_by_key(|g| g.mask);
        let list: Arc<[Group]> = list.into();
        let mut kept = self.groups.lock();
        if kept.len() >= Self::KEPT {
            kept.clear();
        }
        kept.insert(preds.to_vec(), list.clone());
        list
    }
}

/// The registered star predicates of one subject variable, and the characteristic sets
/// summed by the registered predicates they hold.
#[derive(Default)]
pub(super) struct StarVar {
    /// bit `i` of a mask stands for `preds[i]`
    preds: Vec<u64>,
    /// the bits of predicates the kept sets hold nearly all triples of
    usable: u64,
    groups: Option<Arc<[Group]>>,
    /// mask → (rows, subjects) of its star
    memo: FxHashMap<u64, (f64, f64)>,
}

/// The sets that hold exactly the registered predicates `mask`, summed.
pub(super) struct Group {
    mask: u64,
    subjects: f64,
    /// triples per registered predicate (by bit)
    triples: Vec<f64>,
}

impl StarVar {
    /// The rows and the distinct subjects of the star of the predicates `mask`.
    fn star(&mut self, mask: u64) -> (f64, f64) {
        if let Some(&x) = self.memo.get(&mask) {
            return x;
        }
        let (mut rows, mut subjects) = (0.0, 0.0);
        for g in self.groups.iter().flat_map(|g| g.iter()) {
            if g.mask & mask != mask {
                continue;
            }
            subjects += g.subjects;
            let mut r = g.subjects;
            let mut bits = mask;
            while bits != 0 {
                let i = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                r *= g.triples[i] / g.subjects;
            }
            rows += r;
        }
        self.memo.insert(mask, (rows, subjects));
        (rows, subjects)
    }
}

/// Register the star patterns `(subject variable, predicate)` of a group about to be
/// ordered, so that joins on those variables can be estimated from the sets.
pub(super) fn register(ctx: &Ctx, stars: &[(VarId, u64)]) {
    let generation = &ctx.snap.generation;
    let stats = &generation.stats;
    if stats.charsets.is_empty() || stars.is_empty() {
        return;
    }
    let idx = generation.charsets.get_or_init(|| CharIndex::new(stats));
    let mut reg = ctx.stars.lock();
    let mut changed: Vec<VarId> = Vec::new();
    for &(v, p) in stars {
        let sv = reg.entry(v).or_default();
        if sv.preds.contains(&p) || sv.preds.len() >= MAX_PREDS {
            continue;
        }
        let bit = sv.preds.len();
        sv.preds.push(p);
        let count = stats.predicate(p).map_or(0, |s| s.count);
        let covered = idx.covered.get(&p).copied().unwrap_or(0);
        if count > 0 && covered as f64 >= COVERAGE * count as f64 {
            sv.usable |= 1 << bit;
        }
        if !changed.contains(&v) {
            changed.push(v);
        }
    }
    for v in changed {
        if let Some(sv) = reg.get_mut(&v) {
            sv.groups = Some(idx.groups(&sv.preds, sv.usable));
            sv.memo.clear();
        }
    }
}

/// Combine the star masks of two sets of patterns on one variable: a predicate in both
/// makes the result unusable.
#[inline]
pub(super) fn merge(a: u64, b: u64) -> u64 {
    if a & b & !BAD != 0 {
        a | b | BAD
    } else {
        a | b
    }
}

/// The variable and predicate of a scan of a pattern `?s p o` with a constant predicate.
fn star_of(spec: &ScanSpec) -> Option<(VarId, u64)> {
    let order = spec.perm.order();
    if (0..spec.prefix.len()).any(|i| order[i] == O) {
        return None;
    }
    let p = spec
        .prefix
        .iter()
        .enumerate()
        .find(|&(i, _)| order[i] == P)
        .map(|(_, &p)| p)?;
    let &(_, v) = spec.cols.iter().find(|&&(kc, _)| order[kc] == S)?;
    Some((v, p))
}

/// The star masks per subject variable of the patterns in plan `n`, merged into `out`.
pub(super) fn of_node(ctx: &Ctx, n: &Node, out: &mut Vec<(VarId, u64)>) {
    let reg = ctx.stars.lock();
    if reg.is_empty() {
        return;
    }
    walk(&reg, n, out);
}

fn walk(reg: &FxHashMap<VarId, StarVar>, n: &Node, out: &mut Vec<(VarId, u64)>) {
    let mut add = |spec: &ScanSpec| {
        if let Some((v, p)) = star_of(spec) {
            let bit = reg
                .get(&v)
                .and_then(|sv| sv.preds.iter().position(|&x| x == p))
                .map_or(BAD, |i| 1 << i);
            match out.iter_mut().find(|(x, _)| *x == v) {
                Some((_, m)) => *m = merge(*m, bit),
                None => out.push((v, bit)),
            }
        }
    };
    match &n.kind {
        Kind::Scan(spec) | Kind::RangeScan(spec, _) => add(spec),
        Kind::SpatialScan(s) => add(&s.scan),
        Kind::IndexJoin(j) => {
            for p in &j.probes {
                add(&p.scan);
            }
            walk(reg, &n.children[0], out);
        }
        Kind::Join { .. } | Kind::Filter(_) | Kind::Sort(_) => {
            for c in &n.children {
                walk(reg, c, out);
            }
        }
        _ => {}
    }
}

/// The star mask of `v` among `masks` (0 when it has none).
pub(super) fn mask_of(masks: &[(VarId, u64)], v: VarId) -> u64 {
    masks.iter().find(|(x, _)| *x == v).map_or(0, |(_, m)| *m)
}

/// For a join on `v` of inputs with the star predicates `qa` and `qb`: the share of the
/// product of their rows that it keeps and the share of the product of their distinct
/// values of `v`. `None` when the sets cannot tell.
pub(super) fn factor(ctx: &Ctx, v: VarId, qa: u64, qb: u64) -> Option<(f64, f64)> {
    if qa == 0 || qb == 0 || (qa | qb) & BAD != 0 || qa & qb != 0 {
        return None;
    }
    let mut reg = ctx.stars.lock();
    let sv = reg.get_mut(&v)?;
    if (qa | qb) & !sv.usable != 0 {
        return None;
    }
    let (ra, da) = sv.star(qa);
    let (rb, db) = sv.star(qb);
    let (r, d) = sv.star(qa | qb);
    if ra <= 0.0 || rb <= 0.0 || da <= 0.0 || db <= 0.0 {
        return None;
    }
    Some((r / (ra * rb), d / (da * db)))
}

/// The distinct values of the join variable after a join whose inputs have `da` and `db`
/// and keep the share `rd` of their product.
#[inline]
pub(super) fn distinct(da: f64, db: f64, rd: f64) -> f64 {
    (da * db * rd).min(da).min(db)
}
