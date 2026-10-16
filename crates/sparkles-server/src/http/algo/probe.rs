//! Per-request test observers. No hook or threshold override exists in ordinary builds.
use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
pub(crate) struct Probe {
    pub events: Arc<Mutex<Vec<&'static str>>>,
    pub blocking: Option<Arc<Gate>>,
    pub producer: Option<Arc<Gate>>,
    pub cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
}

tokio::task_local! {
    pub(crate) static CURRENT: Probe;
}

pub(crate) fn current() -> Probe {
    CURRENT.try_with(Clone::clone).unwrap_or_default()
}

pub(crate) fn event(extensions: &axum::http::Extensions, name: &'static str) {
    if let Some(probe) = extensions.get::<Probe>() {
        probe.events.lock().push(name);
    }
}

#[derive(Default)]
struct State {
    released: bool,
    finished: bool,
}

#[derive(Default)]
pub(crate) struct Gate {
    pub entered: tokio::sync::Notify,
    pub done: tokio::sync::Notify,
    pub timed_out: AtomicBool,
    state: Mutex<State>,
    changed: Condvar,
}

impl Gate {
    pub fn wait(&self) {
        self.entered.notify_one();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut state = self.state.lock();
        while !state.released {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.timed_out.store(true, Ordering::Relaxed);
                return;
            }
            self.changed.wait_for(&mut state, remaining);
        }
    }
    pub fn release(&self) {
        self.state.lock().released = true;
        self.changed.notify_all();
    }
    pub fn finish(&self) {
        self.state.lock().finished = true;
        self.changed.notify_all();
        self.done.notify_one();
    }
}

/// Completion follows the producer/blocking function's permit release.
pub(crate) struct Completion(pub Arc<Gate>);
impl Drop for Completion {
    fn drop(&mut self) {
        self.0.finish();
    }
}

/// Always releases and joins the observed work on an assertion failure as well.
pub(crate) struct Release(pub Arc<Gate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut state = self.0.state.lock();
        while !state.finished {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                if !std::thread::panicking() {
                    panic!("owned test work did not finish after release");
                }
                return;
            }
            self.0.changed.wait_for(&mut state, remaining);
        }
    }
}
