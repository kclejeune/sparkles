//! Write transactions. The store's write transaction holds a lock guard that must stay
//! on the thread that took it, so each Python transaction runs on its own Rust thread:
//! the Python object sends it operations over a channel and waits for each answer
//! without the GIL.

use crate::errors::{EngineResult, new_err};
use crate::terms::{
    graph_from_py, named_node_from_py, opt, quad_from_py, subject_from_py, term_from_py,
};
use oxrdf::{GraphName, NamedNode, NamedOrBlankNode, Quad, Term};
use pyo3::prelude::*;
use pyo3::types::PyList;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

/// The thread of the open transaction of a dataset, if any: a write on the dataset from
/// that thread would wait for its own lock.
pub type WriterSlot = Arc<Mutex<Option<ThreadId>>>;

enum Req {
    Insert(Vec<Quad>),
    Remove(Vec<Quad>),
    Find(
        Option<GraphName>,
        Option<NamedOrBlankNode>,
        Option<NamedNode>,
        Option<Term>,
    ),
    Commit,
    Rollback,
}

enum Resp {
    Count(u64),
    Quads(Vec<Quad>),
    Done,
    Err(sparkles::Error),
}

struct Chan {
    req: Sender<Req>,
    resp: Receiver<Resp>,
}

impl Chan {
    /// Send a request and wait for its answer (without the GIL).
    fn call(&self, req: Req) -> sparkles::Result<Resp> {
        let ended = || sparkles::Error::invalid("the transaction has ended");
        self.req.send(req).map_err(|_| ended())?;
        match self.resp.recv().map_err(|_| ended())? {
            Resp::Err(e) => Err(e),
            r => Ok(r),
        }
    }
}

/// A write transaction: `with dataset.transaction() as tx:` commits at the end of the
/// block, or rolls back if it raises.
#[pyclass(frozen, module = "sparkles", name = "Transaction")]
pub struct PyTransaction {
    chan: Mutex<Option<Chan>>,
    writer: WriterSlot,
    /// the thread that began it
    owner: ThreadId,
}

impl PyTransaction {
    /// Begin a transaction on `ds`: waits for the writer lock without the GIL.
    pub fn begin(py: Python<'_>, ds: sparkles::Dataset, writer: WriterSlot) -> PyResult<Self> {
        let (req_tx, req_rx) = channel::<Req>();
        let (resp_tx, resp_rx) = channel::<Resp>();
        std::thread::Builder::new()
            .name("sparkles-txn".into())
            .spawn(move || run(ds, req_rx, resp_tx))
            .map_err(pyo3::PyErr::from)?;
        // the first answer comes once the worker holds the writer lock
        let (resp_rx, started) = py.detach(move || {
            let r = resp_rx.recv();
            (resp_rx, r)
        });
        match started {
            Ok(Resp::Done) => {}
            Ok(Resp::Err(e)) => return Err(crate::errors::engine(py, e)),
            _ => {
                return Err(new_err(
                    py,
                    "SparklesError",
                    "the transaction did not start",
                ));
            }
        }
        let owner = std::thread::current().id();
        *writer.lock().unwrap() = Some(owner);
        Ok(PyTransaction {
            chan: Mutex::new(Some(Chan {
                req: req_tx,
                resp: resp_rx,
            })),
            writer,
            owner,
        })
    }

    fn call(&self, py: Python<'_>, req: Req) -> PyResult<Resp> {
        py.detach(|| {
            let chan = self.chan.lock().unwrap();
            match chan.as_ref() {
                Some(c) => c.call(req),
                None => Err(sparkles::Error::invalid("the transaction has ended")),
            }
        })
        .py(py)
    }

    /// End the transaction with `req` (commit or rollback).
    fn end(&self, py: Python<'_>, req: Req) -> PyResult<()> {
        let chan = self.chan.lock().unwrap().take();
        self.release();
        let Some(chan) = chan else {
            return Err(crate::errors::invalid(py, "the transaction has ended"));
        };
        py.detach(move || chan.call(req)).py(py).map(|_| ())
    }

    fn release(&self) {
        let mut w = self.writer.lock().unwrap();
        if *w == Some(self.owner) {
            *w = None;
        }
    }

    fn count(&self, py: Python<'_>, req: Req) -> PyResult<u64> {
        match self.call(py, req)? {
            Resp::Count(n) => Ok(n),
            _ => Ok(0),
        }
    }
}

impl Drop for PyTransaction {
    fn drop(&mut self) {
        // dropping the channel rolls the worker back
        if self.chan.lock().map(|c| c.is_some()).unwrap_or(false) {
            self.release();
        }
    }
}

