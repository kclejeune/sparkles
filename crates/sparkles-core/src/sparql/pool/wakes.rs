//! Arbitrary admission callbacks can synchronously enter any store.
use super::QueryPool;
use parking_lot::Mutex;
use std::{cell::RefCell, mem::ManuallyDrop, sync::Arc};

type Pools = Vec<Arc<QueryPool>>;
type Shared = Arc<Mutex<Pools>>;

enum Pending {
    Local(Pools),
    Shared(Shared),
}

type Current = ManuallyDrop<RefCell<Option<Pending>>>;
// Leave the empty slot usable by TLS destructors.
const _: () = assert!(!std::mem::needs_drop::<Current>());
thread_local! {
    static CURRENT: Current = const { ManuallyDrop::new(RefCell::new(None)) };
}

pub(super) fn defer(pool: &Arc<QueryPool>) -> bool {
    CURRENT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(pending) = slot.as_mut() else {
            return false;
        };
        let push = |pending: &mut Pools| {
            if !pending.iter().any(|p| Arc::ptr_eq(p, pool)) {
                pending.push(pool.clone());
            }
        };
        match pending {
            Pending::Local(pending) => push(pending),
            Pending::Shared(pending) => push(&mut pending.lock()),
        }
        true
    })
}

impl QueryPool {
    /// Make released credit available immediately, and run callbacks after the
    /// outer lock and its guards have left scope, including on unwind.
    #[doc(hidden)]
    pub fn with_deferred_release_wakes<T>(work: impl FnOnce() -> T) -> T {
        struct Drain(bool);
        impl Drop for Drain {
            fn drop(&mut self) {
                if !self.0 {
                    return;
                }
                let pending = CURRENT.with(|slot| slot.borrow_mut().take());
                let pools = match pending.expect("active wake scope") {
                    Pending::Local(pools) => pools,
                    Pending::Shared(pools) => std::mem::take(&mut *pools.lock()),
                };
                for pool in pools {
                    pool.wake();
                }
            }
        }
        let outermost = CURRENT.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_some() {
                return false;
            }
            *slot = Some(Pending::Local(Vec::new()));
            true
        });
        let _drain = Drain(outermost);
        work()
    }

    /// Carry the caller's deferral across a synchronous thread handoff. The
    /// caller must wait for the returned work before leaving its own scope.
    #[doc(hidden)]
    pub fn inherit_release_wakes<T>(work: impl FnOnce() -> T + Send) -> impl FnOnce() -> T + Send {
        let inherited = CURRENT.with(|slot| {
            let mut slot = slot.borrow_mut();
            let pending = slot.as_mut()?;
            match pending {
                Pending::Local(pools) => {
                    let shared = Arc::new(Mutex::new(std::mem::take(pools)));
                    *pending = Pending::Shared(shared.clone());
                    Some(shared)
                }
                Pending::Shared(pools) => Some(pools.clone()),
            }
        });
        move || {
            struct Restore(Option<Pending>);
            impl Drop for Restore {
                fn drop(&mut self) {
                    CURRENT.with(|slot| *slot.borrow_mut() = self.0.take());
                }
            }
            let Some(inherited) = inherited else {
                return work();
            };
            let previous =
                CURRENT.with(|slot| slot.borrow_mut().replace(Pending::Shared(inherited)));
            let _restore = Restore(previous);
            work()
        }
    }
}
