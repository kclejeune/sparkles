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
//! With `baseline: "grandfather"`, `reject` may be enabled on data that does not
//! conform, and a write is judged by the nonconformant associations it introduces: those
//! of the state after it that the state before it did not have (matched by node and
//! shape, as a multiset).
//!
//! ShEx has no severities and no threshold: every nonconformant association blocks, and
//! counts as one violation of the summary. The schema is copied into the database when
//! the configuration is set, with its imports resolved then, so a write never fetches
//! anything; the shape map is expanded again on every validated state, so new focus
//! nodes are picked up.
//!
//! The schema may instead live in named graphs of the dataset, in ShExR
//! (`schema.graphs`). It is then read from the state being validated: a write that
//! changes one of those graphs is validated in full against the schema it leaves, and
//! rejected when that schema does not parse, check or define the shape map's labels.
//! The schema graphs are never part of the data graph.
//!
//! A write is not validated when it cannot change the result map: when it touches no
//! graph of the data graph, or when every quad it changes has a predicate that no triple
//! constraint and no `{FOCUS p …}` selector mentions (a neighbourhood holds only the arcs
//! of the predicates its shape mentions; a CLOSED shape reads every outgoing arc, so a
//! schema with one is always validated).
//!
//! The guard knows the exact counts of the head's result map (after a full validation,
//! kept up to date by every validated write and persisted in `validation-status.json`).
//! A write then validates only the associations whose result it can change (see
//! [`incremental`]), in the states before and after it, and moves the counts by the
//! difference. It validates in full when the state of the head is unknown, in strict
//! `reject` mode when the head does not conform, for bulk writes, for a map with a SPARQL
//! selector, and when too many nodes are affected.

use crate::ast::Schema;
use crate::engine::PairValues;
use crate::ir::{Ir, PairKind};
use crate::resolve::Resolver;
use crate::typing::Verdict;
use crate::{
    CompiledSchema, NoImports, NodeSelector, PrefixMap, ResultMap, SchemaFormat, ShapeMap,
    ValidateOptions,
};
use anyhow::{Context, Result, anyhow, bail};
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use sparkles::commit::CommitKind;
pub use sparkles::guard::config::BaselinePolicy;
use sparkles::guard::config::{
    Baseline, CONFIG_FILE, CheckHistory, CheckRecord, Counters, DataGraphSel, DecisionCounts,
    STATUS_FILE, StatusFile, sha256_hex, write_atomic,
};
use sparkles::guard::{
    Candidate, Changes, CommitGuard, GuardLanguage, GuardMode, GuardStatus, Severity,
    SeverityCounts, Strategy, ValidationSummary, WriteOptions,
};
use sparkles::id::Id;
use sparkles::store::{Snapshot, Store};
use sparkles::validation::DataGraph;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
    /// `strict` (the default) or `grandfather`: a write is judged by the nonconformant
    /// associations it introduces
    #[serde(default, skip_serializing_if = "BaselinePolicy::is_strict")]
    pub baseline: BaselinePolicy,
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
    /// named graphs of the dataset that hold the schema in ShExR, read from the state
    /// being validated (instead of a copied file)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphs: Option<Vec<String>>,
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
        let s = &self.schema;
        if self.mode != GuardMode::Off
            && usize::from(s.file.is_some())
                + usize::from(s.inline.is_some())
                + usize::from(s.graphs.is_some())
                != 1
        {
            bail!(
                "schema: give the schema inline, the file it was copied to, or the graphs that hold it in ShExR"
            );
        }
        if let Some(gs) = &s.graphs {
            if gs.is_empty() {
                bail!("schema.graphs: name at least one graph");
            }
            for g in gs {
                oxrdf::NamedNode::new(g.as_str()).with_context(|| format!("schema graph <{g}>"))?;
            }
            if let DataGraphSel::Graphs(ds) = &self.data_graph
                && let Some(g) = ds.iter().find(|g| gs.contains(g))
            {
                bail!("<{g}> is both a schema graph and a data graph");
            }
        }
        Ok(())
    }

    /// The named graphs that hold the schema (none for a copied schema).
    pub fn schema_graphs(&self) -> &[String] {
        self.schema.graphs.as_deref().unwrap_or(&[])
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
    /// the last validated write
    pub last_check: Option<CheckRecord>,
    /// the last rejected writes, newest first
    pub recent_rejections: Vec<CheckRecord>,
}

mod incremental;

/// Affected nodes past which a write is validated in full (the walk that finds them
/// stops there).
const MAX_FOCUS: usize = 50_000;

/// The outcome of propagating the typings a write changes.
enum Propagation {
    Done(Box<Checked>),
    /// the region grew past [`MAX_FOCUS`] nodes
    Budget,
    /// the typing of the head is not known (or misses a pair it should have)
    Unknown,
}

/// A validated write: its summary, the exact counts it leaves, and what it does to the
/// typing of the head.
type Checked = (ValidationSummary, Exact, TypingUpdate);

/// The exact counts of the result map of a commit.
#[derive(Clone, Copy, Debug)]
struct Exact {
    commit: u64,
    nonconformant: u64,
    total: u64,
}

