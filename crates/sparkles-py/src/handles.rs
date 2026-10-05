//! Python administration handles. Each retains the Python owner rather than a second
//! engine handle, so closing the dataset immediately invalidates all its properties.
use crate::dataset::at_from_py;
use crate::terms::{PyQuad, graph_from_py, named_node_from_py, term_from_py};
use crate::{admin, errors::EngineResult, interrupt};
use pyo3::prelude::*;
use pyo3::types::{PyList, PyTuple};
use sparkles::history::At;
use std::time::Duration;

macro_rules! handle {
    ($rust:ident, $name:literal) => {
        #[pyclass(frozen, module="sparkles", name=$name)]
        pub struct $rust {
            pub(crate) owner: Py<crate::dataset::PyDataset>,
        }
        #[allow(dead_code)] // Read-only and container handles use only one of these helpers.
        impl $rust {
            fn ds(&self, py: Python<'_>) -> PyResult<sparkles::Dataset> {
                self.owner.borrow(py).ds(py)
            }
            fn write(&self, py: Python<'_>) -> PyResult<sparkles::Dataset> {
                self.owner.borrow(py).ds_for_write(py)
            }
        }
    };
}
pub(crate) use handle;
handle!(PySnapshots, "Snapshots");
handle!(PyHistory, "History");
handle!(PySettings, "Settings");
handle!(PyCompactionSetting, "CompactionSetting");
handle!(PyDescribeSetting, "DescribeSetting");
handle!(PyQuotaSetting, "QuotaSetting");
handle!(PyRetentionSetting, "RetentionSetting");
handle!(PyChangeLogSetting, "ChangeLogSetting");

#[pymethods]
impl PySnapshots {
    fn list(&self, py: Python<'_>) -> PyResult<Vec<admin::PySnapshot>> {
        let ds = self.ds(py)?;
        Ok(py
            .detach(|| ds.snapshots().list())
            .into_iter()
            .map(Into::into)
            .collect())
    }
    fn get(&self, py: Python<'_>, name: &str) -> PyResult<Option<admin::PySnapshot>> {
        let ds = self.ds(py)?;
        Ok(py.detach(|| ds.snapshots().get(name)).map(Into::into))
    }
    #[pyo3(signature=(name, *, at=None, note=None, expires_ms=None))]
    fn create(
        &self,
        py: Python<'_>,
        name: &str,
        at: Option<&Bound<'_, PyAny>>,
        note: Option<String>,
        expires_ms: Option<i64>,
    ) -> PyResult<admin::PySnapshot> {
        let ds = self.write(py)?;
        let at = at.map(at_from_py).transpose()?.unwrap_or(At::Head);
        let opts = sparkles::history::SnapshotOptions {
            note,
            expires_ms,
            ..Default::default()
        };
        Ok(py
            .detach(|| ds.snapshots().create(name, &at, &opts))
            .py(py)?
            .0
            .into())
    }
    fn delete(&self, py: Python<'_>, name: &str) -> PyResult<bool> {
        let ds = self.write(py)?;
        py.detach(|| ds.snapshots().delete(name)).py(py)
    }
}

