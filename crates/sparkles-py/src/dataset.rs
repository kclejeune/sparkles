//! `sparkles.Dataset`: a persistent or in-memory dataset. Every call that reads or
//! writes the store runs without the GIL.

use crate::errors::{EngineResult, invalid, new_err};
use crate::io::{
    Output, codec_from_py, format_from_py, format_of_output, output_from_py, serialize_quads,
    source_from_py, write_output,
};
use crate::results::{
    PyPatchStats, PyQuadIterator, PyQuerySolutions, PyQueryTriples, PyUpdateStats,
};
use crate::terms::{
    PyVariable, graph_from_py, graph_to_py, iri_from_py, named_node_from_py, opt, quad_from_py,
    subject_from_py, term_from_py,
};
use crate::txn::{PyTransaction, WriterSlot};
use crate::{admin, interrupt};
use oxrdf::{GraphName, NamedOrBlankNode, Quad, Term};
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyString};
use sparkles::history::{At, HistoryOptions};
use sparkles::sparql::describe::{DescribeMode, DescribeOptions};
use sparkles::sparql::{QueryKind, QueryOptions, QueryResult};
use sparkles::store::{Store, StoreOptions};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// The named graph that `reason` writes and `include_inferred` reads.
pub const INFERRED_GRAPH: &str = "urn:x-sparkles:inferred";

/// An RDF dataset: the default graph and named graphs, in memory or in a database
/// directory.
#[pyclass(frozen, module = "sparkles", name = "Dataset")]
pub struct PyDataset {
    inner: RwLock<Option<sparkles::Dataset>>,
    path: Option<String>,
    writer: WriterSlot,
}

/// Arguments shared by the query methods of datasets and transactions.
#[derive(Default)]
pub struct QueryArgs<'py> {
    pub base_iri: Option<String>,
    pub prefixes: Option<BTreeMap<String, String>>,
    pub bindings: Option<Bound<'py, PyAny>>,
    pub default_graph: Option<Bound<'py, PyAny>>,
    pub named_graphs: Option<Bound<'py, PyAny>>,
    pub include_inferred: bool,
    pub timeout: Option<f64>,
    pub max_rows: Option<usize>,
    pub max_memory_bytes: Option<u64>,
    pub max_rows_produced: Option<u64>,
    pub cancel: Option<Bound<'py, PyAny>>,
    pub at: Option<Bound<'py, PyAny>>,
    /// DESCRIBE options over the dataset's setting: a mode, or a dict of options.
    pub describe: Option<Bound<'py, PyAny>>,
}

/// The DESCRIBE options of a query: `base` (the dataset's setting) with the `describe`
/// argument over it, which is a mode (`"cbd"`, `"scbd"` or `"outgoing"`) or a dict of
/// `mode`, `labels`, `reifiers`, `max_triples` and `max_depth` (`None` or `0`: no limit).
pub fn describe_options(
    base: DescribeOptions,
    ob: Option<&Bound<'_, PyAny>>,
) -> PyResult<DescribeOptions> {
    let Some(ob) = ob.filter(|o| !o.is_none()) else {
        return Ok(base);
    };
    let py = ob.py();
    let mut o = base;
    if let Ok(s) = ob.cast::<PyString>() {
        o.mode = DescribeMode::parse(s.to_str()?).py(py)?;
        return Ok(o);
    }
    let Ok(d) = ob.cast::<PyDict>() else {
        return Err(PyTypeError::new_err(
            "describe must be a mode (str) or a dict of DESCRIBE options",
        ));
    };
    for (k, v) in d.iter() {
        let k: String = k
            .extract()
            .map_err(|_| PyTypeError::new_err("DESCRIBE option names are str"))?;
        let key = match k.as_str() {
            "max_triples" => "maxTriples",
            "max_depth" => "maxDepth",
            other => other,
        };
        let value = if v.is_none() {
            "none".to_string()
        } else if let Ok(b) = v.cast::<pyo3::types::PyBool>() {
            b.is_true().to_string()
        } else if let Ok(n) = v.extract::<u64>() {
            n.to_string()
        } else if let Ok(s) = v.cast::<PyString>() {
            s.to_str()?.to_string()
        } else {
            return Err(PyTypeError::new_err(format!(
                "DESCRIBE option {k}: expected a str, bool, int or None"
            )));
        };
        o.set(key, &value).py(py)?;
    }
    Ok(o)
}

