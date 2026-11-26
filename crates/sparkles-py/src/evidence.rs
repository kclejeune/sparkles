//! Evidence settings, durable holds, reference resolution, claim proposals, reviews,
//! withdrawals, identity resolution and dependent assessment lookups (spec C25, item 509).

use crate::{admin, errors::EngineResult, handles::handle, interrupt};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyString};
use serde_json::{Value, json};
use sparkles::evidence::binding;

handle!(PyEvidence, "Evidence");

fn settings_bytes(ob: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    if let Ok(b) = ob.cast::<PyBytes>() {
        return Ok(b.as_bytes().to_vec());
    }
    if let Ok(s) = ob.cast::<PyString>() {
        return Ok(s.to_str()?.as_bytes().to_vec());
    }
    let val: Value = admin::from_py(ob, "evidence settings")?;
    serde_json::to_vec(&val).map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
}

impl PyEvidence {
    /// Acquire a writable dataset handle and check that this thread owns no transaction
    /// on `main` or any currently cached branch of the family.
    fn write_family(&self, py: Python<'_>) -> PyResult<sparkles::Dataset> {
        let ds = self.write(py)?;
        if ds.store().branch_set().is_some()
            && let Ok(branches) = py.detach(|| ds.branches())
        {
            for info in branches {
                if let Some(br) = py.detach(|| ds.cached_branch(&info.name)).py(py)? {
                    crate::dataset::check_transaction(py, &br)?;
                }
            }
        }
        Ok(ds)
    }

    /// Acquire a writable dataset handle and check that this thread owns no transaction
    /// on `main` or the target `branch`.
    fn write_branch(&self, py: Python<'_>, branch: Option<&str>) -> PyResult<sparkles::Dataset> {
        let ds = self.write(py)?;
        if let Some(name) = branch
            && name != sparkles::branch::MAIN
            && let Some(br) = py.detach(|| ds.cached_branch(name)).py(py)?
        {
            crate::dataset::check_transaction(py, &br)?;
        }
        Ok(ds)
    }
}

