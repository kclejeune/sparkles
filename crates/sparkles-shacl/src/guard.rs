//! Write-time validation: a [`CommitGuard`] that validates the post-state of every
//! commit against a shapes graph, configured per dataset in `<db>/validation.json`.
//!
//! * `reject`: a commit that would leave results at or above the threshold is not
//!   written (the store reports [`sparkles::Error::Rejected`]).
//! * `warn`: the commit is written; its receipt carries the findings.
//!
//! Enabling `reject` requires the current data to pass, and every later commit is
//! validated, so a committed head never has blocking results: judging the post-state
//! alone is then the same as judging what the write added. With `baseline:
//! "grandfather"`, `reject` may be enabled on data that does not pass, and a write is
//! judged by the blocking results it introduces.
//!
//! The guard knows the exact result counts of the head (after a full validation, kept
//! up to date by every validated write and persisted in `validation-status.json`). A
//! write then validates only the focus nodes whose results it can change
//! ([`crate::incremental`]) in the states before and after it: the counts move by the
//! difference, and the decision is the one a full validation would make. It falls back
//! to a full validation when the state of the head is unknown, the shapes change,
//! `rdfs:subClassOf` changes while shapes read classes, the write is a bulk rebuild, or
//! too many focus nodes are affected; shapes with SHACL-SPARQL constraints and recursive
//! shapes are validated in full on every write.

use crate::data::DataGraph;
use crate::incremental::{Fallback, Model, Tuning};
use crate::validate::{Sel, ShapeRun, validate_selected};
use crate::{PropertyPath, Shapes, ValidateOptions, ValidationReport, ValidationResult};
use anyhow::{Context, Result, bail};
use oxrdf::{NamedNode, Term};
use parking_lot::{Mutex, RwLock};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use sparkles::commit::CommitKind;
pub use sparkles::guard::config::{
    Baseline, BaselinePolicy, CONFIG_FILE, CheckRecord, Counters, DataGraphSel, STATUS_FILE,
};
use sparkles::guard::config::{
    CheckHistory, DecisionCounts, INFERRED_GRAPH as INFERRED, StatusFile, sha256_hex, write_atomic,
};
use sparkles::guard::{
    Candidate, Changes, CommitGuard, GuardLanguage, GuardMode, GuardStatus, Severity,
    SeverityCounts, Strategy, ValidationSummary, WriteOptions,
};
use sparkles::id::Id;
use sparkles::sparql::ctx::{DEFAULT_GRAPH_IRI, UNION_GRAPH_IRI};
use sparkles::store::{Snapshot, Store};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A shapes file copied into the database directory.
pub const SHAPES_FILE: &str = sparkles::guard::config::SHACL_SHAPES_FILE;

/// Write-time SHACL validation of one dataset (`validation.json`, format 2 with
/// `"language": "shacl"`; format 1 files, without `language`, are read too).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidationConfig {
    #[serde(default = "one")]
    pub format: u32,
    /// format 2: `shacl` (format 1 files have none)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<GuardLanguage>,
    pub mode: GuardMode,
    pub shapes: ShapesSource,
    #[serde(default)]
    pub data_graph: DataGraphSel,
    #[serde(default)]
    pub include_inferences: bool,
    #[serde(default = "violation")]
    pub threshold: Severity,
    /// `strict` (the default) or `grandfather`
    #[serde(default, skip_serializing_if = "BaselinePolicy::is_strict")]
    pub baseline: BaselinePolicy,
    #[serde(default = "ten")]
    pub timeout_seconds: f64,
    #[serde(default = "hundred")]
    pub report_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
}

fn one() -> u32 {
    1
}
fn violation() -> Severity {
    Severity::Violation
}
fn ten() -> f64 {
    10.0
}
fn hundred() -> usize {
    100
}

/// Where the shapes come from: named graphs of the dataset (read from the state being
/// validated, so changes to them are validated too), a file copied into the database, or
/// both, merged into one shapes graph.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShapesSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphs: Option<Vec<String>>,
    /// `validation-shapes.ttl` (set by the server when shapes are given inline)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// informational: where the file came from, and its SHA-256
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// shapes text given when setting the configuration (not stored in the file)
    #[serde(default, skip_serializing)]
    pub inline: Option<String>,
    /// media type of `inline` (default Turtle)
    #[serde(default, skip_serializing)]
    pub format: Option<String>,
}

impl ValidationConfig {
    /// Check the fields (not the shapes).
    pub fn check(&self) -> Result<()> {
        if !(1..=2).contains(&self.format) {
            bail!("unknown validation.json format {}", self.format);
        }
        if let Some(l) = self.language
            && l != GuardLanguage::Shacl
        {
            bail!(
                "this is a configuration for write-time {} validation, not SHACL",
                l.title()
            );
        }
        if !(self.timeout_seconds.is_finite() && self.timeout_seconds > 0.0) {
            bail!("timeoutSeconds must be a positive number");
        }
        if !(1..=10_000).contains(&self.report_limit) {
            bail!("reportLimit must be between 1 and 10000");
        }
        if let DataGraphSel::Named(n) = &self.data_graph
            && n != "default"
            && n != "union"
        {
            bail!("dataGraph must be \"default\", \"union\" or a list of graph IRIs");
        }
        let s = &self.shapes;
        if self.mode != GuardMode::Off
            && s.graphs.is_none()
            && s.file.is_none()
            && s.inline.is_none()
        {
            bail!("shapes: give graphs, a file (inline shapes), or both");
        }
        for g in s.graphs.iter().flatten() {
            oxrdf::NamedNode::new(g.as_str()).with_context(|| format!("shapes graph <{g}>"))?;
        }
        Ok(())
    }

