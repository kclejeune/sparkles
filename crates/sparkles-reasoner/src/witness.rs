//! Witnesses of the facts of a closure (spec F18 §7.1), kept with the provenance algebra
//! of F13 §5.1 ([`sparkles_core::incr::prov`]).
//!
//! **What is kept.** For each fact of a closure, at most `max` supports, each a set of
//! input quads ([`FactKey`]s) from which the rules derive the fact, and a flag that says
//! whether supports may be missing. A support of a fact is the set of asserted leaves of
//! one well-founded derivation tree. The kept supports form an antichain, so no kept
//! support contains another, and they are the smallest ones a fact's evaluation saw.
//!
//! **Why sets and not counts.** F13's `why` semiring counts the derivations per support.
//! A fact in a recursive program can have infinitely many derivation trees, since a fact
//! of a cycle supports itself (F13 §4.3), so the reasoner counts each support once. In
//! that semiring addition and multiplication are those of F13, the union of supports and
//! the pairwise [`Witness::union`] of a product, and a sum also absorbs a support that
//! contains another one. The least fixed point of the rules over it is, for a positive
//! program, the set of minimal supports of each fact: the minimal sets of input quads
//! whose closure holds the fact. The reasoner reports a fact's witnesses as an
//! [`Ann::Why`] whose kept supports each count 1 and whose unknown count is 1 when the
//! fact is truncated, and [`Ann::report`] gives the minimal kept supports, smallest
//! first, as it does for a view.
//!
//! **The two rules of F13, for sets.** With the true count of a support being 1 when it
//! is a support of the fact and 0 otherwise, the rules of F13 §5.1 read as follows, and
//! every operation below keeps them.
//!
//! 1. A kept support is a support of the fact: the fact follows from its quads. It
//!    therefore never counts more than its true count.
//! 2. When a fact is not truncated (its unknown count is 0), its kept supports are
//!    exactly its minimal supports.
//!
//! **Truncation.** A fact is truncated locally when its own evaluation may have lost a
//! support. That happens when more minimal supports reach it than `max`, when a product
//! or a sum passes its working bound, when the fact has more rule instances than
//! [`MAX_INSTANCES`], when an instance holds through `noValue`, when the fact is a
//! GeoSPARQL default geometry, when the program derives facts that rule instances do not
//! show, and when a fact keeps changing in one pass. A fact is truncated when it is
//! truncated locally or a premise of one of its instances is truncated, the least such
//! assignment. Rule 2 then holds because a fact that is not truncated has a cone of
//! premises that is not truncated, whose supports satisfy the equations of the rules
//! exactly, and the least solution of those equations is the minimal supports.
//!
//! **Convergence.** A full build evaluates every fact until no value changes. An update
//! keeps the supports that avoid the removed quads, which for a positive program are
//! exactly the minimal supports that remain, and evaluates again the facts that lost a
//! kept support, the facts that joined the closure or the base tier, and every truncated
//! fact that a removed fact reaches through truncated facts or facts that lost a support.
//! Those are the facts whose dropped supports may have used a removed fact. Their
//! truncation is reset first and found again, so a fact whose minimal supports fit the
//! bound once the removed facts are gone is exact again after the update, as F13's rows
//! are once their unknown derivations are removed.
//!
//! **Closure operators.** The operators of [`crate::closure`] compute transitive and
//! equivalence relations without rule instances. Their rules stay in the program, so the
//! witnesses of the facts they derive come from the rules' instances, found as joins over
//! the closure the operators built, like any other fact's. The witnesses therefore do not
//! depend on how the closure was computed. A long chain of a transitive property gives a
//! fact one instance per intermediate node and many minimal supports, one per path, so
//! such facts are often truncated.

use crate::budget::{Budget, BudgetFailure, Held, Meter, table_bytes, table_grown};
use crate::builtins::BuiltinKind;
use crate::engine::{self, CHead, CRule, Eval, Limits, Mode, Slot};
use crate::facts::Memo;
use crate::graph::Triple;
use crate::incremental::{Closure, Plans, bind_head};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_core::id::Id;
use sparkles_core::incr::prov::{Ann, Annotation, Atom, FactKey, Why, Witness};
use std::collections::VecDeque;
use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub use sparkles_core::incr::prov::{DEFAULT_MAX_WITNESSES, MAX_WITNESSES_LIMIT};

/// Rule instances read per fact before the fact is truncated.
pub(crate) const MAX_INSTANCES: usize = 4096;
/// Evaluations that change one fact in one pass before the fact is frozen and truncated.
/// Without truncation the values only grow, so a fact changes at most once per support
/// that reaches it; the bound stops the rare pass whose truncated values keep moving.
const MAX_CHANGES: u32 = 256;
/// List cells read per list before an instance counts as unknown.
const MAX_LIST: usize = 1 << 16;

/// The working bound of the products and sums of one fact's evaluation.
/// The most unions one premise of a rule instance adds to the product of the others.
const MAX_UNIONS: usize = 1 << 14;

fn pool_cap(max: usize) -> usize {
    (max * 4).max(64)
}

/// What a run reports of its witnesses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WitnessReport {
    /// the bound of witnesses per fact
    pub max_witnesses: usize,
    /// derived facts of the closure
    pub facts: u64,
    /// derived facts whose witnesses may be incomplete
    pub truncated: u64,
    /// witnesses kept for the derived facts
    pub witnesses: u64,
    /// facts evaluated by the run
    pub evaluated: u64,
    /// whether the run built the witnesses from the closure rather than updating them
    pub built: bool,
    /// the estimated heap of the witnesses and their index
    pub bytes: u64,
    pub millis: u64,
}

/// How a fact of the closure is held.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// in the input graphs: its own quad is a support
    Base,
    /// derived
    Derived,
    /// in the derived tier and flagged explicit: a GeoSPARQL default geometry, whose
    /// supports are not known
    Flagged,
}

/// Whether a node of the closure has more than one `rdf:first` or more than one
/// `rdf:rest`, which makes a list through it ambiguous.
/// The set of list cells is charged to the run as it grows, and the scan stops when the
/// run is cancelled.
fn tangled_lists(c: &Closure, first: u64, rest: u64, limits: &Limits) -> anyhow::Result<bool> {
    let mut held = Held::new(&limits.budget);
    let mut seen: FxHashSet<(u64, u64)> = FxHashSet::default();
    let mut tangled = false;
    let mut stop: Option<anyhow::Error> = None;
    let cancel = limits.cancel.as_deref();
    let mut see = |t: Triple| {
        if let Err(e) = grow_set(&mut seen, &mut held) {
            stop = Some(e.into());
            return false;
        }
        if seen.len().is_multiple_of(1 << 16) && cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            stop = Some(anyhow::anyhow!("reasoning cancelled"));
            return false;
        }
        if !seen.insert((t[0], t[1])) {
            tangled = true;
        }
        !tangled
    };
    if let Some(base) = c.graph.base() {
        for p in [first, rest] {
            base.for_each(None, Some(p), None, &mut see)?;
        }
    }
    for (t, _) in c.live() {
        if (t[1] == first || t[1] == rest) && !see(t) {
            break;
        }
    }
    if let Some(e) = stop {
        return Err(e);
    }
    Ok(tangled)
}

/// Make room in `s` for one more entry, charging a table's growth to `held` first.
fn grow_set<K: Eq + std::hash::Hash>(
    s: &mut FxHashSet<K>,
    held: &mut Held,
) -> Result<(), BudgetFailure> {
    let cap = s.capacity();
    if s.len() < cap {
        return Ok(());
    }
    let next = table_grown(cap, s.len(), 1);
    let elem = size_of::<K>();
    held.try_grow(table_bytes(next, elem))?;
    s.reserve(next - s.len());
    held.shrink(table_bytes(cap, elem));
    Ok(())
}

/// Whether the base fact `t` may have supports that no instance shows: under an opaque
/// program, or a GeoSPARQL default geometry, which the closure can also derive from the
/// feature's geometries.
fn hidden(c: &Closure, opaque: bool, t: &Triple) -> bool {
    opaque || c.defaults.as_ref().is_some_and(|d| t[1] == d.pred)
}

fn kind(c: &Closure, t: &Triple) -> Kind {
    match c.graph.position(t) {
        Some(r) if c.flagged(r) => Kind::Flagged,
        Some(_) => Kind::Derived,
        None => Kind::Base,
    }
}

#[derive(Clone, Debug)]
struct Node {
    t: Triple,
    /// the key of the fact's input quad, for a fact of the base tier
    key: Option<FactKey>,
    /// the kept supports: an antichain, in the order of [`Witness`]
    w: Vec<Witness>,
    /// supports may be missing at this fact itself
    local: bool,
    /// supports may be missing: at this fact or at a premise
    trunc: bool,
    live: bool,
}

impl Node {
    fn bytes(&self) -> usize {
        size_of::<Node>()
            + (self.w.capacity() - self.w.len()) * size_of::<Witness>()
            + self
                .w
                .iter()
                .map(|x| x.len() * size_of::<Atom>() + size_of::<Witness>())
                .sum::<usize>()
    }
}

/// The witnesses of a closure's facts, with an index from input quads to the facts whose
/// kept witnesses name them.
pub struct Witnesses {
    max: usize,
    nodes: Vec<Node>,
    index: FxHashMap<Triple, u32>,
    free: Vec<u32>,
    /// per input quad: the nodes whose kept witnesses may contain it (a superset)
    rev: FxHashMap<FactKey, Vec<u32>>,
    rev_entries: usize,
    /// the capacity of the lists of `rev`
    rev_cap: usize,
    /// the heap of `nodes`, kept as they change
    node_bytes: usize,
    /// the rules derive facts that their instances do not show, such as those of head
    /// actions: every fact, asserted ones included, may have supports that are not kept
    opaque: bool,
    /// the heap of the witnesses, charged to the run's budget
    held: Held,
    /// the last run's report
    pub(crate) report: WitnessReport,
}

/// What a pass reads.
struct Cx<'a> {
    c: &'a Closure,
    rules: &'a [CRule],
    plans: &'a Plans,
    abort: &'a AtomicBool,
    cancel: Option<&'a AtomicBool>,
    budget: &'a Arc<Budget>,
    /// per rule: its instances hold through a negation
    negated: Vec<bool>,
    /// the program derives facts whose instances a backward search cannot show: head
    /// actions, `makeTemp` and heads without a plan
    opaque: bool,
    /// the program reads RDF lists
    lists: bool,
    /// per rule: it reads lists in a program with a negation, which can derive a
    /// competing list cell from a subset of the facts and so break a list that a
    /// support reads
    unsure_lists: Vec<bool>,
    first: u64,
    rest: u64,
    nil: u64,
}

/// The rule instances of one fact.
#[derive(Default)]
struct Found {
    bodies: Vec<Box<[Triple]>>,
    /// an instance holds through a negation, or reads a list that is not plain
    unknown: bool,
    /// more than [`MAX_INSTANCES`] instances
    capped: bool,
}

/// The instances of one fact, as nodes.
#[derive(Default)]
struct Ins {
    bodies: Vec<Box<[u32]>>,
    unknown: bool,
    capped: bool,
}

/// The state of what changes in an update, found before its deletion (see
/// [`Witnesses::prepare`]).
#[derive(Default)]
pub(crate) struct Prepared {
    /// the keys of the input quads that are no longer input quads
    dead: FxHashSet<FactKey>,
    /// the truncated facts and the facts with a dead key that a removed fact reaches
    region: Vec<u32>,
    /// base facts whose quad changed
    rekeyed: Vec<u32>,
    /// whether the update has to build the witnesses again
    rebuild: bool,
    /// the charge of this state and of the search that found it, until the update ends
    held: Option<Held>,
}

/// The heap of one fact in the worklists of a pass, its queue entry, its entry in the set
/// of queued facts and its count of changes.
const WORK_BYTES: usize = 32;

/// The heap of one fact that the search before a deletion visits, in its set of visited
/// facts, and of one it adds to its region, in the set of seen nodes, the region and the
/// queue, charged as they are reached.
const VISITED_BYTES: usize = size_of::<Triple>() + 8;
const REACHED_BYTES: usize = 2 * size_of::<u32>() + 4 + size_of::<Triple>();

fn cancelled(cx: &Cx<'_>) -> bool {
    cx.abort.load(Ordering::Relaxed) || cx.cancel.is_some_and(|c| c.load(Ordering::Relaxed))
}

