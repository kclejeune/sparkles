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
    if !cancelled && !disabled {
        ctl.progress.report(0.95, "retention");
        let retention = match apply_retention(engine, p, false, engine.now()) {
            Ok(r) => RunRetention {
                deleted: r.delete.into_iter().map(|b| b.name).collect(),
                error: r.errors.map(|e| e.join("; ")),
            },
            Err(e) => RunRetention {
                deleted: Vec::new(),
                error: Some(e.message().into()),
            },
        };
        if p.gc_after_retention && !retention.deleted.is_empty() {
            run.gc = engine
                .start_gc(&p.repository)
                .ok()
                .map(|task| RunGc { task });
        }
        run.retention = Some(retention);
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
}
impl Engine for CatalogEngine<'_> {
    fn datasets(&self) -> Vec<DatasetInfo> {
        self.catalog
            .list()
            .into_iter()
            .filter_map(|d| {
                self.catalog.get(&d.name).map(|ds| DatasetInfo {
                    name: d.name,
                    id: d.id,
                    head: ds.head_commit().seq,
                })
            })
            .collect()
    }
    fn list(&self, _: &str, policy: &str) -> std::result::Result<Vec<BackupSummary>, BackupError> {
        block_on(self.repository.list(&ListFilter {
            policy: Some(policy.into()),
            ..Default::default()
        }))
        .map_err(to_backup)?
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
        block_on(self.repository.delete(backup)).map_err(to_backup)?
    }
    fn busy(&self, _: &str) -> HashSet<String> {
        HashSet::new()
    }
    fn start_gc(&self, _: &str) -> std::result::Result<String, BackupError> {
        block_on(self.repository.gc(&GcOptions::default())).map_err(to_backup)??;
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
        let engine = CatalogEngine {
            catalog: self,
            repository: self.repositories()?.open(&p.repository)?,
        };
        let report = run(
            &engine,
            p,
            RunTrigger::Manual,
            None,
            Utc::now(),
            ctl,
            || true,
        )
        .map_err(error)?;
        ctl.check()?;
        ctl.progress.report(1.0, "policy completed");
        Ok(report)
    }
    pub fn apply_retention(&self, p: &PolicyConfig, dry_run: bool) -> Result<RetentionResponse> {
        let engine = CatalogEngine {
            catalog: self,
            repository: self.repositories()?.open(&p.repository)?,
        };
        apply_retention(&engine, p, dry_run, Utc::now()).map_err(error)
    }
}
