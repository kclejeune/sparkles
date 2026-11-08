use super::*;
use crate::access::{Caller, GraphRule, Graphs, Limits, Protection, Rule, TripleRules};
use crate::sparql::pool::QueryPool;
use crate::sparql::{CursorOptions, QueryOptions, select_source_cursor};
use crate::task::Control;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn preparation() -> Preparation {
    Preparation::new(Control::none(), 32 << 20).unwrap()
}
fn update(store: &super::super::Store, text: &str) {
    crate::sparql::update::update(store, text, &QueryOptions::default()).unwrap();
}
fn options(policy: Arc<GraphAccess>) -> QueryOptions {
    QueryOptions {
        graphs: Some(policy),
        no_cache: true,
        ..Default::default()
    }
}
fn rows(
    source: BoundReadSource,
    store: &super::super::Store,
    policy: Arc<GraphAccess>,
    text: &str,
) -> Vec<String> {
    let mut cursor = select_source_cursor(
        source,
        store,
        text,
        &options(policy),
        &CursorOptions::default(),
    )
    .unwrap();
    let mut out = Vec::new();
    while let Some(batch) = cursor.next_batch().unwrap() {
        for row in 0..batch.len() {
            out.push(batch.term(row, 0).unwrap().unwrap().as_term().to_string());
        }
    }
    out
}
fn hiding_policy() -> Arc<GraphAccess> {
    Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::none(),
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "secret".into(),
                    predicates: Some(vec!["urn:secret".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    ))
}

#[test]
fn strict_origin_and_original_policy_refuse_before_invalid_query_parse() {
    let store = super::super::Store::in_memory(Default::default());
    let foreign = super::super::Store::in_memory(Default::default());
    let prep = preparation();
    let policy = Arc::new(GraphAccess::all());
    let legacy = policy.masked_controlled(&store.snapshot(), &prep).unwrap();
    let opts = options(policy.clone());
    assert!(
        matches!(select_source_cursor(legacy.read_source(), &store, "INVALID", &opts, &Default::default()),
        Err(Error::Invalid(message)) if message.contains("Store-minted"))
    );
    let view = store.prepare_read(policy.clone(), &prep).unwrap();
    assert!(
        matches!(select_source_cursor(view.read_source(), &foreign, "INVALID", &opts, &Default::default()),
        Err(Error::Invalid(message)) if message.contains("another Store instance"))
    );
    assert!(
        matches!(select_source_cursor(view.read_source(), &store, "INVALID", &QueryOptions::default(), &Default::default()),
        Err(Error::NotPermitted(message)) if message.contains("current policy"))
    );
    let replacement = options(Arc::new(GraphAccess::all()));
    assert!(
        matches!(select_source_cursor(view.read_source(), &store, "INVALID", &replacement, &Default::default()),
        Err(Error::NotPermitted(message)) if message.contains("replace"))
    );
}

#[test]
fn strict_reopened_same_uuid_is_a_foreign_runtime_before_parse() {
    let dir = tempfile::tempdir().unwrap();
    let prep = preparation();
    let policy = Arc::new(GraphAccess::all());
    let source = {
        let store = super::super::Store::open(dir.path(), Default::default()).unwrap();
        store
            .prepare_read(policy.clone(), &prep)
            .unwrap()
            .read_source()
    };
    let reopened = super::super::Store::open(dir.path(), Default::default()).unwrap();
    assert!(
        matches!(select_source_cursor(source, &reopened, "INVALID", &options(policy), &Default::default()),
        Err(Error::Invalid(message)) if message.contains("another Store instance"))
    );
}

#[test]
fn ordinary_unbound_retained_commit_name_and_time_remain_supported() {
    let dir = tempfile::tempdir().unwrap();
    let store = super::super::Store::open(dir.path(), Default::default()).unwrap();
    // Commit times are clamped to be monotonic, so the fixture clock starts
    // after the creation commit and separates the two writes.
    let base = store.head_commit().timestamp_ms + 100_000;
    let clock = Arc::new(std::sync::atomic::AtomicI64::new(base));
    let observed = clock.clone();
    store.set_clock(Arc::new(move || observed.load(Ordering::Relaxed)));
    update(&store, "INSERT DATA { <urn:first> <urn:p> 1 }");
    let first = store.head_commit();
    store.create_snapshot("legacy", &At::Head, None).unwrap();
    clock.store(base + 1_000, Ordering::Relaxed);
    update(&store, "INSERT DATA { <urn:second> <urn:p> 2 }");
    let prep = preparation();
    assert!(store.evidence_view(&At::Commit(first.seq), &prep).is_err());
    let policy = Arc::new(GraphAccess::all());
    for at in [
        At::Commit(first.seq),
        At::Snapshot("legacy".into()),
        At::Time(first.timestamp_ms),
    ] {
        let view = store.prepare_read_at(&at, policy.clone(), &prep).unwrap();
        assert_eq!(
            rows(
                view.read_source(),
                &store,
                policy.clone(),
                "SELECT ?s WHERE { ?s <urn:p> ?o }"
            ),
            ["<urn:first>"],
            "{at:?}"
        );
    }
}

