//! Full-text, vector and spatial administration.
use crate::terms::named_node_from_py;
use crate::{
    admin,
    errors::{EngineResult, invalid},
    handles::handle,
    interrupt,
};
use pyo3::prelude::*;
use std::collections::BTreeMap;
handle!(PyIndexes, "Indexes");
handle!(PyTextIndex, "TextIndex");
handle!(PyVectorIndexes, "VectorIndexes");
handle!(PyGeoIndex, "GeoIndex");
#[pymethods]
impl PyIndexes {
    #[getter]
    fn text(&self, py: Python<'_>) -> PyTextIndex {
        PyTextIndex {
            owner: self.owner.clone_ref(py),
        }
    }
    #[getter]
    fn vector(&self, py: Python<'_>) -> PyVectorIndexes {
        PyVectorIndexes {
            owner: self.owner.clone_ref(py),
        }
    }
    #[getter]
    fn geo(&self, py: Python<'_>) -> PyGeoIndex {
        PyGeoIndex {
            owner: self.owner.clone_ref(py),
        }
    }
}
#[pymethods]
impl PyTextIndex {
    fn status<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.indexes().text().status()))
    }
    fn enabled(&self, py: Python<'_>) -> PyResult<bool> {
        Ok(self.ds(py)?.indexes().text().enabled())
    }
    #[pyo3(signature=(config=None,*,cancel=None,progress=None,timeout=None))]
    fn enable<'py>(
        &self,
        py: Python<'py>,
        config: Option<&Bound<'py, PyAny>>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        let cfg = config
            .map(|v| admin::from_py(v, "text index"))
            .transpose()?
            .unwrap_or_default();
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            ctl.progress.report(0.0, "building text index");
            ctl.check()?;
            let r = ds.indexes().text().enable(cfg)?;
            ctl.check()?;
            ctl.progress.report(1.0, "built text index");
            Ok(r)
        })?;
        admin::to_py(py, &r)
    }
    fn disable(&self, py: Python<'_>) -> PyResult<()> {
        let ds = self.write(py)?;
        py.detach(|| ds.indexes().text().disable()).py(py)
    }
    #[pyo3(signature=(*,cancel=None,progress=None,timeout=None))]
    fn rebuild<'py>(
        &self,
        py: Python<'py>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let r = ds.indexes().text().rebuild()?;
            ctl.check()?;
            ctl.progress.report(1.0, "rebuilt text index");
            Ok(r)
        })?;
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(query,*,predicates=None,lang=None,graph=None,limit=20,highlight=true,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn search<'py>(
        &self,
        py: Python<'py>,
        query: String,
        predicates: Option<&Bound<'py, PyAny>>,
        lang: Option<String>,
        graph: Option<&Bound<'py, PyAny>>,
        limit: usize,
        highlight: bool,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let predicates = predicates
            .map(|p| {
                p.try_iter()?
                    .map(|v| named_node_from_py(&v?))
                    .collect::<PyResult<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        let graph = graph.map(named_node_from_py).transpose()?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let req = sparkles::handles::TextSearch {
                query,
                predicates,
                lang,
                graph,
                limit,
                highlight,
                options: sparkles::sparql::QueryOptions {
                    cancel: Some(ctl.cancel.flag()),
                    timeout: ctl
                        .deadline
                        .map(|d| d.saturating_duration_since(std::time::Instant::now())),
                    ..Default::default()
                },
            };
            let r = ds.indexes().text().search(&req)?;
            ctl.progress.report(1.0, "searched text index");
            Ok(r)
        })?;
        admin::to_py(py, &r)
    }
}
#[pymethods]
impl PyVectorIndexes {
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.indexes().vector().list()))
    }
    fn get<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.indexes().vector().get(name)))
    }
    #[pyo3(signature=(name,config,*,cancel=None,progress=None,timeout=None))]
    fn put(
        &self,
        py: Python<'_>,
        name: String,
        config: &Bound<'_, PyAny>,
        cancel: Option<&Bound<'_, PyAny>>,
        progress: Option<&Bound<'_, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<bool> {
        let ds = self.write(py)?;
        let cfg = admin::from_py(config, "vector index")?;
        interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let r = ds.indexes().vector().put(&name, cfg)?;
            ctl.progress.report(1.0, "started vector index build");
            Ok(r)
        })
    }
    fn drop(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        let ds = self.write(py)?;
        py.detach(|| ds.indexes().vector().drop(name)).py(py)
    }
    fn rebuild(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        let ds = self.write(py)?;
        py.detach(|| ds.indexes().vector().rebuild(name)).py(py)
    }
    fn reembed(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        let ds = self.write(py)?;
        py.detach(|| ds.indexes().vector().reembed(name)).py(py)
    }
    fn embedding_status<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &ds.indexes().vector().embedding_status(name))
    }
    #[pyo3(signature=(name,*,cancel=None,progress=None,timeout=None))]
    fn wait<'py>(
        &self,
        py: Python<'py>,
        name: String,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            loop {
                ctl.check()?;
                let s = ds.indexes().vector().get(&name);
                if s.as_ref().is_none_or(|s| s.state != "building") {
                    ctl.progress.report(1.0, "vector build finished");
                    return Ok(s);
                }
                if let Some(s) = &s {
                    ctl.progress.report(
                        s.progress.unwrap_or(0.0),
                        s.message.as_deref().unwrap_or("building vector index"),
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })?;
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(name,*,samples=100,k=10,ef=None,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn recall<'py>(
        &self,
        py: Python<'py>,
        name: String,
        samples: usize,
        k: usize,
        ef: Option<usize>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let r = ds
                .indexes()
                .vector()
                .recall(&name, &sparkles::handles::RecallOptions { samples, k, ef })?;
            ctl.check()?;
            ctl.progress.report(1.0, "measured recall");
            Ok(r)
        })?;
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(*,timeout=3600.0,allow_private=true,secrets=None,cancel=None,progress=None))]
    #[allow(clippy::too_many_arguments)]
    fn embed_until_idle(
        &self,
        py: Python<'_>,
        timeout: f64,
        allow_private: bool,
        secrets: Option<BTreeMap<String, String>>,
        cancel: Option<&Bound<'_, PyAny>>,
        progress: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let ds = self.write(py)?;
        let mut env = sparkles::vector::embed::Environment {
            outbound: sparkles::outbound::OutboundPolicy {
                allow_private,
                ..Default::default()
            },
            ..Default::default()
        };
        for (k, v) in secrets.unwrap_or_default() {
            env.secrets
                .insert(k, v.parse().map_err(|e: String| invalid(py, e))?);
        }
        interrupt::controlled(py, cancel, progress, Some(timeout), move |ctl| {
            ds.indexes().vector().set_embedding_environment(Some(env));
            loop {
                ctl.check()?;
                match ds
                    .indexes()
                    .vector()
                    .embed_until_idle(std::time::Duration::from_millis(20))
                {
                    Ok(()) => {
                        ctl.progress.report(1.0, "embeddings complete");
                        return Ok(());
                    }
                    Err(sparkles::Error::Timeout) => {}
                    Err(e) => return Err(e),
                }
            }
        })
    }
}
#[pymethods]
impl PyGeoIndex {
    fn status<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.indexes().geo().status()))
    }
    fn enabled(&self, py: Python<'_>) -> PyResult<bool> {
        Ok(self.ds(py)?.indexes().geo().enabled())
    }
    fn enable<'py>(
        &self,
        py: Python<'py>,
        config: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        let cfg = admin::from_py(config, "geo index")?;
        admin::to_py(py, &py.detach(|| ds.indexes().geo().enable(cfg)).py(py)?)
    }
    fn disable(&self, py: Python<'_>) -> PyResult<()> {
        let ds = self.write(py)?;
        py.detach(|| ds.indexes().geo().disable()).py(py)
    }
    fn rebuild<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        admin::to_py(py, &py.detach(|| ds.indexes().geo().rebuild()).py(py)?)
    }
    #[pyo3(signature=(*,cancel=None,progress=None,timeout=None))]
    fn wait<'py>(
        &self,
        py: Python<'py>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            loop {
                ctl.check()?;
                let s = ds.indexes().geo().status();
                if s.as_ref().is_none_or(|s| s.state != "building") {
                    ctl.progress.report(1.0, "geo build finished");
                    return Ok(s);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })?;
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(*,bbox,limit=1000,graph=None,predicate=None,tolerance=None,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn features<'py>(
        &self,
        py: Python<'py>,
        bbox: [f64; 4],
        limit: usize,
        graph: Option<String>,
        predicate: Option<String>,
        tolerance: Option<f64>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        #[cfg(feature = "geo")]
        {
            let q = sparkles::geo::map::BoxQuery {
                bbox,
                limit,
                graph,
                predicate,
                tolerance,
            };
            let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
                let r = ds.indexes().geo().features(&q, None)?;
                ctl.check()?;
                ctl.progress.report(1.0, "read geometries");
                Ok(r)
            })?;
            admin::to_py(py, &r)
        }
        #[cfg(not(feature = "geo"))]
        {
            let _ = (
                ds, bbox, limit, graph, predicate, tolerance, cancel, progress, timeout,
            );
            Err(crate::errors::new_err(
                py,
                "UnsupportedError",
                "geo feature is disabled",
            ))
        }
    }
}
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyIndexes>()?;
    m.add_class::<PyTextIndex>()?;
    m.add_class::<PyVectorIndexes>()?;
    m.add_class::<PyGeoIndex>()?;
    Ok(())
}
