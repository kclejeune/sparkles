//! Branches of a persistent or in-memory dataset
//! ([F09](../../../docs/specs/F09-branches-and-merges.md)).
//!
//! The branch operations are methods of the dataset's own store
//! ([`Store::create_branch`](crate::store::Store::create_branch),
//! [`Store::merge`](crate::store::Store::merge), [`Store::branches`](crate::store::Store::branches),
//! …), reached through [`Dataset::store`]. [`Dataset::branch`] gives a `Dataset` bound
//! to one branch, for reads and writes with the usual API.

use crate::Dataset;
use crate::Result;
use crate::branch::{BranchInfo, BranchOptions, MergeOptions, MergeOutcome, MergeReport};
use crate::store::{CommitGraph, CommitGraphOptions};

impl Dataset {
    /// The dataset bound to branch `name`: reads and writes go to that branch. `main`
    /// is this dataset itself. Branch operations (create, merge, delete) are made on
    /// the dataset that owns the branches, the one [`Dataset::open`] returned.
    pub fn branch(&self, name: &str) -> Result<Dataset> {
        if name == crate::branch::MAIN {
            return Ok(self.clone());
        }
        let id = self.store().branch_id_of(name)?;
        let upstream = if self.store().root().is_none() {
            self.branch_info(name)?
                .upstream
                .filter(|n| n != "main")
                .map(|n| self.branch(&n))
                .transpose()?
        } else {
            None
        };
        let mut branches = self.state().branches.lock();
        if let Some(ds) = branches.get(name)
            && ds.store().branch_id() == id
        {
            return Ok(ds.clone());
        }
        let store = self
            .store()
            .branch(name)?
            .shared()
            .expect("non-main branch");
        let ds = Dataset::from_shared(
            store,
            crate::DatasetOptions {
                store: self.store().options().clone(),
                name: self.name().map(str::to_string),
                closure_cache_triples: self.state().closure_cache_triples,
                ..Default::default()
            },
        );
        if ds.store().root().is_none() {
            let source = upstream.as_ref().unwrap_or(self);
            *ds.state().reasoning.write() = source.reasoning_record();
            *ds.state().rdfs.write() = source.state().rdfs.read().as_ref().map(|r| {
                std::sync::Arc::new(crate::sparql::rdfs::RdfsOnRead::new(r.source.clone()))
            });
            let guard = source
                .write_guard()
                .map(|guard| -> Result<_> {
                    match guard {
                        #[cfg(feature = "shacl")]
                        crate::write_guard::WriteGuard::Shacl(g) => {
                            Ok(crate::write_guard::WriteGuard::Shacl(
                                g.fork_for_store(ds.store())
                                    .map_err(crate::catalog::component)?,
                            ))
                        }
                        #[cfg(feature = "shex")]
                        crate::write_guard::WriteGuard::Shex(g) => {
                            Ok(crate::write_guard::WriteGuard::Shex(
                                g.fork_for_store(ds.store())
                                    .map_err(crate::catalog::component)?,
                            ))
                        }
                    }
                })
                .transpose()?;
            ds.set_write_guard(guard);
        }
        self.state()
            .branch_handles
            .lock()
            .push(std::sync::Arc::downgrade(&ds.inner));
        branches.insert(name.to_string(), ds.clone());
        Ok(ds)
    }

    /// The branches, `main` first.
    pub fn branches(&self) -> Result<Vec<BranchInfo>> {
        self.store().branches()
    }

    /// Branch `name`, `main` included.
    pub fn branch_info(&self, name: &str) -> Result<BranchInfo> {
        self.store().branch_info(name)
    }

    /// Protect branch `name` from writes other than merges, or lift the protection.
    pub fn set_branch_protected(&self, name: &str, on: bool) -> Result<BranchInfo> {
        self.store().set_branch_protected(name, on)
    }

    /// Set or clear the note of branch `name`.
    pub fn set_branch_note(&self, name: &str, note: Option<String>) -> Result<BranchInfo> {
        self.store().set_branch_note(name, note)
    }

    /// Create branch `name` (see [`Store::create_branch`](crate::store::Store::create_branch)).
    pub fn create_branch(&self, name: &str, o: &BranchOptions) -> Result<BranchInfo> {
        let info = self.store().create_branch(name, o)?;
        if self.store().root().is_none() {
            self.branch(name)?;
        }
        Ok(info)
    }

    /// Relink a persistent linked branch to main's current immutable index without
    /// changing its head, identity or state. Ordinary compaction still builds an
    /// independent index. Historical pins and concurrent writes remain readable.
    pub fn relink_branch(
        &self,
        name: &str,
        options: &crate::store::CompactOptions,
    ) -> Result<crate::store::CompactReport> {
        self.store().relink_branch(name, options)
    }

    /// [`relink_branch`](Self::relink_branch) with shared cancellation and progress.
    pub fn relink_branch_with(
        &self,
        name: &str,
        options: &crate::store::CompactOptions,
        ctl: &crate::task::Control,
    ) -> Result<crate::store::CompactReport> {
        ctl.check()?;
        let mut options = options.clone();
        options.cancel = Some(ctl.cancel.flag());
        let progress = ctl.progress.clone();
        if progress.is_some() {
            progress.report(0.0, "relinking branch");
            let p = progress.clone();
            options.progress = Some(std::sync::Arc::new(move |message: &str| {
                p.report(0.0, message)
            }));
        }
        let report = self.store().relink_branch(name, &options)?;
        progress.report(1.0, "relinked branch");
        Ok(report)
    }

