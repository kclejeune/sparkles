//! Materializing inferences (feature `reasoning`): the run that `ds.reasoning().run`
//! makes, the record it leaves, and the diagnostics of a dataset.

use super::freshness::{Freshness, freshness};
use super::{ReasoningRecord, RunChanges, RunInfo};
use crate::Dataset;
use crate::error::{ComponentError, Error, Result};
use crate::handles::from_anyhow;
use crate::sparql::QueryOptions;
use crate::store::Store;
use crate::task::Control;
use sparkles_reasoner::diagnostics::{DiagnoseOptions, DiagnosticsReport};
use sparkles_reasoner::{Extras, ImportMode, Inputs, Profile, ReasonReport};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A materialization (see [`Reasoning::run`](crate::handles::Reasoning::run)).
#[derive(Clone, Debug)]
pub struct ReasonRequest {
    pub profile: Profile,
    pub extras: Extras,
    /// the graphs the run reads; the default graph and its imports by default
    pub inputs: Inputs,
    /// update the previous materialization from the changes since it, when it can be
    pub incremental: bool,
    /// load again the imports that earlier runs fetched
    pub refresh_imports: bool,
    /// stop, and fail with a `superseded` error, as soon as a write waits for the
    /// writer lock that the run holds (for runs that a later run replaces anyway)
    pub yield_to_writers: bool,
    /// The rules that loading imports follows: the outbound policy, the file-load
    /// rules, the timeouts and the response ceiling of a SPARQL `LOAD`. Its `cancel` is
    /// the run's.
    pub load: QueryOptions,
}

impl Default for ReasonRequest {
    /// RDFS over the default graph, incrementally when the record allows it.
    fn default() -> ReasonRequest {
        ReasonRequest {
            profile: Profile::Rdfs,
            extras: Extras::default(),
            inputs: Inputs::default(),
            incremental: true,
            refresh_imports: false,
            yield_to_writers: false,
            load: QueryOptions::default(),
        }
    }
}

/// What a materialization did, and the record it left.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ReasonOutcome {
    pub report: ReasonReport,
    pub record: ReasoningRecord,
}

/// The error code of a run that yielded to a waiting write
/// ([`ReasonRequest::yield_to_writers`]).
pub const SUPERSEDED: &str = "superseded";

