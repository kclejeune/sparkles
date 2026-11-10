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
//! sums are kept with the generation for later queries. When an item occurs twice among
//! the inputs, the join keeps the estimate from distinct values.
//!
//! The statistics keep the sets of the most subjects, and count the subjects of the others
//! as a remainder, with per item the subjects having it and its triples over them (see
//! [`crate::builder::correlations`]). The rows of a star over the remainder are estimated
//! with its items taken to be independent within the remainder, `N Π t_i/N`, and added to
//! the sums over the kept sets. Statistics written before the remainder existed use an
//! item only when the kept sets hold nearly all of its triples, as before.
//!
//! **Node sets.** A pattern `?x p ?v` with a constant predicate is an incoming item of
//! `?v`. The node sets of the statistics count, for every node, its outgoing predicates,
//! its incoming predicates and its classes, so joins on a variable that is the object of
//! some pattern of its group, such as `?a p ?v . ?b q ?v` or `?a p ?v . ?v q ?b`, are
//! estimated from them in the same way. A variable is estimated from one family of sets
//! for the whole group: the node sets once it has an incoming item, so that the shares of
//! the joins along a plan multiply out, and the subject sets otherwise, which are less
//! fragmented. A pattern `?v p ?v` counts for neither.

use super::ctx::Ctx;
use super::plan::{Kind, Node, ScanSpec};
use super::table::VarId;
use crate::builder::CharSet;
use crate::builder::correlations::{Rest, in_item, is_in_item};
use crate::index::{O, P, S};
use crate::store::Generation;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// The bit of a variable's star mask that marks predicates the sets cannot answer for:
/// unregistered, too rarely kept, or present twice.
pub(super) const BAD: u64 = 1 << 63;

/// The bit of a node-family variable's star mask that marks a restriction the node sets
/// do not count, such as a constant object: the subject sets then estimate the join from
/// its outgoing items alone, as if the variable had no incoming ones.
pub(super) const RESTRICTED: u64 = 1 << 62;

/// Predicates registered per subject variable at most.
const MAX_PREDS: usize = 62;

/// A predicate is used when the sets kept hold at least this share of its triples.
const COVERAGE: f64 = 0.95;

/// The sets holding an item, by index into the statistics' list, with the item's triples
/// in each.
type Postings = Arc<[(u32, f64)]>;

/// The sets of one family (subject sets or node sets) of a generation's statistics.
pub struct Family {
    /// the holders (subjects or nodes) of each set kept
    holders: Vec<f64>,
    /// item → the sets holding it, by index into the family's list, with its triples in
    /// each; found on first use
    postings: Mutex<FxHashMap<u64, Postings>>,
    /// the sets summed by the items of a list registered for a variable, kept for later
    /// queries
    groups: Mutex<FxHashMap<Vec<u64>, Arc<[Group]>>>,
}

/// The characteristic sets of a generation's statistics by item.
pub struct CharIndex {
    /// the base id of `rdf:type`
    rdf_type: Option<u64>,
    subject: Family,
    /// the node sets, when the statistics have them
    node: Option<Family>,
}

/// Which sets estimate the joins on a variable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Kin {
    #[default]
    Subject,
    Node,
}

impl Family {
    /// Lists of items whose sums are kept before the cache starts over.
    const KEPT: usize = 1024;

    fn new(sets: &[CharSet]) -> Family {
        Family {
            holders: sets.iter().map(|c| c.subjects as f64).collect(),
            postings: Default::default(),
            groups: Default::default(),
        }
    }

