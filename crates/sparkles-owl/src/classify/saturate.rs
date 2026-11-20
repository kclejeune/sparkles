//! Concurrent saturation of contexts by the consequence-based calculus of ELK (Kazakov,
//! Krötzsch and Simančík, JAR 2014, §3 and §5).
//!
//! A context stands for an instance of its root expression. Its conclusions are the
//! subsumers of the root, the backward links from contexts whose instances have an
//! instance of the root as a role successor, the forward links that property chains need
//! and the role loops of `ObjectHasSelf` and reflexive roles. Every rule reads only the
//! state of the context it runs in and sends conclusions to other contexts as messages.
//! A context is processed by one worker at a time: a worker takes an active context,
//! applies the rules to the conclusions waiting in its to-do list, and a conclusion sent
//! to an inactive context activates it. A worker keeps the contexts it activates on a
//! stack of its own and moves half of them to the shared queue when another worker is
//! idle, so that the workers do not meet at one lock and one condition variable for each
//! activation.
//!
//! Individuals add three rules. A context that derives `{a}` stands for `a`, so it takes
//! every subsumer of `a` (it registers with `a`'s context, which sends them). A context
//! that has an instance in every model, which `owl:Thing`, the individuals and every
//! context they reach do and which is called real here, also gives `a` every subsumer it
//! has. And a link by `r` from a context with `{a}` to one with `{a}` is a loop by `r`.
//! What a context that is not real knows of an individual holds only where its root has
//! an instance; [`super::world`] classifies the classes that reach such a context.

use super::data::DataVal;
use super::dl_root;
use super::dl_rules;
use super::horn::HornIx;
use super::horn_rules;
use super::index::{BOTTOM, Ex, Index, NO_ROLE, Node, Role, TOP};
use parking_lot::{Condvar, Mutex, RwLock};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::VecDeque;
use std::collections::hash_map::Entry;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// A conclusion waiting in a context's to-do list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Concl {
    /// the context's root is subsumed by the expression
    Sub(Ex),
    /// an instance of the source context has an instance of this context as an
    /// `r`-successor
    Back(Ex, Role),
    /// an instance of this context has an instance of the target as an `r`-successor,
    /// kept for property chains
    Fwd(Role, Ex),
    /// every instance of this context is its own `r`-successor
    Loop(Role),
    /// the source context's instance is this context's individual
    Nom(Ex),
    /// the context has an instance in every model
    Real,
    /// an instance of this context has the individual as an `r`-successor
    LinkTo(Role, Ex),
    /// Tier H: the context, a successor of this one, holds the at-most filler
    SuccIn(Ex, Ex),
    /// Tier H: the context, a predecessor of this one, holds the at-most filler
    PredIn(Ex, Ex),
    /// Tier H: an instance of this context has an instance of the target as an
    /// `r`-successor, which this context links to
    Succ(Role, Ex),
    /// Tier H with individuals (roadmap item 692): the predecessor is real
    RealBack(Ex),
    /// Tier H with individuals: an at-most restriction of a singular successor counts
    /// this context, so it has at most one instance
    Singular,
    /// Tier H with individuals: the context stands for the same element as this one,
    /// and takes what this one holds and derives
    Same(Ex),
    /// Tier D: the marker `A → A` of an atom that may hold of the context's instance
    Hyp(Ex),
    /// Tier D: the source context registers with this hypothetical successor by its
    /// existential restriction
    DEdge(Ex, Ex),
    /// Tier D: the successor sent a clause over its predecessor, by its id in the run's
    /// table
    PredC(Ex, u32),
    /// Tier D in a rooted run: the root sent a processed clause with an empty body over
    /// one of the constants the context registered for, by its id in the run's table
    RPred(u32),
    /// Tier D in a rooted run: the marker of a ground literal that a predecessor holds as
    /// a maximal literal, by its id in the run's literals (Succ)
    HypL(u32),
    /// Tier D in a rooted run: the source context registers with the root for the
    /// constant, which a ground literal in one of its bodies mentions
    REdge(Ex, u32),
    /// Tier D in a rooted run: a hypothetical successor derived a clause with an empty
    /// body over constants and their successor terms, a fact of the root, by its id in
    /// the run's table
    RFact(u32),
    /// Tier D in a rooted run with row 1231's marker repair: the source context holds the
    /// ground literal, by its id in the run's literals, as a maximal literal of a processed
    /// clause other than a marker, so the root sends its marker to the contexts registered
    /// for its constants (M-send)
    RMark(Ex, u32),
    /// Tier D in a rooted run with row 1231's marker repair: the root sent a processed
    /// clause whose body is ground to a context that holds every body literal as a maximal
    /// literal, by its id in the run's table (M-return)
    RRet(u32),
    /// Tier D in a rooted run with row 1231's marker repair: the root reaches the context
    /// by successors, or in the root, the root reaches the context named
    RReach(Ex),
}

// a conclusion takes three words, so that the to-do lists and outboxes keep their size
const _: () = assert!(std::mem::size_of::<Concl>() == 12);

/// A set kept as a list in the order of insertion, with a hash set beside it once it
/// has more than a few members, so that a small set takes one allocation.
#[derive(Clone)]
pub struct Small<T> {
    pub list: Vec<T>,
    set: Option<FxHashSet<T>>,
}

impl<T> Default for Small<T> {
    fn default() -> Small<T> {
        Small {
            list: Vec::new(),
            set: None,
        }
    }
}

/// The bytes of a hash table with room for `capacity` entries of `size` bytes: its
/// buckets, of which it fills at most seven in eight, and a control byte for each.
pub fn table_bytes(capacity: usize, size: usize) -> usize {
    capacity * (size + 1) * 8 / 7
}

/// The members a [`Small`] set searches in its list before it keeps a hash set.
const SMALL: usize = 16;

impl<T: Copy + Eq + std::hash::Hash> Small<T> {
    /// Adds a member, and says whether it is new.
    #[inline]
    pub fn insert(&mut self, x: T) -> bool {
        match &mut self.set {
            Some(set) => {
                if !set.insert(x) {
                    return false;
                }
            }
            None => {
                if self.list.contains(&x) {
                    return false;
                }
                if self.list.len() >= SMALL {
                    let mut set: FxHashSet<T> =
                        FxHashSet::with_capacity_and_hasher(2 * SMALL, Default::default());
                    set.extend(self.list.iter().copied());
                    set.insert(x);
                    self.set = Some(set);
                }
            }
        }
        self.list.push(x);
        true
    }

    // always inlined, since the delivery of each conclusion asks it and an outlined call
    // cost 4% of the instructions of ore_ont_8737 (item 710's lane)
    #[inline(always)]
    pub fn contains(&self, x: &T) -> bool {
        match &self.set {
            Some(set) => set.contains(x),
            None => self.list.contains(x),
        }
    }

    pub fn retain(&mut self, keep: impl Fn(&T) -> bool) {
        self.list.retain(|x| keep(x));
        if let Some(set) = &mut self.set {
            set.retain(|x| keep(x));
        }
    }

    /// Replaces each member by `f` of it, which must keep distinct members distinct.
    pub fn map(&mut self, f: impl Fn(T) -> T) {
        for x in &mut self.list {
            *x = f(*x);
        }
        if let Some(set) = &mut self.set {
            let mapped: FxHashSet<T> = set.iter().map(|&x| f(x)).collect();
            debug_assert_eq!(mapped.len(), self.list.len());
            *set = mapped;
        }
    }

    pub fn bytes(&self) -> usize {
        self.list.capacity() * std::mem::size_of::<T>()
            + self
                .set
                .as_ref()
                .map_or(0, |s| table_bytes(s.capacity(), std::mem::size_of::<T>()))
    }
}

/// The members a context's subsumers keep in a list before they become a hash set.
const SUBS_LIST: usize = 12;

/// A context's subsumers: a list while they are few, which most contexts' are, so that
/// a small context takes one allocation and no rehashing as it grows, and a hash set
/// once they pass [`SUBS_LIST`].
#[derive(Clone)]
pub enum Subs {
    List(Vec<Ex>),
    Set(FxHashSet<Ex>),
}

impl Default for Subs {
    fn default() -> Subs {
        Subs::List(Vec::new())
    }
}

impl Subs {
    /// Adds a member, and says whether it is new.
    #[inline]
    pub fn insert(&mut self, x: Ex) -> bool {
        match self {
            Subs::Set(set) => set.insert(x),
            Subs::List(list) => {
                if has(list, x) {
                    return false;
                }
                if list.len() < SUBS_LIST {
                    if list.capacity() == 0 {
                        list.reserve_exact(8);
                    }
                    list.push(x);
                } else {
                    let mut set: FxHashSet<Ex> =
                        FxHashSet::with_capacity_and_hasher(2 * SUBS_LIST, Default::default());
                    set.extend(list.iter().copied());
                    set.insert(x);
                    *self = Subs::Set(set);
                }
                true
            }
        }
    }

    #[inline]
    pub fn contains(&self, x: &Ex) -> bool {
        match self {
            Subs::Set(set) => set.contains(x),
            Subs::List(list) => has(list, *x),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Subs::Set(set) => set.len(),
            Subs::List(list) => list.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> SubsIter<'_> {
        match self {
            Subs::Set(set) => SubsIter::Set(set.iter()),
            Subs::List(list) => SubsIter::List(list.iter()),
        }
    }

    /// The bytes the members take.
    pub fn bytes(&self) -> usize {
        match self {
            Subs::Set(set) => table_bytes(set.capacity(), 4),
            Subs::List(list) => list.capacity() * 4,
        }
    }
}

/// Whether a list holds `x`, comparing eight members at a time without a branch, which
/// the compiler can do in vector registers.
#[inline]
fn has(list: &[Ex], x: Ex) -> bool {
    let (chunks, rest) = list.as_chunks::<8>();
    for c in chunks {
        if c.iter().fold(false, |a, &y| a | (y == x)) {
            return true;
        }
    }
    rest.contains(&x)
}

/// The members of a [`Subs`], in no particular order.
pub enum SubsIter<'a> {
    List(std::slice::Iter<'a, Ex>),
    Set(std::collections::hash_set::Iter<'a, Ex>),
}

impl<'a> Iterator for SubsIter<'a> {
    type Item = &'a Ex;
    #[inline]
    fn next(&mut self) -> Option<&'a Ex> {
        match self {
            SubsIter::List(i) => i.next(),
            SubsIter::Set(i) => i.next(),
        }
    }
}

impl<'a> IntoIterator for &'a Subs {
    type Item = &'a Ex;
    type IntoIter = SubsIter<'a>;
    fn into_iter(self) -> SubsIter<'a> {
        self.iter()
    }
}

/// The conclusions of one context.
#[derive(Default, Clone)]
pub struct State {
    pub init: bool,
    pub subs: Subs,
    pub bottom: bool,
    /// whether the context has an instance in every model
    pub real: bool,
    /// the negative existential restrictions whose filler is a subsumer, which a new
    /// backward link propagates to its source
    pub props: Vec<(Role, Ex)>,
    /// the sources of backward links, by role
    pub back: Vec<(Role, Small<Ex>)>,
    /// the forward links, by role and target
    pub fwd: Small<(Role, Ex)>,
    /// the member of each disjointness set among the subsumers
    pub disj: FxHashMap<u32, Ex>,
    pub loops: Vec<Role>,
    /// what only contexts with individuals or functional data roles need
    pub extra: Option<Box<Extra>>,
}

/// The conclusions of a context that only individuals and functional data roles give.
#[derive(Default, Clone)]
pub struct Extra {
    /// the individuals among the subsumers, the root aside
    pub noms: Vec<Ex>,
    /// for an individual's context, the contexts whose instance it is
    pub members: Small<Ex>,
    /// the individuals that an instance has as `r`-successors, for the loops they close
    pub links: Vec<(Role, Ex)>,
    /// the data values that functional data roles make one
    pub groups: Vec<Group>,
    /// what the Horn rules keep
    pub horn: super::horn_rules::HornSt,
    /// what Tier D keeps, once the context has a clause that is not a unit
    pub dl: Option<Box<super::dl_rules::DlSt>>,
}

/// Data successors of a context that functional data roles make one value: the
/// functional roles, the roles of the successors and the intersection of their ranges.
#[derive(Clone)]
pub struct Group {
    pub funs: Vec<Role>,
    pub roles: Vec<Role>,
    pub val: DataVal,
}

impl Extra {
    fn bytes(&self) -> usize {
        std::mem::size_of::<Extra>()
            + self.noms.capacity() * 4
            + self.members.bytes()
            + self.links.capacity() * 8
            + self
                .groups
                .iter()
                .map(|g| (g.funs.capacity() + g.roles.capacity()) * 4 + g.val.owned_bytes())
                .sum::<usize>()
            + self.groups.capacity() * std::mem::size_of::<Group>()
            + self.horn.bytes()
            + self.dl.as_ref().map_or(0, |d| d.bytes())
    }
}

impl State {
    /// The bytes the state's tables and lists take, roughly.
    pub fn bytes(&self) -> usize {
        self.subs.bytes()
            + self.props.capacity() * 8
            + self.back.capacity() * std::mem::size_of::<(Role, Small<Ex>)>()
            + self.back.iter().map(|(_, v)| v.bytes()).sum::<usize>()
            + self.fwd.bytes()
            + table_bytes(self.disj.capacity(), 8)
            + self.loops.capacity() * 4
            + self.extra.as_ref().map_or(0, |x| x.bytes())
    }

    pub fn extra(&mut self) -> &mut Extra {
        self.extra.get_or_insert_with(Box::default)
    }

    /// The individuals among the context's subsumers, its root aside.
    pub fn noms(&self) -> &[Ex] {
        self.extra.as_ref().map_or(&[], |x| x.noms.as_slice())
    }

    /// Whether the state already holds a conclusion, so that a world over it or a
    /// congested delivery can drop the conclusion: the rules have applied to it, and
    /// every rule returns at once from a conclusion its state holds. A kind this does
    /// not answer is never held.
    pub fn holds(&self, c: &Concl) -> bool {
        match *c {
            Concl::Sub(d) => self.bottom || self.subs.contains(&d),
            Concl::Back(e, r) => self.back.iter().any(|(s, v)| *s == r && v.contains(&e)),
            Concl::Fwd(r, f) => self.fwd.contains(&(r, f)),
            Concl::Loop(r) => self.loops.contains(&r),
            Concl::Nom(k) => self.extra.as_ref().is_some_and(|x| x.members.contains(&k)),
            Concl::Real => self.real,
            Concl::LinkTo(r, n) => self
                .extra
                .as_ref()
                .is_some_and(|x| x.links.contains(&(r, n))),
            Concl::Hyp(a) => self
                .extra
                .as_ref()
                .and_then(|x| x.dl.as_ref())
                .is_some_and(|d| d.has_hyp(a)),
            Concl::RealBack(m) => self
                .extra
                .as_ref()
                .is_some_and(|x| x.horn.real_preds.contains(&m)),
            Concl::Singular => self.extra.as_ref().is_some_and(|x| x.horn.counted),
            Concl::Same(k) => self
                .extra
                .as_ref()
                .is_some_and(|x| x.horn.same.contains(&k)),
            Concl::SuccIn(..)
            | Concl::PredIn(..)
            | Concl::Succ(..)
            | Concl::DEdge(..)
            | Concl::PredC(..)
            | Concl::RPred(..)
            | Concl::HypL(..)
            | Concl::REdge(..)
            | Concl::RFact(..)
            | Concl::RMark(..)
            | Concl::RRet(..)
            | Concl::RReach(..) => false,
        }
    }

    /// Appends the messages of a run that the state does not hold, as [`State::holds`]
    /// answers. The run's backward links mostly share their role, whose sources it looks
    /// up once for each change of role, and its links take inlined probes of the
    /// context's tables (roadmap item 889).
    #[inline]
    fn add_new(&self, run: &[Concl], out: &mut Vec<Concl>) {
        let mut by: Option<(Role, Option<&Small<Ex>>)> = None;
        for &c in run {
            let held = match c {
                Concl::Back(e, r) => {
                    let srcs = match by {
                        Some((s, srcs)) if s == r => srcs,
                        _ => {
                            let srcs = self.back.iter().find(|(s, _)| *s == r).map(|(_, v)| v);
                            by = Some((r, srcs));
                            srcs
                        }
                    };
                    srcs.is_some_and(|v| v.contains(&e))
                }
                Concl::Fwd(r, f) => self.fwd.contains(&(r, f)),
                _ => self.holds(&c),
            };
            if !held {
                out.push(c);
            }
        }
    }

    /// Removes the backward links whose source is in `gone`.
    pub fn drop_links_from(&mut self, gone: &FxHashSet<Ex>) {
        for (_, srcs) in &mut self.back {
            if srcs.list.iter().any(|e| gone.contains(e)) {
                srcs.retain(|e| !gone.contains(e));
            }
        }
        self.back.retain(|(_, v)| !v.list.is_empty());
    }

    /// Removes what the state keeps of the contexts in `gone` as its predecessors,
    /// which a run that clears them does before they register again (roadmap item
    /// 753): their backward links and what the Horn rules were told of them.
    pub fn drop_predecessors(&mut self, gone: &FxHashSet<Ex>) {
        self.drop_links_from(gone);
        if let Some(x) = self.extra.as_mut() {
            let h = &mut x.horn;
            if h.pred_in.list.iter().any(|(m, _)| gone.contains(m)) {
                h.pred_in.retain(|(m, _)| !gone.contains(m));
            }
            if h.told.list.iter().any(|(m, _)| gone.contains(m)) {
                h.told.retain(|(m, _)| !gone.contains(m));
            }
            for set in [&mut h.real_preds, &mut h.same, &mut h.told_singular] {
                if set.list.iter().any(|m| gone.contains(m)) {
                    set.retain(|m| !gone.contains(m));
                }
            }
            if let Some(d) = x.dl.as_deref_mut() {
                d.drop_predecessors(gone);
            }
        }
    }

    /// Adds `delta` to every context id from `from` upward that the state names, the
    /// ids of the contexts that a saturation creates above the index's, when the index
    /// of the next run has `delta` more expressions (roadmap item 753). The subsumers,
    /// the fillers and the roles are expressions and roles of the index and stay.
    pub fn shift_contexts(&mut self, from: Ex, delta: Ex) {
        let up = |e: Ex| if e >= from { e + delta } else { e };
        for (_, srcs) in &mut self.back {
            if srcs.list.iter().any(|&e| e >= from) {
                srcs.map(up);
            }
        }
        if self.fwd.list.iter().any(|&(_, f)| f >= from) {
            self.fwd.map(|(r, f)| (r, up(f)));
        }
        if let Some(x) = self.extra.as_mut() {
            debug_assert!(x.noms.is_empty() && x.links.is_empty());
            if let Some(d) = x.dl.as_deref_mut() {
                d.shift_contexts(from, delta);
            }
            if x.members.list.iter().any(|&e| e >= from) {
                x.members.map(up);
            }
            let h = &mut x.horn;
            debug_assert!(h.pending.is_empty() && h.merges.is_empty());
            h.forget_dominance();
            if h.succ.list.iter().any(|&(_, n)| n >= from) {
                h.succ.map(|(r, n)| (r, up(n)));
            }
            if h.succ_in.list.iter().any(|&(n, _)| n >= from) {
                h.succ_in.map(|(n, b)| (up(n), b));
            }
            if h.pred_in.list.iter().any(|&(m, _)| m >= from) {
                h.pred_in.map(|(m, b)| (up(m), b));
            }
            if h.told.list.iter().any(|&(m, n)| m >= from || n >= from) {
                h.told.map(|(m, n)| (up(m), up(n)));
            }
            for set in [&mut h.real_preds, &mut h.same, &mut h.told_singular] {
                if set.list.iter().any(|&e| e >= from) {
                    set.map(up);
                }
            }
        }
    }

    /// Recomputes the members of the disjointness sets among the subsumers, whose
    /// numbers the index of each run gives anew.
    pub fn reindex_disjoint(&mut self, ix: &Index) {
        self.disj.clear();
        for &d in &self.subs {
            for &s in ix.disjoint(d) {
                self.disj.entry(s).or_insert(d);
            }
        }
    }

    /// The sources of backward links by one role.
    #[inline]
    fn back_by(&mut self, r: Role) -> &mut Small<Ex> {
        let at = match self.back.iter().position(|(s, _)| *s == r) {
            Some(at) => at,
            None => {
                self.back.push((r, Small::default()));
                self.back.len() - 1
            }
        };
        &mut self.back[at].1
    }

    /// Recomputes what depends on the index rather than on the derivations: the
    /// propagations and the disjointness members of the subsumers.
    pub fn reindex(&mut self, ix: &Index) {
        self.props.clear();
        self.disj.clear();
        for &d in &self.subs {
            self.props.extend_from_slice(ix.exist(d));
            for &s in ix.disjoint(d) {
                self.disj.entry(s).or_insert(d);
            }
        }
    }
}

