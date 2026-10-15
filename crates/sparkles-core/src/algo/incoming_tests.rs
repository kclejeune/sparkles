use super::*;
use crate::Error;
use crate::algo::memory::probe::{self, Point};
use crate::algo::testutil::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

fn weighted() -> Graph {
    Graph::build(
        RawEdges {
            nodes: vec![0, 1, 2],
            src: vec![0, 0, 2],
            dst: vec![1, 2, 1],
            w: Some(vec![2., 3., 4.]),
            symmetric: false,
        },
        Aggregate::None,
        &Par::single(),
        &Control::unlimited(),
    )
    .unwrap()
}

#[test]
fn r7_exact_incoming_capacity_and_cache_independent_call_charge() {
    for (g, retained, scratch) in [
        (weighted(), 56, 24),
        (digraph(3, &[(0, 1), (0, 2), (2, 1)]), 44, 24),
        (digraph(0, &[]), 8, 0),
    ] {
        let ctl = Control::new(Default::default(), None, retained + scratch);
        let observations = Rc::new(RefCell::new(Vec::new()));
        let seen = observations.clone();
        let c = ctl.clone();
        let _hook = probe::install(
            move |p| {
                if let Point::Allocation(name, cap) = p {
                    seen.borrow_mut().push((name, cap, c.remaining()));
                }
            },
            None,
        );
        let before = g.base_bytes().unwrap();
        let incoming = g.inn(&ctl).unwrap();
        assert_eq!(incoming.bytes(), retained);
        if g.n() > 0 {
            assert_eq!(incoming.row(1), &[0, 2]);
            if g.weighted() {
                assert_eq!(incoming.wrow(1), Some([2., 4.].as_slice()));
            }
        }
        assert_eq!(ctl.peak(), retained + scratch);
        assert_eq!(ctl.remaining(), scratch);
        assert_eq!(g.base_bytes().unwrap(), before);
        drop(incoming);
        assert_eq!(ctl.remaining(), retained + scratch);
        assert!(
            observations
                .borrow()
                .iter()
                .any(|(name, _, _)| *name == "incoming-offsets")
        );
        drop(_hook);
        let warm = Control::new(Default::default(), None, retained);
        let h = g.inn(&warm).unwrap();
        assert_eq!(warm.peak(), retained);
        assert_eq!(warm.remaining(), 0);
        drop(h);
        assert_eq!(warm.remaining(), retained);
        let short = Control::new(Default::default(), None, retained - 1);
        assert!(matches!(g.inn(&short), Err(Error::BudgetExceeded(_))));
        assert_eq!(short.remaining(), retained - 1);
    }
}

#[test]
fn r7_scratch_refuses_before_allocation_and_symmetric_reuses_out() {
    let g = weighted();
    let sym = ugraph(3, &[(0, 1)]);
    let ctl = Control::new(Default::default(), None, 56);
    let _hook = probe::install(
        |p| {
            if let Point::Allocation(..) = p {
                panic!("allocation after denied scratch");
            }
        },
        None,
    );
    assert!(matches!(g.inn(&ctl), Err(Error::BudgetExceeded(_))));
    assert_eq!(ctl.remaining(), 56);
    assert!(g.inn.get().is_none());
    let zero = Control::new(Default::default(), None, 0);
    assert!(std::ptr::eq(&*sym.inn(&zero).unwrap(), &sym.out));
    assert_eq!(zero.peak(), 0);
    let stopped = Control::new(Arc::new(AtomicBool::new(true)), None, 0);
    assert!(matches!(sym.inn(&stopped), Err(Error::Cancelled)));
}

#[test]
fn r7_initializer_allocation_errors_and_unwind_release_every_charge() {
    for name in [
        "incoming-cursors",
        "incoming-offsets",
        "incoming-edges",
        "incoming-weights",
    ] {
        let g = weighted();
        let ctl = Control::new(Default::default(), None, 80);
        {
            let _scope = probe::install(|_| {}, Some(name));
            assert!(matches!(g.inn(&ctl), Err(Error::Invalid(_))));
        }
        assert!(g.inn.get().is_none());
        assert_eq!(ctl.remaining(), 80);
        drop(g.inn(&ctl).unwrap());
        assert_eq!(ctl.remaining(), 80);
    }
    for point in ["allocation", "publication"] {
        let g = weighted();
        let ctl = Control::new(Default::default(), None, 80);
        {
            let _scope = probe::install(
                move |p| match (point, p) {
                    ("allocation", Point::Allocation("incoming-edges", _))
                    | ("publication", Point::Publishing) => panic!("test initializer unwind"),
                    _ => (),
                },
                None,
            );
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| g.inn(&ctl))).is_err()
            );
        }
        assert!(g.inn.get().is_none());
        assert_eq!(ctl.remaining(), 80);
        drop(g.inn(&ctl).unwrap());
        assert_eq!(ctl.remaining(), 80);
    }
}

