//! The rules of roadmap item 603's Tier D for ALCHI: Hyper, Succ and Pred of the calculus of
//! Bate, Motik, Cuenca Grau, Tena Cucala, Simančík and Horrocks (JAIR 2018, Table 2), over
//! the saturation's contexts, beside the EL rules.
//!
//! A context's units `→ A(x)` are its subsumers, and the EL rules are Hyper, Succ and Pred
//! on units: told subsumers, conjunctions and disjointness are Hyper with Horn clauses, an
//! existential restriction is Hyper and Succ with the cautious strategy, whose successor is
//! the context of its filler with the role's range, and the propagation of negative
//! existential restrictions over backward links is Hyper and Pred. The rules here are the
//! rest: Hyper on the clauses that are not units and with the clauses that have two or more
//! disjuncts, the universal restrictions over both the successors and the predecessor,
//! Succ for the literals over successor terms, with the markers `A → A` of the atoms that
//! may hold of a successor, and Pred of the clauses over the predecessor.
//!
//! A context's store holds its clauses that are not units, and is allocated with the first
//! one. Its units serve as premises in place, so the store needs no copy of them. An
//! existential restriction e that a context holds as a unit has the EL rules' successor,
//! and the context sends it the markers of the atoms its clauses give `f_e(x)`. A context
//! whose clause has e only conditionally registers with e's hypothetical successor `h_e`,
//! a context with the empty core, and sends it every marker it needs for e. Once it has,
//! `h_e` takes every later marker for e too, so that one edge for `f_e` holds every atom
//! that may hold of the successor, which the Succ rule's condition asks. A context answers
//! its predecessors, those of its backward links and those that registered, with the
//! clauses whose head is over the predecessor alone, which each predecessor resolves with
//! its own clauses over `f_e(x)`.
//!
//! Every rule reads only its own context's state and sends conclusions to itself, to the
//! successors it registered with or to the predecessors it holds, as the EL rules do.

use super::dl_clause::{Clause, F, Lit, RF, RY, Store, Table, X, Y};
use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use super::saturate::{Concl, Sink, State, flatten, table_bytes};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A clause of the axioms with two or more disjuncts: its body, where an empty one is
/// `owl:Thing`, and its head.
pub type AxClause = (Box<[Ex]>, Box<[Ex]>);

/// What Tier D reads in the index.
#[derive(Default)]
pub struct DlIx {
    /// the clauses with two or more disjuncts, by id: the body, where an empty one is
    /// `owl:Thing`, and the head
    pub clauses: Vec<AxClause>,
    /// for each body atom, the clauses whose body has it
    pub by_body: FxHashMap<Ex, Vec<u32>>,
    /// the disjointness sets, by the index's numbering
    pub sets: Vec<Vec<Ex>>,
    /// the hypothetical successor of each existential restriction, and back
    pub hyp: FxHashMap<Ex, Ex>,
    pub of_hyp: FxHashMap<Ex, Ex>,
    /// the core of each existential restriction's successor: its filler and the role's
    /// range, as the sorted expressions that are not intersections
    cores: FxHashMap<Ex, Box<[Ex]>>,
    /// for each successor context, the existential restrictions whose successor it is
    by_succ: FxHashMap<Ex, Vec<Ex>>,
    /// for each role, whether the role of an existential restriction is below it, so that
    /// a universal on it can meet a successor
    reach: Vec<bool>,
}

impl DlIx {
    pub fn new(dl: &super::dl_norm::DlAx, sets: &[Vec<Ex>], ix: &Index, nodes: usize) -> DlIx {
        let mut d = DlIx {
            sets: sets.to_vec(),
            hyp: dl.hyp.clone(),
            of_hyp: dl.hyp.iter().map(|(&e, &h)| (h, e)).collect(),
            ..DlIx::default()
        };
        for (b, h) in &dl.clauses {
            let id = d.clauses.len() as u32;
            let mut b = b.clone();
            if b.is_empty() {
                b.push(TOP);
            }
            b.sort_unstable();
            b.dedup();
            for &a in &b {
                d.by_body.entry(a).or_default().push(id);
            }
            d.clauses
                .push((b.into_boxed_slice(), h.clone().into_boxed_slice()));
        }
        let mut roles: Vec<Role> = Vec::new();
        for e in 0..nodes as Ex {
            let h = ix.head(e);
            if let Node::Exist(r, _) = h.node {
                let mut core = Vec::new();
                flatten(ix, h.succ, &mut core);
                d.cores.insert(e, core.into_boxed_slice());
                d.by_succ.entry(h.succ).or_default().push(e);
                roles.push(r);
            }
        }
        roles.sort_unstable();
        roles.dedup();
        let n = ix.roles.sup.len();
        d.reach = vec![false; n];
        for &r in &roles {
            for &s in ix
                .roles
                .sup
                .get(r as usize)
                .map_or(&[][..], |v| v.as_slice())
            {
                if let Some(x) = d.reach.get_mut(s as usize) {
                    *x = true;
                }
            }
            if let Some(x) = d.reach.get_mut(r as usize) {
                *x = true;
            }
        }
        d
    }

    pub fn bytes(&self) -> usize {
        self.clauses
            .iter()
            .map(|(b, h)| 40 + (b.len() + h.len()) * 4)
            .sum::<usize>()
            + self
                .by_body
                .values()
                .map(|v| 48 + v.capacity() * 4)
                .sum::<usize>()
            + self
                .sets
                .iter()
                .map(|s| 24 + s.capacity() * 4)
                .sum::<usize>()
            + table_bytes(self.hyp.capacity(), 8) * 2
            + self.cores.values().map(|c| 24 + c.len() * 4).sum::<usize>()
            + self
                .by_succ
                .values()
                .map(|v| 32 + v.capacity() * 4)
                .sum::<usize>()
            + self.reach.len()
    }

