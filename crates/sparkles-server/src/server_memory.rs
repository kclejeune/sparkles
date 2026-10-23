//! Server-wide memory settings: the memory profiles, the shared cache sizes and the
//! query memory pool with its admission queue (spec X08 §2 to §4).

use crate::Cli;
use crate::ratelimit::{Class, classify};
use crate::state::AppState;
use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::response::Response;
use serde_json::{Value as J, json};
use sparkles::sparql::pool::{INITIAL_GRANT, PoolGrant};
use sparkles::store::caches::CacheSizes;
use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A set of defaults for the memory flags. A flag given on the command line overrides
/// the profile, and the profile overrides the built-in default.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Profile {
    #[default]
    Default,
    Small,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Profile::Default => "default",
            Profile::Small => "small",
        }
    }

    /// The profile's cache sizes.
    pub fn caches(self) -> CacheSizes {
        match self {
            Profile::Default => CacheSizes::default(),
            Profile::Small => CacheSizes::small(),
        }
    }

    /// The profile's budget for one query, in MiB.
    pub fn query_memory_mb(self) -> u64 {
        match self {
            Profile::Default => 8192,
            Profile::Small => 1024,
        }
    }

    /// The profile's query memory pool in MiB, or `None` when the pool is sized from the
    /// memory of the host.
    pub fn total_query_memory_mb(self) -> Option<u64> {
        match self {
            Profile::Default => None,
            Profile::Small => Some(2048),
        }
    }
}

/// The cache sizes: each flag, else the profile's value.
pub fn cache_sizes(cli: &Cli) -> CacheSizes {
    let p = cli.memory_profile.caches();
    let mib = |flag: Option<u64>, profile: u64| flag.map_or(profile, |m| m << 20);
    CacheSizes {
        blocks: mib(cli.cache_mb, p.blocks),
        results: mib(cli.result_cache_mb, p.results),
        service: mib(cli.service_cache_mb, p.service),
        history: mib(cli.history_cache_mb, p.history),
    }
}

/// The smallest pool the host-derived default produces.
const MIN_POOL: u64 = 256 << 20;

/// The memory the process may use: the cgroup v2 limit of its cgroup when one is set,
/// otherwise the physical memory. `None` when neither is known (not Linux).
pub fn available_memory() -> Option<u64> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("0::").map(str::to_string))
        })
        .unwrap_or_default();
    let mut limit = None;
    // the process's own cgroup and its ancestors, nearest first
    let mut path = cgroup.trim().trim_end_matches('/').to_string();
    loop {
        let file = format!("/sys/fs/cgroup{path}/memory.max");
        if let Ok(v) = std::fs::read_to_string(&file)
            && let Ok(n) = v.trim().parse::<u64>()
        {
            limit = Some(limit.map_or(n, |l: u64| l.min(n)));
        }
        match path.rfind('/') {
            Some(i) => path.truncate(i),
            None => break,
        }
    }
    let physical = std::fs::read_to_string("/proc/meminfo").ok().and_then(|s| {
        s.lines()
            .find_map(|l| l.strip_prefix("MemTotal:"))
            .and_then(|r| r.split_whitespace().next()?.parse::<u64>().ok())
            .map(|kb| kb << 10)
    });
    match (limit, physical) {
        (Some(l), Some(p)) => Some(l.min(p)),
        (l, p) => l.or(p),
    }
}

/// The pool's capacity in bytes, or `None` for no pool. `flag` is
/// `--total-query-memory-mb` (0 turns the pool off), `profile` the memory profile and
/// `caches` the configured caches, which the default leaves out.
pub fn pool_capacity(flag: Option<u64>, profile: Profile, caches: &CacheSizes) -> Option<u64> {
    if let Some(mb) = flag.or(profile.total_query_memory_mb()) {
        return (mb > 0).then_some(mb << 20);
    }
    let Some(avail) = available_memory() else {
        tracing::warn!(
            "the memory available to the process is unknown, so the query memory pool is off; set --total-query-memory-mb"
        );
        return None;
    };
    let caches = caches.blocks + caches.results + caches.service + caches.history;
    let half = avail.saturating_sub(caches) / 2;
    if half < MIN_POOL {
        tracing::warn!(
            "the query memory pool defaults to {} MiB, because the host has {} MiB for the process and the caches take {} MiB",
            MIN_POOL >> 20,
            avail >> 20,
            caches >> 20
        );
        return Some(MIN_POOL);
    }
    Some(half)
}