    fn shapes_graphs(&self) -> &[String] {
        self.shapes.graphs.as_deref().unwrap_or(&[])
    }

    /// Validation options for this configuration over `snap`, or `None` when the data
    /// graph is a list of graphs none of which exists (nothing to validate).
    fn validate_options(&self, snap: &Snapshot) -> Option<ValidateOptions> {
        let mut o = ValidateOptions {
            exclude_graphs: self.shapes_graphs().to_vec(),
            ..Default::default()
        };
        if self.include_inferences {
            o.extra_graphs.push(INFERRED.into());
        } else {
            o.exclude_graphs.push(INFERRED.into());
        }
        match &self.data_graph {
            DataGraphSel::Named(n) if n == "union" => o.data_graph = Some(UNION_GRAPH_IRI.into()),
            DataGraphSel::Named(_) => {}
            DataGraphSel::Graphs(gs) => {
                let exists = |g: &str| g == DEFAULT_GRAPH_IRI || snap.lookup_iri(g).is_some();
                let mut present = gs.iter().filter(|g| exists(g));
                o.data_graph = Some(present.next()?.clone());
                o.extra_graphs.extend(present.cloned());
            }
        }
        Some(o)
    }

    /// Whether a change to graph `g` can change the validation (data or shapes).
    fn touches(&self, snap: &Snapshot, g: u64) -> (bool, bool) {
        let id = |iri: &str| {
            if iri == DEFAULT_GRAPH_IRI {
                Some(Id::DEFAULT_GRAPH.0)
            } else {
                snap.lookup_iri(iri).map(|i| i.0)
            }
        };
        let shapes = self.shapes_graphs().iter().any(|s| id(s) == Some(g));
        let inferred = id(INFERRED) == Some(g);
        let data = if shapes {
            false
        } else if inferred {
            self.include_inferences
        } else {
            match &self.data_graph {
                DataGraphSel::Named(n) if n == "union" => true,
                DataGraphSel::Named(_) => snap.union_default_graph || g == Id::DEFAULT_GRAPH.0,
                DataGraphSel::Graphs(gs) => gs.iter().any(|x| id(x) == Some(g)),
            }
        };
        (data, shapes)
    }
}

/// Shapes and their incremental analysis, replaced together.
#[derive(Clone)]
struct Loaded {
    shapes: Arc<Shapes>,
    model: Arc<Model>,
}

impl Loaded {
    fn new(shapes: Shapes) -> Loaded {
        let model = Model::new(&shapes);
        Loaded {
            shapes: Arc::new(shapes),
            model: Arc::new(model),
        }
    }
}

/// What the guard knows exactly about the validation of a commit (with the shapes of
/// its [`Loaded`]).
#[derive(Clone, Debug)]
struct Exact {
    commit: u64,
    /// every result, by severity
    counts: SeverityCounts,
    /// the results of the shapes validated in full on every write, by shape (the shapes
    /// missing were not counted on their own since the shapes were loaded)
    per_shape: FxHashMap<usize, SeverityCounts>,
    /// focus nodes by shape at the last full validation
    focus: Arc<Vec<Option<usize>>>,
}

struct Pending {
    seq: u64,
    loaded: Option<Loaded>,
    baseline: Baseline,
    exact: Option<Exact>,
}

/// A validation and the exact state it leaves.
struct Checked {
    summary: ValidationSummary,
    exact: Exact,
}

/// `GET /$/validation/{ds}` status.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationStatus {
    pub mode: GuardMode,
    pub shape_count: usize,
    pub baseline: Option<Baseline>,
    pub last_full_millis: Option<u64>,
    pub counters: Counters,
    pub warnings: Vec<String>,
    /// how writes are validated
    pub incremental: IncrementalStatus,
    /// the last validated write
    pub last_check: Option<CheckRecord>,
    /// the last rejected writes, newest first
    pub recent_rejections: Vec<CheckRecord>,
}

/// How the shapes are validated on a write.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncrementalStatus {
    /// shapes with targets validated on the focus nodes a write affects
    pub local_shapes: usize,
    /// shapes with targets validated in full on every write, and why
    pub full_shapes: Vec<FullShape>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FullShape {
    pub shape: String,
    pub reason: &'static str,
}

/// Write-time SHACL validation of one store.
pub struct ShaclGuard {
    cfg: ValidationConfig,
    loaded: RwLock<Loaded>,
    pending: Mutex<Option<Pending>>,
    baseline: Mutex<Option<Baseline>>,
    exact: Mutex<Option<Exact>>,
    counters: DecisionCounts,
    last_full: AtomicU64,
    tuning: RwLock<Tuning>,
    history: CheckHistory,
    /// the database directory and the SHA-256 of its `validation.json`, where the state
    /// of each validated commit is kept
    persist: Option<(PathBuf, String)>,
    /// the shapes file's triples, when the shapes are graphs and a file: they are merged
    /// with the graphs whenever a write changes them
    file_shapes: Option<Arc<oxrdf::Graph>>,
}

/// The data graph of one state, ready to validate.
struct State {
    data: DataGraph,
    ids: Vec<Id>,
    opts: ValidateOptions,
}

