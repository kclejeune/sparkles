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
