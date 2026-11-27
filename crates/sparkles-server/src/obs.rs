//! Observability: request ids, the access log, Prometheus metrics and readiness.
//!
//! [`observe`] is the outermost middleware. It resolves the request id, opens the request
//! span (which `TraceLayer` then uses through [`MakeSpan`]), classifies the request by
//! its matched route, counts it as active, and when the response is ready records the
//! metrics and emits one `sparkles::access` event. If the request future is dropped
//! first (the client went away, or the server shut down), a drop guard records the
//! request as cancelled instead. Handlers add detail (rows, timing, sizes) by putting a
//! [`RequestReport`] into the response extensions.
//!
//! The metrics registry is hand-rolled: atomics on the request path, labels from closed
//! sets plus dataset names capped at `--metrics-max-datasets` (the rest share `$other`;
//! requests naming no existing dataset use `$none`).

use crate::state::{AppState, DbType};
use axum::extract::{MatchedPath, Path, Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use parking_lot::{Mutex, RwLock};
use serde_json::{Value as J, json};
use sparkles::BudgetKind;
use sparkles::sparql::Timing;
use sparkles::store::{REBUILD_BUCKETS, RebuildHistogram, RebuildReason};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing::Span;

mod fuseki;
mod streaming;
pub(crate) use streaming::DeferredReport;

#[cfg(test)]
mod completion_dispatch_tests;

pub static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Default value of `--metrics-max-datasets`.
pub const DEFAULT_METRICS_MAX_DATASETS: usize = 100;

/// Dataset label of requests that name no existing dataset.
const NONE: &str = "$none";
/// Dataset label shared by the datasets beyond the label cap.
const OTHER: &str = "$other";

static NEXT_METRICS_ID: AtomicU64 = AtomicU64::new(1);

fn next_metrics_id() -> u64 {
    NEXT_METRICS_ID.fetch_add(1, Ordering::Relaxed)
}

struct MetricsId(u64);

impl Default for MetricsId {
    fn default() -> Self {
        Self(next_metrics_id())
    }
}

fn clone_rebuild(h: &RebuildHistogram) -> RebuildHistogram {
    RebuildHistogram {
        buckets: h.buckets,
        sum_seconds: h.sum_seconds,
    }
}

fn add_rebuild(dst: &mut RebuildHistogram, src: &RebuildHistogram) {
    for (a, b) in dst.buckets.iter_mut().zip(src.buckets) {
        *a = a.saturating_add(b);
    }
    dst.sum_seconds += src.sum_seconds;
}

fn sub_rebuild(a: &RebuildHistogram, b: &RebuildHistogram) -> RebuildHistogram {
    let mut buckets = [0u64; REBUILD_BUCKETS.len() + 1];
    for ((out, x), y) in buckets.iter_mut().zip(a.buckets).zip(b.buckets) {
        *out = x.saturating_sub(y);
    }
    RebuildHistogram {
        buckets,
        sum_seconds: (a.sum_seconds - b.sum_seconds).max(0.0),
    }
}

fn max_rebuild(a: &RebuildHistogram, b: &RebuildHistogram) -> RebuildHistogram {
    let mut buckets = [0u64; REBUILD_BUCKETS.len() + 1];
    for ((out, x), y) in buckets.iter_mut().zip(a.buckets).zip(b.buckets) {
        *out = x.max(y);
    }
    RebuildHistogram {
        buckets,
        sum_seconds: a.sum_seconds.max(b.sum_seconds),
    }
}

fn any_rebuild(hs: &[RebuildHistogram; 2]) -> bool {
    hs.iter()
        .any(|h| h.buckets.iter().any(|&c| c > 0) || h.sum_seconds > 0.0)
}

/// Per-store monotonic counters carried in `$other` across deletion, promotion, and
/// persistent store reopen so shared overflow series never drop or emit negative deltas.
#[derive(Default)]
pub(crate) struct StoreCounters {
    rebuilds: [RebuildHistogram; 2],
    cache_hits: u64,
    cache_misses: u64,
    rcache_hits: u64,
    rcache_misses: u64,
}

impl StoreCounters {
    pub(crate) fn read(store: &sparkles::store::Store) -> Self {
        let (c, r) = (store.cache(), store.result_cache());
        Self {
            rebuilds: RebuildReason::ALL.map(|reason| store.rebuilds(reason)),
            cache_hits: c.hits(),
            cache_misses: c.misses(),
            rcache_hits: r.hits(),
            rcache_misses: r.misses(),
        }
    }

    fn clone_counters(&self) -> Self {
        Self {
            rebuilds: [
                clone_rebuild(&self.rebuilds[0]),
                clone_rebuild(&self.rebuilds[1]),
            ],
            cache_hits: self.cache_hits,
            cache_misses: self.cache_misses,
            rcache_hits: self.rcache_hits,
            rcache_misses: self.rcache_misses,
        }
    }

    fn add_assign(&mut self, src: &Self) {
        add_rebuild(&mut self.rebuilds[0], &src.rebuilds[0]);
        add_rebuild(&mut self.rebuilds[1], &src.rebuilds[1]);
        self.cache_hits = self.cache_hits.saturating_add(src.cache_hits);
        self.cache_misses = self.cache_misses.saturating_add(src.cache_misses);
        self.rcache_hits = self.rcache_hits.saturating_add(src.rcache_hits);
        self.rcache_misses = self.rcache_misses.saturating_add(src.rcache_misses);
    }

    fn sub(&self, b: &Self) -> Self {
        Self {
            rebuilds: [
                sub_rebuild(&self.rebuilds[0], &b.rebuilds[0]),
                sub_rebuild(&self.rebuilds[1], &b.rebuilds[1]),
            ],
            cache_hits: self.cache_hits.saturating_sub(b.cache_hits),
            cache_misses: self.cache_misses.saturating_sub(b.cache_misses),
            rcache_hits: self.rcache_hits.saturating_sub(b.rcache_hits),
            rcache_misses: self.rcache_misses.saturating_sub(b.rcache_misses),
        }
    }

    fn max_with(&self, b: &Self) -> Self {
        Self {
            rebuilds: [
                max_rebuild(&self.rebuilds[0], &b.rebuilds[0]),
                max_rebuild(&self.rebuilds[1], &b.rebuilds[1]),
            ],
            cache_hits: self.cache_hits.max(b.cache_hits),
            cache_misses: self.cache_misses.max(b.cache_misses),
            rcache_hits: self.rcache_hits.max(b.rcache_hits),
            rcache_misses: self.rcache_misses.max(b.rcache_misses),
        }
    }

    fn any(&self) -> bool {
        any_rebuild(&self.rebuilds)
            || self.cache_hits > 0
            || self.cache_misses > 0
            || self.rcache_hits > 0
            || self.rcache_misses > 0
    }
}

#[derive(Default)]
struct StoreOtherState {
    /// Highest live-store counters observed while this open store was aggregated under `$other`.
    seen_under_other: StoreCounters,
    /// Counters retired to `$other` when this open store was promoted to a named slot,
    /// subtracted from its named slot to prevent both `$other` shrinkage and double counting.
    promoted_offset: StoreCounters,
}

struct PendingStore {
    /// Active leaving/swapping tokens for this `open_id`. Normally 1; can briefly be 2
    /// when a doomed delete or rename overlaps an in-place restore swap on the same store.
    tokens: BTreeSet<u64>,
    contrib: Option<StoreCounters>,
    in_overflow: bool,
}

#[derive(Default)]
struct OtherStoreLedger {
    /// Permanently retired live-store counters from deleted, promoted, or reopened overflow stores.
    retired: StoreCounters,
    /// Counters temporarily held for stores currently leaving or swapping (delete/rename/restore),
    /// keyed by `DatasetState::open_id`.
    pending: BTreeMap<u64, PendingStore>,
    /// Monotonic token generator for `pending` entries so an older cancellation cannot clear
    /// a newer or overlapping pending owner for the same `open_id`.
    next_pending_token: u64,
    /// Counters of stores whose `leaving` ended without retiring (failed delete/rename/swap or
    /// memory rename) while a scrape was in flight (`active_scrapes > 0`). Added to `$other`
    /// only for scrapes whose live sample set missed the store. Cleared when `active_scrapes == 0`.
    restored_during_scrape: BTreeMap<u64, StoreCounters>,
    /// `open_id`s finalized (deleted or closed/reopened) while `active_scrapes > 0`, so a
    /// stale `Arc<Dataset>` held by a concurrent scrape is skipped rather than doubled or
    /// resurrecting a deleted named slot. Cleared when `active_scrapes == 0`.
    retired_ids: BTreeSet<u64>,
    /// `(from_name, open_id)` aliases vacated by an in-memory rename while `active_scrapes > 0`,
    /// so a stale `LiveSample` holding the dead alias is skipped instead of allocating a ghost
    /// named slot or misattributing named counters to `$other`. Cleared when `active_scrapes == 0`.
    renamed_from: BTreeSet<(String, u64)>,
    /// Number of `gauges()` scrapes currently in flight.
    active_scrapes: usize,
    /// Per-open-store `$other` tracking, keyed by `DatasetState::open_id`.
    stores: BTreeMap<u64, StoreOtherState>,
}

fn is_in_overflow(
    max_datasets: usize,
    name: &str,
    datasets: &BTreeMap<String, Arc<DsMetrics>>,
) -> bool {
    let named = datasets.keys().filter(|k| !k.starts_with('$')).count();
    !datasets.contains_key(name) && named >= max_datasets && datasets.contains_key(OTHER)
}

fn other_contribution_for(
    in_overflow: bool,
    open_id: u64,
    raw: &StoreCounters,
    stores: &BTreeMap<u64, StoreOtherState>,
) -> Option<StoreCounters> {
    let st_entry = stores.get(&open_id);
    let seen = st_entry
        .map(|s| s.seen_under_other.clone_counters())
        .unwrap_or_default();
    let contrib = if in_overflow {
        let eff = match st_entry {
            Some(s) => raw.sub(&s.promoted_offset),
            None => raw.clone_counters(),
        };
        eff.max_with(&seen)
    } else {
        seen
    };
    contrib.any().then_some(contrib)
}

fn max_opt_counters(
    prev: Option<StoreCounters>,
    next: Option<StoreCounters>,
) -> Option<StoreCounters> {
    match (prev, next) {
        (Some(a), Some(b)) => Some(a.max_with(&b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

#[cfg(all(test, feature = "otel"))]
type GaugesHook = Box<dyn FnOnce() + Send>;

#[cfg(all(test, feature = "otel"))]
static GAUGES_AFTER_DATASETS_HOOKS: std::sync::LazyLock<Mutex<BTreeMap<usize, GaugesHook>>> =
    std::sync::LazyLock::new(|| Mutex::new(BTreeMap::new()));

#[cfg(all(test, feature = "otel"))]
pub(crate) fn set_gauges_after_datasets_hook(st: &AppState, f: impl FnOnce() + Send + 'static) {
    let key = std::ptr::from_ref(st) as usize;
    GAUGES_AFTER_DATASETS_HOOKS.lock().insert(key, Box::new(f));
}

// ---------------------------------------------------------------- request ids ------

/// An incoming id is kept when it is 1–128 bytes of `[A-Za-z0-9._:-]`.
pub fn valid_request_id(id: &[u8]) -> bool {
    (1..=128).contains(&id.len())
        && id
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// `{boot:08x}-{seq:012x}`: 32 random bits per process and a process-wide sequence, so
/// ids are unique within a process lifetime and sort in arrival order.
pub fn new_request_id() -> String {
    static BOOT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let boot = *BOOT.get_or_init(|| {
        use std::hash::{BuildHasher, Hasher};
        // std's per-process random hash keys, mixed with the clock and the pid
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        h.write_u32(std::process::id());
        let x = h.finish();
        (x ^ (x >> 32)) as u32
    });
    format!("{boot:08x}-{:012x}", SEQ.fetch_add(1, Ordering::Relaxed))
}

// -------------------------------------------------------------- classification ------

/// The kind of work a request does (a metric label).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Query,
    Update,
    Gsp,
    Upload,
    /// applying RDF Patch
    Patch,
    Shacl,
    Shex,
    Explain,
    Admin,
    /// a message to the MCP endpoint (`/$/mcp`)
    Mcp,
    /// a GraphQL request (`/{ds}/graphql`)
    Graphql,
    Other,
}

impl Op {
    pub const ALL: [Op; 12] = [
        Op::Query,
        Op::Update,
        Op::Gsp,
        Op::Upload,
        Op::Patch,
        Op::Shacl,
        Op::Shex,
        Op::Explain,
        Op::Admin,
        Op::Mcp,
        Op::Graphql,
        Op::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Op::Query => "query",
            Op::Update => "update",
            Op::Gsp => "gsp",
            Op::Upload => "upload",
            Op::Patch => "patch",
            Op::Shacl => "shacl",
            Op::Shex => "shex",
            Op::Explain => "explain",
            Op::Admin => "admin",
            Op::Mcp => "mcp",
            Op::Graphql => "graphql",
            Op::Other => "other",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// How a request ended (a metric label).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    ClientError,
    Error,
    Timeout,
    Cancelled,
    Budget,
    /// refused by the auth layer (401, 403, cross-origin, hidden-dataset 404)
    Denied,
    /// refused by a rate or concurrency limit (`429`, or `503` when saturated)
    RateLimited,
    /// a write rejected by write-time validation (`422`)
    Rejected,
}

impl Outcome {
    pub const ALL: [Outcome; 9] = [
        Outcome::Ok,
        Outcome::ClientError,
        Outcome::Error,
        Outcome::Timeout,
        Outcome::Cancelled,
        Outcome::Budget,
        Outcome::Denied,
        Outcome::RateLimited,
        Outcome::Rejected,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::ClientError => "client_error",
            Outcome::Error => "error",
            Outcome::Timeout => "timeout",
            Outcome::Cancelled => "cancelled",
            Outcome::Budget => "budget",
            Outcome::Denied => "denied",
            Outcome::RateLimited => "rate_limited",
            Outcome::Rejected => "rejected",
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    fn from_status(s: StatusCode) -> Outcome {
        if s.is_client_error() {
            Outcome::ClientError
        } else if s.is_server_error() {
            Outcome::Error
        } else {
            Outcome::Ok
        }
    }
}

/// What a handler reports about its request, carried in the response extensions.
#[derive(Clone, Debug, Default)]
pub struct RequestReport {
    /// refines the route's operation (`/{ds}` dispatches by parameters and body)
    pub operation: Option<Op>,
    /// set when the engine error has an outcome of its own (timeout, cancelled, budget)
    pub outcome: Option<Outcome>,
    pub budget: Option<BudgetKind>,
    /// query: result rows (triples for CONSTRUCT / DESCRIBE); writes: quads changed
    pub rows: Option<u64>,
    pub timing: Option<Timing>,
    pub serialize_ms: Option<f64>,
    /// uncompressed body size
    pub response_bytes: Option<u64>,
    pub mem_peak_bytes: Option<u64>,
    /// queries and updates: the rows every operator produced (the `rows-produced` work)
    pub rows_produced: Option<u64>,
    /// Native cursor completion, distinct from a consumer output cap.
    pub cursor_status: Option<sparkles::sparql::CursorStatus>,
    pub transport_wait_ms: Option<f64>,
    /// the limit class that refused the request (outcome `rate_limited`)
    pub limit_class: Option<crate::ratelimit::Class>,
    /// write-time validation status and time (from the `Sparkles-Validation` header)
    pub validation: Option<&'static str>,
    pub validation_ms: Option<u64>,
    /// query: the work of its spatial operators (candidates, exact tests, matches)
    pub geo_work: Option<crate::geo::Work>,
    /// a GraphQL request: its operation name, document hash and engine executions
    pub graphql: Option<GraphqlReport>,
}

/// What the access log and the metrics record of a GraphQL request.
#[derive(Clone, Debug, Default)]
pub struct GraphqlReport {
    pub operation: Option<String>,
    /// SHA-256 of the document
    pub document: String,
    pub groups: u64,
}

impl RequestReport {
    /// Attach the report to a response.
    pub fn attach(self, mut resp: Response) -> Response {
        resp.extensions_mut().insert(self);
        resp
    }
}

/// Routes that serve the UI or health checks: logged at DEBUG and not counted.
fn quiet(route: Option<&str>) -> bool {
    route.is_some_and(|r| {
        r == "/"
            || r == "/ui"
            || r.starts_with("/ui/")
            || r == "/$/ping"
            || r.starts_with("/$/ready")
            || r == "/$/metrics"
    })
}

/// The operation of a matched route; `/{ds}` is classified from its parameters and
/// content type (a form body is refined by the handler's report).
fn route_op(route: Option<&str>, req: &Request) -> Op {
    let Some(r) = route else { return Op::Other };
    if r == "/$/mcp" {
        return Op::Mcp;
    }
    if r.starts_with("/$/") {
        return Op::Admin;
    }
    match r {
        "/{ds}/sparql" | "/{ds}/query" | "/{ds}/sparql/diff" | "/{ds}/queries/{name}" => Op::Query,
        "/{ds}/update" => Op::Update,
        "/{ds}/data" | "/{ds}/get" | "/{ds}/{*graph}" => Op::Gsp,
        "/{ds}/upload" | "/{ds}/mappings/{name}/run" => Op::Upload,
        "/{ds}/patch" => Op::Patch,
        "/{ds}/shacl" => Op::Shacl,
        "/{ds}/shex" => Op::Shex,
        "/{ds}/explain" => Op::Explain,
        "/{ds}/graphql" | "/{ds}/graphql/schema" => Op::Graphql,
        "/{ds}" => {
            let q = req.uri().query().unwrap_or("");
            let has = |k: &str| form_urlencoded::parse(q.as_bytes()).any(|(a, _)| a == k);
            let ct = req
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if has("query") || ct.starts_with("application/sparql-query") {
                Op::Query
            } else if has("update") || ct.starts_with("application/sparql-update") {
                Op::Update
            } else if ct.starts_with(sparkles::patch::MEDIA_TYPE) {
                Op::Patch
            } else {
                Op::Gsp
            }
        }
        _ => Op::Other,
    }
}

/// The `{ds}` segment of the request path, if the route has one.
pub(crate) fn ds_param(route: Option<&str>, uri: &Uri) -> Option<String> {
    let pos = route?.split('/').position(|s| s == "{ds}")?;
    let seg = uri.path().split('/').nth(pos)?;
    Some(
        percent_encoding::percent_decode_str(seg)
            .decode_utf8_lossy()
            .into_owned(),
    )
}

// ----------------------------------------------------------------- middleware ------

/// The request span, created by [`observe`] and handed to `TraceLayer`.
#[derive(Clone)]
struct RequestSpan(Span);

/// `TraceLayer` span maker: the span [`observe`] opened for the request.
#[derive(Clone, Copy)]
pub struct MakeSpan;

impl<B> tower_http::trace::MakeSpan<B> for MakeSpan {
    fn make_span(&mut self, req: &axum::http::Request<B>) -> Span {
        req.extensions()
            .get::<RequestSpan>()
            .map_or_else(Span::none, |s| s.0.clone())
    }
}

/// Sets the query's cancellation flag when dropped: a handler holds one while it waits
/// for the blocking task, so a client disconnect (which drops the handler) stops the
/// query at its next check.
pub struct CancelOnDrop(pub Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub async fn observe(State(st): State<Arc<AppState>>, mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(&X_REQUEST_ID)
        .filter(|v| valid_request_id(v.as_bytes()))
        .and_then(|v| v.to_str().ok())
        .map_or_else(new_request_id, str::to_string);
    let id_value = HeaderValue::from_str(&id).expect("request ids are header-safe");
    req.headers_mut()
        .insert(X_REQUEST_ID.clone(), id_value.clone());
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string());
    let span = crate::otel::request_span(&id, req.method(), route.as_deref());
    let otel = crate::otel::on_request(&span, &req, route.as_deref(), &id);
    req.extensions_mut().insert(RequestSpan(span.clone()));
    let op = route_op(route.as_deref(), &req);
    let head = req.method() == axum::http::Method::HEAD;
    let mut pending = Pending::new(
        st,
        span,
        op,
        ds_param(route.as_deref(), req.uri()),
        quiet(route.as_deref()),
    );
    pending.otel = otel;
    pending.fuseki = fuseki::route(route.as_deref());
    let mut resp = next.run(req).await;
    let mut report = resp
        .extensions_mut()
        .remove::<RequestReport>()
        .unwrap_or_default();
    if let Some((status, ms)) = resp
        .headers()
        .get(SPARKLES_VALIDATION)
        .and_then(|v| validation_header(v.to_str().ok()?))
    {
        report.validation = Some(status);
        report.validation_ms = ms;
        if status == "rejected" && report.outcome.is_none() {
            report.outcome = Some(Outcome::Rejected);
        }
    }
    let auth = resp.extensions_mut().remove::<crate::auth::AuthReport>();
    if auth
        .as_ref()
        .is_some_and(|a| a.denied.is_some() || a.error.is_some())
    {
        report.outcome = Some(Outcome::Denied);
    }
    pending.auth = auth;
    crate::otel::response_headers(&pending.otel, resp.headers_mut());
    if let Some(deferred) = resp.extensions_mut().remove::<DeferredReport>() {
        deferred.observe(pending, resp.status().as_u16(), head);
        let body = std::mem::replace(resp.body_mut(), axum::body::Body::empty());
        *resp.body_mut() = deferred.wrap(body);
        if let Some(abort) = resp.extensions().get::<crate::http::StreamAbort>() {
            abort.bind_observation(move || deferred.abort_body());
        }
    } else {
        pending.complete(resp.status().as_u16(), &report);
    }
    resp.headers_mut().insert(X_REQUEST_ID.clone(), id_value);
    resp
}

/// A request in flight: counted as active until dropped; dropped before
/// [`Pending::complete`] means the request was abandoned.
struct Pending {
    st: Arc<AppState>,
    span: Span,
    access_dispatch: Option<tracing::Dispatch>,
    op: Op,
    dataset: Option<String>,
    quiet: bool,
    start: Instant,
    done: bool,
    auth: Option<crate::auth::AuthReport>,
    otel: crate::otel::Req,
    /// the Fuseki endpoint of the route, if it has one
    fuseki: Option<fuseki::Route>,
}

impl Pending {
    fn new(st: Arc<AppState>, span: Span, op: Op, dataset: Option<String>, quiet: bool) -> Self {
        if !quiet {
            st.metrics.active[op.index()].fetch_add(1, Ordering::Relaxed);
        }
        // Completion may run on a producer thread with another default subscriber.
        let access_dispatch = st
            .access_log
            .then(|| tracing::dispatcher::get_default(|dispatch| dispatch.clone()));
        Pending {
            st,
            span,
            access_dispatch,
            op,
            dataset,
            quiet,
            start: Instant::now(),
            done: false,
            auth: None,
            otel: Default::default(),
            fuseki: None,
        }
    }

    fn complete(mut self, status: u16, report: &RequestReport) {
        self.finish(status, report);
    }

    fn finish(&mut self, status: u16, report: &RequestReport) {
        self.done = true;
        let elapsed = self.start.elapsed();
        let op = report.operation.unwrap_or(self.op);
        let outcome = report.outcome.unwrap_or_else(|| {
            Outcome::from_status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
        });
        // checked now: a dataset deleted by this request has no series any more
        let dataset = self.dataset.take().filter(|d| self.st.get(d).is_some());
        if !self.quiet {
            self.st
                .metrics
                .record(dataset.as_deref(), op, outcome, elapsed, report);
            if let Some(r) = self.fuseki {
                let ok = outcome == Outcome::Ok;
                let m = &self.st.metrics;
                m.record_fuseki(dataset.as_deref(), r, op, ok, self.st.read_only);
            }
        }
        crate::otel::on_response(
            &self.otel,
            &self.span,
            &self.st,
            status,
            op,
            outcome,
            dataset.as_deref(),
            elapsed,
            report,
        );
        if let Some(dispatch) = &self.access_dispatch {
            // The request's span may be disabled while access events are enabled.
            tracing::dispatcher::with_default(dispatch, || {
                access_event(
                    &self.span,
                    self.quiet,
                    dataset.as_deref(),
                    op,
                    status,
                    outcome,
                    elapsed,
                    report,
                    self.auth.as_ref(),
                );
            });
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.done {
            // never sent: the status appears only in the log
            let report = RequestReport {
                outcome: Some(Outcome::Cancelled),
                ..Default::default()
            };
            self.finish(499, &report);
        }
        if !self.quiet {
            self.st.metrics.active[self.op.index()].fetch_sub(1, Ordering::Relaxed);
        }
    }
}

fn ms(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

const SPARKLES_VALIDATION: &str = "sparkles-validation";

/// `status` and `ms` of a `Sparkles-Validation` header (`status=passed, mode=…, ms=14`).
fn validation_header(v: &str) -> Option<(&'static str, Option<u64>)> {
    let mut status = None;
    let mut ms = None;
    for (k, v) in v.split(',').filter_map(|kv| kv.trim().split_once('=')) {
        match k {
            "status" => status = VALIDATION_STATUS.iter().copied().find(|s| *s == v),
            "ms" => ms = v.parse().ok(),
            _ => {}
        }
    }
    Some((status?, ms))
}

#[allow(clippy::too_many_arguments)]
fn access_event(
    span: &Span,
    quiet: bool,
    dataset: Option<&str>,
    op: Op,
    status: u16,
    outcome: Outcome,
    elapsed: Duration,
    r: &RequestReport,
    auth: Option<&crate::auth::AuthReport>,
) {
    let t = r.timing.as_ref();
    // the principal's log name only; never a credential
    let principal = auth.map(|a| a.principal.as_deref().unwrap_or("-"));
    let scheme = auth.and_then(|a| a.scheme);
    let auth_error = auth.and_then(|a| a.error);
    macro_rules! event {
        ($level:ident) => {
            tracing::$level!(
                target: "sparkles::access",
                dataset = dataset.unwrap_or(NONE),
                operation = op.as_str(),
                status,
                outcome = outcome.as_str(),
                rows = r.rows,
                parse_ms = t.map(|t| ms(t.parse_ms)),
                plan_ms = t.map(|t| ms(t.plan_ms)),
                exec_ms = t.map(|t| ms(t.exec_ms)),
                serialize_ms = r.serialize_ms.map(ms),
                total_ms = ms(elapsed.as_secs_f64() * 1000.0),
                response_bytes = r.response_bytes,
                mem_peak_bytes = r.mem_peak_bytes,
                rows_produced = r.rows_produced,
                cursor_status = r.cursor_status.map(|s| s.as_str()),
                transport_wait_ms = r.transport_wait_ms.map(ms),
                validation = r.validation,
                validation_ms = r.validation_ms,
                graphql_operation = r.graphql.as_ref().and_then(|g| g.operation.as_deref()),
                graphql_document = r.graphql.as_ref().map(|g| g.document.as_str()),
                graphql_groups = r.graphql.as_ref().map(|g| g.groups),
                principal,
                auth = scheme,
                auth_error,
                "completed"
            )
        };
    }
    span.in_scope(|| if quiet { event!(debug) } else { event!(info) });
}

/// Log the text of a query or update at DEBUG under `sparkles::query` (never at INFO:
/// it may hold data), cut to its first 2048 characters.
pub fn log_query_text(text: &str) {
    if tracing::enabled!(target: "sparkles::query", tracing::Level::DEBUG) {
        let cut = text
            .char_indices()
            .nth(2048)
            .map_or(text, |(i, _)| &text[..i]);
        tracing::debug!(target: "sparkles::query", query_len = text.len(), query = cut);
    }
    crate::otel::query_text(text);
}

// -------------------------------------------------------------------- metrics ------

/// Upper bounds (seconds) of the request duration histogram, without `+Inf`.
const BUCKETS: [f64; 16] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0,
];
const BUCKET_LABELS: [&str; 17] = [
    "0.001", "0.0025", "0.005", "0.01", "0.025", "0.05", "0.1", "0.25", "0.5", "1", "2.5", "5",
    "10", "30", "60", "300", "+Inf",
];

/// Non-cumulative bucket counts (cumulated when scraped), so `+Inf` equals `_count`
/// exactly even under concurrent updates.
#[derive(Default)]
struct Histogram {
    buckets: [AtomicU64; 17],
    sum_nanos: AtomicU64,
}

impl Histogram {
    fn observe(&self, d: Duration) {
        let s = d.as_secs_f64();
        let i = BUCKETS
            .iter()
            .position(|b| s <= *b)
            .unwrap_or(BUCKETS.len());
        self.buckets[i].fetch_add(1, Ordering::Relaxed);
        self.sum_nanos.fetch_add(
            u64::try_from(d.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    /// Cumulative counts per bucket (the last is `+Inf`, the total count).
    fn cumulative(&self) -> [u64; 17] {
        let mut out = [0u64; 17];
        let mut acc = 0;
        for (i, b) in self.buckets.iter().enumerate() {
            acc += b.load(Ordering::Relaxed);
            out[i] = acc;
        }
        out
    }
}

#[derive(Default)]
struct OpMetrics {
    seen: AtomicBool,
    outcomes: [AtomicU64; 9],
    duration: Histogram,
    response_bytes: AtomicU64,
}

/// Counters of one dataset label.
#[derive(Default)]
pub struct DsMetrics {
    series_id: MetricsId,
    ops: [OpMetrics; Op::ALL.len()],
    result_rows: AtomicU64,
    budget: [AtomicU64; BudgetKind::ALL.len()],
    rate_limited: [AtomicU64; crate::ratelimit::Class::COUNT],
    /// candidates, exact tests and matches of spatial operators
    geo_work: [AtomicU64; crate::geo::WORK.len()],
    /// requests per Fuseki endpoint (`--metrics-fuseki-names`)
    fuseki: fuseki::Counters,
    /// engine executions per GraphQL request, by [`GROUP_BUCKETS`]
    graphql_groups: [AtomicU64; GROUP_BUCKETS.len() + 1],
    graphql_groups_sum: AtomicU64,
    /// rows produced by the operators of queries and updates
    rows_produced: AtomicU64,
    /// queries by their estimated memory peak, by [`MEMORY_BUCKETS`], not cumulative
    mem_peak: [AtomicU64; MEMORY_BUCKETS.len() + 1],
    mem_peak_sum: AtomicU64,
}

/// Upper bounds in bytes of the `sparkles_query_memory_peak_bytes` histogram, without
/// `+Inf`: 1 MiB to 16 GiB in steps of ×4.
pub(crate) const MEMORY_BUCKETS: [u64; 8] = [
    1 << 20,
    1 << 22,
    1 << 24,
    1 << 26,
    1 << 28,
    1 << 30,
    1 << 32,
    1 << 34,
];

impl DsMetrics {
    /// Cumulative bucket counts (the last is `+Inf`, the total count) and byte sum of
    /// `sparkles_query_memory_peak_bytes`, or `None` when no query has been observed.
    fn mem_peak_snapshot(&self) -> Option<([u64; MEMORY_BUCKETS.len() + 1], u64)> {
        let mut cum = [0u64; MEMORY_BUCKETS.len() + 1];
        let mut acc = 0u64;
        for (i, c) in self.mem_peak.iter().enumerate() {
            acc += c.load(Ordering::Relaxed);
            cum[i] = acc;
        }
        if acc == 0 {
            return None;
        }
        Some((cum, self.mem_peak_sum.load(Ordering::Relaxed)))
    }
}

/// Cumulative bucket counts (the last is `+Inf`, the total count) and second sum of a
/// rebuild histogram, or `None` when no rebuild has been recorded.
fn rebuild_cumulative(h: &RebuildHistogram) -> Option<([u64; REBUILD_BUCKETS.len() + 1], f64)> {
    let mut cum = [0u64; REBUILD_BUCKETS.len() + 1];
    let mut acc = 0u64;
    for (i, &n) in h.buckets.iter().enumerate() {
        acc += n;
        cum[i] = acc;
    }
    if acc == 0 {
        return None;
    }
    Some((cum, h.sum_seconds))
}

/// Upper bounds of the `sparkles_graphql_groups` histogram, without `+Inf`.
const GROUP_BUCKETS: [u64; 7] = [1, 2, 4, 8, 16, 32, 64];

fn budget_index(k: BudgetKind) -> usize {
    match k {
        BudgetKind::Rows => 0,
        BudgetKind::Memory => 1,
        BudgetKind::ResultBytes => 2,
        BudgetKind::DecompressedBytes => 3,
        BudgetKind::OutboundBytes => 4,
        BudgetKind::ValidationWork => 5,
        BudgetKind::RowsProduced => 6,
        BudgetKind::DatasetBytes => 7,
        BudgetKind::HiddenQuads => 8,
        BudgetKind::ServerMemory => 9,
        BudgetKind::Spill => 10,
    }
}

pub struct Metrics {
    pub enabled: bool,
    /// also render Fuseki's metric names (`--metrics-fuseki-names`)
    pub fuseki_names: bool,
    max_datasets: usize,
    datasets: RwLock<BTreeMap<String, Arc<DsMetrics>>>,
    /// Coordinated `$other` live-store counter ledger (always locked before `datasets`).
    other_store: Mutex<OtherStoreLedger>,
    /// in-flight requests per operation of the matched route (kept even when metrics
    /// are off)
    active: [AtomicI64; Op::ALL.len()],
}

impl Metrics {
    pub fn new(enabled: bool, max_datasets: usize) -> Metrics {
        Metrics {
            enabled,
            fuseki_names: false,
            max_datasets,
            datasets: RwLock::new(BTreeMap::new()),
            other_store: Mutex::new(OtherStoreLedger::default()),
            active: Default::default(),
        }
    }

    /// The counters for a dataset label: the dataset's own while fewer than
    /// `max_datasets` have one, else `$other`; `None` is `$none`.
    fn series(&self, dataset: Option<&str>) -> (String, Arc<DsMetrics>) {
        let key = dataset.unwrap_or(NONE);
        if let Some(m) = self.datasets.read().get(key) {
            return (key.to_string(), m.clone());
        }
        let mut w = self.datasets.write();
        let named = w.keys().filter(|k| !k.starts_with('$')).count();
        let key = if dataset.is_some() && named >= self.max_datasets {
            OTHER
        } else {
            key
        };
        let m = w.entry(key.to_string()).or_default().clone();
        (key.to_string(), m)
    }

    fn record(
        &self,
        dataset: Option<&str>,
        op: Op,
        outcome: Outcome,
        elapsed: Duration,
        r: &RequestReport,
    ) {
        if !self.enabled {
            return;
        }
        let (_, ds) = self.series(dataset);
        let m = &ds.ops[op.index()];
        m.seen.store(true, Ordering::Relaxed);
        m.outcomes[outcome.index()].fetch_add(1, Ordering::Relaxed);
        m.duration.observe(elapsed);
        if let Some(b) = r.response_bytes {
            m.response_bytes.fetch_add(b, Ordering::Relaxed);
        }
        if op == Op::Query
            && let Some(n) = r.rows
        {
            ds.result_rows.fetch_add(n, Ordering::Relaxed);
        }
        if dataset.is_some()
            && let Some(n) = r.rows_produced
        {
            ds.rows_produced.fetch_add(n, Ordering::Relaxed);
        }
        if dataset.is_some()
            && op == Op::Query
            && let Some(b) = r.mem_peak_bytes
        {
            let i = MEMORY_BUCKETS
                .iter()
                .position(|m| b <= *m)
                .unwrap_or(MEMORY_BUCKETS.len());
            ds.mem_peak[i].fetch_add(1, Ordering::Relaxed);
            ds.mem_peak_sum.fetch_add(b, Ordering::Relaxed);
        }
        if let Some(g) = r.geo_work {
            for (c, n) in ds.geo_work.iter().zip(g) {
                c.fetch_add(n, Ordering::Relaxed);
            }
        }
        if let Some(g) = &r.graphql {
            let i = GROUP_BUCKETS
                .iter()
                .position(|b| g.groups <= *b)
                .unwrap_or(GROUP_BUCKETS.len());
            ds.graphql_groups[i].fetch_add(1, Ordering::Relaxed);
            ds.graphql_groups_sum.fetch_add(g.groups, Ordering::Relaxed);
        }
        if outcome == Outcome::Budget
            && let Some(k) = r.budget
        {
            ds.budget[budget_index(k)].fetch_add(1, Ordering::Relaxed);
        }
        if let Some(c) = r.limit_class {
            ds.rate_limited[c.index()].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Count the bytes of a streamed response, known only once the stream ends.
    pub fn add_response_bytes(&self, dataset: Option<&str>, op: Op, bytes: u64) {
        if !self.enabled {
            return;
        }
        let (_, ds) = self.series(dataset);
        ds.ops[op.index()]
            .response_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }

    /// The `dataset` label of a request to `dataset` (see [`Metrics::series`]).
    pub fn dataset_label(&self, dataset: Option<&str>) -> String {
        self.series(dataset).0
    }

    /// `--metrics-max-datasets` (other capped labels, like backup repositories, share it).
    #[cfg_attr(not(any(feature = "backup", feature = "otel")), allow(dead_code))]
    pub fn max_datasets(&self) -> usize {
        self.max_datasets
    }

    /// Drop a deleted dataset's series (a recreated name starts again at zero).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn forget(&self, dataset: &str) {
        self.commit_delete(dataset, None);
    }

    /// Record a leaving or swapping store's pending `$other` counters so concurrent scrapes
    /// during delete, rename, or in-place restore never observe a transient drop. Returns a
    /// token for [`Self::cancel_leaving`] when metrics are enabled.
    pub(crate) fn begin_leaving(
        &self,
        name: &str,
        open_id: u64,
        raw: StoreCounters,
    ) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        let mut ledger_guard = self.other_store.lock();
        let ledger = &mut *ledger_guard;
        ledger.restored_during_scrape.remove(&open_id);
        let r = self.datasets.read();
        let in_overflow = is_in_overflow(self.max_datasets, name, &r);
        let contrib = other_contribution_for(in_overflow, open_id, &raw, &ledger.stores);
        let token = ledger.next_pending_token;
        ledger.next_pending_token = ledger.next_pending_token.wrapping_add(1);
        match ledger.pending.get_mut(&open_id) {
            Some(existing) => {
                existing.tokens.insert(token);
                existing.in_overflow |= in_overflow;
                existing.contrib = max_opt_counters(existing.contrib.take(), contrib);
            }
            None => {
                let mut tokens = BTreeSet::new();
                tokens.insert(token);
                ledger.pending.insert(
                    open_id,
                    PendingStore {
                        tokens,
                        contrib,
                        in_overflow,
                    },
                );
            }
        }
        Some(token)
    }

    /// Refresh a leaving store's pending `$other` counters after draining in-flight
    /// handles, preserving the monotonic maximum against the initial pending snapshot.
    pub(crate) fn refresh_leaving(&self, name: &str, open_id: u64, raw: StoreCounters) {
        if !self.enabled {
            return;
        }
        let mut ledger_guard = self.other_store.lock();
        let ledger = &mut *ledger_guard;
        let Some(existing) = ledger.pending.get_mut(&open_id) else {
            return;
        };
        let r = self.datasets.read();
        let in_overflow = existing.in_overflow || is_in_overflow(self.max_datasets, name, &r);
        let contrib = other_contribution_for(in_overflow, open_id, &raw, &ledger.stores);
        existing.in_overflow = in_overflow;
        existing.contrib = max_opt_counters(existing.contrib.take(), contrib);
    }

    /// Clear a leaving or swapping store's pending token when delete, rename, or restore
    /// did not close the store.
    pub(crate) fn cancel_leaving(&self, open_id: u64, token: u64) {
        if !self.enabled {
            return;
        }
        let mut ledger = self.other_store.lock();
        let remove = match ledger.pending.get_mut(&open_id) {
            Some(existing) => {
                existing.tokens.remove(&token);
                existing.tokens.is_empty()
            }
            None => return,
        };
        if remove
            && let Some(entry) = ledger.pending.remove(&open_id)
            && let Some(contrib) = entry.contrib
            && ledger.active_scrapes > 0
        {
            ledger.restored_during_scrape.insert(open_id, contrib);
        }
    }

    /// Commit a completed dataset deletion: drop any named series and retire any pending
    /// `$other` counters atomically under `other_store -> datasets`.
    pub(crate) fn commit_delete(&self, dataset: &str, open_id: Option<u64>) {
        let mut ledger = self.other_store.lock();
        let mut w = self.datasets.write();
        w.remove(dataset);
        if let Some(id) = open_id {
            ledger.restored_during_scrape.remove(&id);
            if let Some(contrib) = ledger.pending.remove(&id).and_then(|e| e.contrib) {
                ledger.retired.add_assign(&contrib);
                w.entry(OTHER.to_string()).or_default();
            }
            ledger.stores.remove(&id);
            if ledger.active_scrapes > 0 {
                ledger.retired_ids.insert(id);
            }
        }
    }

    /// Retire a leaving or swapping store whose underlying store was closed and either
    /// reopened at zero or left closed after a failed delete, rename, or restore swap.
    pub(crate) fn retire_reopened(&self, open_id: u64) {
        if !self.enabled {
            return;
        }
        let mut ledger = self.other_store.lock();
        let mut w = self.datasets.write();
        ledger.restored_during_scrape.remove(&open_id);
        if let Some(contrib) = ledger.pending.remove(&open_id).and_then(|e| e.contrib) {
            ledger.retired.add_assign(&contrib);
            w.entry(OTHER.to_string()).or_default();
        }
        ledger.stores.remove(&open_id);
        if ledger.active_scrapes > 0 {
            ledger.retired_ids.insert(open_id);
        }
    }

    /// Commit a completed rename: move any named series from `from` to `to`, keeping the
    /// store's `$other` state intact when the same open store survived (memory rename) or
    /// retiring old `$other` counters and dropping old promotion offsets when the store
    /// reopened at zero (persistent rename).
    pub(crate) fn commit_rename(
        &self,
        from: &str,
        to: &str,
        old_open_id: Option<u64>,
        new_open_id: u64,
    ) {
        let mut ledger = self.other_store.lock();
        let mut w = self.datasets.write();
        if let Some(series) = w.remove(from) {
            w.insert(to.to_string(), series);
        }
        if let Some(old_id) = old_open_id {
            ledger.restored_during_scrape.remove(&old_id);
            let contrib = ledger.pending.remove(&old_id).and_then(|e| e.contrib);
            if old_id == new_open_id {
                if ledger.active_scrapes > 0 {
                    ledger.renamed_from.insert((from.to_string(), old_id));
                    ledger.renamed_from.remove(&(to.to_string(), old_id));
                    if let Some(c) = contrib {
                        ledger.restored_during_scrape.insert(old_id, c);
                    }
                }
            } else {
                if let Some(c) = contrib {
                    ledger.retired.add_assign(&c);
                    w.entry(OTHER.to_string()).or_default();
                }
                ledger.stores.remove(&old_id);
                if ledger.active_scrapes > 0 {
                    ledger.retired_ids.insert(old_id);
                }
            }
        }
    }

    #[cfg(all(test, feature = "otel", feature = "backup"))]
    pub(crate) fn other_ledger_store_ids(&self) -> Vec<u64> {
        self.other_store.lock().stores.keys().copied().collect()
    }

    fn ensure_other_slot(&self) {
        self.datasets.write().entry(OTHER.to_string()).or_default();
    }

    fn has_other_slot(&self) -> bool {
        self.datasets.read().contains_key(OTHER)
    }

    #[cfg(all(test, feature = "otel"))]
    pub(crate) fn record_test_work(&self, dataset: &str, rows_produced: u64, mem_peak_bytes: u64) {
        let (_, ds) = self.series(Some(dataset));
        ds.rows_produced.fetch_add(rows_produced, Ordering::Relaxed);
        let i = MEMORY_BUCKETS
            .iter()
            .position(|m| mem_peak_bytes <= *m)
            .unwrap_or(MEMORY_BUCKETS.len());
        ds.mem_peak[i].fetch_add(1, Ordering::Relaxed);
        ds.mem_peak_sum.fetch_add(mem_peak_bytes, Ordering::Relaxed);
    }

    /// Carry a renamed dataset's series over to its new name.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn rename(&self, from: &str, to: &str) {
        self.commit_rename(from, to, None, 0);
    }

    pub fn active(&self, op: Op) -> i64 {
        self.active[op.index()].load(Ordering::Relaxed)
    }

    /// The spatial operators' work per dataset label: candidates, exact tests, matches.
    pub fn geo_work(&self) -> Vec<(String, crate::geo::Work)> {
        self.snapshot()
            .into_iter()
            .map(|(k, m)| (k, m.geo_work.each_ref().map(|c| c.load(Ordering::Relaxed))))
            .collect()
    }

    fn snapshot(&self) -> Vec<(String, Arc<DsMetrics>)> {
        self.datasets
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

// ------------------------------------------------------- write-time validation ------

/// `status` label values of `sparkles_validation_total`.
const VALIDATION_STATUS: [&str; 7] = [
    "passed", "warned", "rejected", "skipped", "bypassed", "timeout", "error",
];
/// `strategy` label values of `sparkles_validation_duration_seconds`.
const STRATEGIES: [&str; 2] = ["full", "incremental"];
/// `severity` label values of `sparkles_validation_results_total`.
const SEVERITIES: [&str; 3] = ["violation", "warning", "info"];
/// `reason` label values of `sparkles_validation_fallbacks_total` (why a write, or a
/// shape, was validated in full instead of incrementally).
const FALLBACKS: [&str; 7] = [
    "baseline",
    "shapes",
    "subclass",
    "sparql",
    "recursive",
    "bulk",
    "budget",
];

/// Write-time validation counters of one dataset, fed by its store's guard observer
/// (every write path: HTTP, MCP, the reasoner) and read when scraped, like the cache
/// counters. They live with the dataset, so a deleted dataset's series go with it.
pub struct ValidationMetrics {
    dataset: String,
    /// by [`sparkles::guard::GuardLanguage::index`]
    by_language: [LanguageCounters; 2],
}

/// The validation counters of one language.
#[derive(Default)]
struct LanguageCounters {
    status: [AtomicU64; 7],
    duration: [Histogram; 2],
    /// results found by validated writes, by severity (ShEx: nonconformant associations
    /// count as violations)
    results: [AtomicU64; 3],
    /// validations that fell back to full validation, by reason
    fallbacks: [AtomicU64; 7],
    /// validations by the focus nodes they validated, per strategy, by [`FOCUS_BUCKETS`]
    /// (not cumulative), and the sum of the focus nodes
    focus: [[AtomicU64; FOCUS_BUCKETS.len() + 1]; 2],
    focus_sum: [AtomicU64; 2],
}

/// Upper bounds of the `sparkles_validation_focus_nodes` histogram, without `+Inf`.
const FOCUS_BUCKETS: [u64; 7] = [1, 10, 100, 1_000, 10_000, 100_000, 1_000_000];

impl ValidationMetrics {
    pub fn new(dataset: &str) -> ValidationMetrics {
        ValidationMetrics {
            dataset: dataset.to_string(),
            by_language: Default::default(),
        }
    }

    fn seen(&self, language: sparkles::guard::GuardLanguage) -> bool {
        self.by_language[language.index()]
            .status
            .iter()
            .any(|c| c.load(Ordering::Relaxed) > 0)
    }
}

impl sparkles::guard::GuardObserver for ValidationMetrics {
    fn observe(
        &self,
        language: sparkles::guard::GuardLanguage,
        kind: sparkles::commit::CommitKind,
        outcome: std::result::Result<&sparkles::guard::ValidationSummary, &sparkles::Error>,
        elapsed: Duration,
    ) {
        use sparkles::guard::{GuardStatus, Strategy};
        let m = &self.by_language[language.index()];
        let status = match outcome {
            Ok(s) => match s.status {
                GuardStatus::Passed => 0,
                GuardStatus::Warned => 1,
                GuardStatus::Rejected => 2,
                GuardStatus::Skipped => 3,
                GuardStatus::Bypassed => 4,
            },
            Err(sparkles::Error::Timeout) => 5,
            // the client went away: not an outcome of the validation
            Err(sparkles::Error::Cancelled) => return,
            Err(_) => 6,
        };
        m.status[status].fetch_add(1, Ordering::Relaxed);
        let s = match outcome {
            Ok(s) => s,
            Err(_) => {
                // validation is in full; a timeout or failure still took its time
                m.duration[0].observe(elapsed);
                return;
            }
        };
        let strategy = match s.strategy {
            Strategy::Full => 0,
            Strategy::Incremental => 1,
            // skipped or bypassed: nothing was validated
            Strategy::None => return,
        };
        m.duration[strategy].observe(elapsed);
        if let Some(n) = s.focus_nodes {
            let i = FOCUS_BUCKETS
                .iter()
                .position(|b| n <= *b)
                .unwrap_or(FOCUS_BUCKETS.len());
            m.focus[strategy][i].fetch_add(1, Ordering::Relaxed);
            m.focus_sum[strategy].fetch_add(n, Ordering::Relaxed);
        }
        if let Some(i) = s
            .fallback
            .as_deref()
            .and_then(|f| FALLBACKS.iter().position(|r| *r == f))
        {
            m.fallbacks[i].fetch_add(1, Ordering::Relaxed);
        }
        let c = &s.by_severity;
        for (i, n) in [c.violation, c.warning, c.info].into_iter().enumerate() {
            m.results[i].fetch_add(n, Ordering::Relaxed);
        }
        if s.status == GuardStatus::Rejected {
            let (shape, focus_node) = first_result(s).unzip();
            tracing::info!(
                target: "sparkles::validation",
                dataset = %self.dataset,
                language = language.name(),
                kind = kind.name(),
                blocking = s.blocking,
                total = s.total,
                shape,
                focus_node,
                "write rejected by {} validation",
                language.title()
            );
        }
    }
}

/// The source shape and focus node of a summary's first (highest ranked) result, as
/// N-Triples-style terms (of a ShEx result: its shape and node; START as `START`).
pub fn first_result(s: &sparkles::guard::ValidationSummary) -> Option<(String, String)> {
    let r = s.results.first()?;
    let term = |t: &J| {
        let v = t["value"].as_str().unwrap_or("");
        match t["type"].as_str() {
            Some("uri") => format!("<{v}>"),
            Some("bnode") => format!("_:{v}"),
            Some("literal") => format!("\"{v}\""),
            _ => v.to_string(),
        }
    };
    if s.language == sparkles::guard::GuardLanguage::Shex {
        let shape = match r["shape"]["type"].as_str() {
            Some("start") => "START".to_string(),
            _ => term(&r["shape"]),
        };
        return Some((shape, term(&r["node"])));
    }
    Some((term(&r["sourceShape"]), term(&r["focusNode"])))
}

/// Validation counters of one dataset label (summed over datasets sharing `$other`).
#[derive(Default)]
struct ValidationTotals {
    status: [u64; 7],
    /// cumulative buckets and the sum in nanoseconds, per strategy
    duration: [([u64; 17], u64); 2],
    results: [u64; 3],
    fallbacks: [u64; 7],
    /// bucket counts (not cumulative) and the sum of focus nodes, per strategy
    focus: [([u64; FOCUS_BUCKETS.len() + 1], u64); 2],
}

/// Validation counters by dataset label and language, for the language of a dataset's
/// configuration and the languages with counts.
fn validation_totals(st: &AppState) -> BTreeMap<(String, &'static str), ValidationTotals> {
    let datasets: Vec<_> = st.datasets().values().cloned().collect();
    let mut out: BTreeMap<(String, &'static str), ValidationTotals> = BTreeMap::new();
    for d in datasets {
        let configured = d.validation.read().as_ref().map(|v| v.language());
        for language in sparkles::guard::GuardLanguage::ALL {
            let vm = &d.validation_metrics;
            if configured != Some(language) && !vm.seen(language) {
                continue;
            }
            let m = &vm.by_language[language.index()];
            let label = if st.metrics.enabled {
                st.metrics.series(Some(&d.name)).0
            } else {
                d.name.clone()
            };
            add_totals(out.entry((label, language.name())).or_default(), m);
        }
    }
    out
}

fn add_totals(t: &mut ValidationTotals, m: &LanguageCounters) {
    for (a, c) in t.status.iter_mut().zip(&m.status) {
        *a += c.load(Ordering::Relaxed);
    }
    for ((buckets, sum), h) in t.duration.iter_mut().zip(&m.duration) {
        for (a, n) in buckets.iter_mut().zip(h.cumulative()) {
            *a += n;
        }
        *sum += h.sum_nanos.load(Ordering::Relaxed);
    }
    for (a, c) in t.results.iter_mut().zip(&m.results) {
        *a += c.load(Ordering::Relaxed);
    }
    for (a, c) in t.fallbacks.iter_mut().zip(&m.fallbacks) {
        *a += c.load(Ordering::Relaxed);
    }
    for (((buckets, sum), h), s) in t.focus.iter_mut().zip(&m.focus).zip(&m.focus_sum) {
        for (a, c) in buckets.iter_mut().zip(h) {
            *a += c.load(Ordering::Relaxed);
        }
        *sum += s.load(Ordering::Relaxed);
    }
}

/// Scrape-time state of one dataset label (summed over datasets sharing `$other`).
#[derive(Default)]
struct DsGauges {
    /// server-local store instance id for a named dataset (0 for `$other`)
    instance_id: u64,
    quads: u64,
    delta_inserts: u64,
    delta_deletes: u64,
    wal_bytes: u64,
    disk_bytes: u64,
    /// storage quotas (0: unlimited)
    quota_bytes: u64,
    cache_bytes: u64,
    cache_capacity: u64,
    cache_entries: u64,
    cache_hits: u64,
    cache_misses: u64,
    rcache_enabled: bool,
    rcache_bytes: u64,
    rcache_capacity: u64,
    rcache_entries: u64,
    rcache_hits: u64,
    rcache_misses: u64,
    /// published rebuilds per [`RebuildReason::ALL`]
    rebuilds: [RebuildHistogram; 2],
    /// the commit path's synchronization, group and fallback counters
    commit: sparkles::store::CommitMetrics,
}

struct ActiveScrapeGuard<'a>(&'a Metrics);

impl Drop for ActiveScrapeGuard<'_> {
    fn drop(&mut self) {
        let mut ledger = self.0.other_store.lock();
        ledger.active_scrapes = ledger.active_scrapes.saturating_sub(1);
        if ledger.active_scrapes == 0 {
            ledger.retired_ids.clear();
            ledger.restored_during_scrape.clear();
            ledger.renamed_from.clear();
        }
    }
}

fn add_store_counters(g: &mut DsGauges, c: &StoreCounters) {
    g.cache_hits = g.cache_hits.saturating_add(c.cache_hits);
    g.cache_misses = g.cache_misses.saturating_add(c.cache_misses);
    g.rcache_hits = g.rcache_hits.saturating_add(c.rcache_hits);
    g.rcache_misses = g.rcache_misses.saturating_add(c.rcache_misses);
    for (h, eff) in g.rebuilds.iter_mut().zip(&c.rebuilds) {
        add_rebuild(h, eff);
    }
}

fn gauges(st: &AppState) -> BTreeMap<String, DsGauges> {
    let _scrape_guard = if st.metrics.enabled {
        st.metrics.other_store.lock().active_scrapes += 1;
        Some(ActiveScrapeGuard(&st.metrics))
    } else {
        None
    };
    let datasets: Vec<_> = st.datasets().values().cloned().collect();
    // the caches are shared, so one pass over each gives every dataset's bytes
    let (block_usage, result_usage) = match st.catalog.caches() {
        Some(c) => (Some(c.blocks().usage()), Some(c.results().usage())),
        None => (None, None),
    };
    struct LiveSample {
        name: String,
        open_id: u64,
        quads: u64,
        delta_inserts: u64,
        delta_deletes: u64,
        wal_bytes: u64,
        disk_bytes: u64,
        quota_bytes: u64,
        cache_bytes: u64,
        cache_capacity: u64,
        cache_entries: u64,
        rcache_enabled: bool,
        rcache_bytes: u64,
        rcache_capacity: u64,
        rcache_entries: u64,
        commit: sparkles::store::CommitMetrics,
        counters: StoreCounters,
    }
    let samples: Vec<LiveSample> = datasets
        .into_iter()
        .map(|d| {
            let snap = d.store.snapshot();
            let (c, r) = (d.store.cache(), d.store.result_cache());
            // measured at most once a second, like the quota checks of commits
            let quota = d.dataset.settings().quota().get();
            let cu = match &block_usage {
                Some(u) => u.get(&c.namespace()).copied().unwrap_or_default(),
                None => sparkles::index::NamespaceUsage {
                    bytes: c.bytes(),
                    entries: c.entries() as u64,
                },
            };
            let ru = match &result_usage {
                Some(u) => u.get(&r.namespace()).copied().unwrap_or_default(),
                None => sparkles::index::NamespaceUsage {
                    bytes: r.bytes(),
                    entries: r.entries() as u64,
                },
            };
            LiveSample {
                name: d.name.clone(),
                open_id: d.open_id,
                quads: snap.len(),
                delta_inserts: snap.delta.inserts() as u64,
                delta_deletes: snap.delta.deletes() as u64,
                wal_bytes: d.store.wal_bytes(),
                disk_bytes: quota.used_bytes,
                quota_bytes: quota.max_bytes.unwrap_or(0),
                cache_bytes: cu.bytes,
                cache_capacity: c.capacity(),
                cache_entries: cu.entries,
                rcache_enabled: r.enabled(),
                rcache_bytes: ru.bytes,
                rcache_capacity: r.capacity(),
                rcache_entries: ru.entries,
                commit: d.store.commit_metrics(),
                counters: StoreCounters::read(&d.store),
            }
        })
        .collect();
    #[cfg(all(test, feature = "otel"))]
    {
        let key = std::ptr::from_ref(st) as usize;
        let hook = GAUGES_AFTER_DATASETS_HOOKS.lock().remove(&key);
        if let Some(hook) = hook {
            hook();
        }
    }
    let mut out: BTreeMap<String, DsGauges> = BTreeMap::new();
    if !st.metrics.enabled {
        for s in samples {
            let g = out.entry(s.name).or_default();
            g.instance_id = s.open_id;
            g.quads += s.quads;
            g.delta_inserts += s.delta_inserts;
            g.delta_deletes += s.delta_deletes;
            g.wal_bytes += s.wal_bytes;
            g.disk_bytes += s.disk_bytes;
            g.quota_bytes += s.quota_bytes;
            g.cache_bytes += s.cache_bytes;
            g.cache_capacity = s.cache_capacity;
            g.cache_entries += s.cache_entries;
            g.rcache_enabled |= s.rcache_enabled;
            g.rcache_bytes += s.rcache_bytes;
            g.rcache_capacity = s.rcache_capacity;
            g.rcache_entries += s.rcache_entries;
            g.commit.merge(&s.commit);
            add_store_counters(g, &s.counters);
        }
        return out;
    }
    // Hold `other_store` across slot resolution, promotion retirement, and pending/retired
    // `$other` aggregation so concurrent scrapes never double-count or drop an overflow store.
    {
        let mut ledger_guard = st.metrics.other_store.lock();
        let ledger = &mut *ledger_guard;
        let mut live_ids = BTreeSet::new();
        for s in samples {
            if ledger.retired_ids.contains(&s.open_id)
                || ledger.pending.contains_key(&s.open_id)
                || (!ledger.renamed_from.is_empty()
                    && ledger.renamed_from.contains(&(s.name.clone(), s.open_id)))
            {
                continue;
            }
            if !live_ids.insert(s.open_id) {
                continue;
            }
            let label = st.metrics.series(Some(&s.name)).0;
            let is_other = label == OTHER;
            let effective = if is_other {
                let entry = ledger.stores.entry(s.open_id).or_default();
                let eff = s.counters.sub(&entry.promoted_offset);
                entry.seen_under_other = entry.seen_under_other.max_with(&eff);
                if let Some(restored) = ledger.restored_during_scrape.get(&s.open_id) {
                    entry.seen_under_other = entry.seen_under_other.max_with(restored);
                }
                entry.seen_under_other.clone_counters()
            } else {
                ledger.restored_during_scrape.remove(&s.open_id);
                if let Some(entry) = ledger.stores.get_mut(&s.open_id) {
                    if entry.seen_under_other.any() {
                        let retired = entry.seen_under_other.clone_counters();
                        ledger.retired.add_assign(&retired);
                        st.metrics.ensure_other_slot();
                        entry.promoted_offset.add_assign(&retired);
                        entry.seen_under_other = StoreCounters::default();
                    }
                    s.counters.sub(&entry.promoted_offset)
                } else {
                    s.counters
                }
            };
            let g = out.entry(label).or_default();
            if !is_other {
                g.instance_id = s.open_id;
            }
            g.quads += s.quads;
            g.delta_inserts += s.delta_inserts;
            g.delta_deletes += s.delta_deletes;
            g.wal_bytes += s.wal_bytes;
            g.disk_bytes += s.disk_bytes;
            g.quota_bytes += s.quota_bytes;
            g.cache_bytes += s.cache_bytes;
            g.cache_capacity = s.cache_capacity;
            g.cache_entries += s.cache_entries;
            g.rcache_enabled |= s.rcache_enabled;
            g.rcache_bytes += s.rcache_bytes;
            g.rcache_capacity = s.rcache_capacity;
            g.rcache_entries += s.rcache_entries;
            g.commit.merge(&s.commit);
            add_store_counters(g, &effective);
        }
        let mut carried = ledger.retired.clone_counters();
        for (&id, entry) in &ledger.pending {
            if let Some(c) = &entry.contrib {
                let st_entry = ledger.stores.entry(id).or_default();
                st_entry.seen_under_other = st_entry.seen_under_other.max_with(c);
                carried.add_assign(&st_entry.seen_under_other);
            }
        }
        for (&id, c) in &ledger.restored_during_scrape {
            if !live_ids.contains(&id)
                && !ledger.pending.contains_key(&id)
                && !ledger.retired_ids.contains(&id)
            {
                let st_entry = ledger.stores.entry(id).or_default();
                st_entry.seen_under_other = st_entry.seen_under_other.max_with(c);
                carried.add_assign(&st_entry.seen_under_other);
            }
        }
        if carried.any() || st.metrics.has_other_slot() {
            let g = out.entry(OTHER.to_string()).or_default();
            add_store_counters(g, &carried);
        }
    }
    out
}

/// The commit path's histograms and counters (spec X09 section 8).
fn render_commit_metrics(o: &mut String, gauges: &BTreeMap<String, DsGauges>) {
    use sparkles::store::{Fallback, SIZE_BUCKETS, SYNC_BUCKETS, SyncPath};
    let family = |o: &mut String, name: &str, kind: &str, help: &str| {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} {kind}");
    };
    fn histogram(
        o: &mut String,
        name: &str,
        labels: &str,
        bounds: &[f64],
        h: &sparkles::store::HistogramSnapshot,
    ) {
        let mut acc = 0;
        for (i, n) in h.buckets.iter().enumerate() {
            acc += n;
            let le = bounds.get(i).map_or("+Inf".to_string(), |b| b.to_string());
            let _ = writeln!(o, "{name}_bucket{{{labels},le=\"{le}\"}} {acc}");
        }
        let _ = writeln!(o, "{name}_sum{{{labels}}} {}", h.sum);
        let _ = writeln!(o, "{name}_count{{{labels}}} {acc}");
    }
    family(
        o,
        "sparkles_commit_sync_seconds",
        "histogram",
        "Time of each commit synchronization in seconds, by path: direct (write-through), buffered (fdatasync of one commit) or group (one fdatasync for the commits of a fence).",
    );
    for (ds, g) in gauges {
        let ds = escape_label(ds);
        for (h, path) in g.commit.sync.iter().zip(SyncPath::ALL) {
            if h.count() > 0 {
                let labels = format!("dataset=\"{ds}\",path=\"{}\"", path.as_str());
                histogram(o, "sparkles_commit_sync_seconds", &labels, &SYNC_BUCKETS, h);
            }
        }
    }
    family(
        o,
        "sparkles_group_commit_size",
        "histogram",
        "Commits per group commit fence.",
    );
    for (ds, g) in gauges {
        if g.commit.size.count() > 0 {
            let labels = format!("dataset=\"{}\"", escape_label(ds));
            histogram(
                o,
                "sparkles_group_commit_size",
                &labels,
                &SIZE_BUCKETS,
                &g.commit.size,
            );
        }
    }
    family(
        o,
        "sparkles_group_commit_wait_seconds",
        "histogram",
        "Time from a grouped commit joining the queue to its acknowledgement, in seconds.",
    );
    for (ds, g) in gauges {
        if g.commit.wait.count() > 0 {
            let labels = format!("dataset=\"{}\"", escape_label(ds));
            histogram(
                o,
                "sparkles_group_commit_wait_seconds",
                &labels,
                &SYNC_BUCKETS,
                &g.commit.wait,
            );
        }
    }
    family(
        o,
        "sparkles_group_commit_fallbacks_total",
        "counter",
        "Commits of a dataset with group commit on that took the ordinary path, by the first condition that kept them out of a group.",
    );
    for (ds, g) in gauges {
        let ds = escape_label(ds);
        for (n, why) in g.commit.fallbacks.iter().zip(Fallback::ALL) {
            if *n > 0 {
                let _ = writeln!(
                    o,
                    "sparkles_group_commit_fallbacks_total{{dataset=\"{ds}\",reason=\"{}\"}} {n}",
                    why.as_str()
                );
            }
        }
    }
    family(
        o,
        "sparkles_group_commit_failures_total",
        "counter",
        "Group commit fences whose synchronization failed.",
    );
    for (ds, g) in gauges {
        if g.commit.group_commit {
            let _ = writeln!(
                o,
                "sparkles_group_commit_failures_total{{dataset=\"{}\"}} {}",
                escape_label(ds),
                g.commit.failures
            );
        }
    }
}

/// Resident set size of this process (Linux: `VmRSS` of `/proc/self/status`).
pub fn resident_bytes() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

fn start_time_seconds(st: &AppState) -> f64 {
    (std::time::SystemTime::now() - st.started.elapsed())
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// The metrics of incremental views and live queries (spec F13 §8.2), read from each
/// open dataset's runtime.
fn views_metrics(st: &AppState, o: &mut String) {
    let family = |o: &mut String, name: &str, kind: &str, help: &str| {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} {kind}");
    };
    // the datasets open now: metrics never open one
    let open = st.open_datasets();
    let stats: Vec<(String, sparkles::incr::RuntimeStats)> = open
        .into_iter()
        .map(|(n, ds)| {
            let s = ds.views.stats(&ds.store);
            (n, s)
        })
        .collect();
    // datasets past the label cap share `$other`: their series are summed (lag: the most)
    let mut per_view: std::collections::BTreeMap<(String, String), (u64, u64)> = Default::default();
    let mut live: std::collections::BTreeMap<String, usize> = Default::default();
    for (n, s) in &stats {
        let label = st.metrics.dataset_label(Some(n));
        *live.entry(label.clone()).or_default() += s.live_queries;
        for v in &s.views {
            let e = per_view.entry((label.clone(), v.name.clone())).or_default();
            e.0 = e.0.max(v.lag_commits);
            e.1 += v.state_bytes;
        }
    }
    family(
        o,
        "sparkles_view_lag_commits",
        "gauge",
        "Commits a view is behind the head.",
    );
    for ((ds, view), (lag, _)) in &per_view {
        let _ = writeln!(
            o,
            "sparkles_view_lag_commits{{dataset=\"{}\",view=\"{}\"}} {lag}",
            escape_label(ds),
            escape_label(view)
        );
    }
    family(
        o,
        "sparkles_view_state_bytes",
        "gauge",
        "Approximate memory of a view's state.",
    );
    for ((ds, view), (_, bytes)) in &per_view {
        let _ = writeln!(
            o,
            "sparkles_view_state_bytes{{dataset=\"{}\",view=\"{}\"}} {bytes}",
            escape_label(ds),
            escape_label(view)
        );
    }
    family(o, "sparkles_live_queries", "gauge", "Live queries running.");
    for (ds, n) in &live {
        let _ = writeln!(
            o,
            "sparkles_live_queries{{dataset=\"{}\"}} {n}",
            escape_label(ds)
        );
    }
    let sum = |f: &dyn Fn(&sparkles::incr::RuntimeStats) -> f64| {
        stats.iter().map(|(_, s)| f(s)).sum::<f64>()
    };
    family(
        o,
        "sparkles_live_events_total",
        "counter",
        "Events sent to live queries.",
    );
    let _ = writeln!(
        o,
        "sparkles_live_events_total {}",
        sum(&|s| s.live_events as f64)
    );
    family(
        o,
        "sparkles_view_recompute_seconds_total",
        "counter",
        "Time spent evaluating recompute nodes of views and live queries.",
    );
    let _ = writeln!(
        o,
        "sparkles_view_recompute_seconds_total {:.6}",
        sum(&|s| s.recompute_seconds)
    );
    family(
        o,
        "sparkles_view_pass_seconds",
        "histogram",
        "Time of a maintenance pass.",
    );
    let mut cum = 0u64;
    for (i, le) in sparkles::incr::runtime::PASS_BUCKETS.iter().enumerate() {
        cum += stats
            .iter()
            .map(|(_, s)| s.pass_buckets.get(i).copied().unwrap_or(0))
            .sum::<u64>();
        let _ = writeln!(o, "sparkles_view_pass_seconds_bucket{{le=\"{le}\"}} {cum}");
    }
    let total: u64 = stats
        .iter()
        .map(|(_, s)| s.pass_buckets.iter().sum::<u64>())
        .sum();
    let _ = writeln!(
        o,
        "sparkles_view_pass_seconds_bucket{{le=\"+Inf\"}} {total}"
    );
    let _ = writeln!(
        o,
        "sparkles_view_pass_seconds_sum {:.6}",
        sum(&|s| s.pass_seconds_sum)
    );
    let _ = writeln!(o, "sparkles_view_pass_seconds_count {total}");
    family(
        o,
        "sparkles_view_passes_skipped_total",
        "counter",
        "Passes a view's footprint skipped.",
    );
    let _ = writeln!(
        o,
        "sparkles_view_passes_skipped_total {}",
        sum(&|s| s.skipped as f64)
    );
}

fn escape_label(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Prometheus text exposition format 0.0.4.
pub fn render_prometheus(st: &AppState) -> String {
    let mut o = String::with_capacity(16 << 10);
    let family = |o: &mut String, name: &str, kind: &str, help: &str| {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} {kind}");
    };
    let gauges = gauges(st);
    let series = st.metrics.snapshot();
    let seen = |m: &DsMetrics| -> Vec<Op> {
        Op::ALL
            .into_iter()
            .filter(|op| m.ops[op.index()].seen.load(Ordering::Relaxed))
            .collect()
    };

    family(&mut o, "sparkles_build_info", "gauge", "Sparkles version.");
    let _ = writeln!(
        o,
        "sparkles_build_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    );
    family(
        &mut o,
        "sparkles_start_time_seconds",
        "gauge",
        "Start time of the process since the Unix epoch in seconds.",
    );
    let _ = writeln!(
        o,
        "sparkles_start_time_seconds {:.3}",
        start_time_seconds(st)
    );
    family(
        &mut o,
        "sparkles_ready",
        "gauge",
        "Whether the server is ready to serve requests (1) or not (0).",
    );
    let _ = writeln!(o, "sparkles_ready {}", u8::from(server_status(st).1));
    family(
        &mut o,
        "sparkles_datasets",
        "gauge",
        "Declared datasets by state: open, opening, or failed to open.",
    );
    let states = dataset_states(st);
    for (state, n) in [
        ("open", states.0),
        ("opening", states.1),
        ("failed", states.2),
    ] {
        let _ = writeln!(o, "sparkles_datasets{{state=\"{state}\"}} {n}");
    }

    family(
        &mut o,
        "sparkles_requests_total",
        "counter",
        "Completed requests by dataset, operation and outcome.",
    );
    for (ds, m) in &series {
        let ds = escape_label(ds);
        for op in seen(m) {
            for oc in Outcome::ALL {
                let _ = writeln!(
                    o,
                    "sparkles_requests_total{{dataset=\"{ds}\",operation=\"{}\",outcome=\"{}\"}} {}",
                    op.as_str(),
                    oc.as_str(),
                    m.ops[op.index()].outcomes[oc.index()].load(Ordering::Relaxed)
                );
            }
        }
    }
    family(
        &mut o,
        "sparkles_request_duration_seconds",
        "histogram",
        "Request duration in seconds.",
    );
    for (ds, m) in &series {
        let ds = escape_label(ds);
        for op in seen(m) {
            let h = &m.ops[op.index()].duration;
            let cum = h.cumulative();
            for (le, n) in BUCKET_LABELS.iter().zip(cum) {
                let _ = writeln!(
                    o,
                    "sparkles_request_duration_seconds_bucket{{dataset=\"{ds}\",operation=\"{}\",le=\"{le}\"}} {n}",
                    op.as_str()
                );
            }
            let labels = format!("dataset=\"{ds}\",operation=\"{}\"", op.as_str());
            let _ = writeln!(
                o,
                "sparkles_request_duration_seconds_sum{{{labels}}} {}",
                h.sum_nanos.load(Ordering::Relaxed) as f64 / 1e9
            );
            let _ = writeln!(
                o,
                "sparkles_request_duration_seconds_count{{{labels}}} {}",
                cum[16]
            );
        }
    }
    family(
        &mut o,
        "sparkles_requests_active",
        "gauge",
        "Requests in progress by operation.",
    );
    for op in Op::ALL {
        let _ = writeln!(
            o,
            "sparkles_requests_active{{operation=\"{}\"}} {}",
            op.as_str(),
            st.metrics.active(op)
        );
    }
    family(
        &mut o,
        "sparkles_response_bytes_total",
        "counter",
        "Uncompressed bytes of response bodies whose size is known.",
    );
    for (ds, m) in &series {
        let ds = escape_label(ds);
        for op in seen(m) {
            let _ = writeln!(
                o,
                "sparkles_response_bytes_total{{dataset=\"{ds}\",operation=\"{}\"}} {}",
                op.as_str(),
                m.ops[op.index()].response_bytes.load(Ordering::Relaxed)
            );
        }
    }
    family(
        &mut o,
        "sparkles_graphql_groups",
        "histogram",
        "Engine executions (fetch groups) per GraphQL request.",
    );
    for (ds, m) in &series {
        let total: u64 = m
            .graphql_groups
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .sum();
        if total == 0 {
            continue;
        }
        let ds = escape_label(ds);
        let mut acc = 0;
        for (i, c) in m.graphql_groups.iter().enumerate() {
            acc += c.load(Ordering::Relaxed);
            let le = GROUP_BUCKETS
                .get(i)
                .map_or("+Inf".to_string(), |b| b.to_string());
            let _ = writeln!(
                o,
                "sparkles_graphql_groups_bucket{{dataset=\"{ds}\",le=\"{le}\"}} {acc}"
            );
        }
        let _ = writeln!(
            o,
            "sparkles_graphql_groups_sum{{dataset=\"{ds}\"}} {}",
            m.graphql_groups_sum.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            o,
            "sparkles_graphql_groups_count{{dataset=\"{ds}\"}} {total}"
        );
    }
    family(
        &mut o,
        "sparkles_result_rows_total",
        "counter",
        "Result rows of queries (triples for CONSTRUCT and DESCRIBE).",
    );
    for (ds, m) in &series {
        let _ = writeln!(
            o,
            "sparkles_result_rows_total{{dataset=\"{}\"}} {}",
            escape_label(ds),
            m.result_rows.load(Ordering::Relaxed)
        );
    }
    family(
        &mut o,
        "sparkles_rows_produced_total",
        "counter",
        "Rows produced by all operators of queries and updates (the rows-produced work).",
    );
    for (ds, m) in &series {
        if ds == NONE {
            continue;
        }
        let _ = writeln!(
            o,
            "sparkles_rows_produced_total{{dataset=\"{}\"}} {}",
            escape_label(ds),
            m.rows_produced.load(Ordering::Relaxed)
        );
    }
    family(
        &mut o,
        "sparkles_query_memory_peak_bytes",
        "histogram",
        "Estimated peak memory of queries in bytes.",
    );
    for (ds, m) in &series {
        if ds == NONE {
            continue;
        }
        let Some((cum, sum)) = m.mem_peak_snapshot() else {
            continue;
        };
        let ds = escape_label(ds);
        for (i, acc) in cum.iter().enumerate() {
            let le = MEMORY_BUCKETS
                .get(i)
                .map_or("+Inf".to_string(), |b| b.to_string());
            let _ = writeln!(
                o,
                "sparkles_query_memory_peak_bytes_bucket{{dataset=\"{ds}\",le=\"{le}\"}} {acc}"
            );
        }
        let _ = writeln!(
            o,
            "sparkles_query_memory_peak_bytes_sum{{dataset=\"{ds}\"}} {sum}"
        );
        let _ = writeln!(
            o,
            "sparkles_query_memory_peak_bytes_count{{dataset=\"{ds}\"}} {}",
            cum[MEMORY_BUCKETS.len()]
        );
    }
    family(
        &mut o,
        "sparkles_budget_exceeded_total",
        "counter",
        "Requests that exceeded a budget (rows, memory, result-bytes, decompressed-bytes, outbound-bytes, validation-work).",
    );
    for (ds, m) in &series {
        let ds = escape_label(ds);
        for k in BudgetKind::ALL {
            let _ = writeln!(
                o,
                "sparkles_budget_exceeded_total{{dataset=\"{ds}\",budget=\"{}\"}} {}",
                k.as_str(),
                m.budget[budget_index(k)].load(Ordering::Relaxed)
            );
        }
    }
    let throttle = crate::auth::throttle(st);
    if st.rate_limit.is_some() || throttle.is_some() {
        family(
            &mut o,
            "sparkles_rate_limited_total",
            "counter",
            "Requests refused by a rate or concurrency limit, by limit class.",
        );
        for (ds, m) in &series {
            let ds = escape_label(ds);
            for c in crate::ratelimit::Class::ALL {
                let _ = writeln!(
                    o,
                    "sparkles_rate_limited_total{{dataset=\"{ds}\",class=\"{}\"}} {}",
                    c.as_str(),
                    m.rate_limited[c.index()].load(Ordering::Relaxed)
                );
            }
        }
        let limiters: Vec<(&str, &crate::ratelimit::RateLimiter)> =
            [("requests", st.rate_limit.as_deref()), ("auth", throttle)]
                .into_iter()
                .filter_map(|(l, rl)| Some((l, rl?)))
                .collect();
        crate::ratelimit::render_metrics(&mut o, &limiters);
    }

    let validation = validation_totals(st);
    family(
        &mut o,
        "sparkles_validation_total",
        "counter",
        "Write-time validations by dataset, language and status.",
    );
    for ((ds, lang), t) in &validation {
        let ds = escape_label(ds);
        for (status, n) in VALIDATION_STATUS.iter().zip(t.status) {
            let _ = writeln!(
                o,
                "sparkles_validation_total{{dataset=\"{ds}\",language=\"{lang}\",status=\"{status}\"}} {n}"
            );
        }
    }
    family(
        &mut o,
        "sparkles_validation_duration_seconds",
        "histogram",
        "Duration of write-time validations in seconds.",
    );
    for ((ds, lang), t) in &validation {
        let ds = escape_label(ds);
        for (strategy, (cum, sum)) in STRATEGIES.iter().zip(&t.duration) {
            if cum[16] == 0 {
                continue;
            }
            for (le, n) in BUCKET_LABELS.iter().zip(cum) {
                let _ = writeln!(
                    o,
                    "sparkles_validation_duration_seconds_bucket{{dataset=\"{ds}\",language=\"{lang}\",strategy=\"{strategy}\",le=\"{le}\"}} {n}"
                );
            }
            let labels = format!("dataset=\"{ds}\",language=\"{lang}\",strategy=\"{strategy}\"");
            let _ = writeln!(
                o,
                "sparkles_validation_duration_seconds_sum{{{labels}}} {}",
                *sum as f64 / 1e9
            );
            let _ = writeln!(
                o,
                "sparkles_validation_duration_seconds_count{{{labels}}} {}",
                cum[16]
            );
        }
    }
    family(
        &mut o,
        "sparkles_validation_results_total",
        "counter",
        "Validation results found by validated writes, by severity (ShEx: nonconformant associations, as violations).",
    );
    for ((ds, lang), t) in &validation {
        let ds = escape_label(ds);
        for (severity, n) in SEVERITIES.iter().zip(t.results) {
            let _ = writeln!(
                o,
                "sparkles_validation_results_total{{dataset=\"{ds}\",language=\"{lang}\",severity=\"{severity}\"}} {n}"
            );
        }
    }
    family(
        &mut o,
        "sparkles_validation_focus_nodes",
        "histogram",
        "Focus nodes that each write-time validation validated, by strategy.",
    );
    for ((ds, lang), t) in &validation {
        let ds = escape_label(ds);
        for (strategy, (buckets, sum)) in STRATEGIES.iter().zip(&t.focus) {
            let total: u64 = buckets.iter().sum();
            if total == 0 {
                continue;
            }
            let labels = format!("dataset=\"{ds}\",language=\"{lang}\",strategy=\"{strategy}\"");
            let mut acc = 0;
            for (i, n) in buckets.iter().enumerate() {
                acc += n;
                let le = FOCUS_BUCKETS
                    .get(i)
                    .map_or("+Inf".to_string(), |b| b.to_string());
                let _ = writeln!(
                    o,
                    "sparkles_validation_focus_nodes_bucket{{{labels},le=\"{le}\"}} {acc}"
                );
            }
            let _ = writeln!(o, "sparkles_validation_focus_nodes_sum{{{labels}}} {sum}");
            let _ = writeln!(
                o,
                "sparkles_validation_focus_nodes_count{{{labels}}} {total}"
            );
        }
    }
    family(
        &mut o,
        "sparkles_validation_fallbacks_total",
        "counter",
        "Validated writes that ran a full validation instead of an incremental one (or validated some shapes in full), by reason.",
    );
    for ((ds, lang), t) in &validation {
        let ds = escape_label(ds);
        for (reason, n) in FALLBACKS.iter().zip(t.fallbacks) {
            let _ = writeln!(
                o,
                "sparkles_validation_fallbacks_total{{dataset=\"{ds}\",language=\"{lang}\",reason=\"{reason}\"}} {n}"
            );
        }
    }

    type Field = fn(&DsGauges) -> u64;
    let per_dataset: [(&str, &str, &str, Field); 10] = [
        (
            "sparkles_dataset_quads",
            "gauge",
            "Quads in the dataset.",
            |g| g.quads,
        ),
        (
            "sparkles_wal_bytes",
            "gauge",
            "Size of the write-ahead log.",
            |g| g.wal_bytes,
        ),
        (
            "sparkles_disk_bytes",
            "gauge",
            "Size of the database directory.",
            |g| g.disk_bytes,
        ),
        (
            "sparkles_dataset_quota_bytes",
            "gauge",
            "Storage quota of the database directory (0: unlimited).",
            |g| g.quota_bytes,
        ),
        (
            "sparkles_block_cache_bytes",
            "gauge",
            "Bytes held by the decoded-block cache.",
            |g| g.cache_bytes,
        ),
        (
            "sparkles_block_cache_hits_total",
            "counter",
            "Decoded-block cache hits.",
            |g| g.cache_hits,
        ),
        (
            "sparkles_block_cache_misses_total",
            "counter",
            "Decoded-block cache misses.",
            |g| g.cache_misses,
        ),
        (
            "sparkles_result_cache_bytes",
            "gauge",
            "Bytes held by the query result cache.",
            |g| g.rcache_bytes,
        ),
        (
            "sparkles_result_cache_hits_total",
            "counter",
            "Query result cache hits.",
            |g| g.rcache_hits,
        ),
        (
            "sparkles_result_cache_misses_total",
            "counter",
            "Query result cache misses.",
            |g| g.rcache_misses,
        ),
    ];
    // the caches are sized once for the server, so their capacities carry no label
    {
        let (block, result) = (
            gauges.values().map(|g| g.cache_capacity).max().unwrap_or(0),
            gauges
                .values()
                .map(|g| g.rcache_capacity)
                .max()
                .unwrap_or(0),
        );
        let (block, result) = st.catalog.caches().map_or((block, result), |c| {
            (c.blocks().capacity(), c.results().capacity())
        });
        family(
            &mut o,
            "sparkles_block_cache_capacity_bytes",
            "gauge",
            "Capacity of the decoded-block cache, shared by all datasets.",
        );
        let _ = writeln!(o, "sparkles_block_cache_capacity_bytes {block}");
        family(
            &mut o,
            "sparkles_result_cache_capacity_bytes",
            "gauge",
            "Capacity of the query result cache, shared by all datasets.",
        );
        let _ = writeln!(o, "sparkles_result_cache_capacity_bytes {result}");
    }
    crate::server_memory::render_prometheus(st, &mut o);
    for (name, kind, help, f) in per_dataset {
        family(&mut o, name, kind, help);
        for (ds, g) in &gauges {
            let _ = writeln!(o, "{name}{{dataset=\"{}\"}} {}", escape_label(ds), f(g));
        }
        if name == "sparkles_dataset_quads" {
            family(
                &mut o,
                "sparkles_delta_quads",
                "gauge",
                "Inserted and deleted quads not yet compacted into the index.",
            );
            for (ds, g) in &gauges {
                let ds = escape_label(ds);
                let _ = writeln!(
                    o,
                    "sparkles_delta_quads{{dataset=\"{ds}\",kind=\"insert\"}} {}",
                    g.delta_inserts
                );
                let _ = writeln!(
                    o,
                    "sparkles_delta_quads{{dataset=\"{ds}\",kind=\"delete\"}} {}",
                    g.delta_deletes
                );
            }
        }
    }
    family(
        &mut o,
        "sparkles_rebuilds_total",
        "counter",
        "New generations published since the dataset was opened, by reason: compact or bulk (a bulk commit).",
    );
    for (ds, g) in &gauges {
        let ds = escape_label(ds);
        for (h, reason) in g.rebuilds.iter().zip(RebuildReason::ALL) {
            let _ = writeln!(
                o,
                "sparkles_rebuilds_total{{dataset=\"{ds}\",reason=\"{}\"}} {}",
                reason.as_str(),
                h.count()
            );
        }
    }
    family(
        &mut o,
        "sparkles_rebuild_duration_seconds",
        "histogram",
        "Duration of the rebuilds that published a new generation, in seconds.",
    );
    for (ds, g) in &gauges {
        let ds = escape_label(ds);
        for (h, reason) in g.rebuilds.iter().zip(RebuildReason::ALL) {
            let Some((cum, sum)) = rebuild_cumulative(h) else {
                continue;
            };
            let labels = format!("dataset=\"{ds}\",reason=\"{}\"", reason.as_str());
            for (i, acc) in cum.iter().enumerate() {
                let le = REBUILD_BUCKETS
                    .get(i)
                    .map_or("+Inf".to_string(), |b| b.to_string());
                let _ = writeln!(
                    o,
                    "sparkles_rebuild_duration_seconds_bucket{{{labels},le=\"{le}\"}} {acc}"
                );
            }
            let _ = writeln!(o, "sparkles_rebuild_duration_seconds_sum{{{labels}}} {sum}");
            let _ = writeln!(
                o,
                "sparkles_rebuild_duration_seconds_count{{{labels}}} {}",
                cum[REBUILD_BUCKETS.len()]
            );
        }
    }
    render_commit_metrics(&mut o, &gauges);
    family(
        &mut o,
        "sparkles_result_cache_entries",
        "gauge",
        "Entries in the query result cache.",
    );
    for (ds, g) in &gauges {
        let _ = writeln!(
            o,
            "sparkles_result_cache_entries{{dataset=\"{}\"}} {}",
            escape_label(ds),
            g.rcache_entries
        );
    }
    if st.metrics.fuseki_names {
        fuseki::render(&mut o, st, &series);
    }
    crate::auth::render_metrics(st, &mut o);
    #[cfg(feature = "backup")]
    crate::backup::metrics::render(st, &mut o);
    crate::geo::metrics(st, &mut o);
    crate::compaction::metrics(st, &mut o);
    crate::vector::metrics(st, &mut o);
    crate::projection::metrics(st, &mut o);
    crate::http::history::metrics(st, &mut o);
    crate::http::branches::metrics(st, &mut o);
    crate::http::algo_view_metrics(&mut o);
    crate::assistant::metrics(st, &mut o);
    crate::notify::metrics(st, &mut o);
    #[cfg(feature = "mcp")]
    crate::mcp::branches::metrics(&mut o);
    #[cfg(feature = "mcp")]
    crate::mcp::memory::pending::metrics(st, &mut o);
    #[cfg(feature = "mcp")]
    crate::graphrag::http::metrics(st, &mut o);
    io_metrics(&mut o);
    views_metrics(st, &mut o);
    if let Some(rss) = resident_bytes() {
        family(
            &mut o,
            "process_resident_memory_bytes",
            "gauge",
            "Resident memory size in bytes.",
        );
        let _ = writeln!(o, "process_resident_memory_bytes {rss}");
    }
    o
}

/// What the read path asked of the kernel and decoded since the process started, as
/// Prometheus counters (spec X07 §4). They are process-wide: datasets share the readers.
fn io_metrics(o: &mut String) {
    use sparkles::index::Perm;
    use sparkles::ioacct::{IoFile, IoSnapshot};
    let s = IoSnapshot::now();
    fn family(o: &mut String, name: &str, help: &str) {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} counter");
    }
    family(
        o,
        "sparkles_index_columns_decoded_total",
        "Index block columns decoded, by permutation.",
    );
    for p in Perm::ALL {
        let _ = writeln!(
            o,
            "sparkles_index_columns_decoded_total{{permutation=\"{}\"}} {}",
            p.name(),
            s.cols_decoded[p.index()]
        );
    }
    family(
        o,
        "sparkles_index_readahead_bytes_total",
        "Bytes of index and vocabulary files requested from the kernel with WILLNEED, by file.",
    );
    for f in IoFile::ALL {
        let _ = writeln!(
            o,
            "sparkles_index_readahead_bytes_total{{file=\"{}\"}} {}",
            f.name(),
            s.willneed_bytes[f as usize]
        );
    }
    family(
        o,
        "sparkles_vocab_groups_read_total",
        "Vocabulary groups read ahead by a first lookup.",
    );
    let _ = writeln!(
        o,
        "sparkles_vocab_groups_read_total {}",
        s.vocab_groups_hinted
    );
}

/// Internal snapshot shared by `/$/metrics?format=json` and the OTLP observer.
/// `dataset_gens` carries `(series_id, instance_id)` per dataset label so OTLP can
/// detect same-cycle delete/recreate resets without changing the public JSON schema.
pub(crate) struct OtelMetricsSnapshot {
    pub(crate) json: J,
    #[cfg_attr(not(feature = "otel"), allow(dead_code))]
    pub(crate) dataset_gens: BTreeMap<String, (u64, u64)>,
}

/// The JSON snapshot of the registry (`/$/metrics?format=json`) for the UI.
pub fn metrics_json(st: &AppState) -> J {
    metrics_otel_snapshot(st).json
}

pub(crate) fn metrics_otel_snapshot(st: &AppState) -> OtelMetricsSnapshot {
    // Match `render_prometheus`: compute gauges first so live datasets register their
    // metric slots before `st.metrics.snapshot()` runs.
    let gauges = gauges(st);
    let mut requests = Vec::new();
    let mut counters = serde_json::Map::new();
    let mut series_ids: BTreeMap<String, u64> = BTreeMap::new();
    for (ds, m) in st.metrics.snapshot() {
        series_ids.insert(ds.clone(), m.series_id.0);
        for op in Op::ALL {
            let om = &m.ops[op.index()];
            if !om.seen.load(Ordering::Relaxed) {
                continue;
            }
            let outcomes: serde_json::Map<String, J> = Outcome::ALL
                .iter()
                .map(|oc| {
                    (
                        oc.as_str().to_string(),
                        om.outcomes[oc.index()].load(Ordering::Relaxed).into(),
                    )
                })
                .collect();
            let cum = om.duration.cumulative();
            requests.push(json!({
                "dataset": ds,
                "operation": op.as_str(),
                "outcomes": outcomes,
                "count": cum[16],
                "sumSeconds": om.duration.sum_nanos.load(Ordering::Relaxed) as f64 / 1e9,
                "buckets": cum.to_vec(),
                "responseBytes": om.response_bytes.load(Ordering::Relaxed),
            }));
        }
        let budget: serde_json::Map<String, J> = BudgetKind::ALL
            .iter()
            .map(|k| {
                (
                    k.as_str().to_string(),
                    m.budget[budget_index(*k)].load(Ordering::Relaxed).into(),
                )
            })
            .collect();
        let query_memory_peak = m.mem_peak_snapshot().map_or(J::Null, |(cum, sum)| {
            json!({
                "count": cum[MEMORY_BUCKETS.len()],
                "sumBytes": sum,
                "buckets": cum.to_vec(),
            })
        });
        counters.insert(
            ds,
            json!({
                "resultRows": m.result_rows.load(Ordering::Relaxed),
                "rowsProduced": m.rows_produced.load(Ordering::Relaxed),
                "queryMemoryPeak": query_memory_peak,
                "budgetExceeded": budget,
                "rateLimited": crate::ratelimit::Class::ALL
                    .iter()
                    .map(|c| {
                        let n = m.rate_limited[c.index()].load(Ordering::Relaxed);
                        (c.as_str().to_string(), J::from(n))
                    })
                    .collect::<serde_json::Map<_, _>>(),
            }),
        );
    }
    let geo = crate::geo::series(st);
    let mut dataset_gens = BTreeMap::new();
    let datasets: Vec<J> = gauges
        .into_iter()
        .map(|(ds, g)| {
            dataset_gens.insert(
                ds.clone(),
                (series_ids.get(&ds).copied().unwrap_or(0), g.instance_id),
            );
            let rebuilds: serde_json::Map<String, J> = RebuildReason::ALL
                .iter()
                .zip(&g.rebuilds)
                .map(|(reason, h)| {
                    let duration = rebuild_cumulative(h).map_or(J::Null, |(cum, sum)| {
                        json!({
                            "count": cum[REBUILD_BUCKETS.len()],
                            "sumSeconds": sum,
                            "buckets": cum.to_vec(),
                        })
                    });
                    (
                        reason.as_str().to_string(),
                        json!({
                            "count": h.count(),
                            "duration": duration,
                        }),
                    )
                })
                .collect();
            json!({
                "geo": crate::geo::series_json(geo.get(&ds)),
                "name": ds,
                "quads": g.quads,
                "deltaInserts": g.delta_inserts,
                "deltaDeletes": g.delta_deletes,
                "walBytes": g.wal_bytes,
                "groupCommit": g.commit.group_commit,
                "commits": {
                    "fences": g.commit.size.count(),
                    "failures": g.commit.failures,
                    "fallbacks": sparkles::store::Fallback::ALL
                        .iter()
                        .zip(g.commit.fallbacks)
                        .filter(|(_, n)| *n > 0)
                        .map(|(why, n)| (why.as_str().to_string(), J::from(n)))
                        .collect::<serde_json::Map<_, _>>(),
                },
                "diskBytes": g.disk_bytes,
                "quotaBytes": g.quota_bytes,
                "resultRows": counters.get(&ds).map_or(J::from(0), |c| c["resultRows"].clone()),
                "rowsProduced": counters.get(&ds).map_or(J::from(0), |c| c["rowsProduced"].clone()),
                "queryMemoryPeak": counters.get(&ds).map_or(J::Null, |c| c["queryMemoryPeak"].clone()),
                "rebuilds": rebuilds,
                "budgetExceeded": counters.get(&ds).map_or(J::Null, |c| c["budgetExceeded"].clone()),
                "rateLimited": counters.get(&ds).map_or(J::Null, |c| c["rateLimited"].clone()),
                "blockCache": {
                    "bytes": g.cache_bytes,
                    "capacityBytes": g.cache_capacity,
                    "entries": g.cache_entries,
                    "hits": g.cache_hits,
                    "misses": g.cache_misses,
                },
                "resultCache": {
                    "enabled": g.rcache_enabled,
                    "bytes": g.rcache_bytes,
                    "capacityBytes": g.rcache_capacity,
                    "entries": g.rcache_entries,
                    "hits": g.rcache_hits,
                    "misses": g.rcache_misses,
                },
            })
        })
        .collect();
    let active: serde_json::Map<String, J> = Op::ALL
        .iter()
        .map(|op| (op.as_str().to_string(), st.metrics.active(*op).into()))
        .collect();
    let (status, states) = (server_status(st), dataset_states(st));
    let json = json!({
        "formatVersion": 1,
        "version": env!("CARGO_PKG_VERSION"),
        "uptimeSeconds": st.started.elapsed().as_secs_f64(),
        "ready": status.1,
        "status": status.0,
        "datasetStates": {
            "open": states.0,
            "opening": states.1,
            "failed": states.2,
        },
        "processResidentBytes": resident_bytes(),
        "io": sparkles::ioacct::IoSnapshot::now().report(),
        "limits": st.limits.json(st.default_timeout),
        "bucketBounds": BUCKETS.to_vec(),
        "queryMemoryPeakBucketBounds": MEMORY_BUCKETS.to_vec(),
        "rebuildDurationBucketBounds": REBUILD_BUCKETS.to_vec(),
        "active": active,
        "requests": requests,
        "datasets": datasets,
        "memoryReview": memory_review(st),
    });
    OtelMetricsSnapshot { json, dataset_gens }
}

/// The open items of the agent memory review inboxes (`memoryReview`).
fn memory_review(st: &AppState) -> J {
    #[cfg(feature = "mcp")]
    return crate::mcp::memory::pending::series_json(st);
    #[cfg(not(feature = "mcp"))]
    {
        let _ = st;
        J::Array(Vec::new())
    }
}

/// `GET /$/metrics`: Prometheus text, or the JSON snapshot with `?format=json`.
pub async fn metrics_endpoint(State(st): State<Arc<AppState>>, uri: Uri) -> Response {
    if !st.metrics.enabled {
        return (StatusCode::NOT_FOUND, "metrics are disabled").into_response();
    }
    let json_format = uri.query().is_some_and(|q| {
        form_urlencoded::parse(q.as_bytes()).any(|(k, v)| k == "format" && v == "json")
    });
    let st2 = st.clone();
    let body = tokio::task::spawn_blocking(move || {
        if json_format {
            serde_json::to_vec(&metrics_json(&st2)).unwrap_or_default()
        } else {
            render_prometheus(&st2).into_bytes()
        }
    })
    .await
    .unwrap_or_default();
    let ct = if json_format {
        "application/json"
    } else {
        "text/plain; version=0.0.4; charset=utf-8"
    };
    (
        [
            (header::CONTENT_TYPE, ct),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

/// The router of `--metrics-addr`: `/$/metrics` alone, behind the same authentication,
/// `metrics` permission and `Host` check as on the main listener. Its requests are not
/// counted, as on the main listener.
pub fn metrics_router(st: Arc<AppState>) -> axum::Router {
    axum::Router::new()
        .route("/$/metrics", axum::routing::get(metrics_endpoint))
        .layer(axum::middleware::from_fn_with_state(
            st.clone(),
            crate::auth::middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            st.rate_limit.clone(),
            crate::ratelimit::admit,
        ))
        // a scrape keeps the server from counting as idle (see `alloc`)
        .layer(axum::middleware::from_fn(crate::alloc::track))
        .with_state(st)
}

// ------------------------------------------------------------------ readiness ------

/// Lifecycle phase of the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    /// the listener answers, and datasets are being opened
    Starting = 0,
    Ready = 1,
    /// shutting down: finishing requests in flight
    Draining = 2,
}

impl Phase {
    pub fn from_u8(v: u8) -> Phase {
        match v {
            1 => Phase::Ready,
            2 => Phase::Draining,
            _ => Phase::Starting,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Phase::Starting => "starting",
            Phase::Ready => "ready",
            Phase::Draining => "draining",
        }
    }
}

/// [`dataset_ready`] for `p`: without the sizes, which count every graph, when its
/// grants cover only some of the dataset's graphs.
fn dataset_ready_for(d: &crate::state::Dataset, p: &crate::auth::Principal) -> J {
    let mut v = dataset_ready(d);
    if p.restricted(&d.name)
        && let Some(m) = v.as_object_mut()
    {
        m.remove("walBytes");
        m.remove("deltaQuads");
    }
    v
}

fn dataset_ready(d: &crate::state::Dataset) -> J {
    let snap = d.store.snapshot();
    let mut v = json!({
        "name": d.name,
        "type": d.kind,
        // registered datasets are open: registration follows a successful open
        "state": "open",
        "ready": true,
        "walBytes": d.store.wal_bytes(),
        "deltaQuads": snap.delta.inserts() + snap.delta.deletes(),
    });
    if d.kind == DbType::Persistent {
        v["generation"] = snap.generation.name.clone().into();
    }
    v
}

/// The numbers of declared datasets that are open, opening and failed, each counted
/// once by the state a request sees: a dataset that startup still holds back is
/// opening, even once the catalog has it.
fn dataset_states(st: &AppState) -> (usize, usize, usize) {
    let unavailable = st.unavailable_datasets();
    let opening = unavailable
        .iter()
        .filter(|(_, _, u)| matches!(u, crate::state::Unavailable::Opening))
        .count();
    let failed = unavailable.len() - opening;
    let held = st.startup_pending.lock().clone();
    let open = st
        .catalog
        .list()
        .iter()
        .filter(|d| !held.contains_key(&d.name))
        .count();
    (open, opening, failed)
}

/// The server's readiness status and whether it is ready (spec C01 §4.1). A server whose
/// startup finished is `degraded` while a dataset failed to open, and is then not ready
/// unless `--ready-ignore-failed` is set.
pub fn server_status(st: &AppState) -> (&'static str, bool) {
    match st.phase() {
        Phase::Ready if st.catalog.refusals().is_empty() => ("ready", true),
        Phase::Ready => ("degraded", st.ready_ignore_failed),
        p => (p.as_str(), false),
    }
}

/// The `ReadyInfo` entry of a dataset that is opening or failed to open. Only a server
/// administrator reads the reason of a failure, which names paths.
fn unavailable_ready(
    name: &str,
    kind: DbType,
    u: &crate::state::Unavailable,
    p: &crate::auth::Principal,
) -> J {
    let mut v = json!({
        "name": name,
        "type": kind,
        "state": u.state(),
        "ready": false,
    });
    if let crate::state::Unavailable::Failed(r) = u {
        v["error"] = if p.has(crate::auth::ServerPerm::ServerAdmin) {
            r.message.clone().into()
        } else {
            crate::state::redacted_refusal(r.kind).into()
        };
        v["errorKind"] = r.kind.into();
    }
    v
}

/// Whether the server is ready, and its `ReadyInfo` document, listing the datasets the
/// caller may read (all of them with `metrics`), open or not.
fn ready_for(st: &AppState, p: &crate::auth::Principal) -> (bool, J) {
    let (status, ok) = server_status(st);
    let all = p.has(crate::auth::ServerPerm::Metrics);
    let mut datasets: Vec<(String, J)> = st
        .datasets()
        .values()
        .filter(|d| all || p.can(&d.name, crate::auth::Level::Read))
        .map(|d| (d.name.clone(), dataset_ready_for(d, p)))
        .collect();
    datasets.extend(
        st.unavailable_datasets()
            .into_iter()
            .filter(|(n, _, _)| all || p.can(n, crate::auth::Level::Read))
            .map(|(n, k, u)| {
                let v = unavailable_ready(&n, k, &u, p);
                (n, v)
            }),
    );
    datasets.sort_by(|a, b| a.0.cmp(&b.0));
    let datasets: Vec<J> = datasets.into_iter().map(|(_, v)| v).collect();
    (
        ok,
        json!({
            "status": status,
            "ready": ok,
            "uptimeSeconds": st.started.elapsed().as_secs(),
            "datasets": datasets,
        }),
    )
}

fn ready_response(ok: bool, body: J) -> Response {
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        axum::Json(body),
    )
        .into_response()
}

/// `GET /$/ready`: 200 when ready, else 503; the body is always `ReadyInfo`.
pub async fn ready_endpoint(
    State(st): State<Arc<AppState>>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> Response {
    let (ok, body) = tokio::task::spawn_blocking(move || ready_for(&st, &p))
        .await
        .unwrap_or((false, J::Null));
    ready_response(ok, body)
}

/// `GET /$/ready/{ds}`: readiness of the server and one dataset; 404 if unknown.
pub async fn ready_dataset(
    State(st): State<Arc<AppState>>,
    Path(name): Path<String>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
) -> Response {
    let (status, server_ok) = server_status(&st);
    let (ok, entry) = match st.lookup(&name) {
        crate::state::Lookup::Open(d) => (server_ok, dataset_ready_for(&d, &p)),
        found => match found {
            crate::state::Lookup::Unavailable(u) => {
                let kind = st
                    .startup_pending
                    .lock()
                    .get(&name)
                    .copied()
                    .unwrap_or(DbType::Persistent);
                (false, unavailable_ready(&name, kind, &u, &p))
            }
            _ => {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(json!({ "error": format!("no such dataset: /{name}") })),
                )
                    .into_response();
            }
        },
    };
    let body = json!({
        "status": status,
        "ready": ok,
        "uptimeSeconds": st.started.elapsed().as_secs(),
        "datasets": [entry],
    });
    ready_response(ok, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids() {
        assert!(valid_request_id(b"abc-123"));
        assert!(valid_request_id(b"a.b_c:d"));
        assert!(!valid_request_id(b""));
        assert!(!valid_request_id(b"has space"));
        assert!(!valid_request_id("é".as_bytes()));
        assert!(valid_request_id(&[b'a'; 128]));
        assert!(!valid_request_id(&[b'a'; 129]));
        let (a, b) = (new_request_id(), new_request_id());
        assert_ne!(a, b);
        assert!(valid_request_id(a.as_bytes()));
        assert_eq!(a.len(), 21);
        assert!(a < b, "{a} {b}");
    }

    #[test]
    fn histogram_buckets() {
        let h = Histogram::default();
        h.observe(Duration::from_micros(500));
        h.observe(Duration::from_millis(1));
        h.observe(Duration::from_millis(3));
        h.observe(Duration::from_secs(400));
        let c = h.cumulative();
        assert_eq!(c[0], 2); // le=0.001 is inclusive
        assert_eq!(c[2], 3);
        assert_eq!(c[15], 3);
        assert_eq!(c[16], 4);
    }

    #[test]
    fn dataset_labels_are_capped() {
        let m = Metrics::new(true, 2);
        assert_eq!(m.series(Some("a")).0, "a");
        assert_eq!(m.series(None).0, NONE);
        assert_eq!(m.series(Some("b")).0, "b");
        assert_eq!(m.series(Some("c")).0, OTHER);
        assert_eq!(m.series(Some("a")).0, "a");
        m.forget("a");
        assert_eq!(m.series(Some("c")).0, "c");
    }

    #[test]
    fn a_rename_carries_the_series_over() {
        let m = Metrics::new(true, 10);
        let a = m.series(Some("a")).1;
        m.rename("a", "b");
        assert!(Arc::ptr_eq(&a, &m.series(Some("b")).1));
        assert!(!Arc::ptr_eq(&a, &m.series(Some("a")).1));
    }
}