impl Ctx {
    /// Forgets the context's conclusions, so that the next saturation starts it again.
    pub fn clear(&self) {
        *self.st.lock() = State::default();
        self.todo.lock().clear();
        self.started.store(false, Ordering::Release);
    }
}

/// A context: its state, its to-do list and whether a worker holds it.
#[derive(Default)]
pub struct Ctx {
    active: AtomicBool,
    /// whether the context's state is started, which [`State::init`] says under the
    /// state's lock, kept here so that the workers that take roots need not wait for
    /// that lock while the context is processed
    pub started: AtomicBool,
    pub todo: Mutex<Vec<Concl>>,
    pub st: Mutex<State>,
}

/// The contexts, by root expression.
pub type Contexts = Vec<OnceLock<Box<Ctx>>>;

/// Why a saturation stopped before its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Cancelled,
    Timeout,
    Memory {
        used: usize,
    },
    /// Tier D derived more conclusions than its bound, which is the run's own stop and
    /// not the caller's
    Work {
        used: usize,
    },
}

/// What bounds a saturation.
pub struct Bounds<'a> {
    pub threads: usize,
    pub deadline: Option<Instant>,
    pub max_memory: Option<usize>,
    /// polled while the saturation runs: true asks it to stop as cancelled
    pub cancelled: Option<&'a (dyn Fn() -> bool + Sync)>,
    /// told each growth of the estimated memory: false stops the run
    pub charge: Option<&'a (dyn Fn(usize) -> bool + Sync)>,
    /// the bytes held before the saturation starts
    pub base_memory: usize,
    /// the peak the run reached before the saturation, which the pool has been charged
    /// for, so that only growth past it is charged
    pub peak_memory: usize,
    /// the contexts of conjunctions that the Horn rules create, in a run with them
    pub dynamic: Option<&'a Dynamic>,
    /// what the contexts of Tier D share, in a run with it
    pub dl: Option<&'a super::dl_rules::DlRun>,
    /// whether a stop leaves every context's unapplied conclusions in its to-do list, for
    /// a partial answer
    pub partial: bool,
    /// whether a run of Tier D proceeds in steps ([`Run::steps`]), so that what it derives
    /// and where it passes its bound on derived conclusions do not depend on the threads
    /// (roadmap item 727)
    pub steps: bool,
}

const STOP_NONE: u8 = 0;
const STOP_CANCEL: u8 = 1;
const STOP_TIMEOUT: u8 = 2;
const STOP_MEMORY: u8 = 3;
const STOP_WORK: u8 = 4;

/// The conclusions a worker applies between checks of the bounds.
const CHECK_EVERY: usize = 4096;
/// The messages a worker sends between checks of the bounds.
const SEND_EVERY: usize = 16_384;
/// The bytes counted for each stored conclusion.
pub const CONCLUSION_BYTES: usize = 16;
/// The roots a worker takes from the shared list at a time.
const ROOT_CHUNK: usize = 64;
/// The roots a worker activates before the contexts they woke.
const ROOTS_PER_ROUND: usize = 1024;
/// The bytes counted for each context a saturation starts.
pub const CONTEXT_BYTES: usize = 320;
/// The messages a worker's outbox holds before it delivers them.
const OUTBOX: usize = 4096;
/// The messages that the runs of an outbox may stand for before it delivers them, those
/// that several destinations share counted for each
const OUTBOX_TOTAL: usize = 16 * OUTBOX;
/// The clauses of Tier D a context processes between checks of the bounds.
const DL_CLAUSES: usize = 256;
/// The contexts a step of [`Run::steps`] activates.
pub(super) const STEP_CONTEXTS: usize = 4096;
/// The conclusions of Tier D after which an activation of a run in steps processes no
/// further clause.
const STEP_WORK: usize = 4096;
/// The contexts from which the workers activate a step together, whatever the last
/// step took.
const STEP_PARALLEL: usize = if cfg!(test) { 2 } else { 128 };
/// The time of a step's activations from which the workers activate the next step
/// together, when it has two contexts or more.
const STEP_HEAVY: Duration = Duration::from_micros(if cfg!(test) { 0 } else { 150 });
/// The messages from which the workers deliver a step's messages together.
const STEP_MESSAGES: usize = if cfg!(test) { 1 } else { 4096 };
/// The bytes of waiting messages below which a saturation never counts as congested.
const CONGESTED_BYTES: usize = 256 << 20;
/// The roots from which a saturation on several threads starts them all at once.
const EAGER_ROOTS: usize = if cfg!(test) { 64 } else { 4096 };
/// How long the first worker of a smaller saturation runs before the others start, so
/// that a run that ends sooner pays for no thread (roadmap item 709), until a start in
/// the process has measured what starting them costs ([`STARTED_NS`]).
const LAZY_START: Duration = Duration::from_micros(if cfg!(test) { 0 } else { 500 });
/// The least time the first worker runs alone once a start has been measured.
const LAZY_MIN: Duration = Duration::from_micros(if cfg!(test) { 0 } else { 20 });
/// How long the first worker of the process's last saturation that started helper
/// threads took to start them, in nanoseconds, and 0 before any start. A process's first
/// threads cost much more to start than later ones, whose stacks it keeps, so a later
/// saturation waits only as long as the last start took, between [`LAZY_MIN`] and
/// [`LAZY_START`] (roadmap item 720).
static STARTED_NS: AtomicU64 = AtomicU64::new(0);

/// How long the first worker of a saturation on several threads runs before the others
/// start: as long as starting them last took, as `started` holds it, so that a run that
/// ends sooner pays for no thread, and one that runs longer waits no longer than starting
/// them costs it.
fn lazy_start(started: &AtomicU64) -> Duration {
    match started.load(Ordering::Relaxed) {
        0 => LAZY_START,
        ns => Duration::from_nanos(ns).max(LAZY_MIN).min(LAZY_START),
    }
}

#[cfg(test)]
thread_local! {
    /// Makes the saturations this thread starts count as congested from the start, so
    /// that the tests reach the delivery of congested runs.
    pub(super) static ALWAYS_CONGESTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Where the saturations this thread starts record their starts of the helpers in
    /// place of [`STARTED_NS`], so that a test sees its own.
    pub(super) static STARTED_IN: std::cell::Cell<Option<&'static AtomicU64>> = const { std::cell::Cell::new(None) };
    /// What this thread is in a saturation that runs now: 0 outside one, [`FIRST_WORKER`]
    /// and [`HELPER`] in a run without steps, [`STEP_CALLER`] and [`STEP_HELPER`] in a run
    /// in steps, so that a test's callback knows where it is called.
    pub(super) static WORKER: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub(super) const FIRST_WORKER: u8 = 1;
#[cfg(test)]
pub(super) const HELPER: u8 = 2;
#[cfg(test)]
pub(super) const STEP_CALLER: u8 = 3;
#[cfg(test)]
pub(super) const STEP_HELPER: u8 = 4;

struct Run<'a> {
    ix: &'a Index,
    ctx: &'a Contexts,
    /// the subsumers that every context has beyond the index's: the facts of universal
    /// roles found by earlier rounds
    universal: &'a [Ex],
    /// the roots, which the workers take in chunks from `next_root` on
    roots: &'a [Ex],
    next_root: AtomicUsize,
    queue: Mutex<Vec<Ex>>,
    ready: Condvar,
    /// the contexts in the shared queue and the workers that are not idle, which hold
    /// the other active contexts on their stacks or in their outboxes: the saturation
    /// ends when it is 0. A worker that wakes a context keeps it, so this changes only
    /// when contexts go to the shared queue and when a worker goes idle.
    pending: AtomicUsize,
    /// the workers waiting for the queue
    idle: AtomicUsize,
    stop: AtomicU8,
    /// the estimated bytes of the stored conclusions and the index
    memory: AtomicUsize,
    /// the messages waiting in to-do lists, which count as memory while they wait
    queued: AtomicIsize,
    /// the most memory counted so far, which the pool's grant covers
    peak: AtomicUsize,
    applied: AtomicUsize,
    /// whether a context derived an individual other than its root
    merged: AtomicBool,
    /// whether the workers send through outboxes, which they do when there are several,
    /// and which one worker starts doing once the run is congested, so that its messages
    /// reach the delivery filter (roadmap item 721)
    batch: AtomicBool,
    /// whether the rules run without those of individuals and functional data roles
    plain: bool,
    /// whether the rules are those of Tier H
    horn: bool,
    /// whether the rules are those of Tier D
    disj: bool,
    /// whether the run proceeds in steps, [`Bounds::steps`] in a run of Tier D
    steps: bool,
    dynamic: Option<&'a Dynamic>,
    bounds: &'a Bounds<'a>,
    /// where the run records its start of the helpers, [`STARTED_NS`] outside tests
    started: &'static AtomicU64,
    #[cfg(test)]
    always_congested: bool,
}

/// What the workers of a run in steps share ([`Run::steps`]).
struct Steps {
    /// the contexts of the step
    frontier: RwLock<Vec<Ex>>,
    /// the next place of the frontier to activate
    next: AtomicUsize,
    /// what each worker's activations left
    outs: Vec<RwLock<StepOut>>,
    /// the contexts each worker's deliveries woke
    woke: Vec<Mutex<Vec<Ex>>>,
    /// the step's activations that sent messages, by place in the frontier: the place,
    /// the worker, and the range of the worker's messages
    order: RwLock<Vec<(u32, u32, usize, usize)>>,
    /// the phase the helpers take part in
    gate: Mutex<Gate>,
    /// tells the helpers that a phase opened
    opened: Condvar,
    /// tells the calling thread that the last helper left a phase
    left: Condvar,
}

/// Where the helpers of a run in steps are. The calling thread opens a phase for them
/// and waits until each has left it. A helper leaves a phase when it unwinds too, and
/// the calling thread releases the helpers when it unwinds, so that a panic in one
/// worker ends the run and is not waited for.
struct Gate {
    /// counts the phases opened, so that a helper takes each once
    round: u64,
    /// what the helpers do in the phase
    phase: u8,
    /// the helpers that have not left the phase
    busy: usize,
    /// whether a helper panicked
    failed: bool,
}

const STEP_ACTIVATE: u8 = 0;
const STEP_DELIVER: u8 = 1;
const STEP_DONE: u8 = 2;

/// Whether the thread began to unwind since `before` was read from
/// [`std::thread::panicking`], which is true from the start for a saturation that a
/// destructor runs while its thread unwinds.
fn unwinds(before: bool) -> bool {
    !before && std::thread::panicking()
}

/// Stops the run when its thread unwinds, so that the other workers of the saturation
/// end and the panic reaches the caller. A worker that panicked was otherwise waited for
/// without end.
struct HaltOnPanic<'r, 'a>(&'r Run<'a>, bool);

impl<'r, 'a> HaltOnPanic<'r, 'a> {
    fn new(run: &'r Run<'a>) -> Self {
        HaltOnPanic(run, std::thread::panicking())
    }
}

impl Drop for HaltOnPanic<'_, '_> {
    fn drop(&mut self) {
        if unwinds(self.1) {
            self.0.halt(STOP_CANCEL);
        }
    }
}

/// A helper's part in a phase of a run in steps, which it leaves when it is dropped,
/// at the end of its work and when it unwinds.
struct InPhase<'r, 'a>(&'r Run<'a>, &'r Steps);

impl Drop for InPhase<'_, '_> {
    fn drop(&mut self) {
        // a helper is a thread of the run, which does not start while unwinding
        let panicked = std::thread::panicking();
        if panicked {
            self.0.halt(STOP_CANCEL);
        }
        let mut g = self.1.gate.lock();
        g.failed |= panicked;
        g.busy -= 1;
        if g.busy == 0 {
            self.1.left.notify_one();
        }
    }
}

/// The calling thread's hold on the helpers of a run in steps, which ends them when it
/// is dropped, at the end of the run and when the calling thread unwinds.
struct Helpers<'r, 'a>(&'r Run<'a>, &'r Steps, bool);

impl<'r, 'a> Helpers<'r, 'a> {
    fn new(run: &'r Run<'a>, steps: &'r Steps) -> Self {
        Helpers(run, steps, std::thread::panicking())
    }
}

impl Drop for Helpers<'_, '_> {
    fn drop(&mut self) {
        if unwinds(self.2) {
            self.0.halt(STOP_CANCEL);
        }
        let mut g = self.1.gate.lock();
        g.round += 1;
        g.phase = STEP_DONE;
        drop(g);
        self.1.opened.notify_all();
    }
}

/// What a worker's activations of one step left.
#[derive(Default)]
struct StepOut {
    /// the messages, in the order the activations sent them
    outbox: Outbox,
    /// for each activation, its place in the frontier and the end of its messages
    segs: Vec<(u32, usize)>,
    /// the contexts that gave way
    again: VecDeque<Ex>,
    tally: Tally,
    /// the buffer of a context's conclusions, kept across contexts
    local: Vec<Concl>,
    /// where the lists of the context of the activation stood at its last flush of its
    /// link products, with the buffers of the flush
    marks: Marks,
}

/// What a worker counted since it last added to the totals.
#[derive(Default)]
struct Tally {
    applied: usize,
    /// stored conclusions
    stored: usize,
    /// the change in waiting messages
    queued: isize,
}

/// What a saturation did.
#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    pub conclusions: usize,
    pub memory: usize,
    /// the most memory counted, waiting messages included
    pub peak: usize,
    /// whether a context derived an individual other than its root
    pub merged: bool,
}

/// Saturates `roots` and every context they reach. Contexts already saturated keep their
/// conclusions and take new ones: `seeds` are contexts with conclusions waiting in their
/// to-do lists. `universal` are subsumers of every context beyond the index's.
pub fn saturate(
    ix: &Index,
    ctx: &Contexts,
    roots: &[Ex],
    seeds: &[Ex],
    universal: &[Ex],
    bounds: &Bounds<'_>,
) -> Result<Totals, Stop> {
    match saturate_counted(ix, ctx, roots, seeds, universal, bounds) {
        (t, None) => Ok(t),
        (_, Some(s)) => Err(s),
    }
}

/// Saturates as [`saturate`] does, and gives the totals of the work done with the reason
/// the run stopped, for a partial answer.
pub fn saturate_counted(
    ix: &Index,
    ctx: &Contexts,
    roots: &[Ex],
    seeds: &[Ex],
    universal: &[Ex],
    bounds: &Bounds<'_>,
) -> (Totals, Option<Stop>) {
    let run = Run {
        ix,
        ctx,
        universal,
        roots,
        next_root: AtomicUsize::new(0),
        queue: Mutex::new(Vec::new()),
        ready: Condvar::new(),
        // the first worker, which the others join when they start (`Run::workers`)
        pending: AtomicUsize::new(1),
        idle: AtomicUsize::new(0),
        stop: AtomicU8::new(STOP_NONE),
        memory: AtomicUsize::new(bounds.base_memory),
        queued: AtomicIsize::new(0),
        peak: AtomicUsize::new(bounds.base_memory.max(bounds.peak_memory)),
        applied: AtomicUsize::new(0),
        merged: AtomicBool::new(false),
        batch: AtomicBool::new(bounds.threads > 1),
        plain: plain(ix),
        horn: ix.horn.is_some() && bounds.dynamic.is_some(),
        disj: ix.dl.is_some() && bounds.dl.is_some(),
        steps: bounds.steps && ix.dl.is_some() && bounds.dl.is_some(),
        dynamic: bounds.dynamic,
        bounds,
        #[cfg(not(test))]
        started: &STARTED_NS,
        #[cfg(test)]
        started: STARTED_IN.get().unwrap_or(&STARTED_NS),
        #[cfg(test)]
        always_congested: ALWAYS_CONGESTED.get(),
    };
    {
        let mut q = run.queue.lock();
        for &e in seeds {
            if run.mark(run.get(e)) {
                q.push(e);
            }
        }
        run.pending.fetch_add(q.len(), Ordering::SeqCst);
    }
    let threads = bounds.threads.max(1);
    if run.horn && run.plain {
        run.workers::<Horn>(threads);
    } else if run.horn {
        run.workers::<HornFull>(threads);
    } else if run.steps {
        run.steps::<Disj>(threads);
    } else if run.disj {
        run.workers::<Disj>(threads);
    } else if run.plain {
        run.workers::<Plain>(threads);
    } else {
        run.workers::<Full>(threads);
    }
    let memory = run.memory.load(Ordering::Relaxed);
    let peak = run.peak.load(Ordering::Relaxed).max(memory);
    let totals = Totals {
        conclusions: run.applied.load(Ordering::Relaxed),
        memory,
        peak,
        merged: run.merged.load(Ordering::Relaxed),
    };
    let stop = match run.stop.load(Ordering::SeqCst) {
        STOP_CANCEL => Some(Stop::Cancelled),
        STOP_TIMEOUT => Some(Stop::Timeout),
        // the most counted, waiting messages included, which is what passed the limit
        STOP_MEMORY => Some(Stop::Memory { used: peak }),
        STOP_WORK => Some(Stop::Work { used: peak }),
        _ => None,
    };
    (totals, stop)
}

/// Where a context's rules send their conclusions: to itself, applied in the same pass,
/// or to the worker's outbox, which delivers them to other contexts' to-do lists.
struct Out<'r, 'a> {
    run: &'r Run<'a>,
    me: Ex,
    local: &'r mut Vec<Concl>,
    /// the contexts this worker activated
    woke: &'r mut VecDeque<Ex>,
    /// the messages for other contexts that wait to be delivered
    outbox: &'r mut Outbox,
    /// where the context's lists stood at the last flush of its link products
    marks: &'r mut Marks,
    /// whether a rule stored a link or propagation since the last flush
    pending: bool,
    /// the messages sent to other contexts since the last check
    sent: usize,
    /// the conclusions of Tier D this activation derived
    work: usize,
    /// the bytes the rules kept beside their conclusions that are not charged yet
    /// ([`Sink::keep_data`])
    kept: usize,
}

/// The bytes an activation keeps uncharged ([`Sink::keep_data`]) before it charges them.
const KEEP_BATCH: usize = 16 << 10;

impl Drop for Out<'_, '_> {
    /// An activation charges what it kept and has not charged yet as it ends.
    fn drop(&mut self) {
        if self.kept > 0 {
            let kept = std::mem::take(&mut self.kept);
            let _ = self.reserve_data(kept);
        }
    }
}

/// Where a context's lists of forward links, propagations and backward links stood when
/// its activation last sent the products of its links ([`Run::flush_links`]), so that
/// the next flush pairs what is new since then with everything, and what is old only with
/// what is new. A worker keeps one across its activations, with the buffers the flush
/// reuses (roadmap item 818).
#[derive(Default)]
struct Marks {
    fwd: usize,
    props: usize,
    /// one per entry of [`State::back`], in its order
    back: Vec<usize>,
    /// the messages that every source of one group of backward links takes, built
    /// [`RUN_CHUNK`] at a time and sent to each source as one run
    run: Vec<Concl>,
    /// the targets of consecutive forward links by one role, which take the same
    /// backward links
    dests: Vec<Ex>,
}

/// The products of one group of a context's links that a flush builds at a time, so that
/// its buffer stays this size whatever the context's lists hold.
const RUN_CHUNK: usize = OUTBOX;

impl Marks {
    /// Sets the marks to the lists' ends, at the start of an activation.
    fn set(&mut self, st: &State) {
        self.fwd = st.fwd.list.len();
        self.props = st.props.len();
        self.back.clear();
        self.back
            .extend(st.back.iter().map(|(_, srcs)| srcs.list.len()));
    }
}

/// A run of an outbox: consecutive messages to one destination.
#[derive(Clone, Copy, Default)]
struct Dest {
    to: Ex,
    /// the first message and the one after the last, in [`Outbox::msgs`]
    start: u32,
    end: u32,
}

/// A worker's messages for other contexts, which it delivers by destination, with the
/// buffers that group them. Consecutive messages to one destination form a run, and a
/// delivery sorts the runs rather than the messages, since the products of a context's
/// links leave it in one run per source or target (roadmap item 818). The products that
/// several destinations take are written once, and each destination's run points at them
/// (roadmap item 889).
#[derive(Default)]
struct Outbox {
    msgs: Vec<Concl>,
    /// the runs, in the order sent, which may share their messages
    runs: Vec<Dest>,
    /// the messages the runs stand for, those they share counted for each
    total: usize,
    tmp: Vec<Dest>,
    counts: Vec<u32>,
    /// the messages of a run in steps with their destinations, in the order sent, which
    /// the step delivers at its end. Their places are `usize`, since a worker holds the
    /// messages of a whole step here.
    steps: Vec<(Ex, Concl)>,
}

/// The bits of a destination that one pass of the outbox's radix sort reads.
const RADIX_BITS: u32 = 11;
/// The runs below which an outbox sorts by comparison instead.
const RADIX_MIN: usize = 512;

impl Outbox {
    fn is_empty(&self) -> bool {
        self.msgs.is_empty()
    }

    #[inline]
    fn push(&mut self, to: Ex, c: Concl) {
        let at = self.msgs.len() as u32;
        self.to(to, at, at + 1);
        self.msgs.push(c);
    }

    /// Writes messages that several destinations take, and gives their places for
    /// [`Outbox::to`].
    fn shared(&mut self, msgs: &[Concl]) -> (u32, u32) {
        let start = self.msgs.len() as u32;
        self.msgs.extend_from_slice(msgs);
        (start, self.msgs.len() as u32)
    }

    /// Sends a destination the messages at the places `start..end`, which continue its
    /// last run when they follow it.
    #[inline]
    fn to(&mut self, to: Ex, start: u32, end: u32) {
        self.total += (end - start) as usize;
        match self.runs.last_mut() {
            Some(d) if d.to == to && d.end == start => d.end = end,
            _ => self.runs.push(Dest { to, start, end }),
        }
    }

    /// Whether the outbox holds enough to deliver: [`OUTBOX`] messages written, as many
    /// runs, or runs that stand for [`OUTBOX_TOTAL`] messages.
    #[inline]
    fn full(&self) -> bool {
        self.msgs.len() >= OUTBOX || self.runs.len() >= OUTBOX || self.total >= OUTBOX_TOTAL
    }

    fn clear(&mut self) {
        self.msgs.clear();
        self.runs.clear();
        self.total = 0;
    }

    /// Orders the runs by destination, keeping the order of each destination's.
    fn group(&mut self) {
        let n = self.runs.len();
        if n < RADIX_MIN {
            self.runs.sort_by_key(|d| d.to);
            return;
        }
        let max = self.runs.iter().map(|d| d.to).max().unwrap_or(0);
        let buckets = 1usize << RADIX_BITS;
        let mask = (buckets - 1) as u32;
        let mut shift = 0u32;
        self.tmp.clear();
        self.tmp.resize(n, Dest::default());
        while shift == 0 || (shift < 32 && max >> shift != 0) {
            self.counts.clear();
            self.counts.resize(buckets, 0);
            for d in &self.runs {
                self.counts[((d.to >> shift) & mask) as usize] += 1;
            }
            let mut at = 0u32;
            for c in self.counts.iter_mut() {
                let k = *c;
                *c = at;
                at += k;
            }
            for &d in &self.runs {
                let b = ((d.to >> shift) & mask) as usize;
                self.tmp[self.counts[b] as usize] = d;
                self.counts[b] += 1;
            }
            std::mem::swap(&mut self.runs, &mut self.tmp);
            shift += RADIX_BITS;
        }
    }
}

/// Where the rules of a context send their conclusions.
pub trait Sink {
    /// Pre-admit functional-data allocation, with retained growth reconciled afterwards.
    fn reserve_data(&mut self, _bytes: usize) -> bool {
        true
    }
    /// Charges bytes that the rules keep beside their conclusions, whose charge may wait
    /// for a batch, since the caller does not stop on its answer. A release nets against
    /// what waits first.
    fn keep_data(&mut self, bytes: usize) {
        let _ = self.reserve_data(bytes);
    }
    fn release_data(&mut self, _bytes: usize) {}
    fn data_overflow(&mut self) {}

    /// sends a conclusion to a context, which may be the one the rules run in
    fn send(&mut self, to: Ex, c: Concl);
    /// a conclusion for the context the rules run in
    fn me(&mut self, c: Concl);
    /// the context the rules run in derived an individual other than its root
    fn merged(&mut self) {}
    /// the conjunction a context stands for, as the sorted expressions that are not
    /// intersections, which the Horn rules read
    fn core(&self, ix: &Index, e: Ex, out: &mut Vec<Ex>) {
        let _ = (ix, e, out);
        unreachable!("the Horn rules run only in a saturation")
    }
    /// the context of a conjunction of two or more expressions, sorted, which the Horn
    /// rules create
    fn intern(&mut self, core: &[Ex]) -> Ex {
        let _ = core;
        unreachable!("the Horn rules run only in a saturation")
    }
    /// what the contexts of Tier D share
    fn dl(&self) -> &super::dl_rules::DlRun {
        unreachable!("Tier D runs only in a saturation")
    }
    /// whether the context has conclusions of its own waiting
    fn busy(&self) -> bool {
        false
    }
    /// whether the sink sends the products of the context's backward links with its
    /// forward links and propagations itself, once the activation's to-do list is empty
    /// ([`Run::flush_links`]), so that the rules of `Back`, `Fwd` and the existential
    /// subsumers leave them out. A rule calls it when it has stored such a link or
    /// propagation, so that the sink knows a flush has work.
    fn defer(&mut self) -> bool {
        false
    }
    /// whether the run goes on, after a check of its bounds, for rules whose one
    /// inference can make many conclusions
    fn go_on(&self) -> bool {
        true
    }
    /// whether the run goes on, after a check of its bounds that counts the messages the
    /// sink has sent as waiting, for a rule whose one inference sends many messages
    /// before the activation applies a conclusion
    fn sent_go_on(&mut self) -> bool {
        self.go_on()
    }
    /// Counts `n` conclusions of Tier D for the run's bound on derived conclusions.
    fn worked(&mut self, n: usize) {
        self.dl().count(n);
    }
    /// whether the activation has derived its share of a step of a run in steps, so that
    /// it processes no further clause
    fn pause(&self) -> bool {
        false
    }
    /// whether the run has stopped, without a check of its bounds
    fn stopped(&self) -> bool {
        false
    }
}

impl Sink for Out<'_, '_> {
    fn reserve_data(&mut self, bytes: usize) -> bool {
        if bytes > isize::MAX as usize
            || self
                .run
                .memory
                .load(Ordering::Relaxed)
                .checked_add(bytes)
                .is_none()
        {
            self.data_overflow();
            return false;
        }
        // An inference of a run in steps that reserves as it goes ends as one that does
        // not: once the activation alone has derived more than the run's bound
        // ([`Sink::go_on`]). The caller releases what it reserved.
        self.run.check(0, bytes, 0) && !(self.run.steps && self.dl().past(self.work))
    }
    fn data_overflow(&mut self) {
        self.run.peak.fetch_max(usize::MAX, Ordering::Relaxed);
        self.run.halt(STOP_MEMORY);
    }
    /// The bytes wait in the activation until they pass [`KEEP_BATCH`] or it ends, so that
    /// the rules that keep a little at a time do not each update the run's shared counts,
    /// which four threads contend for.
    fn keep_data(&mut self, bytes: usize) {
        self.kept += bytes;
        if self.kept >= KEEP_BATCH {
            let kept = std::mem::take(&mut self.kept);
            let _ = self.reserve_data(kept);
        }
    }
    fn release_data(&mut self, bytes: usize) {
        // what waits uncharged comes off first, and only the rest leaves the count
        let waiting = bytes.min(self.kept);
        self.kept -= waiting;
        let bytes = bytes - waiting;
        if bytes == 0 {
            return;
        }
        // a caller releases what it reserved, and a reservation the overflow check refused
        // before it added anything is released as well, so the count stops at zero
        let _ = self
            .run
            .memory
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                Some(m.saturating_sub(bytes))
            });
    }

    #[inline]
    fn send(&mut self, to: Ex, c: Concl) {
        if to == self.me {
            self.local.push(c);
        } else if self.run.steps {
            // a step delivers its messages at its end, in an order that the threads do
            // not decide
            self.outbox.steps.push((to, c));
            self.sent += 1;
        } else if !self.run.batch.load(Ordering::Relaxed) {
            // one worker meets no other at a to-do list, and sorting costs more than it
            // saves
            let ctx = self.run.get(to);
            ctx.todo.lock().push(c);
            self.sent += 1;
            if self.run.mark(ctx) {
                self.woke.push_back(to);
            }
        } else {
            self.outbox.push(to, c);
            self.sent += 1;
            if self.outbox.full() {
                self.run.deliver(self.outbox, self.woke);
            }
        }
    }

    #[inline]
    fn me(&mut self, c: Concl) {
        self.local.push(c);
    }

    fn merged(&mut self) {
        if !self.run.merged.load(Ordering::Relaxed) {
            self.run.merged.store(true, Ordering::Relaxed);
        }
    }

    fn core(&self, ix: &Index, e: Ex, out: &mut Vec<Ex>) {
        out.clear();
        if (e as usize) < self.run.ctx.len() {
            flatten(ix, e, out);
        } else {
            out.extend_from_slice(self.run.get_dynamic_core(e));
        }
    }

    fn intern(&mut self, core: &[Ex]) -> Ex {
        self.run
            .dynamic
            .expect("a run with the Horn rules")
            .intern(core)
    }

    fn dl(&self) -> &super::dl_rules::DlRun {
        self.run.bounds.dl.expect("a run with Tier D")
    }

    #[inline]
    fn busy(&self) -> bool {
        !self.local.is_empty()
    }

    #[inline]
    fn defer(&mut self) -> bool {
        self.pending = true;
        true
    }

    fn go_on(&self) -> bool {
        // An activation of a run in steps that alone derived more than the run's bound
        // ends its inference, and the run is past its bound whatever the others derive.
        // The activation decides it from its own count, so no schedule changes it.
        if self.run.steps && self.dl().past(self.work) {
            return false;
        }
        self.run.check(0, 0, 0)
    }

    fn sent_go_on(&mut self) -> bool {
        if self.run.steps && self.dl().past(self.work) {
            return false;
        }
        // the messages count as waiting from here on, as they do when the activation
        // counts them after a conclusion
        let queued = std::mem::take(&mut self.sent) as isize;
        self.run.check(0, 0, queued)
    }

    #[inline]
    fn worked(&mut self, n: usize) {
        self.work += n;
        self.dl().count(n);
    }

    #[inline]
    fn pause(&self) -> bool {
        self.run.steps && (self.work >= STEP_WORK || self.dl().past(self.work))
    }

    #[inline]
    fn stopped(&self) -> bool {
        self.run.stopped()
    }
}

