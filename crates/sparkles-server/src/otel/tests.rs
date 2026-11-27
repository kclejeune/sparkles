//! OpenTelemetry tests with in-memory exporters. One pipeline serves the whole test
//! binary (the `tracing` subscriber is process-global), so each test finds its spans by
//! a trace id of its own, sent in `traceparent`.

use super::*;
use crate::http::router;
use crate::obs::Phase;
use crate::state::DbType;
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request as HttpRequest, StatusCode};
use opentelemetry::trace::{SpanId, SpanKind, TraceId};
use opentelemetry::{KeyValue, Value};
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{
    Aggregation, InMemoryMetricExporter, InMemoryMetricExporterBuilder, Instrument, PeriodicReader,
    SdkMeterProvider, Stream, Temporality,
};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::net::SocketAddr;
use std::sync::OnceLock;
use tower::ServiceExt;

struct Pipeline {
    spans: InMemorySpanExporter,
    metrics: InMemoryMetricExporter,
    meter: SdkMeterProvider,
    _providers: sdk::Providers,
}

fn pipeline() -> &'static Pipeline {
    static P: OnceLock<Pipeline> = OnceLock::new();
    P.get_or_init(|| {
        use tracing_subscriber::prelude::*;
        let spans = InMemorySpanExporter::default();
        let tp = SdkTracerProvider::builder()
            .with_simple_exporter(spans.clone())
            .build();
        let metrics = InMemoryMetricExporter::default();
        let meter = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(metrics.clone()).build())
            .build();
        let providers = sdk::Providers::with(Some(tp), Some(meter.clone()));
        let subscriber = tracing_subscriber::registry().with(providers.layers());
        tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
        QUERY_TEXT.store(true, Ordering::Relaxed);
        PLAN_SPANS.store(true, Ordering::Relaxed);
        ON.store(true, Ordering::Relaxed);
        Pipeline {
            spans,
            metrics,
            meter,
            _providers: providers,
        }
    })
}

const DATA: &str = r#"
<http://example.org/alice> <http://xmlns.com/foaf/0.1/name> "Alice" .
<http://example.org/bob> <http://xmlns.com/foaf/0.1/name> "Bob" .
"#;

struct Server {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

fn server() -> Server {
    pipeline();
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    // SERVICE calls go to a stub on 127.0.0.1
    st.outbound.allow_private = true;
    // the requests name the server by its public host
    st.hosts = crate::exposure::Hosts::new("127.0.0.1", &["sparql.example".into()]).unwrap();
    let st = Arc::new(st);
    let ds = st.attach("ds", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    st.set_phase(Phase::Ready);
    Server {
        _dir: dir,
        app: router(st.clone()),
        state: st,
    }
}

/// A fresh trace id per call.
fn trace_id() -> TraceId {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = N.fetch_add(1, Ordering::Relaxed);
    TraceId::from_bytes(((0x5eed_u128 << 64) | u128::from(n)).to_be_bytes())
}

const PARENT: &str = "00f067aa0ba902b7";

fn traceparent(t: TraceId, sampled: bool) -> String {
    format!("00-{t}-{PARENT}-{}", if sampled { "01" } else { "00" })
}

async fn call(app: &Router, req: HttpRequest<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut req = req;
    let addr: SocketAddr = "192.0.2.7:5555".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, headers, body)
}

/// What `f` finds among the finished spans, waiting (up to 10 s) for spans that end on
/// other threads.
async fn wait_for<T>(what: &str, mut f: impl FnMut(Vec<SpanData>) -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(found) = f(pipeline().spans.get_finished_spans().unwrap()) {
            return found;
        }
        assert!(std::time::Instant::now() < deadline, "no {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The finished spans of trace `t`, once its `requests` server spans (the children of
/// the incoming `traceparent`) have ended. A span ends after its children, so a count
/// of spans would not do: the phase and operator spans are all there while the server
/// span may still be open.
async fn spans_of(t: TraceId, requests: usize) -> Vec<SpanData> {
    let parent = SpanId::from_hex(PARENT).unwrap();
    wait_for(&format!("{requests} server spans in trace {t}"), |all| {
        let v: Vec<SpanData> = all
            .into_iter()
            .filter(|s| s.span_context.trace_id() == t)
            .collect();
        let ended = v.iter().filter(|s| s.parent_span_id == parent).count();
        (ended >= requests).then_some(v)
    })
    .await
}

fn attr<'a>(s: &'a SpanData, key: &str) -> Option<&'a Value> {
    s.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| &kv.value)
}

fn attr_str(s: &SpanData, key: &str) -> String {
    attr(s, key)
        .map(|v| v.as_str().into_owned())
        .unwrap_or_default()
}

fn named<'a>(v: &'a [SpanData], name: &str) -> &'a SpanData {
    v.iter().find(|s| s.name == name).unwrap_or_else(|| {
        panic!(
            "no span {name} in {:?}",
            v.iter().map(|s| &s.name).collect::<Vec<_>>()
        )
    })
}

