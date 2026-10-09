//! Blocking policy execution, independent of server scheduling and task admission.
use super::*;
use crate::task::Control;
use crate::{Error, Result};
use chrono::{DateTime, Utc};
pub use sparkles_backup::policy::*;
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct DatasetInfo {
    pub name: String,
    pub id: Uuid,
    pub head: u64,
}

/// The effects of a policy. A server supplies task admission, active-backup claims
/// and its clock here; a catalog runs directly against a repository.
pub trait Engine {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
    fn datasets(&self) -> Vec<DatasetInfo>;
    fn list(
        &self,
        repository: &str,
        policy: &str,
    ) -> std::result::Result<Vec<BackupSummary>, BackupError>;
    fn create(
        &self,
        repository: &str,
        dataset: &str,
        options: CreateOptions,
    ) -> std::result::Result<BackupSummary, BackupError>;
    fn delete(&self, repository: &str, backup: &str) -> std::result::Result<bool, BackupError>;
    fn busy(&self, repository: &str) -> HashSet<String>;
    fn start_gc(&self, repository: &str) -> std::result::Result<String, BackupError>;
}

pub fn apply_retention(
    engine: &dyn Engine,
    p: &PolicyConfig,
    dry_run: bool,
    now: DateTime<Utc>,
) -> std::result::Result<RetentionResponse, BackupError> {
    let list = engine.list(&p.repository, &p.name)?;
    let plan = retention(
        &list,
        &p.name,
        &p.retention,
        now,
        &engine.busy(&p.repository),
    );
    let mut deleted = Vec::new();
    let mut errors = Vec::new();
    for b in plan.delete {
        if dry_run {
            deleted.push(b);
            continue;
        }
        match engine.delete(&p.repository, &b.name) {
            Ok(_) => deleted.push(b),
            Err(e) => errors.push(format!("{}: {}", b.name, e.message())),
        }
    }
    Ok(RetentionResponse {
        dry_run,
        delete: deleted,
        keep: plan.keep,
        errors: (!errors.is_empty()).then_some(errors),
    })
}

