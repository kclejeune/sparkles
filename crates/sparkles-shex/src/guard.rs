//! Write-time ShEx validation: a [`CommitGuard`] that validates the post-state of every
//! commit against a schema and a query shape map, configured per dataset in
//! `<db>/validation.json` (format 2, `"language": "shex"`).
//!
//! The decisions are those of write-time SHACL validation (`sparkles_shacl::guard`):
//!
//! * `reject`: a commit that would leave a nonconformant association is not written
//!   (the store reports [`sparkles::Error::Rejected`]); enabling it requires the current
//!   data to conform;
//! * `warn`: the commit is written; its receipt carries the findings.
//!
//! ShEx has no severities and no threshold: every nonconformant association blocks, and
//! counts as one violation of the summary. The schema is copied into the database when
//! the configuration is set, with its imports resolved then, so a write never fetches
//! anything; the shape map is expanded again on every validated state, so new focus
//! nodes are picked up.
//!
//! A write is not validated when it cannot change the result map: when it touches no
//! graph of the data graph, or when every quad it changes has a predicate that no triple
//! constraint and no `{FOCUS p …}` selector mentions (a neighbourhood holds only the arcs
//! of the predicates its shape mentions; a CLOSED shape reads every outgoing arc, so a
//! schema with one is always validated).

use crate::ast::Schema;
use crate::ir::Ir;
use crate::resolve::Resolver;
use crate::{
    CompiledSchema, NoImports, NodeSelector, PrefixMap, ResultMap, SchemaFormat, ShapeMap,
    ValidateOptions,
};
use anyhow::{Context, Result, anyhow, bail};
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use sparkles::commit::CommitKind;
use sparkles::guard::config::{
    Baseline, CONFIG_FILE, Counters, DataGraphSel, DecisionCounts, sha256_hex, write_atomic,
};
use sparkles::guard::{
    Candidate, Changes, CommitGuard, GuardLanguage, GuardMode, GuardStatus, Severity,
    SeverityCounts, Strategy, ValidationSummary, WriteOptions,
};
use sparkles::id::Id;
use sparkles::store::{Snapshot, Store};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub use sparkles::guard::config::{SHEX_SCHEMA_SHEXC_FILE, SHEX_SCHEMA_SHEXJ_FILE};

/// The `format` of a ShEx `validation.json`.
pub const CONFIG_FORMAT: u32 = 2;

/// Write-time ShEx validation of one dataset (`validation.json`, format 2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShexValidationConfig {
    #[serde(default = "two")]
    pub format: u32,
    /// `shex`
    pub language: GuardLanguage,
    pub mode: GuardMode,
    pub schema: SchemaSource,
    /// a query shape map, expanded on every validated state
    pub shape_map: MapSource,
    #[serde(default)]
    pub data_graph: DataGraphSel,
    #[serde(default)]
    pub include_inferences: bool,
    #[serde(default = "ten")]
    pub timeout_seconds: f64,
    #[serde(default = "hundred")]
    pub report_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
}

fn two() -> u32 {
    CONFIG_FORMAT
}
fn ten() -> f64 {
    10.0
}
fn hundred() -> usize {
    100
}

/// The schema: a file copied into the database (`validation-schema.shex`, or
/// `validation-schema.json` for ShExJ and for schemas whose imports were resolved and
/// merged when the configuration was set).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchemaSource {
    /// the copied file, in the database directory (set when the configuration is set)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// the syntax: `shexc` or `shexj` (of the copied file); of `inline`, also `shexr`
    /// (Turtle). Default: sniffed (`{` → ShExJ, otherwise ShExC)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// the base IRI the schema's relative IRIs resolve against
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// the schema's prefixes, for the shape map, when the copy is ShExJ made from a
    /// schema that had them (ShExJ has none); set when the configuration is set
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prefixes: BTreeMap<String, String>,
    /// informational: where the schema came from, and the SHA-256 of its text as given
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// schema text given when setting the configuration (not stored in the file)
    #[serde(default, skip_serializing)]
    pub inline: Option<String>,
}

/// A query shape map: compact syntax, or the JSON form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MapSource {
    Compact(String),
    Json(Vec<serde_json::Value>),
}