struct Pending {
    seq: u64,
    baseline: Baseline,
    exact: Option<Exact>,
    typing: TypingUpdate,
    /// the schema a write to the schema graphs leaves
    model: Option<Arc<Model>>,
}

/// The schema a guard validates against, with what it reads: replaced when a write
/// changes the schema graphs.
pub(crate) struct Model {
    pub(crate) schema: Arc<CompiledSchema>,
    pub(crate) map: ShapeMap,
    shape_count: usize,
    /// the predicates a validation can read (see [`read_predicates`])
    pub(crate) reads: Option<Vec<String>>,
    /// what a write can affect
    plan: incremental::Plan,
}

impl Model {
    fn new(loaded: Loaded, map: ShapeMap) -> Model {
        Model {
            reads: read_predicates(loaded.compiled.ir(), &map),
            plan: incremental::Plan::new(loaded.compiled.ir(), &map),
            schema: Arc::new(loaded.compiled),
            map,
            shape_count: loaded.shape_count,
        }
    }

    /// Whether the guard keeps the typing of the head: when references follow arcs.
    fn keeps_typing(&self) -> bool {
        self.plan.refers()
    }
}

/// The typing of the head: every pair the validations of its shape map discovered,
/// with its value. It is exact for the head, and closed: the pairs any of its pairs
/// reads are in it. Kept for schemas whose references follow arcs, so a write can
/// propagate only the typings it changes ([`ShexGuard::propagated`]).
struct HeadTyping {
    commit: u64,
    /// the generation whose ids it uses (a compaction renumbers terms)
    generation: u64,
    values: FxHashMap<(Id, PairKind), Verdict>,
    /// the pair kinds of each node
    by_node: FxHashMap<Id, Vec<PairKind>>,
}

impl HeadTyping {
    fn new(commit: u64, generation: u64, values: PairValues) -> HeadTyping {
        let mut t = HeadTyping {
            commit,
            generation,
            values: FxHashMap::default(),
            by_node: FxHashMap::default(),
        };
        t.apply(values);
        t
    }

    fn apply(&mut self, values: PairValues) {
        for ((n, k), v) in values {
            if self.values.insert((n, k), v).is_none() {
                self.by_node.entry(n).or_default().push(k);
            }
        }
    }
}

/// What a commit does to the typing of the head.
enum TypingUpdate {
    /// nothing changes (a write no validation reads)
    Keep,
    /// the typing of a full validation, over the generation with this uid
    Replace(u64, PairValues),
    /// the pairs a write changed or added
    Delta(PairValues),
    /// unknown from now on
    Drop,
}

/// Write-time ShEx validation of one store.
pub struct ShexGuard {
    cfg: ShexValidationConfig,
    model: parking_lot::RwLock<Arc<Model>>,
    pending: Mutex<Option<Pending>>,
    baseline: Mutex<Option<Baseline>>,
    exact: Mutex<Option<Exact>>,
    typing: Mutex<Option<HeadTyping>>,
    /// the database directory and the SHA-256 of its `validation.json`, where the state
    /// of each validated commit is kept
    persist: Option<(PathBuf, String)>,
    counters: DecisionCounts,
    last_full: AtomicU64,
    /// associations of the last validation (`u64::MAX`: none yet)
    associations: AtomicU64,
    /// the last validation's warnings (semantic actions not run, …)
    warnings: Mutex<Vec<String>>,
    /// the schema copy an in-memory dataset keeps for backups (a persistent one has
    /// it in its directory)
    copy: Option<String>,
    history: CheckHistory,
}

impl ShexGuard {
    fn new(
        cfg: ShexValidationConfig,
        loaded: Loaded,
        map: ShapeMap,
        persist: Option<(PathBuf, String)>,
    ) -> ShexGuard {
        ShexGuard {
            model: parking_lot::RwLock::new(Arc::new(Model::new(loaded, map))),
            exact: Mutex::new(None),
            typing: Mutex::new(None),
            persist,
            cfg,
            pending: Mutex::new(None),
            baseline: Mutex::new(None),
            counters: DecisionCounts::default(),
            last_full: AtomicU64::new(u64::MAX),
            associations: AtomicU64::new(u64::MAX),
            warnings: Mutex::new(Vec::new()),
            copy: None,
            history: CheckHistory::default(),
        }
    }

    pub fn config(&self) -> &ShexValidationConfig {
        &self.cfg
    }

