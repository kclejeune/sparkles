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
/// `fatal`, which should stop the server. `then` gets the flag that a shutdown sets
/// and checks it with [`StopFlag::check`] before each step, so a shutdown starts no
/// further step. An error that `then` returns once the flag is set is not fatal.
pub fn spawn(
    st: Arc<AppState>,
    pending: Vec<String>,
    options: Options,
    then: impl FnOnce(&Arc<AppState>, &StopFlag) -> anyhow::Result<()> + Send + 'static,
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
            // the later steps see the datasets that startup holds back from requests
            let flag = StopFlag(stopping.clone());
            if let Err(e) = with_access(|| then(&st, &flag)) {
                if !flag.stopped() {
                    fatal(e);
                }
                return;
            }
            if stopping.load(Ordering::SeqCst) {
                return;
            }
            st.release_startup();
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

/// Tells startup to stop: [`begin_shutdown`] sets it.
#[derive(Clone)]
pub struct StopFlag(Arc<AtomicBool>);

impl StopFlag {
    /// Whether a shutdown has started.
    pub fn stopped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// An error once a shutdown has started, so a startup step returns before it
    /// starts the next one.
    pub fn check(&self) -> anyhow::Result<()> {
        if self.stopped() {
            anyhow::bail!("startup stopped: the server is shutting down");
        }
        Ok(())
    }
}

/// What the server does first when a shutdown starts, before it drains its requests:
/// readiness reports `draining`, and startup opens no more datasets and runs no more
/// steps. The datasets opening now finish, and the server waits for them once the
/// requests have drained ([`Startup::stop_and_join`]).
pub fn begin_shutdown(st: &AppState, stop: &StopFlag) {
    st.set_phase(crate::obs::Phase::Draining);
    stop.0.store(true, Ordering::SeqCst);
}

thread_local! {
    static ACCESS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread runs startup's own steps, which see the datasets that startup
/// holds back from requests.
pub fn has_access() -> bool {
    ACCESS.get()
}

fn with_access<R>(f: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ACCESS.set(false);
        }
    }
    ACCESS.set(true);
    let _reset = Reset;
    f()
}

/// Hold back from requests, until startup has finished, the datasets that startup
/// attaches (`attach`, which the catalog reserves now) and those it configures
/// (`configured`, by name). Each answers as opening meanwhile, so no request reads or
/// writes it before its configuration is installed.
pub fn hold(
    st: &AppState,
    attach: &[(String, crate::state::DbType)],
    configured: &[String],
) -> anyhow::Result<()> {
    for (name, kind) in attach {
        st.catalog.expect(name)?;
        st.hold_for_startup(name, *kind);
    }
    for name in configured {
        if !st.startup_pending.lock().contains_key(name) {
            st.hold_for_startup(name, crate::state::DbType::Persistent);
        }
    }
    Ok(())
}

/// The dataset a startup step such as `--text NAME[=CONFIG]` names.
pub fn step_dataset(spec: &str) -> &str {
    spec.split('=').next().unwrap_or("").trim_start_matches('/')
}

/// Hooks that run in [`AppState::lookup`] between its first miss and the state checks,
/// by dataset name (each test uses names of its own).
#[cfg(test)]
pub(crate) static LOOKUP_HOOKS: parking_lot::Mutex<
    std::collections::BTreeMap<String, Arc<dyn Fn() + Send + Sync>>,
> = parking_lot::Mutex::new(std::collections::BTreeMap::new());

#[cfg(test)]
pub(crate) fn lookup_hook(name: &str) {
    let hook = LOOKUP_HOOKS.lock().get(name).cloned();
    if let Some(h) = hook {
        h()
    }
}

/// Hooks that run at the end of a startup step for a dataset, by dataset name: so far
/// after each data load of `--fuseki-config`.
#[cfg(test)]
pub(crate) static STEP_HOOKS: parking_lot::Mutex<
    std::collections::BTreeMap<String, Arc<dyn Fn() + Send + Sync>>,
> = parking_lot::Mutex::new(std::collections::BTreeMap::new());

#[cfg(test)]
pub(crate) fn step_hook(name: &str) {
    let hook = STEP_HOOKS.lock().get(name).cloned();
    if let Some(h) = hook {
        h()
    }
}

impl Startup {
    /// The flag that [`begin_shutdown`] sets.
    pub fn stop_flag(&self) -> StopFlag {
        StopFlag(self.stop.clone())
    }

    /// Whether startup has finished or given up.
    #[cfg(test)]
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }

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
                                // the routing object, with its views started, so that
                                // the first request does not make it
                                let _ = st.get(&name);
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
