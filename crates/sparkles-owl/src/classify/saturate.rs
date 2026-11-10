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
use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use parking_lot::{Condvar, Mutex};
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

    /// Whether the state already holds a conclusion, so that a world over it can drop
    /// the conclusion: the rules have applied to it.
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
            | Concl::RFact(..) => false,
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
/// The clauses of Tier D a context processes between checks of the bounds.
const DL_CLAUSES: usize = 256;
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
}

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
    dynamic: Option<&'a Dynamic>,
    bounds: &'a Bounds<'a>,
    /// where the run records its start of the helpers, [`STARTED_NS`] outside tests
    started: &'static AtomicU64,
    #[cfg(test)]
    always_congested: bool,
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
    /// the messages sent to other contexts since the last check
    sent: usize,
}

/// A worker's messages for other contexts, which it delivers by destination, with the
/// buffers that group them.
#[derive(Default)]
struct Outbox {
    msgs: Vec<(Ex, Concl)>,
    tmp: Vec<(Ex, Concl)>,
    counts: Vec<u32>,
}

/// The bits of a destination that one pass of the outbox's radix sort reads.
const RADIX_BITS: u32 = 11;
/// The messages below which an outbox sorts by comparison instead.
const RADIX_MIN: usize = 512;

impl Outbox {
    fn is_empty(&self) -> bool {
        self.msgs.is_empty()
    }

    /// Orders the messages by destination, keeping the order of each destination's.
    fn group(&mut self) {
        let n = self.msgs.len();
        if n < RADIX_MIN {
            self.msgs.sort_by_key(|m| m.0);
            return;
        }
        let max = self.msgs.iter().map(|m| m.0).max().unwrap_or(0);
        let buckets = 1usize << RADIX_BITS;
        let mask = (buckets - 1) as u32;
        let mut shift = 0u32;
        self.tmp.clear();
        self.tmp.resize(n, (0, Concl::Real));
        while shift == 0 || (shift < 32 && max >> shift != 0) {
            self.counts.clear();
            self.counts.resize(buckets, 0);
            for m in &self.msgs {
                self.counts[((m.0 >> shift) & mask) as usize] += 1;
            }
            let mut at = 0u32;
            for c in self.counts.iter_mut() {
                let k = *c;
                *c = at;
                at += k;
            }
            for &m in &self.msgs {
                let d = ((m.0 >> shift) & mask) as usize;
                self.tmp[self.counts[d] as usize] = m;
                self.counts[d] += 1;
            }
            std::mem::swap(&mut self.msgs, &mut self.tmp);
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
    /// the existential restriction whose hypothetical successor, keyed by a predecessor,
    /// the context `v` made during the run is (item 728)
    fn keyed_hyp(&self, v: Ex) -> Option<Ex> {
        let _ = v;
        None
    }
    /// whether the context has conclusions of its own waiting
    fn busy(&self) -> bool {
        false
    }
    /// whether the run goes on, after a check of its bounds, for rules whose one
    /// inference can make many conclusions
    fn go_on(&self) -> bool {
        true
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
        self.run.check(0, bytes, 0)
    }
    fn data_overflow(&mut self) {
        self.run.peak.fetch_max(usize::MAX, Ordering::Relaxed);
        self.run.halt(STOP_MEMORY);
    }
    fn release_data(&mut self, bytes: usize) {
        // a caller releases what it reserved, and a reservation the overflow check refused
        // before it added anything is released as well, so the count stops at zero
        let _ = self
            .run
            .memory
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                Some(m.saturating_sub(bytes))
            });
    }

