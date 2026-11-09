//! Independent ownership scans and actual insertion admission controls.
use super::*;
use crate::classify::a1_probe_alloc::measure;
use crate::classify::numeric::Budget;
use crate::classify::{Error, Limits, Saturation};
use std::mem::size_of;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

fn value(id: u32, capacity: usize) -> DataVal {
    let mut opaque = Vec::with_capacity(capacity.max(1));
    opaque.push(id);
    DataVal {
        known: super::super::data::Known::Top,
        opaque,
    }
}

fn scanned_bytes(it: &Interner) -> (usize, usize) {
    // Read both owners independently, without the production cache or owned_bytes.
    let opaque = it
        .vals
        .iter()
        .map(|v| v.opaque.capacity() * size_of::<u32>())
        .sum::<usize>()
        + it.val_ids
            .keys()
            .map(|v| v.opaque.capacity() * size_of::<u32>())
            .sum::<usize>();
    let containers = it.vals.capacity() * size_of::<DataVal>()
        + super::super::saturate::table_bytes(it.val_ids.capacity(), size_of::<(DataVal, u32)>());
    (opaque, containers + opaque)
}

fn assert_cache(it: &Interner) {
    let (opaque, bytes) = scanned_bytes(it);
    assert_eq!(it.val_opaque_bytes, opaque);
    assert_eq!(it.data_bytes(), bytes);
}

#[test]
fn both_opaque_owners_duplicates_chunks_and_actual_reset_match_full_scan() {
    let mut it = Interner::new();
    assert_cache(&it);
    for id in 0..64 {
        assert_eq!(it.val(value(id, id as usize + 17)), id);
        assert_cache(&it);
    }
    // The moved key retains its deliberately oversized capacity; Vec::clone owns
    // its separate allocation. Counting only lengths or only one owner fails this.
    assert!(
        it.val_ids
            .keys()
            .any(|v| v.opaque.capacity() > v.opaque.len())
    );
    let before = (
        it.data_bytes(),
        it.val_opaque_bytes,
        it.vals.len(),
        it.val_ids.len(),
    );
    let duplicate = value(17, 1024);
    let (id, sample) = measure(|| it.val(duplicate));
    assert_eq!(id, 17);
    assert_eq!(sample.events, 0, "deduplication allocated storage");
    assert_eq!(
        before,
        (
            it.data_bytes(),
            it.val_opaque_bytes,
            it.vals.len(),
            it.val_ids.len()
        )
    );
    assert_cache(&it);

    let mut chunk = Interner::chunk(&FxHashMap::default());
    assert!(!chunk.chunk_has_data());
    chunk.val(value(91, 113));
    assert!(chunk.chunk_has_data());
    assert_cache(&chunk);
    let mut sat = Saturation::new();
    sat.interner = it;
    sat.reset();
    assert!(sat.interner.vals.is_empty() && sat.interner.val_ids.is_empty());
    assert_eq!(sat.interner.val_opaque_bytes, 0);
    assert_cache(&sat.interner);
    assert_eq!(sat.interner.val(value(17, 29)), 0);
    assert_cache(&sat.interner);
}

#[test]
fn actual_interner_retains_only_actual_growth_and_keeps_peak_credit() {
    let mut it = Interner::new();
    let granted = Arc::new(AtomicUsize::new(0));
    let g = granted.clone();
    let budget = Budget::new(
        &Limits {
            charge: Some(Arc::new(move |bytes| {
                g.fetch_add(bytes, Ordering::Relaxed);
                true
            })),
            ..Limits::default()
        },
        it.data_bytes(),
        None,
    );
    it.ranges.begin(budget.clone());
    for id in 0..256 {
        let input = value(id, 3 + id as usize % 23);
        assert_eq!(it.val(input), id);
        it.ranges.check().unwrap();
        assert_cache(&it);
        assert_eq!(
            budget.retained_storage(),
            (it.data_bytes(), 0),
            "insertion {id} retained an allowance rather than actual storage"
        );
        assert_eq!(granted.load(Ordering::Relaxed), budget.peak());
        assert!(budget.peak() >= it.data_bytes());
    }
    let peak = budget.peak();
    let spare_credit = peak - it.data_bytes();
    assert!(
        spare_credit > 0,
        "replacement overlap should leave reusable peak credit"
    );
    let temporary = budget.reserve(spare_credit).unwrap();
    drop(temporary);
    assert_eq!(granted.load(Ordering::Relaxed), peak);
    assert_eq!(budget.retained_storage(), (it.data_bytes(), 0));
    // Querying bytes does no ownership work after publication.
    a1_reset_accounting_visits();
    for _ in 0..256 {
        assert_eq!(it.data_bytes(), scanned_bytes(&it).1);
    }
    assert_eq!(a1_accounting_visits(), 0);
}

