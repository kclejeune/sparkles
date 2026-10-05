//! Term checks, syntax checks, geometry conversion and schedule previews.
use crate::{
    admin,
    errors::{EngineResult, invalid},
};
use pyo3::prelude::*;
use std::collections::BTreeMap;
#[pyfunction]
fn check_iri<'py>(py: Python<'py>, iri: &str) -> PyResult<Bound<'py, PyAny>> {
    let c = sparkles::terms::check_iri(iri);
    admin::to_py(
        py,
        &serde_json::json!({"errors":c.errors,"warnings":c.warnings}),
    )
}
#[pyfunction]
fn check_langtag<'py>(py: Python<'py>, tag: &str) -> PyResult<Bound<'py, PyAny>> {
    let c = sparkles::terms::check_langtag(tag);
    admin::to_py(
        py,
        &serde_json::json!({"error":c.error,"canonical":c.canonical,"language":c.language,"script":c.script,"region":c.region,"variant":c.variant,"extension":c.extension,"privateUse":c.private_use}),
    )
}
#[pyfunction]
#[pyo3(signature=(text,format,*,base_iri=None))]
fn check_data<'py>(
    py: Python<'py>,
    text: &str,
    format: &str,
    base_iri: Option<&str>,
) -> PyResult<Bound<'py, PyAny>> {
    let syntax = sparkles::io::DataSyntax::from_name(format)
        .ok_or_else(|| invalid(py, format!("unknown RDF format: {format}")))?;
    if let Some(base) = base_iri {
        oxrdf::NamedNode::new(base).map_err(|e| invalid(py, e.to_string()))?;
    }
    admin::to_py(
        py,
        &py.detach(|| sparkles::io::check_data(syntax, text, base_iri)),
    )
}
#[pyfunction]
#[pyo3(signature=(query,*,base_iri=None,prefixes=None))]
fn parse_query(
    py: Python<'_>,
    query: &str,
    base_iri: Option<&str>,
    prefixes: Option<BTreeMap<String, String>>,
) -> PyResult<String> {
    let prefixes = prefixes.unwrap_or_default().into_iter().collect::<Vec<_>>();
    Ok(py
        .detach(|| sparkles::sparql::parse_query(query, base_iri, &prefixes))
        .py(py)?
        .to_string())
}
#[pyfunction]
#[pyo3(signature=(update,*,base_iri=None,prefixes=None))]
fn parse_update(
    py: Python<'_>,
    update: &str,
    base_iri: Option<String>,
    prefixes: Option<BTreeMap<String, String>>,
) -> PyResult<String> {
    let opts = sparkles::sparql::QueryOptions {
        base_iri,
        prefixes: prefixes.unwrap_or_default().into_iter().collect(),
        ..Default::default()
    };
    Ok(py
        .detach(|| sparkles::sparql::update::parse_update(update, &opts))
        .py(py)?
        .to_string())
}
#[pyfunction]
fn convert_geometries<'py>(
    py: Python<'py>,
    items: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    #[cfg(feature = "geo")]
    {
        let items: Vec<sparkles::geo::convert::ConvertItem> =
            admin::from_py(items, "geometry literals")?;
        admin::to_py(
            py,
            &py.detach(|| sparkles::geo::convert::convert(&items))
                .py(py)?,
        )
    }
    #[cfg(not(feature = "geo"))]
    {
        let _ = items;
        Err(crate::errors::new_err(
            py,
            "UnsupportedError",
            "geo feature is disabled",
        ))
    }
}
#[pyfunction]
#[pyo3(signature=(schedule,timezone="UTC",n=5,*,after=None))]
fn preview_schedule(
    py: Python<'_>,
    schedule: &str,
    timezone: &str,
    n: usize,
    after: Option<&str>,
) -> PyResult<Vec<String>> {
    #[cfg(feature = "backup")]
    {
        use sparkles::backup::policy::{next_runs, parse_schedule, parse_timezone};
        let map = |e| crate::errors::engine(py, sparkles::backup::error(e));
        let schedule = parse_schedule(schedule).map_err(map)?;
        let tz = parse_timezone(timezone).map_err(map)?;
        let now = after
            .map(ToOwned::to_owned)
            .unwrap_or_else(sparkles::backup::now_rfc3339);
        let after = now
            .parse()
            .map_err(|e| invalid(py, format!("invalid after time: {e}")))?;
        Ok(py
            .detach(|| next_runs(&schedule, tz, after, n))
            .into_iter()
            .map(|t| t.to_rfc3339())
            .collect())
    }
    #[cfg(not(feature = "backup"))]
    {
        let _ = (schedule, timezone, n, after);
        Err(crate::errors::new_err(
            py,
            "UnsupportedError",
            "backup feature is disabled",
        ))
    }
}
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(check_iri, m)?)?;
    m.add_function(wrap_pyfunction!(check_langtag, m)?)?;
    m.add_function(wrap_pyfunction!(check_data, m)?)?;
    m.add_function(wrap_pyfunction!(parse_query, m)?)?;
    m.add_function(wrap_pyfunction!(parse_update, m)?)?;
    m.add_function(wrap_pyfunction!(convert_geometries, m)?)?;
    m.add_function(wrap_pyfunction!(preview_schedule, m)?)?;
    Ok(())
}
