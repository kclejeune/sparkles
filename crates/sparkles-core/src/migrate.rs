//! Upgrades of a closed database's files to the versions this build writes, for
//! `sparkles migrate`.
//!
//! [`plan`] reads the version of every versioned file of a database (see
//! [`crate::formats`]) and lists the registered [`Step`]s that would upgrade the ones
//! not at the version this build writes. [`migrate`] runs them on a copy of the
//! database in a sibling directory, checks the copy with [`crate::check`], and swaps it
//! in by two renames. A copy that passed the check carries a mark, so that a run that
//! stopped between the renames is finished by [`recover`], which [`migrate`] calls
//! first, and a store refuses to open the database meanwhile (see [`pending`]).
//!
//! [`plan`] never changes anything. [`migrate`] holds the migration lock, a file next to
//! the database that covers planning, recovery, the copy and both renames, and the
//! database's own lock until the renames start. It refuses a database that holds a
//! symlink, since the copy would leave it out, and one whose `.pre-migrate-*` directory
//! exists.
//!
//! A step is a function from one version of a file kind to the next. Register a new one
//! in [`STEPS`]. Its version must be in the kind's `reads` in [`crate::formats`] unless
//! the step is the only way to read it (an explicit migration), in which case the
//! store's refusal of the old version names `sparkles migrate`.
//!
//! The server never runs steps itself. It reads the versions inside the window
//! directly, upgrades a file lazily the next time it writes it, and rebuilds derived
//! indexes in the background.

use crate::error::{Error, Result};
use crate::formats::{self, FileKind};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// An upgrade of one file kind from one version to the next.
pub struct Step {
    /// the [`FileKind::id`] it upgrades
    pub kind: &'static str,
    pub from: u32,
    pub to: u32,
    /// what it does, for `--check`
    pub summary: &'static str,
    /// Upgrade the file at the path (relative to the database's directory) of a closed
    /// database in `root`.
    pub run: fn(root: &Path, file: &Path) -> Result<()>,
}

/// The registered steps.
pub const STEPS: &[Step] = &[Step {
    kind: "delta-vocab",
    from: 1,
    to: 2,
    summary: "rewrite a legacy delta vocabulary as a framed one, as the first write to it would",
    run: delta_vocab_framed,
}];

fn delta_vocab_framed(root: &Path, file: &Path) -> Result<()> {
    crate::commit::require_reader(root, crate::vocab::delta::FRAMED_READER)?;
    crate::vocab::delta::convert_legacy_file(&root.join(file))?;
    Ok(())
}

/// Whether a registered step upgrades version `from` of the kind `kind`.
pub fn upgrades(kind: &str, from: u32) -> bool {
    STEPS.iter().any(|s| s.kind == kind && s.from == from)
}

fn step(kind: &str, from: u32) -> Option<&'static Step> {
    STEPS.iter().find(|s| s.kind == kind && s.from == from)
}

/// The version of one file.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileVersion {
    pub kind: &'static str,
    /// relative to the database's directory
    pub file: String,
    pub version: u32,
    /// whether this build reads the version without an upgrade
    pub readable: bool,
}

/// A step the upgrade of a database would run.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedStep {
    pub kind: &'static str,
    pub file: String,
    pub from: u32,
    pub to: u32,
    pub summary: &'static str,
}

/// What [`plan`] found in a database.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub root: PathBuf,
    /// the versions of dataset.json, if it exists and this build reads it
    pub dataset: Option<crate::commit::DatasetVersions>,
    pub files: Vec<FileVersion>,
    pub steps: Vec<PlannedStep>,
    /// files this build reads neither directly nor after a step, with the refusal
    pub refused: Vec<String>,
    /// a migration that did not finish, which the next run without `--check` finishes or
    /// undoes
    pub interrupted: Option<String>,
}

impl Plan {
    /// Whether this build opens the database, directly or after the planned steps.
    pub fn in_window(&self) -> bool {
        self.refused.is_empty()
    }
}

fn add(
    plan: &mut Plan,
    kind: &'static FileKind,
    file: String,
    version: u32,
    written_by: Option<&str>,
) {
    let readable = kind.reads(version);
    if version != kind.writes {
        // follow the steps from this version as far as they go
        let mut v = version;
        while let Some(s) = step(kind.id, v) {
            plan.steps.push(PlannedStep {
                kind: kind.id,
                file: file.clone(),
                from: s.from,
                to: s.to,
                summary: s.summary,
            });
            v = s.to;
        }
        if !readable && !kind.reads(v) {
            plan.refused
                .push(kind.refuse(&file, version, written_by).to_string());
        }
    }
    plan.files.push(FileVersion {
        kind: kind.id,
        file,
        version,
        readable,
    });
}

