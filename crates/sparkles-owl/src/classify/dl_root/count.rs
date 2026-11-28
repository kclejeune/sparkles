//! Ground instances of the qualified-role and at-most clauses. Each premise is a
//! maximal literal in this context. Its body and its other head literals travel
//! together into the result, including when a query assumption conditions a merge.

use super::super::dl_rules::{
    AT_MOST_PAYLOAD, AT_MOST_SUBSETS, AT_MOST_WIDEST, choose, side_bytes,
};
use super::*;
use std::mem::size_of;
use std::sync::OnceLock;

pub(in crate::classify) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SPARKLES_ROOTED_ATMOST").is_none_or(|v| v != "0"))
}

fn ground(t: RTerm) -> bool {
    matches!(t, RTerm::C(_) | RTerm::Fc(..))
}

/// One direction of a role premise for each ground holder. Canonical storage can
/// give either direction and a loop can take two different roles.
fn directions(
    v: XLit,
    inv: impl Fn(Role) -> Option<Role>,
) -> impl Iterator<Item = (RTerm, Role, RTerm)> {
    let mut edges = [None; 2];
    if let XLit::Role(s, a, b) = v {
        if ground(a) {
            edges[0] = Some((a, s, b));
        }
        if ground(b)
            && let Some(i) = inv(s)
            && (a != b || i != s)
        {
            edges[1] = Some((b, i, a));
        }
    }
    edges.into_iter().flatten()
}

type Pivot = (u32, Lit);

fn with_term(d: &DlSt, t: RTerm) -> &[u32] {
    d.store
        .terms
        .as_deref()
        .and_then(|ts| ts.any.get(&t))
        .map_or(&[], Vec::as_slice)
}

fn roles_at(d: &DlSt, t: RTerm) -> &[u32] {
    if let RTerm::C(u) = t {
        d.root
            .as_deref()
            .and_then(|r| r.roles_of.get(&u))
            .map_or(&[], Vec::as_slice)
    } else {
        with_term(d, t)
    }
}

/// The term index also lists literals mentioning the constant inside f(u). Test
/// the maximal literal itself in each caller before treating it as a premise.
fn maxima(d: &DlSt, i: u32) -> &[Lit] {
    let s = &d.store.clauses[i as usize];
    if s.dead { &[] } else { d.store.maxima(i) }
}

fn pivots(d: &DlSt, lit: Lit) -> &[u32] {
    d.store.by_xl.get(&lit).map_or(&[], Vec::as_slice)
}

fn has_unit<S: Sink>(d: &DlSt, l: Lit, found: &mut Found, out: &mut S) -> Option<bool> {
    for &i in pivots(d, l) {
        if !scanning(found, out) {
            return None;
        }
        let s = &d.store.clauses[i as usize];
        if !s.dead && s.c.body().is_empty() && s.c.head().len() == 1 {
            return Some(true);
        }
    }
    Some(false)
}

/// A unit equality in this context permits the same substitution in every head
/// alternative. A singleton in the input alone is insufficient evidence here.
fn reduced_value<S: Sink>(
    d: &DlSt,
    t: RTerm,
    inv: impl Fn(Role) -> Option<Role> + Copy,
    found: &mut Found,
    out: &mut Guard<'_, S>,
) -> Option<RTerm> {
    if !matches!(t, RTerm::Fc(..)) {
        return Some(t);
    }
    let ids = d.store.terms.as_deref().and_then(|by| by.eqs.get(&t));
    let mut value = None;
    for &i in ids.into_iter().flatten() {
        if !scanning(found, out) {
            return None;
        }
        let s = &d.store.clauses[i as usize];
        if s.dead || !s.c.body().is_empty() || s.c.head().len() != 1 {
            continue;
        }
        if let XLit::Eq(a, b) = out.dl().lits.view(s.c.head()[0], inv) {
            let x = if a == t { b } else { a };
            if let RTerm::C(c) = x {
                value = Some(value.map_or(c, |known: u32| known.min(c)));
            }
        }
    }
    Some(value.map_or(t, RTerm::C))
}

