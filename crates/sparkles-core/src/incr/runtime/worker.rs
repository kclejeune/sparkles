//! The worker thread, shutdown, and the operations a thread runs on the runtime.

use super::*;

struct Operations {
    depth: usize,
    close: Vec<Box<dyn FnOnce()>>,
}

type OperationStorage = ManuallyDrop<RefCell<Operations>>;

// A TLS owner can call views after other TLS destructors have run. This key must
// never register a destructor or become inaccessible during thread teardown.
const _: () = assert!(!std::mem::needs_drop::<OperationStorage>());

thread_local! {
    // Nesting can cross runtimes. No cleanup runs while any caller's metadata
    // locks or worker gate remain held on this thread. Every outer drain takes
    // and frees the pending Vec, leaving this destruction-free storage empty.
    static OPERATIONS: OperationStorage = const {
        ManuallyDrop::new(RefCell::new(Operations { depth: 0, close: Vec::new() }))
    };
}

/// Declare before operation-local guards, so cleanup runs after their locks drop.
pub(super) struct Operation;

impl Operation {
    pub(super) fn enter() -> Self {
        OPERATIONS.with(|s| s.borrow_mut().depth += 1);
        Operation
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        let close = OPERATIONS.with(|s| {
            let mut s = s.borrow_mut();
            s.depth -= 1;
            if s.depth == 0 {
                std::mem::take(&mut s.close)
            } else {
                Vec::new()
            }
        });
        // Never retain a TLS borrow while cleanup can call another runtime.
        for close in close {
            close();
        }
    }
}

/// Finish deferred shutdown after the round releases its locks, including while
/// unwinding a panic. The worker's store wrapper remains alive until this drops.
struct WorkerRound<'a> {
    runtime: &'a Runtime,
    store: &'a Store,
}

impl Drop for WorkerRound<'_> {
    fn drop(&mut self) {
        self.runtime.worker_round.store(false, Ordering::Release);
        if self.runtime.deferred_close.swap(false, Ordering::AcqRel) {
            self.runtime.close(self.store);
        }
    }
}

/// Keeps a views worker from starting a round while it lives (from
/// [`Runtime::pause_worker`]).
#[must_use]
pub struct WorkerPause<'a> {
    _gate: parking_lot::ReentrantMutexGuard<'a, ()>,
}

impl Runtime {
    /// Keep the worker from starting a round until the returned pause drops, waiting
    /// until `until` for a round in progress to end. `None` means that the round
    /// outlasted the wait. The worker holds its dataset only during a round, so a
    /// catalog pauses it to tell the dataset's own work apart from other handles before
    /// it closes the dataset. The gate is reentrant, so the pausing thread can still
    /// close this runtime while it holds the pause.
    pub fn pause_worker(&self, until: Instant) -> Option<WorkerPause<'_>> {
        self.worker
            .try_lock_until(until)
            .map(|gate| WorkerPause { _gate: gate })
    }

    /// Coordinate an owned dataset close with this thread's runtime operations.
    /// Synchronous callbacks retain the owner until their outer operation ends.
    /// A worker round already defers close and retains its store until cleanup.
    #[doc(hidden)]
    pub fn defer_close(&self, close: impl FnOnce() + 'static) {
        if self.worker.is_owned_by_current_thread() && self.worker_round.load(Ordering::Acquire) {
            // Runtime::close defers cleanup until WorkerRound drops. Do not retain
            // another owner in TLS beyond the worker gate's release.
            close();
            return;
        }
        let mut close: Option<Box<dyn FnOnce()>> = Some(Box::new(close));
        let _ = OPERATIONS.try_with(|s| {
            let mut s = s.borrow_mut();
            if s.depth != 0 {
                s.close.push(close.take().unwrap());
            }
        });
        if let Some(close) = close {
            close();
        }
    }

    /// Stop the rounds and checkpoint every view (a clean close).
    pub fn close(&self, store: &Store) {
        if self.worker.is_owned_by_current_thread() && self.worker_round.load(Ordering::Acquire) {
            self.deferred_close.store(true, Ordering::Release);
            return;
        }
        let _worker = self.worker.lock();
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.checkpoint(store);
        let cells: Vec<Arc<Cell>> = {
            let mut reg = self.reg.lock();
            reg.internal.clear();
            reg.cells.drain().map(|(_, c)| c).collect()
        };
        for c in cells {
            let mut st = c.state.lock();
            for s in st.subs.drain(..) {
                if let Some(sh) = s.shared.upgrade() {
                    sh.close();
                }
            }
        }
        *self.subscription.lock() = None;
        self.kick();
        self.cv.notify_all();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Run rounds on a thread of its own until `store` returns `None` (the dataset is
    /// gone) or the runtime closes. A round runs after each commit and each
    /// [`kick`](Self::kick).
    pub fn spawn_worker<S, F>(
        self: &Arc<Self>,
        name: String,
        mut commits: tokio::sync::watch::Receiver<u64>,
        store: F,
    ) -> std::io::Result<std::thread::JoinHandle<()>>
    where
        S: std::ops::Deref<Target = Store>,
        F: Fn() -> Option<S> + Send + 'static,
    {
        let rt = Arc::downgrade(self);
        let kick = self.kick.clone();
        std::thread::Builder::new().name(name).spawn(move || {
            crate::threads::started();
            let Ok(exec) = tokio::runtime::Builder::new_current_thread().build() else {
                return;
            };
            loop {
                let Some(runtime) = rt.upgrade() else { return };
                {
                    let _operation = Operation::enter();
                    let _worker = runtime.worker.lock();
                    if runtime.is_closed() {
                        return;
                    }
                    commits.borrow_and_update();
                    let Some(s) = store() else { return };
                    runtime.worker_round.store(true, Ordering::Release);
                    let active = WorkerRound {
                        runtime: &runtime,
                        store: &s,
                    };
                    runtime.round(&s);
                    drop(active);
                    drop(s);
                }
                drop(runtime);
                let gone = exec.block_on(async {
                    let mut changed = std::pin::pin!(commits.changed());
                    let mut kicked = std::pin::pin!(kick.notified());
                    std::future::poll_fn(|cx| {
                        if let std::task::Poll::Ready(r) = changed.as_mut().poll(cx) {
                            return std::task::Poll::Ready(r.is_err());
                        }
                        if kicked.as_mut().poll(cx).is_ready() {
                            return std::task::Poll::Ready(false);
                        }
                        std::task::Poll::Pending
                    })
                    .await
                });
                if gone {
                    return;
                }
            }
        })
    }

    /// Whether a worker round or its synchronized shutdown is in progress.
    pub fn worker_busy(&self) -> bool {
        self.worker.is_locked()
    }

    /// Run a round on the worker and wait for it, at most `timeout`: views defined in
    /// the dataset are then attached and recovered.
    pub fn settle(&self, timeout: Duration) {
        self.kick();
        self.wait_round(Instant::now() + timeout);
    }

    /// Wait until a round has finished after this call, at most until `until`.
    pub(super) fn wait_round(&self, until: Instant) -> bool {
        let mut e = self.epoch.lock();
        let start = *e;
        while *e == start {
            if self.cv.wait_until(&mut e, until).timed_out() {
                return false;
            }
        }
        true
    }
}