    /// The schema and shape map every write is validated against now.
    pub(crate) fn model(&self) -> Arc<Model> {
        self.model.read().clone()
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
            shape_count: self.model().shape_count,
            associations: (associations != u64::MAX).then_some(associations),
            baseline: self.baseline.lock().clone(),
            last_full_millis: (last != u64::MAX).then_some(last),
            counters: self.counters.get(),
            warnings,
            last_check: self.history.last(),
            recent_rejections: self.history.rejected(),
        }
    }

    fn grandfather(&self) -> bool {
        self.cfg.baseline == BaselinePolicy::Grandfather
    }

    fn limit(&self, o: &WriteOptions) -> usize {
        o.report_limit
            .unwrap_or(self.cfg.report_limit)
            .clamp(1, 10_000)
    }

    /// The result map of all of `snap` under `m`, and its typing when the guard keeps
    /// it.
    fn validate_map(
        &self,
        m: &Model,
        snap: &Arc<Snapshot>,
        o: &WriteOptions,
        deadline: Instant,
    ) -> sparkles::Result<(ResultMap, PairValues)> {
        match self.options(snap, o, deadline) {
            Some(vo) => {
                let (rm, mut values) =
                    crate::engine::validate_typed(snap, &m.schema, &m.map, &vo, None, &[])
                        .map_err(engine_error)?;
                if !m.keeps_typing() {
                    values = Vec::new();
                }
                Ok((rm, values))
            }
            // none of the listed data graphs exists: nothing to validate
            None => Ok((
                ResultMap {
                    conforms: true,
                    ..Default::default()
                },
                Vec::new(),
            )),
        }
    }

    /// Validate all of `view` under `m` and summarize under this configuration. In
    /// grandfather mode `before` (the state before the write, with the schema it was
    /// validated against) is validated too, and the nonconformant associations `view`
    /// adds decide; without it none counts as added.
    fn validate_state(
        &self,
        m: &Model,
        view: &Arc<Snapshot>,
        before: Option<(&Arc<Snapshot>, &Model)>,
        o: &WriteOptions,
    ) -> sparkles::Result<(ValidationSummary, PairValues)> {
        let t0 = Instant::now();
        let deadline = self.deadline(t0, o);
        let (rm, typing) = self.validate_map(m, view, o, deadline)?;
        let introduced = match before {
            Some((base, bm)) if self.grandfather() => {
                let (pre, _) = self.validate_map(bm, base, o, deadline)?;
                Some(new_nonconformant(&rm.results, &pre.results))
            }
            _ if self.grandfather() => Some(vec![false; rm.results.len()]),
            _ => None,
        };
        let ms = t0.elapsed().as_millis() as u64;
        // a dry run changes none of the status the guard reports
        if o.dry_run.is_none() {
            self.last_full.store(ms, Ordering::Relaxed);
            self.associations
                .store((rm.conformant + rm.nonconformant) as u64, Ordering::Relaxed);
            *self.warnings.lock() = rm.warnings.clone();
        }
        Ok((self.summarize(rm, introduced, self.limit(o), ms), typing))
    }

    fn deadline(&self, t0: Instant, o: &WriteOptions) -> Instant {
        let d = t0 + Duration::from_secs_f64(self.cfg.timeout_seconds);
        o.deadline.map_or(d, |x| d.min(x))
    }

    /// Validation options over `snap` (`None`: none of the listed data graphs exists).
    fn options(
        &self,
        snap: &Snapshot,
        o: &WriteOptions,
        deadline: Instant,
    ) -> Option<ValidateOptions> {
        let graphs = self.cfg.data_graph.graphs(
            snap,
            self.cfg.include_inferences,
            self.cfg.schema_graphs(),
        )?;
        Some(ValidateOptions {
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
        })
    }

    /// Validate in full under `m`, saying why it is not incremental. In grandfather mode
    /// the state before the write is judged under `before` (the schema it had).
    fn full(
        &self,
        m: &Model,
        before: &Model,
        c: &Candidate<'_>,
        reason: &str,
    ) -> sparkles::Result<Checked> {
        let base = Arc::new(c.base.clone());
        let (mut s, typing) = self.validate_state(m, &c.view, Some((&base, before)), c.opts)?;
        s.fallback = Some(reason.to_string());
        s.focus_nodes = Some(s.total);
        let exact = Exact {
            commit: c.base.commit + 1,
            nonconformant: s.blocking,
            total: s.total,
        };
        let typing = if m.keeps_typing() {
            TypingUpdate::Replace(c.view.generation.uid, typing)
        } else {
            TypingUpdate::Drop
        };
        Ok((s, exact, typing))
    }

    /// Validate a write by propagating the typings it changes (see the module
    /// documentation of `incremental`), when the guard knows the typing of the head.
    ///
    /// The region starts with the nodes whose neighbourhood the write changed. Its
    /// pairs are typed in the state after the write, with every other pair the typing
    /// of the head has as `true` read as `true`; other pairs (`false`, or not typed
    /// before) are typed too. A pair whose value changed adds its node and the nodes
    /// that read it, transitively, to the region, and the region is typed again, until
    /// no value outside it changes. The typing of every pair the region does not reach
    /// is then the head's, so the associations of the region's nodes are the only ones
    /// whose results the write can change.
    #[allow(clippy::too_many_arguments)]
    fn propagated(
        &self,
        m: &Model,
        c: &Candidate<'_>,
        changes: &[[Id; 3]],
        exact: &Exact,
        post: &Option<(ValidateOptions, DataGraph)>,
        pre: &Option<(ValidateOptions, DataGraph)>,
        t0: Instant,
    ) -> sparkles::Result<Propagation> {
        let typing = self.typing.lock();
        let Some(t) = typing
            .as_ref()
            .filter(|t| t.commit == c.base.commit && t.generation == c.base.generation.uid)
        else {
            return Ok(Propagation::Unknown);
        };
        let (Some((vo, post_data)), Some((_, pre_data))) = (post, pre) else {
            return Ok(Propagation::Unknown);
        };
        let r = m.plan.resolve(&c.view);
        let mut region: FxHashSet<Id> = FxHashSet::default();
        let mut queue: Vec<Id> = Vec::new();
        for x in incremental::Plan::direct(&r, changes) {
            if region.insert(x) {
                queue.push(x);
            }
        }
        let mut seeds: Vec<(Id, PairKind)> = Vec::new();
        let (after, values, nodes) = loop {
            for n in queue.drain(..) {
                if let Some(ks) = t.by_node.get(&n) {
                    seeds.extend(ks.iter().map(|&k| (n, k)));
                }
            }
            if region.len() > MAX_FOCUS {
                return Ok(Propagation::Budget);
            }
            let mut ids: Vec<Id> = region.iter().copied().collect();
            ids.sort_unstable();
            let nodes: Vec<(Id, oxrdf::Term)> = ids
                .into_iter()
                .filter_map(|id| c.view.term(id).map(|term| (id, term)))
                .collect();
            let map = incremental::associations(&m.map, &c.view, Some(post_data), &nodes)?;
            // outside the region: what conformed, and what fails whatever it reads
            let fixed = |n: Id, k: PairKind| -> Option<bool> {
                if region.contains(&n) {
                    return None;
                }
                match t.values.get(&(n, k))? {
                    Verdict::True => Some(true),
                    Verdict::AlwaysFalse => Some(false),
                    Verdict::False => None,
                }
            };
            let (after, values) =
                crate::engine::validate_typed(&c.view, &m.schema, &map, vo, Some(&fixed), &seeds)
                    .map_err(engine_error)?;
            // a changed value is read by the pair's node and the nodes that refer to it;
            // those reached from them backwards join at once, so a change that runs
            // along a chain takes one more typing, not one per link
            let mut stack: Vec<Id> = Vec::new();
            for &((n, k), v) in &values {
                if t.values
                    .get(&(n, k))
                    .is_some_and(|old| old.holds() != v.holds())
                {
                    stack.push(n);
                }
            }
            let mut grown = false;
            while let Some(y) = stack.pop() {
                if region.insert(y) {
                    queue.push(y);
                    grown = true;
                }
                for x in incremental::Plan::readers(&r, post_data, y)? {
                    if region.insert(x) {
                        queue.push(x);
                        stack.push(x);
                        grown = true;
                    }
                }
                if region.len() > MAX_FOCUS {
                    return Ok(Propagation::Budget);
                }
            }
            if !grown {
                break (after, values, nodes);
            }
        };
        // the results before the write at the region's nodes, from the head's typing
        let before_map = incremental::associations(&m.map, &c.view, Some(pre_data), &nodes)?;
        let mut before: Vec<crate::ShapeResult> = Vec::new();
        let mut before_total = 0usize;
        for a in &before_map.0 {
            let NodeSelector::Term(node) = &a.node else {
                return Ok(Propagation::Unknown);
            };
            let (Some(id), Ok(kind)) = (
                c.view.lookup_term(node),
                crate::shapemap::label_kind(&m.schema, &a.shape),
            ) else {
                return Ok(Propagation::Unknown);
            };
            match t.values.get(&(id, kind)).map(|v| v.holds()) {
                Some(true) => {}
                Some(false) => before.push(crate::ShapeResult {
                    node: node.clone(),
                    shape: a.shape.clone(),
                    status: crate::Status::Nonconformant,
                    reason: None,
                    failures: Vec::new(),
                    prints: Vec::new(),
                }),
                None => return Ok(Propagation::Unknown),
            }
            before_total += 1;
        }
        let Some(checked) = self.moved(c, exact, after, &before, (before.len(), before_total), t0)
        else {
            return Ok(Propagation::Unknown);
        };
        let (s, exact, _) = checked;
        Ok(Propagation::Done(Box::new((
            s,
            exact,
            TypingUpdate::Delta(values),
        ))))
    }

    /// Move the counts of `exact` by the results of the associations validated in the
    /// states after (`after`) and before (`before`, with its nonconformant and total
    /// counts) the write, and summarize; `None` when the counts of the head cannot hold
    /// them (they are wrong).
    fn moved(
        &self,
        c: &Candidate<'_>,
        exact: &Exact,
        after: ResultMap,
        before: &[crate::ShapeResult],
        (before_nc, before_total): (usize, usize),
        t0: Instant,
    ) -> Option<Checked> {
        // the associations elsewhere are unchanged: move the counts by the difference
        let move_by =
            |n: u64, minus: usize, plus: usize| (n + plus as u64).checked_sub(minus as u64);
        let nonconformant = move_by(exact.nonconformant, before_nc, after.nonconformant)?;
        let total = move_by(
            exact.total,
            before_total,
            after.conformant + after.nonconformant,
        )?;
        let focus = (after.conformant + after.nonconformant) as u64;
        let introduced = self
            .grandfather()
            .then(|| new_nonconformant(&after.results, before));
        let mut s = self.summarize(
            after,
            introduced,
            self.limit(c.opts),
            t0.elapsed().as_millis() as u64,
        );
        s.strategy = Strategy::Incremental;
        s.blocking = nonconformant;
        s.total = total;
        s.by_severity.violation = nonconformant;
        s.conforms = nonconformant == 0;
        s.status = match (s.introduced.unwrap_or(nonconformant) > 0, self.cfg.mode) {
            (true, GuardMode::Reject) => GuardStatus::Rejected,
            (true, _) => GuardStatus::Warned,
            (false, _) => GuardStatus::Passed,
        };
        s.focus_nodes = Some(focus);
        if c.opts.dry_run.is_none() {
            self.associations.store(total, Ordering::Relaxed);
        }
        let exact = Exact {
            commit: c.base.commit + 1,
            nonconformant,
            total,
        };
        Some((s, exact, TypingUpdate::Drop))
    }

    /// Validate a write to the data graph: the associations it can affect when the
    /// counts of the head are known, everything otherwise.
    fn check_data(&self, m: &Model, c: &Candidate<'_>) -> sparkles::Result<Checked> {
        let t0 = Instant::now();
        let Changes::Log(log) = c.changes else {
            return self.full(m, m, c, "bulk");
        };
        if m.plan.sparql {
            return self.full(m, m, c, "sparql");
        }
        let exact = *self.exact.lock();
        let Some(exact) = exact.filter(|e| e.commit == c.base.commit) else {
            return self.full(m, m, c, "baseline");
        };
        if self.cfg.mode == GuardMode::Reject && !self.grandfather() && exact.nonconformant > 0 {
            // the report must show the associations that block every write
            return self.full(m, m, c, "baseline");
        }
        let mut graphs: FxHashMap<u64, bool> = FxHashMap::default();
        let mut seen: FxHashSet<[Id; 3]> = FxHashSet::default();
        let mut changes = Vec::new();
        for (_, q) in log.iter() {
            let data = *graphs.entry(q[3].0).or_insert_with(|| {
                self.cfg.data_graph.touches(
                    &c.view,
                    q[3].0,
                    self.cfg.include_inferences,
                    self.cfg.schema_graphs(),
                )
            });
            if data && seen.insert([q[0], q[1], q[2]]) {
                changes.push([q[0], q[1], q[2]]);
            }
        }
        let deadline = self.deadline(t0, c.opts);
        let base = Arc::new(c.base.clone());
        let state =
            |snap: &Arc<Snapshot>| -> sparkles::Result<Option<(ValidateOptions, DataGraph)>> {
                let Some(vo) = self.options(snap, c.opts, deadline) else {
                    return Ok(None);
                };
                let data = DataGraph::new(
                    snap.clone(),
                    vo.data_graph.as_deref(),
                    &vo.extra_graphs,
                    &vo.exclude_graphs,
                )?;
                Ok(Some((vo, data)))
            };
        let post = state(&c.view)?;
        let pre = state(&base)?;
        match self.propagated(m, c, &changes, &exact, &post, &pre, t0)? {
            Propagation::Done(checked) => return Ok(*checked),
            Propagation::Budget => return self.full(m, m, c, "budget"),
            Propagation::Unknown => {}
        }
        let affected = m.plan.affected(
            &c.view,
            [post.as_ref().map(|s| &s.1), pre.as_ref().map(|s| &s.1)],
            &changes,
            MAX_FOCUS,
        )?;
        let Some(affected) = affected else {
            return self.full(m, m, c, "budget");
        };
        let nodes: Vec<(Id, oxrdf::Term)> = affected
            .into_iter()
            .filter_map(|id| c.view.term(id).map(|t| (id, t)))
            .collect();
        let run = |snap: &Arc<Snapshot>, st: &Option<(ValidateOptions, DataGraph)>| {
            let map =
                incremental::associations(&m.map, &c.view, st.as_ref().map(|s| &s.1), &nodes)?;
            match st {
                Some((vo, _)) if !map.0.is_empty() => {
                    crate::validate(snap, &m.schema, &map, vo).map_err(engine_error)
                }
                _ => Ok(ResultMap {
                    conforms: true,
                    ..Default::default()
                }),
            }
        };
        let after = run(&c.view, &post)?;
        let before = run(&base, &pre)?;
        let counts = (
            before.nonconformant,
            before.conformant + before.nonconformant,
        );
        match self.moved(c, &exact, after, &before.results, counts, t0) {
            Some(checked) => Ok(checked),
            None => self.full(m, m, c, "baseline"),
        }
    }

    /// A write that changes a schema graph: the schema it leaves is read from the state
    /// after it, and that state is validated in full against it (in grandfather mode the
    /// state before is judged under the old schema). A schema that does not load, or
    /// does not define the shape map's labels, rejects the write.
    fn schema_write(
        &self,
        c: &Candidate<'_>,
        old: &Arc<Model>,
        seq: u64,
        live: bool,
    ) -> sparkles::Result<ValidationSummary> {
        let new = match load_graph_model(&self.cfg, &c.view) {
            Ok(m) => Arc::new(m),
            Err(e) => {
                let mut s = ValidationSummary::empty(
                    GuardStatus::Rejected,
                    self.cfg.mode,
                    Severity::Violation,
                );
                s.language = GuardLanguage::Shex;
                s.limit = self.cfg.report_limit;
                s.shapes_error = Some(format!("{e:#}"));
                if live {
                    self.counters.count(GuardStatus::Rejected);
                    self.history.record(c.kind, &s);
                }
                return Ok(s);
            }
        };
        let (summary, exact, typing) = self.full(&new, old, c, "schema")?;
        if !live {
            return Ok(summary);
        }
        self.counters.count(summary.status);
        self.history.record(c.kind, &summary);
        if summary.status != GuardStatus::Rejected {
            *self.pending.lock() = Some(Pending {
                seq,
                baseline: baseline_of(&summary, seq),
                exact: Some(exact),
                typing,
                model: Some(new),
            });
        }
        Ok(summary)
    }

    /// Count and bound the results, and decide: every nonconformant association blocks,
    /// or in grandfather mode (`introduced`: which results are new) every new one. New
    /// results are listed first.
    fn summarize(
        &self,
        mut rm: ResultMap,
        introduced: Option<Vec<bool>>,
        limit: usize,
        millis: u64,
    ) -> ValidationSummary {
        let blocking = rm.nonconformant as u64;
        let introduced_n = introduced
            .as_ref()
            .map(|v| v.iter().filter(|x| **x).count() as u64);
        if let Some(new) = &introduced {
            let mut ranked: Vec<(bool, crate::ShapeResult)> = new
                .iter()
                .copied()
                .zip(std::mem::take(&mut rm.results))
                .collect();
            // stable: map order within each group
            ranked.sort_by_key(|(n, _)| !*n);
            rm.results = ranked.into_iter().map(|(_, r)| r).collect();
        }
        let status = match (introduced_n.unwrap_or(blocking) > 0, self.cfg.mode) {
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
            introduced: introduced_n,
            focus_nodes: None,
            fallback: None,
            report_turtle: None,
        }
    }

    /// Whether a change can change the result map: a changed quad in a graph of the
    /// data graph, with a predicate a validation reads (any, when it may read any); and
    /// whether it changes a schema graph.
    fn relevant(&self, m: &Model, view: &Snapshot, changes: &Changes<'_>) -> (bool, bool) {
        let schema_graphs: Vec<u64> = self
            .cfg
            .schema_graphs()
            .iter()
            .filter_map(|g| view.lookup_iri(g).map(|i| i.0))
            .collect();
        let (log, bulk) = match changes {
            Changes::Log(log) => (*log, &[][..]),
            Changes::Rebuilt { log, bulk } => (*log, *bulk),
            Changes::Unknown => return (true, !self.cfg.schema_graphs().is_empty()),
        };
        let preds: Option<FxHashSet<Id>> = m
            .reads
            .as_ref()
            .map(|ps| ps.iter().filter_map(|p| view.lookup_iri(p)).collect());
        let mut graphs: FxHashMap<u64, bool> = FxHashMap::default();
        let (mut data, mut schema) = (false, false);
        for q in log.iter().map(|(_, q)| q).chain(bulk) {
            let g = q[3].0;
            if schema_graphs.contains(&g) {
                schema = true;
                continue;
            }
            if data || preds.as_ref().is_some_and(|ps| !ps.contains(&q[1])) {
                continue;
            }
            data |= *graphs.entry(g).or_insert_with(|| {
                self.cfg.data_graph.touches(
                    view,
                    g,
                    self.cfg.include_inferences,
                    self.cfg.schema_graphs(),
                )
            });
        }
        (data, schema)
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
        // a dry run validates like a write and records nothing: no counters, no history,
        // no state for the next commit
        let live = c.opts.dry_run.is_none();
        let model = self.model();
        let (relevant, schema_changed) = self.relevant(&model, &c.view, &c.changes);
        if schema_changed {
            return self.schema_write(c, &model, seq, live);
        }
        if !relevant {
            if !live {
                let mut s = ValidationSummary::empty(
                    GuardStatus::Skipped,
                    self.cfg.mode,
                    Severity::Violation,
                );
                s.language = GuardLanguage::Shex;
                s.limit = self.cfg.report_limit;
                return Ok(s);
            }
            self.counters.count(GuardStatus::Skipped);
            let baseline = self
                .baseline
                .lock()
                .clone()
                .map(|b| Baseline { commit: seq, ..b });
            let exact = self.exact.lock().map(|e| Exact { commit: seq, ..e });
            if let Some(baseline) = baseline {
                *self.pending.lock() = Some(Pending {
                    seq,
                    baseline,
                    exact,
                    typing: TypingUpdate::Keep,
                    model: None,
                });
            }
            let mut s =
                ValidationSummary::empty(GuardStatus::Skipped, self.cfg.mode, Severity::Violation);
            s.language = GuardLanguage::Shex;
            s.limit = self.cfg.report_limit;
            return Ok(s);
        }
        let (summary, exact, typing) = self.check_data(&model, c)?;
        if !live {
            return Ok(summary);
        }
        self.counters.count(summary.status);
        self.history.record(c.kind, &summary);
        if summary.status != GuardStatus::Rejected {
            *self.pending.lock() = Some(Pending {
                seq,
                baseline: baseline_of(&summary, seq),
                exact: Some(exact),
                typing,
                model: None,
            });
        }
        Ok(summary)
    }

    fn committed(&self, seq: u64) {
        let p = self.pending.lock().take();
        match p {
            Some(p) if p.seq == seq => {
                if let (Some((root, hash)), Some(_)) = (&self.persist, &p.exact)
                    && let Err(e) = StatusFile::of(&p.baseline, hash).write(root)
                {
                    tracing::warn!("cannot write {STATUS_FILE}: {e}");
                }
                if let Some(m) = p.model {
                    *self.model.write() = m;
                }
                *self.baseline.lock() = Some(p.baseline);
                *self.exact.lock() = p.exact;
                let mut typing = self.typing.lock();
                match p.typing {
                    TypingUpdate::Keep => match typing.as_mut() {
                        Some(t) if t.commit + 1 == seq => t.commit = seq,
                        _ => *typing = None,
                    },
                    TypingUpdate::Replace(generation, values) => {
                        *typing = Some(HeadTyping::new(seq, generation, values))
                    }
                    TypingUpdate::Delta(values) => match typing.as_mut() {
                        Some(t) if t.commit + 1 == seq => {
                            t.apply(values);
                            t.commit = seq;
                        }
                        _ => *typing = None,
                    },
                    TypingUpdate::Drop => *typing = None,
                }
            }
            // a commit the guard did not judge: the state is unknown
            _ => {
                if let Some(b) = self.baseline.lock().as_mut() {
                    b.commit = seq;
                    b.conforms = None;
                }
                *self.exact.lock() = None;
                *self.typing.lock() = None;
            }
        }
    }

    fn bypassed(&self) {
        self.counters.count(GuardStatus::Bypassed);
        if let Some(b) = self.baseline.lock().as_mut() {
            b.conforms = None;
        }
        *self.exact.lock() = None;
        *self.typing.lock() = None;
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

/// For each result of `after`: whether it is nonconformant and not among `before`
/// (associations, by node and shape, are matched as a multiset).
fn new_nonconformant(after: &[crate::ShapeResult], before: &[crate::ShapeResult]) -> Vec<bool> {
    let key = |r: &crate::ShapeResult| (r.node.to_string(), format!("{:?}", r.shape));
    let mut left: FxHashMap<(String, String), usize> = FxHashMap::default();
    for r in before
        .iter()
        .filter(|r| r.status == crate::Status::Nonconformant)
    {
        *left.entry(key(r)).or_default() += 1;
    }
    after
        .iter()
        .map(|r| {
            if r.status != crate::Status::Nonconformant {
                return false;
            }
            match left.get_mut(&key(r)) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    false
                }
                _ => true,
            }
        })
        .collect()
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
    let (compiled, closed) = compile_schema(&parsed, resolver)?;
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