impl Out<'_, '_> {
    /// Sends the same messages to each destination in turn, as [`Sink::send`] does one
    /// at a time. An outbox writes them once and gives each destination a run that
    /// points at them, a run without one takes each to-do list's lock once, and a run in
    /// steps keeps a message for each destination in its own list (roadmap item 889).
    fn send_all(&mut self, dests: &[Ex], msgs: &[Concl]) {
        if msgs.is_empty() {
            return;
        }
        if self.run.steps {
            for &to in dests {
                for &c in msgs {
                    self.send(to, c);
                }
            }
            return;
        }
        let batch = self.run.batch.load(Ordering::Relaxed);
        let mut at = None;
        for &to in dests {
            if to == self.me {
                self.local.extend_from_slice(msgs);
                continue;
            }
            self.sent += msgs.len();
            if batch {
                let (start, end) = match at {
                    Some(at) => at,
                    None => *at.insert(self.outbox.shared(msgs)),
                };
                self.outbox.to(to, start, end);
            } else {
                let ctx = self.run.get(to);
                ctx.todo.lock().extend_from_slice(msgs);
                if self.run.mark(ctx) {
                    self.woke.push_back(to);
                }
            }
        }
        if batch && self.outbox.full() {
            self.run.deliver(self.outbox, self.woke);
        }
    }
}

impl<'a> Run<'a> {
    fn get_dynamic_core(&self, e: Ex) -> &'a [Ex] {
        self.dynamic.expect("a run with the Horn rules").core(e)
    }
}

/// The expressions an expression is the intersection of, sorted, without `owl:Thing`.
pub fn flatten(ix: &Index, e: Ex, out: &mut Vec<Ex>) {
    let start = out.len();
    let mut stack = vec![e];
    while let Some(x) = stack.pop() {
        match ix.head(x).node {
            Node::Conj(a, b) => {
                stack.push(a);
                stack.push(b);
            }
            Node::Top => {}
            _ => out.push(x),
        }
    }
    out[start..].sort_unstable();
    let mut v = out.split_off(start);
    v.dedup();
    out.extend(v);
}

/// The contexts of conjunctions that the Horn rules create during a saturation, with ids
/// above the index's: a sharded table from each conjunction, as its sorted expressions,
/// to its id, and an append-only table of the conjunctions and their contexts whose
/// entries never move, so that a worker reads a context without a lock on the table.
pub struct Dynamic {
    base: Ex,
    shards: Box<[Shard]>,
    next: std::sync::atomic::AtomicU32,
    segs: Box<[OnceLock<Box<[Slot]>>]>,
    bytes: AtomicUsize,
}

/// One shard of [`Dynamic`]'s table from conjunctions to ids.
type Shard = Mutex<FxHashMap<Box<[Ex]>, Ex>>;

#[derive(Default)]
struct Slot {
    core: OnceLock<Box<[Ex]>>,
    ctx: OnceLock<Box<Ctx>>,
}

/// The slots of the first segment of [`Dynamic`]; each further one doubles.
const SEG0: usize = 1024;
/// The shards of [`Dynamic`]'s table.
const SHARDS: usize = 64;

impl Dynamic {
    pub fn new(base: Ex) -> Dynamic {
        Dynamic {
            base,
            shards: (0..SHARDS)
                .map(|_| Mutex::new(FxHashMap::default()))
                .collect(),
            next: std::sync::atomic::AtomicU32::new(0),
            segs: (0..32).map(|_| OnceLock::new()).collect(),
            bytes: AtomicUsize::new(0),
        }
    }

    fn slot(&self, e: Ex) -> &Slot {
        let off = (e - self.base) as usize;
        let k = (off / SEG0 + 1).ilog2() as usize;
        let at = off - SEG0 * ((1 << k) - 1);
        let seg = self.segs[k].get_or_init(|| {
            self.bytes
                .fetch_add((SEG0 << k) * std::mem::size_of::<Slot>(), Ordering::Relaxed);
            (0..SEG0 << k).map(|_| Slot::default()).collect()
        });
        &seg[at]
    }

    /// The id of a conjunction of two or more sorted expressions.
    pub fn intern(&self, core: &[Ex]) -> Ex {
        use std::hash::BuildHasher;
        debug_assert!(core.len() >= 2 && core.is_sorted());
        let h = rustc_hash::FxBuildHasher.hash_one(core);
        let mut m = self.shards[(h >> 32) as usize % SHARDS].lock();
        if let Some(&e) = m.get(core) {
            return e;
        }
        let e = self.base + self.next.fetch_add(1, Ordering::Relaxed);
        let b: Box<[Ex]> = core.into();
        let _ = self.slot(e).core.set(b.clone());
        self.bytes.fetch_add(
            2 * core.len() * 4 + 64 + std::mem::size_of::<Ctx>(),
            Ordering::Relaxed,
        );
        m.insert(b, e);
        e
    }

    /// The id of a conjunction the table holds, without creating one.
    pub fn find(&self, core: &[Ex]) -> Option<Ex> {
        use std::hash::BuildHasher;
        let h = rustc_hash::FxBuildHasher.hash_one(core);
        self.shards[(h >> 32) as usize % SHARDS]
            .lock()
            .get(core)
            .copied()
    }

    /// The first id of the table's contexts.
    pub fn base(&self) -> Ex {
        self.base
    }

    /// Moves the table's contexts to the ids from `base` upward, when the index of the
    /// next run has more expressions (roadmap item 753). The caller shifts the ids that
    /// the kept states name ([`State::shift_contexts`]).
    pub fn rebase(&mut self, base: Ex) {
        debug_assert!(base >= self.base);
        let delta = base - self.base;
        if delta == 0 {
            return;
        }
        self.base = base;
        for shard in self.shards.iter_mut() {
            for e in shard.get_mut().values_mut() {
                *e += delta;
            }
        }
    }

    /// Whether the id is one of the table's contexts.
    pub fn holds(&self, e: Ex) -> bool {
        e >= self.base && ((e - self.base) as usize) < self.len()
    }

    pub fn core(&self, e: Ex) -> &[Ex] {
        self.slot(e).core.get().map_or(&[], |c| c)
    }

    pub fn ctx(&self, e: Ex) -> &Ctx {
        self.slot(e).ctx.get_or_init(Box::default)
    }

    /// The context of a conjunction, when the saturation has made one.
    pub fn get_ctx(&self, e: Ex) -> Option<&Ctx> {
        self.slot(e).ctx.get().map(|c| &**c)
    }

    /// The contexts created.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.next.load(Ordering::Relaxed) as usize
    }

    /// The bytes of the tables and the conjunctions.
    pub fn bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Tables/cores and the states of initialized contexts, without creating any.
    pub fn retained_bytes(&self) -> usize {
        self.bytes()
            + (0..self.len() as Ex)
                .filter_map(|i| self.slot(self.base + i).ctx.get())
                .map(|c| c.st.lock().bytes())
                .sum::<usize>()
    }
}