#[pyclass(frozen, module = "sparkles", name = "Diff")]
pub struct PyDiff {
    #[pyo3(get)]
    pub from_commit: u64,
    #[pyo3(get)]
    pub to_commit: u64,
    #[pyo3(get)]
    pub added: u64,
    #[pyo3(get)]
    pub removed: u64,
    #[pyo3(get)]
    pub method: String,
    rows: Vec<(String, PyQuad)>,
}
impl From<sparkles::store::Diff> for PyDiff {
    fn from(d: sparkles::store::Diff) -> Self {
        Self {
            from_commit: d.from.commit.seq,
            to_commit: d.to.commit.seq,
            added: d.added,
            removed: d.removed,
            method: d.method.as_str().into(),
            rows: d
                .iter()
                .map(|(op, inner)| (op.sign().to_string(), PyQuad { inner }))
                .collect(),
        }
    }
}
#[pymethods]
impl PyDiff {
    fn __len__(&self) -> usize {
        self.rows.len()
    }
    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rows = self
            .rows
            .iter()
            .map(|(op, q)| Ok((op.clone(), Py::new(py, q.clone())?)))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, rows)?.call_method0("__iter__")
    }
}
#[pyclass(frozen, module = "sparkles", name = "CommitChanges")]
pub struct PyCommitChanges {
    #[pyo3(get)]
    commit: Py<admin::PyCommit>,
    #[pyo3(get)]
    added: u64,
    #[pyo3(get)]
    removed: u64,
    #[pyo3(get)]
    complete: bool,
    changes: Vec<(String, PyQuad)>,
}
#[pymethods]
impl PyCommitChanges {
    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rows = self
            .changes
            .iter()
            .map(|(op, q)| Ok((op.clone(), Py::new(py, q.clone())?)))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, rows)?.call_method0("__iter__")
    }
}
#[pyclass(frozen, module = "sparkles", name = "ChangePage")]
pub struct PyChangePage {
    #[pyo3(get)]
    after: u64,
    #[pyo3(get)]
    head: Py<admin::PyCommit>,
    #[pyo3(get)]
    next: u64,
    commits: Vec<Py<PyCommitChanges>>,
}
#[pymethods]
impl PyChangePage {
    #[getter]
    fn commits<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, self.commits.iter().map(|c| c.clone_ref(py)))
    }
    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.commits(py)?.call_method0("__iter__")
    }
}
#[pyclass(frozen, module = "sparkles", name = "HistoryChange")]
pub struct PyHistoryChange {
    #[pyo3(get)]
    seq: u64,
    #[pyo3(get)]
    op: String,
    quad: PyQuad,
}
#[pymethods]
impl PyHistoryChange {
    #[getter]
    fn quad(&self) -> PyQuad {
        self.quad.clone()
    }
}
#[pymethods]
impl PyHistory {
    fn status<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        history_status(py, &ds, py.detach(|| ds.history().status()))
    }
    fn commit(
        &self,
        py: Python<'_>,
        reference: &Bound<'_, PyAny>,
    ) -> PyResult<Option<admin::PyCommit>> {
        let ds = self.ds(py)?;
        let text = reference.str()?.to_str()?.to_owned();
        let r = text.parse().py(py)?;
        Ok(py
            .detach(|| ds.history().commit(&r))
            .py(py)?
            .map(Into::into))
    }
    #[pyo3(signature=(limit=100,*,before=None,after=None))]
    fn commits(
        &self,
        py: Python<'_>,
        limit: usize,
        before: Option<u64>,
        after: Option<u64>,
    ) -> PyResult<Vec<admin::PyCommit>> {
        admin::commits(py, &self.ds(py)?, limit, before, after)
    }
    #[pyo3(signature=(from_commit,to_commit=None,*,graph=None,max_quads=0,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn diff(
        &self,
        py: Python<'_>,
        from_commit: &Bound<'_, PyAny>,
        to_commit: Option<&Bound<'_, PyAny>>,
        graph: Option<&Bound<'_, PyAny>>,
        max_quads: u64,
        cancel: Option<&Bound<'_, PyAny>>,
        progress: Option<&Bound<'_, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<PyDiff> {
        let ds = self.ds(py)?;
        let from = at_from_py(from_commit)?;
        let to = to_commit.map(at_from_py).transpose()?.unwrap_or(At::Head);
        let graph = graph.map(graph_from_py).transpose()?;
        Ok(
            interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
                ctl.progress.report(0.0, "comparing commits");
                let opts = sparkles::store::DiffOptions {
                    graph,
                    max_quads,
                    cancel: Some(ctl.cancel.flag()),
                    deadline: ctl.deadline,
                    ..Default::default()
                };
                let diff = ds.history().diff(&from, &to, &opts)?;
                ctl.progress.report(1.0, "compared commits");
                Ok(diff)
            })?
            .into(),
        )
    }
    #[pyo3(signature=(after,*,max_commits=100,max_quads=0,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn changes(
        &self,
        py: Python<'_>,
        after: u64,
        max_commits: usize,
        max_quads: u64,
        cancel: Option<&Bound<'_, PyAny>>,
        progress: Option<&Bound<'_, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<PyChangePage> {
        let ds = self.ds(py)?;
        let page = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let opts = sparkles::store::ChangesOptions {
                max_commits,
                max_quads,
                cancel: Some(ctl.cancel.flag()),
                deadline: ctl.deadline,
                ..Default::default()
            };
            let out = ds.history().changes(after, &opts)?;
            ctl.progress.report(1.0, "read changes");
            Ok(out)
        })?;
        let next = page.next();
        let commits = page
            .commits
            .into_iter()
            .map(|c| {
                let changes = c
                    .iter()
                    .map(|(op, inner)| (op.sign().to_string(), PyQuad { inner }))
                    .collect();
                let complete = c.complete();
                Py::new(
                    py,
                    PyCommitChanges {
                        commit: Py::new(py, admin::PyCommit::from(c.commit))?,
                        added: c.added,
                        removed: c.removed,
                        complete,
                        changes,
                    },
                )
            })
            .collect::<PyResult<Vec<_>>>()?;
        Ok(PyChangePage {
            after: page.after,
            head: Py::new(py, admin::PyCommit::from(page.head))?,
            next,
            commits,
        })
    }
    #[pyo3(signature=(after,*,timeout=30.0,cancel=None,progress=None))]
    fn wait_for_commit(
        &self,
        py: Python<'_>,
        after: u64,
        timeout: f64,
        cancel: Option<&Bound<'_, PyAny>>,
        progress: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<u64>> {
        let ds = self.ds(py)?;
        if !timeout.is_finite() || timeout < 0.0 {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "timeout must be finite and nonnegative",
            ));
        }
        interrupt::controlled(py, cancel, progress, None, move |ctl| {
            let end = std::time::Instant::now()
                .checked_add(
                    Duration::try_from_secs_f64(timeout)
                        .map_err(|_| sparkles::Error::invalid("timeout is too large"))?,
                )
                .ok_or_else(|| sparkles::Error::invalid("timeout is too large"))?;
            loop {
                ctl.check()?;
                let remaining = end.saturating_duration_since(std::time::Instant::now());
                if let Some(head) = ds
                    .history()
                    .wait_for_commit(after, remaining.min(Duration::from_millis(20)))
                {
                    ctl.progress.report(1.0, "commit published");
                    return Ok(Some(head));
                }
                if std::time::Instant::now() >= end {
                    return Ok(None);
                }
            }
        })
    }
    fn prune(&self, py: Python<'_>) -> PyResult<u64> {
        let ds = self.write(py)?;
        py.detach(|| ds.history().prune()).py(py)
    }
    fn tick<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        {
            let r = py.detach(|| ds.history().tick()).py(py)?;
            admin::to_py(
                py,
                &serde_json::json!({"created":r.created,"expired":r.expired,"rotated":r.rotated,"warmed":r.warmed,"pruned":r.pruned}),
            )
        }
    }
    #[pyo3(signature=(*,subjects=None,predicates=None,objects=None,graphs=None,from_commit=None,to_commit=None,op=None,limit=0,descending=false,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn query<'py>(
        &self,
        py: Python<'py>,
        subjects: Option<&Bound<'py, PyAny>>,
        predicates: Option<&Bound<'py, PyAny>>,
        objects: Option<&Bound<'py, PyAny>>,
        graphs: Option<&Bound<'py, PyAny>>,
        from_commit: Option<&Bound<'py, PyAny>>,
        to_commit: Option<&Bound<'py, PyAny>>,
        op: Option<&str>,
        limit: usize,
        descending: bool,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let terms = |v: Option<&Bound<'py, PyAny>>| {
            v.map(|v| {
                v.try_iter()?
                    .map(|t| term_from_py(&t?))
                    .collect::<PyResult<Vec<_>>>()
            })
            .transpose()
            .map(Option::unwrap_or_default)
        };
        let mut q = sparkles::store::HistoryQuery {
            subjects: terms(subjects)?,
            objects: terms(objects)?,
            limit,
            descending,
            ..Default::default()
        };
        q.predicates = predicates
            .map(|v| {
                v.try_iter()?
                    .map(|t| named_node_from_py(&t?))
                    .collect::<PyResult<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        q.graphs = graphs
            .map(|v| {
                v.try_iter()?
                    .map(|t| graph_from_py(&t?))
                    .collect::<PyResult<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        q.from = from_commit
            .map(at_from_py)
            .transpose()?
            .map(sparkles::store::HistoryBound::At);
        q.to = to_commit
            .map(at_from_py)
            .transpose()?
            .map(sparkles::store::HistoryBound::At);
        q.op = match op {
            None => None,
            Some("+") => Some(sparkles::store::DiffOp::Add),
            Some("-") => Some(sparkles::store::DiffOp::Remove),
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "op must be '+' or '-'",
                ));
            }
        };
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            q.cancel = Some(ctl.cancel.flag());
            q.deadline = ctl.deadline;
            let r = ds.history().query(&q)?;
            ctl.progress.report(1.0, "read history");
            Ok(r)
        })?;
        let out = pyo3::types::PyDict::new(py);
        out.set_item("from", r.from)?;
        out.set_item("to", r.to)?;
        out.set_item("head", r.head)?;
        out.set_item("truncated", r.truncated)?;
        out.set_item("unrecorded", admin::to_py(py, &r.unrecorded)?)?;
        out.set_item(
            "changes",
            PyList::new(
                py,
                r.changes
                    .into_iter()
                    .map(|c| {
                        Py::new(
                            py,
                            PyHistoryChange {
                                seq: c.commit.seq,
                                op: c.op.sign().to_string(),
                                quad: PyQuad { inner: c.quad },
                            },
                        )
                    })
                    .collect::<PyResult<Vec<_>>>()?,
            )?,
        )?;
        Ok(out.into_any())
    }
}