pub fn run(
    engine: &dyn Engine,
    p: &PolicyConfig,
    trigger: RunTrigger,
    scheduled: Option<DateTime<Utc>>,
    started: DateTime<Utc>,
    ctl: &Control,
    enabled: impl Fn() -> bool,
) -> std::result::Result<PolicyRun, BackupError> {
    let tz = parse_timezone(&p.timezone)?;
    check_policy(p)?;
    let id = Uuid::new_v4();
    let at = scheduled.unwrap_or(started);
    let selected: Vec<_> = engine
        .datasets()
        .into_iter()
        .filter(|d| p.datasets.iter().any(|g| matches_dataset(g, &d.name)))
        .collect();
    let previous = if p.skip_unchanged {
        engine.list(&p.repository, &p.name).unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut run = PolicyRun {
        id: id.to_string(),
        policy: p.name.clone(),
        trigger,
        scheduled_for: scheduled.map(time),
        started: time(started),
        finished: None,
        result: RunResult::Ok,
        reason: None,
        datasets: Vec::new(),
        retention: None,
        gc: None,
    };
    let (mut cancelled, mut disabled) = (false, false);
    for (i, ds) in selected.iter().enumerate() {
        cancelled |= ctl.check().is_err();
        if !cancelled && !disabled {
            disabled = !enabled();
        }
        let why = if cancelled {
            Some("cancelled")
        } else if disabled {
            Some("policy disabled")
        } else {
            previous
                .iter()
                .filter(|b| b.dataset.id == ds.id)
                .max_by(|a, b| a.completed.cmp(&b.completed))
                .filter(|b| b.commit.seq == ds.head)
                .map(|_| "unchanged")
        };
        if let Some(why) = why {
            run.datasets.push(PolicyRunDataset {
                dataset: ds.name.clone(),
                backup: None,
                result: DatasetRunResult::Skipped,
                reason: Some(why.into()),
                added_bytes: None,
                millis: None,
            });
            continue;
        }
        ctl.progress.report(
            i as f32 / selected.len() as f32 * 0.9,
            &format!("dataset {}/{}: {}", i + 1, selected.len(), ds.name),
        );
        let t0 = std::time::Instant::now();
        let result = (|| {
            let base = render_name(
                &p.name_template,
                &NameCtx {
                    policy: &p.name,
                    dataset: &ds.name,
                    seq: ds.head,
                    run: id,
                    time: at,
                    tz,
                },
            )?;
            for k in 1..=100 {
                let name = if k == 1 {
                    base.clone()
                } else {
                    let suffix = format!("-{k}");
                    format!("{}{suffix}", &base[..base.len().min(64 - suffix.len())])
                };
                let opts = CreateOptions {
                    name,
                    note: None,
                    policy: Some((p.name.clone(), id.to_string())),
                    dataset_name: ds.name.clone(),
                    extra: Vec::new(),
                    min_free_disk_bytes: None,
                    ctl: (&ctl.part(
                        i as f32 / selected.len() as f32 * 0.9,
                        (i + 1) as f32 / selected.len() as f32 * 0.9,
                    ))
                        .into(),
                };
                match engine.create(&p.repository, &ds.name, opts) {
                    Err(e) if e.code() == Code::BackupExists && k < 100 => continue,
                    result => return result,
                }
            }
            unreachable!("last attempt returns")
        })();
        run.datasets.push(match result {
            Ok(b) => PolicyRunDataset {
                dataset: ds.name.clone(),
                backup: Some(b.name),
                result: DatasetRunResult::Ok,
                reason: None,
                added_bytes: Some(b.added_bytes),
                millis: Some(t0.elapsed().as_millis() as u64),
            },
            Err(e) => PolicyRunDataset {
                dataset: ds.name.clone(),
                backup: None,
                result: DatasetRunResult::Failed,
                reason: Some(e.message().into()),
                added_bytes: None,
                millis: Some(t0.elapsed().as_millis() as u64),
            },
        });
    }
    // A cancellation during the final create has no next dataset to observe it.
    // Classify it before retention, which must not delete after cancellation.
    cancelled |= ctl.check().is_err();
    let ok = run
        .datasets
        .iter()
        .filter(|d| d.result == DatasetRunResult::Ok)
        .count();
    let failed = run
        .datasets
        .iter()
        .filter(|d| d.result == DatasetRunResult::Failed)
        .count();
    if !cancelled && !disabled {
        ctl.progress.report(0.95, "retention");
        let mut retention = match apply_retention(engine, p, false, engine.now()) {
            Ok(r) => RunRetention {
                deleted: r.delete.into_iter().map(|b| b.name).collect(),
                error: r.errors.map(|e| e.join("; ")),
            },
            Err(e) => RunRetention {
                deleted: Vec::new(),
                error: Some(e.message().into()),
            },
        };
        if p.gc_after_retention && !retention.deleted.is_empty() && ctl.check().is_ok() {
            match engine.start_gc(&p.repository) {
                Ok(task) => run.gc = Some(RunGc { task }),
                // The check below reports a cancelled collection as the run's cancellation.
                Err(e) if e.code() == Code::Cancelled => {}
                // The run result has no GC error field, so a collection that fails to
                // start is reported with the retention it follows.
                Err(e) => {
                    let gc = format!("gc after retention failed: {}", e.message());
                    retention.error = Some(match retention.error.take() {
                        Some(earlier) => format!("{earlier}; {gc}"),
                        None => gc,
                    });
                }
            }
        }
        run.retention = Some(retention);
    }
    // Retention may also observe cancellation during its final list/delete.
    cancelled |= ctl.check().is_err();
    run.result = if disabled {
        RunResult::Skipped
    } else if failed == 0 && !cancelled {
        RunResult::Ok
    } else if ok == 0 {
        RunResult::Failed
    } else {
        RunResult::Partial
    };
    if cancelled {
        run.reason = Some("cancelled".into());
    }
    run.finished = Some(time(engine.now()));
    Ok(run)
}
fn time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
/// Whether a dataset alias matches a backup policy pattern (`*` is the wildcard).
pub fn matches_dataset(pattern: &str, name: &str) -> bool {
    let mut rest = name;
    let parts: Vec<_> = pattern.split('*').collect();
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            let Some(tail) = rest.strip_prefix(part) else {
                return false;
            };
            rest = tail;
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else if let Some(at) = rest.find(part) {
            rest = &rest[at + part.len()..];
        } else {
            return false;
        }
    }
    rest.is_empty()
}

