//! Request-scoped historical view preparation. No write or authority is granted here.
use super::*;
use crate::history::{At, HistoryOptions, ReadControl, ReadLimit, Resolved};

/// Where a pinned read starts: an open store, or a branch read without opening it.
enum PinnedSource<'a> {
    Resident(branching::BranchStore<'a>),
    Cold(uuid::Uuid),
}

impl Store {
    /// An unresolved time has no honest commit number after catalog pruning.
    pub(super) fn unresolved_time_limited(&self, ms: i64, limit: &ReadLimit<'_>) -> Result<Error> {
        let writer = limit.lock(&self.writer)?;
        let head = writer.head.seq;
        let current = commit::generation_number(&self.snapshot().generation.name);
        let reconstructable = if let Some(history) = &self.history {
            limit
                .lock(history)?
                .reconstructable_with(current, head, limit)?
        } else if let Some(history) = &self.mem_history {
            limit.lock(history)?.reconstructable_with(head, limit)?
        } else {
            limit.check(16)?;
            vec![(head, head)]
        };
        limit.check(512)?;
        Ok(Error::HistoryGone(Box::new(crate::history::HistoryGone {
            message: format!(
                "{} cannot be resolved because its branch commit metadata is no longer retained",
                At::Time(ms)
            ),
            seq: None,
            at: Some(At::Time(ms)),
            head,
            snapshot: None,
            reconstructable,
            metadata: None,
        })))
    }

    /// Prove only the immutable fork clock. This never opens a parent Store or
    /// materializes its head just to decide whether time inheritance is legitimate.
    pub(super) fn origin_timestamp_limited(
        &self,
        mut commit: crate::branch::CommitRef,
        limit: &ReadLimit<'_>,
    ) -> Result<i64> {
        use branching::readonly::{OriginRead, Purpose};
        for _ in 0..1024 {
            limit.check(0)?;
            commit = self.owned_set()?.normalize_with(commit, limit)?;
            let result = if commit.branch_id == self.dataset_id {
                self.resolve_under(&At::Commit(commit.seq), limit)
                    .map(|resolved| OriginRead::Clock(resolved.commit.timestamp_ms))
            } else {
                match self.readonly_branch_read_limited(
                    commit.branch_id,
                    &At::Commit(commit.seq),
                    Purpose::Clock,
                    limit,
                )? {
                    OriginRead::Resident(store) => store
                        .resolve_under(&At::Commit(commit.seq), limit)
                        .map(|resolved| OriginRead::Clock(resolved.commit.timestamp_ms)),
                    value => Ok(value),
                }
            };
            match result {
                Ok(OriginRead::Clock(timestamp)) => {
                    limit.check(0)?;
                    return Ok(timestamp);
                }
                Ok(OriginRead::Inherited(parent)) => commit = parent,
                Err(error) => {
                    let Some(parent) = crate::branch::inherited_commit(&error) else {
                        return Err(error);
                    };
                    commit = parent;
                }
                _ => {
                    return Err(Error::Corrupt(
                        "fork clock lookup returned a data view".into(),
                    ));
                }
            }
        }
        Err(Error::invalid("fork clock has no acyclic retained origin"))
    }

    /// [`resolve`](Store::resolve) under `limit`. Under an accounting control, refuses an
    /// invalid snapshot name and takes the writer's head under its control.
    pub(super) fn resolve_under<C: crate::budgeted::Control>(
        &self,
        at: &At,
        limit: &C,
    ) -> Result<Resolved> {
        if !C::ACCOUNTS {
            return self.resolve(at);
        }
        if let At::Snapshot(name) = at
            && !crate::history::valid_name(name)
        {
            return Err(Error::invalid("invalid snapshot name"));
        }
        // Capture the same writer head as resolve(), but do not wait without control.
        let head = limit.lock(&self.writer)?.head;
        self.resolve_in(at, head, limit)
    }

