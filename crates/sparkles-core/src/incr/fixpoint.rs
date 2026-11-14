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
//! and gives the first frontier, the facts that the changes may have made unprovable. The
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
//! **Memory.** Producers write their instances into a [`Sink`], which reserves the bytes
//! of each growth through the store before it grows and fails when the store refuses or
//! the pass's `max_bytes` would be passed, so one fact with a large fan-out fails while it
//! produces. The operator reserves its own frontiers, sets and change list through the
//! store too, and releases every reservation when the pass ends, whether it succeeded or
//! failed. A caller that keeps the returned changes charges them itself. A failed pass
//! leaves the fixed point unusable until the next [`FixedPoint::build`].

use super::pass::Which;
use super::persist::{Reader, Writer};
use super::prov::{Ann, Atom, Provenance};
use crate::error::{Error, Result};
use rustc_hash::{FxHashMap, FxHashSet};
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

    fn clear(&mut self);

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

    fn contains(&self, f: &F) -> bool {
        self.map.contains_key(f)
    }

    fn record(&self, f: &F) -> Option<R> {
        self.map.get(f).copied()
    }

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

    fn insert(&mut self, f: F, r: R) -> Result<bool> {
        match self.map.entry(f) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(r);
                Ok(true)
            }
            std::collections::hash_map::Entry::Occupied(_) => Ok(false),
        }
    }

    fn set_record(&mut self, f: F, r: R) -> Result<()> {
        self.map.insert(f, r);
        Ok(())
    }

    fn remove(&mut self, f: &F) -> Result<bool> {
        Ok(self.map.remove(f).is_some())
    }

    fn clear(&mut self) {
        self.map.clear();
    }
}

/// Where a producer writes the instances it finds: a buffer whose every growth is
/// reserved through the store, and checked against the pass's memory limit, before it
/// happens.
pub struct Sink<'s, F, R, E> {
    buf: Vec<(F, R)>,
    reserve: Option<&'s dyn Fn(u64) -> std::result::Result<(), E>>,
    /// the limit, and the bytes counted against it before the sink grew
    max: Option<(u64, u64)>,
    /// the bytes this sink reserved
    reserved: u64,
}