#[pymethods]
impl PyTransaction {
    /// Add a quad (or a triple, to the default graph); true if it was new.
    fn add(&self, py: Python<'_>, quad: &Bound<'_, PyAny>) -> PyResult<bool> {
        Ok(self.count(py, Req::Insert(vec![quad_from_py(quad)?]))? > 0)
    }

    /// Remove a quad; true if it was present.
    fn remove(&self, py: Python<'_>, quad: &Bound<'_, PyAny>) -> PyResult<bool> {
        Ok(self.count(py, Req::Remove(vec![quad_from_py(quad)?]))? > 0)
    }

    /// Add quads (triples go to the default graph); returns how many were new.
    fn extend(&self, py: Python<'_>, quads: &Bound<'_, PyAny>) -> PyResult<u64> {
        let quads = quads
            .try_iter()?
            .map(|q| quad_from_py(&q?))
            .collect::<PyResult<Vec<_>>>()?;
        self.count(py, Req::Insert(quads))
    }

    /// The quads matching a pattern, with this transaction's changes.
    #[pyo3(signature = (subject = None, predicate = None, object = None, graph_name = None))]
    fn quads_for_pattern<'py>(
        &self,
        py: Python<'py>,
        subject: Option<&Bound<'py, PyAny>>,
        predicate: Option<&Bound<'py, PyAny>>,
        object: Option<&Bound<'py, PyAny>>,
        graph_name: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyList>> {
        let req = Req::Find(
            opt(graph_name, graph_from_py)?,
            opt(subject, subject_from_py)?,
            opt(predicate, named_node_from_py)?,
            opt(object, term_from_py)?,
        );
        let quads = match self.call(py, req)? {
            Resp::Quads(q) => q,
            _ => Vec::new(),
        };
        let items = quads
            .into_iter()
            .map(|q| crate::terms::quad_to_py(py, q))
            .collect::<PyResult<Vec<_>>>()?;
        PyList::new(py, items)
    }

    fn __contains__(&self, py: Python<'_>, quad: &Bound<'_, PyAny>) -> PyResult<bool> {
        let q = quad_from_py(quad)?;
        let req = Req::Find(
            Some(q.graph_name),
            Some(q.subject),
            Some(q.predicate),
            Some(q.object),
        );
        Ok(matches!(self.call(py, req)?, Resp::Quads(q) if !q.is_empty()))
    }

    fn commit(&self, py: Python<'_>) -> PyResult<()> {
        self.end(py, Req::Commit)
    }

    fn rollback(&self, py: Python<'_>) -> PyResult<()> {
        self.end(py, Req::Rollback)
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (exc_type = None, _exc_value = None, _traceback = None))]
    fn __exit__(
        &self,
        py: Python<'_>,
        exc_type: Option<&Bound<'_, PyAny>>,
        _exc_value: Option<&Bound<'_, PyAny>>,
        _traceback: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        let open = self.chan.lock().unwrap().is_some();
        if open {
            if exc_type.is_some_and(|t| !t.is_none()) {
                self.rollback(py)?;
            } else {
                self.commit(py)?;
            }
        }
        Ok(false)
    }
}

/// The worker: one write transaction, driven by the requests.
fn run(ds: sparkles::Dataset, req: Receiver<Req>, resp: Sender<Resp>) {
    let mut rolled_back = false;
    let r = ds.transaction(|tx| {
        let _ = resp.send(Resp::Done);
        loop {
            let Ok(r) = req.recv() else {
                // the Python object is gone
                rolled_back = true;
                return Err(sparkles::Error::Cancelled);
            };
            let answer = match r {
                Req::Insert(quads) => quads
                    .iter()
                    .try_fold(0u64, |n, q| Ok(n + tx.insert(q.as_ref())? as u64))
                    .map(Resp::Count),
                Req::Remove(quads) => quads
                    .iter()
                    .try_fold(0u64, |n, q| Ok(n + tx.remove(q.as_ref())? as u64))
                    .map(Resp::Count),
                Req::Find(g, s, p, o) => tx
                    .find(
                        g.as_ref().map(GraphName::as_ref),
                        s.as_ref(),
                        p.as_ref(),
                        o.as_ref(),
                    )
                    .map(Resp::Quads),
                Req::Commit => return Ok(()),
                Req::Rollback => {
                    rolled_back = true;
                    return Err(sparkles::Error::Cancelled);
                }
            };
            let _ = resp.send(answer.unwrap_or_else(Resp::Err));
        }
    });
    let last = match r {
        Ok(()) => Resp::Done,
        Err(_) if rolled_back => Resp::Done,
        Err(e) => Resp::Err(e),
    };
    let _ = resp.send(last);
}
