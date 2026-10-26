//! Cold generation preparation preserves exact retained state and durable bytes.
use sparkles_core::{
    Error,
    history::{At, HistoryOptions},
    io::{RdfFormat, Source},
    sparql::{QueryOptions, query, update::update},
    store::{Store, StoreOptions},
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

struct Counting;
#[derive(Clone, Copy, Default)]
struct Allocations {
    active: bool,
    largest: usize,
    total: usize,
}
thread_local! {
    static CALLER: std::cell::Cell<Allocations> = const {
        std::cell::Cell::new(Allocations { active: false, largest: 0, total: 0 })
    };
}
static WINDOW: AtomicBool = AtomicBool::new(false);
static OUTSIDE: AtomicUsize = AtomicUsize::new(0);
static MEASURE_LOCK: Mutex<()> = Mutex::new(());
fn record(size: usize) {
    let caller = CALLER
        .try_with(|cell| {
            let mut counts = cell.get();
            if counts.active {
                counts.largest = counts.largest.max(size);
                counts.total = counts.total.saturating_add(size);
                cell.set(counts);
            }
            counts.active
        })
        .unwrap_or(false);
    if !caller && WINDOW.load(Ordering::SeqCst) {
        OUTSIDE.fetch_add(size, Ordering::SeqCst);
    }
}
// SAFETY: pointers, layouts and allocation behavior are delegated unchanged to
// System. Const TLS counters observe only caller sizes, without tracing or allocation.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, layout, size) };
        if !pointer.is_null() {
            record(size);
        }
        pointer
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;
fn measure<T>(operation: impl FnOnce() -> T) -> (T, Allocations, usize) {
    let _serial = MEASURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    struct Measuring;
    impl Drop for Measuring {
        fn drop(&mut self) {
            WINDOW.store(false, Ordering::SeqCst);
            let _ = CALLER.try_with(|cell| {
                let mut counts = cell.get();
                counts.active = false;
                cell.set(counts);
            });
        }
    }
    CALLER.with(|cell| {
        cell.set(Allocations {
            active: true,
            ..Default::default()
        })
    });
    OUTSIDE.store(0, Ordering::SeqCst);
    WINDOW.store(true, Ordering::SeqCst);
    let measuring = Measuring;
    let result = operation();
    drop(measuring);
    (
        result,
        CALLER.with(std::cell::Cell::get),
        OUTSIDE.load(Ordering::SeqCst),
    )
}

fn bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                result.insert(entry.path(), std::fs::read(entry.path()).unwrap());
            }
        }
    }
    result
}

