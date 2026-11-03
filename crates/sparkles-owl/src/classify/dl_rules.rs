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

/// What Tier D reads in the index.
#[derive(Default)]
pub struct DlIx {
    /// the clauses with two or more disjuncts, by id: the body, where an empty one is
    /// `owl:Thing`, and the head
    pub clauses: Vec<(Box<[Ex]>, Box<[Ex]>)>,
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
    stores: AtomicUsize,
    /// the clause with an empty body and head, which an unsatisfiable context sends
    empty: OnceLock<u32>,
}

impl DlRun {
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
    /// the existential restrictions whose hypothetical successor this context registered
    /// with
    pub dedges: Vec<Ex>,
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
            + self.dedges.capacity() * 4
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

/// Each choice of one premise from every list, merged with `base`.
fn products(lists: &[Vec<Side>], base: Side, found: &mut Vec<Side>) {
    fn go(lists: &[Vec<Side>], acc: &Side, found: &mut Vec<Side>) {
        match lists.split_first() {
            None => found.push(acc.clone()),
            Some((first, rest)) => {
                for (b, h) in first {
                    let mut next = acc.clone();
                    next.0.extend_from_slice(b);
                    next.1.extend_from_slice(h);
                    go(rest, &next, found);
                }
            }
        }
    }
    if lists.iter().any(|l| l.is_empty()) {
        return;
    }
    go(lists, &base, found);
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
fn hyper_clause(ix: &Index, st: &State, id: u32, d: Ex, prem: &Side, found: &mut Vec<Side>) {
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
    products(&lists, base, found);
}

/// The universals of the holders over the successor by e: the units' through the
/// propagations, and the stored holders', each with `prem`, the premise of `RF(e)`.
fn univ_over(ix: &Index, st: &State, e: Ex, r: Role, prem: &Side, found: &mut Vec<Side>) {
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
    let mut found: Vec<Side> = Vec::new();
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
                    found.extend(partners_x(st, m));
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
    for c in found {
        derive(ix, st, c, out);
    }
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
    if let Some(&e) = dx.of_hyp.get(&v) {
        if dl(st).is_some_and(|x| x.dedges.contains(&e)) {
            out.push(e);
        }
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
    let mut found = Vec::new();
    for e in es {
        pred(ix, st, e, &c, None, &mut found);
    }
    for c in found {
        derive(ix, st, c, out);
    }
    true
}

/// Pred over e with the clause `c` of e's successor: each body atom `A` takes a premise
/// whose maximal literal is `F(e, A)`, the role atoms one whose maximal literal is
/// `RF(e)`, and the head's atoms over y become atoms over x. `fixed` is the premise that a
/// new clause gives for one of these literals, which the inference must use.
fn pred(
    ix: &Index,
    st: &State,
    e: Ex,
    c: &Clause,
    fixed: Option<(Lit, &Side)>,
    found: &mut Vec<Side>,
) {
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
    products(&lists, base, found);
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
        if out.busy() {
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
    let mut found: Vec<Side> = Vec::new();
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
                    send_hyp(dx, st, e, v, l.atom(), out);
                } else if let Some(r) = role_of(ix, e) {
                    univ_over(ix, st, e, r, &prem, &mut found);
                }
                // Pred with the clauses the successors of e sent: the hypothetical one once
                // registered with, and the EL one when e is a unit
                es.clear();
                if dl(st).is_some_and(|x| x.dedges.contains(&e))
                    && let Some(&h) = dx.hyp.get(&e)
                {
                    es.push(h);
                }
                if st.subs.contains(&e) {
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
    for c in found {
        derive(ix, st, c, out);
    }
}

/// Sends the successor `v` by e the marker of `a`, unless the successor's core has it.
fn send_hyp<S: Sink>(dx: &DlIx, st: &mut State, e: Ex, v: Ex, a: Ex, out: &mut S) {
    if a == TOP || (dx.of_hyp.get(&v).is_none() && dx.in_core(e, a)) {
        return;
    }
    if ds(st).sent.insert((v, a)) {
        out.send(v, Concl::Hyp(a));
    }
}

/// The successor that e's literals go to, by the cautious strategy: the hypothetical
/// successor once the context has registered with it, else the EL successor when e is a
/// unit, else the hypothetical successor, which the context registers with and gives the
/// markers of the EL successor's core.
fn target<S: Sink>(ix: &Index, me: Ex, st: &mut State, e: Ex, out: &mut S) -> Ex {
    let dx = dix(ix);
    let unit = st.subs.contains(&e);
    let h = dx.hyp.get(&e).copied().unwrap_or(TOP);
    let x = ds(st);
    if x.dedges.contains(&e) {
        return h;
    }
    if unit {
        return ix.head(e).succ;
    }
    x.dedges.push(e);
    out.send(h, Concl::DEdge(me, e));
    if let Some(core) = dx.cores.get(&e) {
        for &a in core.iter() {
            send_hyp(dx, st, e, h, a, out);
        }
    }
    h
}
