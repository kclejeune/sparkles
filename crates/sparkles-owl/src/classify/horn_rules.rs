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
use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use super::saturate::{Concl, Sink, Small, State};
use rustc_hash::{FxHashMap, FxHashSet};

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
    /// the successors that no other successor dominates, in a run without individuals
    /// (item 710)
    pub live: Vec<(Role, Ex)>,
    /// the place of the core of each successor of `live` in `cores`, its length and its
    /// signature
    pub meta: Vec<(u32, u32, u64)>,
    /// the cores of the successors in `live`
    pub cores: Vec<Ex>,
    /// the successors that another successor dominates, which R3, R5 and R6 pass over
    pub dominated: Small<(Role, Ex)>,
    /// the successors of `succ` that `live` and `dominated` sort, from the first
    pub sorted: usize,
    /// `(M, N)`: R6 sent the predecessor M the core of the successor N, in a run that
    /// passes over dominated successors
    pub told: Small<(Ex, Ex)>,
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
            + self.live.capacity() * 8
            + self.meta.capacity() * 16
            + self.cores.capacity() * 4
            + self.dominated.bytes()
            + self.told.bytes()
    }

    /// Forgets which successors dominate which, which [`sync`] finds again from the
    /// successors in their order, for a state whose context ids change.
    pub fn forget_dominance(&mut self) {
        self.live = Vec::new();
        self.meta = Vec::new();
        self.cores = Vec::new();
        self.dominated = Small::default();
        self.sorted = 0;
    }
}

/// A successor that an at-most restriction counts: a link, or a loop, which makes the
/// instance its own successor.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Member {
    Link(Role, Ex),
    Loop(Role),
}

/// How much of the work that roadmap item 1025 found beside the rules of individuals a
/// run leaves out: 0 runs the rules as they were, 1 leaves out work that changes no
/// conclusion, which is R7's search for a pair when the predecessor that triggers it is
/// not a real predecessor that a restriction counts, the checks of the run's bounds after
/// a search that did nothing, and the copies of a context's at-most restrictions, and 2,
/// the default, also passes over a link that the Horn rules give a context beside a
/// link to the same context by a subrole, in a run with individuals, and R6's core for a
/// predecessor that is the successor itself. `SPARKLES_HORN_RULES` sets it, for the comparison of the forms.
#[inline]
pub fn spare() -> u8 {
    static LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| {
        std::env::var("SPARKLES_HORN_RULES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2)
    })
}

/// The at-most restriction at `i` of a context's list, when it has that many. The rules
/// that change the context while they read the list take its entries one at a time.
#[inline]
fn atmost_at(st: &State, i: usize) -> Option<(Role, Ex)> {
    st.extra
        .as_ref()
        .and_then(|x| x.horn.atmost.get(i).copied())
}

/// Whether a context with the Horn state `h` passes over a link by `r` to `n` that the
/// Horn rules give it, because it links to `n` by a subrole of `r` already
/// ([`spare`], level 2). [`succ`] asks it for a context of few successors, and the
/// search for witnesses for any, since a context may have passed over the link while it
/// had few.
pub fn covered(ix: &Index, h: &HornSt, r: Role, n: Ex) -> bool {
    let roles = &ix.roles;
    // the rules at `n` read a backward link through the inverse of its role, so the
    // subrole's link stands for the other only when its inverse is under the other's
    let inverse = |r2: Role| match roles.inv(r) {
        None => true,
        Some(i) => roles.inv(r2).is_some_and(|i2| roles.sub(i2, i)),
    };
    spare() >= 2
        && h.succ
            .list
            .iter()
            .any(|&(r2, n2)| n2 == n && r2 != r && roles.sub(r2, r) && inverse(r2))
}

/// The successors past which a context keeps every link the Horn rules give it, so
/// that the search of [`covered`] for each link costs at most this many comparisons.
const COVERED_MOST: usize = 16;

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

/// Whether the run passes over dominated successors: a run of the Horn rules without
/// individuals, universal roles or functional data roles, whose contexts derive what their
/// cores and successors give (F24, "The Horn rules", item 710).
#[inline]
fn dominates(ix: &Index) -> bool {
    ix.horn.as_ref().is_some_and(|h| h.dominate)
}

/// Whether the sorted `a` is a subset of the sorted `b`.
fn subset(a: &[Ex], b: &[Ex]) -> bool {
    if a.len() > b.len() {
        return false;
    }
    let mut j = 0;
    for &x in a {
        while j < b.len() && b[j] < x {
            j += 1;
        }
        if j == b.len() || b[j] != x {
            return false;
        }
        j += 1;
    }
    true
}

/// Sorts the successors that came since the last call into those that another successor
/// dominates and those that no other does (item 710). A successor by `r'` in `n'`
/// dominates one by `r` in `n` when `r'` is under `r` and the core of `n` is part of that
/// of `n'`: every rule that reads the second derives from the first what it would derive
/// from the second, or more. A successor that a new one dominates leaves `live`, and one
/// that a successor in `live` dominates when it comes goes to `dominated` at once.
fn sync<S: Sink>(ix: &Index, st: &mut State, out: &mut S) {
    if !dominates(ix) {
        return;
    }
    let Some(x) = st.extra.as_mut() else { return };
    let h = &mut x.horn;
    if h.sorted == h.succ.list.len() {
        return;
    }
    let roles = &ix.roles;
    let mut core = Vec::new();
    // the sort's entries are charged to the run's memory at each check of the bounds and
    // at the end, as data the activation keeps, whose charge the run's sink batches
    let mut charge = 0usize;
    while h.sorted < h.succ.list.len() {
        // a sort of many successors polls the run's bounds, and one that stops leaves
        // the rest unsorted, which the rules read as successors that no other dominates
        if h.sorted.is_multiple_of(64) {
            if charge > 0 {
                out.keep_data(std::mem::take(&mut charge));
            }
            if !out.go_on() {
                return;
            }
        }
        let (r, n) = h.succ.list[h.sorted];
        h.sorted += 1;
        out.core(ix, n, &mut core);
        let sig = signature(&core);
        // the sort's entry for the successor, in `live` with its core or in `dominated`,
        // is charged to the run's memory as it is kept, and the cores that a compaction
        // drops are released; a run past its budget stops at its next check
        // past LIVE_MOST successors that no other dominates, a successor is compared with
        // none, so that the sort costs at most that many comparisons for each successor
        if h.live.len() >= LIVE_MOST {
            h.meta.push((h.cores.len() as u32, core.len() as u32, sig));
            h.cores.extend_from_slice(&core);
            h.live.push((r, n));
            charge += SORT_BYTES + 4 * core.len();
            continue;
        }
        let cores = &h.cores;
        let of = |at: u32, len: u32| &cores[at as usize..(at + len) as usize];
        // a core is part of another only when its signature is part of the other's, which
        // passes over most pairs before the roles and the cores are compared
        if h.meta
            .iter()
            .zip(&h.live)
            .any(|(&(at, len, sig2), &(r2, _))| {
                sig & !sig2 == 0 && roles.sub(r2, r) && subset(&core, of(at, len))
            })
        {
            h.dominated.insert((r, n));
            charge += SORT_BYTES;
            continue;
        }
        // the successors that the new one dominates leave `live`, keeping an entry in
        // `dominated`, and their cores stay charged until a compaction drops them
        let mut kept = 0;
        for i in 0..h.live.len() {
            let (r2, n2) = h.live[i];
            let (at, len, sig2) = h.meta[i];
            if sig2 & !sig == 0 && roles.sub(r, r2) && subset(of(at, len), &core) {
                h.dominated.insert((r2, n2));
            } else {
                h.live[kept] = h.live[i];
                h.meta[kept] = h.meta[i];
                kept += 1;
            }
        }
        let left = kept < h.live.len();
        h.live.truncate(kept);
        h.meta.truncate(kept);
        h.meta.push((h.cores.len() as u32, core.len() as u32, sig));
        h.cores.extend_from_slice(&core);
        h.live.push((r, n));
        charge += SORT_BYTES + 4 * core.len();
        // the cores of the successors that left `live` go once they take half the room,
        // which only a successor's leaving can bring about
        if left
            && h.cores.len() > 64
            && h.cores.len() > 2 * h.meta.iter().map(|l| l.1 as usize).sum::<usize>()
        {
            let mut cores = Vec::with_capacity(h.cores.len() / 2);
            for l in &mut h.meta {
                let at = cores.len() as u32;
                cores.extend_from_slice(&h.cores[l.0 as usize..(l.0 + l.1) as usize]);
                l.0 = at;
            }
            // the cores this sort has not charged yet come off what it has still to
            // charge, and only the rest, charged before, is released
            let freed = 4 * (h.cores.len() - cores.len());
            let unpaid = freed.min(charge);
            charge -= unpaid;
            if freed > unpaid {
                out.release_data(freed - unpaid);
            }
            h.cores = cores;
        }
    }
    if charge > 0 {
        out.keep_data(charge);
    }
}

