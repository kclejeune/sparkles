//! The root context of a rooted run of roadmap item 603's Tier D (F24, "Nominals in the
//! disjunctive rules"): the context with the empty core, whose clauses hold no x, where the
//! facts about named individuals meet. It follows the general rooted calculus of Tena
//! Cucala, Cuenca Grau and Horrocks (*Pay-As-You-Go Consequence-Based Reasoning for the
//! Description Logic SROIQ*, AIJ 2021, Tables 2 and 3).
//!
//! The root starts with `⊤ → N_u(u)` for the singleton class `N_u` of every constant u, so
//! that every named individual exists, and with what the ontology's clauses with the body
//! `owl:Thing` give each constant. Its Hyper rule binds the ontology clause's x to a
//! constant, so that a premise is a root clause whose maximal literal is a ground atom
//! `B(u)`. The ontology clauses are those of the EL index, read as the rules of
//! [`super::dl_rules`] read them over x (told subsumers, conjunctions, existential
//! restrictions, disjointness and universal restrictions), Tier D's clauses with two or
//! more disjuncts, and `N_v(x) → x ≈ v` of each singleton class. An existential restriction
//! e gives `r(u, f_e(u))` and the atom of its successor's core over `f_e(u)`, and a
//! universal restriction meets the role atoms over `f_e(u)` in the root. The empty clause
//! makes the root unsatisfiable, which makes the ontology inconsistent.
//!
//! The equality rules are those of [`super::dl_eq`], which rewrite a constant everywhere in
//! a root literal. Join resolves a maximal ground atom or equality of constants with the
//! same literal in another clause's body, in the root and in every other context.
//!
//! Every context reasons about its own ground literals (F24, item 804's first departure).
//! Hyper binds the ontology clause's x to a constant in every context, so that a premise is
//! any clause whose maximal literal is a ground atom `B(u)`, and a role atom with a constant
//! at an end meets the universal restrictions of the constant's holders in the context that
//! holds it. Each context keeps its processed clauses by the constants of their maximal
//! literals for these rules, as the root does. The root holds the unconditional facts, and
//! its processed clauses with an empty body go to the contexts registered for one of
//! their constants, whether the registration or the clause comes first. A context
//! registers with the root for the constants of every maximal literal of its processed
//! clauses, inside `f_e(u)` included, and for those of the ground literals in its bodies.
//! The run uses no markers in the root, no r-Succ and no r-Pred of a root clause with a
//! body. A conditional ground clause of a context, such as `⊤ → B(u) ∨ C(x)`, has its
//! consequences at u derived in that context from the actual disjunction, where the
//! journal's root derives them under every combination of markers.
//!
//! A maximal literal over `f_e(u)` in any context takes Succ: the context registers with e's
//! hypothetical successor, which has the empty core, records the constant u for the edge and
//! sends the successor the markers of the atoms over `f_e(u)`. The edge is identified by the
//! context, the restriction and the constant, and is distinct from the context's edge for
//! `f_e(x)`. The successor's clauses over its predecessor come back with y read as u and x as
//! `f_e(u)` (Pred), for every constant the context reached it by, and a role atom between x
//! and a constant comes back too, as Table 3 admits for the root. Every context sends each
//! of its successors the markers of its maximal ground atoms and equalities of constants,
//! so that one edge covers every successor trigger of Definition 4 (the journal's K2).
//!
//! The at-most restrictions and the qualified roles of counting are not read in the root.

use super::dl_clause::{Clause, Lit, Made, RTerm, Rooted, XL, XLit, terms_of};
use super::dl_rules::{
    DlIx, Found, Side, alls, dix, dl, drain, drain_with, ds, merge, pred_id, restricted, side,
    unit, with,
};
use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use super::saturate::{Concl, Sink, State, table_bytes};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::atomic::Ordering;

/// What a context of a rooted run keeps for the rules at the constants and over `f_e(u)`.
/// Every context keeps its processed clauses by constant and its edges over `f_e(u)`. The
/// root keeps the contexts registered with it and the facts it sends them, and another
/// context keeps the root's facts it received and the constants it registered for.
#[derive(Default, Clone)]
pub struct RootSt {
    /// the processed clauses with a maximal ground atom `B(u)`, by u
    by_const: FxHashMap<u32, Vec<u32>>,
    /// the processed clauses with a maximal role atom that has the constant u at an end,
    /// by u
    roles_of: FxHashMap<u32, Vec<u32>>,
    /// in the root, the contexts registered for each constant
    edges: FxHashMap<u32, Vec<Ex>>,
    /// in the root, the same registrations as pairs of context and constant
    edge_set: FxHashSet<(Ex, u32)>,
    /// in the root, the processed clauses with an empty body, by each constant of their
    /// heads
    ready: FxHashMap<u32, Vec<u32>>,
    /// in another context, the root's clauses it received, by table id
    seen: FxHashSet<u32>,
    /// in another context, the constants it registered with the root for
    edged: FxHashSet<u32>,
    /// the context's maximal ground atoms and equalities of constants, in the order found,
    /// which its successors take as markers
    ground: Vec<Lit>,
    ground_set: FxHashSet<Lit>,
    /// the ground markers sent, by successor
    gsent: FxHashSet<(Ex, Lit)>,
    /// in another context, the ground markers it holds
    gmarks: FxHashSet<Lit>,
    /// the context's edges over `f_e(u)`, by existential restriction and constant
    succ_set: FxHashSet<(Ex, u32)>,
    /// the existential restrictions and constants of the context's edges over `f_e(u)` to
    /// each successor
    by_succ: FxHashMap<Ex, Vec<(Ex, u32)>>,
}

impl RootSt {
    /// The existential restrictions and constants of the context's edges over `f_e(u)` to
    /// the successor w.
    pub(super) fn edges_to(&self, w: Ex) -> &[(Ex, u32)] {
        self.by_succ.get(&w).map_or(&[][..], |v| v)
    }

    pub fn bytes(&self) -> usize {
        let lists = |m: &FxHashMap<u32, Vec<u32>>| {
            table_bytes(m.capacity(), 32) + m.values().map(|v| v.capacity() * 4).sum::<usize>()
        };
        std::mem::size_of::<RootSt>()
            + lists(&self.by_const)
            + lists(&self.roles_of)
            + lists(&self.edges)
            + table_bytes(self.edge_set.capacity(), 8)
            + lists(&self.ready)
            + table_bytes(self.seen.capacity(), 4)
            + table_bytes(self.edged.capacity(), 4)
            + self.ground.capacity() * 8
            + table_bytes(self.ground_set.capacity(), 8)
            + table_bytes(self.gsent.capacity(), 16)
            + table_bytes(self.gmarks.capacity(), 8)
            + table_bytes(self.succ_set.capacity(), 8)
            + table_bytes(self.by_succ.capacity(), 32)
            + self
                .by_succ
                .values()
                .map(|v| v.capacity() * 8)
                .sum::<usize>()
    }

    /// Inserts `v` into `set`, whose entries take `size` bytes, and answers the bytes the
    /// set grew by, or `None` when it held v.
    fn ins<T: std::hash::Hash + Eq>(set: &mut FxHashSet<T>, v: T, size: usize) -> Option<usize> {
        let cap = set.capacity();
        if !set.insert(v) {
            return None;
        }
        Some(table_bytes(set.capacity(), size).saturating_sub(table_bytes(cap, size)))
    }

    /// Adds `id` to the list of `k` in `map`, and answers the bytes the map and the list
    /// grew by.
    fn push<K: std::hash::Hash + Eq>(map: &mut FxHashMap<K, Vec<u32>>, k: K, id: u32) -> usize {
        let table = table_bytes(map.capacity(), 32);
        let v = map.entry(k).or_default();
        if v.last() == Some(&id) {
            return 0;
        }
        let cap = v.capacity();
        v.push(id);
        let list = (v.capacity() - cap) * 4;
        table_bytes(map.capacity(), 32).saturating_sub(table) + list
    }
}

/// A role's inverse, the qualified roles' included, for the literals of the root.
#[inline]
fn inv_of<'a>(ix: &'a Index, dx: &'a DlIx) -> impl Fn(Role) -> Option<Role> + Copy + Sync + 'a {
    move |r| dx.inv(ix, r)
}

/// The literal of `l`, which in the root is never true or false but for an equality or
/// inequality of a term with itself.
#[inline]
fn lit<S: Sink>(ix: &Index, out: &S, l: XLit) -> Made {
    out.dl().lits.lit(l, inv_of(ix, dix(ix)))
}

/// The literal of a ground atom `a(u)`.
#[inline]
fn atom<S: Sink>(ix: &Index, out: &S, a: Ex, u: u32) -> Lit {
    match lit(ix, out, XLit::Atom(a, RTerm::C(u))) {
        Made::Lit(l) => l,
        _ => unreachable!("an atom is a literal"),
    }
}

/// Starts the root: `⊤ → N_u(u)` for every constant u, and what the ontology's clauses with
/// the body `owl:Thing` give u.
pub fn init<S: Sink>(ix: &Index, st: &mut State, out: &mut S) {
    st.init = true;
    root_of(ds(st), out);
    let mut found = Found::default();
    for &(u, n) in &dix(ix).consts {
        found.push((Vec::new(), vec![atom(ix, out, n, u)]));
        hyper_at(ix, st, out, TOP, u, &unit(), &mut found, false);
    }
    drain_with(ix, st, found, out, derive);
}

/// Runs the root's rules on up to `budget` clauses of its store, as
/// [`super::dl_rules::flush`] does for the other contexts.
pub fn flush<S: Sink>(ix: &Index, st: &mut State, out: &mut S, budget: usize) -> usize {
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let mut n = 0;
    let mut max: Vec<Lit> = Vec::new();
    while n < budget {
        if st.bottom {
            break;
        }
        let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) else {
            break;
        };
        let id = {
            let o = Rooted {
                ix,
                lits: &out.dl().lits,
                inv: &inv,
                ground_total: super::dl_clause::ground_total(),
            };
            x.store.next(ix, Some(&o), &mut max)
        };
        let Some(id) = id else {
            break;
        };
        let grown = x.store.take_grown();
        if grown > 0 {
            out.dl().grow(grown);
        }
        index_consts(ix, x, &max, id, out);
        n += 1;
        let c = x.store.clauses[id as usize].c.clone();
        process(ix, st, &c, &max, out);
        offer(ix, st, id, &c, out);
        if out.stopped() {
            break;
        }
    }
    n
}

/// Lists the processed clause `id` of a context by the constants of its maximal literals
/// `max`, in the context's indexes by constant, before the rules so that a clause meets
/// itself.
pub(super) fn index_consts<S: Sink>(
    ix: &Index,
    x: &mut super::dl_rules::DlSt,
    max: &[Lit],
    id: u32,
    out: &S,
) {
    let inv = inv_of(ix, dix(ix));
    let mut grown = 0;
    let r = root_of(x, out);
    for &l in max {
        match out.dl().lits.view(l, inv) {
            XLit::Atom(_, RTerm::C(u)) => grown += RootSt::push(&mut r.by_const, u, id),
            XLit::Role(_, a, b) => {
                for t in [a, b] {
                    if let RTerm::C(u) = t {
                        grown += RootSt::push(&mut r.roles_of, u, id);
                    }
                }
            }
            _ => {}
        }
    }
    if grown > 0 {
        out.dl().grow(grown);
    }
}

/// The rules on one clause of the root's store, whose maximal literals are `max`.
fn process<S: Sink>(ix: &Index, st: &mut State, c: &Clause, max: &[Lit], out: &mut S) {
    let dx = dix(ix);
    let me = dx.root.expect("a rooted run");
    let mut found = Found::default();
    join_body(ix, st, c, &mut found, &out.dl().lits);
    for &l in max {
        let prem = side(c, l);
        let q = super::dl_eq::Eqs {
            ix,
            dx,
            lits: &out.dl().lits,
            root: true,
            query: None,
        };
        q.on_max(st, l, &prem, &mut found, false);
        join_head(ix, st, c, l, &prem, &mut found, &out.dl().lits, false);
        let v = out.dl().lits.view(l, inv_of(ix, dx));
        if ground_trigger(v) {
            ground_out(ix, st, l, out);
        }
        at_consts(ix, me, st, l, v, &prem, &mut found, out, false);
    }
    drain_with(ix, st, found, out, derive);
}

