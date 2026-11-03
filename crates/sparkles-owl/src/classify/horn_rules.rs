//! The rules of Tier H (roadmap item 603): Kazakov's consequence-driven calculus for
//! Horn-SHIQ (*Consequence-Driven Reasoning for Horn SHIQ Ontologies*, IJCAI 2009, Table
//! 3) over the saturation's contexts, with self restrictions, role disjointness and
//! asymmetry beside it.
//!
//! A context's root stands for the conjunction M of the paper. I1, I2 and R1 are the EL
//! rules of initialization, told subsumers and conjunctions, R2 is the propagation of ⊥
//! over backward links, and R4 is the propagation of negative existentials over backward
//! links, which `∀S.A` joins as the pair `(S⁻, A)`. The rules here are the rest.
//!
//! * R3: a context with `∀S.A` and a successor N by a role under S links to the successor
//!   whose conjunction is N's with the fillers of every universal on a superrole of the
//!   role, which the saturation creates when the index has no context for it.
//! * R5: a context with `≤1 S.B` and two successors by roles under S that hold B links by
//!   each role to the successor of the two conjunctions together. A successor tells its
//!   predecessors the at-most fillers it holds (`SuccIn`).
//! * R6: a context N with `≤1 S.B`, a predecessor M by a role under S⁻ that holds B and a
//!   successor by a role R under S that holds B: the successor is M's instance, so M
//!   takes the successor's conjunction and a link by R⁻ to N. A predecessor tells its
//!   successors the at-most fillers it holds (`PredIn`).
//!
//! A loop by S, from `ObjectHasSelf` or a reflexive role, is a successor that is the
//! instance itself, and the rules treat it so. A loop by S is one by S⁻ too. Two roles
//! that a merge or a loop puts between one pair of elements are checked against the
//! disjoint and asymmetric roles.
//!
//! Every rule reads only its own context's state and sends conclusions to itself or to
//! the contexts it links to, as section 2 of the lane plan requires: a backward link is
//! used only to send a conclusion to its source, and two links into one context are never
//! composed.

use super::horn::HornIx;
use super::index::{BOTTOM, Ex, Index, Role, TOP};
use super::saturate::{Concl, Sink, Small, State};
use rustc_hash::FxHashMap;

/// What a context keeps for the Horn rules.
#[derive(Default, Clone)]
pub struct HornSt {
    /// the successors, by role and context
    pub succ: Small<(Role, Ex)>,
    /// the universals `∀S.A` among the subsumers
    pub alls: Vec<(Role, Ex)>,
    /// the roles of the universals, each once
    pub all_roles: Vec<Role>,
    /// the at-most restrictions `≤1 S.B` among the subsumers
    pub atmost: Vec<(Role, Ex)>,
    /// `(N, B)`: the successor N holds the at-most filler B
    pub succ_in: Small<(Ex, Ex)>,
    /// `(M, B)`: the predecessor M holds the at-most filler B
    pub pred_in: Small<(Ex, Ex)>,
    /// the at-most fillers among the subsumers, `owl:Thing` aside
    pub fills: Vec<Ex>,
    /// the successors that R3 has still to extend, which [`flush`] does at the end of
    /// the context's activation
    pub pending: Vec<(Role, Ex)>,
    /// whether a universal came since the last [`flush`], which then extends every
    /// successor
    pub dirty: bool,
    /// the at-most restrictions whose successors R5 has still to merge, which [`flush`]
    /// does at the end of the context's activation
    pub merges: Vec<(Role, Ex)>,
}

impl HornSt {
    pub fn bytes(&self) -> usize {
        self.succ.bytes()
            + self.alls.capacity() * 8
            + self.all_roles.capacity() * 4
            + self.atmost.capacity() * 8
            + self.succ_in.bytes()
            + self.pred_in.bytes()
            + self.fills.capacity() * 4
            + self.pending.capacity() * 8
            + self.merges.capacity() * 8
    }
}

