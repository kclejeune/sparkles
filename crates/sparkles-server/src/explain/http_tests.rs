//! `POST /{ds}/sparql/explain` and `explain_query` against mock providers (C18 A44 to
//! A47 and A57).

use crate::auth::{Access, Grants, Kind, Level as L, Principal, Restricted, Scheme};
use crate::mcp::{Call, McpServer, Outcome};
use crate::models::mock::{self, MockModel};
use crate::models::{Models, ModelsConfig};
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Map, Value, json};
use sparkles::io::{RdfFormat, Source};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use tower::ServiceExt;

fn people(n: usize) -> String {
    let mut d = String::from("@prefix ex: <http://example.org/> .\n");
    for i in 0..n {
        d.push_str(&format!(
            "ex:p{i} ex:v {} ; ex:name \"n{i}\" ; ex:team ex:t{} .\n",
            i % 97,
            i % 7
        ));
    }
    d
}

/// A server with `org` and, when `url` is given, a provider `mock` for the explain role.
fn app(url: Option<&str>, n: usize) -> (Arc<AppState>, Router) {
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    if let Some(url) = url {
        let cfg = json!({
            "providers": { "mock": { "kind": "openai", "endpoint": url, "requestTimeoutSecs": 5,
                                      "allowedModels": ["m"] } },
            "roles": { "explain": [ { "provider": "mock", "model": "m" } ] },
        });
        st.models = Some(Arc::new(Models::new(
            ModelsConfig::parse(&cfg.to_string()).unwrap(),
            Default::default(),
            sparkles::outbound::OutboundPolicy {
                allow_private: true,
                ..Default::default()
            },
        )));
    }
    let st = Arc::new(st);
    let ds = st.attach("org", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            people(n).into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let router = crate::http::router(st.clone());
    (st, router)
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let res = app.clone().oneshot(req).await.unwrap();
    let s = res.status();
    let b = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (s, String::from_utf8_lossy(&b).into_owned())
}

async fn explain_json(app: &Router, body: Value) -> (StatusCode, Value) {
    let r = Request::post("/org/sparql/explain")
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (s, t) = send(app, r).await;
    (s, serde_json::from_str(&t).unwrap_or(Value::Null))
}

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

/// Every node id of a `PlanNode` or `CursorPlan` tree, checking that each is its path.
fn ids(v: &Value, want: &str, out: &mut Vec<String>) {
    assert_eq!(v["id"], want, "{v}");
    out.push(want.to_string());
    for (i, c) in v["children"].as_array().unwrap().iter().enumerate() {
        ids(c, &format!("{want}.{i}"), out);
    }
}

fn node(op: &str, desc: &str, est: f64, act: i64, ms: f64, children: Vec<Value>) -> Value {
    json!({ "operator": op, "description": desc, "columns": ["team"], "estimatedRows": est,
            "estimatedCost": est, "actualRows": act, "timeMs": ms, "children": children })
}

/// A plan whose hash join takes most of the time, with a misestimated scan.
fn join_plan() -> Value {
    node(
        "Project",
        "?p ?team",
        50.0,
        52_000,
        2_400.0,
        vec![node(
            "HashJoin",
            "on ?team",
            50.0,
            52_000,
            2_350.0,
            vec![
                node(
                    "IndexScan",
                    "PSO ?p <http://example.org/team> ?team",
                    300.0,
                    3_000,
                    2_000.0,
                    vec![],
                ),
                node(
                    "IndexScan",
                    "PSO ?team <http://example.org/partOf> ?u",
                    7.0,
                    7,
                    0.5,
                    vec![],
                ),
            ],
        )],
    )
}

/// A44: without an explain role, a run explains its executed plan with node ids and a
/// template description whose every sentence names a node of the plan. A plan whose
/// hash join takes most of the time gets the `dominant` note on that join.
#[tokio::test(flavor = "multi_thread")]
async fn a44_a_run_is_explained_without_a_model() {
    let (_, app) = app(None, 400);
    let (s, v) = explain_json(
        &app,
        json!({ "query": "PREFIX ex: <http://example.org/> SELECT ?p ?n WHERE { ?p ex:team ?t ; ex:name ?n FILTER(?t != ex:t3) } ORDER BY ?n LIMIT 10", "profile": "run" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["executed"], true);
    assert_eq!(v["rows"], 10);
    let mut all = Vec::new();
    ids(&v["plan"], "0", &mut all);
    assert!(all.len() >= 3, "{v}");
    let e = &v["explanation"];
    assert_eq!(e["source"], "template", "{e}");
    let asks = e["asks"].as_array().unwrap();
    assert!((1..=4).contains(&asks.len()), "{e}");
    for a in asks {
        let nodes = a["nodes"].as_array().unwrap();
        assert!(!nodes.is_empty(), "{a}");
        assert!(
            nodes
                .iter()
                .all(|n| all.contains(&n.as_str().unwrap().to_string())),
            "{a}"
        );
    }
    for n in v["notes"].as_array().unwrap() {
        if let Some(id) = n["node"].as_str() {
            assert!(all.contains(&id.to_string()), "{n}");
        }
    }
    // the hash join's own time dominates, and the scan was misestimated tenfold
    let (s, v) = explain_json(
        &app,
        json!({ "query": "SELECT * WHERE { ?p <http://example.org/team> ?team . ?team <http://example.org/partOf> ?u }",
                "profile": "given", "plan": join_plan(), "commit": 1 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let notes = v["notes"].as_array().unwrap();
    let dominant = notes.iter().find(|n| n["code"] == "dominant").unwrap();
    assert_eq!(dominant["node"], "0.0.0", "{v:#}");
    assert!(
        notes
            .iter()
            .any(|n| n["code"] == "misestimate" && n["node"] == "0.0.0"),
        "{v:#}"
    );
    assert_eq!(v["explanation"]["source"], "template");
    assert_eq!(v["commit"], 1);
}

/// A45: the model's sentence that cites a node the plan lacks is dropped, and its note
/// with a time the node did not take is replaced by the deterministic one.
#[tokio::test(flavor = "multi_thread")]
async fn a45_model_text_is_checked_against_the_plan() {
    let answer = json!({
        "asks": [
            { "text": "Finds each person's team and what it is part of.", "nodes": ["0.0"] },
            { "text": "Looks at a node that is not there.", "nodes": ["0.9"] }
        ],
        "notes": [ { "node": "0.0.0", "text": "The scan of teams took 40 s." } ]
    });
    let mock = MockModel::start(move |_, _| (200, mock::openai(&answer.to_string())));
    let (_, app) = app(Some(&mock.url()), 50);
    let r = Request::put("/$/assistant/org")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "enabled": true, "explain": true }).to_string(),
        ))
        .unwrap();
    let (s, t) = send(&app, r).await;
    assert_eq!(s, StatusCode::OK, "{t}");
    let r = Request::post("/org/sparql/explain")
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .body(Body::from(
            json!({ "query": "SELECT * WHERE { ?p <http://example.org/team> ?team . ?team <http://example.org/partOf> ?u }",
                    "profile": "given", "plan": join_plan() })
            .to_string(),
        ))
        .unwrap();
    let (s, t) = send(&app, r).await;
    assert_eq!(s, StatusCode::OK, "{t}");
    let ev = events(&t);
    let names: Vec<&str> = ev.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["plan", "notes", "explanation", "usage"], "{t}");
    let e = &ev[2].1;
    assert_eq!(e["source"], "model", "{e}");
    assert_eq!(e["asks"].as_array().unwrap().len(), 1);
    assert_eq!(e["asks"][0]["nodes"], json!(["0.0"]));
    assert_eq!(e["dropped"], 1);
    assert_eq!(e["replaced"], 1);
    let scan = e["notes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node"] == "0.0.0" && n["code"] == "dominant")
        .unwrap();
    assert_eq!(scan["source"], "explain");
    assert!(!scan["text"].as_str().unwrap().contains("40 s"), "{scan}");
    assert!(!e["template"]["asks"].as_array().unwrap().is_empty());
    // the prompt holds the plan's lines and no result rows
    let prompt = mock::prompt_of(&mock.requests()[0]);
    assert!(prompt.contains("0.0.0 IndexScan"), "{prompt}");
    assert_eq!(ev[3].1["modelCalls"], 1);
}

