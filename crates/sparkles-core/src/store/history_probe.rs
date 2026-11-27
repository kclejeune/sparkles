//! Bounded raw history inspection for the server's private GraphRAG refresh check.

use super::*;
use crate::history::{HistoryOptions, ReadLimit};
use std::cell::Cell;
use std::time::Instant;

/// A graph to watch for raw changes, optionally limited to an IRI predicate and object.
pub struct HistoryPattern<'a> {
    pub graph: &'a str,
    pub predicate_object: Option<(&'a str, &'a str)>,
}

const MAX_PATTERNS: usize = 1024;
const MAX_COMMITS: u64 = 1000;
const MEMORY_BYTES: u64 = 1 << 20;

impl Store {
    /// An internal hint for a private input check, never a caller-visible change feed.
    /// `Some(false)` requires a complete negative probe. Missing history, a generation
    /// boundary or exhausted work gives `None` or a controlled read error. A WAL
    /// match may include a write that was reversed in its transaction.
    #[doc(hidden)]
    pub fn probe_raw_history<'a>(
        &self,
        after: u64,
        through: u64,
        patterns: impl IntoIterator<Item = HistoryPattern<'a>>,
        max_work: u64,
        deadline: Instant,
    ) -> Result<Option<bool>> {
        if after >= through {
            return Ok(Some(false));
        }
        let live = self.snapshot();
        if through > live.commit || through - after > MAX_COMMITS {
            return Ok(None);
        }
        let options = HistoryOptions {
            deadline: Some(deadline),
            ..Default::default()
        };
        let limit = ReadLimit {
            options: &options,
            memory: Some(MEMORY_BYTES),
            held: 0,
        };
        let mut budget = cold::ColdBudget::new(limit)?;
        let mut ids = Vec::new();
        for (n, pattern) in patterns.into_iter().enumerate() {
            if n == MAX_PATTERNS {
                return Ok(None);
            }
            let owned = budget.owned_limit()?;
            let lookup = |iri| {
                live.lookup_iri_controlled(iri, &mut |bytes| owned.check(bytes as u64), &mut || {
                    owned.check(0)
                })
            };
            let Some(graph) = lookup(pattern.graph)? else {
                continue;
            };
            let (p, o) = match pattern.predicate_object {
                Some((p, o)) => match (lookup(p)?, lookup(o)?) {
                    (Some(p), Some(o)) => (p, o),
                    _ => continue,
                },
                None => (Id::UNDEF, Id::UNDEF),
            };
            let needed = ids.len() + 1;
            budget.reserve(&mut ids, needed)?;
            ids.push([graph, p, o]);
        }
        ids.sort_unstable();
        ids.dedup();

        let matches = |[_, p, o, g]: [Id; 4]| {
            ids.binary_search(&[g, Id::UNDEF, Id::UNDEF]).is_ok()
                || ids.binary_search(&[g, p, o]).is_ok()
        };

        if let Some(history) = &self.history {
            let generation = &live.generation;
            let number = commit::generation_number(&generation.name);
            let (path, file, base) = {
                let history = limit.lock(history)?;
                let Some(entry) = history.gens.get(&number).filter(|e| e.base.seq <= after) else {
                    return Ok(None);
                };
                let path = entry.dir.join("wal.log");
                let file = File::open(&path)?;
                (path, file, entry.base.seq)
            };
            let from = limit.lock(&generation.wal_index)?.as_ref().map_or(
                wal::WalPoint {
                    seq: base,
                    offset: 0,
                },
                |i| i.floor(after),
            );
            return wal::probe::changed_through(
                file,
                &path,
                from,
                after,
                through,
                live.dvocab_len,
                max_work,
                &budget,
                matches,
            );
        }

        let left = Cell::new(max_work);
        let step = || {
            limit.check(0)?;
            let remaining = left.get();
            if remaining == 0 {
                return Err(Error::BudgetExceeded(crate::Budget {
                    kind: crate::BudgetKind::Rows,
                    limit: max_work,
                    requested: max_work.saturating_add(1),
                }));
            }
            left.set(remaining - 1);
            Ok(())
        };
        if let Some(changed) =
            self.probe_memory_states(after..=through, &live, &ids, limit, &step)?
        {
            return Ok(Some(changed));
        }
        // In-memory datasets ordinarily keep no past snapshots. The refresh
        // accumulator already subscribes to this ring, covering recent commits.
        self.deltas.probe_raw(
            &live.generation,
            after + 1..=through,
            limit,
            &step,
            &matches,
        )
    }

    fn probe_memory_states(
        &self,
        range: std::ops::RangeInclusive<u64>,
        live: &Arc<Snapshot>,
        ids: &[[Id; 3]],
        limit: ReadLimit<'_>,
        step: &impl Fn() -> Result<()>,
    ) -> Result<Option<bool>> {
        let Some(history) = &self.mem_history else {
            return Ok(None);
        };
        let read = |seq| {
            if seq == live.commit {
                Ok(Some(live.clone()))
            } else {
                limit.lock(history)?.get_with(seq, &limit)
            }
        };
        let Some(mut prev) = read(*range.start())? else {
            return Ok(None);
        };
        for seq in *range.start() + 1..=*range.end() {
            step()?;
            let Some(next) = read(seq)? else {
                return Ok(None);
            };
            if !Arc::ptr_eq(&prev.generation, &live.generation)
                || !Arc::ptr_eq(&next.generation, &live.generation)
            {
                return Ok(None);
            }
            if changed_in_memory(&prev, &next, ids, step)?
                || changed_in_memory(&next, &prev, ids, step)?
            {
                return Ok(Some(true));
            }
            prev = next;
        }
        Ok(Some(false))
    }
}

