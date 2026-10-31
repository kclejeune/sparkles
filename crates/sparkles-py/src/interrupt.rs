//! Cancelling queries and updates from Python: `CancelToken`, and Ctrl-C. A request from
//! Python's main thread runs on a helper thread while the main thread waits for it
//! without the GIL, waking every few milliseconds to let Python run its signal handlers.
//! When a handler raises, such as `KeyboardInterrupt` on Ctrl-C, the request's
//! cancellation flag is set, the request stops at its next check, and the handler's
//! exception propagates.

use crate::errors::EngineResult;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use sparkles::embed::{WORKER_SPIN, recv_spin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often a waiting thread lets Python check for signals.
const POLL: Duration = Duration::from_millis(20);

/// How long the main thread waits for an answer by spinning, without the GIL, before it
/// sleeps. A small query answers within this, and the main thread then takes the answer
/// without being woken, which takes far longer than the spin on a busy machine. The
/// helper thread spins for its next request in the same way (`WORKER_SPIN`). The cost is
/// up to this much CPU time on the main thread per request.
const CALLER_SPIN: Duration = Duration::from_micros(200);

/// A flag that cancels the queries and updates it is passed to. `cancel()` may be
/// called from any thread, and a cancelled request raises `CancelledError`.
#[pyclass(frozen, module = "sparkles", name = "CancelToken", skip_from_py_object)]
pub struct PyCancelToken {
    pub flag: Arc<AtomicBool>,
    pub(crate) additional: Option<sparkles::task::Cancellation>,
}

#[pymethods]
impl PyCancelToken {
    #[new]
    fn new() -> Self {
        PyCancelToken {
            flag: Arc::new(AtomicBool::new(false)),
            additional: None,
        }
    }

    /// Cancel every request that uses this token, now and later.
    fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    #[getter]
    fn cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
            || self
                .additional
                .as_ref()
                .is_some_and(|sources| sources.check().is_err())
    }

    /// Internal async plumbing: combine flags without mutating a caller's token.
    fn _with_cancel(&self, other: PyRef<'_, PyCancelToken>) -> Self {
        let mut additional = sparkles::task::Cancellation::from_flags([other.flag.clone()]);
        if let Some(sources) = &self.additional {
            additional = additional.combine(sources);
        }
        if let Some(sources) = &other.additional {
            additional = additional.combine(sources);
        }
        Self {
            flag: self.flag.clone(),
            additional: Some(additional),
        }
    }

    fn __repr__(&self) -> String {
        format!("<CancelToken cancelled={}>", self.cancelled())
    }
}

/// The cancellation flag for a request: the token's, or a new one.
pub fn flag(token: Option<&Bound<'_, PyAny>>) -> PyResult<Arc<AtomicBool>> {
    match token.filter(|t| !t.is_none()) {
        None => Ok(Arc::new(AtomicBool::new(false))),
        Some(t) => Ok(t
            .cast::<PyCancelToken>()
            .map_err(|_| pyo3::exceptions::PyTypeError::new_err("cancel must be a CancelToken"))?
            .get()
            .flag
            .clone()),
    }
}

/// Additional async-owned/caller cancellation sources, absent for ordinary tokens.
pub(crate) fn additional(
    token: Option<&Bound<'_, PyAny>>,
) -> PyResult<Option<sparkles::task::Cancellation>> {
    match token.filter(|token| !token.is_none()) {
        None => Ok(None),
        Some(token) => Ok(token
            .cast::<PyCancelToken>()
            .map_err(|_| pyo3::exceptions::PyTypeError::new_err("cancel must be a CancelToken"))?
            .get()
            .additional
            .clone()),
    }
}

/// Wait for the answer on `rx` without the GIL, running Python's signal handlers in
/// between, and give `rx` back. When a handler raises, `cancel` is set and the answer
/// is still awaited, so that the request has stopped when the exception propagates.
/// `None` means the sender is gone.
pub fn wait<T: Send>(
    py: Python<'_>,
    cancel: &AtomicBool,
    rx: Receiver<T>,
) -> (Receiver<T>, PyResult<Option<T>>) {
    let mut rx = rx;
    let mut spin = CALLER_SPIN;
    loop {
        let (back, r) = py.detach(move || {
            let r = recv_spin_timeout(&rx, spin, POLL);
            (rx, r)
        });
        spin = Duration::ZERO;
        rx = back;
        match r {
            Ok(v) => return (rx, Ok(Some(v))),
            Err(RecvTimeoutError::Disconnected) => return (rx, Ok(None)),
            Err(RecvTimeoutError::Timeout) => {
                if let Err(e) = py.check_signals() {
                    cancel.store(true, Ordering::Relaxed);
                    let (back, _) = py.detach(move || {
                        let r = rx.recv();
                        (rx, r.is_ok())
                    });
                    return (back, Err(e));
                }
            }
        }
    }
}

