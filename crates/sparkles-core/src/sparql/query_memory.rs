//! Private query ownership. Payloads never enter this ledger or its locks.
use super::pool::PoolGrant;
use crate::error::{Budget, BudgetKind, Error, Result};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct QueryMemoryLedger {
    pub(crate) live: AtomicU64,
    pub(crate) peak: AtomicU64,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    owners: Vec<Account>,
    next: u64,
    cursor: bool,
    calls: usize,
}
/// The owner id of an eager context's fast account, the first owner of its ledger.
pub(crate) const PRIMARY: u64 = 1;
pub(crate) struct MemoryOwner {
    pub(crate) id: u64,
    grant: Option<Arc<PoolGrant>>,
}
struct Account {
    owner: Arc<MemoryOwner>,
    live: u64,
    eager: u64,
    decode: u64,
}
struct Release {
    grant: Option<Arc<PoolGrant>>,
    bytes: u64,
}
impl Release {
    fn finish(self) {
        if let Some(grant) = self.grant {
            grant.release(self.bytes);
        }
    }
}
fn memory(limit: u64, requested: u64) -> Error {
    Error::BudgetExceeded(Budget {
        kind: BudgetKind::Memory,
        limit,
        requested,
    })
}
pub(super) fn same(a: Option<&Arc<PoolGrant>>, b: Option<&Arc<PoolGrant>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}
#[cfg(test)]
thread_local! {
    /// Ledger creations and lock acquisitions on this thread (for tests of the
    /// per-query cost of queries that never need the ledger). The counter is
    /// thread-local, so ledger work on other threads, such as rayon workers, is not
    /// counted.
    pub(crate) static LEDGER_OPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
impl QueryMemoryLedger {
    #[inline]
    fn lock(&self) -> parking_lot::MutexGuard<'_, State> {
        #[cfg(test)]
        LEDGER_OPS.with(|n| n.set(n.get() + 1));
        self.state.lock()
    }
    /// A ledger that takes over an eager context's fast account (see
    /// [`super::ctx::Ctx::memory`]). Owner [`PRIMARY`] is that account, with its live
    /// bytes and its admitted eager contribution to `grant`, so charges made before the
    /// ledger existed release to it and the ledger returns its contribution on drop.
    pub(crate) fn seeded(
        grant: Option<Arc<PoolGrant>>,
        live: u64,
        eager: u64,
        peak: u64,
    ) -> Arc<Self> {
        #[cfg(test)]
        LEDGER_OPS.with(|n| n.set(n.get() + 1));
        debug_assert!(grant.is_none() || eager >= live, "eager covers live");
        Arc::new(Self {
            live: AtomicU64::new(live),
            peak: AtomicU64::new(peak),
            state: Mutex::new(State {
                owners: vec![Account {
                    owner: Arc::new(MemoryOwner { id: PRIMARY, grant }),
                    live,
                    eager,
                    decode: 0,
                }],
                next: PRIMARY,
                cursor: false,
                calls: 0,
            }),
        })
    }
    /// Prepare bookkeeping outside the admission lock. Failed and zero-only public
    /// reconfiguration cannot accumulate idle owner records.
    pub(crate) fn owner(&self, grant: Option<&Arc<PoolGrant>>) -> Result<Arc<MemoryOwner>> {
        let mut prepared = None;
        let mut storage = Vec::new();
        loop {
            let mut state = self.lock();
            if let Some(i) = state.owners.iter().position(|a| {
                a.live == 0
                    && a.eager == 0
                    && Arc::strong_count(&a.owner) == 1
                    && !same(a.owner.grant.as_ref(), grant)
            }) {
                let retired = state.owners.swap_remove(i);
                drop(state);
                drop(retired);
                continue;
            }
            if let Some(account) = state
                .owners
                .iter()
                .find(|a| same(a.owner.grant.as_ref(), grant))
            {
                let owner = account.owner.clone();
                drop(state);
                return Ok(owner);
            }
            if prepared.is_none() {
                drop(state);
                prepared = Some(Arc::new(MemoryOwner {
                    id: 0,
                    grant: grant.cloned(),
                }));
                continue;
            }
            if state.owners.len() == state.owners.capacity() {
                let need = state
                    .owners
                    .len()
                    .checked_add(1)
                    .ok_or_else(|| memory(u64::MAX, u64::MAX))?;
                if storage.capacity() < need {
                    drop(state);
                    storage
                        .try_reserve_exact(need)
                        .map_err(|_| memory(u64::MAX, u64::MAX))?;
                    continue;
                }
                storage.append(&mut state.owners);
                std::mem::swap(&mut state.owners, &mut storage);
            }
            let id = state
                .next
                .checked_add(1)
                .ok_or_else(|| memory(u64::MAX, u64::MAX))?;
            let mut owner = prepared.take().expect("prepared owner");
            Arc::get_mut(&mut owner).expect("unpublished owner").id = id;
            state.next = id;
            state.owners.push(Account {
                owner: owner.clone(),
                live: 0,
                eager: 0,
                decode: 0,
            });
            drop(state);
            return Ok(owner);
        }
    }
    pub(crate) fn owner_by_id(&self, id: u64) -> Arc<MemoryOwner> {
        self.lock()
            .owners
            .iter()
            .find(|a| a.owner.id == id)
            .expect("owned account")
            .owner
            .clone()
    }
    fn admit(
        &self,
        state: &mut State,
        owner: &MemoryOwner,
        limit: u64,
        bytes: u64,
        decode: bool,
    ) -> Result<()> {
        let prior = self.live.load(Ordering::Relaxed);
        let live = prior
            .checked_add(bytes)
            .filter(|n| *n <= limit)
            .ok_or_else(|| memory(limit, prior.saturating_add(bytes)))?;
        let cursor = state.cursor;
        let account = state
            .owners
            .iter_mut()
            .find(|a| a.owner.id == owner.id)
            .expect("live owner");
        let own_live = account
            .live
            .checked_add(bytes)
            .ok_or_else(|| memory(limit, u64::MAX))?;
        let own_decode = account
            .decode
            .checked_add(if decode { bytes } else { 0 })
            .ok_or_else(|| memory(limit, u64::MAX))?;
        if let Some(grant) = &owner.grant {
            if cursor {
                grant.charge(bytes)?;
            } else if own_live > account.eager {
                grant.charge(own_live - account.eager)?;
                account.eager = own_live;
            }
        }
        account.live = own_live;
        account.decode = own_decode;
        self.live.store(live, Ordering::Release);
        self.peak.fetch_max(live, Ordering::Relaxed);
        Ok(())
    }
    pub(crate) fn reserve(
        &self,
        owner: &MemoryOwner,
        limit: u64,
        bytes: u64,
        decode: bool,
    ) -> Result<()> {
        if bytes == 0 {
            return Ok(());
        }
        self.admit(&mut self.lock(), owner, limit, bytes, decode)
    }
    fn release_locked(&self, state: &mut State, id: u64, bytes: u64, decode: bool) -> Release {
        let cursor = state.cursor;
        let account = state
            .owners
            .iter_mut()
            .find(|a| a.owner.id == id)
            .expect("retained owner");
        if decode {
            account.decode = account
                .decode
                .checked_sub(bytes)
                .expect("owned decode bytes");
        } else {
            assert!(
                bytes <= account.live - account.decode,
                "release only owned nondecode bytes"
            );
        }
        account.live = account.live.checked_sub(bytes).expect("owned live bytes");
        let live = self
            .live
            .load(Ordering::Relaxed)
            .checked_sub(bytes)
            .expect("query live bytes");
        self.live.store(live, Ordering::Release);
        Release {
            grant: cursor.then(|| account.owner.grant.clone()).flatten(),
            bytes,
        }
    }
    pub(crate) fn release(&self, id: u64, bytes: u64) {
        let release = {
            let mut state = self.lock();
            self.release_locked(&mut state, id, bytes, false)
        };
        release.finish();
    }
    pub(crate) fn trim_zero(&self, grant: Option<&Arc<PoolGrant>>) {
        let cursor = self.lock().cursor;
        if cursor && let Some(grant) = grant {
            grant.release(0);
        }
    }
    pub(crate) fn release_decode(&self) {
        // One original owner at a time; no allocation, grant wake or callback under lock.
        loop {
            let release = {
                let mut state = self.lock();
                let Some((id, bytes)) = state
                    .owners
                    .iter()
                    .find(|a| a.decode != 0)
                    .map(|a| (a.owner.id, a.decode))
                else {
                    break;
                };
                self.release_locked(&mut state, id, bytes, true)
            };
            release.finish();
        }
    }
    pub(crate) fn check_output(&self, owner: &MemoryOwner, limit: u64, bytes: u64) -> Result<()> {
        let mut state = self.lock();
        let need = self.live.load(Ordering::Relaxed).saturating_add(bytes);
        // Historical Ctx output peak is an attempted estimate, unlike admitted ownership.
        self.peak.fetch_max(need, Ordering::Relaxed);
        if need > limit {
            return Err(memory(limit, need));
        }
        let cursor = state.cursor;
        let account = state
            .owners
            .iter_mut()
            .find(|a| a.owner.id == owner.id)
            .expect("output owner");
        if let Some(grant) = &owner.grant {
            if cursor {
                grant.cover_transient(grant.used().saturating_add(bytes))?;
            } else {
                let own = account
                    .live
                    .checked_add(bytes)
                    .ok_or_else(|| memory(limit, u64::MAX))?;
                if own > account.eager {
                    grant.charge(own - account.eager)?;
                    account.eager = own;
                }
            }
        }
        Ok(())
    }
    pub(crate) fn check_cursor_transition(&self) -> Result<()> {
        if self.lock().calls != 0 {
            return Err(Error::invalid(
                "algorithm call still owns preparation memory",
            ));
        }
        Ok(())
    }
    pub(crate) fn cursor_mode(&self) -> Result<()> {
        let mut releases = Vec::new();
        loop {
            let mut state = self.lock();
            if state.calls != 0 {
                return Err(Error::invalid(
                    "algorithm call still owns preparation memory",
                ));
            }
            if releases.capacity() < state.owners.len() {
                let n = state.owners.len();
                drop(state);
                releases
                    .try_reserve_exact(n)
                    .map_err(|_| memory(u64::MAX, u64::MAX))?;
                continue;
            }
            for account in &mut state.owners {
                if let Some(grant) = &account.owner.grant {
                    let bytes = account
                        .eager
                        .checked_sub(account.live)
                        .expect("admitted eager live ownership");
                    releases.push(Release {
                        grant: Some(grant.clone()),
                        bytes,
                    });
                }
                account.eager = 0;
            }
            state.cursor = true;
            drop(state);
            break;
        }
        for release in releases {
            release.finish();
        }
        Ok(())
    }
    pub(crate) fn start_call(&self) {
        let mut state = self.lock();
        state.calls = state.calls.checked_add(1).expect("bounded active calls");
    }
    pub(crate) fn start_service_frame(&self, service: &crate::algo::ServiceMemory) -> Result<()> {
        let mut state = self.lock();
        if service.closed.load(Ordering::Relaxed) {
            return Err(Error::invalid("SERVICE memory owner is closed"));
        }
        let frames = service
            .frames
            .load(Ordering::Relaxed)
            .checked_add(1)
            .ok_or_else(|| Error::invalid("SERVICE frame count overflows"))?;
        state.calls = state
            .calls
            .checked_add(1)
            .ok_or_else(|| Error::invalid("query frame count overflows"))?;
        service.frames.store(frames, Ordering::Relaxed);
        Ok(())
    }
    pub(crate) fn end_service_frame(&self, service: &crate::algo::ServiceMemory) {
        let mut state = self.lock();
        state.calls = state.calls.checked_sub(1).expect("active SERVICE frame");
        service.frames.store(
            service
                .frames
                .load(Ordering::Relaxed)
                .checked_sub(1)
                .expect("owned SERVICE frame"),
            Ordering::Relaxed,
        );
    }
    fn retire_service(&self, lease: &mut QueryMemoryLease) -> Result<()> {
        let Some(service) = lease.service.as_ref().cloned() else {
            return Ok(());
        };
        let state = self.lock();
        if !Arc::ptr_eq(&service.ledger, &lease.ledger)
            || service.owner.id != lease.owner.id
            || service.frames.load(Ordering::Relaxed) != 0
            || service.used.load(Ordering::Relaxed) != lease.bytes
            || service.closed.load(Ordering::Relaxed)
        {
            return Err(Error::invalid(
                "SERVICE output cannot retire before all native frames and scratch are gone",
            ));
        }
        service.used.store(0, Ordering::Relaxed);
        service.closed.store(true, Ordering::Release);
        drop(state);
        lease.service = None;
        Ok(())
    }
    /// SERVICE and query admission publish in one transaction. A residual frame is
    /// optional for transferred output leases; their shared SERVICE live stays owned.
    pub(crate) fn reserve_service(
        &self,
        service: &crate::algo::ServiceMemory,
        bytes: u64,
        frame: Option<&AtomicU64>,
    ) -> Result<()> {
        let mut state = self.lock();
        if service.closed.load(Ordering::Relaxed) {
            return Err(Error::invalid("SERVICE memory owner is closed"));
        }
        let prior = service.used.load(Ordering::Relaxed);
        let next = prior
            .checked_add(bytes)
            .filter(|n| *n <= service.limit)
            .ok_or_else(|| memory(service.limit, prior.saturating_add(bytes)))?;
        let residual = frame
            .map(|f| {
                f.load(Ordering::Relaxed)
                    .checked_add(bytes)
                    .ok_or_else(|| memory(service.limit, u64::MAX))
            })
            .transpose()?;
        if bytes != 0 {
            self.admit(
                &mut state,
                &service.owner,
                service.query_limit,
                bytes,
                false,
            )?;
        }
        service.used.store(next, Ordering::Relaxed);
        service.peak.fetch_max(next, Ordering::Relaxed);
        if let (Some(frame), Some(next)) = (frame, residual) {
            frame.store(next, Ordering::Relaxed);
        }
        Ok(())
    }
    pub(crate) fn require_service(
        &self,
        service: &crate::algo::ServiceMemory,
        bytes: u64,
    ) -> Result<()> {
        let _state = self.lock();
        if service.closed.load(Ordering::Relaxed) {
            return Err(Error::invalid("SERVICE memory owner is closed"));
        }
        let used = service.used.load(Ordering::Relaxed);
        used.checked_add(bytes)
            .filter(|n| *n <= service.limit)
            .ok_or_else(|| memory(service.limit, used.saturating_add(bytes)))?;
        let live = self.live.load(Ordering::Relaxed);
        live.checked_add(bytes)
            .filter(|n| *n <= service.query_limit)
            .map(|_| ())
            .ok_or_else(|| memory(service.query_limit, live.saturating_add(bytes)))
    }
    pub(crate) fn release_service(
        &self,
        service: &crate::algo::ServiceMemory,
        bytes: u64,
        frame: Option<&AtomicU64>,
    ) {
        let release = {
            let mut state = self.lock();
            let remaining = service
                .used
                .load(Ordering::Relaxed)
                .checked_sub(bytes)
                .expect("SERVICE owned bytes");
            let residual = frame.map(|f| {
                f.load(Ordering::Relaxed)
                    .checked_sub(bytes)
                    .expect("frame owned bytes")
            });
            let release = self.release_locked(&mut state, service.owner.id, bytes, false);
            service.used.store(remaining, Ordering::Relaxed);
            if let (Some(frame), Some(next)) = (frame, residual) {
                frame.store(next, Ordering::Relaxed);
            }
            release
        };
        release.finish();
    }
    pub(crate) fn transfer_native(
        self: &Arc<Self>,
        service: &Arc<crate::algo::ServiceMemory>,
        bytes: u64,
        frame: &AtomicU64,
    ) -> Result<QueryMemoryLease> {
        let _state = self.lock();
        let next = frame
            .load(Ordering::Relaxed)
            .checked_sub(bytes)
            .ok_or_else(|| Error::invalid("algorithm output exceeds its frame-owned capacity"))?;
        frame.store(next, Ordering::Relaxed);
        Ok(QueryMemoryLease {
            ledger: self.clone(),
            owner: service.owner.clone(),
            bytes,
            service: Some(service.clone()),
        })
    }
    #[cfg(test)]
    pub(crate) fn owners(&self) -> usize {
        self.lock().owners.len()
    }
}
impl Drop for QueryMemoryLedger {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        for account in &state.owners {
            if let Some(grant) = &account.owner.grant {
                grant.release(if state.cursor {
                    account.live
                } else {
                    account.eager
                });
            }
        }
    }
}