impl<'a> Run<'a> {
    #[inline]
    fn get(&self, e: Ex) -> &'a Ctx {
        match self.ctx.get(e as usize) {
            Some(c) => c.get_or_init(Box::default),
            None => self.get_dynamic(e),
        }
    }

    /// The context of a conjunction that the Horn rules created.
    #[cold]
    #[inline(never)]
    fn get_dynamic(&self, e: Ex) -> &'a Ctx {
        self.dynamic
            .expect("a context above the index's in a run with the Horn rules")
            .ctx(e)
    }

    /// Marks a context active, and says whether this call did, so that the caller
    /// schedules it. The plain load first keeps a busy context's flag from bouncing
    /// between cores. It cannot miss a context that is going idle: a sender pushes to the
    /// to-do list before it looks at the flag, and the processing worker clears the flag
    /// before it looks at the list, both under the list's lock.
    #[inline]
    fn mark(&self, c: &Ctx) -> bool {
        !(c.active.load(Ordering::SeqCst) || c.active.swap(true, Ordering::SeqCst))
    }

    /// Delivers a worker's outbox: the messages grouped by destination, each
    /// destination's to-do list taken once, and the contexts they activate added to the
    /// worker's queue.
    fn deliver(&self, outbox: &mut Outbox, mine: &mut VecDeque<Ex>) {
        if outbox.is_empty() {
            return;
        }
        outbox.group();
        let filter = self.congested();
        let (msgs, runs) = (&outbox.msgs, &outbox.runs);
        let mut at = 0;
        while at < runs.len() {
            let to = runs[at].to;
            let end = at + runs[at..].iter().take_while(|d| d.to == to).count();
            let ctx = self.get(to);
            if !(filter && self.deliver_new(ctx, &runs[at..end], msgs)) {
                let mut t = ctx.todo.lock();
                for d in &runs[at..end] {
                    t.extend_from_slice(&msgs[d.start as usize..d.end as usize]);
                }
            }
            if self.mark(ctx) {
                mine.push_back(to);
            }
            at = end;
        }
        outbox.clear();
    }

    /// Delivers to a context the messages it does not already hold, and says whether it
    /// could look, which it cannot while another worker holds the context (roadmap item
    /// 695). A held conclusion is one the rules have applied, and applying it again does
    /// nothing, so dropping it here is the same as applying it later, for every kind that
    /// [`State::holds`] answers, forward links and loops included (roadmap item 818).
    #[cold]
    #[inline(never)]
    fn deliver_new(&self, ctx: &Ctx, runs: &[Dest], msgs: &[Concl]) -> bool {
        let Some(st) = ctx.st.try_lock() else {
            return false;
        };
        if !st.init {
            return false;
        }
        let mut t = ctx.todo.lock();
        let before = t.len();
        let mut total = 0;
        for d in runs {
            let run = &msgs[d.start as usize..d.end as usize];
            total += run.len();
            st.add_new(run, &mut t);
        }
        let dropped = total - (t.len() - before);
        if dropped > 0 {
            self.queued.fetch_sub(dropped as isize, Ordering::Relaxed);
        }
        true
    }

    /// Sends the products of a context's backward links with its forward links and its
    /// propagations: the subsumer each propagation gives each source, and the links the
    /// chains compose from each pair of a backward and a forward link, which the rules of
    /// `Back`, `Fwd` and the existential subsumers would otherwise send one conclusion at
    /// a time, interleaved by destination. What is new since the last flush, as
    /// `out.marks` records, pairs with everything, and what is old pairs with what is
    /// new, so that each pair is sent once. The messages leave in one run per source or
    /// target, so that the outbox groups them without sorting each message (roadmap item
    /// 818). Says whether the run goes on, after the checks of the bounds a long flush
    /// makes, and leaves the marks at the lists' ends.
    fn flush_links(&self, st: &State, out: &mut Out<'_, '_>, tally: &mut Tally) -> bool {
        if !out.pending {
            return true;
        }
        out.pending = false;
        let roles = &self.ix.roles;
        let mut marks = std::mem::take(out.marks);
        marks.back.resize(st.back.len(), 0);
        let fwd_mark = marks.fwd.min(st.fwd.list.len());
        let props_mark = marks.props.min(st.props.len());
        let (new_fwd, old_fwd) = (&st.fwd.list[fwd_mark..], &st.fwd.list[..fwd_mark]);
        // a check of the bounds after every SEND_EVERY messages, as the rules make them; a
        // run that answers in part keeps sending, so that its contexts hold every product
        let mut ok = true;
        'sources: for (i, (r1, srcs)) in st.back.iter().enumerate() {
            let mark = marks.back[i].min(srcs.list.len());
            let (new_srcs, old_srcs) = (&srcs.list[mark..], &srcs.list[..mark]);
            // the new sources take every propagation and every forward link, the old ones
            // those that are new
            for (sources, props, fwds) in [
                (new_srcs, &st.props[..], &st.fwd.list[..]),
                (old_srcs, &st.props[props_mark..], new_fwd),
            ] {
                if sources.is_empty() {
                    continue;
                }
                let r1 = *r1;
                let subs = props
                    .iter()
                    .filter(|&&(s, _)| !st.bottom && roles.sub(r1, s))
                    .map(|&(_, x)| Concl::Sub(x));
                // a left-linear composition keeps no forward link (roadmap item 956)
                let links = fwds.iter().flat_map(|&(r2, f)| {
                    roles.comp_fwd(r1, r2).iter().map(move |&t| {
                        if t == NO_ROLE {
                            Concl::Sub(BOTTOM)
                        } else {
                            Concl::Fwd(t, f)
                        }
                    })
                });
                let mut products = subs.chain(links);
                loop {
                    marks.run.clear();
                    marks.run.extend(products.by_ref().take(RUN_CHUNK));
                    if marks.run.is_empty() {
                        break;
                    }
                    // the sources take the chunk a slice at a time, so that the bounds
                    // are checked as often as when each message was sent alone
                    let slice = (SEND_EVERY / marks.run.len()).max(1);
                    for some in sources.chunks(slice) {
                        out.send_all(some, &marks.run);
                        if out.sent >= SEND_EVERY && !self.flush_sends(out, tally) {
                            ok = false;
                            if !self.bounds.partial {
                                break 'sources;
                            }
                        }
                    }
                }
            }
        }
        // the backward link each composed link gives its target: new targets take every
        // source, old targets the new sources. Consecutive forward links by one role take
        // the same links, which go to their targets together, so that each target
        // receives its messages in the order of the forward links.
        if ok || self.bounds.partial {
            'targets: for (fwds, all) in [(new_fwd, true), (old_fwd, false)] {
                let mut next = 0;
                while next < fwds.len() {
                    let r2 = fwds[next].0;
                    let n = fwds[next..].iter().take_while(|&&(r, _)| r == r2).count();
                    marks.dests.clear();
                    marks
                        .dests
                        .extend(fwds[next..next + n].iter().map(|&(_, f)| f));
                    next += n;
                    let back_marks = &marks.back;
                    let mut links = st.back.iter().enumerate().flat_map(|(i, (r1, srcs))| {
                        let mark = back_marks[i].min(srcs.list.len());
                        let sources = if all {
                            &srcs.list[..]
                        } else {
                            &srcs.list[mark..]
                        };
                        roles
                            .comp(*r1, r2)
                            .iter()
                            .flat_map(move |&t| sources.iter().map(move |&e| Concl::Back(e, t)))
                    });
                    loop {
                        marks.run.clear();
                        marks.run.extend(links.by_ref().take(RUN_CHUNK));
                        if marks.run.is_empty() {
                            break;
                        }
                        let slice = (SEND_EVERY / marks.run.len()).max(1);
                        for some in marks.dests.chunks(slice) {
                            out.send_all(some, &marks.run);
                            if out.sent >= SEND_EVERY && !self.flush_sends(out, tally) {
                                ok = false;
                                if !self.bounds.partial {
                                    break 'targets;
                                }
                            }
                        }
                    }
                }
            }
        }
        marks.set(st);
        *out.marks = marks;
        ok
    }

    /// Adds the messages a context sent since its last check to the totals and checks the
    /// bounds.
    #[inline]
    fn flush_sends(&self, out: &mut Out<'_, '_>, tally: &mut Tally) -> bool {
        tally.queued += out.sent as isize;
        out.sent = 0;
        self.flush(tally)
    }

    /// Whether the messages waiting in to-do lists and outboxes take more than the stored
    /// conclusions and [`CONGESTED_BYTES`], as of the workers' last checks.
    fn congested(&self) -> bool {
        #[cfg(test)]
        if self.always_congested {
            return true;
        }
        let waiting = self.queued.load(Ordering::Relaxed).max(0) as usize;
        waiting * std::mem::size_of::<Concl>()
            > self.memory.load(Ordering::Relaxed).max(CONGESTED_BYTES)
    }

    /// Makes a run on one worker send through its outbox once it is congested, so that
    /// its deliveries drop the messages their contexts hold, as those of several workers
    /// do (roadmap item 721).
    #[cold]
    #[inline(never)]
    fn batch_if_congested(&self) {
        if self.congested() {
            self.batch.store(true, Ordering::Relaxed);
        }
    }

    /// Moves half of a worker's queue to the shared queue when a worker waits for it.
    fn share(&self, mine: &mut VecDeque<Ex>) {
        let idle = self.idle.load(Ordering::SeqCst);
        if idle == 0 || mine.is_empty() {
            return;
        }
        let give = mine.len().div_ceil(2);
        // counted before another worker can take them
        self.pending.fetch_add(give, Ordering::SeqCst);
        let mut q = self.queue.lock();
        q.extend(mine.drain(..give));
        if give >= idle {
            self.ready.notify_all();
        } else {
            for _ in 0..give {
                self.ready.notify_one();
            }
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed) != STOP_NONE
    }

    fn halt(&self, why: u8) {
        if super::dl_rules::tracing() {
            eprintln!(
                "TR HALT why={why} peak={} memory={}",
                self.peak.load(Ordering::Relaxed),
                self.memory.load(Ordering::Relaxed)
            );
        }
        let _ = self
            .stop
            .compare_exchange(STOP_NONE, why, Ordering::SeqCst, Ordering::SeqCst);
        let _q = self.queue.lock();
        self.ready.notify_all();
    }

    /// Takes the next root this worker activates, from its chunk of the roots or a new
    /// one. Roots already saturated or active are passed over.
    fn next_root(&self, chunk: &mut Vec<Ex>) -> Option<Ex> {
        loop {
            if let Some(r) = chunk.pop() {
                return Some(r);
            }
            let at = self.next_root.fetch_add(ROOT_CHUNK, Ordering::Relaxed);
            if at >= self.roots.len() {
                return None;
            }
            let end = (at + ROOT_CHUNK).min(self.roots.len());
            for &r in self.roots[at..end].iter().rev() {
                let c = self.get(r);
                if !c.started.load(Ordering::Acquire) && self.mark(c) {
                    chunk.push(r);
                }
            }
        }
    }

    /// Runs the saturation on `threads` workers with the rules that `R` holds. Each rule
    /// set has a worker of its own that inlines its activation, so that the worker of the
    /// EL rules does not hold the activations of the Horn and disjunctive rules
    /// (roadmap items 711 and 715).
    ///
    /// The calling thread is the first worker. A run of many roots starts the others at
    /// once, and a smaller one when the first has run for [`lazy_start`], so that a
    /// small ontology, whose saturation takes less time than starting threads, runs on
    /// one (roadmap items 709 and 720).
    fn workers<R: Rules>(&self, threads: usize) {
        #[cfg(test)]
        WORKER.with(|w| w.set(FIRST_WORKER));
        if threads == 1 {
            self.worker::<R>(None);
            #[cfg(test)]
            WORKER.with(|w| w.set(0));
            return;
        }
        std::thread::scope(|s| {
            let start = || {
                // counted while the first worker is busy, so that the count cannot reach 0
                // before they start
                self.pending.fetch_add(threads - 1, Ordering::SeqCst);
                let called = Instant::now();
                for _ in 1..threads {
                    s.spawn(move || {
                        #[cfg(test)]
                        WORKER.with(|w| w.set(HELPER));
                        self.worker::<R>(None)
                    });
                }
                // the saturations that start after this one wait as long, also while
                // this one runs
                let ns = called.elapsed().as_nanos().clamp(1, u64::MAX as u128);
                self.started.store(ns as u64, Ordering::Relaxed);
            };
            self.worker::<R>(Some(&start));
        });
        #[cfg(test)]
        WORKER.with(|w| w.set(0));
    }

    /// A worker activates up to [`ROOTS_PER_ROUND`] roots, then the contexts those
    /// activations woke, in the order they woke, and the contexts these wake in turn,
    /// before it takes more roots. A context that many others send to, such as a class
    /// that fills many existential restrictions, then takes the conclusions of a round
    /// in one activation rather than one at a time.
    fn worker<R: Rules>(&self, mut others: Option<&dyn Fn()>) {
        // dropped last, after what the worker holds
        let _halt = HaltOnPanic::new(self);
        if self.roots.len() >= EAGER_ROOTS
            && let Some(start) = others.take()
        {
            start();
        }
        let began = others.map(|_| (Instant::now(), lazy_start(self.started)));
        let mut mine: VecDeque<Ex> = VecDeque::new();
        let mut chunk: Vec<Ex> = Vec::new();
        let mut round = 0usize;
        let mut tally = Tally::default();
        // the buffer of a context's conclusions, kept across contexts
        let mut local: Vec<Concl> = Vec::new();
        let mut outbox = Outbox::default();
        let mut marks = Marks::default();
        loop {
            if round >= ROOTS_PER_ROUND && mine.is_empty() {
                round = 0;
            }
            let root = if round < ROOTS_PER_ROUND {
                self.next_root(&mut chunk)
            } else {
                None
            };
            let next = if let Some(r) = root {
                round += 1;
                r
            } else if let Some(e) = mine.pop_front() {
                e
            } else if !outbox.is_empty() {
                // the messages the worker holds go out before it looks for more work
                self.deliver(&mut outbox, &mut mine);
                continue;
            } else {
                // no roots are left: what the worker counted goes to the totals before
                // it waits for the others
                round = 0;
                if tally.applied > 0 || tally.stored > 0 || tally.queued != 0 {
                    self.flush(&mut tally);
                }
                // the worker goes idle; the last to do so with the queue empty ends the
                // saturation
                if self.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
                    let _q = self.queue.lock();
                    self.ready.notify_all();
                    return;
                }
                let mut q = self.queue.lock();
                loop {
                    if self.stopped() {
                        return;
                    }
                    // a context taken from the queue makes the worker busy again, which
                    // leaves the count as it was
                    if let Some(e) = q.pop() {
                        break e;
                    }
                    if self.pending.load(Ordering::SeqCst) == 0 {
                        return;
                    }
                    self.idle.fetch_add(1, Ordering::SeqCst);
                    self.ready.wait(&mut q);
                    self.idle.fetch_sub(1, Ordering::SeqCst);
                }
            };
            if self.stopped() {
                self.flush(&mut tally);
                // the messages the worker holds reach their contexts' to-do lists, which
                // then show the work left for a partial answer
                if self.bounds.partial {
                    self.deliver(&mut outbox, &mut mine);
                }
                return;
            }
            self.process::<R>(
                next,
                &mut mine,
                &mut tally,
                &mut local,
                &mut outbox,
                &mut marks,
            );
            // a Horn or disjunctive activation can take tens of microseconds, so the
            // first worker looks at the clock after each
            if let Some(start) = others
                && began.is_some_and(|(b, wait)| b.elapsed() >= wait)
            {
                others = None;
                start();
            }
            if !outbox.is_empty() && self.idle.load(Ordering::Relaxed) > 0 {
                self.deliver(&mut outbox, &mut mine);
            }
            self.share(&mut mine);
        }
    }

    /// Runs a saturation of Tier D in steps, so that what it derives does not depend on
    /// the threads (roadmap item 727). The frontier is a queue of active contexts, which
    /// the seeds start and the roots join while it is short. A step takes its first
    /// [`STEP_CONTEXTS`] entries. The workers activate them, and an activation only reads
    /// its context's state and to-do list and appends its messages to its worker's list,
    /// so that no to-do list changes meanwhile. Then the workers deliver the messages:
    /// each takes the destinations of its share and reads every activation's messages in
    /// the order of the frontier, so that a to-do list receives its messages in the
    /// order of their senders in the frontier and, for one sender, the order it sent
    /// them. The contexts the deliveries woke and those that gave way join the queue
    /// sorted. The bound on derived conclusions is compared between steps, where the
    /// count is a sum over the step's activations. A stop by the memory budget, the
    /// deadline or a cancellation still comes when its check sees it.
    fn steps<R: Rules>(&self, threads: usize) {
        let sh = Steps {
            frontier: RwLock::new(Vec::new()),
            next: AtomicUsize::new(0),
            outs: (0..threads).map(|_| RwLock::default()).collect(),
            woke: (0..threads).map(|_| Mutex::default()).collect(),
            order: RwLock::new(Vec::new()),
            gate: Mutex::new(Gate {
                round: 0,
                phase: STEP_ACTIVATE,
                busy: 0,
                failed: false,
            }),
            opened: Condvar::new(),
            left: Condvar::new(),
        };
        let mut queue: VecDeque<Ex> = {
            let mut seeds = std::mem::take(&mut *self.queue.lock());
            seeds.sort_unstable();
            seeds.into()
        };
        let mut root = 0;
        std::thread::scope(|s| {
            // dropped last, so that the helpers end however this thread leaves
            let _helpers = Helpers::new(self, &sh);
            let mut helpers = false;
            // whether the last step's activations took long
            let mut heavy = false;
            // The contexts that gave way and have no message. They go on once no other
            // context is active, since what the others conclude can close them, as
            // `Run::process` says of a context that gives way.
            let mut later: Vec<Ex> = Vec::new();
            loop {
                while queue.len() < STEP_CONTEXTS && root < self.roots.len() {
                    let c = self.get(self.roots[root]);
                    if !c.started.load(Ordering::Acquire) && self.mark(c) {
                        queue.push_back(self.roots[root]);
                    }
                    root += 1;
                }
                if queue.is_empty() {
                    later.sort_unstable();
                    queue.extend(later.drain(..));
                }
                if queue.is_empty() {
                    break;
                }
                let n = queue.len().min(STEP_CONTEXTS);
                {
                    let mut f = sh.frontier.write();
                    f.clear();
                    f.extend(queue.drain(..n));
                }
                sh.next.store(0, Ordering::SeqCst);
                // Which workers take a phase does not change what it does, so the clock
                // may decide it: the helpers join for a long frontier, for one whose
                // contexts were slow in the last step, and for many messages.
                let together = threads > 1 && n >= 2 && (n >= STEP_PARALLEL || heavy);
                if together && !helpers {
                    helpers = true;
                    for w in 1..threads {
                        let sh = &sh;
                        s.spawn(move || {
                            let mut seen = 0;
                            loop {
                                let phase = {
                                    let mut g = sh.gate.lock();
                                    while g.round == seen {
                                        sh.opened.wait(&mut g);
                                    }
                                    seen = g.round;
                                    g.phase
                                };
                                if phase == STEP_DONE {
                                    break;
                                }
                                let _in = InPhase(self, sh);
                                if phase == STEP_ACTIVATE {
                                    self.step_activate::<R>(sh, w);
                                } else {
                                    self.step_deliver(sh, w, threads);
                                }
                            }
                        });
                    }
                }
                // a phase on every worker, or on this one alone, and whether every
                // helper came through it
                let phase = |p: u8, together: bool| {
                    if together {
                        let mut g = sh.gate.lock();
                        g.round += 1;
                        g.phase = p;
                        g.busy = threads - 1;
                        drop(g);
                        sh.opened.notify_all();
                    }
                    if p == STEP_ACTIVATE {
                        self.step_activate::<R>(&sh, 0);
                    } else {
                        self.step_deliver(&sh, 0, if together { threads } else { 1 });
                    }
                    if !together {
                        return true;
                    }
                    let mut g = sh.gate.lock();
                    while g.busy > 0 {
                        sh.left.wait(&mut g);
                    }
                    !g.failed
                };
                let began = Instant::now();
                // a helper that panicked has stopped the run, and the scope passes its
                // panic on once the others have ended
                if !phase(STEP_ACTIVATE, together) {
                    break;
                }
                heavy = began.elapsed() >= STEP_HEAVY;
                let mut messages = 0;
                {
                    let mut order = sh.order.write();
                    order.clear();
                    for (w, o) in sh.outs.iter().enumerate() {
                        let mut from = 0;
                        for &(at, to) in &o.read().segs {
                            if to > from {
                                order.push((at, w as u32, from, to));
                            }
                            from = to;
                        }
                        messages += from;
                    }
                    order.sort_unstable();
                }
                if !phase(STEP_DELIVER, helpers && messages >= STEP_MESSAGES) {
                    break;
                }
                let mut woke: Vec<Ex> = Vec::new();
                for (o, w) in sh.outs.iter().zip(&sh.woke) {
                    let mut o = o.write();
                    later.extend(o.again.drain(..));
                    o.outbox.steps.clear();
                    o.segs.clear();
                    woke.append(&mut w.lock());
                }
                // a context that gave way and has messages goes on with the others
                later.retain(|&e| {
                    let waits = self.get(e).todo.lock().is_empty();
                    if !waits {
                        woke.push(e);
                    }
                    waits
                });
                woke.sort_unstable();
                woke.dedup();
                queue.extend(woke);
                if !self.stopped() && self.bounds.dl.is_some_and(|d| d.over_work()) {
                    self.halt(STOP_WORK);
                }
                if self.stopped() {
                    break;
                }
            }
        });
        #[cfg(test)]
        WORKER.with(|w| w.set(0));
    }

    /// A worker's part of a step's first phase: it activates the contexts of the
    /// frontier that it takes, and records for each its place in the frontier and the
    /// end of its messages in the worker's list.
    fn step_activate<R: Rules>(&self, sh: &Steps, w: usize) {
        #[cfg(test)]
        WORKER.with(|at| at.set(if w == 0 { STEP_CALLER } else { STEP_HELPER }));
        let frontier = sh.frontier.read();
        let mut o = sh.outs[w].write();
        let o = &mut *o;
        loop {
            if self.stopped() {
                break;
            }
            let at = sh.next.fetch_add(1, Ordering::Relaxed);
            if at >= frontier.len() {
                break;
            }
            self.process::<R>(
                frontier[at],
                &mut o.again,
                &mut o.tally,
                &mut o.local,
                &mut o.outbox,
                &mut o.marks,
            );
            o.segs.push((at as u32, o.outbox.steps.len()));
        }
        self.flush(&mut o.tally);
    }

    /// A worker's part of a step's second phase, as one of `of` workers: it delivers the
    /// messages whose destination is in its share, reading the activations in the order
    /// of the frontier, and records the contexts it woke.
    fn step_deliver(&self, sh: &Steps, w: usize, of: usize) {
        let outs: Vec<_> = sh.outs.iter().map(|o| o.read()).collect();
        let mut woke = sh.woke[w].lock();
        for &(_, from, a, b) in sh.order.read().iter() {
            for &(to, c) in &outs[from as usize].outbox.steps[a..b] {
                // a destination is in the share of one worker
                if ((to as usize).wrapping_mul(0x9E37_79B9) >> 16) % of != w {
                    continue;
                }
                let ctx = self.get(to);
                ctx.todo.lock().push(c);
                if self.mark(ctx) {
                    woke.push(to);
                }
            }
        }
    }

    /// Checks the bounds, and stops the run when one is reached. `bytes` is the growth
    /// of the stored conclusions and `queued` the change in waiting messages since the
    /// worker's last check.
    fn check(&self, applied: usize, bytes: usize, queued: isize) -> bool {
        self.applied.fetch_add(applied, Ordering::Relaxed);
        let held = self.memory.fetch_add(bytes, Ordering::Relaxed) + bytes;
        let waiting = self.queued.fetch_add(queued, Ordering::Relaxed) + queued;
        let dynamic =
            self.dynamic.map_or(0, |d| d.bytes()) + self.bounds.dl.map_or(0, |d| d.bytes());
        let used = held + dynamic + waiting.max(0) as usize * std::mem::size_of::<Concl>();
        let before = self.peak.fetch_max(used, Ordering::Relaxed);
        if self.stopped() {
            return false;
        }
        if !self.batch.load(Ordering::Relaxed) {
            self.batch_if_congested();
        }
        let b = self.bounds;
        let grown = used.saturating_sub(before);
        if b.max_memory.is_some_and(|m| used > m)
            || b.charge.is_some_and(|f| grown > 0 && !f(grown))
        {
            self.halt(STOP_MEMORY);
            return false;
        }
        // the work bound is its own reason, so that the first stop published decides
        // whether the run falls back or answers in part; a run in steps compares it
        // between steps only
        if !self.steps && b.dl.is_some_and(|d| d.over_work()) {
            self.halt(STOP_WORK);
            return false;
        }
        if b.deadline.is_some_and(|d| Instant::now() >= d) {
            self.halt(STOP_TIMEOUT);
            return false;
        }
        if b.cancelled.is_some_and(|f| f()) {
            self.halt(STOP_CANCEL);
            return false;
        }
        true
    }

    /// Adds a worker's counts to the totals and checks the bounds.
    fn flush(&self, t: &mut Tally) -> bool {
        let ok = self.check(t.applied, t.stored * CONCLUSION_BYTES, t.queued);
        *t = Tally::default();
        ok
    }

    /// Activates a context: applies the rules to what waits in its to-do list until none
    /// is left, with the rules that `R` holds. Its one call is in the worker of its rule
    /// set, which it inlines into.
    #[inline(always)]
    fn process<R: Rules>(
        &self,
        me: Ex,
        mine: &mut VecDeque<Ex>,
        tally: &mut Tally,
        local: &mut Vec<Concl>,
        outbox: &mut Outbox,
        marks: &mut Marks,
    ) {
        let ctx = self.get(me);
        let mut st = ctx.st.lock();
        local.clear();
        marks.set(&st);
        let mut out = Out {
            run: self,
            me,
            local,
            woke: mine,
            outbox,
            marks,
            pending: false,
            sent: 0,
            work: 0,
            kept: 0,
        };
        if !st.init {
            tally.stored += CONTEXT_BYTES / CONCLUSION_BYTES;
            if R::DISJ && me as usize >= self.ctx.len() {
                let core = self.dynamic.expect("a run with Tier D").core(me);
                st.init = true;
                dl_rules::init_test(&mut st, core, &mut out);
            } else if R::HORN && me as usize >= self.ctx.len() {
                let core = self.dynamic.expect("a run with the Horn rules").core(me);
                init_conjunction(self.ix, core, &mut st, self.universal, &mut out);
            } else if R::DISJ && self.ix.dl.as_deref().is_some_and(|d| d.is_root(me)) {
                dl_root::init(self.ix, &mut st, &mut out);
            } else {
                init(self.ix, me, &mut st, self.universal, &mut out);
                // owl:Thing and the individuals have an instance in every model, which
                // a run with the Horn rules reads from the start, so that R3 and R5 at
                // a real context send an individual what its successor holds instead
                // of creating a context for the conjunction (roadmap item 869)
                if R::FULL
                    && R::HORN
                    && self.ix.has_individuals()
                    && (me == TOP || matches!(self.ix.head(me).node, Node::Nominal(_)))
                {
                    out.me(Concl::Real);
                }
            }
            ctx.started.store(true, Ordering::Release);
        }
        // the clauses Tier D processed in this activation, with the minimums whose
        // witnesses it released, and whether it gave way
        let mut clauses = 0usize;
        let mut gave_way = false;
        // the batch taken from the to-do list, applied in the order it arrived from its
        // own buffer, and the place of its next conclusion. The conclusions the rules
        // give the context itself come first, as they did when the batch went onto
        // `local` reversed (roadmap item 889).
        let mut taken: Vec<Concl> = Vec::new();
        let mut at = 0;
        'outer: loop {
            loop {
                let c = match out.local.pop() {
                    Some(c) => c,
                    None if at < taken.len() => {
                        at += 1;
                        taken[at - 1]
                    }
                    None => break,
                };
                if apply_rules::<_, R>(self.ix, me, &mut st, c, &mut out) {
                    tally.stored += 1;
                }
                tally.applied += 1;
                if tally.applied >= CHECK_EVERY || out.sent >= SEND_EVERY {
                    if !self.flush_sends(&mut out, tally) {
                        break 'outer;
                    }
                    // a long activation gives idle workers what it woke as it goes, the
                    // products of its links so far included, and one of a run in steps
                    // delivers nothing before its step ends and sends those products
                    // where no schedule decides it
                    if !self.steps && self.idle.load(Ordering::Relaxed) > 0 {
                        if !self.flush_links(&st, &mut out, tally) {
                            break 'outer;
                        }
                        self.deliver(out.outbox, out.woke);
                        self.share(out.woke);
                    }
                }
            }
            // An activation of a run in steps that derived its share of conclusions gives
            // way too, so that the bound is compared before one context derives far more.
            // It sends the products of its links first, as the context below does that
            // processed its share of clauses.
            if self.steps && out.pause() && !st.bottom {
                if !self.flush_links(&st, &mut out, tally) {
                    break 'outer;
                }
                if !out.local.is_empty() {
                    continue;
                }
                gave_way = true;
                break;
            }
            // A context of Tier D that processed its share of clauses, which counts the
            // minimums it released as [`dl_rules::flush`] does, gives way to the
            // contexts this worker woke and goes behind them, with its other clauses left
            // in its store. What they conclude can close it before it derives the rest, as
            // a successor without a model does for a context that is equating its
            // successors, and one worker otherwise finishes that derivation first.
            // The messages in the outbox show first whom they wake. The products of the
            // links and propagations this activation stored go out before that, since the
            // marks of the context's next activation start at the lists' ends and that
            // activation would leave them out. Those for the context itself come back as
            // its own conclusions, which it applies before it gives way. In a run in
            // steps the context gives way whoever waits, so that its activation does not
            // depend on the others'.
            if R::DISJ && clauses >= DL_CLAUSES {
                if !self.flush_links(&st, &mut out, tally) {
                    break 'outer;
                }
                if !out.local.is_empty() {
                    continue;
                }
                if self.steps {
                    gave_way = true;
                    break;
                }
                if out.woke.is_empty() {
                    self.deliver(out.outbox, out.woke);
                }
                if !out.woke.is_empty() {
                    gave_way = true;
                    break;
                }
                clauses = 0;
            }
            // the to-do list leaves the context with its buffer, so that a context keeps
            // no room for the messages it took
            let batch = {
                let mut t = ctx.todo.lock();
                if t.is_empty() {
                    None
                } else {
                    Some(std::mem::take(&mut *t))
                }
            };
            let Some(batch) = batch else {
                // the products of the links and propagations this activation found go out
                // once its own conclusions are applied, and those for the context itself
                // come back as conclusions of its own
                if !self.flush_links(&st, &mut out, tally) {
                    break 'outer;
                }
                if !out.local.is_empty() {
                    continue;
                }
                // R3 of the Horn rules waits for the activation's universals
                if R::HORN && horn_rules::flush(self.ix, me, &mut st, &mut out) {
                    continue;
                }
                // Tier D's clauses wait for the units, which the EL rules take first
                if R::DISJ {
                    // an activation of a run in steps has one share of clauses in all
                    let share = if self.steps {
                        DL_CLAUSES - clauses
                    } else {
                        DL_CLAUSES
                    };
                    let n = if self.ix.dl.as_deref().is_some_and(|d| d.is_root(me)) {
                        dl_root::flush(self.ix, &mut st, &mut out, share)
                    } else {
                        dl_rules::flush(self.ix, me, &mut st, &mut out, share)
                    };
                    if n > 0 {
                        clauses += n;
                        tally.applied += n;
                        if !self.flush_sends(&mut out, tally) {
                            break 'outer;
                        }
                        continue;
                    }
                }
                break;
            };
            tally.queued -= batch.len() as isize;
            taken = batch;
            at = 0;
        }
        // A run that answers in part when it stops gives back the conclusions it did not
        // apply, so that the context shows as not closed, and sends the products of the
        // links it did apply, so that its state and its messages agree (roadmap item 603).
        if self.bounds.partial {
            self.flush_links(&st, &mut out, tally);
        }
        tally.queued += out.sent as isize;
        if self.bounds.partial && (at < taken.len() || !out.local.is_empty()) {
            // the rest of the batch in the order the reversed batch left it on `local`,
            // below what the rules gave the context
            let mut t = ctx.todo.lock();
            t.extend(taken[at..].iter().rev());
            t.append(out.local);
        }
        // the activation charges what it kept as it ends
        drop(out);
        drop(st);
        ctx.active.store(false, Ordering::SeqCst);
        if !self.stopped() && (gave_way || !ctx.todo.lock().is_empty()) && self.mark(ctx) {
            mine.push_back(me);
        }
    }
}

