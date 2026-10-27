//! Bounded preparation must preserve exact history and refuse before eager replay.
use sparkles_core::{
    Error,
    history::{At, HistoryOptions},
    sparql::{QueryOptions, query, update::update},
    store::{Store, StoreOptions},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

fn write(store: &Store, body: &str) {
    update(store, body, &QueryOptions::default()).unwrap();
}

fn values(snapshot: Arc<sparkles_core::store::Snapshot>) -> Vec<Vec<Option<oxrdf::Term>>> {
    query(
        snapshot,
        "SELECT ?s ?p ?o WHERE { ?s ?p ?o } ORDER BY ?s ?p ?o",
        &QueryOptions::default(),
    )
    .unwrap()
    .rows()
}

#[test]
fn live_cached_and_cold_reads_obey_one_cap_without_changing_legacy_limits() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreOptions::default()).unwrap();
    write(&store, "INSERT DATA { <urn:a> <urn:p> 1 }");
    write(&store, "INSERT DATA { <urn:b> <urn:p> 2 }");
    let options = HistoryOptions::default();
    assert!(matches!(
        store.snapshot_at_limited(&At::Head, &options, 0),
        Err(Error::BudgetExceeded(_))
    ));
    assert!(matches!(
        store.snapshot_at_limited(&At::Commit(1), &options, 0),
        Err(Error::BudgetExceeded(_))
    ));
    let bounded = store
        .snapshot_at_limited(&At::Commit(1), &options, 2 << 20)
        .unwrap();
    assert_eq!(bounded.1.commit.seq, 1);
    assert_eq!(
        values(bounded.0),
        values(store.snapshot_at(&At::Commit(1), &options).unwrap().0)
    );
    // A cached view must not escape a smaller request's cap.
    assert!(matches!(
        store.snapshot_at_limited(&At::Commit(1), &options, 0),
        Err(Error::BudgetExceeded(_))
    ));
    assert_eq!(
        values(store.snapshot_at(&At::Head, &options).unwrap().0).len(),
        2
    );
    assert!(matches!(
        store.snapshot_at_limited(&At::Commit(100), &options, 2 << 20),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn an_oversized_nearby_seed_is_skipped_when_base_replay_fits() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreOptions::default()).unwrap();
    for _ in 0..120 {
        write(&store, "INSERT DATA { <urn:a> <urn:p> 1 }");
        write(&store, "DELETE DATA { <urn:a> <urn:p> 1 }");
    }
    let target = store.head_commit().seq;
    let data = (0..200)
        .map(|n| format!("<urn:s{n}> <urn:p> 1 ."))
        .collect::<String>();
    write(&store, &format!("INSERT DATA {{ {data} }}"));
    let (snapshot, resolved) = store
        .snapshot_at_limited(&At::Commit(target), &HistoryOptions::default(), 400 << 10)
        .unwrap();
    assert_eq!(resolved.commit.seq, target);
    assert!(values(snapshot).is_empty());
    assert_eq!(values(store.snapshot()).len(), 200);
}

#[test]
fn cancellation_and_deadline_apply_to_live_fast_paths() {
    let store = Store::in_memory(StoreOptions::default());
    let cancel = Arc::new(AtomicBool::new(true));
    let mut options = HistoryOptions {
        cancel: Some(cancel.clone()),
        deadline: None,
    };
    assert!(matches!(
        store.snapshot_at_limited(&At::Head, &options, u64::MAX),
        Err(Error::Cancelled)
    ));
    cancel.store(false, Ordering::Relaxed);
    options.deadline = Some(Instant::now() - Duration::from_millis(1));
    assert!(matches!(
        store.snapshot_at_limited(&At::Head, &options, u64::MAX),
        Err(Error::Timeout)
    ));
    assert!(
        store
            .snapshot_at(&At::Head, &HistoryOptions::default())
            .is_ok()
    );
}