/// A successor that an at-most restriction counts: a link, or a loop, which makes the
/// instance its own successor.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Member {
    Link(Role, Ex),
    Loop(Role),
}

fn hs(st: &mut State) -> &mut HornSt {
    &mut st.extra().horn
}

fn held_succ(st: &State, n: Ex, b: Ex) -> bool {
    b == TOP
        || st
            .extra
            .as_ref()
            .is_some_and(|x| x.horn.succ_in.contains(&(n, b)))
}

fn held_pred(st: &State, m: Ex, b: Ex) -> bool {
    b == TOP
        || st
            .extra
            .as_ref()
            .is_some_and(|x| x.horn.pred_in.contains(&(m, b)))
}

fn held_own(st: &State, b: Ex) -> bool {
    b == TOP || st.subs.contains(&b)
}

/// The successors that `≤1 S.B` counts.
fn members(ix: &Index, st: &State, s: Role, b: Ex) -> Vec<Member> {
    let roles = &ix.roles;
    let mut k: Vec<Member> = Vec::new();
    if let Some(x) = &st.extra {
        for &(r, n) in &x.horn.succ.list {
            if roles.sub(r, s) && held_succ(st, n, b) {
                k.push(Member::Link(r, n));
            }
        }
    }
    if held_own(st, b) {
        for &l in &st.loops {
            if roles.sub(l, s) {
                k.push(Member::Loop(l));
            }
        }
    }
    k
}

/// The predecessors that `≤1 S.B` counts: the sources of backward links by roles under
/// S⁻ that hold B, with the role.
fn preds(ix: &Index, st: &State, s: Role, b: Ex) -> Vec<(Ex, Role)> {
    let roles = &ix.roles;
    let mut out = Vec::new();
    for (r, srcs) in &st.back {
        if roles.inv(*r).is_some_and(|i| roles.sub(i, s)) {
            for &m in &srcs.list {
                if held_pred(st, m, b) {
                    out.push((m, *r));
                }
            }
        }
    }
    out
}

/// Sends a context the conjunction that `n` stands for.
fn send_core<S: Sink>(ix: &Index, to: Ex, n: Ex, out: &mut S) {
    let mut core = Vec::new();
    out.core(ix, n, &mut core);
    for m in core {
        out.send(to, Concl::Sub(m));
    }
}

/// A successor by `r` in `n`, which the context links to unless `sent` says the EL rule
/// of an existential restriction did.
pub fn succ<S: Sink>(ix: &Index, me: Ex, st: &mut State, r: Role, n: Ex, sent: bool, out: &mut S) {
    if !hs(st).succ.insert((r, n)) {
        return;
    }
    let roles = &ix.roles;
    if !sent {
        out.send(n, Concl::Back(me, r));
        if roles.keeps_forward(r) {
            out.me(Concl::Fwd(r, n));
        }
        if roles.is_empty_role(r) {
            out.me(Concl::Sub(BOTTOM));
        }
    }
    let h = hs(st);
    for &b in &h.fills {
        out.send(n, Concl::PredIn(me, b));
    }
    if !h.dirty && h.all_roles.iter().any(|&s| roles.sub(r, s)) {
        h.pending.push((r, n));
    }
    let atmost = hs(st).atmost.clone();
    for (s, b) in atmost {
        if roles.sub(r, s) && held_succ(st, n, b) {
            counted(ix, me, st, s, b, Member::Link(r, n), out);
        }
    }
}

/// R3: the successor by `r` in `n` with the fillers of the universals on the superroles
/// of `r`.
fn extend<S: Sink>(ix: &Index, fs: &[Ex], n: Ex, r: Role, out: &mut S) -> bool {
    if fs.is_empty() {
        return false;
    }
    let mut core = Vec::new();
    out.core(ix, n, &mut core);
    let before = core.clone();
    core.extend_from_slice(fs);
    core.retain(|&e| e != TOP);
    core.sort_unstable();
    core.dedup();
    if core == before {
        return false;
    }
    let m = if core.len() == 1 {
        core[0]
    } else {
        out.intern(&core)
    };
    out.me(Concl::Succ(r, m));
    true
}