/// The rules at the constants on a new premise `prem` of the context `me` whose maximal
/// literal is `l`, viewed as `v`: Succ over each successor term `f_e(u)` of v, with the
/// markers of an atom over it and Pred with the clauses the successor already sent, Hyper
/// with x bound to u on a ground atom `B(u)`, and the universal restrictions of the
/// constant's holders on a role atom with a constant at an end.
#[allow(clippy::too_many_arguments)]
fn at_consts<S: Sink>(
    ix: &Index,
    me: Ex,
    st: &mut State,
    l: Lit,
    v: XLit,
    prem: &Side,
    found: &mut Found,
    out: &mut S,
    sel: bool,
) {
    let dx = dix(ix);
    let (ts, n) = terms_of(v);
    for k in 0..n {
        let RTerm::Fc(e, u) = ts[k] else { continue };
        if ts[..k].contains(&ts[k]) {
            continue;
        }
        let w = succ_at(ix, me, st, e, u, found, out);
        if let XLit::Atom(a, _) = v {
            super::dl_rules::send_hyp(ix, st, w, a, out);
        }
        let cids = dl(st)
            .and_then(|x| x.pred_in.get(&w))
            .cloned()
            .unwrap_or_default();
        for cid in cids {
            let c = out.dl().table.get(cid);
            root_pred(ix, st, e, u, c, Some((l, prem)), found, &out.dl().lits);
        }
    }
    match v {
        XLit::Atom(d, RTerm::C(u)) => hyper_at(ix, st, out, d, u, prem, found, sel),
        v @ XLit::Role(..) => {
            for (u, s, t) in from(v, inv_of(ix, dx)) {
                on_role(ix, st, out, s, u, t, prem, found, sel);
            }
        }
        _ => {}
    }
}

/// The premises of a ground inference whose maximal literal is `l`, the selected clauses
/// and the markers left out.
fn partners(st: &State, l: Lit) -> Vec<Side> {
    let Some(d) = dl(st) else {
        return Vec::new();
    };
    d.store
        .by_xl
        .get(&l)
        .into_iter()
        .flatten()
        .filter(|&&i| {
            let s = &d.store.clauses[i as usize];
            !s.dead && !s.sel
        })
        .map(|&i| side(&d.store.clauses[i as usize].c, l))
        .collect()
}

/// The premises of `a(u)`: the unit for `owl:Thing`, which holds of every constant, and the
/// root's clauses otherwise.
fn partners_at<S: Sink>(ix: &Index, st: &State, out: &S, a: Ex, u: u32) -> Vec<Side> {
    if a == TOP {
        return vec![unit()];
    }
    partners(st, atom(ix, out, a, u))
}

/// Adds the conclusion `s` of a ground inference, unless the restriction on ground
/// inferences leaves it out.
fn put<S: Sink>(ix: &Index, out: &S, found: &mut Found, s: Side) {
    if !restricted(&out.dl().lits, inv_of(ix, dix(ix)), &s) {
        found.push(s);
    }
}

/// Hyper on a new premise `prem` of `d(u)`: the ontology clauses with d in the body, with
/// their x bound to u. A conclusion is a ground inference's unless it holds x or y. A
/// selected premise, a marker included, takes only the universals over the role atoms
/// from u that are local to the context ([`super::dl_rules::local`]).
#[allow(clippy::too_many_arguments)]
fn hyper_at<S: Sink>(
    ix: &Index,
    st: &State,
    out: &S,
    d: Ex,
    u: u32,
    prem: &Side,
    found: &mut Found,
    sel: bool,
) {
    let dx = dix(ix);
    if sel {
        hyper_roles(ix, st, out, d, u, prem, found, sel);
        return;
    }
    let at = |a: Ex| atom(ix, out, a, u);
    let h = ix.head(d);
    match h.node {
        Node::Conj(a, b) => {
            put(ix, out, found, with(prem, at(a)));
            put(ix, out, found, with(prem, at(b)));
        }
        Node::Exist(r, _) => {
            let f = RTerm::Fc(d, u);
            for l in [XLit::Role(r, RTerm::C(u), f), XLit::Atom(h.succ, f)] {
                if let Made::Lit(l) = lit(ix, out, l) {
                    put(ix, out, found, with(prem, l));
                }
            }
        }
        // N_v(x) → x ≈ v
        Node::Nominal(v) if v != u => {
            if let Made::Lit(l) = lit(ix, out, XLit::Eq(RTerm::C(v), RTerm::C(u))) {
                put(ix, out, found, with(prem, l));
            }
        }
        _ => {}
    }
    // M(x) → f_i(x) ≉ f_j(x) of a minimum's holder
    if let Some(es) = dx.ne_of.get(&d) {
        for (i, &a) in es.iter().enumerate() {
            for &b in &es[i + 1..] {
                if let Made::Lit(n) = lit(ix, out, XLit::Ne(RTerm::Fc(a, u), RTerm::Fc(b, u))) {
                    put(ix, out, found, with(prem, n));
                }
            }
        }
    }
    for &t in ix.told_of(h) {
        put(ix, out, found, with(prem, at(t)));
    }
    for &(p, k) in ix.conj_of(h) {
        for q in partners_at(ix, st, out, p, u) {
            put(ix, out, found, with(&merge(prem, &q), at(k)));
        }
    }
    for &set in ix.disjoint_of(h) {
        for &m in &dx.sets[set as usize] {
            if m != d {
                for q in partners_at(ix, st, out, m, u) {
                    put(ix, out, found, merge(prem, &q));
                }
            }
        }
    }
    if let Some(ids) = dx.by_body.get(&d) {
        for &cl in ids {
            let (body, head) = &dx.clauses[cl as usize];
            let lists: Vec<Vec<Side>> = body
                .iter()
                .map(|&a| {
                    if a == d {
                        vec![prem.clone()]
                    } else {
                        partners_at(ix, st, out, a, u)
                    }
                })
                .collect();
            let base: Side = (Vec::new(), head.iter().map(|&a| at(a)).collect());
            found.ground_products(lists, base);
        }
    }
    hyper_roles(ix, st, out, d, u, prem, found, sel);
}

/// The universals `d(u)` stands for, over the role atoms from u in the context. A role atom
/// whose other end is a constant or a successor term of one makes a ground inference, which
/// takes no selected premise, and any other is local and takes every premise.
#[allow(clippy::too_many_arguments)]
fn hyper_roles<S: Sink>(
    ix: &Index,
    st: &State,
    out: &S,
    d: Ex,
    u: u32,
    prem: &Side,
    found: &mut Found,
    sel: bool,
) {
    let dx = dix(ix);
    let mut ps = Vec::new();
    alls(ix, d, &mut ps);
    if ps.is_empty() {
        return;
    }
    let Some(x) = dl(st) else { return };
    let Some(r) = x.root.as_deref() else { return };
    let inv = inv_of(ix, dx);
    for &i in r.roles_of.get(&u).into_iter().flatten() {
        let s = &x.store.clauses[i as usize];
        if s.dead {
            continue;
        }
        for &l in x.store.maxima(i) {
            for (w, t, f) in from(out.dl().lits.view(l, inv), inv) {
                if w != u {
                    continue;
                }
                if matches!(f, RTerm::C(_) | RTerm::Fc(..)) && (sel || s.sel) {
                    continue;
                }
                for &(a, b) in &ps {
                    if dx.sub(ix, t, a)
                        && let Made::Lit(m) = lit(ix, out, XLit::Atom(b, f))
                    {
                        put(ix, out, found, with(&merge(prem, &side(&s.c, l)), m));
                    }
                }
            }
        }
    }
}

/// The role atom `v` read from each constant u at one of its ends: `(u, s, t)` for
/// `s(u, t)`, by the inverse role when u is its second term.
fn from(v: XLit, inv: impl Fn(Role) -> Option<Role>) -> Vec<(u32, Role, RTerm)> {
    let mut out = Vec::new();
    if let XLit::Role(s, a, b) = v {
        if let RTerm::C(u) = a {
            out.push((u, s, b));
        }
        if let RTerm::C(u) = b
            && let Some(i) = inv(s)
        {
            out.push((u, i, a));
        }
    }
    out
}

/// Hyper on a new premise `prem` of the role atom `s(u, f)`: the universals of the holders
/// of u, `owl:Thing` included, and the disjoint roles. The inference is ground when f is a
/// constant or a successor term of one, and the restriction then applies and no selected
/// premise takes part; otherwise it is local and every premise takes part.
#[allow(clippy::too_many_arguments)]
fn on_role<S: Sink>(
    ix: &Index,
    st: &State,
    out: &S,
    s: Role,
    u: u32,
    f: RTerm,
    prem: &Side,
    found: &mut Found,
    sel: bool,
) {
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let ground = matches!(f, RTerm::C(_) | RTerm::Fc(..));
    if ground && sel {
        return;
    }
    let add = |found: &mut Found, c: Side| {
        if !ground || !restricted(&out.dl().lits, inv, &c) {
            found.push(c);
        }
    };
    let mut ps = Vec::new();
    let over = |b: Ex, q: &Side, ps: &mut Vec<(Role, Ex)>, found: &mut Found| {
        alls(ix, b, ps);
        for &(t, c) in ps.iter() {
            if dx.sub(ix, s, t)
                && let Made::Lit(m) = lit(ix, out, XLit::Atom(c, f))
            {
                add(found, with(&merge(prem, q), m));
            }
        }
    };
    over(TOP, &unit(), &mut ps, found);
    let Some(x) = dl(st) else { return };
    if let Some(r) = x.root.as_deref() {
        for &i in r.by_const.get(&u).into_iter().flatten() {
            let c = &x.store.clauses[i as usize];
            if c.dead || ground && c.sel {
                continue;
            }
            for &l in x.store.maxima(i) {
                if let XLit::Atom(b, RTerm::C(w)) = out.dl().lits.view(l, inv)
                    && w == u
                {
                    over(b, &side(&c.c, l), &mut ps, found);
                }
            }
        }
        for &(a, b) in &dx.rdis {
            if !dx.sub(ix, s, a) {
                continue;
            }
            for &i in r.roles_of.get(&u).into_iter().flatten() {
                let c = &x.store.clauses[i as usize];
                if c.dead || ground && c.sel {
                    continue;
                }
                for &l in x.store.maxima(i) {
                    for (w, t, g) in from(out.dl().lits.view(l, inv), inv) {
                        if w == u && g == f && dx.sub(ix, t, b) {
                            add(found, merge(prem, &side(&c.c, l)));
                        }
                    }
                }
            }
        }
    }
}

