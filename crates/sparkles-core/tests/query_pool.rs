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
    let s = Store::in_memory(StoreOptions::default());
    let mut body = String::new();
    for i in 0..1_000 {
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
    assert!(batch.len() > 0);
    drop(c);
    // a batch kept past the cursor's close keeps its share until it drops
    drop(batch);
    assert_eq!(pool.granted(), 0);
}
