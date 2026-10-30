//! Reversible admission and ownership proofs for a managed Store close.

use super::{BranchSet, Store, branching::SetRef};
use crate::{Error, Result};
use parking_lot::{Condvar, Mutex};
use std::collections::BTreeMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

enum Phase<T> {
    Open(Weak<T>),
    Closing,
    Closed,
}

struct LinkState<T> {
    phase: Phase<T>,
    uses: usize,
}

/// A built-in weak reference whose upgrades can be paused without changing identity.
/// Ordinary callers park during a reversible close. Controlled callers use
/// `try_upgrade` and check their own cancellation, deadline and no-wait policy.
#[doc(hidden)]
pub struct WeakLink<T> {
    address: usize,
    state: Mutex<LinkState<T>>,
    changed: Condvar,
}

impl<T> std::fmt::Debug for WeakLink<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeakLink")
            .field("address", &self.address)
            .finish()
    }
}

impl<T> WeakLink<T> {
    pub fn new(weak: Weak<T>) -> Arc<Self> {
        Arc::new(Self {
            address: weak.as_ptr() as usize,
            state: Mutex::new(LinkState {
                phase: Phase::Open(weak),
                uses: 0,
            }),
            changed: Condvar::new(),
        })
    }

    pub fn address(&self) -> usize {
        self.address
    }

    pub fn upgrade(&self) -> Option<Arc<T>> {
        let mut state = self.state.lock();
        loop {
            match &state.phase {
                Phase::Open(weak) => return weak.upgrade(),
                Phase::Closed => return None,
                Phase::Closing => self.changed.wait(&mut state),
            }
        }
    }

    /// Never wait for a contended link or a closing reservation.
    pub fn try_upgrade(&self) -> Result<Option<Arc<T>>> {
        let state = self.state.try_lock().ok_or(Error::WriterBusy)?;
        match &state.phase {
            Phase::Open(weak) => Ok(weak.upgrade()),
            Phase::Closing => Err(Error::WriterBusy),
            Phase::Closed => Ok(None),
        }
    }

    pub fn upgrade_controlled(&self, opts: &crate::guard::WriteOptions) -> Result<Option<Arc<T>>> {
        loop {
            opts.check()?;
            match self.try_upgrade() {
                Err(Error::WriterBusy) if !opts.no_wait => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                result => {
                    opts.check()?;
                    return result;
                }
            }
        }
    }

    pub fn strong_count(&self) -> usize {
        let state = self.state.lock();
        match &state.phase {
            Phase::Open(weak) => weak.strong_count(),
            Phase::Closing | Phase::Closed => 0,
        }
    }

    pub fn enter(self: &Arc<Self>) -> Option<LinkUse<T>> {
        let mut state = self.state.lock();
        loop {
            match state.phase {
                Phase::Open(_) => {
                    state.uses += 1;
                    return Some(LinkUse(self.clone()));
                }
                Phase::Closed => return None,
                Phase::Closing => self.changed.wait(&mut state),
            }
        }
    }

    /// Nonblocking admission for controlled opening and resident lookup paths.
    pub fn try_enter(self: &Arc<Self>) -> Result<Option<LinkUse<T>>> {
        let mut state = self.state.try_lock().ok_or(Error::WriterBusy)?;
        match state.phase {
            Phase::Open(_) => {
                state.uses += 1;
                Ok(Some(LinkUse(self.clone())))
            }
            Phase::Closing => Err(Error::WriterBusy),
            Phase::Closed => Ok(None),
        }
    }

    pub fn freeze(self: &Arc<Self>) -> Result<FrozenWeak<T>> {
        let mut state = self.state.try_lock().ok_or(Error::WriterBusy)?;
        if state.uses != 0 || !matches!(state.phase, Phase::Open(_)) {
            return Err(Error::WriterBusy);
        }
        // Drop the actual Weak. Retaining it in the token would defeat get_mut.
        state.phase = Phase::Closing;
        Ok(FrozenWeak {
            link: self.clone(),
            resolved: false,
        })
    }
}

#[doc(hidden)]
pub struct LinkUse<T>(Arc<WeakLink<T>>);

impl<T> Drop for LinkUse<T> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock();
        state.uses -= 1;
        self.0.changed.notify_all();
    }
}

#[doc(hidden)]
pub struct FrozenWeak<T> {
    link: Arc<WeakLink<T>>,
    resolved: bool,
}

