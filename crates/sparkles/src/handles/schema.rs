//! Schema discovery: the schema report with its paginated lists, the report of two
//! states compared, class profiles, draft shapes, VoID and the SHACL constraints layer.
//!
//! The dataset keeps the last report it computed with every graph readable. A later
//! request with the same selection at the same state reads it, and one at a newer commit
//! brings it up to date from the changes when they are few enough
//! ([`schema::update`](crate::schema::update)). The cursors of the paginated lists name
//! the report they belong to, so a listing can be finished after a write.

use crate::Dataset;
use crate::access::GraphAccess;
use crate::dataset::SchemaCacheEntry;
use crate::error::{ComponentError, Error, Result};
use crate::history::{At, HistoryOptions};
use crate::schema::draft::{DraftOptions, ShapesDraft};
use crate::schema::profile::{ClassProfiles, ProfileOptions};
use crate::schema::{
    self, ClassEntry, ConstraintsLayer, HasIri, Page, PredicateEntry, SchemaDiff, SchemaError,
    SchemaOptions, SchemaReport, SchemaSummary, VoidOptions,
};
use crate::store::Snapshot;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Schema discovery (from [`Dataset::schema`]).
#[derive(Clone)]
pub struct Schema {
    pub(crate) ds: Dataset,
}

/// The number of entries in a page of a report's lists, unless a request asks for
/// another.
pub const DEFAULT_PAGE: usize = 1000;

/// A request for a schema report: what it describes, the state it describes, and the
/// page of its lists.
#[derive(Clone, Debug)]
pub struct ReportRequest {
    /// The selection and limits of the report. Its `deadline` bounds the work when
    /// `timeout` is `None`.
    pub options: SchemaOptions,
    /// A past state (`None`: the head).
    pub at: Option<At>,
    /// How long opening a past state, and then computing the report, may each take.
    /// Each step's deadline starts when the step does.
    pub timeout: Option<Duration>,
    /// The most entries of a page.
    pub limit: usize,
    /// The `next` token of the previous page, for the page after it.
    pub cursor: Option<String>,
}

impl Default for ReportRequest {
    fn default() -> ReportRequest {
        ReportRequest {
            options: SchemaOptions::default(),
            at: None,
            timeout: None,
            limit: DEFAULT_PAGE,
            cursor: None,
        }
    }
}

/// How the report of a request came about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Computed {
    /// the dataset's kept report
    Cached,
    /// computed from the indexes
    Full,
    /// the kept report of an earlier commit, brought up to date from this many changes
    Updated(usize),
}

impl std::fmt::Display for Computed {
    /// `cached`, `full` or `updated; changes=N`, as the server's
    /// `Sparkles-Schema-Report` header has it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Computed::Cached => f.write_str("cached"),
            Computed::Full => f.write_str("full"),
            Computed::Updated(n) => write!(f, "updated; changes={n}"),
        }
    }
}

/// A schema report with the page of its lists that the request asked for.
#[derive(Clone)]
#[non_exhaustive]
pub struct ReportOutcome {
    pub report: Arc<SchemaReport>,
    pub computed: Computed,
    /// the state the report describes
    pub snapshot: Arc<Snapshot>,
    selection: u64,
    limit: usize,
    /// the last IRI of the previous page
    after: Option<String>,
}

impl std::fmt::Debug for ReportOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReportOutcome")
            .field("report", &self.report)
            .field("computed", &self.computed)
            .field("commit", &self.snapshot.commit)
            .field("limit", &self.limit)
            .field("after", &self.after)
            .finish_non_exhaustive()
    }
}

