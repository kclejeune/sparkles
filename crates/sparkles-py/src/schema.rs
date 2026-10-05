//! Schema discovery, comparison, profiles, draft shapes and VoID.
use crate::{admin, errors::invalid, handles::handle, interrupt};
use pyo3::prelude::*;
use sparkles::handles::ReportRequest;
use sparkles::schema::{GraphSelection, SchemaOptions};
handle!(PySchema, "Schema");
fn options(
    graph: &str,
    declared_graph: Option<&str>,
    include_inferred: bool,
    max_entries: usize,
    ctl: &sparkles::task::Control,
) -> sparkles::Result<SchemaOptions> {
    Ok(SchemaOptions {
        graph: GraphSelection::parse(graph).map_err(sparkles::Error::invalid)?,
        declared_graph: declared_graph
            .map(GraphSelection::parse)
            .transpose()
            .map_err(sparkles::Error::invalid)?,
        inferred_graph: Some(crate::dataset::INFERRED_GRAPH.into()),
        include_inferred,
        max_entries,
        cancel: Some(ctl.cancel.flag()),
        deadline: ctl.deadline,
        ..Default::default()
    })
}
#[pymethods]
impl PySchema {
    #[pyo3(signature=(*,graph="default".to_string(),declared_graph=None,include_inferred=true,at=None,limit=1000,cursor=None,max_entries=100000,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn report<'py>(
        &self,
        py: Python<'py>,
        graph: String,
        declared_graph: Option<String>,
        include_inferred: bool,
        at: Option<&Bound<'py, PyAny>>,
        limit: usize,
        cursor: Option<String>,
        max_entries: usize,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let at = at.map(crate::dataset::at_from_py).transpose()?;
        let j = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let req = ReportRequest {
                options: options(
                    &graph,
                    declared_graph.as_deref(),
                    include_inferred,
                    max_entries,
                    &ctl,
                )?,
                at,
                limit,
                cursor,
                timeout: ctl
                    .deadline
                    .map(|d| d.saturating_duration_since(std::time::Instant::now())),
            };
            let r = ds.schema().report(&req)?;
            let j = serde_json::to_value(r.summary(ds.name().unwrap_or("dataset")))
                .map_err(|e| sparkles::Error::invalid(e.to_string()))?;
            ctl.progress.report(1.0, "computed schema");
            Ok(j)
        })?;
        admin::to_py(py, &j)
    }
    #[pyo3(signature=(*,graph="default".to_string(),declared_graph=None,include_inferred=true,at=None,limit=1000,cursor=None,max_entries=100000,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn classes<'py>(
        &self,
        py: Python<'py>,
        graph: String,
        declared_graph: Option<String>,
        include_inferred: bool,
        at: Option<&Bound<'py, PyAny>>,
        limit: usize,
        cursor: Option<String>,
        max_entries: usize,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let at = at.map(crate::dataset::at_from_py).transpose()?;
        let j = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let req = ReportRequest {
                options: options(
                    &graph,
                    declared_graph.as_deref(),
                    include_inferred,
                    max_entries,
                    &ctl,
                )?,
                at,
                limit,
                cursor,
                timeout: ctl
                    .deadline
                    .map(|d| d.saturating_duration_since(std::time::Instant::now())),
            };
            let r = ds.schema().report(&req)?;
            let j = serde_json::to_value(r.classes())
                .map_err(|e| sparkles::Error::invalid(e.to_string()))?;
            ctl.progress.report(1.0, "computed schema");
            Ok(j)
        })?;
        admin::to_py(py, &j)
    }
    #[pyo3(signature=(*,graph="default".to_string(),declared_graph=None,include_inferred=true,at=None,limit=1000,cursor=None,max_entries=100000,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn predicates<'py>(
        &self,
        py: Python<'py>,
        graph: String,
        declared_graph: Option<String>,
        include_inferred: bool,
        at: Option<&Bound<'py, PyAny>>,
        limit: usize,
        cursor: Option<String>,
        max_entries: usize,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let at = at.map(crate::dataset::at_from_py).transpose()?;
        let j = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let req = ReportRequest {
                options: options(
                    &graph,
                    declared_graph.as_deref(),
                    include_inferred,
                    max_entries,
                    &ctl,
                )?,
                at,
                limit,
                cursor,
                timeout: ctl
                    .deadline
                    .map(|d| d.saturating_duration_since(std::time::Instant::now())),
            };
            let r = ds.schema().report(&req)?;
            let j = serde_json::to_value(r.predicates())
                .map_err(|e| sparkles::Error::invalid(e.to_string()))?;
            ctl.progress.report(1.0, "computed schema");
            Ok(j)
        })?;
        admin::to_py(py, &j)
    }
    #[pyo3(signature=(from_commit,to_commit=None,*,graph="default".to_string(),declared_graph=None,include_inferred=true,limit=1000,cursor=None,max_entries=100000,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn diff<'py>(
        &self,
        py: Python<'py>,
        from_commit: &Bound<'py, PyAny>,
        to_commit: Option<&Bound<'py, PyAny>>,
        graph: String,
        declared_graph: Option<String>,
        include_inferred: bool,
        limit: usize,
        cursor: Option<String>,
        max_entries: usize,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let from = crate::dataset::at_from_py(from_commit)?;
        let at = to_commit.map(crate::dataset::at_from_py).transpose()?;
        let j = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let req = ReportRequest {
                options: options(
                    &graph,
                    declared_graph.as_deref(),
                    include_inferred,
                    max_entries,
                    &ctl,
                )?,
                at,
                limit,
                cursor,
                timeout: ctl
                    .deadline
                    .map(|d| d.saturating_duration_since(std::time::Instant::now())),
            };
            let (r, _) = ds.schema().diff(&from, &req)?;
            let j = serde_json::to_value(r).map_err(|e| sparkles::Error::invalid(e.to_string()))?;
            ctl.progress.report(1.0, "computed schema");
            Ok(j)
        })?;
        admin::to_py(py, &j)
    }

    #[pyo3(signature=(*,classes=None,graph="default".to_string(),include_inferred=true,at=None,max_entries=100000,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn profiles<'py>(
        &self,
        py: Python<'py>,
        classes: Option<Vec<String>>,
        graph: String,
        include_inferred: bool,
        at: Option<&Bound<'py, PyAny>>,
        max_entries: usize,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let at = at.map(crate::dataset::at_from_py).transpose()?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let opts = sparkles::schema::profile::ProfileOptions {
                schema: options(&graph, None, include_inferred, max_entries, &ctl)?,
                classes: classes.unwrap_or_default(),
            };
            let r = ds.schema().profiles_at(&opts, at.as_ref())?;
            ctl.progress.report(1.0, "computed profiles");
            Ok(r)
        })?;
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(*,format="turtle",classes=None,support=1.0,min_instances=1,max_in=20,max_count=10,closed=false,base=None,graph="default".to_string(),include_inferred=true,at=None,max_entries=100000,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn draft_shapes<'py>(
        &self,
        py: Python<'py>,
        format: &str,
        classes: Option<Vec<String>>,
        support: f64,
        min_instances: u64,
        max_in: usize,
        max_count: u64,
        closed: bool,
        base: Option<String>,
        graph: String,
        include_inferred: bool,
        at: Option<&Bound<'py, PyAny>>,
        max_entries: usize,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        if !matches!(format, "json" | "turtle" | "shaclc" | "shex") {
            return Err(invalid(py, "format must be turtle, json, shaclc or shex"));
        }
        let ds = self.ds(py)?;
        let at = at.map(crate::dataset::at_from_py).transpose()?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let mut opts = sparkles::schema::draft::DraftOptions {
                schema: options(&graph, None, include_inferred, max_entries, &ctl)?,
                dataset: ds.name().unwrap_or("dataset").into(),
                classes: classes.unwrap_or_default(),
                support,
                min_instances,
                max_in,
                max_count,
                closed,
                prefixes: ds.prefixes().into_iter().collect(),
                ..Default::default()
            };
            if let Some(base) = base {
                opts.base = base;
            }
            let r = ds.schema().draft_shapes_at(&opts, at.as_ref())?;
            ctl.progress.report(1.0, "drafted shapes");
            Ok(r)
        })?;
        match format {
            "json" => admin::to_py(py, &r),
            "shaclc" => Ok(r.shaclc.into_pyobject(py)?.into_any()),
            "shex" => Ok(r.shex.into_pyobject(py)?.into_any()),
            _ => Ok(r.shacl.into_pyobject(py)?.into_any()),
        }
    }
    #[pyo3(signature=(*,graph="default".to_string(),include_inferred=true,at=None,declarations=true,max_entries=100000,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn void(
        &self,
        py: Python<'_>,
        graph: String,
        include_inferred: bool,
        at: Option<&Bound<'_, PyAny>>,
        declarations: bool,
        max_entries: usize,
        cancel: Option<&Bound<'_, PyAny>>,
        progress: Option<&Bound<'_, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<String> {
        let ds = self.ds(py)?;
        let at = at.map(crate::dataset::at_from_py).transpose()?;
        interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let req = ReportRequest {
                options: options(&graph, None, include_inferred, max_entries, &ctl)?,
                at,
                ..Default::default()
            };
            let opts = sparkles::schema::VoidOptions {
                dataset: ds.name().unwrap_or("dataset"),
                declarations,
                prefixes: ds.prefixes().into_iter().collect(),
            };
            let triples = ds.schema().void(&req, &opts)?;
            let mut out = oxrdfio::RdfSerializer::from_format(oxrdfio::RdfFormat::Turtle)
                .for_writer(Vec::new());
            for t in &triples {
                ctl.check()?;
                out.serialize_triple(t)?;
            }
            let bytes = out.finish()?;
            ctl.progress.report(1.0, "exported VoID");
            String::from_utf8(bytes).map_err(|e| sparkles::Error::invalid(e.to_string()))
        })
    }
    #[pyo3(signature=(*,shapes=None,at=None,cancel=None,progress=None,timeout=None))]
    fn constraints<'py>(
        &self,
        py: Python<'py>,
        shapes: Option<Vec<String>>,
        at: Option<&Bound<'py, PyAny>>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let at = at.map(crate::dataset::at_from_py).transpose()?;
        let shapes = sparkles::handles::ShapesRequest::from_values(&shapes.unwrap_or_default())
            .map_err(|e| invalid(py, e))?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let r = ds
                .schema()
                .constraints(&sparkles::handles::ConstraintsRequest {
                    shapes,
                    at,
                    timeout: ctl
                        .deadline
                        .map(|d| d.saturating_duration_since(std::time::Instant::now())),
                    ..Default::default()
                })?;
            ctl.check()?;
            ctl.progress.report(1.0, "read constraints");
            Ok(r)
        })?;
        admin::to_py(py, &r)
    }
}
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySchema>()?;
    Ok(())
}
