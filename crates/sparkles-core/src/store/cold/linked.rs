//! Read-only linked generation vocabulary and segment validation under the preparation
//! allowance. Ordinary and controlled linked opens share [`Generation::open_linked_with`]
//! in `store/link.rs`.
use super::*;
use crate::{
    budgeted::BudgetControl,
    store::{WAL_REC, link},
    vocab::DeltaVocab,
};
use std::path::{Component, PathBuf};

pub(in crate::store) fn segment_dir<C: BudgetControl>(
    segment: &link::Segment,
    root: &Path,
    control: &mut C,
) -> Result<Loaded<PathBuf>> {
    control.check(0)?;
    for component in Path::new(&segment.path).components() {
        control.check(0)?;
        if matches!(
            component,
            Component::RootDir | Component::Prefix(_) | Component::ParentDir
        ) {
            return Err(Error::Corrupt(
                "linked generation leaves its dataset directory".into(),
            ));
        }
    }
    if Path::new(&segment.path).file_name() != Some(std::ffi::OsStr::new(&segment.generation)) {
        return Err(Error::Corrupt("linked path and generation disagree".into()));
    }
    let charge =
        (root.as_os_str().len() as u64 + segment.path.len() as u64 + 128).saturating_mul(2);
    control.charge(charge)?;
    Ok(Loaded {
        value: segment.dir(root),
        charge,
    })
}

pub(in crate::store) struct OpenedVocabulary {
    pub(in crate::store) base: Loaded<PathBuf>,
    pub(in crate::store) vocab: Loaded<DeltaVocab>,
}

/// The physical dictionary and source framing checks shared by selected linked
/// generation opening and current-head proof. No quads or head delta are decoded.
pub(in crate::store) fn open_link_vocabulary<C: BudgetControl>(
    dir: &Path,
    dataset_root: &Path,
    file: &link::LinkFile,
    control: &mut C,
) -> Result<OpenedVocabulary> {
    let base = segment_dir(&file.segments[0], dataset_root, control)?;
    if let Some(nested) = link::read_link_with(&base.value, control)? {
        drop(nested.value);
        return Err(Error::Corrupt(
            "a linked generation's first segment is not a base index".into(),
        ));
    }
    let mut prefix = Vec::new();
    let mut prefix_paths = 0u64;
    for segment in &file.segments {
        control.check(0)?;
        let source = segment_dir(segment, dataset_root, control)?;
        let identity = crate::commit::read_gen_commit_with(&source.value, control)?
            .ok_or_else(|| Error::Corrupt("a linked segment has no generation identity".into()))?;
        if identity.value.dataset_id != segment.branch_id
            || identity.value.commit.seq != segment.base_seq
            || segment.end_seq < segment.base_seq
            || !segment.wal_end.is_multiple_of(WAL_REC as u64)
        {
            return Err(Error::Corrupt(
                "a linked segment disagrees with its physical generation".into(),
            ));
        }
        drop(identity.value);
        control.release(identity.charge);
        let need = prefix.len() + 1;
        control.reserve(&mut prefix, need)?;
        // The selected path and its extension can coexist during PathBuf growth.
        let path_charge = source.value.as_os_str().len() as u64 * 2 + 128;
        control.charge(path_charge)?;
        prefix.push((source.value.join("delta.vocab"), segment.dvocab_len));
        prefix_paths = prefix_paths.saturating_add(path_charge);
        drop(source.value);
        control.release(source.charge);
    }
    let vocab = DeltaVocab::open_layered_with(&prefix, &dir.join("delta.vocab"), true, control)?;
    control.drop_vec(prefix);
    control.release(prefix_paths);
    Ok(OpenedVocabulary { base, vocab })
}

/// Obtain only the terms needed to validate a cold origin's committed WAL prefix.
/// This never opens the head's index columns or materializes its quad state.
pub(crate) fn head_vocabulary_controlled(
    dir: &Path,
    branch_root: &Path,
    budget: &mut ColdBudget<'_>,
) -> Result<Loaded<DeltaVocab>> {
    let start = budget.used();
    let path_scratch = (dir.as_os_str().len() as u64)
        .saturating_add(branch_root.as_os_str().len() as u64)
        .saturating_mul(8)
        .saturating_add(1024);
    budget.charge(path_scratch)?;
    let result = (|| {
        let Some(file) = link::read_link_with(dir, budget)? else {
            return DeltaVocab::open_read_only_with(&dir.join("delta.vocab"), budget);
        };
        let root_charge = branch_root.as_os_str().len() as u64 + 64;
        budget.charge(root_charge)?;
        let dataset_root = link::dataset_root_of(branch_root)?;
        let OpenedVocabulary { base, vocab } =
            open_link_vocabulary(dir, &dataset_root, &file.value, budget)?;
        drop(base.value);
        budget.release(base.charge);
        drop(file.value);
        budget.release(file.charge);
        drop(dataset_root);
        budget.release(root_charge);
        budget.check()?;
        Ok(vocab)
    })();
    budget.release(path_scratch);
    if result.is_err() {
        budget.release(budget.used() - start);
    }
    result
}