#[test]
fn inherited_physical_state_keeps_logical_branch_and_actual_main() {
    let dir = tempfile::tempdir().unwrap();
    let main = super::super::Store::open(dir.path(), Default::default()).unwrap();
    update(&main, "INSERT DATA { <urn:ancestor> <urn:p> 1 }");
    let at = At::Commit(main.head_commit().seq);
    main.create_branch("dev", &Default::default()).unwrap();
    let branch = main
        .owned_set()
        .unwrap()
        .open_store(main.branch_id_of("dev").unwrap())
        .unwrap();
    for n in 0..3 {
        update(
            &branch,
            &format!("INSERT DATA {{ <urn:branch{n}> <urn:p> {n} }}"),
        );
        branch.compact().unwrap();
    }
    let prep = preparation();
    let policy = Arc::new(GraphAccess::all());
    let view = main
        .prepare_read_with_branch(&branch, &at, policy.clone(), &prep)
        .unwrap();
    assert_eq!(
        rows(
            view.read_source(),
            &branch,
            policy.clone(),
            "SELECT ?s WHERE { ?s <urn:p> ?o }"
        ),
        ["<urn:ancestor>"]
    );
    assert!(
        matches!(select_source_cursor(view.read_source(), &main, "INVALID", &options(policy.clone()), &Default::default()),
        Err(Error::Invalid(message)) if message.contains("another Store instance"))
    );
    let foreign = super::super::Store::in_memory(Default::default());
    assert!(matches!(
        foreign.prepare_read_with_branch(&branch, &at, policy, &prep),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn renamed_branch_retains_origin_and_recreated_branch_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let main = super::super::Store::open(dir.path(), Default::default()).unwrap();
    main.create_branch("dev", &Default::default()).unwrap();
    let old = main
        .owned_set()
        .unwrap()
        .open_store(main.branch_id_of("dev").unwrap())
        .unwrap();
    update(&old, "INSERT DATA { <urn:branch> <urn:p> 1 }");
    let prep = preparation();
    let policy = Arc::new(GraphAccess::all());
    let view = main
        .prepare_read_with_branch(&old, &At::Head, policy.clone(), &prep)
        .unwrap();
    main.rename_branch("dev", "renamed").unwrap();
    assert_eq!(
        rows(
            view.read_source(),
            &old,
            policy.clone(),
            "SELECT ?s WHERE { ?s <urn:p> ?o }"
        ),
        ["<urn:branch>"]
    );
    main.create_branch("dev", &Default::default()).unwrap();
    let recreated = main.branch("dev").unwrap();
    assert!(
        matches!(select_source_cursor(view.read_source(), &recreated, "INVALID", &options(policy), &Default::default()),
        Err(Error::Invalid(message)) if message.contains("another Store instance"))
    );
}

#[test]
fn strict_graph_and_reads_all_triple_policies_stay_hidden() {
    let store = super::super::Store::in_memory(Default::default());
    update(
        &store,
        "INSERT DATA { <urn:s> <urn:secret> 1 ; <urn:visible> 2 . GRAPH <urn:allowed> { <urn:a> <urn:p> 1 } GRAPH <urn:hidden> { <urn:h> <urn:p> 2 } }",
    );
    let prep = preparation();
    let policy = hiding_policy();
    assert!(policy.reads_all());
    let view = store.prepare_read(policy.clone(), &prep).unwrap();
    assert_eq!(
        rows(
            view.read_source(),
            &store,
            policy,
            "SELECT ?p WHERE { <urn:s> ?p ?o }"
        ),
        ["<urn:visible>"]
    );
    let policy = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:allowed"], &[])),
        Graphs::none(),
    ));
    let view = store.prepare_read(policy.clone(), &prep).unwrap();
    assert_eq!(
        rows(
            view.read_source(),
            &store,
            policy,
            "SELECT ?s WHERE { GRAPH ?g { ?s <urn:p> ?o } }"
        ),
        ["<urn:a>"]
    );
}

