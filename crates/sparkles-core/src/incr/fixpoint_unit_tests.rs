//! The fixed-point operator's accounting, error type and pass hooks, with a program of
//! numbers: the axioms are `0..axioms`, and fact `n` derives `n + 1` below `chain`.

use super::*;
use crate::incr::persist::{Reader, Writer};
use crate::incr::prov::FactKey;
use crate::incr::terms::Terms;
use crate::store::{Store, StoreOptions};
use std::cell::Cell;

/// The program's own error, which the operator's errors convert into.
#[derive(Debug)]
enum Oops {
    Core(Error),
    Mine,
}

impl From<Error> for Oops {
    fn from(e: Error) -> Oops {
        Oops::Core(e)
    }
}

/// A [`HashStore`] that counts what is reserved through it and refuses past `cap`.
struct Tally {
    inner: HashStore<u32, u32>,
    reserved: Cell<u64>,
    peak: Cell<u64>,
    pass_reserved: Cell<u64>,
    pass_min: Cell<u64>,
    cap: Option<u64>,
}

impl Tally {
    fn new(cap: Option<u64>) -> Tally {
        Tally {
            inner: HashStore::new(8),
            reserved: Cell::new(0),
            peak: Cell::new(0),
            pass_reserved: Cell::new(0),
            pass_min: Cell::new(0),
            cap,
        }
    }
}

impl FactStore<u32, u32> for Tally {
    type Error = Oops;

    fn contains(&self, f: &u32) -> bool {
        self.inner.contains(f)
    }

    fn record(&self, f: &u32) -> Option<u32> {
        self.inner.record(f)
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn for_each(&self, visit: &mut dyn FnMut(&u32, &u32)) {
        self.inner.for_each(visit)
    }

    fn bytes(&self) -> u64 {
        self.inner.bytes()
    }

    fn insert(&mut self, f: u32, r: u32) -> std::result::Result<bool, Oops> {
        Ok(self.inner.insert(f, r)?)
    }

    fn set_record(&mut self, f: u32, r: u32) -> std::result::Result<(), Oops> {
        Ok(self.inner.set_record(f, r)?)
    }

    fn remove(&mut self, f: &u32) -> std::result::Result<bool, Oops> {
        Ok(self.inner.remove(f)?)
    }

    fn clear(&mut self) -> std::result::Result<(), Oops> {
        self.inner.clear()?;
        Ok(())
    }

    fn reserve(&self, bytes: u64) -> std::result::Result<(), Oops> {
        let now = self.reserved.get() + bytes;
        if self.cap.is_some_and(|c| now > c) {
            return Err(over(self.cap.unwrap_or(0), now).into());
        }
        self.reserved.set(now);
        self.peak.set(self.peak.get().max(now));
        Ok(())
    }

    fn release(&self, bytes: u64) {
        // an underflow is a release of more than was reserved
        self.reserved.set(self.reserved.get() - bytes);
    }

    fn reserve_pass(&self, bytes: u64) -> std::result::Result<(), Oops> {
        let now = self.pass_reserved.get() + bytes;
        self.pass_reserved.set(now);
        Ok(())
    }

    fn release_pass(&self, bytes: u64) {
        let now = self.pass_reserved.get() - bytes;
        self.pass_reserved.set(now);
        self.pass_min.set(self.pass_min.get().min(now));
    }
}

/// The record of an axiom.
const AXIOM: u32 = u32::MAX;

/// The program of numbers, in exact mode when `X`.
struct Count<const X: bool> {
    axioms: u32,
    chain: u32,
    cap: Option<u64>,
    /// fail with [`Oops::Mine`] at this tick
    fail_at: Option<u64>,
    ticks: u64,
    /// fail in `pass_end`
    end_fails: bool,
    ends: usize,
    /// claim to saturate by itself
    own: bool,
    /// list each record's premise this many times
    fan_in: u32,
    /// fail in `annotate` at this call
    annotate_fails_at: Option<u64>,
    annotated: u64,
    /// the facts below this are broken at every pass
    broken: u32,
    /// the scratch the program holds, which grows by `scratch_step` at each call of
    /// `consequences`
    scratch: u64,
    scratch_step: u64,
}

impl<const X: bool> Count<X> {
    fn new(axioms: u32, chain: u32) -> Count<X> {
        Count {
            axioms,
            chain,
            cap: None,
            fail_at: None,
            ticks: 0,
            end_fails: false,
            ends: 0,
            own: false,
            fan_in: 1,
            annotate_fails_at: None,
            annotated: 0,
            broken: 0,
            scratch: 0,
            scratch_step: 0,
        }
    }
}

impl<const X: bool> NewStore for Count<X> {
    fn new_store(&self) -> Tally {
        Tally::new(self.cap)
    }
}

impl<const X: bool> Program for Count<X> {
    type Fact = u32;
    type Record = u32;
    type Cx<'c> = ();
    type Error = Oops;
    type Store = Tally;

