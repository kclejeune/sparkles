//! The OpenTelemetry SDK side of [`super`]: providers, exporters, span and metric hooks.

use super::Settings;
use crate::obs::{Op, Outcome, RequestReport};
use crate::state::AppState;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, header};
use opentelemetry::metrics::{Histogram, Meter, MeterProvider as _};
use opentelemetry::propagation::{Extractor, Injector, TextMapPropagator};
use opentelemetry::trace::{
    Span as _, SpanBuilder, SpanContext, SpanKind, Status, TraceContextExt, Tracer as _,
    TracerProvider as _,
};
use opentelemetry::{Context, KeyValue};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};
use parking_lot::Mutex;
use serde_json::Value as J;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// The tracer of synthesized spans and the request-duration histogram.
struct Globals {
    tracer: Option<SdkTracer>,
    meter: Option<Meter>,
    duration: Option<Histogram<f64>>,
}

static GLOBALS: OnceLock<Globals> = OnceLock::new();

fn globals() -> Option<&'static Globals> {
    GLOBALS.get()
}

/// Upper bounds of `http.server.request.duration`: those of the Prometheus histogram.
const BOUNDS: [f64; 16] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0,
];

/// Most operator spans synthesized for one query.
const MAX_PLAN_SPANS: usize = 256;

/// Characters of query text recorded in `db.query.text`.
const MAX_QUERY_TEXT: usize = 2048;

// ----------------------------------------------------------------- providers ------