#[pymethods]
impl PySettings {
    #[getter]
    fn compaction(&self, py: Python<'_>) -> PyCompactionSetting {
        PyCompactionSetting {
            owner: self.owner.clone_ref(py),
        }
    }
    #[getter]
    fn describe(&self, py: Python<'_>) -> PyDescribeSetting {
        PyDescribeSetting {
            owner: self.owner.clone_ref(py),
        }
    }
    #[getter]
    fn quota(&self, py: Python<'_>) -> PyQuotaSetting {
        PyQuotaSetting {
            owner: self.owner.clone_ref(py),
        }
    }
    #[getter]
    fn retention(&self, py: Python<'_>) -> PyRetentionSetting {
        PyRetentionSetting {
            owner: self.owner.clone_ref(py),
        }
    }
    #[getter]
    fn change_log(&self, py: Python<'_>) -> PyChangeLogSetting {
        PyChangeLogSetting {
            owner: self.owner.clone_ref(py),
        }
    }
}

#[pymethods]
impl PyCompactionSetting {
    fn get<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.settings().compaction().get()))
    }
    fn reset<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        admin::to_py(
            py,
            &py.detach(|| ds.settings().compaction().reset()).py(py)?,
        )
    }
    fn set(&self, py: Python<'_>, config: &Bound<'_, PyAny>) -> PyResult<()> {
        let ds = self.write(py)?;
        let c = admin::from_py(config, "settings")?;
        py.detach(|| ds.settings().compaction().set(c))
            .py(py)
            .map(|_| ())
    }
    fn status<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(
            py,
            &py.detach(|| ds.settings().compaction().status(&Default::default())),
        )
    }
}

