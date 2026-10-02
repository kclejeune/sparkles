//! Write-time SHACL validation over HTTP: configuration, 422 rejections (JSON and
//! Turtle), the `Sparkles-Validation` header, warn mode, and the bypass.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

const SHAPES: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ] ."#;

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
    fn header(&self, n: &str) -> String {
        self.headers
            .get(n)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default()
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
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

fn req(method: &str, uri: &str, ct: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn update(app: &Router, u: &str) -> Resp {
    send(
        app,
        req(
            "POST",
            "/v/update",
            "application/sparql-update",
            &format!("PREFIX ex: <http://ex.org/> {u}"),
        ),
    )
    .await
}

fn server(allow_bypass: bool) -> (tempfile::TempDir, Arc<AppState>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.allow_unvalidated_writes = allow_bypass;
    let st = Arc::new(st);
    st.create("v", DbType::Persistent).unwrap();
    let app = router(st.clone());
    (dir, st, app)
}

async fn enable(app: &Router, mode: &str) -> Resp {
    let cfg = json!({ "mode": mode, "shapes": { "inline": SHAPES } });
    send(
        app,
        req(
            "PUT",
            "/$/validation/v",
            "application/json",
            &cfg.to_string(),
        ),
    )
    .await
}

#[tokio::test]
async fn reject_mode_over_http() {
    let (_d, _st, app) = server(false);
    let r = enable(&app, "reject").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["status"]["baseline"]["conforms"], true);
    assert_eq!(
        r.json()["config"]["shapes"]["file"],
        "validation-shapes.ttl"
    );
    // a conforming write: header, and the receipt carries the summary
    let r = send(
        &app,
        req(
            "POST",
            "/v/update?receipt=true",
            "application/sparql-update",
            "PREFIX ex: <http://ex.org/> INSERT DATA { ex:a a ex:Person ; ex:name \"A\" }",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.header("sparkles-validation")
            .starts_with("status=passed, mode=reject")
    );
    assert_eq!(r.json()["validation"]["status"], "passed");
    // a violation: 422 JSON, nothing committed
    let r = update(&app, "INSERT DATA { ex:b a ex:Person }").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let j = r.json();
    assert_eq!(j["validation"]["blocking"], 1);
    assert_eq!(
        j["validation"]["results"][0]["focusNode"]["value"],
        "http://ex.org/b"
    );
    assert!(j.get("_turtle").is_none() && j.get("requestId").is_some());
    assert!(r.header("sparkles-validation").contains("status=rejected"));
    // the same as Turtle
    let mut rq = req(
        "POST",
        "/v/update",
        "application/sparql-update",
        "PREFIX ex: <http://ex.org/> INSERT DATA { ex:b a ex:Person }",
    );
    rq.headers_mut()
        .insert(header::ACCEPT, "text/turtle".parse().unwrap());
    let r = send(&app, rq).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.header("content-type"), "text/turtle");
    assert!(r.text().contains("ValidationReport"), "{}", r.text());
    // Graph Store PUT that would violate keeps the graph as it was
    let r = send(
        &app,
        req(
            "PUT",
            "/v/data?default",
            "text/turtle",
            "<http://ex.org/z> a <http://ex.org/Person> .",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    let r = send(
        &app,
        Request::get("/v/data?default").body(Body::empty()).unwrap(),
    )
    .await;
    assert!(r.text().contains("ex.org/a") && !r.text().contains("ex.org/z"));
    // status
    let s = send(
        &app,
        Request::get("/$/validation/v").body(Body::empty()).unwrap(),
    )
    .await
    .json();
    assert!(s["status"]["counters"]["rejected"].as_u64().unwrap() >= 2);
    // the last write and the last rejections, for the dataset page
    let st = &s["status"];
    assert_eq!(st["lastCheck"]["status"], "rejected");
    assert_eq!(st["recentRejections"][0]["kind"], "gsp-put");
    assert_eq!(
        st["recentRejections"][1]["first"]["focusNode"]["value"],
        "http://ex.org/b"
    );
    assert_eq!(st["incremental"]["localShapes"], 1);
    assert_eq!(st["baseline"]["bySeverity"]["violation"], 0);
}

#[tokio::test]
async fn enabling_warn_and_bypass() {
    let (_d, st, app) = server(false);
    update(&app, "INSERT DATA { ex:d a ex:Person }").await;
    // the head does not conform: reject is refused, warn is accepted
    let r = enable(&app, "reject").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r = enable(&app, "warn").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = update(&app, "INSERT DATA { ex:e a ex:Person }").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.header("sparkles-validation").contains("status=warned"));
    // bypass needs the server flag
    let r = send(
        &app,
        req(
            "POST",
            "/v/update?validate=false",
            "application/sparql-update",
            "INSERT DATA { <urn:x> <urn:p> 1 }",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // off
    let r = send(
        &app,
        Request::delete("/$/validation/v")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(st.get("v").unwrap().validation.read().is_none());
    let r = update(&app, "INSERT DATA { ex:f a ex:Person }").await;
    assert_eq!(r.header("sparkles-validation"), "");
}

#[tokio::test]
async fn bypass_and_reopen() {
    let (dir, st, app) = server(true);
    enable(&app, "reject").await;
    let r = send(
        &app,
        req(
            "POST",
            "/v/update?validate=false",
            "application/sparql-update",
            "PREFIX ex: <http://ex.org/> INSERT DATA { ex:q a ex:Person }",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.header("sparkles-validation").contains("status=bypassed"));
    // reopened from the registry: the guard is installed from validation.json
    drop(app);
    drop(st);
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let app = router(st.clone());
    let r = update(&app, "INSERT DATA { ex:r a ex:Person }").await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
}

/// A materialization that would break the shapes fails its task with the validation
/// summary, and commits nothing.
#[cfg(feature = "reasoning")]
#[tokio::test]
async fn rejected_inferences_fail_the_reason_task() {
    let (_d, st, app) = server(false);
    let r = update(
        &app,
        "INSERT DATA { ex:worksFor <http://www.w3.org/2000/01/rdf-schema#domain> ex:Person . ex:s ex:worksFor ex:acme }",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // the inferred `ex:s a ex:Person` has no name
    let cfg =
        json!({ "mode": "reject", "includeInferences": true, "shapes": { "inline": SHAPES } });
    let r = send(
        &app,
        req(
            "PUT",
            "/$/validation/v",
            "application/json",
            &cfg.to_string(),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let head = st.get("v").unwrap().store.snapshot().commit;
    let r = send(
        &app,
        req(
            "POST",
            "/$/reason/v",
            "application/json",
            r#"{"profile":"rdfs"}"#,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let t0 = std::time::Instant::now();
    let task = loop {
        let t = st.tasks.lock().last().cloned().unwrap();
        if t.state != "running" {
            break t;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "task did not finish"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(task.state, "failed", "{task:?}");
    // the first result's source shape is the (blank) property shape
    let msg = task.message.unwrap_or_default();
    assert!(
        msg.starts_with("inferences rejected by SHACL validation: 1 blocking result (first: _:")
            && msg.ends_with(" at <http://ex.org/s>)"),
        "{msg}"
    );
    assert_eq!(st.get("v").unwrap().store.snapshot().commit, head);
}
