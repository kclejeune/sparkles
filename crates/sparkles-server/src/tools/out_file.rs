//! Output files of the command-line tools, written through a temporary file in the
//! destination's directory that is renamed over the destination only when the command
//! succeeds.
//!
//! A failed or interrupted command leaves an existing destination as it was. An output
//! that is also one of the command's inputs, by the same path, a symbolic link or a hard
//! link, is refused before anything is written.

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::path::{Path, PathBuf};

/// An output file being written. [`OutFile::commit`] puts it in place, and dropping it
/// without a commit removes the temporary file.
pub struct OutFile {
    tmp: tempfile::NamedTempFile,
    /// The file that is replaced: the destination with its symbolic links resolved, so
    /// that a link keeps pointing at the new contents.
    target: PathBuf,
    /// The destination as the user named it, for messages.
    shown: PathBuf,
}

impl OutFile {
    /// Start writing `path`. Refuses a destination that is the same file as one of
    /// `inputs` (paths that do not exist are skipped).
    pub fn create<'a>(path: &Path, inputs: impl IntoIterator<Item = &'a Path>) -> Result<OutFile> {
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.is_dir() {
                bail!("{} is a directory", path.display());
            }
            for input in inputs {
                if let Ok(m) = std::fs::metadata(input)
                    && same_file(&meta, &m, path, input)
                {
                    bail!(
                        "the output {} is the input {}: write to another file and then \
                         rename it",
                        path.display(),
                        input.display()
                    );
                }
            }
        }
        let target = resolve(path);
        let dir = match target.parent() {
            Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let mut b = tempfile::Builder::new();
        b.prefix(".sparkles-out-").suffix(".tmp");
        // a new file gets the mode `File::create` gives it, an existing one keeps its own
        let existing = std::fs::metadata(&target).ok().map(|m| m.permissions());
        #[cfg(unix)]
        if existing.is_none() {
            use std::os::unix::fs::PermissionsExt;
            b.permissions(std::fs::Permissions::from_mode(0o666));
        }
        let tmp = b
            .tempfile_in(&dir)
            .with_context(|| format!("creating a file in {}", dir.display()))?;
        if let Some(p) = existing {
            tmp.as_file()
                .set_permissions(p)
                .with_context(|| format!("setting the mode of {}", tmp.path().display()))?;
        }
        Ok(OutFile {
            tmp,
            target,
            shown: path.to_path_buf(),
        })
    }

    /// A handle that writes the temporary file.
    pub fn file(&self) -> Result<File> {
        self.tmp
            .reopen()
            .with_context(|| format!("opening {}", self.tmp.path().display()))
    }

    /// The destination as it was named.
    pub fn path(&self) -> &Path {
        &self.shown
    }

    /// Replace the destination with what was written, durably.
    pub fn commit(self) -> Result<()> {
        let OutFile { tmp, target, shown } = self;
        tmp.as_file()
            .sync_all()
            .with_context(|| format!("writing {}", shown.display()))?;
        tmp.persist(&target)
            .map_err(|e| e.error)
            .with_context(|| format!("writing {}", shown.display()))?;
        #[cfg(unix)]
        if let Some(dir) = target.parent().filter(|d| !d.as_os_str().is_empty()) {
            // the rename itself
            File::open(dir).and_then(|d| d.sync_all()).ok();
        }
        Ok(())
    }
}

/// The file a write to `path` should replace: an existing path with its links resolved,
/// else the path itself.
fn resolve(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata, _: &Path, _: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(_: &std::fs::Metadata, _: &std::fs::Metadata, a: &Path, b: &Path) -> bool {
    matches!(
        (std::fs::canonicalize(a), std::fs::canonicalize(b)),
        (Ok(x), Ok(y)) if x == y
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn an_uncommitted_output_leaves_the_destination_alone() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("out.nt");
        std::fs::write(&p, "old\n").unwrap();
        let o = OutFile::create(&p, []).unwrap();
        o.file().unwrap().write_all(b"new\n").unwrap();
        drop(o);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "old\n");
        // no temporary file is left behind
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_commit_replaces_the_destination_and_keeps_its_mode() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("out.nt");
        std::fs::write(&p, "old\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        let o = OutFile::create(&p, []).unwrap();
        o.file().unwrap().write_all(b"new\n").unwrap();
        o.commit().unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o640);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_the_destination_follows_the_new_contents() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real.nt");
        let link = d.path().join("link.nt");
        std::fs::write(&real, "old\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let o = OutFile::create(&link, []).unwrap();
        o.file().unwrap().write_all(b"new\n").unwrap();
        o.commit().unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new\n");
    }

    #[test]
    fn an_output_that_is_an_input_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("in.nt");
        std::fs::write(&p, "data\n").unwrap();
        let e = OutFile::create(&p, [p.as_path()]).err().unwrap();
        assert!(e.to_string().contains("is the input"), "{e}");
        #[cfg(unix)]
        {
            let link = d.path().join("link.nt");
            std::os::unix::fs::symlink(&p, &link).unwrap();
            assert!(OutFile::create(&link, [p.as_path()]).is_err());
            assert!(OutFile::create(&p, [link.as_path()]).is_err());
            let hard = d.path().join("hard.nt");
            std::fs::hard_link(&p, &hard).unwrap();
            assert!(OutFile::create(&hard, [p.as_path()]).is_err());
        }
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "data\n");
    }
}
