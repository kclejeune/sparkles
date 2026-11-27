use super::*;
use sparkles_core::sparql::pool::QueryPool;
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn local_reasoner_error_retains_public_downcast_and_first_context() {
    let error = Budget::new(Some(100))
        .try_charge(101)
        .unwrap_err()
        .with_context(" first".into())
        .with_context(" second".into());
    let error = anyhow::Error::from(error).context("outer context");
    let local = error.downcast_ref::<crate::MemoryLimitExceeded>().unwrap();
    assert_eq!((local.limit, local.context.as_str()), (100, " first"));
    assert!(error.downcast_ref::<ParentRefused>().is_none());
}

#[test]
fn parent_error_retains_scope_and_first_context_through_anyhow() {
    for scope in [Scope::Local, Scope::Work, Scope::Retained] {
        let domain = sparkles_budget::MemoryDomain::new();
        let parent = domain.parent(
            move |requested| {
                Err(ParentRefused::Limit {
                    limit: 0,
                    requested,
                    scope,
                })
            },
            |_| {},
        );
        let b = Budget::with_parent(None, Some(parent));
        let error = b.try_charge(1).unwrap_err().with_context(" first".into());
        let error = anyhow::Error::from(error).context("outer context");
        assert!(error.downcast_ref::<crate::MemoryLimitExceeded>().is_none());
        assert_eq!(
            *error.downcast_ref::<ParentRefused>().unwrap(),
            ParentRefused::Limit {
                limit: 0,
                requested: 1,
                scope
            }
        );
        let sparkles_core::Error::BudgetExceeded(core) =
            error.downcast_ref::<sparkles_core::Error>().unwrap()
        else {
            panic!("wrong core error");
        };
        assert_eq!(
            core.kind,
            if scope == Scope::Local {
                sparkles_core::BudgetKind::Memory
            } else {
                sparkles_core::BudgetKind::ServerMemory
            }
        );
        assert_eq!((core.limit, core.requested), (0, 1));
        let typed = BudgetFailure::from_anyhow(&error)
            .unwrap()
            .with_context(" replacement".into());
        assert!(typed.to_string().ends_with(" first"));
        assert_eq!((b.used(), b.peak()), (0, 0));
    }
}

#[test]
fn ownership_misuse_never_becomes_a_capacity_refusal() {
    let domain = sparkles_budget::MemoryDomain::new();
    let parent = domain.parent(|_| Err(ParentRefused::ForeignDomain), |_| {});
    let b = Budget::with_parent(None, Some(parent));
    let error: anyhow::Error = b.try_charge(1).unwrap_err().into();
    assert_eq!(
        error.downcast_ref::<ParentRefused>(),
        Some(&ParentRefused::ForeignDomain)
    );
    assert!(error.downcast_ref::<sparkles_core::Error>().is_none());
    assert_eq!((b.used(), b.peak()), (0, 0));
}

