//! Branches of a persistent dataset ([F09](../../../docs/specs/F09-branches-and-merges.md)).
//!
//! The branch operations are methods of the dataset's own store
//! ([`Store::create_branch`](crate::store::Store::create_branch),
//! [`Store::merge`](crate::store::Store::merge), [`Store::branches`](crate::store::Store::branches),
//! …), reached through [`Dataset::store`]. [`Dataset::branch`] gives a `Dataset` bound
//! to one branch, for reads and writes with the usual API.

use crate::Dataset;
use crate::Result;
use crate::branch::{BranchInfo, BranchOptions, MergeOptions, MergeOutcome, MergeReport};

impl Dataset {
    /// The dataset bound to branch `name`: reads and writes go to that branch. `main`
    /// is this dataset itself. Branch operations (create, merge, delete) are made on
    /// the dataset that owns the branches, the one [`Dataset::open`] returned.
    pub fn branch(&self, name: &str) -> Result<Dataset> {
        match self.store().branch(name)?.shared() {
            Some(s) => Ok(Dataset::from_shared(
                s,
                crate::DatasetOptions {
                    name: self.name().map(str::to_string),
                    ..Default::default()
                },
            )),
            None => Ok(self.clone()),
        }
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
        self.store().create_branch(name, o)
    }

    /// Merge branch `source` into `target` (see [`Store::merge`](crate::store::Store::merge)).
    pub fn merge(&self, source: &str, target: &str, o: &MergeOptions) -> Result<MergeOutcome> {
        self.store().merge(source, target, o)
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
        self.store().rename_branch(name, new)
    }

    /// Delete branch `name` with options, which can re-parent the branches created from
    /// it (see [`Store::delete_branch_with`](crate::store::Store::delete_branch_with)).
    pub fn delete_branch_with(&self, name: &str, o: &crate::branch::DeleteOptions) -> Result<()> {
        self.store().delete_branch_with(name, o)
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
        self.store().delete_branch(name, force)
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
