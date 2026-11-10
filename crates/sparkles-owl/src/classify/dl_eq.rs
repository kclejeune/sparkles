//! The equality rules of a rooted run of roadmap item 603's Tier D: Eq, Ineq and Factor of
//! the journal's Table 2 (Tena Cucala, Cuenca Grau and Horrocks, AIJ 2021), over the
//! general form of literals, in every context of the run whether it counts or not.
//!
//! An equality `s₁ ≈ t₁` that is maximal in its clause, read in either direction for which
//! `t₁` is not above `s₁` and which is not `x ≈ y` or `y ≈ u`, rewrites an occurrence of
//! `s₁` in a maximal literal `s₂ ⋈ t₂` of another clause, where `t₂` is not above `s₂`. An
//! occurrence that is x must be in a side that holds no successor term and no y. A constant
//! is rewritten everywhere in the side, inside `f(u)` included, in every context, as Table 2
//! has the root do, so that `S(u, f(u))` rewritten by `u ≈ v` is `S(v, f(v))` (F24, item
//! 804's departures), and a successor term is rewritten one position at a time. The run's
//! literals hold no `t ≉ t`, since [`Lits::lit`] makes it
//! false, which is the Ineq rule. Factor takes two equalities with one left side in a head.
//!
//! The equality `x ≈ u` as the rewriting equality is blocked outside query contexts (F24,
//! item 804's third departure, behind `SPARKLES_ROOTED_EQ_BLOCK`): it rewrites `x` to `u`
//! only in the unit of a core atom of the context of a named class, and `u` to `x` only
//! in a named-class atom over `u` in such a context, the two forms the journal's Lemmas 21
//! and 22 use. The equality is still a rewritten literal, so `u ≈ v` turns `x ≈ u` into
//! `x ≈ v`, and Factor takes it as any equality.
//!
//! The premises in place of a context other than the root are its units: the atoms `B(x)`
//! of its subsumers, and the role atom and core of a unit existential restriction over its
//! successor term.

use super::dl_clause::{Lit, Lits, Made, RTerm, Rooted, XLit, rank, terms_of};
use super::dl_rules::{DlIx, Found, Side, dl, merge, side, unit, with};
use super::index::{Ex, Index, Role, TOP};
use super::saturate::State;
use std::sync::OnceLock;

/// Whether `x ≈ u` rewrites only in the two forms of a query context, item 804's third
/// departure. `SPARKLES_ROOTED_EQ_BLOCK=0` turns the blocking off, for measurement.
pub fn eq_block() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_EQ_BLOCK").is_none_or(|v| v != "0"))
}

/// What the equality rules read beside a context's state: the index, the run's literals,
/// whether the context is the root, and the named class of a query context, which is its
/// core atom.
pub(super) struct Eqs<'a> {
    pub ix: &'a Index,
    pub dx: &'a DlIx,
    pub lits: &'a Lits,
    pub root: bool,
    pub query: Option<Ex>,
}

