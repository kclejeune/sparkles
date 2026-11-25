//! Linked generations: the first generation of a branch, which shares the index files of
//! an upstream generation instead of building its own.
//!
//! `gen-0001/link.json` (a branch's first generation) lists the log segments that lead from the shared base index to
//! the branch's starting commit, oldest first. The first segment's generation holds the
//! base files (vocabulary, permutations, statistics). Opening the generation opens those
//! files, layers the delta vocabularies of the segments under the branch's own
//! `delta.vocab`, and replays each segment's log up to its recorded end. The result is
//! the delta of the starting commit, the branch's *base delta*. The branch's own log is
//! then replayed on top of it, as for any generation.

use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The file of a linked generation.
pub(crate) const LINK_FILE: &str = "link.json";
pub(crate) const OVERLAY_FILE: &str = "base.delta";

pub(crate) fn overlay_checksum(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Overlay {
    pub bytes: u64,
    pub sha256: String,
    pub next_bnode: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LinkFile {
    pub format: u32,
    /// the commit the linked generation starts at (the branch's starting commit)
    pub base_seq: u64,
    pub segments: Vec<Segment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<Overlay>,
}

/// One upstream log segment of a linked generation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Segment {
    /// the branch whose generation it is
    pub branch_id: uuid::Uuid,
    /// the generation's directory name (`gen-NNNN`)
    pub generation: String,
    /// the generation's directory, relative to the dataset's directory
    pub path: String,
    /// the commit the generation's base holds (or, for a linked generation, starts at)
    pub base_seq: u64,
    /// the commit the segment ends at
    pub end_seq: u64,
    /// where that commit ends in the generation's log
    pub wal_end: u64,
    /// the delta-vocabulary length (ids below it) the segment's records may name
    pub dvocab_len: u64,
}

impl Segment {
    pub fn dir(&self, dataset_root: &Path) -> PathBuf {
        dataset_root.join(&self.path)
    }
}

/// What a linked generation knows about its link.
pub struct Linked {
    pub(crate) file: LinkFile,
    /// the delta of the starting commit over the shared base index
    pub(crate) base_delta: Delta,
    /// the directory of the shared base files
    pub(crate) base_dir: PathBuf,
}

impl Linked {
    /// The directory of the generation whose base files a linked generation reads.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// The commit the linked generation starts at.
    pub fn base_seq(&self) -> u64 {
        self.file.base_seq
    }

    /// The upstream generations the link reads, as `(branch id, generation name)`.
    pub fn generations(&self) -> Vec<(uuid::Uuid, String)> {
        self.file
            .segments
            .iter()
            .map(|s| (s.branch_id, s.generation.clone()))
            .collect()
    }
}

pub(crate) fn read_link(gen_dir: &Path) -> Result<Option<LinkFile>> {
    Ok(read_link_with(gen_dir, &mut crate::budgeted::Unlimited)?.map(|loaded| loaded.value))
}

pub(crate) fn read_link_with<C: crate::budgeted::BudgetControl>(
    gen_dir: &Path,
    control: &mut C,
) -> Result<Option<cold::Loaded<LinkFile>>> {
    let path = gen_dir.join(LINK_FILE);
    let bytes = match control.read_file(&path) {
        Ok(b) => b,
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let label = (!C::ACCOUNTS).then(|| path.display().to_string());
    let loaded: cold::Loaded<LinkFile> =
        control.json(&bytes, label.as_deref().unwrap_or(LINK_FILE))?;
    control.drop_vec(bytes);
    check_link(&path, &loaded.value)?;
    if !C::ACCOUNTS {
        return Ok(Some(loaded));
    }
    let mut retained = std::mem::size_of::<LinkFile>() as u64
        + loaded.value.segments.capacity() as u64 * std::mem::size_of::<Segment>() as u64;
    for segment in &loaded.value.segments {
        control.check(0)?;
        retained = retained
            .saturating_add(segment.generation.capacity() as u64)
            .saturating_add(segment.path.capacity() as u64);
    }
    if let Some(overlay) = &loaded.value.overlay {
        retained = retained.saturating_add(overlay.sha256.capacity() as u64);
    }
    Ok(Some(cold::Loaded {
        value: cold::retain_parse(loaded, retained, control)?,
        charge: retained,
    }))
}

pub(super) fn check_link(path: &Path, f: &LinkFile) -> Result<()> {
    if !matches!((f.format, f.overlay.is_some()), (1, false) | (2, true)) {
        return Err(Error::Corrupt(format!(
            "{} has unsupported format {} or incompatible overlay metadata",
            path.display(),
            f.format
        )));
    }
    if f.segments.is_empty() {
        return Err(Error::Corrupt(format!(
            "{} lists no segment",
            path.display()
        )));
    }
    Ok(())
}

/// The dataset directory of a branch store's root (`<dataset>/branches/<id>`).
pub(crate) fn dataset_root_of(branch_root: &Path) -> Result<PathBuf> {
    branch_root
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            Error::Corrupt(format!(
                "{} is not inside a dataset's branches/ directory",
                branch_root.display()
            ))
        })
}

