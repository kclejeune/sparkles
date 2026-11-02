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

use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use parking_lot::{Condvar, Mutex};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::VecDeque;
use std::collections::hash_map::Entry;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, AtomicUsize, Ordering};
use std::time::Instant;

/// A conclusion waiting in a context's to-do list.
#[derive(Clone, Copy, Debug)]
enum Concl {
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
}

/// A set kept as a list in the order of insertion, with a hash set beside it once it
/// has more than a few members, so that a small set takes one allocation.
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

/// The conclusions of one context.
#[derive(Default)]
pub struct State {
    pub init: bool,
    pub subs: FxHashSet<Ex>,
    pub bottom: bool,
    /// the negative existential restrictions whose filler is a subsumer, which a new
    /// backward link propagates to its source
    pub props: Vec<(Role, Ex)>,
    /// the sources of backward links, by role
    pub back: Vec<(Role, Small<Ex>)>,
    /// the forward links, by role and target
    pub fwd: Small<(Role, Ex)>,
    /// the member of each disjointness set among the subsumers
    pub disj: FxHashMap<u32, Ex>,
    loops: Vec<Role>,
}

impl State {
    /// The bytes the state's tables and lists take, roughly.
    pub fn bytes(&self) -> usize {
        table_bytes(self.subs.capacity(), 4)
            + self.props.capacity() * 8
            + self.back.capacity() * std::mem::size_of::<(Role, Small<Ex>)>()
            + self.back.iter().map(|(_, v)| v.bytes()).sum::<usize>()
            + self.fwd.bytes()
            + table_bytes(self.disj.capacity(), 8)
            + self.loops.capacity() * 4
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
    todo: Mutex<Vec<Concl>>,
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
const CONCLUSION_BYTES: usize = 16;
/// The roots a worker takes from the shared list at a time.
const ROOT_CHUNK: usize = 64;
/// The roots a worker activates before the contexts they woke.
const ROOTS_PER_ROUND: usize = 1024;
/// The bytes counted for each context a saturation starts.
const CONTEXT_BYTES: usize = 320;

struct Run<'a> {
    ix: &'a Index,
    ctx: &'a Contexts,
    /// the roots, which the workers take in chunks from `next_root` on
    roots: &'a [Ex],
    next_root: AtomicUsize,
    queue: Mutex<Vec<Ex>>,
    ready: Condvar,
    /// the active contexts, on a worker's stack, in the queue or being processed
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
}

/// Saturates `roots` and every context they reach. Contexts already saturated keep their
/// conclusions and take new ones.
pub fn saturate(
    ix: &Index,
    ctx: &Contexts,
    roots: &[Ex],
    bounds: &Bounds<'_>,
) -> Result<Totals, Stop> {
    let run = Run {
        ix,
        ctx,
        roots,
        next_root: AtomicUsize::new(0),
        queue: Mutex::new(Vec::new()),
        ready: Condvar::new(),
        pending: AtomicUsize::new(0),
        idle: AtomicUsize::new(0),
        stop: AtomicU8::new(STOP_NONE),
        memory: AtomicUsize::new(bounds.base_memory),
        queued: AtomicIsize::new(0),
        peak: AtomicUsize::new(bounds.base_memory),
        applied: AtomicUsize::new(0),
        bounds,
    };
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
        }),
    }
}

/// Where a context's rules send their conclusions: to itself, applied in the same pass,
/// or to other contexts' to-do lists.
struct Out<'r, 'a> {
    run: &'r Run<'a>,
    me: Ex,
    local: &'r mut Vec<Concl>,
    /// the contexts this worker activated
    woke: &'r mut VecDeque<Ex>,
    /// the messages sent to other contexts since the last check
    sent: usize,
}

impl Out<'_, '_> {
    #[inline]
    fn send(&mut self, to: Ex, c: Concl) {
        if to == self.me {
            self.local.push(c);
        } else {
            let ctx = self.run.get(to);
            ctx.todo.lock().push(c);
            self.sent += 1;
            if self.run.mark(ctx) {
                self.woke.push_back(to);
            }
        }
    }

    #[inline]
    fn me(&mut self, c: Concl) {
        self.local.push(c);
    }
}

