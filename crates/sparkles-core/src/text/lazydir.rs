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
//!
//! Tantivy finishes the segment files before publishing metadata that references them.
//! Finishing a segment marks it here, and an atomic metadata write marks before its
//! temporary file is written. Finished segments present before the counter snapshot
//! are included in the scan; finishing another segment during it changes the counter.
//! A later metadata publication marks again before changing its file. An in-flight
//! atomic-write guard also keeps a checkpoint from declaring a metadata replacement
//! clean while that replacement is still being written or renamed.
//!
//! A checkpoint synchronizes only the files finished or replaced since they were last
//! synchronized, which the directory records as they finish. Tantivy never changes a
//! finished file, and it replaces metadata by renaming a new file over the old name, so a
//! file that one pass synchronized stays durable, and a replacement is recorded again. A
//! pass that fails puts its files back. A directory opened with its marker present may hold
//! unsynchronized files from an earlier process, so its first checkpoint synchronizes every
//! file, as every checkpoint did before. The parent directory is synchronized when a file
//! was created, renamed or removed since the last synchronization of it. A checkpoint that a
//! write raced therefore still makes progress, and the next one has only the files written
//! during it left to synchronize.
//!
//! The pathname lease serializes checkpoints with directory replacement. It does not
//! stop Tantivy publication. The checkpoint streams file handles rather than pinning
//! every segment at once, so its handle count does not grow with the index.

use parking_lot::{Mutex, MutexGuard};
use std::collections::HashSet;
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
    /// atomic replacements that have marked their writes but not finished publishing
    atomic_writes: usize,
    /// the marker file exists
    marked: bool,
    /// its unlink happened, but synchronizing the parent directory has not succeeded
    marker_sync_pending: bool,
    /// files finished or replaced since a checkpoint last synchronized them, by name
    unsynced: HashSet<PathBuf>,
    /// any file may be unsynchronized: the marker was present when the directory opened
    all_unsynced: bool,
    /// a file was created, renamed or removed since the directory was last synchronized
    dir_unsynced: bool,
}

/// What one checkpoint pass synchronizes, taken from [`State`] at its start and put back
/// when the pass fails.
struct Pending {
    files: Vec<PathBuf>,
    all: bool,
    dir: bool,
}

struct Shared {
    root: PathBuf,
    marker: PathBuf,
    state: Mutex<State>,
    checkpoint: Mutex<()>,
    #[cfg(test)]
    hooks: Mutex<std::collections::BTreeMap<TestPoint, TestHook>>,
    /// the names each checkpoint pass synchronized, in order, for tests
    #[cfg(test)]
    synced: Mutex<Vec<PathBuf>>,
}

