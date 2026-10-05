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
#[pyclass(frozen, module = "sparkles", name = "Dataset", weakref)]
pub struct PyDataset {
    inner: RwLock<Option<sparkles::Dataset>>,
    path: Option<String>,
    writer: WriterSlot,
    pub(crate) group: Option<crate::catalog::DatasetGroup>,
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
    pub(crate) fn from_dataset(ds: sparkles::Dataset, path: Option<String>) -> PyDataset {
        let writer = writer_slot(&ds);
        PyDataset {
            inner: RwLock::new(Some(ds)),
            path,
            writer,
            group: None,
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
    pub(crate) fn ds(&self, py: Python<'_>) -> PyResult<sparkles::Dataset> {
        self.inner
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| invalid(py, "the dataset is closed"))
    }

    /// The handle for an operation that takes the writer lock, including captures.
    pub(crate) fn ds_for_write(&self, py: Python<'_>) -> PyResult<sparkles::Dataset> {
        let ds = self.ds(py)?;
        check_transaction(py, &ds)?;
        Ok(ds)
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
    pub(crate) fn close(&self, py: Python<'_>) {
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
    #[pyo3(signature=(*,cancel=None,progress=None,timeout=None))]
    fn compact<'py>(
        &self,
        py: Python<'py>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds_for_write(py)?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            ds.compact_with(&Default::default(), &ctl)
        })?;
        admin::to_py(py, &r)
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

    // ------------------------------------------------------- history, snapshots ----

    /// The latest commit.
    #[getter]
    fn head_commit(&self, py: Python<'_>) -> PyResult<admin::PyCommit> {
        Ok(admin::head_commit(&self.ds(py)?))
    }

    /// Copy a consistent state to an unregistered persistent directory.
    #[pyo3(signature=(directory,*,at=None,graphs=None,inferences="copy",cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn clone_to<'py>(
        &self,
        py: Python<'py>,
        directory: PathBuf,
        at: Option<&Bound<'py, PyAny>>,
        graphs: Option<Vec<String>>,
        inferences: &str,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds_for_write(py)?;
        let spec = sparkles::cloning::Spec {
            at: at.map(at_from_py).transpose()?,
            graphs,
            inferences: sparkles::cloning::Inferences::parse(inferences)
                .ok_or_else(|| invalid(py, "inferences must be copy or drop"))?,
            ..Default::default()
        };
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            ds.clone_to_with(&directory, &spec, &ctl)
        })?;
        admin::to_py(
            py,
            &serde_json::json!({"datasetId":r.dataset_id,"commit":r.forked_from.seq,"sourceQuads":r.source_quads,"quads":r.quads,"graphs":r.graphs,"millis":r.millis,"method":r.method.name()}),
        )
    }
    #[pyo3(signature=(*,at=None,graphs=None,inferences="copy",cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn clone_to_memory(
        &self,
        py: Python<'_>,
        at: Option<&Bound<'_, PyAny>>,
        graphs: Option<Vec<String>>,
        inferences: &str,
        cancel: Option<&Bound<'_, PyAny>>,
        progress: Option<&Bound<'_, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Py<Self>> {
        let ds = self.ds_for_write(py)?;
        let spec = sparkles::cloning::Spec {
            at: at.map(at_from_py).transpose()?,
            graphs,
            inferences: sparkles::cloning::Inferences::parse(inferences)
                .ok_or_else(|| invalid(py, "inferences must be copy or drop"))?,
            ..Default::default()
        };
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            ds.clone_to_memory_with("copy", &spec, &ctl)
        })?;
        let cloned = sparkles::Dataset::from_store(r.store);
        cloned.state().set_reasoning(r.reasoning).py(py)?;
        crate::catalog::owned(py, cloned, None, self.group.clone())
    }

    #[getter]
    fn snapshots(slf: Py<Self>) -> crate::handles::PySnapshots {
        crate::handles::PySnapshots { owner: slf }
    }
    #[getter]
    fn history(slf: Py<Self>) -> crate::handles::PyHistory {
        crate::handles::PyHistory { owner: slf }
    }
    #[getter]
    fn settings(slf: Py<Self>) -> crate::handles::PySettings {
        crate::handles::PySettings { owner: slf }
    }

    #[getter]
    fn validation(slf: Py<Self>) -> crate::validation::PyValidation {
        crate::validation::PyValidation { owner: slf }
    }

    #[getter]
    fn indexes(slf: Py<Self>) -> crate::indexes::PyIndexes {
        crate::indexes::PyIndexes { owner: slf }
    }

    #[getter]
    fn reasoning(slf: Py<Self>) -> crate::reasoning::PyReasoning {
        crate::reasoning::PyReasoning { owner: slf }
    }

    #[getter]
    fn dataset_id(&self, py: Python<'_>) -> PyResult<String> {
        Ok(self.ds(py)?.dataset_id().to_string())
    }
    fn remove_prefix(&self, py: Python<'_>, prefix: &str) -> PyResult<bool> {
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.remove_prefix(prefix)).py(py)
    }
    fn clear_cache(&self, py: Python<'_>) -> PyResult<()> {
        let ds = self.ds(py)?;
        py.detach(|| ds.clear_cache());
        Ok(())
    }
    #[pyo3(signature=(*,at=None,cancel=None,progress=None,timeout=None))]
    fn stats<'py>(
        &self,
        py: Python<'py>,
        at: Option<&Bound<'py, PyAny>>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let at = at.map(at_from_py).transpose()?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let r = ds.stats(&sparkles::stats::StatsOptions {
                at,
                cancel: Some(ctl.cancel.flag()),
                deadline: ctl.deadline,
            })?;
            ctl.progress.report(1.0, "read statistics");
            Ok(r)
        })?;
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(query,*,base_iri=None,prefixes=None))]
    fn explain<'py>(
        &self,
        py: Python<'py>,
        query: &str,
        base_iri: Option<String>,
        prefixes: Option<BTreeMap<String, String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let opts = QueryOptions {
            base_iri,
            prefixes: prefixes.unwrap_or_default().into_iter().collect(),
            ..Default::default()
        };
        let (plan, info) = py.detach(|| ds.explain(query, &opts)).py(py)?;
        let out = PyDict::new(py);
        out.set_item("plan", plan)?;
        out.set_item("summary", admin::to_py(py, &info)?)?;
        Ok(out.into_any())
    }
    #[pyo3(signature=(input=None,*,format=None,path=None,target=None,base_iri=None,compression=None,lenient=false,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn replace<'py>(
        &self,
        py: Python<'py>,
        input: Option<&Bound<'py, PyAny>>,
        format: Option<&Bound<'py, PyAny>>,
        path: Option<PathBuf>,
        target: Option<&Bound<'py, PyAny>>,
        base_iri: Option<String>,
        compression: Option<&str>,
        lenient: bool,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds_for_write(py)?;
        let target = match target {
            None => sparkles::store::ReplaceTarget::All,
            Some(g) if g.is_none() => sparkles::store::ReplaceTarget::Default,
            Some(g) => match graph_from_py(g)? {
                GraphName::DefaultGraph => sparkles::store::ReplaceTarget::Default,
                GraphName::NamedNode(n) => sparkles::store::ReplaceTarget::Named(n),
                _ => return Err(invalid(py, "target cannot be a blank node")),
            },
        };
        let (src, spool) = source_from_py(
            py,
            input,
            format,
            path,
            base_iri,
            None,
            compression,
            lenient,
        )?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let _spool = spool;
            ctl.progress.report(0.0, "replacing data");
            ctl.check()?;
            let r = ds.replace(target, &[src])?;
            ctl.progress.report(1.0, "replaced data");
            Ok(r)
        })?;
        admin::to_py(py, &r)
    }

    #[getter]
    fn queries(slf: Py<Self>) -> crate::queries::PyStoredQueries {
        crate::queries::PyStoredQueries { owner: slf }
    }

    #[getter]
    fn schema(slf: Py<Self>) -> crate::schema::PySchema {
        crate::schema::PySchema { owner: slf }
    }

    #[getter]
    fn graphql(slf: Py<Self>) -> crate::graphql::PyGraphQl {
        crate::graphql::PyGraphQl { owner: slf }
    }

    #[cfg(feature = "backup")]
    fn backups(
        slf: Py<Self>,
        repository: Py<crate::backups::PyBackupRepository>,
    ) -> crate::backups::PyBackups {
        crate::backups::PyBackups {
            owner: slf,
            repository,
        }
    }

    #[getter]
    fn branches(slf: Py<Self>) -> crate::branches::PyBranches {
        crate::branches::PyBranches { owner: slf }
    }
    fn branch(&self, py: Python<'_>, name: &str) -> PyResult<Py<Self>> {
        let ds = self.ds(py)?;
        let ds = py.detach(|| ds.branch(name)).py(py)?;
        let path = ds.store().root().map(|p| p.display().to_string());
        crate::catalog::owned(py, ds, path, self.group.clone())
    }
}

