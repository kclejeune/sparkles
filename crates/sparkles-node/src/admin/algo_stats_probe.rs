//! Private, feature-only filesystem acknowledgments for isolated Node control tests.
//! No JavaScript API or production option is added.
#[cfg(unix)]
use sparkles::algo::{CheckOutcome, pool};
use sparkles::algo::{Control, Par, set_check_observer};
use sparkles::{Error, Result};
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub(super) struct Probe {
    dir: PathBuf,
    entered: Arc<AtomicBool>,
    notified: Arc<AtomicBool>,
    outcomes: Arc<Mutex<Vec<String>>>,
    early_release: Arc<AtomicBool>,
    release: Arc<Mutex<Option<mpsc::Sender<()>>>>,
    holder: Option<JoinHandle<()>>,
    original_deadline: Option<Instant>,
}

impl Probe {
    pub(super) fn install(_ctl: &sparkles::task::Control) -> Result<Option<Self>> {
        let Some(dir) = std::env::var_os("SPARKLES_TEST_ALGO_STATS_DIR").map(PathBuf::from) else {
            return Ok(None);
        };
        let mode = match std::fs::read_to_string(dir.join("armed")) {
            Ok(mode) => mode,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let slot = mode.trim() == "slot";
        if !slot && mode.trim() != "compute" {
            return Err(Error::invalid("unknown private stats probe mode"));
        }
        #[cfg(not(unix))]
        return Err(Error::unsupported(
            "private stats ACK probes require Unix checked file close",
        ));
        #[cfg(unix)]
        {
            let ctl = _ctl;
            pool::configure(1, 1);
            let release = Arc::new(Mutex::new(None));
            let holder = if slot {
                let (ready_tx, ready_rx) = mpsc::channel();
                let (tx, rx) = mpsc::channel();
                *release.lock().unwrap() = Some(tx);
                let h = std::thread::spawn(move || {
                    let held = Par::acquire(
                        1,
                        &Control::new(
                            Arc::new(AtomicBool::new(false)),
                            Some(Instant::now() + Duration::from_secs(2)),
                            u64::MAX,
                        ),
                    )
                    .unwrap();
                    ready_tx.send(()).unwrap();
                    let _ = rx.recv_timeout(Duration::from_secs(8));
                    drop(held);
                });
                if ready_rx.recv_timeout(Duration::from_secs(3)).is_err() {
                    if let Some(tx) = release.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                    let _ = h.join();
                    return Err(Error::invalid("private slot holder did not enter"));
                }
                Some(h)
            } else {
                None
            };
            let probe = Self {
                dir: dir.clone(),
                entered: Arc::new(AtomicBool::new(false)),
                notified: Arc::new(AtomicBool::new(false)),
                outcomes: Arc::new(Mutex::new(Vec::new())),
                early_release: Arc::new(AtomicBool::new(false)),
                release: release.clone(),
                holder,
                original_deadline: ctl.deadline,
            };
            let entered = probe.entered.clone();
            let notified = probe.notified.clone();
            let outcomes = probe.outcomes.clone();
            let early_release = probe.early_release.clone();
            let pool_checks = AtomicUsize::new(0);
            let expected_deadline = ctl.deadline;
            set_check_observer(Some(Arc::new(move |file, line, outcome, deadline| {
                let node = file.ends_with("/algo/node2vec.rs");
                if node && outcome == CheckOutcome::Passed {
                    entered.store(true, Ordering::SeqCst);
                }
                let boundary = if slot {
                    file.ends_with("/algo/pool.rs")
                        && outcome == CheckOutcome::Before
                        && pool_checks.fetch_add(1, Ordering::SeqCst) == 2
                } else {
                    node && outcome == CheckOutcome::Before && entered.load(Ordering::SeqCst)
                };
                if boundary && !notified.swap(true, Ordering::SeqCst) {
                    publish_ack(
                        &dir,
                        "entered",
                        &serde_json::json!({
                            "mode": if slot {"slot"} else {"compute"}, "file": file, "line": line,
                            "deadlineForwarded": deadline == expected_deadline,
                            "hasTaskDeadline": expected_deadline.is_some(),
                        }),
                    )
                    .expect("private entered ACK publication failed");
                    let stop = Instant::now() + Duration::from_secs(5);
                    while !dir.join("release").exists() {
                        assert!(
                            Instant::now() < stop,
                            "private stats probe release timed out"
                        );
                        std::thread::park_timeout(Duration::from_millis(2));
                    }
                }
                if notified.load(Ordering::SeqCst) && outcome != CheckOutcome::Before {
                    outcomes.lock().unwrap().push(format!("{outcome:?}"));
                    // Let the buggy before source complete, so its actual ignored check
                    // fails an assertion instead of stranding the child behind the holder.
                    if slot
                        && outcome == CheckOutcome::Passed
                        && let Some(tx) = release.lock().unwrap().take()
                    {
                        early_release.store(true, Ordering::SeqCst);
                        let _ = tx.send(());
                    }
                }
            })));
            Ok(Some(probe))
        }
    }

    pub(super) fn finish(mut self, result: &Result<String>) -> Result<()> {
        set_check_observer(None);
        self.release_holder();
        let deadline = Instant::now() + Duration::from_secs(1);
        let reusable = Par::acquire(
            1,
            &Control::new(Arc::new(AtomicBool::new(false)), Some(deadline), u64::MAX),
        )
        .is_ok();
        let kind = match result {
            Ok(_) => "Ok",
            Err(Error::Cancelled) => "Cancelled",
            Err(Error::Timeout) => "Timeout",
            Err(_) => "OtherError",
        };
        publish_ack(
            &self.dir,
            "finished",
            &serde_json::json!({
                "result": kind, "algorithmEntered": self.entered.load(Ordering::SeqCst),
                "acknowledged": self.notified.load(Ordering::SeqCst),
                "nativeOutcomes": *self.outcomes.lock().unwrap(),
                "holderReleasedEarly": self.early_release.load(Ordering::SeqCst),
                "slotReusable": reusable, "hadTaskDeadline": self.original_deadline.is_some(),
            }),
        )?;
        Ok(())
    }

    fn release_holder(&mut self) {
        if let Some(tx) = self.release.lock().unwrap().take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.holder.take() {
            h.join().expect("private slot holder panicked");
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        set_check_observer(None);
        self.release_holder();
    }
}

// The directory belongs exclusively to one isolated child. Each ACK has one
// publisher; its create_new temporary file also rejects concurrent duplicates.
#[cfg(unix)]
fn publish_ack(
    dir: &std::path::Path,
    name: &str,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    use std::io::Write;
    let bytes = serde_json::to_vec(value)?;
    publish_bytes(dir, name, &bytes, |file, bytes| file.write_all(bytes))
}

#[cfg(not(unix))]
fn publish_ack(_: &std::path::Path, _: &str, _: &serde_json::Value) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "private stats ACK probes require Unix checked file close",
    ))
}

#[cfg(unix)]
fn publish_bytes(
    dir: &std::path::Path,
    name: &str,
    bytes: &[u8],
    write: impl FnOnce(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    if !matches!(name, "entered" | "finished") {
        return Err(Error::new(ErrorKind::InvalidInput, "unknown private ACK"));
    }
    let final_path = dir.join(format!("{name}.json"));
    let fresh = || match final_path.symlink_metadata() {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(_) => Err(Error::new(
            ErrorKind::AlreadyExists,
            "stale or duplicate private ACK",
        )),
    };
    fresh()?;
    let temporary = dir.join(format!(".{name}.json.tmp"));
    // Failure here must not remove another publisher's file.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let written = write(&mut file, bytes);
    // Consuming close reports errors without a second close or EINTR retry. This
    // is ephemeral publication, not an fsync/durability protocol.
    let closed = nix::unistd::close(file).map_err(std::io::Error::from);
    let result = written
        .and(closed)
        .and_then(|()| fresh())
        .and_then(|()| std::fs::rename(&temporary, &final_path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary); // Only our successfully created file.
    }
    result
}

#[cfg(all(test, unix))]
#[path = "algo_stats_probe_tests.rs"]
mod publication_tests;