/// A signature of a sorted core, one bit for each of its expressions, so that the core of
/// a subset has a signature that is part of the other's.
#[inline]
fn signature(core: &[Ex]) -> u64 {
    core.iter()
        .fold(0, |s, &x| s | 1u64 << (x.wrapping_mul(0x9E37_79B1) >> 26))
}

/// The successors that no other dominates past which [`sync`] compares a successor with
/// none of them.
const LIVE_MOST: usize = 512;

/// The bytes of a successor's entry in [`HornSt::live`] or [`HornSt::dominated`], without
/// its core.
const SORT_BYTES: usize = 24;

/// Whether a dominated successor, which [`sync`] sorted, is the one by `r` in `n`.
fn is_dominated(ix: &Index, st: &State, r: Role, n: Ex) -> bool {
    dominates(ix)
        && st
            .extra
            .as_ref()
            .is_some_and(|x| x.horn.dominated.contains(&(r, n)))
}

/// The successors that `≤1 S.B` counts.
fn members(ix: &Index, st: &State, s: Role, b: Ex) -> Vec<Member> {
    let roles = &ix.roles;
    let mut k: Vec<Member> = Vec::new();
    if let Some(x) = &st.extra {
        let h = &x.horn;
        // the successors that a stopped sort left unsorted count as well
        let (sorted, rest) = if dominates(ix) {
            (&h.live[..], &h.succ.list[h.sorted..])
        } else {
            (&h.succ.list[..], &h.succ.list[..0])
        };
        for &(r, n) in sorted.iter().chain(rest) {
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

/// The bytes a pair of [`Told`] takes in its list and its hash set.
const TOLD_BYTES: usize = 24;

/// The pairs of a predecessor and a successor whose core R6 sent the predecessor, which
/// a run that passes over dominated successors keeps so that it sends each core once,
/// with the bytes of the pairs it recorded since it was taken.
struct Told(Option<Small<(Ex, Ex)>>, usize);

impl Told {
    fn take(ix: &Index, st: &mut State) -> Told {
        Told(dominates(ix).then(|| std::mem::take(&mut hs(st).told)), 0)
    }
    /// Puts the record back and charges the pairs it recorded to the run's memory, once
    /// for all of them; a run past its budget stops at its next check.
    fn put<S: Sink>(self, st: &mut State, out: &mut S) {
        if let Some(t) = self.0 {
            hs(st).told = t;
        }
        if self.1 > 0 {
            out.keep_data(self.1);
        }
    }
    /// Whether R6 has still to send `m` the core of `n`, which it records.
    fn first(&mut self, m: Ex, n: Ex) -> bool {
        let Some(t) = self.0.as_mut() else {
            return true;
        };
        if !t.insert((m, n)) {
            return false;
        }
        self.1 += TOLD_BYTES;
        true
    }
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
    let roles = &ix.roles;
    // a link that the Horn rules give the context beside one to the same context by a
    // subrole says nothing new: every rule reads a link's role as a role under another,
    // through a chain that the subrole composes as well, or against a clash that the
    // subrole has too (roadmap item 1025)
    // only in a run with individuals, whose asserted inverse pairs make such links,
    // so that a run without them searches for none
    if !sent
        && ix.has_individuals()
        && st
            .extra
            .as_ref()
            .is_some_and(|x| x.horn.succ.list.len() <= COVERED_MOST && covered(ix, &x.horn, r, n))
    {
        return false;
    }
    if !hs(st).succ.insert((r, n)) {
        return false;
    }
    debug_assert!(
        ix.horn.as_ref().is_none_or(|h| !h.dead_over(roles, r)),
        "a link by a role under one whose universals the automata dropped"
    );
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
    // R5 reads the successors that no other dominates as they come, so a context with an
    // at-most restriction sorts them here; R3 reads them at the end of the activation,
    // whose flush sorts them and passes over the dominated ones it finds pending
    if !h.atmost.is_empty() {
        sync(ix, st, out);
        if is_dominated(ix, st, r, n) {
            return true;
        }
    }
    let h = hs(st);
    if !h.dirty && h.all_roles.iter().any(|&s| roles.sub(r, s)) {
        h.pending.push((r, n));
    }
    if spare() == 0 {
        let atmost = hs(st).atmost.clone();
        for (s, b) in atmost {
            if roles.sub(r, s) && held_succ(st, n, b) {
                counted(ix, me, st, s, b, Member::Link(r, n), out);
            }
        }
        return true;
    }
    let mut i = 0;
    while let Some((s, b)) = atmost_at(st, i) {
        if roles.sub(r, s) && held_succ(st, n, b) {
            counted(ix, me, st, s, b, Member::Link(r, n), out);
        }
        i += 1;
    }
    true
}

/// R3: the successor by `r` in `n` with the fillers of the universals on the superroles
/// of `r`.
fn extend<S: Sink>(
    ix: &Index,
    real: bool,
    fs: &[Ex],
    n: Ex,
    r: Role,
    sends: &mut Sends,
    out: &mut S,
) -> bool {
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
    let m = match named(ix, real, &core) {
        Some(a) => {
            send_named(a, &core, sends, out);
            if a != n {
                out.me(Concl::Succ(r, a));
            }
            // the individual can be the context itself, whose conclusions wait in the
            // activation's own list, so the caller applies them before it ends
            return true;
        }
        None if core.len() == 1 => core[0],
        None => out.intern(&core),
    };
    out.me(Concl::Succ(r, m));
    true
}

/// The individual that stands for the conjunction `core` as a successor of a real
/// context, when the core holds one (roadmap item 869). The context has an instance,
/// whose successor in the conjunction is the individual, so the individual holds the
/// rest of the core, which [`send_named`] sends it, and the context links to the
/// individual's own context where it would link to the conjunction's. The conjunction's
/// context would be real and derive the individual, and the two would exchange
/// everything they hold, so the link reads the same conclusions from either.
fn named(ix: &Index, real: bool, core: &[Ex]) -> Option<Ex> {
    if !real {
        return None;
    }
    core.iter()
        .copied()
        .find(|&e| matches!(ix.head(e).node, Node::Nominal(_)))
}

/// What a flush keeps of the conclusions it gives its own context and of its sends to
/// individuals.
struct Sends {
    /// the context of the flush
    me: Ex,
    /// the conclusions that R5 and the step to an individual gave the context itself
    own: FxHashSet<Concl>,
    /// whether a check of the run's bounds after a send stopped the run
    stop: bool,
}

impl Sends {
    fn new(me: Ex) -> Sends {
        Sends {
            me,
            own: FxHashSet::default(),
            stop: false,
        }
    }

    /// Gives the context itself a conclusion, once in a flush. The conclusions wait in
    /// the activation's own list, which no bound counts, and successors by several roles
    /// give the context the same ones. The list then takes each member of the cores and
    /// each filler that the context reads once, each loop once, and one link for each
    /// pair of a role and the target of R5. A flush merges the successors of one at-most
    /// restriction and leaves the others to the next, so the pairs are those of one
    /// restriction (roadmap item 1026).
    fn me<S: Sink>(&mut self, c: Concl, out: &mut S) {
        if self.own.insert(c) {
            out.me(c);
        }
    }
}

/// Sends the individual `a` the other members of the conjunction `core`. One flush can
/// send a conjunction for each successor before the activation applies a conclusion, so
/// the messages to another context are counted against the run's bounds, and
/// `sends.stop` says that the run stopped there. The context itself takes each member
/// once in a flush ([`Sends::me`]).
fn send_named<S: Sink>(a: Ex, core: &[Ex], sends: &mut Sends, out: &mut S) {
    if a == sends.me {
        for &e in core {
            if e != a {
                sends.me(Concl::Sub(e), out);
            }
        }
        return;
    }
    for &e in core {
        if e != a {
            out.send(a, Concl::Sub(e));
        }
    }
    sends.stop |= !out.sent_go_on();
}

/// R3 at the end of an activation, over the successors that came since the last call, or
/// over all of them when a universal came. Each successor is extended once with every
/// universal the context holds by then, so that universals that come one after another
/// create no context for each conjunction on the way. The answer is whether it sent a
/// conclusion to the context.
pub fn flush<S: Sink>(ix: &Index, me: Ex, st: &mut State, out: &mut S) -> bool {
    let Some(x) = st.extra.as_mut() else {
        return false;
    };
    let h = &mut x.horn;
    if !h.dirty && h.pending.is_empty() && h.merges.is_empty() {
        return false;
    }
    sync(ix, st, out);
    let h = &mut st.extra.as_mut().expect("the context's Horn state").horn;
    let mut sent = false;
    let mut merges = std::mem::take(&mut h.merges);
    merges.sort_unstable();
    merges.dedup();
    // what the flush sent the context itself, and whether the run stopped at a send
    let mut sends = Sends::new(me);
    let mut merges = merges.into_iter();
    while let Some((s, b)) = merges.next() {
        let gave = merge_all(ix, st, s, b, &mut sends, out);
        // a run that stopped keeps nothing of the flush
        if sends.stop {
            return sent | gave;
        }
        // The conclusions R5 gives the context wait in the activation's own list, which
        // no bound counts. One restriction gives at most a link for each successor it
        // counts and the members of their cores, which the context's state holds and
        // the run has charged. The other restrictions and R3 wait until the activation
        // has applied them, so the list never holds the links of every restriction at
        // once (roadmap item 1026).
        if gave {
            hs(st).merges.extend(merges);
            return true;
        }
    }
    let h = &mut st.extra.as_mut().expect("the context's Horn state").horn;
    let mut todo: Vec<(Role, Ex)> = if h.dirty {
        h.pending.clear();
        if dominates(ix) {
            h.live
                .iter()
                .copied()
                .chain(h.succ.list[h.sorted..].iter().copied())
                .collect()
        } else {
            h.succ.list.to_vec()
        }
    } else {
        std::mem::take(&mut h.pending)
    };
    // a dominated successor's extension is dominated by that of the successor that
    // dominates it, which R3 extends as well
    if dominates(ix) && !h.dominated.list.is_empty() {
        todo.retain(|&(r, n)| !h.dominated.contains(&(r, n)));
    }
    h.dirty = false;
    let real = st.real;
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
        sent |= extend(ix, real, fs, n, r, &mut sends, out);
        // a run that stopped keeps nothing of the flush
        if sends.stop {
            break;
        }
    }
    sent
}

/// The context derives `∀s.a`: R4 to the predecessors, and R3 to the successors.
pub fn forall<S: Sink>(ix: &Index, _me: Ex, st: &mut State, s: Role, a: Ex, out: &mut S) {
    let roles = &ix.roles;
    if let Some(i) = roles.inv(s) {
        st.props.push((i, a));
        if !out.defer() {
            for (r, srcs) in &st.back {
                if roles.sub(*r, i) {
                    for &e in &srcs.list {
                        out.send(e, Concl::Sub(a));
                    }
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
    sync(ix, st, out);
    let k = members(ix, st, s, b);
    let ps = preds(ix, st, s, b);
    if !k.is_empty() && !ps.is_empty() {
        let mut told = Told::take(ix, st);
        for (m, r1) in ps {
            for &x in &k {
                identify(ix, me, st, &mut told, m, r1, x, out);
            }
        }
        told.put(st, out);
    }
    counted_full(ix, me, st, None, out);
}

/// A successor that `≤1 s.b` counts is new: R5 at the end of the activation, and R6
/// with every predecessor.
fn counted<S: Sink>(ix: &Index, me: Ex, st: &mut State, s: Role, b: Ex, x: Member, out: &mut S) {
    hs(st).merges.push((s, b));
    let ps = preds(ix, st, s, b);
    if ps.is_empty() {
        return;
    }
    let mut told = Told::take(ix, st);
    for (m, r1) in ps {
        identify(ix, me, st, &mut told, m, r1, x, out);
    }
    told.put(st, out);
}

/// R5 at the end of an activation: every successor that `≤1 s.b` counts is one, so each
/// link goes to the conjunction of all of them at once. Merging them two at a time would
/// make a context for every subset on the way, since each merged successor is counted
/// again and merges with the rest. A loop makes each successor the instance itself. The
/// answer is whether it sent a conclusion to the context.
fn merge_all<S: Sink>(
    ix: &Index,
    st: &mut State,
    s: Role,
    b: Ex,
    sends: &mut Sends,
    out: &mut S,
) -> bool {
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
                    sends.me(Concl::Sub(BOTTOM), out);
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
                sends.me(Concl::Sub(m), out);
            }
            sends.me(Concl::Loop(r), out);
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
    let a = named(ix, st.real, &u);
    let m = match a {
        Some(a) => {
            send_named(a, &u, sends, out);
            a
        }
        None if u.len() == 1 => u[0],
        None => out.intern(&u),
    };
    // the individual can be the context itself, whose conclusions wait in the
    // activation's own list, so the caller applies them before it ends
    let mut sent = a.is_some();
    for (&(r, n), c) in links.iter().zip(&cores) {
        if *c != u && a != Some(n) {
            sends.me(Concl::Succ(r, m), out);
            sent = true;
        }
    }
    sent
}

/// R6: the predecessor `m`, by `r1`, and the successor `x` are one.
#[allow(clippy::too_many_arguments)]
fn identify<S: Sink>(
    ix: &Index,
    me: Ex,
    st: &State,
    told: &mut Told,
    m: Ex,
    r1: Role,
    x: Member,
    out: &mut S,
) {
    let roles = &ix.roles;
    match x {
        Member::Link(r2, n2) => {
            if m == me && st.loops.contains(&r1) {
                // the predecessor is the instance itself, so the successor is too
                send_core(ix, me, n2, out);
                out.me(Concl::Loop(r2));
                return;
            }
            // a predecessor holds for good the core it was sent once, and one that is
            // the successor itself holds its own
            if (m != n2 || spare() < 2) && told.first(m, n2) {
                send_core(ix, m, n2, out);
            }
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
    if st.extra.as_ref().is_some_and(|x| !x.horn.atmost.is_empty()) {
        sync(ix, st, out);
    }
    let Some(x) = &st.extra else { return };
    for &b in &x.horn.fills {
        out.send(e, Concl::SuccIn(me, b));
    }
    let roles = &ix.roles;
    let Some(i) = roles.inv(r) else { return };
    if x.horn.atmost.is_empty() {
        return;
    }
    let mut told = Told::take(ix, st);
    let x = st.extra.as_ref().expect("the context's Horn state");
    for &(s, b) in &x.horn.atmost {
        if roles.sub(i, s) && held_pred(st, e, b) {
            for y in members(ix, st, s, b) {
                identify(ix, me, st, &mut told, e, r, y, out);
            }
        }
    }
    told.put(st, out);
}

/// `SuccIn(n, b)`: the successor n holds b.
pub fn succ_in<S: Sink>(ix: &Index, me: Ex, st: &mut State, n: Ex, b: Ex, out: &mut S) -> bool {
    if !hs(st).succ_in.insert((n, b)) {
        return false;
    }
    if !hs(st).atmost.is_empty() {
        sync(ix, st, out);
    }
    let roles = &ix.roles;
    let dom = dominates(ix);
    let h: &HornSt = hs(st);
    let found: Vec<(Role, Role)> = h
        .atmost
        .iter()
        .filter(|&&(_, b2)| b2 == b)
        .flat_map(|&(s, _)| {
            h.succ
                .list
                .iter()
                .filter(move |&&(r, n2)| {
                    n2 == n && roles.sub(r, s) && !(dom && h.dominated.contains(&(r, n2)))
                })
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
    if !hs(st).atmost.is_empty() {
        sync(ix, st, out);
    }
    let roles = &ix.roles;
    let atmost: Vec<Role> = hs(st)
        .atmost
        .iter()
        .filter(|&&(_, b2)| b2 == b)
        .map(|&(s, _)| s)
        .collect();
    let mut told = Told::take(ix, st);
    for s in atmost {
        let k = members(ix, st, s, b);
        for (r1, srcs) in &st.back {
            if srcs.contains(&m) && roles.inv(*r1).is_some_and(|i| roles.sub(i, s)) {
                for &x in &k {
                    identify(ix, me, st, &mut told, m, *r1, x, out);
                }
            }
        }
    }
    told.put(st, out);
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
    let mut work = 0usize;
    for &l in &st.loops {
        out.send(to, Concl::Loop(l));
        work += 1;
        if work.is_multiple_of(POLL) && !out.go_on() {
            return;
        }
    }
    if let Some(x) = &st.extra {
        for &(r, n) in &x.horn.succ.list {
            out.send(to, Concl::Succ(r, n));
            work += 1;
            if work.is_multiple_of(POLL) && !out.go_on() {
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
    // the scans inside poll when they are long, and the calls poll when they are many
    let mut work = 0usize;
    for (r, srcs) in links {
        for &m in &srcs.list {
            if !r8_back(ix, me, st, m, r, out) {
                return;
            }
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
    if spare() == 0 {
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
        return;
    }
    let mut i = 0;
    while let Some((s, b)) = atmost_at(st, i) {
        for (m, _) in preds(ix, st, s, b) {
            if hs(st).told_singular.insert(m) {
                out.send(m, Concl::Singular);
            }
        }
        i += 1;
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
    if spare() >= 1 {
        return r7_spare(ix, me, st, only, out);
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

/// The steps of reading after which [`r7_spare`] checks the run's bounds: one for an
/// at-most restriction, for each group of backward links it looks at, for each source
/// of a group it counts, for each loop and for each predecessor of a pair it visits,
/// and the length of the table of the roles that clash for each lookup in it.
const R7_SCAN: usize = 64;

/// [`r7`] without the work that changes nothing (roadmap item 1025). A predecessor that
/// triggers the rule and is not real, or that a restriction does not count, is in no
/// pair of that restriction, so the search for the pairs is left out for it. The run's
/// bounds are checked after a restriction that made a pair or a send, and once for
/// every [`R7_SCAN`] steps of reading restrictions, groups of backward links, their
/// sources, loops and pairs, so that a context with many of any of them still stops
/// with the run.
fn r7_spare<S: Sink>(ix: &Index, me: Ex, st: &mut State, only: Option<Ex>, out: &mut S) {
    let is_real = |st: &State, m: Ex| {
        st.extra
            .as_ref()
            .is_some_and(|x| x.horn.real_preds.contains(&m))
    };
    if let Some(o) = only
        && !is_real(st, o)
    {
        return;
    }
    let roles = &ix.roles;
    let mut i = 0;
    let mut steps = 0usize;
    // counts steps of reading, and answers whether the run goes on
    let mut read = |n: usize, out: &mut S| {
        steps += n;
        if steps < R7_SCAN {
            return true;
        }
        steps = 0;
        out.go_on()
    };
    while let Some((s, b)) = atmost_at(st, i) {
        i += 1;
        if !read(1, out) {
            return;
        }
        // whether the restriction counts the predecessor that triggers the rule, by a
        // backward link from it by a role under S⁻ and B among what it holds, and then
        // the real predecessors it counts in the order of [`preds`]
        if let Some(o) = only {
            let mut counts = false;
            if held_pred(st, o, b) {
                for (r, srcs) in &st.back {
                    if !read(1, out) {
                        return;
                    }
                    if roles.inv(*r).is_some_and(|i| roles.sub(i, s)) && srcs.contains(&o) {
                        counts = true;
                        break;
                    }
                }
            }
            if !counts {
                continue;
            }
        }
        let mut real: Vec<(Ex, Role)> = Vec::new();
        for (r, srcs) in &st.back {
            if !read(1, out) {
                return;
            }
            if roles.inv(*r).is_some_and(|i| roles.sub(i, s)) {
                for &m in &srcs.list {
                    if !read(1, out) {
                        return;
                    }
                    if held_pred(st, m, b) && is_real(st, m) {
                        real.push((m, *r));
                    }
                }
            }
        }
        if real.is_empty() {
            continue;
        }
        // whether a loop of the context is a successor that the restriction counts,
        // which [`members`] lists with the links
        let mut looped = false;
        if held_own(st, b) {
            for &l in &st.loops {
                if !read(1, out) {
                    return;
                }
                if roles.sub(l, s) {
                    looped = true;
                    break;
                }
            }
        }
        // a lookup of a clash reads the table of the roles that clash twice
        let lookup = 2 * roles.clash.len();
        for &(m1, r1) in &real {
            if !read(1, out) {
                return;
            }
            if only.is_some_and(|o| m1 != o) {
                continue;
            }
            if looped {
                out.send(m1, Concl::Same(me));
                out.me(Concl::Same(m1));
            }
            for &(m2, r2) in &real {
                if !read(1, out) {
                    return;
                }
                if m2 == m1 || (only.is_none() && m2 > m1) {
                    continue;
                }
                if !read(lookup, out) {
                    return;
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
        // a restriction with one real predecessor and no loop made no pair and no send
        if (looped || real.len() > 1) && !out.go_on() {
            return;
        }
    }
}

/// R8 at a singular context, for a backward link from `m` by `r`: two links from `m` by
/// roles that clash make `m` unsatisfiable, and a forward link to `m` by a role that
/// clashes with `r⁻` makes the context unsatisfiable.
fn r8_back<S: Sink>(ix: &Index, _me: Ex, st: &mut State, m: Ex, r: Role, out: &mut S) -> bool {
    if !is_singular(st) || ix.roles.clash.is_empty() {
        return true;
    }
    // the scans poll the run's bounds, and the answer is whether the run goes on
    let mut work = 0usize;
    for (r2, srcs) in &st.back {
        if *r2 != r && srcs.contains(&m) && clash(ix, r, *r2) {
            out.send(m, Concl::Sub(BOTTOM));
        }
        work += 1;
        if work.is_multiple_of(64) && !out.go_on() {
            return false;
        }
    }
    if let Some(i) = ix.roles.inv(r)
        && let Some(x) = &st.extra
    {
        for &(r1, n) in &x.horn.succ.list {
            if n == m && clash(ix, r1, i) {
                out.me(Concl::Sub(BOTTOM));
                break;
            }
            work += 1;
            if work.is_multiple_of(64) && !out.go_on() {
                return false;
            }
        }
    }
    true
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
        let _ = r8_back(ix, me, st, e, r, out);
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

#[cfg(test)]
mod tests {
    use super::super::horn::HornIx;
    use super::super::index::{Axioms, Interner, Node};
    use super::*;
    use crate::model::Class;

    /// A sink that counts charges and releases and lets the run go on for a number of
    /// checks.
    struct Checks {
        reserved: usize,
        released: usize,
        left: std::cell::Cell<usize>,
    }

    impl Checks {
        fn new(left: usize) -> Checks {
            Checks {
                reserved: 0,
                released: 0,
                left: std::cell::Cell::new(left),
            }
        }
    }

    impl Sink for Checks {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn reserve_data(&mut self, bytes: usize) -> bool {
            self.reserved += bytes;
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.released += bytes;
        }
        fn go_on(&self) -> bool {
            let left = self.left.get();
            self.left.set(left.saturating_sub(1));
            left > 0
        }
        fn core(&self, ix: &Index, e: Ex, out: &mut Vec<Ex>) {
            out.clear();
            super::super::saturate::flatten(ix, e, out);
        }
    }

    /// An index of `n` classes whose Horn rules pass over dominated successors.
    fn index(n: usize) -> (Index, Vec<Ex>) {
        let mut it = Interner::new();
        let cs: Vec<Ex> = (0..n)
            .map(|k| it.class(&Class::new(format!("urn:dominate:c{k}"))))
            .collect();
        let _ = it.node(Node::Top);
        let mut ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
        ix.horn = Some(Box::new(HornIx {
            flags: Vec::new(),
            base: it.nodes.len() as Ex,
            dominate: true,
            dead: Vec::new(),
        }));
        (ix, cs)
    }

    /// A sink that records what a flush sends and counts its checks of the sent
    /// messages, and answers that the run stopped after a number of them. It keeps the
    /// default answer of [`Sink::stopped`], so that the flush stops on the check's answer.
    struct Recorded {
        sent: Vec<(Ex, Concl)>,
        local: Vec<Concl>,
        checks: usize,
        left: usize,
    }

    impl Sink for Recorded {
        fn send(&mut self, to: Ex, c: Concl) {
            self.sent.push((to, c));
        }
        fn me(&mut self, c: Concl) {
            self.local.push(c);
        }
        fn sent_go_on(&mut self) -> bool {
            self.checks += 1;
            self.left = self.left.saturating_sub(1);
            self.left > 0
        }
        fn core(&self, ix: &Index, e: Ex, out: &mut Vec<Ex>) {
            out.clear();
            super::super::saturate::flatten(ix, e, out);
        }
    }

    /// An index of two classes, an individual and three roles, of which role 0 is under role 1 and
    /// roles 0 and 2 are inverses, for the rules that read a link's role.
    fn roles_index() -> (Index, Ex, Ex) {
        let mut it = Interner::new();
        let me = it.class(&Class::new("urn:spare:me"));
        let n = it.class(&Class::new("urn:spare:n"));
        // the rules pass over a covered link only in a run with individuals
        let _ = it.nominal(&crate::model::NamedIndividual::new("urn:spare:a"));
        let _ = it.node(Node::Top);
        let mut ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
        ix.roles.sup = vec![vec![0, 1], vec![1], vec![2]];
        ix.roles.inv = vec![2, super::super::index::NO_ROLE, 0];
        ix.horn = Some(Box::new(HornIx {
            flags: Vec::new(),
            base: it.nodes.len() as Ex,
            dominate: false,
            dead: Vec::new(),
        }));
        (ix, me, n)
    }

    /// A sink that records the messages and counts the checks of the run's bounds.
    struct Polled {
        sent: Vec<(Ex, Concl)>,
        local: Vec<Concl>,
        checks: std::cell::Cell<usize>,
    }

    impl Polled {
        fn new() -> Polled {
            Polled {
                sent: Vec::new(),
                local: Vec::new(),
                checks: std::cell::Cell::new(0),
            }
        }
    }

    impl Sink for Polled {
        fn send(&mut self, to: Ex, c: Concl) {
            self.sent.push((to, c));
        }
        fn me(&mut self, c: Concl) {
            self.local.push(c);
        }
        fn go_on(&self) -> bool {
            self.checks.set(self.checks.get() + 1);
            true
        }
        fn core(&self, ix: &Index, e: Ex, out: &mut Vec<Ex>) {
            out.clear();
            super::super::saturate::flatten(ix, e, out);
        }
    }

    /// A link that the Horn rules give a context beside a link to the same context by a
    /// subrole is passed over and sends nothing, while the link by the subrole that
    /// comes second is kept, as is a link by another role (item 1025).
    #[test]
    fn a_link_beside_one_by_a_subrole_is_passed_over() {
        // the levels below 2 keep every link, which the comparison of the forms runs
        let pass = spare() >= 2;
        let (ix, me, n) = roles_index();
        let mut st = State::default();
        let mut out = Polled::new();
        assert!(succ(&ix, me, &mut st, 0, n, false, &mut out));
        assert_eq!(out.sent, vec![(n, Concl::Back(me, 0))]);
        let mut out = Polled::new();
        assert_eq!(succ(&ix, me, &mut st, 1, n, false, &mut out), !pass);
        if pass {
            assert!(out.sent.is_empty() && out.local.is_empty());
            assert_eq!(hs(&mut st).succ.list, vec![(0, n)]);
        } else {
            assert_eq!(out.sent, vec![(n, Concl::Back(me, 1))]);
            assert_eq!(hs(&mut st).succ.list, vec![(0, n), (1, n)]);
        }
        // a link by a role with an inverse stays beside a subrole's link whose inverse
        // the run does not have, since the target reads its links through the inverse
        let mut other =
            Index::build(&Axioms::default(), &mut Interner::new(), None, &|| false).unwrap();
        other.nominals = ix.nominals.clone();
        other.roles.sup = vec![vec![0, 1], vec![1], vec![2]];
        other.roles.inv = vec![super::super::index::NO_ROLE, 2, 1];
        other.horn = Some(Box::new(HornIx {
            flags: Vec::new(),
            base: ix.horn.as_ref().unwrap().base,
            dominate: false,
            dead: Vec::new(),
        }));
        let mut st2 = State::default();
        let mut out = Polled::new();
        assert!(succ(&other, me, &mut st2, 0, n, false, &mut out));
        assert!(succ(&other, me, &mut st2, 1, n, false, &mut out));
        assert_eq!(hs(&mut st2).succ.list, vec![(0, n), (1, n)]);
        // with both inverses in the run, as every named role has, the link by role 1 is
        // passed over when the inverse of role 0 is under its inverse, and kept when not
        for under in [true, false] {
            let mut both =
                Index::build(&Axioms::default(), &mut Interner::new(), None, &|| false).unwrap();
            both.nominals = ix.nominals.clone();
            both.roles.sup = vec![
                vec![0, 1],
                vec![1],
                if under { vec![2, 3] } else { vec![2] },
                vec![3],
            ];
            both.roles.inv = vec![2, 3, 0, 1];
            both.horn = Some(Box::new(HornIx {
                flags: Vec::new(),
                base: ix.horn.as_ref().unwrap().base,
                dominate: false,
                dead: Vec::new(),
            }));
            let over = under && pass;
            let mut st3 = State::default();
            let mut out = Polled::new();
            assert!(succ(&both, me, &mut st3, 0, n, false, &mut out));
            assert_eq!(succ(&both, me, &mut st3, 1, n, false, &mut out), !over);
            let kept = if over {
                vec![(0, n)]
            } else {
                vec![(0, n), (1, n)]
            };
            assert_eq!(hs(&mut st3).succ.list, kept);
            let backs = if over {
                vec![(n, Concl::Back(me, 0))]
            } else {
                vec![(n, Concl::Back(me, 0)), (n, Concl::Back(me, 1))]
            };
            assert_eq!(out.sent, backs);
        }
        // a link by a role that is not above the first is another link
        let mut out = Polled::new();
        assert!(succ(&ix, me, &mut st, 2, n, false, &mut out));
        assert_eq!(out.sent, vec![(n, Concl::Back(me, 2))]);
        // the link by the superrole that came first stays beside the subrole's
        let mut st = State::default();
        let mut out = Polled::new();
        assert!(succ(&ix, me, &mut st, 1, n, false, &mut out));
        assert!(succ(&ix, me, &mut st, 0, n, false, &mut out));
        assert_eq!(hs(&mut st).succ.list, vec![(1, n), (0, n)]);
    }

    /// R7 at a singular context searches for no pair and checks no bound for a
    /// predecessor that is not real or that no restriction counts, and identifies two
    /// real predecessors that a restriction counts (item 1025).
    #[test]
    fn r7_does_nothing_for_a_predecessor_outside_every_pair() {
        let (ix, me, n) = roles_index();
        let m = TOP;
        let mut st = State::default();
        // the restriction on role 0 counts the sources of links by its inverse, role 2
        hs(&mut st).atmost.push((0, TOP));
        hs(&mut st).singular = true;
        st.back.push((2, Small::default()));
        st.back[0].1.insert(n);
        st.back[0].1.insert(m);
        st.back.push((1, Small::default()));
        st.back[1].1.insert(me);
        // the rules as they were check the bounds after every restriction
        let spared = spare() >= 1;
        // no predecessor is real
        let mut out = Polled::new();
        r7(&ix, me, &mut st, Some(n), &mut out);
        assert!(out.sent.is_empty() && out.local.is_empty());
        assert!(!spared || out.checks.get() == 0);
        // one real predecessor makes no pair
        hs(&mut st).real_preds.insert(n);
        let mut out = Polled::new();
        r7(&ix, me, &mut st, Some(n), &mut out);
        assert!(out.sent.is_empty());
        assert!(!spared || out.checks.get() == 0);
        // a real predecessor that the restriction does not count is in no pair
        hs(&mut st).real_preds.insert(me);
        let mut out = Polled::new();
        r7(&ix, me, &mut st, Some(me), &mut out);
        assert!(out.sent.is_empty());
        assert!(!spared || out.checks.get() == 0);
        // two real predecessors that it counts are one element
        hs(&mut st).real_preds.insert(m);
        let mut out = Polled::new();
        r7(&ix, me, &mut st, Some(m), &mut out);
        assert_eq!(out.sent, vec![(m, Concl::Same(n)), (n, Concl::Same(m))]);
        let mut out = Polled::new();
        r7(&ix, me, &mut st, None, &mut out);
        assert_eq!(out.sent.len(), 2);
    }

    /// A sink that counts the checks of the run's bounds and stops the run at the check
    /// after the first `allow`.
    struct Stopped {
        sent: usize,
        checks: std::cell::Cell<usize>,
        allow: usize,
    }

    impl Sink for Stopped {
        fn send(&mut self, _: Ex, _: Concl) {
            self.sent += 1;
        }
        fn me(&mut self, _: Concl) {
            self.sent += 1;
        }
        fn go_on(&self) -> bool {
            self.checks.set(self.checks.get() + 1);
            self.checks.get() <= self.allow
        }
        fn core(&self, ix: &Index, e: Ex, out: &mut Vec<Ex>) {
            out.clear();
            super::super::saturate::flatten(ix, e, out);
        }
    }

    /// R7 at a singular context of many at-most restrictions that make no pair still
    /// checks the run's bounds while it reads them and stops with the run, whether the
    /// restrictions count the predecessor that triggers the rule, or none, or every
    /// predecessor is considered (item 1025).
    #[test]
    fn r7_over_many_restrictions_without_a_pair_stops_with_the_run() {
        let (ix, me, n) = roles_index();
        let mut st = State::default();
        hs(&mut st).singular = true;
        // role 0 counts the sources of links by its inverse, role 2, and role 2 counts
        // those of links by role 0, of which there are none
        for k in 0..200 {
            hs(&mut st)
                .atmost
                .push((if k % 2 == 0 { 0 } else { 2 }, TOP));
        }
        st.back.push((2, Small::default()));
        st.back[0].1.insert(n);
        st.back.push((1, Small::default()));
        st.back[1].1.insert(me);
        hs(&mut st).real_preds.insert(n);
        hs(&mut st).real_preds.insert(me);
        // `n` is the one real predecessor that half of the restrictions count, `me` is
        // one that none counts, and without a trigger every restriction is read
        for only in [Some(n), Some(me), None] {
            let mut out = Stopped {
                sent: 0,
                checks: std::cell::Cell::new(0),
                allow: 0,
            };
            r7(&ix, me, &mut st, only, &mut out);
            assert_eq!(out.sent, 0);
            assert_eq!(out.checks.get(), 1, "for {only:?}");
        }
    }

    /// R7 at a singular context checks the run's bounds while it reads one restriction's
    /// groups of backward links and their sources, and stops at the first check that
    /// ends the run: a group of many sources of which one or none is real, and many
    /// groups that the restriction does not count (item 1025).
    #[test]
    fn r7_over_many_backward_links_without_a_pair_stops_with_the_run() {
        let (ix, me, _) = roles_index();
        let srcs: Vec<Ex> = (100..600).collect();
        // one group by role 2, the inverse of the restriction's role, with 500 sources
        let mut wide = State::default();
        hs(&mut wide).singular = true;
        hs(&mut wide).atmost.push((0, TOP));
        wide.back.push((2, Small::default()));
        for &m in &srcs {
            wide.back[0].1.insert(m);
        }
        let mut one = wide.clone();
        hs(&mut one).real_preds.insert(srcs[0]);
        // 500 groups by role 1, whose sources the restriction does not count
        let mut many = State::default();
        hs(&mut many).singular = true;
        hs(&mut many).atmost.push((0, TOP));
        for &m in &srcs {
            many.back.push((1, Small::default()));
            many.back.last_mut().unwrap().1.insert(m);
        }
        hs(&mut many).real_preds.insert(me);
        let cases = [
            (one, Some(srcs[0])),
            (wide, None),
            (many.clone(), Some(me)),
            (many, None),
        ];
        for (mut st, only) in cases {
            let st = &mut st;
            let mut out = Stopped {
                sent: 0,
                checks: std::cell::Cell::new(0),
                allow: 0,
            };
            r7(&ix, me, st, only, &mut out);
            assert_eq!((out.sent, out.checks.get()), (0, 1), "for {only:?}");
            // the rules as they were check once after the restriction
            let mut out = Polled::new();
            r7(&ix, me, st, only, &mut out);
            assert!(out.sent.is_empty());
            let least = if spare() >= 1 { srcs.len() / 64 } else { 1 };
            assert!(out.checks.get() >= least, "for {only:?}");
        }
    }

    /// R7 at a singular context checks the run's bounds while it reads the context's
    /// loops, the predecessors of a pair and the table of the roles that clash, and a
    /// check that ends the run after one that let it go on stops the rule before the
    /// pair it was about to make (item 1025).
    #[test]
    fn r7_stops_with_the_run_before_a_pair_it_has_not_reached() {
        let (mut ix, me, n) = roles_index();
        let m = TOP;
        let stopped = |allow| Stopped {
            sent: 0,
            checks: std::cell::Cell::new(0),
            allow,
        };
        // two real predecessors that the restriction counts, behind 500 that are not real
        let mut st = State::default();
        hs(&mut st).singular = true;
        hs(&mut st).atmost.push((0, TOP));
        st.back.push((2, Small::default()));
        for k in 100..600 {
            st.back[0].1.insert(k);
        }
        st.back[0].1.insert(n);
        st.back[0].1.insert(m);
        hs(&mut st).real_preds.insert(n);
        hs(&mut st).real_preds.insert(m);
        let mut out = Polled::new();
        r7(&ix, me, &mut st, None, &mut out);
        assert_eq!(out.sent.len(), 2);
        let all = out.checks.get();
        // the run ends at the second check, which the rules as they were reach only
        // after the pair
        let mut out = stopped(1);
        r7(&ix, me, &mut st, None, &mut out);
        if spare() >= 1 {
            assert!(all >= 500 / 64);
            assert_eq!((out.sent, out.checks.get()), (0, 2));
        }
        // loops by roles that the restriction does not count, and one real predecessor
        let mut st = State::default();
        hs(&mut st).singular = true;
        hs(&mut st).atmost.push((0, TOP));
        st.back.push((2, Small::default()));
        st.back[0].1.insert(n);
        hs(&mut st).real_preds.insert(n);
        st.loops = vec![2; 500];
        let mut out = stopped(0);
        r7(&ix, me, &mut st, Some(n), &mut out);
        assert_eq!((out.sent, out.checks.get()), (0, 1));
        // a long table of roles that clash, none of them the roles of the pair, is read
        // for each pair, so the run ends before the first pair is made
        ix.roles.clash = vec![(1, 1); 500];
        let mut st = State::default();
        hs(&mut st).singular = true;
        hs(&mut st).atmost.push((0, TOP));
        st.back.push((2, Small::default()));
        st.back[0].1.insert(n);
        st.back[0].1.insert(m);
        hs(&mut st).real_preds.insert(n);
        hs(&mut st).real_preds.insert(m);
        let mut out = stopped(0);
        r7(&ix, me, &mut st, None, &mut out);
        if spare() >= 1 {
            assert_eq!((out.sent, out.checks.get()), (0, 1));
        }
        let mut out = Polled::new();
        r7(&ix, me, &mut st, None, &mut out);
        assert_eq!(out.sent.len(), 2);
    }

    /// R3 at a real context sends each individual among its successors the fillers of
    /// its universals, counts the messages of each against the run's bounds, and stops
    /// with the run (item 869).
    #[test]
    fn a_flush_at_a_real_context_counts_what_it_sends_the_individuals() {
        let mut it = Interner::new();
        let c = it.class(&Class::new("urn:named:c"));
        let ns: Vec<Ex> = (0..3)
            .map(|k| {
                it.nominal(&crate::model::NamedIndividual::new(format!(
                    "urn:named:n{k}"
                )))
            })
            .collect();
        let _ = it.node(Node::Top);
        let mut ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
        ix.horn = Some(Box::new(HornIx {
            flags: Vec::new(),
            base: it.nodes.len() as Ex,
            dominate: false,
            dead: Vec::new(),
        }));
        let state = |real: bool| {
            let mut st = State {
                real,
                ..State::default()
            };
            let h = hs(&mut st);
            for &n in &ns {
                h.succ.insert((0, n));
            }
            h.alls.push((0, c));
            h.all_roles.push(0);
            h.dirty = true;
            st
        };
        let sink = |left: usize| Recorded {
            sent: Vec::new(),
            local: Vec::new(),
            checks: 0,
            left,
        };
        // every individual takes the filler, with a check after each, and the context,
        // which is the class, gets no successor
        let me = c;
        let mut out = sink(usize::MAX);
        assert!(flush(&ix, me, &mut state(true), &mut out));
        let expected: Vec<(Ex, Concl)> = ns.iter().map(|&n| (n, Concl::Sub(c))).collect();
        assert_eq!(out.sent, expected);
        assert_eq!(out.checks, ns.len());
        assert!(out.local.is_empty());
        // the run stops at the first check, and the flush sends no more
        let mut out = sink(1);
        flush(&ix, me, &mut state(true), &mut out);
        assert_eq!(out.sent, expected[..1]);
    }

    /// R3 at an individual that is its own successor by several roles sends the context
    /// each filler once in a flush. The messages wait in the activation's own list,
    /// which no bound counts, so the list grows by the distinct fillers and not by the
    /// fillers of every role (item 869).
    #[test]
    fn a_flush_at_an_individual_sends_the_context_itself_each_member_once() {
        let mut it = Interner::new();
        let d = it.class(&Class::new("urn:named:d"));
        let cs: Vec<Ex> = (0..3)
            .map(|k| it.class(&Class::new(format!("urn:named:c{k}"))))
            .collect();
        let a = it.nominal(&crate::model::NamedIndividual::new("urn:named:a"));
        let _ = it.node(Node::Top);
        let mut ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
        ix.horn = Some(Box::new(HornIx {
            flags: Vec::new(),
            base: it.nodes.len() as Ex,
            dominate: false,
            dead: Vec::new(),
        }));
        // a is its own successor by three roles, each with a filler of its own and one
        // that all three share
        let mut st = State {
            real: true,
            ..State::default()
        };
        let h = hs(&mut st);
        for (r, &c) in cs.iter().enumerate() {
            let r = r as Role;
            h.succ.insert((r, a));
            h.alls.push((r, c));
            h.alls.push((r, d));
            h.all_roles.push(r);
        }
        h.dirty = true;
        let mut out = Recorded {
            sent: Vec::new(),
            local: Vec::new(),
            checks: 0,
            left: usize::MAX,
        };
        assert!(flush(&ix, a, &mut st, &mut out));
        // the three conjunctions hold six fillers in all, four of them distinct
        assert_eq!(out.local.len(), 4, "{:?}", out.local);
        for &e in cs.iter().chain([&d]) {
            assert!(out.local.contains(&Concl::Sub(e)));
        }
        // a already names itself by each role, and its own conclusions need no check
        assert!(out.sent.is_empty());
        assert_eq!(out.checks, 0);
    }

    /// R5 at an individual gives the context each conclusion once in a flush, however
    /// many at-most restrictions count the same successors (item 869).
    #[test]
    fn a_flush_of_several_merges_gives_the_context_each_conclusion_once() {
        let mut it = Interner::new();
        let cs: Vec<Ex> = (0..3)
            .map(|k| it.class(&Class::new(format!("urn:named:c{k}"))))
            .collect();
        let bs: Vec<Ex> = (0..4)
            .map(|k| it.class(&Class::new(format!("urn:named:b{k}"))))
            .collect();
        let a = it.nominal(&crate::model::NamedIndividual::new("urn:named:a"));
        let _ = it.node(Node::Top);
        let mut ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
        ix.horn = Some(Box::new(HornIx {
            flags: Vec::new(),
            base: it.nodes.len() as Ex,
            dominate: false,
            dead: Vec::new(),
        }));
        // a has itself and three classes as successors by one role, and four at-most
        // restrictions on the role, each of which counts all of them
        let mut st = State {
            real: true,
            ..State::default()
        };
        let h = hs(&mut st);
        for &n in cs.iter().chain([&a]) {
            h.succ.insert((0, n));
            for &b in &bs {
                h.succ_in.insert((n, b));
            }
        }
        for &b in &bs {
            h.atmost.push((0, b));
            h.merges.push((0, b));
        }
        let mut out = Recorded {
            sent: Vec::new(),
            local: Vec::new(),
            checks: 0,
            left: usize::MAX,
        };
        assert!(flush(&ix, a, &mut st, &mut out));
        // each merge gives a the three classes and a link to itself for each of them,
        // and the four merges give the same
        let mut expected: Vec<Concl> = cs.iter().map(|&c| Concl::Sub(c)).collect();
        expected.push(Concl::Succ(0, a));
        out.local.sort();
        expected.sort();
        assert_eq!(out.local, expected);
        assert!(out.sent.is_empty());
    }

    /// A flush merges the successors of one at-most restriction and leaves the others
    /// pending, so the activation's own list holds the links of one restriction and not
    /// of all of them (item 1026).
    #[test]
    fn a_flush_gives_the_context_the_links_of_one_merge_at_a_time() {
        const RESTRICTIONS: u32 = 5;
        let mut it = Interner::new();
        let cs: Vec<Ex> = (0..RESTRICTIONS)
            .map(|k| it.class(&Class::new(format!("urn:named:c{k}"))))
            .collect();
        let ns: Vec<Ex> = (0..RESTRICTIONS)
            .map(|k| {
                it.nominal(&crate::model::NamedIndividual::new(format!(
                    "urn:named:n{k}"
                )))
            })
            .collect();
        let me = it.class(&Class::new("urn:named:me"));
        let _ = it.node(Node::Top);
        let mut ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
        ix.horn = Some(Box::new(HornIx {
            flags: Vec::new(),
            base: it.nodes.len() as Ex,
            dominate: false,
            dead: Vec::new(),
        }));
        // each role has an at-most restriction of its own, with an individual and a class
        // as its successors, so each restriction merges into a target of its own
        let mut st = State {
            real: true,
            ..State::default()
        };
        let h = hs(&mut st);
        for k in 0..RESTRICTIONS {
            h.succ.insert((k, ns[k as usize]));
            h.succ.insert((k, cs[k as usize]));
            h.atmost.push((k, TOP));
            h.merges.push((k, TOP));
        }
        let mut given: Vec<Concl> = Vec::new();
        for k in 0..RESTRICTIONS {
            let mut out = Recorded {
                sent: Vec::new(),
                local: Vec::new(),
                checks: 0,
                left: usize::MAX,
            };
            assert!(flush(&ix, me, &mut st, &mut out));
            // one link to the individual of the restriction, which takes the class
            assert_eq!(out.local.len(), 1, "flush {k}");
            assert_eq!(out.sent.len(), 1, "flush {k}");
            assert_eq!(hs(&mut st).merges.len() as u32, RESTRICTIONS - 1 - k);
            given.extend(out.local);
        }
        let mut expected: Vec<Concl> = (0..RESTRICTIONS)
            .map(|k| Concl::Succ(k, ns[k as usize]))
            .collect();
        given.sort();
        expected.sort();
        assert_eq!(given, expected);
        let mut out = Recorded {
            sent: Vec::new(),
            local: Vec::new(),
            checks: 0,
            left: usize::MAX,
        };
        assert!(!flush(&ix, me, &mut st, &mut out));
    }

    /// R6 records each pair of a predecessor and a successor whose core it sent, and the
    /// record is charged to the run's memory (item 710).
    #[test]
    fn the_record_of_sent_cores_is_charged() {
        let (ix, cs) = index(2);
        let mut st = State::default();
        let mut told = Told::take(&ix, &mut st);
        let mut out = Checks::new(1);
        assert!(told.first(cs[0], cs[1]));
        assert!(!told.first(cs[0], cs[1]));
        told.put(&mut st, &mut out);
        assert_eq!(out.reserved, TOLD_BYTES);
        assert_eq!(st.extra.as_ref().unwrap().horn.told.list.len(), 1);
    }

    /// A sort of many successors polls the run's bounds and stops when the run does, and
    /// the successors it left unsorted still count for the at-most restrictions.
    #[test]
    fn a_sort_of_successors_stops_with_the_run() {
        let (ix, cs) = index(300);
        let mut st = State::default();
        for &c in &cs {
            hs(&mut st).succ.insert((0, c));
        }
        // the run stops at the sort's third check, after it sorted 128 successors
        let mut out = Checks::new(2);
        sync(&ix, &mut st, &mut out);
        let h = &st.extra.as_ref().unwrap().horn;
        assert_eq!(h.sorted, 128, "the sort did not poll");
        assert_eq!(members(&ix, &st, 0, TOP).len(), cs.len());
        let mut out = Checks::new(usize::MAX);
        sync(&ix, &mut st, &mut out);
        assert_eq!(st.extra.as_ref().unwrap().horn.sorted, cs.len());
        assert_eq!(members(&ix, &st, 0, TOP).len(), cs.len());
        assert!(out.reserved > 0);
    }

    /// The sort charges a dominated successor its entry alone, and a compaction releases
    /// the cores it drops, so that the charge follows what the sort keeps.
    #[test]
    fn the_sort_charges_what_it_keeps() {
        let mut it = Interner::new();
        let cs: Vec<Ex> = (0..101)
            .map(|k| it.class(&Class::new(format!("urn:dominate:c{k}"))))
            .collect();
        let conj = |it: &mut Interner, of: &[Ex]| {
            of[1..]
                .iter()
                .fold(of[0], |a, &b| it.node(Node::Conj(a, b)))
        };
        let k = conj(&mut it, &cs[..100]);
        let k2 = conj(&mut it, &cs);
        let mut ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
        ix.horn = Some(Box::new(HornIx {
            flags: Vec::new(),
            base: it.nodes.len() as Ex,
            dominate: true,
            dead: Vec::new(),
        }));
        let mut st = State::default();
        let mut out = Checks::new(usize::MAX);
        for &c in &cs[..100] {
            hs(&mut st).succ.insert((0, c));
        }
        sync(&ix, &mut st, &mut out);
        assert_eq!(out.reserved, 100 * (SORT_BYTES + 4));
        // owl:Thing's successor is dominated and takes its entry alone
        hs(&mut st).succ.insert((0, TOP));
        sync(&ix, &mut st, &mut out);
        assert_eq!(out.reserved, 100 * (SORT_BYTES + 4) + SORT_BYTES);
        // the conjunction of the hundred classes dominates them, and that of all of them
        // dominates it, which compacts the cores
        hs(&mut st).succ.insert((0, k));
        hs(&mut st).succ.insert((0, k2));
        sync(&ix, &mut st, &mut out);
        let h = &st.extra.as_ref().unwrap().horn;
        assert_eq!(h.live.len(), 1);
        assert_eq!(h.cores.len(), 101);
        let kept = SORT_BYTES + h.dominated.list.len() * SORT_BYTES + 4 * h.cores.len();
        assert_eq!(out.reserved - out.released, kept);
    }

    /// A sink whose count, like the run's, stops at zero on a release.
    struct Held(usize);

    impl Sink for Held {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn reserve_data(&mut self, bytes: usize) -> bool {
            self.0 += bytes;
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.0 = self.0.saturating_sub(bytes);
        }
        fn core(&self, ix: &Index, e: Ex, out: &mut Vec<Ex>) {
            out.clear();
            super::super::saturate::flatten(ix, e, out);
        }
    }

    /// A compaction within one sort releases the cores that the sort has not charged
    /// yet from what it has still to charge, so that a count that stops at zero ends at
    /// what the sort keeps.
    #[test]
    fn a_compaction_within_one_sort_nets_its_release_against_the_charge() {
        let mut it = Interner::new();
        let cs: Vec<Ex> = (0..40)
            .map(|k| it.class(&Class::new(format!("urn:dominate:c{k}"))))
            .collect();
        let conj = |it: &mut Interner, of: &[Ex]| {
            of[1..]
                .iter()
                .fold(of[0], |a, &b| it.node(Node::Conj(a, b)))
        };
        // twenty conjunctions of two classes, ten of four, which dominate them, and the
        // conjunction of all forty, which dominates those, come in one sort
        let mut parts: Vec<Ex> = cs.chunks(2).map(|c| conj(&mut it, c)).collect();
        parts.extend(cs.chunks(4).map(|c| conj(&mut it, c)).collect::<Vec<_>>());
        parts.push(conj(&mut it, &cs));
        let mut ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
        ix.horn = Some(Box::new(HornIx {
            flags: Vec::new(),
            base: it.nodes.len() as Ex,
            dominate: true,
            dead: Vec::new(),
        }));
        let mut st = State::default();
        for &m in &parts {
            hs(&mut st).succ.insert((0, m));
        }
        let mut out = Held(0);
        sync(&ix, &mut st, &mut out);
        let h = &st.extra.as_ref().unwrap().horn;
        assert_eq!(h.live.len(), 1);
        assert_eq!(h.cores.len(), 40, "the sort did not compact");
        let kept = SORT_BYTES + h.dominated.list.len() * SORT_BYTES + 4 * h.cores.len();
        assert_eq!(out.0, kept);
    }
}
