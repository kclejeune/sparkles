//! The work of backup tasks: create, restore, verify and GC run on task threads, each
//! holding a task slot while it works, and count themselves in the metrics; delete runs
//! in the request. Claims ([`super::BackupState::claim`]) are the callers'.

use super::cli::plural;
use super::metrics::{Operation, Outcome};
use super::{BackupState, ClaimSpec, Started, swap};
use crate::state::{AppState, Dataset, Reservation, Task, TaskHandle};
use serde_json::{Value as J, json};
use sparkles_backup::{
    BackupError, BackupSummary, Code, CreateOptions, Ctl, GcOptions, GcReport, LastGc, RepoConfig,
    RestoreRequest, Verified, VerifyLevel, VerifyOptions, VerifyReport,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The backup state of a server that has one.
pub fn backup_state(st: &AppState) -> Result<Arc<BackupState>, BackupError> {
    if let Some(b) = &st.backup {
        st.catalog
            .share_repositories(b.registry.clone())
            .map_err(library_error)?;
    }
    st.backup.clone().ok_or_else(|| {
        BackupError::new(
            Code::NotImplemented,
            "backup repositories are not enabled on this server",
        )
    })
}

/// Cancellation and progress of task `h` for the engine; progress `p` is mapped to
/// `lo + p × (hi − lo)`.
fn ctl(h: &TaskHandle, lo: f32, hi: f32) -> Ctl {
    Ctl::from(&h.control().part(lo, hi))
}

/// Count an operation of repository `repo`.
fn count<T>(
    st: &AppState,
    b: &BackupState,
    repo: &str,
    op: Operation,
    r: &Result<(T, Outcome), BackupError>,
    t0: Instant,
) {
    let cap = st.metrics.max_datasets();
    let res = r.as_ref().map(|(_, o)| o);
    b.metrics.operation(cap, repo, op, res, t0.elapsed());
}

/// Log the end of an operation (INFO, or WARN on failure).
fn log_end<T>(what: &str, r: &Result<T, BackupError>, t0: Instant) {
    let ms = t0.elapsed().as_millis() as u64;
    match r {
        Ok(_) => tracing::info!(target: "sparkles::backup", millis = ms, "{what}: done"),
        Err(e) if e.is_cancelled() => {
            tracing::info!(target: "sparkles::backup", millis = ms, "{what}: cancelled")
        }
        Err(e) => tracing::warn!(
            target: "sparkles::backup",
            millis = ms,
            code = e.code().as_str(),
            "{what}: {e}"
        ),
    }
}

/// Preserve the backup component's structured error at HTTP/task boundaries.
pub fn library_error(e: sparkles::Error) -> BackupError {
    sparkles::backup::policy::to_backup(e)
}

// ----------------------------------------------------------------- create ------

/// What to call a backup.
#[derive(Clone, Debug, Default)]
pub struct CreateArgs {
    pub name: String,
    pub note: Option<String>,
    /// `(policy, run id)` of a policy run
    pub policy: Option<(String, String)>,
}

/// `409 repository-read-only` for a write to a read-only repository.
pub fn writable(cfg: &RepoConfig) -> Result<(), BackupError> {
    if cfg.readonly {
        return Err(BackupError::new(
            Code::RepositoryReadOnly,
            format!("repository \u{201c}{}\u{201d} is read-only", cfg.name),
        ));
    }
    Ok(())
}

/// Back up dataset `ds` into repository `repo` as `a.name`, in task `h` (which gets
/// the progress; its cancel flag cancels). Waits for a task slot first.
pub fn create(
    st: &Arc<AppState>,
    ds: &Arc<Dataset>,
    repo: &str,
    a: CreateArgs,
    h: &TaskHandle,
    started: Option<Started>,
) -> Result<BackupSummary, BackupError> {
    let b = backup_state(st)?;
    let _slot = b.slots.acquire(h, started)?;
    create_now(st, &b, ds, repo, a, ctl(h, 0.0, 1.0))
}

/// Back up dataset `dataset` into repository `repo` for a policy run, inside its
/// `backup-policy` task: like any backup it may not run next to another backup of the
/// same dataset into the same repository (`409 backup-in-progress`) and waits for a
/// task slot (`o.ctl` cancels the wait).
pub fn create_for_policy(
    st: &Arc<AppState>,
    dataset: &str,
    repo: &str,
    o: CreateOptions,
) -> Result<BackupSummary, BackupError> {
    let b = backup_state(st)?;
    let ds = st.get(dataset).ok_or_else(|| {
        BackupError::new(Code::InvalidRequest, format!("no such dataset: /{dataset}"))
    })?;
    writable(&b.registry.config(repo)?)?;
    // the claim names the policy's task
    let task = o
        .policy
        .as_ref()
        .and_then(|(p, _)| b.policies.running_task(p))
        .unwrap_or_else(|| "policy".into());
    let _claim = b.claim(
        &task,
        ClaimSpec {
            create: Some((ds.name.clone(), repo.to_string())),
            repo: Some(repo.to_string()),
            ..Default::default()
        },
    )?;
    let _slot = b.slots.acquire_quietly(&o.ctl.cancel)?;
    let a = CreateArgs {
        name: o.name,
        note: o.note,
        policy: o.policy,
    };
    create_now(st, &b, &ds, repo, a, o.ctl)
}

/// Back up now (the slot is held), counted and logged.
fn create_now(
    st: &AppState,
    b: &Arc<BackupState>,
    ds: &Arc<Dataset>,
    repo: &str,
    a: CreateArgs,
    ctl: Ctl,
) -> Result<BackupSummary, BackupError> {
    let t0 = Instant::now();
    let what = format!("backup {} of /{} into {repo}", a.name, ds.name);
    tracing::info!(target: "sparkles::backup", "{what}: started");
    let tmp = st.data_dir.join("tmp");
    let r = create_in(b, ds, repo, a, st.limits.min_free_disk_bytes, &tmp, ctl);
    count(st, b, repo, Operation::Create, &r, t0);
    log_end(&what, &r, t0);
    b.refresh_later(repo);
    r.map(|(s, _)| s)
}

fn create_in(
    b: &BackupState,
    ds: &Arc<Dataset>,
    repo_name: &str,
    a: CreateArgs,
    reserve: Option<u64>,
    tmp: &std::path::Path,
    ctl: Ctl,
) -> Result<(BackupSummary, Outcome), BackupError> {
    let repo = b.repo_with_ctl(repo_name, &ctl)?;
    let o = CreateOptions {
        name: a.name,
        note: a.note,
        policy: a.policy,
        dataset_name: ds.name.clone(),
        extra: Vec::new(),
        min_free_disk_bytes: reserve,
        ctl: ctl.clone(),
    };
    let create = || {
        ds.dataset
            .backups(&repo)
            .create_observed_with(&o, &ctl.control(), tmp, |cap| {
                b.metrics.capture_lock(cap.lock_hold)
            })
    };
    // Tokio's blocking pool still enters its runtime. The library's blocking API
    // runs outside that context, including when an embedding host drives this path.
    let s = if tokio::runtime::Handle::try_current().is_ok() {
        let span = tracing::Span::current();
        std::thread::scope(|scope| {
            scope
                .spawn(|| span.in_scope(create))
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        })
    } else {
        create()
    }
    .map_err(library_error)?;
    // the manifest (cached by the create) has the blob counts
    let stats = b
        .block_on(repo.manifest(&s.name))
        .ok()
        .and_then(Result::ok)
        .map(|m| m.stats)
        .unwrap_or_default();
    let out = Outcome {
        uploaded: s.added_bytes,
        blobs_uploaded: stats.new_blobs,
        blobs_reused: stats.reused_blobs,
        dataset: Some(ds.name.clone()),
        ..Default::default()
    };
    Ok((s, out))
}

// ---------------------------------------------------------------- restore ------

/// A restore to run (checked by the handler).
pub struct RestoreArgs {
    pub repo: String,
    pub backup: String,
    /// the backup's dataset id
    /// the dataset to create, or to replace with `req.replace`
    pub target: String,
    pub req: RestoreRequest,
    /// the reserved name of a new dataset (not in place)
    pub reservation: Option<Reservation>,
    /// the task id (names the temporary and replaced directories)
    pub task: String,
    /// who asked (audit)
    pub principal: String,
}

/// Restore a backup into a temporary directory next to the target, then publish it as
/// a new dataset or in place (see `swap`). Returns the task detail `{backup, dataset,
/// datasetId, identity, forkedFrom?, check, millis}`.
pub fn restore(
    st: &Arc<AppState>,
    a: RestoreArgs,
    h: &TaskHandle,
    started: Option<Started>,
) -> Result<J, BackupError> {
    let b = backup_state(st)?;
    let _slot = b.slots.acquire(h, started)?;
    let t0 = Instant::now();
    let what = format!(
        "restore of {}/{} into /{}{}",
        a.repo,
        a.backup,
        a.target,
        if a.req.replace { " (in place)" } else { "" }
    );
    tracing::info!(target: "sparkles::backup", "{what}: started");
    let repo_name = a.repo.clone();
    let target = a.target.clone();
    let principal = a.principal.clone();
    let r = restore_in(st, &b, a, h);
    count(st, &b, &repo_name, Operation::Restore, &r, t0);
    log_end(&what, &r, t0);
    if let Ok((d, _)) = &r {
        tracing::info!(
            target: "sparkles::audit",
            event = "restore_finished",
            target = target.as_str(),
            identity = d["identity"].as_str().unwrap_or(""),
            principal = principal.as_str()
        );
    }
    r.map(|(d, _)| d)
}

fn restore_in(
    st: &Arc<AppState>,
    b: &BackupState,
    mut a: RestoreArgs,
    h: &TaskHandle,
) -> Result<(J, Outcome), BackupError> {
    let t0 = Instant::now();
    let c = ctl(h, 0.0, 0.9);
    let repo = b.repo_with_ctl(&a.repo, &c)?;
    let databases = st.data_dir.join("databases");
    let tmp = databases.join(format!(
        "{}{}-{}",
        super::recover::RESTORE_PREFIX,
        a.target,
        a.task
    ));
    if tmp.exists() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    let mut store_opts = st.store_opts.clone();
    store_opts.min_free_disk_bytes = st
        .limits
        .min_free_disk_bytes
        .or(store_opts.min_free_disk_bytes);
    let report = match st.catalog.download_restore(
        &repo,
        &a.backup,
        &a.target,
        &a.req,
        &tmp,
        store_opts,
        &c.control(),
    ) {
        Ok(report) => report,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(library_error(e));
        }
    };
    // a cancel before the swap leaves everything as it was
    if let Err(e) = c.check() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let published = if a.req.replace {
        h.set_cancellable(false);
        h.progress(0.95, &format!("swapping /{}", a.target));
        swap::replace_in_place(st, &a.target, &tmp, &a.task, a.req.keep_replaced)
    } else {
        h.progress(0.95, &format!("publishing /{}", a.target));
        let r = a
            .reservation
            .take()
            .ok_or_else(|| BackupError::internal("no reservation for the new dataset"))?;
        h.set_cancellable(false);
        swap::publish_new(st, r, &tmp)
    };
    if let Err(e) = published {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let mut d = json!({
        "backup": report.backup,
        "dataset": a.target,
        "datasetId": report.dataset_id,
        "identity": report.identity,
        "check": report.check,
        "millis": t0.elapsed().as_millis() as u64,
    });
    if let Some(f) = report.forked_from {
        d["forkedFrom"] = json!(f);
    }
    let out = Outcome {
        downloaded: report.backup.logical_bytes,
        ..Default::default()
    };
    Ok((d, out))
}

// ----------------------------------------------------------------- verify ------

/// Verify backups `names` of repository `repo` (none: the whole repository, with
/// orphans) at `level`, and remember each backup's result (`verified`).
pub fn verify(
    st: &Arc<AppState>,
    repo: &str,
    names: Vec<String>,
    level: VerifyLevel,
    h: &TaskHandle,
    started: Option<Started>,
) -> Result<VerifyReport, BackupError> {
    let b = backup_state(st)?;
    let _slot = b.slots.acquire(h, started)?;
    let t0 = Instant::now();
    let what = match names.as_slice() {
        [] => format!("verification of {repo}"),
        [n] => format!("verification of {repo}/{n}"),
        _ => format!("verification of {} backups of {repo}", names.len()),
    };
    tracing::info!(target: "sparkles::backup", level = ?level, "{what}: started");
    let r = verify_in(st, &b, repo, &names, level, h);
    count(st, &b, repo, Operation::Verify, &r, t0);
    log_end(&what, &r, t0);
    r.map(|(v, _)| v)
}

fn verify_in(
    st: &AppState,
    b: &BackupState,
    repo_name: &str,
    names: &[String],
    level: VerifyLevel,
    h: &TaskHandle,
) -> Result<(VerifyReport, Outcome), BackupError> {
    let c = ctl(h, 0.0, 1.0);
    let repo = b.repo_with_ctl(repo_name, &c)?;
    // a `restore`-level verification restores into `<data>/tmp/verify-*` (removed by
    // the next start if the server stops meanwhile)
    let tmp = st.data_dir.join("tmp");
    if level == VerifyLevel::Restore {
        std::fs::create_dir_all(&tmp)?;
    }
    let o = VerifyOptions {
        level,
        tmp_dir: Some(tmp),
        store_opts: st.store_opts.clone(),
        ctl: c,
    };
    let report = b.block_on(repo.verify(names, &o))??;
    let at = sparkles_backup::now_rfc3339();
    b.verified.record(
        repo.id(),
        report.backups.iter().map(|v| {
            (
                v.name.clone(),
                Verified {
                    level,
                    status: v.status,
                    at: at.clone(),
                },
            )
        }),
    );
    Ok((report, Outcome::default()))
}

// --------------------------------------------------------------------- gc ------

/// Collect repository `repo`'s unreferenced blobs (`dry_run`: report only). A real run
/// records `lastGc`.
pub fn gc(
    st: &Arc<AppState>,
    repo: &str,
    dry_run: bool,
    grace: Duration,
    principal: &str,
    h: &TaskHandle,
    started: Option<Started>,
) -> Result<GcReport, BackupError> {
    let b = backup_state(st)?;
    let generation = b.registry.repos.read().get(repo).map(|e| e.generation());
    let _slot = b.slots.acquire(h, started)?;
    let t0 = Instant::now();
    let what = format!("GC of {repo}{}", if dry_run { " (dry run)" } else { "" });
    tracing::info!(target: "sparkles::backup", "{what}: started");
    let r = (|| {
        let c = ctl(h, 0.0, 1.0);
        let r = b.repo_with_ctl(repo, &c)?;
        let o = GcOptions {
            dry_run,
            grace,
            ctl: c,
        };
        let report = b.block_on(r.gc(&o))??;
        Ok((report, Outcome::default()))
    })();
    count(st, &b, repo, Operation::Gc, &r, t0);
    log_end(&what, &r, t0);
    if let Ok((report, _)) = &r
        && !dry_run
    {
        b.registry.update(repo, |e| {
            if Some(e.generation()) == generation {
                e.last_gc = Some(LastGc {
                    report: report.clone(),
                    finished: sparkles_backup::now_rfc3339(),
                });
            }
        });
        tracing::info!(
            target: "sparkles::audit",
            event = "gc_finished",
            repository = repo,
            deleted = report.deleted,
            deleted_bytes = report.deleted_bytes,
            principal = principal
        );
        b.refresh_later(repo);
    }
    r.map(|(g, _)| g)
}

/// Start a `backup-gc` task on repository `repo` (server-scoped, cancellable; the
/// repository counts as in use while it runs): `409 repository-read-only`. The
/// receiver is told once the task runs or queues. For `POST /$/repositories/{repo}/gc`
/// and `gcAfterRetention` (`admission`: that of the request, see
/// [`BackupState::admit`](super::BackupState::admit)).
pub fn start_gc(
    st: &Arc<AppState>,
    repo: &str,
    dry_run: bool,
    grace: Duration,
    principal: String,
    admission: Option<super::Admission>,
) -> Result<(Task, tokio::sync::oneshot::Receiver<()>), BackupError> {
    let b = backup_state(st)?;
    writable(&b.registry.config(repo)?)?;
    let id = st.next_task_id();
    let claim = b.claim(
        &id,
        ClaimSpec {
            repo: Some(repo.to_string()),
            ..Default::default()
        },
    )?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let st2 = st.clone();
    let name = repo.to_string();
    let task = st.start_task_opts(id, "backup-gc", "", Some(repo), true, move |h| {
        let _held = (claim, admission);
        let r = gc(&st2, &name, dry_run, grace, &principal, h, Some(tx)).map_err(task_error)?;
        h.set_detail(serde_json::to_value(&r)?);
        Ok(if dry_run {
            format!(
                "dry run: {} ({} MB) can be deleted",
                plural(r.candidates, "blob", "blobs"),
                mb(r.deleted_bytes)
            )
        } else {
            format!(
                "deleted {} ({} MB)",
                plural(r.deleted, "blob", "blobs"),
                mb(r.deleted_bytes)
            )
        })
    });
    Ok((task, rx))
}

/// A task's error for the task list (`code: message`; a cancellation stays one).
pub fn task_error(e: BackupError) -> anyhow::Error {
    let code = e.code();
    anyhow::Error::new(e).context(code.as_str())
}

fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1e6)
}

