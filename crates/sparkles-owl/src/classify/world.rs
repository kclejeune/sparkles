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
use super::index::{BOTTOM, Ex, Halt, Index, Node, Role};
use super::saturate::{
    CONCLUSION_BYTES, CONTEXT_BYTES, Concl, Contexts, Dynamic, Sink, State, apply, flatten, init,
    init_conjunction,
};
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::Cell;

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

/// The global contexts with the contexts of conjunctions that the Horn rules created,
/// whose ids lie above the index's (roadmap item 692).
#[derive(Clone, Copy)]
pub struct GlobalAll<'a> {
    pub ctx: &'a Contexts,
    pub dynamic: Option<&'a Dynamic>,
}

impl View for GlobalAll<'_> {
    fn with(&self, e: Ex, f: &mut dyn FnMut(&State, bool)) {
        if (e as usize) < self.ctx.len() {
            Global(self.ctx).with(e, f);
        } else if let Some(d) = self.dynamic
            && d.holds(e)
            && let Some(c) = d.get_ctx(e)
        {
            let st = c.st.lock();
            if st.init {
                f(&st, st.real);
            }
        }
    }
    fn len(&self) -> usize {
        self.ctx.len() + self.dynamic.map_or(0, |d| d.len())
    }
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

/// The pairs of named individuals made equal by keys. All vector capacities remain
/// admitted until their owners die; only the returned pair owner outlives this call.
pub struct KeyMerges<'a> {
    pairs: KeyStorage<'a, (Ex, Ex)>,
}
impl std::ops::Deref for KeyMerges<'_> {
    type Target = [(Ex, Ex)];
    fn deref(&self) -> &Self::Target {
        &self.pairs.rows
    }
}
struct Scratch<'a> {
    bound: &'a (dyn Fn(isize) -> bool + Sync),
    bytes: usize,
}
impl Drop for Scratch<'_> {
    fn drop(&mut self) {
        if self.bytes > 0 {
            (self.bound)(-(self.bytes as isize));
        }
    }
}
#[cfg(test)]
thread_local! { pub(super) static KEY_ADMITTING: Cell<bool> = const { Cell::new(false) }; }

/// A key-local vector with an exact requested capacity. Its rows drop before its
/// reservation, including on an early error. Numeric Value clones share payloads
/// already covered by the existing numeric ledger.
struct KeyStorage<'a, T> {
    rows: Vec<T>,
    _scratch: Scratch<'a>,
}
impl<'a, T> KeyStorage<'a, T> {
    fn new(count: usize, bound: &'a (dyn Fn(isize) -> bool + Sync)) -> Result<Self, Halt> {
        Self::with_payload(count, 0, bound)
    }
    fn with_payload(
        count: usize,
        payload: usize,
        bound: &'a (dyn Fn(isize) -> bool + Sync),
    ) -> Result<Self, Halt> {
        Self::with_live(count, payload, 0, bound)
    }
    fn with_live(
        count: usize,
        payload: usize,
        live: usize,
        bound: &'a (dyn Fn(isize) -> bool + Sync),
    ) -> Result<Self, Halt> {
        let bytes = count
            .checked_mul(std::mem::size_of::<T>())
            .and_then(|bytes| bytes.checked_add(payload))
            .filter(|&bytes| bytes <= isize::MAX as usize)
            .ok_or(Halt::Memory(usize::MAX))?;
        live.checked_add(bytes)
            .filter(|&total| total <= isize::MAX as usize)
            .ok_or(Halt::Memory(usize::MAX))?;
        let scratch = Scratch { bound, bytes };
        if bytes > 0 {
            #[cfg(test)]
            KEY_ADMITTING.with(|p| p.set(true));
            let accepted = bound(bytes as isize);
            #[cfg(test)]
            KEY_ADMITTING.with(|p| p.set(false));
            if !accepted {
                return Err(Halt::Stopped);
            }
        }
        let mut rows = Vec::new();
        rows.try_reserve_exact(count)
            .map_err(|_| Halt::Memory(usize::MAX))?;
        // The bound concerns owned requested capacity, not allocator rounding/RSS.
        // Refuse an allocator-provided larger capacity rather than use it uncharged.
        if rows.capacity() != count {
            return Err(Halt::Memory(usize::MAX));
        }
        Ok(Self {
            rows,
            _scratch: scratch,
        })
    }
    fn push(&mut self, row: T) -> Result<(), Halt> {
        if self.rows.len() == self.rows.capacity() {
            // A changing view cannot make a counted owner grow without admission.
            return Err(Halt::Memory(usize::MAX));
        }
        self.rows.push(row);
        Ok(())
    }
}
/// Strings clone their bytes; numeric Arc payloads and fixed values do not.
fn key_value_copy_bytes(value: &Value) -> Result<usize, Halt> {
    match value {
        Value::Str(s) | Value::Uri(s) | Value::Hex(s) | Value::Base64(s) => Ok(s.len()),
        Value::Lang(s, language) => s
            .len()
            .checked_add(language.len())
            .ok_or(Halt::Memory(usize::MAX)),
        Value::Num(_) | Value::Time(..) => Ok(0),
    }
}
fn key_live(bytes: &[usize]) -> Result<usize, Halt> {
    bytes
        .iter()
        .try_fold(0usize, |held, &n| held.checked_add(n))
        .filter(|&total| total <= isize::MAX as usize)
        .ok_or(Halt::Memory(usize::MAX))
}
fn key_poll(bound: &(dyn Fn(isize) -> bool + Sync)) -> Result<(), Halt> {
    if bound(0) { Ok(()) } else { Err(Halt::Stopped) }
}
fn key_add(count: &mut usize, more: usize) -> Result<(), Halt> {
    *count = count.checked_add(more).ok_or(Halt::Memory(usize::MAX))?;
    Ok(())
}
fn key_member(ix: &Index, view: &dyn View, individual: Ex, class: Ex) -> bool {
    let mut member = false;
    view.with(individual, &mut |st, real| {
        member = real
            && !st.bottom
            && matches!(ix.head(individual).node, Node::Nominal(_))
            && st.subs.contains(&class);
    });
    member
}
fn key_member_count(
    ix: &Index,
    view: &dyn View,
    class: Ex,
    bound: &(dyn Fn(isize) -> bool + Sync),
) -> Result<usize, Halt> {
    let mut count = 0;
    for &individual in &ix.nominals {
        key_poll(bound)?;
        if key_member(ix, view, individual, class) {
            key_add(&mut count, 1)?;
        }
    }
    Ok(count)
}
fn key_aliases<'s>(ix: &Index, e: Ex, st: &'s State) -> impl Iterator<Item = Ex> + use<'s> {
    matches!(ix.head(e).node, Node::Nominal(_))
        .then_some(e)
        .into_iter()
        .chain(st.noms().iter().copied())
}

