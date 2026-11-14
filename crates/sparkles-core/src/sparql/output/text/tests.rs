use super::*;
use crate::{
    algo::memory::probe,
    error::BudgetKind,
    sparql::{QueryOptions, QueryResult, ctx::Ctx, depth, pool::QueryPool, query},
    store::{Store, StoreOptions},
};
use std::{
    cell::Cell,
    sync::atomic::{AtomicUsize, Ordering},
};

fn fixture() -> (QueryResult, Arc<QueryPool>) {
    let store = Store::in_memory(StoreOptions::default());
    let pool = QueryPool::new(16 << 20);
    let result = query(
        store.snapshot(),
        "SELECT ?v WHERE { VALUES ?v { \"seed\" } }",
        &QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    (result, pool)
}

#[test]
fn counted_text_retains_real_payload_and_original_owner_through_last_clone() {
    let (result, pool) = fixture();
    let payload = "x".repeat(64 << 10);
    let before = result.inner.ctx.live_bytes();
    let output = result
        .output_budget()
        .format_text(&format_args!("prefix:{payload}:suffix"))
        .unwrap();
    assert_eq!(output.as_str(), format!("prefix:{payload}:suffix"));
    assert!(result.inner.ctx.live_bytes() >= before + output.len() as u64);
    let last = output.clone();
    assert_eq!(output.as_str().as_ptr(), last.as_str().as_ptr());
    let weak = Arc::downgrade(&result.inner.ctx);
    drop(output);
    drop(result);
    assert!(weak.upgrade().is_some());
    assert!(pool.granted() >= 64 << 10);
    assert!(last.as_str().starts_with("prefix:"));
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(pool.granted(), 0);
}

#[test]
fn full_count_refuses_before_destination_allocation_and_keeps_borrowed_source() {
    let (result, pool) = fixture();
    let payload = "x".repeat(64 << 10);
    let ctx = &result.inner.ctx;
    let header = crate::sparql::cursor::arc_backing_bytes::<TextBacking>(ctx).unwrap();
    let pressure = ctx
        .charge(ctx.memory_remaining() - header - payload.len() as u64 + 1)
        .unwrap();
    let before = ctx.live_bytes();
    let allocations = Arc::new(AtomicUsize::new(0));
    let observed = allocations.clone();
    let hook = probe::install(
        move |point| {
            if matches!(point, probe::Point::Allocation("owned-output-text", 1)) {
                observed.fetch_add(1, Ordering::SeqCst);
            }
        },
        None,
    );
    assert!(
        matches!(result.output_budget().format_text(payload.as_str()), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory && b.limit == 4 << 20)
    );
    assert_eq!(allocations.load(Ordering::SeqCst), 0);
    assert_eq!(ctx.live_bytes(), before);
    assert_eq!(payload.len(), 64 << 10);
    drop(hook);
    drop(pressure);
    let output = result
        .output_budget()
        .format_text(payload.as_str())
        .unwrap();
    assert_eq!(output.as_str(), payload);
    drop(output);
    drop(result);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn changed_formatter_count_refuses_before_growth_and_refunds_actual_prefix() {
    struct Changed<'a> {
        calls: Cell<usize>,
        second: &'a str,
    }
    impl fmt::Display for Changed<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let call = self.calls.get();
            self.calls.set(call + 1);
            if call == 0 {
                f.write_str("ab")
            } else {
                f.write_str("a")?;
                f.write_str(self.second)
            }
        }
    }
    let (result, pool) = fixture();
    let before = result.inner.ctx.live_bytes();
    let large = "x".repeat(64 << 10);
    for second in [large.as_str(), ""] {
        let value = Changed {
            calls: Cell::new(0),
            second,
        };
        assert!(matches!(
            result.output_budget().format_text(&value),
            Err(Error::Invalid(_))
        ));
        assert_eq!(value.calls.get(), 2);
        assert_eq!(result.inner.ctx.live_bytes(), before);
    }
    // Directly exercise the very same production writer with an actual one-byte
    // allocation. The rejected write leaves pointer, capacity and prefix intact.
    let native = result.output_budget().native_budget().unwrap();
    let mut writer = Bounded {
        text: String::with_capacity(1),
        bytes: 1,
        budget: &native,
        first: None,
    };
    let pointer = writer.text.as_ptr();
    fmt::Write::write_str(&mut writer, "a").unwrap();
    assert!(fmt::Write::write_str(&mut writer, &large).is_err());
    assert_eq!(writer.text, "a");
    assert_eq!(writer.text.capacity(), 1);
    assert_eq!(writer.text.as_ptr(), pointer);
    drop(writer);
    drop(native);
    drop(result);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn original_cancel_and_caller_panic_retire_partial_text_without_secondary_error() {
    struct Fails<'a> {
        calls: Cell<usize>,
        ctx: &'a Ctx,
        panic: bool,
    }
    impl fmt::Display for Fails<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let call = self.calls.get();
            self.calls.set(call + 1);
            f.write_str("actual prefix")?;
            if call == 1 {
                if self.panic {
                    panic!("original formatter panic");
                }
                self.ctx.cancel.store(true, Ordering::SeqCst);
                let _ = f.write_str("secondary write");
                return Err(fmt::Error);
            }
            Ok(())
        }
    }
    for panic in [false, true] {
        let (result, pool) = fixture();
        let ctx = &result.inner.ctx;
        let before = ctx.live_bytes();
        let value = Fails {
            calls: Cell::new(0),
            ctx,
            panic,
        };
        let output = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            result.output_budget().format_text(&value)
        }));
        if panic {
            let error = output.err().unwrap();
            assert_eq!(
                error.downcast_ref::<&str>(),
                Some(&"original formatter panic")
            );
        } else {
            assert!(matches!(output.unwrap(), Err(Error::Cancelled)));
        }
        assert_eq!(value.calls.get(), 2);
        assert_eq!(ctx.live_bytes(), before);
        drop(result);
        assert_eq!(pool.granted(), 0);
    }
}

