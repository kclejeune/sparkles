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
}

#[pymethods]
impl PyCancelToken {
    #[new]
    fn new() -> Self {
        PyCancelToken {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Cancel every request that uses this token, now and later.
    fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    #[getter]
    fn cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
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
    use sparkles::task::{Control, Progress};
    use std::time::Instant;
    let cancel = flag(token)?;
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
    let result = run(py, &cancel, move || {
        ctl.check()?;
        let result = f(ctl.clone())?;
        ctl.check()?;
        ctl.progress.report(1.0, "completed");
        ctl.check()?;
        Ok(result)
    });
    let error = failure.lock().unwrap().take();
    match error {
        Some(e) => Err(e),
        None => result,
    }
}
