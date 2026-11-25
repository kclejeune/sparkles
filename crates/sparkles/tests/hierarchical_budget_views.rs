//! Facade integration tests for incremental views wired to Dataset's QueryPool
//! (Item 675 Track 2).

use sparkles::incr::views::ViewDef;
use sparkles::sparql::pool::QueryPool;
use sparkles::store::{Store, StoreOptions};
use sparkles::{Dataset, DatasetOptions};
use std::time::Duration;

const MB: u64 = 1 << 20;

#[test]
fn dataset_views_share_query_pool_and_return_exact_zero_on_delete() {
    let pool = QueryPool::new(16 * MB);
    let ds = Dataset::from_store_with(
        Store::in_memory(StoreOptions::default()),
        DatasetOptions {
            pool: Some(pool.clone()),
            ..Default::default()
        },
    );
    let mut insert = String::from("INSERT DATA { ");
    for i in 0..400 {
        insert.push_str(&format!("<urn:d:{i}> <urn:p> \"val-{i}\" . "));
    }
    insert.push('}');
    ds.update(&insert).unwrap();

    ds.views()
        .put(
            "items",
            ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o }"),
        )
        .unwrap();
    ds.views()
        .wait_built("items", Duration::from_secs(10))
        .unwrap();
    ds.views().settle(Duration::from_secs(10));
    assert!(
        pool.granted() > 0,
        "Dataset view must reserve from Dataset's QueryPool"
    );

    // Plain query not touching views succeeds and returns its own credit cleanly.
    let before = pool.granted();
    let res = ds
        .query("SELECT (COUNT(*) AS ?c) WHERE { ?s <urn:p> ?o }")
        .unwrap();
    assert_eq!(res.len(), 1);
    drop(res);
    assert_eq!(pool.granted(), before);

    assert!(ds.views().delete("items").unwrap());
    // Ensure any worker round that held an Arc<Cell> has finished before asserting 0.
    ds.views().settle(Duration::from_secs(10));
    assert_eq!(
        pool.granted(),
        0,
        "deleting Dataset view must return QueryPool credit to 0"
    );
}
