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
pub(crate) struct Operation;

impl Operation {
    pub(super) fn enter() -> Self {
        OPERATIONS.with(|s| s.borrow_mut().depth += 1);
        Operation
    }

    pub(crate) fn release_publication(credit: MemoryLease) {
        Self::after(move || drop(credit));
    }

    pub(super) fn after(f: impl FnOnce() + 'static) {
        let mut f = Some(f);
        let _ = OPERATIONS.try_with(|s| {
            let mut s = s.borrow_mut();
            if s.depth != 0 {
                s.close.push(Box::new(f.take().unwrap()));
            }
        });
        if let Some(f) = f {
            f();
        }
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
    _operation: Operation,
}

impl Runtime {
    /// Keep the worker from starting a round until the returned pause drops, waiting
    /// until `until` for a round in progress to end. `None` means that the round
    /// outlasted the wait. The worker holds its dataset only during a round, so a
    /// catalog pauses it to tell the dataset's own work apart from other handles before
    /// it closes the dataset. The gate is reentrant, so the pausing thread can still
    /// close this runtime while it holds the pause.
    pub fn pause_worker(&self, until: Instant) -> Option<WorkerPause<'_>> {
        self.worker.try_lock_until(until).map(|gate| WorkerPause {
            _gate: gate,
            _operation: Operation::enter(),
        })
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
        Operation::after(close);
    }

    /// Stop the rounds and checkpoint every view (a clean close).
    pub fn close(&self, store: &Store) {
        let _operation = Operation::enter();
        if self.worker.is_owned_by_current_thread() && self.worker_round.load(Ordering::Acquire) {
            self.deferred_close.store(true, Ordering::Release);
            return;
        }
        let _worker = self.worker.lock();
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        // Guarantee registry teardown, circuit clearing, and lock-free credit release
        // even if `checkpoint(store)` unwinds on a failpoint or panic.
        struct CloseCleanup<'a> {
            runtime: &'a Runtime,
        }
        impl Drop for CloseCleanup<'_> {
            fn drop(&mut self) {
                let mut queued = Vec::new();
                let mut retired = Vec::new();
                let (mut cells, internal, live) = {
                    let mut reg = self.runtime.reg.lock();
                    let internal = std::mem::take(&mut reg.internal);
                    let mut cells: Vec<Arc<Cell>> = reg.cells.drain().map(|(_, c)| c).collect();
                    cells.extend(reg.views.values_mut().filter_map(|e| e.cell.take()));
                    let live = std::mem::take(&mut reg.live);
                    (cells, internal, live)
                };
                cells.extend(live.iter().filter_map(|entry| entry.cell.upgrade()));
                cells.sort_unstable_by_key(Arc::as_ptr);
                cells.dedup_by(|a, b| Arc::ptr_eq(a, b));
                for c in &cells {
                    let mut st = c.state.lock();
                    st.subs.clear();
                    st.clear_history();
                    st.circuit.clear_state();
                    retired.extend(st.circuit.drain_retired());
                }
                for c in internal.values() {
                    let mut st = c.state.lock();
                    st.tally = None;
                    st.circuit.set_external_best_effort(0);
                    st.circuit.clear_state();
                    retired.extend(st.circuit.drain_retired());
                }
                for entry in live {
                    if let Some(shared) = entry.queue.upgrade() {
                        queued.push(shared.close(true));
                    }
                }
                drop(cells);
                drop(internal);
                // WorkerRound may still own an outer worker guard on this thread.
                Operation::after(move || {
                    drop(queued);
                    drop(retired);
                });
                *self.runtime.subscription.lock() = None;
                self.runtime.kick();
                let _e = self.runtime.epoch.lock();
                self.runtime.cv.notify_all();
            }
        }
        let _cleanup = CloseCleanup { runtime: self };
        self.checkpoint(store);
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
                runtime.complete_round();
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

    pub(super) fn complete_round(&self) {
        *self.epoch.lock() += 1;
        self.cv.notify_all();
    }

    /// Whether a worker round or its synchronized shutdown is in progress.
    pub fn worker_busy(&self) -> bool {
        self.worker.is_locked()
    }

    /// Run a round on the worker and wait for it, at most `timeout`: views defined in
    /// the dataset are then attached and recovered.
    pub fn settle(&self, timeout: Duration) {
        self.settle_checked(timeout);
    }

    /// Kick the worker and report whether a round completed.
    pub fn settle_checked(&self, timeout: Duration) -> bool {
        let start = self.round_epoch();
        self.kick();
        self.wait_round_after(start, Instant::now() + timeout)
    }

    /// The current completed-round epoch, captured before inspecting a condition that
    /// [`wait_round_after`](Self::wait_round_after) will wait on.
    pub(super) fn round_epoch(&self) -> u64 {
        *self.epoch.lock()
    }

    /// Wait until a round has finished after `start`, at most until `until`.
    pub(super) fn wait_round_after(&self, start: u64, until: Instant) -> bool {
        let mut e = self.epoch.lock();
        while *e == start {
            if self.is_closed() {
                return false;
            }
            if self.cv.wait_until(&mut e, until).timed_out() {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
#[test]
fn close_releases_credit_after_unlocking_live_cell() {
    let store = Store::in_memory(Default::default());
    crate::sparql::update::update(
        &store,
        "INSERT DATA { <urn:s> <urn:p> 1 }",
        &Default::default(),
    )
    .unwrap();
    let pool = QueryPool::new(8 << 20);
    for (in_worker, paused) in [(false, false), (true, false), (false, true)] {
        let runtime = Runtime::open_with_pool(
            None,
            store.dataset_id(),
            RuntimeOptions::default(),
            Some(pool.clone()),
        );
        runtime.set_host(Arc::new(OpenHost));
        let query = "SELECT ?s WHERE { ?s <urn:p> ?o }";
        runtime.put(&store, "view", ViewDef::table(query)).unwrap();
        runtime.round(&store);
        let live = runtime
            .live(&store, query, None, &LiveOptions::default())
            .unwrap();
        let filler = pool
            .work_parent()
            .reserve(pool.capacity() - (4 << 20) - pool.granted() + 1)
            .unwrap();
        let cell = live.slot.clone();
        let rt = runtime.clone();
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback = woke.clone();
        let ticket = pool.enter(
            4 << 20,
            Some(Arc::new(move || {
                let status = if !rt.worker_busy() && cell.state.try_lock().is_some() {
                    1
                } else {
                    2
                };
                let _ = callback.compare_exchange(0, status, Ordering::SeqCst, Ordering::SeqCst);
            })),
        );
        if in_worker {
            let _operation = Operation::enter();
            let _worker = runtime.worker.lock();
            runtime.worker_round.store(true, Ordering::Release);
            let active = WorkerRound {
                runtime: &runtime,
                store: &store,
            };
            runtime.close(&store);
            drop(active);
            assert_eq!(woke.load(Ordering::SeqCst), 0);
        } else if paused {
            let pause = runtime
                .pause_worker(Instant::now() + Duration::from_secs(1))
                .unwrap();
            runtime.close(&store);
            assert_eq!(woke.load(Ordering::SeqCst), 0);
            drop(pause);
        } else {
            runtime.close(&store);
        }
        assert_eq!(woke.load(Ordering::SeqCst), 1);
        assert_eq!(pool.granted(), filler.bytes());
        drop(ticket);
    }
}