impl<T> FrozenWeak<T> {
    pub fn restore(mut self, owner: &Arc<T>) {
        assert_eq!(Arc::as_ptr(owner) as usize, self.link.address);
        self.link.state.lock().phase = Phase::Open(Arc::downgrade(owner));
        self.resolved = true;
        self.link.changed.notify_all();
    }

    pub fn close(mut self) {
        self.link.state.lock().phase = Phase::Closed;
        self.resolved = true;
        self.link.changed.notify_all();
    }
}

impl<T> Drop for FrozenWeak<T> {
    fn drop(&mut self) {
        if !self.resolved {
            // The owning reservation restores tokens before dropping them. On an
            // unexpected unwind, wake waiters rather than leaving a permanent park.
            self.link.state.lock().phase = Phase::Closed;
            self.link.changed.notify_all();
        }
    }
}

/// A real exclusive family permit retained until all committed Store destructors
/// finish. It is a lifetime boundary, never a managed-write authority.
#[doc(hidden)]
pub struct CloseLease {
    _admission: super::family::AdmissionPermit,
    _family: Arc<super::family::Family>,
}

/// Reversible core proof over an exclusively borrowed main Store. The facade
/// keeps that Store in its original Own payload and evacuates only Branch Arcs.
#[doc(hidden)]
pub struct FamilyClose<'a> {
    main: &'a mut Store,
    stores: BTreeMap<usize, Arc<Store>>,
    resident: Vec<(uuid::Uuid, usize)>,
    inventory: Vec<usize>,
    set: Option<FrozenWeak<BranchSet>>,
    admission: Option<super::family::AdmissionPermit>,
    evacuated: bool,
    restoring: bool,
    committed: bool,
    prepared: bool,
}

impl<'a> FamilyClose<'a> {
    pub fn new(main: &'a mut Store, stores: Vec<Arc<Store>>) -> Self {
        let mut canonical = BTreeMap::new();
        for store in stores {
            canonical
                .entry(Arc::as_ptr(&store) as usize)
                .or_insert(store);
        }
        Self {
            main,
            stores: canonical,
            resident: Vec::new(),
            inventory: Vec::new(),
            set: None,
            admission: None,
            evacuated: false,
            restoring: false,
            committed: false,
            prepared: false,
        }
    }

    /// Nonblocking, with no file writes and no destruction of actual Stores.
    pub fn reserve(&mut self) -> bool {
        if self.admission.is_some() || self.restoring || self.committed || self.main.is_branch() {
            return false;
        }
        if self
            .stores
            .values()
            .any(|store| !Arc::ptr_eq(&store.family, &self.main.family))
        {
            return false;
        }
        let Ok(admission) = self.main.family.gate.try_admit() else {
            return false;
        };
        self.admission = Some(admission);
        let set = match &self.main.branching.set {
            SetRef::Owner(set) => Some(set.clone()),
            SetRef::None => None,
            SetRef::Member(_) => return false,
        };
        if let Some(set) = set {
            let Ok(frozen) = set.me.freeze() else {
                return false;
            };
            self.set = Some(frozen);
            let Some(mut inventory) = set.openings.try_lock() else {
                return false;
            };
            let Some(mut resident) = set.stores.try_lock() else {
                return false;
            };
            // Capture only normal resident/facade ownership. Upgrading an
            // unrelated detached Weak could make this close its final owner,
            // whose destructor must never run during a reversible refusal.
            self.resident.reserve(resident.len());
            for (id, store) in resident.drain() {
                let address = Arc::as_ptr(&store) as usize;
                self.resident.push((id, address));
                self.stores.entry(address).or_insert(store);
            }
            self.evacuated = true;
            for (address, opening) in inventory.iter() {
                if self.stores.contains_key(address) {
                    self.inventory.push(*address);
                } else if opening.store.strong_count() != 0 || opening.complete.strong_count() != 0
                {
                    // These counts justify refusal, never a live uniqueness
                    // proof. Strong-zero is absent only after final resources.
                    return false;
                }
            }
            inventory.clear();
            drop(resident);
            drop(inventory);
            drop(set);
        }
        self.main.failpoint("family-close-inventoried");
        if self
            .stores
            .values_mut()
            .any(|store| Arc::get_mut(store).is_none())
        {
            return false;
        }
        if let SetRef::Owner(set) = &mut self.main.branching.set
            && Arc::get_mut(set).is_none()
        {
            return false;
        }
        self.prepared = true;
        true
    }