/// Receive from `rx`, spinning for up to `spin` before sleeping for up to `timeout`.
fn recv_spin_timeout<T>(
    rx: &Receiver<T>,
    spin: Duration,
    timeout: Duration,
) -> Result<T, RecvTimeoutError> {
    let start = std::time::Instant::now();
    let mut round = 0u32;
    while !spin.is_zero() {
        match rx.try_recv() {
            Ok(v) => return Ok(v),
            Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
            Err(TryRecvError::Empty) => {}
        }
        round = round.wrapping_add(1);
        if round.is_multiple_of(64) && start.elapsed() >= spin {
            break;
        }
        std::hint::spin_loop();
    }
    rx.recv_timeout(timeout)
}

type Job = Box<dyn FnOnce() + Send>;

/// The helper thread that runs the main thread's requests, started on first use.
static HELPER: Mutex<Option<Sender<Job>>> = Mutex::new(None);

/// Run `job` on the helper thread, starting a new one if there is none or it died.
fn submit(job: Job) -> std::io::Result<()> {
    let mut helper = HELPER.lock().unwrap_or_else(|e| e.into_inner());
    let job = match helper.as_ref() {
        Some(tx) => match tx.send(job) {
            Ok(()) => return Ok(()),
            Err(e) => e.0,
        },
        None => job,
    };
    let (tx, rx) = channel::<Job>();
    std::thread::Builder::new()
        .name("sparkles-request".into())
        .spawn(move || {
            while let Ok(job) = recv_spin(&rx, WORKER_SPIN) {
                job();
            }
        })?;
    let _ = tx.send(job);
    *helper = Some(tx);
    Ok(())
}

/// Python's main thread id, and `_thread.get_ident`.
static MAIN: PyOnceLock<(u64, Py<PyAny>)> = PyOnceLock::new();

/// Whether this is Python's main thread, the only one that runs signal handlers.
fn on_main_thread(py: Python<'_>) -> PyResult<bool> {
    let (main, get_ident) = MAIN.get_or_try_init(py, || -> PyResult<_> {
        let threading = py.import("threading")?;
        let main: u64 = threading
            .call_method0("main_thread")?
            .getattr("ident")?
            .extract()?;
        Ok((main, py.import("_thread")?.getattr("get_ident")?.unbind()))
    })?;
    Ok(get_ident.bind(py).call0()?.extract::<u64>()? == *main)
}

/// Run a request without the GIL. On Python's main thread it runs on a helper thread
/// while this one waits as [`wait`] does, so that Ctrl-C cancels it. On other threads,
/// where Python runs no signal handlers, it runs in place. `f` should stop soon after
/// `cancel` is set.
pub fn run<T: Send + 'static>(
    py: Python<'_>,
    cancel: &AtomicBool,
    f: impl FnOnce() -> sparkles::Result<T> + Send + 'static,
) -> PyResult<T> {
    if !on_main_thread(py)? {
        return py.detach(f).py(py);
    }
    let (tx, rx) = channel();
    submit(Box::new(move || {
        let _ = tx.send(f());
    }))?;
    match wait(py, cancel, rx).1? {
        Some(r) => r.py(py),
        None => Err(crate::errors::new_err(
            py,
            "SparklesError",
            "the request's thread ended without an answer",
        )),
    }
}

/// The signal controls of a request that reads a Python stream, such as a load or a
/// replacement from a file object, which [`run_stream`] runs. The stream's reader calls
/// [`StreamSignals::checkpoint`] before a read when a signal is pending, and the store
/// calls it through [`StreamSignals::publish_hook`] right before it publishes the write.
pub struct StreamSignals {
    /// set when a signal handler raised: the request stops at its next check
    pub cancel: Arc<AtomicBool>,
    /// set by the signal watcher, without the GIL, when a signal arrives
    signalled: Arc<AtomicBool>,
    /// where a checkpoint asks the waiting main thread to run the signal handlers
    checks: Mutex<Option<Sender<Event>>>,
}

/// What the thread of a stream request tells the main thread that waits for it.
enum Event {
    /// run the signal handlers, then answer
    Check(Sender<()>),
    Done,
}

