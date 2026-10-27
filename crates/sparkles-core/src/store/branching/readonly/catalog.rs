//! Scalar reconciliation follows Catalog::open without rewriting any file.
use super::*;

/// Selected records are metadata only. A missing supported tail is explicit until
/// its selected replay supplies counts; a pruned prefix must never be resurrected.
pub(super) struct CatalogSelection {
    pub(super) selected: Option<CommitInfo>,
    pub(super) recovered: bool,
    pub(super) last: Option<u64>,
    pub(super) first: u64,
}

pub(super) fn select_catalog(
    root: &Path,
    id: uuid::Uuid,
    at: &At,
    wanted: Option<u64>,
    base: CommitInfo,
    head: u64,
    budget: &mut ColdBudget<'_>,
) -> Result<CatalogSelection> {
    let start = budget.used();
    budget.charge(
        (root.as_os_str().len() as u64)
            .saturating_mul(4)
            .saturating_add(1024),
    )?;
    let result = (|| {
        let mut first = None;
        let mut last = None;
        let mut previous_time = None;
        let mut selected = None;
        let accepts = |commit: &CommitInfo| match at {
            At::Time(ms) => commit.timestamp_ms <= *ms,
            _ => wanted == Some(commit.seq),
        };
        let catalog = commit::walk_catalog_controlled(
            &root.join("commits.bin"),
            budget,
            |commit, budget| {
                budget.check()?;
                if commit.seq > head {
                    return Ok(());
                }
                if previous_time.is_some_and(|time| commit.timestamp_ms < time) {
                    return Err(Error::Corrupt(
                        "retained catalog timestamps are not monotonic".into(),
                    ));
                }
                previous_time = Some(commit.timestamp_ms);
                first.get_or_insert(commit.seq);
                last = Some(commit.seq);
                if accepts(&commit) {
                    selected = Some(commit);
                }
                Ok(())
            },
        )?;
        if catalog.is_some_and(|(found, _, _)| found != id) {
            return Err(Error::Corrupt(
                "retained catalog belongs to another branch".into(),
            ));
        }
        let gap = last.is_some_and(|last: u64| last.saturating_add(1) < base.seq);
        if gap {
            first = None;
            last = None;
            selected = None;
        }
        let effective_first = first.unwrap_or(base.seq);
        if last.is_none_or(|last| last < base.seq) && base.seq >= effective_first && accepts(&base)
        {
            selected = Some(base);
        }
        if last.is_none_or(|last| last < base.seq) {
            last = Some(base.seq);
        }
        budget.check()?;
        Ok(CatalogSelection {
            selected,
            recovered: false,
            last,
            first: effective_first,
        })
    })();
    budget.release(budget.used() - start);
    result
}

/// Recover only the supported missing tail. Already present catalog metadata wins,
/// and a pruned prefix never becomes available merely because generation bytes exist.
#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile_tail(
    root: &Path,
    id: uuid::Uuid,
    current: &Current,
    at: &At,
    wanted: Option<u64>,
    head: u64,
    selection: &mut CatalogSelection,
    budget: &mut ColdBudget<'_>,
) -> Result<()> {
    let last = selection.last.unwrap_or(current.base.seq);
    if last >= head {
        return budget.check();
    }
    let start = budget.used();
    let result = (|| {
        let vocab = crate::store::cold::head_vocabulary_controlled(&current.dir, root, budget)?;
        let terms = vocab.value.len_limited(&budget.owned_limit()?)?;
        let mut previous_time = current.base.timestamp_ms;
        prove_file_with(
            root,
            id,
            current,
            u64::MAX,
            terms,
            budget,
            &mut |observation| {
                let (timestamp_ms, kind, flags, reconstructed) = match observation.metadata {
                    Some((timestamp, kind, flags)) => (timestamp, kind, flags, false),
                    None => (previous_time, commit::CommitKind::Unknown, 0, true),
                };
                previous_time = timestamp_ms;
                let seq = observation.point.seq;
                let accepts = match at {
                    At::Head => seq == head,
                    At::Time(ms) => timestamp_ms <= *ms,
                    _ => wanted == Some(seq),
                };
                if seq > last && seq >= selection.first && accepts {
                    selection.selected = Some(CommitInfo {
                        seq,
                        timestamp_ms,
                        kind,
                        inserted: 0,
                        deleted: 0,
                        quads: 0,
                        generation: current.number,
                        bulk: false,
                        exact: true,
                        reconstructed,
                        default_graph: false,
                        unvalidated: flags & commit::WAL_FLAG_UNVALIDATED != 0,
                    });
                    selection.recovered = true;
                }
                Ok(())
            },
        )?;
        // Nothing escaped until the entire prefix, including its durable boundary,
        // passed. Selected counts are filled by that exact transaction's replay.
        budget.check()
    })();
    budget.release(budget.used() - start);
    result
}
