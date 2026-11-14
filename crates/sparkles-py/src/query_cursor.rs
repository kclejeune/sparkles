//! Fallible SELECT iteration over bounded engine batches.
use crate::admin;
use crate::errors::{EngineResult, invalid};
use crate::interrupt;
use crate::io::{Output, output_from_py, write_to};
use crate::results::PyQuerySolution;
use crate::terms::PyVariable;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyTuple};
use sparkles::sparql::results::{SolutionsFormat, write_cursor_solutions};
use sparkles::sparql::{
    CursorStats, CursorStatus, GraphBatch, GraphCursor, QueryBatch, QueryCursor,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError, atomic::AtomicBool};

/// Rows that one fetch or refill decodes: the row asked for and the ones after it in the
/// batch, so that iteration leaves the caller's thread once per so many rows.
const AHEAD_ROWS: usize = 256;
/// The bytes of term text at which a refill stops decoding ahead. The rows decoded ahead
/// are held outside the query's memory budget until they are handed out, so this bounds
/// what a cursor holds there at about this much and one row.
const AHEAD_BYTES: usize = 1 << 20;

/// About how many bytes of text a term holds, for [`AHEAD_BYTES`].
fn term_bytes(term: &oxrdf::Term) -> usize {
    64 + match term {
        oxrdf::Term::NamedNode(n) => n.as_str().len(),
        oxrdf::Term::BlankNode(b) => b.as_str().len(),
        oxrdf::Term::Literal(l) => l.value().len(),
        oxrdf::Term::Triple(_) => 256,
    }
}

struct State {
    cursor: Option<QueryCursor>,
    batch: Option<QueryBatch>,
    /// the next row of `batch` to decode
    row: usize,
    /// rows of `batch` before `row` that are decoded and not yet handed out
    ahead: VecDeque<Vec<Option<oxrdf::Term>>>,
    emitted: u64,
    stats: CursorStats,
    plan: String,
    serializing: bool,
}

impl State {
    fn status(&self) -> CursorStatus {
        let status = self
            .cursor
            .as_ref()
            .map_or(self.stats.status, QueryCursor::status);
        if status == CursorStatus::Complete && self.pending() {
            CursorStatus::Open
        } else {
            status
        }
    }

    /// Whether rows that were fetched have not been handed out.
    fn pending(&self) -> bool {
        !self.ahead.is_empty() || self.batch.as_ref().is_some_and(|b| self.row < b.len())
    }

    /// Whether the next row to decode is in the batch already fetched, so that reading it
    /// runs no part of the query.
    fn buffered(&self) -> bool {
        !self.serializing
            && self.cursor.is_some()
            && self.batch.as_ref().is_some_and(|b| self.row < b.len())
    }

    /// The next row, with up to `rows - 1` of the rows after it in the batch decoded
    /// into `ahead`. Decoding ahead stops at [`AHEAD_BYTES`] of text and at a row that
    /// fails. That row is decoded again when it is asked for, so its error is raised and
    /// recorded for the row it belongs to, after the rows before it were handed out.
    fn refill(&mut self, rows: usize) -> sparkles::Result<Option<Vec<Option<oxrdf::Term>>>> {
        let Some(first) = self.next_row()? else {
            return Ok(None);
        };
        let mut bytes = 0;
        while self.ahead.len() + 1 < rows && bytes < AHEAD_BYTES && self.buffered() {
            let Some(Ok(row)) = self.batch.as_ref().map(|batch| batch.row(self.row)) else {
                break;
            };
            bytes += row.iter().flatten().map(term_bytes).sum::<usize>();
            self.row += 1;
            self.ahead.push_back(row);
        }
        Ok(Some(first))
    }

