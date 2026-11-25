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

#[test]
fn join_cross_product_fails_mid_join_even_when_inputs_and_grouped_result_fit() {
    let store = Store::in_memory(StoreOptions::default());
    let mut nt = String::new();
    // Two small inputs (300 rows each) sharing the same join key `?k`: each input is
    // ~15 KiB and the final grouped COUNT(*) is 1 row (~64 bytes), but the join cross
    // product is 90,000 rows (~3+ MiB).
    for i in 0..300 {
        nt.push_str(&format!("<urn:l:{i}> <urn:lk> <urn:shared> .\n"));
        nt.push_str(&format!("<urn:r:{i}> <urn:rk> <urn:shared> .\n"));
    }
    store
        .load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();

    // 512 KiB pool easily holds both inputs (~30 KiB) and the 1-row grouped result,
    // so only mid-join cross-product pre-admission can reject this view.
    let pool = QueryPool::new(512 * KB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));
    let mut def = ViewDef::table(
        "SELECT (COUNT(*) AS ?c) WHERE { { SELECT ?l ?k WHERE { ?l <urn:lk> ?k } } { SELECT ?r ?k WHERE { ?r <urn:rk> ?k } } }",
    );
    def.memory_bytes = 32 * MB;
    rt.put(&store, "cross", def).unwrap();
    rt.round(&store);

    let info = rt.get(&store, "cross").unwrap();
    assert!(
        matches!(&info.state, ViewState::Suspended { reason } if reason.contains("over-budget")),
        "quadratic join intermediate must fail over-budget mid-pass even with 1-row grouped output: {:?}",
        info.state
    );
    assert_eq!(pool.granted(), 0);
}