#[test]
fn r7_transpose_checks_all_phases_and_bounds_high_degree_work() {
    for phase in [
        "incoming-zero-offsets",
        "incoming-count",
        "incoming-prefix",
        "incoming-cursors",
        "incoming-zero-edges",
        "incoming-zero-weights",
        "incoming-source",
        "incoming-scatter",
    ] {
        let g = weighted();
        let flag = Arc::new(AtomicBool::new(false));
        let ctl = Control::new(flag.clone(), None, 80);
        let f = flag.clone();
        let _scope = probe::install(
            move |p| {
                if let Point::Work(at) = p
                    && at == phase
                {
                    f.store(true, Ordering::Relaxed);
                }
            },
            None,
        );
        assert!(matches!(g.inn(&ctl), Err(Error::Cancelled)), "{phase}");
        assert!(flag.load(Ordering::Relaxed));
        assert!(g.inn.get().is_none());
        assert_eq!(ctl.remaining(), 80);
    }
    let edges = vec![(0, 1); 70_000];
    let g = digraph(2, &edges);
    let flag = Arc::new(AtomicBool::new(false));
    let ctl = Control::new(flag.clone(), None, u64::MAX);
    let count = Rc::new(Cell::new(0));
    let seen = count.clone();
    let _scope = probe::install(
        move |p| {
            if let Point::Work(at) = p
                && (at == "incoming-scatter" || flag.load(Ordering::Relaxed))
            {
                flag.store(true, Ordering::Relaxed);
                seen.set(seen.get() + 1);
            }
        },
        None,
    );
    assert!(matches!(g.inn(&ctl), Err(Error::Cancelled)));
    assert!(count.get() > 0 && count.get() <= crate::algo::CHECK_EVERY);
    assert_eq!(ctl.remaining(), u64::MAX);
    assert!(g.inn.get().is_none());
}

