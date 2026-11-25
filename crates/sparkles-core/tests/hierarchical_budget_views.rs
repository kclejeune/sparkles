//! Track 2 hierarchical budget tests: incremental views, live queries, internal
//! circuits, nested SPARQL evaluation, SERVICE view:read fallbacks, and lock-free
//! release under QueryPool.

use parking_lot::Mutex;
use sparkles_core::access::GraphAccess;
use sparkles_core::error::{BudgetKind, Error, Result};
use sparkles_core::incr::{
    Host, Inference, InternalAccess, InternalSpec, LiveEvent, LiveOptions, OpenHost, Runtime,
    RuntimeOptions, ViewDef, ViewState,
};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::pool::QueryPool;
use sparkles_core::sparql::rdfs::{RdfsOnRead, SchemaSource};
use sparkles_core::sparql::{self, AggregateErrors, QueryOptions};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const KB: u64 = 1 << 10;
const MB: u64 = 1 << 20;

fn load_rows(store: &Store, prefix: &str, count: usize, payload_len: usize) {
    let payload = "v".repeat(payload_len);
    let mut nt = String::new();
    for i in 0..count {
        nt.push_str(&format!("<urn:{prefix}:{i}> <urn:p> \"{payload}{i}\" .\n"));
    }
    store
        .load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
}

fn upd(store: &Store, text: &str) {
    sparql::update::update(store, text, &QueryOptions::default()).unwrap();
}

#[test]
fn named_view_charges_pool_and_returns_exact_zero_on_delete_and_close() {
    let store = Store::in_memory(StoreOptions::default());
    load_rows(&store, "s", 400, 16);

    let pool = QueryPool::new(16 * MB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));

    rt.put(
        &store,
        "items",
        ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o }"),
    )
    .unwrap();
    rt.round(&store);

    assert!(
        pool.granted() > 0,
        "active named view must reserve credit from QueryPool"
    );

    // Apply commits so Slot::history retains decoded diffs too.
    for i in 0..5 {
        upd(
            &store,
            &format!("INSERT DATA {{ <urn:extra:{i}> <urn:p> \"payload-{i}\" }}"),
        );
        rt.round(&store);
    }
    assert!(pool.granted() > 0);

    // Deleting the sole view frees the cell and history immediately via gc().
    rt.delete("items").unwrap();
    assert_eq!(
        pool.granted(),
        0,
        "deleting a view must return all state and history credit to 0"
    );

    // Recreate, build, then verify Runtime::close returns credit to 0 even while `rt` lives.
    rt.put(
        &store,
        "items2",
        ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o }"),
    )
    .unwrap();
    rt.round(&store);
    assert!(pool.granted() > 0);

    rt.close(&store);
    assert_eq!(
        pool.granted(),
        0,
        "Runtime::close must return all view credit to 0 immediately"
    );
}

#[test]
fn live_query_and_internal_accumulator_return_exact_zero_on_drop_and_gc() {
    let store = Store::in_memory(StoreOptions::default());
    load_rows(&store, "l", 300, 16);

    let pool = QueryPool::new(16 * MB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));

    // Live query charges pool until dropped + gc().
    let live = rt
        .live(
            &store,
            "SELECT ?s ?o WHERE { ?s <urn:p> ?o }",
            None,
            &LiveOptions::default(),
        )
        .unwrap();
    assert!(matches!(live.try_next(), Some(LiveEvent::Snapshot { .. })));
    assert!(pool.granted() > 0);

    drop(live);
    rt.round(&store);
    assert_eq!(
        pool.granted(),
        0,
        "dropping the last LiveQuery and running gc() must return pool credit to 0"
    );

    // Internal Sink::Rows circuit charges pool and returns to 0 after drop + collect_internal.
    let rows_ic = rt
        .attach_internal(
            &store,
            InternalSpec::rows("ic_rows", "SELECT ?s ?o WHERE { ?s <urn:p> ?o }"),
            InternalAccess::everything(),
        )
        .unwrap();
    assert!(pool.granted() > 0);
    drop(rows_ic);
    rt.round(&store);
    assert_eq!(pool.granted(), 0);

    // Internal Sink::Accumulator with Tally charges pool, shrinks on discard_through, and returns to 0.
    let acc = rt
        .attach_internal(
            &store,
            InternalSpec::accumulator("ic_acc", "SELECT ?s ?o WHERE { ?s <urn:p> ?o }", vec!["s"]),
            InternalAccess::everything(),
        )
        .unwrap();
    let mut big = String::from("INSERT DATA { ");
    for i in 0..1000 {
        big.push_str(&format!("<urn:acc:{i}> <urn:p> \"{}\". ", "a".repeat(256)));
    }
    big.push('}');
    upd(&store, &big);
    rt.round(&store);
    let peak_granted = pool.granted();
    assert!(
        peak_granted > 0,
        "accumulator tally must reserve from QueryPool"
    );

    acc.discard_through(store.snapshot().commit);
    drop(acc);
    rt.round(&store);
    assert_eq!(
        pool.granted(),
        0,
        "dropping InternalCircuit accumulator and collecting must return credit to 0"
    );
}