    const EXACT: bool = X;

    fn premises(
        &self,
        _f: &u32,
        r: &u32,
        out: &mut Sink<'_, u32, Oops>,
    ) -> std::result::Result<(), Oops> {
        if *r != AXIOM {
            for _ in 0..self.fan_in {
                out.push(*r)?;
            }
        }
        Ok(())
    }

    fn axioms(
        &mut self,
        _cx: &mut (),
        _store: &Tally,
        out: &mut Sink<'_, (u32, u32), Oops>,
    ) -> std::result::Result<(), Oops> {
        for n in 0..self.axioms {
            out.push((n, AXIOM))?;
        }
        Ok(())
    }

    fn consequences(
        &mut self,
        _cx: &mut (),
        _store: &Tally,
        _w: Which,
        f: &u32,
        _held: &dyn Fn(&u32) -> bool,
        out: &mut Sink<'_, (u32, u32), Oops>,
    ) -> std::result::Result<(), Oops> {
        self.scratch += self.scratch_step;
        if f + 1 < self.chain {
            out.push((f + 1, *f))?;
        }
        Ok(())
    }

    fn prove(
        &mut self,
        _cx: &mut (),
        _store: &Tally,
        f: &u32,
        held: &dyn Fn(&u32) -> bool,
    ) -> std::result::Result<Option<u32>, Oops> {
        if *f < self.axioms {
            return Ok(Some(AXIOM));
        }
        Ok((*f > 0 && held(&(f - 1))).then(|| f - 1))
    }

    fn broken(
        &mut self,
        _cx: &mut (),
        _store: &Tally,
        _recorded: &dyn Fn(&u32) -> Option<u32>,
        out: &mut Sink<'_, u32, Oops>,
    ) -> std::result::Result<(), Oops> {
        for n in 0..self.broken {
            out.push(n)?;
        }
        Ok(())
    }

    fn added(
        &mut self,
        _cx: &mut (),
        _store: &Tally,
        _held: &dyn Fn(&u32) -> bool,
        _out: &mut Sink<'_, (u32, u32), Oops>,
    ) -> std::result::Result<(), Oops> {
        Ok(())
    }

    fn saturate(&mut self, _cx: &mut (), _store: &mut Tally) -> std::result::Result<bool, Oops> {
        Ok(self.own)
    }

    fn pass_end(&mut self, _cx: &mut ()) -> std::result::Result<(), Oops> {
        self.ends += 1;
        if self.end_fails {
            return Err(Oops::Mine);
        }
        Ok(())
    }

    fn annotate(
        &mut self,
        _cx: &mut (),
        _store: &Tally,
        mode: Provenance,
        f: &u32,
        r: &u32,
        premises: &[&Ann],
    ) -> std::result::Result<Ann, Oops> {
        self.annotated += 1;
        if self.annotate_fails_at == Some(self.annotated) {
            return Err(Oops::Mine);
        }
        let mut atoms: Vec<Atom> = premises.iter().flat_map(|a| atoms_of(a)).collect();
        if *r == AXIOM {
            atoms.push(Atom::Fact(FactKey::of_keys(
                &f.to_le_bytes(),
                b"p",
                b"o",
                None,
            )));
        }
        Ok(Ann::derivation(mode, atoms))
    }

    fn tick(&mut self, _cx: &mut ()) -> std::result::Result<(), Oops> {
        self.ticks += 1;
        if self.fail_at == Some(self.ticks) {
            return Err(Oops::Mine);
        }
        Ok(())
    }

    fn save(&self, w: &mut Writer, f: &u32, r: &u32) -> Result<()> {
        w.u64(u64::from(*f));
        w.u64(u64::from(*r));
        Ok(())
    }

    fn scratch_bytes(&self) -> u64 {
        self.scratch
    }