/// R3 at the end of an activation, over the successors that came since the last call, or
/// over all of them when a universal came. Each successor is extended once with every
/// universal the context holds by then, so that universals that come one after another
/// create no context for each conjunction on the way. The answer is whether it sent a
/// conclusion to the context.
pub fn flush<S: Sink>(ix: &Index, st: &mut State, out: &mut S) -> bool {
    let Some(x) = st.extra.as_mut() else {
        return false;
    };
    let h = &mut x.horn;
    if !h.dirty && h.pending.is_empty() && h.merges.is_empty() {
        return false;
    }
    let mut sent = false;
    let mut merges = std::mem::take(&mut h.merges);
    merges.sort_unstable();
    merges.dedup();
    for (s, b) in merges {
        sent |= merge_all(ix, st, s, b, out);
    }
    let h = &mut st.extra.as_mut().expect("the context's Horn state").horn;
    let todo: Vec<(Role, Ex)> = if h.dirty {
        h.pending.clear();
        h.succ.list.to_vec()
    } else {
        std::mem::take(&mut h.pending)
    };
    h.dirty = false;
    // the fillers of the universals that apply to each role of the successors
    let roles = &ix.roles;
    let mut fillers: FxHashMap<Role, Vec<Ex>> = FxHashMap::default();
    for (r, n) in todo {
        let fs = fillers.entry(r).or_insert_with(|| {
            h.alls
                .iter()
                .filter(|&&(s, _)| roles.sub(r, s))
                .map(|&(_, a)| a)
                .collect()
        });
        sent |= extend(ix, fs, n, r, out);
    }
    sent
}

/// The context derives `∀s.a`: R4 to the predecessors, and R3 to the successors.
pub fn forall<S: Sink>(ix: &Index, _me: Ex, st: &mut State, s: Role, a: Ex, out: &mut S) {
    let roles = &ix.roles;
    if let Some(i) = roles.inv(s) {
        st.props.push((i, a));
        for (r, srcs) in &st.back {
            if roles.sub(*r, i) {
                for &e in &srcs.list {
                    out.send(e, Concl::Sub(a));
                }
            }
        }
    }
    // R3 passes over a filler that changes no successor
    if absorbed(ix, a) {
        return;
    }
    let h = hs(st);
    if h.alls.contains(&(s, a)) {
        return;
    }
    h.alls.push((s, a));
    if !h.all_roles.contains(&s) {
        h.all_roles.push(s);
    }
    if h.succ.list.iter().any(|&(r, _)| roles.sub(r, s)) {
        h.dirty = true;
    }
}

/// The context derives `≤1 s.b`: R5 over its successors and R6 over its predecessors.
pub fn at_most<S: Sink>(ix: &Index, me: Ex, st: &mut State, s: Role, b: Ex, out: &mut S) {
    let h = hs(st);
    if h.atmost.contains(&(s, b)) {
        return;
    }
    h.atmost.push((s, b));
    h.merges.push((s, b));
    let k = members(ix, st, s, b);
    for (m, r1) in preds(ix, st, s, b) {
        for &x in &k {
            identify(ix, me, st, m, r1, x, out);
        }
    }
}

/// A successor that `≤1 s.b` counts is new: R5 at the end of the activation, and R6
/// with every predecessor.
fn counted<S: Sink>(ix: &Index, me: Ex, st: &mut State, s: Role, b: Ex, x: Member, out: &mut S) {
    hs(st).merges.push((s, b));
    for (m, r1) in preds(ix, st, s, b) {
        identify(ix, me, st, m, r1, x, out);
    }
}

