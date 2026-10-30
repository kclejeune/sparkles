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

// The measured replay/decoder path is synchronous and owns no worker-thread
// allocations. Shared background work remains visible in global telemetry. A
// future offload must propagate this scope before these assertions remain valid.
// Cumulative allocation never subtracts frees, including cross-thread/pre-window
// storage, and therefore conservatively bounds newly owned caller storage.
#[derive(Clone, Copy)]
struct CallerCounts {
    active: bool,
    largest: usize,
    allocated: usize,
}
thread_local! {
    static CALLER: std::cell::Cell<CallerCounts> = const {
        std::cell::Cell::new(CallerCounts { active: false, largest: 0, allocated: 0 })
    };
}
fn caller_allocated(bytes: usize) {
    let _ = CALLER.try_with(|cell| {
        let mut counts = cell.get();
        if counts.active {
            counts.largest = counts.largest.max(bytes);
            counts.allocated = counts.allocated.saturating_add(bytes);
            cell.set(counts);
        }
    });
}

// SAFETY: allocation/deallocation use System with the caller's layout. Reallocation
// deliberately moves: allocate with the original alignment, copy the allocation
// prefix, then free the old block. Failure preserves the original allocation. This
// isolated test allocator observes both blocks before releasing the old one.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            caller_allocated(layout.size());
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
            caller_allocated(size);
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
    CALLER.with(|cell| {
        cell.set(CallerCounts {
            active: true,
            largest: 0,
            allocated: 0,
        })
    });
    let base = LIVE.load(Ordering::SeqCst);
    LARGEST.store(0, Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    struct Measuring;
    impl Drop for Measuring {
        fn drop(&mut self) {
            ENABLED.store(false, Ordering::SeqCst);
            let _ = CALLER.try_with(|cell| {
                let mut counts = cell.get();
                counts.active = false;
                cell.set(counts);
            });
        }
    }
    ENABLED.store(true, Ordering::SeqCst);
    let measuring = Measuring;
    let result = operation();
    drop(measuring);
    let caller = CALLER.with(std::cell::Cell::get);
    eprintln!(
        "allocation attribution: global_largest={}, global_peak={}, caller_largest={}, caller_total={}",
        LARGEST.load(Ordering::SeqCst),
        PEAK.load(Ordering::SeqCst).saturating_sub(base),
        caller.largest,
        caller.allocated
    );
    (result, caller.largest, caller.allocated)
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;
#[test]
fn measurement_excludes_explicit_background_allocation_from_caller_bound() {
    use std::sync::{Arc, Barrier};
    let _lock = MEASURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let barrier = Arc::new(Barrier::new(2));
    let other = barrier.clone();
    let worker = std::thread::spawn(move || {
        other.wait();
        let allocation = vec![1u8; 700_000];
        std::hint::black_box(&allocation);
        other.wait();
        other.wait();
        drop(allocation);
    });
    let (allocation, largest, bound) = measure(|| {
        barrier.wait();
        barrier.wait();
        let allocation = vec![2u8; 128];
        std::hint::black_box(&allocation);
        allocation
    });
    barrier.wait();
    worker.join().unwrap();
    let caller = CALLER.with(std::cell::Cell::get);
    assert!(
        LARGEST.load(Ordering::SeqCst) >= 700_000,
        "outside allocation did not occur in the measured window"
    );
    assert!(caller.largest < 8192 && caller.allocated < 8192);
    assert!(
        largest < 8192 && bound < 8192,
        "background allocation contaminated caller admission"
    );
    assert_eq!(allocation.len(), 128);
}

#[test]
fn measurement_preexisting_frees_cannot_hide_new_caller_storage() {
    let _lock = MEASURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for cross_thread in [false, true] {
        let previous = if cross_thread {
            std::thread::spawn(|| vec![1u8; 1 << 20]).join().unwrap()
        } else {
            vec![1u8; 1 << 20]
        };
        std::hint::black_box(&previous);
        let (allocations, _, bound) = measure(|| {
            drop(previous);
            let allocations = (vec![2u8; 70_000], vec![3u8; 70_000]);
            std::hint::black_box(&allocations);
            allocations
        });
        let caller = CALLER.with(std::cell::Cell::get);
        assert!(caller.allocated >= 140_000);
        assert!(bound >= 140_000, "a pre-window free hid new caller storage");
        assert_eq!(allocations.0.len() + allocations.1.len(), 140_000);
    }
}

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
    let (result, largest, bound) =
        measure(|| store.snapshot_at_limited(&At::Commit(1), &HistoryOptions::default(), cap));
    assert!(matches!(result, Err(Error::BudgetExceeded(_))));
    eprintln!("bounded WAL refusal: largest={largest}, allocation_bound={bound}, cap={cap}");
    assert!(
        largest <= cap as usize,
        "whole transaction was allocated first: {largest}"
    );
    assert!(
        bound <= cap as usize,
        "cumulative caller allocation exceeded cap: {bound}"
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
    let (result, largest, bound) =
        measure(|| store.snapshot_at_limited(&selector, &HistoryOptions::default(), 16));
    assert!(
        matches!(result, Err(Error::BudgetExceeded(_))),
        "selector name copied before metadata admission"
    );
    eprintln!("selector metadata refusal: largest={largest}, allocation_bound={bound}, cap=16");
    assert!(
        largest <= 16 && bound <= 16,
        "selector metadata copied before refusal: {largest}/{bound}"
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
    let (result, largest, bound) =
        measure(|| store.snapshot_at_limited(&At::Commit(1), &HistoryOptions::default(), cap));
    assert!(matches!(result, Err(Error::BudgetExceeded(_))));
    eprintln!("moving WAL refusal: largest={largest}, allocation_bound={bound}, cap={cap}");
    assert!(largest <= cap as usize);
    assert!(
        bound <= cap as usize,
        "moving allocation bound exceeded cap: {bound}"
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
    let (result, largest, bound) =
        measure(|| store.snapshot_at_limited(&At::Commit(target), &HistoryOptions::default(), cap));
    eprintln!("whole-column refusal: largest={largest}, allocation_bound={bound}, cap={cap}");
    assert!(
        matches!(result, Err(Error::BudgetExceeded(_))),
        "base index decoded without admission"
    );
    assert!(largest <= cap as usize);
    assert!(bound <= cap as usize, "base decode exceeded cap: {bound}");
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
    let (result, largest, bound) =
        measure(|| store.snapshot_at_limited(&At::Commit(target), &HistoryOptions::default(), cap));
    eprintln!(
        "backward whole-column refusal: largest={largest}, allocation_bound={bound}, cap={cap}"
    );
    assert!(
        matches!(result, Err(Error::BudgetExceeded(_))),
        "backward base index decoded without admission"
    );
    assert!(largest <= cap as usize);
    assert!(
        bound <= cap as usize,
        "backward base decode exceeded cap: {bound}"
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

/// Permission scans must admit decode and mask work before allocating them, just
/// as retained replay does. Reuse this binary's existing moving/counting allocator.
#[test]
fn controlled_permissions_refuse_before_oversized_scan_and_mask_allocations() {
    use sparkles_core::access::{
        Caller, GraphAccess, Graphs, Limits, Preparation, Protection, Rule, TripleRules,
    };
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::task::Control;
    use std::sync::Arc;
    let _serial = MEASURE_LOCK.lock().unwrap();
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for row in 0..6000 {
        data.push_str(&format!("<urn:s{row}> <urn:secret> <urn:o> .\n"));
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    store.compact().unwrap();
    let snap = store.snapshot();
    for classes in [None, Some(vec!["urn:Class".into()])] {
        let view = GraphAccess::with_triples(
            Graphs::All,
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "secret".into(),
                        predicates: Some(vec!["urn:s*".into()]),
                        classes,
                        subclasses: true,
                        graphs: None,
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        );
        let cap = 32 << 10;
        let preparation = Preparation::new(Control::none(), cap).unwrap();
        let (result, largest, allocated) = measure(|| view.masked_controlled(&snap, &preparation));
        assert!(
            matches!(result, Err(Error::BudgetExceeded(ref budget)) if budget.kind == sparkles_core::BudgetKind::Memory && budget.limit == cap && budget.requested == cap + 1)
        );
        assert!(
            largest <= cap as usize,
            "oversized allocation occurred before refusal: {largest}"
        );
        // This refused call performs only its small policy/key work. Its cumulative
        // caller allocation also bounds its newly owned peak, without subtracting
        // pre-window or other-thread frees.
        assert!(
            allocated <= cap as usize,
            "oversized preparation occurred before refusal: {allocated}"
        );
        assert_eq!(store.snapshot().commit, snap.commit);
        assert_eq!(store.snapshot().len(), 6000);
    }
}

/// An opt-in local cost diagnostic. The allocator and cold/warm stores are held
/// fixed across the two entry points. Published performance results use the
/// repository benchmark workflow; this produces private raw acceptance evidence.
#[test]
#[ignore = "local preparation allocation and latency diagnostic"]
fn controlled_permission_preparation_cost_diagnostic() {
    use sparkles_core::access::{
        Caller, GraphAccess, Graphs, Limits, Preparation, Protection, Rule, TripleRules,
    };
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::task::Control;
    use std::{collections::BTreeMap, sync::Arc, time::Instant};
    let _lock = MEASURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let view = GraphAccess::with_triples(
        Graphs::All,
        Graphs::none(),
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "secret".into(),
                    predicates: Some(vec!["urn:secret".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: BTreeMap::new(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    );
    for rows in [1000, 6000] {
        let data = (0..rows)
            .map(|row| {
                format!(
                    "<urn:s{row}> <{}> <urn:o> .\n",
                    if row % 2 == 0 {
                        "urn:secret"
                    } else {
                        "urn:visible"
                    }
                )
            })
            .collect::<String>();
        for warm in [false, true] {
            for round in 0..3 {
                let build = || {
                    let store = Store::in_memory(StoreOptions::default());
                    store
                        .load(&[Source::from_bytes(
                            data.as_bytes().to_vec(),
                            RdfFormat::NTriples,
                            None,
                        )])
                        .unwrap();
                    store.compact().unwrap();
                    store
                };
                let old = build();
                let bounded = build();
                if warm {
                    view.masked(&old.snapshot()).unwrap();
                    view.masked_controlled(
                        &bounded.snapshot(),
                        &Preparation::new(Control::none(), 64 << 20).unwrap(),
                    )
                    .unwrap();
                }
                eprintln!("cost scenario rows={rows} warm={warm} round={round} path=legacy");
                let ((legacy, old_ns), old_largest, old_total) = measure(|| {
                    let start = Instant::now();
                    let result = view.masked(&old.snapshot()).unwrap();
                    (result, start.elapsed().as_nanos())
                });
                let preparation = Preparation::new(Control::none(), 64 << 20).unwrap();
                eprintln!("cost scenario rows={rows} warm={warm} round={round} path=controlled");
                let ((prepared, new_ns), new_largest, new_total) = measure(|| {
                    let start = Instant::now();
                    let result = view
                        .masked_controlled(&bounded.snapshot(), &preparation)
                        .unwrap();
                    (result, start.elapsed().as_nanos())
                });
                assert_eq!(prepared.snapshot().len(), legacy.len());
                assert_eq!(prepared.snapshot().len(), rows / 2);
                let render = |snapshot: &sparkles_core::store::Snapshot| {
                    let mut lines = snapshot
                        .scan_keys(sparkles_core::index::Perm::Spo, &[])
                        .unwrap()
                        .iter()
                        .map(|quad| {
                            quad.iter()
                                .map(|id| {
                                    snapshot
                                        .term(sparkles_core::id::Id(*id))
                                        .map(|term| term.to_string())
                                        .unwrap_or_else(|| {
                                            format!("{:?}", sparkles_core::id::Id(*id))
                                        })
                                })
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .collect::<Vec<_>>();
                    lines.sort();
                    lines
                };
                assert_eq!(render(prepared.snapshot()), render(&legacy));
                // The preceding allocation attribution lines carry actual peak
                // above the pre-call live baseline, including moving reallocs.
                eprintln!(
                    "cost sample rows={rows} warm={warm} round={round} legacy_ns={old_ns} controlled_ns={new_ns} legacy_largest={old_largest} controlled_largest={new_largest} legacy_total={old_total} controlled_total={new_total}"
                );
            }
        }
    }
}
