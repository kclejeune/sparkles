//! Private ownership probes ported from the integration fixture. Public controls
//! remain in query_pool.rs. Raw contexts are observed only inside this unit module.
use crate::sparql::pool::QueryPool;
use crate::sparql::update::update;
use crate::sparql::{QueryOptions, query};
use crate::store::{Store, StoreOptions};
use crate::{BudgetKind, Error};
const MB: u64 = 1 << 20;
const CROSS: &str = "SELECT ?a ?b ?x ?y WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y } ORDER BY ?x ?y";
#[cfg(feature = "failpoints")]
include!("../support/fixture_rows_eq.rs");

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

#[cfg(feature = "failpoints")]
#[test]
fn algorithm_cursor_fallback_keeps_its_table_admitted_during_transfer() {
    use crate::algo::{ProjectionEvent, set_projection_observer};
    use crate::sparql::{CursorOptions, FallbackPolicy, select_cursor};
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
    use crate::algo::{ProjectionEvent, set_projection_observer};
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
    use crate::algo::{ProjectionEvent, set_projection_observer};
    use crate::io::{RdfFormat, Source};
    use crate::sparql::{CursorOptions, FallbackPolicy, select_cursor};
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
    // Independent fixture oracle; it must not retain the original query account.
    let expected: Vec<_> = first
        .rows()
        .unwrap()
        .iter()
        .map(|row| row.values().to_vec())
        .collect();
    let weak = Arc::downgrade(&first.inner.ctx);
    drop(first);
    assert!(weak.upgrade().is_none());
    assert_eq!(grant.used(), 0);
    let hit = query(s.snapshot(), q, &options).unwrap();
    assert!(hit.plan().cached);
    assert_fixture_rows_eq!(hit.rows().unwrap(), expected);
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
    assert!(
        retained
            .row(0)
            .unwrap()
            .values()
            .iter()
            .all(Option::is_some)
    );
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
    use crate::algo::{ProjectionEvent, set_projection_observer};
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
    use crate::sparql::{CursorOptions, FallbackPolicy, select_cursor};
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
                // A retained batch keeps its payload and credit after the cursor
                // closes. Decoding it is work of the query, so a cancelled query
                // refuses to decode it.
                if cancel_after_first {
                    assert!(matches!(batch.row(0), Err(Error::Cancelled)));
                } else {
                    assert!(batch.row(0).unwrap().values().iter().all(Option::is_some));
                }
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
    use crate::algo::{ProjectionEvent, set_projection_observer};
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
                assert_eq!(result.len(), 2);
                let used = result.inner.ctx.mem_peak();
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
        // A finished result keeps its own estimate, at most its query's peak, on the
        // grant it ran under (X08-P1-retained-results).
        if shared {
            assert!(a.used() > 0 && a.used() <= peaks.iter().sum::<u64>());
        } else {
            assert!(a.used() > 0 && a.used() <= peaks[0]);
            assert!(b.used() > 0 && b.used() <= peaks[1]);
        }
        drop(results.remove(0));
        assert!(b.used() > 0);
        drop(results);
        assert_eq!((a.used(), b.used()), (0, 0));
        drop((a, b));
        assert_eq!(pool.granted(), 0);
    }
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
    // A finished result keeps its table, not its query's peak (X08-P1-retained-results).
    let tables: u64 = held.iter().map(|r| r.inner.table.mem_bytes()).sum();
    assert!(grant.used() >= tables);
    assert!(pool.granted() >= grant.used());
    let used = grant.used();
    let kept = held[0].clone();
    assert!(std::sync::Arc::ptr_eq(&kept.inner, &held[0].inner));
    let first = held.remove(0);
    drop(first);
    assert_eq!(
        grant.used(),
        used,
        "the extra result owner retains its contribution"
    );
    assert_eq!(kept.len(), 640_000);
    assert!(kept.row(0).unwrap().values().iter().all(Option::is_some));
    drop(kept);
    let again = query(s.snapshot(), q, &opts).unwrap();
    assert_eq!(again.len(), 640_000);
    for result in &held {
        assert_eq!(result.len(), 640_000);
        assert!(
            result
                .term(result.inner.table.cols[0][0])
                .unwrap()
                .is_some()
        );
    }
    drop(again);
    drop(held);
    assert_eq!(grant.used(), 0);
    drop(opts);
    drop(grant);
    assert_eq!(pool.granted(), 0);
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
        // A graph result keeps its query's admitted high point on the pool until the
        // result is dropped (X14 stage 4).
        let peak = result.mem_peak_bytes;
        assert!(result.inner._graph_credit.as_ref().unwrap().bytes() >= 30_000_000);
        assert!(pool.granted() >= peak);
        let kept = result.clone();
        assert!(std::sync::Arc::ptr_eq(&kept.inner, &result.inner));
        drop(result);
        assert!(pool.granted() >= peak);
        assert_eq!(kept.triples().len() + kept.quads().len(), 300);
        let object = kept
            .triples()
            .first()
            .map(|t| &t.object)
            .or_else(|| kept.quads().first().map(|q| &q.object))
            .unwrap();
        assert!(matches!(object, oxrdf::Term::Literal(l) if l.value().len() == 100_000));
        drop(kept);
        assert_eq!(pool.granted(), 0);
        let short = QueryPool::new(16 * MB);
        assert!(matches!(query(s.snapshot(), q, &QueryOptions {
            pool: Some(short.clone()), ..Default::default()
        }), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::ServerMemory));
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
    // Each graph result keeps its query's admitted high point on the shared grant.
    let sum = one.mem_peak_bytes + two.mem_peak_bytes;
    assert!(grant.used() >= sum);
    let context = two.clone();
    assert!(std::sync::Arc::ptr_eq(&context.inner, &two.inner));
    drop(two);
    assert!(grant.used() >= sum);
    assert_eq!(context.triples().len(), 300);
    assert!(
        matches!(&context.triples()[0].object, oxrdf::Term::Literal(l) if l.value().len() == 100_000)
    );
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
        held[0].inner.ctx.grant().unwrap(),
        held[1].inner.ctx.grant().unwrap()
    ));
    drop(held);
    assert_eq!(pool.granted(), 0);
}

