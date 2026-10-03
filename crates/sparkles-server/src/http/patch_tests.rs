//! Applying RDF Patch over HTTP (spec F10 Phase 1): the endpoint's methods, media
//! types and dispatch, its answers, `prev`, `TA`, prefix rows, dry runs and write-time
//! validation. Graph-limited callers are in `router_tests/auth/graphs.rs`.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

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
    fn header(&self, n: &str) -> Option<String> {
        self.headers.get(n).map(|v| v.to_str().unwrap().to_string())
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: impl Into<Vec<u8>>,
) -> Resp {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.into())).unwrap())
        .await
        .unwrap();
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

const TEXT: &str = "application/rdf-patch";
const BINARY: &str = "application/rdf-patch+thrift";

struct Server {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

impl Server {
    fn ds(&self) -> Arc<Dataset> {
        self.state.get("ds").unwrap()
    }
    fn head(&self) -> u64 {
        self.ds().store.head_commit().seq
    }
    async fn patch(&self, body: &str) -> Resp {
        call(
            &self.app,
            "POST",
            "/ds/patch",
            &[("content-type", TEXT)],
            body,
        )
        .await
    }
    async fn ask(&self, q: &str) -> bool {
        let r = call(
            &self.app,
            "POST",
            "/ds/sparql",
            &[
                ("content-type", "application/sparql-query"),
                ("accept", "application/sparql-results+json"),
            ],
            q,
        )
        .await;
        r.json()["boolean"].as_bool().unwrap()
    }
}

/// An empty persistent dataset `ds`.
fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("ds", DbType::Persistent, None).unwrap();
    let app = router(state.clone());
    Server {
        _dir: dir,
        state,
        app,
    }
}

#[tokio::test]
async fn p1_p2_a_patch_is_one_commit_of_kind_patch() {
    let s = server();
    let r = s
        .patch("TX .\nA <urn:a> <urn:p> \"1\" .\nA <urn:b> <urn:p> <urn:c> <urn:g> .\nTC .\n")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["inserted"], 2);
    assert_eq!(j["deleted"], 0);
    assert_eq!(j["rows"], 4);
    assert_eq!(j["committed"], true);
    assert_eq!(j["aborted"], false);
    assert_eq!(j["prevChecked"], false);
    assert!(j["timing"]["totalMs"].is_number());
    assert_eq!(r.header("sparkles-commit").as_deref(), Some("1"));
    assert!(r.header("sparkles-dataset-id").is_some());
    let c = call(&s.app, "GET", "/$/commits/ds/1", &[], "").await.json();
    assert_eq!(c["commit"]["kind"], "patch", "{c}");
    // no net change: no commit, and a receipt when asked for
    let r = call(
        &s.app,
        "POST",
        "/ds/patch?receipt=true",
        &[("content-type", TEXT)],
        "A <urn:a> <urn:p> \"1\" .\nD <urn:x> <urn:p> <urn:y> .",
    )
    .await;
    let j = r.json();
    assert_eq!(j["committed"], false, "{j}");
    assert_eq!(j["commit"]["seq"], 1, "{j}");
    assert_eq!(s.head(), 1);
}

#[tokio::test]
async fn p4_prev_mismatch_is_412() {
    let s = server();
    s.patch("A <urn:a> <urn:p> 1 .").await;
    let id = s.ds().store.dataset_id();
    let p = format!("H prev <urn:uuid:{id}#commit:1> .\nA <urn:n> <urn:p> 1 .");
    let r = s.patch(&p).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["prevChecked"], true);
    let r = s.patch(&p).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED, "{}", r.text());
    let j = r.json();
    assert_eq!(j["code"], "prev-mismatch");
    assert_eq!(j["head"], 2);
    assert_eq!(j["prev"], format!("urn:uuid:{id}#commit:1"));
    assert_eq!(
        j["error"],
        "the patch expects commit 1 as the head of ds; the head is 2"
    );
    assert_eq!(s.head(), 2);
    // a dry run reports the precondition with its preview
    let r = call(
        &s.app,
        "POST",
        "/ds/patch?dryRun=true",
        &[("content-type", TEXT)],
        p.as_str(),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED, "{}", r.text());
}

#[tokio::test]
async fn p5_p6_abort_and_prefix_rows() {
    let s = server();
    let r = s.patch("TX . A <urn:q> <urn:p> \"1\" . TA .").await;
    assert_eq!(r.status, StatusCode::OK);
    let j = r.json();
    assert_eq!(
        (j["aborted"].clone(), j["committed"].clone()),
        (json!(true), json!(false))
    );
    assert!(!s.ask("ASK { <urn:q> ?p ?o }").await);
    let r = s.patch("PA \"ex\" <http://example.org/> .").await;
    let j = r.json();
    assert_eq!(j["prefixesSet"], 1, "{j}");
    assert_eq!(j["committed"], false);
    let p = call(&s.app, "GET", "/ds/prefixes?prefix=ex", &[], "").await;
    assert!(p.text().contains("http://example.org/"), "{}", p.text());
    let r = s.patch("PD \"ex\" <urn:g> .").await;
    assert_eq!(r.json()["prefixesRemoved"], 1);
    assert!(s.ds().store.prefixes().is_empty());
}

