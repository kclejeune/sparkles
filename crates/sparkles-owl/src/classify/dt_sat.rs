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
    cache: Mutex<FxHashMap<Key, Result<Units, Why>>>,
    pub bounds: Bounds,
    /// the run's components
    pub comps: Comps,
    /// for each told subsumer of a body atom, the components it re-evaluates
    refire: FxHashMap<Ex, Vec<u32>>,
    /// the worst evaluation of each component in states that the run does not keep,
    /// such as those of worlds, which no last evaluation stands for
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

    /// The bytes the tables hold, the cache and the ranges with them.
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
            + self.ranges.lock().bytes()
            + self.cache.lock().keys().map(key_bytes).sum::<usize>()
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

fn key_bytes(k: &Key) -> usize {
    std::mem::size_of::<Key>() + k.1.len() * 4 + k.2.len() * std::mem::size_of::<(Ex, Group)>() + 64
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
            if a != Err(Why::Stopped) {
                let bytes = key_bytes(&key);
                dt.cache.lock().insert(key.clone(), a.clone());
                out.keep_data(bytes);
            }
            a
        }
    };
    let last = match answer {
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
    if last != FINE && !out.keeps_states() {
        let mut s = dt.sticky.lock();
        s[comp as usize] = s[comp as usize].max(last);
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
    let mut roles: Vec<Role> = Vec::new();
    let add = |r: Role, roles: &mut Vec<Role>| {
        if !roles.contains(&r) {
            roles.push(r);
            if let Some(ri) = dt.roles.get(&r) {
                for &s in &ri.supers {
                    if !roles.contains(&s) {
                        roles.push(s);
                    }
                }
            }
        }
    };
    for &e in held.iter().chain(body.iter().map(|(e, _)| e)) {
        if let Some(k) = key_of(ix, dt, e) {
            add(k.role, &mut roles);
        }
    }
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
    let a = rule::units(&mut ranges, &p, &tests, &dt.bounds, &|| {
        !out.borrow_mut().reserve_data(0)
    });
    let out = out.into_inner();
    let grown = ranges.bytes().saturating_sub(before);
    drop(ranges);
    out.keep_data(grown);
    a
}

fn key_of(ix: &Index, dt: &DtIx, e: Ex) -> Option<DtKey> {
    match ix.head(e).node {
        Node::DtAtom(id) => dt.keys.get(id as usize).copied(),
        _ => None,
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
    let mut read = |st: &State| {
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
