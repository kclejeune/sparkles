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
use super::dl_rules;
use super::horn::HornIx;
use super::horn_rules;
use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use parking_lot::{Condvar, Mutex};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::VecDeque;
use std::collections::hash_map::Entry;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, AtomicUsize, Ordering};
use std::time::Instant;

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
    /// Tier D: the marker `A → A` of an atom that may hold of the context's instance
    Hyp(Ex),
    /// Tier D: the source context registers with this hypothetical successor by its
    /// existential restriction
    DEdge(Ex, Ex),
    /// Tier D: the successor sent a clause over its predecessor, by its id in the run's
    /// table
    PredC(Ex, u32),
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
                .map(|g| 96 + (g.funs.len() + g.roles.len() + g.val.opaque.len()) * 4)
                .sum::<usize>()
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
            Concl::SuccIn(..)
            | Concl::PredIn(..)
            | Concl::Succ(..)
            | Concl::DEdge(..)
            | Concl::PredC(..) => false,
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
    Memory { used: usize },
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
    /// whether the workers send through outboxes, which they do when there are several
    batch: bool,
    /// whether the rules run without those of individuals and functional data roles
    plain: bool,
    /// whether the rules are those of Tier H
    horn: bool,
    /// whether the rules are those of Tier D
    disj: bool,
    dynamic: Option<&'a Dynamic>,
    bounds: &'a Bounds<'a>,
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
    let run = Run {
        ix,
        ctx,
        universal,
        roots,
        next_root: AtomicUsize::new(0),
        queue: Mutex::new(Vec::new()),
        ready: Condvar::new(),
        pending: AtomicUsize::new(bounds.threads.max(1)),
        idle: AtomicUsize::new(0),
        stop: AtomicU8::new(STOP_NONE),
        memory: AtomicUsize::new(bounds.base_memory),
        queued: AtomicIsize::new(0),
        peak: AtomicUsize::new(bounds.base_memory.max(bounds.peak_memory)),
        applied: AtomicUsize::new(0),
        merged: AtomicBool::new(false),
        batch: bounds.threads > 1,
        plain: plain(ix),
        horn: ix.horn.is_some() && bounds.dynamic.is_some(),
        disj: ix.dl.is_some() && bounds.dl.is_some(),
        dynamic: bounds.dynamic,
        bounds,
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
    if threads == 1 {
        run.worker();
    } else {
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| run.worker());
            }
        });
    }
    let memory = run.memory.load(Ordering::Relaxed);
    match run.stop.load(Ordering::SeqCst) {
        STOP_CANCEL => Err(Stop::Cancelled),
        STOP_TIMEOUT => Err(Stop::Timeout),
        // the most counted, waiting messages included, which is what passed the limit
        STOP_MEMORY => Err(Stop::Memory {
            used: run.peak.load(Ordering::Relaxed).max(memory),
        }),
        _ => Ok(Totals {
            conclusions: run.applied.load(Ordering::Relaxed),
            memory,
            peak: run.peak.load(Ordering::Relaxed).max(memory),
            merged: run.merged.load(Ordering::Relaxed),
        }),
    }
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
}