/// No callback enters another View: Global holds a context mutex during with().
fn key_sources(
    ix: &Index,
    view: &dyn View,
    members: &[Ex],
    bound: &(dyn Fn(isize) -> bool + Sync),
    mut emit: impl FnMut((Ex, Ex)) -> Result<(), Halt>,
) -> Result<(), Halt> {
    for e in 0..view.len() as Ex {
        key_poll(bound)?;
        let mut status = Ok(());
        view.with(e, &mut |st, real| {
            if !real {
                return;
            }
            for alias in key_aliases(ix, e, st) {
                status = key_poll(bound);
                if status.is_err() {
                    break;
                }
                if members.contains(&alias) {
                    status = emit((e, alias));
                    if status.is_err() {
                        break;
                    }
                }
            }
        });
        status?;
    }
    Ok(())
}
fn source_aliases(sources: &[(Ex, Ex)], e: Ex) -> &[(Ex, Ex)] {
    let lo = sources.partition_point(|&(context, _)| context < e);
    let hi = sources[lo..].partition_point(|&(context, _)| context == e) + lo;
    &sources[lo..hi]
}
fn key_records(
    ix: &Index,
    view: &dyn View,
    key: &super::index::Key,
    sources: &[(Ex, Ex)],
    bound: &(dyn Fn(isize) -> bool + Sync),
    mut object: impl FnMut((Ex, Role, Ex)) -> Result<(), Halt>,
    mut data: impl FnMut(Ex, Role, &Value) -> Result<(), Halt>,
) -> Result<(), Halt> {
    let object_role = |r| {
        key.objects
            .iter()
            .any(|&p| !ix.is_universal(p) && ix.roles.sub(r, p))
    };
    let data_role = |r| {
        key.data
            .iter()
            .any(|&p| !ix.is_universal(p) && ix.roles.sub(r, p))
    };
    for e in 0..view.len() as Ex {
        key_poll(bound)?;
        let mut status = Ok(());
        view.with(e, &mut |st, real| {
            status = (|| {
                for (role, srcs) in &st.back {
                    key_poll(bound)?;
                    if !object_role(*role) {
                        continue;
                    }
                    for src in &srcs.list {
                        key_poll(bound)?;
                        for &(_, individual) in source_aliases(sources, *src) {
                            // A shared named target need not belong to the key class,
                            // and its context need not be real.
                            for target in key_aliases(ix, e, st) {
                                key_poll(bound)?;
                                object((individual, *role, target))?;
                            }
                        }
                    }
                }
                if !real {
                    return Ok(());
                }
                for &(_, individual) in source_aliases(sources, e) {
                    for &sub in &st.subs {
                        key_poll(bound)?;
                        let head = ix.head(sub);
                        if let Node::DataSome(role, _) = head.node
                            && data_role(role)
                            && let Some(value) = ix.vals[head.succ as usize].single()
                        {
                            data(individual, role, value)?;
                        }
                    }
                    if let Some(extra) = &st.extra {
                        for group in &extra.groups {
                            key_poll(bound)?;
                            if let Some(value) = group.val.single() {
                                for &role in &group.roles {
                                    key_poll(bound)?;
                                    if data_role(role) {
                                        data(individual, role, value)?;
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(())
            })();
        });
        status?;
    }
    Ok(())
}

pub fn key_merges<'a>(
    ix: &Index,
    view: &dyn View,
    bound: &'a (dyn Fn(isize) -> bool + Sync),
) -> Result<KeyMerges<'a>, Halt> {
    // The global fixpoint and each conditional world are stable while this routine
    // reads them. The first pass counts the pairs each key actually merges, so that the
    // returned storage is admitted for those pairs only, and not for every pair of key
    // members. The second pass repeats the matching and keeps the pairs. This doubles
    // the key work in exchange for storage that grows with the merges found.
    let mut found = 0;
    for key in &ix.keys {
        key_matches(ix, view, key, 0, bound, |_| key_add(&mut found, 1))?;
    }
    let mut pairs = KeyStorage::new(found, bound)?;
    let live = pairs._scratch.bytes;
    for key in &ix.keys {
        key_matches(ix, view, key, live, bound, |pair| pairs.push(pair))?;
    }
    pairs.rows.sort_unstable();
    pairs.rows.dedup();
    Ok(KeyMerges { pairs })
}

/// The rows of `rows`, sorted by owner, that `owner` has.
fn owned<T>(rows: &[T], owner: Ex, of: impl Fn(&T) -> Ex) -> &[T] {
    let start = rows.partition_point(|row| of(row) < owner);
    let len = rows[start..].partition_point(|row| of(row) == owner);
    &rows[start..start + len]
}

/// Emits each pair of members of `key` that the key merges. `live` is the storage the
/// caller already holds, so that each admission here counts it.
fn key_matches(
    ix: &Index,
    view: &dyn View,
    key: &super::index::Key,
    live: usize,
    bound: &(dyn Fn(isize) -> bool + Sync),
    mut emit: impl FnMut((Ex, Ex)) -> Result<(), Halt>,
) -> Result<(), Halt> {
    let count = key_member_count(ix, view, key.class, bound)?;
    if count < 2 {
        return Ok(());
    }
    let mut members = KeyStorage::with_live(count, 0, live, bound)?;
    for &individual in &ix.nominals {
        key_poll(bound)?;
        if key_member(ix, view, individual, key.class) {
            members.push(individual)?;
        }
    }
    let mut count = 0;
    key_sources(ix, view, &members.rows, bound, |_| key_add(&mut count, 1))?;
    let mut sources =
        KeyStorage::with_live(count, 0, key_live(&[live, members._scratch.bytes])?, bound)?;
    key_sources(ix, view, &members.rows, bound, |row| sources.push(row))?;
    sources.rows.sort_unstable();
    let (mut object_count, mut data_count, mut data_payload) = (0, 0, 0);
    key_records(
        ix,
        view,
        key,
        &sources.rows,
        bound,
        |_| key_add(&mut object_count, 1),
        |_, _, value| {
            key_add(&mut data_count, 1)?;
            key_add(&mut data_payload, key_value_copy_bytes(value)?)
        },
    )?;
    let held = key_live(&[live, members._scratch.bytes, sources._scratch.bytes])?;
    let mut objects = KeyStorage::with_live(object_count, 0, held, bound)?;
    let mut data = KeyStorage::with_live(
        data_count,
        data_payload,
        key_live(&[held, objects._scratch.bytes])?,
        bound,
    )?;
    key_records(
        ix,
        view,
        key,
        &sources.rows,
        bound,
        |row| objects.push(row),
        |individual, role, value| data.push((individual, role, value.clone())),
    )?;
    // Grouped by owner, a pair reads only the records of its two members. The sorts
    // are in place and allocate nothing.
    objects.rows.sort_unstable_by_key(|row| row.0);
    data.rows.sort_unstable_by_key(|row| row.0);
    for (position, &a) in members.rows.iter().enumerate() {
        let (a_objects, a_data) = (
            owned(&objects.rows, a, |row| row.0),
            owned(&data.rows, a, |row| row.0),
        );
        for &b in &members.rows[position + 1..] {
            key_poll(bound)?;
            let mut already_merged = false;
            view.with(a, &mut |st, _| already_merged = st.subs.contains(&b));
            if already_merged {
                continue;
            }
            let (b_objects, b_data) = (
                owned(&objects.rows, b, |row| row.0),
                owned(&data.rows, b, |row| row.0),
            );
            let mut matches = true;
            for &role in &key.objects {
                key_poll(bound)?;
                if ix.is_universal(role) {
                    continue;
                }
                let mut found = false;
                for &(_, r, target) in a_objects {
                    key_poll(bound)?;
                    if !ix.roles.sub(r, role) {
                        continue;
                    }
                    for &(_, s, shared) in b_objects {
                        key_poll(bound)?;
                        if shared == target && ix.roles.sub(s, role) {
                            found = true;
                            break;
                        }
                    }
                    if found {
                        break;
                    }
                }
                if !found {
                    matches = false;
                    break;
                }
            }
            if !matches {
                continue;
            }
            for &role in &key.data {
                key_poll(bound)?;
                if ix.is_universal(role) {
                    continue;
                }
                let mut found = false;
                for (_, r, value) in a_data {
                    key_poll(bound)?;
                    if !ix.roles.sub(*r, role) {
                        continue;
                    }
                    for (_, s, shared) in b_data {
                        key_poll(bound)?;
                        if shared == value && ix.roles.sub(*s, role) {
                            found = true;
                            break;
                        }
                    }
                    if found {
                        break;
                    }
                }
                if !found {
                    matches = false;
                    break;
                }
            }
            if matches {
                emit((a, b))?;
            }
        }
    }
    Ok(())
}

/// What a context that derives an individual knows, which the individual lacks when the
/// context is a witness.
struct Knows {
    subs: Vec<Ex>,
    succ: Vec<(Role, Ex)>,
    loops: Vec<Role>,
    noms: Vec<Ex>,
}

/// The witnesses: the contexts that are not real, derive an individual and know more
/// than it, or give a fact of a universal role that is not global. An intersection
/// among the subsumers adds nothing beyond its operands, which are subsumers too, so
/// only the other subsumers count.
pub fn witnesses(
    ix: &Index,
    ctx: &Contexts,
    dynamic: Option<&Dynamic>,
    universal: &[Ex],
) -> Vec<Ex> {
    let view = GlobalAll { ctx, dynamic };
    let horn = ix.horn.is_some();
    let mut out = Vec::new();
    for e in 0..view.len() as Ex {
        // what the context knows: its subsumers, and in a run with the Horn rules its
        // Horn successors and loops, which the individual takes from it when it is real
        let mut mine: Option<Knows> = None;
        let mut universal_witness = false;
        let mut counted = false;
        view.with(e, &mut |st, real| {
            if real || st.bottom {
                return;
            }
            // an at-most restriction of a singular context counts the context, and R7
            // identifies it with another predecessor only where it has an instance
            counted = horn && st.extra.as_ref().is_some_and(|x| x.horn.counted);
            if !st.noms().is_empty() {
                let subs = st
                    .subs
                    .iter()
                    .copied()
                    .filter(|&s| !matches!(ix.head(s).node, Node::Conj(..)))
                    .collect();
                let (succ, loops) = if horn {
                    let succ = st
                        .extra
                        .as_ref()
                        .map_or(Vec::new(), |x| x.horn.succ.list.to_vec());
                    (succ, st.loops.clone())
                } else {
                    (Vec::new(), Vec::new())
                };
                mine = Some(Knows {
                    subs,
                    succ,
                    loops,
                    noms: st.noms().to_vec(),
                });
            }
            universal_witness = ix
                .universal
                .iter()
                .any(|&(f, x)| !universal.contains(&x) && st.subs.contains(&f));
        });
        let more = mine.is_some_and(
            |Knows {
                 subs,
                 succ,
                 loops,
                 noms,
             }| {
                noms.iter().any(|&n| {
                    let mut more = true;
                    view.with(n, &mut |st, _| {
                        more = !st.bottom
                            && (subs.iter().any(|s| !st.subs.contains(s))
                                || loops.iter().any(|l| !st.loops.contains(l))
                                || succ.iter().any(|s| {
                                    !st.extra.as_ref().is_some_and(|x| x.horn.succ.contains(s))
                                }));
                    });
                    more
                })
            },
        );
        if more || universal_witness || counted {
            out.push(e);
        }
    }
    out
}

/// The named classes whose contexts reach one of `from` through links, `from` included,
/// which are not unsatisfiable, sorted.
pub fn reaching(ix: &Index, ctx: &Contexts, dynamic: Option<&Dynamic>, from: &[Ex]) -> Vec<Ex> {
    let view = GlobalAll { ctx, dynamic };
    let mut seen: FxHashSet<Ex> = from.iter().copied().collect();
    let mut stack: Vec<Ex> = from.to_vec();
    let mut out = Vec::new();
    while let Some(e) = stack.pop() {
        let mut next = Vec::new();
        let mut keep = false;
        view.with(e, &mut |st, _| {
            keep =
                !st.bottom && (e as usize) < ctx.len() && matches!(ix.head(e).node, Node::Class(_));
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
    /// the contexts of conjunctions of the global saturation, which no world changes
    dynamic: Option<&'a Dynamic>,
    /// the contexts of conjunctions this world's Horn rules create, with ids above the
    /// global table's (F24, "The Horn rules with individuals")
    local: std::sync::Arc<Dynamic>,
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
    halt_reason: Option<Halt>,
}

/// The conclusions a world applies between checks of its bound.
const CHECK_EVERY: usize = 1024;
/// The bytes counted for a slot.
const SLOT_BYTES: usize = 64;

struct WorldSink<'b> {
    me: Ex,
    local: &'b mut Vec<Concl>,
    out: &'b mut Vec<(Ex, Concl)>,
    bound: &'b (dyn Fn(isize) -> bool + Sync),
    data_growth: &'b Cell<isize>,
    halted: &'b Cell<bool>,
    overflow: &'b Cell<bool>,
    ctx_len: usize,
    dynamic: Option<&'b Dynamic>,
    table: std::sync::Arc<Dynamic>,
    /// the messages in the buffers that the bound has been charged for, and the bytes of
    /// that charge, which a checkpoint folds into what the world reported and the
    /// activation's end releases
    grown: &'b Cell<usize>,
    charged: &'b Cell<isize>,
}

/// The messages a world's sink buffers between two charges of their bytes.
const CHARGE_EVERY: usize = 1024;

impl WorldSink<'_> {
    /// A message was pushed: once the buffers hold [`CHARGE_EVERY`] messages more than
    /// the bound was charged for, the growth is charged, so that a rule whose one
    /// inference sends many messages, such as R7 of the Horn rules with individuals, is
    /// admitted as it grows. The charge follows the live buffers, not the messages sent.
    fn grew(&self) {
        let live = self.local.len() + self.out.len();
        let base = self.grown.get();
        if live >= base + CHARGE_EVERY {
            let bytes = ((live - base) * std::mem::size_of::<(Ex, Concl)>()) as isize;
            self.grown.set(live);
            self.charged.set(self.charged.get() + bytes);
            if !(self.bound)(bytes) {
                self.halted.set(true);
            }
        }
    }
}

impl Sink for WorldSink<'_> {
    fn reserve_data(&mut self, bytes: usize) -> bool {
        if bytes > isize::MAX as usize {
            self.halted.set(true);
            self.overflow.set(true);
            return false;
        }
        self.data_growth
            .set(self.data_growth.get() + bytes as isize);
        let ok = (self.bound)(bytes as isize);
        self.halted.set(self.halted.get() || !ok);
        ok
    }
    fn data_overflow(&mut self) {
        self.halted.set(true);
        self.overflow.set(true);
    }
    fn release_data(&mut self, bytes: usize) {
        self.data_growth
            .set(self.data_growth.get() - bytes as isize);
        (self.bound)(-(bytes as isize));
    }
    fn go_on(&self) -> bool {
        !self.halted.get() && (self.bound)(0)
    }
    fn stopped(&self) -> bool {
        self.halted.get()
    }

    fn send(&mut self, to: Ex, c: Concl) {
        // a halted world's answer is discarded, so its sink keeps nothing more
        if self.halted.get() {
            return;
        }
        if to == self.me && c != Concl::Real {
            self.local.push(c);
        } else {
            self.out.push((to, c));
        }
        self.grew();
    }
    fn me(&mut self, c: Concl) {
        if self.halted.get() {
            return;
        }
        self.local.push(c);
        self.grew();
    }

    fn core(&self, ix: &Index, e: Ex, out: &mut Vec<Ex>) {
        out.clear();
        if (e as usize) < self.ctx_len {
            flatten(ix, e, out);
        } else if let Some(d) = self.dynamic
            && d.holds(e)
        {
            out.extend_from_slice(d.core(e));
        } else {
            out.extend_from_slice(self.table.core(e));
        }
    }

    fn intern(&mut self, core: &[Ex]) -> Ex {
        // the global table first, which no world changes, then the world's own
        match self.dynamic.and_then(|d| d.find(core)) {
            Some(e) => e,
            None => self.table.intern(core),
        }
    }
}

impl World<'_> {
    /// The global state with the global conjunction contexts.
    fn global(&self) -> GlobalAll<'_> {
        GlobalAll {
            ctx: self.ctx,
            dynamic: self.dynamic,
        }
    }

    /// The core of a conjunction context, global or the world's own, or none for a
    /// context of the index.
    fn conjunction_core(&self, e: Ex) -> Option<&[Ex]> {
        if (e as usize) < self.ctx.len() {
            None
        } else if let Some(d) = self.dynamic
            && d.holds(e)
        {
            Some(d.core(e))
        } else {
            Some(self.local.core(e))
        }
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
                self.global().with(e, &mut |st, r| f(st, real || r));
            }
            None => self.global().with(e, f),
        }
    }
    fn len(&self) -> usize {
        self.local.base() as usize + self.local.len()
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
        dynamic: Option<&'a Dynamic>,
        global_universal: &'a [Ex],
        global_real: &'a [Ex],
        bound: &'a (dyn Fn(isize) -> bool + Sync),
    ) -> World<'a> {
        let base = ctx.len() + dynamic.map_or(0, |d| d.len());
        World {
            ix,
            ctx,
            dynamic,
            local: std::sync::Arc::new(Dynamic::new(base as Ex)),
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
            halt_reason: None,
        }
    }

    /// Tells the bound what the world holds now, its waiting messages included, and
    /// stops the world when the bound refuses. `busy` counts the messages in the buffers
    /// of the context being processed.
    fn checkpoint(&mut self, busy: usize) {
        let used = self.held
            + self.local.bytes()
            + (self.waiting + self.pending.len() + busy) * std::mem::size_of::<(Ex, Concl)>();
        let delta = used as isize - self.reported as isize;
        self.reported = used;
        if !(self.bound)(delta) {
            self.halted = true;
        }
    }

    fn global_real(&self, e: Ex) -> bool {
        let mut r = false;
        self.global().with(e, &mut |st, _| r = st.real);
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
            self.global().with(to, &mut |st, _| held = st.holds(&c));
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
            let global = GlobalAll {
                ctx: self.ctx,
                dynamic: self.dynamic,
            };
            let horn = ix.horn.is_some();
            global.with(e, &mut |st, _| {
                init = true;
                for &s in &st.subs {
                    if let Node::Exist(..) = ix.head(s).node {
                        out.push((ix.head(s).succ, Concl::Real));
                        if horn {
                            out.push((ix.head(s).succ, Concl::RealBack(e)));
                        }
                    }
                }
                // the successors the Horn rules made, which learn that a predecessor is
                // real, and the individuals the context stands for, which take its loops
                // and Horn successors (roadmap item 692)
                if let Some(x) = &st.extra
                    && horn
                {
                    for &(_, n) in &x.horn.succ.list {
                        out.push((n, Concl::Real));
                        out.push((n, Concl::RealBack(e)));
                    }
                    for &n in &x.noms {
                        for &l in &st.loops {
                            out.push((n, Concl::Loop(l)));
                        }
                        for &(r, n2) in &x.horn.succ.list {
                            out.push((n, Concl::Succ(r, n2)));
                        }
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
                    let global = GlobalAll {
                        ctx: self.ctx,
                        dynamic: self.dynamic,
                    };
                    global.with(e, &mut |st, _| {
                        let Some(bytes) = CONTEXT_BYTES
                            .checked_add(st.bytes())
                            .filter(|&n| n <= isize::MAX as usize)
                        else {
                            self.halted = true;
                            self.halt_reason = Some(Halt::Memory(usize::MAX));
                            return;
                        };
                        // A clone shares numeric payload, but owns every state container.
                        self.reported += bytes;
                        if !(self.bound)(bytes as isize) {
                            self.halted = true;
                            return;
                        }
                        self.held += bytes;
                        copy = Some(st.clone());
                    });
                    if self.halted {
                        return;
                    }
                    let copied = copy.is_some();
                    let mut st = copy.unwrap_or_default();
                    // the world made it real from the global state already
                    st.real |= real && st.init;
                    if !copied {
                        self.held += CONTEXT_BYTES;
                    }
                    self.checkpoint(todo.len());
                    st
                }
            };
            let mut local: Vec<Concl> = Vec::new();
            let mut out: Vec<(Ex, Concl)> = Vec::new();
            let data_growth = Cell::new(0isize);
            let data_halted = Cell::new(false);
            let data_overflow = Cell::new(false);
            // the initialization's output is metered from zero, and the incoming
            // messages, which were reported as waiting, raise the mark when they join
            // the buffers below
            let grown = Cell::new(0usize);
            let charged = Cell::new(0isize);
            {
                let core = self.conjunction_core(e).map(|c| c.to_vec());
                let mut sink = WorldSink {
                    me: e,
                    local: &mut local,
                    out: &mut out,
                    bound: self.bound,
                    data_growth: &data_growth,
                    halted: &data_halted,
                    overflow: &data_overflow,
                    ctx_len: self.ctx.len(),
                    dynamic: self.dynamic,
                    table: self.local.clone(),
                    grown: &grown,
                    charged: &charged,
                };
                if !st.init {
                    let all: Vec<Ex> = self
                        .global_universal
                        .iter()
                        .chain(&self.universal)
                        .copied()
                        .collect();
                    match &core {
                        Some(core) => init_conjunction(ix, core, &mut st, &all, &mut sink),
                        None => init(ix, e, &mut st, &all, &mut sink),
                    }
                }
                todo.reverse();
                grown.set(grown.get() + todo.len());
                sink.local.append(&mut todo);
                let horn = ix.horn.is_some();
                loop {
                    while let Some(c) = sink.local.pop() {
                        if apply(ix, e, &mut st, c, &mut sink) {
                            self.held += CONCLUSION_BYTES;
                        }
                        self.applied += 1;
                        if data_halted.get() {
                            break;
                        }
                        if self.applied.is_multiple_of(CHECK_EVERY) {
                            // the sink's charge for the buffers counts as reported, so
                            // that the checkpoint charges the buffers once
                            self.reported = self.reported.saturating_add_signed(charged.get());
                            charged.set(0);
                            grown.set(sink.local.len() + sink.out.len());
                            self.checkpoint(sink.local.len() + sink.out.len());
                            if self.halted {
                                break;
                            }
                        }
                    }
                    // R3 and R5 of the Horn rules wait for the activation's universals
                    // and at-most restrictions, as in the saturation (roadmap item 692)
                    if data_halted.get()
                        || self.halted
                        || !horn
                        || !super::horn_rules::flush(ix, &mut st, &mut sink)
                    {
                        break;
                    }
                }
            }
            // the messages charged as they were pushed are released here, and the
            // checkpoint below counts those still waiting
            if charged.get() != 0 {
                (self.bound)(-charged.get());
            }
            self.held = self.held.saturating_add_signed(data_growth.get());
            self.reported = self.reported.saturating_add_signed(data_growth.get());
            self.halted |= data_halted.get();
            if data_overflow.get() {
                self.halt_reason = Some(Halt::Memory(usize::MAX));
            }
            let slot = self.slots.get_mut(&e).expect("slot");
            slot.st = Some(st);
            // the messages the context sent count until they are delivered, and once
            // they wait in their contexts' to-do lists
            self.checkpoint(out.len());
            self.pending.extend(out);
            self.deliver();
        }
    }

    /// Classifies `class` in its world: the class has an instance, and the world
    /// saturates what follows, with the facts of universal roles and the merges of keys
    /// that hold there.
    pub fn classify(mut self, class: Ex) -> Result<Found, Halt> {
        self.pending.push((class, Concl::Real));
        loop {
            self.drain();
            self.checkpoint(0);
            if self.halted {
                return Err(self.halt_reason.unwrap_or(Halt::Stopped));
            }
            let known: Vec<Ex> = self
                .global_universal
                .iter()
                .chain(&self.universal)
                .copied()
                .collect();
            let facts = universal_facts(self.ix, &self, &known);
            let merges = key_merges(self.ix, &self, self.bound)?;
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
            for &(a, b) in merges.iter() {
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
        Ok(found)
    }
}

impl Drop for World<'_> {
    fn drop(&mut self) {
        (self.bound)(-(self.reported as isize));
    }
}

#[cfg(test)]
mod numeric_resource_tests {
    use super::*;
    use crate::classify::data::{Known, Value};
    use crate::classify::numeric::{Number, tests::measure};
    use crate::classify::{Limits, Saturation};
    use std::sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicIsize, AtomicUsize, Ordering},
    };

    fn key_workspace_fixture(
        individuals: usize,
        edges: usize,
        equal: bool,
    ) -> Vec<crate::model::Axiom> {
        let other = if equal { "7.0" } else { "8.0" };
        let mut text = format!(
            "Prefix(:=<urn:key-workspace:>) Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>) Ontology(HasKey(:K () (:d)) ClassAssertion(:K :a) ClassAssertion(:K :b) DataPropertyAssertion(:d :a \"7\"^^xsd:integer) DataPropertyAssertion(:d :b \"{other}\"^^xsd:decimal) "
        );
        for i in 0..individuals {
            text.push_str(&format!("ClassAssertion(:U :u{i}) "));
        }
        for i in 0..edges {
            text.push_str(&format!(
                "ObjectPropertyAssertion(:unrelated :u{} :u{}) ",
                i % individuals,
                (i + 1) % individuals
            ));
        }
        text.push(')');
        crate::ofn_read::parse(&text).unwrap().axioms
    }

    #[test]
    fn key_workspace_ignores_unrelated_state() {
        for equal in [false, true] {
            let mut reference = None;
            for individuals in [16, 64, 256] {
                for edges in [0, 64, 256] {
                    let axioms = key_workspace_fixture(individuals, edges, equal);
                    let refs: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
                    let mut sat = Saturation::new();
                    let classified = sat.classify(&refs, None, &Limits::default()).unwrap();
                    assert!(classified.complete && sat.excluded().is_empty());
                    let held = AtomicIsize::new(0);
                    let peak = AtomicUsize::new(0);
                    let requests = StdMutex::new([0usize; 32]);
                    let count = AtomicUsize::new(0);
                    let bound = |delta: isize| {
                        held.fetch_add(delta, Ordering::Relaxed);
                        assert!(held.load(Ordering::Relaxed) >= 0);
                        peak.fetch_max(held.load(Ordering::Relaxed) as usize, Ordering::Relaxed);
                        if delta > 0 {
                            let mut all = requests.lock().unwrap();
                            all[count.load(Ordering::Relaxed)] = delta as usize;

                            count.fetch_add(1, Ordering::Relaxed);
                        }
                        true
                    };
                    let ix = sat.index.as_ref().unwrap();
                    let (merges, allocated) = measure(|| key_merges(ix, &Global(&sat.ctx), &bound));
                    let merges = merges.unwrap();
                    assert!(
                        merges.is_empty(),
                        "already saturated keys must have no remaining merges"
                    );
                    assert!(
                        allocated.peak <= peak.load(Ordering::Relaxed),
                        "requested allocations exceeded live admitted scratch"
                    );
                    drop(merges);
                    assert_eq!(held.load(Ordering::Relaxed), 0);
                    let actual = (
                        *requests.lock().unwrap(),
                        count.load(Ordering::Relaxed),
                        peak.load(Ordering::Relaxed),
                    );
                    eprintln!(
                        "key workspace equal={equal} unrelated={individuals}/{edges}: requests={:?} peak={} requested_allocation_peak={}",
                        &actual.0[..actual.1],
                        actual.2,
                        allocated.peak
                    );
                    if let Some(expected) = reference {
                        assert_eq!(
                            actual, expected,
                            "unrelated state changed actual key workspace admission"
                        );
                    }
                    reference = Some(actual);
                }
            }
            assert!(
                reference.unwrap().2 < 64 << 10,
                "two-member key scratch must fit the declared local64KiB ceiling"
            );
        }
    }

    fn owner_axioms(body: &str) -> Vec<crate::model::Axiom> {
        crate::ofn_read::parse(&format!(
            "Prefix(:=<urn:key-owner:>) Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>) Prefix(owl:=<http://www.w3.org/2002/07/owl#>) Ontology({body})"
        )).unwrap().axioms
    }

    type FixtureLedger = Option<(
        std::sync::Weak<crate::classify::numeric::NumericLedger>,
        usize,
    )>;

    fn fixture_ledger(sat: &Saturation, stage: &str) -> FixtureLedger {
        let actual = &sat.index.as_ref().unwrap().numeric_ledger;
        let interner = &sat.interner.ranges.ledger;
        eprintln!(
            "key fixture ledger stage={stage}: index={:?} interner={:?}",
            actual.as_ref().map(|x| (Arc::as_ptr(x), x.bytes())),
            interner.as_ref().map(|x| (Arc::as_ptr(x), x.bytes()))
        );
        match (actual, interner) {
            (None, None) => None,
            (Some(a), Some(b)) => {
                assert!(
                    Arc::ptr_eq(a, b),
                    "{stage}: actual Index/interner ledger roots differ"
                );
                let weak = Arc::downgrade(a);
                assert!(
                    weak.upgrade().is_some(),
                    "{stage}: actual ledger is not live"
                );
                Some((weak, a.bytes()))
            }
            _ => panic!("{stage}: actual Index/interner ledger presence mismatch"),
        }
    }

    fn check_fixture_ledger(
        sat: &Saturation,
        expected: &FixtureLedger,
        stage: &str,
        settled: bool,
    ) {
        match (expected, fixture_ledger(sat, stage)) {
            (None, None) => {}
            (Some((a, bytes)), Some((b, now))) => {
                assert!(
                    a.ptr_eq(&b),
                    "{stage}: reference construction changed the ledger root"
                );
                assert!(
                    a.upgrade().is_some(),
                    "{stage}: retained ledger is not live"
                );
                if settled {
                    assert_eq!(
                        *bytes, now,
                        "{stage}: temporary reference payload did not balance"
                    );
                }
            }
            _ => panic!("{stage}: reference construction changed ledger presence"),
        }
    }

    /// Preserve production key-role indexing and Real seeding while an empty class
    /// prevents the helper from consuming the original key's merge before measurement.
    fn before_keys(body: &str) -> Saturation {
        use crate::model::{AxiomKind, Class, ClassExpression, Entity};
        let original = owner_axioms(body);
        let empty = Class::new("urn:key-owner-fixture:Empty");
        assert!(
            original.iter().all(|a| a
                .signature()
                .iter()
                .all(|e| e.iri().as_str() != empty.as_str())),
            "fixture empty class must be fresh in the entire original signature"
        );
        let nonkey_signature = original
            .iter()
            .filter(|a| !matches!(a.kind, AxiomKind::HasKey(..)))
            .flat_map(|a| a.signature())
            .collect::<std::collections::BTreeSet<_>>();
        let mut key_count = 0;
        for a in &original {
            if let AxiomKind::HasKey(class, objects, data) = &a.kind {
                let ClassExpression::Class(class) = class else {
                    panic!("fixture requires atomic named key classes")
                };
                assert!(*class != Class::thing() && *class != Class::nothing());
                assert!(
                    nonkey_signature.contains(&Entity::Class(class.clone())),
                    "fixture original key class must already occur in nonkey input"
                );
                assert!(!objects.is_empty() || !data.is_empty());
                key_count += 1;
            }
        }
        assert!(key_count > 0);
        let empty_axiom = AxiomKind::SubClassOf(empty.clone().into(), Class::nothing().into());
        let mut shadow = original.clone();
        for a in &mut shadow {
            if let AxiomKind::HasKey(class, _, _) = &mut a.kind {
                *class = empty.clone().into();
            }
        }
        shadow.push(empty_axiom.clone().into());
        let refs = shadow.iter().map(|a| &a.kind).collect::<Vec<_>>();
        let mut sat = Saturation::new();
        let result = sat.classify(&refs, None, &Limits::default()).unwrap();
        assert!(result.complete && result.consistent && sat.excluded().is_empty());
        let empty_id = sat.interner.class_ex(&empty).unwrap();
        assert!(result.taxonomy.is_unsatisfiable(&empty));
        let ix = sat.index.as_ref().unwrap();
        assert_eq!(ix.keys.len(), key_count);
        for key in &ix.keys {
            assert_eq!(key.class, empty_id);
            assert_eq!(
                key_member_count(ix, &Global(&sat.ctx), key.class, &|_| true).unwrap(),
                0,
                "shadow key has a real nonbottom member"
            );
        }
        let a_name = crate::model::NamedIndividual::new("urn:key-owner:a");
        let b_name = crate::model::NamedIndividual::new("urn:key-owner:b");
        assert!(
            sat.interner.individuals.get(&a_name).is_some()
                && sat.interner.individuals.get(&b_name).is_some()
        );
        let a = sat.interner.nominal(&a_name);
        let b = sat.interner.nominal(&b_name);
        for (from, other) in [(a, b), (b, a)] {
            let mut seen = false;
            Global(&sat.ctx).with(from, &mut |st, real| {
                seen = true;
                assert!(
                    real && !st.bottom,
                    "shadow fixture must run production Real seeding"
                );
                assert!(
                    !st.subs.contains(&other),
                    "shadow fixture premerged the measured pair"
                );
            });
            assert!(seen, "shadow fixture lacks the measured nominal context");
        }
        // Inspect the actual production roots before constructing any reference owner.
        let ledger_before = fixture_ledger(&sat, "before reference");
        {
            let snapshot = (
                sat.interner.nodes.clone(),
                sat.interner.roles.clone(),
                sat.interner.vals.clone(),
                sat.interner.met.clone(),
            );
            let unchanged = |it: &crate::classify::index::Interner, stage| {
                assert_eq!(it.nodes, snapshot.0, "{stage}: fixture nodes grew");
                assert_eq!(it.roles, snapshot.1, "{stage}: fixture roles grew");
                assert_eq!(it.vals, snapshot.2, "{stage}: fixture values changed");
                assert_eq!(
                    it.met, snapshot.3,
                    "{stage}: fixture signature flags changed"
                );
            };
            let mut normalized = crate::classify::index::Axioms::default();
            for ax in original
                .iter()
                .map(|a| &a.kind)
                .chain(std::iter::once(&empty_axiom))
            {
                assert!(
                    crate::classify::index::normalize(ax, &mut sat.interner, &mut normalized)
                        .is_none(),
                    "original reference fixture needs unsupported normalization"
                );
            }
            assert!(normalized.horn.is_none() && normalized.dl.is_none());
            unchanged(&sat.interner, "after original normalization");
            check_fixture_ledger(&sat, &ledger_before, "after original normalization", false);
            let reference = Index::build(&normalized, &mut sat.interner, None, &|| false).unwrap();
            unchanged(&sat.interner, "after reference Index build");
            check_fixture_ledger(&sat, &ledger_before, "after reference Index build", false);
            let ix = sat.index.as_mut().unwrap();
            assert_eq!(ix.keys.len(), normalized.keys.len());
            for (shadow, actual) in ix.keys.iter().zip(&normalized.keys) {
                assert_eq!(shadow.class, empty_id);
                assert!(
                    matches!(sat.interner.nodes[actual.class as usize], Node::Class(_)),
                    "restored key must have an already indexed atomic class"
                );
                assert_eq!(
                    shadow.objects, actual.objects,
                    "shadow changed original object roles"
                );
                assert_eq!(
                    shadow.data, actual.data,
                    "shadow changed original data roles"
                );
            }
            ix.keys = normalized.keys.clone();
            ix.assert_key_fixture_equivalent(&reference);
            for name in [
                crate::classify::index::RoleName::Object(crate::model::ObjectProperty::top()),
                crate::classify::index::RoleName::Data(crate::model::DataProperty::top()),
            ] {
                if let Some(role) = sat.interner.role_id(&name) {
                    let which =
                        usize::from(matches!(name, crate::classify::index::RoleName::Data(_)));
                    assert_eq!(
                        ix.tops[which],
                        Some(role),
                        "original top key role was not indexed"
                    );
                    assert!(
                        ix.is_universal(role),
                        "original top key role lacks universal closure"
                    );
                }
            }
            // Drop reference/normalization/value snapshots before owner measurement.
        }
        check_fixture_ledger(&sat, &ledger_before, "after reference owners dropped", true);
        assert_two_real_members(sat.index.as_ref().unwrap(), &Global(&sat.ctx));
        sat
    }
    fn assert_two_real_members(ix: &Index, view: &dyn View) {
        assert!(!ix.keys.is_empty(), "fixture must install its actual keys");
        for key in &ix.keys {
            for &role in key.objects.iter().chain(&key.data) {
                let r = role as usize;
                assert!(
                    r < ix.roles.sup.len()
                        && r < ix.roles.range.len()
                        && r < ix.roles.data_range.len()
                        && r < ix.roles.functional.len()
                        && (ix.roles.inv.is_empty() || r < ix.roles.inv.len()),
                    "key role {role} was not built into the production Index"
                );
                assert!(
                    ix.roles.sup[r].binary_search(&role).is_ok(),
                    "key role lacks self closure"
                );
                if let Some(inverse) = ix.roles.inv(role) {
                    assert!(
                        (inverse as usize) < ix.roles.sup.len(),
                        "key inverse role is out of range"
                    );
                }
            }
            for &top in ix.tops.iter().flatten() {
                assert!(
                    ix.is_universal(top),
                    "indexed top role lacks universal recognition"
                );
            }
            let mut members = 0;
            for &individual in &ix.nominals {
                view.with(individual, &mut |st, real| {
                    if !st.bottom && st.subs.contains(&key.class) {
                        assert!(matches!(ix.head(individual).node, Node::Nominal(_)));
                        assert!(real, "key fixture member lacks the production Real stage");
                        members += 1;
                    }
                });
            }
            assert_eq!(
                members, 2,
                "fixture must expose exactly two real nonbottom key members"
            );
        }
    }
    const OWNER: &str = "ClassAssertion(:K :a) ClassAssertion(:K :b) ObjectPropertyAssertion(:r :a :target) ObjectPropertyAssertion(:r :b :target) DataPropertyAssertion(:d :a \"7\"^^xsd:integer) DataPropertyAssertion(:d :b \"7.0\"^^xsd:decimal) HasKey(:K (:r) (:d))";

    #[test]
    fn key_universal_role_merges_through_full_classifier() {
        let common = "ClassAssertion(:K :a) ClassAssertion(:K :b) ";
        let body = format!(
            "{common} Declaration(ObjectProperty(owl:topObjectProperty)) HasKey(:K (owl:topObjectProperty) ())"
        );
        for threads in [1, 8] {
            // The exact original body uses the full production path, with no fixture helper.
            let axioms = owner_axioms(&body);
            let refs = axioms.iter().map(|a| &a.kind).collect::<Vec<_>>();
            let limits = Limits {
                threads,
                ..Limits::default()
            };
            let mut sat = Saturation::new();
            let result = sat.classify(&refs, None, &limits).unwrap();
            assert!(result.complete && result.consistent && sat.excluded().is_empty());
            assert_eq!(result.consistency, crate::classify::Consistency::Consistent);
            let a = sat
                .interner
                .nominal(&crate::model::NamedIndividual::new("urn:key-owner:a"));
            let b = sat
                .interner
                .nominal(&crate::model::NamedIndividual::new("urn:key-owner:b"));
            let ix = sat.index.as_ref().unwrap();
            let top = sat
                .interner
                .role_id(&crate::classify::index::RoleName::Object(
                    crate::model::ObjectProperty::top(),
                ))
                .unwrap();
            assert_eq!(ix.tops[0], Some(top));
            assert!(ix.is_universal(top));
            assert_two_real_members(ix, &Global(&sat.ctx));
            for (from, other) in [(a, b), (b, a)] {
                let mut merged = false;
                Global(&sat.ctx).with(from, &mut |st, real| {
                    merged = real && !st.bottom && st.subs.contains(&other);
                });
                assert!(
                    merged,
                    "full production universal key did not merge the named pair, threads={threads}"
                );
            }
            let different = owner_axioms(&format!("{body} DifferentIndividuals(:a :b)"));
            let mut classifier = crate::classify::Classifier::of(&different);
            let result = classifier.classify(&limits).unwrap();
            assert!(result.complete && !result.consistent && classifier.excluded().is_empty());
            assert_eq!(
                result.consistency,
                crate::classify::Consistency::Inconsistent
            );
        }
    }

    #[test]
    fn key_workspace_preserves_role_value_and_named_witness_semantics() {
        let common = "ClassAssertion(:K :a) ClassAssertion(:K :b) ";
        // Keep the original invalid input as a strict structural-refusal control:
        // OWL 2 Structural Specification 9.5 requires at least one key property.
        let invalid_body = format!("{common} HasKey(:K () ())");
        let invalid = format!(
            "Prefix(:=<urn:key-owner:>) Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>) Prefix(owl:=<http://www.w3.org/2002/07/owl#>) Ontology(SameIndividual(:RealSeed :RealSeedAlias) {invalid_body})"
        );
        let refused = crate::ofn_read::parse(&invalid).unwrap_err();
        assert_eq!(refused.message, "HasKey needs at least 1 operands, not 0");
        for (body, merges) in [
            (OWNER.to_owned(), true),
            (
                format!(
                    "{common} Declaration(ObjectProperty(owl:topObjectProperty)) HasKey(:K (owl:topObjectProperty) ())"
                ),
                true,
            ),
            (
                format!(
                    "{common} SubObjectPropertyOf(:s :r) ObjectPropertyAssertion(:s :a :target) ObjectPropertyAssertion(:r :b :target) HasKey(:K (:r) ())"
                ),
                true,
            ),
            (
                format!(
                    "{common} ObjectPropertyAssertion(:r :a :ta) ObjectPropertyAssertion(:r :b :tb) SameIndividual(:ta :tb) HasKey(:K (:r) ())"
                ),
                true,
            ),
            (
                format!(
                    "{common} ObjectPropertyAssertion(:r :a :ta) ObjectPropertyAssertion(:r :b :tb) HasKey(:K (:r) ())"
                ),
                false,
            ),
            (
                format!(
                    "{common} ObjectPropertyAssertion(:r :a :t) ObjectPropertyAssertion(:r :b :t) ObjectPropertyAssertion(:s :a :ta) ObjectPropertyAssertion(:s :b :tb) HasKey(:K (:r :s) ())"
                ),
                false,
            ),
            (
                format!(
                    "{common} DataPropertyAssertion(:d :a \"7\"^^xsd:integer) DataPropertyAssertion(:d :b \"7.0\"^^xsd:decimal) DataPropertyAssertion(:e :a \"9\"^^xsd:integer) DataPropertyAssertion(:e :b \"8\"^^xsd:integer) HasKey(:K () (:d :e))"
                ),
                false,
            ),
            (
                format!(
                    "{common} DataPropertyAssertion(:d :a \"7\"^^xsd:integer) DataPropertyAssertion(:d :b \"8\"^^xsd:integer) HasKey(:K () (:d))"
                ),
                false,
            ),
            (
                format!(
                    "{common} DataPropertyAssertion(:d :a \"shared\"^^xsd:string) DataPropertyAssertion(:d :b \"shared\"^^xsd:string) HasKey(:K () (:d)) HasKey(:K () (:d))"
                ),
                true,
            ),
        ] {
            let mut sat = before_keys(&body);
            let a = sat
                .interner
                .nominal(&crate::model::NamedIndividual::new("urn:key-owner:a"));
            let b = sat
                .interner
                .nominal(&crate::model::NamedIndividual::new("urn:key-owner:b"));
            let held = AtomicIsize::new(0);
            let bound = |delta: isize| {
                held.fetch_add(delta, Ordering::Relaxed);
                true
            };
            let ix = sat.index.as_ref().unwrap();
            for conditional in [false, true] {
                let global = Global(&sat.ctx);
                let world = World::new(ix, &sat.ctx, None, &[], &[], &bound);
                let view: &dyn View = if conditional { &world } else { &global };
                assert_two_real_members(ix, view);
                let found = key_merges(ix, view, &bound).unwrap();
                assert_eq!(
                    found.len(),
                    usize::from(merges),
                    "{body}, conditional={conditional}"
                );
                if merges {
                    assert!(found[0] == (a, b) || found[0] == (b, a));
                }
                drop(found);
                assert_eq!(held.load(Ordering::Relaxed), 0);
            }
        }
    }

    #[test]
    fn key_workspace_each_owner_refuses_before_its_allocation_and_releases() {
        let sat = before_keys(OWNER);
        let ix = sat.index.as_ref().unwrap();
        let held = AtomicIsize::new(0);
        let positive = AtomicUsize::new(0);
        let admitted = |delta: isize| {
            held.fetch_add(delta, Ordering::Relaxed);
            if delta > 0 {
                positive.fetch_add(1, Ordering::Relaxed);
            }
            true
        };
        assert_two_real_members(ix, &Global(&sat.ctx));
        drop(key_merges(ix, &Global(&sat.ctx), &admitted).unwrap());
        let owners = positive.load(Ordering::Relaxed);
        assert_eq!(
            owners, 9,
            "counting members, sources, objects and data, then returned pairs and the same four"
        );
        assert_eq!(held.load(Ordering::Relaxed), 0);
        for deny in 1..=owners {
            let grants = AtomicUsize::new(0);
            let reject = |delta: isize| {
                held.fetch_add(delta, Ordering::Relaxed);
                if delta > 0 {
                    grants.fetch_add(1, Ordering::Relaxed);
                }
                delta <= 0 || grants.load(Ordering::Relaxed) != deny
            };
            assert_two_real_members(ix, &Global(&sat.ctx));
            let (result, allocated) = measure(|| key_merges(ix, &Global(&sat.ctx), &reject));
            assert!(matches!(result, Err(Halt::Stopped)));
            assert_eq!(
                allocated.alloc_events,
                deny - 1,
                "denied owner must not allocate"
            );
            assert_eq!(allocated.realloc_events, 0);
            assert_eq!(allocated.live, 0);
            assert_eq!(held.load(Ordering::Relaxed), 0);
        }
        assert_two_real_members(ix, &Global(&sat.ctx));
        assert_eq!(
            key_merges(ix, &Global(&sat.ctx), &admitted).unwrap().len(),
            1
        );
        assert_eq!(held.load(Ordering::Relaxed), 0);
        assert_two_real_members(ix, &Global(&sat.ctx));
        let cancelled_bound = |_| false;
        let (cancelled, allocated) =
            measure(|| key_merges(ix, &Global(&sat.ctx), &cancelled_bound));
        assert!(matches!(cancelled, Err(Halt::Stopped)));
        assert_eq!(allocated.events, 0);
    }

    #[test]
    fn key_workspace_accounts_string_copies_and_relevant_growth() {
        let value = "s".repeat(100_000);
        let body = format!(
            "ClassAssertion(:K :a) ClassAssertion(:K :b) DataPropertyAssertion(:d :a \"{value}\"^^xsd:string) DataPropertyAssertion(:d :b \"{value}\"^^xsd:string) HasKey(:K () (:d))"
        );
        let sat = before_keys(&body);
        let ix = sat.index.as_ref().unwrap();
        let held = AtomicIsize::new(0);
        let peak = AtomicUsize::new(0);
        let bound = |delta: isize| {
            held.fetch_add(delta, Ordering::Relaxed);
            peak.fetch_max(
                held.load(Ordering::Relaxed).max(0) as usize,
                Ordering::Relaxed,
            );
            true
        };
        assert_two_real_members(ix, &Global(&sat.ctx));
        let (found, allocations) = measure(|| key_merges(ix, &Global(&sat.ctx), &bound));
        let found = found.unwrap();
        assert_eq!(found.len(), 1);
        assert!(peak.load(Ordering::Relaxed) >= 2 * value.len());
        assert!(allocations.peak <= peak.load(Ordering::Relaxed));
        drop(found);
        assert_eq!(held.load(Ordering::Relaxed), 0);
        let capped = |delta: isize| {
            held.fetch_add(delta, Ordering::Relaxed);
            delta <= 0 || held.load(Ordering::Relaxed) <= 64 << 10
        };
        assert_two_real_members(ix, &Global(&sat.ctx));
        let (refused, allocations) = measure(|| key_merges(ix, &Global(&sat.ctx), &capped));
        assert!(matches!(refused, Err(Halt::Stopped)));
        assert!(
            allocations.requested_alloc_bytes < value.len(),
            "refused string payload must not be copied"
        );
        assert_eq!(held.load(Ordering::Relaxed), 0);
        assert_two_real_members(ix, &Global(&sat.ctx));
        assert_eq!(key_merges(ix, &Global(&sat.ctx), &bound).unwrap().len(), 1);
        assert_eq!(held.load(Ordering::Relaxed), 0);
    }

    fn classified_keys(body: &str) -> Saturation {
        let axioms = owner_axioms(body);
        let refs: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
        let mut sat = Saturation::new();
        let out = sat.classify(&refs, None, &Limits::default()).unwrap();
        assert!(out.complete && out.consistent && sat.excluded().is_empty());
        assert!(!sat.index.as_ref().unwrap().keys.is_empty());
        sat
    }

    #[test]
    fn key_pairs_are_admitted_for_found_merges_not_all_member_pairs() {
        // Many key members without any key value merge nothing, so no storage should
        // grow with the square of the membership.
        let n = 1500;
        let mut body = String::from("ClassAssertion(:K :a) ClassAssertion(:K :b) ");
        for i in 0..n {
            body.push_str(&format!("ClassAssertion(:K :i{i}) "));
        }
        body.push_str("HasKey(:K () (:d))");
        let sat = classified_keys(&body);
        let ix = sat.index.as_ref().unwrap();
        let largest = AtomicUsize::new(0);
        let held = AtomicIsize::new(0);
        let bound = |delta: isize| {
            held.fetch_add(delta, Ordering::Relaxed);
            largest.fetch_max(delta.max(0) as usize, Ordering::Relaxed);
            true
        };
        let found = key_merges(ix, &Global(&sat.ctx), &bound).unwrap();
        assert!(found.is_empty());
        drop(found);
        assert_eq!(held.load(Ordering::Relaxed), 0);
        let largest = largest.load(Ordering::Relaxed);
        assert!(
            largest < 64 * (n + 2),
            "one admission of {largest} bytes for {n} members without key values"
        );
    }

    #[test]
    fn key_matching_reads_only_the_records_of_each_pair() {
        // Distinct single-valued keys: every pair is compared and none merges. The
        // work per pair is constant, so doubling the members about quadruples the
        // polls, where a scan of every record per pair would make them eightfold.
        let polls = |n: usize| {
            let mut body = String::from(
                "ClassAssertion(:K :a) ClassAssertion(:K :b) DataPropertyAssertion(:d :a \"-1\"^^xsd:integer) DataPropertyAssertion(:d :b \"-2\"^^xsd:integer) ",
            );
            for i in 0..n {
                body.push_str(&format!(
                    "ClassAssertion(:K :i{i}) DataPropertyAssertion(:d :i{i} \"{i}\"^^xsd:integer) "
                ));
            }
            body.push_str("HasKey(:K () (:d))");
            let sat = classified_keys(&body);
            let ix = sat.index.as_ref().unwrap();
            let polls = AtomicUsize::new(0);
            let bound = |delta: isize| {
                if delta == 0 {
                    polls.fetch_add(1, Ordering::Relaxed);
                }
                true
            };
            assert!(
                key_merges(ix, &Global(&sat.ctx), &bound)
                    .unwrap()
                    .is_empty()
            );
            polls.load(Ordering::Relaxed)
        };
        let (small, large) = (polls(100), polls(200));
        assert!(
            large * 10 < small * 55,
            "polls grew from {small} to {large} when the members doubled"
        );
    }

    #[test]
    fn key_workspace_overflow_is_an_allocation_free_refusal() {
        let overflow_bound = |_| panic!("overflow must precede admission");
        let (result, allocated) = measure(|| KeyStorage::<Ex>::new(usize::MAX, &overflow_bound));
        assert!(matches!(result, Err(Halt::Memory(usize::MAX))));
        assert_eq!(allocated.events, 0);
        let (aggregate, allocated) =
            measure(|| KeyStorage::<Ex>::with_live(1, 0, isize::MAX as usize, &overflow_bound));
        assert!(matches!(aggregate, Err(Halt::Memory(usize::MAX))));
        assert_eq!(allocated.events, 0);
        assert!(key_live(&[isize::MAX as usize, 1]).is_err());
    }

    fn fixture() -> Vec<crate::model::Axiom> {
        let n = "170141183460469231731687303715884105728";
        let mut text = format!(
            "Prefix(:=<urn:numeric-world:>) Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>) Ontology(FunctionalDataProperty(:d) HasKey(:K () (:d)) ClassAssertion(:K :a) DataPropertyAssertion(:d :a \"{n}\"^^xsd:integer) "
        );
        for i in 0..12 {
            text.push_str(&format!("ClassAssertion(:K :i{i}) DataPropertyAssertion(:d :i{i} \"{n}.0\"^^xsd:decimal) SubClassOf(:C{i} ObjectOneOf(:a)) SubClassOf(:C{i} :B{i}) SubClassOf(:C{i} DataHasValue(:d \"{n}\"^^xsd:integer)) "));
        }
        text.push(')');
        crate::ofn_read::parse(&text).unwrap().axioms
    }
    fn shared(value: &DataVal) -> &Arc<crate::classify::numeric::CanonicalBigNumber> {
        let Known::Value(Value::Num(Number::Big(n))) = &value.known else {
            panic!("expected actual large numeric payload")
        };
        n
    }
    use crate::classify::data::DataVal;

    #[test]
    fn interner_index_functional_groups_world_and_keys_share_one_live_payload() {
        let axioms = fixture();
        let refs: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
        let mut expected = None;
        for threads in [1, 4, 8] {
            let mut sat = Saturation::new();
            let out = sat
                .classify(
                    &refs,
                    None,
                    &Limits {
                        threads,
                        ..Limits::default()
                    },
                )
                .unwrap();
            assert!(out.consistent && out.complete);
            assert!(sat.excluded().is_empty());
            assert!(
                !sat.worlds.is_empty(),
                "the fixture must execute conditional worlds"
            );
            if let Some(taxonomy) = &expected {
                assert_eq!(&out.taxonomy, taxonomy);
            } else {
                expected = Some(out.taxonomy.clone());
            }
            let value = sat
                .interner
                .vals
                .iter()
                .find(|v| v.single().is_some())
                .unwrap();
            let payload = shared(value).clone();
            let ledger = sat.interner.ranges.ledger.clone().unwrap();
            let retained = ledger.bytes();
            for value in &sat.index.as_ref().unwrap().vals {
                if value.single().is_some() {
                    assert!(Arc::ptr_eq(shared(value), &payload));
                }
            }
            let mut groups = 0;
            for ctx in sat.ctx.iter().filter_map(|c| c.get()) {
                if let Some(x) = &ctx.st.lock().extra {
                    for g in &x.groups {
                        assert!(Arc::ptr_eq(shared(&g.val), &payload));
                        groups += 1;
                    }
                }
            }
            assert!(groups >= 12);
            assert_eq!(ledger.bytes(), retained);
            // Exercise a real World::drain copy of a globally saturated state with a
            // functional group, rather than testing State::clone in isolation.
            let root = sat
                .interner
                .class(&crate::model::Class::new("urn:numeric-world:C0"));
            let ix = sat.index.as_ref().unwrap();
            let live = AtomicIsize::new(0);
            let bound = |delta: isize| {
                live.fetch_add(delta, Ordering::Relaxed);
                true
            };
            let mut world = World::new(ix, &sat.ctx, None, &[], &[], &bound);
            world.slots.insert(
                root,
                Slot {
                    real: true,
                    todo: vec![Concl::Sub(BOTTOM)],
                    queued: true,
                    ..Slot::default()
                },
            );
            world.queue.push(root);
            world.waiting = 1;
            world.drain();
            let copied = world.slots[&root].st.as_ref().unwrap();
            let copied_group = &copied.extra.as_ref().unwrap().groups[0];
            assert!(Arc::ptr_eq(shared(&copied_group.val), &payload));
            assert_eq!(ledger.bytes(), retained);
            let alias = copied_group.val.clone();
            drop(world);
            assert_eq!(live.load(Ordering::Relaxed), 0);
            drop(payload);
            drop(sat);
            assert_eq!(
                ledger.bytes(),
                retained,
                "actual state alias outlives both accounting roots"
            );
            drop(alias);
            assert!(
                ledger.bytes() < retained,
                "last alias releases payload charge"
            );
        }
    }

    #[test]
    fn key_scratch_refusal_precedes_its_allocations_and_releases_on_error() {
        let axioms = fixture();
        let refs: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
        let mut sat = Saturation::new();
        sat.classify(&refs, None, &Limits::default()).unwrap();
        let ix = sat.index.as_ref().unwrap();
        let view = Global(&sat.ctx);
        let held = AtomicIsize::new(0);
        let refused = |delta: isize| {
            held.fetch_add(delta, Ordering::Relaxed);
            delta <= 0
        };
        let (r, stats) = measure(|| key_merges(ix, &view, &refused));
        assert!(matches!(r, Err(Halt::Stopped)));
        assert_eq!(stats.events, 0);
        assert_eq!(held.load(Ordering::Relaxed), 0);
        // A sufficient grant must do the real key work and release its scratch. When all
        // equalities are already saturated the returned merge list is empty and holds
        // nothing.
        let grants = AtomicUsize::new(0);
        let admitted = |delta: isize| {
            held.fetch_add(delta, Ordering::Relaxed);
            if delta > 0 {
                grants.fetch_add(1, Ordering::Relaxed);
            }
            true
        };
        let merges = key_merges(ix, &view, &admitted).unwrap();
        assert!(grants.load(Ordering::Relaxed) > 0);
        assert!(merges.is_empty());
        assert_eq!(held.load(Ordering::Relaxed), 0);
        drop(merges);
        assert_eq!(held.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn conditional_clone_refusal_precedes_state_copy_allocation() {
        let axioms = fixture();
        let refs: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
        let mut sat = Saturation::new();
        sat.classify(&refs, None, &Limits::default()).unwrap();
        let root = sat
            .interner
            .class(&crate::model::Class::new("urn:numeric-world:C0"));
        let ix = sat.index.as_ref().unwrap();
        let held = AtomicIsize::new(0);
        let bound = |delta: isize| {
            held.fetch_add(delta, Ordering::Relaxed);
            delta <= 0
        };
        let mut world = World::new(ix, &sat.ctx, None, &[], &[], &bound);
        // Prepare the legacy delivery buffers outside the measured region, so the
        // zero-allocation assertion addresses the newly guarded state-copy operation.
        world.slots.insert(
            root,
            Slot {
                real: true,
                todo: vec![Concl::Sub(BOTTOM)],
                queued: true,
                ..Slot::default()
            },
        );
        world.queue.push(root);
        world.waiting = 1;
        let (_, stats) = measure(|| world.drain());
        assert!(world.halted);
        assert!(world.slots[&root].st.is_none());
        assert_eq!(stats.events, 0);
        drop(world);
        assert_eq!(held.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn key_halts_propagate_from_both_real_callers_and_reuse_is_safe() {
        let axioms = fixture();
        let refs: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
        let mut sat = Saturation::new();
        let expected = sat
            .classify(
                &refs,
                None,
                &Limits {
                    threads: 1,
                    ..Limits::default()
                },
            )
            .unwrap();
        let cancelled = Arc::new(|| KEY_ADMITTING.with(Cell::get));
        assert_eq!(
            sat.classify(
                &refs,
                None,
                &Limits {
                    threads: 1,
                    cancelled: Some(cancelled),
                    ..Limits::default()
                }
            )
            .unwrap_err(),
            crate::classify::Error::Cancelled
        );
        let reused = sat
            .classify(
                &refs,
                None,
                &Limits {
                    threads: 1,
                    ..Limits::default()
                },
            )
            .unwrap();
        assert_eq!(reused.taxonomy, expected.taxonomy);
        assert_eq!(reused.consistent, expected.consistent);
        assert!(sat.excluded().is_empty());
        let root = sat
            .interner
            .class(&crate::model::Class::new("urn:numeric-world:C0"));
        let held = AtomicIsize::new(0);
        let bound = |delta: isize| {
            held.fetch_add(delta, Ordering::Relaxed);
            !KEY_ADMITTING.with(Cell::get)
        };
        let world = World::new(
            sat.index.as_ref().unwrap(),
            &sat.ctx,
            None,
            &[],
            &[],
            &bound,
        );
        assert!(
            matches!(world.classify(root), Err(Halt::Stopped)),
            "world caller must propagate key scratch refusal"
        );
        assert_eq!(held.load(Ordering::Relaxed), 0);
        let allowed = |delta: isize| {
            held.fetch_add(delta, Ordering::Relaxed);
            true
        };
        let world = World::new(
            sat.index.as_ref().unwrap(),
            &sat.ctx,
            None,
            &[],
            &[],
            &allowed,
        );
        assert!(world.classify(root).is_ok());
        assert_eq!(held.load(Ordering::Relaxed), 0);
    }
}