impl StreamSignals {
    /// Controls that cancel through `cancel`.
    pub fn new(cancel: Arc<AtomicBool>) -> Arc<StreamSignals> {
        Arc::new(StreamSignals {
            cancel,
            signalled: Arc::new(AtomicBool::new(false)),
            checks: Mutex::new(None),
        })
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Whether a signal waits for its handlers. The reader then makes a checkpoint
    /// before it calls into Python again, which would otherwise keep the GIL from the
    /// main thread in a tight loop.
    pub fn pending(&self) -> bool {
        self.signalled.load(Ordering::Acquire)
    }

    /// Wait, without the GIL, until the main thread has run the handlers of the signals
    /// that have arrived, and return whether the request is cancelled. A handler that
    /// raises cancels it. Without a waiting main thread, as on another Python thread, it
    /// only reads the flag.
    pub fn checkpoint(&self) -> bool {
        let tx = self
            .checks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(tx) = tx {
            let (done, answer) = channel();
            if tx.send(Event::Check(done)).is_ok() {
                let _ = answer.recv();
            }
        }
        self.cancelled()
    }

    /// The store's last check before it publishes the write: a checkpoint, so that a
    /// handler that raised for a signal during the request, whatever decoder read the
    /// stream last, abandons the write.
    pub fn publish_hook(self: &Arc<Self>) -> sparkles::guard::PublishHook {
        let signals = self.clone();
        sparkles::guard::PublishHook::new(move || {
            if signals.checkpoint() {
                Err(sparkles::Error::Cancelled)
            } else {
                Ok(())
            }
        })
    }
}

/// Run a request that reads a Python stream, without the GIL, on a thread of its own. On
/// Python's main thread, this thread waits for it and runs the signal handlers whenever
/// the stream's reader makes a checkpoint (see [`StreamSignals`]), and every
/// [`POLL`] otherwise. A signal also sets the pending flag at once through the watcher of
/// Python's wakeup descriptor, unless the descriptor is taken, as under asyncio. When a
/// handler raises, `cancel` is set, the request is awaited, and the handler's exception
/// propagates. When none raises, the request goes on.
///
/// The request does not run on the helper thread of [`run`], because a signal handler
/// that runs a query while the stream waits for it sends that query to the helper. The
/// request's store write takes [`StreamSignals::publish_hook`], so an exception that a
/// handler raises for a signal during the request abandons the write before it is
/// published. A signal that arrives during publication itself can raise after it.
///
/// The watcher starts before the request's thread, since its first use imports the
/// `signal` module, which would otherwise run while the thread could already raise a
/// signal and lose the handler's exception.
pub fn run_stream<T: Send + 'static>(
    py: Python<'_>,
    signals: &Arc<StreamSignals>,
    f: impl FnOnce() -> sparkles::Result<T> + Send + 'static,
) -> PyResult<T> {
    if !on_main_thread(py)? {
        return py.detach(f).py(py);
    }
    #[cfg(unix)]
    let watch = wakeup::Watch::start(py, signals.signalled.clone());
    let (tx, rx) = channel::<Event>();
    *signals.checks.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx.clone());
    let slot = Arc::new(Mutex::new(None));
    let out = slot.clone();
    let worker = std::thread::Builder::new()
        .name("sparkles-stream".into())
        .spawn(move || {
            let r = f();
            *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
            let _ = tx.send(Event::Done);
        });
    let worker = match worker {
        Ok(w) => w,
        Err(e) => {
            *signals.checks.lock().unwrap_or_else(|e| e.into_inner()) = None;
            #[cfg(unix)]
            if let Some(w) = watch {
                w.stop(py);
            }
            return Err(e.into());
        }
    };
    let mut raised: Option<PyErr> = None;
    let mut rx = rx;
    loop {
        let (back, r) = py.detach(move || {
            let r = rx.recv_timeout(POLL);
            (rx, r)
        });
        rx = back;
        let answer = match r {
            Ok(Event::Done) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Event::Check(answer)) => Some(answer),
            // a request that panicked sends nothing
            Err(RecvTimeoutError::Timeout) if worker.is_finished() => break,
            Err(RecvTimeoutError::Timeout) => None,
        };
        if let Err(e) = py.check_signals() {
            signals.cancel.store(true, Ordering::Relaxed);
            raised.get_or_insert(e);
        }
        signals.signalled.store(false, Ordering::Release);
        if let Some(answer) = answer {
            let _ = answer.send(());
        }
    }
    *signals.checks.lock().unwrap_or_else(|e| e.into_inner()) = None;
    #[cfg(unix)]
    if let Some(w) = watch {
        w.stop(py);
    }
    py.detach(move || drop(worker.join()));
    if let Some(e) = raised {
        return Err(e);
    }
    let result = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    match result {
        Some(r) => r.py(py),
        None => Err(crate::errors::new_err(
            py,
            "SparklesError",
            "the request's thread ended without an answer",
        )),
    }
}

