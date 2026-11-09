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
//! A context other than the root whose maximal literal is a root successor trigger of the
//! journal's Definition 4 read with y as x, `B(u)`, `S(x, u)`, `S(u, x)`, `S(u1, u2)` or
//! `u1 ≈ u2`, sends the root the trigger's marker `A → A` over y and registers with the root
//! for each of its constants (r-Succ). A processed root clause whose body atoms are such
//! triggers and whose head literals are ground or root predecessor triggers goes back to
//! each context registered for every constant of its body, or for a constant of its head
//! when the body is empty (r-Pred). That context resolves each body atom, read with y as x,
//! with its own clauses where it is maximal, and takes the head with y read as x. The
//! journal's r-Pred has a clause without a body reach every context, but a context that
//! never registered for one of the clause's constants has no literal over that constant
//! that the clause resolves with.
//!
//! A maximal literal over `f_e(u)` in the root takes Succ: the root registers with e's
//! hypothetical successor, which has the empty core, records the constant u for the edge and
//! sends the successor the markers of the atoms over `f_e(u)`. The successor's clauses over
//! its predecessor come back with y read as u and x as `f_e(u)` (Pred), for every constant
//! the root reached it by, and a role atom between x and a constant comes back too, as
//! Table 3 admits for the root. Every context sends each of its successors the markers of
//! its maximal ground atoms and equalities of constants, so that one edge covers every
//! successor trigger of Definition 4. A context that holds a ground literal in a body
//! registers with the root for its constants, so that the root's clauses over them reach it.
//!
//! The at-most restrictions and the qualified roles of counting are not read in the root.

use super::dl_clause::{Clause, Lit, Made, RTerm, Rooted, XL, XLit, terms_of};
use super::dl_rules::{
    DlIx, Found, Side, alls, dix, dl, drain, drain_with, ds, merge, pred_id, side, unit, with,
};
use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use super::saturate::{Concl, Sink, State, table_bytes};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::atomic::Ordering;

/// What a context of a rooted run keeps for the rules that involve the root. The root keeps
/// its processed clauses by constant, the contexts registered with it, its markers and the
/// clauses that r-Pred sends. Another context keeps the markers it sent the root and the
/// root's clauses it received.
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
    /// in the root, the markers it holds
    markers: FxHashSet<Lit>,
    /// in the root, the processed clauses that r-Pred sends, by each constant of their
    /// bodies, or of their heads when the body is empty
    ready: FxHashMap<u32, Vec<u32>>,
    /// in another context, the markers it sent the root
    sent: FxHashSet<Lit>,
    /// in another context, the root's clauses it received, by table id
    seen: FxHashSet<u32>,
    /// in another context, the root's clauses it received, by each body literal read with
    /// y as x
    waiting: FxHashMap<Lit, Vec<u32>>,
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
    /// in the root, its edges by existential restriction and constant
    succ_set: FxHashSet<(Ex, u32)>,
    /// in the root, the existential restrictions and constants of the edges to each
    /// successor
    by_succ: FxHashMap<Ex, Vec<(Ex, u32)>>,
}

