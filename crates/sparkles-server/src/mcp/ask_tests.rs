//! Offline unit and integration tests for the `ask` MCP tool (C18 / C33, item 488):
//! discovery/schema, profile gating, caller-scoped auth & graph visibility, branches,
//! reviewedOnly, provider/model overrides, self-repair & citations, clarification,
//! and deadline/cancellation/token-budget enforcement.

use super::adapter::Adapter;
use super::{Call, McpConfig, McpServer, Outcome};
use crate::auth::{Access, Endpoint, Grants, Kind, Level, Principal, Restricted, Scheme};
use crate::models::mock::{self, MockModel};
use crate::models::{Models, ModelsConfig};
use crate::state::{AppState, DbType};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use sparkles::io::{RdfFormat, Source};
use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use tower::ServiceExt;

const ORG: &str = r#"@prefix ex:   <http://example.org/ontology#> .
@prefix res:  <http://example.org/resource/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:Person a <http://www.w3.org/2002/07/owl#Class> ; rdfs:label "Person"@en .
ex:Team a <http://www.w3.org/2002/07/owl#Class> ; rdfs:label "Team"@en .
res:payments a ex:Team ; rdfs:label "Payments team"@en .
res:platform a ex:Team ; rdfs:label "Platform team"@en .
res:ana a ex:Person ; foaf:name "Ana Lima"@en ; ex:memberOf res:payments .
res:bo a ex:Person ; foaf:name "Bo Chen"@en ; ex:memberOf res:platform .
res:alex1 a ex:Person ; rdfs:label "Alex"@en ; ex:memberOf res:payments .
res:alex2 a ex:Person ; rdfs:label "Alex"@en ; ex:memberOf res:platform .
"#;

const TEAMS_QUERY: &str = "SELECT ?t WHERE { ?t a ex:Team } ORDER BY ?t";

fn draft_json(query: &str) -> String {
    json!({
        "query": query,
        "explanation": "Lists the matching teams.",
        "assumptions": ["team means ex:Team"],
        "clarify": { "question": "", "choices": [] },
        "graph": { "subject": "", "predicate": "", "object": "" }
    })
    .to_string()
}

fn is_summary(r: &mock::Received) -> bool {
    r.body.to_string().contains("rows of a query result")
}

