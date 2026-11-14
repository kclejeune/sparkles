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
use std::sync::{Arc, Mutex, PoisonError, atomic::AtomicBool};

struct State {
    cursor: Option<QueryCursor>,
    batch: Option<QueryBatch>,
    row: usize,
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
        if status == CursorStatus::Complete
            && self.batch.as_ref().is_some_and(|b| self.row < b.len())
        {
            CursorStatus::Open
        } else {
            status
        }
    }

    /// Whether the next row is in the batch already fetched, so that reading it runs no
    /// part of the query.
    fn buffered(&self) -> bool {
        !self.serializing
            && self.cursor.is_some()
            && self.batch.as_ref().is_some_and(|b| self.row < b.len())
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
        // A row of the batch already fetched is decoded here with the GIL held. It runs no
        // part of the query and waits for nothing, and it takes less time than leaving
        // the thread does. Only a fetch runs detached, where Ctrl-C can cancel it.
        let ready = match self.state.try_lock() {
            Ok(mut state) if state.buffered() => Some(state.next_row()),
            _ => None,
        };
        let values = match ready {
            Some(row) => row.py(py)?,
            None => {
                let state = self.state.clone();
                interrupt::run(py, &self.cancel, move || {
                    state
                        .try_lock()
                        .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?
                        .next_row()
                })?
            }
        };
        Ok(values.map(|values| PyQuerySolution::new(self.vars.clone(), values)))
    }

    /// Internal async bridge: deliver one remaining native batch, without prefetch.
    fn _next_batch(&self, py: Python<'_>) -> PyResult<Option<Vec<Py<PyQuerySolution>>>> {
        let Some(first) = self.__next__(py)? else {
            return Ok(None);
        };
        let remaining = self
            .state
            .try_lock()
            .map(|state| {
                state
                    .batch
                    .as_ref()
                    .map_or(0, |batch| batch.len() - state.row)
            })
            .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))
            .py(py)?;
        let mut rows = Vec::with_capacity(remaining + 1);
        rows.push(Py::new(py, first)?);
        for _ in 0..remaining {
            let row = self
                .__next__(py)?
                .ok_or_else(|| invalid(py, "cursor changed while reading a batch"))?;
            rows.push(Py::new(py, row)?);
        }
        Ok(Some(rows))
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
                let pending = state.batch.as_ref().is_some_and(|b| state.row < b.len());
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
    row: usize,
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
        if status == CursorStatus::Complete
            && self.batch.as_ref().is_some_and(|b| self.row < b.len())
        {
            CursorStatus::Open
        } else {
            status
        }
    }

    /// Whether the next quad is in the batch already fetched, so that reading it runs no
    /// part of the query.
    fn buffered(&self) -> bool {
        !self.serializing
            && self.cursor.is_some()
            && self.batch.as_ref().is_some_and(|b| self.row < b.len())
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
        // As for a SELECT cursor, a quad of the batch already fetched is handed out here
        // with the GIL held, and only a fetch runs detached.
        let ready = match self.state.try_lock() {
            Ok(mut state) if state.buffered() => Some(state.next_quad()),
            _ => None,
        };
        let values = match ready {
            Some(quad) => quad.py(py)?,
            None => {
                let state = self.state.clone();
                interrupt::run(py, &self.cancel, move || {
                    state
                        .try_lock()
                        .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?
                        .next_quad()
                })?
            }
        };
        values
            .map(|quad| crate::terms::quad_to_py(py, quad))
            .transpose()
    }

    /// Internal async bridge: deliver one remaining native batch, without prefetch.
    fn _next_batch(&self, py: Python<'_>) -> PyResult<Option<Vec<Py<PyAny>>>> {
        let Some(first) = self.__next__(py)? else {
            return Ok(None);
        };
        let remaining = self
            .state
            .try_lock()
            .map(|state| {
                state
                    .batch
                    .as_ref()
                    .map_or(0, |batch| batch.len() - state.row)
            })
            .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))
            .py(py)?;
        let mut rows = Vec::with_capacity(remaining + 1);
        rows.push(first.unbind());
        for _ in 0..remaining {
            let row = self
                .__next__(py)?
                .ok_or_else(|| invalid(py, "cursor changed while reading a batch"))?;
            rows.push(row.unbind());
        }
        Ok(Some(rows))
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
                let pending = state.batch.as_ref().is_some_and(|b| state.row < b.len());
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