/// An independently owned portion of one query ledger. No payload/cache/Ctx back edge.
pub(crate) struct QueryMemoryLease {
    pub(crate) ledger: Arc<QueryMemoryLedger>,
    pub(crate) owner: Arc<MemoryOwner>,
    pub(crate) bytes: u64,
    pub(crate) service: Option<Arc<crate::algo::ServiceMemory>>,
}
impl QueryMemoryLease {
    pub(crate) fn retire_service(&mut self) -> Result<()> {
        let ledger = self.ledger.clone();
        ledger.retire_service(self)
    }

    pub(crate) fn merge(&mut self, slot: &mut Option<Self>) -> Result<()> {
        let other = slot
            .as_ref()
            .ok_or_else(|| Error::invalid("missing algorithm column admission"))?;
        if !Arc::ptr_eq(&self.ledger, &other.ledger)
            || self.owner.id != other.owner.id
            || !match (&self.service, &other.service) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
        {
            return Err(Error::invalid(
                "algorithm outputs belong to different query owners",
            ));
        }
        self.bytes = self
            .bytes
            .checked_add(other.bytes)
            .ok_or_else(|| memory(u64::MAX, u64::MAX))?;
        slot.take().expect("admitted column").bytes = 0;
        Ok(())
    }

    pub(crate) fn resize(&mut self, limit: u64, bytes: u64) -> Result<()> {
        if let Some(service) = &self.service {
            if bytes > self.bytes {
                self.ledger
                    .reserve_service(service, bytes - self.bytes, None)?;
            } else {
                self.ledger
                    .release_service(service, self.bytes - bytes, None);
            }
        } else if bytes > self.bytes {
            self.ledger
                .reserve(&self.owner, limit, bytes - self.bytes, false)?;
        } else {
            self.ledger.release(self.owner.id, self.bytes - bytes);
        }
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for QueryMemoryLease {
    fn drop(&mut self) {
        if let Some(service) = &self.service {
            self.ledger.release_service(service, self.bytes, None);
        } else {
            self.ledger.release(self.owner.id, self.bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algo::{Control, memory::Buffer};
    use crate::sparql::Ctx;
    use crate::sparql::pool::QueryPool;
    use crate::store::{Store, StoreOptions};
    const MB: u64 = 1 << 20;
    fn context() -> Ctx {
        Ctx::new(Store::in_memory(StoreOptions::default()).snapshot())
    }

    /// Queries that call no graph algorithm and keep no lease take no ledger lock and
    /// create no ledger: the per-query cost F14 added must stay off their path. Updates
    /// run their WHERE clause on the same context and take no ledger either, so one is
    /// covered too. The counter is thread-local, so it sees only the calling thread.
    /// Ledger work on rayon workers is not counted, and this small dataset stays below
    /// the thresholds of the parallel kernels, so this test guards the serial paths.
    #[test]
    fn plain_queries_take_no_ledger_operations() {
        use crate::io::{RdfFormat, Source};
        use crate::sparql::{QueryOptions, query};
        let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
        for i in 0..300 {
            data.push_str(&format!(
                "ex:p{i} a ex:T{} ; ex:name \"n{i}\" ; ex:age {} ; ex:worksFor ex:o{} ; \
                 ex:knows ex:p{} ; ex:authorOf ex:d{i} .\nex:d{i} ex:title \"t{i}\" .\n",
                i % 3,
                i % 90,
                i % 7,
                (i * 7 + 1) % 300
            ));
        }
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let pool = QueryPool::new(64 * MB);
        let opts = QueryOptions {
            prefixes: vec![("ex".into(), "http://ex.org/".into())],
            pool: Some(pool.clone()),
            max_memory_bytes: Some(32 * MB),
            ..Default::default()
        };
        for q in [
            "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s ex:knows ?o }",
            "SELECT ?p (COUNT(*) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p",
            "SELECT ?t (COUNT(?s) AS ?c) WHERE { ?s a ?t } GROUP BY ?t ORDER BY DESC(?c)",
            "SELECT ?p ?n ?a ?k WHERE { VALUES ?p { ex:p1 ex:p10 ex:p100 } \
             ?p ex:name ?n ; ex:age ?a ; ex:knows ?k }",
            "SELECT ?p ?n ?a WHERE { ?p ex:worksFor ex:o3 ; ex:name ?n ; ex:age ?a }",
            "SELECT ?p ?d ?t WHERE { ?p ex:worksFor ex:o3 . ?p ex:authorOf ?d . ?d ex:title ?t }",
            "SELECT ?p ?a WHERE { ?p ex:name ?n OPTIONAL { ?p ex:age ?a FILTER(?a > 50) } }",
            "SELECT ?p ?x WHERE { ?p ex:knows+ ?x } LIMIT 500",
            "SELECT ?p ?x WHERE { ex:p5 (ex:knows|ex:worksFor)* ?x . BIND(ex:p5 AS ?p) }",
            "CONSTRUCT { ?p ex:friend ?o } WHERE { ?p ex:knows ?o . ?o ex:worksFor ex:o3 }",
            "DESCRIBE ex:p3 ex:p4",
        ] {
            let before = LEDGER_OPS.with(|n| n.get());
            let r = query(store.snapshot(), q, &opts).unwrap();
            assert!(!r.rows().is_empty() || !r.triples.is_empty(), "{q}");
            let ops = LEDGER_OPS.with(|n| n.get()) - before;
            eprintln!("{ops} ledger operations for {q}");
            assert_eq!(ops, 0, "{ops} ledger operations for {q}");
        }
        let before = LEDGER_OPS.with(|n| n.get());
        crate::sparql::update::update(
            &store,
            "INSERT { ?p ex:colleague ?q } WHERE { ?p ex:worksFor ?o . ?q ex:worksFor ?o . \
             FILTER(?p != ?q) }",
            &opts,
        )
        .unwrap();
        assert_eq!(
            LEDGER_OPS.with(|n| n.get()),
            before,
            "ledger operations for an update"
        );
        assert_eq!(pool.granted(), 0);
    }
    /// A grant set after the first charge would leave that charge on the old grant,
    /// which debug builds refuse.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "the grant is set before the first charge")]
    fn set_grant_after_a_charge_is_refused() {
        let pool = QueryPool::new(64 * MB);
        let mut ctx = context();
        ctx.set_grant(Some(Arc::new(pool.empty_grant())));
        ctx.check_output((MB / 8) as usize, 1).unwrap();
        ctx.set_grant(None);
    }
    /// Charges made before the ledger exists move to its first owner: they grow and
    /// release there, the eager contribution stays admitted once, and the ledger
    /// returns it when the context drops.
    #[test]
    fn fast_account_hands_over_outstanding_charges_to_the_ledger() {
        let pool = QueryPool::new(64 * MB);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = context();
        ctx.reconfigure_grant(Some(grant.clone()));
        ctx.mem_limit = 8 * MB;
        let before = LEDGER_OPS.with(|n| n.get());
        let held = ctx.charge(3 * MB).unwrap();
        ctx.check_output((MB / 8) as usize, 1).unwrap();
        drop(ctx.charge(2 * MB).unwrap());
        assert!(matches!(ctx.check_output((6 * MB / 8) as usize, 1),
            Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory && b.requested == 9 * MB));
        assert_eq!(LEDGER_OPS.with(|n| n.get()), before, "no ledger yet");
        assert_eq!((grant.used(), ctx.mem_peak()), (5 * MB, 9 * MB));
        // An algorithm call makes the ledger, which takes over the account.
        let ctl = Control::for_query(&ctx, 8 * MB).unwrap();
        assert_eq!(ctx.memory().live.load(Ordering::Acquire), 3 * MB);
        assert_eq!(ctx.memory().owners(), 1);
        held.add(MB).unwrap();
        assert_eq!(
            (ctx.memory().live.load(Ordering::Acquire), grant.used()),
            (4 * MB, 5 * MB)
        );
        held.add(2 * MB).unwrap();
        assert_eq!(grant.used(), 6 * MB);
        assert!(
            matches!(held.add(3 * MB), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
        );
        drop(held);
        assert_eq!(ctx.memory().live.load(Ordering::Acquire), 0);
        drop(ctl);
        assert_eq!(ctx.mem_peak(), 9 * MB);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }
    /// Charges racing the hand-over to the ledger are counted once, on one side.
    #[test]
    fn concurrent_charges_survive_the_hand_over() {
        for round in 0..20 {
            let pool = QueryPool::new(1 << 40);
            let grant = Arc::new(pool.empty_grant());
            let mut ctx = context();
            ctx.reconfigure_grant(Some(grant.clone()));
            let ctx = Arc::new(ctx);
            let barrier = Arc::new(std::sync::Barrier::new(5));
            let threads: Vec<_> = (0..4)
                .map(|t| {
                    let ctx = ctx.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        for i in 0..2000u64 {
                            let c = ctx.charge(1 + (i + t) % 97).unwrap();
                            ctx.check_output(((i * 13) % 50) as usize, 2).unwrap();
                            c.add(5).unwrap();
                            drop(c);
                        }
                    })
                })
                .collect();
            barrier.wait();
            for _ in 0..round * 50 {
                std::hint::spin_loop();
            }
            let ledger = ctx.memory().clone();
            for t in threads {
                t.join().unwrap();
            }
            assert_eq!(ledger.live.load(Ordering::Acquire), 0);
            assert!(grant.used() > 0, "the eager contribution stays admitted");
            drop(ledger);
            drop(Arc::into_inner(ctx).unwrap());
            assert_eq!(grant.used(), 0, "round {round}");
        }
    }
    #[test]
    fn current_public_limit_and_each_original_grant_remain_authoritative() {
        let pool = QueryPool::new(64 * MB);
        let a = Arc::new(pool.empty_grant());
        let b = Arc::new(pool.empty_grant());
        let mut ctx = context();
        ctx.mem_limit = 16 * MB;
        ctx.reconfigure_grant(Some(a.clone()));
        std::mem::forget(ctx.charge(4 * MB).unwrap());
        ctx.reconfigure_grant(Some(b.clone()));
        drop(ctx.charge(2 * MB).unwrap());
        assert_eq!((a.used(), b.used()), (4 * MB, 2 * MB));
        ctx.mem_limit = 5 * MB;
        assert!(
            matches!(ctx.charge(2 * MB), Err(Error::BudgetExceeded(x)) if x.kind==BudgetKind::Memory && x.limit==5*MB)
        );
        assert_eq!((a.used(), b.used()), (4 * MB, 2 * MB));
        ctx.mem_limit = 16 * MB;
        ctx.reconfigure_grant(None);
        drop(ctx.charge(MB).unwrap());
        ctx.reconfigure_grant(Some(a.clone()));
        drop(ctx.charge(MB).unwrap());
        assert_eq!((a.used(), b.used()), (5 * MB, 2 * MB));
        let weak = Arc::downgrade(ctx.memory());
        drop(ctx);
        assert!(weak.upgrade().is_none());
        assert_eq!((a.used(), b.used()), (0, 0));
        drop((a, b));
        assert_eq!(pool.granted(), 0);
    }
    #[test]
    fn rejected_and_zero_configurations_do_not_accumulate_owners() {
        let pool = QueryPool::new(0);
        let mut ctx = context();
        for _ in 0..100 {
            ctx.reconfigure_grant(Some(Arc::new(pool.empty_grant())));
            assert!(
                matches!(ctx.charge(1),Err(Error::BudgetExceeded(x)) if x.kind==BudgetKind::ServerMemory)
            );
            drop(ctx.charge(0).unwrap());
            assert!(ctx.memory().owners() <= 1);
        }
        assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
        assert_eq!(ctx.mem_peak(), 0);
    }
    #[test]
    fn cursor_transition_and_decode_scratch_release_each_original_owner() {
        let pool = QueryPool::new(64 * MB);
        let a = Arc::new(pool.empty_grant());
        let b = Arc::new(pool.empty_grant());
        let mut ctx = context();
        ctx.reconfigure_grant(Some(a.clone()));
        drop(ctx.charge(4 * MB).unwrap());
        ctx.reconfigure_grant(Some(b.clone()));
        std::mem::forget(ctx.charge(2 * MB).unwrap());
        ctx.enable_cursor_accounting().unwrap();
        assert_eq!((a.used(), b.used()), (0, 2 * MB));
        assert!(ctx.cursor_decode(2000));
        let b_before = b.used();
        assert!(b_before > 2 * MB);
        ctx.reconfigure_grant(Some(a.clone()));
        assert!(ctx.cursor_decode(4000));
        assert!(a.used() > 0);
        let keep = ctx.charge(300).unwrap();
        ctx.release_cursor_work();
        assert_eq!((a.used(), b.used()), (300, 2 * MB));
        drop(keep);
        drop(ctx);
        assert_eq!((a.used(), b.used()), (0, 0));
    }
    #[test]
    fn native_call_captures_owner_limit_and_output_outlives_context() {
        let pool = QueryPool::new(64 * MB);
        let a = Arc::new(pool.empty_grant());
        let b = Arc::new(pool.empty_grant());
        let mut ctx = context();
        ctx.reconfigure_grant(Some(a.clone()));
        ctx.mem_limit = 4 * MB;
        let ctl = Control::for_query(&ctx, 8 * MB).unwrap();
        ctx.reconfigure_grant(Some(b.clone()));
        ctx.mem_limit = MB;
        let output = Buffer::<u8>::new("query-ledger-output", 2 * MB as usize, &ctl)
            .unwrap()
            .into_output();
        let lease = ctl.output_lease(output.capacity() as u64).unwrap().unwrap();
        assert_eq!((a.used(), b.used()), (2 * MB, 0));
        assert!(
            matches!(ctx.charge(1),Err(Error::BudgetExceeded(x)) if x.kind==BudgetKind::Memory)
        );
        drop(ctl);
        let weak = Arc::downgrade(ctx.memory());
        drop(ctx);
        assert!(weak.upgrade().is_some());
        assert_eq!(a.used(), 2 * MB);
        drop(output);
        drop(lease);
        assert!(weak.upgrade().is_none());
        assert_eq!((a.used(), b.used()), (0, 0));
    }
    #[test]
    fn cursor_transition_refuses_active_native_adapter_without_partial_transition() {
        let mut ctx = context();
        let ctl = Control::for_query(&ctx, u64::MAX).unwrap();
        assert!(ctx.enable_cursor_accounting().is_err());
        assert!(!ctx.is_cursor());
        drop(ctl);
        ctx.enable_cursor_accounting().unwrap();
        assert!(ctx.is_cursor());
    }
    #[test]
    fn shared_service_frames_transfer_retire_and_close_without_query_release() {
        let mut ctx = context();
        let pool = QueryPool::new(64 * MB);
        let grant = Arc::new(pool.empty_grant());
        ctx.reconfigure_grant(Some(grant.clone()));
        let root = Control::for_query(&ctx, 100).unwrap();
        let frame = root.query_frame().unwrap();
        let data = Buffer::<u8>::new("service-first", 80, &frame)
            .unwrap()
            .into_output();
        let mut lease = frame.output_lease(80).unwrap().unwrap();
        let service = lease.service.as_ref().unwrap().clone();
        assert_eq!((root.remaining(), root.peak()), (20, 80));
        assert!(lease.retire_service().is_err());
        drop(frame);
        let next = root.query_frame().unwrap();
        assert!(
            matches!(Buffer::<u8>::new("service-aggregate",21,&next),Err(Error::BudgetExceeded(b)) if b.kind==BudgetKind::Memory && b.limit==100 && b.requested==101)
        );
        assert_eq!((root.remaining(), root.peak()), (20, 80));
        drop(next);
        drop(root);
        let before = (
            ctx.memory().live.load(Ordering::Acquire),
            grant.used(),
            ctx.mem_peak(),
        );
        lease.retire_service().unwrap();
        assert!(lease.service.is_none());
        assert_eq!(
            before,
            (
                ctx.memory().live.load(Ordering::Acquire),
                grant.used(),
                ctx.mem_peak()
            )
        );
        assert_eq!(service.peak.load(Ordering::Relaxed), 80);
        assert!(service.closed.load(Ordering::Acquire));
        assert!(matches!(
            ctx.memory().reserve_service(&service, 1, None),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            ctx.memory().start_service_frame(&service),
            Err(Error::Invalid(_))
        ));
        // Later query-only parent/cursor growth is not constrained by completed maxMemory.
        lease.resize(ctx.mem_limit, 101).unwrap();
        assert_eq!(service.peak.load(Ordering::Relaxed), 80);
        drop(data);
        drop(lease);
        assert_eq!(ctx.memory().live.load(Ordering::Acquire), 0);
        drop(service);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }
    #[test]
    fn dead_service_frame_residuals_do_not_accumulate_and_exact_capacity_refuses_before_allocation()
    {
        let ctx = context();
        let root = Control::for_query(&ctx, 80).unwrap();
        for _ in 0..8 {
            let frame = root.query_frame().unwrap();
            let data = Buffer::<u8>::new("service-residual", 80, &frame)
                .unwrap()
                .into_output();
            assert_eq!(root.remaining(), 0);
            drop(data);
            drop(frame);
            assert_eq!(root.remaining(), 80);
        }
        assert_eq!(root.peak(), 80);
        let frame = root.query_frame().unwrap();
        assert!(
            matches!(Buffer::<u8>::new("service-one-over",81,&frame),Err(Error::BudgetExceeded(b)) if b.kind==BudgetKind::Memory && b.requested==81)
        );
        assert_eq!((root.remaining(), root.peak()), (80, 80));
        drop(frame);
        drop(root);
        assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn refused_cursor_transition_with_prepared_local_terms_changes_no_ownership() {
        let mut ctx = context();
        ctx.intern_term(&oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
            "local".repeat(1024),
        )));
        let ctl = Control::for_query(&ctx, u64::MAX).unwrap();
        let before = (
            ctx.memory().live.load(Ordering::Acquire),
            ctx.mem_peak(),
            ctx.local_len(),
        );
        assert!(ctx.enable_cursor_accounting().is_err());
        assert_eq!(
            before,
            (
                ctx.memory().live.load(Ordering::Acquire),
                ctx.mem_peak(),
                ctx.local_len()
            ),
            "refused mode change must not charge local terms"
        );
        drop(ctl);
        ctx.enable_cursor_accounting().unwrap();
        assert!(ctx.is_cursor());
    }
    #[test]
    fn concurrent_native_admission_peak_excludes_rejected_pending_capacity() {
        use std::sync::mpsc;
        use std::time::Duration;
        let pool = QueryPool::new(4 * MB);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = context();
        ctx.reconfigure_grant(Some(grant.clone()));
        let controls = [
            Control::for_query(&ctx, 8 * MB).unwrap(),
            Control::for_query(&ctx, 8 * MB).unwrap(),
        ];
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let (send, receive) = mpsc::channel();
        let mut threads = Vec::new();
        let mut releases = Vec::new();
        for (i, ctl) in controls.iter().cloned().enumerate() {
            let (release, wait) = mpsc::channel();
            releases.push(release);
            let send = send.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move|| {
                barrier.wait();let allocation=Buffer::<u8>::new("query-ledger-concurrent",3*MB as usize,&ctl);
                send.send((i,allocation.as_ref().map(|b|b.capacity()).map_err(|e|matches!(e,Error::BudgetExceeded(x) if x.kind==BudgetKind::ServerMemory)))).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();drop(allocation);
            }));
        }
        drop(send);
        let first = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        let second = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        for release in releases {
            release.send(()).unwrap();
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(
            usize::from(first.1.is_ok()) + usize::from(second.1.is_ok()),
            1
        );
        assert!(
            first
                .1
                .as_ref()
                .err()
                .or(second.1.as_ref().err())
                .copied()
                .unwrap()
        );
        let peaks = [controls[0].peak(), controls[1].peak()];
        assert_eq!(peaks.iter().sum::<u64>(), 3 * MB);
        assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
        assert_eq!(ctx.mem_peak(), 3 * MB);
        assert_eq!(grant.used(), 3 * MB);
        drop(controls);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }
}
