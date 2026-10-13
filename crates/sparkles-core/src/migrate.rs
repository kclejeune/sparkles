//! Upgrades of a closed database's files to the versions this build writes, for
//! `sparkles migrate`.
//!
//! [`plan`] reads the version of every versioned file of a database (see
//! [`crate::formats`]) and lists the registered [`Step`]s that would upgrade the ones
//! not at the version this build writes. [`migrate`] runs them on a copy of the
//! database in a sibling directory, checks the copy with [`crate::check`], and swaps it
//! in by two renames. A run that stopped between the renames is finished by
//! [`recover`], which [`plan`] and [`migrate`] call first.
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
/// Nothing is changed, except that an interrupted [`migrate`] is finished first.
pub fn plan(root: &Path) -> Result<Plan> {
    recover(root)?;
    let written_by = crate::commit::read_written_by(root);
    let by = written_by.as_deref();
    let mut plan = Plan {
        root: root.to_path_buf(),
        dataset: None,
        files: Vec::new(),
        steps: Vec::new(),
        refused: Vec::new(),
    };
    match crate::commit::read_dataset_versions(root) {
        Ok(d) => plan.dataset = d,
        Err(e) => plan.refused.push(e.to_string()),
    }
    inspect_store(&mut plan, root, "", by)?;
    // branch stores keep their generations under branches/<id>/
    let branches = root.join("branches");
    if branches.is_dir() {
        let mut dirs: Vec<_> = std::fs::read_dir(&branches)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        dirs.sort();
        for d in dirs {
            inspect_store(&mut plan, &branches.join(&d), &format!("branches/{d}/"), by)?;
        }
    }
    for s in formats::SETTINGS
        .iter()
        .filter(|s| s.scope == formats::Scope::Dataset)
    {
        let Ok(b) = std::fs::read(root.join(s.name)) else {
            continue;
        };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&b) else {
            continue;
        };
        if let Err(e) = formats::check_settings(s.name, &v) {
            plan.refused.push(e.to_string());
        }
    }
    Ok(plan)
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

/// Finish or undo a [`migrate`] of `root` that stopped part way. A copy that was not
/// swapped in yet is removed. When the database's directory is missing because the run
/// stopped between its two renames, the checked copy takes its place.
pub fn recover(root: &Path) -> Result<()> {
    let work = work_dir(root);
    if !work.exists() {
        return Ok(());
    }
    if root.exists() {
        // the copy was never swapped in: the database is as it was
        std::fs::remove_dir_all(&work)?;
    } else {
        // the old directory was moved aside and the copy, already checked, was not yet
        // renamed into place
        std::fs::rename(&work, root)?;
        sync_parent(root)?;
    }
    Ok(())
}

fn sync_parent(root: &Path) -> Result<()> {
    crate::store::sync_dir(root.parent().unwrap_or(Path::new(".")))
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let name = e.file_name();
        // the process lock of the database stays behind
        if name == "sparkles.lock" {
            continue;
        }
        let t = e.file_type()?;
        if t.is_dir() {
            copy_dir(&e.path(), &to.join(&name))?;
        } else if t.is_file() {
            std::fs::copy(e.path(), to.join(&name))?;
            std::fs::File::open(to.join(&name))?.sync_all()?;
        }
    }
    crate::store::sync_dir(to)
}

/// Upgrade the closed database at `root`: run the steps of [`plan`] on a copy, check the
/// copy, and swap it in. With `keep_old`, the database as it was stays in a sibling
/// directory named `.pre-migrate-<version>-<name>`, otherwise it is removed. A database
/// outside the window is refused, as is one whose copy fails the check. The caller holds
/// the catalog's lock, so no server has the database open.
pub fn migrate(root: &Path, keep_old: bool) -> Result<Outcome> {
    migrate_with(root, keep_old, &mut |_| Ok(()))
}

/// [`migrate`] with a hook called at named points (`copied`, `checked`, `moved-aside`),
/// for interruption tests.
#[doc(hidden)]
pub fn migrate_with(
    root: &Path,
    keep_old: bool,
    hook: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<Outcome> {
    let plan = plan(root)?;
    if !plan.in_window() {
        return Err(Error::Unsupported(plan.refused.join("; ")));
    }
    let mut out = Outcome {
        root: root.to_path_buf(),
        steps: plan.steps.clone(),
        kept: None,
    };
    if plan.steps.is_empty() {
        return Ok(out);
    }
    // hold the database's own lock while working on it
    let _lock = crate::store::lock_database(root)?;
    let work = work_dir(root);
    copy_dir(root, &work)?;
    hook("copied")?;
    for p in &plan.steps {
        let s = step(p.kind, p.from).expect("planned from STEPS");
        (s.run)(&work, Path::new(&p.file)).map_err(|e| {
            let _ = std::fs::remove_dir_all(&work);
            e
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
    hook("checked")?;
    let old = old_dir(root);
    if old.exists() {
        std::fs::remove_dir_all(&old)?;
    }
    drop(_lock);
    std::fs::rename(root, &old)?;
    sync_parent(root)?;
    hook("moved-aside")?;
    std::fs::rename(&work, root)?;
    sync_parent(root)?;
    if keep_old {
        out.kept = Some(old);
    } else {
        std::fs::remove_dir_all(&old)?;
    }
    Ok(out)
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
            // the next run (or plan) recovers
            recover(&root).unwrap();
            assert!(root.exists(), "{at}");
            assert!(!work_dir(&root).exists(), "{at}");
            let v = crate::vocab::delta::file_version(&root.join(&g).join("delta.vocab")).unwrap();
            // before the swap the database is unchanged, after it the copy is in place
            assert_eq!(v, if at == "moved-aside" { 2 } else { 1 }, "{at}");
            assert_eq!(count(&root), 1, "{at}");
        }
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
}
