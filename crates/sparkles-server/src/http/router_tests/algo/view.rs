//! View-only runs (spec U10 §3.2): `nodes`, `keep`, `async` and `top` on
//! `POST /$/algo/{ds}/run`.

use super::*;
use crate::http::algo::view;
use serde_json::json;

async fn run(s: &Server, ds: &str, body: &J) -> (StatusCode, J) {
    let r = send(
        &s.app,
        json_req("POST", &format!("/$/algo/{ds}/run"), &body.to_string()),
    )
    .await;
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
    let done = super::super::tasks::wait_done(&s.state, &id).await;
    assert_eq!(done.state, task_state::CANCELLED, "{:?}", done.message);
    // a new run starts, finishes and is kept
    view::PAUSED.lock().retain(|d| d != "paused");
    let (st, t) = run(&s, "paused", &body).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{t}");
    assert_eq!(t["joined"], false);
    let id2 = t["id"].as_str().unwrap().to_string();
    assert_ne!(id2, id);
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
}