impl ShaclGuard {
    fn new(cfg: ValidationConfig, loaded: Loaded, persist: Option<(PathBuf, String)>) -> Self {
        ShaclGuard {
            cfg,
            loaded: RwLock::new(loaded),
            pending: Mutex::new(None),
            baseline: Mutex::new(None),
            exact: Mutex::new(None),
            counters: DecisionCounts::default(),
            last_full: AtomicU64::new(u64::MAX),
            tuning: RwLock::new(Tuning::default()),
            history: CheckHistory::default(),
            persist,
            file_shapes: None,
        }
    }

    pub fn config(&self) -> &ValidationConfig {
        &self.cfg
    }

    /// The limits past which a write is validated in full.
    pub fn tuning(&self) -> Tuning {
        *self.tuning.read()
    }

    /// Change the limits past which a write is validated in full (for tests and
    /// benchmarks).
    #[doc(hidden)]
    pub fn set_tuning(&self, t: Tuning) {
        *self.tuning.write() = t;
    }

    fn count(&self, s: GuardStatus) {
        self.counters.count(s);
    }

    fn grandfather(&self) -> bool {
        self.cfg.baseline == BaselinePolicy::Grandfather
    }

    pub fn status(&self) -> ValidationStatus {
        let loaded = self.loaded.read().clone();
        let shape_count = loaded.shapes.len();
        let last = self.last_full.load(Ordering::Relaxed);
        let mut warnings = Vec::new();
        if shape_count == 0 {
            warnings.push("the shapes graph has no shapes".to_string());
        }
        if last != u64::MAX && last > 1000 {
            warnings.push(format!(
                "full validation took {last} ms; writes that are not validated incrementally wait for it"
            ));
        }
        let full_shapes: Vec<FullShape> = loaded
            .model
            .global()
            .map(|(si, f)| FullShape {
                shape: loaded.shapes.shapes()[si].node.to_string(),
                reason: f.name(),
            })
            .collect();
        let validated = (0..shape_count)
            .filter(|&si| loaded.model.validated(si))
            .count();
        ValidationStatus {
            mode: self.cfg.mode,
            shape_count,
            baseline: self.baseline.lock().clone(),
            last_full_millis: (last != u64::MAX).then_some(last),
            counters: self.counters.get(),
            warnings,
            incremental: IncrementalStatus {
                local_shapes: validated - full_shapes.len(),
                full_shapes,
            },
            last_check: self.history.last(),
            recent_rejections: self.history.rejected(),
        }
    }

    fn deadline(&self, t0: Instant, o: &WriteOptions) -> Instant {
        let d = t0 + Duration::from_secs_f64(self.cfg.timeout_seconds);
        o.deadline.map_or(d, |x| d.min(x))
    }

    fn limit(&self, o: &WriteOptions) -> usize {
        o.report_limit
            .unwrap_or(self.cfg.report_limit)
            .clamp(1, 10_000)
    }

    /// The data graph of `snap` (`None`: none of the listed data graphs exists).
    fn state(
        &self,
        snap: &Arc<Snapshot>,
        shapes: &Shapes,
        o: &WriteOptions,
    ) -> sparkles::Result<Option<State>> {
        let Some(mut vo) = self.cfg.validate_options(snap) else {
            return Ok(None);
        };
        vo.cancel = o.cancel.clone();
        let (data, ids) = DataGraph::new(
            snap.clone(),
            vo.data_graph.as_deref(),
            &vo.extra_graphs,
            &vo.exclude_graphs,
            shapes,
        )
        .map_err(engine_error)?;
        Ok(Some(State {
            data,
            ids,
            opts: vo,
        }))
    }

    /// Validate the shapes on the focus nodes `sel` picks in one state.
    fn run(
        st: Option<&mut State>,
        shapes: &Shapes,
        sel: &[Sel],
        ids_of: Option<&Snapshot>,
        deadline: Instant,
    ) -> sparkles::Result<Vec<ShapeRun>> {
        match st {
            None => Ok((0..shapes.len()).map(|_| ShapeRun::default()).collect()),
            Some(st) => {
                st.opts.timeout = Some(deadline.saturating_duration_since(Instant::now()));
                validate_selected(&mut st.data, &st.ids, shapes, &st.opts, sel, ids_of)
                    .map_err(engine_error)
            }
        }
    }

    /// Validate all of `view` with `loaded`. In grandfather mode, `before` (the state
    /// before the write, with the shapes it had) is validated too, and the results
    /// `view` adds decide; without it nothing counts as added.
    fn full(
        &self,
        view: &Arc<Snapshot>,
        loaded: &Loaded,
        before: Option<(&Arc<Snapshot>, &Loaded)>,
        o: &WriteOptions,
        reason: Option<Fallback>,
        commit: u64,
    ) -> sparkles::Result<Checked> {
        let t0 = Instant::now();
        let deadline = self.deadline(t0, o);
        let shapes = &loaded.shapes;
        let mut post = self.state(view, shapes, o)?;
        let runs = Self::run(
            post.as_mut(),
            shapes,
            &vec![Sel::All; shapes.len()],
            None,
            deadline,
        )?;
        let pre = match before {
            Some((base, old)) if self.grandfather() => {
                let mut st = self.state(base, &old.shapes, o)?;
                let runs = Self::run(
                    st.as_mut(),
                    &old.shapes,
                    &vec![Sel::All; old.shapes.len()],
                    None,
                    deadline,
                )?;
                Some(runs.into_iter().flat_map(|r| r.results).collect::<Vec<_>>())
            }
            _ => None,
        };
        let mut focus = vec![None; shapes.len()];
        let mut per_shape = FxHashMap::default();
        let mut counts = SeverityCounts::default();
        let mut focus_total = 0u64;
        for (si, r) in runs.iter().enumerate() {
            if !loaded.model.validated(si) {
                continue;
            }
            let c = counts_of(&r.results);
            add(&mut counts, &c);
            focus[si] = Some(r.focus);
            focus_total += r.focus as u64;
        }
        for (si, _) in loaded.model.global() {
            per_shape.insert(si, counts_of(&runs[si].results));
        }
        let listed: Vec<ValidationResult> = runs.into_iter().flat_map(|r| r.results).collect();
        let introduced = self.grandfather().then(|| match &pre {
            Some(pre) => new_blocking(&listed, pre, self.cfg.threshold),
            None => vec![false; listed.len()],
        });
        let ms = t0.elapsed().as_millis() as u64;
        self.last_full.store(ms, Ordering::Relaxed);
        let summary = summarize(
            &self.cfg,
            listed,
            introduced,
            counts,
            self.limit(o),
            ms,
            Strategy::Full,
            reason,
            focus_total,
        );
        Ok(Checked {
            summary,
            exact: Exact {
                commit,
                counts,
                per_shape,
                focus: Arc::new(focus),
            },
        })
    }