    pub(super) fn cache_limited_snapshot(
        history: &mut crate::history::HistoryState,
        cached: crate::history::Cached,
        limit: &ReadLimit<'_>,
    ) -> Result<()> {
        let retained = preparation_delta_bytes(&cached.snap.delta);
        let mut before = 0u64;
        for entry in &history.cache {
            limit.check(retained)?;
            before = before.saturating_add(entry.bytes);
        }
        if history.cache.len() == history.cache.capacity() {
            let next = history.cache.capacity().saturating_mul(2).max(1);
            let relocating = (history.cache.capacity().saturating_add(next) as u64)
                .saturating_mul(std::mem::size_of::<crate::history::Cached>() as u64);
            // Caching is optional. A request need not allocate a large shared cache
            // vector or evict unrelated entries merely to return its completed view.
            if retained.saturating_add(relocating) > limit.remaining() {
                limit.check(retained)?;
                return Ok(());
            }
            if history
                .cache
                .try_reserve_exact(next - history.cache.len())
                .is_err()
            {
                limit.check(retained)?;
                return Ok(());
            }
        }
        limit.check(retained)?;
        let after = before.saturating_add(cached.bytes);
        history.cache.insert(0, cached);
        history.settle_precounted(after);
        if history.budget.over() {
            // Only the optional new entry is removed. Existing cache capacity,
            // states and warm pins are not narrowed by this request.
            history.cache.remove(0);
            history.settle_precounted(before);
        }
        Ok(())
    }

    /// Origin-aware bounded counterpart of snapshot_at_with_branch.
    pub fn snapshot_at_with_branch_limited(
        &self,
        store: &Store,
        at: &At,
        options: &HistoryOptions,
        max_memory_bytes: u64,
    ) -> Result<(Arc<Snapshot>, Resolved, uuid::Uuid)> {
        let limit = ReadLimit {
            options,
            memory: Some(max_memory_bytes),
            held: crate::history::READ_METADATA_BYTES,
        };
        limit.check(0)?;
        if self.owner_dataset_id() != store.owner_dataset_id() {
            return Err(Error::invalid(
                "the pinned branch belongs to a different dataset family",
            ));
        }
        self.snapshot_pinned_limited(store, at, options, max_memory_bytes)
    }

    pub(super) fn snapshot_at_prepared(
        &self,
        logical: &Store,
        at: &At,
        preparation: &crate::access::Preparation,
        memory: u64,
    ) -> Result<(Arc<Snapshot>, Resolved, uuid::Uuid)> {
        if self.is_branch()
            || self.owner_dataset_id() != logical.owner_dataset_id()
            || !Arc::ptr_eq(&self.family, &logical.family)
        {
            return Err(Error::invalid(
                "physical selection requires the actual owning main Store",
            ));
        }
        self.snapshot_pinned_limited(logical, at, preparation, memory)
    }

    /// The state of the branch whose identity is `branch` at `at`, within
    /// `max_memory_bytes`, through this dataset's main store. `branch` is the dataset
    /// id for `main` or the id of a listed branch, so a renamed branch keeps resolving
    /// and a recreated name never does. A cold branch is read without opening its
    /// store. A commit at or below the branch's starting point, or its starting commit
    /// after the branch dropped its own copy, is read from the upstream that serves it,
    /// and the returned id names that serving branch. The read follows the same exact
    /// selector, origin and budget rules as
    /// [`snapshot_at_with_branch_limited`](Self::snapshot_at_with_branch_limited).
    /// A deleted, retired or unknown branch is `no-such-branch`. This grants no access:
    /// callers apply their caller's graph policy to the returned physical snapshot.
    pub fn snapshot_of_branch_limited(
        &self,
        branch: uuid::Uuid,
        at: &At,
        options: &HistoryOptions,
        max_memory_bytes: u64,
    ) -> Result<(Arc<Snapshot>, Resolved, uuid::Uuid)> {
        let limit = ReadLimit {
            options,
            memory: Some(max_memory_bytes),
            held: crate::history::READ_METADATA_BYTES,
        };
        limit.check(0)?;
        if self.is_branch() {
            return Err(crate::branch::invalid_branch(
                "a branch is resolved through the dataset's main store",
            ));
        }
        if branch == self.dataset_id {
            return self.snapshot_from_limited(
                PinnedSource::Resident(branching::BranchStore::Main(self)),
                at,
                options,
                max_memory_bytes,
            );
        }
        self.listed_with(branch, &limit)?;
        let read =
            self.snapshot_from_limited(PinnedSource::Cold(branch), at, options, max_memory_bytes)?;
        // A branch deleted while it was read no longer names this state.
        self.listed_with(branch, &limit)?;
        Ok(read)
    }