impl ReportOutcome {
    /// The page of classes after the request's cursor.
    pub fn classes(&self) -> Page<'_, ClassEntry> {
        self.page(&self.report.classes, true)
    }

    /// The page of predicates after the request's cursor.
    pub fn predicates(&self) -> Page<'_, PredicateEntry> {
        self.page(&self.report.predicates, true)
    }

    /// The summary document, with the first page of each list.
    pub fn summary<'a>(&'a self, dataset: &'a str) -> SchemaSummary<'a> {
        self.report.summary(
            dataset,
            self.page(&self.report.classes, false),
            self.page(&self.report.predicates, false),
        )
    }

    fn page<'a, T: HasIri>(&self, items: &'a [T], after_cursor: bool) -> Page<'a, T> {
        let after = self.after.as_deref().filter(|_| after_cursor);
        let (items_page, more) = schema::page_after(items, after, self.limit);
        Page {
            items: items_page,
            total: items.len(),
            next: more.then(|| {
                Cursor {
                    v: self.report.snapshot.version,
                    h: self.selection,
                    a: items_page.last().map_or_else(
                        || after.unwrap_or_default().to_string(),
                        |x| x.iri().to_string(),
                    ),
                }
                .encode()
            }),
        }
    }
}

/// The SHACL shapes a constraints layer is built from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShapesRequest {
    /// the write guard's shapes: `None` uses them when the dataset has them,
    /// `Some(true)` requires them and `Some(false)` leaves them out
    pub guard: Option<bool>,
    /// shapes graphs of the dataset: IRIs, or `default`
    pub graphs: Vec<String>,
}

impl ShapesRequest {
    /// The sources named by `shapes=` values: `guard`, `none`, `default` or a graph IRI.
    /// No value means the guard's shapes when there are any.
    pub fn from_values<S: AsRef<str>>(values: &[S]) -> std::result::Result<ShapesRequest, String> {
        if values.is_empty() {
            return Ok(ShapesRequest::default());
        }
        let mut r = ShapesRequest {
            guard: Some(false),
            graphs: Vec::new(),
        };
        let mut none = false;
        for v in values {
            let g = match v.as_ref().trim() {
                "none" => {
                    none = true;
                    continue;
                }
                "guard" => {
                    r.guard = Some(true);
                    continue;
                }
                "default" | crate::sparql::ctx::DEFAULT_GRAPH_IRI => "default".to_string(),
                "union" | crate::sparql::ctx::UNION_GRAPH_IRI => {
                    return Err(
                        "shapes: name the graphs that hold shapes, not the union graph".into(),
                    );
                }
                iri => {
                    let iri = iri
                        .strip_prefix('<')
                        .and_then(|i| i.strip_suffix('>'))
                        .unwrap_or(iri);
                    oxrdf::NamedNode::new(iri)
                        .map_err(|e| format!("shapes: invalid graph IRI '{iri}': {e}"))?;
                    iri.to_string()
                }
            };
            if !r.graphs.contains(&g) {
                r.graphs.push(g);
            }
        }
        if none && values.len() > 1 {
            return Err("shapes=none cannot be combined with other shapes".into());
        }
        Ok(r)
    }
}

/// A request for the constraints layer alone (see [`Schema::constraints`]).
#[derive(Clone, Debug, Default)]
pub struct ConstraintsRequest {
    pub shapes: ShapesRequest,
    /// A past state (`None`: the head).
    pub at: Option<At>,
    /// How long opening a past state may take.
    pub timeout: Option<Duration>,
    /// The graphs the caller may read (`None`: every graph). It reads only shapes graphs
    /// it may read, and sees the guard's shapes only when it may read all of the guard's
    /// shapes graphs.
    pub graphs: Option<Arc<GraphAccess>>,
}

/// Continuation token of a paginated list: base64url JSON.
#[derive(Serialize, Deserialize)]
struct Cursor {
    /// snapshot identity of the report
    v: u64,
    /// selection hash
    h: u64,
    /// last IRI of the previous page
    a: String,
}

impl Cursor {
    fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).unwrap_or_default())
    }

    fn decode(s: &str) -> Option<Cursor> {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(s.trim()).ok()?).ok()
    }
}