#[tokio::test]
async fn query_spans_follow_the_incoming_trace() {
    let s = server();
    let t = trace_id();
    let q = "SELECT ?n WHERE { ?s <http://xmlns.com/foaf/0.1/name> ?n }";
    let uri = format!(
        "/ds/sparql?query={}",
        percent_encoding::utf8_percent_encode(q, percent_encoding::NON_ALPHANUMERIC)
    );
    let (status, headers, _) = call(
        &s.app,
        HttpRequest::get(uri)
            .header("traceparent", traceparent(t, true))
            .header("host", "sparql.example:3030")
            .header("user-agent", "otel-test/1")
            .header("x-request-id", "otel-q-1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let spans = spans_of(t, 1).await;
    let server = named(&spans, "GET /{ds}/sparql");
    assert_eq!(server.span_kind, SpanKind::Server);
    assert_eq!(server.parent_span_id, SpanId::from_hex(PARENT).unwrap());
    for (k, v) in [
        ("http.request.method", "GET"),
        ("http.route", "/{ds}/sparql"),
        ("url.scheme", "http"),
        ("url.path", "/ds/sparql"),
        ("server.address", "sparql.example"),
        ("client.address", "192.0.2.7"),
        ("user_agent.original", "otel-test/1"),
        ("sparkles.request_id", "otel-q-1"),
        ("db.system.name", "sparkles"),
        ("db.namespace", "ds"),
        ("db.operation.name", "query"),
        ("sparkles.sparql.kind", "SELECT"),
        ("sparkles.outcome", "ok"),
        ("db.query.text", q),
    ] {
        assert_eq!(attr_str(server, k), v, "{k}");
    }
    assert_eq!(
        attr(server, "http.response.status_code"),
        Some(&Value::I64(200))
    );
    assert_eq!(attr(server, "server.port"), Some(&Value::I64(3030)));
    assert_eq!(
        attr(server, "db.response.returned_rows"),
        Some(&Value::I64(2))
    );
    // the phases are children of the server span, in order
    let id = server.span_context.span_id();
    let phases: Vec<&SpanData> = [
        "sparql.parse",
        "sparql.plan",
        "sparql.execute",
        "sparql.serialize",
    ]
    .iter()
    .map(|n| named(&spans, n))
    .collect();
    for w in phases.windows(2) {
        assert!(
            w[0].end_time <= w[1].start_time,
            "{} before {}",
            w[0].name,
            w[1].name
        );
    }
    for p in &phases {
        assert_eq!(p.parent_span_id, id, "{}", p.name);
        assert!(p.start_time >= server.start_time && p.end_time <= server.end_time);
    }
    let exec = phases[2];
    assert_eq!(
        attr(exec, "db.response.returned_rows"),
        Some(&Value::I64(2))
    );
    // the executed plan: operators under `sparql.execute`
    let ops: Vec<&SpanData> = spans
        .iter()
        .filter(|s| attr(s, "sparkles.operator").is_some())
        .collect();
    assert!(!ops.is_empty());
    assert!(
        ops.iter()
            .any(|o| o.parent_span_id == exec.span_context.span_id())
    );
    assert!(ops.iter().all(|o| attr(o, "sparkles.rows").is_some()));
    // the response names the server span
    let tr = headers["traceresponse"].to_str().unwrap();
    assert_eq!(tr, format!("00-{t}-{}-01", id));
}

#[tokio::test]
async fn streamed_query_has_phase_spans() {
    let s = server();
    let t = trace_id();
    let q = "SELECT ?n WHERE { ?s <http://xmlns.com/foaf/0.1/name> ?n }";
    let uri = format!(
        "/ds/sparql?execution=streaming&query={}",
        percent_encoding::utf8_percent_encode(q, percent_encoding::NON_ALPHANUMERIC)
    );
    let (status, _, body) = call(
        &s.app,
        HttpRequest::get(uri)
            .header("traceparent", traceparent(t, true))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("Alice"));
    let spans = spans_of(t, 1).await;
    let server = named(&spans, "GET /{ds}/sparql");
    let id = server.span_context.span_id();
    for name in [
        "sparql.parse",
        "sparql.plan",
        "sparql.execute",
        "sparql.serialize",
    ] {
        assert_eq!(named(&spans, name).parent_span_id, id, "{name}");
    }
    assert_eq!(
        attr(named(&spans, "sparql.execute"), "db.response.returned_rows"),
        Some(&Value::I64(2))
    );
}

#[tokio::test]
async fn unsampled_parents_export_nothing() {
    let s = server();
    let t = trace_id();
    let (status, headers, _) = call(
        &s.app,
        HttpRequest::get("/ds/sparql?query=ASK%7B%7D")
            .header("traceparent", traceparent(t, false))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("traceresponse").is_none());
    // a sampled request afterwards is exported; the unsampled one never was
    let t2 = trace_id();
    call(
        &s.app,
        HttpRequest::get("/ds/sparql?query=ASK%7B%7D")
            .header("traceparent", traceparent(t2, true))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    spans_of(t2, 1).await;
    let all = pipeline().spans.get_finished_spans().unwrap();
    assert!(all.iter().all(|s| s.span_context.trace_id() != t));
}

#[tokio::test]
async fn updates_record_their_commit() {
    let s = server();
    let t = trace_id();
    let (status, _, _) = call(
        &s.app,
        HttpRequest::post("/ds/update")
            .header("traceparent", traceparent(t, true))
            .header("content-type", "application/sparql-update")
            .body(Body::from(
                "INSERT DATA { <http://example.org/carol> <http://xmlns.com/foaf/0.1/name> \"Carol\" }",
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let spans = spans_of(t, 1).await;
    let server = named(&spans, "POST /{ds}/update");
    assert_eq!(attr_str(server, "db.operation.name"), "update");
    assert_eq!(
        attr(server, "sparkles.update.inserted"),
        Some(&Value::I64(1))
    );
    let seq = s.state.get("ds").unwrap().store.head_commit().seq as i64;
    assert_eq!(attr(server, "sparkles.commit.seq"), Some(&Value::I64(seq)));
    // the engine's commit span, inside the request
    let commit = named(&spans, "commit");
    assert_eq!(commit.parent_span_id, server.span_context.span_id());
    assert_eq!(attr(commit, "seq"), Some(&Value::I64(seq)));
    assert_eq!(attr_str(commit, "kind"), "update");
    named(&spans, "sparql.parse");
    named(&spans, "sparql.execute");
}

#[tokio::test]
async fn rate_limited_requests_carry_an_event() {
    pipeline();
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    let mut cfg = crate::ratelimit::Config::default();
    cfg.apply_flag("query=1/min").unwrap();
    st.rate_limit = Some(Arc::new(crate::ratelimit::RateLimiter::new(&cfg).unwrap()));
    let st = Arc::new(st);
    st.attach("ds", DbType::Mem, None).unwrap();
    let app = router(st);
    let t = trace_id();
    for _ in 0..2 {
        call(
            &app,
            HttpRequest::get("/ds/sparql?query=ASK%7B%7D")
                .header("traceparent", traceparent(t, true))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    }
    let spans = spans_of(t, 2).await;
    let limited = spans
        .iter()
        .find(|s| attr(s, "http.response.status_code") == Some(&Value::I64(429)))
        .expect("a 429 span");
    assert_eq!(attr_str(limited, "sparkles.outcome"), "rate_limited");
    assert_eq!(attr_str(limited, "sparkles.rate_limit.class"), "query");
    assert!(
        limited
            .events
            .events
            .iter()
            .any(|e| e.name == "rate_limited")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_calls_propagate_the_trace() {
    let s = server();
    let seen: Arc<parking_lot::Mutex<Option<String>>> = Default::default();
    let stub = {
        let seen = seen.clone();
        Router::new().route(
            "/sparql",
            axum::routing::post(move |headers: HeaderMap| {
                let seen = seen.clone();
                async move {
                    *seen.lock() = headers
                        .get("traceparent")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    (
                        [("content-type", "application/sparql-results+json")],
                        r#"{"head":{"vars":["x"]},"results":{"bindings":[{"x":{"type":"literal","value":"remote"}}]}}"#,
                    )
                }
            }),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });
    let t = trace_id();
    let q =
        format!("SELECT ?x WHERE {{ SERVICE <http://127.0.0.1:{port}/sparql> {{ ?x ?p ?o }} }}");
    let uri = format!(
        "/ds/sparql?query={}",
        percent_encoding::utf8_percent_encode(&q, percent_encoding::NON_ALPHANUMERIC)
    );
    let (status, _, body) = call(
        &s.app,
        HttpRequest::get(uri)
            .header("traceparent", traceparent(t, true))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(String::from_utf8_lossy(&body).contains("remote"));
    let spans = spans_of(t, 1).await;
    let client = named(&spans, "sparql.service");
    assert_eq!(client.span_kind, SpanKind::Client);
    assert_eq!(attr_str(client, "server.address"), "127.0.0.1");
    assert_eq!(
        attr(client, "http.response.status_code"),
        Some(&Value::I64(200))
    );
    let server = named(&spans, "GET /{ds}/sparql");
    assert_eq!(client.parent_span_id, server.span_context.span_id());
    // the remote endpoint continues the trace under the client span
    let tp = seen.lock().clone().expect("traceparent sent");
    assert_eq!(tp, format!("00-{t}-{}-01", client.span_context.span_id()));
}

#[tokio::test]
async fn background_tasks_are_linked_to_their_request() {
    let s = server();
    let t = trace_id();
    let (status, _, body) = call(
        &s.app,
        HttpRequest::post("/$/backup/ds")
            .header("traceparent", traceparent(t, true))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let request = named(&spans_of(t, 1).await, "POST /$/backup/{ds}").clone();
    let task = wait_for("task span", |all| {
        all.into_iter()
            .find(|s| s.name == "task backup" && attr_str(s, "id") == id)
    })
    .await;
    // a trace of its own, linked to the request that started it
    assert_ne!(task.span_context.trace_id(), t);
    assert!(
        task.links
            .links
            .iter()
            .any(|l| l.span_context.span_id() == request.span_context.span_id())
    );
    assert_eq!(attr_str(&task, "kind"), "backup");
    assert_eq!(attr_str(&task, "dataset"), "ds");
}

#[tokio::test]
async fn metrics_are_exported() {
    let s = server();
    register_metrics(&s.state);
    for _ in 0..3 {
        call(
            &s.app,
            HttpRequest::get("/ds/sparql?query=ASK%7B%7D")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    }
    let p = pipeline();
    p.meter.force_flush().unwrap();
    let exported = p.metrics.get_finished_metrics().unwrap();
    let metrics: Vec<_> = exported
        .iter()
        .flat_map(|r| r.scope_metrics())
        .flat_map(|s| s.metrics())
        .collect();
    let find = |name: &str| {
        metrics
            .iter()
            .rev()
            .find(|m| m.name() == name)
            .unwrap_or_else(|| panic!("no metric {name}"))
    };
    let d = find("http.server.request.duration");
    assert_eq!(d.unit(), "s");
    let AggregatedMetrics::F64(MetricData::Histogram(h)) = d.data() else {
        panic!("a histogram");
    };
    assert!(h.data_points().any(|p| {
        has(p.attributes(), "http.route", "/{ds}/sparql")
            && has(p.attributes(), "db.operation.name", "query")
            && p.count() >= 3
            && p.bounds().count() == 16
    }));
    let r = find("sparkles.requests");
    let AggregatedMetrics::F64(MetricData::Sum(sum)) = r.data() else {
        panic!("a sum");
    };
    assert!(sum.is_monotonic());
    assert!(sum.data_points().any(|p| {
        has(p.attributes(), "dataset", "ds")
            && has(p.attributes(), "operation", "query")
            && has(p.attributes(), "outcome", "ok")
            && p.value() >= 3.0
    }));
    let q = find("sparkles.dataset.quads");
    let AggregatedMetrics::F64(MetricData::Gauge(g)) = q.data() else {
        panic!("a gauge");
    };
    assert!(g.data_points().any(|p| p.value() == 2.0));
    find("sparkles.requests.active");
    find("process.uptime");
}

fn has<'a>(mut attrs: impl Iterator<Item = &'a KeyValue>, k: &str, v: &str) -> bool {
    attrs.any(|kv| kv.key.as_str() == k && kv.value.as_str() == v)
}

fn prom_sample(text: &str, series: &str) -> Option<f64> {
    text.lines()
        .find_map(|l| l.strip_prefix(series)?.strip_prefix(' '))
        .map(|v| v.parse().unwrap())
}

fn prom_sample_u64(text: &str, series: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix(series)?.strip_prefix(' '))
        .map(|v| v.parse().unwrap())
}

/// Isolated OTel meter provider for deterministic cross-format parity assertions.
struct MetricsHarness {
    exporter: InMemoryMetricExporter,
    provider: SdkMeterProvider,
}

impl Drop for MetricsHarness {
    fn drop(&mut self) {
        let _ = self.provider.shutdown();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum OtlpValue {
    U64(u64),
    F64(f64),
}

struct OtlpMetric {
    unit: String,
    monotonic: bool,
    temporality: Temporality,
    start_time: std::time::SystemTime,
    time: std::time::SystemTime,
    points: Vec<(Vec<(String, String)>, OtlpValue)>,
}

type OtlpSnapshot = std::collections::BTreeMap<String, OtlpMetric>;

impl MetricsHarness {
    fn new(st: &Arc<AppState>) -> Self {
        Self::with_temporality(st, Temporality::Cumulative)
    }

    fn with_temporality(st: &Arc<AppState>, temporality: Temporality) -> Self {
        Self::with_builder(st, temporality, |b| b)
    }

    fn with_builder(
        st: &Arc<AppState>,
        temporality: Temporality,
        configure: impl FnOnce(
            opentelemetry_sdk::metrics::MeterProviderBuilder,
        ) -> opentelemetry_sdk::metrics::MeterProviderBuilder,
    ) -> Self {
        use opentelemetry::metrics::MeterProvider as _;
        let exporter = InMemoryMetricExporterBuilder::new()
            .with_temporality(temporality)
            .build();
        let builder = sdk::meter_provider_builder(st.metrics.max_datasets())
            .with_reader(PeriodicReader::builder(exporter.clone()).build());
        let provider = configure(builder).build();
        let meter = provider.meter("sparkles");
        sdk::register_metrics_on(&meter, st.clone());
        MetricsHarness { exporter, provider }
    }

    /// Collect one fresh OTLP snapshot keyed by metric name.
    fn collect(&self) -> OtlpSnapshot {
        self.provider.force_flush().unwrap();
        let exported = self.exporter.get_finished_metrics().unwrap();
        let last = exported
            .last()
            .expect("at least one resource metrics batch");
        let mut out = OtlpSnapshot::new();
        for sm in last.scope_metrics() {
            for m in sm.metrics() {
                match m.data() {
                    AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                        let points = sum
                            .data_points()
                            .map(|p| {
                                let mut attrs: Vec<(String, String)> = p
                                    .attributes()
                                    .map(|kv| {
                                        (kv.key.as_str().to_string(), kv.value.as_str().into())
                                    })
                                    .collect();
                                attrs.sort();
                                (attrs, OtlpValue::U64(p.value()))
                            })
                            .collect();
                        out.insert(
                            m.name().to_string(),
                            OtlpMetric {
                                unit: m.unit().to_string(),
                                monotonic: sum.is_monotonic(),
                                temporality: sum.temporality(),
                                start_time: sum.start_time(),
                                time: sum.time(),
                                points,
                            },
                        );
                    }
                    AggregatedMetrics::F64(MetricData::Sum(sum)) => {
                        let points = sum
                            .data_points()
                            .map(|p| {
                                let mut attrs: Vec<(String, String)> = p
                                    .attributes()
                                    .map(|kv| {
                                        (kv.key.as_str().to_string(), kv.value.as_str().into())
                                    })
                                    .collect();
                                attrs.sort();
                                (attrs, OtlpValue::F64(p.value()))
                            })
                            .collect();
                        out.insert(
                            m.name().to_string(),
                            OtlpMetric {
                                unit: m.unit().to_string(),
                                monotonic: sum.is_monotonic(),
                                temporality: sum.temporality(),
                                start_time: sum.start_time(),
                                time: sum.time(),
                                points,
                            },
                        );
                    }
                    _ => {}
                }
            }
        }
        out
    }
}

fn otel_value(
    otlp: &OtlpSnapshot,
    name: &str,
    unit: &str,
    attrs: &[(&str, &str)],
) -> Option<OtlpValue> {
    let metric = otlp.get(name)?;
    assert_eq!(metric.unit, unit, "unit mismatch for {name}");
    assert!(metric.monotonic, "{name} must be monotonic");
    let mut want: Vec<(String, String)> = attrs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    want.sort();
    metric
        .points
        .iter()
        .find_map(|(a, v)| (a == &want).then_some(*v))
}

fn otel_u64(otlp: &OtlpSnapshot, name: &str, unit: &str, attrs: &[(&str, &str)]) -> Option<u64> {
    otel_value(otlp, name, unit, attrs).map(|v| match v {
        OtlpValue::U64(n) => n,
        OtlpValue::F64(n) => panic!("{name} expected U64 sum, got F64({n})"),
    })
}

fn otel_f64(otlp: &OtlpSnapshot, name: &str, unit: &str, attrs: &[(&str, &str)]) -> Option<f64> {
    otel_value(otlp, name, unit, attrs).map(|v| match v {
        OtlpValue::F64(n) => n,
        OtlpValue::U64(n) => panic!("{name} expected F64 sum, got U64({n})"),
    })
}

fn assert_all_three_phase2_parity(st: &Arc<AppState>, harness: &MetricsHarness, label: &str) {
    let prom = crate::obs::render_prometheus(st);
    let json = crate::obs::metrics_json(st);
    let otlp = harness.collect();

    let ds = json["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == label)
        .unwrap_or_else(|| panic!("dataset {label} missing in JSON: {json}"));

    // 1. Produced rows counter (exact u64 across all three formats)
    let rows_produced = ds["rowsProduced"].as_u64().unwrap();
    assert_eq!(
        prom_sample_u64(
            &prom,
            &format!(r#"sparkles_rows_produced_total{{dataset="{label}"}}"#)
        ),
        Some(rows_produced),
        "prom rows_produced mismatch for {label}"
    );
    assert_eq!(
        otel_u64(
            &otlp,
            "sparkles.rows.produced",
            "{row}",
            &[("dataset", label)]
        ),
        Some(rows_produced),
        "otlp rows.produced mismatch for {label}"
    );

    // 2. Query memory peak histogram
    let mem_bounds: Vec<u64> = json["queryMemoryPeakBucketBounds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    assert_eq!(mem_bounds, crate::obs::MEMORY_BUCKETS);
    if ds["queryMemoryPeak"].is_null() {
        assert!(!prom.contains(&format!(
            r#"sparkles_query_memory_peak_bytes_count{{dataset="{label}"}}"#
        )));
        assert_eq!(
            otel_u64(
                &otlp,
                "sparkles.query.memory.peak.count",
                "{query}",
                &[("dataset", label)]
            ),
            None
        );
        assert_eq!(
            otel_u64(
                &otlp,
                "sparkles.query.memory.peak.sum",
                "By",
                &[("dataset", label)]
            ),
            None
        );
        assert_eq!(
            otel_u64(
                &otlp,
                "sparkles.query.memory.peak.bucket",
                "{query}",
                &[("dataset", label), ("le", "+Inf")]
            ),
            None
        );
    } else {
        let peak = &ds["queryMemoryPeak"];
        let count = peak["count"].as_u64().unwrap();
        let sum = peak["sumBytes"].as_u64().unwrap();
        let buckets: Vec<u64> = peak["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        assert_eq!(buckets.len(), mem_bounds.len() + 1);
        assert_eq!(*buckets.last().unwrap(), count);
        assert_eq!(
            prom_sample_u64(
                &prom,
                &format!(r#"sparkles_query_memory_peak_bytes_count{{dataset="{label}"}}"#)
            ),
            Some(count)
        );
        assert_eq!(
            prom_sample_u64(
                &prom,
                &format!(r#"sparkles_query_memory_peak_bytes_sum{{dataset="{label}"}}"#)
            ),
            Some(sum)
        );
        assert_eq!(
            otel_u64(
                &otlp,
                "sparkles.query.memory.peak.count",
                "{query}",
                &[("dataset", label)]
            ),
            Some(count)
        );
        assert_eq!(
            otel_u64(
                &otlp,
                "sparkles.query.memory.peak.sum",
                "By",
                &[("dataset", label)]
            ),
            Some(sum)
        );
        for (i, b) in buckets.iter().enumerate() {
            let le = mem_bounds
                .get(i)
                .map_or("+Inf".to_string(), |v| v.to_string());
            assert_eq!(
                prom_sample_u64(
                    &prom,
                    &format!(
                        r#"sparkles_query_memory_peak_bytes_bucket{{dataset="{label}",le="{le}"}}"#
                    )
                ),
                Some(*b)
            );
            assert_eq!(
                otel_u64(
                    &otlp,
                    "sparkles.query.memory.peak.bucket",
                    "{query}",
                    &[("dataset", label), ("le", &le)]
                ),
                Some(*b),
                "otlp query.memory.peak.bucket mismatch for {label} le={le}"
            );
        }
    }

    // 3. Rebuilds counter + rebuild duration histogram by reason
    let rebuild_bounds: Vec<f64> = json["rebuildDurationBucketBounds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(rebuild_bounds, sparkles::store::REBUILD_BUCKETS);
    for reason in sparkles::store::RebuildReason::ALL {
        let r = reason.as_str();
        let entry = &ds["rebuilds"][r];
        let count = entry["count"].as_u64().unwrap();
        assert_eq!(
            prom_sample_u64(
                &prom,
                &format!(r#"sparkles_rebuilds_total{{dataset="{label}",reason="{r}"}}"#)
            ),
            Some(count)
        );
        assert_eq!(
            otel_u64(
                &otlp,
                "sparkles.rebuilds",
                "{rebuild}",
                &[("dataset", label), ("reason", r)]
            ),
            Some(count),
            "otlp rebuilds mismatch for {label}/{r}"
        );
        if count == 0 {
            assert!(entry["duration"].is_null());
            assert!(!prom.contains(&format!(
                r#"sparkles_rebuild_duration_seconds_count{{dataset="{label}",reason="{r}"}}"#
            )));
            assert_eq!(
                otel_u64(
                    &otlp,
                    "sparkles.rebuild.duration.count",
                    "{rebuild}",
                    &[("dataset", label), ("reason", r)]
                ),
                None
            );
            assert_eq!(
                otel_f64(
                    &otlp,
                    "sparkles.rebuild.duration.sum",
                    "s",
                    &[("dataset", label), ("reason", r)]
                ),
                None
            );
            assert_eq!(
                otel_u64(
                    &otlp,
                    "sparkles.rebuild.duration.bucket",
                    "{rebuild}",
                    &[("dataset", label), ("reason", r), ("le", "+Inf")]
                ),
                None
            );
        } else {
            let dur = &entry["duration"];
            let dcount = dur["count"].as_u64().unwrap();
            let dsum = dur["sumSeconds"].as_f64().unwrap();
            let buckets: Vec<u64> = dur["buckets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap())
                .collect();
            assert_eq!(dcount, count);
            assert_eq!(buckets.len(), rebuild_bounds.len() + 1);
            assert_eq!(*buckets.last().unwrap(), dcount);
            assert_eq!(
                prom_sample_u64(
                    &prom,
                    &format!(
                        r#"sparkles_rebuild_duration_seconds_count{{dataset="{label}",reason="{r}"}}"#
                    )
                ),
                Some(dcount)
            );
            assert_eq!(
                otel_u64(
                    &otlp,
                    "sparkles.rebuild.duration.count",
                    "{rebuild}",
                    &[("dataset", label), ("reason", r)]
                ),
                Some(dcount)
            );
            let psum = prom_sample(
                &prom,
                &format!(
                    r#"sparkles_rebuild_duration_seconds_sum{{dataset="{label}",reason="{r}"}}"#
                ),
            )
            .unwrap();
            let osum = otel_f64(
                &otlp,
                "sparkles.rebuild.duration.sum",
                "s",
                &[("dataset", label), ("reason", r)],
            )
            .unwrap();
            assert!((psum - dsum).abs() <= 1e-9);
            assert!((osum - dsum).abs() <= 1e-9);
            for (i, b) in buckets.iter().enumerate() {
                let le = rebuild_bounds
                    .get(i)
                    .map_or("+Inf".to_string(), |v| v.to_string());
                assert_eq!(
                    prom_sample_u64(
                        &prom,
                        &format!(
                            r#"sparkles_rebuild_duration_seconds_bucket{{dataset="{label}",reason="{r}",le="{le}"}}"#
                        )
                    ),
                    Some(*b)
                );
                assert_eq!(
                    otel_u64(
                        &otlp,
                        "sparkles.rebuild.duration.bucket",
                        "{rebuild}",
                        &[("dataset", label), ("reason", r), ("le", &le)]
                    ),
                    Some(*b),
                    "otlp rebuild.duration.bucket mismatch for {label}/{r} le={le}"
                );
            }
        }
    }
}

#[tokio::test]
async fn phase2_metrics_match_prometheus_json_and_otlp() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        dir.path(),
        StoreOptions {
            bulk_threshold: 1,
            ..Default::default()
        },
        Duration::from_secs(30),
    )
    .unwrap();
    st.hosts = crate::exposure::Hosts::new("127.0.0.1", &["sparql.example".into()]).unwrap();
    let st = Arc::new(st);
    let ds = st.create("ds", DbType::Mem).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    st.attach("empty", DbType::Mem, None).unwrap();
    st.set_phase(Phase::Ready);
    let app = router(st.clone());
    let harness = MetricsHarness::new(&st);

    // 1. Initial state: `ds` has 1 bulk rebuild and 0 queries; `empty` has 0 everything.
    assert_all_three_phase2_parity(&st, &harness, "ds");
    assert_all_three_phase2_parity(&st, &harness, "empty");

    // 2. Run a query and an update: update adds produced rows without observing query_memory_peak.
    let (status, _, _) = call(
        &app,
        HttpRequest::get("/ds/sparql?query=SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(
        &app,
        HttpRequest::post("/ds/update")
            .header("content-type", "application/sparql-update")
            .body(Body::from(
                "DELETE { ?s <http://xmlns.com/foaf/0.1/name> ?n } \
                 WHERE { ?s <http://xmlns.com/foaf/0.1/name> ?n FILTER(?n = \"Bob\") }",
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Probe a nonexistent dataset (`$none`): request series records `$none`, while
    // dataset work/memory series omit `$none` consistently across all three formats.
    let (status, _, _) = call(
        &app,
        HttpRequest::get("/nonexistent/sparql?query=ASK%7B%7D")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_all_three_phase2_parity(&st, &harness, "ds");
    let prom_none = crate::obs::render_prometheus(&st);
    assert!(
        prom_none.contains(r#"sparkles_requests_total{dataset="$none""#),
        "{prom_none}"
    );
    assert!(!prom_none.contains(r#"sparkles_rows_produced_total{dataset="$none"}"#));
    assert!(!prom_none.contains(r#"sparkles_query_memory_peak_bytes_count{dataset="$none"}"#));
    let otlp = harness.collect();
    assert_eq!(
        otel_u64(
            &otlp,
            "sparkles.rows.produced",
            "{row}",
            &[("dataset", "$none")]
        ),
        None
    );
    assert!(
        otel_u64(
            &otlp,
            "sparkles.rows.produced",
            "{row}",
            &[("dataset", "ds")]
        )
        .unwrap()
            >= 3
    );
    assert_eq!(
        otel_u64(
            &otlp,
            "sparkles.query.memory.peak.count",
            "{query}",
            &[("dataset", "ds")]
        ),
        Some(1)
    );

    // 3. Failed bulk load, dry-run bulk load, and cancelled compaction do not change counts.
    assert!(
        ds.store
            .load(&[Source::from_bytes(
                b"invalid".to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .is_err()
    );
    let bulk_nt: String = (0..20)
        .map(|i| format!("<http://example.org/b{i}> <http://example.org/p> \"{i}\" .\n"))
        .collect();
    let dry = sparkles::guard::WriteOptions {
        dry_run: Some(sparkles::preview::DryRun::default()),
        ..Default::default()
    };
    assert!(matches!(
        ds.store.load_with(
            &[Source::from_bytes(
                bulk_nt.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )],
            sparkles::commit::CommitKind::Load,
            &dry,
        ),
        Err(sparkles::Error::DryRun(_))
    ));
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
    assert!(matches!(
        ds.store.compact_with(&sparkles::store::CompactOptions {
            cancel: Some(cancel),
            ..Default::default()
        }),
        Err(sparkles::Error::Cancelled)
    ));
    assert_all_three_phase2_parity(&st, &harness, "ds");
    let otlp = harness.collect();
    assert_eq!(
        otel_u64(
            &otlp,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "ds"), ("reason", "bulk")]
        ),
        Some(1)
    );
    assert_eq!(
        otel_u64(
            &otlp,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "ds"), ("reason", "compact")]
        ),
        Some(0)
    );

    // 4. Successful compaction + second bulk commit.
    ds.store.compact().unwrap();
    ds.store
        .load(&[Source::from_bytes(
            bulk_nt.into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    assert_all_three_phase2_parity(&st, &harness, "ds");
    let otlp = harness.collect();
    let rows_before_rename = otel_u64(
        &otlp,
        "sparkles.rows.produced",
        "{row}",
        &[("dataset", "ds")],
    )
    .unwrap();
    assert_eq!(
        otel_u64(
            &otlp,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "ds"), ("reason", "compact")]
        ),
        Some(1)
    );
    assert_eq!(
        otel_u64(
            &otlp,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "ds"), ("reason", "bulk")]
        ),
        Some(2)
    );

    // 4b. Rename `ds` -> `ds_renamed` moves all Phase 2 counters and histograms cleanly.
    drop(ds);
    st.rename("ds", "ds_renamed").unwrap();
    assert_all_three_phase2_parity(&st, &harness, "ds_renamed");
    let otlp_renamed = harness.collect();
    assert_eq!(
        otel_u64(
            &otlp_renamed,
            "sparkles.rows.produced",
            "{row}",
            &[("dataset", "ds")]
        ),
        None
    );
    assert_eq!(
        otel_u64(
            &otlp_renamed,
            "sparkles.rows.produced",
            "{row}",
            &[("dataset", "ds_renamed")]
        ),
        Some(rows_before_rename)
    );
    assert_eq!(
        otel_u64(
            &otlp_renamed,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "ds_renamed"), ("reason", "bulk")]
        ),
        Some(2)
    );
    assert_eq!(
        otel_u64(
            &otlp_renamed,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "ds_renamed"), ("reason", "compact")]
        ),
        Some(1)
    );

    // 5. Delete & recreate `ds_renamed` removes old OTLP data points and resets counters/histograms.
    let (status, _, _) = call(
        &app,
        HttpRequest::delete("/$/datasets/ds_renamed")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let otlp_deleted = harness.collect();
    assert_eq!(
        otel_u64(
            &otlp_deleted,
            "sparkles.rows.produced",
            "{row}",
            &[("dataset", "ds_renamed")]
        ),
        None
    );
    assert_eq!(
        otel_u64(
            &otlp_deleted,
            "sparkles.query.memory.peak.count",
            "{query}",
            &[("dataset", "ds_renamed")]
        ),
        None
    );
    assert_eq!(
        otel_u64(
            &otlp_deleted,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "ds_renamed"), ("reason", "bulk")]
        ),
        None
    );
    let (status, _, _) = call(
        &app,
        HttpRequest::post("/$/datasets")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"dbName":"ds_renamed","dbType":"mem"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_all_three_phase2_parity(&st, &harness, "ds_renamed");
    let otlp_recreated = harness.collect();
    assert_eq!(
        otel_u64(
            &otlp_recreated,
            "sparkles.rows.produced",
            "{row}",
            &[("dataset", "ds_renamed")]
        ),
        Some(0)
    );
    assert_eq!(
        otel_u64(
            &otlp_recreated,
            "sparkles.query.memory.peak.count",
            "{query}",
            &[("dataset", "ds_renamed")]
        ),
        None
    );
    assert_eq!(
        otel_u64(
            &otlp_recreated,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "ds_renamed"), ("reason", "bulk")]
        ),
        Some(0)
    );

    // 6. Dataset-label cap (`max_datasets = 1`): `$other` aggregates identically in all 3 formats.
    let cap_dir = tempfile::tempdir().unwrap();
    let mut cap_st = AppState::new(
        cap_dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
    )
    .unwrap();
    cap_st.metrics = crate::obs::Metrics::new(true, 1);
    let cap_st = Arc::new(cap_st);
    for name in ["a", "b", "c"] {
        let d = cap_st.attach(name, DbType::Mem, None).unwrap();
        d.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        d.store.compact().unwrap();
    }
    cap_st.set_phase(Phase::Ready);
    let cap_app = router(cap_st.clone());
    let cap_harness = MetricsHarness::new(&cap_st);
    for name in ["a", "b", "c"] {
        let (status, _, _) = call(
            &cap_app,
            HttpRequest::get(format!(
                "/{name}/sparql?query=SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D"
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    assert_all_three_phase2_parity(&cap_st, &cap_harness, "a");
    assert_all_three_phase2_parity(&cap_st, &cap_harness, "$other");
    let cap_otlp = cap_harness.collect();
    assert_eq!(
        otel_u64(
            &cap_otlp,
            "sparkles.rows.produced",
            "{row}",
            &[("dataset", "b")]
        ),
        None
    );
    assert_eq!(
        otel_u64(
            &cap_otlp,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(2)
    );
    assert_eq!(
        otel_u64(
            &cap_otlp,
            "sparkles.query.memory.peak.count",
            "{query}",
            &[("dataset", "$other")]
        ),
        Some(2)
    );

    // 7. Reopening a persistent dataset resets rebuild counters and duration histograms in all 3 formats.
    let disk_dir = tempfile::tempdir().unwrap();
    {
        let st1 = Arc::new(
            AppState::new(
                disk_dir.path(),
                StoreOptions::default(),
                Duration::from_secs(30),
            )
            .unwrap(),
        );
        let d = st1.create("disk", DbType::Persistent).unwrap();
        d.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        d.store.compact().unwrap();
        st1.set_phase(Phase::Ready);
        let h1 = MetricsHarness::new(&st1);
        assert_all_three_phase2_parity(&st1, &h1, "disk");
        let o1 = h1.collect();
        assert_eq!(
            otel_u64(
                &o1,
                "sparkles.rebuilds",
                "{rebuild}",
                &[("dataset", "disk"), ("reason", "bulk")]
            ),
            Some(1)
        );
        assert_eq!(
            otel_u64(
                &o1,
                "sparkles.rebuilds",
                "{rebuild}",
                &[("dataset", "disk"), ("reason", "compact")]
            ),
            Some(1)
        );
    }
    {
        let st2 = Arc::new(
            AppState::new(
                disk_dir.path(),
                StoreOptions::default(),
                Duration::from_secs(30),
            )
            .unwrap(),
        );
        st2.set_phase(Phase::Ready);
        let h2 = MetricsHarness::new(&st2);
        assert_all_three_phase2_parity(&st2, &h2, "disk");
        let o2 = h2.collect();
        assert_eq!(
            otel_u64(
                &o2,
                "sparkles.rebuilds",
                "{rebuild}",
                &[("dataset", "disk"), ("reason", "bulk")]
            ),
            Some(0)
        );
        assert_eq!(
            otel_u64(
                &o2,
                "sparkles.rebuilds",
                "{rebuild}",
                &[("dataset", "disk"), ("reason", "compact")]
            ),
            Some(0)
        );
        assert_eq!(
            otel_u64(
                &o2,
                "sparkles.rebuild.duration.count",
                "{rebuild}",
                &[("dataset", "disk"), ("reason", "bulk")]
            ),
            None
        );
    }
}

#[tokio::test]
async fn phase2_otlp_cardinality_limit_at_default_cap() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        dir.path(),
        StoreOptions {
            bulk_threshold: 1,
            ..Default::default()
        },
        Duration::from_secs(30),
    )
    .unwrap();
    st.metrics = crate::obs::Metrics::new(true, crate::obs::DEFAULT_METRICS_MAX_DATASETS);
    let st = Arc::new(st);
    // 75 datasets × 2 reasons × 14 buckets = 2,100 attribute sets (> SDK default 2,000),
    // well within Sparkles' default `--metrics-max-datasets=100`.
    let n_datasets = 75usize;
    let tiny = b"<http://example.org/s> <http://example.org/p> \"1\" .\n";
    for i in 0..n_datasets {
        let name = format!("d{i:02}");
        let d = st.attach(&name, DbType::Mem, None).unwrap();
        d.store
            .load(&[Source::from_bytes(
                tiny.to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        d.store.compact().unwrap();
    }
    st.set_phase(Phase::Ready);
    let harness = MetricsHarness::new(&st);
    let otlp = harness.collect();
    let bucket_metric = otlp
        .get("sparkles.rebuild.duration.bucket")
        .expect("rebuild duration bucket metric");
    assert!(
        !bucket_metric
            .points
            .iter()
            .any(|(attrs, _)| attrs.iter().any(|(k, _)| k == "otel.metric.overflow")),
        "unexpected otel.metric.overflow on sparkles.rebuild.duration.bucket"
    );
    let expected_points = n_datasets * 2 * (sparkles::store::REBUILD_BUCKETS.len() + 1);
    assert_eq!(bucket_metric.points.len(), expected_points);
    for i in 0..n_datasets {
        let name = format!("d{i:02}");
        for reason in ["compact", "bulk"] {
            let cnt = otel_u64(
                &otlp,
                "sparkles.rebuild.duration.count",
                "{rebuild}",
                &[("dataset", &name), ("reason", reason)],
            );
            let inf = otel_u64(
                &otlp,
                "sparkles.rebuild.duration.bucket",
                "{rebuild}",
                &[("dataset", &name), ("reason", reason), ("le", "+Inf")],
            );
            assert_eq!(cnt, Some(1), "missing count for {name}/{reason}");
            assert_eq!(inf, cnt, "+Inf mismatch for {name}/{reason}");
        }
    }
    // Verify scaling for `sparkles.query.memory.peak.bucket` above `--metrics-max-datasets=222`.
    assert_eq!(
        sdk::stream_cardinality_limit("sparkles.query.memory.peak.bucket", 250),
        Some(252 * (crate::obs::MEMORY_BUCKETS.len() + 1))
    );
}

#[tokio::test]
async fn phase2_otlp_delta_and_cumulative_monotonic_on_other_delete_and_same_cycle_recreate() {
    // Part A: `$other` deletion, final overflow deletion, new overflow arrival, and promotion
    // under both Cumulative and Delta temporalities.
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        dir.path(),
        StoreOptions {
            bulk_threshold: 1,
            ..Default::default()
        },
        Duration::from_secs(30),
    )
    .unwrap();
    st.metrics = crate::obs::Metrics::new(true, 1);
    let st = Arc::new(st);
    for name in ["a", "b", "c"] {
        let d = st.create(name, DbType::Mem).unwrap();
        d.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        d.store.compact().unwrap();
    }
    st.set_phase(Phase::Ready);
    let app = router(st.clone());
    let cum_h = MetricsHarness::with_temporality(&st, Temporality::Cumulative);
    let delta_h = MetricsHarness::with_temporality(&st, Temporality::Delta);

    for name in ["a", "b", "c"] {
        let (status, _, _) = call(
            &app,
            HttpRequest::get(format!(
                "/{name}/sparql?query=SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D"
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let d1 = delta_h.collect();
    assert_eq!(
        d1.get("sparkles.rebuilds").unwrap().temporality,
        Temporality::Delta
    );
    assert_eq!(
        otel_u64(
            &d1,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(2)
    );

    // Delete one overflow dataset (`b`): `$other` must NOT shrink or emit negative delta.
    assert!(st.delete("b").unwrap());
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let d2 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &d2,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(0)
    );
    assert_eq!(
        otel_u64(
            &d2,
            "sparkles.rebuild.duration.count",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(0)
    );
    assert_eq!(
        otel_u64(
            &d2,
            "sparkles.rebuild.duration.bucket",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk"), ("le", "+Inf")]
        ),
        Some(0)
    );
    assert!(
        otel_f64(
            &d2,
            "sparkles.rebuild.duration.sum",
            "s",
            &[("dataset", "$other"), ("reason", "bulk")]
        )
        .unwrap()
            >= 0.0
    );

    // Surviving overflow dataset `c` publishes another bulk rebuild + query.
    {
        let c = st.get("c").unwrap();
        let bulk_nt: String = (0..5)
            .map(|i| format!("<http://example.org/x{i}> <http://example.org/p> \"{i}\" .\n"))
            .collect();
        c.store
            .load(&[Source::from_bytes(
                bulk_nt.into_bytes(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    let (status, _, _) = call(
        &app,
        HttpRequest::get("/c/sparql?query=SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let d3 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &d3,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );
    assert_eq!(
        otel_u64(
            &d3,
            "sparkles.query.memory.peak.count",
            "{query}",
            &[("dataset", "$other")]
        ),
        Some(1)
    );

    // Delete final overflow dataset (`c`): `$other` remains present in all three formats
    // with zero delta, and a later overflow dataset `d` adds only its own increment.
    assert!(st.delete("c").unwrap());
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let d4 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &d4,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(0)
    );
    assert_eq!(
        otel_u64(
            &d4,
            "sparkles.rows.produced",
            "{row}",
            &[("dataset", "$other")]
        ),
        Some(0)
    );
    {
        let d = st.create("d", DbType::Mem).unwrap();
        d.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    let (status, _, _) = call(
        &app,
        HttpRequest::get("/d/sparql?query=SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let d5 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &d5,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );
    assert_eq!(
        otel_u64(
            &d5,
            "sparkles.query.memory.peak.count",
            "{query}",
            &[("dataset", "$other")]
        ),
        Some(1)
    );

    // Delete named slot owner `a`: `d` promotes to named slot `"d"` at 0 while `$other`
    // retains its totals with 0 delta.
    assert!(st.delete("a").unwrap());
    assert_all_three_phase2_parity(&st, &cum_h, "d");
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let d6 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &d6,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(0)
    );
    assert_eq!(
        otel_u64(
            &d6,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "d"), ("reason", "bulk")]
        ),
        Some(0)
    );

    // Part B: Named dataset delete & recreate BETWEEN collections (no absent cycle) under
    // both Delta and Cumulative temporalities.
    let dir2 = tempfile::tempdir().unwrap();
    let st2 = Arc::new(
        AppState::new(
            dir2.path(),
            StoreOptions {
                bulk_threshold: 1,
                ..Default::default()
            },
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    {
        let tmp = st2.create("tmp", DbType::Mem).unwrap();
        for batch in 0..2 {
            let nt: String = (0..5)
                .map(|i| format!("<http://ex/b{batch}_{i}> <http://ex/p> \"{i}\" .\n"))
                .collect();
            tmp.store
                .load(&[Source::from_bytes(
                    nt.into_bytes(),
                    oxrdfio::RdfFormat::NTriples,
                    None,
                )])
                .unwrap();
        }
        tmp.store.compact().unwrap();
    }
    st2.set_phase(Phase::Ready);
    let app2 = router(st2.clone());
    let cum2 = MetricsHarness::with_temporality(&st2, Temporality::Cumulative);
    let delta2 = MetricsHarness::with_temporality(&st2, Temporality::Delta);

    let (status, _, _) = call(
        &app2,
        HttpRequest::get("/tmp/sparql?query=SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let c1 = cum2.collect();
    let dl1 = delta2.collect();
    let c1_rows = otel_u64(
        &c1,
        "sparkles.rows.produced",
        "{row}",
        &[("dataset", "tmp")],
    )
    .unwrap();
    assert_eq!(
        otel_u64(
            &dl1,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk")]
        ),
        Some(2)
    );

    // Delete & immediately recreate `tmp` untouched (no collect while absent).
    assert!(st2.delete("tmp").unwrap());
    st2.create("tmp", DbType::Mem).unwrap();
    let c2 = cum2.collect();
    let dl2 = delta2.collect();
    assert_eq!(
        otel_u64(
            &dl2,
            "sparkles.rows.produced",
            "{row}",
            &[("dataset", "tmp")]
        ),
        Some(0)
    );
    assert_eq!(
        otel_u64(
            &dl2,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk")]
        ),
        Some(0)
    );
    assert_eq!(
        otel_u64(
            &dl2,
            "sparkles.rebuild.duration.count",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk")]
        ),
        Some(0)
    );
    assert!(
        otel_f64(
            &dl2,
            "sparkles.rebuild.duration.sum",
            "s",
            &[("dataset", "tmp"), ("reason", "bulk")]
        )
        .unwrap()
            >= 0.0
    );
    assert_eq!(
        otel_u64(
            &c2,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk")]
        ),
        Some(2)
    );
    assert_eq!(
        otel_u64(
            &c2,
            "sparkles.rebuild.duration.bucket",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk"), ("le", "+Inf")]
        ),
        Some(2)
    );

    // Delete & immediately recreate `tmp` with 1 bulk rebuild + 1 query before next collect.
    assert!(st2.delete("tmp").unwrap());
    {
        let tmp = st2.create("tmp", DbType::Mem).unwrap();
        tmp.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    let (status, _, _) = call(
        &app2,
        HttpRequest::get("/tmp/sparql?query=SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let c3 = cum2.collect();
    let dl3 = delta2.collect();
    assert_eq!(
        otel_u64(
            &dl3,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk")]
        ),
        Some(1)
    );
    assert_eq!(
        otel_u64(
            &dl3,
            "sparkles.rebuild.duration.count",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk")]
        ),
        Some(1)
    );
    assert_eq!(
        otel_u64(
            &dl3,
            "sparkles.rebuild.duration.bucket",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk"), ("le", "+Inf")]
        ),
        Some(1)
    );
    assert!(
        otel_f64(
            &dl3,
            "sparkles.rebuild.duration.sum",
            "s",
            &[("dataset", "tmp"), ("reason", "bulk")]
        )
        .unwrap()
            > 0.0
    );
    assert_eq!(
        otel_u64(
            &dl3,
            "sparkles.query.memory.peak.count",
            "{query}",
            &[("dataset", "tmp")]
        ),
        Some(1)
    );
    assert!(
        otel_u64(
            &dl3,
            "sparkles.rows.produced",
            "{row}",
            &[("dataset", "tmp")]
        )
        .unwrap()
            > 0
    );
    assert_eq!(
        otel_u64(
            &c3,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk")]
        ),
        Some(3)
    );
    assert_eq!(
        otel_u64(
            &c3,
            "sparkles.rebuild.duration.bucket",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk"), ("le", "+Inf")]
        ),
        Some(3)
    );
    assert!(
        otel_u64(
            &c3,
            "sparkles.rows.produced",
            "{row}",
            &[("dataset", "tmp")]
        )
        .unwrap()
            > c1_rows
    );

    // Delete `tmp`, collect its absence, then recreate: both Delta and Cumulative restart
    // at 0 to match Prometheus and JSON. Delta starts a fresh accumulation interval while
    // Cumulative retains the SDK's instrument-level start time.
    let c1_start = c1.get("sparkles.rebuilds").unwrap().start_time;
    assert_eq!(c2.get("sparkles.rebuilds").unwrap().start_time, c1_start);
    assert_eq!(c3.get("sparkles.rebuilds").unwrap().start_time, c1_start);
    assert!(st2.delete("tmp").unwrap());
    let c4 = cum2.collect();
    let dl4 = delta2.collect();
    let dl4_time = dl4
        .get("sparkles.rebuilds")
        .map(|m| m.time)
        .unwrap_or_else(|| dl3.get("sparkles.rebuilds").unwrap().time);
    assert_eq!(
        otel_u64(
            &c4,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk")]
        ),
        None
    );
    assert_eq!(
        otel_u64(
            &dl4,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk")]
        ),
        None
    );
    st2.create("tmp", DbType::Mem).unwrap();
    let c5 = cum2.collect();
    let dl5 = delta2.collect();
    assert_eq!(
        otel_u64(
            &c5,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk")]
        ),
        Some(0)
    );
    assert_eq!(
        otel_u64(
            &dl5,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk")]
        ),
        Some(0)
    );
    assert_eq!(c5.get("sparkles.rebuilds").unwrap().start_time, c1_start);
    assert!(dl5.get("sparkles.rebuilds").unwrap().start_time >= dl4_time);
    // Subsequent work after the observed-absence reset increments cleanly from 0.
    {
        let tmp = st2.get("tmp").unwrap();
        tmp.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    let c6 = cum2.collect();
    let dl6 = delta2.collect();
    assert_eq!(
        otel_u64(
            &c6,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk")]
        ),
        Some(1)
    );
    assert_eq!(
        otel_u64(
            &dl6,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "tmp"), ("reason", "bulk")]
        ),
        Some(1)
    );
}

#[tokio::test]
async fn phase2_otlp_snapshot_refreshes_when_index_zero_dropped_and_out_of_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        dir.path(),
        StoreOptions {
            bulk_threshold: 1,
            ..Default::default()
        },
        Duration::from_secs(30),
    )
    .unwrap();
    // Keep `max_datasets` below the `sparkles.requests` cardinality-view threshold
    // (`3 * 108 = 324 <= 2000`) so the Drop view is the sole view matching index 0.
    st.metrics = crate::obs::Metrics::new(true, 1);
    let st = Arc::new(st);
    let ds = st.attach("ds", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    st.set_phase(Phase::Ready);
    let app = router(st.clone());

    // Drop `sparkles.requests` (callback index 0) via an SDK View.
    let harness = MetricsHarness::with_builder(&st, Temporality::Cumulative, |b| {
        b.with_view(|inst: &Instrument| {
            if inst.name() == "sparkles.requests" {
                Stream::builder()
                    .with_aggregation(Aggregation::Drop)
                    .build()
                    .ok()
            } else {
                None
            }
        })
    });
    let o1 = harness.collect();
    assert!(!o1.contains_key("sparkles.requests"));
    assert_eq!(
        otel_u64(
            &o1,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "ds"), ("reason", "compact")]
        ),
        Some(0)
    );

    // Mutate state and record a real HTTP request; `sparkles.requests` must stay absent
    // (confirming callback index 0 was truly dropped) while later callbacks refresh.
    ds.store.compact().unwrap();
    let (status, _, _) = call(
        &app,
        HttpRequest::get("/ds/sparql?query=SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let o2 = harness.collect();
    assert!(
        !o2.contains_key("sparkles.requests"),
        "sparkles.requests callback was not actually dropped"
    );
    assert!(
        o2.contains_key("sparkles.response.size"),
        "expected HTTP request to record non-dropped request metrics"
    );
    assert_eq!(
        otel_u64(
            &o2,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "ds"), ("reason", "compact")]
        ),
        Some(1)
    );
    assert_eq!(
        otel_u64(
            &o2,
            "sparkles.query.memory.peak.count",
            "{query}",
            &[("dataset", "ds")]
        ),
        Some(1)
    );

    // Direct unit check on `sdk::Snapshot` with reversed callback order across cycles.
    let snap = sdk::Snapshot::new(st.clone());
    let a = snap.get(10);
    let b = snap.get(5);
    let c = snap.get(2);
    assert!(Arc::ptr_eq(&a, &b));
    assert!(Arc::ptr_eq(&a, &c));
    // First repeated callback (in any order) starts a new cycle and refreshes the snapshot.
    let d = snap.get(5);
    let e = snap.get(10);
    assert!(!Arc::ptr_eq(&a, &d));
    assert!(Arc::ptr_eq(&d, &e));
}

#[tokio::test]
async fn phase2_otlp_preserves_u64_precision_above_f64_limit() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    st.attach("ds", DbType::Mem, None).unwrap();
    st.set_phase(Phase::Ready);
    let harness = MetricsHarness::new(&st);

    let two_to_53: u64 = 1u64 << 53; // 9_007_199_254_740_992
    st.metrics.record_test_work("ds", two_to_53, two_to_53);
    assert_all_three_phase2_parity(&st, &harness, "ds");
    let o1 = harness.collect();
    assert_eq!(
        otel_u64(&o1, "sparkles.rows.produced", "{row}", &[("dataset", "ds")]),
        Some(two_to_53)
    );
    assert_eq!(
        otel_u64(
            &o1,
            "sparkles.query.memory.peak.sum",
            "By",
            &[("dataset", "ds")]
        ),
        Some(two_to_53)
    );

    // +1 crosses the exact `f64` integer precision boundary (9_007_199_254_740_993).
    st.metrics.record_test_work("ds", 1, 1);
    assert_all_three_phase2_parity(&st, &harness, "ds");
    let o2 = harness.collect();
    assert_eq!(
        otel_u64(&o2, "sparkles.rows.produced", "{row}", &[("dataset", "ds")]),
        Some(two_to_53 + 1)
    );
    assert_eq!(
        otel_u64(
            &o2,
            "sparkles.query.memory.peak.sum",
            "By",
            &[("dataset", "ds")]
        ),
        Some(two_to_53 + 1)
    );
}

#[test]
fn phase2_otlp_monotonic_continuation_clamps_dips_without_gen_change() {
    use std::collections::BTreeMap;
    let mut monotonic: BTreeMap<String, sdk::DatasetMonotonic> = BTreeMap::new();
    let make_snap = |rows: u64,
                     peak_cnt: u64,
                     peak_sum: u64,
                     bulk_cnt: u64,
                     bulk_sum: f64|
     -> crate::obs::OtelMetricsSnapshot {
        let mut gens = BTreeMap::new();
        gens.insert("$other".to_string(), (1, 0));
        crate::obs::OtelMetricsSnapshot {
            json: serde_json::json!({
                "datasets": [{
                    "name": "$other",
                    "rowsProduced": rows,
                    "queryMemoryPeak": {
                        "count": peak_cnt,
                        "sumBytes": peak_sum,
                        "buckets": [peak_cnt, peak_cnt]
                    },
                    "rebuilds": {
                        "compact": { "count": 0, "duration": null },
                        "bulk": {
                            "count": bulk_cnt,
                            "duration": {
                                "count": bulk_cnt,
                                "sumSeconds": bulk_sum,
                                "buckets": [bulk_cnt, bulk_cnt]
                            }
                        }
                    }
                }]
            }),
            dataset_gens: gens,
        }
    };

    let mut snap1 = make_snap(10, 2, 2048, 2, 0.5);
    sdk::apply_monotonic_continuation(&mut snap1, &mut monotonic);

    // Same `(series_id, instance_id)` with dipped `raw_peak` and `raw_rebuilds`
    // (including a 1-ULP floating-point reassociation dip on `sumSeconds`).
    let dipped_sum = f64::from_bits(0.5f64.to_bits() - 1);
    assert!(dipped_sum < 0.5);
    let mut snap2 = make_snap(10, 1, 1024, 1, dipped_sum);
    sdk::apply_monotonic_continuation(&mut snap2, &mut monotonic);

    let d = &snap2.json["datasets"][0];
    assert_eq!(d["rowsProduced"], 10);
    assert_eq!(d["queryMemoryPeak"]["count"], 2);
    assert_eq!(d["queryMemoryPeak"]["sumBytes"], 2048);
    assert_eq!(d["queryMemoryPeak"]["buckets"], serde_json::json!([2, 2]));
    assert_eq!(d["rebuilds"]["bulk"]["count"], 2);
    assert_eq!(d["rebuilds"]["bulk"]["duration"]["count"], 2);
    assert_eq!(
        d["rebuilds"]["bulk"]["duration"]["buckets"],
        serde_json::json!([2, 2])
    );
    assert_eq!(
        d["rebuilds"]["bulk"]["duration"]["sumSeconds"].as_f64(),
        Some(0.5)
    );
}

#[tokio::test]
async fn phase2_otlp_concurrent_delete_and_promotion_never_drop_or_double_count() {
    use std::sync::mpsc;

    // Part A: Collection paused inside `delete`'s `leaving` window via `delete_hook`.
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        dir.path(),
        StoreOptions {
            bulk_threshold: 1,
            ..Default::default()
        },
        Duration::from_secs(30),
    )
    .unwrap();
    st.metrics = crate::obs::Metrics::new(true, 1);
    let st = Arc::new(st);
    st.create("a", DbType::Mem).unwrap();
    {
        let b = st.create("b", DbType::Persistent).unwrap();
        b.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    st.set_phase(Phase::Ready);
    let app = router(st.clone());
    let (status, _, _) = call(
        &app,
        HttpRequest::get("/a/sparql?query=ASK%7B%7D")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cum_h = MetricsHarness::with_temporality(&st, Temporality::Cumulative);
    let delta_h = MetricsHarness::with_temporality(&st, Temporality::Delta);

    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let c1 = cum_h.collect();
    let d1 = delta_h.collect();
    let sum_before = otel_f64(
        &c1,
        "sparkles.rebuild.duration.sum",
        "s",
        &[("dataset", "$other"), ("reason", "bulk")],
    )
    .unwrap();
    assert_eq!(
        otel_u64(
            &d1,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );

    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    crate::state::tests::set_delete_hook("b", move || {
        entered.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(15)).unwrap();
    });
    let deleter = {
        let st = st.clone();
        std::thread::spawn(move || st.delete("b").unwrap())
    };
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(st.get("b").is_none(), "b must be hidden while leaving");

    // Scrape Prometheus, JSON, OTLP Cumulative, and OTLP Delta while `b` is leaving.
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let c_mid = cum_h.collect();
    let d_mid = delta_h.collect();
    assert_eq!(
        otel_u64(
            &c_mid,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );
    assert_eq!(
        otel_u64(
            &c_mid,
            "sparkles.rebuild.duration.count",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );
    assert_eq!(
        otel_u64(
            &c_mid,
            "sparkles.rebuild.duration.bucket",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk"), ("le", "+Inf")]
        ),
        Some(1)
    );
    assert!(
        (otel_f64(
            &c_mid,
            "sparkles.rebuild.duration.sum",
            "s",
            &[("dataset", "$other"), ("reason", "bulk")]
        )
        .unwrap()
            - sum_before)
            .abs()
            < 1e-12
    );
    assert_eq!(
        otel_u64(
            &d_mid,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(0)
    );
    assert_eq!(
        otel_u64(
            &d_mid,
            "sparkles.rebuild.duration.count",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(0)
    );
    assert_eq!(
        otel_u64(
            &d_mid,
            "sparkles.rebuild.duration.bucket",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk"), ("le", "+Inf")]
        ),
        Some(0)
    );
    assert!(
        otel_f64(
            &d_mid,
            "sparkles.rebuild.duration.sum",
            "s",
            &[("dataset", "$other"), ("reason", "bulk")]
        )
        .unwrap()
            >= 0.0
    );

    release.send(()).unwrap();
    assert!(deleter.join().unwrap());
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let d_after = delta_h.collect();
    assert_eq!(
        otel_u64(
            &d_after,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(0)
    );

    // Part B: Overlapping scrape during promotion of overflow dataset `c` after `a` is deleted.
    {
        let c = st.create("c", DbType::Mem).unwrap();
        c.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let c_pre = cum_h.collect();
    let d_pre = delta_h.collect();
    assert_eq!(
        otel_u64(
            &c_pre,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(2)
    );
    assert_eq!(
        otel_u64(
            &d_pre,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );

    let (s1_entered, s1_entered_rx) = mpsc::channel();
    let (s1_release, s1_release_rx) = mpsc::channel::<()>();
    let scrape1 = {
        let st = st.clone();
        std::thread::spawn(move || {
            crate::obs::set_gauges_after_datasets_hook(&st, move || {
                s1_entered.send(()).unwrap();
                s1_release_rx.recv_timeout(Duration::from_secs(15)).unwrap();
            });
            crate::obs::metrics_json(&st)
        })
    };
    s1_entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();

    // While Scrape 1 holds `[a, c]` from `st.datasets()`, delete `a` and run Scrape 2
    // to promote `c` into the freed named slot and retire `c`'s rebuilds into `$other`.
    assert!(st.delete("a").unwrap());
    assert_all_three_phase2_parity(&st, &cum_h, "c");
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let d_promo = delta_h.collect();
    assert_eq!(
        otel_u64(
            &d_promo,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(0)
    );

    s1_release.send(()).unwrap();
    let j1 = scrape1.join().unwrap();
    let j1_other = j1["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "$other")
        .unwrap();
    assert_eq!(
        j1_other["rebuilds"]["bulk"]["count"], 2,
        "overlapping scrape double-counted promoted overflow rebuilds: {j1_other}"
    );
    assert_eq!(j1_other["rebuilds"]["bulk"]["duration"]["count"], 2);
    assert_eq!(
        j1_other["rebuilds"]["bulk"]["duration"]["buckets"]
            .as_array()
            .unwrap()
            .last()
            .and_then(|v| v.as_u64()),
        Some(2)
    );
    let j1_c = j1["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "c")
        .unwrap();
    assert_eq!(j1_c["rebuilds"]["bulk"]["count"], 0);
}

#[tokio::test]
async fn phase2_otlp_persistent_rename_failed_rename_and_memory_roundtrip() {
    // Part A: Persistent overflow dataset rename (`b -> c`) conserves `$other` rebuilds.
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        dir.path(),
        StoreOptions {
            bulk_threshold: 1,
            ..Default::default()
        },
        Duration::from_secs(30),
    )
    .unwrap();
    st.metrics = crate::obs::Metrics::new(true, 1);
    let st = Arc::new(st);
    st.create("a", DbType::Mem).unwrap();
    {
        let b = st.create("b", DbType::Persistent).unwrap();
        b.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    st.set_phase(Phase::Ready);
    let app = router(st.clone());
    let (status, _, _) = call(
        &app,
        HttpRequest::get("/a/sparql?query=ASK%7B%7D")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cum_h = MetricsHarness::with_temporality(&st, Temporality::Cumulative);
    let delta_h = MetricsHarness::with_temporality(&st, Temporality::Delta);

    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let d1 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &d1,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );

    // Rename persistent overflow `b -> c` (reopens store at 0). `$other` must stay 1.
    st.rename("b", "c").unwrap();
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let c2 = cum_h.collect();
    let d2 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &c2,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );
    assert_eq!(
        otel_u64(
            &d2,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(0)
    );

    // A new rebuild on reopened `c` increments `$other` cleanly from 1 to 2.
    {
        let c = st.get("c").unwrap();
        let nt: String = (0..5)
            .map(|i| format!("<http://ex/c{i}> <http://ex/p> \"{i}\" .\n"))
            .collect();
        c.store
            .load(&[Source::from_bytes(
                nt.into_bytes(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let d3 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &d3,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );

    // Part B: Promote persistent `c` to named slot (sets `promoted_offset = 1`), then
    // rename persistent `c -> d`. Reopened `d` must NOT inherit stale `promoted_offset`.
    assert!(st.delete("a").unwrap());
    assert_all_three_phase2_parity(&st, &cum_h, "c");
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let _ = delta_h.collect();
    st.rename("c", "d").unwrap();
    assert_all_three_phase2_parity(&st, &cum_h, "d");
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let _ = delta_h.collect();
    {
        let d = st.get("d").unwrap();
        let nt: String = (0..5)
            .map(|i| format!("<http://ex/d{i}> <http://ex/p> \"{i}\" .\n"))
            .collect();
        d.store
            .load(&[Source::from_bytes(
                nt.into_bytes(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    assert_all_three_phase2_parity(&st, &cum_h, "d");
    let c4 = cum_h.collect();
    let d4 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &c4,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "d"), ("reason", "bulk")]
        ),
        Some(1),
        "persistent rename must not subtract stale promoted_offset from reopened store"
    );
    assert_eq!(
        otel_u64(
            &d4,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "d"), ("reason", "bulk")]
        ),
        Some(1)
    );

    // Part C: Failed rename (409 Conflict target exists) and memory `m1 -> m2 -> m1`
    // roundtrip must preserve `open_id` and never double-count OTLP rebuilds.
    let dir2 = tempfile::tempdir().unwrap();
    let st2 = Arc::new(
        AppState::new(
            dir2.path(),
            StoreOptions {
                bulk_threshold: 1,
                ..Default::default()
            },
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    {
        let m1 = st2.create("m1", DbType::Mem).unwrap();
        for batch in 0..2 {
            let nt: String = (0..5)
                .map(|i| format!("<http://ex/m{batch}_{i}> <http://ex/p> \"{i}\" .\n"))
                .collect();
            m1.store
                .load(&[Source::from_bytes(
                    nt.into_bytes(),
                    oxrdfio::RdfFormat::NTriples,
                    None,
                )])
                .unwrap();
        }
    }
    st2.create("m2", DbType::Mem).unwrap();
    st2.set_phase(Phase::Ready);
    let cum2 = MetricsHarness::with_temporality(&st2, Temporality::Cumulative);
    let delta2 = MetricsHarness::with_temporality(&st2, Temporality::Delta);

    assert_all_three_phase2_parity(&st2, &cum2, "m1");
    let dl1 = delta2.collect();
    assert_eq!(
        otel_u64(
            &dl1,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "m1"), ("reason", "bulk")]
        ),
        Some(2)
    );

    // Failed rename to existing `m2`.
    assert!(st2.rename("m1", "m2").is_err());
    assert_all_three_phase2_parity(&st2, &cum2, "m1");
    let c_fail = cum2.collect();
    let dl_fail = delta2.collect();
    assert_eq!(
        otel_u64(
            &c_fail,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "m1"), ("reason", "bulk")]
        ),
        Some(2)
    );
    assert_eq!(
        otel_u64(
            &dl_fail,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "m1"), ("reason", "bulk")]
        ),
        Some(0)
    );

    // Memory `m1 -> m2 -> m1` between two OTLP collections.
    assert!(st2.delete("m2").unwrap());
    st2.rename("m1", "m2").unwrap();
    st2.rename("m2", "m1").unwrap();
    assert_all_three_phase2_parity(&st2, &cum2, "m1");
    let c_rt = cum2.collect();
    let dl_rt = delta2.collect();
    assert_eq!(
        otel_u64(
            &c_rt,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "m1"), ("reason", "bulk")]
        ),
        Some(2)
    );
    assert_eq!(
        otel_u64(
            &dl_rt,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "m1"), ("reason", "bulk")]
        ),
        Some(0)
    );

    // Part D: Failed rename on a promoted memory dataset preserves `promoted_offset`.
    let dir3 = tempfile::tempdir().unwrap();
    let mut st3 = AppState::new(
        dir3.path(),
        StoreOptions {
            bulk_threshold: 1,
            ..Default::default()
        },
        Duration::from_secs(30),
    )
    .unwrap();
    st3.metrics = crate::obs::Metrics::new(true, 1);
    let st3 = Arc::new(st3);
    st3.create("slot", DbType::Mem).unwrap();
    {
        let p = st3.create("p", DbType::Mem).unwrap();
        p.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    st3.set_phase(Phase::Ready);
    let app3 = router(st3.clone());
    assert_eq!(
        call(
            &app3,
            HttpRequest::get("/slot/sparql?query=ASK%7B%7D")
                .body(Body::empty())
                .unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );
    let cum3 = MetricsHarness::with_temporality(&st3, Temporality::Cumulative);
    let delta3 = MetricsHarness::with_temporality(&st3, Temporality::Delta);
    assert_all_three_phase2_parity(&st3, &cum3, "$other");
    let _ = delta3.collect();
    assert!(st3.delete("slot").unwrap());
    assert_all_three_phase2_parity(&st3, &cum3, "p");
    assert_all_three_phase2_parity(&st3, &cum3, "$other");
    let _ = delta3.collect();
    st3.create("blocker", DbType::Mem).unwrap();
    assert!(st3.rename("p", "blocker").is_err());
    assert_all_three_phase2_parity(&st3, &cum3, "p");
    assert_all_three_phase2_parity(&st3, &cum3, "$other");
    let c_prom = cum3.collect();
    let d_prom = delta3.collect();
    assert_eq!(
        otel_u64(
            &c_prom,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "p"), ("reason", "bulk")]
        ),
        Some(0),
        "failed rename must preserve promoted_offset on promoted dataset"
    );
    assert_eq!(
        otel_u64(
            &d_prom,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "p"), ("reason", "bulk")]
        ),
        Some(0)
    );
    assert_eq!(
        otel_u64(
            &c_prom,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );
}

#[tokio::test]
async fn phase2_otlp_unscraped_named_eligible_delete_and_final_other_cache_counters() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        dir.path(),
        StoreOptions {
            bulk_threshold: 1,
            result_cache_min_ms: 0.0,
            ..Default::default()
        },
        Duration::from_secs(30),
    )
    .unwrap();
    st.metrics = crate::obs::Metrics::new(true, 1);
    let st = Arc::new(st);
    st.create("a", DbType::Mem).unwrap();
    {
        let b = st.create("b", DbType::Mem).unwrap();
        b.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        b.store.compact().unwrap();
    }
    st.set_phase(Phase::Ready);
    let app = router(st.clone());
    // Claim named slot with `a`, then exercise `b`'s result cache twice (miss + hit).
    assert_eq!(
        call(
            &app,
            HttpRequest::get("/a/sparql?query=ASK%7B%7D")
                .body(Body::empty())
                .unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );
    // Use an ordered query so the first run materializes into the result cache (miss)
    // and the second run serves from the result cache (hit).
    let cached_query =
        "SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D%20ORDER%20BY%20%3Fs%20%3Fp%20%3Fo";
    for _ in 0..2 {
        assert_eq!(
            call(
                &app,
                HttpRequest::get(format!("/b/sparql?query={cached_query}"))
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .0,
            StatusCode::OK
        );
    }

    let cum_h = MetricsHarness::with_temporality(&st, Temporality::Cumulative);
    let delta_h = MetricsHarness::with_temporality(&st, Temporality::Delta);

    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let c1 = cum_h.collect();
    let _d1 = delta_h.collect();
    let rcache_hits_before = otel_f64(
        &c1,
        "sparkles.result_cache.hits",
        "{hit}",
        &[("dataset", "$other")],
    )
    .unwrap();
    let rcache_misses_before = otel_f64(
        &c1,
        "sparkles.result_cache.misses",
        "{miss}",
        &[("dataset", "$other")],
    )
    .unwrap();
    assert!(
        rcache_hits_before >= 1.0 && rcache_misses_before >= 1.0,
        "expected positive result cache counters on $other: hits={rcache_hits_before}, misses={rcache_misses_before}"
    );

    // Part A: Deleting the final overflow dataset `b` preserves positive `$other` cache
    // counters across Prometheus, JSON, OTLP Cumulative, and OTLP Delta (no zero drop).
    assert!(st.delete("b").unwrap());
    let prom = crate::obs::render_prometheus(&st);
    let j = crate::obs::metrics_json(&st);
    let c2 = cum_h.collect();
    let d2 = delta_h.collect();
    let j_other = j["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "$other")
        .unwrap();
    assert_eq!(
        j_other["resultCache"]["hits"].as_u64(),
        Some(rcache_hits_before as u64)
    );
    assert_eq!(
        j_other["resultCache"]["misses"].as_u64(),
        Some(rcache_misses_before as u64)
    );
    assert!(
        prom.contains(&format!(
            "sparkles_result_cache_hits_total{{dataset=\"$other\"}} {}",
            rcache_hits_before as u64
        )),
        "{prom}"
    );
    assert_eq!(
        otel_f64(
            &c2,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        ),
        Some(rcache_hits_before)
    );
    assert_eq!(
        otel_f64(
            &c2,
            "sparkles.result_cache.misses",
            "{miss}",
            &[("dataset", "$other")]
        ),
        Some(rcache_misses_before)
    );
    for (metric, unit) in [
        ("sparkles.block_cache.hits", "{hit}"),
        ("sparkles.block_cache.misses", "{miss}"),
        ("sparkles.result_cache.hits", "{hit}"),
        ("sparkles.result_cache.misses", "{miss}"),
    ] {
        assert_eq!(
            otel_f64(&d2, metric, unit, &[("dataset", "$other")]),
            Some(0.0),
            "expected zero delta on {metric} after final overflow delete"
        );
    }

    // Part B: Create new overflow `c`, exercise its result cache, then delete named `a`
    // so `c` promotes to named `"c"`. `$other` cache counters stay monotonic (Delta = 0)
    // and `"c"` starts cleanly at 0 without double counting.
    {
        let c = st.create("c", DbType::Mem).unwrap();
        c.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    for _ in 0..2 {
        assert_eq!(
            call(
                &app,
                HttpRequest::get(format!("/c/sparql?query={cached_query}"))
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let c3 = cum_h.collect();
    let d3 = delta_h.collect();
    let rcache_hits_with_c = otel_f64(
        &c3,
        "sparkles.result_cache.hits",
        "{hit}",
        &[("dataset", "$other")],
    )
    .unwrap();
    assert!(rcache_hits_with_c > rcache_hits_before);
    assert!(
        otel_f64(
            &d3,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        )
        .unwrap()
            > 0.0
    );

    assert!(st.delete("a").unwrap());
    let c4 = cum_h.collect();
    let d4 = delta_h.collect();
    assert_eq!(
        otel_f64(
            &c4,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        ),
        Some(rcache_hits_with_c)
    );
    assert_eq!(
        otel_f64(
            &d4,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        ),
        Some(0.0)
    );
    assert_eq!(
        otel_f64(
            &c4,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "c")]
        ),
        Some(0.0)
    );

    // Part C: Delete `c` so `named = 0 < max_datasets = 1` while `$other` still exists.
    // Create unscraped named-eligible `x`, run a bulk load, and delete `x` without scraping:
    // `$other` rebuild count must NOT increase.
    assert!(st.delete("c").unwrap());
    let c5 = cum_h.collect();
    let _d5 = delta_h.collect();
    let other_bulk_before = otel_u64(
        &c5,
        "sparkles.rebuilds",
        "{rebuild}",
        &[("dataset", "$other"), ("reason", "bulk")],
    )
    .unwrap();
    {
        let x = st.create("x", DbType::Mem).unwrap();
        x.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    assert!(st.delete("x").unwrap());
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let c6 = cum_h.collect();
    let d6 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &c6,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(other_bulk_before),
        "unscraped named-eligible dataset leaked rebuilds into $other"
    );
    assert_eq!(
        otel_u64(
            &d6,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(0)
    );
}

#[tokio::test]
async fn phase2_otlp_restored_during_scrape_pruned_on_delete_pending_and_promotion() {
    use std::sync::mpsc;

    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        dir.path(),
        StoreOptions {
            bulk_threshold: 1,
            result_cache_min_ms: 0.0,
            ..Default::default()
        },
        Duration::from_secs(30),
    )
    .unwrap();
    st.metrics = crate::obs::Metrics::new(true, 1);
    let st = Arc::new(st);
    st.create("a", DbType::Mem).unwrap();
    async fn load_and_cache(st: &Arc<AppState>, app: &Router, name: &str) {
        {
            let ds = st.create(name, DbType::Persistent).unwrap();
            ds.store
                .load(&[Source::from_bytes(
                    DATA.as_bytes().to_vec(),
                    oxrdfio::RdfFormat::NTriples,
                    None,
                )])
                .unwrap();
        }
        let q = "SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D%20ORDER%20BY%20%3Fs%20%3Fp%20%3Fo";
        for _ in 0..2 {
            assert_eq!(
                call(
                    app,
                    HttpRequest::get(format!("/{name}/sparql?query={q}"))
                        .body(Body::empty())
                        .unwrap()
                )
                .await
                .0,
                StatusCode::OK
            );
        }
    }
    st.set_phase(Phase::Ready);
    let app = router(st.clone());
    assert_eq!(
        call(
            &app,
            HttpRequest::get("/a/sparql?query=ASK%7B%7D")
                .body(Body::empty())
                .unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );

    // Part A: Failed rename -> committed delete during a paused OTLP collection must not
    // double-count `b` from both `retired` and `restored_during_scrape`.
    load_and_cache(&st, &app, "b").await;
    let cum_h = MetricsHarness::with_temporality(&st, Temporality::Cumulative);
    let delta_h = MetricsHarness::with_temporality(&st, Temporality::Delta);
    let c0 = cum_h.collect();
    let _d0 = delta_h.collect();
    let hits_b = otel_f64(
        &c0,
        "sparkles.result_cache.hits",
        "{hit}",
        &[("dataset", "$other")],
    )
    .unwrap();
    assert!(hits_b >= 1.0);
    assert_eq!(
        otel_u64(
            &c0,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );

    let hook_ran_a = Arc::new(AtomicBool::new(false));
    {
        let st_hook = st.clone();
        let ran = hook_ran_a.clone();
        crate::obs::set_gauges_after_datasets_hook(&st, move || {
            assert!(st_hook.rename("b", "a").is_err());
            assert!(st_hook.delete("b").unwrap());
            ran.store(true, Ordering::SeqCst);
        });
    }
    let c1 = cum_h.collect();
    assert!(hook_ran_a.load(Ordering::SeqCst), "Part A hook did not run");
    let d1 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &c1,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1),
        "failed rename followed by committed delete double-counted $other rebuilds"
    );
    assert_eq!(
        otel_f64(
            &c1,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        ),
        Some(hits_b)
    );
    assert_eq!(
        otel_f64(
            &d1,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        ),
        Some(0.0)
    );
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let d1_after = delta_h.collect();
    assert_eq!(
        otel_f64(
            &d1_after,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        ),
        Some(0.0)
    );

    // Part B: Failed rename -> pending delete (paused at delete_hook) during a paused
    // collection must not double-count `c` from both `pending` and `restored_during_scrape`.
    load_and_cache(&st, &app, "c").await;
    let c2 = cum_h.collect();
    let _d2 = delta_h.collect();
    let hits_bc = otel_f64(
        &c2,
        "sparkles.result_cache.hits",
        "{hit}",
        &[("dataset", "$other")],
    )
    .unwrap();
    assert!(hits_bc > hits_b);
    assert_eq!(
        otel_u64(
            &c2,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(2)
    );

    let (del_entered, del_entered_rx) = mpsc::channel();
    let (del_release, del_release_rx) = mpsc::channel::<()>();
    let (del_done, del_done_rx) = mpsc::channel();
    let hook_ran_b = Arc::new(AtomicBool::new(false));
    {
        let st_hook = st.clone();
        let ran = hook_ran_b.clone();
        crate::obs::set_gauges_after_datasets_hook(&st, move || {
            assert!(st_hook.rename("c", "a").is_err());
            crate::state::tests::set_delete_hook("c", move || {
                del_entered.send(()).unwrap();
                del_release_rx
                    .recv_timeout(Duration::from_secs(15))
                    .unwrap();
            });
            let st_del = st_hook.clone();
            let handle = std::thread::spawn(move || st_del.delete("c").unwrap());
            del_entered_rx
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            del_done.send(handle).unwrap();
            ran.store(true, Ordering::SeqCst);
        });
    }
    let c3 = cum_h.collect();
    assert!(hook_ran_b.load(Ordering::SeqCst), "Part B hook did not run");
    let d3 = delta_h.collect();
    del_release.send(()).unwrap();
    assert!(
        del_done_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .join()
            .unwrap()
    );
    assert_eq!(
        otel_u64(
            &c3,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(2),
        "failed rename followed by pending delete double-counted $other rebuilds"
    );
    assert_eq!(
        otel_f64(
            &c3,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        ),
        Some(hits_bc)
    );
    assert_eq!(
        otel_f64(
            &d3,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        ),
        Some(0.0)
    );
    let d3_after = delta_h.collect();
    assert_eq!(
        otel_f64(
            &d3_after,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        ),
        Some(0.0)
    );

    // Part C: Memory rename (`d -> d2`, populating `restored_during_scrape` and
    // `renamed_from`) -> overlapping promotion by Scrape 2 while Scrape 1 is paused must
    // prune `restored_during_scrape` so Scrape 1 (which skips stale `d` and omits `d`'s
    // `open_id` from `live_ids`) does not double-count `d`.
    {
        {
            let d = st.create("d", DbType::Mem).unwrap();
            d.store
                .load(&[Source::from_bytes(
                    DATA.as_bytes().to_vec(),
                    oxrdfio::RdfFormat::NTriples,
                    None,
                )])
                .unwrap();
        }
        let q = "SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D%20ORDER%20BY%20%3Fs%20%3Fp%20%3Fo";
        for _ in 0..2 {
            assert_eq!(
                call(
                    &app,
                    HttpRequest::get(format!("/d/sparql?query={q}"))
                        .body(Body::empty())
                        .unwrap()
                )
                .await
                .0,
                StatusCode::OK
            );
        }
    }
    let c4 = cum_h.collect();
    let _d4 = delta_h.collect();
    let hits_bcd = otel_f64(
        &c4,
        "sparkles.result_cache.hits",
        "{hit}",
        &[("dataset", "$other")],
    )
    .unwrap();
    assert!(hits_bcd > hits_bc);
    assert_eq!(
        otel_u64(
            &c4,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(3)
    );

    let hook_ran_c = Arc::new(AtomicBool::new(false));
    {
        let st_hook = st.clone();
        let ran = hook_ran_c.clone();
        crate::obs::set_gauges_after_datasets_hook(&st, move || {
            st_hook.rename("d", "d2").unwrap();
            assert!(st_hook.delete("a").unwrap());
            // Overlapping Scrape 2 promotes `d2` into the freed named slot while Scrape 1
            // is still active (`active_scrapes == 2 -> 1`).
            let _ = crate::obs::metrics_json(&st_hook);
            ran.store(true, Ordering::SeqCst);
        });
    }
    let c5 = cum_h.collect();
    assert!(hook_ran_c.load(Ordering::SeqCst), "Part C hook did not run");
    let d5 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &c5,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(3),
        "memory rename followed by overlapping promotion double-counted $other rebuilds"
    );
    assert_eq!(
        otel_f64(
            &c5,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        ),
        Some(hits_bcd)
    );
    assert_eq!(
        otel_f64(
            &d5,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        ),
        Some(0.0)
    );
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    assert_all_three_phase2_parity(&st, &cum_h, "d2");
}

#[tokio::test]
async fn phase2_otlp_stale_scrape_across_concurrent_memory_rename() {
    let q = "SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D%20ORDER%20BY%20%3Fs%20%3Fp%20%3Fo";

    // Part A (cap = 1): A stale scrape that sampled named memory dataset `a` before
    // `rename(a, b)` must neither move `a`'s counters to `$other` nor corrupt `b`.
    {
        let dir = tempfile::tempdir().unwrap();
        let mut st = AppState::new(
            dir.path(),
            StoreOptions {
                bulk_threshold: 1,
                result_cache_min_ms: 0.0,
                ..Default::default()
            },
            Duration::from_secs(30),
        )
        .unwrap();
        st.metrics = crate::obs::Metrics::new(true, 1);
        let st = Arc::new(st);
        {
            let a = st.create("a", DbType::Mem).unwrap();
            a.store
                .load(&[Source::from_bytes(
                    DATA.as_bytes().to_vec(),
                    oxrdfio::RdfFormat::NTriples,
                    None,
                )])
                .unwrap();
        }
        st.set_phase(Phase::Ready);
        let app = router(st.clone());
        for _ in 0..2 {
            assert_eq!(
                call(
                    &app,
                    HttpRequest::get(format!("/a/sparql?query={q}"))
                        .body(Body::empty())
                        .unwrap()
                )
                .await
                .0,
                StatusCode::OK
            );
        }
        let cum_h = MetricsHarness::with_temporality(&st, Temporality::Cumulative);
        let c0 = cum_h.collect();
        let hits_a = otel_f64(
            &c0,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "a")],
        )
        .unwrap();
        assert!(hits_a >= 1.0);
        assert_eq!(
            otel_u64(
                &c0,
                "sparkles.rebuilds",
                "{rebuild}",
                &[("dataset", "a"), ("reason", "bulk")]
            ),
            Some(1)
        );

        let hook_ran = Arc::new(AtomicBool::new(false));
        {
            let st_hook = st.clone();
            let ran = hook_ran.clone();
            crate::obs::set_gauges_after_datasets_hook(&st, move || {
                st_hook.rename("a", "b").unwrap();
                ran.store(true, Ordering::SeqCst);
            });
        }
        let j_stale = crate::obs::metrics_json(&st);
        assert!(hook_ran.load(Ordering::SeqCst));
        assert!(
            !j_stale["datasets"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["name"] == "$other"),
            "stale scrape across cap-1 memory rename misattributed named work to $other: {j_stale}"
        );

        assert_all_three_phase2_parity(&st, &cum_h, "b");
        let c1 = cum_h.collect();
        assert_eq!(
            otel_u64(
                &c1,
                "sparkles.rebuilds",
                "{rebuild}",
                &[("dataset", "b"), ("reason", "bulk")]
            ),
            Some(1),
            "renamed dataset `b` lost rebuilds to stale scrape misattribution"
        );
        assert_eq!(
            otel_f64(
                &c1,
                "sparkles.result_cache.hits",
                "{hit}",
                &[("dataset", "b")]
            ),
            Some(hits_a)
        );
        assert_eq!(
            otel_u64(
                &c1,
                "sparkles.rebuilds",
                "{rebuild}",
                &[("dataset", "$other"), ("reason", "bulk")]
            ),
            None
        );
    }

    // Part B (cap = 2): With free slot capacity, a stale scrape across `rename(a, b)`
    // must not resurrect a permanent ghost named slot for dead alias `a`.
    {
        let dir = tempfile::tempdir().unwrap();
        let mut st = AppState::new(
            dir.path(),
            StoreOptions {
                bulk_threshold: 1,
                ..Default::default()
            },
            Duration::from_secs(30),
        )
        .unwrap();
        st.metrics = crate::obs::Metrics::new(true, 2);
        let st = Arc::new(st);
        {
            let a = st.create("a", DbType::Mem).unwrap();
            a.store
                .load(&[Source::from_bytes(
                    DATA.as_bytes().to_vec(),
                    oxrdfio::RdfFormat::NTriples,
                    None,
                )])
                .unwrap();
        }
        st.set_phase(Phase::Ready);
        let cum_h = MetricsHarness::with_temporality(&st, Temporality::Cumulative);
        let _ = cum_h.collect();

        let hook_ran = Arc::new(AtomicBool::new(false));
        {
            let st_hook = st.clone();
            let ran = hook_ran.clone();
            crate::obs::set_gauges_after_datasets_hook(&st, move || {
                st_hook.rename("a", "b").unwrap();
                ran.store(true, Ordering::SeqCst);
            });
        }
        let _ = crate::obs::metrics_json(&st);
        assert!(hook_ran.load(Ordering::SeqCst));

        let prom = crate::obs::render_prometheus(&st);
        assert!(
            !prom.contains("dataset=\"a\""),
            "stale scrape resurrected ghost named slot for dead alias `a`: {prom}"
        );

        // Creating a second dataset `c` must receive the second named slot, not `$other`.
        {
            let c = st.create("c", DbType::Mem).unwrap();
            c.store
                .load(&[Source::from_bytes(
                    DATA.as_bytes().to_vec(),
                    oxrdfio::RdfFormat::NTriples,
                    None,
                )])
                .unwrap();
        }
        assert_all_three_phase2_parity(&st, &cum_h, "b");
        assert_all_three_phase2_parity(&st, &cum_h, "c");
        let c_after = cum_h.collect();
        assert_eq!(
            otel_u64(
                &c_after,
                "sparkles.rebuilds",
                "{rebuild}",
                &[("dataset", "c"), ("reason", "bulk")]
            ),
            Some(1),
            "free named slot was consumed by ghost alias `a`"
        );
        assert_eq!(
            otel_u64(
                &c_after,
                "sparkles.rebuilds",
                "{rebuild}",
                &[("dataset", "$other"), ("reason", "bulk")]
            ),
            None
        );
    }

    // Part C (cap = 1, overflow memory rename `b -> c`): Stale sample for `b` is skipped
    // and `restored_during_scrape` preserves `$other` without drop or double-counting.
    {
        let dir = tempfile::tempdir().unwrap();
        let mut st = AppState::new(
            dir.path(),
            StoreOptions {
                bulk_threshold: 1,
                result_cache_min_ms: 0.0,
                ..Default::default()
            },
            Duration::from_secs(30),
        )
        .unwrap();
        st.metrics = crate::obs::Metrics::new(true, 1);
        let st = Arc::new(st);
        st.create("a", DbType::Mem).unwrap();
        {
            let b = st.create("b", DbType::Mem).unwrap();
            b.store
                .load(&[Source::from_bytes(
                    DATA.as_bytes().to_vec(),
                    oxrdfio::RdfFormat::NTriples,
                    None,
                )])
                .unwrap();
        }
        st.set_phase(Phase::Ready);
        let app = router(st.clone());
        assert_eq!(
            call(
                &app,
                HttpRequest::get("/a/sparql?query=ASK%7B%7D")
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .0,
            StatusCode::OK
        );
        for _ in 0..2 {
            assert_eq!(
                call(
                    &app,
                    HttpRequest::get(format!("/b/sparql?query={q}"))
                        .body(Body::empty())
                        .unwrap()
                )
                .await
                .0,
                StatusCode::OK
            );
        }
        let cum_h = MetricsHarness::with_temporality(&st, Temporality::Cumulative);
        let delta_h = MetricsHarness::with_temporality(&st, Temporality::Delta);
        let c0 = cum_h.collect();
        let _d0 = delta_h.collect();
        let hits0 = otel_f64(
            &c0,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")],
        )
        .unwrap();
        assert!(hits0 >= 1.0);

        let hook_ran = Arc::new(AtomicBool::new(false));
        {
            let st_hook = st.clone();
            let ran = hook_ran.clone();
            crate::obs::set_gauges_after_datasets_hook(&st, move || {
                st_hook.rename("b", "c").unwrap();
                ran.store(true, Ordering::SeqCst);
            });
        }
        let c1 = cum_h.collect();
        assert!(hook_ran.load(Ordering::SeqCst));
        let d1 = delta_h.collect();
        assert_eq!(
            otel_u64(
                &c1,
                "sparkles.rebuilds",
                "{rebuild}",
                &[("dataset", "$other"), ("reason", "bulk")]
            ),
            Some(1)
        );
        assert_eq!(
            otel_f64(
                &c1,
                "sparkles.result_cache.hits",
                "{hit}",
                &[("dataset", "$other")]
            ),
            Some(hits0)
        );
        assert_eq!(
            otel_f64(
                &d1,
                "sparkles.result_cache.hits",
                "{hit}",
                &[("dataset", "$other")]
            ),
            Some(0.0)
        );
    }
}

#[tokio::test]
async fn phase2_otlp_failed_delete_promotion_conserves_published_pending_counters() {
    use std::sync::mpsc;

    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        dir.path(),
        StoreOptions {
            bulk_threshold: 1,
            result_cache_min_ms: 0.0,
            ..Default::default()
        },
        Duration::from_secs(30),
    )
    .unwrap();
    st.metrics = crate::obs::Metrics::new(true, 1);
    let st = Arc::new(st);
    st.create("a", DbType::Mem).unwrap();
    {
        let b = st.create("b", DbType::Persistent).unwrap();
        b.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    st.set_phase(Phase::Ready);
    let app = router(st.clone());
    assert_eq!(
        call(
            &app,
            HttpRequest::get("/a/sparql?query=ASK%7B%7D")
                .body(Body::empty())
                .unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );
    let q = "SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D%20ORDER%20BY%20%3Fs%20%3Fp%20%3Fo";
    for _ in 0..2 {
        assert_eq!(
            call(
                &app,
                HttpRequest::get(format!("/b/sparql?query={q}"))
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .0,
            StatusCode::OK
        );
    }

    let cum_h = MetricsHarness::with_temporality(&st, Temporality::Cumulative);
    let delta_h = MetricsHarness::with_temporality(&st, Temporality::Delta);
    let c0 = cum_h.collect();
    let _d0 = delta_h.collect();
    let hits_initial = otel_f64(
        &c0,
        "sparkles.result_cache.hits",
        "{hit}",
        &[("dataset", "$other")],
    )
    .unwrap();
    assert!(hits_initial >= 1.0);
    assert_eq!(
        otel_u64(
            &c0,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );

    // Complete a second rebuild and extra cache hits on `b` WITHOUT scraping.
    {
        let b = st.get("b").unwrap();
        let nt: String = (0..4)
            .map(|i| format!("<http://ex/faildel{i}> <http://ex/p> \"{i}\" .\n"))
            .collect();
        b.store
            .load(&[Source::from_bytes(
                nt.into_bytes(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    for _ in 0..2 {
        assert_eq!(
            call(
                &app,
                HttpRequest::get(format!("/b/sparql?query={q}"))
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .0,
            StatusCode::OK
        );
    }

    // Reserve `b` so catalog deletion will reject before closing `b`.
    let reservation = st
        .catalog
        .reserve("b", sparkles::catalog::ReservationKind::Restore, "t-res")
        .unwrap();
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    crate::state::tests::set_delete_hook("b", move || {
        entered.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(15)).unwrap();
    });
    let deleter = {
        let st = st.clone();
        std::thread::spawn(move || st.delete("b"))
    };
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();

    // Free the named slot while `b` is paused in `leaving`, then scrape the pending totals.
    assert!(st.delete("a").unwrap());
    let c_mid = cum_h.collect();
    let _d_mid = delta_h.collect();
    let hits_published = otel_f64(
        &c_mid,
        "sparkles.result_cache.hits",
        "{hit}",
        &[("dataset", "$other")],
    )
    .unwrap();
    assert!(hits_published > hits_initial);
    assert_eq!(
        otel_u64(
            &c_mid,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(2)
    );

    // Release the hook so the reservation rejects deletion and `b` survives.
    release.send(()).unwrap();
    assert!(deleter.join().unwrap().is_err());
    drop(reservation);

    // Next scrape promotes `b` into the vacant named slot: `$other` must retain the full
    // published pending counters (2 rebuilds, `hits_published`) with zero negative delta.
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    assert_all_three_phase2_parity(&st, &cum_h, "b");
    let c_after = cum_h.collect();
    let d_after = delta_h.collect();
    assert_eq!(
        otel_u64(
            &c_after,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(2),
        "promotion after failed delete reduced published $other rebuilds"
    );
    assert_eq!(
        otel_u64(
            &c_after,
            "sparkles.rebuild.duration.count",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(2)
    );
    assert_eq!(
        otel_u64(
            &c_after,
            "sparkles.rebuild.duration.bucket",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk"), ("le", "+Inf")]
        ),
        Some(2)
    );
    assert_eq!(
        otel_f64(
            &c_after,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        ),
        Some(hits_published)
    );
    assert_eq!(
        otel_u64(
            &c_after,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "b"), ("reason", "bulk")]
        ),
        Some(0)
    );
    assert_eq!(
        otel_f64(
            &c_after,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "b")]
        ),
        Some(0.0)
    );
    assert_eq!(
        otel_u64(
            &d_after,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(0)
    );
    for (metric, unit) in [
        ("sparkles.block_cache.hits", "{hit}"),
        ("sparkles.block_cache.misses", "{miss}"),
        ("sparkles.result_cache.hits", "{hit}"),
        ("sparkles.result_cache.misses", "{miss}"),
    ] {
        assert_eq!(
            otel_f64(&d_after, metric, unit, &[("dataset", "$other")]),
            Some(0.0),
            "unexpected delta on {metric} after failed-delete promotion"
        );
    }
}

#[tokio::test]
async fn phase2_otlp_work_completed_during_delete_drain_is_retired() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        dir.path(),
        StoreOptions {
            bulk_threshold: 1,
            result_cache_min_ms: 0.0,
            ..Default::default()
        },
        Duration::from_secs(30),
    )
    .unwrap();
    st.metrics = crate::obs::Metrics::new(true, 1);
    let st = Arc::new(st);
    st.create("a", DbType::Mem).unwrap();
    {
        let b = st.create("b", DbType::Persistent).unwrap();
        b.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    st.set_phase(Phase::Ready);
    let app = router(st.clone());
    assert_eq!(
        call(
            &app,
            HttpRequest::get("/a/sparql?query=ASK%7B%7D")
                .body(Body::empty())
                .unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );

    let cum_h = MetricsHarness::with_temporality(&st, Temporality::Cumulative);
    let delta_h = MetricsHarness::with_temporality(&st, Temporality::Delta);
    let c0 = cum_h.collect();
    let _d0 = delta_h.collect();
    let sum_before = otel_f64(
        &c0,
        "sparkles.rebuild.duration.sum",
        "s",
        &[("dataset", "$other"), ("reason", "bulk")],
    )
    .unwrap();
    assert_eq!(
        otel_u64(
            &c0,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );

    // Hold a live request wrapper on `b` while starting deletion so `delete` waits in drain.
    let held = st.get("b").unwrap();
    let deleter = {
        let st = st.clone();
        std::thread::spawn(move || st.delete("b").unwrap())
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while st.get("b").is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "delete never hid `b` in leaving"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    // Complete a second bulk load while `b` is draining, then release the request handle.
    let nt: String = (0..6)
        .map(|i| format!("<http://ex/drain{i}> <http://ex/p> \"{i}\" .\n"))
        .collect();
    held.store
        .load(&[Source::from_bytes(
            nt.into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    drop(held);
    assert!(deleter.join().unwrap());

    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let c1 = cum_h.collect();
    let d1 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &c1,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(2),
        "rebuild completed during request drain was lost on retirement"
    );
    assert_eq!(
        otel_u64(
            &c1,
            "sparkles.rebuild.duration.count",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(2)
    );
    assert_eq!(
        otel_u64(
            &c1,
            "sparkles.rebuild.duration.bucket",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk"), ("le", "+Inf")]
        ),
        Some(2)
    );
    assert!(
        otel_f64(
            &c1,
            "sparkles.rebuild.duration.sum",
            "s",
            &[("dataset", "$other"), ("reason", "bulk")]
        )
        .unwrap()
            > sum_before
    );
    assert_eq!(
        otel_u64(
            &d1,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );
}

#[cfg(feature = "backup")]
#[tokio::test]
async fn phase2_otlp_in_place_backup_restore_retires_overflow_and_handles_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        dir.path(),
        StoreOptions {
            bulk_threshold: 1,
            result_cache_min_ms: 0.0,
            ..Default::default()
        },
        Duration::from_secs(30),
    )
    .unwrap();
    st.metrics = crate::obs::Metrics::new(true, 1);
    let st = Arc::new(st);
    st.create("a", DbType::Mem).unwrap();
    let id1 = {
        let b = st.create("b", DbType::Persistent).unwrap();
        b.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        b.open_id
    };
    st.set_phase(Phase::Ready);
    let app = router(st.clone());
    assert_eq!(
        call(
            &app,
            HttpRequest::get("/a/sparql?query=ASK%7B%7D")
                .body(Body::empty())
                .unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );
    let q = "SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D%20ORDER%20BY%20%3Fs%20%3Fp%20%3Fo";
    for _ in 0..2 {
        assert_eq!(
            call(
                &app,
                HttpRequest::get(format!("/b/sparql?query={q}"))
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .0,
            StatusCode::OK
        );
    }

    let cum_h = MetricsHarness::with_temporality(&st, Temporality::Cumulative);
    let delta_h = MetricsHarness::with_temporality(&st, Temporality::Delta);
    let c0 = cum_h.collect();
    let _d0 = delta_h.collect();
    let hits0 = otel_f64(
        &c0,
        "sparkles.result_cache.hits",
        "{hit}",
        &[("dataset", "$other")],
    )
    .unwrap();
    assert!(hits0 >= 1.0);
    assert_eq!(
        otel_u64(
            &c0,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );

    // Part A: Successful in-place backup restore retires `id1` without drop or ledger leak.
    let restored_dir = dir.path().join("databases/.restore-b-1");
    {
        let s = sparkles::store::Store::open(&restored_dir, StoreOptions::default()).unwrap();
        sparkles::sparql::update::update(
            &s,
            "INSERT DATA { <http://ex/r1> <http://ex/p> \"1\" }",
            &sparkles::sparql::QueryOptions::default(),
        )
        .unwrap();
    }
    let replaced =
        crate::backup::swap::replace_in_place(&st, "b", &restored_dir, "swap-1", false).unwrap();
    let id2 = replaced.open_id;
    drop(replaced);
    assert_ne!(id2, id1);
    assert!(
        !st.metrics.other_ledger_store_ids().contains(&id1),
        "old open_id {id1} leaked in OtherStoreLedger::stores after in-place restore"
    );

    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let c1 = cum_h.collect();
    let d1 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &c1,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1),
        "in-place backup restore dropped $other rebuilds"
    );
    assert_eq!(
        otel_f64(
            &c1,
            "sparkles.result_cache.hits",
            "{hit}",
            &[("dataset", "$other")]
        ),
        Some(hits0)
    );
    for (metric, unit) in [
        ("sparkles.block_cache.hits", "{hit}"),
        ("sparkles.block_cache.misses", "{miss}"),
        ("sparkles.result_cache.hits", "{hit}"),
        ("sparkles.result_cache.misses", "{miss}"),
    ] {
        assert_eq!(
            otel_f64(&d1, metric, unit, &[("dataset", "$other")]),
            Some(0.0),
            "unexpected delta on {metric} after in-place backup restore"
        );
    }

    // New work on the restored store increments `$other` cleanly from 1 to 2.
    {
        let b = st.get("b").unwrap();
        let nt: String = (0..5)
            .map(|i| format!("<http://ex/swap{i}> <http://ex/p> \"{i}\" .\n"))
            .collect();
        b.store
            .load(&[Source::from_bytes(
                nt.into_bytes(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    let c2 = cum_h.collect();
    let d2 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &c2,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(2)
    );
    assert_eq!(
        otel_u64(
            &d2,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(1)
    );

    // Part B: Refusal before close (invalid restored dataset.json) reinserts the same store
    // (`open_id == id2`) and must not retire or double-count it.
    let bad_dir = dir.path().join("databases/.restore-b-bad");
    std::fs::create_dir_all(&bad_dir).unwrap();
    std::fs::write(bad_dir.join("dataset.json"), b"not-json").unwrap();
    assert!(crate::backup::swap::replace_in_place(&st, "b", &bad_dir, "swap-2", false).is_err());
    assert_eq!(st.get("b").unwrap().open_id, id2);
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let c3 = cum_h.collect();
    let d3 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &c3,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(2),
        "pre-close restore refusal double-counted $other rebuilds"
    );
    assert_eq!(
        otel_u64(
            &d3,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(0)
    );

    // Part C: Post-close rollback reopen (missing restored directory fails rename after close
    // and reopens the old root under a new `open_id`) retires `id2` without drop or leak.
    let missing_dir = dir.path().join("databases/.restore-b-missing");
    assert!(
        crate::backup::swap::replace_in_place(&st, "b", &missing_dir, "swap-3", false).is_err()
    );
    let id3 = st.get("b").unwrap().open_id;
    assert_ne!(id3, id2);
    assert!(
        !st.metrics.other_ledger_store_ids().contains(&id2),
        "old open_id {id2} leaked in OtherStoreLedger::stores after post-close rollback"
    );
    assert_all_three_phase2_parity(&st, &cum_h, "$other");
    let c4 = cum_h.collect();
    let d4 = delta_h.collect();
    assert_eq!(
        otel_u64(
            &c4,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(2),
        "post-close restore rollback dropped $other rebuilds"
    );
    assert_eq!(
        otel_u64(
            &d4,
            "sparkles.rebuilds",
            "{rebuild}",
            &[("dataset", "$other"), ("reason", "bulk")]
        ),
        Some(0)
    );
}