/// Adds a clause the root's rules derived: the empty clause makes the root unsatisfiable,
/// and any other clause goes to the store unless it is redundant. The root has no units
/// over x, so even a unit is stored.
pub(super) fn derive<S: Sink>(ix: &Index, st: &mut State, (mut body, mut head): Side, out: &mut S) {
    if st.bottom {
        return;
    }
    out.dl().work.fetch_add(1, Ordering::Relaxed);
    let inv = inv_of(ix, dix(ix));
    let lits = &out.dl().lits;
    body.retain(|&l| !matches!(lits.view(l, inv), XLit::Atom(TOP, _)));
    let mut holds = false;
    head.retain(|&l| match lits.view(l, inv) {
        XLit::Atom(BOTTOM, _) => false,
        XLit::Atom(TOP, _) => {
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
    // Γ ∧ A → Δ ∨ A and Γ → Δ ∨ s ≈ t ∨ s ≉ t
    if head.iter().any(|l| body.binary_search(l).is_ok()) || lits.complementary(&head, inv) {
        return;
    }
    if body.is_empty() && head.is_empty() {
        out.me(Concl::Sub(BOTTOM));
        return;
    }
    if let Some(bytes) = ds(st).store.add(Clause::new(body, head)) {
        out.dl().grow(bytes);
    }
}

/// Whether a clause is a marker `A → A`.
fn is_marker(c: &Clause) -> bool {
    c.body().len() == 1 && c.body() == c.head()
}

/// Join on a new premise `prem` of the clause `c`, whose maximal literal is `l`: when l is a
/// ground atom or an equality of constants, the processed clauses with l in the body. A
/// marker is no premise on either side, since its conclusion would be the other premise,
/// and a selected clause is no premise on the side of the head, as ordered resolution
/// with selection has it. A Join of two clauses that hold neither x nor y is a ground
/// inference, and its conclusion then holds neither, so the restriction applies to the
/// conclusion.
#[allow(clippy::too_many_arguments)]
pub(super) fn join_head(
    ix: &Index,
    st: &State,
    c: &Clause,
    l: Lit,
    prem: &Side,
    found: &mut Found,
    lits: &super::dl_clause::Lits,
    sel: bool,
) {
    if is_marker(c) || sel {
        return;
    }
    let inv = inv_of(ix, dix(ix));
    let Some(x) = dl(st) else { return };
    for &i in x.store.by_ground.get(&l).into_iter().flatten() {
        let s = &x.store.clauses[i as usize];
        if s.dead || is_marker(&s.c) {
            continue;
        }
        let rest: Side = (
            s.c.body().iter().copied().filter(|&m| m != l).collect(),
            s.c.head().to_vec(),
        );
        let c = merge(prem, &rest);
        if !restricted(lits, inv, &c) {
            found.push(c);
        }
    }
}

/// Join on a new clause `c` with ground literals in its body: for each, the processed
/// clauses where it is maximal.
pub(super) fn join_body(
    ix: &Index,
    st: &State,
    c: &Clause,
    found: &mut Found,
    lits: &super::dl_clause::Lits,
) {
    if is_marker(c) {
        return;
    }
    let inv = inv_of(ix, dix(ix));
    for &l in c.body() {
        if l.kind() != XL || !lits.view(l, inv).is_ground() {
            continue;
        }
        let rest: Side = (
            c.body().iter().copied().filter(|&m| m != l).collect(),
            c.head().to_vec(),
        );
        let Some(x) = dl(st) else { return };
        for &i in x.store.by_xl.get(&l).into_iter().flatten() {
            let s = &x.store.clauses[i as usize];
            if !s.dead && !s.sel && !is_marker(&s.c) {
                let c = merge(&side(&s.c, l), &rest);
                if !restricted(lits, inv, &c) {
                    found.push(c);
                }
            }
        }
    }
}

/// The constants of a literal, inside `f_e(u)` included.
fn consts(v: XLit, out: &mut Vec<u32>) {
    let (ts, n) = terms_of(v);
    for t in &ts[..n] {
        if let RTerm::C(u) | RTerm::Fc(_, u) = t {
            out.push(*u);
        }
    }
}

/// The rules of a context other than the root on a new premise `prem` of the clause `c`,
/// whose maximal literal is the interned literal `l`: the context registers with the root
/// for the constants of l, sends its successors the marker of a ground l, resolves a ground
/// l with the clauses that have it in the body (Join), and runs the rules at the constants
/// of l, Hyper with x bound to a constant and Succ over `f_e(u)`, with its own clauses.
#[allow(clippy::too_many_arguments)]
pub(super) fn on_max<S: Sink>(
    ix: &Index,
    me: Ex,
    st: &mut State,
    c: &Clause,
    l: Lit,
    prem: &Side,
    found: &mut Found,
    out: &mut S,
    sel: bool,
) {
    let dx = dix(ix);
    let v = out.dl().lits.view(l, inv_of(ix, dx));
    if let Some(root) = dx.root {
        let mut us = Vec::new();
        consts(v, &mut us);
        edge_to(root, me, st, us, out);
    }
    if ground_trigger(v) {
        ground_out(ix, st, l, out);
    }
    join_head(ix, st, c, l, prem, found, &out.dl().lits, sel);
    at_consts(ix, me, st, l, v, prem, found, out, sel);
}

/// A context other than the root takes a clause the root sent, a processed root clause with
/// an empty body over one of the constants the context registered for, once.
pub fn rpred_c<S: Sink>(ix: &Index, st: &mut State, cid: u32, out: &mut S) -> bool {
    if st.bottom {
        return false;
    }
    let r = root_of(ds(st), out);
    match RootSt::ins(&mut r.seen, cid, 4) {
        Some(b) => out.dl().grow(b),
        None => return false,
    }
    let c = out.dl().table.get(cid);
    debug_assert!(c.body().is_empty(), "the root sends its facts");
    let mut found = Found::default();
    found.push((Vec::new(), c.head().to_vec()));
    drain(ix, st, found, out);
    true
}

/// The context v registers with the root for the constant u, which a maximal literal of
/// one of its clauses or a ground literal in one of its bodies mentions.
pub fn redge<S: Sink>(st: &mut State, v: Ex, u: u32, out: &mut S) -> bool {
    register(st, v, vec![u], out)
}

/// The root registers v for the constants `us`, and v gets the root's facts over the new
/// ones.
fn register<S: Sink>(st: &mut State, v: Ex, mut us: Vec<u32>, out: &mut S) -> bool {
    us.sort_unstable();
    us.dedup();
    let bottom = st.bottom;
    let x = ds(st);
    let r = root_of(x, out);
    let mut grown = 0;
    let mut new = Vec::new();
    for &u in &us {
        if let Some(b) = RootSt::ins(&mut r.edge_set, (v, u), 8) {
            grown += b + RootSt::push(&mut r.edges, u, v);
            new.push(u);
        }
    }
    if grown > 0 {
        out.dl().grow(grown);
    }
    if new.is_empty() {
        return false;
    }
    // an unsatisfiable root sends the empty clause to every context registered with it
    if bottom {
        let id = out.dl().empty();
        out.send(v, Concl::RPred(id));
        return true;
    }
    let mut ids: Vec<u32> = Vec::new();
    {
        let r = x.root.as_deref().expect("made above");
        for &u in &new {
            for &i in r.ready.get(&u).into_iter().flatten() {
                if !x.store.clauses[i as usize].dead {
                    ids.push(i);
                }
            }
        }
    }
    ids.sort_unstable();
    ids.dedup();
    for i in ids {
        if let Some(cid) = pred_id(x, i, out) {
            out.send(v, Concl::RPred(cid));
        }
    }
    true
}

/// A processed root clause with an empty body goes to the contexts registered for one of
/// the constants of its head, and is listed for the contexts that register later.
fn offer<S: Sink>(ix: &Index, st: &mut State, id: u32, c: &Clause, out: &mut S) {
    if st.bottom || !c.body().is_empty() {
        return;
    }
    let inv = inv_of(ix, dix(ix));
    let lits = &out.dl().lits;
    let mut keys = Vec::new();
    for &l in c.head() {
        consts(lits.view(l, inv), &mut keys);
    }
    keys.sort_unstable();
    keys.dedup();
    let x = ds(st);
    let r = root_of(x, out);
    let mut grown = 0;
    for &u in &keys {
        grown += RootSt::push(&mut r.ready, u, id);
    }
    if grown > 0 {
        out.dl().grow(grown);
    }
    let mut to: Vec<Ex> = keys
        .iter()
        .flat_map(|u| r.edges.get(u).into_iter().flatten().copied())
        .collect();
    to.sort_unstable();
    to.dedup();
    if to.is_empty() {
        return;
    }
    let Some(cid) = pred_id(x, id, out) else {
        return;
    };
    for v in to {
        out.send(v, Concl::RPred(cid));
    }
}

/// The root became unsatisfiable: every context registered with it takes the empty clause.
pub(super) fn bottom<S: Sink>(st: &State, out: &mut S) {
    let Some(r) = dl(st).and_then(|x| x.root.as_deref()) else {
        return;
    };
    if r.edges.is_empty() {
        return;
    }
    let mut to: Vec<Ex> = r.edge_set.iter().map(|&(v, _)| v).collect();
    to.sort_unstable();
    to.dedup();
    let id = out.dl().empty();
    for v in to {
        out.send(v, Concl::RPred(id));
    }
}

/// Whether a literal is a successor trigger of Definition 4 that is ground: an atom or a
/// role atom over constants, or an equality of constants. Every predicate is taken as one
/// that a body of the ontology mentions.
fn ground_trigger(v: XLit) -> bool {
    use RTerm::C;
    matches!(
        v,
        XLit::Atom(_, C(_)) | XLit::Role(_, C(_), C(_)) | XLit::Eq(C(_), C(_))
    )
}

/// The context's state for the rules that involve the root, made with its first use and
/// counted then.
fn rst<'a, S: Sink>(st: &'a mut State, out: &S) -> &'a mut RootSt {
    root_of(ds(st), out)
}

/// [`rst`] for a context's state for Tier D.
fn root_of<'a, S: Sink>(x: &'a mut super::dl_rules::DlSt, out: &S) -> &'a mut RootSt {
    if x.root.is_none() {
        out.dl().grow(std::mem::size_of::<RootSt>());
    }
    x.root.get_or_insert_with(Box::default)
}