/// Starts a context: its root, `owl:Thing`, the loops of the reflexive roles and the
/// facts every context has. Which contexts are real is found after a first saturation,
/// and only when a context derived an individual, so that an ontology without such a
/// context sends no message for it.
pub fn init<S: Sink>(ix: &Index, me: Ex, st: &mut State, universal: &[Ex], out: &mut S) {
    st.init = true;
    out.me(Concl::Sub(me));
    out.me(Concl::Sub(TOP));
    for &r in &ix.roles.reflexive {
        out.me(Concl::Loop(r));
    }
    for &x in universal {
        out.me(Concl::Sub(x));
    }
}

/// The individuals a context stands for: its root, when it is one, and the individuals
/// among its subsumers.
fn individuals(ix: &Index, me: Ex, st: &State, mut f: impl FnMut(Ex)) {
    if matches!(ix.head(me).node, Node::Nominal(_)) {
        f(me);
    }
    for &n in st.noms() {
        f(n);
    }
}

/// Applies the rules to one conclusion, in a world: every rule of the EL calculus, with
/// the Horn rules when the index has them. The answer says whether it was new.
pub fn apply<S: Sink>(ix: &Index, me: Ex, st: &mut State, c: Concl, out: &mut S) -> bool {
    if ix.horn.is_some() {
        apply_rules::<S, HornFull>(ix, me, st, c, out)
    } else {
        apply_rules::<S, Full>(ix, me, st, c, out)
    }
}

/// Initializes the context of a conjunction that the Horn rules created: it holds
/// `owl:Thing`, each expression of its core, the reflexive loops and the facts of
/// universal roles.
pub fn init_conjunction<S: Sink>(
    ix: &Index,
    core: &[Ex],
    st: &mut State,
    universal: &[Ex],
    out: &mut S,
) {
    st.init = true;
    out.me(Concl::Sub(TOP));
    for &m in core {
        out.me(Concl::Sub(m));
    }
    for &r in &ix.roles.reflexive {
        out.me(Concl::Loop(r));
    }
    for &x in universal {
        out.me(Concl::Sub(x));
    }
}

/// Whether the rules of individuals and functional data roles can apply to the index:
/// when it has no individuals, universal roles or functional data roles, no context
/// derives an individual, becomes real, takes a link to an individual or keeps data
/// values, and the saturation runs the rules without their checks.
pub fn plain(ix: &Index) -> bool {
    !ix.has_individuals() && !ix.nominal_selfs && ix.roles.functional.iter().all(|f| f.is_empty())
}

/// The rules a saturation runs, as constants that the compiler folds, so that each set
/// is its own instantiation of [`apply_rules`] and a rule left out costs no check.
trait Rules {
    /// the rules of individuals and functional data roles, which [`plain`] leaves out
    const FULL: bool;
    /// the rules of Tier H (roadmap item 603)
    const HORN: bool = false;
    /// the rules of Tier D (roadmap item 603)
    const DISJ: bool = false;
}

/// The rules of Tier D over an index that [`plain`] accepts: the EL rules without those
/// of individuals, with the rules of disjunctive clauses beside them.
struct Disj;

impl Rules for Disj {
    const FULL: bool = false;
    const DISJ: bool = true;
}

/// The rules of Tier H over an index that [`plain`] accepts: the EL rules without those
/// of individuals, with the Horn rules beside them.
struct Horn;

impl Rules for Horn {
    const FULL: bool = false;
    const HORN: bool = true;
}

/// The rules of Tier H over an index with individuals, universal roles or functional
/// data roles (roadmap item 692): every rule of the EL calculus with the Horn rules
/// beside them.
struct HornFull;

impl Rules for HornFull {
    const FULL: bool = true;
    const HORN: bool = true;
}

/// The rules of an index without individuals, universal roles or functional data roles.
struct Plain;

/// Every rule of the EL calculus.
struct Full;

impl Rules for Plain {
    const FULL: bool = false;
}

impl Rules for Full {
    const FULL: bool = true;
}

/// [`apply`], with the rules that `R` holds.
#[inline(always)]
fn apply_rules<S: Sink, R: Rules>(
    ix: &Index,
    me: Ex,
    st: &mut State,
    c: Concl,
    out: &mut S,
) -> bool {
    let roles = &ix.roles;
    match c {
        Concl::Sub(d) => {
            if st.bottom || !st.subs.insert(d) {
                return false;
            }
            let h = ix.head(d);
            match h.node {
                Node::Bottom => {
                    st.bottom = true;
                    for (_, srcs) in &st.back {
                        for &e in &srcs.list {
                            out.send(e, Concl::Sub(BOTTOM));
                        }
                    }
                    if R::DISJ {
                        dl_rules::bottom(me, st, out);
                    }
                    if R::FULL && st.extra.is_some() {
                        give(st, BOTTOM, out);
                    }
                    return true;
                }
                Node::Conj(a, b) => {
                    out.me(Concl::Sub(a));
                    out.me(Concl::Sub(b));
                }
                Node::Exist(r, _) => {
                    let succ = h.succ;
                    out.send(succ, Concl::Back(me, r));
                    if roles.keeps_forward(r) {
                        out.me(Concl::Fwd(r, succ));
                    }
                    if R::FULL && st.real {
                        out.send(succ, Concl::Real);
                        if R::HORN {
                            out.send(succ, Concl::RealBack(me));
                        }
                    }
                    if R::HORN {
                        let new = horn_rules::succ(ix, me, st, r, succ, true, out);
                        if R::FULL && new {
                            horn_rules::succ_full(ix, me, st, r, succ, out);
                        }
                    }
                }
                Node::Forall(s, a)
                    if R::HORN
                        && !ix
                            .horn
                            .as_ref()
                            .is_some_and(|h| h.is(d, HornIx::UNFOLDED | HornIx::GLOBAL)) =>
                {
                    horn_rules::forall(ix, me, st, s, a, out)
                }
                Node::AtMost1(s, b) if R::HORN => horn_rules::at_most(ix, me, st, s, b, out),
                Node::SelfLoop(r) => out.me(Concl::Loop(r)),
                Node::Nominal(_) if R::FULL && R::HORN => {
                    if d != me {
                        nominal(ix, me, st, d, out);
                    }
                    horn_rules::derived_nominal(ix, me, st, d, out);
                }
                Node::Nominal(_) if R::FULL && d != me => nominal(ix, me, st, d, out),
                Node::DataSome(r, _) if R::FULL && !roles.functional(r).is_empty() => {
                    functional(ix, st, r, h.succ, out)
                }
                _ => {}
            }
            let wait = if R::DISJ && matches!(h.node, Node::Fresh(_)) {
                dl_rules::defer(ix, st, d)
            } else {
                None
            };
            for &t in ix.told_of(h) {
                if Some(t) != wait {
                    out.me(Concl::Sub(t));
                }
            }
            for &(p, k) in ix.conj_of(h) {
                if st.subs.contains(&p) {
                    out.me(Concl::Sub(k));
                }
            }
            for &(s, x) in ix.exist_of(h) {
                st.props.push((s, x));
                if !out.defer() {
                    for (r, srcs) in &st.back {
                        if roles.sub(*r, s) {
                            for &e in &srcs.list {
                                out.send(e, Concl::Sub(x));
                            }
                        }
                    }
                }
            }
            for &set in ix.disjoint_of(h) {
                match st.disj.entry(set) {
                    Entry::Vacant(v) => {
                        v.insert(d);
                    }
                    Entry::Occupied(o) => {
                        if *o.get() != d {
                            out.me(Concl::Sub(BOTTOM));
                        }
                    }
                }
            }
            if R::FULL && st.extra.is_some() {
                give(st, d, out);
                if R::HORN {
                    horn_rules::spread_sub(st, d, out);
                }
            }
            if R::HORN && ix.horn.as_ref().is_some_and(|h| h.is(d, HornIx::FILLER)) && d != TOP {
                horn_rules::fill(ix, me, st, d, out);
            }
            if R::DISJ {
                dl_rules::sub(ix, me, st, d, out);
            }
            true
        }
        Concl::Back(e, r) => {
            if !st.back_by(r).insert(e) {
                return false;
            }
            if st.bottom {
                out.send(e, Concl::Sub(BOTTOM));
                return true;
            }
            if !out.defer() {
                for &(s, x) in &st.props {
                    if roles.sub(r, s) {
                        out.send(e, Concl::Sub(x));
                    }
                }
                for &(r2, f) in &st.fwd.list {
                    for &t in roles.comp(r, r2) {
                        compose(roles, e, t, f, roles.keeps_composed(r, r2, t), out);
                    }
                }
            }
            if R::FULL && ix.nominal_selfs {
                link_to(ix, me, st, e, r, out);
            }
            if R::HORN {
                debug_assert!(
                    ix.horn
                        .as_ref()
                        .is_none_or(|h| !h.dead_over_inverse(roles, r)),
                    "a backward link under the inverse of a role whose universals the \
                     automata dropped"
                );
                horn_rules::back(ix, me, st, e, r, out);
                if R::FULL {
                    horn_rules::back_full(ix, me, st, e, r, out);
                }
            }
            if R::DISJ {
                dl_rules::back(ix, me, st, e, r, out);
            }
            true
        }
        Concl::Fwd(r2, f) => {
            if !st.fwd.insert((r2, f)) {
                return false;
            }
            if !out.defer() {
                for (r1, srcs) in &st.back {
                    for &t in roles.comp(*r1, r2) {
                        let fwd = roles.keeps_composed(*r1, r2, t);
                        for &e in &srcs.list {
                            compose(roles, e, t, f, fwd, out);
                        }
                    }
                }
            }
            true
        }
        Concl::Loop(r) => {
            if st.loops.contains(&r) {
                return false;
            }
            st.loops.push(r);
            if roles.is_empty_role(r) {
                out.me(Concl::Sub(BOTTOM));
            }
            // a loop is a link from the context to itself, which meets the role's range
            if let Some(c) = roles.range.get(r as usize).copied().flatten() {
                out.me(Concl::Sub(c));
            }
            out.me(Concl::Back(me, r));
            if roles.keeps_forward(r) {
                out.me(Concl::Fwd(r, me));
            }
            for &(s, x) in &ix.selfs {
                if roles.sub(r, s) {
                    out.me(Concl::Sub(x));
                }
            }
            for &l in &st.loops {
                for &t in roles.comp(l, r).iter().chain(roles.comp(r, l)) {
                    out.me(Concl::Loop(t));
                }
            }
            if R::HORN {
                horn_rules::self_loop(ix, me, st, r, out);
                if R::FULL {
                    horn_rules::loop_full(ix, me, st, r, out);
                }
            }
            true
        }
        Concl::Nom(k) if R::FULL => apply_nom(ix, me, st, k, out),
        Concl::Real if R::FULL => apply_real(ix, me, st, out),
        Concl::LinkTo(r, n) if R::FULL => apply_link_to(me, st, r, n, out),
        Concl::Nom(_) | Concl::Real | Concl::LinkTo(..) => {
            unreachable!("a conclusion of individuals in a saturation without them")
        }
        Concl::SuccIn(n, b) if R::HORN => horn_rules::succ_in(ix, me, st, n, b, out),
        Concl::PredIn(m, b) if R::HORN => {
            let new = horn_rules::pred_in(ix, me, st, m, b, out);
            if R::FULL && new {
                horn_rules::spread(st, Concl::PredIn(m, b), true, false, out);
                horn_rules::counted_full(ix, me, st, Some(m), out);
            }
            new
        }
        Concl::Succ(r, n) if R::HORN => {
            let new = horn_rules::succ(ix, me, st, r, n, false, out);
            if R::FULL && new {
                horn_rules::succ_full(ix, me, st, r, n, out);
            }
            new
        }
        Concl::RealBack(m) if R::HORN && R::FULL => horn_rules::real_back(ix, me, st, m, out),
        Concl::Singular if R::HORN && R::FULL => {
            let new = !st.extra.as_ref().is_some_and(|x| x.horn.counted);
            horn_rules::singular(ix, me, st, true, out);
            new
        }
        Concl::Same(k) if R::HORN && R::FULL => horn_rules::same(ix, me, st, k, out),
        Concl::SuccIn(..)
        | Concl::PredIn(..)
        | Concl::Succ(..)
        | Concl::RealBack(_)
        | Concl::Singular
        | Concl::Same(_) => {
            unreachable!("a conclusion of the Horn rules in a saturation without them")
        }
        Concl::Hyp(a) if R::DISJ => dl_rules::hyp(st, a, out),
        Concl::DEdge(u, e) if R::DISJ => dl_rules::dedge(ix, me, st, u, e, out),
        Concl::PredC(v, c) if R::DISJ => dl_rules::pred_c(ix, me, st, v, c, out),
        Concl::RPred(c) if R::DISJ => dl_root::rpred_c(ix, st, c, out),
        Concl::HypL(l) if R::DISJ => dl_root::hyp_l(st, l, out),
        Concl::REdge(v, u) if R::DISJ => dl_root::redge(st, v, u, out),
        Concl::RFact(c) if R::DISJ => dl_root::rfact(ix, st, c, out),
        Concl::RMark(h, l) if R::DISJ => dl_root::rmark(ix, st, h, l, out),
        Concl::RRet(c) if R::DISJ => dl_root::rret(ix, st, c, out),
        Concl::RReach(e) if R::DISJ => dl_root::rreach(ix, me, st, e, out),
        Concl::Hyp(_)
        | Concl::DEdge(..)
        | Concl::PredC(..)
        | Concl::RPred(..)
        | Concl::HypL(..)
        | Concl::REdge(..)
        | Concl::RFact(..)
        | Concl::RMark(..)
        | Concl::RRet(..)
        | Concl::RReach(..) => {
            unreachable!("a conclusion of Tier D in a saturation without it")
        }
    }
}

