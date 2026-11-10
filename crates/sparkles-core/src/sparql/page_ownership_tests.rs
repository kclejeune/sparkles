use super::*;
use crate::{
    algo::memory::probe,
    io::{RdfFormat, Source},
    store::{Store, StoreOptions},
};
use std::sync::atomic::{AtomicUsize, Ordering};

fn page() -> (QueryResult, Arc<pool::QueryPool>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path(), StoreOptions::default()).unwrap();
    store
        .load(&[Source::from_bytes(
            format!(
                "<urn:s0> <urn:p> \"{}\" .\n<urn:s1> <urn:p> \"{}\" .\n<urn:s2> <urn:p> \"{}\" .",
                "x".repeat(64 << 10),
                "m".repeat(64 << 10),
                "y".repeat(64 << 10),
            )
            .into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let pool = pool::QueryPool::new(16 << 20);
    let result = query(
        store.snapshot(),
        "SELECT ?v ?missing WHERE { ?s <urn:p> ?v } ORDER BY ?s",
        &QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(result.inner.table.width(), 2);
    assert!(
        result.inner.table.cols[0]
            .iter()
            .all(|id| id.tag() == crate::id::Tag::Vocab)
    );
    drop(store);
    (result, pool, directory)
}

#[test]
fn selected_page_owns_only_its_actual_decoded_payload_and_survives_result_drop() {
    let (result, pool, _directory) = page();
    let copies = Arc::new(AtomicUsize::new(0));
    let observed = copies.clone();
    *result.inner.ctx.chunk_hook.lock() = Some(Arc::new(move |stage, _| {
        // A simple literal takes over its reconstructed key's allocation, and
        // other terms are copied from it. Either way one term is decoded.
        if stage == "term-copied" || stage == "owned-key-pointer" {
            observed.fetch_add(1, Ordering::SeqCst);
        }
    }));
    let pressure = result
        .inner
        .ctx
        .charge(result.inner.ctx.memory_remaining() - (120 << 10))
        .unwrap();
    let before = result.inner.ctx.live_bytes();
    let rows = result.rows_range(1..2).unwrap();
    assert_eq!(copies.load(Ordering::SeqCst), 1);
    assert_eq!((rows.len(), rows.width()), (1, 2));
    let row = rows.row(0).unwrap();
    assert!(
        matches!(row.get(0).unwrap(), Some(oxrdf::TermRef::Literal(l)) if l.value() == "m".repeat(64 << 10))
    );
    assert!(row.get(1).unwrap().is_none());
    drop(rows);
    assert_eq!(result.inner.ctx.live_bytes(), before);
    copies.store(0, Ordering::SeqCst);
    assert!(
        matches!(result.rows_range(0..3), Err(Error::BudgetExceeded(b)) if b.kind == crate::error::BudgetKind::Memory && b.limit == 4 << 20)
    );
    assert_eq!(
        copies.load(Ordering::SeqCst),
        1,
        "a real nonempty decoded prefix preceded refusal"
    );
    assert_eq!(result.inner.ctx.live_bytes(), before);
    drop(pressure);
    drop(result.inner.ctx.chunk_hook.lock().take());
    let rows = result.rows_range(1..2).unwrap();
    let last = rows.clone();
    let source = Arc::downgrade(&result.inner.ctx);
    drop(rows);
    drop(result);
    assert!(source.upgrade().is_some());
    assert!(pool.granted() >= 64 << 10);
    assert!(
        matches!(last.row(0).unwrap().get(0).unwrap(), Some(oxrdf::TermRef::Literal(l)) if l.value() == "m".repeat(64 << 10))
    );
    drop(last);
    assert!(source.upgrade().is_none());
    assert_eq!(pool.granted(), 0);
}

#[test]
fn invalid_page_ranges_wrong_kind_and_cancellation_refuse_before_container_allocation() {
    let (result, pool, _directory) = page();
    let store = Store::in_memory(StoreOptions::default());
    let ask = query(
        store.snapshot(),
        "ASK {}",
        &QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let before = result.inner.ctx.live_bytes();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let hook = probe::install(
        move |point| {
            if matches!(point, probe::Point::Allocation("owned-output-container", _)) {
                observed.fetch_add(1, Ordering::SeqCst);
            }
        },
        None,
    );
    for range in [
        std::ops::Range { start: 2, end: 1 },
        0..4,
        4..4,
        usize::MAX..usize::MAX,
    ] {
        assert!(matches!(result.rows_range(range), Err(Error::Invalid(_))));
    }
    assert!(matches!(ask.rows_range(0..0), Err(Error::Invalid(_))));
    result.inner.ctx.cancel.store(true, Ordering::SeqCst);
    assert!(matches!(result.rows_range(1..2), Err(Error::Cancelled)));
    assert!(matches!(result.rows_range(0..0), Err(Error::Cancelled)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(result.inner.ctx.live_bytes(), before);
    drop(hook);
    drop(ask);
    drop(result);
    drop(store);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn empty_and_zero_width_pages_keep_real_shape_header_and_final_credit() {
    for zero_width in [false, true] {
        let store = Store::in_memory(StoreOptions::default());
        let pool = pool::QueryPool::new(16 << 20);
        let text = if zero_width {
            "SELECT * WHERE {}"
        } else {
            "SELECT ?v WHERE { VALUES ?v {} }"
        };
        let result = query(
            store.snapshot(),
            text,
            &QueryOptions {
                pool: Some(pool.clone()),
                max_memory_bytes: Some(4 << 20),
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        let expected = if zero_width { (1, 0) } else { (0, 1) };
        assert_eq!((result.len(), result.inner.table.width()), expected);
        let before = result.inner.ctx.live_bytes();
        let rows = result.rows_range(0..result.len()).unwrap();
        assert_eq!((rows.len(), rows.width()), expected);
        assert!(
            result.inner.ctx.live_bytes() > before,
            "even empty pages have actual admitted backing headers"
        );
        if zero_width {
            assert!(rows.row(0).unwrap().values().is_empty());
        }
        let source = Arc::downgrade(&result.inner.ctx);
        drop(result);
        drop(store);
        assert!(source.upgrade().is_some());
        assert!(pool.granted() > 0);
        drop(rows);
        assert!(source.upgrade().is_none());
        assert_eq!(pool.granted(), 0);
    }
}

#[test]
fn consuming_page_iterator_transfers_same_backing_without_payload_or_credit_copy() {
    let (result, pool, _directory) = page();
    let rows = result.rows_range(0..3).unwrap();
    let before = result.inner.ctx.live_bytes();
    let first_pointer = rows.row(0).unwrap().values().as_ptr();
    let last_pointer = rows.row(2).unwrap().values().as_ptr();
    let source = Arc::downgrade(&result.inner.ctx);
    result.inner.ctx.cancel.store(true, Ordering::SeqCst);
    let mut iterator = rows.into_iter();
    assert_eq!(iterator.size_hint(), (3, Some(3)));
    let first = iterator.next().unwrap();
    let last = iterator.nth(1).unwrap();
    assert_eq!(first.values().as_ptr(), first_pointer);
    assert_eq!(last.values().as_ptr(), last_pointer);
    assert_eq!(iterator.len(), 0);
    assert!(iterator.next().is_none());
    assert!(iterator.next().is_none());
    assert_eq!(result.inner.ctx.live_bytes(), before);
    assert!(
        matches!(first.get(0).unwrap(), Some(oxrdf::TermRef::Literal(l)) if l.value().starts_with('x'))
    );
    let cell = last.term(0).unwrap().unwrap();
    drop(first);
    drop(last);
    drop(result);
    assert!(source.upgrade().is_some());
    drop(iterator);
    assert!(source.upgrade().is_some());
    assert!(matches!(cell.as_term(), oxrdf::Term::Literal(l) if l.value() == "y".repeat(64 << 10)));
    drop(cell);
    assert!(source.upgrade().is_none());
    assert_eq!(pool.granted(), 0);
}

#[test]
fn consuming_empty_and_zero_width_pages_keep_shape_and_real_header_until_drop() {
    for zero_width in [false, true] {
        let store = Store::in_memory(StoreOptions::default());
        let pool = pool::QueryPool::new(16 << 20);
        let result = query(
            store.snapshot(),
            if zero_width {
                "SELECT * WHERE {}"
            } else {
                "SELECT ?v WHERE { VALUES ?v {} }"
            },
            &QueryOptions {
                pool: Some(pool.clone()),
                max_memory_bytes: Some(4 << 20),
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        let rows = result.rows_range(0..result.len()).unwrap();
        let count = usize::from(zero_width);
        let weak = Arc::downgrade(&result.inner.ctx);
        let mut iterator = rows.into_iter();
        assert_eq!(iterator.size_hint(), (count, Some(count)));
        assert_eq!(iterator.len(), count);
        drop(result);
        drop(store);
        assert!(weak.upgrade().is_some());
        assert!(pool.granted() > 0);
        if zero_width {
            let row = iterator.next().unwrap();
            assert!(row.values().is_empty());
            assert!(iterator.next().is_none());
            drop(iterator);
            assert!(weak.upgrade().is_some());
            drop(row);
        } else {
            assert!(iterator.nth(usize::MAX).is_none());
            assert!(iterator.next().is_none());
            assert_eq!(iterator.count(), 0);
        }
        assert!(weak.upgrade().is_none());
        assert_eq!(pool.granted(), 0);
    }
}
