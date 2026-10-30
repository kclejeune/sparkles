//! Incremental views and live queries over HTTP (spec F13 §2).

use super::*;
use futures_util::StreamExt;

const TEAM: &str = "PREFIX ex: <http://example.org/> SELECT ?who (COUNT(?x) AS ?n) WHERE { ?who a ?x } GROUP BY ?who";

fn req(method: &str, uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn enc(q: &str) -> String {
    form_urlencoded::byte_serialize(q.as_bytes()).collect()
}

#[tokio::test]
async fn views_are_defined_read_and_removed() {
    let s = server();
    let body = serde_json::json!({ "query": TEAM }).to_string();
    let r = send(&s.app, req("PUT", "/$/views/ds/types", &body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    assert!(j["plan"].is_object(), "{j}");
    assert!(j["task"].is_string());
    // a read waits for the first evaluation
    let q = "PREFIX view: <urn:x-sparkles:view#> SELECT ?who ?n WHERE { SERVICE view:read { [] view:name \"types\" ; view:bind ?who, ?n } } ORDER BY ?who";
    let r = send(
        &s.app,
        Request::get(format!("/ds/sparql?query={}", enc(q)))
            .header("accept", "application/sparql-results+json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = r.json()["results"]["bindings"].as_array().unwrap().len();
    assert_eq!(rows, 2, "{}", r.text());
    let r = send(&s.app, req("GET", "/$/views/ds", "")).await;
    let j = r.json();
    assert_eq!(j["views"][0]["name"], "types");
    assert_eq!(j["views"][0]["state"], "current", "{j}");
    let r = send(&s.app, req("GET", "/$/views/ds/types", "")).await;
    assert_eq!(r.json()["columns"], serde_json::json!(["who", "n"]));
    let r = send(&s.app, req("POST", "/$/views/ds/types/rebuild", "")).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let r = send(&s.app, req("DELETE", "/$/views/ds/types", "")).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = send(&s.app, req("GET", "/$/views/ds/types", "")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json()["code"], "view-not-found");
}

#[tokio::test]
async fn views_refuse_what_they_cannot_keep() {
    let s = server();
    for (body, code) in [
        (
            serde_json::json!({ "query": "SELECT (NOW() AS ?t) WHERE {}" }),
            "not-incremental",
        ),
        (
            serde_json::json!({ "query": TEAM, "mode": "graph" }),
            "unsupported-option",
        ),
        (
            serde_json::json!({ "query": TEAM, "keepCommits": 5 }),
            "unsupported-option",
        ),
    ] {
        let r = send(&s.app, req("PUT", "/$/views/ds/v", &body.to_string())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
        assert_eq!(r.json()["code"], code, "{}", r.text());
    }
    let r = send(
        &s.app,
        req(
            "PUT",
            "/$/views/ds/v",
            r#"{"query":"SELECT * {}","colour":1}"#,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let q = "PREFIX view: <urn:x-sparkles:view#> SELECT * WHERE { SERVICE view:read { [] view:name \"nope\" ; view:bind ?x } }";
    let r = send(
        &s.app,
        Request::get(format!("/ds/sparql?query={}", enc(q)))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
}

/// The next event's type and data.
async fn next(body: &mut axum::body::BodyDataStream, buf: &mut String) -> (String, String) {
    loop {
        if let Some(end) = buf.find("\n\n") {
            let ev: String = buf.drain(..end + 2).collect();
            let field = |k: &str| {
                ev.lines()
                    .filter_map(|l| l.strip_prefix(k))
                    .map(|v| v.strip_prefix(' ').unwrap_or(v))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            if !field("event:").is_empty() {
                return (field("event:"), field("data:"));
            }
            continue;
        }
        let chunk = tokio::time::timeout(Duration::from_secs(10), body.next())
            .await
            .expect("an event within 10 s")
            .expect("the stream is open")
            .unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk));
    }
}

#[tokio::test]
async fn live_queries_stream_snapshots_and_diffs() {
    let s = server();
    let q = "PREFIX ex: <http://example.org/> SELECT ?p WHERE { ?p a ex:Person }";
    let res = s
        .app
        .clone()
        .oneshot(
            Request::get(format!("/ds/live?query={}", enc(q)))
                .header("accept", "text/event-stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let mut body = res.into_body().into_data_stream();
    let mut buf = String::new();
    let (ev, data) = next(&mut body, &mut buf).await;
    assert_eq!(ev, "snapshot");
    let j: J = serde_json::from_str(&data).unwrap();
    assert_eq!(j["vars"], serde_json::json!(["p"]));
    assert_eq!(j["rows"].as_array().unwrap().len(), 1, "{j}");
    assert_eq!(j["more"], false);
    let start = j["commit"].as_u64().unwrap();
    s.state
        .get("ds")
        .unwrap()
        .dataset
        .update("PREFIX ex: <http://example.org/> INSERT DATA { ex:erin a ex:Person }")
        .unwrap();
    let (ev, data) = next(&mut body, &mut buf).await;
    assert_eq!(ev, "diff");
    let j: J = serde_json::from_str(&data).unwrap();
    assert_eq!(j["from"], start);
    assert_eq!(
        j["added"][0]["values"][0]["value"],
        "http://example.org/erin"
    );
    assert_eq!(j["removed"].as_array().unwrap().len(), 0);
    drop(body);
    // a resume after the start sends the change since it
    let res = s
        .app
        .clone()
        .oneshot(
            Request::get(format!("/ds/live?query={}", enc(q)))
                .header("last-event-id", start.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = res.into_body().into_data_stream();
    let mut buf = String::new();
    let (ev, data) = next(&mut body, &mut buf).await;
    assert!(ev == "diff" || ev == "reset", "{ev} {data}");
    // a query outside the fragment is refused before the stream starts
    let r = send(
        &s.app,
        Request::get(format!(
            "/ds/live?query={}",
            enc("SELECT (RAND() AS ?r) {}")
        ))
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

/// The SPARQL JSON bindings of `q` at the query endpoint, as sorted values of `?p`.
async fn people(s: &Server, q: &str) -> Vec<String> {
    let r = send(
        &s.app,
        Request::get(format!("/ds/sparql?query={}", enc(q)))
            .header("accept", "application/sparql-results+json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mut v: Vec<String> = r.json()["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["p"]["value"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

/// With RDFS on read, a view and a live query evaluate their query again with it and
/// say so, and turning it off switches them back without ending the stream.
#[tokio::test]
async fn views_and_live_queries_run_with_rdfs_on_read() {
    let s = server();
    let r = send(&s.app, req("PUT", "/$/rdfs/ds", r#"{"graph":"default"}"#)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let q = "PREFIX ex: <http://example.org/> SELECT ?p WHERE { ?p a ex:Person }";
    let body = serde_json::json!({ "query": q }).to_string();
    let r = send(&s.app, req("PUT", "/$/views/ds/people", &body)).await;
    assert!(r.status.is_success(), "{}", r.text());
    let read = "PREFIX view: <urn:x-sparkles:view#> SELECT ?p WHERE { SERVICE view:read { [] view:name \"people\" ; view:bind ?p } }";
    assert_eq!(people(&s, read).await, people(&s, q).await);
    assert_eq!(people(&s, q).await.len(), 2);
    let r = send(&s.app, req("GET", "/$/views/ds/people", "")).await;
    let j = r.json();
    assert_eq!(j["evaluation"], "recompute", "{j}");
    assert_eq!(j["evaluationReason"], "RDFS on read", "{j}");
    let r = send(&s.app, req("GET", "/$/views/ds", "")).await;
    assert_eq!(r.json()["views"][0]["evaluation"], "recompute");

    let res = s
        .app
        .clone()
        .oneshot(
            Request::get(format!("/ds/live?query={}", enc(q)))
                .header("accept", "text/event-stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let mut stream = res.into_body().into_data_stream();
    let mut buf = String::new();
    let (ev, data) = next(&mut stream, &mut buf).await;
    assert_eq!(ev, "snapshot");
    let j: J = serde_json::from_str(&data).unwrap();
    assert_eq!(j["rows"].as_array().unwrap().len(), 2, "{j}");
    s.state
        .get("ds")
        .unwrap()
        .dataset
        .update("PREFIX ex: <http://example.org/> INSERT DATA { ex:erin a ex:Student }")
        .unwrap();
    let (ev, data) = next(&mut stream, &mut buf).await;
    assert_eq!(ev, "diff", "{data}");
    let j: J = serde_json::from_str(&data).unwrap();
    assert_eq!(
        j["added"][0]["values"][0]["value"],
        "http://example.org/erin"
    );
    assert_eq!(people(&s, read).await, people(&s, q).await);

    // RDFS on read off: the students leave the stream and the view
    let r = send(&s.app, req("DELETE", "/$/rdfs/ds", "")).await;
    assert!(r.status.is_success(), "{}", r.text());
    let (ev, data) = next(&mut stream, &mut buf).await;
    assert_eq!(ev, "diff", "{data}");
    let j: J = serde_json::from_str(&data).unwrap();
    assert_eq!(j["removed"].as_array().unwrap().len(), 2, "{j}");
    assert_eq!(people(&s, read).await, ["http://example.org/bob"]);
    let r = send(&s.app, req("GET", "/$/views/ds/people", "")).await;
    let j = r.json();
    assert_eq!(j["evaluation"], "incremental", "{j}");
    assert!(j.get("evaluationReason").is_none(), "{j}");
}

/// A read-only server refuses to rebuild a view, which would resume a restored or
/// cloned view and rewrite its definition (review of item 220).
#[tokio::test]
async fn a_read_only_server_refuses_rebuilds() {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    drop(st.create("ds", DbType::Mem).unwrap());
    st.read_only = true;
    let app = router(Arc::new(st));
    let r = send(&app, req("POST", "/$/views/ds/v/rebuild", "")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
}
