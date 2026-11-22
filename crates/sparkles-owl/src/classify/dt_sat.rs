//! The theory rule of the datatype checker in the saturation (F24, "The EL and Horn
//! tiers").
//!
//! A context's data atoms of one checker component are one set of units. Each time the
//! context gains an atom of a component, the rule decides the set with the component's
//! body atoms, the atoms that occur negatively, and sends what it gives: `owl:Nothing`
//! for a conflict, each body atom the set entails and each class that a cover of atoms
//! giving that class gives. A cover that remains, or an answer the checker cannot give,
//! is kept as the context's last evaluation of the component, which the run reads after
//! the saturation to report the component's axioms as used in part. Answers are shared
//! by every context through a cache keyed by the sets asked about.

use super::dtcheck::atoms::DtKey;
use super::dtcheck::comp::Comps;
use super::dtcheck::range::{Ranges, Rid};
use super::dtcheck::rule::{self, Group, Units};
use super::dtcheck::solve::{Atom, Bounds, Problem, Prop, Why};
use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use super::saturate::{Concl, Contexts, Dynamic, Sink, State};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

/// A role of a checker component.
pub struct RoleIx {
    pub comp: u32,
    /// the roles above it in its component, itself aside
    pub supers: Vec<Role>,
    /// its declared ranges
    pub ranges: Vec<Rid>,
    pub functional: bool,
}

/// A body atom: its expression, its key and its told subsumers when every negative
/// occurrence is a told subsumption, `None` otherwise.
pub struct Body {
    pub e: Ex,
    pub told: Option<Box<[Ex]>>,
}

type Key = (u32, Box<[Ex]>, Box<[(Ex, Group)]>);

/// What the rule reads of a run with checker components.
pub struct DtIx {
    /// the keys of the interner's atoms, by atom id
    pub keys: Vec<DtKey>,
    pub roles: FxHashMap<Role, RoleIx>,
    /// the body atoms of each component
    pub body: Vec<Vec<Body>>,
    /// pairs of disjoint roles
    pub disjoint: Vec<(Role, Role)>,
    ranges: Mutex<Ranges>,
    cache: Mutex<FxHashMap<Key, Arc<Result<Units, Why>>>>,
    pub bounds: Bounds,
    /// the run's components
    pub comps: Comps,
    /// for each told subsumer of a body atom, the components it re-evaluates
    refire: FxHashMap<Ex, Vec<u32>>,
    /// the worst last evaluation of each component in the states of worlds, which go
    /// when their world does
    sticky: Mutex<Vec<u8>>,
}

/// A context's last evaluation of a component.
pub const FINE: u8 = 0;
pub const COVER: u8 = 1;
pub const BOUND: u8 = 2;
pub const UNCOUNTED: u8 = 3;
pub const STOPPED: u8 = 4;

/// Why a component's axioms are used in part, by its worst last evaluation.
pub fn reason(why: u8) -> &'static str {
    match why {
        COVER => "a case analysis over data values, which the Horn tier does not make",
        BOUND => "a datatype check past the checker's bound on types or search nodes",
        UNCOUNTED => "a datatype check that turns on a value space the checker does not count",
        _ => "a datatype check that the run stopped",
    }
}

/// A context's data atoms of one component and its last evaluation.
#[derive(Clone, Default)]
pub struct DtSt {
    pub comp: u32,
    pub held: Vec<Ex>,
    pub last: u8,
}

impl DtSt {
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<DtSt>() + self.held.capacity() * 4
    }
}

