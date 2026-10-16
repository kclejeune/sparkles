use crate::error::Error;
use std::cell::{Cell, RefCell};
use std::sync::{Arc, atomic::AtomicBool, mpsc};
use std::time::Instant;

#[derive(Clone, Copy)]
pub(super) enum BuildMode {
    Normal,
    Error,
    Unwind,
}

type WaitObserver = Box<dyn FnMut()>;
type WakeObserver = Box<dyn FnMut(usize)>;

thread_local! {
    static BUILD_MODE: Cell<BuildMode> = const { Cell::new(BuildMode::Normal) };
    static WAITING: RefCell<Option<WaitObserver>> = const { RefCell::new(None) };
    static WOKEN: RefCell<Option<WakeObserver>> = const { RefCell::new(None) };
}

pub(super) fn build_mode() -> BuildMode {
    BUILD_MODE.with(Cell::get)
}

pub(super) fn waiting() {
    WAITING.with(|slot| {
        if let Some(observer) = slot.borrow_mut().as_mut() {
            observer();
        }
    });
}

pub(super) fn woken(in_use: usize) {
    WOKEN.with(|slot| {
        if let Some(observer) = slot.borrow_mut().as_mut() {
            observer(in_use);
        }
    });
}

fn isolated(case: &str, body: impl FnOnce()) {
    const CHILD: &str = "SPARKLES_F14_POOL_CASE";
    if std::env::var(CHILD).as_deref() == Ok(case) {
        println!("isolated pool case {case} pid={}", std::process::id());
        body();
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("algo::pool::tests::{case}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, case)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "isolated {case} returned {status}");
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("isolated {case} exceeded its process deadline");
        }
        // Only polls process completion; ordering inside the fixture uses channels.
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Debug)]
enum Event {
    Waiting,
    Returned(crate::Result<usize>),
}

fn join_bounded(handle: std::thread::JoinHandle<()>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(handle.is_finished(), "released worker failed to finish");
    handle.join().unwrap();
}

fn saturation(capacity: usize, owner_threads: usize, deadline: bool, service: bool) {
    configure(capacity, capacity);
    let owner = Par::acquire(owner_threads, &Control::unlimited()).unwrap();
    let extra = (owner_threads < capacity)
        .then(|| Par::acquire(capacity - owner_threads, &Control::unlimited()).unwrap());
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let observation = tx.clone();
        let mut sent = false;
        WAITING.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                if !sent {
                    observation.send(Event::Waiting).unwrap();
                    sent = true;
                }
            }))
        });
        let result = if service {
            use crate::io::{RdfFormat, Source};
            use crate::sparql::{QueryOptions, query};
            use crate::store::{Store, StoreOptions};
            let store = Store::in_memory(StoreOptions::default());
            store
                .load(&[Source::from_bytes(
                    b"<urn:a> <urn:p> <urn:b> .".to_vec(),
                    RdfFormat::NTriples,
                    None,
                )])
                .unwrap();
            query(store.snapshot(), "PREFIX algo: <urn:x-sparkles:algo#> SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:concurrency 1 ; algo:node ?n ; algo:component ?c } }", &QueryOptions { cancel: Some(flag), ..Default::default() }).map(|r| r.rows().len())
        } else {
            let control = Control::new(
                flag,
                deadline.then(|| Instant::now() + Duration::from_secs(2)),
                u64::MAX,
            );
            Par::acquire(1, &control).map(|par| {
                assert!(
                    par.pool.is_none(),
                    "single-thread admission constructed a pool"
                );
                par.threads()
            })
        };
        tx.send(Event::Returned(result)).unwrap();
    });
    let first = rx.recv_timeout(Duration::from_secs(5));
    let observed_wait = matches!(first, Ok(Event::Waiting));
    if !deadline || !observed_wait {
        cancel.store(true, Ordering::Relaxed);
        SLOTS.freed.notify_all();
    }
    let result = match first {
        Ok(Event::Returned(result)) => Some(result),
        _ => match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Event::Returned(result)) => Some(result),
            _ => None,
        },
    };
    // Record the result while the original owners still hold their reservations.
    let reserved_before_release = *SLOTS.in_use.lock();
    println!("wait={observed_wait} result={result:?} owner_slots={reserved_before_release}");
    cancel.store(true, Ordering::Relaxed);
    drop(extra);
    drop(owner);
    SLOTS.freed.notify_all();
    join_bounded(handle);
    assert!(
        observed_wait,
        "second want1 bypassed occupied process slots"
    );
    assert_eq!(reserved_before_release, capacity);
    assert!(
        matches!(result, Some(Err(Error::Timeout))) == deadline,
        "{result:?}"
    );
    if !deadline {
        assert!(matches!(result, Some(Err(Error::Cancelled))), "{result:?}");
    }
    assert_eq!(*SLOTS.in_use.lock(), 0);
    let next = Par::acquire(1, &Control::unlimited()).unwrap();
    assert_eq!(next.held, 1);
    assert!(next.pool.is_none());
    drop(next);
    assert_eq!(*SLOTS.in_use.lock(), 0);
}

