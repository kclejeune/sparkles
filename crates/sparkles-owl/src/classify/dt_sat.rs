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
use super::dtcheck::range::{Ranges, Rid, map_backing_bytes, map_buckets_for, map_growth};
use super::dtcheck::rule::{self, Group, Units};
use super::dtcheck::solve::{Atom, Bounds, Problem, Prop, Why};
use super::index::{BOTTOM, Ex, Index, Node, Role, TOP};
use super::saturate::{Concl, Contexts, Dynamic, Sink, State};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
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

/// Component alignment between an earlier [`DtIx`] and the current one for incremental
/// maintenance (roadmap item 753).
pub struct DtAlign {
    /// For each old component id `0..old.body.len()`, its component id in `self`, or
    /// `None` if the component changed, split, merged, or disappeared.
    pub comp_map: Vec<Option<u32>>,
    /// For each old component id `0..old.body.len()`, whether the component changed
    /// (`changed_comps[c] == comp_map[c].is_none()`).
    pub changed_comps: Vec<bool>,
}

/// Outcome of aligning a new [`DtIx`] against the previous run's [`DtIx`].
pub enum DtAlignOutcome {
    Aligned(DtAlign),
    /// A data property switched between EL-lattice and checker modes, or checker bounds
    /// changed; the caller must fall back to a fresh full rebuild.
    Incompatible,
    /// Cooperative cancellation or deadline stop triggered during alignment.
    Stopped,
}

/// Items processed between cooperative stop polls inside [`DtIx::align`].
const POLL_ALIGN: usize = 4096;

#[inline]
fn tick_align(ticks: &mut usize, n: usize, stop: &dyn Fn() -> bool) -> bool {
    *ticks += n;
    if *ticks >= POLL_ALIGN {
        *ticks = 0;
        stop()
    } else {
        false
    }
}

fn slice_eq_poll<T: PartialEq>(
    a: &[T],
    b: &[T],
    ticks: &mut usize,
    stop: &dyn Fn() -> bool,
) -> Option<bool> {
    if a.len() != b.len() {
        return Some(false);
    }
    let mut i = 0;
    while i < a.len() {
        let step = POLL_ALIGN.saturating_sub(*ticks).max(1);
        let end = (i + step).min(a.len());
        if a[i..end] != b[i..end] {
            return Some(false);
        }
        *ticks += end - i;
        i = end;
        if *ticks >= POLL_ALIGN {
            *ticks = 0;
            if stop() {
                return None;
            }
        }
    }
    Some(true)
}