#[tokio::test]
async fn p9_binary_patches_written_by_jena() {
    let s = server();
    let trp = include_bytes!("../../../sparkles-core/tests/patch/jena-1.trp");
    let r = call(
        &s.app,
        "PATCH",
        "/ds/patch",
        &[("content-type", BINARY)],
        &trp[..],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(
        (j["inserted"].clone(), j["deleted"].clone()),
        (json!(10), json!(1))
    );
    assert!(
        s.ask("ASK { <http://example/s> <http://example/p> 123, \"chat\"@fr }")
            .await
    );
}

#[tokio::test]
async fn p10_p12_methods_media_types_and_dispatch() {
    let s = server();
    for ct in ["text/turtle", "application/rdf-patch; charset=ISO-8859-1"] {
        let r = call(
            &s.app,
            "POST",
            "/ds/patch",
            &[("content-type", ct)],
            "A <urn:a> <urn:p> 1 .",
        )
        .await;
        assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{ct}");
    }
    // no content type, a form type or UTF-8: the text form
    for h in [
        vec![],
        vec![("content-type", "application/x-www-form-urlencoded")],
        vec![("content-type", "application/rdf-patch; charset=utf-8")],
    ] {
        let r = call(&s.app, "POST", "/ds/patch", &h, "A <urn:a> <urn:p> 1 .").await;
        assert_eq!(r.status, StatusCode::OK, "{h:?}: {}", r.text());
    }
    for m in ["GET", "PUT", "DELETE", "HEAD"] {
        let r = call(&s.app, m, "/ds/patch", &[], "").await;
        assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{m}");
    }
    let r = call(&s.app, "OPTIONS", "/ds/patch", &[], "").await;
    // the CORS layer answers OPTIONS on every route, and lists PATCH
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.header("access-control-allow-methods")
            .is_some_and(|m| m.contains("PATCH"))
    );
    // P12: a patch sent to the dataset
    let r = call(
        &s.app,
        "POST",
        "/ds",
        &[("content-type", TEXT)],
        "A <urn:d> <urn:p> 1 .",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(s.ask("ASK { <urn:d> <urn:p> 1 }").await);
    assert_eq!(
        s.ds().store.head_commit().kind,
        sparkles::commit::CommitKind::Patch
    );
}

#[tokio::test]
async fn p11_syntax_and_term_errors_are_400() {
    let s = server();
    let r = s.patch("A <urn:a> <urn:p> .").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let j = r.json();
    assert_eq!(j["code"], "patch-syntax");
    assert_eq!(
        (j["line"].clone(), j["column"].clone()),
        (json!(1), json!(19))
    );
    let r = s
        .patch("A <urn:a> <urn:p> 1 .\nA <urn:b> <urn:p> 2 .\nA \"x\" <urn:p> 3 .")
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["code"], "patch-term");
    assert_eq!(r.json()["row"], 3);
    assert_eq!(s.head(), 0);
}

#[tokio::test]
async fn p14_dry_runs_preview_and_commit_messages() {
    let s = server();
    let r = call(
        &s.app,
        "POST",
        "/ds/patch?dryRun=true",
        &[("content-type", TEXT)],
        "A <urn:a> <urn:p> 1 .",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["commit"]["kind"], "patch", "{}", r.text());
    assert_eq!(s.head(), 0);
    let r = call(
        &s.app,
        "POST",
        "/ds/patch",
        &[
            ("content-type", TEXT),
            ("sparkles-commit-message", "by header"),
        ],
        "H message \"by row\" .\nA <urn:a> <urn:p> 1 .",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let c = call(&s.app, "GET", "/$/commits/ds/1", &[], "").await.json();
    assert_eq!(c["commit"]["message"], "by header", "{c}");
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn p14_write_time_validation_rejects_a_patch() {
    let s = server();
    let shapes = r#"@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ] ."#;
    let cfg = json!({ "mode": "reject", "shapes": { "inline": shapes, "format": "text/turtle" } });
    let r = call(
        &s.app,
        "PUT",
        "/$/validation/ds",
        &[("content-type", "application/json")],
        cfg.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = s
        .patch("A <http://ex.org/x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Person> .")
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert_eq!(s.head(), 0);
    let r = s
        .patch("A <http://ex.org/x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Person> .\nA <http://ex.org/x> <http://ex.org/name> \"X\" .")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.header("sparkles-validation").is_some());
}