// ----------------------------------------------------------------- admission ------

/// Upper bounds in seconds of the admission wait histogram.
pub const WAIT_BOUNDS: [f64; 9] = [0.001, 0.005, 0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0];

/// The server's admission settings and counters.
pub struct Admission {
    pub profile: Profile,
    /// how long a request waits for memory before `503`
    pub wait: Duration,
    buckets: [AtomicU64; WAIT_BOUNDS.len() + 1],
    sum_micros: AtomicU64,
}

impl Default for Admission {
    fn default() -> Self {
        Admission {
            profile: Profile::Default,
            wait: Duration::from_secs(2),
            buckets: Default::default(),
            sum_micros: AtomicU64::new(0),
        }
    }
}

impl Admission {
    fn observe(&self, waited: Duration) {
        let s = waited.as_secs_f64();
        let i = WAIT_BOUNDS
            .iter()
            .position(|b| s <= *b)
            .unwrap_or(WAIT_BOUNDS.len());
        self.buckets[i].fetch_add(1, Ordering::Relaxed);
        self.sum_micros
            .fetch_add(waited.as_micros() as u64, Ordering::Relaxed);
    }

    /// Cumulative counts per bound and the sum in seconds.
    fn snapshot(&self) -> (Vec<u64>, f64) {
        let mut acc = 0;
        let counts = self
            .buckets
            .iter()
            .map(|b| {
                acc += b.load(Ordering::Relaxed);
                acc
            })
            .collect();
        (counts, self.sum_micros.load(Ordering::Relaxed) as f64 / 1e6)
    }
}

tokio::task_local! {
    /// The grant of the request whose handler runs in this task.
    static GRANT: Arc<PoolGrant>;
}

/// The grant the current request was admitted with, for the queries it runs.
pub fn current_grant() -> Option<Arc<PoolGrant>> {
    GRANT.try_with(Arc::clone).ok()
}

/// Admission to the query memory pool (inside the rate limiter, so the per-class
/// concurrency caps answer first). Query and update requests wait here, in arrival
/// order, for an initial grant. They answer `503` with `Retry-After` when the wait
/// passes `--query-admission-wait`. Other requests run without a grant.
///
/// A request with a body is admitted once its body is read, up to the body limit of its
/// class, so that a slow client holds no pool memory while it sends
/// (X08-P1-admit-after-body). Uploads and Graph Store writes stream bodies of any size
/// and charge none of the load to the pool, so they start with an empty grant and do not
/// queue.
pub async fn admit(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let route = req.extensions().get::<MatchedPath>().map(|m| m.as_str());
    let class = classify(route, req.method(), req.uri(), req.headers());
    let limit = match class {
        Some(Class::Query) => st.limits.max_query_body_bytes,
        Some(Class::Update) => st.limits.max_update_body_bytes,
        _ => return next.run(req).await,
    };
    if data_write(route, req.method(), req.uri(), req.headers()) {
        return match st.catalog.pool() {
            Some(pool) => {
                GRANT
                    .scope(Arc::new(pool.empty_grant()), next.run(req))
                    .await
            }
            None => next.run(req).await,
        };
    }
    let req = if has_body(&req) && st.catalog.pool().is_some() {
        read_ahead(req, limit.unwrap_or(BODY_AHEAD).min(BODY_AHEAD)).await
    } else {
        req
    };
    granted(&st, req, next).await
}

/// The most of a request body read before admission. A larger body is admitted after
/// this much and streams the rest to its handler, which applies its own limit.
const BODY_AHEAD: u64 = 256 << 20;

/// Whether a request is an upload or a Graph Store write, whose body streams to the
/// store and may be of any size: `/{ds}/upload`, a patch, a write to `/{ds}/data` or a
/// graph, and a request to `/{ds}` that is not a SPARQL query, update or form.
fn data_write(
    route: Option<&str>,
    method: &axum::http::Method,
    uri: &axum::http::Uri,
    headers: &axum::http::HeaderMap,
) -> bool {
    use axum::http::{Method, header};
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return false;
    }
    match route {
        Some("/{ds}/upload" | "/{ds}/patch" | "/{ds}/data" | "/{ds}/{*graph}") => true,
        Some("/{ds}") => {
            let has = |k: &str| {
                form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()).any(|(a, _)| a == k)
            };
            let ct = headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let form = *method == Method::POST && ct == "application/x-www-form-urlencoded";
            let query = has("query") || ct == "application/sparql-query";
            let update = has("update") || ct == "application/sparql-update";
            !(form || query || update)
        }
        _ => false,
    }
}

