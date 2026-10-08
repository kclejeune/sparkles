//! Fallible SELECT iteration over bounded engine batches.
use crate::admin;
use crate::errors::{EngineResult, invalid};
use crate::interrupt;
use crate::io::{output_from_py, write_output};
use crate::results::PyQuerySolution;
use crate::terms::PyVariable;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyTuple};
use sparkles::sparql::results::{SolutionsFormat, write_cursor_solutions};
use sparkles::sparql::{
    CursorStats, CursorStatus, GraphBatch, GraphCursor, QueryBatch, QueryCursor,
};
use std::sync::{Arc, Mutex, atomic::AtomicBool};

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
                Ok(state
                    .cursor
                    .as_ref()
                    .map_or_else(|| Ok(state.plan.clone()), QueryCursor::plan_json)?)
            })
            .py(py)?;
        py.import("json")?.call_method1("loads", (plan,))
    }

    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Option<PyQuerySolution>> {
        let state = self.state.clone();
        let values = interrupt::run(py, &self.cancel, move || {
            let mut state = state
                .try_lock()
                .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?;
            if state.serializing {
                return Err(sparkles::Error::invalid("the cursor is being serialized"));
            }
            let Some(mut cursor) = state.cursor.take() else {
                return Ok(None);
            };
            let result: sparkles::Result<Option<Vec<Option<oxrdf::Term>>>> = (|| {
                if state
                    .batch
                    .as_ref()
                    .is_none_or(|batch| state.row == batch.len())
                {
                    state.batch = None;
                    state.row = 0;
                    state.batch = cursor.next_batch()?;
                }
                let Some(batch) = &state.batch else {
                    return Ok(None);
                };
                let values = batch.row(state.row)?;
                state.row += 1;
                state.emitted += 1;
                Ok(Some(values))
            })();
            if result.is_err()
                || (cursor.status() != CursorStatus::Open
                    && state.stats.status == CursorStatus::Open)
            {
                state.plan = cursor.plan_json()?;
            }
            if let Err(error) = &result {
                cursor.close();
                state.batch = None;
                state.stats = cursor.stats();
                state.stats.status = CursorStatus::Failed;
                state.stats.error = Some(error.to_string());
            } else {
                state.stats = cursor.stats();
                state.cursor = Some(cursor);
            }
            result
        })?;
        Ok(values.map(|values| PyQuerySolution::new(self.vars.clone(), values)))
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
                    state.plan = cursor.plan_json()?;
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
        let state = self.state.clone();
        write_output(py, out, None, move |writer| {
            let mut cursor = cursor;
            let result = write_cursor_solutions(&mut cursor, fmt, writer, None);
            let mut state = state.lock().unwrap();
            state.serializing = false;
            state.stats = cursor.stats();
            state.plan = cursor.plan_json()?;
            state.emitted = state.stats.emitted_rows;
            result.map(|_| 0)
        })
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
                Ok(state
                    .cursor
                    .as_ref()
                    .map_or_else(|| Ok(state.plan.clone()), GraphCursor::plan_json)?)
            })
            .py(py)?;
        py.import("json")?.call_method1("loads", (plan,))
    }

    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let state = self.state.clone();
        let values = interrupt::run(py, &self.cancel, move || {
            let mut state = state
                .try_lock()
                .map_err(|_| sparkles::Error::invalid("a cursor operation is in progress"))?;
            if state.serializing {
                return Err(sparkles::Error::invalid("the cursor is being serialized"));
            }
            let Some(mut cursor) = state.cursor.take() else {
                return Ok(None);
            };
            let result: sparkles::Result<Option<oxrdf::Quad>> = (|| {
                if state
                    .batch
                    .as_ref()
                    .is_none_or(|batch| state.row == batch.len())
                {
                    state.batch = None;
                    state.row = 0;
                    state.batch = cursor.next_batch()?;
                }
                let Some(batch) = &state.batch else {
                    return Ok(None);
                };
                let values = batch.quads()[state.row].clone();
                state.row += 1;
                state.emitted += 1;
                Ok(Some(values))
            })();
            if result.is_err()
                || (cursor.status() != CursorStatus::Open
                    && state.stats.status == CursorStatus::Open)
            {
                state.plan = cursor.plan_json()?;
            }
            if let Err(error) = &result {
                cursor.close();
                state.batch = None;
                state.stats = cursor.stats();
                state.stats.status = CursorStatus::Failed;
                state.stats.error = Some(error.to_string());
            } else {
                state.stats = cursor.stats();
                state.cursor = Some(cursor);
            }
            result
        })?;
        values
            .map(|quad| crate::terms::quad_to_py(py, quad))
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
                let pending = state.batch.as_ref().is_some_and(|b| state.row < b.len());
                if let Some(mut cursor) = state.cursor.take() {
                    cursor.close();
                    state.stats = cursor.stats();
                    state.plan = cursor.plan_json()?;
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
        let state = self.state.clone();
        write_output(py, out, None, move |writer| {
            let mut cursor = cursor;
            let result = if native {
                sparkles::sparql::results::write_cursor_graph_native_json(
                    &mut cursor,
                    writer,
                    None,
                    None,
                )
            } else if let Some(fmt) = fmt {
                sparkles::sparql::cursor::graph::write_cursor_graph(
                    &mut cursor,
                    fmt,
                    writer,
                    None,
                    &[],
                )
            } else {
                sparkles::sparql::results::write_cursor_jena_graph(
                    &mut cursor,
                    jena.unwrap(),
                    writer,
                    None,
                )
            };
            let mut state = state.lock().unwrap();
            state.serializing = false;
            state.stats = cursor.stats();
            state.plan = cursor.plan_json()?;
            state.emitted = state.stats.emitted_rows;
            result.map(|_| 0)
        })
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyQueryCursor>()?;
    module.add_class::<PyGraphCursor>()
}