/// The engine's options for a query's arguments, and the state it reads (`None`: the
/// last commit). The options always carry a cancellation flag.
pub fn query_options(args: &QueryArgs<'_>) -> PyResult<(QueryOptions, Option<At>)> {
    let mut opts = QueryOptions {
        base_iri: args.base_iri.clone(),
        prefixes: args
            .prefixes
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect(),
        timeout: args.timeout.map(Duration::from_secs_f64),
        max_rows: args.max_rows,
        max_memory_bytes: args.max_memory_bytes,
        max_rows_produced: args.max_rows_produced,
        cancel: Some(interrupt::flag(args.cancel.as_ref())?),
        ..Default::default()
    };
    if let Some(b) = &args.bindings {
        let items = b.call_method0("items")?;
        for item in items.try_iter()? {
            let item = item?;
            let (k, v): (Bound<'_, PyAny>, Bound<'_, PyAny>) = item.extract()?;
            let name = if let Ok(var) = k.cast::<PyVariable>() {
                var.get().name.clone()
            } else if let Ok(s) = k.cast::<PyString>() {
                s.to_str()?.trim_start_matches(['?', '$']).to_string()
            } else {
                return Err(PyTypeError::new_err("binding names are str or Variable"));
            };
            opts.initial_bindings.push((name, term_from_py(&v)?));
        }
    }
    let iris = |ob: &Option<Bound<'_, PyAny>>| -> PyResult<Vec<String>> {
        match ob {
            None => Ok(Vec::new()),
            Some(ob) => ob
                .try_iter()?
                .map(|g| Ok(iri_from_py(&g?)?.into_string()))
                .collect(),
        }
    };
    opts.default_graph_uris = iris(&args.default_graph)?;
    opts.named_graph_uris = iris(&args.named_graphs)?;
    if args.include_inferred {
        opts.default_graph_extra = vec![INFERRED_GRAPH.to_string()];
    }
    let at = args.at.as_ref().map(at_from_py).transpose()?;
    Ok((opts, at))
}

/// The dataset's query defaults in `opts`: RDFS on read, and the DESCRIBE setting with
/// the query's `describe` options over it. The inferred overlay stays what
/// `include_inferred` asked for, so the options are marked as holding the defaults.
pub fn apply_defaults(
    opts: &mut QueryOptions,
    defaults: &QueryOptions,
    args: &QueryArgs<'_>,
) -> PyResult<()> {
    opts.rdfs = defaults.rdfs.clone();
    opts.describe = describe_options(defaults.describe.clone(), args.describe.as_ref())?;
    opts.defaults_applied = true;
    Ok(())
}

/// A point in a dataset's history: a commit number, or `head`, `commit:N`,
/// `time:<RFC 3339>` or `snapshot:<name>`.
pub fn at_from_py(ob: &Bound<'_, PyAny>) -> PyResult<At> {
    if let Ok(n) = ob.extract::<u64>() {
        return Ok(At::Commit(n));
    }
    let s: String = ob
        .extract()
        .map_err(|_| PyTypeError::new_err("at must be an int or a str"))?;
    s.parse::<At>().py(ob.py())
}

/// A query's result as Python objects, after checking its form against `want`.
pub fn result_to_py<'py>(
    py: Python<'py>,
    r: QueryResult,
    want: Option<&[QueryKind]>,
) -> PyResult<Bound<'py, PyAny>> {
    if let Some(want) = want
        && !want.contains(&r.kind)
    {
        let name = |k: &QueryKind| format!("{k:?}").to_uppercase();
        let names: Vec<String> = want.iter().map(name).collect();
        return Err(invalid(
            py,
            format!(
                "not a {} query (it is a {} query)",
                names.join(" or "),
                name(&r.kind)
            ),
        ));
    }
    Ok(match r.kind {
        QueryKind::Select => PyQuerySolutions::new(r).into_pyobject(py)?.into_any(),
        QueryKind::Ask => pyo3::types::PyBool::new(py, r.boolean)
            .to_owned()
            .into_any(),
        QueryKind::Construct | QueryKind::Describe => PyQueryTriples::new(r.triples, r.quads)
            .into_pyobject(py)?
            .into_any(),
    })
}

impl PyDataset {
    fn from_dataset(ds: sparkles::Dataset, path: Option<String>) -> PyDataset {
        PyDataset {
            inner: RwLock::new(Some(ds)),
            path,
            writer: Arc::new(Mutex::new(None)),
        }
    }

    fn open_at(py: Python<'_>, path: PathBuf, union_default_graph: bool) -> PyResult<PyDataset> {
        let opts = StoreOptions {
            union_default_graph,
            ..Default::default()
        };
        let shown = path.display().to_string();
        let ds = py
            .detach(|| sparkles::Dataset::open_with(&path, opts))
            .py(py)?;
        // the library installs the write-time validation the database sets up; when it
        // cannot be loaded, writes stay refused
        if let Some(e) = ds.guard_error() {
            let msg = format!(
                "{shown}: write-time validation could not be loaded, so writes are refused: {e}"
            );
            PyErr::warn(
                py,
                &py.get_type::<pyo3::exceptions::PyRuntimeWarning>(),
                &std::ffi::CString::new(msg).unwrap_or_default(),
                1,
            )?;
        }
        Ok(PyDataset::from_dataset(ds, Some(shown)))
    }

    fn in_memory(union_default_graph: bool) -> PyDataset {
        let opts = StoreOptions {
            union_default_graph,
            ..Default::default()
        };
        PyDataset::from_dataset(sparkles::Dataset::from_store(Store::in_memory(opts)), None)
    }