impl DtIx {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        keys: Vec<DtKey>,
        roles: FxHashMap<Role, RoleIx>,
        body: Vec<Vec<Body>>,
        disjoint: Vec<(Role, Role)>,
        ranges: Ranges,
        comps: Comps,
        refire: FxHashMap<Ex, Vec<u32>>,
    ) -> DtIx {
        let n = comps.len();
        DtIx {
            comps,
            refire,
            sticky: Mutex::new(vec![FINE; n]),
            keys,
            roles,
            body,
            disjoint,
            ranges: Mutex::new(ranges),
            cache: Mutex::new(FxHashMap::default()),
            bounds: Bounds::default(),
        }
    }

    /// Empties the cache, so that a test's next evaluation solves again.
    #[cfg(test)]
    pub(super) fn forget(&self) {
        self.cache.lock().clear();
    }

    /// The bytes the tables hold, the cache and the ranges with them. The saturation
    /// charges the growth of the cache and the ranges as it makes it, and the estimates
    /// that read this, the base before the saturation and the retained state after it,
    /// are built from the tables alone, so nothing is counted twice.
    pub fn bytes(&self) -> usize {
        let key = std::mem::size_of::<DtKey>();
        self.keys.capacity() * key
            + self
                .roles
                .values()
                .map(|r| {
                    std::mem::size_of::<RoleIx>()
                        + 4
                        + (r.supers.capacity() + r.ranges.capacity()) * 4
                })
                .sum::<usize>()
            + self
                .body
                .iter()
                .flatten()
                .map(|b| std::mem::size_of::<Body>() + b.told.as_ref().map_or(0, |t| t.len() * 4))
                .sum::<usize>()
            + self.disjoint.capacity() * 8
            + self
                .refire
                .values()
                .map(|v| 16 + v.capacity() * 4)
                .sum::<usize>()
            + self.comps.bytes()
            + self.ranges.lock().bytes()
            + self
                .cache
                .lock()
                .iter()
                .map(|(k, v)| entry_bytes(k, v))
                .sum::<usize>()
    }

    fn atom(&self, a: DtKey, at: &FxHashMap<Role, usize>) -> Atom {
        let i = at[&a.role];
        if a.at_least {
            Atom::AtLeast(i, a.n, a.range)
        } else {
            Atom::AtMost(i, a.n, a.range)
        }
    }
}

fn answer_bytes(a: &Result<Units, Why>) -> usize {
    let base = std::mem::size_of::<Result<Units, Why>>() + 16;
    match a {
        Ok(u) => {
            base + u.entailed.capacity() * std::mem::size_of::<usize>()
                + u.classes.capacity() * std::mem::size_of::<u32>()
        }
        Err(_) => base,
    }
}

pub(super) fn entry_bytes(k: &Key, a: &Result<Units, Why>) -> usize {
    std::mem::size_of::<Key>()
        + k.1.len() * 4
        + k.2.len() * std::mem::size_of::<(Ex, Group)>()
        + std::mem::size_of::<Arc<Result<Units, Why>>>()
        + answer_bytes(a)
        + 64
}

/// A new data atom `d`, atom `id` of the interner, in the context: the rule on the
/// context's atoms of its component.
pub fn fire<S: Sink>(ix: &Index, st: &mut State, d: Ex, id: u32, out: &mut S) {
    let Some(dt) = ix.dt.as_deref() else {
        return;
    };
    let Some(comp) = dt
        .keys
        .get(id as usize)
        .and_then(|k| dt.roles.get(&k.role))
        .map(|r| r.comp)
    else {
        return;
    };
    let before = st.bytes();
    let x = st.extra();
    let at = match x.dt.iter().position(|s| s.comp == comp) {
        Some(at) => at,
        None => {
            x.dt.push(DtSt {
                comp,
                ..DtSt::default()
            });
            x.dt.len() - 1
        }
    };
    let held = &mut x.dt[at].held;
    if let Err(i) = held.binary_search(&d) {
        held.insert(i, d);
    }
    let grown = st.bytes().saturating_sub(before);
    out.keep_data(grown);
    evaluate(ix, dt, st, at, out);
}

/// A told subsumer `t` of body atoms in the context: the rule again on each component
/// of those atoms that the context holds atoms of, since their groups changed.
pub fn refire<S: Sink>(ix: &Index, st: &mut State, t: Ex, out: &mut S) {
    let Some(dt) = ix.dt.as_deref() else {
        return;
    };
    let Some(comps) = dt.refire.get(&t) else {
        return;
    };
    for &c in comps {
        let at = st
            .extra
            .as_ref()
            .and_then(|x| x.dt.iter().position(|s| s.comp == c));
        if let Some(at) = at {
            evaluate(ix, dt, st, at, out);
        }
    }
}

