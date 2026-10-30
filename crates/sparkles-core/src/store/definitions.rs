//! Dataset-managed definitions that backups and clones capture with the data (spec F05,
//! "Recovery of managed definitions"): the kinds of [`crate::formats::DEFINITIONS`],
//! such as the views' `views/NAME/view.json`.
//!
//! A capture reads the definitions while it holds the store's writer lock, so the
//! definitions it records were current when the captured commit was the head. The
//! lock order is the store's writer lock, then the registry of [`DefinitionSource`]s,
//! then each source's own snapshot lock. A source's snapshot lock is a leaf: it is held
//! only to clone the current revision, and its owner changes it while holding nothing
//! that waits for the writer lock. No network transfer runs under any of these locks.
//!
//! A store whose definitions no live source manages, such as a stopped database opened
//! for an offline backup, reads them from its directory under the writer lock. Its
//! process lock keeps other writers out of the directory.

use super::Store;
use crate::error::{Error, Result};
use crate::formats::DefinitionKind;
use std::path::Path;
use std::sync::{Arc, Weak};

/// One captured definition: its kind, its path relative to the dataset root, the
/// version of the definition file and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionFile {
    pub kind: &'static str,
    pub path: String,
    pub format: u32,
    pub bytes: Arc<[u8]>,
}

/// Who keeps the current definitions of one kind in memory, such as a dataset's view
/// runtime. [`Store::register_definitions`] makes the store capture through it.
pub trait DefinitionSource: Send + Sync {
    /// The kind, an id of [`crate::formats::DEFINITIONS`].
    fn kind(&self) -> &'static str;

    /// The definitions current now, in a stable order. It is called with the store's
    /// writer lock held, so it must not wait for anything but a leaf lock. An error
    /// refuses the capture, since definitions are never silently left out.
    fn capture_definitions(&self) -> Result<Vec<DefinitionFile>>;
}

/// The registered sources of a store.
#[derive(Default)]
pub(super) struct Sources(parking_lot::Mutex<Vec<Weak<dyn DefinitionSource>>>);

impl Store {
    /// Capture the definitions of `source`'s kind through it from now on, instead of
    /// reading them from the store's directory. The store holds it weakly: once it is
    /// dropped, captures read the directory again.
    pub fn register_definitions(&self, source: Weak<dyn DefinitionSource>) {
        let mut v = self.definitions.0.lock();
        v.retain(|w| w.strong_count() > 0);
        v.push(source);
    }

    /// The definitions of every kind, as of now. Callers hold the writer lock.
    pub(super) fn capture_definitions(&self) -> Result<Vec<DefinitionFile>> {
        let live: Vec<Arc<dyn DefinitionSource>> = self
            .definitions
            .0
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        let mut out = Vec::new();
        for kind in crate::formats::DEFINITIONS {
            match live.iter().find(|s| s.kind() == kind.id) {
                Some(s) => out.extend(s.capture_definitions()?),
                None => {
                    if let Some(root) = &self.root {
                        out.extend(read_dir(root, kind)?);
                    }
                }
            }
        }
        Ok(out)
    }
}

/// The definitions of `kind` stored under `root`, by name, as they run: a paused
/// definition is captured without its pause. A definition that this build does not
/// read, or that does not match its kind's schema, refuses the whole capture.
pub fn read_dir(root: &Path, kind: &DefinitionKind) -> Result<Vec<DefinitionFile>> {
    let base = root.join(kind.dir);
    let entries = match std::fs::read_dir(&base) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut names = Vec::new();
    for e in entries {
        let e = e?;
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        if crate::stored::valid_name(&name) && e.file_type()?.is_dir() {
            names.push(name);
        }
    }
    names.sort();
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let path = kind.path(&name);
        let mut bytes = match std::fs::read(root.join(&path)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let mut format = check(kind, &path, &bytes)?;
        if format == kind.paused {
            bytes = rewrite(kind, &path, &bytes, None)?;
            format = kind.writes;
        }
        out.push(DefinitionFile {
            kind: kind.id,
            path,
            format,
            bytes: bytes.into(),
        });
    }
    Ok(out)
}

