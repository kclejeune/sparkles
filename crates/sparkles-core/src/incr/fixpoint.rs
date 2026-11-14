//! The fixed-point operator (spec F13 §4.3): one interface for recursive programs, which
//! property paths with `+` and `*` and the reasoner's rules plug into.
//!
//! A [`Program`] names its derived facts and the rule instances that derive them, and
//! chooses the [`FactStore`] that holds them. The operator, [`FixedPoint`], keeps every
//! derived fact of the program at the latest snapshot in that store. It maintains them in
//! one of two modes, which the program chooses with [`Program::EXACT`].
//!
//! **One-step mode.** Each fact keeps a *record*: the one rule instance that derived it
//! first. Records are well founded. A fact's recorded instance uses only base facts and
//! derived facts that held before it was recorded, so following records from any fact
//! ends at instances over base facts alone, and no fact supports itself through a cycle.
//! Counts cannot weight recursive maintenance, because a fact in a cycle can count its
//! own derivations, so a change is applied by backward and forward rederivation, in the
//! manner of the reasoner's backward/forward algorithm (Motik, Nenov, Piro and Horrocks,
//! AAAI 2015).
//!
//! 1. The facts whose recorded instance uses a base fact the changes removed are broken
//!    ([`Program::broken`]). They and every fact whose record depends on them, followed
//!    through the records at the snapshot before the changes, are the candidates. A
//!    fact that is not a candidate keeps a record whose every premise still holds, so
//!    it holds after the changes.
//! 2. Each candidate is checked backward ([`Program::prove`]): an instance at the new
//!    snapshot whose derived premises are facts that are not candidates, or candidates
//!    proved before it, keeps the candidate with that instance as its new record.
//! 3. The instances that use a base fact the changes added ([`Program::added`]) derive
//!    new facts, and candidates too.
//! 4. Forward chaining from every fact kept or derived in 2 and 3
//!    ([`Program::consequences`]) proves the candidates that follow from them and
//!    derives the rest of the new facts, semi-naively.
//! 5. The candidates that no step proved no longer follow and are deleted.
//!
//! Unlike DRed, a candidate that has another derivation is never deleted and derived
//! again, so the operator's output changes only by the facts that come and go. Unlike
//! the reasoner's backward search, the candidates are the facts whose records depend
//! on the removed facts, not every fact with a derivation that uses them, so a fact
//! with an intact record is never checked.
//!
//! **Exact mode.** The program keeps no records, and its [`Program::prove`] decides
//! exactly whether a fact follows from the base facts of the new snapshot through
//! instances whose derived premises the store holds, as the reasoner's backward search
//! does. A pass runs in rounds. [`Program::prepare`] applies the program's own preamble
//! and [`Program::frontier`] gives the first frontier, the facts that the changes may
//! have made unprovable. The
//! frontier is proved, the facts without a proof are dead, the heads of the instances at
//! the old snapshot that read a dead fact are enumerated while the dead facts are still
//! held, the dead facts leave the store, and the heads that the store still holds and
//! that no round proved are the next frontier. A fact never in a frontier keeps every
//! premise of an old derivation, by induction on the height of its derivations, and a
//! proved fact rests on base facts of the new snapshot, so the deletion keeps exactly the
//! facts that the new base facts derive without the added facts' consequences. Then
//! [`Program::saturate`], or the operator's own loop, inserts the added facts and
//! everything that follows from them. A fact deleted and derived again is reported as a
//! removal and an addition, which [`consolidate`] cancels.
//!
//! **Annotations.** With a provenance setting (spec F13 §5), each fact's annotation is
//! that of its recorded derivation: one derivation, whose support is the union of the
//! supports of the recorded instances it follows ([`Program::annotate`]). The witness
//! it reports is therefore real, and a fact whose record changes reports a new one. A
//! fact counts one derivation, since a recursive fact has unboundedly many. A program in
//! exact mode keeps no records, so it keeps no annotations.
//!
//! **Memory.** Producers write what they find into a [`Sink`], which reserves each new
//! allocation through the store before it grows, and fails when the store refuses or
//! `max_bytes` would be passed, so one fact with a large fan-out fails while it
//! produces. Against `max_bytes` count the state, the program's scratch and the live
//! allocation of the sink being filled, as they always have. The operator's other
//! buffers, its frontiers, queues, sets, tables and change list, are reserved through the
//! store only, which charges nothing for a [`HashStore`] and charges its run's budget for
//! a program that keeps its own. Each reservation covers the allocation the collection
//! will hold, including a hash table's buckets and control bytes, and both allocations
//! while a buffer moves into a larger one. A buffer freed during a pass releases its
//! reservation, and every reservation still held is released when the pass ends, whether
//! it succeeded or failed. The returned changes are released with them, and a caller
//! that keeps them charges them itself. A failed pass leaves the fixed point unusable
//! until the next [`FixedPoint::build`].

use super::pass::Which;
use super::persist::{Reader, Writer};
use super::prov::{Ann, Atom, Provenance};
use crate::error::{Error, Result};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_budget::sizes::{table_bytes, table_grown};
use std::collections::VecDeque;
use std::fmt::Debug;
use std::hash::Hash;
use std::mem::size_of;

/// Where a [`FixedPoint`] keeps its facts, chosen by the program.
///
/// Membership is the store's own: the facts it holds. A program can treat other facts as
/// held too, such as base facts that it reads from a snapshot instead of storing, and
/// decides about those in its own evaluation. Reads cannot fail, and writes can, so that a
/// store that charges a memory budget can refuse to grow.
pub trait FactStore<F, R> {
    type Error: From<Error>;

    fn contains(&self, f: &F) -> bool;

    /// The record of a held fact. A store of a program in exact mode may give any value.
    fn record(&self, f: &F) -> Option<R>;

    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Visit every held fact with its record. Two calls with no write between them visit
    /// the facts in the same order.
    fn for_each(&self, visit: &mut dyn FnMut(&F, &R));

    /// The approximate memory of the held facts.
    fn bytes(&self) -> u64;

    /// Hold `f` with record `r` when it is not held. Returns whether it was inserted.
    fn insert(&mut self, f: F, r: R) -> std::result::Result<bool, Self::Error>;

    /// Hold `f` with record `r`, replacing the record of a held fact.
    fn set_record(&mut self, f: F, r: R) -> std::result::Result<(), Self::Error>;

    /// Stop holding `f`. Returns whether it was held.
    fn remove(&mut self, f: &F) -> std::result::Result<bool, Self::Error>;

    /// Stop holding every fact.
    fn clear(&mut self) -> std::result::Result<(), Self::Error>;

    /// Charge `bytes` that the operator or a producer is about to allocate.
    fn reserve(&self, _bytes: u64) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    /// Return a charge that [`reserve`](Self::reserve) made.
    fn release(&self, _bytes: u64) {}
}

/// A store of facts in a hash map, charging nothing beyond the operator's `max_bytes`.
#[derive(Clone, Debug)]
pub struct HashStore<F, R> {
    map: FxHashMap<F, R>,
    fact_bytes: u64,
}

impl<F, R> HashStore<F, R> {
    /// A store that counts `fact_bytes` for each slot of its map.
    pub fn new(fact_bytes: u64) -> HashStore<F, R> {
        HashStore {
            map: FxHashMap::default(),
            fact_bytes,
        }
    }
}