#[test]
fn circuit_reset_at_over_50_percent_occupancy_succeeds_without_2x_oom() {
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

    let store = Store::in_memory(StoreOptions::default());
    // DISTINCT keeps both operator `weights` ZSet and `result` ZSet (~2x row state).
    // On a reset, old operator state must be cleared and `before` moved without 2x
    // double-counting so a view occupying ~65% of its budget resets cleanly.
    load_rows(&store, "rst", 600, 16);

    // First measure the view's steady-state bytes on a generous pool.
    let probe_pool = QueryPool::new(16 * MB);
    let probe_rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(probe_pool),
    );
    probe_rt.set_host(Arc::new(OpenHost));
    probe_rt
        .put(
            &store,
            "m",
            ViewDef::table("SELECT DISTINCT ?s ?o WHERE { ?s <urn:p> ?o }"),
        )
        .unwrap();
    probe_rt.round(&store);
    let steady_bytes = probe_rt.get(&store, "m").unwrap().state_bytes;
    assert!(steady_bytes > 0);
    probe_rt.close(&store);

    // Choose a local limit where steady-state occupies ~65% (> 50%): old 2x reset bug
    // would need > 130% and fail over-budget.
    let limit = (steady_bytes * 100) / 65;
    let pool = QueryPool::new(16 * MB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    let host = Arc::new(SwitchHost(Mutex::new(None)));
    rt.set_host(host.clone());
    let mut def = ViewDef::table("SELECT DISTINCT ?s ?o WHERE { ?s <urn:p> ?o }");
    def.memory_bytes = limit;
    rt.put(&store, "d", def).unwrap();
    rt.round(&store);
    assert!(
        matches!(rt.get(&store, "d").unwrap().state, ViewState::Current),
        "initial build must succeed at ~65% occupancy"
    );

    // Trigger an inference switch -> next round runs `Circuit::reset` (old `result`
    // compared against fresh build).
    *host.0.lock() = Inference::new(
        Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
        Vec::new(),
    );
    rt.round(&store);
    let info = rt.get(&store, "d").unwrap();
    assert!(
        matches!(info.state, ViewState::Current),
        "reset at >50% occupancy must succeed without spurious 2x OOM, got {:?}",
        info.state
    );
    rt.close(&store);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn slot_history_evicts_under_pressure_and_active_state_takes_priority() {
    let store = Store::in_memory(StoreOptions::default());
    load_rows(&store, "h", 100, 128);

    // Give the view a 128 KiB local ceiling: initial build (100 rows + 64 KiB child
    // step) easily fits, Update 1 stores an ~80 KiB replay diff in Slot::history
    // alongside ~15 KiB active state, and Update 2's active pass + growth exceeds 128
    // KiB unless Slot::history is evicted during admission.
    let pool = QueryPool::new(16 * MB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));
    let mut def = ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o }");
    def.memory_bytes = 128 * KB;
    rt.put(&store, "hist_prio", def).unwrap();
    rt.round(&store);
    assert!(matches!(
        rt.get(&store, "hist_prio").unwrap().state,
        ViewState::Current
    ));
    let c0 = store.snapshot().commit;

    // Update 1: add 220 rows with 256-byte literals so the decoded diff in Slot::history
    // occupies ~80 KiB alongside ~15 KiB of circuit state (~95 KiB total <= 128 KiB).
    let mut u1 = String::from("INSERT DATA { ");
    for i in 0..220 {
        u1.push_str(&format!("<urn:h1:{i}> <urn:p> \"{}\". ", "x".repeat(256)));
    }
    u1.push('}');
    upd(&store, &u1);
    rt.round(&store);
    assert!(matches!(
        rt.get(&store, "hist_prio").unwrap().state,
        ViewState::Current
    ));
    // Prove Update 1's replay diff WAS retained in Slot::history (past-commit read at c0
    // with `snap: None` succeeds only via Slot::history).
    let past_ok = rt
        .read(&sparkles_core::sparql::viewsvc::ViewRead {
            name: "hist_prio",
            at: c0,
            snap: None,
            allow_stale: false,
            caller: None,
            deadline: None,
            aggregate_errors: None,
            memory_parent: None,
            memory_pool: None,
            max_memory_bytes: None,
        })
        .expect("Update 1 replay diff must be retained in Slot::history before Update 2");
    assert_eq!(past_ok.rows.len(), 100);

    // Update 2: add 200 more rows. Active state + pass exceeds 128 KiB unless Update 1's
    // replay diff is evicted by the budget's reclaim hook.
    let mut u2 = String::from("INSERT DATA { ");
    for i in 0..200 {
        u2.push_str(&format!("<urn:h2:{i}> <urn:p> \"{}\". ", "y".repeat(256)));
    }
    u2.push('}');
    upd(&store, &u2);
    rt.round(&store);
    let info = rt.get(&store, "hist_prio").unwrap();
    assert!(
        matches!(info.state, ViewState::Current),
        "active state must evict optional replay history instead of suspending: {:?}",
        info.state
    );
    assert_eq!(info.rows, 520);
    // Prove Update 1's replay diff WAS evicted during Update 2.
    let past_evicted = rt.read(&sparkles_core::sparql::viewsvc::ViewRead {
        name: "hist_prio",
        at: c0,
        snap: None,
        allow_stale: false,
        caller: None,
        deadline: None,
        aggregate_errors: None,
        memory_parent: None,
        memory_pool: None,
        max_memory_bytes: None,
    });
    assert!(
        past_evicted.is_err(),
        "Update 1 replay diff must have been evicted during Update 2 under pressure"
    );

    rt.close(&store);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn accumulator_tally_discard_shrinks_pool_and_failed_handle_pins_zero_credit() {
    let store = Store::in_memory(StoreOptions::default());
    let pool = QueryPool::new(16 * MB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));

    let acc = rt
        .attach_internal(
            &store,
            InternalSpec::accumulator(
                "ic_shrink",
                "SELECT DISTINCT ?s ?o WHERE { ?s <urn:p> ?o }",
                vec!["s"],
            ),
            InternalAccess::everything(),
        )
        .unwrap();

    // Insert enough distinct large keys (> 1.5 MiB) so LiveCredit crosses LIVE_SLACK (1 MiB)
    // and `discard_through` visibly lowers `pool.granted()`.
    let mut big = String::from("INSERT DATA { ");
    for i in 0..2500 {
        big.push_str(&format!(
            "<urn:bigkey:{}:{i}> <urn:p> \"v\" . ",
            "k".repeat(300)
        ));
    }
    big.push('}');
    upd(&store, &big);
    rt.round(&store);
    let peak = pool.granted();
    assert!(
        peak > MB,
        "large accumulator tally must reserve > 1 MiB from pool, got {peak}"
    );

    acc.discard_through(store.snapshot().commit);
    let after_discard = pool.granted();
    assert!(
        after_discard < peak,
        "discard_through must shrink QueryPool granted bytes (before {peak}, after {after_discard})"
    );

    // Populate tally again, then fill the QueryPool so the next advance fails over-budget.
    let mut big2 = String::from("INSERT DATA { ");
    for i in 0..800 {
        big2.push_str(&format!(
            "<urn:failkey:{}:{i}> <urn:p> \"v\" . ",
            "m".repeat(256)
        ));
    }
    big2.push('}');
    upd(&store, &big2);
    rt.round(&store);
    assert!(pool.granted() > 0);

    // Pin all remaining pool room and apply a commit whose `DISTINCT` operator state
    // alone (> 1.5 MiB) exceeds `LIVE_SLACK` so the circuit's own state is refused by
    // `QueryPool`, failing the accumulator handle and clearing both circuit and tally.
    let filler = pool
        .work_parent()
        .reserve(pool.capacity() - pool.granted())
        .unwrap();
    let mut u3 = String::from("INSERT DATA { ");
    for i in 0..24_000 {
        u3.push_str(&format!("<urn:bm:{i}> <urn:p> {i} . "));
    }
    u3.push('}');
    upd(&store, &u3);
    rt.round(&store);

    // Accumulator is now Failed; even while `acc` handle stays alive, `fail_internal`
    // must have cleared both circuit state and tally storage so only `filler` remains.
    assert!(
        acc.counts(0).is_err(),
        "refused accumulator must report failed status"
    );
    assert_eq!(
        pool.granted(),
        filler.bytes(),
        "failed accumulator handle must not pin QueryPool credit while alive"
    );

    drop(filler);
    assert_eq!(pool.granted(), 0);
    drop(acc);
    rt.round(&store);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn shared_circuit_limit_lowers_when_larger_consumer_detaches() {
    let store = Store::in_memory(StoreOptions::default());
    load_rows(&store, "sh", 50, 16);

    let pool = QueryPool::new(16 * MB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));

    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o }";
    let mut big = ViewDef::table(q);
    big.memory_bytes = 8 * MB;
    let mut small = ViewDef::table(q);
    small.memory_bytes = 16 * KB;

    rt.put(&store, "big_v", big).unwrap();
    rt.put(&store, "small_v", small).unwrap();
    rt.round(&store);
    assert!(matches!(
        rt.get(&store, "small_v").unwrap().state,
        ViewState::Current
    ));

    // Detach the 8 MiB consumer: the shared cell's local limit must drop back to 16 KiB.
    rt.delete("big_v").unwrap();

    // Now grow the result well past 16 KiB; the remaining view must be refused by its own
    // 16 KiB ceiling rather than inheriting `big_v`'s departed 8 MiB ceiling.
    load_rows(&store, "sh_more", 800, 32);
    rt.round(&store);
    let info = rt.get(&store, "small_v").unwrap();
    assert!(
        matches!(&info.state, ViewState::Suspended { reason } if reason.contains("over-budget")),
        "remaining consumer must enforce lowered 16 KiB limit after larger view detached: {:?}",
        info.state
    );
    rt.close(&store);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn service_view_read_lazy_parent_and_unpooled_fallback_uses_runtime_pool() {
    use sparkles_core::sparql::viewsvc::ViewRead;

    let store = Store::in_memory(StoreOptions::default());
    load_rows(&store, "lp", 300, 32);

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

    // 1. Ready-cache hit must NEVER invoke the lazy `memory_parent` supplier.
    let snap = store.snapshot();
    let calls = AtomicUsize::new(0);
    let supplier = || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(pool.work_parent())
    };
    let rows = rt
        .read(&ViewRead {
            name: "v",
            at: snap.commit,
            snap: Some(&snap),
            allow_stale: false,
            caller: None,
            deadline: None,
            aggregate_errors: Some(AggregateErrors::Jena),
            memory_parent: Some(&supplier),
            memory_pool: Some(&pool),
            max_memory_bytes: None,
        })
        .unwrap();
    assert_eq!(rows.rows.len(), 300);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "Ready view read must not call memory_parent supplier or promote Ctx ledger"
    );

    // 2. Fallback (`evaluate_under`) invokes the lazy supplier once and keeps its
    //    decoded rows leased until `rows2` drops.
    let rows2 = rt
        .read(&ViewRead {
            name: "v",
            at: snap.commit,
            snap: Some(&snap),
            allow_stale: false,
            caller: None,
            deadline: None,
            aggregate_errors: Some(AggregateErrors::Sparkles),
            memory_parent: Some(&supplier),
            memory_pool: Some(&pool),
            max_memory_bytes: None,
        })
        .unwrap();
    assert_eq!(rows2.rows.len(), 300);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        pool.granted() > baseline,
        "fallback ViewRows must keep its decoded rows leased until dropped"
    );
    drop(rows2);
    assert_eq!(pool.granted(), baseline);

    // 3. An UNPOOLED query (`pool: None`) running `SERVICE view:read` fallback against
    //    a POOLED `Runtime` must route through `rt`'s `QueryPool` (never bypassing it or
    //    tripping domain `debug_assert!`).
    let blocker = pool
        .work_parent()
        .reserve(pool.capacity() - pool.granted())
        .unwrap();
    let q = "PREFIX view: <urn:x-sparkles:view#> \
             SELECT ?s ?o WHERE { SERVICE view:read { [] view:name \"v\" ; view:bind ?s, ?o . } }";
    let unpooled_fallback_opts = QueryOptions {
        pool: None,
        views: Some(rt.clone()),
        aggregate_errors: Some(AggregateErrors::Sparkles),
        max_memory_bytes: Some(16 * MB),
        no_cache: true,
        ..Default::default()
    };
    let err = sparql::query(store.snapshot(), q, &unpooled_fallback_opts)
        .err()
        .expect("unpooled query fallback against full runtime QueryPool must fail");
    assert!(
        matches!(err, Error::BudgetExceeded(b) if b.kind == BudgetKind::ServerMemory),
        "unpooled query fallback on pooled Runtime must enforce runtime QueryPool (ServerMemory), got {err:?}"
    );
    drop(blocker);
    assert_eq!(pool.granted(), baseline);

    rt.close(&store);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn isolated_single_row_filter_exists_charges_query_pool() {
    let store = Store::in_memory(StoreOptions::default());
    // 1 tiny outer row, 1200 inner rows scanned by EXISTS subquery.
    upd(&store, "INSERT DATA { <urn:only> <urn:tag> \"seed\" }");
    load_rows(&store, "ex", 1200, 64);

    let pool = QueryPool::new(16 * KB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));
    let mut def = ViewDef::table(
        "SELECT ?s WHERE { ?s <urn:tag> \"seed\" FILTER(EXISTS { SELECT (COUNT(?x) AS ?c) WHERE { ?x <urn:p> ?y } GROUP BY ?x } || false) }",
    );
    def.memory_bytes = 32 * MB;
    rt.put(&store, "single_outer_exists", def).unwrap();
    rt.round(&store);

    let info = rt.get(&store, "single_outer_exists").unwrap();
    assert!(
        matches!(&info.state, ViewState::Suspended { reason } if reason.contains("over-budget")),
        "1-row outer view with oversized EXISTS subquery must fail over-budget via Pass::ctx child_parent: {:?}",
        info.state
    );
    assert_eq!(pool.granted(), 0);
}

