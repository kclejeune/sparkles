//! Branch metadata, merge previews and controlled merge operations.
use crate::terms::{graph_from_py, named_node_from_py, term_from_py};
use crate::{
    admin,
    errors::{EngineResult, invalid},
    handles::handle,
    interrupt,
};
use pyo3::prelude::*;
use sparkles::branch::{ConflictScope, MergeOptions, MergeOutcome, Take};
handle!(PyBranches, "Branches");
handle!(PyBranchSettings, "BranchSettings");
impl PyBranches {
    fn locking(&self, py: Python<'_>, branches: &[&str]) -> PyResult<sparkles::Dataset> {
        let ds = self.write(py)?;
        for name in branches {
            let branch = py.detach(|| ds.branch(name)).py(py)?;
            crate::dataset::check_transaction(py, &branch)?;
        }
        Ok(ds)
    }
}
#[pymethods]
impl PyBranches {
    #[getter]
    fn settings(&self, py: Python<'_>) -> PyBranchSettings {
        PyBranchSettings {
            owner: self.owner.clone_ref(py),
        }
    }
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.branches()).py(py)?)
    }
    fn get<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.branch_info(name)).py(py)?)
    }
    #[pyo3(signature=(name,*,from_branch="main".to_string(),at=None,protected=false,note=None,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn create<'py>(
        &self,
        py: Python<'py>,
        name: String,
        from_branch: String,
        at: Option<&Bound<'py, PyAny>>,
        protected: bool,
        note: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.locking(py, &[&from_branch])?;
        let at = at
            .map(crate::dataset::at_from_py)
            .transpose()?
            .unwrap_or(sparkles::history::At::Head);
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let r = ds.create_branch(
                &name,
                &sparkles::branch::BranchOptions {
                    from: from_branch,
                    at,
                    protected,
                    note,
                },
            )?;
            ctl.progress.report(1.0, "created branch");
            Ok(r)
        })?;
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(name,*,protected=None,note=None,new_name=None))]
    fn update<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        protected: Option<bool>,
        note: Option<&Bound<'py, PyAny>>,
        new_name: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        let note = note
            .map(|n| {
                if n.is_none() {
                    Ok(None)
                } else {
                    n.extract::<String>().map(Some)
                }
            })
            .transpose()?;
        let r = py
            .detach(|| {
                let mut r = ds.branch_info(name)?;
                if let Some(on) = protected {
                    r = ds.set_branch_protected(name, on)?;
                }
                if let Some(note) = note {
                    r = ds.set_branch_note(name, note)?;
                }
                if let Some(new) = new_name {
                    r = ds.rename_branch(name, new)?;
                }
                Ok(r)
            })
            .py(py)?;
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(name,*,force=false,reparent=false))]
    fn delete(&self, py: Python<'_>, name: &str, force: bool, reparent: bool) -> PyResult<()> {
        let ds = self.write(py)?;
        py.detach(|| {
            ds.delete_branch_with(name, &sparkles::branch::DeleteOptions { force, reparent })
        })
        .py(py)
    }
    #[pyo3(signature=(name,*,cancel=None,progress=None,timeout=None))]
    fn relink<'py>(
        &self,
        py: Python<'py>,
        name: String,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.locking(py, &[&name])?;
        let report = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            ds.relink_branch_with(&name, &Default::default(), &ctl)
        })?;
        admin::to_py(py, &report)
    }
    #[pyo3(signature=(*,branches=None,before=None,limit=100))]
    fn commit_graph<'py>(
        &self,
        py: Python<'py>,
        branches: Option<Vec<String>>,
        before: Option<&str>,
        limit: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let before = before.map(str::parse).transpose().py(py)?;
        let r = py
            .detach(|| {
                ds.commit_graph(&sparkles::store::CommitGraphOptions {
                    branches,
                    before,
                    limit,
                })
            })
            .py(py)?;
        let branches=r.branches.iter().map(|b|serde_json::json!({"name":b.name,"id":b.id,"ordinal":b.ordinal,"head":b.head,"from":b.from,"upstream":b.upstream,"created":sparkles::commit::rfc3339_ms(b.created_ms)})).collect::<Vec<_>>();
        let commits = r
            .commits
            .iter()
            .map(|c| {
                let mut j = serde_json::json!(sparkles::commit::AnnotatedCommit {
                    commit: &c.commit,
                    annotation: c.annotation.as_ref()
                });
                j["branch"] = serde_json::json!(c.branch);
                j["branchId"] = serde_json::json!(c.branch_id);
                j["parents"] = serde_json::json!(c.parents);
                j["mergedFrom"] = serde_json::json!(c.merged_from);
                j["replayedFrom"] = serde_json::json!(c.replayed_from);
                j
            })
            .collect::<Vec<_>>();
        admin::to_py(
            py,
            &serde_json::json!({"branches":branches,"commits":commits,"next":r.next.map(|n|n.to_string())}),
        )
    }
    #[pyo3(signature=(source,target="main".to_string(),*,ff_only=false,squash=false,replay=false,scope="cell",on_conflict=None,resolutions=None,exempt=None,expect_source=None,expect_target=None,include_inferences=false,max_quads=0,limit=100,message=None,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn merge<'py>(
        &self,
        py: Python<'py>,
        source: String,
        target: String,
        ff_only: bool,
        squash: bool,
        replay: bool,
        scope: &str,
        on_conflict: Option<&str>,
        resolutions: Option<&Bound<'py, PyAny>>,
        exempt: Option<&Bound<'py, PyAny>>,
        expect_source: Option<u64>,
        expect_target: Option<u64>,
        include_inferences: bool,
        max_quads: u64,
        limit: usize,
        message: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.locking(py, &[&source, &target])?;
        let mut o = MergeOptions {
            ff_only,
            squash,
            replay,
            scope: ConflictScope::parse(scope)
                .ok_or_else(|| invalid(py, "scope must be cell, subject or quad"))?,
            on_conflict: on_conflict
                .map(|t| {
                    Take::parse(t).ok_or_else(|| {
                        invalid(py, "on_conflict must be ours, theirs, base or union")
                    })
                })
                .transpose()?,
            expect_source,
            expect_target,
            include_inferences,
            max_quads,
            limit,
            ..Default::default()
        };
        o.resolutions = parse_resolutions(py, resolutions)?;
        o.exempt = exempt
            .map(|v| {
                v.try_iter()?
                    .map(|p| named_node_from_py(&p?))
                    .collect::<PyResult<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        o.write.message = message.map(Into::into);
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let o = o.with_control(&ctl);
            let r = ds.merge(&source, &target, &o)?;
            match r {
                MergeOutcome::UpToDate(r) | MergeOutcome::Merged(r) => Ok(merge_json(&r)),
                MergeOutcome::Conflicts(r) => Err(sparkles::Error::Conflict(
                    serde_json::to_string(&r).unwrap_or_else(|_| "merge conflict".into()),
                )),
            }
        })?;
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(source,target="main".to_string(),*,ff_only=false,squash=false,replay=false,scope="cell",on_conflict=None,resolutions=None,exempt=None,expect_source=None,expect_target=None,include_inferences=false,max_quads=0,limit=100,message=None,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn preview_merge<'py>(
        &self,
        py: Python<'py>,
        source: String,
        target: String,
        ff_only: bool,
        squash: bool,
        replay: bool,
        scope: &str,
        on_conflict: Option<&str>,
        resolutions: Option<&Bound<'py, PyAny>>,
        exempt: Option<&Bound<'py, PyAny>>,
        expect_source: Option<u64>,
        expect_target: Option<u64>,
        include_inferences: bool,
        max_quads: u64,
        limit: usize,
        message: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.locking(py, &[&source, &target])?;
        let mut o = MergeOptions {
            ff_only,
            squash,
            replay,
            scope: ConflictScope::parse(scope)
                .ok_or_else(|| invalid(py, "scope must be cell, subject or quad"))?,
            on_conflict: on_conflict
                .map(|t| {
                    Take::parse(t).ok_or_else(|| {
                        invalid(py, "on_conflict must be ours, theirs, base or union")
                    })
                })
                .transpose()?,
            expect_source,
            expect_target,
            include_inferences,
            max_quads,
            limit,
            ..Default::default()
        };
        o.resolutions = parse_resolutions(py, resolutions)?;
        o.exempt = exempt
            .map(|v| {
                v.try_iter()?
                    .map(|p| named_node_from_py(&p?))
                    .collect::<PyResult<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        o.write.message = message.map(Into::into);
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let o = o.with_control(&ctl);
            let r = ds.preview_merge(&source, &target, &o)?;
            ctl.progress.report(1.0, "preview complete");
            Ok(r)
        })?;
        admin::to_py(py, &merge_json(&r))
    }
    #[pyo3(signature=(commit,*,branch="main".to_string(),ff_only=false,squash=false,replay=false,scope="cell",on_conflict=None,resolutions=None,exempt=None,expect_source=None,expect_target=None,include_inferences=false,max_quads=0,limit=100,message=None,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn revert<'py>(
        &self,
        py: Python<'py>,
        commit: u64,
        branch: String,
        ff_only: bool,
        squash: bool,
        replay: bool,
        scope: &str,
        on_conflict: Option<&str>,
        resolutions: Option<&Bound<'py, PyAny>>,
        exempt: Option<&Bound<'py, PyAny>>,
        expect_source: Option<u64>,
        expect_target: Option<u64>,
        include_inferences: bool,
        max_quads: u64,
        limit: usize,
        message: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.locking(py, &[&branch])?;
        let mut o = MergeOptions {
            ff_only,
            squash,
            replay,
            scope: ConflictScope::parse(scope)
                .ok_or_else(|| invalid(py, "scope must be cell, subject or quad"))?,
            on_conflict: on_conflict
                .map(|t| {
                    Take::parse(t).ok_or_else(|| {
                        invalid(py, "on_conflict must be ours, theirs, base or union")
                    })
                })
                .transpose()?,
            expect_source,
            expect_target,
            include_inferences,
            max_quads,
            limit,
            ..Default::default()
        };
        o.resolutions = parse_resolutions(py, resolutions)?;
        o.exempt = exempt
            .map(|v| {
                v.try_iter()?
                    .map(|p| named_node_from_py(&p?))
                    .collect::<PyResult<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        o.write.message = message.map(Into::into);
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let o = o.with_control(&ctl);
            let r = ds.revert(&branch, commit, &o)?;
            match r {
                MergeOutcome::UpToDate(r) | MergeOutcome::Merged(r) => Ok(merge_json(&r)),
                MergeOutcome::Conflicts(r) => Err(sparkles::Error::Conflict(
                    serde_json::to_string(&r).unwrap_or_else(|_| "merge conflict".into()),
                )),
            }
        })?;
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(commit,*,branch="main".to_string(),ff_only=false,squash=false,replay=false,scope="cell",on_conflict=None,resolutions=None,exempt=None,expect_source=None,expect_target=None,include_inferences=false,max_quads=0,limit=100,message=None,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn preview_revert<'py>(
        &self,
        py: Python<'py>,
        commit: u64,
        branch: String,
        ff_only: bool,
        squash: bool,
        replay: bool,
        scope: &str,
        on_conflict: Option<&str>,
        resolutions: Option<&Bound<'py, PyAny>>,
        exempt: Option<&Bound<'py, PyAny>>,
        expect_source: Option<u64>,
        expect_target: Option<u64>,
        include_inferences: bool,
        max_quads: u64,
        limit: usize,
        message: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.locking(py, &[&branch])?;
        let mut o = MergeOptions {
            ff_only,
            squash,
            replay,
            scope: ConflictScope::parse(scope)
                .ok_or_else(|| invalid(py, "scope must be cell, subject or quad"))?,
            on_conflict: on_conflict
                .map(|t| {
                    Take::parse(t).ok_or_else(|| {
                        invalid(py, "on_conflict must be ours, theirs, base or union")
                    })
                })
                .transpose()?,
            expect_source,
            expect_target,
            include_inferences,
            max_quads,
            limit,
            ..Default::default()
        };
        o.resolutions = parse_resolutions(py, resolutions)?;
        o.exempt = exempt
            .map(|v| {
                v.try_iter()?
                    .map(|p| named_node_from_py(&p?))
                    .collect::<PyResult<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        o.write.message = message.map(Into::into);
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let o = o.with_control(&ctl);
            let r = ds.preview_revert(&branch, commit, &o)?;
            ctl.progress.report(1.0, "preview complete");
            Ok(r)
        })?;
        admin::to_py(py, &merge_json(&r))
    }
    #[pyo3(signature=(source,commit,target="main".to_string(),*,ff_only=false,squash=false,replay=false,scope="cell",on_conflict=None,resolutions=None,exempt=None,expect_source=None,expect_target=None,include_inferences=false,max_quads=0,limit=100,message=None,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn cherry_pick<'py>(
        &self,
        py: Python<'py>,
        source: String,
        commit: u64,
        target: String,
        ff_only: bool,
        squash: bool,
        replay: bool,
        scope: &str,
        on_conflict: Option<&str>,
        resolutions: Option<&Bound<'py, PyAny>>,
        exempt: Option<&Bound<'py, PyAny>>,
        expect_source: Option<u64>,
        expect_target: Option<u64>,
        include_inferences: bool,
        max_quads: u64,
        limit: usize,
        message: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.locking(py, &[&source, &target])?;
        let mut o = MergeOptions {
            ff_only,
            squash,
            replay,
            scope: ConflictScope::parse(scope)
                .ok_or_else(|| invalid(py, "scope must be cell, subject or quad"))?,
            on_conflict: on_conflict
                .map(|t| {
                    Take::parse(t).ok_or_else(|| {
                        invalid(py, "on_conflict must be ours, theirs, base or union")
                    })
                })
                .transpose()?,
            expect_source,
            expect_target,
            include_inferences,
            max_quads,
            limit,
            ..Default::default()
        };
        o.resolutions = parse_resolutions(py, resolutions)?;
        o.exempt = exempt
            .map(|v| {
                v.try_iter()?
                    .map(|p| named_node_from_py(&p?))
                    .collect::<PyResult<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        o.write.message = message.map(Into::into);
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let o = o.with_control(&ctl);
            let r = ds.cherry_pick(&source, commit, &target, &o)?;
            match r {
                MergeOutcome::UpToDate(r) | MergeOutcome::Merged(r) => Ok(merge_json(&r)),
                MergeOutcome::Conflicts(r) => Err(sparkles::Error::Conflict(
                    serde_json::to_string(&r).unwrap_or_else(|_| "merge conflict".into()),
                )),
            }
        })?;
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(source,commit,target="main".to_string(),*,ff_only=false,squash=false,replay=false,scope="cell",on_conflict=None,resolutions=None,exempt=None,expect_source=None,expect_target=None,include_inferences=false,max_quads=0,limit=100,message=None,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn preview_cherry_pick<'py>(
        &self,
        py: Python<'py>,
        source: String,
        commit: u64,
        target: String,
        ff_only: bool,
        squash: bool,
        replay: bool,
        scope: &str,
        on_conflict: Option<&str>,
        resolutions: Option<&Bound<'py, PyAny>>,
        exempt: Option<&Bound<'py, PyAny>>,
        expect_source: Option<u64>,
        expect_target: Option<u64>,
        include_inferences: bool,
        max_quads: u64,
        limit: usize,
        message: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.locking(py, &[&source, &target])?;
        let mut o = MergeOptions {
            ff_only,
            squash,
            replay,
            scope: ConflictScope::parse(scope)
                .ok_or_else(|| invalid(py, "scope must be cell, subject or quad"))?,
            on_conflict: on_conflict
                .map(|t| {
                    Take::parse(t).ok_or_else(|| {
                        invalid(py, "on_conflict must be ours, theirs, base or union")
                    })
                })
                .transpose()?,
            expect_source,
            expect_target,
            include_inferences,
            max_quads,
            limit,
            ..Default::default()
        };
        o.resolutions = parse_resolutions(py, resolutions)?;
        o.exempt = exempt
            .map(|v| {
                v.try_iter()?
                    .map(|p| named_node_from_py(&p?))
                    .collect::<PyResult<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        o.write.message = message.map(Into::into);
        let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            let o = o.with_control(&ctl);
            let r = ds.preview_cherry_pick(&source, commit, &target, &o)?;
            ctl.progress.report(1.0, "preview complete");
            Ok(r)
        })?;
        admin::to_py(py, &merge_json(&r))
    }
}
fn parse_resolutions(
    py: Python<'_>,
    values: Option<&Bound<'_, PyAny>>,
) -> PyResult<Vec<sparkles::branch::Resolution>> {
    let Some(values) = values else {
        return Ok(Vec::new());
    };
    values
        .try_iter()?
        .map(|row| {
            let row = row?;
            let graph = row
                .get_item("graph")
                .ok()
                .filter(|g| !g.is_none())
                .map(|g| graph_from_py(&g))
                .transpose()?
                .unwrap_or(oxrdf::GraphName::DefaultGraph);
            let subject = row
                .get_item("subject")
                .ok()
                .filter(|v| !v.is_none())
                .map(|v| term_from_py(&v))
                .transpose()?;
            let predicate = row
                .get_item("predicate")
                .ok()
                .filter(|v| !v.is_none())
                .map(|v| named_node_from_py(&v))
                .transpose()?;
            let take = row.get_item("take")?.extract::<String>()?;
            let take = if take == "objects" {
                Take::Objects(
                    row.get_item("objects")?
                        .try_iter()?
                        .map(|v| term_from_py(&v?))
                        .collect::<PyResult<Vec<_>>>()?,
                )
            } else {
                Take::parse(&take).ok_or_else(|| invalid(py, "unknown resolution take"))?
            };
            Ok(sparkles::branch::Resolution {
                graph,
                subject,
                predicate,
                take,
            })
        })
        .collect()
}
#[pymethods]
impl PyBranchSettings {
    fn get(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let ds = self.ds(py)?;
        Ok(py
            .detach(|| ds.merge_exempt())
            .py(py)?
            .into_iter()
            .map(|n| n.into_string())
            .collect())
    }
    fn set(&self, py: Python<'_>, predicates: &Bound<'_, PyAny>) -> PyResult<Vec<String>> {
        let ds = self.write(py)?;
        let values = predicates
            .try_iter()?
            .map(|v| named_node_from_py(&v?))
            .collect::<PyResult<Vec<_>>>()?;
        Ok(py
            .detach(|| ds.set_merge_exempt(&values))
            .py(py)?
            .into_iter()
            .map(|n| n.into_string())
            .collect())
    }
    fn reset(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let ds = self.write(py)?;
        Ok(py
            .detach(|| ds.set_merge_exempt(&[]))
            .py(py)?
            .into_iter()
            .map(|n| n.into_string())
            .collect())
    }
}
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyBranches>()?;
    m.add_class::<PyBranchSettings>()?;
    Ok(())
}

