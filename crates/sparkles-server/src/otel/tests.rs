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
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
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