#[test]
fn reentrant_waker_during_sync_inference_discard_and_close_checkpoint_panic() {
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

    let store = Arc::new(Store::in_memory(StoreOptions::default()));
    load_rows(&store, "rw", 2000, 32);

    let pool = QueryPool::new(16 * MB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    let host = Arc::new(SwitchHost(Mutex::new(None)));
    rt.set_host(host.clone());

    // 1. Reentrant waker during `discard_through` + `collect_internal`.
    let acc = rt
        .attach_internal(
            &store,
            InternalSpec::accumulator("ic_wake", "SELECT ?s ?o WHERE { ?s <urn:p> ?o }", vec!["s"]),
            InternalAccess::everything(),
        )
        .unwrap();
    let mut big = String::from("INSERT DATA { ");
    for i in 0..2500 {
        big.push_str(&format!(
            "<urn:wk:{}:{i}> <urn:p> \"v\" . ",
            "w".repeat(300)
        ));
    }
    big.push('}');
    upd(&store, &big);
    rt.round(&store);
    assert!(pool.granted() > MB);

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
            let _ = rt_cb.list(&store_cb);
            let _ = rt_cb.internal_count();
            woke_cb.fetch_add(1, Ordering::SeqCst);
        })),
    );
    acc.discard_through(store.snapshot().commit);
    drop(acc);
    rt.round(&store);
    assert!(
        woke.load(Ordering::SeqCst) >= 1,
        "waker must run without deadlock during discard_through / collect_internal"
    );
    drop(waiter);
    drop(filler);
    assert_eq!(pool.granted(), 0);

    // 2. Reentrant waker during `sync_inference` (triggered via early-`?` `rt.get`).
    rt.put(
        &store,
        "v_sw",
        ViewDef::table("SELECT DISTINCT ?s ?o WHERE { ?s <urn:p> ?o }"),
    )
    .unwrap();
    rt.round(&store);
    let sw_baseline = pool.granted();
    assert!(sw_baseline > 0);
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
            let _ = rt2_cb.list(&store2_cb);
            woke2_cb.fetch_add(1, Ordering::SeqCst);
        })),
    );
    *host.0.lock() = Inference::new(
        Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
        Vec::new(),
    );
    // Calling `get` on an unknown view triggers `sync_for_read` AND returns early via `?`,
    // releasing old operator credit outside `reg.lock()` and waking `waiter2` cleanly.
    let _ = rt.get(&store, "nonexistent");
    rt.close(&store);
    assert!(
        woke2.load(Ordering::SeqCst) >= 1,
        "reentrant waker during sync_inference early-return / close must run without deadlock"
    );
    drop(waiter2);
    drop(filler2);
    assert_eq!(pool.granted(), 0);

    // 3. `Runtime::close` checkpoint panic still tears down and releases all pool credit.
    let dir = tempfile::tempdir().unwrap();
    let disk_store = Store::open(dir.path(), StoreOptions::default()).unwrap();
    load_rows(&disk_store, "cp", 500, 32);
    let pool3 = QueryPool::new(16 * MB);
    let rt3 = Runtime::open_with_pool(
        Some(dir.path().to_path_buf()),
        disk_store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool3.clone()),
    );
    rt3.set_host(Arc::new(OpenHost));
    rt3.put(
        &disk_store,
        "cp_view",
        ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o }"),
    )
    .unwrap();
    rt3.round(&disk_store);
    assert!(pool3.granted() > 0);

    disk_store.set_failpoint(
        "view-checkpoint-written",
        Some(Arc::new(|_| panic!("simulated checkpoint panic"))),
    );
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt3.close(&disk_store);
    }));
    assert!(
        panicked.is_err(),
        "expected failpoint panic during close checkpoint"
    );
    assert_eq!(
        pool3.granted(),
        0,
        "Runtime::close must release all pool credit even if checkpoint panics"
    );
}