/// Inserts `a` under `key` once, admitting its full entry size first and transferring
/// the reservation atomically to shared storage only when this call inserts a new entry.
fn publish<S: Sink>(
    dt: &DtIx,
    key: &Key,
    a: Result<Units, Why>,
    out: &mut S,
) -> Arc<Result<Units, Why>> {
    if a == Err(Why::Stopped) {
        return Arc::new(a);
    }
    if let Some(existing) = dt.cache.lock().get(key).cloned() {
        return existing;
    }
    publish_after_miss(dt, key, a, out)
}

fn publish_after_miss<S: Sink>(
    dt: &DtIx,
    key: &Key,
    a: Result<Units, Why>,
    out: &mut S,
) -> Arc<Result<Units, Why>> {
    let bytes = entry_bytes(key, &a);
    if !out.admit_data(bytes) {
        out.release_data(bytes);
        return Arc::new(Err(Why::Stopped));
    }
    let mut cache = dt.cache.lock();
    match cache.entry(key.clone()) {
        Entry::Occupied(e) => {
            let existing = e.get().clone();
            drop(cache);
            out.release_data(bytes);
            existing
        }
        Entry::Vacant(e) => {
            let arc = Arc::new(a);
            e.insert(arc.clone());
            drop(cache);
            out.retain_shared(bytes);
            arc
        }
    }
}

/// The rule on the context's atoms of the component at `at` in its checker state.
fn evaluate<S: Sink>(ix: &Index, dt: &DtIx, st: &mut State, at: usize, out: &mut S) {
    let Some(x) = st.extra.as_ref() else {
        return;
    };
    let s = &x.dt[at];
    let comp = s.comp;
    let held: Box<[Ex]> = s.held.as_slice().into();
    let body: Box<[(Ex, Group)]> = dt.body[comp as usize]
        .iter()
        .filter(|b| !st.subs.contains(&b.e))
        .map(|b| (b.e, group(st, b)))
        .collect();
    let key: Key = (comp, held, body);
    let cached = dt.cache.lock().get(&key).cloned();
    let answer = match cached {
        Some(a) => a,
        None => {
            let a = solve(ix, dt, &key, out);
            publish(dt, &key, a, out)
        }
    };
    let last = match answer.as_ref() {
        Ok(u) => {
            if u.conflict {
                out.me(Concl::Sub(BOTTOM));
            }
            for &i in &u.entailed {
                out.me(Concl::Sub(key.2[i].0));
            }
            for &c in &u.classes {
                out.me(Concl::Sub(c));
            }
            if u.cover { COVER } else { FINE }
        }
        Err(Why::Bound) => BOUND,
        Err(Why::Uncounted) => UNCOUNTED,
        // the run's check that answered the stop has published it
        Err(Why::Stopped) => STOPPED,
    };
    if let Some(x) = st.extra.as_mut() {
        x.dt[at].last = last;
    }
}

/// The group of a body atom that the context does not hold.
fn group(st: &State, b: &Body) -> Group {
    let Some(told) = &b.told else {
        return Group::Other;
    };
    let mut left = told
        .iter()
        .copied()
        .filter(|&t| t != TOP && !st.subs.contains(&t));
    match (left.next(), left.next()) {
        (None, _) => Group::Free,
        (Some(t), None) => Group::Only(t),
        _ => Group::Other,
    }
}