/// Run a request that is safe to run again, such as a query, without the GIL. `f` gets
/// the cancellation flag of each attempt. With a `token`, or where the signal wakeup
/// descriptor is taken, this is [`run`] with the token's flag.
///
/// On Python's main thread the request runs in place, which saves the hand-off to the
/// helper thread. That hand-off cost 50 to 80 µs when the helper had gone to sleep,
/// which is the usual case for a request that follows other Python work. While the
/// request runs, Python's signal wakeup descriptor points at a socket that a watcher
/// thread reads. Python's C signal handler writes to it for every signal that has a
/// Python handler, and the watcher then cancels the request. The main thread runs the
/// handlers when the request returns. When a handler raises, such as
/// `KeyboardInterrupt` on Ctrl-C, its exception propagates as with [`run`]. When none
/// raises, the cancelled request runs again.
pub fn run_again<T: Send + 'static>(
    py: Python<'_>,
    token: Option<Arc<AtomicBool>>,
    f: impl Fn(Arc<AtomicBool>) -> sparkles::Result<T> + Send + Sync + 'static,
) -> PyResult<T> {
    if let Some(flag) = token {
        let cancel = flag.clone();
        return run(py, &cancel, move || f(flag));
    }
    if !on_main_thread(py)? {
        return py.detach(|| f(Arc::new(AtomicBool::new(false)))).py(py);
    }
    #[cfg(unix)]
    if let Some(r) = wakeup::run_in_place(py, &f) {
        return r;
    }
    let flag = Arc::new(AtomicBool::new(false));
    let cancel = flag.clone();
    run(py, &cancel, move || f(flag))
}

#[cfg(unix)]
mod wakeup {
    use super::*;
    use std::io::Read;
    use std::os::fd::IntoRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::OnceLock;

    /// The write end of the socket the watcher reads, once it runs.
    static SOCKET: OnceLock<Option<i32>> = OnceLock::new();
    /// The cancellation flag of the request running in place on the main thread.
    static CURRENT: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);
    /// `signal.set_wakeup_fd`.
    static SET_WAKEUP_FD: PyOnceLock<Py<PyAny>> = PyOnceLock::new();

    /// The write end of the watcher's socket, starting the watcher on first use.
    fn socket() -> Option<i32> {
        *SOCKET.get_or_init(|| {
            let (mut rx, tx) = UnixStream::pair().ok()?;
            // Python requires a non-blocking descriptor, and a full socket drops bytes
            tx.set_nonblocking(true).ok()?;
            std::thread::Builder::new()
                .name("sparkles-signals".into())
                .spawn(move || {
                    let mut buf = [0u8; 64];
                    loop {
                        match rx.read(&mut buf) {
                            Ok(0) => break,
                            Ok(_) => {
                                let current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
                                if let Some(flag) = current.as_ref() {
                                    flag.store(true, Ordering::Relaxed);
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                            Err(_) => break,
                        }
                    }
                })
                .ok()?;
            Some(tx.into_raw_fd())
        })
    }

    /// Point Python's wakeup descriptor at `fd`, returning the previous one.
    fn set_wakeup_fd(py: Python<'_>, fd: i64) -> Option<i64> {
        let set = SET_WAKEUP_FD
            .get_or_try_init(py, || -> PyResult<_> {
                Ok(py.import("signal")?.getattr("set_wakeup_fd")?.unbind())
            })
            .ok()?;
        set.bind(py).call1((fd,)).ok()?.extract().ok()
    }

    /// A flag that the watcher sets on every signal with a Python handler, for as long as
    /// the watch lasts. Only one watch or in-place request runs at a time, on the main
    /// thread.
    pub(super) struct Watch;

    impl Watch {
        /// Point the wakeup descriptor at the watcher with `flag` as the one to set, or
        /// `None` when the descriptor is taken or cannot be set.
        pub(super) fn start(py: Python<'_>, flag: Arc<AtomicBool>) -> Option<Watch> {
            let fd = i64::from(socket()?);
            let old = set_wakeup_fd(py, fd)?;
            if old != -1 {
                set_wakeup_fd(py, old);
                return None;
            }
            *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(flag);
            Some(Watch)
        }

        pub(super) fn stop(self, py: Python<'_>) {
            *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = None;
            set_wakeup_fd(py, -1);
        }
    }

    /// Run `f` in place on the main thread, or `None` when the wakeup descriptor is
    /// taken (asyncio sets one) or cannot be set.
    pub(super) fn run_in_place<T: Send>(
        py: Python<'_>,
        f: &(impl Fn(Arc<AtomicBool>) -> sparkles::Result<T> + Sync),
    ) -> Option<PyResult<T>> {
        let fd = i64::from(socket()?);
        loop {
            let old = set_wakeup_fd(py, fd)?;
            if old != -1 {
                set_wakeup_fd(py, old);
                return None;
            }
            let flag = Arc::new(AtomicBool::new(false));
            *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(flag.clone());
            let r = py.detach(|| f(flag.clone()));
            *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = None;
            set_wakeup_fd(py, -1);
            if let Err(e) = py.check_signals() {
                return Some(Err(e));
            }
            // a signal whose handlers did not raise cancelled the request: run it again
            if !(flag.load(Ordering::Relaxed) && matches!(r, Err(sparkles::Error::Cancelled))) {
                return Some(r.py(py));
            }
        }
    }
}

/// Run a controlled operation, retaining a callback exception until the worker has
/// stopped. Progress may arrive from multiple engine threads; serialize reports and
/// throttle them before acquiring the GIL.
pub fn controlled<T: Send + 'static>(
    py: Python<'_>,
    token: Option<&Bound<'_, PyAny>>,
    callback: Option<&Bound<'_, PyAny>>,
    timeout: Option<f64>,
    f: impl FnOnce(sparkles::task::Control) -> sparkles::Result<T> + Send + 'static,
) -> PyResult<T> {
    controlled_stream(py, flag(token)?, None, callback, timeout, f)
}