/// Whether a request may carry a body: it declares a length above zero or a chunked one.
fn has_body(req: &Request) -> bool {
    use http_body::Body as _;
    let hint = req.body().size_hint();
    hint.upper() != Some(0)
}

/// Read up to `limit` bytes of the request's body, and give it back with the bytes read
/// in front of the rest. A body that ends within `limit` is handed on whole. A read error
/// is handed on after the bytes before it, so the handler answers it as before.
async fn read_ahead(req: Request, limit: u64) -> Request {
    use http_body::Body as _;
    use http_body_util::BodyExt;
    let (parts, mut body) = req.into_parts();
    // a declared length above the limit is refused by the handler without reading
    if body.size_hint().lower() > limit {
        return Request::from_parts(parts, body);
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut error = None;
    loop {
        match body.frame().await {
            None => {
                return Request::from_parts(parts, axum::body::Body::from(buf));
            }
            Some(Ok(frame)) => {
                // trailers carry nothing a handler reads
                if let Ok(data) = frame.into_data() {
                    buf.extend_from_slice(&data);
                }
                if buf.len() as u64 >= limit {
                    break;
                }
            }
            Some(Err(e)) => {
                error = Some(e);
                break;
            }
        }
    }
    let body = AheadBody {
        ahead: Some(axum::body::Bytes::from(buf)).filter(|b| !b.is_empty()),
        rest: if error.is_some() { None } else { Some(body) },
        error,
    };
    Request::from_parts(parts, axum::body::Body::new(body))
}

/// A request body whose first bytes were read before admission: those bytes, then the
/// read error that stopped the read or the rest of the body.
struct AheadBody {
    ahead: Option<axum::body::Bytes>,
    error: Option<axum::Error>,
    rest: Option<axum::body::Body>,
}

impl http_body::Body for AheadBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        use std::task::Poll;
        if let Some(b) = self.ahead.take() {
            return Poll::Ready(Some(Ok(http_body::Frame::data(b))));
        }
        if let Some(e) = self.error.take() {
            return Poll::Ready(Some(Err(e)));
        }
        match self.rest.as_mut() {
            Some(rest) => std::pin::Pin::new(rest).poll_frame(cx),
            None => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.ahead.is_none()
            && self.error.is_none()
            && self.rest.as_ref().is_none_or(|r| r.is_end_stream())
    }

    fn size_hint(&self) -> http_body::SizeHint {
        let ahead = self.ahead.as_ref().map_or(0, |b| b.len() as u64);
        let rest = self
            .rest
            .as_ref()
            .map_or_else(|| http_body::SizeHint::with_exact(0), |r| r.size_hint());
        let mut hint = http_body::SizeHint::new();
        hint.set_lower(rest.lower().saturating_add(ahead));
        if let Some(u) = rest.upper() {
            hint.set_upper(u.saturating_add(ahead));
        }
        hint
    }
}

/// The initial grant a request asks the pool for: a request whose own budget is below
/// the usual initial grant takes only that much.
pub fn initial_grant(st: &AppState, pool: &sparkles::sparql::pool::QueryPool) -> u64 {
    INITIAL_GRANT
        .min(st.limits.query_memory_bytes.unwrap_or(u64::MAX))
        .min(pool.capacity())
}

/// Run `next` with an initial grant of the query memory pool, waiting in arrival order
/// up to `--query-admission-wait`, as [`admit`] does for the requests it classifies.
/// The algorithm route selects its class from its body, so it calls this itself.
pub async fn granted(st: &AppState, req: Request, next: Next) -> Response {
    match acquire(st).await {
        Ok(Some(grant)) => GRANT.scope(Arc::new(grant), next.run(req)).await,
        Ok(None) => next.run(req).await,
        Err(rejected) => rejected,
    }
}

