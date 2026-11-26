//! Evidence settings, durable holds, reference resolution, claim proposals, reviews,
//! withdrawals, claim states, identity resolution and dependent assessment lookups
//! (spec C25, item 509).

use crate::{admin, errors::EngineResult, handles::handle, interrupt};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyInt, PyString};
use sparkles::evidence::binding;

handle!(PyEvidence, "Evidence");

fn settings_bytes(py: Python<'_>, ob: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    let bytes = if let Ok(b) = ob.cast::<PyBytes>() {
        b.as_bytes().to_vec()
    } else if let Ok(s) = ob.cast::<PyString>() {
        s.to_str()?.as_bytes().to_vec()
    } else {
        admin::dumps_py(ob)?.into_bytes()
    };
    binding::check_raw_bytes(&bytes, binding::MAX_SETTINGS_BYTES).py(py)?;
    Ok(bytes)
}

fn json_bytes(py: Python<'_>, ob: &Bound<'_, PyAny>, limit: usize) -> PyResult<Vec<u8>> {
    let text = admin::dumps_py(ob)?;
    binding::check_raw_bytes(text.as_bytes(), limit).py(py)?;
    Ok(text.into_bytes())
}

fn at_selector(ob: Option<&Bound<'_, PyAny>>) -> PyResult<Option<String>> {
    let Some(ob) = ob else {
        return Ok(None);
    };
    if ob.is_none() {
        return Ok(None);
    }
    if ob.is_instance_of::<PyBool>() {
        return Err(pyo3::exceptions::PyTypeError::new_err(
            "at must be a commit integer or selector string",
        ));
    }
    if let Ok(i) = ob.cast::<PyInt>() {
        let seq: u64 = i.extract()?;
        return Ok(Some(seq.to_string()));
    }
    if let Ok(s) = ob.cast::<PyString>() {
        return Ok(Some(s.to_str()?.to_owned()));
    }
    Err(pyo3::exceptions::PyTypeError::new_err(
        "at must be a commit integer or selector string",
    ))
}

impl PyEvidence {
    /// Acquire a writable dataset handle and check that this thread owns no transaction
    /// or streaming write anywhere in the dataset family (main, live branches, aliases,
    /// or retained deleted branches), without taking any store or branch-table lock.
    fn write_family(&self, py: Python<'_>) -> PyResult<sparkles::Dataset> {
        let ds = self.write(py)?;
        crate::dataset::check_family_transactions(py, &ds)?;
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
        let bytes = settings_bytes(py, settings)?;
        let v = interrupt::controlled_write(py, cancel, progress, timeout, move |ctl| {
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
        let commits_json = admin::dumps_py(commits)?;
        let payload = match id {
            Some(id) => {
                let id_json = serde_json::to_string(&id)
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
                format!("{{\"id\":{id_json},\"commits\":{commits_json}}}")
            }
            None => format!("{{\"commits\":{commits_json}}}"),
        };
        binding::check_raw_bytes(payload.as_bytes(), binding::MAX_HOLD_BYTES).py(py)?;
        let v = py
            .detach(|| binding::acquire_hold_bytes(&ds, payload.as_bytes()))
            .py(py)?;
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
        let bytes = json_bytes(py, references, binding::MAX_RESOLVE_BYTES)?;
        let v = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let v = binding::resolve_bytes(&ds, branch.as_deref(), &bytes, &ctl)?;
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
        let ds = self.write_family(py)?;
        let bytes = json_bytes(py, proposal, binding::MAX_CLAIMS_BYTES)?;
        let v = interrupt::controlled_write(py, cancel, progress, timeout, move |ctl| {
            let v = binding::propose_bytes(
                &ds,
                branch.as_deref(),
                principal.as_deref(),
                author.as_deref(),
                &bytes,
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
        let ds = self.write_family(py)?;
        let bytes = json_bytes(py, review, binding::MAX_REVIEW_BYTES)?;
        let v = interrupt::controlled_write(py, cancel, progress, timeout, move |ctl| {
            let v = binding::review_bytes(
                &ds,
                branch.as_deref(),
                principal.as_deref(),
                author.as_deref(),
                &bytes,
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
        let ds = self.write_family(py)?;
        let bytes = json_bytes(py, withdrawal, binding::MAX_REVIEW_BYTES)?;
        let v = interrupt::controlled_write(py, cancel, progress, timeout, move |ctl| {
            let v = binding::withdraw_bytes(
                &ds,
                branch.as_deref(),
                principal.as_deref(),
                author.as_deref(),
                &bytes,
                &ctl,
            )?;
            ctl.progress.report(1.0, "withdrew claims");
            Ok(v)
        })?;
        admin::to_py(py, &v)
    }

    #[pyo3(signature=(policy, claims, *, branch=None, at=None, cancel=None, progress=None, timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn claim_states<'py>(
        &self,
        py: Python<'py>,
        policy: String,
        claims: Vec<String>,
        branch: Option<String>,
        at: Option<&Bound<'py, PyAny>>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let at = at_selector(at)?;
        let v = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let v = binding::claim_states(
                &ds,
                branch.as_deref(),
                at.as_deref(),
                &policy,
                &claims,
                &ctl,
            )?;
            ctl.progress.report(1.0, "read claim states");
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
