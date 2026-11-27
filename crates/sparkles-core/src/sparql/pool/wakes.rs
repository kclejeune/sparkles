//! Arbitrary admission callbacks can synchronously enter any store.
use super::QueryPool;
use parking_lot::Mutex;
use std::{cell::RefCell, mem::ManuallyDrop, sync::Arc};

type Pools = Vec<Arc<QueryPool>>;
/// A deferral that other threads can reach. The drain closes it by taking the
/// list, so a release that arrives later is never left in a queue nobody drains.
type Shared = Arc<Mutex<Option<Pools>>>;

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

/// Queue `pool`'s wake-up on this thread's open deferral. False when there is none,
/// and the caller then notifies inline.
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
            Pending::Shared(pending) => match pending.lock().as_mut() {
                Some(pending) => push(pending),
                None => return false,
            },
        }
        true
    })
}

/// The release-wake deferral of an open scope, captured so that work for that
/// scope on other threads defers into it. Its owner drains it when the scope ends.
/// After that it is closed, and releases through it notify as they would without it.
#[doc(hidden)]
#[derive(Clone)]
pub struct ReleaseWakes(Shared);

impl std::fmt::Debug for ReleaseWakes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReleaseWakes")
            .field("open", &self.0.lock().is_some())
            .finish()
    }
}

impl ReleaseWakes {
    /// Run `work` with this deferral in place of the thread's own while it is open.
    /// A task stolen onto a thread that works for another scope then still defers
    /// into its own scope.
    pub fn run<T>(&self, work: impl FnOnce() -> T) -> T {
        struct Restore(Option<Pending>);
        impl Drop for Restore {
            fn drop(&mut self) {
                CURRENT.with(|slot| *slot.borrow_mut() = self.0.take());
            }
        }
        if self.0.lock().is_none() {
            return work();
        }
        let previous =
            CURRENT.with(|slot| slot.borrow_mut().replace(Pending::Shared(self.0.clone())));
        let _restore = Restore(previous);
        work()
    }
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
                    // Closing and taking are one step under the lock.
                    Pending::Shared(pools) => pools.lock().take().unwrap_or_default(),
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

    /// The deferral open on this thread, shared so that other threads can reach it.
    /// An owner of pool memory that other threads may release, such as a reasoning
    /// budget, keeps it and releases through [`ReleaseWakes::run`].
    #[doc(hidden)]
    pub fn capture_release_wakes() -> Option<ReleaseWakes> {
        CURRENT.with(|slot| {
            let mut slot = slot.borrow_mut();
            let pending = slot.as_mut()?;
            match pending {
                Pending::Local(pools) => {
                    let shared = Arc::new(Mutex::new(Some(std::mem::take(pools))));
                    *pending = Pending::Shared(shared.clone());
                    Some(ReleaseWakes(shared))
                }
                Pending::Shared(pools) => Some(ReleaseWakes(pools.clone())),
            }
        })
    }

    /// Carry the caller's deferral across a synchronous thread handoff. The
    /// caller must wait for the returned work before leaving its own scope.
    #[doc(hidden)]
    pub fn inherit_release_wakes<T>(work: impl FnOnce() -> T + Send) -> impl FnOnce() -> T + Send {
        let inherited = Self::capture_release_wakes();
        move || match inherited {
            Some(inherited) => inherited.run(work),
            None => work(),
        }
    }
}
