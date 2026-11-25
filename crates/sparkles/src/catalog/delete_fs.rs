//! Filesystem operations for managed deletes and their startup recovery. The catalog lock serializes
//! changes to the managed namespace. On Unix, directory descriptors also pin the
//! tree across inspection, traversal, rename and removal.

use sparkles_core::store::ProcessLock;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

pub(super) struct RecoveryDir {
    path: PathBuf,
    #[cfg(unix)]
    file: File,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Directory,
    File,
    Other,
}

pub(super) struct Entry {
    pub kind: Kind,
    pub bytes: u64,
}

#[cfg(all(unix, test))]
type RemovalHook = Box<dyn FnMut(&Path)>;

#[cfg(all(unix, test))]
thread_local! {
    pub(super) static DURING_REMOVAL: std::cell::RefCell<Option<RemovalHook>> =
        const { std::cell::RefCell::new(None) };
}

impl RecoveryDir {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            path: path.into(),
            #[cfg(unix)]
            file: File::open(path)?,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entry(&self, name: &OsStr) -> io::Result<Entry> {
        #[cfg(unix)]
        {
            let stat = fs::statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW)?;
            let kind = match FileType::from_raw_mode(stat.st_mode) {
                FileType::Directory => Kind::Directory,
                FileType::RegularFile => Kind::File,
                _ => Kind::Other,
            };
            Ok(Entry {
                kind,
                bytes: stat.st_size.try_into().unwrap_or(0),
            })
        }
        #[cfg(not(unix))]
        {
            let meta = std::fs::symlink_metadata(self.path.join(name))?;
            let kind = if meta.is_dir() {
                Kind::Directory
            } else if meta.is_file() {
                Kind::File
            } else {
                Kind::Other
            };
            Ok(Entry {
                kind,
                bytes: meta.len(),
            })
        }
    }

    pub fn child(&self, name: &OsStr) -> io::Result<Self> {
        #[cfg(not(unix))]
        if self.entry(name)?.kind != Kind::Directory {
            return Err(io::Error::new(
                ErrorKind::NotADirectory,
                "expected a real directory",
            ));
        }
        Ok(Self {
            path: self.path.join(name),
            #[cfg(unix)]
            file: fs::openat(
                &self.file,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?
            .into(),
        })
    }

    /// Read a regular file or open the process lock, creating it if a prior
    /// interrupted removal already unlinked it.
    pub fn open_file(&self, name: &OsStr, lock: bool) -> io::Result<File> {
        match self.entry(name) {
            Ok(entry) if entry.kind == Kind::File => {}
            Err(error) if lock && error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "expected a regular file",
                ));
            }
        }
        #[cfg(unix)]
        let file: File = {
            let flags = if lock {
                OFlags::RDWR | OFlags::CREATE
            } else {
                OFlags::RDONLY
            };
            fs::openat(
                &self.file,
                name,
                flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::WGRP | Mode::ROTH | Mode::WOTH,
            )?
            .into()
        };
        #[cfg(not(unix))]
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(lock)
            .create(lock)
            .open(self.path.join(name))?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "expected a regular file",
            ));
        }
        Ok(file)
    }

    pub fn verify_lock(&self, file: &File) -> crate::Result<()> {
        #[cfg(unix)]
        {
            self.verify_entry(OsStr::new("sparkles.lock"), file)?;
            Ok(())
        }
        #[cfg(not(unix))]
        sparkles_core::store::verify_database_lock(&self.path, file)
    }

    #[cfg(unix)]
    fn verify_entry(&self, name: &OsStr, file: &File) -> io::Result<()> {
        let opened = fs::fstat(file)?;
        let current = fs::statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if (opened.st_dev, opened.st_ino) != (current.st_dev, current.st_ino) {
            return Err(io::Error::other("entry changed during delete recovery"));
        }
        Ok(())
    }

    pub fn names(&self) -> io::Result<impl Iterator<Item = io::Result<OsString>>> {
        #[cfg(unix)]
        return Ok(
            fs::Dir::read_from(&self.file)?.filter_map(|entry| match entry {
                Ok(entry) => {
                    let name = entry.file_name().to_bytes();
                    (!matches!(name, b"." | b"..")).then(|| Ok(OsStr::from_bytes(name).into()))
                }
                Err(error) => Some(Err(error.into())),
            }),
        );
        #[cfg(not(unix))]
        Ok(std::fs::read_dir(&self.path)?.map(|entry| entry.map(|entry| entry.file_name())))
    }

    pub fn verify_child(&self, name: &OsStr, _child: &Self) -> io::Result<()> {
        #[cfg(unix)]
        return self.verify_entry(name, &_child.file);
        #[cfg(not(unix))]
        if self.entry(name)?.kind == Kind::Directory {
            Ok(())
        } else {
            Err(io::Error::new(
                ErrorKind::NotADirectory,
                "expected a real directory",
            ))
        }
    }

    pub fn rename_child(&self, child: &mut Self, new: &OsStr) -> io::Result<()> {
        let name = child.name();
        self.verify_child(name, child)?;

        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_vendor = "apple",
            target_os = "redox"
        ))]
        fs::renameat_with(
            &self.file,
            name,
            &self.file,
            new,
            fs::RenameFlags::NOREPLACE,
        )?;

        #[cfg(not(any(
            target_os = "linux",
            target_os = "android",
            target_vendor = "apple",
            target_os = "redox"
        )))]
        {
            match self.entry(new) {
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error),
                Ok(_) => return Err(ErrorKind::AlreadyExists.into()),
            }
            #[cfg(unix)]
            fs::renameat(&self.file, name, &self.file, new)?;
            #[cfg(not(unix))]
            std::fs::rename(child.path(), self.path.join(new))?;
        }
        child.path = self.path.join(new);
        Ok(())
    }

    fn name(&self) -> &OsStr {
        self.path.file_name().expect("child directory name")
    }

    pub fn remove_file(&self, name: &OsStr) -> io::Result<()> {
        #[cfg(unix)]
        return Ok(fs::unlinkat(&self.file, name, AtFlags::empty())?);
        #[cfg(not(unix))]
        std::fs::remove_file(self.path.join(name))
    }

    pub fn remove_child(&self, child: &Self, lock: ProcessLock) -> io::Result<()> {
        let name = child.name();
        let lock_name = OsStr::new("sparkles.lock");
        self.verify_child(name, child)?;
        child.remove_contents(Some(lock_name))?;
        self.verify_child(name, child)?;

        // Windows cannot remove the directory while the lock handle is open.
        #[cfg(not(unix))]
        drop(lock);
        match child.remove_file(lock_name) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            result => result?,
        }
        self.remove_empty_child(child)?;
        #[cfg(unix)]
        drop(lock);
        Ok(())
    }

    fn remove_empty_child(&self, child: &Self) -> io::Result<()> {
        #[cfg(unix)]
        return Ok(fs::unlinkat(&self.file, child.name(), AtFlags::REMOVEDIR)?);
        #[cfg(not(unix))]
        std::fs::remove_dir(child.path())
    }

    fn remove_contents(&self, last: Option<&OsStr>) -> io::Result<()> {
        let names = self.names()?;
        #[cfg(all(unix, test))]
        if last.is_some() {
            DURING_REMOVAL.with_borrow_mut(|hook| {
                if let Some(hook) = hook {
                    hook(self.path());
                }
            });
        }
        for name in names {
            let name = name?;
            if last == Some(&name) {
                continue;
            }
            let removed = match self.entry(&name) {
                Ok(entry) if entry.kind == Kind::Directory => {
                    let child = self.child(&name)?;
                    child.remove_contents(None)?;
                    self.verify_child(&name, &child)?;
                    self.remove_empty_child(&child)
                }
                Ok(_) => self.remove_file(&name),
                Err(error) => Err(error),
            };
            match removed {
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                result => result?,
            }
        }
        Ok(())
    }

    pub fn sync(&self) -> crate::Result<()> {
        #[cfg(unix)]
        self.file.sync_all()?;
        #[cfg(test)]
        super::record_dir_sync(&self.path);
        Ok(())
    }
}
