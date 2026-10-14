//! The catalog's query memory pool reaches every dataset it hands out: clones adopted
//! from another dataset, and the updates of a transaction.

use sparkles::catalog::{CatalogOptions, CloneRequest, CreateDataset, DatasetKind, MemoryOptions};
use sparkles::{BudgetKind, Catalog, Dataset, Error};

const MB: u64 = 1 << 20;

/// A million rows of three columns.
const CROSS: &str = "SELECT ?a ?b ?x ?y WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y } ORDER BY ?x ?y";

fn catalog() -> (Catalog, Dataset) {
    let cat = Catalog::memory(CatalogOptions {
        memory: MemoryOptions {
            pool_bytes: Some(MB),
            ..Default::default()
        },
        ..Default::default()
    });
    let a = cat
        .create(
            "a",
            &CreateDataset {
                kind: DatasetKind::Memory,
                ..Default::default()
            },
        )
        .unwrap();
    let mut body = String::new();
    for i in 0..1_000 {
        body.push_str(&format!("<urn:s{i}> <urn:p> {i} .\n"));
    }
    a.update(&format!("INSERT DATA {{ {body} }}")).unwrap();
    (cat, a)
}

fn server_memory(r: Result<sparkles::sparql::QueryResult, Error>) -> bool {
    matches!(r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::ServerMemory)
}

#[test]
fn memory_clones_use_the_catalog_pool() {
    let (cat, a) = catalog();
    let b = cat
        .clone_dataset("a", "b", &CloneRequest::default(), &Default::default())
        .unwrap();
    assert!(server_memory(a.query_with(CROSS, &Default::default())));
    match b.query_with(CROSS, &Default::default()) {
        Err(Error::BudgetExceeded(x)) => assert_eq!(x.kind, BudgetKind::ServerMemory),
        Err(e) => panic!("{e}"),
        Ok(r) => panic!("{} rows on the clone", r.rows().len()),
    }
}

#[test]
fn memory_clones_get_their_own_cache_namespace() {
    let cat = Catalog::memory(CatalogOptions::from(sparkles::store::StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    }));
    let a = cat
        .create(
            "a",
            &CreateDataset {
                kind: DatasetKind::Memory,
                ..Default::default()
            },
        )
        .unwrap();
    let mut body = String::new();
    for i in 0..200 {
        body.push_str(&format!("<urn:s{i}> <urn:p> {i} .\n"));
    }
    a.update(&format!("INSERT DATA {{ {body} }}")).unwrap();
    let b = cat
        .clone_dataset("a", "b", &CloneRequest::default(), &Default::default())
        .unwrap();
    let (ra, rb) = (a.store().result_cache(), b.store().result_cache());
    assert_ne!(ra.namespace(), rb.namespace());
    assert_ne!(a.store().cache().namespace(), b.store().cache().namespace());
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY ?o";
    for ds in [&a, &b] {
        ds.query_with(q, &Default::default()).unwrap();
    }
    assert!(ra.entries() > 0 && rb.entries() > 0);
    // what `POST /$/cache/clear/b` does
    rb.clear();
    rb.service.clear();
    assert_eq!(rb.entries(), 0);
    let hits = ra.hits();
    a.query_with(q, &Default::default()).unwrap();
    assert!(
        ra.hits() > hits,
        "a's cached result survived the clear of b"
    );
}

#[test]
fn transaction_updates_use_the_dataset_pool() {
    let (_cat, a) = catalog();
    let u = "DELETE { ?a <urn:none> ?b } WHERE { SELECT ?a ?b WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y } ORDER BY ?x ?y }";
    let direct = a.update_with(u, &Default::default());
    assert!(
        matches!(direct, Err(Error::BudgetExceeded(ref b)) if b.kind == BudgetKind::ServerMemory),
        "{direct:?}"
    );
    let through = a.transaction(|tx| tx.update_with(u, &Default::default()));
    assert!(
        matches!(through, Err(Error::BudgetExceeded(ref b)) if b.kind == BudgetKind::ServerMemory),
        "{through:?}"
    );
}
