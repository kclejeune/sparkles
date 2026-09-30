//! A Tantivy directory that defers fsync to checkpoints.
//!
//! Tantivy syncs every new segment file, `meta.json` and the directory on each commit:
//! several fsyncs per write, which roughly tripled the latency of small updates. The
//! write-ahead log already makes every commit durable, and the index can be caught up from
//! it, so this directory writes without syncing and makes the index durable at
//! [`LazySyncDir::checkpoint`].
//!
//! Crash safety rests on one invariant: `meta.json` never refers to unsynced data unless
//! the marker file exists. Before the first unsynced write that could be referenced (a
//! finished file or an atomic write) the marker is created and synced. A checkpoint syncs
//! every file and the directory, and removes the marker only if no such write happened
//! meanwhile. An index found with its marker is verified before it is trusted.

use parking_lot::Mutex;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    AntiCallToken, Directory, DirectoryLock, FileHandle, Lock, MmapDirectory, TerminatingWrite,
    WatchCallback, WatchHandle, WritePtr,
};

struct State {
    /// writes that could be referenced by `meta.json`, ever
    writes: u64,
    /// the marker file exists
    marked: bool,
}

struct Shared {
    root: PathBuf,
    marker: PathBuf,
    state: Mutex<State>,
}

#[derive(Clone)]
pub(crate) struct LazySyncDir {
    inner: MmapDirectory,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for LazySyncDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LazySyncDir({:?})", self.shared.root)
    }
}

fn sync_dir(p: &Path) -> io::Result<()> {
    File::open(p)?.sync_all()
}

impl LazySyncDir {
    /// The directory `root` (which must exist), with `marker` outside it.
    pub(crate) fn open(root: &Path, marker: PathBuf) -> io::Result<LazySyncDir> {
        let inner = MmapDirectory::open(root).map_err(io::Error::other)?;
        let marked = marker.exists();
        Ok(LazySyncDir {
            inner,
            shared: Arc::new(Shared {
                root: root.to_path_buf(),
                marker,
                state: Mutex::new(State { writes: 0, marked }),
            }),
        })
    }

    /// Whether unsynced writes may exist (the marker is present).
    pub(crate) fn is_marked(&self) -> bool {
        self.shared.state.lock().marked
    }

    /// Note a write that `meta.json` may come to refer to; mark the index first.
    fn wrote(&self) -> io::Result<()> {
        let mut s = self.shared.state.lock();
        s.writes += 1;
        if !s.marked {
            File::create(&self.shared.marker)?.sync_all()?;
            if let Some(parent) = self.shared.marker.parent() {
                sync_dir(parent)?;
            }
            s.marked = true;
        }
        Ok(())
    }

    /// Make everything written so far durable, and drop the marker unless a write raced
    /// with the checkpoint.
    pub(crate) fn checkpoint(&self) -> io::Result<()> {
        let before = {
            let s = self.shared.state.lock();
            if !s.marked {
                return Ok(());
            }
            s.writes
        };
        for entry in std::fs::read_dir(&self.shared.root)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                match File::open(entry.path()) {
                    Ok(f) => f.sync_data()?,
                    // deleted meanwhile (merge garbage collection)
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        sync_dir(&self.shared.root)?;
        let mut s = self.shared.state.lock();
        if s.writes == before {
            match std::fs::remove_file(&self.shared.marker) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            if let Some(parent) = self.shared.marker.parent() {
                sync_dir(parent)?;
            }
            s.marked = false;
        }
        Ok(())
    }
}

/// A file written without fsync; finishing it marks the directory.
struct LazyFile {
    file: File,
    dir: LazySyncDir,
}

impl Write for LazyFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TerminatingWrite for LazyFile {
    fn terminate_ref(&mut self, _: AntiCallToken) -> io::Result<()> {
        self.file.flush()?;
        self.dir.wrote()
    }
}

impl Directory for LazySyncDir {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        self.inner.get_file_handle(path)
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        self.inner.delete(path)
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.inner.exists(path)
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.shared.root.join(path))
            .map_err(|e| {
                if e.kind() == io::ErrorKind::AlreadyExists {
                    OpenWriteError::FileAlreadyExists(path.to_path_buf())
                } else {
                    OpenWriteError::wrap_io_error(e, path.to_path_buf())
                }
            })?;
        Ok(BufWriter::new(Box::new(LazyFile {
            file,
            dir: self.clone(),
        })))
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        self.inner.atomic_read(path)
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.wrote()?;
        let target = self.shared.root.join(path);
        let mut tmp = tempfile::Builder::new().tempfile_in(&self.shared.root)?;
        tmp.write_all(data)?;
        tmp.flush()?;
        tmp.into_temp_path().persist(target)?;
        Ok(())
    }

    fn sync_directory(&self) -> io::Result<()> {
        // deferred to the next checkpoint
        Ok(())
    }

    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        self.inner.acquire_lock(lock)
    }

    fn watch(&self, watch_callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        self.inner.watch(watch_callback)
    }
}