#[cfg(test)]
type TestHook = Box<dyn FnOnce() -> io::Result<()> + Send>;

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TestPoint {
    AtomicMarked,
    AtomicBeforeRename,
    CheckpointFileSync,
    CheckpointBeforeMarkerRemoval,
    CheckpointParentSync,
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
                state: Mutex::new(State {
                    writes: 0,
                    atomic_writes: 0,
                    marked,
                    marker_sync_pending: false,
                    unsynced: HashSet::new(),
                    all_unsynced: marked,
                    dir_unsynced: marked,
                }),
                checkpoint: Mutex::new(()),
                #[cfg(test)]
                hooks: Mutex::new(Default::default()),
                #[cfg(test)]
                synced: Mutex::new(Vec::new()),
            }),
        })
    }

    /// Whether unsynced writes may exist (the marker is present).
    pub(crate) fn is_marked(&self) -> bool {
        self.shared.state.lock().marked
    }

    pub(crate) fn same_directory(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    /// Keep the pathname stable against installation of a replacement index. Callers
    /// release this lease before reacquiring the text live mutex.
    pub(crate) fn checkpoint_lease(&self) -> MutexGuard<'_, ()> {
        #[cfg(test)]
        if let Some(root) = self.shared.marker.parent() {
            crate::store::text_recovery::hook(root, "text-checkpoint-before-lease");
        }
        let lease = self.shared.checkpoint.lock();
        #[cfg(test)]
        if let Some(root) = self.shared.marker.parent() {
            crate::store::text_recovery::hook(root, "text-checkpoint-after-lease");
        }
        lease
    }

    #[cfg(test)]
    pub(crate) fn before_checkpoint_file_sync(&self, hook: impl FnOnce() + Send + 'static) {
        self.test_hook(TestPoint::CheckpointFileSync, move || {
            hook();
            Ok(())
        });
    }

    #[cfg(test)]
    pub(crate) fn test_hook(
        &self,
        point: TestPoint,
        hook: impl FnOnce() -> io::Result<()> + Send + 'static,
    ) {
        self.shared.hooks.lock().insert(point, Box::new(hook));
    }

    #[cfg(test)]
    fn run_hook(&self, point: TestPoint) -> io::Result<()> {
        let hook = self.shared.hooks.lock().remove(&point);
        match hook {
            Some(hook) => hook(),
            None => Ok(()),
        }
    }

    /// Note a finished file that `meta.json` may come to refer to; mark the index first.
    /// The name is recorded in the same critical section as the write count, so a
    /// checkpoint that misses the name also sees the count change.
    fn finished(&self, path: &Path) -> io::Result<()> {
        let mut s = self.shared.state.lock();
        self.mark_write(&mut s)?;
        s.unsynced.insert(path.to_path_buf());
        s.dir_unsynced = true;
        Ok(())
    }

    /// How many files the next checkpoint pass would synchronize, or `None` when it
    /// would synchronize every file of the directory.
    pub(crate) fn unsynced_files(&self) -> Option<usize> {
        let s = self.shared.state.lock();
        (!s.all_unsynced).then_some(s.unsynced.len())
    }

    /// The names synchronized by checkpoint passes since the last call, for tests.
    #[cfg(test)]
    pub(crate) fn take_synced(&self) -> Vec<PathBuf> {
        std::mem::take(&mut self.shared.synced.lock())
    }

    fn mark_write(&self, s: &mut State) -> io::Result<()> {
        s.writes += 1;
        if !s.marked {
            File::create(&self.shared.marker)?.sync_all()?;
            if let Some(parent) = self.shared.marker.parent() {
                sync_dir(parent)?;
            }
            s.marked = true;
            s.marker_sync_pending = false;
        }
        Ok(())
    }

    fn atomic_write_started(&self, path: &Path) -> io::Result<AtomicWrite<'_>> {
        let mut state = self.shared.state.lock();
        self.mark_write(&mut state)?;
        state.atomic_writes += 1;
        Ok(AtomicWrite(&self.shared, path.to_path_buf()))
    }

    /// Capture the write counter before releasing the text live mutex. The lease
    /// protects pathnames, not publication: atomic writes can proceed during syncing.
    pub(crate) fn prepare_checkpoint(&self) -> Checkpoint<'_> {
        let lease = self.checkpoint_lease();
        let state = self.shared.state.lock();
        let before = (state.marked || state.marker_sync_pending).then_some(CheckpointStart {
            writes: state.writes,
            active: state.atomic_writes,
            marked: state.marked,
        });
        Checkpoint {
            dir: self,
            _lease: lease,
            before,
        }
    }

    /// Make everything written so far durable, and drop the marker unless a write raced
    /// with the checkpoint.
    pub(crate) fn checkpoint(&self) -> io::Result<()> {
        self.prepare_checkpoint().run().map(|_| ())
    }

    /// Take what the next pass must synchronize. The caller holds the pathname lease,
    /// so no other pass takes names between this and the pass's end.
    fn take_pending(&self) -> Pending {
        let mut s = self.shared.state.lock();
        Pending {
            files: s.unsynced.drain().collect(),
            all: std::mem::take(&mut s.all_unsynced),
            dir: std::mem::take(&mut s.dir_unsynced),
        }
    }

    /// Put back what a failed pass took, so a retry synchronizes it.
    fn restore_pending(&self, pending: Pending) {
        let mut s = self.shared.state.lock();
        s.unsynced.extend(pending.files);
        s.all_unsynced |= pending.all;
        s.dir_unsynced |= pending.dir;
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        match File::open(self.shared.root.join(path)) {
            Ok(f) => {
                #[cfg(test)]
                {
                    self.run_hook(TestPoint::CheckpointFileSync)?;
                    if let Some(root) = self.shared.marker.parent() {
                        crate::store::text_recovery::hook(root, "text-checkpoint-file-sync");
                    }
                }
                f.sync_data()?;
                #[cfg(test)]
                self.shared.synced.lock().push(path.to_path_buf());
                Ok(())
            }
            // deleted meanwhile (merge garbage collection)
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn sync_files(&self, pending: &Pending) -> io::Result<()> {
        if pending.all {
            for entry in std::fs::read_dir(&self.shared.root)? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    self.sync_file(Path::new(&entry.file_name()))?;
                }
            }
        } else {
            for path in &pending.files {
                self.sync_file(path)?;
            }
        }
        if pending.all || pending.dir {
            sync_dir(&self.shared.root)?;
        }
        Ok(())
    }

    fn sync_marker_parent(&self) -> io::Result<()> {
        if let Some(parent) = self.shared.marker.parent() {
            #[cfg(test)]
            self.run_hook(TestPoint::CheckpointParentSync)?;
            sync_dir(parent)?;
        }
        Ok(())
    }
}