/// R5 at the end of an activation: every successor that `≤1 s.b` counts is one, so each
/// link goes to the conjunction of all of them at once. Merging them two at a time would
/// make a context for every subset on the way, since each merged successor is counted
/// again and merges with the rest. A loop makes each successor the instance itself. The
/// answer is whether it sent a conclusion to the context.
fn merge_all<S: Sink>(ix: &Index, st: &mut State, s: Role, b: Ex, out: &mut S) -> bool {
    let roles = &ix.roles;
    let k = members(ix, st, s, b);
    if k.len() < 2 {
        return false;
    }
    let looped = k.iter().any(|m| matches!(m, Member::Loop(_)));
    let links: Vec<(Role, Ex)> = k
        .iter()
        .filter_map(|m| match *m {
            Member::Link(r, n) => Some((r, n)),
            Member::Loop(_) => None,
        })
        .collect();
    if !roles.clash.is_empty() {
        for (i, &(r1, _)) in links.iter().enumerate() {
            for &(r2, _) in &links[..i] {
                if r1 != r2 && (roles.clashes(r1, r2) || roles.clashes(r2, r1)) {
                    out.me(Concl::Sub(BOTTOM));
                    return true;
                }
            }
        }
    }
    if looped {
        // each successor is the instance itself
        let mut core = Vec::new();
        for &(r, n) in &links {
            out.core(ix, n, &mut core);
            for &m in &core {
                out.me(Concl::Sub(m));
            }
            out.me(Concl::Loop(r));
        }
        return !links.is_empty();
    }
    if links.len() < 2 {
        return false;
    }
    let mut cores: Vec<Vec<Ex>> = Vec::with_capacity(links.len());
    let mut u: Vec<Ex> = Vec::new();
    for &(_, n) in &links {
        let mut c = Vec::new();
        out.core(ix, n, &mut c);
        u.extend_from_slice(&c);
        cores.push(c);
    }
    u.sort_unstable();
    u.dedup();
    // every member already holds the union, which may be empty when they hold only ⊤
    if cores.iter().all(|c| *c == u) {
        return false;
    }
    let m = if u.len() == 1 { u[0] } else { out.intern(&u) };
    let mut sent = false;
    for (&(r, _), c) in links.iter().zip(&cores) {
        if *c != u {
            out.me(Concl::Succ(r, m));
            sent = true;
        }
    }
    sent
}

/// R6: the predecessor `m`, by `r1`, and the successor `x` are one.
fn identify<S: Sink>(ix: &Index, me: Ex, st: &State, m: Ex, r1: Role, x: Member, out: &mut S) {
    let roles = &ix.roles;
    match x {
        Member::Link(r2, n2) => {
            if m == me && st.loops.contains(&r1) {
                // the predecessor is the instance itself, so the successor is too
                send_core(ix, me, n2, out);
                out.me(Concl::Loop(r2));
                return;
            }
            send_core(ix, m, n2, out);
            if let Some(i) = roles.inv(r2) {
                if roles.clashes(r1, i) || roles.clashes(i, r1) {
                    out.send(m, Concl::Sub(BOTTOM));
                    return;
                }
                out.send(m, Concl::Succ(i, me));
            }
        }
        Member::Loop(l) => {
            // the instance is its own successor, so it is the predecessor's instance,
            // which has the loops then, also when the predecessor's context is this one
            if m != me {
                send_core(ix, m, me, out);
            }
            out.send(m, Concl::Loop(l));
            out.send(m, Concl::Loop(r1));
        }
    }
}

/// The context derives `b`, an at-most filler: it tells its neighbours, and its loops
/// count as successors in b.
pub fn fill<S: Sink>(ix: &Index, me: Ex, st: &mut State, b: Ex, out: &mut S) {
    hs(st).fills.push(b);
    for (_, srcs) in &st.back {
        for &e in &srcs.list {
            out.send(e, Concl::SuccIn(me, b));
        }
    }
    for &(_, n) in &hs(st).succ.list {
        out.send(n, Concl::PredIn(me, b));
    }
    if st.loops.is_empty() {
        return;
    }
    let roles = &ix.roles;
    let atmost = hs(st).atmost.clone();
    let loops = st.loops.clone();
    for (s, b2) in atmost {
        if b2 != b {
            continue;
        }
        for &l in &loops {
            if roles.sub(l, s) {
                counted(ix, me, st, s, b, Member::Loop(l), out);
            }
        }
    }
}