    /// Validate the focus nodes the changed triples can affect, in the states before
    /// and after the write, and move the counts of `exact` by the difference; or say
    /// why the write must be validated in full.
    fn incremental(
        &self,
        c: &Candidate<'_>,
        loaded: &Loaded,
        exact: &Exact,
        changes: &[[Id; 3]],
    ) -> sparkles::Result<Result<Checked, Fallback>> {
        let t0 = Instant::now();
        let deadline = self.deadline(t0, c.opts);
        let (shapes, model) = (&loaded.shapes, &loaded.model);
        let base = Arc::new(c.base.clone());
        let mut post = self.state(&c.view, shapes, c.opts)?;
        let mut pre = self.state(&base, shapes, c.opts)?;
        let tuning = self.tuning();
        let mut extra;
        let mut changes = changes;
        if model.uses_classes() {
            match class_changes(&c.view, [post.as_ref(), pre.as_ref()], changes, &tuning)? {
                Some(more) if !more.is_empty() => {
                    extra = changes.to_vec();
                    extra.extend(more);
                    changes = &extra;
                }
                Some(_) => {}
                None => return Ok(Err(Fallback::Subclass)),
            }
        }
        let affected = model
            .affected(
                &c.view,
                [
                    post.as_ref().map(|s| &s.data),
                    pre.as_ref().map(|s| &s.data),
                ],
                changes,
                &tuning,
                &exact.focus,
            )
            .map_err(engine_error)?;
        let affected = match affected {
            Ok(a) => a,
            Err(f) => return Ok(Err(f)),
        };
        let n = shapes.len();
        let mut sel_post = vec![Sel::Skip; n];
        let mut sel_pre = vec![Sel::Skip; n];
        // the counts before the write of shapes validated in full, when known
        let mut stored: Vec<Option<SeverityCounts>> = vec![None; n];
        let mut fallback = None;
        for (si, f) in model.global() {
            sel_post[si] = Sel::All;
            match exact.per_shape.get(&si) {
                Some(c) if !self.grandfather() => stored[si] = Some(*c),
                _ => sel_pre[si] = Sel::All,
            }
            fallback.get_or_insert(f);
        }
        for (si, nodes) in affected.into_iter().enumerate() {
            if let Some(nodes) = nodes
                && !nodes.is_empty()
            {
                sel_pre[si] = Sel::Nodes(nodes.clone());
                sel_post[si] = Sel::Nodes(nodes);
            }
        }
        let post_runs = Self::run(post.as_mut(), shapes, &sel_post, None, deadline)?;
        let pre_runs = Self::run(pre.as_mut(), shapes, &sel_pre, Some(&c.view), deadline)?;
        // the results elsewhere are unchanged: move the counts by the difference
        let mut counts = exact.counts;
        let mut per_shape = exact.per_shape.clone();
        let mut focus = 0u64;
        for si in 0..n {
            if matches!(sel_post[si], Sel::Skip) {
                continue;
            }
            let after = counts_of(&post_runs[si].results);
            let before = stored[si].unwrap_or_else(|| counts_of(&pre_runs[si].results));
            add(&mut counts, &after);
            if !sub(&mut counts, &before) {
                // the counts of the head do not hold what it has: they are wrong
                return Ok(Err(Fallback::Baseline));
            }
            if matches!(sel_post[si], Sel::All) {
                per_shape.insert(si, after);
            }
            focus += post_runs[si].focus as u64;
        }
        let listed: Vec<ValidationResult> = post_runs.into_iter().flat_map(|r| r.results).collect();
        let introduced = self.grandfather().then(|| {
            let pre: Vec<ValidationResult> = pre_runs.into_iter().flat_map(|r| r.results).collect();
            new_blocking(&listed, &pre, self.cfg.threshold)
        });
        let summary = summarize(
            &self.cfg,
            listed,
            introduced,
            counts,
            self.limit(c.opts),
            t0.elapsed().as_millis() as u64,
            Strategy::Incremental,
            fallback,
            focus,
        );
        Ok(Ok(Checked {
            summary,
            exact: Exact {
                commit: c.base.commit + 1,
                counts,
                per_shape,
                focus: exact.focus.clone(),
            },
        }))
    }

