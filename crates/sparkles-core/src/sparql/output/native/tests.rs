use super::*;
use crate::{
    algo::memory::probe,
    error::BudgetKind,
    sparql::{QueryOptions, pool::QueryPool, query, query_memory::LEDGER_OPS},
    store::{Store, StoreOptions},
    task::Control,
};
use serde::Serialize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

fn receiver(
    local: u64,
    server: u64,
    cancel: Arc<AtomicBool>,
) -> (
    NativeBudget,
    Preparation,
    Arc<crate::sparql::pool::PoolGrant>,
    Arc<QueryPool>,
) {
    let pool = QueryPool::new(server);
    let grant = Arc::new(pool.empty_grant());
    let preparation =
        Preparation::with_grant(Control::with_cancel(cancel), local, grant.clone()).unwrap();
    let budget = NativeBudget::from_preparation(&preparation).unwrap();
    (budget, preparation, grant, pool)
}

#[test]
fn preparation_text_and_json_share_real_credit_until_their_last_physical_owners() {
    let (budget, preparation, grant, pool) = receiver(4 << 20, 16 << 20, Arc::default());
    let before = grant.used();
    let payload = "x".repeat(64 << 10);
    let text = budget
        .format_text(&format_args!("prefix:{payload}"))
        .unwrap();
    let document = budget.json(&(&payload, &text)).unwrap();
    let last_text = text.clone();
    let last_document = document.clone();
    assert_eq!(text.as_str().as_ptr(), last_text.as_str().as_ptr());
    assert_eq!(document.as_str().as_ptr(), last_document.as_str().as_ptr());
    assert!(grant.used() >= before + text.len() as u64 + document.len() as u64);
    drop(text);
    drop(document);
    drop(budget);
    drop(preparation);
    assert!(grant.used() >= last_text.len() as u64 + last_document.len() as u64);
    drop(last_document);
    assert!(grant.used() >= last_text.len() as u64);
    assert!(last_text.as_str().ends_with(&payload));
    drop(last_text);
    assert_eq!(grant.used(), 0);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn preparation_local_and_server_refusal_retire_actual_nonempty_json_prefix() {
    for (local, server, kind, limit) in [
        (4096, 16 << 20, BudgetKind::Memory, 4096),
        (4 << 20, 8192, BudgetKind::ServerMemory, 8192),
    ] {
        let (budget, preparation, grant, pool) = receiver(local, server, Arc::default());
        let before = grant.used();
        let allocations = Arc::new(AtomicUsize::new(0));
        let observed = allocations.clone();
        let hook = probe::install(
            move |point| {
                if matches!(point, probe::Point::Allocation("native-json-buffer", _)) {
                    observed.fetch_add(1, Ordering::SeqCst);
                }
            },
            None,
        );
        let payload = "x".repeat(64 << 10);
        assert!(
            matches!(budget.json(&payload), Err(Error::BudgetExceeded(b)) if b.kind == kind && b.limit == limit && b.requested > b.limit)
        );
        assert_eq!(
            allocations.load(Ordering::SeqCst),
            1,
            "the real quote prefix existed before the next growth refused"
        );
        assert_eq!(grant.used(), before);
        assert_eq!(payload.len(), 64 << 10);
        drop(hook);
        drop(budget);
        drop(preparation);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }
}

#[test]
fn preparation_allocation_fault_and_original_cancel_override_secondary_serde_error() {
    struct CancelAfter(Arc<AtomicBool>);
    impl Serialize for CancelAfter {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            use serde::ser::Error as _;
            let _value = serializer.serialize_str("real encoded prefix")?;
            self.0.store(true, Ordering::SeqCst);
            Err(S::Error::custom("secondary error"))
        }
    }
    for cancel_late in [false, true] {
        let cancel = Arc::new(AtomicBool::new(false));
        let (budget, preparation, grant, pool) = receiver(4 << 20, 16 << 20, cancel.clone());
        let before = grant.used();
        if cancel_late {
            assert!(matches!(
                budget.json(&CancelAfter(cancel)),
                Err(Error::Cancelled)
            ));
        } else {
            let hook = probe::install(|_| {}, Some("native-json-buffer"));
            assert!(matches!(budget.json(&"value"), Err(Error::Invalid(_))));
            drop(hook);
        }
        assert_eq!(grant.used(), before);
        drop(budget);
        drop(preparation);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }
}

