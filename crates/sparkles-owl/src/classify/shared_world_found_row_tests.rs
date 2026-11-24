//! Fail-first tests of the shared conditional-world driver in Batch 55.

use super::*;
use parking_lot::Mutex;
use std::cell::RefCell;
use std::fmt::Write;
use std::panic::{AssertUnwindSafe, catch_unwind, panic_any};
use std::sync::{Arc, mpsc};
use std::thread::{self, ThreadId};

const PREFIX: &str = "urn:shared-world-row:";
const INDEPENDENT: isize = 37;
const CROSS_REACHERS: usize = 1024;
const SECOND_CHUNK: usize = 513;
const LOCAL: index::Halt = index::Halt::Memory(usize::MAX);
const COLLECTED_STOP_USED: usize = usize::MAX - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Late {
    Cancel,
    Refuse,
    ChargePanic,
    PollPanic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    Local { offset: usize, after: usize },
    WorkerPanic,
    Late(Late),
    Sibling,
    BeforePost,
    ObservePost,
    BeforeFirstInsert,
    BeforeSecondInsert,
    AfterFirstInsert,
    StopAfterCollection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InjectedPanic(Fault);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Marks {
    live: isize,
    paid: usize,
    peak: usize,
}

impl Marks {
    fn read(budget: &WorldBudget<'_>) -> Self {
        Self {
            live: budget.held.load(Ordering::Relaxed),
            paid: budget.paid.load(Ordering::Relaxed),
            peak: budget.peak.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Report {
    id: Ex,
    bytes: usize,
    before: Marks,
}

#[derive(Debug)]
struct Exit {
    before_release: Marks,
    after_release: Marks,
    stop: Result<(), Error>,
    checker_bytes: Option<usize>,
}

#[derive(Debug)]
struct CheckerTrace {
    before_worlds: usize,
    at_join: Option<usize>,
    pooled_live: Option<usize>,
}

#[derive(Debug, Default)]
struct Trace {
    expected: BTreeMap<Ex, Vec<Ex>>,
    base: usize,
    independent: isize,
    workers: BTreeMap<usize, (ThreadId, Vec<Ex>)>,
    reports: BTreeMap<usize, Vec<Report>>,
    admitted: BTreeMap<usize, Vec<Ex>>,
    exited: Vec<usize>,
    collected: Option<(Result<usize, index::Halt>, Marks)>,
    exit: Option<Exit>,
    checker: Option<CheckerTrace>,
    armed: bool,
    row_observations: usize,
    requests: Vec<usize>,
    polls: usize,
    cap: Option<(usize, usize)>,
}

struct Signal(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>);

impl Signal {
    fn new() -> Self {
        let (send, receive) = mpsc::channel();
        Self(send, Mutex::new(receive))
    }

    fn wait(&self) {
        self.1.lock().recv_timeout(admission_test::WAIT).unwrap();
    }
}

enum Fixture {
    Nominal(usize),
    Datatype,
}

pub(super) struct Probe {
    fault: Fault,
    fixture: Fixture,
    trace: Mutex<Trace>,
    first_admitted: Signal,
    sibling_ready: Signal,
    first_exited: Signal,
}

thread_local! {
    static ACTIVE: RefCell<Option<Arc<Probe>>> = const { RefCell::new(None) };
}

pub(super) fn active() -> Option<Arc<Probe>> {
    ACTIVE.with(|slot| slot.borrow().clone())
}

fn with_probe<T>(probe: &Arc<Probe>, run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ACTIVE.with(|slot| slot.borrow_mut().take());
        }
    }
    ACTIVE.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(probe.clone());
    });
    let _reset = Reset;
    run()
}

pub(super) struct Phase<'p, 'b, 'a> {
    probe: &'p Probe,
    budget: &'b WorldBudget<'a>,
    checker: Option<&'b dt_sat::DtIx>,
}

impl Drop for Phase<'_, '_, '_> {
    fn drop(&mut self) {
        // On error or unwind the result owners have already gone out of scope.
        let mut trace = self.probe.trace.lock();
        let before_release = Marks::read(self.budget);
        let stop = self.budget.stopped();
        // Retire the fixture's independent debit without posting a poll or charge.
        self.budget
            .held
            .fetch_sub(trace.independent, Ordering::Relaxed);
        trace.exit = Some(Exit {
            before_release,
            after_release: Marks::read(self.budget),
            stop,
            checker_bytes: self.checker.map(dt_sat::DtIx::bytes),
        });
        if self.checker.is_some() {
            trace.armed = false;
        }
    }
}

pub(super) struct Worker<'p> {
    probe: &'p Probe,
    offset: usize,
}

