//! View-only runs (spec U10 §3.2): `nodes`, `keep`, `async` and `top` on
//! `POST /$/algo/{ds}/run`.

use super::*;
use crate::http::algo::view;
use serde_json::json;

/// The run route of `ds`, which may carry a query string such as `g?branch=dev`.
fn run_path(ds: &str) -> String {
    match ds.split_once('?') {
        Some((d, q)) => format!("/$/algo/{d}/run?{q}"),
        None => format!("/$/algo/{ds}/run"),
    }
}

async fn run(s: &Server, ds: &str, body: &J) -> (StatusCode, J) {
    let r = send(&s.app, json_req("POST", &run_path(ds), &body.to_string())).await;
    (r.status, r.json())
}

fn names(j: &J) -> Vec<String> {
    j["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["node"]["value"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn nodes_filter_the_rows_and_communities_report_their_whole_size() {
    let s = algo_server();
    let body = json!({
        "algorithm": "wcc",
        "projection": {"predicates": ["http://ex/p"]},
        "nodes": ["http://ex/a", "http://ex/d", "http://ex/nowhere"],
    });
    let (st, j) = run(&s, "g", &body).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(
        j["head"]["vars"],
        json!(["node", "component", "communitySize"])
    );
    assert_eq!(names(&j), ["http://ex/a", "http://ex/d"]);
    let b = j["results"]["bindings"].as_array().unwrap();
    assert_eq!(b[0]["communitySize"]["value"], "3");
    assert_eq!(b[1]["communitySize"]["value"], "2");
    assert_eq!(j["run"]["rows"], 5);
    assert_eq!(j["run"]["cached"], false);
    assert!(j["summary"].is_object(), "{j}");
    // not kept: the same request runs again
    let (_, j) = run(&s, "g", &body).await;
    assert_eq!(j["run"]["cached"], false);
}

#[tokio::test]
async fn a_kept_run_serves_other_nodes_until_the_commit_changes() {
    let s = algo_server();
    let mut body = json!({
        "algorithm": "pageRank",
        "projection": {"predicates": ["http://ex/p"]},
        "params": {"dampingFactor": 0.8},
        "nodes": ["http://ex/a"],
        "keep": true,
    });
    let (st, first) = run(&s, "g", &body).await;
    assert_eq!(st, StatusCode::OK, "{first}");
    assert_eq!(first["run"]["cached"], false);
    let (h0, _, _) = view::counts();
    body["nodes"] = json!(["http://ex/b", "http://ex/c"]);
    let (_, second) = run(&s, "g", &body).await;
    assert_eq!(second["run"]["cached"], true, "{second}");
    assert_eq!(names(&second), ["http://ex/b", "http://ex/c"]);
    assert_eq!(second["run"]["commit"], first["run"]["commit"]);
    assert!(view::counts().0 > h0, "the hit is counted");
    // the scores agree with a run that is not kept
    let (_, plain) = run(
        &s,
        "g",
        &json!({"algorithm": "pageRank", "projection": {"predicates": ["http://ex/p"]},
                "params": {"dampingFactor": 0.8}, "nodes": ["http://ex/b"]}),
    )
    .await;
    assert_eq!(
        plain["results"]["bindings"][0]["score"],
        second["results"]["bindings"][0]["score"]
    );
    // other parameters are another key
    body["params"] = json!({"dampingFactor": 0.85});
    let (_, j) = run(&s, "g", &body).await;
    assert_eq!(j["run"]["cached"], false);
    // a write makes a new commit, which runs again
    s.state
        .get("g")
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            b"<http://ex/e> <http://ex/p> <http://ex/a> .".to_vec(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    body["params"] = json!({"dampingFactor": 0.8});
    let (_, j) = run(&s, "g", &body).await;
    assert_eq!(j["run"]["cached"], false, "{j}");
    assert!(j["run"]["commit"].as_u64() > first["run"]["commit"].as_u64());
    // /metrics shows the cache
    let m = send(
        &s.app,
        Request::get("/$/metrics").body(Body::empty()).unwrap(),
    )
    .await
    .text();
    assert!(m.contains("sparkles_algo_view_cache_hits_total"), "{m}");
    assert!(m.contains("sparkles_algo_view_cache_entries"), "{m}");
}

#[tokio::test]
async fn top_returns_the_highest_scores() {
    let s = algo_server();
    let (st, j) = run(
        &s,
        "g",
        &json!({"algorithm": "pageRank", "projection": {"predicates": ["http://ex/p"]}, "top": 2}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{j}");
    let b = j["results"]["bindings"].as_array().unwrap();
    assert_eq!(b.len(), 2);
    let score = |i: usize| {
        b[i]["score"]["value"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap()
    };
    assert!(score(0) >= score(1));
}

#[tokio::test]
async fn view_members_are_refused_on_a_write_and_when_malformed() {
    let s = algo_server();
    for body in [
        json!({"algorithm": "pageRank", "projection": {"predicates": ["http://ex/p"]},
               "write": {"graph": "http://ex/out", "predicates": {"score": "http://ex/rank"}},
               "keep": true}),
        json!({"algorithm": "pageRank", "async": true}),
        json!({"algorithm": "pageRank", "nodes": ["not an iri"]}),
        json!({"algorithm": "pageRank", "top": 0}),
    ] {
        let (st, j) = run(&s, "g", &body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}: {j}");
    }
}

#[tokio::test]
async fn background_runs_are_joined_cancelled_and_kept() {
    let s = algo_server();
    s.state.create("paused", DbType::Mem).unwrap();
    s.state
        .get("paused")
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            GRAPH.as_bytes().to_vec(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    view::PAUSED.lock().push("paused".into());
    let body = json!({
        "algorithm": "leiden",
        "projection": {"predicates": ["http://ex/p"]},
        "params": {"seed": 42},
        "nodes": ["http://ex/a"],
        "keep": true,
        "async": true,
    });
    let (st, t) = run(&s, "paused", &body).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{t}");
    assert_eq!(t["kind"], view::TASK);
    assert_eq!(t["joined"], false);
    let id = t["id"].as_str().unwrap().to_string();
    // the same key while it runs joins it
    let (st, j) = run(&s, "paused", &body).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    assert_eq!(j["id"], id.as_str());
    assert_eq!(j["joined"], true);
    // cancelled, it keeps nothing
    let r = send(
        &s.app,
        Request::delete(format!("/$/tasks/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    // a request right after the cancel does not join the ending run: it starts another
    let (st, t) = run(&s, "paused", &body).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{t}");
    assert_eq!(t["joined"], false, "{t}");
    let id2 = t["id"].as_str().unwrap().to_string();
    assert_ne!(id2, id);
    let done = super::super::tasks::wait_done(&s.state, &id).await;
    assert_eq!(done.state, task_state::CANCELLED, "{:?}", done.message);
    // the new run is still the one a request joins after the old one ended
    let (_, j) = run(&s, "paused", &body).await;
    assert_eq!(j["id"], id2.as_str(), "{j}");
    // it finishes and is kept
    view::PAUSED.lock().retain(|d| d != "paused");
    let done = super::super::tasks::wait_done(&s.state, &id2).await;
    assert_eq!(done.state, task_state::DONE, "{:?}", done.message);
    let detail = done.detail.unwrap();
    assert_eq!(detail["kept"], true, "{detail}");
    assert!(detail["summary"].is_object(), "{detail}");
    // the result is read from the cache, with or without async
    let (st, j) = run(&s, "paused", &body).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(j["run"]["cached"], true);
    assert_eq!(names(&j), ["http://ex/a"]);
    let mut sync = body.clone();
    sync["async"] = json!(false);
    sync["nodes"] = json!(["http://ex/d"]);
    let (st, j) = run(&s, "paused", &sync).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(j["run"]["cached"], true);
    assert_eq!(j["results"]["bindings"][0]["communitySize"]["value"], "2");
    // the UI reads a finished run at its commit, which is the head here
    let at = format!("paused?at=commit:{}", detail["commit"]);
    let (st, j) = run(&s, &at, &sync).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(j["run"]["cached"], true, "{j}");
}

#[tokio::test]
async fn a_branch_has_its_own_kept_results_and_refuses_write_back() {
    let s = algo_server();
    let r = send(
        &s.app,
        json_req("POST", "/$/branches/g", r#"{"name": "dev"}"#),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = send(
        &s.app,
        Request::post("/g@dev/update")
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(Body::from(
                "INSERT DATA { <http://ex/d> <http://ex/p> <http://ex/a> }",
            ))
            .unwrap(),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let body = json!({
        "algorithm": "wcc",
        "projection": {"predicates": ["http://ex/p"]},
        "nodes": ["http://ex/d"],
        "keep": true,
    });
    let (st, main) = run(&s, "g", &body).await;
    assert_eq!(st, StatusCode::OK, "{main}");
    assert_eq!(
        main["results"]["bindings"][0]["communitySize"]["value"],
        "2"
    );
    let (st, dev) = run(&s, "g?branch=dev", &body).await;
    assert_eq!(st, StatusCode::OK, "{dev}");
    assert_eq!(dev["run"]["cached"], false, "{dev}");
    assert_eq!(dev["results"]["bindings"][0]["communitySize"]["value"], "5");
    let (_, dev) = run(&s, "g?branch=dev", &body).await;
    assert_eq!(dev["run"]["cached"], true);
    let (st, j) = run(
        &s,
        "g?branch=dev",
        &json!({"algorithm": "pageRank", "projection": {"predicates": ["http://ex/p"]},
                "write": {"graph": "http://ex/out", "predicates": {"score": "http://ex/rank"}}}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{j}");
}

#[tokio::test]
async fn a_queued_run_cancelled_before_it_starts_leaves_the_running_list() {
    let s = algo_server();
    s.state.task_queue.set_max(1);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let spin = super::super::tasks::spin(&s.state, "g", true, stop.clone());
    let body = json!({
        "algorithm": "wcc",
        "projection": {"predicates": ["http://ex/p"]},
        "nodes": ["http://ex/a"],
        "keep": true,
        "async": true,
    });
    let (st, t) = run(&s, "g", &body).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{t}");
    assert_eq!(t["state"], task_state::QUEUED, "{t}");
    let id = t["id"].as_str().unwrap().to_string();
    let branch = s.state.get("g").unwrap().store.branch_id();
    assert!(view::running_ids(branch).contains(&id));
    let r = send(
        &s.app,
        Request::delete(format!("/$/tasks/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    assert!(
        !view::running_ids(branch).contains(&id),
        "a cancelled queued run still holds its running entry"
    );
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    super::super::tasks::wait_done(&s.state, &spin).await;
}

/// A chain of `n` nodes whose IRIs are `len` characters long.
fn long_chain(n: usize, len: usize) -> String {
    let iri = |i: usize| format!("<http://ex/{:0>width$}>", i, width = len);
    (0..n)
        .map(|i| format!("{} <http://ex/p> {} .\n", iri(i), iri(i + 1)))
        .collect()
}

#[tokio::test]
async fn the_decoded_rows_count_against_the_memory_budget() {
    let s = algo_server();
    s.state.create("long", DbType::Mem).unwrap();
    s.state
        .get("long")
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            long_chain(3000, 2000).into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let body = json!({
        "algorithm": "wcc",
        "projection": {"predicates": ["http://ex/p"]},
        "nodes": ["http://ex/1"],
    });
    // the engine's own peak is small, the decoded IRIs are about 6 MB
    let (st, j) = run(&s, "long?memory-mb=64", &body).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    let (st, j) = run(&s, "long?memory-mb=2", &body).await;
    assert_eq!(st, StatusCode::INSUFFICIENT_STORAGE, "{j}");
    assert_eq!(j["budget"], "memory", "{j}");
}

#[tokio::test]
async fn the_result_byte_limit_bounds_the_answer() {
    let dir = tempfile::tempdir().unwrap();
    let mut state =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    state.limits.max_result_bytes = Some(800);
    let state = Arc::new(state);
    state.create("g", DbType::Mem).unwrap();
    state
        .get("g")
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            GRAPH.as_bytes().to_vec(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let s = Server {
        _dir: dir,
        app: router(state.clone()),
        state,
    };
    let body = json!({
        "algorithm": "wcc",
        "projection": {"predicates": ["http://ex/p"]},
        "keep": true,
    });
    let (st, j) = run(&s, "g", &body).await;
    assert_eq!(st, StatusCode::INSUFFICIENT_STORAGE, "{j}");
    assert_eq!(j["budget"], "result-bytes", "{j}");
    // the kept result answers the same way, and a small answer from it fits
    let (st, j) = run(&s, "g", &body).await;
    assert_eq!(st, StatusCode::INSUFFICIENT_STORAGE, "{j}");
    let mut one = body.clone();
    one["nodes"] = json!(["http://ex/a"]);
    let (st, j) = run(&s, "g", &one).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(j["run"]["cached"], true, "{j}");
}

#[tokio::test]
async fn a_changed_prefix_is_another_key() {
    let s = algo_server();
    let ds = s.state.get("g").unwrap();
    ds.store.set_prefix("pp", "http://ex/").unwrap();
    let body = json!({
        "algorithm": "wcc",
        "projection": {"predicates": ["pp:p"]},
        "nodes": ["http://ex/a"],
        "keep": true,
    });
    let (st, j) = run(&s, "g", &body).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(j["results"]["bindings"][0]["communitySize"]["value"], "3");
    let (_, j) = run(&s, "g", &body).await;
    assert_eq!(j["run"]["cached"], true);
    // the prefix now names another namespace, without a commit
    ds.store.set_prefix("pp", "http://other/").unwrap();
    let (st, j) = run(&s, "g", &body).await;
    assert_eq!(st, StatusCode::OK, "{j}");
    assert_eq!(j["run"]["cached"], false, "{j}");
    assert_eq!(j["results"]["bindings"], json!([]), "{j}");
}

#[tokio::test]
async fn caller_records_stay_while_their_tasks_are_listed() {
    let s = algo_server();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let first = super::super::tasks::spin(&s.state, "g", true, stop.clone());
    let p = crate::auth::Principal::local();
    view::add_caller(&s.state, &first, "g", &p);
    // many runs that ended and are no longer listed
    for i in 0..5000 {
        view::add_caller(&s.state, &format!("gone-{i}"), "g", &p);
    }
    assert!(view::follows(&s.state, &first, "g", &p));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    super::super::tasks::wait_done(&s.state, &first).await;
}

#[tokio::test]
async fn pruning_caller_records_never_waits_for_the_task_list_while_holding_them() {
    let s = algo_server();
    let p = crate::auth::Principal::local();
    for i in 0..view::MAX_CALLERS {
        view::add_caller(&s.state, &format!("fill-{i}"), "g", &p);
    }
    // a task read holds the task list while it asks whether its caller follows the task
    let tasks = s.state.tasks.lock();
    let st = s.state.clone();
    let pruner = std::thread::spawn(move || {
        let p = crate::auth::Principal::local();
        view::add_caller(&st, "another", "g", &p);
    });
    std::thread::sleep(Duration::from_millis(100));
    let (tx, rx) = std::sync::mpsc::channel();
    let st = s.state.clone();
    std::thread::spawn(move || {
        let p = crate::auth::Principal::local();
        let _ = tx.send(view::follows(&st, "fill-0", "g", &p));
    });
    let answered = rx.recv_timeout(Duration::from_secs(5));
    drop(tasks);
    pruner.join().unwrap();
    assert!(
        answered.is_ok(),
        "a follower check waited on a pruning caller table"
    );
}

#[tokio::test]
async fn a_background_run_hands_its_bytes_to_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
        Some(32 << 20),
    )
    .unwrap();
    let state = Arc::new(state);
    state.create("big", DbType::Mem).unwrap();
    state
        .get("big")
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            long_chain(50_000, 30).into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let s = Server {
        _dir: dir,
        app: router(state.clone()),
        state,
    };
    let body = json!({
        "algorithm": "wcc",
        "projection": {"predicates": ["http://ex/p"]},
        "nodes": ["http://ex/1"],
        "keep": true,
        "async": true,
    });
    let (st, t) = run(&s, "big", &body).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{t}");
    let id = t["id"].as_str().unwrap().to_string();
    let done = super::super::tasks::wait_done(&s.state, &id).await;
    assert_eq!(done.state, task_state::DONE, "{:?}", done.message);
    let detail = done.detail.unwrap();
    let pool = s.state.catalog.pool().unwrap();
    assert_eq!(
        detail["kept"],
        true,
        "{detail}, pool {} of {}",
        pool.granted(),
        pool.capacity()
    );
}

#[tokio::test]
async fn a_run_cancelled_after_it_computed_keeps_nothing() {
    let s = algo_server();
    s.state.create("late", DbType::Mem).unwrap();
    s.state
        .get("late")
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            GRAPH.as_bytes().to_vec(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    view::PAUSED_KEEP.lock().push("late".into());
    let body = json!({
        "algorithm": "wcc",
        "projection": {"predicates": ["http://ex/p"]},
        "nodes": ["http://ex/a"],
        "keep": true,
        "async": true,
    });
    let (st, t) = run(&s, "late", &body).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{t}");
    let id = t["id"].as_str().unwrap().to_string();
    // wait until it has computed and waits to keep the result
    let t0 = std::time::Instant::now();
    loop {
        let p = s
            .state
            .tasks
            .lock()
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| t.progress);
        if p.is_some_and(|p| p >= 0.95) || t0.elapsed() > Duration::from_secs(10) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let r = send(
        &s.app,
        Request::delete(format!("/$/tasks/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    view::PAUSED_KEEP.lock().retain(|d| d != "late");
    let done = super::super::tasks::wait_done(&s.state, &id).await;
    assert_eq!(done.state, task_state::CANCELLED, "{:?}", done.message);
    let mut sync = body.clone();
    sync["async"] = json!(false);
    let (_, j) = run(&s, "late", &sync).await;
    assert_eq!(j["run"]["cached"], false, "{j}");
}

#[tokio::test]
async fn answers_of_one_definition_share_its_hash() {
    let s = algo_server();
    let body = json!({
        "algorithm": "wcc",
        "projection": {"predicates": ["http://ex/p"]},
        "nodes": ["http://ex/a"],
        "keep": true,
    });
    let (_, a) = run(&s, "g", &body).await;
    let mut other = body.clone();
    other["nodes"] = json!(["http://ex/d"]);
    let (_, b) = run(&s, "g", &other).await;
    assert_eq!(a["run"]["definition"], b["run"]["definition"]);
    let mut changed = body.clone();
    changed["projection"] = json!({"predicates": ["http://ex/q"]});
    let (_, c) = run(&s, "g", &changed).await;
    assert_ne!(a["run"]["definition"], c["run"]["definition"]);
}

// the GraphRAG routes are built with the `mcp` feature
#[cfg(feature = "mcp")]
#[tokio::test]
async fn memberships_on_a_branch_say_they_are_not_available() {
    let s = algo_server();
    let r = send(
        &s.app,
        json_req("POST", "/$/branches/g", r#"{"name": "dev"}"#),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = send(
        &s.app,
        json_req(
            "POST",
            "/$/graphrag/g/memberships?branch=dev",
            r#"{"nodes": ["http://ex/a"]}"#,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["available"], false, "{}", r.text());
}
