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

/// The definitions of `kind` stored under `root`, by name. A definition file of a
/// version this build does not read refuses the whole capture.
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
        let bytes = match std::fs::read(root.join(&path)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let format = check(kind, &path, &bytes)?;
        out.push(DefinitionFile {
            kind: kind.id,
            path,
            format,
            bytes: bytes.into(),
        });
    }
    Ok(out)
}

/// The version of definition file `bytes` at `path`: a JSON object whose version this
/// build reads (`Error::Corrupt` or `Error::Unsupported`, naming `path`, otherwise).
pub fn check(kind: &DefinitionKind, path: &str, bytes: &[u8]) -> Result<u32> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| Error::Corrupt(format!("{path}: {e}")))?;
    if !v.is_object() {
        return Err(Error::Corrupt(format!("{path}: not a JSON object")));
    }
    crate::formats::check_settings(kind.file, &v).map_err(|e| match e {
        Error::Unsupported(m) => Error::Unsupported(format!("{path}: {m}")),
        Error::Corrupt(m) => Error::Corrupt(format!("{path}: {m}")),
        e => e,
    })
}

/// The marker of a restored or cloned definition that waits for its checks
/// (`DIR/NAME/paused.json`).
pub const PAUSED_FILE: &str = "paused.json";

/// `paused.json`: why a definition restored or cloned into this dataset does not run.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Paused {
    /// `restored` or `cloned`
    pub reason: String,
    /// when the definition arrived (RFC 3339)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
}

/// Write `defs` under `root`, which is not published yet, each beside a `paused.json`
/// with `reason`. Every directory is created here, and a path that already exists or
/// is not a plain directory is refused, so nothing is written through a link.
pub fn stage_paused(root: &Path, defs: &[DefinitionFile], reason: &str) -> Result<()> {
    let paused = paused_bytes(reason)?;
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
        write_new(&file, &d.bytes)?;
        let marker = file.with_file_name(PAUSED_FILE);
        write_new(&marker, &paused)?;
        super::sync_dir(file.parent().expect("a definition has a directory"))?;
    }
    if !defs.is_empty() {
        for kind in crate::formats::DEFINITIONS {
            let dir = root.join(kind.dir);
            if dir.is_dir() {
                super::sync_dir(&dir)?;
            }
        }
    }
    Ok(())
}

fn paused_bytes(reason: &str) -> Result<Vec<u8>> {
    crate::formats::settings_to_vec(
        PAUSED_FILE,
        &Paused {
            reason: reason.to_string(),
            time: Some(crate::builder::now_rfc3339()),
        },
    )
}

/// Pause the definition at `path` under `root`, which is not published yet: write its
/// `paused.json` with `reason`, which must not exist.
pub fn pause(root: &Path, path: &str, reason: &str) -> Result<()> {
    let marker = root.join(path).with_file_name(PAUSED_FILE);
    write_new(&marker, &paused_bytes(reason)?)?;
    super::sync_dir(marker.parent().expect("a definition has a directory"))
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

/// The paused marker of the definition at `path` (`DIR/NAME/FILE`) under `root`, if
/// one is there. A marker that cannot be read, such as one of a newer version, still
/// pauses the definition, with the reason `paused`.
pub fn read_paused(root: &Path, path: &str) -> Option<Paused> {
    let marker = root.join(path).with_file_name(PAUSED_FILE);
    let bytes = match std::fs::read(marker) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => Vec::new(),
    };
    Some(
        crate::formats::settings_from_slice(PAUSED_FILE, &bytes).unwrap_or(Paused {
            reason: "paused".into(),
            time: None,
        }),
    )
}

/// Remove the paused marker of the definition at `path` under `root`.
pub fn clear_paused(root: &Path, path: &str) -> Result<()> {
    match std::fs::remove_file(root.join(path).with_file_name(PAUSED_FILE)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(name: &str, q: &str) -> DefinitionFile {
        let bytes =
            crate::formats::settings_to_vec("view.json", &crate::incr::views::ViewDef::table(q))
                .unwrap();
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
        stage_paused(tmp.path(), &defs, "restored").unwrap();
        let kind = crate::formats::definition_kind("view").unwrap();
        assert_eq!(read_dir(tmp.path(), kind).unwrap(), defs);
        let p = read_paused(tmp.path(), "views/a/view.json").unwrap();
        assert_eq!(p.reason, "restored");
        // an unreadable marker still pauses
        let marker = tmp.path().join("views/b/paused.json");
        std::fs::write(&marker, br#"{"format": 7, "reason": "later"}"#).unwrap();
        assert_eq!(
            read_paused(tmp.path(), "views/b/view.json").unwrap().reason,
            "paused"
        );
        clear_paused(tmp.path(), "views/a/view.json").unwrap();
        assert!(read_paused(tmp.path(), "views/a/view.json").is_none());
        // staging the same definition again never overwrites it
        assert!(stage_paused(tmp.path(), &defs[..1], "restored").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn staging_refuses_links_and_bad_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("views")).unwrap();
        let e = stage_paused(&root, &[view("a", "ASK {}")], "restored").unwrap_err();
        assert!(e.to_string().contains("not a plain directory"), "{e}");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        let mut bad = view("a", "ASK {}");
        bad.path = "views/../../x/view.json".into();
        assert!(stage_paused(tmp.path(), &[bad], "restored").is_err());
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