impl<F: Copy + Eq + Hash, R: Copy> FactStore<F, R> for HashStore<F, R> {
    type Error = Error;

    #[inline]
    fn contains(&self, f: &F) -> bool {
        self.map.contains_key(f)
    }

    #[inline]
    fn record(&self, f: &F) -> Option<R> {
        self.map.get(f).copied()
    }

    #[inline]
    fn len(&self) -> usize {
        self.map.len()
    }

    fn for_each(&self, visit: &mut dyn FnMut(&F, &R)) {
        for (f, r) in &self.map {
            visit(f, r);
        }
    }

    fn bytes(&self) -> u64 {
        self.map.capacity() as u64 * self.fact_bytes
    }

    #[inline]
    fn insert(&mut self, f: F, r: R) -> Result<bool> {
        match self.map.entry(f) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(r);
                Ok(true)
            }
            std::collections::hash_map::Entry::Occupied(_) => Ok(false),
        }
    }

    #[inline]
    fn set_record(&mut self, f: F, r: R) -> Result<()> {
        self.map.insert(f, r);
        Ok(())
    }

    #[inline]
    fn remove(&mut self, f: &F) -> Result<bool> {
        Ok(self.map.remove(f).is_some())
    }

    fn clear(&mut self) -> Result<()> {
        self.map.clear();
        Ok(())
    }
}

/// A collection whose next allocation can be sized before it grows.
trait Buf {
    /// The bytes of the allocation it holds.
    fn alloc_bytes(&self) -> u64;
    /// The bytes of the allocation it will hold once room for `add` more items is made,
    /// or `None` when they fit.
    fn grown(&self, add: usize) -> Option<u64>;
    /// Make room for `add` more items.
    fn grow(&mut self, add: usize);
}

/// The capacity a vector or deque of `cap` slots and `len` items grows to for `add`
/// more: doubling, and at least 16 slots.
fn vec_target(cap: usize, len: usize, add: usize) -> Option<usize> {
    let need = len.saturating_add(add);
    (need > cap).then(|| need.max(cap.saturating_mul(2)).max(16))
}

fn slots_bytes<T>(slots: usize) -> u64 {
    (slots as u64).saturating_mul(size_of::<T>() as u64)
}

impl<T> Buf for Vec<T> {
    fn alloc_bytes(&self) -> u64 {
        slots_bytes::<T>(self.capacity())
    }

    fn grown(&self, add: usize) -> Option<u64> {
        vec_target(self.capacity(), self.len(), add).map(slots_bytes::<T>)
    }

    fn grow(&mut self, add: usize) {
        if let Some(to) = vec_target(self.capacity(), self.len(), add) {
            self.reserve_exact(to - self.len());
        }
    }
}

impl<T> Buf for VecDeque<T> {
    fn alloc_bytes(&self) -> u64 {
        slots_bytes::<T>(self.capacity())
    }

    fn grown(&self, add: usize) -> Option<u64> {
        vec_target(self.capacity(), self.len(), add).map(slots_bytes::<T>)
    }

    fn grow(&mut self, add: usize) {
        if let Some(to) = vec_target(self.capacity(), self.len(), add) {
            self.reserve_exact(to - self.len());
        }
    }
}

impl<T: Eq + Hash> Buf for FxHashSet<T> {
    fn alloc_bytes(&self) -> u64 {
        table_bytes(self.capacity(), size_of::<T>()) as u64
    }

    fn grown(&self, add: usize) -> Option<u64> {
        let to = table_grown(self.capacity(), self.len(), add);
        (to != self.capacity()).then(|| table_bytes(to, size_of::<T>()) as u64)
    }

    fn grow(&mut self, add: usize) {
        self.reserve(add);
    }
}

impl<K: Eq + Hash, V> Buf for FxHashMap<K, V> {
    fn alloc_bytes(&self) -> u64 {
        table_bytes(self.capacity(), size_of::<(K, V)>()) as u64
    }

    fn grown(&self, add: usize) -> Option<u64> {
        let to = table_grown(self.capacity(), self.len(), add);
        (to != self.capacity()).then(|| table_bytes(to, size_of::<(K, V)>()) as u64)
    }

    fn grow(&mut self, add: usize) {
        self.reserve(add);
    }
}

/// How a buffer charges its growth: a reservation through the store and its release.
struct Acct<'s, E> {
    reserve: &'s dyn Fn(u64) -> std::result::Result<(), E>,
    release: &'s dyn Fn(u64),
}

impl<E> Clone for Acct<'_, E> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<E> Copy for Acct<'_, E> {}

/// Make room in `buf`, whose allocation holds `held` reserved bytes, for `add` more
/// items: reserve the new allocation, grow, and release the old one, so that the
/// reservation covers both while the items move. `limit` is a memory limit with the
/// bytes counted against it beside this buffer.
fn grow_charged<B: Buf, E: From<Error>>(
    acct: Option<Acct<'_, E>>,
    limit: Option<(u64, u64)>,
    buf: &mut B,
    held: &mut u64,
    add: usize,
) -> std::result::Result<(), E> {
    let Some(new) = buf.grown(add) else {
        return Ok(());
    };
    if let Some((limit, before)) = limit {
        let requested = before.checked_add(new);
        if requested.is_none_or(|r| r > limit) {
            return Err(over(limit, requested.unwrap_or(u64::MAX)).into());
        }
    }
    let Some(acct) = acct else {
        buf.grow(add);
        return Ok(());
    };
    (acct.reserve)(new)?;
    buf.grow(add);
    let actual = buf.alloc_bytes();
    (acct.release)(*held);
    *held = new;
    // the reservation follows the allocation the collection reports
    if actual < new {
        (acct.release)(new - actual);
        *held = actual;
    } else if actual > new {
        (acct.reserve)(actual - new)?;
        *held = actual;
    }
    Ok(())
}

/// Where a producer writes what it finds: a buffer whose every growth is reserved
/// through the store, and checked against the pass's memory limit, before it happens.
pub struct Sink<'s, T, E> {
    buf: Vec<T>,
    acct: Option<Acct<'s, E>>,
    /// the limit, and the bytes counted against it beside the sink
    max: Option<(u64, u64)>,
    /// the bytes reserved for the buffer's allocation
    held: u64,
}

impl<'s, T, E: From<Error>> Sink<'s, T, E> {
    /// A sink that charges nothing, for a program's own use.
    pub fn unbounded() -> Sink<'s, T, E> {
        Sink {
            buf: Vec::new(),
            acct: None,
            max: None,
            held: 0,
        }
    }

    /// Add one item, growing the buffer only once the growth is charged.
    #[inline]
    pub fn push(&mut self, t: T) -> std::result::Result<(), E> {
        if self.buf.len() == self.buf.capacity() {
            self.grow()?;
        }
        self.buf.push(t);
        Ok(())
    }

    #[inline(never)]
    fn grow(&mut self) -> std::result::Result<(), E> {
        grow_charged(self.acct, self.max, &mut self.buf, &mut self.held, 1)
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The items found so far.
    pub fn items(&self) -> &[T] {
        &self.buf
    }

    /// Take the items found so far, leaving the buffer's capacity.
    pub fn drain(&mut self) -> std::vec::Drain<'_, T> {
        self.buf.drain(..)
    }
}