// ----------------------------------------------------------------- delete ------

/// Delete backup `name` of repository `repo` (its manifest; blobs wait for GC), unless
/// a restore or verification of it runs here (`409 backup-busy`).
pub async fn delete(
    st: &AppState,
    b: &Arc<BackupState>,
    repo: &str,
    name: &str,
    principal: &str,
) -> Result<(), BackupError> {
    delete_with_ctl(st, b, repo, name, principal, &Ctl::default()).await
}

pub async fn delete_with_ctl(
    st: &AppState,
    b: &Arc<BackupState>,
    repo: &str,
    name: &str,
    principal: &str,
    ctl: &Ctl,
) -> Result<(), BackupError> {
    if let Some(t) = b.backup_busy(repo, name) {
        return Err(
            BackupError::new(Code::BackupBusy, format!("task {t} uses {name}")).with("task", t),
        );
    }
    let t0 = Instant::now();
    let r = async {
        let r = b.open_repo_with_ctl(repo, ctl).await?;
        if !r.delete(name).await? {
            return Err(BackupError::new(
                Code::NoSuchBackup,
                format!("no backup \u{201c}{name}\u{201d} in {repo}"),
            ));
        }
        b.verified.remove(r.id(), name);
        Ok(((), Outcome::default()))
    }
    .await;
    count(st, b, repo, Operation::Delete, &r, t0);
    if r.is_ok() {
        tracing::info!(
            target: "sparkles::audit",
            event = "backup_deleted",
            repository = repo,
            backup = name,
            principal = principal
        );
        b.refresh_later(repo);
    }
    r.map(|_| ())
}
