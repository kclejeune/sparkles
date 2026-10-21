use std::fs::File;
use std::ops::Deref;

/// The logical owner of an already acquired exclusive file lock.
///
/// Closing only our descriptor does not release a Unix flock while an unrelated
/// forked child retains the same open file description before exec. Unlock that
/// description explicitly when ownership ends, before File closes the descriptor.
/// Copies made with `try_clone` are descriptors, not additional logical owners.
pub(crate) struct ProcessLock(File);

impl ProcessLock {
    pub(crate) fn from_locked_file(file: File) -> Self {
        Self(file)
    }
}

impl Deref for ProcessLock {
    type Target = File;

    fn deref(&self) -> &File {
        &self.0
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