pub(super) struct Providers {
    traces: Option<SdkTracerProvider>,
    metrics: Option<SdkMeterProvider>,
    logs: Option<SdkLoggerProvider>,
    /// drives gRPC (tonic) exporters
    _runtime: Option<tokio::runtime::Runtime>,
    description: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Protocol {
    Grpc,
    HttpProtobuf,
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

/// `otlp` → `Some(true)`, `none` → `Some(false)`, unset → `None`.
fn exporter_choice(var: &str) -> Option<bool> {
    let v = env(var)?;
    let v = v.trim().to_ascii_lowercase();
    let mut on = false;
    for part in v.split(',').map(str::trim) {
        match part {
            "otlp" => on = true,
            "none" => {}
            other => tracing::warn!("{var}={other}: only `otlp` and `none` are supported"),
        }
    }
    Some(on)
}

/// The OTLP protocol of a signal (`OTEL_EXPORTER_OTLP_{SIGNAL}_PROTOCOL`, then
/// `OTEL_EXPORTER_OTLP_PROTOCOL`; default `http/protobuf`).
fn protocol(signal: &str) -> anyhow::Result<Protocol> {
    let v = env(&format!("OTEL_EXPORTER_OTLP_{signal}_PROTOCOL"))
        .or_else(|| env("OTEL_EXPORTER_OTLP_PROTOCOL"));
    match v.as_deref().map(str::trim) {
        None | Some("http/protobuf") => Ok(Protocol::HttpProtobuf),
        Some("grpc") => Ok(Protocol::Grpc),
        Some(p) => anyhow::bail!("OTLP protocol '{p}' is not supported (grpc or http/protobuf)"),
    }
}

fn host_name() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .or_else(|| env("HOSTNAME"))
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
}

/// `service.name` (default `sparkles`, unless `OTEL_SERVICE_NAME` or
/// `OTEL_RESOURCE_ATTRIBUTES` name it), `service.version`, `service.instance.id`,
/// `host.name`, `process.pid`, and `OTEL_RESOURCE_ATTRIBUTES`.
fn resource() -> Resource {
    let named = env("OTEL_SERVICE_NAME").is_some()
        || env("OTEL_RESOURCE_ATTRIBUTES").is_some_and(|a| {
            a.split(',').any(|kv| {
                kv.split('=')
                    .next()
                    .is_some_and(|k| k.trim() == "service.name")
            })
        });
    let mut b = Resource::builder();
    if !named {
        b = b.with_service_name("sparkles");
    }
    b = b.with_attributes([
        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
        KeyValue::new("service.instance.id", uuid::Uuid::new_v4().to_string()),
        KeyValue::new("process.pid", i64::from(std::process::id())),
    ]);
    if let Some(h) = host_name() {
        b = b.with_attribute(KeyValue::new("host.name", h));
    }
    b.build()
}

impl Providers {
    /// Build the exporters that `settings` and the environment ask for; `None` when OTel
    /// stays off.
    pub(super) fn from_env(settings: &Settings) -> anyhow::Result<Option<Providers>> {
        if env("OTEL_SDK_DISABLED").is_some_and(|v| v.trim().eq_ignore_ascii_case("true")) {
            return Ok(None);
        }
        let (traces, metrics, logs) = (
            exporter_choice("OTEL_TRACES_EXPORTER"),
            exporter_choice("OTEL_METRICS_EXPORTER"),
            exporter_choice("OTEL_LOGS_EXPORTER"),
        );
        let endpoint = [
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
            "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
        ]
        .into_iter()
        .any(|v| env(v).is_some());
        let asked = settings.enabled
            || endpoint
            || traces == Some(true)
            || metrics == Some(true)
            || logs == Some(true);
        if !asked {
            return Ok(None);
        }
        let traces = traces.unwrap_or(true);
        let metrics = metrics.unwrap_or(true);
        // logs are opt-in: every access-log line would become a log record
        let logs = logs.unwrap_or(settings.logs);
        let protos = (protocol("TRACES")?, protocol("METRICS")?, protocol("LOGS")?);
        let grpc = (traces && protos.0 == Protocol::Grpc)
            || (metrics && protos.1 == Protocol::Grpc)
            || (logs && protos.2 == Protocol::Grpc);
        // tonic channels spawn their connection tasks on the runtime they are built in
        let runtime = grpc
            .then(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .thread_name("otel-export")
                    .enable_all()
                    .build()
            })
            .transpose()?;
        let build = |p: Protocol| -> Option<tokio::runtime::EnterGuard<'_>> {
            (p == Protocol::Grpc)
                .then(|| runtime.as_ref().map(|r| r.enter()))
                .flatten()
        };
        let res = resource();
        use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter};
        let traces = if traces {
            let _rt = build(protos.0);
            let e = match protos.0 {
                Protocol::Grpc => SpanExporter::builder().with_tonic().build()?,
                Protocol::HttpProtobuf => SpanExporter::builder().with_http().build()?,
            };
            Some(
                SdkTracerProvider::builder()
                    .with_batch_exporter(e)
                    .with_resource(res.clone())
                    .build(),
            )
        } else {
            None
        };
        let metrics = if metrics {
            let _rt = build(protos.1);
            let e = match protos.1 {
                Protocol::Grpc => MetricExporter::builder().with_tonic().build()?,
                Protocol::HttpProtobuf => MetricExporter::builder().with_http().build()?,
            };
            Some(
                SdkMeterProvider::builder()
                    .with_periodic_exporter(e)
                    .with_resource(res.clone())
                    .build(),
            )
        } else {
            None
        };
        let logs = if logs {
            let _rt = build(protos.2);
            let e = match protos.2 {
                Protocol::Grpc => LogExporter::builder().with_tonic().build()?,
                Protocol::HttpProtobuf => LogExporter::builder().with_http().build()?,
            };
            Some(
                SdkLoggerProvider::builder()
                    .with_batch_exporter(e)
                    .with_resource(res)
                    .build(),
            )
        } else {
            None
        };
        let name = |on: bool, p: Protocol| match (on, p) {
            (false, _) => "off",
            (true, Protocol::Grpc) => "otlp/grpc",
            (true, Protocol::HttpProtobuf) => "otlp/http",
        };
        let description = format!(
            "OpenTelemetry: traces {}, metrics {}, logs {}",
            name(traces.is_some(), protos.0),
            name(metrics.is_some(), protos.1),
            name(logs.is_some(), protos.2),
        );
        let p = Providers {
            traces,
            metrics,
            logs,
            _runtime: runtime,
            description,
        };
        p.install();
        Ok(Some(p))
    }

    /// Providers around given exporters (tests).
    #[cfg(test)]
    pub(super) fn with(
        traces: Option<SdkTracerProvider>,
        metrics: Option<SdkMeterProvider>,
    ) -> Providers {
        let p = Providers {
            traces,
            metrics,
            logs: None,
            _runtime: None,
            description: String::new(),
        };
        p.install();
        p
    }

    /// Publish the tracer and instruments, and propagate trace context on the engine's
    /// outbound requests.
    fn install(&self) {
        let meter = self.metrics.as_ref().map(|m| m.meter("sparkles"));
        let duration = meter.as_ref().map(|m| {
            m.f64_histogram("http.server.request.duration")
                .with_unit("s")
                .with_description("Duration of HTTP server requests.")
                .with_boundaries(BOUNDS.to_vec())
                .build()
        });
        let _ = GLOBALS.set(Globals {
            tracer: self.traces.as_ref().map(|t| t.tracer("sparkles")),
            meter,
            duration,
        });
        if self.traces.is_some() {
            sparkles::outbound::set_headers_hook(|add| {
                let cx = Span::current().context();
                TraceContextPropagator::new().inject_context(&cx, &mut AddHeader(add));
            });
        }
    }

    pub(super) fn describe(&self) -> String {
        self.description.clone()
    }

    pub(super) fn layers<S>(&self) -> Option<Box<dyn tracing_subscriber::Layer<S> + Send + Sync>>
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a> + Send + Sync,
    {
        use tracing_subscriber::Layer;
        // boxed layers in a Vec: an `Option` layer that is `None` reports a maximum level
        // of OFF, which would switch the other layers off with it
        let mut layers: Vec<Box<dyn Layer<S> + Send + Sync>> = Vec::new();
        if let Some(t) = &self.traces {
            layers.push(
                tracing_opentelemetry::layer()
                    .with_tracer(t.tracer("sparkles"))
                    .with_threads(false)
                    .with_location(false)
                    // no busy_ns / idle_ns attributes
                    .with_tracked_inactivity(false)
                    .boxed(),
            );
        }
        if let Some(l) = &self.logs {
            // never export the exporters' own diagnostics (a feedback loop)
            layers.push(
                opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(l)
                    .with_filter(tracing_subscriber::filter::filter_fn(|m| {
                        let t = m.target();
                        !["opentelemetry", "tonic", "h2", "hyper", "reqwest", "tower"]
                            .iter()
                            .any(|p| t.starts_with(p))
                    }))
                    .boxed(),
            );
        }
        (!layers.is_empty()).then(|| layers.boxed())
    }

    /// Flush and shut down every provider within `timeout` in all (a stuck collector
    /// cannot hold up the process exit).
    pub(super) fn shutdown(self, timeout: Duration) {
        let (tx, rx) = std::sync::mpsc::channel();
        let Providers {
            traces,
            metrics,
            logs,
            _runtime,
            ..
        } = self;
        std::thread::Builder::new()
            .name("otel-shutdown".into())
            .spawn(move || {
                let each = timeout.mul_f64(0.9);
                if let Some(t) = traces {
                    let _ = t.force_flush();
                    let _ = t.shutdown_with_timeout(each);
                }
                if let Some(m) = metrics {
                    let _ = m.force_flush();
                    let _ = m.shutdown_with_timeout(each);
                }
                if let Some(l) = logs {
                    let _ = l.force_flush();
                    let _ = l.shutdown_with_timeout(each);
                }
                let _ = tx.send(());
            })
            .ok();
        if rx.recv_timeout(timeout).is_err() {
            eprintln!("OpenTelemetry: exporters did not flush within {timeout:?}");
        }
        if let Some(rt) = _runtime {
            rt.shutdown_timeout(Duration::from_millis(100));
        }
    }
}

