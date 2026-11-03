//! `POST /{ds}/ask`, `/$/assistant/{ds}`, `/$/asks/{ds}` and `GET /$/models/usage`
//! against mock providers (C18 A12, A14, A15, A19, A22, A28, A34, A40 to A42).

use super::tests::{ORG, draft, is_summary};
use crate::models::mock::{self, MockModel};
use crate::models::{Models, ModelsConfig};
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use sparkles::io::{RdfFormat, Source};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const EASY: &str = "SELECT ?t WHERE { ?t a ex:Team }";

/// A mock answering drafts from `drafts` (repeating the last) and summaries citing row 1.
fn scripted(drafts: Vec<String>) -> MockModel {
    let drafts = Mutex::new(drafts.into_iter().collect::<VecDeque<_>>());
    MockModel::start(move |r, _| {
        if is_summary(r) {
            return (
                200,
                mock::openai(
                    &json!({ "text": "Two teams [1] [2]. Row 90 is wrong [90].", "citations": [1, 2, 90] })
                        .to_string(),
                ),
            );
        }
        let mut d = drafts.lock();
        let text = if d.len() > 1 {
            d.pop_front().unwrap()
        } else {
            d.front().cloned().unwrap_or_default()
        };
        (200, mock::openai(&text))
    })
}

