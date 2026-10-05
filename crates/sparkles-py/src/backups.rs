//! Native repository handles, dataset backups and catalog repository configuration.
#![cfg(feature = "backup")]
use crate::{
    admin,
    catalog::PyCatalog,
    dataset::PyDataset,
    errors::{EngineResult, invalid},
    interrupt,
};
use pyo3::prelude::*;
use sparkles::backup::{self, RepoConfig, Repository};
use std::path::PathBuf;
use std::sync::Arc;

#[pyclass(frozen, module = "sparkles", name = "Backup")]
pub struct PyBackup {
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    repository: String,
    #[pyo3(get)]
    dataset_name: String,
    #[pyo3(get)]
    dataset_id: String,
    #[pyo3(get)]
    commit: u64,
    #[pyo3(get)]
    quads: u64,
    #[pyo3(get)]
    created: String,
    #[pyo3(get)]
    completed: String,
    #[pyo3(get)]
    logical_bytes: u64,
    #[pyo3(get)]
    added_bytes: u64,
    #[pyo3(get)]
    note: Option<String>,
    json: serde_json::Value,
}
impl From<backup::BackupSummary> for PyBackup {
    fn from(b: backup::BackupSummary) -> Self {
        let json = serde_json::to_value(&b).expect("backup summary serializes");
        Self {
            name: b.name,
            repository: b.repository,
            dataset_name: b.dataset.name,
            dataset_id: b.dataset.id.to_string(),
            commit: b.commit.seq,
            quads: b.commit.quads,
            created: b.created,
            completed: b.completed,
            logical_bytes: b.logical_bytes,
            added_bytes: b.added_bytes,
            note: b.note,
            json,
        }
    }
}
#[pymethods]
impl PyBackup {
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        admin::to_py(py, &self.json)
    }
}
#[pyclass(frozen, module = "sparkles", name = "BackupRepository")]
pub struct PyBackupRepository {
    pub(crate) repo: Arc<Repository>,
}
#[pymethods]
impl PyBackupRepository {
    #[staticmethod]
    #[pyo3(signature=(url_or_config,*,cache_dir=None,init=true))]
    fn open(
        py: Python<'_>,
        url_or_config: &Bound<'_, PyAny>,
        cache_dir: Option<PathBuf>,
        init: bool,
    ) -> PyResult<Self> {
        let cfg: RepoConfig = if let Ok(url) = url_or_config.extract::<String>() {
            RepoConfig::from_url("python", &url)
                .map_err(|e| crate::errors::engine(py, backup::error(e)))?
        } else {
            admin::from_py(url_or_config, "backup repository")?
        };
        let env = backup::OpenEnv {
            cache_dir,
            init,
            ..Default::default()
        };
        let repo = py.detach(|| backup::open(&cfg, &env)).py(py)?;
        Ok(Self {
            repo: Arc::new(repo),
        })
    }
    #[getter]
    fn id(&self) -> String {
        self.repo.id().to_string()
    }
    fn test<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        admin::to_py(
            py,
            &py.detach(|| backup::blocking(&self.repo).test()).py(py)?,
        )
    }
    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        admin::to_py(
            py,
            &py.detach(|| backup::blocking(&self.repo).stats()).py(py)?,
        )
    }
    #[pyo3(signature=(*,dataset=None,policy=None,limit=None,before=None))]
    fn backups(
        &self,
        py: Python<'_>,
        dataset: Option<String>,
        policy: Option<String>,
        limit: Option<usize>,
        before: Option<String>,
    ) -> PyResult<Vec<PyBackup>> {
        let f = backup::ListFilter {
            dataset,
            policy,
            limit,
            before,
            ..Default::default()
        };
        Ok(py
            .detach(|| backup::blocking(&self.repo).list(&f))
            .py(py)?
            .into_iter()
            .map(Into::into)
            .collect())
    }
    #[pyo3(signature=(names=None,*,level="exists",cancel=None,progress=None,timeout=None))]
    fn verify<'py>(
        &self,
        py: Python<'py>,
        names: Option<Vec<String>>,
        level: &str,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let repo = self.repo.clone();
        let level = enum_value(py, level, "verification level")?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let o = backup::VerifyOptions {
                level,
                ctl: (&ctl).into(),
                ..Default::default()
            };
            backup::blocking(&repo).verify(&names.unwrap_or_default(), &o)
        })?;
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(*,dry_run=false,grace=86400.0,cancel=None,progress=None,timeout=None))]
    fn gc<'py>(
        &self,
        py: Python<'py>,
        dry_run: bool,
        grace: f64,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        if !grace.is_finite() || grace < 0.0 {
            return Err(invalid(py, "grace must be finite and nonnegative"));
        }
        let grace = std::time::Duration::try_from_secs_f64(grace)
            .map_err(|_| invalid(py, "grace is too large"))?;
        let repo = self.repo.clone();
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            backup::blocking(&repo).gc(&backup::GcOptions {
                dry_run,
                grace,
                ctl: (&ctl).into(),
            })
        })?;
        admin::to_py(py, &r)
    }
    fn locks<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        admin::to_py(
            py,
            &py.detach(|| backup::blocking(&self.repo).locks()).py(py)?,
        )
    }
    fn break_lock(&self, py: Python<'_>, id: &str) -> PyResult<bool> {
        py.detach(|| backup::blocking(&self.repo).break_lock(id))
            .py(py)
    }
}
fn enum_value<T: serde::de::DeserializeOwned>(py: Python<'_>, s: &str, what: &str) -> PyResult<T> {
    serde_json::from_value(serde_json::json!(s))
        .map_err(|e| invalid(py, format!("invalid {what}: {e}")))
}
#[pyclass(frozen, module = "sparkles", name = "Backups")]
pub struct PyBackups {
    pub(crate) owner: Py<PyDataset>,
    pub(crate) repository: Py<PyBackupRepository>,
}
impl PyBackups {
    fn pair(&self, py: Python<'_>) -> PyResult<(sparkles::Dataset, Arc<Repository>)> {
        Ok((
            self.owner.borrow(py).ds(py)?,
            self.repository.borrow(py).repo.clone(),
        ))
    }
}
#[pymethods]
impl PyBackups {
    #[pyo3(signature=(name=None,*,note=None,cancel=None,progress=None,timeout=None))]
    fn create(
        &self,
        py: Python<'_>,
        name: Option<String>,
        note: Option<String>,
        cancel: Option<&Bound<'_, PyAny>>,
        progress: Option<&Bound<'_, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<PyBackup> {
        let (ds, repo) = self.pair(py)?;
        crate::dataset::check_transaction(py, &ds)?;
        static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = name.unwrap_or_else(|| {
            format!(
                "backup-{}-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            )
        });
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            ds.backups(&repo).create_with(
                &backup::CreateOptions {
                    name,
                    note,
                    ..Default::default()
                },
                &ctl,
            )
        })?;
        Ok(r.into())
    }
    #[pyo3(signature=(*,policy=None,limit=None,before=None))]
    fn list(
        &self,
        py: Python<'_>,
        policy: Option<String>,
        limit: Option<usize>,
        before: Option<String>,
    ) -> PyResult<Vec<PyBackup>> {
        let (ds, repo) = self.pair(py)?;
        let f = backup::ListFilter {
            policy,
            limit,
            before,
            ..Default::default()
        };
        Ok(py
            .detach(|| ds.backups(&repo).list(&f))
            .py(py)?
            .into_iter()
            .map(Into::into)
            .collect())
    }
    fn get<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        let (ds, repo) = self.pair(py)?;
        admin::to_py(py, &py.detach(|| ds.backups(&repo).get(name)).py(py)?)
    }
    fn delete(&self, py: Python<'_>, name: &str) -> PyResult<bool> {
        let (ds, repo) = self.pair(py)?;
        py.detach(|| ds.backups(&repo).delete(name)).py(py)
    }
    #[pyo3(signature=(name,*,level="exists",cancel=None,progress=None,timeout=None))]
    fn verify<'py>(
        &self,
        py: Python<'py>,
        name: String,
        level: &str,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (ds, repo) = self.pair(py)?;
        let level = enum_value(py, level, "verification level")?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            ds.backups(&repo).verify_with(
                &name,
                &backup::VerifyOptions {
                    level,
                    ..Default::default()
                },
                &ctl,
            )
        })?;
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(name,directory,*,identity="auto",check="quick",cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn restore_to_dir<'py>(
        &self,
        py: Python<'py>,
        name: String,
        directory: PathBuf,
        identity: &str,
        check: &str,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (ds, repo) = self.pair(py)?;
        let identity = enum_value(py, identity, "identity")?;
        let check = enum_value(py, check, "check level")?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            if ds.backups(&repo).get(&name)?.is_none() {
                return Err(sparkles::Error::NotFound(format!(
                    "no backup '{name}' of this dataset"
                )));
            }
            let o = backup::RestoreOptions {
                identity,
                check,
                ctl: (&ctl).into(),
                ..Default::default()
            };
            backup::blocking(&repo).restore_to_dir(&name, &directory, &o)
        })?;
        admin::to_py(
            py,
            &serde_json::json!({"backup":r.backup,"datasetId":r.dataset_id,"identity":r.identity,"forkedFrom":r.forked_from,"check":r.check}),
        )
    }
    #[pyo3(signature=(policy,*,cancel=None,progress=None,timeout=None))]
    fn run_policy<'py>(
        &self,
        py: Python<'py>,
        policy: &Bound<'py, PyAny>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (ds, repo) = self.pair(py)?;
        crate::dataset::check_transaction(py, &ds)?;
        let p = admin::from_py(policy, "backup policy")?;
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            ds.backups(&repo).run_policy(&p, &ctl)
        })?;
        admin::to_py(py, &r)
    }
}
#[pyclass(frozen, module = "sparkles", name = "Repositories")]
pub struct PyRepositories {
    pub(crate) owner: Py<PyCatalog>,
}
impl PyRepositories {
    fn repos(&self, py: Python<'_>) -> PyResult<backup::Repositories> {
        self.owner.borrow(py).catalog(py)?.repositories().py(py)
    }
}
#[pymethods]
impl PyRepositories {
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let r = self.repos(py)?;
        admin::to_py(py, &py.detach(|| r.list()))
    }
    fn get<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        let r = self.repos(py)?;
        admin::to_py(py, &py.detach(|| r.get(name)).py(py)?)
    }
    fn add<'py>(&self, py: Python<'py>, config: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let r = self.repos(py)?;
        let c = admin::from_py(config, "repository")?;
        admin::to_py(py, &py.detach(|| r.add(c)).py(py)?)
    }
    fn update<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        config: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let r = self.repos(py)?;
        let c = admin::from_py(config, "repository")?;
        admin::to_py(py, &py.detach(|| r.update(name, c)).py(py)?)
    }
    fn remove(&self, py: Python<'_>, name: &str) -> PyResult<bool> {
        let r = self.repos(py)?;
        py.detach(|| r.remove(name)).py(py)
    }
    fn open(&self, py: Python<'_>, name: &str) -> PyResult<PyBackupRepository> {
        let r = self.repos(py)?;
        Ok(PyBackupRepository {
            repo: py.detach(|| r.open(name)).py(py)?,
        })
    }
    fn with_fixed<'py>(
        &self,
        py: Python<'py>,
        entries: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let r = self.repos(py)?;
        let entries: Vec<RepoConfig> = admin::from_py(entries, "repositories")?;
        let r = py.detach(|| r.with_fixed(&entries)).py(py)?;
        admin::to_py(py, &r.list())
    }
}
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyBackup>()?;
    m.add_class::<PyBackupRepository>()?;
    m.add_class::<PyBackups>()?;
    m.add_class::<PyRepositories>()?;
    Ok(())
}