    /// The next row, from the batch already fetched or from the cursor's next batch.
    fn next_row(&mut self) -> sparkles::Result<Option<Vec<Option<oxrdf::Term>>>> {
        if self.serializing {
            return Err(sparkles::Error::invalid("the cursor is being serialized"));
        }
        let Some(mut cursor) = self.cursor.take() else {
            if self.stats.status == CursorStatus::Failed {
                return Err(failed(&self.stats));
            }
            return Ok(None);
        };
        let mut fetched = false;
        let result: sparkles::Result<Option<Vec<Option<oxrdf::Term>>>> = (|| {
            if self
                .batch
                .as_ref()
                .is_none_or(|batch| self.row == batch.len())
            {
                self.batch = None;
                self.row = 0;
                fetched = true;
                self.batch = cursor.next_batch()?;
            }
            let Some(batch) = &self.batch else {
                return Ok(None);
            };
            let values = batch.row(self.row)?;
            self.row += 1;
            self.emitted += 1;
            Ok(Some(values))
        })();
        // Record the outcome before anything else can fail, and keep the
        // execution error as the one raised.
        if let Err(error) = &result {
            cursor.close();
            self.batch = None;
            self.stats = cursor.stats();
            self.stats.status = CursorStatus::Failed;
            self.stats.error = Some(error.to_string());
            if let Ok(plan) = cursor.plan_json() {
                self.plan = plan;
            }
        } else {
            // A row of the batch already fetched leaves the cursor as it was, so the
            // statistics recorded with that batch still hold.
            if fetched {
                let finished = cursor.status() != CursorStatus::Open
                    && self.stats.status == CursorStatus::Open;
                self.stats = cursor.stats();
                if finished && let Ok(plan) = cursor.plan_json() {
                    self.plan = plan;
                }
            }
            self.cursor = Some(cursor);
        }
        result
    }
}

/// The engine cursor operations that both Python wrappers share.
trait EngineCursor: Send + 'static {
    fn stats(&self) -> CursorStats;
    fn plan_json(&self) -> sparkles::Result<String>;
    fn close(&mut self);
}

impl EngineCursor for QueryCursor {
    fn stats(&self) -> CursorStats {
        QueryCursor::stats(self)
    }
    fn plan_json(&self) -> sparkles::Result<String> {
        QueryCursor::plan_json(self)
    }
    fn close(&mut self) {
        QueryCursor::close(self)
    }
}

impl EngineCursor for GraphCursor {
    fn stats(&self) -> CursorStats {
        GraphCursor::stats(self)
    }
    fn plan_json(&self) -> sparkles::Result<String> {
        GraphCursor::plan_json(self)
    }
    fn close(&mut self) {
        GraphCursor::close(self)
    }
}

/// Wrapper state that records the outcome of a serialization.
trait Slot: Send + 'static {
    type Cursor: EngineCursor;
    fn finish_serialize(&mut self, stats: CursorStats, plan: Option<String>);
}

impl Slot for State {
    type Cursor = QueryCursor;
    fn finish_serialize(&mut self, stats: CursorStats, plan: Option<String>) {
        self.serializing = false;
        self.emitted = stats.emitted_rows;
        self.stats = stats;
        if let Some(plan) = plan {
            self.plan = plan;
        }
    }
}

impl Slot for GraphState {
    type Cursor = GraphCursor;
    fn finish_serialize(&mut self, stats: CursorStats, plan: Option<String>) {
        self.serializing = false;
        self.emitted = stats.emitted_rows;
        self.stats = stats;
        if let Some(plan) = plan {
            self.plan = plan;
        }
    }
}

/// Owns a cursor taken for serialization and settles the wrapper state when dropped,
/// on every path: success, an output or engine error, a panic, or a job that never
/// ran. A serialization that did not finish leaves the cursor Failed.
struct Serializing<S: Slot> {
    state: Arc<Mutex<S>>,
    cursor: Option<S::Cursor>,
    error: Option<String>,
}

impl<S: Slot> Serializing<S> {
    fn new(state: Arc<Mutex<S>>, cursor: S::Cursor) -> Self {
        Self {
            state,
            cursor: Some(cursor),
            error: Some("serialization stopped before it finished".into()),
        }
    }

    /// Write the cursor's output to `out`, recording whether it finished.
    fn run(
        mut self,
        out: Output,
        write: impl FnOnce(&mut S::Cursor, &mut dyn std::io::Write) -> sparkles::Result<u64>,
    ) -> sparkles::Result<Option<Vec<u8>>> {
        let cursor = self
            .cursor
            .as_mut()
            .expect("cursor taken for serialization");
        let result = write_to(out, None, |w| write(cursor, w));
        self.error = result.as_ref().err().map(ToString::to_string);
        result
    }
}

impl<S: Slot> Drop for Serializing<S> {
    fn drop(&mut self) {
        let Some(mut cursor) = self.cursor.take() else {
            return;
        };
        cursor.close();
        let mut stats = cursor.stats();
        if let Some(error) = self.error.take() {
            stats.status = CursorStatus::Failed;
            stats.error.get_or_insert(error);
        }
        let plan = cursor.plan_json().ok();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.finish_serialize(stats, plan);
    }
}