/// This guard covers temporary-file creation through the metadata rename. In
/// particular, a write that began before a checkpoint's counter snapshot cannot let
/// that checkpoint declare the replacement metadata durable before it is published.
/// Its target is recorded as unsynchronized only when the guard drops, after the rename,
/// so that a pass which synchronized the old file under that name cannot take the record
/// of the new one. A pass that ran meanwhile saw the guard and kept the marker.
struct AtomicWrite<'a>(&'a Shared, PathBuf);

impl Drop for AtomicWrite<'_> {
    fn drop(&mut self) {
        let mut s = self.0.state.lock();
        s.atomic_writes -= 1;
        s.unsynced.insert(std::mem::take(&mut self.1));
        s.dir_unsynced = true;
    }
}

pub(crate) struct Checkpoint<'a> {
    dir: &'a LazySyncDir,
    _lease: MutexGuard<'a, ()>,
    before: Option<CheckpointStart>,
}

#[derive(Clone, Copy)]
struct CheckpointStart {
    writes: u64,
    active: usize,
    marked: bool,
}

impl Checkpoint<'_> {
    /// Return whether the marker was safely cleared. A racing write keeps it, and a
    /// later tick retries. All paths release the pathname lease before returning.
    pub(crate) fn run(self) -> io::Result<bool> {
        let Some(before) = self.before else {
            let state = self.dir.shared.state.lock();
            return Ok(!state.marked && !state.marker_sync_pending);
        };
        if before.marked {
            let pending = self.dir.take_pending();
            if let Err(e) = self.dir.sync_files(&pending) {
                self.dir.restore_pending(pending);
                return Err(e);
            }
        }
        let mut s = self.dir.shared.state.lock();
        if before.active == 0 && s.atomic_writes == 0 && s.writes == before.writes {
            if before.marked {
                #[cfg(test)]
                self.dir
                    .run_hook(TestPoint::CheckpointBeforeMarkerRemoval)?;
                match std::fs::remove_file(&self.dir.shared.marker) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
                // A later writer must recreate and sync the marker even when the
                // directory fence fails. A quiescent retry still owes that fence.
                s.marked = false;
                s.marker_sync_pending = true;
            }
            if s.marker_sync_pending {
                self.dir.sync_marker_parent()?;
                s.marker_sync_pending = false;
            }
            return Ok(true);
        }
        Ok(false)
    }
}

/// A file written without fsync; finishing it marks the directory.
struct LazyFile {
    file: File,
    path: PathBuf,
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
        self.dir.finished(&self.path)
    }
}