impl<'a> Run<'a> {
    fn get(&self, e: Ex) -> &'a Ctx {
        self.ctx[e as usize].get_or_init(Box::default)
    }

    /// Marks a context active, and says whether this call did, so that the caller
    /// schedules it. The plain load first keeps a busy context's flag from bouncing
    /// between cores. It cannot miss a context that is going idle: a sender pushes to the
    /// to-do list before it looks at the flag, and the processing worker clears the flag
    /// before it looks at the list, both under the list's lock.
    #[inline]
    fn mark(&self, c: &Ctx) -> bool {
        if c.active.load(Ordering::SeqCst) || c.active.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.pending.fetch_add(1, Ordering::SeqCst);
        true
    }

    /// Moves half of a worker's queue to the shared queue when a worker waits for it.
    fn share(&self, mine: &mut VecDeque<Ex>) {
        let idle = self.idle.load(Ordering::SeqCst);
        if idle == 0 || mine.is_empty() {
            return;
        }
        let give = mine.len().div_ceil(2);
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
                if !c.st.lock().init && self.mark(c) {
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
            } else {
                // no roots are left: what the worker counted goes to the totals before
                // it waits for the others
                round = 0;
                self.flush(&mut tally);
                let mut q = self.queue.lock();
                loop {
                    if self.stopped() {
                        return;
                    }
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
                return;
            }
            self.process(next, &mut mine, &mut tally, &mut local);
            self.share(&mut mine);
            if self.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
                let _q = self.queue.lock();
                self.ready.notify_all();
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
        let used = held + waiting.max(0) as usize * std::mem::size_of::<Concl>();
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

    fn process(&self, me: Ex, mine: &mut VecDeque<Ex>, tally: &mut Tally, local: &mut Vec<Concl>) {
        let ctx = self.get(me);
        let mut st = ctx.st.lock();
        local.clear();
        let mut out = Out {
            run: self,
            me,
            local,
            woke: mine,
            sent: 0,
        };
        if !st.init {
            st.init = true;
            tally.stored += CONTEXT_BYTES / CONCLUSION_BYTES;
            out.me(Concl::Sub(me));
            out.me(Concl::Sub(TOP));
            for &r in &self.ix.roles.reflexive {
                out.me(Concl::Loop(r));
            }
        }
        'outer: loop {
            while let Some(c) = out.local.pop() {
                if self.apply(me, &mut st, c, &mut out) {
                    tally.stored += 1;
                }
                tally.applied += 1;
                if tally.applied >= CHECK_EVERY || out.sent >= SEND_EVERY {
                    tally.queued += out.sent as isize;
                    out.sent = 0;
                    if !self.flush(tally) {
                        break 'outer;
                    }
                }
            }
            // the to-do list leaves the context with its buffer, so that a context keeps
            // no room for the messages it took
            let mut batch = {
                let mut t = ctx.todo.lock();
                if t.is_empty() {
                    break;
                }
                std::mem::take(&mut *t)
            };
            tally.queued -= batch.len() as isize;
            // apply the batch in the order it arrived
            batch.reverse();
            out.local.append(&mut batch);
        }
        tally.queued += out.sent as isize;
        drop(st);
        ctx.active.store(false, Ordering::SeqCst);
        if !self.stopped() && !ctx.todo.lock().is_empty() && self.mark(ctx) {
            mine.push_back(me);
        }
    }

    /// Applies the rules to one conclusion. The answer says whether it was new.
    fn apply(&self, me: Ex, st: &mut State, c: Concl, out: &mut Out<'_, 'a>) -> bool {
        let ix = self.ix;
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
                    }
                    Node::SelfLoop(r) => out.me(Concl::Loop(r)),
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
                        out.send(f, Concl::Back(e, t));
                        if roles.keeps_forward(t) {
                            out.send(e, Concl::Fwd(t, f));
                        }
                    }
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
                            out.send(f, Concl::Back(e, t));
                            if roles.keeps_forward(t) {
                                out.send(e, Concl::Fwd(t, f));
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
                // a loop is a link from the context to itself
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
                true
            }
        }
    }
}