/// The key of the input quad that holds the base fact `t`: with several input graphs,
/// the smallest key among the graphs that hold it (F13 §5.1).
pub(crate) fn fact_key(c: &Closure, t: &Triple) -> FactKey {
    let terms = &c.terms;
    let key = |x: u64| terms.key_of(x).unwrap_or_default();
    let (s, p, o) = (key(t[0]), key(t[1]), key(t[2]));
    let Some(base) = c.graph.base() else {
        return FactKey::of_keys(&s, &p, &o, None);
    };
    let graphs = base.graph_ids();
    let of = |g: Id| {
        let gk = (g != Id::DEFAULT_GRAPH).then(|| key(g.0));
        FactKey::of_keys(&s, &p, &o, gk.as_deref())
    };
    if graphs.len() == 1 {
        return of(graphs[0]);
    }
    let snap = &base.snap;
    let stored = t
        .iter()
        .all(|&x| Id(x).tag() != sparkles_core::id::Tag::Local);
    graphs
        .iter()
        .filter(|&&g| {
            stored
                && snap
                    .contains(&[Id(t[0]), Id(t[1]), Id(t[2]), g])
                    .unwrap_or(false)
        })
        .map(|&g| of(g))
        .min()
        .unwrap_or_else(|| of(graphs[0]))
}

/// Add `x` to the antichain `a`, kept in the order of [`Witness`]: nothing when a member
/// is a subset of `x`, and otherwise the members that contain `x` leave.
fn insert_min(a: &mut Vec<Witness>, x: Witness) -> isize {
    if a.iter().any(|y| y.len() <= x.len() && y.is_subset(&x)) {
        return 0;
    }
    let mut delta = witness_bytes(&x) as isize;
    a.retain(|y| {
        let keep = !(x.len() < y.len() && x.is_subset(y));
        if !keep {
            delta -= witness_bytes(y) as isize;
        }
        keep
    });
    let at = a.binary_search(&x).unwrap_or_else(|i| i);
    a.insert(at, x);
    delta
}

/// Make room in `m` for one more entry, charging a table's growth to `held` first.
fn grow_map<K: Eq + std::hash::Hash, V>(
    m: &mut FxHashMap<K, V>,
    held: &mut Held,
) -> Result<(), BudgetFailure> {
    let cap = m.capacity();
    if m.len() < cap {
        return Ok(());
    }
    let next = table_grown(cap, m.len(), 1);
    let elem = size_of::<(K, V)>();
    held.try_grow(table_bytes(next, elem))?;
    m.reserve(next - m.len());
    held.shrink(table_bytes(cap, elem));
    Ok(())
}

/// Push `x`, charging the list's growth to `held` first.
fn push_charged(v: &mut Vec<u32>, x: u32, held: &mut Held) -> Result<(), BudgetFailure> {
    if v.len() == v.capacity() {
        let cap = v.capacity();
        let add = cap.max(4);
        let next = cap
            .checked_add(add)
            .and_then(|n| n.checked_mul(size_of::<u32>()))
            .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
        held.try_grow(next)?;
        v.reserve_exact(add);
        held.shrink(cap * size_of::<u32>());
    }
    v.push(x);
    Ok(())
}

/// The heap of one kept support.
fn witness_bytes(x: &Witness) -> usize {
    x.len() * size_of::<Atom>() + size_of::<Witness>()
}

fn witnesses_bytes(a: &[Witness]) -> usize {
    a.iter().map(witness_bytes).sum()
}

/// Add a signed change to a byte count.
fn plus(n: usize, delta: isize) -> usize {
    n.saturating_add_signed(delta)
}

/// The minimal supports of an instance whose premises have the supports `ws`, and
/// whether the working bound cut them.
fn product(
    ws: &[&[Witness]],
    cap: usize,
    stop: &dyn Fn() -> bool,
    tmp: &mut Held,
) -> anyhow::Result<(Vec<Witness>, bool)> {
    let mut over = false;
    // the working antichains are charged to `tmp` above what it held, which then holds
    // the result's heap
    let base = tmp.bytes();
    let mut r: Vec<Witness> = vec![Witness::default()];
    let mut rb = witnesses_bytes(&r);
    for p in ws {
        if p.is_empty() {
            tmp.set(base)?;
            return Ok((Vec::new(), over));
        }
        let mut nb = 0usize;
        // the working antichain stays within twice the bound, and each step forms at most
        // `MAX_UNIONS` unions, so one instance's work is bounded whatever its premises keep
        let mut next = Vec::with_capacity((r.len() * p.len()).min(2 * cap + 1));
        let mut unions = 0usize;
        'step: for a in &r {
            for b in p.iter() {
                if unions == MAX_UNIONS {
                    over = true;
                    break 'step;
                }
                unions += 1;
                if unions.is_multiple_of(1024) && stop() {
                    anyhow::bail!("reasoning cancelled");
                }
                nb = plus(nb, insert_min(&mut next, a.union(b)));
                if next.len() > 2 * cap {
                    next.truncate(cap);
                    nb = witnesses_bytes(&next);
                    over = true;
                }
                tmp.try_set(base + rb + nb)?;
            }
        }
        if next.len() > cap {
            next.truncate(cap);
            nb = witnesses_bytes(&next);
            over = true;
        }
        r = next;
        rb = nb;
        tmp.set(base + rb)?;
    }
    Ok((r, over))
}

/// The heap of one instance body of `n` premises as [`Found`] keeps it.
fn body_bytes(n: usize) -> usize {
    n * size_of::<Triple>() + 2 * size_of::<usize>()
}

