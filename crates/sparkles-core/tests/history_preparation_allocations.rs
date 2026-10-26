//! A bounded read must refuse large transactions before their buffer allocation.
use sparkles_core::{
    Error,
    history::{At, HistoryOptions},
    sparql::{QueryOptions, update::update},
    store::{Store, StoreOptions},
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

struct Counting;
static ENABLED: AtomicBool = AtomicBool::new(false);
static LARGEST: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static MEASURE_LOCK: Mutex<()> = Mutex::new(());

// SAFETY: allocation/deallocation use System with the caller's layout. Reallocation
// deliberately moves: allocate with the original alignment, copy the allocation
// prefix, then free the old block. Failure preserves the original allocation. This
// isolated test allocator observes both blocks before releasing the old one.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            if ENABLED.load(Ordering::SeqCst) {
                LARGEST.fetch_max(layout.size(), Ordering::SeqCst);
                PEAK.fetch_max(live, Ordering::SeqCst);
            }
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let Ok(replacement) = Layout::from_size_align(size, layout.align()) else {
            return std::ptr::null_mut();
        };
        let moved = unsafe { System.alloc(replacement) };
        if !moved.is_null() {
            let live = LIVE.fetch_add(size, Ordering::SeqCst) + size;
            if ENABLED.load(Ordering::SeqCst) {
                LARGEST.fetch_max(size, Ordering::SeqCst);
                PEAK.fetch_max(live, Ordering::SeqCst);
            }
            unsafe {
                std::ptr::copy_nonoverlapping(pointer, moved, layout.size().min(size));
                System.dealloc(pointer, layout);
            }
            LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
        }
        moved
    }
}

