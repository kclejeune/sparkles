//! The Horn normalization of roadmap item 603's Tier H: the axioms outside OWL 2 EL whose
//! clauses have at most one disjunct, turned into the forms that the rules of Kazakov's
//! consequence-driven calculus for Horn-SHIQ read (*Consequence-Driven Reasoning for Horn
//! SHIQ Ontologies*, IJCAI 2009, Table 3).
//!
//! A class expression in a positive position becomes an expression `x` with `x ⊑ E`, and
//! one in a negative position an expression `y` with `E ⊑ y`, by the polarity rules of the
//! structural transformation, with fresh classes for what the index cannot read in place.
//! The forms are those of the EL index with three more nodes: `∀R.A` and `≤1 S.B` in
//! positive positions and the fresh classes. A positive `≥n S.C` becomes n existential
//! restrictions to fresh, pairwise disjoint subclasses of C, as the paper's Lemma 2 does,
//! so that the at-most rule merges two of them only to find the merge unsatisfiable. A
//! negation, a universal or a number restriction on the left side moves to the right side
//! as its dual, which keeps an axiom such as `DisjointClasses(¬A B)` Horn.
//!
//! Roles have inverses, and the role hierarchy is closed under them. A universal
//! restriction on a role that a chain or transitivity reaches is unfolded by the automaton
//! of the role (Horrocks, Kutz and Sattler, *The Even More Irresistible SROIQ*, KR 2006), a
//! fresh class per state, so that the rules propagate universals only along the links that
//! existential restrictions create. The automaton can be exponential in the depth of the
//! role hierarchy, and a run whose automata pass [`MAX_STATES`] states, or whose role
//! inclusions are not regular, takes no axiom outside OWL 2 EL.
//!
//! An axiom the normalization cannot take, because it is not Horn, names an individual,
//! the top or bottom property or a datatype outside OWL 2 EL, or breaks the restriction to
//! simple roles, stays left out and reported, which is sound.

use super::index::{
    Axioms, BOTTOM, Ex, Halt, Interner, NO_ROLE, Node, POLL_WORK, Role, RoleName, Roles, Scope,
    TOP, class_expression,
};
use crate::model::*;
use crate::vocab;
use rustc_hash::FxHashMap;
use std::collections::BTreeSet;

/// The most states the automata of a run may have together.
pub const MAX_STATES: usize = 1 << 16;
/// The largest minimum cardinality the normalization unfolds into distinct successors.
pub const MAX_AT_LEAST: u32 = 64;

/// What the Horn normalization adds to the normalized axioms.
#[derive(Default)]
pub struct HornAx {
    /// pairs of roles that cannot link one pair of elements together
    pub clash: Vec<(Role, Role)>,
    /// the fillers of at-most restrictions, which the index reads as premises
    pub premises: Vec<Ex>,
    /// the automaton of each role that a chain or transitivity reaches
    pub autos: FxHashMap<Role, Nfa>,
    /// the axioms outside OWL 2 EL the run takes
    pub taken: usize,
}

impl HornAx {
    pub fn bytes(&self) -> usize {
        self.clash.capacity() * 8
            + self.premises.capacity() * 4
            + self
                .autos
                .values()
                .map(|a| 64 + a.trans.capacity() * 16)
                .sum::<usize>()
    }
}

/// What the Horn rules read in the index.
pub struct HornIx {
    /// for each expression, [`HornIx::FILLER`] and [`HornIx::UNFOLDED`]
    pub flags: Vec<u8>,
    /// the first id of the contexts the saturation creates for conjunctions
    pub base: Ex,
    /// whether the rules pass over a successor that another dominates, which a run
    /// without individuals, universal roles or functional data roles does (item 710)
    pub dominate: bool,
    /// the roles whose transitions the automata lost because no link or loop of the run
    /// can meet a universal on them ([`EpsFree::prune`]), sorted. The rules check in
    /// debug builds that no link or loop does.
    pub dead: Vec<Role>,
}

impl HornIx {
    /// The expression is the filler of an at-most restriction, whose holders the
    /// contexts tell their neighbours.
    pub const FILLER: u8 = 1;
    /// The expression is a universal on a role with an automaton, whose told subsumer,
    /// the automaton's first state, stands for it, so that the Horn rules pass over it.
    pub const UNFOLDED: u8 = 2;
    /// The expression adds nothing to a successor that every context does not hold: it is
    /// a class that no rule takes as a premise, whose told subsumers are those of
    /// `owl:Thing` or absorbed classes. R3 passes over a universal with such a filler,
    /// whose successors would send nothing back for it, while R4 still takes the filler
    /// to the predecessors, where it can be a named class's answer.
    pub const ABSORBED: u8 = 4;
    /// The expression is a universal on a universal role, which holds everywhere once a
    /// real context holds it, as a fact of a universal role (F24, "The Horn rules with
    /// individuals"), so that R3 and R4 pass over it.
    pub const GLOBAL: u8 = 8;

    #[inline]
    pub fn is(&self, e: Ex, flag: u8) -> bool {
        self.flags.get(e as usize).is_some_and(|&f| f & flag != 0)
    }

    pub fn bytes(&self) -> usize {
        self.flags.capacity() + self.dead.capacity() * size_of::<Role>()
    }

    /// Whether `r` is under a role whose transitions the automata lost, for the checks
    /// of a debug build. It searches the sorted list for `r` and each role above it, so
    /// that a run with many links and many such roles is not checked by their product.
    pub fn dead_over(&self, roles: &Roles, r: Role) -> bool {
        let dead = |t: Role| self.dead.binary_search(&t).is_ok();
        dead(r)
            || roles
                .sup
                .get(r as usize)
                .is_some_and(|v| v.iter().any(|&t| dead(t)))
    }

    /// Whether `r` is under the inverse of a role whose transitions the automata lost.
    pub fn dead_over_inverse(&self, roles: &Roles, r: Role) -> bool {
        let dead = |i: Role| {
            roles
                .inv(i)
                .is_some_and(|t| self.dead.binary_search(&t).is_ok())
        };
        dead(r)
            || roles
                .sup
                .get(r as usize)
                .is_some_and(|v| v.iter().any(|&i| dead(i)))
    }
}

/// Marks the expressions that [`HornIx::ABSORBED`] describes, as the greatest set
/// closed under its definition, or marks none when `stop` says the run's deadline or
/// cancellation came. The work is linear in the told subsumptions: a rejected candidate
/// rejects the candidates whose told subsumers it is. A run with individuals marks none,
/// since a successor that R3 would extend can stand for an individual, which then takes
/// the filler as a subsumer of its own (F24, "The Horn rules with individuals").
pub fn absorb(ix: &mut super::index::Index, stop: &dyn Fn() -> bool) {
    if ix.has_individuals() {
        return;
    }
    let n = ix
        .horn
        .as_ref()
        .expect("a run with the Horn rules")
        .flags
        .len();
    let mut work = 0usize;
    let mut tick = |w: usize| {
        let before = work;
        work += w;
        before / POLL_WORK != work / POLL_WORK && stop()
    };
    let mut global = vec![false; n];
    let mut stack = vec![TOP];
    while let Some(x) = stack.pop() {
        if !std::mem::replace(&mut global[x as usize], true) {
            let told = ix.told_of(ix.head(x));
            if tick(1 + told.len()) {
                return;
            }
            stack.extend(told.iter().copied());
        }
    }
    let flags = &ix.horn.as_ref().expect("a run with the Horn rules").flags;
    let inert = |e: Ex| {
        let h = ix.head(e);
        matches!(h.node, Node::Class(_) | Node::Fresh(_))
            && ix.conj_of(h).is_empty()
            && ix.exist_of(h).is_empty()
            && ix.disjoint_of(h).is_empty()
            && flags
                .get(e as usize)
                .is_none_or(|&f| f & HornIx::FILLER == 0)
    };
    let mut cand: Vec<bool> = (0..n as Ex).map(|e| e != TOP && inert(e)).collect();
    // the candidates each expression is a told subsumer of, and the candidates with a
    // told subsumer that is neither held by ⊤ nor a candidate; a universal among the
    // told subsumers is never absorbed, since R4 takes its filler back to the
    // predecessor that gave the successor the class
    let mut below: Vec<Vec<Ex>> = vec![Vec::new(); n];
    let mut out: Vec<Ex> = Vec::new();
    for e in 0..n {
        if !cand[e] {
            continue;
        }
        let told = ix.told_of(ix.head(e as Ex));
        if tick(1 + told.len()) {
            return;
        }
        let mut ok = true;
        for &t in told {
            if !global[t as usize] {
                below[t as usize].push(e as Ex);
                ok &= cand[t as usize];
            }
        }
        if !ok {
            out.push(e as Ex);
        }
    }
    for &e in &out {
        cand[e as usize] = false;
    }
    while let Some(x) = out.pop() {
        if tick(1 + below[x as usize].len()) {
            return;
        }
        for &e in &below[x as usize] {
            if std::mem::replace(&mut cand[e as usize], false) {
                out.push(e);
            }
        }
    }
    let flags = &mut ix.horn.as_mut().expect("a run with the Horn rules").flags;
    for (e, c) in cand.into_iter().enumerate() {
        if c {
            flags[e] |= HornIx::ABSORBED;
        }
    }
}

