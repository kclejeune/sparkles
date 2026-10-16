//! Where a run evaluates its rules, and how runs share a reasoning pool.
//!
//! **Off the caller's pool.** A run holds the store's writer lock while it evaluates, and
//! evaluation runs on rayon. A thread that waits for rayon work runs other queued rayon
//! jobs meanwhile: a worker that calls [`rayon::ThreadPool::install`] on another pool, or
//! that joins inside its own pool, may pick up a job that asks for the same writer lock
//! and then never return to the run that holds it. So a run never waits inside rayon
//! while it holds the lock. A run called on a rayon worker thread is moved to a native
//! thread of its own ([`off_rayon`]), and the worker parks until it ends, without taking
//! other jobs. From a native thread, `install` and the parallel iterators wait without
//! running other jobs. There is at most one such thread per waiting rayon worker, so their
//! number is bounded by the workers of the caller's pools.
//!
//! **Admission.** A [`ReasoningPool`] has a capacity, its number of threads. A run takes
//! that many workers when it uses the pool itself, or the number it asks for
//! ([`crate::ReasonOptions::threads`]) when that is smaller, on a pool of its own. Runs
//! are admitted in arrival order while the workers they take fit the capacity. Runs that
//! use the shared pool share its workers, so any number of them run together, but not
//! beside a run with a pool of its own. A run is admitted before it takes the writer lock,
//! waits for admission only while it is not cancelled, and gives its workers back when its
//! own pool's threads have ended, also when it fails or panics.

use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A thread pool for rule evaluation, shared by runs that pass it as
/// [`crate::ReasonOptions::pool`], with the admission of the module documentation.
pub struct ReasoningPool {
    pool: rayon::ThreadPool,
    admission: Admission,
}

impl std::fmt::Debug for ReasoningPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReasoningPool")
            .field("threads", &self.threads())
            .field("admitted", &self.admitted_workers())
            .finish()
    }
}

impl ReasoningPool {
    /// A pool of `threads` threads (at least one), named `reason-{i}`.
    pub fn new(threads: usize) -> anyhow::Result<ReasoningPool> {
        let threads = threads.max(1);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("reason-{i}"))
            .build()?;
        Ok(ReasoningPool {
            pool,
            admission: Admission::new(threads),
        })
    }

    /// The pool's threads, which is its capacity.
    pub fn threads(&self) -> usize {
        self.pool.current_num_threads()
    }

    /// The workers of the runs admitted now: the capacity while runs use the shared pool,
    /// otherwise the threads of the runs' own pools.
    pub fn admitted_workers(&self) -> usize {
        let s = self.admission.state.lock();
        self.admission.capacity - s.free
    }

    /// Runs waiting for admission.
    pub fn waiting_runs(&self) -> usize {
        self.admission.state.lock().queue.len()
    }
}

/// What a run asks of the capacity.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Want {
    /// the shared pool, whose workers it shares with the other runs on it
    Shared,
    /// a pool of its own with this many threads
    Own(usize),
}

struct Admission {
    capacity: usize,
    state: Mutex<State>,
    wake: Condvar,
}

struct State {
    /// workers not taken
    free: usize,
    /// runs admitted to the shared pool (which together take the whole capacity)
    shared: usize,
    /// waiting runs, in arrival order
    queue: VecDeque<(u64, Want)>,
    next: u64,
}

impl Admission {
    fn new(capacity: usize) -> Admission {
        Admission {
            capacity,
            state: Mutex::new(State {
                free: capacity,
                shared: 0,
                queue: VecDeque::new(),
                next: 0,
            }),
            wake: Condvar::new(),
        }
    }

