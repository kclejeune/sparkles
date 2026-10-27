//! Retained ranges are prepared from scalar metadata, never by recovering a Store.
use super::*;

fn ranges(
    root: &Path,
    id: uuid::Uuid,
    current: &Current,
    head: u64,
    budget: &mut ColdBudget<'_>,
) -> Result<Vec<(u64, u64)>> {
    let start = budget.used();
    let scratch = (64 << 10) + root.as_os_str().len() as u64 * 4;
    budget.charge(scratch)?;
    let result = (|| {
        let mut ranges = Vec::new();
        for entry in std::fs::read_dir(root)? {
            budget.check()?;
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(digits) = name.strip_prefix("gen-") else {
                continue;
            };
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let Ok(number) = digits.parse::<u32>() else {
                continue;
            };
            if number > current.number {
                continue;
            }
            let before = budget.used();
            budget.charge(
                (root.as_os_str().len() as u64 + name.len() as u64 + 128).saturating_mul(4),
            )?;
            let path = entry.path();
            let identity = match commit::read_gen_commit_controlled(&path, budget) {
                Ok(Some(identity)) => identity,
                Ok(None) => {
                    drop(path);
                    budget.release(budget.used() - before);
                    continue;
                }
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    drop(path);
                    budget.release(budget.used() - before);
                    continue;
                }
                Err(error) => return Err(error),
            };
            let range = if identity.value.dataset_id == id {
                let generation = Current {
                    name: name.to_owned(),
                    number,
                    dir: path,
                    base: identity.value.commit,
                    folding: identity.value.origin == "baseline",
                    strict_from: identity.value.wal_strict_from,
                };
                let end = generation_end(root, id, current.number, head, &generation, budget)?;
                Some((generation.base.seq, end))
            } else {
                drop(path);
                None
            };
            drop(identity.value);
            budget.release(budget.used() - before);
            if let Some(range) = range {
                let next = ranges.len() + 1;
                budget.reserve(&mut ranges, next)?;
                ranges.push(range);
            }
        }
        budget.sort_by(&mut ranges, Ord::cmp)?;
        let mut keep = 0usize;
        for index in 0..ranges.len() {
            budget.check()?;
            let (from, to) = ranges[index];
            if from > to {
                continue;
            }
            if keep > 0 && from <= ranges[keep - 1].1.saturating_add(1) {
                ranges[keep - 1].1 = ranges[keep - 1].1.max(to);
            } else {
                ranges[keep] = (from, to);
                keep += 1;
            }
        }
        ranges.truncate(keep);
        budget.check()?;
        Ok(ranges)
    })();
    budget.release(scratch);
    if result.is_err() {
        budget.release(budget.used() - start);
    }
    result
}

#[allow(clippy::too_many_arguments)]
pub(super) fn gone_controlled(
    root: &Path,
    id: uuid::Uuid,
    current: &Current,
    head: u64,
    seq: Option<u64>,
    at: Option<At>,
    metadata: Option<CommitInfo>,
    budget: &mut ColdBudget<'_>,
) -> Result<Error> {
    let reconstructable = ranges(root, id, current, head, budget)?;
    budget.charge(1024)?;
    let target = match (seq, &at) {
        (Some(seq), _) => format!("commit {seq}"),
        (None, Some(at)) => at.to_string(),
        _ => return Err(Error::invalid("missing retained selector identity")),
    };
    let oldest = reconstructable.first().map_or(String::new(), |range| {
        format!("; the oldest reconstructable commit is {}", range.0)
    });
    budget.check()?;
    Ok(Error::HistoryGone(Box::new(crate::history::HistoryGone {
        message: format!("{target} is no longer reconstructable{oldest}"),
        seq,
        at,
        head,
        snapshot: None,
        reconstructable,
        metadata,
    })))
}