#[pymethods]
impl PyDescribeSetting {
    fn get<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.settings().describe().status()))
    }
    fn reset<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        admin::to_py(py, &py.detach(|| ds.settings().describe().reset()).py(py)?)
    }
    fn set(&self, py: Python<'_>, config: &Bound<'_, PyAny>) -> PyResult<()> {
        let ds = self.write(py)?;
        let c = crate::dataset::describe_options(Default::default(), Some(config))?;
        py.detach(|| ds.settings().describe().set(c))
            .py(py)
            .map(|_| ())
    }
}

#[pymethods]
impl PyQuotaSetting {
    fn get<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.settings().quota().get()))
    }
    fn reset<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        admin::to_py(py, &py.detach(|| ds.settings().quota().reset()).py(py)?)
    }
    #[pyo3(signature=(*,max_bytes))]
    fn set<'py>(&self, py: Python<'py>, max_bytes: u64) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        admin::to_py(
            py,
            &py.detach(|| ds.settings().quota().set(max_bytes)).py(py)?,
        )
    }
}

#[pymethods]
impl PyRetentionSetting {
    fn get<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.settings().retention().get()))
    }
    fn reset<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        history_status(
            py,
            &ds,
            py.detach(|| ds.settings().retention().reset()).py(py)?,
        )
    }
    #[pyo3(signature=(config=None,*,keep_commits=None,keep_age=None,max_bytes=None,schedules=None,catalog=None))]
    #[allow(clippy::too_many_arguments)]
    fn set<'py>(
        &self,
        py: Python<'py>,
        config: Option<&Bound<'py, PyAny>>,
        keep_commits: Option<u64>,
        keep_age: Option<f64>,
        max_bytes: Option<u64>,
        schedules: Option<&Bound<'py, PyAny>>,
        catalog: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        if keep_age.is_some_and(|v| !v.is_finite() || v < 0.0) {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "keep_age must be finite and nonnegative",
            ));
        }
        let retention = config
            .map(|c| admin::from_py(c, "retention"))
            .transpose()?
            .or_else(|| {
                (keep_commits.is_some() || keep_age.is_some() || max_bytes.is_some()).then_some(
                    sparkles::history::Retention {
                        keep_commits,
                        keep_age_ms: keep_age.map(|s| (s * 1000.0).round() as u64),
                        max_bytes,
                    },
                )
            });
        let u = sparkles::handles::HistoryUpdate {
            retention,
            schedules: schedules
                .map(|c| admin::from_py(c, "schedules"))
                .transpose()?,
            catalog: catalog
                .map(|c| admin::from_py(c, "catalog horizon"))
                .transpose()?,
        };
        history_status(
            py,
            &ds,
            py.detach(|| ds.settings().retention().set(u)).py(py)?,
        )
    }
}

