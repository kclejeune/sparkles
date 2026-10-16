//! The query memory pool (spec X08 §4): a query's grant follows its charges, fails with
//! the server-memory budget when the pool is short, and returns to the pool when the
//! query ends.

use sparkles_core::sparql::pool::QueryPool;
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};

const MB: u64 = 1 << 20;

#[cfg(feature = "failpoints")]
#[test]
fn algorithm_cursor_fallback_keeps_its_table_admitted_during_transfer() {
    use sparkles_core::algo::{ProjectionEvent, set_projection_observer};
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    use std::sync::{Arc, Mutex};
    let s = Store::in_memory(StoreOptions::default());
    let body = (0..1_000)
        .map(|i| format!("<urn:n{i}> <urn:edge> <urn:n{}> .", i + 1))
        .collect::<String>();
    update(
        &s,
        &format!("INSERT DATA {{ {body} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let pool = QueryPool::new(64 * MB);
    let grant = Arc::new(pool.try_grant(0).unwrap());
    let observations = Arc::new(Mutex::new(Vec::new()));
    let rows = observations.clone();
    let observed_grant = grant.clone();
    set_projection_observer(Some(Arc::new(move |event| {
        if let ProjectionEvent::Materialized("algorithm-fallback-transfer", capacity) = event {
            rows.lock()
                .unwrap()
                .push((capacity as u64, observed_grant.used()));
        }
    })));
    let mut cursor = select_cursor(s.snapshot(), "PREFIX algo: <urn:x-sparkles:algo#> SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate <urn:edge>; algo:node ?n; algo:component ?c } }", &QueryOptions { grant: Some(grant.clone()), no_cache: true, ..Default::default() }, &CursorOptions { batch_rows: 16, fallback: FallbackPolicy::AllowMaterialization, ..Default::default() }).unwrap();
    let batch = cursor.next_batch().unwrap().unwrap();
    set_projection_observer(None);
    let observed = observations.lock().unwrap().clone();
    drop(cursor);
    assert_eq!(batch.len(), 16);
    drop(batch);
    assert!(
        !observed.is_empty(),
        "actual fallback transfer must be reached"
    );
    assert!(
        observed.iter().all(|(bytes, used)| used >= bytes),
        "algorithm table lost its reservation at transfer: {observed:?}"
    );
    assert_eq!(grant.used(), 0);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[cfg(feature = "failpoints")]
#[test]
fn algorithm_allocations_are_admitted_by_the_query_grant_before_allocation() {
    use sparkles_core::algo::{ProjectionEvent, set_projection_observer};
    use std::sync::{Arc, Mutex};
    let s = Store::in_memory(StoreOptions::default());
    let body = (0..60_000)
        .map(|i| format!("<urn:n{i}> <urn:edge> <urn:n{}> .", i + 1))
        .collect::<String>();
    update(
        &s,
        &format!("INSERT DATA {{ {body} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let pool = QueryPool::new(4 * MB);
    let grant = Arc::new(pool.try_grant(0).unwrap());
    let observations = Arc::new(Mutex::new(Vec::new()));
    let events = observations.clone();
    let observed_grant = grant.clone();
    set_projection_observer(Some(Arc::new(move |event| {
        if let ProjectionEvent::Allocation(name, capacity, bytes, native_live) = event {
            events.lock().unwrap().push((
                name,
                capacity,
                bytes,
                native_live,
                observed_grant.used(),
                observed_grant.granted(),
            ));
        }
    })));
    let result = query(
        s.snapshot(),
        "PREFIX algo: <urn:x-sparkles:algo#> SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate <urn:edge>; algo:node ?n; algo:component ?c } }",
        &QueryOptions {
            grant: Some(grant.clone()),
            max_memory_bytes: Some(128 * MB),
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(30)),
            no_cache: true,
            ..Default::default()
        },
    );
    set_projection_observer(None);
    let rows = observations.lock().unwrap();
    assert!(!rows.is_empty(), "must observe real allocation boundaries");
    let uncovered: Vec<_> = rows.iter().filter(|r| r.4 < r.3 || r.5 < r.3).collect();
    assert!(
        uncovered.is_empty(),
        "native allocation capacity was not admitted through query owner: {uncovered:?}"
    );
    assert!(matches!(result, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::ServerMemory));
    assert_eq!(grant.used(), 0);
    drop(rows);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[cfg(feature = "failpoints")]
#[test]
fn algorithm_warmed_public_cursor_preserves_cache_policy_and_admission_handoff() {
    use sparkles_core::algo::{ProjectionEvent, set_projection_observer};
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    use std::sync::{Arc, Mutex};
    let s = Store::in_memory(StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    s.load(&[Source::from_bytes(
        b"<urn:a> <urn:p> <urn:b>. <urn:c> <urn:p> <urn:d>.".to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let q = "PREFIX algo:<urn:x-sparkles:algo#> SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate <urn:p>;algo:node ?n;algo:component ?c } }";
    let pool = QueryPool::new(64 * MB);
    let grant = Arc::new(pool.empty_grant());
    let options = QueryOptions {
        grant: Some(grant.clone()),
        ..Default::default()
    };
    let first = query(s.snapshot(), q, &options).unwrap();
    let expected = first.rows();
    let weak = Arc::downgrade(&first.ctx);
    drop(first);
    assert!(weak.upgrade().is_none());
    assert_eq!(grant.used(), 0);
    let hit = query(s.snapshot(), q, &options).unwrap();
    assert!(hit.plan.cached);
    assert_eq!(hit.rows(), expected);
    drop(hit);
    assert_eq!(grant.used(), 0);
    let evidence = Arc::new(Mutex::new(Vec::new()));
    let seen = evidence.clone();
    let observed = grant.clone();
    set_projection_observer(Some(Arc::new(move |event| {
        if let ProjectionEvent::Materialized("algorithm-fallback-transfer", capacity) = event {
            seen.lock()
                .unwrap()
                .push((capacity as u64, observed.used()));
        }
    })));
    let hits = s.result_cache().hits();
    let mut cursor = select_cursor(
        s.snapshot(),
        q,
        &options,
        &CursorOptions {
            batch_rows: 2,
            fallback: FallbackPolicy::AllowMaterialization,
            ..Default::default()
        },
    )
    .unwrap();
    let retained = cursor.next_batch().unwrap().unwrap();
    set_projection_observer(None);
    assert_eq!(
        s.result_cache().hits(),
        hits,
        "public cursor preserves its existing no-result-cache policy"
    );
    assert_eq!(retained.len(), 2);
    drop(cursor);
    assert!(grant.used() > 0);
    assert!(retained.row(0).unwrap().iter().all(Option::is_some));
    drop(retained);
    let rows = evidence.lock().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].1 >= rows[0].0,
        "warm cached output has a release/admit gap: {rows:?}"
    );
    assert_eq!(grant.used(), 0);
}

#[cfg(feature = "failpoints")]
#[test]
fn algorithm_actual_service_boundary_preserves_cancel_timeout_and_unwind_cleanup() {
    use sparkles_core::algo::{ProjectionEvent, set_projection_observer};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};
    let s = Store::in_memory(Default::default());
    update(
        &s,
        "INSERT DATA {<urn:a> <urn:p> <urn:b>}",
        &Default::default(),
    )
    .unwrap();
    let q = "PREFIX algo:<urn:x-sparkles:algo#> SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate <urn:p>;algo:concurrency 1;algo:node ?n;algo:component ?c } }";
    for mode in 0..3 {
        let pool = QueryPool::new(64 * MB);
        let grant = Arc::new(pool.empty_grant());
        let cancel = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(AtomicBool::new(false));
        let seen = entered.clone();
        let flag = cancel.clone();
        let deadline = Instant::now() + Duration::from_secs(2);
        set_projection_observer(Some(Arc::new(move |event| {
            if let ProjectionEvent::Allocation("algorithm-pick", n, _, _) = event
                && n > 0
            {
                seen.store(true, Ordering::Release);
                match mode {
                    0 => flag.store(true, Ordering::Release),
                    1 => {
                        while Instant::now() <= deadline {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                    }
                    _ => panic!("actual service allocation boundary unwind"),
                }
            }
        })));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            query(
                s.snapshot(),
                q,
                &QueryOptions {
                    grant: Some(grant.clone()),
                    cancel: Some(cancel),
                    deadline: (mode == 1).then_some(deadline),
                    no_cache: true,
                    ..Default::default()
                },
            )
        }));
        set_projection_observer(None);
        assert!(
            entered.load(Ordering::Acquire),
            "must reach actual algorithm/service boundary before refusal"
        );
        match mode {
            0 => assert!(matches!(result.unwrap(), Err(Error::Cancelled))),
            1 => assert!(matches!(result.unwrap(), Err(Error::Timeout))),
            _ => assert!(result.is_err()),
        }
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }
}

#[cfg(feature = "failpoints")]
#[test]
fn algorithm_sorted_cursor_batches_survive_early_close_and_cancellation() {
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let s = Store::in_memory(Default::default());
    let body = (0..64)
        .map(|i| format!("<urn:n{i}> <urn:p> <urn:n{}>.", i + 1))
        .collect::<String>();
    update(&s, &format!("INSERT DATA {{{body}}}"), &Default::default()).unwrap();
    for empty in [false, true] {
        for cancel_after_first in [false, true] {
            let q = format!(
                "PREFIX algo:<urn:x-sparkles:algo#> SELECT ?n ?c WHERE {{SERVICE algo:wcc {{[] algo:predicate <urn:p>;algo:node ?n;algo:component ?c}} {}}} ORDER BY DESC(?n)",
                if empty { "FILTER(false)" } else { "" }
            );
            let pool = QueryPool::new(64 * MB);
            let grant = Arc::new(pool.empty_grant());
            let cancel = Arc::new(AtomicBool::new(false));
            let opts = QueryOptions {
                grant: Some(grant.clone()),
                cancel: Some(cancel.clone()),
                no_cache: true,
                ..Default::default()
            };
            let mut cursor = select_cursor(
                s.snapshot(),
                &q,
                &opts,
                &CursorOptions {
                    batch_rows: 3,
                    fallback: FallbackPolicy::AllowMaterialization,
                    ..Default::default()
                },
            )
            .unwrap();
            let retained = cursor.next_batch().unwrap();
            if empty {
                assert!(retained.is_none());
            } else {
                assert_eq!(retained.as_ref().unwrap().len(), 3);
                if cancel_after_first {
                    cancel.store(true, Ordering::Release);
                    assert!(matches!(cursor.next_batch(), Err(Error::Cancelled)));
                }
            }
            drop(cursor);
            if let Some(batch) = &retained {
                assert!(grant.used() > 0);
                assert!(batch.row(0).unwrap().iter().all(Option::is_some));
            }
            drop(retained);
            assert_eq!(grant.used(), 0);
            drop(opts);
            drop(grant);
            assert_eq!(pool.granted(), 0);
        }
    }
}

#[cfg(feature = "failpoints")]
#[test]
fn algorithm_concurrent_contexts_keep_separate_or_shared_grant_ownership() {
    use sparkles_core::algo::{ProjectionEvent, set_projection_observer};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    let s = Store::in_memory(Default::default());
    update(
        &s,
        "INSERT DATA {<urn:a> <urn:p> <urn:b>}",
        &Default::default(),
    )
    .unwrap();
    let q = "PREFIX algo:<urn:x-sparkles:algo#> SELECT ?n ?c WHERE {SERVICE algo:wcc {[] algo:predicate <urn:p>;algo:node ?n;algo:component ?c}}";
    for shared in [false, true] {
        let pool = QueryPool::new(64 * MB);
        let a = Arc::new(pool.empty_grant());
        let b = if shared {
            a.clone()
        } else {
            Arc::new(pool.empty_grant())
        };
        let (tx, rx) = mpsc::channel();
        let mut joins = Vec::new();
        let mut release = Vec::new();
        for (id, grant) in [&a, &b].into_iter().enumerate() {
            let grant = grant.clone();
            let snap = s.snapshot();
            let tx = tx.clone();
            let (go, wait) = mpsc::channel();
            release.push(go);
            joins.push(std::thread::spawn(move || {
                let observed = grant.clone();
                set_projection_observer(Some(Arc::new(move |event| {
                    if let ProjectionEvent::Allocation(_, _, _, live) = event {
                        assert!(observed.used() >= live);
                    }
                })));
                let result = query(
                    snap,
                    q,
                    &QueryOptions {
                        grant: Some(grant),
                        no_cache: true,
                        ..Default::default()
                    },
                )
                .unwrap();
                set_projection_observer(None);
                assert_eq!(result.rows().len(), 2);
                let used = result.ctx.mem_peak();
                tx.send((id, used)).unwrap();
                wait.recv_timeout(Duration::from_secs(10)).unwrap();
                result
            }));
        }
        drop(tx);
        let x = rx.recv_timeout(Duration::from_secs(10));
        let y = rx.recv_timeout(Duration::from_secs(10));
        for go in release {
            let _ = go.send(());
        }
        let mut results = joins
            .into_iter()
            .map(|j| j.join().unwrap())
            .collect::<Vec<_>>();
        let mut peaks = [0; 2];
        for (id, used) in [x.unwrap(), y.unwrap()] {
            peaks[id] = used;
        }
        assert!(peaks.iter().all(|p| *p > 0));
        if shared {
            assert_eq!(a.used(), peaks.iter().sum::<u64>());
        } else {
            assert_eq!((a.used(), b.used()), (peaks[0], peaks[1]));
        }
        drop(results.remove(0));
        assert!(b.used() > 0);
        drop(results);
        assert_eq!((a.used(), b.used()), (0, 0));
        drop((a, b));
        assert_eq!(pool.granted(), 0);
    }
}

fn store() -> Store {
    store_rows(1_000)
}

fn store_rows(rows: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut body = String::new();
    for i in 0..rows {
        body.push_str(&format!("<urn:s{i}> <urn:p> {i} .\n"));
    }
    update(
        &s,
        &format!("INSERT DATA {{ {body} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    s
}

/// A million rows of three columns.
const CROSS: &str = "SELECT ?a ?b ?x ?y WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y } ORDER BY ?x ?y";

#[test]
fn a_query_grows_its_grant_and_returns_it() {
    let s = store();
    let pool = QueryPool::new(1024 * MB);
    let opts = QueryOptions {
        pool: Some(pool.clone()),
        ..Default::default()
    };
    let n = query(s.snapshot(), CROSS, &opts).unwrap().rows().len();
    assert_eq!(n, 1_000_000);
    assert_eq!(pool.granted(), 0, "the grant returns with the query");
}

#[test]
fn a_short_pool_fails_the_query_with_server_memory() {
    let s = store();
    let pool = QueryPool::new(16 * MB);
    let opts = QueryOptions {
        pool: Some(pool.clone()),
        ..Default::default()
    };
    match query(s.snapshot(), CROSS, &opts) {
        Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::ServerMemory),
        Err(e) => panic!("{e}"),
        Ok(r) => panic!("{} rows", r.rows().len()),
    }
    assert_eq!(pool.granted(), 0);
    // the query's own budget still answers first when it is lower
    let opts = QueryOptions {
        pool: Some(QueryPool::new(1024 * MB)),
        max_memory_bytes: Some(8 * MB),
        ..Default::default()
    };
    match query(s.snapshot(), CROSS, &opts) {
        Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::Memory),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("fit in 8 MiB"),
    }
}

#[test]
fn two_queries_share_the_pool() {
    let s = store();
    let pool = QueryPool::new(96 * MB);
    let grant = std::sync::Arc::new(pool.try_grant(80 * MB).unwrap());
    let opts = QueryOptions {
        pool: Some(pool.clone()),
        ..Default::default()
    };
    // the first holds most of the pool, so the second cannot grow
    assert!(matches!(
        query(s.snapshot(), CROSS, &opts),
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::ServerMemory
    ));
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn an_update_uses_the_grant_it_is_given() {
    let s = store();
    let pool = QueryPool::new(1024 * MB);
    let grant = std::sync::Arc::new(pool.try_grant(16 * MB).unwrap());
    let opts = QueryOptions {
        grant: Some(grant.clone()),
        ..Default::default()
    };
    update(
        &s,
        "INSERT { ?a <urn:q> ?x } WHERE { ?a <urn:p> ?x }",
        &opts,
    )
    .unwrap();
    assert!(pool.granted() >= 16 * MB);
    drop(opts);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn a_cursor_returns_its_grant_when_it_is_dropped() {
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    let s = store();
    let pool = QueryPool::new(1024 * MB);
    let opts = QueryOptions {
        pool: Some(pool.clone()),
        ..Default::default()
    };
    let cursor = CursorOptions {
        batch_rows: 1024,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let mut c = select_cursor(
        s.snapshot(),
        "SELECT ?a ?x WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y }",
        &opts,
        &cursor,
    )
    .unwrap();
    let batch = c.next_batch().unwrap().unwrap();
    assert!(!batch.is_empty());
    drop(c);
    // a batch kept past the cursor's close keeps its share until it drops
    drop(batch);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn a_cursor_grant_still_shrinks() {
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    let s = store();
    let pool = QueryPool::new(1024 * MB);
    let opts = QueryOptions {
        pool: Some(pool.clone()),
        ..Default::default()
    };
    let cursor = CursorOptions {
        batch_rows: 1024,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let mut c = select_cursor(
        s.snapshot(),
        "SELECT ?a ?x WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y }",
        &opts,
        &cursor,
    )
    .unwrap();
    let mut rows = 0;
    while let Some(batch) = c.next_batch().unwrap() {
        rows += batch.len();
    }
    assert_eq!(rows, 1_000_000);
    // cursor grants must not pin their peak
    assert!(pool.granted() <= 32 * MB, "{} granted", pool.granted());
    drop(c);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn an_update_where_clause_is_bounded_by_the_pool() {
    let s = store();
    let pool = QueryPool::new(MB);
    let opts = QueryOptions {
        pool: Some(pool.clone()),
        ..Default::default()
    };
    match update(
        &s,
        "DELETE { ?a <urn:none> ?b } WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y }",
        &opts,
    ) {
        Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::ServerMemory),
        Err(e) => panic!("{e}"),
        Ok(stats) => panic!("succeeded with a peak of {} bytes", stats.mem_peak_bytes),
    }
    assert_eq!(pool.granted(), 0);
}

#[test]
fn retained_eager_results_keep_their_grant() {
    let s = store();
    let pool = QueryPool::new(128 * MB);
    let opts = QueryOptions {
        pool: Some(pool.clone()),
        ..Default::default()
    };
    let mut held = Vec::new();
    let mut stopped = None;
    for _ in 0..5 {
        match query(
            s.snapshot(),
            "SELECT ?a ?b ?x ?y WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y }",
            &opts,
        ) {
            Ok(r) => held.push(r),
            Err(Error::BudgetExceeded(b)) => {
                stopped = Some(b.kind);
                break;
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(
        stopped,
        Some(BudgetKind::ServerMemory),
        "{} results kept",
        held.len()
    );
    let peaks: u64 = held.iter().map(|r| r.mem_peak_bytes).sum();
    assert!(
        pool.granted() >= peaks,
        "{} granted for {peaks} bytes of peaks",
        pool.granted()
    );
    drop(held);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn retained_eager_results_add_their_contributions_on_one_explicit_grant() {
    let s = store_rows(800);
    let pool = QueryPool::new(64 * MB);
    let grant = std::sync::Arc::new(pool.try_grant(16 * MB).unwrap());
    let opts = QueryOptions {
        grant: Some(grant.clone()),
        ..Default::default()
    };
    let q = "SELECT ?a ?b ?x ?y WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y }";
    let mut held = Vec::new();
    let mut refused = false;
    for _ in 0..4 {
        match query(s.snapshot(), q, &opts) {
            Ok(result) => held.push(result),
            Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::ServerMemory => {
                refused = true;
                break;
            }
            Err(error) => panic!("{error}"),
        }
    }
    assert!(refused, "all four retained results fit");
    assert!(held.len() >= 2);
    let peaks: u64 = held.iter().map(|r| r.mem_peak_bytes).sum();
    let tables: u64 = held.iter().map(|r| r.table.mem_bytes()).sum();
    assert!(grant.used() >= peaks && grant.used() >= tables);
    assert!(pool.granted() >= grant.used());
    let kept = held[0].ctx.clone();
    let first = held.remove(0);
    drop(first);
    assert!(
        grant.used() >= peaks,
        "the extra context owner retains its contribution"
    );
    drop(kept);
    let again = query(s.snapshot(), q, &opts).unwrap();
    assert_eq!(again.len(), 640_000);
    for result in &held {
        assert_eq!(result.len(), 640_000);
        assert!(result.term(result.table.cols[0][0]).is_some());
    }
    drop(again);
    drop(held);
    assert_eq!(grant.used(), 0);
    drop(opts);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

fn large_graph_store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let object = "x".repeat(100_000);
    let mut body = String::new();
    for i in 0..300 {
        body.push_str(&format!("<urn:s{i}> <urn:p> \"{object}\" .\n"));
    }
    update(
        &s,
        &format!("INSERT DATA {{ {body} }}"),
        &Default::default(),
    )
    .unwrap();
    s
}

#[test]
fn retained_eager_construct_graphs_cover_their_lexical_payload() {
    let s = large_graph_store();
    for q in [
        "CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }",
        "CONSTRUCT { GRAPH <urn:g> { ?s <urn:q> ?o . ?s <urn:q> ?o } } WHERE { ?s <urn:p> ?o }",
    ] {
        let pool = QueryPool::new(128 * MB);
        let opts = QueryOptions {
            pool: Some(pool.clone()),
            ..Default::default()
        };
        let result = query(s.snapshot(), q, &opts).unwrap();
        assert_eq!(result.len(), 300);
        assert!(result.mem_peak_bytes >= 30_000_000);
        assert!(pool.granted() >= result.mem_peak_bytes);
        let ctx = result.ctx.clone();
        drop(result);
        assert!(pool.granted() >= 30_000_000);
        drop(ctx);
        assert_eq!(pool.granted(), 0);
        let short = QueryPool::new(16 * MB);
        assert!(matches!(query(s.snapshot(), q, &QueryOptions {
            pool: Some(short.clone()), ..Default::default()
        }), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::ServerMemory));
        assert_eq!(short.granted(), 0);
    }
}

#[test]
fn retained_eager_describe_is_charged_and_checks_both_memory_budgets() {
    let s = large_graph_store();
    let q = "DESCRIBE ?s WHERE { ?s <urn:p> ?o }";
    // DESCRIBE preserves its cursor estimator: encoded bytes ×8, then ×4
    // for decoded output, so 30 MB of lexical payload needs about 960 MB.
    let pool = QueryPool::new(1024 * MB);
    let result = query(
        s.snapshot(),
        q,
        &QueryOptions {
            pool: Some(pool.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.len(), 300);
    assert!(result.mem_peak_bytes >= 30_000_000);
    assert!(pool.granted() >= result.mem_peak_bytes);
    drop(result);
    assert_eq!(pool.granted(), 0);
    for local in [false, true] {
        let short = QueryPool::new(if local { 1024 * MB } else { 16 * MB });
        assert!(matches!(query(s.snapshot(), q, &QueryOptions {
            pool: Some(short.clone()),
            max_memory_bytes: local.then_some(16 * MB),
            ..Default::default()
        }), Err(Error::BudgetExceeded(b)) if b.kind == if local {
            BudgetKind::Memory
        } else { BudgetKind::ServerMemory }));
        assert_eq!(short.granted(), 0);
    }
}

#[test]
fn retained_eager_graphs_add_on_one_grant_and_release_in_different_orders() {
    let s = large_graph_store();
    // Include DESCRIBE's existing conservative decoded-output estimate.
    let pool = QueryPool::new(1024 * MB);
    let grant = std::sync::Arc::new(pool.try_grant(16 * MB).unwrap());
    let opts = QueryOptions {
        grant: Some(grant.clone()),
        ..Default::default()
    };
    let one = query(
        s.snapshot(),
        "CONSTRUCT {?s <urn:q> ?o} WHERE {?s <urn:p> ?o}",
        &opts,
    )
    .unwrap();
    let two = query(s.snapshot(), "DESCRIBE ?s WHERE {?s <urn:p> ?o}", &opts).unwrap();
    let sum = one.mem_peak_bytes + two.mem_peak_bytes;
    assert!(grant.used() >= sum);
    let context = two.ctx.clone();
    drop(two);
    assert!(grant.used() >= sum);
    drop(context);
    assert!(grant.used() >= one.mem_peak_bytes);
    assert_eq!(one.len(), 300);
    drop(one);
    assert_eq!(grant.used(), 0);
    drop(opts);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn empty_and_ask_results_release_each_distinct_grant() {
    let s = store_rows(2);
    let pool = QueryPool::new(64 * MB);
    let mut held = Vec::new();
    for q in [
        "ASK {?s <urn:p> ?o}",
        "SELECT ?s WHERE {?s <urn:missing> ?o}",
    ] {
        held.push(
            query(
                s.snapshot(),
                q,
                &QueryOptions {
                    pool: Some(pool.clone()),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
    }
    assert!(held[0].boolean);
    assert!(held[1].is_empty());
    assert!(!std::sync::Arc::ptr_eq(
        held[0].ctx.grant.as_ref().unwrap(),
        held[1].ctx.grant.as_ref().unwrap()
    ));
    drop(held);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn explicit_cursor_local_terms_and_retained_batches_release_their_ownership() {
    use sparkles_core::sparql::{CursorOptions, select_cursor};
    let s = store_rows(2);
    let pool = QueryPool::new(32 * MB);
    let grant = std::sync::Arc::new(pool.try_grant(16 * MB).unwrap());
    let opts = QueryOptions {
        grant: Some(grant.clone()),
        ..Default::default()
    };
    let lexical = "prepared".repeat(512);
    let q = format!("SELECT ?s (\"{lexical}\" AS ?label) WHERE {{?s <urn:p> ?o}}");
    let mut cursor = select_cursor(
        s.snapshot(),
        &q,
        &opts,
        &CursorOptions {
            batch_rows: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let retained = cursor.next_batch().unwrap().unwrap();
    while cursor.next_batch().unwrap().is_some() {}
    drop(cursor);
    assert!(grant.used() > 0);
    assert!(grant.granted() <= 16 * MB);
    assert_eq!(
        retained.row(0).unwrap()[1].as_ref().unwrap().to_string(),
        format!("\"{lexical}\"")
    );
    drop(retained);
    assert_eq!(grant.used(), 0);
    drop(opts);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn automatic_immutable_scan_releases_its_transient_contribution() {
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::sparql::{ExecutionMode, QueryExecution, query_execution};
    // Exercise the actual automatic threshold, without changing planner estimates.
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..1_000_000 {
        data.push_str(&format!("<urn:s{i}> <urn:p> {i} .\n"));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let pool = QueryPool::new(32 * MB);
    let grant = std::sync::Arc::new(pool.try_grant(16 * MB).unwrap());
    let opts = QueryOptions {
        grant: Some(grant.clone()),
        ..Default::default()
    };
    let execution = query_execution(
        s.snapshot(),
        "SELECT ?s ?o WHERE {?s <urn:p> ?o}",
        &opts,
        &Default::default(),
        ExecutionMode::Auto,
    )
    .unwrap();
    let QueryExecution::Select(mut cursor) = execution else {
        panic!("a million-row immutable scan must convert after preparation")
    };
    let retained = cursor.next_batch().unwrap().unwrap();
    let mut rows = retained.len();
    while let Some(batch) = cursor.next_batch().unwrap() {
        rows += batch.len();
    }
    assert_eq!(rows, 1_000_000);
    assert!(pool.granted() <= 16 * MB);
    drop(cursor);
    assert!(grant.used() > 0);
    assert!(retained.row(0).unwrap().iter().all(Option::is_some));
    drop(retained);
    assert_eq!(grant.used(), 0);
    drop(opts);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}
