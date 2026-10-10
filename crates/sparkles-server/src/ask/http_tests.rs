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
    st.models = Some(Arc::new(models));
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
    let ev = ask_sse(
        &app,
        json!({ "question": "How many teams does Acme have?" }),
    )
    .await;
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