#[pymethods]
impl PyChangeLogSetting {
    fn get<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.settings().change_log().get()))
    }
    fn reset<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        admin::to_py(
            py,
            &py.detach(|| ds.settings().change_log().reset()).py(py)?,
        )
    }
    fn set(&self, py: Python<'_>, config: &Bound<'_, PyAny>) -> PyResult<()> {
        let ds = self.write(py)?;
        let c = admin::from_py(config, "settings")?;
        py.detach(|| ds.settings().change_log().set(c))
            .py(py)
            .map(|_| ())
    }
    fn status<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.settings().change_log().status()))
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySnapshots>()?;
    m.add_class::<PyHistory>()?;
    m.add_class::<PySettings>()?;
    m.add_class::<PyCompactionSetting>()?;
    m.add_class::<PyDescribeSetting>()?;
    m.add_class::<PyQuotaSetting>()?;
    m.add_class::<PyRetentionSetting>()?;
    m.add_class::<PyChangeLogSetting>()?;
    m.add_class::<PyDiff>()?;
    m.add_class::<PyCommitChanges>()?;
    m.add_class::<PyChangePage>()?;
    m.add_class::<PyHistoryChange>()?;
    Ok(())
}

fn history_status<'py>(
    py: Python<'py>,
    ds: &sparkles::Dataset,
    h: sparkles::history::HistoryStatus,
) -> PyResult<Bound<'py, PyAny>> {
    admin::to_py(
        py,
        &serde_json::json!({
            "dataset":ds.name().unwrap_or("dataset"),"datasetId":ds.dataset_id(),
            "head":h.head,"oldestReconstructable":h.oldest_reconstructable(),
            "reconstructable":h.reconstructable.iter().map(|(a,b)|serde_json::json!({"from":a,"to":b})).collect::<Vec<_>>(),
            "bytes":h.bytes,"retention":retention_json(h.retention),"snapshots":h.snapshots,
            "schedules":ds.settings().retention().get().schedules.iter().map(|s|serde_json::json!({"prefix":s.prefix,"every":format!("{}s",s.every_ms/1000),"keepLast":s.keep_last})).collect::<Vec<_>>(),
            "changeLog":ds.settings().change_log().status(),
            "generations":h.generations.iter().map(|g|serde_json::json!({"name":g.name,"baseSeq":g.base_seq,"endSeq":g.end_seq,"bytes":g.bytes,"current":g.current,"heldBy":g.held_by.iter().map(ToString::to_string).collect::<Vec<_>>() })).collect::<Vec<_>>(),
            "catalog":{"keepCommits":h.catalog.keep_commits,"keepAge":h.catalog.keep_age_ms.map(|ms|format!("{}s",ms/1000)),"firstRetained":h.first_commit},
            "cache":{"entries":h.cache_entries,"bytes":h.cache_bytes,"hits":h.hits,"misses":h.misses,"materializations":h.materializations}
        }),
    )
}

fn retention_json(r: sparkles::history::Retention) -> serde_json::Value {
    serde_json::json!({"keepCommits":r.keep_commits,"keepAge":r.keep_age_ms.map(|ms|format!("{}s",ms/1000)),"maxBytes":r.max_bytes})
}