#[test]
fn strict_grant_and_pool_replacements_refuse_before_parse_without_charge() {
    let store = super::super::Store::in_memory(Default::default());
    let pool = QueryPool::new(64 << 20);
    let grant = Arc::new(pool.empty_grant());
    let prep = Preparation::with_grant(Control::none(), 16 << 20, grant.clone()).unwrap();
    let policy = Arc::new(GraphAccess::all());
    let view = store.prepare_read(policy.clone(), &prep).unwrap();
    let before = grant.used();
    let other = Arc::new(pool.empty_grant());
    let mut opts = options(policy);
    opts.grant = Some(other.clone());
    assert!(
        matches!(select_source_cursor(view.read_source(), &store, "INVALID", &opts, &Default::default()),
        Err(Error::Component(error)) if error.code == "foreign-grant")
    );
    assert_eq!(grant.used(), before);
    assert_eq!(other.used(), 0);
    opts.grant = None;
    opts.pool = Some(QueryPool::new(64 << 20));
    assert!(
        matches!(select_source_cursor(view.read_source(), &store, "INVALID", &opts, &Default::default()),
        Err(Error::Component(error)) if error.code == "foreign-domain")
    );
    assert_eq!(grant.used(), before);
}

#[test]
fn selected_source_and_cells_keep_original_credit_after_producer_drop() {
    let store = super::super::Store::in_memory(Default::default());
    update(&store, "INSERT DATA { <urn:retained> <urn:p> 1 }");
    let pool = QueryPool::new(64 << 20);
    let grant = Arc::new(pool.empty_grant());
    let prep = Preparation::with_grant(Control::none(), 16 << 20, grant.clone()).unwrap();
    let policy = Arc::new(GraphAccess::all());
    let view = store.prepare_read(policy.clone(), &prep).unwrap();
    let mut cursor = select_source_cursor(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> 1 }",
        &options(policy),
        &Default::default(),
    )
    .unwrap();
    let batch = cursor.next_batch().unwrap().unwrap();
    let cell = batch.term(0, 0).unwrap().unwrap();
    let clone = cell.clone();
    drop(view);
    drop(store);
    drop(cursor);
    drop(batch);
    drop(prep);
    drop(cell);
    assert!(grant.used() > 0);
    assert_eq!(clone.as_term().to_string(), "<urn:retained>");
    drop(clone);
    assert_eq!(grant.used(), 0);
    // This is payload/credit lifetime, not a surviving-Weak header proof.
}

#[test]
fn history_read_no_wait_checks_real_mutex_and_read_lock() {
    let store = super::super::Store::in_memory(Default::default());
    let prep = preparation().with_no_wait(true);
    let baseline = prep.inner_used();
    let held = store.writer.lock();
    assert!(
        matches!(store.prepare_read(Arc::new(GraphAccess::all()), &prep), Err(Error::Invalid(message)) if message.contains("no_wait"))
    );
    drop(held);
    assert_eq!(prep.inner_used(), baseline);
    let limit = ReadLimit {
        options: &prep,
        memory: Some(1 << 20),
        held: 0,
    };
    let lock = parking_lot::RwLock::new(());
    let held = lock.write();
    assert!(
        matches!(limit.read_lock(&lock), Err(Error::Invalid(message)) if message.contains("no_wait"))
    );
    drop(held);
}

#[test]
fn original_and_composed_work_stop_controls_reach_real_history_lock_waits() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(super::super::Store::open(dir.path(), Default::default()).unwrap());
    update(&store, "INSERT DATA { <urn:first> <urn:p> 1 }");
    update(&store, "INSERT DATA { <urn:second> <urn:p> 2 }");
    for composed in [false, true] {
        let cancel = Arc::new(AtomicBool::new(false));
        let root = Preparation::new(Control::none(), 32 << 20).unwrap();
        let prep = if composed {
            root.checked_for_query(&QueryOptions {
                cancellation: Some(crate::task::Cancellation::from_flags([cancel.clone()])),
                ..Default::default()
            })
            .unwrap()
        } else {
            root.checked_for_query(&QueryOptions {
                cancel: Some(cancel.clone()),
                ..Default::default()
            })
            .unwrap()
        };
        let snapshot = store.snapshot();
        let held = snapshot.generation.dvocab.write_guard_for_test();
        let (send, recv) = std::sync::mpsc::channel();
        let reader = store.clone();
        let task = std::thread::spawn(move || {
            send.send(reader.prepare_read_at(&At::Commit(1), Arc::new(GraphAccess::all()), &prep))
                .unwrap()
        });
        std::thread::sleep(Duration::from_millis(30));
        cancel.store(true, Ordering::Relaxed);
        let result = recv.recv_timeout(Duration::from_secs(2));
        drop(held);
        task.join().unwrap();
        assert!(matches!(result, Ok(Err(Error::Cancelled))));
    }
}