    /// Whether a universal on `t` can meet a successor.
    #[inline]
    fn reaches(&self, t: Role) -> bool {
        self.reach.get(t as usize).copied().unwrap_or(false)
    }

    /// Whether `a` is in the core of the successor that the EL rules give e.
    #[inline]
    fn in_core(&self, e: Ex, a: Ex) -> bool {
        a == TOP
            || self
                .cores
                .get(&e)
                .is_some_and(|c| c.binary_search(&a).is_ok())
    }
}

/// What a saturation with Tier D shares: the table of the clauses that contexts send one
/// another and the bytes of the clause stores.
#[derive(Default)]
pub struct DlRun {
    pub table: Table,
    /// whether the Succ rule takes the eager strategy instead of the cautious one
    pub eager: bool,
    stores: AtomicUsize,
    /// the clause with an empty body and head, which an unsatisfiable context sends
    empty: OnceLock<u32>,
}

impl DlRun {
    pub fn new(eager: bool) -> DlRun {
        DlRun {
            eager,
            ..DlRun::default()
        }
    }
    pub fn bytes(&self) -> usize {
        self.table.bytes() + self.stores.load(Ordering::Relaxed)
    }
    fn grow(&self, bytes: usize) {
        self.stores.fetch_add(bytes, Ordering::Relaxed);
    }
    fn empty(&self) -> u32 {
        *self
            .empty
            .get_or_init(|| self.table.push(Clause::new(Vec::new(), Vec::new())))
    }
}

/// What a context keeps for Tier D.
#[derive(Default, Clone)]
pub struct DlSt {
    pub store: Store,
    /// the clauses over the predecessor that successors sent, by successor
    pub pred_in: FxHashMap<Ex, Vec<u32>>,
    pred_seen: FxHashSet<(Ex, u32)>,
    /// the predecessors that registered, with the existential restriction they reach this
    /// context by
    pub dpreds: Vec<(Ex, Ex)>,
    /// the successors this context registered with, by existential restriction: the
    /// hypothetical successor, or under the eager strategy the context of what holds of
    /// the successor for certain
    pub edges: Vec<(Ex, Ex)>,
    /// the markers this context holds
    hyps: FxHashSet<Ex>,
    /// the markers sent, by target
    sent: FxHashSet<(Ex, Ex)>,
    /// the table ids of the clauses `s(y, x) → c(y)` of the propagations, which the
    /// registered predecessors take
    props: FxHashMap<(Role, Ex), u32>,
    /// in a test context, the named classes that do not hold, sorted
    pub neg: Vec<Ex>,
}

impl DlSt {
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<DlSt>()
            + self.store.bytes()
            + self
                .pred_in
                .values()
                .map(|v| 32 + v.capacity() * 4)
                .sum::<usize>()
            + table_bytes(self.pred_seen.capacity(), 8)
            + self.dpreds.capacity() * 8
            + self.edges.capacity() * 8
            + table_bytes(self.hyps.capacity(), 4)
            + table_bytes(self.sent.capacity(), 8)
            + table_bytes(self.props.capacity(), 12)
    }

    /// Whether the context has no clause waiting for the rules.
    pub fn is_idle(&self) -> bool {
        self.store.is_idle()
    }

    /// Whether the context holds the marker of `a`.
    pub fn has_hyp(&self, a: Ex) -> bool {
        self.hyps.contains(&a)
    }
}

/// The bit that marks the class a test context's core denies.
const NEG: Ex = 1 << 31;

/// The test context of whether `x` is below one of `bs`, sorted: its core is `x` and the
/// complements of `bs`.
pub fn test(d: &super::saturate::Dynamic, x: Ex, bs: &[Ex]) -> Ex {
    let mut core = vec![x];
    core.extend(bs.iter().map(|&b| b | NEG));
    d.intern(&core)
}

/// Starts a test context: its core, `owl:Thing` and the class it denies.
pub fn init_test<S: Sink>(st: &mut State, core: &[Ex], out: &mut S) {
    out.me(Concl::Sub(TOP));
    for &m in core {
        if m & NEG != 0 {
            ds(st).neg.push(m & !NEG);
        } else {
            out.me(Concl::Sub(m));
        }
    }
}

