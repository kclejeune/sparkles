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
use std::collections::hash_map::Entry;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
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
    pub back: FxHashMap<Role, Vec<Ex>>,
    back_seen: FxHashSet<u64>,
    pub fwd: Vec<(Role, Ex)>,
    fwd_seen: FxHashSet<u64>,
    /// the member of each disjointness set among the subsumers
    pub disj: FxHashMap<u32, Ex>,
    loops: Vec<Role>,
}

impl State {
    /// The bytes the state takes, roughly.
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<State>()
            + self.subs.capacity() * 5
            + self.props.capacity() * 8
            + self.back.capacity() * 32
            + self.back.values().map(|v| v.capacity() * 4).sum::<usize>()
            + self.back_seen.capacity() * 9
            + self.fwd.capacity() * 8
            + self.fwd_seen.capacity() * 9
            + self.disj.capacity() * 9
    }

    /// Removes the backward links whose source is in `gone`.
    pub fn drop_links_from(&mut self, gone: &FxHashSet<Ex>) {
        let mut any = false;
        for srcs in self.back.values_mut() {
            let before = srcs.len();
            srcs.retain(|e| !gone.contains(e));
            any |= srcs.len() != before;
        }
        if any {
            self.back.retain(|_, v| !v.is_empty());
            self.back_seen
                .retain(|k| !gone.contains(&((*k >> 32) as Ex)));
        }
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
/// The bytes counted for each stored conclusion.
const CONCLUSION_BYTES: usize = 16;
/// The bytes counted for each context a saturation starts.
const CONTEXT_BYTES: usize = 320;

struct Run<'a> {
    ix: &'a Index,
    nodes: &'a [Node],
    ctx: &'a Contexts,
    queue: Mutex<Vec<Ex>>,
    ready: Condvar,
    /// the active contexts, on a worker's stack, in the queue or being processed
    pending: AtomicUsize,
    /// the workers waiting for the queue
    idle: AtomicUsize,
    stop: AtomicU8,
    memory: AtomicUsize,
    applied: AtomicUsize,
    bounds: &'a Bounds<'a>,
}

/// What a saturation did.
#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    pub conclusions: usize,
    pub memory: usize,
}

/// Saturates `roots` and every context they reach. Contexts already saturated keep their
/// conclusions and take new ones.
pub fn saturate(
    ix: &Index,
    nodes: &[Node],
    ctx: &Contexts,
    roots: &[Ex],
    bounds: &Bounds<'_>,
) -> Result<Totals, Stop> {
    let run = Run {
        ix,
        nodes,
        ctx,
        queue: Mutex::new(Vec::new()),
        ready: Condvar::new(),
        pending: AtomicUsize::new(0),
        idle: AtomicUsize::new(0),
        stop: AtomicU8::new(STOP_NONE),
        memory: AtomicUsize::new(bounds.base_memory),
        applied: AtomicUsize::new(0),
        bounds,
    };
    {
        let mut q = run.queue.lock();
        for &r in roots {
            let c = run.get(r);
            if !c.st.lock().init && run.mark(c) {
                q.push(r);
            }
        }
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
        STOP_MEMORY => Err(Stop::Memory { used: memory }),
        _ => Ok(Totals {
            conclusions: run.applied.load(Ordering::Relaxed),
            memory,
        }),
    }
}

/// Where a context's rules send their conclusions: to itself, applied in the same pass,
/// or to other contexts' to-do lists.
struct Out<'r, 'a> {
    run: &'r Run<'a>,
    me: Ex,
    local: Vec<Concl>,
    /// the contexts this worker activated
    woke: &'r mut Vec<Ex>,
}