/// A nondeterministic automaton over roles, with ε-transitions as `None`.
#[derive(Clone, Debug, Default)]
pub struct Nfa {
    pub states: u32,
    pub start: u32,
    pub end: u32,
    pub trans: Vec<(u32, Option<Role>, u32)>,
}

/// An automaton without ε-transitions and with only the states that its start reaches,
/// which accepts from each state what the [`Nfa`] it comes from accepts there.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EpsFree {
    pub states: u32,
    pub start: u32,
    pub finals: Vec<bool>,
    pub trans: Vec<(u32, Role, u32)>,
    /// for each state, whether a transition enters it
    pub entered: Vec<bool>,
    /// for each state, whether a transition leaves it
    pub left: Vec<bool>,
}

impl Nfa {
    /// The automaton with its ε-transitions closed over: a state takes the transitions of
    /// the states its ε-transitions reach, and is final when one of them is the end.
    pub fn eps_free(&self) -> EpsFree {
        let n = self.states as usize;
        let mut eps: Vec<Vec<u32>> = vec![Vec::new(); n];
        let mut labeled: Vec<Vec<(Role, u32)>> = vec![Vec::new(); n];
        for &(p, l, q) in &self.trans {
            match l {
                None => eps[p as usize].push(q),
                Some(l) => labeled[p as usize].push((l, q)),
            }
        }
        // the visits of the closures, marked by the state each started from
        let mut seen: Vec<u32> = vec![u32::MAX; n];
        let mut closure = |p: u32| -> Vec<u32> {
            let mut stack = vec![p];
            let mut out = Vec::new();
            while let Some(x) = stack.pop() {
                if std::mem::replace(&mut seen[x as usize], p) == p {
                    continue;
                }
                out.push(x);
                stack.extend(eps[x as usize].iter().copied());
            }
            out
        };
        // the states the start reaches, renumbered in the order they are found
        let mut id: Vec<Option<u32>> = vec![None; n];
        let mut order = vec![self.start];
        id[self.start as usize] = Some(0);
        let mut finals = Vec::new();
        let mut trans = Vec::new();
        let mut at = 0;
        while at < order.len() {
            let p = order[at];
            let c = closure(p);
            finals.push(c.contains(&self.end));
            for x in c {
                for &(l, q) in &labeled[x as usize] {
                    let q = *id[q as usize].get_or_insert_with(|| {
                        order.push(q);
                        order.len() as u32 - 1
                    });
                    trans.push((at as u32, l, q));
                }
            }
            at += 1;
        }
        trans.sort_unstable();
        trans.dedup();
        let states = order.len();
        let mut entered = vec![false; states];
        let mut left = vec![false; states];
        for &(p, _, q) in &trans {
            left[p as usize] = true;
            entered[q as usize] = true;
        }
        EpsFree {
            states: states as u32,
            start: 0,
            finals,
            trans,
            entered,
            left,
        }
    }
}

/// How far a run reduces the automata that unfold its universals (roadmap item 693):
/// 0 leaves them as the ε-closure gives them, 1 merges the states that accept the same
/// words by the same transitions, 2 also drops a transition that another from the same
/// state to the same target covers by a superrole, and 3, the default, first drops the
/// transitions by roles that no link or loop of the run can have. `SPARKLES_HORN_AUTOMATA`
/// sets it, for the comparison of the forms.
pub fn reduction() -> u8 {
    static LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| {
        std::env::var("SPARKLES_HORN_AUTOMATA")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3)
    })
}

/// How far a run of Tier D reduces the automata that unfold its universals (roadmap
/// item 712), when [`Limits::dl_automata`](super::Limits::dl_automata) does not say: 0
/// leaves them as the ε-closure gives them, 1 merges the states that accept the same words
/// by the same transitions, and 2, the default, also drops a transition that another from
/// the same state to the same target covers by a superrole. `SPARKLES_DL_AUTOMATA` sets
/// it, for the comparison of the forms, and a value above 2 is 2. Tier D does not drop the
/// transitions by dead roles, the Horn rules' third reduction.
pub fn dl_reduction() -> u8 {
    static LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| {
        std::env::var("SPARKLES_DL_AUTOMATA")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .map_or(2, |l| l.min(2))
    })
}

/// The steps a reduction of one automaton takes at most. A step is a state or a transition
/// that a pass visits, a comparison of a sort, or a comparison of a search in a superrole
/// list, and each pass charges an upper bound of its steps before it starts:
///
/// * the start of a refinement, a step for each state and transition, which lists each
///   state's transitions;
/// * a round of the refinement, a step for each state and `1 + ⌈log2(d + 1)⌉` for each of
///   the `d` transitions of a state, which builds, sorts and hashes the state's signature;
/// * the quotient of a refinement, `1 + ⌈log2(m + 1)⌉` for each of the `m` transitions,
///   which its sort takes, charged before the first round;
/// * a pass over the covered transitions, twice that for its two sorts, and for each
///   group of `g` transitions with one source and one target `2·g²` lookups in the role
///   hierarchy, each of [`log_steps`] of the longest superrole list.
///
/// The automata of the ORE 2015 files and of Uberon need thousands of steps.
const REDUCE_WORK: usize = 1 << 24;

/// The steps that a sort takes for each of `n` elements, and a binary search among `n`:
/// `1 + ⌈log2(n + 1)⌉`.
pub(super) fn log_steps(n: usize) -> usize {
    1 + (usize::BITS - n.leading_zeros()) as usize
}

impl EpsFree {
    /// Reduces the automaton without changing what its states stand for in an unfolding.
    /// A state stands for the universal over every word it accepts, and a universal on a
    /// role holds on the role's subroles, so two reductions keep each state's meaning.
    ///
    /// * States that are final alike and have the same transitions to the same states,
    ///   up to this merge, accept the same words and become one state. The merge is the
    ///   coarsest one of this kind.
    /// * With `covered`, a transition by `l` to a state goes when the same source has a
    ///   transition to the same state by a role that `l` is under: every link by `l` is
    ///   a link by that role, so the other transition's universal reaches every element
    ///   this one's would. Of two transitions by equivalent roles the one with the lower
    ///   id stays.
    ///
    /// The two alternate until neither applies, since a dropped transition can make two
    /// states alike. `sub(r, s)` says whether `r` is under `s`, and `sub_cost` is the
    /// steps one call of it takes at most.
    ///
    /// The work is bounded by [`REDUCE_WORK`] steps, counted by the automaton's size and
    /// `sub_cost` alone, so that a run reduces an automaton the same way every time. A
    /// pass that would pass the bound is left out, and the automaton stays as the passes
    /// before it left it, which is a valid automaton for the unfolding at every point. A
    /// long path, whose refinement takes a round for each state, is the case, and so is
    /// a state with thousands of transitions into one target.
    pub fn reduce(&mut self, covered: bool, sub: &dyn Fn(Role, Role) -> bool, sub_cost: usize) {
        self.reduce_within(covered, sub, sub_cost, REDUCE_WORK);
    }

    /// [`EpsFree::reduce`] with `work` steps, and the steps it leaves.
    fn reduce_within(
        &mut self,
        covered: bool,
        sub: &dyn Fn(Role, Role) -> bool,
        sub_cost: usize,
        mut work: usize,
    ) -> usize {
        loop {
            let merged = self.merge_alike(&mut work);
            let dropped = covered && self.drop_covered(sub, sub_cost.max(1), &mut work);
            if !merged && !dropped {
                return work;
            }
        }
    }

    /// Takes `cost` steps from `work` and says whether they were there. A pass that the
    /// bound refuses ends the reduction, so nothing is left for a later one.
    fn charge(work: &mut usize, cost: usize) -> bool {
        if *work < cost {
            *work = 0;
            return false;
        }
        *work -= cost;
        true
    }