/// The candidate subsumers of the classes that Tier D tests: the named classes that the
/// canonical model of each root's context makes true and that are not among its
/// subsumers, nor tested before.
///
/// The root's context is saturated under a total order of atoms, so its clauses have a
/// canonical model, built as in the completeness proof of the calculus (JAIR 2018,
/// Appendix C): the clauses whose body is empty and whose maximal literal is an atom over
/// x, in the order of that literal, each make it true unless another of its literals
/// already is. That model is one of the ontology and the root's core, so a class it leaves
/// false is not a subsumer, and only the classes it makes true need a test.
pub fn candidates(
    ix: &Index,
    ctx: &super::saturate::Contexts,
    roots: &[Ex],
    tested: &FxHashSet<(Ex, Ex)>,
) -> Vec<(Ex, Ex)> {
    let mut out = Vec::new();
    let mut max = Vec::new();
    for &x in roots {
        let Some(c) = ctx.get(x as usize).and_then(|c| c.get()) else {
            continue;
        };
        let st = c.st.lock();
        if st.bottom {
            continue;
        }
        let Some(d) = dl(&st) else { continue };
        let mut cs: Vec<((u8, Ex), &Clause)> = Vec::new();
        for s in &d.store.clauses {
            if s.dead || !s.c.body().is_empty() {
                continue;
            }
            s.c.maximal(ix, &mut max);
            if let [m] = max.as_slice()
                && m.kind() == X
            {
                cs.push((super::dl_clause::rank(ix, m.atom()), &s.c));
            }
        }
        cs.sort_by_key(|&(k, _)| k);
        let mut model: FxHashSet<Ex> = FxHashSet::default();
        let mut found: Vec<Ex> = Vec::new();
        for (k, c) in cs {
            let a = k.1;
            let holds = c.head().iter().any(|l| {
                l.kind() == X && (st.subs.contains(&l.atom()) || model.contains(&l.atom()))
            });
            if holds {
                continue;
            }
            model.insert(a);
            if a != x && matches!(ix.head(a).node, Node::Class(_)) && !tested.contains(&(x, a)) {
                found.push(a);
            }
        }
        found.sort_unstable();
        out.extend(found.into_iter().map(|b| (x, b)));
    }
    out
}

fn ds(st: &mut State) -> &mut DlSt {
    st.extra().dl.get_or_insert_with(Box::default)
}

fn dl(st: &State) -> Option<&DlSt> {
    st.extra.as_ref().and_then(|x| x.dl.as_deref())
}

#[inline]
fn dix(ix: &Index) -> &DlIx {
    ix.dl.as_deref().expect("a run with Tier D")
}

/// The role of an existential restriction.
#[inline]
fn role_of(ix: &Index, e: Ex) -> Option<Role> {
    match ix.head(e).node {
        Node::Exist(r, _) => Some(r),
        _ => None,
    }
}

/// A premise's side: its body, and its head without the literal resolved on.
type Side = (Vec<Lit>, Vec<Lit>);

fn side(c: &Clause, without: Lit) -> Side {
    (
        c.body().to_vec(),
        c.head().iter().copied().filter(|&l| l != without).collect(),
    )
}

/// The premises whose maximal literal is `X(a)`: the unit, or the stored clauses.
fn partners_x(st: &State, a: Ex) -> Vec<Side> {
    if a == TOP || st.subs.contains(&a) {
        return vec![(Vec::new(), Vec::new())];
    }
    let Some(d) = dl(st) else {
        return Vec::new();
    };
    d.store
        .with_x(a)
        .map(|i| side(&d.store.clauses[i as usize].c, Lit::x(a)))
        .collect()
}

/// Whether the literal over `f_e(x)` holds: e is a unit and the literal is e's role or an
/// atom of e's successor's core.
#[inline]
fn holds_succ(dx: &DlIx, st: &State, l: Lit) -> bool {
    let e = l.fun();
    st.subs.contains(&e) && (l.kind() == RF || dx.in_core(e, l.atom()))
}

/// The premises whose maximal literal is `l`, over `f_e(x)`: the unit when it holds, or
/// the stored clauses.
fn partners_succ(dx: &DlIx, st: &State, l: Lit) -> Vec<Side> {
    if holds_succ(dx, st, l) {
        return vec![(Vec::new(), Vec::new())];
    }
    let Some(d) = dl(st) else {
        return Vec::new();
    };
    d.store
        .with_succ(l)
        .map(|i| side(&d.store.clauses[i as usize].c, l))
        .collect()
}

/// The existential restrictions whose role is below `t`, as units or as the maximal
/// literals of stored clauses, with the premise.
fn partners_rf(ix: &Index, st: &State, t: Role) -> Vec<(Ex, Side)> {
    let mut out = Vec::new();
    if !dix(ix).reaches(t) {
        return out;
    }
    let roles = &ix.roles;
    for &e in &st.subs {
        if let Some(r) = role_of(ix, e)
            && roles.sub(r, t)
        {
            out.push((e, (Vec::new(), Vec::new())));
        }
    }
    if let Some(d) = dl(st) {
        let mut es: Vec<Ex> = d.store.by_f.keys().copied().collect();
        es.sort_unstable();
        for e in es {
            if st.subs.contains(&e) {
                continue;
            }
            let Some(r) = role_of(ix, e) else { continue };
            if !roles.sub(r, t) {
                continue;
            }
            for i in d.store.with_succ(Lit::rf(e)) {
                out.push((e, side(&d.store.clauses[i as usize].c, Lit::rf(e))));
            }
        }
    }
    out
}

/// What the rules on one clause or unit give: single conclusions, and choices of one
/// premise from every list of a product, which [`drain`] derives one at a time so that a
/// wide case analysis is never held whole.
#[derive(Default)]
struct Found {
    one: Vec<Side>,
    prods: Vec<(Vec<Vec<Side>>, Side)>,
}

impl Found {
    fn push(&mut self, s: Side) {
        self.one.push(s);
    }
    /// Each choice of one premise from every list, merged with `base`.
    fn products(&mut self, lists: Vec<Vec<Side>>, base: Side) {
        if lists.iter().any(|l| l.is_empty()) {
            return;
        }
        self.prods.push((lists, base));
    }
}

/// The choices of a product between two checks of the run's bounds.
const PRODUCT_POLL: usize = 256;