/// FNV-1a: stable across restarts and builds.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

/// The hash of what selects a report: the graphs, the declarations, the inferences, the
/// caller's view and the details. A kept report and a cursor apply to one selection.
fn selection(opts: &SchemaOptions) -> u64 {
    let declared = opts.declared_graph.as_ref().unwrap_or(&opts.graph).name();
    let view = opts.graphs.as_ref().map_or(String::new(), |g| g.read_key());
    let mut s = format!(
        "{}\n{declared}\n{}\n{}\n{view}",
        opts.graph.name(),
        opts.include_inferred,
        opts.declared_from_inferred
    );
    if opts.subject_classes {
        s.push_str("\nsubjectClasses");
    }
    fnv(&s)
}

fn deadline(timeout: Option<Duration>, otherwise: Option<Instant>) -> Option<Instant> {
    timeout.map(|t| Instant::now() + t).or(otherwise)
}

impl Schema {
    /// The schema report of the state `req.at` names, with the page of its lists after
    /// `req.cursor`. A cursor of other selection parameters is [`Error::Invalid`], and
    /// one whose report is gone because the dataset changed is [`Error::Conflict`].
    pub fn report(&self, req: &ReportRequest) -> Result<ReportOutcome> {
        let store = self.ds.store();
        let (snap, current) = match &req.at {
            None => {
                let snap = store.snapshot();
                let id = schema::snapshot_identity(&snap);
                (snap, id)
            }
            Some(at) => {
                let o = HistoryOptions {
                    cancel: None,
                    deadline: deadline(req.timeout, req.options.deadline),
                };
                let (snap, r) = store.snapshot_at(at, &o)?;
                // a past state never changes: its identity is its commit (live versions
                // are small counters, so the high bit keeps the two apart)
                let id = if r.historical {
                    (1 << 63) | r.commit.seq
                } else {
                    schema::snapshot_identity(&snap)
                };
                (snap, id)
            }
        };
        let selection = selection(&req.options);
        let cursor = req
            .cursor
            .as_deref()
            .filter(|c| !c.is_empty())
            .map(|c| Cursor::decode(c).ok_or_else(|| Error::invalid("malformed cursor")))
            .transpose()?;
        let wanted = match &cursor {
            Some(c) if c.h != selection => {
                return Err(Error::invalid(
                    "the cursor belongs to different graph/declaredGraph/reasoning/declared/detail parameters",
                ));
            }
            Some(c) => c.v,
            None => current,
        };
        let outcome = |report, computed, snap| ReportOutcome {
            report,
            computed,
            snapshot: snap,
            selection,
            limit: req.limit,
            after: cursor.as_ref().map(|c| c.a.clone()),
        };
        if let Some(report) = self.kept(wanted, selection, req.options.term_totals) {
            return Ok(outcome(report, Computed::Cached, snap));
        }
        if wanted != current {
            return Err(Error::Conflict(format!(
                "snapshot changed since the first page (version {wanted} → {current}); restart from the first page"
            )));
        }
        let mut opts = req.options.clone();
        opts.deadline = deadline(req.timeout, opts.deadline);
        let (report, computed) =
            self.compute(&snap, current, selection, &opts, req.at.is_none())?;
        Ok(outcome(report, computed, snap))
    }

    /// The schema report of a state the caller has opened: the dataset's kept report
    /// when it describes this state with the same selection, else a new one, which the
    /// dataset keeps when `opts.graphs` is `None`.
    pub fn report_at(
        &self,
        snap: &Arc<Snapshot>,
        opts: &SchemaOptions,
    ) -> Result<(Arc<SchemaReport>, Computed)> {
        let identity = schema::snapshot_identity(snap);
        let selection = selection(opts);
        if let Some(report) = self.kept(identity, selection, opts.term_totals) {
            return Ok((report, Computed::Cached));
        }
        self.compute(snap, identity, selection, opts, true)
    }