fn merge_json(r: &sparkles::branch::MergeReport) -> serde_json::Value {
    use serde_json::json;
    let side = |c: &sparkles::branch::NamedCommitRef| json!({"branch": c.branch, "seq": c.seq});
    let commit = r.commit.as_ref().filter(|c| c.committed).map(|c| {
        let mut j = json!(sparkles::commit::AnnotatedCommit {
            commit: &c.commit,
            annotation: Some(&c.annotation)
        });
        j["branch"] = json!(r.target.branch);
        j["branchId"] = json!(r.target.branch_id);
        j
    });
    let mut j = json!({"merged": r.merged, "upToDate": r.up_to_date, "fastForward": r.fast_forward,
        "squashed": r.squashed, "source": side(&r.source), "target": side(&r.target),
        "base": r.base.as_ref().map(side), "changes": {"inserted": r.inserted, "deleted": r.deleted},
        "conflicts": {"found": r.conflicts_found, "resolved": r.conflicts_resolved}, "commit": commit,
        "replayed": (!r.replayed.is_empty()).then(|| r.replayed.iter().map(|c| json!({"from": c.from, "commit": c.receipt.as_ref().map(|r|r.commit.seq)})).collect::<Vec<_>>()),
        "inferences": r.inferences_excluded.map(|n| json!({"excluded":n}))});
    if let Some(c) = &r.conflicts {
        if let serde_json::Value::Object(conflicts) = json!(c) {
            for (k, v) in conflicts {
                if k != "conflicts" {
                    j[k] = v;
                }
            }
        }
        j["conflictCount"] = json!(c.conflicts);
    }
    j
}