/// Derives what the rules found: the single conclusions, then each product's choices in
/// the order of their lists, the last list fastest. The run's bounds are checked every
/// [`PRODUCT_POLL`] choices, and a stop of the run, found there or made by another worker,
/// leaves the rest of the inference undone and marks the context's store interrupted, a
/// store made for the mark if the context has none, so that the dependency record of a
/// partial answer counts the context as open.
fn drain<S: Sink>(ix: &Index, st: &mut State, found: Found, out: &mut S) {
    for c in found.one {
        derive(ix, st, c, out);
    }
    let mut n = 0usize;
    for (lists, base) in found.prods {
        let mut at = vec![0usize; lists.len()];
        'choices: loop {
            if out.stopped() || (n > 0 && n.is_multiple_of(PRODUCT_POLL) && !out.go_on()) {
                ds(st).store.interrupted = true;
                return;
            }
            let mut s = base.clone();
            for (l, &i) in lists.iter().zip(&at) {
                s.0.extend_from_slice(&l[i].0);
                s.1.extend_from_slice(&l[i].1);
            }
            derive(ix, st, s, out);
            n += 1;
            let mut k = lists.len();
            loop {
                if k == 0 {
                    break 'choices;
                }
                k -= 1;
                at[k] += 1;
                if at[k] < lists[k].len() {
                    break;
                }
                at[k] = 0;
            }
        }
    }
}

/// Two premises merged.
fn merge(a: &Side, b: &Side) -> Side {
    let mut s = a.clone();
    s.0.extend_from_slice(&b.0);
    s.1.extend_from_slice(&b.1);
    s
}

/// A premise with one more head literal.
fn with(a: &Side, l: Lit) -> Side {
    let mut s = a.clone();
    s.1.push(l);
    s
}

/// The universal restrictions that a holder `d` stands for, as `(t, c)` for `d ⊑ ∀t.c`:
/// from a universal node, and from the negative existential restrictions `∃s.d ⊑ c`,
/// which are `d ⊑ ∀s⁻.c`.
fn alls(ix: &Index, d: Ex, out: &mut Vec<(Role, Ex)>) {
    out.clear();
    let h = ix.head(d);
    if let Node::Forall(t, c) = h.node {
        out.push((t, c));
    }
    for &(s, x) in ix.exist_of(h) {
        if let Some(i) = ix.roles.inv(s) {
            out.push((i, x));
        }
    }
}

/// Adds a clause the rules derived in this context: a unit over x goes to the EL rules, an
/// empty clause makes the context unsatisfiable, and any other clause goes to the store
/// unless it is redundant.
fn derive<S: Sink>(ix: &Index, st: &mut State, (mut body, mut head): Side, out: &mut S) {
    if st.bottom {
        return;
    }
    let dx = dix(ix);
    // a test context's denied class leaves the head
    if let Some(x) = dl(st)
        && !x.neg.is_empty()
    {
        head.retain(|&l| !(l.kind() == X && x.neg.binary_search(&l.atom()).is_ok()));
    }
    // atoms that hold in the context leave the body
    body.retain(|l| !(l.kind() == X && (l.atom() == TOP || st.subs.contains(&l.atom()))));
    // a literal that holds makes the clause redundant, and one that cannot leaves it
    let mut holds = false;
    head.retain(|&l| match l.kind() {
        X | Y | F if l.atom() == BOTTOM => false,
        X | F if l.atom() == TOP => {
            holds = true;
            true
        }
        X if st.subs.contains(&l.atom()) => {
            holds = true;
            true
        }
        F | RF if holds_succ(dx, st, l) => {
            holds = true;
            true
        }
        _ => true,
    });
    if holds {
        return;
    }
    body.sort_unstable();
    body.dedup();
    head.sort_unstable();
    head.dedup();
    // Γ ∧ A → Δ ∨ A is a consequence of the marker of A
    if head
        .iter()
        .any(|l| l.kind() == X && body.binary_search(l).is_ok())
    {
        return;
    }
    if body.is_empty() {
        match head.as_slice() {
            [] => {
                out.me(Concl::Sub(BOTTOM));
                return;
            }
            [l] if l.kind() == X => {
                out.me(Concl::Sub(l.atom()));
                return;
            }
            _ => {}
        }
    }
    if let Some(bytes) = ds(st).store.add(Clause::new(body, head)) {
        out.dl().grow(bytes);
    }
}

/// Hyper with a clause of case analysis, `id`, with `prem` as the premise of its body atom
/// `d`: the other atoms' premises, and the head as literals over x.
fn hyper_clause(ix: &Index, st: &State, id: u32, d: Ex, prem: &Side, found: &mut Found) {
    let (body, head) = &dix(ix).clauses[id as usize];
    let lists: Vec<Vec<Side>> = body
        .iter()
        .map(|&a| {
            if a == d {
                vec![prem.clone()]
            } else {
                partners_x(st, a)
            }
        })
        .collect();
    let base: Side = (Vec::new(), head.iter().map(|&h| Lit::x(h)).collect());
    found.products(lists, base);
}

/// The universals of the holders over the successor by e: the units' through the
/// propagations, and the stored holders', each with `prem`, the premise of `RF(e)`.
fn univ_over(ix: &Index, st: &State, e: Ex, r: Role, prem: &Side, found: &mut Found) {
    let roles = &ix.roles;
    let dx = dix(ix);
    for &(s, x) in &st.props {
        if let Some(t) = roles.inv(s)
            && dx.reaches(t)
            && roles.sub(r, t)
        {
            found.push(with(prem, Lit::f(e, x)));
        }
    }
    let Some(d) = dl(st) else { return };
    let mut holders: Vec<Ex> = d.store.by_x.keys().copied().collect();
    holders.sort_unstable();
    let mut ps = Vec::new();
    for b in holders {
        if st.subs.contains(&b) {
            continue;
        }
        alls(ix, b, &mut ps);
        for &(t, c) in &ps {
            if roles.sub(r, t) {
                for p in partners_x(st, b) {
                    found.push(with(&merge(prem, &p), Lit::f(e, c)));
                }
            }
        }
    }
}