impl Drop for Worker<'_> {
    fn drop(&mut self) {
        self.probe.trace.lock().exited.push(self.offset);
        if self.probe.fault == Fault::Sibling && self.offset == 0 {
            // This guard precedes the result buffer. On the injected error the
            // buffer and its payloads are gone before the sibling can post.
            let _ = self.probe.first_exited.0.send(());
        }
    }
}

impl Probe {
    fn new(fault: Fault, reachers: usize) -> Arc<Self> {
        Self::with_fixture(fault, Fixture::Nominal(reachers))
    }

    fn with_fixture(fault: Fault, fixture: Fixture) -> Arc<Self> {
        Arc::new(Self {
            fault,
            fixture,
            trace: Mutex::default(),
            first_admitted: Signal::new(),
            sibling_ready: Signal::new(),
            first_exited: Signal::new(),
        })
    }

    pub(super) fn phase<'p, 'b, 'a>(
        &'p self,
        budget: &'b WorldBudget<'a>,
        interner: &Interner,
        classes: &[Ex],
        dt: Option<&'b dt_sat::DtIx>,
    ) -> Phase<'p, 'b, 'a> {
        let (expected, checker) = match self.fixture {
            Fixture::Nominal(reachers) => (Self::nominal_rows(interner, classes, reachers), None),
            Fixture::Datatype => (
                BTreeMap::new(),
                Some(dt.expect("the world's datatype checker")),
            ),
        };
        let checker_trace = checker.map(|dt| CheckerTrace {
            before_worlds: dt.bytes(),
            at_join: None,
            pooled_live: None,
        });
        assert!(budget.tell(INDEPENDENT));
        let mut trace = self.trace.lock();
        trace.expected = expected;
        trace.base = budget.base;
        trace.independent = INDEPENDENT;
        trace.checker = checker_trace;
        Phase {
            probe: self,
            budget,
            checker,
        }
    }

    fn nominal_rows(interner: &Interner, classes: &[Ex], reachers: usize) -> BTreeMap<Ex, Vec<Ex>> {
        let witness = interner.class_ex(&class("Witness")).unwrap();
        let tag = interner.class_ex(&class("Tag")).unwrap();
        let nominal = interner.nominals().next().unwrap();
        let role = interner
            .role_id(&index::RoleName::Object(ObjectProperty::new(format!(
                "{PREFIX}r"
            ))))
            .unwrap();
        let some = interner.get(&Node::Exist(role, witness)).unwrap();
        let mut expected = BTreeMap::from([(witness, vec![TOP, witness, tag, nominal])]);
        for n in 0..reachers {
            let id = interner.class_ex(&class(&format!("C{n:04}"))).unwrap();
            assert!(expected.insert(id, vec![TOP, id, some]).is_none());
        }
        for subs in expected.values_mut() {
            subs.sort_unstable();
        }
        assert_eq!(classes.first(), Some(&witness));
        assert!(classes.iter().copied().eq(expected.keys().copied()));
        expected
    }

    pub(super) fn worker(&self, offset: usize, chunk: &[Ex]) -> Worker<'_> {
        self.trace
            .lock()
            .workers
            .insert(offset, (thread::current().id(), chunk.to_vec()));
        Worker {
            probe: self,
            offset,
        }
    }

    pub(super) fn before_world(&self, offset: usize, buffered: usize) -> Result<(), index::Halt> {
        match self.fault {
            Fault::Local {
                offset: fail,
                after,
            } if (offset, buffered) == (fail, after) => {
                return Err(LOCAL);
            }
            Fault::WorkerPanic if offset == SECOND_CHUNK => panic_any(InjectedPanic(self.fault)),
            Fault::Sibling if (offset, buffered) == (SECOND_CHUNK, 0) => {
                self.first_admitted.wait();
            }
            Fault::Sibling if (offset, buffered) == (0, 1) => {
                self.sibling_ready.wait();
                return Err(LOCAL);
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn before_report(
        &self,
        budget: &WorldBudget<'_>,
        offset: usize,
        id: Ex,
        found: &world::Found,
        bytes: usize,
    ) {
        let position = {
            let mut trace = self.trace.lock();
            match self.fixture {
                Fixture::Nominal(_) => {
                    let mut subs = found.subs.clone();
                    subs.sort_unstable();
                    assert!(!found.bottom);
                    assert_eq!(subs, trace.expected[&id]);
                    assert_eq!(bytes, if subs.len() == 4 { 64 } else { 60 });
                }
                Fixture::Datatype => {
                    assert_eq!(bytes, 48 + 4 * found.subs.len());
                }
            }
            trace.reports.entry(offset).or_default().len()
        };
        if (matches!(self.fault, Fault::Late(_)) && position == 1)
            || (self.fault == Fault::Sibling && (offset, position) == (SECOND_CHUNK, 0))
        {
            // The world's real scratch has dropped. Fill the already-paid headroom
            // with an independent live debit so the next row needs exact growth.
            let before = Marks::read(budget);
            let padding = before
                .paid
                .checked_sub(budget.base + before.live as usize)
                .unwrap();
            if padding != 0 {
                assert!(budget.tell(padding as isize));
            }
            {
                let mut trace = self.trace.lock();
                trace.independent += padding as isize;
                trace.armed = true;
                if self.fault == Fault::Sibling {
                    trace.cap = Some((
                        budget.base + trace.independent as usize + bytes,
                        before.paid,
                    ));
                }
            }
            if self.fault == Fault::Sibling {
                self.sibling_ready.0.send(()).unwrap();
                self.first_exited.wait();
                assert_eq!(budget.stopped(), Ok(()));
            }
        }
        self.trace
            .lock()
            .reports
            .entry(offset)
            .or_default()
            .push(Report {
                id,
                bytes,
                before: Marks::read(budget),
            });
    }

    pub(super) fn before_post(&self, staged: usize) {
        if staged == 2 {
            match self.fault {
                Fault::BeforePost => self.panic_armed(),
                Fault::ObservePost => self.trace.lock().armed = true,
                _ => {}
            }
        }
    }

    fn observation(&self, point: admission_test::Point) {
        let mut trace = self.trace.lock();
        if point == admission_test::Point::Observed && trace.armed {
            trace.row_observations += 1;
            if self.fault == Fault::ObservePost {
                panic_any(InjectedPanic(self.fault));
            }
        }
    }

    pub(super) fn admitted(&self, offset: usize, id: Ex) {
        let mut trace = self.trace.lock();
        let rows = trace.admitted.entry(offset).or_default();
        rows.push(id);
        if self.fault == Fault::Sibling && rows.len() == 1 {
            if offset == 0 {
                self.first_admitted.0.send(()).unwrap();
            } else {
                // The cap is a caller refusal for this one row. Let B finish
                // its real remaining worlds after observing its first admission.
                trace.armed = false;
            }
        }
    }

    pub(super) fn collected(
        &self,
        budget: &WorldBudget<'_>,
        result: Result<usize, index::Halt>,
        dt: Option<&dt_sat::DtIx>,
    ) {
        let mut trace = self.trace.lock();
        trace.collected = Some((result, Marks::read(budget)));
        if let Some(checker) = &mut trace.checker {
            checker.at_join = Some(dt.unwrap().bytes());
            // Count callbacks from collection until the row owners have gone.
            trace.armed = true;
        }
        if self.fault == Fault::StopAfterCollection && result.is_ok() {
            trace.armed = true;
            budget.stop_for_memory(COLLECTED_STOP_USED);
        }
    }

    pub(super) fn pooled_live(&self, live: usize) {
        if let Some(checker) = &mut self.trace.lock().checker {
            checker.pooled_live = Some(live);
        }
    }

    pub(super) fn before_insert(&self, committed: usize) {
        if matches!(
            (committed, self.fault),
            (0, Fault::BeforeFirstInsert) | (1, Fault::BeforeSecondInsert)
        ) {
            self.panic_armed();
        }
    }

    pub(super) fn after_insert(&self, committed: usize) {
        if committed == 1 && self.fault == Fault::AfterFirstInsert {
            self.panic_armed();
        }
    }

    fn panic_armed(&self) -> ! {
        self.trace.lock().armed = true;
        panic_any(InjectedPanic(self.fault));
    }

    fn charge(&self, bytes: usize) -> bool {
        let mut trace = self.trace.lock();
        if !trace.armed {
            return true;
        }
        trace.requests.push(bytes);
        match self.fault {
            Fault::Late(Late::Refuse) => false,
            Fault::Late(Late::ChargePanic) => panic_any(InjectedPanic(self.fault)),
            Fault::Sibling => {
                let (cap, paid) = trace.cap.as_mut().unwrap();
                let used = *paid + bytes;
                if used > *cap {
                    return false;
                }
                *paid = used;
                true
            }
            _ => true,
        }
    }

    fn cancelled(&self) -> bool {
        let mut trace = self.trace.lock();
        if !trace.armed {
            return false;
        }
        trace.polls += 1;
        match self.fault {
            Fault::Late(Late::Cancel) => true,
            Fault::Late(Late::PollPanic) if trace.polls == 1 => {
                panic_any(InjectedPanic(self.fault))
            }
            _ => false,
        }
    }
}

fn class(local: &str) -> Class {
    Class::new(format!("{PREFIX}{local}"))
}

fn ontology(reachers: usize) -> Vec<Axiom> {
    let mut ofn = format!("Prefix(:=<{PREFIX}>) Ontology(\n");
    ofn.push_str("SubClassOf(:Witness ObjectOneOf(:w))\nSubClassOf(:Witness :Tag)\n");
    for n in 0..reachers {
        writeln!(
            ofn,
            "SubClassOf(:C{n:04} ObjectSomeValuesFrom(:r :Witness))"
        )
        .unwrap();
    }
    ofn.push_str(")\n");
    crate::ofn_read::parse(&ofn).unwrap().axioms
}

fn limits(threads: usize) -> Limits {
    Limits {
        threads,
        el_only: true,
        ..Limits::default()
    }
}

fn observed_limits(probe: &Arc<Probe>, threads: usize) -> Limits {
    let charge = probe.clone();
    let cancel = probe.clone();
    Limits {
        charge: Some(Arc::new(move |bytes| charge.charge(bytes))),
        cancelled: Some(Arc::new(move || cancel.cancelled())),
        ..limits(threads)
    }
}

type Answer = (bool, Option<Taxonomy>);
fn run(
    sat: &mut Saturation,
    kinds: &[&AxiomKind],
    limits: &Limits,
    taxonomy: bool,
) -> Result<Answer, Error> {
    if taxonomy {
        sat.classify(kinds, None, limits)
            .map(|out| (out.consistent, Some(out.taxonomy)))
    } else {
        sat.saturate(kinds, None, limits)
            .map(|out| (out.consistent, None))
    }
}

fn assert_abandoned(sat: &Saturation) {
    assert!(sat.worlds.is_empty());
    assert!(sat.dirty);
    assert!(sat.describe(&class("Witness")).is_none());
}

fn assert_retired(trace: &Trace) {
    let exit = trace.exit.as_ref().unwrap();
    assert_eq!(
        exit.before_release.live, trace.independent,
        "abandoned Found rows must leave only the independent debit"
    );
    assert_eq!(exit.after_release.live, 0);
    assert_eq!(exit.before_release.paid, exit.after_release.paid);
    assert_eq!(exit.before_release.peak, exit.after_release.peak);
}

fn local_error() -> Error {
    Error::Memory {
        limit: usize::MAX,
        used: usize::MAX,
    }
}

#[test]
fn single_chunk_discard() {
    let probe = Probe::new(
        Fault::Local {
            offset: 0,
            after: 1,
        },
        2,
    );
    let axioms = ontology(2);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let error = with_probe(&probe, || {
        run(&mut sat, &kinds, &observed_limits(&probe, 1), true)
    })
    .unwrap_err();
    assert_eq!(error, local_error());
    assert_abandoned(&sat);
    let trace = probe.trace.lock();
    let report = trace.reports[&0][0];
    assert_eq!(trace.reports[&0].len(), 1);
    assert_eq!(trace.admitted[&0], [report.id]);
    assert_eq!(report.before.live, INDEPENDENT);
    assert_eq!(trace.exited, [0]);
    let (collected, marks) = trace.collected.unwrap();
    assert_eq!(collected, Err(LOCAL));
    let exit = trace.exit.as_ref().unwrap();
    assert_eq!(exit.stop, Ok(()));
    assert_eq!(exit.before_release, marks);
    assert_eq!(
        marks.paid,
        report
            .before
            .paid
            .max(trace.base + INDEPENDENT as usize + report.bytes)
    );
    assert_eq!(
        marks.peak,
        report
            .before
            .peak
            .max(trace.base + INDEPENDENT as usize + report.bytes)
    );
    assert_retired(&trace);
}

fn late_stop(stop: Late) {
    let fault = Fault::Late(stop);
    let probe = Probe::new(fault, 2);
    let axioms = ontology(2);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let result = catch_unwind(AssertUnwindSafe(|| {
        with_probe(&probe, || {
            run(&mut sat, &kinds, &observed_limits(&probe, 1), true)
        })
    }));
    assert_abandoned(&sat);
    let trace = probe.trace.lock();
    let [first, second] = trace.reports[&0][..] else {
        panic!("expected two row reports");
    };
    assert_eq!(trace.admitted[&0], [first.id]);
    assert_eq!(trace.exited, [0]);
    let expected_paid = trace.base + trace.independent as usize + first.bytes;
    let attempted = expected_paid + second.bytes;
    assert_eq!(second.before.live, trace.independent + first.bytes as isize);
    assert_eq!(second.before.paid, expected_paid);
    assert_eq!(trace.requests, [second.bytes]);
    assert_eq!(
        trace.polls,
        usize::from(matches!(stop, Late::Cancel | Late::PollPanic))
    );

    let typed = match stop {
        Late::Refuse => Some(Error::Memory {
            limit: usize::MAX,
            used: attempted,
        }),
        Late::Cancel => Some(Error::Cancelled),
        Late::ChargePanic | Late::PollPanic => None,
    };
    let expected_stop = match (stop, typed) {
        (_, Some(error)) => {
            assert_eq!(result.unwrap().unwrap_err(), error);
            assert_eq!(trace.collected.unwrap().0, Err(index::Halt::Stopped));
            Err(error)
        }
        (stop, None) => {
            let payload = result.unwrap_err().downcast::<InjectedPanic>().unwrap();
            assert_eq!(*payload, InjectedPanic(fault));
            assert!(trace.collected.is_none());
            if stop == Late::ChargePanic {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        }
    };
    let exit = trace.exit.as_ref().unwrap();
    assert_eq!(exit.stop, expected_stop);
    assert_eq!(exit.before_release.peak, second.before.peak.max(attempted));
    assert_eq!(
        exit.before_release.paid,
        if matches!(stop, Late::Cancel | Late::PollPanic) {
            attempted
        } else {
            expected_paid
        }
    );
    assert_retired(&trace);
}

#[test]
fn late_cancel_after_second_row_payment() {
    late_stop(Late::Cancel);
}

#[test]
fn late_refusal_on_second_row_payment() {
    late_stop(Late::Refuse);
}

#[test]
fn charge_panic_on_second_row_payment() {
    late_stop(Late::ChargePanic);
}

#[test]
fn poll_panic_after_second_row_payment() {
    late_stop(Late::PollPanic);
}

#[test]
fn expired_deadline_stops_at_entry() {
    let probe = Probe::new(Fault::None, 2);
    let axioms = ontology(2);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let expired = Limits {
        deadline: Some(Instant::now() - Duration::from_secs(1)),
        ..observed_limits(&probe, 1)
    };
    let mut sat = Saturation::new();
    assert_eq!(
        with_probe(&probe, || run(&mut sat, &kinds, &expired, true)).unwrap_err(),
        Error::Timeout
    );
    let trace = probe.trace.lock();
    assert!(trace.workers.is_empty());
    assert!(trace.exit.is_none());
}

#[test]
fn public_retry_after_late_cancel_matches_fresh() {
    let axioms = ontology(2);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    for taxonomy in [true, false] {
        let probe = Probe::new(Fault::Late(Late::Cancel), 2);
        let mut sat = Saturation::new();
        assert_eq!(
            with_probe(&probe, || {
                run(&mut sat, &kinds, &observed_limits(&probe, 1), taxonomy)
            })
            .unwrap_err(),
            Error::Cancelled
        );
        assert_abandoned(&sat);
        {
            let trace = probe.trace.lock();
            assert_eq!(trace.admitted[&0].len(), 1);
            assert_eq!(trace.reports[&0].len(), 2);
            assert_eq!(trace.polls, 1);
        }
        assert_retry_matches_fresh(&mut sat, &kinds, taxonomy);
    }
}

fn assert_retry_matches_fresh(sat: &mut Saturation, kinds: &[&AxiomKind], taxonomy: bool) {
    let mut fresh = Saturation::new();
    assert_eq!(
        run(sat, kinds, &limits(1), taxonomy).unwrap(),
        run(&mut fresh, kinds, &limits(1), taxonomy).unwrap()
    );
    for name in ["Witness", "Tag", "C0000", "C0001"] {
        let name = class(name);
        assert_eq!(sat.describe(&name).unwrap(), fresh.describe(&name).unwrap());
    }
    assert_eq!(sat.worlds, fresh.worlds);
}

fn report_unwind(fault: Fault, observations: usize) {
    let probe = Probe::new(fault, 2);
    let axioms = ontology(2);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let observer = probe.clone();
    admission_test::set_hook(move |point| observer.observation(point));
    let result = catch_unwind(AssertUnwindSafe(|| {
        with_probe(&probe, || {
            run(&mut sat, &kinds, &observed_limits(&probe, 1), true)
        })
    }));
    admission_test::set_hook(|_| {});
    assert_eq!(
        *result.unwrap_err().downcast::<InjectedPanic>().unwrap(),
        InjectedPanic(fault)
    );
    assert_abandoned(&sat);
    let trace = probe.trace.lock();
    let [first, second] = trace.reports[&0][..] else {
        panic!("expected the second row to reach its report");
    };
    assert_eq!(trace.admitted[&0], [first.id]);
    assert_eq!(second.before.live, INDEPENDENT + first.bytes as isize);
    assert_eq!(trace.exited, [0]);
    assert!(trace.collected.is_none());
    assert_eq!(trace.row_observations, observations);
    assert!(trace.requests.is_empty());
    assert_eq!(trace.polls, 0);
    let exit = trace.exit.as_ref().unwrap();
    assert_eq!(exit.stop, Ok(()));
    assert_eq!(
        exit.before_release,
        Marks {
            live: INDEPENDENT,
            ..second.before
        }
    );
    assert_retired(&trace);
}

#[test]
fn row_panic_before_post_retires_buffered_rows() {
    report_unwind(Fault::BeforePost, 0);
}

#[test]
fn row_panic_at_first_observation_retires_posted_row() {
    report_unwind(Fault::ObservePost, 1);
}

fn insertion_unwind(fault: Fault) {
    let axioms = ontology(2);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    for taxonomy in [true, false] {
        let probe = Probe::new(fault, 2);
        let mut sat = Saturation::new();
        let result = catch_unwind(AssertUnwindSafe(|| {
            with_probe(&probe, || {
                run(&mut sat, &kinds, &observed_limits(&probe, 1), taxonomy)
            })
        }));
        assert_eq!(
            *result.unwrap_err().downcast::<InjectedPanic>().unwrap(),
            InjectedPanic(fault)
        );
        assert!(sat.dirty);
        assert!(sat.describe(&class("Witness")).is_none());
        {
            let trace = probe.trace.lock();
            let [first, second, third] = trace.reports[&0][..] else {
                panic!("expected all three rows before insertion");
            };
            assert_eq!(trace.admitted[&0], [first.id, second.id, third.id]);
            assert_eq!(sat.worlds.len(), 1);
            let (bottom, subs) = &sat.worlds[&first.id];
            let mut subs = subs.clone();
            subs.sort_unstable();
            assert!(!bottom);
            assert_eq!(subs, trace.expected[&first.id]);
            let (collected, marks) = trace.collected.unwrap();
            assert_eq!(collected, Ok(3));
            assert_eq!(
                marks.live,
                INDEPENDENT + (first.bytes + second.bytes + third.bytes) as isize
            );
            let exit = trace.exit.as_ref().unwrap();
            assert_eq!(exit.stop, Ok(()));
            assert_eq!(
                exit.before_release,
                Marks {
                    live: INDEPENDENT + first.bytes as isize,
                    ..marks
                }
            );
            assert_eq!(
                exit.after_release,
                Marks {
                    live: first.bytes as isize,
                    ..marks
                }
            );
            assert!(trace.requests.is_empty());
            assert_eq!(trace.polls, 0);
        }
        assert_retry_matches_fresh(&mut sat, &kinds, taxonomy);
    }
}

#[test]
fn insertion_panic_with_extracted_row_keeps_only_committed_debit() {
    insertion_unwind(Fault::BeforeSecondInsert);
}

#[test]
fn insertion_panic_after_commit_keeps_only_committed_debit() {
    insertion_unwind(Fault::AfterFirstInsert);
}

#[test]
fn all_successful_chunks_retire_when_stop_wins_before_commit() {
    let probe = Probe::new(Fault::StopAfterCollection, CROSS_REACHERS);
    let axioms = ontology(CROSS_REACHERS);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let error = with_probe(&probe, || {
        run(&mut sat, &kinds, &observed_limits(&probe, 2), true)
    })
    .unwrap_err();
    assert_eq!(
        error,
        Error::Memory {
            limit: usize::MAX,
            used: COLLECTED_STOP_USED,
        }
    );
    assert_abandoned(&sat);
    let trace = probe.trace.lock();
    assert_two_workers(&trace);
    for offset in [0, SECOND_CHUNK] {
        assert_eq!(trace.admitted[&offset], trace.workers[&offset].1);
    }
    let (collected, marks) = trace.collected.unwrap();
    assert_eq!(collected, Ok(CROSS_REACHERS + 1));
    assert_eq!(marks.live, INDEPENDENT + 64 + 60 * CROSS_REACHERS as isize);
    let exit = trace.exit.as_ref().unwrap();
    assert_eq!(exit.stop, Err(error));
    assert_eq!(
        exit.before_release,
        Marks {
            live: INDEPENDENT,
            ..marks
        }
    );
    assert!(trace.requests.is_empty());
    assert_eq!(trace.polls, 0);
    assert_retired(&trace);
}

fn assert_two_workers(trace: &Trace) {
    assert_eq!(trace.workers.len(), 2);
    let (a, first) = &trace.workers[&0];
    let (b, second) = &trace.workers[&SECOND_CHUNK];
    assert_ne!(a, b);
    assert_ne!(*a, thread::current().id());
    assert_ne!(*b, thread::current().id());
    assert_eq!((first.len(), second.len()), (513, 512));
    assert!(
        first
            .iter()
            .chain(second)
            .copied()
            .eq(trace.expected.keys().copied())
    );
    assert_eq!(trace.exited.len(), 2);
    assert!(trace.exited.contains(&0) && trace.exited.contains(&SECOND_CHUNK));
}

fn committed_transfer(reachers: usize, threads: usize) {
    use admission_test::{Attempt, with_attempt};

    let probe = Probe::new(Fault::None, reachers);
    let axioms = ontology(reachers);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let joined = Arc::new(Mutex::new(None));
    let observed = joined.clone();
    let out = with_attempt(
        None,
        false,
        move |attempt| {
            if let Attempt::Joined {
                base,
                live,
                rows,
                paid,
            } = attempt
            {
                assert!(observed.lock().replace((base, live, rows, paid)).is_none());
            }
        },
        || {
            with_probe(&probe, || {
                run(&mut sat, &kinds, &observed_limits(&probe, threads), true)
            })
        },
    )
    .unwrap();
    assert!(out.0);
    let trace = probe.trace.lock();
    if threads == 1 {
        assert_eq!(trace.workers[&0].0, thread::current().id());
        assert_eq!(trace.exited, [0]);
    } else {
        assert_two_workers(&trace);
    }
    assert_eq!(sat.worlds.len(), trace.expected.len());
    let mut bytes = 0;
    for (&id, expected) in &trace.expected {
        let (bottom, subs) = &sat.worlds[&id];
        let mut subs = subs.clone();
        subs.sort_unstable();
        assert!(!bottom);
        assert_eq!(&subs, expected);
        bytes += if subs.len() == 4 { 64 } else { 60 };
    }
    let (collected, marks) = trace.collected.unwrap();
    let exit = trace.exit.as_ref().unwrap();
    assert_eq!(collected, Ok(reachers + 1));
    assert_eq!(marks.live, INDEPENDENT + bytes);
    assert_eq!(
        *joined.lock(),
        Some((trace.base, marks.live, bytes as usize, marks.paid))
    );
    assert_eq!(exit.stop, Ok(()));
    assert_eq!(exit.before_release, marks);
    assert_eq!(
        exit.after_release,
        Marks {
            live: bytes,
            ..marks
        }
    );
}

#[test]
fn single_chunk_committed_transfer() {
    committed_transfer(2, 1);
}

#[test]
fn cross_chunk_committed_transfer() {
    committed_transfer(CROSS_REACHERS, 2);
}

#[test]
fn checker_growth_survives_found_row_commit_and_discard() {
    let axioms = dt_wiring_tests::world_cover_axioms();
    let kinds: Vec<_> = axioms.iter().collect();
    let root = Class::new("urn:dt:C");

    for fault in [Fault::None, Fault::BeforeFirstInsert] {
        let probe = Probe::with_fixture(fault, Fixture::Datatype);
        let limits = Limits {
            el_only: false,
            ..observed_limits(&probe, 1)
        };
        let mut sat = Saturation::new();
        let result = catch_unwind(AssertUnwindSafe(|| {
            with_probe(&probe, || sat.saturate(&kinds, None, &limits))
        }));
        let trace = probe.trace.lock();
        let (collected, joined) = trace.collected.unwrap();
        let reports = &trace.reports[&0];
        let root_id = sat.interner.class_ex(&root).unwrap();
        assert!(reports.iter().any(|report| report.id == root_id));
        assert_eq!(collected, Ok(reports.len()));
        assert!(
            reports
                .iter()
                .map(|report| report.id)
                .eq(trace.admitted[&0].iter().copied())
        );
        let rows = reports.iter().map(|report| report.bytes).sum::<usize>();

        // The checker accounts from its own tables. No row or world ledger
        // measurements enter this difference.
        let checker = trace.checker.as_ref().unwrap();
        let checker_at_join = checker.at_join.unwrap();
        let growth = checker_at_join.checked_sub(checker.before_worlds).unwrap();
        assert!(growth > 0, "the conditional worlds must grow the checker");
        assert_eq!(joined.live, INDEPENDENT + (growth + rows) as isize);
        assert_eq!(checker.pooled_live, Some(joined.live as usize));

        let retained_rows = if fault == Fault::None {
            assert!(result.unwrap().unwrap().consistent);
            assert_eq!(sat.worlds.len(), reports.len());
            for report in reports {
                let (_, subs) = &sat.worlds[&report.id];
                assert_eq!(report.bytes, 48 + 4 * subs.len());
            }
            rows
        } else {
            let panic = result.unwrap_err().downcast::<InjectedPanic>().unwrap();
            assert_eq!(*panic, InjectedPanic(fault));
            assert!(sat.worlds.is_empty() && sat.dirty);
            assert!(sat.describe(&root).is_none());
            0
        };
        let exit = trace.exit.as_ref().unwrap();
        assert_eq!(exit.stop, Ok(()));
        assert_eq!(exit.checker_bytes, Some(checker_at_join));
        let retained = (growth + retained_rows) as isize;
        assert_eq!(
            exit.before_release,
            Marks {
                live: INDEPENDENT + retained,
                ..joined
            }
        );
        assert_eq!(
            exit.after_release,
            Marks {
                live: retained,
                ..joined
            }
        );
        assert!(trace.requests.is_empty());
        assert_eq!(trace.polls, 0);
    }
}

fn cross_chunk_discard(fault: Fault, success_offset: usize) {
    let probe = Probe::new(fault, CROSS_REACHERS);
    let axioms = ontology(CROSS_REACHERS);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let result = catch_unwind(AssertUnwindSafe(|| {
        with_probe(&probe, || {
            run(&mut sat, &kinds, &observed_limits(&probe, 2), true)
        })
    }));
    assert_abandoned(&sat);
    let trace = probe.trace.lock();
    assert_two_workers(&trace);
    assert_eq!(trace.admitted.len(), 1);
    assert_eq!(
        trace.admitted[&success_offset],
        trace.workers[&success_offset].1
    );
    let exit = trace.exit.as_ref().unwrap();
    assert_eq!(exit.stop, Ok(()));
    if fault == Fault::WorkerPanic {
        let payload = result.unwrap_err().downcast::<InjectedPanic>().unwrap();
        assert_eq!(*payload, InjectedPanic(fault));
        assert!(trace.collected.is_none());
    } else {
        assert_eq!(result.unwrap().unwrap_err(), local_error());
        assert_eq!(trace.collected, Some((Err(LOCAL), exit.before_release)));
    }
    let last = trace.reports[&success_offset].last().unwrap();
    let attempted = trace.base + last.before.live as usize + last.bytes;
    assert_eq!(exit.before_release.paid, last.before.paid.max(attempted));
    assert_eq!(exit.before_release.peak, last.before.peak.max(attempted));
    assert_retired(&trace);
}

#[test]
fn cross_chunk_discards_collected_prefix() {
    cross_chunk_discard(
        Fault::Local {
            offset: SECOND_CHUNK,
            after: 0,
        },
        0,
    );
}

#[test]
fn cross_chunk_discards_uncollected_suffix() {
    cross_chunk_discard(
        Fault::Local {
            offset: 0,
            after: 0,
        },
        SECOND_CHUNK,
    );
}

#[test]
fn cross_chunk_discards_earlier_join_on_panic() {
    cross_chunk_discard(Fault::WorkerPanic, 0);
}

#[test]
fn cross_chunk_sibling_exact_headroom() {
    let probe = Probe::new(Fault::Sibling, CROSS_REACHERS);
    let axioms = ontology(CROSS_REACHERS);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let error = with_probe(&probe, || {
        run(&mut sat, &kinds, &observed_limits(&probe, 2), true)
    })
    .unwrap_err();
    assert_abandoned(&sat);
    let trace = probe.trace.lock();
    assert_two_workers(&trace);
    let first = trace.reports[&0][0];
    let sibling = trace.reports[&SECOND_CHUNK][0];
    assert_eq!(trace.admitted[&0], [first.id]);
    let (cap, _) = trace.cap.unwrap();
    assert_eq!(cap, trace.base + trace.independent as usize + sibling.bytes);
    assert_eq!(
        sibling.before.paid,
        trace.base + trace.independent as usize + first.bytes
    );
    let exit = trace.exit.as_ref().unwrap();
    assert_eq!(trace.collected.unwrap().0, Err(LOCAL));
    if trace.admitted.contains_key(&SECOND_CHUNK) {
        assert_eq!(
            trace.admitted[&SECOND_CHUNK],
            trace.workers[&SECOND_CHUNK].1
        );
        assert_eq!(exit.stop, Ok(()));
        let more = cap.saturating_sub(sibling.before.paid);
        assert_eq!(trace.requests, if more == 0 { vec![] } else { vec![more] });
    } else {
        // This is the exact Batch 55 failure signature. The cap is implemented
        // by the caller, so the typed error still names the configured limit.
        let inflated = cap + first.bytes;
        assert_eq!(
            sibling.before.live,
            trace.independent + first.bytes as isize
        );
        assert_eq!(trace.requests, [sibling.bytes]);
        assert_eq!(
            exit.stop,
            Err(Error::Memory {
                limit: usize::MAX,
                used: inflated
            })
        );
        assert_eq!(error, exit.stop.clone().unwrap_err());
        assert_eq!(exit.before_release.paid, sibling.before.paid);
        assert_eq!(exit.before_release.peak, sibling.before.peak.max(inflated));
        assert_eq!(
            exit.before_release.live,
            trace.independent + (first.bytes + sibling.bytes) as isize
        );
    }
    assert_eq!(
        error,
        local_error(),
        "sibling row fits the caller's exact cap of {cap} bytes"
    );
    assert_eq!(sibling.before.live, trace.independent);
    assert_retired(&trace);
}
