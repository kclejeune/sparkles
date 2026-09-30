//! The cost of a request: explicit write timeouts capped by `--max-timeout`, writes
//! cancelled when their client disconnects, concurrency permits held until the
//! blocking work they admitted has ended, bounded reports, and background task slots.

use super::tasks::{spin, wait_done};
use super::*;
use crate::ratelimit::{Config, RateLimiter};
use crate::state::task_state;
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

fn task(st: &AppState, id: &str) -> crate::state::Task {
    st.tasks
        .lock()
        .iter()
        .find(|t| t.id == id)
        .cloned()
        .unwrap_or_else(|| panic!("task {id} is not listed"))
}

async fn wait_state(st: &AppState, id: &str, state: &str) {
    let t0 = Instant::now();
    while task(st, id).state != state {
        assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", task(st, id));
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn tasks_beyond_the_cap_wait_queued() {
    let s = limited(&[], |st| st.task_queue.set_max(1));
    let st = &s.state;
    let stop = Arc::new(AtomicBool::new(false));
    // not cancellable once it runs
    let a = spin(st, "ds", false, stop.clone());
    let b = spin(st, "ds", false, stop.clone());
    let c = spin(st, "ds", false, stop.clone());
    assert_eq!(task(st, &a).state, task_state::RUNNING);
    let t = task(st, &b);
    assert_eq!(t.state, task_state::QUEUED);
    assert!(t.cancellable, "a queued task may always be cancelled");
    assert_eq!(st.task_queue.counts(), (1, 2));
    // a queued task that is cancelled never runs
    let r = send(
        &s.app,
        Request::delete(format!("/$/tasks/{b}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    assert_eq!(r.json()["state"], "cancelled");
    assert_eq!(st.task_queue.counts(), (1, 1));
    // the next one starts when the running one ends
    stop.store(true, Ordering::Relaxed);
    assert_eq!(wait_done(st, &a).await.state, task_state::DONE);
    let t = wait_done(st, &c).await;
    assert_eq!(t.state, task_state::DONE);
    assert_eq!(task(st, &b).state, task_state::CANCELLED);
    assert_eq!(st.task_queue.counts(), (0, 0));
}

#[tokio::test]
async fn the_task_list_never_drops_queued_or_running_tasks() {
    let s = limited(&[], |_| {});
    let st = &s.state;
    let stop = Arc::new(AtomicBool::new(false));
    let running = spin(st, "ds", true, stop.clone());
    let mut last = String::new();
    for _ in 0..250 {
        last = st.next_task_id();
        st.start_task_as(last.clone(), "test", "ds", None, |_| Ok("done".into()));
    }
    wait_done(st, &last).await;
    let t0 = Instant::now();
    while st.tasks.lock().iter().filter(|t| t.active()).count() > 1 {
        assert!(t0.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    // one more start trims the finished ones past the limit, never the running one
    let one = st.next_task_id();
    st.start_task_as(one.clone(), "test", "ds", None, |_| Ok("done".into()));
    wait_done(st, &one).await;
    let tasks = st.tasks.lock().clone();
    let finished = tasks.iter().filter(|t| !t.active()).count();
    // (the last one finished after the trim)
    let kept = crate::state::FINISHED_TASKS_KEPT;
    assert!((kept..=kept + 1).contains(&finished), "{finished}");
    assert!(tasks.iter().any(|t| t.id == running && t.active()));
    // still listed, so still cancellable
    let r = send(
        &s.app,
        Request::delete(format!("/$/tasks/{running}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    assert_eq!(wait_done(st, &running).await.state, task_state::CANCELLED);
}

#[tokio::test]
async fn nquads_backups_are_bounded() {
    let s = limited(&[], |st| st.task_queue.set_max(1));
    let st = &s.state;
    let backup = |q: &str| {
        Request::post(format!("/$/backup/ds{q}"))
            .body(Body::empty())
            .unwrap()
    };
    for q in [
        "?level=999",
        "?level=x",
        "?compression=gzip&level=10",
        "?compression=lz4&level=1",
    ] {
        let r = send(&s.app, backup(q)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}: {}", r.text());
    }
    // one backup of a dataset at a time: the first waits for the busy slot
    let stop = Arc::new(AtomicBool::new(false));
    let busy = spin(st, "", false, stop.clone());
    let r = send(&s.app, backup("?level=9")).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let first = r.json()["id"].as_str().unwrap().to_string();
    assert_eq!(r.json()["state"], "queued");
    let r = send(&s.app, backup("")).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    stop.store(true, Ordering::Relaxed);
    wait_done(st, &busy).await;
    let t = wait_done(st, &first).await;
    assert_eq!(t.state, task_state::DONE, "{:?}", t.message);
    let files: Vec<_> = std::fs::read_dir(st.data_dir.join("backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].starts_with("ds_") && files[0].ends_with(".nq.gz"));

    // no backup when the disk keeps less than the reserve free
    let s = limited(&[], |st| {
        st.limits.min_free_disk_bytes = Some(u64::MAX / 2);
    });
    let r = send(&s.app, backup("")).await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    assert!(s.state.tasks.lock().is_empty());
}

#[tokio::test]
async fn a_second_task_of_a_kind_is_refused_while_one_waits() {
    let s = limited(&[], |st| st.task_queue.set_max(1));
    let stop = Arc::new(AtomicBool::new(false));
    let busy = spin(&s.state, "", false, stop.clone());
    let compact = || Request::post("/$/compact/ds").body(Body::empty()).unwrap();
    let r = send(&s.app, compact()).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    assert_eq!(r.json()["state"], "queued");
    let id = r.json()["id"].as_str().unwrap().to_string();
    let r = send(&s.app, compact()).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    stop.store(true, Ordering::Relaxed);
    wait_done(&s.state, &busy).await;
    wait_state(&s.state, &id, task_state::DONE).await;
}

#[tokio::test]
async fn commits_that_would_fill_the_disk_or_memory_get_507() {
    let dir = tempfile::tempdir().unwrap();
    let opts = StoreOptions {
        // more than any disk keeps free
        min_free_disk_bytes: Some(u64::MAX / 2),
        max_memory_bytes: Some(64 << 10),
        ..Default::default()
    };
    let st = Arc::new(AppState::new(dir.path(), opts, Duration::from_secs(30)).unwrap());
    st.create("disk", DbType::Persistent).unwrap();
    st.create("mem", DbType::Mem).unwrap();
    let app = router(st.clone());
    let small = "<http://example.org/a> <http://example.org/p> \"1\" .";
    let mut large = String::new();
    for i in 0..5000 {
        large.push_str(&format!(
            "<http://example.org/s{i}> <http://example.org/p> \"{i}\" .\n"
        ));
    }
    let writes = [
        post(
            "/disk/update",
            UPDATE,
            "INSERT DATA { <http://example.org/a> <http://example.org/p> 1 }",
        ),
        put("/disk/data?default", "application/n-triples", small),
        post("/disk/upload", "application/n-triples", small),
        put("/mem/data?default", "application/n-triples", &large),
    ];
    for req in writes {
        let what = format!("{} {}", req.method(), req.uri());
        let r = send(&app, req).await;
        assert_eq!(
            r.status,
            StatusCode::INSUFFICIENT_STORAGE,
            "{what}: {}",
            r.text()
        );
        assert_eq!(r.json()["code"], "storage-full", "{what}");
    }
    assert_eq!(st.get("disk").unwrap().store.head_commit().seq, 0);
    assert!(st.get("mem").unwrap().store.snapshot().is_empty());
    // an in-memory dataset below its limit takes writes
    let r = send(
        &app,
        put("/mem/data?default", "application/n-triples", small),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
}
