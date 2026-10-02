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
//! subjects and the triples of each predicate over them. The classes a subject has by
//! `rdf:type` count as predicates of their own, since a class usually decides which
//! predicates its instances have. The subjects that have every item of `Q` are those of
//! the sets that hold `Q`, and the rows of the star of `Q` (one pattern `?s p ?o` per
//! predicate, `?s rdf:type <class>` per class) are, per set, its subjects times the
//! average triples per subject of each item. Both count over every graph.
//!
//! A join on a subject variable `?s` whose inputs hold such patterns, `Qa` on one side and
//! `Qb` on the other, then keeps of the product of the inputs' rows the share
//! `rows(Qa ∪ Qb) / (rows(Qa) · rows(Qb))`, and of the product of their distinct subjects
//! the share `subjects(Qa ∪ Qb) / (subjects(Qa) · subjects(Qb))`. For inputs that are
//! exactly the stars this is the estimate from the sets. Whatever else restricts an input
//! (constant objects other than classes, filters, other joins) is taken to be independent
//! of the items its subjects have.
//!
//! The items of a query's stars are registered per subject variable before its groups are
//! ordered, and the sets are summed once per combination of them that some set holds; the
//! sums are kept with the generation for later queries. An item is used only when the
//! sets kept in the statistics hold nearly all of its triples. Otherwise, and when an
//! item occurs twice among the inputs, the join keeps the estimate from distinct
//! values.

use super::ctx::Ctx;
use super::plan::{Kind, Node, ScanSpec};
use super::table::VarId;
use crate::builder::Stats;
use crate::index::{O, P, S};
use crate::store::Generation;
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
    /// the base id of `rdf:type`
    rdf_type: Option<u64>,
    /// the subjects of each set kept
    subjects: Vec<f64>,
    /// item → the sets holding it, by index into the statistics' list, with its triples
    /// in each; found on first use
    postings: Mutex<FxHashMap<u64, Arc<[(u32, f64)]>>>,
    /// the sets summed by the predicates of a list registered for a variable, kept for
    /// later queries
    groups: Mutex<FxHashMap<Vec<u64>, Arc<[Group]>>>,
}

impl CharIndex {
    /// Lists of predicates whose sums are kept before the cache starts over.
    const KEPT: usize = 1024;

    fn new(generation: &Generation) -> CharIndex {
        let stats = &generation.stats;
        let rdf_type = generation
            .vocab
            .find(&crate::id::iri_key(oxrdf::vocab::rdf::TYPE.as_str()))
            .ok()
            .map(|i| crate::id::Id::vocab(i).0);
        CharIndex {
            rdf_type,
            subjects: stats.charsets.iter().map(|c| c.subjects as f64).collect(),
            postings: Default::default(),
            groups: Default::default(),
        }
    }

    /// Find the sets holding each of `items` not found before, in one pass over the sets.
    fn find(&self, stats: &Stats, items: &[u64]) {
        let mut new: Vec<u64> = {
            let kept = self.postings.lock();
            items
                .iter()
                .copied()
                .filter(|p| !kept.contains_key(p))
                .collect()
        };
        new.sort_unstable();
        new.dedup();
        if new.is_empty() {
            return;
        }
        let mut lists: Vec<Vec<(u32, f64)>> = vec![Vec::new(); new.len()];
        for (i, c) in stats.charsets.iter().enumerate() {
            // both sorted: walk them together
            let (mut a, mut b) = (0, 0);
            while a < c.preds.len() && b < new.len() {
                match c.preds[a].cmp(&new[b]) {
                    std::cmp::Ordering::Less => a += 1,
                    std::cmp::Ordering::Greater => b += 1,
                    std::cmp::Ordering::Equal => {
                        lists[b].push((i as u32, c.triples[a] as f64));
                        a += 1;
                        b += 1;
                    }
                }
            }
        }
        let mut kept = self.postings.lock();
        if kept.len() + new.len() > Self::KEPT {
            kept.clear();
        }
        for (p, l) in new.into_iter().zip(lists) {
            kept.insert(p, l.into());
        }
    }

    /// The sets holding item `p`, with its triples in each.
    fn postings(&self, stats: &Stats, p: u64) -> Arc<[(u32, f64)]> {
        if let Some(x) = self.postings.lock().get(&p) {
            return x.clone();
        }
        self.find(stats, &[p]);
        self.postings
            .lock()
            .get(&p)
            .cloned()
            .unwrap_or_else(|| Arc::from(Vec::new()))
    }

    /// Drop the postings and sums kept for later queries (to time planning without them).
    #[cfg(test)]
    pub(super) fn forget(&self) {
        self.groups.lock().clear();
        self.postings.lock().clear();
    }

