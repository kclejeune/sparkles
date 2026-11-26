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
        let result = (|| -> Result<cold::Loaded<Generation>> {
            let file = loaded.value;
            if C::ACCOUNTS && !read_only {
                return Err(Error::invalid(
                    "accounting linked generation opens are read-only",
                ));
            }
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
                let vocab = control.open_dvocab_layered(
                    &prefix,
                    &gen_dir.join("delta.vocab"),
                    read_only,
                )?;
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
        })();
        if result.is_err() && C::ACCOUNTS {
            // `loaded` and every intermediate structure allocated inside the closure
            // have already been dropped; restore the pre-link budget baseline.
            control.release(control.used() - start);
        }
        result
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
                control.check(0)?;
                let mutation = history_limited::preparation_mutation_bytes(delta);
                let extra = mutation.saturating_sub(*delta_charge);
                control.check(extra)?;
                generation.perm(Perm::Spo).contains_with(
                    cache,
                    &Perm::Spo.to_key(&quad),
                    &*control,
                    extra,
                )?
            } else {
                generation
                    .perm(Perm::Spo)
                    .contains(cache, &Perm::Spo.to_key(&quad))?
            };
            apply(delta, &quad, record[0] == WAL_INSERT, in_base);
            if C::ACCOUNTS {
                let next = history_limited::preparation_delta_bytes(delta);
                if next > *delta_charge {
                    control.charge(next - *delta_charge)?;
                } else if next < *delta_charge {
                    control.release(*delta_charge - next);
                }
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

    struct CountingControl(AtomicUsize);

    impl ReadControl for CountingControl {
        fn check_read(&self) -> Result<()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn temporary(&self, memory_bytes: u64) -> Result<crate::access::Preparation> {
            crate::access::Preparation::new(crate::task::Control::none(), memory_bytes)
        }
    }

    struct FailAfter {
        calls: AtomicUsize,
        limit: usize,
        timeout: bool,
    }

    impl ReadControl for FailAfter {
        fn check_read(&self) -> Result<()> {
            if self.calls.fetch_add(1, Ordering::Relaxed) >= self.limit {
                if self.timeout {
                    Err(Error::Timeout)
                } else {
                    Err(Error::Cancelled)
                }
            } else {
                Ok(())
            }
        }

        fn temporary(&self, memory_bytes: u64) -> Result<crate::access::Preparation> {
            crate::access::Preparation::new(crate::task::Control::none(), memory_bytes)
        }
    }

    fn delta_keys(delta: &Delta) -> (Vec<crate::index::Key>, Vec<crate::index::Key>) {
        (
            delta.ins[Perm::Spo.index()].iter().copied().collect(),
            delta.del[Perm::Spo.index()].iter().copied().collect(),
        )
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
                b"<urn:base> <urn:p> \"b\" .\n<urn:base-del> <urn:p> \"gone\" .\n".to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        store.create_branch("dev", &Default::default()).unwrap();
        let dev = store.branch("dev").unwrap();
        update(
            &dev,
            "INSERT DATA { <urn:dev> <urn:p> \"overlay\" . _:b0 <urn:p> \"bnode\" }",
            &Default::default(),
        )
        .unwrap();
        update(
            &dev,
            "DELETE DATA { <urn:base-del> <urn:p> \"gone\" }",
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
        let overlay = link.overlay.as_ref().expect("relinked overlay");
        assert!(overlay.next_bnode > 0);
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

        // Controlled open matches ordinary open on actual inserted/deleted keys,
        // next_bnode, and retained-byte accounting.
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
        let (plain_ins, plain_del) = delta_keys(&plain.base_delta());
        let (ctrl_ins, ctrl_del) = delta_keys(&controlled.value.base_delta());
        assert!(!plain_ins.is_empty());
        assert!(!plain_del.is_empty());
        assert_eq!(plain_ins, ctrl_ins);
        assert_eq!(plain_del, ctrl_del);
        assert!(plain.meta.next_bnode > 0);
        assert_eq!(plain.meta.next_bnode, controlled.value.meta.next_bnode);
        assert_eq!(controlled.charge, controlled.value.cold_bytes);

        // retained_bytes_with results and interruption behavior across applicable controls.
        assert_eq!(
            plain
                .retained_bytes_with(&crate::budgeted::Unlimited)
                .unwrap(),
            0
        );
        assert_eq!(
            controlled
                .value
                .retained_bytes_with(&crate::budgeted::Unlimited)
                .unwrap(),
            0
        );
        assert_eq!(
            controlled.value.retained_bytes_with(&limit).unwrap(),
            controlled.charge
        );
        let plain_retained = plain.retained_bytes_with(&limit).unwrap();
        assert!(plain_retained > 0);
        let exact_plain_limit = ReadLimit {
            options: &options,
            memory: Some(plain_retained),
            held: 0,
        };
        assert_eq!(
            plain.retained_bytes_with(&exact_plain_limit).unwrap(),
            plain_retained
        );
        let short_plain_limit = ReadLimit {
            options: &options,
            memory: Some(plain_retained - 1),
            held: 0,
        };
        assert!(matches!(
            plain.retained_bytes_with(&short_plain_limit),
            Err(Error::BudgetExceeded(_))
        ));

        let uncapped_ok = ReadLimit {
            options: &options,
            memory: None,
            held: 0,
        };
        assert_eq!(
            controlled.value.retained_bytes_with(&uncapped_ok).unwrap(),
            controlled.charge
        );
        assert_eq!(
            plain.retained_bytes_with(&uncapped_ok).unwrap(),
            plain_retained
        );

        let uncapped_cancel_opts = HistoryOptions {
            cancel: Some(Arc::new(AtomicBool::new(true))),
            deadline: None,
        };
        let uncapped_cancel = ReadLimit {
            options: &uncapped_cancel_opts,
            memory: None,
            held: 0,
        };
        assert!(matches!(
            controlled.value.retained_bytes_with(&uncapped_cancel),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            plain.retained_bytes_with(&uncapped_cancel),
            Err(Error::Cancelled)
        ));

        let retained_timeout = SwitchTimeout(AtomicBool::new(true));
        let uncapped_timeout = ReadLimit {
            options: &retained_timeout,
            memory: None,
            held: 0,
        };
        assert!(matches!(
            controlled.value.retained_bytes_with(&uncapped_timeout),
            Err(Error::Timeout)
        ));
        assert!(matches!(
            plain.retained_bytes_with(&uncapped_timeout),
            Err(Error::Timeout)
        ));

        let prep_ok = crate::access::Preparation::new(
            crate::task::Control::none(),
            plain_retained + (1 << 20),
        )
        .unwrap();
        assert_eq!(plain.retained_bytes_with(&prep_ok).unwrap(), plain_retained);
        let prep_short =
            crate::access::Preparation::new(crate::task::Control::none(), plain_retained - 1)
                .unwrap();
        assert!(matches!(
            plain.retained_bytes_with(&prep_short),
            Err(Error::BudgetExceeded(_))
        ));
        let prep_cancel_ctrl = crate::task::Control::none();
        let prep_cancel = prep_cancel_ctrl.cancel.clone();
        let prep_cancelled =
            crate::access::Preparation::new(prep_cancel_ctrl, plain_retained + (1 << 20)).unwrap();
        prep_cancel.cancel();
        assert!(matches!(
            plain.retained_bytes_with(&prep_cancelled),
            Err(Error::Cancelled)
        ));

        drop(controlled.value);
        budget.release(controlled.charge);
        assert_eq!(budget.used(), 0);

        // Error-boundary accounting: pre-link unrelated charges survive intact and no
        // link/intermediate charges leak on entry cancel, entry timeout, read_only=false,
        // or a mid-open failure inside overlay replay.
        let unrelated = 1234u64;

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
        cancel_budget.charge(unrelated).unwrap();
        let link_for_cancel = read_link_with(&gen_dir, &mut cancel_budget)
            .unwrap()
            .unwrap();
        assert!(cancel_budget.used() > unrelated);
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
        assert_eq!(cancel_budget.used(), unrelated);
        cancel_budget.release(unrelated);
        assert_eq!(cancel_budget.used(), 0);

        let timeout = SwitchTimeout(AtomicBool::new(false));
        let mut timeout_budget = ColdBudget::new(ReadLimit {
            options: &timeout,
            memory: None,
            held: 0,
        })
        .unwrap();
        timeout_budget.charge(unrelated).unwrap();
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
        assert_eq!(timeout_budget.used(), unrelated);
        timeout_budget.release(unrelated);
        assert_eq!(timeout_budget.used(), 0);

        // Accounting open_linked_with refuses read_only=false at its entry point and
        // restores the pre-link baseline.
        let mut ro_budget = ColdBudget::new(limit).unwrap();
        ro_budget.charge(unrelated).unwrap();
        let link_for_ro = read_link_with(&gen_dir, &mut ro_budget).unwrap().unwrap();
        assert!(matches!(
            Generation::open_linked_with(
                &gen_dir,
                "gen-0001",
                &branch_root,
                link_for_ro,
                false,
                &cache,
                &mut ro_budget,
            ),
            Err(Error::Invalid(_))
        ));
        assert_eq!(ro_budget.used(), unrelated);
        ro_budget.release(unrelated);
        assert_eq!(ro_budget.used(), 0);

        // Mid-open error inside overlay replay (after vocabulary, base, and WAL replay)
        // restores the pre-link baseline cleanly.
        let mut mid_budget = ColdBudget::new(limit).unwrap();
        mid_budget.charge(unrelated).unwrap();
        let mut link_for_mid = read_link_with(&gen_dir, &mut mid_budget).unwrap().unwrap();
        link_for_mid.value.overlay.as_mut().unwrap().bytes += 1;
        assert!(matches!(
            Generation::open_linked_with(
                &gen_dir,
                "gen-0001",
                &branch_root,
                link_for_mid,
                true,
                &cache,
                &mut mid_budget,
            ),
            Err(Error::Corrupt(_))
        ));
        assert_eq!(mid_budget.used(), unrelated);
        mid_budget.release(unrelated);
        assert_eq!(mid_budget.used(), 0);

        // Nonempty seed delta for overlay parity and error-accounting tests.
        let mut seed_delta = Delta::default();
        let seed_quad = [Id(999_001), Id(999_002), Id(999_003), Id::DEFAULT_GRAPH];
        apply(&mut seed_delta, &seed_quad, true, false);
        let seed_charge = history_limited::preparation_delta_bytes(&seed_delta);
        assert!(seed_charge > 0);
        let (seed_ins, seed_del) = delta_keys(&seed_delta);

        // Success parity with nonempty seed on the real relinked overlay (insertions,
        // deletion, and next_bnode advancement).
        let valid_overlay = file.overlay.as_ref().unwrap();
        let mut plain_seed_gen =
            Generation::open_sealed(&plain.link.as_ref().unwrap().base_dir, "gen-0001").unwrap();
        let mut ctrl_seed_gen =
            Generation::open_sealed(&plain.link.as_ref().unwrap().base_dir, "gen-0001").unwrap();
        let mut plain_seed_delta = seed_delta.clone();
        let mut plain_seed_charge = 0;
        apply_overlay_with(
            &gen_dir,
            valid_overlay,
            &mut plain_seed_gen,
            &cache,
            &mut plain_seed_delta,
            &mut plain_seed_charge,
            &mut crate::budgeted::Unlimited,
        )
        .unwrap();
        let mut ctrl_seed_budget = ColdBudget::new(limit).unwrap();
        ctrl_seed_budget.charge(unrelated).unwrap();
        ctrl_seed_budget.charge(seed_charge).unwrap();
        let mut ctrl_seed_delta = seed_delta.clone();
        let mut ctrl_seed_charge = seed_charge;
        apply_overlay_with(
            &gen_dir,
            valid_overlay,
            &mut ctrl_seed_gen,
            &cache,
            &mut ctrl_seed_delta,
            &mut ctrl_seed_charge,
            &mut ctrl_seed_budget,
        )
        .unwrap();
        assert_eq!(delta_keys(&plain_seed_delta), delta_keys(&ctrl_seed_delta));
        assert_eq!(
            plain_seed_gen.meta.next_bnode,
            ctrl_seed_gen.meta.next_bnode
        );
        assert_eq!(
            ctrl_seed_charge,
            history_limited::preparation_delta_bytes(&ctrl_seed_delta)
        );
        assert_eq!(ctrl_seed_budget.used(), unrelated + ctrl_seed_charge);
        ctrl_seed_budget.release(ctrl_seed_charge);
        ctrl_seed_budget.release(unrelated);
        assert_eq!(ctrl_seed_budget.used(), 0);

        // Craft a checksum-valid overlay whose first record is valid and second has a bad opcode.
        let overlay_dir = tempfile::tempdir().unwrap();
        let valid_raw = std::fs::read(gen_dir.join(OVERLAY_FILE)).unwrap();
        assert!(valid_raw.len() >= WAL_REC * 2);
        let mut bad_op_raw = valid_raw[..WAL_REC].to_vec();
        let mut bad_rec = [0u8; WAL_REC];
        bad_rec[0] = 0xFF;
        bad_op_raw.extend_from_slice(&bad_rec);
        let malformed = Overlay {
            bytes: bad_op_raw.len() as u64,
            sha256: overlay_checksum(&bad_op_raw),
            next_bnode: 10,
        };
        std::fs::write(overlay_dir.path().join(OVERLAY_FILE), &bad_op_raw).unwrap();

        // Ordinary apply_overlay_with prechecks opcodes before any index lookup or cache fill.
        let fresh_cache = BlockCache::new(1 << 20);
        let mut plain_gen =
            Generation::open_sealed(&plain.link.as_ref().unwrap().base_dir, "gen-0001").unwrap();
        let mut plain_delta = seed_delta.clone();
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
        assert_eq!(
            delta_keys(&plain_delta),
            (seed_ins.clone(), seed_del.clone())
        );
        assert_eq!(fresh_cache.entries(), 0);
        assert_eq!(fresh_cache.misses(), 0);

        // Controlled apply_overlay_with returns static path-free errors and preserves
        // live delta charges while releasing the overlay buffer.
        let dir_str = overlay_dir.path().display().to_string();
        let mut c_budget = ColdBudget::new(limit).unwrap();
        c_budget.charge(unrelated).unwrap();
        c_budget.charge(seed_charge).unwrap();
        let mut c_delta = seed_delta.clone();
        let mut c_charge = seed_charge;
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
        assert_eq!(c_charge, history_limited::preparation_delta_bytes(&c_delta));
        assert_eq!(c_budget.used(), unrelated + c_charge);
        c_budget.release(c_charge);
        c_budget.release(unrelated);
        assert_eq!(c_budget.used(), 0);

        // Bad length (with a VALID checksum) short-circuits before sha256, whereas
        // correct length + bad checksum executes sha256 and fails with the same error.
        let big_raw = vec![WAL_INSERT; WAL_REC * 3000];
        let big_sha = overlay_checksum(&big_raw);
        let mut bad_sha = big_sha.clone();
        let flipped = if bad_sha.ends_with('0') { '1' } else { '0' };
        bad_sha.pop();
        bad_sha.push(flipped);
        std::fs::write(overlay_dir.path().join(OVERLAY_FILE), &big_raw).unwrap();

        let bad_len = Overlay {
            bytes: big_raw.len() as u64 + 1,
            sha256: big_sha,
            next_bnode: 10,
        };
        let len_counter = CountingControl(AtomicUsize::new(0));
        let mut len_budget = ColdBudget::new(ReadLimit {
            options: &len_counter,
            memory: Some(16 << 20),
            held: 0,
        })
        .unwrap();
        len_budget.charge(unrelated).unwrap();
        len_budget.charge(seed_charge).unwrap();
        let mut len_delta = seed_delta.clone();
        let mut len_charge = seed_charge;
        let err = apply_overlay_with(
            overlay_dir.path(),
            &bad_len,
            &mut plain_gen,
            &fresh_cache,
            &mut len_delta,
            &mut len_charge,
            &mut len_budget,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            Error::Corrupt("invalid linked base overlay length or checksum".into()).to_string()
        );
        assert!(!err.to_string().contains(&dir_str));
        assert_eq!(len_charge, seed_charge);
        assert_eq!(len_budget.used(), unrelated + seed_charge);
        let bad_len_checks = len_counter.0.load(Ordering::Relaxed);
        len_budget.release(len_charge);
        len_budget.release(unrelated);
        assert_eq!(len_budget.used(), 0);

        let bad_checksum = Overlay {
            bytes: big_raw.len() as u64,
            sha256: bad_sha,
            next_bnode: 10,
        };
        let sha_counter = CountingControl(AtomicUsize::new(0));
        let mut bad_sha_budget = ColdBudget::new(ReadLimit {
            options: &sha_counter,
            memory: Some(16 << 20),
            held: 0,
        })
        .unwrap();
        bad_sha_budget.charge(unrelated).unwrap();
        bad_sha_budget.charge(seed_charge).unwrap();
        let mut bad_sha_delta = seed_delta.clone();
        let mut bad_sha_charge = seed_charge;
        let err = apply_overlay_with(
            overlay_dir.path(),
            &bad_checksum,
            &mut plain_gen,
            &fresh_cache,
            &mut bad_sha_delta,
            &mut bad_sha_charge,
            &mut bad_sha_budget,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            Error::Corrupt("invalid linked base overlay length or checksum".into()).to_string()
        );
        assert!(!err.to_string().contains(&dir_str));
        assert_eq!(bad_sha_charge, seed_charge);
        assert_eq!(bad_sha_budget.used(), unrelated + seed_charge);
        let bad_sha_checks = sha_counter.0.load(Ordering::Relaxed);
        // Proves sha256 chunk checks ran for bad_checksum and were skipped for bad_len.
        assert_eq!(
            bad_sha_checks,
            bad_len_checks + big_raw.len().div_ceil(64 << 10) + 1
        );
        bad_sha_budget.release(bad_sha_charge);
        bad_sha_budget.release(unrelated);
        assert_eq!(bad_sha_budget.used(), 0);
        // Ordinary path also refuses correct-length bad checksum with the path included.
        let mut plain_sha_delta = seed_delta.clone();
        let mut plain_sha_charge = 0;
        let err = apply_overlay_with(
            overlay_dir.path(),
            &bad_checksum,
            &mut plain_gen,
            &fresh_cache,
            &mut plain_sha_delta,
            &mut plain_sha_charge,
            &mut crate::budgeted::Unlimited,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Corrupt(_)));
        assert!(err.to_string().contains(&dir_str));

        // Cancellation inside read_verified_overlay_with (during sha256) drops the read buffer
        // and preserves the pre-charged nonempty seed without double-release.
        let big_overlay = Overlay {
            bytes: big_raw.len() as u64,
            sha256: overlay_checksum(&big_raw),
            next_bnode: 1,
        };
        let sha_ctrl = FailAfter {
            calls: AtomicUsize::new(0),
            limit: bad_len_checks + 1,
            timeout: false,
        };
        let mut sha_budget = ColdBudget::new(ReadLimit {
            options: &sha_ctrl,
            memory: Some(2 << 20),
            held: 0,
        })
        .unwrap();
        sha_budget.charge(unrelated).unwrap();
        sha_budget.charge(seed_charge).unwrap();
        let mut delta_cancel = seed_delta.clone();
        let mut charge_cancel = seed_charge;
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
        assert_eq!(charge_cancel, seed_charge);
        assert_eq!(sha_budget.used(), unrelated + charge_cancel);
        sha_budget.release(charge_cancel);
        sha_budget.release(unrelated);
        assert_eq!(sha_budget.used(), 0);

        // Capped failure at the record-0 mutation check preserves the still-live seed
        // delta charge instead of zeroing it.
        let two_rec_raw = &valid_raw[..WAL_REC * 2];
        let two_rec_overlay = Overlay {
            bytes: two_rec_raw.len() as u64,
            sha256: overlay_checksum(two_rec_raw),
            next_bnode: 10,
        };
        let one_rec_raw = &valid_raw[..WAL_REC];
        let one_rec_overlay = Overlay {
            bytes: one_rec_raw.len() as u64,
            sha256: overlay_checksum(one_rec_raw),
            next_bnode: 10,
        };
        std::fs::write(overlay_dir.path().join(OVERLAY_FILE), two_rec_raw).unwrap();
        let tight_cap = unrelated + seed_charge + (two_rec_raw.len() as u64) + 8192;
        assert!(tight_cap < unrelated + history_limited::preparation_mutation_bytes(&seed_delta));
        let mut tight_budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(tight_cap),
            held: 0,
        })
        .unwrap();
        tight_budget.charge(unrelated).unwrap();
        tight_budget.charge(seed_charge).unwrap();
        let mut tight_delta = seed_delta.clone();
        let mut tight_charge = seed_charge;
        assert!(matches!(
            apply_overlay_with(
                overlay_dir.path(),
                &two_rec_overlay,
                &mut plain_gen,
                &fresh_cache,
                &mut tight_delta,
                &mut tight_charge,
                &mut tight_budget,
            ),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(
            delta_keys(&tight_delta),
            (seed_ins.clone(), seed_del.clone())
        );
        assert_eq!(tight_charge, seed_charge);
        assert_eq!(tight_budget.used(), unrelated + seed_charge);
        tight_budget.release(tight_charge);
        tight_budget.release(unrelated);
        assert_eq!(tight_budget.used(), 0);

        // Measure exact check counts for verify and 1-record replay so uncapped
        // cancellation and timeout hit replay deterministically before and after
        // record 0 transfers its updated delta charge.
        let verify_counter = CountingControl(AtomicUsize::new(0));
        let mut verify_budget = ColdBudget::new(ReadLimit {
            options: &verify_counter,
            memory: None,
            held: 0,
        })
        .unwrap();
        let verified = read_verified_overlay_with(
            &overlay_dir.path().join(OVERLAY_FILE),
            &two_rec_overlay,
            &mut verify_budget,
        )
        .unwrap();
        verify_budget.drop_vec(verified);
        let verify_checks = verify_counter.0.load(Ordering::Relaxed);

        let one_rec_dir = tempfile::tempdir().unwrap();
        std::fs::write(one_rec_dir.path().join(OVERLAY_FILE), one_rec_raw).unwrap();
        let one_rec_counter = CountingControl(AtomicUsize::new(0));
        let mut one_rec_budget = ColdBudget::new(ReadLimit {
            options: &one_rec_counter,
            memory: None,
            held: 0,
        })
        .unwrap();
        one_rec_budget.charge(unrelated).unwrap();
        one_rec_budget.charge(seed_charge).unwrap();
        let mut one_rec_delta = seed_delta.clone();
        let mut one_rec_charge = seed_charge;
        apply_overlay_with(
            one_rec_dir.path(),
            &one_rec_overlay,
            &mut plain_gen,
            &fresh_cache,
            &mut one_rec_delta,
            &mut one_rec_charge,
            &mut one_rec_budget,
        )
        .unwrap();
        let one_rec_checks = one_rec_counter.0.load(Ordering::Relaxed);
        let expected_after_rec0 = delta_keys(&one_rec_delta);
        assert_ne!(expected_after_rec0, (seed_ins.clone(), seed_del.clone()));
        assert!(one_rec_charge > seed_charge);
        one_rec_budget.release(one_rec_charge);
        one_rec_budget.release(unrelated);
        assert_eq!(one_rec_budget.used(), 0);

        // Uncapped cancel and timeout reaching replay on record 0 (before apply) and on
        // record 1 (after record 0 transfers its updated delta charge).
        for timeout_mode in [false, true] {
            // Record 0: fails inside the replay loop before record 0 mutates delta.
            // ColdBudget::new consumes 1 check, unrelated+seed charges consume 2 checks,
            // read_verified_overlay_with consumes (verify_checks - 1) checks, so
            // (verify_checks + 2 + 1) fires inside record 0 after verify succeeds.
            let pre_apply_ctrl = FailAfter {
                calls: AtomicUsize::new(0),
                limit: verify_checks + 3,
                timeout: timeout_mode,
            };
            let mut replay_budget = ColdBudget::new(ReadLimit {
                options: &pre_apply_ctrl,
                memory: None,
                held: 0,
            })
            .unwrap();
            replay_budget.charge(unrelated).unwrap();
            replay_budget.charge(seed_charge).unwrap();
            let mut replay_delta = seed_delta.clone();
            let mut replay_charge = seed_charge;
            let err = apply_overlay_with(
                overlay_dir.path(),
                &two_rec_overlay,
                &mut plain_gen,
                &fresh_cache,
                &mut replay_delta,
                &mut replay_charge,
                &mut replay_budget,
            )
            .unwrap_err();
            if timeout_mode {
                assert!(matches!(err, Error::Timeout));
            } else {
                assert!(matches!(err, Error::Cancelled));
            }
            assert_eq!(
                delta_keys(&replay_delta),
                (seed_ins.clone(), seed_del.clone())
            );
            assert_eq!(replay_charge, seed_charge);
            assert_eq!(replay_budget.used(), unrelated + seed_charge);
            replay_budget.release(replay_charge);
            replay_budget.release(unrelated);
            assert_eq!(replay_budget.used(), 0);

            // Record 1: fails strictly after record 0 applied and updated delta_charge.
            // one_rec_checks includes the final post-loop check(0) of the 1-record run,
            // so (one_rec_checks - 1) is the exact check index at record 1's loop entry.
            let post_rec0_ctrl = FailAfter {
                calls: AtomicUsize::new(0),
                limit: one_rec_checks - 1,
                timeout: timeout_mode,
            };
            let mut post_budget = ColdBudget::new(ReadLimit {
                options: &post_rec0_ctrl,
                memory: None,
                held: 0,
            })
            .unwrap();
            post_budget.charge(unrelated).unwrap();
            post_budget.charge(seed_charge).unwrap();
            let mut post_delta = seed_delta.clone();
            let mut post_charge = seed_charge;
            let err = apply_overlay_with(
                overlay_dir.path(),
                &two_rec_overlay,
                &mut plain_gen,
                &fresh_cache,
                &mut post_delta,
                &mut post_charge,
                &mut post_budget,
            )
            .unwrap_err();
            if timeout_mode {
                assert!(matches!(err, Error::Timeout));
            } else {
                assert!(matches!(err, Error::Cancelled));
            }
            assert_eq!(delta_keys(&post_delta), expected_after_rec0);
            assert_eq!(post_charge, one_rec_charge);
            assert_eq!(post_budget.used(), unrelated + post_charge);
            drop(post_delta);
            post_budget.release(post_charge);
            post_budget.release(unrelated);
            assert_eq!(post_budget.used(), 0);
        }
    }
}
