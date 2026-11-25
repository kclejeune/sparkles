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

/// Reports live changes. An installed named-domain table gives its worlds direct
/// access to the ledger so unwinding can retire ownership without polling.
#[derive(Clone, Copy)]
enum Reporter<'a> {
    Callback(&'a (dyn Fn(isize) -> bool + Sync)),
    NamedDomain(&'a super::WorldBudget<'a>),
}

impl Reporter<'_> {
    fn tell(self, delta: isize) -> bool {
        self.tell_owned(delta, || {})
    }

    /// `own` records the posted delta using only infallible assignments.
    fn tell_owned(self, delta: isize, own: impl FnOnce()) -> bool {
        match self {
            Self::Callback(bound) => {
                own();
                bound(delta)
            }
            Self::NamedDomain(budget) => {
                let previous = budget.post(delta);
                own();
                #[cfg(test)]
                super::world_retirement_mixed_tests::world_posted(budget, delta, previous);
                budget.check_post(delta, previous)
                    && budget.halt.load(std::sync::atomic::Ordering::Relaxed) == 0
            }
        }
    }

    fn retire(self, delta: isize) {
        match self {
            Self::Callback(bound) => {
                bound(-delta);
            }
            Self::NamedDomain(budget) => budget.retire(delta),
        }
    }
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
    reporter: Reporter<'a>,
    bytes: usize,
}
impl Drop for Scratch<'_> {
    fn drop(&mut self) {
        if self.bytes > 0 {
            self.reporter.retire(self.bytes as isize);
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
    fn new(count: usize, reporter: Reporter<'a>) -> Result<Self, Halt> {
        Self::with_payload(count, 0, reporter)
    }
    fn with_payload(count: usize, payload: usize, reporter: Reporter<'a>) -> Result<Self, Halt> {
        Self::with_live(count, payload, 0, reporter)
    }
    fn with_live(
        count: usize,
        payload: usize,
        live: usize,
        reporter: Reporter<'a>,
    ) -> Result<Self, Halt> {
        let bytes = count
            .checked_mul(std::mem::size_of::<T>())
            .and_then(|bytes| bytes.checked_add(payload))
            .filter(|&bytes| bytes <= isize::MAX as usize)
            .ok_or(Halt::Memory(usize::MAX))?;
        live.checked_add(bytes)
            .filter(|&total| total <= isize::MAX as usize)
            .ok_or(Halt::Memory(usize::MAX))?;
        let mut scratch = Scratch { reporter, bytes: 0 };
        if bytes > 0 {
            #[cfg(test)]
            KEY_ADMITTING.with(|p| p.set(true));
            let accepted = reporter.tell_owned(bytes as isize, || scratch.bytes = bytes);
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
        member = real && !st.bottom && ix.named_nominal(individual) && st.subs.contains(&class);
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
/// The named individuals that the context `e` stands for. A key binds named individuals
/// alone, so an anonymous one is not among them (roadmap item 1281).
fn key_aliases<'s>(ix: &'s Index, e: Ex, st: &'s State) -> impl Iterator<Item = Ex> + use<'s> {
    ix.named_nominal(e)
        .then_some(e)
        .into_iter()
        .chain(st.noms().iter().copied().filter(|&n| ix.named_nominal(n)))
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
    key_merges_reported(ix, view, Reporter::Callback(bound))
}

fn key_merges_reported<'a>(
    ix: &Index,
    view: &dyn View,
    reporter: Reporter<'a>,
) -> Result<KeyMerges<'a>, Halt> {
    // The global fixpoint and each conditional world are stable while this routine
    // reads them. The first pass counts the pairs each key actually merges, so that the
    // returned storage is admitted for those pairs only, and not for every pair of key
    // members. The second pass repeats the matching and keeps the pairs. This doubles
    // the key work in exchange for storage that grows with the merges found.
    let mut found = 0;
    for key in &ix.keys {
        key_matches(ix, view, key, 0, reporter, |_| key_add(&mut found, 1))?;
    }
    let mut pairs = KeyStorage::new(found, reporter)?;
    let live = pairs._scratch.bytes;
    for key in &ix.keys {
        key_matches(ix, view, key, live, reporter, |pair| pairs.push(pair))?;
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
    reporter: Reporter<'_>,
    mut emit: impl FnMut((Ex, Ex)) -> Result<(), Halt>,
) -> Result<(), Halt> {
    let report = |delta| reporter.tell(delta);
    let bound: &(dyn Fn(isize) -> bool + Sync) = match reporter {
        Reporter::Callback(bound) => bound,
        Reporter::NamedDomain(_) => &report,
    };
    let count = key_member_count(ix, view, key.class, bound)?;
    if count < 2 {
        return Ok(());
    }
    let mut members = KeyStorage::with_live(count, 0, live, reporter)?;
    for &individual in &ix.nominals {
        key_poll(bound)?;
        if key_member(ix, view, individual, key.class) {
            members.push(individual)?;
        }
    }
    let mut count = 0;
    key_sources(ix, view, &members.rows, bound, |_| key_add(&mut count, 1))?;
    let mut sources = KeyStorage::with_live(
        count,
        0,
        key_live(&[live, members._scratch.bytes])?,
        reporter,
    )?;
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
    let mut objects = KeyStorage::with_live(object_count, 0, held, reporter)?;
    let mut data = KeyStorage::with_live(
        data_count,
        data_payload,
        key_live(&[held, objects._scratch.bytes])?,
        reporter,
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
                                    !st.extra.as_ref().is_some_and(|x| {
                                        x.horn.succ.contains(s)
                                            || super::horn_rules::covered(ix, &x.horn, s.0, s.1)
                                    })
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

/// Owns the estimate reported for a world's private state.
struct WorldDebit<'a> {
    reporter: Reporter<'a>,
    reported: Cell<usize>,
}

impl Drop for WorldDebit<'_> {
    fn drop(&mut self) {
        match self.reporter {
            Reporter::Callback(_) => {}
            Reporter::NamedDomain(budget) => budget.retire(self.reported.get() as isize),
        }
    }
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
    /// the bytes of the copies, the slots and the stored conclusions
    held: Cell<usize>,
    /// the messages waiting in the slots' to-do lists
    waiting: usize,
    applied: usize,
    halted: bool,
    halt_reason: Option<Halt>,
    #[cfg(test)]
    retirement_witness: Option<retirement_tests::Witness<'a>>,
    // Every field that owns private allocations must precede the estimate's owner.
    debit: WorldDebit<'a>,
}

/// The conclusions a world applies between checks of its bound.
const CHECK_EVERY: usize = 1024;
/// The bytes counted for a slot.
const SLOT_BYTES: usize = 64;

struct WorldSink<'b> {
    me: Ex,
    local: &'b mut Vec<Concl>,
    out: &'b mut Vec<(Ex, Concl)>,
    reporter: Reporter<'b>,
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

/// The activation owns all posted bytes that have yet to move into the world's debit.
struct ActivationDebit<'a> {
    budget: &'a super::WorldBudget<'a>,
    data_growth: &'a Cell<isize>,
    charged: &'a Cell<isize>,
}

impl Drop for ActivationDebit<'_> {
    fn drop(&mut self) {
        let delta = self.data_growth.get().wrapping_add(self.charged.get());
        self.budget.retire(delta);
    }
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
            let charged = self.charged.get() + bytes;
            if !self
                .reporter
                .tell_owned(bytes, || self.charged.set(charged))
            {
                self.halted.set(true);
            }
        }
    }
}

