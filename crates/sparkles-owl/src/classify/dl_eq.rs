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
//! in a named-class atom over `u` in such a context or that of `owl:Thing` (item 1070), the two forms the journal's Lemmas 21
//! and 22 use. The equality is still a rewritten literal, so `u ≈ v` turns `x ≈ u` into
//! `x ≈ v`, and Factor takes it as any equality.
//!
//! An Eq or Factor whose equality and rewritten literal hold neither `x` nor `y` is a ground
//! inference, and the restriction on ground inferences (F24, item 804's first departure)
//! leaves out its conclusion when the conclusion has a body and holds neither.
//!
//! The premises in place of a context other than the root are its units: the atoms `B(x)`
//! of its subsumers, and the role atom and core of a unit existential restriction over its
//! successor term.

use super::dl_clause::{Lit, Lits, Made, RTerm, Rooted, XLit, admit_lit, rank, terms_of};
use super::dl_rules::{
    DlIx, Found, Meter, Side, dl, local, merge_parts, no_xy, side_exact_bytes, side_parts_bytes,
    unit,
};
use super::index::{BOTTOM, Ex, Index, Role, TOP};
use super::saturate::{Guard, Sink, State};
use std::sync::OnceLock;

/// Whether `x ≈ u` rewrites only in the two forms of a query context, item 804's third
/// departure. `SPARKLES_ROOTED_EQ_BLOCK=0` turns the blocking off, for measurement.
pub fn eq_block() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_EQ_BLOCK").is_none_or(|v| v != "0"))
}

/// Whether a rooted query skips Eq products whose rewritten atom it already holds.
/// The experiment is off unless `SPARKLES_ROOTED_EQ_KNOWN=1`.
pub(super) fn skip_known_atoms_switch() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_EQ_KNOWN").is_some_and(|v| v == "1"))
}

/// The rule's name in the derivation trace with the shape of its rewriting equality
/// `s₁ ↦ t₁`, by the kinds of terms: x, a constant, `f(x)` and `f(u)`.
fn shape(rule: &'static str, s1: RTerm, t1: RTerm) -> &'static str {
    use RTerm::*;
    let k = |t: RTerm| match t {
        X | Y => 0,
        C(_) => 1,
        Fx(_) => 2,
        Fc(..) => 3,
    };
    const EQ: [&str; 16] = [
        "eq:x>x", "eq:x>c", "eq:x>fx", "eq:x>fc", "eq:c>x", "eq:c>c", "eq:c>fx", "eq:c>fc",
        "eq:fx>x", "eq:fx>c", "eq:fx>fx", "eq:fx>fc", "eq:fc>x", "eq:fc>c", "eq:fc>fx", "eq:fc>fc",
    ];
    const INTO: [&str; 16] = [
        "eqinto:x>x",
        "eqinto:x>c",
        "eqinto:x>fx",
        "eqinto:x>fc",
        "eqinto:c>x",
        "eqinto:c>c",
        "eqinto:c>fx",
        "eqinto:c>fc",
        "eqinto:fx>x",
        "eqinto:fx>c",
        "eqinto:fx>fx",
        "eqinto:fx>fc",
        "eqinto:fc>x",
        "eqinto:fc>c",
        "eqinto:fc>fx",
        "eqinto:fc>fc",
    ];
    let i = k(s1) * 4 + k(t1);
    if rule == "eq" { EQ[i] } else { INTO[i] }
}

/// What the equality rules read beside a context's state: the index, whether the context
/// is the root, and the named class of a query context, which is its core atom.
pub(super) struct Eqs<'a> {
    pub ix: &'a Index,
    pub dx: &'a DlIx,
    pub root: bool,
    pub query: Option<Ex>,
}

fn side_eq(prem: &Side, body: &[Lit], head: &[Lit], without: Option<Lit>) -> bool {
    prem.0 == body
        && prem
            .1
            .iter()
            .copied()
            .eq(head.iter().copied().filter(|&l| Some(l) != without))
}