fn generations(dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("gen-") && e.file_type()?.is_dir() {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

/// Read the versions of a database's files and list the steps an upgrade would run.
/// Nothing is changed, and a migration that did not finish is only reported in
/// [`Plan::interrupted`]. The migration lock is taken, so a plan of a database that
/// [`migrate`] is working on fails with [`Error::Locked`] and does not read directories
/// that are being renamed.
pub fn plan(root: &Path) -> Result<Plan> {
    let root = &absolute(root)?;
    require_named(root)?;
    // A directory that cannot be written to has no migration running in it, so a plan
    // there reads without the lock.
    let _mlock = match lock_migration(root) {
        Ok(l) => Some(l),
        Err(Error::Io(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            None
        }
        Err(e) => return Err(e),
    };
    plan_locked(root)
}

/// [`plan`] with the migration lock held.
fn plan_locked(root: &Path) -> Result<Plan> {
    let written_by = crate::commit::read_written_by(root);
    let by = written_by.as_deref();
    let mut plan = Plan {
        root: root.to_path_buf(),
        dataset: None,
        files: Vec::new(),
        steps: Vec::new(),
        refused: Vec::new(),
        interrupted: None,
    };
    let work = work_dir(root);
    if work.is_dir() {
        plan.interrupted = Some(format!(
            "an interrupted migration left {}; run without --check to finish or undo it",
            work.display()
        ));
        if !root.exists() {
            // the checked copy is not in place yet, so there is nothing to read
            return Ok(plan);
        }
    }
    inspect_dir(&mut plan, root, "", by)?;
    // branch stores, retained ones included, live under branches/<id>/
    let branches = root.join("branches");
    if branches.is_dir() {
        let mut dirs: Vec<_> = std::fs::read_dir(&branches)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        dirs.sort();
        for d in dirs {
            inspect_dir(&mut plan, &branches.join(&d), &format!("branches/{d}/"), by)?;
        }
    }
    Ok(plan)
}

/// What opening the store in `dir` checks: its `dataset.json`, its registered settings
/// files other than the informational ones, and the files of its generations. `prefix`
/// names a branch's directory in the messages and is empty for the root, whose
/// `dataset.json` versions the plan reports.
fn inspect_dir(plan: &mut Plan, dir: &Path, prefix: &str, by: Option<&str>) -> Result<()> {
    match crate::commit::read_dataset_versions(dir) {
        Ok(d) if prefix.is_empty() => plan.dataset = d,
        Ok(_) => {}
        Err(e) => plan.refused.push(format!("{prefix}{e}")),
    }
    for e in formats::dataset_dir_refusals(dir) {
        // the message already names the directory
        plan.refused.push(e.to_string());
    }
    inspect_store(plan, dir, prefix, by)
}

fn inspect_store(plan: &mut Plan, dir: &Path, prefix: &str, by: Option<&str>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for g in generations(dir)? {
        let gdir = dir.join(&g);
        let rel = format!("{prefix}{g}");
        if let Ok(b) = std::fs::read(gdir.join("meta.json")) {
            let meta: crate::builder::IndexMeta = serde_json::from_slice(&b)
                .map_err(|e| Error::Corrupt(format!("{rel}/meta.json: {e}")))?;
            add(plan, &formats::INDEX, rel.clone(), meta.format_version, by);
        }
        let wal = gdir.join("wal.log");
        if wal.exists() {
            let v = match crate::store::wal::file_version(&wal) {
                Ok(v) => v,
                Err(Error::Unsupported(_)) => newer_than(&formats::WAL),
                Err(e) => return Err(e),
            };
            add(plan, &formats::WAL, format!("{rel}/wal.log"), v, by);
        }
        let dv = gdir.join("delta.vocab");
        if dv.exists() {
            let v = crate::vocab::delta::file_version(&dv)?;
            // a legacy file without entries is written framed by its first write
            let empty = v == 1 && std::fs::metadata(&dv)?.len() == 0;
            if !empty {
                add(
                    plan,
                    &formats::DELTA_VOCAB,
                    format!("{rel}/delta.vocab"),
                    v,
                    by,
                );
            }
        }
    }
    Ok(())
}

/// A version past every one `kind` reads, for a file whose header refused to parse.
fn newer_than(kind: &FileKind) -> u32 {
    kind.reads.last().copied().unwrap_or(0) + 1
}

/// What [`migrate`] did.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub root: PathBuf,
    pub steps: Vec<PlannedStep>,
    /// where the database before the upgrade was kept, with `keep_old`
    pub kept: Option<PathBuf>,
}

/// The file that marks a work directory as checked. It is written inside the work
/// directory after the copy passes [`crate::check::check`], with `keep-old` or
/// `remove-old`, and removed from the database's directory once the swap is done.
const CHECKED_MARK: &str = ".migrate-checked";
const MARK_KEEP_OLD: &str = "keep-old";
const MARK_REMOVE_OLD: &str = "remove-old";

/// The sibling directory a migration builds the upgraded copy in.
fn work_dir(root: &Path) -> PathBuf {
    sibling(root, ".migrate-")
}

/// The sibling directory the database before the upgrade is moved to.
fn old_dir(root: &Path) -> PathBuf {
    sibling(
        root,
        &format!(".pre-migrate-{}-", formats::SPARKLES_VERSION),
    )
}

fn sibling(root: &Path, prefix: &str) -> PathBuf {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    root.with_file_name(format!("{prefix}{name}"))
}

/// The work directory of a [`migrate`] of `root` that has not finished, if there is one.
/// Nothing is changed. A database with a pending migration is not opened (see
/// [`check_not_pending`]), and the next [`migrate`] of it finishes or undoes the run.
pub fn pending(root: &Path) -> Result<Option<PathBuf>> {
    let root = absolute(root)?;
    if root.file_name().is_none() {
        return Ok(None);
    }
    let work = work_dir(&root);
    Ok(work.is_dir().then_some(work))
}

/// Refuse a database whose migration has not finished, for [`crate::store::Store::open`].
/// The root may be missing, which is what a run that stopped between its two renames
/// leaves, and opening it would create an empty database next to the checked copy.
pub(crate) fn check_not_pending(root: &Path) -> Result<()> {
    match pending(root)? {
        None => Ok(()),
        Some(w) => Err(Error::Unsupported(format!(
            "{}: an interrupted `sparkles migrate` left {}; run `sparkles migrate` on this database again (without --check) to finish or undo it before opening it",
            absolute(root)?.display(),
            w.display()
        ))),
    }
}

/// Finish or undo a [`migrate`] of `root` that stopped part way, with the locks of
/// [`migrate_with`] held.
///
/// * Without a work directory there is nothing to do, except that a mark left in the
///   database by a run that stopped right after the swap is cleaned up.
/// * A copy next to an existing database was never swapped in, and is removed.
/// * With the database missing, a checked copy takes its place, since the run stopped
///   between its two renames. A work directory without the mark of a passed check is
///   not touched and is reported, because only outside interference produces it.
fn recover_locked(root: &Path) -> Result<Option<std::fs::File>> {
    let work = work_dir(root);
    let old = old_dir(root);
    if !work.is_dir() {
        // the run stopped after the second rename and before it removed its mark
        if let Some(mode) = read_mark(root)? {
            finish_swap(root, &old, &mode)?;
        }
        return Ok(None);
    }
    if root.exists() {
        if read_mark(&work)?.is_some() && old.exists() {
            // The first rename has happened, because a run does not start while the old
            // directory exists, and something created the root again since, such as an
            // open that was already past its checks. Neither side is removed.
            return Err(Error::Corrupt(format!(
                "{} exists next to the checked copy {} and the previous files {}; move the new {} away and run again, or restore the previous files by hand",
                root.display(),
                work.display(),
                old.display(),
                root.display()
            )));
        }
        // the copy was never swapped in: the database is as it was
        std::fs::remove_dir_all(&work)?;
        return Ok(None);
    }
    let Some(mode) = read_mark(&work)? else {
        return Err(Error::Corrupt(format!(
            "{} is missing and {} has no mark of a passed check; the previous files may be in {}. Restore the database by hand",
            root.display(),
            work.display(),
            old.display()
        )));
    };
    // the old directory was moved aside and the copy, already checked, was not yet
    // renamed into place
    std::fs::rename(&work, root)?;
    sync_parent(root)?;
    // the database is locked before anything else can open it
    let lock = crate::store::lock_database(root)?;
    finish_swap(root, &old, &mode)?;
    Ok(Some(lock))
}

/// The content of the checked mark in `dir`, if it has one.
fn read_mark(dir: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(dir.join(CHECKED_MARK)) {
        Ok(m) => Ok(Some(m.trim().to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// After the checked copy is in place at `root`: remove its mark, and the old directory
/// unless the run kept it.
fn finish_swap(root: &Path, old: &Path, mode: &str) -> Result<()> {
    // the old directory goes first, so that the mark still records the unfinished
    // cleanup if the removal is interrupted, and the next run retries it
    if mode != MARK_KEEP_OLD && old.exists() {
        std::fs::remove_dir_all(old)?;
        sync_parent(root)?;
    }
    std::fs::remove_file(root.join(CHECKED_MARK))?;
    crate::store::sync_dir(root)
}

/// Finish or undo a [`migrate`] of `root` that stopped part way, under the migration
/// lock and the database's own lock. [`migrate`] does the same before it plans.
pub fn recover(root: &Path) -> Result<()> {
    let root = &absolute(root)?;
    require_named(root)?;
    let _mlock = lock_migration(root)?;
    let db = if root.exists() {
        Some(crate::store::lock_database(root)?)
    } else {
        None
    };
    // a rolled-forward database stays locked until this returns
    let _rolled = recover_locked(root)?;
    drop(db);
    Ok(())
}

/// A root without a final component (`/` or `..`) has no sibling to work in.
fn require_named(root: &Path) -> Result<()> {
    if root.file_name().is_none() {
        return Err(Error::Invalid(format!(
            "{} does not name a database directory",
            root.display()
        )));
    }
    Ok(())
}

/// The lock that serializes migrations of one database: a file next to the database,
/// `.migrate.lock-<name>`, which survives both renames of a swap. It is left in place
/// after the run, since removing a lock file races with the processes waiting for it.
fn lock_migration(root: &Path) -> Result<std::fs::File> {
    use std::io::{Read, Write};
    // a name that no work or old directory of another database can have: `.migrate-a.lock`
    // would be the work directory of a database named `a.lock`
    let path = sibling(root, ".migrate.lock-");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    match f.try_lock() {
        Ok(()) => {
            f.set_len(0)?;
            writeln!(f, "{}", std::process::id())?;
            Ok(f)
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut pid = String::new();
            let _ = std::fs::File::open(&path).and_then(|mut f| f.read_to_string(&mut pid));
            Err(Error::Locked {
                path,
                pid: pid.trim().parse().ok(),
            })
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// The path as an absolute one, without resolving symlinks and without the path having
/// to exist. A relative root would otherwise name a different directory once the
/// database is moved aside, and recovery has to work on a root that is missing.
fn absolute(root: &Path) -> Result<PathBuf> {
    Ok(std::path::absolute(root)?)
}

fn sync_parent(root: &Path) -> Result<()> {
    let parent = match root.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    crate::store::sync_dir(parent)
}

/// Mark the work directory as checked, durably.
fn write_mark(work: &Path, mode: &str) -> Result<()> {
    let path = work.join(CHECKED_MARK);
    let mut f = std::fs::File::create(&path)?;
    std::io::Write::write_all(&mut f, mode.as_bytes())?;
    f.sync_all()?;
    crate::store::sync_dir(work)?;
    // the work directory's own entry must be durable before the root is moved aside
    sync_parent(work)
}

/// Copy the files and directories of `from` into the existing directory `to`. The
/// process lock and the checked mark stay behind. An entry that is neither a regular
/// file nor a directory is refused, since a copy would silently leave it out.
fn copy_contents(from: &Path, to: &Path) -> Result<()> {
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let name = e.file_name();
        if name == "sparkles.lock" || name == CHECKED_MARK {
            continue;
        }
        let t = e.file_type()?;
        if t.is_dir() {
            std::fs::create_dir(to.join(&name))?;
            copy_contents(&e.path(), &to.join(&name))?;
        } else if t.is_file() {
            std::fs::copy(e.path(), to.join(&name))?;
            std::fs::File::open(to.join(&name))?.sync_all()?;
        } else {
            return Err(Error::Invalid(format!(
                "{}: `sparkles migrate` does not copy symlinks or special files; replace it with the files it points to and run again",
                e.path().display()
            )));
        }
    }
    crate::store::sync_dir(to)
}

/// Upgrade the closed database at `root`: run the steps of [`plan`] on a copy, check the
/// copy, and swap it in. With `keep_old`, the database as it was stays in a sibling
/// directory named `.pre-migrate-<version>-<name>`, otherwise it is removed. A database
/// outside the window is refused, as is one whose copy fails the check, one that holds a
/// symlink, and one whose `.pre-migrate-<version>-<name>` exists already. A database that
/// a store has open fails with [`Error::Locked`], and so does a second migration of it.
/// The catalog's lock is held by the caller, so no server has the database open.
pub fn migrate(root: &Path, keep_old: bool) -> Result<Outcome> {
    migrate_with(root, keep_old, &mut |_| Ok(()))
}

/// [`migrate`] with a hook called at named points (`copied`, `checked`, `moved-aside`,
/// `swapped`), for interruption tests.
#[doc(hidden)]
pub fn migrate_with(
    root: &Path,
    keep_old: bool,
    hook: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<Outcome> {
    let (plan, out) = migrate_planned_with(root, keep_old, hook)?;
    match out {
        Some(o) => Ok(o),
        None => Err(Error::Unsupported(plan.refused.join("; "))),
    }
}

/// [`migrate`] that also returns the [`Plan`] it ran under the locks, for a report. The
/// outcome is `None` when the database is outside the window, which the plan explains.
pub fn migrate_planned(root: &Path, keep_old: bool) -> Result<(Plan, Option<Outcome>)> {
    migrate_planned_with(root, keep_old, &mut |_| Ok(()))
}

fn migrate_planned_with(
    root: &Path,
    keep_old: bool,
    hook: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<(Plan, Option<Outcome>)> {
    let root = &absolute(root)?;
    require_named(root)?;
    // Exclusion runs through the whole command. The migration lock covers planning,
    // recovery, the copy and both renames. The database's own lock comes before recovery
    // so that a live store makes the command fail before anything is touched.
    let _mlock = lock_migration(root)?;
    let mut db_lock = if root.exists() {
        Some(crate::store::lock_database(root)?)
    } else {
        None
    };
    if let Some(rolled) = recover_locked(root)? {
        // recovery rolled a checked copy forward and kept it locked
        db_lock = Some(rolled);
    }

    let plan = plan_locked(root)?;
    if !plan.in_window() {
        return Ok((plan, None));
    }
    let mut out = Outcome {
        root: root.to_path_buf(),
        steps: plan.steps.clone(),
        kept: None,
    };
    if plan.steps.is_empty() {
        return Ok((plan, Some(out)));
    }
    let old = old_dir(root);
    if old.exists() {
        return Err(Error::Invalid(format!(
            "{} exists from an earlier run; move or remove it before migrating again",
            old.display()
        )));
    }
    let work = work_dir(root);
    std::fs::create_dir(&work)?;
    copy_contents(root, &work).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&work);
    })?;
    hook("copied")?;
    for p in &plan.steps {
        let s = step(p.kind, p.from).expect("planned from STEPS");
        (s.run)(&work, Path::new(&p.file)).inspect_err(|_| {
            let _ = std::fs::remove_dir_all(&work);
        })?;
    }
    let report = crate::check::check(&work, &crate::check::CheckOptions { quick: false })?;
    if report.status == crate::check::Status::Error {
        let _ = std::fs::remove_dir_all(&work);
        return Err(Error::Corrupt(format!(
            "{}: the upgraded copy failed `sparkles check` with {} error(s); the database is unchanged",
            root.display(),
            report.errors
        )));
    }
    let mode = if keep_old {
        MARK_KEEP_OLD
    } else {
        MARK_REMOVE_OLD
    };
    write_mark(&work, mode)?;
    hook("checked")?;
    // The database's lock is released before its directory is renamed, since a directory
    // with an open handle cannot be renamed on every platform. From here on exclusion
    // comes from the migration lock and from the pending work directory, which
    // Store::open refuses after it has taken the database's lock, so no store can open
    // the old files while they are copied away or write to files the copy lacks.
    drop(db_lock);
    std::fs::rename(root, &old)?;
    sync_parent(root)?;
    hook("moved-aside")?;
    std::fs::rename(&work, root)?;
    sync_parent(root)?;
    hook("swapped")?;
    finish_swap(root, &old, mode)?;
    if keep_old {
        out.kept = Some(old);
    }
    Ok((plan, Some(out)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, StoreOptions};

    /// A database whose current delta vocabulary is legacy (as a release before framed
    /// files wrote it), with one commit that uses its terms.
    fn legacy_database(root: &Path) -> String {
        let s = Store::open(root, StoreOptions::default()).unwrap();
        crate::sparql::update::update(
            &s,
            "INSERT DATA { <http://e/a> <http://e/p> \"new term\" }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let gen_name = s.snapshot().generation.name.clone();
        drop(s);
        let dv = root.join(&gen_name).join("delta.vocab");
        let b = std::fs::read(&dv).unwrap();
        let parsed = crate::vocab::delta::parse_delta(&b).unwrap();
        let mut legacy = Vec::new();
        for k in &parsed.keys {
            legacy.extend_from_slice(&(k.len() as u32).to_le_bytes());
            legacy.extend_from_slice(k);
        }
        assert!(!legacy.is_empty());
        std::fs::write(&dv, &legacy).unwrap();
        gen_name
    }

    fn count(root: &Path) -> usize {
        let s = Store::open(root, StoreOptions::default()).unwrap();
        s.snapshot().len() as usize
    }

    #[test]
    fn a_legacy_delta_vocabulary_is_upgraded_on_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let g = legacy_database(&root);
        let p = plan(&root).unwrap();
        assert!(p.in_window(), "{:?}", p.refused);
        assert_eq!(p.steps.len(), 1, "{:?}", p.steps);
        assert_eq!(p.steps[0].file, format!("{g}/delta.vocab"));
        let out = migrate(&root, true).unwrap();
        assert_eq!(out.steps.len(), 1);
        let kept = out.kept.unwrap();
        assert!(kept.join(&g).join("delta.vocab").exists());
        assert_eq!(
            crate::vocab::delta::file_version(&root.join(&g).join("delta.vocab")).unwrap(),
            2
        );
        assert!(plan(&root).unwrap().steps.is_empty());
        assert_eq!(count(&root), 1);
        // nothing left to do
        assert!(migrate(&root, false).unwrap().steps.is_empty());
    }

    #[test]
    fn an_interrupted_migration_finishes_or_leaves_the_database_intact() {
        for at in ["copied", "checked", "moved-aside"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("db");
            let g = legacy_database(&root);
            let e = migrate_with(&root, false, &mut |p| {
                if p == at {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(e.is_err(), "{at}");
            // recovery alone: before the swap the database is as it was, after it the
            // checked copy is in place
            recover(&root).unwrap();
            assert!(root.exists(), "{at}");
            assert_nothing_pending(dir.path(), &root, at);
            let v = crate::vocab::delta::file_version(&root.join(&g).join("delta.vocab")).unwrap();
            assert_eq!(v, if at == "moved-aside" { 2 } else { 1 }, "{at}");
            assert_eq!(count(&root), 1, "{at}");
        }
    }

    #[test]
    fn the_next_migrate_finishes_an_interrupted_one() {
        for at in ["copied", "checked", "moved-aside"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("db");
            let g = legacy_database(&root);
            let e = migrate_with(&root, false, &mut |p| {
                if p == at {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(e.is_err(), "{at}");
            migrate(&root, false).unwrap();
            assert!(root.exists(), "{at}");
            assert_nothing_pending(dir.path(), &root, at);
            let v = crate::vocab::delta::file_version(&root.join(&g).join("delta.vocab")).unwrap();
            assert_eq!(v, 2, "{at}");
            assert_eq!(count(&root), 1, "{at}");
        }
    }

    #[test]
    fn sync_parent_of_a_single_component_path() {
        sync_parent(Path::new("db")).unwrap();
    }

    /// Serializes the tests that change the working directory.
    static CWD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct RestoreCwd(PathBuf);
    impl Drop for RestoreCwd {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    #[test]
    fn a_relative_root_is_migrated() {
        let _g = CWD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let g = legacy_database(&root);
        let _restore = RestoreCwd(std::env::current_dir().unwrap());
        std::env::set_current_dir(dir.path()).unwrap();
        let out = migrate(Path::new("db"), false).unwrap();
        assert_eq!(out.steps.len(), 1);
        assert!(root.exists());
        assert_eq!(
            crate::vocab::delta::file_version(&root.join(&g).join("delta.vocab")).unwrap(),
            2
        );
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            // the migration lock file stays next to the database
            .filter(|n| !n.starts_with(".migrate.lock-"))
            .collect();
        assert_eq!(left, vec!["db".to_string()], "{left:?}");
    }

    #[test]
    fn a_newer_index_is_refused_and_named() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        drop(Store::open(&root, StoreOptions::default()).unwrap());
        let meta = root.join("gen-0001").join("meta.json");
        let mut m: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        m["format_version"] = 99.into();
        std::fs::write(&meta, serde_json::to_vec(&m).unwrap()).unwrap();
        let p = plan(&root).unwrap();
        assert!(!p.in_window());
        assert!(p.refused[0].contains("index version 99"), "{:?}", p.refused);
        let e = migrate(&root, false).unwrap_err().to_string();
        assert!(e.contains("newer release"), "{e}");
    }

    /// A database with one branch, and the directory of the branch.
    fn branched_database(root: &Path) -> PathBuf {
        let s = Store::open(root, StoreOptions::default()).unwrap();
        s.create_branch("dev", &Default::default()).unwrap();
        drop(s);
        let mut dirs: Vec<_> = std::fs::read_dir(root.join("branches"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.is_dir())
            .collect();
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        dirs.pop().unwrap()
    }

    #[test]
    fn a_branch_with_a_newer_settings_file_is_outside_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let branch = branched_database(&root);
        let id = branch.file_name().unwrap().to_string_lossy().into_owned();
        assert!(plan(&root).unwrap().in_window());
        std::fs::write(branch.join("compaction.json"), br#"{"format": 2}"#).unwrap();
        let p = plan(&root).unwrap();
        assert!(!p.in_window());
        assert!(
            p.refused
                .iter()
                .any(|r| r.contains(&format!("branches/{id}")) && r.contains("compaction.json")),
            "{:?}",
            p.refused
        );
        let e = migrate(&root, false).expect_err("refused");
        assert!(matches!(e, Error::Unsupported(_)), "{e:?}");
    }

    #[test]
    fn a_branch_dataset_json_needing_a_later_reader_is_outside_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let branch = branched_database(&root);
        let path = branch.join("dataset.json");
        if let Ok(b) = std::fs::read(&path) {
            let mut v: serde_json::Value = serde_json::from_slice(&b).unwrap();
            v["minimumReader"] = 99.into();
            std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        } else {
            // a branch directory without its own dataset.json takes the root's id
            let b = std::fs::read(root.join("dataset.json")).unwrap();
            let mut v: serde_json::Value = serde_json::from_slice(&b).unwrap();
            v["minimumReader"] = 99.into();
            std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        }
        let p = plan(&root).unwrap();
        assert!(!p.in_window(), "{:?}", p.refused);
    }

    #[test]
    fn a_newer_origin_file_stays_inside_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        drop(Store::open(&root, StoreOptions::default()).unwrap());
        std::fs::write(root.join("origin.json"), br#"{"originFormat": 9}"#).unwrap();
        std::fs::write(root.join("restore.json"), br#"{"restoreFormat": 9}"#).unwrap();
        let p = plan(&root).unwrap();
        assert!(p.in_window(), "{:?}", p.refused);
    }

    /// Every path under `dir` whose file name is `name`.
    fn find(dir: &Path, name: &str) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.file_name().is_some_and(|n| n == name) {
                out.push(p.clone());
            }
            if p.is_dir() {
                out.extend(find(&p, name));
            }
        }
        out
    }

    fn assert_nothing_pending(parent: &Path, root: &Path, at: &str) {
        assert!(!work_dir(root).exists(), "{at}");
        assert!(!old_dir(root).exists(), "{at}");
        assert!(find(parent, CHECKED_MARK).is_empty(), "{at}");
    }

    #[test]
    fn an_open_after_an_interrupted_swap_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let g = legacy_database(&root);
        let e = migrate_with(&root, false, &mut |p| {
            if p == "moved-aside" {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(e.is_err());
        let e = Store::open(&root, StoreOptions::default())
            .err()
            .expect("an open of a database with a pending swap is refused");
        assert!(matches!(e, Error::Unsupported(_)), "{e:?}");
        assert!(e.to_string().contains(".migrate-db"), "{e}");
        assert!(!root.exists(), "the open created the database");
        // the next migrate finishes the swap
        migrate(&root, false).unwrap();
        assert_eq!(count(&root), 1);
        assert_eq!(
            crate::vocab::delta::file_version(&root.join(&g).join("delta.vocab")).unwrap(),
            2
        );
        assert_nothing_pending(dir.path(), &root, "after the finish");
    }

    #[test]
    fn an_open_during_a_copy_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let g = legacy_database(&root);
        let mut refused = false;
        migrate_with(&root, false, &mut |p| {
            if p == "copied" {
                refused = Store::open(&root, StoreOptions::default()).is_err();
            }
            Ok(())
        })
        .unwrap();
        assert!(refused);
        assert_eq!(
            crate::vocab::delta::file_version(&root.join(&g).join("delta.vocab")).unwrap(),
            2
        );
        assert_eq!(count(&root), 1);
    }

    #[test]
    fn a_missing_root_without_a_checked_copy_is_not_rolled_forward() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let work = work_dir(&root);
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("x"), b"x").unwrap();
        let e = migrate(&root, false).expect_err("refused");
        assert!(matches!(e, Error::Corrupt(_)), "{e:?}");
        assert!(work.join("x").exists());
        assert!(!root.exists());
    }

    #[test]
    fn a_run_that_stopped_after_the_swap_is_cleaned_up_by_the_next_one() {
        for keep in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("db");
            let g = legacy_database(&root);
            let e = migrate_with(&root, keep, &mut |p| {
                if p == "swapped" {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(e.is_err());
            // the new database is in place with the mark of the unfinished cleanup
            assert!(root.join(CHECKED_MARK).exists());
            assert!(old_dir(&root).exists());
            recover(&root).unwrap();
            assert!(!root.join(CHECKED_MARK).exists(), "keep {keep}");
            assert_eq!(old_dir(&root).exists(), keep, "keep {keep}");
            assert!(!work_dir(&root).exists());
            assert_eq!(
                crate::vocab::delta::file_version(&root.join(&g).join("delta.vocab")).unwrap(),
                2
            );
            assert_eq!(count(&root), 1);
        }
    }

    #[test]
    fn a_concurrent_check_does_not_disturb_a_running_migration() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let g = legacy_database(&root);
        let mut during = None;
        migrate_with(&root, false, &mut |p| {
            if p == "copied" {
                during = Some(plan(&root));
            }
            Ok(())
        })
        .unwrap();
        let e = during
            .unwrap()
            .expect_err("the check waits for the migration");
        assert!(matches!(e, Error::Locked { .. }), "{e:?}");
        assert_eq!(count(&root), 1);
        assert_eq!(
            crate::vocab::delta::file_version(&root.join(&g).join("delta.vocab")).unwrap(),
            2
        );
    }

    #[test]
    fn check_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        legacy_database(&root);
        let work = work_dir(&root);
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("stale"), b"stale").unwrap();
        let p = plan(&root).unwrap();
        assert!(p.interrupted.is_some());
        assert_eq!(std::fs::read(work.join("stale")).unwrap(), b"stale");
        migrate(&root, false).unwrap();
        assert!(!work.exists());
        assert!(plan(&root).unwrap().interrupted.is_none());
    }

    #[test]
    fn a_live_store_blocks_migration() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        legacy_database(&root);
        let _open = Store::open(&root, StoreOptions::default()).unwrap();
        let e = migrate(&root, false).expect_err("a store has the database open");
        assert!(matches!(e, Error::Locked { .. }), "{e:?}");
        assert!(!work_dir(&root).exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        legacy_database(&root);
        let target = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(target.path(), root.join("linked")).unwrap();
        let e = migrate(&root, false).expect_err("symlinks are not copied");
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
        assert!(root.join("linked").is_symlink());
        assert!(!work_dir(&root).exists());
    }

    #[test]
    fn an_existing_kept_directory_is_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        legacy_database(&root);
        let old = old_dir(&root);
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("marker"), b"mine").unwrap();
        let e = migrate(&root, true).expect_err("the earlier copy is kept");
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
        assert!(old.join("marker").exists());
        assert!(!work_dir(&root).exists());
    }

    #[test]
    fn the_lock_file_of_one_database_is_not_the_work_directory_of_another() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let other = dir.path().join("a.lock");
        legacy_database(&a);
        drop(Store::open(&other, StoreOptions::default()).unwrap());
        migrate(&a, false).unwrap();
        assert!(pending(&other).unwrap().is_none());
        drop(Store::open(&other, StoreOptions::default()).unwrap());
        assert!(plan(&other).unwrap().interrupted.is_none());
    }

    #[test]
    fn a_root_created_next_to_a_checked_copy_keeps_both() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        legacy_database(&root);
        migrate_with(&root, false, &mut |p| {
            if p == "moved-aside" {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        // something created the root again after the first rename
        std::fs::create_dir(&root).unwrap();
        let e = migrate(&root, false).expect_err("both sides are kept");
        assert!(matches!(e, Error::Corrupt(_)), "{e:?}");
        assert!(work_dir(&root).is_dir());
        assert!(old_dir(&root).is_dir());
    }
}
