//! R2RML, RML and YARRRML mappings: stored mappings and their runs, and runs of mapping
//! texts (spec F19).
use crate::{admin, errors::EngineResult, handles::handle};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use sparkles::mapping::{self, Connection, Inputs, MappingKind, NamedConnection, RunOptions};
use std::collections::BTreeMap;
use std::path::PathBuf;

handle!(PyStoredMappings, "StoredMappings");

fn kind_of(kind: Option<&str>) -> PyResult<Option<MappingKind>> {
    kind.map(|k| {
        MappingKind::parse(k).ok_or_else(|| {
            PyValueError::new_err(format!("kind {k:?}: use rml, yarrrml, csvw or template"))
        })
    })
    .transpose()
}

/// The inputs of a run: files by the names the mapping gives them, connections by name
/// with their URLs (`sqlite:PATH`), and a directory for the other relative names.
pub(crate) fn inputs(
    sources: Option<BTreeMap<String, PathBuf>>,
    connections: Option<BTreeMap<String, String>>,
    directory: Option<PathBuf>,
) -> PyResult<Inputs> {
    let mut conns = Vec::new();
    for (name, url) in connections.unwrap_or_default() {
        let connection = Connection::from_url(&url)
            .map_err(|e| PyValueError::new_err(format!("connection {name}: {e}")))?;
        conns.push(NamedConnection {
            name,
            connection,
            jdbc_dsn: None,
        });
    }
    Ok(Inputs {
        files: sources.unwrap_or_default().into_iter().collect(),
        dir: directory,
        connections: conns,
    })
}

fn run_options(base: Option<String>) -> RunOptions {
    RunOptions {
        base,
        ..Default::default()
    }
}

fn run_json<'py>(
    py: Python<'py>,
    r: &sparkles::handles::MappingRun,
) -> PyResult<Bound<'py, PyAny>> {
    admin::to_py(
        py,
        &serde_json::json!({ "receipt": r.receipt, "stats": r.stats }),
    )
}

#[pymethods]
impl PyStoredMappings {
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let rows: Vec<serde_json::Value> = py
            .detach(|| ds.mappings().list())
            .into_iter()
            .map(|(name, s, last)| {
                serde_json::json!({
                    "name": name,
                    "kind": s.definition.kind,
                    "description": s.definition.description,
                    "version": s.version,
                    "lastRun": last,
                })
            })
            .collect();
        admin::to_py(py, &rows)
    }
    #[pyo3(signature=(name,*,version=None))]
    fn get<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        version: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.mappings().get(name, version)))
    }
    fn versions<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.mappings().versions(name)))
    }
    #[pyo3(signature=(name,text,*,kind=None,description=None,author=None,message=None,if_version=None))]
    #[allow(clippy::too_many_arguments)]
    fn put<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        text: String,
        kind: Option<&str>,
        description: Option<String>,
        author: Option<String>,
        message: Option<String>,
        if_version: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        let kind = match kind_of(kind)? {
            Some(k) => k,
            None => mapping::detect(&text).ok_or_else(|| {
                PyValueError::new_err(
                    "the text is not CSVW metadata, R2RML, RML, YARRRML or a CONSTRUCT template; give kind",
                )
            })?,
        };
        let def = sparkles::stored_mappings::Definition {
            kind: kind.name().to_string(),
            text,
            description,
        };
        let change = sparkles::stored_mappings::Change {
            author,
            message,
            if_version,
            dataset_commit: Some(ds.head_commit().seq),
        };
        let r = py.detach(|| ds.mappings().put(name, def, change)).py(py)?;
        admin::to_py(
            py,
            &serde_json::json!({
                "stored": r.stored,
                "summary": r.summary,
                "created": r.created,
                "changed": r.changed,
            }),
        )
    }
    #[pyo3(signature=(name,*,if_version=None))]
    fn delete(&self, py: Python<'_>, name: &str, if_version: Option<u64>) -> PyResult<bool> {
        let ds = self.write(py)?;
        py.detach(|| ds.mappings().delete(name, if_version)).py(py)
    }
    #[pyo3(signature=(name,*,version=None,sources=None,connections=None,directory=None,base=None,replace=false,author=None,message=None))]
    #[allow(clippy::too_many_arguments)]
    fn run<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        version: Option<u64>,
        sources: Option<BTreeMap<String, PathBuf>>,
        connections: Option<BTreeMap<String, String>>,
        directory: Option<PathBuf>,
        base: Option<String>,
        replace: bool,
        author: Option<String>,
        message: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        let ins = inputs(sources, connections, directory)?;
        let opts = run_options(base);
        let o = sparkles::guard::WriteOptions {
            author: author.map(Into::into),
            message: message.map(Into::into),
            ..Default::default()
        };
        let r = py
            .detach(|| ds.mappings().run(name, version, &ins, &opts, replace, &o))
            .py(py)?;
        run_json(py, &r)
    }
}

/// `Dataset.run_mapping`: run a mapping text and load its quads in one commit.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_text<'py>(
    py: Python<'py>,
    ds: sparkles::Dataset,
    text: &str,
    kind: Option<&str>,
    sources: Option<BTreeMap<String, PathBuf>>,
    connections: Option<BTreeMap<String, String>>,
    directory: Option<PathBuf>,
    base: Option<String>,
    graph: Option<String>,
) -> PyResult<Bound<'py, PyAny>> {
    let kind = kind_of(kind)?;
    let ins = inputs(sources, connections, directory)?;
    let opts = run_options(base);
    let r = py
        .detach(|| ds.run_mapping(text, kind, &ins, &opts, graph.as_deref()))
        .py(py)?;
    run_json(py, &r)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStoredMappings>()?;
    Ok(())
}