#[test]
fn query_and_view_cannot_overshoot_shared_pool_together() {
    let store = Store::in_memory(StoreOptions::default());
    load_rows(&store, "q", 2000, 32);

    // Pool is 2 MiB; query takes 1.95 MiB (leaving < 64 KiB). View local limit is 16 MiB.
    let pool = QueryPool::new(2 * MB);
    let holder = pool.work_parent().reserve(2 * MB - 16 * KB).unwrap();

    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));

    let mut def = ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o }");
    def.memory_bytes = 16 * MB;
    rt.put(&store, "big", def).unwrap();
    rt.round(&store);

    let info = rt.get(&store, "big").unwrap();
    assert!(
        matches!(&info.state, ViewState::Suspended { reason } if reason.contains("over-budget")),
        "expected view to suspend over-budget when QueryPool is full, got {:?}",
        info.state
    );
    // Failed view cleared its state and returned all credit, leaving only `holder`.
    assert_eq!(pool.granted(), holder.bytes());

    drop(holder);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn window_and_path_views_enforce_pool_cap_mid_pass() {
    let store = Store::in_memory(StoreOptions::default());
    let mut nt = String::new();
    for i in 0..250 {
        nt.push_str(&format!("<urn:n{i}> <urn:edge> <urn:n{}> .\n", i + 1));
    }
    store
        .load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();

    // Tight 128 KiB pool: transitive closure of 250-node chain has ~31k pairs (~1.2 MiB)
    // and must be refused cleanly by the pool before pass completion.
    let pool = QueryPool::new(128 * KB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));

    let mut path_def = ViewDef::table("SELECT ?x ?y WHERE { ?x <urn:edge>+ ?y }");
    path_def.memory_bytes = 32 * MB;
    rt.put(&store, "reach", path_def).unwrap();
    rt.round(&store);
    let info = rt.get(&store, "reach").unwrap();
    assert!(
        matches!(&info.state, ViewState::Suspended { reason } if reason.contains("over-budget")),
        "path view over QueryPool must fail over-budget: {:?}",
        info.state
    );
    assert_eq!(pool.granted(), 0);

    // Ordered window with large keys exceeding tight 64 KiB pool.
    let store2 = Store::in_memory(StoreOptions::default());
    load_rows(&store2, "w", 500, 512);
    let pool2 = QueryPool::new(64 * KB);
    let rt2 = Runtime::open_with_pool(
        None,
        store2.dataset_id(),
        RuntimeOptions::default(),
        Some(pool2.clone()),
    );
    rt2.set_host(Arc::new(OpenHost));
    let mut win_def = ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY ?o LIMIT 400");
    win_def.memory_bytes = 32 * MB;
    rt2.put(&store2, "top", win_def).unwrap();
    rt2.round(&store2);
    let info2 = rt2.get(&store2, "top").unwrap();
    assert!(
        matches!(&info2.state, ViewState::Suspended { reason } if reason.contains("over-budget")),
        "window view over QueryPool must fail over-budget: {:?}",
        info2.state
    );
    assert_eq!(pool2.granted(), 0);
}