    /// The sets summed by which of the predicates `preds` they hold, of those whose bit
    /// is in `usable`.
    fn groups(&self, stats: &Stats, preds: &[u64], usable: u64) -> Arc<[Group]> {
        if let Some(g) = self.groups.lock().get(preds) {
            return g.clone();
        }
        let lists: Vec<Arc<[(u32, f64)]>> = (0..preds.len())
            .map(|i| {
                if usable & (1 << i) == 0 {
                    Arc::from(Vec::new())
                } else {
                    self.postings(stats, preds[i])
                }
            })
            .collect();
        let posting = |i: usize| -> &[(u32, f64)] { &lists[i] };
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

/// The item a pattern with predicate `p` and object `o` (when constant) stands for in a
/// characteristic set: the predicate, or the class of `?s rdf:type <class>`.
fn item(p: u64, o: Option<u64>, rdf_type: Option<u64>) -> Option<u64> {
    match o {
        None => Some(p),
        Some(o) if Some(p) == rdf_type => crate::builder::class_item(o),
        Some(_) => None,
    }
}

/// Register the star patterns `(subject variable, predicate, constant object)` of a group
/// about to be ordered, so that joins on those variables can be estimated from the sets.
pub(super) fn register(ctx: &Ctx, stars: &[(VarId, u64, Option<u64>)]) {
    let generation = &ctx.snap.generation;
    let stats = &generation.stats;
    if stats.charsets.is_empty() || stars.is_empty() {
        return;
    }
    let idx = generation
        .charsets
        .get_or_init(|| CharIndex::new(generation));
    let items: Vec<u64> = stars
        .iter()
        .filter_map(|&(_, p, o)| item(p, o, idx.rdf_type))
        .collect();
    idx.find(stats, &items);
    let mut reg = ctx.stars.lock();
    let mut changed: Vec<VarId> = Vec::new();
    for &(v, p, o) in stars {
        let Some(p) = item(p, o, idx.rdf_type) else {
            continue;
        };
        let sv = reg.entry(v).or_default();
        if sv.preds.contains(&p) || sv.preds.len() >= MAX_PREDS {
            continue;
        }
        let bit = sv.preds.len();
        sv.preds.push(p);
        let count = match crate::builder::item_class(p) {
            Some(c) => stats.classes.iter().find(|x| x.0 == c).map_or(0, |x| x.1),
            None => stats.predicate(p).map_or(0, |s| s.count),
        };
        let covered: f64 = idx.postings(stats, p).iter().map(|x| x.1).sum();
        if count > 0 && covered >= COVERAGE * count as f64 {
            sv.usable |= 1 << bit;
        }
        if !changed.contains(&v) {
            changed.push(v);
        }
    }
    for v in changed {
        if let Some(sv) = reg.get_mut(&v) {
            sv.groups = Some(idx.groups(stats, &sv.preds, sv.usable));
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

/// The variable and item (see [`item`]) of a scan of a pattern `?s p ?o` with a constant
/// predicate, or `?s rdf:type <class>`.
fn star_of(spec: &ScanSpec, rdf_type: Option<u64>) -> Option<(VarId, u64)> {
    let order = spec.perm.order();
    let at = |c: usize| {
        spec.prefix
            .iter()
            .enumerate()
            .find(|&(i, _)| order[i] == c)
            .map(|(_, &x)| x)
    };
    let p = at(P)?;
    let &(_, v) = spec.cols.iter().find(|&&(kc, _)| order[kc] == S)?;
    Some((v, item(p, at(O), rdf_type)?))
}

/// Whether the scan `spec` is a star pattern on `v` the sets could count.
pub(super) fn is_star(ctx: &Ctx, spec: &ScanSpec, v: VarId) -> bool {
    let rdf_type = ctx.snap.generation.charsets.get().and_then(|i| i.rdf_type);
    star_of(spec, rdf_type).is_some_and(|(x, _)| x == v)
}

/// The star masks per subject variable of the patterns in plan `n`, merged into `out`.
pub(super) fn of_node(ctx: &Ctx, n: &Node, out: &mut Vec<(VarId, u64)>) {
    let reg = ctx.stars.lock();
    if reg.is_empty() {
        return;
    }
    let rdf_type = ctx.snap.generation.charsets.get().and_then(|i| i.rdf_type);
    walk(&reg, n, rdf_type, out);
}

fn walk(
    reg: &FxHashMap<VarId, StarVar>,
    n: &Node,
    rdf_type: Option<u64>,
    out: &mut Vec<(VarId, u64)>,
) {
    let mut add = |spec: &ScanSpec| {
        if let Some((v, p)) = star_of(spec, rdf_type) {
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
            walk(reg, &n.children[0], rdf_type, out);
        }
        Kind::Join { .. } | Kind::Filter(_) | Kind::Sort(_) => {
            for c in &n.children {
                walk(reg, c, rdf_type, out);
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
