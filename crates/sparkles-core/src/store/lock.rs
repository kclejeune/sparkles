use std::fs::File;
use std::ops::Deref;

/// The logical owner of an already acquired exclusive file lock.
///
/// Closing only our descriptor does not release a Unix flock while an unrelated
/// forked child retains the same open file description before exec. Unlock that
/// description explicitly when ownership ends, before File closes the descriptor.
/// Copies made with `try_clone` are descriptors, not additional logical owners.
///
/// A forked child shares the open file description and so the lock itself. Only the
/// process that acquired the lock unlocks it, so a child that drops an inherited
/// guard, such as a Python worker started with `fork`, leaves the parent's lock held.
pub(crate) struct ProcessLock {
    file: File,
    owner: u32,
}

impl ProcessLock {
    pub(crate) fn from_locked_file(file: File) -> Self {
        Self {
            file,
            owner: std::process::id(),
        }
    }
}

impl Deref for ProcessLock {
    type Target = File;

    fn deref(&self) -> &File {
        &self.file
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        if std::process::id() == self.owner {
            let _ = self.file.unlock();
        }
    }
}