/// A context registers as one that stands for its individual.
#[cold]
#[inline(never)]
fn apply_nom<S: Sink>(ix: &Index, me: Ex, st: &mut State, k: Ex, out: &mut S) -> bool {
    if k == me || !st.extra().members.insert(k) {
        return false;
    }
    if st.bottom {
        out.send(k, Concl::Sub(BOTTOM));
    } else {
        for &s in &st.subs {
            out.send(k, Concl::Sub(s));
        }
        // a run with the Horn rules gives the member the loops and Horn successors
        // too (roadmap item 692)
        if ix.horn.is_some() {
            horn_rules::send_own(st, k, out);
        }
    }
    true
}

/// A context becomes real: its successors do too, and it gives its individuals what it
/// knows.
#[cold]
#[inline(never)]
fn apply_real<S: Sink>(ix: &Index, me: Ex, st: &mut State, out: &mut S) -> bool {
    if st.real {
        return false;
    }
    st.real = true;
    if st.bottom {
        if let Some(x) = &st.extra {
            for &n in &x.noms {
                out.send(n, Concl::Sub(BOTTOM));
            }
        }
        return true;
    }
    for &s in &st.subs {
        if let Node::Exist(..) = ix.head(s).node {
            out.send(ix.head(s).succ, Concl::Real);
        }
    }
    if let Some(x) = &st.extra {
        // the successors the Horn rules made (roadmap item 692)
        for &(_, n) in &x.horn.succ.list {
            out.send(n, Concl::Real);
        }
        for &n in &x.noms {
            for &s in &st.subs {
                out.send(n, Concl::Sub(s));
            }
        }
    }
    if ix.horn.is_some() {
        horn_rules::became_real(me, st, out);
    }
    true
}

/// An instance of the context has an individual as a successor, which closes a loop when
/// the context stands for the individual.
#[cold]
#[inline(never)]
fn apply_link_to<S: Sink>(me: Ex, st: &mut State, r: Role, n: Ex, out: &mut S) -> bool {
    let x = st.extra();
    if x.links.contains(&(r, n)) {
        return false;
    }
    x.links.push((r, n));
    if n == me || st.subs.contains(&n) {
        out.me(Concl::Loop(r));
    }
    true
}

/// A new subsumer of a context with individuals: it goes to the contexts that stand for
/// the context's individual, and from a real context to its individuals.
#[cold]
#[inline(never)]
fn give<S: Sink>(st: &State, d: Ex, out: &mut S) {
    let Some(x) = &st.extra else {
        return;
    };
    for &m in &x.members.list {
        out.send(m, Concl::Sub(d));
    }
    if st.real {
        for &n in &x.noms {
            out.send(n, Concl::Sub(d));
        }
    }
}

/// A backward link from `e` by `r` reaches the individuals the context stands for.
#[cold]
#[inline(never)]
fn link_to<S: Sink>(ix: &Index, me: Ex, st: &State, e: Ex, r: Role, out: &mut S) {
    individuals(ix, me, st, |n| out.send(e, Concl::LinkTo(r, n)));
}

/// The link of a chain: an instance of `e` has an instance of `f` as a `t`-successor,
/// which `e` keeps as a forward link when `fwd` says so ([`super::index::Roles::comp_fwd`]).
#[inline]
fn compose<S: Sink>(roles: &super::index::Roles, e: Ex, t: Role, f: Ex, fwd: bool, out: &mut S) {
    out.send(f, Concl::Back(e, t));
    if fwd {
        out.send(e, Concl::Fwd(t, f));
    }
    if roles.is_empty_role(t) {
        out.send(e, Concl::Sub(BOTTOM));
    }
}

/// A context derives the individual `n`: it registers with `n` to take its subsumers,
/// gives `n` its own when it is real, and closes the loops of links to `n`.
#[cold]
#[inline(never)]
fn nominal<S: Sink>(ix: &Index, me: Ex, st: &mut State, n: Ex, out: &mut S) {
    out.send(n, Concl::Nom(me));
    out.merged();
    let real = st.real;
    let x = st.extra.get_or_insert_with(Box::default);
    x.noms.push(n);
    if real {
        for &s in &st.subs {
            out.send(n, Concl::Sub(s));
        }
    }
    if ix.nominal_selfs {
        let x = st
            .extra
            .as_ref()
            .map(|x| x.links.clone())
            .unwrap_or_default();
        for (r, m) in x {
            if m == n {
                out.me(Concl::Loop(r));
            }
        }
        for (r, srcs) in &st.back {
            for &e in &srcs.list {
                out.send(e, Concl::LinkTo(*r, n));
            }
        }
    }
}

/// A context derives a data restriction by a role with functional superroles: its data
/// successors by the roles under each of them are one value, in the intersection of
/// their ranges.
#[cold]
#[inline(never)]
fn functional<S: Sink>(ix: &Index, st: &mut State, r: Role, w: Ex, out: &mut S) {
    let before = st.bytes();
    let fs = ix.roles.functional(r);
    // Covers the output group/vector and old/new capacity overlap of every merge.
    // Numeric limbs are shared; this reservation owns only containers and handles.
    let reserve = before
        .checked_add(std::mem::size_of::<Extra>())
        .and_then(|n| n.checked_add(std::mem::size_of::<Group>() * 4))
        .and_then(|n| fs.len().checked_mul(8).and_then(|m| n.checked_add(m)))
        .and_then(|n| {
            ix.vals[w as usize]
                .opaque
                .len()
                .checked_mul(8)
                .and_then(|m| n.checked_add(m))
        })
        .and_then(|n| n.checked_mul(8));
    let Some(reserve) = reserve else {
        out.data_overflow();
        return;
    };
    if !out.reserve_data(reserve) {
        return;
    }
    let x = st.extra.get_or_insert_with(Box::default);
    let mut merged = Group {
        funs: fs.to_vec(),
        roles: vec![r],
        val: ix.vals[w as usize].clone(),
    };
    let mut i = 0;
    while i < x.groups.len() {
        if x.groups[i].funs.iter().any(|f| merged.funs.contains(f)) {
            let g = x.groups.swap_remove(i);
            merged.funs.extend(g.funs);
            merged.roles.extend(g.roles);
            merged.val = merged.val.meet(&g.val);
            // the merged group may now meet groups it did not before
            i = 0;
        } else {
            i += 1;
        }
    }
    merged.funs.sort_unstable();
    merged.funs.dedup();
    merged.roles.sort_unstable();
    merged.roles.dedup();
    if merged.val.is_empty() {
        out.me(Concl::Sub(BOTTOM));
    } else {
        for &d in &merged.roles {
            ix.data_matches(d, &merged.val, |x| out.me(Concl::Sub(x)));
        }
    }
    x.groups.push(merged);
    x.groups.sort_by(|a, b| a.funs.cmp(&b.funs));
    let after = st.bytes();
    debug_assert!(after.saturating_sub(before) <= reserve);
    out.release_data(reserve.saturating_add(before).saturating_sub(after));
}

#[cfg(test)]
mod retained_storage_tests {
    use super::*;

    #[test]
    fn retained_dynamic_bytes_include_states_without_initializing_contexts() {
        let dynamic = Dynamic::new(8);
        assert_eq!(dynamic.retained_bytes(), 0);
        let initialized = dynamic.intern(&[2, 3]);
        let untouched = dynamic.intern(&[3, 4]);
        let table_bytes = dynamic.bytes();
        assert_eq!(dynamic.retained_bytes(), table_bytes);
        assert!(dynamic.slot(initialized).ctx.get().is_none());
        assert!(dynamic.slot(untouched).ctx.get().is_none());
        let context = dynamic.ctx(initialized);
        let state_bytes = {
            let mut state = context.st.lock();
            state.init = true;
            state.subs.insert(2);
            state.bytes()
        };
        assert!(state_bytes > 0);
        assert_eq!(dynamic.retained_bytes(), table_bytes + state_bytes);
        assert!(dynamic.slot(untouched).ctx.get().is_none());
    }
}

#[cfg(test)]
mod horn_accounting_tests {
    use super::*;
    use crate::classify::horn::HornAx;
    use crate::classify::index::{Axioms, Interner, RoleName};
    use crate::model::{Class, ObjectProperty};

    struct Fixture {
        ix: Index,
        size: usize,
        root: Ex,
        targets: Vec<Ex>,
        r: Role,
        s: Role,
        t: Role,
        exist: Ex,
    }

    fn fixture() -> Fixture {
        let mut it = Interner::new();
        let mut class = |name: &str| it.class(&Class::new(format!("urn:horn-accounting:{name}")));
        let root = class("root");
        let targets: Vec<Ex> = (0..SMALL + 3)
            .map(|n| class(&format!("target{n}")))
            .collect();
        let mut role = |name: &str| {
            it.role(RoleName::Object(ObjectProperty::new(format!(
                "urn:horn-accounting:{name}"
            ))))
        };
        let r = role("r");
        let s = role("s");
        let t = role("t");
        let exist = it.node(Node::Exist(r, targets[0]));
        let ax = Axioms {
            subs: vec![(root, exist)],
            // r is a right operand, so both the EL and Horn paths must retain Fwd.
            chains: vec![(vec![s, r], t)],
            horn: Some(Box::new(HornAx::default())),
            ..Axioms::default()
        };
        let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();
        assert!(ix.roles.keeps_forward(r));
        assert!(!ix.roles.keeps_forward(s));
        assert_eq!(ix.head(exist).succ, targets[0]);
        Fixture {
            ix,
            size: it.len(),
            root,
            targets,
            r,
            s,
            t,
            exist,
        }
    }

    #[derive(Default)]
    struct Recording {
        sent: Vec<(Ex, Concl)>,
        local: Vec<Concl>,
    }

    impl Sink for Recording {
        fn send(&mut self, to: Ex, c: Concl) {
            self.sent.push((to, c));
        }
        fn me(&mut self, c: Concl) {
            self.local.push(c);
        }
    }

    fn check_successor_set(hashed: bool) {
        let f = fixture();
        let mut st = State::default();
        let targets = if hashed {
            &f.targets[..]
        } else {
            &f.targets[..2]
        };
        for (i, &n) in targets.iter().enumerate() {
            let mut out = Recording::default();
            assert!(apply_rules::<_, Horn>(
                &f.ix,
                f.root,
                &mut st,
                Concl::Succ(f.r, n),
                &mut out
            ));
            assert_eq!(out.sent, vec![(n, Concl::Back(f.root, f.r))]);
            assert_eq!(out.local, vec![Concl::Fwd(f.r, n)]);
            let h = &st.extra.as_ref().unwrap().horn;
            assert_eq!(h.succ.list.len(), i + 1);
            assert_eq!(h.succ.set.is_some(), i >= SMALL);
        }
        for &n in targets {
            let mut duplicate = Recording::default();
            assert!(
                !apply_rules::<_, Horn>(
                    &f.ix,
                    f.root,
                    &mut st,
                    Concl::Succ(f.r, n),
                    &mut duplicate
                ),
                "target {n}, hashed={hashed}"
            );
            assert!(duplicate.sent.is_empty());
            assert!(duplicate.local.is_empty());
            assert_eq!(
                st.extra.as_ref().unwrap().horn.succ.list.len(),
                targets.len()
            );
        }
        let mut out = Recording::default();
        assert!(apply_rules::<_, Horn>(
            &f.ix,
            f.root,
            &mut st,
            Concl::Succ(f.s, f.targets[0]),
            &mut out
        ));
        assert_eq!(out.sent, vec![(f.targets[0], Concl::Back(f.root, f.s))]);
        assert!(out.local.is_empty());
        assert_eq!(
            st.extra.as_ref().unwrap().horn.succ.list.len(),
            targets.len() + 1
        );
    }

    #[test]
    fn duplicate_horn_successors_are_not_new_in_list_sets() {
        check_successor_set(false);
    }

    #[test]
    fn duplicate_horn_successors_are_not_new_in_hash_sets() {
        check_successor_set(true);
    }

    #[test]
    fn exist_and_direct_successors_keep_their_own_insertion_and_horn_bookkeeping() {
        let f = fixture();
        for exist_first in [true, false] {
            let mut st = State::default();
            let h = &mut st.extra().horn;
            h.fills.push(f.targets[1]);
            h.alls.push((f.r, f.targets[2]));
            h.all_roles.push(f.r);
            h.atmost.push((f.r, TOP));
            let direct = Concl::Succ(f.r, f.targets[0]);
            let existential = Concl::Sub(f.exist);
            let mut first = Recording::default();
            assert!(apply_rules::<_, Horn>(
                &f.ix,
                f.root,
                &mut st,
                if exist_first { existential } else { direct },
                &mut first
            ));
            assert_eq!(
                first.sent,
                vec![
                    (f.targets[0], Concl::Back(f.root, f.r)),
                    (f.targets[0], Concl::PredIn(f.root, f.targets[1])),
                ]
            );
            assert_eq!(first.local, vec![Concl::Fwd(f.r, f.targets[0])]);
            let h = &st.extra.as_ref().unwrap().horn;
            assert_eq!(h.succ.list, vec![(f.r, f.targets[0])]);
            assert_eq!(h.pending, vec![(f.r, f.targets[0])]);
            assert_eq!(h.merges, vec![(f.r, TOP)]);
            let mut second = Recording::default();
            let new = apply_rules::<_, Horn>(
                &f.ix,
                f.root,
                &mut st,
                if exist_first { direct } else { existential },
                &mut second,
            );
            assert_eq!(new, !exist_first);
            if exist_first {
                assert!(second.sent.is_empty());
                assert!(second.local.is_empty());
            } else {
                // A new Sub still emits its EL links, but sent=true must not repeat
                // the already registered Horn predecessor, pending or merge work.
                assert_eq!(second.sent, vec![(f.targets[0], Concl::Back(f.root, f.r))]);
                assert_eq!(second.local, vec![Concl::Fwd(f.r, f.targets[0])]);
            }
            let h = &st.extra.as_ref().unwrap().horn;
            assert_eq!(h.pending.len(), 1);
            assert_eq!(h.merges.len(), 1);
            let mut again = Recording::default();
            assert!(!apply_rules::<_, Horn>(
                &f.ix,
                f.root,
                &mut st,
                existential,
                &mut again
            ));
            assert!(again.sent.is_empty());
            assert!(again.local.is_empty());
        }
    }

    fn run<'a>(ix: &'a Index, ctx: &'a Contexts, bounds: &'a Bounds<'a>) -> Run<'a> {
        Run {
            ix,
            ctx,
            universal: &[],
            roots: &[],
            next_root: AtomicUsize::new(0),
            queue: Mutex::default(),
            ready: Condvar::new(),
            pending: AtomicUsize::new(1),
            idle: AtomicUsize::new(0),
            stop: AtomicU8::new(STOP_NONE),
            memory: AtomicUsize::new(bounds.base_memory),
            queued: AtomicIsize::new(1),
            peak: AtomicUsize::new(bounds.peak_memory),
            applied: AtomicUsize::new(0),
            merged: AtomicBool::new(false),
            batch: AtomicBool::new(false),
            plain: false,
            horn: true,
            disj: false,
            steps: false,
            dynamic: None,
            bounds,
            started: &STARTED_NS,
            always_congested: false,
        }
    }

    /// A congested delivery drops every kind of conclusion the context holds, the forward
    /// links and loops of chains included, and keeps the ones it does not (roadmap item
    /// 818).
    #[test]
    fn a_congested_delivery_drops_the_forward_links_and_loops_a_context_holds() {
        let f = fixture();
        let ctx: Contexts = (0..f.size).map(|_| OnceLock::new()).collect();
        let root = ctx[f.root as usize].get_or_init(Box::default);
        {
            let mut st = root.st.lock();
            st.init = true;
            st.fwd.insert((f.r, f.targets[0]));
            st.loops.push(f.r);
            st.back_by(f.s).insert(f.targets[1]);
        }
        let bounds = Bounds {
            threads: 1,
            deadline: None,
            max_memory: None,
            cancelled: None,
            charge: None,
            base_memory: 0,
            peak_memory: 0,
            dynamic: None,
            dl: None,
            partial: false,
            steps: false,
        };
        let run = run(&f.ix, &ctx, &bounds);
        let msgs = [
            Concl::Fwd(f.r, f.targets[0]),
            Concl::Loop(f.r),
            Concl::Back(f.targets[1], f.s),
            Concl::Fwd(f.r, f.targets[1]),
            Concl::Loop(f.s),
            Concl::Back(f.targets[0], f.s),
        ];
        // two runs to the one destination, as an outbox groups them
        let runs = [
            Dest {
                to: f.root,
                start: 0,
                end: 4,
            },
            Dest {
                to: f.root,
                start: 4,
                end: 6,
            },
        ];
        run.queued.store(msgs.len() as isize, Ordering::Relaxed);
        assert!(run.deliver_new(root, &runs, &msgs));
        assert_eq!(
            *root.todo.lock(),
            vec![
                Concl::Fwd(f.r, f.targets[1]),
                Concl::Loop(f.s),
                Concl::Back(f.targets[0], f.s)
            ]
        );
        assert_eq!(run.queued.load(Ordering::Relaxed), 3);
    }

    /// Data that the rules keep waits in the activation until it passes a batch or the
    /// activation ends, a release nets against what waits first, and the run's count ends
    /// at what was kept and not released.
    #[test]
    fn kept_data_waits_for_a_batch_and_a_release_nets_against_it() {
        let f = fixture();
        let ctx: Contexts = (0..f.size).map(|_| OnceLock::new()).collect();
        let bounds = Bounds {
            threads: 2,
            deadline: None,
            max_memory: None,
            cancelled: None,
            charge: None,
            base_memory: 0,
            peak_memory: 0,
            dynamic: None,
            dl: None,
            partial: false,
            steps: false,
        };
        let run = run(&f.ix, &ctx, &bounds);
        let mut marks = Marks::default();
        let (mut local, mut woke, mut outbox) = (Vec::new(), VecDeque::new(), Outbox::default());
        let held = |run: &Run| run.memory.load(Ordering::Relaxed);
        {
            let mut out = Out {
                run: &run,
                me: f.root,
                local: &mut local,
                woke: &mut woke,
                outbox: &mut outbox,
                marks: &mut marks,
                pending: false,
                sent: 0,
                work: 0,
                kept: 0,
            };
            out.keep_data(100);
            assert_eq!(held(out.run), 0);
            out.release_data(60);
            assert_eq!(held(out.run), 0);
            out.keep_data(KEEP_BATCH);
            assert_eq!(held(out.run), KEEP_BATCH + 40);
            out.keep_data(8);
            out.release_data(KEEP_BATCH);
            assert_eq!(held(out.run), 48);
            out.keep_data(24);
        }
        assert_eq!(held(&run), 72);
    }

    /// A flush of a context's link products pairs what is new since the marks with
    /// everything and what is old only with what is new, each pair once, and sends the
    /// messages in one run per source or target (roadmap item 818).
    #[test]
    fn a_flush_of_the_link_products_pairs_new_with_all_and_old_with_new_in_runs() {
        let f = fixture();
        let ctx: Contexts = (0..f.size).map(|_| OnceLock::new()).collect();
        let [e_old, e_new, f_old, f_new, x_old, x_new] = [1, 2, 3, 4, 5, 6].map(|i| f.targets[i]);
        let mut st = State {
            init: true,
            ..Default::default()
        };
        st.back_by(f.s).insert(e_old);
        st.fwd.insert((f.r, f_old));
        st.props.push((f.s, x_old));
        // the marks stand where an activation set them, before the new entries
        let mut marks = Marks::default();
        marks.set(&st);
        st.back_by(f.s).insert(e_new);
        st.fwd.insert((f.r, f_new));
        st.props.push((f.s, x_new));
        let bounds = Bounds {
            threads: 2,
            deadline: None,
            max_memory: None,
            cancelled: None,
            charge: None,
            base_memory: 0,
            peak_memory: 0,
            dynamic: None,
            dl: None,
            partial: false,
            steps: false,
        };
        let run = run(&f.ix, &ctx, &bounds);
        run.batch.store(true, Ordering::Relaxed);
        let (mut local, mut woke, mut outbox) = (Vec::new(), VecDeque::new(), Outbox::default());
        let mut out = Out {
            run: &run,
            me: f.root,
            local: &mut local,
            woke: &mut woke,
            outbox: &mut outbox,
            marks: &mut marks,
            pending: true,
            sent: 0,
            work: 0,
            kept: 0,
        };
        let mut tally = Tally::default();
        assert!(run.flush_links(&st, &mut out, &mut tally));
        assert_eq!(out.sent, 6);
        drop(out);
        let sent: Vec<(Ex, Vec<Concl>)> = outbox
            .runs
            .iter()
            .map(|d| (d.to, outbox.msgs[d.start as usize..d.end as usize].to_vec()))
            .collect();
        assert_eq!(
            sent,
            vec![
                // a new source takes every propagation, an old one the new propagation; the
                // chain s∘r ⊑ t keeps no forward link by t
                (e_new, vec![Concl::Sub(x_old), Concl::Sub(x_new)]),
                (e_old, vec![Concl::Sub(x_new)]),
                // a new target takes a backward link from every source, an old one from
                // the new source
                (
                    f_new,
                    vec![Concl::Back(e_old, f.t), Concl::Back(e_new, f.t)]
                ),
                (f_old, vec![Concl::Back(e_new, f.t)]),
            ]
        );
        assert_eq!(
            (marks.fwd, marks.props, marks.back.as_slice()),
            (2, 2, &[2][..])
        );
        // a second flush with nothing new sends nothing
        let mut out = Out {
            run: &run,
            me: f.root,
            local: &mut local,
            woke: &mut woke,
            outbox: &mut outbox,
            marks: &mut marks,
            pending: true,
            sent: 0,
            work: 0,
            kept: 0,
        };
        assert!(run.flush_links(&st, &mut out, &mut tally));
        assert_eq!(out.sent, 0);
    }

