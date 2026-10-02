//! `GET /{ds}/diff`, `Accept-Datetime` negotiation on the Graph Store, and the Phase 2
//! history settings: `maxBytes`, schedules, pin expiry, history metrics and in-memory
//! history.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

struct Server {
    _dir: tempfile::TempDir,
    app: Router,
    state: Arc<AppState>,
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
    fn headers_all(&self, n: &str) -> Vec<String> {
        self.headers
            .get_all(n)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect()
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

async fn get_with(app: &Router, uri: &str, h: &str, v: &str) -> Resp {
    send(
        app,
        Request::get(uri).header(h, v).body(Body::empty()).unwrap(),
    )
    .await
}

async fn update(app: &Router, ds: &str, u: &str) {
    let r = send(
        app,
        Request::post(format!("/{ds}/update"))
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(Body::from(u.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

async fn put_json(app: &Router, uri: &str, body: &str) -> Resp {
    send(
        app,
        Request::put(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

/// Commit `s` of the test datasets happens at `T0 + 10 s · s`.
const T0: i64 = 1_893_456_000_000; // 2030-01-01T00:00:00Z

/// A persistent dataset `h`: 1 (+a), 2 (+b in g1), 3 (−a), 4 (+c).
async fn server(limits: impl FnOnce(&mut crate::state::Limits)) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    limits(&mut st.limits);
    let state = Arc::new(st);
    state.attach("h", DbType::Persistent, None).unwrap();
    let store = state.datasets.read()["h"].clone();
    let clock = Arc::new(std::sync::atomic::AtomicI64::new(T0));
    let c = clock.clone();
    store.store.set_clock(Arc::new(move || {
        c.load(std::sync::atomic::Ordering::SeqCst)
    }));
    let app = router(state.clone());
    for (i, u) in [
        "INSERT DATA { <urn:a> <urn:p> 1 }",
        "INSERT DATA { GRAPH <urn:g1> { <urn:b> <urn:p> 2 } }",
        "DELETE DATA { <urn:a> <urn:p> 1 }",
        "INSERT DATA { <urn:c> <urn:p> \"three\" }",
    ]
    .iter()
    .enumerate()
    {
        clock.store(
            T0 + 10_000 * (i as i64 + 1),
            std::sync::atomic::Ordering::SeqCst,
        );
        update(&app, "h", u).await;
    }
    Server {
        _dir: dir,
        app,
        state,
    }
}

#[tokio::test]
async fn diffs_as_json_and_lines() {
    let s = server(|_| {}).await;
    let r = get(&s.app, "/h/diff?from=commit:1&to=commit:4").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(
        (j["added"].as_u64(), j["removed"].as_u64()),
        (Some(2), Some(1))
    );
    assert_eq!(j["from"]["commit"]["seq"], 1);
    assert_eq!(j["to"]["selector"], "commit:4");
    assert_eq!(j["method"], "log");
    assert!(j.get("quads").is_none());
    assert_eq!(r.header("sparkles-diff-added").as_deref(), Some("2"));
    // with the quads: removals first
    let j = get(&s.app, "/h/diff?from=1&to=4&quads=true").await.json();
    let q = j["quads"].as_array().unwrap();
    assert_eq!(q.len(), 3);
    assert_eq!(q[0]["op"], "-");
    assert_eq!(q[0]["subject"], "<urn:a>");
    assert_eq!(q[0]["graph"], J::Null);
    // additions by graph, the default graph first
    assert_eq!(q[1]["op"], "+");
    assert_eq!(q[1]["graph"], J::Null);
    assert_eq!(q[2]["graph"], "<urn:g1>");
    // lines
    let r = get_with(
        &s.app,
        "/h/diff?from=1&to=4",
        "accept",
        "text/x-sparkles-diff",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.header("content-type")
            .unwrap()
            .starts_with("text/x-sparkles-diff")
    );
    let lines: Vec<String> = r.text().lines().map(str::to_string).collect();
    assert_eq!(
        lines,
        [
            "- <urn:a> <urn:p> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> .",
            "+ <urn:c> <urn:p> \"three\" .",
            "+ <urn:b> <urn:p> \"2\"^^<http://www.w3.org/2001/XMLSchema#integer> <urn:g1> .",
        ]
    );
    // reversed, and the default `from`: the commit before `to`
    let j = get(&s.app, "/h/diff?from=4&to=1").await.json();
    assert_eq!(
        (j["added"].as_u64(), j["removed"].as_u64()),
        (Some(1), Some(2))
    );
    let j = get(&s.app, "/h/diff?to=commit:3").await.json();
    assert_eq!(j["from"]["commit"]["seq"], 2);
    assert_eq!(j["removed"], 1);
    let j = get(&s.app, "/h/diff").await.json();
    assert_eq!(
        (j["from"]["commit"]["seq"].as_u64(), j["added"].as_u64()),
        (Some(3), Some(1))
    );
    // a graph
    let j = get(&s.app, "/h/diff?from=0&graph=urn:g1").await.json();
    assert_eq!(
        (j["added"].as_u64(), j["removed"].as_u64()),
        (Some(1), Some(0))
    );
    let j = get(&s.app, "/h/diff?from=0&default").await.json();
    assert_eq!(
        (j["added"].as_u64(), j["removed"].as_u64()),
        (Some(1), Some(0))
    );
    // `limit` caps the quads listed, not the counts
    let r = get(&s.app, "/h/diff?from=0&format=diff&limit=1").await;
    assert_eq!(r.text().lines().count(), 1);
    // a time and a snapshot as ends
    let t = sparkles::commit::rfc3339_ms(T0 + 25_000);
    let j = get(&s.app, &format!("/h/diff?from=time:{t}&to=head"))
        .await
        .json();
    assert_eq!(j["from"]["commit"]["seq"], 2);
}

#[tokio::test]
async fn diff_errors_tags_and_budgets() {
    let s = server(|l| l.max_rows = 1).await;
    let r = get(&s.app, "/h/diff?from=abc").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["code"], "invalid-at");
    assert_eq!(
        get(&s.app, "/h/diff?from=99").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&s.app, "/h/diff?from=0&format=xml").await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&s.app, "/h/diff?from=0&graph=not%20an%20iri")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    // over the rows budget
    let r = get(&s.app, "/h/diff?from=0&to=4").await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    assert_eq!(r.json()["budget"], "rows");
    // a diff between two commits never changes: tagged, and revalidated with 304
    let r = get(&s.app, "/h/diff?from=commit:3&to=commit:4").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let tag = r.header("etag").unwrap();
    assert!(tag.starts_with("W/\""), "{tag}");
    let r = get_with(
        &s.app,
        "/h/diff?from=commit:3&to=commit:4",
        "if-none-match",
        &tag,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    // the head moves: no tag
    assert!(get(&s.app, "/h/diff?from=3").await.header("etag").is_none());
    // a diff of a gone commit is 410
    let r = send(
        &s.app,
        Request::post("/$/compact/h").body(Body::empty()).unwrap(),
    )
    .await;
    assert!(r.status.is_success());
    for _ in 0..200 {
        if get(&s.app, "/h/diff?from=commit:1&to=commit:3")
            .await
            .status
            == StatusCode::GONE
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("commit 1 stayed readable after compaction");
}

#[tokio::test]
async fn accept_datetime_negotiates_a_memento() {
    let s = server(|_| {}).await;
    let http_date =
        |ms: i64| httpdate::fmt_http_date(std::time::UNIX_EPOCH + Duration::from_millis(ms as u64));
    // between commits 1 (T0+10 s) and 2 (T0+20 s): commit 1
    let r = get_with(
        &s.app,
        "/h/data?default",
        "accept-datetime",
        &http_date(T0 + 15_000),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("urn:a") && !r.text().contains("urn:c"));
    assert_eq!(r.header("sparkles-commit").as_deref(), Some("1"));
    assert_eq!(r.header("memento-datetime"), Some(http_date(T0 + 10_000)));
    assert_eq!(
        r.header("content-location").as_deref(),
        Some("/h/data?default&at=commit:1")
    );
    assert_eq!(
        r.header("link").as_deref(),
        Some("</h/data?default>; rel=\"original timegate\"")
    );
    assert!(r.headers_all("vary").iter().any(|v| v == "accept-datetime"));
    // the second of a commit is enough
    let r = get_with(
        &s.app,
        "/h/data",
        "accept-datetime",
        &http_date(T0 + 20_000),
    )
    .await;
    assert_eq!(r.header("sparkles-commit").as_deref(), Some("2"));
    // before history: the oldest readable state (RFC 7089 clamps)
    let r = get_with(
        &s.app,
        "/h/data",
        "accept-datetime",
        "Mon, 01 Jan 2001 00:00:00 GMT",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("sparkles-commit").as_deref(), Some("0"));
    // after the head: the head
    let r = get_with(
        &s.app,
        "/h/data",
        "accept-datetime",
        &http_date(T0 + 99_000_000),
    )
    .await;
    assert_eq!(r.header("sparkles-commit").as_deref(), Some("4"));
    // malformed
    let r = get_with(&s.app, "/h/data", "accept-datetime", "yesterday").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["code"], "invalid-accept-datetime");
    // `at` wins over the header; a plain read varies by it
    let r = get_with(&s.app, "/h/data?at=2", "accept-datetime", &http_date(T0)).await;
    assert_eq!(r.header("sparkles-commit").as_deref(), Some("2"));
    let r = get(&s.app, "/h/data").await;
    assert!(r.headers_all("vary").iter().any(|v| v == "accept-datetime"));
    assert!(r.header("memento-datetime").is_none());
}

#[tokio::test]
async fn retention_bytes_schedules_expiry_and_metrics() {
    let s = server(|_| {}).await;
    let r = put_json(
        &s.app,
        "/$/history/h",
        r#"{"keepCommits":10,"maxBytes":"1GiB","schedules":[{"prefix":"daily-","every":"1d","keepLast":7}]}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["retention"]["maxBytes"], 1u64 << 30);
    assert_eq!(j["schedules"][0]["prefix"], "daily-");
    assert_eq!(j["schedules"][0]["every"], "86400s");
    assert_eq!(j["schedules"][0]["keepLast"], 7);
    for bad in [
        r#"{"maxBytes":"lots"}"#,
        r#"{"schedules":[{"prefix":"a/b","every":"1d","keepLast":1}]}"#,
        r#"{"schedules":[{"prefix":"x","every":"1s","keepLast":1}]}"#,
        r#"{"schedules":"daily"}"#,
    ] {
        let r = put_json(&s.app, "/$/history/h", bad).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }
    // a pin that expires
    let r = send(
        &s.app,
        Request::post("/$/snapshots/h")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"name":"tmp","expires":"2030-01-01T00:01:00Z"}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["expires"], "2030-01-01T00:01:00.000Z");
    let r = send(
        &s.app,
        Request::post("/$/snapshots/h?name=x&expires=soon")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // the tick: the schedule pins the head, the expired pin goes (the clock is at
    // T0 + 40 s, before the expiry at T0 + 60 s)
    let ds = s.state.datasets.read()["h"].clone();
    let store = &ds.store;
    let t = store.history_tick().unwrap();
    assert_eq!(t.created.len(), 1);
    assert!(t.expired.is_empty());
    store.set_clock(Arc::new(|| T0 + 60_000));
    let t = store.history_tick().unwrap();
    assert_eq!(t.expired, ["tmp"]);
    // metrics
    let m = get(&s.app, "/$/metrics").await.text();
    for name in [
        "sparkles_history_bytes{dataset=\"h\"}",
        "sparkles_history_snapshots{dataset=\"h\"} 1",
        "sparkles_history_cache_misses_total{dataset=\"h\"}",
        "sparkles_history_materialize_seconds_count{dataset=\"h\"}",
    ] {
        assert!(m.contains(name), "{name} missing");
    }
}

#[tokio::test]
async fn in_memory_datasets_keep_history() {
    let s = server(|_| {}).await;
    s.state.attach("m", DbType::Mem, None).unwrap();
    // without a window, a past commit is gone
    update(&s.app, "m", "INSERT DATA { <urn:x> <urn:p> 1 }").await;
    update(&s.app, "m", "INSERT DATA { <urn:y> <urn:p> 2 }").await;
    let r = get(&s.app, "/m/data?at=1").await;
    assert_eq!(r.status, StatusCode::GONE, "{}", r.text());
    let r = put_json(&s.app, "/$/history/m", r#"{"keepCommits":5}"#).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    update(&s.app, "m", "DELETE DATA { <urn:x> <urn:p> 1 }").await;
    update(&s.app, "m", "INSERT DATA { <urn:z> <urn:p> 3 }").await;
    let r = get(&s.app, "/m/data?at=3").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("urn:y") && !r.text().contains("urn:z"));
    let j = get(&s.app, "/m/diff?from=2&to=4").await.json();
    assert_eq!(
        (j["added"].as_u64(), j["removed"].as_u64()),
        (Some(1), Some(1))
    );
    assert_eq!(j["method"], "compare");
    // pins over HTTP
    let r = send(
        &s.app,
        Request::post("/$/snapshots/m?name=keep&at=commit:3")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let h = get(&s.app, "/$/history/m").await.json();
    assert_eq!(
        h["reconstructable"],
        serde_json::json!([{ "from": 2, "to": 4 }])
    );
}
