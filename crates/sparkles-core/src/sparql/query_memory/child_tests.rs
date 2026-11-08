use sparkles_budget::{ParentRefused, Scope};

fn child_ctx(
    pool: &std::sync::Arc<super::super::pool::QueryPool>,
    limit: u64,
) -> super::super::ctx::Ctx {
    let store = crate::store::Store::in_memory(crate::store::StoreOptions::default());
    let mut ctx = super::super::ctx::Ctx::new(store.snapshot());
    ctx.mem_limit = limit;
    ctx.set_grant(Some(std::sync::Arc::new(pool.empty_grant())));
    ctx
}

#[test]
fn reasoner_child_release_preserves_executor_ownership() {
    let pool = super::super::pool::QueryPool::new(64 << 20);
    let ctx = child_ctx(&pool, 8 << 20);
    let executor = ctx.charge(5 << 20).unwrap();
    let parent = ctx.memory_parent().unwrap();
    let grant = ctx.grant().unwrap();
    let before = (ctx.live_bytes(), grant.used());
    let child = parent.clone().reserve(3 << 20).unwrap();
    assert_eq!(
        (ctx.live_bytes(), grant.used()),
        (8 << 20, before.1 + (3 << 20))
    );
    assert!(matches!(
        parent.reserve(1),
        Err(ParentRefused::Limit {
            scope: Scope::Local,
            ..
        })
    ));
    drop(child);
    assert_eq!((ctx.live_bytes(), grant.used()), before);
    drop(executor);
    assert_eq!(ctx.live_bytes(), 0);
    drop(ctx);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn exact_child_credit_has_no_second_step_in_eager_or_cursor_mode() {
    for cursor in [false, true] {
        let pool = super::super::pool::QueryPool::new(64 << 20);
        let ctx = child_ctx(&pool, 1024);
        if cursor {
            ctx.memory().cursor_mode().unwrap();
        }
        let parent = ctx.memory_parent().unwrap();
        let lease = parent.reserve(37).unwrap();
        assert_eq!((ctx.live_bytes(), ctx.grant().unwrap().used()), (37, 37));
        drop(lease);
        assert_eq!((ctx.live_bytes(), ctx.grant().unwrap().used()), (0, 0));
    }
}

#[test]
fn reasoner_child_captures_original_grant() {
    let first_pool = super::super::pool::QueryPool::new(64 << 20);
    let second_pool = super::super::pool::QueryPool::new(64 << 20);
    let mut ctx = child_ctx(&first_pool, 8 << 20);
    let first_grant = ctx.grant().unwrap().clone();
    let child = ctx.memory_parent().unwrap();
    ctx.reconfigure_grant(Some(std::sync::Arc::new(second_pool.empty_grant())));
    let bytes = child.reserve(37).unwrap();
    assert_eq!((first_grant.used(), ctx.grant().unwrap().used()), (37, 0));
    assert_eq!(bytes.domain(), first_pool.domain());
    drop(bytes);
    assert_eq!((first_grant.used(), ctx.grant().unwrap().used()), (0, 0));
}

#[test]
fn child_parent_final_drop_retires_its_zero_account() {
    let pool = super::super::pool::QueryPool::new(64 << 20);
    let ctx = child_ctx(&pool, 8 << 20);
    let baseline = ctx.memory().lock().owners.len();
    for _ in 0..100 {
        let child = ctx.memory_parent().unwrap();
        assert_eq!(ctx.memory().lock().owners.len(), baseline + 1);
        let lease = child.reserve(1).unwrap();
        drop(lease);
        assert_eq!(ctx.memory().lock().owners.len(), baseline);
    }
}

#[test]
fn child_release_waker_can_reenter_the_query_ledger() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let pool = super::super::pool::QueryPool::new(64 << 20);
    let ctx = child_ctx(&pool, 64 << 20);
    let executor = ctx.charge(1 << 20).unwrap();
    let child = ctx.memory_parent().unwrap();
    let lease = child.clone().reserve(48 << 20).unwrap();
    let seen = std::sync::Arc::new(AtomicUsize::new(0));
    let called = seen.clone();
    let callback = child.clone();
    let waiter = pool.enter(
        32 << 20,
        Some(std::sync::Arc::new(move || {
            // A real nonzero reserve takes both the ledger and grant paths.
            drop(callback.clone().reserve(1).unwrap());
            called.fetch_add(1, Ordering::SeqCst);
        })),
    );
    drop(lease);
    assert!(seen.load(Ordering::SeqCst) > 0);
    drop(waiter);
    drop(child);
    drop(executor);
    drop(ctx);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn nested_context_shares_total_and_releases_only_its_own_credit() {
    let pool = super::super::pool::QueryPool::new(64 << 20);
    let outer = child_ctx(&pool, 8 << 20);
    let executor = outer.charge(5 << 20).unwrap();
    let mut nested = child_ctx(&pool, 8 << 20);
    nested
        .set_memory_parent(outer.memory_parent().unwrap())
        .unwrap();
    assert!(matches!(
        nested.charge(5 << 20),
        Err(crate::Error::BudgetExceeded(crate::Budget {
            kind: crate::BudgetKind::Memory,
            ..
        }))
    ));
    assert_eq!(outer.live_bytes(), 5 << 20);
    let owned = nested.charge(1 << 20).unwrap();
    assert!(outer.live_bytes() >= 6 << 20);
    drop(owned);
    drop(nested);
    assert_eq!(outer.live_bytes(), 5 << 20);
    drop(executor);
    assert_eq!(outer.live_bytes(), 0);
}

#[test]
fn nested_exact_child_credit_has_no_second_reservation_step() {
    let pool = super::super::pool::QueryPool::new(64 << 20);
    let outer = child_ctx(&pool, 8 << 20);
    let executor = outer.charge(5 << 20).unwrap();
    let mut nested = child_ctx(&pool, 8 << 20);
    nested
        .set_memory_parent(outer.memory_parent().unwrap())
        .unwrap();
    let child = nested.memory_parent().unwrap();
    let lease = child.reserve(37).unwrap();
    assert_eq!(nested.live_bytes(), 37);
    assert_eq!(outer.live_bytes(), (5 << 20) + 37);
    assert_eq!(
        nested.grant().unwrap().used(),
        0,
        "metadata grant must not be charged twice"
    );
    assert_eq!(outer.grant().unwrap().used(), (5 << 20) + 37);
    drop(lease);
    assert_eq!((nested.live_bytes(), outer.live_bytes()), (0, 5 << 20));
    drop(nested);
    drop(executor);
    drop(outer);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn nested_foreign_domain_refuses_before_changing_the_context() {
    let pool = super::super::pool::QueryPool::new(64 << 20);
    let foreign = super::super::pool::QueryPool::new(64 << 20);
    let mut nested = child_ctx(&pool, 8 << 20);
    let before = super::LEDGER_OPS.with(|n| n.get());
    let error = nested.set_memory_parent(foreign.work_parent()).unwrap_err();
    assert!(matches!(error, crate::Error::Component(c) if c.code == "foreign-domain"));
    let ordinary = nested.charge(1).unwrap();
    assert_eq!(super::LEDGER_OPS.with(|n| n.get()), before);
    // Ordinary eager accounting still reserves its existing 64 KiB credit step.
    assert_eq!(
        (nested.grant().unwrap().used(), foreign.granted()),
        (64 << 10, 0)
    );
    drop(ordinary);
    drop(nested);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn nested_release_waker_can_reenter_both_ledgers() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let pool = super::super::pool::QueryPool::new(64 << 20);
    let outer = child_ctx(&pool, 64 << 20);
    let executor = outer.charge(1 << 20).unwrap();
    let mut nested = child_ctx(&pool, 64 << 20);
    nested
        .set_memory_parent(outer.memory_parent().unwrap())
        .unwrap();
    let callback = nested.memory_parent().unwrap();
    let allocated = nested.charge(48 << 20).unwrap();
    let called = std::sync::Arc::new(AtomicUsize::new(0));
    let seen = called.clone();
    let waiter = pool.enter(
        32 << 20,
        Some(std::sync::Arc::new(move || {
            drop(callback.clone().reserve(1).unwrap());
            seen.fetch_add(1, Ordering::SeqCst);
        })),
    );
    drop(allocated);
    assert!(called.load(Ordering::SeqCst) > 0);
    assert_eq!(nested.live_bytes(), 0);
    assert_eq!(outer.live_bytes(), 1 << 20);
    drop(waiter);
    drop(nested);
    drop(executor);
    drop(outer);
    assert_eq!(pool.granted(), 0);
}
