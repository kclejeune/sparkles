//! Write-time validation: a [`CommitGuard`] that validates the post-state of every
//! commit against a shapes graph, configured per dataset in `<db>/validation.json`.
//!
//! * `reject`: a commit that would leave results at or above the threshold is not
//!   written (the store reports [`sparkles::Error::Rejected`]).
//! * `warn`: the commit is written; its receipt carries the findings.
//!
//! Enabling `reject` requires the current data to pass, and every later commit is
//! validated, so a committed head never has blocking results: judging the post-state
//! alone is then the same as judging what the write added.

use crate::{Shapes, ValidateOptions, ValidationReport, validate};
use anyhow::{Context, Result, bail};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sparkles::commit::CommitKind;
pub use sparkles::guard::config::{Baseline, CONFIG_FILE, Counters, DataGraphSel};
use sparkles::guard::config::{
    DecisionCounts, INFERRED_GRAPH as INFERRED, sha256_hex, write_atomic,
};
use sparkles::guard::{
    Candidate, CommitGuard, GuardLanguage, GuardMode, GuardStatus, Severity, SeverityCounts,
    Strategy, ValidationSummary, WriteOptions,
};
use sparkles::id::Id;
use sparkles::sparql::ctx::{DEFAULT_GRAPH_IRI, UNION_GRAPH_IRI};
use sparkles::store::{Snapshot, Store};
use std::path::Path;
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
/// validated, so changes to them are validated too), or a file copied into the database.
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
        let sources =
            usize::from(s.graphs.is_some()) + usize::from(s.file.is_some() || s.inline.is_some());
        if self.mode != GuardMode::Off && sources != 1 {
            bail!("shapes: give either graphs or a file (inline shapes)");
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

struct Pending {
    seq: u64,
    shapes: Option<Arc<Shapes>>,
    baseline: Baseline,
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
}

/// Write-time SHACL validation of one store.
pub struct ShaclGuard {
    cfg: ValidationConfig,
    shapes: RwLock<Arc<Shapes>>,
    pending: Mutex<Option<Pending>>,
    baseline: Mutex<Option<Baseline>>,
    counters: DecisionCounts,
    last_full: AtomicU64,
}

impl ShaclGuard {
    fn new(cfg: ValidationConfig, shapes: Arc<Shapes>) -> ShaclGuard {
        ShaclGuard {
            cfg,
            shapes: RwLock::new(shapes),
            pending: Mutex::new(None),
            baseline: Mutex::new(None),
            counters: DecisionCounts::default(),
            last_full: AtomicU64::new(u64::MAX),
        }
    }

    pub fn config(&self) -> &ValidationConfig {
        &self.cfg
    }

    fn count(&self, s: GuardStatus) {
        self.counters.count(s);
    }

    pub fn status(&self) -> ValidationStatus {
        let shape_count = self.shapes.read().len();
        let last = self.last_full.load(Ordering::Relaxed);
        let mut warnings = Vec::new();
        if shape_count == 0 {
            warnings.push("the shapes graph has no shapes".to_string());
        }
        if last != u64::MAX && last > 1000 {
            warnings.push(format!(
                "full validation took {last} ms; every write waits for it"
            ));
        }
        ValidationStatus {
            mode: self.cfg.mode,
            shape_count,
            baseline: self.baseline.lock().clone(),
            last_full_millis: (last != u64::MAX).then_some(last),
            counters: self.counters.get(),
            warnings,
        }
    }

    /// Validate `view` with `shapes` and summarize under this configuration.
    fn validate_state(
        &self,
        shapes: &Shapes,
        view: &Arc<Snapshot>,
        o: &WriteOptions,
    ) -> sparkles::Result<ValidationSummary> {
        let t0 = Instant::now();
        let limit = o
            .report_limit
            .unwrap_or(self.cfg.report_limit)
            .clamp(1, 10_000);
        let Some(mut vo) = self.cfg.validate_options(view) else {
            return Ok(summarize(
                &self.cfg,
                &ValidationReport {
                    conforms: true,
                    results: Vec::new(),
                },
                limit,
                0,
            ));
        };
        let mut deadline = t0 + Duration::from_secs_f64(self.cfg.timeout_seconds);
        if let Some(d) = o.deadline {
            deadline = deadline.min(d);
        }
        vo.timeout = Some(deadline.saturating_duration_since(Instant::now()));
        vo.cancel = o.cancel.clone();
        let report = validate(view, shapes, &vo).map_err(engine_error)?;
        let ms = t0.elapsed().as_millis() as u64;
        self.last_full.store(ms, Ordering::Relaxed);
        Ok(summarize(&self.cfg, &report, limit, ms))
    }
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

/// Count, rank and bound the results, and decide under `cfg`.
fn summarize(
    cfg: &ValidationConfig,
    report: &ValidationReport,
    limit: usize,
    millis: u64,
) -> ValidationSummary {
    let mut counts = SeverityCounts::default();
    let mut ranked: Vec<(Severity, &crate::ValidationResult)> = report
        .results
        .iter()
        .map(|r| {
            let s = severity_of(r.severity.as_str());
            match s {
                Severity::Violation => counts.violation += 1,
                Severity::Warning => counts.warning += 1,
                Severity::Info => counts.info += 1,
            }
            (s, r)
        })
        .collect();
    ranked.sort_by(|(a, x), (b, y)| {
        b.cmp(a)
            .then_with(|| x.source_shape.to_string().cmp(&y.source_shape.to_string()))
            .then_with(|| x.focus_node.to_string().cmp(&y.focus_node.to_string()))
    });
    let blocking = ranked.iter().filter(|(s, _)| *s >= cfg.threshold).count() as u64;
    let kept: Vec<crate::ValidationResult> = ranked
        .iter()
        .take(limit)
        .map(|(_, r)| (*r).clone())
        .collect();
    let status = match (blocking > 0, cfg.mode) {
        (true, GuardMode::Reject) => GuardStatus::Rejected,
        (true, _) => GuardStatus::Warned,
        (false, _) => GuardStatus::Passed,
    };
    let report_turtle = (status == GuardStatus::Rejected).then(|| {
        ValidationReport {
            conforms: report.conforms,
            results: kept.clone(),
        }
        .to_turtle()
    });
    ValidationSummary {
        language: GuardLanguage::Shacl,
        status,
        mode: cfg.mode,
        strategy: Strategy::Full,
        threshold: cfg.threshold,
        conforms: report.conforms,
        blocking,
        total: report.results.len() as u64,
        by_severity: counts,
        limit,
        truncated: report.results.len() > limit,
        millis,
        results: kept.iter().map(crate::report::result_json).collect(),
        shapes_error: None,
        report_turtle,
    }
}

impl CommitGuard for ShaclGuard {
    fn check(&self, c: &Candidate<'_>) -> sparkles::Result<ValidationSummary> {
        let seq = c.base.commit + 1;
        let skip = |status| {
            let mut s = ValidationSummary::empty(status, self.cfg.mode, self.cfg.threshold);
            s.limit = self.cfg.report_limit;
            s
        };
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
            self.count(GuardStatus::Skipped);
            let baseline = self
                .baseline
                .lock()
                .clone()
                .map(|b| Baseline { commit: seq, ..b });
            if let Some(baseline) = baseline {
                *self.pending.lock() = Some(Pending {
                    seq,
                    shapes: None,
                    baseline,
                });
            }
            return Ok(skip(GuardStatus::Skipped));
        }
        // changed shapes are read from the new state, and must parse
        let (shapes, new_shapes) = if shapes_changed {
            match Shapes::from_store_graphs(&c.view, self.cfg.shapes_graphs()) {
                Ok(s) => {
                    let s = Arc::new(s);
                    (s.clone(), Some(s))
                }
                Err(e) => {
                    self.count(GuardStatus::Rejected);
                    let mut s = skip(GuardStatus::Rejected);
                    s.shapes_error = Some(format!("{e:#}"));
                    return Ok(s);
                }
            }
        } else {
            (self.shapes.read().clone(), None)
        };
        let summary = self.validate_state(&shapes, &c.view, c.opts)?;
        self.count(summary.status);
        if summary.status != GuardStatus::Rejected {
            *self.pending.lock() = Some(Pending {
                seq,
                shapes: new_shapes,
                baseline: Baseline {
                    commit: seq,
                    conforms: Some(summary.blocking == 0),
                    blocking: summary.blocking,
                    total: summary.total,
                    millis: summary.millis,
                },
            });
        }
        Ok(summary)
    }

    fn committed(&self, seq: u64) {
        let p = self.pending.lock().take();
        match p {
            Some(p) if p.seq == seq => {
                if let Some(s) = p.shapes {
                    *self.shapes.write() = s;
                }
                *self.baseline.lock() = Some(p.baseline);
            }
            // a commit the guard did not judge: the state is unknown
            _ => {
                if let Some(b) = self.baseline.lock().as_mut() {
                    b.commit = seq;
                    b.conforms = None;
                }
            }
        }
    }

    fn bypassed(&self) {
        self.count(GuardStatus::Bypassed);
        if let Some(b) = self.baseline.lock().as_mut() {
            b.conforms = None;
        }
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

/// `validation.json` of a database, if any.
pub fn read_config(root: &Path) -> Result<Option<ValidationConfig>> {
    let path = root.join(CONFIG_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let cfg: ValidationConfig =
        serde_json::from_slice(&bytes).with_context(|| format!("{}", path.display()))?;
    cfg.check()?;
    Ok(Some(cfg))
}

fn load_shapes(cfg: &ValidationConfig, root: Option<&Path>, snap: &Snapshot) -> Result<Shapes> {
    if let Some(graphs) = &cfg.shapes.graphs {
        return Shapes::from_store_graphs(snap, graphs);
    }
    let format = cfg
        .shapes
        .format
        .as_deref()
        .and_then(sparkles::io::format_for_media_type)
        .unwrap_or(crate::RdfFormat::Turtle);
    let text = match (&cfg.shapes.inline, root) {
        (Some(t), _) => t.clone(),
        (None, Some(r)) => std::fs::read_to_string(r.join(SHAPES_FILE))
            .with_context(|| format!("reading {SHAPES_FILE}"))?,
        (None, None) => bail!("no shapes given"),
    };
    Shapes::parse(&text, format, None)
}

/// Install the guard of a persistent store from its `validation.json` (after
/// [`Store::open`]). Without a configuration nothing happens. A configuration that
/// cannot be loaded is an error, and the store keeps refusing writes (fail closed).
pub fn install(store: &Store) -> Result<Option<Arc<ShaclGuard>>> {
    let Some(root) = store.root() else {
        return Ok(None);
    };
    let Some(cfg) = read_config(root)? else {
        return Ok(None);
    };
    if cfg.mode == GuardMode::Off {
        store.set_guard_required(false);
        return Ok(None);
    }
    let shapes = load_shapes(&cfg, Some(root), &store.snapshot())?;
    let g = Arc::new(ShaclGuard::new(cfg, Arc::new(shapes)));
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
/// configuration, and `reject` is refused when it does not pass, so no write can commit
/// between the check and the switch.
pub fn set_config(store: &Store, cfg: Option<ValidationConfig>) -> Result<SetOutcome> {
    let root = store.root().map(Path::to_path_buf);
    let txn = store.write_as(CommitKind::Transaction);
    let Some(mut cfg) = cfg.filter(|c| c.mode != GuardMode::Off) else {
        store.set_guard(None);
        store.set_guard_required(false);
        if let Some(r) = &root {
            sparkles::guard::config::remove_files(r, &[])?;
        }
        drop(txn);
        return Ok(SetOutcome::Removed);
    };
    cfg.check()?;
    let view = Arc::new(txn.view());
    let shapes = Arc::new(load_shapes(&cfg, root.as_deref(), &view)?);
    let probe = ShaclGuard::new(cfg.clone(), shapes.clone());
    let summary = probe.validate_state(&shapes, &view, &WriteOptions::default())?;
    if cfg.mode == GuardMode::Reject && summary.blocking > 0 {
        return Ok(SetOutcome::NotConforming(summary));
    }
    cfg.updated = Some(sparkles::guard::config::now_rfc3339());
    // written as format 2, whatever was given
    cfg.format = 2;
    cfg.language = Some(GuardLanguage::Shacl);
    if let Some(r) = &root {
        // a ShEx configuration this one replaces leaves nothing behind
        sparkles::guard::config::remove_files(r, &[CONFIG_FILE, SHAPES_FILE])?;
        if let Some(text) = cfg.shapes.inline.take() {
            write_atomic(&r.join(SHAPES_FILE), text.as_bytes())?;
            cfg.shapes.file = Some(SHAPES_FILE.into());
            cfg.shapes.sha256 = Some(sha256_hex(text.as_bytes()));
            cfg.shapes.format = None;
        } else if cfg.shapes.graphs.is_some() {
            let _ = std::fs::remove_file(r.join(SHAPES_FILE));
        }
        write_atomic(&r.join(CONFIG_FILE), &serde_json::to_vec_pretty(&cfg)?)?;
    }
    let guard = Arc::new(ShaclGuard::new(cfg, shapes));
    *guard.baseline.lock() = Some(Baseline {
        commit: view.commit,
        conforms: Some(summary.blocking == 0),
        blocking: summary.blocking,
        total: summary.total,
        millis: summary.millis,
    });
    guard.last_full.store(summary.millis, Ordering::Relaxed);
    store.set_guard(Some(guard.clone()));
    store.set_guard_required(true);
    drop(txn);
    Ok(SetOutcome::Installed(guard, summary))
}
