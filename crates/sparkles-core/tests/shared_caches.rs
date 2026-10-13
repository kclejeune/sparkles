//! Caches that several stores share (spec X08 §3): one block and result cache for the
//! process, bytes and counters per dataset, and clearing one dataset only.

use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::caches::{CacheSizes, SharedCaches};
use sparkles_core::store::{Store, StoreOptions};

fn store(shared: &SharedCaches, quads: usize) -> Store {
    let opts = StoreOptions {
        shared: Some(shared.scope()),
        result_cache_min_ms: 0.0,
        ..Default::default()
    };
    let s = Store::in_memory(opts);
    let mut body = String::new();
    for i in 0..quads {
        body.push_str(&format!(
            "<urn:s{i}> <urn:p> {i} . <urn:s{i}> <urn:q> \"v{i}\" .\n"
        ));
    }
    update(
        &s,
        &format!("INSERT DATA {{ {body} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    s.compact().unwrap();
    s
}

fn run(s: &Store, q: &str) -> usize {
    query(s.snapshot(), q, &QueryOptions::default())
        .unwrap()
        .rows()
        .len()
}

const Q: &str = "SELECT ?s ?o WHERE { ?s <urn:p> ?o . ?s <urn:q> ?v } ORDER BY ?o";

#[test]
fn datasets_share_one_block_cache_and_count_their_own_bytes() {
    let shared = SharedCaches::new(CacheSizes {
        blocks: 64 << 20,
        ..Default::default()
    });
    let (a, b) = (store(&shared, 2_000), store(&shared, 1_000));
    assert_eq!(run(&a, Q), 2_000);
    assert_eq!(run(&b, Q), 1_000);
    let (ab, bb) = (a.cache().bytes(), b.cache().bytes());
    assert!(ab > 0 && bb > 0, "{ab} {bb}");
    // the namespaces add up to the cache's weight, which never passes its capacity
    let total = shared.blocks().bytes();
    assert_eq!(ab + bb, total);
    assert!(total <= shared.blocks().capacity());
    assert_eq!(a.cache().capacity(), shared.blocks().capacity());
    let usage = shared.blocks().usage();
    assert_eq!(usage.len(), 2);
    assert_eq!(usage[&a.cache().namespace()].bytes, ab);
    // each view counts its own lookups
    assert!(a.cache().misses() > 0 && b.cache().misses() > 0);
    let before = a.cache().hits();
    run(&a, Q);
    assert!(a.cache().hits() > before);
}

#[test]
fn a_small_shared_block_cache_bounds_every_dataset_together() {
    let shared = SharedCaches::new(CacheSizes {
        blocks: 1 << 20,
        ..Default::default()
    });
    let stores: Vec<Store> = (0..4).map(|_| store(&shared, 20_000)).collect();
    for s in &stores {
        run(s, Q);
    }
    let sum: u64 = stores.iter().map(|s| s.cache().bytes()).sum();
    assert_eq!(sum, shared.blocks().bytes());
    // quick_cache keeps each shard within its share, so the total stays near the limit
    assert!(sum <= 2 << 20, "{sum}");
}

#[test]
fn clearing_one_dataset_keeps_the_others_results() {
    let shared = SharedCaches::new(CacheSizes::default());
    let (a, b) = (store(&shared, 500), store(&shared, 500));
    for s in [&a, &b] {
        run(s, Q);
    }
    let (ra, rb) = (a.result_cache(), b.result_cache());
    assert!(ra.entries() > 0 && rb.entries() > 0);
    let kept = rb.entries();
    assert_eq!(
        ra.entries() + rb.entries(),
        shared.results().entries(),
        "namespaces add up to the cache"
    );
    ra.clear();
    assert_eq!(ra.entries(), 0);
    assert_eq!(ra.bytes(), 0);
    assert_eq!(rb.entries(), kept);
    // b hits, a recomputes
    let (hb, ha) = (rb.hits(), ra.hits());
    run(&b, Q);
    run(&a, Q);
    assert!(rb.hits() > hb);
    assert_eq!(ra.hits(), ha);
}

#[test]
fn history_states_share_one_budget() {
    let shared = SharedCaches::new(CacheSizes::default());
    let dir = tempfile::tempdir().unwrap();
    let opts = StoreOptions {
        shared: Some(shared.scope()),
        ..Default::default()
    };
    {
        let s = Store::open(&dir.path().join("db"), opts).unwrap();
        for i in 0..4 {
            update(
                &s,
                &format!("INSERT DATA {{ <urn:s{i}> <urn:p> 1, 2, 3 }}"),
                &QueryOptions::default(),
            )
            .unwrap();
        }
        s.snapshot_at(&sparkles_core::history::At::Commit(2), &Default::default())
            .unwrap();
        assert!(shared.history().used() > 0);
        assert_eq!(shared.history().used(), s.history().cache_bytes);
    }
    // closing the store gives its bytes back
    assert_eq!(shared.history().used(), 0);
}