/// Proves that an incomplete-delta reset of a grouped view with > LIVE_SLACK (1 MiB) of
/// old operator state and a tiny result succeeds inside a tight `QueryPool` by recycling
/// detached credit from `BudgetState::retired` instead of stranding it until round end.
#[test]
fn tight_pool_incomplete_delta_reset_recycles_retired_operator_credit_above_live_slack() {
    use sparkles_core::incr::circuit::{Circuit, DeltaSet};
    use sparkles_core::incr::compile::compile;
    use sparkles_core::sparql::parse_query;
    use sparkles_core::task::Control;

    let store = Store::in_memory(StoreOptions::default());
    // 48,000 rows across 8 groups with `GROUP_CONCAT` (`AState::Rows`) -> > 1.6 MiB of
    // Group operator state (well above LIVE_SLACK = 1 MiB) and an 8-row tiny result.
    for batch in 0..24 {
        let mut insert = String::from("INSERT DATA { ");
        for i in 0..2000 {
            let id = batch * 2000 + i;
            let g = id % 8;
            insert.push_str(&format!(
                "<urn:gr:{id}> <urn:g> <urn:grp:{g}> ; <urn:o> \"val-{id}\" . "
            ));
        }
        insert.push('}');
        upd(&store, &insert);
    }

    let q_str =
        "SELECT ?g (GROUP_CONCAT(?o) AS ?all) WHERE { ?s <urn:g> ?g . ?s <urn:o> ?o } GROUP BY ?g";
    let q = parse_query(q_str, None, &[]).unwrap();

    // Measure steady-state bytes and peak on a generous pool.
    let probe_pool = QueryPool::new(64 * MB);
    let mut probe = Circuit::new(compile(&q, None).unwrap(), None);
    probe.set_pool(Some(probe_pool.clone()), Some(probe_pool.work_parent()));
    probe.build(store.snapshot(), &Control::none()).unwrap();
    let steady_bytes = probe.bytes();
    assert!(
        steady_bytes > 1_500_000,
        "grouped view operator state must exceed LIVE_SLACK (1 MiB), got {steady_bytes}"
    );
    let steady_granted = probe_pool.granted();
    drop(probe);
    assert_eq!(probe_pool.granted(), 0);

    // Build on a tight pool (~1.45× steady-state): enough for the old tiny result plus
    // the full replacement pass, but strictly less than 2× steady-state (which would fail
    // if `shrink_state_to(0)` stranded > 1 MiB in `BudgetState::retired`).
    let tight_cap = ((steady_granted as f64) * 1.45) as u64;
    let pool = QueryPool::new(tight_cap);
    let mut circ = Circuit::new(compile(&q, None).unwrap(), None);
    circ.set_pool(Some(pool.clone()), Some(pool.work_parent()));
    circ.build(store.snapshot(), &Control::none()).unwrap();
    assert!(pool.granted() > MB);

    // Apply an incomplete-delta reset directly (simulating a bulk commit or gap reset
    // without draining `retired` between `shrink_state_to(0)` and `root.init`).
    let mut incomplete = DeltaSet::at(store.snapshot());
    incomplete.complete = false;
    circ.advance(&incomplete, &Control::none())
        .expect("incomplete-delta reset must recycle retired >1 MiB operator credit in tight pool");
    drop(circ);
    assert_eq!(pool.granted(), 0);
}

