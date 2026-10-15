use super::*;

fn args() -> Value {
    json!({"request":{"algorithm":"node2vec","projection":{"predicates":["urn:p"],"direction":"undirected"},
        "params":{"dimension":1,"walkLength":70000,"walksPerNode":1,"negativeSamples":0,"epochs":1,"concurrency":1}}})
}

fn dataset() -> (Dataset, Arc<sparkles::sparql::pool::QueryPool>) {
    let pool = sparkles::sparql::pool::QueryPool::new(64 << 20);
    let ds = Dataset::from_store_with(
        sparkles::store::Store::in_memory(Default::default()),
        sparkles::DatasetOptions {
            pool: Some(pool.clone()),
            ..Default::default()
        },
    );
    ds.update("INSERT DATA { <urn:a> <urn:p> <urn:b> }")
        .unwrap();
    (ds, pool)
}

#[test]
fn algo_stats_success_uses_dataset_catalog_and_pool() {
    let (ds, pool) = dataset();
    let definition = sparkles::algo::catalog::ProjectionDef::from_json(
        json!({"predicates":["urn:p"],"direction":"forward"}),
    )
    .unwrap();
    ds.algorithms()
        .projections()
        .put("chosen", definition, None, None)
        .unwrap();
    let request =
        json!({"request":{"algorithm":"wcc","projection":"chosen","params":{"concurrency":1}}});
    let result = run_admin(
        &ds,
        "algo.stats",
        &request,
        &json!({}),
        Arc::new(AtomicBool::new(false)),
        &sparkles::task::Control::none(),
    )
    .unwrap();
    assert_eq!(result["nodes"], 2);
    assert_eq!(result["components"], 1);
    assert_eq!(pool.granted(), 0);
    let denied_pool = sparkles::sparql::pool::QueryPool::new(0);
    let denied_store = sparkles::store::Store::in_memory(Default::default());
    sparkles::sparql::update::update(
        &denied_store,
        "INSERT DATA { <urn:a> <urn:p> <urn:b> }",
        &Default::default(),
    )
    .unwrap();
    let denied = Dataset::from_store_with(
        denied_store,
        sparkles::DatasetOptions {
            pool: Some(denied_pool.clone()),
            ..Default::default()
        },
    );
    let inline = json!({"request":{"algorithm":"wcc","projection":{"predicates":["urn:p"]},"params":{"concurrency":1}}});
    let result = run_admin(
        &denied,
        "algo.stats",
        &inline,
        &json!({}),
        Arc::new(AtomicBool::new(false)),
        &sparkles::task::Control::none(),
    );
    assert!(
        matches!(result,Err(EngineError::BudgetExceeded(b)) if b.kind == sparkles::error::BudgetKind::ServerMemory)
    );
    assert_eq!(denied_pool.granted(), 0);
    ds.create_branch("stats-view", &Default::default()).unwrap();
    let branch = ds.branch("stats-view").unwrap();
    branch
        .update("INSERT DATA { <urn:b> <urn:p> <urn:c> }")
        .unwrap();
    let result = run_admin(
        &branch,
        "algo.stats",
        &inline,
        &json!({}),
        Arc::new(AtomicBool::new(false)),
        &sparkles::task::Control::none(),
    )
    .unwrap();
    assert_eq!(result["nodes"], 3);
    let result = run_admin(
        &ds,
        "algo.stats",
        &inline,
        &json!({}),
        Arc::new(AtomicBool::new(false)),
        &sparkles::task::Control::none(),
    )
    .unwrap();
    assert_eq!(result["nodes"], 2);
    let empty = json!({"request":{"algorithm":"wcc","projection":{"predicates":["urn:absent"]}}});
    let result = run_admin(
        &ds,
        "algo.stats",
        &empty,
        &json!({}),
        Arc::new(AtomicBool::new(false)),
        &sparkles::task::Control::none(),
    )
    .unwrap();
    assert_eq!(result["nodes"], 0);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn algo_stats_precancelled_flag_is_rejected() {
    let (ds, pool) = dataset();
    let flag = Arc::new(AtomicBool::new(true));
    let ctl = sparkles::task::Control::with_cancel(flag.clone());
    let result = run_admin(&ds, "algo.stats", &args(), &json!({}), flag, &ctl);
    assert!(matches!(result, Err(EngineError::Cancelled)), "{result:?}");
    assert_eq!(pool.granted(), 0);
}

#[cfg(feature = "failpoints")]
mod observed {
    use super::*;
    use sparkles::algo::{CheckOutcome, Control, Par, pool, set_check_observer};
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    struct Held {
        release: Arc<Mutex<Option<mpsc::Sender<()>>>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Held {
        fn drop(&mut self) {
            set_check_observer(None);
            if let Some(tx) = self.release.lock().take() {
                let _ = tx.send(());
            }
            if let Some(h) = self.thread.take() {
                h.join().expect("holder panicked");
            }
        }
    }

    fn scenario(slot: bool, timeout: bool) {
        pool::configure(1, 1);
        let (ds, memory) = dataset();
        let flag = Arc::new(AtomicBool::new(false));
        let deadline = timeout.then(|| Instant::now() + Duration::from_millis(150));
        let ctl = sparkles::task::Control {
            deadline,
            ..sparkles::task::Control::with_cancel(flag.clone())
        };
        let release = Arc::new(Mutex::new(None::<mpsc::Sender<()>>));
        let holder = if slot {
            let (tx, rx) = mpsc::channel();
            let (ready_tx, ready_rx) = mpsc::channel();
            *release.lock() = Some(tx);
            let h = std::thread::spawn(move || {
                let held = Par::acquire(
                    1,
                    &Control::new(
                        Arc::new(AtomicBool::new(false)),
                        Some(Instant::now() + Duration::from_secs(2)),
                        u64::MAX,
                    ),
                )
                .unwrap();
                ready_tx.send(()).unwrap();
                let _ = rx.recv_timeout(Duration::from_secs(5));
                drop(held);
            });
            if ready_rx.recv_timeout(Duration::from_secs(3)).is_err() {
                if let Some(tx) = release.lock().take() {
                    let _ = tx.send(());
                }
                let _ = h.join();
                panic!("slot holder did not enter");
            }
            Some(h)
        } else {
            None
        };
        let held = Held {
            release: release.clone(),
            thread: holder,
        };
        let entered = Arc::new(AtomicBool::new(false));
        let hit = Arc::new(AtomicBool::new(false));
        let outcome = Arc::new(Mutex::new(None));
        let early = Arc::new(AtomicBool::new(false));
        let checks = AtomicUsize::new(0);
        let (e, h, o, r, f, early_c) = (
            entered.clone(),
            hit.clone(),
            outcome.clone(),
            release.clone(),
            flag.clone(),
            early.clone(),
        );
        set_check_observer(Some(Arc::new(move |file, _, event, _| {
            let node = file.ends_with("/algo/node2vec.rs");
            if node && event == CheckOutcome::Passed {
                e.store(true, Ordering::SeqCst);
            }
            let boundary = if slot {
                file.ends_with("/algo/pool.rs")
                    && event == CheckOutcome::Before
                    && checks.fetch_add(1, Ordering::SeqCst) == 2
            } else {
                node && event == CheckOutcome::Before && e.load(Ordering::SeqCst)
            };
            if boundary && !h.swap(true, Ordering::SeqCst) {
                if let Some(d) = deadline {
                    assert!(
                        Instant::now() < d,
                        "deadline expired before actual boundary"
                    );
                    while Instant::now() < d {
                        std::thread::park_timeout(d.saturating_duration_since(Instant::now()));
                    }
                } else {
                    f.store(true, Ordering::SeqCst);
                }
            }
            if h.load(Ordering::SeqCst) && event != CheckOutcome::Before {
                let mut seen = o.lock();
                if seen.is_none() {
                    *seen = Some(event);
                }
                if slot
                    && event == CheckOutcome::Passed
                    && let Some(tx) = r.lock().take()
                {
                    early_c.store(true, Ordering::SeqCst);
                    let _ = tx.send(());
                }
            }
        })));
        let result = run_admin(&ds, "algo.stats", &args(), &json!({}), flag, &ctl);
        drop(held);
        assert!(hit.load(Ordering::SeqCst), "did not reach actual work/wait");
        let expected = if timeout {
            CheckOutcome::Timeout
        } else {
            CheckOutcome::Cancelled
        };
        assert_eq!(
            *outcome.lock(),
            Some(expected),
            "actual native check ignored task control; result={result:?}"
        );
        assert!(
            if timeout {
                matches!(result, Err(EngineError::Timeout))
            } else {
                matches!(result, Err(EngineError::Cancelled))
            },
            "{result:?}"
        );
        assert!(
            !early.load(Ordering::SeqCst),
            "native call required holder release"
        );
        assert_eq!(entered.load(Ordering::SeqCst), !slot);
        assert_eq!(memory.granted(), 0);
        let reusable = Par::acquire(
            1,
            &Control::new(
                Arc::new(AtomicBool::new(false)),
                Some(Instant::now() + Duration::from_secs(1)),
                u64::MAX,
            ),
        )
        .unwrap();
        drop(reusable);
    }

    #[test]
    fn algo_stats_child() {
        if let Ok(mode) = std::env::var("SPARKLES_STATS_UNIT_CHILD") {
            scenario(mode.starts_with("slot"), mode.ends_with("timeout"));
        }
    }
    fn child(mode: &str) {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                if self.0.try_wait().ok().flatten().is_none() {
                    let _ = self.0.kill();
                }
                let _ = self.0.wait();
            }
        }
        let mut child = Child(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "admin::algo_stats_tests::observed::algo_stats_child",
                    "--nocapture",
                ])
                .env("SPARKLES_STATS_UNIT_CHILD", mode)
                .spawn()
                .unwrap(),
        );
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success(), "child {mode}: {status}");
                break;
            }
            assert!(Instant::now() < end, "child {mode} exceeded deadline");
            std::thread::park_timeout(Duration::from_millis(5));
        }
    }
    #[test]
    fn algo_stats_cancel_during_native_work() {
        child("compute-cancel");
    }
    #[test]
    fn algo_stats_deadline_during_native_work() {
        child("compute-timeout");
    }
    #[test]
    fn algo_stats_cancel_waiting_for_slot() {
        child("slot-cancel");
    }
    #[test]
    fn algo_stats_deadline_waiting_for_slot() {
        child("slot-timeout");
    }
}
