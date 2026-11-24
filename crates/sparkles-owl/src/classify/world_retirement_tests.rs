//! Physical teardown of one resident COW allocation, on return and on unwind.

use super::*;
use crate::classify::numeric::tests::{
    take_watched_deallocations, watch_deallocation, watched_deallocations,
};
use crate::classify::saturate::{Subs, Totals};
use crate::classify::top_chain_cases::{TRAILING, accounting::id, fixture};
use crate::classify::{Limits, Saturation, WorldBudget, admission_test::WAIT};
use parking_lot::Mutex;
use std::alloc::Layout;
use std::panic::{AssertUnwindSafe, catch_unwind, panic_any};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, mpsc};

const INDEPENDENT: isize = 37;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompletionPanic;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Marks {
    live: isize,
    paid: usize,
    peak: usize,
    halt: u8,
    halted_at: usize,
    grants: usize,
    polls: usize,
}

#[derive(Default)]
struct Observations {
    ready: Option<(usize, Marks)>,
    fields_dropped: Option<(Option<usize>, Marks)>,
}

pub(super) struct Witness<'a> {
    marks: &'a (dyn Fn() -> Marks + Sync),
    observed: &'a Mutex<Observations>,
    global_address: usize,
    fresh: Ex,
    panic: bool,
}

impl Witness<'_> {
    pub(super) fn ready(&self, class: Ex, state: &State, found: &Found, reported: usize) {
        let Subs::List(list) = &state.subs else {
            panic!("retirement fixture needs heap-backed private subsumers");
        };
        assert!(!list.is_empty());
        assert!(list.contains(&class) && list.contains(&self.fresh));
        assert!(!found.bottom);
        assert_eq!(found.subs, *list);
        assert_ne!(list.as_ptr().addr(), self.global_address);
        assert_ne!(list.as_ptr().addr(), found.subs.as_ptr().addr());
        assert!(reported > 0);

        watch_deallocation(
            list.as_ptr().addr(),
            Layout::array::<Ex>(list.capacity()).unwrap(),
        );
        self.observed.lock().ready = Some((reported, (self.marks)()));
        if self.panic {
            panic_any(CompletionPanic);
        }
    }
}

impl Drop for Witness<'_> {
    fn drop(&mut self) {
        // Record only. In particular, an early refund must not panic during unwind.
        self.observed.lock().fields_dropped = Some((watched_deallocations(), (self.marks)()));
    }
}