#[test]
fn earliest_absolute_history_deadline_is_not_renewed() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(super::super::Store::open(dir.path(), Default::default()).unwrap());
    update(&store, "INSERT DATA { <urn:first> <urn:p> 1 }");
    update(&store, "INSERT DATA { <urn:second> <urn:p> 2 }");
    let root = preparation();
    let prep = root
        .checked_for_query(&QueryOptions {
            deadline: Some(Instant::now() + Duration::from_millis(60)),
            ..Default::default()
        })
        .unwrap();
    let snapshot = store.snapshot();
    let held = snapshot.generation.dvocab.write_guard_for_test();
    let (send, recv) = std::sync::mpsc::channel();
    let reader = store.clone();
    let task = std::thread::spawn(move || {
        send.send(reader.prepare_read_at(&At::Commit(1), Arc::new(GraphAccess::all()), &prep))
            .unwrap()
    });
    let result = recv.recv_timeout(Duration::from_secs(2));
    drop(held);
    task.join().unwrap();
    assert!(matches!(result, Ok(Err(Error::Timeout))));
}

#[test]
fn selection_failure_and_pool_sibling_loan_refund_without_fallback() {
    let store = super::super::Store::in_memory(Default::default());
    let pool = QueryPool::new(16 << 20);
    let grant = Arc::new(pool.empty_grant());
    let prep = Preparation::with_grant(Control::none(), 32 << 20, grant.clone()).unwrap();
    let other = pool.try_grant(12 << 20).unwrap();
    let baseline = grant.used();
    assert!(prep.selection_room() < prep.room());
    let error = store
        .prepare_read_at(
            &At::Snapshot("missing".into()),
            Arc::new(GraphAccess::all()),
            &prep,
        )
        .err()
        .unwrap();
    assert!(matches!(error, Error::NotFound(message) if message == "no snapshot 'missing'"));
    assert_eq!(grant.used(), baseline);
    let held = prep.reserve(prep.selection_room()).unwrap();
    let before = grant.used();
    assert!(matches!(
        store.prepare_read(Arc::new(GraphAccess::all()), &prep),
        Err(Error::BudgetExceeded(_))
    ));
    assert_eq!(grant.used(), before);
    drop(held);
    drop(other);
    drop(prep);
    assert_eq!(grant.used(), 0);
}

#[test]
fn data_dependent_guard_binds_once_on_the_original_before_hiding_its_evidence() {
    struct OriginalGuard(std::sync::atomic::AtomicUsize);
    impl crate::access::GuardDecision for OriginalGuard {
        fn exempt(&self, _: &Arc<super::super::Snapshot>) -> Vec<String> {
            panic!("strict factory must not call legacy uncontrolled provider")
        }
        fn exempt_controlled(
            &self,
            snap: &Arc<super::super::Snapshot>,
            prep: &Preparation,
        ) -> Result<crate::access::Exemptions> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let mut allowed = crate::access::Exemptions::new(prep)?;
            if let Some(predicate) = prep.lookup_iri(snap, "urn:secret")? {
                let mut found = false;
                prep.scan(snap, crate::index::Perm::Pso, &[predicate.0], |_| {
                    found = true;
                    Ok(false)
                })?;
                if found {
                    allowed.push("urn:guarded:allowed")?;
                }
            }
            Ok(allowed)
        }
    }
    let store = super::super::Store::in_memory(Default::default());
    update(
        &store,
        "INSERT DATA { <urn:s> <urn:secret> 1 . GRAPH <urn:guarded:allowed> { <urn:a> <urn:p> 1 } }",
    );
    let guard = Arc::new(OriginalGuard(std::sync::atomic::AtomicUsize::new(0)));
    let mut policy = (*hiding_policy()).clone();
    policy.read = Graphs::All.guarded_by("urn:guarded:*", crate::access::Decider(guard.clone()));
    let policy = Arc::new(policy);
    let prep = preparation();
    let view = store.prepare_read(policy.clone(), &prep).unwrap();
    assert_eq!(
        rows(
            view.read_source(),
            &store,
            policy.clone(),
            "SELECT ?s WHERE { GRAPH ?g { ?s <urn:p> ?o } }"
        ),
        ["<urn:a>"]
    );
    assert!(
        rows(
            view.read_source(),
            &store,
            policy,
            "SELECT ?s WHERE { ?s <urn:secret> ?o }"
        )
        .is_empty()
    );
    assert_eq!(guard.0.load(Ordering::SeqCst), 1);
}

