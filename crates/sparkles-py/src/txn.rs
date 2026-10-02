//! Write transactions. The store's write transaction holds a lock guard that must stay
//! on the thread that took it, so each Python transaction runs on its own Rust thread:
//! the Python object sends it operations over a channel and waits for each answer
//! without the GIL.

use crate::dataset::{QueryArgs, query_args, query_options, result_to_py};
use crate::errors::{EngineResult, new_err};
use crate::interrupt;
use crate::results::PyUpdateStats;
use crate::terms::{
    graph_from_py, named_node_from_py, opt, quad_from_py, subject_from_py, term_from_py,
};
use oxrdf::{GraphName, NamedNode, NamedOrBlankNode, Quad, Term};
use pyo3::prelude::*;
use pyo3::types::{PyList, PyTuple};
use sparkles::sparql::update::UpdateStats;
use sparkles::sparql::{QueryKind, QueryOptions, QueryResult};
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::Duration;

/// The thread of the open transaction of a dataset, if any: a write on the dataset from
/// that thread would wait for its own lock.
pub type WriterSlot = Arc<Mutex<Option<ThreadId>>>;

enum Req {
    Insert(Vec<Quad>),
    Remove(Vec<Quad>),
    Apply(Vec<(bool, Quad)>),
    Find(
        Option<GraphName>,
        Option<NamedOrBlankNode>,
        Option<NamedNode>,
        Option<Term>,
    ),
    Query(String, Box<QueryOptions>),
    Update(String, Box<QueryOptions>),
    Commit,
    Rollback,
}

enum Resp {
    Count(u64),
    Applied(u64, u64, BTreeMap<String, String>),
    Quads(Vec<Quad>),
    Query(Box<QueryResult>),
    Update(Box<UpdateStats>),
    Done,
    Err(sparkles::Error),
}

struct Chan {
    req: Sender<Req>,
    resp: Receiver<Resp>,
}

/// The channel to the worker: open, lent to a request in progress, or gone.
enum Slot {
    Open(Chan),
    Busy,
    Ended,
}

/// A write transaction: `with dataset.transaction() as tx:` commits at the end of the
/// block, or rolls back if it raises.
#[pyclass(frozen, module = "sparkles", name = "Transaction")]
pub struct PyTransaction {
    slot: Mutex<Slot>,
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
            slot: Mutex::new(Slot::Open(Chan {
                req: req_tx,
                resp: resp_rx,
            })),
            writer,
            owner,
        })
    }

    /// Take the channel for one request, waiting without the GIL while another thread
    /// uses it.
    fn take(&self, py: Python<'_>) -> PyResult<Chan> {
        py.detach(|| {
            loop {
                let mut slot = self.slot.lock().unwrap();
                match std::mem::replace(&mut *slot, Slot::Busy) {
                    Slot::Open(c) => return Ok(c),
                    Slot::Busy => {}
                    Slot::Ended => {
                        *slot = Slot::Ended;
                        return Err(sparkles::Error::invalid("the transaction has ended"));
                    }
                }
                drop(slot);
                std::thread::sleep(Duration::from_millis(1));
            }
        })
        .py(py)
    }

    fn put_back(&self, chan: Chan) {
        *self.slot.lock().unwrap() = Slot::Open(chan);
    }

    /// Send a request and wait for its answer. With `cancel`, Python's signal handlers
    /// run while it waits, and one that raises cancels the request.
    fn call(&self, py: Python<'_>, req: Req, cancel: Option<&AtomicBool>) -> PyResult<Resp> {
        let chan = self.take(py)?;
        let ended = || sparkles::Error::invalid("the transaction has ended");
        if chan.req.send(req).is_err() {
            *self.slot.lock().unwrap() = Slot::Ended;
            return Err(ended()).py(py);
        }
        let (resp, answer) = match cancel {
            Some(flag) => interrupt::wait(py, flag, chan.resp),
            None => {
                let rx = chan.resp;
                py.detach(move || {
                    let r = rx.recv().ok();
                    (rx, Ok(r))
                })
            }
        };
        self.put_back(Chan {
            req: chan.req,
            resp,
        });
        match answer? {
            Some(Resp::Err(e)) => Err(crate::errors::engine(py, e)),
            Some(r) => Ok(r),
            None => {
                *self.slot.lock().unwrap() = Slot::Ended;
                Err(ended()).py(py)
            }
        }
    }

    /// End the transaction with `req` (commit or rollback).
    fn end(&self, py: Python<'_>, req: Req) -> PyResult<()> {
        let chan = self.take(py);
        *self.slot.lock().unwrap() = Slot::Ended;
        self.release();
        let chan = chan?;
        py.detach(move || {
            chan.req
                .send(req)
                .map_err(|_| sparkles::Error::invalid("the transaction has ended"))?;
            match chan.resp.recv() {
                Ok(Resp::Err(e)) => Err(e),
                Ok(_) => Ok(()),
                Err(_) => Err(sparkles::Error::invalid("the transaction has ended")),
            }
        })
        .py(py)
    }

    fn release(&self) {
        let mut w = self.writer.lock().unwrap();
        if *w == Some(self.owner) {
            *w = None;
        }
    }

    fn count(&self, py: Python<'_>, req: Req) -> PyResult<u64> {
        match self.call(py, req, None)? {
            Resp::Count(n) => Ok(n),
            _ => Ok(0),
        }
    }

    fn is_open(&self) -> bool {
        matches!(*self.slot.lock().unwrap(), Slot::Open(_) | Slot::Busy)
    }

    fn run_query<'py>(
        &self,
        py: Python<'py>,
        query: &str,
        args: QueryArgs<'py>,
        want: Option<&[QueryKind]>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (opts, at) = query_options(&args)?;
        if at.is_some() {
            return Err(crate::errors::invalid(
                py,
                "a query in a transaction reads the transaction's state: `at` is for queries on the dataset",
            ));
        }
        let flag = opts.cancel.clone().unwrap_or_default();
        match self.call(
            py,
            Req::Query(query.to_string(), Box::new(opts)),
            Some(&flag),
        )? {
            Resp::Query(r) => result_to_py(py, *r, want),
            _ => Err(new_err(py, "SparklesError", "unexpected answer")),
        }
    }
}