#[test]
fn built_in_nested_term_formatting_keeps_its_protected_small_stack_contract() {
    let (result, pool) = fixture();
    let term = depth::with_stack(256, || {
        let mut term = oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal("leaf"));
        for _ in 0..256 {
            term = oxrdf::Term::Triple(Box::new(oxrdf::Triple::new(
                oxrdf::NamedNode::new_unchecked("urn:s"),
                oxrdf::NamedNode::new_unchecked("urn:p"),
                term,
            )));
        }
        term
    });
    let output = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(32 << 10)
            .spawn_scoped(scope, || result.output_budget().term_text(&term).unwrap())
            .unwrap()
            .join()
            .unwrap()
    });
    assert_eq!(output.as_str().matches("urn:p").count(), 256);
    assert!(output.as_str().contains("leaf"));
    depth::with_stack(256, || drop(term));
    drop(result);
    assert!(pool.granted() > 0);
    drop(output);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn formatter_admission_preserves_original_server_pool_refusal() {
    let store = Store::in_memory(StoreOptions::default());
    let pool = QueryPool::new(8 << 20);
    let result = query(
        store.snapshot(),
        "SELECT ?v WHERE { VALUES ?v { \"seed\" } }",
        &QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(32 << 20),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let ctx = &result.inner.ctx;
    let pressure = ctx.charge((8 << 20) - ctx.live_bytes() - 1).unwrap();
    let before = ctx.live_bytes();
    assert!(
        matches!(result.output_budget().format_text(&format_args!("borrowed {}", "receiver")), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::ServerMemory && b.limit == 8 << 20 && b.requested > b.limit)
    );
    assert_eq!(ctx.live_bytes(), before);
    drop(pressure);
    let output = result
        .output_budget()
        .format_text(&format_args!("borrowed {}", "receiver"))
        .unwrap();
    assert_eq!(output.as_str(), "borrowed receiver");
    drop(output);
    drop(result);
    drop(store);
    assert_eq!(pool.granted(), 0);
}