/// The error raised when iteration continues after a failure, so that a failed
/// cursor never looks like a complete one.
fn failed(stats: &CursorStats) -> sparkles::Error {
    sparkles::Error::invalid(format!(
        "the cursor failed: {}",
        stats.error.as_deref().unwrap_or("unknown error")
    ))
}

#[pyclass(module = "sparkles", name = "QueryCursor")]
pub struct PyQueryCursor {
    vars: Arc<[String]>,
    state: Arc<Mutex<State>>,
    cancel: Arc<AtomicBool>,
}

impl PyQueryCursor {
    pub(crate) fn new(cursor: QueryCursor, cancel: Arc<AtomicBool>) -> sparkles::Result<Self> {
        let vars = cursor.variables().to_vec().into();
        let stats = cursor.stats();
        let plan = cursor.plan_json()?;
        Ok(Self {
            vars,
            state: Arc::new(Mutex::new(State {
                cursor: Some(cursor),
                batch: None,
                row: 0,
                ahead: VecDeque::new(),
                emitted: 0,
                stats,
                plan,
                serializing: false,
            })),
            cancel,
        })
    }
}

#[pymethods]
impl PyQueryCursor {
    #[getter]
    fn variables(&self) -> Vec<PyVariable> {
        self.vars
            .iter()
            .map(|name| PyVariable { name: name.clone() })
            .collect()
    }

    #[getter]
    fn status(&self, py: Python<'_>) -> PyResult<String> {
        py.detach(|| {
            let state = self
                .state
                .try_lock()
                .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?;
            if state.serializing {
                return Err(sparkles::Error::invalid("the cursor is being serialized"));
            }
            Ok(state.status().as_str().into())
        })
        .py(py)
    }

    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stats = py
            .detach(|| {
                let state = self
                    .state
                    .try_lock()
                    .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?;
                let mut stats = state
                    .cursor
                    .as_ref()
                    .map_or_else(|| state.stats.clone(), QueryCursor::stats);
                if !state.serializing {
                    stats.status = state.status();
                    stats.emitted_rows = state.emitted;
                }
                Ok(stats)
            })
            .py(py)?;
        admin::to_py(py, &stats)
    }

    fn plan<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let plan = py
            .detach(|| {
                let state = self
                    .state
                    .try_lock()
                    .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?;
                state
                    .cursor
                    .as_ref()
                    .map_or_else(|| Ok(state.plan.clone()), QueryCursor::plan_json)
            })
            .py(py)?;
        py.import("json")?.call_method1("loads", (plan,))
    }

    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Option<PyQuerySolution>> {
        // A row that an earlier call decoded is handed out here, which takes no work. A
        // fetch or a refill runs detached, where Ctrl-C can cancel it, and decodes the
        // rows that follow in its batch, so iteration leaves this thread once per
        // `AHEAD_ROWS` rows and decodes nothing with the GIL held.
        let ready = self.state.try_lock().ok().and_then(|mut state| {
            let row = state.ahead.pop_front()?;
            state.emitted += 1;
            Some(row)
        });
        let values = match ready {
            Some(row) => Some(row),
            None => {
                let state = self.state.clone();
                interrupt::run(py, &self.cancel, move || {
                    state
                        .try_lock()
                        .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?
                        .refill(AHEAD_ROWS)
                })?
            }
        };
        Ok(values.map(|values| PyQuerySolution::new(self.vars.clone(), values)))
    }

    /// Internal async bridge: deliver one remaining native batch, without prefetch. The
    /// whole batch is decoded in one detached call, so the event loop's thread can run
    /// while it is.
    fn _next_batch(&self, py: Python<'_>) -> PyResult<Option<Vec<Py<PyQuerySolution>>>> {
        let state = self.state.clone();
        let rows = interrupt::run(py, &self.cancel, move || {
            let mut state = state
                .try_lock()
                .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?;
            // rows that `__next__` decoded ahead come first
            let mut rows: Vec<_> = state.ahead.drain(..).collect();
            state.emitted += rows.len() as u64;
            if rows.is_empty() {
                let Some(first) = state.next_row()? else {
                    return Ok(None);
                };
                rows.push(first);
            }
            while state.buffered() {
                match state.next_row()? {
                    Some(row) => rows.push(row),
                    None => break,
                }
            }
            Ok(Some(rows))
        })?;
        rows.map(|rows| {
            rows.into_iter()
                .map(|values| Py::new(py, PyQuerySolution::new(self.vars.clone(), values)))
                .collect()
        })
        .transpose()
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let ok = py
            .detach(|| -> sparkles::Result<bool> {
                let Ok(mut state) = self.state.try_lock() else {
                    return Ok(false);
                };
                if state.serializing {
                    return Ok(false);
                }
                let pending = state.pending();
                state.ahead.clear();
                if let Some(mut cursor) = state.cursor.take() {
                    cursor.close();
                    state.stats = cursor.stats();
                    if let Ok(plan) = cursor.plan_json() {
                        state.plan = plan;
                    }
                    if pending && state.stats.status == CursorStatus::Complete {
                        state.stats.status = CursorStatus::Stopped;
                    }
                }
                state.batch = None;
                Ok(true)
            })
            .py(py)?;
        if ok {
            Ok(())
        } else {
            Err(invalid(
                py,
                "wait for serialization to finish before closing the cursor",
            ))
        }
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (*_args))]
    fn __exit__(&self, py: Python<'_>, _args: &Bound<'_, PyTuple>) -> PyResult<bool> {
        self.close(py)?;
        Ok(false)
    }

    #[pyo3(signature = (output = None, format = "json"))]
    fn serialize<'py>(
        &self,
        py: Python<'py>,
        output: Option<&Bound<'py, PyAny>>,
        format: &str,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let fmt = SolutionsFormat::from_name(format)
            .ok_or_else(|| invalid(py, "unknown SPARQL results format"))?;
        let out = output_from_py(output)?;
        let cursor = py
            .detach(|| {
                let Ok(mut state) = self.state.try_lock() else {
                    return None;
                };
                if state.emitted != 0 || state.serializing {
                    return None;
                }
                let cursor = state.cursor.take()?;
                if cursor.status() != CursorStatus::Open {
                    state.cursor = Some(cursor);
                    return None;
                }
                state.serializing = true;
                Some(cursor)
            })
            .ok_or_else(|| invalid(py, "serialize a fresh open cursor before iterating it"))?;
        let job = Serializing::new(self.state.clone(), cursor);
        let buf = interrupt::run(py, &self.cancel, move || {
            job.run(out, |cursor, writer| {
                write_cursor_solutions(cursor, fmt, writer, None).map(|_| 0)
            })
        })?;
        Ok(buf.map(|buf| PyBytes::new(py, &buf)))
    }
}

