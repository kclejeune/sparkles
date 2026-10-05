//! Cancelling queries and updates from Python: `CancelToken`, and Ctrl-C. A request from
//! Python's main thread runs on a helper thread while the main thread waits for it
//! without the GIL, waking every few milliseconds to let Python run its signal handlers.
//! When a handler raises, such as `KeyboardInterrupt` on Ctrl-C, the request's
//! cancellation flag is set, the request stops at its next check, and the handler's
//! exception propagates.

use crate::errors::EngineResult;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often a waiting thread lets Python check for signals.
const POLL: Duration = Duration::from_millis(20);

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
    loop {
        let (back, r) = py.detach(move || {
            let r = rx.recv_timeout(POLL);
            (rx, r)
        });
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
            while let Ok(job) = rx.recv() {
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
