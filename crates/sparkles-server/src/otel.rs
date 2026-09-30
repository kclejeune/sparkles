//! OpenTelemetry: traces, metrics and logs over OTLP (cargo feature `otel`, on by
//! default; off at run time unless `serve --otel` or the `OTEL_*` environment enables it).
//!
//! * **Traces.** The request span of [`crate::obs`] becomes an OTel server span
//!   (`GET /{ds}/sparql`) with HTTP and database semantic-convention attributes, a parent
//!   taken from an incoming W3C `traceparent`, and a `traceresponse` header. Query
//!   phases (parse, plan, execute, serialize) and, optionally, the executed operator tree
//!   become child spans synthesized after the fact from the recorded timings; nothing is
//!   instrumented inside the executor. Engine `tracing` spans (commits, SERVICE calls)
//!   and background tasks are exported through `tracing-opentelemetry`. Outbound SERVICE
//!   and LOAD requests carry `traceparent`.
//! * **Metrics.** `http.server.request.duration` is recorded per request; the counters
//!   and gauges of the Prometheus registry are exported as observable instruments read
//!   from the same registry at collection time (nothing is counted twice).
//! * **Logs.** Optionally, `tracing` events are exported as OTLP log records with their
//!   trace and span ids.
//!
//! Every hook here returns at once when OTel is off: one relaxed atomic load.

#![cfg_attr(not(feature = "otel"), allow(dead_code))]

use crate::obs::{Op, Outcome, RequestReport};
use crate::state::AppState;
use axum::extract::Request;
use axum::http::HeaderMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};
use tracing::Span;

#[cfg(feature = "otel")]
mod sdk;
#[cfg(all(test, feature = "otel"))]
mod tests;

static ON: AtomicBool = AtomicBool::new(false);
static QUERY_TEXT: AtomicBool = AtomicBool::new(false);
static PLAN_SPANS: AtomicBool = AtomicBool::new(false);

/// Whether OpenTelemetry is exporting.
#[inline]
pub fn enabled() -> bool {
    cfg!(feature = "otel") && ON.load(Ordering::Relaxed)
}

/// What `serve` asked for on its command line (the environment adds to it).
#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// `--otel`: export even when no `OTEL_*` variable asks for it
    pub enabled: bool,
    /// `--otel-query-text`: record query and update text (`db.query.text`) and plan
    /// operator descriptions, which may hold data
    pub query_text: bool,
    /// `--otel-plan-spans`: one span per executed plan operator
    pub plan_spans: bool,
    /// `--otel-logs`: export `tracing` events as OTLP logs (also `OTEL_LOGS_EXPORTER=otlp`)
    pub logs: bool,
}

/// The exporters of a running process; [`Guard::shutdown`] flushes them.
pub struct Guard {
    #[cfg(feature = "otel")]
    inner: Option<sdk::Providers>,
}

/// Longest wait for the exporters to flush at shutdown.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

impl Guard {
    /// No exporters.
    pub fn none() -> Guard {
        Guard {
            #[cfg(feature = "otel")]
            inner: None,
        }
    }

    /// The `tracing` layers that feed the exporters (spans, and logs when enabled).
    pub fn layers<S>(&self) -> Option<Box<dyn tracing_subscriber::Layer<S> + Send + Sync>>
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a> + Send + Sync,
    {
        #[cfg(feature = "otel")]
        {
            self.inner.as_ref().and_then(sdk::Providers::layers)
        }
        #[cfg(not(feature = "otel"))]
        None
    }

    /// Flush and stop the exporters, waiting at most [`SHUTDOWN_TIMEOUT`].
    pub fn shutdown(self) {
        ON.store(false, Ordering::Relaxed);
        #[cfg(feature = "otel")]
        if let Some(p) = self.inner {
            p.shutdown(SHUTDOWN_TIMEOUT);
        }
    }
}

/// Set up the exporters when `settings` or the environment enable them (before the
/// `tracing` subscriber is installed; [`Guard::layers`] gives its layers).
pub fn init(settings: &Settings) -> anyhow::Result<Guard> {
    #[cfg(feature = "otel")]
    {
        let Some(p) = sdk::Providers::from_env(settings)? else {
            return Ok(Guard::none());
        };
        QUERY_TEXT.store(settings.query_text, Ordering::Relaxed);
        PLAN_SPANS.store(settings.plan_spans, Ordering::Relaxed);
        ON.store(true, Ordering::Relaxed);
        Ok(Guard { inner: Some(p) })
    }
    #[cfg(not(feature = "otel"))]
    {
        if settings.enabled {
            anyhow::bail!("--otel: built without the `otel` feature");
        }
        Ok(Guard::none())
    }
}

/// A one-line description of the exporters for the startup log.
pub fn describe(g: &Guard) -> Option<String> {
    #[cfg(feature = "otel")]
    {
        g.inner.as_ref().map(sdk::Providers::describe)
    }
    #[cfg(not(feature = "otel"))]
    {
        let _ = g;
        None
    }
}

/// Export the metrics registry of `st` through the OTel meter.
pub fn register_metrics(st: &Arc<AppState>) {
    #[cfg(feature = "otel")]
    if enabled() {
        sdk::register_metrics(st.clone());
    }
    #[cfg(not(feature = "otel"))]
    let _ = st;
}

