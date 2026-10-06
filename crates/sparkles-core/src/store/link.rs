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
    let path = gen_dir.join(LINK_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let f: LinkFile = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display())))?;
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
    Ok(Some(f))
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
        let ds_root = dataset_root_of(branch_root)?;
        let base_dir = file.segments[0].dir(&ds_root);
        let prefix: Vec<(PathBuf, u64)> = file
            .segments
            .iter()
            .map(|s| (s.dir(&ds_root).join("delta.vocab"), s.dvocab_len))
            .collect();
        let dvocab = DeltaVocab::open_layered(&prefix, &gen_dir.join("delta.vocab"), read_only)?;
        let mut g = Generation::open_with(&base_dir, name, dvocab)?;
        g.dir = Some(gen_dir.to_path_buf());
        let mut delta = Delta::default();
        for s in &file.segments {
            // A child of a relinked branch inherits its immutable base overlay as
            // well as its log. The layered vocabulary keeps the same prefix IDs.
            if let Some(inherited) = read_link(&s.dir(&ds_root))?
                && let Some(overlay) = inherited.overlay
            {
                apply_overlay(&s.dir(&ds_root), &overlay, &mut g, cache, &mut delta)?;
            }
            let path = s.dir(&ds_root).join("wal.log");
            let from = wal::WalPoint {
                seq: s.base_seq,
                offset: 0,
                folding: false,
            };
            let to = wal::WalPoint {
                seq: s.end_seq,
                offset: s.wal_end,
                folding: false,
            };
            wal::apply_forward(&path, &g, cache, &mut delta, from, to, &mut |_| Ok(()))?;
        }
        if let Some(overlay) = &file.overlay {
            apply_overlay(gen_dir, overlay, &mut g, cache, &mut delta)?;
        }
        g.link = Some(Arc::new(Linked {
            file,
            base_delta: delta,
            base_dir,
        }));
        Ok(g)
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

fn apply_overlay(
    dir: &Path,
    overlay: &Overlay,
    generation: &mut Generation,
    cache: &BlockCache,
    delta: &mut Delta,
) -> Result<()> {
    let bytes = checked_overlay(dir, overlay)?;
    for rec in bytes.as_chunks::<WAL_REC>().0 {
        let q = wal::record_quad(rec);
        let in_base = generation
            .perm(Perm::Spo)
            .contains(cache, &Perm::Spo.to_key(&q))?;
        apply(delta, &q, rec[0] == WAL_INSERT, in_base);
    }
    generation.meta.next_bnode = generation.meta.next_bnode.max(overlay.next_bnode);
    Ok(())
}

fn checked_overlay(dir: &Path, overlay: &Overlay) -> Result<Vec<u8>> {
    let path = dir.join(OVERLAY_FILE);
    let bytes = std::fs::read(&path)?;
    if bytes.len() as u64 != overlay.bytes
        || !bytes.len().is_multiple_of(WAL_REC)
        || overlay_checksum(&bytes) != overlay.sha256
    {
        return Err(Error::Corrupt(format!(
            "{}: invalid base overlay length or checksum",
            path.display()
        )));
    }
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