impl Directory for LazySyncDir {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        self.inner.get_file_handle(path)
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        self.inner.delete(path)?;
        self.shared.state.lock().dir_unsynced = true;
        Ok(())
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
            path: path.to_path_buf(),
            dir: self.clone(),
        })))
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        self.inner.atomic_read(path)
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let _publication = self.atomic_write_started(path)?;
        #[cfg(test)]
        self.run_hook(TestPoint::AtomicMarked)?;
        let target = self.shared.root.join(path);
        let mut tmp = tempfile::Builder::new().tempfile_in(&self.shared.root)?;
        tmp.write_all(data)?;
        tmp.flush()?;
        #[cfg(test)]
        self.run_hook(TestPoint::AtomicBeforeRename)?;
        // `std::fs::rename` rather than `TempPath::persist`, which may rename by a raw
        // system call: the power-loss tests record renames through libc
        let tmp = tmp.into_temp_path().keep().map_err(|e| e.error)?;
        std::fs::rename(&tmp, target).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn directory() -> (tempfile::TempDir, LazySyncDir) {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("text")).unwrap();
        let dir = LazySyncDir::open(&root.path().join("text"), root.path().join("dirty")).unwrap();
        dir.atomic_write(Path::new("meta.json"), br#"{"segment":"old.seg"}"#)
            .unwrap();
        let mut file = dir.open_write(Path::new("old.seg")).unwrap();
        file.write_all(b"old segment contents").unwrap();
        file.terminate().unwrap();
        (root, dir)
    }

    fn pause(dir: &LazySyncDir, point: TestPoint) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        dir.test_hook(point, move || {
            let _ = entered_tx.send(());
            release_rx
                .recv_timeout(Duration::from_secs(10))
                .map_err(io::Error::other)
        });
        (entered_rx, release_tx)
    }

    #[test]
    fn an_atomic_write_already_in_flight_keeps_the_marker() {
        for point in [TestPoint::AtomicMarked, TestPoint::AtomicBeforeRename] {
            let (_root, dir) = directory();
            let mut file = dir.open_write(Path::new("new.seg")).unwrap();
            file.write_all(b"new segment contents").unwrap();
            file.terminate().unwrap();
            let (entered, release) = pause(&dir, point);
            let writer_dir = dir.clone();
            let writer = std::thread::spawn(move || {
                writer_dir.atomic_write(Path::new("meta.json"), br#"{"segment":"new.seg"}"#)
            });
            let reached = entered.recv_timeout(Duration::from_secs(5));
            let checkpoint = reached
                .as_ref()
                .ok()
                .map(|_| dir.prepare_checkpoint().run());
            let _ = release.send(());
            writer.join().unwrap().unwrap();
            reached.unwrap();
            assert!(!checkpoint.unwrap().unwrap());
            assert!(dir.is_marked());
            assert!(dir.shared.marker.exists());
            assert_eq!(
                dir.atomic_read(Path::new("meta.json")).unwrap(),
                br#"{"segment":"new.seg"}"#
            );
            assert_eq!(
                std::fs::read(dir.shared.root.join("new.seg")).unwrap(),
                b"new segment contents"
            );
            assert!(dir.prepare_checkpoint().run().unwrap());
            assert!(!dir.is_marked());
        }
    }

    #[test]
    fn a_write_during_file_sync_keeps_the_marker_until_retry() {
        let (_root, dir) = directory();
        let (entered, release) = pause(&dir, TestPoint::CheckpointFileSync);
        let checkpoint_dir = dir.clone();
        let checkpoint = std::thread::spawn(move || checkpoint_dir.prepare_checkpoint().run());
        let reached = entered.recv_timeout(Duration::from_secs(5));
        let write = reached.as_ref().ok().map(|_| {
            dir.atomic_write(
                Path::new("meta.json"),
                br#"{"segment":"old.seg","revision":2}"#,
            )
        });
        let _ = release.send(());
        let clean = checkpoint.join().unwrap().unwrap();
        reached.unwrap();
        write.unwrap().unwrap();
        assert!(!clean);
        assert!(dir.is_marked());
        assert!(dir.prepare_checkpoint().run().unwrap());
        assert_eq!(
            dir.atomic_read(Path::new("meta.json")).unwrap(),
            br#"{"segment":"old.seg","revision":2}"#
        );
    }

    fn names(paths: &[PathBuf]) -> std::collections::BTreeSet<String> {
        paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    fn finish(dir: &LazySyncDir, name: &str) {
        let mut file = dir.open_write(Path::new(name)).unwrap();
        file.write_all(name.as_bytes()).unwrap();
        file.terminate().unwrap();
    }

    /// A pass that a write raced keeps the marker but not the work it did: the next pass
    /// synchronizes only the file written during the first.
    #[test]
    fn a_raced_pass_keeps_its_progress() {
        let (_root, dir) = directory();
        let (entered, release) = pause(&dir, TestPoint::CheckpointFileSync);
        let checkpoint_dir = dir.clone();
        let checkpoint = std::thread::spawn(move || checkpoint_dir.prepare_checkpoint().run());
        let reached = entered.recv_timeout(Duration::from_secs(5));
        if reached.is_ok() {
            finish(&dir, "new.seg");
        }
        let _ = release.send(());
        let clean = checkpoint.join().unwrap().unwrap();
        reached.unwrap();
        assert!(!clean);
        assert!(dir.is_marked());
        assert_eq!(
            names(&dir.take_synced()),
            ["meta.json", "old.seg"].map(String::from).into()
        );
        assert!(dir.prepare_checkpoint().run().unwrap());
        assert!(!dir.is_marked());
        assert_eq!(names(&dir.take_synced()), ["new.seg".to_string()].into());
        // a clean directory has nothing left to synchronize
        assert!(dir.prepare_checkpoint().run().unwrap());
        assert!(dir.take_synced().is_empty());
    }

    /// Metadata replaced while a pass runs is synchronized again by the next pass, both
    /// when the replacement finishes during the pass and when it is still in flight. A
    /// replacement recorded at its start rather than after its rename would let the pass
    /// synchronize the old file and take the record of the new one.
    #[test]
    fn replaced_metadata_is_synchronized_again() {
        for in_flight in [false, true] {
            let (_root, dir) = directory();
            assert!(dir.prepare_checkpoint().run().unwrap());
            finish(&dir, "new.seg");
            dir.take_synced();
            let writer = if in_flight {
                let (entered, release) = pause(&dir, TestPoint::AtomicBeforeRename);
                let writer_dir = dir.clone();
                let writer = std::thread::spawn(move || {
                    writer_dir.atomic_write(Path::new("meta.json"), br#"{"segment":"new.seg"}"#)
                });
                entered.recv_timeout(Duration::from_secs(5)).unwrap();
                assert!(!dir.prepare_checkpoint().run().unwrap());
                let _ = release.send(());
                writer.join().unwrap().unwrap();
                None
            } else {
                let (entered, release) = pause(&dir, TestPoint::CheckpointFileSync);
                let checkpoint_dir = dir.clone();
                let checkpoint =
                    std::thread::spawn(move || checkpoint_dir.prepare_checkpoint().run());
                let reached = entered.recv_timeout(Duration::from_secs(5));
                let write = reached
                    .as_ref()
                    .ok()
                    .map(|_| dir.atomic_write(Path::new("meta.json"), br#"{"segment":"new.seg"}"#));
                let _ = release.send(());
                assert!(!checkpoint.join().unwrap().unwrap());
                reached.unwrap();
                Some(write.unwrap())
            };
            if let Some(write) = writer {
                write.unwrap();
            }
            assert!(dir.is_marked());
            dir.take_synced();
            assert!(dir.prepare_checkpoint().run().unwrap());
            assert!(
                names(&dir.take_synced()).contains("meta.json"),
                "in flight {in_flight}: the replacement was never synchronized"
            );
            assert!(!dir.is_marked());
        }
    }

    /// A pass that fails puts back what it took, so the retry synchronizes every file the
    /// failed pass had not finished with.
    #[test]
    fn a_failed_pass_puts_its_files_back() {
        let (_root, dir) = directory();
        dir.test_hook(TestPoint::CheckpointFileSync, || {
            Err(io::Error::other("sync probe"))
        });
        assert!(dir.checkpoint().is_err());
        assert!(dir.is_marked());
        assert!(dir.take_synced().is_empty());
        assert!(dir.prepare_checkpoint().run().unwrap());
        assert_eq!(
            names(&dir.take_synced()),
            ["meta.json", "old.seg"].map(String::from).into()
        );
    }

    /// A directory opened with its marker present may hold files that an earlier process
    /// never synchronized, so its first checkpoint synchronizes every file in it.
    #[test]
    fn a_directory_opened_with_its_marker_synchronizes_every_file() {
        let (root, dir) = directory();
        finish(&dir, "earlier.seg");
        drop(dir);
        let dir = LazySyncDir::open(&root.path().join("text"), root.path().join("dirty")).unwrap();
        assert!(dir.is_marked());
        assert!(dir.prepare_checkpoint().run().unwrap());
        assert_eq!(
            names(&dir.take_synced()),
            ["earlier.seg", "meta.json", "old.seg"]
                .map(String::from)
                .into()
        );
        assert!(!dir.is_marked());
        drop(dir);
        // a clean directory reopens with nothing to synchronize
        let dir = LazySyncDir::open(&root.path().join("text"), root.path().join("dirty")).unwrap();
        assert!(!dir.is_marked());
        finish(&dir, "later.seg");
        assert!(dir.prepare_checkpoint().run().unwrap());
        assert_eq!(names(&dir.take_synced()), ["later.seg".to_string()].into());
    }

    #[test]
    fn a_failed_atomic_rename_releases_its_publication_guard() {
        let (_root, dir) = directory();
        dir.test_hook(TestPoint::AtomicBeforeRename, || {
            Err(io::Error::other("rename probe"))
        });
        assert!(
            dir.atomic_write(Path::new("meta.json"), b"never published")
                .is_err()
        );
        assert_eq!(
            dir.atomic_read(Path::new("meta.json")).unwrap(),
            br#"{"segment":"old.seg"}"#
        );
        assert!(dir.is_marked());
        assert!(dir.prepare_checkpoint().run().unwrap());
        assert!(!dir.is_marked());
        assert!(
            !std::fs::read_dir(&dir.shared.root).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp"))
        );
    }

    #[test]
    fn checkpoint_errors_preserve_a_retryable_marker_state() {
        for point in [
            TestPoint::CheckpointFileSync,
            TestPoint::CheckpointBeforeMarkerRemoval,
            TestPoint::CheckpointParentSync,
        ] {
            let (_root, dir) = directory();
            dir.test_hook(point, || Err(io::Error::other("sync probe")));
            assert!(dir.checkpoint().is_err());
            assert_eq!(dir.is_marked(), dir.shared.marker.exists());
            // A parent-sync failure follows unlink: the next publication must recreate
            // the marker rather than trusting the old in-memory marked bit.
            dir.atomic_write(
                Path::new("meta.json"),
                br#"{"segment":"old.seg","revision":2}"#,
            )
            .unwrap();
            assert!(dir.is_marked());
            assert!(dir.shared.marker.exists());
            assert!(dir.prepare_checkpoint().run().unwrap());
            assert!(!dir.is_marked());
        }
    }

    #[test]
    fn a_retry_completes_the_parent_fence_after_failed_marker_unlink_sync() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (_root, dir) = directory();
        dir.test_hook(TestPoint::CheckpointParentSync, || {
            Err(io::Error::other("parent sync probe"))
        });
        assert!(dir.checkpoint().is_err());
        assert!(!dir.is_marked());
        assert!(!dir.shared.marker.exists());
        let synced = Arc::new(AtomicBool::new(false));
        let observed = synced.clone();
        dir.test_hook(TestPoint::CheckpointParentSync, move || {
            observed.store(true, Ordering::SeqCst);
            Ok(())
        });
        dir.checkpoint().unwrap();
        assert!(
            synced.load(Ordering::SeqCst),
            "retry skipped the unfinished directory fence"
        );
        assert!(dir.prepare_checkpoint().run().unwrap());
    }

    #[test]
    fn a_writer_at_marker_removal_marks_again_before_publishing() {
        let (_root, dir) = directory();
        let (entered, release) = pause(&dir, TestPoint::CheckpointBeforeMarkerRemoval);
        let checkpoint_dir = dir.clone();
        let checkpoint = std::thread::spawn(move || checkpoint_dir.prepare_checkpoint().run());
        let reached = entered.recv_timeout(Duration::from_secs(5));
        let (writing_tx, writing_rx) = mpsc::channel();
        let writer_dir = dir.clone();
        let writer = std::thread::spawn(move || {
            let _ = writing_tx.send(());
            writer_dir.atomic_write(
                Path::new("meta.json"),
                br#"{"segment":"old.seg","revision":2}"#,
            )
        });
        let writing = writing_rx.recv_timeout(Duration::from_secs(5));
        let _ = release.send(());
        checkpoint.join().unwrap().unwrap();
        writer.join().unwrap().unwrap();
        reached.unwrap();
        writing.unwrap();
        assert!(dir.is_marked());
        assert!(dir.shared.marker.exists());
        assert!(dir.prepare_checkpoint().run().unwrap());
    }

    #[test]
    fn concurrent_checkpoints_serialize_and_release_after_a_failure() {
        let (_root, dir) = directory();
        let (entered, release) = pause(&dir, TestPoint::CheckpointFileSync);
        let first_dir = dir.clone();
        let first = std::thread::spawn(move || first_dir.prepare_checkpoint().run());
        let reached = entered.recv_timeout(Duration::from_secs(5));
        let second_dir = dir.clone();
        let second = std::thread::spawn(move || second_dir.prepare_checkpoint().run());
        let _ = release.send(());
        let a = first.join().unwrap();
        let b = second.join().unwrap();
        reached.unwrap();
        assert!(a.unwrap());
        assert!(b.unwrap());
        dir.test_hook(TestPoint::CheckpointFileSync, || {
            Err(io::Error::other("failed pass"))
        });
        dir.atomic_write(
            Path::new("meta.json"),
            br#"{"segment":"old.seg","revision":2}"#,
        )
        .unwrap();
        assert!(dir.checkpoint().is_err());
        assert!(dir.prepare_checkpoint().run().unwrap());
    }
}
