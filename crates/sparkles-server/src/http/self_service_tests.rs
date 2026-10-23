//! SERVICE calls to this server's own query endpoints run in process
//! (X08-P1-nested-service).

use super::router;
use super::self_service::Listener;
use crate::obs::Phase;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tower::ServiceExt;

const MB: u64 = 1 << 20;
/// The port the tests pretend the server listens on. Nothing listens there, so a call
/// that went over HTTP to it would fail.
const PORT: u16 = 9;

struct Server {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

fn server() -> Server {
    server_on(PORT)
}

/// The server, listening on a port of its own over real TCP, so that a SERVICE call it
/// sent itself over HTTP would be admitted like any request.
async fn served() -> (Server, u16) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let s = server_on(port);
    let service = s
        .app
        .clone()
        .into_make_service_with_connect_info::<crate::auth::Peer>();
    tokio::spawn(async move { axum::serve(l, service).await });
    (s, port)
}

fn server_on(port: u16) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
        Some(64 * MB),
    )
    .unwrap();
    state.admission.wait = Duration::from_millis(300);
    // calls that do go over HTTP reach a listener on 127.0.0.1
    state.outbound.allow_private = true;
    let state = Arc::new(state);
    state
        .listener
        .set(Listener {
            scheme: "http",
            ip: std::net::Ipv4Addr::UNSPECIFIED.into(),
            port,
        })
        .unwrap();
    let ds = state.attach("ds", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            b"<urn:a> <urn:p> <urn:b> .\n".to_vec(),
            oxrdfio::RdfFormat::NTriples,
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

async fn select(app: &Router, q: &str) -> (StatusCode, String) {
    let uri = format!(
        "/ds/sparql?query={}",
        percent_encoding::utf8_percent_encode(q, percent_encoding::NON_ALPHANUMERIC)
    );
    let res = app
        .clone()
        .oneshot(
            Request::get(uri)
                .header(header::ACCEPT, "text/csv")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// A local HTTP listener that counts connections and answers 500.
fn counting_listener() -> (u16, Arc<AtomicUsize>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            n2.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = c.set_read_timeout(Some(Duration::from_millis(200)));
            let _ = c.read(&mut buf);
            let _ = c.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
        }
    });
    (port, n)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_call_to_this_server_runs_under_its_callers_grant() {
    let (s, port) = served().await;
    let pool = s.state.catalog.pool().unwrap().clone();
    // other queries hold 48 of the 64 MiB: the caller's 16 MiB initial grant fits, and
    // a second request for 16 MiB would wait for the caller and answer 503
    let held = pool.try_grant(48 * MB).unwrap();
    for endpoint in [
        format!("http://127.0.0.1:{port}/ds/sparql"),
        format!("http://localhost:{port}/ds/query"),
        format!("http://[::1]:{port}/ds"),
        format!("http://0.0.0.0:{port}/ds/sparql"),
    ] {
        let started = std::time::Instant::now();
        let (status, body) = select(
            &s.app,
            &format!("SELECT ?o {{ SERVICE <{endpoint}> {{ <urn:a> <urn:p> ?o }} }}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{endpoint}: {body}");
        assert!(body.contains("urn:b"), "{endpoint}: {body}");
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "{endpoint} waited"
        );
    }
    assert_eq!(pool.stats().rejected, 0);
    assert_eq!(pool.waiting(), 0);
    drop(held);
    assert_eq!(pool.granted(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_service_calls_to_this_server_stop_at_the_depth_limit() {
    let s = server();
    let nest = |n: usize| {
        let mut inner = "<urn:a> <urn:p> ?o".to_string();
        for _ in 0..n {
            inner = format!("SERVICE <http://127.0.0.1:{PORT}/ds/sparql> {{ {inner} }}");
        }
        format!("SELECT ?o {{ {inner} }}")
    };
    let (status, body) = select(&s.app, &nest(8)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("urn:b"), "{body}");
    let (status, body) = select(&s.app, &nest(9)).await;
    assert_ne!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("SERVICE calls to this server nest more than 8 deep"),
        "{body}"
    );
    // SILENT hides it as it hides any failed SERVICE call
    let silent = nest(9).replacen("SERVICE", "SERVICE SILENT", 1);
    let (status, body) = select(&s.app, &silent).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_iri_that_does_not_name_this_server_goes_over_http() {
    let s = server();
    let (port, conns) = counting_listener();
    let mut expected = 0;
    for endpoint in [
        // another port on a loopback address
        format!("http://127.0.0.1:{port}/ds/sparql"),
        // the same port with a path that is not a query endpoint
        format!("http://127.0.0.1:{port}/ds/update"),
    ] {
        let (status, body) = select(
            &s.app,
            &format!("SELECT ?o {{ SERVICE <{endpoint}> {{ <urn:a> <urn:p> ?o }} }}"),
        )
        .await;
        expected += 1;
        assert_ne!(status, StatusCode::OK, "{endpoint}: {body}");
        assert_eq!(conns.load(Ordering::SeqCst), expected, "{endpoint}");
    }
    // the listener's port, but not one of its names, a dataset it does not serve, a
    // query string or a branch: none of these is answered in process, and nothing
    // listens on the port
    for endpoint in [
        format!("http://127.0.0.2:{PORT}/ds/sparql"),
        format!("http://example.invalid:{PORT}/ds/sparql"),
        format!("http://127.0.0.1:{PORT}/other/sparql"),
        format!("http://127.0.0.1:{PORT}/ds/sparql?default-graph-uri=urn:g"),
        format!("http://127.0.0.1:{PORT}/ds@b/sparql"),
        format!("http://:pw@127.0.0.1:{PORT}/ds/sparql"),
        format!("http://@127.0.0.1:{PORT}/ds/sparql"),
        format!("http:////@127.0.0.1:{PORT}/ds/sparql"),
        format!("http:///127.0.0.1:{PORT}/ds/sparql"),
        format!("https://127.0.0.1:{PORT}/ds/sparql"),
        // spellings that the URL parser rewrites into this server's endpoint
        format!("http://127.1:{PORT}/ds/sparql"),
        format!("http://2130706433:{PORT}/ds/sparql"),
        format!("http://0x7f.0.0.1:{PORT}/ds/sparql"),
        format!("http://%6cocalhost:{PORT}/ds/sparql"),
        format!("http://127.0.0.1:0{PORT}/ds/sparql"),
        format!("http://127.0.0.1:{PORT}/other/%2e%2e/ds/sparql"),
        format!("http://127.0.0.1:{PORT}/other/../ds/sparql"),
        format!("http://127.0.0.1:{PORT}/./ds/sparql"),
        format!("http://127.0.0.1:{PORT}\\ds/sparql"),
    ] {
        let (status, body) = select(
            &s.app,
            &format!("SELECT ?o {{ SERVICE <{endpoint}> {{ <urn:a> <urn:p> ?o }} }}"),
        )
        .await;
        assert_ne!(status, StatusCode::OK, "{endpoint}: {body}");
        assert!(!body.contains("urn:b"), "{endpoint}: {body}");
    }
}

async fn post_update(app: &Router, update: &str) -> (StatusCode, String) {
    let res = app
        .clone()
        .oneshot(
            Request::post("/ds/update")
                .header(header::CONTENT_TYPE, "application/sparql-update")
                .body(Body::from(update.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_reads_this_server_in_process_under_its_grant() {
    let (s, port) = served().await;
    let pool = s.state.catalog.pool().unwrap().clone();
    let held = pool.try_grant(48 * MB).unwrap();
    let (status, body) = post_update(
        &s.app,
        &format!(
            "INSERT {{ <urn:copy> <urn:p> ?o }} WHERE {{ \
             SERVICE <http://127.0.0.1:{port}/ds/sparql> {{ <urn:a> <urn:p> ?o }} }}"
        ),
    )
    .await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(pool.stats().rejected, 0);
    drop(held);
    let (status, body) = select(&s.app, "SELECT ?o { <urn:copy> <urn:p> ?o }").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("urn:b"), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_call_to_this_server_keeps_the_callers_lowered_budgets() {
    let s = server();
    // the inner query's VALUES has ten rows, above the caller's row budget of one,
    // while the caller's own pattern has one row
    let q = format!(
        "SELECT ?sum {{ SERVICE <http://127.0.0.1:{PORT}/ds/sparql> {{ \
         SELECT (SUM(?x) AS ?sum) {{ VALUES ?x {{ 1 2 3 4 5 6 7 8 9 10 }} }} }} }}"
    );
    let uri = format!(
        "/ds/sparql?max-rows=1&query={}",
        percent_encoding::utf8_percent_encode(&q, percent_encoding::NON_ALPHANUMERIC)
    );
    let res = s
        .app
        .clone()
        .oneshot(
            Request::get(uri)
                .header(header::ACCEPT, "text/csv")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&body);
    assert_ne!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("55"), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_call_to_an_offline_dataset_of_this_server_fails() {
    let s = server();
    let off = s.state.attach("off", DbType::Mem, None).unwrap();
    off.offline.store(true, Ordering::Relaxed);
    let q = format!("SELECT * {{ SERVICE <http://127.0.0.1:{PORT}/off/sparql> {{ ?s ?p ?o }} }}");
    let (status, body) = select(&s.app, &q).await;
    assert_ne!(status, StatusCode::OK, "{body}");
    assert!(body.contains("offline"), "{body}");
    // SILENT hides it, as it hides the 503 of a direct request
    let (status, body) = select(&s.app, &q.replace("SERVICE", "SERVICE SILENT")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