    fn load(&self, r: &mut Reader) -> Result<(u32, u32)> {
        let f = r.u64()? as u32;
        let rec = r.u64()? as u32;
        Ok((f, rec))
    }
}

/// The program's own error type comes back from a pass, which ends once and releases
/// what it reserved.
#[test]
fn a_programs_error_keeps_its_type() {
    let mut p = Count::<false>::new(1, 100);
    p.fail_at = Some(3);
    let mut fp = FixedPoint::new(p);
    assert!(matches!(fp.build(&mut (), None), Err(Oops::Mine)));
    assert_eq!(fp.program.ends, 1);
    assert_eq!(fp.store().reserved.get(), 0);
    // and from a pass
    let mut fp = FixedPoint::new(Count::<false>::new(4, 10));
    fp.build(&mut (), None).expect("build");
    fp.program.broken = 3;
    fp.program.fail_at = Some(fp.program.ticks + 1);
    assert!(matches!(fp.advance(&mut (), None), Err(Oops::Mine)));
    assert_eq!(fp.program.ends, 2);
    assert_eq!(fp.store().reserved.get(), 0);
}

/// `pass_end` runs once for every build and pass: on success, on an evaluation error,
/// on the refusal of annotations in exact mode, and when it fails itself.
#[test]
fn every_pass_ends_once() {
    let mut fp = FixedPoint::new(Count::<false>::new(4, 10));
    fp.build(&mut (), None).expect("build");
    assert_eq!(fp.program.ends, 1);
    fp.advance(&mut (), None).expect("advance");
    assert_eq!(fp.program.ends, 2);
    fp.program.end_fails = true;
    assert!(matches!(fp.advance(&mut (), None), Err(Oops::Mine)));
    assert_eq!(fp.program.ends, 3);

    let mut exact = FixedPoint::new(Count::<true>::new(4, 10));
    exact.build(&mut (), None).expect("build");
    exact.set_provenance(Provenance::why());
    assert!(matches!(exact.build(&mut (), None), Err(Oops::Core(_))));
    assert!(matches!(exact.advance(&mut (), None), Err(Oops::Core(_))));
    assert_eq!(exact.program.ends, 3);
    assert_eq!(exact.store().reserved.get(), 0);
}

/// A one-step program may not saturate by itself, since that would leave the pending
/// candidates unproved.
#[test]
fn a_one_step_program_may_not_saturate_by_itself() {
    let mut fp = FixedPoint::new(Count::<false>::new(4, 10));
    fp.build(&mut (), None).expect("build");
    fp.program.own = true;
    assert!(matches!(fp.advance(&mut (), None), Err(Oops::Core(_))));
    assert_eq!(fp.store().reserved.get(), 0);
}

/// The operator's queues are reserved with its other buffers: a build of many axioms
/// holds the sink of axioms, the order of insertion and the queue at once.
#[test]
fn the_queues_are_reserved() {
    let n = 5000;
    let mut fp = FixedPoint::new(Count::<false>::new(n, 0));
    fp.build(&mut (), None).expect("build");
    let slots = 8192u64;
    let want = slots * (size_of::<(u32, u32)>() + size_of::<u32>() * 2) as u64;
    assert!(
        fp.store().peak.get() >= want,
        "peak {} under {want}",
        fp.store().peak.get()
    );
    assert_eq!(fp.store().reserved.get(), 0);
}

/// A buffer reused batch after batch is charged for what it holds, not for every batch
/// it held: a chain of many one-fact batches fits a budget far below their sum.
#[test]
fn reused_buffers_are_charged_once() {
    let n = 20_000;
    let mut p = Count::<false>::new(1, n);
    p.cap = Some(1 << 20);
    let mut fp = FixedPoint::new(p);
    let changes = fp.build(&mut (), None).expect("the chain fits");
    assert_eq!(changes.len(), n as usize);
    assert!(fp.store().peak.get() <= 1 << 20);
    assert_eq!(fp.store().reserved.get(), 0);
}

/// An annotated fixed point over a hash store survives its checkpoint, with its
/// annotations in the order of its facts, and advances after it.
#[test]
fn an_annotated_hash_store_round_trips() {
    let s = Store::in_memory(StoreOptions::default());
    let snap = s.snapshot();
    let terms = Terms::default();
    let mut fp = FixedPoint::new(Count::<false>::new(3, 40));
    fp.set_provenance(Provenance::why());
    fp.build(&mut (), None).expect("build");
    let mut w = Writer::new(&snap, &terms);
    fp.save(&mut w).expect("save");
    let bytes = w.finish();
    let mut back = FixedPoint::new(Count::<false>::new(3, 40));
    back.set_provenance(Provenance::why());
    let mut terms = Terms::default();
    let mut r = Reader::new(&bytes, &snap, &mut terms).expect("reader");
    back.load(&mut r).expect("load");
    let mut a = fp.facts();
    let mut b = back.facts();
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(a, b);
    for (f, _) in &a {
        assert_eq!(back.annotation(f), fp.annotation(f), "fact {f}");
    }
    // the restored state advances: a pass without changes changes nothing
    let changes = back.advance(&mut (), None).expect("advance");
    assert!(changes.is_empty(), "{changes:?}");
    assert_eq!(back.len(), 40);
}

/// The reservation of a growing table or vector is what it allocates, and covers both
/// allocations while it moves.
#[test]
fn growth_reserves_the_allocation() {
    let reserved = Cell::new(0u64);
    let peak = Cell::new(0u64);
    let reserve = |b: u64| -> std::result::Result<(), Error> {
        reserved.set(reserved.get() + b);
        peak.set(peak.get().max(reserved.get()));
        Ok(())
    };
    let release = |b: u64| reserved.set(reserved.get() - b);
    let acct = Acct {
        reserve: &reserve,
        release: &release,
    };
    let mut set: FxHashSet<[u64; 3]> = FxHashSet::default();
    let mut held = 0;
    for i in 0..20_000u64 {
        let before = held;
        grow_charged(Some(acct), None, &mut set, &mut held, 1).expect("grow");
        set.insert([i; 3]);
        assert_eq!(held, table_bytes(set.capacity(), 24) as u64, "at {i}");
        assert_eq!(reserved.get(), held, "at {i}");
        if held != before {
            assert!(peak.get() >= before + held, "at {i}: the move was covered");
        }
    }
    let mut v: VecDeque<u32> = VecDeque::new();
    let mut vheld = 0;
    for i in 0..5000 {
        grow_charged(Some(acct), None, &mut v, &mut vheld, 1).expect("grow");
        v.push_back(i);
        assert_eq!(vheld, v.capacity() as u64 * 4, "at {i}");
    }
}

/// Byte counts near the top of `u64` refuse without overflowing.
#[test]
fn a_limit_near_the_top_refuses() {
    let mut v: Vec<u64> = Vec::new();
    let mut held = 0;
    let r: std::result::Result<(), Error> = grow_charged(
        None,
        Some((u64::MAX - 1, u64::MAX - 8)),
        &mut v,
        &mut held,
        1,
    );
    assert!(matches!(r, Err(Error::BudgetExceeded(_))));
    let w = consolidate(&[
        Change {
            fact: 1u32,
            weight: i64::MAX,
            ann: None,
        },
        Change {
            fact: 1,
            weight: i64::MAX,
            ann: None,
        },
    ]);
    assert_eq!(w, vec![(1, i64::MAX)]);
}

/// A budget that the order and the queue pass refuses the build, and its reservations
/// return.
#[test]
fn buffers_past_the_budget_are_refused() {
    let mut p = Count::<false>::new(5000, 0);
    // the sink of axioms fits, and the order and the queue beside it do not
    p.cap = Some(120_000);
    let mut fp = FixedPoint::new(p);
    assert!(matches!(
        fp.build(&mut (), None),
        Err(Oops::Core(Error::BudgetExceeded(_)))
    ));
    assert_eq!(fp.program.ticks, 0, "refused before the chase");
    assert_eq!(fp.store().reserved.get(), 0);
    assert_eq!(fp.program.ends, 1);
}

/// The output of `broken` is charged as it is produced, and a refusal there returns
/// every reservation.
#[test]
fn broken_facts_past_the_budget_are_refused() {
    let mut fp = FixedPoint::new(Count::<false>::new(5000, 0));
    fp.build(&mut (), None).expect("build");
    fp.store_mut().cap = Some(10_000);
    fp.program.broken = 5000;
    let ticks = fp.program.ticks;
    assert!(matches!(
        fp.advance(&mut (), None),
        Err(Oops::Core(Error::BudgetExceeded(_)))
    ));
    assert_eq!(
        fp.program.ticks, ticks,
        "refused before any candidate was checked"
    );
    assert_eq!(fp.store().reserved.get(), 0);
}

/// Scratch that grows while a producer fills a sink's retained capacity counts with
/// that capacity: a build whose state and scratch fit `max_bytes` at its end still
/// fails when its last sink did not fit beside them.
#[test]
fn scratch_grown_during_production_counts_with_the_sink() {
    let mut p = Count::<false>::new(1, 200);
    p.scratch_step = 1000;
    let mut fp = FixedPoint::new(p);
    fp.build(&mut (), None).expect("build");
    let end = fp.bytes() + fp.program.scratch;
    let mut p = Count::<false>::new(1, 200);
    p.scratch_step = 1000;
    let mut fp = FixedPoint::new(p);
    assert!(matches!(
        fp.build(&mut (), Some(end + 64)),
        Err(Oops::Core(Error::BudgetExceeded(_)))
    ));
    assert_eq!(fp.store().reserved.get(), 0);
    // state and scratch past `u64::MAX` refuse the top limit, with an empty sink too
    let mut p = Count::<false>::new(1, 0);
    p.scratch_step = u64::MAX;
    let mut fp = FixedPoint::new(p);
    assert!(matches!(
        fp.build(&mut (), Some(u64::MAX)),
        Err(Oops::Core(Error::BudgetExceeded(_)))
    ));
}

/// The annotation walk charges the premises a program lists: a record with many
/// premises is refused while they are listed, and an error of `annotate` returns every
/// reservation too.
#[test]
fn the_annotation_walk_is_charged() {
    let mut p = Count::<false>::new(1, 3);
    p.fan_in = 100_000;
    p.cap = Some(256 << 10);
    let mut fp = FixedPoint::new(p);
    fp.set_provenance(Provenance::why());
    assert!(matches!(
        fp.build(&mut (), None),
        Err(Oops::Core(Error::BudgetExceeded(_)))
    ));
    assert!(
        fp.program.annotated <= 1,
        "refused at the first record with premises"
    );
    assert_eq!(fp.store().reserved.get(), 0);

    let mut p = Count::<false>::new(1, 30);
    p.annotate_fails_at = Some(10);
    let mut fp = FixedPoint::new(p);
    fp.set_provenance(Provenance::why());
    assert!(matches!(fp.build(&mut (), None), Err(Oops::Mine)));
    assert_eq!(fp.store().reserved.get(), 0);
    assert_eq!(fp.program.ends, 1);
}

/// A limit of `u64::MAX` refuses a count that would overflow it.
#[test]
fn the_top_limit_refuses_an_overflow() {
    let mut v: Vec<u64> = Vec::new();
    let mut held = 0;
    let r: std::result::Result<(), Error> =
        grow_charged(None, Some((u64::MAX, u64::MAX - 8)), &mut v, &mut held, 1);
    assert!(matches!(r, Err(Error::BudgetExceeded(_))));
    assert_eq!(v.capacity(), 0);
}

/// Deleting previously retained lineage annotations during `advance_one_step` must not
/// release credit from shared `pass_bytes` that covers another live allocation (such as
/// a path edge memo).
#[test]
fn annotation_deletion_does_not_release_unrelated_pass_bytes() {
    let p = Count::<false>::new(1, 5);
    let mut fp = FixedPoint::new(p);
    fp.set_provenance(Provenance::Lineage);
    let built = fp.build(&mut (), None).unwrap();
    assert_eq!(built.len(), 5);
    assert_eq!(fp.store().reserved.get(), 0);

    // Simulate a new pass after commit: prior annotations live in `state_bytes`, while
    // an edge memo holds 4096 bytes in shared `pass_bytes`.
    let memo_bytes = 4096u64;
    fp.store().pass_reserved.set(memo_bytes);
    fp.store().pass_min.set(memo_bytes);
    fp.program.broken = 5;
    fp.program.axioms = 0;
    let delta = fp.advance(&mut (), None).unwrap();
    assert_eq!(delta.len(), 5);
    assert_eq!(
        fp.store().pass_min.get(),
        memo_bytes,
        "deleting retained annotations must never subtract from another allocation's pass_bytes"
    );
    assert_eq!(fp.store().pass_reserved.get(), memo_bytes);
    assert_eq!(fp.store().reserved.get(), 0);
}
