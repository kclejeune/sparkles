//! The catalog's management lock and the operations that close callbacks defer.
//!
//! Every management call takes one lock. Disposal runs dataset close callbacks on the
//! thread that disposes, sometimes while that thread holds the lock, so a callback that
//! called back into management would wait on itself. A thread-local record of the
//! catalogs that the current thread manages or disposes turns that call into an
//! immediate refusal instead. Callbacks queue such work with [`Catalog::defer`], and the
//! queue runs on the same thread once the outermost management call or disposal of
//! that catalog on the thread has released the lock.
//!
//! The lock order is the management lock, then `entries`, then `detached` or `refused`.
//! The queue's mutex is a leaf that is never held while an operation runs.

use super::{Catalog, Inner};
use crate::{Error, Result};
use parking_lot::{Mutex, MutexGuard};
use std::cell::RefCell;
use std::thread::ThreadId;

type Deferred = Box<dyn FnOnce(&Catalog) + Send>;

pub(crate) struct ManageGate {
    lock: Mutex<()>,
    pub(super) deferred: Mutex<Vec<(ThreadId, Deferred)>>,
}
impl ManageGate {
    pub(crate) fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            deferred: Mutex::new(Vec::new()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// The thread holds the management lock.
    Holding,
    /// The thread runs close callbacks of a dataset that the catalog disposes.
    Disposing,
    /// The thread runs a multi-step operation, such as an in-place restore, whose
    /// deferred work waits until the whole operation returns.
    Batch,
    /// The thread runs the operations it deferred. Management calls that those
    /// operations make, and operations they defer, leave new work to this loop
    /// instead of starting a nested one, so a chain of deferrals uses constant stack.
    Draining,
}

thread_local! {
    /// The catalogs, by `Inner` address, that this thread manages or disposes.
    static MARKS: RefCell<Vec<(usize, Mark)>> = const { RefCell::new(Vec::new()) };
}

fn key(inner: &Inner) -> usize {
    std::ptr::from_ref(inner) as usize
}
fn marked(inner: &Inner, mark: Option<Mark>) -> bool {
    let key = key(inner);
    MARKS.with_borrow(|marks| {
        marks
            .iter()
            .any(|&(k, m)| k == key && mark.is_none_or(|mark| m == mark))
    })
}

/// A mark that this thread pushed. Dropping it removes the mark and, when it was the
/// last one for its catalog, runs the operations this thread deferred.
struct Marked<'a> {
    catalog: &'a Catalog,
    mark: Mark,
}
impl<'a> Marked<'a> {
    fn push(catalog: &'a Catalog, mark: Mark) -> Self {
        MARKS.with_borrow_mut(|marks| marks.push((key(&catalog.inner), mark)));
        Self { catalog, mark }
    }
}
impl Drop for Marked<'_> {
    fn drop(&mut self) {
        let key = key(&self.catalog.inner);
        let last = MARKS.with_borrow_mut(|marks| {
            if let Some(at) = marks.iter().rposition(|&(k, m)| k == key && m == self.mark) {
                marks.remove(at);
            }
            !marks.iter().any(|&(k, _)| k == key)
        });
        if last {
            run_deferred(self.catalog);
        }
    }
}

/// Holds the management lock of a catalog. The lock is released before the mark, so
/// deferred operations run without it.
pub(crate) struct ManageGuard<'a> {
    lock: Option<MutexGuard<'a, ()>>,
    _mark: Marked<'a>,
}
impl Drop for ManageGuard<'_> {
    fn drop(&mut self) {
        drop(self.lock.take());
    }
}

/// Marks the current thread as disposing a dataset of the catalog while close
/// callbacks run.
pub(crate) struct DisposalScope<'a> {
    _mark: Marked<'a>,
}
/// Postpones the operations deferred on this thread until a multi-step operation
/// returns, without refusing its own management calls.
#[cfg_attr(not(feature = "backup"), allow(dead_code))]
pub(crate) struct BatchScope<'a> {
    _mark: Marked<'a>,
}