#[test]
fn actual_interner_refuses_before_allocation_then_reuses_after_a_new_run() {
    for pool in [false, true] {
        let mut it = Interner::new();
        let budget = Budget::new(
            &Limits {
                max_memory: (!pool).then_some(0),
                charge: pool
                    .then(|| Arc::new(|_| false) as Arc<dyn Fn(usize) -> bool + Send + Sync>),
                ..Limits::default()
            },
            0,
            None,
        );
        it.ranges.begin(budget.clone());
        let input = value(7, 71);
        let (_, sample) = measure(|| it.val(input));
        assert_eq!(
            sample.events, 0,
            "pool={pool} allocated after refusing insertion"
        );
        assert!(matches!(it.ranges.check(), Err(Error::Memory { .. })));
        assert!(it.vals.is_empty() && it.val_ids.is_empty());
        assert_eq!(budget.retained_storage(), (0, 0));
        assert_eq!(budget.peak(), 0);
        assert_cache(&it);
        it.ranges.end();
        it.ranges
            .begin(Budget::new(&Limits::default(), it.data_bytes(), None));
        assert_eq!(it.val(value(7, 71)), 0);
        it.ranges.check().unwrap();
        assert_eq!(it.vals.len(), 1);
        assert_cache(&it);
    }
}

#[test]
fn cancellation_after_grant_retains_actual_inserted_storage_and_halts_following_work() {
    let flag = Arc::new(AtomicBool::new(false));
    let (c, granted) = (flag.clone(), flag.clone());
    let mut it = Interner::new();
    let budget = Budget::new(
        &Limits {
            cancelled: Some(Arc::new(move || c.load(Ordering::Relaxed))),
            charge: Some(Arc::new(move |_| {
                granted.store(true, Ordering::Relaxed);
                true
            })),
            ..Limits::default()
        },
        0,
        None,
    );
    it.ranges.begin(budget.clone());
    it.val(value(1, 19));
    assert_eq!(it.ranges.check(), Err(Error::Cancelled));
    assert_eq!(it.vals.len(), 1);
    assert_cache(&it);
    assert_eq!(budget.retained_storage(), (it.data_bytes(), 0));
    let input = value(2, 37);
    let (_, sample) = measure(|| it.val(input));
    assert_eq!(sample.events, 0);
    assert_eq!(it.vals.len(), 1);
    it.ranges.end();
    flag.store(false, Ordering::Relaxed);
    it.ranges
        .begin(Budget::new(&Limits::default(), it.data_bytes(), None));
    assert_eq!(it.val(value(2, 37)), 1);
    it.ranges.check().unwrap();
    assert_cache(&it);
}

#[test]
fn checked_capacity_and_cached_total_overflow_refuse_without_allocation() {
    assert_eq!(next_value_capacity(0, 0), Some(4));
    assert_eq!(next_value_capacity(3, 4), Some(4));
    assert_eq!(next_value_capacity(4, 4), Some(8));
    assert_eq!(next_value_capacity(usize::MAX, usize::MAX), None);
    assert_eq!(
        next_value_capacity(usize::MAX / 2 + 1, usize::MAX / 2 + 1),
        None
    );
    let mut it = Interner::new();
    // A synthetic unrepresentable retained total exercises the checked boundary
    // without trying to manufacture an invalid Vec or allocate host-sized storage.
    it.val_opaque_bytes = usize::MAX;
    let input = value(1, 1);
    let (_, sample) = measure(|| it.val(input));
    assert_eq!(sample.events, 0);
    assert!(matches!(it.ranges.check(), Err(Error::Memory { .. })));
    assert!(it.vals.is_empty() && it.val_ids.is_empty());
}

#[test]
fn geometric_replacement_refusal_preserves_old_owners_before_allocation() {
    let mut it = Interner::new();
    for id in 0..4 {
        assert_eq!(it.val(value(id, 1)), id);
    }
    assert_eq!(it.vals.len(), it.vals.capacity());
    assert!(
        it.val_ids.len() < it.val_ids.capacity(),
        "isolate vector replacement from a map rehash"
    );
    let before = it.data_bytes();
    let cache_before = it.val_opaque_bytes;
    // Independent overlap bound: the existing containers remain live while the
    // complete doubled vector is allocated. Admitting only its net delta would
    // incorrectly let this insertion proceed under the smaller limit.
    let replacement = 2 * it.vals.capacity() * size_of::<DataVal>();
    let budget = Budget::new(
        &Limits {
            max_memory: Some(before + replacement - 1),
            ..Limits::default()
        },
        before,
        None,
    );
    it.ranges.begin(budget.clone());
    let input = value(4, 1);
    let (_, sample) = measure(|| it.val(input));
    assert_eq!(
        sample.events, 0,
        "replacement allocated beyond its overlap limit"
    );
    assert!(matches!(it.ranges.check(), Err(Error::Memory { .. })));
    assert_eq!(it.vals.len(), 4);
    assert_eq!(it.val_ids.len(), 4);
    assert_eq!(it.val_opaque_bytes, cache_before);
    assert_eq!(it.data_bytes(), before);
    assert_eq!(budget.retained_storage(), (before, 0));
    assert_cache(&it);
    it.ranges.end();
    let unrestricted = Budget::new(&Limits::default(), before, None);
    it.ranges.begin(unrestricted.clone());
    assert_eq!(it.val(value(4, 1)), 4);
    it.ranges.check().unwrap();
    assert_eq!(it.vals.len(), 5);
    assert_cache(&it);
    assert_eq!(unrestricted.retained_storage(), (it.data_bytes(), 0));
}
