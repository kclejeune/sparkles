//! The final public native document must retain its physical owner and credit.
use sparkles_core::sparql::{QueryOptions, pool::QueryPool, query, results::sparkles_json};
use sparkles_core::store::{Store, StoreOptions};

// Preserve the same positive-budget ownership oracle across the fallible public
// signature migration. This fixture helper never copies or invents an owner.
trait Admitted {
    type Document;
    fn admitted(self) -> Self::Document;
}
impl Admitted for serde_json::Value {
    type Document = Self;
    fn admitted(self) -> Self {
        self
    }
}
impl<T, E: std::fmt::Debug> Admitted for Result<T, E> {
    type Document = T;
    fn admitted(self) -> T {
        self.expect("positive native document fixture admission")
    }
}

#[test]
fn public_native_document_retains_original_credit_through_its_last_clone() {
    let store = Store::in_memory(StoreOptions::default());
    let pool = QueryPool::new(16 << 20);
    let text = format!(
        "SELECT ?value WHERE {{ VALUES ?value {{ \"{}\" }} }}",
        "x".repeat(64 << 10)
    );
    let result = query(
        store.snapshot(),
        &text,
        &QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    let document = sparkles_json(&result, None).admitted();
    let retained = document.clone();
    // This independent application-owned semantic oracle is dropped before the
    // retained-query lifetime assertions; it is not an engine ownership adapter.
    let semantic = serde_json::to_value(&retained).unwrap();
    assert_eq!(semantic["rows"][0][0]["value"], "x".repeat(64 << 10));
    assert_eq!(semantic["meta"]["sentRows"], 1);
    drop(semantic);
    drop(result);
    drop(store);
    drop(document);
    assert!(
        pool.granted() >= 64 << 10,
        "the actual final native JSON payload still needs its original live credit; live={}",
        pool.granted()
    );
    let semantic = serde_json::to_value(&retained).unwrap();
    assert_eq!(semantic["rows"][0][0]["value"], "x".repeat(64 << 10));
    drop(semantic);
    drop(retained);
    assert_eq!(pool.granted(), 0);
}
