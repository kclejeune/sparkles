//! Replayed fast-forwards (`ff: "replay"`): a merge whose target holds the state of the
//! merge base replays the source's commits one by one, each as its own commit on the
//! target with the original's kind, message and author. Each replayed commit records the
//! source commit it replays as its second parent, flagged as a replay, so the target
//! descends from every commit replayed so far, also when a replay stops part way.

use super::branching::{BranchSet, MERGE_REPLAYED, MergeRec};
use super::changelog::LogCommit;
use super::diff::{DiffOp, DiffOptions, QuadKey};
use super::merge::{INFERRED_GRAPH, quad_ids};
use super::*;
use crate::branch::{self, CommitRef, MergeOptions, MergeOutcome, MergeReport, ReplayedCommit};
use crate::history::At;

impl Store {
    /// The author the change log recorded for commit `seq` (`None` when it recorded
    /// none, or the log does not hold the commit).
    pub(crate) fn commit_author(&self, seq: u64) -> Option<Arc<str>> {
        let log = self.changelog.as_ref().filter(|l| l.is_enabled())?;
        log.flush(false).ok()?;
        let mut author = None;
        log.commits(seq, seq, &mut || Ok(()), &mut |c| {
            if let LogCommit::Changes(c, _) | LogCommit::Summary(c) = c {
                author = c.author.clone();
            }
            Ok(false)
        })
        .ok()?;
        author
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
}