/// A new unit `→ d(x)`, after the EL rules: Hyper with the clauses of case analysis and
/// with the stored clauses, the universals over the successors and the predecessor, the
/// universals over a new successor, and the propagations to the registered predecessors.
pub fn sub<S: Sink>(ix: &Index, me: Ex, st: &mut State, d: Ex, out: &mut S) {
    if dl(st).is_some_and(|x| x.neg.binary_search(&d).is_ok()) {
        out.me(Concl::Sub(BOTTOM));
        return;
    }
    let dx = dix(ix);
    let roles = &ix.roles;
    let mut found = Found::default();
    let unit: Side = (Vec::new(), Vec::new());
    if let Some(ids) = dx.by_body.get(&d) {
        for &id in ids {
            hyper_clause(ix, st, id, d, &unit, &mut found);
        }
    }
    let h = ix.head(d);
    if dl(st).is_some() {
        // the stored clauses meet d in conjunctions and disjointness
        for &(p, k) in ix.conj_of(h) {
            if !st.subs.contains(&p) {
                for p in partners_x(st, p) {
                    found.push(with(&p, Lit::x(k)));
                }
            }
        }
        for &set in ix.disjoint_of(h) {
            for &m in &dx.sets[set as usize] {
                if m != d && !st.subs.contains(&m) {
                    found.one.extend(partners_x(st, m));
                }
            }
        }
        // the stored clauses that d, or the successor by d, satisfies are redundant
        let x = ds(st);
        let mut dead = x.store.with_head(Lit::x(d));
        if let Node::Exist(..) = h.node {
            dead.extend(x.store.with_head(Lit::rf(d)));
            if let Some(core) = dx.cores.get(&d) {
                for &a in core.iter() {
                    dead.extend(x.store.with_head(Lit::f(d, a)));
                }
            }
        }
        for i in dead {
            x.store.clauses[i as usize].dead = true;
        }
    }
    // a universal: the propagation over the backward links, as for a negative
    // existential restriction
    if let Node::Forall(t, c) = h.node
        && let Some(i) = roles.inv(t)
    {
        st.props.push((i, c));
        for (r, srcs) in &st.back {
            if roles.sub(*r, i) {
                for &e in &srcs.list {
                    out.send(e, Concl::Sub(c));
                }
            }
        }
    }
    let mut ps: Vec<(Role, Ex)> = Vec::new();
    alls(ix, d, &mut ps);
    for &(t, c) in &ps {
        // over the successors
        for (e, p) in partners_rf(ix, st, t) {
            found.push(with(&p, Lit::f(e, c)));
        }
        // over the registered predecessors
        if let Some(i) = roles.inv(t) {
            props_to_dpreds(ix, me, st, i, c, out);
        }
    }
    // an existential restriction: the universals of the holders over its successor
    if let Node::Exist(r, _) = h.node {
        univ_over(ix, st, d, r, &unit, &mut found);
    }
    drain(ix, st, found, out);
}

/// The context became unsatisfiable: its registered predecessors learn it.
pub fn bottom<S: Sink>(me: Ex, st: &mut State, out: &mut S) {
    let Some(x) = dl(st) else { return };
    if x.dpreds.is_empty() {
        return;
    }
    let id = out.dl().empty();
    for &(u, _) in &x.dpreds {
        out.send(u, Concl::PredC(me, id));
    }
}

/// The table id of the clause `s(y, x) → c(y)` of a propagation.
fn prop_id<S: Sink>(x: &mut DlSt, s: Role, c: Ex, out: &mut S) -> u32 {
    *x.props.entry((s, c)).or_insert_with(|| {
        out.dl()
            .table
            .push(Clause::new(vec![Lit::ry(s)], vec![Lit::y(c)]))
    })
}

/// Sends the registered predecessors whose role is below `s` the clause `s(y, x) → c(y)`
/// of a propagation.
fn props_to_dpreds<S: Sink>(ix: &Index, me: Ex, st: &mut State, s: Role, c: Ex, out: &mut S) {
    let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) else {
        return;
    };
    if x.dpreds.is_empty() {
        return;
    }
    let id = prop_id(x, s, c, out);
    for &(u, e) in &x.dpreds {
        if role_of(ix, e).is_some_and(|r| ix.roles.sub(r, s)) {
            out.send(u, Concl::PredC(me, id));
        }
    }
}

/// Whether a link by `r` meets the role atoms of a clause's body.
fn meets(ix: &Index, c: &Clause, r: Role) -> bool {
    c.body()
        .iter()
        .filter(|l| l.kind() == RY)
        .all(|l| ix.roles.sub(r, l.atom()))
}

/// The table id of a stored clause over the predecessor, `None` once it is dead.
fn pred_id<S: Sink>(x: &mut DlSt, i: u32, out: &mut S) -> Option<u32> {
    let s = &mut x.store.clauses[i as usize];
    if s.dead {
        return None;
    }
    if s.sent.is_none() {
        s.sent = Some(out.dl().table.push(s.c.clone()));
    }
    s.sent
}