    /// Merges the states of the coarsest partition whose blocks are final alike and have
    /// the same transitions into the same blocks, and says whether any merged.
    fn merge_alike(&mut self, work: &mut usize) -> bool {
        let n = self.states as usize;
        let m = self.trans.len();
        // the lists of each state's transitions and the first partition
        if !Self::charge(work, n.saturating_add(m)) {
            return false;
        }
        let mut out: Vec<Vec<(Role, u32)>> = vec![Vec::new(); n];
        for &(p, l, q) in &self.trans {
            out[p as usize].push((l, q));
        }
        // a round builds, sorts and hashes each state's signature
        let round = out.iter().fold(n, |c, o| {
            c.saturating_add(o.len().saturating_mul(log_steps(o.len())))
        });
        // the quotient's sort
        if !Self::charge(work, m.saturating_mul(log_steps(m))) {
            return false;
        }
        let mut block: Vec<u32> = self.finals.iter().map(|&f| f as u32).collect();
        let mut blocks = 0usize;
        loop {
            // a refinement that the bound cuts short is no partition to merge by
            if !Self::charge(work, round) {
                return false;
            }
            // sized for a block of each state, so that it never grows
            let mut ids: FxHashMap<(u32, Vec<(Role, u32)>), u32> =
                FxHashMap::with_capacity_and_hasher(n, Default::default());
            let mut next = Vec::with_capacity(n);
            for s in 0..n {
                let mut sig: Vec<(Role, u32)> = out[s]
                    .iter()
                    .map(|&(l, q)| (l, block[q as usize]))
                    .collect();
                sig.sort_unstable();
                sig.dedup();
                let k = ids.len() as u32;
                next.push(*ids.entry((block[s], sig)).or_insert(k));
            }
            let stable = ids.len() == blocks;
            blocks = ids.len();
            block = next;
            if stable {
                break;
            }
        }
        if blocks == n {
            return false;
        }
        // the blocks in the order of their first states, so that the start stays first
        // when it was
        let mut finals = vec![false; blocks];
        for s in 0..n {
            finals[block[s] as usize] = self.finals[s];
        }
        let mut trans: Vec<(u32, Role, u32)> = self
            .trans
            .iter()
            .map(|&(p, l, q)| (block[p as usize], l, block[q as usize]))
            .collect();
        trans.sort_unstable();
        trans.dedup();
        self.start = block[self.start as usize];
        self.states = blocks as u32;
        self.finals = finals;
        self.trans = trans;
        self.mark();
        true
    }

    /// Drops the transitions that another from the same state to the same target covers
    /// by a superrole, and says whether any went.
    fn drop_covered(
        &mut self,
        sub: &dyn Fn(Role, Role) -> bool,
        sub_cost: usize,
        work: &mut usize,
    ) -> bool {
        let m = self.trans.len();
        // the sort by source and target here, and the sort of what stays
        if !Self::charge(work, m.saturating_mul(log_steps(m)).saturating_mul(2)) {
            return false;
        }
        // a transition is compared with those of its source and target, which are a run
        // of this order
        let mut by: Vec<(u32, u32, Role)> = self.trans.iter().map(|&(p, l, q)| (p, q, l)).collect();
        by.sort_unstable();
        let pairs = by
            .chunk_by(|a, b| (a.0, a.1) == (b.0, b.1))
            .fold(0usize, |c, g| {
                c.saturating_add(g.len().saturating_mul(g.len()))
            });
        // two lookups for a pair
        if !Self::charge(work, pairs.saturating_mul(sub_cost).saturating_mul(2)) {
            return false;
        }
        let mut kept: Vec<(u32, Role, u32)> = Vec::with_capacity(m);
        for g in by.chunk_by(|a, b| (a.0, a.1) == (b.0, b.1)) {
            for &(p, q, l) in g {
                let covered = g
                    .iter()
                    .any(|&(_, _, l2)| l2 != l && sub(l, l2) && !(sub(l2, l) && l < l2));
                if !covered {
                    kept.push((p, l, q));
                }
            }
        }
        if kept.len() == m {
            return false;
        }
        kept.sort_unstable();
        self.trans = kept;
        self.mark();
        true
    }

    /// Drops the transitions by roles that `live` does not hold of, the transitions into
    /// states that reach no final state, and the states the start no longer reaches.
    /// `live(l)` says whether a universal on `l` can apply to anything, which it cannot
    /// when no link or loop of the run is under `l` or under its inverse. A universal by
    /// such a role is vacuous, and a state that reaches no final state stands for a
    /// universal with no consequence. The roles of the dropped transitions go to `dead`.
    pub fn prune(&mut self, live: &dyn Fn(Role) -> bool, dead: &mut Vec<Role>) {
        let n = self.states as usize;
        self.trans.retain(|&(_, l, _)| {
            let keep = live(l);
            if !keep {
                dead.push(l);
            }
            keep
        });
        // the states that reach a final state, from the final states against the
        // transitions
        let mut into: Vec<Vec<u32>> = vec![Vec::new(); n];
        for &(p, _, q) in &self.trans {
            into[q as usize].push(p);
        }
        let mut useful = self.finals.clone();
        let mut todo: Vec<u32> = (0..n as u32).filter(|&s| useful[s as usize]).collect();
        while let Some(q) = todo.pop() {
            for &p in &into[q as usize] {
                if !useful[p as usize] {
                    useful[p as usize] = true;
                    todo.push(p);
                }
            }
        }
        self.trans.retain(|&(_, _, q)| useful[q as usize]);
        // the states the start reaches, numbered in the order a search from it finds
        // them; the transitions are sorted by source
        let mut id: Vec<Option<u32>> = vec![None; n];
        let mut order = vec![self.start];
        id[self.start as usize] = Some(0);
        let mut at = 0;
        while at < order.len() {
            let p = order[at];
            let from = self.trans.partition_point(|t| t.0 < p);
            for &(_, _, q) in self.trans[from..].iter().take_while(|t| t.0 == p) {
                if id[q as usize].is_none() {
                    id[q as usize] = Some(order.len() as u32);
                    order.push(q);
                }
            }
            at += 1;
        }
        self.finals = order.iter().map(|&s| self.finals[s as usize]).collect();
        self.trans = self
            .trans
            .iter()
            .filter_map(|&(p, l, q)| Some((id[p as usize]?, l, id[q as usize]?)))
            .collect();
        self.trans.sort_unstable();
        self.start = 0;
        self.states = order.len() as u32;
        self.mark();
    }

    /// Marks the states that a transition enters and those that one leaves.
    fn mark(&mut self) {
        let n = self.states as usize;
        self.entered = vec![false; n];
        self.left = vec![false; n];
        for &(p, _, q) in &self.trans {
            self.left[p as usize] = true;
            self.entered[q as usize] = true;
        }
    }
}

impl Nfa {
    fn state(&mut self) -> u32 {
        self.states += 1;
        self.states - 1
    }

    /// Adds a copy of `sub` between `from` and `to`.
    fn embed(&mut self, from: u32, sub: &Nfa, to: u32) {
        let off = self.states;
        self.states += sub.states;
        self.trans.push((from, None, sub.start + off));
        self.trans.push((sub.end + off, None, to));
        self.trans
            .extend(sub.trans.iter().map(|&(a, l, b)| (a + off, l, b + off)));
    }
}

/// The role box of a Horn run: the closure of the role inclusions under inverses, the
/// chains with their mirror images, which roles are simple and the automata.
pub(super) struct RoleBox {
    sup: Vec<Vec<Role>>,
    chains: Vec<(Vec<Role>, Role)>,
    simple: Vec<bool>,
    pub(super) autos: FxHashMap<Role, Nfa>,
    states: usize,
}

impl RoleBox {
    /// The steps one call of [`RoleBox::sub`] takes at most, a search in the longest
    /// superrole list, for the bound of [`EpsFree::reduce`].
    pub(super) fn sub_cost(&self) -> usize {
        log_steps(self.sup.iter().map(Vec::len).max().unwrap_or(0))
    }

    pub(super) fn sub(&self, r: Role, s: Role) -> bool {
        r == s
            || self
                .sup
                .get(r as usize)
                .is_some_and(|v| v.binary_search(&s).is_ok())
    }

    fn equiv(&self, r: Role, s: Role) -> bool {
        self.sub(r, s) && self.sub(s, r)
    }

    pub(super) fn simple(&self, r: Role) -> bool {
        self.simple.get(r as usize).copied().unwrap_or(true)
    }

    /// The role box, or why it stopped: its closure passed `limit` bytes, or `stop`
    /// said the run's deadline or cancellation came, as the index's closure of the roles
    /// checks.
    pub(super) fn new(
        it: &Interner,
        role_subs: &[(Role, Role)],
        chains: &[(Vec<Role>, Role)],
        limit: Option<usize>,
        stop: &dyn Fn() -> bool,
    ) -> Result<RoleBox, Halt> {
        let n = it.roles.len();
        let inv = inverses(it);
        let mut direct: Vec<Vec<Role>> = vec![Vec::new(); n];
        for &(r, s) in role_subs {
            direct[r as usize].push(s);
        }
        let mut bytes = 0usize;
        let mut work = 0usize;
        let mut sup: Vec<Vec<Role>> = Vec::with_capacity(n);
        for r in 0..n as Role {
            let mut seen: BTreeSet<Role> = BTreeSet::new();
            let mut stack = vec![r];
            while let Some(x) = stack.pop() {
                work += 1;
                if work.is_multiple_of(POLL_WORK) && stop() {
                    return Err(Halt::Stopped);
                }
                if seen.insert(x) {
                    stack.extend(direct[x as usize].iter().copied());
                }
            }
            bytes += 48 + seen.len() * 8;
            if limit.is_some_and(|l| bytes > l) {
                return Err(Halt::Memory(bytes));
            }
            sup.push(seen.into_iter().collect());
        }
        let mut all: Vec<(Vec<Role>, Role)> = Vec::with_capacity(2 * chains.len());
        for (ops, t) in chains {
            all.push((ops.clone(), *t));
            let mirror: Option<Vec<Role>> = ops
                .iter()
                .rev()
                .map(|&o| Some(inv[o as usize]).filter(|&i| i != NO_ROLE))
                .collect();
            if let (Some(m), Some(i)) = (mirror, Some(inv[*t as usize]).filter(|&i| i != NO_ROLE)) {
                all.push((m, i));
            }
        }
        all.sort();
        all.dedup();
        let mut simple = vec![true; n];
        for (_, t) in &all {
            for &s in &sup[*t as usize] {
                simple[s as usize] = false;
            }
        }
        Ok(RoleBox {
            sup,
            chains: all,
            simple,
            autos: FxHashMap::default(),
            states: 0,
        })
    }