/// Proves that evicting > LIVE_SLACK (1 MiB) of older tally commits under a tight
/// `QueryPool` recycles the detached credit from `BudgetState::retired` so the incoming
/// commit fits instead of failing spuriously.
#[test]
fn tight_pool_tally_eviction_recycles_retired_credit_above_live_slack() {
    let store = Store::in_memory(StoreOptions::default());
    // Pool is 2.4 MiB: Commits 1a and 1b together reserve ~1.5 MiB (> LIVE_SLACK) of
    // tally keys; Commit 2 needs ~1.1 MiB peak scratch + tally, which requires evicting
    // Commits 1a and 1b (> 1 MiB detached into `retired`) and recycling that credit
    // inside the same locked `round_internal`.
    let pool = QueryPool::new(2400 * KB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));

    let mut spec = InternalSpec::accumulator(
        "ic_tight_evict",
        "SELECT ?s ?o WHERE { ?s <urn:p> ?o }",
        vec!["s"],
    );
    spec.max_memory_bytes = 2200 * KB;
    let acc = rt
        .attach_internal(&store, spec, InternalAccess::everything())
        .unwrap();

    for batch in 0..2 {
        let mut c1_upd = String::from("INSERT DATA { ");
        for i in 0..1000 {
            c1_upd.push_str(&format!(
                "<urn:ev1:{batch}:{}:{i}> <urn:p> \"v\" . ",
                "a".repeat(300)
            ));
        }
        c1_upd.push('}');
        upd(&store, &c1_upd);
        rt.round(&store);
    }
    let c1 = store.snapshot().commit;
    assert!(
        pool.granted() > MB,
        "Commits 1a+1b tally must exceed LIVE_SLACK (1 MiB), got {}",
        pool.granted()
    );

    let mut c2_upd = String::from("INSERT DATA { ");
    for i in 0..950 {
        c2_upd.push_str(&format!(
            "<urn:ev2:{}:{i}> <urn:p> \"v\" . ",
            "b".repeat(300)
        ));
    }
    c2_upd.push('}');
    upd(&store, &c2_upd);
    rt.round(&store);

    // Commits 1a+1b were evicted and their >1 MiB retired credit recycled so Commit 2 is
    // counted cleanly without failing the handle or dropping Commit 2.
    let counts = acc
        .counts(c1)
        .expect("accumulator must remain healthy after tight-pool >1 MiB tally eviction");
    assert!(
        counts.unknown.is_none(),
        "Commit 2 counts must be retained via recycled retired credit, got unknown={:?}",
        counts.unknown
    );
    assert_eq!(counts.keys.len(), 950);

    drop(acc);
    rt.close(&store);
    assert_eq!(pool.granted(), 0);
}