    fn restore_edges(&mut self, weak: bool) {
        if !self.evacuated {
            return;
        }
        if let SetRef::Owner(set) = &self.main.branching.set {
            let mut resident = set.stores.lock();
            for (id, address) in &self.resident {
                resident.insert(*id, self.stores[address].clone());
            }
            if weak {
                let mut inventory = set.openings.lock();
                for address in &self.inventory {
                    inventory.insert(
                        *address,
                        super::branching::Opening {
                            store: Arc::downgrade(&self.stores[address]),
                            complete: Arc::downgrade(&self.stores[address]._close_complete),
                        },
                    );
                }
            }
        }
        self.evacuated = false;
    }

    pub fn restore_core(&mut self) {
        self.restore_edges(true);
        self.restoring = true;
    }

    /// Only rollback or committed close may recreate facade owning edges.
    pub fn restored_store(&self, address: usize) -> Arc<Store> {
        assert!(self.restoring || self.committed);
        self.stores[&address].clone()
    }

    pub fn finish_rollback(&mut self) {
        assert!(self.restoring);
        if let Some(frozen) = self.set.take()
            && let SetRef::Owner(set) = &self.main.branching.set
        {
            frozen.restore(set);
        }
        self.admission = None;
    }

    pub fn commit(&mut self) {
        assert!(self.prepared && !self.restoring && !self.committed);
        self.committed = true;
        if let Some(frozen) = self.set.take() {
            frozen.close();
        }
        self.restore_edges(false);
    }

    /// End the exclusive Store borrow while preserving real family exclusion.
    /// The caller drops every committed facade/core owner before this lease.
    pub fn finish_commit(mut self) -> (CloseLease, Option<Box<dyn std::any::Any + Send>>) {
        assert!(self.committed);
        let lease = CloseLease {
            _admission: self.admission.take().expect("committed family admission"),
            _family: self.main.family.clone(),
        };
        // A detached member may have no normal owning edge after commit. Finish
        // every such destructor while exclusion remains held, preserving the
        // first callback panic for the facade's remaining resource finalization.
        let mut panic = None;
        for (_, store) in std::mem::take(&mut self.stores) {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(store)));
            if panic.is_none() {
                panic = result.err();
            }
        }
        (lease, panic)
    }
}

