//! The store side of durable source holds (see [`crate::holds`]): acquiring, releasing,
//! listing and reconciling the holds of a dataset family.
//!
//! # Ordering
//!
//! An acquisition publishes the new protection before it checks that the commits are
//! still readable, and it makes the hold durable before it returns. A collection,
//! whether a compaction, a retention change, a branch deletion or a backup lease's
//! release, decides what to remove and removes it from the generation table under the
//! store's history lock, reading the protection there. The check takes the same lock.
//! So either the collection decided before the protection was published, and the check
//! then finds the commit gone and the acquisition fails, or the collection reads the
//! protection and keeps the commit. A release makes `holds.json` durable without the
//! hold before it removes the protection, so a crash leaves the hold in place.
//!
//! Lock order: the family's holds before a branch table or a store's history, since a
//! collection can take the table under its history lock. Nothing that holds a table or a
//! history lock waits for the holds' lock. An acquisition never takes a writer lock, and
//! a collection reads the protection without taking the holds' lock.

use super::{Store, write_atomic};
use crate::branch::CommitRef;
use crate::error::{Error, Result};
use crate::holds::{self, HeldIndex, HoldsState, Record, SourceHold};
use crate::managed::OwnerKind;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// The outcome of [`Store::acquire_hold`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Acquired {
    pub hold: SourceHold,
    /// false when a hold of that id already held the same commits
    pub created: bool,
}

/// Puts the protection back to what `holds.json` records when an acquisition fails.
struct Unpublished<'a> {
    store: &'a Store,
    previous: Option<Arc<HeldIndex>>,
    touched: Vec<uuid::Uuid>,
}

impl Drop for Unpublished<'_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.store.family.holds.index.store(previous);
            self.store.settle_members(&self.touched);
        }
    }
}