/// Close a parsed schema over its imports, refuse EXTERNAL shapes without a definition,
/// and check and compile it.
fn compile_schema(parsed: &Schema, resolver: &dyn Resolver) -> Result<(CompiledSchema, Schema)> {
    let closed = crate::resolve::close(parsed, resolver).map_err(|e| anyhow!("schema: {e}"))?;
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
    Ok((compiled, closed))
}

/// The triples of the named graphs `graphs` of `snap`, as one graph (graphs the
/// snapshot does not have add nothing).
fn graphs_of(snap: &Snapshot, graphs: &[String]) -> sparkles::Result<oxrdf::Graph> {
    use sparkles::index::Perm;
    use sparkles::store::Chunk;
    let mut g = oxrdf::Graph::new();
    for iri in graphs {
        let Some(id) = snap.lookup_iri(iri) else {
            continue;
        };
        snap.scan(Perm::Gspo, &[id.0], |c| {
            let mut add = |k: &sparkles::index::Key| {
                if let Some(q) = snap.quad_to_terms(&Perm::Gspo.to_quad(k)) {
                    g.insert(&oxrdf::Triple::from(q));
                }
            };
            match c {
                Chunk::Block(b, s, e) => (s..e).for_each(|i| add(&b.key(i))),
                Chunk::Row(k) => add(&k),
            }
            Ok(true)
        })?;
    }
    Ok(g)
}