/// Prepay a block on its first read. All scans for this given share the unused
/// reads, including partner lookups and successive subsets. The last partial
/// block is therefore charged even if the rule exits early.
fn scanning<S: Sink>(found: &mut Found, out: &mut S) -> bool {
    if out.stopped() {
        return false;
    }
    if found.count_reads_left == 0 {
        out.worked(1);
        found.count_reads_left = WORK_READS as u8;
        if !out.go_on() {
            return false;
        }
    }
    found.count_reads_left -= 1;
    true
}

#[derive(Clone, Copy)]
struct Given<'a> {
    side: &'a Side,
    gbody: bool,
}

pub(super) fn on_max<S: Sink>(
    ix: &Index,
    st: &State,
    v: XLit,
    prem: &Side,
    gbody: bool,
    found: &mut Found,
    out: &mut Guard<'_, S>,
) -> bool {
    let Some(d) = dl(st) else { return true };
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    if found.over {
        return true;
    }
    if let XLit::Atom(a, t) = v {
        if !(gbody && ground(t)) && !on_filler(ix, st, a, t, prem, found, out) {
            return false;
        }
    } else if !matches!(v, XLit::Role(..)) {
        return true;
    }
    for (u, s, t) in directions(v, inv) {
        if ground(t) && gbody {
            continue;
        }
        for (k, &(r, b)) in dx.quals.iter().enumerate() {
            if !scanning(found, out) {
                return false;
            }
            if !dx.sub(ix, s, r) {
                continue;
            }
            let Some(a) = out.dl().lits.get(XLit::Atom(b, t), inv) else {
                continue;
            };
            let q = dx.base + 2 * k as Role;
            let implicit = match t {
                RTerm::X => st.subs.contains(&b),
                RTerm::Fx(e) => st.subs.contains(&e) && dx.core(e).contains(&b),
                _ => false,
            };
            let Some(mut edge) = QualifiedEdge::lookup(ix, d, XLit::Role(q, u, t), found, out)
            else {
                return false;
            };
            if edge.held {
                continue;
            }
            if implicit {
                if !edge.qualify(ix, st, prem, None, found, out) {
                    return false;
                }
            } else if ground(t) {
                for &i in pivots(d, a) {
                    if !scanning(found, out) {
                        return false;
                    }
                    let s = &d.store.clauses[i as usize];
                    if s.dead || s.gbody {
                        continue;
                    }
                    let p = (i, a);
                    if !edge.qualify(ix, st, prem, Some(p), found, out) {
                        return false;
                    }
                }
            } else {
                for &i in with_term(d, t) {
                    if !scanning(found, out) {
                        return false;
                    }
                    for &l in maxima(d, i) {
                        if !scanning(found, out) {
                            return false;
                        }
                        if l == a {
                            if !edge.qualify(ix, st, prem, Some((i, a)), found, out) {
                                return false;
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
    let given = Given { side: prem, gbody };
    for &(guard, n, q) in &dx.atmosts {
        if !scanning(found, out) {
            return false;
        }
        let rule = Maximum { guard, n, q };
        if let XLit::Atom(a, t) = v
            && ground(t)
            && a == guard
        {
            if !rule.apply(ix, st, t, None, given, found, out) {
                return false;
            }
            out.release_to(found.held_bytes());
            if found.over {
                return true;
            }
        }
        for (u, s, t) in directions(v, inv) {
            if dx.sub(ix, s, q) {
                if !rule.apply(ix, st, u, Some(t), given, found, out) {
                    return false;
                }
                out.release_to(found.held_bytes());
                if found.over {
                    return true;
                }
            }
        }
    }
    true
}

pub(in crate::classify) fn on_filler<S: Sink>(
    ix: &Index,
    st: &State,
    a: Ex,
    t: RTerm,
    prem: &Side,
    found: &mut Found,
    out: &mut Guard<'_, S>,
) -> bool {
    let dx = dix(ix);
    let Some(qs) = dx.quals_of.get(&a) else {
        return true;
    };
    let Some(d) = dl(st) else { return true };
    let inv = inv_of(ix, dx);
    for &q in qs {
        if !scanning(found, out) {
            return false;
        }
        // t and q are fixed, but the role scan discovers the holder u. Reuse
        // its unit check when several premises give the same qualified edge.
        let mut previous: Option<QualifiedEdge> = None;
        let (r, _) = dx.quals[((q - dx.base) / 2) as usize];
        for &i in roles_at(d, t) {
            if !scanning(found, out) {
                return false;
            }
            if ground(t) && d.store.clauses[i as usize].gbody {
                continue;
            }
            for &l in maxima(d, i) {
                if !scanning(found, out) {
                    return false;
                }
                for (u, s, z) in directions(out.dl().lits.view(l, inv), inv) {
                    if z != t || !dx.sub(ix, s, r) {
                        continue;
                    }
                    let v = XLit::Role(q, u, t);
                    let edge = match &mut previous {
                        Some(edge) if edge.v == v => edge,
                        slot => {
                            let Some(edge) = QualifiedEdge::lookup(ix, d, v, found, out) else {
                                return false;
                            };
                            slot.insert(edge)
                        }
                    };
                    if !edge.held && !edge.qualify(ix, st, prem, Some((i, l)), found, out) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

struct QualifiedEdge {
    v: XLit,
    add: Option<Lit>,
    held: bool,
}

impl QualifiedEdge {
    fn lookup<S: Sink>(
        ix: &Index,
        d: &DlSt,
        v: XLit,
        found: &mut Found,
        out: &mut S,
    ) -> Option<Self> {
        let add = out.dl().lits.get(v, inv_of(ix, dix(ix)));
        let held = match add {
            Some(l) => has_unit(d, l, found, out)?,
            None => false,
        };
        Some(Self { v, add, held })
    }

    fn qualify<S: Sink>(
        &mut self,
        ix: &Index,
        st: &State,
        prem: &Side,
        partner: Option<Pivot>,
        found: &mut Found,
        out: &mut Guard<'_, S>,
    ) -> bool {
        let add = match self.add {
            Some(l) => l,
            None => {
                let Some(Made::Lit(l)) = lit_admitted(ix, out, self.v) else {
                    return false;
                };
                self.add = Some(l);
                l
            }
        };
        let d = dl(st).expect("a counting premise has a clause store");
        let (body, head, without, fact) = match partner {
            Some((i, l)) => {
                let s = &d.store.clauses[i as usize];
                (s.c.body(), s.c.head(), Some(l), s.fact)
            }
            None => (&[][..], &[][..], None, false),
        };
        found.rule = "qual-at";
        put_with_fact_parts_admitted(
            ix,
            st,
            out,
            found,
            no_xy(self.v),
            fact,
            prem,
            body,
            head,
            without,
            Some(add),
        )
    }
}

struct Maximum {
    guard: Ex,
    n: u32,
    q: Role,
}

impl Maximum {
    /// With a new holder the product chooses n+1 edges. With a new edge its
    /// neighbour and premise are fixed, so choose only n others and one holder.
    #[allow(clippy::too_many_arguments)]
    fn apply<S: Sink>(
        &self,
        ix: &Index,
        st: &State,
        u: RTerm,
        fixed: Option<RTerm>,
        given: Given,
        found: &mut Found,
        out: &mut Guard<'_, S>,
    ) -> bool {
        let dx = dix(ix);
        let inv = inv_of(ix, dx);
        let d = dl(st).expect("a counting premise has a clause store");
        let holder = if fixed.is_some() {
            let Some(l) = out.dl().lits.get(XLit::Atom(self.guard, u), inv) else {
                return true;
            };
            if pivots(d, l).is_empty() {
                return true;
            }
            Some(l)
        } else {
            None
        };
        let ids = roles_at(d, u);
        let k = (self.n as usize).saturating_add(usize::from(fixed.is_none()));
        let mut refs_count = 0usize;
        let mut gbody_count = 0;
        let mut local = fixed.is_some_and(|t| !ground(t));
        // Count the exact number of borrowed edge references before allocating.
        let scan =
            |found: &mut Found, out: &mut Guard<'_, S>, visit: &mut dyn FnMut(RTerm, Pivot)| {
                for &i in ids {
                    if !scanning(found, out) {
                        return false;
                    }
                    for &l in maxima(d, i) {
                        if !scanning(found, out) {
                            return false;
                        }
                        for (w, s, t) in directions(out.dl().lits.view(l, inv), inv) {
                            if w == u && Some(t) != fixed && dx.sub(ix, s, self.q) {
                                visit(t, (i, l));
                            }
                        }
                    }
                }
                true
            };
        if !scan(found, out, &mut |t, (i, _)| {
            refs_count += 1;
            gbody_count += usize::from(d.store.clauses[i as usize].gbody);
            local |= k != 0 && !ground(t);
        }) {
            return false;
        }
        // When every possible subset is ground, discard cut premises before
        // applying any memory or subset bound. Mixed pools need the per-subset cut.
        if !local {
            if given.gbody {
                return true;
            }
            refs_count -= gbody_count;
        }
        let eligible = |i: u32| {
            let s = &d.store.clauses[i as usize];
            !s.dead && (local || !s.gbody)
        };
        if refs_count < k {
            return true;
        }
        // Sorted references and the start of each group live through the products.
        let mut holder_count = 0;
        if let Some(l) = holder {
            for &i in pivots(d, l) {
                if !scanning(found, out) {
                    return false;
                }
                holder_count += usize::from(eligible(i));
            }
            if holder_count == 0 {
                return true;
            }
        }
        let total = refs_count.checked_add(holder_count);
        let Some(scratch) = total
            .and_then(|n| n.checked_mul(size_of::<(RTerm, Pivot)>()))
            .and_then(|b| {
                b.checked_add(refs_count.checked_add(1)?.checked_mul(size_of::<usize>())?)
            })
        else {
            out.speculative_overflow();
            return false;
        };
        let Some(need) = found.held_bytes().checked_add(scratch) else {
            out.speculative_overflow();
            return false;
        };
        if !out.ensure_admitted(need) {
            return false;
        }
        let mut refs = Vec::with_capacity(refs_count + holder_count);
        if !scan(found, out, &mut |t, p| {
            if eligible(p.0) {
                refs.push((t, p));
            }
        }) {
            return false;
        }
        refs.sort_unstable();
        refs.dedup();
        let mut starts = Vec::with_capacity(refs_count + 1);
        for (i, (t, _)) in refs.iter().enumerate() {
            if !scanning(found, out) {
                return false;
            }
            if i == 0 || refs[i - 1].0 != *t {
                starts.push(i);
            }
        }
        let groups = starts.len();
        starts.push(refs.len());
        let edge_count = refs.len();
        if let Some(l) = holder {
            for &i in pivots(d, l) {
                if !scanning(found, out) {
                    return false;
                }
                if eligible(i) {
                    refs.push((u, (i, l)));
                }
            }
        }
        let (refs, holders) = refs.split_at(edge_count);
        if groups < k {
            return true;
        }
        let width = (self.n as usize).saturating_add(1);
        let subsets = choose(groups, k);
        if width > AT_MOST_WIDEST
            || subsets > AT_MOST_SUBSETS
            || subsets.saturating_mul(width * (width - 1) / 2 + width + 1) > AT_MOST_PAYLOAD
        {
            found.over = true;
            return true;
        }
        let mut picks = [0usize; AT_MOST_WIDEST];
        let picks = &mut picks[..k];
        for (i, p) in picks.iter_mut().enumerate() {
            *p = i
        }
        found.rule = "atmost-at";
        loop {
            if !scanning(found, out) {
                return false;
            }
            let mut terms = [RTerm::X; AT_MOST_WIDEST];
            let mut lists = [&[][..]; AT_MOST_WIDEST];
            for (i, &p) in picks.iter().enumerate() {
                lists[i] = &refs[starts[p]..starts[p + 1]];
                terms[i] = lists[i][0].0;
            }
            if let Some(t) = fixed {
                terms[k] = t;
            }
            if !holders.is_empty() {
                lists[k] = holders;
            }
            if !self.product(
                ix,
                st,
                &terms[..width],
                &lists[..k + usize::from(!holders.is_empty())],
                given,
                scratch,
                found,
                out,
            ) {
                return false;
            }
            let Some(j) = (0..k).rev().find(|&j| picks[j] < groups - k + j) else {
                break;
            };
            picks[j] += 1;
            for i in j + 1..k {
                picks[i] = picks[i - 1] + 1;
            }
        }
        true
    }

    /// One representative of each selected term supplies a role premise. When
    /// the given clause is a role, the final list supplies the holder if needed.
    #[allow(clippy::too_many_arguments)]
    fn product<S: Sink>(
        &self,
        ix: &Index,
        st: &State,
        terms: &[RTerm],
        lists: &[&[(RTerm, Pivot)]],
        given: Given,
        scratch: usize,
        found: &mut Found,
        out: &mut Guard<'_, S>,
    ) -> bool {
        let inv = inv_of(ix, dix(ix));
        let d = dl(st).expect("a counting premise has a clause store");
        let pure = terms.iter().copied().all(ground);
        if pure && given.gbody {
            return true;
        }
        let prem = given.side;
        let eligible = |i: u32| !pure || !d.store.clauses[i as usize].gbody;
        let mut reduced = [RTerm::X; AT_MOST_WIDEST];
        let terms = if terms.len() > 2 {
            for (j, &term) in terms.iter().enumerate() {
                let Some(t) = reduced_value(d, term, inv, found, out) else {
                    return false;
                };
                if reduced[..j].contains(&t) {
                    return true;
                }
                reduced[j] = t;
            }
            &reduced[..terms.len()]
        } else {
            terms
        };
        let pairs = || {
            terms
                .iter()
                .enumerate()
                .flat_map(|(j, &a)| terms[..j].iter().map(move |&b| XLit::Eq(a, b)))
        };
        // A held unit equality satisfies the whole disjunctive conclusion.
        for v in pairs() {
            if !scanning(found, out) {
                return false;
            }
            if let Some(l) = out.dl().lits.get(v, inv) {
                match has_unit(d, l, found, out) {
                    Some(true) => return true,
                    Some(false) => {}
                    None => return false,
                }
            }
        }
        let head_len = terms.len() * (terms.len() - 1) / 2;
        let mut bytes = lists
            .len()
            .checked_mul(size_of::<Vec<Side>>())
            .and_then(|b| {
                b.checked_add(side_exact_bytes(
                    prem.0.len(),
                    prem.1.len().checked_add(head_len)?,
                )?)
            });
        let mut longest = 0usize;
        let mut root_facts = pure && terms.len() == 2 && d.given_fact;
        for list in lists {
            bytes = bytes.and_then(|b| b.checked_add(list.len().checked_mul(size_of::<Side>())?));
            let mut count = 0;
            let mut fact = false;
            for &(_, (i, l)) in *list {
                if !scanning(found, out) {
                    return false;
                }
                if !eligible(i) {
                    continue;
                }
                count += 1;
                fact |= d.store.clauses[i as usize].fact;
                bytes = bytes
                    .and_then(|b| b.checked_add(side_bytes(&d.store.clauses[i as usize].c, l)?));
            }
            if count == 0 {
                return true;
            }
            longest = longest.max(count);
            root_facts &= fact;
        }
        if root_facts {
            let Some(Made::Lit(l)) = lit_admitted(ix, out, XLit::Eq(terms[0], terms[1])) else {
                return false;
            };
            let lits = &out.dl().lits;
            if routable_unit_parts(ix, st, lits, &prem.0, &[], &prem.1, &[], None, Some(l)) {
                if super::super::dl_rules::tracing() {
                    trace_shared_parts(
                        ix,
                        st,
                        lits,
                        found.rule,
                        &prem.0,
                        &[],
                        &prem.1,
                        &[],
                        None,
                        Some(l),
                    );
                }
                return true;
            }
        }
        let Some(bytes) = bytes else {
            out.speculative_overflow();
            return false;
        };
        let pruning = if longest > 1 {
            let Some(pruning) = longest.checked_mul(PRUNE_SCRATCH_ELEM) else {
                out.speculative_overflow();
                return false;
            };
            pruning
        } else {
            0
        };
        if !found.reserve_prod_admitted(bytes, pruning, scratch, out) {
            return false;
        }
        let mut head = Vec::with_capacity(prem.1.len() + head_len);
        head.extend_from_slice(&prem.1);
        for v in pairs() {
            let Some(Made::Lit(l)) = lit_admitted(ix, out, v) else {
                return false;
            };
            head.push(l);
        }
        let mut product = Vec::with_capacity(lists.len());
        for list in lists {
            let mut sides = Vec::with_capacity(list.len());
            for &(_, (i, l)) in *list {
                if !scanning(found, out) {
                    return false;
                }
                if eligible(i) {
                    sides.push(side(&d.store.clauses[i as usize].c, l));
                }
            }
            product.push(sides);
        }
        let base = (prem.0.clone(), head);
        if pure {
            found.ground_products(product, base);
        } else {
            found.products(product, base);
        }
        out.release_to(found.held_bytes() + scratch);
        true
    }
}