struct CatalogEngine<'a> {
    catalog: &'a crate::Catalog,
    repository: std::sync::Arc<Repository>,
    selected: Option<&'a [String]>,
    ctl: Ctl,
}
impl Engine for CatalogEngine<'_> {
    fn datasets(&self) -> Vec<DatasetInfo> {
        self.catalog
            .list()
            .into_iter()
            // Only committed, published state matters for skip_unchanged. Reading
            // the snapshot preserves the family guard without waiting for a writer.
            .filter(|d| {
                self.selected
                    .is_none_or(|patterns| patterns.iter().any(|p| matches_dataset(p, &d.name)))
            })
            .filter_map(|d| {
                self.catalog.get(&d.name).map(|ds| DatasetInfo {
                    name: d.name,
                    id: d.id,
                    head: ds.snapshot().commit,
                })
            })
            .collect()
    }
    fn list(&self, _: &str, policy: &str) -> std::result::Result<Vec<BackupSummary>, BackupError> {
        self.ctl.check()?;
        let listed = block_on(self.repository.list(&ListFilter {
            policy: Some(policy.into()),
            ..Default::default()
        }))
        .map_err(to_backup)??;
        self.ctl.check()?;
        Ok(listed)
    }
    fn create(
        &self,
        _: &str,
        dataset: &str,
        options: CreateOptions,
    ) -> std::result::Result<BackupSummary, BackupError> {
        let ds = self
            .catalog
            .get(dataset)
            .ok_or_else(|| BackupError::new(Code::InvalidRequest, "no such dataset"))?;
        ds.backups(&self.repository)
            .create_with(&options, &options.ctl.control())
            .map_err(to_backup)
    }
    fn delete(&self, _: &str, backup: &str) -> std::result::Result<bool, BackupError> {
        self.ctl.check()?;
        let deleted = block_on(self.repository.delete(backup)).map_err(to_backup)??;
        self.ctl.check()?;
        Ok(deleted)
    }
    fn busy(&self, _: &str) -> HashSet<String> {
        HashSet::new()
    }
    fn start_gc(&self, _: &str) -> std::result::Result<String, BackupError> {
        block_on(self.repository.gc(&GcOptions {
            ctl: self.ctl.clone(),
            ..Default::default()
        }))
        .map_err(to_backup)??;
        Ok("completed".into())
    }
}
pub fn to_backup(e: Error) -> BackupError {
    match e {
        Error::Component(c) => {
            if let Some(s) = c.source
                && let Ok(e) = s.downcast::<BackupError>()
            {
                return *e;
            }
            BackupError::internal(c.message)
        }
        Error::Cancelled => BackupError::cancelled(),
        e => e.into(),
    }
}
impl crate::Catalog {
    pub fn run_policy(&self, p: &PolicyConfig, ctl: &Control) -> Result<PolicyRun> {
        ctl.check()?;
        self.run_policy_in(p, self.repositories()?.open(&p.repository)?, ctl)
    }