#[pymethods]
impl PyEvidence {
    fn settings<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let v = py.detach(|| binding::settings(&ds)).py(py)?;
        admin::to_py(py, &v)
    }

    #[pyo3(signature=(settings, *, cancel=None, progress=None, timeout=None))]
    fn configure<'py>(
        &self,
        py: Python<'py>,
        settings: &Bound<'py, PyAny>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write_family(py)?;
        let bytes = settings_bytes(settings)?;
        let v = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let v = binding::configure(&ds, &bytes, &ctl)?;
            ctl.progress.report(1.0, "configured evidence settings");
            Ok(v)
        })?;
        admin::to_py(py, &v)
    }

    fn detach<'py>(&self, py: Python<'py>, graph: &str) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write_family(py)?;
        let v = py.detach(|| binding::detach(&ds, graph)).py(py)?;
        admin::to_py(py, &v)
    }

    #[pyo3(signature=(*, on_branch=None, after=None, limit=None))]
    fn holds<'py>(
        &self,
        py: Python<'py>,
        on_branch: Option<String>,
        after: Option<String>,
        limit: Option<usize>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let v = py
            .detach(|| binding::holds(&ds, on_branch.as_deref(), after.as_deref(), limit))
            .py(py)?;
        admin::to_py(py, &v)
    }

    #[pyo3(signature=(commits, *, id=None))]
    fn acquire_hold<'py>(
        &self,
        py: Python<'py>,
        commits: &Bound<'py, PyAny>,
        id: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write_family(py)?;
        let commits: Value = admin::from_py(commits, "evidence hold commits")?;
        let mut req = json!({ "commits": commits });
        if let Some(id) = id {
            req["id"] = Value::String(id);
        }
        let v = py.detach(|| binding::acquire_hold(&ds, &req)).py(py)?;
        admin::to_py(py, &v)
    }

    fn release_hold(&self, py: Python<'_>, id: &str) -> PyResult<bool> {
        let ds = self.write_family(py)?;
        py.detach(|| binding::release_hold(&ds, id)).py(py)
    }

    #[pyo3(signature=(references, *, branch=None, cancel=None, progress=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn resolve<'py>(
        &self,
        py: Python<'py>,
        references: &Bound<'py, PyAny>,
        branch: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let raw: Value = admin::from_py(references, "evidence resolution request")?;
        let req = if raw.is_array() {
            json!({ "references": raw })
        } else {
            raw
        };
        let v = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let v = binding::resolve(&ds, branch.as_deref(), &req, &ctl)?;
            ctl.progress.report(1.0, "resolved evidence references");
            Ok(v)
        })?;
        admin::to_py(py, &v)
    }

    #[pyo3(signature=(proposal, *, branch=None, principal=None, author=None, cancel=None, progress=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn propose<'py>(
        &self,
        py: Python<'py>,
        proposal: &Bound<'py, PyAny>,
        branch: Option<String>,
        principal: Option<String>,
        author: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let req: Value = admin::from_py(proposal, "evidence proposal")?;
        let target_branch = branch
            .as_deref()
            .or_else(|| req.get("branch").and_then(Value::as_str));
        let ds = self.write_branch(py, target_branch)?;
        let v = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let v = binding::propose(
                &ds,
                branch.as_deref(),
                principal.as_deref(),
                author.as_deref(),
                &req,
                &ctl,
            )?;
            ctl.progress.report(1.0, "proposed claims");
            Ok(v)
        })?;
        admin::to_py(py, &v)
    }

    #[pyo3(signature=(review, *, branch=None, principal=None, author=None, cancel=None, progress=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn review<'py>(
        &self,
        py: Python<'py>,
        review: &Bound<'py, PyAny>,
        branch: Option<String>,
        principal: Option<String>,
        author: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let req: Value = admin::from_py(review, "evidence review")?;
        let target_branch = branch
            .as_deref()
            .or_else(|| req.get("branch").and_then(Value::as_str));
        let ds = self.write_branch(py, target_branch)?;
        let v = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let v = binding::review(
                &ds,
                branch.as_deref(),
                principal.as_deref(),
                author.as_deref(),
                &req,
                &ctl,
            )?;
            ctl.progress.report(1.0, "reviewed claims");
            Ok(v)
        })?;
        admin::to_py(py, &v)
    }

    #[pyo3(signature=(withdrawal, *, branch=None, principal=None, author=None, cancel=None, progress=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn withdraw<'py>(
        &self,
        py: Python<'py>,
        withdrawal: &Bound<'py, PyAny>,
        branch: Option<String>,
        principal: Option<String>,
        author: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let req: Value = admin::from_py(withdrawal, "evidence withdrawal")?;
        let target_branch = branch
            .as_deref()
            .or_else(|| req.get("branch").and_then(Value::as_str));
        let ds = self.write_branch(py, target_branch)?;
        let v = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let v = binding::withdraw(
                &ds,
                branch.as_deref(),
                principal.as_deref(),
                author.as_deref(),
                &req,
                &ctl,
            )?;
            ctl.progress.report(1.0, "withdrew claims");
            Ok(v)
        })?;
        admin::to_py(py, &v)
    }

    #[pyo3(signature=(policy, entities, *, branch=None, cancel=None, progress=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn resolve_entities<'py>(
        &self,
        py: Python<'py>,
        policy: String,
        entities: Vec<String>,
        branch: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let v = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let v = binding::resolve_entities(&ds, branch.as_deref(), &policy, &entities, &ctl)?;
            ctl.progress.report(1.0, "resolved entities");
            Ok(v)
        })?;
        admin::to_py(py, &v)
    }

    #[pyo3(signature=(changed, *, transitive=false, branch=None, cancel=None, progress=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn find_dependents<'py>(
        &self,
        py: Python<'py>,
        changed: Vec<String>,
        transitive: bool,
        branch: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let v = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let v = binding::find_dependents(&ds, branch.as_deref(), &changed, transitive, &ctl)?;
            ctl.progress.report(1.0, "found evidence dependents");
            Ok(v)
        })?;
        admin::to_py(py, &v)
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyEvidence>()?;
    Ok(())
}