fn scripted(drafts: Vec<String>) -> MockModel {
    let drafts = Mutex::new(drafts.into_iter().collect::<VecDeque<_>>());
    MockModel::start(move |r, _| {
        if is_summary(r) {
            return (
                200,
                mock::openai(
                    &json!({
                        "text": "There are two teams [1] [2]. Ghost row [90].",
                        "citations": [1, 2, 90]
                    })
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

fn make_models(cheap_url: &str, top_url: &str, roles: Value) -> Models {
    let cfg = json!({
        "providers": {
            "cheap": {
                "kind": "openai",
                "endpoint": cheap_url,
                "requestTimeoutSecs": 5,
                "allowedModels": ["m", "m2"]
            },
            "top": {
                "kind": "openai",
                "endpoint": top_url,
                "requestTimeoutSecs": 5,
                "allowedModels": ["m", "m2"]
            }
        },
        "roles": roles
    });
    Models::new(
        ModelsConfig::parse(&cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    )
}

fn make_state(models: Option<Models>) -> Arc<AppState> {
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    st.limits.max_result_bytes = None;
    if let Some(m) = models {
        st.models = arc_swap::ArcSwapOption::from_pointee(m);
    }
    let st = Arc::new(st);
    let ds = st.attach("org", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            ORG.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    st
}

async fn put_assistant(st: &Arc<AppState>, ds: &str, body: Value) {
    let router = crate::http::router(st.clone());
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/$/assistant/{ds}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "PUT /$/assistant/{ds} failed: {}",
        String::from_utf8_lossy(&bytes)
    );
}

async fn put_memory(st: &Arc<AppState>, ds: &str, body: Value) {
    let router = crate::http::router(st.clone());
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/$/memory/{ds}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "PUT /$/memory/{ds} failed: {}",
        String::from_utf8_lossy(&bytes)
    );
}

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

struct Client {
    w: WriteHalf<DuplexStream>,
    r: Lines<BufReader<ReadHalf<DuplexStream>>>,
}

impl Client {
    fn start(server: McpServer) -> Client {
        let (client, srv) = tokio::io::duplex(1 << 22);
        let (sr, sw) = tokio::io::split(srv);
        tokio::spawn(super::adapter::serve(Adapter::new(server), sr, sw));
        let (r, w) = tokio::io::split(client);
        Client {
            w,
            r: BufReader::new(r).lines(),
        }
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        let mut line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .unwrap();
        line.push('\n');
        self.w.write_all(line.as_bytes()).await.unwrap();
        self.w.flush().await.unwrap();
        let resp = tokio::time::timeout(Duration::from_secs(30), self.r.next_line())
            .await
            .expect("MCP response timeout")
            .unwrap()
            .expect("MCP stream closed");
        serde_json::from_str(&resp).unwrap()
    }

    async fn tools_list(&mut self) -> Vec<Value> {
        let r = self
            .request(1, "tools/list", json!({ "_meta": modern_meta() }))
            .await;
        r["result"]["tools"].as_array().cloned().unwrap()
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request(
            2,
            "tools/call",
            json!({ "_meta": modern_meta(), "name": name, "arguments": arguments }),
        )
        .await
    }

    async fn structured(&mut self, name: &str, arguments: Value) -> Value {
        let r = self.call(name, arguments).await;
        assert!(r.get("error").is_none(), "protocol error: {r}");
        let res = &r["result"];
        assert_ne!(res["isError"], true, "unexpected tool error: {r}");
        res["structuredContent"].clone()
    }

    async fn tool_error(&mut self, name: &str, arguments: Value) -> (String, Value) {
        let r = self.call(name, arguments).await;
        assert!(
            r.get("error").is_none(),
            "expected tool error, got protocol error: {r}"
        );
        let res = &r["result"];
        assert_eq!(res["isError"], true, "expected tool error: {r}");
        let text = res["content"][0]["text"].as_str().unwrap_or("").to_string();
        let meta = res["_meta"][super::errors::ERROR_META].clone();
        (text, meta)
    }
}

fn call_for(principal: Principal) -> Call {
    Call {
        arrived: Instant::now(),
        cancel: Arc::new(AtomicBool::new(false)),
        request_id: "ask-test".into(),
        principal,
        headers: None,
        held: None,
        grant: None,
        preparation: None,
    }
}

fn args_map(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

fn restricted_principal(
    name: &str,
    dataset: &str,
    level: Level,
    graphs: Option<Vec<String>>,
    endpoints: Option<Vec<Endpoint>>,
    branches: Option<Vec<String>>,
    server_models: bool,
) -> Principal {
    let access = Access::of(Grants {
        restricted: vec![Restricted {
            dataset: dataset.into(),
            level,
            graphs,
            endpoints,
            lifts: Vec::new(),
            branches,
            server_models,
        }],
        ..Default::default()
    });
    Principal::new(Kind::User, name, Scheme::Basic, access)
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_discovery_schema_and_list_datasets() {
    // Without model providers, `ask` is hidden from tools/list and list_datasets omits `ask`.
    let st_no_models = make_state(None);
    let mut c = Client::start(McpServer::new(st_no_models, McpConfig::default()));
    let names: Vec<String> = c
        .tools_list()
        .await
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(!names.contains(&"ask".to_string()), "{names:?}");
    let ds_list = c.structured("list_datasets", json!({})).await;
    assert!(ds_list["datasets"][0].get("ask").is_none(), "{ds_list}");

    // With model providers but assistant disabled, `ask` is still hidden.
    let m = scripted(vec![draft_json(TEAMS_QUERY)]);
    let models = make_models(
        &m.url(),
        &m.url(),
        json!({ "draft": [{ "provider": "cheap", "model": "m" }] }),
    );
    let st = make_state(Some(models));
    st.attach("other", DbType::Mem, None).unwrap();
    let server = McpServer::new(st.clone(), McpConfig::default());
    let mut c = Client::start(server.clone());
    let names: Vec<String> = c
        .tools_list()
        .await
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(!names.contains(&"ask".to_string()), "{names:?}");

    // Enabling assistant on `org` surfaces `ask` dynamically with its schema and annotations.
    put_assistant(&st, "org", json!({ "enabled": true, "send": "rows" })).await;
    let tools = c.tools_list().await;
    let ask_tool = tools
        .iter()
        .find(|t| t["name"] == "ask")
        .expect("ask should be listed once assistant is enabled");
    assert_eq!(
        ask_tool["annotations"],
        json!({ "readOnlyHint": true, "openWorldHint": true })
    );
    let props = &ask_tool["inputSchema"]["properties"];
    for key in [
        "dataset",
        "branch",
        "question",
        "context",
        "clarification",
        "atCommit",
        "at",
        "reasoning",
        "mode",
        "run",
        "summary",
        "maxRows",
        "reviewedOnly",
        "query",
        "tryHarder",
        "reviewPolicy",
        "matchingPolicy",
        "provider",
        "model",
        "maxTokens",
        "timeoutSeconds",
    ] {
        assert!(props.get(key).is_some(), "missing input property {key}");
    }
    assert!(ask_tool["outputSchema"].is_object());

    // `list_datasets` marks `org` with `ask: true` and leaves `other` unmarked.
    let ds_list = c.structured("list_datasets", json!({})).await;
    let find_ds = |name: &str| {
        ds_list["datasets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(find_ds("org")["ask"], true);
    assert!(find_ds("other").get("ask").is_none());

    // Calling `ask` on `other` (which has no assistant) returns `no-assistant` before any model call.
    let (_, meta) = c
        .tool_error(
            "ask",
            json!({ "dataset": "other", "question": "What exists?" }),
        )
        .await;
    assert_eq!(meta["code"], "no-assistant");
    assert_eq!(meta["status"], 404);
    assert!(m.requests().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_profile_gating_and_tools_only() {
    let m = scripted(vec![draft_json(TEAMS_QUERY)]);
    let models = make_models(
        &m.url(),
        &m.url(),
        json!({
            "draft": [{ "provider": "cheap", "model": "m" }],
            "summarize": [{ "provider": "cheap", "model": "m" }]
        }),
    );
    let st = make_state(Some(models));
    put_assistant(&st, "org", json!({ "enabled": true, "send": "rows" })).await;

    // `explore` profile includes `ask`.
    let only_explore =
        super::profiles::select(&["explore".into()], &[], ("--profile", "--tools")).unwrap();
    let srv_explore = McpServer::new(
        st.clone(),
        McpConfig {
            only: only_explore,
            ..McpConfig::default()
        },
    );
    assert!(
        srv_explore
            .tools_for(&Principal::local())
            .iter()
            .any(|t| t.name == "ask")
    );

    // `memory` and `admin` profiles exclude `ask`.
    for profile in ["memory", "admin"] {
        let only =
            super::profiles::select(&[profile.into()], &[], ("--profile", "--tools")).unwrap();
        let srv = McpServer::new(
            st.clone(),
            McpConfig {
                only,
                ..McpConfig::default()
            },
        );
        let mut c = Client::start(srv);
        assert!(
            !c.tools_list().await.iter().any(|t| t["name"] == "ask"),
            "{profile} must not list ask"
        );
        let r = c
            .call("ask", json!({ "question": "Which teams exist?" }))
            .await;
        assert_eq!(r["error"]["code"], -32602, "{profile}: {r}");
    }

    // `--disable-tool ask` hides and refuses `ask`.
    let srv_disabled = McpServer::new(
        st.clone(),
        McpConfig {
            disabled: BTreeSet::from(["ask".to_string()]),
            ..McpConfig::default()
        },
    );
    assert!(!srv_disabled.known(&Principal::local(), "ask"));

    // `--tools ask` exposes ONLY `ask`, yet internal grounding/check/sparql sub-tools still run.
    let only_ask = super::profiles::select(&[], &["ask".into()], ("--profile", "--tools")).unwrap();
    let srv_only_ask = McpServer::new(
        st.clone(),
        McpConfig {
            only: only_ask,
            ..McpConfig::default()
        },
    );
    let mut c = Client::start(srv_only_ask);
    let listed: Vec<String> = c
        .tools_list()
        .await
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(listed, vec!["ask"]);
    let out = c
        .structured("ask", json!({ "question": "Which teams are there?" }))
        .await;
    assert_eq!(out["outcome"], "answered", "{out}");
    assert_eq!(
        out["result"]["results"]["rows"].as_array().unwrap().len(),
        2
    );
    assert_eq!(out["summary"]["citations"], json!([1, 2]));
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_answers_repairs_citations_preview_privacy_and_try_harder() {
    // First draft uses an unknown predicate so check_query fails and triggers self-repair.
    let bad_draft = draft_json("SELECT ?t WHERE { ?t ex:nonexistent ?x }");
    let good_draft = draft_json(TEAMS_QUERY);
    let cheap = scripted(vec![bad_draft, good_draft.clone()]);
    let top = scripted(vec![good_draft]);
    let models = make_models(
        &cheap.url(),
        &top.url(),
        json!({
            "draft": [
                { "provider": "cheap", "model": "m" },
                { "provider": "top", "model": "m" }
            ],
            "repair": [{ "provider": "cheap", "model": "m" }],
            "summarize": [{ "provider": "cheap", "model": "m" }]
        }),
    );
    let st = make_state(Some(models));
    put_assistant(&st, "org", json!({ "enabled": true, "send": "rows" })).await;
    let mut c = Client::start(McpServer::new(st.clone(), McpConfig::default()));

    // Self-repair + citations (invalid [90] stripped).
    let out = c
        .structured("ask", json!({ "question": "Which teams exist?" }))
        .await;
    assert_eq!(out["outcome"], "answered", "{out}");
    assert_eq!(out["attempts"].as_array().unwrap().len(), 2, "{out}");
    assert!(out["result"]["query"].as_str().unwrap().contains("ex:Team"));
    assert_eq!(out["result"]["results"]["queryType"], "SELECT");
    assert_eq!(
        out["result"]["results"]["rows"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        out["summary"]["text"],
        "There are two teams [1] [2]. Ghost row."
    );
    assert_eq!(out["summary"]["citations"], json!([1, 2]));
    let ask_id = out["usage"]["askId"].as_str().unwrap().to_string();
    assert_eq!(out["usage"]["tryHarder"], true);

    // `tryHarder` starts drafting at the next pair (`top`).
    let harder = c
        .structured(
            "ask",
            json!({ "question": "Which teams exist?", "tryHarder": ask_id }),
        )
        .await;
    assert_eq!(harder["outcome"], "answered", "{harder}");
    assert_eq!(harder["usage"]["answeredBy"]["provider"], "top", "{harder}");
    assert!(!top.requests().is_empty());

    // `run: false` stops after check_query without executing the query.
    let preview = c
        .structured(
            "ask",
            json!({ "question": "Which teams exist?", "run": false }),
        )
        .await;
    assert_eq!(preview["outcome"], "checked", "{preview}");
    assert!(
        preview["result"]["query"]
            .as_str()
            .unwrap()
            .contains("ex:Team")
    );
    assert!(preview["result"].get("results").is_none());
    assert!(preview.get("summary").is_none());

    // `send: "schema"` disables summaries and never sends rows to any provider.
    put_assistant(&st, "org", json!({ "enabled": true, "send": "schema" })).await;
    let before = cheap.requests().len();
    let schema_only = c
        .structured("ask", json!({ "question": "Which teams exist?" }))
        .await;
    assert_eq!(schema_only["outcome"], "answered", "{schema_only}");
    assert!(schema_only.get("summary").is_none(), "{schema_only}");
    let new_reqs = &cheap.requests()[before..];
    assert_eq!(new_reqs.len(), 1);
    assert!(!is_summary(&new_reqs[0]));
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_clarification_unanswerable_and_validation() {
    let m = scripted(vec![draft_json(
        "SELECT ?t WHERE { res:alex1 ex:memberOf ?t }",
    )]);
    let models = make_models(
        &m.url(),
        &m.url(),
        json!({ "draft": [{ "provider": "cheap", "model": "m" }] }),
    );
    let st = make_state(Some(models));
    put_assistant(&st, "org", json!({ "enabled": true, "send": "schema" })).await;
    let mut c = Client::start(McpServer::new(st.clone(), McpConfig::default()));

    // Ambiguous mention ("Alex") returns `clarify` before calling any model.
    let clarify = c
        .structured("ask", json!({ "question": "Which team is Alex in?" }))
        .await;
    assert_eq!(clarify["outcome"], "clarify", "{clarify}");
    assert_eq!(clarify["clarify"]["id"], "mention");
    assert_eq!(
        clarify["clarify"]["choices"].as_array().unwrap().len(),
        2,
        "{clarify}"
    );
    assert!(m.requests().is_empty(), "clarify should not call the model");

    // Passing `clarification` resumes and answers.
    let answered = c
        .structured(
            "ask",
            json!({
                "question": "Which team is Alex in?",
                "clarification": { "id": "mention", "value": "res:alex1" }
            }),
        )
        .await;
    assert_eq!(answered["outcome"], "answered", "{answered}");
    assert_eq!(
        answered["result"]["results"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Argument validation errors happen before calling any model.
    let before = m.requests().len();
    for bad in [
        json!({ "question": "   " }),
        json!({ "question": "Who?", "at": "head", "atCommit": 1 }),
        json!({ "question": "Who?", "provider": "cheap" }),
        json!({ "question": "Who?", "query": TEAMS_QUERY, "tryHarder": "abc" }),
        json!({ "question": "Who?", "maxTokens": 999_999 }),
        json!({ "question": "Who?", "mode": "global", "reviewPolicy": "default" }),
    ] {
        let (_, meta) = c.tool_error("ask", bad.clone()).await;
        assert_eq!(
            meta["code"], "bad-argument",
            "expected bad-argument for {bad}"
        );
    }
    assert_eq!(m.requests().len(), before);

    // Empty draft returns `unanswerable`.
    let m_unans = scripted(vec![draft_json("")]);
    let st_unans = make_state(Some(make_models(
        &m_unans.url(),
        &m_unans.url(),
        json!({ "draft": [{ "provider": "cheap", "model": "m" }] }),
    )));
    put_assistant(&st_unans, "org", json!({ "enabled": true })).await;
    let mut c2 = Client::start(McpServer::new(st_unans, McpConfig::default()));
    let unans = c2
        .structured("ask", json!({ "question": "What is the weather?" }))
        .await;
    assert_eq!(unans["outcome"], "unanswerable", "{unans}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_auth_graph_visibility_branches_reviewed_only_and_overrides() {
    let cheap = scripted(vec![draft_json(
        "SELECT ?t ?l WHERE { GRAPH ?g { ?t a ex:Team ; rdfs:label ?l } } ORDER BY ?t",
    )]);
    let top = scripted(vec![draft_json(
        "SELECT ?t ?l WHERE { GRAPH ?g { ?t a ex:Team ; rdfs:label ?l } } ORDER BY ?t",
    )]);
    let models = make_models(
        &cheap.url(),
        &top.url(),
        json!({
            "draft": [{ "provider": "cheap", "model": "m" }],
            "summarize": [
                { "provider": "cheap", "model": "m" },
                { "provider": "top", "model": "m" }
            ]
        }),
    );
    let st = make_state(Some(models));
    let ds = st.get("org").unwrap();
    ds.store
        .load(&[Source::from_bytes(
            br#"@prefix ex: <http://example.org/ontology#> .
@prefix res: <http://example.org/resource/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/public> {
  res:pub_team a ex:Team ; rdfs:label "Public team"@en .
}
<http://example.org/secret> {
  res:sec_team a ex:Team ; rdfs:label "TopSecretCodename"@en .
}
<http://example.org/agent/session1> {
  res:unrev_team a ex:Team ; rdfs:label "UnreviewedAgentTeam"@en .
}
"#
            .to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    put_assistant(
        &st,
        "org",
        json!({
            "enabled": true,
            "send": "rows",
            "sendByProvider": { "top": "schema" }
        }),
    )
    .await;
    put_memory(
        &st,
        "org",
        json!({ "agentGraphs": ["http://example.org/agent/*"] }),
    )
    .await;
    let server = McpServer::new(st.clone(), McpConfig::default());

    // 1. Caller with no grant on `org` does not see `ask` and gets unknown-dataset.
    let outsider = restricted_principal("outsider", "other", Level::Read, None, None, None, false);
    assert!(!server.allows(&outsider, "ask"));
    assert!(
        server
            .call(
                "ask",
                args_map(json!({ "dataset": "org", "question": "Teams?" })),
                call_for(outsider)
            )
            .await
            .is_err()
    );

    // 2. Caller with Info-only endpoint grant does not see `ask` and gets forbidden on run_now.
    let info_only = restricted_principal(
        "info",
        "org",
        Level::Read,
        None,
        Some(vec![Endpoint::Info]),
        None,
        false,
    );
    assert!(!server.allows(&info_only, "ask"));
    let mut info_call = call_for(info_only);
    let err = server
        .run_now(
            "ask",
            args_map(json!({ "dataset": "org", "question": "Teams?" })),
            &mut info_call,
        )
        .err()
        .expect("info-only caller must be forbidden");
    assert_eq!(err.code.as_ref(), "forbidden");
    assert_eq!(err.status, 403);

    // 3. Graph visibility: caller limited to `<http://example.org/public>` never leaks
    //    `TopSecretCodename` into model prompts or returned rows.
    let pub_reader = restricted_principal(
        "alice",
        "org",
        Level::Read,
        Some(vec!["http://example.org/public".into()]),
        None,
        None,
        false,
    );
    assert!(server.allows(&pub_reader, "ask"));
    let Outcome::Structured(out) = server
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "List the teams" })),
            call_for(pub_reader.clone()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured ask output");
    };
    assert_eq!(out["outcome"], "answered", "{out}");
    let out_str = out.to_string();
    assert!(out_str.contains("Public team"), "{out}");
    assert!(
        !out_str.contains("TopSecretCodename"),
        "leaked secret row: {out}"
    );
    for req in cheap.requests() {
        assert!(
            !req.body.to_string().contains("TopSecretCodename"),
            "leaked secret label to provider: {}",
            req.body
        );
    }

    // 4. `reviewedOnly: true` hides agentGraphs from Ask.
    let Outcome::Structured(rev_out) = server
        .call(
            "ask",
            args_map(
                json!({ "dataset": "org", "question": "List the teams", "reviewedOnly": true }),
            ),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured ask output");
    };
    assert!(
        !rev_out.to_string().contains("UnreviewedAgentTeam"),
        "reviewedOnly must hide agent graphs: {rev_out}"
    );

    // 5. Branch support: create a feature branch, add a team there, ask on it.
    ds.store
        .create_branch("feature", &sparkles::branch::BranchOptions::default())
        .unwrap();
    let feat_ds = st.branch_dataset(&ds, "feature").unwrap();
    feat_ds
        .store
        .load(&[Source::from_bytes(
            br#"@prefix ex: <http://example.org/ontology#> .
@prefix res: <http://example.org/resource/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/public> {
  res:branch_team a ex:Team ; rdfs:label "FeatureBranchTeam"@en .
}
"#
            .to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let Outcome::Structured(branch_out) = server
        .call(
            "ask",
            args_map(
                json!({ "dataset": "org", "branch": "feature", "question": "List the teams" }),
            ),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured ask output");
    };
    assert_eq!(branch_out["branch"], "feature", "{branch_out}");
    assert!(
        branch_out.to_string().contains("FeatureBranchTeam"),
        "{branch_out}"
    );
    // Caller restricted to `main` branch only cannot ask on `feature`.
    let main_only = restricted_principal(
        "mainonly",
        "org",
        Level::Read,
        None,
        None,
        Some(vec!["main".into()]),
        false,
    );
    let branch_err = server
        .call(
            "ask",
            args_map(
                json!({ "dataset": "org", "branch": "feature", "question": "List the teams" }),
            ),
            call_for(main_only),
        )
        .await
        .unwrap()
        .err()
        .expect("expected tool error");
    assert_eq!(branch_err.code.as_ref(), "no-such-branch");

    // 6. Provider/model overrides:
    //    - Refused without serverModels permission.
    let err = server
        .call(
            "ask",
            args_map(json!({
                "dataset": "org",
                "question": "List the teams",
                "provider": "top",
                "model": "m2"
            })),
            call_for(pub_reader),
        )
        .await
        .unwrap()
        .err()
        .expect("expected tool error");
    assert_eq!(err.code.as_ref(), "server-model-not-allowed");
    assert_eq!(err.status, 403);
    assert!(top.requests().is_empty());

    //    - Allowed with serverModels permission, routes draft to `top/m2`, and still honors
    //      `sendByProvider: { "top": "schema" }` (no rows sent to `top`).
    let privileged = restricted_principal("priv", "org", Level::Read, None, None, None, true);
    let bad_model = server
        .call(
            "ask",
            args_map(json!({
                "dataset": "org",
                "question": "List the teams",
                "provider": "top",
                "model": "not-allowed"
            })),
            call_for(privileged.clone()),
        )
        .await
        .unwrap()
        .err()
        .expect("expected tool error");
    assert_eq!(bad_model.code.as_ref(), "bad-argument");

    let Outcome::Structured(over_out) = server
        .call(
            "ask",
            args_map(json!({
                "dataset": "org",
                "question": "List the teams",
                "provider": "top",
                "model": "m2"
            })),
            call_for(privileged),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured ask output");
    };
    assert_eq!(over_out["outcome"], "answered", "{over_out}");
    assert_eq!(over_out["usage"]["answeredBy"]["provider"], "top");
    assert_eq!(over_out["usage"]["answeredBy"]["model"], "m2");
    assert!(
        top.requests().iter().all(|r| !is_summary(r)),
        "top is schema-only and must never receive rows"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_deadlines_cancellation_and_token_budgets() {
    let slow = MockModel::start(|_, _| {
        std::thread::sleep(Duration::from_millis(600));
        (200, mock::openai(&draft_json(TEAMS_QUERY)))
    });
    let fast = scripted(vec![draft_json(TEAMS_QUERY)]);
    let models = make_models(
        &fast.url(),
        &slow.url(),
        json!({ "draft": [{ "provider": "cheap", "model": "m" }] }),
    );
    let st = make_state(Some(models));
    put_assistant(
        &st,
        "org",
        json!({
            "enabled": true,
            "send": "schema",
            "budget": { "perPrincipalPerDay": 10 }
        }),
    )
    .await;
    let server = McpServer::new(st.clone(), McpConfig::default());

    // 1. Pre-cancelled call fails immediately without hitting any model.
    let cancelled_call = call_for(Principal::local());
    cancelled_call
        .cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let err = server
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "Which teams?" })),
            cancelled_call,
        )
        .await
        .unwrap()
        .err()
        .expect("expected tool error");
    assert!(
        matches!(err.code.as_ref(), "cancelled" | "internal"),
        "expected cancelled error, got {err:?}"
    );
    assert!(fast.requests().is_empty());

    // 2. Expired arrival deadline fails with `timeout` before any model call.
    let mut expired_call = call_for(Principal::local());
    expired_call.arrived = Instant::now() - Duration::from_secs(120);
    let err = server
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "Which teams?" })),
            expired_call,
        )
        .await
        .unwrap()
        .err()
        .expect("expected tool error");
    assert_eq!(err.code.as_ref(), "timeout");
    assert!(fast.requests().is_empty());

    // 3. Tight `timeoutSeconds` against a slow provider times out.
    let err = server
        .call(
            "ask",
            args_map(json!({
                "dataset": "org",
                "question": "Which teams?",
                "provider": "top",
                "model": "m",
                "timeoutSeconds": 0.15
            })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .err()
        .expect("expected tool error");
    assert_eq!(err.code.as_ref(), "timeout", "{err:?}");

    // 4. Daily token budget: first ask succeeds and charges > 10 tokens; second ask is
    //    refused with 429 `budget-exceeded` and `resetAt` before any model call.
    let alice = restricted_principal("alice", "org", Level::Read, None, None, None, false);
    let Outcome::Structured(first) = server
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "Which teams?" })),
            call_for(alice.clone()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected first ask to succeed");
    };
    assert_eq!(first["outcome"], "answered", "{first}");
    let reqs_after_first = fast.requests().len();
    let err = server
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "Which teams?" })),
            call_for(alice),
        )
        .await
        .unwrap()
        .err()
        .expect("expected tool error");
    assert_eq!(err.code.as_ref(), "budget-exceeded", "{err:?}");
    assert_eq!(err.status, 429);
    assert_eq!(err.budget, Some("tokens"));
    assert!(
        err.data
            .as_ref()
            .and_then(|d| d.get("resetAt"))
            .and_then(Value::as_str)
            .is_some(),
        "expected resetAt in error data: {err:?}"
    );
    assert_eq!(
        fast.requests().len(),
        reqs_after_first,
        "daily budget refusal must not call a model"
    );
}