    /// A write that cannot change the results: the state of the head carries over.
    fn skipped(&self, seq: u64) -> ValidationSummary {
        self.count(GuardStatus::Skipped);
        let baseline = self
            .baseline
            .lock()
            .clone()
            .map(|b| Baseline { commit: seq, ..b });
        let exact = self
            .exact
            .lock()
            .clone()
            .map(|e| Exact { commit: seq, ..e });
        if let Some(baseline) = baseline {
            *self.pending.lock() = Some(Pending {
                seq,
                loaded: None,
                baseline,
                exact,
            });
        }
        let mut s =
            ValidationSummary::empty(GuardStatus::Skipped, self.cfg.mode, self.cfg.threshold);
        s.limit = self.cfg.report_limit;
        s
    }

    /// Validate a write to the data graph with unchanged shapes: incrementally when the
    /// guard knows the state of the head exactly, in full otherwise.
    fn check_data(&self, c: &Candidate<'_>, loaded: &Loaded) -> sparkles::Result<Checked> {
        let seq = c.base.commit + 1;
        let base = Arc::new(c.base.clone());
        let full = |reason| {
            self.full(
                &c.view,
                loaded,
                Some((&base, loaded)),
                c.opts,
                Some(reason),
                seq,
            )
        };
        let Changes::Log(log) = c.changes else {
            return full(Fallback::Bulk);
        };
        let exact = self.exact.lock().clone();
        let Some(exact) = exact.filter(|e| e.commit == c.base.commit) else {
            return full(Fallback::Baseline);
        };
        if self.cfg.mode == GuardMode::Reject
            && !self.grandfather()
            && blocking_of(&exact.counts, self.cfg.threshold) > 0
        {
            // strict: the report must show the results that block every write
            return full(Fallback::Baseline);
        }
        // the changed triples of the data graph
        let mut graphs: FxHashMap<u64, bool> = FxHashMap::default();
        let mut seen: FxHashSet<[Id; 3]> = FxHashSet::default();
        let mut changes: Vec<[Id; 3]> = Vec::new();
        for (_, q) in log.iter() {
            let data = *graphs
                .entry(q[3].0)
                .or_insert_with(|| self.cfg.touches(&c.view, q[3].0).0);
            if data && seen.insert([q[0], q[1], q[2]]) {
                changes.push([q[0], q[1], q[2]]);
            }
        }
        match self.incremental(c, loaded, &exact, &changes)? {
            Ok(checked) => Ok(checked),
            Err(f) => full(f),
        }
    }

    /// Whether the changed data triples of a write touch a predicate some shape reads.
    fn reads_changes(&self, c: &Candidate<'_>, loaded: &Loaded) -> bool {
        let Changes::Log(log) = c.changes else {
            return true;
        };
        let mut graphs: FxHashMap<u64, bool> = FxHashMap::default();
        let preds: FxHashSet<Id> = log
            .iter()
            .filter(|(_, q)| {
                *graphs
                    .entry(q[3].0)
                    .or_insert_with(|| self.cfg.touches(&c.view, q[3].0).0)
            })
            .map(|(_, q)| q[1])
            .collect();
        loaded.model.reads_any(&c.view, &preds)
    }
}

/// A changed `rdfs:subClassOf` edge `(s, rdfs:subClassOf, o)` changes which classes
/// have the instances of `s` (and of its subclasses) as instances, and nothing else that
/// `sh:class` and class targets read. Those instances, in the states before and after
/// the write, are returned as changed `rdf:type` edges, so the shapes that read their
/// types validate them; `None` when there are more than the tuning's `max_visit`.
fn class_changes(
    view: &Snapshot,
    states: [Option<&State>; 2],
    changes: &[[Id; 3]],
    tuning: &Tuning,
) -> sparkles::Result<Option<Vec<[Id; 3]>>> {
    let Some(sub) = view.lookup_iri(crate::vocab::rdfs::SUB_CLASS_OF.as_str()) else {
        return Ok(Some(Vec::new()));
    };
    let ty = view
        .lookup_iri(crate::vocab::rdf::TYPE.as_str())
        .unwrap_or(Id::UNDEF);
    let mut out = Vec::new();
    let mut seen = FxHashSet::default();
    for &[s, p, _] in changes {
        if p != sub {
            continue;
        }
        for st in states.iter().flatten() {
            for x in st.data.instances(s).map_err(engine_error)? {
                if seen.insert(x) {
                    out.push([x, ty, s]);
                    if out.len() > tuning.max_visit {
                        return Ok(None);
                    }
                }
            }
        }
    }
    Ok(Some(out))
}

/// An error of the validation engine as a store error.
fn engine_error(e: anyhow::Error) -> sparkles::Error {
    let msg = format!("{e:#}");
    match e.downcast::<sparkles::Error>() {
        Ok(e) => e,
        Err(_) if msg.contains("timed out") => sparkles::Error::Timeout,
        Err(_) if msg.contains("cancel") => sparkles::Error::Cancelled,
        Err(_) => sparkles::Error::Invalid(format!("SHACL validation failed: {msg}")),
    }
}

fn severity_of(iri: &str) -> Severity {
    match iri {
        "http://www.w3.org/ns/shacl#Warning" => Severity::Warning,
        "http://www.w3.org/ns/shacl#Info" => Severity::Info,
        // SHACL 1.2 sh:Debug / sh:Trace rank below Info; any other IRI fails closed
        "http://www.w3.org/ns/shacl#Debug" | "http://www.w3.org/ns/shacl#Trace" => Severity::Info,
        _ => Severity::Violation,
    }
}