fn cold_fixture() -> (tempfile::TempDir, Store, u64) {
    let root = tempfile::tempdir().unwrap();
    let options = StoreOptions {
        build: sparkles_core::builder::BuildOptions {
            threads: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let store = Store::open(root.path(), options.clone()).unwrap();
    store
        .load(&[Source::from_bytes(
            b"<urn:base> <urn:p> 1 .".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    // The requested state has a tiny delta, but keeps an independently large owned
    // dictionary from earlier now-deleted terms. A cached view must charge it too.
    let triple = format!("<urn:transient> <urn:p> \"{}\" .", "x".repeat(400_000));
    for verb in ["INSERT", "DELETE"] {
        update(
            &store,
            &format!("{verb} DATA {{ {triple} }}"),
            &QueryOptions::default(),
        )
        .unwrap();
    }
    let target = store.head_commit().seq;
    store
        .create_snapshot("retained", &At::Commit(target), None)
        .unwrap();
    update(
        &store,
        "INSERT DATA { <urn:head-only> <urn:p> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
    store.compact().unwrap();
    store.flush_change_log().unwrap();
    drop(store);
    let store = Store::open(root.path(), options).unwrap();
    store.flush_change_log().unwrap();
    (root, store, target)
}

#[test]
fn cold_generation_and_cached_backing_keep_exact_state_and_charge_owned_dictionary() {
    let (root, store, target) = cold_fixture();
    let before = bytes(root.path());
    let options = HistoryOptions::default();
    let (refusal, allocations, outside) =
        measure(|| store.snapshot_at_limited(&At::Commit(target), &options, 32 << 10));
    assert!(matches!(refusal, Err(Error::BudgetExceeded(_))));
    eprintln!(
        "cold owned decode refusal: caller largest={}, total={}, outside total={outside}",
        allocations.largest, allocations.total
    );
    assert!(
        allocations.largest <= 32 << 10,
        "a cold file was copied before admission"
    );
    // The serial decoder owns only new temporary storage in this window. Total
    // allocation is a conservative bound on its peak, independent of async workers.
    assert!(
        allocations.total <= 32 << 10,
        "cold temporary allocation exceeded the request cap"
    );
    assert_eq!(before, bytes(root.path()));
    let (view, resolved) = store
        .snapshot_at_limited(&At::Commit(target), &options, 16 << 20)
        .unwrap();
    assert_eq!(resolved.commit.seq, target);
    assert_eq!(view.commit, target);
    let result = query(
        view,
        "SELECT ?s WHERE { ?s ?p ?o }",
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(result.rows().len(), 1);
    assert_eq!(
        result.rows()[0][0].as_ref().unwrap().to_string(),
        "<urn:base>"
    );
    // This is now a cache hit with no new dictionary decode. The retained owned
    // dictionary still cannot be represented by a 256KiB preparation allowance.
    assert!(matches!(
        store.snapshot_at_limited(&At::Commit(target), &options, 256 << 10),
        Err(Error::BudgetExceeded(_))
    ));
    let (again, _) = store
        .snapshot_at_limited(&At::Commit(target), &options, 16 << 20)
        .unwrap();
    assert_eq!(again.commit, target);
    assert_eq!(before, bytes(root.path()));
}

#[test]
fn compatible_unlimited_cache_cannot_bypass_later_request_cap_or_cancellation() {
    let (root, store, target) = cold_fixture();
    let options = HistoryOptions::default();
    store.snapshot_at(&At::Commit(target), &options).unwrap();
    let before = bytes(root.path());
    assert!(matches!(
        store.snapshot_at_limited(&At::Commit(target), &options, 256 << 10),
        Err(Error::BudgetExceeded(_))
    ));
    let cancelled = HistoryOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        deadline: None,
    };
    assert!(matches!(
        store.snapshot_at_limited(&At::Commit(target), &cancelled, 16 << 20),
        Err(Error::Cancelled)
    ));
    assert_eq!(before, bytes(root.path()));
}

#[test]
fn cold_mapped_giant_base_key_is_not_reconstructed_during_preparation() {
    let root = tempfile::tempdir().unwrap();
    let options = StoreOptions {
        build: sparkles_core::builder::BuildOptions {
            threads: 1,
            vocab_codec: Some(sparkles_core::builder::VocabCodec::Front),
            ..Default::default()
        },
        ..Default::default()
    };
    let store = Store::open(root.path(), options.clone()).unwrap();
    let text = format!("<urn:base> <urn:p> \"{}\" .", "x".repeat(400_000));
    store
        .load(&[Source::from_bytes(
            text.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let target = store.head_commit().seq;
    store
        .create_snapshot("retained", &At::Commit(target), None)
        .unwrap();
    update(
        &store,
        "INSERT DATA { <urn:later> <urn:p> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
    store.compact().unwrap();
    store.flush_change_log().unwrap();
    drop(store);
    let store = Store::open(root.path(), options).unwrap();
    store.flush_change_log().unwrap();
    let before = bytes(root.path());
    let (result, counts, outside) = measure(|| {
        store.snapshot_at_limited(&At::Commit(target), &HistoryOptions::default(), 256 << 10)
    });
    let (view, _) = result.expect("immutable mapped key does not need an owned reconstruction");
    assert_eq!(view.commit, target);
    eprintln!(
        "giant mapped base: caller largest={}, total={}, outside total={outside}",
        counts.largest, counts.total
    );
    assert!(
        counts.largest <= 256 << 10,
        "mapped key was eagerly reconstructed"
    );
    assert!(
        counts.total <= 256 << 10,
        "serial cold preparation allocated more than its cap"
    );
    assert_eq!(before, bytes(root.path()));
}
