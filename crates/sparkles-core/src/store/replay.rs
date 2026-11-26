//! Replayed fast-forwards (`ff: "replay"`): a merge whose target holds the state of the
//! merge base replays the source's commits one by one, each as its own commit on the
//! target with the original's kind, message and author. Each replayed commit records the
//! source commit it replays as its second parent, flagged as a replay, so the target
//! descends from every commit replayed so far, also when a replay stops part way.

use super::branching::{BranchSet, MERGE_REC, MERGE_REPLAYED, MergeRec};
use super::changelog::LogCommit;
use super::diff::{DiffOp, DiffOptions, QuadKey};
use super::merge::{INFERRED_GRAPH, quad_ids};
use super::*;
use crate::branch::{self, CommitRef, MergeOptions, MergeOutcome, MergeReport, ReplayedCommit};
use crate::history::At;

struct FileProjection {
    end: u64,
    alloc: u64,
}

impl FileProjection {
    /// Carry the file's new logical end and tail into the next step, returning its
    /// growth on disk.
    fn append(&mut self, bytes: u64, prealloc: u64) -> u64 {
        self.end += bytes;
        let alloc = wal::allocation_after_write(self.alloc, self.end, prealloc);
        let growth = alloc - self.alloc;
        self.alloc = alloc;
        growth
    }
}

impl Store {
    /// The author the change log recorded for commit `seq` (`None` when it recorded
    /// none, or the log does not hold the commit).
    pub fn commit_author(&self, seq: u64) -> Option<Arc<str>> {
        self.commit_record(seq).and_then(|c| c.author)
    }

    /// The change log's record of commit `seq`, with its time, author and message
    /// (`None` when the log does not hold the commit).
    pub fn commit_record(&self, seq: u64) -> Option<changelog::ChangeCommit> {
        let log = self.changelog.as_ref().filter(|l| l.is_enabled())?;
        log.flush(false).ok()?;
        let mut record = None;
        log.commits(seq, seq, &mut || Ok(()), &mut |c| {
            if let LogCommit::Changes(c, _) | LogCommit::Summary(c) = c {
                record = Some(changelog::ChangeCommit::clone(&c));
            }
            Ok(false)
        })
        .ok()?;
        record
    }