impl ShexValidationConfig {
    /// Check the fields (not the schema or the shape map).
    pub fn check(&self) -> Result<()> {
        if self.format != CONFIG_FORMAT {
            bail!(
                "a ShEx configuration is validation.json format {CONFIG_FORMAT}, not {}",
                self.format
            );
        }
        if self.language != GuardLanguage::Shex {
            bail!("language must be \"shex\"");
        }
        if !(self.timeout_seconds.is_finite() && self.timeout_seconds > 0.0) {
            bail!("timeoutSeconds must be a positive number");
        }
        if !(1..=10_000).contains(&self.report_limit) {
            bail!("reportLimit must be between 1 and 10000");
        }
        self.data_graph.check()?;
        if self.mode != GuardMode::Off
            && usize::from(self.schema.file.is_some()) + usize::from(self.schema.inline.is_some())
                != 1
        {
            bail!("schema: give the schema inline (or the file it was copied to)");
        }
        Ok(())
    }
}

/// `GET /$/validation/{ds}` status of a ShEx guard.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShexValidationStatus {
    pub mode: GuardMode,
    /// shape declarations of the schema (imports included)
    pub shape_count: usize,
    /// associations of the last validated state's fixed map
    pub associations: Option<u64>,
    pub baseline: Option<Baseline>,
    pub last_full_millis: Option<u64>,
    pub counters: Counters,
    pub warnings: Vec<String>,
}

/// Write-time ShEx validation of one store.
pub struct ShexGuard {
    cfg: ShexValidationConfig,
    schema: Arc<CompiledSchema>,
    map: ShapeMap,
    shape_count: usize,
    /// the predicates a validation can read (see [`read_predicates`])
    reads: Option<Vec<String>>,
    pending: Mutex<Option<(u64, Baseline)>>,
    baseline: Mutex<Option<Baseline>>,
    counters: DecisionCounts,
    last_full: AtomicU64,
    /// associations of the last validation (`u64::MAX`: none yet)
    associations: AtomicU64,
    /// the last validation's warnings (semantic actions not run, …)
    warnings: Mutex<Vec<String>>,
    /// the schema copy an in-memory dataset keeps for backups (a persistent one has
    /// it in its directory)
    copy: Option<String>,
}

impl ShexGuard {
    fn new(cfg: ShexValidationConfig, loaded: Loaded, map: ShapeMap) -> ShexGuard {
        ShexGuard {
            reads: read_predicates(loaded.compiled.ir(), &map),
            cfg,
            schema: Arc::new(loaded.compiled),
            map,
            shape_count: loaded.shape_count,
            pending: Mutex::new(None),
            baseline: Mutex::new(None),
            counters: DecisionCounts::default(),
            last_full: AtomicU64::new(u64::MAX),
            associations: AtomicU64::new(u64::MAX),
            warnings: Mutex::new(Vec::new()),
            copy: None,
        }
    }

    pub fn config(&self) -> &ShexValidationConfig {
        &self.cfg
    }

    /// The schema copy of an in-memory dataset: its file name in a database directory
    /// (`schema.file` of the configuration) and its text. `None` for a persistent
    /// dataset, whose copy is in its directory.
    pub fn schema_copy(&self) -> Option<(&str, &str)> {
        Some((self.cfg.schema.file.as_deref()?, self.copy.as_deref()?))
    }

    pub fn status(&self) -> ShexValidationStatus {
        let last = self.last_full.load(Ordering::Relaxed);
        let associations = self.associations.load(Ordering::Relaxed);
        let mut warnings = self.warnings.lock().clone();
        if associations == 0 {
            warnings.push("the shape map selects no nodes".to_string());
        }
        if last != u64::MAX && last > 1000 {
            warnings.push(format!(
                "full validation took {last} ms; every write waits for it"
            ));
        }
        ShexValidationStatus {
            mode: self.cfg.mode,
            shape_count: self.shape_count,
            associations: (associations != u64::MAX).then_some(associations),
            baseline: self.baseline.lock().clone(),
            last_full_millis: (last != u64::MAX).then_some(last),
            counters: self.counters.get(),
            warnings,
        }
    }