    /// The engine's handle (a cheap clone), or an error once closed.
    fn ds(&self, py: Python<'_>) -> PyResult<sparkles::Dataset> {
        self.inner
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| invalid(py, "the dataset is closed"))
    }

    /// The handle for a write: refused on the thread whose transaction is open, where
    /// it would wait for its own lock.
    fn ds_for_write(&self, py: Python<'_>) -> PyResult<sparkles::Dataset> {
        if *self.writer.lock().unwrap() == Some(std::thread::current().id()) {
            return Err(new_err(
                py,
                "ConflictError",
                "this thread has an open transaction on the dataset: write through the transaction",
            ));
        }
        self.ds(py)
    }

    fn run_query<'py>(
        &self,
        py: Python<'py>,
        query: &str,
        args: QueryArgs<'py>,
        want: Option<&[QueryKind]>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let (mut opts, at) = query_options(&args)?;
        apply_defaults(&mut opts, &ds.query_options(), &args)?;
        let cancel = opts.cancel.clone().unwrap_or_default();
        let query = query.to_string();
        let r = interrupt::run(py, &cancel, move || {
            query_at(&ds, &query, &opts, at.as_ref())
        })?;
        result_to_py(py, r, want)
    }
}

/// Run a query on the last commit, or on the state `at` names.
fn query_at(
    ds: &sparkles::Dataset,
    query: &str,
    opts: &QueryOptions,
    at: Option<&At>,
) -> sparkles::Result<QueryResult> {
    match at {
        None => ds.query_with(query, opts),
        Some(at) => {
            let ho = HistoryOptions {
                cancel: opts.cancel.clone(),
                deadline: opts.timeout.map(|t| Instant::now() + t),
            };
            let (snap, _) = ds.store().snapshot_at(at, &ho)?;
            sparkles::sparql::query(snap, query, opts)
        }
    }
}

/// The arguments of a query method.
#[allow(clippy::too_many_arguments)]
pub fn query_args<'py>(
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
    at: Option<Bound<'py, PyAny>>,
    describe: Option<Bound<'py, PyAny>>,
) -> QueryArgs<'py> {
    QueryArgs {
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
        at,
        describe,
    }
}

#[pymethods]
impl PyDataset {
    /// `Dataset()` is in memory; `Dataset(path)` opens or creates a database directory.
    #[new]
    #[pyo3(signature = (path = None, *, union_default_graph = false))]
    fn new(py: Python<'_>, path: Option<PathBuf>, union_default_graph: bool) -> PyResult<Self> {
        match path {
            Some(p) => PyDataset::open_at(py, p, union_default_graph),
            None => Ok(PyDataset::in_memory(union_default_graph)),
        }
    }

    /// A new, empty in-memory dataset.
    #[staticmethod]
    #[pyo3(signature = (*, union_default_graph = false))]
    fn memory(union_default_graph: bool) -> Self {
        PyDataset::in_memory(union_default_graph)
    }

    /// Open (or create) a database directory; it stays locked while open.
    #[staticmethod]
    #[pyo3(signature = (path, *, union_default_graph = false))]
    fn open(py: Python<'_>, path: PathBuf, union_default_graph: bool) -> PyResult<Self> {
        PyDataset::open_at(py, path, union_default_graph)
    }

    /// The database directory, or `None` in memory.
    #[getter]
    fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// Release this handle. The directory lock goes once the iterators and transactions
    /// still using the dataset are gone.
    fn close(&self, py: Python<'_>) {
        let ds = self.inner.write().unwrap().take();
        py.detach(|| drop(ds));
    }