impl Drop for FamilyClose<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.restore_core();
            self.finish_rollback();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::Instant;

    #[test]
    fn internal_weak_freeze_is_reversible_and_preserves_the_control_block() {
        let mut owner = Arc::new(41);
        let link = WeakLink::new(Arc::downgrade(&owner));
        let address = Arc::as_ptr(&owner);
        let frozen = link.freeze().unwrap();
        assert!(matches!(link.try_upgrade(), Err(Error::WriterBusy)));
        assert!(Arc::get_mut(&mut owner).is_some());
        frozen.restore(&owner);
        assert_eq!(Arc::as_ptr(&link.upgrade().unwrap()), address);
    }

    #[test]
    fn an_external_weak_blocks_uniqueness_without_changing_its_identity() {
        let mut owner = Arc::new(41);
        let link = WeakLink::new(Arc::downgrade(&owner));
        let external = Arc::downgrade(&owner);
        let frozen = link.freeze().unwrap();
        assert!(Arc::get_mut(&mut owner).is_none());
        frozen.restore(&owner);
        assert!(Arc::ptr_eq(&external.upgrade().unwrap(), &owner));
        drop(external);
        let frozen = link.freeze().unwrap();
        assert!(Arc::get_mut(&mut owner).is_some());
        frozen.close();
        assert!(link.upgrade().is_none());
    }

    #[test]
    fn parked_upgrade_resumes_on_rollback_and_observes_committed_close() {
        for commit in [false, true] {
            let owner = Arc::new(41);
            let link = WeakLink::new(Arc::downgrade(&owner));
            let frozen = link.freeze().unwrap();
            let worker = link.clone();
            let (started, ready) = mpsc::channel();
            let (finished, result) = mpsc::channel();
            let join = std::thread::spawn(move || {
                started.send(()).unwrap();
                finished.send(worker.upgrade()).unwrap();
            });
            ready.recv_timeout(Duration::from_secs(1)).unwrap();
            assert!(result.recv_timeout(Duration::from_millis(10)).is_err());
            if commit {
                frozen.close();
            } else {
                frozen.restore(&owner);
            }
            let upgraded = result.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(upgraded.is_none(), commit);
            if let Some(upgraded) = upgraded {
                assert!(Arc::ptr_eq(&upgraded, &owner));
            }
            join.join().unwrap();
        }
    }

    #[test]
    fn controlled_upgrade_checks_no_wait_cancellation_and_deadline_while_closing() {
        let owner = Arc::new(41);
        let link = WeakLink::new(Arc::downgrade(&owner));
        let frozen = link.freeze().unwrap();
        let no_wait = crate::guard::WriteOptions {
            no_wait: true,
            ..Default::default()
        };
        assert!(matches!(
            link.upgrade_controlled(&no_wait),
            Err(Error::WriterBusy)
        ));
        let cancel = Arc::new(AtomicBool::new(true));
        let cancelled = crate::guard::WriteOptions {
            cancel: Some(cancel.clone()),
            ..Default::default()
        };
        assert!(matches!(
            link.upgrade_controlled(&cancelled),
            Err(Error::Cancelled)
        ));
        cancel.store(false, Ordering::SeqCst);
        let deadline = crate::guard::WriteOptions {
            deadline: Some(Instant::now()),
            ..Default::default()
        };
        assert!(matches!(
            link.upgrade_controlled(&deadline),
            Err(Error::Timeout)
        ));
        frozen.restore(&owner);
    }

    #[test]
    fn controlled_wait_stops_before_the_closing_reservation_is_released() {
        for cancelled in [false, true] {
            let owner = Arc::new(41);
            let link = WeakLink::new(Arc::downgrade(&owner));
            let frozen = link.freeze().unwrap();
            let cancel = Arc::new(AtomicBool::new(false));
            let options = crate::guard::WriteOptions {
                cancel: Some(cancel.clone()),
                deadline: (!cancelled).then(|| Instant::now() + Duration::from_millis(20)),
                ..Default::default()
            };
            let (started, ready) = mpsc::channel();
            let (finished, result) = mpsc::channel();
            let worker = link.clone();
            let join = std::thread::spawn(move || {
                started.send(()).unwrap();
                finished.send(worker.upgrade_controlled(&options)).unwrap();
            });
            ready.recv_timeout(Duration::from_secs(1)).unwrap();
            if cancelled {
                cancel.store(true, Ordering::SeqCst);
            }
            let stopped = result
                .recv_timeout(Duration::from_secs(1))
                .expect("controls must stop without thawing the link");
            assert!(if cancelled {
                matches!(stopped, Err(Error::Cancelled))
            } else {
                matches!(stopped, Err(Error::Timeout))
            });
            frozen.restore(&owner);
            join.join().unwrap();
        }
    }

    #[test]
    fn nonblocking_link_admission_refuses_active_operations_and_mutex_contention() {
        let owner = Arc::new(41);
        let link = WeakLink::new(Arc::downgrade(&owner));
        let active = link.enter().unwrap();
        assert!(matches!(link.freeze(), Err(Error::WriterBusy)));
        let lock = link.state.lock();
        assert!(matches!(link.try_upgrade(), Err(Error::WriterBusy)));
        assert!(matches!(link.try_enter(), Err(Error::WriterBusy)));
        drop(lock);
        drop(active);
        link.freeze().unwrap().restore(&owner);
    }

    #[test]
    fn exact_member_strong_and_weak_refusals_restore_the_same_opening() {
        let mut main = Store::in_memory(Default::default());
        main.create_branch("child", &Default::default()).unwrap();
        let child = main.branch("child").unwrap().shared().unwrap();
        let pointer = Arc::as_ptr(&child);
        {
            let mut close = FamilyClose::new(&mut main, vec![]);
            assert!(!close.reserve());
        }
        assert_eq!(
            Arc::as_ptr(&main.branch("child").unwrap().shared().unwrap()),
            pointer
        );
        let weak = Arc::downgrade(&child);
        drop(child);
        {
            let mut close = FamilyClose::new(&mut main, vec![]);
            assert!(!close.reserve());
        }
        assert_eq!(Arc::as_ptr(&weak.upgrade().unwrap()), pointer);
        drop(weak);
        let mut close = FamilyClose::new(&mut main, vec![]);
        assert!(close.reserve());
        close.commit();
        let (lease, panic) = close.finish_commit();
        assert!(panic.is_none());
        drop(main);
        drop(lease);
    }

    #[test]
    fn exact_set_strong_and_weak_refusals_restore_the_same_control_block() {
        let mut main = Store::in_memory(Default::default());
        main.create_branch("child", &Default::default()).unwrap();
        let set = main.branch_set().unwrap();
        let pointer = Arc::as_ptr(&set);
        {
            let mut close = FamilyClose::new(&mut main, vec![]);
            assert!(!close.reserve());
        }
        assert_eq!(Arc::as_ptr(&main.branch_set().unwrap()), pointer);
        let weak = Arc::downgrade(&set);
        drop(set);
        {
            let mut close = FamilyClose::new(&mut main, vec![]);
            assert!(!close.reserve());
        }
        assert_eq!(Arc::as_ptr(&weak.upgrade().unwrap()), pointer);
        drop(weak);
        {
            let mut close = FamilyClose::new(&mut main, vec![]);
            assert!(close.reserve());
        }
        main.create_branch("after-refusal", &Default::default())
            .unwrap();
    }

    #[test]
    fn actual_orphaned_pending_refuses_close_without_drain_or_file_changes() {
        for fail_sync in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut main = Store::open(
                dir.path(),
                super::super::StoreOptions {
                    group_commit: true,
                    group_commit_idle_direct: false,
                    ..Default::default()
                },
            )
            .unwrap();
            main.set_failpoint(
                "group-appended",
                Some(Arc::new(|_| panic!("abandon real pending"))),
            );
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                use oxrdf::{GraphName, NamedNode, Quad};
                let mut txn = main
                    .try_write_with(crate::commit::CommitKind::Update, Default::default())
                    .unwrap();
                let quad = Quad::new(
                    NamedNode::new("urn:s").unwrap(),
                    NamedNode::new("urn:p").unwrap(),
                    NamedNode::new("urn:o").unwrap(),
                    GraphName::DefaultGraph,
                );
                let ids = txn.encode_quad(&quad, &mut Default::default()).unwrap();
                txn.insert(ids).unwrap();
                txn.commit().unwrap();
            }));
            assert!(result.is_err());
            main.set_failpoint("group-appended", None);
            let before = std::fs::read(dir.path().join("CURRENT")).unwrap();
            {
                let mut close = FamilyClose::new(&mut main, vec![]);
                assert!(!close.reserve());
            }
            assert_eq!(std::fs::read(dir.path().join("CURRENT")).unwrap(), before);
            assert_eq!(main.snapshot().commit, 0);
            if fail_sync {
                main.fail_next_group_sync();
            }
            let drained = main.group.as_ref().unwrap().drain(&main, None);
            assert_eq!(drained.is_err(), fail_sync);
            let mut close = FamilyClose::new(&mut main, vec![]);
            assert!(close.reserve());
            close.commit();
            let (lease, panic) = close.finish_commit();
            assert!(panic.is_none());
            drop(main);
            drop(lease);
        }
    }

    #[test]
    fn detached_strong_zero_store_remains_busy_until_its_real_drop_finishes() {
        use crate::guard::{Candidate, CommitGuard, ValidationSummary};
        struct Pause {
            hit: mpsc::Sender<()>,
            go: Mutex<mpsc::Receiver<()>>,
        }
        impl CommitGuard for Pause {
            fn check(&self, _: &Candidate<'_>) -> Result<ValidationSummary> {
                unreachable!()
            }
            fn describe(&self) -> String {
                "pause real detached close".into()
            }
            fn close(&self, _: &crate::commit::CommitInfo, _: &super::super::Snapshot) {
                self.hit.send(()).unwrap();
                self.go
                    .lock()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mut main = Store::open(dir.path(), Default::default()).unwrap();
        main.create_branch("detached", &Default::default()).unwrap();
        let child = main.branch("detached").unwrap().shared().unwrap();
        main.delete_branch("detached", true).unwrap();
        let weak = Arc::downgrade(&child);
        let (hit, ready) = mpsc::channel();
        let (release, go) = mpsc::channel();
        child.set_guard(Some(Arc::new(Pause {
            hit,
            go: Mutex::new(go),
        })));
        let worker = std::thread::spawn(move || drop(child));
        let entered = ready.recv_timeout(Duration::from_secs(5));
        let zero = weak.upgrade().is_none();
        let refused = {
            let mut close = FamilyClose::new(&mut main, vec![]);
            !close.reserve()
        };
        let _ = release.send(());
        worker.join().unwrap();
        entered.unwrap();
        assert!(zero && refused);
        drop(weak);
        let mut close = FamilyClose::new(&mut main, vec![]);
        assert!(close.reserve());
    }
    #[test]
    fn moved_out_detached_store_value_keeps_the_completion_witness_live() {
        let dir = tempfile::tempdir().unwrap();
        let mut main = Store::open(dir.path(), Default::default()).unwrap();
        main.create_branch("moved-out", &Default::default())
            .unwrap();
        let child = main.branch("moved-out").unwrap().shared().unwrap();
        main.delete_branch("moved-out", true).unwrap();
        let child = Arc::try_unwrap(child)
            .ok()
            .expect("only the real outside owner remains");
        {
            let mut close = FamilyClose::new(&mut main, vec![]);
            assert!(!close.reserve());
        }
        drop(child);
        let mut close = FamilyClose::new(&mut main, vec![]);
        assert!(close.reserve());
    }

    #[test]
    fn existing_open_refuses_missing_and_wrong_roots_without_empty_creation() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert!(
            Store::open_existing(&missing, Default::default(), Some(uuid::Uuid::new_v4())).is_err()
        );
        assert!(!missing.exists());
        let root = dir.path().join("actual");
        let store = Store::open(&root, Default::default()).unwrap();
        store
            .load(&[crate::io::Source::from_bytes(
                b"<urn:actual> <urn:p> <urn:o> .".to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        let id = store.dataset_id();
        drop(store);
        let header = std::fs::read(root.join("dataset.json")).unwrap();
        let current = std::fs::read(root.join("CURRENT")).unwrap();
        assert!(
            Store::open_existing(&root, Default::default(), Some(uuid::Uuid::new_v4())).is_err()
        );
        assert_eq!(std::fs::read(root.join("dataset.json")).unwrap(), header);
        std::fs::remove_file(root.join("CURRENT")).unwrap();
        assert!(Store::open_existing(&root, Default::default(), Some(id)).is_err());
        assert!(!root.join("CURRENT").exists());
        std::fs::write(root.join("CURRENT"), &current).unwrap();
        std::fs::remove_file(root.join("dataset.json")).unwrap();
        assert!(Store::open_existing(&root, Default::default(), Some(id)).is_err());
        assert!(!root.join("dataset.json").exists());
        std::fs::write(root.join("dataset.json"), &header).unwrap();
        let reopened = Store::open_existing(&root, Default::default(), Some(id)).unwrap();
        assert_eq!(reopened.dataset_id(), id);
        assert_eq!(reopened.snapshot().len(), 1);
    }
    #[test]
    fn existing_unknown_identity_preserves_the_supported_legacy_metadata_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("legacy");
        let store = Store::open(&root, Default::default()).unwrap();
        store
            .load(&[crate::io::Source::from_bytes(
                b"<urn:legacy> <urn:p> <urn:o> .".to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        let previous_id = store.dataset_id();
        drop(store);
        // Match the genuine legacy transformation exercised by
        // tests/commits.rs::databases_from_older_versions_get_a_baseline_commit.
        // Removing only the header from a modern UUID-stamped catalog is corrupt.
        let current = std::fs::read_to_string(root.join("CURRENT")).unwrap();
        let generation = root.join(current.trim());
        let wal = generation.join("wal.log");
        let mut bytes = std::fs::read(&wal).unwrap();
        assert!(bytes.len() >= 33);
        assert_eq!(&bytes[..8], b"\x89SPKWAL\n");
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 1);
        bytes.drain(..33);
        for record in bytes.as_chunks_mut::<33>().0 {
            if record[0] == 3 {
                record[9..].fill(0);
            }
        }
        std::fs::write(wal, bytes).unwrap();
        std::fs::remove_file(generation.join("commit.json")).unwrap();
        std::fs::remove_file(root.join("commits.bin")).unwrap();
        std::fs::remove_file(root.join("dataset.json")).unwrap();
        assert!(Store::open_existing(&root, Default::default(), Some(previous_id)).is_err());
        assert!(!root.join("dataset.json").exists());
        fn copy_fixture(from: &std::path::Path, to: &std::path::Path) {
            std::fs::create_dir(to).unwrap();
            for entry in std::fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let destination = to.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    copy_fixture(&entry.path(), &destination);
                } else {
                    std::fs::copy(entry.path(), destination).unwrap();
                }
            }
        }
        let ordinary = dir.path().join("ordinary-control");
        copy_fixture(&root, &ordinary);
        let control = Store::open(&ordinary, Default::default()).unwrap();
        assert_eq!(control.snapshot().len(), 1);
        assert!(!control.dataset_id().is_nil());
        drop(control);
        let store = Store::open_existing(&root, Default::default(), None).unwrap();
        assert_eq!(store.snapshot().len(), 1);
        assert!(!store.dataset_id().is_nil());
        assert!(root.join("dataset.json").is_file());
    }
}