    /// Run a policy against an explicitly opened repository, retaining its protected
    /// keys for the complete run without storing provider references in the catalog.
    /// The handle must match the policy name and any registered identity/location.
    pub fn run_policy_in(
        &self,
        p: &PolicyConfig,
        repository: std::sync::Arc<Repository>,
        ctl: &Control,
    ) -> Result<PolicyRun> {
        ctl.check()?;
        check_policy(p).map_err(error)?;
        if repository.config().name != p.repository {
            return Err(error(BackupError::new(
                Code::InvalidRequest,
                "the opened repository does not match the policy repository name",
            )));
        }
        // Load persisted registration metadata even for a freshly opened catalog.
        // This never opens a repository backend or resolves provider inputs.
        let registered = self.repositories()?;
        {
            let entries = registered.registry.repos.read();
            if let Some(entry) = entries.get(&p.repository)
                && (!entry.config.same_location(repository.config())
                    || entry.id.is_some_and(|id| id != repository.id()))
            {
                return Err(error(BackupError::new(
                    Code::InvalidRequest,
                    "the opened repository does not match the registered repository identity",
                )));
            }
        }
        let encrypted = repository
            .marker()
            .encryption
            .as_ref()
            .is_some_and(|value| !value.is_null());
        let mut forbidden = self
            .dir()
            .map(|dir| vec![dir.to_path_buf()])
            .unwrap_or_default();
        forbidden.extend(self.list().into_iter().filter_map(|dataset| dataset.path));
        repository.config().validate(&forbidden).map_err(|e| {
            error(if encrypted {
                BackupError::new(
                    e.code(),
                    "encrypted policy repository configuration is invalid",
                )
            } else {
                e
            })
        })?;
        let engine = CatalogEngine {
            catalog: self,
            repository,
            selected: Some(&p.datasets),
            // `run` reports retention at 0.95, so the collection's progress fills the rest.
            ctl: (&ctl.part(0.95, 1.0)).into(),
        };
        let mut report = run(
            &engine,
            p,
            RunTrigger::Manual,
            None,
            Utc::now(),
            ctl,
            || true,
        )
        .map_err(|e| {
            error(if encrypted {
                BackupError::new(e.code(), "encrypted repository policy execution failed")
            } else {
                e
            })
        })?;
        if encrypted {
            for dataset in &mut report.datasets {
                if dataset.result == DatasetRunResult::Failed && dataset.reason.is_some() {
                    dataset.reason = Some("encrypted repository backup failed".into());
                }
            }
            if let Some(retention) = &mut report.retention
                && retention.error.is_some()
            {
                retention.error = Some("encrypted repository retention failed".into());
            }
        }
        ctl.check()?;
        ctl.progress.report(1.0, "policy completed");
        Ok(report)
    }
    pub fn apply_retention(&self, p: &PolicyConfig, dry_run: bool) -> Result<RetentionResponse> {
        let engine = CatalogEngine {
            catalog: self,
            repository: self.repositories()?.open(&p.repository)?,
            selected: None,
            ctl: Ctl::default(),
        };
        apply_retention(&engine, p, dry_run, Utc::now()).map_err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone, Copy)]
    enum StopAt {
        Create,
        List,
        Delete,
        AfterDelete,
        OrdinaryFailure,
        GcFailure,
        GcCancelled,
    }
    struct StoppingEngine {
        control: Control,
        stop: StopAt,
        listed: std::cell::Cell<usize>,
        deleted: std::cell::Cell<usize>,
        gc: std::cell::Cell<usize>,
    }
    fn summary(name: &str, completed: &str) -> BackupSummary {
        BackupSummary {
            name: name.into(),
            repository: "local".into(),
            dataset: sparkles_backup::DatasetRef {
                branch: None,
                name: "ds".into(),
                id: Uuid::from_u128(1),
                kind: "persistent".into(),
            },
            commit: sparkles_backup::CommitRef {
                seq: 1,
                timestamp: completed.into(),
                quads: 1,
                reference: "commit:1".into(),
            },
            created: completed.into(),
            completed: completed.into(),
            millis: 1,
            logical_bytes: 1,
            added_bytes: 1,
            policy: Some("nightly".into()),
            run: None,
            note: None,
            same_lineage: None,
            verified: None,
        }
    }
    impl Engine for StoppingEngine {
        fn datasets(&self) -> Vec<DatasetInfo> {
            vec![DatasetInfo {
                name: "ds".into(),
                id: Uuid::from_u128(1),
                head: 1,
            }]
        }
        fn list(&self, _: &str, _: &str) -> std::result::Result<Vec<BackupSummary>, BackupError> {
            self.listed.set(self.listed.get() + 1);
            if matches!(self.stop, StopAt::List) {
                self.control.cancel.cancel();
                return Err(BackupError::cancelled());
            }
            Ok(vec![
                summary("new", "2026-10-05T00:00:00Z"),
                summary("old", "2025-10-05T00:00:00Z"),
            ])
        }
        fn create(
            &self,
            _: &str,
            _: &str,
            o: CreateOptions,
        ) -> std::result::Result<BackupSummary, BackupError> {
            if matches!(self.stop, StopAt::Create) {
                self.control.cancel.cancel();
                return Err(BackupError::cancelled());
            }
            if matches!(self.stop, StopAt::OrdinaryFailure) {
                return Err(BackupError::new(
                    Code::RepositoryUnavailable,
                    "normal failure",
                ));
            }
            Ok(summary(&o.name, "2026-10-05T00:00:00Z"))
        }
        fn delete(&self, _: &str, _: &str) -> std::result::Result<bool, BackupError> {
            self.deleted.set(self.deleted.get() + 1);
            if matches!(self.stop, StopAt::Delete | StopAt::AfterDelete) {
                self.control.cancel.cancel();
                if matches!(self.stop, StopAt::Delete) {
                    return Err(BackupError::cancelled());
                }
            }
            Ok(true)
        }
        fn busy(&self, _: &str) -> HashSet<String> {
            HashSet::new()
        }
        fn start_gc(&self, _: &str) -> std::result::Result<String, BackupError> {
            self.gc.set(self.gc.get() + 1);
            match self.stop {
                StopAt::GcFailure => Err(BackupError::new(
                    Code::RepositoryUnavailable,
                    "repository locked",
                )),
                StopAt::GcCancelled => {
                    self.control.cancel.cancel();
                    Err(BackupError::cancelled())
                }
                _ => Ok("gc".into()),
            }
        }
    }
    fn stopping_run(stop: StopAt) -> (PolicyRun, StoppingEngine) {
        let control = Control::default();
        let engine = StoppingEngine {
            control: control.clone(),
            stop,
            listed: std::cell::Cell::new(0),
            deleted: std::cell::Cell::new(0),
            gc: std::cell::Cell::new(0),
        };
        let policy: PolicyConfig = serde_json::from_value(serde_json::json!({"name":"nightly","repository":"local","datasets":["ds"],"schedule":"0 0 * * *","nameTemplate":"new","retention":{"maxCount":1},"gcAfterRetention":true})).unwrap();
        let report = run(
            &engine,
            &policy,
            RunTrigger::Schedule,
            None,
            Utc::now(),
            &control,
            || true,
        )
        .unwrap();
        (report, engine)
    }
    #[test]
    fn cancellation_during_final_capture_skips_retention_and_is_classified() {
        let (report, engine) = stopping_run(StopAt::Create);
        assert_eq!(report.reason.as_deref(), Some("cancelled"));
        assert_eq!(report.result, RunResult::Failed);
        assert!(report.retention.is_none());
        assert_eq!(engine.listed.get(), 0);
        assert_eq!(engine.deleted.get(), 0);
        assert_eq!(engine.gc.get(), 0);
    }
    #[test]
    fn cancellation_during_final_retention_list_or_delete_is_classified_and_skips_gc() {
        for stop in [StopAt::List, StopAt::Delete, StopAt::AfterDelete] {
            let (report, engine) = stopping_run(stop);
            assert_eq!(report.reason.as_deref(), Some("cancelled"));
            assert_eq!(report.result, RunResult::Partial);
            assert_eq!(engine.listed.get(), 1);
            assert_eq!(engine.gc.get(), 0);
            assert!(report.gc.is_none());
        }
    }
    #[test]
    fn ordinary_capture_failure_keeps_recoverable_failure_and_retention_semantics() {
        let (report, engine) = stopping_run(StopAt::OrdinaryFailure);
        assert_eq!(report.reason, None);
        assert_eq!(report.result, RunResult::Failed);
        assert_eq!(report.datasets[0].reason.as_deref(), Some("normal failure"));
        assert!(report.retention.is_some());
        assert_eq!(engine.listed.get(), 1);
        assert_eq!(engine.deleted.get(), 1);
        assert_eq!(engine.gc.get(), 1);
    }

    #[test]
    fn failed_gc_start_is_reported_with_retention() {
        let (report, engine) = stopping_run(StopAt::GcFailure);
        assert_eq!(engine.gc.get(), 1);
        assert!(report.gc.is_none());
        let retention = report.retention.unwrap();
        assert_eq!(retention.deleted, ["old"]);
        assert_eq!(
            retention.error.as_deref(),
            Some("gc after retention failed: repository locked")
        );
        assert_eq!(report.result, RunResult::Ok);
        assert_eq!(report.reason, None);
    }
    #[test]
    fn cancelled_gc_is_the_run_cancellation_not_a_retention_error() {
        let (report, engine) = stopping_run(StopAt::GcCancelled);
        assert_eq!(engine.gc.get(), 1);
        assert!(report.gc.is_none());
        assert_eq!(report.retention.unwrap().error, None);
        assert_eq!(report.reason.as_deref(), Some("cancelled"));
        assert_eq!(report.result, RunResult::Partial);
    }

    #[test]
    fn policy_discovery_does_not_capture_unselected_writer() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = crate::Catalog::memory(Default::default());
        let options = crate::catalog::CreateDataset {
            kind: crate::catalog::DatasetKind::Memory,
            ..Default::default()
        };
        catalog.create("selected", &options).unwrap();
        let unrelated = catalog.create("other", &options).unwrap();
        catalog
            .repositories()
            .unwrap()
            .add(
                serde_json::from_value(serde_json::json!({
                    "name":"local", "type":"fs", "path":directory.path()
                }))
                .unwrap(),
            )
            .unwrap();
        let patterns = vec!["sel*".to_string()];
        let mut engine = CatalogEngine {
            catalog: &catalog,
            repository: catalog.repositories().unwrap().open("local").unwrap(),
            selected: Some(&patterns),
            ctl: Ctl::default(),
        };
        let writer = unrelated.store().write();
        let rows = std::thread::scope(|scope| {
            let (send, receive) = std::sync::mpsc::channel();
            let selected_engine = &engine;
            let capture = scope.spawn(move || send.send(selected_engine.datasets()).unwrap());
            let result = receive.recv_timeout(std::time::Duration::from_secs(2));
            // Release the held writer even on failure before joining the reader.
            drop(writer);
            capture.join().unwrap();
            result.expect("unselected dataset writer must not block policy discovery")
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "selected");
        assert_eq!(rows[0].head, 0);
        engine.selected = None;
        assert_eq!(engine.datasets().len(), 2);
    }

    fn manual_policy(repository: &str, datasets: &[&str]) -> PolicyConfig {
        serde_json::from_value(serde_json::json!({
            "name":"manual", "repository":repository, "datasets":datasets,
            "schedule":"every 1h", "nameTemplate":"{policy}-{dataset}-{run}"
        }))
        .unwrap()
    }

    #[test]
    fn explicit_policy_repository_matches_name_location_and_registered_uuid() {
        let root = tempfile::tempdir().unwrap();
        let catalog = crate::Catalog::memory(Default::default());
        let config = |name: &str, path: &str| {
            serde_json::from_value(serde_json::json!({
                "name":name,"type":"fs","path":root.path().join(path)
            }))
            .unwrap()
        };
        let repositories = catalog.repositories().unwrap();
        repositories.add(config("local", "one")).unwrap();
        let registered = repositories.open("local").unwrap();
        let policy = manual_policy("local", &["*"]);
        let other = std::sync::Arc::new(
            super::super::open(
                &config("local", "two"),
                &OpenEnv {
                    init: true,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        assert_eq!(
            to_backup(
                catalog
                    .run_policy_in(&policy, other, &Control::default())
                    .unwrap_err()
            )
            .code(),
            Code::InvalidRequest
        );
        let wrong_name = manual_policy("other", &["*"]);
        assert_eq!(
            to_backup(
                catalog
                    .run_policy_in(&wrong_name, registered.clone(), &Control::default())
                    .unwrap_err()
            )
            .code(),
            Code::InvalidRequest
        );
        repositories
            .registry
            .repos
            .write()
            .get_mut("local")
            .unwrap()
            .id = Some(Uuid::new_v4());
        assert_eq!(
            to_backup(
                catalog
                    .run_policy_in(&policy, registered, &Control::default())
                    .unwrap_err()
            )
            .code(),
            Code::InvalidRequest
        );
    }

    #[test]
    fn selected_writer_does_not_block_published_policy_metadata_or_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let catalog = crate::Catalog::memory(Default::default());
        let ds = catalog
            .create(
                "selected",
                &crate::catalog::CreateDataset {
                    kind: crate::catalog::DatasetKind::Memory,
                    ..Default::default()
                },
            )
            .unwrap();
        ds.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
        let repository = std::sync::Arc::new(
            super::super::open(
                &serde_json::from_value(
                    serde_json::json!({"name":"local","type":"fs","path":root.path()}),
                )
                .unwrap(),
                &OpenEnv {
                    init: true,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let patterns = vec!["selected".to_string()];
        let engine = CatalogEngine {
            catalog: &catalog,
            repository: repository.clone(),
            selected: Some(&patterns),
            ctl: Ctl::default(),
        };
        let control = Control::default();
        let writer = ds.store().write();
        std::thread::scope(|scope| {
            let (send, receive) = std::sync::mpsc::channel();
            let metadata = scope.spawn(move || send.send(engine.datasets()).unwrap());
            let result = receive.recv_timeout(std::time::Duration::from_secs(2));
            // If metadata ever regresses, release the writer before joining.
            if result.is_err() {
                drop(writer);
                metadata.join().unwrap();
                panic!("selected writer blocked committed metadata");
            }
            metadata.join().unwrap();
            let rows = result.unwrap();
            assert_eq!(rows[0].head, 1);
            let (send, receive) = std::sync::mpsc::channel();
            let policy = manual_policy("local", &["selected"]);
            let worker_control = control.clone();
            let catalog = &catalog;
            let run = scope.spawn(move || {
                send.send(catalog.run_policy_in(&policy, repository, &worker_control))
                    .unwrap()
            });
            control.cancel.cancel();
            let result = receive.recv_timeout(std::time::Duration::from_secs(2));
            drop(writer);
            run.join().unwrap();
            assert!(matches!(
                result.expect("policy cancellation waited for selected writer"),
                Err(Error::Cancelled)
            ));
        });
    }

    #[test]
    fn zero_dataset_policy_gc_observes_control_and_releases_lease() {
        let root = tempfile::tempdir().unwrap();
        let catalog = crate::Catalog::memory(Default::default());
        let ds = catalog
            .create(
                "ds",
                &crate::catalog::CreateDataset {
                    kind: crate::catalog::DatasetKind::Memory,
                    ..Default::default()
                },
            )
            .unwrap();
        ds.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
        let repository = std::sync::Arc::new(
            super::super::open(
                &serde_json::from_value(
                    serde_json::json!({"name":"local","type":"fs","path":root.path()}),
                )
                .unwrap(),
                &OpenEnv {
                    init: true,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let mut policy = manual_policy("local", &["ds"]);
        catalog
            .run_policy_in(&policy, repository.clone(), &Control::default())
            .unwrap();
        catalog
            .run_policy_in(&policy, repository.clone(), &Control::default())
            .unwrap();
        policy.datasets = vec!["absent".into()];
        policy.retention.min_count = 0;
        policy.retention.max_count = Some(1);
        policy.gc_after_retention = true;
        let mut control = Control::default();
        let cancel = control.cancel.clone();
        control.progress = crate::task::Progress::new(move |_, message| {
            if message == "reading manifests" {
                cancel.cancel();
            }
        });
        assert!(matches!(
            catalog.run_policy_in(&policy, repository.clone(), &control),
            Err(Error::Cancelled)
        ));
        assert!(
            super::super::block_on(repository.locks())
                .unwrap()
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn explicit_policy_repository_cannot_be_inside_live_attached_dataset() {
        let root = tempfile::tempdir().unwrap();
        let catalog = crate::Catalog::memory(Default::default());
        let external = root.path().join("external");
        catalog
            .attach(
                "external",
                crate::catalog::Attach::Directory(external.clone()),
            )
            .unwrap();
        let repository = std::sync::Arc::new(
            super::super::open(
                &RepoConfig::from_url(
                    "local",
                    &format!("file://{}", external.join("backup").display()),
                )
                .unwrap(),
                &OpenEnv {
                    init: true,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let result = catalog.run_policy_in(
            &manual_policy("local", &["external"]),
            repository.clone(),
            &Control::default(),
        );
        assert_eq!(to_backup(result.unwrap_err()).code(), Code::InvalidConfig);
        assert!(
            super::super::block_on(repository.list(&ListFilter::default()))
                .unwrap()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn fresh_catalog_policy_validates_persisted_repository_without_opening_it() {
        for same_location in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let data = root.path().join("catalog");
            let registered_path = root.path().join("registered");
            let config =
                RepoConfig::from_url("local", &format!("file://{}", registered_path.display()))
                    .unwrap();
            let catalog = crate::Catalog::open(&data, Default::default()).unwrap();
            catalog.create("ds", &Default::default()).unwrap();
            let repositories = catalog.repositories().unwrap();
            repositories.add(config.clone()).unwrap();
            let old_id = repositories.open("local").unwrap().id();
            drop(repositories);
            drop(catalog);
            let registry_path = data
                .join("backup")
                .join(super::super::registry::REPOSITORIES_FILE);
            let registry_before = std::fs::read(&registry_path).unwrap();
            let supplied_config = if same_location {
                // A replacement marker must not satisfy the persisted expected UUID.
                std::fs::remove_dir_all(&registered_path).unwrap();
                config
            } else {
                RepoConfig::from_url(
                    "local",
                    &format!("file://{}", root.path().join("other").display()),
                )
                .unwrap()
            };
            let supplied = std::sync::Arc::new(
                super::super::open(
                    &supplied_config,
                    &OpenEnv {
                        init: true,
                        ..Default::default()
                    },
                )
                .unwrap(),
            );
            assert_ne!(supplied.id(), old_id);
            let catalog = crate::Catalog::open(&data, Default::default()).unwrap();
            assert!(
                catalog.inner.repositories.lock().is_none(),
                "fixture must begin with lazy metadata"
            );
            let result = catalog.run_policy_in(
                &manual_policy("local", &["absent"]),
                supplied.clone(),
                &Control::default(),
            );
            // Guard loads metadata only; neither backend admission nor a registry save
            // may replace its remembered UUID/config or open the registered location.
            assert_eq!(std::fs::read(&registry_path).unwrap(), registry_before);
            assert!(
                super::super::block_on(supplied.list(&ListFilter::default()))
                    .unwrap()
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                to_backup(result.unwrap_err()).code(),
                Code::InvalidRequest,
                "same_location={same_location}"
            );
            let loaded = catalog.repositories().unwrap();
            let entries = loaded.registry.repos.read();
            let entry = entries.get("local").unwrap();
            assert_eq!(entry.id, Some(old_id));
            assert!(
                entry.opened.is_none(),
                "metadata guard must not open registered backend"
            );
        }
    }
}