impl Generation {
    /// Whether this generation reads an upstream generation's files.
    pub fn linked(&self) -> Option<&Arc<Linked>> {
        self.link.as_ref()
    }

    /// The delta of the state the generation's base holds: empty, except for a linked
    /// generation, whose base is its starting commit.
    pub(crate) fn base_delta(&self) -> Delta {
        self.link
            .as_ref()
            .map(|l| l.base_delta.clone())
            .unwrap_or_default()
    }

    /// Open the linked generation in `gen_dir` (named `name`) of the branch store rooted
    /// at `branch_root`. `read_only` opens it for history reads, without taking the
    /// appends of its delta vocabulary.
    pub(crate) fn open_linked(
        gen_dir: &Path,
        name: &str,
        branch_root: &Path,
        file: LinkFile,
        read_only: bool,
        cache: &Arc<BlockCache>,
    ) -> Result<Generation> {
        Ok(Self::open_linked_with(
            gen_dir,
            name,
            branch_root,
            cold::Loaded {
                value: file,
                charge: 0,
            },
            read_only,
            cache,
            &mut crate::budgeted::Unlimited,
        )?
        .value)
    }

    pub(crate) fn open_linked_with<C: crate::budgeted::BudgetControl>(
        gen_dir: &Path,
        name: &str,
        branch_root: &Path,
        loaded: cold::Loaded<LinkFile>,
        read_only: bool,
        cache: &BlockCache,
        control: &mut C,
    ) -> Result<cold::Loaded<Generation>> {
        let start = control
            .used()
            .checked_sub(loaded.charge)
            .expect("link metadata was admitted by the same budget");
        let file = loaded.value;
        let root_charge = branch_root.as_os_str().len() as u64 + 64;
        control.charge(root_charge)?;
        let dataset_root = dataset_root_of(branch_root)?;
        let (base, vocab) = if C::ACCOUNTS {
            let opened = cold::open_link_vocabulary(gen_dir, &dataset_root, &file, control)?;
            (opened.base, opened.vocab)
        } else {
            let base = cold::Loaded {
                value: file.segments[0].dir(&dataset_root),
                charge: 0,
            };
            let prefix: Vec<(PathBuf, u64)> = file
                .segments
                .iter()
                .map(|s| (s.dir(&dataset_root).join("delta.vocab"), s.dvocab_len))
                .collect();
            let vocab =
                control.open_dvocab_layered(&prefix, &gen_dir.join("delta.vocab"), read_only)?;
            (base, vocab)
        };
        let written_by = if C::ACCOUNTS {
            None
        } else {
            base.value.parent().and_then(commit::read_written_by)
        };
        let mut generation = Self::open_base_with(
            &base.value,
            name,
            written_by.as_deref(),
            Some(vocab),
            control,
        )?
        .value;
        control.charge(gen_dir.as_os_str().len() as u64 + 64)?;
        generation.dir = Some(gen_dir.to_path_buf());
        let mut delta = Delta::default();
        let mut delta_charge = 0u64;
        for segment in &file.segments {
            let source = if C::ACCOUNTS {
                cold::segment_dir(segment, &dataset_root, control)?
            } else {
                cold::Loaded {
                    value: segment.dir(&dataset_root),
                    charge: 0,
                }
            };
            // A child of a relinked branch inherits its immutable base overlay as
            // well as its log. The layered vocabulary keeps the same prefix IDs.
            if let Some(inherited) = read_link_with(&source.value, control)? {
                if let Some(overlay) = &inherited.value.overlay {
                    apply_overlay_with(
                        &source.value,
                        overlay,
                        &mut generation,
                        cache,
                        &mut delta,
                        &mut delta_charge,
                        control,
                    )?;
                }
                drop(inherited.value);
                control.release(inherited.charge);
            }
            // WAL replay owns and admits the delta itself. Keep all dictionary,
            // descriptor and link storage in the same remaining allowance.
            control.release(delta_charge);
            control.check(0)?;
            wal::apply_forward(
                &source.value.join("wal.log"),
                &generation,
                cache,
                &mut delta,
                wal::WalPoint {
                    seq: segment.base_seq,
                    offset: 0,
                },
                wal::WalPoint {
                    seq: segment.end_seq,
                    offset: segment.wal_end,
                },
                &*control,
            )?;
            if C::ACCOUNTS {
                delta_charge = history_limited::preparation_delta_bytes(&delta);
                control.charge(delta_charge)?;
            }
            drop(source.value);
            control.release(source.charge);
        }
        if let Some(overlay) = &file.overlay {
            apply_overlay_with(
                gen_dir,
                overlay,
                &mut generation,
                cache,
                &mut delta,
                &mut delta_charge,
                control,
            )?;
        }
        control.charge(std::mem::size_of::<Linked>() as u64 + 64)?;
        generation.link = Some(Arc::new(Linked {
            file,
            base_delta: delta,
            base_dir: base.value,
        }));
        // base.value now belongs to Linked; its admission stays retained.
        drop(dataset_root);
        control.release(root_charge);
        let charge = control.used() - start;
        generation.cold_bytes = charge;
        control.check(0)?;
        Ok(cold::Loaded {
            value: generation,
            charge,
        })
    }