/// The version of definition file `bytes` at `path`, once it is checked: at most
/// [`MAX_DEFINITION_BYTES`](crate::formats::MAX_DEFINITION_BYTES), a version this
/// build reads, and the schema of its kind with options this release supports
/// (`Error::Corrupt`, `Error::Unsupported` or `Error::Invalid`, naming `path`). The
/// references a definition makes, such as a view's principal and stored query, are
/// resolved when it runs, not here.
pub fn check(kind: &DefinitionKind, path: &str, bytes: &[u8]) -> Result<u32> {
    let max = crate::formats::MAX_DEFINITION_BYTES;
    let too_large = || Error::Invalid(format!("{path} is larger than {max} bytes"));
    // a paused file is a little larger than the definition it holds
    if bytes.len() as u64 > 2 * max {
        return Err(too_large());
    }
    match kind.id {
        "view" => {
            let (def, paused) = crate::incr::views::parse(path, bytes)?;
            def.check()
                .map_err(|e| Error::Invalid(format!("{path}: {e}")))?;
            let running = match paused {
                Some(_) => crate::incr::views::to_bytes(&def, None)?.len(),
                None => bytes.len(),
            };
            if running as u64 > max {
                return Err(too_large());
            }
            Ok(if paused.is_some() {
                kind.paused
            } else {
                kind.writes
            })
        }
        other => Err(Error::Invalid(format!("unknown definition kind {other:?}"))),
    }
}

/// Check every definition of a capture before anything of it is published.
pub fn check_all(defs: &[DefinitionFile]) -> Result<()> {
    for d in defs {
        let kind = crate::formats::definition_kind(d.kind)
            .ok_or_else(|| Error::Invalid(format!("unknown definition kind {:?}", d.kind)))?;
        if kind.name_of(&d.path).is_none() {
            return Err(Error::Invalid(format!(
                "{:?} is not a definition path",
                d.path
            )));
        }
        let found = check(kind, &d.path, &d.bytes)?;
        if found != d.format {
            return Err(Error::Invalid(format!(
                "{} has format {found}, not {}",
                d.path, d.format
            )));
        }
    }
    Ok(())
}

/// The bytes of definition `bytes` at `path`, paused with `paused` or running.
fn rewrite(
    kind: &DefinitionKind,
    path: &str,
    bytes: &[u8],
    paused: Option<&Paused>,
) -> Result<Vec<u8>> {
    match kind.id {
        "view" => {
            let (def, _) = crate::incr::views::parse(path, bytes)?;
            crate::incr::views::to_bytes(&def, paused)
        }
        other => Err(Error::Invalid(format!("unknown definition kind {other:?}"))),
    }
}

/// Why a restored or cloned definition does not run, kept inside its paused file
/// (format [`DefinitionKind::paused`]). Releases that do not know pauses refuse that
/// version, so they never run it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Paused {
    /// `restored` or `cloned`
    pub reason: String,
    /// when the definition arrived (RFC 3339)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
}

impl Paused {
    pub fn now(reason: &str) -> Paused {
        Paused {
            reason: reason.to_string(),
            time: Some(crate::builder::now_rfc3339()),
        }
    }
}

/// Write `defs` under `root`, which is not published yet: paused with `reason`, or as
/// they are without one. Every directory is created here, and a path that already
/// exists or is not a plain directory is refused, so nothing is written through a
/// link. The files and every directory up to `root` are synced.
pub fn write_all(root: &Path, defs: &[DefinitionFile], reason: Option<&str>) -> Result<()> {
    let paused = reason.map(Paused::now);
    let mut dirs = std::collections::BTreeSet::new();
    for d in defs {
        let kind = crate::formats::definition_kind(d.kind)
            .ok_or_else(|| Error::Invalid(format!("unknown definition kind {:?}", d.kind)))?;
        if kind.name_of(&d.path).is_none() {
            return Err(Error::Invalid(format!(
                "{:?} is not a definition path",
                d.path
            )));
        }
        let file = stage_file(root, &d.path)?;
        match &paused {
            Some(p) => write_new(&file, &rewrite(kind, &d.path, &d.bytes, Some(p))?)?,
            None => write_new(&file, &d.bytes)?,
        }
        let mut dir = file.parent();
        while let Some(x) = dir.filter(|x| x.starts_with(root) && *x != root) {
            dirs.insert(x.to_path_buf());
            dir = x.parent();
        }
    }
    // deepest first, so that each entry is durable before its parent's
    for d in dirs.iter().rev() {
        super::sync_dir(d)?;
    }
    if !defs.is_empty() {
        super::sync_dir(root)?;
    }
    Ok(())
}

