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
    DlIx, DlSt, Found, MREPAIR_CF, MREPAIR_HOLDER, MREPAIR_NO_REPLAY, MREPAIR_NO_RETURN,
    MREPAIR_ON, MREPAIR_UNITS, MREPAIR_WIDE, RECORD_SLACK, Side, WORK_READS, alls, dix, dl, drain,
    drain_with, ds, merge, pred_id, restricted, side, unit, with,
};
use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use super::saturate::{Concl, Sink, State, table_bytes};
use rustc_hash::{FxHashMap, FxHashSet};

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
    /// row 1231's marker repair state, made with its first use and only with the repair
    m: Option<Box<MRep>>,
}

/// What a context keeps for row 1231's marker repair (PLAN v3.1's rule M), apart from
/// [`RootSt`] so that a run without the repair holds none of it.
#[derive(Default, Clone)]
struct MRep {
    /// with row 1231's marker repair, in another context, the ground literals it told the
    /// root it holds (M-send)
    mheld: FxHashSet<Lit>,
    /// with the repair, in another context, the clauses the root returned to it, by table
    /// id, under each literal of their bodies (M-return)
    rets: FxHashMap<Lit, Vec<u32>>,
    /// with the repair, in the root, the contexts that hold each ground literal
    holders: FxHashMap<Lit, Vec<Ex>>,
    /// with the repair, in the root, the same holders as pairs of context and literal
    hold_set: FxHashSet<(Ex, Lit)>,
    /// with the repair, in the root, the held literals by id, under each of their constants
    held_at: FxHashMap<u32, Vec<u32>>,
    /// with the repair, in the root, its processed clauses whose bodies are ground
    /// triggers, under each body literal
    gbody: FxHashMap<Lit, Vec<u32>>,
    /// with the repair's pair set, whether the root reaches the context by successors,
    /// which the root is from its start
    reached: bool,
    /// with the repair's pair set, in the root, the contexts it reaches by successors that
    /// told it so, and the constants each context registered for
    reach_set: FxHashSet<Ex>,
    regs: FxHashMap<Ex, Vec<u32>>,
    /// with the repair's pair set, in a context the root reaches, the successors it told
    told: FxHashSet<Ex>,
    /// with the repair, in the root, the ids of its processed clauses with an empty body
    /// and one ground literal in the head, under that literal
    units: FxHashMap<Lit, Vec<u32>>,
}

impl MRep {
    fn bytes(&self) -> usize {
        fn lists<K>(m: &FxHashMap<K, Vec<u32>>) -> usize {
            table_bytes(m.capacity(), 32) + m.values().map(|v| v.capacity() * 4).sum::<usize>()
        }
        std::mem::size_of::<MRep>()
            + table_bytes(self.mheld.capacity(), 8)
            + lists(&self.rets)
            + lists(&self.holders)
            + table_bytes(self.hold_set.capacity(), 16)
            + lists(&self.held_at)
            + lists(&self.gbody)
            + table_bytes(self.reach_set.capacity(), 4)
            + lists(&self.regs)
            + table_bytes(self.told.capacity(), 4)
            + lists(&self.units)
    }
}

impl RootSt {
    /// The existential restrictions and constants of the context's edges over `f_e(u)` to
    /// the successor w.
    pub(super) fn edges_to(&self, w: Ex) -> &[(Ex, u32)] {
        self.by_succ.get(&w).map_or(&[][..], |v| v)
    }

    pub fn bytes(&self) -> usize {
        fn lists<K>(m: &FxHashMap<K, Vec<u32>>) -> usize {
            table_bytes(m.capacity(), 32) + m.values().map(|v| v.capacity() * 4).sum::<usize>()
        }
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
            + self.m.as_deref().map_or(0, MRep::bytes)
    }

    /// The repair's state, which [`mrep_new`] made.
    fn mr(&self) -> &MRep {
        self.m.as_deref().expect("made by mrep_new")
    }
    fn mw(&mut self) -> &mut MRep {
        self.m.as_deref_mut().expect("made by mrep_new")
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
    let r = root_of(ds(st), out);
    if out.dl().mrepair & MREPAIR_ON != 0 {
        mrep_new(r, out);
        r.mw().reached = true;
    }
    let mut found = Found::default();
    found.root = true;
    found.rule = "init";
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
        m_offer(ix, st, id, &c, out);
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
    found.root = true;
    if super::dl_rules::tracing() {
        let lits = &out.dl().lits;
        eprintln!(
            "TR P R - {} -> {}",
            super::dl_rules::tr(ix, lits, c.body()),
            super::dl_rules::tr(ix, lits, c.head())
        );
    }
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
            if let Some((s, u)) = to_const(v, inv_of(ix, dx)) {
                univ_to_const(ix, st, out, s, u, prem, found);
            }
        }
        _ => {}
    }
}

/// The role atom `v` read from `x` to a constant: `(s, u)` for `s(x, u)`, by the inverse
/// role when x is its second term.
fn to_const(v: XLit, inv: impl Fn(Role) -> Option<Role>) -> Option<(Role, u32)> {
    match v {
        XLit::Role(s, RTerm::X, RTerm::C(u)) => Some((s, u)),
        XLit::Role(s, RTerm::C(u), RTerm::X) => inv(s).map(|i| (i, u)),
        _ => None,
    }
}

/// Hyper on a new premise `prem` of the role atom `s(x, u)` with the universal restrictions
/// that the holders of `x` stand for: `∀t.C(x) ∧ s(x, u) → C(u)` for `s ⊑ t`, with the
/// unit holders and the clauses whose maximal literal is a holder over `x`. The inference
/// is local to the context, since no predecessor sees `s(x, u)`, so a selected premise
/// takes part.
fn univ_to_const<S: Sink>(
    ix: &Index,
    st: &State,
    out: &S,
    s: Role,
    u: u32,
    prem: &Side,
    found: &mut Found,
) {
    found.rule = "univc";
    let dx = dix(ix);
    let mut ps = Vec::new();
    let mut over = |b: Ex, q: &Side, found: &mut Found| {
        alls(ix, b, &mut ps);
        for &(t, c) in ps.iter() {
            if dx.sub(ix, s, t)
                && let Made::Lit(m) = lit(ix, out, XLit::Atom(c, RTerm::C(u)))
            {
                found.push(with(&merge(prem, q), m));
            }
        }
    };
    over(TOP, &unit(), found);
    for &b in &st.subs {
        if b != TOP {
            over(b, &unit(), found);
        }
    }
    let Some(d) = dl(st) else { return };
    let mut holders: Vec<Ex> = d.store.by_x.keys().copied().collect();
    holders.sort_unstable();
    for b in holders {
        if st.subs.contains(&b) {
            continue;
        }
        for i in d.store.with_x(b) {
            let p = side(&d.store.clauses[i as usize].c, Lit::x(b));
            over(b, &p, found);
        }
    }
}

/// Hyper on a new premise `prem` of a holder `d(x)` of universal restrictions with the
/// processed role atoms from `x` to a constant: `∀t.C(x) ∧ s(x, u) → C(u)` for `s ⊑ t`,
/// the other arrival order of [`univ_to_const`].
pub(super) fn univ_from_x(
    ix: &Index,
    st: &State,
    lits: &super::dl_clause::Lits,
    d: Ex,
    prem: &Side,
    found: &mut Found,
) {
    found.rule = "univx";
    let dx = dix(ix);
    let mut ps = Vec::new();
    alls(ix, d, &mut ps);
    if ps.is_empty() {
        return;
    }
    let Some(x) = dl(st) else { return };
    let Some(r) = x.root.as_deref() else { return };
    let inv = inv_of(ix, dx);
    let mut ids: Vec<u32> = r.roles_of.values().flatten().copied().collect();
    ids.sort_unstable();
    ids.dedup();
    for i in ids {
        let c = &x.store.clauses[i as usize];
        if c.dead {
            continue;
        }
        for &l in x.store.maxima(i) {
            let Some((s, u)) = to_const(lits.view(l, inv), inv) else {
                continue;
            };
            for &(t, b) in &ps {
                if dx.sub(ix, s, t)
                    && let Made::Lit(m) = lits.lit(XLit::Atom(b, RTerm::C(u)), inv)
                {
                    found.push(with(&merge(prem, &side(&c.c, l)), m));
                }
            }
        }
    }
}

/// The premises of a ground inference whose maximal literal is `l`, the selected clauses
/// and the markers left out.
fn partners(st: &State, l: Lit) -> Vec<Side> {
    partners_of(st, l, false)
}

/// [`partners`], with the selected clauses too when `sel` is set.
fn partners_of(st: &State, l: Lit, sel: bool) -> Vec<Side> {
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
            !s.dead && (sel || !s.sel)
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
    found.rule = "hyperc";
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
            let f = match super::dl_rules::into_nominal(ix, d) {
                Some((_, v)) => RTerm::C(v),
                None => RTerm::Fc(d, u),
            };
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
    found.rule = "hyperr";
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
    found.rule = "onrole";
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
    out.worked(1);
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
/// conclusion as [`super::dl_rules::join_exempt`] says.
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
    found.rule = "joinh";
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
        if !super::dl_rules::join_restricted(lits, inv, &prem.0, &c) {
            found.push(c);
        }
    }
}