fn bind_atom(a: &engine::Atom, t: &Triple, b: &mut [u64]) -> bool {
    for (s, v) in [(a.s, t[0]), (a.p, t[1]), (a.o, t[2])] {
        match s {
            Slot::Const(c) if c != v => return false,
            Slot::Var(x) => {
                if b[x] == 0 {
                    b[x] = v;
                } else if b[x] != v {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

fn reads_list(k: BuiltinKind) -> bool {
    use BuiltinKind as B;
    matches!(
        k,
        B::ListMember
            | B::ListContains
            | B::ListNotContains
            | B::ListLength
            | B::ListEntry
            | B::ListForAll
    )
}

impl<'a> Cx<'a> {
    fn new(
        c: &'a Closure,
        rules: &'a [CRule],
        plans: &'a Plans,
        abort: &'a AtomicBool,
        limits: &'a Limits,
    ) -> anyhow::Result<Cx<'a>> {
        let negated: Vec<bool> = rules
            .iter()
            .map(|r| {
                // a rule that tests for absent data, whose supports can be smaller than
                // the data it reads
                r.builtins
                    .iter()
                    .any(|b| matches!(b.kind, BuiltinKind::NoValue | BuiltinKind::ListNotContains))
            })
            .collect();
        let any_negated = negated.iter().any(|&n| n);
        let t = &c.terms;
        // the program is not monotone when a smaller set of facts can derive facts the
        // closure lacks, also through intermediates it does not hold: under a negation,
        // under the default geometries, which a feature gains when it has fewer
        // geometries, and while a list of the closure is not plain, since a subset can
        // make it plain with other members. No support is then shown to be complete,
        // and the rules that read lists keep no witnesses, since a subset can also derive
        // a second cell of a list that is plain now and break what a support reads.
        let lists = rules.iter().any(engine::reads_lists);
        let nonmonotone = any_negated
            || c.defaults.is_some()
            || (lists && tangled_lists(c, t.rdf_first, t.rdf_rest, limits)?);
        let opaque = nonmonotone
            || rules.iter().enumerate().any(|(ri, r)| {
                r.builtins.iter().any(|b| b.kind == BuiltinKind::MakeTemp)
                    || r.head.iter().enumerate().any(|(hi, h)| match h {
                        CHead::Action(..) => true,
                        CHead::Triple(..) => plans.back[ri][hi].is_none(),
                    })
            });
        let unsure_lists = rules
            .iter()
            .map(|r| nonmonotone && engine::reads_lists(r))
            .collect();
        Ok(Cx {
            c,
            rules,
            plans,
            abort,
            cancel: limits.cancel.as_deref(),
            budget: &limits.budget,
            negated,
            opaque,
            lists,
            unsure_lists,
            first: t.rdf_first,
            rest: t.rdf_rest,
            nil: t.rdf_nil,
        })
    }

    /// Whether `t` may be the head of a rule instance.
    fn head_candidate(&self, t: &Triple) -> bool {
        self.plans.heads(t[1]).next().is_some()
    }

    /// The list cells that the list builtins of an instance of `rule` with bindings
    /// `vals` read, added to `out`; false when a list is not plain (a cell with several
    /// values, whose reading depends on the order of derivation, or a cycle).
    fn list_cells(
        &self,
        rule: &CRule,
        vals: &[u64],
        out: &mut Vec<Triple>,
        memo: &mut Memo,
    ) -> bool {
        let g = &self.c.graph;
        for bi in &rule.builtins {
            if !reads_list(bi.kind) {
                continue;
            }
            let l = match bi.args.first() {
                Some(Slot::Var(v)) => vals[*v],
                Some(Slot::Const(c)) => *c,
                _ => continue,
            };
            let mut cur = l;
            let mut seen = 0usize;
            while cur != self.nil {
                seen += 1;
                if seen > MAX_LIST {
                    return false;
                }
                let one = |memo: &mut Memo, p: u64| -> Option<u64> {
                    let mut v = g.objects(memo, cur, p, g.len());
                    v.sort_unstable();
                    v.dedup();
                    (v.len() == 1).then(|| v[0])
                };
                let (Some(f), Some(r)) = (one(memo, self.first), one(memo, self.rest)) else {
                    return false;
                };
                out.push([cur, self.first, f]);
                out.push([cur, self.rest, r]);
                if r == l {
                    return false;
                }
                cur = r;
            }
        }
        true
    }

    /// The rule instances that derive `t`, with their premises: the facts their atoms
    /// match, the facts `listForAll` reads and the cells of the lists they read.
    /// Each instance's body is charged through `charge` before it is kept, and the
    /// search stops at the first charge that does not fit.
    fn instances(
        &self,
        t: Triple,
        memo: &mut Memo,
        charge: &mut dyn FnMut(isize) -> Result<(), BudgetFailure>,
    ) -> anyhow::Result<Found> {
        let g = &self.c.graph;
        let mut f = Found::default();
        let mut lmemo = Memo::default();
        let mut refused: Option<BudgetFailure> = None;
        for (ri, hi) in self.plans.heads(t[1]) {
            let Some((steps, pre)) = &self.plans.back[ri][hi] else {
                continue;
            };
            let rule = &self.rules[ri];
            let Some(mut b) = bind_head(rule, hi, pre, t) else {
                continue;
            };
            // a rule with a negation may derive the fact from fewer facts than the closure
            // holds, also when its negation fails now: its supports are not shown
            if self.negated[ri] || self.unsure_lists[ri] {
                f.unknown = true;
                continue;
            }
            let m = std::mem::take(memo);
            let (failed, m) = {
                let mut sink = |body: &[Triple], implicit: &[Triple], vals: &[u64]| -> bool {
                    // an instance that uses its own head adds no support
                    if body.iter().chain(implicit).any(|u| *u == t) {
                        return true;
                    }
                    if f.bodies.len() >= MAX_INSTANCES {
                        f.capped = true;
                        return false;
                    }
                    let mut all: Vec<Triple> = body.iter().chain(implicit).copied().collect();
                    if !self.list_cells(rule, vals, &mut all, &mut lmemo) {
                        f.unknown = true;
                        return true;
                    }
                    all.sort_unstable();
                    all.dedup();
                    // charged before it is kept, list cells included
                    if let Err(e) = charge(body_bytes(all.len()) as isize) {
                        refused = Some(e);
                        return false;
                    }
                    f.bodies.push(all.into());
                    true
                };
                let mut ev = Eval::new(
                    g,
                    &self.c.terms,
                    rule,
                    steps,
                    g.len(),
                    self.abort,
                    self.cancel,
                    usize::MAX,
                    Mode::Instances {
                        head: hi,
                        target: t,
                        sink: &mut sink,
                    },
                    self.budget,
                );
                ev.lend_memo(m);
                ev.run(0, &mut b, None);
                (ev.err.take(), ev.take_memo())
            };
            *memo = m;
            if let Some(e) = refused.take() {
                return Err(e.into());
            }
            if let Some(e) = failed {
                return Err(e);
            }
            if f.capped {
                break;
            }
        }
        // repeated instances (other bindings of the same body) add nothing, and their
        // charge is given back
        let all: usize = f.bodies.iter().map(|b| body_bytes(b.len())).sum();
        f.bodies.sort_unstable();
        f.bodies.dedup();
        let kept: usize = f.bodies.iter().map(|b| body_bytes(b.len())).sum();
        charge(-((all - kept) as isize))?;
        Ok(f)
    }

    /// The facts of the closure that are heads of rule instances with `t` matched by a
    /// body atom; `t` need not be in the closure.
    /// The list is charged to `held` before it grows, and stays charged there.
    fn users(&self, t: &Triple, memo: &mut Memo, held: &mut Held) -> anyhow::Result<Vec<Triple>> {
        let g = &self.c.graph;
        let mut out = Vec::new();
        for (ri, r) in self.rules.iter().enumerate() {
            for (d, a) in r.atoms.iter().enumerate() {
                let Some(steps) = &self.plans.forward[ri][d] else {
                    continue;
                };
                let mut b = vec![0u64; r.nvars];
                if !bind_atom(a, t, &mut b) {
                    continue;
                }
                let m = std::mem::take(memo);
                let mut ev = Eval::new(
                    g,
                    &self.c.terms,
                    r,
                    steps,
                    g.len(),
                    self.abort,
                    self.cancel,
                    usize::MAX,
                    Mode::Existing,
                    self.budget,
                );
                ev.lend_memo(m);
                // the plan's first step is the atom that `t` matches
                ev.run(1, &mut b, None);
                *memo = ev.take_memo();
                if let Some(e) = ev.err.take() {
                    return Err(e);
                }
                let (heads, _held) = ev.take_out();
                if out.capacity() - out.len() < heads.len() {
                    let more = heads.len().max(out.capacity());
                    held.try_grow(more * size_of::<Triple>())?;
                    out.reserve_exact(more);
                }
                out.extend(heads);
            }
        }
        out.sort_unstable();
        out.dedup();
        out.retain(|h| h != t);
        Ok(out)
    }
}

/// The instances and users a pass has read, by node.
#[derive(Default)]
struct Pass {
    ins: FxHashMap<u32, Ins>,
    /// the heap of `ins`, charged as it grows
    scratch: Option<Held>,
    users: FxHashMap<u32, Vec<u32>>,
    /// the users of every node are known from `ins` (a full build)
    complete: bool,
    evaluated: u64,
    /// a list cell changed while the rules read lists: build again
    rebuild: bool,
}

impl Witnesses {
    fn empty(max: usize, budget: &Arc<Budget>) -> Witnesses {
        Witnesses {
            max,
            nodes: Vec::new(),
            index: FxHashMap::default(),
            free: Vec::new(),
            rev: FxHashMap::default(),
            rev_entries: 0,
            rev_cap: 0,
            node_bytes: 0,
            opaque: false,
            held: Held::new(budget),
            report: WitnessReport::default(),
        }
    }

    /// The bound of witnesses per fact.
    pub fn max(&self) -> usize {
        self.max
    }

    /// Check a bound asked for.
    pub fn check_max(max: usize) -> anyhow::Result<()> {
        if max == 0 || max > MAX_WITNESSES_LIMIT {
            anyhow::bail!("maxWitnesses must be between 1 and {MAX_WITNESSES_LIMIT}, not {max}");
        }
        Ok(())
    }

    /// The estimated heap.
    pub fn heap_bytes(&self) -> usize {
        debug_assert_eq!(
            self.node_bytes,
            self.nodes.iter().map(Node::bytes).sum::<usize>()
        );
        self.heap_bytes_now()
    }

    /// Charge the witnesses to `budget`, as the closure they belong to is.
    pub(crate) fn set_budget(&mut self, budget: &Arc<Budget>) -> Result<(), BudgetFailure> {
        let n = self.held.bytes();
        let mut h = Held::try_new(budget, n)?;
        std::mem::swap(&mut self.held, &mut h);
        drop(h);
        Ok(())
    }

    /// Charge the heap as it is now, even past the limit: the witnesses change one node
    /// at a time, and the passes check the budget after each, so a run past its limit
    /// stops there.
    fn recharge(&mut self) -> Result<(), BudgetFailure> {
        let n = self.heap_bytes_now();
        self.held.set(n)
    }

    /// [`heap_bytes`](Self::heap_bytes) without the check, in constant time.
    fn heap_bytes_now(&self) -> usize {
        self.node_bytes
            + (self.nodes.capacity() - self.nodes.len()) * size_of::<Node>()
            + self.free.capacity() * 4
            + table_bytes(self.index.capacity(), size_of::<(Triple, u32)>())
            + table_bytes(self.rev.capacity(), size_of::<(FactKey, Vec<u32>)>())
            + self.rev_cap * 4
    }

    fn settle_charge(&mut self) -> Result<(), BudgetFailure> {
        let n = self.heap_bytes();
        self.held.try_set(n)
    }

    fn node(&self, t: &Triple) -> Option<u32> {
        self.index.get(t).copied()
    }

    fn add_node(&mut self, t: Triple, key: Option<FactKey>) -> Result<u32, BudgetFailure> {
        if let Some(&i) = self.index.get(&t) {
            return Ok(i);
        }
        grow_map(&mut self.index, &mut self.held)?;
        if self.free.is_empty() && self.nodes.len() == self.nodes.capacity() {
            let cap = self.nodes.capacity();
            let next = cap
                .checked_mul(2)
                .map(|n| n.max(4))
                .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
            let bytes = next
                .checked_mul(size_of::<Node>())
                .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
            self.held.try_grow(bytes)?;
            self.nodes.reserve_exact(next - self.nodes.len());
            self.held.shrink(cap * size_of::<Node>());
        }
        let n = Node {
            t,
            key,
            w: Vec::new(),
            local: false,
            trunc: false,
            live: true,
        };
        let i = match self.free.pop() {
            Some(i) => {
                self.nodes[i as usize] = n;
                i
            }
            None => {
                self.nodes.push(n);
                self.node_bytes += size_of::<Node>();
                (self.nodes.len() - 1) as u32
            }
        };
        self.index.insert(t, i);
        self.recharge()?;
        Ok(i)
    }

    fn remove_node(&mut self, i: u32) -> Result<(), BudgetFailure> {
        if !self.nodes[i as usize].live {
            return Ok(());
        }
        push_charged(&mut self.free, i, &mut self.held)?;
        let n = &mut self.nodes[i as usize];
        n.live = false;
        let gone = n.bytes() - size_of::<Node>();
        n.w = Vec::new();
        self.node_bytes -= gone;
        let t = n.t;
        self.index.remove(&t);
        self.recharge()
    }

    /// Replace the kept supports of node `i`, indexing the quads they name.
    fn set(
        &mut self,
        i: u32,
        w: Vec<Witness>,
        local: bool,
        trunc: bool,
    ) -> Result<(), BudgetFailure> {
        let atoms = w
            .iter()
            .try_fold(0usize, |n, x| n.checked_add(x.len()))
            .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
        let scratch = atoms
            .checked_mul(size_of::<FactKey>())
            .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
        let _scratch = Held::try_new_exact(self.held.budget(), scratch)?;
        let mut added = Vec::with_capacity(atoms);
        let node = &self.nodes[i as usize];
        for x in &w {
            if node.w.contains(x) {
                continue;
            }
            for atom in x.atoms() {
                if let Atom::Fact(key) = atom {
                    added.push(*key);
                }
            }
        }
        added.sort_unstable();
        added.dedup();
        let payload = witnesses_bytes(&w)
            .checked_add((w.capacity() - w.len()) * size_of::<Witness>())
            .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
        self.held.try_grow(payload)?;
        for key in added {
            if !self.rev.contains_key(&key) {
                grow_map(&mut self.rev, &mut self.held)?;
            }
            let users = self.rev.entry(key).or_default();
            let cap = users.capacity();
            push_charged(users, i, &mut self.held)?;
            self.rev_cap += users.capacity() - cap;
            self.rev_entries += 1;
        }
        let node = &mut self.nodes[i as usize];
        let before = node.bytes();
        let retired = std::mem::replace(&mut node.w, w);
        node.local = local;
        node.trunc = trunc;
        let after = node.bytes();
        self.node_bytes = self.node_bytes + after - before;
        drop(retired);
        self.recharge()
    }

    fn set_base_support(
        &mut self,
        i: u32,
        key: FactKey,
        local: bool,
        trunc: bool,
    ) -> Result<(), BudgetFailure> {
        let _temporary = Held::try_new_exact(
            self.held.budget(),
            size_of::<Witness>() + 2 * size_of::<Atom>(),
        )?;
        self.set(i, vec![Witness::new(vec![Atom::Fact(key)])], local, trunc)
    }

    /// The witnesses of `t` as an annotation of the `why` semiring: each kept support
    /// counts once, and the unknown count is 1 when supports may be missing. `None` when
    /// `t` is not a fact the witnesses know.
    pub(crate) fn ann(&self, t: &Triple) -> Option<Ann> {
        let n = &self.nodes[self.node(t)? as usize];
        Some(Ann::Why(Box::new(Why {
            unknown: n.trunc as i64,
            supports: n.w.iter().map(|x| (x.clone(), 1)).collect(),
        })))
    }

    /// The witnesses of `t` as a reader sees them ([`Ann::report`]); the count is the
    /// number of kept supports. A fact of the base tier that no rule derives has its own
    /// quad as its only witness.
    pub(crate) fn report_of(&self, c: &Closure, t: &Triple) -> Option<Annotation> {
        if let Some(a) = self.ann(t) {
            // the count is that of the kept witnesses; the unknown support only marks
            // the annotation truncated
            let mut r = a.report(self.max);
            r.count = r.witnesses.len() as i64;
            return Some(r);
        }
        if !c.graph.contains(t) || kind(c, t) != Kind::Base {
            return None;
        }
        let w = Witness::new(vec![Atom::Fact(fact_key(c, t))]);
        Some(Annotation {
            count: 1,
            lineage: Vec::new(),
            witnesses: vec![w],
            truncated: hidden(c, self.opaque, t),
        })
    }

    /// The facts whose kept witnesses name the input quad `k`.
    pub(crate) fn dependents(&self, k: &FactKey) -> Vec<Triple> {
        let Some(v) = self.rev.get(k) else {
            return Vec::new();
        };
        let mut out: Vec<Triple> = v
            .iter()
            .map(|&i| &self.nodes[i as usize])
            .filter(|n| {
                n.live
                    && n.w
                        .iter()
                        .any(|x| x.atoms().binary_search(&Atom::Fact(*k)).is_ok())
            })
            .map(|n| n.t)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Replace ids that moved (local ids the store now has).
    pub(crate) fn remap(&mut self, moved: &FxHashMap<u64, u64>) {
        if moved.is_empty() {
            return;
        }
        let m = |x: u64| moved.get(&x).copied().unwrap_or(x);
        self.index.clear();
        for (i, n) in self.nodes.iter_mut().enumerate() {
            if !n.live {
                continue;
            }
            n.t = [m(n.t[0]), m(n.t[1]), m(n.t[2])];
            self.index.insert(n.t, i as u32);
        }
    }

    /// Build the witnesses of every fact of `c`.
    pub(crate) fn build(
        c: &mut Closure,
        rules: &[CRule],
        max: usize,
        limits: &Limits,
    ) -> anyhow::Result<Witnesses> {
        let t0 = Instant::now();
        Self::check_max(max)?;
        let plans = Plans::new(&mut c.graph, rules)?;
        // the plans are charged while the pass reads them, as the update charges its own
        let _plans_held = Held::try_new(&limits.budget, plans.bytes)?;
        let c = &*c;
        let abort = AtomicBool::new(false);
        let cx = Cx::new(c, rules, &plans, &abort, limits)?;
        let mut w = Witnesses::empty(max, &limits.budget);
        w.opaque = cx.opaque;
        // the facts that may be heads: every derived fact, and the base facts whose
        // predicate a head can have
        // the list is charged before it grows, and the scan stops at a refusal or when the
        // run is cancelled
        let mut held = Held::new(&limits.budget);
        let mut heads: Vec<Triple> = Vec::new();
        let mut stop: Option<anyhow::Error> = None;
        let mut push = |t: Triple, stop: &mut Option<anyhow::Error>| -> bool {
            if heads.len() == heads.capacity() {
                let more = heads.capacity().max(1024);
                if let Err(e) = held.try_grow(more * size_of::<Triple>()) {
                    *stop = Some(e.into());
                    return false;
                }
                heads.reserve_exact(more);
            }
            if heads.len().is_multiple_of(1 << 16) && cancelled(&cx) {
                *stop = Some(anyhow::anyhow!("reasoning cancelled"));
                return false;
            }
            heads.push(t);
            true
        };
        for (t, _) in c.live() {
            if !push(t, &mut stop) {
                break;
            }
        }
        if stop.is_none()
            && let Some(base) = c.graph.base()
        {
            let mut scan = |p: Option<u64>| base.for_each(None, p, None, |t| push(t, &mut stop));
            if plans.any_pred.is_empty() {
                let mut ps: Vec<u64> = plans.by_pred.keys().copied().collect();
                ps.sort_unstable();
                for p in ps {
                    scan(Some(p))?;
                }
            } else {
                scan(None)?;
            }
        }
        if let Some(e) = stop {
            return Err(e);
        }
        heads.sort_unstable();
        heads.dedup();
        // the instances are charged as each head's are found
        let found: Vec<anyhow::Result<(Vec<Found>, usize)>> = heads
            .par_chunks(256)
            .map(|ch| {
                let mut memo = Memo::default();
                let mut meter = Meter::new(cx.budget, true);
                let mut out = Vec::with_capacity(ch.len());
                for &t in ch {
                    if cancelled(&cx) {
                        anyhow::bail!("reasoning cancelled");
                    }
                    meter.add(size_of::<Found>())?;
                    let f = cx.instances(t, &mut memo, &mut |n| {
                        if n >= 0 {
                            meter.add(n as usize)
                        } else {
                            meter.sub(n.unsigned_abs());
                            Ok(())
                        }
                    })?;
                    out.push(f);
                }
                Ok((out, meter.into_bytes()))
            })
            .collect();
        let mut found_held = Held::new(&limits.budget);
        let found: Vec<Vec<Found>> = {
            let mut ok = Vec::with_capacity(found.len());
            let mut err = None;
            for f in found {
                match f {
                    Ok((f, n)) => {
                        found_held.adopt(n);
                        ok.push(f);
                    }
                    Err(e) => err = err.or(Some(e)),
                }
            }
            if let Some(e) = err {
                return Err(e);
            }
            ok
        };
        let mut pass = Pass {
            complete: true,
            ..Pass::default()
        };
        held.try_grow(heads.len() * 4)?;
        let mut seeds = Vec::with_capacity(heads.len());
        for &t in &heads {
            let key = (kind(c, &t) == Kind::Base).then(|| fact_key(c, &t));
            seeds.push(w.add_node(t, key)?);
            // the nodes charge themselves as they are added
            cx.budget.check()?;
        }
        let mut bytes = 0usize;
        for (ch, f) in heads.chunks(256).zip(found) {
            for (t, f) in ch.iter().zip(f) {
                // each fact's instances are given back as their node ids are charged
                let fb = size_of::<Found>()
                    + f.bodies.iter().map(|b| body_bytes(b.len())).sum::<usize>();
                let ib: usize = f.bodies.iter().map(|b| b.len() * 8 + 32).sum();
                held.try_grow(ib)?;
                bytes += ib;
                let i = w.index[t];
                let mut ins = Ins {
                    bodies: Vec::with_capacity(f.bodies.len()),
                    unknown: f.unknown,
                    capped: f.capped,
                };
                for body in f.bodies {
                    let ids: Box<[u32]> = body
                        .iter()
                        .map(|u| {
                            Ok(match w.node(u) {
                                Some(j) => j,
                                None => {
                                    let key = (kind(c, u) == Kind::Base).then(|| fact_key(c, u));
                                    w.add_node(*u, key)?
                                }
                            })
                        })
                        .collect::<Result<_, BudgetFailure>>()?;
                    ins.bodies.push(ids);
                }
                grow_map(&mut pass.ins, &mut held)?;
                pass.ins.insert(i, ins);
                found_held.shrink(fb);
                cx.budget.check()?;
            }
        }
        held.try_set(
            bytes
                + heads.len() * 4
                + heads.capacity() * size_of::<Triple>()
                + table_bytes(pass.ins.capacity(), size_of::<(u32, Ins)>()),
        )?;
        drop(found_held);
        drop(heads);
        // the base facts that are premises only have their own quad
        for i in 0..w.nodes.len() as u32 {
            if !pass.ins.contains_key(&i)
                && let Some(k) = w.nodes[i as usize].key
            {
                let h = hidden(c, cx.opaque, &w.nodes[i as usize].t);
                w.set_base_support(i, k, h, h)?;
                cx.budget.check()?;
            }
        }
        // users from the instances
        let mut users: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        for (&h, ins) in &pass.ins {
            for b in &ins.bodies {
                for &p in b.iter() {
                    grow_map(&mut users, &mut held)?;
                    push_charged(users.entry(p).or_default(), h, &mut held)?;
                }
            }
        }
        for v in users.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        pass.users = users;
        w.settle(&cx, seeds, &mut pass)?;
        drop(held);
        w.settle_charge()?;
        w.finish_report(c, true, pass.evaluated, t0);
        Ok(w)
    }

    /// Find, before an update deletes facts from `c`, what the update changes: the keys
    /// of the input quads that go, and the truncated facts and facts with such a key that
    /// a removed fact reaches. Called when the closure still holds every fact of the
    /// previous closure (and the removed explicit facts as candidates).
    pub(crate) fn prepare(
        &mut self,
        c: &mut Closure,
        rules: &[CRule],
        limits: &Limits,
        removed: &[Triple],
        moved: &[Triple],
    ) -> anyhow::Result<Prepared> {
        let mut p = Prepared::default();
        let mut start: Vec<Triple> = Vec::new();
        // each starting fact is charged before it is collected, with its dead key, its
        // place in the search and a rekeyed entry
        let mut reached = Held::new(&limits.budget);
        let one = REACHED_BYTES + VISITED_BYTES + size_of::<FactKey>() + 8 + 4;
        for t in removed {
            reached.try_grow(one)?;
            if let Some(i) = self.node(t)
                && let Some(k) = self.nodes[i as usize].key.take()
            {
                p.dead.insert(k);
            }
            start.push(*t);
        }
        for t in moved {
            if let Some(i) = self.node(t)
                && let Some(k) = self.nodes[i as usize].key
            {
                let now = fact_key(c, t);
                if now != k {
                    reached.try_grow(one)?;
                    p.dead.insert(k);
                    self.nodes[i as usize].key = Some(now);
                    p.rekeyed.push(i);
                    start.push(*t);
                }
            }
        }
        if p.dead.is_empty() && start.is_empty() {
            return Ok(p);
        }
        let lost = self.lost(&p.dead, &mut reached)?;
        let plans = Plans::new(&mut c.graph, rules)?;
        // the plans are charged while the pass reads them, as the update charges its own
        let _plans_held = Held::try_new(&limits.budget, plans.bytes)?;
        let abort = AtomicBool::new(false);
        let cx = Cx::new(c, rules, &plans, &abort, limits)?;
        let mut seen: FxHashSet<u32> = FxHashSet::default();
        // the removed facts that stay derived are evaluated again, as are the facts that
        // lost a kept support
        for t in &start {
            if let Some(i) = self.node(t)
                && seen.insert(i)
            {
                p.region.push(i);
            }
        }
        let mut queue: VecDeque<Triple> = start.into_iter().collect();
        for &i in &lost {
            if seen.insert(i) {
                p.region.push(i);
                queue.push_back(self.nodes[i as usize].t);
            }
        }
        let mut memo = Memo::default();
        reached.try_grow(lost.len() * VISITED_BYTES)?;
        let mut visited: FxHashSet<Triple> = queue.iter().copied().collect();
        while let Some(x) = queue.pop_front() {
            if cancelled(&cx) {
                anyhow::bail!("reasoning cancelled");
            }
            let mut held = Held::new(cx.budget);
            for h in cx.users(&x, &mut memo, &mut held)? {
                if !visited.insert(h) {
                    continue;
                }
                reached.try_grow(VISITED_BYTES)?;
                let Some(j) = self.node(&h) else {
                    continue;
                };
                if (self.nodes[j as usize].trunc || lost.contains(&j)) && seen.insert(j) {
                    reached.try_grow(REACHED_BYTES)?;
                    p.region.push(j);
                    queue.push_back(h);
                }
            }
        }
        p.held = Some(reached);
        Ok(p)
    }

    /// The nodes whose kept witnesses name one of `dead`.
    fn lost(
        &self,
        dead: &FxHashSet<FactKey>,
        held: &mut Held,
    ) -> Result<FxHashSet<u32>, BudgetFailure> {
        let mut out = FxHashSet::default();
        for k in dead {
            for &i in self.rev.get(k).into_iter().flatten() {
                let n = &self.nodes[i as usize];
                if n.live
                    && n.w
                        .iter()
                        .any(|x| x.atoms().binary_search(&Atom::Fact(*k)).is_ok())
                    && out.insert(i)
                {
                    held.try_grow(REACHED_BYTES)?;
                }
            }
        }
        Ok(out)
    }

    /// Bring the witnesses to the closure `c` after an update: the explicit facts
    /// `inserted` were added and `removed` ones taken out, `deleted` facts left the
    /// derived tier and `added` joined it, and `touched` default geometries changed.
    /// `prep` is what [`prepare`](Self::prepare) found before the deletion.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update(
        &mut self,
        c: &mut Closure,
        rules: &[CRule],
        limits: &Limits,
        prep: Prepared,
        inserted: &[Triple],
        removed: &[Triple],
        deleted: &[Triple],
        added: &[Triple],
        touched: &[Triple],
    ) -> anyhow::Result<bool> {
        let t0 = Instant::now();
        if prep.rebuild {
            return Ok(false);
        }
        // the supports that name a quad that is gone
        // the seeds are charged as they are collected, until the pass charges its queue
        let mut sh = Held::new(&limits.budget);
        let mut seeds: Vec<u32> = Vec::new();
        for k in &prep.dead {
            let Some(v) = self.rev.remove(k) else {
                continue;
            };
            self.rev_entries -= v.len().min(self.rev_entries);
            self.rev_cap -= v.capacity().min(self.rev_cap);
            for i in v {
                let n = &mut self.nodes[i as usize];
                if !n.live {
                    continue;
                }
                let (before, bytes) = (n.w.len(), n.bytes());
                n.w.retain(|x| x.atoms().binary_search(&Atom::Fact(*k)).is_err());
                if n.w.len() != before {
                    self.node_bytes -= bytes - n.bytes();
                    sh.try_grow(4)?;
                    seeds.push(i);
                }
            }
            self.recharge()?;
        }
        // the facts that left the closure
        for t in deleted.iter().chain(removed).chain(touched) {
            if let Some(i) = self.node(t)
                && !c.graph.contains(t)
            {
                self.remove_node(i)?;
            }
        }
        // truncation is found again in the region
        for &i in &prep.region {
            let n = &mut self.nodes[i as usize];
            if n.live {
                n.trunc = false;
                n.local = false;
                sh.try_grow(4)?;
                seeds.push(i);
            }
        }
        sh.try_grow(4 * prep.rekeyed.len())?;
        seeds.extend(prep.rekeyed.iter().copied());
        for t in inserted {
            if !c.graph.contains(t) {
                continue;
            }
            let i = self.add_node(*t, None)?;
            if kind(c, t) == Kind::Base {
                self.nodes[i as usize].key = Some(fact_key(c, t));
            }
            sh.try_grow(4)?;
            seeds.push(i);
            self.held.budget().check()?;
        }
        for t in added.iter().chain(touched) {
            if c.graph.contains(t) {
                sh.try_grow(4)?;
                seeds.push(self.add_node(*t, None)?);
                self.held.budget().check()?;
            }
        }
        // a fact that moved to the base tier by other means keeps its key current
        for t in removed {
            if let Some(i) = self.node(t)
                && kind(c, t) == Kind::Base
                && self.nodes[i as usize].key.is_none()
            {
                self.nodes[i as usize].key = Some(fact_key(c, t));
                sh.try_grow(4)?;
                seeds.push(i);
            }
        }
        seeds.retain(|&i| self.nodes[i as usize].live);
        seeds.sort_unstable();
        seeds.dedup();
        let plans = Plans::new(&mut c.graph, rules)?;
        // the plans are charged while the pass reads them, as the update charges its own
        let _plans_held = Held::try_new(&limits.budget, plans.bytes)?;
        let c = &*c;
        let abort = AtomicBool::new(false);
        let cx = Cx::new(c, rules, &plans, &abort, limits)?;
        self.opaque = cx.opaque;
        let mut pass = Pass::default();
        self.settle(&cx, seeds, &mut pass)?;
        if pass.rebuild {
            return Ok(false);
        }
        self.compact_rev()?;
        self.settle_charge()?;
        self.finish_report(c, false, pass.evaluated, t0);
        Ok(true)
    }

    /// Drop the stale entries of the reverse index once they are most of it.
    fn compact_rev(&mut self) -> Result<(), BudgetFailure> {
        let live: usize = self
            .nodes
            .iter()
            .filter(|n| n.live)
            .map(|n| n.w.iter().map(|x| x.len()).sum::<usize>())
            .sum();
        if self.rev_entries <= 2 * live + 1024 {
            return Ok(());
        }
        // the new index is charged as it is built, beside the old one
        let mut tmp = Held::new(self.held.budget());
        let mut rev: FxHashMap<FactKey, Vec<u32>> = FxHashMap::default();
        let (mut n, mut cap) = (0, 0);
        for (i, node) in self.nodes.iter().enumerate() {
            if !node.live {
                continue;
            }
            let mut ks: Vec<FactKey> = node
                .w
                .iter()
                .flat_map(|x| x.atoms())
                .filter_map(|a| match a {
                    Atom::Fact(k) => Some(*k),
                    Atom::Absent(_) => None,
                })
                .collect();
            ks.sort_unstable();
            ks.dedup();
            for k in ks {
                grow_map(&mut rev, &mut tmp)?;
                let e = rev.entry(k).or_default();
                let c0 = e.capacity();
                push_charged(e, i as u32, &mut tmp)?;
                cap += e.capacity() - c0;
                n += 1;
            }
        }
        self.rev = rev;
        self.rev_entries = n;
        self.rev_cap = cap;
        self.recharge()?;
        tmp.set(0)?;
        Ok(())
    }

    fn finish_report(&mut self, c: &Closure, built: bool, evaluated: u64, t0: Instant) {
        let mut r = WitnessReport {
            max_witnesses: self.max,
            built,
            evaluated,
            bytes: self.heap_bytes() as u64,
            millis: t0.elapsed().as_millis() as u64,
            ..Default::default()
        };
        for n in self.nodes.iter().filter(|n| n.live) {
            if kind(c, &n.t) == Kind::Base {
                continue;
            }
            r.facts += 1;
            r.truncated += n.trunc as u64;
            r.witnesses += n.w.len() as u64;
        }
        self.report = r;
    }

    /// The instances of node `i` as nodes, read once per pass. Premises without a node
    /// get one: a base fact, whose own quad is a support, evaluated when it may be a head.
    fn ins<'p>(
        &mut self,
        cx: &Cx<'_>,
        pass: &'p mut Pass,
        i: u32,
        queue: &mut Vec<u32>,
        memo: &mut Memo,
    ) -> anyhow::Result<&'p Ins> {
        if let std::collections::hash_map::Entry::Vacant(_) = pass.ins.entry(i) {
            let t = self.nodes[i as usize].t;
            let held = pass.scratch.get_or_insert_with(|| Held::new(cx.budget));
            // the bodies are charged as they are found, and their nodes before they are kept
            let mut found = 0usize;
            let f = cx.instances(t, memo, &mut |n| {
                if n >= 0 {
                    found += n as usize;
                    held.try_grow(n as usize)
                } else {
                    found -= n.unsigned_abs();
                    held.shrink(n.unsigned_abs());
                    Ok(())
                }
            })?;
            held.try_grow(f.bodies.iter().map(|b| b.len() * 4 + 32).sum::<usize>() + 64)?;
            let mut ins = Ins {
                bodies: Vec::with_capacity(f.bodies.len()),
                unknown: f.unknown,
                capped: f.capped,
            };
            for body in f.bodies {
                let ids: Box<[u32]> = body
                    .iter()
                    .map(|u| {
                        Ok(match self.node(u) {
                            Some(j) => j,
                            None => {
                                let base = kind(cx.c, u) == Kind::Base;
                                let key = base.then(|| fact_key(cx.c, u));
                                let j = self.add_node(*u, key)?;
                                if let Some(k) = key {
                                    let h = hidden(cx.c, cx.opaque, u);
                                    self.set_base_support(j, k, h, h)?;
                                }
                                if !base || cx.head_candidate(u) {
                                    queue.push(j);
                                }
                                j
                            }
                        })
                    })
                    .collect::<Result<_, BudgetFailure>>()?;
                ins.bodies.push(ids);
            }
            held.shrink(found);
            grow_map(&mut pass.ins, held)?;
            pass.ins.insert(i, ins);
        }
        Ok(&pass.ins[&i])
    }

    /// The nodes that are heads of instances with node `i` among their premises.
    fn users_of<'p>(
        &mut self,
        cx: &Cx<'_>,
        pass: &'p mut Pass,
        i: u32,
        memo: &mut Memo,
    ) -> anyhow::Result<&'p [u32]> {
        if pass.complete || pass.users.contains_key(&i) {
            return Ok(pass.users.get(&i).map_or(&[][..], Vec::as_slice));
        }
        let t = self.nodes[i as usize].t;
        let mut found = Held::new(cx.budget);
        let heads = cx.users(&t, memo, &mut found)?;
        // the cached list is charged to the pass before it is built
        pass.scratch
            .get_or_insert_with(|| Held::new(cx.budget))
            .try_grow(heads.len() * 4 + size_of::<(u32, Vec<u32>)>() + 16)?;
        let mut out = Vec::with_capacity(heads.len());
        for h in heads {
            out.push(match self.node(&h) {
                Some(j) => j,
                None => {
                    let key = (kind(cx.c, &h) == Kind::Base).then(|| fact_key(cx.c, &h));
                    self.add_node(h, key)?
                }
            });
        }
        drop(found);
        let held = pass.scratch.get_or_insert_with(|| Held::new(cx.budget));
        grow_map(&mut pass.users, held)?;
        Ok(pass.users.entry(i).or_insert(out).as_slice())
    }

    /// Evaluate the facts of `seeds`, and the users of every fact whose value changes,
    /// until no value changes.
    fn settle(&mut self, cx: &Cx<'_>, seeds: Vec<u32>, pass: &mut Pass) -> anyhow::Result<()> {
        let cap = pool_cap(self.max);
        // the worklists are charged per fact queued or changed
        let mut wl = Held::new(cx.budget);
        wl.try_grow(seeds.len() * WORK_BYTES)?;
        let mut queue: VecDeque<u32> = VecDeque::with_capacity(seeds.len());
        let mut queued: FxHashSet<u32> = FxHashSet::default();
        for i in seeds {
            if queued.insert(i) {
                queue.push_back(i);
            }
        }
        wl.set(queue.len() * WORK_BYTES)?;
        let mut changes: FxHashMap<u32, u32> = FxHashMap::default();
        let mut memo = Memo::default();
        let mut more: Vec<u32> = Vec::new();
        let mut gone: Vec<u32> = Vec::new();
        let mut frozen: FxHashSet<u32> = FxHashSet::default();
        // the supports of the evaluation in progress
        let mut tmp = Held::new(cx.budget);
        let mut ticks = 0u32;
        while let Some(i) = queue.pop_front() {
            // regression (fourth review of item 416): no evaluation leaves its supports
            // charged here once they move into a node or are dropped
            debug_assert_eq!(tmp.bytes(), 0);
            queued.remove(&i);
            wl.shrink(WORK_BYTES);
            ticks = ticks.wrapping_add(1);
            if ticks.is_multiple_of(256) && cancelled(cx) {
                anyhow::bail!("reasoning cancelled");
            }
            // the witnesses charge their growth as it happens
            cx.budget.check()?;
            if !self.nodes[i as usize].live {
                continue;
            }
            let t = self.nodes[i as usize].t;
            if !cx.c.graph.contains(&t) {
                // not a fact of the closure: the node goes after the pass, whose caches
                // name it
                gone.push(i);
                continue;
            }
            let k = kind(cx.c, &t);
            match (k, self.nodes[i as usize].key) {
                (Kind::Base, None) => self.nodes[i as usize].key = Some(fact_key(cx.c, &t)),
                (Kind::Derived | Kind::Flagged, Some(_)) => self.nodes[i as usize].key = None,
                _ => {}
            }
            pass.evaluated += 1;
            more.clear();
            let ins = self.ins(cx, pass, i, &mut more, &mut memo)?;
            // evaluate
            let n = &self.nodes[i as usize];
            let mut local = ins.unknown || ins.capped;
            let mut pool: Vec<Witness> = Vec::new();
            match k {
                Kind::Base => {
                    let k = n.key.expect("the key was set above");
                    pool.push(Witness::new(vec![Atom::Fact(k)]));
                    local |= hidden(cx.c, cx.opaque, &t);
                }
                Kind::Flagged => local = true,
                Kind::Derived => local |= ins.bodies.is_empty() || cx.opaque,
            }
            let mut trunc = false;
            let mut ws: Vec<&[Witness]> = Vec::new();
            // the products and the pool are charged while they are built
            let mut pool_bytes = witnesses_bytes(&pool);
            tmp.try_set(pool_bytes)?;
            for body in &ins.bodies {
                ws.clear();
                for &p in body.iter() {
                    let pn = &self.nodes[p as usize];
                    trunc |= pn.trunc;
                    ws.push(&pn.w);
                }
                let (prod, over) = product(&ws, cap, &|| cancelled(cx), &mut tmp)?;
                local |= over;
                for x in prod {
                    pool_bytes = plus(pool_bytes, insert_min(&mut pool, x));
                }
                if pool.len() > cap {
                    pool.truncate(cap);
                    pool_bytes = witnesses_bytes(&pool);
                    local = true;
                }
                tmp.set(pool_bytes)?;
            }
            if pool.len() > self.max {
                pool.truncate(self.max);
                local = true;
            }
            // the empty support absorbs every other one: the fact follows from nothing,
            // and that is its only minimal support whatever its premises keep
            if pool.first().is_some_and(Witness::is_empty) {
                pool.truncate(1);
                local = false;
                trunc = false;
            }
            let trunc = trunc || local;
            // the pool now either moves into the node, which charges it, or is dropped
            tmp.set(0)?;
            let n = &self.nodes[i as usize];
            let same = n.w == pool && n.local == local && n.trunc == trunc;
            if cx.lists && !same && (t[1] == cx.first || t[1] == cx.rest) && !pass.complete {
                pass.rebuild = true;
                return Ok(());
            }
            for &j in &more {
                if queued.insert(j) {
                    wl.try_grow(WORK_BYTES)?;
                    queue.push_back(j);
                }
            }
            if same {
                continue;
            }
            let was = n.trunc;
            if !changes.contains_key(&i) {
                wl.try_grow(WORK_BYTES)?;
            }
            let c = changes.entry(i).or_insert(0);
            *c += 1;
            if *c > MAX_CHANGES {
                // frozen: the value stays, known to be incomplete
                frozen.insert(i);
                if !n.trunc {
                    let w = n.w.clone();
                    self.set(i, w, true, true)?;
                } else {
                    continue;
                }
            } else {
                self.set(i, pool, local, trunc)?;
            }
            if was && !trunc {
                // the truncation of the facts downstream may rest on this fact's, also
                // through a cycle of truncated facts: it is reset for all of them and
                // found again, so that it stays the least one
                let mut stack = vec![i];
                while let Some(x) = stack.pop() {
                    for &u in self.users_of(cx, pass, x, &mut memo)? {
                        let un = &mut self.nodes[u as usize];
                        if un.live && un.trunc && !frozen.contains(&u) {
                            un.trunc = false;
                            stack.push(u);
                            if queued.insert(u) {
                                wl.try_grow(WORK_BYTES)?;
                                queue.push_back(u);
                            }
                        }
                    }
                }
            }
            for &u in self.users_of(cx, pass, i, &mut memo)? {
                if queued.insert(u) {
                    wl.try_grow(WORK_BYTES)?;
                    queue.push_back(u);
                }
            }
        }
        for i in gone {
            if !cx.c.graph.contains(&self.nodes[i as usize].t) {
                self.remove_node(i)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Brute-force oracle tests: the minimal supports of every fact, found by running the
    //! rules over every subset of the explicit facts, against the witnesses a build and a
    //! history of updates keep.

    use super::*;
    use crate::facts::Base;
    use crate::graph::Graph;
    use crate::incremental::tests::{
        OWL, RDF, RDFS, Rng, Vocab, compiled, full, limits, set_default_graph,
    };
    use crate::incremental::{Fallback, update};
    use crate::terms::Terms;

    /// The number of random cases, `SPARKLES_WITNESS_CASES` for longer runs.
    fn cases(default: u64) -> u64 {
        std::env::var("SPARKLES_WITNESS_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default)
    }

    fn seed0() -> u64 {
        std::env::var("SPARKLES_WITNESS_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1)
    }

    /// A random fact over three names of each kind, so that few facts interact.
    fn small(v: &Vocab, r: &mut Rng, owl: bool) -> Triple {
        let rdf = |l| v.id(RDF, l);
        let rdfs = |l| v.id(RDFS, l);
        let o = |l| v.id(OWL, l);
        let (c, p, i) = (&v.classes[..3], &v.props[..3], &v.inds[..3]);
        match r.below(if owl { 14 } else { 8 }) {
            0 => [r.pick(c), rdfs("subClassOf"), r.pick(c)],
            1 => [r.pick(p), rdfs("subPropertyOf"), r.pick(p)],
            2 => [r.pick(p), rdfs("domain"), r.pick(c)],
            3 => [r.pick(p), rdfs("range"), r.pick(c)],
            4 => [r.pick(i), rdf("type"), r.pick(c)],
            5..=7 => [r.pick(i), r.pick(p), r.pick(i)],
            8 => [r.pick(i), o("sameAs"), r.pick(i)],
            9 => [r.pick(p), o("inverseOf"), r.pick(p)],
            10 => [
                r.pick(p),
                rdf("type"),
                r.pick(&[o("TransitiveProperty"), o("SymmetricProperty")]),
            ],
            11 => [r.pick(c), o("equivalentClass"), r.pick(c)],
            12 => [r.pick(p), o("equivalentProperty"), r.pick(p)],
            _ => [r.pick(i), r.pick(&p[..1]), r.pick(i)],
        }
    }

    /// A closure of `base` built with the closure operators.
    fn full_operators(v: &Vocab, text: &str, base: &FxHashSet<Triple>) -> Closure {
        let snap = set_default_graph(v, base);
        let terms = Terms::new(snap.clone());
        let rules = compiled(text, &terms);
        let mut lim = limits();
        lim.closures = true;
        let b = Arc::new(Base::new(snap, &[Id::DEFAULT_GRAPH]).unwrap());
        let mut g = Graph::with_base(b.clone());
        if let Err(e) = engine::run(&mut g, &rules, &terms, &lim, None) {
            assert!(e.is::<crate::closure::Restart>(), "{e:#}");
            g = Graph::with_base(b);
            engine::run(&mut g, &rules, &terms, &limits(), None).unwrap();
        }
        Closure::new(g, terms).unwrap()
    }

    /// The minimal supports of every fact of the closure of `base`, by brute force.
    struct Oracle {
        /// the explicit facts, by bit
        keys: FxHashMap<FactKey, u32>,
        /// per subset of the explicit facts: its closure, as text
        closures: Vec<FxHashSet<String>>,
        /// per fact, as text: the subsets that minimally derive it
        minimal: FxHashMap<String, Vec<u32>>,
    }

    fn text(terms: &Terms, t: &Triple) -> String {
        let s = |x: u64| terms.term(x).map_or(format!("?{x:x}"), |t| t.to_string());
        format!("{} {} {}", s(t[0]), s(t[1]), s(t[2]))
    }

    fn oracle(v: &Vocab, rule_text: &str, base: &[Triple]) -> Oracle {
        let terms = Terms::new(v.store.snapshot());
        let rules = compiled(rule_text, &terms);
        let n = base.len();
        assert!(n <= 12);
        let key = |x: u64| terms.key_of(x).unwrap();
        let keys = base
            .iter()
            .enumerate()
            .map(|(i, t)| {
                (
                    FactKey::of_keys(&key(t[0]), &key(t[1]), &key(t[2]), None),
                    i as u32,
                )
            })
            .collect();
        let mut closures = Vec::with_capacity(1 << n);
        for mask in 0u32..(1 << n) {
            let mut g = Graph::with_capacity(16);
            g.add_batch((0..n).filter(|i| mask & (1 << i) != 0).map(|i| base[i]));
            engine::run(&mut g, &rules, &terms, &limits(), None).unwrap();
            let set: FxHashSet<String> = (0..g.len())
                .filter(|&r| g.alive(r))
                .map(|r| text(&terms, &g.triple(r)))
                .collect();
            closures.push(set);
        }
        let mut masks: Vec<u32> = (0..(1u32 << n)).collect();
        masks.sort_by_key(|m| m.count_ones());
        let mut minimal: FxHashMap<String, Vec<u32>> = FxHashMap::default();
        for m in masks {
            for f in &closures[m as usize] {
                let e = minimal.entry(f.clone()).or_default();
                if !e.iter().any(|&x| x & !m == 0) {
                    e.push(m);
                }
            }
        }
        Oracle {
            keys,
            closures,
            minimal,
        }
    }

    /// The facts of both tiers, by their text.
    fn facts(c: &Closure) -> FxHashMap<String, Triple> {
        let mut out: FxHashMap<String, Triple> =
            c.live().map(|(t, _)| (text(&c.terms, &t), t)).collect();
        c.graph
            .base()
            .unwrap()
            .for_each(None, None, None, |t| {
                out.insert(text(&c.terms, &t), t);
                true
            })
            .unwrap();
        out
    }

    fn mask(o: &Oracle, w: &Witness, what: &str) -> u32 {
        w.atoms()
            .iter()
            .map(|a| match a {
                Atom::Fact(k) => {
                    1 << o.keys.get(k).unwrap_or_else(|| {
                        panic!("{what}: a witness names {k:?}, not an explicit fact")
                    })
                }
                Atom::Absent(_) => panic!("{what}: a negative condition"),
            })
            .fold(0, |a, b| a | b)
    }

    /// The kept supports and the truncation of `t`.
    fn kept(c: &Closure, w: &Witnesses, t: &Triple) -> Option<(Vec<Witness>, bool)> {
        match w.node(t) {
            Some(i) => {
                let n = &w.nodes[i as usize];
                Some((n.w.clone(), n.trunc))
            }
            None => {
                let a = w.report_of(c, t)?;
                Some((a.witnesses, a.truncated))
            }
        }
    }

    /// Check the two rules for every fact; returns the facts that are exact and those
    /// that are truncated.
    fn check(o: &Oracle, c: &Closure, w: &Witnesses, what: &str) -> (usize, usize) {
        let all = o.closures.last().unwrap();
        let have = facts(c);
        let names: FxHashSet<&String> = have.keys().collect();
        assert_eq!(
            names,
            all.iter().collect::<FxHashSet<_>>(),
            "{what}: the closure differs from the oracle's"
        );
        let (mut exact, mut truncated) = (0, 0);
        for (f, t) in &have {
            let (ws, trunc) =
                kept(c, w, t).unwrap_or_else(|| panic!("{what}: no witnesses for {f}"));
            assert!(
                ws.len() <= w.max,
                "{what}: {f} keeps {} witnesses",
                ws.len()
            );
            let masks: Vec<u32> = ws.iter().map(|x| mask(o, x, what)).collect();
            // rule 1: every kept support derives the fact
            for (x, m) in ws.iter().zip(&masks) {
                assert!(
                    o.closures[*m as usize].contains(f),
                    "{what}: {f} keeps {x:?}, which does not derive it"
                );
            }
            // the kept supports form an antichain
            for (a, x) in masks.iter().enumerate() {
                for (b, y) in masks.iter().enumerate() {
                    assert!(
                        a == b || x & y != *x,
                        "{what}: {f} keeps a support and a superset"
                    );
                }
            }
            if trunc {
                truncated += 1;
                continue;
            }
            // rule 2: a fact that is not truncated keeps exactly its minimal supports
            let mut want = o.minimal[f].clone();
            want.sort_unstable();
            let mut got = masks.clone();
            got.sort_unstable();
            assert_eq!(got, want, "{what}: {f} is exact with other supports");
            exact += 1;
        }
        (exact, truncated)
    }

    /// Facts that a fresh build of `c` keeps exactly are kept exactly, with the same
    /// supports, by `w`.
    fn converged(c: &mut Closure, rules: &[CRule], w: &Witnesses, what: &str) {
        let fresh = Witnesses::build(c, rules, w.max, &limits()).unwrap();
        for (f, t) in facts(c) {
            let (a, ta) = kept(c, &fresh, &t).unwrap();
            if ta {
                continue;
            }
            let (b, tb) = kept(c, w, &t).unwrap();
            assert!(
                !tb,
                "{what}: {f} is exact in a fresh build and truncated after the update"
            );
            assert_eq!(a, b, "{what}: {f}");
        }
    }

    fn base_of(v: &Vocab, r: &mut Rng, owl: bool, n: usize) -> FxHashSet<Triple> {
        let mut base = FxHashSet::default();
        while base.len() < n {
            base.insert(small(v, r, owl));
        }
        base
    }

    fn sorted(b: &FxHashSet<Triple>) -> Vec<Triple> {
        let mut v: Vec<Triple> = b.iter().copied().collect();
        v.sort_unstable();
        v
    }

    /// A build over random small ontologies, checked against the oracle.
    fn builds(
        text_rules: &str,
        owl: bool,
        max: usize,
        operators: bool,
        cases: u64,
    ) -> (usize, usize) {
        let v = Vocab::new();
        let (mut exact, mut truncated) = (0, 0);
        for seed in seed0()..seed0() + cases {
            let mut r = Rng(seed * 7919 + max as u64);
            let n = 3 + r.below(6);
            let base = base_of(&v, &mut r, owl, n);
            let mut c = if operators {
                full_operators(&v, text_rules, &base)
            } else {
                full(&v, text_rules, &base)
            };
            let rules = compiled(text_rules, &c.terms);
            let w = Witnesses::build(&mut c, &rules, max, &limits()).unwrap();
            let o = oracle(&v, text_rules, &sorted(&base));
            let (e, t) = check(&o, &c, &w, &format!("seed {seed} max {max}"));
            exact += e;
            truncated += t;
        }
        (exact, truncated)
    }

    #[test]
    fn rdfs_builds_match_the_oracle() {
        let (exact, _) = builds(crate::RDFS_RULES, false, 256, false, cases(12));
        assert!(exact > 0);
    }

    #[test]
    fn rdfs_builds_without_truncation_are_exact() {
        // with a bound above every fact's minimal supports nothing is truncated
        let (_, truncated) = builds(crate::RDFS_SIMPLE_RULES, false, 256, false, cases(12));
        assert_eq!(truncated, 0);
    }

    #[test]
    fn owl_rl_builds_match_the_oracle_with_closure_operators() {
        let (exact, _) = builds(crate::OWL_RL_RULES, true, 8, true, cases(8));
        assert!(exact > 0);
    }

    #[test]
    fn truncated_builds_keep_real_witnesses() {
        for max in [1, 2] {
            let (_, truncated) = builds(crate::RDFS_RULES, false, max, false, cases(8));
            assert!(truncated > 0, "max {max}: nothing was truncated");
            builds(crate::OWL_RL_RULES, true, max, true, cases(4));
        }
    }

    /// Bring `c` and `w` to the explicit facts `base`, reached by `inserted` and
    /// `removed`, as an incremental run does.
    #[allow(clippy::too_many_arguments)]
    fn step(
        v: &Vocab,
        text_rules: &str,
        max: usize,
        c: &mut Closure,
        w: &mut Witnesses,
        base: &FxHashSet<Triple>,
        inserted: &[Triple],
        removed: &[Triple],
    ) {
        let snap = set_default_graph(v, base);
        let moved = c.terms.rebase(snap.clone());
        c.remap(&moved).unwrap();
        w.remap(&moved);
        if let Some(mut l) = c.set_inputs(snap.clone(), &[Id::DEFAULT_GRAPH]).unwrap() {
            l.update(inserted, removed, crate::facts::rank);
            c.put_lookups(l);
        }
        let rules = compiled(text_rules, &c.terms);
        let mut prep = None;
        let res = {
            let mut hook = |cl: &mut Closure| -> anyhow::Result<()> {
                prep = Some(w.prepare(cl, &rules, &limits(), removed, &[])?);
                Ok(())
            };
            update(c, &rules, &limits(), inserted, removed, Some(&mut hook))
        };
        match res {
            Ok(up) => {
                let added: Vec<Triple> = up.added(c).collect();
                let ok = w
                    .update(
                        c,
                        &rules,
                        &limits(),
                        prep.take().expect("the hook ran"),
                        inserted,
                        removed,
                        &up.deleted,
                        &added,
                        &[],
                    )
                    .unwrap();
                if !ok {
                    *w = Witnesses::build(c, &rules, max, &limits()).unwrap();
                }
            }
            Err(e) => {
                assert!(e.downcast_ref::<Fallback>().is_some(), "{e:#}");
                *c = full(v, text_rules, base);
                let rules = compiled(text_rules, &c.terms);
                *w = Witnesses::build(c, &rules, max, &limits()).unwrap();
            }
        }
    }

    /// Random insertions and deletions, each followed by an update of the closure and
    /// its witnesses, checked against the oracle and against a fresh build.
    fn history(text_rules: &str, owl: bool, max: usize, seed: u64, steps: usize) {
        let v = Vocab::new();
        let mut r = Rng(seed * 104_729 + max as u64);
        let n = 3 + r.below(5);
        let mut base = base_of(&v, &mut r, owl, n);
        let mut c = full(&v, text_rules, &base);
        let rules = compiled(text_rules, &c.terms);
        let mut w = Witnesses::build(&mut c, &rules, max, &limits()).unwrap();
        for k in 0..steps {
            let what = format!("seed {seed} max {max} step {k}");
            let (mut inserted, mut removed) = (Vec::new(), Vec::new());
            let current = sorted(&base);
            for _ in 0..r.below(3) {
                if current.is_empty() {
                    break;
                }
                let t = r.pick(&current);
                if base.remove(&t) {
                    removed.push(t);
                }
            }
            for _ in 0..r.below(3) {
                if base.len() >= 9 {
                    break;
                }
                let t = small(&v, &mut r, owl);
                if base.insert(t) {
                    inserted.push(t);
                }
            }
            step(
                &v, text_rules, max, &mut c, &mut w, &base, &inserted, &removed,
            );
            let o = oracle(&v, text_rules, &sorted(&base));
            check(&o, &c, &w, &what);
            let rules = compiled(text_rules, &c.terms);
            converged(&mut c, &rules, &w, &what);
            c.compact_if_needed(false).unwrap();
        }
    }

    #[test]
    fn rdfs_histories_match_the_oracle() {
        for seed in seed0()..seed0() + cases(6) {
            for max in [8, 2, 1] {
                history(crate::RDFS_SIMPLE_RULES, false, max, seed, 6);
            }
        }
    }

    #[test]
    fn full_rdfs_histories_match_the_oracle() {
        for seed in seed0()..seed0() + cases(3) {
            history(crate::RDFS_RULES, false, 8, seed, 5);
            history(crate::RDFS_RULES, false, 1, seed, 5);
        }
    }

    #[test]
    fn owl_rl_histories_match_the_oracle() {
        for seed in seed0()..seed0() + cases(3) {
            history(crate::OWL_RL_RULES, true, 8, seed, 5);
            history(crate::OWL_RL_RULES, true, 2, seed, 5);
        }
    }

    /// The facts of `v` named by local name, for the hand-made cases.
    fn ex(v: &Vocab, l: &str) -> u64 {
        v.id(crate::incremental::tests::EX, l)
    }

    /// Two derivations of one fact under a bound of 1: the fact is truncated, keeps the
    /// smaller support, and is exact again once either support is removed, also when the
    /// removed fact is only in the support it did not keep.
    #[test]
    fn a_truncated_fact_converges_after_a_deletion() {
        let v = Vocab::new();
        let sub = v.id(RDFS, "subClassOf");
        let ty = v.id(RDF, "type");
        let (c0, c1, c2) = (ex(&v, "C0"), ex(&v, "C1"), ex(&v, "C2"));
        let i0 = ex(&v, "i0");
        let rules_text = crate::RDFS_SIMPLE_RULES;
        for drop_kept in [false, true] {
            // i0 a C2 follows from i0 a C0 with C0 ⊑ C2, and from i0 a C1 with C1 ⊑ C2
            let facts = [[i0, ty, c0], [c0, sub, c2], [i0, ty, c1], [c1, sub, c2]];
            let mut base: FxHashSet<Triple> = facts.iter().copied().collect();
            let mut c = full(&v, rules_text, &base);
            let rules = compiled(rules_text, &c.terms);
            let mut w = Witnesses::build(&mut c, &rules, 1, &limits()).unwrap();
            let target = [i0, ty, c2];
            let (ws, trunc) = kept(&c, &w, &target).unwrap();
            assert!(trunc && ws.len() == 1);
            // remove a fact of the kept support, or of the one that was dropped
            let kept_keys: FxHashSet<FactKey> = ws[0]
                .atoms()
                .iter()
                .filter_map(|a| match a {
                    Atom::Fact(k) => Some(*k),
                    _ => None,
                })
                .collect();
            let gone = *facts
                .iter()
                .find(|t| kept_keys.contains(&fact_key(&c, t)) == drop_kept && t[1] == sub)
                .unwrap();
            base.remove(&gone);
            let snap = set_default_graph(&v, &base);
            let moved = c.terms.rebase(snap.clone());
            c.remap(&moved).unwrap();
            w.remap(&moved);
            let _ = c.set_inputs(snap, &[Id::DEFAULT_GRAPH]).unwrap();
            let rules = compiled(rules_text, &c.terms);
            let mut prep = None;
            let up = {
                let mut hook = |cl: &mut Closure| -> anyhow::Result<()> {
                    prep = Some(w.prepare(cl, &rules, &limits(), &[gone], &[])?);
                    Ok(())
                };
                update(&mut c, &rules, &limits(), &[], &[gone], Some(&mut hook)).unwrap()
            };
            let added: Vec<Triple> = up.added(&c).collect();
            assert!(
                w.update(
                    &mut c,
                    &rules,
                    &limits(),
                    prep.unwrap(),
                    &[],
                    &[gone],
                    &up.deleted,
                    &added,
                    &[]
                )
                .unwrap()
            );
            let (ws, trunc) = kept(&c, &w, &target).unwrap();
            assert!(!trunc, "drop_kept {drop_kept}: still truncated");
            assert_eq!(ws.len(), 1);
            assert!(
                ws[0]
                    .atoms()
                    .iter()
                    .all(|a| matches!(a, Atom::Fact(k) if *k != fact_key(&c, &gone)))
            );
        }
    }

    /// A transitive chain closed by the closure operator: each fact's witness is its path
    /// with the property's declaration.
    #[test]
    fn closure_operator_facts_have_path_witnesses() {
        let v = Vocab::new();
        let ty = v.id(RDF, "type");
        let trans = v.id(OWL, "TransitiveProperty");
        let p = v.props[0];
        let i = &v.inds;
        let mut base: FxHashSet<Triple> = FxHashSet::default();
        base.insert([p, ty, trans]);
        for k in 0..4 {
            base.insert([i[k], p, i[k + 1]]);
        }
        let mut c = full_operators(&v, crate::OWL_RL_RULES, &base);
        let rules = compiled(crate::OWL_RL_RULES, &c.terms);
        let w = Witnesses::build(&mut c, &rules, 8, &limits()).unwrap();
        let (ws, trunc) = kept(&c, &w, &[i[0], p, i[4]]).unwrap();
        assert!(!trunc);
        let mut want: Vec<Atom> = (0..4)
            .map(|k| Atom::Fact(fact_key(&c, &[i[k], p, i[k + 1]])))
            .chain([Atom::Fact(fact_key(&c, &[p, ty, trans]))])
            .collect();
        want.sort_unstable();
        assert_eq!(ws, vec![Witness::new(want)]);
        let o = oracle(&v, crate::OWL_RL_RULES, &sorted(&base));
        check(&o, &c, &w, "chain");
    }

    /// Regression: with a bound of 1, `p1 rdf:type rdf:Property` follows from
    /// `i0 p1 i0` and from the asserted `p1 rdfs:subPropertyOf p1`, which it derives in
    /// turn, so the two facts and `p1 rdf:type rdfs:Resource` form a cycle of truncated
    /// facts. Removing the asserted fact, which kept the other support and not its own,
    /// leaves one support each. The removed fact must be evaluated again with its
    /// truncation reset, and the reset must reach the whole cycle.
    #[test]
    fn removing_a_truncated_fact_resets_its_cycle() {
        let v = Vocab::new();
        let spo = v.id(RDFS, "subPropertyOf");
        let (p1, i0) = (ex(&v, "p1"), ex(&v, "i0"));
        let text_rules = crate::RDFS_RULES;
        let gone = [p1, spo, p1];
        let mut base: FxHashSet<Triple> = [[i0, p1, i0], gone].into_iter().collect();
        let mut c = full(&v, text_rules, &base);
        let rules = compiled(text_rules, &c.terms);
        let mut w = Witnesses::build(&mut c, &rules, 1, &limits()).unwrap();
        let ty = v.id(RDF, "type");
        let prop = v.id(RDF, "Property");
        assert!(kept(&c, &w, &[p1, ty, prop]).unwrap().1);
        base.remove(&gone);
        step(&v, text_rules, 1, &mut c, &mut w, &base, &[], &[gone]);
        let o = oracle(&v, text_rules, &sorted(&base));
        check(&o, &c, &w, "after the removal");
        assert!(!kept(&c, &w, &[p1, ty, prop]).unwrap().1);
        let rules = compiled(text_rules, &c.terms);
        converged(&mut c, &rules, &w, "after the removal");
    }

    fn singleton(n: u64) -> Witness {
        let k = FactKey::of_terms(
            &oxrdf::NamedNode::new_unchecked(format!("http://ex.org/s{n}")).into(),
            &oxrdf::NamedNode::new_unchecked("http://ex.org/p").into(),
            &oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked("http://ex.org/o")),
            None,
        );
        Witness::new(vec![Atom::Fact(k)])
    }

    /// Regression (review of item 416): at the largest bound, two premises of 256
    /// incomparable supports each formed all 65,536 unions in one antichain before the
    /// product was cut, billions of subset checks without a check for cancellation. The
    /// working antichain and the unions per premise are bounded, and the cut says so.
    #[test]
    fn products_of_large_premises_are_bounded() {
        let mut a: Vec<Witness> = (0..256).map(singleton).collect();
        let mut b: Vec<Witness> = (256..512).map(singleton).collect();
        a.sort();
        b.sort();
        let cap = pool_cap(MAX_WITNESSES_LIMIT);
        let t0 = Instant::now();
        let budget = Budget::new(None);
        let mut tmp = Held::new(&budget);
        let (r, over) = product(&[&a, &b], cap, &|| false, &mut tmp).unwrap();
        assert!(over);
        assert!(r.len() <= cap, "{}", r.len());
        assert!(r.iter().all(|x| x.len() == 2));
        assert!(t0.elapsed().as_secs() < 20, "{:?}", t0.elapsed());
        // the result's heap stays charged
        assert_eq!(tmp.bytes(), witnesses_bytes(&r));
        assert!(budget.peak() >= witnesses_bytes(&r));
        drop(tmp);
        // a cancelled run stops inside the product
        let mut tmp = Held::new(&budget);
        let e = product(&[&a, &b], cap, &|| true, &mut tmp).unwrap_err();
        assert!(e.to_string().contains("cancelled"), "{e:#}");
        // small products are exact and unchanged
        let (r, over) = product(&[&a[..4], &b[..4]], cap, &|| false, &mut tmp).unwrap();
        assert!(!over);
        assert_eq!(r.len(), 16);
    }

    /// Regression (third review of item 416): the working antichains of a product were
    /// not charged, so long supports could pass the limit unseen. A product that does not
    /// fit stops with the memory error.
    #[test]
    fn products_are_charged_while_they_are_built() {
        let long = |from: u64| {
            let atoms: Vec<Atom> = (from..from + 50).map(|n| singleton(n).atoms()[0]).collect();
            Witness::new(atoms)
        };
        let mut a: Vec<Witness> = (0..16).map(|k| long(k * 100)).collect();
        let mut b: Vec<Witness> = (0..16).map(|k| long(10_000 + k * 100)).collect();
        a.sort();
        b.sort();
        let budget = Budget::new(Some(64 * 1024));
        let mut tmp = Held::new(&budget);
        let e = product(&[&a, &b], 256, &|| false, &mut tmp).unwrap_err();
        assert!(BudgetFailure::from_anyhow(&e).is_some(), "{e:#}");
        let budget = Budget::new(None);
        let mut tmp = Held::new(&budget);
        let (r, over) = product(&[&a, &b], 256, &|| false, &mut tmp).unwrap();
        assert!(!over);
        assert_eq!(r.len(), 256);
        assert!(
            budget.peak() >= 256 * 100 * size_of::<Atom>(),
            "{}",
            budget.peak()
        );
    }

    /// Regression (review of item 416): the witnesses were charged to the budget only
    /// after a build or an update had allocated them. Each node and each change of kept
    /// supports is now charged as it happens.
    #[test]
    fn witness_state_is_charged_as_it_grows() {
        let budget = Budget::new(None);
        let mut w = Witnesses::empty(8, &budget);
        let before = budget.used();
        let mut held = 0;
        for n in 0..1000u64 {
            let one = singleton(n);
            let Atom::Fact(k) = &one.atoms()[0] else {
                unreachable!()
            };
            let k = *k;
            let i = w.add_node([n, 1, 2], Some(k)).unwrap();
            let ws: Vec<Witness> = (0..8).map(|k| singleton(n * 8 + k)).collect();
            w.set(i, ws, false, false).unwrap();
            held = w.held.bytes();
            // regression (third review): the charge is the heap the final settlement
            // computes, so that refunds match it
            assert_eq!(held, w.heap_bytes());
        }
        assert!(
            held >= 1000 * (size_of::<Node>() + 8 * size_of::<Witness>()),
            "{held}"
        );
        assert!(budget.used() >= before + held, "{} {held}", budget.used());
        for n in 0..1000u64 {
            let i = w.node(&[n, 1, 2]).unwrap();
            w.remove_node(i).unwrap();
            assert_eq!(w.held.bytes(), w.heap_bytes());
        }
        w.compact_rev().unwrap();
        assert_eq!(w.held.bytes(), w.heap_bytes());
        assert!(w.held.bytes() < held / 2, "{}", w.held.bytes());
        assert_eq!(w.rev_entries, 0);
    }

    /// Regression (third review of item 416): bodies that repeat under other bindings
    /// were charged and then dropped by the deduplication without a refund.
    #[test]
    fn repeated_instances_give_their_charge_back() {
        let v = Vocab::new();
        let (p1, i0) = (ex(&v, "p1"), ex(&v, "i0"));
        let e = crate::incremental::tests::EX;
        let text = format!("[r: (?s <{e}p0> ?a) (?s <{e}p0> ?b) -> (<{e}i0> <{e}p1> <{e}i0>)]");
        let p0 = ex(&v, "p0");
        let base: FxHashSet<Triple> = (0..4)
            .map(|b| [ex(&v, "i1"), p0, ex(&v, &format!("i{b}"))])
            .collect();
        let mut c = full(&v, &text, &base);
        let rules = compiled(&text, &c.terms);
        let plans = Plans::new(&mut c.graph, &rules).unwrap();
        let abort = AtomicBool::new(false);
        let l = limits();
        let cx = Cx::new(&c, &rules, &plans, &abort, &l).unwrap();
        let mut net = 0isize;
        let mut gross = 0isize;
        let f = cx
            .instances([i0, p1, i0], &mut Memo::default(), &mut |n| {
                net += n;
                gross += n.max(0);
                Ok(())
            })
            .unwrap();
        // 16 bindings, of which the 4 with a = b have one premise, and the 12 others
        // repeat 6 bodies of two premises
        assert_eq!(f.bodies.len(), 10);
        let kept: usize = f.bodies.iter().map(|b| body_bytes(b.len())).sum();
        assert_eq!(net as usize, kept);
        assert!(gross > net, "{gross} {net}");
    }

    /// Regression (second review of item 416): a fact's instances were charged only
    /// after the search had kept every body, so a fact with many large instances could
    /// pass the limit by far before the run refused it. Each body is now charged before it
    /// is kept, and the search stops at the first refusal.
    #[test]
    fn instances_are_charged_as_they_are_found() {
        let v = Vocab::new();
        let (p0, p1, i0) = (ex(&v, "p0"), ex(&v, "p1"), ex(&v, "i0"));
        let text = format!(
            "[r: (?s <{e}p0> ?o) -> (<{e}i0> <{e}p1> <{e}i0>)]",
            e = crate::incremental::tests::EX
        );
        let base: FxHashSet<Triple> = (0..8)
            .flat_map(|a| (0..8).map(move |b| (a, b)))
            .map(|(a, b)| [ex(&v, &format!("i{a}")), p0, ex(&v, &format!("i{b}"))])
            .collect();
        let mut c = full(&v, &text, &base);
        let rules = compiled(&text, &c.terms);
        let plans = Plans::new(&mut c.graph, &rules).unwrap();
        let abort = AtomicBool::new(false);
        let l = limits();
        let cx = Cx::new(&c, &rules, &plans, &abort, &l).unwrap();
        let head = [i0, p1, i0];
        let mut all = 0isize;
        let f = cx
            .instances(head, &mut Memo::default(), &mut |n| {
                all += n;
                Ok(())
            })
            .unwrap();
        assert_eq!(f.bodies.len(), 64);
        let one = body_bytes(1);
        assert_eq!(all as usize, 64 * one);
        let budget = Budget::new(Some(10 * one + one / 2));
        let mut held = Held::new(&budget);
        let mut charged = 0usize;
        let Err(e) = cx.instances(head, &mut Memo::default(), &mut |n| {
            held.try_grow(n as usize)?;
            charged += n as usize;
            Ok(())
        }) else {
            panic!("the search passed the limit");
        };
        assert!(BudgetFailure::from_anyhow(&e).is_some(), "{e:#}");
        assert!(charged <= 10 * one, "{charged}");
    }

    /// Regression (fifth review of item 416): the search before a deletion kept its sets,
    /// queue and region without a charge. The prepared state now holds the charge of
    /// every fact it reached until the update consumes it.
    #[test]
    fn the_search_before_a_deletion_is_charged() {
        let v = Vocab::new();
        let sub = v.id(RDFS, "subClassOf");
        let ty = v.id(RDF, "type");
        let cs: Vec<u64> = (0..6).map(|k| ex(&v, &format!("C{k}"))).collect();
        let mut base: FxHashSet<Triple> = cs.windows(2).map(|w| [w[0], sub, w[1]]).collect();
        for k in 0..8 {
            base.insert([ex(&v, &format!("i{k}")), ty, cs[0]]);
        }
        let text = crate::RDFS_RULES;
        let mut c = full(&v, text, &base);
        let rules = compiled(text, &c.terms);
        let mut w = Witnesses::build(&mut c, &rules, 8, &limits()).unwrap();
        let budget = Budget::new(None);
        let l = Limits {
            budget: budget.clone(),
            ..limits()
        };
        let gone = [cs[0], sub, cs[1]];
        let prep = w.prepare(&mut c, &rules, &l, &[gone], &[]).unwrap();
        assert!(!prep.region.is_empty());
        let reached = prep.held.as_ref().map_or(0, Held::bytes);
        // every fact of the region was visited, those it started from included (sixth
        // review)
        assert!(
            reached >= prep.region.len() * (REACHED_BYTES + VISITED_BYTES),
            "{reached} {}",
            prep.region.len()
        );
        assert!(budget.used() >= reached);
        drop(prep);
        assert_eq!(budget.used(), 0);
    }

    /// Regression (fifth review of item 416): compacting the reverse index built the new
    /// index before charging it, beside the old one. Both are charged during the
    /// compaction.
    #[test]
    fn compaction_charges_both_indexes() {
        let budget = Budget::new(None);
        let mut w = Witnesses::empty(8, &budget);
        for n in 0..1000u64 {
            let i = w.add_node([n, 1, 2], None).unwrap();
            let ws: Vec<Witness> = (0..4).map(|k| singleton(n * 4 + k)).collect();
            w.set(i, ws, false, false).unwrap();
        }
        // the supports change, so that the index holds stale entries
        for n in 0..1000u64 {
            let i = w.node(&[n, 1, 2]).unwrap();
            w.set(i, vec![singleton(10_000 + n)], false, false).unwrap();
        }
        // a fresh budget, whose peak is the charge from here on
        let budget = Budget::new(None);
        w.set_budget(&budget).unwrap();
        let before = budget.used();
        assert_eq!(budget.peak(), before);
        w.compact_rev().unwrap();
        assert!(w.rev_entries == 1000, "{}", w.rev_entries);
        // the lists' capacity is charged, not only their entries (sixth review)
        assert!(w.rev_cap >= 4 * 1000, "{}", w.rev_cap);
        assert!(budget.used() < before);
        assert!(
            budget.peak() >= before + 1000 * 4,
            "{} {before}",
            budget.peak()
        );
        assert_eq!(w.held.bytes(), w.heap_bytes());
    }

    /// Regression (eighth review of item 416): the search before a deletion collected
    /// every moved or removed fact before its first charge. Each is charged as it is
    /// collected, so a large batch under a small budget is refused at once.
    #[test]
    fn a_large_deletion_is_refused_as_it_is_collected() {
        let v = Vocab::new();
        let sub = v.id(RDFS, "subClassOf");
        let ty = v.id(RDF, "type");
        let mut base: FxHashSet<Triple> = FxHashSet::default();
        for k in 0..8 {
            for c in 0..6 {
                base.insert([ex(&v, &format!("i{k}")), ty, ex(&v, &format!("C{c}"))]);
            }
        }
        base.insert([ex(&v, "C0"), sub, ex(&v, "C1")]);
        let text = crate::RDFS_RULES;
        let mut c = full(&v, text, &base);
        let rules = compiled(text, &c.terms);
        let mut w = Witnesses::build(&mut c, &rules, 8, &limits()).unwrap();
        let removed: Vec<Triple> = base.iter().copied().collect();
        let one = REACHED_BYTES + VISITED_BYTES + size_of::<FactKey>() + 8 + 4;
        let budget = Budget::new(Some(4 * one));
        let l = Limits {
            budget: budget.clone(),
            ..limits()
        };
        let Err(e) = w.prepare(&mut c, &rules, &l, &removed, &[]) else {
            panic!("the batch passed the limit");
        };
        assert!(BudgetFailure::from_anyhow(&e).is_some(), "{e:#}");
        assert!(budget.peak() <= 4 * one, "{}", budget.peak());
        assert_eq!(budget.used(), 0);
        // the collection stopped after the facts that fit: the others keep their keys
        let taken = removed
            .iter()
            .filter(|t| w.node(t).is_some_and(|i| w.nodes[i as usize].key.is_none()))
            .count();
        assert!(taken <= 4, "{taken} of {}", removed.len());
        // and so does a batch of facts that move to another graph's quad: each changed
        // key is charged before it is taken
        let mut w = Witnesses::build(&mut c, &rules, 8, &limits()).unwrap();
        let fake = |t: &Triple| FactKey::of_keys(&t[0].to_le_bytes(), b"moved", b"", None);
        for t in &removed {
            let i = w.node(t).unwrap();
            w.nodes[i as usize].key = Some(fake(t));
        }
        let budget = Budget::new(Some(4 * one));
        let l = Limits {
            budget: budget.clone(),
            ..limits()
        };
        let Err(e) = w.prepare(&mut c, &rules, &l, &[], &removed) else {
            panic!("the batch passed the limit");
        };
        assert!(BudgetFailure::from_anyhow(&e).is_some(), "{e:#}");
        let rekeyed = removed
            .iter()
            .filter(|t| w.nodes[w.node(t).unwrap() as usize].key != Some(fake(t)))
            .count();
        assert!(rekeyed <= 4, "{rekeyed} of {}", removed.len());
        assert_eq!(budget.used(), 0);
    }
}