    /// Wait until `want` fits, in arrival order, unless `cancel` is set first.
    fn admit(&self, want: Want, cancel: Option<&AtomicBool>) -> anyhow::Result<()> {
        let mut s = self.state.lock();
        let ticket = s.next;
        s.next += 1;
        s.queue.push_back((ticket, want));
        loop {
            if s.queue.front().is_some_and(|x| x.0 == ticket) && self.fits(&s, want) {
                s.queue.pop_front();
                match want {
                    Want::Shared => {
                        if s.shared == 0 {
                            s.free -= self.capacity;
                        }
                        s.shared += 1;
                    }
                    Want::Own(n) => s.free -= n,
                }
                // the next run may fit too
                self.wake.notify_all();
                return Ok(());
            }
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                s.queue.retain(|x| x.0 != ticket);
                self.wake.notify_all();
                anyhow::bail!("reasoning cancelled");
            }
            // cancellation is not signalled through the condition variable
            self.wake.wait_for(&mut s, Duration::from_millis(20));
        }
    }

    fn fits(&self, s: &State, want: Want) -> bool {
        match want {
            Want::Shared => s.shared > 0 || s.free == self.capacity,
            Want::Own(n) => s.shared == 0 && s.free >= n,
        }
    }

    fn release(&self, want: Want) {
        let mut s = self.state.lock();
        match want {
            Want::Shared => {
                s.shared -= 1;
                if s.shared == 0 {
                    s.free += self.capacity;
                }
            }
            Want::Own(n) => s.free += n,
        }
        self.wake.notify_all();
    }
}

/// Workers a run took from a [`ReasoningPool`], given back on drop.
struct Permit {
    pool: Arc<ReasoningPool>,
    want: Want,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.pool.admission.release(self.want);
    }
}

/// A pool built for one run, whose drop waits for its threads to exit.
struct OwnPool {
    pool: Option<rayon::ThreadPool>,
    threads: usize,
    exited: Arc<(Mutex<usize>, Condvar)>,
    /// the workers' threads, joined when the pool stops
    handles: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
}

/// How long a run's own pool may take to stop before its workers are given back anyway.
const STOP_WAIT: Duration = Duration::from_secs(30);

impl OwnPool {
    fn new(threads: usize) -> anyhow::Result<OwnPool> {
        let exited: Arc<(Mutex<usize>, Condvar)> = Arc::default();
        let handles: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>> = Arc::default();
        let e = exited.clone();
        let h = handles.clone();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .spawn_handler(move |t| {
                let j = std::thread::Builder::new()
                    .name(format!("reason-run-{}", t.index()))
                    .spawn(|| t.run())?;
                h.lock().push(j);
                Ok(())
            })
            .exit_handler(move |_| {
                *e.0.lock() += 1;
                e.1.notify_all();
            })
            .build()?;
        Ok(OwnPool {
            pool: Some(pool),
            threads,
            exited,
            handles,
        })
    }
}

impl Drop for OwnPool {
    /// Stop the workers and wait until their threads have ended, so that the workers
    /// given back are not still running beside the next run's.
    fn drop(&mut self) {
        drop(self.pool.take());
        let (n, cv) = &*self.exited;
        let mut n = n.lock();
        let deadline = std::time::Instant::now() + STOP_WAIT;
        while *n < self.threads {
            if cv.wait_until(&mut n, deadline).timed_out() {
                tracing::warn!(
                    threads = self.threads,
                    exited = *n,
                    "a reasoning run's threads did not stop in time"
                );
                return;
            }
        }
        drop(n);
        // every worker left its loop: its thread ends at once
        for j in self.handles.lock().drain(..) {
            let _ = j.join();
        }
    }
}

/// Where a run evaluates: the shared pool, a pool of its own, or the global pool. Its
/// drop stops a pool of its own, then gives the workers back.
pub(crate) struct Exec {
    // declared before the permit: dropped first
    own: Option<OwnPool>,
    shared: Option<Arc<ReasoningPool>>,
    _permit: Option<Permit>,
}