fn counts_of(results: &[ValidationResult]) -> SeverityCounts {
    let mut c = SeverityCounts::default();
    for r in results {
        match severity_of(r.severity.as_str()) {
            Severity::Violation => c.violation += 1,
            Severity::Warning => c.warning += 1,
            Severity::Info => c.info += 1,
        }
    }
    c
}

fn add(a: &mut SeverityCounts, b: &SeverityCounts) {
    a.violation += b.violation;
    a.warning += b.warning;
    a.info += b.info;
}

/// `a -= b`; `false` (and `a` unchanged) when `b` does not fit.
fn sub(a: &mut SeverityCounts, b: &SeverityCounts) -> bool {
    if b.violation > a.violation || b.warning > a.warning || b.info > a.info {
        return false;
    }
    a.violation -= b.violation;
    a.warning -= b.warning;
    a.info -= b.info;
    true
}

/// The results at or above `threshold`.
fn blocking_of(c: &SeverityCounts, threshold: Severity) -> u64 {
    match threshold {
        Severity::Violation => c.violation,
        Severity::Warning => c.violation + c.warning,
        Severity::Info => c.violation + c.warning + c.info,
    }
}

/// What identifies a result across states (its messages aside).
type ResultKey = (
    Term,
    Option<PropertyPath>,
    Option<Term>,
    Term,
    NamedNode,
    Option<Term>,
);

fn key(r: &ValidationResult) -> ResultKey {
    (
        r.focus_node.clone(),
        r.result_path.clone(),
        r.value.clone(),
        r.source_shape.clone(),
        r.source_constraint_component.clone(),
        r.source_constraint.clone(),
    )
}

/// For each result of `after`: whether it is blocking and not among `before` (results
/// are matched as a multiset).
fn new_blocking(
    after: &[ValidationResult],
    before: &[ValidationResult],
    threshold: Severity,
) -> Vec<bool> {
    let mut left: FxHashMap<ResultKey, usize> = FxHashMap::default();
    for r in before {
        *left.entry(key(r)).or_default() += 1;
    }
    after
        .iter()
        .map(|r| match left.get_mut(&key(r)) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => severity_of(r.severity.as_str()) >= threshold,
        })
        .collect()
}

/// Rank and bound the results listed, and decide under `cfg` with the exact counts of
/// the state (`introduced`: grandfather mode, which listed results are new and block).
#[allow(clippy::too_many_arguments)]
fn summarize(
    cfg: &ValidationConfig,
    listed: Vec<ValidationResult>,
    introduced: Option<Vec<bool>>,
    counts: SeverityCounts,
    limit: usize,
    millis: u64,
    strategy: Strategy,
    fallback: Option<Fallback>,
    focus_nodes: u64,
) -> ValidationSummary {
    let introduced_n = introduced
        .as_ref()
        .map(|v| v.iter().filter(|x| **x).count() as u64);
    let n = listed.len();
    let mut ranked: Vec<(bool, Severity, String, String, ValidationResult)> = listed
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            (
                introduced.as_ref().is_some_and(|v| v[i]),
                severity_of(r.severity.as_str()),
                r.source_shape.to_string(),
                r.focus_node.to_string(),
                r,
            )
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    let blocking = blocking_of(&counts, cfg.threshold);
    let total = counts.violation + counts.warning + counts.info;
    let kept: Vec<ValidationResult> = ranked
        .into_iter()
        .take(limit)
        .map(|(_, _, _, _, r)| r)
        .collect();
    let status = match (introduced_n.unwrap_or(blocking) > 0, cfg.mode) {
        (true, GuardMode::Reject) => GuardStatus::Rejected,
        (true, _) => GuardStatus::Warned,
        (false, _) => GuardStatus::Passed,
    };
    let report_turtle = (status == GuardStatus::Rejected).then(|| {
        ValidationReport {
            conforms: total == 0,
            results: kept.clone(),
        }
        .to_turtle()
    });
    ValidationSummary {
        language: GuardLanguage::Shacl,
        status,
        mode: cfg.mode,
        strategy,
        threshold: cfg.threshold,
        conforms: total == 0,
        blocking,
        total,
        by_severity: counts,
        limit,
        truncated: n > limit,
        millis,
        results: kept.iter().map(crate::report::result_json).collect(),
        shapes_error: None,
        introduced: introduced_n,
        focus_nodes: Some(focus_nodes),
        fallback: fallback.map(|f| f.name().to_string()),
        report_turtle,
    }
}

fn baseline_of(s: &ValidationSummary, commit: u64) -> Baseline {
    Baseline {
        commit,
        conforms: Some(s.blocking == 0),
        blocking: s.blocking,
        total: s.total,
        by_severity: s.by_severity,
        millis: s.millis,
    }
}

