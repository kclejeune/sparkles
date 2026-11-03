//! What individuals make global: the facts of universal roles, the merges that keys
//! give, and the conditional worlds of the classes whose instances reach an individual.
//!
//! A context that derives `{a}` and is not real (see [`super::saturate`]) stands for `a`
//! only in the models where its root has an instance. What it knows beyond `a`'s
//! subsumers then holds of `a` only there (Baader, Brandt and Lutz, *Pushing the EL
//! Envelope*, IJCAI 2005, the reachability condition of rule CR6; Kazakov, Krötzsch and
//! Simančík, *Practical Reasoning with Nominals in the EL Family of Description Logics*,
//! KR 2012). Such a context is a *witness*. When its subsumers are those of `a`, merging
//! it with `a` changes no subsumer, link or loop of any context, since a context's links
//! follow from its subsumers and the successor contexts they share, so a class whose
//! instances reach no witness is classified completely by the global saturation.
//!
//! Every named class whose context reaches a witness through links is classified again
//! in a *world*: a copy-on-write layer over the saturated contexts in which the class has
//! an instance, so that every context it reaches is real there and gives its individuals
//! what it knows. A world keeps only the conclusions that the global state lacks, runs on
//! one thread, and reaches the same fixpoint in any order. Worlds of different classes
//! run in parallel, and only the class's own subsumers are read from its world.
//!
//! A world is the natural place for further assumptions: roadmap item 603's case
//! analysis can saturate a world with a chosen disjunct over the same shared state, and
//! the classes it needs are found by the same closure over links.

use super::data::Value;
use super::index::{BOTTOM, Ex, Index, Node, Role};
use super::saturate::{CONCLUSION_BYTES, CONTEXT_BYTES, Concl, Contexts, Sink, State, apply, init};
use rustc_hash::{FxHashMap, FxHashSet};

/// Reads the state of a context in place: the global one, or a world's copy.
pub trait View {
    /// Calls `f` with the state of `e` and whether it is real in this view, when `e`
    /// has a context.
    fn with(&self, e: Ex, f: &mut dyn FnMut(&State, bool));
    /// The number of contexts.
    fn len(&self) -> usize;
}

/// The global contexts.
pub struct Global<'a>(pub &'a Contexts);

impl View for Global<'_> {
    fn with(&self, e: Ex, f: &mut dyn FnMut(&State, bool)) {
        if let Some(c) = self.0.get(e as usize).and_then(|c| c.get()) {
            let st = c.st.lock();
            if st.init {
                f(&st, st.real);
            }
        }
    }
    fn len(&self) -> usize {
        self.0.len()
    }
}

/// The individuals a context stands for.
fn individuals_of(ix: &Index, e: Ex, st: &State) -> Vec<Ex> {
    let mut v = Vec::new();
    if matches!(ix.head(e).node, Node::Nominal(_)) {
        v.push(e);
    }
    v.extend_from_slice(st.noms());
    v
}

/// The facts of universal roles that `known` lacks and some real context now gives: a
/// negative `∃u.F` on a universal role u holds everywhere once F has an instance.
pub fn universal_facts(ix: &Index, view: &dyn View, known: &[Ex]) -> Vec<Ex> {
    let mut out: Vec<Ex> = Vec::new();
    if ix.universal.is_empty() {
        return out;
    }
    for e in 0..view.len() as Ex {
        view.with(e, &mut |st, real| {
            if !real {
                return;
            }
            for &(f, x) in &ix.universal {
                if !known.contains(&x) && !out.contains(&x) && st.subs.contains(&f) {
                    out.push(x);
                }
            }
        });
    }
    out.sort_unstable();
    out
}