#[test]
fn parent_rounding_exact_fallback_and_overflow_are_atomic() {
    let pool = QueryPool::new(100);
    let b = Budget::with_parent(None, Some(pool.work_parent()));
    b.try_charge(37).unwrap();
    assert_eq!((b.used(), pool.granted()), (37, 37));
    assert!(b.try_charge(64).is_err());
    assert_eq!((b.used(), b.peak(), pool.granted()), (37, 37, 37));
    b.release(37);
    drop(b);
    assert_eq!(pool.granted(), 0);

    let pool = QueryPool::new(u64::MAX);
    let b = Budget::with_parent(None, Some(pool.work_parent()));
    b.try_charge(usize::MAX).unwrap();
    assert!(matches!(
        b.try_charge(1),
        Err(BudgetFailure::Parent {
            refusal: ParentRefused::Overflow,
            ..
        })
    ));
    assert_eq!(b.used(), usize::MAX);
    b.release(usize::MAX);
    drop(b);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn reasoner_slow_growth_rollback_preserves_concurrent_contributions() {
    let pool = QueryPool::new(80);
    let parent = pool.work_parent();
    let barrier = std::sync::Barrier::new(3);
    std::thread::scope(|scope| {
        for _ in 0..2 {
            let parent = parent.clone();
            let barrier = &barrier;
            scope.spawn(move || {
                let b = Budget::with_parent(None, Some(parent));
                b.try_charge(40).unwrap();
                barrier.wait();
                assert!(b.try_charge(30).is_err());
                assert_eq!(b.used(), 40);
                barrier.wait();
                barrier.wait();
                b.release(40);
            });
        }
        barrier.wait();
        barrier.wait();
        assert_eq!(pool.granted(), 80);
        barrier.wait();
    });
    assert_eq!(pool.granted(), 0);
}

#[test]
fn reasoner_release_waker_reenters_without_a_resize_lock() {
    let holder = Arc::new(Mutex::new(std::sync::Weak::<Budget>::new()));
    let counter = Arc::new(AtomicU64::new(0));
    let callbacks = Arc::new(AtomicUsize::new(0));
    let take = counter.clone();
    let give = counter.clone();
    let callback = holder.clone();
    let called = callbacks.clone();
    let domain = sparkles_budget::MemoryDomain::new();
    let parent = domain.parent(
        move |n| {
            take.fetch_add(n, Ordering::SeqCst);
            Ok(())
        },
        move |n| {
            give.fetch_sub(n, Ordering::SeqCst);
            // Drop the holder mutex before reentering; this takes the Budget's actual
            // slow resize path after the trimmer published zero coverage.
            let budget = callback.lock().upgrade();
            if let Some(budget) = budget {
                budget.try_charge(1).unwrap();
                budget.release(1);
                called.fetch_add(1, Ordering::SeqCst);
            }
        },
    );
    let b = Budget::with_parent(None, Some(parent));
    *holder.lock() = Arc::downgrade(&b);
    b.try_charge(4 << 20).unwrap();
    b.release(4 << 20);
    assert_eq!(b.used(), 0);
    assert!(callbacks.load(Ordering::SeqCst) > 0);
    drop(b);
    assert_eq!(counter.load(Ordering::SeqCst), 0);
}

#[test]
fn charges_inside_parent_credit_do_not_invoke_the_backend() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let domain = sparkles_budget::MemoryDomain::new();
    let parent = domain.parent(
        move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
        |_| {},
    );
    let b = Budget::with_parent(None, Some(parent));
    for _ in 0..1024 {
        b.try_charge(1).unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    b.release(1024);
}

#[test]
fn reasoner_shrinks_whole_chunks_and_returns_its_final_reservation() {
    let step = PARENT_STEP;
    let pool = QueryPool::new((4 * PARENT_SLACK) as u64);
    let parent = pool.work_parent();
    let sibling = parent.reserve(17).unwrap();
    let budget = Budget::with_parent(None, Some(parent));

    budget.try_charge(step - 1).unwrap();
    assert_eq!(pool.granted(), sibling.bytes() + step as u64);
    budget.try_charge(1).unwrap();
    assert_eq!(pool.granted(), sibling.bytes() + step as u64);
    budget.try_charge(1).unwrap();
    assert_eq!(pool.granted(), sibling.bytes() + (2 * step) as u64);

    budget.try_charge(2 * PARENT_SLACK).unwrap();
    assert_eq!(
        pool.granted(),
        sibling.bytes() + (2 * PARENT_SLACK + 2 * step) as u64
    );
    budget.release(2 * PARENT_SLACK);
    assert_eq!(pool.granted(), sibling.bytes() + (2 * step) as u64);
    budget.release(step + 1);
    drop(budget);
    assert_eq!(pool.granted(), sibling.bytes());
    drop(sibling);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn sibling_worker_releases_defer_query_wakes_into_the_run() {
    use sparkles_core::sparql::pool::GRANULE;
    use std::sync::atomic::AtomicUsize;
    #[derive(Clone, Copy, Debug)]
    enum Release {
        Trim,
        TrimUnderAnotherScope,
        LastOwner,
    }
    let workers = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    for case in [
        Release::Trim,
        Release::TrimUnderAnotherScope,
        Release::LastOwner,
    ] {
        let pool = QueryPool::new(2 * GRANULE);
        let calls = Arc::new(AtomicUsize::new(0));
        let released = AtomicUsize::new(0);
        // The waiter stays queued until the run's drain wakes it.
        let _ticket = pool.enter(
            GRANULE,
            Some({
                let calls = calls.clone();
                Arc::new(move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                })
            }),
        );
        QueryPool::with_deferred_release_wakes(|| {
            let budget = Budget::with_parent(None, Some(pool.work_parent()));
            let bytes = usize::try_from(2 * GRANULE).unwrap();
            budget.try_charge(bytes).unwrap();
            let owner = Mutex::new(Some(budget));
            workers.install(QueryPool::inherit_release_wakes(|| {
                let root = rayon::current_thread_index().unwrap();
                // a parallel task on the sibling worker, which inherits nothing
                rayon::broadcast(|ctx| {
                    if ctx.index() == root {
                        return;
                    }
                    assert!(QueryPool::capture_release_wakes().is_none());
                    let release = || match case {
                        Release::LastOwner => drop(owner.lock().take()),
                        _ => owner.lock().as_ref().unwrap().release(bytes),
                    };
                    match case {
                        Release::TrimUnderAnotherScope => {
                            QueryPool::with_deferred_release_wakes(release)
                        }
                        _ => release(),
                    }
                    released.fetch_add(1, Ordering::SeqCst);
                });
            }));
            assert_eq!(released.load(Ordering::SeqCst), 1, "{case:?}");
            assert_eq!(pool.granted(), 0, "{case:?}");
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "{case:?} woke inside the run"
            );
            drop(owner);
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{case:?} lost its wake");
    }
}

/// A cache's budget is published by one run and evicted by another, which may hold
/// its writer after the publishing run has finished. The eviction's query wake waits
/// for the evicting run's scope, whichever scope closes first.
#[test]
fn a_retained_budget_defers_its_release_into_the_releasing_scope() {
    use sparkles_core::sparql::pool::GRANULE;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc::channel;
    let pool = QueryPool::new(2 * GRANULE);
    let calls = Arc::new(AtomicUsize::new(0));
    let _ticket = pool.enter(
        GRANULE,
        Some({
            let calls = calls.clone();
            Arc::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
            })
        }),
    );
    let (budget_tx, budget_rx) = channel();
    let (dropped_tx, dropped_rx) = channel();
    let (closed_tx, closed_rx) = channel();
    std::thread::scope(|s| {
        s.spawn({
            let calls = calls.clone();
            move || {
                let budget: Arc<Budget> = budget_rx.recv().unwrap();
                QueryPool::with_deferred_release_wakes(|| {
                    // the evicting run drops the cached closure under its writer
                    drop(budget);
                    dropped_tx.send(()).unwrap();
                    closed_rx.recv().unwrap();
                    assert_eq!(
                        calls.load(Ordering::SeqCst),
                        0,
                        "the publishing run woke the waiter inside the evicting run"
                    );
                });
                assert_eq!(calls.load(Ordering::SeqCst), 1, "the wake was lost");
            }
        });
        QueryPool::with_deferred_release_wakes(|| {
            let budget = Budget::retained(None, Some(pool.work_parent()));
            budget
                .try_charge(usize::try_from(2 * GRANULE).unwrap())
                .unwrap();
            budget_tx.send(budget).unwrap();
            dropped_rx.recv().unwrap();
        });
        closed_tx.send(()).unwrap();
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