impl CommitGuard for ShaclGuard {
    fn check(&self, c: &Candidate<'_>) -> sparkles::Result<ValidationSummary> {
        let seq = c.base.commit + 1;
        // relevance: a write that touches neither the data graph nor the shapes cannot
        // change the report
        let (mut data, mut shapes_changed) = (false, false);
        match c.changes.graphs() {
            Some(gs) => {
                for g in gs {
                    let (d, s) = self.cfg.touches(&c.view, g);
                    data |= d;
                    shapes_changed |= s;
                }
            }
            None => (data, shapes_changed) = (true, !self.cfg.shapes_graphs().is_empty()),
        }
        if !data && !shapes_changed {
            return Ok(self.skipped(seq));
        }
        let loaded = self.loaded.read().clone();
        let (checked, new_loaded) = if shapes_changed {
            // changed shapes are read from the new state, and must parse
            let new = match Shapes::from_store_graphs_with(
                &c.view,
                self.cfg.shapes_graphs(),
                self.file_shapes.as_deref(),
            ) {
                Ok(s) => Loaded::new(s),
                Err(e) => {
                    self.count(GuardStatus::Rejected);
                    let mut s = ValidationSummary::empty(
                        GuardStatus::Rejected,
                        self.cfg.mode,
                        self.cfg.threshold,
                    );
                    s.limit = self.cfg.report_limit;
                    s.shapes_error = Some(format!("{e:#}"));
                    self.history.record(c.kind, &s);
                    return Ok(s);
                }
            };
            let base = Arc::new(c.base.clone());
            let checked = self.full(
                &c.view,
                &new,
                Some((&base, &loaded)),
                c.opts,
                // a load from sources may have changed anything, the shapes included
                Some(match c.changes {
                    Changes::Unknown => Fallback::Bulk,
                    _ => Fallback::Shapes,
                }),
                seq,
            )?;
            (checked, Some(new))
        } else {
            // a write no shape can read: with the state of the head known, it carries over
            let known = self
                .exact
                .lock()
                .as_ref()
                .is_some_and(|e| e.commit == c.base.commit);
            if known && !self.reads_changes(c, &loaded) {
                return Ok(self.skipped(seq));
            }
            (self.check_data(c, &loaded)?, None)
        };
        let summary = checked.summary;
        self.count(summary.status);
        self.history.record(c.kind, &summary);
        if summary.status != GuardStatus::Rejected {
            *self.pending.lock() = Some(Pending {
                seq,
                loaded: new_loaded,
                baseline: baseline_of(&summary, seq),
                exact: Some(checked.exact),
            });
        }
        Ok(summary)
    }

    fn committed(&self, seq: u64) {
        let p = self.pending.lock().take();
        match p {
            Some(p) if p.seq == seq => {
                if let Some(l) = p.loaded {
                    *self.loaded.write() = l;
                }
                if let (Some((root, hash)), Some(_)) = (&self.persist, &p.exact)
                    && let Err(e) = StatusFile::of(&p.baseline, hash).write(root)
                {
                    tracing::warn!("cannot write {STATUS_FILE}: {e}");
                }
                *self.baseline.lock() = Some(p.baseline);
                *self.exact.lock() = p.exact;
            }
            // a commit the guard did not judge: the state is unknown
            _ => {
                if let Some(b) = self.baseline.lock().as_mut() {
                    b.commit = seq;
                    b.conforms = None;
                }
                *self.exact.lock() = None;
            }
        }
    }

    fn bypassed(&self) {
        self.count(GuardStatus::Bypassed);
        if let Some(b) = self.baseline.lock().as_mut() {
            b.conforms = None;
        }
        *self.exact.lock() = None;
    }

    fn describe(&self) -> String {
        format!(
            "write-time SHACL validation ({})",
            match self.cfg.mode {
                GuardMode::Reject => "reject",
                GuardMode::Warn => "warn",
                GuardMode::Off => "off",
            }
        )
    }
}

// ------------------------------------------------------------ configuration ------