    /// What changed from the report of state `from` to the report `req` asks for
    /// (whose `at` is the newer state), with how the newer report came about. The
    /// older report is computed for this call and not kept.
    pub fn diff(&self, from: &At, req: &ReportRequest) -> Result<(SchemaDiff, Computed)> {
        let deadline = deadline(req.timeout, req.options.deadline);
        let o = HistoryOptions {
            cancel: None,
            deadline,
        };
        let (old_snap, _) = self.ds.store().snapshot_at(from, &o)?;
        let mut opts = req.options.clone();
        opts.deadline = deadline;
        let old = schema::discover(&old_snap, &opts).map_err(schema_error)?;
        let new = self.report(&ReportRequest {
            cursor: None,
            ..req.clone()
        })?;
        Ok((schema::compare(&old, &new.report), new.computed))
    }

    /// The VoID description of the report `req` asks for, with the declarations or
    /// without. The report counts the distinct subjects and objects of the selection as
    /// well.
    pub fn void(&self, req: &ReportRequest, opts: &VoidOptions) -> Result<Vec<oxrdf::Triple>> {
        let mut req = req.clone();
        req.options.term_totals = true;
        req.cursor = None;
        let r = self.report(&req)?;
        Ok(schema::void_triples(&r.report, opts))
    }

    /// For each class, the predicates its instances use and the kinds of their values,
    /// at the state `at` names (`None`: the head). `opts.schema.deadline` also bounds
    /// opening a past state.
    pub fn profiles_at(&self, opts: &ProfileOptions, at: Option<&At>) -> Result<ClassProfiles> {
        let snap = self.state(at, opts.schema.deadline)?;
        schema::profile::profiles(&snap, opts).map_err(schema_error)
    }

    /// [`profiles_at`](Self::profiles_at) the head.
    pub fn profiles(&self, opts: &ProfileOptions) -> Result<ClassProfiles> {
        self.profiles_at(opts, None)
    }

    /// SHACL shapes drafted from the data at the state `at` names (`None`: the head).
    /// `opts.schema.deadline` also bounds opening a past state.
    pub fn draft_shapes_at(&self, opts: &DraftOptions, at: Option<&At>) -> Result<ShapesDraft> {
        let snap = self.state(at, opts.schema.deadline)?;
        schema::draft::draft_shapes(&snap, opts).map_err(schema_error)
    }

    /// [`draft_shapes_at`](Self::draft_shapes_at) the head.
    pub fn draft_shapes(&self, opts: &DraftOptions) -> Result<ShapesDraft> {
        self.draft_shapes_at(opts, None)
    }

    /// The SHACL constraints layer of the shapes `req` names, at the state `req.at`
    /// names. Without a source (no guard and no graphs) the layer is empty. A shapes
    /// graph that does not exist, or that the caller may not read, is
    /// [`Error::NotFound`], and so is a required guard that the dataset does not have;
    /// shapes that do not parse are [`Error::Invalid`]. Builds without the `shacl`
    /// feature fail with [`Error::Unsupported`] for any source.
    pub fn constraints(&self, req: &ConstraintsRequest) -> Result<ConstraintsLayer> {
        let snap = self.state(req.at.as_ref(), deadline(req.timeout, None))?;
        Ok(self
            .constraints_at(&snap, &req.shapes, req.graphs.as_deref())?
            .unwrap_or_default())
    }

    /// The constraints layer of `shapes` over a state the caller has opened, or `None`
    /// when it has no source (see [`constraints`](Self::constraints)).
    pub fn constraints_at(
        &self,
        snap: &Snapshot,
        shapes: &ShapesRequest,
        view: Option<&GraphAccess>,
    ) -> Result<Option<ConstraintsLayer>> {
        #[cfg(feature = "shacl")]
        return constraints_layer(&self.ds, snap, shapes, view);
        #[cfg(not(feature = "shacl"))]
        {
            let _ = (snap, view);
            if shapes.guard == Some(true) || !shapes.graphs.is_empty() {
                return Err(Error::unsupported("built without the `shacl` feature"));
            }
            Ok(None)
        }
    }