/// The schema a configuration keeps in ShExR graphs, read from `snap`. Imports are not
/// fetched (a write never fetches anything); the shape map may use the configuration's
/// `schema.prefixes`, or full IRIs.
fn load_graphs(cfg: &ShexValidationConfig, snap: &Snapshot) -> Result<Loaded> {
    let g = graphs_of(snap, cfg.schema_graphs())?;
    if g.is_empty() {
        bail!(
            "schema: the schema graphs {} hold no triples",
            cfg.schema_graphs()
                .iter()
                .map(|g| format!("<{g}>"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let mut parsed = crate::shexr::from_graph(&g, cfg.schema.base.as_deref())
        .map_err(|e| anyhow!("schema: {e}"))?;
    if !parsed.imports.is_empty() {
        bail!("schema: a schema in graphs cannot import other schemas (nothing is fetched)");
    }
    if parsed.prefixes.is_empty() {
        parsed.prefixes = cfg
            .schema
            .prefixes
            .iter()
            .map(|(p, ns)| (p.clone(), ns.clone()))
            .collect();
    }
    let (compiled, closed) = compile_schema(&parsed, &NoImports)?;
    Ok(Loaded {
        compiled,
        shape_count: closed.shapes.len(),
        file: "",
        format: "shexr",
        text: String::new(),
        prefixes: Vec::new(),
        base: closed.base.clone(),
    })
}

/// The schema and shape map of a configuration whose schema is in graphs, from `snap`.
fn load_graph_model(cfg: &ShexValidationConfig, snap: &Snapshot) -> Result<Model> {
    let loaded = load_graphs(cfg, snap)?;
    let map = parse_map(&cfg.shape_map, &loaded.compiled)?;
    Ok(Model::new(loaded, map))
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
    Ok(read_config_hashed(root)?.map(|(c, _)| c))
}

/// [`read_config`], and the SHA-256 of the file.
fn read_config_hashed(root: &Path) -> Result<Option<(ShexValidationConfig, String)>> {
    let path = root.join(CONFIG_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let cfg: ShexValidationConfig =
        serde_json::from_slice(&bytes).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    cfg.check()?;
    Ok(Some((cfg, sha256_hex(&bytes))))
}

/// Install the guard of a persistent store from its ShEx `validation.json` (after
/// [`Store::open`]): the schema is read from its copy in the database, without
/// resolving anything. Without a configuration nothing happens. A configuration that
/// cannot be loaded is an error, and the store keeps refusing writes (fail closed).
pub fn install(store: &Store) -> Result<Option<Arc<ShexGuard>>> {
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
    let loaded = match &cfg.schema.graphs {
        Some(_) => load_graphs(&cfg, &store.snapshot())?,
        None => load_copy(&cfg, root)?,
    };
    let map = parse_map(&cfg.shape_map, &loaded.compiled)?;
    let g = Arc::new(ShexGuard::new(
        cfg,
        loaded,
        map,
        Some((root.to_path_buf(), hash.clone())),
    ));
    // the state of the head, when the status file records it for this configuration
    let head = store.head_commit().seq;
    if let Some(b) = StatusFile::read(root, head, &hash) {
        *g.exact.lock() = Some(Exact {
            commit: head,
            nonconformant: b.blocking,
            total: b.total,
        });
        g.associations.store(b.total, Ordering::Relaxed);
        *g.baseline.lock() = Some(b);
    }
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
            StatusFile::remove(r)?;
        }
        drop(txn);
        return Ok(SetOutcome::Removed);
    };
    cfg.check()?;
    let view = Arc::new(txn.view());
    let in_graphs = cfg.schema.graphs.is_some();
    let mut loaded = match cfg.schema.inline.take() {
        _ if in_graphs => load_graphs(&cfg, &view)?,
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
    if !in_graphs {
        cfg.schema.file = Some(loaded.file.to_string());
        cfg.schema.format = Some(loaded.format.to_string());
        cfg.schema.prefixes = loaded.prefixes.iter().cloned().collect();
        if loaded.file == SHEX_SCHEMA_SHEXJ_FILE && cfg.schema.base.is_none() {
            cfg.schema.base = loaded.base.clone();
        }
    }
    let file = loaded.file;
    let text = std::mem::take(&mut loaded.text);
    let mut guard = ShexGuard::new(cfg, loaded, map, None);
    if root.is_none() && !in_graphs {
        guard.copy = Some(text.clone());
    }

    let model = guard.model();
    let (summary, typing) = guard.validate_state(&model, &view, None, &WriteOptions::default())?;
    if guard.cfg.mode == GuardMode::Reject && !guard.grandfather() && summary.blocking > 0 {
        return Ok(SetOutcome::NotConforming(summary));
    }
    let head = txn.base().commit;
    let baseline = baseline_of(&summary, head);
    if let Some(r) = &root {
        // a configuration of either language this one replaces leaves nothing behind
        // a schema in graphs keeps no copy
        let keep: &[&str] = if in_graphs {
            &[CONFIG_FILE]
        } else {
            &[CONFIG_FILE, file]
        };
        sparkles::guard::config::remove_files(r, keep)?;
        StatusFile::remove(r)?;
        if !in_graphs {
            write_atomic(&r.join(file), text.as_bytes())?;
        }
        let bytes = serde_json::to_vec_pretty(&guard.cfg)?;
        write_atomic(&r.join(CONFIG_FILE), &bytes)?;
        let hash = sha256_hex(&bytes);
        StatusFile::of(&baseline, &hash).write(r)?;
        guard.persist = Some((r.clone(), hash));
    }
    *guard.exact.lock() = Some(Exact {
        commit: head,
        nonconformant: summary.blocking,
        total: summary.total,
    });
    if model.keeps_typing() {
        *guard.typing.lock() = Some(HeadTyping::new(head, view.generation.uid, typing));
    }
    *guard.baseline.lock() = Some(baseline);
    let guard = Arc::new(guard);
    store.set_guard(Some(guard.clone()));
    store.set_guard_required(true);
    drop(txn);
    Ok(SetOutcome::Installed(guard, summary))
}

#[cfg(test)]
mod tests;