struct GraphState {
    cursor: Option<GraphCursor>,
    batch: Option<GraphBatch>,
    /// the next quad of `batch` to hand out or copy ahead
    row: usize,
    /// quads of `batch` before `row` that are copied and not yet handed out
    ahead: VecDeque<oxrdf::Quad>,
    emitted: u64,
    stats: CursorStats,
    plan: String,
    serializing: bool,
}

impl GraphState {
    fn status(&self) -> CursorStatus {
        let status = self
            .cursor
            .as_ref()
            .map_or(self.stats.status, GraphCursor::status);
        if status == CursorStatus::Complete && self.pending() {
            CursorStatus::Open
        } else {
            status
        }
    }

    /// Whether quads that were fetched have not been handed out.
    fn pending(&self) -> bool {
        !self.ahead.is_empty() || self.batch.as_ref().is_some_and(|b| self.row < b.len())
    }

    /// Whether the next quad is in the batch already fetched, so that reading it runs no
    /// part of the query.
    fn buffered(&self) -> bool {
        !self.serializing
            && self.cursor.is_some()
            && self.batch.as_ref().is_some_and(|b| self.row < b.len())
    }

    /// The next quad, with up to `quads - 1` of the quads after it in the batch copied
    /// into `ahead`, up to [`AHEAD_BYTES`] of text.
    fn refill(&mut self, quads: usize) -> sparkles::Result<Option<oxrdf::Quad>> {
        let Some(first) = self.next_quad()? else {
            return Ok(None);
        };
        let mut bytes = 0;
        while self.ahead.len() + 1 < quads && bytes < AHEAD_BYTES && self.buffered() {
            let Some(quad) = self
                .batch
                .as_ref()
                .map(|batch| batch.quads()[self.row].clone())
            else {
                break;
            };
            bytes += 192 + term_bytes(&quad.object);
            self.row += 1;
            self.ahead.push_back(quad);
        }
        Ok(Some(first))
    }

