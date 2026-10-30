//! Owned resident source openings. These leases provide lifetime and identity,
//! never authentication, read permission, managed admission or durable retention.

use crate::store::Store;
use crate::{Error, Result};
use std::sync::Arc;
use uuid::Uuid;

pub(crate) fn lock<'a, T>(
    mutex: &'a parking_lot::Mutex<T>,
    options: &crate::guard::WriteOptions,
) -> Result<parking_lot::MutexGuard<'a, T>> {
    loop {
        options.check()?;
        let guard = if options.no_wait {
            mutex.try_lock()
        } else {
            mutex.try_lock_for(std::time::Duration::from_millis(10))
        };
        if let Some(guard) = guard {
            options.check()?;
            return Ok(guard);
        }
        if options.no_wait {
            return Err(Error::WriterBusy);
        }
    }
}

/// Exact dataset and logical branch identities, independent of catalog aliases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub dataset: Uuid,
    pub branch: Uuid,
}

/// Keeps an already resolved main and logical store alive for one operation.
///
/// Accessors must only borrow retained stores. Resolve routing, locks and possible
/// I/O before returning the owner. Sample both accessors before taking any family
/// permit, writer or authority fence, then retain this owner through publication.
/// Implementing this trait grants no authority over the returned stores.
pub trait SourceOwner: Send + Sync {
    fn actual_main(&self) -> &Store;
    fn logical(&self) -> &Store;
}

/// A source lease for low-level embeddings that already own real store Arcs.
///
/// This does not add facade catalog lifecycle exclusion. Catalog embeddings use
/// the facade provider, which also retains the catalog's Dataset owner.
pub struct ArcSourceOwner {
    main: Arc<Store>,
    logical: Arc<Store>,
}

impl ArcSourceOwner {
    /// Retain exactly these existing openings; no store is opened or copied.
    pub fn new(main: Arc<Store>, logical: Arc<Store>) -> Self {
        Self { main, logical }
    }
}

impl SourceOwner for ArcSourceOwner {
    fn actual_main(&self) -> &Store {
        &self.main
    }

    fn logical(&self) -> &Store {
        &self.logical
    }
}

/// Borrowed identity sampled once from an independently retained source owner.
///
/// Keep the owner collection separate from these contexts. This type borrows the
/// stores, so it cannot outlive that collection. It is not an authorization proof.
pub struct SampledSource<'a> {
    main: &'a Store,
    logical: &'a Store,
    target: Target,
    main_opening: Uuid,
    logical_opening: Uuid,
}

impl<'a> SampledSource<'a> {
    /// Call each owner accessor once, before any operation exclusion is acquired.
    /// Reject foreign families even when their stored UUIDs happen to agree.
    pub fn sample(owner: &'a dyn SourceOwner, target: Target) -> Result<Self> {
        let main = owner.actual_main();
        let logical = owner.logical();
        if main.is_branch()
            || main.dataset_id() != target.dataset
            || logical.owner_dataset_id() != target.dataset
            || logical.branch_id() != target.branch
            || !main.same_source_family(logical)
            || (target.branch == target.dataset && !std::ptr::eq(main, logical))
        {
            return Err(Error::invalid(
                "the source opening does not match its target",
            ));
        }
        Ok(Self {
            main,
            logical,
            target,
            main_opening: main.source_opening(),
            logical_opening: logical.source_opening(),
        })
    }

    /// The exact main sampled before the operation acquired exclusion.
    pub fn actual_main(&self) -> &'a Store {
        self.main
    }

    /// The exact logical store sampled before the operation acquired exclusion.
    pub fn logical(&self) -> &'a Store {
        self.logical
    }

    pub fn target(&self) -> Target {
        self.target
    }

    /// Compare already sampled values without invoking the source owner again.
    pub fn observes(&self, main: &Store, logical: &Store) -> bool {
        std::ptr::eq(self.main, main)
            && std::ptr::eq(self.logical, logical)
            && self.main_opening == main.source_opening()
            && self.logical_opening == logical.source_opening()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn sampling_calls_each_accessor_once_and_retains_exact_borrows() {
        struct CountingOwner {
            store: Store,
            calls: AtomicUsize,
        }
        impl SourceOwner for CountingOwner {
            fn actual_main(&self) -> &Store {
                self.calls.fetch_add(1, Ordering::Relaxed);
                &self.store
            }
            fn logical(&self) -> &Store {
                self.calls.fetch_add(1, Ordering::Relaxed);
                &self.store
            }
        }
        let owner = CountingOwner {
            store: Store::in_memory(Default::default()),
            calls: AtomicUsize::new(0),
        };
        let target = Target {
            dataset: owner.store.dataset_id(),
            branch: owner.store.branch_id(),
        };
        let sampled = SampledSource::sample(&owner, target).unwrap();
        assert!(sampled.observes(&owner.store, &owner.store));
        assert!(std::ptr::eq(sampled.actual_main(), &owner.store));
        assert!(std::ptr::eq(sampled.logical(), &owner.store));
        assert_eq!(sampled.target(), target);
        assert_eq!(owner.calls.load(Ordering::Relaxed), 2);
    }
}
