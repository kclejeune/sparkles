//! One lazily started vocabulary-sync worker per store writer, not per snapshot.
use super::DeltaFile;
use crate::error::Result;
use parking_lot::Mutex;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

struct Job {
    file: Arc<Mutex<DeltaFile>>,
    /// through the write-through handle when it can
    direct: bool,
    done: mpsc::Sender<Result<()>>,
}

struct Worker {
    send: Option<mpsc::SyncSender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Closing the queue lets all admitted work finish before shutdown.
        self.send.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
pub(crate) struct LazySync {
    attempted: bool,
    worker: Option<Worker>,
    #[cfg(test)]
    pub(crate) fail_spawn: bool,
}

pub(crate) struct PendingSync(mpsc::Receiver<Result<()>>);
impl PendingSync {
    pub(crate) fn wait(self) -> Result<()> {
        self.0
            .recv()
            .map_err(|_| std::io::Error::other("the delta vocabulary sync worker stopped"))?
    }
}

impl LazySync {
    pub(super) fn start(
        &mut self,
        file: Arc<Mutex<DeltaFile>>,
        direct: bool,
    ) -> Option<PendingSync> {
        if !self.attempted {
            self.attempted = true;
            let (send, receive) = mpsc::sync_channel::<Job>(1);
            #[cfg(test)]
            let refused = self.fail_spawn;
            #[cfg(not(test))]
            let refused = false;
            if !refused
                && let Ok(thread) =
                    std::thread::Builder::new()
                        .name("vocab-sync".into())
                        .spawn(move || {
                            while let Ok(job) = receive.recv() {
                                let result = if job.direct {
                                    DeltaFile::sync_direct(&job.file)
                                } else {
                                    DeltaFile::sync(&job.file)
                                };
                                let _ = job.done.send(result);
                            }
                        })
            {
                self.worker = Some(Worker {
                    send: Some(send),
                    thread: Some(thread),
                });
            }
        }
        // Failed creation retains the safe sequential fallback for this store.
        let worker = self.worker.as_ref()?;
        let (done, receive) = mpsc::channel();
        // A dead worker closes the reply, which becomes an error rather than
        // silently retrying an admitted synchronization with an unknown outcome.
        let _ = worker
            .send
            .as_ref()
            .expect("live worker sender")
            .send(Job { file, direct, done });
        Some(PendingSync(receive))
    }

    #[cfg(test)]
    pub(crate) fn thread_id(&self) -> Option<std::thread::ThreadId> {
        self.worker
            .as_ref()
            .and_then(|w| w.thread.as_ref())
            .map(|t| t.thread().id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vocab::DeltaVocab;
    use std::time::Duration;

    fn pause(v: &DeltaVocab) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (hit, observed) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let resume = Mutex::new(resume);
        let once = AtomicBool::new(false);
        v.set_sync_hook(Arc::new(move || {
            if !once.swap(true, Ordering::Relaxed) {
                hit.send(()).unwrap();
                resume.lock().recv_timeout(Duration::from_secs(5)).unwrap();
            }
        }));
        (observed, release)
    }

    #[test]
    fn captured_prefix_allows_mark_append_and_keeps_suffix_dirty() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("vocab");
        let v = Arc::new(DeltaVocab::open(&path).unwrap());
        v.insert(b"<urn:first>").unwrap();
        let (observed, release) = pause(&v);
        let mut worker = LazySync::default();
        let pending = v.sync_on(&mut worker, false).unwrap();
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let (done, arrived) = mpsc::channel();
        let writer = {
            let v = v.clone();
            std::thread::spawn(move || {
                let mark = v.mark();
                v.insert(b"<urn:second>").unwrap();
                v.flush().unwrap();
                done.send(mark).unwrap();
            })
        };
        let progress = arrived.recv_timeout(Duration::from_secs(1));
        release.send(()).unwrap();
        pending.wait().unwrap();
        writer.join().unwrap();
        assert!(progress.is_ok(), "mark/append blocked behind fdatasync");
        assert!(v.needs_sync(), "old prefix cannot clean a newer append");
        v.sync_on(&mut worker, false).unwrap().wait().unwrap();
        assert!(!v.needs_sync());
        assert_eq!(DeltaVocab::open(&path).unwrap().len(), 2);
    }