    /// The state `at` names, or the head.
    fn state(&self, at: Option<&At>, deadline: Option<Instant>) -> Result<Arc<Snapshot>> {
        Ok(match at {
            None => self.ds.snapshot(),
            Some(at) => {
                let o = HistoryOptions {
                    cancel: None,
                    deadline,
                };
                self.ds.store().snapshot_at(at, &o)?.0
            }
        })
    }

    /// The kept report of state `identity` and `selection`, if it has what the request
    /// needs.
    fn kept(&self, identity: u64, selection: u64, term_totals: bool) -> Option<Arc<SchemaReport>> {
        let cache = self.ds.state().schema_cache.lock();
        cache
            .as_ref()
            .filter(|e| {
                e.identity == identity
                    && e.selection == selection
                    && (e.report.term_totals.is_some() || !term_totals)
            })
            .map(|e| e.report.clone())
    }

    /// A new report of `snap`, brought up to date from the kept one when `maintain`
    /// allows it and it can be, and kept when the caller reads every graph.
    fn compute(
        &self,
        snap: &Arc<Snapshot>,
        identity: u64,
        selection: u64,
        opts: &SchemaOptions,
        maintain: bool,
    ) -> Result<(Arc<SchemaReport>, Computed)> {
        let started = Instant::now();
        let updated = if maintain && opts.graphs.is_none() {
            self.maintained(snap, selection, opts)
                .map_err(schema_error)?
        } else {
            None
        };
        let (report, computed) = match updated {
            Some((r, n)) => (Arc::new(r), Computed::Updated(n)),
            None => (
                Arc::new(schema::discover(snap, opts).map_err(schema_error)?),
                Computed::Full,
            ),
        };
        tracing::debug!(
            "schema of {} at version {identity} ({computed}) in {:?}: {} classes, {} predicates",
            self.ds.name().unwrap_or("the dataset"),
            started.elapsed(),
            report.classes.len(),
            report.predicates.len()
        );
        // the dataset keeps the report everyone with access to every graph shares
        if opts.graphs.is_none() {
            *self.ds.state().schema_cache.lock() = Some(SchemaCacheEntry {
                identity,
                selection,
                report: report.clone(),
                mark: (!self.ds.store().is_persistent()).then(|| snap.mark()),
            });
        }
        Ok((report, computed))
    }

    /// The kept report brought up to date at `snap`, when it has the same selection at
    /// an earlier commit, the changes since then are in the write-ahead logs and number
    /// at most [`schema::max_changes`], and the report is one that can be maintained.
    fn maintained(
        &self,
        snap: &Arc<Snapshot>,
        selection: u64,
        opts: &SchemaOptions,
    ) -> std::result::Result<Option<(SchemaReport, usize)>, SchemaError> {
        let store = self.ds.store();
        let (old, mark) = match self.ds.state().schema_cache.lock().as_ref() {
            Some(e) if e.selection == selection => (e.report.clone(), e.mark.clone()),
            _ => return Ok(None),
        };
        if old.maintenance.is_none() || old.snapshot.commit > snap.commit {
            return Ok(None);
        }
        let o = crate::store::DiffOptions {
            max_quads: schema::max_changes(&old),
            deadline: opts.deadline,
            cancel: opts.cancel.clone(),
            ..Default::default()
        };
        // a persistent dataset reads the changes from its write-ahead logs; an in-memory
        // one compares the deltas of two states of one generation
        let diff = match &mark {
            Some(m) => store.diff_since(m, snap, &o).ok().flatten(),
            None => {
                let at = At::Commit;
                store
                    .diff(&at(old.snapshot.commit), &at(snap.commit), &o)
                    .ok()
                    // a state comparison reads the whole store: a new report costs as much
                    .filter(|d| d.method != crate::store::DiffMethod::Compare)
            }
        };
        // history that is gone, or more changes than an update is worth, is a new report
        let Some(diff) = diff else {
            return Ok(None);
        };
        Ok(schema::update(&old, snap, &diff, opts)?.map(|r| (r, diff.len())))
    }
}