    /// Give this generation's permutations the block-cache identities of `other`'s when
    /// both read the same files, so that a branch and its upstream share cached blocks.
    pub(crate) fn share_blocks_with(&mut self, other: &Generation) {
        let mine = self
            .link
            .as_ref()
            .map(|l| l.base_dir.clone())
            .or_else(|| self.dir.clone());
        let theirs = other
            .link
            .as_ref()
            .map(|l| l.base_dir.clone())
            .or_else(|| other.dir.clone());
        if mine.is_some() && mine == theirs {
            for (a, b) in self.perms.iter_mut().zip(other.perms.iter()) {
                if a.rows == b.rows && a.blocks.len() == b.blocks.len() {
                    a.uid = b.uid;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_overlay_with<C: crate::budgeted::BudgetControl>(
    dir: &Path,
    overlay: &Overlay,
    generation: &mut Generation,
    cache: &BlockCache,
    delta: &mut Delta,
    delta_charge: &mut u64,
    control: &mut C,
) -> Result<()> {
    let path = dir.join(OVERLAY_FILE);
    let bytes = read_verified_overlay_with(&path, overlay, control)?;
    for record in bytes.as_chunks::<WAL_REC>().0 {
        control.check(0)?;
        if !matches!(record[0], WAL_INSERT | WAL_DELETE) {
            control.drop_vec(bytes);
            return Err(Error::Corrupt(format!(
                "{}: invalid base overlay operation",
                path.display()
            )));
        }
        let quad = wal::record_quad(record);
        let in_base = if C::ACCOUNTS {
            control.release(*delta_charge);
            control.check(0)?;
            let mutation = history_limited::preparation_mutation_bytes(delta);
            control.check(mutation)?;
            generation.perm(Perm::Spo).contains_with(
                cache,
                &Perm::Spo.to_key(&quad),
                &*control,
                mutation,
            )?
        } else {
            generation
                .perm(Perm::Spo)
                .contains(cache, &Perm::Spo.to_key(&quad))?
        };
        apply(delta, &quad, record[0] == WAL_INSERT, in_base);
        if C::ACCOUNTS {
            *delta_charge = history_limited::preparation_delta_bytes(delta);
            control.charge(*delta_charge)?;
        }
    }
    generation.meta.next_bnode = generation.meta.next_bnode.max(overlay.next_bnode);
    control.drop_vec(bytes);
    control.check(0)
}

fn read_verified_overlay_with<C: crate::budgeted::BudgetControl>(
    path: &Path,
    overlay: &Overlay,
    control: &mut C,
) -> Result<Vec<u8>> {
    let bytes = control.read_file(path)?;
    let digest = control.sha256(&bytes)?;
    let hex = b"0123456789abcdef";
    let matches = overlay.sha256.len() == 64
        && digest.iter().enumerate().all(|(index, byte)| {
            overlay.sha256.as_bytes()[index * 2] == hex[(byte >> 4) as usize]
                && overlay.sha256.as_bytes()[index * 2 + 1] == hex[(byte & 15) as usize]
        });
    if bytes.len() as u64 != overlay.bytes || !bytes.len().is_multiple_of(WAL_REC) || !matches {
        control.drop_vec(bytes);
        return Err(Error::Corrupt(format!(
            "{}: invalid base overlay length or checksum",
            path.display()
        )));
    }
    Ok(bytes)
}

fn checked_overlay(dir: &Path, overlay: &Overlay) -> Result<Vec<u8>> {
    let path = dir.join(OVERLAY_FILE);
    let bytes = read_verified_overlay_with(&path, overlay, &mut crate::budgeted::Unlimited)?;
    for rec in bytes.as_chunks::<WAL_REC>().0 {
        if !matches!(rec[0], WAL_INSERT | WAL_DELETE) {
            return Err(Error::Corrupt(format!(
                "{}: invalid base overlay operation",
                path.display()
            )));
        }
    }
    Ok(bytes)
}

/// The segments of the linked generation in `gen_dir`, as (directory relative to the
/// dataset, log bytes, delta terms), for `sparkles check` (`None` when not linked).
pub fn read_link_file(gen_dir: &Path) -> Result<Option<Vec<(String, u64, u64)>>> {
    let Some(file) = read_link(gen_dir)? else {
        return Ok(None);
    };
    if let Some(overlay) = &file.overlay {
        checked_overlay(gen_dir, overlay)?;
    }
    let dataset_root = dataset_root_of(
        gen_dir
            .parent()
            .ok_or_else(|| Error::Corrupt("linked generation lacks a branch directory".into()))?,
    )?;
    for segment in &file.segments {
        let dir = segment.dir(&dataset_root);
        if let Some(parent) = read_link(&dir)?
            && let Some(overlay) = parent.overlay
        {
            checked_overlay(&dir, &overlay)?;
        }
    }
    Ok(Some(
        file.segments
            .into_iter()
            .map(|s| (s.path, s.wal_end, s.dvocab_len))
            .collect(),
    ))
}