/// A finished eager result keeps in the pool what it holds, not its query's highest
/// estimate (X08-P1-retained-results). A query that built a large intermediate table
/// for a small answer gives the rest back when it finishes, while its result is alive.
#[test]
fn a_finished_eager_result_retains_only_what_it_holds() {
    let s = store();
    let pool = QueryPool::new(1024 * MB);
    let opts = QueryOptions {
        pool: Some(pool.clone()),
        no_cache: true,
        ..Default::default()
    };
    // a million-row product reduced to a thousand distinct values
    let small = "SELECT DISTINCT ?x WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y FILTER(?y >= 0) }";
    let result = query(s.snapshot(), small, &opts).unwrap();
    assert_eq!(result.len(), 1_000);
    assert!(result.mem_peak_bytes > 16 * MB, "{}", result.mem_peak_bytes);
    let grant = result.inner.ctx.grant().unwrap().clone();
    assert!(
        grant.used() <= 2 * MB,
        "the small result keeps {} bytes of the pool",
        grant.used()
    );
    // a large result keeps its rows
    let large = query(s.snapshot(), CROSS, &opts).unwrap();
    let rows = large.len() as u64;
    let kept = large.inner.ctx.grant().unwrap().used();
    assert!(kept >= rows * 4 * 8, "{kept} for {rows} rows");
    drop(large);
    drop(result);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

/// A FILTER that compacts its columns in place keeps their allocations, and the result
/// that owns them keeps that capacity covered in the pool.
#[test]
fn a_compacted_result_keeps_its_column_capacity_covered() {
    let s = store_rows(100_000);
    let pool = QueryPool::new(1024 * MB);
    let opts = QueryOptions {
        pool: Some(pool.clone()),
        no_cache: true,
        ..Default::default()
    };
    let q = "SELECT ?s ?v ?w { ?s <urn:p> ?v BIND(?v + 1 AS ?w) FILTER(?w < 3) }";
    let result = query(s.snapshot(), q, &opts).unwrap();
    assert_eq!(result.len(), 2);
    let capacity: u64 = result
        .inner
        .table
        .cols
        .iter()
        .map(|c| c.capacity() as u64 * 8)
        .sum();
    let used = result.inner.ctx.grant().unwrap().used();
    assert!(
        used >= capacity,
        "{used} bytes cover {capacity} bytes of columns"
    );
}

/// The blank nodes that BNODE(string) made for each solution stay in the context's
/// memo, and a held result does not give up the pool memory that covers them.
#[test]
fn a_result_keeps_its_blank_node_memo_covered() {
    let s = store_rows(100_000);
    let pool = QueryPool::new(1024 * MB);
    let opts = QueryOptions {
        pool: Some(pool.clone()),
        no_cache: true,
        ..Default::default()
    };
    let q = "SELECT (COUNT(?b) AS ?n) { ?s <urn:p> ?v BIND(BNODE(\"x\") AS ?b) }";
    let result = query(s.snapshot(), q, &opts).unwrap();
    assert_eq!(result.len(), 1);
    // Eager execution never charged the memo, and the query's peak estimate covered it
    // only in part. The finished result does not drop below either.
    let used = result.inner.ctx.grant().unwrap().used();
    let floor = result.mem_peak_bytes.min(100_000 * 32);
    assert!(
        used >= floor,
        "{used} bytes for 100,000 memo entries, at least {floor}"
    );
}

/// The EXISTS memo and key sets of a finished eager query are charged and belong to its
/// plan, which goes before the pool contribution shrinks, so a small result of a query
/// with 100,000 EXISTS keys does not keep them in the pool (item 453 stage 4).
#[test]
fn a_finished_result_gives_back_its_exists_state() {
    let s = store_rows(100_000);
    let pool = QueryPool::new(1024 * MB);
    let opts = QueryOptions {
        pool: Some(pool.clone()),
        no_cache: true,
        ..Default::default()
    };
    let q = "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER EXISTS { ?s <urn:p> ?w } }";
    let result = query(s.snapshot(), q, &opts).unwrap();
    assert_eq!(result.len(), 1);
    assert!(result.mem_peak_bytes > 4 * MB, "{}", result.mem_peak_bytes);
    let used = result.inner.ctx.grant().unwrap().used();
    assert!(
        used <= 2 * MB,
        "the one-row result keeps {used} bytes of the pool"
    );
    drop(result);
    assert_eq!(pool.granted(), 0);
}

// A raw Ctx is an account owner, not an alias of the retained graph allocation.
#[test]
fn raw_context_does_not_retain_a_freed_result_graph_contribution() {
    let s = large_graph_store();
    let pool = QueryPool::new(128 * MB);
    let result = query(
        s.snapshot(),
        "CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }",
        &QueryOptions {
            pool: Some(pool.clone()),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let graph_bytes = result.inner._graph_credit.as_ref().unwrap().bytes();
    assert!(graph_bytes >= 30_000_000);
    let ctx = result.inner.ctx.clone();
    let backing = std::sync::Arc::downgrade(&result.inner);
    let before = ctx.live_bytes();
    assert!(before >= graph_bytes);
    assert!(ctx.grant().unwrap().used() >= graph_bytes);
    drop(result);
    assert!(backing.upgrade().is_none());
    let after = ctx.live_bytes();
    assert!(
        before - after >= graph_bytes,
        "freed graph credit {graph_bytes} must leave the raw context account: {before} -> {after}"
    );
    // X14 stage 4 keeps an eager context's pool contribution at its admitted
    // high point until the context goes, so only dropping it returns the grant.
    drop(ctx);
    assert_eq!(pool.granted(), 0);
}