#[test]
fn bounded_origin_selection_follows_only_the_requested_family() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreOptions::default()).unwrap();
    write(&store, "INSERT DATA { <urn:a> <urn:p> 1 }");
    write(&store, "INSERT DATA { <urn:b> <urn:p> 2 }");
    store.create_branch("dev", &Default::default()).unwrap();
    let branch = store.branch("dev").unwrap();
    write(&branch, "INSERT DATA { <urn:branch> <urn:p> 3 }");
    let (snapshot, resolved, origin) = store
        .snapshot_at_with_branch_limited(
            &branch,
            &At::Commit(1),
            &HistoryOptions::default(),
            2 << 20,
        )
        .unwrap();
    assert_eq!(origin, store.branch_id());
    assert_eq!(resolved.commit.seq, 1);
    assert_eq!(values(snapshot).len(), 1);
    let alien = Store::in_memory(StoreOptions::default());
    assert!(
        store
            .snapshot_at_with_branch_limited(&alien, &At::Head, &HistoryOptions::default(), 2 << 20)
            .is_err()
    );
}

#[test]
fn missing_memory_history_is_not_replaced_by_head() {
    let store = Store::in_memory(StoreOptions::default());
    write(&store, "INSERT DATA { <urn:a> <urn:p> 1 }");
    assert!(matches!(
        store.snapshot_at_limited(&At::Commit(0), &HistoryOptions::default(), 2 << 20),
        Err(Error::HistoryGone(_))
    ));
}

fn shared_options(history: u64, local: u64) -> StoreOptions {
    use sparkles_core::store::caches::{CacheSizes, SharedCaches};
    StoreOptions {
        history_cache_bytes: local,
        shared: Some(
            SharedCaches::new(CacheSizes {
                blocks: 0,
                results: 0,
                service: 0,
                history,
            })
            .scope(),
        ),
        ..Default::default()
    }
}

#[test]
fn effective_shared_capacity_precedes_live_memory_and_persistent_fast_paths() {
    for memory in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let options = shared_options(64 << 10, 512 << 20);
        let store = if memory {
            Store::in_memory(options)
        } else {
            Store::open(root.path(), options).unwrap()
        };
        write(&store, "INSERT DATA { <urn:a> <urn:p> 1 }");
        store.create_snapshot("old", &At::Head, None).unwrap();
        write(&store, "INSERT DATA { <urn:b> <urn:p> 2 }");
        assert!(
            matches!(
                store.snapshot_at_limited(&At::Head, &HistoryOptions::default(), 2 << 20),
                Err(Error::BudgetExceeded(_))
            ),
            "live scope bypass, memory={memory}"
        );
        assert!(
            matches!(
                store.snapshot_at_limited(&At::Commit(1), &HistoryOptions::default(), 2 << 20),
                Err(Error::BudgetExceeded(_))
            ),
            "retained scope bypass, memory={memory}"
        );
        store
            .snapshot_at(&At::Commit(1), &HistoryOptions::default())
            .unwrap();
        assert!(
            matches!(
                store.snapshot_at_limited(&At::Commit(1), &HistoryOptions::default(), 2 << 20),
                Err(Error::BudgetExceeded(_))
            ),
            "cached scope bypass, memory={memory}"
        );
    }
}

#[test]
fn ignored_zero_local_capacity_cannot_override_a_shared_scope() {
    for memory in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let options = shared_options(2 << 20, 0);
        let store = if memory {
            Store::in_memory(options)
        } else {
            Store::open(root.path(), options).unwrap()
        };
        write(&store, "INSERT DATA { <urn:a> <urn:p> 1 }");
        store.create_snapshot("old", &At::Head, None).unwrap();
        write(&store, "INSERT DATA { <urn:b> <urn:p> 2 }");
        assert!(
            store
                .snapshot_at_limited(&At::Head, &HistoryOptions::default(), 2 << 20)
                .is_ok()
        );
        let (_, resolved) = store
            .snapshot_at_limited(&At::Commit(1), &HistoryOptions::default(), 2 << 20)
            .unwrap();
        assert_eq!(resolved.commit.seq, 1);
    }
}