impl RootSt {
    pub fn bytes(&self) -> usize {
        let lists = |m: &FxHashMap<u32, Vec<u32>>| {
            table_bytes(m.capacity(), 32) + m.values().map(|v| v.capacity() * 4).sum::<usize>()
        };
        std::mem::size_of::<RootSt>()
            + lists(&self.by_const)
            + lists(&self.roles_of)
            + lists(&self.edges)
            + table_bytes(self.edge_set.capacity(), 8)
            + table_bytes(self.markers.capacity(), 8)
            + lists(&self.ready)
            + table_bytes(self.sent.capacity(), 8)
            + table_bytes(self.seen.capacity(), 4)
            + table_bytes(self.waiting.capacity(), 32)
            + self
                .waiting
                .values()
                .map(|v| v.capacity() * 4)
                .sum::<usize>()
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
        hyper_at(ix, st, out, TOP, u, &unit(), &mut found);
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
            };
            x.store.next(ix, Some(&o), &mut max)
        };
        let Some(id) = id else {
            break;
        };
        let mut grown = x.store.take_grown();
        // the indexes by constant, before the rules, so that a clause meets itself
        let r = root_of(x, out);
        for &l in &max {
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

/// The rules on one clause of the root's store, whose maximal literals are `max`.
fn process<S: Sink>(ix: &Index, st: &mut State, c: &Clause, max: &[Lit], out: &mut S) {
    let dx = dix(ix);
    let mut found = Found::default();
    join_body(ix, st, c, &mut found, &out.dl().lits);
    for &l in max {
        let prem = side(c, l);
        let q = super::dl_eq::Eqs {
            ix,
            dx,
            lits: &out.dl().lits,
            root: true,
        };
        q.on_max(st, l, &prem, &mut found);
        join_head(st, c, l, &prem, &mut found);
        let v = out.dl().lits.view(l, inv_of(ix, dx));
        if ground_trigger(v) {
            ground_out(ix, st, l, out);
        }
        let (ts, n) = terms_of(v);
        for k in 0..n {
            let RTerm::Fc(e, u) = ts[k] else { continue };
            if ts[..k].contains(&ts[k]) {
                continue;
            }
            let w = succ_at(ix, st, e, u, &mut found, out);
            if let XLit::Atom(a, _) = v {
                super::dl_rules::send_hyp(ix, st, w, a, out);
            }
            let cids = dl(st)
                .and_then(|x| x.pred_in.get(&w))
                .cloned()
                .unwrap_or_default();
            for cid in cids {
                let c = out.dl().table.get(cid);
                root_pred(
                    ix,
                    st,
                    e,
                    u,
                    c,
                    Some((l, &prem)),
                    &mut found,
                    &out.dl().lits,
                );
            }
        }
        match v {
            XLit::Atom(d, RTerm::C(u)) => hyper_at(ix, st, out, d, u, &prem, &mut found),
            v @ XLit::Role(..) => {
                for (u, s, t) in from(v, inv_of(ix, dx)) {
                    on_role(ix, st, out, s, u, t, &prem, &mut found);
                }
            }
            _ => {}
        }
    }
    drain_with(ix, st, found, out, derive);
}

/// The premises in the root whose maximal literal is `l`.
fn partners(st: &State, l: Lit) -> Vec<Side> {
    let Some(d) = dl(st) else {
        return Vec::new();
    };
    d.store
        .by_xl
        .get(&l)
        .into_iter()
        .flatten()
        .filter(|&&i| !d.store.clauses[i as usize].dead)
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

/// Hyper on a new premise `prem` of `d(u)`: the ontology clauses with d in the body, with
/// their x bound to u.
fn hyper_at<S: Sink>(
    ix: &Index,
    st: &State,
    out: &S,
    d: Ex,
    u: u32,
    prem: &Side,
    found: &mut Found,
) {
    let dx = dix(ix);
    let at = |a: Ex| atom(ix, out, a, u);
    let h = ix.head(d);
    match h.node {
        Node::Conj(a, b) => {
            found.push(with(prem, at(a)));
            found.push(with(prem, at(b)));
        }
        Node::Exist(r, _) => {
            let f = RTerm::Fc(d, u);
            for l in [XLit::Role(r, RTerm::C(u), f), XLit::Atom(h.succ, f)] {
                if let Made::Lit(l) = lit(ix, out, l) {
                    found.push(with(prem, l));
                }
            }
        }
        // N_v(x) → x ≈ v
        Node::Nominal(v) if v != u => {
            if let Made::Lit(l) = lit(ix, out, XLit::Eq(RTerm::C(v), RTerm::C(u))) {
                found.push(with(prem, l));
            }
        }
        _ => {}
    }
    // M(x) → f_i(x) ≉ f_j(x) of a minimum's holder
    if let Some(es) = dx.ne_of.get(&d) {
        for (i, &a) in es.iter().enumerate() {
            for &b in &es[i + 1..] {
                if let Made::Lit(n) = lit(ix, out, XLit::Ne(RTerm::Fc(a, u), RTerm::Fc(b, u))) {
                    found.push(with(prem, n));
                }
            }
        }
    }
    for &t in ix.told_of(h) {
        found.push(with(prem, at(t)));
    }
    for &(p, k) in ix.conj_of(h) {
        for q in partners_at(ix, st, out, p, u) {
            found.push(with(&merge(prem, &q), at(k)));
        }
    }
    for &set in ix.disjoint_of(h) {
        for &m in &dx.sets[set as usize] {
            if m != d {
                for q in partners_at(ix, st, out, m, u) {
                    found.push(merge(prem, &q));
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
            found.products(lists, base);
        }
    }
    // the universals d stands for, over the role atoms from u
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
                for &(a, b) in &ps {
                    if dx.sub(ix, t, a)
                        && let Made::Lit(m) = lit(ix, out, XLit::Atom(b, f))
                    {
                        found.push(with(&merge(prem, &side(&s.c, l)), m));
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
/// of u, `owl:Thing` included, and the disjoint roles.
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
) {
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let mut ps = Vec::new();
    let over = |b: Ex, q: &Side, ps: &mut Vec<(Role, Ex)>, found: &mut Found| {
        alls(ix, b, ps);
        for &(t, c) in ps.iter() {
            if dx.sub(ix, s, t)
                && let Made::Lit(m) = lit(ix, out, XLit::Atom(c, f))
            {
                found.push(with(&merge(prem, q), m));
            }
        }
    };
    over(TOP, &unit(), &mut ps, found);
    let Some(x) = dl(st) else { return };
    if let Some(r) = x.root.as_deref() {
        for &i in r.by_const.get(&u).into_iter().flatten() {
            let c = &x.store.clauses[i as usize];
            if c.dead {
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
                if c.dead {
                    continue;
                }
                for &l in x.store.maxima(i) {
                    for (w, t, g) in from(out.dl().lits.view(l, inv), inv) {
                        if w == u && g == f && dx.sub(ix, t, b) {
                            found.push(merge(prem, &side(&c.c, l)));
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
fn derive<S: Sink>(ix: &Index, st: &mut State, (mut body, mut head): Side, out: &mut S) {
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
    // Γ ∧ A → Δ ∨ A
    if head.iter().any(|l| body.binary_search(l).is_ok()) {
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
/// marker is no premise on either side, since its conclusion would be the other premise.
pub(super) fn join_head(st: &State, c: &Clause, l: Lit, prem: &Side, found: &mut Found) {
    if is_marker(c) {
        return;
    }
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
        found.push(merge(prem, &rest));
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
            if !s.dead && !is_marker(&s.c) {
                found.push(merge(&side(&s.c, l), &rest));
            }
        }
    }
}

/// The constants of a literal.
fn consts(v: XLit, out: &mut Vec<u32>) {
    let (ts, n) = terms_of(v);
    for t in &ts[..n] {
        if let RTerm::C(u) = t {
            out.push(*u);
        }
    }
}

/// A literal with every y read as x.
fn over_x(v: XLit) -> XLit {
    let t = |t: RTerm| if t == RTerm::Y { RTerm::X } else { t };
    match v {
        XLit::Atom(a, b) => XLit::Atom(a, t(b)),
        XLit::Role(r, a, b) => XLit::Role(r, t(a), t(b)),
        XLit::Eq(a, b) => XLit::Eq(t(a), t(b)),
        XLit::Ne(a, b) => XLit::Ne(t(a), t(b)),
    }
}

/// The root successor trigger of the journal's Definition 4 whose instance under
/// σ = {y ↦ x} is `v`: `B(u)`, `S(x, u)`, `S(u, x)`, `S(u1, u2)` or `u1 ≈ u2` give `B(u)`,
/// `S(y, u)`, `S(u, y)`, `S(u1, u2)` and `u1 ≈ u2`.
fn trigger_of(v: XLit) -> Option<XLit> {
    use RTerm::{C, X, Y};
    match v {
        XLit::Atom(_, C(_)) | XLit::Eq(C(_), C(_)) | XLit::Role(_, C(_), C(_)) => Some(v),
        XLit::Role(s, X, b @ C(_)) => Some(XLit::Role(s, Y, b)),
        XLit::Role(s, a @ C(_), X) => Some(XLit::Role(s, a, Y)),
        _ => None,
    }
}

/// Whether a root literal is a root successor trigger: `B(u)`, `S(y, u)`, `S(u, y)`,
/// `S(u1, u2)` or `u1 ≈ u2`.
fn is_trigger(v: XLit) -> bool {
    use RTerm::{C, Y};
    matches!(
        v,
        XLit::Atom(_, C(_))
            | XLit::Eq(C(_), C(_))
            | XLit::Role(_, C(_), C(_))
            | XLit::Role(_, Y, C(_))
            | XLit::Role(_, C(_), Y)
    )
}

/// Whether r-Pred takes a head literal of a root clause: a ground literal, or a root
/// predecessor trigger, which is a root successor trigger, `B(y)` or `y ≈ u`.
fn returns(v: XLit) -> bool {
    use RTerm::{C, Y};
    v.is_ground()
        || is_trigger(v)
        || matches!(v, XLit::Atom(_, Y) | XLit::Eq(Y, C(_)) | XLit::Eq(C(_), Y))
}

/// The rules between a context other than the root and the root on a new premise `prem` of
/// the context, whose maximal literal is the interned literal `l`: r-Succ sends the root the
/// marker of a root successor trigger, Join resolves a ground l with the clauses that have
/// it in the body, and r-Pred resolves l with the root's clauses that wait for it.
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
) {
    let dx = dix(ix);
    let v = out.dl().lits.view(l, inv_of(ix, dx));
    if let Some(a) = trigger_of(v)
        && let Made::Lit(m) = lit(ix, out, a)
        && let Some(root) = dx.root
    {
        let mut grown = 0;
        let r = root_of(ds(st), out);
        if let Some(b) = RootSt::ins(&mut r.sent, m, 8) {
            out.send(root, Concl::RHyp(me, m.xid()));
            grown += b;
            let mut us = Vec::new();
            consts(a, &mut us);
            for u in us {
                grown += RootSt::ins(&mut r.edged, u, 4).unwrap_or(0);
            }
        }
        if grown > 0 {
            out.dl().grow(grown);
        }
    }
    if ground_trigger(v) {
        ground_out(ix, st, l, out);
    }
    join_head(st, c, l, prem, found);
    let Some(cids) = dl(st)
        .and_then(|x| x.root.as_deref())
        .and_then(|r| r.waiting.get(&l))
        .cloned()
    else {
        return;
    };
    for cid in cids {
        rpred(
            ix,
            st,
            out.dl().table.get(cid),
            Some((l, prem)),
            found,
            &out.dl().lits,
        );
    }
}

/// r-Pred with the root's clause `c` in a context other than the root: each body atom, read
/// with y as x, takes a premise of the context where it is maximal, and the head is read
/// with y as x. `fixed` is the premise that a new clause gives for one of the body atoms.
fn rpred(
    ix: &Index,
    st: &State,
    c: &Clause,
    fixed: Option<(Lit, &Side)>,
    found: &mut Found,
    lits: &super::dl_clause::Lits,
) {
    let inv = inv_of(ix, dix(ix));
    let mut keys = Vec::with_capacity(c.body().len());
    for &l in c.body() {
        match lits.lit(over_x(lits.view(l, inv)), inv) {
            Made::Lit(k) => keys.push(k),
            // a body literal that holds is no premise, and one that cannot makes the
            // clause hold
            Made::True => {}
            Made::False => return,
        }
    }
    if let Some((p, _)) = fixed
        && !keys.contains(&p)
    {
        return;
    }
    let mut head = Vec::with_capacity(c.head().len());
    for &l in c.head() {
        match lits.lit(over_x(lits.view(l, inv)), inv) {
            Made::Lit(m) => head.push(m),
            Made::False => {}
            Made::True => return,
        }
    }
    let lists: Vec<Vec<Side>> = keys
        .iter()
        .map(|&k| match fixed {
            Some((p, s)) if p == k => vec![s.clone()],
            _ => partners(st, k),
        })
        .collect();
    found.products(lists, (Vec::new(), head));
}

/// r-Pred: a context other than the root takes a clause the root sent.
pub fn rpred_c<S: Sink>(ix: &Index, st: &mut State, cid: u32, out: &mut S) -> bool {
    if st.bottom {
        return false;
    }
    let inv = inv_of(ix, dix(ix));
    let mut grown = 0;
    let r = root_of(ds(st), out);
    match RootSt::ins(&mut r.seen, cid, 4) {
        Some(b) => grown += b,
        None => return false,
    }
    let c = out.dl().table.get(cid).clone();
    for &l in c.body() {
        let lits = &out.dl().lits;
        if let Made::Lit(k) = lits.lit(over_x(lits.view(l, inv)), inv) {
            let table = table_bytes(r.waiting.capacity(), 32);
            let w = r.waiting.entry(k).or_default();
            let cap = w.capacity();
            w.push(cid);
            grown += (w.capacity() - cap) * 4
                + table_bytes(r.waiting.capacity(), 32).saturating_sub(table);
        }
    }
    out.dl().grow(grown);
    let mut found = Found::default();
    rpred(ix, st, &c, None, &mut found, &out.dl().lits);
    drain(ix, st, found, out);
    true
}

/// r-Succ in the root: the context v sends the marker of the root successor trigger with id
/// `xid`. The root registers v for each of the marker's constants, takes the marker, and
/// sends v the clauses that r-Pred gives it for the new registrations.
pub fn rhyp<S: Sink>(ix: &Index, st: &mut State, v: Ex, xid: u32, out: &mut S) -> bool {
    let l = Lit::xl(xid);
    let mut us = Vec::new();
    consts(out.dl().lits.view(l, inv_of(ix, dix(ix))), &mut us);
    register(ix, st, v, us, Some(l), out)
}

/// The context v registers with the root for the constant u, which a ground literal in one
/// of its bodies mentions.
pub fn redge<S: Sink>(ix: &Index, st: &mut State, v: Ex, u: u32, out: &mut S) -> bool {
    register(ix, st, v, vec![u], None, out)
}

/// The root registers v for the constants `us` and takes the marker of `l` when given, and
/// v gets the clauses that r-Pred gives it for the new registrations.
fn register<S: Sink>(
    ix: &Index,
    st: &mut State,
    v: Ex,
    mut us: Vec<u32>,
    l: Option<Lit>,
    out: &mut S,
) -> bool {
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
    let mut marked = false;
    if !bottom
        && let Some(l) = l
        && let Some(b) = RootSt::ins(&mut r.markers, l, 8)
    {
        grown += b;
        marked = true;
        if let Some(b) = x.store.add(Clause::new(vec![l], vec![l])) {
            grown += b;
        }
    }
    if grown > 0 {
        out.dl().grow(grown);
    }
    if new.is_empty() {
        return marked;
    }
    // an unsatisfiable root sends the empty clause to every context registered with it
    if bottom {
        let id = out.dl().empty();
        out.send(v, Concl::RPred(id));
        return true;
    }
    let lits = &out.dl().lits;
    let inv = inv_of(ix, dix(ix));
    let mut ids: Vec<u32> = Vec::new();
    {
        let r = x.root.as_deref().expect("made above");
        for &u in &new {
            for &i in r.ready.get(&u).into_iter().flatten() {
                let s = &x.store.clauses[i as usize];
                if !s.dead && registered(r, lits, inv, &s.c, v) {
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

/// Whether the context v is registered with the root for every constant of the body of the
/// root's clause c. A clause without a body is listed by the constants of its head, each of
/// which it is sent for.
fn registered(
    r: &RootSt,
    lits: &super::dl_clause::Lits,
    inv: impl Fn(Role) -> Option<Role> + Copy,
    c: &Clause,
    v: Ex,
) -> bool {
    let mut us = Vec::new();
    for &l in c.body() {
        consts(lits.view(l, inv), &mut us);
    }
    us.iter().all(|&u| r.edge_set.contains(&(v, u)))
}

/// r-Pred from the root: a processed clause whose body atoms are root successor triggers and
/// whose head literals r-Pred takes goes to the contexts registered for the constants of its
/// body, or for a constant of its head when the body is empty, and is listed for the
/// contexts that register later. A marker stays in the root, since its conclusion would be
/// the context's premise.
fn offer<S: Sink>(ix: &Index, st: &mut State, id: u32, c: &Clause, out: &mut S) {
    if st.bottom || is_marker(c) {
        return;
    }
    let inv = inv_of(ix, dix(ix));
    let lits = &out.dl().lits;
    if !c.body().iter().all(|&l| is_trigger(lits.view(l, inv)))
        || !c.head().iter().all(|&l| returns(lits.view(l, inv)))
    {
        return;
    }
    let mut keys = Vec::new();
    let from = if c.body().is_empty() {
        c.head()
    } else {
        c.body()
    };
    for &l in from {
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
    to.retain(|&v| registered(r, lits, inv, c, v));
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

/// The successors of a context: those it registered with, the EL successors of its unit
/// existential restrictions, and in the root those of its successor terms.
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

/// Succ in the root for the successor term `f_e(u)`: the successor is e's hypothetical
/// successor, which has the empty core. A new edge registers the root with it, sends it
/// the markers of the root's ground literals when it is a new successor, and replays the
/// clauses it already sent for the new constant.
fn succ_at<S: Sink>(
    ix: &Index,
    st: &mut State,
    e: Ex,
    u: u32,
    found: &mut Found,
    out: &mut S,
) -> Ex {
    let dx = dix(ix);
    let w = dx.hyp.get(&e).copied().unwrap_or(TOP);
    let me = dx.root.expect("a rooted run");
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

/// Pred into the root with the clause `c` of the successor of `f_e(u)`: y is read as u and x
/// as `f_e(u)`. Each body atom `B(x)` takes a premise whose maximal literal is `B(f_e(u))`,
/// each role atom `s(y, x)` one whose maximal literal is a role atom below s from u to
/// `f_e(u)`, and the ground body literals stay. `fixed` is the premise that a new clause
/// gives for one of these literals. The successor's core is empty, so no core atom is
/// resolved.
#[allow(clippy::too_many_arguments)]
fn root_pred(
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

/// Pred into the root: the successor w sent a clause over its predecessor, which the root
/// takes for each constant it reaches w by.
pub fn pred_c<S: Sink>(ix: &Index, st: &mut State, w: Ex, cid: u32, out: &mut S) -> bool {
    if st.bottom {
        return false;
    }
    let x = ds(st);
    if !x.pred_seen.insert((w, cid)) {
        return false;
    }
    x.pred_in.entry(w).or_default().push(cid);
    let pairs = x
        .root
        .as_deref()
        .and_then(|r| r.by_succ.get(&w))
        .cloned()
        .unwrap_or_default();
    let mut found = Found::default();
    let c = out.dl().table.get(cid);
    for (e, u) in pairs {
        root_pred(ix, st, e, u, c, None, &mut found, &out.dl().lits);
    }
    drain_with(ix, st, found, out, derive);
    true
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
        // {a} ⊑ ∃r.E, {a} ⊑ A ⊔ B, A ⊑ ∀r.H: A(a) ∨ B(a) gives H(f(a)) ∨ B(a)
        let mut s = Sig::new();
        let na = s.ind("a");
        let [e, a, b, h] = ["E", "A", "B", "H"].map(|n| s.class(n));
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
    fn an_equality_in_a_disjunction_keeps_its_case() {
        // {a} ⊑ {b} ⊔ C, B(b), with a and b different only under D: B(a) ∨ C(a)
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let [b, cc] = ["B", "C"].map(|n| s.class(n));
        s.sub(nb, b);
        s.dl.clauses.push((vec![na], vec![nb, cc]));
        let d = saturate_root(s);
        let (ua, ub) = (c(na, &d.ix), c(nb, &d.ix));
        let lo = ua.min(ub);
        let hi = ua.max(ub);
        // B holds of a when a is b, so B(lo) ∨ C(a) when a is the smaller
        assert!(
            d.clauses().iter().any(|(bd, h)| bd.is_empty()
                && h.contains(&XLit::Eq(lo, hi))
                && h.contains(&XLit::Atom(cc, ua))),
            "a ≈ b ∨ C(a)"
        );
        assert!(!d.has(&[XLit::Atom(b, ua)]) || ua == ub);
        assert!(!d.bottom());
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
        assert!(d.has(&[XLit::Atom(b, o), XLit::Atom(cc, RTerm::X)]));
        assert!(d.has(&[XLit::Atom(b, RTerm::X), XLit::Atom(cc, o)]));
        assert!(!d.has(&[XLit::Atom(b, o)]));
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
        assert!(!ready(vec![XLit::Ne(ua, ub)]));
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
            let [b, cc, dd, ee] = ["B", "C", "D", "E"].map(|n| s.class(n));
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
    fn a_root_clause_that_comes_first_waits_for_its_premise() {
        // the root's r(y, o) → B(y) reaches A's context before A has r(x, o): the premise
        // that comes later meets it, and A ⊑ B
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b] = ["A", "B"].map(|n| s.class(n));
        let r = s.role("r");
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
        let o = c(no, &ix);
        let inv = inv_of(&ix, dix(&ix));
        let l = |v: XLit| match run.lits.lit(v, inv) {
            Made::Lit(l) => l,
            _ => panic!("a literal"),
        };
        let k = run.table.push(Clause::new(
            vec![l(XLit::Role(r, RTerm::Y, o))],
            vec![l(XLit::Atom(b, RTerm::Y))],
        ));
        let mut st = State {
            init: true,
            ..State::default()
        };
        assert!(rpred_c(&ix, &mut st, k, &mut out));
        assert!(
            !rpred_c(&ix, &mut st, k, &mut out),
            "a clause is taken once"
        );
        assert!(out.mine.is_empty());
        ds(&mut st)
            .store
            .add(Clause::new(Vec::new(), vec![l(XLit::Role(r, RTerm::X, o))]));
        super::super::dl_rules::flush(&ix, a, &mut st, &mut out, 100);
        assert!(out.mine.contains(&Concl::Sub(b)));
        // the premise sent the root the marker r(y, o)
        let m = l(XLit::Role(r, RTerm::Y, o));
        assert!(out.sent.contains(&(root, Concl::RHyp(a, m.xid()))));
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
}