/// Materialize the inferences of `req` into `ds`, then record the run: the data first,
/// then the record, so a crash in between reads as stale.
pub(crate) fn run(ds: &Dataset, req: &ReasonRequest, ctl: &Control) -> Result<ReasonOutcome> {
    ctl.check()?;
    let state = ds.state();
    let store: &Store = &state.store;
    let cancel = ctl.cancel.flag();
    let fetched_before: Vec<String> = ds
        .reasoning_record()
        .map(|r| r.fetched_imports)
        .unwrap_or_default();
    let mut fetched = fetched_before.clone();
    let mut fetch_warnings = Vec::new();
    if req.inputs.imports == ImportMode::Fetch || req.refresh_imports {
        ctl.progress.report(0.02, "fetching imports");
        let qopts = QueryOptions {
            cancel: Some(cancel.clone()),
            ..req.load.clone()
        };
        let refresh = if req.refresh_imports {
            fetched_before
        } else {
            Vec::new()
        };
        let f = sparkles_reasoner::fetch_imports(store, &req.inputs, &refresh, &qopts)
            .map_err(|e| from_anyhow(e.context("fetching imports")))?;
        fetched.extend(f.fetched);
        fetch_warnings = f.warnings;
    }
    ctl.progress.report(0.05, "loading triples");
    // the reasoner reports progress only once it holds the writer lock
    let locked = Arc::new(AtomicBool::new(false));
    let progress: sparkles_reasoner::ProgressFn = {
        let locked = locked.clone();
        let p = ctl.progress.clone();
        Arc::new(move |f, msg: &str| {
            locked.store(true, Ordering::Relaxed);
            p.report(f, msg);
        })
    };
    let opts = sparkles_reasoner::ReasonOptions {
        progress: Some(progress),
        cancel: Some(cancel),
        inputs: req.inputs.clone(),
        ..Default::default()
    };
    let superseded = AtomicBool::new(false);
    let done = AtomicBool::new(false);
    let result = std::thread::scope(|s| {
        if req.yield_to_writers {
            // a newer write waiting for the lock supersedes this run
            s.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    // before that, the waiting writer may be this run itself
                    if locked.load(Ordering::Relaxed) && store.writers_waiting() > 0 {
                        superseded.store(true, Ordering::Relaxed);
                        ctl.cancel.cancel();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
        }
        let since = if req.incremental {
            incremental_since(ds.reasoning_record().as_ref(), store)
        } else {
            None
        };
        let inc = sparkles_reasoner::Incremental {
            since,
            cache: Some(&state.closure),
        };
        let r = sparkles_reasoner::materialize_incremental(
            store,
            &req.profile,
            &req.extras,
            inc,
            &opts,
        );
        done.store(true, Ordering::Relaxed);
        r
    });
    let report = match result {
        Ok(r) => r,
        Err(_) if superseded.load(Ordering::Relaxed) => {
            return Err(Error::Component(Box::new(ComponentError::new(
                "reasoner",
                SUPERSEDED,
                "superseded by a write",
            ))));
        }
        Err(_) if ctl.cancel.is_cancelled() => return Err(Error::Cancelled),
        Err(e) => return Err(from_anyhow(e)),
    };
    let mut record = recorded(&req.profile, &req.extras, &report, store);
    record_inputs(&mut record, &req.inputs, &report, fetched);
    record.warnings.splice(0..0, fetch_warnings);
    // a run keeps the dataset's automatic re-run setting
    record.auto = ds.reasoning_record().and_then(|r| r.auto);
    state.set_reasoning(Some(record.clone()))?;
    ctl.progress.report(1.0, "materialized");
    Ok(ReasonOutcome { report, record })
}

/// The record of a materialization.
pub fn recorded(
    profile: &Profile,
    extras: &Extras,
    report: &ReasonReport,
    store: &Store,
) -> ReasoningRecord {
    let receipt = report.receipt.as_ref();
    ReasoningRecord {
        reasoning_format: 2,
        profile: profile.name().to_string(),
        inferred: report.inferred,
        at: crate::builder::now_rfc3339(),
        // the run's own commit, or the head it read when it changed nothing
        commit: receipt.map(|r| r.commit.seq),
        position_source: receipt.map(|_| "commit".to_string()),
        dataset_id: Some(
            receipt
                .map_or(store.dataset_id(), |r| r.dataset_id)
                .to_string(),
        ),
        rules: match profile {
            Profile::Rules(t) => Some(t.clone()),
            _ => None,
        },
        vocabularies: extras.names(),
        geo_default_geometry: extras.geo_default_geometry,
        warnings: report.warnings.clone(),
        millis: Some(report.millis),
        inherited_stale: false,
        auto: None,
        run: Some(run_info(report)),
        ..Default::default()
    }
}

/// Record a run's input graphs: the configuration (unless it is the default), the graphs
/// read and watched and the imports (unless the run read the default graph alone), and
/// the imports fetched by it or by earlier runs that it still imports.
pub fn record_inputs(
    record: &mut ReasoningRecord,
    inputs: &Inputs,
    report: &ReasonReport,
    fetched: Vec<String>,
) {
    if *inputs != Inputs::default() {
        record.inputs = serde_json::to_value(inputs).ok();
    }
    let Some(r) = &report.inputs else {
        return;
    };
    let names = |g: Vec<sparkles_reasoner::GraphRef>| -> Vec<String> {
        g.iter().map(|g| g.as_str().to_string()).collect()
    };
    if !r.default_only() || !r.imports.is_empty() {
        record.input_graphs = Some(names(r.graphs.clone()));
        record.watched_graphs = Some(names(r.watched()));
        record.imports = r
            .imports
            .iter()
            .filter_map(|i| serde_json::to_value(i).ok())
            .collect();
    }
    let mut f: Vec<String> = fetched
        .into_iter()
        .filter(|iri| r.imports.iter().any(|i| &i.iri == iri && i.graph.is_some()))
        .collect();
    f.sort();
    f.dedup();
    record.fetched_imports = f;
}

/// How a run materialized, for its record.
pub fn run_info(report: &ReasonReport) -> RunInfo {
    RunInfo {
        method: report.method.as_str().to_string(),
        fallback: report.fallback.clone(),
        inferred_added: report.inferred_added,
        inferred_removed: report.inferred_removed,
        changes: report.changes.as_ref().map(|c| RunChanges {
            explicit_added: c.base_added,
            explicit_removed: c.base_removed,
            checked: c.checked,
            removed: c.removed,
            derived: c.derived,
            source: c.source.clone(),
        }),
    }
}

/// The commit of the recorded materialization that a run can update incrementally: the
/// record's own commit, when the record belongs to this dataset.
pub fn incremental_since(record: Option<&ReasoningRecord>, store: &Store) -> Option<u64> {
    let record = record?;
    if record.inherited_stale
        || record.position_source.as_deref() != Some("commit")
        || record.dataset_id.as_deref() != Some(store.dataset_id().to_string().as_str())
    {
        return None;
    }
    record.commit
}

/// One line on how a run went: `incremental: 3 explicit triples added, 1 removed; …`.
pub fn run_text(report: &ReasonReport) -> String {
    let graph = format!(
        "inferred graph +{} -{}",
        report.inferred_added, report.inferred_removed
    );
    match (&report.changes, &report.fallback) {
        (Some(c), _) => format!(
            "incremental: {} explicit triples added, {} removed; {} derived triples removed, {} added; {graph}",
            c.base_added, c.base_removed, c.removed, c.derived
        ),
        (None, Some(why)) => format!(
            "full, {} iterations, because {why}; {graph}",
            report.iterations
        ),
        (None, None) => format!("full, {} iterations; {graph}", report.iterations),
    }
}

impl ReasoningRecord {
    /// The profile the record re-runs, with its custom rules.
    pub fn profile(&self) -> Result<Profile> {
        if self.profile == "rules" {
            return match &self.rules {
                Some(t) => Ok(Profile::Rules(t.clone())),
                None => Err(Error::invalid(
                    "the recorded custom rules are not available to re-run",
                )),
            };
        }
        self.profile
            .parse()
            .map_err(|e: sparkles_reasoner::UnknownProfile| Error::invalid(e.to_string()))
    }

    /// The extras the record re-runs.
    pub fn extras(&self) -> Result<Extras> {
        Extras::parse(&self.vocabularies, self.geo_default_geometry)
            .map_err(|e| Error::invalid(e.to_string()))
    }

    /// The input graphs the record re-runs.
    pub fn run_inputs(&self) -> Result<Inputs> {
        match &self.inputs {
            None => Ok(Inputs::default()),
            Some(j) => serde_json::from_value(j.clone()).map_err(|e| Error::invalid(e.to_string())),
        }
    }

    /// The request that runs the record's profile, extras and inputs again.
    pub fn rerun(&self) -> Result<ReasonRequest> {
        Ok(ReasonRequest {
            profile: self.profile()?,
            extras: self.extras()?,
            inputs: self.run_inputs()?,
            ..ReasonRequest::default()
        })
    }
}

/// Inconsistency checks of a dataset's current state.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Diagnostics {
    pub report: DiagnosticsReport,
    /// the commit the checks read
    pub commit: u64,
    /// the recorded profile and the inferences' freshness at that commit, when the
    /// checks read the inferences
    pub inferences: Option<(String, Freshness)>,
}

/// The diagnostics of `store`'s current state, with the freshness of the inferences
/// that `record` describes when `opts.inferences` reads them.
pub fn diagnose(
    store: &Store,
    record: Option<&ReasoningRecord>,
    opts: &DiagnoseOptions,
) -> Result<Diagnostics> {
    let snap = store.snapshot();
    let commit = snap.commit;
    let report = sparkles_reasoner::diagnostics::diagnose(snap, opts).map_err(from_anyhow)?;
    let inferences = record
        .filter(|_| opts.inferences)
        .map(|r| (r.profile.clone(), freshness(r, store, commit)));
    Ok(Diagnostics {
        report,
        commit,
        inferences,
    })
}
