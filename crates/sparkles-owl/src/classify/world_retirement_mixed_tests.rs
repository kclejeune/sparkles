//! Ordered failures with an installed world and an independently owned result chunk.

use super::*;
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind, panic_any};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::thread::{self, ThreadId};

const PREFIX_ROWS: usize = 4;
const LIMIT: usize = 1 << 30;
const FIRST_STOP: usize = LIMIT + 17;
const B_LOCAL: index::Halt = index::Halt::Memory(usize::MAX - 11);
const A_LOCAL: index::Halt = index::Halt::Memory(usize::MAX - 12);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    RefuseWorld,
    PanicAfterRowGrant,
    StopBeforeRow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RowPollPanic;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    A = 0,
    B = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    WorldPost,
    WorldCharge,
    RowCharge,
    RowPoll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Marks {
    live: isize,
    paid: usize,
    peak: usize,
    halt: u8,
    payload: usize,
    grants: usize,
    polls: usize,
}

struct Entry {
    thread: ThreadId,
    offset: usize,
    classes: Vec<Ex>,
}

#[derive(Default)]
struct Trace {
    base: usize,
    classes: Vec<Ex>,
    workers: [Option<Entry>; 2],
    exited: [Option<Marks>; 2],
    prefix: Vec<(Ex, isize)>,
    b_parked: bool,
    b_local: Option<Marks>,
    a_private: Option<(Ex, isize, isize, Marks)>,
    a_retired: Option<(Marks, isize)>,
    a_next_slot: Option<(Ex, isize, Marks)>,
    armed_row: Option<(Ex, isize, isize, Marks)>,
    posted: Option<Marks>,
    requests: Vec<(Arm, usize)>,
    collected: Option<(Option<index::Halt>, Marks)>,
    phase_exit: Option<(Marks, Marks)>,
}

pub(super) struct Case {
    scenario: Scenario,
    trace: Mutex<Trace>,
    changed: Condvar,
    independent: AtomicIsize,
    grants: AtomicUsize,
    polls: AtomicUsize,
}

struct Thread {
    case: Arc<Case>,
    lane: Lane,
    slot_posts: usize,
}

thread_local! {
    static ACTIVE: RefCell<Option<Arc<Case>>> = const { RefCell::new(None) };
    static WORKER: RefCell<Option<Thread>> = const { RefCell::new(None) };
    static ARM: Cell<Option<Arm>> = const { Cell::new(None) };
}

pub(super) fn active() -> Option<Arc<Case>> {
    ACTIVE.with(|case| case.borrow().clone())
}

fn worker_a() -> Option<Arc<Case>> {
    WORKER.with(|worker| {
        let worker = worker.borrow();
        let worker = worker.as_ref()?;
        (worker.lane == Lane::A).then(|| worker.case.clone())
    })
}

pub(super) fn before_slot_post(budget: &WorldBudget<'_>, to: Ex, delta: isize) {
    let selected = WORKER.with(|worker| {
        let mut worker = worker.borrow_mut();
        let worker = worker.as_mut()?;
        if worker.lane != Lane::A || worker.case.scenario != Scenario::RefuseWorld {
            return None;
        }
        // The first slot belongs to A's root. The second is a genuine destination
        // reached from that root, with the root's 64-byte slot still owned.
        worker.slot_posts += 1;
        (worker.slot_posts == 2).then(|| worker.case.clone())
    });
    let Some(case) = selected else { return };
    {
        let mut trace = case.trace.lock();
        case.wait(&mut trace, "B's admitted row prefix", |t| {
            t.b_parked || t.exited[Lane::B as usize].is_some()
        });
        assert!(trace.b_parked);
        assert_ne!(to, trace.classes[0]);
    }
    let before = case.fill_headroom(budget);
    let mut trace = case.trace.lock();
    let rows = trace.prefix.iter().map(|&(_, bytes)| bytes).sum::<isize>();
    assert!(rows >= delta && delta > 0);
    assert_eq!(before.live, case.independent.load(Relaxed) + rows + delta);
    trace.a_next_slot = Some((to, delta, before));
    ARM.set(Some(Arm::WorldPost));
}

