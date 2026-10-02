//! Entity tags, conditional Graph Store requests, and commit messages over HTTP.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

struct Server {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
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
    /// `Vary` lists `accept` (other `Vary` values, such as `accept-encoding`, may come too).
    fn varies_on_accept(&self) -> bool {
        self.headers
            .get_all("vary")
            .iter()
            .flat_map(|v| v.to_str().unwrap().split(','))
            .any(|v| v.trim().eq_ignore_ascii_case("accept"))
    }
    fn etag(&self) -> String {
        self.header("etag").expect("an ETag")
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

/// `method uri` with headers and a body.
async fn call(app: &Router, method: &str, uri: &str, headers: &[(&str, &str)], body: &str) -> Resp {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    send(app, req.body(Body::from(body.to_string())).unwrap()).await
}

async fn get(app: &Router, uri: &str, headers: &[(&str, &str)]) -> Resp {
    call(app, "GET", uri, headers, "").await
}

async fn update(app: &Router, u: &str) -> Resp {
    let r = call(
        app,
        "POST",
        "/h/update",
        &[("content-type", "application/sparql-update")],
        u,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r
}

/// A persistent dataset `h` with commits 1 and 2.
async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    state.attach("h", DbType::Persistent, None).unwrap();
    let app = router(state.clone());
    update(&app, "INSERT DATA { <urn:a> <urn:p> 1 }").await;
    update(&app, "INSERT DATA { GRAPH <urn:g> { <urn:b> <urn:p> 2 } }").await;
    Server {
        _dir: dir,
        state,
        app,
    }
}

impl Server {
    fn id(&self) -> String {
        self.state.get("h").unwrap().store.dataset_id().to_string()
    }
    fn head(&self) -> u64 {
        self.state.get("h").unwrap().store.head_commit().seq
    }
}

const TTL: &str = "text/turtle";

#[tokio::test]
async fn reads_carry_a_weak_tag_per_commit_and_format() {
    let s = server().await;
    let id = s.id();
    let r = get(&s.app, "/h/data?default", &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.etag(), format!("W/\"{id}:2:ttl\""));
    assert!(r.varies_on_accept());
    let r = get(
        &s.app,
        "/h/data?default",
        &[("accept", "application/n-triples")],
    )
    .await;
    assert_eq!(r.etag(), format!("W/\"{id}:2:nt\""));
    // a format chosen by parameter is not negotiated
    let r = get(&s.app, "/h/data?graph=urn:g&format=nt", &[]).await;
    assert_eq!(r.etag(), format!("W/\"{id}:2:nt\""));
    assert!(!r.varies_on_accept());
    let r = get(&s.app, "/h/data", &[]).await;
    assert_eq!(r.etag(), format!("W/\"{id}:2:trig\""));
    let r = call(&s.app, "HEAD", "/h/data?graph=urn:g", &[], "").await;
    assert_eq!(
        (r.status, r.etag()),
        (StatusCode::OK, format!("W/\"{id}:2:ttl\""))
    );
    // queries get no tag
    let r = get(&s.app, "/h/sparql?query=ASK%7B%7D", &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.header("etag").is_none());
}

#[tokio::test]
async fn if_none_match_answers_304_until_the_next_commit() {
    let s = server().await;
    let r = get(&s.app, "/h/data?default", &[]).await;
    let tag = r.etag();
    for method in ["GET", "HEAD"] {
        let r = call(
            &s.app,
            method,
            "/h/data?default",
            &[("if-none-match", &tag)],
            "",
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_MODIFIED, "{method}");
        assert!(r.body.is_empty());
        assert_eq!(r.etag(), tag);
        assert_eq!(r.header("sparkles-commit").as_deref(), Some("2"));
    }
    // the weak comparison: the tag without W/ matches too; a list matches any member
    let strong = tag.trim_start_matches("W/").to_string();
    let r = get(&s.app, "/h/data?default", &[("if-none-match", &strong)]).await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    let list = format!("\"nope\", {tag}");
    let r = get(&s.app, "/h/data?default", &[("if-none-match", &list)]).await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    let r = get(&s.app, "/h/data?default", &[("if-none-match", "*")]).await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    // another format's tag does not match this representation
    let r = get(
        &s.app,
        "/h/data?default",
        &[("if-none-match", &tag), ("accept", "application/n-triples")],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    // a commit, even to another graph, changes the tag
    update(
        &s.app,
        "INSERT DATA { GRAPH <urn:other> { <urn:x> <urn:p> 3 } }",
    )
    .await;
    let r = get(&s.app, "/h/data?default", &[("if-none-match", &tag)]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("urn:a"));
    assert_ne!(r.etag(), tag);
    // a missing graph is still 404
    let r = get(&s.app, "/h/data?graph=urn:none", &[("if-none-match", "*")]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reads_at_a_past_commit_carry_its_tag() {
    let s = server().await;
    let id = s.id();
    let r = get(&s.app, "/h/data?default&at=1", &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    let tag = r.etag();
    assert_eq!(tag, format!("W/\"{id}:1:ttl\""));
    update(&s.app, "INSERT DATA { <urn:c> <urn:p> 3 }").await;
    // commit 1 never changes
    let r = get(
        &s.app,
        "/h/data?default&at=commit:1",
        &[("if-none-match", &tag)],
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    assert_eq!(r.header("sparkles-commit").as_deref(), Some("1"));
    assert_eq!(r.header("sparkles-at").as_deref(), Some("commit:1"));
    // the head is commit 3
    let r = get(
        &s.app,
        "/h/data?default&at=head",
        &[("if-none-match", &tag)],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.etag(), format!("W/\"{id}:3:ttl\""));
}

#[tokio::test]
async fn if_match_on_reads() {
    let s = server().await;
    let tag = get(&s.app, "/h/data?default", &[]).await.etag();
    let r = get(&s.app, "/h/data?graph=urn:g", &[("if-match", &tag)]).await;
    assert_eq!(r.status, StatusCode::OK);
    update(&s.app, "INSERT DATA { <urn:c> <urn:p> 3 }").await;
    let r = get(&s.app, "/h/data?default", &[("if-match", &tag)]).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(r.json()["code"], "precondition-failed");
    let r = get(&s.app, "/h/data?default&at=2", &[("if-match", &tag)]).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn if_match_guards_graph_store_writes() {
    let s = server().await;
    let tag = get(&s.app, "/h/data?graph=urn:g", &[]).await.etag();
    let put = |tag: String| {
        let app = s.app.clone();
        async move {
            call(
                &app,
                "PUT",
                "/h/data?graph=urn:g",
                &[("content-type", TTL), ("if-match", &tag)],
                "<urn:b> <urn:p> 20 .",
            )
            .await
        }
    };
    let r = put(tag.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(s.head(), 3);
    // the tag is stale now: nothing is written
    let r = put(tag.clone()).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED, "{}", r.text());
    assert_eq!(r.json()["code"], "precondition-failed");
    assert_eq!(s.head(), 3);
    for (method, body) in [("POST", "<urn:z> <urn:p> 1 ."), ("DELETE", "")] {
        let r = call(
            &s.app,
            method,
            "/h/data?graph=urn:g",
            &[("content-type", TTL), ("if-match", &tag)],
            body,
        )
        .await;
        assert_eq!(r.status, StatusCode::PRECONDITION_FAILED, "{method}");
    }
    assert_eq!(s.head(), 3);
    // a tag of the current head, in any format, matches
    let nt = get(&s.app, "/h/data?default&format=nt", &[]).await.etag();
    let r = call(
        &s.app,
        "DELETE",
        "/h/data?graph=urn:g",
        &[("if-match", &nt)],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(s.head(), 4);
    // `*` needs the graph to exist; `If-None-Match: *` needs it absent
    let r = call(
        &s.app,
        "PUT",
        "/h/data?graph=urn:g",
        &[("content-type", TTL), ("if-match", "*")],
        "<urn:b> <urn:p> 2 .",
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    let create = || {
        call(
            &s.app,
            "PUT",
            "/h/data?graph=urn:g",
            &[("content-type", TTL), ("if-none-match", "*")],
            "<urn:b> <urn:p> 2 .",
        )
    };
    assert_eq!(create().await.status, StatusCode::OK);
    assert_eq!(create().await.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(s.head(), 5);
    // a tag of another dataset never matches
    let other = format!("W/\"{}:5:ttl\"", uuid::Uuid::new_v4());
    let r = call(
        &s.app,
        "POST",
        "/h/data?default",
        &[("content-type", TTL), ("if-match", &other)],
        "<urn:z> <urn:p> 1 .",
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
}

#[tokio::test]
async fn concurrent_writers_with_the_same_tag_commit_once() {
    let s = server().await;
    let tag = get(&s.app, "/h/data?default", &[]).await.etag();
    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let app = s.app.clone();
            let tag = tag.clone();
            tokio::spawn(async move {
                call(
                    &app,
                    "PUT",
                    "/h/data?default",
                    &[("content-type", TTL), ("if-match", &tag)],
                    &format!("<urn:w{i}> <urn:p> {i} ."),
                )
                .await
                .status
            })
        })
        .collect();
    let mut statuses = Vec::new();
    for t in tasks {
        statuses.push(t.await.unwrap());
    }
    let ok = statuses.iter().filter(|s| **s == StatusCode::OK).count();
    let failed = statuses
        .iter()
        .filter(|s| **s == StatusCode::PRECONDITION_FAILED)
        .count();
    assert_eq!((ok, failed), (1, 7), "{statuses:?}");
    assert_eq!(s.head(), 3);
}

#[tokio::test]
async fn commit_messages_are_recorded_and_listed() {
    let s = server().await;
    let r = call(
        &s.app,
        "POST",
        "/h/update",
        &[
            ("content-type", "application/sparql-update"),
            ("accept", "application/x-sparkles+json"),
            ("sparkles-commit-message", "add c"),
        ],
        "INSERT DATA { <urn:c> <urn:p> 3 }",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["commit"]["message"], "add c");
    // Graph Store writes and uploads, with an RFC 8187 value
    let r = call(
        &s.app,
        "PUT",
        "/h/data?graph=urn:g&receipt=true",
        &[
            ("content-type", TTL),
            ("sparkles-commit-message", "UTF-8''r%C3%A9%C3%A9crit"),
        ],
        "<urn:b> <urn:p> 22 .",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["commit"]["message"], "réécrit");
    let r = call(
        &s.app,
        "POST",
        "/h/upload",
        &[
            ("content-type", TTL),
            ("sparkles-commit-message", "uploaded"),
        ],
        "<urn:u> <urn:p> 1 .",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = get(&s.app, "/$/commits/h?limit=4", &[]).await;
    let msgs: Vec<J> = r.json()["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["message"].clone())
        .collect();
    assert_eq!(
        msgs,
        [json!("uploaded"), json!("réécrit"), json!("add c"), J::Null]
    );
    let r = get(&s.app, "/$/commits/h/3", &[]).await;
    assert_eq!(r.json()["commit"]["message"], "add c");
    // control characters and non-UTF-8 are refused, and nothing is committed
    for bad in ["UTF-8''a%0Ab", "UTF-8''%FF"] {
        let r = call(
            &s.app,
            "POST",
            "/h/update",
            &[
                ("content-type", "application/sparql-update"),
                ("sparkles-commit-message", bad),
            ],
            "INSERT DATA { <urn:d> <urn:p> 4 }",
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }
    let long = "x".repeat(sparkles::annotations::MAX_MESSAGE_BYTES + 1);
    let r = call(
        &s.app,
        "POST",
        "/h/data?default",
        &[("content-type", TTL), ("sparkles-commit-message", &long)],
        "<urn:d> <urn:p> 4 .",
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(s.head(), 5);
}

#[tokio::test]
async fn cors_allows_the_conditional_headers() {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.cors_origins = vec!["https://app.example".into()];
    let state = Arc::new(st);
    state.attach("h", DbType::Mem, None).unwrap();
    let app = router(state);
    let r = get(
        &app,
        "/h/data?default",
        &[("origin", "https://app.example")],
    )
    .await;
    let exposed = r
        .header("access-control-expose-headers")
        .unwrap()
        .to_ascii_lowercase();
    assert!(exposed.contains("etag"), "{exposed}");
    let r = call(
        &app,
        "OPTIONS",
        "/h/data?default",
        &[
            ("origin", "https://app.example"),
            ("access-control-request-method", "PUT"),
            (
                "access-control-request-headers",
                "if-match,if-none-match,sparkles-commit-message",
            ),
        ],
        "",
    )
    .await;
    let allowed = r
        .header("access-control-allow-headers")
        .unwrap()
        .to_ascii_lowercase();
    for h in ["if-match", "if-none-match", "sparkles-commit-message"] {
        assert!(allowed.contains(h), "{allowed}");
    }
}
