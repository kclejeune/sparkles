//! The cost of a request: explicit write timeouts capped by `--max-timeout`, writes
//! cancelled when their client disconnects, and concurrency permits held until the
//! blocking work they admitted has ended.

use super::*;
use crate::ratelimit::{Config, RateLimiter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

const TTL: &str = "text/turtle";
const UPDATE: &str = "application/sparql-update";

fn post(uri: &str, ct: &str, body: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn put(uri: &str, ct: &str, body: &str) -> Request<Body> {
    Request::put(uri)
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// A server whose requests go through the rate limiter configured by `flags`, with its
/// state adjusted by `tune`.
fn limited(flags: &[&str], tune: impl FnOnce(&mut AppState)) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    tune(&mut st);
    if !flags.is_empty() {
        let mut cfg = Config::default();
        for f in flags {
            cfg.apply_flag(f).unwrap();
        }
        st.rate_limit = Some(Arc::new(RateLimiter::new(&cfg).unwrap()));
    }
    let state = Arc::new(st);
    let ds = state.attach("ds", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let app = router(state.clone());
    Server {
        _dir: dir,
        state,
        app,
    }
}

async fn ask(app: &Router, pattern: &str) -> bool {
    let q = form_urlencoded::byte_serialize(format!("ASK {{ {pattern} }}").as_bytes())
        .collect::<String>();
    let r = send(
        app,
        Request::get(format!("/ds/sparql?query={q}"))
            .header(header::ACCEPT, "application/sparql-results+json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["boolean"].as_bool().unwrap()
}

#[tokio::test]
async fn explicit_write_timeouts_are_capped_by_max_timeout() {
    let s = limited(&[], |st| {
        st.limits.max_timeout = Some(Duration::from_millis(200))
    });
    let ds = s.state.get("ds").unwrap();
    // every write below waits for the writer lock until its (capped) deadline
    let txn = ds.store.write();
    let t0 = Instant::now();
    let writes = [
        post(
            "/ds/update?timeout=100",
            UPDATE,
            "INSERT DATA { <http://example.org/x> <http://example.org/p> 1 }",
        ),
        put(
            "/ds/data?default&timeout=100",
            TTL,
            "<http://example.org/x> <http://example.org/p> 2 .",
        ),
        post(
            "/ds/data?default&timeout=100",
            TTL,
            "<http://example.org/x> <http://example.org/p> 3 .",
        ),
        Request::delete("/ds/data?default&timeout=100")
            .body(Body::empty())
            .unwrap(),
        post(
            "/ds/upload?timeout=100",
            TTL,
            "<http://example.org/x> <http://example.org/p> 4 .",
        ),
    ];
    for req in writes {
        let what = format!("{} {}", req.method(), req.uri());
        let r = send(&s.app, req).await;
        assert_eq!(
            r.status,
            StatusCode::REQUEST_TIMEOUT,
            "{what}: {}",
            r.text()
        );
        assert_eq!(r.json()["timeoutSeconds"], 0.2, "{what}");
    }
    assert!(t0.elapsed() < Duration::from_secs(10));
    drop(txn);
    assert!(!ask(&s.app, "<http://example.org/x> ?p ?o").await);
    // without a timeout parameter a write has no deadline (none by default)
    let r = send(
        &s.app,
        post(
            "/ds/update",
            UPDATE,
            "INSERT DATA { <http://example.org/x> <http://example.org/p> 5 }",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// A request started as its own task; aborting it drops the handler's future, as a
/// client disconnect does.
fn start(app: &Router, req: Request<Body>) -> tokio::task::JoinHandle<()> {
    let app = app.clone();
    tokio::spawn(async move {
        let _ = app.oneshot(req).await;
    })
}

#[tokio::test]
async fn disconnected_writes_are_cancelled_and_commit_nothing() {
    let s = limited(&["update=concurrency=1"], |_| {});
    let ds = s.state.get("ds").unwrap();
    let head = ds.store.head_commit().seq;
    let txn = ds.store.write();
    let a = start(
        &s.app,
        post(
            "/ds/update",
            UPDATE,
            "INSERT DATA { <http://example.org/gone> <http://example.org/p> 1 }",
        ),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    // the update in flight holds the only slot
    let r = send(
        &s.app,
        post(
            "/ds/update",
            UPDATE,
            "INSERT DATA { <http://example.org/b> <http://example.org/p> 1 }",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text());
    assert_eq!(r.json()["reason"], "concurrency");
    // the client goes away: the update stops waiting for the writer lock, and its slot
    // is free once it has
    a.abort();
    let t0 = Instant::now();
    let r = loop {
        let r = send(
            &s.app,
            post(
                "/ds/update?timeout=0.05",
                UPDATE,
                "INSERT DATA { <http://example.org/c> <http://example.org/p> 1 }",
            ),
        )
        .await;
        if r.status != StatusCode::SERVICE_UNAVAILABLE || t0.elapsed() > Duration::from_secs(10) {
            break r;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(r.status, StatusCode::REQUEST_TIMEOUT, "{}", r.text());

    // Graph Store writes and uploads too
    let b = start(
        &s.app,
        put(
            "/ds/data?default",
            TTL,
            "<http://example.org/gone> <http://example.org/p> 2 .",
        ),
    );
    let c = start(
        &s.app,
        post(
            "/ds/upload",
            TTL,
            "<http://example.org/gone> <http://example.org/p> 3 .",
        ),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    b.abort();
    c.abort();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // had any of them kept waiting, it would commit now
    drop(txn);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(ds.store.head_commit().seq, head, "nothing was committed");
    assert!(!ask(&s.app, "<http://example.org/gone> ?p ?o").await);
    assert!(ask(&s.app, "<http://example.org/alice> ?p ?o").await);
}

#[tokio::test]
async fn concurrency_permits_are_held_until_the_blocking_work_ends() {
    use axum::extract::State;
    use axum::response::IntoResponse;
    // a handler whose blocking work ignores cancellation until `open` is set
    async fn slow(State(open): State<Arc<AtomicBool>>) -> super::super::ApiResult {
        super::super::blocking(move || {
            while !open.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(StatusCode::OK.into_response())
        })
        .await
    }
    let mut cfg = Config::default();
    cfg.apply_flag("update=concurrency=1").unwrap();
    let rl = Arc::new(RateLimiter::new(&cfg).unwrap());
    let open = Arc::new(AtomicBool::new(false));
    let app = Router::new()
        .route("/{ds}/update", axum::routing::post(slow))
        .with_state(open.clone())
        .layer(axum::middleware::from_fn_with_state(
            rl,
            crate::ratelimit::limit,
        ));
    let req = || Request::post("/ds/update").body(Body::empty()).unwrap();
    let a = start(&app, req());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        send(&app, req()).await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    // the client is gone, but its work is not: the slot stays taken
    a.abort();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        send(&app, req()).await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    open.store(true, Ordering::Relaxed);
    let t0 = Instant::now();
    loop {
        let r = send(&app, req()).await;
        if r.status == StatusCode::OK {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "{}", r.text());
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn shacl_reports_are_bounded_by_the_result_budget() {
    // 64 KiB: at most 1365 results (48 bytes each at least)
    let s = limited(&[], |st| st.limits.max_result_bytes = Some(64 << 10));
    let data = |n: usize| {
        let mut nt = String::new();
        for i in 0..n {
            nt.push_str(&format!(
                "<http://example.org/n{i}> <http://example.org/v> \"x\" .\n"
            ));
        }
        nt
    };
    // every subject of ex:v is a violation
    let shapes = "@prefix sh: <http://www.w3.org/ns/shacl#> .
        @prefix ex: <http://example.org/> .
        ex:S a sh:NodeShape ; sh:targetSubjectsOf ex:v ;
          sh:property [ sh:path ex:v ; sh:maxCount 0 ] .";
    let validate = |graph: &str| {
        Request::post(format!("/ds/shacl?graph={graph}"))
            .header(header::CONTENT_TYPE, TTL)
            .header(header::ACCEPT, TTL)
            .body(Body::from(shapes))
            .unwrap()
    };
    let load = |graph: &str, n: usize| {
        put(
            &format!("/ds/data?graph={graph}"),
            "application/n-triples",
            &data(n),
        )
    };
    // too many results: stopped while validating
    let r = send(&s.app, load("http://example.org/many", 2000)).await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = send(&s.app, validate("http://example.org/many")).await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    assert_eq!(r.json()["budget"], "result-bytes");
    assert_eq!(r.json()["limit"], 64 << 10);
    // fewer results, but a report larger than the budget
    let r = send(&s.app, load("http://example.org/some", 1000)).await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = send(&s.app, validate("http://example.org/some")).await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    assert_eq!(r.json()["budget"], "result-bytes");
    assert!(r.json()["requested"].as_u64().unwrap() > 64 << 10);
    // a report within the budget
    let r = send(&s.app, load("http://example.org/few", 10)).await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = send(&s.app, validate("http://example.org/few")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("MaxCountConstraintComponent"));
}