// Called after the destination Scratch owns the specific positive slot post.
pub(super) fn world_posted(budget: &WorldBudget<'_>, delta: isize, previous: isize) {
    if ARM.get() != Some(Arm::WorldPost) {
        return;
    }
    ARM.set(None);
    let case = worker_a().expect("armed destination belongs to worker A");
    let mut trace = case.trace.lock();
    let (_, armed_delta, before) = trace.a_next_slot.unwrap();
    assert!(trace.posted.is_none());
    let posted = case.marks(budget);
    assert_eq!((delta, previous), (armed_delta, before.live));
    assert_eq!(
        posted,
        Marks {
            live: before.live + delta,
            ..before
        }
    );
    trace.posted = Some(posted);
    case.changed.notify_all();
    case.wait(&mut trace, "B's post-chunk exit", |t| {
        t.exited[Lane::B as usize].is_some()
    });
    ARM.set(Some(Arm::WorldCharge));
}

pub(super) fn world_ready(budget: &WorldBudget<'_>, class: Ex, reported: usize, subs_bytes: isize) {
    let Some(case) = worker_a() else { return };
    if case.scenario == Scenario::RefuseWorld {
        return;
    }
    let mut trace = case.trace.lock();
    assert_eq!(class, trace.classes[0]);
    assert!(reported > 0);
    let marks = case.marks(budget);
    assert_eq!(
        marks.live,
        case.independent.load(Relaxed) + reported as isize
    );
    trace.a_private = Some((class, reported as isize, subs_bytes, marks));
    case.changed.notify_all();
    case.wait(&mut trace, "B's post-chunk exit", |t| {
        t.exited[Lane::B as usize].is_some()
    });
}

// Called only for the row owner, after `grow_guard` owns the posted resize bytes.
pub(super) fn row_posted(
    budget: &WorldBudget<'_>,
    bytes: isize,
    previous: isize,
    outstanding: isize,
) {
    let case = WORKER.with(|worker| {
        let worker = worker.borrow();
        let worker = worker.as_ref()?;
        (worker.lane == Lane::B).then(|| worker.case.clone())
    });
    let Some(case) = case else { return };
    let mut trace = case.trace.lock();
    let Some((_, armed_bytes, _, before)) = trace.armed_row else {
        return;
    };
    assert!(trace.posted.is_none());
    assert_eq!((bytes, previous), (armed_bytes, before.live));
    let prefix = trace.prefix.iter().map(|&(_, bytes)| bytes).sum::<isize>();
    assert_eq!(outstanding, prefix + bytes);
    let posted = case.marks(budget);
    assert_eq!(
        posted,
        Marks {
            live: before.live + bytes,
            ..before
        }
    );
    trace.posted = Some(posted);
    if case.scenario == Scenario::PanicAfterRowGrant {
        ARM.set(Some(Arm::RowCharge));
    }
}

impl Case {
    fn marks(&self, budget: &WorldBudget<'_>) -> Marks {
        Marks {
            live: budget.held.load(Relaxed),
            paid: budget.paid.load(Relaxed),
            peak: budget.peak.load(Relaxed),
            halt: budget.halt.load(Relaxed),
            payload: budget.halted_at.load(Relaxed),
            grants: self.grants.load(Relaxed),
            polls: self.polls.load(Relaxed),
        }
    }

    fn wait(
        &self,
        trace: &mut MutexGuard<'_, Trace>,
        boundary: &str,
        ready: impl Fn(&Trace) -> bool,
    ) {
        let deadline = Instant::now() + admission_test::WAIT;
        while !ready(trace) {
            // The condvar unlocks the trace while parked. No world context or
            // admission lock is held at any of these boundaries.
            let timed_out = self.changed.wait_until(trace, deadline).timed_out();
            assert!(
                !timed_out || ready(trace),
                "timed out waiting for {boundary}"
            );
        }
    }

    fn post_independent(&self, budget: &WorldBudget<'_>, delta: isize) {
        if delta > 0 {
            let previous = budget.post(delta);
            self.independent.fetch_add(delta, Relaxed);
            assert!(budget.check_post(delta, previous));
        }
    }

    fn fill_headroom(&self, budget: &WorldBudget<'_>) -> Marks {
        let before = self.marks(budget);
        let unused = before
            .paid
            .checked_sub(budget.base + before.live as usize)
            .unwrap();
        self.post_independent(budget, unused as isize);
        let filled = self.marks(budget);
        assert_eq!(filled.paid, budget.base + filled.live as usize);
        assert_eq!(filled.halt, 0);
        filled
    }