    fn snapshot_pinned_limited(
        &self,
        store: &Store,
        at: &At,
        options: &dyn ReadControl,
        memory: u64,
    ) -> Result<(Arc<Snapshot>, Resolved, uuid::Uuid)> {
        self.snapshot_from_limited(
            PinnedSource::Resident(branching::BranchStore::Main(store)),
            at,
            options,
            memory,
        )
    }

    fn snapshot_from_limited(
        &self,
        start: PinnedSource<'_>,
        at: &At,
        options: &dyn ReadControl,
        memory: u64,
    ) -> Result<(Arc<Snapshot>, Resolved, uuid::Uuid)> {
        use PinnedSource as Source;
        use branching::readonly::OriginRead;
        let capacity = self
            .opts
            .shared
            .as_ref()
            .map_or(self.opts.history_cache_bytes, |scope| {
                scope.caches.history().capacity()
            });
        let memory = memory.min(capacity);
        let limit = ReadLimit {
            options,
            memory: Some(memory),
            held: crate::history::READ_METADATA_BYTES,
        };
        let mut selected = start;
        let mut inherited = None;
        let mut hops = 0usize;
        loop {
            limit.check(0)?;
            if hops >= 1024 {
                return Err(Error::invalid(
                    "inherited history has no acyclic retained origin",
                ));
            }
            let requested = inherited.as_ref().unwrap_or(at);
            let (id, result) = match &selected {
                Source::Resident(store) => (
                    store.branch_id(),
                    store
                        .snapshot_at_inner(requested, options, Some(memory))
                        .map(|(snapshot, resolved)| OriginRead::View(snapshot, resolved)),
                ),
                Source::Cold(id) => (
                    *id,
                    self.readonly_branch_snapshot_limited(*id, requested, &limit),
                ),
            };
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(error) => {
                    let Some(commit) = crate::branch::inherited_commit(&error) else {
                        return Err(error);
                    };
                    drop(error);
                    OriginRead::Inherited(commit)
                }
            };
            match outcome {
                OriginRead::View(snapshot, resolved) => {
                    limit.check(0)?;
                    return Ok((snapshot, resolved, id));
                }
                OriginRead::Resident(store) => {
                    selected = Source::Resident(branching::BranchStore::Other(store))
                }
                OriginRead::Clock(_) | OriginRead::Configuration { .. } => {
                    return Err(Error::Corrupt(
                        "snapshot lookup returned only a fork clock".into(),
                    ));
                }
                OriginRead::Inherited(commit) => {
                    if let At::Time(ms) = at {
                        let fork = self.origin_timestamp_limited(commit, &limit)?;
                        if *ms >= fork {
                            return match &selected {
                                Source::Resident(store) => {
                                    Err(store.unresolved_time_limited(*ms, &limit)?)
                                }
                                Source::Cold(id) => match self.readonly_branch_read_limited(
                                    *id,
                                    at,
                                    branching::readonly::Purpose::TimeGone(*ms),
                                    &limit,
                                ) {
                                    Err(error) => Err(error),
                                    Ok(OriginRead::Resident(store)) => {
                                        Err(store.unresolved_time_limited(*ms, &limit)?)
                                    }
                                    _ => Err(Error::Corrupt(
                                        "missing branch time produced a data view".into(),
                                    )),
                                },
                            };
                        }
                    }
                    hops += 1;
                    let owner = self.owned_set()?.normalize_with(commit, &limit)?;
                    selected = if owner.branch_id == self.dataset_id {
                        Source::Resident(branching::BranchStore::Main(self))
                    } else {
                        Source::Cold(owner.branch_id)
                    };
                    if !matches!(at, At::Time(_)) {
                        inherited = Some(At::Commit(owner.seq));
                    }
                }
            }
        }
    }
}

// The cache's 448-byte/quad estimate is not an allocation bound: even a one-key
// KeySet leaf reserves 65 keys. Bounded reads include sparse root capacity and the
// predicate map as well. Leaves rebalance below 16 keys and branches below eight
// children; 256 bytes per key plus 8 KiB per tree conservatively covers retained
// node/vector capacity, Arc headers, and the per-predicate persistent map.
impl Snapshot {
    /// The conservative preparation bytes a bounded read charges for this state's
    /// delta, which a caller that retains the snapshot keeps charged.
    pub(crate) fn retained_preparation_bytes(&self) -> u64 {
        preparation_delta_bytes(&self.delta)
    }
}