/// A new backward link from `e` by `r`: the source takes the clauses over the predecessor
/// that the role meets.
pub fn back<S: Sink>(ix: &Index, me: Ex, st: &mut State, e: Ex, r: Role, out: &mut S) {
    let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) else {
        return;
    };
    for k in 0..x.store.preds.len() {
        let i = x.store.preds[k];
        if let Some(cid) = pred_id(x, i, out)
            && meets(ix, &x.store.clauses[i as usize].c, r)
        {
            out.send(e, Concl::PredC(me, cid));
        }
    }
}

/// A marker `a → a`: the atom may hold of the context's instance.
pub fn hyp<S: Sink>(st: &mut State, a: Ex, out: &mut S) -> bool {
    if st.bottom || a == TOP || st.subs.contains(&a) {
        return false;
    }
    let x = ds(st);
    if !x.hyps.insert(a) {
        return false;
    }
    if let Some(bytes) = x.store.add(Clause::new(vec![Lit::x(a)], vec![Lit::x(a)])) {
        out.dl().grow(bytes + 4);
    }
    true
}

/// A predecessor registers by its existential restriction e: it takes the clauses over the
/// predecessor that e's role meets, the propagations, and unsatisfiability.
pub fn dedge<S: Sink>(ix: &Index, me: Ex, st: &mut State, u: Ex, e: Ex, out: &mut S) -> bool {
    let bottom = st.bottom;
    let props: Vec<(Role, Ex)> = st.props.clone();
    let x = ds(st);
    if x.dpreds.contains(&(u, e)) {
        return false;
    }
    x.dpreds.push((u, e));
    if bottom {
        let id = out.dl().empty();
        out.send(u, Concl::PredC(me, id));
        return true;
    }
    let Some(r) = role_of(ix, e) else { return true };
    for k in 0..x.store.preds.len() {
        let i = x.store.preds[k];
        if let Some(cid) = pred_id(x, i, out)
            && meets(ix, &x.store.clauses[i as usize].c, r)
        {
            out.send(u, Concl::PredC(me, cid));
        }
    }
    for (s, c) in props {
        if ix.roles.sub(r, s) {
            let id = prop_id(x, s, c, out);
            out.send(u, Concl::PredC(me, id));
        }
    }
    true
}

/// The existential restrictions of this context whose successor is `v`: those it holds as
/// units whose EL successor is v, and the one whose hypothetical successor v is, once the
/// context registered with it.
fn reaching(dx: &DlIx, st: &State, v: Ex, out: &mut Vec<Ex>) {
    out.clear();
    if let Some(x) = dl(st) {
        out.extend(x.edges.iter().filter(|&&(_, w)| w == v).map(|&(e, _)| e));
    }
    if dx.of_hyp.contains_key(&v) {
        return;
    }
    if let Some(es) = dx.by_succ.get(&v) {
        out.extend(es.iter().copied().filter(|e| st.subs.contains(e)));
    }
}

/// A clause over the predecessor from the successor `v`: Pred with each existential
/// restriction that reaches v.
pub fn pred_c<S: Sink>(ix: &Index, st: &mut State, v: Ex, cid: u32, out: &mut S) -> bool {
    if st.bottom {
        return false;
    }
    let x = ds(st);
    if !x.pred_seen.insert((v, cid)) {
        return false;
    }
    x.pred_in.entry(v).or_default().push(cid);
    let c = out.dl().table.get(cid).clone();
    let mut es = Vec::new();
    reaching(dix(ix), st, v, &mut es);
    let mut found = Found::default();
    for e in es {
        pred(ix, st, e, &c, None, &mut found);
    }
    drain(ix, st, found, out);
    true
}

/// Pred over e with the clause `c` of e's successor: each body atom `A` takes a premise
/// whose maximal literal is `F(e, A)`, the role atoms one whose maximal literal is
/// `RF(e)`, and the head's atoms over y become atoms over x. `fixed` is the premise that a
/// new clause gives for one of these literals, which the inference must use.
fn pred(ix: &Index, st: &State, e: Ex, c: &Clause, fixed: Option<(Lit, &Side)>, found: &mut Found) {
    let Some(r) = role_of(ix, e) else { return };
    if !meets(ix, c, r) {
        return;
    }
    let dx = dix(ix);
    let mut need: Vec<Lit> = c
        .body()
        .iter()
        .filter(|l| l.kind() == X)
        .map(|l| Lit::f(e, l.atom()))
        .collect();
    if c.body().iter().any(|l| l.kind() == RY) {
        need.push(Lit::rf(e));
    }
    if let Some((f, _)) = fixed
        && !need.contains(&f)
    {
        return;
    }
    let mut used = false;
    let mut lists: Vec<Vec<Side>> = Vec::with_capacity(need.len());
    for l in need {
        match fixed {
            Some((f, p)) if f == l && !used => {
                used = true;
                lists.push(vec![p.clone()]);
            }
            _ => lists.push(partners_succ(dx, st, l)),
        }
    }
    let base: Side = (
        Vec::new(),
        c.head().iter().map(|l| Lit::x(l.atom())).collect(),
    );
    found.products(lists, base);
}

