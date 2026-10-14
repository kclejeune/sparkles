//! The query memory pool (spec X08 §4): a query's grant follows its charges, fails with
//! the server-memory budget when the pool is short, and returns to the pool when the
//! query ends.

use sparkles_core::sparql::pool::QueryPool;
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};

const MB: u64 = 1 << 20;

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