impl Eqs<'_> {
    fn inv(&self) -> impl Fn(Role) -> Option<Role> + Copy + '_ {
        move |r| self.dx.inv(self.ix, r)
    }

    fn view(&self, l: Lit) -> XLit {
        self.lits.view(l, self.inv())
    }

    fn lit(&self, l: XLit) -> Made {
        self.lits.lit(l, self.inv())
    }

    /// Whether `s₁ ≈ t₁` may rewrite `s₁`: `t₁` is not above it, and it is not `x ≈ y` or
    /// `y ≈ u`.
    fn orients(s1: RTerm, t1: RTerm) -> bool {
        use RTerm::*;
        !Rooted::a_gt(t1, s1) && !matches!((s1, t1), (X, Y) | (Y, X) | (Y, C(_)) | (C(_), Y))
    }

    /// Whether Eq with `s₁ ↦ t₁` may rewrite the literal `v` of the premise `prem` (the
    /// premise without v). An equality other than `x ≈ u` always may. `x ↦ u` rewrites the
    /// unit of a query context's core atom, and `u ↦ x` a named-class atom over u in a
    /// query context, and nothing else when the blocking is on.
    fn admits(&self, s1: RTerm, t1: RTerm, v: XLit, prem: &Side) -> bool {
        use RTerm::*;
        match (s1, t1) {
            (X, C(_)) => {
                !eq_block()
                    || self.query.is_some_and(|me| {
                        prem.0.is_empty() && prem.1.is_empty() && v == XLit::Atom(me, X)
                    })
            }
            (C(u), X) => {
                !eq_block()
                    || self.query.is_some()
                        && matches!(v, XLit::Atom(b, C(w)) if w == u && rank(self.ix, b).0 == 1)
            }
            _ => true,
        }
    }

    /// The literals `v` rewrites to with `s₁ ↦ t₁`, one for each position the rule allows.
    fn rewrites(&self, v: XLit, s1: RTerm, t1: RTerm, out: &mut Vec<Made>) {
        use RTerm::*;
        // a constant everywhere in the side, inside f(u) too, in every context
        let everywhere = matches!(s1, C(_));
        let sub = |t: RTerm| -> RTerm {
            match (t, s1, t1) {
                _ if t == s1 => t1,
                (Fc(e, u), C(w), C(z)) if everywhere && u == w => Fc(e, z),
                _ => t,
            }
        };
        let holds =
            |t: RTerm| t == s1 || everywhere && matches!((t, s1), (Fc(_, u), C(w)) if u == w);
        // an occurrence that is x needs a side without successor terms and y
        let plain = |ts: &[RTerm]| ts.iter().all(|t| matches!(t, X | C(_)));
        match v {
            XLit::Atom(b, a) => {
                if holds(a) && (s1 != X || plain(&[a])) {
                    out.push(self.lit(XLit::Atom(b, sub(a))));
                }
            }
            XLit::Role(r, a, b) => {
                if s1 == X && !plain(&[a, b]) {
                    return;
                }
                if everywhere {
                    if holds(a) || holds(b) {
                        out.push(self.lit(XLit::Role(r, sub(a), sub(b))));
                    }
                    return;
                }
                if a == s1 {
                    out.push(self.lit(XLit::Role(r, t1, b)));
                }
                if b == s1 {
                    out.push(self.lit(XLit::Role(r, a, t1)));
                }
            }
            XLit::Eq(a, b) | XLit::Ne(a, b) => {
                let make = |s: RTerm, t: RTerm| match v {
                    XLit::Eq(..) => XLit::Eq(s, t),
                    _ => XLit::Ne(s, t),
                };
                // the side s₂ with t₂ not above it
                for (s2, t2) in [(a, b), (b, a)] {
                    if Rooted::a_gt(t2, s2) || !holds(s2) {
                        continue;
                    }
                    if s1 == X && !plain(&[s2]) {
                        continue;
                    }
                    out.push(self.lit(make(sub(s2), t2)));
                    if a == b {
                        break;
                    }
                }
            }
        }
    }

    /// The premises whose maximal literals mention `t`, each with its literal: the stored
    /// clauses, and the units in place of a context other than the root.
    fn targets(&self, st: &State, t: RTerm, out: &mut Vec<(Side, XLit)>) {
        out.clear();
        if !self.root {
            match t {
                RTerm::X => {
                    for &a in &st.subs {
                        if a != TOP {
                            out.push((unit(), XLit::Atom(a, RTerm::X)));
                        }
                    }
                }
                RTerm::Fx(e) => {
                    let mut us = Vec::new();
                    unit_lits(self, st, e, &mut us);
                    out.extend(us.into_iter().map(|l| (unit(), l)));
                }
                _ => {}
            }
        }
        let Some(d) = dl(st) else { return };
        let Some(by) = d.store.terms.as_deref() else {
            return;
        };
        for &i in by.any.get(&t).into_iter().flatten() {
            let s = &d.store.clauses[i as usize];
            if s.dead {
                continue;
            }
            for &l in d.store.maxima(i) {
                out.push((side(&s.c, l), self.view(l)));
            }
        }
    }

    /// The rules on a new premise `prem` whose maximal literal is `l`: as an equality it
    /// rewrites the premises that mention its side and factors with its own head, and as
    /// any literal it is rewritten by the stored equalities of its terms.
    pub fn on_max(&self, st: &State, l: Lit, prem: &Side, found: &mut Found) {
        let v = self.view(l);
        let mut made = Vec::new();
        let mut ts = Vec::new();
        if let XLit::Eq(a, b) = v {
            for (s1, t1) in [(a, b), (b, a)] {
                if Rooted::a_gt(t1, s1) {
                    continue;
                }
                // Factor has no exclusions of x ≈ y and y ≈ u, which only Eq has
                self.factor(prem, l, s1, t1, found);
                if !Eqs::orients(s1, t1) {
                    continue;
                }
                self.targets(st, s1, &mut ts);
                for (c2, v2) in &ts {
                    if *v2 == v && c2 == prem || !self.admits(s1, t1, *v2, c2) {
                        continue;
                    }
                    made.clear();
                    self.rewrites(*v2, s1, t1, &mut made);
                    push(found, prem, c2, &made);
                }
            }
        }
        self.into(st, v, prem, found);
    }

    /// A new premise in place `prem` of `v`, a unit of a context other than the root: the
    /// stored equalities of its terms rewrite it.
    pub fn on_unit(&self, st: &State, v: XLit, found: &mut Found) {
        self.into(st, v, &unit(), found);
    }

    /// The stored equalities whose side is a term of `v` rewrite `prem`'s `v`.
    fn into(&self, st: &State, v: XLit, prem: &Side, found: &mut Found) {
        let Some(d) = dl(st) else { return };
        let Some(by) = d.store.terms.as_deref() else {
            return;
        };
        let (ts, n) = terms_of(v);
        let mut seen: Vec<RTerm> = Vec::with_capacity(4);
        let mut made = Vec::new();
        for &t in &ts[..n] {
            let mut look = vec![t];
            if let RTerm::Fc(_, u) = t {
                look.push(RTerm::C(u));
            }
            for s1 in look {
                if seen.contains(&s1) {
                    continue;
                }
                seen.push(s1);
                for &i in by.eqs.get(&s1).into_iter().flatten() {
                    let s = &d.store.clauses[i as usize];
                    if s.dead {
                        continue;
                    }
                    for &e in d.store.maxima(i) {
                        let XLit::Eq(a, b) = self.view(e) else {
                            continue;
                        };
                        let t1 = if a == s1 { b } else { a };
                        if (a != s1 && b != s1) || !Eqs::orients(s1, t1) {
                            continue;
                        }
                        if !self.admits(s1, t1, v, prem) {
                            continue;
                        }
                        let c1 = side(&s.c, e);
                        if self.view(e) == v && &c1 == prem {
                            continue;
                        }
                        made.clear();
                        self.rewrites(v, s1, t1, &mut made);
                        push(found, &c1, prem, &made);
                    }
                }
            }
        }
    }

    /// Factor: `Γ → Δ ∨ s ≈ t₀ ∨ s ≈ t₁`, with `s ≈ t₁` maximal and read as `s ↦ t₁`,
    /// gives `Γ → Δ ∨ t₀ ≉ t₁ ∨ s ≈ t₁`.
    fn factor(&self, prem: &Side, l: Lit, s: RTerm, t1: RTerm, found: &mut Found) {
        for &o in &prem.1 {
            let XLit::Eq(a, b) = self.view(o) else {
                continue;
            };
            let t0 = if a == s {
                b
            } else if b == s {
                a
            } else {
                continue;
            };
            let mut c: Side = (
                prem.0.clone(),
                prem.1.iter().copied().filter(|&m| m != o).collect(),
            );
            c.1.push(l);
            match self.lit(XLit::Ne(t0, t1)) {
                Made::Lit(n) => c.1.push(n),
                Made::False => {}
                Made::True => continue,
            }
            found.push(c);
        }
    }
}

/// The conclusions of rewriting `c2` with `c1`, one for each literal made: a false literal
/// leaves the clause without it, and a true one makes no conclusion.
fn push(found: &mut Found, c1: &Side, c2: &Side, made: &[Made]) {
    for &m in made {
        match m {
            Made::Lit(l) => found.push(with(&merge(c1, c2), l)),
            Made::False => found.push(merge(c1, c2)),
            Made::True => {}
        }
    }
}

/// The literals over `f_e(x)` that a unit existential restriction e holds, its role and its
/// successor's core, in general form.
fn unit_lits(q: &Eqs<'_>, st: &State, e: Ex, out: &mut Vec<XLit>) {
    out.clear();
    if !st.subs.contains(&e) {
        return;
    }
    let f = RTerm::Fx(e);
    if let super::index::Node::Exist(r, _) = q.ix.head(e).node {
        out.push(XLit::Role(r, RTerm::X, f).canon(q.inv()));
    }
    for &a in q.dx.core(e) {
        if a != TOP {
            out.push(XLit::Atom(a, f));
        }
    }
}