impl Out<'_, '_> {
    #[inline]
    fn send(&mut self, to: Ex, c: Concl) {
        if to == self.me {
            self.local.push(c);
        } else {
            let ctx = self.run.get(to);
            ctx.todo.lock().push(c);
            if self.run.mark(ctx) {
                self.woke.push(to);
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

    /// Moves half of a worker's stack to the shared queue when a worker waits for it.
    fn share(&self, mine: &mut Vec<Ex>) {
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

    fn worker(&self) {
        let mut mine: Vec<Ex> = Vec::new();
        loop {
            let next = match mine.pop() {
                Some(e) => e,
                None => {
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
                }
            };
            if self.stopped() {
                return;
            }
            self.process(next, &mut mine);
            self.share(&mut mine);
            if self.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
                let _q = self.queue.lock();
                self.ready.notify_all();
            }
        }
    }

    /// Checks the bounds, and stops the run when one is reached.
    fn check(&self, applied: usize, bytes: usize) -> bool {
        self.applied.fetch_add(applied, Ordering::Relaxed);
        let used = self.memory.fetch_add(bytes, Ordering::Relaxed) + bytes;
        if self.stopped() {
            return false;
        }
        let b = self.bounds;
        if b.max_memory.is_some_and(|m| used > m)
            || b.charge.is_some_and(|f| bytes > 0 && !f(bytes))
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

    fn process(&self, me: Ex, mine: &mut Vec<Ex>) {
        let ctx = self.get(me);
        let mut st = ctx.st.lock();
        let mut out = Out {
            run: self,
            me,
            local: Vec::new(),
            woke: mine,
        };
        let mut applied = 0usize;
        let mut stored = 0usize;
        if !st.init {
            st.init = true;
            stored += CONTEXT_BYTES / CONCLUSION_BYTES;
            out.me(Concl::Sub(me));
            out.me(Concl::Sub(TOP));
            for &r in &self.ix.roles.reflexive {
                out.me(Concl::Loop(r));
            }
        }
        let mut batch: Vec<Concl> = Vec::new();
        'outer: loop {
            while let Some(c) = out.local.pop() {
                if self.apply(me, &mut st, c, &mut out) {
                    stored += 1;
                }
                applied += 1;
                if applied.is_multiple_of(CHECK_EVERY) {
                    if !self.check(applied, stored * CONCLUSION_BYTES) {
                        break 'outer;
                    }
                    applied = 0;
                    stored = 0;
                }
            }
            {
                let mut t = ctx.todo.lock();
                if t.is_empty() {
                    break;
                }
                std::mem::swap(&mut *t, &mut batch);
            }
            // apply the batch in the order it arrived
            batch.reverse();
            out.local.append(&mut batch);
        }
        self.check(applied, stored * CONCLUSION_BYTES);
        drop(out);
        drop(st);
        ctx.active.store(false, Ordering::SeqCst);
        if !self.stopped() && !ctx.todo.lock().is_empty() && self.mark(ctx) {
            mine.push(me);
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
                match self.nodes[d as usize] {
                    Node::Bottom => {
                        st.bottom = true;
                        for srcs in st.back.values() {
                            for &e in srcs {
                                out.send(e, Concl::Sub(BOTTOM));
                            }
                        }
                        return true;
                    }
                    Node::Conj(a, b) => {
                        out.me(Concl::Sub(a));
                        out.me(Concl::Sub(b));
                    }
                    Node::Exist(r, f) => {
                        let succ = ix.succ.get(&d).copied().unwrap_or(f);
                        out.send(succ, Concl::Back(me, r));
                        if roles.keeps_forward(r) {
                            out.me(Concl::Fwd(r, succ));
                        }
                    }
                    Node::SelfLoop(r) => out.me(Concl::Loop(r)),
                    _ => {}
                }
                for &t in ix.told(d) {
                    out.me(Concl::Sub(t));
                }
                for &(p, k) in ix.conj(d) {
                    if st.subs.contains(&p) {
                        out.me(Concl::Sub(k));
                    }
                }
                for &(s, x) in ix.exist(d) {
                    st.props.push((s, x));
                    for (&r, srcs) in &st.back {
                        if roles.sub(r, s) {
                            for &e in srcs {
                                out.send(e, Concl::Sub(x));
                            }
                        }
                    }
                }
                for &set in ix.disjoint(d) {
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
                if !st.back_seen.insert(((e as u64) << 32) | r as u64) {
                    return false;
                }
                st.back.entry(r).or_default().push(e);
                if st.bottom {
                    out.send(e, Concl::Sub(BOTTOM));
                    return true;
                }
                for &(s, x) in &st.props {
                    if roles.sub(r, s) {
                        out.send(e, Concl::Sub(x));
                    }
                }
                for &(r2, f) in &st.fwd {
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
                if !st.fwd_seen.insert(((r2 as u64) << 32) | f as u64) {
                    return false;
                }
                st.fwd.push((r2, f));
                for (&r1, srcs) in &st.back {
                    for &t in roles.comp(r1, r2) {
                        for &e in srcs {
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