    /// A flush writes the products of one group of links into the outbox once, with one
    /// run header per destination, and a source that is the context itself takes them as
    /// its own conclusions. Each destination receives what it did when every message was
    /// written for it, in the same order. Without batching each destination's to-do
    /// list takes the products directly (roadmap item 889).
    #[test]
    fn a_flush_writes_each_run_once_for_all_its_destinations() {
        let f = fixture();
        let [e1, e2, f1, f2, x1, x2] = [1, 2, 3, 4, 5, 6].map(|i| f.targets[i]);
        let mut st = State {
            init: true,
            ..Default::default()
        };
        for e in [e1, f.root, e2] {
            st.back_by(f.s).insert(e);
        }
        st.fwd.insert((f.r, f1));
        st.fwd.insert((f.r, f2));
        st.props.push((f.s, x1));
        st.props.push((f.s, x2));
        let bounds = bounds(false);
        let products = [Concl::Sub(x1), Concl::Sub(x2)];
        let backs = [
            Concl::Back(e1, f.t),
            Concl::Back(f.root, f.t),
            Concl::Back(e2, f.t),
        ];
        for (batch, steps) in [(true, false), (false, false), (true, true)] {
            let ctx: Contexts = (0..f.size).map(|_| OnceLock::new()).collect();
            let mut run = run(&f.ix, &ctx, &bounds);
            run.steps = steps;
            run.batch.store(batch, Ordering::Relaxed);
            let (mut local, mut woke, mut outbox, mut marks) = (
                Vec::new(),
                VecDeque::new(),
                Outbox::default(),
                Marks::default(),
            );
            let mut out = Out {
                run: &run,
                me: f.root,
                local: &mut local,
                woke: &mut woke,
                outbox: &mut outbox,
                marks: &mut marks,
                pending: true,
                sent: 0,
                work: 0,
                kept: 0,
            };
            assert!(run.flush_links(&st, &mut out, &mut Tally::default()));
            // the messages to other contexts, the context's own aside
            assert_eq!(out.sent, 2 * 2 + 2 * 3);
            drop(out);
            assert_eq!(local, products);
            let want = vec![
                (e1, products.to_vec()),
                (e2, products.to_vec()),
                (f1, backs.to_vec()),
                (f2, backs.to_vec()),
            ];
            if steps {
                // a run in steps keeps a message for each destination, in the order the
                // destinations took them one at a time
                let each: Vec<(Ex, Concl)> = want
                    .iter()
                    .flat_map(|(to, msgs)| msgs.iter().map(move |&c| (*to, c)))
                    .collect();
                assert_eq!(outbox.steps, each);
                assert!(outbox.msgs.is_empty());
            } else if batch {
                // one copy of the products and one of the backward links
                assert_eq!(outbox.msgs.len(), products.len() + backs.len());
                let mut by: Vec<(Ex, Vec<Concl>)> = Vec::new();
                for (to, c) in sent(&outbox) {
                    match by.iter_mut().find(|(t, _)| *t == to) {
                        Some((_, v)) => v.push(c),
                        None => by.push((to, vec![c])),
                    }
                }
                assert_eq!(by, want);
            } else {
                assert!(outbox.is_empty());
                for (to, msgs) in want {
                    assert_eq!(*ctx[to as usize].get().unwrap().todo.lock(), msgs);
                    assert!(woke.contains(&to));
                }
            }
        }
    }

    /// The check of a run against a context's tables keeps exactly the messages that
    /// [`State::holds`] does not answer, whatever the kinds and roles of the run and
    /// whether the context's sets are lists or hash sets (roadmap item 889).
    #[test]
    fn the_check_of_a_run_keeps_what_holds_does_not_answer() {
        let f = fixture();
        let roles = [f.r, f.s, f.t];
        let ids: Vec<Ex> = (0..40).collect();
        let mut st = State {
            init: true,
            ..Default::default()
        };
        // a short list of sources by r, a hash set by s, none by t
        for &e in &ids[..5] {
            st.back_by(f.r).insert(e);
        }
        for &e in ids.iter().step_by(2) {
            st.back_by(f.s).insert(e);
        }
        // a hash set of forward links by r
        for &e in ids.iter().skip(1).step_by(2) {
            st.fwd.insert((f.r, e));
        }
        assert!(st.fwd.set.is_some() && st.back[1].1.set.is_some());
        for &e in &ids[..7] {
            st.subs.insert(e);
        }
        st.loops.push(f.s);
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let mut run = Vec::new();
        for _ in 0..2000 {
            let e = ids[next(ids.len())];
            let r = roles[next(roles.len())];
            // runs of backward links by one role, as the flush sends them, mixed with
            // the other kinds
            let c = match next(8) {
                0..=3 => Concl::Back(e, if next(4) == 0 { r } else { f.s }),
                4 | 5 => Concl::Fwd(r, e),
                6 => Concl::Sub(e),
                _ => Concl::Loop(r),
            };
            run.push(c);
        }
        let want: Vec<Concl> = run.iter().copied().filter(|c| !st.holds(c)).collect();
        assert!(!want.is_empty() && want.len() < run.len());
        let mut got = Vec::new();
        st.add_new(&run, &mut got);
        assert_eq!(got, want);
    }

    /// The messages an outbox holds, by destination, in the order sent.
    fn sent(outbox: &Outbox) -> Vec<(Ex, Concl)> {
        outbox
            .runs
            .iter()
            .flat_map(|d| {
                outbox.msgs[d.start as usize..d.end as usize]
                    .iter()
                    .map(move |&c| (d.to, c))
            })
            .collect()
    }

