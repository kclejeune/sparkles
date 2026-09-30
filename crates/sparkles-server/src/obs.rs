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
use parking_lot::RwLock;
use serde_json::{Value as J, json};
use sparkles::BudgetKind;
use sparkles::sparql::Timing;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing::Span;

pub static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Dataset label of requests that name no existing dataset.
const NONE: &str = "$none";
/// Dataset label shared by the datasets beyond the label cap.
const OTHER: &str = "$other";

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
    Shacl,
    Explain,
    Admin,
    Other,
}

impl Op {
    pub const ALL: [Op; 8] = [
        Op::Query,
        Op::Update,
        Op::Gsp,
        Op::Upload,
        Op::Shacl,
        Op::Explain,
        Op::Admin,
        Op::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Op::Query => "query",
            Op::Update => "update",
            Op::Gsp => "gsp",
            Op::Upload => "upload",
            Op::Shacl => "shacl",
            Op::Explain => "explain",
            Op::Admin => "admin",
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
    /// refused by a rate or concurrency limit (`429`, or `503` when saturated)
    RateLimited,
}

impl Outcome {
    pub const ALL: [Outcome; 7] = [
        Outcome::Ok,
        Outcome::ClientError,
        Outcome::Error,
        Outcome::Timeout,
        Outcome::Cancelled,
        Outcome::Budget,
        Outcome::RateLimited,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::ClientError => "client_error",
            Outcome::Error => "error",
            Outcome::Timeout => "timeout",
            Outcome::Cancelled => "cancelled",
            Outcome::Budget => "budget",
            Outcome::RateLimited => "rate_limited",
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
    /// the limit class that refused the request (outcome `rate_limited`)
    pub limit_class: Option<crate::ratelimit::Class>,
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
    if r.starts_with("/$/") {
        return Op::Admin;
    }
    match r {
        "/{ds}/sparql" | "/{ds}/query" => Op::Query,
        "/{ds}/update" => Op::Update,
        "/{ds}/data" | "/{ds}/get" => Op::Gsp,
        "/{ds}/upload" => Op::Upload,
        "/{ds}/shacl" => Op::Shacl,
        "/{ds}/explain" => Op::Explain,
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
            } else {
                Op::Gsp
            }
        }
        _ => Op::Other,
    }
}

/// The `{ds}` segment of the request path, if the route has one.
fn ds_param(route: Option<&str>, uri: &Uri) -> Option<String> {
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
    let span = tracing::info_span!(
        "request",
        request_id = %id,
        method = %req.method(),
        route = route.as_deref().unwrap_or("-"),
    );
    req.extensions_mut().insert(RequestSpan(span.clone()));
    let op = route_op(route.as_deref(), &req);
    let pending = Pending::new(
        st,
        span,
        op,
        ds_param(route.as_deref(), req.uri()),
        quiet(route.as_deref()),
    );
    let mut resp = next.run(req).await;
    let report = resp
        .extensions_mut()
        .remove::<RequestReport>()
        .unwrap_or_default();
    pending.complete(resp.status().as_u16(), &report);
    resp.headers_mut().insert(X_REQUEST_ID.clone(), id_value);
    resp
}

/// A request in flight: counted as active until dropped; dropped before
/// [`Pending::complete`] means the request was abandoned.
struct Pending {
    st: Arc<AppState>,
    span: Span,
    op: Op,
    dataset: Option<String>,
    quiet: bool,
    start: Instant,
    done: bool,
}