/// `validation.json` of a database, if any, and the SHA-256 of the file.
fn read_config_hashed(root: &Path) -> Result<Option<(ValidationConfig, String)>> {
    let path = root.join(CONFIG_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let cfg: ValidationConfig =
        serde_json::from_slice(&bytes).with_context(|| format!("{}", path.display()))?;
    cfg.check()?;
    Ok(Some((cfg, sha256_hex(&bytes))))
}

/// `validation.json` of a database, if any.
pub fn read_config(root: &Path) -> Result<Option<ValidationConfig>> {
    Ok(read_config_hashed(root)?.map(|(c, _)| c))
}

/// The text and format of a configuration's shapes file (given inline, or the copy in
/// the database); `None` when its shapes are graphs alone.
fn shapes_text(
    cfg: &ValidationConfig,
    root: Option<&Path>,
) -> Result<Option<(String, crate::RdfFormat)>> {
    let format = cfg
        .shapes
        .format
        .as_deref()
        .and_then(sparkles::io::format_for_media_type)
        .unwrap_or(crate::RdfFormat::Turtle);
    let text = match (&cfg.shapes.inline, &cfg.shapes.file, root) {
        (Some(t), _, _) => t.clone(),
        (None, None, _) if cfg.shapes.graphs.is_some() => return Ok(None),
        (None, _, Some(r)) => std::fs::read_to_string(r.join(SHAPES_FILE))
            .with_context(|| format!("reading {SHAPES_FILE}"))?,
        (None, _, None) => bail!("no shapes given"),
    };
    Ok(Some((text, format)))
}

/// The shapes of a configuration over `snap`, and the graph of its shapes file when it
/// also has shapes graphs (merged with them whenever they are read again).
fn load_shapes(
    cfg: &ValidationConfig,
    root: Option<&Path>,
    snap: &Snapshot,
) -> Result<(Shapes, Option<Arc<oxrdf::Graph>>)> {
    let text = shapes_text(cfg, root)?;
    match (&cfg.shapes.graphs, text) {
        (Some(graphs), None) => Ok((Shapes::from_store_graphs(snap, graphs)?, None)),
        (Some(graphs), Some((text, format))) => {
            let file = Arc::new(Shapes::read_graph(&text, format, None)?);
            let shapes = Shapes::from_store_graphs_with(snap, graphs, Some(&file))?;
            Ok((shapes, Some(file)))
        }
        (None, Some((text, format))) => Ok((Shapes::parse(&text, format, None)?, None)),
        (None, None) => bail!("no shapes given"),
    }
}

/// Install the guard of a persistent store from its `validation.json` (after
/// [`Store::open`]). Without a configuration nothing happens. A configuration that
/// cannot be loaded is an error, and the store keeps refusing writes (fail closed). The
/// state of the head is known when `validation-status.json` records it for this
/// configuration; otherwise the first validated write runs a full validation.
pub fn install(store: &Store) -> Result<Option<Arc<ShaclGuard>>> {
    let Some(root) = store.root() else {
        return Ok(None);
    };
    let Some((cfg, hash)) = read_config_hashed(root)? else {
        return Ok(None);
    };
    if cfg.mode == GuardMode::Off {
        store.set_guard_required(false);
        return Ok(None);
    }
    let (shapes, file_shapes) = load_shapes(&cfg, Some(root), &store.snapshot())?;
    let loaded = Loaded::new(shapes);
    let n = loaded.shapes.len();
    let mut g = ShaclGuard::new(cfg, loaded, Some((root.to_path_buf(), hash.clone())));
    g.file_shapes = file_shapes;
    let g = Arc::new(g);
    let head = store.head_commit().seq;
    if let Some(b) = StatusFile::read(root, head, &hash) {
        *g.exact.lock() = Some(Exact {
            commit: head,
            counts: b.by_severity,
            per_shape: FxHashMap::default(),
            focus: Arc::new(vec![None; n]),
        });
        *g.baseline.lock() = Some(b);
    }
    store.set_guard(Some(g.clone()));
    store.set_guard_required(true);
    Ok(Some(g))
}

/// The outcome of [`set_config`].
pub enum SetOutcome {
    /// installed; the validation of the current state
    Installed(Arc<ShaclGuard>, ValidationSummary),
    /// `reject` was refused: the current state has blocking results
    NotConforming(ValidationSummary),
    /// validation is off
    Removed,
}

/// Set (or with `None` / mode `off`, remove) the write-time validation of a store.
/// Runs under the writer lock: the current state is validated with the new
/// configuration, and `reject` is refused when it does not pass (unless the baseline
/// policy is `grandfather`), so no write can commit between the check and the switch.
pub fn set_config(store: &Store, cfg: Option<ValidationConfig>) -> Result<SetOutcome> {
    let root = store.root().map(Path::to_path_buf);
    let txn = store.write_as(CommitKind::Transaction);
    let Some(mut cfg) = cfg.filter(|c| c.mode != GuardMode::Off) else {
        store.set_guard(None);
        store.set_guard_required(false);
        if let Some(r) = &root {
            sparkles::guard::config::remove_files(r, &[])?;
            StatusFile::remove(r)?;
        }
        drop(txn);
        return Ok(SetOutcome::Removed);
    };
    cfg.check()?;
    let view = Arc::new(txn.view());
    let head = txn.base().commit;
    let (shapes, file_shapes) = load_shapes(&cfg, root.as_deref(), &view)?;
    let loaded = Loaded::new(shapes);
    let probe = ShaclGuard::new(cfg.clone(), loaded.clone(), None);
    let checked = probe.full(&view, &loaded, None, &WriteOptions::default(), None, head)?;
    let summary = checked.summary;
    if cfg.mode == GuardMode::Reject
        && cfg.baseline == BaselinePolicy::Strict
        && summary.blocking > 0
    {
        return Ok(SetOutcome::NotConforming(summary));
    }
    cfg.updated = Some(sparkles::guard::config::now_rfc3339());
    // written as format 2, whatever was given
    cfg.format = 2;
    cfg.language = Some(GuardLanguage::Shacl);
    let mut persist = None;
    if let Some(r) = &root {
        // a ShEx configuration this one replaces leaves nothing behind
        sparkles::guard::config::remove_files(r, &[CONFIG_FILE, SHAPES_FILE])?;
        StatusFile::remove(r)?;
        if let Some(text) = cfg.shapes.inline.take() {
            write_atomic(&r.join(SHAPES_FILE), text.as_bytes())?;
            cfg.shapes.file = Some(SHAPES_FILE.into());
            cfg.shapes.sha256 = Some(sha256_hex(text.as_bytes()));
            cfg.shapes.format = None;
        } else if cfg.shapes.file.is_none() {
            let _ = std::fs::remove_file(r.join(SHAPES_FILE));
        }
        let bytes = serde_json::to_vec_pretty(&cfg)?;
        write_atomic(&r.join(CONFIG_FILE), &bytes)?;
        persist = Some((r.clone(), sha256_hex(&bytes)));
    }
    let mut guard = ShaclGuard::new(cfg, loaded, persist);
    guard.file_shapes = file_shapes;
    let guard = Arc::new(guard);
    let baseline = baseline_of(&summary, head);
    if let Some((r, hash)) = &guard.persist {
        StatusFile::of(&baseline, hash).write(r)?;
    }
    *guard.baseline.lock() = Some(baseline);
    *guard.exact.lock() = Some(checked.exact);
    guard.last_full.store(summary.millis, Ordering::Relaxed);
    store.set_guard(Some(guard.clone()));
    store.set_guard_required(true);
    drop(txn);
    Ok(SetOutcome::Installed(guard, summary))
}
