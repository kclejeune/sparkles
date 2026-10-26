//! Read-only linked generations under the same preparation allowance.
use super::*;
use crate::{
    index::{BlockCache, Perm},
    store::{
        Delta, Generation, WAL_DELETE, WAL_INSERT, WAL_REC, apply, history_limited, link, wal,
    },
    vocab::DeltaVocab,
};
use std::{
    path::{Component, PathBuf},
    sync::Arc,
};

pub(crate) fn read_link_controlled(
    dir: &Path,
    budget: &mut ColdBudget<'_>,
) -> Result<Option<Loaded<link::LinkFile>>> {
    let path = dir.join(link::LINK_FILE);
    let bytes = match budget.read_file(&path) {
        Ok(bytes) => bytes,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let loaded: Loaded<link::LinkFile> = budget.json(&bytes)?;
    budget.drop_vec(bytes);
    link::check_link(&path, &loaded.value)?;
    let mut retained = std::mem::size_of::<link::LinkFile>() as u64
        + loaded.value.segments.capacity() as u64 * std::mem::size_of::<link::Segment>() as u64;
    for segment in &loaded.value.segments {
        budget.check()?;
        retained = retained
            .saturating_add(segment.generation.capacity() as u64)
            .saturating_add(segment.path.capacity() as u64);
    }
    if let Some(overlay) = &loaded.value.overlay {
        retained = retained.saturating_add(overlay.sha256.capacity() as u64);
    }
    Ok(Some(Loaded {
        value: retain_parse(loaded, retained, budget)?,
        charge: retained,
    }))
}

fn segment_dir(
    segment: &link::Segment,
    root: &Path,
    budget: &mut ColdBudget<'_>,
) -> Result<Loaded<PathBuf>> {
    budget.check()?;
    for component in Path::new(&segment.path).components() {
        budget.check()?;
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
    budget.charge(charge)?;
    Ok(Loaded {
        value: segment.dir(root),
        charge,
    })
}

impl Generation {
    pub(crate) fn open_linked_controlled(
        dir: &Path,
        name: &str,
        branch_root: &Path,
        loaded: Loaded<link::LinkFile>,
        cache: &BlockCache,
        budget: &mut ColdBudget<'_>,
    ) -> Result<Loaded<Self>> {
        let start = budget
            .used()
            .checked_sub(loaded.charge)
            .expect("link metadata was admitted by the same budget");
        let file = loaded.value;
        let root_charge = branch_root.as_os_str().len() as u64 + 64;
        budget.charge(root_charge)?;
        let dataset_root = link::dataset_root_of(branch_root)?;
        let base = segment_dir(&file.segments[0], &dataset_root, budget)?;
        if let Some(nested) = read_link_controlled(&base.value, budget)? {
            drop(nested.value);
            return Err(Error::Corrupt(
                "a linked generation's first segment is not a base index".into(),
            ));
        }
        let mut prefix = Vec::new();
        let mut prefix_paths = 0u64;
        for segment in &file.segments {
            budget.check()?;
            let source = segment_dir(segment, &dataset_root, budget)?;
            let identity = crate::commit::read_gen_commit_controlled(&source.value, budget)?
                .ok_or_else(|| {
                    Error::Corrupt("a linked segment has no generation identity".into())
                })?;
            if identity.value.dataset_id != segment.branch_id
                || identity.value.commit.seq != segment.base_seq
                || segment.end_seq < segment.base_seq
                || !segment.wal_end.is_multiple_of(WAL_REC as u64)
            {
                return Err(Error::Corrupt(
                    "a linked segment disagrees with its physical generation".into(),
                ));
            }
            crate::store::check_strict_from(
                &source.value.join("wal.log"),
                identity.value.wal_strict_from,
            )?;
            drop(identity.value);
            budget.release(identity.charge);
            let need = prefix.len() + 1;
            budget.reserve(&mut prefix, need)?;
            // The selected path and its extension can coexist during PathBuf growth.
            let path_charge = source.value.as_os_str().len() as u64 * 2 + 128;
            budget.charge(path_charge)?;
            prefix.push((source.value.join("delta.vocab"), segment.dvocab_len));
            prefix_paths = prefix_paths.saturating_add(path_charge);
            drop(source.value);
            budget.release(source.charge);
        }
        let vocab = DeltaVocab::open_layered_controlled(&prefix, &dir.join("delta.vocab"), budget)?;
        budget.drop_vec(prefix);
        budget.release(prefix_paths);
        let mut generation =
            Self::open_base_controlled(&base.value, name, None, Some(vocab), budget)?.value;
        budget.charge(dir.as_os_str().len() as u64 + 64)?;
        generation.dir = Some(dir.to_owned());
        let mut delta = Delta::default();
        let mut delta_charge = 0;
        for segment in &file.segments {
            let source = segment_dir(segment, &dataset_root, budget)?;
            if let Some(inherited) = read_link_controlled(&source.value, budget)? {
                if let Some(overlay) = &inherited.value.overlay {
                    apply_overlay_controlled(
                        &source.value,
                        overlay,
                        &mut generation,
                        cache,
                        &mut delta,
                        &mut delta_charge,
                        budget,
                    )?;
                }
                drop(inherited.value);
                budget.release(inherited.charge);
            }
            // WAL replay owns and admits the delta itself. Keep all dictionary,
            // descriptor and link storage in the same remaining allowance.
            budget.release(delta_charge);
            let limit = budget.owned_limit()?;
            wal::apply_forward_limited(
                &source.value.join("wal.log"),
                &generation,
                cache,
                &mut delta,
                wal::WalPoint {
                    seq: segment.base_seq,
                    offset: 0,
                    folding: false,
                },
                wal::WalPoint {
                    seq: segment.end_seq,
                    offset: segment.wal_end,
                    folding: false,
                },
                &limit,
            )?;
            delta_charge = history_limited::preparation_delta_bytes(&delta);
            budget.charge(delta_charge)?;
            drop(source.value);
            budget.release(source.charge);
        }
        if let Some(overlay) = &file.overlay {
            apply_overlay_controlled(
                dir,
                overlay,
                &mut generation,
                cache,
                &mut delta,
                &mut delta_charge,
                budget,
            )?;
        }
        budget.charge(std::mem::size_of::<link::Linked>() as u64 + 64)?;
        generation.link = Some(Arc::new(link::Linked {
            file,
            base_delta: delta,
            base_dir: base.value,
        }));
        // base.value now belongs to Linked; its admission stays retained.
        drop(dataset_root);
        budget.release(root_charge);
        let charge = budget.used() - start;
        generation.cold_bytes = charge;
        budget.check()?;
        Ok(Loaded {
            value: generation,
            charge,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_overlay_controlled(
    dir: &Path,
    overlay: &link::Overlay,
    generation: &mut Generation,
    cache: &BlockCache,
    delta: &mut Delta,
    delta_charge: &mut u64,
    budget: &mut ColdBudget<'_>,
) -> Result<()> {
    let bytes = budget.read_file(&dir.join(link::OVERLAY_FILE))?;
    let digest = budget.sha256(&bytes)?;
    let hex = b"0123456789abcdef";
    let matches = overlay.sha256.len() == 64
        && digest.iter().enumerate().all(|(index, byte)| {
            overlay.sha256.as_bytes()[index * 2] == hex[(byte >> 4) as usize]
                && overlay.sha256.as_bytes()[index * 2 + 1] == hex[(byte & 15) as usize]
        });
    if bytes.len() as u64 != overlay.bytes || !bytes.len().is_multiple_of(WAL_REC) || !matches {
        return Err(Error::Corrupt(
            "invalid linked base overlay length or checksum".into(),
        ));
    }
    for record in bytes.as_chunks::<WAL_REC>().0 {
        budget.check()?;
        if !matches!(record[0], WAL_INSERT | WAL_DELETE) {
            return Err(Error::Corrupt(
                "invalid linked base overlay operation".into(),
            ));
        }
        budget.release(*delta_charge);
        let limit = budget.owned_limit()?;
        let mutation = history_limited::preparation_mutation_bytes(delta);
        limit.check(mutation)?;
        let quad = wal::record_quad(record);
        let in_base = generation.perm(Perm::Spo).contains_limited(
            cache,
            &Perm::Spo.to_key(&quad),
            &limit,
            mutation,
        )?;
        apply(delta, &quad, record[0] == WAL_INSERT, in_base);
        *delta_charge = history_limited::preparation_delta_bytes(delta);
        budget.charge(*delta_charge)?;
    }
    generation.meta.next_bnode = generation.meta.next_bnode.max(overlay.next_bnode);
    budget.drop_vec(bytes);
    budget.check()
}
