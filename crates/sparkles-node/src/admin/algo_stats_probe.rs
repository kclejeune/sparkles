//! Private, feature-only filesystem acknowledgments for isolated Node control tests.
//! No JavaScript API or production option is added.
use sparkles::algo::{CheckOutcome, Control, Par, pool, set_check_observer};
use sparkles::{Error, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    pub(super) fn install(ctl: &sparkles::task::Control) -> Result<Option<Self>> {
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
                std::fs::write(
                    dir.join("entered.json"),
                    serde_json::json!({
                        "mode": if slot {"slot"} else {"compute"}, "file": file, "line": line,
                        "deadlineForwarded": deadline == expected_deadline,
                        "hasTaskDeadline": expected_deadline.is_some(),
                    })
                    .to_string(),
                )
                .unwrap();
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
        std::fs::write(
            self.dir.join("finished.json"),
            serde_json::json!({
                "result": kind, "algorithmEntered": self.entered.load(Ordering::SeqCst),
                "acknowledged": self.notified.load(Ordering::SeqCst),
                "nativeOutcomes": *self.outcomes.lock().unwrap(),
                "holderReleasedEarly": self.early_release.load(Ordering::SeqCst),
                "slotReusable": reusable, "hadTaskDeadline": self.original_deadline.is_some(),
            })
            .to_string(),
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
