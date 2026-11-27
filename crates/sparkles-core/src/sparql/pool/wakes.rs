//! Release callbacks may synchronously enter the store or wait for other queries.
use super::QueryPool;
use sparkles_budget::{DomainId, ParentHandle};
use std::{cell::RefCell, mem::ManuallyDrop, sync::Arc};

enum Scope {
    All,
    Materialization(Option<DomainId>),
    Completion,
}

struct Frame {
    scope: Scope,
    pending: Vec<Arc<QueryPool>>,
    outer: Option<Box<Frame>>,
}

type Current = ManuallyDrop<RefCell<Option<Frame>>>;
// Each guard restores its predecessor. Leave the empty slot usable by TLS destructors.
const _: () = assert!(!std::mem::needs_drop::<Current>());
thread_local! {
    static CURRENT: Current = const { ManuallyDrop::new(RefCell::new(None)) };
}

impl Frame {
    fn defer(&mut self, pool: &Arc<QueryPool>, current: bool) -> bool {
        if let Some(outer) = &mut self.outer
            && outer.defer(pool, false)
        {
            return true;
        }
        if self.pending.iter().any(|p| Arc::ptr_eq(p, pool)) {
            return true;
        }
        let guard = match &self.scope {
            Scope::All => true,
            Scope::Materialization(domain) => {
                current || domain.as_ref().is_some_and(|id| *id == pool.domain())
            }
            Scope::Completion => false,
        };
        if guard {
            self.pending.push(pool.clone());
        }
        guard
    }
}

pub(super) fn defer(pool: &Arc<QueryPool>) -> bool {
    CURRENT.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .is_some_and(|frame| frame.defer(pool, true))
    })
}

fn scoped<T>(scope: Scope, work: impl FnOnce() -> T) -> T {
    struct Drain;
    impl Drop for Drain {
        fn drop(&mut self) {
            let frame = CURRENT.with(|slot| {
                let mut slot = slot.borrow_mut();
                let mut frame = slot.take().expect("active wake scope");
                *slot = frame.outer.take().map(|outer| *outer);
                frame
            });
            for pool in frame.pending {
                // An ordinary inner lock hands its releases to the enclosing writer.
                if !defer(&pool) {
                    pool.wake();
                }
            }
        }
    }
    CURRENT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let outer = slot.take().map(Box::new);
        *slot = Some(Frame {
            scope,
            pending: Vec::new(),
            outer,
        });
    });
    let _drain = Drain;
    work()
}

impl QueryPool {
    /// Make released credit available immediately, and wake waiters after the
    /// outer lock and its guards have left scope, including on unwind.
    #[doc(hidden)]
    pub fn with_deferred_release_wakes<T>(work: impl FnOnce() -> T) -> T {
        scoped(Scope::All, work)
    }

    /// Drain the run's wakes after its writer and permit are released. A nested
    /// completion can synchronously wait on independent pools while the outer
    /// materialization continues to guard its own memory domain and pending wakes.
    #[doc(hidden)]
    pub fn with_materialization_wakes<T, U>(
        parent: Option<&ParentHandle>,
        run: impl FnOnce() -> T,
        complete: impl FnOnce(T) -> U,
    ) -> U {
        let work = || {
            let result = scoped(
                Scope::Materialization(parent.map(ParentHandle::domain)),
                run,
            );
            complete(result)
        };
        if CURRENT.with(|slot| slot.borrow().is_some()) {
            // The inner drain and its done callback both run outside its writer.
            // Keep only the enclosing guards that own a wake or its memory domain.
            scoped(Scope::Completion, work)
        } else {
            work()
        }
    }
}
