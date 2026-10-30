use super::*;
use crate::guard::WriteOptions;
use std::time::{Duration, Instant};

#[test]
fn source_lookup_obeys_controls_while_entries_are_contended() {
    let catalog = Catalog::memory(Default::default());
    let dataset = catalog.attach("source", Attach::Memory).unwrap();
    let entries = catalog.inner.entries.write();
    let no_wait = WriteOptions {
        no_wait: true,
        ..Default::default()
    };
    assert!(matches!(
        catalog.try_get_by_id(dataset.dataset_id(), &no_wait),
        Err(Error::WriterBusy)
    ));
    let deadline = WriteOptions {
        deadline: Some(Instant::now() + Duration::from_millis(20)),
        ..Default::default()
    };
    let started = Instant::now();
    assert!(matches!(
        catalog.try_get_by_id(dataset.dataset_id(), &deadline),
        Err(Error::Timeout)
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    let cancelled = WriteOptions {
        cancel: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
        ..Default::default()
    };
    assert!(matches!(
        catalog.try_get_by_id(dataset.dataset_id(), &cancelled),
        Err(Error::Cancelled)
    ));
    drop(entries);
    let resolved = catalog
        .try_get_by_id(dataset.dataset_id(), &no_wait)
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(dataset.inner_state(), resolved.inner_state()));
}