    /// Find the sets holding each of `items` not found before, in one pass over the sets.
    fn find(&self, sets: &[CharSet], items: &[u64]) {
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
        for (i, c) in sets.iter().enumerate() {
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
    fn postings(&self, sets: &[CharSet], p: u64) -> Postings {
        if let Some(x) = self.postings.lock().get(&p) {
            return x.clone();
        }
        self.find(sets, &[p]);
        self.postings
            .lock()
            .get(&p)
            .cloned()
            .unwrap_or_else(|| Arc::from(Vec::new()))
    }

    /// Drop the postings and sums kept for later queries.
    #[cfg(test)]
    fn forget(&self) {
        self.groups.lock().clear();
        self.postings.lock().clear();
    }

    /// The sets summed by which of the items `preds` they hold, of those whose bit is in
    /// `usable`.
    fn groups(&self, sets: &[CharSet], preds: &[u64], usable: u64) -> Arc<[Group]> {
        if let Some(g) = self.groups.lock().get(preds) {
            return g.clone();
        }
        let lists: Vec<Postings> = (0..preds.len())
            .map(|i| {
                if usable & (1 << i) == 0 {
                    Arc::from(Vec::new())
                } else {
                    self.postings(sets, preds[i])
                }
            })
            .collect();
        let posting = |i: usize| -> &[(u32, f64)] { &lists[i] };
        let mut mask = vec![0u64; self.holders.len()];
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
        let mut group_of = vec![u32::MAX; self.holders.len()];
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
            list[g as usize].subjects += self.holders[c as usize];
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

impl CharIndex {
    fn new(generation: &Generation) -> CharIndex {
        let stats = &generation.stats;
        let rdf_type = generation
            .vocab
            .find(&crate::id::iri_key(oxrdf::vocab::rdf::TYPE.as_str()))
            .ok()
            .map(|i| crate::id::Id::vocab(i).0);
        CharIndex {
            rdf_type,
            subject: Family::new(&stats.charsets),
            node: stats.correlations.get().map(|c| Family::new(&c.node_sets)),
        }
    }

    fn family(&self, kin: Kin) -> &Family {
        match (kin, &self.node) {
            (Kin::Node, Some(f)) => f,
            _ => &self.subject,
        }
    }

    /// Drop the postings and sums kept for later queries (to time planning without them).
    #[cfg(test)]
    pub(super) fn forget(&self) {
        self.subject.forget();
        if let Some(n) = &self.node {
            n.forget();
        }
    }
}

/// The sets of family `kin` in `stats`, and their remainder when the statistics count one.
fn sets_of(stats: &crate::builder::Stats, kin: Kin) -> (&[CharSet], Option<&Rest>) {
    let c = stats.correlations.get();
    match kin {
        Kin::Subject => (&stats.charsets, c.map(|c| &c.subject_rest)),
        Kin::Node => match c {
            Some(c) => (&c.node_sets, Some(&c.node_rest)),
            None => (&[], None),
        },
    }
}

/// The registered star items of one variable, and the characteristic sets summed by the
/// registered items they hold.
#[derive(Default)]
pub(super) struct StarVar {
    /// bit `i` of a mask stands for `preds[i]`
    preds: Vec<u64>,
    /// the family of sets that estimates joins on the variable
    kin: Kin,
    /// the bits of items the statistics account for: every item when they count a
    /// remainder, else those whose triples the kept sets hold nearly all of
    usable: u64,
    groups: Option<Arc<[Group]>>,
    /// the remainder's holders, and per bit its holders having the item and the item's
    /// triples over them; `None` when the statistics count no remainder or it is not used
    rest: Option<(f64, Vec<(f64, f64)>)>,
    /// mask → (rows, holders) of its star
    memo: FxHashMap<u64, (f64, f64)>,
    /// for a node-family variable, its outgoing items over the subject sets and the bit
    /// there of each of its bits (`u8::MAX` for an incoming item); made when a restricted
    /// join first needs it
    outgoing: Option<(Box<StarVar>, Vec<u8>)>,
}

/// The sets that hold exactly the registered predicates `mask`, summed.
pub(super) struct Group {
    mask: u64,
    subjects: f64,
    /// triples per registered predicate (by bit)
    triples: Vec<f64>,
}

impl StarVar {
    /// The rows and the distinct holders of the star of the items `mask`.
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
        let (rr, rs) = self.rest_star(mask);
        rows += rr;
        subjects += rs;
        self.memo.insert(mask, (rows, subjects));
        (rows, subjects)
    }

    /// The rows and holders of the star of `mask` over the remainder, its items taken to
    /// be independent there.
    fn rest_star(&self, mask: u64) -> (f64, f64) {
        let Some((n, items)) = &self.rest else {
            return (0.0, 0.0);
        };
        if *n <= 0.0 {
            return (0.0, 0.0);
        }
        let (mut r, mut d) = (*n, *n);
        let mut bits = mask;
        while bits != 0 {
            let i = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let (h, t) = items[i];
            r *= t / n;
            d *= h / n;
        }
        (r, d)
    }
}

/// The item a pattern with predicate `p` and object `o` (when constant) stands for in a
/// characteristic set of its subject: the predicate, or the class of
/// `?s rdf:type <class>`.
fn item(p: u64, o: Option<u64>, rdf_type: Option<u64>) -> Option<u64> {
    match o {
        None => Some(p),
        Some(o) if Some(p) == rdf_type => crate::builder::class_item(o),
        Some(_) => None,
    }
}

/// A triple pattern's terms for registering its star items: a variable or a constant id.
#[derive(Clone, Copy)]
pub(super) enum Term {
    V(VarId),
    C(u64),
}

/// The star items of the triple pattern `(s, p, o)`, with the variable each belongs to:
/// the subject's item of a pattern with a constant predicate (see [`item`]), and the
/// incoming item of its object variable when its subject is another variable and
/// `incoming` is on.
pub(super) fn items_of(
    s: Term,
    p: Term,
    o: Term,
    rdf_type: Option<u64>,
    incoming: bool,
    out: &mut Vec<(VarId, u64)>,
) {
    let Term::C(p) = p else { return };
    match (s, o) {
        (Term::V(s), Term::V(o)) if s != o => {
            out.push((s, p));
            // the classes of `?x a ?c` are few nodes whose incoming triples depend
            // almost wholly on what `?x` is, which the node sets do not tell
            if incoming
                && Some(p) != rdf_type
                && let Some(i) = in_item(p)
            {
                out.push((o, i));
            }
        }
        (Term::V(s), Term::C(o)) => {
            if let Some(i) = item(p, Some(o), rdf_type) {
                out.push((s, i));
            }
        }
        _ => {}
    }
}

/// The `rdf:type` id the sets count classes by, once the index is made.
pub(super) fn rdf_type(ctx: &Ctx) -> Option<u64> {
    let generation = &ctx.snap.generation;
    generation
        .charsets
        .get_or_init(|| CharIndex::new(generation))
        .rdf_type
}

/// Whether incoming items are registered: node sets are on and the statistics have them.
pub(super) fn incoming(ctx: &Ctx) -> bool {
    ctx.opt.node_sets && ctx.snap.generation.stats.correlations.get().is_some()
}

/// Register the star items `(variable, item)` of a group about to be ordered (see
/// [`items_of`]), so that joins on those variables can be estimated from the sets.
pub(super) fn register(ctx: &Ctx, stars: &[(VarId, u64)]) {
    let generation = &ctx.snap.generation;
    let stats = &generation.stats;
    if (stats.charsets.is_empty() && stats.correlations.get().is_none()) || stars.is_empty() {
        return;
    }
    let idx = generation
        .charsets
        .get_or_init(|| CharIndex::new(generation));
    let remainder = ctx.opt.set_remainders && stats.correlations.get().is_some();
    let mut reg = ctx.stars.lock();
    let mut changed: Vec<VarId> = Vec::new();
    for &(v, p) in stars {
        let sv = reg.entry(v).or_default();
        if sv.preds.contains(&p) || sv.preds.len() >= MAX_PREDS {
            continue;
        }
        sv.preds.push(p);
        if is_in_item(p) && idx.node.is_some() {
            sv.kin = Kin::Node;
        }
        if !changed.contains(&v) {
            changed.push(v);
        }
    }
    for v in changed {
        let Some(sv) = reg.get_mut(&v) else { continue };
        fill(sv, stats, idx, remainder);
    }
}

/// Find the sets of the items registered for `sv` and sum them by the items they hold.
fn fill(sv: &mut StarVar, stats: &crate::builder::Stats, idx: &CharIndex, remainder: bool) {
    let kin = sv.kin;
    let (sets, rest) = sets_of(stats, kin);
    let fam = idx.family(kin);
    fam.find(sets, &sv.preds);
    // with a remainder every triple is accounted for, and the node sets always have
    // one; without, an item is used when the kept sets hold nearly all of it
    let rest = rest.filter(|_| remainder || kin == Kin::Node);
    let mut usable = 0u64;
    for (bit, &p) in sv.preds.iter().enumerate() {
        let ok = if rest.is_some() {
            // an incoming item of the subject family has no sets
            kin == Kin::Node || !is_in_item(p)
        } else {
            let count = match crate::builder::item_class(p) {
                Some(c) => stats.classes.iter().find(|x| x.0 == c).map_or(0, |x| x.1),
                None => stats.predicate(p).map_or(0, |s| s.count),
            };
            let covered: f64 = fam.postings(sets, p).iter().map(|x| x.1).sum();
            !is_in_item(p) && count > 0 && covered >= COVERAGE * count as f64
        };
        if ok {
            usable |= 1 << bit;
        }
    }
    sv.usable = usable;
    sv.rest = rest.map(|r| {
        (
            r.n as f64,
            sv.preds
                .iter()
                .map(|&p| r.item(p).map_or((0.0, 0.0), |(h, t)| (h as f64, t as f64)))
                .collect(),
        )
    });
    sv.groups = Some(fam.groups(sets, &sv.preds, sv.usable));
    sv.memo.clear();
    sv.outgoing = None;
}

/// Combine the star masks of two sets of patterns on one variable: a predicate in both
/// makes the result unusable.
#[inline]
pub(super) fn merge(a: u64, b: u64) -> u64 {
    if a & b & !(BAD | RESTRICTED) != 0 {
        a | b | BAD
    } else {
        a | b
    }
}

/// The star items of a scan (see [`items_of`]), with the variable of each: at most two.
fn star_of(spec: &ScanSpec, rdf_type: Option<u64>, incoming: bool) -> ([(VarId, u64); 2], usize) {
    let mut out = [(VarId::default(), 0); 2];
    let order = spec.perm.order();
    let at = |c: usize| {
        spec.prefix
            .iter()
            .enumerate()
            .find(|&(i, _)| order[i] == c)
            .map(|(_, &x)| x)
    };
    let var = |c: usize| {
        spec.cols
            .iter()
            .find(|&&(kc, _)| order[kc] == c)
            .map(|&(_, v)| v)
    };
    let term = |c: usize| match (at(c), var(c)) {
        (Some(x), _) => Some(Term::C(x)),
        (None, Some(v)) => Some(Term::V(v)),
        _ => None,
    };
    // a pattern whose columns are tied by an equality counts for no set
    let (Some(s), Some(p), Some(o)) = (term(S), term(P), term(O)) else {
        return (out, 0);
    };
    if !spec.eqs.is_empty() {
        return (out, 0);
    }
    let mut v: Vec<(VarId, u64)> = Vec::with_capacity(2);
    items_of(s, p, o, rdf_type, incoming, &mut v);
    for (i, x) in v.iter().enumerate() {
        out[i] = *x;
    }
    (out, v.len())
}

/// Whether the scan `spec` is a subject star pattern on `v` the sets could count.
pub(super) fn is_star(ctx: &Ctx, spec: &ScanSpec, v: VarId) -> bool {
    let rdf_type = ctx.snap.generation.charsets.get().and_then(|i| i.rdf_type);
    let (items, n) = star_of(spec, rdf_type, false);
    items[..n].iter().any(|&(x, i)| x == v && !is_in_item(i))
}

/// The star masks per subject variable of the patterns in plan `n`, merged into `out`.
pub(super) fn of_node(ctx: &Ctx, n: &Node, out: &mut Vec<(VarId, u64)>) {
    let reg = ctx.stars.lock();
    if reg.is_empty() {
        return;
    }
    let rdf_type = ctx.snap.generation.charsets.get().and_then(|i| i.rdf_type);
    // the place of each variable in `out`, since a plan can hold thousands of patterns
    let mut at: FxHashMap<VarId, usize> =
        out.iter().enumerate().map(|(i, &(v, _))| (v, i)).collect();
    walk(&reg, n, rdf_type, None, &mut |v, bit| match at.get(&v) {
        Some(&i) => out[i].1 = merge(out[i].1, bit),
        None => {
            at.insert(v, out.len());
            out.push((v, bit));
        }
    });
}

/// The star mask of `v` among the patterns of plan `n` (0 when it has none): its entry
/// in [`of_node`], found without collecting the others.
pub(super) fn mask_in(ctx: &Ctx, n: &Node, v: VarId) -> u64 {
    let reg = ctx.stars.lock();
    if reg.is_empty() {
        return 0;
    }
    let rdf_type = ctx.snap.generation.charsets.get().and_then(|i| i.rdf_type);
    let mut m = 0;
    walk(&reg, n, rdf_type, Some(v), &mut |_, bit| m = merge(m, bit));
    m
}

/// Call `add` with the variable and bit of each star pattern of plan `n`, or of those on
/// `only`.
fn walk(
    reg: &FxHashMap<VarId, StarVar>,
    n: &Node,
    rdf_type: Option<u64>,
    only: Option<VarId>,
    add: &mut dyn FnMut(VarId, u64),
) {
    let mut scan = |spec: &ScanSpec| {
        let (items, n) = star_of(spec, rdf_type, true);
        for &(v, p) in &items[..n] {
            if only.is_some_and(|o| o != v) {
                continue;
            }
            // an incoming item of a variable registered without one is not counted
            let Some(sv) = reg.get(&v) else {
                if !is_in_item(p) {
                    add(v, BAD);
                }
                continue;
            };
            if is_in_item(p) && sv.kin != Kin::Node && !sv.preds.contains(&p) {
                continue;
            }
            let bit = sv
                .preds
                .iter()
                .position(|&x| x == p)
                .map_or(BAD, |i| 1 << i);
            add(v, bit);
        }
        // a node-family variable the scan binds without an item for it, such as the
        // subject of `?v p <o>`, is restricted in a way the node sets do not count
        for &(_, v) in &spec.cols {
            if only.is_some_and(|o| o != v) || items[..n].iter().any(|&(x, _)| x == v) {
                continue;
            }
            if reg.get(&v).is_some_and(|sv| sv.kin == Kin::Node) {
                add(v, RESTRICTED);
            }
        }
    };
    match &n.kind {
        Kind::Scan(spec) | Kind::RangeScan(spec, _) => scan(spec),
        Kind::SpatialScan(s) => scan(&s.scan),
        Kind::IndexJoin(j) => {
            for p in &j.probes {
                scan(&p.scan);
            }
            walk(reg, &n.children[0], rdf_type, only, add);
        }
        Kind::Join { .. } | Kind::Filter(_) | Kind::Sort(_) => {
            for c in &n.children {
                walk(reg, c, rdf_type, only, add);
            }
        }
        // any other input, such as a subquery or a VALUES table, restricts the
        // node-family variables it binds in a way the node sets do not count
        _ => {
            for &v in &n.vars {
                if only.is_some_and(|o| o != v) {
                    continue;
                }
                if reg.get(&v).is_some_and(|sv| sv.kin == Kin::Node) {
                    add(v, RESTRICTED);
                }
            }
        }
    }
}

/// For a join on `v` of inputs with the star predicates `qa` and `qb`: the share of the
/// product of their rows that it keeps and the share of the product of their distinct
/// values of `v`. `None` when the sets cannot tell.
pub(super) fn factor(ctx: &Ctx, v: VarId, qa: u64, qb: u64) -> Option<(f64, f64)> {
    let (pa, pb) = (qa & !RESTRICTED, qb & !RESTRICTED);
    if pa == 0 || pb == 0 || (qa | qb) & BAD != 0 || pa & pb != 0 {
        return None;
    }
    let mut reg = ctx.stars.lock();
    let sv = reg.get_mut(&v)?;
    if (qa | qb) & RESTRICTED == 0 {
        return star_factor(sv, pa, pb);
    }
    // a restricted node-family variable: its outgoing items over the subject sets
    if sv.kin != Kin::Node {
        return None;
    }
    if sv.outgoing.is_none() {
        let generation = &ctx.snap.generation;
        let idx = generation.charsets.get()?;
        let mut out = StarVar::default();
        let mut at = Vec::with_capacity(sv.preds.len());
        for &p in &sv.preds {
            if is_in_item(p) {
                at.push(u8::MAX);
            } else {
                at.push(out.preds.len() as u8);
                out.preds.push(p);
            }
        }
        let remainder = ctx.opt.set_remainders && generation.stats.correlations.get().is_some();
        fill(&mut out, &generation.stats, idx, remainder);
        sv.outgoing = Some((Box::new(out), at));
    }
    let (out, at) = sv.outgoing.as_mut()?;
    let map = |mut m: u64| -> Option<u64> {
        let mut r = 0;
        while m != 0 {
            let i = m.trailing_zeros() as usize;
            m &= m - 1;
            match at[i] {
                u8::MAX => return None,
                j => r |= 1 << j,
            }
        }
        Some(r)
    };
    let (oa, ob) = (map(pa)?, map(pb)?);
    star_factor(out, oa, ob)
}

/// [`factor`] from the sets of `sv` for the items `qa` and `qb`.
fn star_factor(sv: &mut StarVar, qa: u64, qb: u64) -> Option<(f64, f64)> {
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