#[test]
fn single_thread_waits_and_cancels_while_owner_held() {
    isolated("single_thread_waits_and_cancels_while_owner_held", || {
        saturation(1, 1, false, false)
    });
}

#[test]
fn single_thread_waits_and_times_out_while_owner_held() {
    isolated("single_thread_waits_and_times_out_while_owner_held", || {
        saturation(1, 1, true, false)
    });
}

#[test]
fn single_thread_respects_mixed_saturation() {
    isolated("single_thread_respects_mixed_saturation", || {
        saturation(3, 2, false, false)
    });
}

#[test]
fn service_single_thread_waits_for_process_admission() {
    isolated("service_single_thread_waits_for_process_admission", || {
        saturation(1, 1, false, true)
    });
}

#[test]
fn unavailable_controls_refuse_free_slots() {
    isolated("unavailable_controls_refuse_free_slots", || {
        configure(2, 2);
        for requested in [1, 2] {
            let cancelled = Control::new(Arc::new(AtomicBool::new(true)), None, u64::MAX);
            assert!(matches!(
                Par::acquire(requested, &cancelled),
                Err(Error::Cancelled)
            ));
            let expired = Control::new(
                Arc::new(AtomicBool::new(false)),
                Some(Instant::now()),
                u64::MAX,
            );
            assert!(matches!(
                Par::acquire(requested, &expired),
                Err(Error::Timeout)
            ));
            assert_eq!(*SLOTS.in_use.lock(), 0);
        }
    });
}

#[test]
fn cancelled_waiter_cannot_take_newly_freed_slots() {
    isolated("cancelled_waiter_cannot_take_newly_freed_slots", || {
        configure(2, 2);
        let owner = Par::acquire(2, &Control::unlimited()).unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let cancel = flag.clone();
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let observed = tx.clone();
            let mut sent = false;
            WAITING.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    if !sent {
                        observed.send(Event::Waiting).unwrap();
                        sent = true;
                    }
                }))
            });
            let on_wake = flag.clone();
            WOKEN.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |in_use| {
                    // Inject cancellation at the actual wake boundary only after the
                    // original owner's Drop has made the two requested slots free.
                    if in_use == 0 {
                        on_wake.store(true, Ordering::Relaxed);
                    }
                }))
            });
            let result = Par::acquire(2, &Control::new(flag, None, u64::MAX)).map(|p| p.threads());
            tx.send(Event::Returned(result)).unwrap();
        });
        let first = rx.recv_timeout(Duration::from_secs(5));
        if !matches!(first, Ok(Event::Waiting)) {
            cancel.store(true, Ordering::Relaxed);
        }
        drop(owner);
        SLOTS.freed.notify_all();
        let result = rx.recv_timeout(Duration::from_secs(5));
        join_bounded(handle);
        assert!(matches!(first, Ok(Event::Waiting)), "{first:?}");
        assert!(
            matches!(result, Ok(Event::Returned(Err(Error::Cancelled)))),
            "{result:?}"
        );
        assert_eq!(*SLOTS.in_use.lock(), 0);
    });
}

#[test]
fn explicit_native_single_remains_outside_admission() {
    isolated("explicit_native_single_remains_outside_admission", || {
        configure(1, 1);
        let owner = Par::acquire(1, &Control::unlimited()).unwrap();
        let before = *SLOTS.in_use.lock();
        for par in [Par::single(), Par::with_threads(1)] {
            assert_eq!(par.held, 0);
            assert!(par.pool.is_none());
        }
        assert_eq!(*SLOTS.in_use.lock(), before);
        drop(owner);
        assert_eq!(*SLOTS.in_use.lock(), 0);
    });
}

#[test]
fn failed_pool_builder_keeps_slots_until_drop() {
    isolated("failed_pool_builder_keeps_slots_until_drop", || {
        configure(2, 2);
        BUILD_MODE.with(|mode| mode.set(BuildMode::Error));
        let par = Par::acquire(2, &Control::unlimited()).unwrap();
        BUILD_MODE.with(|mode| mode.set(BuildMode::Normal));
        assert!(par.pool.is_none());
        assert_eq!(par.held, 2);
        assert_eq!(*SLOTS.in_use.lock(), 2);
        drop(par);
        assert_eq!(*SLOTS.in_use.lock(), 0);
        let next = Par::acquire(1, &Control::unlimited()).unwrap();
        drop(next);
        assert_eq!(*SLOTS.in_use.lock(), 0);
    });
}

#[test]
fn pool_builder_unwind_releases_slots() {
    isolated("pool_builder_unwind_releases_slots", || {
        configure(2, 2);
        BUILD_MODE.with(|mode| mode.set(BuildMode::Unwind));
        let result = std::panic::catch_unwind(|| Par::acquire(2, &Control::unlimited()));
        BUILD_MODE.with(|mode| mode.set(BuildMode::Normal));
        assert!(result.is_err());
        assert_eq!(
            *SLOTS.in_use.lock(),
            0,
            "builder unwind leaked the reservation"
        );
        let next = Par::acquire(1, &Control::unlimited()).unwrap();
        assert!(next.pool.is_none());
        drop(next);
        assert_eq!(*SLOTS.in_use.lock(), 0);
    });
}
