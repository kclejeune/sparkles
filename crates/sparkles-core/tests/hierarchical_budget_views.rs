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
                "SELECT ?s ?o WHERE { ?s <urn:p> ?o }",
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

    // Pin all remaining pool room and apply a commit strictly larger than the tally's
    // existing credit (~512 KiB) so even after evicting all prior commits, the new commit
    // is refused by QueryPool.
    let filler = pool
        .work_parent()
        .reserve(pool.capacity() - pool.granted())
        .unwrap();
    let mut u3 = String::from("INSERT DATA { ");
    for i in 0..2500 {
        u3.push_str(&format!(
            "<urn:boom:{}:{i}> <urn:p> \"v\" . ",
            "z".repeat(512)
        ));
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

    // 2. Fallback (`evaluate_under`) invokes the lazy supplier once.
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

    // 2. Reentrant waker during `sync_inference` (triggered via `rt.list`).
    rt.put(
        &store,
        "v_sw",
        ViewDef::table("SELECT DISTINCT ?s ?o WHERE { ?s <urn:p> ?o }"),
    )
    .unwrap();
    rt.round(&store);
    assert!(pool.granted() > 0);
    *host.0.lock() = Inference::new(
        Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
        Vec::new(),
    );
    // Calling `get` on an unknown view triggers `sync_for_read` AND returns early via `?`.
    let _ = rt.get(&store, "nonexistent");
    rt.close(&store);
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