    /// Merge branch `source` into `target` (see [`Store::merge`](crate::store::Store::merge)).
    pub fn merge(&self, source: &str, target: &str, o: &MergeOptions) -> Result<MergeOutcome> {
        self.store().merge(source, target, o)
    }

    /// [`merge`](Self::merge) under `ctl`: cancelled at its next check, with nothing
    /// published, and reporting its stages (a replay, each commit) to its progress.
    pub fn merge_with(
        &self,
        source: &str,
        target: &str,
        o: &MergeOptions,
        ctl: &crate::task::Control,
    ) -> Result<MergeOutcome> {
        ctl.check()?;
        self.store()
            .merge(source, target, &o.clone().with_control(ctl))
    }

    /// What merging `source` into `target` would do, without writing (see
    /// [`Store::preview_merge`](crate::store::Store::preview_merge)).
    pub fn preview_merge(
        &self,
        source: &str,
        target: &str,
        o: &MergeOptions,
    ) -> Result<MergeReport> {
        self.store().preview_merge(source, target, o)
    }

    /// Revert commit `commit` of branch `branch`'s history on that branch (see
    /// [`Store::revert`](crate::store::Store::revert)).
    pub fn revert(&self, branch: &str, commit: u64, o: &MergeOptions) -> Result<MergeOutcome> {
        self.store().revert(branch, commit, o)
    }

    /// What reverting commit `commit` on branch `branch` would do, without writing.
    pub fn preview_revert(
        &self,
        branch: &str,
        commit: u64,
        o: &MergeOptions,
    ) -> Result<MergeReport> {
        self.store().preview_revert(branch, commit, o)
    }

    /// Apply commit `commit` of branch `source`'s history to branch `target` (see
    /// [`Store::cherry_pick`](crate::store::Store::cherry_pick)).
    pub fn cherry_pick(
        &self,
        source: &str,
        commit: u64,
        target: &str,
        o: &MergeOptions,
    ) -> Result<MergeOutcome> {
        self.store().cherry_pick(source, commit, target, o)
    }

    /// What applying commit `commit` of `source` to `target` would do, without writing.
    pub fn preview_cherry_pick(
        &self,
        source: &str,
        commit: u64,
        target: &str,
        o: &MergeOptions,
    ) -> Result<MergeReport> {
        self.store().preview_cherry_pick(source, commit, target, o)
    }

    /// Rename branch `name` to `new` (see
    /// [`Store::rename_branch`](crate::store::Store::rename_branch)).
    pub fn rename_branch(&self, name: &str, new: &str) -> Result<BranchInfo> {
        let info = self.store().rename_branch(name, new)?;
        let mut branches = self.state().branches.lock();
        if let Some(ds) = branches.remove(name) {
            branches.insert(new.to_string(), ds);
        }
        Ok(info)
    }

    /// Delete branch `name` with options, which can re-parent the branches created from
    /// it (see [`Store::delete_branch_with`](crate::store::Store::delete_branch_with)).
    pub fn delete_branch_with(&self, name: &str, o: &crate::branch::DeleteOptions) -> Result<()> {
        self.store().delete_branch_with(name, o)?;
        self.state().branches.lock().remove(name);
        Ok(())
    }

    /// The predicates whose cells never conflict in this dataset's merges.
    pub fn merge_exempt(&self) -> Result<Vec<oxrdf::NamedNode>> {
        self.store().merge_exempt()
    }

    /// Set the predicates whose cells never conflict in this dataset's merges.
    pub fn set_merge_exempt(
        &self,
        predicates: &[oxrdf::NamedNode],
    ) -> Result<Vec<oxrdf::NamedNode>> {
        self.store().set_merge_exempt(predicates)
    }

    /// Delete branch `name`, also with unmerged commits when `force`.
    pub fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        self.store().delete_branch(name, force)?;
        self.state().branches.lock().remove(name);
        Ok(())
    }

    /// A page of the commit graph of several branches, newest first (see
    /// [`Store::commit_graph`](crate::store::Store::commit_graph)).
    pub fn commit_graph(&self, o: &CommitGraphOptions) -> Result<CommitGraph> {
        self.store().commit_graph(o)
    }
}

#[cfg(test)]
mod tests {
    use crate::Dataset;
    use crate::branch::{BranchOptions, MergeOutcome};

    #[test]
    fn a_dataset_bound_to_a_branch() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path().join("db")).unwrap();
        ds.update("INSERT DATA { <urn:a> <urn:p> 1 }").unwrap();
        ds.create_branch("dev", &BranchOptions::default()).unwrap();
        let dev = ds.branch("dev").unwrap();
        dev.update("INSERT DATA { <urn:b> <urn:p> 2 }").unwrap();
        let count = |d: &Dataset| d.store().snapshot().len();
        assert_eq!((count(&ds), count(&dev)), (1, 2));
        assert!(matches!(
            ds.merge("dev", "main", &Default::default()).unwrap(),
            MergeOutcome::Merged(_)
        ));
        assert_eq!(count(&ds), 2);
        assert_eq!(ds.branches().unwrap().len(), 2);
        assert_eq!(ds.branch("main").unwrap().store().branch_name(), "main");
    }
}
