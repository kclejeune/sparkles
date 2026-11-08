use sparkles_budget::{
    LIVE_SLACK, LIVE_STEP, LiveCredit, MemoryDomain, MemoryParent, ParentRefused, Scope,
};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

#[derive(Default)]
struct Counts {
    reserved: AtomicU64,
    reserves: AtomicUsize,
    releases: AtomicUsize,
}

fn parent(domain: &MemoryDomain, limit: u64) -> (Arc<dyn MemoryParent>, Arc<Counts>) {
    let counts = Arc::new(Counts::default());
    let take = counts.clone();
    let give = counts.clone();
    let p = domain.parent(
        move |bytes| {
            take.reserves.fetch_add(1, Ordering::SeqCst);
            take.reserved
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                    used.checked_add(bytes).filter(|next| *next <= limit)
                })
                .map(|_| ())
                .map_err(|_| ParentRefused::Limit {
                    limit,
                    requested: bytes,
                    scope: Scope::Work,
                })
        },
        move |bytes| {
            let prior = give.reserved.fetch_sub(bytes, Ordering::SeqCst);
            assert!(prior >= bytes);
            give.releases.fetch_add(1, Ordering::SeqCst);
        },
    );
    (p, counts)
}

#[test]
fn split_merge_and_drop_return_only_owned_credit() {
    let domain = MemoryDomain::new();
    let (p, c) = parent(&domain, 100);
    let mut a = p.clone().reserve(80).unwrap();
    let mut b = a.split(30).unwrap();
    assert_eq!(
        (a.bytes(), b.bytes(), c.reserved.load(Ordering::SeqCst)),
        (50, 30, 80)
    );
    assert!(a.split(51).is_err());
    assert!(a.shrink(51).is_err());
    assert_eq!(a.bytes(), 50);
    a.try_merge(&mut b).unwrap();
    assert_eq!((a.bytes(), b.bytes()), (80, 0));
    drop(b);
    assert_eq!(c.releases.load(Ordering::SeqCst), 0);
    a.shrink(20).unwrap();
    a.try_grow(40).unwrap();
    assert_eq!((a.bytes(), c.reserved.load(Ordering::SeqCst)), (100, 100));
    assert!(a.try_grow(1).is_err());
    assert_eq!(a.bytes(), 100);
    drop(a);
    assert_eq!(c.reserved.load(Ordering::SeqCst), 0);
}

#[test]
fn foreign_domains_and_different_branches_cannot_merge() {
    let first = MemoryDomain::new();
    let second = MemoryDomain::new();
    let (a, ca) = parent(&first, 100);
    let (b, cb) = parent(&first, 100);
    let (c, cc) = parent(&second, 100);
    let mut x = a.reserve(10).unwrap();
    let mut y = b.reserve(20).unwrap();
    let mut z = c.clone().reserve(30).unwrap();
    assert_eq!(x.try_merge(&mut y), Err(ParentRefused::DifferentParent));
    assert_eq!(x.try_merge(&mut z), Err(ParentRefused::ForeignDomain));
    assert_eq!(x.try_move_to(c), Err(ParentRefused::ForeignDomain));
    assert_eq!((x.bytes(), y.bytes(), z.bytes()), (10, 20, 30));
    assert_eq!(
        (
            ca.reserved.load(Ordering::SeqCst),
            cb.reserved.load(Ordering::SeqCst),
            cc.reserved.load(Ordering::SeqCst)
        ),
        (10, 20, 30)
    );
}

#[test]
fn move_prepares_destination_before_retiring_source_and_rolls_back_refusal() {
    let domain = MemoryDomain::new();
    let (source, old) = parent(&domain, 100);
    let (small, refused) = parent(&domain, 10);
    let mut lease = source.clone().reserve(40).unwrap();
    assert!(lease.try_move_to(small).is_err());
    assert_eq!(
        (
            lease.bytes(),
            old.reserved.load(Ordering::SeqCst),
            refused.reserved.load(Ordering::SeqCst)
        ),
        (40, 40, 0)
    );
    lease.try_move_to(source).unwrap();
    assert_eq!(old.reserves.load(Ordering::SeqCst), 1);
    let destination_bytes = Arc::new(AtomicU64::new(0));
    let take = destination_bytes.clone();
    let give = destination_bytes.clone();
    let still_backed = old.clone();
    let destination = domain.parent(
        move |bytes| {
            assert_eq!(still_backed.reserved.load(Ordering::SeqCst), 40);
            take.fetch_add(bytes, Ordering::SeqCst);
            Ok(())
        },
        move |bytes| {
            give.fetch_sub(bytes, Ordering::SeqCst);
        },
    );
    lease.try_move_to(destination).unwrap();
    assert_eq!(
        (
            lease.bytes(),
            old.reserved.load(Ordering::SeqCst),
            destination_bytes.load(Ordering::SeqCst)
        ),
        (40, 0, 40)
    );
    drop(lease);
    assert_eq!(destination_bytes.load(Ordering::SeqCst), 0);
}

