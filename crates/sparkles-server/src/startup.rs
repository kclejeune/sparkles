//! Bind-before-open startup (spec C01 §4.1, Phase 2).
//!
//! `serve` binds its listeners first and then opens the registered datasets here, on a
//! bounded set of threads. Meanwhile readiness reports `starting`, each waiting dataset
//! reports `opening`, and its requests answer `503` with `Retry-After`. A dataset that
//! fails to open stays registered and closed, and the server is then `degraded`. With
//! `--require-all-datasets` such a failure stops the server instead.

use crate::state::AppState;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// The most datasets opened at once by default. Each open replays its log and maps its
/// indexes, so a few in parallel use the disk well without multiplying peak memory by
/// the number of datasets.
pub const DEFAULT_OPEN_THREADS: usize = 4;

/// A test's hook that runs before a dataset opens.
#[cfg(test)]
pub type Hook = Arc<dyn Fn(&str) + Send + Sync>;

/// How startup opens the datasets.
#[derive(Clone)]
pub struct Options {
    /// datasets opened at once (at least 1)
    pub threads: usize,
    /// a dataset that fails to open stops the server (`--require-all-datasets`)
    pub require_all: bool,
    /// runs on the opening thread just before each dataset opens
    #[cfg(test)]
    pub before_open: Option<Hook>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            threads: std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .min(DEFAULT_OPEN_THREADS),
            require_all: false,
            #[cfg(test)]
            before_open: None,
        }
    }
}

/// The thread that opens the datasets, then runs the rest of startup.
pub struct Startup {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// Open `pending`, the registered datasets of [`AppState::deferred`], then run `then`
/// (the startup steps that need the datasets open), then mark the server ready. A
/// fatal error, from `then` or from a failed dataset under `require_all`, goes to
/// `fatal`, which should stop the server.
pub fn spawn(
    st: Arc<AppState>,
    pending: Vec<String>,
    options: Options,
    then: impl FnOnce(&Arc<AppState>) -> anyhow::Result<()> + Send + 'static,
    fatal: impl FnOnce(anyhow::Error) + Send + 'static,
) -> Startup {
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let handle = std::thread::Builder::new()
        .name("startup".into())
        .spawn(move || {
            let t0 = Instant::now();
            let failed = open_all(&st, pending, &options, &stopping);
            if stopping.load(Ordering::SeqCst) {
                return;
            }
            if options.require_all && !failed.is_empty() {
                fatal(anyhow::anyhow!(
                    "--require-all-datasets: {} did not open: /{}",
                    if failed.len() == 1 {
                        "a dataset"
                    } else {
                        "datasets"
                    },
                    failed.join(", /")
                ));
                return;
            }
            if let Err(e) = then(&st) {
                fatal(e);
                return;
            }
            if st.mark_ready() {
                let (status, ready) = crate::obs::server_status(&st);
                tracing::info!(
                    "datasets opened in {:.1?}: status {status}, ready {ready}",
                    t0.elapsed()
                );
            }
        })
        .expect("spawning the startup thread");
    Startup {
        stop,
        handle: Some(handle),
    }
}

impl Startup {
    /// Open no more datasets, and wait for the ones opening now. A dataset left
    /// unopened stays registered and opens at the next start.
    pub fn stop_and_join(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            if !h.is_finished() {
                tracing::info!("waiting for the datasets that are opening");
            }
            let _ = h.join();
        }
    }
}

impl Drop for Startup {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Open `pending` on up to `options.threads` threads, in registry order, and return the
/// names that failed. Stops taking new datasets once `stop` is set.
fn open_all(
    st: &AppState,
    pending: Vec<String>,
    options: &Options,
    stop: &AtomicBool,
) -> Vec<String> {
    let threads = options.threads.max(1).min(pending.len());
    let queue = Mutex::new(VecDeque::from(pending));
    let failed = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for i in 0..threads {
            let (queue, failed) = (&queue, &failed);
            std::thread::Builder::new()
                .name(format!("open-{i}"))
                .spawn_scoped(s, move || {
                    loop {
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        let Some(name) = queue.lock().pop_front() else {
                            break;
                        };
                        #[cfg(test)]
                        if let Some(hook) = &options.before_open {
                            hook(&name);
                        }
                        let t0 = Instant::now();
                        match st.catalog.open_registered(&name) {
                            Ok(()) => {
                                tracing::info!(
                                    "  dataset /{name} opened in {:.1?}  →  /{name}/sparql  /{name}/update  /{name}/data",
                                    t0.elapsed()
                                );
                            }
                            // the catalog logged the reason
                            Err(_) => failed.lock().push(name),
                        }
                    }
                })
                .expect("spawning a dataset opening thread");
        }
    });
    let mut failed = failed.into_inner();
    failed.sort();
    failed
}

/// A hook for [`Options::before_open`] that holds the opening of chosen datasets until
/// the test releases them.
#[cfg(test)]
pub(crate) mod gate {
    use parking_lot::{Condvar, Mutex};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    #[derive(Default)]
    pub struct Gate {
        held: Mutex<BTreeSet<String>>,
        entered: Mutex<BTreeSet<String>>,
        // one condition variable per mutex
        released: Condvar,
        reached: Condvar,
    }

    impl Gate {
        /// A gate that holds `names`.
        pub fn holding(names: &[&str]) -> Arc<Gate> {
            let g = Gate::default();
            g.held.lock().extend(names.iter().map(|n| n.to_string()));
            Arc::new(g)
        }
        pub fn hook(self: &Arc<Self>) -> super::Hook {
            let g = self.clone();
            Arc::new(move |name: &str| {
                g.entered.lock().insert(name.to_string());
                g.reached.notify_all();
                let mut held = g.held.lock();
                while held.contains(name) {
                    g.released.wait(&mut held);
                }
            })
        }
        /// Wait until the opening of `name` reached the gate.
        pub fn wait_entered(&self, name: &str) {
            let mut e = self.entered.lock();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !e.contains(name) {
                assert!(
                    !self.reached.wait_until(&mut e, deadline).timed_out(),
                    "{name} never started to open"
                );
            }
        }
        pub fn entered(&self, name: &str) -> bool {
            self.entered.lock().contains(name)
        }
        pub fn release(&self, name: &str) {
            self.held.lock().remove(name);
            self.released.notify_all();
        }
    }
}
