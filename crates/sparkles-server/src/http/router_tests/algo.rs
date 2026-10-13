//! Graph algorithms over HTTP: `POST /$/algo/{ds}/run` (rows, summary, write-back as a
//! task) and the named projections of `/$/algo/{ds}/projections`.

use super::*;
use crate::state::task_state;

const GRAPH: &str = "@prefix ex: <http://ex/> .
ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:a . ex:d ex:p ex:e .";

fn algo_server() -> Server {
    let s = server();
    s.state.create("g", DbType::Persistent).unwrap();
    s.state
        .get("g")
        .unwrap()
        .store
        .load(&[Source::from_bytes(
            GRAPH.as_bytes().to_vec(),
            oxrdfio::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    s
}

fn json_req(method: &str, path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/sparql-results+json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn projections_are_managed_and_used() {
    let s = algo_server();
    let r = send(
        &s.app,
        json_req(
            "PUT",
            "/$/algo/g/projections/p",
            r#"{"predicates": ["http://ex/p"], "direction": "undirected"}"#,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    assert_eq!(j["revision"], 1);
    assert_eq!(j["definition"]["predicates"][0]["iri"], "http://ex/p");
    // a stale revision is refused, a current one replaces
    let mut stale = json_req(
        "PUT",
        "/$/algo/g/projections/p",
        r#"{"direction":"forward"}"#,
    );
    stale
        .headers_mut()
        .insert(header::IF_MATCH, "\"r7\"".parse().unwrap());
    assert_eq!(
        send(&s.app, stale).await.status,
        StatusCode::PRECONDITION_FAILED
    );
    let mut ok = json_req(
        "PUT",
        "/$/algo/g/projections/p",
        r#"{"predicates": ["http://ex/p"], "direction": "undirected"}"#,
    );
    ok.headers_mut()
        .insert(header::IF_MATCH, "\"r1\"".parse().unwrap());
    assert_eq!(send(&s.app, ok).await.status, StatusCode::OK);
    let r = send(
        &s.app,
        json_req("PUT", "/$/algo/g/projections/bad", r#"{"predicates": []}"#),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let l = send(&s.app, json_req("GET", "/$/algo/g/projections", ""))
        .await
        .json();
    assert_eq!(l["projections"].as_array().unwrap().len(), 1, "{l}");
    // a run by name over two components
    let r = send(
        &s.app,
        json_req(
            "POST",
            "/$/algo/g/run",
            r#"{"algorithm": "wcc", "projection": "p"}"#,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = r.json()["results"]["bindings"].as_array().unwrap().len();
    assert_eq!(rows, 5);
    // the summary alone
    let r = send(
        &s.app,
        json_req(
            "POST",
            "/$/algo/g/run?summary=true",
            r#"{"algorithm": "wcc", "projection": "p"}"#,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("nodes"), "{}", r.text());
    // an unknown algorithm
    let r = send(
        &s.app,
        json_req("POST", "/$/algo/g/run", r#"{"algorithm": "nope"}"#),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = send(&s.app, json_req("DELETE", "/$/algo/g/projections/p", "")).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = send(&s.app, json_req("DELETE", "/$/algo/g/projections/p", "")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_run_that_writes_is_a_task() {
    let s = algo_server();
    let r = send(
        &s.app,
        json_req(
            "POST",
            "/$/algo/g/run",
            r#"{"algorithm": "pageRank", "projection": {"predicates": ["http://ex/p"]},
                "write": {"graph": "http://ex/out", "predicates": {"score": "http://ex/rank"},
                          "batchSize": 2}}"#,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let t = r.json();
    assert_eq!(t["kind"], "algo-run", "{t}");
    let done = super::tasks::wait_done(&s.state, t["id"].as_str().unwrap()).await;
    assert_eq!(done.state, task_state::DONE, "{:?}", done.message);
    let q = "/g/sparql?query=SELECT%20%3Fn%20%7B%20GRAPH%20%3Chttp%3A%2F%2Fex%2Fout%3E%20%7B%20%3Fn%20%3Chttp%3A%2F%2Fex%2Frank%3E%20%3Fr%20%7D%20%7D";
    let r = send(
        &s.app,
        Request::get(q)
            .header(header::ACCEPT, "application/sparql-results+json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.json()["results"]["bindings"].as_array().unwrap().len(), 5);
}