impl Sink for Out<'_, '_> {
    #[inline]
    fn send(&mut self, to: Ex, c: Concl) {
        if to == self.me {
            self.local.push(c);
        } else if !self.run.batch {
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

    #[inline]
    fn busy(&self) -> bool {
        !self.local.is_empty()
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

    pub fn core(&self, e: Ex) -> &[Ex] {
        self.slot(e).core.get().map_or(&[], |c| c)
    }

    pub fn ctx(&self, e: Ex) -> &Ctx {
        self.slot(e).ctx.get_or_init(Box::default)
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
        let outbox = &mut outbox.msgs;
        let mut at = 0;
        while at < outbox.len() {
            let to = outbox[at].0;
            let end = at + outbox[at..].iter().take_while(|m| m.0 == to).count();
            let ctx = self.get(to);
            ctx.todo
                .lock()
                .extend(outbox[at..end].iter().map(|&(_, c)| c));
            if self.mark(ctx) {
                mine.push_back(to);
            }
            at = end;
        }
        outbox.clear();
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

    /// A worker activates up to [`ROOTS_PER_ROUND`] roots, then the contexts those
    /// activations woke, in the order they woke, and the contexts these wake in turn,
    /// before it takes more roots. A context that many others send to, such as a class
    /// that fills many existential restrictions, then takes the conclusions of a round
    /// in one activation rather than one at a time.
    fn worker(&self) {
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
            if self.horn {
                self.process::<Horn>(next, &mut mine, &mut tally, &mut local, &mut outbox);
            } else if self.disj {
                self.process::<Disj>(next, &mut mine, &mut tally, &mut local, &mut outbox);
            } else if self.plain {
                self.process::<Plain>(next, &mut mine, &mut tally, &mut local, &mut outbox);
            } else {
                self.process::<Full>(next, &mut mine, &mut tally, &mut local, &mut outbox);
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
        let b = self.bounds;
        let grown = used.saturating_sub(before);
        if b.max_memory.is_some_and(|m| used > m)
            || b.charge.is_some_and(|f| grown > 0 && !f(grown))
        {
            self.halt(STOP_MEMORY);
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
    /// is left, with the rules that `R` holds.
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
                st.init = true;
                out.me(Concl::Sub(TOP));
                for &m in core {
                    out.me(Concl::Sub(m));
                }
                for &r in &self.ix.roles.reflexive {
                    out.me(Concl::Loop(r));
                }
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
                    let n = dl_rules::flush(self.ix, me, &mut st, &mut out, DL_CLAUSES);
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

/// Applies the rules to one conclusion. The answer says whether it was new.
pub fn apply<S: Sink>(ix: &Index, me: Ex, st: &mut State, c: Concl, out: &mut S) -> bool {
    apply_rules::<S, Full>(ix, me, st, c, out)
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
                    }
                    if R::HORN {
                        horn_rules::succ(ix, me, st, r, succ, true, out);
                    }
                }
                Node::Forall(s, a)
                    if R::HORN && !ix.horn.as_ref().is_some_and(|h| h.is(d, HornIx::UNFOLDED)) =>
                {
                    horn_rules::forall(ix, me, st, s, a, out)
                }
                Node::AtMost1(s, b) if R::HORN => horn_rules::at_most(ix, me, st, s, b, out),
                Node::SelfLoop(r) => out.me(Concl::Loop(r)),
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
            }
            true
        }
        Concl::Nom(k) if R::FULL => apply_nom(me, st, k, out),
        Concl::Real if R::FULL => apply_real(ix, st, out),
        Concl::LinkTo(r, n) if R::FULL => apply_link_to(me, st, r, n, out),
        Concl::Nom(_) | Concl::Real | Concl::LinkTo(..) => {
            unreachable!("a conclusion of individuals in a saturation without them")
        }
        Concl::SuccIn(n, b) if R::HORN => horn_rules::succ_in(ix, me, st, n, b, out),
        Concl::PredIn(m, b) if R::HORN => horn_rules::pred_in(ix, me, st, m, b, out),
        Concl::Succ(r, n) if R::HORN => {
            horn_rules::succ(ix, me, st, r, n, false, out);
            true
        }
        Concl::SuccIn(..) | Concl::PredIn(..) | Concl::Succ(..) => {
            unreachable!("a conclusion of the Horn rules in a saturation without them")
        }
        Concl::Hyp(a) if R::DISJ => dl_rules::hyp(st, a, out),
        Concl::DEdge(u, e) if R::DISJ => dl_rules::dedge(ix, me, st, u, e, out),
        Concl::PredC(v, c) if R::DISJ => dl_rules::pred_c(ix, st, v, c, out),
        Concl::Hyp(_) | Concl::DEdge(..) | Concl::PredC(..) => {
            unreachable!("a conclusion of Tier D in a saturation without it")
        }
    }
}

/// A context registers as one that stands for its individual.
#[cold]
#[inline(never)]
fn apply_nom<S: Sink>(me: Ex, st: &mut State, k: Ex, out: &mut S) -> bool {
    if k == me || !st.extra().members.insert(k) {
        return false;
    }
    if st.bottom {
        out.send(k, Concl::Sub(BOTTOM));
    } else {
        for &s in &st.subs {
            out.send(k, Concl::Sub(s));
        }
    }
    true
}

/// A context becomes real: its successors do too, and it gives its individuals what it
/// knows.
#[cold]
#[inline(never)]
fn apply_real<S: Sink>(ix: &Index, st: &mut State, out: &mut S) -> bool {
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
        for &n in &x.noms {
            for &s in &st.subs {
                out.send(n, Concl::Sub(s));
            }
        }
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
    let fs = ix.roles.functional(r);
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
}