/// The pairs of individuals that a key makes one and that are not one yet (Structural
/// Specification §9.5): two named instances of the key's class with a named individual
/// in common for each object role and a data value in common for each data role.
pub fn key_merges(ix: &Index, view: &dyn View) -> Vec<(Ex, Ex)> {
    if ix.keys.is_empty() {
        return Vec::new();
    }
    let n = view.len();
    // the individuals of each context, and whether it is real
    let mut inds: FxHashMap<Ex, (Vec<Ex>, bool)> = FxHashMap::default();
    for e in 0..n as Ex {
        view.with(e, &mut |st, real| {
            let v = individuals_of(ix, e, st);
            if !v.is_empty() {
                inds.insert(e, (v, real));
            }
        });
    }
    // the named successors and the data values of each individual
    let mut obj: FxHashMap<Ex, Vec<(Role, Ex)>> = FxHashMap::default();
    let mut dat: FxHashMap<Ex, Vec<(Role, Value)>> = FxHashMap::default();
    let mut subs: FxHashMap<Ex, FxHashSet<Ex>> = FxHashMap::default();
    let mut keyed: FxHashMap<Ex, FxHashSet<Ex>> = FxHashMap::default();
    for (&k, (zs, _)) in &inds {
        view.with(k, &mut |st, real| {
            for (r, srcs) in &st.back {
                for src in &srcs.list {
                    if let Some((is, true)) = inds.get(src) {
                        for &i in is {
                            for &z in zs {
                                obj.entry(i).or_default().push((*r, z));
                            }
                        }
                    }
                }
            }
            if !real {
                return;
            }
            for &i in zs {
                let d = dat.entry(i).or_default();
                for &s in &st.subs {
                    let h = ix.head(s);
                    if let Node::DataSome(r, _) = h.node
                        && let Some(v) = ix.vals[h.succ as usize].single()
                    {
                        d.push((r, v.clone()));
                    }
                }
                if let Some(x) = &st.extra {
                    for g in &x.groups {
                        if let Some(v) = g.val.single() {
                            for &r in &g.roles {
                                d.push((r, v.clone()));
                            }
                        }
                    }
                }
            }
            // an individual without an instance makes the ontology inconsistent, and
            // a merge with it would add nothing
            if matches!(ix.head(k).node, Node::Nominal(_)) && !st.bottom {
                subs.insert(k, st.subs.clone());
            }
        });
    }
    for key in &ix.keys {
        let members: Vec<Ex> = ix
            .nominals
            .iter()
            .copied()
            .filter(|i| subs.get(i).is_some_and(|s| s.contains(&key.class)))
            .collect();
        for (x, &a) in members.iter().enumerate() {
            for &b in &members[x + 1..] {
                if subs[&a].contains(&b) {
                    continue;
                }
                // a universal role links every pair, so the two share every named
                // individual and every literal by it
                let objects = key.objects.iter().all(|&p| {
                    if ix.is_universal(p) {
                        return true;
                    }
                    let of = |i: Ex| -> FxHashSet<Ex> {
                        obj.get(&i)
                            .into_iter()
                            .flatten()
                            .filter(|(r, _)| ix.roles.sub(*r, p))
                            .map(|&(_, z)| z)
                            .collect()
                    };
                    let (va, vb) = (of(a), of(b));
                    va.iter().any(|z| vb.contains(z))
                });
                let data = key.data.iter().all(|&p| {
                    if ix.is_universal(p) {
                        return true;
                    }
                    let of = |i: Ex| -> Vec<&Value> {
                        dat.get(&i)
                            .into_iter()
                            .flatten()
                            .filter(|(r, _)| ix.roles.sub(*r, p))
                            .map(|(_, v)| v)
                            .collect()
                    };
                    let (va, vb) = (of(a), of(b));
                    va.iter().any(|v| vb.contains(v))
                });
                if objects && data {
                    keyed.entry(a).or_default().insert(b);
                }
            }
        }
    }
    let mut out: Vec<(Ex, Ex)> = keyed
        .into_iter()
        .flat_map(|(a, bs)| bs.into_iter().map(move |b| (a, b)))
        .collect();
    out.sort_unstable();
    out
}

