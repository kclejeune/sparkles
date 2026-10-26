//! Recover the selected missing catalog record from its exact transaction only.
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn selected_metadata(
    generation: &Generation,
    cache: &BlockCache,
    delta: &mut Delta,
    path: &Path,
    from: wal::WalPoint,
    to: wal::WalPoint,
    metadata: &mut CommitInfo,
    budget: &mut ColdBudget<'_>,
) -> Result<()> {
    let start = budget.used();
    let result = (|| {
        let mut cursor = wal::WalCursor::open_limited(
            path,
            from,
            &budget.owned_limit()?,
            history_limited::preparation_delta_bytes(delta),
        )?;
        let Some((seq, transaction, buffers)) = cursor.next_limited(
            &budget.owned_limit()?,
            history_limited::preparation_delta_bytes(delta),
        )?
        else {
            return Err(Error::Corrupt(
                "selected retained transaction is missing".into(),
            ));
        };
        if seq != metadata.seq {
            return Err(Error::Corrupt(
                "selected retained transaction has another sequence".into(),
            ));
        }
        // Remember each record's original visibility. Sorting by key and original
        // position later identifies the first observation without a sparse map or
        // another materialized Delta. The cursor's bytes remain admitted too.
        let count = transaction.len() / WAL_REC;
        let mut touched: Vec<([u64; 4], usize, bool, bool)> = Vec::new();
        let bytes = (count as u64)
            .checked_mul(std::mem::size_of::<([u64; 4], usize, bool, bool)>() as u64)
            .ok_or_else(|| Error::invalid("retained transaction size overflows"))?;
        budget.owned_limit()?.check(
            history_limited::preparation_delta_bytes(delta)
                .saturating_add(buffers)
                .saturating_add(bytes),
        )?;
        budget.reserve(&mut touched, count)?;
        let spo = generation.perm(Perm::Spo);
        let mut default_graph = false;
        for (index, record) in transaction.as_chunks::<WAL_REC>().0.iter().enumerate() {
            let held = history_limited::preparation_mutation_bytes(delta).saturating_add(buffers);
            budget.owned_limit()?.check(held)?;
            let quad = std::array::from_fn(|j| {
                Id(u64::from_le_bytes(
                    record[1 + j * 8..9 + j * 8].try_into().unwrap(),
                ))
            });
            let key = Perm::Spo.to_key(&quad);
            let in_base = spo.contains_limited(cache, &key, &budget.owned_limit()?, held)?;
            let present = delta.ins[Perm::Spo.index()].contains(&key)
                || (in_base && !delta.del[Perm::Spo.index()].contains(&key));
            touched.push((key, index, present, in_base));
            default_graph |= quad[3] == Id::DEFAULT_GRAPH;
            apply(delta, &quad, record[0] == WAL_INSERT, in_base);
        }
        budget.sort_by(&mut touched, |left, right| {
            left.0.cmp(&right.0).then(left.1.cmp(&right.1))
        })?;
        let (mut inserted, mut deleted) = (0u64, 0u64);
        let mut previous = None;
        for (key, _, before, in_base) in touched {
            budget.check()?;
            if previous == Some(key) {
                continue;
            }
            previous = Some(key);
            let after = delta.ins[Perm::Spo.index()].contains(&key)
                || (in_base && !delta.del[Perm::Spo.index()].contains(&key));
            match (before, after) {
                (false, true) => inserted += 1,
                (true, false) => deleted += 1,
                _ => {}
            }
        }
        if cursor.position() != to {
            return Err(Error::Corrupt(
                "selected retained transaction ends at another offset".into(),
            ));
        }
        metadata.inserted = inserted;
        metadata.deleted = deleted;
        metadata.quads = generation
            .meta
            .quads
            .saturating_add(delta.inserts() as u64)
            .saturating_sub(delta.deletes() as u64);
        metadata.default_graph = default_graph;
        budget.owned_limit()?.check(
            history_limited::preparation_delta_bytes(delta).saturating_add(cursor.buffer_bytes()),
        )
    })();
    budget.release(budget.used() - start);
    result
}
