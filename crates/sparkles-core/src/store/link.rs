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
    let loaded: cold::Loaded<LinkFile> = control.json(&bytes, path.display())?;
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
            delta_charge = 0;
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
                let next = history_limited::preparation_delta_bytes(&delta);
                control.charge(next)?;
                delta_charge = next;
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
    let result = (|| -> Result<()> {
        if !C::ACCOUNTS {
            for record in bytes.as_chunks::<WAL_REC>().0 {
                if !matches!(record[0], WAL_INSERT | WAL_DELETE) {
                    return Err(Error::Corrupt(format!(
                        "{}: invalid base overlay operation",
                        path.display()
                    )));
                }
            }
        }
        for record in bytes.as_chunks::<WAL_REC>().0 {
            control.check(0)?;
            if C::ACCOUNTS && !matches!(record[0], WAL_INSERT | WAL_DELETE) {
                return Err(Error::Corrupt(
                    "invalid linked base overlay operation".into(),
                ));
            }
            let quad = wal::record_quad(record);
            let in_base = if C::ACCOUNTS {
                control.release(*delta_charge);
                *delta_charge = 0;
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
                let next = history_limited::preparation_delta_bytes(delta);
                control.charge(next)?;
                *delta_charge = next;
            }
        }
        generation.meta.next_bnode = generation.meta.next_bnode.max(overlay.next_bnode);
        Ok(())
    })();
    control.drop_vec(bytes);
    result?;
    control.check(0)
}