/// An initial grant of the query memory pool, waiting in arrival order up to
/// `--query-admission-wait`. `None` without a pool, and the `503` response with
/// `Retry-After` when the wait passes. MCP calls, which name their class in their body,
/// take their grant here and hand it to their tool through the request.
pub async fn acquire(st: &AppState) -> Result<Option<PoolGrant>, Response> {
    let Some(pool) = st.catalog.pool() else {
        return Ok(None);
    };
    let want = initial_grant(st, pool);
    let started = Instant::now();
    let grant = match pool.try_grant(want) {
        Some(g) => g,
        None => {
            let notify = Arc::new(tokio::sync::Notify::new());
            let n = notify.clone();
            let mut ticket = pool.enter(want, Some(Arc::new(move || n.notify_one())));
            let end = tokio::time::Instant::now() + st.admission.wait;
            loop {
                if let Some(g) = ticket.poll() {
                    break g;
                }
                tokio::select! {
                    _ = notify.notified() => {}
                    _ = tokio::time::sleep_until(end) => {
                        // one last look, since the wake-up may have raced the timer
                        if let Some(g) = ticket.poll() {
                            break g;
                        }
                        pool.note_rejected();
                        return Err(crate::http::ApiError::from(pool.exhausted(want)).into_response());
                    }
                }
            }
        }
    };
    st.admission.observe(started.elapsed());
    Ok(Some(grant))
}

// ------------------------------------------------------------------- reports ------

/// `GET /$/server/memory`: the pool, the caches and their bytes per dataset.
pub fn report(st: &AppState) -> J {
    let pool = match st.catalog.pool() {
        Some(p) => {
            let s = p.stats();
            json!({
                "enabled": true,
                "capacityBytes": s.capacity,
                "grantedBytes": s.granted,
                "waiting": s.waiting,
                "admitted": s.admitted,
                "rejected": s.rejected,
                "admissionWaitSeconds": st.admission.wait.as_secs_f64(),
            })
        }
        None => json!({ "enabled": false }),
    };
    let mut caches = json!({});
    let mut datasets = Vec::new();
    if let Some(c) = st.catalog.caches() {
        let blocks = c.blocks().usage();
        let results = c.results().usage();
        let service = c.results().service().usage();
        caches = json!({
            "shared": true,
            "block": {
                "capacityBytes": c.blocks().capacity(),
                "bytes": c.blocks().bytes(),
                "entries": c.blocks().entries(),
            },
            "result": {
                "capacityBytes": c.results().capacity(),
                "bytes": c.results().bytes(),
                "entries": c.results().entries(),
            },
            "service": {
                "capacityBytes": c.results().service().capacity(),
                "bytes": c.results().service().bytes(),
                "entries": c.results().service().entries(),
            },
            "history": {
                "capacityBytes": c.history().capacity(),
                "bytes": c.history().used(),
            },
        });
        for (name, d) in st.datasets().iter() {
            let (b, r, s) = (
                blocks
                    .get(&d.store.cache().namespace())
                    .copied()
                    .unwrap_or_default(),
                results
                    .get(&d.store.result_cache().namespace())
                    .copied()
                    .unwrap_or_default(),
                service
                    .get(&d.store.result_cache().namespace())
                    .copied()
                    .unwrap_or_default(),
            );
            datasets.push(json!({
                "name": name,
                "blockBytes": b.bytes,
                "blockEntries": b.entries,
                "resultBytes": r.bytes,
                "resultEntries": r.entries,
                "serviceBytes": s.bytes,
                "serviceEntries": s.entries,
            }));
        }
    }
    let spill = match &st.spill {
        Some(s) => json!({
            "enabled": true,
            "capacityBytes": s.capacity(),
            "queryBytes": st.limits.query_spill_bytes,
            "reservedBytes": s.reserved_bytes(),
            "writtenBytes": s.written_bytes(),
            "spilledQueries": s.spilled_queries(),
        }),
        None => json!({ "enabled": false }),
    };
    json!({
        "profile": st.admission.profile.name(),
        "queryMemoryBytes": st.limits.query_memory_bytes,
        "pool": pool,
        "spill": spill,
        "caches": caches,
        "datasets": datasets,
    })
}

