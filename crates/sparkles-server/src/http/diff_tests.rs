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
async fn diffs_as_rdf_patch() {
    let s = server(|_| {}).await;
    let id = s.state.datasets.read()["h"].store.dataset_id();
    let r = get_with(
        &s.app,
        "/h/diff?from=1&to=4",
        "accept",
        "application/rdf-patch",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.header("content-type")
            .unwrap()
            .starts_with("application/rdf-patch")
    );
    assert_eq!(
        r.text(),
        format!(
            "H id <urn:uuid:{id}#commit:4> .\nH prev <urn:uuid:{id}#commit:1> .\nTX .\n\
             D <urn:a> <urn:p> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n\
             A <urn:c> <urn:p> \"three\" .\n\
             A <urn:b> <urn:p> \"2\"^^<http://www.w3.org/2001/XMLSchema#integer> <urn:g1> .\n\
             TC .\n"
        )
    );
    // text/rdf-patch names the same format
    let r = get_with(&s.app, "/h/diff?from=1&to=4", "accept", "text/rdf-patch").await;
    assert!(
        r.header("content-type")
            .unwrap()
            .starts_with("text/rdf-patch")
    );
    assert!(r.text().starts_with("H id "));
    // binary: Thrift compact rows, the first a header
    let r = get(
        &s.app,
        "/h/diff?from=commit:1&to=commit:4&format=patch-binary",
    )
    .await;
    assert_eq!(
        r.header("content-type").as_deref(),
        Some("application/rdf-patch+thrift")
    );
    assert_eq!(&r.body[..5], &[0x1C, 0x18, 2, b'i', b'd']);
    let tag = r.header("etag").unwrap();
    assert!(tag.contains(":patch-binary"), "{tag}");
    // a patch lists every change
    assert_eq!(
        get(&s.app, "/h/diff?from=1&format=patch&limit=1")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

/// The `data:` of each `commit` event a stream sends until `n` arrived, and their ids.
async fn read_events(body: &mut axum::body::BodyDataStream, n: usize) -> Vec<(String, String)> {
    use futures_util::StreamExt;
    let mut buf = String::new();
    let mut out = Vec::new();
    while out.len() < n {
        let chunk = tokio::time::timeout(Duration::from_secs(10), body.next())
            .await
            .expect("an event within 10 s")
            .expect("the stream is open")
            .unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buf.find("\n\n") {
            let ev: String = buf.drain(..end + 2).collect();
            // a field's value, without the one space after the colon
            let field = |k: &str| {
                ev.lines()
                    .filter_map(|l| l.strip_prefix(k))
                    .map(|v| v.strip_prefix(' ').unwrap_or(v))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            if field("event:") == "commit" {
                out.push((field("id:"), field("data:")));
            }
        }
    }
    out
}

#[tokio::test]
async fn the_change_feed() {
    let s = server(|_| {}).await;
    let id = s.state.datasets.read()["h"].store.dataset_id();
    // every commit after 0, oldest first
    let r = get(&s.app, "/h/changes?after=0").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.header("sparkles-changes-next").as_deref(), Some("4"));
    assert_eq!(r.header("sparkles-head").as_deref(), Some("4"));
    let j = r.json();
    assert_eq!(
        (j["after"].as_u64(), j["next"].as_u64()),
        (Some(0), Some(4))
    );
    let c = j["commits"].as_array().unwrap();
    assert_eq!(c.len(), 4);
    assert_eq!(c[0]["commit"]["seq"], 1);
    assert_eq!(c[0]["complete"], true);
    assert_eq!(c[0]["changes"][0]["op"], "+");
    assert_eq!(c[0]["changes"][0]["subject"], "<urn:a>");
    assert_eq!(c[2]["changes"][0]["op"], "-");
    assert_eq!(c[1]["changes"][0]["graph"], "<urn:g1>");
    // pages
    let r = get(&s.app, "/h/changes?after=commit:1&limit=2").await;
    assert_eq!(r.header("sparkles-changes-next").as_deref(), Some("3"));
    assert_eq!(r.json()["commits"].as_array().unwrap().len(), 2);
    // nothing after the head (the default)
    let j = get(&s.app, "/h/changes").await.json();
    assert_eq!(j["commits"], json!([]));
    assert_eq!(j["next"], 4);
    // RDF Patch: one patch per commit, chained by prev
    let r = get_with(
        &s.app,
        "/h/changes?after=2",
        "accept",
        "application/rdf-patch",
    )
    .await;
    assert_eq!(
        r.text(),
        format!(
            "H id <urn:uuid:{id}#commit:3> .\nH prev <urn:uuid:{id}#commit:2> .\nTX .\n\
             D <urn:a> <urn:p> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> .\nTC .\n\
             H id <urn:uuid:{id}#commit:4> .\nH prev <urn:uuid:{id}#commit:3> .\nTX .\n\
             A <urn:c> <urn:p> \"three\" .\nTC .\n"
        )
    );
    // errors
    assert_eq!(
        get(&s.app, "/h/changes?after=99").await.status,
        StatusCode::NOT_FOUND
    );
    for q in [
        "limit=0",
        "limit=1001",
        "wait=soon",
        "format=diff",
        "after=x",
    ] {
        assert_eq!(
            get(&s.app, &format!("/h/changes?{q}")).await.status,
            StatusCode::BAD_REQUEST,
            "{q}"
        );
    }
    // long polling: nothing new within the wait
    let t = std::time::Instant::now();
    let j = get(&s.app, "/h/changes?after=4&wait=0.3").await.json();
    assert_eq!(j["commits"], json!([]));
    assert!(t.elapsed() >= Duration::from_millis(250));
    // and a commit that arrives during it
    let app = s.app.clone();
    // (the wait is long so that a loaded machine cannot end it before the commit does)
    let poll = tokio::spawn(async move { get(&app, "/h/changes?after=4&wait=120").await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let t = std::time::Instant::now();
    update(&s.app, "h", "INSERT DATA { <urn:d> <urn:p> 4 }").await;
    let r = poll.await.unwrap();
    assert!(t.elapsed() < Duration::from_secs(60));
    assert_eq!(r.json()["commits"][0]["commit"]["seq"], 5);
    assert_eq!(r.header("sparkles-changes-next").as_deref(), Some("5"));
    // server-sent events: the commits after 3, then new ones as they come
    let res = s
        .app
        .clone()
        .oneshot(
            Request::get("/h/changes?after=3")
                .header("accept", "text/event-stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut body = res.into_body().into_data_stream();
    let ev = read_events(&mut body, 2).await;
    assert_eq!(ev[0].0, "4");
    assert_eq!(ev[1].0, "5");
    let first: J = serde_json::from_str(&ev[0].1).unwrap();
    assert_eq!(first["changes"][0]["object"], "\"three\"");
    update(&s.app, "h", "INSERT DATA { <urn:e> <urn:p> 5 }").await;
    let ev = read_events(&mut body, 1).await;
    assert_eq!(ev[0].0, "6");
    drop(body);
    // a reconnect resumes after Last-Event-ID, here with patches as data
    let res = s
        .app
        .clone()
        .oneshot(
            Request::get("/h/changes?format=patch")
                .header("accept", "text/event-stream")
                .header("last-event-id", "5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = res.into_body().into_data_stream();
    let ev = read_events(&mut body, 1).await;
    assert_eq!(ev[0].0, "6");
    assert!(
        ev[0]
            .1
            .starts_with(&format!("H id <urn:uuid:{id}#commit:6> .\nH prev")),
        "{}",
        ev[0].1
    );
    assert!(ev[0].1.ends_with("TC ."));
}

#[tokio::test]
async fn the_change_feed_keeps_its_budget() {
    let s = server(|l| l.max_rows = 1).await;
    update(
        &s.app,
        "h",
        "INSERT DATA { <urn:x> <urn:p> 1 . <urn:y> <urn:p> 2 }",
    )
    .await;
    // commits 1–4 change one quad each; 5 changes two, more than a page may list
    let j = get(&s.app, "/h/changes?after=0").await.json();
    assert_eq!(j["commits"].as_array().unwrap().len(), 1);
    let j = get(&s.app, "/h/changes?after=4").await.json();
    let c = &j["commits"][0];
    assert_eq!(
        (c["complete"].as_bool(), c["added"].as_u64()),
        (Some(false), Some(2))
    );
    assert!(c.get("changes").is_none());
    // a patch cannot leave changes out
    let r = get(&s.app, "/h/changes?after=4&format=patch").await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(r.json()["code"], "changes-too-large");
    assert_eq!(r.json()["commit"], 5);
    // after a compaction the change log serves the commits, within the same budget
    compact_until_gone(&s).await;
    let r = get(&s.app, "/h/changes?after=1").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["commits"].as_array().unwrap().len(), 1, "{j}");
    assert_eq!(j["commits"][0]["commit"]["seq"], 2);
    // history that neither source holds is 410
    let r = put_json(
        &s.app,
        "/$/history/h",
        r#"{"changeLog": {"enabled": false}}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = get(&s.app, "/h/changes?after=1").await;
    assert_eq!(r.status, StatusCode::GONE, "{}", r.text());
    assert_eq!(r.json()["code"], "history-gone");
}

/// Compact `h` and wait until commit 1 can no longer be read.
async fn compact_until_gone(s: &Server) {
    let r = send(
        &s.app,
        Request::post("/$/compact/h").body(Body::empty()).unwrap(),
    )
    .await;
    assert!(r.status.is_success());
    // the compaction is a background task, slow on a loaded machine
    for _ in 0..6000 {
        if get(&s.app, "/h/sparql?at=1&query=ASK%7B%7D").await.status == StatusCode::GONE {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("commit 1 stayed readable after compaction");
}

#[tokio::test]
async fn the_change_feed_reads_the_change_log_across_a_compaction() {
    let s = server(|_| {}).await;
    let feed = |uri: &'static str, accept: &'static str| {
        let app = s.app.clone();
        async move {
            let r = get_with(&app, uri, "accept", accept).await;
            assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
            (r.text(), r.header("sparkles-changes-next"))
        }
    };
    let asks = [
        ("/h/changes?after=0", "application/json"),
        ("/h/changes?after=commit:1&limit=2", "application/json"),
        ("/h/changes?after=0", "application/rdf-patch"),
    ];
    let mut before = Vec::new();
    for (uri, accept) in asks {
        before.push(feed(uri, accept).await);
    }
    compact_until_gone(&s).await;
    // the same pages, now read from the change log
    for ((uri, accept), b) in asks.into_iter().zip(&before) {
        assert_eq!(&feed(uri, accept).await, b, "{uri}");
    }
    // an event stream resumes from a commit whose generation is gone
    let res = s
        .app
        .clone()
        .oneshot(
            Request::get("/h/changes")
                .header("accept", "text/event-stream")
                .header("last-event-id", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let mut body = res.into_body().into_data_stream();
    let ev = read_events(&mut body, 3).await;
    let ids: Vec<&str> = ev.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(ids, ["2", "3", "4"]);
    let third: J = serde_json::from_str(&ev[1].1).unwrap();
    assert_eq!(third["changes"][0]["op"], "-");
    assert_eq!(third["changes"][0]["subject"], "<urn:a>");
    // new commits follow on the same stream
    update(&s.app, "h", "INSERT DATA { <urn:d> <urn:p> 4 }").await;
    let ev = read_events(&mut body, 1).await;
    assert_eq!(ev[0].0, "5");
}

#[tokio::test]
async fn warm_snapshots_are_materialized() {
    let s = server(|_| {}).await;
    let r = send(
        &s.app,
        Request::post("/$/snapshots/h")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"name": "w", "at": "commit:2", "warm": true}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["warm"], true);
    let h = get(&s.app, "/$/history/h").await.json();
    assert_eq!(h["cache"]["entries"], 1);
    assert_eq!(get(&s.app, "/$/snapshots/h/w").await.json()["warm"], true);
    // a form works too, and a pin is not warm unless asked
    let r = send(
        &s.app,
        Request::post("/$/snapshots/h")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("name=c&at=commit:1"))
            .unwrap(),
    )
    .await;
    assert_eq!(r.json()["warm"], false);
}

#[tokio::test]
async fn the_catalog_horizon_prunes_commits() {
    let s = server(|_| {}).await;
    let r = send(
        &s.app,
        Request::post("/$/compact/h").body(Body::empty()).unwrap(),
    )
    .await;
    assert!(r.status.is_success());
    // the compaction is a background task, slow on a loaded machine
    let mut compacted = false;
    for _ in 0..6000 {
        if get(&s.app, "/$/history/h").await.json()["oldestReconstructable"] == 4 {
            compacted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(compacted, "the compaction did not finish");
    let r = put_json(&s.app, "/$/history/h", r#"{"catalog": {"keepCommits": 2}}"#).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["catalog"]["keepCommits"], 2);
    assert_eq!(j["catalog"]["firstRetained"], 3);
    assert_eq!(get(&s.app, "/$/commits/h/1").await.status, StatusCode::GONE);
    assert_eq!(get(&s.app, "/$/commits/h").await.json()["firstRetained"], 3);
    // a PUT without `catalog` keeps the horizon; null turns it off
    let j = put_json(&s.app, "/$/history/h", "{}").await.json();
    assert_eq!(j["catalog"]["keepCommits"], 2);
    let j = put_json(&s.app, "/$/history/h", r#"{"catalog": null}"#)
        .await
        .json();
    assert_eq!(j["catalog"]["keepCommits"], J::Null);
    for bad in [r#"{"catalog": 3}"#, r#"{"catalog": {"keepAge": "soon"}}"#] {
        assert_eq!(
            put_json(&s.app, "/$/history/h", bad).await.status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
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
    // the quads listed shape the body, so they shape the tag
    let other = get(&s.app, "/h/diff?from=commit:3&to=commit:4&quads=true").await;
    assert_ne!(other.header("etag").unwrap(), tag);
    let r = get_with(
        &s.app,
        "/h/diff?from=commit:3&to=commit:4&quads=true",
        "if-none-match",
        &tag,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    // the head moves: no tag
    assert!(get(&s.app, "/h/diff?from=3").await.header("etag").is_none());
    // a diff of a gone commit is 410
    let r = send(
        &s.app,
        Request::post("/$/compact/h").body(Body::empty()).unwrap(),
    )
    .await;
    assert!(r.status.is_success());
    // the compaction is a background task, slow on a loaded machine
    let mut gone = false;
    for _ in 0..6000 {
        if get(&s.app, "/h/sparql?at=1&query=ASK%7B%7D").await.status == StatusCode::GONE {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(gone, "commit 1 stayed readable after compaction");
    // the change log still has the changes of commit 3, whose parent state is gone
    let r = get(&s.app, "/h/diff?from=commit:2&to=commit:3").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["method"], "log");
    // without it, a diff of a gone commit is 410
    let r = put_json(
        &s.app,
        "/$/history/h",
        r#"{"changeLog": {"enabled": false}}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = get(&s.app, "/h/diff?from=commit:2&to=commit:3").await;
    assert_eq!(r.status, StatusCode::GONE, "{}", r.text());
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

async fn wait_task(st: &AppState, id: &str) -> crate::state::Task {
    let t0 = std::time::Instant::now();
    loop {
        let t = st
            .tasks
            .lock()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .unwrap();
        if t.state != "running" {
            return t;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "task {id} did not finish"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn stats_and_schema_at_a_commit() {
    let s = server(|_| {}).await;
    let r = get(&s.app, "/$/stats/h?at=1").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(
        (j["quads"].as_u64(), j["commit"].as_u64()),
        (Some(1), Some(1))
    );
    assert_eq!(j["at"], "commit:1");
    assert_eq!(r.header("sparkles-commit").as_deref(), Some("1"));
    assert!(r.header("memento-datetime").is_some());
    let j = get(&s.app, "/$/stats/h").await.json();
    assert_eq!(
        (j["quads"].as_u64(), j["commit"].as_u64()),
        (Some(2), Some(4))
    );
    assert_eq!(j["history"]["oldestReconstructable"], 0);
    assert_eq!(
        get(&s.app, "/$/stats/h?at=99").await.status,
        StatusCode::NOT_FOUND
    );
    // schema discovery of a past state
    let preds = |j: &J| j["predicates"]["items"].as_array().map_or(0, Vec::len);
    let r = get(&s.app, "/$/schema/h?at=0").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(preds(&r.json()), 0);
    let r = get(&s.app, "/$/schema/h?at=1").await;
    assert_eq!(preds(&r.json()), 1, "{}", r.text());
    assert_eq!(
        get(&s.app, "/$/schema/h?at=abc").await.status,
        StatusCode::BAD_REQUEST
    );
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn shacl_at_a_commit() {
    let s = server(|_| {}).await;
    let shapes = "@prefix sh: <http://www.w3.org/ns/shacl#> .
        @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
        <urn:S> a sh:NodeShape ; sh:targetSubjectsOf <urn:p> ;
          sh:property [ sh:path <urn:p> ; sh:datatype xsd:integer ] .";
    let validate = |at: &'static str| {
        let app = s.app.clone();
        async move {
            send(
                &app,
                Request::post(format!("/h/shacl?{at}"))
                    .header(header::CONTENT_TYPE, "text/turtle")
                    .header(header::ACCEPT, "application/json")
                    .body(Body::from(shapes))
                    .unwrap(),
            )
            .await
        }
    };
    // the head has "three", not an integer
    let r = validate("").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("\"conforms\":false"), "{}", r.text());
    let r = validate("at=commit:1").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("\"conforms\":true"), "{}", r.text());
    assert_eq!(r.header("sparkles-commit").as_deref(), Some("1"));
    assert_eq!(r.header("sparkles-at").as_deref(), Some("commit:1"));
}

#[tokio::test]
async fn clone_and_backup_at_a_commit() {
    let s = server(|_| {}).await;
    let r = send(
        &s.app,
        Request::post("/$/datasets/h/clone")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"name":"h2","at":"commit:1"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let task = r.json()["id"].as_str().unwrap().to_string();
    let t = wait_task(&s.state, &task).await;
    assert_eq!(t.state, "done", "{t:?}");
    let r = get(&s.app, "/h2/data").await;
    assert!(
        r.text().contains("urn:a") && !r.text().contains("urn:c"),
        "{}",
        r.text()
    );
    let j = get(&s.app, "/$/datasets/h2").await.json();
    assert_eq!(j["forkedFrom"]["seq"], 1, "{j}");
    // a commit that cannot be read is refused before a task starts
    let r = send(
        &s.app,
        Request::post("/$/datasets/h/clone?name=h3&at=99")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    // a backup of a past state
    let r = send(
        &s.app,
        Request::post("/$/backup/h?at=2&compression=none")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let task = r.json()["id"].as_str().unwrap().to_string();
    let t = wait_task(&s.state, &task).await;
    assert_eq!(t.state, "done", "{t:?}");
    let file = std::fs::read_dir(s._dir.path().join("backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().contains("_commit-2"))
        .expect("a backup named after its commit");
    let nq = std::fs::read_to_string(file).unwrap();
    assert_eq!(nq.lines().count(), 2, "{nq}");
    assert!(nq.contains("urn:g1") && !nq.contains("urn:c"));
}

#[tokio::test]
async fn history_queries_over_http() {
    let s = server(|_| {}).await;
    let ops = |j: &J| -> Vec<String> {
        j["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                format!(
                    "{} {} {}",
                    c["commit"],
                    c["op"].as_str().unwrap(),
                    c["subject"].as_str().unwrap()
                )
            })
            .collect()
    };
    let r = get(&s.app, "/h/history").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(
        ops(&j),
        [
            "1 add <urn:a>",
            "2 add <urn:b>",
            "3 remove <urn:a>",
            "4 add <urn:c>"
        ]
    );
    assert_eq!(
        (j["from"].as_u64(), j["to"].as_u64(), j["head"].as_u64()),
        (Some(1), Some(4), Some(4))
    );
    assert_eq!(j["truncated"], false);
    assert_eq!(j["unrecorded"], json!([]));
    assert_eq!(j["changes"][0]["timestamp"], "2030-01-01T00:00:10.000Z");
    assert_eq!(j["changes"][0]["kind"], "update");
    assert_eq!(j["changes"][1]["graph"], "<urn:g1>");
    // filters: a bare IRI, an N-Triples literal, a graph, an operation, a range
    let j = get(&s.app, "/h/history?subject=urn:a").await.json();
    assert_eq!(ops(&j), ["1 add <urn:a>", "3 remove <urn:a>"]);
    let j = get(&s.app, "/h/history?subject=%3Curn:a%3E&op=remove")
        .await
        .json();
    assert_eq!(ops(&j), ["3 remove <urn:a>"]);
    let j = get(&s.app, "/h/history?object=%22three%22").await.json();
    assert_eq!(ops(&j), ["4 add <urn:c>"]);
    let j = get(&s.app, "/h/history?graph=urn:g1").await.json();
    assert_eq!(ops(&j), ["2 add <urn:b>"]);
    let j = get(&s.app, "/h/history?graph=default&from=2&to=commit:3")
        .await
        .json();
    assert_eq!(ops(&j), ["3 remove <urn:a>"]);
    let j = get(&s.app, "/h/history?from=time:2030-01-01T00:00:25Z")
        .await
        .json();
    assert_eq!(ops(&j), ["3 remove <urn:a>", "4 add <urn:c>"]);
    // the last change, newest first
    let j = get(&s.app, "/h/history?order=desc&limit=1").await.json();
    assert_eq!(ops(&j), ["4 add <urn:c>"]);
    assert_eq!(j["truncated"], true);
    // bad parameters
    for q in [
        "op=maybe",
        "order=up",
        "limit=0",
        "subject=%3Curn:a",
        "predicate=%22x%22",
        "from=yesterday",
    ] {
        let r = get(&s.app, &format!("/h/history?{q}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}: {}", r.text());
    }
    // SPARQL over the protocol
    let q = "PREFIX hist: <urn:x-sparkles:history#> SELECT (COUNT(*) AS ?n) WHERE { SERVICE hist:changes { << ?s ?p ?o >> hist:op ?op } }";
    let r = get_with(
        &s.app,
        &format!(
            "/h/sparql?query={}",
            form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>()
        ),
        "accept",
        "text/csv",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.text().lines().nth(1), Some("4"));
    // a past state sees the history up to it
    let r = get_with(
        &s.app,
        &format!(
            "/h/sparql?at=2&query={}",
            form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>()
        ),
        "accept",
        "text/csv",
    )
    .await;
    assert_eq!(r.text().lines().nth(1), Some("2"));
    // the settings and state in /$/history
    let j = get(&s.app, "/$/history/h").await.json();
    assert_eq!(j["changeLog"]["enabled"], true);
    assert_eq!(j["changeLog"]["last"], 4);
    let r = put_json(
        &s.app,
        "/$/history/h",
        r#"{"changeLog": {"keepCommits": 10, "maxBytes": "64MiB"}}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["changeLog"]["settings"]["keepCommits"], 10);
    assert_eq!(j["changeLog"]["maxBytes"], 64 << 20);
    let r = put_json(&s.app, "/$/history/h", r#"{"changeLog": {"keep": 1}}"#).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = put_json(
        &s.app,
        "/$/history/h",
        r#"{"changeLog": {"enabled": false}}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = get(&s.app, "/h/history").await;
    assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED, "{}", r.text());
    assert_eq!(r.json()["code"], "history-unsupported");
}