/// Scan indexed ranges of one state and look for an entry missing in the other.
/// Both snapshots use the same IDs. Only a graph range or a predicate-object
/// range is read, and each candidate is discarded after its membership probe.
fn changed_in_memory(
    from: &Snapshot,
    to: &Snapshot,
    patterns: &[[Id; 3]],
    step: &impl Fn() -> Result<()>,
) -> Result<bool> {
    for &[graph, p, o] in patterns {
        step()?;
        let graph_prefix = [graph.0];
        let po_prefix = [p.0, o.0];
        let (perm, prefix): (_, &[u64]) = if p.is_undef() {
            (Perm::Gspo, &graph_prefix)
        } else {
            (Perm::Pos, &po_prefix)
        };
        let mut changed = false;
        from.scan_checked(perm, prefix, step, |chunk| {
            let mut visit = |key| {
                step()?;
                let quad = perm.to_quad(&key);
                changed = quad[3] == graph && !to.contains(&quad)?;
                Ok(!changed)
            };
            match chunk {
                Chunk::Row(key) => visit(key),
                Chunk::Block(block, start, end) => {
                    for i in start..end {
                        if !visit(block.key(i))? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
            }
        })?;
        if changed {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_probe_bounds_a_large_unrelated_transaction_before_its_commit_marker() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
        let mut txn = store.write();
        let subject = txn
            .intern(&oxrdf::NamedNode::new_unchecked("urn:s").into())
            .unwrap();
        let predicate = txn
            .intern(&oxrdf::NamedNode::new_unchecked("urn:p").into())
            .unwrap();
        let unrelated = txn
            .intern(&oxrdf::NamedNode::new_unchecked("urn:other").into())
            .unwrap();
        let watched = txn
            .intern(&oxrdf::NamedNode::new_unchecked("urn:watched").into())
            .unwrap();
        let quad = |graph, value| [subject, predicate, Id::from_i64(value).unwrap(), graph];
        txn.insert(quad(watched, -1)).unwrap();
        txn.commit().unwrap();
        let after = store.head_commit().seq;

        let count = 4096;
        let mut txn = store.write();
        for n in 0..count {
            txn.insert(quad(unrelated, n)).unwrap();
        }
        txn.commit().unwrap();
        let unrelated_head = store.head_commit().seq;

        let mut txn = store.write();
        txn.insert(quad(watched, 0)).unwrap();
        txn.commit().unwrap();
        let watched_head = store.head_commit().seq;

        let probe = |after, through, work, deadline| {
            store.probe_raw_history(
                after,
                through,
                [HistoryPattern {
                    graph: "urn:watched",
                    predicate_object: None,
                }],
                work,
                deadline,
            )
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        assert_eq!(
            probe(after, unrelated_head, count as u64, deadline).unwrap(),
            None
        );
        assert_eq!(
            probe(after, unrelated_head, count as u64 + 1, deadline).unwrap(),
            Some(false)
        );
        assert_eq!(
            probe(unrelated_head, watched_head, 2, deadline).unwrap(),
            Some(true)
        );
        assert!(matches!(
            probe(after, unrelated_head, count as u64 + 1, Instant::now()),
            Err(Error::Timeout)
        ));
    }
}