#[test]
fn rounding_overflow_falls_back_to_exact_and_add_overflow_is_atomic() {
    let domain = MemoryDomain::new();
    let (p, c) = parent(&domain, u64::MAX);
    let mut live = LiveCredit::new(p);
    live.try_add(u64::MAX - 1).unwrap();
    assert_eq!((live.live(), live.reserved()), (u64::MAX - 1, u64::MAX - 1));
    let before = c.reserves.load(Ordering::SeqCst);
    assert_eq!(live.try_add(2), Err(ParentRefused::Overflow));
    assert_eq!(c.reserves.load(Ordering::SeqCst), before);
    assert_eq!(live.live(), u64::MAX - 1);
    let mut lease = live.into_lease();
    assert_eq!(lease.try_grow(2), Err(ParentRefused::Overflow));
    assert_eq!(lease.bytes(), u64::MAX - 1);
    drop(lease);
    assert_eq!(c.reserved.load(Ordering::SeqCst), 0);
}

#[test]
fn exact_fallback_uses_last_available_byte_and_failed_growth_changes_nothing() {
    let domain = MemoryDomain::new();
    let (p, c) = parent(&domain, 100);
    let mut live = LiveCredit::new(p);
    live.try_add(99).unwrap();
    assert_eq!(
        (
            live.live(),
            live.reserved(),
            c.reserves.load(Ordering::SeqCst)
        ),
        (99, 99, 2)
    );
    live.try_add(1).unwrap();
    assert_eq!((live.live(), live.reserved()), (100, 100));
    assert!(live.try_add(1).is_err());
    assert_eq!(
        (
            live.live(),
            live.reserved(),
            c.reserved.load(Ordering::SeqCst)
        ),
        (100, 100, 100)
    );
    assert!(live.remove(101).is_err());
    assert_eq!(live.live(), 100);
    drop(live);
    assert_eq!(c.reserved.load(Ordering::SeqCst), 0);
}

#[test]
fn local_updates_amortize_parent_calls_and_preserve_release_slack() {
    let domain = MemoryDomain::new();
    let (p, c) = parent(&domain, 4 * LIVE_SLACK);
    let mut live = LiveCredit::new(p);
    for _ in 0..2 * LIVE_STEP {
        live.try_add(1).unwrap();
    }
    assert_eq!(c.reserves.load(Ordering::SeqCst), 2);
    live.try_add(2 * LIVE_SLACK).unwrap();
    live.remove(LIVE_SLACK).unwrap();
    assert_eq!(c.releases.load(Ordering::SeqCst), 0);
    live.remove(1).unwrap();
    assert_eq!(c.releases.load(Ordering::SeqCst), 1);
    assert_eq!(live.reserved(), 2 * LIVE_STEP + LIVE_SLACK);
    live.remove(live.live()).unwrap();
    assert_eq!((live.live(), live.reserved()), (0, 0));
    assert_eq!(c.reserved.load(Ordering::SeqCst), 0);
}

#[test]
fn local_split_and_merge_transfer_live_and_promised_credit_without_new_reservations() {
    let domain = MemoryDomain::new();
    let (p, c) = parent(&domain, LIVE_STEP);
    let mut a = LiveCredit::new(p);
    a.try_add(100).unwrap();
    let mut b = a.split_live(40).unwrap();
    assert_eq!(
        (a.live(), b.live(), a.reserved() + b.reserved()),
        (60, 40, LIVE_STEP)
    );
    assert!(a.split_live(61).is_err());
    a.try_merge(&mut b).unwrap();
    assert_eq!(
        (a.live(), b.live(), a.reserved(), b.reserved()),
        (100, 0, LIVE_STEP, 0)
    );
    assert_eq!(c.reserves.load(Ordering::SeqCst), 1);
}

