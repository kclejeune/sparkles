//! Foundation adapters only. Subsystem-local and payload integration comes later.
use sparkles_budget::{LiveCredit, ParentRefused, Scope};
use sparkles_core::sparql::pool::QueryPool;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const MB: u64 = 1 << 20;

#[test]
fn fixed_allowances_are_checked_and_do_not_reserve_idle_bytes() {
    assert!(matches!(
        QueryPool::with_retained_capacity(u64::MAX, 1),
        Err(ParentRefused::Overflow)
    ));
    let pool = QueryPool::with_retained_capacity(32 * MB, 8 * MB).unwrap();
    let stats = pool.domain_stats();
    assert_eq!(
        (
            stats.work_capacity,
            stats.retained_capacity,
            stats.total_capacity
        ),
        (32 * MB, 8 * MB, 40 * MB)
    );
    assert_eq!((stats.work_reserved, stats.retained_reserved), (0, 0));
    assert_ne!(pool.domain(), QueryPool::new(32 * MB).domain());
    assert_eq!(pool.work_parent().domain(), pool.retained_parent().domain());
    assert!(
        QueryPool::new(32 * MB)
            .retained_parent()
            .reserve(1)
            .is_err()
    );
}

#[test]
fn query_admission_and_background_growth_share_the_authoritative_counter() {
    let pool = QueryPool::new(32 * MB);
    let query = pool.try_grant(16 * MB).unwrap();
    let parent = pool.work_parent();
    let background = parent.clone().reserve(16 * MB).unwrap();
    assert_eq!(pool.granted(), 32 * MB);
    assert!(pool.try_grant(1).is_none());
    assert!(query.charge(20 * MB).is_err());
    assert_eq!(
        (query.used(), query.granted(), pool.granted()),
        (0, 16 * MB, 32 * MB)
    );
    assert!(parent.reserve(1).is_err());
    drop(background);
    query.charge(20 * MB).unwrap();
    assert_eq!(pool.granted(), 20 * MB);
    drop(query);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn work_owners_start_at_zero_and_return_all_credit_without_a_query_floor() {
    let pool = QueryPool::new(32 * MB);
    let parent = pool.work_parent();
    assert_eq!(pool.granted(), 0);
    let mut exact = parent.clone().reserve(37).unwrap();
    assert_eq!(pool.granted(), 37);
    exact.shrink(37).unwrap();
    assert_eq!(pool.granted(), 0);
    let mut live = LiveCredit::new(parent);
    live.try_add(1).unwrap();
    assert_eq!(pool.granted(), 64 << 10);
    drop(live);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn retained_parent_handles_share_fixed_c_without_decreasing_q() {
    let pool = QueryPool::with_retained_capacity(16 * MB, 8 * MB).unwrap();
    let query = pool.try_grant(16 * MB).unwrap();
    let first = pool.retained_parent().reserve(8 * MB).unwrap();
    assert!(matches!(
        pool.retained_parent().reserve(1),
        Err(ParentRefused::Limit {
            scope: Scope::Retained,
            ..
        })
    ));
    assert_eq!(
        (pool.granted(), pool.domain_stats().retained_reserved),
        (16 * MB, 8 * MB)
    );
    drop(query);
    assert_eq!(
        (pool.granted(), pool.domain_stats().retained_reserved),
        (0, 8 * MB)
    );
    drop(first);
    assert_eq!(pool.domain_stats().retained_reserved, 0);
}

#[test]
fn promotion_requires_backed_destination_and_keeps_original_on_refusal() {
    let pool = QueryPool::with_retained_capacity(32 * MB, 8 * MB).unwrap();
    let mut payload = pool.work_parent().reserve(4 * MB).unwrap();
    let blocker = pool.retained_parent().reserve(6 * MB).unwrap();
    assert!(payload.try_move_to(pool.retained_parent()).is_err());
    assert_eq!(
        (
            pool.granted(),
            pool.domain_stats().retained_reserved,
            payload.bytes()
        ),
        (4 * MB, 6 * MB, 4 * MB)
    );
    let foreign = QueryPool::with_retained_capacity(32 * MB, 8 * MB).unwrap();
    assert_eq!(
        payload.try_move_to(foreign.retained_parent()),
        Err(ParentRefused::ForeignDomain)
    );
    assert_eq!(foreign.domain_stats().retained_reserved, 0);
    drop(blocker);
    payload.try_move_to(pool.retained_parent()).unwrap();
    assert_eq!(
        (pool.granted(), pool.domain_stats().retained_reserved),
        (0, 4 * MB)
    );
    drop(payload);
    assert_eq!(pool.domain_stats().retained_reserved, 0);
}

#[test]
fn waiting_queries_limit_background_growth_without_revoking_existing_ownership() {
    let pool = QueryPool::new(64 * MB);
    let mut background = pool.work_parent().reserve(40 * MB).unwrap();
    let query = pool.try_grant(24 * MB).unwrap();
    let mut waiter = pool.enter(16 * MB, None);
    assert!(waiter.poll().is_none());
    assert!(background.try_grow(1).is_err());
    assert_eq!((background.bytes(), pool.granted()), (40 * MB, 64 * MB));
    background.shrink(24 * MB).unwrap();
    assert!(waiter.poll().is_some());
    drop((background, query));
    assert_eq!(pool.granted(), 0);
}

#[test]
fn a_background_release_waker_can_reenter_its_parent() {
    let pool = QueryPool::new(64 * MB);
    let parent = pool.work_parent();
    let mut background = parent.clone().reserve(64 * MB).unwrap();
    let called = Arc::new(AtomicUsize::new(0));
    let seen = called.clone();
    let callback = parent.clone();
    let waiter = pool.enter(
        16 * MB,
        Some(Arc::new(move || {
            // This takes the same adapter lock and refuses the share policy. A release
            // must not hold that lock while running the callback.
            assert!(callback.clone().reserve(1).is_err());
            seen.fetch_add(1, Ordering::SeqCst);
        })),
    );
    background.shrink(32 * MB).unwrap();
    assert_eq!(called.load(Ordering::SeqCst), 1);
    drop(waiter);
    drop(background);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn a_grant_release_waker_can_reenter_the_resize_lock() {
    let pool = QueryPool::new(64 * MB);
    let grant = Arc::new(pool.try_grant(64 * MB).unwrap());
    grant.charge(64 * MB).unwrap();
    let called = Arc::new(AtomicUsize::new(0));
    let seen = called.clone();
    let callback = grant.clone();
    let waiter = pool.enter(
        16 * MB,
        Some(Arc::new(move || {
            callback.cover_transient(16 * MB).unwrap();
            seen.fetch_add(1, Ordering::SeqCst);
        })),
    );
    grant.release(48 * MB);
    assert_eq!(called.load(Ordering::SeqCst), 1);
    assert_eq!(
        (grant.used(), grant.granted(), pool.granted()),
        (16 * MB, 16 * MB, 16 * MB)
    );
    drop(waiter);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn a_grant_parent_retains_only_its_actual_grant_owner_and_returns_direct_usage() {
    let pool = QueryPool::new(32 * MB);
    let grant = Arc::new(pool.empty_grant());
    let weak = Arc::downgrade(&grant);
    let parent = grant.memory_parent();
    assert_eq!(parent.domain(), pool.domain());
    let lease = parent.reserve(10 * MB).unwrap();
    assert_eq!(grant.used(), 10 * MB);
    drop(grant);
    assert!(weak.upgrade().is_some());
    assert_eq!(pool.granted(), 12 * MB);
    drop(lease);
    // The parent handle and every lease retain the grant owner.
    assert!(weak.upgrade().is_some());
    drop(parent);
    assert!(weak.upgrade().is_none());
    assert_eq!(pool.granted(), 0);
}

#[test]
fn concurrent_work_owners_roll_back_only_their_own_refused_growth() {
    let pool = QueryPool::new(100);
    let barrier = std::sync::Barrier::new(3);
    std::thread::scope(|scope| {
        for _ in 0..2 {
            let parent = pool.work_parent();
            let barrier = &barrier;
            scope.spawn(move || {
                let mut lease = parent.reserve(40).unwrap();
                barrier.wait();
                assert!(lease.try_grow(30).is_err());
                assert_eq!(lease.bytes(), 40);
                barrier.wait();
                barrier.wait();
            });
        }
        barrier.wait();
        barrier.wait();
        assert_eq!(pool.granted(), 80);
        barrier.wait();
    });
    assert_eq!(pool.granted(), 0);
    drop(pool.work_parent().reserve(100).unwrap());
    assert_eq!(pool.granted(), 0);
}