/// `traceparent` / `tracestate` into the engine's outbound header callback.
struct AddHeader<'a>(&'a mut dyn FnMut(&str, &str));

impl Injector for AddHeader<'_> {
    fn set(&mut self, key: &str, value: String) {
        (self.0)(key, &value);
    }
}

struct Headers<'a>(&'a HeaderMap);

impl Extractor for Headers<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }
    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

// ------------------------------------------------------------------ requests ------

pub(super) struct ReqInfo {
    method: &'static str,
    route: Option<String>,
    /// trace and span id of a sampled request
    sampled: Option<SpanContext>,
}

fn method_name(m: &axum::http::Method) -> &'static str {
    use axum::http::Method as M;
    match *m {
        M::GET => "GET",
        M::HEAD => "HEAD",
        M::POST => "POST",
        M::PUT => "PUT",
        M::DELETE => "DELETE",
        M::OPTIONS => "OPTIONS",
        M::PATCH => "PATCH",
        M::TRACE => "TRACE",
        M::CONNECT => "CONNECT",
        _ => "_OTHER",
    }
}

pub(super) fn on_request(span: &Span, req: &Request, route: Option<&str>, id: &str) -> ReqInfo {
    let parent = TraceContextPropagator::new().extract(&Headers(req.headers()));
    if parent.span().span_context().is_valid() {
        let _ = span.set_parent(parent);
    }
    let method = method_name(req.method());
    span.set_attribute("http.request.method", method);
    if method == "_OTHER" {
        span.set_attribute("http.request.method_original", req.method().to_string());
    }
    if let Some(r) = route {
        span.set_attribute("http.route", r.to_string());
    }
    span.set_attribute("url.path", req.uri().path().to_string());
    span.set_attribute(
        "url.scheme",
        req.uri().scheme_str().unwrap_or("http").to_string(),
    );
    if let Some(host) = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.parse::<axum::http::uri::Authority>().ok())
    {
        span.set_attribute("server.address", host.host().to_string());
        if let Some(p) = host.port_u16() {
            span.set_attribute("server.port", i64::from(p));
        }
    }
    if let Some(peer) = crate::ratelimit::peer_ip(req) {
        span.set_attribute("client.address", peer.to_string());
        span.set_attribute("network.peer.address", peer.to_string());
    }
    if let Some(ua) = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
    {
        span.set_attribute("user_agent.original", ua.to_string());
    }
    span.set_attribute("sparkles.request_id", id.to_string());
    span.set_attribute("db.system.name", "sparkles");
    let cx = span.context();
    let sc = cx.span().span_context().clone();
    let sampled = sc.is_sampled().then(|| {
        span.record("trace_id", tracing::field::display(sc.trace_id()));
        span.record("span_id", tracing::field::display(sc.span_id()));
        sc
    });
    ReqInfo {
        method,
        route: route.map(str::to_string),
        sampled,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn on_response(
    info: &ReqInfo,
    span: &Span,
    st: &AppState,
    status: u16,
    op: Op,
    outcome: Outcome,
    dataset: Option<&str>,
    elapsed: Duration,
    report: &RequestReport,
) {
    let error = (status >= 500).then(|| status.to_string());
    if info.sampled.is_some() {
        span.set_attribute("http.response.status_code", i64::from(status));
        span.set_attribute("db.operation.name", op.as_str());
        span.set_attribute("sparkles.outcome", outcome.as_str());
        if let Some(d) = dataset {
            span.set_attribute("db.namespace", d.to_string());
        }
        if let Some(n) = report.rows {
            span.set_attribute("db.response.returned_rows", n as i64);
        }
        if let Some(b) = report.response_bytes {
            span.set_attribute("http.response.body.size", b as i64);
        }
        if let Some(m) = report.mem_peak_bytes {
            span.set_attribute("sparkles.memory.peak_bytes", m as i64);
        }
        if let Some(b) = report.budget {
            span.set_attribute("sparkles.budget", b.as_str());
        }
        if let Some(e) = &error {
            span.set_attribute("error.type", e.clone());
            span.set_status(Status::error(format!("HTTP {status}")));
        } else if outcome == Outcome::Cancelled {
            span.set_attribute("error.type", "cancelled");
        }
    }
    if let Some(h) = globals().and_then(|g| g.duration.as_ref()) {
        let mut attrs = vec![
            KeyValue::new("http.request.method", info.method),
            KeyValue::new("url.scheme", "http"),
            KeyValue::new("http.response.status_code", i64::from(status)),
            KeyValue::new("db.operation.name", op.as_str()),
            KeyValue::new("db.namespace", st.metrics.dataset_label(dataset)),
        ];
        if let Some(r) = &info.route {
            attrs.push(KeyValue::new("http.route", r.clone()));
        }
        if let Some(e) = error {
            attrs.push(KeyValue::new("error.type", e));
        }
        h.record(elapsed.as_secs_f64(), &attrs);
    }
}

pub(super) fn response_headers(info: &ReqInfo, headers: &mut HeaderMap) {
    if let Some(sc) = &info.sampled
        && let Ok(v) = HeaderValue::from_str(&format!(
            "00-{}-{}-{:02x}",
            sc.trace_id(),
            sc.span_id(),
            sc.trace_flags().to_u8()
        ))
    {
        headers.insert("traceresponse", v);
    }
}

pub(super) fn query_text(span: &Span, text: &str) {
    let cut = text
        .char_indices()
        .nth(MAX_QUERY_TEXT)
        .map_or(text, |(i, _)| &text[..i]);
    span.set_attribute("db.query.text", cut.to_string());
}

pub(super) fn rate_limited(span: &Span, class: &'static str, reason: &'static str) {
    span.set_attribute("sparkles.rate_limit.class", class);
    span.add_event(
        "rate_limited",
        vec![
            KeyValue::new("sparkles.rate_limit.class", class),
            KeyValue::new("sparkles.rate_limit.reason", reason),
        ],
    );
}

pub(super) fn commit(span: &Span, r: &sparkles::commit::Receipt) {
    span.set_attribute("sparkles.commit.seq", r.commit.seq as i64);
    span.set_attribute("sparkles.commit.committed", r.committed);
    span.set_attribute("sparkles.dataset.id", r.dataset_id.to_string());
}

pub(super) fn link_current(span: &Span) {
    let cx = Span::current().context();
    let sc = cx.span().span_context().clone();
    if sc.is_valid() {
        span.add_link(sc);
    }
}

// ----------------------------------------------------------- synthesized spans ------

/// A finished child span of `parent` from `start` to `end`.
fn child(
    tracer: &SdkTracer,
    parent: &Context,
    name: impl Into<std::borrow::Cow<'static, str>>,
    start: SystemTime,
    end: SystemTime,
    attrs: Vec<KeyValue>,
) -> Context {
    let mut s = tracer.build_with_context(
        SpanBuilder::from_name(name)
            .with_kind(SpanKind::Internal)
            .with_start_time(start)
            .with_attributes(attrs),
        parent,
    );
    let cx = parent.with_remote_span_context(s.span_context().clone());
    s.end_with_timestamp(end);
    cx
}

fn ms(x: f64) -> Duration {
    Duration::from_secs_f64((x / 1000.0).max(0.0))
}

/// The sampled context of `span` and the tracer, or nothing to do.
fn sampled(span: &Span) -> Option<(&'static SdkTracer, Context)> {
    let tracer = globals()?.tracer.as_ref()?;
    let cx = span.context();
    cx.span()
        .span_context()
        .is_sampled()
        .then_some((tracer, cx))
}

pub(super) fn query_done(
    span: &Span,
    t0: SystemTime,
    r: &sparkles::sparql::QueryResult,
    serialize_ms: f64,
    plan_spans: bool,
    descriptions: bool,
) {
    let kind = match r.kind {
        sparkles::sparql::QueryKind::Select => "SELECT",
        sparkles::sparql::QueryKind::Ask => "ASK",
        sparkles::sparql::QueryKind::Construct => "CONSTRUCT",
        sparkles::sparql::QueryKind::Describe => "DESCRIBE",
    };
    let Some((tracer, cx)) = sampled(span) else {
        return;
    };
    span.set_attribute("sparkles.sparql.kind", kind);
    let t = &r.timing;
    let parse_end = t0 + ms(t.parse_ms);
    let plan_end = parse_end + ms(t.plan_ms);
    let exec_end = plan_end + ms(t.exec_ms);
    child(tracer, &cx, "sparql.parse", t0, parse_end, vec![]);
    child(tracer, &cx, "sparql.plan", parse_end, plan_end, vec![]);
    let exec = child(
        tracer,
        &cx,
        "sparql.execute",
        plan_end,
        exec_end,
        vec![
            KeyValue::new("db.response.returned_rows", r.len() as i64),
            KeyValue::new("sparkles.memory.peak_bytes", r.mem_peak_bytes as i64),
        ],
    );
    let now = SystemTime::now();
    let ser_start = now
        .checked_sub(ms(serialize_ms))
        .unwrap_or(now)
        .max(exec_end);
    child(tracer, &cx, "sparql.serialize", ser_start, now, vec![]);
    if plan_spans {
        let mut budget = MAX_PLAN_SPANS;
        plan_tree(tracer, &exec, &r.plan, plan_end, descriptions, &mut budget);
    }
}

/// Operator spans: each lasts its recorded time; children are laid out one after the
/// other from their parent's start (durations are exact, offsets approximate).
fn plan_tree(
    tracer: &SdkTracer,
    parent: &Context,
    p: &sparkles::sparql::PlanInfo,
    start: SystemTime,
    descriptions: bool,
    budget: &mut usize,
) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    let mut attrs = vec![
        KeyValue::new("sparkles.operator", p.operator.clone()),
        KeyValue::new("sparkles.rows", p.actual_rows),
        KeyValue::new("sparkles.rows.estimated", p.estimated_rows),
        KeyValue::new("sparkles.cost.estimated", p.estimated_cost),
    ];
    if p.cached {
        attrs.push(KeyValue::new("sparkles.cached", true));
    }
    if descriptions && !p.description.is_empty() {
        attrs.push(KeyValue::new(
            "sparkles.operator.description",
            p.description.clone(),
        ));
    }
    let end = start + ms(p.time_ms);
    let cx = child(tracer, parent, p.operator.clone(), start, end, attrs);
    let mut at = start;
    for c in &p.children {
        plan_tree(tracer, &cx, c, at, descriptions, budget);
        at += ms(c.time_ms);
    }
}

