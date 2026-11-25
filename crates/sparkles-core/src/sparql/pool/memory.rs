//! Explicit reservation adapters. Ordinary query accounting does not call these.

use super::*;
use sparkles_budget::{DomainId, ParentHandle, ParentRefused, Scope};

/// Reserved credit in the fixed work and retained branches. These counters are
/// neither local live estimates, the cache eviction-policy estimate nor RSS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DomainStats {
    pub work_capacity: u64,
    pub retained_capacity: u64,
    pub total_capacity: u64,
    pub work_reserved: u64,
    pub retained_reserved: u64,
}

impl QueryPool {
    /// Bind fixed work Q and retained-cache C allowances before admission. The
    /// existing host-sizing policy has already excluded C from Q. Neither this
    /// constructor nor cache policy setters reserve an idle allowance's bytes.
    pub fn with_retained_capacity(
        work: u64,
        retained: u64,
    ) -> Result<Arc<QueryPool>, ParentRefused> {
        work.checked_add(retained).ok_or(ParentRefused::Overflow)?;
        Ok(Arc::new(QueryPool {
            capacity: work,
            domain: MemoryDomain::new(),
            retained_capacity: retained,
            retained_reserved: AtomicU64::new(0),
            granted: AtomicU64::new(0),
            waiting: AtomicUsize::new(0),
            queue: Mutex::new(Queue::default()),
            changed: Condvar::new(),
            admitted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        }))
    }

    pub fn domain(&self) -> DomainId {
        self.domain.id()
    }

    pub(crate) fn memory_domain(&self) -> &MemoryDomain {
        &self.domain
    }

    pub fn domain_stats(&self) -> DomainStats {
        DomainStats {
            work_capacity: self.capacity,
            retained_capacity: self.retained_capacity,
            total_capacity: self.capacity + self.retained_capacity,
            work_reserved: self.granted(),
            retained_reserved: self.retained_reserved.load(Ordering::SeqCst),
        }
    }

    /// An independent zero-initial work owner, with no admission queue or query
    /// floor/granule. LiveCredit amortizes its parent operations separately. Its
    /// reservations use the authoritative query-pool counter, so query admission
    /// observes background ownership immediately.
    /// Release can invoke the pool's synchronous wake callbacks, including in Drop.
    /// They must not panic; see [`QueryPool::enter`]. Panics are not contained.
    pub fn work_parent(self: &Arc<Self>) -> ParentHandle {
        let held = Arc::new(Mutex::new(0u64));
        let take = held.clone();
        let reserve_pool = self.clone();
        let release_pool = self.clone();
        self.domain.parent(
            move |bytes| {
                let mut owned = take.lock();
                let next = owned.checked_add(bytes).ok_or(ParentRefused::Overflow)?;
                let share = if reserve_pool.waiting() > 0 {
                    reserve_pool.capacity / 2
                } else {
                    reserve_pool.capacity
                };
                // Keep the existing grant growth's bounded-share threshold. This
                // permits no initial floor allocation for a small retained owner.
                let limit = share.max(INITIAL_GRANT.min(reserve_pool.capacity));
                if next > limit || !reserve_pool.reserve(bytes, reserve_pool.capacity) {
                    return Err(ParentRefused::Limit {
                        limit: reserve_pool.capacity,
                        requested: next,
                        scope: Scope::Work,
                    });
                }
                *owned = next;
                Ok(())
            },
            move |bytes| {
                let mut owned = held.lock();
                *owned = owned.checked_sub(bytes).expect("owned work reservation");
                drop(owned);
                release_pool.release(bytes);
            },
        )
    }

    /// Combine an unpooled query's memory parent with this pool's work parent in this
    /// pool's accounting domain. Reservations check and update the outer query's live
    /// budget first, then reserve from this pool (rolling back the outer lease if the
    /// pool refuses). Releases drop both leases outside the internal lock.
    pub(crate) fn combine_unpooled_parent(self: &Arc<Self>, outer: ParentHandle) -> ParentHandle {
        debug_assert_eq!(
            outer.domain(),
            MemoryDomain::unpooled().id(),
            "combine_unpooled_parent expects an unpooled outer parent"
        );
        let backing = self.work_parent();
        let held = Arc::new(Mutex::new((
            outer.reserve(0).expect("sealed zero outer reservation"),
            backing.reserve(0).expect("sealed zero pool reservation"),
        )));
        let take_held = held.clone();
        self.domain.parent(
            move |bytes| {
                if bytes == 0 {
                    return Ok(());
                }
                let mut outer_lease = outer.reserve(bytes)?;
                let mut pool_lease = match backing.reserve(bytes) {
                    Ok(lease) => lease,
                    Err(err) => {
                        drop(outer_lease);
                        return Err(err);
                    }
                };
                let mut guard = take_held.lock();
                guard
                    .0
                    .try_merge(&mut outer_lease)
                    .expect("same outer parent");
                guard
                    .1
                    .try_merge(&mut pool_lease)
                    .expect("same pool work parent");
                Ok(())
            },
            move |bytes| {
                if bytes == 0 {
                    return;
                }
                let (outer_release, pool_release) = {
                    let mut guard = held.lock();
                    (
                        guard.0.split(bytes).expect("owned outer reservation"),
                        guard.1.split(bytes).expect("owned pool work reservation"),
                    )
                };
                drop(pool_release);
                drop(outer_release);
            },
        )
    }

    /// A retained-cache parent in this domain's fixed C branch. Separate returned
    /// handles use the same branch counter. Policy changes never enlarge C or
    /// revoke reservations held by callers after eviction.
    pub fn retained_parent(self: &Arc<Self>) -> ParentHandle {
        let take = self.clone();
        let give = self.clone();
        self.domain.parent(
            move |bytes| {
                take.retained_reserved
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                        used.checked_add(bytes)
                            .filter(|next| *next <= take.retained_capacity)
                    })
                    .map(|_| ())
                    .map_err(|_| ParentRefused::Limit {
                        limit: take.retained_capacity,
                        requested: take
                            .retained_reserved
                            .load(Ordering::SeqCst)
                            .saturating_add(bytes),
                        scope: Scope::Retained,
                    })
            },
            move |bytes| {
                give.retained_reserved
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                        used.checked_sub(bytes)
                    })
                    .expect("owned retained reservation");
            },
        )
    }
}

impl PoolGrant {
    /// The factory stays inside SPARQL accounting; consumers receive identity only.
    pub(crate) fn memory_domain(&self) -> &MemoryDomain {
        &self.pool.domain
    }

    /// Adapt existing direct grant ownership. The later query-ledger child must
    /// additionally enforce its shared local query limit; this adapter alone is
    /// only pool backing. An owner retaining this parent also retains the grant.
    /// Release can invoke the pool's synchronous wake callbacks, including in Drop.
    /// They must not panic; see [`QueryPool::enter`]. Panics are not contained.
    pub fn memory_parent(self: &Arc<Self>) -> ParentHandle {
        let take = self.clone();
        let give = self.clone();
        self.pool.domain.parent(
            move |bytes| {
                take.charge(bytes).map_err(|error| {
                    let Error::BudgetExceeded(budget) = error else {
                        unreachable!("grant charge only returns memory refusals")
                    };
                    ParentRefused::Limit {
                        limit: budget.limit,
                        requested: budget.requested,
                        scope: Scope::Work,
                    }
                })
            },
            move |bytes| give.release(bytes),
        )
    }
}