impl Eqs<'_> {
    fn inv(&self) -> impl Fn(Role) -> Option<Role> + Copy + '_ {
        move |r| self.dx.inv(self.ix, r)
    }

    fn view(&self, lits: &Lits, l: Lit) -> XLit {
        lits.view(l, self.inv())
    }

    fn lit<S: Sink>(&self, out: &mut Guard<'_, S>, l: XLit) -> Option<Made> {
        admit_lit(l, self.inv(), out)
    }

    /// Whether `s₁ ≈ t₁` may rewrite `s₁`: `t₁` is not above it, and it is not `x ≈ y` or
    /// `y ≈ u`.
    fn orients(s1: RTerm, t1: RTerm) -> bool {
        use RTerm::*;
        !Rooted::a_gt(t1, s1) && !matches!((s1, t1), (X, Y) | (Y, X) | (Y, C(_)) | (C(_), Y))
    }

    /// Whether Eq with `s₁ ↦ t₁` may rewrite the literal `v` of the premise `(body, head)`
    /// (excluding `without`). An equality other than `x ≈ u` always may. `x ↦ u` rewrites
    /// the unit of a query context's core atom, and `u ↦ x` a named-class atom over u in a
    /// query context, and nothing else when the blocking is on.
    fn admits_parts(
        &self,
        s1: RTerm,
        t1: RTerm,
        v: XLit,
        body: &[Lit],
        head: &[Lit],
        without: Option<Lit>,
    ) -> bool {
        use RTerm::*;
        match (s1, t1) {
            (X, C(_)) => {
                !eq_block()
                    || self.query.is_some_and(|me| {
                        body.is_empty()
                            && !head.iter().any(|&l| Some(l) != without)
                            && v == XLit::Atom(me, X)
                    })
            }
            (C(u), X) => {
                !eq_block()
                    || self.query.is_some()
                        && matches!(v, XLit::Atom(b, C(w)) if w == u && rank(self.ix, b).0 == 2)
            }
            _ => true,
        }
    }

    /// For B(u), return u when rewriting it to x would put a known unit B(x)
    /// in the head. Derive removes a denied atom or bottom before it checks
    /// the units, so those atoms still take the ordinary path.
    fn known_atom_constant(st: &State, v: XLit) -> Option<RTerm> {
        let XLit::Atom(b, u @ RTerm::C(_)) = v else {
            return None;
        };
        (b != BOTTOM
            && st.subs.contains(&b)
            && dl(st).is_none_or(|x| x.neg.binary_search(&b).is_err()))
        .then_some(u)
    }

    /// The literals `v` rewrites to with `s₁ ↦ t₁`, one for each position the rule allows.
    /// Returns `false` if literal admission failed.
    fn rewrites<S: Sink>(
        &self,
        v: XLit,
        s1: RTerm,
        t1: RTerm,
        out: &mut Vec<Made>,
        guard: &mut Guard<'_, S>,
    ) -> bool {
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
                    let Some(m) = self.lit(guard, XLit::Atom(b, sub(a))) else {
                        return false;
                    };
                    out.push(m);
                }
            }
            XLit::Role(r, a, b) => {
                if s1 == X && !plain(&[a, b]) {
                    return true;
                }
                if everywhere {
                    if holds(a) || holds(b) {
                        let Some(m) = self.lit(guard, XLit::Role(r, sub(a), sub(b))) else {
                            return false;
                        };
                        out.push(m);
                    }
                    return true;
                }
                if a == s1 {
                    let Some(m) = self.lit(guard, XLit::Role(r, t1, b)) else {
                        return false;
                    };
                    out.push(m);
                }
                if b == s1 {
                    let Some(m) = self.lit(guard, XLit::Role(r, a, t1)) else {
                        return false;
                    };
                    out.push(m);
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
                    let Some(m) = self.lit(guard, make(sub(s2), t2)) else {
                        return false;
                    };
                    out.push(m);
                    if a == b {
                        break;
                    }
                }
            }
        }
        true
    }

    /// The rules on a new premise `prem` whose maximal literal is `l`: as an equality it
    /// rewrites the premises that mention its side and factors with its own head, and as
    /// any literal it is rewritten by the stored equalities of its terms. A fully selected
    /// premise (`sel`) takes only local inferences ([`super::dl_rules::local`]); a premise
    /// cut only by `gbody` (item 1250) is blocked from purely `no_xy` Eq and Factor pairs
    /// while remaining eligible for mixed non-local pairs.
    #[allow(clippy::too_many_arguments)]
    pub fn on_max<S: Sink>(
        &self,
        st: &State,
        l: Lit,
        prem: &Side,
        found: &mut Found,
        guard: &mut Guard<'_, S>,
        sel: bool,
        gbody: bool,
    ) -> bool {
        found.rule = "eqmax";
        let v = self.view(&guard.dl().lits, l);
        let mut made = Vec::new();
        if let XLit::Eq(a, b) = v {
            for (s1, t1) in [(a, b), (b, a)] {
                if Rooted::a_gt(t1, s1) {
                    continue;
                }
                // Factor has no exclusions of x ≈ y and y ≈ u, which only Eq has.
                // Preserve full selection's asymmetry (testing locality on the maximal
                // equality `v` alone) while checking `gbody` per pair inside `factor`.
                if (!sel || local(v)) && !self.factor(prem, l, s1, t1, found, guard, gbody) {
                    return false;
                }
                found.rule = "eq";
                if !Eqs::orients(s1, t1) {
                    continue;
                }
                // Targets read `st` immutably while `guard` may admit literals and `Found::one`.
                let ok = if !self.root {
                    match s1 {
                        RTerm::X => {
                            let mut ok = true;
                            for &a in &st.subs {
                                if a != TOP {
                                    let v2 = XLit::Atom(a, RTerm::X);
                                    if (v2 == v && prem.0.is_empty() && prem.1.is_empty())
                                        || !self.admits_parts(s1, t1, v2, &[], &[], None)
                                        || (sel && !(local(v) || local(v2)))
                                    {
                                        continue;
                                    }
                                    made.clear();
                                    if !self.rewrites(v2, s1, t1, &mut made, guard) {
                                        ok = false;
                                        break;
                                    }
                                    found.rule = shape("eq", s1, t1);
                                    if !self.push_parts(
                                        st,
                                        found,
                                        prem,
                                        &[],
                                        &[],
                                        None,
                                        &made,
                                        no_xy(v) && no_xy(v2),
                                        false,
                                        guard,
                                    ) {
                                        ok = false;
                                        break;
                                    }
                                }
                            }
                            ok
                        }
                        RTerm::Fx(e) => {
                            let mut us = Vec::new();
                            unit_lits(self, st, e, &mut us);
                            let mut ok = true;
                            for v2 in us {
                                if (v2 == v && prem.0.is_empty() && prem.1.is_empty())
                                    || !self.admits_parts(s1, t1, v2, &[], &[], None)
                                    || (sel && !(local(v) || local(v2)))
                                {
                                    continue;
                                }
                                made.clear();
                                if !self.rewrites(v2, s1, t1, &mut made, guard) {
                                    ok = false;
                                    break;
                                }
                                found.rule = shape("eq", s1, t1);
                                if !self.push_parts(
                                    st,
                                    found,
                                    prem,
                                    &[],
                                    &[],
                                    None,
                                    &made,
                                    no_xy(v) && no_xy(v2),
                                    false,
                                    guard,
                                ) {
                                    ok = false;
                                    break;
                                }
                            }
                            ok
                        }
                        _ => true,
                    }
                } else {
                    true
                };
                if !ok {
                    return false;
                }
                if let Some(d) = dl(st)
                    && let Some(by) = d.store.terms.as_deref()
                {
                    let skip_known = guard.dl().skip_known_eq_atoms
                        && self.query.is_some()
                        && matches!((s1, t1), (RTerm::C(_), RTerm::X));
                    // Known pairs do not reach literal admission, so poll their scan
                    // even when it produces no conclusions.
                    let scan = Meter::checks();
                    if skip_known && guard.stopped() {
                        return false;
                    }
                    for &i in by.any.get(&s1).into_iter().flatten() {
                        if skip_known && !scan.read(1, guard) {
                            return false;
                        }
                        let s = &d.store.clauses[i as usize];
                        if s.dead {
                            continue;
                        }
                        for &l2 in d.store.maxima(i) {
                            if skip_known && !scan.read(1, guard) {
                                return false;
                            }
                            let v2 = self.view(&guard.dl().lits, l2);
                            if (skip_known && Self::known_atom_constant(st, v2) == Some(s1))
                                || (v2 == v && side_eq(prem, s.c.body(), s.c.head(), Some(l2)))
                                || !self.admits_parts(s1, t1, v2, s.c.body(), s.c.head(), Some(l2))
                            {
                                continue;
                            }
                            let pure = no_xy(v) && no_xy(v2);
                            if (sel || s.sel || (pure && (gbody || s.gbody)))
                                && !(local(v) || local(v2))
                            {
                                continue;
                            }
                            made.clear();
                            if !self.rewrites(v2, s1, t1, &mut made, guard) {
                                return false;
                            }
                            found.rule = shape("eq", s1, t1);
                            if !self.push_parts(
                                st,
                                found,
                                prem,
                                s.c.body(),
                                s.c.head(),
                                Some(l2),
                                &made,
                                pure,
                                s.fact,
                                guard,
                            ) {
                                return false;
                            }
                        }
                    }
                }
            }
        }
        self.into(st, v, prem, found, guard, sel, gbody)
    }

    /// A new premise in place `prem` of `v`, a unit of a context other than the root: the
    /// stored equalities of its terms rewrite it.
    pub fn on_unit<S: Sink>(
        &self,
        st: &State,
        v: XLit,
        found: &mut Found,
        guard: &mut Guard<'_, S>,
    ) -> bool {
        found.rule = "equnit";
        self.into(st, v, &unit(), found, guard, false, false)
    }

    /// The stored equalities whose side is a term of `v` rewrite `prem`'s `v`.
    #[allow(clippy::too_many_arguments)]
    fn into<S: Sink>(
        &self,
        st: &State,
        v: XLit,
        prem: &Side,
        found: &mut Found,
        guard: &mut Guard<'_, S>,
        sel: bool,
        gbody: bool,
    ) -> bool {
        found.rule = "eqinto";
        let Some(d) = dl(st) else { return true };
        let Some(by) = d.store.terms.as_deref() else {
            return true;
        };
        let known = if guard.dl().skip_known_eq_atoms && self.query.is_some() {
            Self::known_atom_constant(st, v)
        } else {
            None
        };
        let scan = Meter::checks();
        if known.is_some() && guard.stopped() {
            return false;
        }
        let (ts, n) = terms_of(v);
        let mut seen = [RTerm::X; 4];
        let mut seen_len = 0usize;
        let mut made = Vec::new();
        for &t in &ts[..n] {
            let mut look = [t, RTerm::X];
            let mut look_len = 1usize;
            if let RTerm::Fc(_, u) = t {
                look[1] = RTerm::C(u);
                look_len = 2;
            }
            for &s1 in &look[..look_len] {
                if seen[..seen_len].contains(&s1) {
                    continue;
                }
                seen[seen_len] = s1;
                seen_len += 1;
                for &i in by.eqs.get(&s1).into_iter().flatten() {
                    if known.is_some() && !scan.read(1, guard) {
                        return false;
                    }
                    let s = &d.store.clauses[i as usize];
                    if s.dead {
                        continue;
                    }
                    for &e in d.store.maxima(i) {
                        if known.is_some() && !scan.read(1, guard) {
                            return false;
                        }
                        let ve = self.view(&guard.dl().lits, e);
                        let XLit::Eq(a, b) = ve else {
                            continue;
                        };
                        let t1 = if a == s1 { b } else { a };
                        if (t1 == RTerm::X && known == Some(s1))
                            || (a != s1 && b != s1)
                            || !Eqs::orients(s1, t1)
                        {
                            continue;
                        }
                        if !self.admits_parts(s1, t1, v, &prem.0, &prem.1, None) {
                            continue;
                        }
                        let pure = no_xy(ve) && no_xy(v);
                        if (sel || s.sel || (pure && (gbody || s.gbody)))
                            && !(local(v) || local(ve))
                        {
                            continue;
                        }
                        if ve == v && side_eq(prem, s.c.body(), s.c.head(), Some(e)) {
                            continue;
                        }
                        made.clear();
                        if !self.rewrites(v, s1, t1, &mut made, guard) {
                            return false;
                        }
                        found.rule = shape("eqinto", s1, t1);
                        if !self.push_parts(
                            st,
                            found,
                            prem,
                            s.c.body(),
                            s.c.head(),
                            Some(e),
                            &made,
                            pure,
                            s.fact,
                            guard,
                        ) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// Factor: `Γ → Δ ∨ s ≈ t₀ ∨ s ≈ t₁`, with `s ≈ t₁` maximal and read as `s ↦ t₁`,
    /// gives `Γ → Δ ∨ t₀ ≉ t₁ ∨ s ≈ t₁`.
    #[allow(clippy::too_many_arguments)]
    fn factor<S: Sink>(
        &self,
        prem: &Side,
        l: Lit,
        s: RTerm,
        t1: RTerm,
        found: &mut Found,
        guard: &mut Guard<'_, S>,
        gbody: bool,
    ) -> bool {
        found.rule = "factor";
        for &o in &prem.1 {
            let XLit::Eq(a, b) = self.view(&guard.dl().lits, o) else {
                continue;
            };
            let t0 = if a == s {
                b
            } else if b == s {
                a
            } else {
                continue;
            };
            let is_ground = no_xy(XLit::Eq(s, t1)) && no_xy(XLit::Eq(s, t0));
            if is_ground && gbody {
                continue;
            }
            let Some(ne_made) = self.lit(guard, XLit::Ne(t0, t1)) else {
                return false;
            };
            let ne_lit = match ne_made {
                Made::Lit(n) => Some(n),
                Made::False => None,
                Made::True => continue,
            };
            if is_ground
                && super::dl_root::restricted_parts(
                    &guard.dl().lits,
                    self.inv(),
                    &prem.0,
                    &[],
                    &[l],
                    &prem.1,
                    Some(o),
                    ne_lit,
                )
            {
                continue;
            }
            let h_len =
                prem.1.iter().filter(|&&m| m != o).count() + 1 + usize::from(ne_lit.is_some());
            let Some(bytes) = side_exact_bytes(prem.0.len(), h_len) else {
                guard.speculative_overflow();
                return false;
            };
            if !found.push_admitted(
                bytes,
                || {
                    let mut h = Vec::with_capacity(h_len);
                    h.extend(prem.1.iter().copied().filter(|&m| m != o));
                    h.push(l);
                    if let Some(n) = ne_lit {
                        h.push(n);
                    }
                    (prem.0.clone(), h)
                },
                guard,
            ) {
                return false;
            }
        }
        true
    }

    /// The conclusions of rewriting with `prem` and borrowed `(part_body, part_head, without)`,
    /// one for each literal made: a false literal leaves the clause without it, and a true
    /// one makes no conclusion. The restriction on ground inferences applies when `ground`,
    /// and unit-only root-fact suppression applies when both premises are certified root units.
    #[allow(clippy::too_many_arguments)]
    fn push_parts<S: Sink>(
        &self,
        st: &State,
        found: &mut Found,
        prem: &Side,
        part_body: &[Lit],
        part_head: &[Lit],
        without: Option<Lit>,
        made: &[Made],
        ground: bool,
        fact: bool,
        guard: &mut Guard<'_, S>,
    ) -> bool {
        let given_fact = !self.root && fact && dl(st).is_some_and(|x| x.given_fact);
        for &m in made {
            let add = match m {
                Made::Lit(l) => Some(l),
                Made::False => None,
                Made::True => continue,
            };
            let lits = &guard.dl().lits;
            if ground
                && super::dl_root::restricted_parts(
                    lits,
                    self.inv(),
                    &prem.0,
                    part_body,
                    &prem.1,
                    part_head,
                    without,
                    add,
                )
            {
                continue;
            }
            if given_fact
                && super::dl_root::routable_unit_parts(
                    self.ix, st, lits, &prem.0, part_body, &prem.1, part_head, without, add,
                )
            {
                if super::dl_rules::tracing() {
                    super::dl_root::trace_shared_parts(
                        self.ix, st, lits, found.rule, &prem.0, part_body, &prem.1, part_head,
                        without, add,
                    );
                }
                continue;
            }
            let Some(bytes) = side_parts_bytes(prem, part_body, part_head, without, add) else {
                guard.speculative_overflow();
                return false;
            };
            if !found.push_admitted(
                bytes,
                || merge_parts(prem, part_body, part_head, without, add),
                guard,
            ) {
                return false;
            }
        }
        true
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_denied_or_bottom_atom_cannot_discard_an_equality_conclusion() {
        let mut st = State::default();
        let b = 12;
        let u = RTerm::C(0);
        let target = XLit::Atom(b, u);
        assert_eq!(Eqs::known_atom_constant(&st, target), None);
        st.subs.insert(b);
        assert_eq!(Eqs::known_atom_constant(&st, target), Some(u));
        super::super::dl_rules::ds(&mut st).neg.push(b);
        assert_eq!(Eqs::known_atom_constant(&st, target), None);
        st.subs.insert(BOTTOM);
        assert_eq!(Eqs::known_atom_constant(&st, XLit::Atom(BOTTOM, u)), None);
    }
}