pub(super) fn update_done(span: &Span, t0: SystemTime, s: &sparkles::sparql::update::UpdateStats) {
    let Some((tracer, cx)) = sampled(span) else {
        return;
    };
    span.set_attribute("sparkles.update.operations", s.operations as i64);
    span.set_attribute("sparkles.update.inserted", s.inserted as i64);
    span.set_attribute("sparkles.update.deleted", s.deleted as i64);
    let parse_end = t0 + ms(s.timing.parse_ms);
    child(tracer, &cx, "sparql.parse", t0, parse_end, vec![]);
    child(
        tracer,
        &cx,
        "sparql.execute",
        parse_end,
        parse_end + ms(s.timing.exec_ms),
        vec![
            KeyValue::new("sparkles.update.inserted", s.inserted as i64),
            KeyValue::new("sparkles.update.deleted", s.deleted as i64),
        ],
    );
}

// ------------------------------------------------------------------- metrics ------

/// One registry snapshot per collection, shared by every observable instrument.
struct Snapshot {
    st: Arc<AppState>,
    last: Mutex<Option<(Instant, Arc<J>)>>,
}

impl Snapshot {
    fn get(&self) -> Arc<J> {
        let mut last = self.last.lock();
        if let Some((t, v)) = &*last
            && t.elapsed() < Duration::from_secs(1)
        {
            return v.clone();
        }
        let v = Arc::new(crate::obs::metrics_json(&self.st));
        *last = Some((Instant::now(), v.clone()));
        v
    }
}