fn over(limit: u64, requested: u64) -> Error {
    Error::BudgetExceeded(crate::error::Budget {
        kind: crate::error::BudgetKind::Memory,
        limit,
        requested,
    })
}

/// A recursive program that the fixed-point operator maintains: its derived facts, the
/// rule instances that derive them and the base facts those instances read.
///
/// Every method that reads base facts reads them through the context `Cx` at the
/// snapshot it names, `Which::New` unless a method says otherwise. A `held` callback
/// tells whether a derived fact holds at that moment of the maintenance: an instance
/// whose derived premises do not all hold must not be returned. The store is passed to
/// the methods that evaluate rules, so a program can match premises through it.
pub trait Program {
    /// A derived fact.
    type Fact: Copy + Eq + Hash + Debug;
    /// The rule instance that derived a fact, enough to name its derived premises.
    type Record: Copy + Eq + Debug;
    /// What the program reads base facts through.
    type Cx<'c>;
    /// The program's errors, which the operator's own errors convert into.
    type Error: From<Error>;
    /// Where the operator keeps the program's facts.
    type Store: FactStore<Self::Fact, Self::Record, Error = Self::Error>;

    /// Whether the program maintains in exact mode (see the module's documentation).
    const EXACT: bool = false;

    /// How many facts one call of a batched method takes.
    fn batch_len(&self) -> usize {
        1
    }

    /// The derived premises of instance `r` of fact `f`.
    fn premises(
        &self,
        f: &Self::Fact,
        r: &Self::Record,
        out: &mut Sink<'_, Self::Fact, Self::Error>,
    ) -> std::result::Result<(), Self::Error>;

    /// The instances whose premises are all base facts, at the new snapshot.
    fn axioms(
        &mut self,
        cx: &mut Self::Cx<'_>,
        store: &Self::Store,
        out: &mut Sink<'_, (Self::Fact, Self::Record), Self::Error>,
    ) -> std::result::Result<(), Self::Error>;

    /// The instances at snapshot `w` that use derived fact `f` as a premise, with their
    /// other derived premises held.
    fn consequences(
        &mut self,
        cx: &mut Self::Cx<'_>,
        store: &Self::Store,
        w: Which,
        f: &Self::Fact,
        held: &dyn Fn(&Self::Fact) -> bool,
        out: &mut Sink<'_, (Self::Fact, Self::Record), Self::Error>,
    ) -> std::result::Result<(), Self::Error>;

    /// [`consequences`](Self::consequences) of each of `facts`.
    fn consequences_batch(
        &mut self,
        cx: &mut Self::Cx<'_>,
        store: &Self::Store,
        w: Which,
        facts: &[Self::Fact],
        held: &dyn Fn(&Self::Fact) -> bool,
        out: &mut Sink<'_, (Self::Fact, Self::Record), Self::Error>,
    ) -> std::result::Result<(), Self::Error> {
        for f in facts {
            self.consequences(cx, store, w, f, held, out)?;
        }
        Ok(())
    }

    /// One instance at the new snapshot that derives `f` from base facts and held
    /// derived facts, if there is one. In exact mode, whether `f` follows from the base
    /// facts through instances whose derived premises the store holds, whatever `held`
    /// says.
    fn prove(
        &mut self,
        cx: &mut Self::Cx<'_>,
        store: &Self::Store,
        f: &Self::Fact,
        held: &dyn Fn(&Self::Fact) -> bool,
    ) -> std::result::Result<Option<Self::Record>, Self::Error>;

    /// [`prove`](Self::prove) for each of `facts`, in their order, into `out`.
    fn prove_batch(
        &mut self,
        cx: &mut Self::Cx<'_>,
        store: &Self::Store,
        facts: &[Self::Fact],
        held: &dyn Fn(&Self::Fact) -> bool,
        out: &mut Vec<Option<Self::Record>>,
    ) -> std::result::Result<(), Self::Error> {
        for f in facts {
            out.push(self.prove(cx, store, f, held)?);
        }
        Ok(())
    }

    /// The facts whose recorded instance, as `recorded` gives it, uses a base fact the
    /// changes removed. One-step mode only.
    fn broken(
        &mut self,
        cx: &mut Self::Cx<'_>,
        store: &Self::Store,
        recorded: &dyn Fn(&Self::Fact) -> Option<Self::Record>,
        out: &mut Sink<'_, Self::Fact, Self::Error>,
    ) -> std::result::Result<(), Self::Error>;

    /// The instances that use a base fact the changes added, with their derived
    /// premises held.
    fn added(
        &mut self,
        cx: &mut Self::Cx<'_>,
        store: &Self::Store,
        held: &dyn Fn(&Self::Fact) -> bool,
        out: &mut Sink<'_, (Self::Fact, Self::Record), Self::Error>,
    ) -> std::result::Result<(), Self::Error>;