    /// Validate `view` and summarize under this configuration.
    fn validate_state(
        &self,
        view: &Arc<Snapshot>,
        o: &WriteOptions,
    ) -> sparkles::Result<ValidationSummary> {
        let t0 = Instant::now();
        let limit = o
            .report_limit
            .unwrap_or(self.cfg.report_limit)
            .clamp(1, 10_000);
        let Some(graphs) = self
            .cfg
            .data_graph
            .graphs(view, self.cfg.include_inferences, &[])
        else {
            // none of the listed data graphs exists: nothing to validate
            self.associations.store(0, Ordering::Relaxed);
            let empty = ResultMap {
                conforms: true,
                ..Default::default()
            };
            return Ok(self.summarize(empty, limit, 0));
        };
        let mut deadline = t0 + Duration::from_secs_f64(self.cfg.timeout_seconds);
        if let Some(d) = o.deadline {
            deadline = deadline.min(d);
        }
        let vo = ValidateOptions {
            data_graph: graphs.data_graph,
            extra_graphs: graphs.extra_graphs,
            exclude_graphs: graphs.exclude_graphs,
            timeout: Some(deadline.saturating_duration_since(Instant::now())),
            cancel: o.cancel.clone(),
            only_nonconformant: true,
            // a guard never runs SERVICE
            selector_query: Some(sparkles::sparql::QueryOptions {
                forbid_service: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let rm = crate::validate(view, &self.schema, &self.map, &vo).map_err(engine_error)?;
        let ms = t0.elapsed().as_millis() as u64;
        self.last_full.store(ms, Ordering::Relaxed);
        self.associations
            .store((rm.conformant + rm.nonconformant) as u64, Ordering::Relaxed);
        *self.warnings.lock() = rm.warnings.clone();
        Ok(self.summarize(rm, limit, ms))
    }

    /// Count and bound the results, and decide: every nonconformant association blocks.
    fn summarize(&self, mut rm: ResultMap, limit: usize, millis: u64) -> ValidationSummary {
        let blocking = rm.nonconformant as u64;
        let status = match (blocking > 0, self.cfg.mode) {
            (true, GuardMode::Reject) => GuardStatus::Rejected,
            (true, _) => GuardStatus::Warned,
            (false, _) => GuardStatus::Passed,
        };
        let truncated = rm.results.len() > limit;
        rm.results.truncate(limit);
        let results = match rm.to_json()["results"].take() {
            serde_json::Value::Array(a) => a,
            _ => Vec::new(),
        };
        ValidationSummary {
            language: GuardLanguage::Shex,
            status,
            mode: self.cfg.mode,
            strategy: Strategy::Full,
            threshold: Severity::Violation,
            conforms: rm.conforms,
            blocking,
            total: (rm.conformant + rm.nonconformant) as u64,
            by_severity: SeverityCounts {
                violation: blocking,
                ..Default::default()
            },
            limit,
            truncated,
            millis,
            results,
            shapes_error: None,
            report_turtle: None,
        }
    }

    /// Whether a change can change the result map: a changed quad in a graph of the
    /// data graph, with a predicate a validation reads (any, when it may read any).
    fn relevant(&self, view: &Snapshot, changes: &Changes<'_>) -> bool {
        let (log, bulk) = match changes {
            Changes::Log(log) => (*log, &[][..]),
            Changes::Rebuilt { log, bulk } => (*log, *bulk),
            Changes::Unknown => return true,
        };
        let preds: Option<FxHashSet<Id>> = self
            .reads
            .as_ref()
            .map(|ps| ps.iter().filter_map(|p| view.lookup_iri(p)).collect());
        let mut graphs: FxHashMap<u64, bool> = FxHashMap::default();
        for q in log.iter().map(|(_, q)| q).chain(bulk) {
            if preds.as_ref().is_some_and(|ps| !ps.contains(&q[1])) {
                continue;
            }
            let g = q[3].0;
            if *graphs.entry(g).or_insert_with(|| {
                self.cfg
                    .data_graph
                    .touches(view, g, self.cfg.include_inferences, &[])
            }) {
                return true;
            }
        }
        false
    }
}

/// The predicates a validation reads: those of the schema's triple constraints and of
/// the map's `{FOCUS p …}` selectors; `None` when it may read any (a CLOSED shape reads
/// every outgoing arc of its nodes, a SPARQL selector anything).
fn read_predicates(ir: &Ir, map: &ShapeMap) -> Option<Vec<String>> {
    if ir.shapes.iter().any(|s| s.closed) {
        return None;
    }
    let mut ps: Vec<String> = ir
        .shapes
        .iter()
        .flat_map(|s| s.tcs.iter().map(|t| t.pred.clone()))
        .collect();
    for a in &map.0 {
        match &a.node {
            NodeSelector::Term(_) => {}
            NodeSelector::Focus { predicate, .. } => ps.push(predicate.as_str().to_string()),
            NodeSelector::Sparql(_) => return None,
        }
    }
    ps.sort_unstable();
    ps.dedup();
    Some(ps)
}

/// An error of the validation engine as a store error: timeouts, cancellation and
/// budgets keep their kind.
fn engine_error(e: anyhow::Error) -> sparkles::Error {
    match e.downcast::<sparkles::Error>() {
        Ok(e) => e,
        Err(e) => sparkles::Error::Invalid(format!("ShEx validation failed: {e:#}")),
    }
}

impl CommitGuard for ShexGuard {
    fn check(&self, c: &Candidate<'_>) -> sparkles::Result<ValidationSummary> {
        let seq = c.base.commit + 1;
        if !self.relevant(&c.view, &c.changes) {
            self.counters.count(GuardStatus::Skipped);
            let baseline = self
                .baseline
                .lock()
                .clone()
                .map(|b| Baseline { commit: seq, ..b });
            if let Some(b) = baseline {
                *self.pending.lock() = Some((seq, b));
            }
            let mut s =
                ValidationSummary::empty(GuardStatus::Skipped, self.cfg.mode, Severity::Violation);
            s.language = GuardLanguage::Shex;
            s.limit = self.cfg.report_limit;
            return Ok(s);
        }
        let summary = self.validate_state(&c.view, c.opts)?;
        self.counters.count(summary.status);
        if summary.status != GuardStatus::Rejected {
            *self.pending.lock() = Some((seq, baseline_of(&summary, seq)));
        }
        Ok(summary)
    }

    fn committed(&self, seq: u64) {
        let p = self.pending.lock().take();
        match p {
            Some((s, b)) if s == seq => *self.baseline.lock() = Some(b),
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
        self.counters.count(GuardStatus::Bypassed);
        if let Some(b) = self.baseline.lock().as_mut() {
            b.conforms = None;
        }
    }

    fn describe(&self) -> String {
        format!(
            "write-time ShEx validation ({})",
            match self.cfg.mode {
                GuardMode::Reject => "reject",
                GuardMode::Warn => "warn",
                GuardMode::Off => "off",
            }
        )
    }

    fn language(&self) -> GuardLanguage {
        GuardLanguage::Shex
    }
}

fn baseline_of(s: &ValidationSummary, commit: u64) -> Baseline {
    Baseline {
        commit,
        conforms: Some(s.blocking == 0),
        blocking: s.blocking,
        total: s.total,
        millis: s.millis,
    }
}

// ------------------------------------------------------------ the schema ---------

/// A schema ready for the guard, and the copy the database keeps of it.
struct Loaded {
    compiled: CompiledSchema,
    /// shape declarations, imports included
    shape_count: usize,
    /// the copy: its file name, `format` and text
    file: &'static str,
    format: &'static str,
    text: String,
    /// the prefixes the copy cannot hold (a ShExJ copy of a schema that had some)
    prefixes: PrefixMap,
    /// the base of the closed schema (`BASE`, or the one given)
    base: Option<String>,
}

/// Parse a schema text, resolve its imports and EXTERNAL shapes through `resolver`,
/// check and compile it, and choose its copy: the text itself when nothing was
/// resolved into it (ShExC or ShExJ), otherwise the closed schema as ShExJ. `prefixes`
/// stand in for a text without its own (a ShExJ copy).
fn load(
    text: String,
    format: Option<&str>,
    base: Option<&str>,
    prefixes: &BTreeMap<String, String>,
    resolver: &dyn Resolver,
) -> Result<Loaded> {
    let format = match format {
        Some(f) => SchemaFormat::from_name(f)
            .with_context(|| format!("schema.format must be shexc, shexj or shexr, not {f:?}"))?,
        None if text.trim_start().starts_with('{') => SchemaFormat::ShExJ,
        None => SchemaFormat::ShExC,
    };
    let mut parsed =
        crate::parse_schema(&text, base, Some(format)).map_err(|e| anyhow!("schema: {e}"))?;
    if parsed.prefixes.is_empty() {
        parsed.prefixes = prefixes
            .iter()
            .map(|(p, ns)| (p.clone(), ns.clone()))
            .collect();
    }
    let closed = crate::resolve::close(&parsed, resolver).map_err(|e| anyhow!("schema: {e}"))?;
    if let Some(d) = closed
        .shapes
        .iter()
        .find(|d| matches!(d.expr, crate::ShapeExpr::External))
    {
        bail!(
            "schema: the EXTERNAL shape {} has no definition (write-time validation takes none)",
            d.label
        );
    }
    let checked = crate::check::check(&closed).map_err(|e| anyhow!("schema: {e}"))?;
    let compiled =
        crate::compile::compile(&closed, &checked).map_err(|e| anyhow!("schema: {e}"))?;
    let verbatim = parsed.imports.is_empty() && closed == parsed;
    let (file, format, text) = match format {
        SchemaFormat::ShExC if verbatim => (SHEX_SCHEMA_SHEXC_FILE, "shexc", text),
        SchemaFormat::ShExJ if verbatim => (SHEX_SCHEMA_SHEXJ_FILE, "shexj", text),
        _ => {
            let merged = Schema {
                imports: Vec::new(),
                ..closed.clone()
            };
            let text = serde_json::to_string_pretty(&merged.to_shexj())?;
            (SHEX_SCHEMA_SHEXJ_FILE, "shexj", text)
        }
    };
    Ok(Loaded {
        compiled,
        shape_count: closed.shapes.len(),
        file,
        format,
        text,
        prefixes: if file == SHEX_SCHEMA_SHEXJ_FILE {
            closed.prefixes.clone()
        } else {
            Vec::new()
        },
        base: closed.base.clone(),
    })
}

/// Load the copy of the schema a configuration names (nothing is resolved: the copy
/// holds its imports).
fn load_copy(cfg: &ShexValidationConfig, root: &Path) -> Result<Loaded> {
    let file = cfg.schema.file.as_deref().unwrap_or_default();
    let format = match file {
        SHEX_SCHEMA_SHEXC_FILE => "shexc",
        SHEX_SCHEMA_SHEXJ_FILE => "shexj",
        _ => bail!(
            "schema.file must be {SHEX_SCHEMA_SHEXC_FILE} or {SHEX_SCHEMA_SHEXJ_FILE} (the copy in the database), not {file:?}"
        ),
    };
    let text =
        std::fs::read_to_string(root.join(file)).with_context(|| format!("reading {file}"))?;
    load(
        text,
        Some(format),
        cfg.schema.base.as_deref(),
        &cfg.schema.prefixes,
        &NoImports,
    )
}

/// Parse a configuration's shape map against the schema: labels it does not define,
/// START without a start shape, and SPARQL selectors (which every validated write would
/// run) are errors.
fn parse_map(src: &MapSource, schema: &CompiledSchema) -> Result<ShapeMap> {
    let map = match src {
        MapSource::Compact(s) => ShapeMap::parse(s, schema.prefixes(), schema.base()),
        MapSource::Json(v) => ShapeMap::from_json(&serde_json::to_string(v)?),
    }
    .map_err(|e| anyhow!("shapeMap: {e}"))?;
    if map.0.is_empty() {
        bail!("shapeMap: the shape map has no associations");
    }
    for a in &map.0 {
        if matches!(a.node, NodeSelector::Sparql(_)) {
            bail!(
                "shapeMap: SPARQL node selectors are not allowed in write-time validation; use {{FOCUS p o}} selectors or nodes"
            );
        }
        crate::shapemap::label_kind(schema, &a.shape).map_err(|e| anyhow!("shapeMap: {e}"))?;
    }
    Ok(map)
}

// ------------------------------------------------------------ configuration ------

/// The ShEx configuration of a database, if `validation.json` is one (`None` without a
/// file; an error for a SHACL configuration or one that does not parse).
pub fn read_config(root: &Path) -> Result<Option<ShexValidationConfig>> {
    let path = root.join(CONFIG_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let cfg: ShexValidationConfig =
        serde_json::from_slice(&bytes).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    cfg.check()?;
    Ok(Some(cfg))
}

/// Install the guard of a persistent store from its ShEx `validation.json` (after
/// [`Store::open`]): the schema is read from its copy in the database, without
/// resolving anything. Without a configuration nothing happens. A configuration that
/// cannot be loaded is an error, and the store keeps refusing writes (fail closed).
pub fn install(store: &Store) -> Result<Option<Arc<ShexGuard>>> {
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
    let loaded = load_copy(&cfg, root)?;
    let map = parse_map(&cfg.shape_map, &loaded.compiled)?;
    let g = Arc::new(ShexGuard::new(cfg, loaded, map));
    store.set_guard(Some(g.clone()));
    store.set_guard_required(true);
    Ok(Some(g))
}

/// The outcome of [`set_config`].
pub enum SetOutcome {
    /// installed; the validation of the current state
    Installed(Arc<ShexGuard>, ValidationSummary),
    /// `reject` was refused: the current state has nonconformant associations
    NotConforming(ValidationSummary),
    /// validation is off
    Removed,
}

/// Set (or with `None` / mode `off`, remove) the write-time ShEx validation of a store.
/// The schema is parsed from `cfg.schema.inline` (or, without it, read from the copy an
/// earlier configuration left in the database), its imports resolved through
/// `resolver` and checked, and copied into the database with the configuration. Runs
/// under the writer lock: the current state is validated with the new configuration,
/// and `reject` is refused when an association does not conform, so no write can commit
/// between the check and the switch.
pub fn set_config(
    store: &Store,
    cfg: Option<ShexValidationConfig>,
    resolver: &dyn Resolver,
) -> Result<SetOutcome> {
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
    let mut loaded = match cfg.schema.inline.take() {
        Some(text) => {
            cfg.schema.sha256 = Some(sha256_hex(text.as_bytes()));
            load(
                text,
                cfg.schema.format.as_deref(),
                cfg.schema.base.as_deref(),
                &BTreeMap::new(),
                resolver,
            )?
        }
        None => {
            let r = root
                .as_deref()
                .context("schema: give the schema inline (an in-memory dataset keeps no copy)")?;
            load_copy(&cfg, r)?
        }
    };
    let map = parse_map(&cfg.shape_map, &loaded.compiled)?;
    // the configuration as written
    cfg.format = CONFIG_FORMAT;
    cfg.updated = Some(sparkles::guard::config::now_rfc3339());
    cfg.schema.file = Some(loaded.file.to_string());
    cfg.schema.format = Some(loaded.format.to_string());
    cfg.schema.prefixes = loaded.prefixes.iter().cloned().collect();
    if loaded.file == SHEX_SCHEMA_SHEXJ_FILE && cfg.schema.base.is_none() {
        cfg.schema.base = loaded.base.clone();
    }
    let file = loaded.file;
    let text = std::mem::take(&mut loaded.text);
    let mut guard = ShexGuard::new(cfg, loaded, map);
    if root.is_none() {
        guard.copy = Some(text.clone());
    }
    let guard = Arc::new(guard);

    let view = Arc::new(txn.view());
    let summary = guard.validate_state(&view, &WriteOptions::default())?;
    if guard.cfg.mode == GuardMode::Reject && summary.blocking > 0 {
        return Ok(SetOutcome::NotConforming(summary));
    }
    if let Some(r) = &root {
        // a configuration of either language this one replaces leaves nothing behind
        sparkles::guard::config::remove_files(r, &[CONFIG_FILE, file])?;
        write_atomic(&r.join(file), text.as_bytes())?;
        write_atomic(
            &r.join(CONFIG_FILE),
            &serde_json::to_vec_pretty(&guard.cfg)?,
        )?;
    }
    *guard.baseline.lock() = Some(baseline_of(&summary, view.commit));
    store.set_guard(Some(guard.clone()));
    store.set_guard_required(true);
    drop(txn);
    Ok(SetOutcome::Installed(guard, summary))
}

#[cfg(test)]
mod tests;