/// A server with the organisation graph as `org` and providers `cheap`, `mid` and `top`.
fn app(urls: [&str; 3], roles: Value) -> (Arc<AppState>, Router) {
    let mut providers = Map::new();
    for (name, url) in ["cheap", "mid", "top"].into_iter().zip(urls) {
        providers.insert(
            name.into(),
            json!({ "kind": "openai", "endpoint": url, "requestTimeoutSecs": 5,
                    "allowedModels": ["m", "m2"] }),
        );
    }
    let cfg = json!({ "providers": providers, "roles": roles });
    let models = Models::new(
        ModelsConfig::parse(&cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    );
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    st.models = arc_swap::ArcSwapOption::from_pointee(models);
    st.limits.max_result_bytes = None;
    let st = Arc::new(st);
    let ds = st.attach("org", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            ORG.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let router = crate::http::router(st.clone());
    (st, router)
}

fn pair(p: &str) -> Value {
    json!({ "provider": p, "model": "m" })
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let res = app.clone().oneshot(req).await.unwrap();
    let s = res.status();
    let b = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (s, String::from_utf8_lossy(&b).into_owned())
}

fn req(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn put_settings(app: &Router, s: Value) -> (StatusCode, Value) {
    let (st, b) = send(app, req("PUT", "/$/assistant/org", s)).await;
    (st, serde_json::from_str(&b).unwrap_or(Value::Null))
}

/// The events of a server-sent event stream, in order.
fn events(text: &str) -> Vec<(String, Value)> {
    text.split("\n\n")
        .filter_map(|block| {
            let mut name = None;
            let mut data = String::new();
            for line in block.lines() {
                if let Some(e) = line.strip_prefix("event:") {
                    name = Some(e.trim().to_string());
                } else if let Some(d) = line.strip_prefix("data:") {
                    data.push_str(d.trim_start());
                }
            }
            Some((name?, serde_json::from_str(&data).unwrap_or(Value::Null)))
        })
        .collect()
}

async fn ask_sse(app: &Router, body: Value) -> Vec<(String, Value)> {
    let r = Request::post("/org/ask")
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (s, text) = send(app, r).await;
    assert_eq!(s, StatusCode::OK, "{text}");
    events(&text)
}

async fn ask_json(app: &Router, body: Value) -> (StatusCode, Value) {
    let r = Request::post("/org/ask")
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (s, text) = send(app, r).await;
    (s, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn names(ev: &[(String, Value)]) -> Vec<&str> {
    ev.iter().map(|(n, _)| n.as_str()).collect()
}

fn find<'a>(ev: &'a [(String, Value)], name: &str) -> &'a Value {
    &ev.iter().find(|(n, _)| n == name).unwrap().1
}

/// A12: with `send: "schema"` the stream has no summary, and no request carries rows.
/// A22 on the server: `run: false` stops after the check.
#[tokio::test(flavor = "multi_thread")]
async fn a12_schema_only_streams_without_a_summary() {
    let m = scripted(vec![draft(EASY)]);
    let (_, app) = app(
        [&m.url(), &m.url(), &m.url()],
        json!({ "draft": [pair("cheap")], "summarize": [pair("cheap")] }),
    );
    // without an assistant there is no asking
    let (s, b) = ask_json(&app, json!({ "question": "Which teams are there?" })).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(b["code"], "no-assistant");
    let (s, v) = put_settings(&app, json!({ "enabled": true, "send": "schema" })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"]["ask"], true);
    assert_eq!(v["status"]["summary"], false);
    let ev = ask_sse(&app, json!({ "question": "Which teams does Acme have?" })).await;
    assert_eq!(
        names(&ev),
        ["ground", "draft", "check", "run", "result", "usage"],
        "{ev:?}"
    );
    let r = find(&ev, "result");
    assert_eq!(r["results"]["queryType"], "SELECT");
    assert_eq!(r["results"]["rows"].as_array().unwrap().len(), 2);
    assert!(r["results"]["meta"]["plan"].is_object());
    assert!(find(&ev, "usage")["askId"].is_string());
    let reqs = m.requests();
    assert_eq!(reqs.len(), 1);
    assert!(reqs.iter().all(|r| !is_summary(r)));
    assert!(!reqs[0].body.to_string().contains("[1] ?t"));
    // preview: stops after the check
    let ev = ask_sse(
        &app,
        json!({ "question": "Which teams are there?", "run": false }),
    )
    .await;
    assert_eq!(names(&ev), ["ground", "draft", "check", "result", "usage"]);
    assert!(find(&ev, "result")["results"].is_null());
    assert_eq!(find(&ev, "usage")["outcome"], "checked");

    // a draft without a query ends the stream with an `unanswerable` error
    let m = scripted(vec![draft("")]);
    let (_, other) = self::app(
        [&m.url(), &m.url(), &m.url()],
        json!({ "draft": [pair("cheap")] }),
    );
    put_settings(&other, json!({ "enabled": true })).await;
    let ev = ask_sse(
        &other,
        json!({ "question": "What is the meaning of life?" }),
    )
    .await;
    assert_eq!(names(&ev), ["ground", "draft", "error", "usage"], "{ev:?}");
    assert_eq!(find(&ev, "error")["code"], "unanswerable");
    assert_eq!(find(&ev, "usage")["outcome"], "unanswerable");
}

/// A42 and the summary's citations (A24): a provider that may not receive rows is
/// skipped for the summary; a summary marker for a row not sent is dropped.
#[tokio::test(flavor = "multi_thread")]
async fn a42_send_by_provider() {
    let cheap = scripted(vec![draft(EASY)]);
    let top = scripted(vec![draft(EASY)]);
    let (_, app) = app(
        [&cheap.url(), &cheap.url(), &top.url()],
        json!({ "draft": [pair("cheap")], "summarize": [pair("top")] }),
    );
    let (s, v) = put_settings(
        &app,
        json!({ "enabled": true, "send": "rows", "sendByProvider": { "top": "schema" } }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ev = ask_sse(&app, json!({ "question": "Which teams are there?" })).await;
    assert!(!names(&ev).contains(&"summary"), "{ev:?}");
    assert!(top.requests().is_empty(), "top holds no row");
    let (s, _) = put_settings(
        &app,
        json!({ "enabled": true, "send": "rows", "sendByProvider": { "top": "schema" },
                "roles": { "summarize": [pair("cheap"), pair("top")] } }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let ev = ask_sse(&app, json!({ "question": "Which teams are there?" })).await;
    let sm = find(&ev, "summary");
    assert_eq!(sm["provider"], "cheap");
    assert_eq!(sm["text"], "Two teams [1] [2]. Row 90 is wrong.");
    assert_eq!(sm["citations"], json!([1, 2]));
    assert!(top.requests().is_empty());
}

/// A14 over HTTP: an ambiguous mention ends the stream with `clarify`; the answer
/// resumes the pipeline with the chosen IRI.
#[tokio::test(flavor = "multi_thread")]
async fn a14_clarification() {
    let m = scripted(vec![draft(
        "SELECT ?t WHERE { <http://example.org/resource/ana2> ex:memberOf ?t }",
    )]);
    let (st, app) = app(
        [&m.url(), &m.url(), &m.url()],
        json!({ "draft": [pair("cheap")] }),
    );
    let ds = st.get("org").unwrap();
    ds.store
        .load(&[Source::from_bytes(
            b"@prefix ex: <http://example.org/ontology#> . @prefix res: <http://example.org/resource/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\nres:ana2 a ex:Person ; rdfs:label \"Ana\"@en ; ex:memberOf res:platform .\nres:ana rdfs:label \"Ana\"@en .".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    put_settings(&app, json!({ "enabled": true })).await;
    let ev = ask_sse(&app, json!({ "question": "Which team is Ana on?" })).await;
    assert_eq!(names(&ev), ["ground", "clarify", "usage"]);
    let c = find(&ev, "clarify");
    assert_eq!(c["choices"].as_array().unwrap().len(), 2, "{c}");
    assert!(m.requests().is_empty());
    let ev = ask_sse(
        &app,
        json!({ "question": "Which team is Ana on?", "clarification": { "id": c["id"], "value": "res:ana2" } }),
    )
    .await;
    let r = find(&ev, "result");
    assert!(r["query"].as_str().unwrap().contains("ana2"), "{r}");
    assert!(mock::prompt_of(&m.requests()[0]).contains("res:ana2"));
}

/// A15: with the day's budget used, the ask is refused before any provider call.
#[tokio::test(flavor = "multi_thread")]
async fn a15_daily_budget() {
    let m = scripted(vec![draft(EASY)]);
    let (st, app) = app(
        [&m.url(), &m.url(), &m.url()],
        json!({ "draft": [pair("cheap")] }),
    );
    put_settings(
        &app,
        json!({ "enabled": true, "budget": { "perDatasetPerDay": 1000 } }),
    )
    .await;
    st.asks.add_tokens("org", "someone", 1000);
    let (s, b) = ask_json(&app, json!({ "question": "Which teams are there?" })).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{b}");
    assert_eq!(b["code"], "budget-exceeded");
    assert!(b["resetAt"].is_string());
    assert!(m.requests().is_empty());
    // the tokens of an ask count for the principal
    put_settings(
        &app,
        json!({ "enabled": true, "budget": { "perPrincipalPerDay": 25 } }),
    )
    .await;
    let (s, _) = ask_json(&app, json!({ "question": "Which teams are there?" })).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(st.asks.tokens_today("org", Some("local")), 15);
    let (s, _) = ask_json(&app, json!({ "question": "Which teams are there?" })).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = ask_json(&app, json!({ "question": "Which teams are there?" })).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

/// A19: settings that name an unknown provider, a model the provider does not allow, an
/// unknown role, or an endpoint or key are refused.
#[tokio::test(flavor = "multi_thread")]
async fn a19_settings_refusals() {
    let (_, app) = app(
        [
            "http://127.0.0.1:9",
            "http://127.0.0.1:9",
            "http://127.0.0.1:9",
        ],
        json!({}),
    );
    for bad in [
        json!({ "enabled": true, "roles": { "draft": [{ "provider": "nope", "model": "m" }] } }),
        json!({ "enabled": true, "roles": { "draft": [{ "provider": "cheap", "model": "other" }] } }),
        json!({ "enabled": true, "roles": { "drafts": [pair("cheap")] } }),
        json!({ "enabled": true, "roles": { "draft": [{ "provider": "cheap", "model": "m", "endpoint": "http://evil" }] } }),
        json!({ "enabled": true, "apiKey": { "secret": "x" } }),
        json!({ "enabled": true, "sendByProvider": { "nope": "rows" } }),
        json!({ "enabled": true, "send": "everything" }),
    ] {
        let (s, v) = put_settings(&app, bad.clone()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad} {v}");
    }
    let ok = json!({ "enabled": true, "roles": { "draft": [{ "provider": "cheap", "model": "m2" }] }, "historyDays": 7 });
    let (s, v) = put_settings(&app, ok).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // GET answers what PUT accepts, with a status PUT ignores
    let (s, body) = send(
        &app,
        Request::get("/$/assistant/org")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let got: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(got["historyDays"], 7);
    assert_eq!(got["status"]["historyDays"], 7);
    let (s, _) = put_settings(&app, got).await;
    assert_eq!(s, StatusCode::OK);
}

/// A40 and A41 over HTTP: Try harder drafts with the next pair and rejects the earlier
/// answer; feedback reaches the history and the usage counts, which hold no text.
#[tokio::test(flavor = "multi_thread")]
async fn a40_a41_try_harder_feedback_and_usage() {
    let (c, mi, t) = (
        scripted(vec![draft(EASY)]),
        scripted(vec![draft(EASY)]),
        scripted(vec![draft(EASY)]),
    );
    let (_, app) = app(
        [&c.url(), &mi.url(), &t.url()],
        json!({ "draft": [pair("cheap"), pair("mid"), pair("top")] }),
    );
    put_settings(&app, json!({ "enabled": true })).await;
    let q = "Which teams are there in the secret plan?";
    let (_, first) = ask_json(&app, json!({ "question": q })).await;
    let id1 = first["usage"]["askId"].as_str().unwrap().to_string();
    assert_eq!(first["usage"]["answeredBy"]["provider"], "cheap");
    assert_eq!(first["usage"]["tryHarder"], true);
    let (_, second) = ask_json(&app, json!({ "question": q, "tryHarder": id1 })).await;
    assert_eq!(
        second["usage"]["answeredBy"]["provider"], "mid",
        "{second:#}"
    );
    assert_eq!(second["usage"]["escalations"][0]["signal"], "try-harder");
    let id2 = second["usage"]["askId"].as_str().unwrap().to_string();
    let (_, third) = ask_json(&app, json!({ "question": q, "tryHarder": id2 })).await;
    assert_eq!(third["usage"]["answeredBy"]["provider"], "top");
    assert_eq!(third["usage"]["tryHarder"], false);
    let id3 = third["usage"]["askId"].as_str().unwrap().to_string();
    let (s, b) = ask_json(&app, json!({ "question": q, "tryHarder": id3 })).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    // feedback
    let (s, _) = send(
        &app,
        req(
            "POST",
            &format!("/$/asks/org/{id2}/feedback"),
            json!({ "outcome": "accepted" }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = send(
        &app,
        req(
            "POST",
            "/$/asks/org/nope/feedback",
            json!({ "outcome": "accepted" }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, h) = send(
        &app,
        Request::get("/$/asks/org").body(Body::empty()).unwrap(),
    )
    .await;
    let h: Value = serde_json::from_str(&h).unwrap();
    let asks = h["asks"].as_array().unwrap();
    assert_eq!(asks.len(), 3, "{h:#}");
    let outcome = |id: &str| {
        asks.iter()
            .find(|a| a["id"] == id)
            .map(|a| a["outcome"].clone())
            .unwrap()
    };
    assert_eq!(outcome(&id1), "rejected");
    assert_eq!(outcome(&id2), "accepted");
    assert_eq!(outcome(&id3), "none");
    assert!(asks[0]["routing"]["steps"].is_array());
    assert!(asks[0].get("summary").is_none());
    let (s, u) = send(
        &app,
        Request::get("/$/models/usage?days=7")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(!u.contains("secret plan") && !u.contains("ex:Team"), "{u}");
    let u: Value = serde_json::from_str(&u).unwrap();
    let ds = &u["datasets"][0];
    assert_eq!(ds["dataset"], "org");
    assert_eq!(ds["asks"], 3);
    let count = |p: &str, o: &str| {
        ds["answers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["provider"] == p && a["outcome"] == o)
            .map_or(0, |a| a["count"].as_u64().unwrap())
    };
    assert_eq!(count("cheap", "rejected"), 1, "{ds:#}");
    assert_eq!(count("mid", "accepted"), 1);
    assert!(
        ds["escalations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["signal"] == "try-harder")
    );
}

/// A34 and A41: with `historyDays: 0` nothing is kept, the counts still change, and
/// history lists only the caller's own entries.
#[tokio::test(flavor = "multi_thread")]
async fn a34_history_is_private_and_optional() {
    let m = scripted(vec![draft(EASY)]);
    let (st, app) = app(
        [&m.url(), &m.url(), &m.url()],
        json!({ "draft": [pair("cheap")] }),
    );
    put_settings(&app, json!({ "enabled": true, "historyDays": 0 })).await;
    let (_, out) = ask_json(&app, json!({ "question": "Which teams are there?" })).await;
    let id = out["usage"]["askId"].as_str().unwrap().to_string();
    let (_, h) = send(
        &app,
        Request::get("/$/asks/org").body(Body::empty()).unwrap(),
    )
    .await;
    let h: Value = serde_json::from_str(&h).unwrap();
    assert_eq!(h["asks"], json!([]));
    let ds = st.get("org").unwrap();
    assert!(
        crate::assist::read_file(&st, &ds, crate::assistant::ASKS_FILE)
            .unwrap()
            .is_none()
    );
    let (s, _) = send(
        &app,
        req(
            "POST",
            &format!("/$/asks/org/{id}/feedback"),
            json!({ "outcome": "accepted" }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let u = st.asks.usage_json(1);
    assert_eq!(
        u["datasets"][0]["answers"][0]["outcome"], "accepted",
        "{u:#}"
    );
    // another principal's entries are never listed
    put_settings(&app, json!({ "enabled": true })).await;
    let mut other = crate::assistant::record_of("x1", "user:bob", &out);
    other.question = "Bob's private question".into();
    crate::assistant::remember(&st, &ds, other);
    ask_json(&app, json!({ "question": "Which teams are there?" })).await;
    let (_, h) = send(
        &app,
        Request::get("/$/asks/org").body(Body::empty()).unwrap(),
    )
    .await;
    assert!(!h.contains("Bob's private question"), "{h}");
    let h: Value = serde_json::from_str(&h).unwrap();
    assert_eq!(h["asks"].as_array().unwrap().len(), 1);
    // deleting one's own history leaves the others'
    let (s, _) = send(
        &app,
        Request::delete("/$/asks/org").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let raw = crate::assist::read_file(&st, &ds, crate::assistant::ASKS_FILE)
        .unwrap()
        .unwrap();
    assert_eq!(raw["asks"].as_array().unwrap().len(), 1);
    assert_eq!(raw["asks"][0]["principal"], "user:bob");
}

/// A28: with `reviewedOnly`, the query does not see a fact that only an agent's session
/// graph holds.
#[tokio::test(flavor = "multi_thread")]
async fn a28_reviewed_only() {
    let m = scripted(vec![draft(
        "SELECT ?t WHERE { { <http://example.org/resource/bo> ex:memberOf ?t } UNION { GRAPH ?g { <http://example.org/resource/bo> ex:memberOf ?t } } }",
    )]);
    let (st, app) = app(
        [&m.url(), &m.url(), &m.url()],
        json!({ "draft": [pair("cheap")] }),
    );
    let ds = st.get("org").unwrap();
    ds.store
        .load(&[Source::from_bytes(
            b"<http://example.org/resource/bo> <http://example.org/ontology#memberOf> <http://example.org/resource/payments> .".to_vec(),
            RdfFormat::NTriples,
            Some(oxrdf::NamedNode::new("https://example.org/memory/agents/agent-7/sessions/s1").unwrap()),
        )])
        .unwrap();
    let (s, _) = send(
        &app,
        req(
            "PUT",
            "/$/memory/org",
            json!({ "agentGraphs": ["https://example.org/memory/agents/*"] }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    put_settings(&app, json!({ "enabled": true })).await;
    let rows = |v: &Value| {
        v["result"]["results"]["rows"]
            .as_array()
            .map(|r| r.len())
            .unwrap_or(0)
    };
    let (_, all) = ask_json(
        &app,
        json!({ "question": "Which teams is Bo on?", "summary": false }),
    )
    .await;
    assert_eq!(rows(&all), 2, "{all:#}");
    let (_, reviewed) = ask_json(
        &app,
        json!({ "question": "Which teams is Bo on?", "summary": false, "reviewedOnly": true }),
    )
    .await;
    assert_eq!(rows(&reviewed), 1, "{reviewed:#}");
}

/// The dataset's `repairs` and a request's own repair fields reach the pipeline, the
/// request's over the dataset's.
#[tokio::test(flavor = "multi_thread")]
async fn repair_options_come_from_the_settings_and_the_request() {
    let m = scripted(vec![draft(EASY)]);
    let (_, app) = app(
        [&m.url(), &m.url(), &m.url()],
        json!({ "draft": [pair("cheap")] }),
    );
    let (s, v) = put_settings(&app, json!({ "enabled": true, "repairs": { "drafts": 9 } })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, v) = put_settings(
        &app,
        json!({ "enabled": true, "repairs": { "drafts": 2, "shapeCheck": true, "explore": false } }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ev = ask_sse(&app, json!({ "question": "How many teams are there?" })).await;
    assert!(names(&ev).contains(&"vote"), "{ev:?}");
    let shape = |ev: &[(String, Value)]| {
        ev.iter()
            .any(|(n, v)| n == "diagnosis" && v["kind"] == "shape")
    };
    assert!(shape(&ev), "{ev:?}");
    let ev = ask_sse(
        &app,
        json!({ "question": "How many teams are there?", "drafts": 1, "shapeCheck": false }),
    )
    .await;
    assert!(!names(&ev).contains(&"vote"), "{ev:?}");
    assert!(!shape(&ev), "{ev:?}");
    let (s, b) = ask_json(&app, json!({ "question": "Which teams?", "drafts": 0 })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    let (s, b) = ask_json(
        &app,
        json!({ "question": "Which teams?", "typeHints": true, "explore": true }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
}

/// The history of `org` as `GET /$/asks/org{query}` answers it.
async fn history(app: &Router, query: &str) -> (StatusCode, Value) {
    let (s, b) = send(
        app,
        Request::get(format!("/$/asks/org{query}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    (s, serde_json::from_str(&b).unwrap_or(Value::Null))
}

/// Ask threads (C18 §6.4): an ask stores its answer, its citations and its steps but
/// never its rows; a follow-up that names the thread joins it; the list filters by
/// thread oldest first; `about` reaches the model as the question's resource while the
/// history keeps the person's words.
#[tokio::test(flavor = "multi_thread")]
async fn threads_store_answers_and_group_follow_ups() {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let m = {
        let seen = seen.clone();
        MockModel::start(move |r, _| {
            seen.lock().push(r.body.to_string());
            if is_summary(r) {
                return (
                    200,
                    mock::openai(
                        &json!({ "text": "Two teams [1] [2].", "citations": [1, 2] }).to_string(),
                    ),
                );
            }
            (200, mock::openai(&draft(EASY)))
        })
    };
    let (st, app) = app(
        [&m.url(), &m.url(), &m.url()],
        json!({ "draft": [pair("cheap")], "summarize": [pair("cheap")] }),
    );
    put_settings(&app, json!({ "enabled": true, "send": "rows" })).await;
    let ev = ask_sse(
        &app,
        json!({ "question": "Which teams are there?", "about": { "iri": "http://example.org/Team", "kind": "class", "label": "Team" } }),
    )
    .await;
    let usage = find(&ev, "usage");
    let first = usage["askId"].as_str().unwrap().to_string();
    assert_eq!(usage["threadId"], first, "{usage}");
    assert!(
        seen.lock()
            .iter()
            .any(|b| b.contains("Which teams are there? (about <http://example.org/Team>)")),
        "the model reads the resource"
    );
    // a follow-up in the thread, over JSON
    let (s, out) = ask_json(
        &app,
        json!({ "question": "And their members?", "thread": first,
                "context": [{ "question": "Which teams are there?", "query": EASY }] }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["threadId"], first);
    assert_eq!(out["usage"]["threadId"], first);
    let second = out["usage"]["askId"].as_str().unwrap().to_string();
    // another thread
    ask_json(&app, json!({ "question": "Who leads them?" })).await;
    let (_, all) = history(&app, "").await;
    assert_eq!(all["asks"].as_array().unwrap().len(), 3, "{all}");
    let (s, t) = history(&app, &format!("?thread={first}")).await;
    assert_eq!(s, StatusCode::OK);
    let t = t["asks"].as_array().unwrap();
    assert_eq!(t.len(), 2);
    assert_eq!(t[0]["id"], first, "oldest first");
    assert_eq!(t[1]["id"], second);
    assert!(t.iter().all(|r| r["thread"] == first.as_str()));
    // the person's words, the resource, the stored answer and the steps; no rows
    assert_eq!(t[0]["question"], "Which teams are there?");
    assert_eq!(
        t[0]["about"],
        json!({ "iri": "http://example.org/Team", "kind": "class", "label": "Team" })
    );
    let a = &t[0]["answer"];
    assert_eq!(a["summary"]["text"], "Two teams [1] [2].", "{a}");
    assert_eq!(a["summary"]["citations"], json!([1, 2]));
    assert!(a["explanation"].is_string(), "{a}");
    assert!(
        t[0]["routing"]["steps"]
            .as_array()
            .is_some_and(|s| !s.is_empty())
    );
    assert!(
        t[0].get("access").is_none(),
        "what an answer read stays on the server"
    );
    assert!(t[0].get("answerWithheld").is_none());
    let raw = st.get("org").map(|ds| {
        crate::assist::read_file(&st, &ds, crate::assistant::ASKS_FILE)
            .unwrap()
            .unwrap()
    });
    let raw = raw.unwrap();
    assert_eq!(raw["format"], 2);
    assert!(!raw.to_string().contains("\"rows\""), "no rows are stored");
    assert!(raw["asks"][0]["access"]["footprint"].is_object(), "{raw}");
    // bad threads and resources are refused
    for (body, what) in [
        (json!({ "question": "x", "thread": "a b" }), "thread"),
        (
            json!({ "question": "x", "thread": "x".repeat(65) }),
            "thread",
        ),
        (
            json!({ "question": "x", "about": { "iri": "not an iri" } }),
            "about",
        ),
        (
            json!({ "question": "x", "about": { "iri": "http://e/x", "label": "l".repeat(201) } }),
            "about",
        ),
    ] {
        let (s, v) = ask_json(&app, body).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{what}: {v}");
    }
    let (s, _) = history(&app, "?thread=a%20b").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

fn put_raw_history(st: &AppState, v: Value) {
    let ds = st.get("org").unwrap();
    crate::assist::write_file(st, &ds, crate::assistant::ASKS_FILE, &v).unwrap();
}

fn raw_history(st: &AppState) -> Value {
    let ds = st.get("org").unwrap();
    crate::assist::read_file(st, &ds, crate::assistant::ASKS_FILE)
        .unwrap()
        .unwrap()
}

/// A history written before format 2 reads: its records have no thread, no `about`
/// and no answer, each is a thread of its own, and a new ask keeps them.
#[tokio::test(flavor = "multi_thread")]
async fn format_1_history_round_trips() {
    let m = scripted(vec![draft(EASY)]);
    let (st, app) = app(
        [&m.url(), &m.url(), &m.url()],
        json!({ "draft": [pair("cheap")] }),
    );
    put_settings(&app, json!({ "enabled": true })).await;
    let at = chrono::Utc::now().to_rfc3339();
    put_raw_history(
        &st,
        json!({ "format": 1, "asks": [{
            "id": "old1", "principal": "local", "at": at, "question": "Which teams are there?",
            "query": EASY, "commit": 1, "result": "answered", "outcome": "accepted",
            "routing": { "steps": [] }
        }] }),
    );
    let (s, h) = history(&app, "").await;
    assert_eq!(s, StatusCode::OK, "{h}");
    let r = &h["asks"][0];
    assert_eq!(r["id"], "old1");
    assert_eq!(r["thread"], "old1", "a record without a thread is its own");
    assert!(
        r.get("answer").is_none() && r.get("answerWithheld").is_none(),
        "{r}"
    );
    let (_, t) = history(&app, "?thread=old1").await;
    assert_eq!(t["asks"].as_array().unwrap().len(), 1);
    // a follow-up of the old record joins its thread, and the old record survives
    let (_, out) = ask_json(&app, json!({ "question": "Members?", "thread": "old1" })).await;
    assert_eq!(out["threadId"], "old1");
    let (_, t) = history(&app, "?thread=old1").await;
    let t = t["asks"].as_array().unwrap();
    assert_eq!(t.len(), 2);
    assert_eq!(t[0]["outcome"], "accepted");
    assert_eq!(raw_history(&st)["format"], 2);
}

/// A history of a newer format, or one this release cannot parse, is refused on every
/// route and never written over by an ask, feedback or a delete.
#[tokio::test(flavor = "multi_thread")]
async fn newer_or_damaged_history_is_refused_and_kept() {
    let m = scripted(vec![draft(EASY)]);
    let (st, app) = app(
        [&m.url(), &m.url(), &m.url()],
        json!({ "draft": [pair("cheap")] }),
    );
    put_settings(&app, json!({ "enabled": true })).await;
    for (file, code) in [
        (
            json!({ "format": 3, "asks": [{ "id": "future", "shape": "unknown" }] }),
            "unsupported-format",
        ),
        (
            json!({ "format": 2, "asks": "not a list" }),
            "unreadable-history",
        ),
    ] {
        put_raw_history(&st, file.clone());
        let (s, v) = history(&app, "").await;
        assert_eq!(s, StatusCode::CONFLICT, "{v}");
        assert_eq!(v["code"], code, "{v}");
        let (_, out) = ask_json(&app, json!({ "question": "Which teams are there?" })).await;
        let id = out["usage"]["askId"].as_str().unwrap().to_string();
        assert_eq!(raw_history(&st), file, "an ask leaves the file");
        let (s, _) = send(
            &app,
            req(
                "POST",
                &format!("/$/asks/org/{id}/feedback"),
                json!({ "outcome": "rejected" }),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT, "feedback still counts");
        assert_eq!(raw_history(&st), file, "feedback leaves the file");
        let (s, _) = send(
            &app,
            Request::delete("/$/asks/org").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT);
        assert_eq!(raw_history(&st), file, "a delete leaves the file");
        crate::assistant::prune(&st);
        assert_eq!(raw_history(&st), file, "pruning leaves the file");
    }
}