// ------------------------------------------------------------------- requests ------

/// What the response hooks need to know about a request.
#[derive(Default)]
pub struct Req {
    #[cfg(feature = "otel")]
    inner: Option<Box<sdk::ReqInfo>>,
}

/// The request span: `request{request_id, method, route}`, plus the OTel span name and
/// kind (and, once known, `trace_id` / `span_id` for the logs) when OTel is on.
pub fn request_span(id: &str, method: &axum::http::Method, route: Option<&str>) -> Span {
    if enabled() {
        let name = match route {
            Some(r) => format!("{method} {r}"),
            None => method.to_string(),
        };
        tracing::info_span!(
            target: "sparkles_server::obs",
            "request",
            request_id = %id,
            method = %method,
            route = route.unwrap_or("-"),
            otel.name = %name,
            otel.kind = "server",
            trace_id = tracing::field::Empty,
            span_id = tracing::field::Empty,
        )
    } else {
        tracing::info_span!(
            target: "sparkles_server::obs",
            "request",
            request_id = %id,
            method = %method,
            route = route.unwrap_or("-"),
        )
    }
}

/// Continue the trace of an incoming `traceparent` and describe the request.
pub fn on_request(span: &Span, req: &Request, route: Option<&str>, request_id: &str) -> Req {
    #[cfg(feature = "otel")]
    if enabled() {
        return Req {
            inner: Some(Box::new(sdk::on_request(span, req, route, request_id))),
        };
    }
    let _ = (span, req, route, request_id);
    Req::default()
}

/// Record the outcome of a request on its span and in `http.server.request.duration`.
#[allow(clippy::too_many_arguments)]
pub fn on_response(
    r: &Req,
    span: &Span,
    st: &AppState,
    status: u16,
    op: Op,
    outcome: Outcome,
    dataset: Option<&str>,
    elapsed: Duration,
    report: &RequestReport,
) {
    #[cfg(feature = "otel")]
    if let Some(info) = &r.inner {
        sdk::on_response(
            info, span, st, status, op, outcome, dataset, elapsed, report,
        );
    }
    #[cfg(not(feature = "otel"))]
    let _ = (r, span, st, status, op, outcome, dataset, elapsed, report);
}

/// `traceresponse` (W3C Trace Context Level 2) for a sampled request.
pub fn response_headers(r: &Req, headers: &mut HeaderMap) {
    #[cfg(feature = "otel")]
    if let Some(info) = &r.inner {
        sdk::response_headers(info, headers);
    }
    #[cfg(not(feature = "otel"))]
    let _ = (r, headers);
}

/// `db.query.text` on the current span, when enabled (`--otel-query-text`).
pub fn query_text(text: &str) {
    #[cfg(feature = "otel")]
    if enabled() && QUERY_TEXT.load(Ordering::Relaxed) {
        sdk::query_text(&Span::current(), text);
    }
    #[cfg(not(feature = "otel"))]
    let _ = text;
}

/// The start of a query or update, for the phase spans (`None` when OTel is off).
pub fn start() -> Option<SystemTime> {
    enabled().then(SystemTime::now)
}

/// Phase spans of an executed query (children of the current span); `serialize_ms` is
/// the serialization that just ended.
pub fn query_done(t0: Option<SystemTime>, r: &sparkles::sparql::QueryResult, serialize_ms: f64) {
    #[cfg(feature = "otel")]
    if let Some(t0) = t0 {
        sdk::query_done(
            &Span::current(),
            t0,
            r,
            serialize_ms,
            PLAN_SPANS.load(Ordering::Relaxed),
            QUERY_TEXT.load(Ordering::Relaxed),
        );
    }
    #[cfg(not(feature = "otel"))]
    let _ = (t0, r, serialize_ms);
}

/// Phase spans and counts of an executed update.
pub fn update_done(t0: Option<SystemTime>, stats: &sparkles::sparql::update::UpdateStats) {
    #[cfg(feature = "otel")]
    if let Some(t0) = t0 {
        sdk::update_done(&Span::current(), t0, stats);
    }
    #[cfg(not(feature = "otel"))]
    let _ = (t0, stats);
}

/// The commit a write produced, on the current span.
pub fn commit(receipt: &sparkles::commit::Receipt) {
    #[cfg(feature = "otel")]
    if enabled() {
        sdk::commit(&Span::current(), receipt);
    }
    #[cfg(not(feature = "otel"))]
    let _ = receipt;
}

/// A request refused by a rate or concurrency limit.
pub fn rate_limited(class: &'static str, reason: &'static str) {
    #[cfg(feature = "otel")]
    if enabled() {
        sdk::rate_limited(&Span::current(), class, reason);
    }
    #[cfg(not(feature = "otel"))]
    let _ = (class, reason);
}

/// The root span of a background task, linked to the request that started it.
pub fn task_span(kind: &str, id: &str, dataset: &str) -> Span {
    if !enabled() {
        return tracing::info_span!(parent: None, "task", kind, id, dataset);
    }
    let span = tracing::info_span!(
        parent: None,
        "task",
        kind,
        id,
        dataset,
        otel.name = %format!("task {kind}"),
    );
    #[cfg(feature = "otel")]
    sdk::link_current(&span);
    span
}