#[test]
fn strict_provider_wait_observes_the_original_composed_cancellation() {
    struct WaitingGuard {
        lock: Arc<parking_lot::Mutex<()>>,
        entered: std::sync::mpsc::SyncSender<()>,
    }
    impl crate::access::GuardDecision for WaitingGuard {
        fn exempt(&self, _: &Arc<super::super::Snapshot>) -> Vec<String> {
            panic!("uncontrolled provider")
        }
        fn exempt_controlled(
            &self,
            _: &Arc<super::super::Snapshot>,
            prep: &Preparation,
        ) -> Result<crate::access::Exemptions> {
            self.entered.send(()).unwrap();
            let _guard = prep.lock(&self.lock)?;
            crate::access::Exemptions::new(prep)
        }
    }
    let store = Arc::new(super::super::Store::in_memory(Default::default()));
    let lock = Arc::new(parking_lot::Mutex::new(()));
    let held = lock.lock();
    let (entered, seen) = std::sync::mpsc::sync_channel(1);
    let guard = Arc::new(WaitingGuard {
        lock: lock.clone(),
        entered,
    });
    let policy = Arc::new(GraphAccess::graphs(
        Graphs::All.guarded_by("urn:*", crate::access::Decider(guard)),
        Graphs::none(),
    ));
    let flag = Arc::new(AtomicBool::new(false));
    let root = preparation();
    let prep = root
        .checked_for_query(&QueryOptions {
            cancellation: Some(crate::task::Cancellation::from_flags([flag.clone()])),
            ..Default::default()
        })
        .unwrap();
    let baseline = root.inner_used();
    let reader = store.clone();
    let (send, recv) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || send.send(reader.prepare_read(policy, &prep)).unwrap());
    seen.recv_timeout(Duration::from_secs(2)).unwrap();
    flag.store(true, Ordering::Relaxed);
    let result = recv.recv_timeout(Duration::from_secs(2));
    drop(held);
    task.join().unwrap();
    assert!(matches!(result, Ok(Err(Error::Cancelled))));
    assert_eq!(root.inner_used(), baseline);
}

#[test]
fn local_sibling_source_mask_overlap_and_concurrent_loan_refusal_leave_originals() {
    let store = super::super::Store::in_memory(Default::default());
    update(&store, "INSERT DATA { <urn:s> <urn:secret> 1 ; <urn:p> 2 }");
    let prep = preparation();
    let policy = hiding_policy();
    let first = store.prepare_read(policy.clone(), &prep).unwrap();
    let used = prep.inner_used();
    let sibling = prep.reserve(prep.room()).unwrap();
    assert!(matches!(
        store.prepare_read(policy.clone(), &prep),
        Err(Error::BudgetExceeded(_))
    ));
    assert_eq!(prep.inner_used(), 32 << 20);
    drop(sibling);
    assert_eq!(prep.inner_used(), used);
    assert_eq!(
        rows(
            first.read_source(),
            &store,
            policy,
            "SELECT ?p WHERE { <urn:s> ?p ?o }"
        ),
        ["<urn:p>"]
    );
    drop(first);
    assert_eq!(prep.inner_used(), 256);

    let prep = Preparation::new(Control::none(), 4096).unwrap();
    let start = Arc::new(std::sync::Barrier::new(3));
    let finish = Arc::new(std::sync::Barrier::new(3));
    let tasks: Vec<_> = (0..2)
        .map(|_| {
            let prep = prep.clone();
            let start = start.clone();
            let finish = finish.clone();
            std::thread::spawn(move || {
                start.wait();
                let held = prep.reserve(3072);
                finish.wait();
                held.is_ok()
            })
        })
        .collect();
    start.wait();
    finish.wait();
    assert_eq!(
        tasks
            .into_iter()
            .map(|task| usize::from(task.join().unwrap()))
            .sum::<usize>(),
        1
    );
    assert_eq!(prep.inner_used(), 256);
    assert!(matches!(
        prep.reserve(u64::MAX),
        Err(Error::BudgetExceeded(_))
    ));
    assert_eq!(prep.inner_used(), 256);
}
