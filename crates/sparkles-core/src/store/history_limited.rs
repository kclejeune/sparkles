//! Request-scoped historical view preparation. No write or authority is granted here.
use super::*;
use crate::history::{At, HistoryOptions, ReadLimit, Resolved};

impl Store {
    pub(super) fn resolve_limited(&self, at: &At, limit: &ReadLimit<'_>) -> Result<Resolved> {
        if let At::Snapshot(name) = at
            && !crate::history::valid_name(name)
        {
            return Err(Error::invalid("invalid snapshot name"));
        }
        // Capture the same writer head as resolve(), but do not wait without control.
        let head = limit.lock(&self.writer)?.head;
        let seq = match at {
            At::Head => head.seq,
            At::Commit(n) => *n,
            At::Time(ms) => {
                let catalog = limit.lock(&self.catalog)?;
                match catalog.at_time(*ms) {
                    Some(commit) => commit.seq,
                    None if self.branching.ident.is_some() => {
                        drop(catalog);
                        return Err(self.inherited(at, 0));
                    }
                    None => {
                        return Err(Error::NotFound(format!(
                            "no commit at or before {}",
                            commit::rfc3339_ms(*ms)
                        )));
                    }
                }
            }
            At::Snapshot(name) => {
                let found = if let Some(history) = &self.history {
                    limit.lock(history)?.pins.get(name).map(|p| p.seq)
                } else {
                    None
                };
                let found = match (found, &self.mem_history) {
                    (None, Some(history)) => limit.lock(history)?.pins.get(name).map(|p| p.0.seq),
                    (found, _) => found,
                };
                found.ok_or_else(|| Error::NotFound(format!("no snapshot '{name}'")))?
            }
        };
        if seq > head.seq {
            return Err(Error::NotFound(format!(
                "no commit {seq} (head is {})",
                head.seq
            )));
        }
        if let Some(identity) = &self.branching.ident {
            let before = match at {
                At::Time(ms) => limit
                    .lock(&self.catalog)?
                    .get(identity.from.seq)
                    .is_some_and(|c| c.timestamp_ms > *ms),
                _ => seq < identity.from.seq,
            };
            if before {
                return Err(self.inherited(at, seq));
            }
        }
        let metadata = limit.lock(&self.catalog)?.get(seq);
        let metadata = match (metadata, &self.mem_history) {
            (None, Some(history)) => limit.lock(history)?.branch_bases.get(&seq).map(|(c, _)| *c),
            (metadata, _) => metadata,
        };
        let Some(commit) = metadata else {
            let name = match at {
                At::Snapshot(name) => Some(name.clone()),
                _ => None,
            };
            return Err(match &self.history {
                Some(history) => self.history_gone_limited(
                    &*limit.lock(history)?,
                    seq,
                    head.seq,
                    name,
                    None,
                    limit,
                )?,
                None => self.mem_gone_limited(seq, head.seq, name, None, limit)?,
            });
        };
        limit.check(0)?;
        Ok(Resolved {
            at: at.clone(),
            commit,
            head: head.seq,
            historical: seq != self.snapshot().commit,
        })
    }