    /// The next quad, from the batch already fetched or from the cursor's next batch.
    fn next_quad(&mut self) -> sparkles::Result<Option<oxrdf::Quad>> {
        if self.serializing {
            return Err(sparkles::Error::invalid("the cursor is being serialized"));
        }
        let Some(mut cursor) = self.cursor.take() else {
            if self.stats.status == CursorStatus::Failed {
                return Err(failed(&self.stats));
            }
            return Ok(None);
        };
        let mut fetched = false;
        let result: sparkles::Result<Option<oxrdf::Quad>> = (|| {
            if self
                .batch
                .as_ref()
                .is_none_or(|batch| self.row == batch.len())
            {
                self.batch = None;
                self.row = 0;
                fetched = true;
                self.batch = cursor.next_batch()?;
            }
            let Some(batch) = &self.batch else {
                return Ok(None);
            };
            let values = batch.quads()[self.row].clone();
            self.row += 1;
            self.emitted += 1;
            Ok(Some(values))
        })();
        // Record the outcome before anything else can fail, and keep the
        // execution error as the one raised.
        if let Err(error) = &result {
            cursor.close();
            self.batch = None;
            self.stats = cursor.stats();
            self.stats.status = CursorStatus::Failed;
            self.stats.error = Some(error.to_string());
            if let Ok(plan) = cursor.plan_json() {
                self.plan = plan;
            }
        } else {
            // A quad of the batch already fetched leaves the cursor as it was, so the
            // statistics recorded with that batch still hold.
            if fetched {
                let finished = cursor.status() != CursorStatus::Open
                    && self.stats.status == CursorStatus::Open;
                self.stats = cursor.stats();
                if finished && let Ok(plan) = cursor.plan_json() {
                    self.plan = plan;
                }
            }
            self.cursor = Some(cursor);
        }
        result
    }
}

#[pyclass(module = "sparkles", name = "GraphCursor")]
pub struct PyGraphCursor {
    state: Arc<Mutex<GraphState>>,
    cancel: Arc<AtomicBool>,
}

impl PyGraphCursor {
    pub(crate) fn new(cursor: GraphCursor, cancel: Arc<AtomicBool>) -> sparkles::Result<Self> {
        let stats = cursor.stats();
        let plan = cursor.plan_json()?;
        Ok(Self {
            state: Arc::new(Mutex::new(GraphState {
                cursor: Some(cursor),
                batch: None,
                row: 0,
                ahead: VecDeque::new(),
                emitted: 0,
                stats,
                plan,
                serializing: false,
            })),
            cancel,
        })
    }
}