#[test]
fn concurrent_rejected_growth_never_refunds_another_owner() {
    let domain = MemoryDomain::new();
    let (p, c) = parent(&domain, 10);
    let barrier = Barrier::new(3);
    std::thread::scope(|scope| {
        for _ in 0..2 {
            let p = p.clone();
            let barrier = &barrier;
            scope.spawn(move || {
                let mut owned = p.reserve(4).unwrap();
                barrier.wait();
                assert!(owned.try_grow(3).is_err());
                assert_eq!(owned.bytes(), 4);
                barrier.wait();
                barrier.wait();
            });
        }
        barrier.wait();
        barrier.wait();
        assert_eq!(c.reserved.load(Ordering::SeqCst), 8);
        barrier.wait();
    });
    assert_eq!(c.reserved.load(Ordering::SeqCst), 0);
    drop(p.reserve(10).unwrap());
    assert_eq!(c.reserved.load(Ordering::SeqCst), 0);
}

#[test]
fn last_shared_owner_destroys_payload_before_returning_credit() {
    struct Probe {
        alive: Arc<AtomicBool>,
        reserved: Arc<AtomicU64>,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            assert_eq!(self.reserved.load(Ordering::SeqCst), 10);
            assert!(self.alive.swap(false, Ordering::SeqCst));
        }
    }
    let domain = MemoryDomain::new();
    let alive = Arc::new(AtomicBool::new(true));
    let bytes = Arc::new(AtomicU64::new(0));
    let take = bytes.clone();
    let give = bytes.clone();
    let on_release = alive.clone();
    let p = domain.parent(
        move |n| {
            take.fetch_add(n, Ordering::SeqCst);
            Ok(())
        },
        move |n| {
            assert!(!on_release.load(Ordering::SeqCst));
            assert_eq!(give.fetch_sub(n, Ordering::SeqCst), n);
        },
    );
    let value = Arc::new(p.reserve(10).unwrap().attach(Probe {
        alive: alive.clone(),
        reserved: bytes.clone(),
    }));
    let retained = value.clone();
    drop(value);
    assert!(alive.load(Ordering::SeqCst));
    assert_eq!(retained.reserved(), 10);
    drop(retained);
    assert!(!alive.load(Ordering::SeqCst));
    assert_eq!(bytes.load(Ordering::SeqCst), 0);
}

#[test]
fn unwinding_frees_payload_before_return_and_zero_credit_never_calls_backend() {
    let domain = MemoryDomain::new();
    let (p, c) = parent(&domain, 100);
    let empty = p.clone().reserve(0).unwrap();
    drop(empty);
    assert_eq!(
        (
            c.reserves.load(Ordering::SeqCst),
            c.releases.load(Ordering::SeqCst)
        ),
        (0, 0)
    );
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _owner = p.reserve(30).unwrap().attach(vec![0u8; 30]);
        panic!("inert unwind after ownership established");
    }));
    assert!(outcome.is_err());
    assert_eq!(c.reserved.load(Ordering::SeqCst), 0);
}

#[test]
fn release_can_reenter_the_parent_without_a_primitive_lock_or_cycle() {
    let domain = MemoryDomain::new();
    let reserved = Arc::new(AtomicU64::new(0));
    let slot = Arc::new(Mutex::new(None::<std::sync::Weak<dyn MemoryParent>>));
    let take = reserved.clone();
    let give = reserved.clone();
    let callback = slot.clone();
    let once = Arc::new(AtomicBool::new(false));
    let seen = once.clone();
    let p = domain.parent(
        move |n| {
            take.fetch_add(n, Ordering::SeqCst);
            Ok(())
        },
        move |n| {
            give.fetch_sub(n, Ordering::SeqCst);
            if !seen.swap(true, Ordering::SeqCst) {
                let p = callback
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .upgrade()
                    .unwrap();
                drop(p.reserve(1).unwrap());
            }
        },
    );
    *slot.lock().unwrap() = Some(Arc::downgrade(&p));
    drop(p.clone().reserve(10).unwrap());
    assert!(once.load(Ordering::SeqCst));
    assert_eq!(reserved.load(Ordering::SeqCst), 0);
    drop(p);
    assert!(slot.lock().unwrap().as_ref().unwrap().upgrade().is_none());
}

#[test]
fn explicit_unpooled_domain_is_stable_and_distinct_from_bounded_roots() {
    let first = MemoryDomain::unpooled();
    let second = MemoryDomain::unpooled();
    assert_eq!(first.id(), second.id());
    assert_ne!(first.id(), MemoryDomain::new().id());
}