pub(super) fn preparation_delta_bytes(delta: &Delta) -> u64 {
    let quads = delta.inserts().saturating_add(delta.deletes()) as u64;
    if quads == 0 {
        return 0;
    }
    quads.saturating_mul(2048).saturating_add(128 << 10)
}

// Mutation may copy/split two paths in each of seven insertion/deletion trees.
// Count their transient storage while both the seed and working state remain live.
pub(super) fn preparation_mutation_bytes(delta: &Delta) -> u64 {
    let quads = (delta.inserts().saturating_add(delta.deletes()) as u64).saturating_add(1);
    preparation_delta_bytes(delta)
        .saturating_add(2048)
        .saturating_add((u64::from(quads.ilog2()) / 3 + 2).saturating_mul(64 << 10))
        .max(128 << 10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    #[test]
    fn inherited_name_read_wait_is_interruptible() {
        let root = tempfile::tempdir().unwrap();
        let owner = Arc::new(Store::open(root.path(), StoreOptions::default()).unwrap());
        crate::sparql::update::update(
            &owner,
            "INSERT DATA { <urn:a> <urn:p> 1 }",
            &Default::default(),
        )
        .unwrap();
        owner.create_branch("dev", &Default::default()).unwrap();
        let branch = owner
            .owned_set()
            .unwrap()
            .open_store(owner.branch_id_of("dev").unwrap())
            .unwrap();
        let held = branch.branching.name.write();
        let read = branch.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            send.send(read.snapshot_at_limited(
                &At::Commit(0),
                &HistoryOptions {
                    cancel: Some(signal),
                    deadline: None,
                },
                2 << 20,
            ))
            .unwrap();
        });
        std::thread::sleep(Duration::from_millis(30));
        cancel.store(true, Ordering::Relaxed);
        let result = receive.recv_timeout(Duration::from_secs(1));
        drop(held);
        task.join().unwrap();
        assert!(matches!(result, Ok(Err(Error::Cancelled))));
    }

    /// The same wait on a branch's starting commit whose own state was collected, which
    /// the read follows to the upstream.
    #[test]
    fn inherited_name_read_wait_at_a_collected_start_is_interruptible() {
        let root = tempfile::tempdir().unwrap();
        let owner = Arc::new(Store::open(root.path(), StoreOptions::default()).unwrap());
        let update = |s: &Store, n: u64| {
            crate::sparql::update::update(
                s,
                &format!("INSERT DATA {{ <urn:a> <urn:p> {n} }}"),
                &Default::default(),
            )
            .unwrap();
        };
        update(&owner, 1);
        owner.create_branch("dev", &Default::default()).unwrap();
        let start = owner.head_commit().seq;
        let branch = owner
            .owned_set()
            .unwrap()
            .open_store(owner.branch_id_of("dev").unwrap())
            .unwrap();
        for n in 0..3 {
            update(&branch, 10 + n);
            branch.compact().unwrap();
        }
        let o = HistoryOptions::default();
        assert!(matches!(
            branch.snapshot_at_limited(&At::Commit(start), &o, 2 << 20),
            Err(ref e) if crate::branch::inherited_commit(e).is_some()
        ));
        let held = branch.branching.name.write();
        let read = branch.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            send.send(read.snapshot_at_limited(
                &At::Commit(start),
                &HistoryOptions {
                    cancel: Some(signal),
                    deadline: None,
                },
                2 << 20,
            ))
            .unwrap();
        });
        std::thread::sleep(Duration::from_millis(30));
        cancel.store(true, Ordering::Relaxed);
        let result = receive.recv_timeout(Duration::from_secs(1));
        drop(held);
        task.join().unwrap();
        assert!(matches!(result, Ok(Err(Error::Cancelled))));
    }

    #[test]
    fn historical_vocabulary_length_wait_observes_deadline() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(root.path(), StoreOptions::default()).unwrap());
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:a> <urn:p> 1 }",
            &Default::default(),
        )
        .unwrap();
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:b> <urn:p> 2 }",
            &Default::default(),
        )
        .unwrap();
        let snapshot = store.snapshot();
        let held = snapshot.generation.dvocab.write_guard_for_test();
        let read = store.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            send.send(read.snapshot_at_limited(
                &At::Commit(1),
                &HistoryOptions {
                    cancel: None,
                    deadline: Some(Instant::now() + Duration::from_millis(40)),
                },
                2 << 20,
            ))
            .unwrap();
        });
        let result = receive.recv_timeout(Duration::from_secs(1));
        drop(held);
        task.join().unwrap();
        assert!(matches!(result, Ok(Err(Error::Timeout))));
    }

    #[test]
    fn controlled_ranges_agree_with_sorted_interval_union() {
        let options = HistoryOptions::default();
        let limit = ReadLimit {
            options: &options,
            memory: Some(4 << 20),
            held: 0,
        };
        let ranges: Vec<_> = (0..10_000)
            .map(|n| ((n * 7919) % 50_000, (n * 7919) % 50_000 + n % 7))
            .collect();
        let mut sorted = ranges.clone();
        sorted.sort_unstable();
        let mut expected: Vec<(u64, u64)> = Vec::new();
        for (begin, end) in sorted {
            if let Some(last) = expected.last_mut()
                && begin <= last.1.saturating_add(1)
            {
                last.1 = last.1.max(end);
            } else {
                expected.push((begin, end));
            }
        }
        assert_eq!(
            crate::history::merge_ranges(ranges.iter().copied(), &limit).unwrap(),
            expected
        );
        assert_eq!(
            crate::history::merge_ranges(ranges.into_iter(), &crate::budgeted::Unlimited).unwrap(),
            expected
        );
        // single commits with duplicates and neighbours, as an in-memory history keeps
        // them, merge as the deduplicated runs of consecutive commits
        let seqs: Vec<u64> = (0..1_000)
            .map(|n| (n * 31) % 1_500)
            .chain([0, 1, 31, 31, 32])
            .collect();
        let mut unique = seqs.clone();
        unique.sort_unstable();
        unique.dedup();
        let mut runs: Vec<(u64, u64)> = Vec::new();
        for s in unique {
            match runs.last_mut() {
                Some(last) if s == last.1 + 1 => last.1 = s,
                _ => runs.push((s, s)),
            }
        }
        for merged in [
            crate::history::merge_ranges(seqs.iter().map(|&s| (s, s)), &limit).unwrap(),
            crate::history::merge_ranges(seqs.iter().map(|&s| (s, s)), &crate::budgeted::Unlimited)
                .unwrap(),
        ] {
            assert_eq!(merged, runs);
        }
        let low = ReadLimit {
            memory: Some(1024),
            ..limit
        };
        assert!(matches!(
            crate::history::merge_ranges((0..10_000).map(|n| (n * 2, n * 2)), &low),
            Err(Error::BudgetExceeded(_))
        ));
    }

    #[test]
    fn selector_writer_wait_can_be_cancelled() {
        let store = Arc::new(Store::in_memory(StoreOptions::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let options = HistoryOptions {
            cancel: Some(cancel.clone()),
            deadline: None,
        };
        let held = store.writer.lock();
        let read = store.clone();
        let started = Arc::new(std::sync::Barrier::new(2));
        let waiting = started.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            waiting.wait();
            send.send(read.snapshot_at_limited(&At::Head, &options, 1 << 20))
                .unwrap();
        });
        started.wait();
        std::thread::sleep(Duration::from_millis(30));
        cancel.store(true, Ordering::Relaxed);
        let result = receive.recv_timeout(Duration::from_secs(1));
        drop(held);
        task.join().unwrap();
        assert!(matches!(result, Ok(Err(Error::Cancelled))));
    }

    #[test]
    fn history_lock_wait_observes_deadline() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(root.path(), StoreOptions::default()).unwrap());
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:a> <urn:p> 1 }",
            &Default::default(),
        )
        .unwrap();
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:b> <urn:p> 2 }",
            &Default::default(),
        )
        .unwrap();
        let held = store.history.as_ref().unwrap().lock();
        let read = store.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            send.send(read.snapshot_at_limited(
                &At::Commit(1),
                &HistoryOptions {
                    cancel: None,
                    deadline: Some(Instant::now() + Duration::from_millis(40)),
                },
                1 << 20,
            ))
            .unwrap();
        });
        let result = receive.recv_timeout(Duration::from_secs(1));
        drop(held);
        task.join().unwrap();
        assert!(matches!(result, Ok(Err(Error::Timeout))));
    }
}