    fn bounds(partial: bool) -> Bounds<'static> {
        Bounds {
            threads: 2,
            deadline: None,
            max_memory: None,
            cancelled: None,
            charge: None,
            base_memory: 0,
            peak_memory: 0,
            dynamic: None,
            dl: None,
            partial,
            steps: false,
        }
    }

    /// An activation composes the links its rules store through the deferred flush: the
    /// first sends the backward link of the chain s∘r ⊑ t for the pair it finds, a later
    /// one only for its new forward link, and one with a new source pairs it with the old
    /// forward links (roadmap item 818).
    #[test]
    fn activations_compose_new_links_with_old_ones_once_through_the_flush() {
        let f = fixture();
        let ctx: Contexts = (0..f.size).map(|_| OnceLock::new()).collect();
        let root = ctx[f.root as usize].get_or_init(Box::default);
        let [e1, e2, f1] = [1, 2, 3].map(|i| f.targets[i]);
        let bounds = bounds(false);
        let run = run(&f.ix, &ctx, &bounds);
        run.batch.store(true, Ordering::Relaxed);
        let (mut mine, mut tally, mut local, mut marks) = (
            VecDeque::new(),
            Tally::default(),
            Vec::new(),
            Marks::default(),
        );
        let mut outbox = Outbox::default();
        let mut activate = |todo: Vec<Concl>, outbox: &mut Outbox| {
            root.todo.lock().extend(todo);
            run.process::<Horn>(
                f.root, &mut mine, &mut tally, &mut local, outbox, &mut marks,
            );
            let mut s: Vec<(Ex, Concl)> = sent(outbox)
                .into_iter()
                .filter(|(_, c)| matches!(c, Concl::Back(..) | Concl::Fwd(..) | Concl::Sub(_)))
                .collect();
            s.sort();
            outbox.clear();
            s
        };
        // the root's told existential gives Fwd(r, target0) and the to-do list Back(e1, s)
        let mut want = vec![
            (f.targets[0], Concl::Back(e1, f.t)),
            (f.targets[0], Concl::Back(f.root, f.r)),
        ];
        want.sort();
        assert_eq!(activate(vec![Concl::Back(e1, f.s)], &mut outbox), want);
        assert_eq!(
            activate(vec![Concl::Fwd(f.r, f1)], &mut outbox),
            vec![(f1, Concl::Back(e1, f.t))]
        );
        let mut want = vec![
            (f.targets[0], Concl::Back(e2, f.t)),
            (f1, Concl::Back(e2, f.t)),
        ];
        want.sort();
        assert_eq!(activate(vec![Concl::Back(e2, f.s)], &mut outbox), want);
    }

    /// A run that answers in part and stops inside an activation still sends the products
    /// of the links the activation stored, and gives the conclusions it did not apply back
    /// to the to-do list (roadmap items 603 and 818).
    #[test]
    fn a_stopped_partial_activation_flushes_its_link_products() {
        let f = fixture();
        let ctx: Contexts = (0..f.size).map(|_| OnceLock::new()).collect();
        let root = ctx[f.root as usize].get_or_init(Box::default);
        let e1 = f.targets[1];
        let bounds = bounds(true);
        let run = run(&f.ix, &ctx, &bounds);
        run.batch.store(true, Ordering::Relaxed);
        run.stop.store(STOP_CANCEL, Ordering::SeqCst);
        {
            let mut t = root.todo.lock();
            t.push(Concl::Back(e1, f.s));
            // enough repeats to reach a check of the bounds, which finds the run stopped
            t.extend(std::iter::repeat_n(Concl::Sub(TOP), CHECK_EVERY + 4));
        }
        let mut outbox = Outbox::default();
        run.process::<Horn>(
            f.root,
            &mut VecDeque::new(),
            &mut Tally::default(),
            &mut Vec::new(),
            &mut outbox,
            &mut Marks::default(),
        );
        assert!(sent(&outbox).contains(&(f.targets[0], Concl::Back(e1, f.t))));
        assert!(!root.todo.lock().is_empty());
    }

    /// A run that answers in part and stops inside an activation gives back the rest
    /// of the batch it took, last first, below the conclusions the rules gave the
    /// context, as it did when the batch went onto the context's own list reversed
    /// (roadmap item 889).
    #[test]
    fn a_stopped_partial_activation_gives_back_the_rest_of_its_batch() {
        let f = fixture();
        let ctx: Contexts = (0..f.size).map(|_| OnceLock::new()).collect();
        let root = ctx[f.root as usize].get_or_init(Box::default);
        let [e1, x1, x2] = [1, 2, 3].map(|i| f.targets[i]);
        let bounds = bounds(true);
        let run = run(&f.ix, &ctx, &bounds);
        run.batch.store(true, Ordering::Relaxed);
        run.stop.store(STOP_CANCEL, Ordering::SeqCst);
        {
            let mut t = root.todo.lock();
            t.push(Concl::Back(e1, f.s));
            t.extend(std::iter::repeat_n(Concl::Sub(TOP), CHECK_EVERY + 4));
            t.push(Concl::Sub(x1));
            t.push(Concl::Sub(x2));
        }
        run.process::<Horn>(
            f.root,
            &mut VecDeque::new(),
            &mut Tally::default(),
            &mut Vec::new(),
            &mut Outbox::default(),
            &mut Marks::default(),
        );
        // the check after CHECK_EVERY conclusions found the run stopped, with the last
        // repeats and the two subsumers not applied
        let mut want = vec![Concl::Sub(x2), Concl::Sub(x1)];
        want.extend(std::iter::repeat_n(Concl::Sub(TOP), 9));
        assert_eq!(*root.todo.lock(), want);
    }

    #[test]
    fn a_duplicate_successor_does_not_cross_the_storage_budget_after_flush() {
        let f = fixture();
        let ctx: Contexts = (0..f.size).map(|_| OnceLock::new()).collect();
        let root = ctx[f.root as usize].get_or_init(Box::default);
        root.st.lock().init = true;
        root.st.lock().extra().horn.succ.insert((f.r, f.targets[0]));
        root.todo.lock().push(Concl::Succ(f.r, f.targets[0]));
        let base = 4096;
        let cap = base + std::mem::size_of::<Concl>();
        let bounds = Bounds {
            threads: 1,
            deadline: None,
            max_memory: Some(cap),
            cancelled: None,
            charge: None,
            base_memory: base,
            peak_memory: cap,
            dynamic: None,
            dl: None,
            partial: false,
            steps: false,
        };
        let run = run(&f.ix, &ctx, &bounds);
        let mut tally = Tally::default();
        run.process::<Horn>(
            f.root,
            &mut VecDeque::new(),
            &mut tally,
            &mut Vec::new(),
            &mut Outbox::default(),
            &mut Marks::default(),
        );
        let ok = run.flush(&mut tally);
        eprintln!(
            "duplicate budget boundary: cap={cap} held={} queued={} peak={} attempted={} ok={ok}",
            run.memory.load(Ordering::Relaxed),
            run.queued.load(Ordering::Relaxed),
            run.peak.load(Ordering::Relaxed),
            run.applied.load(Ordering::Relaxed)
        );
        assert!(ok, "a duplicate alone must not stop the run as over budget");
        assert_eq!(run.memory.load(Ordering::Relaxed), base);
        assert_eq!(run.queued.load(Ordering::Relaxed), 0);
        assert_eq!(run.applied.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn real_horn_process_flush_counts_attempts_storage_queues_and_budget_refusals() {
        let f = fixture();
        let base = 4096;
        let queued_bytes = std::mem::size_of::<Concl>();
        for (label, c, held_pair, stored) in [
            ("duplicate", Concl::Succ(f.r, f.targets[0]), true, 0),
            ("first", Concl::Succ(f.r, f.targets[0]), false, 2),
            ("new-role", Concl::Succ(f.s, f.targets[0]), true, 1),
            ("new-target", Concl::Succ(f.r, f.targets[1]), true, 2),
        ] {
            for high_peak in [false, true] {
                for bounded in [false, true] {
                    let ctx: Contexts = (0..f.size).map(|_| OnceLock::new()).collect();
                    let root = ctx[f.root as usize].get_or_init(Box::default);
                    root.st.lock().init = true;
                    if held_pair {
                        assert!(root.st.lock().extra().horn.succ.insert((f.r, f.targets[0])));
                    }
                    root.todo.lock().push(c);
                    let initial_peak = if high_peak {
                        base + 8192
                    } else {
                        base + queued_bytes
                    };
                    let charged = AtomicUsize::new(0);
                    let charge = |bytes| {
                        charged.fetch_add(bytes, Ordering::Relaxed);
                        true
                    };
                    let bounds = Bounds {
                        threads: 1,
                        deadline: None,
                        max_memory: bounded.then_some(base + queued_bytes),
                        cancelled: None,
                        charge: Some(&charge),
                        base_memory: base,
                        peak_memory: initial_peak,
                        dynamic: None,
                        dl: None,
                        partial: false,
                        steps: false,
                    };
                    let run = run(&f.ix, &ctx, &bounds);
                    let mut tally = Tally::default();
                    run.process::<Horn>(
                        f.root,
                        &mut VecDeque::new(),
                        &mut tally,
                        &mut Vec::new(),
                        &mut Outbox::default(),
                        &mut Marks::default(),
                    );
                    let sent = usize::from(stored != 0);
                    // The source is initialized. The only extra local application is
                    // Fwd on r; the new remote Back remains in its target's todo.
                    assert_eq!(tally.applied, if stored == 2 { 2 } else { 1 }, "{label}");
                    assert_eq!(tally.stored, stored, "{label}");
                    assert_eq!(tally.queued, sent as isize - 1, "{label}");
                    assert_eq!(run.memory.load(Ordering::Relaxed), base);
                    assert!(root.todo.lock().is_empty());
                    assert_eq!(
                        ctx.iter()
                            .filter_map(|s| s.get())
                            .map(|s| s.todo.lock().len())
                            .sum::<usize>(),
                        sent
                    );
                    let applied = tally.applied;
                    let ok = run.flush(&mut tally);
                    let held = base + stored * CONCLUSION_BYTES;
                    let used = held + sent * queued_bytes;
                    assert_eq!(run.applied.load(Ordering::Relaxed), applied);
                    assert_eq!(run.memory.load(Ordering::Relaxed), held);
                    assert_eq!(run.queued.load(Ordering::Relaxed), sent as isize);
                    assert_eq!(run.peak.load(Ordering::Relaxed), initial_peak.max(used));
                    assert_eq!(ok, !bounded || stored == 0, "{label}");
                    assert_eq!(
                        run.stop.load(Ordering::Relaxed),
                        if ok { STOP_NONE } else { STOP_MEMORY }
                    );
                    let expected_charge = if ok {
                        used.saturating_sub(initial_peak)
                    } else {
                        0
                    };
                    assert_eq!(charged.load(Ordering::Relaxed), expected_charge);
                    assert_eq!((tally.applied, tally.stored, tally.queued), (0, 0, 0));
                    eprintln!(
                        "horn-accounting {label}: high_peak={high_peak} bounded={bounded} attempted={applied} stored={stored} held={held} queued={sent} peak={} ok={ok}",
                        initial_peak.max(used)
                    );
                }
            }
        }
        // An actual pool refusal must still stop genuine new storage, while a
        // duplicate below its previous peak has no fresh grant to refuse.
        for duplicate in [true, false] {
            let ctx: Contexts = (0..f.size).map(|_| OnceLock::new()).collect();
            let root = ctx[f.root as usize].get_or_init(Box::default);
            root.st.lock().init = true;
            if duplicate {
                root.st.lock().extra().horn.succ.insert((f.r, f.targets[0]));
            }
            root.todo.lock().push(Concl::Succ(f.r, f.targets[0]));
            let calls = AtomicUsize::new(0);
            let charge = |_| {
                calls.fetch_add(1, Ordering::Relaxed);
                false
            };
            let bounds = Bounds {
                threads: 1,
                deadline: None,
                max_memory: None,
                cancelled: None,
                charge: Some(&charge),
                base_memory: base,
                peak_memory: base + queued_bytes,
                dynamic: None,
                dl: None,
                partial: false,
                steps: false,
            };
            let run = run(&f.ix, &ctx, &bounds);
            let mut tally = Tally::default();
            run.process::<Horn>(
                f.root,
                &mut VecDeque::new(),
                &mut tally,
                &mut Vec::new(),
                &mut Outbox::default(),
                &mut Marks::default(),
            );
            assert_eq!(run.flush(&mut tally), duplicate);
            assert_eq!(calls.load(Ordering::Relaxed), usize::from(!duplicate));
            assert_eq!(
                run.memory.load(Ordering::Relaxed),
                base + if duplicate { 0 } else { 2 * CONCLUSION_BYTES }
            );
            assert_eq!(
                run.stop.load(Ordering::Relaxed),
                if duplicate { STOP_NONE } else { STOP_MEMORY }
            );
        }
    }
}

#[cfg(test)]
mod numeric_resource_tests {
    use super::*;
    use crate::classify::numeric::tests::measure;
    use crate::classify::{Limits, Saturation};

    struct DataSink {
        admit: bool,
        held: usize,
    }
    impl Sink for DataSink {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn reserve_data(&mut self, bytes: usize) -> bool {
            if self.admit {
                self.held += bytes;
            }
            self.admit
        }
        fn release_data(&mut self, bytes: usize) {
            self.held -= bytes;
        }
    }
    #[test]
    fn functional_group_refusal_precedes_box_vector_and_handle_storage() {
        let doc = crate::ofn_read::parse(include_str!(
            "../../tests/fixtures/precision663/functional-equal.ofn"
        ))
        .unwrap();
        let refs: Vec<_> = doc.axioms.iter().map(|a| &a.kind).collect();
        let mut sat = Saturation::new();
        sat.classify(&refs, None, &Limits::default()).unwrap();
        let ix = sat.index.as_ref().unwrap();
        let (role, value) = sat
            .interner
            .nodes
            .iter()
            .find_map(|n| match n {
                Node::DataSome(r, w)
                    if !ix.roles.functional(*r).is_empty()
                        && ix.vals[*w as usize].single().is_some() =>
                {
                    Some((*r, *w))
                }
                _ => None,
            })
            .unwrap();
        let mut state = State::default();
        let mut sink = DataSink {
            admit: false,
            held: 0,
        };
        let (_, stats) = measure(|| functional(ix, &mut state, role, value, &mut sink));
        assert_eq!(stats.events, 0);
        assert!(state.extra.is_none());
        let retained = sat.interner.ranges.ledger.as_ref().unwrap().bytes();
        sink.admit = true;
        functional(ix, &mut state, role, value, &mut sink);
        assert_eq!(sink.held, state.bytes());
        assert_eq!(state.extra.as_ref().unwrap().groups.len(), 1);
        assert_eq!(
            sat.interner.ranges.ledger.as_ref().unwrap().bytes(),
            retained
        );
    }
}

#[cfg(test)]
mod horn_delivery_tests {
    //! The delivery orders behind the review of roadmap item 692 (F24, "The Horn rules
    //! with individuals"): a Horn successor made after its source became real, a member
    //! registering after its individual has loops and Horn successors, and the filler of
    //! a counted loop arriving last. A saturation's order of activation does not pin these
    //! down, so each test delivers the conclusions to one context in a chosen order and
    //! checks what the context sends, against the other orders of the same conclusions.
    use super::*;
    use crate::classify::horn;
    use crate::classify::index::{self, Axioms, Interner, RoleName};
    use crate::model::{AxiomKind, Class, ObjectProperty};

    const EX: &str = "http://ex.org/delivery#";

    struct Fixture {
        ix: Index,
        it: Interner,
    }

    impl Fixture {
        /// The index of an ontology, through the EL normalization and then the Horn
        /// normalization of what it left out, as the classifier builds it.
        fn new(body: &str) -> Fixture {
            let doc = crate::ofn_read::parse(&format!(
                "Prefix(:=<{EX}>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n{body}\n)"
            ))
            .unwrap();
            let mut it = Interner::new();
            let mut ax = Axioms::default();
            let mut outside: Vec<&AxiomKind> = Vec::new();
            for a in &doc.axioms {
                if index::normalize(&a.kind, &mut it, &mut ax).is_some() {
                    outside.push(&a.kind);
                }
            }
            let taken = horn::normalize(&outside, &mut it, &mut ax, None, &|| false).unwrap();
            assert!(taken.iter().all(|t| *t), "the Horn rules take every axiom");
            ax.classes.extend(it.met_classes());
            let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();
            assert!(ix.horn.is_some(), "the index has the Horn rules");
            Fixture { ix, it }
        }

        fn class(&self, name: &str) -> Ex {
            self.it
                .class_ex(&Class::new(format!("{EX}{name}")))
                .unwrap()
        }

        fn role(&self, name: &str) -> Role {
            self.it
                .role_id(&RoleName::Object(ObjectProperty::new(format!(
                    "{EX}{name}"
                ))))
                .unwrap()
        }

        /// The at-most restriction among the told subsumers of `e`, with its role and its
        /// filler as the index holds them.
        fn at_most_of(&self, e: Ex) -> (Ex, Role, Ex) {
            let h = self.ix.head(e);
            for &d in self.ix.told_of(h) {
                if let Node::AtMost1(s, b) = self.ix.head(d).node {
                    return (d, s, b);
                }
            }
            panic!("no at-most restriction among the told subsumers");
        }

        fn nominal(&self) -> Ex {
            let mut noms = self.it.nominals();
            let n = noms.next().expect("one individual");
            assert!(noms.next().is_none(), "one individual");
            n
        }
    }

    #[derive(Default)]
    struct Recording {
        sent: Vec<(Ex, Concl)>,
        local: Vec<Concl>,
    }

    impl Sink for Recording {
        fn send(&mut self, to: Ex, c: Concl) {
            self.sent.push((to, c));
        }
        fn me(&mut self, c: Concl) {
            self.local.push(c);
        }
        /// every context here stands for one named expression
        fn core(&self, _ix: &Index, e: Ex, out: &mut Vec<Ex>) {
            out.push(e);
        }
    }

    /// Delivers the conclusions to the context in order and returns what each one made the
    /// context send.
    fn deliver(f: &Fixture, me: Ex, seq: &[Concl]) -> Vec<Recording> {
        let mut st = State::default();
        seq.iter()
            .map(|&c| {
                let mut out = Recording::default();
                apply_rules::<_, HornFull>(&f.ix, me, &mut st, c, &mut out);
                out
            })
            .collect()
    }

    fn sent_to(outs: &[Recording], to: Ex, c: Concl) -> usize {
        outs.iter()
            .flat_map(|o| o.sent.iter())
            .filter(|&&(t, d)| t == to && d == c)
            .count()
    }

    fn local(outs: &[Recording], c: Concl) -> usize {
        outs.iter()
            .flat_map(|o| o.local.iter())
            .filter(|&&d| d == c)
            .count()
    }

    /// Finding 2 of the implementation review: a Horn successor that R3, R5 or R6 makes
    /// after its source became real learns that the source is real, as a successor made
    /// before does when the source becomes real.
    #[test]
    fn a_horn_successor_of_a_real_source_learns_the_source_is_real_in_either_order() {
        let f = Fixture::new(
            "SubClassOf(:M ObjectSomeValuesFrom(:r :N))
             SubClassOf(:N ObjectAllValuesFrom(:s :N))
             SubObjectPropertyOf(ObjectInverseOf(:r) :s)
             ClassAssertion(:M :a)",
        );
        let (m, n, r) = (f.class("M"), f.class("N"), f.role("r"));
        let real_first = [Concl::Real, Concl::Succ(r, n)];
        let succ_first = [Concl::Succ(r, n), Concl::Real];
        for (name, seq) in [
            ("real first", &real_first),
            ("successor first", &succ_first),
        ] {
            let outs = deliver(&f, m, seq);
            assert_eq!(sent_to(&outs, n, Concl::Back(m, r)), 1, "{name}: the link");
            assert_eq!(
                sent_to(&outs, n, Concl::Real),
                1,
                "{name}: the successor is real"
            );
            assert_eq!(
                sent_to(&outs, n, Concl::RealBack(m)),
                1,
                "{name}: the successor learns the source is real"
            );
            // the message comes with the step that completes the pair
            assert_eq!(
                sent_to(&outs[..1], n, Concl::RealBack(m)),
                0,
                "{name}: not before"
            );
        }
    }

    /// Finding 3 of the implementation review: a context that registers as a member of an
    /// individual after the individual's context has loops and Horn successors takes
    /// them, as a member registered before takes them as they come.
    #[test]
    fn a_member_that_registers_late_takes_the_loops_and_horn_successors() {
        let f = Fixture::new(
            "ClassAssertion(:C :a)
             SubClassOf(:C ObjectHasSelf(:l))
             SubClassOf(:C ObjectSomeValuesFrom(:r :N))
             SubObjectPropertyOf(ObjectInverseOf(:r) :s)",
        );
        let (a, k, n) = (f.nominal(), f.class("C"), f.class("N"));
        let (l, r) = (f.role("l"), f.role("r"));
        let member_last = [Concl::Loop(l), Concl::Succ(r, n), Concl::Nom(k)];
        let member_first = [Concl::Nom(k), Concl::Loop(l), Concl::Succ(r, n)];
        for (name, seq) in [
            ("member last", &member_last),
            ("member first", &member_first),
        ] {
            let outs = deliver(&f, a, seq);
            assert_eq!(sent_to(&outs, k, Concl::Loop(l)), 1, "{name}: the loop");
            assert_eq!(
                sent_to(&outs, k, Concl::Succ(r, n)),
                1,
                "{name}: the successor"
            );
        }
        // the late member takes both in the step it registers
        let outs = deliver(&f, a, &member_last);
        assert_eq!(sent_to(&outs[..2], k, Concl::Loop(l)), 0);
        assert_eq!(sent_to(&outs[..2], k, Concl::Succ(r, n)), 0);
    }

    /// Finding 4 of the implementation review: a loop that an at-most restriction of a
    /// singular context counts, beside a real predecessor the restriction counts, makes R7
    /// identify the predecessor with the context, whichever of the filler, the loop and
    /// the restriction the context learns last. The filler last is the order the round-1
    /// probe did not reach.
    #[test]
    fn r7_identifies_a_real_predecessor_with_a_counted_loop_whatever_comes_last() {
        let f = Fixture::new(
            "SubClassOf(:M ObjectMaxCardinality(1 :s :B))
             SubClassOf(:M ObjectHasSelf(:l))
             SubObjectPropertyOf(:l :s)
             SubObjectPropertyOf(ObjectInverseOf(:r) :s)
             SubClassOf(:P ObjectSomeValuesFrom(:r :M))
             ClassAssertion(:B :a)",
        );
        let (m, p) = (f.class("M"), f.class("P"));
        let (r, l) = (f.role("r"), f.role("l"));
        let (at_most, _, b) = f.at_most_of(m);
        assert!(
            f.ix.horn.as_ref().unwrap().is(b, HornIx::FILLER),
            "the filler is marked"
        );
        let filler_last = [
            Concl::Singular,
            Concl::Sub(at_most),
            Concl::Back(p, r),
            Concl::PredIn(p, b),
            Concl::RealBack(p),
            Concl::Loop(l),
            Concl::Sub(b),
        ];
        let loop_last = [
            Concl::Singular,
            Concl::Sub(at_most),
            Concl::Back(p, r),
            Concl::PredIn(p, b),
            Concl::RealBack(p),
            Concl::Sub(b),
            Concl::Loop(l),
        ];
        let restriction_last = [
            Concl::Singular,
            Concl::Back(p, r),
            Concl::PredIn(p, b),
            Concl::RealBack(p),
            Concl::Loop(l),
            Concl::Sub(b),
            Concl::Sub(at_most),
        ];
        for (name, seq) in [
            ("filler last", &filler_last),
            ("loop last", &loop_last),
            ("restriction last", &restriction_last),
        ] {
            let outs = deliver(&f, m, seq);
            let before = &outs[..seq.len() - 1];
            assert_eq!(
                sent_to(before, p, Concl::Same(m)),
                0,
                "{name}: nothing before the last step"
            );
            assert_eq!(
                local(before, Concl::Same(p)),
                0,
                "{name}: nothing before the last step"
            );
            assert_eq!(
                sent_to(&outs, p, Concl::Same(m)),
                1,
                "{name}: the predecessor is told"
            );
            assert_eq!(
                local(&outs, Concl::Same(p)),
                1,
                "{name}: the context is told"
            );
            // R6 also makes the predecessor the instance, with the loops
            assert_eq!(
                sent_to(&outs, p, Concl::Loop(l)),
                1,
                "{name}: R6 gives the loop"
            );
            assert_eq!(
                sent_to(&outs, p, Concl::Sub(m)),
                1,
                "{name}: R6 gives the conjunction"
            );
        }
    }
}

/// Left-linear composition of transitive roles (roadmap item 956).
#[cfg(test)]
mod linear_chain_tests {
    use super::*;
    use crate::classify::index::{Axioms, Interner, RoleName};
    use crate::model::{Class, ObjectProperty};

    /// The roles of a case: which are transitive, which are under which, and the other
    /// chains, by index.
    struct Case {
        roles: usize,
        transitive: &'static [usize],
        subs: &'static [(usize, usize)],
        chains: &'static [(usize, usize, usize)],
        /// the roles that should compose left-linearly
        linear: &'static [usize],
    }

    const CASES: &[Case] = &[
        // one transitive role
        Case {
            roles: 1,
            transitive: &[0],
            subs: &[],
            chains: &[],
            linear: &[0],
        },
        // a strict subrole of a transitive role composes with its links into the role
        Case {
            roles: 2,
            transitive: &[0],
            subs: &[(1, 0)],
            chains: &[],
            linear: &[0],
        },
        // equivalent roles, of which the first stays the least result
        Case {
            roles: 2,
            transitive: &[0],
            subs: &[(1, 0), (0, 1)],
            chains: &[],
            linear: &[0],
        },
        // a chain into the transitive role from other roles keeps it linear
        Case {
            roles: 3,
            transitive: &[0],
            subs: &[],
            chains: &[(1, 2, 0)],
            linear: &[0],
        },
        // a chain with the transitive role on its right into the role keeps it linear
        Case {
            roles: 2,
            transitive: &[0],
            subs: &[],
            chains: &[(1, 0, 0)],
            linear: &[0],
        },
        // and into the left role, as regulates ∘ part_of ⊑ regulates
        Case {
            roles: 2,
            transitive: &[0],
            subs: &[],
            chains: &[(1, 0, 1)],
            linear: &[0],
        },
        // but not into a third role that no chain composes again with the role
        Case {
            roles: 3,
            transitive: &[0],
            subs: &[],
            chains: &[(1, 0, 2)],
            linear: &[],
        },
        // a chain with the transitive role on its left keeps it linear, and two
        // transitive roles under a third
        Case {
            roles: 4,
            transitive: &[0, 2],
            subs: &[(0, 3), (2, 3)],
            chains: &[(0, 1, 1)],
            linear: &[0, 2],
        },
    ];

    struct Built {
        ix: Index,
        size: usize,
        classes: Vec<Ex>,
        roles: Vec<Role>,
    }

    /// The case's roles over `n` classes with pseudo-random existential restrictions,
    /// two thirds of them on the first role, and a negative existential over each role
    /// so that the links propagate a subsumer.
    fn build(case: &Case, n: usize, seed: u64) -> Built {
        let mut it = Interner::new();
        let classes: Vec<Ex> = (0..n)
            .map(|i| it.class(&Class::new(format!("urn:linear:C{i}"))))
            .collect();
        let roles: Vec<Role> = (0..case.roles)
            .map(|i| {
                it.role(RoleName::Object(ObjectProperty::new(format!(
                    "urn:linear:r{i}"
                ))))
            })
            .collect();
        let mut x = seed | 1;
        let mut next = move |m: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % m as u64) as usize
        };
        let mut subs = Vec::new();
        for _ in 0..3 * n {
            let (a, b) = (classes[next(n)], classes[next(n)]);
            let r = if next(3) < 2 {
                roles[0]
            } else {
                roles[next(case.roles)]
            };
            let e = it.node(Node::Exist(r, b));
            subs.push((a, e));
        }
        for (i, &r) in roles.iter().enumerate() {
            let goal = it.class(&Class::new(format!("urn:linear:G{i}")));
            let e = it.node(Node::Exist(r, classes[next(n)]));
            subs.push((e, goal));
        }
        let mut chains: Vec<(Vec<Role>, Role)> = case
            .transitive
            .iter()
            .map(|&t| (vec![roles[t], roles[t]], roles[t]))
            .collect();
        chains.extend(
            case.chains
                .iter()
                .map(|&(a, b, t)| (vec![roles[a], roles[b]], roles[t])),
        );
        let ax = Axioms {
            subs,
            role_subs: case
                .subs
                .iter()
                .map(|&(a, b)| (roles[a], roles[b]))
                .collect(),
            chains,
            ..Axioms::default()
        };
        let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();
        Built {
            ix,
            size: it.len(),
            classes,
            roles,
        }
    }

    /// What a saturation of every class leaves: each context's subsumers and backward
    /// links, sorted, and the forward links stored in all.
    /// A context's subsumers and backward links.
    type Held = (Vec<Ex>, Vec<(Role, Ex)>);

    fn saturated(b: &Built, threads: usize) -> (Vec<Held>, usize) {
        let ctx: Contexts = (0..b.size).map(|_| OnceLock::new()).collect();
        let bounds = Bounds {
            threads,
            deadline: None,
            max_memory: None,
            cancelled: None,
            charge: None,
            base_memory: 0,
            peak_memory: 0,
            dynamic: None,
            dl: None,
            partial: false,
            steps: false,
        };
        saturate(&b.ix, &ctx, &b.classes, &[], &[], &bounds).unwrap();
        let mut fwd = 0;
        let states = ctx
            .iter()
            .map(|c| {
                let Some(c) = c.get() else {
                    return (Vec::new(), Vec::new());
                };
                let st = c.st.lock();
                fwd += st.fwd.list.len();
                let mut subs: Vec<Ex> = st.subs.iter().copied().collect();
                subs.sort_unstable();
                let mut back: Vec<(Role, Ex)> = st
                    .back
                    .iter()
                    .flat_map(|(r, v)| v.list.iter().map(move |&e| (*r, e)))
                    .collect();
                back.sort_unstable();
                (subs, back)
            })
            .collect();
        (states, fwd)
    }

    #[test]
    fn transitive_roles_without_other_chains_on_their_right_are_linear() {
        for (i, case) in CASES.iter().enumerate() {
            let b = build(case, 4, 7);
            let want: Vec<bool> = (0..case.roles).map(|r| case.linear.contains(&r)).collect();
            let got: Vec<bool> = b
                .roles
                .iter()
                .map(|&r| b.ix.roles.linear[r as usize])
                .collect();
            assert_eq!(got, want, "case {i}");
        }
    }

    /// The rules applied one conclusion at a time with a sink that does not defer the
    /// products of links, as a world's does, to a fixpoint on one thread.
    fn immediate(b: &Built) -> Vec<Held> {
        struct Queue {
            me: Ex,
            out: Vec<(Ex, Concl)>,
        }
        impl Sink for Queue {
            fn send(&mut self, to: Ex, c: Concl) {
                self.out.push((to, c));
            }
            fn me(&mut self, c: Concl) {
                self.out.push((self.me, c));
            }
        }
        let mut states = vec![State::default(); b.size];
        let mut q = Queue {
            me: 0,
            out: Vec::new(),
        };
        let mut todo: VecDeque<(Ex, Concl)> = VecDeque::new();
        for &c in &b.classes {
            q.me = c;
            init(&b.ix, c, &mut states[c as usize], &[], &mut q);
            todo.extend(q.out.drain(..));
        }
        while let Some((e, c)) = todo.pop_front() {
            q.me = e;
            let st = &mut states[e as usize];
            if !st.init {
                init(&b.ix, e, st, &[], &mut q);
            }
            apply(&b.ix, e, st, c, &mut q);
            todo.extend(q.out.drain(..));
        }
        states
            .iter()
            .map(|st| {
                if !st.init {
                    return (Vec::new(), Vec::new());
                }
                let mut subs: Vec<Ex> = st.subs.iter().copied().collect();
                subs.sort_unstable();
                let mut back: Vec<(Role, Ex)> = st
                    .back
                    .iter()
                    .flat_map(|(r, v)| v.list.iter().map(move |&e| (*r, e)))
                    .collect();
                back.sort_unstable();
                (subs, back)
            })
            .collect()
    }

    /// The rules without the deferred flush give the same subsumers and backward links
    /// with left-linear composition as without it, and as the concurrent saturation.
    #[test]
    fn immediate_composition_stores_the_same_backward_links() {
        for (i, case) in CASES.iter().enumerate() {
            for seed in [3, 17] {
                let lin = build(case, 32, seed);
                let mut all = build(case, 32, seed);
                all.ix.roles.without_linear();
                let a = immediate(&lin);
                assert!(a == immediate(&all), "case {i} seed {seed}");
                assert!(
                    a == saturated(&lin, 1).0,
                    "case {i} seed {seed} against a run"
                );
            }
        }
    }

    /// A run that keeps the contexts of another compares the linear roles of the two
    /// indexes, since a kept context of a linear role lacks the forward links that a run
    /// without it composes with.
    #[test]
    fn indexes_with_other_linear_roles_do_not_share_their_contexts() {
        let a = build(&CASES[0], 8, 5);
        let mut b = build(&CASES[0], 8, 5);
        assert!(a.ix.same_tables(&b.ix) && b.ix.same_tables(&a.ix));
        b.ix.roles.without_linear();
        assert!(!a.ix.same_tables(&b.ix));
        assert!(!b.ix.same_tables(&a.ix));
    }

    /// The saturation with left-linear composition stores the subsumers and backward
    /// links of the one that composes every pair, on one thread and on several, with
    /// and without congested deliveries, and stores fewer forward links where a role is
    /// linear.
    #[test]
    fn left_linear_composition_stores_the_same_backward_links() {
        for (i, case) in CASES.iter().enumerate() {
            for seed in [3, 11, 29] {
                let lin = build(case, 48, seed);
                let mut all = build(case, 48, seed);
                all.ix.roles.without_linear();
                for threads in [1, 3] {
                    for congested in [false, true] {
                        ALWAYS_CONGESTED.set(congested);
                        let (a, fa) = saturated(&lin, threads);
                        let (b, fb) = saturated(&all, threads);
                        ALWAYS_CONGESTED.set(false);
                        let label = format!("case {i} seed {seed} {threads} threads {congested}");
                        assert!(a == b, "{label}");
                        assert!(fa <= fb, "{label}");
                        if !case.linear.is_empty() {
                            assert!(fa < fb, "{label}: {fa} against {fb}");
                        }
                    }
                }
            }
        }
    }
}