/// The spill directory's series for `/$/metrics`.
fn render_spill(st: &AppState, o: &mut String) {
    let Some(s) = &st.spill else {
        return;
    };
    let mut family = |name: &str, kind: &str, help: &str, value: u64| {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} {kind}");
        let _ = writeln!(o, "{name} {value}");
    };
    family(
        "sparkles_spill_capacity_bytes",
        "gauge",
        "Bytes of disk all queries' spill files may reserve together.",
        s.capacity(),
    );
    family(
        "sparkles_spill_reserved_bytes",
        "gauge",
        "Bytes of disk running queries have reserved for spill files.",
        s.reserved_bytes(),
    );
    family(
        "sparkles_spill_written_bytes_total",
        "counter",
        "Bytes written to spill files.",
        s.written_bytes(),
    );
    family(
        "sparkles_spill_queries_total",
        "counter",
        "Queries that wrote a spill file.",
        s.spilled_queries(),
    );
    family(
        "sparkles_spill_eager_retries_total",
        "counter",
        "Eager queries that exceeded their memory budget and ran again as cursors that may spill.",
        s.eager_retries(),
    );
}

/// The pool's series for `/$/metrics`.
pub fn render_prometheus(st: &AppState, o: &mut String) {
    render_spill(st, o);
    let Some(p) = st.catalog.pool() else {
        return;
    };
    let s = p.stats();
    let mut family = |name: &str, kind: &str, help: &str, value: String| {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} {kind}");
        let _ = writeln!(o, "{name} {value}");
    };
    family(
        "sparkles_query_pool_capacity_bytes",
        "gauge",
        "Bytes of estimated query memory the server may grant in total.",
        s.capacity.to_string(),
    );
    family(
        "sparkles_query_pool_granted_bytes",
        "gauge",
        "Bytes of the query memory pool granted to running queries.",
        s.granted.to_string(),
    );
    family(
        "sparkles_query_admission_waiting",
        "gauge",
        "Requests waiting for memory to run a query.",
        s.waiting.to_string(),
    );
    family(
        "sparkles_query_admission_rejected_total",
        "counter",
        "Requests refused with 503 because the query memory pool stayed full.",
        s.rejected.to_string(),
    );
    let (counts, sum) = st.admission.snapshot();
    let _ = writeln!(
        o,
        "# HELP sparkles_query_admission_wait_seconds Time requests waited for query memory before they ran."
    );
    let _ = writeln!(o, "# TYPE sparkles_query_admission_wait_seconds histogram");
    for (b, c) in WAIT_BOUNDS.iter().zip(&counts) {
        let _ = writeln!(
            o,
            "sparkles_query_admission_wait_seconds_bucket{{le=\"{b}\"}} {c}"
        );
    }
    let total = counts.last().copied().unwrap_or(0);
    let _ = writeln!(
        o,
        "sparkles_query_admission_wait_seconds_bucket{{le=\"+Inf\"}} {total}"
    );
    let _ = writeln!(o, "sparkles_query_admission_wait_seconds_sum {sum}");
    let _ = writeln!(o, "sparkles_query_admission_wait_seconds_count {total}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn sizes(args: &[&str]) -> CacheSizes {
        let mut v = vec!["sparkles"];
        v.extend_from_slice(args);
        v.push("serve");
        cache_sizes(&Cli::try_parse_from(v).unwrap())
    }

    #[test]
    fn profiles_set_defaults_and_flags_override_them() {
        // the complete CLI parser needs the stack of the server's workers
        std::thread::Builder::new()
            .stack_size(crate::THREAD_STACK)
            .spawn(|| {
                assert_eq!(sizes(&[]), CacheSizes::default());
                assert_eq!(sizes(&["--memory-profile", "small"]), CacheSizes::small());
                let s = sizes(&["--memory-profile", "small", "--cache-mb", "512"]);
                assert_eq!(s.blocks, 512 << 20);
                assert_eq!(s.results, CacheSizes::small().results);
                assert_eq!(sizes(&["--result-cache-mb", "0"]).results, 0);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn the_pool_size_comes_from_the_flag_then_the_profile_then_the_host() {
        let c = CacheSizes::default();
        assert_eq!(
            pool_capacity(Some(64), Profile::Default, &c),
            Some(64 << 20)
        );
        assert_eq!(pool_capacity(Some(0), Profile::Small, &c), None);
        assert_eq!(pool_capacity(None, Profile::Small, &c), Some(2048 << 20));
        if let Some(avail) = available_memory() {
            let got = pool_capacity(None, Profile::Default, &c).unwrap();
            assert!(got >= MIN_POOL && got <= avail.max(MIN_POOL), "{got}");
        }
    }
}