    /// The held facts whose recorded instance's base facts hold before and after the
    /// changes with another annotation. Only annotated maintenance asks.
    fn moved(
        &mut self,
        _cx: &mut Self::Cx<'_>,
        _store: &Self::Store,
        _recorded: &dyn Fn(&Self::Fact) -> Option<Self::Record>,
        _out: &mut Sink<'_, Self::Fact, Self::Error>,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    /// Exact mode: apply the program's preamble to the store before anything is deleted.
    fn prepare(
        &mut self,
        _cx: &mut Self::Cx<'_>,
        _store: &mut Self::Store,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    /// Exact mode: the first frontier, the facts that the changes may have made
    /// unprovable, after [`prepare`](Self::prepare).
    fn frontier(
        &mut self,
        _cx: &mut Self::Cx<'_>,
        _store: &Self::Store,
        _out: &mut Sink<'_, Self::Fact, Self::Error>,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    /// Exact mode: insert the added facts and everything that follows from them, and
    /// return `true`, or return `false` to leave it to the operator, which inserts the
    /// facts [`added`](Self::added) gives and follows
    /// [`consequences`](Self::consequences) from every fact it inserts.
    fn saturate(
        &mut self,
        _cx: &mut Self::Cx<'_>,
        _store: &mut Self::Store,
    ) -> std::result::Result<bool, Self::Error> {
        Ok(false)
    }

    /// Exact mode, after [`saturate`](Self::saturate) returned `true`: the facts it
    /// inserted.
    fn saturated(
        &mut self,
        _cx: &mut Self::Cx<'_>,
        _store: &Self::Store,
        _out: &mut Sink<'_, Self::Fact, Self::Error>,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    /// A pass or a build ended, with success or not: drop what the program kept for it.
    fn pass_end(&mut self, _cx: &mut Self::Cx<'_>) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    /// The annotation of fact `f` derived by instance `r`, given the annotations of its
    /// derived premises in the order of [`premises`](Self::premises).
    fn annotate(
        &mut self,
        cx: &mut Self::Cx<'_>,
        store: &Self::Store,
        mode: Provenance,
        f: &Self::Fact,
        r: &Self::Record,
        premises: &[&Ann],
    ) -> std::result::Result<Ann, Self::Error>;

    /// Check the deadline and the cancel flag.
    fn tick(&mut self, cx: &mut Self::Cx<'_>) -> std::result::Result<(), Self::Error>;

    /// Write one fact with its record. A program that gives terms local ids must be
    /// saved through a writer over the terms that hold those ids.
    fn save(&self, w: &mut Writer, f: &Self::Fact, r: &Self::Record) -> Result<()>;

    /// Read what [`save`](Self::save) wrote.
    fn load(&self, r: &mut Reader) -> Result<(Self::Fact, Self::Record)>;

    /// The approximate memory the program holds for the current pass beyond its facts,
    /// such as memoized lookups, which counts against the budget while the pass runs.
    fn scratch_bytes(&self) -> u64 {
        0
    }
}

/// A program that makes an empty store of its own, which [`FixedPoint::new`] starts
/// from. A program whose store wraps state it does not own, such as the reasoner's
/// closure, starts from [`FixedPoint::with_store`] instead.
pub trait NewStore: Program {
    /// An empty store for the program's facts.
    fn new_store(&self) -> Self::Store;
}

/// One change of the operator's facts: the fact, its weight's change (`+1` when it
/// starts to hold, `−1` when it stops, `0` when only its annotation moved), and the
/// change of its annotation in annotated maintenance.
#[derive(Clone, Debug)]
pub struct Change<F> {
    pub fact: F,
    pub weight: i64,
    pub ann: Option<Ann>,
}

/// The change of membership that `changes` make, with each fact once and its weights
/// summed, leaving out the facts whose weights cancel, such as a fact that an exact pass
/// deleted and derived again. Annotations are not carried. A sum past the range of
/// `i64` saturates.
pub fn consolidate<F: Copy + Eq + Hash>(changes: &[Change<F>]) -> Vec<(F, i64)> {
    let mut sum: FxHashMap<F, i128> = FxHashMap::default();
    let mut order = Vec::new();
    for c in changes {
        let e = sum.entry(c.fact).or_insert_with(|| {
            order.push(c.fact);
            0
        });
        *e += i128::from(c.weight);
    }
    order
        .into_iter()
        .filter_map(|f| {
            let w = sum[&f];
            (w != 0).then(|| {
                (
                    f,
                    w.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64,
                )
            })
        })
        .collect()
}

/// The state of a fixed-point operator: the facts that hold at the latest snapshot
/// with their records, and their annotations when it keeps them.
#[derive(Clone, Debug)]
pub struct FixedPoint<P: Program> {
    pub program: P,
    store: P::Store,
    ann: FxHashMap<P::Fact, Ann>,
    ann_bytes: u64,
    mode: Provenance,
}

/// The atoms of an annotation of one derivation: its support's atoms for `why`, its
/// atoms for `lineage`, none for `count`.
pub fn atoms_of(a: &Ann) -> Vec<Atom> {
    match a {
        Ann::Count(_) => Vec::new(),
        Ann::Lineage(l) => {
            let mut v: Vec<Atom> = l.atoms.keys().copied().collect();
            v.sort_unstable();
            v
        }
        Ann::Why(w) => w
            .supports
            .iter()
            .flat_map(|(x, _)| x.atoms().iter().copied())
            .collect(),
    }
}

/// What a pass reserved through the store and has not released yet, released when the
/// pass ends.
#[derive(Default)]
struct Charges {
    live: u64,
}

/// A buffer of a pass with the bytes reserved for its allocation. It grows only through
/// [`FixedPoint::room`], and is read through `Deref`.
struct Held<C> {
    buf: C,
    bytes: u64,
}

impl<C: Default> Default for Held<C> {
    fn default() -> Self {
        Held {
            buf: C::default(),
            bytes: 0,
        }
    }
}

impl<C> std::ops::Deref for Held<C> {
    type Target = C;

    fn deref(&self) -> &C {
        &self.buf
    }
}

impl<P: NewStore> FixedPoint<P> {
    pub fn new(program: P) -> FixedPoint<P> {
        let store = program.new_store();
        FixedPoint::with_store(program, store)
    }
}

impl<P: Program> FixedPoint<P> {
    /// A fixed point over `store`, which holds what it holds; call
    /// [`build`](Self::build) or [`load`](Self::load) unless it is the program's closure.
    pub fn with_store(program: P, store: P::Store) -> FixedPoint<P> {
        FixedPoint {
            program,
            store,
            ann: FxHashMap::default(),
            ann_bytes: 0,
            mode: Provenance::None,
        }
    }

    /// Keep annotation `mode` from the next build on. A program in exact mode keeps
    /// none, and its builds and passes fail with another mode.
    pub fn set_provenance(&mut self, mode: Provenance) {
        self.mode = mode;
        self.ann.clear();
        self.ann_bytes = 0;
    }

    pub fn provenance(&self) -> Provenance {
        self.mode
    }

    pub fn store(&self) -> &P::Store {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut P::Store {
        &mut self.store
    }

    /// The facts that hold, with their records.
    pub fn facts(&self) -> Vec<(P::Fact, P::Record)> {
        let mut out = Vec::with_capacity(self.store.len());
        self.store.for_each(&mut |f, r| out.push((*f, *r)));
        out
    }

    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    pub fn holds(&self, f: &P::Fact) -> bool {
        self.store.contains(f)
    }

    pub fn record(&self, f: &P::Fact) -> Option<P::Record> {
        self.store.record(f)
    }

    /// A held fact's annotation, in annotated maintenance.
    pub fn annotation(&self, f: &P::Fact) -> Option<&Ann> {
        self.ann.get(f)
    }

    /// The approximate memory of the state.
    pub fn bytes(&self) -> u64 {
        self.store
            .bytes()
            .saturating_add(self.ann_bytes)
            .saturating_add(self.ann.len() as u64 * 16)
    }

    /// What counts against `max_bytes` beside a producer's sink: the state and the
    /// program's scratch, or `None` past `u64::MAX`.
    fn used_checked(&self) -> Option<u64> {
        self.store
            .bytes()
            .checked_add(self.ann_bytes)?
            .checked_add(self.ann.len() as u64 * 16)?
            .checked_add(self.program.scratch_bytes())
    }

    fn used(&self) -> u64 {
        self.used_checked().unwrap_or(u64::MAX)
    }

    /// Check the budget every 4096 facts.
    fn over(&self, max: Option<u64>) -> std::result::Result<(), P::Error> {
        if self.store.len().is_multiple_of(4096) {
            return self.check(max, 0);
        }
        Ok(())
    }

    /// Fail when the state, the program's scratch and `pending` bytes not yet stored
    /// pass `max`.
    fn check(&self, max: Option<u64>, pending: u64) -> std::result::Result<(), P::Error> {
        let Some(m) = max else {
            return Ok(());
        };
        let used = self.used_checked().and_then(|u| u.checked_add(pending));
        if used.is_none_or(|u| u > m) {
            return Err(over(m, used.unwrap_or(u64::MAX)).into());
        }
        Ok(())
    }

    fn refuse_annotations(&self) -> std::result::Result<(), P::Error> {
        if P::EXACT && !self.mode.is_none() {
            return Err(Error::Invalid(
                "a fixed point in exact mode keeps no records, so it keeps no annotations".into(),
            )
            .into());
        }
        Ok(())
    }

    /// Run `body` as one pass: refuse what the mode cannot keep, release what the pass
    /// reserved and tell the program it ended, whether it succeeded or not.
    fn pass<T>(
        &mut self,
        cx: &mut P::Cx<'_>,
        body: impl FnOnce(&mut Self, &mut P::Cx<'_>, &mut Charges) -> std::result::Result<T, P::Error>,
    ) -> std::result::Result<T, P::Error> {
        let mut charges = Charges::default();
        let out = match self.refuse_annotations() {
            Ok(()) => body(self, cx, &mut charges),
            Err(e) => Err(e),
        };
        self.store.release(charges.live);
        let end = self.program.pass_end(cx);
        let out = out?;
        end?;
        Ok(out)
    }

    /// Make room in `h` for `add` more items, reserving the new allocation through the
    /// store before it grows and releasing the old one after.
    #[inline]
    fn room<B: Buf>(
        &self,
        charges: &mut Charges,
        h: &mut Held<B>,
        add: usize,
    ) -> std::result::Result<(), P::Error> {
        // most calls add one item to a buffer with room for it
        if h.buf.grown(add).is_none() {
            return Ok(());
        }
        self.grow(charges, h, add)
    }

    /// [`room`](Self::room) when `h` has to grow.
    #[inline(never)]
    fn grow<B: Buf>(
        &self,
        charges: &mut Charges,
        h: &mut Held<B>,
        add: usize,
    ) -> std::result::Result<(), P::Error> {
        let store = &self.store;
        let reserve = |b: u64| store.reserve(b);
        let release = |b: u64| store.release(b);
        let acct = Acct {
            reserve: &reserve,
            release: &release,
        };
        let before = h.bytes;
        let res = grow_charged(Some(acct), None, &mut h.buf, &mut h.bytes, add);
        charges.live = charges.live.saturating_sub(before).saturating_add(h.bytes);
        res
    }

    /// Free `h` and release its reservation.
    fn free<C>(&self, charges: &mut Charges, h: Held<C>) {
        self.store.release(h.bytes);
        charges.live = charges.live.saturating_sub(h.bytes);
    }

    /// Insert `f` with record `r` when the store does not hold it. Returns whether it
    /// was inserted.
    fn insert_new(
        &mut self,
        f: P::Fact,
        r: P::Record,
        max: Option<u64>,
    ) -> std::result::Result<bool, P::Error> {
        if !self.store.insert(f, r)? {
            return Ok(false);
        }
        self.over(max)?;
        Ok(true)
    }

    /// Run `produce` with a sink over `buf`, which keeps its capacity and reservation, so
    /// that what it finds is in `buf`. The sink's allocation counts against `max` beside
    /// the state and the program's scratch, and is reserved through the store.
    fn produce<T>(
        &mut self,
        cx: &mut P::Cx<'_>,
        max: Option<u64>,
        charges: &mut Charges,
        buf: &mut Held<Vec<T>>,
        produce: impl FnOnce(
            &mut P,
            &mut P::Cx<'_>,
            &P::Store,
            &mut Sink<'_, T, P::Error>,
        ) -> std::result::Result<(), P::Error>,
    ) -> std::result::Result<(), P::Error> {
        // the state counted beside the sink, read only with a limit
        let limit = max.map(|m| (m, self.used()));
        if limit.is_some() && buf.buf.capacity() > 0 {
            self.check(max, buf.buf.alloc_bytes())?;
        }
        let store = &self.store;
        let reserve = |b: u64| store.reserve(b);
        let release = |b: u64| store.release(b);
        let held = buf.bytes;
        let mut sink = Sink {
            buf: std::mem::take(&mut buf.buf),
            acct: Some(Acct {
                reserve: &reserve,
                release: &release,
            }),
            max: limit,
            held,
        };
        let res = produce(&mut self.program, cx, store, &mut sink);
        buf.buf = sink.buf;
        buf.bytes = sink.held;
        charges.live = charges.live.saturating_sub(held).saturating_add(buf.bytes);
        res?;
        // the program's scratch may have grown while the sink filled its capacity
        self.check(max, buf.buf.alloc_bytes())
    }

    /// The operator's own saturation: follow [`Program::consequences`] from every fact of
    /// `queue`, inserting the facts not held, in one-step mode reviving the pending
    /// candidates it derives. Returns the facts inserted, in order, in `added`, and the
    /// candidates proved, in `proved`.
    #[allow(clippy::too_many_arguments)]
    fn chase(
        &mut self,
        cx: &mut P::Cx<'_>,
        max: Option<u64>,
        charges: &mut Charges,
        queue: &mut Held<VecDeque<P::Fact>>,
        pending: &mut Held<FxHashSet<P::Fact>>,
        proved: &mut Held<Vec<P::Fact>>,
        added: &mut Held<Vec<P::Fact>>,
    ) -> std::result::Result<(), P::Error> {
        let n = self.program.batch_len().max(1);
        let mut found: Held<Vec<(P::Fact, P::Record)>> = Held::default();
        let mut batch: Held<Vec<P::Fact>> = Held::default();
        while let Some(first) = queue.buf.pop_front() {
            self.program.tick(cx)?;
            // a batch of one fact needs no buffer
            let facts: &[P::Fact] = if n == 1 {
                std::slice::from_ref(&first)
            } else {
                batch.buf.clear();
                self.room(charges, &mut batch, n.min(queue.len() + 1))?;
                batch.buf.push(first);
                while batch.len() < n
                    && let Some(f) = queue.buf.pop_front()
                {
                    batch.buf.push(f);
                }
                &batch.buf
            };
            {
                let pend = &pending.buf;
                self.produce(cx, max, charges, &mut found, |p, cx, store, sink| {
                    p.consequences_batch(
                        cx,
                        store,
                        Which::New,
                        facts,
                        &|x| store.contains(x) && !pend.contains(x),
                        sink,
                    )
                })?;
            }
            for (g, r) in found.buf.drain(..) {
                self.take(g, r, max, charges, queue, pending, proved, added)?;
            }
        }
        self.free(charges, found);
        self.free(charges, batch);
        Ok(())
    }

    /// Hold `g` with record `r`: a pending candidate is proved, a fact not held is
    /// added, and either joins `queue`.
    #[allow(clippy::too_many_arguments)]
    fn take(
        &mut self,
        g: P::Fact,
        r: P::Record,
        max: Option<u64>,
        charges: &mut Charges,
        queue: &mut Held<VecDeque<P::Fact>>,
        pending: &mut Held<FxHashSet<P::Fact>>,
        proved: &mut Held<Vec<P::Fact>>,
        added: &mut Held<Vec<P::Fact>>,
    ) -> std::result::Result<(), P::Error> {
        if !pending.buf.is_empty() && pending.buf.remove(&g) {
            self.store.set_record(g, r)?;
            self.room(charges, proved, 1)?;
            proved.buf.push(g);
        } else if self.insert_new(g, r, max)? {
            self.room(charges, added, 1)?;
            added.buf.push(g);
        } else {
            return Ok(());
        }
        self.room(charges, queue, 1)?;
        queue.buf.push_back(g);
        Ok(())
    }

    /// Derive every fact at the new snapshot from nothing, and return them all as
    /// additions.
    pub fn build(
        &mut self,
        cx: &mut P::Cx<'_>,
        max_bytes: Option<u64>,
    ) -> std::result::Result<Vec<Change<P::Fact>>, P::Error> {
        self.pass(cx, |s, cx, charges| s.build_pass(cx, max_bytes, charges))
    }

    fn build_pass(
        &mut self,
        cx: &mut P::Cx<'_>,
        max_bytes: Option<u64>,
        charges: &mut Charges,
    ) -> std::result::Result<Vec<Change<P::Fact>>, P::Error> {
        self.store.clear()?;
        self.ann.clear();
        self.ann_bytes = 0;
        let mut found: Held<Vec<(P::Fact, P::Record)>> = Held::default();
        self.produce(cx, max_bytes, charges, &mut found, |p, cx, store, sink| {
            p.axioms(cx, store, sink)
        })?;
        let mut queue: Held<VecDeque<P::Fact>> = Held::default();
        let mut order: Held<Vec<P::Fact>> = Held::default();
        for (f, r) in found.buf.drain(..) {
            if self.insert_new(f, r, max_bytes)? {
                self.room(charges, &mut order, 1)?;
                self.room(charges, &mut queue, 1)?;
                queue.buf.push_back(f);
                order.buf.push(f);
            }
        }
        self.free(charges, found);
        let mut pending = Held::default();
        let mut proved = Held::default();
        self.chase(
            cx,
            max_bytes,
            charges,
            &mut queue,
            &mut pending,
            &mut proved,
            &mut order,
        )?;
        self.free(charges, queue);
        let anns = if self.mode.is_none() {
            None
        } else {
            let mut dirty: Held<FxHashSet<P::Fact>> = Held::default();
            self.room(charges, &mut dirty, order.len())?;
            dirty.buf.extend(order.iter().copied());
            let anns = self.annotate(cx, charges, &dirty, max_bytes)?;
            self.free(charges, dirty);
            anns
        };
        let mut out: Held<Vec<Change<P::Fact>>> = Held::default();
        self.room(charges, &mut out, order.len())?;
        for &f in order.iter() {
            let ann = match anns.as_ref() {
                Some(m) => {
                    let a = m.get(&f).cloned().ok_or_else(|| {
                        Error::Corrupt("a fixed point did not annotate a fact it built".into())
                    })?;
                    self.store_ann(f, a.clone());
                    Some(a)
                }
                None => None,
            };
            out.buf.push(Change {
                fact: f,
                weight: 1,
                ann,
            });
        }
        self.check(max_bytes, 0)?;
        Ok(out.buf)
    }

    fn store_ann(&mut self, f: P::Fact, a: Ann) {
        self.ann_bytes += a.bytes();
        if let Some(old) = self.ann.insert(f, a) {
            self.ann_bytes -= old.bytes();
        }
    }

    fn drop_ann(&mut self, f: &P::Fact) -> Option<Ann> {
        let old = self.ann.remove(f);
        if let Some(o) = &old {
            self.ann_bytes -= o.bytes();
        }
        old
    }

    /// The annotations of the facts of `dirty` from their records, premises first; the
    /// other facts keep the annotations held. `None` without annotations. The
    /// annotations made count against `max` with the state, and the walk's tables,
    /// premises and references are reserved through the store.
    fn annotate(
        &mut self,
        cx: &mut P::Cx<'_>,
        charges: &mut Charges,
        dirty: &FxHashSet<P::Fact>,
        max: Option<u64>,
    ) -> std::result::Result<Option<FxHashMap<P::Fact, Ann>>, P::Error> {
        if self.mode.is_none() {
            return Ok(None);
        }
        let mut done: Held<FxHashMap<P::Fact, Ann>> = Held::default();
        let mut pending = 0u64;
        // a depth-first walk over the records: a frame is a fact and whether its
        // premises were scheduled, and `open` holds the facts whose premises are being
        // annotated, so meeting one of them again is a cycle of records
        let mut open: Held<FxHashSet<P::Fact>> = Held::default();
        let mut stack: Held<Vec<(P::Fact, bool)>> = Held::default();
        let mut scheduled: Held<FxHashSet<P::Fact>> = Held::default();
        let mut prem: Held<Vec<P::Fact>> = Held::default();
        for &f in dirty {
            if done.contains_key(&f) {
                continue;
            }
            self.room(charges, &mut stack, 1)?;
            stack.buf.push((f, false));
            while let Some(&(top, expanded)) = stack.last() {
                if !expanded && done.contains_key(&top) {
                    stack.buf.pop();
                    continue;
                }
                let r = self.store.record(&top).ok_or_else(|| {
                    Error::Corrupt("a fixed point annotates a fact it does not hold".into())
                })?;
                prem.buf.clear();
                self.produce(cx, max, charges, &mut prem, |p, _cx, _store, sink| {
                    p.premises(&top, &r, sink)
                })?;
                if !expanded {
                    self.room(charges, &mut open, 1)?;
                    if !open.buf.insert(top) {
                        return Err(
                            Error::Corrupt("a fixed point's records form a cycle".into()).into(),
                        );
                    }
                    stack.buf.last_mut().expect("the frame").1 = true;
                    scheduled.buf.clear();
                    for p in prem.iter() {
                        if !dirty.contains(p) || done.contains_key(p) {
                            continue;
                        }
                        self.room(charges, &mut scheduled, 1)?;
                        if scheduled.buf.insert(*p) {
                            if open.contains(p) {
                                return Err(Error::Corrupt(
                                    "a fixed point's records form a cycle".into(),
                                )
                                .into());
                            }
                            self.room(charges, &mut stack, 1)?;
                            stack.buf.push((*p, false));
                        }
                    }
                    continue;
                }
                // the premises' annotations, reserved for the call that reads them
                let refs = slots_bytes::<&Ann>(prem.len());
                self.store.reserve(refs)?;
                charges.live = charges.live.saturating_add(refs);
                let a = {
                    let mut anns: Vec<&Ann> = Vec::with_capacity(prem.len());
                    for p in prem.iter() {
                        let a = done.get(p).or_else(|| self.ann.get(p)).ok_or_else(|| {
                            Error::Corrupt(
                                "a fixed point's record names a fact without annotation".into(),
                            )
                        })?;
                        anns.push(a);
                    }
                    self.program
                        .annotate(cx, &self.store, self.mode, &top, &r, &anns)?
                };
                self.store.release(refs);
                charges.live = charges.live.saturating_sub(refs);
                pending = pending.saturating_add(a.bytes() + 16);
                self.room(charges, &mut done, 1)?;
                done.buf.insert(top, a);
                open.buf.remove(&top);
                stack.buf.pop();
                if done.len().is_multiple_of(4096) {
                    self.check(max, pending)?;
                }
            }
        }
        self.check(max, pending)?;
        self.free(charges, open);
        self.free(charges, stack);
        self.free(charges, scheduled);
        self.free(charges, prem);
        // the table's reservation stays with the pass, which releases it at its end
        Ok(Some(done.buf))
    }

    /// Apply the changes that the context lists, and return the change of the facts.
    pub fn advance(
        &mut self,
        cx: &mut P::Cx<'_>,
        max_bytes: Option<u64>,
    ) -> std::result::Result<Vec<Change<P::Fact>>, P::Error> {
        if P::EXACT {
            self.pass(cx, |s, cx, charges| s.advance_exact(cx, max_bytes, charges))
        } else {
            self.pass(cx, |s, cx, charges| {
                s.advance_one_step(cx, max_bytes, charges)
            })
        }
    }

    /// A pass of the exact mode (see the module's documentation).
    fn advance_exact(
        &mut self,
        cx: &mut P::Cx<'_>,
        max_bytes: Option<u64>,
        charges: &mut Charges,
    ) -> std::result::Result<Vec<Change<P::Fact>>, P::Error> {
        let n = self.program.batch_len().max(1);
        self.program.prepare(cx, &mut self.store)?;
        let mut frontier: Held<Vec<P::Fact>> = Held::default();
        self.produce(
            cx,
            max_bytes,
            charges,
            &mut frontier,
            |p, cx, store, sink| p.frontier(cx, store, sink),
        )?;
        let mut next: Held<Vec<P::Fact>> = Held::default();
        let mut proved: Held<FxHashSet<P::Fact>> = Held::default();
        let mut out: Held<Vec<Change<P::Fact>>> = Held::default();
        let mut verdicts: Held<Vec<Option<P::Record>>> = Held::default();
        let mut found: Held<Vec<(P::Fact, P::Record)>> = Held::default();
        let mut seen: Held<FxHashSet<P::Fact>> = Held::default();
        let mut dead: Held<Vec<P::Fact>> = Held::default();
        loop {
            // the frontier, once each, without the facts gone or proved
            seen.buf.clear();
            self.room(charges, &mut seen, frontier.len())?;
            {
                let (store, done, once) = (&self.store, &proved.buf, &mut seen.buf);
                frontier
                    .buf
                    .retain(|f| store.contains(f) && !done.contains(f) && once.insert(*f));
            }
            if frontier.is_empty() {
                break;
            }
            dead.buf.clear();
            for batch in frontier.chunks(n) {
                self.program.tick(cx)?;
                verdicts.buf.clear();
                self.room(charges, &mut verdicts, batch.len())?;
                let store = &self.store;
                self.program.prove_batch(
                    cx,
                    store,
                    batch,
                    &|x| store.contains(x),
                    &mut verdicts.buf,
                )?;
                if verdicts.len() != batch.len() {
                    return Err(Error::Corrupt(
                        "a fixed point's program did not decide every fact of a batch".into(),
                    )
                    .into());
                }
                for (f, v) in batch.iter().zip(verdicts.iter()) {
                    if v.is_some() {
                        self.room(charges, &mut proved, 1)?;
                        proved.buf.insert(*f);
                    } else {
                        self.room(charges, &mut dead, 1)?;
                        dead.buf.push(*f);
                    }
                }
            }
            if dead.is_empty() {
                break;
            }
            // the heads of the instances that read a dead fact, while the dead facts are
            // still held
            next.buf.clear();
            for batch in dead.chunks(n) {
                self.program.tick(cx)?;
                self.produce(cx, max_bytes, charges, &mut found, |p, cx, store, sink| {
                    p.consequences_batch(cx, store, Which::Old, batch, &|x| store.contains(x), sink)
                })?;
                self.room(charges, &mut next, found.len())?;
                next.buf.extend(found.buf.drain(..).map(|(g, _)| g));
            }
            self.room(charges, &mut out, dead.len())?;
            for f in dead.iter() {
                if self.store.remove(f)? {
                    out.buf.push(Change {
                        fact: *f,
                        weight: -1,
                        ann: None,
                    });
                }
            }
            std::mem::swap(&mut frontier, &mut next);
        }
        self.free(charges, seen);
        self.free(charges, frontier);
        self.free(charges, next);
        self.free(charges, dead);
        self.free(charges, verdicts);
        self.free(charges, proved);
        let mut inserted: Held<Vec<P::Fact>> = Held::default();
        if self.program.saturate(cx, &mut self.store)? {
            self.produce(
                cx,
                max_bytes,
                charges,
                &mut inserted,
                |p, cx, store, sink| p.saturated(cx, store, sink),
            )?;
        } else {
            self.produce(cx, max_bytes, charges, &mut found, |p, cx, store, sink| {
                p.added(cx, store, &|x| store.contains(x), sink)
            })?;
            let mut queue: Held<VecDeque<P::Fact>> = Held::default();
            for (g, r) in found.buf.drain(..) {
                if self.insert_new(g, r, max_bytes)? {
                    self.room(charges, &mut inserted, 1)?;
                    inserted.buf.push(g);
                    self.room(charges, &mut queue, 1)?;
                    queue.buf.push_back(g);
                }
            }
            let mut pending = Held::default();
            let mut revived = Held::default();
            self.chase(
                cx,
                max_bytes,
                charges,
                &mut queue,
                &mut pending,
                &mut revived,
                &mut inserted,
            )?;
            self.free(charges, queue);
        }
        self.free(charges, found);
        self.room(charges, &mut out, inserted.len())?;
        out.buf.extend(inserted.buf.drain(..).map(|f| Change {
            fact: f,
            weight: 1,
            ann: None,
        }));
        self.check(max_bytes, 0)?;
        Ok(out.buf)
    }

    /// A pass of the one-step mode (see the module's documentation).
    fn advance_one_step(
        &mut self,
        cx: &mut P::Cx<'_>,
        max_bytes: Option<u64>,
        charges: &mut Charges,
    ) -> std::result::Result<Vec<Change<P::Fact>>, P::Error> {
        let n = self.program.batch_len().max(1);
        // 1. the broken facts, and every fact whose record depends on them, at the
        //    snapshot before the changes
        let mut broken: Held<Vec<P::Fact>> = Held::default();
        self.produce(cx, max_bytes, charges, &mut broken, |p, cx, store, sink| {
            p.broken(cx, store, &|f| store.record(f), sink)
        })?;
        let mut pending: Held<FxHashSet<P::Fact>> = Held::default();
        let mut candidates: Held<Vec<P::Fact>> = Held::default();
        let mut found: Held<Vec<(P::Fact, P::Record)>> = Held::default();
        let mut batch: Held<Vec<P::Fact>> = Held::default();
        while !broken.is_empty() {
            batch.buf.clear();
            let mut one = None;
            while batch.len() < n
                && one.is_none()
                && let Some(f) = broken.buf.pop()
            {
                if !self.store.contains(&f) || pending.contains(&f) {
                    continue;
                }
                self.room(charges, &mut pending, 1)?;
                pending.buf.insert(f);
                self.room(charges, &mut candidates, 1)?;
                candidates.buf.push(f);
                // a batch of one fact needs no buffer
                if n == 1 {
                    one = Some(f);
                } else {
                    self.room(charges, &mut batch, 1)?;
                    batch.buf.push(f);
                }
            }
            if batch.is_empty() && one.is_none() {
                continue;
            }
            self.program.tick(cx)?;
            {
                let facts: &[P::Fact] = match &one {
                    Some(f) => std::slice::from_ref(f),
                    None => &batch.buf,
                };
                self.produce(cx, max_bytes, charges, &mut found, |p, cx, store, sink| {
                    p.consequences_batch(cx, store, Which::Old, facts, &|x| store.contains(x), sink)
                })?;
            }
            for (g, r) in found.buf.drain(..) {
                if self.store.record(&g) == Some(r) && !pending.contains(&g) {
                    self.room(charges, &mut broken, 1)?;
                    broken.buf.push(g);
                }
            }
        }
        self.free(charges, broken);
        self.free(charges, batch);
        let mut queue: Held<VecDeque<P::Fact>> = Held::default();
        let mut proved: Held<Vec<P::Fact>> = Held::default();
        let mut added: Held<Vec<P::Fact>> = Held::default();
        // 2. a backward check of each candidate against the facts that hold
        let mut verdicts: Held<Vec<Option<P::Record>>> = Held::default();
        for chunk in candidates.chunks(n) {
            self.program.tick(cx)?;
            verdicts.buf.clear();
            self.room(charges, &mut verdicts, chunk.len())?;
            {
                let (store, pend) = (&self.store, &pending.buf);
                let held = |x: &P::Fact| store.contains(x) && !pend.contains(x);
                if n == 1
                    && let [f] = chunk
                {
                    // a batch of one fact, without the batched form's loop
                    let v = self.program.prove(cx, store, f, &held)?;
                    verdicts.buf.push(v);
                } else {
                    self.program
                        .prove_batch(cx, store, chunk, &held, &mut verdicts.buf)?;
                }
            }
            if verdicts.len() != chunk.len() {
                return Err(Error::Corrupt(
                    "a fixed point's program did not decide every fact of a batch".into(),
                )
                .into());
            }
            for (&f, v) in chunk.iter().zip(verdicts.buf.drain(..)) {
                if let Some(r) = v {
                    pending.buf.remove(&f);
                    self.store.set_record(f, r)?;
                    self.room(charges, &mut proved, 1)?;
                    proved.buf.push(f);
                    self.room(charges, &mut queue, 1)?;
                    queue.buf.push_back(f);
                }
            }
        }
        self.free(charges, verdicts);
        // 3. the instances of the added base facts
        {
            let pend = &pending.buf;
            self.produce(cx, max_bytes, charges, &mut found, |p, cx, store, sink| {
                p.added(cx, store, &|x| store.contains(x) && !pend.contains(x), sink)
            })?;
        }
        for (g, r) in found.buf.drain(..) {
            self.take(
                g,
                r,
                max_bytes,
                charges,
                &mut queue,
                &mut pending,
                &mut proved,
                &mut added,
            )?;
        }
        // 4. forward chaining from what was kept or derived; a program that saturates
        //    by itself would leave pending candidates unproved
        if self.program.saturate(cx, &mut self.store)? {
            return Err(Error::Invalid(
                "a fixed point's program saturates by itself in one-step mode".into(),
            )
            .into());
        }
        self.chase(
            cx,
            max_bytes,
            charges,
            &mut queue,
            &mut pending,
            &mut proved,
            &mut added,
        )?;
        self.free(charges, queue);
        // 5. the candidates without a derivation
        let mut out: Held<Vec<Change<P::Fact>>> = Held::default();
        for f in candidates.iter() {
            if pending.contains(f) {
                self.store.remove(f)?;
                let ann = self.drop_ann(f).map(|a| a.scaled(-1));
                self.room(charges, &mut out, 1)?;
                out.buf.push(Change {
                    fact: *f,
                    weight: -1,
                    ann,
                });
            }
        }
        self.free(charges, candidates);
        self.free(charges, pending);
        if self.mode.is_none() {
            self.room(charges, &mut out, added.len())?;
            out.buf.extend(added.buf.drain(..).map(|f| Change {
                fact: f,
                weight: 1,
                ann: None,
            }));
            self.check(max_bytes, 0)?;
            return Ok(out.buf);
        }
        // the annotations of the facts whose records changed, of the facts whose
        // recorded base facts moved, and of every fact whose record depends on them
        let mut dirty: Held<FxHashSet<P::Fact>> = Held::default();
        self.room(
            charges,
            &mut dirty,
            proved.len().saturating_add(added.len()),
        )?;
        dirty.buf.extend(proved.iter().chain(added.iter()).copied());
        let mut moved: Held<Vec<P::Fact>> = Held::default();
        self.produce(cx, max_bytes, charges, &mut moved, |p, cx, store, sink| {
            p.moved(cx, store, &|f| store.record(f), sink)
        })?;
        let mut stack: Held<Vec<P::Fact>> = Held::default();
        self.room(
            charges,
            &mut stack,
            moved.len().saturating_add(proved.len()),
        )?;
        {
            let store = &self.store;
            stack.buf.extend(
                moved
                    .iter()
                    .copied()
                    .filter(|f| store.contains(f))
                    .chain(proved.iter().copied()),
            );
        }
        self.free(charges, moved);
        let mut seen: Held<FxHashSet<P::Fact>> = Held::default();
        while let Some(f) = stack.buf.pop() {
            self.room(charges, &mut seen, 1)?;
            if !seen.buf.insert(f) {
                continue;
            }
            self.room(charges, &mut dirty, 1)?;
            dirty.buf.insert(f);
            self.program.tick(cx)?;
            self.produce(cx, max_bytes, charges, &mut found, |p, cx, store, sink| {
                p.consequences(cx, store, Which::New, &f, &|x| store.contains(x), sink)
            })?;
            for (g, r) in found.buf.drain(..) {
                if self.store.record(&g) == Some(r) {
                    self.room(charges, &mut stack, 1)?;
                    stack.buf.push(g);
                }
            }
        }
        self.free(charges, stack);
        self.free(charges, seen);
        self.free(charges, found);
        let anns = self
            .annotate(cx, charges, &dirty, max_bytes)?
            .unwrap_or_default();
        let mut fresh: Held<FxHashSet<P::Fact>> = Held::default();
        self.room(charges, &mut fresh, added.len())?;
        fresh.buf.extend(added.iter().copied());
        for &f in dirty.iter() {
            let Some(a) = anns.get(&f).cloned() else {
                continue;
            };
            let old = self.ann.get(&f).cloned();
            let d = match &old {
                Some(o) => a.sub(o),
                None => a.clone(),
            };
            let weight = i64::from(fresh.contains(&f));
            if weight == 0 && d.is_zero() {
                continue;
            }
            self.store_ann(f, a);
            self.room(charges, &mut out, 1)?;
            out.buf.push(Change {
                fact: f,
                weight,
                ann: Some(d),
            });
        }
        self.check(max_bytes, 0)?;
        Ok(out.buf)
    }

    /// Write the facts with their records, then their annotations in the same order.
    pub fn save(&self, w: &mut Writer) -> std::result::Result<(), P::Error> {
        w.u64(self.store.len() as u64);
        let mut err = None;
        self.store.for_each(&mut |f, r| {
            if err.is_none()
                && let Err(e) = self.program.save(w, f, r)
            {
                err = Some(e);
            }
        });
        if let Some(e) = err {
            return Err(e.into());
        }
        if !self.mode.is_none() {
            self.store.for_each(&mut |f, _| match self.ann.get(f) {
                Some(a) => a.save(w),
                None => Ann::zero(self.mode).save(w),
            });
        }
        Ok(())
    }

    /// Read what [`save`](Self::save) wrote.
    pub fn load(&mut self, r: &mut Reader) -> std::result::Result<(), P::Error> {
        self.store.clear()?;
        self.ann.clear();
        self.ann_bytes = 0;
        let n = r.u64()?;
        let mut order = Vec::with_capacity(n.min(1 << 20) as usize);
        for _ in 0..n {
            let (f, rec) = self.program.load(r)?;
            if !self.store.insert(f, rec)? {
                return Err(
                    Error::Corrupt("a fixed point's state holds a fact twice".into()).into(),
                );
            }
            order.push(f);
        }
        if !self.mode.is_none() {
            for f in order {
                let a = Ann::load(r, self.mode)?;
                self.store_ann(f, a);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "fixpoint_unit_tests.rs"]
mod tests;