impl Drop for PyTransaction {
    fn drop(&mut self) {
        // dropping the channel rolls the worker back
        if self.is_open() {
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

    /// `Dataset._apply` in this transaction. A failure leaves the transaction to be
    /// rolled back.
    fn _apply(
        &self,
        py: Python<'_>,
        ops: &Bound<'_, PyAny>,
    ) -> PyResult<(u64, u64, BTreeMap<String, String>)> {
        match self.call(py, Req::Apply(ops_from_py(ops)?), None)? {
            Resp::Applied(i, d, labels) => Ok((i, d, labels)),
            _ => Err(new_err(py, "SparklesError", "unexpected answer")),
        }
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
        let quads = match self.call(py, req, None)? {
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
        Ok(matches!(self.call(py, req, None)?, Resp::Quads(q) if !q.is_empty()))
    }

    /// Run a SPARQL query that sees this transaction's changes: `QuerySolutions` for
    /// SELECT, `bool` for ASK, `QueryTriples` for CONSTRUCT and DESCRIBE.
    #[pyo3(signature = (query, *, base_iri = None, prefixes = None, bindings = None, default_graph = None, named_graphs = None, include_inferred = false, timeout = None, max_rows = None, max_memory_bytes = None, max_rows_produced = None, cancel = None))]
    #[allow(clippy::too_many_arguments)]
    fn query<'py>(
        &self,
        py: Python<'py>,
        query: &str,
        base_iri: Option<String>,
        prefixes: Option<BTreeMap<String, String>>,
        bindings: Option<Bound<'py, PyAny>>,
        default_graph: Option<Bound<'py, PyAny>>,
        named_graphs: Option<Bound<'py, PyAny>>,
        include_inferred: bool,
        timeout: Option<f64>,
        max_rows: Option<usize>,
        max_memory_bytes: Option<u64>,
        max_rows_produced: Option<u64>,
        cancel: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let args = query_args(
            base_iri,
            prefixes,
            bindings,
            default_graph,
            named_graphs,
            include_inferred,
            timeout,
            max_rows,
            max_memory_bytes,
            max_rows_produced,
            cancel,
            None,
        );
        self.run_query(py, query, args, None)
    }

    /// Run a SPARQL Update request in this transaction. Its changes commit or roll back
    /// with the transaction. An update that fails after it began to change data leaves
    /// the transaction to be rolled back.
    #[pyo3(signature = (update, *, base_iri = None, prefixes = None, timeout = None, max_rows = None, max_memory_bytes = None, max_rows_produced = None, cancel = None))]
    #[allow(clippy::too_many_arguments)]
    fn update(
        &self,
        py: Python<'_>,
        update: &str,
        base_iri: Option<String>,
        prefixes: Option<BTreeMap<String, String>>,
        timeout: Option<f64>,
        max_rows: Option<usize>,
        max_memory_bytes: Option<u64>,
        max_rows_produced: Option<u64>,
        cancel: Option<Bound<'_, PyAny>>,
    ) -> PyResult<PyUpdateStats> {
        let args = QueryArgs {
            base_iri,
            prefixes,
            timeout,
            max_rows,
            max_memory_bytes,
            max_rows_produced,
            cancel,
            ..Default::default()
        };
        let (opts, _) = query_options(&args)?;
        let flag = opts.cancel.clone().unwrap_or_default();
        match self.call(
            py,
            Req::Update(update.to_string(), Box::new(opts)),
            Some(&flag),
        )? {
            Resp::Update(s) => Ok(PyUpdateStats::from(*s)),
            _ => Err(new_err(py, "SparklesError", "unexpected answer")),
        }
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
        if self.is_open() {
            if exc_type.is_some_and(|t| !t.is_none()) {
                self.rollback(py)?;
            } else {
                self.commit(py)?;
            }
        }
        Ok(false)
    }
}

/// `(insert, quad)` pairs from Python.
pub fn ops_from_py(ops: &Bound<'_, PyAny>) -> PyResult<Vec<(bool, Quad)>> {
    ops.try_iter()?
        .map(|op| {
            let op = op?;
            let op = op.cast::<PyTuple>()?;
            Ok((
                op.get_item(0)?.is_truthy()?,
                quad_from_py(&op.get_item(1)?)?,
            ))
        })
        .collect()
}

/// Apply `(insert, quad)` operations in order. Returns the quads inserted and removed,
/// and the stored label of each blank node label that an insert gave a new node.
pub fn apply_ops(
    tx: &mut sparkles::Transaction<'_>,
    ops: &[(bool, Quad)],
) -> sparkles::Result<(u64, u64, BTreeMap<String, String>)> {
    let (mut ins, mut del) = (0, 0);
    let mut labels = BTreeMap::new();
    for (insert, q) in ops {
        if *insert {
            ins += tx.insert_linked(q.as_ref())? as u64;
            for_each_label(q, &mut |l| {
                if let Some(b) = tx.blank_node(l)
                    && b.as_str() != l
                {
                    labels.insert(l.to_string(), b.into_string());
                }
            });
        } else {
            del += tx.remove(q.as_ref())? as u64;
        }
    }
    Ok((ins, del, labels))
}

/// Call `f` with each blank node label of a quad, inside triple terms too.
fn for_each_label(q: &Quad, f: &mut impl FnMut(&str)) {
    fn term(t: &Term, f: &mut impl FnMut(&str)) {
        match t {
            Term::BlankNode(b) => f(b.as_str()),
            Term::Triple(tr) => {
                if let NamedOrBlankNode::BlankNode(b) = &tr.subject {
                    f(b.as_str());
                }
                term(&tr.object, f);
            }
            _ => {}
        }
    }
    if let NamedOrBlankNode::BlankNode(b) = &q.subject {
        f(b.as_str());
    }
    term(&q.object, f);
    if let GraphName::BlankNode(b) = &q.graph_name {
        f(b.as_str());
    }
}

/// The worker: one write transaction, driven by the requests.
fn run(ds: sparkles::Dataset, req: Receiver<Req>, resp: Sender<Resp>) {
    let mut rolled_back = false;
    let r = ds.transaction(|tx| {
        let _ = resp.send(Resp::Done);
        // set by a failure that may have left part of its changes behind
        let mut aborted: Option<String> = None;
        loop {
            let Ok(r) = req.recv() else {
                // the Python object is gone
                rolled_back = true;
                return Err(sparkles::Error::Cancelled);
            };
            if let Some(why) = &aborted
                && !matches!(r, Req::Rollback)
            {
                if matches!(r, Req::Commit) {
                    // the caller sees the error; nothing is committed
                    return Err(sparkles::Error::invalid(format!(
                        "the transaction was aborted by an earlier failure and has been rolled back: {why}"
                    )));
                }
                let _ = resp.send(Resp::Err(sparkles::Error::invalid(format!(
                    "the transaction was aborted by an earlier failure: roll it back ({why})"
                ))));
                continue;
            }
            let answer = match r {
                Req::Insert(quads) => quads
                    .iter()
                    .try_fold(0u64, |n, q| Ok(n + tx.insert_linked(q.as_ref())? as u64))
                    .map(Resp::Count),
                Req::Remove(quads) => quads
                    .iter()
                    .try_fold(0u64, |n, q| Ok(n + tx.remove(q.as_ref())? as u64))
                    .map(Resp::Count),
                Req::Apply(ops) => {
                    let r = apply_ops(tx, &ops);
                    if let Err(e) = &r {
                        aborted = Some(e.to_string());
                    }
                    r.map(|(i, d, l)| Resp::Applied(i, d, l))
                }
                Req::Find(g, s, p, o) => tx
                    .find(
                        g.as_ref().map(GraphName::as_ref),
                        s.as_ref(),
                        p.as_ref(),
                        o.as_ref(),
                    )
                    .map(Resp::Quads),
                Req::Query(q, opts) => tx.query_with(&q, &opts).map(|r| Resp::Query(Box::new(r))),
                Req::Update(u, opts) => {
                    let r = tx.update_with(&u, &opts);
                    // a syntax error changes nothing; any other failure may have
                    // stopped between operations
                    if let Err(e) = &r
                        && !matches!(e, sparkles::Error::SparqlSyntax(_))
                    {
                        aborted = Some(e.to_string());
                    }
                    r.map(|s| Resp::Update(Box::new(s)))
                }
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