/// The rule's answer for a key, with the problem restricted to the roles of its atoms
/// and the roles above them, since the bound on properties counts every role it holds.
fn solve<S: Sink>(ix: &Index, dt: &DtIx, key: &Key, out: &mut S) -> Result<Units, Why> {
    let (_, held, body) = key;
    let reserve = |out: &mut S, n: usize| -> bool {
        if n == 0 {
            return true;
        }
        if out.admit_data(n) {
            true
        } else {
            out.release_data(n);
            false
        }
    };
    let mut setup_reserved = 0usize;
    let mut roles: Vec<Role> = Vec::new();
    for &e in held.iter().chain(body.iter().map(|(e, _)| e)) {
        if let Some(k) = key_of(ix, dt, e)
            && !roles.contains(&k.role)
        {
            let supers_len = dt.roles.get(&k.role).map_or(0, |ri| ri.supers.len());
            let step_est = (1 + supers_len).saturating_mul(std::mem::size_of::<Role>() + 4);
            if !reserve(out, step_est) {
                out.release_data(setup_reserved);
                return Err(Why::Stopped);
            }
            setup_reserved = setup_reserved.saturating_add(step_est);
            roles.push(k.role);
            if let Some(ri) = dt.roles.get(&k.role) {
                for &s in &ri.supers {
                    if !roles.contains(&s) {
                        roles.push(s);
                    }
                }
            }
        }
    }
    let props_bytes: usize = roles
        .iter()
        .map(|r| {
            dt.roles.get(r).map_or(std::mem::size_of::<Prop>(), |ri| {
                std::mem::size_of::<Prop>()
                    + ri.supers.len() * std::mem::size_of::<usize>()
                    + ri.ranges.len() * 4
            })
        })
        .sum();
    let build_est = roles
        .len()
        .saturating_mul(std::mem::size_of::<(Role, usize)>() + 16)
        .saturating_add(props_bytes)
        .saturating_add(dt.disjoint.len().saturating_mul(16))
        .saturating_add(
            held.len()
                .saturating_add(roles.len())
                .saturating_mul(std::mem::size_of::<Atom>()),
        )
        .saturating_add(
            body.len()
                .saturating_mul(std::mem::size_of::<(Atom, Group)>()),
        )
        .saturating_add(128);
    if !reserve(out, build_est) {
        out.release_data(setup_reserved);
        return Err(Why::Stopped);
    }
    setup_reserved = setup_reserved.saturating_add(build_est);
    roles.sort_unstable();
    let at: FxHashMap<Role, usize> = roles.iter().enumerate().map(|(i, &r)| (r, i)).collect();
    let mut p = Problem::default();
    for &r in &roles {
        let ri = dt.roles.get(&r);
        p.props.push(Prop {
            supers: ri.map_or(Vec::new(), |ri| {
                ri.supers
                    .iter()
                    .filter_map(|s| at.get(s).copied())
                    .collect()
            }),
            ranges: ri.map_or(Vec::new(), |ri| ri.ranges.clone()),
        });
        if ri.is_some_and(|ri| ri.functional) {
            p.atoms.push(Atom::AtMost(at[&r], 1, Ranges::FULL));
        }
    }
    for &(a, b) in &dt.disjoint {
        if let (Some(&i), Some(&j)) = (at.get(&a), at.get(&b)) {
            p.disjoint.push((i, j));
        }
    }
    for &e in held.iter() {
        if let Some(k) = key_of(ix, dt, e) {
            p.atoms.push(dt.atom(k, &at));
        }
    }
    let tests: Vec<(Atom, Group)> = body
        .iter()
        .filter_map(|&(e, g)| key_of(ix, dt, e).map(|k| (dt.atom(k, &at), g)))
        .collect();
    debug_assert_eq!(tests.len(), body.len());
    let mut ranges = dt.ranges.lock();
    let before = ranges.bytes();
    // the run's own check, which publishes the stop it sees with its reason
    let out = std::cell::RefCell::new(out);
    let ranges_reserved = std::cell::Cell::new(0usize);
    let stop = || !out.borrow_mut().reserve_data(0);
    let admit_ranges = |bytes: usize| {
        if bytes == 0 {
            return true;
        }
        let mut s = out.borrow_mut();
        if s.admit_data(bytes) {
            ranges_reserved.set(ranges_reserved.get().saturating_add(bytes));
            true
        } else {
            s.release_data(bytes);
            false
        }
    };
    let temp_mem = |delta: isize| {
        let mut s = out.borrow_mut();
        if delta > 0 {
            let n = delta as usize;
            if s.admit_data(n) {
                true
            } else {
                s.release_data(n);
                false
            }
        } else if delta < 0 {
            s.release_data((-delta) as usize);
            true
        } else {
            true
        }
    };
    let a = rule::units_bounded(
        &mut ranges,
        &p,
        &tests,
        &dt.bounds,
        &stop,
        &admit_ranges,
        &temp_mem,
    );
    let out = out.into_inner();
    let grown = ranges.bytes().saturating_sub(before);
    drop(ranges);
    let rr = ranges_reserved.get();
    if grown <= rr {
        if grown > 0 {
            out.retain_shared(grown);
        }
        if rr > grown {
            out.release_data(rr - grown);
        }
    } else {
        if rr > 0 {
            out.retain_shared(rr);
        }
        out.keep_shared(grown - rr);
    }
    out.release_data(setup_reserved);
    a
}