/// Dataset aliases share the transaction owner slot while any alias or transaction
/// lives, so an alias cannot wait for a writer lock held by its own Python thread.
fn writer_slot(ds: &sparkles::Dataset) -> WriterSlot {
    use std::sync::{OnceLock, Weak};
    type Slots = std::collections::HashMap<usize, Weak<Mutex<Option<std::thread::ThreadId>>>>;
    static SLOTS: OnceLock<Mutex<Slots>> = OnceLock::new();
    let mut slots = SLOTS.get_or_init(Default::default).lock().unwrap();
    slots.retain(|_, v| v.strong_count() > 0);
    let key = ds.state() as *const _ as usize;
    if let Some(slot) = slots.get(&key).and_then(Weak::upgrade) {
        return slot;
    }
    let slot = Arc::new(Mutex::new(None));
    slots.insert(key, Arc::downgrade(&slot));
    slot
}

/// Reject waiting for a store lock owned by this Python thread's transaction.
/// Checking the shared state also covers aliases obtained through a catalog.
pub(crate) fn check_transaction(py: Python<'_>, ds: &sparkles::Dataset) -> PyResult<()> {
    if *writer_slot(ds).lock().unwrap() == Some(std::thread::current().id()) {
        return Err(new_err(
            py,
            "ConflictError",
            "this thread has an open transaction on the dataset: finish the transaction before this operation",
        ));
    }
    Ok(())
}
