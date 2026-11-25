use parking_lot::Mutex;
use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Point {
    BeforeDebit,
    Observed,
    #[allow(dead_code)] // The test-only parent snapshot has no admission lock.
    Contended,
    WorldDataReserve,
    TopChainScratch,
    TopChainRetained,
    TopChainKeptPlan,
    TopChainWorldCheckpoint,
    TopChainWorldDestination,
    TopChainGlobalQueued {
        batched: bool,
    },
    TopChainGlobalTaken {
        tagged: usize,
        ordinary: usize,
    },
    TopChainGlobalOutbox {
        tagged: usize,
        ordinary: usize,
        ordinary_payloads: usize,
    },
    TopChainGlobalFiltered {
        tagged: usize,
    },
    TopChainGlobalJoin {
        tagged: usize,
        queued: isize,
    },
    TopChainScan,
    TopChainGlobalMessage,
    TopChainWorldMessage,
    BeforePanicWake,
    RebaseScratchStart,
    RebaseAdmit {
        live: usize,
        scratch: usize,
        paid: usize,
    },
    RebaseShift,
    RebaseDynamicStart,
    RebaseDynamicEnd,
    InvalidateInitialStart,
    InvalidateInitialEnd,
    InvalidateLargeCoreStart,
    InvalidateLargeCoreEnd,
    InvalidateClosureStart,
    InvalidateClosureEnd,
    DisjointGrowthAdmit {
        live: usize,
        growth: usize,
        paid: usize,
    },
    DisjointReindex,
    DisjointReindexEnd,
    InvalidateFinalStart,
    InvalidateFinalEnd,
    DlWidthStop,
    KeptRetry {
        dl_worked: bool,
    },
    Idle,
}

type Hook = Box<dyn FnMut(Point)>;

thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

pub(super) fn set_hook(hook: impl FnMut(Point) + 'static) {
    HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
}

pub(super) fn with_hook<T>(hook: impl FnMut(Point) + 'static, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<Hook>);
    impl Drop for Restore {
        fn drop(&mut self) {
            HOOK.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let previous = HOOK.with(|slot| slot.borrow_mut().replace(Box::new(hook)));
    let _restore = Restore(previous);
    run()
}

pub(super) fn with_point_hook<T>(hook: impl FnMut(Point) + 'static, run: impl FnOnce() -> T) -> T {
    with_hook(hook, run)
}

pub(super) fn has_hook() -> bool {
    HOOK.with(|slot| slot.borrow().is_some())
}
pub(super) fn hit(point: Point) {
    HOOK.with(|h| {
        if let Some(hook) = h.borrow_mut().as_mut() {
            hook(point);
        }
    });
}

pub(super) const WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Attempt {
    Probe {
        paid: usize,
    },
    Dependencies {
        messages: usize,
        peak: usize,
        paid: usize,
    },
    Pending {
        actual: usize,
        counted: usize,
    },
    Marking {
        before: usize,
        after: usize,
        seeds: usize,
        counted: usize,
    },
    Joined {
        base: usize,
        live: isize,
        rows: usize,
        paid: usize,
    },
    Marker {
        base: usize,
        rows: usize,
        paid: usize,
        contexts: usize,
    },
    Scan {
        scratch: usize,
        paid: usize,
    },
    Retained {
        actual: usize,
        pending: usize,
        bytes: usize,
        root_bytes: usize,
        paid: usize,
    },
    Taxonomy {
        scratch: usize,
        paid: usize,
    },
}

#[derive(Clone, Copy)]
pub(super) struct PendingProbe {
    pub(super) messages: usize,
    pub(super) after_merge: bool,
}

struct AttemptHook {
    observe: Box<dyn FnMut(Attempt)>,
    probe: Option<PendingProbe>,
    individuals: bool,
}

thread_local! {
    static ATTEMPT: RefCell<Option<AttemptHook>> = const { RefCell::new(None) };
}

pub(super) fn with_attempt<T>(
    probe: Option<PendingProbe>,
    individuals: bool,
    observe: impl FnMut(Attempt) + 'static,
    run: impl FnOnce() -> T,
) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ATTEMPT.with(|slot| slot.borrow_mut().take());
        }
    }
    ATTEMPT.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_none());
        *slot = Some(AttemptHook {
            observe: Box::new(observe),
            probe,
            individuals,
        });
    });
    let _reset = Reset;
    run()
}

pub(super) fn observing() -> bool {
    ATTEMPT.with(|slot| slot.borrow().is_some())
}

pub(super) fn observe(event: Attempt) {
    ATTEMPT.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            (hook.observe)(event);
        }
    });
}

pub(super) fn probe(merged: bool) -> Option<PendingProbe> {
    ATTEMPT.with(|slot| {
        let mut attempt = slot.borrow_mut();
        let probe = &mut attempt.as_mut()?.probe;
        if probe.is_some_and(|p| !p.after_merge || merged) {
            probe.take()
        } else {
            None
        }
    })
}

pub(super) fn individual_rules() -> bool {
    ATTEMPT.with(|slot| slot.borrow().as_ref().is_some_and(|h| h.individuals))
}

