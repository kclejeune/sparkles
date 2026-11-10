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
    /// the context has at most one instance (F24, "The Horn rules with individuals")
    pub singular: bool,
    /// an at-most restriction of a singular successor counts the context, which makes it
    /// a witness when it is not real
    pub counted: bool,
    /// the predecessors that are real
    pub real_preds: Small<Ex>,
    /// the contexts that stand for the same element, which take what this one holds
    pub same: Small<Ex>,
    /// the predecessors told that this context is singular
    pub told_singular: Small<Ex>,
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
            + self.real_preds.bytes()
            + self.same.bytes()
            + self.told_singular.bytes()
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
/// of an existential restriction did. The answer says whether the successor was new.
pub fn succ<S: Sink>(
    ix: &Index,
    me: Ex,
    st: &mut State,
    r: Role,
    n: Ex,
    sent: bool,
    out: &mut S,
) -> bool {
    if !hs(st).succ.insert((r, n)) {
        return false;
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
    // a real context's successors are real (roadmap item 692); the EL rule sent `Real`
    // to a successor it created
    if st.real && !sent {
        out.send(n, Concl::Real);
        out.send(n, Concl::RealBack(me));
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
    true
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
    // a short list for the few roles of most contexts, and a map beyond them
    let mut few: Vec<(Role, Vec<Ex>)> = Vec::new();
    let mut many: FxHashMap<Role, Vec<Ex>> = FxHashMap::default();
    let of = |r: Role| -> Vec<Ex> {
        h.alls
            .iter()
            .filter(|&&(s, _)| roles.sub(r, s))
            .map(|&(_, a)| a)
            .collect()
    };
    for (r, n) in todo {
        let fs = if many.is_empty() {
            match few.iter().position(|(x, _)| *x == r) {
                Some(at) => &few[at].1,
                None if few.len() < 8 => {
                    few.push((r, of(r)));
                    &few[few.len() - 1].1
                }
                None => {
                    many.extend(few.drain(..));
                    many.entry(r).or_insert_with(|| of(r))
                }
            }
        } else {
            many.entry(r).or_insert_with(|| of(r))
        };
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

/// The context derives `≤1 s.b`: R5 over its successors and R6 over its predecessors,
/// and at a singular context the predecessors it counts learn so, and R7 runs.
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
    counted_full(ix, me, st, None, out);
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
    // the loop now counts the context itself among the neighbours, which R7 reads
    counted_full(ix, me, st, None, out);
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

// The rules of Tier H with individuals (roadmap item 692, F24 "The Horn rules with
// individuals"). A context is singular when it has at most one instance: when it derives
// an individual, or when an at-most restriction of a singular successor counts it. R7
// identifies two real predecessors that an at-most restriction of a singular context
// counts, and R8 derives `owl:Nothing` from two roles that clash on a pair of elements a
// singular context forces. Contexts that stand for one element exchange their subsumers,
// Horn successors, backward links with what they know of each predecessor, and loops.

/// Whether the context is singular.
fn is_singular(st: &State) -> bool {
    st.extra.as_ref().is_some_and(|x| x.horn.singular)
}

/// Whether two roles clash, in either order.
fn clash(ix: &Index, r1: Role, r2: Role) -> bool {
    let roles = &ix.roles;
    !roles.clash.is_empty() && (roles.clashes(r1, r2) || roles.clashes(r2, r1))
}

/// Sends a conclusion to the contexts that stand for the same element as this one: the
/// partners R7 identified, and, when `noms` says so, the individuals the context derives
/// and the contexts that stand for the context's individual.
pub fn spread<S: Sink>(st: &State, c: Concl, noms: bool, members: bool, out: &mut S) {
    let Some(x) = &st.extra else { return };
    let mut work = 0usize;
    let mut poll = |out: &mut S| {
        work += 1;
        !work.is_multiple_of(POLL) || out.go_on()
    };
    for &k in &x.horn.same.list {
        out.send(k, c);
        if !poll(out) {
            return;
        }
    }
    if noms {
        for &n in &x.noms {
            out.send(n, c);
            if !poll(out) {
                return;
            }
        }
    }
    if members {
        for &m in &x.members.list {
            out.send(m, c);
            if !poll(out) {
                return;
            }
        }
    }
}

/// The messages or scanned pairs between two polls of the run's bounds in the rules of
/// individuals, whose one inference can send many messages.
const POLL: usize = 256;

/// A new subsumer goes to the partners R7 identified; the rules of individuals give it
/// to the context's individuals and the contexts that stand for them.
pub fn spread_sub<S: Sink>(st: &State, d: Ex, out: &mut S) {
    spread(st, Concl::Sub(d), false, false, out);
}

/// What the context knows of its backward links, for a context that stands for the same
/// element: each link, whether its source holds each at-most filler and whether the
/// source is real.
fn send_links<S: Sink>(st: &State, to: Ex, out: &mut S) {
    let mut work = 0usize;
    let mut poll = |out: &mut S| {
        work += 1;
        !work.is_multiple_of(POLL) || out.go_on()
    };
    for (r, srcs) in &st.back {
        for &e in &srcs.list {
            out.send(to, Concl::Back(e, *r));
            if !poll(out) {
                return;
            }
        }
    }
    let Some(x) = &st.extra else { return };
    for &(m, b) in &x.horn.pred_in.list {
        out.send(to, Concl::PredIn(m, b));
        if !poll(out) {
            return;
        }
    }
    for &m in &x.horn.real_preds.list {
        out.send(to, Concl::RealBack(m));
        if !poll(out) {
            return;
        }
    }
}

/// What a real context knows of its own element, for a context that stands for the same
/// element: its loops and its Horn successors. The subsumers go by the rules of
/// individuals.
pub fn send_own<S: Sink>(st: &State, to: Ex, out: &mut S) {
    for &l in &st.loops {
        out.send(to, Concl::Loop(l));
    }
    if let Some(x) = &st.extra {
        for (i, &(r, n)) in x.horn.succ.list.iter().enumerate() {
            out.send(to, Concl::Succ(r, n));
            if (i + 1).is_multiple_of(POLL) && !out.go_on() {
                return;
            }
        }
    }
}

/// The context derives the individual `n`: it gives `n` its backward links, with what it
/// knows of each predecessor, which hold of `n` whether or not the context has an
/// instance, and, when real, its loops and Horn successors. It is singular.
pub fn derived_nominal<S: Sink>(ix: &Index, me: Ex, st: &mut State, n: Ex, out: &mut S) {
    if n != me {
        send_links(st, n, out);
        if st.real {
            send_own(st, n, out);
        }
    }
    singular(ix, me, st, false, out);
}

/// The context becomes real: its individuals take its loops and Horn successors, and its
/// successors learn that a predecessor is real.
pub fn became_real<S: Sink>(me: Ex, st: &State, out: &mut S) {
    if let Some(x) = &st.extra {
        for &n in &x.noms {
            send_own(st, n, out);
        }
        for &(_, n) in &x.horn.succ.list {
            out.send(n, Concl::RealBack(me));
        }
    }
}

/// The context is singular: when `counted` an at-most restriction of a singular
/// successor counts it. It tells the predecessors its at-most restrictions count, and R7
/// and R8 run over what it holds.
pub fn singular<S: Sink>(ix: &Index, me: Ex, st: &mut State, counted: bool, out: &mut S) {
    let h = hs(st);
    if counted && !h.counted {
        h.counted = true;
        out.merged();
    }
    if h.singular {
        return;
    }
    h.singular = true;
    out.merged();
    propagate_singular(ix, st, out);
    r7(ix, me, st, None, out);
    let links: Vec<(Role, Small<Ex>)> = st.back.iter().map(|(r, s)| (*r, s.clone())).collect();
    let mut work = 0usize;
    for (r, srcs) in links {
        for &m in &srcs.list {
            r8_back(ix, me, st, m, r, out);
            work += 1;
            if work.is_multiple_of(64) && !out.go_on() {
                return;
            }
        }
    }
}

/// Tells each predecessor that an at-most restriction of this singular context counts
/// that it is singular.
fn propagate_singular<S: Sink>(ix: &Index, st: &mut State, out: &mut S) {
    if !is_singular(st) {
        return;
    }
    let atmost = hs(st).atmost.clone();
    let mut told = Vec::new();
    for (s, b) in atmost {
        for (m, _) in preds(ix, st, s, b) {
            told.push(m);
        }
    }
    let h = hs(st);
    for m in told {
        if h.told_singular.insert(m) {
            out.send(m, Concl::Singular);
        }
    }
}

/// R7 at a singular context: two real predecessors that one at-most restriction counts
/// are one element, and so are a real predecessor and the context itself when a loop
/// counts the context. With `only`, the pairs considered are those with that
/// predecessor.
fn r7<S: Sink>(ix: &Index, me: Ex, st: &mut State, only: Option<Ex>, out: &mut S) {
    if !is_singular(st) {
        return;
    }
    let Some(x) = &st.extra else { return };
    let atmost = x.horn.atmost.clone();
    for (s, b) in atmost {
        let real: Vec<(Ex, Role)> = preds(ix, st, s, b)
            .into_iter()
            .filter(|&(m, _)| {
                st.extra
                    .as_ref()
                    .is_some_and(|x| x.horn.real_preds.contains(&m))
            })
            .collect();
        let looped = members(ix, st, s, b)
            .iter()
            .any(|m| matches!(m, Member::Loop(_)));
        // the pairs to consider: all of them, or those with the one predecessor the
        // trigger named
        let firsts: Vec<(Ex, Role)> = match only {
            Some(o) => real.iter().copied().filter(|&(m, _)| m == o).collect(),
            None => real.clone(),
        };
        let mut work = 0usize;
        for &(m1, r1) in &firsts {
            if looped {
                out.send(m1, Concl::Same(me));
                out.me(Concl::Same(m1));
            }
            for &(m2, r2) in &real {
                work += 1;
                if work.is_multiple_of(64) && !out.go_on() {
                    return;
                }
                if m2 == m1 || (only.is_none() && m2 > m1) {
                    continue;
                }
                if clash(ix, r1, r2) {
                    out.send(m1, Concl::Sub(BOTTOM));
                    out.send(m2, Concl::Sub(BOTTOM));
                } else {
                    out.send(m1, Concl::Same(m2));
                    out.send(m2, Concl::Same(m1));
                }
            }
        }
        if !out.go_on() {
            return;
        }
    }
}

/// R8 at a singular context, for a backward link from `m` by `r`: two links from `m` by
/// roles that clash make `m` unsatisfiable, and a forward link to `m` by a role that
/// clashes with `r⁻` makes the context unsatisfiable.
fn r8_back<S: Sink>(ix: &Index, _me: Ex, st: &mut State, m: Ex, r: Role, out: &mut S) {
    if !is_singular(st) || ix.roles.clash.is_empty() {
        return;
    }
    for (r2, srcs) in &st.back {
        if *r2 != r && srcs.contains(&m) && clash(ix, r, *r2) {
            out.send(m, Concl::Sub(BOTTOM));
        }
    }
    if let Some(i) = ix.roles.inv(r)
        && let Some(x) = &st.extra
        && x.horn
            .succ
            .list
            .iter()
            .any(|&(r1, n)| n == m && clash(ix, r1, i))
    {
        out.me(Concl::Sub(BOTTOM));
    }
}

/// R8 at a singular context, for a forward link to `n` by `r1`: a backward link from `n`
/// by a role whose inverse clashes with `r1` makes the context unsatisfiable.
fn r8_succ<S: Sink>(ix: &Index, st: &State, r1: Role, n: Ex, out: &mut S) {
    if !is_singular(st) || ix.roles.clash.is_empty() {
        return;
    }
    for (r2, srcs) in &st.back {
        if srcs.contains(&n) && ix.roles.inv(*r2).is_some_and(|i| clash(ix, r1, i)) {
            out.me(Concl::Sub(BOTTOM));
            return;
        }
    }
}

/// `RealBack(m)`: the predecessor m is real.
pub fn real_back<S: Sink>(ix: &Index, me: Ex, st: &mut State, m: Ex, out: &mut S) -> bool {
    if !hs(st).real_preds.insert(m) {
        return false;
    }
    spread(st, Concl::RealBack(m), true, false, out);
    r7(ix, me, st, Some(m), out);
    true
}

/// `Same(k)`: the context k stands for the same element, and takes everything this
/// context holds now; what comes later goes to it as it comes.
pub fn same<S: Sink>(_ix: &Index, me: Ex, st: &mut State, k: Ex, out: &mut S) -> bool {
    if k == me || !hs(st).same.insert(k) {
        return false;
    }
    if st.bottom {
        out.send(k, Concl::Sub(BOTTOM));
        return true;
    }
    for (i, &s) in st.subs.iter().enumerate() {
        out.send(k, Concl::Sub(s));
        if (i + 1).is_multiple_of(POLL) && !out.go_on() {
            return true;
        }
    }
    send_own(st, k, out);
    send_links(st, k, out);
    true
}

/// A new backward link from `e` by `r` in a run with individuals: it goes to the
/// contexts that stand for the same element and to the individuals the context derives,
/// the singular context tells `e` when the link counts, and R7 and R8 run for it.
pub fn back_full<S: Sink>(ix: &Index, me: Ex, st: &mut State, e: Ex, r: Role, out: &mut S) {
    spread(st, Concl::Back(e, r), true, false, out);
    if is_singular(st) {
        propagate_singular(ix, st, out);
        r7(ix, me, st, Some(e), out);
        r8_back(ix, me, st, e, r, out);
    }
}

/// A new loop by `r` in a run with individuals goes to the contexts that stand for the
/// same element, and R7 runs for the loop, which counts the context itself.
pub fn loop_full<S: Sink>(ix: &Index, me: Ex, st: &mut State, r: Role, out: &mut S) {
    spread(st, Concl::Loop(r), st.real, true, out);
    r7(ix, me, st, None, out);
}

/// A new Horn successor by `r` in `n` in a run with individuals goes to the contexts that
/// stand for the same element, and R8 runs for it.
pub fn succ_full<S: Sink>(ix: &Index, _me: Ex, st: &mut State, r: Role, n: Ex, out: &mut S) {
    spread(st, Concl::Succ(r, n), st.real, true, out);
    r8_succ(ix, st, r, n, out);
}

/// A new at-most restriction or a new predecessor filler at a singular context in a run
/// with individuals: the predecessors it counts learn that they are singular, and R7 runs.
pub fn counted_full<S: Sink>(ix: &Index, me: Ex, st: &mut State, only: Option<Ex>, out: &mut S) {
    if is_singular(st) {
        propagate_singular(ix, st, out);
        r7(ix, me, st, only, out);
    }
}
