//! Engine errors as Python exceptions. The classes live in `sparkles/_errors.py`, where
//! each can have a built-in exception as a second base.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyModule, PyType};

static ERRORS: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

/// An exception of class `class` (a name in `sparkles._errors`) with `msg`.
pub fn new_err(py: Python<'_>, class: &str, msg: impl Into<String>) -> PyErr {
    let msg = msg.into();
    let module = ERRORS.get_or_try_init(py, || py.import("sparkles._errors").map(Bound::unbind));
    let ty = module.and_then(|m| {
        m.bind(py)
            .getattr(class)?
            .cast_into::<PyType>()
            .map_err(PyErr::from)
    });
    match ty {
        Ok(ty) => PyErr::from_type(ty, (msg,)),
        Err(e) => PyRuntimeError::new_err(format!("{msg} (and sparkles._errors failed: {e})")),
    }
}

/// `sparkles::Error` as the exception of §5 of the spec.
pub fn engine(py: Python<'_>, e: sparkles::Error) -> PyErr {
    use sparkles::Error as E;
    let msg = e.to_string();
    let class = match e {
        E::Io(io) => return PyErr::from(io),
        E::SparqlSyntax(_) => "SparqlSyntaxError",
        E::RdfParse(_) => "RdfSyntaxError",
        E::Timeout => "QueryTimeoutError",
        E::Cancelled => "CancelledError",
        E::BudgetExceeded(b) => {
            let err = new_err(py, "BudgetExceededError", msg);
            let v = err.value(py);
            let _ = v.setattr("kind", b.kind.as_str());
            let _ = v.setattr("limit", b.limit);
            let _ = v.setattr("requested", b.requested);
            return err;
        }
        E::Unsupported(_) => "UnsupportedError",
        // the store's lock on a database directory (`lock_dir` in store.rs)
        E::Invalid(ref m) if m.contains("is in use by another process") => "DatasetLockedError",
        E::Invalid(_) => "InvalidInputError",
        E::Corrupt(_) | E::Poisoned | E::StorageFull(_) => "StorageError",
        E::Service(_) => "ServiceError",
        E::NotFound(_) => "NotFoundError",
        E::Conflict(_) | E::PreconditionFailed(_) | E::WriterBusy => "ConflictError",
        E::Rejected(_) | E::GuardMissing(_) => "WriteRejectedError",
        E::NotPermitted(_) => "PermissionDeniedError",
        _ => "SparklesError",
    };
    new_err(py, class, msg)
}

/// An error of the reasoner or a validator: a `sparkles::Error` inside keeps its class.
#[cfg(any(feature = "reasoning", feature = "shacl", feature = "shex"))]
pub fn anyhow(py: Python<'_>, e: anyhow::Error) -> PyErr {
    match e.downcast::<sparkles::Error>() {
        Ok(e) => engine(py, e),
        Err(e) => new_err(py, "SparklesError", format!("{e:#}")),
    }
}

/// A method of a feature this build left out.
#[cfg(not(all(feature = "reasoning", feature = "shacl", feature = "shex")))]
pub fn missing_feature(py: Python<'_>, feature: &str) -> PyErr {
    new_err(
        py,
        "UnsupportedError",
        format!("this build of sparkles has no `{feature}` feature"),
    )
}

pub fn invalid(py: Python<'_>, msg: impl Into<String>) -> PyErr {
    new_err(py, "InvalidInputError", msg)
}

#[cfg(any(feature = "reasoning", feature = "shacl", feature = "shex"))]
pub fn syntax(py: Python<'_>, msg: impl Into<String>) -> PyErr {
    new_err(py, "RdfSyntaxError", msg)
}

/// Extension trait: `result.py(py)?` maps an engine error.
pub trait EngineResult<T> {
    fn py(self, py: Python<'_>) -> PyResult<T>;
}

impl<T> EngineResult<T> for sparkles::Result<T> {
    fn py(self, py: Python<'_>) -> PyResult<T> {
        self.map_err(|e| engine(py, e))
    }
}