/// The successors of a context: those it registered with over `f_e(x)` and over `f_e(u)`,
/// and the EL successors of its unit existential restrictions.
fn successors(ix: &Index, st: &State) -> Vec<Ex> {
    let mut v: Vec<Ex> = Vec::new();
    if let Some(x) = dl(st) {
        v.extend(x.edges.iter().map(|&(_, w)| w));
        if let Some(r) = x.root.as_deref() {
            v.extend(r.by_succ.keys().copied());
        }
    }
    for &d in &st.subs {
        let h = ix.head(d);
        if let Node::Exist(..) = h.node {
            v.push(h.succ);
        }
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// Succ's ground triggers: a new maximal ground atom or equality of constants of the
/// context goes to each of its successors as a marker.
fn ground_out<S: Sink>(ix: &Index, st: &mut State, l: Lit, out: &mut S) {
    let r = rst(st, out);
    match RootSt::ins(&mut r.ground_set, l, 8) {
        Some(b) => {
            let cap = r.ground.capacity();
            r.ground.push(l);
            out.dl().grow(b + (r.ground.capacity() - cap) * 8);
        }
        None => return,
    }
    for w in successors(ix, st) {
        send_ground(st, w, l, out);
    }
}

/// Sends the successor w the marker of the ground literal l, once.
fn send_ground<S: Sink>(st: &mut State, w: Ex, l: Lit, out: &mut S) {
    let r = rst(st, out);
    if let Some(b) = RootSt::ins(&mut r.gsent, (w, l), 16) {
        out.dl().grow(b);
        out.send(w, Concl::HypL(l.xid()));
    }
}

/// A new successor w of the context takes the markers of the context's maximal ground
/// literals.
pub(super) fn new_succ<S: Sink>(st: &mut State, w: Ex, out: &mut S) {
    let Some(ls) = dl(st)
        .and_then(|x| x.root.as_deref())
        .map(|r| r.ground.clone())
    else {
        return;
    };
    for l in ls {
        send_ground(st, w, l, out);
    }
}

/// The marker `l → l` of a ground literal that a predecessor holds as a maximal literal.
pub fn hyp_l<S: Sink>(st: &mut State, xid: u32, out: &mut S) -> bool {
    if st.bottom {
        return false;
    }
    let l = Lit::xl(xid);
    let r = rst(st, out);
    let Some(b) = RootSt::ins(&mut r.gmarks, l, 8) else {
        return false;
    };
    out.dl().grow(b);
    if let Some(b) = ds(st).store.add(Clause::new(vec![l], vec![l])) {
        out.dl().grow(b);
    }
    true
}

/// A context other than the root registers with the root for the constants of the ground
/// literals in the body of its new clause c.
pub(super) fn register_body<S: Sink>(ix: &Index, me: Ex, st: &mut State, c: &Clause, out: &mut S) {
    let dx = dix(ix);
    let Some(root) = dx.root else { return };
    let inv = inv_of(ix, dx);
    let mut us = Vec::new();
    for &l in c.body() {
        if l.kind() == XL {
            let v = out.dl().lits.view(l, inv);
            if v.is_ground() {
                consts(v, &mut us);
            }
        }
    }
    edge_to(root, me, st, us, out);
}

/// The context `me`, other than the root, registers with the root for the constants
/// `us`, once each.
fn edge_to<S: Sink>(root: Ex, me: Ex, st: &mut State, us: Vec<u32>, out: &mut S) {
    if us.is_empty() {
        return;
    }
    let r = rst(st, out);
    for u in us {
        if let Some(b) = RootSt::ins(&mut r.edged, u, 4) {
            out.dl().grow(b);
            out.send(root, Concl::REdge(me, u));
        }
    }
}

/// Succ in the context `me` for the successor term `f_e(u)`: the successor is e's
/// hypothetical successor, which has the empty core. A new edge registers the context with
/// it, sends it the markers of the context's ground literals when it is a new successor,
/// and replays the clauses it already sent for the new constant.
fn succ_at<S: Sink>(
    ix: &Index,
    me: Ex,
    st: &mut State,
    e: Ex,
    u: u32,
    found: &mut Found,
    out: &mut S,
) -> Ex {
    let dx = dix(ix);
    let w = dx.hyp.get(&e).copied().unwrap_or(TOP);
    let r = rst(st, out);
    let Some(b) = RootSt::ins(&mut r.succ_set, (e, u), 8) else {
        return w;
    };
    let table = table_bytes(r.by_succ.capacity(), 32);
    let fresh = !r.by_succ.contains_key(&w);
    let list = r.by_succ.entry(w).or_default();
    let cap = list.capacity();
    list.push((e, u));
    let grown = b
        + (list.capacity() - cap) * 8
        + table_bytes(r.by_succ.capacity(), 32).saturating_sub(table);
    out.dl().grow(grown);
    out.send(w, Concl::DEdge(me, e));
    if fresh {
        new_succ(st, w, out);
    }
    let cids = dl(st)
        .and_then(|x| x.pred_in.get(&w))
        .cloned()
        .unwrap_or_default();
    for cid in cids {
        root_pred(
            ix,
            st,
            e,
            u,
            out.dl().table.get(cid),
            None,
            found,
            &out.dl().lits,
        );
    }
    w
}

/// Pred along the edge over `f_e(u)` with the clause `c` of its successor, in any context:
/// y is read as u and x as `f_e(u)`. Each body atom `B(x)` takes a premise whose maximal
/// literal is `B(f_e(u))`, each role atom `s(y, x)` one whose maximal literal is a role
/// atom below s from u to `f_e(u)`, and the ground body literals stay. `fixed` is the
/// premise that a new clause gives for one of these literals. The successor's core is
/// empty, so no core atom is resolved.
#[allow(clippy::too_many_arguments)]
pub(super) fn root_pred(
    ix: &Index,
    st: &State,
    e: Ex,
    u: u32,
    c: &Clause,
    fixed: Option<(Lit, &Side)>,
    found: &mut Found,
    lits: &super::dl_clause::Lits,
) {
    use super::dl_clause::{RY, X as KX};
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let f = RTerm::Fc(e, u);
    let at = RTerm::C(u);
    let map = |t: RTerm| match t {
        RTerm::X => Some(f),
        RTerm::Y => Some(at),
        RTerm::C(_) => Some(t),
        _ => None,
    };
    // the premises: an atom over f_e(u), or a role below s from u to f_e(u)
    enum Need {
        Atom(Lit),
        Role(Role),
    }
    let mut need = Vec::new();
    let mut ground = Vec::new();
    for &l in c.body() {
        match l.kind() {
            KX => match lits.lit(XLit::Atom(l.atom(), f), inv) {
                Made::Lit(k) => need.push(Need::Atom(k)),
                _ => return,
            },
            RY => need.push(Need::Role(l.atom())),
            XL if lits.view(l, inv).is_ground() => ground.push(l),
            _ => return,
        }
    }
    let role_of = |p: Lit| match lits.view(p, inv) {
        XLit::Role(t, a, b) if a == at && b == f => Some(t),
        XLit::Role(t, a, b) if a == f && b == at => dx.inv(ix, t),
        _ => None,
    };
    let fits = |n: &Need, p: Lit| match n {
        Need::Atom(k) => *k == p,
        Need::Role(s) => role_of(p).is_some_and(|t| dx.sub(ix, t, *s)),
    };
    if let Some((p, _)) = fixed
        && !need.iter().any(|n| fits(n, p))
    {
        return;
    }
    let mut head = Vec::with_capacity(c.head().len());
    for &l in c.head() {
        let w = match lits.view(l, inv) {
            XLit::Atom(a, b) => map(b).map(|b| XLit::Atom(a, b)),
            XLit::Role(r, a, b) => map(a).zip(map(b)).map(|(a, b)| XLit::Role(r, a, b)),
            XLit::Eq(a, b) => map(a).zip(map(b)).map(|(a, b)| XLit::Eq(a, b)),
            XLit::Ne(a, b) => map(a).zip(map(b)).map(|(a, b)| XLit::Ne(a, b)),
        };
        match w.map(|w| lits.lit(w, inv)) {
            Some(Made::Lit(m)) => head.push(m),
            Some(Made::False) => {}
            Some(Made::True) | None => return,
        }
    }
    let roles = |s: Role| -> Vec<Side> {
        let Some(x) = dl(st) else { return Vec::new() };
        let Some(t) = x.store.terms.as_deref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for &i in t.any.get(&f).into_iter().flatten() {
            let sc = &x.store.clauses[i as usize];
            if sc.dead {
                continue;
            }
            for &m in x.store.maxima(i) {
                if role_of(m).is_some_and(|t| dx.sub(ix, t, s)) {
                    out.push(side(&sc.c, m));
                }
            }
        }
        out
    };
    let free: Vec<Vec<Side>> = need
        .iter()
        .map(|n| match n {
            Need::Atom(k) => partners(st, *k),
            Need::Role(s) => roles(*s),
        })
        .collect();
    let base: Side = (ground, head);
    match fixed {
        None => found.products(free, base),
        Some((p, prem)) => {
            for (k, n) in need.iter().enumerate() {
                if fits(n, p) {
                    let mut lists = free.clone();
                    lists[k] = vec![prem.clone()];
                    found.products(lists, base.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::dl_norm::DlAx;
    use super::super::dl_rules::DlRun;
    use super::super::index::{Axioms, Interner, RoleName};
    use super::super::saturate::{Bounds, Contexts, saturate};
    use super::*;
    use crate::model::{Class, NamedIndividual, ObjectProperty};
    use std::sync::OnceLock;

    /// A small signature: classes, individuals and roles by name.
    struct Sig {
        it: Interner,
        ax: Axioms,
        dl: DlAx,
    }

    impl Sig {
        fn new() -> Sig {
            Sig {
                it: Interner::new(),
                ax: Axioms::default(),
                dl: DlAx::default(),
            }
        }
        fn class(&mut self, n: &str) -> Ex {
            self.it.class(&Class::new(format!("urn:t:{n}")))
        }
        fn ind(&mut self, n: &str) -> Ex {
            self.it.nominal(&NamedIndividual::new(format!("urn:t:{n}")))
        }
        fn role(&mut self, n: &str) -> Role {
            self.it
                .role(RoleName::Object(ObjectProperty::new(format!("urn:t:{n}"))))
        }
        fn some(&mut self, r: Role, c: Ex) -> Ex {
            self.it.node(Node::Exist(r, c))
        }
        /// An existential restriction with its hypothetical successor, as normalization
        /// gives every existential restriction of a run.
        fn some_h(&mut self, r: Role, c: Ex) -> Ex {
            let e = self.some(r, c);
            let h = self.it.fresh_class();
            self.dl.hyp.insert(e, h);
            e
        }
        fn all(&mut self, r: Role, c: Ex) -> Ex {
            self.it.node(Node::Forall(r, c))
        }
        fn sub(&mut self, a: Ex, b: Ex) {
            self.ax.subs.push((a, b));
        }
    }

    /// The state after a saturation of one context, the root unless the test names
    /// another, as `root` here.
    struct Done {
        ix: Index,
        ctx: Contexts,
        root: Ex,
        run: DlRun,
    }

    impl Done {
        fn bottom(&self) -> bool {
            self.ctx[self.root as usize].get().unwrap().st.lock().bottom
        }
        /// The root's live clauses, each as its body and head in general form.
        fn clauses(&self) -> Vec<(Vec<XLit>, Vec<XLit>)> {
            self.clauses_of(self.root)
        }
        /// The live clauses of the context of `at`.
        fn clauses_of(&self, at: Ex) -> Vec<(Vec<XLit>, Vec<XLit>)> {
            let dx = dix(&self.ix);
            let inv = inv_of(&self.ix, dx);
            let Some(c) = self.ctx[at as usize].get() else {
                return Vec::new();
            };
            let st = c.st.lock();
            let Some(d) = dl(&st) else { return Vec::new() };
            d.store
                .clauses
                .iter()
                .filter(|s| !s.dead)
                .map(|s| {
                    let v = |ls: &[Lit]| ls.iter().map(|&l| self.run.lits.view(l, inv)).collect();
                    (v(s.c.body()), v(s.c.head()))
                })
                .collect()
        }
        /// Whether the root holds the clause with an empty body and this head, or one that
        /// subsumes it.
        fn has(&self, head: &[XLit]) -> bool {
            self.clauses()
                .iter()
                .any(|(b, h)| b.is_empty() && h.iter().all(|l| head.contains(l)))
        }
        /// Whether the context holds the unit `a(x)`.
        fn subs(&self, a: Ex) -> bool {
            self.ctx[self.root as usize]
                .get()
                .unwrap()
                .st
                .lock()
                .subs
                .contains(&a)
        }
    }

    /// In `before`, the root, which [`saturate_after`] makes.
    const ROOT: Ex = Ex::MAX;

    fn saturate_root(s: Sig) -> Done {
        saturate_at(s, None)
    }

    /// Saturates the context of `at`, or of the root.
    fn saturate_at(s: Sig, at: Option<Ex>) -> Done {
        saturate_seeded(s, at, |_, _| Vec::new())
    }

    /// Saturates as [`saturate_at`] does, with the root's store holding the clauses of
    /// `seed`, as body and head in general form, in place of its start.
    fn saturate_seeded(
        s: Sig,
        at: Option<Ex>,
        seed: impl FnOnce(&Index, &mut Interner) -> Vec<(Vec<XLit>, Vec<XLit>)>,
    ) -> Done {
        saturate_after(s, &[], at, seed)
    }

    /// Saturates as [`saturate_seeded`] does, after saturating the contexts of `before`
    /// one at a time.
    fn saturate_after(
        mut s: Sig,
        before: &[Ex],
        at: Option<Ex>,
        seed: impl FnOnce(&Index, &mut Interner) -> Vec<(Vec<XLit>, Vec<XLit>)>,
    ) -> Done {
        let root = s.it.fresh_class();
        s.dl.root = Some(root);
        for set in &mut s.ax.disjoint {
            set.sort_unstable();
        }
        s.ax.dl = Some(Box::new(std::mem::take(&mut s.dl)));
        let ix = Index::build(&s.ax, &mut s.it, None, &|| false).unwrap();
        let ctx: Contexts = (0..s.it.nodes.len()).map(|_| OnceLock::new()).collect();
        let run = DlRun::new(false, Default::default());
        let bounds = Bounds {
            threads: 1,
            deadline: None,
            max_memory: None,
            cancelled: None,
            charge: None,
            base_memory: 0,
            peak_memory: 0,
            dynamic: None,
            dl: Some(&run),
            partial: false,
        };
        let seeds = seed(&ix, &mut s.it);
        if !seeds.is_empty() {
            let dx = dix(&ix);
            let inv = inv_of(&ix, dx);
            let l = |v: &XLit| match run.lits.lit(*v, inv) {
                Made::Lit(l) => l,
                _ => panic!("a literal"),
            };
            // the seeds go to the first context saturated
            let to = match before.first().copied() {
                Some(ROOT) | None => root,
                Some(e) => e,
            };
            let c = ctx[to as usize].get_or_init(Box::default);
            let mut st = c.st.lock();
            st.init = true;
            let x = ds(&mut st);
            if to == root {
                x.root.get_or_insert_with(Box::default);
            }
            for (b, h) in &seeds {
                x.store.add(Clause::new(
                    b.iter().map(l).collect(),
                    h.iter().map(l).collect(),
                ));
            }
        }
        let root_ex = root;
        let root = at.unwrap_or(root);
        for &e in before {
            let e = if e == ROOT { root_ex } else { e };
            saturate(&ix, &ctx, &[e], &[], &[], &bounds).unwrap();
        }
        saturate(&ix, &ctx, &[root], &[], &[], &bounds).unwrap();
        Done { ix, ctx, root, run }
    }

    fn c(u: Ex, ix: &Index) -> RTerm {
        match ix.head(u).node {
            Node::Nominal(k) => RTerm::C(k),
            _ => panic!("not a nominal"),
        }
    }

    #[test]
    fn every_constant_exists_and_takes_the_global_axioms() {
        let mut s = Sig::new();
        let a = s.ind("a");
        let b = s.ind("b");
        let g = s.class("g");
        s.sub(TOP, g);
        let d = saturate_root(s);
        assert!(!d.bottom());
        for n in [a, b] {
            let u = c(n, &d.ix);
            assert!(d.has(&[XLit::Atom(n, u)]), "N_u(u) for {n}");
            assert!(d.has(&[XLit::Atom(g, u)]), "⊤ ⊑ G gives G(u) for {n}");
        }
    }

    #[test]
    fn an_assertion_through_a_disjunction_can_clash() {
        // A(a), A ⊑ B ⊔ C, B ⊑ D, C ⊑ D, A ⊓ D ⊑ ⊥
        let mut s = Sig::new();
        let na = s.ind("a");
        let [a, b, cc, dd] = ["A", "B", "C", "D"].map(|n| s.class(n));
        s.sub(na, a);
        s.sub(b, dd);
        s.sub(cc, dd);
        s.ax.disjoint.push(vec![a, dd]);
        s.dl.clauses.push((vec![a], vec![b, cc]));
        let d = saturate_root(s);
        assert!(d.bottom(), "the root derives the empty clause");
    }

    #[test]
    fn an_assertion_through_a_disjunction_keeps_both_cases() {
        // A(a), A ⊑ B ⊔ C: B(a) ∨ C(a), and no B(a) or C(a) alone
        let mut s = Sig::new();
        let na = s.ind("a");
        let [a, b, cc] = ["A", "B", "C"].map(|n| s.class(n));
        s.sub(na, a);
        s.dl.clauses.push((vec![a], vec![b, cc]));
        let d = saturate_root(s);
        assert!(!d.bottom());
        let u = c(na, &d.ix);
        assert!(d.has(&[XLit::Atom(b, u), XLit::Atom(cc, u)]));
        assert!(!d.has(&[XLit::Atom(b, u)]));
        assert!(!d.has(&[XLit::Atom(cc, u)]));
    }

    #[test]
    fn a_conjunction_of_ground_atoms_meets_at_one_constant() {
        // A(a), B(a), B(b), A ⊓ B ⊑ E: E(a) and not E(b)
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let [a, b, e] = ["A", "B", "E"].map(|n| s.class(n));
        let ab = s.it.conj(a, b);
        s.sub(na, a);
        s.sub(na, b);
        s.sub(nb, b);
        s.sub(ab, e);
        let d = saturate_root(s);
        assert!(d.has(&[XLit::Atom(e, c(na, &d.ix))]));
        assert!(!d.has(&[XLit::Atom(e, c(nb, &d.ix))]));
    }

    #[test]
    fn an_existential_meets_the_universals_in_either_order() {
        // {a} ⊑ ∃r.E, ⊤ ⊑ ∀r.G, {a} ⊑ ∀r.H: r(a, f(a)), E(f(a)), G(f(a)), H(f(a))
        let mut s = Sig::new();
        let na = s.ind("a");
        let [e, g, h] = ["E", "G", "H"].map(|n| s.class(n));
        let r = s.role("r");
        let some = s.some(r, e);
        let all_g = s.all(r, g);
        let all_h = s.all(r, h);
        s.sub(na, some);
        s.sub(TOP, all_g);
        s.sub(na, all_h);
        let d = saturate_root(s);
        let u = c(na, &d.ix);
        let f = RTerm::Fc(
            some,
            match u {
                RTerm::C(k) => k,
                _ => unreachable!(),
            },
        );
        assert!(d.has(&[XLit::Role(r, u, f)]));
        for x in [e, g, h] {
            assert!(d.has(&[XLit::Atom(x, f)]), "{x} over f(a)");
        }
    }

    #[test]
    fn a_universal_over_a_disjunction_keeps_its_case() {
        // {a} ⊑ ∃r.E, {a} ⊑ A ⊔ B, A ⊑ ∀r.H: A(a) ∨ B(a) gives H(f(a)) ∨ B(a), with A
        // declared after B so that A(a) is the maximal literal under the shared ground
        // order of item 804
        let mut s = Sig::new();
        let na = s.ind("a");
        let [e, b, a, h] = ["E", "B", "A", "H"].map(|n| s.class(n));
        let r = s.role("r");
        let some = s.some(r, e);
        let all_h = s.all(r, h);
        s.sub(na, some);
        s.sub(a, all_h);
        s.dl.clauses.push((vec![na], vec![a, b]));
        let d = saturate_root(s);
        let u = c(na, &d.ix);
        let RTerm::C(k) = u else { unreachable!() };
        let f = RTerm::Fc(some, k);
        assert!(d.has(&[XLit::Atom(h, f), XLit::Atom(b, u)]));
        assert!(!d.has(&[XLit::Atom(h, f)]));
    }

    #[test]
    fn disjoint_roles_clash_over_one_successor_term() {
        // {a} ⊑ ∃r.E, r ⊑ s, r and s disjoint
        let mut s = Sig::new();
        let na = s.ind("a");
        let e = s.class("E");
        let r = s.role("r");
        let t = s.role("s");
        let some = s.some(r, e);
        s.sub(na, some);
        s.ax.role_subs.push((r, t));
        s.dl.rdis.push((r, t));
        let d = saturate_root(s);
        assert!(d.bottom());
    }

    #[test]
    fn a_singleton_below_another_gives_an_equality_of_constants() {
        // {a} ⊑ {b}: a ≈ b
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        s.sub(na, nb);
        let d = saturate_root(s);
        let (ua, ub) = (c(na, &d.ix), c(nb, &d.ix));
        assert!(d.has(&[XLit::Eq(ua.min(ub), ua.max(ub))]));
        assert!(!d.bottom());
    }

    fn k(u: RTerm) -> u32 {
        match u {
            RTerm::C(k) => k,
            _ => unreachable!(),
        }
    }

    #[test]
    fn an_equality_of_constants_moves_the_greater_constants_facts() {
        // {a} ⊑ {b}, A(a), B(b): A and B hold of the smaller constant
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let [a, b] = ["A", "B"].map(|n| s.class(n));
        s.sub(na, nb);
        s.sub(na, a);
        s.sub(nb, b);
        let d = saturate_root(s);
        let (ua, ub) = (c(na, &d.ix), c(nb, &d.ix));
        let lo = ua.min(ub);
        assert!(d.has(&[XLit::Atom(a, lo)]));
        assert!(d.has(&[XLit::Atom(b, lo)]));
        assert!(!d.bottom());
    }

    #[test]
    fn different_individuals_made_equal_clash() {
        // {a} ⊑ {b}, and a and b are different
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        s.sub(na, nb);
        s.ax.disjoint.push(vec![na, nb]);
        let d = saturate_root(s);
        assert!(d.bottom());
    }

    #[test]
    fn an_equality_below_a_ground_atom_waits_for_the_atom() {
        // {a} ⊑ {b} ⊔ C, B(b): under the shared ground order of item 804 the atom C(a) is
        // above a ≈ b, so a ≈ b ∨ C(a) stands, and once C ⊑ ⊥ resolves C(a) away the unit
        // a ≈ b rewrites B(b) to B of the smaller constant
        for bottom in [false, true] {
            let mut s = Sig::new();
            // B and C before the individuals, so that N_b(a) is maximal beside C(a)
            let [b, cc] = ["B", "C"].map(|n| s.class(n));
            let na = s.ind("a");
            let nb = s.ind("b");
            s.sub(nb, b);
            if bottom {
                s.sub(cc, BOTTOM);
            }
            s.dl.clauses.push((vec![na], vec![nb, cc]));
            let d = saturate_root(s);
            let (ua, ub) = (c(na, &d.ix), c(nb, &d.ix));
            let lo = ua.min(ub);
            let hi = ua.max(ub);
            if !bottom {
                assert!(
                    d.clauses().iter().any(|(bd, h)| bd.is_empty()
                        && h.contains(&XLit::Eq(lo, hi))
                        && h.contains(&XLit::Atom(cc, ua))),
                    "a ≈ b ∨ C(a)"
                );
                assert!(!d.bottom());
                continue;
            }
            assert!(d.has(&[XLit::Eq(lo, hi)]));
            assert!(d.has(&[XLit::Atom(b, lo)]));
            assert!(!d.bottom());
        }
    }

    #[test]
    fn a_minimum_s_successors_differ_in_the_root() {
        // {a} ⊑ M, M ⊑ ∃r.B1 ⊓ ∃r.B2 with f1(a) ≉ f2(a)
        let mut s = Sig::new();
        let na = s.ind("a");
        let [m, b1, b2] = ["M", "B1", "B2"].map(|n| s.class(n));
        let r = s.role("r");
        let e1 = s.some(r, b1);
        let e2 = s.some(r, b2);
        s.sub(na, m);
        s.sub(m, e1);
        s.sub(m, e2);
        s.dl.ne.push((m, vec![e1, e2]));
        s.dl.counting = true;
        let d = saturate_root(s);
        let u = k(c(na, &d.ix));
        let (f1, f2) = (RTerm::Fc(e1, u), RTerm::Fc(e2, u));
        assert!(d.has(&[XLit::Ne(f1.min(f2), f1.max(f2))]));
    }

    #[test]
    fn a_context_in_a_singleton_holds_its_facts_of_the_constant() {
        // A ⊑ {o}, A ⊑ B ⊔ C: in A's context A(o), and B(o) ∨ C(x), B(x) ∨ C(o)
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, cc] = ["A", "B", "C"].map(|n| s.class(n));
        s.sub(a, no);
        s.dl.clauses.push((vec![a], vec![b, cc]));
        let d = saturate_at(s, Some(a));
        let o = c(no, &d.ix);
        assert!(d.has(&[XLit::Eq(RTerm::X, o)]), "x ≈ o");
        assert!(d.has(&[XLit::Atom(a, o)]), "A(o)");
        assert!(d.has(&[XLit::Atom(no, o)]), "N_o(o)");
        // with the blocking of item 804's third departure x ≈ o rewrites the core unit
        // A(x) only, and the disjunction over x stays over x
        let mixed = d.has(&[XLit::Atom(b, o), XLit::Atom(cc, RTerm::X)])
            && d.has(&[XLit::Atom(b, RTerm::X), XLit::Atom(cc, o)]);
        assert_eq!(mixed, !super::super::dl_eq::eq_block());
        assert!(!d.has(&[XLit::Atom(b, o)]));
    }

    #[test]
    fn a_root_fact_of_a_named_class_is_rewritten_back_over_x_in_a_query_context() {
        // A ⊑ {o}, and the root holds B(o) and F(o) for a fresh F: A's context takes both
        // and rewrites B(o) to the unit B(x) with o ↦ x. With the blocking, the fresh atom
        // stays over o, since only a named-class atom is rewritten back.
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b] = ["A", "B"].map(|n| s.class(n));
        let f = s.it.fresh_class();
        s.sub(a, no);
        let d = saturate_after(s, &[ROOT], Some(a), |ix, _| {
            let o = c(no, ix);
            vec![
                (vec![], vec![XLit::Atom(b, o)]),
                (vec![], vec![XLit::Atom(f, o)]),
            ]
        });
        let o = c(no, &d.ix);
        assert!(d.has(&[XLit::Atom(a, o)]), "A(o)");
        assert!(d.has(&[XLit::Atom(b, o)]), "B(o) reached A's context");
        assert!(d.has(&[XLit::Atom(f, o)]), "F(o) reached A's context");
        assert!(d.subs(b), "B(x)");
        assert_eq!(d.subs(f), !super::super::dl_eq::eq_block());
    }

    #[test]
    fn a_hypothetical_successor_rewrites_nothing_with_an_equality_to_a_constant() {
        // A ⊑ ∃r.B ⊔ C, B ⊑ {o}, B ⊑ G: the hypothetical successor of ∃r.B holds the
        // marker B(x) → B(x), B(x) → x ≈ o and B(x) → G(x). With the blocking, x ≈ o
        // rewrites nothing there, since the successor is no query context; without it,
        // B(x) → G(o) and B(x) → B(o) follow.
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, cc, g] = ["A", "B", "C", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some_h(r, b);
        s.dl.clauses.push((vec![a], vec![e, cc]));
        s.sub(b, no);
        s.sub(b, g);
        let d = saturate_at(s, Some(a));
        let h = dix(&d.ix).hyp[&e];
        let o = c(no, &d.ix);
        let cs = d.clauses_of(h);
        let bx = XLit::Atom(b, RTerm::X);
        let has = |head: XLit| {
            cs.iter()
                .any(|(bd, hd)| bd == &vec![bx] && hd == &vec![head])
        };
        assert!(has(XLit::Eq(RTerm::X, o)), "B(x) → x ≈ o in {cs:?}");
        assert!(has(XLit::Atom(g, RTerm::X)), "B(x) → G(x)");
        let ground = has(XLit::Atom(g, o)) || has(XLit::Atom(b, o));
        assert_eq!(ground, !super::super::dl_eq::eq_block(), "{cs:?}");
    }

    #[test]
    fn a_clause_over_x_is_rewritten_at_each_maximal_atom() {
        // A ⊑ {o} and ⊤ → B(x) ∨ C(x): in A's context x ≈ o rewrites each of the two
        // incomparable atoms, and then the other, to B(o) ∨ C(o)
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, cc] = ["A", "B", "C"].map(|n| s.class(n));
        s.sub(a, no);
        s.dl.clauses.push((vec![], vec![b, cc]));
        let d = saturate_at(s, Some(a));
        let o = c(no, &d.ix);
        assert!(d.has(&[XLit::Atom(b, RTerm::X), XLit::Atom(cc, RTerm::X)]));
        assert!(d.has(&[XLit::Atom(b, o), XLit::Atom(cc, o)]));
    }

    #[test]
    fn a_universal_meets_a_role_atom_between_constants_from_either_end() {
        // ⊤ → r(a, b), ⊤ → (∀r.B)(a), ⊤ → (∀r⁻.C)(b): B(b) and C(a), in both arrival
        // orders of the role atom and the universal
        for role_first in [true, false] {
            let mut s = Sig::new();
            let na = s.ind("a");
            let nb = s.ind("b");
            let [b, cc] = ["B", "C"].map(|n| s.class(n));
            let r = s.role("r");
            let ri = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:r")));
            let all_b = s.all(r, b);
            let all_c = s.all(ri, cc);
            let d = saturate_seeded(s, None, |ix, _| {
                let (ua, ub) = (c(na, ix), c(nb, ix));
                let role = (vec![], vec![XLit::Role(r, ua, ub)]);
                let alls = [
                    (vec![], vec![XLit::Atom(all_b, ua)]),
                    (vec![], vec![XLit::Atom(all_c, ub)]),
                ];
                let mut v = Vec::new();
                if role_first {
                    v.push(role.clone());
                }
                v.extend(alls);
                if !role_first {
                    v.push(role);
                }
                v
            });
            let (ua, ub) = (c(na, &d.ix), c(nb, &d.ix));
            assert!(d.has(&[XLit::Atom(b, ub)]), "B(b), role first {role_first}");
            assert!(
                d.has(&[XLit::Atom(cc, ua)]),
                "C(a), role first {role_first}"
            );
        }
    }

    #[test]
    fn disjoint_roles_clash_between_constants() {
        // ⊤ → r(a, b), ⊤ → s(b, a)... with s⁻ below t, r and t disjoint
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let r = s.role("r");
        let t = s.role("t");
        s.dl.rdis.push((r, t));
        let d = saturate_seeded(s, None, |ix, _| {
            let (ua, ub) = (c(na, ix), c(nb, ix));
            vec![
                (vec![], vec![XLit::Role(r, ua, ub)]),
                (vec![], vec![XLit::Role(t, ua, ub)]),
            ]
        });
        assert!(d.bottom());
    }

    #[test]
    fn a_successor_in_a_singleton_sends_its_equality_and_ground_facts_back() {
        // A ⊑ ∃r.B, B ⊑ {o}, B ⊑ G: B's context has x ≈ o and G(o), and A's context
        // gets f(x) ≈ o and G(o), and through Eq r(x, o)
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, g] = ["A", "B", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some(r, b);
        s.sub(a, e);
        s.sub(b, no);
        s.sub(b, g);
        let d = saturate_at(s, Some(a));
        let o = c(no, &d.ix);
        assert!(d.has(&[XLit::Eq(RTerm::Fx(e), o)]), "f(x) ≈ o");
        assert!(d.has(&[XLit::Atom(g, o)]), "G(o)");
        assert!(d.has(&[XLit::Role(r, RTerm::X, o)]), "r(x, o)");
    }

    #[test]
    fn a_conditional_ground_fact_keeps_its_condition_through_pred() {
        // A ⊑ ∃r.B, B ⊑ C ⊔ {o}, B ⊑ G: G(o) holds only when B's instance is o, so A's
        // context has no G(o) alone. With {o} ⊑ G in place of B ⊑ G, G(o) would hold in
        // every model, and the root would give it to A's context.
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, cc, g] = ["A", "B", "C", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some(r, b);
        s.sub(a, e);
        s.dl.clauses.push((vec![b], vec![cc, no]));
        s.sub(b, g);
        let d = saturate_at(s, Some(a));
        let o = c(no, &d.ix);
        assert!(!d.has(&[XLit::Atom(g, o)]));
        assert!(!d.has(&[XLit::Eq(RTerm::Fx(e), o)]));
        assert!(!d.bottom());
    }

    #[test]
    fn factor_takes_two_equalities_of_y() {
        // ⊤ → y ≈ a ∨ y ≈ b, b above a: a ≉ b ∨ y ≈ b
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let d = saturate_seeded(s, None, |ix, _| {
            let (ua, ub) = (c(na, ix), c(nb, ix));
            vec![(vec![], vec![XLit::Eq(RTerm::Y, ua), XLit::Eq(RTerm::Y, ub)])]
        });
        let (ua, ub) = (c(na, &d.ix), c(nb, &d.ix));
        let (lo, hi) = (ua.min(ub), ua.max(ub));
        assert!(d.has(&[XLit::Ne(lo, hi), XLit::Eq(RTerm::Y, hi)]));
    }

    #[test]
    fn the_index_by_term_is_counted_from_its_first_allocation() {
        let mut st = super::super::dl_clause::Store::default();
        let mut s = Sig::new();
        let _ = s.ind("a");
        let ix = Index::build(&s.ax, &mut s.it, None, &|| false).unwrap();
        let lits = super::super::dl_clause::Lits::default();
        let inv = |_: Role| None;
        let o = Rooted {
            ix: &ix,
            lits: &lits,
            inv: &inv,
            ground_total: false,
        };
        let Made::Lit(l) = lits.lit(XLit::Atom(TOP, RTerm::C(0)), inv) else {
            unreachable!()
        };
        st.add(Clause::new(Vec::new(), vec![l]));
        let before = st.bytes();
        let added = st.take_grown();
        let mut max = Vec::new();
        st.next(&ix, Some(&o), &mut max);
        let grown = st.take_grown();
        assert!(added == 0);
        assert!(grown >= std::mem::size_of::<super::super::dl_clause::ByTerm>());
        assert!(
            st.bytes() - before <= grown,
            "growth {grown} covers {}",
            st.bytes() - before
        );
    }

    #[test]
    fn a_predecessor_that_comes_later_gets_the_clauses_with_maxima() {
        // B ⊑ {o}, B ⊑ G, A ⊑ ∃r.B, with B's context saturated before A reaches it
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, g] = ["A", "B", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some(r, b);
        s.sub(a, e);
        s.sub(b, no);
        s.sub(b, g);
        let d = saturate_after(s, &[b], Some(a), |_, _| Vec::new());
        let o = c(no, &d.ix);
        assert!(d.has(&[XLit::Eq(RTerm::Fx(e), o)]), "f(x) ≈ o");
        assert!(d.has(&[XLit::Atom(g, o)]), "G(o)");
    }

    #[test]
    fn only_predecessor_triggers_go_to_the_predecessor() {
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let bb = s.class("B");
        let r = s.role("r");
        let root = s.it.fresh_class();
        s.dl.root = Some(root);
        s.ax.dl = Some(Box::new(std::mem::take(&mut s.dl)));
        let ix = Index::build(&s.ax, &mut s.it, None, &|| false).unwrap();
        let dx = dix(&ix);
        let lits = super::super::dl_clause::Lits::default();
        let inv = inv_of(&ix, dx);
        let (ua, ub) = (c(na, &ix), c(nb, &ix));
        let l = |v: XLit| match lits.lit(v, inv) {
            Made::Lit(l) => l,
            _ => panic!(),
        };
        let ready = |h: Vec<XLit>| {
            super::super::dl_rules::pred_ready(
                &ix,
                dx,
                &lits,
                &Clause::new(Vec::new(), h.into_iter().map(l).collect()),
            )
        };
        assert!(ready(vec![XLit::Atom(bb, RTerm::Y)]));
        assert!(ready(vec![XLit::Atom(bb, ua)]));
        assert!(ready(vec![XLit::Role(r, ua, ub)]));
        assert!(ready(vec![XLit::Eq(RTerm::X, ua)]));
        assert!(ready(vec![XLit::Eq(RTerm::Y, ua)]));
        assert!(ready(vec![XLit::Eq(RTerm::X, RTerm::Y)]));
        assert!(ready(vec![XLit::Eq(ua, ub)]));
        assert!(!ready(vec![XLit::Role(r, RTerm::Y, ua)]));
        assert!(!ready(vec![XLit::Role(r, ua, RTerm::Y)]));
        assert!(!ready(vec![XLit::Role(r, RTerm::Y, RTerm::Y)]));
        assert!(!ready(vec![XLit::Ne(RTerm::Y, ua)]));
        // a ground literal goes whatever its form
        assert!(ready(vec![XLit::Ne(ua, ub)]));
        // a role atom between x and a constant goes, and only the root takes it
        assert!(ready(vec![XLit::Role(r, RTerm::X, ua)]));
        assert!(!ready(vec![XLit::Atom(bb, RTerm::X)]));
    }

    #[test]
    fn a_later_predecessor_replays_no_clause_outside_the_triggers() {
        // B's context holds ⊤ → s(y, y) before A ⊑ ∃r.B reaches it: A gets no s(x, x)
        let mut s = Sig::new();
        let _ = s.ind("o");
        let [a, b] = ["A", "B"].map(|n| s.class(n));
        let r = s.role("r");
        let t = s.role("s");
        let e = s.some(r, b);
        s.sub(a, e);
        let d = saturate_after(s, &[b], Some(a), |_, _| {
            vec![(vec![], vec![XLit::Role(t, RTerm::Y, RTerm::Y)])]
        });
        assert!(!d.has(&[XLit::Role(t, RTerm::X, RTerm::X)]));
        assert!(
            !d.clauses()
                .iter()
                .any(|(_, h)| h.contains(&XLit::Role(t, RTerm::X, RTerm::X)))
        );
    }

    #[test]
    fn a_fact_of_an_equal_constant_reaches_a_context_in_the_singleton() {
        // {p} ⊑ {o}, {p} ⊑ C, A ⊑ {o}: A ⊑ C, whichever constant is greater and whichever
        // of the root and A's context saturates first
        for (o_first, root_first) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut s = Sig::new();
            let (no, np) = if o_first {
                let o = s.ind("o");
                (o, s.ind("p"))
            } else {
                let p = s.ind("p");
                (s.ind("o"), p)
            };
            let [a, cc] = ["A", "C"].map(|n| s.class(n));
            s.sub(np, no);
            s.sub(np, cc);
            s.sub(a, no);
            let before: &[Ex] = if root_first { &[ROOT] } else { &[] };
            let d = saturate_after(s, before, Some(a), |_, _| Vec::new());
            assert!(
                d.subs(cc),
                "A ⊑ C, o first {o_first}, root first {root_first}"
            );
            assert!(!d.bottom());
        }
    }

    #[test]
    fn a_root_clause_over_y_returns_to_the_context_with_the_edge() {
        // A ⊑ ∃r.{o}, {p} ⊑ {o}, {p} ⊑ ∀r⁻.B: A's context has r(x, o), the root the marker
        // r(y, o) and r(y, o) → B(y), and A ⊑ B, in every order
        for (o_first, root_first) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut s = Sig::new();
            let (no, np) = if o_first {
                let o = s.ind("o");
                (o, s.ind("p"))
            } else {
                let p = s.ind("p");
                (s.ind("o"), p)
            };
            let [a, b] = ["A", "B"].map(|n| s.class(n));
            let r = s.role("r");
            let ri = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:r")));
            let e = s.some(r, no);
            let all = s.all(ri, b);
            s.sub(a, e);
            s.sub(np, no);
            s.sub(np, all);
            let before: &[Ex] = if root_first { &[ROOT] } else { &[] };
            let d = saturate_after(s, before, Some(a), |_, _| Vec::new());
            assert!(
                d.subs(b),
                "A ⊑ B, o first {o_first}, root first {root_first}"
            );
        }
    }

    #[test]
    fn join_resolves_a_ground_atom_in_either_arrival_order() {
        // the Horn clause first: ⊤ → B(o) with B(o) → C(o) ∨ D(o), and B(o) → C(o) with
        // ⊤ → B(o) ∨ E(o)
        for head_first in [true, false] {
            let mut s = Sig::new();
            let no = s.ind("o");
            // E before B, so that B(o) is maximal beside E(o) under the shared ground order
            let [ee, b, cc, dd] = ["E", "B", "C", "D"].map(|n| s.class(n));
            let d = saturate_seeded(s, None, |ix, _| {
                let o = c(no, ix);
                let at = |a: Ex| XLit::Atom(a, o);
                if head_first {
                    vec![(vec![], vec![at(b)]), (vec![at(b)], vec![at(cc), at(dd)])]
                } else {
                    vec![(vec![], vec![at(b), at(ee)]), (vec![at(b)], vec![at(cc)])]
                }
            });
            let o = c(no, &d.ix);
            let at = |a: Ex| XLit::Atom(a, o);
            if head_first {
                assert!(d.has(&[at(cc), at(dd)]));
            } else {
                assert!(d.has(&[at(cc), at(ee)]));
            }
        }
    }

    #[test]
    fn the_root_s_bottom_reaches_a_context_with_an_edge() {
        // {p} ⊑ {o}, {p} ⊑ C, {o} and C disjoint: the ontology is inconsistent, and
        // A ⊑ {q}, which reaches the root by q, is unsatisfiable
        for root_first in [true, false] {
            let mut s = Sig::new();
            let no = s.ind("o");
            let np = s.ind("p");
            let nq = s.ind("q");
            let [a, cc] = ["A", "C"].map(|n| s.class(n));
            s.sub(np, no);
            s.sub(np, cc);
            s.ax.disjoint.push(vec![no, cc]);
            s.sub(a, nq);
            let before: &[Ex] = if root_first { &[ROOT] } else { &[] };
            let d = saturate_after(s, before, Some(a), |_, _| Vec::new());
            assert!(d.bottom(), "root first {root_first}");
        }
    }

    #[test]
    fn root_clauses_meet_the_cases_of_a_disjunction() {
        // A ⊑ D ⊔ E, D ⊑ ∃r.{o}, E ⊑ ∃s.{o}, {p} ⊑ {o}, {p} ⊑ ∀r⁻.B ⊓ ∀s⁻.B: A ⊑ B, which
        // takes the root's clauses r(y, o) → B(y) and s(y, o) → B(y) in each case
        for (o_first, root_first) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut s = Sig::new();
            let (no, np) = if o_first {
                let o = s.ind("o");
                (o, s.ind("p"))
            } else {
                let p = s.ind("p");
                (s.ind("o"), p)
            };
            let [a, b, dd, ee] = ["A", "B", "D", "E"].map(|n| s.class(n));
            let r = s.role("r");
            let t = s.role("s");
            let ri = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:r")));
            let ti = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:s")));
            let er = s.some(r, no);
            let et = s.some(t, no);
            let ar = s.all(ri, b);
            let at = s.all(ti, b);
            s.dl.clauses.push((vec![a], vec![dd, ee]));
            s.sub(dd, er);
            s.sub(ee, et);
            s.sub(np, no);
            s.sub(np, ar);
            s.sub(np, at);
            let before: &[Ex] = if root_first { &[ROOT] } else { &[] };
            let d = saturate_after(s, before, Some(a), |_, _| Vec::new());
            assert!(
                d.subs(b),
                "A ⊑ B, o first {o_first}, root first {root_first}"
            );
        }
    }

    /// A sink that keeps what the rules send.
    struct Probe<'a> {
        run: &'a DlRun,
        sent: Vec<(Ex, Concl)>,
        mine: Vec<Concl>,
    }

    impl Sink for Probe<'_> {
        fn send(&mut self, to: Ex, c: Concl) {
            self.sent.push((to, c));
        }
        fn me(&mut self, c: Concl) {
            self.mine.push(c);
        }
        fn dl(&self) -> &DlRun {
            self.run
        }
    }

    #[test]
    fn a_context_derives_the_consequences_of_its_own_ground_disjunction() {
        // Z's context holds ⊤ → C(a) ∨ C(b) and C ⊑ D: Hyper at the greater constant gives
        // ⊤ → D(a) ∨ C(b) or ⊤ → C(a) ∨ D(b) in Z's context itself, and the root holds no
        // clause with a body, since the run has no markers in the root
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let [cc, dd, z] = ["C", "D", "Z"].map(|n| s.class(n));
        s.sub(cc, dd);
        let d = saturate_after(s, &[z], Some(z), |ix, _| {
            vec![(
                vec![],
                vec![XLit::Atom(cc, c(na, ix)), XLit::Atom(cc, c(nb, ix))],
            )]
        });
        let (a, b) = (c(na, &d.ix), c(nb, &d.ix));
        let cs = d.clauses_of(z);
        let has = |h: &[XLit]| {
            cs.iter().any(|(bd, hd)| {
                bd.is_empty() && hd.len() == h.len() && h.iter().all(|l| hd.contains(l))
            })
        };
        let (hi, lo) = if Rooted::a_gt(a, b) { (a, b) } else { (b, a) };
        assert!(has(&[XLit::Atom(dd, hi), XLit::Atom(cc, lo)]), "{cs:?}");
        if super::super::dl_clause::ground_total() {
            assert!(!has(&[XLit::Atom(cc, hi), XLit::Atom(dd, lo)]), "{cs:?}");
        }
        let root = dix(&d.ix).root.unwrap();
        assert!(
            d.clauses_of(root).iter().all(|(bd, _)| bd.is_empty()),
            "{:?}",
            d.clauses_of(root)
        );
    }

    #[test]
    fn a_root_fact_reaches_a_context_that_mentions_the_constant_inside_a_successor_term() {
        // {o} ⊑ G, and Z's context holds ⊤ → K(f_e(o)): Z registers for o and takes the
        // root's G(o)
        let mut s = Sig::new();
        let no = s.ind("o");
        let [g, k, z] = ["G", "K", "Z"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some_h(r, k);
        s.sub(no, g);
        let d = saturate_after(s, &[z], Some(z), |ix, _| {
            let RTerm::C(o) = c(no, ix) else { panic!() };
            vec![(vec![], vec![XLit::Atom(k, RTerm::Fc(e, o))])]
        });
        let o = c(no, &d.ix);
        let cs = d.clauses_of(z);
        assert!(
            cs.iter()
                .any(|(bd, hd)| bd.is_empty() && hd == &vec![XLit::Atom(g, o)]),
            "{cs:?}"
        );
    }

    #[test]
    fn a_context_takes_a_successor_over_a_constant_through_its_own_edge() {
        // Z's context holds ⊤ → B(a) ∨ E(x), B ⊑ ∃r.C and C ⊑ ⊥: Z's own Succ over f(a)
        // reaches the hypothetical successor, which refutes C, and Pred along the edge
        // gives ⊤ → E(x), so Z ⊑ E
        let mut s = Sig::new();
        let na = s.ind("a");
        let [b, cc, ee, z] = ["B", "C", "E", "Z"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some_h(r, cc);
        s.sub(b, e);
        s.sub(cc, BOTTOM);
        let d = saturate_after(s, &[z], Some(z), |ix, _| {
            vec![(
                vec![],
                vec![XLit::Atom(b, c(na, ix)), XLit::Atom(ee, RTerm::X)],
            )]
        });
        assert!(d.subs(ee), "{:?}", d.clauses_of(z));
        assert!(!d.bottom());
    }

    #[test]
    fn pred_along_an_edge_over_a_constant_replays_in_either_order() {
        // the successor's clauses C(x) → ⊥ and C(x) → s(v, x) reach Z before or after Z
        // has ⊤ → B(a) ∨ E(x) with B ⊑ ∃r.C: Z derives ⊤ → E(x) and ⊤ → s(v, f(a)) ∨ E(x)
        // either way, the role atom between a constant and x returning along an edge over
        // a constant as Table 3 admits for the root
        for clause_first in [true, false] {
            let mut s = Sig::new();
            let na = s.ind("a");
            let nv = s.ind("v");
            let [b, cc, ee, z] = ["B", "C", "E", "Z"].map(|n| s.class(n));
            let r = s.role("r");
            let t = s.role("s");
            let e = s.some_h(r, cc);
            s.sub(b, e);
            let (ix, _) = probe_ix(s);
            let h = dix(&ix).hyp[&e];
            let run = DlRun::new(false, Default::default());
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let (a, v) = (c(na, &ix), c(nv, &ix));
            let inv = inv_of(&ix, dix(&ix));
            let l = |w: XLit| match run.lits.lit(w, inv) {
                Made::Lit(l) => l,
                _ => panic!("a literal"),
            };
            let k1 = run.table.push(Clause::new(vec![Lit::x(cc)], Vec::new()));
            let k2 = run.table.push(Clause::new(
                vec![Lit::x(cc)],
                vec![l(XLit::Role(t, v, RTerm::X))],
            ));
            let mut st = State {
                init: true,
                ..State::default()
            };
            let send = |st: &mut State, out: &mut Probe| {
                assert!(super::super::dl_rules::pred_c(&ix, z, st, h, k1, out));
                assert!(super::super::dl_rules::pred_c(&ix, z, st, h, k2, out));
                assert!(
                    !super::super::dl_rules::pred_c(&ix, z, st, h, k1, out),
                    "a clause is taken once"
                );
            };
            if clause_first {
                send(&mut st, &mut out);
                assert!(out.mine.is_empty());
            }
            ds(&mut st).store.add(Clause::new(
                Vec::new(),
                vec![l(XLit::Atom(b, a)), Lit::x(ee)],
            ));
            super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
            if !clause_first {
                send(&mut st, &mut out);
                super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 100);
            }
            assert!(
                out.mine.contains(&Concl::Sub(ee)),
                "clause first {clause_first}: {:?}",
                out.mine
            );
            assert!(
                out.sent.contains(&(h, Concl::DEdge(z, e))),
                "the edge, clause first {clause_first}"
            );
            let RTerm::C(ua) = a else { panic!() };
            let want = l(XLit::Role(t, v, RTerm::Fc(e, ua)));
            let store = &dl(&st).unwrap().store;
            assert!(
                store
                    .clauses
                    .iter()
                    .any(|c| c.c.body().is_empty() && c.c.head().contains(&want)),
                "s(v, f(a)) ∨ E(x), clause first {clause_first}"
            );
        }
    }

    #[test]
    fn an_assertion_s_successor_meets_its_constant() {
        // r(a, o) as {a} ⊑ ∃r.{o}, {a} ⊑ ∀r.C, A ⊑ {o}: the root's successor for f(a) gives
        // f(a) ≈ o, the root then has r(a, o) and C(o), and A ⊑ C
        for root_first in [true, false] {
            let mut s = Sig::new();
            let na = s.ind("a");
            let no = s.ind("o");
            let [a, cc] = ["A", "C"].map(|n| s.class(n));
            let r = s.role("r");
            let e = s.some_h(r, no);
            let all = s.all(r, cc);
            s.sub(na, e);
            s.sub(na, all);
            s.sub(a, no);
            let before: &[Ex] = if root_first { &[ROOT] } else { &[] };
            let d = saturate_after(s, before, Some(a), |_, _| Vec::new());
            assert!(d.subs(cc), "A ⊑ C, root first {root_first}");
            assert!(!d.bottom());
        }
    }

    #[test]
    fn a_successor_of_the_root_sends_back_what_holds_of_the_constant() {
        // {a} ⊑ ∃r.B, B ⊑ ∃s.{o}, {o} ⊑ ∀s⁻.G, G ⊑ ∀r⁻.K: K(a), and the root's successor
        // took the marker of the ground fact N_a(a), which the root's ⊤ → N_a(a) then
        // subsumes
        let mut s = Sig::new();
        let na = s.ind("a");
        let no = s.ind("o");
        let [b, g, k] = ["B", "G", "K"].map(|n| s.class(n));
        let r = s.role("r");
        let t = s.role("s");
        let ri = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:r")));
        let ti = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:s")));
        let er = s.some_h(r, b);
        let et = s.some_h(t, no);
        let ag = s.all(ti, g);
        let ak = s.all(ri, k);
        s.sub(na, er);
        s.sub(b, et);
        s.sub(no, ag);
        s.sub(g, ak);
        let h = s.dl.hyp[&er];
        let d = saturate_root(s);
        let ua = c(na, &d.ix);
        assert!(d.has(&[XLit::Atom(k, ua)]), "K(a)");
        let m = XLit::Atom(na, ua);
        assert!(
            d.clauses_of(h)
                .iter()
                .any(|(bd, hd)| bd.iter().all(|l| *l == m) && hd == &vec![m]),
            "the marker of N_a(a)"
        );
        assert!(!d.bottom());
    }

    #[test]
    fn an_unsatisfiable_successor_of_the_root_makes_it_unsatisfiable() {
        // {a} ⊑ ∃r.D, D ⊑ E ⊔ F, E and F unsatisfiable
        let mut s = Sig::new();
        let na = s.ind("a");
        let [dd, ee, ff] = ["D", "E", "F"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some_h(r, dd);
        s.sub(na, e);
        s.dl.clauses.push((vec![dd], vec![ee, ff]));
        s.sub(ee, BOTTOM);
        s.sub(ff, BOTTOM);
        let d = saturate_root(s);
        assert!(d.bottom());
    }

    /// The index and a probe over a signature whose root is made here.
    fn probe_ix(mut s: Sig) -> (Index, Ex) {
        let root = s.it.fresh_class();
        s.dl.root = Some(root);
        s.ax.dl = Some(Box::new(std::mem::take(&mut s.dl)));
        (
            Index::build(&s.ax, &mut s.it, None, &|| false).unwrap(),
            root,
        )
    }

    #[test]
    fn a_marker_is_no_premise_of_join() {
        // A(o) → A(o) and A(o) → B(o) in a context other than the root, in both orders:
        // Join derives nothing
        for marker_first in [true, false] {
            let mut s = Sig::new();
            let no = s.ind("o");
            let [a, b] = ["A", "B"].map(|n| s.class(n));
            let (ix, _) = probe_ix(s);
            let run = DlRun::new(false, Default::default());
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let o = c(no, &ix);
            let inv = inv_of(&ix, dix(&ix));
            let l = |v: XLit| match run.lits.lit(v, inv) {
                Made::Lit(l) => l,
                _ => panic!("a literal"),
            };
            let (ao, bo) = (l(XLit::Atom(a, o)), l(XLit::Atom(b, o)));
            let mut st = State {
                init: true,
                ..State::default()
            };
            let mut cs = vec![
                Clause::new(vec![ao], vec![ao]),
                Clause::new(vec![ao], vec![bo]),
            ];
            if !marker_first {
                cs.reverse();
            }
            for c in cs {
                ds(&mut st).store.add(c);
            }
            super::super::dl_rules::flush(&ix, a, &mut st, &mut out, 100);
            assert_eq!(run.work(), 0, "marker first {marker_first}");
        }
    }

    #[test]
    fn the_root_s_state_is_counted_when_made() {
        let s = Sig::new();
        let (ix, _) = probe_ix(s);
        let run = DlRun::new(false, Default::default());
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let mut st = State::default();
        init(&ix, &mut st, &mut out);
        assert!(dl(&st).unwrap().root.is_some());
        assert!(run.bytes() >= std::mem::size_of::<RootSt>());
    }

    #[test]
    fn every_predecessor_of_a_rooted_run_takes_a_clause_over_any_role_of_its_successor() {
        // a successor by r, with r not below s, holds s(y, x) → K(y): the root and another
        // predecessor by r get it whether they register before or after the clause, since
        // a predecessor may reach the successor over f_e(u) and finds the role premises
        // itself, and Pred over f_e(x) refuses the clause at the predecessor
        for register_first in [true, false] {
            let mut s = Sig::new();
            let _ = s.ind("a");
            let [a, b, k] = ["A", "B", "K"].map(|n| s.class(n));
            let r = s.role("r");
            let t = s.role("s");
            let e = s.some(r, b);
            let (ix, root) = probe_ix(s);
            let run = DlRun::new(false, Default::default());
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let mut st = State {
                init: true,
                ..State::default()
            };
            let add = |st: &mut State, out: &mut Probe| {
                ds(st)
                    .store
                    .add(Clause::new(vec![Lit::ry(t)], vec![Lit::y(k)]));
                super::super::dl_rules::flush(&ix, b, st, out, 100);
            };
            if !register_first {
                add(&mut st, &mut out);
            }
            super::super::dl_rules::dedge(&ix, b, &mut st, root, e, &mut out);
            super::super::dl_rules::dedge(&ix, b, &mut st, a, e, &mut out);
            if register_first {
                add(&mut st, &mut out);
            }
            let to = |v: Ex| {
                out.sent
                    .iter()
                    .any(|&(w, c)| w == v && matches!(c, Concl::PredC(f, _) if f == b))
            };
            assert!(to(root), "the root, register first {register_first}");
            assert!(
                to(a),
                "another predecessor, register first {register_first}"
            );
        }
    }

    #[test]
    fn the_root_counts_the_clauses_its_successors_send() {
        let mut s = Sig::new();
        let _ = s.ind("a");
        let b = s.class("B");
        let (ix, _) = probe_ix(s);
        let run = DlRun::new(false, Default::default());
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let cid = run.table.push(Clause::new(Vec::new(), vec![Lit::y(b)]));
        let mut st = State {
            init: true,
            ..State::default()
        };
        ds(&mut st);
        let before = run.bytes();
        let root = dix(&ix).root.unwrap();
        assert!(super::super::dl_rules::pred_c(
            &ix, root, &mut st, b, cid, &mut out
        ));
        assert!(
            !super::super::dl_rules::pred_c(&ix, root, &mut st, b, cid, &mut out),
            "a clause is taken once"
        );
        assert!(run.bytes() > before);
    }

    #[test]
    fn every_predecessor_of_a_rooted_run_takes_a_propagation_over_any_role_of_its_successor() {
        // B's context by r, with r not below s, holds ∀s⁻.K as a unit: the root and another
        // predecessor by r get s(y, x) → K(y) whether they register before or after the
        // unit, as above
        for register_first in [true, false] {
            let mut s = Sig::new();
            let _ = s.ind("a");
            let [a, b, k] = ["A", "B", "K"].map(|n| s.class(n));
            let r = s.role("r");
            let _ = s.role("s");
            let si = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:s")));
            let e = s.some(r, b);
            let all = s.all(si, k);
            let (ix, root) = probe_ix(s);
            let run = DlRun::new(false, Default::default());
            let mut out = Probe {
                run: &run,
                sent: Vec::new(),
                mine: Vec::new(),
            };
            let mut st = State {
                init: true,
                ..State::default()
            };
            ds(&mut st);
            if !register_first {
                super::super::dl_rules::sub(&ix, b, &mut st, all, &mut out);
            }
            super::super::dl_rules::dedge(&ix, b, &mut st, root, e, &mut out);
            super::super::dl_rules::dedge(&ix, b, &mut st, a, e, &mut out);
            if register_first {
                super::super::dl_rules::sub(&ix, b, &mut st, all, &mut out);
            }
            let to = |v: Ex| {
                out.sent
                    .iter()
                    .any(|&(w, c)| w == v && matches!(c, Concl::PredC(f, _) if f == b))
            };
            assert!(to(root), "the root, register first {register_first}");
            assert!(
                to(a),
                "another predecessor, register first {register_first}"
            );
        }
    }

    /// A probe run of the context `me` over the clauses `seed`, flushed, with the live
    /// clauses of its store in general form.
    fn probe_flush(
        s: Sig,
        me: impl Fn(Ex) -> Ex,
        seed: impl Fn(&Index, &dyn Fn(XLit) -> Lit) -> Vec<(Vec<XLit>, Vec<XLit>)>,
    ) -> Vec<(Vec<XLit>, Vec<XLit>)> {
        let (ix, root) = probe_ix(s);
        let run = DlRun::new(false, Default::default());
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let inv = inv_of(&ix, dix(&ix));
        let l = |v: XLit| match run.lits.lit(v, inv) {
            Made::Lit(l) => l,
            _ => panic!("a literal"),
        };
        let mut st = State {
            init: true,
            ..State::default()
        };
        for (b, h) in seed(&ix, &l) {
            ds(&mut st).store.add(Clause::new(
                b.iter().copied().map(l).collect(),
                h.iter().copied().map(l).collect(),
            ));
        }
        let me = me(root);
        if me == root {
            flush(&ix, &mut st, &mut out, 100);
        } else {
            super::super::dl_rules::flush(&ix, me, &mut st, &mut out, 100);
        }
        let v = |ls: &[Lit]| ls.iter().map(|&l| run.lits.view(l, inv)).collect();
        dl(&st)
            .unwrap()
            .store
            .clauses
            .iter()
            .filter(|c| !c.dead)
            .map(|c| (v(c.c.body()), v(c.c.head())))
            .collect()
    }

    #[test]
    fn a_context_makes_no_ground_clause_with_a_body_from_its_markers() {
        // the markers A(a) → A(a) and B(a) → B(a) and A ⊓ B ⊑ H: Hyper at a would give
        // A(a) ∧ B(a) → H(a), a ground clause with a body, which the restriction on ground
        // inferences leaves out
        let mut s = Sig::new();
        let na = s.ind("a");
        let [a, b, h, z] = ["A", "B", "H", "Z"].map(|n| s.class(n));
        s.dl.clauses.push((vec![a, b], vec![h]));
        let cs = probe_flush(
            s,
            |_| z,
            |ix, _| {
                let u = c(na, ix);
                vec![
                    (vec![XLit::Atom(a, u)], vec![XLit::Atom(a, u)]),
                    (vec![XLit::Atom(b, u)], vec![XLit::Atom(b, u)]),
                ]
            },
        );
        let made = cs.iter().any(|(bd, hd)| {
            bd.len() == 2 && hd.iter().any(|l| matches!(l, XLit::Atom(k, _) if *k == h))
        });
        assert_eq!(made, !super::super::dl_rules::ground_restrict(), "{cs:?}");
    }

    #[test]
    fn a_disjoint_role_clash_over_x_keeps_its_ground_condition() {
        // A(a) → s(a, x) and ⊤ → t(a, x) with s and t disjoint: the Hyper binds y to x, so
        // it is no ground inference, and A(a) → ⊥ is made with the restriction on
        let mut s = Sig::new();
        let na = s.ind("a");
        let [a, z] = ["A", "Z"].map(|n| s.class(n));
        let r = s.role("s");
        let t = s.role("t");
        s.dl.rdis.push((r, t));
        let cs = probe_flush(
            s,
            |_| z,
            |ix, _| {
                let u = c(na, ix);
                vec![
                    (vec![XLit::Atom(a, u)], vec![XLit::Role(r, u, RTerm::X)]),
                    (vec![], vec![XLit::Role(t, u, RTerm::X)]),
                ]
            },
        );
        assert!(
            cs.iter().any(|(bd, hd)| hd.is_empty()
                && bd.len() == 1
                && matches!(bd[0], XLit::Atom(k, _) if k == a)),
            "{cs:?}"
        );
    }

    #[test]
    fn an_equality_over_a_successor_term_keeps_its_ground_condition() {
        // A(a) → f(x) ≈ u and ⊤ → B(f(x)): the equality holds x, so Eq is no ground
        // inference, and A(a) → B(u) is made with the restriction on
        let mut s = Sig::new();
        let na = s.ind("a");
        let nu = s.ind("u");
        let [a, b, k, z] = ["A", "B", "K", "Z"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some_h(r, k);
        let cs = probe_flush(
            s,
            |_| z,
            |ix, _| {
                let (ua, uu) = (c(na, ix), c(nu, ix));
                vec![
                    (vec![XLit::Atom(a, ua)], vec![XLit::Eq(RTerm::Fx(e), uu)]),
                    (vec![], vec![XLit::Atom(b, RTerm::Fx(e))]),
                ]
            },
        );
        assert!(
            cs.iter().any(|(bd, hd)| {
                bd.len() == 1
                    && matches!(bd[0], XLit::Atom(k, _) if k == a)
                    && matches!(hd.as_slice(), [XLit::Atom(k, RTerm::C(_))] if *k == b)
            }),
            "{cs:?}"
        );
    }

    #[test]
    fn the_root_discharges_a_ground_body_by_join() {
        // ⊤ → A(a) and A(a) → D(a) in the root, as Pred along an edge over a constant
        // leaves a successor's marker in the body: Join gives ⊤ → D(a), which has no body
        // and so passes the restriction on ground inferences
        let mut s = Sig::new();
        let na = s.ind("a");
        let [a, dd] = ["A", "D"].map(|n| s.class(n));
        let cs = probe_flush(
            s,
            |root| root,
            |ix, _| {
                let u = c(na, ix);
                vec![
                    (vec![], vec![XLit::Atom(a, u)]),
                    (vec![XLit::Atom(a, u)], vec![XLit::Atom(dd, u)]),
                ]
            },
        );
        assert!(
            cs.iter()
                .any(|(bd, hd)| bd.is_empty()
                    && matches!(hd.as_slice(), [XLit::Atom(k, _)] if *k == dd)),
            "{cs:?}"
        );
    }

    /// A context of the class `at`, or the root, seeded with `seeds` and flushed, with its
    /// live clauses in general form. o is the greater constant of o and p.
    fn flushed_tautologies(at_root: bool) -> Vec<(Vec<XLit>, Vec<XLit>)> {
        let mut s = Sig::new();
        let np = s.ind("p");
        let no = s.ind("o");
        let [a, cc, g, z] = ["A", "C", "G", "Z"].map(|n| s.class(n));
        let root = s.it.fresh_class();
        s.dl.root = Some(root);
        s.ax.dl = Some(Box::new(std::mem::take(&mut s.dl)));
        let ix = Index::build(&s.ax, &mut s.it, None, &|| false).unwrap();
        let run = DlRun::new(false, Default::default());
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let (o, p) = (c(no, &ix), c(np, &ix));
        assert!(Rooted::a_gt(o, p));
        let inv = inv_of(&ix, dix(&ix));
        let l = |v: XLit| match run.lits.lit(v, inv) {
            Made::Lit(l) => l,
            _ => panic!("a literal"),
        };
        let mut st = State {
            init: true,
            ..State::default()
        };
        if at_root {
            ds(&mut st).root.get_or_insert_with(Box::default);
        }
        let seeds = [
            // Join on A(o) gives ⊤ → o ≈ p ∨ o ≉ p
            (vec![], vec![XLit::Atom(a, o), XLit::Eq(o, p)]),
            (vec![XLit::Atom(a, o)], vec![XLit::Ne(o, p)]),
            // Join on A(o) gives G(p) → C(o) ∨ G(p)
            (
                vec![XLit::Atom(g, p)],
                vec![XLit::Atom(a, o), XLit::Atom(cc, o)],
            ),
            (vec![XLit::Atom(a, o)], vec![XLit::Atom(g, p)]),
        ];
        for (b, h) in seeds {
            ds(&mut st).store.add(Clause::new(
                b.into_iter().map(l).collect(),
                h.into_iter().map(l).collect(),
            ));
        }
        if at_root {
            flush(&ix, &mut st, &mut out, 1000);
        } else {
            super::super::dl_rules::flush(&ix, z, &mut st, &mut out, 1000);
        }
        let x = dl(&st).unwrap();
        x.store
            .clauses
            .iter()
            .filter(|s| !s.dead)
            .map(|s| {
                let v = |ls: &[Lit]| ls.iter().map(|&l| run.lits.view(l, inv)).collect();
                (v(s.c.body()), v(s.c.head()))
            })
            .collect()
    }

    #[test]
    fn a_ground_tautology_is_no_clause() {
        // Elim drops Γ ∧ A → Δ ∨ A and Γ → Δ ∨ s ≈ t ∨ s ≉ t whatever their literals, in
        // the root and in every other context, with counting off
        for at_root in [true, false] {
            let kept = flushed_tautologies(at_root);
            assert!(
                kept.iter().any(|(b, _)| b.is_empty()),
                "the seeds are kept, root {at_root}"
            );
            for (b, h) in &kept {
                assert!(
                    !h.iter().any(|l| b.contains(l)),
                    "{b:?} → {h:?} is a tautology, root {at_root}"
                );
            }
            for (b, h) in &kept {
                assert!(
                    !h.iter()
                        .any(|&e| matches!(e, XLit::Eq(s, t) if h.contains(&XLit::Ne(s, t)))),
                    "{b:?} → {h:?} is a tautology, root {at_root}"
                );
            }
        }
    }

    #[test]
    fn a_maximal_equality_to_a_constant_registers_for_it() {
        // {o} ⊑ {p} with o above p gives the root ⊤ → o ≈ p. A context whose only clause is
        // ⊤ → x ≈ o takes it, as the journal's r-Pred gives every context a root clause
        // without a body, and Eq gives it ⊤ → x ≈ p
        let mut s = Sig::new();
        let np = s.ind("p");
        let no = s.ind("o");
        let z = s.class("Z");
        s.sub(no, np);
        let d = saturate_after(s, &[z], Some(z), |ix, _| {
            vec![(vec![], vec![XLit::Eq(RTerm::X, c(no, ix))])]
        });
        let (o, p) = (c(no, &d.ix), c(np, &d.ix));
        assert!(Rooted::a_gt(o, p));
        let has = |h: XLit| {
            d.clauses_of(z)
                .iter()
                .any(|(b, hs)| b.is_empty() && hs == &vec![h])
        };
        assert!(has(XLit::Eq(RTerm::X, o)), "the seed is kept");
        assert!(has(XLit::Eq(RTerm::X, p)), "{:?}", d.clauses_of(z));
    }
}