/// The witnesses: the contexts that are not real, derive an individual and know more
/// than it, or give a fact of a universal role that is not global. An intersection
/// among the subsumers adds nothing beyond its operands, which are subsumers too, so
/// only the other subsumers count.
pub fn witnesses(ix: &Index, ctx: &Contexts, universal: &[Ex]) -> Vec<Ex> {
    let view = Global(ctx);
    let mut out = Vec::new();
    for e in 0..ctx.len() as Ex {
        let mut mine: Option<(Vec<Ex>, Vec<Ex>)> = None;
        let mut universal_witness = false;
        view.with(e, &mut |st, real| {
            if real || st.bottom {
                return;
            }
            if !st.noms().is_empty() {
                let subs = st
                    .subs
                    .iter()
                    .copied()
                    .filter(|&s| !matches!(ix.head(s).node, Node::Conj(..)))
                    .collect();
                mine = Some((subs, st.noms().to_vec()));
            }
            universal_witness = ix
                .universal
                .iter()
                .any(|&(f, x)| !universal.contains(&x) && st.subs.contains(&f));
        });
        let more = mine.is_some_and(|(subs, noms)| {
            noms.iter().any(|&n| {
                let mut more = true;
                view.with(n, &mut |st, _| {
                    more = !st.bottom && subs.iter().any(|s| !st.subs.contains(s));
                });
                more
            })
        });
        if more || universal_witness {
            out.push(e);
        }
    }
    out
}

/// The named classes whose contexts reach one of `from` through links, `from` included,
/// which are not unsatisfiable, sorted.
pub fn reaching(ix: &Index, ctx: &Contexts, from: &[Ex]) -> Vec<Ex> {
    let view = Global(ctx);
    let mut seen: FxHashSet<Ex> = from.iter().copied().collect();
    let mut stack: Vec<Ex> = from.to_vec();
    let mut out = Vec::new();
    while let Some(e) = stack.pop() {
        let mut next = Vec::new();
        let mut keep = false;
        view.with(e, &mut |st, _| {
            keep = !st.bottom && matches!(ix.head(e).node, Node::Class(_));
            for (_, srcs) in &st.back {
                next.extend(srcs.list.iter().copied());
            }
        });
        if keep {
            out.push(e);
        }
        for s in next {
            if seen.insert(s) {
                stack.push(s);
            }
        }
    }
    out.sort_unstable();
    out
}

/// One context in a world.
#[derive(Default)]
struct Slot {
    real: bool,
    /// the world's copy of the state, made when a conclusion the global state lacks
    /// arrives
    st: Option<State>,
    todo: Vec<Concl>,
    queued: bool,
}

/// A world over the global contexts, in which one class has an instance.
pub struct World<'a> {
    ix: &'a Index,
    ctx: &'a Contexts,
    /// the facts of universal roles of the global state
    global_universal: &'a [Ex],
    /// the contexts that are real in the global state
    global_real: &'a [Ex],
    slots: FxHashMap<Ex, Slot>,
    queue: Vec<Ex>,
    pending: Vec<(Ex, Concl)>,
    /// the facts of universal roles that hold in this world alone
    universal: Vec<Ex>,
    /// told each change of the bytes the world holds: false stops it
    bound: &'a (dyn Fn(isize) -> bool + Sync),
    /// the bytes of the copies, the slots and the stored conclusions
    held: usize,
    /// the messages waiting in the slots' to-do lists
    waiting: usize,
    /// the bytes last told to `bound`
    reported: usize,
    applied: usize,
    halted: bool,
}

/// The conclusions a world applies between checks of its bound.
const CHECK_EVERY: usize = 1024;
/// The bytes counted for a slot.
const SLOT_BYTES: usize = 64;

struct WorldSink<'b> {
    me: Ex,
    local: &'b mut Vec<Concl>,
    out: &'b mut Vec<(Ex, Concl)>,
}

impl Sink for WorldSink<'_> {
    fn send(&mut self, to: Ex, c: Concl) {
        if to == self.me && c != Concl::Real {
            self.local.push(c);
        } else {
            self.out.push((to, c));
        }
    }
    fn me(&mut self, c: Concl) {
        self.local.push(c);
    }
}