    #[inline]
    fn send(&mut self, to: Ex, c: Concl) {
        if to == self.me {
            self.local.push(c);
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
            self.outbox.msgs.push((to, c));
            self.sent += 1;
            if self.outbox.msgs.len() >= OUTBOX {
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

    fn keyed_hyp(&self, v: Ex) -> Option<Ex> {
        if (v as usize) < self.run.ctx.len() {
            return None;
        }
        dl_rules::keyed_of(self.run.get_dynamic_core(v))
    }

    #[inline]
    fn busy(&self) -> bool {
        !self.local.is_empty()
    }

    fn go_on(&self) -> bool {
        self.run.check(0, 0, 0)
    }

    #[inline]
    fn stopped(&self) -> bool {
        self.run.stopped()
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
        let outbox = &mut outbox.msgs;
        let mut at = 0;
        while at < outbox.len() {
            let to = outbox[at].0;
            let end = at + outbox[at..].iter().take_while(|m| m.0 == to).count();
            let ctx = self.get(to);
            if !(filter && self.deliver_new(ctx, &outbox[at..end])) {
                ctx.todo
                    .lock()
                    .extend(outbox[at..end].iter().map(|&(_, c)| c));
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
    /// 695).
    #[cold]
    #[inline(never)]
    fn deliver_new(&self, ctx: &Ctx, msgs: &[(Ex, Concl)]) -> bool {
        let Some(st) = ctx.st.try_lock() else {
            return false;
        };
        if !st.init {
            return false;
        }
        let mut t = ctx.todo.lock();
        let before = t.len();
        t.extend(
            msgs.iter()
                .map(|&(_, c)| c)
                .filter(|c| !(matches!(c, Concl::Sub(_) | Concl::Back(..)) && st.holds(c))),
        );
        let dropped = msgs.len() - (t.len() - before);
        if dropped > 0 {
            self.queued.fetch_sub(dropped as isize, Ordering::Relaxed);
        }
        true
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
        if threads == 1 {
            self.worker::<R>(None);
            return;
        }
        std::thread::scope(|s| {
            let start = || {
                // counted while the first worker is busy, so that the count cannot reach 0
                // before they start
                self.pending.fetch_add(threads - 1, Ordering::SeqCst);
                let called = Instant::now();
                for _ in 1..threads {
                    s.spawn(move || self.worker::<R>(None));
                }
                // the saturations that start after this one wait as long, also while
                // this one runs
                let ns = called.elapsed().as_nanos().clamp(1, u64::MAX as u128);
                self.started.store(ns as u64, Ordering::Relaxed);
            };
            self.worker::<R>(Some(&start));
        });
    }

    /// A worker activates up to [`ROOTS_PER_ROUND`] roots, then the contexts those
    /// activations woke, in the order they woke, and the contexts these wake in turn,
    /// before it takes more roots. A context that many others send to, such as a class
    /// that fills many existential restrictions, then takes the conclusions of a round
    /// in one activation rather than one at a time.
    fn worker<R: Rules>(&self, mut others: Option<&dyn Fn()>) {
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
            self.process::<R>(next, &mut mine, &mut tally, &mut local, &mut outbox);
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
        // whether the run falls back or answers in part
        if b.dl.is_some_and(|d| d.over_work()) {
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
    ) {
        let ctx = self.get(me);
        let mut st = ctx.st.lock();
        local.clear();
        let mut out = Out {
            run: self,
            me,
            local,
            woke: mine,
            outbox,
            sent: 0,
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
            }
            ctx.started.store(true, Ordering::Release);
        }
        'outer: loop {
            while let Some(c) = out.local.pop() {
                if apply_rules::<_, R>(self.ix, me, &mut st, c, &mut out) {
                    tally.stored += 1;
                }
                tally.applied += 1;
                if tally.applied >= CHECK_EVERY || out.sent >= SEND_EVERY {
                    tally.queued += out.sent as isize;
                    out.sent = 0;
                    if !self.flush(tally) {
                        break 'outer;
                    }
                    // a long activation gives idle workers what it woke as it goes
                    if self.idle.load(Ordering::Relaxed) > 0 {
                        self.deliver(out.outbox, out.woke);
                        self.share(out.woke);
                    }
                }
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
            let Some(mut batch) = batch else {
                // R3 of the Horn rules waits for the activation's universals
                if R::HORN && horn_rules::flush(self.ix, &mut st, &mut out) {
                    continue;
                }
                // Tier D's clauses wait for the units, which the EL rules take first
                if R::DISJ {
                    let n = if self.ix.dl.as_deref().is_some_and(|d| d.is_root(me)) {
                        dl_root::flush(self.ix, &mut st, &mut out, DL_CLAUSES)
                    } else {
                        dl_rules::flush(self.ix, me, &mut st, &mut out, DL_CLAUSES)
                    };
                    if n > 0 {
                        tally.applied += n;
                        tally.queued += out.sent as isize;
                        out.sent = 0;
                        if !self.flush(tally) {
                            break 'outer;
                        }
                        continue;
                    }
                }
                break;
            };
            tally.queued -= batch.len() as isize;
            // apply the batch in the order it arrived
            batch.reverse();
            out.local.append(&mut batch);
        }
        tally.queued += out.sent as isize;
        // A run that answers in part when it stops gives back the conclusions it did not
        // apply, so that the context shows as not closed (roadmap item 603).
        if self.bounds.partial && !out.local.is_empty() {
            ctx.todo.lock().append(out.local);
        }
        drop(st);
        ctx.active.store(false, Ordering::SeqCst);
        if !self.stopped() && !ctx.todo.lock().is_empty() && self.mark(ctx) {
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
            for &t in ix.told_of(h) {
                out.me(Concl::Sub(t));
            }
            for &(p, k) in ix.conj_of(h) {
                if st.subs.contains(&p) {
                    out.me(Concl::Sub(k));
                }
            }
            for &(s, x) in ix.exist_of(h) {
                st.props.push((s, x));
                for (r, srcs) in &st.back {
                    if roles.sub(*r, s) {
                        for &e in &srcs.list {
                            out.send(e, Concl::Sub(x));
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
            for &(s, x) in &st.props {
                if roles.sub(r, s) {
                    out.send(e, Concl::Sub(x));
                }
            }
            for &(r2, f) in &st.fwd.list {
                for &t in roles.comp(r, r2) {
                    compose(roles, e, t, f, out);
                }
            }
            if R::FULL && ix.nominal_selfs {
                link_to(ix, me, st, e, r, out);
            }
            if R::HORN {
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
            for (r1, srcs) in &st.back {
                for &t in roles.comp(*r1, r2) {
                    for &e in &srcs.list {
                        compose(roles, e, t, f, out);
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
        Concl::Hyp(_)
        | Concl::DEdge(..)
        | Concl::PredC(..)
        | Concl::RPred(..)
        | Concl::HypL(..)
        | Concl::REdge(..)
        | Concl::RFact(..) => {
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

/// The link of a chain: an instance of `e` has an instance of `f` as a `t`-successor.
#[inline]
fn compose<S: Sink>(roles: &super::index::Roles, e: Ex, t: Role, f: Ex, out: &mut S) {
    out.send(f, Concl::Back(e, t));
    if roles.keeps_forward(t) {
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
            dynamic: None,
            bounds,
            started: &STARTED_NS,
            always_congested: false,
        }
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
        };
        let run = run(&f.ix, &ctx, &bounds);
        let mut tally = Tally::default();
        run.process::<Horn>(
            f.root,
            &mut VecDeque::new(),
            &mut tally,
            &mut Vec::new(),
            &mut Outbox::default(),
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
                    };
                    let run = run(&f.ix, &ctx, &bounds);
                    let mut tally = Tally::default();
                    run.process::<Horn>(
                        f.root,
                        &mut VecDeque::new(),
                        &mut tally,
                        &mut Vec::new(),
                        &mut Outbox::default(),
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
            };
            let run = run(&f.ix, &ctx, &bounds);
            let mut tally = Tally::default();
            run.process::<Horn>(
                f.root,
                &mut VecDeque::new(),
                &mut tally,
                &mut Vec::new(),
                &mut Outbox::default(),
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