/// A backward link from `e` by `r`: the source learns the fillers the context holds, and
/// R6 runs for the fillers the source holds, `owl:Thing` and those it told the context
/// of through an earlier link.
pub fn back<S: Sink>(ix: &Index, me: Ex, st: &mut State, e: Ex, r: Role, out: &mut S) {
    let Some(x) = &st.extra else { return };
    for &b in &x.horn.fills {
        out.send(e, Concl::SuccIn(me, b));
    }
    let roles = &ix.roles;
    let Some(i) = roles.inv(r) else { return };
    for &(s, b) in &x.horn.atmost {
        if roles.sub(i, s) && held_pred(st, e, b) {
            for y in members(ix, st, s, b) {
                identify(ix, me, st, e, r, y, out);
            }
        }
    }
}

/// `SuccIn(n, b)`: the successor n holds b.
pub fn succ_in<S: Sink>(ix: &Index, me: Ex, st: &mut State, n: Ex, b: Ex, out: &mut S) -> bool {
    if !hs(st).succ_in.insert((n, b)) {
        return false;
    }
    let roles = &ix.roles;
    let h = hs(st);
    let found: Vec<(Role, Role)> = h
        .atmost
        .iter()
        .filter(|&&(_, b2)| b2 == b)
        .flat_map(|&(s, _)| {
            h.succ
                .list
                .iter()
                .filter(move |&&(r, n2)| n2 == n && roles.sub(r, s))
                .map(move |&(r, _)| (s, r))
        })
        .collect();
    for (s, r) in found {
        counted(ix, me, st, s, b, Member::Link(r, n), out);
    }
    true
}

/// `PredIn(m, b)`: the predecessor m holds b.
pub fn pred_in<S: Sink>(ix: &Index, me: Ex, st: &mut State, m: Ex, b: Ex, out: &mut S) -> bool {
    if !hs(st).pred_in.insert((m, b)) {
        return false;
    }
    let roles = &ix.roles;
    let atmost: Vec<Role> = hs(st)
        .atmost
        .iter()
        .filter(|&&(_, b2)| b2 == b)
        .map(|&(s, _)| s)
        .collect();
    for s in atmost {
        let k = members(ix, st, s, b);
        for (r1, srcs) in &st.back {
            if srcs.contains(&m) && roles.inv(*r1).is_some_and(|i| roles.sub(i, s)) {
                for &x in &k {
                    identify(ix, me, st, m, *r1, x, out);
                }
            }
        }
    }
    true
}

/// A new loop by `r`: one by r⁻ too, a clash with a disjoint or asymmetric role, and a
/// successor that the at-most restrictions count.
pub fn self_loop<S: Sink>(ix: &Index, me: Ex, st: &mut State, r: Role, out: &mut S) {
    let roles = &ix.roles;
    if let Some(i) = roles.inv(r) {
        out.me(Concl::Loop(i));
    }
    if !roles.clash.is_empty()
        && st
            .loops
            .iter()
            .any(|&l| roles.clashes(l, r) || roles.clashes(r, l))
    {
        out.me(Concl::Sub(BOTTOM));
        return;
    }
    let Some(x) = &st.extra else { return };
    let atmost = x.horn.atmost.clone();
    for (s, b) in atmost {
        if roles.sub(r, s) && held_own(st, b) {
            counted(ix, me, st, s, b, Member::Loop(r), out);
        }
    }
}

/// Whether an expression adds nothing that every context does not hold, which
/// [`HornIx::ABSORBED`] describes.
#[inline]
fn absorbed(ix: &Index, a: Ex) -> bool {
    ix.horn.as_ref().is_some_and(|h| h.is(a, HornIx::ABSORBED))
}
