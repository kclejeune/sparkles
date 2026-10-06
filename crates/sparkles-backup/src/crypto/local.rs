//! Native-filesystem marker CAS simulation. Never used for injected object stores.
use super::{repository, slots};
use crate::{BackupError, Code, Ctl, LockObject, RepoType, Repository, Result, layout, lock};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(super) struct Guard {
    root: PathBuf,
    configured: PathBuf,
    directory: std::sync::Arc<File>,
    file: std::sync::Arc<File>,
}
impl Guard {
    pub async fn acquire(repo: &Repository, ctl: &Ctl) -> Result<Option<Self>> {
        if repo.config.kind != RepoType::Fs || repo.env.store.is_some() {
            return Ok(None);
        }
        let (root, directory) = repo
            .native_root
            .as_ref()
            .ok_or_else(|| slots::config("native filesystem root identity missing"))?;
        if std::fs::canonicalize(
            repo.config
                .path
                .as_ref()
                .ok_or_else(|| slots::config("filesystem repository path missing"))?,
        )? != *root
        {
            return Err(slots::config(
                "configured filesystem repository root changed",
            ));
        }
        let (root, directory) = (root.clone(), directory.clone());
        let configured = PathBuf::from(repo.config.path.as_ref().expect("validated native path"));
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(root.join("sparkles-key-management.lock"))?;
        if !file.metadata()?.is_file() {
            return Err(slots::config("key management lock is not a regular file"));
        }
        let start = Instant::now();
        loop {
            ctl.check()?;
            match file.try_lock() {
                Ok(()) => {
                    let guard = Self {
                        root,
                        configured,
                        directory,
                        file: std::sync::Arc::new(file),
                    };
                    guard.check_identity()?;
                    return Ok(Some(guard));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    if start.elapsed() >= repo.env.lock_wait {
                        return Err(BackupError::new(
                            Code::RepositoryLocked,
                            "filesystem key management is locked",
                        ));
                    }
                    lock::sleep_cancellable(Duration::from_millis(25), ctl).await?;
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
    }
    fn check_identity(&self) -> Result<()> {
        if std::fs::canonicalize(&self.configured)? != self.root {
            return Err(slots::config(
                "configured filesystem repository root changed",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            for (opened, path) in [
                (self.directory.as_ref(), self.root.clone()),
                (
                    self.file.as_ref(),
                    self.root.join("sparkles-key-management.lock"),
                ),
            ] {
                let original = opened.metadata()?;
                let current = std::fs::symlink_metadata(path)?;
                if original.dev() != current.dev() || original.ino() != current.ino() {
                    return Err(slots::config(
                        "filesystem key management root or lock changed",
                    ));
                }
            }
        }
        Ok(())
    }
    pub async fn verify_lease(
        &self,
        repo: &Repository,
        lease: &lock::LockGuard,
        expected: &LockObject,
    ) -> Result<()> {
        self.check_identity()?;
        let key = layout::lock_key(
            lease
                .id()
                .ok_or_else(|| slots::config("exclusive key lease is missing"))?,
        );
        let objects = lock::list_locks(repo.store.as_ref()).await?;
        let own = objects
            .iter()
            .find(|object| object.location == key)
            .ok_or_else(|| slots::config("exclusive key lease was lost"))?;
        let now = chrono::Utc::now();
        if lock::is_stale(own.last_modified, now)
            || objects
                .iter()
                .any(|object| object.location != key && !lock::is_stale(object.last_modified, now))
        {
            return Err(slots::config(
                "exclusive key lease expired or conflicts with another lease",
            ));
        }
        let bytes =
            repository::read_bounded(&repo.store, &key, slots::MAX_SLOT_BYTES as u64).await?;
        let actual: LockObject = serde_json::from_slice(&bytes)
            .map_err(|_| slots::config("invalid exclusive key lease"))?;
        if &actual != expected {
            return Err(slots::config("exclusive key lease ownership changed"));
        }
        Ok(())
    }
    pub async fn prepare(&self, next: &layout::Marker, ctl: &Ctl) -> Result<Prepared> {
        let guard = self.clone();
        let bytes = next.to_bytes();
        wait(
            tokio::task::spawn_blocking(move || guard.prepare_blocking(&bytes)),
            ctl,
        )
        .await
    }
    fn prepare_blocking(self, bytes: &[u8]) -> Result<Prepared> {
        self.check_identity()?;
        let staging = self
            .root
            .join(format!(".sparkles-marker-{}.tmp", uuid::Uuid::new_v4()));
        let prepared = Prepared {
            guard: self,
            path: staging,
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&prepared.path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        #[cfg(test)]
        let pause = pauses().lock().unwrap().remove(&prepared.guard.root);
        #[cfg(test)]
        if let Some(pause) = pause {
            let _ = pause.entered.send(());
            pause
                .release
                .recv_timeout(Duration::from_secs(10))
                .map_err(|_| slots::config("native preparation test barrier timed out"))?;
        }
        Ok(prepared)
    }
}

pub(super) struct Prepared {
    guard: Guard,
    path: PathBuf,
}
impl Prepared {
    pub async fn publish(self, ctl: &Ctl, lease: std::sync::Arc<lock::LockGuard>) -> Result<()> {
        self.guard.check_identity()?;
        ctl.check()?;
        std::fs::rename(
            &self.path,
            self.guard.root.join(layout::marker_key().as_ref()),
        )?;
        let result: Result<()> = tokio::task::spawn_blocking(move || {
            let _lease = lease;
            sync_directory(&self.guard.root, &self.guard.directory)?;
            // Retain the entire Prepared capability until sync completes.
            drop(self);
            Ok(())
        })
        .await
        .map_err(|_| BackupError::new(Code::Internal, "native directory sync worker failed"))?;
        // After rename, keep the object lease and heartbeat until sync finishes.
        // Report cancellation afterwards, retaining the recovery intent.
        result?;
        ctl.check()
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
pub(crate) fn sync_directory(root: &std::path::Path, directory: &File) -> Result<()> {
    #[cfg(not(test))]
    let _ = root;
    #[cfg(test)]
    let pause = sync_pauses().lock().unwrap().remove(root);
    #[cfg(test)]
    if let Some(pause) = pause {
        let _ = pause.entered.send(());
        pause
            .release
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| slots::config("native sync test barrier timed out"))?;
        if pause.fail {
            return Err(slots::config("native directory sync test failure"));
        }
    }
    directory.sync_all()?;
    Ok(())
}
async fn wait<T>(mut task: tokio::task::JoinHandle<Result<T>>, ctl: &Ctl) -> Result<T> {
    loop {
        ctl.check()?;
        tokio::select! {
            result=&mut task => return result.map_err(|_| BackupError::new(Code::Internal,"native marker worker failed"))?,
            _=tokio::time::sleep(Duration::from_millis(25)) => {},
        }
    }
}

#[cfg(test)]
struct Pause {
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
    fail: bool,
}
#[cfg(test)]
fn pauses() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, Pause>> {
    static PAUSES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, Pause>>,
    > = std::sync::OnceLock::new();
    PAUSES.get_or_init(Default::default)
}
#[cfg(test)]
pub(super) fn pause(
    root: &std::path::Path,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, continuing) = std::sync::mpsc::channel();
    pauses().lock().unwrap().insert(
        root.into(),
        Pause {
            entered,
            release: continuing,
            fail: false,
        },
    );
    (waiting, release)
}
#[cfg(test)]
fn sync_pauses() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, Pause>> {
    static PAUSES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, Pause>>,
    > = std::sync::OnceLock::new();
    PAUSES.get_or_init(Default::default)
}
#[cfg(test)]
pub(crate) fn pause_sync(
    root: &std::path::Path,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    pause_sync_inner(root, false)
}
#[cfg(test)]
pub(crate) fn pause_sync_failure(
    root: &std::path::Path,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    pause_sync_inner(root, true)
}
#[cfg(test)]
fn pause_sync_inner(
    root: &std::path::Path,
    fail: bool,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, continuing) = std::sync::mpsc::channel();
    sync_pauses().lock().unwrap().insert(
        root.into(),
        Pause {
            entered,
            release: continuing,
            fail,
        },
    );
    (waiting, release)
}