#[pymethods]
impl PyGraphCursor {
    #[getter]
    fn status(&self, py: Python<'_>) -> PyResult<String> {
        py.detach(|| {
            let state = self
                .state
                .try_lock()
                .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?;
            if state.serializing {
                return Err(sparkles::Error::invalid("the cursor is being serialized"));
            }
            Ok(state.status().as_str().into())
        })
        .py(py)
    }

    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stats = py
            .detach(|| {
                let state = self
                    .state
                    .try_lock()
                    .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?;
                let mut stats = state
                    .cursor
                    .as_ref()
                    .map_or_else(|| state.stats.clone(), GraphCursor::stats);
                if !state.serializing {
                    stats.status = state.status();
                    stats.emitted_rows = state.emitted;
                }
                Ok(stats)
            })
            .py(py)?;
        admin::to_py(py, &stats)
    }

    fn plan<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let plan = py
            .detach(|| {
                let state = self
                    .state
                    .try_lock()
                    .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?;
                state
                    .cursor
                    .as_ref()
                    .map_or_else(|| Ok(state.plan.clone()), GraphCursor::plan_json)
            })
            .py(py)?;
        py.import("json")?.call_method1("loads", (plan,))
    }

    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        // As for a SELECT cursor: a quad copied ahead is handed out here, and a fetch or
        // a refill runs detached.
        let ready = self.state.try_lock().ok().and_then(|mut state| {
            let quad = state.ahead.pop_front()?;
            state.emitted += 1;
            Some(quad)
        });
        let values = match ready {
            Some(quad) => Some(quad),
            None => {
                let state = self.state.clone();
                interrupt::run(py, &self.cancel, move || {
                    state
                        .try_lock()
                        .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?
                        .refill(AHEAD_ROWS)
                })?
            }
        };
        values
            .map(|quad| crate::terms::quad_to_py(py, quad))
            .transpose()
    }

    /// Internal async bridge: deliver one remaining native batch, without prefetch, copied
    /// in one detached call.
    fn _next_batch(&self, py: Python<'_>) -> PyResult<Option<Vec<Py<PyAny>>>> {
        let state = self.state.clone();
        let quads = interrupt::run(py, &self.cancel, move || {
            let mut state = state
                .try_lock()
                .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?;
            let mut quads: Vec<_> = state.ahead.drain(..).collect();
            state.emitted += quads.len() as u64;
            if quads.is_empty() {
                let Some(first) = state.next_quad()? else {
                    return Ok(None);
                };
                quads.push(first);
            }
            while state.buffered() {
                match state.next_quad()? {
                    Some(quad) => quads.push(quad),
                    None => break,
                }
            }
            Ok(Some(quads))
        })?;
        quads
            .map(|quads| {
                quads
                    .into_iter()
                    .map(|quad| crate::terms::quad_to_py(py, quad).map(Bound::unbind))
                    .collect()
            })
            .transpose()
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let ok = py
            .detach(|| -> sparkles::Result<bool> {
                let Ok(mut state) = self.state.try_lock() else {
                    return Ok(false);
                };
                if state.serializing {
                    return Ok(false);
                }
                let pending = state.pending();
                state.ahead.clear();
                if let Some(mut cursor) = state.cursor.take() {
                    cursor.close();
                    state.stats = cursor.stats();
                    if let Ok(plan) = cursor.plan_json() {
                        state.plan = plan;
                    }
                    if pending && state.stats.status == CursorStatus::Complete {
                        state.stats.status = CursorStatus::Stopped;
                    }
                }
                state.batch = None;
                Ok(true)
            })
            .py(py)?;
        if ok {
            Ok(())
        } else {
            Err(invalid(
                py,
                "wait for serialization to finish before closing the cursor",
            ))
        }
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (*_args))]
    fn __exit__(&self, py: Python<'_>, _args: &Bound<'_, PyTuple>) -> PyResult<bool> {
        self.close(py)?;
        Ok(false)
    }

    #[pyo3(signature = (output = None, format = "turtle"))]
    fn serialize<'py>(
        &self,
        py: Python<'py>,
        output: Option<&Bound<'py, PyAny>>,
        format: &str,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let fmt = sparkles::sparql::results::rdf_format_from_name(format);
        let jena = sparkles::jena_formats::JenaFormat::from_name(format);
        let native = SolutionsFormat::from_name(format) == Some(SolutionsFormat::Sparkles);
        if fmt.is_none() && jena.is_none() && !native {
            return Err(invalid(py, "unknown RDF result format"));
        }
        let out = output_from_py(output)?;
        let cursor = py
            .detach(|| {
                let Ok(mut state) = self.state.try_lock() else {
                    return None;
                };
                if state.emitted != 0 || state.serializing {
                    return None;
                }
                let cursor = state.cursor.take()?;
                if cursor.status() != CursorStatus::Open {
                    state.cursor = Some(cursor);
                    return None;
                }
                state.serializing = true;
                Some(cursor)
            })
            .ok_or_else(|| invalid(py, "serialize a fresh open cursor before iterating it"))?;
        let job = Serializing::new(self.state.clone(), cursor);
        let buf = interrupt::run(py, &self.cancel, move || {
            job.run(out, |cursor, writer| {
                if native {
                    sparkles::sparql::results::write_cursor_graph_native_json(
                        cursor, writer, None, None,
                    )
                } else if let Some(fmt) = fmt {
                    sparkles::sparql::cursor::graph::write_cursor_graph(
                        cursor,
                        fmt,
                        writer,
                        None,
                        &[],
                    )
                } else {
                    sparkles::sparql::results::write_cursor_jena_graph(
                        cursor,
                        jena.unwrap(),
                        writer,
                        None,
                    )
                }
                .map(|_| 0)
            })
        })?;
        Ok(buf.map(|buf| PyBytes::new(py, &buf)))
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyQueryCursor>()?;
    module.add_class::<PyGraphCursor>()
}