impl Store {
    fn holds_main(&self) -> Result<()> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        if self.is_branch() {
            return Err(crate::branch::invalid_branch(
                "source holds are changed through the dataset's main store",
            ));
        }
        Ok(())
    }

    /// Every source hold of the dataset and its branches, by id. Fails with
    /// `holds-unavailable` when `holds.json` cannot be read.
    pub fn source_holds(&self) -> Result<Vec<SourceHold>> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        match &*self.family.holds.state.lock() {
            HoldsState::Ready(r) => Ok(r.values().map(|r| r.hold.clone()).collect()),
            HoldsState::Unavailable(reason) => Err(holds::unavailable(reason)),
        }
    }

    /// The commits all the holds name together.
    pub fn held_commits(&self) -> Result<usize> {
        match &*self.family.holds.state.lock() {
            HoldsState::Ready(r) => Ok(HoldsState::commits(r)),
            HoldsState::Unavailable(reason) => Err(holds::unavailable(reason)),
        }
    }

    /// Keep `commits` readable for `owner` under the hold `id`, through the dataset's
    /// main store. Each commit names `main` or a listed branch by its id. A commit at or
    /// below a branch's starting point is kept in its upstream's history.
    ///
    /// The hold is durable in `holds.json` when this returns, and from then on no
    /// trimming, compaction or generation retirement removes the commits' states, and no
    /// branch the hold names is deleted unless the deletion releases its holds. The first
    /// hold raises the dataset reader to 7 durably. Acquiring the same `id` for the same
    /// owner and commits again returns the existing hold. A different owner or set of
    /// commits is a `hold-conflict`, and more than [`holds::MAX_COMMITS`] held commits
    /// in all is `holds-limit`. A commit that is no longer readable is `410
    /// history-gone`, and a branch that is not listed is `no-such-branch`. A failed
    /// acquisition leaves the holds as they were.
    pub fn acquire_hold(
        &self,
        owner: OwnerKind,
        id: uuid::Uuid,
        commits: &[CommitRef],
    ) -> Result<Acquired> {
        self.holds_main()?;
        if commits.is_empty() {
            return Err(Error::invalid("a source hold names at least one commit"));
        }
        if commits.len() > holds::MAX_COMMITS {
            return Err(holds::limit(0, commits.len()));
        }
        let mut commits = commits.to_vec();
        commits.sort_unstable();
        commits.dedup();
        let _family = self.shared_family(&Default::default())?;
        let set = self.branching.set();
        // 1. the branch store that serves each commit
        let stores = match &set {
            Some(set) => set.hold_stores(&commits)?,
            None => {
                if let Some(c) = commits.iter().find(|c| c.branch_id != self.dataset_id) {
                    return Err(crate::branch::no_such_branch(&c.branch_id.to_string()));
                }
                vec![self.dataset_id; commits.len()]
            }
        };
        // 2. those stores, opened before any lock below is taken
        let mut members: BTreeMap<uuid::Uuid, Option<Arc<Store>>> = BTreeMap::new();
        for s in &stores {
            if *s != self.dataset_id && !members.contains_key(s) {
                let set = set.as_ref().expect("a branch store has a set");
                members.insert(*s, Some(set.open_store(*s)?));
            }
        }
        // 3. under the holds' lock, which a branch deletion takes before its commit
        // point, the named branches must still be listed
        let mut state = self.family.holds.state.lock();
        if let Some(set) = &set {
            drop(set.lock_listed(commits.iter().map(|c| c.branch_id))?);
        }
        let records = match &mut *state {
            HoldsState::Ready(r) => r,
            HoldsState::Unavailable(reason) => return Err(holds::unavailable(reason)),
        };
        let touch = self.family.holds.touches.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(existing) = records.get_mut(&id) {
            if existing.hold.owner == owner && existing.hold.commits == commits {
                // a reconciliation that looked before this no longer releases it
                existing.touch = touch;
                existing.acquired_ms = existing.acquired_ms.max(self.now_ms());
                let hold = existing.hold.clone();
                // the hold is durable before it is acknowledged again
                if self.family.holds.uncertain.load(Ordering::Relaxed) {
                    self.persist_holds(records)?;
                }
                return Ok(Acquired {
                    hold,
                    created: false,
                });
            }
            return Err(holds::conflict(format!(
                "source hold {id} already holds other commits"
            )));
        }
        let held = HoldsState::commits(records);
        if held + commits.len() > holds::MAX_COMMITS {
            return Err(holds::limit(held, commits.len()));
        }
        let mut next = records.clone();
        let now = self.now_ms();
        let hold = SourceHold {
            id,
            owner,
            created_ms: now,
            commits: commits.clone(),
        };
        next.insert(
            id,
            Record {
                hold: hold.clone(),
                stores: stores.clone(),
                touch,
                acquired_ms: now,
            },
        );
        // 4. the protection first: a collection that decides from here keeps them
        let mut undo = Unpublished {
            store: self,
            previous: Some(self.family.holds.index.load_full()),
            touched: members.keys().copied().chain([self.dataset_id]).collect(),
        };
        self.family
            .holds
            .index
            .store(Arc::new(HeldIndex::of(next.values())));
        self.failpoint("holds-protected");
        // 5. then the commits must still be readable
        for (c, s) in commits.iter().zip(&stores) {
            match members.get(s) {
                Some(Some(m)) => m.hold_readable(c.seq)?,
                _ => self.hold_readable(c.seq)?,
            }
        }
        self.failpoint("holds-verified");
        // 6. the commit point: `holds.json` replaced. From the rename on, the hold stays
        // installed even if the rename reports an error or syncing the directory fails,
        // so that the running store never protects less than the file may record. A
        // failed rename leaves the file uncertain, so the next change rewrites it, and
        // an unpublished hold is an orphan for reconciliation.
        let root = self.managed_root();
        if let Some(root) = &root {
            crate::commit::require_reader(root, holds::READER)?;
            self.failpoint("holds-reader-raised");
            let path = root.join(holds::FILE);
            let tmp = path.with_extension("tmp");
            super::write_synced(&tmp, &holds::encode(self.owner_dataset_id(), &next))?;
            if let Err(e) = std::fs::rename(&tmp, &path) {
                // the file may or may not have been replaced: keep protecting the hold
                self.family.holds.uncertain.store(true, Ordering::Relaxed);
                undo.previous = None;
                *state = HoldsState::Ready(next);
                return Err(e.into());
            }
        }
        undo.previous = None;
        *state = HoldsState::Ready(next);
        if let Some(root) = &root {
            self.family.holds.uncertain.store(true, Ordering::Relaxed);
            self.failpoint("holds-renamed");
            super::sync_dir(root)?;
            self.family.holds.uncertain.store(false, Ordering::Relaxed);
            self.failpoint("holds-published");
        }
        Ok(Acquired {
            hold,
            created: true,
        })
    }

    /// Release the holds `ids` of `owner` that exist, and return their ids.
    /// `holds.json` is durable without them before the commits they held may be
    /// collected. Unknown ids and holds of other owners are ignored, so a retried
    /// release succeeds.
    pub fn release_holds(&self, owner: OwnerKind, ids: &[uuid::Uuid]) -> Result<Vec<uuid::Uuid>> {
        self.holds_main()?;
        let _family = self.shared_family(&Default::default())?;
        self.release_where(|r| r.hold.owner == owner && ids.contains(&r.hold.id))
    }

    /// Release the holds of `owner` last acquired at least `grace_ms` milliseconds ago that
    /// `keep` does not keep, as after an operation that acquired them never published
    /// the commit that refers to them. `keep` is asked outside every lock, and a hold
    /// acquired again in the meantime, even with the same commits, is not released.
    /// Returns the released ids.
    pub fn reconcile_holds(
        &self,
        owner: OwnerKind,
        grace_ms: i64,
        keep: &mut dyn FnMut(&SourceHold) -> Result<bool>,
    ) -> Result<Vec<uuid::Uuid>> {
        self.holds_main()?;
        let cutoff = self.now_ms().saturating_sub(grace_ms.max(0));
        let candidates: Vec<(SourceHold, u64)> = match &*self.family.holds.state.lock() {
            HoldsState::Ready(r) => r
                .values()
                .filter(|r| r.hold.owner == owner && r.acquired_ms <= cutoff)
                .map(|r| (r.hold.clone(), r.touch))
                .collect(),
            HoldsState::Unavailable(reason) => return Err(holds::unavailable(reason)),
        };
        let mut orphans: Vec<(uuid::Uuid, u64)> = Vec::new();
        for (h, touch) in &candidates {
            if !keep(h)? {
                orphans.push((h.id, *touch));
            }
        }
        if orphans.is_empty() {
            return Ok(Vec::new());
        }
        let _family = self.shared_family(&Default::default())?;
        self.release_where(|r| {
            orphans
                .iter()
                .any(|(id, touch)| r.hold.id == *id && r.touch == *touch)
        })
    }

    /// Release the holds `which` selects.
    fn release_where(&self, which: impl Fn(&Record) -> bool) -> Result<Vec<uuid::Uuid>> {
        let mut state = self.family.holds.state.lock();
        let records = match &*state {
            HoldsState::Ready(r) => r,
            HoldsState::Unavailable(reason) => return Err(holds::unavailable(reason)),
        };
        let gone: Vec<&Record> = records.values().filter(|r| which(r)).collect();
        if gone.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<uuid::Uuid> = gone.iter().map(|r| r.hold.id).collect();
        let touched = Self::serving(&gone);
        let mut next = records.clone();
        for id in &ids {
            next.remove(id);
        }
        self.persist_holds(&next)?;
        self.failpoint("holds-release-published");
        self.install_holds(&mut *state, next, &touched);
        drop(state);
        self.collect_members(&touched);
        Ok(ids)
    }

    /// The stores that serve the commits of `records`.
    pub(super) fn serving(records: &[&Record]) -> Vec<uuid::Uuid> {
        let mut out: Vec<uuid::Uuid> = records.iter().flat_map(|r| r.stores.clone()).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Install `next` as the holds, under their lock `state`, and drop the held
    /// in-memory states of stores `touched` that nothing holds any more.
    pub(super) fn install_holds(
        &self,
        state: &mut HoldsState,
        next: BTreeMap<uuid::Uuid, Record>,
        touched: &[uuid::Uuid],
    ) {
        self.family
            .holds
            .index
            .store(Arc::new(HeldIndex::of(next.values())));
        *state = HoldsState::Ready(next);
        self.settle_members(touched);
    }

    /// Write `holds.json` for `records` durably (nothing for an in-memory store), under
    /// the holds' lock. A failure leaves the file uncertain until a later write succeeds.
    pub(super) fn persist_holds(&self, records: &BTreeMap<uuid::Uuid, Record>) -> Result<()> {
        let Some(root) = self.managed_root() else {
            return Ok(());
        };
        let written = crate::commit::require_reader(&root, holds::READER).and_then(|()| {
            write_atomic(
                &root.join(holds::FILE),
                &holds::encode(self.owner_dataset_id(), records),
            )
        });
        self.family
            .holds
            .uncertain
            .store(written.is_err(), Ordering::Relaxed);
        written
    }

    /// Fail with `410 history-gone` unless commit `seq` of this store is readable now,
    /// with the protection of the holds already published. An in-memory store takes the
    /// state into its held states.
    fn hold_readable(&self, seq: u64) -> Result<()> {
        if let Some(hist) = &self.history {
            let h = hist.lock();
            let snap = self.snapshot();
            let current = crate::commit::generation_number(&snap.generation.name);
            let head = snap.commit;
            let readable = seq <= head
                && h.reconstructable(current, head)
                    .iter()
                    .any(|(a, b)| *a <= seq && seq <= *b);
            if !readable {
                let metadata = self.catalog.lock().get(seq);
                return Err(self.history_gone(&h, seq, head, None, metadata));
            }
            return Ok(());
        }
        if let Some(mem) = &self.mem_history {
            let live = self.snapshot();
            let mut m = mem.lock();
            if m.held.contains_key(&seq) {
                return Ok(());
            }
            let state = if live.commit == seq {
                Some(Arc::new(super::mem_history::past(&live)))
            } else {
                m.get(seq)
            };
            let metadata = m.kept_commit(seq).or_else(|| self.catalog.lock().get(seq));
            match (state, metadata) {
                (Some(state), Some(commit)) => {
                    m.held.insert(seq, (commit, state));
                }
                (_, metadata) => {
                    drop(m);
                    return Err(self.mem_gone(seq, live.commit, None, metadata));
                }
            }
        }
        Ok(())
    }

    /// Drop the held in-memory states of stores `ids` that the published index no
    /// longer holds. Called under the holds' lock, so no acquisition runs meanwhile.
    fn settle_members(&self, ids: &[uuid::Uuid]) {
        let index = self.family.holds.index.load_full();
        self.each_member(ids, |m| {
            if let Some(mem) = &m.mem_history {
                mem.lock()
                    .held
                    .retain(|seq, _| index.holds(m.dataset_id, *seq));
            }
        });
    }

    /// Collect the generations of stores `ids` that nothing needs any more, when their
    /// writers are free now (the next collection point does it otherwise).
    fn collect_members(&self, ids: &[uuid::Uuid]) {
        self.each_member(ids, |m| m.try_collect_history());
    }

    /// Run `f` on each of the stores `ids` that is open: this one or a branch store.
    fn each_member(&self, ids: &[uuid::Uuid], f: impl Fn(&Store)) {
        let set = self.branching.set();
        for id in ids {
            if *id == self.dataset_id {
                f(self);
            } else if let Some(m) = set.as_ref().and_then(|s| s.stores.lock().get(id).cloned()) {
                f(&m);
            }
        }
    }
}