/// RAII guard for an in-flight positive delta reported to a world's `bound` callback.
/// Retires `-delta` on refusal OR callback panic so `WorldBudget.held` never leaks.
struct WorldDeltaGuard<'b> {
    bound: &'b (dyn Fn(isize) -> bool + Sync),
    delta: isize,
}

impl Drop for WorldDeltaGuard<'_> {
    fn drop(&mut self) {
        if self.delta != 0 {
            let back = std::mem::take(&mut self.delta);
            (self.bound)(-back);
        }
    }
}

/// Flushes an activation's retained `data_growth` into `World::{held, reported}` and
/// refunds buffered message charges on BOTH normal activation exit and panic unwind.
///
/// Order is explicit: `data_growth` transfers to `held`/`reported` BEFORE calling
/// `(self.bound)(-charged)` so that if the negative message-refund call itself panics on
/// cancellation, `World::drop` still releases earlier retained local debits cleanly.
struct ActivationFlushGuard<'a> {
    held: &'a Cell<usize>,
    reported: &'a Cell<usize>,
    bound: &'a (dyn Fn(isize) -> bool + Sync),
    data_growth: &'a Cell<isize>,
    charged: &'a Cell<isize>,
}

impl Drop for ActivationFlushGuard<'_> {
    fn drop(&mut self) {
        let dg = self.data_growth.replace(0);
        if dg != 0 {
            self.held.set(self.held.get().saturating_add_signed(dg));
            self.reported
                .set(self.reported.get().saturating_add_signed(dg));
        }
        let ch = self.charged.replace(0);
        if ch != 0 {
            (self.bound)(-ch);
        }
    }
}