impl View for World<'_> {
    fn with(&self, e: Ex, f: &mut dyn FnMut(&State, bool)) {
        match self.slots.get(&e) {
            Some(Slot {
                st: Some(st), real, ..
            }) => {
                if st.init {
                    f(st, *real || st.real)
                }
            }
            Some(Slot { real, .. }) => {
                let real = *real;
                Global(self.ctx).with(e, &mut |st, r| f(st, real || r));
            }
            None => Global(self.ctx).with(e, f),
        }
    }
    fn len(&self) -> usize {
        self.ctx.len()
    }
}

/// What a world found of its class: whether it is unsatisfiable, and its subsumers.
pub struct Found {
    pub bottom: bool,
    pub subs: Vec<Ex>,
}

impl<'a> World<'a> {
    pub fn new(
        ix: &'a Index,
        ctx: &'a Contexts,
        global_universal: &'a [Ex],
        global_real: &'a [Ex],
        bound: &'a (dyn Fn(isize) -> bool + Sync),
    ) -> World<'a> {
        World {
            ix,
            ctx,
            global_universal,
            global_real,
            slots: FxHashMap::default(),
            queue: Vec::new(),
            pending: Vec::new(),
            universal: Vec::new(),
            bound,
            held: 0,
            waiting: 0,
            reported: 0,
            applied: 0,
            halted: false,
        }
    }

    /// Tells the bound what the world holds now, its waiting messages included, and
    /// stops the world when the bound refuses.
    fn checkpoint(&mut self) {
        let used =
            self.held + (self.waiting + self.pending.len()) * std::mem::size_of::<(Ex, Concl)>();
        let delta = used as isize - self.reported as isize;
        self.reported = used;
        if !(self.bound)(delta) {
            self.halted = true;
        }
    }

    fn global_real(&self, e: Ex) -> bool {
        let mut r = false;
        Global(self.ctx).with(e, &mut |st, _| r = st.real);
        r
    }

    /// Delivers the pending conclusions.
    fn deliver(&mut self) {
        while let Some((to, c)) = self.pending.pop() {
            self.deliver_one(to, c);
        }
    }

    fn deliver_one(&mut self, to: Ex, c: Concl) {
        if !self.slots.contains_key(&to) {
            let real = self.global_real(to);
            self.held += SLOT_BYTES;
            self.slots.insert(
                to,
                Slot {
                    real,
                    ..Slot::default()
                },
            );
        }
        if c == Concl::Real {
            self.make_real(to);
            return;
        }
        let slot = &self.slots[&to];
        if slot.st.is_none() {
            let mut held = false;
            Global(self.ctx).with(to, &mut |st, _| held = st.holds(&c));
            if held {
                return;
            }
        }
        let slot = self.slots.get_mut(&to).expect("slot");
        slot.todo.push(c);
        self.waiting += 1;
        if slot.real && !slot.queued {
            slot.queued = true;
            self.queue.push(to);
        }
    }

    /// Makes a context real in the world: its successors become real, and it gives its
    /// individuals what it knows. A context without a copy does this from the global
    /// state, which it reads in place.
    fn make_real(&mut self, e: Ex) {
        let slot = self.slots.get_mut(&e).expect("slot");
        if slot.real {
            return;
        }
        slot.real = true;
        if slot.st.is_some() {
            slot.todo.push(Concl::Real);
            self.waiting += 1;
        } else {
            let ix = self.ix;
            let mut init = false;
            let mut out: Vec<(Ex, Concl)> = Vec::new();
            Global(self.ctx).with(e, &mut |st, _| {
                init = true;
                for &s in &st.subs {
                    if let Node::Exist(..) = ix.head(s).node {
                        out.push((ix.head(s).succ, Concl::Real));
                    }
                }
                for &n in st.noms() {
                    if st.bottom {
                        out.push((n, Concl::Sub(BOTTOM)));
                    }
                    for &s in &st.subs {
                        out.push((n, Concl::Sub(s)));
                    }
                }
            });
            if !init {
                // a context that the global saturation never reached
                slot.todo.push(Concl::Real);
                self.waiting += 1;
            }
            self.pending.extend(out);
        }
        for &x in &self.universal {
            self.pending.push((e, Concl::Sub(x)));
        }
        let slot = self.slots.get_mut(&e).expect("slot");
        if !slot.todo.is_empty() && !slot.queued {
            slot.queued = true;
            self.queue.push(e);
        }
    }

    /// Applies the waiting conclusions of the queued contexts until none is left.
    fn drain(&mut self) {
        self.deliver();
        while let Some(e) = self.queue.pop() {
            if self.halted {
                return;
            }
            let ix = self.ix;
            let slot = self.slots.get_mut(&e).expect("slot");
            slot.queued = false;
            let mut todo = std::mem::take(&mut slot.todo);
            self.waiting -= todo.len();
            let real = slot.real;
            let mut st = match slot.st.take() {
                Some(st) => st,
                None => {
                    let mut copy = None;
                    Global(self.ctx).with(e, &mut |st, _| copy = Some(st.clone()));
                    let mut st = copy.unwrap_or_default();
                    // the world made it real from the global state already
                    st.real |= real && st.init;
                    self.held += CONTEXT_BYTES + st.bytes();
                    self.checkpoint();
                    st
                }
            };
            let mut local: Vec<Concl> = Vec::new();
            let mut out: Vec<(Ex, Concl)> = Vec::new();
            {
                let mut sink = WorldSink {
                    me: e,
                    local: &mut local,
                    out: &mut out,
                };
                if !st.init {
                    let all: Vec<Ex> = self
                        .global_universal
                        .iter()
                        .chain(&self.universal)
                        .copied()
                        .collect();
                    init(ix, e, &mut st, &all, &mut sink);
                }
                todo.reverse();
                sink.local.append(&mut todo);
                while let Some(c) = sink.local.pop() {
                    if apply(ix, e, &mut st, c, &mut sink) {
                        self.held += CONCLUSION_BYTES;
                    }
                    self.applied += 1;
                    if self.applied.is_multiple_of(CHECK_EVERY) {
                        self.checkpoint();
                        if self.halted {
                            break;
                        }
                    }
                }
            }
            let slot = self.slots.get_mut(&e).expect("slot");
            slot.st = Some(st);
            self.pending.extend(out);
            self.deliver();
        }
    }

    /// Classifies `class` in its world: the class has an instance, and the world
    /// saturates what follows, with the facts of universal roles and the merges of keys
    /// that hold there.
    pub fn classify(mut self, class: Ex) -> Option<Found> {
        self.pending.push((class, Concl::Real));
        loop {
            self.drain();
            self.checkpoint();
            if self.halted {
                (self.bound)(-(self.reported as isize));
                return None;
            }
            let known: Vec<Ex> = self
                .global_universal
                .iter()
                .chain(&self.universal)
                .copied()
                .collect();
            let facts = universal_facts(self.ix, &self, &known);
            let merges = key_merges(self.ix, &self);
            if facts.is_empty() && merges.is_empty() {
                break;
            }
            for &x in &facts {
                self.universal.push(x);
                let real: Vec<Ex> = self
                    .slots
                    .iter()
                    .filter(|(_, s)| s.real)
                    .map(|(&e, _)| e)
                    .chain(self.global_real.iter().copied())
                    .collect();
                for e in real {
                    self.pending.push((e, Concl::Sub(x)));
                }
            }
            for (a, b) in merges {
                self.pending.push((a, Concl::Sub(b)));
                self.pending.push((b, Concl::Sub(a)));
            }
        }
        // a real context without an instance makes the world inconsistent, and then the
        // class has none
        let clash = self
            .slots
            .values()
            .any(|s| s.real && s.st.as_ref().is_some_and(|st| st.bottom));
        let mut found = Found {
            bottom: clash,
            subs: Vec::new(),
        };
        self.with(class, &mut |st, _| {
            found.bottom |= st.bottom;
            found.subs = st.subs.iter().copied().collect();
        });
        // the world's copies go when it does
        (self.bound)(-(self.reported as isize));
        Some(found)
    }
}
