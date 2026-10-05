//! The dataset's backups in a repository (feature `backup`).

use crate::Dataset;
use crate::backup::{
    BackupSummary, Code, ListFilter, Manifest, Repository, VerifyOptions, VerifyReport, block_on,
    error,
};
use crate::error::Result;
use crate::task::Control;

/// The dataset's backups in one repository (from [`Dataset::backups`]). It borrows the
/// repository, which is the caller's. Its calls block (see [`crate::backup`]).
#[derive(Clone)]
pub struct Backups<'r> {
    pub(crate) ds: Dataset,
    pub(crate) repo: &'r Repository,
}

impl Backups<'_> {
    /// Capture a consistent dataset and upload it, preserving the reasoning record
    /// at the captured commit and in-memory validation configuration.
    pub fn create_with(
        &self,
        opts: &crate::backup::CreateOptions,
        ctl: &Control,
    ) -> Result<BackupSummary> {
        self.create_observed_with(opts, ctl, &std::env::temp_dir(), |_| {})
    }

    #[doc(hidden)]
    pub fn create_observed_with(
        &self,
        opts: &crate::backup::CreateOptions,
        ctl: &Control,
        tmp: &std::path::Path,
        captured: impl FnOnce(&crate::store::BackupCapture),
    ) -> Result<BackupSummary> {
        ctl.check()?;
        let cap = if self.ds.store().root().is_some()
            && self.ds.store().snapshot().generation.linked().is_none()
        {
            self.ds.store().backup_capture_with(
                &opts.name,
                &crate::guard::WriteOptions {
                    cancel: Some(ctl.cancel.flag()),
                    deadline: ctl.deadline,
                    ..Default::default()
                },
            )?
        } else {
            self.ds.store().materialized_backup_capture(
                &opts.name,
                &crate::store::MemoryCaptureOptions {
                    tmp_dir: tmp.to_path_buf(),
                    min_free_disk_bytes: opts.min_free_disk_bytes,
                    cancel: Some(ctl.cancel.flag()),
                    progress: ctl.part(0.0, 0.4).progress.as_fn(),
                    deadline: ctl.deadline,
                    no_wait: false,
                },
            )?
        };
        captured(&cap);
        let mut extra = opts.extra.clone();
        if let Some(mut record) = self.ds.reasoning_record()
            && record.commit.is_none_or(|c| c <= cap.commit.seq)
        {
            record.reasoning_format = 2;
            extra.push((
                "reasoning.json".into(),
                serde_json::to_vec_pretty(&record)
                    .map_err(|e| crate::Error::invalid(e.to_string()))?,
            ));
        }
        if cap.in_memory
            && let Some(guard) = self.ds.write_guard()
        {
            extra.extend(guard.memory_files());
        }
        let progress = if cap.materialized {
            ctl.part(0.4, 1.0)
        } else {
            ctl.clone()
        };
        let o = crate::backup::CreateOptions {
            dataset_name: self.ds.name().unwrap_or(&opts.dataset_name).into(),
            extra,
            ctl: (&progress).into(),
            ..opts.clone()
        };
        block_on(self.repo.create(crate::backup::Source::from(cap), &o))?.map_err(error)
    }

    /// The dataset's backups matching `filter`, newest first. The filter's
    /// `dataset_id` is the dataset's id.
    pub fn list(&self, filter: &ListFilter) -> Result<Vec<BackupSummary>> {
        let f = ListFilter {
            dataset_id: Some(self.ds.dataset_id()),
            ..filter.clone()
        };
        block_on(self.repo.list(&f))?.map_err(error)
    }

    /// The manifest of backup `name`, or `None` when the repository has no such backup
    /// of this dataset.
    pub fn get(&self, name: &str) -> Result<Option<Manifest>> {
        match block_on(self.repo.manifest(name))? {
            Ok(m) if m.dataset.id == self.ds.dataset_id() => Ok(Some(m)),
            Ok(_) => Ok(None),
            Err(e) if e.code() == Code::NoSuchBackup => Ok(None),
            Err(e) => Err(error(e)),
        }
    }

    /// Delete backup `name` of this dataset; whether it existed.
    pub fn delete(&self, name: &str) -> Result<bool> {
        if self.get(name)?.is_none() {
            return Ok(false);
        }
        block_on(self.repo.delete(name))?.map_err(error)
    }

    /// Verify backup `name` at the level of `opts`, with the cancellation and progress
    /// of `ctl`.
    pub fn verify_with(
        &self,
        name: &str,
        opts: &VerifyOptions,
        ctl: &Control,
    ) -> Result<VerifyReport> {
        let o = VerifyOptions {
            ctl: ctl.into(),
            ..opts.clone()
        };
        block_on(self.repo.verify(&[name.to_string()], &o))?.map_err(error)
    }
}

impl Backups<'_> {
    /// Run a policy for this dataset in this repository, including its retention.
    pub fn run_policy(
        &self,
        policy: &crate::backup::PolicyConfig,
        ctl: &Control,
    ) -> Result<crate::backup::PolicyRun> {
        use crate::backup::policy::{DatasetInfo, Engine, to_backup};
        struct Single<'a>(Backups<'a>);
        impl Engine for Single<'_> {
            fn datasets(&self) -> Vec<DatasetInfo> {
                vec![DatasetInfo {
                    name: self.0.ds.name().unwrap_or("dataset").into(),
                    id: self.0.ds.dataset_id(),
                    head: self.0.ds.head_commit().seq,
                }]
            }
            fn list(
                &self,
                _: &str,
                policy: &str,
            ) -> std::result::Result<Vec<BackupSummary>, crate::backup::BackupError> {
                self.0
                    .list(&ListFilter {
                        policy: Some(policy.into()),
                        ..Default::default()
                    })
                    .map_err(to_backup)
            }
            fn create(
                &self,
                _: &str,
                _: &str,
                o: crate::backup::CreateOptions,
            ) -> std::result::Result<BackupSummary, crate::backup::BackupError> {
                self.0.create_with(&o, &o.ctl.control()).map_err(to_backup)
            }
            fn delete(
                &self,
                _: &str,
                name: &str,
            ) -> std::result::Result<bool, crate::backup::BackupError> {
                self.0.delete(name).map_err(to_backup)
            }
            fn busy(&self, _: &str) -> std::collections::HashSet<String> {
                Default::default()
            }
            fn start_gc(&self, _: &str) -> std::result::Result<String, crate::backup::BackupError> {
                crate::backup::blocking(self.0.repo)
                    .gc(&Default::default())
                    .map_err(to_backup)?;
                Ok("completed".into())
            }
        }
        ctl.check()?;
        if policy.repository != self.repo.config().name {
            return Err(crate::Error::invalid("the policy names another repository"));
        }
        let report = crate::backup::policy::run(
            &Single(self.clone()),
            policy,
            crate::backup::RunTrigger::Manual,
            None,
            chrono::Utc::now(),
            ctl,
            || true,
        )
        .map_err(error)?;
        ctl.check()?;
        ctl.progress.report(1.0, "policy completed");
        Ok(report)
    }
}