    fn charge(&self, bytes: usize) -> bool {
        self.grants.fetch_add(1, Relaxed);
        match ARM.take() {
            Some(Arm::WorldCharge) => {
                self.trace.lock().requests.push((Arm::WorldCharge, bytes));
                false
            }
            Some(Arm::RowCharge) => {
                self.trace.lock().requests.push((Arm::RowCharge, bytes));
                ARM.set(Some(Arm::RowPoll));
                true
            }
            _ => true,
        }
    }

    fn cancelled(&self) -> bool {
        self.polls.fetch_add(1, Relaxed);
        // Disarm the fault before unwinding. The final poll is outside admission.
        if ARM.take() == Some(Arm::RowPoll) {
            panic_any(RowPollPanic);
        }
        false
    }

    pub(super) fn phase<'p, 'b, 'a>(
        &'p self,
        budget: &'b WorldBudget<'a>,
        classes: &[Ex],
        installed: bool,
        committed: usize,
    ) -> Phase<'p, 'b, 'a> {
        assert!(installed);
        assert_eq!(committed, 0);
        assert_eq!(classes.len(), MIN_CLASSES + 1);
        {
            let mut trace = self.trace.lock();
            trace.base = budget.base;
            trace.classes = classes.to_vec();
        }
        let phase = Phase { case: self, budget };
        self.post_independent(budget, 37);
        phase
    }

    pub(super) fn worker<'p, 'b, 'a>(
        self: &'p Arc<Self>,
        offset: usize,
        chunk: &[Ex],
        budget: &'b WorldBudget<'a>,
    ) -> Worker<'p, 'b, 'a> {
        let lane = if offset == 0 { Lane::A } else { Lane::B };
        let entry = Entry {
            thread: thread::current().id(),
            offset,
            classes: chunk.to_vec(),
        };
        assert!(
            self.trace.lock().workers[lane as usize]
                .replace(entry)
                .is_none()
        );
        WORKER.with(|worker| {
            assert!(
                worker
                    .borrow_mut()
                    .replace(Thread {
                        case: self.clone(),
                        lane,
                        slot_posts: 0,
                    })
                    .is_none()
            );
        });
        Worker {
            case: self,
            budget,
            lane,
        }
    }

    pub(super) fn collected(&self, budget: &WorldBudget<'_>, halt: Option<index::Halt>) {
        self.trace.lock().collected = Some((halt, self.marks(budget)));
    }
}

pub(super) struct Phase<'p, 'b, 'a> {
    case: &'p Case,
    budget: &'b WorldBudget<'a>,
}

impl Drop for Phase<'_, '_, '_> {
    fn drop(&mut self) {
        let before = self.case.marks(self.budget);
        self.budget.retire(self.case.independent.load(Relaxed));
        self.case.trace.lock().phase_exit = Some((before, self.case.marks(self.budget)));
    }
}

pub(super) struct Worker<'p, 'b, 'a> {
    case: &'p Case,
    budget: &'b WorldBudget<'a>,
    lane: Lane,
}

impl Worker<'_, '_, '_> {
    pub(super) fn before_world(&self, buffered: usize) -> Result<(), index::Halt> {
        if self.lane == Lane::A {
            return Ok(());
        }
        let case = self.case;
        let mut trace = case.trace.lock();
        if case.scenario == Scenario::RefuseWorld && buffered == PREFIX_ROWS {
            trace.b_parked = true;
            case.changed.notify_all();
            case.wait(&mut trace, "A's posted slot", |t| {
                t.posted.is_some() || t.exited[Lane::A as usize].is_some()
            });
            assert!(trace.posted.is_some());
            trace.b_local = Some(case.marks(self.budget));
            return Err(B_LOCAL);
        }
        if case.scenario != Scenario::RefuseWorld && buffered == 0 {
            case.wait(&mut trace, "A's completed world", |t| {
                t.a_private.is_some() || t.exited[Lane::A as usize].is_some()
            });
            assert!(trace.a_private.is_some());
        }
        Ok(())
    }

    pub(super) fn after_world(&self, class: Ex, subs_bytes: isize) -> Result<(), index::Halt> {
        if self.lane == Lane::A {
            let mut trace = self.case.trace.lock();
            assert_eq!(class, trace.classes[0]);
            trace.a_retired = Some((self.case.marks(self.budget), subs_bytes));
            // A cannot finish its world in the refusal schedule. In the row
            // schedules, sample A's retirement and end before staging another row.
            return Err(A_LOCAL);
        }
        Ok(())
    }

    pub(super) fn before_row_post(&self, staged: usize, class: Ex, bytes: isize) {
        if self.lane == Lane::B
            && self.case.scenario != Scenario::RefuseWorld
            && staged == PREFIX_ROWS + 1
        {
            let case = self.case;
            let mut before = case.fill_headroom(self.budget);
            if case.scenario == Scenario::StopBeforeRow {
                self.budget.stop_for_memory(FIRST_STOP);
                before = case.marks(self.budget);
            }
            let mut trace = case.trace.lock();
            let prefix = trace.prefix.iter().map(|&(_, bytes)| bytes).sum::<isize>();
            let (_, private, _, _) = trace.a_private.unwrap();
            let row_subs = before.live - (case.independent.load(Relaxed) + prefix + private);
            assert!(row_subs > 0, "armed row subs must be pre-admitted");
            trace.armed_row = Some((class, bytes, row_subs, before));
        }
    }

    pub(super) fn admitted(&self, class: Ex, bytes: isize) {
        if self.lane == Lane::B {
            self.case.trace.lock().prefix.push((class, bytes));
        }
    }
}