#[test]
fn nested_exists_and_recompute_inside_views_charge_query_pool() {
    let store = Store::in_memory(StoreOptions::default());
    load_rows(&store, "e", 600, 64);
    upd(
        &store,
        "INSERT DATA { <urn:Sub> <http://www.w3.org/2000/01/rdf-schema#subPropertyOf> <urn:Sup> }",
    );

    // 1. FILTER EXISTS (Pass::ctx) inside view under a tight pool.
    let pool = QueryPool::new(16 * KB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));
    let mut def = ViewDef::table(
        "SELECT ?s WHERE { ?s <urn:p> ?o FILTER(EXISTS { SELECT (COUNT(?x) AS ?c) WHERE { ?x <urn:p> ?y } } || false) }",
    );
    def.memory_bytes = 32 * MB;
    rt.put(&store, "exists_view", def).unwrap();
    rt.round(&store);
    let info = rt.get(&store, "exists_view").unwrap();
    assert!(
        matches!(&info.state, ViewState::Suspended { reason } if reason.contains("over-budget")),
        "FILTER EXISTS inside view must fail over-budget on tight QueryPool: {:?}",
        info.state
    );
    assert_eq!(pool.granted(), 0);

    // 2. Recompute node (Pass::evaluate via RDFS on read) under a tight pool.
    struct RdfsHost;
    impl Host for RdfsHost {
        fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
            Ok(None)
        }
        fn stored_query(&self, _: &str) -> Result<String> {
            Err(Error::NotFound("none".into()))
        }
        fn inference(&self) -> Option<Inference> {
            Inference::new(
                Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
                Vec::new(),
            )
        }
    }
    let pool2 = QueryPool::new(16 * KB);
    let rt2 = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool2.clone()),
    );
    rt2.set_host(Arc::new(RdfsHost));
    let mut rdef = ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o }");
    rdef.memory_bytes = 32 * MB;
    rt2.put(&store, "recomp_view", rdef).unwrap();
    rt2.round(&store);
    let info2 = rt2.get(&store, "recomp_view").unwrap();
    assert!(
        matches!(&info2.state, ViewState::Suspended { reason } if reason.contains("over-budget")),
        "Recompute view must fail over-budget on tight QueryPool: {:?}",
        info2.state
    );
    assert_eq!(pool2.granted(), 0);
}

