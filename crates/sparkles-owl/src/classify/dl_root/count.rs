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
    if s.dead || s.gbody {
        &[]
    } else {
        d.store.maxima(i)
    }
}

fn pivots(d: &DlSt, lit: Lit) -> impl Iterator<Item = Pivot> + '_ {
    d.store
        .by_xl
        .get(&lit)
        .into_iter()
        .flatten()
        .copied()
        .filter_map(move |i| {
            let s = &d.store.clauses[i as usize];
            (!s.dead && !s.gbody).then_some((i, lit))
        })
}

fn has_unit(d: &DlSt, l: Lit) -> bool {
    pivots(d, l).any(|(i, _)| {
        let c = &d.store.clauses[i as usize].c;
        c.body().is_empty() && c.head().len() == 1
    })
}

/// A unit equality in this context permits the same substitution in every head
/// alternative. A singleton in the input alone is insufficient evidence here.
fn reduced_value<S: Sink>(
    d: &DlSt,
    t: RTerm,
    inv: impl Fn(Role) -> Option<Role> + Copy,
    out: &mut Guard<'_, S>,
) -> Option<RTerm> {
    if !matches!(t, RTerm::Fc(..)) {
        return Some(t);
    }
    let ids = d.store.terms.as_deref().and_then(|by| by.eqs.get(&t));
    let mut value = None;
    for (j, &i) in ids.into_iter().flatten().enumerate() {
        if !scanning(j, out) {
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

/// Charge long index scans even if they do not produce an inference.
fn scanning<S: Sink>(offset: usize, out: &mut S) -> bool {
    const BLOCK: usize = 256;
    if (offset + 1).is_multiple_of(BLOCK) {
        out.worked(BLOCK / WORK_READS);
        out.go_on()
    } else {
        !out.stopped()
    }
}

pub(super) fn on_max<S: Sink>(
    ix: &Index,
    st: &State,
    v: XLit,
    prem: &Side,
    found: &mut Found,
    out: &mut Guard<'_, S>,
) -> bool {
    let Some(d) = dl(st) else { return true };
    let dx = dix(ix);
    let inv = inv_of(ix, dx);
    if let XLit::Atom(a, t) = v {
        if !on_filler(ix, st, a, t, prem, found, out) {
            return false;
        }
    } else if !matches!(v, XLit::Role(..)) {
        return true;
    }
    for (u, s, t) in directions(v, inv) {
        for (k, &(r, b)) in dx.quals.iter().enumerate() {
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
            if implicit {
                if !qualify(ix, st, (u, q, t), prem, None, found, out) {
                    return false;
                }
            } else if ground(t) {
                for p in pivots(d, a) {
                    if !qualify(ix, st, (u, q, t), prem, Some(p), found, out) {
                        return false;
                    }
                }
            } else {
                for (j, &i) in with_term(d, t).iter().enumerate() {
                    if !scanning(j, out) {
                        return false;
                    }
                    if maxima(d, i).contains(&a)
                        && !qualify(ix, st, (u, q, t), prem, Some((i, a)), found, out)
                    {
                        return false;
                    }
                }
            }
        }
    }
    for (g, n, q, extra) in dx
        .atmosts
        .iter()
        .map(|&(a, n, q)| (Some(a), n, q, &[][..]))
        .chain(dx.lazy.iter().map(|(g, n, q, h)| (*g, *n, *q, h.as_ref())))
    {
        let rule = Maximum {
            guard: g,
            n,
            q,
            extra,
        };
        if let XLit::Atom(a, t) = v
            && ground(t)
            && Some(a) == g
        {
            if !rule.apply(ix, d, t, None, prem, found, out) {
                return false;
            }
            out.release_to(found.held_bytes());
        }
        for (u, s, t) in directions(v, inv) {
            if dx.sub(ix, s, q) {
                if !rule.apply(ix, d, u, Some(t), prem, found, out) {
                    return false;
                }
                out.release_to(found.held_bytes());
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
        let (r, _) = dx.quals[((q - dx.base) / 2) as usize];
        for (j, &i) in roles_at(d, t).iter().enumerate() {
            if !scanning(j, out) {
                return false;
            }
            for &l in maxima(d, i) {
                for (u, s, z) in directions(out.dl().lits.view(l, inv), inv) {
                    if z == t
                        && dx.sub(ix, s, r)
                        && !qualify(ix, st, (u, q, t), prem, Some((i, l)), found, out)
                    {
                        return false;
                    }
                }
            }
        }
    }
    true
}

fn qualify<S: Sink>(
    ix: &Index,
    st: &State,
    (u, q, t): (RTerm, Role, RTerm),
    prem: &Side,
    partner: Option<Pivot>,
    found: &mut Found,
    out: &mut Guard<'_, S>,
) -> bool {
    let Some(Made::Lit(add)) = lit_admitted(ix, out, XLit::Role(q, u, t)) else {
        return false;
    };
    let d = dl(st).expect("a counting premise has a clause store");
    if has_unit(d, add) {
        return true;
    }
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
        ground(t),
        fact,
        prem,
        body,
        head,
        without,
        Some(add),
    )
}

struct Maximum<'a> {
    guard: Option<Ex>,
    n: u32,
    q: Role,
    extra: &'a [Ex],
}

impl Maximum<'_> {
    /// With a new holder the product chooses n+1 edges. With a new edge its
    /// neighbour and premise are fixed, so choose only n others and one holder.
    #[allow(clippy::too_many_arguments)]
    fn apply<S: Sink>(
        &self,
        ix: &Index,
        d: &DlSt,
        u: RTerm,
        fixed: Option<RTerm>,
        prem: &Side,
        found: &mut Found,
        out: &mut Guard<'_, S>,
    ) -> bool {
        let dx = dix(ix);
        let inv = inv_of(ix, dx);
        let mut holder = None;
        if fixed.is_some()
            && let Some(a) = self.guard
        {
            let Some(l) = out.dl().lits.get(XLit::Atom(a, u), inv) else {
                return true;
            };
            if pivots(d, l).next().is_none() {
                return true;
            }
            holder = Some(l);
        }
        let ids = roles_at(d, u);
        let mut refs_count = 0usize;
        // Count the exact number of borrowed edge references before allocating.
        let scan = |out: &mut Guard<'_, S>, visit: &mut dyn FnMut(RTerm, Pivot)| {
            for (j, &i) in ids.iter().enumerate() {
                if !scanning(j, out) {
                    return false;
                }
                for &l in maxima(d, i) {
                    for (w, s, t) in directions(out.dl().lits.view(l, inv), inv) {
                        if w == u && Some(t) != fixed && dx.sub(ix, s, self.q) {
                            visit(t, (i, l));
                        }
                    }
                }
            }
            true
        };
        if !scan(out, &mut |_, _| refs_count += 1) {
            return false;
        }
        let k = (self.n as usize).saturating_add(usize::from(fixed.is_none()));
        if refs_count < k {
            return true;
        }
        // Sorted references and the start of each group live through the products.
        let holder_count = holder.map_or(0, |l| pivots(d, l).count());
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
        if !out.ensure_admitted(found.held_bytes().saturating_add(scratch)) {
            return false;
        }
        let mut refs = Vec::with_capacity(refs_count + holder_count);
        if !scan(out, &mut |t, p| refs.push((t, p))) {
            return false;
        }
        refs.sort_unstable();
        refs.dedup();
        let mut starts = Vec::with_capacity(refs_count + 1);
        for (i, (t, _)) in refs.iter().enumerate() {
            if i == 0 || refs[i - 1].0 != *t {
                starts.push(i);
            }
        }
        let groups = starts.len();
        starts.push(refs.len());
        let edge_count = refs.len();
        if let Some(l) = holder {
            refs.extend(pivots(d, l).map(|p| (u, p)));
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
            if out.stopped() {
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
                d,
                u,
                &terms[..width],
                &lists[..k + usize::from(!holders.is_empty())],
                prem,
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
        d: &DlSt,
        u: RTerm,
        terms: &[RTerm],
        lists: &[&[(RTerm, Pivot)]],
        prem: &Side,
        scratch: usize,
        found: &mut Found,
        out: &mut Guard<'_, S>,
    ) -> bool {
        let inv = inv_of(ix, dix(ix));
        let mut reduced = [RTerm::X; AT_MOST_WIDEST];
        let terms = if terms.len() > 2 {
            for (j, &term) in terms.iter().enumerate() {
                let Some(t) = reduced_value(d, term, inv, out) else {
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
        if pairs().any(|v| out.dl().lits.get(v, inv).is_some_and(|l| has_unit(d, l))) {
            return true;
        }
        // D1 delivers a unit equality from the root when every premise has an
        // unconditional root proof and the context registered for its constants.
        if terms.len() == 2
            && self.extra.is_empty()
            && d.given_fact
            && prem.0.is_empty()
            && prem.1.is_empty()
            && terms.iter().all(|t| match *t {
                RTerm::C(c) | RTerm::Fc(_, c) => {
                    d.root.as_deref().is_some_and(|r| r.edged.contains(&c))
                }
                _ => false,
            })
            && lists.iter().all(|list| {
                list.iter()
                    .any(|&(_, (i, _))| d.store.clauses[i as usize].fact)
            })
        {
            return true;
        }
        let head_len = self.extra.len() + terms.len() * (terms.len() - 1) / 2;
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
        for list in lists {
            longest = longest.max(list.len());
            for &(_, (i, l)) in *list {
                bytes = bytes.and_then(|b| {
                    b.checked_add(size_of::<Side>())?
                        .checked_add(side_bytes(&d.store.clauses[i as usize].c, l)?)
                });
            }
        }
        let Some(bytes) = bytes else {
            out.speculative_overflow();
            return false;
        };
        let pruning = if longest > 1 {
            longest.saturating_mul(PRUNE_SCRATCH_ELEM)
        } else {
            0
        };
        if !found.reserve_prod_admitted(bytes, pruning, scratch, out) {
            return false;
        }
        let mut head = Vec::with_capacity(prem.1.len() + head_len);
        head.extend_from_slice(&prem.1);
        for v in self.extra.iter().map(|&a| XLit::Atom(a, u)).chain(pairs()) {
            let Some(Made::Lit(l)) = lit_admitted(ix, out, v) else {
                return false;
            };
            head.push(l);
        }
        let product = lists
            .iter()
            .map(|list| {
                list.iter()
                    .map(|&(_, (i, l))| side(&d.store.clauses[i as usize].c, l))
                    .collect()
            })
            .collect();
        let base = (prem.0.clone(), head);
        if terms.iter().copied().all(ground) {
            found.ground_products(product, base);
        } else {
            found.products(product, base);
        }
        out.release_to(found.held_bytes() + scratch);
        true
    }
}
