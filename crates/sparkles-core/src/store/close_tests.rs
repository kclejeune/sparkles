//! Shutdown boundaries for components that can outlive their Store.
use super::*;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[test]
fn retained_embedder_output_finishes_when_store_drops() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("db");
    let store = Store::open(&root, Default::default()).unwrap();
    let embedder = store.embedder();
    let writer = store.writer.clone();
    let record_dir = root.join("embed");
    let mut work = crate::vector::embed::worker::Work::new(
        "close-test",
        "urn:p",
        2,
        Default::default(),
        Some(&record_dir),
        0,
        0,
    );
    work.record.set(1, 2);
    embedder.works.lock().insert("close-test".into(), work);
    let record = record_dir.join("close-test.log");
    assert_eq!(std::fs::read(&record).unwrap().len(), 16);
    drop(store);
    let closed = embedder.is_closed();
    let writer_closed = writer.lock().closed;
    let finalized = std::fs::read(&record).unwrap();
    drop(lock_database(&root).expect("shutdown must release actual process exclusion"));
    let old = directory.path().join("old");
    std::fs::rename(&root, &old).unwrap();
    std::fs::create_dir_all(root.join("embed")).unwrap();
    std::fs::write(root.join("embed/close-test.log"), b"replacement").unwrap();
    drop(embedder);
    drop(writer);
    let after_retained_drop = std::fs::read(old.join("embed/close-test.log")).unwrap();
    let replacement = std::fs::read(root.join("embed/close-test.log")).unwrap();
    eprintln!(
        "normal-close-finalization writer_closed={writer_closed} embedder_closed={closed} before_retained_drop={} after_retained_drop={} replacement_unchanged={}",
        finalized.len(),
        after_retained_drop.len(),
        replacement == b"replacement",
    );
    assert!(closed);
    assert!(writer_closed);
    assert_eq!(finalized.len(), 32);
    assert_eq!(after_retained_drop, finalized);
    assert_eq!(replacement, b"replacement");
}

fn assert_panicking_guard_finishes_resources(
    store: Store,
    root: &std::path::Path,
    record_name: &str,
    expected_bytes: usize,
) {
    use crate::guard::{Candidate, CommitGuard, ValidationSummary};
    use std::sync::atomic::AtomicUsize;

    struct PanicOnClose(Arc<AtomicUsize>);
    impl CommitGuard for PanicOnClose {
        fn check(&self, _: &Candidate<'_>) -> Result<ValidationSummary> {
            unreachable!("the fixture installs this guard after its final write")
        }

        fn describe(&self) -> String {
            "panic-on-close".into()
        }

        fn close(&self, _: &crate::commit::CommitInfo, _: &crate::store::Snapshot) {
            assert!(!std::thread::panicking());
            self.0.fetch_add(1, Ordering::SeqCst);
            std::panic::panic_any("guard-close-payload");
        }
    }

    let writer = store.writer.clone();
    let log = store.changelog.as_ref().unwrap().clone();
    assert!(
        log.status().pending > 0,
        "the real change log must need draining"
    );
    let embedder = store.embedder();
    let calls = Arc::new(AtomicUsize::new(0));
    store.set_guard(Some(Arc::new(PanicOnClose(calls.clone()))));
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(store)));
    let payload = caught.expect_err("the original callback panic must propagate");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"guard-close-payload"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // These owners deliberately outlive Store; its field destruction alone
    // cannot revoke the collector or finish their pending output.
    let writer_closed = writer.lock().closed;
    let log_status = log.status();
    let embedder_closed = embedder.is_closed();
    let relative = format!("embed/{record_name}.log");
    let finalized = std::fs::read(root.join(&relative)).unwrap();
    // Probe the actual database process lock without reopening the embedding
    // index, whose normal record compaction would change this byte fixture.
    drop(crate::store::lock_database(root).expect("unwinding must release exclusion"));
    let old = root.parent().unwrap().join("old");
    std::fs::rename(root, &old).unwrap();
    std::fs::create_dir_all(root.join("embed")).unwrap();
    std::fs::write(root.join(&relative), b"replacement").unwrap();
    drop(embedder);
    drop(log);
    drop(writer);
    let after_retained_drop = std::fs::read(old.join(&relative)).unwrap();
    let replacement = std::fs::read(root.join(&relative)).unwrap();
    eprintln!(
        "close-finalization writer_closed={writer_closed} embedder_closed={embedder_closed} \
         pending={} before_retained_drop={} after_retained_drop={} replacement_unchanged={}",
        log_status.pending,
        finalized.len(),
        after_retained_drop.len(),
        replacement == b"replacement",
    );
    assert!(writer_closed);
    assert_eq!(log_status.pending, 0);
    assert!(log_status.error.is_none());
    assert!(embedder_closed);
    assert_eq!(finalized.len(), expected_bytes);
    assert_eq!(after_retained_drop, finalized);
    assert_eq!(replacement, b"replacement");
}