/// Runs the rules on up to `budget` clauses waiting in the store, and stops early once
/// they gave the context units, which the EL rules take first. The answer is the number
/// of clauses processed.
pub fn flush<S: Sink>(ix: &Index, me: Ex, st: &mut State, out: &mut S, budget: usize) -> usize {
    let mut n = 0;
    let mut max: Vec<Lit> = Vec::new();
    while n < budget {
        if st.bottom {
            break;
        }
        let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) else {
            break;
        };
        let Some(id) = x.store.next(ix, &mut max) else {
            break;
        };
        n += 1;
        let c = x.store.clauses[id as usize].c.clone();
        process(ix, me, st, id, &c, &max, out);
        // the units go to the EL rules first, and a stopped run leaves the rest queued
        if out.busy() || out.stopped() {
            break;
        }
    }
    n
}

/// The rules on one clause of the store, whose maximal literals are `max`.
fn process<S: Sink>(
    ix: &Index,
    me: Ex,
    st: &mut State,
    id: u32,
    c: &Clause,
    max: &[Lit],
    out: &mut S,
) {
    let roles = &ix.roles;
    let dx = dix(ix);
    if max.is_empty() {
        // Pred: the clause goes to the predecessors whose role meets its role atoms
        let x = ds(st);
        let Some(cid) = pred_id(x, id, out) else {
            return;
        };
        for &(u, e) in &x.dpreds {
            if role_of(ix, e).is_some_and(|r| meets(ix, c, r)) {
                out.send(u, Concl::PredC(me, cid));
            }
        }
        for (r, srcs) in &st.back {
            if meets(ix, c, *r) {
                for &e in &srcs.list {
                    out.send(e, Concl::PredC(me, cid));
                }
            }
        }
        return;
    }
    let mut found = Found::default();
    let mut ps: Vec<(Role, Ex)> = Vec::new();
    let mut es: Vec<Ex> = Vec::new();
    for &l in max {
        let prem = side(c, l);
        match l.kind() {
            X => {
                let d = l.atom();
                let h = ix.head(d);
                match h.node {
                    Node::Conj(a, b) => {
                        found.push(with(&prem, Lit::x(a)));
                        found.push(with(&prem, Lit::x(b)));
                    }
                    Node::Exist(..) => {
                        found.push(with(&prem, Lit::rf(d)));
                        found.push(with(&prem, Lit::f(d, h.succ)));
                    }
                    _ => {}
                }
                for &t in ix.told_of(h) {
                    found.push(with(&prem, Lit::x(t)));
                }
                for &(p, k) in ix.conj_of(h) {
                    for q in partners_x(st, p) {
                        found.push(with(&merge(&prem, &q), Lit::x(k)));
                    }
                }
                for &set in ix.disjoint_of(h) {
                    for &m in &dx.sets[set as usize] {
                        if m != d {
                            for q in partners_x(st, m) {
                                found.push(merge(&prem, &q));
                            }
                        }
                    }
                }
                if let Some(ids) = dx.by_body.get(&d) {
                    for &cl in ids {
                        hyper_clause(ix, st, cl, d, &prem, &mut found);
                    }
                }
                // the universals d stands for, over the predecessor and the successors
                alls(ix, d, &mut ps);
                for &(t, x) in &ps {
                    if let Some(i) = roles.inv(t) {
                        let mut p = prem.clone();
                        p.0.push(Lit::ry(i));
                        p.1.push(Lit::y(x));
                        found.push(p);
                    }
                    for (e, q) in partners_rf(ix, st, t) {
                        found.push(with(&merge(&prem, &q), Lit::f(e, x)));
                    }
                }
            }
            F | RF => {
                let e = l.fun();
                // Succ: the successor takes the marker
                let v = target(ix, me, st, e, out);
                if l.kind() == F {
                    send_hyp(ix, st, v, l.atom(), out);
                } else if let Some(r) = role_of(ix, e) {
                    univ_over(ix, st, e, r, &prem, &mut found);
                }
                // Pred with the clauses the successors of e sent: the one registered with,
                // and the EL one when e is a unit
                es.clear();
                if let Some(&(_, w)) = dl(st).and_then(|x| x.edges.iter().find(|&&(f, _)| f == e)) {
                    es.push(w);
                }
                if st.subs.contains(&e) && !es.contains(&ix.head(e).succ) {
                    es.push(ix.head(e).succ);
                }
                for &v in &es {
                    let cids: Vec<u32> = dl(st)
                        .and_then(|x| x.pred_in.get(&v))
                        .cloned()
                        .unwrap_or_default();
                    for cid in cids {
                        let pc = out.dl().table.get(cid).clone();
                        pred(ix, st, e, &pc, Some((l, &prem)), &mut found);
                    }
                }
            }
            _ => {}
        }
    }
    drain(ix, st, found, out);
}

/// Sends the successor `v` the marker of `a`, unless the successor's core has it.
fn send_hyp<S: Sink>(ix: &Index, st: &mut State, v: Ex, a: Ex, out: &mut S) {
    if a == TOP || ds(st).sent.contains(&(v, a)) {
        return;
    }
    let mut core = Vec::new();
    if !dix(ix).of_hyp.contains_key(&v) {
        out.core(ix, v, &mut core);
    }
    if core.binary_search(&a).is_err() {
        ds(st).sent.insert((v, a));
        out.send(v, Concl::Hyp(a));
    }
}

