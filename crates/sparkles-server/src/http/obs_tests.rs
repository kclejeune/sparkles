//! In-process tests of request ids, the access log, metrics, readiness and budgets.

use super::router;
use crate::obs::Phase;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use serde_json::Value as J;
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

/// 9 default-graph triples, 2 in `ex:g1`.
const DATA: &str = r#"
@prefix ex: <http://example.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

ex:Student rdfs:subClassOf ex:Person .
ex:worksFor rdfs:domain ex:Employee .
ex:alice a ex:Student ; foaf:name "Alice" ; foaf:age 30 .
ex:bob a ex:Person ; foaf:name "Bob" ; foaf:age 200 .
ex:carol ex:worksFor ex:acme .

GRAPH ex:g1 {
  ex:dave a ex:Person ; foaf:age "unknown" .
}
"#;

const ALL: &str = "/ds/sparql?query=SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D";

struct Server {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

fn server() -> Server {
    server_with(|_| {})
}

/// A ready server with dataset `ds`; `configure` sets limits and flags first.
fn server_with(configure: impl FnOnce(&mut AppState)) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut state =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    configure(&mut state);
    let state = Arc::new(state);
    let ds = state.attach("ds", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    state.set_phase(Phase::Ready);
    let app = router(state.clone());
    Server {
        _dir: dir,
        state,
        app,
    }
}

struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> J {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default()
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Resp {
        status,
        headers,
        body,
    }
}

async fn get(app: &Router, uri: &str) -> Resp {
    send(app, Request::get(uri).body(Body::empty()).unwrap()).await
}

async fn get_with(app: &Router, uri: &str, name: &str, value: &str) -> Resp {
    send(
        app,
        Request::get(uri)
            .header(name, value)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn metrics(app: &Router) -> String {
    let r = get(app, "/$/metrics").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.text()
}

/// The value of the sample with exactly this name and label set.
fn sample(text: &str, series: &str) -> Option<f64> {
    text.lines()
        .find_map(|l| l.strip_prefix(series)?.strip_prefix(' '))
        .map(|v| v.parse().unwrap())
}

fn is_generated_id(id: &str) -> bool {
    let b = id.as_bytes();
    id.len() == 21
        && b[8] == b'-'
        && id
            .chars()
            .enumerate()
            .all(|(i, c)| i == 8 || c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

// ------------------------------------------------------------------ request ids ------

#[tokio::test]
async fn request_ids_are_echoed_or_generated() {
    let s = server();
    let r = get_with(&s.app, "/$/ping", "x-request-id", "abc-123").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("x-request-id"), "abc-123");

    let long = "a".repeat(129);
    for bad in ["has space", long.as_str()] {
        let r = get_with(&s.app, "/$/ping", "x-request-id", bad).await;
        let id = r.header("x-request-id");
        assert!(is_generated_id(&id), "{id}");
    }
    let (a, b) = (
        get(&s.app, "/$/ping").await.header("x-request-id"),
        get(&s.app, "/$/ping").await.header("x-request-id"),
    );
    assert!(is_generated_id(&a) && is_generated_id(&b));
    assert!(a[9..] < b[9..], "{a} then {b}");

    // errors and unknown routes carry an id too; CORS exposes it
    for uri in [
        "/nope/sparql?query=ASK%7B%7D",
        "/$/no-such-route",
        "/ds/sparql?query=SELEKT",
    ] {
        let r = get(&s.app, uri).await;
        assert!(r.status.is_client_error(), "{uri}: {}", r.status);
        assert!(!r.header("x-request-id").is_empty(), "{uri}");
    }
    let r = get_with(&s.app, "/$/ping", "origin", "http://example.com").await;
    assert!(
        r.header("access-control-expose-headers")
            .contains("x-request-id"),
        "{:?}",
        r.headers
    );
}

// ---------------------------------------------------------------------- metrics ------

#[tokio::test]
async fn metrics_count_requests_by_dataset_operation_and_outcome() {
    let s = server();
    assert_eq!(get(&s.app, ALL).await.status, StatusCode::OK);
    assert_eq!(
        get(&s.app, "/ds/sparql?query=SELEKT").await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&s.app, "/nope/sparql?query=ASK%7B%7D").await.status,
        StatusCode::NOT_FOUND
    );
    let r = get(&s.app, "/$/metrics").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.header("content-type")
            .starts_with("text/plain; version=0.0.4"),
        "{}",
        r.header("content-type")
    );
    assert_eq!(r.header("cache-control"), "no-store");
    let m = r.text();
    for (series, v) in [
        (
            r#"sparkles_requests_total{dataset="ds",operation="query",outcome="ok"}"#,
            1.0,
        ),
        (
            r#"sparkles_requests_total{dataset="ds",operation="query",outcome="client_error"}"#,
            1.0,
        ),
        (
            r#"sparkles_requests_total{dataset="$none",operation="query",outcome="client_error"}"#,
            1.0,
        ),
        (
            r#"sparkles_request_duration_seconds_count{dataset="ds",operation="query"}"#,
            2.0,
        ),
        (r#"sparkles_result_rows_total{dataset="ds"}"#, 9.0),
        (r#"sparkles_dataset_quads{dataset="ds"}"#, 11.0),
        ("sparkles_ready", 1.0),
    ] {
        assert_eq!(sample(&m, series), Some(v), "{series}\n{m}");
    }
    assert!(
        sample(
            &m,
            r#"sparkles_response_bytes_total{dataset="ds",operation="query"}"#
        )
        .unwrap()
            > 100.0
    );
    // nothing from the requests themselves leaks into labels
    for leak in ["nope", "SELEKT", "?s"] {
        assert!(!m.contains(leak), "{leak}\n{m}");
    }
    assert!(m.ends_with('\n'));
    // one TYPE line per family, before its samples
    let mut types = std::collections::HashSet::new();
    for l in m.lines() {
        if let Some(t) = l.strip_prefix("# TYPE ") {
            let name = t.split(' ').next().unwrap();
            assert!(types.insert(name.to_string()), "duplicate TYPE {name}");
        } else if !l.starts_with('#') {
            let name = l.split(['{', ' ']).next().unwrap();
            let family = ["_bucket", "_sum", "_count"]
                .iter()
                .find_map(|s| name.strip_suffix(s).filter(|f| types.contains(*f)))
                .unwrap_or(name);
            assert!(types.contains(family), "sample before its TYPE: {l}");
        }
    }
    // every histogram's +Inf bucket equals its count
    for l in m.lines().filter(|l| l.contains("le=\"+Inf\"")) {
        let (series, v) = l.rsplit_once(' ').unwrap();
        let count = series
            .replace("_bucket{", "_count{")
            .replace(",le=\"+Inf\"", "");
        assert_eq!(sample(&m, &count), Some(v.parse().unwrap()), "{l}");
    }
}

#[tokio::test]
async fn health_and_metrics_traffic_is_not_counted() {
    let s = server();
    for _ in 0..5 {
        get(&s.app, "/$/ping").await;
    }
    for _ in 0..3 {
        metrics(&s.app).await;
    }
    get(&s.app, "/$/ready").await;
    let m = metrics(&s.app).await;
    assert!(
        !m.lines()
            .any(|l| l.starts_with("sparkles_requests_total") && l.contains("operation=\"admin\"")),
        "{m}"
    );
    // other admin requests are
    get(&s.app, "/$/server").await;
    let m = metrics(&s.app).await;
    assert_eq!(
        sample(
            &m,
            r#"sparkles_requests_total{dataset="$none",operation="admin",outcome="ok"}"#
        ),
        Some(1.0),
        "{m}"
    );
}

#[tokio::test]
async fn metrics_json_snapshot_and_disabled_metrics() {
    let s = server();
    get(&s.app, ALL).await;
    let r = get(&s.app, "/$/metrics?format=json").await;
    assert_eq!(r.status, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["formatVersion"], 1);
    assert_eq!(j["ready"], true);
    assert_eq!(j["bucketBounds"].as_array().unwrap().len(), 16);
    let q = j["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["dataset"] == "ds" && r["operation"] == "query")
        .unwrap();
    assert_eq!(q["outcomes"]["ok"], 1);
    assert_eq!(q["count"], 1);
    assert_eq!(q["buckets"].as_array().unwrap().len(), 17);
    let ds = &j["datasets"][0];
    assert_eq!(ds["name"], "ds");
    assert_eq!(ds["quads"], 11);
    assert_eq!(ds["resultRows"], 9);
    assert!(ds["blockCache"]["capacityBytes"].as_u64().unwrap() > 0);
    assert_eq!(j["limits"]["maxRows"], 200_000_000);

    let s = server_with(|st| st.metrics = crate::obs::Metrics::new(false, 100));
    get(&s.app, ALL).await;
    assert_eq!(
        get(&s.app, "/$/metrics").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&s.app, "/$/metrics?format=json").await.status,
        StatusCode::NOT_FOUND
    );
}

/// A Fuseki series of dataset `ds`.
fn fuseki(name: &str, endpoint: &str, operation: &str, description: &str) -> String {
    format!(
        r#"{name}{{application="fuseki",dataset="/ds",description="{description}",endpoint="{endpoint}",operation="{operation}"}}"#
    )
}

#[tokio::test]
async fn fuseki_metric_names_are_opt_in() {
    let s = server();
    get(&s.app, ALL).await;
    let m = metrics(&s.app).await;
    assert!(!m.contains("fuseki"), "{m}");

    let s = server_with(|st| st.metrics.fuseki_names = true);
    let m = metrics(&s.app).await;
    // no series before the first request, but the process gauges at once
    assert!(!m.lines().any(|l| l.starts_with("fuseki_requests")), "{m}");
    assert!(m.contains("# TYPE fuseki_requests_good gauge"), "{m}");
    assert!(sample(&m, r#"system_cpu_count{application="fuseki"}"#).unwrap() >= 1.0);
    assert!(sample(&m, r#"process_uptime_seconds{application="fuseki"}"#).is_some());
    assert!(sample(&m, r#"process_start_time_seconds{application="fuseki"}"#).unwrap() > 1e9);

    assert_eq!(get(&s.app, ALL).await.status, StatusCode::OK);
    assert_eq!(get(&s.app, ALL).await.status, StatusCode::OK);
    assert_eq!(
        get(&s.app, "/ds/sparql?query=SELEKT").await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&s.app, "/ds?query=ASK%7B%7D").await.status,
        StatusCode::OK
    );
    assert_eq!(get(&s.app, "/ds/get?default").await.status, StatusCode::OK);
    let update = Request::post("/ds/update")
        .header("content-type", "application/sparql-update")
        .body(Body::from("INSERT DATA { <urn:a> <urn:b> <urn:c> }"))
        .unwrap();
    assert_eq!(send(&s.app, update).await.status, StatusCode::OK);
    // requests that name no dataset, and routes Fuseki has no endpoint for, are left out
    get(&s.app, "/nope/sparql?query=ASK%7B%7D").await;
    get(&s.app, "/$/server").await;

    let m = metrics(&s.app).await;
    let query = |n| fuseki(n, "sparql", "query", "SPARQL Query");
    for (series, v) in [
        (query("fuseki_requests"), 3.0),
        (query("fuseki_requests_good"), 2.0),
        (query("fuseki_requests_bad"), 1.0),
        (
            fuseki("fuseki_requests_good", "", "query", "SPARQL Query"),
            1.0,
        ),
        (
            fuseki(
                "fuseki_requests_good",
                "get",
                "gsp-r",
                "Graph Store Protocol (Read)",
            ),
            1.0,
        ),
        (
            fuseki("fuseki_requests", "update", "update", "SPARQL Update"),
            1.0,
        ),
        (
            fuseki("fuseki_requests_bad", "update", "update", "SPARQL Update"),
            0.0,
        ),
    ] {
        assert_eq!(sample(&m, &series), Some(v), "{series}\n{m}");
    }
    let fuseki_lines: Vec<&str> = m
        .lines()
        .filter(|l| l.starts_with("fuseki_requests"))
        .collect();
    // three families of four endpoints, all of dataset /ds
    assert_eq!(fuseki_lines.len(), 12, "{m}");
    assert!(
        fuseki_lines.iter().all(|l| l.contains("dataset=\"/ds\"")),
        "{m}"
    );
    // the Sparkles names are still there
    assert_eq!(
        sample(
            &m,
            r#"sparkles_requests_total{dataset="ds",operation="query",outcome="ok"}"#
        ),
        Some(3.0),
        "{m}"
    );
}

#[tokio::test]
async fn read_only_graph_store_counts_as_fuseki_gsp_r() {
    let s = server_with(|st| {
        st.metrics.fuseki_names = true;
        st.read_only = true;
    });
    assert_eq!(get(&s.app, "/ds/data?default").await.status, StatusCode::OK);
    let m = metrics(&s.app).await;
    let series = fuseki(
        "fuseki_requests_good",
        "data",
        "gsp-r",
        "Graph Store Protocol (Read)",
    );
    assert_eq!(sample(&m, &series), Some(1.0), "{m}");
}

#[tokio::test]
async fn metrics_listener_serves_only_metrics() {
    let s = server();
    get(&s.app, ALL).await;
    let app = crate::obs::metrics_router(s.state.clone());
    let r = get(&app, "/$/metrics").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        sample(
            &r.text(),
            r#"sparkles_requests_total{dataset="ds",operation="query",outcome="ok"}"#
        ),
        Some(1.0)
    );
    assert_eq!(get(&app, ALL).await.status, StatusCode::NOT_FOUND);
    assert_eq!(get(&app, "/$/ping").await.status, StatusCode::NOT_FOUND);
    // a page that rebinds its DNS name to the listener is refused, as on the main one
    let r = get_with(&app, "/$/metrics", "host", "evil.example").await;
    assert_eq!(r.status, StatusCode::MISDIRECTED_REQUEST);
    // scrapes are not counted on either listener
    let m = metrics(&s.app).await;
    assert!(
        !m.lines()
            .any(|l| l.starts_with("sparkles_requests_total") && l.contains("operation=\"admin\"")),
        "{m}"
    );
}

#[tokio::test]
async fn deleted_datasets_lose_their_series() {
    let s = server();
    let create = Request::post("/$/datasets")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"dbName":"tmp","dbType":"mem"}"#))
        .unwrap();
    assert_eq!(send(&s.app, create).await.status, StatusCode::CREATED);
    get(&s.app, "/tmp/sparql?query=ASK%7B%7D").await;
    assert!(metrics(&s.app).await.contains("dataset=\"tmp\""));
    let del = Request::delete("/$/datasets/tmp")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&s.app, del).await.status, StatusCode::OK);
    let m = metrics(&s.app).await;
    assert!(!m.contains("dataset=\"tmp\""), "{m}");
}

// -------------------------------------------------------------------- readiness ------

#[tokio::test]
async fn readiness_reports_phase_and_datasets() {
    let s = server();
    let r = get(&s.app, "/$/ready").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("cache-control"), "no-store");
    let j = r.json();
    assert_eq!(j["status"], "ready");
    assert_eq!(j["ready"], true);
    assert!(j["uptimeSeconds"].as_u64().is_some());
    assert_eq!(
        j["datasets"],
        serde_json::json!([{"name":"ds","type":"mem","state":"open","ready":true,"walBytes":0,"deltaQuads":0}])
    );
    assert_eq!(get(&s.app, "/$/ready/ds").await.status, StatusCode::OK);
    assert_eq!(
        get(&s.app, "/$/ready/zz").await.status,
        StatusCode::NOT_FOUND
    );

    // a persistent dataset reports its generation and write-ahead log
    let create = Request::post("/$/datasets")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"dbName":"disk","dbType":"persistent"}"#))
        .unwrap();
    assert_eq!(send(&s.app, create).await.status, StatusCode::CREATED);
    let upd = Request::post("/disk/update")
        .header(header::CONTENT_TYPE, "application/sparql-update")
        .body(Body::from("INSERT DATA { <urn:a> <urn:b> <urn:c> }"))
        .unwrap();
    assert_eq!(send(&s.app, upd).await.status, StatusCode::OK);
    let j = get(&s.app, "/$/ready/disk").await.json();
    let d = &j["datasets"][0];
    assert_eq!(d["type"], "persistent");
    assert!(d["generation"].as_str().unwrap().starts_with("gen-"));
    assert!(d["walBytes"].as_u64().unwrap() > 0, "{d}");
    assert_eq!(d["deltaQuads"], 1);

    s.state.set_phase(Phase::Draining);
    let r = get(&s.app, "/$/ready").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    let j = r.json();
    assert_eq!(
        (j["status"].as_str(), j["ready"].as_bool()),
        (Some("draining"), Some(false))
    );
    assert_eq!(sample(&metrics(&s.app).await, "sparkles_ready"), Some(0.0));
}

#[tokio::test]
async fn server_info_lists_the_limits() {
    let s = server_with(|st| st.limits.max_result_bytes = None);
    let j = get(&s.app, "/$/server").await.json();
    assert_eq!(
        j["limits"],
        serde_json::json!({
            "timeoutSeconds": 30.0,
            "updateTimeoutSeconds": 0.0,
            "maxTimeoutSeconds": 1800.0,
            "queryMemoryBytes": 8u64 << 30,
            "maxResultBytes": 0,
            "maxExportBytes": 0,
            "maxRows": 200_000_000,
            "maxQueryBodyBytes": 16u64 << 20,
            "maxUpdateBodyBytes": 256u64 << 20,
            "maxAdminBodyBytes": 16u64 << 20,
            "maxUploadBytes": 4u64 << 30,
        })
    );
}

#[tokio::test]
async fn updates_have_their_own_timeout() {
    let update = |uri: &str| {
        Request::post(uri)
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(Body::from("INSERT DATA { <urn:t> <urn:p> 1 }"))
            .unwrap()
    };
    // the query timeout does not apply to updates
    let s = server_with(|st| st.default_timeout = Duration::from_nanos(1));
    let r = send(&s.app, update("/ds/update")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // --update-timeout does, and a timed-out update changes nothing
    let s = server_with(|st| st.limits.update_timeout = Some(Duration::from_nanos(1)));
    let before = s.state.get("ds").unwrap().store.snapshot().len();
    let r = send(&s.app, update("/ds/update")).await;
    assert_eq!(r.status, StatusCode::REQUEST_TIMEOUT, "{}", r.text());
    assert_eq!(s.state.get("ds").unwrap().store.snapshot().len(), before);
    // an explicit timeout= overrides it
    let r = send(&s.app, update("/ds/update?timeout=30")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

// ---------------------------------------------------------------------- budgets ------

#[tokio::test]
async fn memory_budget_fails_with_507() {
    let s = server_with(|st| st.limits.query_memory_bytes = Some(1024));
    let r = get(
        &s.app,
        "/ds/sparql?query=SELECT%20*%20WHERE%20%7B%3Fa%20%3Fb%20%3Fc%20.%20%3Fd%20%3Fe%20%3Ff%7D",
    )
    .await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    let j = r.json();
    assert!(
        j["error"]
            .as_str()
            .unwrap()
            .starts_with("query exceeds its memory budget: needs about "),
        "{j}"
    );
    assert_eq!(j["budget"], "memory");
    assert_eq!(j["limit"], 1024);
    assert!(j["requested"].as_u64().unwrap() > 1024);
    let m = metrics(&s.app).await;
    assert_eq!(
        sample(
            &m,
            r#"sparkles_requests_total{dataset="ds",operation="query",outcome="budget"}"#
        ),
        Some(1.0),
        "{m}"
    );
    assert_eq!(
        sample(
            &m,
            r#"sparkles_budget_exceeded_total{dataset="ds",budget="memory"}"#
        ),
        Some(1.0)
    );
    let r = get(
        &s.app,
        "/ds/sparql?query=SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D%20LIMIT%201",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // the update WHERE clause runs under the same budget
    let upd = Request::post("/ds/update")
        .header(header::CONTENT_TYPE, "application/sparql-update")
        .body(Body::from(
            "INSERT { ?a <urn:x> ?d } WHERE { ?a ?b ?c . ?d ?e ?f }",
        ))
        .unwrap();
    let r = send(&s.app, upd).await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    assert_eq!(r.json()["budget"], "memory");
}

async fn tsv(app: &Router, uri: &str) -> Resp {
    get_with(app, uri, "accept", "text/tab-separated-values").await
}

#[tokio::test]
async fn result_budget_fails_with_507() {
    let s = server_with(|st| st.limits.max_result_bytes = Some(200));
    let r = tsv(&s.app, ALL).await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    let j = r.json();
    assert_eq!(j["budget"], "result-bytes");
    assert_eq!(j["limit"], 200);
    assert_eq!(
        j["error"],
        "response exceeds the result size budget of 200 B"
    );
    assert_eq!(
        get(&s.app, "/ds/sparql?query=ASK%20%7B%7D").await.status,
        StatusCode::OK
    );
    // the Sparkles JSON format
    let r = get_with(&s.app, ALL, "accept", "application/x-sparkles+json").await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE);
    // Graph Store GET has its own budget (unlimited by default)
    let r = get(&s.app, "/ds/data?default").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.body.len() > 200);
    let m = metrics(&s.app).await;
    assert_eq!(
        sample(
            &m,
            r#"sparkles_budget_exceeded_total{dataset="ds",budget="result-bytes"}"#
        ),
        Some(2.0),
        "{m}"
    );
    // `send` limits what is serialized
    let r = tsv(&s.app, &format!("{ALL}&send=1")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let s = server_with(|st| st.limits.max_result_bytes = None);
    let r = tsv(&s.app, ALL).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.len() > 200);
    assert_eq!(get(&s.app, "/ds/data?default").await.status, StatusCode::OK);
}

#[tokio::test]
async fn graph_store_get_streams_large_graphs() {
    let s = server_with(|st| st.limits.max_result_bytes = None);
    let ds = s.state.get("ds").unwrap();
    // well past one 64 KiB chunk
    let nt: String = (0..5000)
        .map(|i| format!("<urn:s{i}> <urn:p> \"value number {i}\" .\n"))
        .collect();
    ds.store
        .load(&[Source::from_bytes(
            nt.into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let r = get_with(&s.app, "/ds/data", "accept", "application/n-quads").await;
    assert_eq!(r.status, StatusCode::OK);
    let mut got: Vec<&str> = std::str::from_utf8(&r.body).unwrap().lines().collect();
    got.sort_unstable();
    let mut dump = Vec::new();
    ds.store.dump_nquads(&mut dump).unwrap();
    let mut want: Vec<&str> = std::str::from_utf8(&dump).unwrap().lines().collect();
    want.sort_unstable();
    assert_eq!(got, want);
    assert!(r.body.len() > 128 << 10);
    // the streamed bytes are counted once the stream ends
    let m = metrics(&s.app).await;
    assert_eq!(
        sample(
            &m,
            r#"sparkles_response_bytes_total{dataset="ds",operation="gsp"}"#
        ),
        Some(r.body.len() as f64),
        "{m}"
    );
    // through the compression layer, which polls the body again after its end
    let gz = Request::get("/ds/data")
        .header(header::ACCEPT, "application/n-quads")
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .unwrap();
    let r2 = send(&s.app, gz).await;
    assert_eq!(r2.status, StatusCode::OK);
    let mut plain = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&r2.body[..]), &mut plain)
        .unwrap();
    assert_eq!(plain, r.body);
    // HEAD has the headers and no body
    let head = Request::head("/ds/data?default")
        .body(Body::empty())
        .unwrap();
    let r = send(&s.app, head).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.is_empty());
}

#[tokio::test]
async fn large_query_results_are_streamed() {
    let s = server_with(|st| st.limits.max_result_bytes = Some(64 << 20));
    let ds = s.state.get("ds").unwrap();
    // about 3 MiB as TSV: well past the 1 MiB a response is buffered for
    let nt: String = (0..30_000)
        .map(|i| format!("<urn:big:{i}> <urn:big:p> \"{}\" .\n", "x".repeat(80)))
        .collect();
    ds.store
        .load(&[Source::from_bytes(
            nt.into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let q =
        "/ds/sparql?query=SELECT%20%3Fs%20%3Fo%20WHERE%20%7B%3Fs%20%3Curn%3Abig%3Ap%3E%20%3Fo%7D";
    let r = tsv(&s.app, q).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.header("content-length").is_empty(), "streamed");
    assert_eq!(r.text().lines().count(), 30_001);
    assert!(r.body.len() > 2 << 20);
    // the commit header is set before the body streams
    assert!(!r.header("sparkles-commit").is_empty());
    let m = metrics(&s.app).await;
    assert_eq!(
        sample(
            &m,
            r#"sparkles_response_bytes_total{dataset="ds",operation="query"}"#
        ),
        Some(r.body.len() as f64),
        "{m}"
    );
    // a small result is still sent whole
    let r = tsv(&s.app, "/ds/sparql?query=ASK%20%7B%7D").await;
    assert_eq!(r.header("content-length"), r.body.len().to_string());
}

#[tokio::test]
async fn row_limit_keeps_507() {
    let s = server_with(|st| st.limits.max_rows = 5);
    let r = tsv(&s.app, ALL).await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    let j = r.json();
    assert_eq!(
        (
            j["budget"].as_str(),
            j["limit"].as_u64(),
            j["requested"].as_u64()
        ),
        (Some("rows"), Some(5), Some(9))
    );
}

// ------------------------------------------------------------------- access log ------

/// Collects everything the subscriber writes.
#[derive(Clone, Default)]
struct Buf(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
    type Writer = Buf;
    fn make_writer(&'a self) -> Buf {
        self.clone()
    }
}

fn access_lines(access_log: bool) -> Vec<String> {
    access_lines_of(|| async move {
        let s = server_with(|st| st.access_log = access_log);
        let r = get_with(&s.app, ALL, "x-request-id", "t-1").await;
        assert_eq!(r.status, StatusCode::OK);
        // health checks are logged at DEBUG, below this subscriber's level
        get(&s.app, "/$/ping").await;
    })
}

/// The `sparkles::access` lines logged while `run` runs on a current-thread runtime.
fn access_lines_of<F: std::future::Future<Output = ()>>(run: impl FnOnce() -> F) -> Vec<String> {
    // While a single dispatcher exists, tracing computes the interest of a callsite first
    // hit on another thread from that thread's default (none, in parallel tests) and
    // caches it for everyone; a second live dispatcher turns that shortcut off.
    let _second = tracing::Dispatch::new(tracing_subscriber::registry());
    let buf = Buf::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_current_span(true)
        .with_span_list(false)
        .with_writer(buf.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        // callsites first hit by tests running in parallel (without a subscriber) may
        // have cached that they are disabled
        tracing::callsite::rebuild_interest_cache();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run());
    });
    let text = String::from_utf8(buf.0.lock().clone()).unwrap();
    text.lines()
        .filter(|l| l.contains(r#""target":"sparkles::access""#))
        .map(str::to_string)
        .collect()
}

#[test]
fn access_log_has_one_structured_line_per_request() {
    let lines = access_lines(true);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    let l = &lines[0];
    let j: J = serde_json::from_str(l).unwrap();
    assert_eq!(j["level"], "INFO");
    assert_eq!(j["span"]["request_id"], "t-1");
    assert_eq!(j["span"]["route"], "/{ds}/sparql");
    let f = &j["fields"];
    assert_eq!(f["message"], "completed");
    assert_eq!(f["dataset"], "ds");
    assert_eq!(f["operation"], "query");
    assert_eq!(f["outcome"], "ok");
    assert_eq!(f["status"], 200);
    assert_eq!(f["rows"], 9);
    assert!(f["parse_ms"].is_number() && f["exec_ms"].is_number(), "{l}");
    assert!(
        f["total_ms"].is_number() && f["response_bytes"].is_number(),
        "{l}"
    );
    // neither the query nor the raw URI
    assert!(!l.contains("?o") && !l.contains("query="), "{l}");

    assert!(access_lines(false).is_empty());
}

// -------------------------------------------------------- write-time validation ------

/// Turn on `reject` validation of `ds`, then write: rejected, passed, skipped (a graph
/// outside the data graph). Returns the statuses.
#[cfg(feature = "shacl")]
async fn validated_writes(app: &Router) -> Vec<StatusCode> {
    const SHAPES: &str =
        "@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://example.org/> .
        ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
          sh:property [ sh:path <http://xmlns.com/foaf/0.1/name> ; sh:minCount 1 ] .";
    let cfg = serde_json::json!({ "mode": "reject", "shapes": { "inline": SHAPES } });
    let put = Request::put("/$/validation/ds")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(cfg.to_string()))
        .unwrap();
    let r = send(app, put).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mut out = Vec::new();
    for u in [
        "INSERT DATA { ex:eve a ex:Person }",
        "INSERT DATA { ex:frank a ex:Person ; foaf:name \"Frank\" }",
        "INSERT DATA { GRAPH ex:g2 { ex:x ex:p 1 } }",
    ] {
        let rq = Request::post("/ds/update")
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(Body::from(format!(
                "PREFIX ex: <http://example.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> {u}"
            )))
            .unwrap();
        out.push(send(app, rq).await.status);
    }
    out
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn validation_metrics_count_writes_by_status_and_severity() {
    let s = server();
    assert_eq!(
        validated_writes(&s.app).await,
        [
            StatusCode::UNPROCESSABLE_ENTITY,
            StatusCode::OK,
            StatusCode::OK
        ]
    );
    let m = metrics(&s.app).await;
    for (series, v) in [
        (
            r#"sparkles_validation_total{dataset="ds",language="shacl",status="passed"}"#,
            1.0,
        ),
        (
            r#"sparkles_validation_total{dataset="ds",language="shacl",status="rejected"}"#,
            1.0,
        ),
        (
            r#"sparkles_validation_total{dataset="ds",language="shacl",status="skipped"}"#,
            1.0,
        ),
        (
            r#"sparkles_validation_total{dataset="ds",language="shacl",status="timeout"}"#,
            0.0,
        ),
        (
            r#"sparkles_validation_duration_seconds_count{dataset="ds",language="shacl",strategy="full"}"#,
            2.0,
        ),
        (
            r#"sparkles_validation_duration_seconds_bucket{dataset="ds",language="shacl",strategy="full",le="+Inf"}"#,
            2.0,
        ),
        (
            r#"sparkles_validation_results_total{dataset="ds",language="shacl",severity="violation"}"#,
            1.0,
        ),
        (
            r#"sparkles_validation_results_total{dataset="ds",language="shacl",severity="warning"}"#,
            0.0,
        ),
        // a rejection is not a plain client error
        (
            r#"sparkles_requests_total{dataset="ds",operation="update",outcome="rejected"}"#,
            1.0,
        ),
        (
            r#"sparkles_requests_total{dataset="ds",operation="update",outcome="client_error"}"#,
            0.0,
        ),
    ] {
        assert_eq!(sample(&m, series), Some(v), "{series}\n{m}");
    }
    // no strategy without validations
    assert!(!m.contains(r#"strategy="incremental""#), "{m}");

    // datasets beyond the label cap share `$other`
    let s = server_with(|st| st.metrics = crate::obs::Metrics::new(true, 0));
    validated_writes(&s.app).await;
    let m = metrics(&s.app).await;
    assert_eq!(
        sample(
            &m,
            r#"sparkles_validation_total{dataset="$other",language="shacl",status="rejected"}"#
        ),
        Some(1.0),
        "{m}"
    );
    assert!(!m.contains(r#"dataset="ds""#), "{m}");
}

#[cfg(feature = "shacl")]
#[test]
fn access_log_carries_the_validation_status() {
    let lines = access_lines_of(|| async {
        let s = server_with(|st| st.access_log = true);
        validated_writes(&s.app).await;
    });
    let updates: Vec<J> = lines
        .iter()
        .map(|l| serde_json::from_str::<J>(l).unwrap()["fields"].clone())
        .filter(|f| f["operation"] == "update")
        .collect();
    assert_eq!(updates.len(), 3, "{lines:#?}");
    let got: Vec<_> = updates
        .iter()
        .map(|f| (f["outcome"].clone(), f["validation"].clone()))
        .collect();
    assert_eq!(
        got,
        [
            ("rejected".into(), "rejected".into()),
            ("ok".into(), "passed".into()),
            ("ok".into(), "skipped".into()),
        ]
    );
    assert!(updates.iter().all(|f| f["validation_ms"].is_number()));
    // requests without validation have no such fields
    let admin = lines
        .iter()
        .map(|l| serde_json::from_str::<J>(l).unwrap()["fields"].clone())
        .find(|f| f["operation"] == "admin")
        .unwrap();
    assert!(admin.get("validation").is_none(), "{admin}");
}

// ----------------------------------------------------------------- cancellation ------

#[tokio::test]
async fn client_disconnect_cancels_the_query() {
    let s = server_with(|st| st.limits.query_memory_bytes = None);
    // a long chain: `?x ex:next* ?y` runs a long walk from every node
    let chain = s.state.attach("chain", DbType::Mem, None).unwrap();
    let mut nt = String::new();
    for i in 0..3000 {
        nt.push_str(&format!("<urn:n{i}> <urn:next> <urn:n{}> .\n", i + 1));
    }
    chain
        .store
        .load(&[Source::from_bytes(
            nt.into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let heavy = Request::post("/chain/sparql?timeout=30")
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .body(Body::from(
            "SELECT (COUNT(*) AS ?n) WHERE { ?x <urn:next>* ?y }",
        ))
        .unwrap();
    let app = s.app.clone();
    let task = tokio::spawn(async move { app.oneshot(heavy).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let m = metrics(&s.app).await;
    assert_eq!(
        sample(&m, r#"sparkles_requests_active{operation="query"}"#),
        Some(1.0),
        "the query must still be running: {m}"
    );
    task.abort();
    let cancelled =
        r#"sparkles_requests_total{dataset="chain",operation="query",outcome="cancelled"}"#;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let m = metrics(&s.app).await;
        if sample(&m, cancelled) == Some(1.0) {
            assert_eq!(
                sample(&m, r#"sparkles_requests_active{operation="query"}"#),
                Some(0.0)
            );
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{m}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn error_bodies_carry_the_request_id() {
    let s = server();
    let r = get(&s.app, "/nope/sparql?query=ASK%7B%7D").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let j = r.json();
    assert_eq!(j["requestId"], r.header("x-request-id"), "{j}");
    assert!(j["error"].as_str().unwrap().contains("nope"));
    // an incoming id is the one reported
    let r = get_with(
        &s.app,
        "/ds/sparql?query=SELEC",
        "x-request-id",
        "client-42",
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["requestId"], "client-42");
    assert_eq!(r.header("content-length"), r.body.len().to_string());
}
