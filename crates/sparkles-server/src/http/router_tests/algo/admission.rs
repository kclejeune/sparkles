//! Body-selected algorithm authorization and class admission through the real router.
use super::*;
use crate::ratelimit::{Config, RateLimiter};
use axum::response::{IntoResponse, Response};

const READ: &str = r#"{"algorithm":"wcc","projection":{"predicates":["http://ex/p"]}}"#;
const WRITE: &str = r#"{"algorithm":"pageRank","projection":{"predicates":["http://ex/p"]},"write":{"graph":"http://ex/out","predicates":{"score":"http://ex/rank"}}}"#;

struct Fixture {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

fn fixture(flags: &[&str], authenticated: bool, read_only: bool) -> Fixture {
    configured(flags, authenticated, read_only, |_| {})
}

fn configured(
    flags: &[&str],
    authenticated: bool,
    read_only: bool,
    configure: impl FnOnce(&mut AppState),
) -> Fixture {
    policy_fixture(flags, authenticated, read_only, "", configure)
}

fn policy_fixture(
    flags: &[&str],
    authenticated: bool,
    read_only: bool,
    extra_policy: &str,
    configure: impl FnOnce(&mut AppState),
) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut state =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    state.read_only = read_only;
    let mut config = Config::default();
    for flag in flags {
        config.apply_flag(flag).unwrap();
    }
    state.rate_limit = Some(Arc::new(
        RateLimiter::new(&config)
            .unwrap()
            .with_keyer(Arc::new(crate::auth::PrincipalKeyer)),
    ));
    #[cfg(feature = "auth")]
    if authenticated {
        let mut text =
            "version = 1\n[server]\npublic_url = \"http://localhost:3030\"\n".to_string();
        for (name, token, endpoint, level) in [
            ("reader", 'H', "query", "read"),
            ("reader2", 'M', "query", "read"),
            ("writer", 'I', "update", "write"),
            ("info", 'J', "info", "read"),
        ] {
            text.push_str(&format!("\n[[tokens]]\nname = \"{name}\"\nhash = \"{}\"\n[[tokens.grants]]\ndataset = \"g\"\nlevel = \"{level}\"\nendpoints = [\"{endpoint}\"]\n", crate::auth::token_hash(&token_text(token))));
        }
        text.push_str(extra_policy);
        let path = dir.path().join("auth.toml");
        std::fs::write(&path, text).unwrap();
        state.auth = Some(Arc::new(
            crate::auth::Auth::open(&path, dir.path()).unwrap().0,
        ));
    }
    #[cfg(not(feature = "auth"))]
    assert!(!authenticated && extra_policy.is_empty());
    configure(&mut state);
    let state = Arc::new(state);
    for name in ["g", "other"] {
        let ds = state.attach(name, DbType::Persistent, None).unwrap();
        ds.store
            .load(&[Source::from_bytes(
                GRAPH.as_bytes().to_vec(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
    }
    let app = super::super::super::router(state.clone());
    Fixture {
        _dir: dir,
        state,
        app,
    }
}

#[cfg(feature = "auth")]
fn token_text(token: char) -> String {
    format!("spk_{}", token.to_string().repeat(43))
}

fn request(path: &str, body: &str, token: Option<char>) -> Request<Body> {
    let mut req = json_req("POST", path, body);
    #[cfg(feature = "auth")]
    if let Some(token) = token {
        req.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {}", token_text(token)).parse().unwrap(),
        );
    }
    #[cfg(not(feature = "auth"))]
    assert!(token.is_none());
    // Keep the same client key across every request in rate tests.
    req.extensions_mut().insert(axum::extract::ConnectInfo(
        "192.0.2.10:25020".parse::<std::net::SocketAddr>().unwrap(),
    ));
    req
}

async fn raw(s: &Fixture, path: &str, body: &str, token: Option<char>) -> Response {
    tokio::time::timeout(
        Duration::from_secs(10),
        s.app.clone().oneshot(request(path, body, token)),
    )
    .await
    .unwrap()
    .unwrap()
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn body_aware_query_only_reads_rows_and_summary() {
    let s = fixture(&[], true, false);
    for path in ["/$/algo/g/run", "/$/algo/g/run?summary=true"] {
        let r = send(&s.app, request(path, READ, Some('H'))).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert!(
            !r.json()["results"]["bindings"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn body_aware_update_only_creates_task() {
    let s = fixture(&[], true, false);
    let r = send(&s.app, request("/$/algo/g/run", WRITE, Some('I'))).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    let task = super::super::tasks::wait_done(&s.state, &id).await;
    assert_eq!(task.state, task_state::DONE, "{:?}", task.message);
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn body_aware_info_cannot_run() {
    let s = fixture(&[], true, false);
    let r = send(&s.app, request("/$/algo/g/run", READ, Some('J'))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn body_aware_denied_mode_precedes_restore_disclosure() {
    let s = fixture(&[], true, false);
    let _restoring = s
        .state
        .catalog
        .reserve(
            "g",
            sparkles::catalog::ReservationKind::Restore,
            "secret-restore-task",
        )
        .unwrap();
    let r = send(&s.app, request("/$/algo/g/run", READ, Some('J'))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert!(!r.text().contains("secret-restore-task"));
    let r = send(&s.app, request("/$/algo/g/run", READ, Some('H'))).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert!(r.text().contains("secret-restore-task"));
}

#[tokio::test]
async fn body_aware_query_rate_covers_rows_and_summary() {
    let s = fixture(&["query=1/min"], false, false);
    let first = raw(&s, "/$/algo/g/run", READ, None).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert!(first.headers().contains_key("ratelimit"));
    drop(first);
    let r = raw(&s, "/$/algo/g/run?summary=true", READ, None).await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(r.headers().contains_key(header::RETRY_AFTER));
}

#[tokio::test]
async fn body_aware_query_body_retains_concurrency() {
    let s = fixture(&["query=concurrency=1"], false, false);
    let first = raw(&s, "/$/algo/g/run?execution=eager", READ, None).await;
    assert_eq!(first.status(), StatusCode::OK);
    let busy = raw(&s, "/$/algo/g/run?summary=true", READ, None).await;
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(first);
    assert_eq!(
        raw(&s, "/$/algo/g/run", READ, None).await.status(),
        StatusCode::OK
    );
}

// Finding-validation regressions originally failing on a0101f50.
async fn finding_json_error_keeps_raw_body(authenticated: bool, token: Option<char>) {
    let s = fixture(&["query=concurrency=1"], authenticated, false);
    let first = raw(&s, "/$/algo/g/run?execution=bad", READ, token).await;
    assert_eq!(first.status(), StatusCode::BAD_REQUEST);
    let id = first.headers()["x-request-id"].to_str().unwrap().to_owned();
    assert_eq!(first.headers()["x-content-type-options"], "nosniff");
    let (parts, body) = first.into_parts();
    drop(parts);
    // Hold the raw unread Body after dropping all Response Parts/extensions.
    let busy = raw(&s, "/$/algo/g/run?execution=eager", READ, token).await;
    let held_status = busy.status();
    drop(busy);
    let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap();
    let error: J = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(error["requestId"], id);
    assert!(error["error"].as_str().unwrap().contains("execution"));
    let after_eof = raw(&s, "/$/algo/g/run?execution=eager", READ, token).await;
    let released_status = after_eof.status();
    drop(after_eof);
    assert_eq!(released_status, StatusCode::OK);
    assert_eq!(
        held_status,
        StatusCode::SERVICE_UNAVAILABLE,
        "admitted JSON400 must retain Query concurrency through raw unread body"
    );
}

#[tokio::test]
async fn finding_json_error_body_holds_query_no_auth() {
    finding_json_error_keeps_raw_body(false, None).await;
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn finding_json_error_body_holds_query_with_auth() {
    finding_json_error_keeps_raw_body(true, Some('H')).await;
}

#[tokio::test]
async fn finding_success_raw_body_control() {
    let s = fixture(&["query=concurrency=1"], false, false);
    let first = raw(&s, "/$/algo/g/run?execution=eager", READ, None).await;
    assert_eq!(first.status(), StatusCode::OK);
    let (parts, body) = first.into_parts();
    drop(parts);
    let busy = raw(&s, "/$/algo/g/run?execution=eager", READ, None).await;
    let held_status = busy.status();
    drop(busy);
    drop(body);
    let released = raw(&s, "/$/algo/g/run?execution=eager", READ, None).await;
    let released_status = released.status();
    drop(released);
    assert_eq!(held_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(released_status, StatusCode::OK);
}

async fn finding_update_error_keeps_raw_body(authenticated: bool, token: Option<char>) {
    let s = fixture(&["update=concurrency=1"], authenticated, false);
    let _reservation = s
        .state
        .catalog
        .reserve(
            "g",
            sparkles::catalog::ReservationKind::Restore,
            "owned-restore",
        )
        .unwrap();
    let first = raw(&s, "/$/algo/g/run", WRITE, token).await;
    assert_eq!(first.status(), StatusCode::SERVICE_UNAVAILABLE);
    let (parts, body) = first.into_parts();
    let id = parts.headers["x-request-id"].to_str().unwrap().to_owned();
    assert_eq!(parts.headers[header::RETRY_AFTER], "5");
    drop(parts);
    let busy = raw(&s, "/$/algo/g/run", WRITE, token).await;
    let busy_json: J = serde_json::from_slice(
        &axum::body::to_bytes(busy.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .unwrap();
    let first_json: J =
        serde_json::from_slice(&axum::body::to_bytes(body, 1 << 20).await.unwrap()).unwrap();
    assert_eq!(first_json["code"], "dataset-restoring");
    assert_eq!(first_json["requestId"], id);
    let after = raw(&s, "/$/algo/g/run", WRITE, token).await;
    let after_json: J = serde_json::from_slice(
        &axum::body::to_bytes(after.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(after_json["code"], "dataset-restoring");
    assert!(s.state.tasks.lock().is_empty());
    assert_eq!(
        busy_json["reason"], "concurrency",
        "admitted Update503 must hold slot, not disclose another restore response: {busy_json}"
    );
}

#[tokio::test]
async fn finding_json_error_body_holds_update_no_auth() {
    finding_update_error_keeps_raw_body(false, None).await;
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn finding_json_error_body_holds_update_with_auth() {
    finding_update_error_keeps_raw_body(true, Some('I')).await;
}

const FINDING_TURTLE: &str = "<urn:report> <urn:result> <urn:rejected> .\n";

/// Controlled downstream error only. This is not an algorithm/SHACL execution.
/// It exercises actual admission, ErrorJson/RejectionTurtle and outer formatters.
fn finding_error_adapter(s: &Fixture, inner_formatter: bool) -> Router {
    let app = Router::new()
        .route(
            "/$/algo/{ds}/run",
            axum::routing::post(|| async {
                crate::http::err_body(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    serde_json::json!({"error":"controlled downstream rejection",
                        "_validationHeader":"rejected", "_turtle":FINDING_TURTLE}),
                )
                .into_response()
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            s.state.clone(),
            crate::http::algo::admit_run,
        ));
    let app = if inner_formatter {
        app.layer(axum::middleware::from_fn(crate::http::error_request_id))
    } else {
        app
    };
    app.layer(axum::middleware::from_fn_with_state(
        s.state.clone(),
        crate::auth::middleware,
    ))
    .layer(axum::middleware::from_fn_with_state(
        s.state.clone(),
        crate::obs::observe,
    ))
}

async fn finding_adapter_keeps_body(authenticated: bool, token: Option<char>, turtle: bool) {
    let s = fixture(&["query=concurrency=1"], authenticated, false);
    // No inner formatter isolates the real outer auth add_request_id path.
    let app = finding_error_adapter(&s, turtle);
    let mut req = request("/$/algo/g/run", READ, token);
    if turtle {
        req.headers_mut()
            .insert(header::ACCEPT, "text/turtle".parse().unwrap());
    }
    let first = app.clone().oneshot(req).await.unwrap();
    assert_eq!(first.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let (parts, body) = first.into_parts();
    let id = parts.headers["x-request-id"].to_str().unwrap().to_owned();
    if turtle {
        assert_eq!(parts.headers[header::CONTENT_TYPE], "text/turtle");
        assert_eq!(
            parts.headers[header::CONTENT_LENGTH],
            FINDING_TURTLE.len().to_string()
        );
        assert_eq!(parts.headers["sparkles-validation"], "rejected");
    }
    drop(parts);
    let busy = app
        .clone()
        .oneshot(request("/$/algo/g/run", READ, token))
        .await
        .unwrap();
    let held_status = busy.status();
    drop(busy);
    let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap();
    if turtle {
        assert_eq!(&bytes[..], FINDING_TURTLE.as_bytes());
    } else {
        assert_eq!(
            serde_json::from_slice::<J>(&bytes).unwrap()["requestId"],
            id
        );
    }
    let released = app
        .oneshot(request("/$/algo/g/run", READ, token))
        .await
        .unwrap();
    let released_status = released.status();
    drop(released);
    assert_eq!(released_status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        held_status,
        StatusCode::SERVICE_UNAVAILABLE,
        "labelled controlled formatter adapter must preserve raw body permit; turtle={turtle}"
    );
}

#[tokio::test]
async fn finding_turtle_formatter_body_holds_no_auth() {
    finding_adapter_keeps_body(false, None, true).await;
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn finding_turtle_formatter_body_holds_with_auth() {
    finding_adapter_keeps_body(true, Some('H'), true).await;
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn finding_outer_auth_json_formatter_body_holds() {
    finding_adapter_keeps_body(true, Some('H'), false).await;
}

#[tokio::test]
async fn finding_busy_error_does_not_spend_rate_token() {
    let s = fixture(&["query=1/min,burst=2,concurrency=1"], false, false);
    let first = raw(&s, "/$/algo/g/run?execution=bad", READ, None).await;
    assert_eq!(first.status(), StatusCode::BAD_REQUEST);
    assert!(
        first.headers()["ratelimit-policy"]
            .to_str()
            .unwrap()
            .contains("query")
    );
    let body = first.into_body();
    let busy = raw(&s, "/$/algo/g/run?execution=eager", READ, None).await;
    let held_status = busy.status();
    drop(busy);
    drop(body);
    let released = raw(&s, "/$/algo/g/run?execution=eager", READ, None).await;
    let released_status = released.status();
    drop(released);
    eprintln!("finding token witness: held={held_status} released={released_status}");
    assert_eq!(held_status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        released_status,
        StatusCode::OK,
        "busy attempt cannot spend second token"
    );
}

fn probed(
    path: &str,
    body: &str,
    token: Option<char>,
    probe: &crate::http::algo::probe::Probe,
) -> Request<Body> {
    let mut req = request(path, body, token);
    req.extensions_mut().insert(probe.clone());
    req
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn body_aware_parse_grant_restore_lookup_order_and_denials() {
    use crate::http::algo::probe::Probe;
    let s = fixture(&[], true, false);
    let p = Probe::default();
    let r = send(&s.app, probed("/$/algo/g/run", READ, Some('H'), &p)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        *p.events.lock(),
        ["parse", "grant", "restore", "lookup", "projection"]
    );
    for (name, body, token, status, events) in [
        (
            "g",
            READ,
            Some('J'),
            StatusCode::FORBIDDEN,
            vec!["parse", "grant"],
        ),
        (
            "other",
            READ,
            Some('H'),
            StatusCode::NOT_FOUND,
            vec!["parse", "grant"],
        ),
        (
            "missing",
            READ,
            Some('H'),
            StatusCode::NOT_FOUND,
            vec!["parse", "grant"],
        ),
        ("g", "{", Some('H'), StatusCode::BAD_REQUEST, vec!["parse"]),
        (
            "missing",
            "{}",
            Some('H'),
            StatusCode::BAD_REQUEST,
            vec!["parse"],
        ),
        (
            "g",
            r#"{"algorithm":"wcc","write":{"graph":"urn:o","mode":"bad","predicates":{}}}"#,
            Some('I'),
            StatusCode::BAD_REQUEST,
            vec!["parse"],
        ),
        (
            "g",
            r#"{"algorithm":"wcc","unknown":true}"#,
            Some('H'),
            StatusCode::BAD_REQUEST,
            vec!["parse"],
        ),
        ("g", READ, Some('Z'), StatusCode::UNAUTHORIZED, vec![]),
        (
            "g",
            READ,
            None,
            StatusCode::UNAUTHORIZED,
            vec!["parse", "grant"],
        ),
    ] {
        let p = Probe::default();
        let r = send(
            &s.app,
            probed(&format!("/$/algo/{name}/run"), body, token, &p),
        )
        .await;
        assert_eq!(r.status, status, "{}", r.text());
        assert_eq!(*p.events.lock(), events);
        assert!(s.state.tasks.lock().is_empty());
    }
    // Exact POST only. Projection policy stays Info for reads and Admin for writes.
    for method in ["GET", "PUT", "DELETE", "PATCH"] {
        let mut req = request("/$/algo/g/run", READ, Some('H'));
        *req.method_mut() = method.parse().unwrap();
        assert_eq!(send(&s.app, req).await.status, StatusCode::FORBIDDEN);
    }
    for (method, path, token, status) in [
        ("GET", "/$/algo/g/projections", 'J', StatusCode::OK),
        ("GET", "/$/algo/g/projections", 'H', StatusCode::FORBIDDEN),
        ("PUT", "/$/algo/g/projections/p", 'I', StatusCode::FORBIDDEN),
        (
            "DELETE",
            "/$/algo/g/projections/p",
            'I',
            StatusCode::FORBIDDEN,
        ),
    ] {
        let mut req = request(path, "{}", Some(token));
        *req.method_mut() = method.parse().unwrap();
        assert_eq!(send(&s.app, req).await.status, status);
    }
    assert_eq!(
        send(
            &s.app,
            request("/$/algo/g/run?branch=main", READ, Some('H'))
        )
        .await
        .status,
        StatusCode::OK
    );
    let p = Probe::default();
    assert_eq!(
        send(
            &s.app,
            probed("/$/algo/g/run?branch=other", READ, Some('H'), &p)
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    assert!(p.events.lock().is_empty());
}

#[tokio::test]
async fn body_aware_admin_is_not_charged_and_update_has_its_own_rate() {
    let s = fixture(
        &["admin=1/min", "query=2/min", "update=1/min"],
        false,
        false,
    );
    let r = send(&s.app, json_req("PUT", "/$/algo/g/projections/p", "{}")).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    for path in ["/$/algo/g/run", "/$/algo/g/run?summary=true"] {
        let r = raw(&s, path, READ, None).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert!(
            r.headers()["ratelimit-policy"]
                .to_str()
                .unwrap()
                .contains("query")
        );
    }
    assert_eq!(
        raw(&s, "/$/algo/g/run", READ, None).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    let r = send(&s.app, request("/$/algo/g/run", WRITE, None)).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        super::super::tasks::wait_done(&s.state, &id).await.state,
        task_state::DONE
    );
    let before = s.state.tasks.lock().len();
    assert_eq!(
        raw(&s, "/$/algo/other/run", WRITE, None).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(s.state.tasks.lock().len(), before);
}

#[tokio::test]
async fn body_aware_read_only_writes_refuse_before_restore_or_task() {
    let s = fixture(&["update=1/min"], false, true);
    let p = crate::http::algo::probe::Probe::default();
    let _restore = s
        .state
        .catalog
        .reserve(
            "g",
            sparkles::catalog::ReservationKind::Restore,
            "not-for-readonly",
        )
        .unwrap();
    let r = send(&s.app, probed("/$/algo/g/run", WRITE, None, &p)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(*p.events.lock(), ["parse", "grant"]);
    assert!(!r.text().contains("not-for-readonly"));
    assert!(s.state.tasks.lock().is_empty());
}

#[tokio::test]
async fn body_aware_caps_and_compression_preserve_request_parts() {
    use crate::http::algo::probe::Probe;
    use std::io::Write as _;
    let s = configured(&[], false, false, |st| {
        st.limits.max_admin_body_bytes = Some(128);
        st.limits.max_decompressed_bytes = Some(256);
    });
    let gzip = |data: &[u8]| {
        let mut out = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        out.write_all(data).unwrap();
        out.finish().unwrap()
    };
    let p = Probe::default();
    let mut req = probed("/$/algo/g/run", READ, None, &p);
    *req.body_mut() = Body::from(gzip(READ.as_bytes()));
    req.headers_mut()
        .insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
    req.headers_mut()
        .insert(header::ACCEPT, "text/csv".parse().unwrap());
    let r = send(&s.app, req).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.content_type.starts_with("text/csv"));
    assert!(r.text().contains("component"));
    assert_eq!(
        *p.events.lock(),
        ["parse", "grant", "restore", "lookup", "projection"]
    );
    for compressed in [false, true] {
        let p = Probe::default();
        let mut req = probed("/$/algo/g/run", READ, None, &p);
        *req.body_mut() = Body::from(if compressed {
            gzip(&[b' '; 200])
        } else {
            vec![b' '; 200]
        });
        if compressed {
            req.headers_mut()
                .insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
        }
        let r = send(&s.app, req).await;
        assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
        assert!(p.events.lock().is_empty());
    }
    let p = Probe::default();
    let mut req = probed("/$/algo/g/run", READ, None, &p);
    *req.body_mut() = Body::from_stream(futures_util::stream::iter([
        Ok::<_, std::io::Error>(vec![b' '; 80]),
        Ok(vec![b' '; 80]),
    ]));
    assert_eq!(
        send(&s.app, req).await.status,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert!(p.events.lock().is_empty());
    let mut req = request("/$/algo/g/run", "invalid gzip", None);
    req.headers_mut()
        .insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
    assert_eq!(send(&s.app, req).await.status, StatusCode::BAD_REQUEST);
    let s = configured(&[], false, false, |st| {
        st.limits.max_admin_body_bytes = Some(512);
        st.limits.max_decompressed_bytes = Some(32);
    });
    let mut req = request("/$/algo/g/run", READ, None);
    *req.body_mut() = Body::from(gzip(READ.as_bytes()));
    req.headers_mut()
        .insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
    assert_eq!(
        send(&s.app, req).await.status,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn body_aware_query_blocking_share_survives_disconnect_all_modes() {
    use crate::http::algo::probe::{Gate, Probe, Release};
    for mode in ["eager", "streaming", "auto"] {
        let s = fixture(&["query=1/min,burst=2,concurrency=1"], false, false);
        let gate = Arc::new(Gate::default());
        let release = Release(gate.clone());
        let probe = Probe {
            blocking: Some(gate.clone()),
            ..Default::default()
        };
        let req = probed(
            &format!("/$/algo/g/run?execution={mode}"),
            READ,
            None,
            &probe,
        );
        let app = s.app.clone();
        let mut running = AbortOnDrop(Some(tokio::spawn(async move {
            app.oneshot(req).await.unwrap()
        })));
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .unwrap();
        let busy = raw(&s, "/$/algo/g/run", READ, None).await;
        assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
        running.0.as_ref().unwrap().abort();
        assert!(running.0.take().unwrap().await.unwrap_err().is_cancelled());
        assert!(
            probe
                .cancel
                .lock()
                .as_ref()
                .unwrap()
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        assert_eq!(
            raw(&s, "/$/algo/g/run", READ, None).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        gate.release();
        tokio::time::timeout(Duration::from_secs(5), gate.done.notified())
            .await
            .unwrap();
        assert!(!gate.timed_out.load(std::sync::atomic::Ordering::Relaxed));
        drop(release);
        let next = raw(&s, "/$/algo/g/run", READ, None).await;
        assert_eq!(next.status(), StatusCode::OK);
        drop(next);
        // Busy attempts did not consume the second token.
        assert_eq!(
            raw(&s, "/$/algo/g/run", READ, None).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }
}

#[tokio::test]
async fn body_aware_stream_producer_share_survives_dropped_body() {
    use crate::http::algo::probe::{Gate, Probe, Release};
    for mode in ["streaming", "auto"] {
        let s = fixture(&["query=concurrency=1"], false, false);
        let gate = Arc::new(Gate::default());
        let release = Release(gate.clone());
        let probe = Probe {
            producer: Some(gate.clone()),
            ..Default::default()
        };
        let first = tokio::time::timeout(
            Duration::from_secs(5),
            s.app.clone().oneshot(probed(
                &format!("/$/algo/g/run?execution={mode}"),
                READ,
                None,
                &probe,
            )),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .unwrap();
        assert_eq!(
            raw(&s, "/$/algo/g/run", READ, None).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        drop(first);
        assert!(
            probe
                .cancel
                .lock()
                .as_ref()
                .unwrap()
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        assert_eq!(
            raw(&s, "/$/algo/g/run", READ, None).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        gate.release();
        tokio::time::timeout(Duration::from_secs(5), gate.done.notified())
            .await
            .unwrap();
        assert!(!gate.timed_out.load(std::sync::atomic::Ordering::Relaxed));
        drop(release);
        assert_eq!(
            raw(&s, "/$/algo/g/run", READ, None).await.status(),
            StatusCode::OK
        );
    }
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn body_aware_anonymous_and_error_headers_keep_outer_auth() {
    let s = policy_fixture(
        &[],
        true,
        false,
        "\n[anonymous]\ndatasets = { g = \"read\" }\n",
        |_| {},
    );
    assert_eq!(
        raw(&s, "/$/algo/g/run", READ, None).await.status(),
        StatusCode::OK
    );
    let p = crate::http::algo::probe::Probe::default();
    let response = s
        .app
        .clone()
        .oneshot(probed("/$/algo/g/run", WRITE, None, &p))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().contains_key(header::WWW_AUTHENTICATE));
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert!(response.headers().contains_key("x-content-type-options"));
    let id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_string();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(serde_json::from_slice::<J>(&body).unwrap()["requestId"], id);
    assert_eq!(*p.events.lock(), ["parse", "grant"]);
    assert!(s.state.tasks.lock().is_empty());
    for path in ["/$/algo/other/run", "/$/algo/missing/run"] {
        assert_eq!(
            raw(&s, path, READ, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    // Read-only refusal still does not disclose a hidden dataset to an authenticated reader.
    let s = fixture(&[], true, true);
    for path in ["/$/algo/other/run", "/$/algo/missing/run"] {
        assert_eq!(
            raw(&s, path, WRITE, Some('H')).await.status(),
            StatusCode::NOT_FOUND
        );
    }
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn body_aware_principal_and_client_keys_survive_preparation() {
    let s = fixture(&["query=client-concurrency=1"], true, false);
    let first = raw(&s, "/$/algo/g/run", READ, Some('H')).await;
    let mut same_principal = request("/$/algo/g/run", READ, Some('H'));
    same_principal
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(
            "192.0.2.11:25021".parse::<std::net::SocketAddr>().unwrap(),
        ));
    assert_eq!(
        s.app
            .clone()
            .oneshot(same_principal)
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        raw(&s, "/$/algo/g/run", READ, Some('M')).await.status(),
        StatusCode::OK
    );
    drop(first);
    assert_eq!(
        raw(&s, "/$/algo/g/run", READ, Some('H')).await.status(),
        StatusCode::OK
    );
    let s = fixture(&["query=client-concurrency=1"], false, false);
    let first = raw(&s, "/$/algo/g/run", READ, None).await;
    assert_eq!(
        raw(&s, "/$/algo/g/run", READ, None).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let mut other_client = request("/$/algo/g/run", READ, None);
    other_client
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(
            "192.0.2.11:25021".parse::<std::net::SocketAddr>().unwrap(),
        ));
    assert_eq!(
        s.app.clone().oneshot(other_client).await.unwrap().status(),
        StatusCode::OK
    );
    drop(first);
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn body_aware_session_csrf_and_origin_precede_parsing() {
    use super::super::auth;
    let s = auth::auth_server();
    let login = auth::call(
        &s.app,
        "POST",
        "/$/auth/login",
        &[
            ("content-type", "application/json"),
            ("origin", "http://localhost:3030"),
        ],
        r#"{"user":"bob","password":"bob-pw"}"#,
    )
    .await;
    assert_eq!(login.status, StatusCode::NO_CONTENT, "{}", login.text());
    let cookie = login.cookie("sparkles_session").unwrap();
    let who = auth::call(&s.app, "GET", "/$/whoami", &[("cookie", &cookie)], "")
        .await
        .json();
    let csrf = who["csrfToken"].as_str().unwrap();
    for (origin, token, status) in [
        ("http://localhost:3030", None, StatusCode::FORBIDDEN),
        ("https://foreign.example", Some(csrf), StatusCode::FORBIDDEN),
        ("http://localhost:3030", Some(csrf), StatusCode::OK),
    ] {
        let p = crate::http::algo::probe::Probe::default();
        let mut req = probed("/$/algo/wiki/run", READ, None, &p);
        req.headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
        req.headers_mut()
            .insert(header::ORIGIN, origin.parse().unwrap());
        if let Some(token) = token {
            req.headers_mut()
                .insert("x-sparkles-csrf", token.parse().unwrap());
        }
        let r = send(&s.app, req).await;
        assert_eq!(r.status, status, "{}", r.text());
        assert_eq!(p.events.lock().is_empty(), status != StatusCode::OK);
    }
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn body_aware_query_and_update_views_preserve_graph_and_triple_limits() {
    let mut policy = "\n[[protections]]\nname = \"hidden-edges\"\ndataset = \"g\"\npredicates = [\"http://ex/hidden\"]\n".to_string();
    for (name, token, endpoint, level, graphs) in [
        (
            "view-reader",
            'V',
            "query",
            "read",
            "[\"http://ex/visible\"]",
        ),
        (
            "view-writer",
            'W',
            "update",
            "write",
            "[\"http://ex/visible\",\"http://ex/out\"]",
        ),
    ] {
        policy.push_str(&format!("\n[[tokens]]\nname = \"{name}\"\nhash = \"{}\"\n[[tokens.grants]]\ndataset = \"g\"\nlevel = \"{level}\"\ngraphs = {graphs}\nendpoints = [\"{endpoint}\"]\n", crate::auth::token_hash(&token_text(token))));
    }
    let s = policy_fixture(&[], true, false, &policy, |_| {});
    let ds = s.state.get("g").unwrap();
    ds.store.load(&[Source::from_bytes(b"<http://ex/visible> { <http://ex/allowed-a> <http://ex/p> <http://ex/allowed-b> . <http://ex/protected-a> <http://ex/hidden> <http://ex/protected-b> . } <http://ex/secret> { <http://ex/secret-a> <http://ex/p> <http://ex/secret-b> . }".to_vec(), oxrdfio::RdfFormat::TriG, None)]).unwrap();
    let body = r#"{"algorithm":"pageRank","projection":{"predicates":["http://ex/p","http://ex/hidden"]}}"#;
    let path = "/$/algo/g/run?default-graph-uri=http%3A%2F%2Fex%2Fvisible&default-graph-uri=http%3A%2F%2Fex%2Fsecret";
    let r = send(&s.app, request(path, body, Some('V'))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mut nodes: Vec<String> = r.json()["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["node"]["value"].as_str().unwrap().to_string())
        .collect();
    nodes.sort();
    assert_eq!(nodes, ["http://ex/allowed-a", "http://ex/allowed-b"]);
    let mut j: J = serde_json::from_str(body).unwrap();
    j["write"] =
        serde_json::json!({"graph":"http://ex/out","predicates":{"score":"http://ex/rank"}});
    let r = send(&s.app, request(path, &j.to_string(), Some('W'))).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        super::super::tasks::wait_done(&s.state, &id).await.state,
        task_state::DONE
    );
    let result = ds
        .dataset
        .query("SELECT ?n { GRAPH <http://ex/out> { ?n <http://ex/rank> ?score } } ORDER BY ?n")
        .unwrap();
    assert_eq!(result.len(), 2);
    let output: Vec<String> = result
        .rows()
        .iter()
        .map(|row| row[0].as_ref().unwrap().to_string())
        .collect();
    assert_eq!(output, ["<http://ex/allowed-a>", "<http://ex/allowed-b>"]);
}

async fn blocked_task(
    s: &Fixture,
) -> (
    String,
    Arc<crate::http::algo::probe::Gate>,
    crate::http::algo::probe::Release,
) {
    use crate::http::algo::probe::{Completion, Gate, Release};
    s.state.task_queue.set_max(1);
    let gate = Arc::new(Gate::default());
    let release = Release(gate.clone());
    let in_task = gate.clone();
    let id = s.state.next_task_id();
    s.state
        .start_task_opts(id.clone(), "test-blocker", "", None, true, move |_| {
            let _done = Completion(in_task.clone());
            in_task.wait();
            Ok("released".to_string())
        });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    (id, gate, release)
}

#[tokio::test]
async fn body_aware_update_permit_is_http_body_not_background_task() {
    let s = fixture(&["update=concurrency=1"], false, false);
    let (blocker, gate, release) = blocked_task(&s).await;
    let _cleanup = TaskCleanup(s.state.clone());
    let first = raw(&s, "/$/algo/g/run", WRITE, None).await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let id = s.state.active_task(crate::http::algo::TASK, "g").unwrap();
    assert_eq!(
        s.state
            .tasks
            .lock()
            .iter()
            .find(|t| t.id == id)
            .unwrap()
            .state,
        task_state::QUEUED
    );
    let before = s.state.tasks.lock().len();
    assert_eq!(
        raw(&s, "/$/algo/other/run", WRITE, None).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(s.state.tasks.lock().len(), before);
    // Draining202 releases HTTP admission while the original task remains queued.
    let body = axum::body::to_bytes(first.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(serde_json::from_slice::<J>(&body).unwrap()["id"], id);
    assert_eq!(
        raw(&s, "/$/algo/g/run", WRITE, None).await.status(),
        StatusCode::CONFLICT
    );
    let second = raw(&s, "/$/algo/other/run", WRITE, None).await;
    assert_eq!(second.status(), StatusCode::ACCEPTED);
    let other = s
        .state
        .active_task(crate::http::algo::TASK, "other")
        .unwrap();
    drop(second);
    assert!(s.state.cancel_task(&id).unwrap().is_ok());
    assert!(s.state.cancel_task(&other).unwrap().is_ok());
    gate.release();
    tokio::time::timeout(Duration::from_secs(5), gate.done.notified())
        .await
        .unwrap();
    drop(release);
    assert_eq!(
        super::super::tasks::wait_done(&s.state, &blocker)
            .await
            .state,
        task_state::DONE
    );
    let r = send(&s.app, request("/$/algo/g/run", WRITE, None)).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let id = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        super::super::tasks::wait_done(&s.state, &id).await.state,
        task_state::DONE
    );
}

#[tokio::test]
async fn body_aware_queue_full_refuses_before_new_task_id() {
    let s = fixture(&["update=concurrency=1"], false, false);
    let (blocker, gate, release) = blocked_task(&s).await;
    let _cleanup = TaskCleanup(s.state.clone());
    let mut ids = Vec::new();
    for _ in 0..1000 {
        let id = s.state.next_task_id();
        s.state
            .start_task_opts(id.clone(), "queued-test", "", None, true, |_| {
                Ok("unused".into())
            });
        ids.push(id);
    }
    let p = crate::http::algo::probe::Probe::default();
    let before = s.state.tasks.lock().len();
    let r = send(&s.app, probed("/$/algo/g/run", WRITE, None, &p)).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(*p.events.lock(), ["parse", "grant", "restore", "lookup"]);
    assert_eq!(s.state.tasks.lock().len(), before);
    for id in ids {
        assert!(s.state.cancel_task(&id).unwrap().is_ok());
    }
    gate.release();
    tokio::time::timeout(Duration::from_secs(5), gate.done.notified())
        .await
        .unwrap();
    drop(release);
    assert_eq!(
        super::super::tasks::wait_done(&s.state, &blocker)
            .await
            .state,
        task_state::DONE
    );
}

struct AbortOnDrop<T>(Option<tokio::task::JoinHandle<T>>);
impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

/// Cancel queued work before the blocker's release guard runs during unwinding.
struct TaskCleanup(Arc<AppState>);
impl Drop for TaskCleanup {
    fn drop(&mut self) {
        let ids: Vec<String> = self
            .0
            .tasks
            .lock()
            .iter()
            .filter(|t| t.active())
            .map(|t| t.id.clone())
            .collect();
        for id in ids {
            self.0.cancel_task(&id);
        }
    }
}