/// Proves that an oversized count batch on a healthy accumulator records the commit as
/// unavailable (F13 §3.6) instead of failing the circuit, and subsequent small commits
/// continue counting cleanly.
#[test]
fn oversized_accumulator_count_batch_marks_commit_unavailable_without_failing_handle() {
    let store = Store::in_memory(StoreOptions::default());
    let pool = QueryPool::new(16 * MB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));

    let mut spec = InternalSpec::accumulator(
        "ic_oversized_counts",
        "SELECT ?s ?o WHERE { ?s <urn:p> ?o }",
        vec!["s"],
    );
    spec.max_memory_bytes = 48 * KB;
    let acc = rt
        .attach_internal(&store, spec, InternalAccess::everything())
        .unwrap();

    // Commit 1: small update fits easily and is counted.
    upd(&store, "INSERT DATA { <urn:small:1> <urn:p> \"a\" }");
    rt.round(&store);
    let c1 = store.snapshot().commit;
    let r1 = acc.counts(0).unwrap();
    assert!(r1.unknown.is_none());
    assert_eq!(r1.keys.len(), 1);

    // Commit 2: compact BGP output fits, but decoding 400 long distinct IRI keys exceeds
    // the 48 KiB local cap -> commit 2's counts are dropped/marked unavailable, handle
    // stays healthy.
    let mut big = String::from("INSERT DATA { ");
    for i in 0..400 {
        big.push_str(&format!(
            "<urn:longkey:{}:{i}> <urn:p> \"v\" . ",
            "k".repeat(300)
        ));
    }
    big.push('}');
    upd(&store, &big);
    rt.round(&store);
    let c2 = store.snapshot().commit;
    let r2 = acc
        .counts(0)
        .expect("oversized count batch must not fail healthy accumulator handle");
    assert!(
        r2.unknown.is_some(),
        "oversized count batch must be recorded as unavailable"
    );

    // Commit 3: subsequent small commit is counted cleanly from c2 onward.
    upd(&store, "INSERT DATA { <urn:small:2> <urn:p> \"b\" }");
    rt.round(&store);
    let r3 = acc
        .counts(c2)
        .expect("accumulator must remain available after oversized count commit");
    assert!(
        r3.unknown.is_none(),
        "counts after oversized commit c2 must be exact, got unknown={:?}",
        r3.unknown
    );
    assert_eq!(r3.keys.len(), 1);
    assert!(c1 < c2);

    drop(acc);
    rt.close(&store);
    assert_eq!(pool.granted(), 0);
}