impl Exec {
    /// Admit a run with `threads` and `pool` ([`crate::ReasonOptions`]) and set up its
    /// threads. Called off rayon ([`off_rayon`]).
    pub fn admit(
        threads: Option<usize>,
        pool: Option<&Arc<ReasoningPool>>,
        cancel: Option<&AtomicBool>,
    ) -> anyhow::Result<Exec> {
        let asked = threads.filter(|&n| n > 0);
        match pool {
            Some(p) => {
                let cap = p.threads();
                let want = match asked {
                    Some(n) if n < cap => Want::Own(n),
                    _ => Want::Shared,
                };
                p.admission.admit(want, cancel)?;
                let permit = Permit {
                    pool: p.clone(),
                    want,
                };
                Ok(match want {
                    Want::Shared => Exec {
                        own: None,
                        shared: Some(p.clone()),
                        _permit: Some(permit),
                    },
                    Want::Own(n) => Exec {
                        own: Some(OwnPool::new(n)?),
                        shared: None,
                        _permit: Some(permit),
                    },
                })
            }
            None => Ok(Exec {
                own: asked
                    .or_else(caller_threads)
                    .map(OwnPool::new)
                    .transpose()?,
                shared: None,
                _permit: None,
            }),
        }
    }

    /// The error for a run called on a thread of the reasoning pool it would use: its
    /// evaluation would wait for workers that wait for it.
    pub fn refuse_own_threads(pool: Option<&Arc<ReasoningPool>>) -> anyhow::Result<()> {
        if pool.is_some_and(|p| p.pool.current_thread_index().is_some()) {
            anyhow::bail!(
                "a reasoning run cannot be started on a thread of its own reasoning pool"
            );
        }
        Ok(())
    }

    /// Run `f` on the run's threads.
    pub fn install<T: Send>(&self, f: impl FnOnce() -> T + Send) -> T {
        if let Some(p) = self.own.as_ref().and_then(|o| o.pool.as_ref()) {
            return p.install(f);
        }
        if let Some(p) = &self.shared {
            return p.pool.install(f);
        }
        f()
    }
}

/// Run `f` on a native thread when the caller is a rayon worker, parking the worker
/// until it ends; otherwise on the caller's thread. A panic in `f` is resumed in the
/// caller.
pub(crate) fn off_rayon<T: Send>(
    f: impl FnOnce() -> anyhow::Result<T> + Send,
) -> anyhow::Result<T> {
    let Some(_) = rayon::current_thread_index() else {
        return f();
    };
    // the caller's pool size: what "the caller's threads" means for a run without a pool
    let threads = rayon::current_num_threads();
    std::thread::scope(|s| {
        let h = std::thread::Builder::new()
            .name("call-reasoning".into())
            .spawn_scoped(s, move || {
                CALLER_THREADS.with(|c| {
                    c.set(Some(threads));
                    f()
                })
            })
            .map_err(|e| anyhow::anyhow!("starting a reasoning thread: {e}"))?;
        match h.join() {
            Ok(r) => r,
            Err(p) => std::panic::resume_unwind(p),
        }
    })
}

thread_local! {
    /// On a thread [`off_rayon`] started: the threads of the rayon pool that called.
    static CALLER_THREADS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// The threads of the rayon pool a run was called on, when [`off_rayon`] moved it off
/// that pool: such a run, without a pool or a thread count, evaluates on a pool of its
/// own of that size rather than on the global pool, whose workers may all be waiting
/// for runs.
pub(crate) fn caller_threads() -> Option<usize> {
    CALLER_THREADS.with(|c| c.get())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run started on a thread of the pool it would use is refused: it would wait for
    /// workers that wait for it.
    #[test]
    fn a_run_on_its_own_pool_is_refused() {
        let p = Arc::new(ReasoningPool::new(1).unwrap());
        assert!(Exec::refuse_own_threads(Some(&p)).is_ok());
        let e = p
            .pool
            .install(|| Exec::refuse_own_threads(Some(&p)))
            .unwrap_err();
        assert!(e.to_string().contains("its own reasoning pool"), "{e}");
        let other = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        assert!(other.install(|| Exec::refuse_own_threads(Some(&p))).is_ok());
    }

    /// A run moved off a rayon worker without a pool evaluates on a pool of its own of
    /// the caller pool's size.
    #[test]
    fn off_rayon_keeps_the_caller_pool_size() {
        let caller = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .unwrap();
        let n = caller
            .install(|| {
                off_rayon(|| {
                    assert!(rayon::current_thread_index().is_none());
                    let e = Exec::admit(None, None, None)?;
                    Ok(e.install(rayon::current_num_threads))
                })
            })
            .unwrap();
        assert_eq!(n, 3);
    }
}
