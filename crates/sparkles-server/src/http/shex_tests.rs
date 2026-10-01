//! `POST /{ds}/shex` over HTTP: routing, permissions, rate-limit class, metrics,
//! parameters, and the `validation-work` budget.

use super::super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

struct Resp {
    status: StatusCode,
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
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Resp { status, body }
}

fn server() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let st = AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    let st = Arc::new(st);
    let ds = st.attach("ds", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            b"<http://ex.org/a> <http://ex.org/p> 1 . <http://ex.org/g> { <http://ex.org/a> <http://ex.org/p> 2 }"
                .to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let app = router(st);
    (dir, app)
}

fn post(uri: &str, body: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, "text/shex")
        .body(Body::from(body.to_string()))
        .unwrap()
}

const SCHEMA: &str = "<http://ex.org/S> { <http://ex.org/p> . }";

#[test]
fn route_and_class() {
    let methods = crate::auth::ROUTES
        .iter()
        .find(|(r, _)| *r == "/{ds}/shex")
        .map(|(_, m)| *m);
    assert_eq!(methods, Some(&["POST"][..]));
    let uri: Uri = "/ds/shex".parse().unwrap();
    assert_eq!(
        crate::ratelimit::classify(Some("/{ds}/shex"), &Method::POST, &uri, &HeaderMap::new()),
        Some(crate::ratelimit::Class::Query)
    );
    assert_eq!(Op::Shex.as_str(), "shex");
    assert!(Op::ALL.contains(&Op::Shex));
}

#[test]
fn validation_work_is_a_507_budget() {
    let e = Error::BudgetExceeded(sparkles::Budget {
        kind: BudgetKind::ValidationWork,
        limit: 100_000,
        requested: 100_001,
    });
    let ApiError(status, body) = ApiError::from(e);
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(body["budget"], "validation-work");
    assert_eq!(body["limit"], 100_000);
    assert_eq!(body["requested"], 100_001);
}

#[tokio::test]
async fn dataset_info_lists_the_endpoint() {
    let (_dir, app) = server();
    let r = send(
        &app,
        Request::get("/$/datasets/ds").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["endpoints"]["shex"], "/ds/shex");
}

#[tokio::test]
async fn parameters_are_checked() {
    let (_dir, app) = server();
    for (q, needle) in [
        ("graph=not%20an%20iri", "invalid graph IRI"),
        ("results=some", "invalid results"),
        ("format=ttl", "unknown report format"),
        ("semact-trace=yes", "invalid semact-trace"),
        ("map=x&node=y", "either map or node"),
        ("shape=S", "shape needs node"),
    ] {
        let r = send(&app, post(&format!("/ds/shex?{q}"), SCHEMA)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}: {}", r.text());
        assert!(
            r.json()["error"].as_str().unwrap().contains(needle),
            "{q}: {}",
            r.text()
        );
    }
    let r = send(&app, post("/ds/shex?graph=http://ex.org/none", SCHEMA)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json()["error"], "no such graph: <http://ex.org/none>");
    let r = send(&app, post("/nope/shex?node=x", SCHEMA)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn requests_count_as_shex() {
    let (_dir, app) = server();
    let r = send(
        &app,
        post(
            "/ds/shex?graph=http://ex.org/g&results=nonconformant&format=smap&node=%3Chttp://ex.org/a%3E&shape=%3Chttp://ex.org/S%3E",
            SCHEMA,
        ),
    )
    .await;
    // the validation itself is not wired in yet
    assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED, "{}", r.text());
    let m = send(
        &app,
        Request::get("/$/metrics").body(Body::empty()).unwrap(),
    )
    .await
    .text();
    assert!(
        m.contains(r#"sparkles_requests_total{dataset="ds",operation="shex""#),
        "{m}"
    );
    assert!(m.contains(r#"budget="validation-work""#), "{m}");
}