/// The constraints layer of `req` over `snap`, or `None` when it has no source.
#[cfg(feature = "shacl")]
fn constraints_layer(
    ds: &Dataset,
    snap: &Snapshot,
    req: &ShapesRequest,
    view: Option<&GraphAccess>,
) -> Result<Option<ConstraintsLayer>> {
    let view = view.filter(|v| !v.reads_all());
    let readable = |g: &str| {
        view.is_none_or(|v| {
            if g == "default" {
                v.read.default_graph()
            } else {
                v.read.allows_iri(g)
            }
        })
    };
    let mut layer = ConstraintsLayer::default();
    if req.guard != Some(false) {
        let guard = match ds.write_guard() {
            Some(crate::write_guard::WriteGuard::Shacl(g)) => Some(g),
            _ => None,
        };
        let guard = guard.filter(|g| {
            g.config()
                .shapes
                .graphs
                .iter()
                .flatten()
                .all(|s| readable(s))
        });
        match guard {
            Some(g) => layer
                .sources
                .push(sparkles_shacl::constraints::guard_source(&g)),
            None if req.guard == Some(true) => {
                return Err(Error::NotFound(format!(
                    "dataset {} has no write-time SHACL validation whose shapes this caller may read",
                    ds.name().unwrap_or("")
                )));
            }
            None => {}
        }
    }
    if !req.graphs.is_empty() {
        for g in &req.graphs {
            let exists = g == "default" || graph_exists(snap, g);
            if !exists || !readable(g) {
                return Err(Error::NotFound(format!("no such graph: <{g}>")));
            }
        }
        let source = sparkles_shacl::constraints::graphs_source(snap, &req.graphs)
            .map_err(|e| Error::invalid(format!("shapes: {e:#}")))?;
        layer.sources.push(source);
    }
    Ok((!layer.is_empty()).then_some(layer))
}

/// Whether the dataset has a named graph `iri` with at least one quad.
#[cfg(feature = "shacl")]
fn graph_exists(snap: &Snapshot, iri: &str) -> bool {
    snap.lookup_iri(iri)
        .is_some_and(|g| snap.count(crate::index::Perm::Gspo, &[g.0]).unwrap_or(0) > 0)
}

/// The engine error of a schema discovery error. A store error keeps its variant and
/// cancellation is [`Error::Cancelled`]. A missing graph, a deadline that passed and a
/// selection over its entry limit are [`Error::Component`] errors of the `schema`
/// component, with the codes `no-such-graph`, `timeout` and `too-many-entries`, whose
/// source is the [`SchemaError`] (see [`schema_error_of`]).
pub(crate) fn schema_error(e: SchemaError) -> Error {
    let code = match e {
        SchemaError::Store(e) => return e,
        SchemaError::Cancelled => return Error::Cancelled,
        SchemaError::NoSuchGraph(_) => "no-such-graph",
        SchemaError::Timeout { .. } => "timeout",
        SchemaError::TooManyEntries { .. } => "too-many-entries",
    };
    Error::Component(Box::new(
        ComponentError::new("schema", code, e.to_string()).with_source(e),
    ))
}

/// The schema discovery error inside an error of a [`Schema`] call, if it is one.
pub fn schema_error_of(e: &Error) -> Option<&SchemaError> {
    match e {
        Error::Component(c) if c.component == "schema" => {
            c.source.as_ref()?.downcast_ref::<SchemaError>()
        }
        _ => None,
    }
}