/// Join on a new clause `c` with ground literals in its body: for each, the processed
/// clauses where it is maximal. The restriction on ground inferences applies to its
/// conclusions as [`super::dl_rules::join_exempt`] says.
pub(super) fn join_body(
    ix: &Index,
    st: &State,
    c: &Clause,
    found: &mut Found,
    lits: &super::dl_clause::Lits,
) {
    found.rule = "joinb";
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
                if !super::dl_rules::join_restricted(lits, inv, s.c.body(), &c) {
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
        if out.dl().mrepair & MREPAIR_ON != 0
            && !is_marker(c)
            && let Some(root) = dx.root
            && !m_note(ix, root, me, st, l, prem, out)
        {
            return;
        }
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
    found.rule = "rpred";
    found.push((Vec::new(), c.head().to_vec()));
    ds(st).taking_fact = true;
    drain(ix, st, found, out);
    ds(st).taking_fact = false;
    true
}

/// The root takes a fact that a hypothetical successor derived, a clause with an empty
/// body whose literals hold neither `x` nor `y`: the successor has the empty core and no
/// assumption, so its clauses hold of every element, and this one holds in every model of
/// the ontology. The root derives it as its own, and the redundancy of its store leaves out
/// one that it holds already.
pub fn rfact<S: Sink>(ix: &Index, st: &mut State, cid: u32, out: &mut S) -> bool {
    if st.bottom {
        return false;
    }
    let c = out.dl().table.get(cid);
    debug_assert!(c.body().is_empty(), "a promoted fact has no body");
    let mut found = Found::default();
    found.root = true;
    found.rule = "rfact";
    found.push((Vec::new(), c.head().to_vec()));
    drain_with(ix, st, found, out, derive);
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
    let pairs = out.dl().mrepair & (MREPAIR_ON | MREPAIR_WIDE) == MREPAIR_ON;
    for &u in &us {
        if let Some(b) = RootSt::ins(&mut r.edge_set, (v, u), 8) {
            grown += b + RootSt::push(&mut r.edges, u, v);
            // a context the root reaches already takes its markers below, so only one
            // that it does not reach yet waits for its reach
            if pairs {
                mrep_new(r, out);
                if !r.mr().reach_set.contains(&v) {
                    grown += RootSt::push(&mut r.mw().regs, v, u);
                }
            }
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
    if out.dl().mrepair & MREPAIR_ON != 0
        && (!pairs
            || x.root
                .as_deref()
                .and_then(|r| r.m.as_deref())
                .is_some_and(|m| m.reach_set.contains(&v)))
    {
        m_registered(x, v, &new, out);
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
/// and the EL successors of its unit existential restrictions, other than those into a
/// singleton, which name their constant in place of a successor (item 822).
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
        if let Node::Exist(..) = h.node
            && super::dl_rules::into_nominal(ix, d).is_none()
        {
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
    if out.dl().mrepair & (MREPAIR_ON | MREPAIR_WIDE) == MREPAIR_ON
        && dl(st)
            .and_then(|x| x.root.as_deref())
            .and_then(|r| r.m.as_deref())
            .is_some_and(|m| m.reached)
    {
        m_tell(st, w, out);
    }
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
    found.rule = "succ";
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
    root_pred_of(
        ix,
        st,
        e,
        u,
        c,
        fixed,
        found,
        lits,
        super::dl_rules::sel_partners_x(),
    );
}

/// [`root_pred`], whose premises over `f_e(u)` include the selected clauses when `sel` is
/// set, as Hyper over x has them under [`super::dl_rules::sel_partners_x`].
#[allow(clippy::too_many_arguments)]
fn root_pred_of(
    ix: &Index,
    st: &State,
    e: Ex,
    u: u32,
    c: &Clause,
    fixed: Option<(Lit, &Side)>,
    found: &mut Found,
    lits: &super::dl_clause::Lits,
    sel: bool,
) {
    found.rule = "rpredc";
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
            Need::Atom(k) => partners_of(st, *k, sel),
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

/// Row 1231's marker repair for a run, as [`DlRun::mrepair`](super::dl_rules::DlRun)
/// holds it: `on` is [`Limits::rooted_marker_repair`](super::Limits), and
/// `SPARKLES_ROOTED_MARKER_REPAIR` overrides it for measurements with `0`, `1` or a sum of
/// the bits [`MREPAIR_ON`], [`MREPAIR_HOLDER`], [`MREPAIR_UNITS`], [`MREPAIR_WIDE`] and
/// [`MREPAIR_CF`].
pub(super) fn marker_repair(on: bool) -> u8 {
    static ENV: std::sync::OnceLock<Option<u8>> = std::sync::OnceLock::new();
    let env = *ENV.get_or_init(|| {
        std::env::var("SPARKLES_ROOTED_MARKER_REPAIR")
            .ok()
            .and_then(|v| v.parse().ok())
    });
    let test = TEST_MODE.load(std::sync::atomic::Ordering::Relaxed);
    env.unwrap_or(if test != 0 {
        test
    } else if on {
        MREPAIR_ON
    } else {
        0
    })
}

/// The repair's mode and the diagnostic that [`marker_test_mode`] sets, for the deficits'
/// tests, which run in a test binary of their own.
static TEST_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static TEST_DIAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Sets the repair's mode for the runs that start from here on, which the environment
/// overrides, `0` for [`Limits::rooted_marker_repair`](super::Limits)'s, and turns on the
/// diagnostic's deficits in [`Stats::dl_marker_deficits`](super::Stats), for tests.
pub fn marker_test_mode(mode: u8, diag: bool) {
    TEST_MODE.store(mode, std::sync::atomic::Ordering::Relaxed);
    TEST_DIAG.store(diag, std::sync::atomic::Ordering::Relaxed);
}

/// M-send in the context `me`, other than the root, on its new premise `prem` whose maximal
/// literal `l` is a ground trigger of a processed clause other than a marker: the context
/// tells the root once that it holds l, and the root sends l's marker to the contexts
/// registered for l's constants. The premise also meets the clauses the root returned
/// with l in their bodies (M-return's late premise, [`m_late`]). It is false when a stop of
/// the run cut the replay off, which marks the store interrupted.
fn m_note<S: Sink>(
    ix: &Index,
    root: Ex,
    me: Ex,
    st: &mut State,
    l: Lit,
    prem: &Side,
    out: &mut S,
) -> bool {
    if m_stopped(st, out) {
        return false;
    }
    let r = rst(st, out);
    mrep_new(r, out);
    if let Some(b) = RootSt::ins(&mut r.mw().mheld, l, 8) {
        out.worked(1);
        out.dl().grow(b);
        out.send(root, Concl::RMark(me, l.xid()));
    }
    let cf = cf_of(ix, out);
    if cf.is_some_and(|cf| ground_body(cf, &prem.0)) {
        return true;
    }
    m_late(ix, st, l, prem, out)
}

/// M-return's late premise `prem`, whose maximal literal is `l`: the clauses the root
/// returned with l in their bodies are resolved with it in batches, each drained before the
/// next is made, so that the many returns of a literal are never prepared whole. A batch
/// closes after [`M_POLL`] returns or once its products hold [`RECORD_SLACK`] bytes, and its
/// bytes are admitted to the run's memory before it drains and released after, so what one
/// batch holds uncharged is at most the slack and one return's lists. The admission checks
/// the run's bounds, and a batch with nothing to admit polls them instead. A refusal, or a
/// drain that stopped, ends the replay with the store interrupted, as [`drain`] leaves it,
/// and the function is false. The conclusions are charged as `drain` charges them.
/// The premise lists of a batch's returns are read without a count of the reads, as the
/// premise lists of the other rules are, so up to [`M_POLL`] returns' preparation runs between
/// two checks (roadmap item 1280).
fn m_late<S: Sink>(ix: &Index, st: &mut State, l: Lit, prem: &Side, out: &mut S) -> bool {
    let mut batch = Found::default();
    let mut since = 0;
    for k in 0.. {
        if st.bottom {
            return true;
        }
        let Some(cid) = rst(st, out)
            .mr()
            .rets
            .get(&l)
            .and_then(|v| v.get(k).copied())
        else {
            break;
        };
        let c = out.dl().table.get(cid).clone();
        ret_products(st, &c, Some((l, prem)), cf_of(ix, out), &mut batch);
        since += 1;
        if since >= M_POLL || batch.held_bytes() >= RECORD_SLACK {
            since = 0;
            if !m_flush(ix, st, &mut batch, out) {
                return false;
            }
        }
    }
    since == 0 || m_flush(ix, st, &mut batch, out)
}

/// Admits and drains one batch of [`m_late`], false when the run stopped.
fn m_flush<S: Sink>(ix: &Index, st: &mut State, batch: &mut Found, out: &mut S) -> bool {
    let b = std::mem::take(batch);
    let held = b.held_bytes();
    let go = if held > 0 {
        out.reserve_data(held)
    } else {
        out.go_on()
    };
    if !go {
        out.release_data(held);
        ds(st).store.interrupted = true;
        return false;
    }
    let was = ds(st).store.interrupted;
    drain(ix, st, b, out);
    out.release_data(held);
    was || !ds(st).store.interrupted
}

/// The repair's pair set: the root reaches the context `me` by successors, from a
/// predecessor that it reaches. The context tells the root, which sends it the markers of
/// the held literals over the constants it registered for, and tells its successors in
/// turn. In the root, the message is a context's, `e`, that the root now reaches.
pub fn rreach<S: Sink>(ix: &Index, me: Ex, st: &mut State, e: Ex, out: &mut S) -> bool {
    let Some(root) = dix(ix).root else {
        return false;
    };
    if m_stopped(st, out) {
        return false;
    }
    if me == root {
        let bottom = st.bottom;
        let x = m_dl(st, out);
        let r = root_of(x, out);
        mrep_new(r, out);
        let Some(b) = RootSt::ins(&mut r.mw().reach_set, e, 4) else {
            return false;
        };
        out.dl().grow(b);
        if bottom {
            return true;
        }
        // the registrations that waited for the reach are replayed once, and the ones
        // that come later go to the context as they come ([`register`])
        let us = r.mw().regs.remove(&e).unwrap_or_default();
        m_registered(x, e, &us, out);
        return true;
    }
    let r = root_of(m_dl(st, out), out);
    mrep_new(r, out);
    if r.mr().reached {
        return false;
    }
    r.mw().reached = true;
    out.send(root, Concl::RReach(me));
    out.worked(1);
    for (k, w) in successors(ix, st).into_iter().enumerate() {
        if !m_poll(k, st, out) {
            break;
        }
        m_tell(st, w, out);
    }
    true
}

/// A context that the root reaches tells its successor `w` so, once.
fn m_tell<S: Sink>(st: &mut State, w: Ex, out: &mut S) {
    let m = rst(st, out).mw();
    if let Some(b) = RootSt::ins(&mut m.told, w, 4) {
        out.dl().grow(b);
        out.send(w, Concl::RReach(TOP));
        out.worked(1);
    }
}

/// The repair's handlers take no notice once the run has stopped, which ends it without a
/// claim: the context's store, when it has one, is interrupted, as [`drain`] leaves it, and
/// the handler makes nothing, so that a stopped run's queued messages cost no more than
/// this test.
fn m_stopped<S: Sink>(st: &mut State, out: &S) -> bool {
    if !out.stopped() {
        return false;
    }
    if let Some(x) = st.extra.as_mut().and_then(|x| x.dl.as_deref_mut()) {
        x.store.interrupted = true;
    }
    true
}

/// The context's state for Tier D, which the repair's reach makes in a context that has
/// none, such as one of the EL rules only: the run's memory takes the parts it makes.
fn m_dl<'a, S: Sink>(st: &'a mut State, out: &S) -> &'a mut DlSt {
    let mut made = 0;
    if st.extra.is_none() {
        made += std::mem::size_of::<super::saturate::Extra>();
    }
    if dl(st).is_none() {
        made += std::mem::size_of::<DlSt>();
    }
    if made > 0 {
        out.dl().grow(made);
    }
    ds(st)
}

/// A scan of the repair whose reads mostly send nothing: it counts them, charges a
/// conclusion for each [`WORK_READS`] of them and checks the run's bounds after every
/// [`M_SCAN`] of them.
struct Scan(usize);

impl Scan {
    /// Counts `n` reads, false when a check of the bounds stopped the scan.
    fn read<S: Sink>(&mut self, n: usize, out: &mut S) -> bool {
        let before = self.0 / M_SCAN;
        self.0 += n;
        let polls = self.0 / M_SCAN - before;
        if polls == 0 {
            return true;
        }
        out.worked(polls * (M_SCAN / WORK_READS));
        out.go_on()
    }

    /// Charges the reads since the last check.
    fn end<S: Sink>(self, out: &mut S) {
        out.worked(self.0 % M_SCAN / WORK_READS);
    }
}

/// Admits `bytes` of a handler's scratch before the handler builds it, false when the run
/// refused them, which marks the store `x` interrupted. The caller releases the bytes
/// either way.
fn m_admit<S: Sink>(x: &mut DlSt, bytes: usize, out: &mut S) -> bool {
    if out.reserve_data(bytes) {
        return true;
    }
    x.store.interrupted = true;
    false
}

/// The repair's sends poll the run's bounds after every [`M_POLL`] of them, as a rule whose
/// one inference sends many messages does: `k` counts the loop's sends. A stop ends the
/// loop and marks the context's store interrupted, as [`drain`] does. It comes only when the
/// run has stopped or an activation of a run in steps is past the bound, which both end the
/// run without a claim, so a send left undone never hides behind a complete answer.
fn m_poll<S: Sink>(k: usize, st: &mut State, out: &mut S) -> bool {
    let go = if k == 0 {
        // the first send of a loop tests the stop without a check of the bounds
        !out.stopped()
    } else {
        !k.is_multiple_of(M_POLL) || out.sent_go_on()
    };
    if go {
        return true;
    }
    ds(st).store.interrupted = true;
    false
}

/// The sends between two polls of the run's bounds in the repair's loops.
const M_POLL: usize = 64;

/// The reads between two polls of the run's bounds in the repair's scans.
const M_SCAN: usize = 1024;

/// Makes the repair's state of a context with its first use, and counts it.
fn mrep_new<S: Sink>(r: &mut RootSt, out: &S) {
    if r.m.is_none() {
        out.dl().grow(std::mem::size_of::<MRep>());
        r.m = Some(Box::default());
    }
}

/// Whether the ground literal `l` is a live unit of the root, by the index of units that
/// [`m_offer`] keeps.
fn root_unit(x: &DlSt, l: Lit) -> bool {
    x.root
        .as_deref()
        .and_then(|r| r.m.as_deref())
        .and_then(|m| m.units.get(&l))
        .is_some_and(|v| v.iter().any(|&i| !x.store.clauses[i as usize].dead))
}

/// The root sends the marker of `l` to the context `d`, once.
fn m_fwd<S: Sink>(r: &mut RootSt, d: Ex, l: Lit, out: &mut S) {
    if let Some(b) = RootSt::ins(&mut r.gsent, (d, l), 16) {
        out.dl().grow(b);
        out.send(d, Concl::HypL(l.xid()));
        out.worked(1);
        out.dl().mcount[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The table id of the root's processed clause `i`, which a returned clause keeps whether
/// or not it was subsumed since, since it holds in every model either way.
fn ret_id<S: Sink>(x: &mut DlSt, i: u32, out: &mut S) -> u32 {
    let s = &mut x.store.clauses[i as usize];
    match s.sent {
        Some(id) => id,
        None => {
            let id = out.dl().table.push(s.c.clone());
            s.sent = Some(id);
            id
        }
    }
}

/// The root takes M-send's notice that the context `h` holds the ground literal `xid`:
/// the contexts registered for one of its constants take its marker, unless it is a live
/// unit of the root, which they take as a fact already, and `h` takes the root's clauses
/// whose ground bodies it now holds every literal of (M-return).
pub fn rmark<S: Sink>(ix: &Index, st: &mut State, h: Ex, xid: u32, out: &mut S) -> bool {
    if st.bottom || m_stopped(st, out) {
        return false;
    }
    let dx = dix(ix);
    let Some(root) = dx.root else { return false };
    let l = Lit::xl(xid);
    let mut us = Vec::new();
    consts(out.dl().lits.view(l, inv_of(ix, dx)), &mut us);
    us.sort_unstable();
    us.dedup();
    let mode = out.dl().mrepair;
    let x = ds(st);
    mrep_new(root_of(x, out), out);
    let r = x.root.as_deref_mut().expect("made above");
    let Some(mut grown) = RootSt::ins(&mut r.mw().hold_set, (h, l), 16) else {
        return false;
    };
    out.dl().mcount[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let table = table_bytes(r.mr().holders.capacity(), 32);
    let first = !r.mr().holders.contains_key(&l);
    let hs = r.mw().holders.entry(l).or_default();
    let cap = hs.capacity();
    hs.push(h);
    grown += (hs.capacity() - cap) * 4
        + table_bytes(r.mr().holders.capacity(), 32).saturating_sub(table);
    if first {
        for &u in &us {
            grown += RootSt::push(&mut r.mw().held_at, u, xid);
        }
    }
    out.dl().grow(grown);
    let unit = mode & MREPAIR_UNITS == 0 && root_unit(x, l);
    if !unit {
        // the destinations, admitted before they are listed
        let r = x.root.as_deref().expect("made above");
        let n: usize = us.iter().map(|u| r.edges.get(u).map_or(0, Vec::len)).sum();
        let bytes = n * std::mem::size_of::<Ex>();
        if !m_admit(x, bytes, out) {
            out.release_data(bytes);
            return true;
        }
        // the registrations are read as a scan, charged and polled, and the destinations
        // are built in the capacity admitted for them
        let r = x.root.as_deref_mut().expect("made above");
        let mut scan = Scan(0);
        let mut to: Vec<Ex> = Vec::with_capacity(n);
        let mut cut = false;
        'scan: for u in &us {
            for &d in r.edges.get(u).into_iter().flatten() {
                if !scan.read(1, out) {
                    cut = true;
                    break 'scan;
                }
                if d != root
                    && (d != h || mode & MREPAIR_HOLDER != 0)
                    && (mode & MREPAIR_WIDE != 0 || r.mr().reach_set.contains(&d))
                {
                    to.push(d);
                }
            }
        }
        scan.end(out);
        if !cut {
            to.sort_unstable();
            to.dedup();
            for (k, d) in to.into_iter().enumerate() {
                if k > 0 && k.is_multiple_of(M_POLL) && !out.sent_go_on() {
                    cut = true;
                    break;
                }
                m_fwd(r, d, l, out);
            }
        }
        out.release_data(bytes);
        if cut {
            x.store.interrupted = true;
            return true;
        }
    }
    if mode & MREPAIR_NO_RETURN != 0 {
        return true;
    }
    // the returns: the root's clauses with l in the body whose other body literals h
    // holds, their ids admitted before they are listed and the scan charged and polled
    let g = x
        .root
        .as_deref()
        .and_then(|r| r.mr().gbody.get(&l))
        .map_or(0, Vec::len);
    let bytes = g * 4;
    if !m_admit(x, bytes, out) {
        out.release_data(bytes);
        return true;
    }
    let r = x.root.as_deref().expect("made above");
    let mut scan = Scan(0);
    let mut ids: Vec<u32> = Vec::with_capacity(g);
    let mut cut = false;
    'scan: for &i in r.mr().gbody.get(&l).into_iter().flatten() {
        let mut all = true;
        for &m in x.store.clauses[i as usize].c.body() {
            // each body literal checked is a read
            if !scan.read(1, out) {
                cut = true;
                break 'scan;
            }
            if m != l && !r.mr().hold_set.contains(&(h, m)) {
                all = false;
                break;
            }
        }
        if all {
            ids.push(i);
        }
    }
    scan.end(out);
    if cut {
        x.store.interrupted = true;
        ids.clear();
    }
    for (k, i) in ids.into_iter().enumerate() {
        if !m_poll(k, st, out) {
            break;
        }
        let cid = ret_id(ds(st), i, out);
        out.send(h, Concl::RRet(cid));
        out.worked(1);
        out.dl().mcount[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    out.release_data(bytes);
    true
}

/// The root registered the context `v` for the constants `new`: v takes the markers of the
/// literals over them that a context other than v holds, unless they are live units.
fn m_registered<S: Sink>(x: &mut DlSt, v: Ex, new: &[u32], out: &mut S) {
    let mode = out.dl().mrepair;
    if mode & MREPAIR_NO_REPLAY != 0 {
        return;
    }
    if out.stopped() {
        x.store.interrupted = true;
        return;
    }
    let Some(r) = x.root.as_deref().filter(|r| r.m.is_some()) else {
        return;
    };
    // the held literals over the constants, admitted before they are listed; the sizing,
    // the listing and the sends are read as one scan, a read per constant and per pair
    let mut scan = Scan(0);
    let mut n = 0;
    let mut cut = false;
    for u in new {
        if !scan.read(1, out) {
            cut = true;
            break;
        }
        n += r.mr().held_at.get(u).map_or(0, Vec::len);
    }
    if cut {
        scan.end(out);
        x.store.interrupted = true;
        return;
    }
    let bytes = n * std::mem::size_of::<(u32, u32)>();
    if !m_admit(x, bytes, out) {
        scan.end(out);
        out.release_data(bytes);
        return;
    }
    let r = x.root.as_deref().expect("read above");
    let mut ls: Vec<(u32, u32)> = Vec::with_capacity(n);
    'list: for &u in new {
        if !scan.read(1, out) {
            cut = true;
            break;
        }
        for &i in r.mr().held_at.get(&u).into_iter().flatten() {
            if !scan.read(1, out) {
                cut = true;
                break 'list;
            }
            ls.push((i, u));
        }
    }
    if cut {
        ls.clear();
    }
    ls.sort_unstable();
    ls.dedup_by_key(|p| p.0);
    for (k, (i, _)) in ls.into_iter().enumerate() {
        if (k > 0 && k.is_multiple_of(M_POLL) && !out.sent_go_on()) || !scan.read(1, out) {
            cut = true;
            break;
        }
        let l = Lit::xl(i);
        let r = x.root.as_deref().expect("read above");
        let held = r.mr().holders.get(&l).is_some_and(|hs| {
            hs.iter().any(|&h| h != v) || (mode & MREPAIR_HOLDER != 0 && !hs.is_empty())
        });
        if !held || (mode & MREPAIR_UNITS == 0 && root_unit(x, l)) {
            continue;
        }
        m_fwd(x.root.as_deref_mut().expect("read above"), v, l, out);
    }
    scan.end(out);
    out.release_data(bytes);
    if cut {
        x.store.interrupted = true;
    }
}

/// M-return in the root: a processed clause `c` whose body is ground triggers goes to each
/// context that holds every body literal, and is listed under its body literals for the
/// holders that come later. The head may be empty, so a denial returns too.
fn m_offer<S: Sink>(ix: &Index, st: &mut State, id: u32, c: &Clause, out: &mut S) {
    let mode = out.dl().mrepair;
    if mode & MREPAIR_ON == 0 || st.bottom || m_stopped(st, out) {
        return;
    }
    if c.body().is_empty() {
        // a unit of the root, for [`root_unit`]
        if let &[l] = c.head()
            && l.kind() == XL
        {
            let r = root_of(ds(st), out);
            mrep_new(r, out);
            let b = RootSt::push(&mut r.mw().units, l, id);
            out.dl().grow(b);
        }
        return;
    }
    if mode & MREPAIR_NO_RETURN != 0 {
        return;
    }
    let inv = inv_of(ix, dix(ix));
    if !c
        .body()
        .iter()
        .all(|&l| l.kind() == XL && ground_trigger(out.dl().lits.view(l, inv)))
    {
        return;
    }
    let x = ds(st);
    let r = root_of(x, out);
    mrep_new(r, out);
    let mut grown = 0;
    for &l in c.body() {
        grown += RootSt::push(&mut r.mw().gbody, l, id);
    }
    out.dl().grow(grown);
    // the holders of the body literal with the fewest, admitted before they are listed,
    // and the scan of the others' charged and polled
    let len = |l: &Lit| r.mr().holders.get(l).map_or(0, Vec::len);
    let first = *c.body().iter().min_by_key(|l| len(l)).expect("a body");
    let cap = len(&first);
    let bytes = cap * std::mem::size_of::<Ex>();
    if !m_admit(x, bytes, out) {
        out.release_data(bytes);
        return;
    }
    let r = x.root.as_deref().expect("made above");
    let mut scan = Scan(0);
    let mut hs: Vec<Ex> = Vec::with_capacity(cap);
    let mut cut = false;
    'scan: for &h in r.mr().holders.get(&first).into_iter().flatten() {
        let mut all = true;
        for &m in c.body() {
            // each body literal checked is a read
            if !scan.read(1, out) {
                cut = true;
                break 'scan;
            }
            if m != first && !r.mr().hold_set.contains(&(h, m)) {
                all = false;
                break;
            }
        }
        if all {
            hs.push(h);
        }
    }
    scan.end(out);
    if cut {
        x.store.interrupted = true;
        hs.clear();
    }
    if !hs.is_empty() {
        let cid = ret_id(x, id, out);
        for (k, h) in hs.into_iter().enumerate() {
            if !m_poll(k, st, out) {
                break;
            }
            out.send(h, Concl::RRet(cid));
            out.worked(1);
            out.dl().mcount[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    out.release_data(bytes);
}

/// A context other than the root takes a clause the root returned (M-return), once: each
/// choice of one of its processed clauses, other than a marker, with each body literal
/// maximal, resolved with the returned clause on those literals, keeping the premises'
/// bodies and residual heads. The clause is kept under its body literals for the premises
/// that come later.
pub fn rret<S: Sink>(ix: &Index, st: &mut State, cid: u32, out: &mut S) -> bool {
    if st.bottom || m_stopped(st, out) {
        return false;
    }
    let r = root_of(m_dl(st, out), out);
    match RootSt::ins(&mut r.seen, cid, 4) {
        Some(b) => out.dl().grow(b),
        None => return false,
    }
    mrep_new(r, out);
    let c = out.dl().table.get(cid).clone();
    let mut grown = 0;
    for &l in c.body() {
        grown += RootSt::push(&mut r.mw().rets, l, cid);
    }
    out.dl().grow(grown);
    let mut found = Found::default();
    ret_products(st, &c, None, cf_of(ix, out), &mut found);
    drain(ix, st, found, out);
    true
}

/// M-return's resolution of the returned clause `c` in a context: for each body literal,
/// the context's processed clauses other than markers where it is maximal, selected ones
/// included, or the premise `fixed` for its literal. With `cf`, M-return-cf, the premises
/// whose body has a ground literal are left out ([`ret_lists`]).
fn ret_products(
    st: &State,
    c: &Clause,
    fixed: Option<(Lit, &Side)>,
    cf: Option<Cf>,
    found: &mut Found,
) {
    found.rule = "mret";
    let body = c.body();
    let free = ret_lists(st, body, cf);
    let base: Side = (Vec::new(), c.head().to_vec());
    match fixed {
        None => found.products(free, base),
        Some((p, prem)) => {
            for (k, &l) in body.iter().enumerate() {
                if l == p {
                    let mut lists = free.clone();
                    lists[k] = vec![prem.clone()];
                    found.products(lists, base.clone());
                }
            }
        }
    }
}

/// The premises of M-return's resolution on each literal of `body`: the context's processed
/// clauses other than markers where it is maximal, selected ones included. With `cf`,
/// M-return-cf ([`MREPAIR_CF`]), only those whose body has no ground literal
/// ([`ground_body`]).
fn ret_lists(st: &State, body: &[Lit], cf: Option<Cf>) -> Vec<Vec<Side>> {
    ret_lists_with(st, body, cf, &mut |_| true).expect("a read that never stops")
}

/// [`ret_lists`], which tells `read` of each premise it reads, with the length of its
/// body when M-return-cf tests it, and is `None` once read answers false.
fn ret_lists_with(
    st: &State,
    body: &[Lit],
    cf: Option<Cf>,
    read: &mut dyn FnMut(usize) -> bool,
) -> Option<Vec<Vec<Side>>> {
    let mut lists = Vec::with_capacity(body.len());
    for &l in body {
        let mut v = Vec::new();
        if let Some(d) = dl(st) {
            for &i in d.store.by_xl.get(&l).into_iter().flatten() {
                if !read(1) {
                    return None;
                }
                // partners_of with selected premises
                let s = &d.store.clauses[i as usize];
                if s.dead {
                    continue;
                }
                let p = side(&s.c, l);
                if p.1.is_empty() && p.0 == [l] {
                    continue;
                }
                if let Some(cf) = cf {
                    if !read(p.0.len()) {
                        return None;
                    }
                    if ground_body(cf, &p.0) {
                        continue;
                    }
                }
                v.push(p);
            }
        }
        lists.push(v);
    }
    Some(lists)
}

/// What M-return-cf's test of a premise's body reads: the index and the run's interned
/// literals.
type Cf<'a> = (&'a Index, &'a super::dl_clause::Lits);

/// M-return-cf's test, `Some` when the run's repair has [`MREPAIR_CF`].
fn cf_of<'a, S: Sink>(ix: &'a Index, out: &'a S) -> Option<Cf<'a>> {
    (out.dl().mrepair & MREPAIR_CF != 0).then(|| (ix, &out.dl().lits))
}

/// Whether a premise's body holds a ground literal, for M-return-cf: an interned literal
/// whose terms are constants or function terms of constants, so that it mentions neither x
/// nor y.
fn ground_body((ix, lits): Cf, body: &[Lit]) -> bool {
    let inv = inv_of(ix, dix(ix));
    body.iter()
        .any(|&l| l.kind() == XL && super::dl_rules::no_xy(lits.view(l, inv)))
}

/// Row 1231's diagnostic, which `SPARKLES_ROOTED_MARKER_DIAG=1` turns on: after a rooted
/// run saturates, the run reports on stderr how often a context that is registered with the
/// root for a constant of a conditional ground trigger lies outside the successors of the
/// trigger's holder, so that the trigger's marker never reaches it.
pub(super) fn marker_diag() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("SPARKLES_ROOTED_MARKER_DIAG").is_some_and(|v| v == "1" || v == "2")
    }) || TEST_DIAG.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether row 1231's diagnostic also prints each pair, which `SPARKLES_ROOTED_MARKER_DIAG=2`
/// turns on.
fn marker_detail() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_MARKER_DIAG").is_some_and(|v| v == "2"))
}

/// The tier of [`marker_trips`] at which row 1231's decline withdraws a rooted run's answer,
/// or `None` for no decline. A finished rooted run with a pair at that tier makes no claim:
/// the classifier starts again without the rooted rules and reports
/// `Stats::dl_rooted_declined`. `on` is [`Limits::rooted_decline`](super::Limits), which
/// gives tier 4, and `SPARKLES_ROOTED_MARKER_DECLINE` overrides it for measurements, with
/// `off` for no decline or a tier.
pub(super) fn marker_decline(on: bool) -> Option<usize> {
    static ENV: std::sync::OnceLock<Option<Option<usize>>> = std::sync::OnceLock::new();
    let env = *ENV.get_or_init(|| {
        let v = std::env::var("SPARKLES_ROOTED_MARKER_DECLINE").ok()?;
        Some(if v == "off" {
            None
        } else {
            v.parse().ok().filter(|&t| t < MARKER_TIERS)
        })
    });
    env.unwrap_or(if on { Some(MARKER_DECLINE_TIER) } else { None })
}

/// The tier of [`marker_trips`] at which the decline is on by default.
pub(super) const MARKER_DECLINE_TIER: usize = 4;

/// The tiers of [`marker_trips`].
pub(super) const MARKER_TIERS: usize = 5;

/// What one context of a rooted run holds for [`marker_trips`].
pub(super) struct MarkerCtx {
    pub(super) me: Ex,
    /// the context's successors
    pub(super) succ: Vec<Ex>,
    /// the context's maximal ground triggers that a processed clause other than a marker has
    pub(super) held: Vec<Lit>,
    /// in the root, the contexts registered for each constant
    pub(super) edges: Vec<(u32, Vec<Ex>)>,
    /// in the root, the heads of its live facts with one literal
    pub(super) units: Vec<Lit>,
}

/// What [`marker_deficits`] reads of one context, which [`marker_def`] copies only with the
/// diagnostic, beside the context's [`MarkerCtx`], so that a copy without it is no larger.
#[derive(Default)]
pub(super) struct MarkerDef {
    /// whether the context is unsatisfiable, which discharges what it is owed
    bottom: bool,
    /// in another context, the ground literals whose markers it holds or has a live clause
    /// that subsumes, in order
    marks: Vec<Lit>,
    /// in another context, the live root clauses with ground bodies that it holds every body
    /// literal of and lacks a product of, and those whose products are too many to check
    owed: usize,
    unchecked: usize,
}

/// The factor by which a vector's capacity or a hash table's buckets can pass the
/// entries they hold: a vector doubles, and a table keeps 7 of 8 buckets at most and
/// rounds them up to a power of two.
const SLACK: usize = 3;

/// A bound on the bytes of the maps, sets and buffers that [`marker_trips`] builds over
/// the copies `cs` with a `stop`, which the run admits before the search: the index by
/// context, the predecessors with their lists, the root's reach, its successors and the
/// singleton contexts, the root's units and registrations, one holder's successors and
/// predecessors at a time, and the copies of successor lists that the searches take.
pub(super) fn marker_search_bytes(cs: &[MarkerCtx]) -> usize {
    use std::mem::size_of;
    let n = cs.len();
    let succ: usize = cs.iter().map(|c| c.succ.len()).sum();
    let root: usize = cs.iter().map(|c| c.units.len() + c.edges.len()).sum();
    // a hash entry of a context, of a context and its index, of a context and its list
    let set = size_of::<Ex>() + 1;
    let map = size_of::<(Ex, usize)>() + 1;
    let list = size_of::<(Ex, Vec<Ex>)>() + 1;
    let tables = n * (map + list + 4 * set) + root * (size_of::<(u32, &[Ex])>() + 1);
    // the predecessor lists and the copies of successor lists taken while searching
    let buffers = succ * size_of::<Ex>() * 2;
    SLACK * (tables + buffers)
}

/// What [`marker_trips`] needs of the context `me` with the state `st`.
/// `admit` takes the bytes the copy will hold, and with a negative count gives back
/// those of its scratch sets, before anything is allocated. A refusal ends the copy with
/// `Err`, and the run stops as past its memory budget.
pub(super) fn marker_ctx(
    ix: &Index,
    me: Ex,
    st: &State,
    lits: &super::dl_clause::Lits,
    admit: &mut dyn FnMut(isize) -> bool,
) -> Result<Option<MarkerCtx>, ()> {
    let Some(x) = dl(st) else { return Ok(None) };
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let is_root = dx.root == Some(me);
    // bounds on what the copy holds and on its two scratch sets of ground literals, with
    // the slack of a vector's or a table's capacity
    let ground = x.root.as_deref().map_or(0, |r| r.ground.len());
    let scratch = (SLACK * ground * 2 * (std::mem::size_of::<Lit>() + 1)) as isize;
    let mut kept = ground * std::mem::size_of::<Lit>()
        + (x.edges.len() + x.root.as_deref().map_or(0, |r| r.by_succ.len()) + st.subs.len())
            * std::mem::size_of::<Ex>();
    if is_root {
        kept += x.store.clauses.len() * std::mem::size_of::<Lit>();
        if let Some(r) = x.root.as_deref() {
            kept += r.edges.len() * std::mem::size_of::<(u32, Vec<Ex>)>()
                + r.edges.values().map(|v| v.len()).sum::<usize>() * std::mem::size_of::<Ex>();
        }
    }
    let kept = SLACK * kept;
    if !admit(kept as isize + scratch) {
        return Err(());
    }
    let mut held = Vec::new();
    if let Some(r) = x.root.as_deref()
        && !r.ground.is_empty()
    {
        // every maximal literal of a processed clause other than a marker, dead or not,
        // since a clause can have several and Stored::max holds the first
        let ground: FxHashSet<Lit> = r.ground.iter().copied().collect();
        let mut own: FxHashSet<Lit> = FxHashSet::default();
        for i in 0..x.store.clauses.len() as u32 {
            if is_marker(&x.store.clauses[i as usize].c) {
                continue;
            }
            own.extend(x.store.maxes(i).filter(|m| ground.contains(m)));
        }
        for &l in &r.ground {
            if own.contains(&l) && ground_trigger(lits.view(l, inv)) {
                held.push(l);
            }
        }
    }
    let mut edges = Vec::new();
    let mut units = Vec::new();
    if is_root {
        if let Some(r) = x.root.as_deref() {
            edges = r.edges.iter().map(|(&u, v)| (u, v.clone())).collect();
        }
        for s in &x.store.clauses {
            if !s.dead && s.c.body().is_empty() && s.c.head().len() == 1 {
                units.push(s.c.head()[0]);
            }
        }
    }
    admit(-scratch);
    Ok(Some(MarkerCtx {
        me,
        succ: successors(ix, st),
        held,
        edges,
        units,
    }))
}

/// Whether `e` is the root of the rooted run.
pub(super) fn is_root(ix: &Index, e: Ex) -> bool {
    dix(ix).root == Some(e)
}

/// The root's live clauses whose bodies are ground triggers, which [`marker_def`] reads, for
/// the diagnostic.
pub(super) fn marker_root_gbody(
    ix: &Index,
    me: Ex,
    st: &State,
    lits: &super::dl_clause::Lits,
    poll: &mut dyn FnMut(usize) -> bool,
) -> Option<Vec<Clause>> {
    let inv = inv_of(ix, dix(ix));
    let Some(x) = dl(st).filter(|_| is_root(ix, me)) else {
        return Some(Vec::new());
    };
    let mut rd = Reads::new(poll);
    let mut rg = Vec::new();
    for s in &x.store.clauses {
        if !rd.read(1 + s.c.body().len()) {
            return None;
        }
        if !s.dead
            && !s.c.body().is_empty()
            && s.c
                .body()
                .iter()
                .all(|&l| l.kind() == XL && ground_trigger(lits.view(l, inv)))
        {
            rg.push(s.c.clone());
        }
    }
    rd.end().then_some(rg)
}

/// The choices of one product of [`marker_def`] past which a pair counts as unchecked.
const DEF_CHOICES: usize = 1 << 16;

/// The deficits' copy of the context `m` with the state `st`, for the diagnostic and outside
/// the run's memory budget, as the diagnostic's sets are. `rg` holds the root's live clauses
/// with ground bodies ([`marker_root_gbody`]). A clause is owed to the context when the
/// context holds every body literal, is satisfiable, and lacks one of the products that
/// M-return derives from it, each choice of a live premise with each body literal maximal
/// merged with the clause's head: the product is neither a tautology, after the sorting and
/// the tests that `derive` applies first in a rooted run (owl:Thing and owl:Nothing over
/// constants, a head literal in the body, an equality beside its inequality), nor subsumed
/// by a live clause of the context. derive's other simplifications are not replayed, so a
/// product they drop can count. With `cf`, the run's M-return-cf ([`MREPAIR_CF`]), the
/// products take only the premises that M-return-cf takes, so the deficit checks the
/// delivery of the rule the run applies, not what M-return-cf leaves out. A product that
/// derive promoted to the root as a fact ([`super::dl_rules::DlSt`]'s promoted heads) is
/// held. The copy charges `poll` with its reads and choices, after every [`DIAG_POLL`] of
/// them, and is `Err` once poll answers false, so that the run's deadline and
/// cancellation bound it.
pub(super) fn marker_def(
    ix: &Index,
    m: &MarkerCtx,
    st: &State,
    rg: &[Clause],
    lits: &super::dl_clause::Lits,
    cf: bool,
    poll: &mut dyn FnMut(usize) -> bool,
) -> Result<Option<MarkerDef>, ()> {
    let Some(x) = dl(st) else { return Ok(None) };
    let is_root = is_root(ix, m.me);
    let mut rd = Reads::new(poll);
    let mut marks = Vec::new();
    for &l in x.root.as_deref().into_iter().flat_map(|r| &r.gmarks) {
        if !rd.read(1) {
            return Err(());
        }
        marks.push(l);
    }
    for s in &x.store.clauses {
        if !rd.read(1) {
            return Err(());
        }
        if s.dead {
            continue;
        }
        let (b, h) = (s.c.body(), s.c.head());
        let one: Vec<Lit> = b.iter().chain(h).copied().collect();
        if one.len() <= 2
            && b.len() <= 1
            && h.len() <= 1
            && one.windows(2).all(|w| w[0] == w[1])
            && let Some(&l) = one.first()
            && l.kind() == XL
        {
            marks.push(l);
        }
    }
    marks.sort_unstable();
    marks.dedup();
    let (mut owed, mut unchecked) = (0, 0);
    if !is_root && !st.bottom && !rg.is_empty() {
        let mut held: FxHashSet<Lit> = FxHashSet::default();
        for &l in &m.held {
            if !rd.read(1) {
                return Err(());
            }
            held.insert(l);
        }
        for c in rg {
            if !rd.read(1 + c.body().len()) {
                return Err(());
            }
            if !c.body().iter().all(|l| held.contains(l)) {
                continue;
            }
            // the premises, each candidate read
            let lists = ret_lists_with(st, c.body(), cf.then_some((ix, lits)), &mut |n| rd.read(n))
                .ok_or(())?;
            let n = lists
                .iter()
                .try_fold(1usize, |n, l| n.checked_mul(l.len()))
                .unwrap_or(usize::MAX);
            if n > DEF_CHOICES {
                unchecked += 1;
                continue;
            }
            if n > 0 && !products_held(ix, st, &lists, c.head(), lits, &mut rd).ok_or(())? {
                owed += 1;
            }
        }
    }
    if !rd.end() {
        return Err(());
    }
    Ok(Some(MarkerDef {
        bottom: st.bottom,
        marks,
        owed,
        unchecked,
    }))
}

/// Whether [`marker_def`] takes M-return-cf's premises for the repair's mode `mode`: only
/// where M-return ran with [`MREPAIR_CF`], since without [`MREPAIR_ON`] no rule restricted
/// them.
pub(super) fn marker_def_cf(mode: u8) -> bool {
    mode & (MREPAIR_ON | MREPAIR_CF) == MREPAIR_ON | MREPAIR_CF
}

/// The reads or choices of the diagnostic between two of its polls.
const DIAG_POLL: usize = 4096;

/// The diagnostic's count of what it reads: it charges `poll` a conclusion for each
/// [`WORK_READS`] reads, after every [`DIAG_POLL`] of them, and stops once poll answers
/// false. The redundancy test of each product in [`products_held`] scans the store's
/// candidates outside this count, and [`marker_trips`] counts nothing (roadmap items 1260
/// and 1280).
struct Reads<'a> {
    n: usize,
    poll: &'a mut dyn FnMut(usize) -> bool,
}

impl<'a> Reads<'a> {
    fn new(poll: &'a mut dyn FnMut(usize) -> bool) -> Self {
        Reads { n: 0, poll }
    }

    /// Counts `k` reads, false when a check of the bounds stopped the diagnostic.
    fn read(&mut self, k: usize) -> bool {
        let before = self.n / DIAG_POLL;
        self.n += k;
        let polls = self.n / DIAG_POLL - before;
        polls == 0 || (self.poll)(polls * (DIAG_POLL / WORK_READS))
    }

    /// Charges `w` conclusions and checks the bounds at once.
    fn check(&mut self, w: usize) -> bool {
        (self.poll)(w)
    }

    /// Charges the reads since the last check and checks the bounds, false when the check
    /// stopped the diagnostic.
    fn end(self) -> bool {
        (self.poll)(self.n % DIAG_POLL / WORK_READS)
    }
}

/// Whether the context holds every product of `lists` with the head `head`: each is a
/// tautology or a live clause of the context subsumes it.
fn products_held(
    ix: &Index,
    st: &State,
    lists: &[Vec<Side>],
    head: &[Lit],
    lits: &super::dl_clause::Lits,
    rd: &mut Reads,
) -> Option<bool> {
    use super::dl_clause::X;
    let Some(x) = dl(st) else { return Some(false) };
    let inv = inv_of(ix, dix(ix));
    let is =
        |l: Lit, a: Ex| l.kind() == XL && matches!(lits.view(l, inv), XLit::Atom(b, _) if b == a);
    let mut at = vec![0usize; lists.len()];
    loop {
        // a choice counts as a conclusion
        if !rd.read(WORK_READS) {
            return None;
        }
        let mut body: Vec<Lit> = Vec::new();
        let mut hd: Vec<Lit> = head.to_vec();
        for (l, &i) in lists.iter().zip(&at) {
            body.extend_from_slice(&l[i].0);
            hd.extend_from_slice(&l[i].1);
        }
        body.retain(|l| !(l.kind() == X && (l.atom() == TOP || st.subs.contains(&l.atom()))));
        body.retain(|&l| !is(l, TOP));
        hd.retain(|&l| !is(l, BOTTOM));
        body.sort_unstable();
        body.dedup();
        hd.sort_unstable();
        hd.dedup();
        let taut = lits.complementary(&hd, inv)
            || hd.iter().any(|&l| {
                (l.kind() == X && (l.atom() == TOP || st.subs.contains(&l.atom())))
                    || is(l, TOP)
                    || body.binary_search(&l).is_ok()
            });
        // a fact that derive sent the root ([`super::dl_rules::derive`]) is held
        let promoted = body.is_empty() && x.promoted.contains(hd.as_slice());
        if !taut && !promoted {
            let c = Clause::new(body, hd);
            if !x.store.subsumed(&c, c.sig()) {
                return Some(false);
            }
        }
        let mut k = lists.len();
        loop {
            if k == 0 {
                return Some(true);
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

/// Counts, for each tier, the conditional ground triggers, their constants and the pairs
/// of a trigger's holder and a context registered with the root for one of the trigger's
/// constants, that the holder's markers do not reach. A trigger is conditional when it is
/// no fact of the root. The tiers leave out more contexts in turn: tier 0 leaves out only
/// the holder and its successors, tier 1 also the holder's predecessors, tier 2 also the
/// constant's singleton context, tier 3 also the contexts that the root does not reach,
/// and tier 4 also the root's own successors. With `stop`, and without the pairs on
/// stderr, the count ends at the first pair of that tier, which is all a decline needs.
/// The answer also counts the pair set P_R of the repair (PLAN v3.1, D3), the pairs that
/// tier 0 keeps whose context the root reaches, as triples of holder, trigger and context
/// and as sends, the distinct pairs of context and trigger. Those two counts are complete
/// only without `stop`, and the run builds the sets of distinct triggers, constants and
/// sends only then, for the diagnostic, outside the run's memory budget.
pub(super) fn marker_trips(
    ix: &Index,
    cs: &[MarkerCtx],
    lits: &super::dl_clause::Lits,
    stop: Option<usize>,
) -> ([[usize; 3]; MARKER_TIERS], [usize; 2]) {
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let mut out = [[0usize; 3]; MARKER_TIERS];
    let mut pr = [0usize; 2];
    let mut sends: FxHashSet<(Ex, Lit)> = FxHashSet::default();
    let Some(root) = dx.root else {
        return (out, pr);
    };
    let by: FxHashMap<Ex, usize> = cs.iter().enumerate().map(|(i, c)| (c.me, i)).collect();
    let Some(&ri) = by.get(&root) else {
        return (out, pr);
    };
    let detail = marker_detail();
    let stop = stop.filter(|_| !detail);
    let units: FxHashSet<Lit> = cs[ri].units.iter().copied().collect();
    let edges: FxHashMap<u32, &[Ex]> = cs[ri].edges.iter().map(|(u, v)| (*u, &v[..])).collect();
    let mut preds: FxHashMap<Ex, Vec<Ex>> = FxHashMap::default();
    for c in cs {
        for &w in &c.succ {
            preds.entry(w).or_default().push(c.me);
        }
    }
    let reach = |from: Ex, next: &dyn Fn(Ex) -> Vec<Ex>| {
        let mut seen: FxHashSet<Ex> = FxHashSet::default();
        let mut todo = vec![from];
        seen.insert(from);
        while let Some(e) = todo.pop() {
            for w in next(e) {
                if seen.insert(w) {
                    todo.push(w);
                }
            }
        }
        seen
    };
    let succ_of = |e: Ex| by.get(&e).map_or(Vec::new(), |&i| cs[i].succ.clone());
    let pred_of = |e: Ex| preds.get(&e).cloned().unwrap_or_default();
    let from_root = reach(root, &succ_of);
    let root_succ: FxHashSet<Ex> = cs[ri].succ.iter().copied().collect();
    let singleton: FxHashMap<u32, Ex> = dx.consts.iter().copied().collect();
    let mut trig: [FxHashSet<Lit>; MARKER_TIERS] = Default::default();
    let mut kons: [FxHashSet<u32>; MARKER_TIERS] = Default::default();
    for c in cs {
        if c.me == root || c.held.is_empty() {
            continue;
        }
        // the holder's successors and predecessors, found only when a pair needs them
        let down: std::cell::OnceCell<FxHashSet<Ex>> = std::cell::OnceCell::new();
        let up: std::cell::OnceCell<FxHashSet<Ex>> = std::cell::OnceCell::new();
        for &l in &c.held {
            if units.contains(&l) {
                continue;
            }
            let mut us = Vec::new();
            consts(lits.view(l, inv), &mut us);
            us.sort_unstable();
            us.dedup();
            let mut regs: Vec<(u32, Ex)> = Vec::new();
            for &u in &us {
                for &d in edges.get(&u).copied().unwrap_or(&[]) {
                    regs.push((u, d));
                }
            }
            regs.sort_unstable_by_key(|&(_, d)| d);
            regs.dedup_by_key(|p| p.1);
            for (_, d) in regs {
                if d == root {
                    continue;
                }
                // the tiers that need no search first, so that a decline skips the
                // searches of the pairs it cannot use
                let single = !us.iter().any(|v| {
                    singleton.get(v) == Some(&d) || super::dl_rules::singleton_of(ix, d) == Some(*v)
                });
                let reached = from_root.contains(&d);
                let direct = !root_succ.contains(&d);
                if let Some(t) = stop
                    && ![single, reached, direct][..t.saturating_sub(1).min(3)]
                        .iter()
                        .all(|&k| k)
                {
                    continue;
                }
                if down.get_or_init(|| reach(c.me, &succ_of)).contains(&d) {
                    continue;
                }
                if reached && stop.is_none() {
                    pr[0] += 1;
                    sends.insert((d, l));
                }
                let keep = [
                    true,
                    !up.get_or_init(|| reach(c.me, &pred_of)).contains(&d),
                    single,
                    reached,
                    direct,
                ];
                if detail {
                    let t = keep.iter().take_while(|&&k| k).count();
                    eprintln!(
                        "MARKER-PAIR tier<={} holder={} lit={} ctx={d}",
                        t.saturating_sub(1),
                        c.me,
                        super::dl_rules::tr(ix, lits, &[l])
                    );
                }
                let mut tier = 0;
                while tier < MARKER_TIERS && keep[..=tier].iter().all(|&k| k) {
                    out[tier][2] += 1;
                    // the sets of the diagnostic, which a decline does not build
                    if stop.is_none() {
                        trig[tier].insert(l);
                        kons[tier].extend(us.iter().copied());
                    }
                    tier += 1;
                }
                if stop.is_some_and(|t| out[t][2] > 0) {
                    pr[1] = sends.len();
                    return (out, pr);
                }
            }
        }
    }
    for t in 0..MARKER_TIERS {
        out[t][0] = trig[t].len();
        out[t][1] = kons[t].len();
    }
    pr[1] = sends.len();
    (out, pr)
}

/// The two deficits of the marker repair (PLAN v3.1, D4), found at quiescence from the
/// contexts' clauses and not from the sends: the triples of P_R, holder, trigger and context,
/// whose context holds neither the trigger's marker nor a clause that subsumes it, and the
/// pairs of a live root clause whose body is ground triggers and a context that holds every
/// body literal and lacks one of the clause's M-return products ([`marker_def`]), and the
/// pairs whose products are too many to check. `defs` holds marker_def's copy of each
/// context of `cs`, in its order. The count charges `poll` with the contexts, successors,
/// literals, registrations and destinations it reads, checks it for each context and
/// after every [`DIAG_POLL`] reads, and is `None` once poll answers false.
pub(super) fn marker_deficits(
    ix: &Index,
    cs: &[MarkerCtx],
    defs: &[Option<MarkerDef>],
    lits: &super::dl_clause::Lits,
    poll: &mut dyn FnMut(usize) -> bool,
) -> Option<[usize; 3]> {
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    let mut out = [0usize; 3];
    let Some(root) = dx.root else {
        return Some(out);
    };
    let mut rd = Reads::new(poll);
    let mut by: FxHashMap<Ex, usize> = FxHashMap::default();
    for (i, c) in cs.iter().enumerate() {
        if !rd.read(1) {
            return None;
        }
        by.insert(c.me, i);
    }
    let Some(&ri) = by.get(&root) else {
        return rd.end().then_some(out);
    };
    let mut units: FxHashSet<Lit> = FxHashSet::default();
    for &l in &cs[ri].units {
        if !rd.read(1) {
            return None;
        }
        units.insert(l);
    }
    let mut edges: FxHashMap<u32, &[Ex]> = FxHashMap::default();
    for (u, v) in &cs[ri].edges {
        if !rd.read(1) {
            return None;
        }
        edges.insert(*u, &v[..]);
    }
    // the contexts that `from` reaches, each successor read a read
    let reach = |from: Ex, rd: &mut Reads| {
        let mut seen: FxHashSet<Ex> = FxHashSet::default();
        let mut todo = vec![from];
        seen.insert(from);
        while let Some(e) = todo.pop() {
            let succ = by.get(&e).map_or(&[][..], |&i| &cs[i].succ[..]);
            if !rd.read(1) {
                return None;
            }
            for &w in succ {
                if !rd.read(1) {
                    return None;
                }
                if seen.insert(w) {
                    todo.push(w);
                }
            }
        }
        Some(seen)
    };
    let from_root = reach(root, &mut rd)?;
    for c in cs {
        if !rd.check(1) {
            return None;
        }
        if c.me == root || c.held.is_empty() {
            continue;
        }
        let down = reach(c.me, &mut rd)?;
        let mut us = Vec::new();
        let mut to: Vec<Ex> = Vec::new();
        for &l in &c.held {
            if !rd.read(1) {
                return None;
            }
            if units.contains(&l) {
                continue;
            }
            us.clear();
            consts(lits.view(l, inv), &mut us);
            to.clear();
            for &d in us
                .iter()
                .flat_map(|u| edges.get(u).copied().unwrap_or(&[]).iter())
            {
                if !rd.read(1) {
                    return None;
                }
                if d != root && from_root.contains(&d) && !down.contains(&d) {
                    to.push(d);
                }
            }
            to.sort_unstable();
            to.dedup();
            for &d in &to {
                if !rd.read(1) {
                    return None;
                }
                let Some(&i) = by.get(&d) else { continue };
                let Some(dd) = defs.get(i).and_then(Option::as_ref) else {
                    continue;
                };
                if !dd.bottom && dd.marks.binary_search(&l).is_err() {
                    out[0] += 1;
                }
            }
        }
    }
    for d in defs.iter().flatten() {
        if !rd.read(1) {
            return None;
        }
        out[1] += d.owed;
        out[2] += d.unchecked;
    }
    rd.end().then_some(out)
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

    /// A sink for M-return's late replay: its answer to every check of the run's bounds, the
    /// checks it was asked, and the bytes it holds reserved and their most.
    struct Bounded<'a> {
        run: &'a DlRun,
        go: bool,
        polls: std::cell::Cell<usize>,
        held: usize,
        peak: usize,
    }

    impl Sink for Bounded<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn go_on(&self) -> bool {
            self.polls.set(self.polls.get() + 1);
            self.go
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            self.polls.set(self.polls.get() + 1);
            self.held += bytes;
            self.peak = self.peak.max(self.held);
            self.go
        }
        fn release_data(&mut self, bytes: usize) {
            self.held -= bytes;
        }
    }

    /// What a late replay did, [`late_replay`]'s answer.
    #[derive(Debug)]
    struct Late {
        /// the checks of the bounds before the caller drained the rule's other conclusions
        polls: usize,
        /// whether the store was interrupted then
        interrupted: bool,
        /// the conclusions derived in all
        work: usize,
        /// the most bytes reserved at once, and the bytes still reserved at the end
        peak: usize,
        held: usize,
    }

    /// A context other than the root holds `rets` returned clauses A(u) ∧ M(u) → H_n(u) and
    /// `partners` processed premises B_j(x) → M(u), with ground-free bodies so that
    /// M-return-cf keeps them, and takes the late premise K(x) → A(u) with the repair in
    /// `mode`, under a sink whose checks of the bounds all answer `go`.
    fn late_replay(mode: u8, rets: usize, partners: usize, go: bool) -> Late {
        let mut sig = Sig::new();
        let [a, m, k] = ["A", "M", "K"].map(|n| sig.class(n));
        let u = sig.ind("u");
        let bs: Vec<Ex> = (0..partners).map(|j| sig.class(&format!("B{j}"))).collect();
        let hs: Vec<Ex> = (0..rets).map(|n| sig.class(&format!("H{n}"))).collect();
        let (ix, _) = probe_ix(sig);
        let mut run = DlRun::new(false, Default::default());
        run.mrepair = mode;
        let inv = inv_of(&ix, dix(&ix));
        let lit = |a: Ex| match run.lits.lit(XLit::Atom(a, c(u, &ix)), inv) {
            Made::Lit(l) => l,
            _ => panic!("a literal"),
        };
        let (la, lm) = (lit(a), lit(m));
        let ids: Vec<u32> = hs
            .iter()
            .map(|&h| run.table.push(Clause::new(vec![la, lm], vec![lit(h)])))
            .collect();
        let mut st = State {
            init: true,
            ..State::default()
        };
        for &b in &bs {
            ds(&mut st)
                .store
                .add(Clause::new(vec![Lit::x(b)], vec![lm]));
        }
        let order = Rooted {
            ix: &ix,
            lits: &run.lits,
            inv: &inv,
            ground_total: true,
        };
        let mut max = Vec::new();
        while ds(&mut st)
            .store
            .next(&ix, Some(&order), &mut max)
            .is_some()
        {}
        let mut out = Bounded {
            run: &run,
            go,
            polls: std::cell::Cell::new(0),
            held: 0,
            peak: 0,
        };
        let r = rst(&mut st, &out);
        mrep_new(r, &out);
        r.mw().mheld.insert(la);
        r.mw().rets.insert(la, ids);
        let work = run.work();
        let premise = Clause::new(vec![Lit::x(k)], vec![la]);
        let prem = side(&premise, la);
        let mut found = Found::default();
        on_max(
            &ix, k, &mut st, &premise, la, &prem, &mut found, &mut out, true,
        );
        let (polls, interrupted) = (out.polls.get(), ds(&mut st).store.interrupted);
        drain(&ix, &mut st, found, &mut out);
        Late {
            polls,
            interrupted,
            work: run.work() - work,
            peak: out.peak,
            held: out.held,
        }
    }

    #[test]
    fn m_return_s_late_replay_is_bounded_and_complete() {
        // the replay of many saved returns checks the run's bounds and admits what it
        // prepares batch by batch, so a refusal stops it before the returns are all
        // expanded, whatever their number, and with every check passed it derives each
        // return's resolvent with each partner, a last part batch included
        for mode in [MREPAIR_ON, MREPAIR_ON | MREPAIR_CF] {
            let (rets, partners) = (4 * M_POLL + 5, 3);
            let all = late_replay(mode, rets, partners, true);
            assert_eq!(all.work, rets * partners, "mode {mode}: {all:?}");
            assert!(
                !all.interrupted && all.peak > 0 && all.held == 0,
                "mode {mode}: {all:?}"
            );
            let more = late_replay(mode, 4 * rets, partners, true);
            assert_eq!(more.work, 4 * rets * partners, "mode {mode}: {more:?}");
            assert_eq!(more.peak, all.peak, "mode {mode}: one batch at a time");
            for rets in [rets, 4 * rets] {
                let cut = late_replay(mode, rets, partners, false);
                assert!(cut.polls >= 1 && cut.interrupted, "mode {mode}: {cut:?}");
                assert!(cut.work < M_POLL * partners, "mode {mode}: {cut:?}");
                assert!(
                    cut.peak <= all.peak && cut.held == 0,
                    "mode {mode}: {cut:?}"
                );
            }
        }
    }

    #[test]
    fn m_return_s_late_replay_closes_a_batch_on_its_bytes() {
        // with many partners a batch closes on RECORD_SLACK after a few returns, well before
        // M_POLL of them, so what the replay and its drain hold at once stays near the slack,
        // far below the returns' products together, whatever the number of returns
        for mode in [MREPAIR_ON, MREPAIR_ON | MREPAIR_CF] {
            let partners = 100;
            let one = late_replay(mode, 1, partners, true).peak;
            assert!(
                one > 0 && one < RECORD_SLACK / 4,
                "mode {mode}: one return {one}"
            );
            let rets = M_POLL - 1;
            let all = late_replay(mode, rets, partners, true);
            assert_eq!(all.work, rets * partners, "mode {mode}: {all:?}");
            assert!(
                all.peak <= 2 * RECORD_SLACK,
                "mode {mode}: {all:?}, one {one}"
            );
            assert!(2 * all.peak < rets * one, "mode {mode}: {all:?}, one {one}");
            let more = late_replay(mode, 4 * rets, partners, true);
            assert_eq!(more.work, 4 * rets * partners, "mode {mode}: {more:?}");
            assert!(
                more.peak <= 2 * RECORD_SLACK,
                "mode {mode}: {more:?}, {all:?}"
            );
            let cut = late_replay(mode, 4 * rets, partners, false);
            assert!(
                cut.polls >= 1 && cut.interrupted && cut.held == 0,
                "mode {mode}: {cut:?}"
            );
            assert!(cut.peak <= 2 * RECORD_SLACK, "mode {mode}: {cut:?}");
        }
    }

    /// The repair's modes that the bounds tests run: M-return alone and with M-return-cf.
    const MODES: [u8; 2] = [MREPAIR_ON, MREPAIR_ON | MREPAIR_CF];

    /// A sink for the repair's bounds: it records what the rules send, counts the checks
    /// of the run's bounds, answers each with `refuse` unset or among the first `after`,
    /// and stays stopped after a refusal or once `stop` is set, as a run does. It tracks
    /// the bytes reserved.
    struct Stops<'a> {
        run: &'a DlRun,
        polls: std::cell::Cell<usize>,
        stop: std::cell::Cell<bool>,
        refuse: bool,
        after: usize,
        sent: Vec<(Ex, Concl)>,
        asked: usize,
        held: usize,
        peak: usize,
    }

    impl<'a> Stops<'a> {
        fn new(run: &'a DlRun) -> Self {
            Stops {
                run,
                polls: Default::default(),
                stop: Default::default(),
                refuse: false,
                after: 0,
                sent: Vec::new(),
                asked: 0,
                held: 0,
                peak: 0,
            }
        }
    }

    impl Sink for Stops<'_> {
        fn send(&mut self, e: Ex, c: Concl) {
            self.sent.push((e, c));
        }
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn go_on(&self) -> bool {
            self.polls.set(self.polls.get() + 1);
            if self.refuse && self.polls.get() > self.after {
                self.stop.set(true);
            }
            !self.stop.get()
        }
        fn stopped(&self) -> bool {
            self.stop.get()
        }
        fn reserve_data(&mut self, n: usize) -> bool {
            self.asked += n;
            self.held += n;
            self.peak = self.peak.max(self.held);
            self.go_on()
        }
        fn release_data(&mut self, n: usize) {
            self.held -= n;
        }
    }

    /// The literal `a(u)` of the run.
    fn glit(run: &DlRun, ix: &Index, a: Ex, u: Ex) -> Lit {
        match run.lits.lit(XLit::Atom(a, c(u, ix)), inv_of(ix, dix(ix))) {
            Made::Lit(l) => l,
            _ => panic!("a literal"),
        }
    }

    #[test]
    fn the_repair_s_handlers_do_nothing_once_the_run_stopped() {
        // a run that stopped takes no further notice, reach or return: the handlers neither
        // record, allocate, charge nor send, and M-send's replay is cut off at its entry
        for mode in MODES {
            let mut sig = Sig::new();
            let [a, b, k] = ["A", "B", "K"].map(|n| sig.class(n));
            let u = sig.ind("u");
            let (ix, root) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let (ga, gb) = (glit(&run, &ix, a, u), glit(&run, &ix, b, u));
            let RTerm::C(cu) = c(u, &ix) else { panic!() };
            let mut st = State::default();
            let mut out = Stops::new(&run);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            r.edges.insert(cu, vec![500]);
            r.mw().reach_set.insert(500);
            r.mw().holders.insert(ga, vec![501]);
            r.mw().hold_set.insert((501, ga));
            r.mw().held_at.insert(cu, vec![ga.xid()]);
            let cid = run.table.push(Clause::new(vec![ga], vec![gb]));
            out.stop.set(true);
            let (work, bytes) = (run.work(), run.bytes());
            assert!(!rmark(&ix, &mut st, k, gb.xid(), &mut out), "mode {mode}");
            assert!(!rreach(&ix, root, &mut st, 600, &mut out), "mode {mode}");
            m_offer(&ix, &mut st, 0, &Clause::new(vec![ga], vec![gb]), &mut out);
            m_registered(ds(&mut st), 500, &[cu], &mut out);
            let m = dl(&st).unwrap().root.as_deref().unwrap().mr();
            assert!(!m.hold_set.contains(&(k, gb)), "mode {mode}");
            assert!(
                m.gbody.is_empty() && !m.reach_set.contains(&600),
                "mode {mode}"
            );
            assert!(dl(&st).unwrap().store.interrupted, "mode {mode}");
            // a context other than the root that the stop finds without state keeps none
            let mut other = State::default();
            assert!(!rreach(&ix, k, &mut other, TOP, &mut out), "mode {mode}");
            assert!(!rret(&ix, &mut other, cid, &mut out), "mode {mode}");
            assert!(other.extra.is_none(), "mode {mode}");
            // M-send's notice and its late replay
            let mut other = State::default();
            ds(&mut other);
            let premise = Clause::new(vec![Lit::x(k)], vec![ga]);
            let prem = side(&premise, ga);
            assert!(
                !m_note(&ix, root, k, &mut other, ga, &prem, &mut out),
                "mode {mode}"
            );
            assert!(dl(&other).unwrap().root.is_none(), "mode {mode}");
            assert!(!m_poll(0, &mut other, &mut out), "mode {mode}");
            assert!(out.sent.is_empty(), "mode {mode}: {:?}", out.sent);
            assert_eq!((run.work(), run.bytes()), (work, bytes), "mode {mode}");
        }
    }

    /// A root whose repair state lists 256 constants, each with 256 ground literals that
    /// the context `k` alone holds, for [`m_registered`]'s scratch.
    fn many_held(mode: u8) -> (Index, DlRun, State, Ex, Vec<u32>) {
        let mut sig = Sig::new();
        let k = sig.class("K");
        let atoms: Vec<Ex> = (0..256).map(|i| sig.class(&format!("S{i}"))).collect();
        let inds: Vec<Ex> = (0..256).map(|i| sig.ind(&format!("v{i}"))).collect();
        let (ix, _) = probe_ix(sig);
        let mut run = DlRun::new(false, Default::default());
        run.mrepair = mode;
        let mut st = State::default();
        let mut us = Vec::new();
        {
            let out = Stops::new(&run);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            for &ind in &inds {
                let RTerm::C(cu) = c(ind, &ix) else { panic!() };
                us.push(cu);
                let mut held = Vec::new();
                for &a in &atoms {
                    let l = glit(&run, &ix, a, ind);
                    held.push(l.xid());
                    r.mw().holders.insert(l, vec![k]);
                    r.mw().hold_set.insert((k, l));
                }
                r.mw().held_at.insert(cu, held);
            }
        }
        (ix, run, st, k, us)
    }

    #[test]
    fn the_repair_s_scratch_is_admitted_before_it_is_built() {
        // the replay of a context's registrations admits the pairs it lists before it lists
        // them, so a refusal stops it before the scratch is built, and with every check
        // passed it charges and polls the scan of pairs it sends nothing for
        for mode in MODES {
            let (_ix, run, mut st, k, us) = many_held(mode);
            let mut out = Stops::new(&run);
            out.refuse = true;
            let ((), alloc) = super::super::numeric::tests::measure(|| {
                m_registered(ds(&mut st), k, &us, &mut out)
            });
            assert!(out.stopped() && out.asked >= 65_536 * 8, "mode {mode}");
            assert!(alloc.peak < 65_536, "mode {mode}: {alloc:?}");
            assert!(
                ds(&mut st).store.interrupted && out.held == 0,
                "mode {mode}"
            );
            let (_ix, run, mut st, k, us) = many_held(mode);
            let mut out = Stops::new(&run);
            let work = run.work();
            m_registered(ds(&mut st), k, &us, &mut out);
            assert!(out.sent.is_empty() && out.held == 0, "mode {mode}");
            assert!(out.asked >= 65_536 * 8, "mode {mode}");
            assert!(run.work() - work >= 65_536 / 16 - 1, "mode {mode}");
            assert!(out.polls.get() >= 65_536 / M_SCAN, "mode {mode}");
        }
    }

    #[test]
    fn the_repair_s_scans_without_output_are_charged_and_polled() {
        // a notice whose literal is in the bodies of 50,000 root clauses that the holder
        // holds no other literal of, and a root clause offered to the holders of its body,
        // which share none, each charge their reads and check the run's bounds
        for mode in MODES {
            let mut sig = Sig::new();
            let [a, b, k] = ["A", "B", "K"].map(|n| sig.class(n));
            let u = sig.ind("u");
            let heads: Vec<Ex> = (0..64).map(|i| sig.class(&format!("H{i}"))).collect();
            let (ix, _) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let (ga, gb) = (glit(&run, &ix, a, u), glit(&run, &ix, b, u));
            let mut st = State::default();
            let out = Stops::new(&run);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            ds(&mut st).store.add(Clause::new(
                vec![ga, gb],
                vec![glit(&run, &ix, heads[0], u)],
            ));
            let sample = ds(&mut st).store.clauses[0].clone();
            for _ in 1..50_000 {
                ds(&mut st).store.clauses.push(sample.clone());
            }
            let r = ds(&mut st).root.as_deref_mut().unwrap();
            r.mw().gbody.insert(ga, (0..50_000).collect());
            let mut out = Stops::new(&run);
            let work = run.work();
            assert!(rmark(&ix, &mut st, k, ga.xid(), &mut out), "mode {mode}");
            let notice = run.work() - work;
            assert!(notice >= 50_000 / 16 - 1, "mode {mode}: {notice}");
            assert!(out.polls.get() >= 50_000 / M_SCAN, "mode {mode}");
            assert_eq!(out.held, 0, "mode {mode}");
            assert!(
                out.asked >= 50_000 * 4,
                "mode {mode}: the returns' ids admitted"
            );
            // two body literals held by 50,000 contexts each, none by both
            let r = ds(&mut st).root.as_deref_mut().unwrap();
            r.mw().holders.insert(ga, (100..50_100).collect());
            r.mw().holders.insert(gb, (60_000..110_000).collect());
            let c = Clause::new(vec![ga, gb], vec![glit(&run, &ix, heads[1], u)]);
            let mut out = Stops::new(&run);
            let work = run.work();
            m_offer(&ix, &mut st, 1, &c, &mut out);
            assert!(run.work() - work >= 50_000 / 16 - 1, "mode {mode}");
            assert!(out.polls.get() >= 1 && out.sent.is_empty(), "mode {mode}");
            assert_eq!(out.held, 0, "mode {mode}");
            // with one of them held once, the offer reads the shorter list
            let r = ds(&mut st).root.as_deref_mut().unwrap();
            r.mw().holders.insert(gb, vec![60_000]);
            let mut out = Stops::new(&run);
            let work = run.work();
            m_offer(&ix, &mut st, 2, &c, &mut out);
            assert!(run.work() - work <= 1, "mode {mode}: the shorter list");
        }
    }

    #[test]
    fn a_root_unit_is_found_by_its_literal_while_it_lives() {
        // the root's units are indexed by literal as the root processes them, so the test
        // reads no list of the root's facts, and a unit that died is no unit
        for mode in MODES {
            let mut sig = Sig::new();
            let [a, b, cc] = ["A", "B", "C"].map(|n| sig.class(n));
            let u = sig.ind("u");
            let (ix, _) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let (ga, gb) = (glit(&run, &ix, a, u), glit(&run, &ix, b, u));
            let gc = glit(&run, &ix, cc, u);
            let RTerm::C(cu) = c(u, &ix) else { panic!() };
            let mut st = State::default();
            let mut out = Stops::new(&run);
            mrep_new(rst(&mut st, &out), &out);
            let unit = Clause::new(vec![], vec![ga]);
            let two = Clause::new(vec![], vec![gb, gc]);
            ds(&mut st).store.add(unit.clone());
            ds(&mut st).store.add(two.clone());
            m_offer(&ix, &mut st, 0, &unit, &mut out);
            m_offer(&ix, &mut st, 1, &two, &mut out);
            // facts the index does not list are not read
            ds(&mut st)
                .root
                .as_deref_mut()
                .unwrap()
                .ready
                .insert(cu, vec![0, 1]);
            assert!(root_unit(dl(&st).unwrap(), ga), "mode {mode}");
            assert!(!root_unit(dl(&st).unwrap(), gb), "mode {mode}");
            ds(&mut st).store.clauses[0].dead = true;
            assert!(!root_unit(dl(&st).unwrap(), ga), "mode {mode}");
            let mut fresh = State::default();
            mrep_new(rst(&mut fresh, &out), &out);
            ds(&mut fresh).store.add(unit.clone());
            ds(&mut fresh)
                .root
                .as_deref_mut()
                .unwrap()
                .ready
                .insert(cu, vec![0]);
            assert!(
                !root_unit(dl(&fresh).unwrap(), ga),
                "mode {mode}: not indexed"
            );
        }
    }

    #[test]
    fn a_reach_charges_the_state_it_makes() {
        // the first reach of a context that has no state for Tier D makes it, and the run's
        // memory takes all of it
        for mode in MODES {
            let mut sig = Sig::new();
            let k = sig.class("K");
            let (ix, _) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let mut out = Stops::new(&run);
            let mut st = State::default();
            let (before, held) = (run.bytes(), st.bytes());
            assert!(rreach(&ix, k, &mut st, TOP, &mut out), "mode {mode}");
            let (charged, made) = (run.bytes() - before, st.bytes() - held);
            assert!(
                charged >= made,
                "mode {mode}: charged {charged}, made {made}"
            );
        }
    }

    #[test]
    fn a_reach_replays_registrations_once_and_tells_a_successor_once() {
        // the registrations that wait for a context's reach go with it, and a reached
        // context tells each successor of its reach once, however often it meets it
        for mode in MODES {
            let mut sig = Sig::new();
            let k = sig.class("K");
            let (ix, root) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let mut out = Stops::new(&run);
            let mut st = State::default();
            register(&mut st, k, vec![0], &mut out);
            rreach(&ix, root, &mut st, k, &mut out);
            register(&mut st, k, vec![1], &mut out);
            let m = dl(&st).unwrap().root.as_deref().unwrap().mr();
            assert!(m.regs.get(&k).is_none_or(|v| v.is_empty()), "mode {mode}");
            let mut st = State::default();
            rreach(&ix, k, &mut st, TOP, &mut out);
            out.sent.clear();
            new_succ(&mut st, 77, &mut out);
            new_succ(&mut st, 77, &mut out);
            let n = out
                .sent
                .iter()
                .filter(|(_, c)| matches!(c, Concl::RReach(_)))
                .count();
            assert_eq!(n, 1, "mode {mode}");
        }
    }

    #[test]
    fn the_deficits_hold_promoted_facts_and_take_cf_only_with_the_repair() {
        // a product that derive sent the root as a fact is held, and the diagnostic reads
        // M-return-cf's premises only where M-return ran with them
        let mut sig = Sig::new();
        let [b, k] = ["B", "K"].map(|n| sig.class(n));
        let u = sig.ind("u");
        let (ix, _) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let gb = glit(&run, &ix, b, u);
        let mut st = State::default();
        st.subs.insert(k);
        ds(&mut st).empty_core = true;
        let mut out = Stops::new(&run);
        super::super::dl_rules::derive(&ix, &mut st, (vec![], vec![gb]), &mut out);
        assert!(dl(&st).unwrap().promoted.contains(&[gb][..]));
        let lists = [vec![(vec![Lit::x(k)], vec![])]];
        let held = products_held(
            &ix,
            &st,
            &lists,
            &[gb],
            &run.lits,
            &mut Reads::new(&mut |_| true),
        );
        assert_eq!(held, Some(true));
        assert!(!marker_def_cf(MREPAIR_CF) && !marker_def_cf(0));
        assert!(marker_def_cf(MREPAIR_ON | MREPAIR_CF) && !marker_def_cf(MREPAIR_ON));
    }

    #[test]
    fn the_deficits_charge_and_poll_their_products() {
        // 256 premises on each of two literals give 65,536 products, all tautologies, which
        // the copy charges and polls as it goes, and a refusal of a poll ends it
        let mut sig = Sig::new();
        let [a, b, k, z] = ["A", "B", "K", "Z"].map(|n| sig.class(n));
        let u = sig.ind("u");
        let hs: Vec<Ex> = (0..512).map(|i| sig.class(&format!("H{i}"))).collect();
        let (ix, _) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let (ga, gb) = (glit(&run, &ix, a, u), glit(&run, &ix, b, u));
        let mut st = State::default();
        for &h in &hs[..256] {
            ds(&mut st)
                .store
                .add(Clause::new(vec![Lit::x(k)], vec![ga, Lit::x(z), Lit::x(h)]));
        }
        for &h in &hs[256..] {
            ds(&mut st)
                .store
                .add(Clause::new(vec![Lit::x(z)], vec![gb, Lit::x(k), Lit::x(h)]));
        }
        let inv = inv_of(&ix, dix(&ix));
        let order = Rooted {
            ix: &ix,
            lits: &run.lits,
            inv: &inv,
            ground_total: true,
        };
        let mut max = Vec::new();
        while ds(&mut st)
            .store
            .next(&ix, Some(&order), &mut max)
            .is_some()
        {}
        let m = MarkerCtx {
            me: k,
            held: vec![ga, gb],
            succ: vec![],
            edges: vec![],
            units: vec![],
        };
        let rg = [Clause::new(vec![ga, gb], vec![])];
        let (mut polls, mut work) = (0, 0);
        let def = marker_def(&ix, &m, &st, &rg, &run.lits, false, &mut |n| {
            polls += 1;
            work += n;
            true
        });
        let def = def.unwrap().unwrap();
        assert_eq!((def.owed, def.unchecked), (0, 0));
        assert!(
            polls >= 65_536 / DIAG_POLL && work >= 65_536,
            "{polls} {work}"
        );
        let cut = marker_def(&ix, &m, &st, &rg, &run.lits, false, &mut |_| false);
        assert!(cut.is_err());
    }

    /// Reports the failures that a test over [`MODES`] collected, so that one run shows
    /// every mode's.
    fn report(fails: &[String]) {
        assert!(fails.is_empty(), "{}", fails.join("\n"));
    }

    #[test]
    fn the_repair_s_scratch_stays_within_its_admission() {
        // the destinations of a notice, the returns of a notice and the holders of an
        // offered clause, 40,000 of each and all passing, are built in the bytes admitted
        // for them: what the handler allocates at its most is the admission and the state
        // it charged
        const N: u32 = 40_000;
        let mut fails = Vec::new();
        for mode in MODES {
            let mut sig = Sig::new();
            let [a, b, k, hd] = ["A", "B", "K", "H"].map(|n| sig.class(n));
            let u = sig.ind("u");
            let (ix, _) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let (ga, gb) = (glit(&run, &ix, a, u), glit(&run, &ix, b, u));
            let gh = glit(&run, &ix, hd, u);
            let RTerm::C(cu) = c(u, &ix) else { panic!() };
            let many: Vec<Ex> = (100_000..100_000 + N).collect();
            let within = |fails: &mut Vec<String>, what: &str, peak, out: &Stops, grown| {
                if peak > out.peak + grown + 4096 {
                    fails.push(format!(
                        "mode {mode} {what}: allocated {peak}, admitted {} and charged {grown}",
                        out.peak
                    ));
                }
            };
            // the destinations of the notice of ga, each sent the marker already
            let mut st = State::default();
            let mut out = Stops::new(&run);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            r.edges.insert(cu, many.clone());
            for &d in &many {
                r.mw().reach_set.insert(d);
                r.gsent.insert((d, ga));
            }
            let before = run.bytes();
            let (_, alloc) = super::super::numeric::tests::measure(|| {
                rmark(&ix, &mut st, k, ga.xid(), &mut out)
            });
            within(
                &mut fails,
                "destinations",
                alloc.peak,
                &out,
                run.bytes() - before,
            );
            // the returns of the notice of ga: N root clauses ga → H whose ids it sent
            let mut st = State::default();
            let mut out = Stops::new(&run);
            out.sent.reserve(N as usize + 16);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            r.mw().gbody.insert(ga, (0..N).collect());
            let mut one = Clause::new(vec![ga], vec![gh]);
            ds(&mut st).store.add(one.clone());
            ds(&mut st).store.clauses[0].sent = Some(0);
            let sample = ds(&mut st).store.clauses[0].clone();
            for _ in 1..N {
                ds(&mut st).store.clauses.push(sample.clone());
            }
            let before = run.bytes();
            let (_, alloc) = super::super::numeric::tests::measure(|| {
                rmark(&ix, &mut st, k, ga.xid(), &mut out)
            });
            within(
                &mut fails,
                "returns",
                alloc.peak,
                &out,
                run.bytes() - before,
            );
            if out.sent.len() != N as usize {
                fails.push(format!("mode {mode} returns: {} sent", out.sent.len()));
            }
            // the holders of ga and gb, N contexts that hold both, for the offer of ga ∧ gb
            let mut st = State::default();
            let mut out = Stops::new(&run);
            out.sent.reserve(N as usize + 16);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            r.mw().holders.insert(ga, many.clone());
            r.mw().holders.insert(gb, many.clone());
            for &h in &many {
                r.mw().hold_set.insert((h, ga));
                r.mw().hold_set.insert((h, gb));
            }
            one = Clause::new(vec![ga, gb], vec![gh]);
            ds(&mut st).store.add(one.clone());
            ds(&mut st).store.clauses[0].sent = Some(0);
            let before = run.bytes();
            let (_, alloc) =
                super::super::numeric::tests::measure(|| m_offer(&ix, &mut st, 0, &one, &mut out));
            within(
                &mut fails,
                "holders",
                alloc.peak,
                &out,
                run.bytes() - before,
            );
            if out.sent.len() != N as usize {
                fails.push(format!("mode {mode} holders: {} sent", out.sent.len()));
            }
        }
        report(&fails);
    }

    #[test]
    fn the_destination_scan_is_charged_and_polled() {
        // a notice over a constant that 200,000 contexts the root does not reach registered
        // for sends nothing, charges its reads, checks the run's bounds as it goes, and
        // stops at a refusal after its admission
        const N: u32 = 200_000;
        let mut fails = Vec::new();
        for mode in MODES {
            let mut sig = Sig::new();
            let [a, k] = ["A", "K"].map(|n| sig.class(n));
            let u = sig.ind("u");
            let (ix, _) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let ga = glit(&run, &ix, a, u);
            let RTerm::C(cu) = c(u, &ix) else { panic!() };
            for after in [usize::MAX, 1] {
                let mut st = State::default();
                let mut out = Stops::new(&run);
                let r = rst(&mut st, &out);
                mrep_new(r, &out);
                r.edges.insert(cu, (100_000..100_000 + N).collect());
                out.refuse = true;
                out.after = after;
                let work = run.work();
                rmark(&ix, &mut st, k, ga.xid(), &mut out);
                let (work, polls) = (run.work() - work, out.polls.get());
                let cut = ds(&mut st).store.interrupted;
                if !out.sent.is_empty() || out.held != 0 {
                    fails.push(format!("mode {mode} after {after}: sent or held"));
                }
                if after == 1 {
                    if !cut || polls != 2 {
                        fails.push(format!(
                            "mode {mode}: a refusal after the admission, interrupted {cut}, {polls} checks"
                        ));
                    }
                } else if cut || work < N as usize / WORK_READS - 1 || polls < N as usize / M_SCAN {
                    fails.push(format!(
                        "mode {mode}: {work} work and {polls} checks for {N} reads"
                    ));
                }
            }
        }
        report(&fails);
    }

    #[test]
    fn the_body_checks_are_charged_per_literal() {
        // 256 root clauses with the same 2,048 ground body literals, of which the holder
        // holds all but the last in the body's order, and an offer of the clause to 256
        // such holders: each check of a body literal counts as a read
        const B: usize = 2048;
        const H: u32 = 256;
        let reads = H as usize * (B - 1);
        let mut fails = Vec::new();
        for mode in MODES {
            let mut sig = Sig::new();
            let [k, hd] = ["K", "H"].map(|n| sig.class(n));
            let ss: Vec<Ex> = (0..B).map(|i| sig.class(&format!("S{i}"))).collect();
            let u = sig.ind("u");
            let (ix, _) = probe_ix(sig);
            let mut run = DlRun::new(false, Default::default());
            run.mrepair = mode;
            let body: Vec<Lit> = ss.iter().map(|&s| glit(&run, &ix, s, u)).collect();
            let one = Clause::new(body, vec![glit(&run, &ix, hd, u)]);
            let (&l, &last) = (one.body().first().unwrap(), one.body().last().unwrap());
            // the notice of the body's first literal
            let mut st = State::default();
            let mut out = Stops::new(&run);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            r.mw().gbody.insert(l, (0..H).collect());
            for &m in &one.body()[1..B - 1] {
                r.mw().hold_set.insert((k, m));
            }
            for _ in 0..H {
                ds(&mut st).store.add(one.clone());
            }
            let sample = ds(&mut st).store.clauses[0].clone();
            ds(&mut st).store.clauses.resize(H as usize, sample);
            let work = run.work();
            rmark(&ix, &mut st, k, l.xid(), &mut out);
            let (work, polls) = (run.work() - work, out.polls.get());
            if !out.sent.is_empty() || work < reads / WORK_READS - 1 || polls < reads / M_SCAN {
                fails.push(format!(
                    "mode {mode} notice: {work} work, {polls} checks, {} sent",
                    out.sent.len()
                ));
            }
            // the offer to 256 holders of every body literal but the last
            let mut st = State::default();
            let mut out = Stops::new(&run);
            let r = rst(&mut st, &out);
            mrep_new(r, &out);
            let hs: Vec<Ex> = (100_000..100_000 + H).collect();
            for &m in one.body() {
                r.mw().holders.insert(m, hs.clone());
                if m != last {
                    for &h in &hs {
                        r.mw().hold_set.insert((h, m));
                    }
                }
            }
            ds(&mut st).store.add(one.clone());
            let work = run.work();
            m_offer(&ix, &mut st, 0, &one, &mut out);
            let (work, polls) = (run.work() - work, out.polls.get());
            if !out.sent.is_empty() || work < reads / WORK_READS - 1 || polls < reads / M_SCAN {
                fails.push(format!(
                    "mode {mode} offer: {work} work, {polls} checks, {} sent",
                    out.sent.len()
                ));
            }
        }
        report(&fails);
    }

    #[test]
    fn the_deficit_count_polls_within_a_context() {
        // one holder of 4,096 ground literals over a constant that 256 contexts the root
        // reaches registered for, none of which holds a marker: the count charges the
        // 1,048,576 pairs it checks and polls among them, and a refusal of the first poll
        // inside the holder ends it
        const L: usize = 4096;
        const D: u32 = 256;
        let mut sig = Sig::new();
        let k = sig.class("K");
        let ss: Vec<Ex> = (0..L).map(|i| sig.class(&format!("S{i}"))).collect();
        let u = sig.ind("u");
        let (ix, root) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let RTerm::C(cu) = c(u, &ix) else { panic!() };
        let dests: Vec<Ex> = (100_000..100_000 + D).collect();
        let ctx = |me: Ex| MarkerCtx {
            me,
            held: vec![],
            succ: vec![],
            edges: vec![],
            units: vec![],
        };
        let mut cs = vec![MarkerCtx {
            succ: dests.iter().copied().chain([k]).collect(),
            edges: vec![(cu, dests.clone())],
            ..ctx(root)
        }];
        cs.extend(dests.iter().map(|&d| ctx(d)));
        cs.push(MarkerCtx {
            held: ss.iter().map(|&s| glit(&run, &ix, s, u)).collect(),
            ..ctx(k)
        });
        let def = || MarkerDef {
            bottom: false,
            marks: vec![],
            owed: 0,
            unchecked: 0,
        };
        let defs: Vec<Option<MarkerDef>> = cs.iter().map(|_| Some(def())).collect();
        let (mut polls, mut work) = (0, 0);
        let n = marker_deficits(&ix, &cs, &defs, &run.lits, &mut |w| {
            polls += 1;
            work += w;
            true
        });
        let pairs = L * D as usize;
        assert_eq!(n, Some([pairs, 0, 0]));
        assert!(
            work >= pairs / WORK_READS && polls >= pairs / DIAG_POLL,
            "{work} work, {polls} checks"
        );
        // a check per context passes, and the first one inside the holder refuses
        let mut calls = 0;
        let n = marker_deficits(&ix, &cs, &defs, &run.lits, &mut |_| {
            calls += 1;
            calls <= cs.len()
        });
        assert_eq!((n, calls), (None, cs.len() + 1));
    }

    /// A context K holding a(u) and b(u), with `na` premises K → a(u) ∨ Z ∨ H_i and `nb`
    /// premises Z → b(u) ∨ K ∨ H_j, whose products with the root clause a(u) ∧ b(u) → ⊥ are
    /// all tautologies, and `dead` further premises on a(u) that died, for the diagnostic.
    fn def_ctx(na: usize, nb: usize, dead: usize) -> (Index, DlRun, State, MarkerCtx, Clause) {
        let mut sig = Sig::new();
        let [a, b, k, z] = ["A", "B", "K", "Z"].map(|n| sig.class(n));
        let u = sig.ind("u");
        let hs: Vec<Ex> = (0..na + nb + dead)
            .map(|i| sig.class(&format!("H{i}")))
            .collect();
        let (ix, _) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let (ga, gb) = (glit(&run, &ix, a, u), glit(&run, &ix, b, u));
        let mut st = State::default();
        for &h in &hs[..na] {
            ds(&mut st)
                .store
                .add(Clause::new(vec![Lit::x(k)], vec![ga, Lit::x(z), Lit::x(h)]));
        }
        for &h in &hs[na..na + nb] {
            ds(&mut st)
                .store
                .add(Clause::new(vec![Lit::x(z)], vec![gb, Lit::x(k), Lit::x(h)]));
        }
        for &h in &hs[na + nb..] {
            ds(&mut st)
                .store
                .add(Clause::new(vec![Lit::x(k)], vec![ga, Lit::x(h)]));
        }
        let inv = inv_of(&ix, dix(&ix));
        let order = Rooted {
            ix: &ix,
            lits: &run.lits,
            inv: &inv,
            ground_total: true,
        };
        let mut max = Vec::new();
        while ds(&mut st)
            .store
            .next(&ix, Some(&order), &mut max)
            .is_some()
        {}
        let n = ds(&mut st).store.clauses.len();
        for s in &mut ds(&mut st).store.clauses[n - dead..] {
            s.dead = true;
        }
        let m = MarkerCtx {
            me: k,
            held: vec![ga, gb],
            succ: vec![],
            edges: vec![],
            units: vec![],
        };
        (ix, run, st, m, Clause::new(vec![ga, gb], vec![]))
    }

    #[test]
    fn the_diagnostic_charges_what_it_reads_where_it_reads_it() {
        // 63 x 65 tautological products, short of a multiple of the poll, charge every
        // choice; 8,192 dead premises charge their reads though no list keeps them; a
        // holder of 100,000 literals is read under the run's checks
        let (ix, run, st, m, rg) = def_ctx(63, 65, 0);
        let mut work = 0;
        let def = marker_def(&ix, &m, &st, &[rg], &run.lits, false, &mut |n| {
            work += n;
            true
        });
        assert_eq!(
            def.map(|d| d.map(|d| (d.owed, d.unchecked))),
            Ok(Some((0, 0)))
        );
        assert!(work >= 63 * 65, "{work} work for {} products", 63 * 65);
        // the dead premises are read twice, as stored clauses and as candidates
        let read = |dead| {
            let (ix, run, st, m, rg) = def_ctx(1, 1, dead);
            let mut work = 0;
            let def = marker_def(&ix, &m, &st, &[rg], &run.lits, false, &mut |n| {
                work += n;
                true
            });
            assert!(def.is_ok_and(|d| d.is_some()));
            work
        };
        let (none, dead) = (read(0), read(8192));
        assert!(
            dead - none >= 2 * 8192 / WORK_READS - 16,
            "{} work for 8,192 dead premises",
            dead - none
        );
        let (ix, run, st, mut m, rg) = def_ctx(1, 1, 0);
        m.held.extend((0..100_000).map(Lit::xl));
        let mut polls = 0;
        let def = marker_def(&ix, &m, &st, &[rg], &run.lits, false, &mut |_| {
            polls += 1;
            true
        });
        assert!(def.is_ok_and(|d| d.is_some()));
        assert!(
            polls >= 100_000 / DIAG_POLL,
            "{polls} checks for 100,000 held literals"
        );
    }

    #[test]
    fn a_refusal_at_any_check_of_the_diagnostic_ends_it() {
        // for each check k of a complete count, a run whose check k refuses gets no count
        let (ix, run, st, m, rg) = def_ctx(16, 16, 64);
        let rg = [rg];
        let mut total = 0;
        assert!(
            marker_def(&ix, &m, &st, &rg, &run.lits, false, &mut |_| {
                total += 1;
                true
            })
            .is_ok_and(|d| d.is_some())
        );
        for k in 1..=total {
            let mut calls = 0;
            let def = marker_def(&ix, &m, &st, &rg, &run.lits, false, &mut |_| {
                calls += 1;
                calls < k
            });
            assert!(def.is_err(), "marker_def, check {k} of {total}");
        }
        // the count over a holder of 64 literals and 64 destinations
        let mut sig = Sig::new();
        let k = sig.class("K");
        let ss: Vec<Ex> = (0..64).map(|i| sig.class(&format!("S{i}"))).collect();
        let u = sig.ind("u");
        let (ix, root) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let RTerm::C(cu) = c(u, &ix) else { panic!() };
        let dests: Vec<Ex> = (100_000..100_064).collect();
        let ctx = |me: Ex| MarkerCtx {
            me,
            held: vec![],
            succ: vec![],
            edges: vec![],
            units: vec![],
        };
        let mut cs = vec![MarkerCtx {
            succ: dests.iter().copied().chain([k]).collect(),
            edges: vec![(cu, dests.clone())],
            ..ctx(root)
        }];
        cs.extend(dests.iter().map(|&d| ctx(d)));
        cs.push(MarkerCtx {
            held: ss.iter().map(|&s| glit(&run, &ix, s, u)).collect(),
            ..ctx(k)
        });
        let defs: Vec<Option<MarkerDef>> = cs
            .iter()
            .map(|_| {
                Some(MarkerDef {
                    bottom: false,
                    marks: vec![],
                    owed: 0,
                    unchecked: 0,
                })
            })
            .collect();
        let mut total = 0;
        assert_eq!(
            marker_deficits(&ix, &cs, &defs, &run.lits, &mut |_| {
                total += 1;
                true
            }),
            Some([64 * 64, 0, 0])
        );
        for k in 1..=total {
            let mut calls = 0;
            let n = marker_deficits(&ix, &cs, &defs, &run.lits, &mut |_| {
                calls += 1;
                calls < k
            });
            assert_eq!(n, None, "marker_deficits, check {k} of {total}");
        }
    }

    #[test]
    fn a_replay_over_many_constants_checks_the_bounds() {
        // a registration replay for 200,000 constants that hold no literal reads each, so a
        // stopped run's check ends it before any admission
        for mode in MODES {
            let (_ix, run, mut st, k, _) = many_held(mode);
            let new: Vec<u32> = (1_000_000..1_200_000).collect();
            let mut out = Stops::new(&run);
            let work = run.work();
            m_registered(ds(&mut st), k, &new, &mut out);
            assert!(
                out.polls.get() >= 200_000 / M_SCAN,
                "mode {mode}: {}",
                out.polls.get()
            );
            assert!(run.work() - work >= 200_000 / WORK_READS - 1, "mode {mode}");
            let (_ix, run, mut st, k, _) = many_held(mode);
            let mut out = Stops::new(&run);
            out.refuse = true;
            m_registered(ds(&mut st), k, &new, &mut out);
            assert!(out.asked == 0 && out.held == 0, "mode {mode}");
            assert!(ds(&mut st).store.interrupted, "mode {mode}");
        }
    }

    #[test]
    fn the_deficit_count_reads_each_successor_and_copy() {
        // a root's 50,000 successor entries, and 50,000 copies to total, are read one by one
        // under the run's checks
        let sig = Sig::new();
        let (ix, root) = probe_ix(sig);
        let run = DlRun::new(false, Default::default());
        let none = |me: Ex, succ: Vec<Ex>| MarkerCtx {
            me,
            held: vec![],
            succ,
            edges: vec![],
            units: vec![],
        };
        let def = || {
            Some(MarkerDef {
                bottom: false,
                marks: vec![],
                owed: 1,
                unchecked: 0,
            })
        };
        for (succ, copies) in [(50_000, 1), (0, 50_000)] {
            // one successor listed 50,000 times, which the reach meets once
            let cs = [none(root, std::iter::repeat_n(100_000, succ).collect())];
            let defs: Vec<Option<MarkerDef>> = (0..copies).map(|_| def()).collect();
            let mut polls = 0;
            let n = marker_deficits(&ix, &cs, &defs, &run.lits, &mut |_| {
                polls += 1;
                true
            });
            assert_eq!(n, Some([0, copies as usize, 0]));
            assert!(
                polls >= 50_000 / DIAG_POLL,
                "{polls} checks for {succ} successors and {copies} copies"
            );
        }
    }

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
            steps: false,
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

    /// Item 1220's repair R1 in Pred from a successor over `f_e(u)`: two selected clauses
    /// `H(u) → A(f_e(u)) ∨ Z(x)` and `H(u) → B(f_e(u)) ∨ Z(x)` are the premises of the
    /// successor's `A(x) ∧ B(x) → Z(y)` only when selected partners count, with no fixed
    /// premise and with either one fixed. The clauses cannot go to a predecessor, since
    /// their heads hold an atom over x and one over `f_e(u)`.
    #[test]
    fn root_pred_takes_selected_partners_under_r1() {
        use super::super::dl_clause::Rooted;
        let mut sig = Sig::new();
        let a = sig.class("A");
        let b = sig.class("B");
        let h = sig.class("H");
        let z = sig.class("Z");
        let v = sig.ind("v");
        let r = sig.role("r");
        let e = sig.some_h(r, a);
        let done = saturate_root(sig);
        let ix = &done.ix;
        let lits = &done.run.lits;
        let inv = inv_of(ix, dix(ix));
        let RTerm::C(u) = c(v, ix) else {
            unreachable!()
        };
        let literal = |x| match lits.lit(x, inv) {
            Made::Lit(l) => l,
            _ => panic!("literal"),
        };
        let hyp = literal(XLit::Atom(h, RTerm::C(u)));
        let fa = literal(XLit::Atom(a, RTerm::Fc(e, u)));
        let fb = literal(XLit::Atom(b, RTerm::Fc(e, u)));
        let answer = literal(XLit::Atom(z, RTerm::C(u)));
        let mut st = State::default();
        let o = Rooted {
            ix,
            lits,
            inv: &inv,
            ground_total: true,
        };
        for l in [fa, fb] {
            ds(&mut st)
                .store
                .add(Clause::new(vec![hyp], vec![l, Lit::x(z)]));
        }
        let mut maxima = Vec::new();
        while let Some(i) = ds(&mut st).store.next(ix, Some(&o), &mut maxima) {
            let s = &mut ds(&mut st).store.clauses[i as usize];
            s.sel = super::super::dl_rules::selected_always(lits, inv, &s.c);
        }
        assert!(ds(&mut st).store.clauses.iter().all(|s| s.sel));
        let succ = Clause::new(vec![Lit::x(a), Lit::x(b)], vec![answer]);
        let fixed = side(&ds(&mut st).store.clauses[0].c, fa);
        for (sel, want) in [(false, 0), (true, 1)] {
            let mut found = Found::default();
            root_pred_of(ix, &st, e, u, &succ, None, &mut found, lits, sel);
            assert_eq!(found.products_len(), want, "no fixed premise, sel {sel}");
            let mut found = Found::default();
            root_pred_of(
                ix,
                &st,
                e,
                u,
                &succ,
                Some((fa, &fixed)),
                &mut found,
                lits,
                sel,
            );
            assert_eq!(found.products_len(), want, "fixed premise, sel {sel}");
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
        // A ⊑ ∃r.B ⊔ C, B ⊑ {o} as a clause, B ⊑ G: the hypothetical successor of ∃r.B
        // holds the marker B(x) → B(x), B(x) → x ≈ o and B(x) → G(x). With the blocking,
        // x ≈ o rewrites nothing there, since the successor is no query context; without
        // it, B(x) → G(o) and B(x) → B(o) follow. A told B ⊑ {o} would name o in place
        // of the successor (item 822), so the singleton is a clause here.
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, cc, g] = ["A", "B", "C", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some_h(r, b);
        s.dl.clauses.push((vec![a], vec![e, cc]));
        s.dl.clauses.push((vec![b], vec![no]));
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
    fn a_restriction_into_a_singleton_names_its_constant_whichever_comes_first() {
        // A ⊑ ∀r.C, A ⊑ B, B ⊑ ∃r.{o}, and D ⊑ ∃r.{o} ⊔ E with D ⊑ ∀r.C: A's context
        // holds the restriction as a unit after its universal, D's in a clause, and each
        // holds r(x, o) and C(o) and no literal over the restriction's successor term
        // (item 822)
        if !super::super::dl_rules::nominal_succ() {
            return;
        }
        for named in ["A", "D"] {
            let mut s = Sig::new();
            let no = s.ind("o");
            let [a, b, cc, dd, ee] = ["A", "B", "C", "D", "E"].map(|n| s.class(n));
            let r = s.role("r");
            let e = s.some_h(r, no);
            let all = s.all(r, cc);
            s.sub(a, all);
            s.sub(a, b);
            s.sub(b, e);
            s.sub(dd, all);
            s.dl.clauses.push((vec![dd], vec![e, ee]));
            let at = if named == "A" { a } else { dd };
            let d = saturate_at(s, Some(at));
            let o = c(no, &d.ix);
            let cs = d.clauses_of(at);
            let over_f = cs
                .iter()
                .flat_map(|(bd, hd)| bd.iter().chain(hd))
                .any(|&l| {
                    let (ts, n) = super::super::dl_clause::terms_of(l);
                    ts[..n]
                        .iter()
                        .any(|t| matches!(t, RTerm::Fx(f) | RTerm::Fc(f, _) if *f == e))
                });
            assert!(!over_f, "{named}: a literal over f(x) in {cs:?}");
            let head = |l: XLit| cs.iter().any(|(_, hd)| hd.contains(&l));
            let rxo = XLit::Role(r, RTerm::X, o).canon(inv_of(&d.ix, dix(&d.ix)));
            assert!(head(rxo), "{named}: r(x, o) in {cs:?}");
            assert!(head(XLit::Atom(cc, o)), "{named}: C(o) in {cs:?}");
        }
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
        // gets f(x) ≈ o and G(o), and through Eq r(x, o). The singleton is a clause, since
        // a told B ⊑ {o} names o in place of the successor (item 822).
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, g] = ["A", "B", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some(r, b);
        s.sub(a, e);
        s.dl.clauses.push((vec![b], vec![no]));
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
        // B ⊑ {o}, B ⊑ G, A ⊑ ∃r.B, with B's context saturated before A reaches it, and
        // the singleton a clause, as above
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, g] = ["A", "B", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some(r, b);
        s.sub(a, e);
        s.dl.clauses.push((vec![b], vec![no]));
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

    #[test]
    fn a_hypothetical_successor_s_unconditional_ground_fact_goes_to_the_root() {
        // the hypothetical successor of ∃r.D holds ⊤ → t(x, b) ∨ E(x) and ⊤ → t(x, b) under
        // ⊤ ⊑ ∀t.G: the clause with E(x) gives ⊤ → G(b) ∨ E(x), which the context keeps,
        // and the other gives the fact ⊤ → G(b), which goes to the root and is not kept
        let mut s = Sig::new();
        let nb = s.ind("b");
        let [dd, ee, g] = ["D", "E", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let t = s.role("t");
        let er = s.some_h(r, dd);
        let ag = s.all(t, g);
        s.sub(TOP, ag);
        let (ix, root) = probe_ix(s);
        let h = dix(&ix).hyp[&er];
        let run = DlRun::new(false, Default::default());
        let mut out = Probe {
            run: &run,
            sent: Vec::new(),
            mine: Vec::new(),
        };
        let ub = c(nb, &ix);
        let inv = inv_of(&ix, dix(&ix));
        let l = |w: XLit| match run.lits.lit(w, inv) {
            Made::Lit(l) => l,
            _ => panic!("a literal"),
        };
        let mut st = State {
            init: true,
            ..State::default()
        };
        st.subs.insert(TOP);
        st.subs.insert(ag);
        let tb = l(XLit::Role(t, RTerm::X, ub));
        let gb = l(XLit::Atom(g, ub));
        let kept = |st: &State, head: &[Lit]| {
            dl(st).unwrap().store.clauses.iter().any(|c| {
                !c.dead && c.c.body().is_empty() && {
                    let mut h = c.c.head().to_vec();
                    h.sort_unstable();
                    let mut w = head.to_vec();
                    w.sort_unstable();
                    h == w
                }
            })
        };
        ds(&mut st)
            .store
            .add(Clause::new(Vec::new(), vec![tb, Lit::x(ee)]));
        super::super::dl_rules::flush(&ix, h, &mut st, &mut out, 100);
        assert!(kept(&st, &[gb, Lit::x(ee)]), "⊤ → G(b) ∨ E(x) is kept");
        assert!(
            !out.sent.iter().any(|(_, c)| matches!(c, Concl::RFact(_))),
            "a clause over x stays: {:?}",
            out.sent
        );
        ds(&mut st).store.add(Clause::new(Vec::new(), vec![tb]));
        super::super::dl_rules::flush(&ix, h, &mut st, &mut out, 100);
        let facts: Vec<&Clause> = out
            .sent
            .iter()
            .filter_map(|(to, c)| match c {
                Concl::RFact(cid) if *to == root => Some(run.table.get(*cid)),
                _ => None,
            })
            .collect();
        assert_eq!(facts.len(), 1, "one fact to the root: {:?}", out.sent);
        assert!(
            facts[0].body().is_empty() && facts[0].head() == [gb],
            "⊤ → G(b)"
        );
        assert!(!kept(&st, &[gb]), "the fact is not kept");
    }

    #[test]
    fn an_inconsistency_a_hypothetical_successor_finds_without_assumptions_reaches_the_root() {
        // {a} ⊑ ∃r.D, ⊤ ⊑ ∃t.{b}, ⊤ ⊑ ∀t.E, E ⊑ ⊥: every element has b as a t-successor, so
        // b is in E and the ontology is inconsistent, which the root's successor for f(a)
        // finds as the empty clause with an empty body
        let mut s = Sig::new();
        let na = s.ind("a");
        let nb = s.ind("b");
        let [dd, ee] = ["D", "E"].map(|n| s.class(n));
        let r = s.role("r");
        let t = s.role("t");
        let er = s.some_h(r, dd);
        let et = s.some_h(t, nb);
        let ae = s.all(t, ee);
        s.sub(na, er);
        s.sub(TOP, et);
        s.sub(TOP, ae);
        s.sub(ee, BOTTOM);
        let d = saturate_root(s);
        assert!(d.bottom());
    }

    #[test]
    fn a_selected_clause_headed_by_an_atom_over_x_still_takes_the_hyper_over_x() {
        // Q ⊑ ∃s.D, Q ⊑ ∃t.{a}, Q ⊑ ∀t.∀r.A, D ⊑ ∃r⁻.{a}, A ⊑ ⊥: a Q has ∀r.A at a and a D
        // reached from a by r, which is in A, so Q is unsatisfiable. The D context holds
        // the marker of ∀r.A(a) and the local role atom r(a, x), whose Hyper gives the
        // selected clause ∀r.A(a) → A(x), which Pred cannot carry, so the context itself
        // takes the Hyper over x to ∀r.A(a) → ⊥
        let mut s = Sig::new();
        let na = s.ind("a");
        let [q, dd, aa] = ["Q", "D", "A"].map(|n| s.class(n));
        let r = s.role("r");
        let t = s.role("t");
        let sr = s.role("s");
        let ri = s.it.role(RoleName::Inverse(ObjectProperty::new("urn:t:r")));
        let es = s.some_h(sr, dd);
        let et = s.some_h(t, na);
        let ara = s.all(r, aa);
        let atra = s.all(t, ara);
        let eri = s.some_h(ri, na);
        s.sub(q, es);
        s.sub(q, et);
        s.sub(q, atra);
        s.sub(dd, eri);
        s.sub(aa, BOTTOM);
        let d = saturate_at(s, Some(q));
        assert!(d.bottom(), "Q ⊑ ⊥");
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