/// The successor that e's literals go to. The context keeps the one it registered with.
/// Otherwise the cautious strategy takes the EL successor when e is a unit, whose core is
/// e's filler with the role's range, and else the hypothetical successor, with the empty
/// core. The eager strategy takes the context whose core is what holds of the successor
/// for certain: the EL successor when that is all, and else the hypothetical successor
/// for nothing, the context of the one atom, or that of the conjunction. A successor the
/// context registers with takes the markers of the EL core's atoms it lacks.
fn target<S: Sink>(ix: &Index, me: Ex, st: &mut State, e: Ex, out: &mut S) -> Ex {
    let dx = dix(ix);
    let unit = st.subs.contains(&e);
    let h = dx.hyp.get(&e).copied().unwrap_or(TOP);
    let succ = ix.head(e).succ;
    if let Some(&(_, v)) = dl(st).and_then(|x| x.edges.iter().find(|&&(f, _)| f == e)) {
        return v;
    }
    let v = if out.dl().eager {
        let mut k1: Vec<Ex> = Vec::new();
        if unit {
            k1.extend(dx.cores.get(&e).into_iter().flat_map(|c| c.iter().copied()));
        }
        if let Some(x) = dl(st) {
            for i in x.store.with_succ_fun(e) {
                let c = &x.store.clauses[i as usize].c;
                if let ([], [l]) = (c.body(), c.head())
                    && l.kind() == F
                {
                    k1.push(l.atom());
                }
            }
        }
        k1.retain(|&a| a != TOP);
        k1.sort_unstable();
        k1.dedup();
        if unit && dx.cores.get(&e).is_some_and(|c| **c == *k1) {
            return succ;
        }
        match k1.as_slice() {
            [] => h,
            [a] => *a,
            _ => out.intern(&k1),
        }
    } else {
        if unit {
            return succ;
        }
        h
    };
    ds(st).edges.push((e, v));
    out.send(v, Concl::DEdge(me, e));
    if let Some(core) = dx.cores.get(&e) {
        for &a in core.iter() {
            send_hyp(ix, st, v, a, out);
        }
    }
    v
}

/// Whether the ontology has a model, as far as a stopped run of Tier D knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Known {
    Consistent,
    Inconsistent,
    Unknown,
}

/// The dependency record of a run of Tier D that stopped: which roots have their whole
/// answer, and whether the ontology's consistency is decided.
///
/// A context's units and unsatisfiability depend only on the contexts it reaches forward:
/// the successors of its existential restrictions, the hypothetical successors it
/// registered with and the targets of its forward links. A predecessor's later markers
/// make clauses whose bodies only that predecessor resolves. A context is closed when it
/// has started and its to-do list and clause queue are empty, which a stopped worker's
/// returned conclusions and delivered outbox make false where work was left. A root is
/// complete when every context it reaches is closed, no candidate of its tests is left
/// untested, and the ontology is consistent. The ontology is consistent when `owl:Thing`'s
/// context and every context it reaches are closed and it is satisfiable, and
/// inconsistent when that context is unsatisfiable, whatever else is open.
pub fn record(
    ix: &Index,
    ctx: &super::saturate::Contexts,
    dynamic: Option<&super::saturate::Dynamic>,
    roots: &[Ex],
    refuted: &FxHashSet<(Ex, Ex)>,
) -> (Vec<bool>, Known) {
    let n = ctx.len() as Ex;
    let get = |e: Ex| -> Option<&super::saturate::Ctx> {
        if e < n {
            ctx[e as usize].get().map(|c| &**c)
        } else {
            dynamic.map(|d| d.ctx(e))
        }
    };
    let all: Vec<Ex> = (0..n)
        .chain(dynamic.map_or(0..0, |d| n..n + d.len() as Ex))
        .collect();
    // the contexts that are not closed, and the reverse of the forward edges
    let mut open: FxHashSet<Ex> = FxHashSet::default();
    let mut rev: FxHashMap<Ex, Vec<Ex>> = FxHashMap::default();
    let mut reached: Vec<Ex> = Vec::new();
    for &e in &all {
        let Some(c) = get(e) else { continue };
        let st = c.st.lock();
        let started = c.started.load(Ordering::Acquire) && st.init;
        let idle = dl(&st).is_none_or(|d| d.is_idle());
        if !started || !idle || !c.todo.lock().is_empty() {
            open.insert(e);
        }
        let mut edge = |to: Ex| {
            rev.entry(to).or_default().push(e);
            reached.push(to);
        };
        for &d in &st.subs {
            if let Node::Exist(..) = ix.head(d).node {
                edge(ix.head(d).succ);
            }
        }
        if let Some(d) = dl(&st) {
            for &(_, v) in &d.edges {
                edge(v);
            }
        }
        for &(_, f) in &st.fwd.list {
            edge(f);
        }
    }
    // a context reached but never made has not started either
    for e in reached {
        if get(e).is_none() {
            open.insert(e);
        }
    }
    // the contexts that reach an open one
    let mut bad = open.clone();
    let mut stack: Vec<Ex> = open.into_iter().collect();
    while let Some(e) = stack.pop() {
        for &p in rev.get(&e).into_iter().flatten() {
            if bad.insert(p) {
                stack.push(p);
            }
        }
    }
    let unsat = |e: Ex| {
        ctx.get(e as usize)
            .and_then(|c| c.get())
            .is_some_and(|c| c.st.lock().bottom)
    };
    let known = if unsat(TOP) {
        Known::Inconsistent
    } else if !bad.contains(&TOP) {
        Known::Consistent
    } else {
        Known::Unknown
    };
    let pending: FxHashSet<Ex> = candidates(ix, ctx, roots, refuted)
        .into_iter()
        .map(|(x, _)| x)
        .collect();
    let complete = roots
        .iter()
        .map(|&r| match known {
            Known::Inconsistent => true,
            Known::Unknown => false,
            Known::Consistent => !bad.contains(&r) && !pending.contains(&r),
        })
        .collect();
    (complete, known)
}