/// [`controlled`] with the cancellation flag `cancel`, run by [`run_stream`] when the
/// operation reads a Python stream with `signals`, whose flag must be `cancel`.
pub fn controlled_stream<T: Send + 'static>(
    py: Python<'_>,
    cancel: Arc<AtomicBool>,
    signals: Option<Arc<StreamSignals>>,
    callback: Option<&Bound<'_, PyAny>>,
    timeout: Option<f64>,
    f: impl FnOnce(sparkles::task::Control) -> sparkles::Result<T> + Send + 'static,
) -> PyResult<T> {
    use sparkles::task::{Control, Progress};
    use std::time::Instant;
    let deadline =
        timeout
            .map(|s| {
                if !s.is_finite() || s < 0.0 {
                    return Err(pyo3::exceptions::PyValueError::new_err(
                        "timeout must be finite and nonnegative",
                    ));
                }
                Instant::now()
                    .checked_add(Duration::try_from_secs_f64(s).map_err(|_| {
                        pyo3::exceptions::PyValueError::new_err("timeout is too large")
                    })?)
                    .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("timeout is too large"))
            })
            .transpose()?;
    let failure = Arc::new(Mutex::new(None::<PyErr>));
    let progress = match callback.filter(|c| !c.is_none()) {
        None => Progress::default(),
        Some(cb) => {
            if !cb.is_callable() {
                return Err(pyo3::exceptions::PyTypeError::new_err(
                    "progress must be callable",
                ));
            }
            let cb = cb.clone().unbind();
            let error = failure.clone();
            let flag = cancel.clone();
            let last = Mutex::new(None::<(Instant, f32)>);
            Progress::new(move |fraction, message| {
                let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
                if error.lock().unwrap().is_some() {
                    return;
                }
                let now = Instant::now();
                let fraction = fraction.max(last.map_or(0.0, |(_, f)| f));
                if last.is_some_and(|(_, f)| f >= 1.0)
                    || (fraction < 1.0
                        && last.is_some_and(|(t, _)| {
                            now.duration_since(t) < Duration::from_millis(100)
                        }))
                {
                    return;
                }
                *last = Some((now, fraction));
                Python::attach(|py| {
                    if let Err(e) = cb.bind(py).call1((fraction, message)) {
                        flag.store(true, Ordering::Relaxed);
                        *error.lock().unwrap() = Some(e);
                    }
                });
            })
        }
    };
    let ctl = Control {
        progress,
        deadline,
        ..Control::with_cancel(cancel.clone())
    };
    let job = move || {
        ctl.check()?;
        let result = f(ctl.clone())?;
        ctl.check()?;
        ctl.progress.report(1.0, "completed");
        ctl.check()?;
        Ok(result)
    };
    let result = match &signals {
        Some(signals) => run_stream(py, signals, job),
        None => run(py, &cancel, job),
    };
    let error = failure.lock().unwrap().take();
    match error {
        Some(e) => Err(e),
        None => result,
    }
}