#[test]
fn service_view_read_fallbacks_charge_reading_query_parent_and_return_credit() {
    let store = Store::in_memory(StoreOptions::default());
    load_rows(&store, "f", 800, 32);

    let pool = QueryPool::new(16 * MB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));
    rt.put(
        &store,
        "v",
        ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o }"),
    )
    .unwrap();
    rt.round(&store);
    let baseline = pool.granted();
    assert!(baseline > 0);

    // 1. Trigger `evaluate_under` by requesting Sparkles aggregate-error reading while
    //    the view keeps Jena reading, with a tight local query memory limit.
    let q = "PREFIX view: <urn:x-sparkles:view#> \
             SELECT ?s ?o WHERE { SERVICE view:read { [] view:name \"v\" ; view:bind ?s, ?o . } }";
    let tight_opts = QueryOptions {
        pool: Some(pool.clone()),
        views: Some(rt.clone()),
        aggregate_errors: Some(AggregateErrors::Sparkles),
        max_memory_bytes: Some(8 * KB),
        no_cache: true,
        ..Default::default()
    };
    let err = sparql::query(store.snapshot(), q, &tight_opts)
        .err()
        .expect("tight evaluate_under must fail");
    assert!(
        matches!(err, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory),
        "evaluate_under fallback must respect reading query's local memory limit: {err:?}"
    );
    assert_eq!(
        pool.granted(),
        baseline,
        "refused evaluate_under fallback must leak zero credit"
    );

    // Now succeed with generous limit and confirm credit returns to baseline.
    let ok_opts = QueryOptions {
        pool: Some(pool.clone()),
        views: Some(rt.clone()),
        aggregate_errors: Some(AggregateErrors::Sparkles),
        max_memory_bytes: Some(16 * MB),
        no_cache: true,
        ..Default::default()
    };
    let res = sparql::query(store.snapshot(), q, &ok_opts).unwrap();
    assert_eq!(res.len(), 800);
    drop(res);
    assert_eq!(pool.granted(), baseline);

    // 2. Trigger `read_back` by holding an older snapshot and advancing > VIEW_HISTORY_COMMITS.
    let old_snap = store.snapshot();
    for i in 0..20 {
        upd(
            &store,
            &format!("INSERT DATA {{ <urn:adv:{i}> <urn:p> \"x{i}\" }}"),
        );
        rt.round(&store);
    }
    let baseline_after = pool.granted();
    let tight_read_back = QueryOptions {
        pool: Some(pool.clone()),
        views: Some(rt.clone()),
        max_memory_bytes: Some(8 * KB),
        no_cache: true,
        ..Default::default()
    };
    let err2 = sparql::query(old_snap.clone(), q, &tight_read_back)
        .err()
        .expect("tight read_back must fail");
    assert!(
        matches!(err2, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory),
        "read_back fallback must respect reading query's local memory limit: {err2:?}"
    );
    assert_eq!(pool.granted(), baseline_after);

    // Also test read_back under a nearly-full QueryPool (ServerMemory).
    let blocker = pool
        .work_parent()
        .reserve(pool.capacity() - pool.granted())
        .unwrap();
    let pool_tight_read_back = QueryOptions {
        pool: Some(pool.clone()),
        views: Some(rt.clone()),
        max_memory_bytes: Some(16 * MB),
        no_cache: true,
        ..Default::default()
    };
    let err3 = sparql::query(old_snap, q, &pool_tight_read_back)
        .err()
        .expect("pool-tight read_back must fail");
    assert!(
        matches!(err3, Error::BudgetExceeded(b) if b.kind == BudgetKind::ServerMemory),
        "read_back fallback must refuse with ServerMemory when QueryPool is full: {err3:?}"
    );
    drop(blocker);
    assert_eq!(pool.granted(), baseline_after);

    rt.close(&store);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn inference_switch_and_checkpoint_recovery_preserve_pool_invariants() {
    struct SwitchHost(Mutex<Option<Inference>>);
    impl Host for SwitchHost {
        fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
            Ok(None)
        }
        fn stored_query(&self, _: &str) -> Result<String> {
            Err(Error::NotFound("none".into()))
        }
        fn inference(&self) -> Option<Inference> {
            self.0.lock().clone()
        }
    }

    // 1. Inference mode switch preserves exact credit and returns to 0 on close.
    let store = Store::in_memory(StoreOptions::default());
    load_rows(&store, "sw", 300, 16);
    let pool = QueryPool::new(16 * MB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    let host = Arc::new(SwitchHost(Mutex::new(None)));
    rt.set_host(host.clone());
    rt.put(
        &store,
        "sw_view",
        ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o }"),
    )
    .unwrap();
    rt.round(&store);
    assert!(pool.granted() > 0);

    *host.0.lock() = Inference::new(
        Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
        Vec::new(),
    );
    rt.round(&store);
    assert!(pool.granted() > 0);
    rt.close(&store);
    assert_eq!(pool.granted(), 0);

    // 2. Checkpoint recovery over a tight QueryPool fails cleanly with pool.granted() == 0.
    let dir = tempfile::tempdir().unwrap();
    let disk_store = Store::open(dir.path(), StoreOptions::default()).unwrap();
    load_rows(&disk_store, "ck", 1000, 32);
    {
        let big_pool = QueryPool::new(16 * MB);
        let rt1 = Runtime::open_with_pool(
            Some(dir.path().to_path_buf()),
            disk_store.dataset_id(),
            RuntimeOptions::default(),
            Some(big_pool.clone()),
        );
        rt1.set_host(Arc::new(OpenHost));
        rt1.put(
            &disk_store,
            "persisted",
            ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o }"),
        )
        .unwrap();
        rt1.round(&disk_store);
        assert!(big_pool.granted() > 0);
        rt1.close(&disk_store);
        assert_eq!(big_pool.granted(), 0);
    }
    // Reopen with a tiny pool that cannot hold the recovered checkpoint or fresh build.
    let tiny_pool = QueryPool::new(16 * KB);
    let rt2 = Runtime::open_with_pool(
        Some(dir.path().to_path_buf()),
        disk_store.dataset_id(),
        RuntimeOptions::default(),
        Some(tiny_pool.clone()),
    );
    rt2.set_host(Arc::new(OpenHost));
    rt2.round(&disk_store);
    let info = rt2.get(&disk_store, "persisted").unwrap();
    assert!(
        matches!(&info.state, ViewState::Suspended { reason } if reason.contains("over-budget")),
        "checkpoint recovery exceeding tight pool must suspend over-budget: {:?}",
        info.state
    );
    assert_eq!(
        tiny_pool.granted(),
        0,
        "refused checkpoint recovery must return pool credit to 0"
    );
}

