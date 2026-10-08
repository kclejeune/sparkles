//! Catalog ownership and native dataset lifetime tracking.
use crate::{
    admin,
    dataset::PyDataset,
    errors::{EngineResult, invalid},
    interrupt,
};
use pyo3::prelude::*;
use sparkles::catalog::{
    Attach, Catalog, CatalogOptions, CloneRequest, CreateDataset, DatasetInfo, DatasetKind,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
pub(crate) type DatasetGroup = Arc<Mutex<Vec<Py<PyAny>>>>;

/// Track `obj` in `group` by a weak reference, dropping the references of objects that
/// are gone so that the group does not grow with every lookup.
fn track(py: Python<'_>, group: &DatasetGroup, obj: &Bound<'_, PyAny>) -> PyResult<()> {
    let weak = py
        .import("weakref")?
        .getattr("ref")?
        .call1((obj,))?
        .unbind();
    let mut g = group.lock().unwrap();
    g.retain(|w| w.bind(py).call0().is_ok_and(|o| !o.is_none()));
    g.push(weak);
    Ok(())
}

pub(crate) fn owned(
    py: Python<'_>,
    ds: sparkles::Dataset,
    path: Option<String>,
    group: Option<DatasetGroup>,
) -> PyResult<Py<PyDataset>> {
    let mut dataset = PyDataset::from_dataset(ds, path);
    dataset.group = group.clone();
    let obj = Py::new(py, dataset)?;
    if let Some(group) = group {
        track(py, &group, obj.bind(py).as_any())?;
    }
    Ok(obj)
}
#[pyclass(frozen, module = "sparkles", name = "DatasetInfo")]
pub struct PyDatasetInfo {
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    id: String,
    #[pyo3(get)]
    kind: &'static str,
    #[pyo3(get)]
    path: Option<String>,
    #[pyo3(get)]
    attached: bool,
    #[pyo3(get)]
    reserved_by: Option<String>,
}
impl From<DatasetInfo> for PyDatasetInfo {
    fn from(i: DatasetInfo) -> Self {
        Self {
            name: i.name,
            id: i.id.to_string(),
            kind: match i.kind {
                DatasetKind::Persistent => "persistent",
                DatasetKind::Memory => "mem",
            },
            path: i.path.map(|p| p.display().to_string()),
            attached: i.attached,
            reserved_by: i.reserved_by,
        }
    }
}
#[pyclass(frozen, module = "sparkles", name = "Catalog")]
pub struct PyCatalog {
    inner: Mutex<Option<Catalog>>,
    group: DatasetGroup,
    cache: Mutex<BTreeMap<String, Py<PyAny>>>,
}
impl PyCatalog {
    pub(crate) fn catalog(&self, py: Python<'_>) -> PyResult<Catalog> {
        self.inner
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| invalid(py, "the catalog is closed"))
    }
    fn wrap(&self, py: Python<'_>, name: &str, ds: sparkles::Dataset) -> PyResult<Py<PyDataset>> {
        self.catalog(py)?;
        let mut cache = self.cache.lock().unwrap();
        if let Some(weak) = cache.get(name) {
            let old = weak.bind(py).call0()?;
            if !old.is_none() {
                let obj = old.extract::<Py<PyDataset>>()?;
                if obj.borrow(py).ds(py).is_ok_and(|cached| {
                    std::ptr::eq(cached.state(), ds.state()) && cached.name() == ds.name()
                }) {
                    return Ok(obj);
                }
            }
        }
        let path = ds.store().root().map(|p| p.display().to_string());
        let obj = owned(py, ds, path, Some(self.group.clone()))?;
        let weak = py
            .import("weakref")?
            .getattr("ref")?
            .call1((obj.bind(py),))?
            .unbind();
        cache.insert(name.into(), weak);
        Ok(obj)
    }
    fn from_catalog(cat: Catalog) -> Self {
        Self {
            inner: Mutex::new(Some(cat)),
            group: Default::default(),
            cache: Default::default(),
        }
    }
}
#[pymethods]
impl PyCatalog {
    #[new]
    #[pyo3(signature=(path,*,union_default_graph=false))]
    fn new(py: Python<'_>, path: PathBuf, union_default_graph: bool) -> PyResult<Self> {
        let opts = CatalogOptions::from(sparkles::store::StoreOptions {
            union_default_graph,
            ..Default::default()
        });
        let absolute = std::path::absolute(&path)?;
        let cat = py
            .detach(|| Catalog::open(&path, opts))
            .map_err(|e| match &e {
                sparkles::Error::Locked { path: lock, .. } if lock == &absolute => {
                    crate::errors::new_err(py, "CatalogLockedError", e.to_string())
                }
                _ => crate::errors::engine(py, e),
            })?;
        Ok(Self::from_catalog(cat))
    }
    #[staticmethod]
    #[pyo3(signature=(*,union_default_graph=false))]
    fn memory(union_default_graph: bool) -> Self {
        Self::from_catalog(Catalog::memory(
            sparkles::store::StoreOptions {
                union_default_graph,
                ..Default::default()
            }
            .into(),
        ))
    }
    #[staticmethod]
    fn inspect(py: Python<'_>, path: PathBuf) -> PyResult<Vec<PyDatasetInfo>> {
        Ok(py
            .detach(|| Catalog::inspect(path))
            .py(py)?
            .into_iter()
            .map(Into::into)
            .collect())
    }
    fn list(&self, py: Python<'_>) -> PyResult<Vec<PyDatasetInfo>> {
        let cat = self.catalog(py)?;
        Ok(py
            .detach(|| cat.list())
            .into_iter()
            .map(Into::into)
            .collect())
    }
    fn info(&self, py: Python<'_>, name: &str) -> PyResult<Option<PyDatasetInfo>> {
        Ok(self.catalog(py)?.info(name).map(Into::into))
    }
    fn get(&self, py: Python<'_>, name: &str) -> PyResult<Option<Py<PyDataset>>> {
        let cat = self.catalog(py)?;
        let ds = py.detach(|| cat.get(name));
        ds.map(|ds| self.wrap(py, name, ds)).transpose()
    }
    fn __getitem__(&self, py: Python<'_>, name: &str) -> PyResult<Py<PyDataset>> {
        self.get(py, name)?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(name.to_owned()))
    }
    fn get_by_id(&self, py: Python<'_>, id: &str) -> PyResult<Option<Py<PyDataset>>> {
        let cat = self.catalog(py)?;
        let id = id
            .parse()
            .map_err(|e| invalid(py, format!("invalid dataset id: {e}")))?;
        let ds = py.detach(|| cat.get_by_id(id));
        ds.map(|ds| {
            let name = ds.name().unwrap_or("").to_owned();
            self.wrap(py, &name, ds)
        })
        .transpose()
    }
    #[pyo3(signature=(name,*,kind="persistent",geo=None))]
    fn create(
        &self,
        py: Python<'_>,
        name: &str,
        kind: &str,
        geo: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyDataset>> {
        let cat = self.catalog(py)?;
        let req = CreateDataset {
            kind: match kind {
                "persistent" => DatasetKind::Persistent,
                "mem" | "memory" => DatasetKind::Memory,
                _ => return Err(invalid(py, "kind must be 'persistent' or 'mem'")),
            },
            geo: geo
                .map(|v| admin::from_py(v, "geo configuration"))
                .transpose()?,
        };
        let ds = py.detach(|| cat.create(name, &req)).py(py)?;
        self.wrap(py, name, ds)
    }
    #[pyo3(signature=(name,path=None))]
    fn attach(&self, py: Python<'_>, name: &str, path: Option<PathBuf>) -> PyResult<Py<PyDataset>> {
        let cat = self.catalog(py)?;
        let source = path.map(Attach::Directory).unwrap_or(Attach::Memory);
        let ds = py.detach(|| cat.attach(name, source)).py(py)?;
        self.wrap(py, name, ds)
    }
    fn delete(&self, py: Python<'_>, name: &str) -> PyResult<bool> {
        let cat = self.catalog(py)?;
        let deleted = py.detach(|| cat.delete(name)).py(py)?;
        self.cache.lock().unwrap().remove(name);
        Ok(deleted)
    }
    fn rename(&self, py: Python<'_>, name: &str, new_name: &str) -> PyResult<Py<PyDataset>> {
        let cat = self.catalog(py)?;
        let ds = py.detach(|| cat.rename(name, new_name)).py(py)?;
        self.cache.lock().unwrap().remove(name);
        self.wrap(py, new_name, ds)
    }
    #[pyo3(signature=(source,name,*,kind=None,at=None,inferences="copy",cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn clone_dataset(
        &self,
        py: Python<'_>,
        source: String,
        name: String,
        kind: Option<&str>,
        at: Option<&Bound<'_, PyAny>>,
        inferences: &str,
        cancel: Option<&Bound<'_, PyAny>>,
        progress: Option<&Bound<'_, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Py<PyDataset>> {
        let cat = self.catalog(py)?;
        if let Some(ds) = cat.get(&source) {
            crate::dataset::check_transaction(py, &ds)?;
        }
        let mut req = CloneRequest {
            kind: match kind {
                None => None,
                Some("persistent") => Some(DatasetKind::Persistent),
                Some("mem" | "memory") => Some(DatasetKind::Memory),
                _ => return Err(invalid(py, "invalid dataset kind")),
            },
            ..Default::default()
        };
        req.spec.at = at.map(crate::dataset::at_from_py).transpose()?;
        req.spec.inferences = sparkles::cloning::Inferences::parse(inferences)
            .ok_or_else(|| invalid(py, "inferences must be 'copy' or 'drop'"))?;
        let target = name.clone();
        let ds = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            cat.clone_dataset(&source, &name, &req, &ctl)
        })?;
        self.wrap(py, &target, ds)
    }
    fn backup_files<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let cat = self.catalog(py)?;
        let files = py.detach(|| cat.backup_files()).py(py)?;
        let files = files
            .into_iter()
            .map(|f| serde_json::json!({"name":f.name,"path":f.path}))
            .collect::<Vec<_>>();
        admin::to_py(py, &files)
    }
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let weak = std::mem::take(&mut *self.group.lock().unwrap());
        for w in weak {
            let obj = w.bind(py).call0()?;
            if !obj.is_none() {
                obj.call_method0("close")?;
            }
        }
        self.cache.lock().unwrap().clear();
        let cat = self.inner.lock().unwrap().take();
        py.detach(|| drop(cat));
        Ok(())
    }
    fn __enter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<Self>> {
        slf.borrow(py).catalog(py)?;
        Ok(slf)
    }
    fn __exit__(
        &self,
        py: Python<'_>,
        _ty: &Bound<'_, PyAny>,
        _value: &Bound<'_, PyAny>,
        _traceback: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        self.close(py)
    }
    #[cfg(feature = "backup")]
    #[getter]
    fn repositories(slf: Py<Self>) -> crate::backups::PyRepositories {
        crate::backups::PyRepositories { owner: slf }
    }
    #[cfg(feature = "backup")]
    #[pyo3(signature=(repository,backup,*,name,identity="auto",in_place=false,check="quick",cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn restore(
        &self,
        py: Python<'_>,
        repository: Py<crate::backups::PyBackupRepository>,
        backup: String,
        name: String,
        identity: &str,
        in_place: bool,
        check: &str,
        cancel: Option<&Bound<'_, PyAny>>,
        progress: Option<&Bound<'_, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Py<PyDataset>> {
        let cat = self.catalog(py)?;
        // The in-place swap waits up to 30 s for handles to close, which suits a
        // server's requests. Handles this thread holds would never close while it
        // waits, so refuse at once, as a rename does.
        if in_place && cat.in_use(&name) {
            let err = crate::errors::new_err(
                py,
                "BackupError",
                format!("dataset /{name} still has live handles; close them first"),
            );
            let _ = err.value(py).setattr("code", "dataset-busy");
            return Err(err);
        }
        let repo = repository.borrow(py).repo.clone();
        let target = name.clone();
        let req = sparkles::backup::RestoreRequest {
            target: Some(name),
            replace: in_place,
            identity: serde_json::from_value(serde_json::json!(identity))
                .map_err(|e| invalid(py, e.to_string()))?,
            check: serde_json::from_value(serde_json::json!(check))
                .map_err(|e| invalid(py, e.to_string()))?,
            keep_replaced: false,
        };
        let ds = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            cat.restore(&repo, &backup, &req, &ctl)
        })?;
        self.wrap(py, &target, ds)
    }
    #[cfg(feature = "backup")]
    #[pyo3(signature=(policy,*,cancel=None,progress=None,timeout=None))]
    fn run_policy<'py>(
        &self,
        py: Python<'py>,
        policy: &Bound<'py, PyAny>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let cat = self.catalog(py)?;
        let p: sparkles::backup::PolicyConfig = admin::from_py(policy, "backup policy")?;
        for info in cat.list() {
            if p.datasets
                .iter()
                .any(|pattern| sparkles::backup::policy::matches_dataset(pattern, &info.name))
                && let Some(ds) = cat.get(&info.name)
            {
                crate::dataset::check_transaction(py, &ds)?;
            }
        }
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            cat.run_policy(&p, &ctl)
        })?;
        admin::to_py(py, &r)
    }
    #[cfg(feature = "backup")]
    #[pyo3(signature=(policy,*,dry_run=false))]
    fn apply_retention<'py>(
        &self,
        py: Python<'py>,
        policy: &Bound<'py, PyAny>,
        dry_run: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let cat = self.catalog(py)?;
        let p = admin::from_py(policy, "backup policy")?;
        let r = py.detach(|| cat.apply_retention(&p, dry_run)).py(py)?;
        admin::to_py(py, &r)
    }

    #[pyo3(signature=(name,*,kind="clone",holder="python"))]
    fn reserve(
        &self,
        py: Python<'_>,
        name: &str,
        kind: &str,
        holder: &str,
    ) -> PyResult<Py<PyReservation>> {
        let cat = self.catalog(py)?;
        let kind = match kind {
            "clone" => sparkles::catalog::ReservationKind::Clone,
            "restore" => sparkles::catalog::ReservationKind::Restore,
            _ => return Err(invalid(py, "kind must be clone or restore")),
        };
        let r = py.detach(|| cat.reserve(name, kind, holder)).py(py)?;
        let obj = Py::new(
            py,
            PyReservation {
                inner: Mutex::new(Some(r)),
                name: name.into(),
            },
        )?;
        track(py, &self.group, obj.bind(py).as_any())?;
        Ok(obj)
    }
}
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCatalog>()?;
    m.add_class::<PyReservation>()?;
    m.add_class::<PyDatasetInfo>()?;
    Ok(())
}

#[pyclass(frozen, module = "sparkles", name = "Reservation", weakref)]
pub struct PyReservation {
    inner: Mutex<Option<sparkles::catalog::Reservation>>,
    #[pyo3(get)]
    name: String,
}
#[pymethods]
impl PyReservation {
    fn close(&self, py: Python<'_>) {
        let r = self.inner.lock().unwrap().take();
        py.detach(|| drop(r));
    }
    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }
    fn __exit__(
        &self,
        py: Python<'_>,
        _ty: &Bound<'_, PyAny>,
        _value: &Bound<'_, PyAny>,
        _tb: &Bound<'_, PyAny>,
    ) {
        self.close(py);
    }
}