// An outer process deadline bounds a broken initializer as well as the test's own
// channel deadlines. No process-global pool/cache configuration is changed here.
fn isolated(body: impl FnOnce()) {
    const NAME: &str =
        "algo::graph::incoming_tests::r7_concurrent_initializer_ownership_and_wait_controls";
    if std::env::var_os("SPARKLES_R7_INCOMING_CHILD").is_some() {
        body();
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env("SPARKLES_R7_INCOMING_CHILD", "1")
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "initializer child {status}");
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("initializer child exceeded deadline");
        }
        std::thread::yield_now();
    }
}
struct Release(mpsc::Sender<()>);
impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[test]
fn r7_concurrent_initializer_ownership_and_wait_controls() {
    isolated(|| {
        for mode in [
            "low",
            "retention",
            "cancel",
            "deadline",
            "owner-cancel",
            "owner-error",
        ] {
            let g = Arc::new(weighted());
            let (ready_tx, ready_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let (wait_tx, wait_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            std::thread::scope(|scope| {
                let owner_g = g.clone();
                let owner_flag = Arc::new(AtomicBool::new(false));
                let owner_f = owner_flag.clone();
                let owner = scope.spawn(move || {
                    let _hook = probe::install(
                        move |p| {
                            let boundary =
                                matches!((mode, p), ("owner-error", Point::Initializing))
                                    || (mode != "owner-error" && matches!(p, Point::Publishing));
                            if boundary {
                                ready_tx.send(()).unwrap();
                                release_rx.recv_timeout(Duration::from_secs(8)).unwrap();
                            }
                        },
                        (mode == "owner-error").then_some("incoming-edges"),
                    );
                    let ctl =
                        Control::new(owner_f, Some(Instant::now() + Duration::from_secs(12)), 80);
                    let result = owner_g.inn(&ctl).map(|v| v.nnz());
                    (result, ctl.remaining())
                });
                let release = Release(release_tx);
                ready_rx.recv_timeout(Duration::from_secs(4)).unwrap();
                let flag = Arc::new(AtomicBool::new(false));
                let f = flag.clone();
                let waiter_g = g.clone();
                let waiter = scope.spawn(move || {
                    let seen = Cell::new(false);
                    let _hook = probe::install(
                        move |p| match p {
                            Point::Waiting if !seen.replace(true) => {
                                wait_tx.send(()).unwrap();
                            }
                            Point::Allocation(..) if !mode.starts_with("owner-") => {
                                panic!("waiter must not construct incoming")
                            }
                            _ => (),
                        },
                        None,
                    );
                    let deadline =
                        (mode == "deadline").then(|| Instant::now() + Duration::from_secs(2));
                    let limit = if mode == "low" {
                        55
                    } else if mode.starts_with("owner-") {
                        80
                    } else {
                        56
                    };
                    let ctl = Control::new(f, deadline, limit);
                    let result = waiter_g.inn(&ctl).map(|v| v.nnz());
                    let code = match result {
                        Ok(3) => "ok",
                        Err(Error::Cancelled) => "cancel",
                        Err(Error::Timeout) => "deadline",
                        Err(Error::BudgetExceeded(_)) => "low",
                        _ => "wrong",
                    };
                    done_tx.send((code, ctl.remaining(), ctl.peak())).unwrap();
                });
                if mode == "low" {
                    assert_eq!(
                        done_rx.recv_timeout(Duration::from_secs(4)).unwrap().0,
                        "low"
                    );
                } else {
                    wait_rx.recv_timeout(Duration::from_secs(4)).unwrap();
                    if mode == "cancel" {
                        flag.store(true, Ordering::Relaxed);
                    }
                    if mode != "retention" && !mode.starts_with("owner-") {
                        let (code, remaining, _) =
                            done_rx.recv_timeout(Duration::from_secs(4)).unwrap();
                        assert_eq!(code, mode);
                        assert_eq!(remaining, 56);
                        assert!(g.inn.get().is_none(), "owner still held before publish");
                    }
                }
                if mode == "owner-cancel" {
                    owner_flag.store(true, Ordering::Relaxed);
                }
                drop(release);
                let (owner_result, owner_remaining) = owner.join().unwrap();
                assert_eq!(owner_remaining, 80);
                match mode {
                    "owner-cancel" => assert!(matches!(owner_result, Err(Error::Cancelled))),
                    "owner-error" => assert!(matches!(owner_result, Err(Error::Invalid(_)))),
                    _ => assert_eq!(owner_result.unwrap(), 3),
                }
                if mode.starts_with("owner-") {
                    assert_eq!(
                        done_rx.recv_timeout(Duration::from_secs(4)).unwrap(),
                        ("ok", 80, 80)
                    );
                }
                if mode == "retention" {
                    assert_eq!(
                        done_rx.recv_timeout(Duration::from_secs(4)).unwrap(),
                        ("ok", 56, 56)
                    );
                }
                waiter.join().unwrap();
                assert!(g.inn.get().is_some());
                let low = Control::new(Default::default(), None, 55);
                assert!(matches!(g.inn(&low), Err(Error::BudgetExceeded(_))));
            });
        }
    });
}

#[test]
fn r7_checked_capacity_overflow_is_controlled() {
    assert!(memory::bytes::<u64>(usize::MAX).is_err());
    assert!(memory::sum(&[u64::MAX, 1]).is_err());
}

#[test]
fn r7_algorithm_invocations_hold_incoming_once_and_release_it() {
    let g = digraph(4, &[(0, 1), (1, 2), (2, 3)]);
    // Incoming52 plus five four-element f64 arrays and one partial = 220.
    let ctl = Control::new(Default::default(), None, 220);
    let output = crate::algo::pagerank::run(
        &g,
        &crate::algo::pagerank::Params {
            max_iterations: 5,
            fixed: true,
            ..Default::default()
        },
        &Par::single(),
        &ctl,
    )
    .unwrap();
    assert_eq!(ctl.peak(), 220);
    assert_eq!(ctl.remaining(), 220 - 32);
    drop(output);
    // Incoming is cached now; four source searches share its one call charge.
    let ctl = Control::new(Default::default(), None, 52 + 4 * 36);
    crate::algo::betweenness::run(&g, &Default::default(), &Par::single(), &ctl).unwrap();
    assert_eq!(ctl.peak(), 52 + 4 * 36);
    assert_eq!(ctl.remaining(), 52);
}