    /// Replay the commits of the source's first-parent chain after `base` onto the
    /// target, whose head `tc` holds the state of `base`. `report` describes the merge
    /// so far.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn replay(
        &self,
        set: &BranchSet,
        tgt: &Store,
        mut report: MergeReport,
        base: CommitRef,
        sc: CommitRef,
        tc: CommitRef,
        o: &MergeOptions,
        preview: bool,
    ) -> Result<MergeOutcome> {
        let target = report.target.branch.clone().unwrap_or_default();
        // the source's own history must pass through the base
        let chain = set.chain(sc);
        let Some(j) = chain
            .iter()
            .enumerate()
            .position(|(i, (id, after, through))| {
                *id == base.branch_id && base.seq <= *through && (base.seq > *after || i == 0)
            })
        else {
            return Err(branch::conflict(
                "cannot-replay",
                "the merge base is not in the source's own history; merge without replay",
            ));
        };
        let dopts = DiffOptions {
            max_quads: o.max_quads,
            cancel: o.cancel.clone(),
            deadline: o.deadline,
            ..Default::default()
        };
        // the target must hold the base's state: nothing of its own to merge with
        if base != tc && !self.toggles(set, base, tc, &dopts)?.is_empty() {
            return Err(branch::conflict(
                "not-fast-forward",
                format!(
                    "{target} has changes of its own since the merge base; replay needs a fast-forward"
                ),
            ));
        }
        let mut commits: Vec<(uuid::Uuid, u64)> = Vec::new();
        for (k, &(bid, after, through)) in chain.iter().enumerate().skip(j) {
            let lo = if k == j { base.seq } else { after };
            commits.extend((lo + 1..=through).map(|s| (bid, s)));
        }
        if !preview && let Some(dry) = &o.write.dry_run {
            return self.preview_replay(tgt, &commits, &report, o, &dopts, dry);
        }
        let inferred: Arc<[u8]> = crate::id::iri_key(INFERRED_GRAPH).into();
        let mut excluded = 0u64;
        let mut expected = report.target.seq;
        let n = commits.len();
        for (i, (bid, seq)) in commits.into_iter().enumerate() {
            o.progress.report(
                i as f32 / n.max(1) as f32,
                &format!("replaying commit {seq} ({} of {n})", i + 1),
            );
            if let Some(c) = &o.cancel
                && c.load(Ordering::Relaxed)
            {
                return Err(Error::Cancelled);
            }
            let from = set.named(CommitRef {
                branch_id: bid,
                seq,
            });
            let src = self.branch_by_id(bid)?;
            let d = src
                .diff(&At::Commit(seq - 1), &At::Commit(seq), &dopts)
                .map_err(super::merge::gone)?;
            let mut changes: Vec<(QuadKey, bool)> = Vec::new();
            for (op, k) in d.keys() {
                if !o.include_inferences && k[0] == inferred {
                    excluded += 1;
                    continue;
                }
                changes.push((k.clone(), op == DiffOp::Add));
            }
            let ins = changes.iter().filter(|c| c.1).count() as u64;
            report.inserted += ins;
            report.deleted += changes.len() as u64 - ins;
            // a replay is refused at the first commit that changes a managed graph
            tgt.admit_managed_keys(changes.iter().map(|(k, _)| &*k[0]))?;
            if preview {
                report.replayed.push(ReplayedCommit {
                    from,
                    receipt: None,
                });
                continue;
            }
            let info = src.commit(seq);
            let mut wo = o.write.clone();
            wo.message = src.annotation(seq).and_then(|a| a.message);
            wo.author = src.commit_author(seq).or(wo.author);
            wo.cancel = wo.cancel.or_else(|| o.cancel.clone());
            wo.deadline = wo.deadline.or(o.deadline);
            let kind = info.map_or(CommitKind::Transaction, |c| c.kind);
            let mut txn = tgt.try_write_with(kind, wo)?;
            if txn.guard.head.seq != expected {
                let done = report.replayed.len();
                return Err(branch::conflict(
                    "head-moved",
                    format!(
                        "{target} moved during the replay: {done} of {n} commits were replayed; merge again"
                    ),
                ));
            }
            txn.force = true;
            txn.merge = Some(MergeRec {
                seq: 0,
                source: set.normalize(CommitRef {
                    branch_id: bid,
                    seq,
                }),
                resolved: 0,
                flags: MERGE_REPLAYED,
            });
            // deletions first, each in key order, as a merge writes them
            changes.sort_unstable_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
            for (k, insert) in &changes {
                match quad_ids(&mut txn, k, *insert)? {
                    Some(q) if *insert => {
                        txn.insert(q)?;
                    }
                    Some(q) => {
                        txn.delete(q)?;
                    }
                    None => {}
                }
            }
            let receipt = txn.commit()?;
            expected = receipt.commit.seq;
            report.commit = Some(receipt.clone());
            report.replayed.push(ReplayedCommit {
                from,
                receipt: Some(receipt),
            });
        }
        o.progress.report(1.0, "replayed");
        report.merged = !preview;
        if !o.include_inferences {
            report.inferences_excluded = Some(excluded);
        }
        Ok(MergeOutcome::Merged(report))
    }

    /// Simulate every replayed commit in one rollback transaction. Guards see each
    /// step's own predecessor and changes, while the retained preview covers the
    /// complete proposed state. Nothing publishes a commit or guard-cache update.
    fn preview_replay(
        &self,
        target: &Store,
        commits: &[(uuid::Uuid, u64)],
        report: &MergeReport,
        options: &MergeOptions,
        diff_options: &DiffOptions,
        dry: &crate::preview::DryRun,
    ) -> Result<MergeOutcome> {
        let mut write = options.write.clone();
        write.cancel = write.cancel.or_else(|| options.cancel.clone());
        write.deadline = write.deadline.or(options.deadline);
        let mut txn = target.try_write_with(CommitKind::Merge, write)?;
        if txn.guard.head.seq != report.target.seq {
            return Err(branch::conflict(
                "head-moved",
                "target moved before replay preview",
            ));
        }
        let inferred: Arc<[u8]> = crate::id::iri_key(INFERRED_GRAPH).into();
        let mut terms_from = txn.mark.entries();
        let mut wal_file = FileProjection {
            end: txn.guard.wal_len,
            alloc: txn.guard.tail.alloc(),
        };
        let mut vocab_file = FileProjection {
            end: txn.mark.file_end(),
            alloc: txn.base.generation.dvocab.allocated(),
        };
        let (mut annotation_header, mut annotation_file) = {
            let annotations = target.annotations.lock();
            let (end, alloc) = annotations.file_lengths();
            (annotations.pending_header(), FileProjection { end, alloc })
        };
        let mut fenced_quota = 0u64;
        let mut fenced_disk = 0u64;
        for (index, &(branch_id, seq)) in commits.iter().enumerate() {
            txn.opts.check()?;
            options.progress.report(
                index as f32 / commits.len().max(1) as f32,
                "previewing replay",
            );
            let source = self.branch_by_id(branch_id)?;
            let changes = source
                .diff(&At::Commit(seq - 1), &At::Commit(seq), diff_options)
                .map_err(super::merge::gone)?;
            let mut before = txn.view();
            before.commit = report.target.seq + index as u64;
            let before = Arc::new(before);
            txn.kind = source
                .commit(seq)
                .map_or(CommitKind::Transaction, |commit| commit.kind);
            txn.opts.message = source
                .annotation(seq)
                .and_then(|annotation| annotation.message);
            txn.opts.author = source
                .commit_author(seq)
                .or_else(|| options.write.author.clone());
            txn.force = true;
            txn.merge = Some(MergeRec {
                seq: 0,
                source: CommitRef { branch_id, seq },
                resolved: 0,
                flags: MERGE_REPLAYED,
            });
            txn.writable.clear();
            txn.base_check = match txn
                .opts
                .graphs
                .as_ref()
                .and_then(|access| access.triples.as_ref())
            {
                Some(rules) => crate::access::triples::WriteCheck::new(rules, before.clone())?,
                None => None,
            };
            let start = txn.log.len();
            for (operation, key) in changes.keys() {
                if !options.include_inferences && key[0] == inferred {
                    continue;
                }
                let added = operation == DiffOp::Add;
                match quad_ids(&mut txn, key, added)? {
                    Some(quad) if added => {
                        txn.insert(quad)?;
                    }
                    Some(quad) => {
                        txn.delete(quad)?;
                    }
                    None => {}
                }
            }
            txn.check_requested_all()?;
            target.admit_managed_log(&before, &txn.log[start..], None)?;
            txn.opts.check()?;
            let precondition = super::preview::precondition_on(&before, &txn.opts);
            let validation = match target.run_guard(
                &before,
                || {
                    let mut view = txn.view();
                    view.commit = before.commit;
                    Arc::new(view)
                },
                txn.kind,
                crate::guard::Changes::Log(&txn.log[start..]),
                &txn.opts,
                before.commit,
            ) {
                Ok(summary) => summary,
                Err(Error::Rejected(rejected)) => Some(Arc::new(rejected.summary)),
                Err(error) => return Err(error),
            };
            let inserts = txn.log[start..]
                .iter()
                .any(|(operation, _)| *operation == WAL_INSERT);
            // Only this step's WAL records plus its own marker, even on a zero-net step.
            let wal_bytes = txn.wal_bytes() - start as u64 * WAL_REC as u64;
            let (vocab_bytes, annotation) =
                txn.pending_storage_bytes(terms_from, txn.opts.message.as_deref());
            let pending = wal_bytes
                .saturating_add(vocab_bytes)
                .saturating_add(annotation);
            let storage = txn.preview_storage(
                inserts,
                fenced_quota.saturating_add(pending),
                fenced_disk.saturating_add(pending),
            );
            let refused = precondition
                .as_ref()
                .is_some_and(|outcome| outcome.is_err())
                || validation
                    .as_ref()
                    .is_some_and(|summary| summary.status == crate::guard::GuardStatus::Rejected)
                || storage.refused.is_some();
            if refused || index + 1 == commits.len() {
                let checks = super::preview::LogChecks {
                    precondition,
                    validation,
                    storage,
                };
                let mut preview = txn.preview_log_checked(dry, checks)?;
                if let Some(commit) = &mut preview.commit {
                    commit.seq = before.commit + 1;
                }
                return Err(Error::DryRun(Box::new(preview)));
            }
            if target.root.is_some() {
                // The next step sees the metadata records and the framed vocabulary.
                let vocab = &txn.base.generation.dvocab;
                let framing = vocab.framing_bytes(terms_from, vocab_file.end == 0);
                let header = if annotation > 0 {
                    std::mem::take(&mut annotation_header)
                } else {
                    0
                };
                let before_fence = MERGE_REC as u64 + header;
                let metadata = before_fence + commit::REC as u64;
                fenced_quota = fenced_quota
                    .saturating_add(pending)
                    .saturating_add(framing)
                    .saturating_add(metadata);
                // The merge record and any annotation header exist before the fence
                // checks its promise. The promise also covers the earlier WAL-only
                // preallocation check, and the catalog is appended after the fence.
                let cap = target.opts.wal_prealloc_bytes;
                let prealloc = if cap > 0
                    && target
                        .check_disk(
                            fenced_disk
                                .saturating_add(before_fence)
                                .saturating_add(pending)
                                .saturating_add(target.quota.unwritten())
                                .saturating_add(group::preallocation_promise(cap, annotation > 0)),
                        )
                        .is_ok()
                {
                    cap
                } else {
                    0
                };
                let annotation_prealloc = if prealloc > 0 {
                    crate::annotations::MAX_PREALLOC
                } else {
                    0
                };
                annotation_file.append(header, 0);
                fenced_disk = fenced_disk
                    .saturating_add(metadata)
                    .saturating_add(wal_file.append(wal_bytes, prealloc))
                    .saturating_add(vocab_file.append(
                        vocab_bytes.saturating_add(framing),
                        prealloc.min(crate::vocab::delta::MAX_PREALLOC),
                    ))
                    .saturating_add(annotation_file.append(annotation, annotation_prealloc));
                terms_from = vocab.len();
            }
        }
        let mut unchanged = report.clone();
        unchanged.up_to_date = true;
        Ok(MergeOutcome::UpToDate(unchanged))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::{DryRun, Outcome, Preview};
    use crate::sparql::update::update;
    use std::fs;

    fn target(root: &Path, opts: StoreOptions) -> Store {
        let main = Store::open(root, opts).unwrap();
        update(
            &main,
            "INSERT DATA {<urn:base> <urn:p> 0}",
            &Default::default(),
        )
        .unwrap();
        main.create_branch("dev", &Default::default()).unwrap();
        main
    }

    fn replay() -> MergeOptions {
        MergeOptions {
            replay: true,
            ..Default::default()
        }
    }

    fn preview(main: &Store) -> Preview {
        let mut opts = replay();
        opts.write.dry_run = Some(DryRun::default());
        crate::preview::catch(main.merge("dev", "main", &opts)).unwrap()
    }

    #[test]
    fn replay_preview_quota_matches_a_refresh_between_steps() {
        let dir = tempfile::tempdir().unwrap();
        let main = Arc::new(target(
            dir.path(),
            StoreOptions {
                change_log: false,
                wal_prealloc_bytes: 0,
                ..Default::default()
            },
        ));
        let dev = main.branch("dev").unwrap();
        update(
            &dev,
            "INSERT DATA {<urn:new> <urn:p> 1}",
            &crate::sparql::QueryOptions {
                write: crate::guard::WriteOptions {
                    message: Some("first replay step".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        main.set_quota(Some(u64::MAX)).unwrap();
        main.set_quota(Some(preview(&main).storage.projected.unwrap() + 1024))
            .unwrap();
        main.quota.remeasure();
        main.quota.used();
        let quota_file = dir.path().join(quota::QUOTA_FILE);
        let quota_file_len = fs::metadata(&quota_file).unwrap().len();
        let measures = main.quota.measures();
        // This ordinary source commit appends to the shared catalog after the walk.
        update(
            &dev,
            "INSERT DATA {<urn:base> <urn:p> 2}",
            &Default::default(),
        )
        .unwrap();
        let limit = preview(&main).storage.projected.unwrap();
        main.set_quota(Some(limit - 1)).unwrap();
        assert_eq!(preview(&main).outcome(), Outcome::StorageRefused);
        main.set_quota(Some(limit)).unwrap();
        assert_eq!(fs::metadata(quota_file).unwrap().len(), quota_file_len);
        let candidate = preview(&main);
        assert_eq!(candidate.storage.projected, Some(limit));
        assert_eq!(candidate.outcome(), Outcome::Commit);
        assert_eq!(main.quota.measures(), measures);

        let head = main.snapshot().commit;
        let refreshed = Arc::new(parking_lot::Mutex::new(None));
        let opts = MergeOptions {
            progress: crate::task::Progress::new({
                let main = main.clone();
                let refreshed = refreshed.clone();
                move |fraction, message| {
                    if fraction == 0.5 && message.starts_with("replaying commit ") {
                        assert_eq!(main.snapshot().commit, head + 1);
                        let cached = main.quota.used();
                        main.quota.remeasure();
                        *refreshed.lock() = Some((cached, main.quota.used()));
                    }
                }
            }),
            ..replay()
        };
        main.merge("dev", "main", &opts).unwrap();
        let (cached, measured) = refreshed.lock().unwrap();
        assert_eq!(cached, measured);
        assert_eq!(main.snapshot().commit, head + 2);
    }

    #[test]
    fn replay_preview_reserve_uses_existing_wal_and_vocabulary_tails() {
        const RESERVE: u64 = 1 << 20;
        let dir = tempfile::tempdir().unwrap();
        let main = target(
            dir.path(),
            StoreOptions {
                change_log: false,
                min_free_disk_bytes: Some(RESERVE),
                ..Default::default()
            },
        );
        let dev = main.branch("dev").unwrap();
        update(
            &dev,
            "INSERT DATA {<urn:new-vocabulary-term-that-fits-in-the-preallocated-vocabulary-tail> <urn:p> 1}",
            &Default::default(),
        )
        .unwrap();
        main.set_quota(Some(u64::MAX)).unwrap();
        let first = preview(&main);
        let first_pending = first.storage.projected.unwrap() - first.storage.used.unwrap();
        let wal_before = dev.wal_bytes();
        update(
            &dev,
            "INSERT DATA {<urn:base> <urn:p> 2}",
            &Default::default(),
        )
        .unwrap();
        let second_pending = dev.wal_bytes() - wal_before;
        let headroom = first_pending + second_pending;
        let vocab = &main.snapshot().generation.dvocab;
        let wal_alloc = main.writer.lock().tail.alloc();
        let vocab_alloc = vocab.allocated();
        assert!(wal_alloc - main.wal_bytes() >= headroom);
        assert!(vocab.preallocated() >= headroom);
        let _disk =
            crate::disk::fake::install(dir.path(), dir_size(dir.path()) + RESERVE + headroom);

        let candidate = preview(&main);
        main.merge("dev", "main", &replay()).unwrap();
        assert_eq!(main.writer.lock().tail.alloc(), wal_alloc);
        assert_eq!(vocab.allocated(), vocab_alloc);
        assert_eq!(
            candidate.outcome(),
            Outcome::Commit,
            "{:?}",
            candidate.storage.refused
        );
    }

    #[test]
    fn replay_preview_reserve_counts_a_predecessors_new_tail() {
        const RESERVE: u64 = 1 << 20;
        const HEADROOM: u64 = 512 << 10;
        let dir = tempfile::tempdir().unwrap();
        let main = target(
            dir.path(),
            StoreOptions {
                change_log: false,
                wal_prealloc_bytes: 16 << 10,
                min_free_disk_bytes: Some(RESERVE),
                ..Default::default()
            },
        );
        let dev = main.branch("dev").unwrap();
        let first = (1..=64)
            .map(|n| format!("<urn:base> <urn:p> {n} ."))
            .collect::<Vec<_>>()
            .join(" ");
        update(
            &dev,
            &format!("INSERT DATA {{ {first} }}"),
            &crate::sparql::QueryOptions {
                write: crate::guard::WriteOptions {
                    message: Some("first replay step".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        update(
            &dev,
            &format!(
                "INSERT DATA {{<urn:base> <urn:p> \"{}\"}}",
                "x".repeat(511 * 1024 - 128)
            ),
            &Default::default(),
        )
        .unwrap();
        let wal_alloc = main.writer.lock().tail.alloc();
        assert!(wal_alloc - main.wal_bytes() >= 65 * WAL_REC as u64);
        assert!(main.annotations.lock().pending_header() > 0);
        let _disk =
            crate::disk::fake::install(dir.path(), dir_size(dir.path()) + RESERVE + HEADROOM);

        let head = main.snapshot().commit;
        let candidate = preview(&main);
        assert_eq!(main.snapshot().commit, head);
        assert!(matches!(
            main.merge("dev", "main", &replay()),
            Err(Error::StorageFull(_))
        ));
        assert_eq!(main.snapshot().commit, head + 1);
        assert_eq!(main.writer.lock().tail.alloc(), wal_alloc);
        let (annotation_end, annotation_alloc) = main.annotations.lock().file_lengths();
        assert!(annotation_alloc > annotation_end);
        assert_eq!(candidate.outcome(), Outcome::StorageRefused);
    }
}