fn read_verified_overlay_with<C: crate::budgeted::BudgetControl>(
    path: &Path,
    overlay: &Overlay,
    control: &mut C,
) -> Result<Vec<u8>> {
    let bytes = control.read_file(path)?;
    let valid = (|| -> Result<bool> {
        if bytes.len() as u64 != overlay.bytes || !bytes.len().is_multiple_of(WAL_REC) {
            return Ok(false);
        }
        let digest = control.sha256(&bytes)?;
        let hex = b"0123456789abcdef";
        Ok(overlay.sha256.len() == 64
            && digest.iter().enumerate().all(|(index, byte)| {
                overlay.sha256.as_bytes()[index * 2] == hex[(byte >> 4) as usize]
                    && overlay.sha256.as_bytes()[index * 2 + 1] == hex[(byte & 15) as usize]
            }))
    })();
    match valid {
        Ok(true) => Ok(bytes),
        Ok(false) => {
            control.drop_vec(bytes);
            Err(if C::ACCOUNTS {
                Error::Corrupt("invalid linked base overlay length or checksum".into())
            } else {
                Error::Corrupt(format!(
                    "{}: invalid base overlay length or checksum",
                    path.display()
                ))
            })
        }
        Err(error) => {
            control.drop_vec(bytes);
            Err(error)
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{HistoryOptions, ReadControl, ReadLimit};
    use crate::store::cold::ColdBudget;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct SwitchTimeout(AtomicBool);

    impl ReadControl for SwitchTimeout {
        fn check_read(&self) -> Result<()> {
            if self.0.load(Ordering::Relaxed) {
                Err(Error::Timeout)
            } else {
                Ok(())
            }
        }

        fn temporary(&self, memory_bytes: u64) -> Result<crate::access::Preparation> {
            crate::access::Preparation::new(crate::task::Control::none(), memory_bytes)
        }
    }

    fn linked_overlay_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, LinkFile) {
        use crate::{io::Source, sparql::update::update};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ds");
        let store = Store::open(
            &root,
            StoreOptions {
                build: crate::builder::BuildOptions {
                    threads: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        store
            .load(&[Source::from_bytes(
                b"<urn:base> <urn:p> \"b\" .\n".to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        store.create_branch("dev", &Default::default()).unwrap();
        let dev = store.branch("dev").unwrap();
        update(
            &dev,
            "INSERT DATA { <urn:dev> <urn:p> \"overlay\" }",
            &Default::default(),
        )
        .unwrap();
        update(
            &store,
            "INSERT DATA { <urn:main> <urn:p> \"compact\" }",
            &Default::default(),
        )
        .unwrap();
        store.compact().unwrap();
        store.relink_branch("dev", &Default::default()).unwrap();
        let gen_dir = dev.snapshot().generation.dir.clone().unwrap();
        let branch_root = gen_dir.parent().unwrap().to_path_buf();
        let link = read_link(&gen_dir).unwrap().expect("linked generation");
        assert!(link.overlay.is_some());
        drop(dev);
        drop(store);
        (dir, gen_dir, branch_root, link)
    }

    #[test]
    fn open_linked_and_overlay_preserve_precheck_path_free_errors_and_cleanup() {
        let (_dir, gen_dir, branch_root, file) = linked_overlay_fixture();
        let cache = Arc::new(BlockCache::new(1 << 20));
        let plain = Generation::open_linked(
            &gen_dir,
            "gen-0001",
            &branch_root,
            file.clone(),
            true,
            &cache,
        )
        .unwrap();

        // Controlled open matches ordinary open and accounts for retained bytes.
        let options = HistoryOptions::default();
        let limit = ReadLimit {
            options: &options,
            memory: Some(16 << 20),
            held: 0,
        };
        let mut budget = ColdBudget::new(limit).unwrap();
        let loaded_link = read_link_with(&gen_dir, &mut budget).unwrap().unwrap();
        let controlled = Generation::open_linked_with(
            &gen_dir,
            "gen-0001",
            &branch_root,
            loaded_link,
            true,
            &cache,
            &mut budget,
        )
        .unwrap();
        assert_eq!(
            plain.base_delta().ins[Perm::Spo.index()].len(),
            controlled.value.base_delta().ins[Perm::Spo.index()].len()
        );
        assert_eq!(controlled.charge, controlled.value.cold_bytes);
        assert_eq!(
            controlled.value.retained_bytes_with(&limit).unwrap(),
            controlled.charge
        );
        assert!(plain.retained_bytes_with(&limit).unwrap() > 0);
        drop(controlled.value);
        budget.release(controlled.charge);
        assert_eq!(budget.used(), 0);

        // Uncapped cancel and deterministic timeout stop open_linked_with cleanly.
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_opts = HistoryOptions {
            cancel: Some(cancel.clone()),
            deadline: None,
        };
        let mut cancel_budget = ColdBudget::new(ReadLimit {
            options: &cancel_opts,
            memory: None,
            held: 0,
        })
        .unwrap();
        let link_for_cancel = read_link_with(&gen_dir, &mut cancel_budget)
            .unwrap()
            .unwrap();
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            Generation::open_linked_with(
                &gen_dir,
                "gen-0001",
                &branch_root,
                link_for_cancel,
                true,
                &cache,
                &mut cancel_budget,
            ),
            Err(Error::Cancelled)
        ));

        let timeout = SwitchTimeout(AtomicBool::new(false));
        let mut timeout_budget = ColdBudget::new(ReadLimit {
            options: &timeout,
            memory: None,
            held: 0,
        })
        .unwrap();
        let link_for_timeout = read_link_with(&gen_dir, &mut timeout_budget)
            .unwrap()
            .unwrap();
        timeout.0.store(true, Ordering::Relaxed);
        assert!(matches!(
            Generation::open_linked_with(
                &gen_dir,
                "gen-0001",
                &branch_root,
                link_for_timeout,
                true,
                &cache,
                &mut timeout_budget,
            ),
            Err(Error::Timeout)
        ));

        // Craft a checksum-valid overlay whose first record is valid and second has a bad opcode.
        let overlay_dir = tempfile::tempdir().unwrap();
        let mut raw = std::fs::read(gen_dir.join(OVERLAY_FILE)).unwrap();
        assert!(raw.len() >= WAL_REC);
        let mut bad_rec = [0u8; WAL_REC];
        bad_rec[0] = 0xFF;
        raw.extend_from_slice(&bad_rec);
        let malformed = Overlay {
            bytes: raw.len() as u64,
            sha256: overlay_checksum(&raw),
            next_bnode: 10,
        };
        std::fs::write(overlay_dir.path().join(OVERLAY_FILE), &raw).unwrap();

        // Ordinary apply_overlay_with prechecks opcodes before any index lookup or cache fill.
        let fresh_cache = BlockCache::new(1 << 20);
        let mut plain_gen =
            Generation::open_sealed(&plain.link.as_ref().unwrap().base_dir, "gen-0001").unwrap();
        let mut plain_delta = Delta::default();
        let mut plain_charge = 0;
        let err = apply_overlay_with(
            overlay_dir.path(),
            &malformed,
            &mut plain_gen,
            &fresh_cache,
            &mut plain_delta,
            &mut plain_charge,
            &mut crate::budgeted::Unlimited,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Corrupt(_)));
        assert!(err.to_string().contains("invalid base overlay operation"));
        assert!(plain_delta.ins[Perm::Spo.index()].is_empty());
        assert_eq!(fresh_cache.entries(), 0);
        assert_eq!(fresh_cache.misses(), 0);

        // Controlled apply_overlay_with returns static path-free errors and releases buffer & delta charges.
        let dir_str = overlay_dir.path().display().to_string();
        let mut c_budget = ColdBudget::new(limit).unwrap();
        let mut c_delta = Delta::default();
        let mut c_charge = 0;
        let err = apply_overlay_with(
            overlay_dir.path(),
            &malformed,
            &mut plain_gen,
            &fresh_cache,
            &mut c_delta,
            &mut c_charge,
            &mut c_budget,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            Error::Corrupt("invalid linked base overlay operation".into()).to_string()
        );
        assert!(!err.to_string().contains(&dir_str));
        // Only the admitted first-record delta remains charged; the overlay buffer was dropped.
        assert_eq!(c_budget.used(), c_charge);
        c_budget.release(c_charge);
        assert_eq!(c_budget.used(), 0);

        // Bad length short-circuits cleanly, is path-free under ColdBudget, and leaves budget.used() == 0.
        let bad_len = Overlay {
            bytes: raw.len() as u64 + 1,
            sha256: "not-even-hex".into(),
            next_bnode: 10,
        };
        let mut len_delta = Delta::default();
        let mut len_charge = 0;
        let err = apply_overlay_with(
            overlay_dir.path(),
            &bad_len,
            &mut plain_gen,
            &fresh_cache,
            &mut len_delta,
            &mut len_charge,
            &mut c_budget,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            Error::Corrupt("invalid linked base overlay length or checksum".into()).to_string()
        );
        assert!(!err.to_string().contains(&dir_str));
        assert_eq!(c_budget.used(), 0);

        // Cancellation inside read_verified_overlay_with (during sha256) drops the read buffer,
        // and pre-charged *delta_charge is preserved without double-release.
        struct CancelAfter(std::sync::atomic::AtomicUsize, usize);
        impl ReadControl for CancelAfter {
            fn check_read(&self) -> Result<()> {
                let n = self.0.fetch_add(1, Ordering::Relaxed);
                if n >= self.1 {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            }

            fn temporary(&self, memory_bytes: u64) -> Result<crate::access::Preparation> {
                crate::access::Preparation::new(crate::task::Control::none(), memory_bytes)
            }
        }
        let big_raw = vec![WAL_INSERT; WAL_REC * 3000];
        let big_overlay = Overlay {
            bytes: big_raw.len() as u64,
            sha256: overlay_checksum(&big_raw),
            next_bnode: 1,
        };
        std::fs::write(overlay_dir.path().join(OVERLAY_FILE), &big_raw).unwrap();
        // ColdBudget::new + read_file checks run first; cancel during sha256 chunk loop.
        let sha_ctrl = CancelAfter(std::sync::atomic::AtomicUsize::new(0), 10);
        let mut sha_budget = ColdBudget::new(ReadLimit {
            options: &sha_ctrl,
            memory: Some(2 << 20),
            held: 0,
        })
        .unwrap();
        sha_budget.charge(64).unwrap();
        let mut delta_cancel = Delta::default();
        let mut charge_cancel = 64;
        assert!(matches!(
            apply_overlay_with(
                overlay_dir.path(),
                &big_overlay,
                &mut plain_gen,
                &fresh_cache,
                &mut delta_cancel,
                &mut charge_cancel,
                &mut sha_budget,
            ),
            Err(Error::Cancelled)
        ));
        assert_eq!(sha_budget.used(), charge_cancel);
        sha_budget.release(charge_cancel);
        assert_eq!(sha_budget.used(), 0);
    }
}