/// Proves that a zero-length property path (`*`) view pre-admits graph-wide node sets
/// under a tight `QueryPool` even when the path predicate has zero edges.
#[test]
fn zero_length_path_view_pre_admits_graph_nodes_under_tight_pool() {
    let store = Store::in_memory(StoreOptions::default());
    // Populate many terms with `<urn:other>` and ZERO `<urn:p>` edges.
    load_rows(&store, "zp", 2000, 16);

    let pool = QueryPool::new(32 * KB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));
    let mut def = ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:no_edges>* ?o }");
    def.memory_bytes = 32 * MB;
    rt.put(&store, "zero_path", def).unwrap();
    rt.round(&store);

    let info = rt.get(&store, "zero_path").unwrap();
    assert!(
        matches!(&info.state, ViewState::Suspended { reason } if reason.contains("over-budget")),
        "zero-length path view must fail over-budget during zero_build under tight pool: {:?}",
        info.state
    );
    rt.close(&store);
    assert_eq!(pool.granted(), 0);
}

/// Proves that (1) the VERY FIRST worker pass after dropping a larger live subscriber
/// enforces the remaining narrow view's ceiling even on a transient join spike whose
/// retained result is 1 row, and (2) `Runtime::fail` lowers `local_limit` after draining
/// live subscribers so a subsequent `rebuild` enforces the narrow view's cap.
#[test]
fn live_subscriber_drop_and_runtime_fail_lower_shared_circuit_limit() {
    use sparkles_core::incr::LiveOptions;

    let store = Store::in_memory(StoreOptions::default());
    upd(
        &store,
        "INSERT DATA { <urn:l0> <urn:lk> <urn:k> . <urn:r0> <urn:rk> <urn:k> }",
    );

    let pool = QueryPool::new(32 * MB);
    let rt = Runtime::open_with_pool(
        None,
        store.dataset_id(),
        RuntimeOptions::default(),
        Some(pool.clone()),
    );
    rt.set_host(Arc::new(OpenHost));

    let q = "SELECT (COUNT(*) AS ?c) WHERE { { SELECT ?l ?k WHERE { ?l <urn:lk> ?k } } { SELECT ?r ?k WHERE { ?r <urn:rk> ?k } } }";
    let mut narrow = ViewDef::table(q);
    narrow.memory_bytes = 256 * KB;
    rt.put(&store, "narrow_v", narrow).unwrap();
    rt.round(&store);
    assert!(matches!(
        rt.get(&store, "narrow_v").unwrap().state,
        ViewState::Current
    ));

    // Attach a wide live subscriber sharing the same circuit (raising local_limit to 16 MiB).
    let wide_live = rt
        .live(
            &store,
            q,
            None,
            &LiveOptions {
                max_memory_bytes: Some(16 * MB),
                ..LiveOptions::default()
            },
        )
        .unwrap();
    // Drop the wide live subscriber BEFORE the next commit.
    drop(wide_live);

    // Apply a commit causing a large transient Cartesian join (> 1 MiB) while the grouped
    // result stays 1 row. The VERY FIRST worker pass must prune the departed subscriber
    // and enforce `narrow_v`'s 64 KiB cap mid-pass.
    let mut cross = String::from("INSERT DATA { ");
    for i in 1..200 {
        cross.push_str(&format!(
            "<urn:l{i}> <urn:lk> <urn:k> . <urn:r{i}> <urn:rk> <urn:k> . "
        ));
    }
    cross.push('}');
    upd(&store, &cross);
    rt.round(&store);

    let info = rt.get(&store, "narrow_v").unwrap();
    assert!(
        matches!(&info.state, ViewState::Suspended { reason } if reason.contains("over-budget")),
        "first pass after dropping larger live subscriber must enforce narrow view's 64 KiB cap: {:?}",
        info.state
    );

    // Part 2: `Runtime::fail` must recompute `local_limit` after draining `st.subs`, so a
    // subsequent `rebuild` enforces the remaining named view's narrow cap.
    let store2 = Store::in_memory(StoreOptions::default());
    load_rows(&store2, "rf", 20, 16);
    let pool2 = QueryPool::new(16 * MB);
    let rt2 = Runtime::open_with_pool(
        None,
        store2.dataset_id(),
        RuntimeOptions::default(),
        Some(pool2.clone()),
    );
    rt2.set_host(Arc::new(OpenHost));
    let q2 = "SELECT ?s ?o WHERE { ?s <urn:p> ?o }";
    let mut narrow2 = ViewDef::table(q2);
    narrow2.memory_bytes = 24 * KB;
    rt2.put(&store2, "narrow2", narrow2).unwrap();
    rt2.round(&store2);

    let _wide2 = rt2
        .live(
            &store2,
            q2,
            None,
            &LiveOptions {
                max_memory_bytes: Some(8 * MB),
                ..LiveOptions::default()
            },
        )
        .unwrap();
    // Force `Runtime::fail` by exhausting `QueryPool` on an update.
    let blocker = pool2
        .work_parent()
        .reserve(pool2.capacity() - pool2.granted())
        .unwrap();
    load_rows(&store2, "rf_more", 600, 32);
    rt2.round(&store2);
    drop(blocker);
    // `Runtime::fail` drained `_wide2`. Now rebuild `narrow2`: `narrow2`'s 24 KiB cap
    // must be enforced (not `_wide2`'s departed 8 MiB cap).
    rt2.rebuild("narrow2").unwrap();
    rt2.round(&store2);
    let info2 = rt2.get(&store2, "narrow2").unwrap();
    assert!(
        matches!(&info2.state, ViewState::Suspended { reason } if reason.contains("over-budget")),
        "rebuild after Runtime::fail must enforce narrow view's 24 KiB cap, got {:?}",
        info2.state
    );

    rt.close(&store);
    rt2.close(&store2);
    assert_eq!(pool.granted(), 0);
    assert_eq!(pool2.granted(), 0);
}