    pub(super) fn history_generation_limited(
        &self,
        history: &mut crate::history::HistoryState,
        owner: u32,
        current: u32,
        live: &Snapshot,
        limit: &ReadLimit<'_>,
    ) -> Result<Arc<Generation>> {
        limit.check(0)?;
        if owner == current {
            return Ok(live.generation.clone());
        }
        if let Some(index) = history.open.iter().position(|(number, _)| *number == owner) {
            let entry = history.open.remove(index);
            let generation = entry.1.clone();
            history.open.insert(0, entry);
            limit.check(0)?;
            return Ok(generation);
        }
        Err(Error::HistoryUnsupported(
            "bounded reads require a retained generation; cold opening is not controlled".into(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn history_gone_limited(
        &self,
        history: &crate::history::HistoryState,
        seq: u64,
        head: u64,
        snapshot: Option<String>,
        metadata: Option<CommitInfo>,
        limit: &ReadLimit<'_>,
    ) -> Result<Error> {
        let current = commit::generation_number(&self.snapshot().generation.name);
        let reconstructable = bounded_ranges(
            history.gens.iter().map(|(&number, generation)| {
                (
                    generation.base.seq,
                    if number == current {
                        head
                    } else {
                        generation.end
                    },
                )
            }),
            limit,
        )?;
        let oldest = reconstructable.first().map_or(String::new(), |r| {
            format!("; the oldest reconstructable commit is {}", r.0)
        });
        limit.check(512)?;
        Ok(Error::HistoryGone(Box::new(crate::history::HistoryGone {
            message: format!("commit {seq} is no longer reconstructable{oldest}"),
            seq,
            head,
            snapshot,
            reconstructable,
            metadata,
        })))
    }

    pub(super) fn mem_gone_limited(
        &self,
        seq: u64,
        head: u64,
        snapshot: Option<String>,
        metadata: Option<CommitInfo>,
        limit: &ReadLimit<'_>,
    ) -> Result<Error> {
        let reconstructable = match &self.mem_history {
            Some(history) => {
                let history = limit.lock(history)?;
                bounded_ranges(
                    history
                        .window
                        .iter()
                        .map(|s| s.commit)
                        .chain(history.pins.values().map(|p| p.1.commit))
                        .chain(history.branch_bases.keys().copied())
                        .chain(std::iter::once(head))
                        .map(|seq| (seq, seq)),
                    limit,
                )?
            }
            None => bounded_ranges(std::iter::once((head, head)), limit)?,
        };
        limit.check(512)?;
        Ok(Error::HistoryGone(Box::new(crate::history::HistoryGone {
            message: format!(
                "commit {seq} is no longer reconstructable; an in-memory dataset keeps the states its snapshots and retention window hold"
            ),
            seq,
            head,
            snapshot,
            reconstructable,
            metadata,
        })))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn materialize_limited(
        &self,
        history: &crate::history::HistoryState,
        owner: u32,
        entry: &crate::history::GenEntry,
        generation: &Arc<Generation>,
        live: &Snapshot,
        seq: u64,
        limit: &ReadLimit<'_>,
    ) -> Result<(Delta, wal::WalPoint)> {
        let path = entry.dir.join("wal.log");
        let target = wal_point_limited(generation, entry, seq, limit)?;
        let base = wal::WalPoint {
            seq: entry.base.seq,
            offset: 0,
            folding: entry.fold_legacy,
        };
        let base_delta = generation.link.as_ref().map(|l| &l.base_delta);
        let mut best = (
            if base_delta.is_none_or(|d| preparation_delta_bytes(d) <= limit.remaining()) {
                target.offset
            } else {
                u64::MAX
            },
            base,
            base_delta,
        );
        for cached in history.cache.iter().filter(|c| c.generation == owner) {
            limit.check(0)?;
            let cost = cached.end.offset.abs_diff(target.offset);
            if cost < best.0 && preparation_delta_bytes(&cached.snap.delta) <= limit.remaining() {
                best = (cost, cached.end, Some(&cached.snap.delta));
            }
        }
        if Arc::ptr_eq(&live.generation, generation)
            && !live.historical
            && preparation_delta_bytes(&live.delta) <= limit.remaining()
        {
            let point = wal_point_limited(generation, entry, live.commit, limit)?;
            let cost = point.offset.abs_diff(target.offset);
            if cost < best.0 {
                best = (cost, point, Some(&live.delta));
            }
        }
        let (_, from, seed) = best;
        limit.check(seed.map_or(0, preparation_delta_bytes))?;
        let mut delta = seed.cloned().unwrap_or_default();
        if from.offset <= target.offset {
            wal::apply_forward_limited(
                &path,
                generation,
                &self.cache,
                &mut delta,
                from,
                target,
                limit,
            )?;
            return Ok((delta, target));
        }
        match wal::apply_backward_limited(
            &path,
            generation,
            &self.cache,
            &mut delta,
            target,
            from,
            limit,
        ) {
            Ok(_) => Ok((delta, target)),
            Err(wal::Backward::Failed(error)) => Err(error),
            Err(wal::Backward::Legacy) => {
                // Release the failed backward working state before preparing its replacement.
                drop(delta);
                limit.check(base_delta.map_or(0, preparation_delta_bytes))?;
                let mut delta = base_delta.cloned().unwrap_or_default();
                wal::apply_forward_limited(
                    &path,
                    generation,
                    &self.cache,
                    &mut delta,
                    base,
                    target,
                    limit,
                )?;
                Ok((delta, target))
            }
        }
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
            held: 0,
        };
        limit.check(0)?;
        if self.owner_dataset_id() != store.owner_dataset_id() {
            return Err(Error::invalid(
                "the pinned branch belongs to a different dataset family",
            ));
        }
        self.snapshot_pinned_limited(store, at, options, max_memory_bytes)
    }

    fn snapshot_pinned_limited(
        &self,
        store: &Store,
        at: &At,
        options: &HistoryOptions,
        memory: u64,
    ) -> Result<(Arc<Snapshot>, Resolved, uuid::Uuid)> {
        match store.snapshot_at_limited(at, options, memory) {
            Ok((snapshot, resolved)) => Ok((snapshot, resolved, store.branch_id())),
            Err(error) => match crate::branch::inherited_commit(&error) {
                Some(commit) => {
                    // Follow only the branch's recorded origin. Never search by sequence.
                    let limit = ReadLimit {
                        options,
                        memory: Some(memory),
                        held: 0,
                    };
                    let owner = self.owned_set()?.normalize_limited(commit, &limit)?;
                    let upstream = self.retained_branch_by_id_limited(owner.branch_id, &limit)?;
                    match at {
                        At::Time(_) => self.snapshot_pinned_limited(&upstream, at, options, memory),
                        _ => self.snapshot_pinned_limited(
                            &upstream,
                            &At::Commit(commit.seq),
                            options,
                            memory,
                        ),
                    }
                }
                None => Err(error),
            },
        }
    }
}

fn wal_point_limited(
    generation: &Generation,
    entry: &crate::history::GenEntry,
    seq: u64,
    limit: &ReadLimit<'_>,
) -> Result<wal::WalPoint> {
    let path = entry.dir.join("wal.log");
    // Existing sparse indexes are shared store state. An unindexed sealed generation
    // is scanned without building an unbounded index as a side effect of this read.
    let floor = limit
        .lock(&generation.wal_index)?
        .as_ref()
        .map(|index| index.floor(seq))
        .unwrap_or(wal::WalPoint {
            seq: entry.base.seq,
            offset: 0,
            folding: entry.fold_legacy,
        });
    limit.check(0)?;
    if floor.seq == seq && !floor.folding {
        return Ok(floor);
    }
    let mut cursor = wal::WalCursor::open_limited(&path, floor, limit, 0)?;
    loop {
        let before = cursor.position();
        if before.seq == seq && !before.folding {
            limit.check(0)?;
            return Ok(before);
        }
        if before.seq > seq {
            return Err(Error::Corrupt(format!(
                "{}: read past commit {seq}",
                path.display()
            )));
        }
        match cursor.next_limited(limit, 0)? {
            Some((found, _, _)) if found > seq => return Ok(before),
            Some(_) => {}
            None if before.seq == seq => return Ok(before),
            None => {
                return Err(Error::Corrupt(format!(
                    "{}: the log ends before commit {seq}",
                    path.display()
                )));
            }
        }
    }
}

// Error metadata is preparation too. Build and merge ranges in one vector with
// preflighted relocation and interruptible in-place ordering.
fn bounded_ranges(
    ranges: impl Iterator<Item = (u64, u64)>,
    limit: &ReadLimit<'_>,
) -> Result<Vec<(u64, u64)>> {
    let mut result: Vec<(u64, u64)> = Vec::new();
    for range in ranges {
        limit.check(
            (result.capacity() as u64)
                .saturating_mul(16)
                .saturating_add(1024),
        )?;
        if result.len() == result.capacity() {
            let next = result.capacity().saturating_mul(2).max(1);
            limit.check(
                (result.capacity().saturating_add(next) as u64)
                    .saturating_mul(16)
                    .saturating_add(1024),
            )?;
            result.try_reserve_exact(next - result.len()).map_err(|_| {
                let memory = limit.memory.unwrap_or(u64::MAX);
                Error::BudgetExceeded(crate::Budget {
                    kind: crate::BudgetKind::Memory,
                    limit: memory,
                    requested: memory.saturating_add(1),
                })
            })?;
        }
        result.push(range);
    }
    let sift = |values: &mut [(u64, u64)], mut root: usize, end: usize| -> Result<()> {
        loop {
            limit.check(
                (values.len() as u64)
                    .saturating_mul(16)
                    .saturating_add(1024),
            )?;
            let left = root.saturating_mul(2).saturating_add(1);
            if left >= end {
                return Ok(());
            }
            let child = if left + 1 < end && values[left] < values[left + 1] {
                left + 1
            } else {
                left
            };
            if values[root] >= values[child] {
                return Ok(());
            }
            values.swap(root, child);
            root = child;
        }
    };
    let len = result.len();
    for root in (0..len / 2).rev() {
        sift(&mut result, root, len)?;
    }
    for end in (1..len).rev() {
        result.swap(0, end);
        sift(&mut result, 0, end)?;
    }
    let mut kept = 0;
    for index in 0..len {
        limit.check(
            (result.capacity() as u64)
                .saturating_mul(16)
                .saturating_add(1024),
        )?;
        let (begin, end) = result[index];
        if kept > 0 && begin <= result[kept - 1].1.saturating_add(1) {
            result[kept - 1].1 = result[kept - 1].1.max(end);
        } else {
            result[kept] = (begin, end);
            kept += 1;
        }
    }
    result.truncate(kept);
    Ok(result)
}

// The cache's 448-byte/quad estimate is not an allocation bound: even a one-key
// KeySet leaf reserves 65 keys. Bounded reads include sparse root capacity and the
// predicate map as well. Leaves rebalance below 16 keys and branches below eight
// children; 256 bytes per key plus 8 KiB per tree conservatively covers retained
// node/vector capacity, Arc headers, and the per-predicate persistent map.
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
            bounded_ranges(ranges.into_iter(), &limit).unwrap(),
            expected
        );
        let low = ReadLimit {
            memory: Some(1024),
            ..limit
        };
        assert!(matches!(
            bounded_ranges((0..10_000).map(|n| (n * 2, n * 2)), &low),
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
        let task = std::thread::spawn(move || {
            waiting.wait();
            read.snapshot_at_limited(&At::Head, &options, 1 << 20)
        });
        started.wait();
        std::thread::sleep(Duration::from_millis(30));
        cancel.store(true, Ordering::Relaxed);
        let result = task.join().unwrap();
        drop(held);
        assert!(matches!(result, Err(Error::Cancelled)));
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
        let task = std::thread::spawn(move || {
            read.snapshot_at_limited(
                &At::Commit(1),
                &HistoryOptions {
                    cancel: None,
                    deadline: Some(Instant::now() + Duration::from_millis(40)),
                },
                1 << 20,
            )
        });
        let result = task.join().unwrap();
        drop(held);
        assert!(matches!(result, Err(Error::Timeout)));
    }
}