/// A46: a query stopped by its timeout keeps its plan in the error body, and **Explain
/// why** puts the budget first, then the node that spent the time, with partial counts.
#[tokio::test(flavor = "multi_thread")]
async fn a46_a_timeout_is_explained_from_its_partial_plan() {
    let (_, app) = app(None, 3000);
    let q = "PREFIX ex: <http://example.org/> SELECT * WHERE { ?a ex:v ?x . ?b ex:v ?y FILTER(REGEX(STR(?x + ?y), \"^zzz\")) }";
    let r = Request::post("/org/sparql?timeout=0.3")
        .header("content-type", "application/sparql-query")
        .header("accept", "application/x-sparkles+json")
        .body(Body::from(q))
        .unwrap();
    let (s, t) = send(&app, r).await;
    assert_eq!(s, StatusCode::REQUEST_TIMEOUT, "{t}");
    let err: Value = serde_json::from_str(&t).unwrap();
    let plan = err["plan"].clone();
    assert!(plan.is_object(), "{err}");
    let text = plan.to_string();
    assert!(text.contains("\"complete\":false"), "{text}");
    let mut body = err.clone();
    body.as_object_mut().unwrap().remove("plan");
    let (s, v) = explain_json(
        &app,
        json!({ "query": q, "profile": "given", "plan": plan, "commit": err["commit"], "error": body }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let notes = v["notes"].as_array().unwrap();
    assert_eq!(notes[0]["code"], "budget", "{v:#}");
    assert!(
        notes[0]["text"].as_str().unwrap().contains("timeout"),
        "{v:#}"
    );
    assert_eq!(notes[1]["code"], "dominant", "{v:#}");
    let dominant = notes[1]["node"].as_str().unwrap();
    let facts = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == dominant)
        .unwrap();
    assert!(
        facts["partial"] == true || facts["complete"] == true,
        "{facts}"
    );
    assert!(
        v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["partial"] == true),
        "{v:#}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_requests_are_refused() {
    let (_, app) = app(None, 10);
    let (s, v) = explain_json(
        &app,
        json!({ "query": "INSERT DATA { <a:a> <a:b> <a:c> }" }),
    )
    .await;
    assert_eq!(
        (s, v["code"].clone()),
        (StatusCode::BAD_REQUEST, json!("not-a-query")),
        "{v}"
    );
    let (s, v) = explain_json(&app, json!({ "query": "SELECT * {}", "profile": "given" })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, v) = explain_json(
        &app,
        json!({ "query": "SELECT * {}", "profile": "given", "plan": { "nope": 1 } }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, _) = explain_json(&app, json!({ "query": "SELECT * {}", "profile": "fast" })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = explain_json(&app, json!({ "query": "SELECT * { ?s ?p ?o } LIMIT 1" })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["executed"], false);
}

fn principal(name: &str, server_models: bool, graphs: Option<Vec<String>>) -> Principal {
    let access = Access::of(Grants {
        restricted: vec![Restricted {
            dataset: "org".into(),
            level: L::Read,
            graphs,
            endpoints: None,
            lifts: Vec::new(),
            branches: None,
            server_models,
        }],
        ..Default::default()
    });
    Principal::new(Kind::User, name, Scheme::Basic, access)
}

fn call(p: Principal) -> Call {
    Call {
        arrived: Instant::now(),
        cancel: Arc::new(AtomicBool::new(false)),
        request_id: String::new(),
        principal: p,
        headers: None,
        held: None,
    }
}

fn tool(
    st: &Arc<AppState>,
    p: Principal,
    args: Value,
) -> Result<Value, crate::mcp::errors::ToolError> {
    let server = McpServer::new(
        st.clone(),
        crate::mcp::rest::config(st, crate::mcp::rest::Mode::Read),
    );
    let Value::Object(m) = args else { panic!() };
    let mut m: Map<String, Value> = m;
    m.insert("dataset".into(), "org".into());
    match server.run_now("explain_query", m, &call(p))? {
        Outcome::Structured(v) => Ok(v),
        Outcome::Text(t) => panic!("{t}"),
    }
}

/// A47: without the new arguments the tool answers as before. With a run and notes it
/// adds node facts, notes and the template description, and no provider is asked. A
/// limited caller sees hidden estimates as hidden.
#[tokio::test(flavor = "multi_thread")]
async fn a47_explain_query_with_notes_and_a_run() {
    let mock = MockModel::start(|_, _| (200, mock::openai("{}")));
    let (st, _) = app(Some(&mock.url()), 300);
    let q = "PREFIX ex: <http://example.org/> SELECT ?p ?n WHERE { ?p ex:team ex:t2 ; ex:name ?n } LIMIT 5";
    let full = principal("ana", false, None);
    let st2 = st.clone();
    let before = tokio::task::spawn_blocking(move || tool(&st2, full, json!({ "query": q })))
        .await
        .unwrap()
        .unwrap();
    let keys: Vec<&String> = before.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "commit",
            "dataset",
            "estimatedRows",
            "plan",
            "queryType",
            "warnings"
        ]
    );
    assert!(!before["plan"].as_str().unwrap().starts_with("0 "));
    let full = principal("ana", false, None);
    let st2 = st.clone();
    let v = tokio::task::spawn_blocking(move || {
        tool(
            &st2,
            full,
            json!({ "query": q, "profile": "run", "notes": true }),
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(v["rows"], 5, "{v:#}");
    assert_eq!(v["source"], "template");
    let nodes = v["nodes"].as_array().unwrap();
    assert_eq!(nodes[0]["id"], "0");
    let plan = v["plan"].as_str().unwrap();
    assert!(plan.starts_with("0 "), "{plan}");
    assert!(plan.contains("act=") && plan.contains("ms="), "{plan}");
    for s in v["asks"].as_array().unwrap() {
        for n in s["nodes"].as_array().unwrap() {
            assert!(nodes.iter().any(|x| x["id"] == *n), "{s}");
        }
    }
    assert!(v["notes"].is_array());
    assert!(mock.requests().is_empty());
    // a caller limited to the default graph sees no estimates
    let limited = principal("bo", false, Some(vec!["default".into()]));
    let st2 = st.clone();
    let v = tokio::task::spawn_blocking(move || {
        tool(&st2, limited, json!({ "query": q, "notes": true }))
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(v["estimatedRows"], Value::Null, "{v:#}");
    assert!(v["plan"].as_str().unwrap().contains("est=?"));
    assert!(v["nodes"][0]["estimatedRows"].is_null());
    assert!(
        v["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["code"] == "hidden-estimates"),
        "{v:#}"
    );
}

/// A57: `useServerModel` without the `serverModels` grant is refused before any
/// provider is called. With it, the model writes the description, every sentence cites
/// a node, and the tokens are charged to the caller.
#[tokio::test(flavor = "multi_thread")]
async fn a57_server_models_need_the_grant() {
    let mock = MockModel::start(|_, _| {
        (
            200,
            mock::openai(
                &json!({ "asks": [ { "text": "Finds the names of people in team 2.", "nodes": ["0"] } ],
                         "notes": [] })
                .to_string(),
            ),
        )
    });
    let (st, _) = app(Some(&mock.url()), 100);
    let q = "PREFIX ex: <http://example.org/> SELECT ?n WHERE { ?p ex:team ex:t2 ; ex:name ?n } LIMIT 5";
    let st2 = st.clone();
    let e = tokio::task::spawn_blocking(move || {
        tool(
            &st2,
            principal("agent-7", false, None),
            json!({ "query": q, "notes": true, "useServerModel": true }),
        )
    })
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(e.code, "server-model-not-allowed");
    assert!(mock.requests().is_empty());
    let st2 = st.clone();
    let v = tokio::task::spawn_blocking(move || {
        tool(
            &st2,
            principal("agent-7", true, None),
            json!({ "query": q, "notes": true, "useServerModel": true }),
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(v["source"], "model", "{v:#}");
    let nodes = v["nodes"].as_array().unwrap();
    for s in v["asks"].as_array().unwrap() {
        for n in s["nodes"].as_array().unwrap() {
            assert!(nodes.iter().any(|x| x["id"] == *n), "{s}");
        }
    }
    assert_eq!(mock.requests().len(), 1);
    let me = principal("agent-7", true, None).id();
    assert!(st.asks.tokens_today("org", Some(&me)) > 0);
}