impl Sink for WorldSink<'_> {
    fn reserve_data(&mut self, bytes: usize) -> bool {
        if self.halted.get() {
            return false;
        }
        let Ok(add) = isize::try_from(bytes) else {
            self.speculative_overflow();
            return false;
        };
        let Some(next) = self.data_growth.get().checked_add(add) else {
            self.speculative_overflow();
            return false;
        };
        #[cfg(test)]
        if add > 0 {
            super::admission_test::hit(super::admission_test::Point::WorldDataReserve);
        }
        let ok = match self.reporter {
            Reporter::Callback(bound) => {
                let mut in_flight = WorldDeltaGuard { bound, delta: add };
                let ok = bound(add);
                if ok {
                    in_flight.delta = 0;
                    self.data_growth.set(next);
                } else {
                    self.halted.set(true);
                }
                ok
            }
            Reporter::NamedDomain(_) => {
                let ok = self.reporter.tell_owned(add, || self.data_growth.set(next));
                if !ok {
                    self.data_growth.set(next - add);
                    self.reporter.retire(add);
                    self.halted.set(true);
                }
                ok
            }
        };
        ok
    }
    fn speculative_overflow(&mut self) {
        self.halted.set(true);
        self.overflow.set(true);
    }
    fn retained_overflow(&mut self) {
        self.halted.set(true);
        self.overflow.set(true);
    }
    // what the run's tables keep stays charged after the world's copies go, until the
    // worlds end
    fn keep_shared(&mut self, bytes: usize) {
        if bytes > isize::MAX as usize {
            self.retained_overflow();
        } else if !self.reporter.tell(bytes as isize) {
            self.halted.set(true);
        }
    }
    // `reserve_data` already charged `self.reporter` and added `bytes` to `data_growth`;
    // taking `bytes` out of `data_growth` keeps the bound charged after the world drops.
    fn retain_shared(&mut self, bytes: usize) {
        self.data_growth
            .set(self.data_growth.get() - bytes as isize);
    }
    fn release_data(&mut self, bytes: usize) {
        self.data_growth
            .set(self.data_growth.get() - bytes as isize);
        self.reporter.tell(-(bytes as isize));
    }
    fn go_on(&self) -> bool {
        !self.halted.get() && self.reporter.tell(0)
    }
    fn stopped(&self) -> bool {
        self.halted.get()
    }
    fn halt_merge(&self) -> bool {
        false
    }
    fn publish_split_stop(&self) -> bool {
        false
    }
    fn wake_split_stop(&mut self) {}

    fn top_chain_sub(
        &mut self,
        to: Ex,
        domain: Ex,
        #[cfg(test)] probe: Option<&super::top_chain_diagnostic::Probe>,
    ) -> bool {
        let bytes = size_of::<(Ex, Concl)>() as isize;
        let (Some(charged), Some(grown)) = (
            self.charged.get().checked_add(bytes),
            self.grown.get().checked_add(1),
        ) else {
            self.data_overflow();
            return false;
        };
        #[cfg(test)]
        super::admission_test::hit(super::admission_test::Point::TopChainWorldMessage);
        if !self.reserve_data(bytes as usize) {
            return false;
        }
        self.data_growth.set(self.data_growth.get() - bytes);
        self.charged.set(charged);
        self.grown.set(grown);
        self.out.push((to, Concl::Sub(domain)));
        #[cfg(test)]
        if let Some(probe) = probe {
            probe
                .world_messages
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        true
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

    fn conjunction(&mut self, core: &[Ex]) -> Option<Ex> {
        // Global hit first: zero bytes requested.
        if let Some(e) = self.dynamic.and_then(|d| d.find(core)) {
            return Some(e);
        }
        if self.halted.get() {
            return None;
        }
        struct WorldDynamicBudget<'b> {
            reporter: Reporter<'b>,
            data_growth: &'b Cell<isize>,
            halted: &'b Cell<bool>,
            overflow: &'b Cell<bool>,
        }
        impl super::saturate::DynamicBudget for WorldDynamicBudget<'_> {
            fn admit(&mut self, bytes: usize) -> bool {
                if self.halted.get() {
                    return false;
                }
                let Ok(add) = isize::try_from(bytes) else {
                    self.on_overflow();
                    return false;
                };
                let Some(next) = self.data_growth.get().checked_add(add) else {
                    self.on_overflow();
                    return false;
                };
                match self.reporter {
                    Reporter::Callback(bound) => {
                        let mut in_flight = WorldDeltaGuard { bound, delta: add };
                        let ok = bound(add);
                        if ok {
                            in_flight.delta = 0;
                            self.data_growth.set(next);
                        } else {
                            self.halted.set(true);
                        }
                        ok
                    }
                    Reporter::NamedDomain(_) => {
                        let ok = self.reporter.tell_owned(add, || self.data_growth.set(next));
                        if !ok {
                            self.data_growth.set(next - add);
                            self.reporter.retire(add);
                            self.halted.set(true);
                        }
                        ok
                    }
                }
            }
            fn credit(&mut self, bytes: usize) {
                if bytes > 0 {
                    let sub = bytes as isize;
                    self.data_growth.set(self.data_growth.get() - sub);
                    self.reporter.retire(sub);
                }
            }
            fn on_overflow(&mut self) {
                self.halted.set(true);
                self.overflow.set(true);
            }
        }
        let mut budget = WorldDynamicBudget {
            reporter: self.reporter,
            data_growth: self.data_growth,
            halted: self.halted,
            overflow: self.overflow,
        };
        self.table.intern_with_budget(core, &mut budget)
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
    #[cfg(test)]
    pub fn new(
        ix: &'a Index,
        ctx: &'a Contexts,
        dynamic: Option<&'a Dynamic>,
        global_universal: &'a [Ex],
        global_real: &'a [Ex],
        bound: &'a (dyn Fn(isize) -> bool + Sync),
    ) -> World<'a> {
        assert!(
            ix.top_chain.is_none(),
            "an active world requires its budget"
        );
        Self::new_reported(
            ix,
            ctx,
            dynamic,
            global_universal,
            global_real,
            Reporter::Callback(bound),
        )
    }

    pub(super) fn new_with_budget(
        ix: &'a Index,
        ctx: &'a Contexts,
        dynamic: Option<&'a Dynamic>,
        global_universal: &'a [Ex],
        global_real: &'a [Ex],
        bound: &'a (dyn Fn(isize) -> bool + Sync),
        budget: &'a super::WorldBudget<'a>,
    ) -> Self {
        let reporter = if ix.top_chain.is_some() {
            Reporter::NamedDomain(budget)
        } else {
            Reporter::Callback(bound)
        };
        Self::new_reported(ix, ctx, dynamic, global_universal, global_real, reporter)
    }

    fn new_reported(
        ix: &'a Index,
        ctx: &'a Contexts,
        dynamic: Option<&'a Dynamic>,
        global_universal: &'a [Ex],
        global_real: &'a [Ex],
        reporter: Reporter<'a>,
    ) -> Self {
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
            held: Cell::new(0),
            waiting: 0,
            applied: 0,
            halted: false,
            halt_reason: None,
            #[cfg(test)]
            retirement_witness: None,
            debit: WorldDebit {
                reporter,
                reported: Cell::new(0),
            },
        }
    }

    /// Tells the bound what the world holds now, its waiting messages included, and
    /// stops the world when the bound refuses. `busy` counts the messages in the buffers
    /// of the context being processed.
    fn checkpoint(&mut self, busy: usize) {
        let used = self.held.get()
            + self.local.unadmitted_bytes()
            + (self.waiting + self.pending.len() + busy) * std::mem::size_of::<(Ex, Concl)>();
        let delta = used as isize - self.debit.reported.get() as isize;
        #[cfg(test)]
        if self.ix.top_chain.is_some() {
            super::admission_test::hit(super::admission_test::Point::TopChainWorldCheckpoint);
        }
        if !self
            .debit
            .reporter
            .tell_owned(delta, || self.debit.reported.set(used))
        {
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
            if self.ix.top_chain.is_some() && self.halted {
                break;
            }
        }
    }

    fn admit_slot(&mut self, to: Ex) {
        let (Some(held), Some(reported)) = (
            self.held.get().checked_add(SLOT_BYTES),
            self.debit
                .reported
                .get()
                .checked_add(SLOT_BYTES)
                .filter(|&total| isize::try_from(total).is_ok()),
        ) else {
            self.halt_reason = Some(Halt::Memory(usize::MAX));
            self.halted = true;
            return;
        };
        let reporter = self.debit.reporter;
        let mut debit = Scratch { reporter, bytes: 0 };
        #[cfg(test)]
        super::admission_test::hit(super::admission_test::Point::TopChainWorldDestination);
        #[cfg(test)]
        if let Reporter::NamedDomain(budget) = reporter {
            super::world_retirement_mixed_tests::before_slot_post(budget, to, SLOT_BYTES as isize);
        }
        if !reporter.tell_owned(SLOT_BYTES as isize, || debit.bytes = SLOT_BYTES) {
            self.halted = true;
            return;
        }
        let real = self.global_real(to);
        self.insert_slot(to, real);
        self.held.set(held);
        self.debit.reported.set(reported);
        debit.bytes = 0;
    }

    fn insert_slot(&mut self, to: Ex, real: bool) {
        self.slots.insert(
            to,
            Slot {
                real,
                ..Slot::default()
            },
        );
    }

    fn deliver_one(&mut self, to: Ex, c: Concl) {
        if !self.slots.contains_key(&to) {
            let top_chain = self.ix.top_chain.is_some();
            if top_chain {
                self.admit_slot(to);
                if self.halted {
                    return;
                }
            } else {
                let real = self.global_real(to);
                self.held.set(self.held.get() + SLOT_BYTES);
                self.insert_slot(to, real);
            }
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
            let data_growth = Cell::new(0isize);
            let charged = Cell::new(0isize);
            let _debit = match self.debit.reporter {
                Reporter::NamedDomain(budget) => Some(ActivationDebit {
                    budget,
                    data_growth: &data_growth,
                    charged: &charged,
                }),
                Reporter::Callback(_) => None,
            };
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
                        let reported = self.debit.reported.get() + bytes;
                        if !self
                            .debit
                            .reporter
                            .tell_owned(bytes as isize, || self.debit.reported.set(reported))
                        {
                            self.halted = true;
                            return;
                        }
                        self.held.set(self.held.get() + bytes);
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
                        self.held.set(self.held.get() + CONTEXT_BYTES);
                    }
                    self.checkpoint(todo.len());
                    st
                }
            };
            let mut local: Vec<Concl> = Vec::new();
            let mut out: Vec<(Ex, Concl)> = Vec::new();
            let data_halted = Cell::new(false);
            let data_overflow = Cell::new(false);
            // the initialization's output is metered from zero, and the incoming
            // messages, which were reported as waiting, raise the mark when they join
            // the buffers below
            let grown = Cell::new(0usize);
            {
                let _flush_guard = match self.debit.reporter {
                    Reporter::Callback(bound) => Some(ActivationFlushGuard {
                        held: &self.held,
                        reported: &self.debit.reported,
                        bound,
                        data_growth: &data_growth,
                        charged: &charged,
                    }),
                    Reporter::NamedDomain(_) => None,
                };
                let core = self.conjunction_core(e).map(|c| c.to_vec());
                let mut sink = WorldSink {
                    me: e,
                    local: &mut local,
                    out: &mut out,
                    reporter: self.debit.reporter,
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
                            self.held.set(self.held.get() + CONCLUSION_BYTES);
                        }
                        self.applied += 1;
                        if data_halted.get() {
                            break;
                        }
                        if self.applied.is_multiple_of(CHECK_EVERY) {
                            // the sink's charge for the buffers counts as reported, so
                            // that the checkpoint charges the buffers once
                            self.debit.reported.set(
                                self.debit
                                    .reported
                                    .get()
                                    .saturating_add_signed(charged.replace(0)),
                            );
                            let busy = sink.local.len() + sink.out.len();
                            grown.set(busy);
                            let used = self.held.get()
                                + self.local.unadmitted_bytes()
                                + (self.waiting + self.pending.len() + busy)
                                    * std::mem::size_of::<(Ex, Concl)>();
                            let delta = used as isize - self.debit.reported.get() as isize;
                            #[cfg(test)]
                            if self.ix.top_chain.is_some() {
                                super::admission_test::hit(
                                    super::admission_test::Point::TopChainWorldCheckpoint,
                                );
                            }
                            if !self
                                .debit
                                .reporter
                                .tell_owned(delta, || self.debit.reported.set(used))
                            {
                                self.halted = true;
                                break;
                            }
                        }
                    }
                    // R3 and R5 of the Horn rules wait for the activation's universals
                    // and at-most restrictions, as in the saturation (roadmap item 692)
                    if data_halted.get()
                        || self.halted
                        || !horn
                        || !super::horn_rules::flush(ix, e, &mut st, &mut sink)
                    {
                        break;
                    }
                }
            }
            // the messages charged as they were pushed are released here (or already
            // flushed by `_flush_guard` for callback reporters), and the checkpoint below
            // counts those still waiting
            let top_chain = self.ix.top_chain.is_some();
            if top_chain {
                // The local work is finished or discarded. The outgoing buffer stays
                // owned through the checkpoint and moves to pending only on success.
                drop(local);
                self.debit.reported.set(
                    self.debit
                        .reported
                        .get()
                        .saturating_add_signed(charged.replace(0)),
                );
                let data = data_growth.replace(0);
                self.held.set(self.held.get().saturating_add_signed(data));
                self.debit
                    .reported
                    .set(self.debit.reported.get().saturating_add_signed(data));
            }
            self.halted |= data_halted.get();
            if data_overflow.get() {
                self.halt_reason = Some(Halt::Memory(usize::MAX));
            }
            let slot = self.slots.get_mut(&e).expect("slot");
            slot.st = Some(st);
            // the messages the context sent count until they are delivered, and once
            // they wait in their contexts' to-do lists
            self.checkpoint(out.len());
            if top_chain {
                if self.halted {
                    return;
                }
                debug_assert!(self.pending.is_empty());
                self.pending = out;
            } else {
                self.pending.extend(out);
            }
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
            let merges = key_merges_reported(self.ix, &self, self.debit.reporter)?;
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
        if !found.bottom {
            super::dt_sat::world(self.ix, self.slots.values().filter_map(|s| s.st.as_ref()));
        }
        #[cfg(test)]
        if let Some(rules) = &self.ix.top_chain {
            rules.world_ready(class, self.debit.reported.get());
        }
        #[cfg(test)]
        if let Reporter::NamedDomain(budget) = self.debit.reporter {
            super::world_retirement_mixed_tests::world_ready(
                budget,
                class,
                self.debit.reported.get(),
            );
        }
        #[cfg(test)]
        if let Some(witness) = &self.retirement_witness {
            let state = self.slots[&class].st.as_ref().expect("resident COW slot");
            witness.ready(class, state, &found, self.debit.reported.get());
        }
        // the world's copies go when it does
        Ok(found)
    }
}