impl Drop for Worker<'_, '_, '_> {
    fn drop(&mut self) {
        // This guard is declared before the chunk. Do not assert or allocate here.
        ARM.set(None);
        drop(WORKER.take());
        self.case.trace.lock().exited[self.lane as usize] = Some(self.case.marks(self.budget));
        self.case.changed.notify_all();
    }
}

fn exercise(scenario: Scenario) {
    use top_chain_cases::{TRAILING, fixture};

    let mut body = format!(
        "{TRAILING}
        SubClassOf(:Witness ObjectOneOf(:w)) SubClassOf(:Witness :Tag)"
    );
    for n in 0..MIN_CLASSES {
        write!(
            body,
            " SubClassOf(:C{n:04} ObjectSomeValuesFrom(:r :Witness))"
        )
        .unwrap();
    }
    let axioms = fixture(&body);
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let case = Arc::new(Case {
        scenario,
        trace: Mutex::default(),
        changed: Condvar::new(),
        independent: AtomicIsize::new(0),
        grants: AtomicUsize::new(0),
        polls: AtomicUsize::new(0),
    });
    let grant = case.clone();
    let poll = case.clone();
    let limits = Limits {
        threads: 2,
        max_memory: Some(LIMIT),
        charge: Some(Arc::new(move |bytes| grant.charge(bytes))),
        cancelled: Some(Arc::new(move || poll.cancelled())),
        ..Limits::default()
    };
    ACTIVE.with(|slot| assert!(slot.borrow_mut().replace(case.clone()).is_none()));
    let result = catch_unwind(AssertUnwindSafe(|| sat.saturate(&kinds, None, &limits)));
    drop(ACTIVE.take());

    assert!(sat.worlds.is_empty() && sat.dirty);
    let trace = case.trace.lock();
    let a = trace.workers[Lane::A as usize].as_ref().unwrap();
    let b = trace.workers[Lane::B as usize].as_ref().unwrap();
    assert_ne!(a.thread, b.thread);
    assert_eq!((a.offset, b.offset), (0, trace.classes.len().div_ceil(2)));
    assert_eq!(a.classes, trace.classes[..b.offset]);
    assert_eq!(b.classes, trace.classes[b.offset..]);
    assert_eq!(trace.prefix.len(), PREFIX_ROWS);
    assert!(trace.prefix.iter().all(|&(_, bytes)| bytes > 0));
    assert!(
        trace
            .prefix
            .iter()
            .map(|&(class, _)| class)
            .eq(b.classes[..PREFIX_ROWS].iter().copied())
    );
    let prefix = trace.prefix.iter().map(|&(_, bytes)| bytes).sum::<isize>();
    assert!(prefix >= (PREFIX_ROWS * ROW_SLOT_BYTES) as isize);
    let independent = case.independent.load(Relaxed);
    let posted = trace.posted.unwrap();
    let a_exit = trace.exited[Lane::A as usize].unwrap();
    let b_exit = trace.exited[Lane::B as usize].unwrap();
    let (phase, released) = trace.phase_exit.unwrap();
    assert_eq!(a_exit, phase);
    assert_eq!(phase.live, independent);
    assert_eq!(released, Marks { live: 0, ..phase });

    match scenario {
        Scenario::RefuseWorld => {
            let (to, delta, before) = trace.a_next_slot.unwrap();
            let used = before.paid + delta as usize;
            assert_ne!(to, a.classes[0]);
            assert_eq!(before.paid, trace.base + before.live as usize);
            assert!(trace.a_retired.is_none());
            assert_eq!(trace.b_local, Some(posted));
            assert_eq!(
                b_exit,
                Marks {
                    live: posted.live - prefix,
                    ..posted
                }
            );
            assert!(trace.base + b_exit.live as usize <= before.paid);
            assert_eq!(trace.requests, [(Arm::WorldCharge, delta as usize)]);
            assert_eq!(
                phase,
                Marks {
                    live: independent,
                    peak: posted.peak.max(used),
                    halt: WORLD_MEMORY,
                    payload: used,
                    grants: posted.grants + 1,
                    ..posted
                }
            );
            assert_eq!(trace.collected, Some((Some(index::Halt::Stopped), phase)));
            assert!(
                matches!(result, Ok(Err(Error::Memory { limit: LIMIT, used: got })) if got == used)
            );
            eprintln!("retirement mixed refused-world delta={delta} prefix={prefix} used={used}");
        }
        Scenario::PanicAfterRowGrant | Scenario::StopBeforeRow => {
            let (class, private, a_subs, ready) = trace.a_private.unwrap();
            let (row, bytes, row_subs, before) = trace.armed_row.unwrap();
            assert_eq!((class, row), (a.classes[0], b.classes[PREFIX_ROWS]));
            assert_eq!(before.paid, trace.base + before.live as usize);
            assert_eq!(ready.halt, 0);
            assert_eq!(posted.live - b_exit.live, prefix + row_subs + bytes);
            assert_eq!(b_exit.live, independent + private);
            let (a_retired_marks, observed_a_subs) = trace.a_retired.unwrap();
            assert_eq!(observed_a_subs, a_subs);
            assert_eq!(
                a_retired_marks,
                Marks {
                    live: independent + a_subs,
                    ..b_exit
                }
            );
            assert_eq!(
                phase,
                Marks {
                    live: independent,
                    ..a_retired_marks
                }
            );
            if scenario == Scenario::PanicAfterRowGrant {
                let used = before.paid + bytes as usize;
                assert_eq!(trace.requests, [(Arm::RowCharge, bytes as usize)]);
                assert_eq!(
                    b_exit,
                    Marks {
                        live: independent + private,
                        paid: used,
                        peak: before.peak.max(used),
                        grants: before.grants + 1,
                        polls: before.polls + 1,
                        ..before
                    }
                );
                assert_eq!((b_exit.halt, b_exit.payload), (0, 0));
                assert!(trace.collected.is_none());
                let Err(payload) = result else {
                    panic!("scoped join did not propagate the row poll panic");
                };
                assert_eq!(payload.downcast_ref::<RowPollPanic>(), Some(&RowPollPanic));
                eprintln!(
                    "retirement mixed row-poll-panic row={bytes} prefix={prefix} private={private}"
                );
            } else {
                assert!(trace.requests.is_empty());
                assert_eq!(
                    b_exit,
                    Marks {
                        live: independent + private,
                        ..before
                    }
                );
                assert_eq!((b_exit.halt, b_exit.payload), (WORLD_MEMORY, FIRST_STOP));
                assert_eq!(trace.collected, Some((Some(A_LOCAL), phase)));
                assert!(matches!(
                    result,
                    Ok(Err(Error::Memory {
                        limit: LIMIT,
                        used: FIRST_STOP
                    }))
                ));
                eprintln!(
                    "retirement mixed first-stop row={bytes} prefix={prefix} used={FIRST_STOP}"
                );
            }
        }
    }
    eprintln!(
        "retirement mixed workers {:?} offset={} {:?} offset={} selected={}",
        a.thread,
        a.offset,
        b.thread,
        b.offset,
        trace.classes.len()
    );
}

#[test]
fn private_post_refuses_against_snapshot_after_sibling_rows_drop() {
    exercise(Scenario::RefuseWorld);
}

#[test]
fn row_poll_panic_discards_rows_while_installed_world_is_live() {
    exercise(Scenario::PanicAfterRowGrant);
}

#[test]
fn prepublished_memory_stop_survives_sibling_row_post_and_drop() {
    exercise(Scenario::StopBeforeRow);
}