fn key_of(ix: &Index, dt: &DtIx, e: Ex) -> Option<DtKey> {
    match ix.head(e).node {
        Node::DtAtom(id) => dt.keys.get(id as usize).copied(),
        _ => None,
    }
}

/// A world's last evaluations, read once its fixpoint is reached, from the states that
/// it copied. A world whose class has no instance leaves nothing out, and neither does
/// an unsatisfiable state, so that what a world reports does not depend on the order in
/// which it reached its fixpoint.
pub fn world<'s>(ix: &Index, states: impl Iterator<Item = &'s State>) {
    let Some(dt) = ix.dt.as_deref() else {
        return;
    };
    let mut sticky = None;
    for st in states.filter(|st| !st.bottom) {
        let Some(x) = &st.extra else {
            continue;
        };
        for s in x.dt.iter().filter(|s| s.last != FINE) {
            let w = sticky.get_or_insert_with(|| dt.sticky.lock());
            let w = &mut w[s.comp as usize];
            *w = (*w).max(s.last);
        }
    }
}

/// The worst last evaluation of each component among the contexts.
pub fn collect(ix: &Index, ctx: &Contexts, dynamic: Option<&Dynamic>, worst: &mut Vec<u8>) {
    let Some(dt) = ix.dt.as_deref() else {
        return;
    };
    worst.resize(dt.body.len().max(worst.len()), FINE);
    for (w, &s) in worst.iter_mut().zip(dt.sticky.lock().iter()) {
        *w = (*w).max(s);
    }
    // an unsatisfiable context has every subsumer, so an evaluation it made before its
    // conflict leaves nothing out
    let mut read = |st: &State| {
        if st.bottom {
            return;
        }
        if let Some(x) = &st.extra {
            for s in &x.dt {
                let w = &mut worst[s.comp as usize];
                *w = (*w).max(s.last);
            }
        }
    };
    for c in ctx.iter().filter_map(|c| c.get()) {
        read(&c.st.lock());
    }
    if let Some(d) = dynamic {
        d.each(|c| read(&c.st.lock()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountSink<'a> {
        shared: &'a AtomicUsize,
        reserved: usize,
    }

    impl Sink for CountSink<'_> {
        fn reserve_data(&mut self, bytes: usize) -> bool {
            self.reserved += bytes;
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.reserved = self.reserved.saturating_sub(bytes);
        }
        fn keep_shared(&mut self, bytes: usize) {
            self.shared.fetch_add(bytes, Ordering::Relaxed);
        }
        fn retain_shared(&mut self, bytes: usize) {
            self.reserved = self.reserved.saturating_sub(bytes);
            self.shared.fetch_add(bytes, Ordering::Relaxed);
        }
        fn send(&mut self, _to: Ex, _c: Concl) {}
        fn me(&mut self, _c: Concl) {}
    }

    #[test]
    fn concurrent_cache_publish_charges_shared_once_and_counts_answer_payload() {
        let dt = DtIx::new(
            Vec::new(),
            FxHashMap::default(),
            vec![Vec::new()],
            Vec::new(),
            Ranges::new(),
            Comps::default(),
            FxHashMap::default(),
        );
        let key: Key = (0, vec![1u32, 2].into_boxed_slice(), Box::new([]));
        let make_ans = || {
            Ok(Units {
                conflict: false,
                entailed: (0..256).collect(),
                classes: vec![10, 20, 30],
                cover: false,
            })
        };
        let expected = entry_bytes(&key, &make_ans());
        let empty_ans = entry_bytes(&key, &Ok(Units::default()));
        assert!(expected >= empty_ans + 256 * std::mem::size_of::<usize>());

        let total_shared = AtomicUsize::new(0);
        let barrier = Barrier::new(2);
        // Force both threads past the initial miss into `publish_after_miss` so both
        // reserve and contend on the insertion lock.
        std::thread::scope(|s| {
            for _ in 0..2 {
                s.spawn(|| {
                    let mut sink = CountSink {
                        shared: &total_shared,
                        reserved: 0,
                    };
                    let ans = make_ans();
                    barrier.wait();
                    let _ = publish_after_miss(&dt, &key, ans, &mut sink);
                    assert_eq!(sink.reserved, 0);
                });
            }
        });
        assert_eq!(total_shared.load(Ordering::Relaxed), expected);
        assert!(dt.bytes() >= expected);
    }

    #[test]
    fn saturation_solve_refuses_before_growing_ranges_past_budget() {
        // Real `Out` and `WorldSink` charge `bytes` in `reserve_data` even when returning
        // false, relying on the caller to reconcile with `release_data`. Mirror that here.
        struct CapSink {
            cap: usize,
            held: isize,
            peak: usize,
            shared: usize,
        }
        impl Sink for CapSink {
            fn reserve_data(&mut self, bytes: usize) -> bool {
                self.held += bytes as isize;
                let ok = self.held as usize <= self.cap;
                if ok {
                    self.peak = self.peak.max(self.held as usize);
                }
                ok
            }
            fn release_data(&mut self, bytes: usize) {
                self.held -= bytes as isize;
            }
            fn keep_shared(&mut self, bytes: usize) {
                self.shared = self.shared.saturating_add(bytes);
            }
            fn retain_shared(&mut self, bytes: usize) {
                self.held -= bytes as isize;
                self.shared = self.shared.saturating_add(bytes);
            }
            fn send(&mut self, _to: Ex, _c: Concl) {}
            fn me(&mut self, _c: Concl) {}
        }

        let mut ranges = Ranges::new();
        let lits: Vec<_> = (0..20)
            .map(|i| crate::model::Literal::typed(format!("{i:0>100}"), crate::vocab::XSD_STRING))
            .collect();
        let r1 = ranges
            .of(&crate::model::DataRange::OneOf(
                lits[..10].iter().cloned().collect(),
            ))
            .unwrap();
        let r2 = ranges
            .of(&crate::model::DataRange::OneOf(
                lits[5..15].iter().cloned().collect(),
            ))
            .unwrap();
        let r3 = ranges
            .of(&crate::model::DataRange::OneOf(
                lits[10..].iter().cloned().collect(),
            ))
            .unwrap();
        let before_bytes = ranges.bytes();
        let p = Problem {
            props: vec![Prop {
                supers: Vec::new(),
                ranges: vec![r1],
            }],
            disjoint: Vec::new(),
            atoms: vec![Atom::AtLeast(0, 1, r2), Atom::AtLeast(0, 1, r3)],
        };
        let mut sink = CapSink {
            cap: 512,
            held: 0,
            peak: 0,
            shared: 0,
        };
        let sink_cell = std::cell::RefCell::new(&mut sink);
        let ranges_reserved = std::cell::Cell::new(0usize);
        let stop = || !sink_cell.borrow_mut().reserve_data(0);
        let admit_ranges = |bytes: usize| {
            if bytes == 0 {
                return true;
            }
            let mut s = sink_cell.borrow_mut();
            if s.reserve_data(bytes) {
                ranges_reserved.set(ranges_reserved.get().saturating_add(bytes));
                true
            } else {
                s.release_data(bytes);
                false
            }
        };
        let temp_mem = |delta: isize| {
            let mut s = sink_cell.borrow_mut();
            if delta > 0 {
                let n = delta as usize;
                if s.reserve_data(n) {
                    true
                } else {
                    s.release_data(n);
                    false
                }
            } else if delta < 0 {
                s.release_data((-delta) as usize);
                true
            } else {
                true
            }
        };
        let ans = rule::units_bounded(
            &mut ranges,
            &p,
            &[],
            &Bounds::default(),
            &stop,
            &admit_ranges,
            &temp_mem,
        );
        let sink = sink_cell.into_inner();
        sink.release_data(ranges_reserved.get());
        assert_eq!(ans, Err(Why::Stopped));
        assert!(sink.peak <= sink.cap);
        assert_eq!(sink.held, 0);
        // Ranges did not grow past the cap because admission refused before interning.
        assert_eq!(ranges.bytes(), before_bytes);
    }
}