impl Drop for World<'_> {
    fn drop(&mut self) {
        match self.debit.reporter {
            Reporter::Callback(bound) => {
                // Keep the ordinary callback ahead of field destruction, even at zero.
                bound(-(self.debit.reported.get() as isize));
            }
            Reporter::NamedDomain(_) => {}
        }
    }
}

#[cfg(test)]
#[path = "world_retirement_tests.rs"]
mod retirement_tests;

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

    use crate::classify::{
        Error, WorldBudget,
        admission_test::{ChargePanic, Point, with_hook},
        saturate::Totals,
        top_chain_cases::accounting::{Grant, id, prefix, trailing_index},
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Fault {
        Accept,
        Refuse,
        Panic,
        BeforePost,
        AfterPost,
        AfterGrant,
    }

    impl Fault {
        fn unwinds(self) -> bool {
            !matches!(self, Self::Accept | Self::Refuse)
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum WorldBoundary {
        TwoRows,
        AfterCheckpoint,
        Destination,
        DestinationWithPending,
    }

    fn check_world_admission(boundary: WorldBoundary, fault: Fault) {
        use std::{
            panic::{AssertUnwindSafe, catch_unwind, panic_any},
            sync::atomic::AtomicBool,
        };

        const BASE: usize = 4096;
        const SIBLING: isize = 128;
        const MESSAGE: usize = size_of::<(Ex, Concl)>();
        assert_eq!(MESSAGE, CONCLUSION_BYTES);

        let (sat, probe) = trailing_index(boundary == WorldBoundary::TwoRows);
        let (tail, source, fence, domain) = (
            id(&sat, "Tail"),
            id(&sat, "Source"),
            id(&sat, "Fence"),
            id(&sat, "D"),
        );
        let r = prefix(&sat);
        let destination = matches!(
            boundary,
            WorldBoundary::Destination | WorldBoundary::DestinationWithPending
        );
        let two_inputs = matches!(
            boundary,
            WorldBoundary::AfterCheckpoint | WorldBoundary::DestinationWithPending
        );
        let grant = Arc::new(Grant::default());
        let armed = Arc::new(AtomicBool::new(false));
        let cancellation_polls = Arc::new(AtomicUsize::new(0));
        let limits = Limits {
            threads: 1,
            cancelled: (fault == Fault::AfterGrant).then(|| {
                let (armed, polls) = (armed.clone(), cancellation_polls.clone());
                let cancelled: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
                    if armed.load(Ordering::Relaxed) && polls.fetch_add(1, Ordering::Relaxed) == 0 {
                        panic_any(ChargePanic);
                    }
                    false
                });
                cancelled
            }),
            charge: Some(Arc::new({
                let (grant, armed) = (grant.clone(), armed.clone());
                move |bytes| {
                    let fault = if armed.load(Ordering::Relaxed) {
                        fault
                    } else {
                        Fault::Accept
                    };
                    let ok = grant.request(
                        bytes,
                        if matches!(fault, Fault::Accept | Fault::AfterGrant) {
                            usize::MAX
                        } else {
                            0
                        },
                    );
                    if fault == Fault::Panic {
                        panic_any(ChargePanic);
                    }
                    ok
                }
            })),
            ..Limits::default()
        };
        let charge = limits.charge.as_deref().unwrap() as &(dyn Fn(usize) -> bool + Sync);
        let mut totals = Totals::default();
        let budget = WorldBudget::start(&limits, Some(charge), BASE, &mut totals).unwrap();
        let bound = |delta| budget.tell(delta);
        assert!(budget.tell(SIBLING));

        let frozen: Contexts = (0..sat.ctx.len())
            .map(|_| std::sync::OnceLock::new())
            .collect();
        let mut world = World::new_with_budget(
            sat.index.as_ref().unwrap(),
            &frozen,
            None,
            &[],
            &[],
            &bound,
            &budget,
        );
        let mut todo = vec![Concl::Back(source, r)];
        if two_inputs {
            todo.push(Concl::Back(fence, r));
        }
        let st = State {
            init: true,
            real: true,
            ..State::default()
        };
        world.held.set(SLOT_BYTES + CONTEXT_BYTES + st.bytes());
        world.waiting = todo.len();
        world
            .debit
            .reported
            .set(world.held.get() + world.waiting * MESSAGE);
        let setup = world.debit.reported.get();
        assert!(bound(setup as isize));
        world.slots.insert(
            tail,
            Slot {
                real: true,
                st: Some(st),
                todo,
                queued: true,
            },
        );
        world.queue.push(tail);
        if boundary == WorldBoundary::AfterCheckpoint {
            world.applied = CHECK_EVERY - 1;
        }
        let baseline = BASE + SIBLING as usize + setup;
        assert_eq!(budget.paid.load(Ordering::Relaxed), baseline);
        assert_eq!(
            budget.held.load(Ordering::Relaxed),
            SIBLING + setup as isize
        );
        assert!(!world.slots.contains_key(&source));
        assert!(!world.slots.contains_key(&fence));

        let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let result = with_hook(
            {
                let (events, armed) = (events.clone(), armed.clone());
                let mut messages = 0;
                move |point| match point {
                    Point::TopChainWorldMessage => {
                        messages += 1;
                        events.lock().push(point);
                        if !destination && messages == 2 {
                            armed.store(true, Ordering::Relaxed);
                            if fault == Fault::BeforePost {
                                panic_any(ChargePanic);
                            }
                        }
                    }
                    Point::Observed
                        if fault == Fault::AfterPost && armed.load(Ordering::Relaxed) =>
                    {
                        panic_any(ChargePanic);
                    }
                    Point::TopChainWorldDestination => {
                        events.lock().push(point);
                        armed.store(true, Ordering::Relaxed);
                    }
                    Point::TopChainWorldCheckpoint => events.lock().push(point),
                    _ => {}
                }
            },
            || {
                catch_unwind(AssertUnwindSafe(move || {
                    world.drain();
                    world
                }))
            },
        );

        let expected_events = match boundary {
            WorldBoundary::TwoRows if fault.unwinds() => {
                vec![Point::TopChainWorldMessage, Point::TopChainWorldMessage]
            }
            WorldBoundary::TwoRows => vec![
                Point::TopChainWorldMessage,
                Point::TopChainWorldMessage,
                Point::TopChainWorldCheckpoint,
            ],
            WorldBoundary::AfterCheckpoint => vec![
                Point::TopChainWorldMessage,
                Point::TopChainWorldCheckpoint,
                Point::TopChainWorldMessage,
            ],
            WorldBoundary::Destination => vec![
                Point::TopChainWorldMessage,
                Point::TopChainWorldCheckpoint,
                Point::TopChainWorldDestination,
            ],
            WorldBoundary::DestinationWithPending => vec![
                Point::TopChainWorldMessage,
                Point::TopChainWorldMessage,
                Point::TopChainWorldCheckpoint,
                Point::TopChainWorldDestination,
            ],
        };
        assert_eq!(*events.lock(), expected_events);
        assert!(armed.load(Ordering::Relaxed));
        let sent = if boundary == WorldBoundary::DestinationWithPending {
            2
        } else {
            1
        };
        assert_eq!(probe.world_messages.load(Ordering::Relaxed), sent);
        let fault_bytes = if destination { SLOT_BYTES } else { MESSAGE };
        let mut requests = vec![(BASE, true), (SIBLING as usize, true), (setup, true)];
        requests.extend(std::iter::repeat_n((MESSAGE, true), sent));
        if !matches!(fault, Fault::BeforePost | Fault::AfterPost) {
            requests.push((
                fault_bytes,
                matches!(fault, Fault::Accept | Fault::AfterGrant),
            ));
        }
        assert_eq!(grant.calls(), requests);
        let first_peak = baseline + sent * MESSAGE;
        let full_peak = first_peak + fault_bytes;
        let peak = budget.peak.load(Ordering::Relaxed);
        if fault == Fault::AfterPost {
            assert!((first_peak..=full_peak).contains(&peak));
        } else {
            assert_eq!(
                peak,
                if fault == Fault::BeforePost {
                    first_peak
                } else {
                    full_peak
                }
            );
        }
        let paid = if matches!(fault, Fault::Accept | Fault::AfterGrant) {
            full_peak
        } else {
            first_peak
        };
        assert_eq!(budget.paid.load(Ordering::Relaxed), paid);
        assert_eq!(grant.accepted(), paid);

        match result {
            Ok(world) => {
                assert!(!fault.unwinds());
                let tail_state = world.slots[&tail].st.as_ref().unwrap();
                assert!(tail_state.holds(&Concl::Back(source, r)));
                if fault == Fault::Accept {
                    assert!(budget.stopped().is_ok());
                    assert!(!world.halted);
                    assert_eq!(world.slots[&source].todo, [Concl::Sub(domain)]);
                } else {
                    assert!(
                        matches!(budget.stopped(), Err(Error::Memory { limit: usize::MAX, used }) if used == peak)
                    );
                    assert!(world.halted);
                    assert!(!world.slots.contains_key(&source));
                    assert!(!world.slots.contains_key(&fence));
                    if boundary == WorldBoundary::DestinationWithPending {
                        assert_eq!(world.pending, [(source, Concl::Sub(domain))]);
                    }
                }
                drop(world);
            }
            Err(payload) => {
                assert!(fault.unwinds());
                assert_eq!(payload.downcast_ref::<ChargePanic>(), Some(&ChargePanic));
                if fault == Fault::Panic {
                    assert!(matches!(budget.stopped(), Err(Error::Cancelled)));
                }
            }
        }
        let live = budget.held.load(Ordering::Relaxed);
        let polls = cancellation_polls.load(Ordering::Relaxed);
        eprintln!(
            "world {boundary:?} {fault:?}: live={live} sibling={SIBLING} paid={paid} peak={peak} polls={polls} requests={requests:?}"
        );
        let _ = budget.tell(-SIBLING);
        let after_sibling = budget.held.load(Ordering::Relaxed);
        assert_eq!(
            (live, polls),
            (SIBLING, usize::from(fault == Fault::AfterGrant)),
            "world debit survived {boundary:?} {fault:?} or teardown repolled"
        );
        assert_eq!(after_sibling, 0);
    }

    #[test]
    fn named_domain_activation_debits_retire_on_unwind() {
        check_world_admission(WorldBoundary::TwoRows, Fault::Refuse);
        check_world_admission(WorldBoundary::TwoRows, Fault::Panic);
    }

    #[test]
    fn named_domain_activation_retires_only_untransferred_debits_after_checkpoint() {
        check_world_admission(WorldBoundary::AfterCheckpoint, Fault::Panic);
    }

    #[test]
    fn named_domain_destination_slot_admission_owns_its_debit() {
        check_world_admission(WorldBoundary::Destination, Fault::Accept);
        check_world_admission(WorldBoundary::Destination, Fault::Refuse);
        check_world_admission(WorldBoundary::DestinationWithPending, Fault::Refuse);
        check_world_admission(WorldBoundary::Destination, Fault::Panic);
    }

    #[test]
    fn named_domain_world_prepost_panic_retires_only_the_buffered_debit() {
        check_world_admission(WorldBoundary::TwoRows, Fault::BeforePost);
    }

    #[test]
    fn named_domain_world_posted_debit_is_owned_before_the_caller_grant() {
        check_world_admission(WorldBoundary::TwoRows, Fault::AfterPost);
    }

    #[test]
    fn named_domain_world_cancel_panic_keeps_payment_and_never_repolls_on_drop() {
        check_world_admission(WorldBoundary::TwoRows, Fault::AfterGrant);
    }

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
    fn a_world_sink_rolls_back_a_refused_reservation_and_releases_its_own_data() {
        use crate::classify::{Error, WorldBudget, saturate::Totals};

        let ax = owner_axioms("SubClassOf(:A :B)");
        let kinds: Vec<_> = ax.iter().map(|a| &a.kind).collect();
        let mut sat = Saturation::new();
        sat.saturate(&kinds, None, &Limits::default()).unwrap();
        let limits = Limits {
            charge: Some(std::sync::Arc::new(|_| false)),
            ..Limits::default()
        };
        let charge = limits
            .charge
            .as_deref()
            .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
        let mut totals = Totals {
            peak: 4160,
            paid: 4160,
            ..Totals::default()
        };
        let budget = WorldBudget::start(&limits, charge, 4096, &mut totals).unwrap();
        let reports = parking_lot::Mutex::new(Vec::new());
        let bound = |delta| {
            reports.lock().push(delta);
            budget.tell(delta)
        };
        let world = World::new(
            sat.index.as_ref().unwrap(),
            &sat.ctx,
            None,
            &[],
            &[],
            &bound,
        );
        let growth = Cell::new(0);
        let halted = Cell::new(false);
        let mut sink = WorldSink {
            me: 0,
            local: &mut Vec::new(),
            out: &mut Vec::new(),
            reporter: Reporter::Callback(&bound),
            data_growth: &growth,
            halted: &halted,
            overflow: &Cell::new(false),
            ctx_len: sat.ctx.len(),
            dynamic: None,
            table: world.local.clone(),
            grown: &Cell::new(0),
            charged: &Cell::new(0),
        };
        assert!(sink.admit_data(64));
        sink.retain_shared(32);
        assert!(!sink.admit_data(128));
        assert!(halted.get());
        assert_eq!(growth.get(), 32);
        assert_eq!(budget.held.load(Ordering::Relaxed), 64);
        // A later reservation at the halted sink makes no bound report.
        assert!(!sink.admit_data(1));
        sink.release_data(16);
        assert_eq!(growth.get(), 16);
        // The activation transfers only its remaining local debit to the world.
        world
            .debit
            .reported
            .set(world.debit.reported.get() + growth.get() as usize);
        drop(world);
        assert_eq!(budget.held.load(Ordering::Relaxed), 32);
        assert_eq!(*reports.lock(), [64, 128, -128, -16, -16]);
        assert_eq!(
            budget.stopped(),
            Err(Error::Memory {
                limit: usize::MAX,
                used: 4288,
            })
        );
        budget.finish(&mut totals);
        assert_eq!((totals.peak, totals.paid), (4288, 4160));
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
        let (result, allocated) =
            measure(|| KeyStorage::<Ex>::new(usize::MAX, Reporter::Callback(&overflow_bound)));
        assert!(matches!(result, Err(Halt::Memory(usize::MAX))));
        assert_eq!(allocated.events, 0);
        let (aggregate, allocated) = measure(|| {
            KeyStorage::<Ex>::with_live(
                1,
                0,
                isize::MAX as usize,
                Reporter::Callback(&overflow_bound),
            )
        });
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

    /// Astra r12 minor implementation note test: `ActivationFlushGuard::drop` transfers
    /// `data_growth` to `World::{held, reported}` BEFORE calling `(self.bound)(-charged)`
    /// to refund buffered messages. With both counters nonzero and a cancellation panic
    /// armed ONLY for that negative message-refund call, catching the original panic and
    /// dropping the world must return `WorldBudget.held` to its exact starting balance.
    #[test]
    fn activation_flush_guard_drop_transfers_data_growth_before_message_refund_panic() {
        use super::super::saturate::Totals;
        use super::super::{Error, Limits, Saturation, WorldBudget};
        use std::panic::{AssertUnwindSafe, catch_unwind, panic_any};
        use std::sync::atomic::AtomicBool;

        #[derive(Debug, PartialEq, Eq)]
        struct RefundPanic;

        let ax = owner_axioms("SubClassOf(:A :B)");
        let kinds: Vec<_> = ax.iter().map(|a| &a.kind).collect();
        let mut sat = Saturation::new();
        sat.saturate(&kinds, None, &Limits::default()).unwrap();

        let arm_panic = Arc::new(AtomicBool::new(false));
        let arm_clone = arm_panic.clone();
        let limits = Limits {
            cancelled: Some(Arc::new(move || {
                if arm_clone.load(Ordering::SeqCst) {
                    panic_any(RefundPanic);
                }
                false
            })),
            ..Limits::default()
        };
        let mut totals = Totals {
            peak: 4096,
            paid: 4096,
            ..Totals::default()
        };
        let budget = WorldBudget::start(&limits, None, 4096, &mut totals).unwrap();
        // Establish a known non-zero starting balance (64 B held outside this world).
        assert!(budget.tell(64));
        assert_eq!(budget.held.load(Ordering::Relaxed), 64);

        let bound = |delta: isize| budget.tell(delta);
        let world = World::new(
            sat.index.as_ref().unwrap(),
            &sat.ctx,
            None,
            &[],
            &[],
            &bound,
        );
        let data_growth = Cell::new(0isize);
        let charged = Cell::new(0isize);
        // Simulate an activation that admitted 96 B of local data growth AND 48 B of
        // buffered outgoing messages, both live in `WorldBudget.held`.
        assert!(bound(96));
        data_growth.set(96);
        assert!(bound(48));
        charged.set(48);
        assert_eq!(budget.held.load(Ordering::Relaxed), 64 + 96 + 48);

        // Arm the cancellation panic ONLY for the negative message refund on ordinary
        // activation exit (NOT while already panicking!).
        let caught = catch_unwind(AssertUnwindSafe(|| {
            let _guard = ActivationFlushGuard {
                held: &world.held,
                reported: &world.reported,
                bound: &bound,
                data_growth: &data_growth,
                charged: &charged,
            };
            arm_panic.store(true, Ordering::SeqCst);
            // Normal scope exit drops `_guard`, which MUST transfer `data_growth` (96)
            // into `world.{held, reported}` BEFORE calling `(bound)(-48)` which panics
            // in `WorldBudget::tell`'s cancellation poll (after already subtracting 48
            // from `budget.held`!).
        }))
        .unwrap_err();
        assert_eq!(
            caught.downcast_ref::<RefundPanic>(),
            Some(&RefundPanic),
            "original refund panic must reach catcher"
        );
        // Disarm so drop check verifies clean release (and even armed, unwind skips poll).
        arm_panic.store(false, Ordering::SeqCst);
        // `data_growth` (96) must already be in `world.{held, reported}`, while `-48`
        // already retired from `budget.held` before the panic!
        assert_eq!(world.held.get(), 96);
        assert_eq!(world.reported.get(), 96);
        assert_eq!(budget.held.load(Ordering::Relaxed), 64 + 96);

        drop(world);
        assert_eq!(
            budget.held.load(Ordering::Relaxed),
            64,
            "dropping world must release the 96 B local debit and return to exact starting balance"
        );
        assert_eq!(budget.stopped(), Err(Error::Cancelled));
    }

    /// Design v12 §4.3.4 / r10 #3 / r11: exercises `WorldSink::conjunction` against the
    /// production `WorldBudget` across (a) global hit (0 B), (b) local segment-boundary
    /// refusal restoring `WorldBudget.held` immediately, (c) exact sufficient budget with
    /// zero double-count at `World::checkpoint`, and (d) panicking `charge` and panicking
    /// `cancelled` after an earlier successful local insertion in the same activation.
    #[test]
    fn world_local_conjunction_accounting_and_unwind_with_production_budget() {
        use super::super::saturate::Dynamic;
        use super::super::saturate::Totals;
        use super::super::{Error, Limits, Saturation, WorldBudget};
        use std::panic::{AssertUnwindSafe, catch_unwind, panic_any};
        use std::sync::atomic::AtomicUsize;

        let ax = owner_axioms("SubClassOf(:A :B)");
        let kinds: Vec<_> = ax.iter().map(|a| &a.kind).collect();
        let mut sat = Saturation::new();
        sat.saturate(&kinds, None, &Limits::default()).unwrap();

        // (a) Global hit requests 0 bytes even under a refusing bound.
        let global_dyn = Dynamic::new(100);
        let global_ex = global_dyn.intern(&[1, 2]);
        {
            let limits = Limits {
                charge: Some(Arc::new(|_| false)),
                ..Limits::default()
            };
            let charge = limits
                .charge
                .as_deref()
                .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
            let mut totals = Totals {
                peak: 4096,
                paid: 4096,
                ..Totals::default()
            };
            let budget = WorldBudget::start(&limits, charge, 4096, &mut totals).unwrap();
            let calls = AtomicUsize::new(0);
            let bound = |d: isize| {
                calls.fetch_add(1, Ordering::Relaxed);
                budget.tell(d)
            };
            let world = World::new(
                sat.index.as_ref().unwrap(),
                &sat.ctx,
                Some(&global_dyn),
                &[],
                &[],
                &bound,
            );
            let growth = Cell::new(0);
            let halted = Cell::new(false);
            let overflow = Cell::new(false);
            let mut sink = WorldSink {
                me: 0,
                local: &mut Vec::new(),
                out: &mut Vec::new(),
                bound: &bound,
                data_growth: &growth,
                halted: &halted,
                overflow: &overflow,
                ctx_len: sat.ctx.len(),
                dynamic: Some(&global_dyn),
                table: world.local.clone(),
                grown: &Cell::new(0),
                charged: &Cell::new(0),
            };
            assert_eq!(sink.conjunction(&[1, 2]), Some(global_ex));
            assert_eq!(calls.load(Ordering::Relaxed), 0, "global hit requests 0 B");
            assert_eq!(growth.get(), 0);
            assert!(!halted.get());
        }

        // (b) Local segment-boundary miss with refusing budget: zero segment allocation,
        // zero published ID, restores `WorldBudget.held` immediately back to 64, records
        // refusal stop & attempted peak, and dropping world releases earlier 64 -> 0.
        {
            let limits = Limits {
                charge: Some(Arc::new(|_| false)),
                ..Limits::default()
            };
            let charge = limits
                .charge
                .as_deref()
                .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
            let mut totals = Totals {
                peak: 4160,
                paid: 4160,
                ..Totals::default()
            };
            let budget = WorldBudget::start(&limits, charge, 4096, &mut totals).unwrap();
            let bound = |d: isize| budget.tell(d);
            let world = World::new(
                sat.index.as_ref().unwrap(),
                &sat.ctx,
                None,
                &[],
                &[],
                &bound,
            );
            assert!(bound(64));
            world.held.set(64);
            world.reported.set(64);
            assert_eq!(budget.held.load(Ordering::Relaxed), 64);

            let growth = Cell::new(0);
            let halted = Cell::new(false);
            let overflow = Cell::new(false);
            let mut sink = WorldSink {
                me: 0,
                local: &mut Vec::new(),
                out: &mut Vec::new(),
                bound: &bound,
                data_growth: &growth,
                halted: &halted,
                overflow: &overflow,
                ctx_len: sat.ctx.len(),
                dynamic: None,
                table: world.local.clone(),
                grown: &Cell::new(0),
                charged: &Cell::new(0),
            };
            assert_eq!(sink.conjunction(&[10, 20]), None);
            assert!(halted.get());
            assert_eq!(world.local.len(), 0, "refused miss must publish zero ID");
            assert_eq!(
                world.local.bytes(),
                0,
                "refused miss must allocate zero segment"
            );
            assert_eq!(growth.get(), 0);
            assert_eq!(
                budget.held.load(Ordering::Relaxed),
                64,
                "refused local miss must restore WorldBudget.held back to 64 immediately"
            );
            assert!(matches!(budget.stopped(), Err(Error::Memory { used, .. }) if used > 4160));
            drop(world);
            assert_eq!(budget.held.load(Ordering::Relaxed), 0);
        }

        // (c) Local segment-boundary miss with exact sufficient budget: zero double-count
        // delta at next `World::checkpoint` and exact release on `World::drop`.
        {
            let limits = Limits::default();
            let mut totals = Totals {
                peak: 4096,
                paid: 4096,
                ..Totals::default()
            };
            let budget = WorldBudget::start(&limits, None, 4096, &mut totals).unwrap();
            let bound = |d: isize| budget.tell(d);
            let world = World::new(
                sat.index.as_ref().unwrap(),
                &sat.ctx,
                None,
                &[],
                &[],
                &bound,
            );
            let growth = Cell::new(0);
            let halted = Cell::new(false);
            let overflow = Cell::new(false);
            let charged = Cell::new(0);
            let id = {
                let _flush = ActivationFlushGuard {
                    held: &world.held,
                    reported: &world.reported,
                    bound: &bound,
                    data_growth: &growth,
                    charged: &charged,
                };
                let mut sink = WorldSink {
                    me: 0,
                    local: &mut Vec::new(),
                    out: &mut Vec::new(),
                    bound: &bound,
                    data_growth: &growth,
                    halted: &halted,
                    overflow: &overflow,
                    ctx_len: sat.ctx.len(),
                    dynamic: None,
                    table: world.local.clone(),
                    grown: &Cell::new(0),
                    charged: &charged,
                };
                sink.conjunction(&[10, 20]).expect("sufficient budget")
            };
            assert!(id >= sat.ctx.len() as u32);
            let exact_bytes = world.local.bytes();
            assert!(exact_bytes > 0);
            assert_eq!(world.local.unadmitted_bytes(), 0);
            assert_eq!(budget.held.load(Ordering::Relaxed), exact_bytes as isize);
            assert!(World::checkpoint_fields(
                &world.held,
                &world.reported,
                &world.local,
                0,
                0,
                0,
                &bound
            ));
            assert_eq!(
                budget.held.load(Ordering::Relaxed),
                exact_bytes as isize,
                "checkpoint must not double-count admitted local conjunction bytes"
            );
            drop(world);
            assert_eq!(budget.held.load(Ordering::Relaxed), 0);
        }

        // (d) Panicking `charge` OR panicking `cancelled` after an earlier successful
        // local insertion in the same activation: catches original panic, rolls back
        // in-flight delta, transfers earlier retained insertion via `ActivationFlushGuard`,
        // releases cleanly on `World::drop` (`held == 0`), and preserves `paid`.
        #[derive(Debug, PartialEq, Eq)]
        struct CallbackBoom;
        for panic_in_charge in [true, false] {
            let arm = Arc::new(AtomicUsize::new(0));
            let arm_c = arm.clone();
            let arm_x = arm.clone();
            let limits = Limits {
                charge: Some(Arc::new(move |_| {
                    if panic_in_charge && arm_c.load(Ordering::SeqCst) == 1 {
                        panic_any(CallbackBoom);
                    }
                    true
                })),
                cancelled: Some(Arc::new(move || {
                    if !panic_in_charge && arm_x.load(Ordering::SeqCst) == 1 {
                        panic_any(CallbackBoom);
                    }
                    false
                })),
                ..Limits::default()
            };
            let charge = limits
                .charge
                .as_deref()
                .map(|f| f as &(dyn Fn(usize) -> bool + Sync));
            let mut totals = Totals {
                peak: 4096,
                paid: 4096,
                ..Totals::default()
            };
            let budget = WorldBudget::start(&limits, charge, 4096, &mut totals).unwrap();
            let bound = |d: isize| budget.tell(d);
            let world = World::new(
                sat.index.as_ref().unwrap(),
                &sat.ctx,
                None,
                &[],
                &[],
                &bound,
            );
            let growth = Cell::new(0);
            let halted = Cell::new(false);
            let overflow = Cell::new(false);
            let charged = Cell::new(0);

            let first_bytes = Cell::new(0usize);
            let caught = catch_unwind(AssertUnwindSafe(|| {
                let _flush = ActivationFlushGuard {
                    held: &world.held,
                    reported: &world.reported,
                    bound: &bound,
                    data_growth: &growth,
                    charged: &charged,
                };
                let mut sink = WorldSink {
                    me: 0,
                    local: &mut Vec::new(),
                    out: &mut Vec::new(),
                    bound: &bound,
                    data_growth: &growth,
                    halted: &halted,
                    overflow: &overflow,
                    ctx_len: sat.ctx.len(),
                    dynamic: None,
                    table: world.local.clone(),
                    grown: &Cell::new(0),
                    charged: &charged,
                };
                // First local insertion succeeds (allocates segment 0 + entry).
                assert!(sink.conjunction(&[1, 2]).is_some());
                first_bytes.set(world.local.bytes());
                // Now arm the callback panic and request a distinct second insertion.
                arm.store(1, Ordering::SeqCst);
                let _ = sink.conjunction(&[3, 4]);
            }))
            .unwrap_err();
            assert_eq!(
                caught.downcast_ref::<CallbackBoom>(),
                Some(&CallbackBoom),
                "panic_in_charge={panic_in_charge}: original panic must reach catcher"
            );
            budget.finish(&mut totals);
            assert!(
                totals.paid >= 4096 + first_bytes.get(),
                "prior successful pool payment must be preserved in paid ({})",
                totals.paid
            );
            assert_eq!(
                budget.held.load(Ordering::Relaxed),
                first_bytes.get() as isize,
                "in-flight delta must roll back while earlier insertion stays tracked on world"
            );
            drop(world);
            assert_eq!(
                budget.held.load(Ordering::Relaxed),
                0,
                "dropping world must release the earlier retained insertion completely"
            );
        }

        // Speculative vs retained overflow both halt and mark overflow on WorldSink.
        {
            let bound = |_: isize| true;
            let world = World::new(
                sat.index.as_ref().unwrap(),
                &sat.ctx,
                None,
                &[],
                &[],
                &bound,
            );
            for retained in [false, true] {
                let halted = Cell::new(false);
                let overflow = Cell::new(false);
                let mut sink = WorldSink {
                    me: 0,
                    local: &mut Vec::new(),
                    out: &mut Vec::new(),
                    bound: &bound,
                    data_growth: &Cell::new(0),
                    halted: &halted,
                    overflow: &overflow,
                    ctx_len: sat.ctx.len(),
                    dynamic: None,
                    table: world.local.clone(),
                    grown: &Cell::new(0),
                    charged: &Cell::new(0),
                };
                if retained {
                    sink.retained_overflow();
                } else {
                    sink.speculative_overflow();
                }
                assert!(halted.get());
                assert!(overflow.get());
            }
        }
    }
}