    #[test]
    fn rollback_waits_for_a_captured_prefix() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("vocab");
        let v = Arc::new(DeltaVocab::open(&path).unwrap());
        v.insert(b"<urn:base>").unwrap();
        v.sync().unwrap();
        let mark = v.mark();
        v.insert(b"<urn:temporary>").unwrap();
        let (observed, release) = pause(&v);
        let mut worker = LazySync::default();
        let pending = v.sync_on(&mut worker, false).unwrap();
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let (done, arrived) = mpsc::channel();
        let rollback = {
            let v = v.clone();
            std::thread::spawn(move || {
                v.rollback(&mark).unwrap();
                done.send(()).unwrap();
            })
        };
        assert!(arrived.recv_timeout(Duration::from_millis(50)).is_err());
        release.send(()).unwrap();
        pending.wait().unwrap();
        arrived.recv_timeout(Duration::from_secs(2)).unwrap();
        rollback.join().unwrap();
        assert!(!v.needs_sync(), "the rollback zeroed and synced the chunk");
        v.insert(b"<urn:replacement>").unwrap();
        v.sync_on(&mut worker, false).unwrap().wait().unwrap();
        let reopened = DeltaVocab::open(&path).unwrap();
        assert_eq!(reopened.len(), 2);
        assert_eq!(reopened.get(1).unwrap(), b"<urn:replacement>");
    }

    #[test]
    fn reusable_sync_handles_exact_generation_and_survives_owner_release() {
        let root = tempfile::tempdir().unwrap();
        let mut worker = LazySync::default();
        assert!(worker.thread_id().is_none());
        let mut first = None;
        for generation in 0..3 {
            let path = root.path().join(format!("vocab-{generation}"));
            let v = DeltaVocab::open(&path).unwrap();
            v.insert(format!("<urn:g{generation}>").as_bytes()).unwrap();
            v.flush().unwrap();
            let pending = v.sync_on(&mut worker, false).unwrap();
            let id = worker.thread_id().unwrap();
            assert_eq!(*first.get_or_insert(id), id);
            drop(v);
            pending.wait().unwrap();
            assert_eq!(DeltaVocab::open(&path).unwrap().len(), 1);
            assert_eq!(worker.thread_id(), Some(id));
        }
    }

    #[test]
    fn sync_failure_retains_dirty_state_and_reports_error() {
        let root = tempfile::tempdir().unwrap();
        let v = DeltaVocab::open(&root.path().join("vocab")).unwrap();
        let mut worker = LazySync::default();
        v.insert(b"<urn:failed>").unwrap();
        v.flush().unwrap();
        v.fail_next_sync();
        assert!(v.sync_on(&mut worker, false).unwrap().wait().is_err());
        assert!(v.needs_sync());
        let id = worker.thread_id();
        v.sync_on(&mut worker, false).unwrap().wait().unwrap();
        assert_eq!(worker.thread_id(), id);
        assert!(!v.needs_sync());
    }

    #[test]
    fn worker_drop_waits_for_queued_sync() {
        let root = tempfile::tempdir().unwrap();
        let v = DeltaVocab::open(&root.path().join("vocab")).unwrap();
        v.insert(b"<urn:pending>").unwrap();
        v.flush().unwrap();
        let file = v.file_handle();
        let held = file.lock();
        let mut worker = LazySync::default();
        let pending = v.sync_on(&mut worker, false).unwrap();
        let (done, finish) = mpsc::channel();
        let closer = std::thread::spawn(move || {
            drop(worker);
            done.send(()).unwrap();
        });
        assert!(matches!(
            finish.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(held);
        pending.wait().unwrap();
        finish.recv_timeout(Duration::from_secs(5)).unwrap();
        closer.join().unwrap();
        assert!(!v.needs_sync());
    }

    #[test]
    fn spawn_failure_uses_permanent_sequential_fallback() {
        let root = tempfile::tempdir().unwrap();
        let v = DeltaVocab::open(&root.path().join("vocab")).unwrap();
        let mut worker = LazySync {
            fail_spawn: true,
            ..Default::default()
        };
        v.insert(b"<urn:first>").unwrap();
        assert!(v.sync_on(&mut worker, false).is_none());
        v.sync().unwrap();
        worker.fail_spawn = false;
        v.insert(b"<urn:second>").unwrap();
        assert!(v.sync_on(&mut worker, false).is_none());
        v.sync().unwrap();
        assert!(worker.thread_id().is_none());
        assert!(!v.needs_sync());
    }
}
