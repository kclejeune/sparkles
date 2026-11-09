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
//! a root literal. The root's other rules wait for the later steps of the phase: Succ on
//! the literals over `f_e(u)`, Join on ground body literals, and the clauses over y that
//! r-Succ's markers bring. The at-most restrictions and the qualified roles of counting
//! are not read in the root.

use super::dl_clause::{Clause, Lit, Made, RTerm, Rooted, XLit};
use super::dl_rules::{DlIx, Found, Side, alls, dix, dl, drain_with, ds, merge, side, unit, with};
use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use super::saturate::{Concl, Sink, State, table_bytes};
use rustc_hash::FxHashMap;
use std::sync::atomic::Ordering;

/// What the root keeps beside its store: its processed clauses by constant.
#[derive(Default, Clone)]
pub struct RootSt {
    /// the processed clauses with a maximal ground atom `B(u)`, by u
    by_const: FxHashMap<u32, Vec<u32>>,
    /// the processed clauses with a maximal role atom that has the constant u at an end,
    /// by u
    roles_of: FxHashMap<u32, Vec<u32>>,
}

impl RootSt {
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<RootSt>()
            + table_bytes(self.by_const.capacity(), 32)
            + self
                .by_const
                .values()
                .map(|v| v.capacity() * 4)
                .sum::<usize>()
            + table_bytes(self.roles_of.capacity(), 32)
            + self
                .roles_of
                .values()
                .map(|v| v.capacity() * 4)
                .sum::<usize>()
    }

    /// Adds `id` to the list of `k` in `map`, and answers the bytes the map and the list
    /// grew by.
    fn push(map: &mut FxHashMap<u32, Vec<u32>>, k: u32, id: u32) -> usize {
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
    let x = ds(st);
    x.root.get_or_insert_with(Box::default);
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
        let r = x.root.get_or_insert_with(Box::default);
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
    for &l in max {
        let prem = side(c, l);
        let q = super::dl_eq::Eqs {
            ix,
            dx,
            lits: &out.dl().lits,
            root: true,
        };
        q.on_max(st, l, &prem, &mut found);
        match out.dl().lits.view(l, inv_of(ix, dx)) {
            XLit::Atom(d, RTerm::C(u)) => hyper_at(ix, st, out, d, u, &prem, &mut found),
            v @ XLit::Role(..) => {
                for (u, s, t) in from(v, inv_of(ix, dx)) {
                    on_role(ix, st, out, s, u, t, &prem, &mut found);
                }
            }
            // Succ and Join come with the later steps of the phase
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
            let dx = dix(&self.ix);
            let inv = inv_of(&self.ix, dx);
            let st = self.ctx[self.root as usize].get().unwrap().st.lock();
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
    }

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
            let c = ctx[root as usize].get_or_init(Box::default);
            let mut st = c.st.lock();
            st.init = true;
            let x = ds(&mut st);
            x.root.get_or_insert_with(Box::default);
            for (b, h) in &seeds {
                x.store.add(Clause::new(
                    b.iter().map(l).collect(),
                    h.iter().map(l).collect(),
                ));
            }
        }
        let root = at.unwrap_or(root);
        for &e in before {
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
        // A ⊑ ∃r.B, B ⊑ C ⊔ {o}, {o} ⊑ G: A's context has no G(o) alone
        let mut s = Sig::new();
        let no = s.ind("o");
        let [a, b, cc, g] = ["A", "B", "C", "G"].map(|n| s.class(n));
        let r = s.role("r");
        let e = s.some(r, b);
        s.sub(a, e);
        s.dl.clauses.push((vec![b], vec![cc, no]));
        s.sub(no, g);
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
}