impl Pending {
    fn new(st: Arc<AppState>, span: Span, op: Op, dataset: Option<String>, quiet: bool) -> Self {
        if !quiet {
            st.metrics.active[op.index()].fetch_add(1, Ordering::Relaxed);
        }
        Pending {
            st,
            span,
            op,
            dataset,
            quiet,
            start: Instant::now(),
            done: false,
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
        }
        if self.st.access_log {
            access_event(
                &self.span,
                self.quiet,
                dataset.as_deref(),
                op,
                status,
                outcome,
                elapsed,
                report,
            );
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
) {
    let t = r.timing.as_ref();
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
    outcomes: [AtomicU64; 7],
    duration: Histogram,
    response_bytes: AtomicU64,
}

/// Counters of one dataset label.
#[derive(Default)]
pub struct DsMetrics {
    ops: [OpMetrics; 8],
    result_rows: AtomicU64,
    budget: [AtomicU64; 3],
    rate_limited: [AtomicU64; 4],
}

fn budget_index(k: BudgetKind) -> usize {
    match k {
        BudgetKind::Rows => 0,
        BudgetKind::Memory => 1,
        BudgetKind::ResultBytes => 2,
    }
}

pub struct Metrics {
    pub enabled: bool,
    max_datasets: usize,
    datasets: RwLock<BTreeMap<String, Arc<DsMetrics>>>,
    /// in-flight requests per operation of the matched route (kept even when metrics
    /// are off)
    active: [AtomicI64; 8],
}

impl Metrics {
    pub fn new(enabled: bool, max_datasets: usize) -> Metrics {
        Metrics {
            enabled,
            max_datasets,
            datasets: RwLock::new(BTreeMap::new()),
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

    /// Drop a deleted dataset's series (a recreated name starts again at zero).
    pub fn forget(&self, dataset: &str) {
        self.datasets.write().remove(dataset);
    }

    pub fn active(&self, op: Op) -> i64 {
        self.active[op.index()].load(Ordering::Relaxed)
    }

    fn snapshot(&self) -> Vec<(String, Arc<DsMetrics>)> {
        self.datasets
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// Scrape-time state of one dataset label (summed over datasets sharing `$other`).
#[derive(Default)]
struct DsGauges {
    quads: u64,
    delta_inserts: u64,
    delta_deletes: u64,
    wal_bytes: u64,
    disk_bytes: u64,
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
}

fn gauges(st: &AppState) -> BTreeMap<String, DsGauges> {
    let datasets: Vec<_> = st.datasets.read().values().cloned().collect();
    let mut out: BTreeMap<String, DsGauges> = BTreeMap::new();
    for d in datasets {
        let label = if st.metrics.enabled {
            st.metrics.series(Some(&d.name)).0
        } else {
            d.name.clone()
        };
        let g = out.entry(label).or_default();
        let snap = d.store.snapshot();
        let opts = d.store.options();
        let (c, r) = (d.store.cache(), d.store.result_cache());
        g.quads += snap.len();
        g.delta_inserts += snap.delta.inserts() as u64;
        g.delta_deletes += snap.delta.deletes() as u64;
        g.wal_bytes += d.store.wal_bytes();
        g.disk_bytes += d.store.disk_bytes();
        g.cache_bytes += c.bytes();
        g.cache_capacity += opts.cache_bytes;
        g.cache_entries += c.entries() as u64;
        g.cache_hits += c.hits();
        g.cache_misses += c.misses();
        g.rcache_enabled |= r.enabled();
        g.rcache_bytes += r.bytes();
        g.rcache_capacity += if r.enabled() {
            opts.result_cache_bytes
        } else {
            0
        };
        g.rcache_entries += r.entries() as u64;
        g.rcache_hits += r.hits();
        g.rcache_misses += r.misses();
    }
    out
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
    let _ = writeln!(o, "sparkles_ready {}", u8::from(ready(st).0));

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
        "sparkles_budget_exceeded_total",
        "counter",
        "Requests that exceeded a budget (rows, memory, result-bytes).",
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
    if st.rate_limit.is_some() {
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
    }

    type Field = fn(&DsGauges) -> u64;
    let per_dataset: [(&str, &str, &str, Field); 11] = [
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
            "sparkles_block_cache_bytes",
            "gauge",
            "Bytes held by the decoded-block cache.",
            |g| g.cache_bytes,
        ),
        (
            "sparkles_block_cache_capacity_bytes",
            "gauge",
            "Capacity of the decoded-block cache.",
            |g| g.cache_capacity,
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
            "sparkles_result_cache_capacity_bytes",
            "gauge",
            "Capacity of the query result cache.",
            |g| g.rcache_capacity,
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

/// The JSON snapshot of the registry (`/$/metrics?format=json`) for the UI.
pub fn metrics_json(st: &AppState) -> J {
    let mut requests = Vec::new();
    let mut counters = serde_json::Map::new();
    for (ds, m) in st.metrics.snapshot() {
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
        counters.insert(
            ds,
            json!({
                "resultRows": m.result_rows.load(Ordering::Relaxed),
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
    let datasets: Vec<J> = gauges(st)
        .into_iter()
        .map(|(ds, g)| {
            json!({
                "name": ds,
                "quads": g.quads,
                "deltaInserts": g.delta_inserts,
                "deltaDeletes": g.delta_deletes,
                "walBytes": g.wal_bytes,
                "diskBytes": g.disk_bytes,
                "resultRows": counters.get(&ds).map_or(J::from(0), |c| c["resultRows"].clone()),
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
    json!({
        "formatVersion": 1,
        "version": env!("CARGO_PKG_VERSION"),
        "uptimeSeconds": st.started.elapsed().as_secs_f64(),
        "ready": ready(st).0,
        "processResidentBytes": resident_bytes(),
        "limits": st.limits.json(st.default_timeout),
        "bucketBounds": BUCKETS.to_vec(),
        "active": active,
        "requests": requests,
        "datasets": datasets,
    })
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

// ------------------------------------------------------------------ readiness ------

/// Lifecycle phase of the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    /// datasets are being opened; not serving yet
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

/// Whether the server is ready, and its `ReadyInfo` document.
fn ready(st: &AppState) -> (bool, J) {
    let phase = st.phase();
    let datasets: Vec<J> = st
        .datasets
        .read()
        .values()
        .map(|d| dataset_ready(d))
        .collect();
    let ok = phase == Phase::Ready;
    (
        ok,
        json!({
            "status": phase.as_str(),
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
pub async fn ready_endpoint(State(st): State<Arc<AppState>>) -> Response {
    let (ok, body) = tokio::task::spawn_blocking(move || ready(&st))
        .await
        .unwrap_or((false, J::Null));
    ready_response(ok, body)
}

/// `GET /$/ready/{ds}`: readiness of the server and one dataset; 404 if unknown.
pub async fn ready_dataset(State(st): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    let Some(d) = st.get(&name) else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({ "error": format!("no such dataset: /{name}") })),
        )
            .into_response();
    };
    let phase = st.phase();
    let ok = phase == Phase::Ready;
    let body = json!({
        "status": phase.as_str(),
        "ready": ok,
        "uptimeSeconds": st.started.elapsed().as_secs(),
        "datasets": [dataset_ready(&d)],
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
}