    /// The automaton of a role, whose words are the role paths that imply it, built as
    /// Horrocks, Kutz and Sattler do. `None` when the role inclusions are not regular or
    /// the automata pass [`MAX_STATES`].
    pub(super) fn auto(&mut self, r: Role, stack: &mut Vec<Role>) -> Option<Nfa> {
        if let Some(a) = self.autos.get(&r) {
            return Some(a.clone());
        }
        if stack.iter().any(|&s| self.equiv(s, r)) {
            return None;
        }
        stack.push(r);
        let mut a = Nfa::default();
        a.start = a.state();
        a.end = a.state();
        a.trans.push((a.start, Some(r), a.end));
        let chains: Vec<(Vec<Role>, Role)> = self
            .chains
            .iter()
            .filter(|(_, t)| self.sub(*t, r))
            .cloned()
            .collect();
        let mut below: Vec<Role> = Vec::new();
        for (ops, t) in chains {
            if !self.equiv(t, r) {
                // a chain under a role strictly below r: the automaton of that role
                if !below.contains(&t) {
                    below.push(t);
                    let sub = self.auto(t, stack)?;
                    a.embed(a.start, &sub, a.end);
                }
                continue;
            }
            let n = ops.len();
            let first = self.equiv(ops[0], r);
            let last = self.equiv(ops[n - 1], r);
            let (from, mid, to) = match (first, last) {
                (true, true) if n == 2 => {
                    a.trans.push((a.end, None, a.start));
                    continue;
                }
                (true, true) => return None,
                (true, false) => (a.end, &ops[1..], a.end),
                (false, true) => (a.start, &ops[..n - 1], a.start),
                (false, false) => (a.start, &ops[..], a.end),
            };
            let mut cur = from;
            for (k, &op) in mid.iter().enumerate() {
                let next = if k + 1 == mid.len() { to } else { a.state() };
                if self.simple(op) {
                    a.trans.push((cur, Some(op), next));
                } else {
                    let sub = self.auto(op, stack)?;
                    a.embed(cur, &sub, next);
                }
                cur = next;
            }
        }
        stack.pop();
        self.states += a.states as usize;
        if self.states > MAX_STATES {
            return None;
        }
        self.autos.insert(r, a.clone());
        Some(a)
    }
}

/// The inverse of each role of the interner, [`NO_ROLE`] for a role without one.
pub fn inverses(it: &Interner) -> Vec<Role> {
    let mut inv = vec![NO_ROLE; it.roles.len()];
    for (r, name) in it.roles.iter().enumerate() {
        let other = match name {
            RoleName::Object(p) => it.role_id(&RoleName::Inverse(p.clone())),
            RoleName::Inverse(p) => it.role_id(&RoleName::Object(p.clone())),
            _ => None,
        };
        if let Some(o) = other {
            inv[r] = o;
        }
    }
    inv
}

/// Why an axiom stays left out.
type Why = &'static str;

/// What one axiom adds, kept apart until the whole axiom is normalized.
#[derive(Default)]
struct Out {
    subs: Vec<(Ex, Ex)>,
    disjoint: Vec<Vec<Ex>>,
    ranges: Vec<(Role, Ex)>,
    role_subs: Vec<(Role, Role)>,
    chains: Vec<(Vec<Role>, Role)>,
    reflexive: Vec<Role>,
    clash: Vec<(Role, Role)>,
    premises: Vec<Ex>,
}

impl Out {
    fn commit(self, ax: &mut Axioms, h: &mut HornAx) {
        ax.subs.extend(self.subs);
        ax.disjoint.extend(self.disjoint);
        ax.ranges.extend(self.ranges);
        ax.role_subs.extend(self.role_subs);
        ax.chains.extend(self.chains);
        ax.reflexive.extend(self.reflexive);
        h.clash.extend(self.clash);
        h.premises.extend(self.premises);
    }
}

struct Norm<'a> {
    it: &'a mut Interner,
    rb: Option<&'a RoleBox>,
    out: Out,
    /// whether the ontology has individuals, which leaves at-least restrictions above
    /// one out (F24, "The Horn rules with individuals")
    individuals: bool,
}