#[test]
fn panicking_close_guard_finishes_retained_buffered_output() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("db");
    let store = Store::open(&root, Default::default()).unwrap();
    store.changelog.as_ref().unwrap().set_background(false);
    crate::sparql::update::update(
        &store,
        "INSERT DATA { <urn:a> <urn:p> <urn:b> }",
        &Default::default(),
    )
    .unwrap();
    let record_dir = root.join("embed");
    let mut work = crate::vector::embed::worker::Work::new(
        "close-test",
        "urn:p",
        2,
        Default::default(),
        Some(&record_dir),
        0,
        0,
    );
    work.record.set(1, 2);
    store
        .embedder()
        .works
        .lock()
        .insert("close-test".into(), work);
    assert_eq!(
        std::fs::metadata(record_dir.join("close-test.log"))
            .unwrap()
            .len(),
        16
    );
    assert_panicking_guard_finishes_resources(store, &root, "close-test", 32);
}

#[test]
fn panicking_close_guard_finishes_public_embedding_removal_output() {
    use crate::outbound::OutboundPolicy;
    use crate::vector::VectorIndexConfig;
    use crate::vector::embed::mock::MockProvider;
    use crate::vector::embed::{EmbeddingConfig, Environment, Prepared};

    let mock = MockProvider::start(8);
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("db");
    let store = Store::open(&root, Default::default()).unwrap();
    store.changelog.as_ref().unwrap().set_background(false);
    store.set_embedding_environment(Some(Environment {
        outbound: OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
        ..Default::default()
    }));
    let mut embedding =
        EmbeddingConfig::new(&mock.url(), "mock-model").from_predicates(&["urn:label"]);
    embedding.batch_size = 2;
    embedding.requests_per_minute = 0;
    embedding.tokens_per_minute = 0;
    embedding.max_retries = 0;
    let mut config = VectorIndexConfig::new("urn:vector", 8);
    config.embedding = Some(embedding);
    store.create_vector_index("docs", config).unwrap();
    crate::sparql::update::update(
        &store,
        "INSERT DATA { <urn:a> <urn:label> \"alpha\" }",
        &Default::default(),
    )
    .unwrap();
    store.embed_until_idle(Duration::from_secs(20)).unwrap();
    let record = root.join("embed/docs.log");
    let before = std::fs::read(&record).unwrap();
    assert_eq!(
        before.len(),
        32,
        "A must have a persisted reconciliation record"
    );
    crate::sparql::update::update(
        &store,
        "DELETE WHERE { <urn:a> <urn:label> ?label }; \
         DELETE WHERE { <urn:a> <urn:vector> ?vector }; \
         INSERT DATA { <urn:b> <urn:label> \"beta\" }",
        &Default::default(),
    )
    .unwrap();
    // Both changed subjects fit in one batch. Removing A requires no request;
    // B keeps the batch nonempty, leaving A's removal buffered until close.
    let mut prepared = None;
    for _ in 0..8 {
        match store.embed_prepare() {
            Prepared::Batch(batch) => {
                prepared = Some(batch);
                break;
            }
            Prepared::Progress => {}
            Prepared::Idle | Prepared::Wait(_) => panic!("expected the mixed batch"),
        }
    }
    let batch = prepared.expect("bounded prepare steps must produce B's request");
    assert_eq!(batch.items.len(), 1);
    assert_eq!(batch.items[0].subject, "<urn:b>");
    assert_eq!(
        std::fs::read(&record).unwrap(),
        before,
        "A's removal remains buffered"
    );
    drop(batch);
    assert_panicking_guard_finishes_resources(store, &root, "docs", 48);
}
