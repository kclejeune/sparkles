//! `?at=` reads, the Memento headers, write rejection, and the snapshot and history
//! endpoints, on a persistent dataset.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

struct Server {
    _dir: tempfile::TempDir,
    app: Router,
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
    fn header(&self, n: &str) -> Option<String> {
        self.headers.get(n).map(|v| v.to_str().unwrap().to_string())
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

async fn post(app: &Router, uri: &str, ct: &str, body: &str) -> Resp {
    send(
        app,
        Request::post(uri)
            .header(header::CONTENT_TYPE, ct)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

const Q: &str = "SELECT ?s WHERE { ?s ?p ?o } ORDER BY ?s";

async fn subjects(app: &Router, at: &str) -> Resp {
    get(app, &format!("/h/sparql?query={}&at={}", enc(Q), enc(at))).await
}

fn rows(r: &Resp) -> Vec<String> {
    r.json()["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["s"]["value"].as_str().unwrap().to_string())
        .collect()
}

/// A persistent dataset `h` with commits 1 (+a), 2 (+b), 3 (−a).
async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("h", DbType::Persistent, None).unwrap();
    let app = router(state);
    for u in [
        "INSERT DATA { <urn:a> <urn:p> 1 }",
        "INSERT DATA { <urn:b> <urn:p> 2 }",
        "DELETE DATA { <urn:a> <urn:p> 1 }",
    ] {
        let r = post(&app, "/h/update", "application/sparql-update", u).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    Server { _dir: dir, app }
}

#[tokio::test]
async fn reads_at_past_commits() {
    let s = server().await;
    let r = subjects(&s.app, "commit:1").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(rows(&r), ["urn:a"]);
    assert_eq!(r.header("sparkles-commit").as_deref(), Some("1"));
    assert_eq!(r.header("sparkles-at").as_deref(), Some("commit:1"));
    assert_eq!(r.header("sparkles-head").as_deref(), Some("3"));
    assert!(r.header("memento-datetime").unwrap().ends_with(" GMT"));
    let link = r.header("link").unwrap();
    assert!(link.starts_with("</h/sparql?query=") && link.ends_with(">; rel=\"original\""));
    assert!(!link.contains("at="), "{link}");
    assert_eq!(rows(&subjects(&s.app, "2").await), ["urn:a", "urn:b"]);
    // the head: no Memento headers
    let r = subjects(&s.app, "head").await;
    assert_eq!(rows(&r), ["urn:b"]);
    assert!(r.header("memento-datetime").is_none());
    assert_eq!(r.header("sparkles-at").as_deref(), Some("head"));
    // Graph Store GET and explain
    let r = get(&s.app, "/h/data?default&at=1").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("urn:a") && !r.text().contains("urn:b"));
    assert_eq!(r.header("sparkles-commit").as_deref(), Some("1"));
    let r = get(&s.app, &format!("/h/explain?query={}&at=1", enc(Q))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("sparkles-commit").as_deref(), Some("1"));
}

#[tokio::test]
async fn selector_and_write_errors() {
    let s = server().await;
    let r = subjects(&s.app, "abc").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["code"], "invalid-at");
    assert_eq!(subjects(&s.app, "99").await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        subjects(&s.app, "snapshot:nope").await.status,
        StatusCode::NOT_FOUND
    );
    // repeated with different values
    let r = post(
        &s.app,
        "/h/sparql?at=1",
        "application/x-www-form-urlencoded",
        &format!("query={}&at=2", enc(Q)),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // writes never take `at`, not even head
    let r = post(
        &s.app,
        "/h/update?at=1",
        "application/sparql-update",
        "INSERT DATA { <urn:z> <urn:p> 9 }",
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["code"], "at-on-write");
    let r = send(
        &s.app,
        Request::put("/h/data?default&at=head")
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from("<urn:z> <urn:p> 9 ."))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // nothing was committed
    assert_eq!(rows(&subjects(&s.app, "head").await), ["urn:b"]);
}

#[tokio::test]
async fn snapshots_history_and_gone_commits() {
    let s = server().await;
    let r = post(
        &s.app,
        "/$/snapshots/h",
        "application/json",
        r#"{"name":"v1","at":"commit:1","note":"first"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.header("location").as_deref(), Some("/$/snapshots/h/v1"));
    assert_eq!(r.json()["seq"], 1);
    // idempotent, then a conflict
    let again = post(
        &s.app,
        "/$/snapshots/h",
        "application/json",
        r#"{"name":"v1","at":"1"}"#,
    )
    .await;
    assert_eq!(again.status, StatusCode::OK);
    let conflict = post(
        &s.app,
        "/$/snapshots/h",
        "application/json",
        r#"{"name":"v1","at":"2"}"#,
    )
    .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    // compaction keeps the pinned generation; unpinned commits of it stay readable too
    let r = send(
        &s.app,
        Request::post("/$/compact/h").body(Body::empty()).unwrap(),
    )
    .await;
    assert!(r.status.is_success());
    let mut done = false;
    for _ in 0..200 {
        let h = get(&s.app, "/$/history/h").await.json();
        if h["generations"].as_array().unwrap().len() == 2 {
            done = true;
            assert_eq!(
                h["generations"][0]["heldBy"],
                serde_json::json!(["snapshot:v1"])
            );
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(done, "compaction did not finish");
    assert_eq!(rows(&subjects(&s.app, "snapshot:v1").await), ["urn:a"]);
    let list = get(&s.app, "/$/snapshots/h").await.json();
    assert_eq!(list["snapshots"][0]["name"], "v1");
    assert_eq!(
        get(&s.app, "/$/snapshots/h/v1").await.json()["note"],
        "first"
    );
    let commits = get(&s.app, "/$/commits/h").await.json();
    assert_eq!(commits["oldestReconstructable"], 0);
    // deleting the pin: commit 1 is gone for good
    let r = send(
        &s.app,
        Request::delete("/$/snapshots/h/v1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = subjects(&s.app, "1").await;
    assert_eq!(r.status, StatusCode::GONE);
    let j = r.json();
    assert_eq!(
        (j["code"].as_str(), j["commit"].as_u64()),
        (Some("history-gone"), Some(1))
    );
    assert_eq!(j["oldestReconstructable"], 3);
    assert_eq!(j["metadata"]["seq"], 1);
    let commits = get(&s.app, "/$/commits/h").await.json();
    let c1 = commits["commits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["seq"] == 1)
        .unwrap()
        .clone();
    assert_eq!(c1["reconstructable"], false);
    // retention over HTTP
    let r = send(
        &s.app,
        Request::put("/$/history/h")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"keepCommits":5,"keepAge":"7d"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["retention"]["keepCommits"], 5);
}