impl Norm<'_> {
    /// The role of an object property expression, with its inverse interned beside it.
    fn role(&mut self, p: &ObjectPropertyExpression) -> Result<Role, Why> {
        let named = p.named();
        if named.as_str() == vocab::OWL_BOTTOM_OBJECT_PROPERTY {
            return Err("the bottom object property");
        }
        let r = self.it.role(RoleName::Object(named.clone()));
        let i = self.it.role(RoleName::Inverse(named.clone()));
        Ok(match p {
            ObjectPropertyExpression::Property(_) => r,
            ObjectPropertyExpression::InverseOf(_) => i,
        })
    }

    /// The role of an expression that OWL 2 DL requires to be simple.
    fn simple_role(&mut self, p: &ObjectPropertyExpression) -> Result<Role, Why> {
        if is_top(p) {
            return Err("the top object property where OWL 2 DL needs a simple one");
        }
        let r = self.role(p)?;
        match self.rb {
            Some(rb) if !rb.simple(r) => Err("a role that is not simple where OWL 2 DL needs one"),
            _ => Ok(r),
        }
    }

    fn conj(&mut self, mut es: Vec<Ex>) -> Ex {
        es.sort_unstable();
        es.dedup();
        es.retain(|&e| e != TOP);
        if es.contains(&BOTTOM) {
            return BOTTOM;
        }
        match es.len() {
            0 => TOP,
            1 => es[0],
            _ => {
                let mut acc = es[0];
                for &e in &es[1..] {
                    acc = self.it.conj(acc, e);
                }
                acc
            }
        }
    }

    /// An expression of the EL index, for data restrictions inside a Horn axiom.
    fn el(&mut self, c: &ClassExpression) -> Result<Ex, Why> {
        match class_expression(c, self.it, 0)? {
            (e, None) => Ok(e),
            (_, Some(why)) => Err(why),
        }
    }

    /// `x` with `x ⊑ c`.
    fn pos(&mut self, c: &ClassExpression, depth: usize) -> Result<Ex, Why> {
        use ClassExpression as C;
        if depth > 256 {
            return Err("class expressions nested too deeply");
        }
        let d = depth + 1;
        Ok(match c {
            C::Class(c) => self.it.class(c),
            C::ObjectIntersectionOf(cs) => {
                let es = cs
                    .iter()
                    .map(|c| self.pos(c, d))
                    .collect::<Result<_, _>>()?;
                self.conj(es)
            }
            C::ObjectUnionOf(cs) => {
                let mut es: Vec<Ex> = cs
                    .iter()
                    .map(|c| self.pos(c, d))
                    .collect::<Result<_, _>>()?;
                if es.contains(&TOP) {
                    return Ok(TOP);
                }
                es.retain(|&e| e != BOTTOM);
                es.sort_unstable();
                es.dedup();
                match es.len() {
                    0 => BOTTOM,
                    1 => es[0],
                    _ => return Err("a disjunction in a positive position"),
                }
            }
            C::ObjectComplementOf(f) => {
                let g = self.neg(f, d)?;
                match g {
                    TOP => BOTTOM,
                    BOTTOM => TOP,
                    _ => {
                        let x = self.it.fresh_class();
                        let both = self.it.conj(x, g);
                        self.out.subs.push((both, BOTTOM));
                        x
                    }
                }
            }
            C::ObjectSomeValuesFrom(p, f) => {
                let r = self.role(p)?;
                let f = self.pos(f, d)?;
                if f == BOTTOM {
                    BOTTOM
                } else {
                    self.it.node(Node::Exist(r, f))
                }
            }
            C::ObjectAllValuesFrom(p, f) => {
                let r = self.role(p)?;
                let f = self.pos(f, d)?;
                if f == TOP {
                    TOP
                } else {
                    self.it.node(Node::Forall(r, f))
                }
            }
            C::ObjectHasSelf(p) => {
                self.simple_role(p)?;
                let r = self.role(&ObjectPropertyExpression::Property(p.named().clone()))?;
                self.it.node(Node::SelfLoop(r))
            }
            C::ObjectMinCardinality(n, p, f) => self.at_least(*n, p, f.as_deref(), d)?,
            C::ObjectMaxCardinality(n, p, f) => self.at_most(*n, p, f.as_deref(), d)?,
            C::ObjectExactCardinality(n, p, f) => {
                let a = self.at_least(*n, p, f.as_deref(), d)?;
                let b = self.at_most(*n, p, f.as_deref(), d)?;
                self.conj(vec![a, b])
            }
            C::ObjectOneOf(is) if is.len() == 1 => self.el(c)?,
            C::ObjectOneOf(_) => {
                return Err("an enumeration of several individuals in a positive position");
            }
            C::ObjectHasValue(p, a) => self.has_value(p, a)?,
            C::DataSomeValuesFrom(..) | C::DataHasValue(..) => self.el(c)?,
            _ => return Err("a data restriction outside OWL 2 EL"),
        })
    }

    /// `∃p.{a}`, with `p` an object property or its inverse.
    fn has_value(&mut self, p: &ObjectPropertyExpression, a: &Individual) -> Result<Ex, Why> {
        let r = self.role(p)?;
        let n = self.el(&ClassExpression::ObjectOneOf(
            std::iter::once(a.clone()).collect(),
        ))?;
        Ok(self.it.node(Node::Exist(r, n)))
    }

    fn at_least(
        &mut self,
        n: u32,
        p: &ObjectPropertyExpression,
        f: Option<&ClassExpression>,
        d: usize,
    ) -> Result<Ex, Why> {
        if n == 0 {
            return Ok(TOP);
        }
        if n > 1 && self.individuals {
            // Lemma 2's fresh disjoint fillers are argued for tree-shaped models, and a
            // nominal is a shared successor (roadmap item 867)
            return Err("an at-least restriction above one beside individuals");
        }
        let r = if n == 1 {
            self.role(p)?
        } else {
            self.simple_role(p)?
        };
        let f = match f {
            Some(f) => self.pos(f, d)?,
            None => TOP,
        };
        if f == BOTTOM {
            return Ok(BOTTOM);
        }
        if n == 1 {
            return Ok(self.it.node(Node::Exist(r, f)));
        }
        if n > MAX_AT_LEAST {
            return Err("a minimum cardinality above the limit the normalization unfolds");
        }
        // n distinct successors: fresh, pairwise disjoint subclasses of the filler
        let mut parts = Vec::with_capacity(n as usize);
        let mut fresh = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let b = self.it.fresh_class();
            if f != TOP {
                self.out.subs.push((b, f));
            }
            fresh.push(b);
            parts.push(self.it.node(Node::Exist(r, b)));
        }
        fresh.sort_unstable();
        self.out.disjoint.push(fresh);
        Ok(self.conj(parts))
    }

    fn at_most(
        &mut self,
        n: u32,
        p: &ObjectPropertyExpression,
        f: Option<&ClassExpression>,
        d: usize,
    ) -> Result<Ex, Why> {
        let r = self.simple_role(p)?;
        let g = match f {
            Some(f) => self.neg(f, d)?,
            None => TOP,
        };
        if g == BOTTOM {
            return Ok(TOP);
        }
        match n {
            0 => {
                // ≤0 r.F is ¬∃r.F
                let x = self.it.fresh_class();
                let e = self.it.node(Node::Exist(r, g));
                let both = self.it.conj(x, e);
                self.out.subs.push((both, BOTTOM));
                Ok(x)
            }
            1 => {
                self.out.premises.push(g);
                Ok(self.it.node(Node::AtMost1(r, g)))
            }
            _ => Err("a maximum cardinality above one in a positive position"),
        }
    }

    /// `y` with `c ⊑ y`.
    fn neg(&mut self, c: &ClassExpression, depth: usize) -> Result<Ex, Why> {
        use ClassExpression as C;
        if depth > 256 {
            return Err("class expressions nested too deeply");
        }
        let d = depth + 1;
        Ok(match c {
            C::Class(c) => self.it.class(c),
            C::ObjectIntersectionOf(cs) => {
                let es = cs
                    .iter()
                    .map(|c| self.neg(c, d))
                    .collect::<Result<_, _>>()?;
                self.conj(es)
            }
            C::ObjectUnionOf(cs) => {
                let mut es: Vec<Ex> = cs
                    .iter()
                    .map(|c| self.neg(c, d))
                    .collect::<Result<_, _>>()?;
                if es.contains(&TOP) {
                    return Ok(TOP);
                }
                es.retain(|&e| e != BOTTOM);
                es.sort_unstable();
                es.dedup();
                match es.len() {
                    0 => BOTTOM,
                    1 => es[0],
                    _ => {
                        let x = self.it.fresh_class();
                        for e in es {
                            self.out.subs.push((e, x));
                        }
                        x
                    }
                }
            }
            C::ObjectSomeValuesFrom(p, f) => {
                let r = self.role(p)?;
                let g = self.neg(f, d)?;
                if g == BOTTOM {
                    BOTTOM
                } else {
                    self.it.node(Node::Exist(r, g))
                }
            }
            C::ObjectMinCardinality(0, ..) => TOP,
            C::ObjectMinCardinality(1, p, f) => {
                let r = self.role(p)?;
                let g = match f {
                    Some(f) => self.neg(f, d)?,
                    None => TOP,
                };
                if g == BOTTOM {
                    BOTTOM
                } else {
                    self.it.node(Node::Exist(r, g))
                }
            }
            C::ObjectHasSelf(p) => {
                self.simple_role(p)?;
                let r = self.role(&ObjectPropertyExpression::Property(p.named().clone()))?;
                self.it.node(Node::SelfLoop(r))
            }
            C::ObjectOneOf(is) if is.len() == 1 => self.el(c)?,
            C::ObjectOneOf(is) => {
                // the union of the singletons: a fresh class above each of them
                let x = self.it.fresh_class();
                for a in is {
                    let n = self.el(&C::ObjectOneOf(std::iter::once(a.clone()).collect()))?;
                    self.out.subs.push((n, x));
                }
                x
            }
            C::ObjectHasValue(p, a) => self.has_value(p, a)?,
            C::DataSomeValuesFrom(..) | C::DataHasValue(..) => self.el(c)?,
            C::DataAllValuesFrom(..)
            | C::DataMinCardinality(..)
            | C::DataMaxCardinality(..)
            | C::DataExactCardinality(..) => return Err("a data restriction outside OWL 2 EL"),
            _ => return Err("a class expression that is not Horn in a negative position"),
        })
    }

    /// `⊓lhs ⊑ ⊔rhs`, Horn once the duals of what cannot stand on its side have moved.
    fn sub(&mut self, lhs: Vec<ClassExpression>, rhs: Vec<ClassExpression>) -> Result<(), Why> {
        use ClassExpression as C;
        let mut left: Vec<ClassExpression> = Vec::new();
        let mut right: Vec<ClassExpression> = Vec::new();
        let mut todo_l = lhs;
        let mut todo_r = rhs;
        let mut steps = 0usize;
        while !todo_l.is_empty() || !todo_r.is_empty() {
            steps += 1;
            if steps > 4096 {
                return Err("class expressions nested too deeply");
            }
            if let Some(c) = todo_l.pop() {
                match c {
                    C::ObjectIntersectionOf(cs) => todo_l.extend(cs),
                    C::ObjectComplementOf(f) => todo_r.push(*f),
                    C::ObjectAllValuesFrom(p, f) => todo_r.push(C::ObjectSomeValuesFrom(
                        p,
                        Box::new(C::ObjectComplementOf(f)),
                    )),
                    C::ObjectMaxCardinality(n, p, f) => {
                        let Some(m) = n.checked_add(1) else {
                            return Err("a cardinality too large");
                        };
                        todo_r.push(C::ObjectMinCardinality(m, p, f))
                    }
                    C::ObjectMinCardinality(n, p, f) if n >= 2 => {
                        todo_r.push(C::ObjectMaxCardinality(n - 1, p, f))
                    }
                    C::Class(ref k) if k.as_str() == vocab::OWL_THING => {}
                    c => left.push(c),
                }
                continue;
            }
            if let Some(c) = todo_r.pop() {
                match c {
                    C::ObjectUnionOf(cs) => todo_r.extend(cs),
                    C::ObjectComplementOf(f) => todo_l.push(*f),
                    C::Class(ref k) if k.as_str() == vocab::OWL_NOTHING => {}
                    c => right.push(c),
                }
            }
        }
        let y = {
            let es = left
                .iter()
                .map(|c| self.neg(c, 0))
                .collect::<Result<Vec<_>, _>>()?;
            self.conj(es)
        };
        let mut xs = right
            .iter()
            .map(|c| self.pos(c, 0))
            .collect::<Result<Vec<_>, _>>()?;
        if xs.contains(&TOP) || y == BOTTOM {
            return Ok(());
        }
        xs.retain(|&x| x != BOTTOM);
        xs.sort_unstable();
        xs.dedup();
        let x = match xs.len() {
            0 => BOTTOM,
            1 => xs[0],
            _ => return Err("a disjunction in a positive position"),
        };
        if x != y {
            self.out.subs.push((y, x));
        }
        Ok(())
    }

    /// Whether an axiom is a role axiom that needs no simple roles.
    fn is_role_axiom(ax: &AxiomKind) -> bool {
        use AxiomKind as K;
        matches!(
            ax,
            K::SubObjectPropertyOf(..)
                | K::EquivalentObjectProperties(_)
                | K::InverseObjectProperties(..)
                | K::SymmetricObjectProperty(_)
                | K::TransitiveObjectProperty(_)
                | K::ReflexiveObjectProperty(_)
        )
    }

    /// A role axiom of [`Norm::is_role_axiom`]. An inclusion under the top property is a
    /// tautology and is taken as nothing, and the top property elsewhere in a role axiom
    /// stays out: a chain through it is partial (roadmap item 663), and its inverse,
    /// transitivity, symmetry and reflexivity hold already.
    fn role_axiom(&mut self, ax: &AxiomKind) -> Result<(), Why> {
        use AxiomKind as K;
        match ax {
            K::SubObjectPropertyOf(_, s) if is_top(s) => return Ok(()),
            K::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(p), s) => {
                let (r, s) = (self.role(p)?, self.role(s)?);
                self.out.role_subs.push((r, s));
            }
            K::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), _)
                if ps.iter().any(is_top) =>
            {
                return Err("a property chain through the top property");
            }
            K::EquivalentObjectProperties(ps) if ps.iter().any(is_top) => {
                return Err("the top property in a property equivalence");
            }
            K::InverseObjectProperties(p, q) if is_top(p) || is_top(q) => {
                return Err("the top property in an inverse property axiom");
            }
            K::SymmetricObjectProperty(p)
            | K::TransitiveObjectProperty(p)
            | K::ReflexiveObjectProperty(p)
                if is_top(p) =>
            {
                return Err("a characteristic the top property has already");
            }
            K::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), s) => {
                let s = self.role(s)?;
                let rs = ps
                    .iter()
                    .map(|p| self.role(p))
                    .collect::<Result<Vec<_>, _>>()?;
                match rs.len() {
                    0 => {}
                    1 => self.out.role_subs.push((rs[0], s)),
                    _ => self.out.chains.push((rs, s)),
                }
            }
            K::EquivalentObjectProperties(ps) => {
                let rs = ps
                    .iter()
                    .map(|p| self.role(p))
                    .collect::<Result<Vec<_>, _>>()?;
                for w in 0..rs.len() {
                    self.out.role_subs.push((rs[w], rs[(w + 1) % rs.len()]));
                }
            }
            K::InverseObjectProperties(p, q) => {
                // p is q⁻, both ways
                let (p, q) = (self.role(p)?, self.role(&q.inverse())?);
                self.out.role_subs.push((p, q));
                self.out.role_subs.push((q, p));
            }
            K::SymmetricObjectProperty(p) => {
                let (r, i) = (self.role(p)?, self.role(&p.inverse())?);
                self.out.role_subs.push((r, i));
            }
            K::TransitiveObjectProperty(p) => {
                let r = self.role(p)?;
                self.out.chains.push((vec![r, r], r));
            }
            K::ReflexiveObjectProperty(p) => {
                let r = self.role(&ObjectPropertyExpression::Property(p.named().clone()))?;
                self.out.reflexive.push(r);
            }
            _ => return Err("not a role axiom"),
        }
        Ok(())
    }

    /// A class axiom or a role axiom that needs simple roles.
    fn axiom(&mut self, ax: &AxiomKind) -> Result<(), Why> {
        use AxiomKind as K;
        use ClassExpression as C;
        let one = |a: &Individual| C::ObjectOneOf(std::iter::once(a.clone()).collect());
        match ax {
            K::SubClassOf(c, d) => self.sub(vec![c.clone()], vec![d.clone()]),
            // the assertions with a Horn class expression or an inverse property, as the
            // class axioms they are: `{a} ⊑ C`, `{a} ⊑ ∃p.{b}` and `{a} ⊓ ∃p.{b} ⊑ ⊥`
            K::ClassAssertion(c, a) => self.sub(vec![one(a)], vec![c.clone()]),
            K::ObjectPropertyAssertion(p, a, b) => {
                self.sub(vec![one(a)], vec![C::ObjectHasValue(p.clone(), b.clone())])
            }
            K::NegativeObjectPropertyAssertion(p, a, b) => self.sub(
                vec![one(a), C::ObjectHasValue(p.clone(), b.clone())],
                vec![],
            ),
            K::EquivalentClasses(cs) => {
                let cs: Vec<&ClassExpression> = cs.iter().collect();
                for w in 0..cs.len() {
                    let next = cs[(w + 1) % cs.len()];
                    if cs[w] != next {
                        self.sub(vec![cs[w].clone()], vec![next.clone()])?;
                    }
                }
                Ok(())
            }
            K::DisjointClasses(cs) => {
                let cs: Vec<&ClassExpression> = cs.iter().collect();
                // operands with a negative form make one disjointness set
                if let Ok(mut es) = cs
                    .iter()
                    .map(|c| self.neg(c, 0))
                    .collect::<Result<Vec<_>, _>>()
                {
                    es.sort_unstable();
                    for w in es.windows(2) {
                        if w[0] == w[1] {
                            self.out.subs.push((w[0], BOTTOM));
                        }
                    }
                    es.dedup();
                    es.retain(|&e| e != BOTTOM);
                    if es.len() >= 2 {
                        self.out.disjoint.push(es);
                    }
                    return Ok(());
                }
                for i in 0..cs.len() {
                    for j in i + 1..cs.len() {
                        self.sub(vec![cs[i].clone(), cs[j].clone()], vec![])?;
                    }
                }
                Ok(())
            }
            K::DisjointUnion(c, ds) if ds.len() <= 1 => {
                let d = ds
                    .iter()
                    .next()
                    .cloned()
                    .unwrap_or(C::Class(Class::nothing()));
                let c = C::Class(c.clone());
                self.sub(vec![c.clone()], vec![d.clone()])?;
                self.sub(vec![d], vec![c])
            }
            K::ObjectPropertyDomain(p, c) => self.sub(
                vec![C::ObjectSomeValuesFrom(
                    p.clone(),
                    Box::new(C::Class(Class::thing())),
                )],
                vec![c.clone()],
            ),
            K::ObjectPropertyRange(p, c) => {
                let r = self.role(p)?;
                let c = self.pos(c, 0)?;
                if c != TOP {
                    self.out.ranges.push((r, c));
                }
                Ok(())
            }
            K::DataPropertyDomain(d, c) => self.sub(
                vec![C::DataSomeValuesFrom(
                    vec![d.clone()],
                    DataRange::Datatype(Datatype::new(vocab::RDFS_LITERAL)),
                )],
                vec![c.clone()],
            ),
            K::FunctionalObjectProperty(p) => {
                let r = self.simple_role(p)?;
                let e = self.it.node(Node::AtMost1(r, TOP));
                self.out.premises.push(TOP);
                self.out.subs.push((TOP, e));
                Ok(())
            }
            K::InverseFunctionalObjectProperty(p) => {
                let r = self.simple_role(&p.inverse())?;
                let e = self.it.node(Node::AtMost1(r, TOP));
                self.out.premises.push(TOP);
                self.out.subs.push((TOP, e));
                Ok(())
            }
            K::IrreflexiveObjectProperty(p) => {
                self.simple_role(p)?;
                let r = self.role(&ObjectPropertyExpression::Property(p.named().clone()))?;
                let e = self.it.node(Node::SelfLoop(r));
                self.out.subs.push((e, BOTTOM));
                Ok(())
            }
            K::AsymmetricObjectProperty(p) => {
                let r = self.simple_role(p)?;
                let i = self.simple_role(&p.inverse())?;
                self.out.clash.push((r, i));
                self.out.clash.push((i, r));
                Ok(())
            }
            K::DisjointObjectProperties(ps) => {
                let rs = ps
                    .iter()
                    .map(|p| self.simple_role(p))
                    .collect::<Result<Vec<_>, _>>()?;
                for &a in &rs {
                    for &b in &rs {
                        if a != b {
                            self.out.clash.push((a, b));
                        }
                    }
                }
                Ok(())
            }
            _ => Err("an axiom the Horn rules do not take"),
        }
    }
}