fn saturation(active: bool) -> Saturation {
    let axioms = fixture(&format!(
        "{TRAILING} SubClassOf(:A :Tag) Declaration(Class(:Fresh))"
    ));
    let kinds: Vec<_> = axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    sat.saturate(
        &kinds,
        None,
        &Limits {
            threads: 1,
            el_only: !active,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(sat.index.as_ref().unwrap().top_chain.is_some(), active);
    sat
}

fn with_parked_remainder(budget: &WorldBudget<'_>, check: impl FnOnce()) {
    std::thread::scope(|scope| {
        let (held, wait_held) = mpsc::channel();
        let (release, wait_release) = mpsc::channel();
        let sibling = scope.spawn(move || {
            let previous = budget.post(INDEPENDENT);
            let _debit = Scratch {
                reporter: Reporter::NamedDomain(budget),
                bytes: INDEPENDENT as usize,
            };
            assert!(budget.check_post(INDEPENDENT, previous));
            if held.send(()).is_ok() {
                // A failed assertion drops the release sender before scope joins.
                let _ = wait_release.recv_timeout(WAIT);
            }
        });
        wait_held.recv_timeout(WAIT).unwrap();
        check();
        release.send(()).unwrap();
        sibling.join().unwrap();
    });
    assert_eq!(budget.held.load(Relaxed), 0);
}

fn check_resident_slot(active: bool, panic: bool) {
    let sat = saturation(active);
    let (a, fresh) = (id(&sat, "A"), id(&sat, "Fresh"));
    let ix = sat.index.as_ref().unwrap();
    let global_address = {
        let global = sat.ctx[a as usize].get().unwrap().st.lock();
        assert!(global.init);
        let Subs::List(list) = &global.subs else {
            panic!("fixture's global subsumers must fit a list");
        };
        assert!(!list.is_empty() && !list.contains(&fresh));
        list.as_ptr().addr()
    };

    let grants = Arc::new(AtomicUsize::new(0));
    let polls = Arc::new(AtomicUsize::new(0));
    let limits = Limits {
        charge: Some(Arc::new({
            let grants = grants.clone();
            move |_| {
                grants.fetch_add(1, Relaxed);
                true
            }
        })),
        cancelled: Some(Arc::new({
            let polls = polls.clone();
            move || {
                polls.fetch_add(1, Relaxed);
                false
            }
        })),
        ..Limits::default()
    };
    let charge = limits.charge.as_deref().unwrap() as &(dyn Fn(usize) -> bool + Sync);
    let budget = WorldBudget::start(&limits, Some(charge), 4096, &mut Totals::default()).unwrap();
    let read = || Marks {
        live: budget.held.load(Relaxed),
        paid: budget.paid.load(Relaxed),
        peak: budget.peak.load(Relaxed),
        halt: budget.halt.load(Relaxed),
        halted_at: budget.halted_at.load(Relaxed),
        grants: grants.load(Relaxed),
        polls: polls.load(Relaxed),
    };
    with_parked_remainder(&budget, || {
        let callback_calls = Mutex::new(Vec::with_capacity(1));
        let bound = |delta| {
            if let Some(frees) = watched_deallocations() {
                callback_calls.lock().push((delta, frees, read()));
            }
            budget.tell(delta)
        };
        let observed = Mutex::new(Observations::default());
        let mut world = World::new_with_budget(ix, &sat.ctx, None, &[], &[], &bound, &budget);
        world.retirement_witness = Some(Witness {
            marks: &read,
            observed: &observed,
            global_address,
            fresh,
            panic,
        });
        // Admit the private conclusion through the real queue so A copies its global slot.
        world.pending.push((a, Concl::Sub(fresh)));
        let result = catch_unwind(AssertUnwindSafe(|| world.classify(a)));
        let after = read();
        let final_frees = take_watched_deallocations();
        if panic {
            let Err(payload) = result else {
                panic!("fixture did not raise its completion panic");
            };
            assert_eq!(
                payload.downcast_ref::<CompletionPanic>(),
                Some(&CompletionPanic)
            );
        } else {
            assert!(result.unwrap().unwrap().subs.contains(&fresh));
        }

        let observed = observed.lock();
        let (reported, ready) = observed.ready.unwrap();
        let (frees, fields_dropped) = observed.fields_dropped.unwrap();
        assert_eq!(ready.live, INDEPENDENT + reported as isize);
        assert_eq!((frees, final_frees), (Some(1), Some(1)));
        assert_eq!((ready.halt, ready.halted_at), (0, 0));
        if active {
            assert_eq!(
                fields_dropped, ready,
                "active-world estimate retired before the private fields"
            );
            assert!(callback_calls.lock().is_empty());
            assert_eq!(
                after,
                Marks {
                    live: INDEPENDENT,
                    ..ready
                }
            );
            eprintln!("retirement physical active panic={panic} frees=1 bytes={reported}");
        } else {
            assert_eq!(*callback_calls.lock(), [(-(reported as isize), 0, ready)]);
            assert_eq!(
                after,
                Marks {
                    live: INDEPENDENT,
                    polls: ready.polls + 1,
                    ..ready
                }
            );
            assert_eq!(fields_dropped, after);
            eprintln!("retirement physical inactive frees=1 bytes={reported}");
        }
    });
}

#[test]
fn installed_private_list_drops_before_retirement_on_return() {
    check_resident_slot(true, false);
}

#[test]
fn installed_private_list_drops_before_retirement_on_unwind() {
    check_resident_slot(true, true);
}

#[test]
fn ordinary_callback_precedes_private_list_drop() {
    check_resident_slot(false, false);
}

#[test]
fn ordinary_empty_world_calls_bound_with_zero() {
    let sat = saturation(false);
    let calls = Mutex::new(Vec::new());
    let bound = |delta| {
        calls.lock().push(delta);
        true
    };
    drop(World::new(
        sat.index.as_ref().unwrap(),
        &sat.ctx,
        None,
        &[],
        &[],
        &bound,
    ));
    assert_eq!(*calls.lock(), [0]);
    eprintln!("retirement physical inactive empty delta=0");
}