fn measure<T>(operation: impl FnOnce() -> T) -> (T, usize, usize) {
    let base = LIVE.load(Ordering::SeqCst);
    LARGEST.store(0, Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    struct Measuring;
    impl Drop for Measuring {
        fn drop(&mut self) {
            ENABLED.store(false, Ordering::SeqCst);
        }
    }
    ENABLED.store(true, Ordering::SeqCst);
    let measuring = Measuring;
    let result = operation();
    drop(measuring);
    (
        result,
        LARGEST.load(Ordering::SeqCst),
        PEAK.load(Ordering::SeqCst).saturating_sub(base),
    )
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;
#[test]
fn cold_replay_refuses_transaction_growth_before_allocating_the_transaction() {
    let _lock = MEASURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreOptions::default()).unwrap();
    let data = (0..12_000)
        .map(|n| format!("<urn:s{n}> <urn:p> 1 ."))
        .collect::<String>();
    update(
        &store,
        &format!("INSERT DATA {{ {data} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    update(
        &store,
        "INSERT DATA { <urn:later> <urn:p> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let cap = 64 << 10;
    let (result, largest, peak) =
        measure(|| store.snapshot_at_limited(&At::Commit(1), &HistoryOptions::default(), cap));
    assert!(matches!(result, Err(Error::BudgetExceeded(_))));
    eprintln!("bounded WAL refusal: largest={largest}, peak={peak}, cap={cap}");
    assert!(
        largest <= cap as usize,
        "whole transaction was allocated first: {largest}"
    );
    assert!(
        peak <= cap as usize,
        "temporary allocation peak exceeded cap: {peak}"
    );
    // The refused attempt must not poison the history cache or select head.
    let (exact, resolved) = store
        .snapshot_at_limited(&At::Commit(1), &HistoryOptions::default(), 64 << 20)
        .unwrap();
    assert_eq!(resolved.commit.seq, 1);
    assert_eq!(exact.commit, 1);
}

#[test]
fn selector_metadata_is_refused_before_clone() {
    let _lock = MEASURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let store = Store::in_memory(StoreOptions::default());
    let name = "a".repeat(64);
    store.create_snapshot(&name, &At::Head, None).unwrap();
    let selector = At::Snapshot(name);
    let (result, largest, peak) =
        measure(|| store.snapshot_at_limited(&selector, &HistoryOptions::default(), 16));
    assert!(
        matches!(result, Err(Error::BudgetExceeded(_))),
        "selector name copied before metadata admission"
    );
    eprintln!("selector metadata refusal: largest={largest}, peak={peak}, cap=16");
    assert!(
        largest <= 16 && peak <= 16,
        "selector metadata copied before refusal: {largest}/{peak}"
    );
}

#[test]
fn moving_transaction_growth_counts_old_and_replacement_together() {
    let _lock = MEASURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreOptions::default()).unwrap();
    let data = (0..12_000)
        .map(|n| format!("<urn:s{n}> <urn:p> 1 ."))
        .collect::<String>();
    update(
        &store,
        &format!("INSERT DATA {{ {data} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    update(
        &store,
        "INSERT DATA { <urn:later> <urn:p> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let cap = 100 << 10;
    let (result, largest, peak) =
        measure(|| store.snapshot_at_limited(&At::Commit(1), &HistoryOptions::default(), cap));
    assert!(matches!(result, Err(Error::BudgetExceeded(_))));
    eprintln!("moving WAL refusal: largest={largest}, peak={peak}, cap={cap}");
    assert!(largest <= cap as usize);
    assert!(
        peak <= cap as usize,
        "moving replacement exceeded cap: {peak}"
    );
}

// A valid format2 fixture built from real stored IDs. Re-encode each whole column
// using the documented zigzag-varint/LZ4 codec and100-byte BlockMeta layout.
// Keeping this fixture generator in the test avoids exposing raw builder internals.
fn rewrite_whole_column_layout(root: &std::path::Path, quads: &[[sparkles_core::id::Id; 4]]) {
    use sparkles_core::index::{BLOCK_ROWS, Perm};
    let current = std::fs::read_to_string(root.join("CURRENT")).unwrap();
    let generation = root.join(current.trim());
    for perm in Perm::ALL {
        let mut keys: Vec<_> = quads.iter().map(|quad| perm.to_key(quad)).collect();
        keys.sort_unstable();
        let mut data = Vec::new();
        let mut metadata = Vec::new();
        let mut row_start = 0u64;
        for block in keys.chunks(BLOCK_ROWS) {
            let offset = data.len() as u64;
            let mut lengths = [0u32; 4];
            for (column, length) in lengths.iter_mut().enumerate() {
                let mut raw = Vec::new();
                let mut previous = 0u64;
                for key in block {
                    let difference = key[column].wrapping_sub(previous) as i64;
                    let mut encoded = ((difference << 1) ^ (difference >> 63)) as u64;
                    while encoded >= 128 {
                        raw.push(encoded as u8 | 128);
                        encoded >>= 7;
                    }
                    raw.push(encoded as u8);
                    previous = key[column];
                }
                let compressed = lz4_flex::compress_prepend_size(&raw);
                *length = compressed.len() as u32;
                data.extend(compressed);
            }
            for value in block.first().unwrap().iter().chain(block.last().unwrap()) {
                metadata.extend_from_slice(&value.to_le_bytes());
            }
            metadata.extend_from_slice(&offset.to_le_bytes());
            for length in lengths {
                metadata.extend_from_slice(&length.to_le_bytes());
            }
            metadata.extend_from_slice(&(block.len() as u32).to_le_bytes());
            metadata.extend_from_slice(&row_start.to_le_bytes());
            row_start += block.len() as u64;
        }
        std::fs::write(generation.join(format!("{}.dat", perm.name())), data).unwrap();
        std::fs::write(generation.join(format!("{}.meta", perm.name())), metadata).unwrap();
        std::fs::remove_file(generation.join(format!("{}.seg", perm.name()))).unwrap();
    }
    let path = generation.join("meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    meta["format_version"] = 2.into();
    std::fs::write(path, serde_json::to_vec(&meta).unwrap()).unwrap();
}

fn durable_bytes(
    root: &std::path::Path,
) -> std::collections::BTreeMap<std::path::PathBuf, [u8; 32]> {
    use sha2::{Digest, Sha256};
    let mut pending = vec![root.to_path_buf()];
    let mut hashes = std::collections::BTreeMap::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                hashes.insert(
                    entry.path(),
                    Sha256::digest(std::fs::read(entry.path()).unwrap()).into(),
                );
            }
        }
    }
    hashes
}

fn whole_column_store() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let options = StoreOptions {
        cache_bytes: 0,
        build: sparkles_core::builder::BuildOptions {
            vocab_codec: Some(sparkles_core::builder::VocabCodec::Front),
            threads: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    let store = Store::open(root.path(), options.clone()).unwrap();
    let data = (0..sparkles_core::index::BLOCK_ROWS)
        .map(|n| format!("<urn:s{n}> <urn:p> 1 ."))
        .collect::<String>();
    update(
        &store,
        &format!("INSERT DATA {{ {data} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    store.compact().unwrap();
    let mut quads = Vec::new();
    store
        .snapshot()
        .for_each_quad(|quad| {
            quads.push(*quad);
            Ok(())
        })
        .unwrap();
    drop(store);
    rewrite_whole_column_layout(root.path(), &quads);
    let store = Store::open(root.path(), options).unwrap();
    (root, store)
}

#[test]
fn cold_nonempty_whole_column_base_is_admitted_before_decode() {
    let _lock = MEASURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (root, store) = whole_column_store();
    update(
        &store,
        "DELETE DATA { <urn:s0> <urn:p> 1 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let target = store.head_commit().seq;
    update(
        &store,
        "INSERT DATA { <urn:future> <urn:p> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
    // Two later commits keep the live seed farther away than the base, including
    // the WAL header. This fixture must exercise forward replay.
    update(
        &store,
        "INSERT DATA { <urn:future2> <urn:p> 3 }",
        &QueryOptions::default(),
    )
    .unwrap();
    // Commits enqueue change-log writes independently of snapshot preparation.
    // Quiesce that producer before asserting that the read changes no bytes.
    store.flush_change_log().unwrap();
    let before = durable_bytes(root.path());
    let cap = 256 << 10;
    let (result, largest, peak) =
        measure(|| store.snapshot_at_limited(&At::Commit(target), &HistoryOptions::default(), cap));
    eprintln!("whole-column refusal: largest={largest}, peak={peak}, cap={cap}");
    assert!(
        matches!(result, Err(Error::BudgetExceeded(_))),
        "base index decoded without admission"
    );
    assert!(largest <= cap as usize);
    assert!(peak <= cap as usize, "base decode exceeded cap: {peak}");
    assert!(
        before == durable_bytes(root.path()),
        "refusal modified durable bytes"
    );
    let (view, resolved) = store
        .snapshot_at_limited(&At::Commit(target), &HistoryOptions::default(), 2 << 20)
        .unwrap();
    assert_eq!(resolved.commit.seq, target);
    assert_eq!(view.len(), sparkles_core::index::BLOCK_ROWS as u64 - 1);
    assert!(
        before == durable_bytes(root.path()),
        "read modified durable bytes"
    );
}

#[test]
fn backward_nonempty_whole_column_base_is_admitted_before_decode() {
    let _lock = MEASURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (root, store) = whole_column_store();
    update(
        &store,
        "DELETE DATA { <urn:s0> <urn:p> 1 }",
        &QueryOptions::default(),
    )
    .unwrap();
    update(
        &store,
        "INSERT DATA { <urn:future> <urn:p> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let target = store.head_commit().seq;
    update(
        &store,
        "DELETE DATA { <urn:s1> <urn:p> 1 }",
        &QueryOptions::default(),
    )
    .unwrap();
    // Commits enqueue change-log writes independently of snapshot preparation.
    // Quiesce that producer before asserting that the read changes no bytes.
    store.flush_change_log().unwrap();
    let before = durable_bytes(root.path());
    let cap = 512 << 10;
    let (result, largest, peak) =
        measure(|| store.snapshot_at_limited(&At::Commit(target), &HistoryOptions::default(), cap));
    eprintln!("backward whole-column refusal: largest={largest}, peak={peak}, cap={cap}");
    assert!(
        matches!(result, Err(Error::BudgetExceeded(_))),
        "backward base index decoded without admission"
    );
    assert!(largest <= cap as usize);
    assert!(
        peak <= cap as usize,
        "backward base decode exceeded cap: {peak}"
    );
    assert!(
        before == durable_bytes(root.path()),
        "read/refusal modified durable bytes"
    );
    let (view, resolved) = store
        .snapshot_at_limited(&At::Commit(target), &HistoryOptions::default(), 2 << 20)
        .unwrap();
    assert_eq!(resolved.commit.seq, target);
    assert_eq!(view.len(), sparkles_core::index::BLOCK_ROWS as u64);
    assert!(
        before == durable_bytes(root.path()),
        "read/refusal modified durable bytes"
    );
}