    #[getter]
    fn closed(&self) -> bool {
        self.inner.read().unwrap().is_none()
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (*_args))]
    fn __exit__(&self, py: Python<'_>, _args: &Bound<'_, pyo3::types::PyTuple>) -> bool {
        self.close(py);
        false
    }

    fn __repr__(&self) -> String {
        match (&self.path, self.closed()) {
            (_, true) => "<Dataset (closed)>".into(),
            (Some(p), false) => format!("<Dataset path={p:?}>"),
            (None, false) => "<Dataset in memory>".into(),
        }
    }

    // ---------------------------------------------------------------- loading ----

    /// Load RDF from `input` (str, bytes or a binary file object) or the file at
    /// `path`; returns the number of new quads.
    #[pyo3(signature = (input = None, format = None, *, path = None, base_iri = None, to_graph = None, compression = None, lenient = false, mapping = None, template = None, key = None))]
    #[allow(clippy::too_many_arguments)]
    fn load(
        &self,
        py: Python<'_>,
        input: Option<&Bound<'_, PyAny>>,
        format: Option<&Bound<'_, PyAny>>,
        path: Option<PathBuf>,
        base_iri: Option<String>,
        to_graph: Option<&Bound<'_, PyAny>>,
        compression: Option<&str>,
        lenient: bool,
        mapping: Option<PathBuf>,
        template: Option<PathBuf>,
        key: Option<String>,
    ) -> PyResult<u64> {
        let graph = opt(to_graph, iri_from_py)?;
        let table = crate::io::table_kind(format, path.as_deref())?;
        if let Some(kind) = table {
            let args = crate::io::TableArgs {
                kind,
                mapping,
                template,
                key,
                base: base_iri,
                compression: codec_from_py(py, compression)?,
            };
            let ds = self.ds_for_write(py)?;
            return crate::io::load_table(py, input, path.as_deref(), &args, graph, move |src| {
                ds.store().load(&[src])
            });
        }
        if mapping.is_some() || template.is_some() || key.is_some() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "mapping, template and key apply to CSV and TSV input",
            ));
        }
        if let Some(i) = input.filter(|i| !i.is_none())
            && path.is_none()
            && crate::io::is_file_object(i)?
        {
            // a file object streams into one transaction as it is read
            let mut stream = crate::io::quad_stream(
                py,
                Some(i),
                format,
                None,
                base_iri,
                graph,
                compression,
                lenient,
            )?;
            let ds = self.ds_for_write(py)?;
            return py
                .detach(|| {
                    let n = ds.transaction(|tx| {
                        let mut n = 0;
                        for q in stream.by_ref() {
                            n += tx.insert(q?.as_ref())? as u64;
                        }
                        Ok(n)
                    })?;
                    ds.store().add_prefixes(stream.prefixes())?;
                    Ok(n)
                })
                .py(py);
        }
        // the spool of an input in one of Jena's syntaxes lives until the load ends
        let (src, _spool) = source_from_py(
            py,
            input,
            format,
            path,
            base_iri,
            graph,
            compression,
            lenient,
        )?;
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.store().load(&[src])).py(py)
    }

    /// Load many files in one commit (the parallel bulk path); returns the new quads.
    #[pyo3(signature = (paths, *, to_graph = None))]
    fn load_files(
        &self,
        py: Python<'_>,
        paths: Vec<PathBuf>,
        to_graph: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<u64> {
        let graph = opt(to_graph, iri_from_py)?;
        let sources = paths
            .iter()
            .map(|p| {
                source_from_py(
                    py,
                    None,
                    None,
                    Some(p.clone()),
                    None,
                    graph.clone(),
                    None,
                    false,
                )
            })
            .collect::<PyResult<Vec<_>>>()?;
        let (sources, _spools): (Vec<_>, Vec<_>) = sources.into_iter().unzip();
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.store().load(&sources)).py(py)
    }

    // ----------------------------------------------------------------- SPARQL ----

    /// Run a SPARQL query: `QuerySolutions` for SELECT, `bool` for ASK, `QueryTriples`
    /// for CONSTRUCT and DESCRIBE.
    #[pyo3(signature = (query, *, base_iri = None, prefixes = None, bindings = None, default_graph = None, named_graphs = None, include_inferred = false, timeout = None, max_rows = None, max_memory_bytes = None, max_rows_produced = None, cancel = None, at = None, describe = None))]
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
        at: Option<Bound<'py, PyAny>>,
        describe: Option<Bound<'py, PyAny>>,
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
            at,
            describe,
        );
        self.run_query(py, query, args, None)
    }

    /// Run a SELECT query.
    #[pyo3(signature = (query, *, base_iri = None, prefixes = None, bindings = None, default_graph = None, named_graphs = None, include_inferred = false, timeout = None, max_rows = None, max_memory_bytes = None, max_rows_produced = None, cancel = None, at = None, describe = None))]
    #[allow(clippy::too_many_arguments)]
    fn select<'py>(
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
        at: Option<Bound<'py, PyAny>>,
        describe: Option<Bound<'py, PyAny>>,
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
            at,
            describe,
        );
        self.run_query(py, query, args, Some(&[QueryKind::Select]))
    }

    /// Run an ASK query.
    #[pyo3(signature = (query, *, base_iri = None, prefixes = None, bindings = None, default_graph = None, named_graphs = None, include_inferred = false, timeout = None, max_rows = None, max_memory_bytes = None, max_rows_produced = None, cancel = None, at = None, describe = None))]
    #[allow(clippy::too_many_arguments)]
    fn ask<'py>(
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
        at: Option<Bound<'py, PyAny>>,
        describe: Option<Bound<'py, PyAny>>,
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
            at,
            describe,
        );
        self.run_query(py, query, args, Some(&[QueryKind::Ask]))
    }

    /// Run a CONSTRUCT or DESCRIBE query.
    #[pyo3(signature = (query, *, base_iri = None, prefixes = None, bindings = None, default_graph = None, named_graphs = None, include_inferred = false, timeout = None, max_rows = None, max_memory_bytes = None, max_rows_produced = None, cancel = None, at = None, describe = None))]
    #[allow(clippy::too_many_arguments)]
    fn construct<'py>(
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
        at: Option<Bound<'py, PyAny>>,
        describe: Option<Bound<'py, PyAny>>,
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
            at,
            describe,
        );
        self.run_query(
            py,
            query,
            args,
            Some(&[QueryKind::Construct, QueryKind::Describe]),
        )
    }

    /// Run a SPARQL Update request in one transaction.
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
        let ds = self.ds_for_write(py)?;
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
        let update = update.to_string();
        let s = interrupt::run(py, &flag, move || ds.update_with(&update, &opts))?;
        Ok(PyUpdateStats::from(s))
    }

    /// Apply an RDF Patch, in the text form or (`binary`) the RDF Thrift form, as one
    /// commit of kind `patch`. `data` is the patch as `str` or `bytes`. A `TA` row
    /// aborts the patch and nothing is applied.
    #[pyo3(signature = (data, binary = false, *, message = None))]
    fn apply_patch(
        &self,
        py: Python<'_>,
        data: &Bound<'_, PyAny>,
        binary: bool,
        message: Option<String>,
    ) -> PyResult<PyPatchStats> {
        let bytes: Vec<u8> = if let Ok(s) = data.cast::<PyString>() {
            s.to_str()?.as_bytes().to_vec()
        } else if let Ok(b) = data.cast::<PyBytes>() {
            b.as_bytes().to_vec()
        } else {
            return Err(PyTypeError::new_err("data must be str or bytes"));
        };
        let message = match message {
            Some(m) => sparkles::annotations::validate_message(&m).py(py)?,
            None => None,
        };
        let ds = self.ds_for_write(py)?;
        let o = py
            .detach(move || {
                ds.store().apply_patch(
                    &bytes[..],
                    &sparkles::store::PatchOptions {
                        binary,
                        write: sparkles::guard::WriteOptions {
                            message,
                            ..Default::default()
                        },
                    },
                )
            })
            .py(py)?;
        Ok(PyPatchStats::from(o))
    }

    // ------------------------------------------------------------------ quads ----

    /// Add a quad (or a triple, to the default graph); true if it was new.
    fn add(&self, py: Python<'_>, quad: &Bound<'_, PyAny>) -> PyResult<bool> {
        let q = quad_from_py(quad)?;
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.transaction(|tx| tx.insert_linked(q.as_ref())))
            .py(py)
    }

    /// Apply inserts and removals in order in one commit. `ops` holds
    /// `(insert, quad)` pairs. Returns the number of quads inserted and removed, and the
    /// stored label of each blank node label that an insert gave a new node.
    /// `sparkles.rdflib` writes through this.
    fn _apply(
        &self,
        py: Python<'_>,
        ops: &Bound<'_, PyAny>,
    ) -> PyResult<(u64, u64, BTreeMap<String, String>)> {
        let ops = crate::txn::ops_from_py(ops)?;
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.transaction(|tx| crate::txn::apply_ops(tx, &ops)))
            .py(py)
    }

    /// The quads of a pattern over every graph, with the quads of one triple next to
    /// each other (for `sparkles.rdflib`).
    #[pyo3(signature = (subject = None, predicate = None, object = None))]
    fn _quads_by_triple(
        &self,
        py: Python<'_>,
        subject: Option<&Bound<'_, PyAny>>,
        predicate: Option<&Bound<'_, PyAny>>,
        object: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PyQuadIterator> {
        let s: Option<NamedOrBlankNode> = opt(subject, subject_from_py)?;
        let p = opt(predicate, named_node_from_py)?;
        let o: Option<Term> = opt(object, term_from_py)?;
        let ds = self.ds(py)?;
        let it = py.detach(|| ds.quads_by_triple(s.as_ref(), p.as_ref(), o.as_ref()));
        Ok(PyQuadIterator::from_scan(it))
    }

    /// Remove a quad; true if it was present.
    fn remove(&self, py: Python<'_>, quad: &Bound<'_, PyAny>) -> PyResult<bool> {
        let q = quad_from_py(quad)?;
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.remove(q.as_ref())).py(py)
    }

    /// Add quads in one commit (triples go to the default graph); returns how many were
    /// new.
    fn extend(&self, py: Python<'_>, quads: &Bound<'_, PyAny>) -> PyResult<u64> {
        let quads = quads
            .try_iter()?
            .map(|q| quad_from_py(&q?))
            .collect::<PyResult<Vec<Quad>>>()?;
        let ds = self.ds_for_write(py)?;
        py.detach(|| {
            ds.transaction(|tx| {
                quads
                    .iter()
                    .try_fold(0, |n, q| Ok(n + tx.insert_linked(q.as_ref())? as u64))
            })
        })
        .py(py)
    }

    /// The quads matching a pattern; `None` matches anything, and `graph_name=None`
    /// matches every graph.
    #[pyo3(signature = (subject = None, predicate = None, object = None, graph_name = None))]
    fn quads_for_pattern(
        &self,
        py: Python<'_>,
        subject: Option<&Bound<'_, PyAny>>,
        predicate: Option<&Bound<'_, PyAny>>,
        object: Option<&Bound<'_, PyAny>>,
        graph_name: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PyQuadIterator> {
        let s: Option<NamedOrBlankNode> = opt(subject, subject_from_py)?;
        let p = opt(predicate, named_node_from_py)?;
        let o: Option<Term> = opt(object, term_from_py)?;
        let g: Option<GraphName> = opt(graph_name, graph_from_py)?;
        let ds = self.ds(py)?;
        let it = py.detach(|| {
            ds.quads(
                g.as_ref().map(GraphName::as_ref),
                s.as_ref(),
                p.as_ref(),
                o.as_ref(),
            )
        });
        Ok(PyQuadIterator::from_scan(it))
    }

    fn __iter__(&self, py: Python<'_>) -> PyResult<PyQuadIterator> {
        self.quads_for_pattern(py, None, None, None, None)
    }

    fn __contains__(&self, py: Python<'_>, quad: &Bound<'_, PyAny>) -> PyResult<bool> {
        let q = quad_from_py(quad)?;
        let ds = self.ds(py)?;
        py.detach(|| ds.contains(q.as_ref())).py(py)
    }

    /// The number of quads.
    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        let ds = self.ds(py)?;
        Ok(py.detach(|| ds.len()) as usize)
    }

    /// The names of the non-empty named graphs.
    fn named_graphs<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyAny>>> {
        let ds = self.ds(py)?;
        let names = py.detach(|| ds.graph_names()).py(py)?;
        names
            .into_iter()
            .map(|n| graph_to_py(py, &n.into()))
            .collect()
    }

    /// Remove every triple of a graph; returns how many were removed.
    fn clear_graph(&self, py: Python<'_>, graph_name: &Bound<'_, PyAny>) -> PyResult<u64> {
        let g = graph_from_py(graph_name)?;
        let ds = self.ds_for_write(py)?;
        py.detach(|| match &g {
            GraphName::DefaultGraph => ds.default_graph().clear(),
            GraphName::NamedNode(n) => ds.named_graph(n.as_str())?.clear(),
            GraphName::BlankNode(_) => {
                Err(sparkles::Error::unsupported("clearing a blank-node graph"))
            }
        })
        .py(py)
    }

    /// Remove every quad.
    fn clear(&self, py: Python<'_>) -> PyResult<()> {
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.update("CLEAR ALL")).py(py).map(|_| ())
    }

    /// Begin a write transaction (a context manager).
    fn transaction(&self, py: Python<'_>) -> PyResult<PyTransaction> {
        let ds = self.ds_for_write(py)?;
        PyTransaction::begin(py, ds, self.writer.clone())
    }

    // ----------------------------------------------------------- output, admin ----

    /// Serialize the dataset (a quad format: every graph; a triple format: the default
    /// graph or `from_graph`). Returns bytes when `output` is `None`.
    #[pyo3(signature = (output = None, format = None, *, from_graph = None, compression = None))]
    fn dump<'py>(
        &self,
        py: Python<'py>,
        output: Option<&Bound<'py, PyAny>>,
        format: Option<&Bound<'py, PyAny>>,
        from_graph: Option<&Bound<'py, PyAny>>,
        compression: Option<&str>,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let out = output_from_py(output)?;
        let format = match format_from_py(format)?.or_else(|| format_of_output(&out)) {
            Some(f) => f,
            None => {
                return Err(invalid(
                    py,
                    "give a format (or an output path with an RDF extension)",
                ));
            }
        };
        let codec = codec_from_py(py, compression)?;
        let graph = opt(from_graph, graph_from_py)?;
        let ds = self.ds(py)?;
        if let Output::Path(p) = &out
            && let Some(dir) = p.parent()
            && !dir.as_os_str().is_empty()
            && !dir.exists()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("No such directory: {}", dir.display()),
            )
            .into());
        }
        write_output(py, out, codec, move |w| match (graph, format) {
            (None, crate::io::Fmt::Rdf(f)) => ds.dump(w, f),
            (None, crate::io::Fmt::Jena(_)) => {
                // a triple syntax holds the default graph, as `dump` writes it
                let g = (!format.supports_datasets()).then_some(GraphName::DefaultGraph);
                let quads = ds.quads(g.as_ref().map(|g| g.as_ref()), None, None, None);
                serialize_quads(w, format, ds.prefixes(), quads, false)
            }
            (Some(g), _) => {
                let quads = ds.quads(Some(g.as_ref()), None, None, None);
                serialize_quads(w, format, ds.prefixes(), quads, false)
            }
        })
    }

    /// Merge the pending changes into a new index generation.
    fn compact(&self, py: Python<'_>) -> PyResult<()> {
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.compact()).py(py)
    }

    /// Write a compressed N-Quads backup into a directory; returns its path.
    fn backup(&self, py: Python<'_>, directory: PathBuf) -> PyResult<String> {
        let ds = self.ds(py)?;
        let p = py.detach(|| ds.backup(&directory)).py(py)?;
        Ok(p.display().to_string())
    }

    /// The dataset's prefixes, as `{prefix: namespace}`.
    #[getter]
    fn prefixes(&self, py: Python<'_>) -> PyResult<BTreeMap<String, String>> {
        Ok(self.ds(py)?.prefixes())
    }

    fn set_prefix(&self, py: Python<'_>, prefix: &str, iri: &str) -> PyResult<()> {
        let ds = self.ds(py)?;
        py.detach(|| ds.set_prefix(prefix, iri)).py(py)
    }

    // ------------------------------------------------------ reasoning, validation ----

    /// Materialize entailments into `urn:x-sparkles:inferred`.
    #[pyo3(signature = (profile = "rdfs", *, rules = None))]
    fn reason(
        &self,
        py: Python<'_>,
        profile: &str,
        rules: Option<String>,
    ) -> PyResult<crate::validate::PyReasonReport> {
        let ds = self.ds_for_write(py)?;
        crate::validate::reason(py, ds, profile, rules)
    }

    /// Remove the materialized entailments; returns how many triples were removed.
    fn clear_inferences(&self, py: Python<'_>) -> PyResult<u64> {
        let ds = self.ds_for_write(py)?;
        crate::validate::clear_inferences(py, ds)
    }

    /// Validate with SHACL shapes given as text, or read from `shapes_graph`.
    #[pyo3(signature = (shapes = None, *, format = None, shapes_graph = None, data_graph = None, include_inferred = false))]
    fn validate_shacl(
        &self,
        py: Python<'_>,
        shapes: Option<&Bound<'_, PyAny>>,
        format: Option<&Bound<'_, PyAny>>,
        shapes_graph: Option<&Bound<'_, PyAny>>,
        data_graph: Option<&Bound<'_, PyAny>>,
        include_inferred: bool,
    ) -> PyResult<crate::validate::PyShaclReport> {
        let ds = self.ds(py)?;
        let shapes_graph = opt(shapes_graph, iri_from_py)?.map(|n| n.into_string());
        let data_graph = opt(data_graph, iri_from_py)?.map(|n| n.into_string());
        // "shaclc" (or `text/shaclc`) is the SHACL Compact Syntax; other names are RDF
        // formats
        let compact = match format.and_then(|f| f.cast::<PyString>().ok()) {
            Some(s) => matches!(
                s.to_str()?.trim().to_ascii_lowercase().as_str(),
                "shaclc" | "shc" | "text/shaclc"
            ),
            None => false,
        };
        let format = if compact {
            None
        } else {
            crate::io::rdf_only(format_from_py(format)?, "shapes")?
        };
        let shapes = match shapes.filter(|s| !s.is_none()) {
            None => None,
            Some(s) => Some(if let Ok(b) = s.cast::<PyBytes>() {
                String::from_utf8(b.as_bytes().to_vec())
                    .map_err(|_| invalid(py, "the shapes are not UTF-8"))?
            } else {
                s.extract::<String>()?
            }),
        };
        crate::validate::shacl(
            py,
            ds,
            shapes,
            format,
            compact,
            shapes_graph,
            data_graph,
            include_inferred,
        )
    }

    /// Validate with a ShEx schema (ShExC or ShExJ) and a shape map.
    #[pyo3(signature = (schema, shape_map, *, format = None, data_graph = None, include_inferred = false))]
    fn validate_shex(
        &self,
        py: Python<'_>,
        schema: String,
        shape_map: String,
        format: Option<String>,
        data_graph: Option<&Bound<'_, PyAny>>,
        include_inferred: bool,
    ) -> PyResult<crate::validate::PyShexReport> {
        let ds = self.ds(py)?;
        let data_graph = opt(data_graph, iri_from_py)?.map(|n| n.into_string());
        crate::validate::shex(
            py,
            ds,
            schema,
            shape_map,
            format,
            data_graph,
            include_inferred,
        )
    }

    // ------------------------------------------------------- history, snapshots ----

    /// The latest commit.
    #[getter]
    fn head_commit(&self, py: Python<'_>) -> PyResult<admin::PyCommit> {
        Ok(admin::head_commit(&self.ds(py)?))
    }

    /// Commits, newest first, or oldest first with `after`.
    #[pyo3(signature = (limit = 100, *, before = None, after = None))]
    fn commits(
        &self,
        py: Python<'_>,
        limit: usize,
        before: Option<u64>,
        after: Option<u64>,
    ) -> PyResult<Vec<admin::PyCommit>> {
        admin::commits(py, &self.ds(py)?, limit, before, after)
    }

    /// Keep a commit readable under a name (`at="snapshot:<name>"`).
    #[pyo3(signature = (name, at = None, *, note = None, expires_ms = None))]
    fn create_snapshot(
        &self,
        py: Python<'_>,
        name: &str,
        at: Option<&Bound<'_, PyAny>>,
        note: Option<String>,
        expires_ms: Option<i64>,
    ) -> PyResult<admin::PySnapshot> {
        let at = opt(at, at_from_py)?.unwrap_or(At::Head);
        admin::create_snapshot(py, &self.ds(py)?, name, at, note, expires_ms)
    }

    /// The named snapshots.
    fn snapshots(&self, py: Python<'_>) -> PyResult<Vec<admin::PySnapshot>> {
        Ok(admin::snapshots(&self.ds(py)?))
    }

    /// Remove a named snapshot; true if it existed.
    fn delete_snapshot(&self, py: Python<'_>, name: &str) -> PyResult<bool> {
        admin::delete_snapshot(py, &self.ds(py)?, name)
    }

    /// What the dataset's history holds: the head, the readable commit ranges, the
    /// retention window and the number of snapshots.
    fn history<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
        admin::history(py, &self.ds(py)?)
    }

    /// Keep past states readable for the last `keep_commits` commits or `keep_age`
    /// seconds, in at most `max_bytes` of disk; returns `history()`.
    #[pyo3(signature = (*, keep_commits = None, keep_age = None, max_bytes = None))]
    fn set_retention<'py>(
        &self,
        py: Python<'py>,
        keep_commits: Option<u64>,
        keep_age: Option<f64>,
        max_bytes: Option<u64>,
    ) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
        admin::set_retention(py, &self.ds(py)?, keep_commits, keep_age, max_bytes)
    }

    /// Copy the dataset, or its state `at`, into a new database directory with a new
    /// dataset id; returns a report.
    #[pyo3(signature = (directory, *, at = None, exclude_graphs = None))]
    fn clone_to<'py>(
        &self,
        py: Python<'py>,
        directory: PathBuf,
        at: Option<&Bound<'py, PyAny>>,
        exclude_graphs: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
        let at = opt(at, at_from_py)?;
        let exclude = match exclude_graphs {
            None => Vec::new(),
            Some(g) => g
                .try_iter()?
                .map(|g| iri_from_py(&g?))
                .collect::<PyResult<_>>()?,
        };
        admin::clone_to(py, &self.ds(py)?, directory, at, exclude)
    }

    // --------------------------------------------------------- text and vectors ----

    /// Enable (or reconfigure) full-text search, as a dict like `text.json`, and build
    /// the index; returns its status.
    #[pyo3(signature = (config = None))]
    fn enable_text<'py>(
        &self,
        py: Python<'py>,
        config: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds_for_write(py)?;
        #[cfg(feature = "text")]
        return admin::enable_text(py, &ds, config);
        #[cfg(not(feature = "text"))]
        {
            let _ = (ds, config);
            Err(crate::errors::missing_feature(py, "text"))
        }
    }

    /// Rebuild the full-text index from the current state; returns its status.
    fn rebuild_text<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds_for_write(py)?;
        #[cfg(feature = "text")]
        return admin::rebuild_text(py, &ds);
        #[cfg(not(feature = "text"))]
        {
            let _ = ds;
            Err(crate::errors::missing_feature(py, "text"))
        }
    }

    /// Turn full-text search off and remove the index.
    fn disable_text(&self, py: Python<'_>) -> PyResult<()> {
        let ds = self.ds_for_write(py)?;
        #[cfg(feature = "text")]
        return admin::disable_text(py, &ds);
        #[cfg(not(feature = "text"))]
        {
            let _ = ds;
            Err(crate::errors::missing_feature(py, "text"))
        }
    }

    /// The full-text index's status, or `None` when it is off.
    fn text_status<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let ds = self.ds(py)?;
        #[cfg(feature = "text")]
        return admin::text_status(py, &ds);
        #[cfg(not(feature = "text"))]
        {
            let _ = ds;
            Ok(None)
        }
    }

    /// Create or replace a vector index over the embeddings of `predicate`, and start
    /// building it in the background; true when it was created. `options` holds more of
    /// the configuration (`metric`, `model`, `hnsw`, `exactThreshold`, and `embedding` for
    /// an index that computes its vectors with an embeddings endpoint).
    #[pyo3(signature = (name, predicate, dimension, *, options = None))]
    fn create_vector_index(
        &self,
        py: Python<'_>,
        name: &str,
        predicate: &Bound<'_, PyAny>,
        dimension: usize,
        options: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        let predicate = iri_from_py(predicate)?;
        let ds = self.ds_for_write(py)?;
        admin::create_vector_index(py, &ds, name, predicate, dimension, options)
    }

    /// Remove a vector index.
    fn drop_vector_index(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.store().drop_vector_index(name)).py(py)
    }

    /// Rebuild a vector index in the background.
    fn rebuild_vector_index(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.store().rebuild_vector_index(name)).py(py)
    }

    /// Embed every selected text of a vector index again, the next time `embed` runs.
    fn reembed_vector_index(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.store().reembed(name)).py(py)
    }

    /// Embed the text waiting for the vector indexes that compute their vectors, on this
    /// thread, and return when nothing is left. Requests may reach private addresses
    /// (a local Ollama) unless `allow_private` is false. `secrets` maps the names an
    /// `apiKey` may give to `env:VARIABLE` or `file:PATH`. Raises `QueryTimeoutError`
    /// when `timeout` seconds pass with work left, as when the provider keeps failing.
    #[pyo3(signature = (*, timeout = 3600.0, allow_private = true, secrets = None))]
    fn embed(
        &self,
        py: Python<'_>,
        timeout: f64,
        allow_private: bool,
        secrets: Option<std::collections::BTreeMap<String, String>>,
    ) -> PyResult<()> {
        if !(timeout.is_finite() && timeout > 0.0) {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "timeout must be a positive number of seconds",
            ));
        }
        let mut env = sparkles::vector::embed::Environment {
            outbound: sparkles::outbound::OutboundPolicy {
                allow_private,
                ..Default::default()
            },
            ..Default::default()
        };
        for (name, src) in secrets.unwrap_or_default() {
            let src = src
                .parse()
                .map_err(|e: String| pyo3::exceptions::PyValueError::new_err(e))?;
            env.secrets.insert(name, src);
        }
        let ds = self.ds_for_write(py)?;
        ds.store().set_embedding_environment(Some(env));
        py.detach(|| {
            ds.store()
                .embed_until_idle(std::time::Duration::from_secs_f64(timeout))
        })
        .py(py)
    }

    /// The status of a vector index, or `None`; with `wait`, once its build is done.
    #[pyo3(signature = (name, *, wait = false))]
    fn vector_index<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        wait: bool,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        admin::vector_index(py, &self.ds(py)?, name, wait)
    }

    /// The status of every vector index.
    fn vector_indexes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        admin::vector_indexes(py, &self.ds(py)?)
    }

    // ---------------------------------------------------------- write validation ----

    /// Set, replace or (with `None`) remove write-time validation, from a dict like
    /// `validation.json` with the shapes or schema given inline. Returns the outcome
    /// and the validation of the current state.
    #[pyo3(signature = (config))]
    fn set_write_validation<'py>(
        &self,
        py: Python<'py>,
        config: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
        let ds = self.ds_for_write(py)?;
        let (out, guard) = admin::set_write_validation(py, &ds, config)?;
        if let Some(g) = guard {
            ds.set_write_guard(g);
        }
        Ok(out)
    }

    /// The installed write-time validation's configuration and status, or `None`.
    fn write_validation<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<Option<Bound<'py, pyo3::types::PyDict>>> {
        let g = self.ds(py)?.write_guard();
        g.as_ref().map(|g| admin::guard_status(py, g)).transpose()
    }
}