impl<'s, F, R, E: From<Error>> Sink<'s, F, R, E> {
    /// A sink that charges nothing, for a program's own use.
    pub fn unbounded() -> Sink<'s, F, R, E> {
        Sink {
            buf: Vec::new(),
            reserve: None,
            max: None,
            reserved: 0,
        }
    }

    fn charged(
        buf: Vec<(F, R)>,
        reserve: &'s dyn Fn(u64) -> std::result::Result<(), E>,
        max: Option<(u64, u64)>,
    ) -> Sink<'s, F, R, E> {
        debug_assert!(buf.is_empty());
        Sink {
            buf,
            reserve: Some(reserve),
            max,
            reserved: 0,
        }
    }

    /// Add one instance, growing the buffer only once the growth is charged.
    pub fn push(&mut self, f: F, r: R) -> std::result::Result<(), E> {
        if self.buf.len() == self.buf.capacity() {
            let cap = self.buf.capacity();
            let want = (cap * 2).max(16);
            let grow = ((want - cap) * size_of::<(F, R)>()) as u64;
            if let Some((limit, before)) = self.max {
                let requested = before + self.reserved + grow;
                if requested > limit {
                    return Err(over(limit, requested).into());
                }
            }
            if let Some(reserve) = self.reserve {
                reserve(grow)?;
                self.reserved += grow;
            }
            self.buf.reserve_exact(want - cap);
        }
        self.buf.push((f, r));
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Take the instances found so far, leaving the buffer's capacity.
    pub fn drain(&mut self) -> std::vec::Drain<'_, (F, R)> {
        self.buf.drain(..)
    }

    /// The buffer and the bytes reserved for it.
    fn into_parts(self) -> (Vec<(F, R)>, u64) {
        (self.buf, self.reserved)
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

    /// An empty store for the program's facts.
    fn new_store(&self) -> Self::Store;

    /// How many facts one call of a batched method takes.
    fn batch_len(&self) -> usize {
        1
    }

    /// The derived premises of instance `r` of fact `f`.
    fn premises(&self, f: &Self::Fact, r: &Self::Record, out: &mut Vec<Self::Fact>);

    /// The instances whose premises are all base facts, at the new snapshot.
    fn axioms(
        &mut self,
        cx: &mut Self::Cx<'_>,
        store: &Self::Store,
        out: &mut Sink<'_, Self::Fact, Self::Record, Self::Error>,
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
        out: &mut Sink<'_, Self::Fact, Self::Record, Self::Error>,
    ) -> std::result::Result<(), Self::Error>;

    /// [`consequences`](Self::consequences) of each of `facts`.
    fn consequences_batch(
        &mut self,
        cx: &mut Self::Cx<'_>,
        store: &Self::Store,
        w: Which,
        facts: &[Self::Fact],
        held: &dyn Fn(&Self::Fact) -> bool,
        out: &mut Sink<'_, Self::Fact, Self::Record, Self::Error>,
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
        out: &mut Vec<Self::Fact>,
    ) -> std::result::Result<(), Self::Error>;

    /// The instances that use a base fact the changes added, with their derived
    /// premises held.
    fn added(
        &mut self,
        cx: &mut Self::Cx<'_>,
        store: &Self::Store,
        held: &dyn Fn(&Self::Fact) -> bool,
        out: &mut Sink<'_, Self::Fact, Self::Record, Self::Error>,
    ) -> std::result::Result<(), Self::Error>;

    /// The held facts whose recorded instance's base facts hold before and after the
    /// changes with another annotation. Only annotated maintenance asks.
    fn moved(
        &mut self,
        _cx: &mut Self::Cx<'_>,
        _store: &Self::Store,
        _recorded: &dyn Fn(&Self::Fact) -> Option<Self::Record>,
        _out: &mut Vec<Self::Fact>,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    /// Exact mode: apply the program's preamble to the store before anything is deleted,
    /// and give the first frontier, the facts that the changes may have made
    /// unprovable.
    fn prepare(
        &mut self,
        _cx: &mut Self::Cx<'_>,
        _store: &mut Self::Store,
        _frontier: &mut Vec<Self::Fact>,
    ) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    /// Exact mode: insert the added facts and everything that follows from them, and
    /// return the facts inserted, or `None` to leave it to the operator, which inserts
    /// the facts [`added`](Self::added) gives and follows
    /// [`consequences`](Self::consequences) from every fact it inserts.
    fn saturate(
        &mut self,
        _cx: &mut Self::Cx<'_>,
        _store: &mut Self::Store,
    ) -> std::result::Result<Option<Vec<Self::Fact>>, Self::Error> {
        Ok(None)
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
/// deleted and derived again. Annotations are not carried.
pub fn consolidate<F: Copy + Eq + Hash>(changes: &[Change<F>]) -> Vec<(F, i64)> {
    let mut sum: FxHashMap<F, i64> = FxHashMap::default();
    let mut order = Vec::new();
    for c in changes {
        let e = sum.entry(c.fact).or_insert_with(|| {
            order.push(c.fact);
            0
        });
        *e += c.weight;
    }
    order
        .into_iter()
        .filter_map(|f| {
            let w = sum[&f];
            (w != 0).then_some((f, w))
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

/// The bytes that a pass reserved through the store, released when the pass ends.
#[derive(Default)]
struct Charges {
    reserved: u64,
}

impl Charges {
    /// Make room in `v` for `add` more items, reserving the growth first.
    fn room<T, E>(
        &mut self,
        reserve: &dyn Fn(u64) -> std::result::Result<(), E>,
        v: &mut Vec<T>,
        add: usize,
    ) -> std::result::Result<(), E> {
        let need = v.len() + add;
        if need <= v.capacity() {
            return Ok(());
        }
        let want = need.max(v.capacity() * 2).max(16);
        let grow = ((want - v.capacity()) * size_of::<T>()) as u64;
        reserve(grow)?;
        self.reserved += grow;
        v.reserve_exact(want - v.len());
        Ok(())
    }

    /// Make room in `s` for `add` more items, reserving the growth first.
    fn room_set<T: Eq + Hash, E>(
        &mut self,
        reserve: &dyn Fn(u64) -> std::result::Result<(), E>,
        s: &mut FxHashSet<T>,
        add: usize,
    ) -> std::result::Result<(), E> {
        let need = s.len() + add;
        if need <= s.capacity() {
            return Ok(());
        }
        let want = need.max(s.capacity() * 2).max(16);
        // a slot holds the item and a control byte, at a load of 7/8
        let slot = size_of::<T>() as u64 + 1;
        let grow = (want - s.capacity()) as u64 * slot * 8 / 7;
        reserve(grow)?;
        self.reserved += grow;
        s.reserve(want - s.len());
        Ok(())
    }

    fn take(&mut self, bytes: u64) {
        self.reserved += bytes;
    }
}

impl<P: Program> FixedPoint<P> {
    pub fn new(program: P) -> FixedPoint<P> {
        let store = program.new_store();
        FixedPoint::with_store(program, store)
    }

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
        self.store.bytes() + self.ann_bytes + self.ann.len() as u64 * 16
    }

    /// What counts against `max_bytes` before a producer's sink grows: the state and the
    /// program's scratch.
    fn used(&self) -> u64 {
        self.bytes() + self.program.scratch_bytes()
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
        let used = self.used() + pending;
        if used > m {
            return Err(over(m, used).into());
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

    /// Run `body` as one pass: release what it reserved and tell the program it ended,
    /// whether it succeeded or not.
    fn pass<T>(
        &mut self,
        cx: &mut P::Cx<'_>,
        body: impl FnOnce(&mut Self, &mut P::Cx<'_>, &mut Charges) -> std::result::Result<T, P::Error>,
    ) -> std::result::Result<T, P::Error> {
        let mut charges = Charges::default();
        let out = body(self, cx, &mut charges);
        self.store.release(charges.reserved);
        let end = self.program.pass_end(cx);
        let out = out?;
        end?;
        Ok(out)
    }

    /// Make room in `v` for `add` more items, reserving the growth through the store.
    fn room<T>(
        &self,
        charges: &mut Charges,
        v: &mut Vec<T>,
        add: usize,
    ) -> std::result::Result<(), P::Error> {
        charges.room(&|b| self.store.reserve(b), v, add)
    }

    /// Make room in `s` for `add` more items, reserving the growth through the store.
    fn room_set<T: Eq + Hash>(
        &self,
        charges: &mut Charges,
        s: &mut FxHashSet<T>,
        add: usize,
    ) -> std::result::Result<(), P::Error> {
        charges.room_set(&|b| self.store.reserve(b), s, add)
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

    /// Run `produce` with a sink that charges the store, reusing `buf`'s capacity, and
    /// return the instances it found in `buf`. The sink's reservation is added to
    /// `charges`.
    fn produce(
        &mut self,
        cx: &mut P::Cx<'_>,
        max: Option<u64>,
        charges: &mut Charges,
        buf: &mut Vec<(P::Fact, P::Record)>,
        produce: impl FnOnce(
            &mut P,
            &mut P::Cx<'_>,
            &P::Store,
            &mut Sink<'_, P::Fact, P::Record, P::Error>,
        ) -> std::result::Result<(), P::Error>,
    ) -> std::result::Result<(), P::Error> {
        // the sink's growth counts against `max` beside the state and the scratch; the
        // operator's other buffers are charged to the store, which a program with a
        // budget of its own charges
        let before = self.used();
        let store = &self.store;
        let reserve = |b: u64| store.reserve(b);
        // the buffer's capacity is already reserved: the sink grows from it
        let mut sink = Sink::charged(std::mem::take(buf), &reserve, max.map(|m| (m, before)));
        let res = produce(&mut self.program, cx, store, &mut sink);
        let (b, reserved) = sink.into_parts();
        *buf = b;
        charges.take(reserved);
        res
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
        mut queue: VecDeque<P::Fact>,
        pending: &mut FxHashSet<P::Fact>,
        proved: &mut Vec<P::Fact>,
        added: &mut Vec<P::Fact>,
    ) -> std::result::Result<(), P::Error> {
        let n = self.program.batch_len().max(1);
        let mut found = Vec::new();
        let mut batch: Vec<P::Fact> = Vec::new();
        while !queue.is_empty() {
            self.program.tick(cx)?;
            batch.clear();
            self.room(charges, &mut batch, n.min(queue.len()))?;
            while batch.len() < n
                && let Some(f) = queue.pop_front()
            {
                batch.push(f);
            }
            {
                let pend = &*pending;
                self.produce(cx, max, charges, &mut found, |p, cx, store, sink| {
                    p.consequences_batch(
                        cx,
                        store,
                        Which::New,
                        &batch,
                        &|x| store.contains(x) && !pend.contains(x),
                        sink,
                    )
                })?;
            }
            for (g, r) in std::mem::take(&mut found) {
                self.take(g, r, max, charges, &mut queue, pending, proved, added)?;
            }
        }
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
        queue: &mut VecDeque<P::Fact>,
        pending: &mut FxHashSet<P::Fact>,
        proved: &mut Vec<P::Fact>,
        added: &mut Vec<P::Fact>,
    ) -> std::result::Result<(), P::Error> {
        if pending.remove(&g) {
            self.store.set_record(g, r)?;
            self.room(charges, proved, 1)?;
            proved.push(g);
            queue.push_back(g);
        } else if self.insert_new(g, r, max)? {
            self.room(charges, added, 1)?;
            added.push(g);
            queue.push_back(g);
        }
        Ok(())
    }

    /// Derive every fact at the new snapshot from nothing, and return them all as
    /// additions.
    pub fn build(
        &mut self,
        cx: &mut P::Cx<'_>,
        max_bytes: Option<u64>,
    ) -> std::result::Result<Vec<Change<P::Fact>>, P::Error> {
        self.refuse_annotations()?;
        self.pass(cx, |s, cx, charges| s.build_pass(cx, max_bytes, charges))
    }

    fn build_pass(
        &mut self,
        cx: &mut P::Cx<'_>,
        max_bytes: Option<u64>,
        charges: &mut Charges,
    ) -> std::result::Result<Vec<Change<P::Fact>>, P::Error> {
        self.store.clear();
        self.ann.clear();
        self.ann_bytes = 0;
        let mut found = Vec::new();
        self.produce(cx, max_bytes, charges, &mut found, |p, cx, store, sink| {
            p.axioms(cx, store, sink)
        })?;
        let mut queue: VecDeque<P::Fact> = VecDeque::new();
        let mut order = Vec::new();
        for (f, r) in std::mem::take(&mut found) {
            if self.insert_new(f, r, max_bytes)? {
                self.room(charges, &mut order, 1)?;
                queue.push_back(f);
                order.push(f);
            }
        }
        let mut pending = FxHashSet::default();
        let mut proved = Vec::new();
        self.chase(
            cx,
            max_bytes,
            charges,
            queue,
            &mut pending,
            &mut proved,
            &mut order,
        )?;
        let dirty: FxHashSet<P::Fact> = order.iter().copied().collect();
        let anns = self.annotate(cx, &dirty, max_bytes)?;
        let mut out = Vec::new();
        self.room(charges, &mut out, order.len())?;
        for f in order {
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
            out.push(Change {
                fact: f,
                weight: 1,
                ann,
            });
        }
        self.check(max_bytes, 0)?;
        Ok(out)
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
    /// annotations made count against `max` with the state.
    fn annotate(
        &mut self,
        cx: &mut P::Cx<'_>,
        dirty: &FxHashSet<P::Fact>,
        max: Option<u64>,
    ) -> std::result::Result<Option<FxHashMap<P::Fact, Ann>>, P::Error> {
        if self.mode.is_none() {
            return Ok(None);
        }
        let mut done: FxHashMap<P::Fact, Ann> = FxHashMap::default();
        let mut pending = 0u64;
        // a depth-first walk over the records: a frame is a fact and whether its
        // premises were scheduled, and `open` holds the facts whose premises are being
        // annotated, so meeting one of them again is a cycle of records
        let mut open: FxHashSet<P::Fact> = FxHashSet::default();
        let mut stack: Vec<(P::Fact, bool)> = Vec::new();
        let mut prem = Vec::new();
        for &f in dirty {
            if done.contains_key(&f) {
                continue;
            }
            stack.push((f, false));
            while let Some(&(top, expanded)) = stack.last() {
                if !expanded && done.contains_key(&top) {
                    stack.pop();
                    continue;
                }
                let r = self.store.record(&top).ok_or_else(|| {
                    Error::Corrupt("a fixed point annotates a fact it does not hold".into())
                })?;
                prem.clear();
                self.program.premises(&top, &r, &mut prem);
                if !expanded {
                    if !open.insert(top) {
                        return Err(
                            Error::Corrupt("a fixed point's records form a cycle".into()).into(),
                        );
                    }
                    stack.last_mut().expect("the frame").1 = true;
                    let mut scheduled: FxHashSet<P::Fact> = FxHashSet::default();
                    for p in &prem {
                        if dirty.contains(p) && !done.contains_key(p) && scheduled.insert(*p) {
                            if open.contains(p) {
                                return Err(Error::Corrupt(
                                    "a fixed point's records form a cycle".into(),
                                )
                                .into());
                            }
                            stack.push((*p, false));
                        }
                    }
                    continue;
                }
                let mut anns: Vec<&Ann> = Vec::with_capacity(prem.len());
                for p in &prem {
                    let a = done.get(p).or_else(|| self.ann.get(p)).ok_or_else(|| {
                        Error::Corrupt(
                            "a fixed point's record names a fact without annotation".into(),
                        )
                    })?;
                    anns.push(a);
                }
                let a = self
                    .program
                    .annotate(cx, &self.store, self.mode, &top, &r, &anns)?;
                pending += a.bytes() + 16;
                done.insert(top, a);
                open.remove(&top);
                stack.pop();
                if done.len().is_multiple_of(4096) {
                    self.check(max, pending)?;
                }
            }
        }
        self.check(max, pending)?;
        Ok(Some(done))
    }

    /// Apply the changes that the context lists, and return the change of the facts.
    pub fn advance(
        &mut self,
        cx: &mut P::Cx<'_>,
        max_bytes: Option<u64>,
    ) -> std::result::Result<Vec<Change<P::Fact>>, P::Error> {
        self.refuse_annotations()?;
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
        let mut frontier: Vec<P::Fact> = Vec::new();
        self.program.prepare(cx, &mut self.store, &mut frontier)?;
        let mut proved: FxHashSet<P::Fact> = FxHashSet::default();
        let mut out: Vec<Change<P::Fact>> = Vec::new();
        let mut verdicts: Vec<Option<P::Record>> = Vec::new();
        let mut found = Vec::new();
        let mut seen: FxHashSet<P::Fact> = FxHashSet::default();
        loop {
            // the frontier, once each, without the facts gone or proved
            seen.clear();
            self.room_set(charges, &mut seen, frontier.len())?;
            frontier.retain(|f| self.store.contains(f) && !proved.contains(f) && seen.insert(*f));
            if frontier.is_empty() {
                break;
            }
            let mut dead: Vec<P::Fact> = Vec::new();
            for batch in frontier.chunks(n) {
                self.program.tick(cx)?;
                verdicts.clear();
                self.room(charges, &mut verdicts, batch.len())?;
                let store = &self.store;
                self.program.prove_batch(
                    cx,
                    store,
                    batch,
                    &|x| store.contains(x),
                    &mut verdicts,
                )?;
                if verdicts.len() != batch.len() {
                    return Err(Error::Corrupt(
                        "a fixed point's program did not decide every fact of a batch".into(),
                    )
                    .into());
                }
                for (f, v) in batch.iter().zip(&verdicts) {
                    if v.is_some() {
                        self.room_set(charges, &mut proved, 1)?;
                        proved.insert(*f);
                    } else {
                        self.room(charges, &mut dead, 1)?;
                        dead.push(*f);
                    }
                }
            }
            if dead.is_empty() {
                break;
            }
            // the heads of the instances that read a dead fact, while the dead facts are
            // still held
            let mut next: Vec<P::Fact> = Vec::new();
            for batch in dead.chunks(n) {
                self.program.tick(cx)?;
                self.produce(cx, max_bytes, charges, &mut found, |p, cx, store, sink| {
                    p.consequences_batch(cx, store, Which::Old, batch, &|x| store.contains(x), sink)
                })?;
                self.room(charges, &mut next, found.len())?;
                next.extend(found.drain(..).map(|(g, _)| g));
            }
            self.room(charges, &mut out, dead.len())?;
            for f in &dead {
                if self.store.remove(f)? {
                    out.push(Change {
                        fact: *f,
                        weight: -1,
                        ann: None,
                    });
                }
            }
            frontier = next;
        }
        drop(seen);
        drop(frontier);
        let inserted = match self.program.saturate(cx, &mut self.store)? {
            Some(v) => v,
            None => {
                self.produce(cx, max_bytes, charges, &mut found, |p, cx, store, sink| {
                    p.added(cx, store, &|x| store.contains(x), sink)
                })?;
                let mut queue = VecDeque::new();
                let mut inserted = Vec::new();
                for (g, r) in std::mem::take(&mut found) {
                    if self.insert_new(g, r, max_bytes)? {
                        self.room(charges, &mut inserted, 1)?;
                        inserted.push(g);
                        queue.push_back(g);
                    }
                }
                let mut pending = FxHashSet::default();
                let mut revived = Vec::new();
                self.chase(
                    cx,
                    max_bytes,
                    charges,
                    queue,
                    &mut pending,
                    &mut revived,
                    &mut inserted,
                )?;
                inserted
            }
        };
        self.room(charges, &mut out, inserted.len())?;
        out.extend(inserted.into_iter().map(|f| Change {
            fact: f,
            weight: 1,
            ann: None,
        }));
        self.check(max_bytes, 0)?;
        Ok(out)
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
        let mut broken = Vec::new();
        {
            let store = &self.store;
            self.program
                .broken(cx, store, &|f| store.record(f), &mut broken)?;
        }
        let mut pending: FxHashSet<P::Fact> = FxHashSet::default();
        let mut candidates: Vec<P::Fact> = Vec::new();
        let mut found = Vec::new();
        let mut batch: Vec<P::Fact> = Vec::new();
        while !broken.is_empty() {
            batch.clear();
            while batch.len() < n
                && let Some(f) = broken.pop()
            {
                if !self.store.contains(&f) || pending.contains(&f) {
                    continue;
                }
                self.room_set(charges, &mut pending, 1)?;
                pending.insert(f);
                self.room(charges, &mut candidates, 1)?;
                candidates.push(f);
                self.room(charges, &mut batch, 1)?;
                batch.push(f);
            }
            if batch.is_empty() {
                continue;
            }
            self.program.tick(cx)?;
            self.produce(cx, max_bytes, charges, &mut found, |p, cx, store, sink| {
                p.consequences_batch(cx, store, Which::Old, &batch, &|x| store.contains(x), sink)
            })?;
            for (g, r) in found.drain(..) {
                if self.store.record(&g) == Some(r) && !pending.contains(&g) {
                    self.room(charges, &mut broken, 1)?;
                    broken.push(g);
                }
            }
        }
        let mut queue: VecDeque<P::Fact> = VecDeque::new();
        let mut proved: Vec<P::Fact> = Vec::new();
        let mut added: Vec<P::Fact> = Vec::new();
        // 2. a backward check of each candidate against the facts that hold
        let mut verdicts: Vec<Option<P::Record>> = Vec::new();
        for chunk in candidates.chunks(n) {
            self.program.tick(cx)?;
            verdicts.clear();
            self.room(charges, &mut verdicts, chunk.len())?;
            {
                let (store, pend) = (&self.store, &pending);
                self.program.prove_batch(
                    cx,
                    store,
                    chunk,
                    &|x| store.contains(x) && !pend.contains(x),
                    &mut verdicts,
                )?;
            }
            if verdicts.len() != chunk.len() {
                return Err(Error::Corrupt(
                    "a fixed point's program did not decide every fact of a batch".into(),
                )
                .into());
            }
            for (&f, v) in chunk.iter().zip(verdicts.drain(..)) {
                if let Some(r) = v {
                    pending.remove(&f);
                    self.store.set_record(f, r)?;
                    self.room(charges, &mut proved, 1)?;
                    proved.push(f);
                    queue.push_back(f);
                }
            }
        }
        // 3. the instances of the added base facts
        {
            let pend = &pending;
            self.produce(cx, max_bytes, charges, &mut found, |p, cx, store, sink| {
                p.added(cx, store, &|x| store.contains(x) && !pend.contains(x), sink)
            })?;
        }
        for (g, r) in std::mem::take(&mut found) {
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
        if self.program.saturate(cx, &mut self.store)?.is_some() {
            return Err(Error::Invalid(
                "a fixed point's program saturates by itself in one-step mode".into(),
            )
            .into());
        }
        self.chase(
            cx,
            max_bytes,
            charges,
            queue,
            &mut pending,
            &mut proved,
            &mut added,
        )?;
        // 5. the candidates without a derivation
        let mut out: Vec<Change<P::Fact>> = Vec::new();
        for f in &candidates {
            if pending.contains(f) {
                self.store.remove(f)?;
                let ann = self.drop_ann(f).map(|a| a.scaled(-1));
                self.room(charges, &mut out, 1)?;
                out.push(Change {
                    fact: *f,
                    weight: -1,
                    ann,
                });
            }
        }
        if self.mode.is_none() {
            self.room(charges, &mut out, added.len())?;
            out.extend(added.into_iter().map(|f| Change {
                fact: f,
                weight: 1,
                ann: None,
            }));
            self.check(max_bytes, 0)?;
            return Ok(out);
        }
        // the annotations of the facts whose records changed, of the facts whose
        // recorded base facts moved, and of every fact whose record depends on them
        let mut dirty: FxHashSet<P::Fact> = proved.iter().chain(added.iter()).copied().collect();
        let mut moved = Vec::new();
        {
            let store = &self.store;
            self.program
                .moved(cx, store, &|f| store.record(f), &mut moved)?;
        }
        let mut stack: Vec<P::Fact> = moved
            .into_iter()
            .filter(|f| self.store.contains(f))
            .chain(proved.iter().copied())
            .collect();
        let mut seen: FxHashSet<P::Fact> = FxHashSet::default();
        while let Some(f) = stack.pop() {
            if !seen.insert(f) {
                continue;
            }
            dirty.insert(f);
            self.program.tick(cx)?;
            self.produce(cx, max_bytes, charges, &mut found, |p, cx, store, sink| {
                p.consequences(cx, store, Which::New, &f, &|x| store.contains(x), sink)
            })?;
            for (g, r) in found.drain(..) {
                if self.store.record(&g) == Some(r) {
                    stack.push(g);
                }
            }
        }
        let anns = self.annotate(cx, &dirty, max_bytes)?.unwrap_or_default();
        let fresh: FxHashSet<P::Fact> = added.iter().copied().collect();
        for f in dirty {
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
            out.push(Change {
                fact: f,
                weight,
                ann: Some(d),
            });
        }
        self.check(max_bytes, 0)?;
        Ok(out)
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
        self.store.clear();
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