/// Inspect actual lists independently of the handoff's counted value.
pub(super) fn queued(ctx: &super::Contexts, dynamic: Option<&super::saturate::Dynamic>) -> usize {
    let mut messages: usize = ctx
        .iter()
        .filter_map(|c| c.get())
        .map(|c| c.todo.lock().len())
        .sum();
    if let Some(d) = dynamic {
        for i in 0..d.len() as super::Ex {
            if let Some(c) = d.get_ctx(d.base() + i) {
                messages += c.todo.lock().len();
            }
        }
    }
    messages
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ChargePanic;

/// Blocks the first charge until the controller has arranged the race. Any
/// re-entry during unwind records its request and refuses without panicking.
pub(super) struct FirstCharge {
    pub(super) entered: Receiver<usize>,
    pub(super) release: Sender<()>,
    pub(super) requests: Mutex<Vec<usize>>,
    send: Sender<usize>,
    proceed: Mutex<Receiver<()>>,
}

impl FirstCharge {
    pub(super) fn new() -> Self {
        let (send, entered) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        Self {
            entered,
            release,
            requests: Mutex::default(),
            send,
            proceed: Mutex::new(proceed),
        }
    }

    pub(super) fn charge(&self, panics: bool) -> impl Fn(usize) -> bool + Sync {
        let (send, proceed, requests) = (&self.send, &self.proceed, &self.requests);
        move |bytes| {
            let mut calls = requests.lock();
            let first = calls.is_empty();
            calls.push(bytes);
            drop(calls);
            if !first || send.send(bytes).is_err() || proceed.lock().recv_timeout(WAIT).is_err() {
                return false;
            }
            if panics {
                std::panic::panic_any(ChargePanic);
            }
            true
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Observed,
    Charge(usize),
    Contended,
    Returned(bool),
}

/// The controller is the only participant that releases the first pool call. All
/// expectations are checked after releasing the workers, including on a timeout.
pub(super) struct Race {
    send: Sender<Event>,
    events: Receiver<Event>,
    release: Sender<()>,
    pool: Mutex<Receiver<()>>,
    requests: Mutex<Vec<usize>>,
    accept: bool,
}

impl Race {
    pub(super) fn new(accept: bool) -> Self {
        let (send, events) = mpsc::channel();
        let (release, pool) = mpsc::channel();
        Self {
            send,
            events,
            release,
            pool: Mutex::new(pool),
            requests: Mutex::default(),
            accept,
        }
    }

    pub(super) fn charge(&self) -> impl Fn(usize) -> bool + Sync {
        // Capture the callback's fields so the controller receiver need not be Sync.
        let (send, requests, pool, accept) = (&self.send, &self.requests, &self.pool, self.accept);
        move |bytes| {
            let mut calls = requests.lock();
            let first = calls.is_empty();
            calls.push(bytes);
            drop(calls);
            send.send(Event::Charge(bytes)).unwrap();
            if first {
                pool.lock().recv_timeout(WAIT).unwrap();
            }
            accept
        }
    }

    pub(super) fn run(
        &self,
        first: impl FnOnce() -> bool + Send,
        second: impl FnOnce() -> bool + Send,
        smaller_first: bool,
    ) {
        let (resume, paused) = mpsc::channel();
        let (initial, callback, next, first, second) = thread::scope(|scope| {
            let a = self.send.clone();
            let first = scope.spawn(move || {
                let out = a.clone();
                set_hook(move |point| match point {
                    Point::Observed if !smaller_first => {
                        a.send(Event::Observed).unwrap();
                        paused.recv_timeout(WAIT).unwrap();
                    }
                    Point::Contended => a.send(Event::Contended).unwrap(),
                    _ => {}
                });
                let ok = first();
                out.send(Event::Returned(ok)).unwrap();
                ok
            });
            let initial = self.events.recv_timeout(WAIT);
            let b = self.send.clone();
            let second = scope.spawn(move || {
                let out = b.clone();
                set_hook(move |point| {
                    if point == Point::Contended {
                        b.send(Event::Contended).unwrap();
                    }
                });
                let ok = second();
                out.send(Event::Returned(ok)).unwrap();
                ok
            });
            let callback = (!smaller_first).then(|| self.events.recv_timeout(WAIT));
            let _ = resume.send(());
            let next = self.events.recv_timeout(WAIT);
            let _ = self.release.send(());
            (initial, callback, next, first.join(), second.join())
        });
        if smaller_first {
            assert_eq!(initial.unwrap(), Event::Charge(64));
        } else {
            assert_eq!(initial.unwrap(), Event::Observed);
            assert_eq!(callback.unwrap().unwrap(), Event::Charge(192));
        }
        assert_eq!(next.unwrap(), Event::Contended);
        assert_eq!(first.unwrap(), self.accept);
        assert_eq!(second.unwrap(), self.accept);
        let want = match (smaller_first, self.accept) {
            (true, true) => &[64, 128][..],
            (true, false) => &[64],
            (false, _) => &[192],
        };
        assert_eq!(&*self.requests.lock(), want);
    }
}