impl Catalog {
    /// Take the management lock. A call on a thread that is disposing a dataset of
    /// this catalog, or that already holds the lock, is refused at once with a
    /// `catalog-disposing` conflict, because only the call itself could release the
    /// lock it would wait for.
    pub(crate) fn manage(&self) -> Result<ManageGuard<'_>> {
        self.check_manageable()?;
        let lock = self.inner.manage.lock.lock();
        Ok(ManageGuard {
            lock: Some(lock),
            _mark: Marked::push(self, Mark::Holding),
        })
    }
    /// Refuse, without taking the lock, a management call that [`manage`](Self::manage)
    /// would refuse. Entry points that change files or other state before they take the
    /// lock call it first, so a refusal leaves everything as it was. The answer cannot
    /// change before such a call takes the lock, because only the current thread's own
    /// scopes change it and they are balanced within the call.
    pub(crate) fn check_manageable(&self) -> Result<()> {
        // Holding covers callbacks of datasets that a management call drops, such as
        // a deleted in-memory dataset, which also run under the lock.
        if marked(&self.inner, Some(Mark::Disposing)) || marked(&self.inner, Some(Mark::Holding)) {
            return Err(Error::Conflict(
                "catalog-disposing: a dataset close callback cannot manage the catalog while \
                 this thread disposes or manages it; queue the operation with Catalog::defer"
                    .into(),
            ));
        }
        Ok(())
    }
    /// Take the management lock for an internal step that must not fail. The caller
    /// guarantees that this thread neither holds the lock nor runs a disposal, for
    /// example by calling [`check_manageable`](Self::check_manageable) first.
    pub(crate) fn manage_internal(&self) -> ManageGuard<'_> {
        debug_assert!(
            !marked(&self.inner, Some(Mark::Disposing))
                && !marked(&self.inner, Some(Mark::Holding)),
            "internal catalog management from a disposal or under the lock"
        );
        let lock = self.inner.manage.lock.lock();
        ManageGuard {
            lock: Some(lock),
            _mark: Marked::push(self, Mark::Holding),
        }
    }
    pub(crate) fn disposal_scope(&self) -> DisposalScope<'_> {
        DisposalScope {
            _mark: Marked::push(self, Mark::Disposing),
        }
    }
    #[cfg_attr(not(feature = "backup"), allow(dead_code))]
    pub(crate) fn batch_scope(&self) -> BatchScope<'_> {
        BatchScope {
            _mark: Marked::push(self, Mark::Batch),
        }
    }

    /// Run a catalog operation once the current thread's management of this catalog
    /// returns. A dataset close callback that runs while the catalog deletes, renames
    /// or restores a dataset cannot call [`create`](Self::create),
    /// [`delete`](Self::delete), [`rename`](Self::rename) or another management
    /// method, which refuse with a `catalog-disposing` conflict. It defers the call
    /// instead. The operation then runs on the same thread after the disposing call has
    /// released the management lock and before that call returns.
    ///
    /// Outside such a call, the operation runs at once. Deferred operations live only
    /// in memory, so a crash or a panic that unwinds the disposing call loses them.
    /// Callbacks must therefore be idempotent and must not rely on a deferred
    /// operation for durability. A deferred operation that panics is logged, and the
    /// operations queued after it still run. Operations that a deferred operation
    /// defers run in the same loop, so a long chain needs no extra stack.
    ///
    /// When the disposing call unwinds, its queued operations are dropped without
    /// running, each under its own panic boundary. Operations that their destructors
    /// defer are dropped as well, for at most 64 generations. Anything a destructor
    /// chain still defers after that is leaked with an error log, so a destructor that
    /// keeps deferring cannot stop the unwind from finishing.
    pub fn defer(&self, op: impl FnOnce(&Catalog) + Send + 'static) {
        if marked(&self.inner, None) {
            self.inner
                .manage
                .deferred
                .lock()
                .push((std::thread::current().id(), Box::new(op)));
        } else {
            op(self);
        }
    }
}