impl DtIx {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        keys: Vec<DtKey>,
        mut roles: FxHashMap<Role, RoleIx>,
        body: Vec<Vec<Body>>,
        disjoint: Vec<(Role, Role)>,
        ranges: Ranges,
        comps: Comps,
        refire: FxHashMap<Ex, Vec<u32>>,
    ) -> DtIx {
        for ri in roles.values_mut() {
            ri.ranges.sort_unstable();
            ri.ranges.dedup();
        }
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

    /// Number of cached evaluations currently stored for checker component `comp`.
    #[cfg(test)]
    pub(super) fn cache_entries_for(&self, comp: u32) -> usize {
        self.cache.lock().keys().filter(|k| k.0 == comp).count()
    }

    /// The bytes the tables hold, the cache and the ranges with them. The saturation
    /// charges the growth of the cache and the ranges as it makes it, and the estimates
    /// that read this, the base before the saturation and the retained state after it,
    /// are built from the tables alone, so nothing is counted twice.
    pub fn bytes(&self) -> usize {
        let cache_bytes = {
            let cache = self.cache.lock();
            map_backing_bytes(&cache).saturating_add(
                cache
                    .iter()
                    .map(|(k, v)| entry_payload_bytes(k, v))
                    .sum::<usize>(),
            )
        };
        std::mem::size_of::<DtIx>()
            .saturating_add(
                self.keys
                    .capacity()
                    .saturating_mul(std::mem::size_of::<DtKey>()),
            )
            .saturating_add(map_backing_bytes(&self.roles))
            .saturating_add(
                self.roles
                    .values()
                    .map(|r| {
                        r.supers
                            .capacity()
                            .saturating_mul(std::mem::size_of::<Role>())
                            .saturating_add(
                                r.ranges
                                    .capacity()
                                    .saturating_mul(std::mem::size_of::<Rid>()),
                            )
                    })
                    .sum::<usize>(),
            )
            .saturating_add(
                self.body
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Vec<Body>>()),
            )
            .saturating_add(
                self.body
                    .iter()
                    .map(|v| {
                        v.capacity()
                            .saturating_mul(std::mem::size_of::<Body>())
                            .saturating_add(
                                v.iter()
                                    .map(|b| {
                                        b.told
                                            .as_ref()
                                            .map_or(0, |t| t.len() * std::mem::size_of::<Ex>())
                                    })
                                    .sum::<usize>(),
                            )
                    })
                    .sum::<usize>(),
            )
            .saturating_add(
                self.disjoint
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(Role, Role)>()),
            )
            .saturating_add(map_backing_bytes(&self.refire))
            .saturating_add(
                self.refire
                    .values()
                    .map(|v| v.capacity().saturating_mul(std::mem::size_of::<u32>()))
                    .sum::<usize>(),
            )
            .saturating_add(self.sticky.lock().capacity())
            .saturating_add(self.comps.bytes())
            .saturating_add(self.ranges.lock().bytes())
            .saturating_add(cache_bytes)
    }

    pub fn logical_bytes(&self) -> usize {
        std::mem::size_of::<DtIx>()
            .saturating_add(self.keys.len().saturating_mul(std::mem::size_of::<DtKey>()))
            .saturating_add(
                self.roles
                    .len()
                    .saturating_mul(std::mem::size_of::<(Role, RoleIx)>()),
            )
            .saturating_add(
                self.roles
                    .values()
                    .map(|r| {
                        r.supers
                            .len()
                            .saturating_mul(std::mem::size_of::<Role>())
                            .saturating_add(
                                r.ranges.len().saturating_mul(std::mem::size_of::<Rid>()),
                            )
                    })
                    .sum::<usize>(),
            )
            .saturating_add(
                self.body
                    .len()
                    .saturating_mul(std::mem::size_of::<Vec<Body>>()),
            )
            .saturating_add(
                self.body
                    .iter()
                    .map(|v| {
                        v.len()
                            .saturating_mul(std::mem::size_of::<Body>())
                            .saturating_add(
                                v.iter()
                                    .map(|b| {
                                        b.told
                                            .as_ref()
                                            .map_or(0, |t| t.len() * std::mem::size_of::<Ex>())
                                    })
                                    .sum::<usize>(),
                            )
                    })
                    .sum::<usize>(),
            )
            .saturating_add(
                self.disjoint
                    .len()
                    .saturating_mul(std::mem::size_of::<(Role, Role)>()),
            )
            .saturating_add(
                self.refire
                    .len()
                    .saturating_mul(std::mem::size_of::<(u32, Vec<u32>)>()),
            )
            .saturating_add(
                self.refire
                    .values()
                    .map(|v| v.len().saturating_mul(std::mem::size_of::<u32>()))
                    .sum::<usize>(),
            )
            .saturating_add(self.sticky.lock().len())
    }

    fn atom(&self, a: DtKey, at: &FxHashMap<Role, usize>) -> Atom {
        let i = at[&a.role];
        if a.at_least {
            Atom::AtLeast(i, a.n, a.range)
        } else {
            Atom::AtMost(i, a.n, a.range)
        }
    }

    /// Components re-evaluated when a context derives `t`.
    pub fn refire_of(&self, t: Ex) -> Option<&[u32]> {
        self.refire.get(&t).map(Vec::as_slice)
    }

    /// Zero-allocation compatibility check: whether checker bounds match and no existing
    /// data property switched between the EL data lattice and the datatype checker.
    #[allow(dead_code)]
    pub fn modes_compatible(&self, old: &DtIx, old_roles_len: usize) -> bool {
        self.modes_compatible_with_stop(old, old_roles_len, &|| false)
            .unwrap_or(false)
    }

    /// Cooperative polling variant of [`DtIx::modes_compatible`]; returns `None` when
    /// `stop` signals cancellation or deadline expiry.
    pub fn modes_compatible_with_stop(
        &self,
        old: &DtIx,
        old_roles_len: usize,
        stop: &dyn Fn() -> bool,
    ) -> Option<bool> {
        if (self.bounds.props, self.bounds.types, self.bounds.nodes)
            != (old.bounds.props, old.bounds.types, old.bounds.nodes)
        {
            return Some(false);
        }
        let mut ticks = 0usize;
        let mut poll = || -> bool {
            ticks += 1;
            if ticks >= POLL_ALIGN {
                ticks = 0;
                stop()
            } else {
                false
            }
        };
        // Any role already interned in `old` that is now a checker role must already have
        // been a checker role in `old`; otherwise it switched from EL lattice (or was
        // previously removed and re-entered as a checker role).
        for &r in self.roles.keys() {
            if poll() {
                return None;
            }
            if (r as usize) < old_roles_len && !old.roles.contains_key(&r) {
                return Some(false);
            }
        }
        // Any data property present in both runs' component maps must keep the same
        // EL-lattice vs checker assignment.
        for (d, old_c) in old.comps.iter() {
            if poll() {
                return None;
            }
            if let Some(new_c) = self.comps.component(d)
                && old.comps.is_checker(old_c) != self.comps.is_checker(new_c)
            {
                return Some(false);
            }
        }
        Some(true)
    }

    /// Upper-bound scratch bytes allocated by [`DtIx::align`] and `refire` comparison.
    pub fn align_scratch(&self, old: &DtIx) -> usize {
        let old_comps = old.body.len();
        let new_comps = self.body.len();
        let groups = old_comps
            .saturating_mul(std::mem::size_of::<Vec<Role>>())
            .saturating_add(
                old.roles
                    .len()
                    .saturating_mul(std::mem::size_of::<Role>() * 4),
            );
        let counts = new_comps.saturating_mul(std::mem::size_of::<usize>());
        let disj = (old_comps + new_comps)
            .saturating_mul(std::mem::size_of::<Vec<(Role, Role)>>())
            .saturating_add(
                (old.disjoint.len() + self.disjoint.len())
                    .saturating_mul(std::mem::size_of::<(Role, Role)>() * 8),
            );
        let out = old_comps.saturating_mul(std::mem::size_of::<Option<u32>>() + 1);
        groups
            .saturating_add(counts)
            .saturating_add(disj)
            .saturating_add(out)
            .saturating_add(64)
    }

    /// Aligns components of `old` against `self` for incremental maintenance.
    ///
    /// An old checker component `old_c` maps to `Some(new_c)` iff `new_c` has the exact
    /// same role set, per-role superroles/ranges/functionality, touching disjointness
    /// pairs, and body atoms (with identical `told` subsumer lists).
    pub fn align(
        &self,
        old: &DtIx,
        old_roles_len: usize,
        stop: &dyn Fn() -> bool,
    ) -> DtAlignOutcome {
        match self.modes_compatible_with_stop(old, old_roles_len, stop) {
            Some(true) => {}
            Some(false) => return DtAlignOutcome::Incompatible,
            None => return DtAlignOutcome::Stopped,
        }
        let mut ticks = 0usize;

        let mut old_groups: Vec<Vec<Role>> = vec![Vec::new(); old.body.len()];
        for (&r, ri) in &old.roles {
            if tick_align(&mut ticks, 1, stop) {
                return DtAlignOutcome::Stopped;
            }
            old_groups[ri.comp as usize].push(r);
        }

        let mut new_counts: Vec<usize> = vec![0; self.body.len()];
        for ri in self.roles.values() {
            if tick_align(&mut ticks, 1, stop) {
                return DtAlignOutcome::Stopped;
            }
            new_counts[ri.comp as usize] += 1;
        }

        let mut old_disj: Vec<Vec<(Role, Role)>> = vec![Vec::new(); old.body.len()];
        for &(a, b) in &old.disjoint {
            if tick_align(&mut ticks, 1, stop) {
                return DtAlignOutcome::Stopped;
            }
            if let Some(ca) = old.roles.get(&a).map(|r| r.comp) {
                old_disj[ca as usize].push((a, b));
                if let Some(cb) = old.roles.get(&b).map(|r| r.comp)
                    && cb != ca
                {
                    old_disj[cb as usize].push((a, b));
                }
            }
        }
        let mut new_disj: Vec<Vec<(Role, Role)>> = vec![Vec::new(); self.body.len()];
        for &(a, b) in &self.disjoint {
            if tick_align(&mut ticks, 1, stop) {
                return DtAlignOutcome::Stopped;
            }
            if let Some(ca) = self.roles.get(&a).map(|r| r.comp) {
                new_disj[ca as usize].push((a, b));
                if let Some(cb) = self.roles.get(&b).map(|r| r.comp)
                    && cb != ca
                {
                    new_disj[cb as usize].push((a, b));
                }
            }
        }

        let mut comp_map = vec![None; old.body.len()];
        let mut changed_comps = vec![true; old.body.len()];
        for old_c in 0..old.body.len() {
            if tick_align(&mut ticks, 1, stop) {
                return DtAlignOutcome::Stopped;
            }
            let roles = &old_groups[old_c];
            let Some(&first) = roles.first() else {
                continue;
            };
            let Some(new_c) = self.roles.get(&first).map(|ri| ri.comp) else {
                continue;
            };
            if new_counts[new_c as usize] != roles.len() {
                continue;
            }
            // `roles` holds the distinct keys of `old.roles` in `old_c`; verifying every
            // role maps to `new_c` alongside equal cardinality proves exact set equality
            // without sorting or hashing whole role lists.
            let mut same_members = true;
            for &r in roles {
                if tick_align(&mut ticks, 1, stop) {
                    return DtAlignOutcome::Stopped;
                }
                if self.roles.get(&r).map(|ri| ri.comp) != Some(new_c) {
                    same_members = false;
                    break;
                }
            }
            if !same_members {
                continue;
            }
            match slice_eq_poll(
                &old_disj[old_c],
                &new_disj[new_c as usize],
                &mut ticks,
                stop,
            ) {
                Some(true) => {}
                Some(false) => continue,
                None => return DtAlignOutcome::Stopped,
            }
            let old_body = &old.body[old_c];
            let new_body = &self.body[new_c as usize];
            if old_body.len() != new_body.len() {
                continue;
            }
            let mut body_same = true;
            for (a, b) in old_body.iter().zip(new_body) {
                if tick_align(&mut ticks, 1, stop) {
                    return DtAlignOutcome::Stopped;
                }
                if a.e != b.e {
                    body_same = false;
                    break;
                }
                let told_same = match (a.told.as_deref(), b.told.as_deref()) {
                    (None, None) => true,
                    (Some(ta), Some(tb)) => match slice_eq_poll(ta, tb, &mut ticks, stop) {
                        Some(eq) => eq,
                        None => return DtAlignOutcome::Stopped,
                    },
                    _ => false,
                };
                if !told_same {
                    body_same = false;
                    break;
                }
            }
            if !body_same {
                continue;
            }
            let mut roles_same = true;
            for &r in roles {
                if tick_align(&mut ticks, 1, stop) {
                    return DtAlignOutcome::Stopped;
                }
                let old_ri = &old.roles[&r];
                let new_ri = &self.roles[&r];
                if old_ri.functional != new_ri.functional {
                    roles_same = false;
                    break;
                }
                match slice_eq_poll(&old_ri.supers, &new_ri.supers, &mut ticks, stop) {
                    Some(true) => {}
                    Some(false) => {
                        roles_same = false;
                        break;
                    }
                    None => return DtAlignOutcome::Stopped,
                }
                match slice_eq_poll(&old_ri.ranges, &new_ri.ranges, &mut ticks, stop) {
                    Some(true) => {}
                    Some(false) => {
                        roles_same = false;
                        break;
                    }
                    None => return DtAlignOutcome::Stopped,
                }
            }
            if roles_same {
                comp_map[old_c] = Some(new_c);
                changed_comps[old_c] = false;
            }
        }
        if stop() {
            return DtAlignOutcome::Stopped;
        }
        DtAlignOutcome::Aligned(DtAlign {
            comp_map,
            changed_comps,
        })
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

pub(super) fn entry_payload_bytes(k: &Key, a: &Result<Units, Why>) -> usize {
    k.1.len() * std::mem::size_of::<Ex>()
        + k.2.len() * std::mem::size_of::<(Ex, Group)>()
        + answer_bytes(a)
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
    let mut cache = dt.cache.lock();
    if let Some(existing) = cache.get(key).cloned() {
        return existing;
    }
    let payload = entry_payload_bytes(key, &a);
    let table_growth = map_growth(&cache);
    let admit = payload.saturating_add(table_growth);
    if !out.admit_data(admit) {
        drop(cache);
        return Arc::new(Err(Why::Stopped));
    }
    let before_backing = map_backing_bytes(&cache);
    let arc = Arc::new(a);
    cache.insert(key.clone(), arc.clone());
    let added_backing = map_backing_bytes(&cache).saturating_sub(before_backing);
    drop(cache);
    let retained = payload.saturating_add(added_backing);
    if retained <= admit {
        out.retain_shared(retained);
        if admit > retained {
            out.release_data(admit - retained);
        }
    } else {
        out.retain_shared(admit);
        out.keep_shared(retained - admit);
    }
    arc
}

/// The rule on the context's atoms of the component at `at` in its checker state.
fn evaluate<S: Sink>(ix: &Index, dt: &DtIx, st: &mut State, at: usize, out: &mut S) {
    let Some(x) = st.extra.as_ref() else {
        return;
    };
    let s = &x.dt[at];
    let comp = s.comp;
    let body_cap = dt.body[comp as usize].len();
    let key_est = s
        .held
        .len()
        .saturating_mul(std::mem::size_of::<Ex>())
        .saturating_add(
            body_cap
                .saturating_mul(2)
                .saturating_mul(std::mem::size_of::<(Ex, Group)>()),
        )
        .saturating_add(32);
    if !out.admit_data(key_est) {
        if let Some(x) = st.extra.as_mut() {
            x.dt[at].last = STOPPED;
        }
        return;
    }
    let held: Box<[Ex]> = s.held.as_slice().into();
    let mut body_vec = Vec::with_capacity(body_cap);
    for b in &dt.body[comp as usize] {
        if !st.subs.contains(&b.e) {
            body_vec.push((b.e, group(st, b)));
        }
    }
    let body: Box<[(Ex, Group)]> = body_vec.into_boxed_slice();
    let key: Key = (comp, held, body);
    let cached = dt.cache.lock().get(&key).cloned();
    let answer = match cached {
        Some(a) => a,
        None => {
            let (a, ans_reserved) = solve(ix, dt, &key, out);
            let published = publish(dt, &key, a, out);
            out.release_data(ans_reserved);
            published
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
    drop(answer);
    drop(key);
    out.release_data(key_est);
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
/// Also returns the bytes reserved for the returned `Units` vectors, held until `publish`
/// transfers or drops the answer.
fn solve<S: Sink>(ix: &Index, dt: &DtIx, key: &Key, out: &mut S) -> (Result<Units, Why>, usize) {
    let (_, held, body) = key;
    let reserve = |out: &mut S, n| n == 0 || out.admit_data(n);
    let mut setup_reserved = 0usize;
    let mut roles: Vec<Role> = Vec::new();
    for &e in held.iter().chain(body.iter().map(|(e, _)| e)) {
        if let Some(k) = key_of(ix, dt, e)
            && !roles.contains(&k.role)
        {
            let supers_len = dt.roles.get(&k.role).map_or(0, |ri| ri.supers.len());
            let step_est = (1 + supers_len).saturating_mul(std::mem::size_of::<Role>() * 3);
            if !reserve(out, step_est) {
                out.release_data(setup_reserved);
                return (Err(Why::Stopped), 0);
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
    let at_backing = map_buckets_for(roles.len())
        .saturating_mul(std::mem::size_of::<(Role, usize)>() + 1)
        .saturating_add(16);
    let build_est = at_backing
        .saturating_add(props_bytes)
        .saturating_add(
            dt.disjoint
                .len()
                .saturating_mul(std::mem::size_of::<(usize, usize)>()),
        )
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
        return (Err(Why::Stopped), 0);
    }
    setup_reserved = setup_reserved.saturating_add(build_est);
    roles.sort_unstable();
    let mut at: FxHashMap<Role, usize> =
        FxHashMap::with_capacity_and_hasher(roles.len(), Default::default());
    for (i, &r) in roles.iter().enumerate() {
        at.insert(r, i);
    }
    let mut p = Problem {
        props: Vec::with_capacity(roles.len()),
        disjoint: Vec::with_capacity(dt.disjoint.len()),
        atoms: Vec::with_capacity(held.len().saturating_add(roles.len())),
    };
    for &r in &roles {
        let ri = dt.roles.get(&r);
        let supers = ri.map_or(Vec::new(), |ri| {
            let mut v = Vec::with_capacity(ri.supers.len());
            for s in &ri.supers {
                if let Some(&idx) = at.get(s) {
                    v.push(idx);
                }
            }
            v
        });
        p.props.push(Prop {
            supers,
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
    let mut tests: Vec<(Atom, Group)> = Vec::with_capacity(body.len());
    for &(e, g) in body.iter() {
        if let Some(k) = key_of(ix, dt, e) {
            tests.push((dt.atom(k, &at), g));
        }
    }
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
            false
        }
    };
    let temp_mem = |delta: isize| {
        let mut s = out.borrow_mut();
        if delta > 0 {
            s.admit_data(delta as usize)
        } else if delta < 0 {
            s.release_data((-delta) as usize);
            true
        } else {
            true
        }
    };
    let ans_reserved = std::cell::Cell::new(0usize);
    let admit_out = |bytes: usize| {
        if bytes == 0 {
            return true;
        }
        let mut s = out.borrow_mut();
        if s.admit_data(bytes) {
            ans_reserved.set(ans_reserved.get().saturating_add(bytes));
            true
        } else {
            false
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
        &admit_out,
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
    (a, ans_reserved.get())
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
        let before_bytes = dt.bytes();
        let payload = entry_payload_bytes(&key, &make_ans());
        let empty_payload = entry_payload_bytes(&key, &Ok(Units::default()));
        assert!(payload >= empty_payload + 256 * std::mem::size_of::<usize>());

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
        let added = dt.bytes() - before_bytes;
        assert_eq!(total_shared.load(Ordering::Relaxed), added);
        assert!(added >= payload);
    }

    #[test]
    fn saturation_solve_refuses_before_growing_ranges_past_budget() {
        struct CapSink {
            cap: usize,
            held: usize,
            peak: usize,
            shared: usize,
        }
        impl Sink for CapSink {
            fn reserve_data(&mut self, bytes: usize) -> bool {
                let held = self.held + bytes;
                if held > self.cap {
                    return false;
                }
                self.held = held;
                self.peak = self.peak.max(held);
                true
            }
            fn release_data(&mut self, bytes: usize) {
                self.held -= bytes;
            }
            fn keep_shared(&mut self, bytes: usize) {
                self.shared = self.shared.saturating_add(bytes);
            }
            fn retain_shared(&mut self, bytes: usize) {
                self.held -= bytes;
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
            if s.admit_data(bytes) {
                ranges_reserved.set(ranges_reserved.get().saturating_add(bytes));
                true
            } else {
                false
            }
        };
        let temp_mem = |delta: isize| {
            let mut s = sink_cell.borrow_mut();
            if delta > 0 {
                s.admit_data(delta as usize)
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
            &|_| true,
        );
        let sink = sink_cell.into_inner();
        sink.release_data(ranges_reserved.get());
        assert_eq!(ans, Err(Why::Stopped));
        assert!(sink.peak <= sink.cap);
        assert_eq!(sink.held, 0);
        // Ranges did not grow past the cap because admission refused before interning.
        assert_eq!(ranges.bytes(), before_bytes);
    }

    #[test]
    fn evaluate_admits_key_before_allocation_including_cache_hit() {
        struct CapSink {
            cap: usize,
            held: usize,
            peak: usize,
        }
        impl Sink for CapSink {
            fn reserve_data(&mut self, bytes: usize) -> bool {
                let held = self.held + bytes;
                if held > self.cap {
                    return false;
                }
                self.held = held;
                self.peak = self.peak.max(held);
                true
            }
            fn release_data(&mut self, bytes: usize) {
                self.held -= bytes;
            }
            fn send(&mut self, _to: Ex, _c: Concl) {}
            fn me(&mut self, _c: Concl) {}
        }

        let body: Vec<Body> = (10..266u32).map(|e| Body { e, told: None }).collect();
        let dt = DtIx::new(
            Vec::new(),
            FxHashMap::default(),
            vec![body],
            Vec::new(),
            Ranges::new(),
            Comps::default(),
            FxHashMap::default(),
        );
        // Pre-populate cache with the exact key that `evaluate` would build.
        let key: Key = (
            0,
            vec![1u32].into_boxed_slice(),
            (10..266u32)
                .map(|e| (e, Group::Other))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        dt.cache.lock().insert(key, Arc::new(Ok(Units::default())));

        let ix = Index::default();
        let mut st = State::default();
        st.extra().dt.push(DtSt {
            comp: 0,
            held: vec![1],
            last: FINE,
        });
        // Cap is far smaller than the 256-entry body key (~2048 B).
        let mut sink = CapSink {
            cap: 256,
            held: 0,
            peak: 0,
        };
        evaluate(&ix, &dt, &mut st, 0, &mut sink);
        assert_eq!(st.extra.as_ref().unwrap().dt[0].last, STOPPED);
        assert!(sink.peak <= sink.cap);
        assert_eq!(sink.held, 0);
    }

    #[test]
    fn check_bounded_admits_initial_workspace_and_size_cache_and_units_handoff() {
        // 1. Initial range-index workspace refused before allocation under a tiny mem cap.
        let mut r = Ranges::new();
        let mut atoms = Vec::new();
        for i in 0..32 {
            let rid = r
                .of(&crate::model::DataRange::OneOf(
                    [crate::model::Literal::typed(
                        format!("{i}"),
                        crate::vocab::XSD_INTEGER,
                    )]
                    .into_iter()
                    .collect(),
                ))
                .unwrap();
            atoms.push(Atom::AtLeast(0, 1, rid));
        }
        let p = Problem {
            props: vec![Prop {
                supers: Vec::new(),
                ranges: Vec::new(),
            }],
            disjoint: Vec::new(),
            atoms,
        };
        let mem_held = std::cell::Cell::new(0isize);
        let mem_peak = std::cell::Cell::new(0usize);
        let tiny_mem = |delta: isize| {
            if delta > 0 {
                let next = mem_held.get() + delta;
                if next as usize > 128 {
                    return false;
                }
                mem_held.set(next);
                mem_peak.set(mem_peak.get().max(next as usize));
                true
            } else {
                mem_held.set(mem_held.get() + delta);
                true
            }
        };
        let ans = super::super::dtcheck::solve::check_bounded(
            &mut r,
            &p,
            &Bounds::default(),
            &|| false,
            &|_| true,
            &tiny_mem,
        );
        assert_eq!(
            ans,
            super::super::dtcheck::solve::Answer::Unknown(Why::Stopped)
        );
        assert!(mem_peak.get() <= 128);
        assert_eq!(mem_held.get(), 0);

        // 2. Pre-populated meet/not caches still admit size-cache growth via size_bounded.
        let mut r2 = Ranges::new();
        let xsd_int = format!("{}int", crate::vocab::XSD);
        let x = r2
            .of(&crate::model::DataRange::Datatype(
                crate::model::Datatype::new(&xsd_int),
            ))
            .unwrap();
        let nx = r2.not(x);
        let _ = r2.meet(Ranges::FULL, x);
        let _ = r2.meet(Ranges::FULL, nx);
        let bytes_before_size = r2.bytes();
        let p2 = Problem {
            props: vec![Prop {
                supers: Vec::new(),
                ranges: Vec::new(),
            }],
            disjoint: Vec::new(),
            atoms: vec![Atom::AtLeast(0, 1, x)],
        };
        let ans2 = super::super::dtcheck::solve::check_bounded(
            &mut r2,
            &p2,
            &Bounds::default(),
            &|| false,
            &|_| false, // deny even size-cache insertion
            &|_| true,
        );
        assert_eq!(
            ans2,
            super::super::dtcheck::solve::Answer::Unknown(Why::Stopped)
        );
        assert_eq!(r2.bytes(), bytes_before_size);

        // 3. `units_bounded` transfers output-vector reservation BEFORE `body_scope` drops.
        let mut r3 = Ranges::new();
        let int_r = r3
            .of(&crate::model::DataRange::Datatype(
                crate::model::Datatype::new(&xsd_int),
            ))
            .unwrap();
        let p3 = Problem {
            props: vec![Prop {
                supers: Vec::new(),
                ranges: Vec::new(),
            }],
            disjoint: Vec::new(),
            atoms: vec![Atom::AtLeast(0, 2, int_r)],
        };
        let body3 = vec![(Atom::AtLeast(0, 1, int_r), Group::Free)];
        let live_mem = std::cell::Cell::new(0isize);
        let handoff_saw_live_body = std::cell::Cell::new(false);
        let out_bytes_charged = std::cell::Cell::new(0usize);
        let got = rule::units_bounded(
            &mut r3,
            &p3,
            &body3,
            &Bounds::default(),
            &|| false,
            &|_| true,
            &|d| {
                live_mem.set(live_mem.get() + d);
                true
            },
            &|n| {
                // `body_scope` must still be live when `admit_out` transfers the Vectors.
                handoff_saw_live_body.set(live_mem.get() > 0);
                out_bytes_charged.set(out_bytes_charged.get() + n);
                true
            },
        )
        .unwrap();
        assert_eq!(got.entailed, vec![0]);
        assert!(handoff_saw_live_body.get());
        assert!(out_bytes_charged.get() >= got.entailed.capacity() * std::mem::size_of::<usize>());
        assert_eq!(live_mem.get(), 0);

        // 4. Non-power-of-two evaluation key (513 body atoms) admits both Vec backing and
        // boxed-slice conversion without un-admitted 1024-slot doubling.
        let body513: Vec<Body> = (10..523u32).map(|e| Body { e, told: None }).collect();
        let dt513 = DtIx::new(
            Vec::new(),
            FxHashMap::default(),
            vec![body513],
            Vec::new(),
            Ranges::new(),
            Comps::default(),
            FxHashMap::default(),
        );
        let key513: Key = (
            0,
            vec![1u32].into_boxed_slice(),
            (10..523u32)
                .map(|e| (e, Group::Other))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        dt513
            .cache
            .lock()
            .insert(key513, Arc::new(Ok(Units::default())));
        let ix513 = Index::default();
        let mut st513 = State::default();
        st513.extra().dt.push(DtSt {
            comp: 0,
            held: vec![1],
            last: FINE,
        });
        struct RecordSink {
            cap: usize,
            held: usize,
            peak: usize,
        }
        impl Sink for RecordSink {
            fn reserve_data(&mut self, bytes: usize) -> bool {
                let held = self.held + bytes;
                if held > self.cap {
                    return false;
                }
                self.held = held;
                self.peak = self.peak.max(held);
                true
            }
            fn release_data(&mut self, bytes: usize) {
                self.held -= bytes;
            }
            fn send(&mut self, _to: Ex, _c: Concl) {}
            fn me(&mut self, _c: Concl) {}
        }
        // Old single-copy estimate (513 * size_of + 4 + 32) is refused now because
        // `key_est` covers both `Vec::with_capacity(513)` and `into_boxed_slice`.
        let old_key_est = 4 + 513 * std::mem::size_of::<(Ex, Group)>() + 32;
        let mut sink513 = RecordSink {
            cap: old_key_est + 64,
            held: 0,
            peak: 0,
        };
        evaluate(&ix513, &dt513, &mut st513, 0, &mut sink513);
        assert_eq!(st513.extra.as_ref().unwrap().dt[0].last, STOPPED);
        assert_eq!(sink513.held, 0);

        // 5. Initial solver workspace with 897 distinct ranges (hashbrown 2048-bucket
        // boundary): refuses old 32,396-byte underestimate and accepts exact planned cap.
        let mut r897 = Ranges::new();
        let mut atoms897 = Vec::with_capacity(897);
        for i in 0..897i64 {
            let rid = r897
                .of(&crate::model::DataRange::OneOf(
                    [crate::model::Literal::typed(
                        format!("{i}"),
                        crate::vocab::XSD_INTEGER,
                    )]
                    .into_iter()
                    .collect(),
                ))
                .unwrap();
            atoms897.push(Atom::AtLeast(0, 1, rid));
        }
        let p897 = Problem {
            props: vec![Prop {
                supers: Vec::new(),
                ranges: Vec::new(),
            }],
            disjoint: Vec::new(),
            atoms: atoms897,
        };
        let old_init_underest = 33_000usize;
        let held897 = std::cell::Cell::new(0isize);
        let peak897 = std::cell::Cell::new(0usize);
        let ans897 = super::super::dtcheck::solve::check_bounded(
            &mut r897,
            &p897,
            &Bounds::default(),
            &|| false,
            &|_| true,
            &|d| {
                if d > 0 {
                    let next = held897.get() + d;
                    if next as usize > old_init_underest {
                        return false;
                    }
                    held897.set(next);
                    peak897.set(peak897.get().max(next as usize));
                    true
                } else {
                    held897.set(held897.get() + d);
                    true
                }
            },
        );
        assert_eq!(
            ans897,
            super::super::dtcheck::solve::Answer::Unknown(Why::Stopped)
        );
        assert_eq!(held897.get(), 0);
        // Verify exact planned initial reservation covers the 2048-bucket map + 897 Vec.
        let first_admit = std::cell::Cell::new(0usize);
        let _ = super::super::dtcheck::solve::check_bounded(
            &mut r897,
            &p897,
            &Bounds::default(),
            &|| false,
            &|_| true,
            &|d| {
                if d > 0 && first_admit.get() == 0 {
                    first_admit.set(d as usize);
                }
                false
            },
        );
        let actual_backing =
            8 + 897 * std::mem::size_of::<Rid>() + 2048 * (std::mem::size_of::<(Rid, usize)>() + 1);
        assert!(
            first_admit.get() >= actual_backing,
            "first_admit {} must cover actual backing >= {actual_backing}",
            first_admit.get()
        );
    }

    #[test]
    fn cache_rehash_publish_admits_table_growth_before_inserting() {
        struct CapSink {
            cap: usize,
            held: usize,
            peak: usize,
            shared: usize,
        }
        impl Sink for CapSink {
            fn reserve_data(&mut self, bytes: usize) -> bool {
                let held = self.held + bytes;
                if held > self.cap {
                    return false;
                }
                self.held = held;
                self.peak = self.peak.max(held);
                true
            }
            fn release_data(&mut self, bytes: usize) {
                self.held -= bytes;
            }
            fn retain_shared(&mut self, bytes: usize) {
                self.held -= bytes;
                self.shared += bytes;
            }
            fn keep_shared(&mut self, bytes: usize) {
                self.shared += bytes;
            }
            fn send(&mut self, _to: Ex, _c: Concl) {}
            fn me(&mut self, _c: Concl) {}
        }

        let dt = DtIx::new(
            Vec::new(),
            FxHashMap::default(),
            vec![Vec::new()],
            Vec::new(),
            Ranges::new(),
            Comps::default(),
            FxHashMap::default(),
        );
        // Fill `cache` to exact capacity boundary 896 (1024 buckets -> next insert rehashes to 2048).
        {
            let mut cache = dt.cache.lock();
            for i in 0..896u32 {
                let k: Key = (0, vec![i].into_boxed_slice(), Box::new([]));
                cache.insert(k, Arc::new(Ok(Units::default())));
            }
            assert_eq!(cache.len(), 896);
            assert_eq!(cache.capacity(), 896);
        }
        let next_key: Key = (0, vec![896u32].into_boxed_slice(), Box::new([]));
        let ans = Ok(Units::default());
        let payload = entry_payload_bytes(&next_key, &ans);
        let growth = map_growth(&dt.cache.lock());
        assert!(growth >= 2048 * (std::mem::size_of::<(Key, Arc<Result<Units, Why>>)>() + 1));

        // Old single-entry estimate (payload + inline slot + 64) must be refused BEFORE rehash.
        let old_est = payload
            + std::mem::size_of::<Key>()
            + std::mem::size_of::<Arc<Result<Units, Why>>>()
            + 64;
        assert!(old_est < payload + growth);
        let mut tight = CapSink {
            cap: old_est + 256,
            held: 0,
            peak: 0,
            shared: 0,
        };
        let refused = publish_after_miss(&dt, &next_key, ans.clone(), &mut tight);
        assert_eq!(*refused, Err(Why::Stopped));
        assert_eq!(tight.held, 0);
        assert_eq!(tight.shared, 0);
        {
            let cache = dt.cache.lock();
            assert_eq!(cache.len(), 896);
            assert_eq!(cache.capacity(), 896);
        }

        // With exact rehash budget, publish succeeds and retains exact `DtIx::bytes()` delta.
        let before = dt.bytes();
        let mut exact = CapSink {
            cap: payload + growth,
            held: 0,
            peak: 0,
            shared: 0,
        };
        let ok = publish_after_miss(&dt, &next_key, ans, &mut exact);
        assert!(ok.is_ok());
        assert_eq!(exact.held, 0);
        assert_eq!(exact.peak, payload + growth);
        assert_eq!(exact.shared, dt.bytes() - before);
        assert_eq!(dt.cache.lock().len(), 897);
    }

    #[test]
    fn dt_ix_and_extra_bytes_cover_backing_capacities_and_sticky() {
        // Build a `DtIx` with 897 roles (2048 buckets), 897 refire keys (2048 buckets),
        // 256 components (outer `body` Vec + `sticky` Vec) and spare inner `body` capacity.
        let mut roles = FxHashMap::default();
        let mut refire = FxHashMap::default();
        for i in 0..897u32 {
            roles.insert(
                i,
                RoleIx {
                    comp: i % 256,
                    supers: Vec::new(),
                    ranges: Vec::new(),
                    functional: false,
                },
            );
            refire.insert(i, vec![i % 256]);
        }
        let mut body: Vec<Vec<Body>> = (0..256).map(|_| Vec::new()).collect();
        body[0].reserve(512);
        let expected_min = std::mem::size_of::<DtIx>()
            + map_backing_bytes(&roles)
            + body.capacity() * std::mem::size_of::<Vec<Body>>()
            + body[0].capacity() * std::mem::size_of::<Body>()
            + map_backing_bytes(&refire)
            + refire
                .values()
                .map(|v| v.capacity() * std::mem::size_of::<u32>())
                .sum::<usize>()
            + 256; // sticky
        // Build `DtIx` with `body.len() == 256`; `sticky` has length `comps.len()`.
        let dt = DtIx::new(
            Vec::new(),
            roles,
            body,
            Vec::new(),
            Ranges::new(),
            Comps::default(),
            refire,
        );
        // Reserve 256 bytes in `sticky` to mirror 256 components.
        dt.sticky.lock().resize(256, FINE);
        assert!(
            dt.bytes() >= expected_min,
            "DtIx::bytes() {} must cover backing capacities + sticky >= {expected_min}",
            dt.bytes()
        );

        // `Extra::bytes()` must include spare `Extra::dt` vector slots, not only occupied entries.
        let mut st = State::default();
        let base = st.bytes();
        {
            let x = st.extra();
            x.dt.reserve(128);
            x.dt.push(DtSt {
                comp: 0,
                held: Vec::new(),
                last: FINE,
            });
        }
        let cap = st.extra.as_ref().unwrap().dt.capacity();
        assert!(cap >= 128);
        let grown = st.bytes() - base;
        assert!(
            grown >= cap * std::mem::size_of::<DtSt>(),
            "State::bytes() growth {grown} must include spare Extra::dt slots >= {}",
            cap * std::mem::size_of::<DtSt>()
        );
    }

    #[test]
    fn rule_units_513_body_and_solve_types_cap_admit_without_doubling() {
        // 1. 513 unentailed non-free body atoms in `rule::units_bounded`: verify refusal
        // happens specifically on the `body_est` reservation right after the base
        // `refutes(r, base, &[], ...)` completes (not later in the first atom refutation).
        let mut r = Ranges::new();
        let base = Problem {
            props: vec![Prop {
                supers: Vec::new(),
                ranges: Vec::new(),
            }],
            disjoint: Vec::new(),
            atoms: Vec::new(),
        };
        let body: Vec<(Atom, Group)> = (0..513)
            .map(|_| (Atom::AtLeast(0, 1, Ranges::FULL), Group::Only(1)))
            .collect();
        let old_body_est =
            513 * (std::mem::size_of::<(Atom, Group)>() + std::mem::size_of::<usize>() + 16);
        let live = std::cell::Cell::new(0isize);
        let base_done = std::cell::Cell::new(false);
        let body_req = std::cell::Cell::new(0usize);
        let calls_after_body_refusal = std::cell::Cell::new(0usize);
        let cap = old_body_est + 512;
        let refused = rule::units_bounded(
            &mut r,
            &base,
            &body,
            &Bounds::default(),
            &|| false,
            &|_| true,
            &|d| {
                if body_req.get() > 0 {
                    calls_after_body_refusal.set(calls_after_body_refusal.get() + 1);
                }
                let next = live.get() + d;
                if d > 0 && base_done.get() && body_req.get() == 0 {
                    body_req.set(d as usize);
                }
                if d > 0 && next as usize > cap {
                    return false;
                }
                live.set(next);
                if d < 0 && next == 0 {
                    base_done.set(true);
                }
                true
            },
            &|_| true,
        );
        assert_eq!(refused, Err(Why::Stopped));
        assert_eq!(live.get(), 0);
        // Refusal must occur on `body_est` itself with zero subsequent `mem` calls.
        assert!(
            body_req.get() > cap,
            "body_est {} must exceed old cap {cap} and refuse immediately",
            body_req.get()
        );
        assert_eq!(calls_after_body_refusal.get(), 0);

        // Unbounded 513-body run: directly assert the isolated `body_est` reservation
        // covers simultaneous `left`, `classes`, `w`, and doubling scratch of `out`.
        let base_done2 = std::cell::Cell::new(false);
        let body_req2 = std::cell::Cell::new(0usize);
        let ok = rule::units_bounded(
            &mut r,
            &base,
            &body,
            &Bounds::default(),
            &|| false,
            &|_| true,
            &|d| {
                let next = live.get() + d;
                if d > 0 && base_done2.get() && body_req2.get() == 0 {
                    body_req2.set(d as usize);
                }
                live.set(next);
                if d < 0 && next == 0 {
                    base_done2.set(true);
                }
                true
            },
            &|_| true,
        );
        assert!(ok.is_ok());
        assert_eq!(live.get(), 0);
        let min_body_est = 513
            * (std::mem::size_of::<(Atom, Group)>()
                + std::mem::size_of::<u32>()
                + std::mem::size_of::<Atom>())
            + 2 * 1024 * (std::mem::size_of::<usize>() + std::mem::size_of::<u32>());
        assert!(
            body_req2.get() >= min_body_est,
            "body_est {} must cover simultaneous vectors + out doubling >= {min_body_est}",
            body_req2.get()
        );

        // 2. `solve::check_bounded` checks `types.len() == b.types` BEFORE pushing type
        // `b.types + 1`: 10 independent properties yield 1023 valid sets on 1 cell,
        // exceeding `b.types = 512` without pushing type 513 or doubling capacity to 1024.
        let props10: Vec<Prop> = (0..10)
            .map(|_| Prop {
                supers: Vec::new(),
                ranges: Vec::new(),
            })
            .collect();
        let p10 = Problem {
            props: props10,
            disjoint: Vec::new(),
            atoms: Vec::new(),
        };
        let bounds512 = Bounds {
            types: 512,
            ..Bounds::default()
        };
        let exact_cap = 10 * std::mem::size_of::<u64>()
            + std::mem::size_of::<(Rid, Vec<bool>)>()
            + 64
            + (1usize << 10) * std::mem::size_of::<u64>()
            + 512 * std::mem::size_of::<(usize, u64)>();
        let live2 = std::cell::Cell::new(0isize);
        let ans = super::super::dtcheck::solve::check_bounded(
            &mut r,
            &p10,
            &bounds512,
            &|| false,
            &|_| true,
            &|d| {
                let next = live2.get() + d;
                if d > 0 && next as usize > exact_cap {
                    return false;
                }
                live2.set(next);
                true
            },
        );
        assert_eq!(
            ans,
            super::super::dtcheck::solve::Answer::Unknown(Why::Bound)
        );
        assert_eq!(live2.get(), 0);
        // Must observe exact pre-push length and capacity 512 (not 513 / 1024).
        assert_eq!(
            super::super::dtcheck::solve::last_bound_caps(),
            (0, 0, 512, 512)
        );

        // 3. Cell-split loop checks `next.len() == b.types` BEFORE pushing cell
        // `b.types + 1`: 5 independent integer ranges split into 32 cells, exceeding
        // `b.types = 16` at exact length and capacity 16 (not 17 / 32).
        let mut rcells = Ranges::new();
        let mut cell_atoms = Vec::new();
        for bit in 0..5u32 {
            // Integers in [0, 31] whose `bit` is set -> 5 independent sets -> 32 cells.
            let lits: std::collections::BTreeSet<crate::model::Literal> = (0..32u32)
                .filter(|v| (v >> bit) & 1 != 0)
                .map(|v| crate::model::Literal::typed(format!("{v}"), crate::vocab::XSD_INTEGER))
                .collect();
            let rid = rcells.of(&crate::model::DataRange::OneOf(lits)).unwrap();
            cell_atoms.push(Atom::AtLeast(0, 1, rid));
        }
        let pcells = Problem {
            props: vec![Prop {
                supers: Vec::new(),
                ranges: Vec::new(),
            }],
            disjoint: Vec::new(),
            atoms: cell_atoms,
        };
        let bounds16 = Bounds {
            types: 16,
            ..Bounds::default()
        };
        let live3 = std::cell::Cell::new(0isize);
        let ans_cells = super::super::dtcheck::solve::check_bounded(
            &mut rcells,
            &pcells,
            &bounds16,
            &|| false,
            &|_| true,
            &|d| {
                live3.set(live3.get() + d);
                true
            },
        );
        assert_eq!(
            ans_cells,
            super::super::dtcheck::solve::Answer::Unknown(Why::Bound)
        );
        assert_eq!(live3.get(), 0);
        assert_eq!(
            super::super::dtcheck::solve::last_bound_caps(),
            (16, 16, 0, 0)
        );
    }
}
