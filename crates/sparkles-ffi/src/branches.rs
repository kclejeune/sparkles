use crate::{ErrorKind, FfiDataset, FfiError, FfiOperation, FfiResult, Receipt};
use std::sync::Arc;
#[derive(Clone, Debug, uniffi::Record)]
pub struct BranchInfo {
    pub name: String,
    pub id: String,
    pub head: Option<u64>,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub protected: bool,
    pub note: Option<String>,
    pub linked: bool,
    pub broken: bool,
}
fn info(b: sparkles::branch::BranchInfo) -> BranchInfo {
    BranchInfo {
        name: b.name,
        id: b.id.to_string(),
        head: b.head.map(|h| h.seq),
        upstream: b.upstream,
        ahead: b.ahead,
        behind: b.behind,
        protected: b.protected,
        note: b.note,
        linked: b.storage.linked,
        broken: b.broken,
    }
}
/// What relinking a branch to main's index did.
#[derive(Clone, Debug, uniffi::Record)]
pub struct RelinkInfo {
    pub generation: String,
    pub quads: u64,
    pub base_commit: u64,
    pub caught_up_commits: u64,
    pub abandoned: Option<String>,
    pub lock_ms: f64,
    pub build_ms: f64,
    pub total_ms: f64,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct MergeSettings {
    pub ff_only: bool,
    pub squash: bool,
    pub replay: bool,
    pub scope: String,
    pub on_conflict: Option<String>,
    pub expect_source: Option<u64>,
    pub expect_target: Option<u64>,
    pub include_inferences: bool,
    pub message: Option<String>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct ConflictCell {
    pub graph: Option<String>,
    pub subject: String,
    pub predicate: Option<String>,
    pub base: Vec<String>,
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct MergeInfo {
    pub merged: bool,
    pub up_to_date: bool,
    pub fast_forward: bool,
    pub squashed: bool,
    pub inserted: u64,
    pub deleted: u64,
    pub conflicts_found: u64,
    pub conflicts_resolved: u64,
    pub conflicts: Vec<ConflictCell>,
    pub truncated: bool,
    pub receipt: Option<Receipt>,
}
fn cells(c: sparkles::branch::ConflictReport) -> (Vec<ConflictCell>, bool) {
    (
        c.cells
            .into_iter()
            .map(|c| ConflictCell {
                graph: c.graph,
                subject: c.subject,
                predicate: c.predicate,
                base: c.base,
                ours: c.ours,
                theirs: c.theirs,
            })
            .collect(),
        c.truncated,
    )
}
fn report(r: sparkles::branch::MergeReport) -> MergeInfo {
    let (conflicts, truncated) = r.conflicts.map(cells).unwrap_or_default();
    MergeInfo {
        merged: r.merged,
        up_to_date: r.up_to_date,
        fast_forward: r.fast_forward,
        squashed: r.squashed,
        inserted: r.inserted,
        deleted: r.deleted,
        conflicts_found: r.conflicts_found,
        conflicts_resolved: r.conflicts_resolved,
        conflicts,
        truncated,
        receipt: r.commit.as_ref().map(Into::into),
    }
}
fn outcome(r: sparkles::branch::MergeOutcome) -> MergeInfo {
    match r {
        sparkles::branch::MergeOutcome::Merged(r) | sparkles::branch::MergeOutcome::UpToDate(r) => {
            report(r)
        }
        sparkles::branch::MergeOutcome::Conflicts(r) => {
            let count = r.conflicts;
            let (conflicts, truncated) = cells(*r);
            MergeInfo {
                merged: false,
                up_to_date: false,
                fast_forward: false,
                squashed: false,
                inserted: 0,
                deleted: 0,
                conflicts_found: count,
                conflicts_resolved: 0,
                conflicts,
                truncated,
                receipt: None,
            }
        }
    }
}
fn options(s: MergeSettings, op: &FfiOperation) -> FfiResult<sparkles::branch::MergeOptions> {
    let bad = |text| FfiError::new(ErrorKind::Invalid, text);
    let scope = sparkles::branch::ConflictScope::parse(&s.scope)
        .ok_or_else(|| bad("unknown conflict scope"))?;
    let take = s
        .on_conflict
        .map(|s| {
            sparkles::branch::Take::parse(&s).ok_or_else(|| bad("unknown conflict resolution"))
        })
        .transpose()?;
    let mut options = sparkles::branch::MergeOptions {
        ff_only: s.ff_only,
        squash: s.squash,
        replay: s.replay,
        scope,
        on_conflict: take,
        expect_source: s.expect_source,
        expect_target: s.expect_target,
        include_inferences: s.include_inferences,
        ..Default::default()
    };
    options.write.message = s.message.map(Into::into);
    Ok(options.with_control(&op.control))
}
#[uniffi::export]
impl FfiDataset {
    /// Resolve identities from the branch table without capturing writer-owned snapshots.
    pub fn branches_ids(&self, names: Vec<String>) -> FfiResult<Vec<String>> {
        names
            .iter()
            .map(|n| Ok(self.inner.ds.store().branch_id_of(n)?.to_string()))
            .collect()
    }
    pub fn branches_list(&self) -> FfiResult<Vec<BranchInfo>> {
        Ok(self.inner.ds.branches()?.into_iter().map(info).collect())
    }
    pub fn branches_get(&self, name: String) -> FfiResult<BranchInfo> {
        Ok(info(self.inner.ds.branch_info(&name)?))
    }
    pub fn branch(&self, name: String) -> FfiResult<Arc<FfiDataset>> {
        Ok(FfiDataset::new(
            self.inner.ds.branch(&name)?,
            self.inner.opts.clone(),
        ))
    }
    pub fn branches_create(
        &self,
        name: String,
        from: String,
        at: String,
        protected: bool,
        note: Option<String>,
    ) -> FfiResult<BranchInfo> {
        self.inner.check_writable()?;
        Ok(info(self.inner.ds.create_branch(
            &name,
            &sparkles::branch::BranchOptions {
                from,
                at: at.parse()?,
                protected,
                note,
            },
        )?))
    }
    pub fn branches_delete(&self, name: String, force: bool, reparent: bool) -> FfiResult<()> {
        self.inner.check_writable()?;
        Ok(self
            .inner
            .ds
            .delete_branch_with(&name, &sparkles::branch::DeleteOptions { force, reparent })?)
    }
    pub fn branches_rename(&self, name: String, to: String) -> FfiResult<BranchInfo> {
        self.inner.check_writable()?;
        Ok(info(self.inner.ds.rename_branch(&name, &to)?))
    }
    pub fn branches_protect(&self, name: String, on: bool) -> FfiResult<BranchInfo> {
        self.inner.check_writable()?;
        Ok(info(self.inner.ds.set_branch_protected(&name, on)?))
    }
    pub fn branches_note(&self, name: String, note: Option<String>) -> FfiResult<BranchInfo> {
        self.inner.check_writable()?;
        Ok(info(self.inner.ds.set_branch_note(&name, note)?))
    }
    pub fn branches_relink(
        &self,
        name: String,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<RelinkInfo> {
        self.inner.check_writable()?;
        operation.control.check()?;
        let r = self
            .inner
            .ds
            .relink_branch_with(&name, &Default::default(), &operation.control)?;
        Ok(RelinkInfo {
            generation: r.generation,
            quads: r.quads,
            base_commit: r.base_commit,
            caught_up_commits: r.caught_up_commits,
            abandoned: r.abandoned,
            lock_ms: r.lock_ms,
            build_ms: r.build_ms,
            total_ms: r.total_ms,
        })
    }
    pub fn branches_settings(&self) -> FfiResult<Vec<String>> {
        Ok(self
            .inner
            .ds
            .merge_exempt()?
            .into_iter()
            .map(|p| p.into_string())
            .collect())
    }
    pub fn branches_set_settings(&self, predicates: Vec<String>) -> FfiResult<Vec<String>> {
        self.inner.check_writable()?;
        let p = predicates
            .into_iter()
            .map(oxrdf::NamedNode::new)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| FfiError::new(ErrorKind::Invalid, e.to_string()))?;
        Ok(self
            .inner
            .ds
            .set_merge_exempt(&p)?
            .into_iter()
            .map(|p| p.into_string())
            .collect())
    }
    pub fn branches_merge(
        &self,
        source: String,
        target: String,
        s: MergeSettings,
        preview: bool,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<MergeInfo> {
        operation.control.check()?;
        let opts = options(s, &operation)?;
        if preview {
            Ok(report(
                self.inner.ds.preview_merge(&source, &target, &opts)?,
            ))
        } else {
            self.inner.check_writable()?;
            Ok(outcome(self.inner.ds.merge_with(
                &source,
                &target,
                &opts,
                &operation.control,
            )?))
        }
    }
    pub fn branches_revert(
        &self,
        branch: String,
        commit: u64,
        s: MergeSettings,
        preview: bool,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<MergeInfo> {
        operation.control.check()?;
        let opts = options(s, &operation)?;
        if preview {
            Ok(report(
                self.inner.ds.preview_revert(&branch, commit, &opts)?,
            ))
        } else {
            self.inner.check_writable()?;
            Ok(outcome(self.inner.ds.revert(&branch, commit, &opts)?))
        }
    }
    pub fn branches_cherry_pick(
        &self,
        source: String,
        commit: u64,
        target: String,
        s: MergeSettings,
        preview: bool,
        operation: Arc<FfiOperation>,
    ) -> FfiResult<MergeInfo> {
        operation.control.check()?;
        let opts = options(s, &operation)?;
        if preview {
            Ok(report(
                self.inner
                    .ds
                    .preview_cherry_pick(&source, commit, &target, &opts)?,
            ))
        } else {
            self.inner.check_writable()?;
            Ok(outcome(
                self.inner.ds.cherry_pick(&source, commit, &target, &opts)?,
            ))
        }
    }
}