/// How many generations of deferred operations an unwinding drain drops, when the
/// destructors of each generation defer the next. What remains after that is leaked.
pub(crate) const UNWIND_DISCARD_GENERATIONS: usize = 64;
/// The operations that unwinding drains leaked, for tests.
#[cfg(test)]
pub(crate) static UNWIND_LEAKED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Removes the draining mark of a drain loop, also when the loop unwinds, without
/// starting another drain.
struct DrainMark(usize);
impl Drop for DrainMark {
    fn drop(&mut self) {
        MARKS.with_borrow_mut(|marks| {
            if let Some(at) = marks
                .iter()
                .rposition(|&(k, m)| k == self.0 && m == Mark::Draining)
            {
                marks.remove(at);
            }
        });
    }
}

/// Drop a caught panic payload. A payload whose destructor panics again is
/// forgotten, so the drain keeps its panic isolation.
fn drop_payload(payload: Box<dyn std::any::Any + Send>) {
    let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(payload)));
    if let Err(again) = dropped {
        tracing::error!(
            target: "sparkles::catalog",
            "the panic payload of a deferred catalog operation panicked when dropped; it is leaked"
        );
        std::mem::forget(again);
    }
}

/// Run the operations that this thread deferred on the catalog, in a loop that also
/// runs whatever they defer. During an unwind they are dropped, since running them
/// could abort the process, and so is anything their destructors defer, for at most
/// [`UNWIND_DISCARD_GENERATIONS`] generations.
fn run_deferred(catalog: &Catalog) {
    let me = std::thread::current().id();
    let key = key(&catalog.inner);
    MARKS.with_borrow_mut(|marks| marks.push((key, Mark::Draining)));
    let _draining = DrainMark(key);
    let mut discarded = 0;
    loop {
        let ops: Vec<Deferred> = {
            let mut queue = catalog.inner.manage.deferred.lock();
            let (mine, others) = std::mem::take(&mut *queue)
                .into_iter()
                .partition::<Vec<_>, _>(|(id, _)| *id == me);
            *queue = others;
            mine.into_iter().map(|(_, op)| op).collect()
        };
        if ops.is_empty() {
            return;
        }
        if std::thread::panicking() {
            discarded += 1;
            if discarded > UNWIND_DISCARD_GENERATIONS {
                tracing::error!(
                    target: "sparkles::catalog",
                    leaked = ops.len(),
                    generations = UNWIND_DISCARD_GENERATIONS,
                    "deferred catalog operations kept deferring more as they were dropped during an unwind; the rest are leaked"
                );
                #[cfg(test)]
                UNWIND_LEAKED.fetch_add(ops.len(), std::sync::atomic::Ordering::SeqCst);
                std::mem::forget(ops);
                return;
            }
            if discarded == 1 {
                tracing::error!(
                    target: "sparkles::catalog",
                    dropped = ops.len(),
                    "deferred catalog operations were dropped because the call that queued them unwound"
                );
            }
            // Drop them outside the queue lock while the draining mark is still set.
            // A destructor that defers again queues more work, which the next pass
            // discards too, so nothing stays queued to retain the catalog. Each drop
            // has its own panic boundary, so a destructor panic during the unwind
            // cannot abort the process.
            for op in ops {
                let dropped =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(op)));
                if let Err(payload) = dropped {
                    tracing::error!(
                        target: "sparkles::catalog",
                        "a discarded deferred catalog operation panicked when dropped"
                    );
                    drop_payload(payload);
                }
            }
            continue;
        }
        for op in ops {
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| op(catalog)));
            if let Err(payload) = ran {
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "non-string panic payload".into());
                tracing::error!(
                    target: "sparkles::catalog",
                    panic = %message,
                    "a deferred catalog operation panicked"
                );
                drop_payload(payload);
            }
        }
    }
}