/// Whether the axiom is an assertion about an anonymous individual. The rules have no
/// context for an anonymous individual, so the normalization refuses every such axiom,
/// and the caller leaves it out and reports it. Finding it here spares the axiom's scope
/// and the attempt (roadmap item 869).
fn anonymous(ax: &AxiomKind) -> bool {
    use AxiomKind as K;
    let anon = |a: &Individual| matches!(a, Individual::Anonymous(_));
    match ax {
        K::ClassAssertion(_, a)
        | K::DataPropertyAssertion(_, a, _)
        | K::NegativeDataPropertyAssertion(_, a, _) => anon(a),
        K::ObjectPropertyAssertion(_, a, b) | K::NegativeObjectPropertyAssertion(_, a, b) => {
            anon(a) || anon(b)
        }
        _ => false,
    }
}

/// Whether the expression is the top object property or its inverse.
fn is_top(p: &ObjectPropertyExpression) -> bool {
    p.named().as_str() == vocab::OWL_TOP_OBJECT_PROPERTY
}

/// Normalizes the axioms outside OWL 2 EL that the Horn rules can take, after the EL
/// axioms are in `ax`. The answer says which were taken. Nothing is taken when the role
/// inclusions are not regular, their automata are too large or no axiom is Horn, and
/// then `ax` is as it was, the interner aside. The closure of the roles counts against
/// `limit` and polls `stop`, as the index's does.
pub fn normalize(
    axioms: &[&AxiomKind],
    it: &mut Interner,
    ax: &mut Axioms,
    limit: Option<usize>,
    stop: &dyn Fn() -> bool,
) -> Result<Vec<bool>, Halt> {
    let mut taken = vec![false; axioms.len()];
    let mut h = HornAx::default();
    // whether the run has individuals: those the EL axioms interned, and those the axioms
    // here name
    let individuals = !it.individuals.is_empty()
        || axioms.iter().any(|k| {
            Axiom::from((*k).clone())
                .signature()
                .iter()
                .any(|e| matches!(e, Entity::NamedIndividual(_)))
        });
    // every object property has its inverse
    let named: Vec<ObjectProperty> = it
        .roles
        .iter()
        .filter_map(|n| match n {
            RoleName::Object(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    for p in named {
        it.role(RoleName::Inverse(p));
    }
    // the role axioms first, so that the role box is whole before the class axioms ask
    // which roles are simple
    let mut roles = Out::default();
    let mut role_taken = Vec::new();
    for (i, k) in axioms.iter().enumerate() {
        if !Norm::is_role_axiom(k) {
            continue;
        }
        let scope = Scope::Horn(it.scope_axiom(k));
        it.begin_scope(scope);
        let mut n = Norm {
            it,
            rb: None,
            out: Out::default(),
            individuals,
        };
        let accepted = n.role_axiom(k).is_ok();
        n.it.end_scope();
        if accepted {
            let o = n.out;
            roles.role_subs.extend(o.role_subs);
            roles.chains.extend(o.chains);
            roles.reflexive.extend(o.reflexive);
            role_taken.push(i);
        }
    }
    let mut role_subs: Vec<(Role, Role)> = ax.role_subs.clone();
    role_subs.extend(roles.role_subs.iter().copied());
    // the universal role is its own inverse, also when a class expression here is the
    // first to name it, since the role box is built before the class axioms
    let names_top = it
        .role_id(&RoleName::Object(ObjectProperty::top()))
        .is_some()
        || axioms.iter().any(|k| {
            Axiom::from((*k).clone()).signature().iter().any(|e| {
                matches!(e, Entity::ObjectProperty(p) if p.as_str() == vocab::OWL_TOP_OBJECT_PROPERTY)
            })
        });
    if names_top {
        let t = it.role(RoleName::Object(ObjectProperty::top()));
        let ti = it.role(RoleName::Inverse(ObjectProperty::top()));
        role_subs.push((t, ti));
        role_subs.push((ti, t));
    }
    let inv = inverses(it);
    let mirror = |subs: &mut Vec<(Role, Role)>| {
        let n = subs.len();
        for k in 0..n {
            let (r, s) = subs[k];
            if let (Some(&a), Some(&b)) = (inv.get(r as usize), inv.get(s as usize))
                && a != NO_ROLE
                && b != NO_ROLE
            {
                subs.push((a, b));
            }
        }
        subs.sort_unstable();
        subs.dedup();
    };
    mirror(&mut role_subs);
    let mut chains = ax.chains.clone();
    chains.extend(roles.chains.iter().cloned());
    let mut rb = RoleBox::new(it, &role_subs, &chains, limit, stop)?;
    let nonsimple: Vec<Role> = (0..rb.simple.len() as Role)
        .filter(|&r| !rb.simple(r))
        .collect();
    for r in nonsimple {
        if rb.auto(r, &mut Vec::new()).is_none() {
            return Ok(vec![false; axioms.len()]);
        }
    }
    for i in role_taken {
        taken[i] = true;
    }
    roles.role_subs = role_subs;
    // the class axioms and the role axioms that need simple roles, which go to `ax` only
    // once one is taken
    let mut class_out: Vec<Out> = Vec::new();
    for (i, k) in axioms.iter().enumerate() {
        if taken[i] || Norm::is_role_axiom(k) || anonymous(k) {
            continue;
        }
        // the fresh classes of an axiom are those of its last run (roadmap item 753)
        let scope = Scope::Horn(it.scope_axiom(k));
        it.begin_scope(scope);
        let mut n = Norm {
            it,
            rb: Some(&rb),
            out: Out::default(),
            individuals,
        };
        let accepted = n.axiom(k).is_ok();
        n.it.end_scope();
        n.it.ranges.check_halt()?;
        if accepted {
            class_out.push(n.out);
            taken[i] = true;
        }
    }
    if !taken.contains(&true) {
        return Ok(taken);
    }
    ax.role_subs.clear();
    for o in class_out {
        o.commit(ax, &mut h);
    }
    roles.commit(ax, &mut h);
    // a domain of r is a range of r⁻, which the successors of r⁻ take at once
    let inv = inverses(it);
    let domains: Vec<(Role, Ex)> = ax
        .subs
        .iter()
        .filter_map(|&(a, c)| match it.nodes[a as usize] {
            Node::Exist(r, TOP) => Some((inv[r as usize], c)).filter(|&(i, _)| i != NO_ROLE),
            _ => None,
        })
        .collect();
    ax.ranges.extend(domains);
    // a range of a role that a chain reaches holds along the chain's paths too
    let ranges: Vec<(Role, Ex)> = ax
        .ranges
        .iter()
        .copied()
        .filter(|&(r, _)| !rb.simple(r))
        .collect();
    for (r, c) in ranges {
        let e = it.node(Node::Forall(r, c));
        ax.subs.push((TOP, e));
    }
    h.taken = taken.iter().filter(|t| **t).count();
    h.autos = std::mem::take(&mut rb.autos);
    ax.horn = Some(Box::new(h));
    Ok(taken)
}

#[cfg(test)]
mod reduction_tests {
    use super::*;

    fn auto(states: u32, finals: &[u32], trans: &[(u32, Role, u32)]) -> EpsFree {
        let mut a = EpsFree {
            states,
            start: 0,
            finals: (0..states).map(|s| finals.contains(&s)).collect(),
            trans: trans.to_vec(),
            ..EpsFree::default()
        };
        a.trans.sort_unstable();
        a.mark();
        a
    }

    /// The words of at most `len` roles out of `0..roles` that the automaton accepts.
    fn words(a: &EpsFree, roles: Role, len: usize) -> BTreeSet<Vec<Role>> {
        let mut out = BTreeSet::new();
        let mut at: Vec<(Vec<Role>, BTreeSet<u32>)> = vec![(Vec::new(), [a.start].into())];
        for _ in 0..=len {
            let mut next = Vec::new();
            for (w, states) in at {
                if states.iter().any(|&s| a.finals[s as usize]) {
                    out.insert(w.clone());
                }
                for l in 0..roles {
                    let to: BTreeSet<u32> = a
                        .trans
                        .iter()
                        .filter(|t| states.contains(&t.0) && t.1 == l)
                        .map(|t| t.2)
                        .collect();
                    if !to.is_empty() {
                        let mut w = w.clone();
                        w.push(l);
                        next.push((w, to));
                    }
                }
            }
            at = next;
        }
        out
    }

    /// The words that some word of `ws` covers, role by role, by a role above.
    fn under(ws: &BTreeSet<Vec<Role>>, w: &[Role], sub: &dyn Fn(Role, Role) -> bool) -> bool {
        ws.iter()
            .any(|v| v.len() == w.len() && w.iter().zip(v).all(|(&x, &y)| sub(x, y)))
    }

    #[test]
    fn states_that_are_final_alike_with_the_same_transitions_merge() {
        // the shape of a transitive role's automaton after the ε-closure: two final
        // states that both loop into the first
        let mut a = auto(3, &[1, 2], &[(0, 0, 1), (0, 0, 2), (1, 0, 2), (2, 0, 2)]);
        let before = words(&a, 1, 5);
        a.reduce(false, &|r, s| r == s, 1);
        assert_eq!(a.states, 2);
        assert_eq!(a.trans, vec![(0, 0, 1), (1, 0, 1)]);
        assert_eq!(a.finals, vec![false, true]);
        assert_eq!(
            (a.entered.clone(), a.left.clone()),
            (vec![false, true], vec![true, true])
        );
        assert_eq!(words(&a, 1, 5), before);
    }

    #[test]
    fn a_state_that_differs_only_later_does_not_merge() {
        // 1 and 2 are final alike, but 2 goes on by another role
        let mut a = auto(
            4,
            &[3],
            &[(0, 0, 1), (0, 0, 2), (1, 0, 3), (2, 0, 3), (2, 1, 3)],
        );
        let same = a.clone();
        a.reduce(false, &|r, s| r == s, 1);
        assert_eq!(a, same);
    }

    #[test]
    fn a_transition_under_another_to_the_same_state_goes() {
        // role 1 is under role 0, and roles 2 and 3 are equivalent
        let sub = |r: Role, s: Role| r == s || (r, s) == (1, 0) || (r.min(s), r.max(s)) == (2, 3);
        let mut a = auto(
            3,
            &[2],
            &[(0, 0, 1), (0, 1, 1), (0, 1, 2), (1, 2, 2), (1, 3, 2)],
        );
        a.reduce(true, &sub, 1);
        // the transition by 1 to another state stays, and of the equivalent roles the
        // lower one
        assert_eq!(a.trans, vec![(0, 0, 1), (0, 1, 2), (1, 2, 2)]);
    }

    #[test]
    fn a_dropped_transition_lets_states_merge() {
        // without the transition by the subrole, 1 and 2 are alike
        let sub = |r: Role, s: Role| r == s || (r, s) == (1, 0);
        let mut a = auto(
            4,
            &[3],
            &[(0, 0, 1), (0, 0, 2), (1, 0, 3), (2, 0, 3), (2, 1, 3)],
        );
        a.reduce(true, &sub, 1);
        assert_eq!(a.states, 3);
        assert_eq!(a.trans, vec![(0, 0, 1), (1, 0, 2)]);
    }

    #[test]
    fn pruning_drops_dead_roles_and_what_only_they_reach() {
        // 0 -0-> 1 -0-> 3 is alive, 0 -1-> 2 -0-> 3 goes with the dead role 1, and
        // 1 -0-> 4 leads to no final state
        let mut a = auto(
            5,
            &[3],
            &[(0, 0, 1), (0, 1, 2), (1, 0, 3), (2, 0, 3), (1, 0, 4)],
        );
        let mut dead = Vec::new();
        a.prune(&|l| l != 1, &mut dead);
        assert_eq!(dead, vec![1]);
        assert_eq!(a.states, 3);
        assert_eq!(a.trans, vec![(0, 0, 1), (1, 0, 2)]);
        assert_eq!(a.finals, vec![false, false, true]);
        assert_eq!(a.start, 0);
    }

    #[test]
    fn pruning_every_way_to_a_final_state_leaves_the_start_alone() {
        let mut a = auto(2, &[1], &[(0, 0, 1), (1, 0, 1)]);
        let mut dead = Vec::new();
        a.prune(&|_| false, &mut dead);
        assert_eq!((a.states, a.start), (1, 0));
        assert!(a.trans.is_empty());
        assert_eq!(a.finals, vec![false]);
        assert_eq!(
            (a.entered.clone(), a.left.clone()),
            (vec![false], vec![false])
        );
    }

    #[test]
    fn a_long_path_is_left_as_it_is_within_the_bound() {
        // a chain of 40,000 operands: the refinement would take a round for each state,
        // which the bound of the reduction does not allow, and pruning is linear
        let n = 40_000u32;
        let trans: Vec<(u32, Role, u32)> = (0..n).map(|s| (s, s % 2, s + 1)).collect();
        let mut a = auto(n + 1, &[n], &trans);
        let same = a.clone();
        let start = std::time::Instant::now();
        a.reduce(true, &|r, s| r == s, 1);
        assert_eq!(a, same);
        let mut dead = Vec::new();
        a.prune(&|_| true, &mut dead);
        assert_eq!(a, same);
        a.prune(&|l| l == 0, &mut dead);
        assert_eq!((a.states, a.trans.len()), (1, 0));
        assert!(start.elapsed() < std::time::Duration::from_secs(20));
        // a path within the bound has no two states alike either
        let mut b = auto(4, &[3], &[(0, 0, 1), (1, 0, 2), (2, 0, 3)]);
        let same = b.clone();
        b.reduce(true, &|r, s| r == s, 1);
        assert_eq!(b, same);
    }

    #[test]
    fn many_labels_into_one_target_are_left_within_the_bound() {
        // 4,000 roles from one state into one target: sixteen million pairs with two
        // lookups each pass the bound, so the pass is left out even where it would drop
        // every transition but one
        let n = 4_000u32;
        let trans: Vec<(u32, Role, u32)> = (0..n).map(|l| (0, l, 1)).collect();
        let mut a = auto(2, &[1], &trans);
        let same = a.clone();
        let calls = std::cell::Cell::new(0usize);
        let sub = |_: Role, s: Role| {
            calls.set(calls.get() + 1);
            s == 0
        };
        a.reduce(true, &sub, 1);
        assert_eq!(a, same);
        assert_eq!(calls.get(), 0);
        // within the bound the pass drops them, by the lookups it charged
        let trans: Vec<(u32, Role, u32)> = (0..1_000).map(|l| (0, l, 1)).collect();
        let mut a = auto(2, &[1], &trans);
        a.reduce(true, &sub, 1);
        assert_eq!(a.trans, vec![(0, 0, 1)]);
        assert!(calls.get() <= 2 * 1_000 * 1_000);
        // a hierarchy whose lookups take more steps leaves the same automaton alone
        let mut a = auto(2, &[1], &trans);
        let same = a.clone();
        a.reduce(true, &sub, 10);
        assert_eq!(a, same);
    }

    #[test]
    fn a_state_of_high_degree_charges_its_sorts() {
        // a state with 30,000 transitions into the states of a path of 300, out of their
        // order: the refinement takes a round for each state of the path, and each round
        // charges the sort of the large signature, about 480,000 steps, so the bound
        // ends it after some thirty rounds. A step for each transition would let all
        // 300 rounds run.
        let (fan, path) = (30_000u32, 300u32);
        let mut trans: Vec<(u32, Role, u32)> = (0..fan).map(|k| (0, k, 1 + k % path)).collect();
        trans.extend((1..path).map(|s| (s, 0, s + 1)));
        trans.sort_unstable();
        let mut a = auto(path + 1, &[path], &trans);
        let same = a.clone();
        assert!(300 * (a.states as usize + a.trans.len()) < REDUCE_WORK);
        assert_eq!(a.reduce_within(false, &|r, s| r == s, 1, REDUCE_WORK), 0);
        assert_eq!(a, same);
        // a small automaton leaves nearly all of the bound
        let mut b = auto(3, &[1, 2], &[(0, 0, 1), (0, 0, 2), (1, 0, 2), (2, 0, 2)]);
        assert!(b.reduce_within(true, &|r, s| r == s, 1, REDUCE_WORK) > REDUCE_WORK - 400);
        assert_eq!(b.states, 2);
        assert_eq!((log_steps(0), log_steps(1), log_steps(4)), (1, 2, 4));
    }

    /// Random automata over four roles keep their words under the merge, and under the
    /// drop of covered transitions keep them up to the roles above.
    #[test]
    fn random_automata_keep_their_words() {
        // role 1 is under 0, and 3 under 2 under 0
        let sub = |r: Role, s: Role| r == s || matches!((r, s), (1, 0) | (2, 0) | (3, 0) | (3, 2));
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n) as u32
        };
        let (mut merged, mut dropped) = (0, 0);
        for _ in 0..3000 {
            let n = 2 + rnd(5);
            let finals: Vec<u32> = (0..n).filter(|_| rnd(3) == 0).collect();
            let mut trans = Vec::new();
            for _ in 0..rnd(4 * n as u64) {
                trans.push((rnd(n as u64), rnd(4), rnd(n as u64)));
            }
            trans.sort_unstable();
            trans.dedup();
            let a = auto(n, &finals, &trans);
            let before = words(&a, 4, 5);
            let mut m = a.clone();
            m.reduce(false, &sub, 1);
            assert_eq!(words(&m, 4, 5), before, "{a:?}");
            merged += usize::from(m.states < a.states);
            let mut d = a.clone();
            d.reduce(true, &sub, 1);
            let after = words(&d, 4, 5);
            assert!(after.is_subset(&before), "{a:?}");
            assert!(before.iter().all(|w| under(&after, w, &sub)), "{a:?}");
            dropped += usize::from(d.trans.len() < m.trans.len());
            // the marks are those of the transitions
            let mut marked = d.clone();
            marked.mark();
            assert_eq!(marked, d);
        }
        assert!(merged > 300 && dropped > 300, "{merged} {dropped}");
    }
}
