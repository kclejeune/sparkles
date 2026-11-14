//! The one durable atomic-write helper and the file and directory syncs it is made of.
//! Every crate that replaces a file durably goes through [`write_atomic`] or
//! [`write_atomic_via`].

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

/// Write a file and flush its contents to stable storage.
pub fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Flush a directory's entries (created, renamed or removed files) to stable storage.
/// An empty path is the current directory. Directories cannot be opened for syncing on
/// Windows, where this is a no-op.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        File::open(dir)?.sync_all()?;
        #[cfg(test)]
        DIR_SYNCS.with(|s| {
            let names = std::fs::read_dir(dir)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            s.borrow_mut().push((dir.to_path_buf(), names));
        });
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// The directories this thread synced, each with the names it held at the time, for
    /// tests of what a sync made durable. [`take_dir_syncs`] drains it.
    pub static DIR_SYNCS: std::cell::RefCell<Vec<(std::path::PathBuf, Vec<String>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The directory syncs of this thread since the last call, oldest first.
#[cfg(test)]
pub fn take_dir_syncs() -> Vec<(std::path::PathBuf, Vec<String>)> {
    DIR_SYNCS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// Replace `path` durably through the temporary file `tmp`, which must be in the same
/// directory: write and sync `tmp`, rename it over `path`, then sync the directory so
/// that the rename itself survives a power loss. A failed write removes `tmp`.
pub fn write_atomic_via(path: &Path, tmp: &Path, bytes: &[u8]) -> io::Result<()> {
    let written = write_synced(tmp, bytes).and_then(|()| std::fs::rename(tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    written?;
    sync_dir(path.parent().unwrap_or(Path::new(".")))
}

/// [`write_atomic_via`] through `path` with the extension `tmp` in place of its own
/// (`CURRENT.tmp`, `dataset.tmp`).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_via(path, &path.with_extension("tmp"), bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["state.json"]);
    }

    #[test]
    fn a_failed_rename_removes_the_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        // a directory cannot be renamed over by a file
        let path = dir.path().join("taken");
        std::fs::create_dir_all(path.join("inner")).unwrap();
        let tmp = dir.path().join("taken.tmp");
        assert!(write_atomic_via(&path, &tmp, b"x").is_err());
        assert!(!tmp.exists());
    }
}