type Observe = fn(&J, &mut dyn FnMut(f64, Vec<KeyValue>));

/// The registry's series (from its JSON snapshot) as OTel instruments.
pub(super) fn register_metrics(st: Arc<AppState>) {
    let Some(meter) = globals().and_then(|g| g.meter.clone()) else {
        return;
    };
    let snap = Arc::new(Snapshot {
        st,
        last: Mutex::new(None),
    });
    // (name, unit, description, kind, observe)
    enum Kind {
        Counter,
        UpDown,
        Gauge,
    }
    let series: Vec<(&'static str, &'static str, &'static str, Kind, Observe)> = vec![
        (
            "sparkles.requests",
            "{request}",
            "Completed requests by dataset, operation and outcome.",
            Kind::Counter,
            |j, f| {
                for r in j["requests"].as_array().into_iter().flatten() {
                    for (oc, n) in r["outcomes"].as_object().into_iter().flatten() {
                        f(
                            n.as_f64().unwrap_or(0.0),
                            vec![
                                KeyValue::new("dataset", str_of(&r["dataset"])),
                                KeyValue::new("operation", str_of(&r["operation"])),
                                KeyValue::new("outcome", oc.clone()),
                            ],
                        );
                    }
                }
            },
        ),
        (
            "sparkles.response.size",
            "By",
            "Uncompressed bytes of response bodies whose size is known.",
            Kind::Counter,
            |j, f| {
                for r in j["requests"].as_array().into_iter().flatten() {
                    f(
                        r["responseBytes"].as_f64().unwrap_or(0.0),
                        vec![
                            KeyValue::new("dataset", str_of(&r["dataset"])),
                            KeyValue::new("operation", str_of(&r["operation"])),
                        ],
                    );
                }
            },
        ),
        (
            "sparkles.requests.active",
            "{request}",
            "Requests in progress by operation.",
            Kind::UpDown,
            |j, f| {
                for (op, n) in j["active"].as_object().into_iter().flatten() {
                    f(
                        n.as_f64().unwrap_or(0.0),
                        vec![KeyValue::new("operation", op.clone())],
                    );
                }
            },
        ),
        (
            "sparkles.result.rows",
            "{row}",
            "Result rows of queries (triples for CONSTRUCT and DESCRIBE).",
            Kind::Counter,
            |j, f| per_dataset(j, f, |d| d["resultRows"].as_f64()),
        ),
        (
            "sparkles.budget.exceeded",
            "{request}",
            "Requests that exceeded a budget.",
            Kind::Counter,
            |j, f| per_dataset_map(j, f, "budgetExceeded", "budget"),
        ),
        (
            "sparkles.rate_limited",
            "{request}",
            "Requests refused by a rate or concurrency limit.",
            Kind::Counter,
            |j, f| per_dataset_map(j, f, "rateLimited", "class"),
        ),
        (
            "sparkles.dataset.quads",
            "{quad}",
            "Quads in the dataset.",
            Kind::Gauge,
            |j, f| per_dataset(j, f, |d| d["quads"].as_f64()),
        ),
        (
            "sparkles.delta.quads",
            "{quad}",
            "Inserted and deleted quads not yet compacted into the index.",
            Kind::Gauge,
            |j, f| {
                for d in j["datasets"].as_array().into_iter().flatten() {
                    for (kind, key) in [("insert", "deltaInserts"), ("delete", "deltaDeletes")] {
                        f(
                            d[key].as_f64().unwrap_or(0.0),
                            vec![
                                KeyValue::new("dataset", str_of(&d["name"])),
                                KeyValue::new("kind", kind),
                            ],
                        );
                    }
                }
            },
        ),
        (
            "sparkles.wal.size",
            "By",
            "Size of the write-ahead log.",
            Kind::Gauge,
            |j, f| per_dataset(j, f, |d| d["walBytes"].as_f64()),
        ),
        (
            "sparkles.disk.size",
            "By",
            "Size of the database directory.",
            Kind::Gauge,
            |j, f| per_dataset(j, f, |d| d["diskBytes"].as_f64()),
        ),
        (
            "sparkles.block_cache.size",
            "By",
            "Bytes held by the decoded-block cache.",
            Kind::Gauge,
            |j, f| per_dataset(j, f, |d| d["blockCache"]["bytes"].as_f64()),
        ),
        (
            "sparkles.block_cache.capacity",
            "By",
            "Capacity of the decoded-block cache.",
            Kind::Gauge,
            |j, f| per_dataset(j, f, |d| d["blockCache"]["capacityBytes"].as_f64()),
        ),
        (
            "sparkles.block_cache.hits",
            "{hit}",
            "Decoded-block cache hits.",
            Kind::Counter,
            |j, f| per_dataset(j, f, |d| d["blockCache"]["hits"].as_f64()),
        ),
        (
            "sparkles.block_cache.misses",
            "{miss}",
            "Decoded-block cache misses.",
            Kind::Counter,
            |j, f| per_dataset(j, f, |d| d["blockCache"]["misses"].as_f64()),
        ),
        (
            "sparkles.result_cache.size",
            "By",
            "Bytes held by the query result cache.",
            Kind::Gauge,
            |j, f| per_dataset(j, f, |d| d["resultCache"]["bytes"].as_f64()),
        ),
        (
            "sparkles.result_cache.capacity",
            "By",
            "Capacity of the query result cache.",
            Kind::Gauge,
            |j, f| per_dataset(j, f, |d| d["resultCache"]["capacityBytes"].as_f64()),
        ),
        (
            "sparkles.result_cache.entries",
            "{entry}",
            "Entries in the query result cache.",
            Kind::Gauge,
            |j, f| per_dataset(j, f, |d| d["resultCache"]["entries"].as_f64()),
        ),
        (
            "sparkles.result_cache.hits",
            "{hit}",
            "Query result cache hits.",
            Kind::Counter,
            |j, f| per_dataset(j, f, |d| d["resultCache"]["hits"].as_f64()),
        ),
        (
            "sparkles.result_cache.misses",
            "{miss}",
            "Query result cache misses.",
            Kind::Counter,
            |j, f| per_dataset(j, f, |d| d["resultCache"]["misses"].as_f64()),
        ),
        (
            "sparkles.geo.rows",
            "{row}",
            "Rows of the spatial index by part (base, overlay, tail).",
            Kind::Gauge,
            |j, f| {
                for d in j["datasets"].as_array().into_iter().flatten() {
                    let rows = &d["geo"]["rows"];
                    if d["geo"]["enabled"] != true {
                        continue;
                    }
                    for part in ["base", "overlay", "tail"] {
                        f(
                            rows[part].as_f64().unwrap_or(0.0),
                            vec![
                                KeyValue::new("dataset", str_of(&d["name"])),
                                KeyValue::new("part", part),
                            ],
                        );
                    }
                }
            },
        ),
        (
            "sparkles.geo.build.duration",
            "s",
            "Duration of the last build of the spatial index's base.",
            Kind::Gauge,
            |j, f| per_dataset(j, f, |d| d["geo"]["buildSeconds"].as_f64()),
        ),
        (
            "sparkles.geo.candidates",
            "{row}",
            "Index candidates of spatial operators (rows whose envelope matched).",
            Kind::Counter,
            |j, f| per_dataset(j, f, |d| d["geo"]["candidates"].as_f64()),
        ),
        (
            "sparkles.geo.refined",
            "{test}",
            "Exact geometry tests run by spatial operators.",
            Kind::Counter,
            |j, f| per_dataset(j, f, |d| d["geo"]["refined"].as_f64()),
        ),
        (
            "sparkles.geo.matches",
            "{row}",
            "Rows that passed the exact test of spatial operators.",
            Kind::Counter,
            |j, f| per_dataset(j, f, |d| d["geo"]["matches"].as_f64()),
        ),
        (
            "sparkles.geo.rechecked",
            "{row}",
            "Candidates of spatial operators that the index could not place, tested whatever the search window.",
            Kind::Counter,
            |j, f| per_dataset(j, f, |d| d["geo"]["rechecked"].as_f64()),
        ),
        (
            "sparkles.ready",
            "1",
            "Whether the server is ready to serve requests (1) or not (0).",
            Kind::Gauge,
            |j, f| f(if j["ready"] == true { 1.0 } else { 0.0 }, vec![]),
        ),
        (
            "process.uptime",
            "s",
            "The time the process has been running.",
            Kind::Gauge,
            |j, f| f(j["uptimeSeconds"].as_f64().unwrap_or(0.0), vec![]),
        ),
        (
            "process.memory.usage",
            "By",
            "The amount of physical memory in use.",
            Kind::Gauge,
            |j, f| {
                if let Some(v) = j["processResidentBytes"].as_f64() {
                    f(v, vec![]);
                }
            },
        ),
    ];
    for (name, unit, desc, kind, observe) in series {
        let snap = snap.clone();
        // the instruments live as long as the meter provider
        match kind {
            Kind::Counter => {
                meter
                    .f64_observable_counter(name)
                    .with_unit(unit)
                    .with_description(desc)
                    .with_callback(move |o| observe(&snap.get(), &mut |v, a| o.observe(v, &a)))
                    .build();
            }
            Kind::UpDown => {
                meter
                    .f64_observable_up_down_counter(name)
                    .with_unit(unit)
                    .with_description(desc)
                    .with_callback(move |o| observe(&snap.get(), &mut |v, a| o.observe(v, &a)))
                    .build();
            }
            Kind::Gauge => {
                meter
                    .f64_observable_gauge(name)
                    .with_unit(unit)
                    .with_description(desc)
                    .with_callback(move |o| observe(&snap.get(), &mut |v, a| o.observe(v, &a)))
                    .build();
            }
        }
    }
}

fn str_of(v: &J) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn per_dataset(j: &J, f: &mut dyn FnMut(f64, Vec<KeyValue>), get: fn(&J) -> Option<f64>) {
    for d in j["datasets"].as_array().into_iter().flatten() {
        if let Some(v) = get(d) {
            f(v, vec![KeyValue::new("dataset", str_of(&d["name"]))]);
        }
    }
}

fn per_dataset_map(j: &J, f: &mut dyn FnMut(f64, Vec<KeyValue>), key: &str, label: &'static str) {
    for d in j["datasets"].as_array().into_iter().flatten() {
        for (k, n) in d[key].as_object().into_iter().flatten() {
            f(
                n.as_f64().unwrap_or(0.0),
                vec![
                    KeyValue::new("dataset", str_of(&d["name"])),
                    KeyValue::new(label, k.clone()),
                ],
            );
        }
    }
}