#[test]
fn independent_preparation_copy_borrows_stopped_query_bytes_and_retains_its_own_receiver() {
    let store = Store::in_memory(StoreOptions::default());
    let source_pool = QueryPool::new(16 << 20);
    let result = query(
        store.snapshot(),
        "ASK {}",
        &QueryOptions {
            pool: Some(source_pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let source = result.output_budget().json(&"x".repeat(64 << 10)).unwrap();
    let source_context = Arc::downgrade(&result.inner.ctx);
    result.inner.ctx.cancel.store(true, Ordering::SeqCst);
    let (budget, preparation, grant, pool) = receiver(4 << 20, 16 << 20, Arc::default());
    let copied = source.try_copy_in_native(&budget).unwrap();
    assert_eq!(source.as_str(), copied.as_str());
    assert_ne!(source.as_str().as_ptr(), copied.as_str().as_ptr());
    let last = copied.clone();
    drop(copied);
    drop(source);
    drop(result);
    drop(store);
    assert!(source_context.upgrade().is_none());
    assert_eq!(source_pool.granted(), 0);
    drop(budget);
    drop(preparation);
    assert!(grant.used() >= last.len() as u64);
    assert_eq!(last.len(), (64 << 10) + 2);
    drop(last);
    assert_eq!(grant.used(), 0);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn zero_native_reservations_keep_real_owners_without_credit_or_ledger_promotion() {
    let store = Store::in_memory(StoreOptions::default());
    let result = query(
        store.snapshot(),
        "ASK {}",
        &QueryOptions {
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let before = result.inner.ctx.live_bytes();
    LEDGER_OPS.with(|ops| ops.set(0));
    let budget = result.output_budget().native_budget().unwrap();
    let mut zero = budget.reserve(0).unwrap();
    zero.grow_to(0).unwrap();
    assert_eq!(zero.bytes(), 0);
    assert_eq!(result.inner.ctx.live_bytes(), before);
    assert_eq!(LEDGER_OPS.with(|ops| ops.get()), 0);
    drop(zero);
    drop(budget);
    drop(result);
    let (budget, preparation, grant, pool) = receiver(4096, 16 << 20, Arc::default());
    let before = grant.used();
    let mut zero = budget.reserve(0).unwrap();
    zero.grow_to(0).unwrap();
    assert_eq!(grant.used(), before);
    zero.grow_to(64).unwrap();
    assert_eq!(grant.used(), before + 64);
    drop(zero);
    assert_eq!(grant.used(), before);
    drop(budget);
    drop(preparation);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn encoded_frame_keeps_actual_capacity_and_credit_through_last_clone() {
    let (budget, preparation, grant, pool) = receiver(4 << 20, 16 << 20, Arc::default());
    let payload = "x".repeat(64 << 10);
    let before = grant.used();
    let frame = budget
        .encode_bytes(|writer| {
            writer.write_all(b"data: ")?;
            serde_json::to_writer(&mut *writer, &payload).map_err(std::io::Error::other)?;
            writer.write_all(b"\n\n")
        })
        .unwrap();
    let last = frame.clone();
    assert_eq!(frame.as_ref().as_ptr(), last.as_ref().as_ptr());
    let held = grant.used();
    assert!(held >= before + frame.len() as u64);
    assert_eq!(&frame.as_ref()[..7], b"data: \"");
    assert!(frame.as_ref().ends_with(b"\"\n\n"));
    drop(frame);
    assert_eq!(grant.used(), held);
    drop(budget);
    drop(preparation);
    assert!(grant.used() >= last.len() as u64);
    drop(last);
    assert_eq!(grant.used(), 0);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn encoded_frame_real_prefix_refunds_and_preserves_original_io_and_control_order() {
    for cancel_late in [false, true] {
        let cancel = Arc::new(AtomicBool::new(false));
        let (budget, preparation, grant, pool) = receiver(4 << 20, 16 << 20, cancel.clone());
        let before = grant.used();
        let error = budget
            .encode_bytes(|writer| {
                writer.write_all(b"actual nonempty prefix")?;
                assert!(grant.used() > before);
                if cancel_late {
                    cancel.store(true, Ordering::SeqCst);
                }
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "first I/O",
                ))
            })
            .unwrap_err();
        if cancel_late {
            assert!(matches!(error, Error::Cancelled));
        } else {
            assert!(
                matches!(error, Error::Io(ref error) if error.kind() == std::io::ErrorKind::BrokenPipe && error.to_string() == "first I/O")
            );
        }
        assert_eq!(grant.used(), before);
        drop(budget);
        drop(preparation);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }
}

#[test]
fn encoded_frame_does_not_publish_partial_bytes_after_real_growth_refusal_or_panic() {
    let (budget, preparation, grant, pool) = receiver(4096, 16 << 20, Arc::default());
    let before = grant.used();
    let large = "x".repeat(64 << 10);
    let error = budget
        .encode_bytes(|writer| {
            writer.write_all(b"real prefix")?;
            assert!(grant.used() > before);
            writer.write_all(large.as_bytes())
        })
        .unwrap_err();
    assert!(
        matches!(error, Error::BudgetExceeded(ref b) if b.kind == BudgetKind::Memory && b.limit == 4096)
    );
    assert_eq!(grant.used(), before);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = budget.encode_bytes(|writer| {
            writer.write_all(b"real prefix before original panic")?;
            assert!(grant.used() > before);
            panic!("original frame encoder panic")
        });
    }));
    assert_eq!(
        panic.unwrap_err().downcast_ref::<&str>(),
        Some(&"original frame encoder panic")
    );
    assert_eq!(grant.used(), before);
    assert_eq!(large.len(), 64 << 10);
    drop(budget);
    drop(preparation);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}