#[test]
fn reentrant_pool_waker_never_deadlocks_during_view_lifecycle() {
    let store = Arc::new(Store::in_memory(StoreOptions::default()));
    load_rows(&store, "lk", 2500, 32);

    let pool = QueryPool::new(16 * MB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));
    rt.put(
        &store,
        "v",
        ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o }"),
    )
    .unwrap();
    rt.round(&store);
    let view_granted = pool.granted();
    assert!(view_granted > 0);

    // QueryPool::enter rounds waiter requests up to GRANULE (4 MiB). Reserve
    // `capacity - 4 MiB` so the 4 MiB waiter does not fit while the view holds credit,
    // and wakes synchronously the instant the view releases its credit.
    let filler = pool
        .work_parent()
        .reserve(pool.capacity() - 4 * MB)
        .unwrap();
    let woke = Arc::new(AtomicUsize::new(0));
    let woke_cb = woke.clone();
    let rt_cb = rt.clone();
    let store_cb = store.clone();
    let waiter = pool.enter(
        4 * MB,
        Some(Arc::new(move || {
            // Reenter Runtime locks (reg.lock(), cell.info.lock()) synchronously from
            // inside QueryPool::release()! Any lease dropped under reg/cell.state would
            // deadlock right here.
            let _ = rt_cb.has_views();
            let _ = rt_cb.list(&store_cb);
            let _ = rt_cb.stats(&store_cb);
            woke_cb.fetch_add(1, Ordering::SeqCst);
        })),
    );

    // 1. Delete view -> drops cell & lease -> fires synchronous waker.
    rt.delete("v").unwrap();
    assert!(
        woke.load(Ordering::SeqCst) >= 1,
        "waker must have run synchronously on view delete"
    );
    assert_eq!(pool.granted(), filler.bytes());

    drop(waiter);
    drop(filler);
    assert_eq!(pool.granted(), 0);

    // 2. Also test Runtime::close firing a reentrant waker.
    rt.put(
        &store,
        "v2",
        ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o }"),
    )
    .unwrap();
    rt.round(&store);
    let g2 = pool.granted();
    assert!(g2 > 0);
    let filler2 = pool
        .work_parent()
        .reserve(pool.capacity() - 4 * MB)
        .unwrap();
    let woke2 = Arc::new(AtomicUsize::new(0));
    let woke2_cb = woke2.clone();
    let rt2_cb = rt.clone();
    let store2_cb = store.clone();
    let waiter2 = pool.enter(
        4 * MB,
        Some(Arc::new(move || {
            let _ = rt2_cb.has_views();
            let _ = rt2_cb.stats(&store2_cb);
            woke2_cb.fetch_add(1, Ordering::SeqCst);
        })),
    );
    rt.close(&store);
    assert!(
        woke2.load(Ordering::SeqCst) >= 1,
        "waker must have run synchronously on Runtime::close"
    );
    drop(waiter2);
    drop(filler2);
    assert_eq!(pool.granted(), 0);
}