/// Pause the definition at `path` under `root`, which is not published yet: rewrite it
/// atomically as its paused version with `reason`.
pub fn pause(root: &Path, path: &str, reason: &str) -> Result<()> {
    let kind = crate::formats::DEFINITIONS
        .iter()
        .find(|k| k.name_of(path).is_some())
        .ok_or_else(|| Error::Invalid(format!("{path:?} is not a definition path")))?;
    let file = root.join(path);
    let bytes = std::fs::read(&file)?;
    let paused = rewrite(kind, path, &bytes, Some(&Paused::now(reason)))?;
    super::write_atomic(&file, &paused)
}

/// Create the directories of relative path `rel` under `root`, refusing any that exists
/// as a link or a file, and return the file's full path. `rel` has been checked
/// against the definition grammar, so it has no `.`, `..` or empty segments.
pub fn stage_file(root: &Path, rel: &str) -> Result<std::path::PathBuf> {
    let mut parts: Vec<&str> = rel.split('/').collect();
    let file = parts.pop().unwrap_or_default();
    if file.is_empty()
        || parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return Err(Error::Invalid(format!("{rel:?} is not a relative path")));
    }
    let mut dir = root.to_path_buf();
    for p in parts {
        dir.push(p);
        match std::fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let m = std::fs::symlink_metadata(&dir)?;
                if !m.file_type().is_dir() {
                    return Err(Error::Invalid(format!(
                        "{} is not a plain directory",
                        dir.display()
                    )));
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(dir.join(file))
}

/// Write a new file (never one that exists, and never through a link) and sync it.
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(name: &str, q: &str) -> DefinitionFile {
        let bytes =
            crate::incr::views::to_bytes(&crate::incr::views::ViewDef::table(q), None).unwrap();
        DefinitionFile {
            kind: "view",
            path: format!("views/{name}/view.json"),
            format: 1,
            bytes: bytes.into(),
        }
    }

    #[test]
    fn staged_definitions_read_back_paused() {
        let tmp = tempfile::tempdir().unwrap();
        let defs = vec![view("a", "SELECT * { ?s ?p ?o }"), view("b", "ASK {}")];
        write_all(tmp.path(), &defs, Some("restored")).unwrap();
        let kind = crate::formats::definition_kind("view").unwrap();
        // a capture holds them as they run, without the pause
        assert_eq!(read_dir(tmp.path(), kind).unwrap(), defs);
        let all = crate::incr::views::read_all(tmp.path()).unwrap();
        assert!(
            all.iter()
                .all(|(_, _, p)| p.as_ref().unwrap().reason == "restored")
        );
        let raw = std::fs::read(tmp.path().join("views/a/view.json")).unwrap();
        assert_eq!(check(kind, "views/a/view.json", &raw).unwrap(), kind.paused);
        // staging the same definition again never overwrites it
        assert!(write_all(tmp.path(), &defs[..1], Some("restored")).is_err());
        // a copy without a reason holds the bytes as captured
        let copy = tmp.path().join("copy");
        std::fs::create_dir(&copy).unwrap();
        write_all(&copy, &defs, None).unwrap();
        assert_eq!(
            std::fs::read(copy.join("views/b/view.json")).unwrap(),
            &*defs[1].bytes
        );
        // a restore pauses a downloaded definition in place
        pause(&copy, "views/b/view.json", "restored").unwrap();
        let raw = std::fs::read(copy.join("views/b/view.json")).unwrap();
        assert_eq!(check(kind, "views/b/view.json", &raw).unwrap(), kind.paused);
    }

    #[cfg(unix)]
    #[test]
    fn staging_refuses_links_and_bad_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("views")).unwrap();
        let e = write_all(&root, &[view("a", "ASK {}")], Some("restored")).unwrap_err();
        assert!(e.to_string().contains("not a plain directory"), "{e}");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        let mut bad = view("a", "ASK {}");
        bad.path = "views/../../x/view.json".into();
        assert!(write_all(tmp.path(), &[bad], Some("restored")).is_err());
        assert!(stage_file(tmp.path(), "a/../b").is_err());
    }

    #[test]
    fn a_newer_definition_refuses_the_read() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("views/a");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("view.json"), br#"{"format": 9, "query": "ASK {}"}"#).unwrap();
        let kind = crate::formats::definition_kind("view").unwrap();
        let e = read_dir(tmp.path(), kind).unwrap_err();
        assert!(matches!(e, Error::Unsupported(_)), "{e}");
        assert!(e.to_string().contains("views/a/view.json"), "{e}");
        std::fs::write(d.join("view.json"), b"[]").unwrap();
        assert!(matches!(read_dir(tmp.path(), kind), Err(Error::Corrupt(_))));
    }
}
