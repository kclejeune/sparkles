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
            views: Vec::new(),
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
    assert_eq!(
        ask_tool["outputSchema"]["properties"]["claims"],
        json!({ "type": "object" })
    );
    assert_eq!(
        ask_tool["outputSchema"]["properties"]["summary"]["properties"]["uncitedSentences"],
        json!({ "type": "array", "items": { "type": "string" } })
    );
    assert_eq!(props["timeoutSeconds"]["default"], json!(30));

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

    let cheap_before_override = cheap.requests().len();
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
        over_out.get("summary").is_none(),
        "schema-only provider override must not produce a summary via another provider: {over_out}"
    );
    assert_eq!(
        cheap.requests().len(),
        cheap_before_override,
        "schema-only provider override must never fall back to cheap for summarization"
    );
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

#[tokio::test(flavor = "multi_thread")]
async fn ask_persisted_allowlist_enforcement() {
    let mock = scripted(vec![draft_json(TEAMS_QUERY)]);
    // Start with a config allowing `obsolete` and `m` so PUT /$/assistant accepts `obsolete`.
    let initial_cfg = json!({
        "providers": {
            "cheap": {
                "kind": "openai",
                "endpoint": mock.url(),
                "requestTimeoutSecs": 5,
                "allowedModels": ["obsolete", "m"]
            }
        },
        "roles": {
            "draft": [{ "provider": "cheap", "model": "m" }]
        }
    });
    let initial_models = Models::new(
        ModelsConfig::parse(&initial_cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    );
    let st = make_state(Some(initial_models));
    put_assistant(
        &st,
        "org",
        json!({
            "enabled": true,
            "send": "schema",
            "roles": {
                "draft": [{ "provider": "cheap", "model": "obsolete" }]
            }
        }),
    )
    .await;

    // Now reopen with a narrower active allowlist that excludes `obsolete`.
    let narrow_cfg = json!({
        "providers": {
            "cheap": {
                "kind": "openai",
                "endpoint": mock.url(),
                "requestTimeoutSecs": 5,
                "allowedModels": ["m"]
            }
        },
        "roles": {
            "draft": [{ "provider": "cheap", "model": "m" }]
        }
    });
    let narrow_models = Arc::new(Models::new(
        ModelsConfig::parse(&narrow_cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    ));
    st.models.store(Some(narrow_models.clone()));

    // 1. Direct chat dispatch refuses disallowed models without hitting the network.
    let direct_err = narrow_models
        .call(
            Some(crate::models::Role::Draft),
            &crate::models::Pair::new("cheap", "obsolete"),
            "sys",
            "usr",
            &crate::models::OutputSchema {
                name: "Test",
                schema: json!({"type": "object"}),
                from_text: |_| None,
                text_instruction: "",
            },
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap_err();
    assert!(
        matches!(direct_err.error, crate::models::StepError::UnknownPair(_)),
        "expected UnknownPair, got {:?}",
        direct_err.error
    );
    assert!(mock.requests().is_empty());

    // 2. Discovery hides `ask` and `list_datasets` omits `ask: true` when no draft pair is allowed.
    let server = McpServer::new(st.clone(), McpConfig::default());
    let mut c = Client::start(server.clone());
    let names: Vec<String> = c
        .tools_list()
        .await
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        !names.contains(&"ask".to_string()),
        "ask must be hidden when persisted draft pairs are disallowed: {names:?}"
    );
    let ds_list = c.structured("list_datasets", json!({})).await;
    assert!(ds_list["datasets"][0].get("ask").is_none(), "{ds_list}");
    assert!(mock.requests().is_empty());

    // 3. With a mixed persisted draft list `[obsolete, m]`, `ask` stays available, skips
    //    `obsolete`, and sends requests only for `m`.
    let wider_models = Arc::new(Models::new(
        ModelsConfig::parse(&initial_cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    ));
    st.models.store(Some(wider_models));
    put_assistant(
        &st,
        "org",
        json!({
            "enabled": true,
            "send": "schema",
            "roles": {
                "draft": [
                    { "provider": "cheap", "model": "obsolete" },
                    { "provider": "cheap", "model": "m" }
                ]
            }
        }),
    )
    .await;
    st.models.store(Some(narrow_models));

    let out = c
        .structured("ask", json!({ "dataset": "org", "question": "List teams" }))
        .await;
    assert_eq!(out["outcome"], "answered", "{out}");
    assert_eq!(out["usage"]["answeredBy"]["model"], "m");
    assert!(
        mock.requests().iter().all(|r| r.body["model"] == "m"),
        "obsolete model must never receive a request: {:?}",
        mock.requests()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_shares_snapshot_pins_across_calls() {
    let m = scripted(vec![draft_json(TEAMS_QUERY)]);
    let models = make_models(
        &m.url(),
        &m.url(),
        json!({ "draft": [{ "provider": "cheap", "model": "m" }] }),
    );
    let st = make_state(Some(models));
    put_assistant(&st, "org", json!({ "enabled": true, "send": "schema" })).await;
    let ds = st.get("org").unwrap();
    let server = McpServer::new(st.clone(), McpConfig::default());

    // Direction 1: `sparql_query` pins commit 1 in outer server, head moves to commit 2,
    // and `ask(atCommit: 1)` reads the pinned commit 1 snapshot.
    let q1_doc = server
        .call(
            "sparql_query",
            args_map(json!({ "dataset": "org", "query": TEAMS_QUERY, "format": "json" })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .unwrap()
        .into_json_value();
    let commit1 = q1_doc["commit"].as_u64().unwrap();
    assert_eq!(q1_doc["returned"], 2);
    assert!(server.shared.pins.holds(&ds, commit1));

    // Advance head to commit 2 by inserting a third team.
    ds.store
        .load(&[Source::from_bytes(
            b"@prefix ex: <http://example.org/ontology#> .\n@prefix res: <http://example.org/resource/> .\nres:infra a ex:Team .\n".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let commit2 = ds.store.snapshot().commit;
    assert!(commit2 > commit1);

    let Outcome::Structured(ask_c1) = server
        .call(
            "ask",
            args_map(json!({
                "dataset": "org",
                "question": "List the teams",
                "atCommit": commit1
            })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured ask output at commit 1");
    };
    assert_eq!(ask_c1["outcome"], "answered", "{ask_c1}");
    assert_eq!(ask_c1["result"]["commit"], commit1);
    assert_eq!(ask_c1["result"]["results"]["returned"], 2);

    // Direction 2: `ask` reads commit 2 (pinning it into the shared outer `Pins`), head
    // advances to commit 3, and `sparql_query(atCommit: commit2)` still reads commit 2.
    let Outcome::Structured(ask_c2) = server
        .call(
            "ask",
            args_map(json!({
                "dataset": "org",
                "question": "List the teams"
            })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured ask output at commit 2");
    };
    assert_eq!(ask_c2["outcome"], "answered", "{ask_c2}");
    assert_eq!(ask_c2["result"]["commit"], commit2);
    assert_eq!(ask_c2["result"]["results"]["returned"], 3);
    assert!(server.shared.pins.holds(&ds, commit2));

    ds.store
        .load(&[Source::from_bytes(
            b"@prefix ex: <http://example.org/ontology#> .\n@prefix res: <http://example.org/resource/> .\nres:security a ex:Team .\n".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let commit3 = ds.store.snapshot().commit;
    assert!(commit3 > commit2);

    let q2_doc = server
        .call(
            "sparql_query",
            args_map(json!({
                "dataset": "org",
                "query": TEAMS_QUERY,
                "format": "json",
                "atCommit": commit2
            })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .unwrap()
        .into_json_value();
    assert_eq!(q2_doc["commit"], commit2);
    assert_eq!(q2_doc["returned"], 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_read_only_mode_does_not_write_history() {
    let dir = tempfile::tempdir().unwrap();
    let db_dir = dir.path().join("org");
    std::fs::create_dir_all(&db_dir).unwrap();
    let cheap = scripted(vec![draft_json(TEAMS_QUERY)]);
    let top = scripted(vec![draft_json(TEAMS_QUERY)]);
    let models = make_models(
        &cheap.url(),
        &top.url(),
        json!({
            "draft": [
                { "provider": "cheap", "model": "m" },
                { "provider": "top", "model": "m" }
            ]
        }),
    );

    // Populate persistent dataset while writable, then write assistant.json and sentinel asks.json.
    {
        let mut st_rw = AppState::standalone(Default::default(), Duration::from_secs(30));
        st_rw.models = arc_swap::ArcSwapOption::from_pointee(make_models(
            &cheap.url(),
            &top.url(),
            json!({
                "draft": [
                    { "provider": "cheap", "model": "m" },
                    { "provider": "top", "model": "m" }
                ]
            }),
        ));
        let st_rw = Arc::new(st_rw);
        let ds = st_rw
            .attach("org", DbType::Persistent, Some(&db_dir))
            .unwrap();
        ds.store
            .load(&[Source::from_bytes(
                ORG.as_bytes().to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        put_assistant(
            &st_rw,
            "org",
            json!({
                "enabled": true,
                "send": "schema",
                "historyDays": 30
            }),
        )
        .await;
    }

    let sentinel = serde_json::to_string_pretty(&json!({
        "format": 2,
        "asks": [{
            "id": "existing-1",
            "principal": "local",
            "at": "2026-11-26T00:00:00Z",
            "question": "Existing?",
            "result": "answered",
            "outcome": "none",
            "routing": {}
        }]
    }))
    .unwrap();
    let asks_path = db_dir.join("asks.json");
    std::fs::write(&asks_path, &sentinel).unwrap();

    // Reopen in read-only mode (as `sparkles mcp --loc` without `--allow-update` does).
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    st.read_only = true;
    st.models = arc_swap::ArcSwapOption::from_pointee(models);
    let st = Arc::new(st);
    st.attach("org", DbType::Persistent, Some(&db_dir)).unwrap();
    let server = McpServer::new(st.clone(), McpConfig::default());

    let Outcome::Structured(first) = server
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "List teams" })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured ask output");
    };
    assert_eq!(first["outcome"], "answered", "{first}");
    let ask_id = first["usage"]["askId"].as_str().unwrap().to_string();

    let Outcome::Structured(second) = server
        .call(
            "ask",
            args_map(json!({
                "dataset": "org",
                "question": "List teams",
                "tryHarder": ask_id
            })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured tryHarder output");
    };
    assert_eq!(second["outcome"], "answered", "{second}");
    assert_eq!(second["usage"]["answeredBy"]["provider"], "top");

    let after = std::fs::read_to_string(&asks_path).unwrap();
    assert_eq!(
        after, sentinel,
        "read-only MCP calls must never mutate asks.json on disk"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_history_visibility_checks_info_view_and_withholds_query() {
    let m = scripted(vec![draft_json(TEAMS_QUERY)]);
    let models = make_models(
        &m.url(),
        &m.url(),
        json!({ "draft": [{ "provider": "cheap", "model": "m" }] }),
    );
    let st = make_state(Some(models));
    let ds = st.get("org").unwrap();
    ds.store
        .load(&[Source::from_bytes(
            br#"@prefix ex: <http://example.org/ontology#> .
GRAPH <http://example.org/g/public> { <http://example.org/r/p> a ex:PublicThing . }
GRAPH <http://example.org/g/secret> { <http://example.org/r/s> a ex:SecretThing . }
"#
            .to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    put_assistant(
        &st,
        "org",
        json!({ "enabled": true, "send": "schema", "historyDays": 30 }),
    )
    .await;

    // Principal has Query on `[default, public]` and Info on `[default, public, secret]`.
    let split_access = Access::of(Grants {
        restricted: vec![
            Restricted {
                dataset: "org".into(),
                level: Level::Read,
                graphs: Some(vec!["default".into(), "http://example.org/g/public".into()]),
                endpoints: Some(vec![Endpoint::Query]),
                lifts: Vec::new(),
                branches: None,
                server_models: false,
                views: Vec::new(),
            },
            Restricted {
                dataset: "org".into(),
                level: Level::Read,
                graphs: Some(vec![
                    "default".into(),
                    "http://example.org/g/public".into(),
                    "http://example.org/g/secret".into(),
                ]),
                endpoints: Some(vec![Endpoint::Info]),
                lifts: Vec::new(),
                branches: None,
                server_models: false,
                views: Vec::new(),
            },
        ],
        ..Default::default()
    });
    let asker = Principal::new(Kind::User, "alice", Scheme::Basic, split_access);
    let server = McpServer::new(st.clone(), McpConfig::default());
    let Outcome::Structured(out) = server
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "List teams" })),
            call_for(asker.clone()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured ask output");
    };
    assert_eq!(out["outcome"], "answered", "{out}");

    // Helper to query GET /$/asks/org as a given principal.
    let read_history = |p: Principal| {
        let app = crate::assistant::routes()
            .layer(axum::Extension(p))
            .with_state(st.clone());
        async move {
            let req = Request::builder()
                .method("GET")
                .uri("/$/asks/org")
                .body(Body::empty())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice::<Value>(&bytes).unwrap()
        }
    };

    // With the original split grants, both answer and query are visible.
    let hist_same = read_history(asker).await;
    let rec_same = &hist_same["asks"][0];
    assert!(rec_same.get("answer").is_some(), "{rec_same}");
    assert!(rec_same.get("query").is_some(), "{rec_same}");
    assert!(rec_same.get("answerWithheld").is_none(), "{rec_same}");

    // Narrow Info to drop `secret` while keeping Query identical -> both answer and query withheld.
    let narrowed_info = Access::of(Grants {
        restricted: vec![
            Restricted {
                dataset: "org".into(),
                level: Level::Read,
                graphs: Some(vec!["default".into(), "http://example.org/g/public".into()]),
                endpoints: Some(vec![Endpoint::Query]),
                lifts: Vec::new(),
                branches: None,
                server_models: false,
                views: Vec::new(),
            },
            Restricted {
                dataset: "org".into(),
                level: Level::Read,
                graphs: Some(vec!["default".into(), "http://example.org/g/public".into()]),
                endpoints: Some(vec![Endpoint::Info]),
                lifts: Vec::new(),
                branches: None,
                server_models: false,
                views: Vec::new(),
            },
        ],
        ..Default::default()
    });
    let hist_narrow = read_history(Principal::new(
        Kind::User,
        "alice",
        Scheme::Basic,
        narrowed_info,
    ))
    .await;
    let rec_narrow = &hist_narrow["asks"][0];
    assert_eq!(rec_narrow["answerWithheld"], true, "{rec_narrow}");
    assert!(
        rec_narrow.get("answer").is_none(),
        "answer must be withheld after losing Info access: {rec_narrow}"
    );
    assert!(
        rec_narrow.get("query").is_none(),
        "generated query must also be withheld after losing Info access: {rec_narrow}"
    );

    // Revoke Info completely while keeping Query -> also withheld.
    let no_info = restricted_principal(
        "alice",
        "org",
        Level::Read,
        Some(vec!["default".into(), "http://example.org/g/public".into()]),
        Some(vec![Endpoint::Query]),
        None,
        false,
    );
    let hist_no_info = read_history(no_info.clone()).await;
    let rec_no_info = &hist_no_info["asks"][0];
    assert_eq!(rec_no_info["answerWithheld"], true, "{rec_no_info}");
    assert!(rec_no_info.get("answer").is_none(), "{rec_no_info}");
    assert!(rec_no_info.get("query").is_none(), "{rec_no_info}");

    // Format 3 is written on disk.
    let raw_after = crate::assist::read_file(&st, &ds, crate::assistant::ASKS_FILE)
        .unwrap()
        .unwrap();
    assert_eq!(raw_after["format"], 3, "{raw_after}");

    // A fresh format-3 ask by a Query-only asker (no Info grant) records explicit absence
    // of Info access (`infoAccess: "none"`), so the same Query-only reader can read it.
    let m2 = scripted(vec![draft_json(TEAMS_QUERY)]);
    let st_qonly = make_state(Some(make_models(
        &m2.url(),
        &m2.url(),
        json!({ "draft": [{ "provider": "cheap", "model": "m" }] }),
    )));
    let ds_qonly = st_qonly.get("org").unwrap();
    ds_qonly
        .store
        .load(&[Source::from_bytes(
            br#"@prefix ex: <http://example.org/ontology#> .
GRAPH <http://example.org/g/public> { <http://example.org/r/p> a ex:PublicThing . }
GRAPH <http://example.org/g/secret> { <http://example.org/r/s> a ex:SecretThing . }
"#
            .to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    put_assistant(
        &st_qonly,
        "org",
        json!({ "enabled": true, "send": "schema", "historyDays": 30 }),
    )
    .await;
    let srv_qonly = McpServer::new(st_qonly.clone(), McpConfig::default());
    let Outcome::Structured(qonly_out) = srv_qonly
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "List teams" })),
            call_for(no_info.clone()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured ask output");
    };
    assert_eq!(qonly_out["outcome"], "answered", "{qonly_out}");
    let app_qonly = crate::assistant::routes()
        .layer(axum::Extension(no_info.clone()))
        .with_state(st_qonly.clone());
    let res_qonly = app_qonly
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/$/asks/org")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res_qonly.status(), StatusCode::OK);
    let hist_qonly: Value = serde_json::from_slice(
        &axum::body::to_bytes(res_qonly.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        hist_qonly["asks"][0].get("answer").is_some(),
        "{hist_qonly}"
    );
    assert!(hist_qonly["asks"][0].get("query").is_some(), "{hist_qonly}");
    assert!(
        hist_qonly["asks"][0].get("answerWithheld").is_none(),
        "{hist_qonly}"
    );

    // Pre-fix format-2 record (Query footprint only, no `infoAccess`) must be withheld
    // under narrowed Info grants BOTH before and after a format-3 history rewrite, while
    // remaining visible to an unrestricted Info reader.
    let snap = ds.store.snapshot();
    let legacy_query_access = crate::assistant::answer_access(
        Some(&snap),
        no_info.view("org", Endpoint::Query).as_ref(),
        &no_info.caller(),
        "org",
        None,
        &no_info.id(),
    );
    let legacy_file = json!({
        "format": 2,
        "asks": [{
            "id": "legacy2",
            "principal": no_info.id(),
            "at": chrono::Utc::now().to_rfc3339(),
            "question": "Legacy question",
            "query": TEAMS_QUERY,
            "commit": snap.commit,
            "result": "answered",
            "outcome": "none",
            "routing": { "steps": [] },
            "thread": "legacy2",
            "answer": { "explanation": "Derived from secret Info schema" },
            "access": legacy_query_access
        }]
    });
    crate::assist::write_file(&st, &ds, crate::assistant::ASKS_FILE, &legacy_file).unwrap();

    let narrowed_info_again = Access::of(Grants {
        restricted: vec![
            Restricted {
                dataset: "org".into(),
                level: Level::Read,
                graphs: Some(vec!["default".into(), "http://example.org/g/public".into()]),
                endpoints: Some(vec![Endpoint::Query]),
                lifts: Vec::new(),
                branches: None,
                server_models: false,
                views: Vec::new(),
            },
            Restricted {
                dataset: "org".into(),
                level: Level::Read,
                graphs: Some(vec!["default".into(), "http://example.org/g/public".into()]),
                endpoints: Some(vec![Endpoint::Info]),
                lifts: Vec::new(),
                branches: None,
                server_models: false,
                views: Vec::new(),
            },
        ],
        ..Default::default()
    });
    let narrowed_reader = Principal::new(Kind::User, "alice", Scheme::Basic, narrowed_info_again);
    let hist_legacy_narrow = read_history(narrowed_reader.clone()).await;
    let rec_legacy = &hist_legacy_narrow["asks"][0];
    assert_eq!(rec_legacy["id"], "legacy2");
    assert_eq!(rec_legacy["answerWithheld"], true, "{rec_legacy}");
    assert!(rec_legacy.get("answer").is_none(), "{rec_legacy}");
    assert!(rec_legacy.get("query").is_none(), "{rec_legacy}");

    // Rewrite the history file from format 2 to format 3 (via feedback on legacy2).
    assert!(crate::assistant::feedback(
        &st,
        &ds,
        &no_info.id(),
        "legacy2",
        "accepted",
        None
    ));
    let rewritten = crate::assist::read_file(&st, &ds, crate::assistant::ASKS_FILE)
        .unwrap()
        .unwrap();
    assert_eq!(rewritten["format"], 3, "{rewritten}");

    // Even after the format-3 rewrite, the legacy record still has unknown Info access
    // and remains withheld from the narrowed Info reader.
    let hist_after_rewrite = read_history(narrowed_reader).await;
    let rec_after_rewrite = &hist_after_rewrite["asks"][0];
    assert_eq!(rec_after_rewrite["id"], "legacy2");
    assert_eq!(
        rec_after_rewrite["answerWithheld"], true,
        "{rec_after_rewrite}"
    );
    assert!(
        rec_after_rewrite.get("answer").is_none(),
        "{rec_after_rewrite}"
    );
    assert!(
        rec_after_rewrite.get("query").is_none(),
        "{rec_after_rewrite}"
    );

    // An unrestricted Info reader (with covering Query access) can still see the legacy record.
    let unrestricted_info_reader = Principal::new(
        Kind::User,
        "alice",
        Scheme::Basic,
        Access::of(Grants {
            restricted: vec![
                Restricted {
                    dataset: "org".into(),
                    level: Level::Read,
                    graphs: Some(vec!["default".into(), "http://example.org/g/public".into()]),
                    endpoints: Some(vec![Endpoint::Query]),
                    lifts: Vec::new(),
                    branches: None,
                    server_models: false,
                    views: Vec::new(),
                },
                Restricted {
                    dataset: "org".into(),
                    level: Level::Read,
                    graphs: None,
                    endpoints: Some(vec![Endpoint::Info]),
                    lifts: Vec::new(),
                    branches: None,
                    server_models: false,
                    views: Vec::new(),
                },
            ],
            ..Default::default()
        }),
    );
    let hist_unrestricted = read_history(unrestricted_info_reader).await;
    let rec_unrestricted = &hist_unrestricted["asks"][0];
    assert_eq!(rec_unrestricted["id"], "legacy2");
    assert!(
        rec_unrestricted.get("answer").is_some(),
        "{rec_unrestricted}"
    );
    assert!(
        rec_unrestricted.get("query").is_some(),
        "{rec_unrestricted}"
    );
    assert!(
        rec_unrestricted.get("answerWithheld").is_none(),
        "{rec_unrestricted}"
    );

    // Format 4 is cleanly refused with 409 unsupported-format and never overwritten.
    let future_file = json!({ "format": 4, "asks": [{ "id": "future" }] });
    crate::assist::write_file(&st, &ds, crate::assistant::ASKS_FILE, &future_file).unwrap();
    let app_future = crate::assistant::routes()
        .layer(axum::Extension(Principal::local()))
        .with_state(st.clone());
    let res_future = app_future
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/$/asks/org")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res_future.status(), StatusCode::CONFLICT);
    let body_future: Value = serde_json::from_slice(
        &axum::body::to_bytes(res_future.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body_future["code"], "unsupported-format", "{body_future}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_global_preview_and_schema_only_override() {
    use crate::graphrag::build::Mode;
    use crate::graphrag::build::tests::{
        Fixture, answer as graphrag_answer, data as graphrag_data, settings as graphrag_settings,
    };

    let cheap_mock = MockModel::start(|r, _| {
        let sys = r.body["messages"][0]["content"].as_str().unwrap_or("");
        let body = r.body.to_string();
        let text = if sys.contains("community reports about one dataset") {
            json!({ "points": [
                { "text": "People cluster in two groups.", "score": 80,
                  "reports": ["R1"], "findings": ["F1.1"] }
            ] })
            .to_string()
        } else if sys.contains("numbered points") {
            json!({ "text": "There are two groups [1].", "citations": [1] }).to_string()
        } else if sys.contains("translate a person's question") {
            let mut d = json!({
                "query": "SELECT ?s WHERE { ?s ?p ?o } LIMIT 3",
                "explanation": "Lists subjects.",
                "assumptions": [],
                "clarify": { "question": "", "choices": [] },
                "graph": { "subject": "", "predicate": "", "object": "" }
            });
            if body.contains("ScopedDraft") {
                d["scope"] = "global".into();
            }
            d.to_string()
        } else if sys.contains("from the rows of a query result") {
            json!({ "text": "Three subjects [1].", "citations": [1] }).to_string()
        } else {
            graphrag_answer(r)
        };
        (200, mock::openai(&text))
    });
    let top_mock = scripted(vec![draft_json("SELECT ?s WHERE { ?s ?p ?o } LIMIT 3")]);

    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    let cfg = json!({
        "providers": {
            "cheap": {
                "kind": "openai",
                "endpoint": cheap_mock.url(),
                "requestTimeoutSecs": 5,
                "allowedModels": ["m"],
                "structuredOutput": "json-schema"
            },
            "top": {
                "kind": "openai",
                "endpoint": top_mock.url(),
                "requestTimeoutSecs": 5,
                "allowedModels": ["m"],
                "structuredOutput": "json-schema"
            }
        },
        "roles": {
            "community": [{ "provider": "cheap", "model": "m" }],
            "draft": [{ "provider": "cheap", "model": "m" }],
            "summarize": [{ "provider": "cheap", "model": "m" }]
        }
    });
    st.models = arc_swap::ArcSwapOption::from_pointee(Models::new(
        ModelsConfig::parse(&cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    ));
    let st = Arc::new(st);
    let ds = st.attach("org", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            graphrag_data(3).into_bytes(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let app = crate::http::router(st.clone());
    let f = Fixture {
        st: st.clone(),
        app,
        mock: cheap_mock,
    };
    f.allow_rows().await;
    let f = tokio::task::spawn_blocking(move || {
        f.run(graphrag_settings(json!({})), Mode::Build).unwrap();
        f
    })
    .await
    .unwrap();

    // Configure dataset with send: rows and sendByProvider: { top: schema }.
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

    let server = McpServer::new(st.clone(), McpConfig::default());
    let reqs_after_build = f.mock.requests().len();

    // 1. Explicit `mode: "global"` with `run: false` is refused before any model call,
    //    both with and without a caller-supplied `query`.
    for extra in [
        json!({}),
        json!({ "query": "SELECT ?s WHERE { ?s ?p ?o } LIMIT 3" }),
    ] {
        let mut req = json!({
            "dataset": "org",
            "question": "What are the main themes?",
            "mode": "global",
            "run": false
        });
        if let Some(q) = extra.get("query") {
            req["query"] = q.clone();
        }
        let err = server
            .call("ask", args_map(req), call_for(Principal::local()))
            .await
            .unwrap()
            .err()
            .expect("global preview should fail");
        assert_eq!(err.code.as_ref(), "bad-argument", "{err:?}");
        assert_eq!(
            f.mock.requests().len(),
            reqs_after_build,
            "global preview must make zero model calls"
        );
    }

    // 2. `mode: "auto"` with `run: false` stays in local check mode and never calls global map/reduce.
    let Outcome::Structured(auto_preview) = server
        .call(
            "ask",
            args_map(json!({
                "dataset": "org",
                "question": "What are the main themes?",
                "mode": "auto",
                "run": false
            })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured preview");
    };
    assert_eq!(auto_preview["outcome"], "checked", "{auto_preview}");
    assert_eq!(auto_preview["mode"], "local", "{auto_preview}");
    let new_reqs = &f.mock.requests()[reqs_after_build..];
    assert_eq!(
        new_reqs.len(),
        1,
        "only a single local draft call should run"
    );
    assert!(
        !new_reqs[0].body.to_string().contains("ScopedDraft"),
        "preview must not ask for global scope classification"
    );

    // 3. `mode: "global"` with a schema-only `provider`/`model` override must NOT fall back
    //    to `cheap` for community report summarization.
    let cheap_before_global_override = f.mock.requests().len();
    let Outcome::Structured(global_over) = server
        .call(
            "ask",
            args_map(json!({
                "dataset": "org",
                "question": "What are the main themes?",
                "mode": "global",
                "provider": "top",
                "model": "m"
            })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured output");
    };
    assert_eq!(global_over["mode"], "local", "{global_over}");
    assert_eq!(global_over["usage"]["answeredBy"]["provider"], "top");
    assert!(global_over.get("summary").is_none(), "{global_over}");
    assert_eq!(
        f.mock.requests().len(),
        cheap_before_global_override,
        "schema-only provider override in global mode must not send reports to cheap"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_charges_usage_on_refusal_invalid_and_runtime_max_tokens() {
    // 1. Provider refusal with reported usage crossing the daily cap charges the budget.
    let refusal_mock = MockModel::start(|_, _| {
        (
            200,
            json!({
                "choices": [{
                    "message": { "refusal": "cannot comply" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 45, "completion_tokens": 15 }
            }),
        )
    });
    let invalid_mock = MockModel::start(|_, _| {
        (
            200,
            json!({
                "choices": [],
                "usage": { "prompt_tokens": 40, "completion_tokens": 10 }
            }),
        )
    });
    let models = make_models(
        &refusal_mock.url(),
        &invalid_mock.url(),
        json!({ "draft": [{ "provider": "cheap", "model": "m" }] }),
    );
    let st = make_state(Some(models));
    put_assistant(
        &st,
        "org",
        json!({
            "enabled": true,
            "send": "schema",
            "budget": { "perPrincipalPerDay": 50, "perDatasetPerDay": 100 }
        }),
    )
    .await;
    let server = McpServer::new(st.clone(), McpConfig::default());

    let alice = restricted_principal("alice", "org", Level::Read, None, None, None, false);
    let err1 = server
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "List teams" })),
            call_for(alice.clone()),
        )
        .await
        .unwrap()
        .err()
        .expect("refusal should fail");
    assert_eq!(err1.code.as_ref(), "provider-unavailable", "{err1:?}");
    let usage1 = err1.data.as_ref().unwrap()["usage"].clone();
    assert_eq!(usage1["inputTokens"], 45, "{usage1}");
    assert_eq!(usage1["outputTokens"], 15, "{usage1}");
    assert_eq!(st.asks.tokens_today("org", Some(&alice.id())), 60);

    // Next call by alice is refused with 429 budget-exceeded before hitting the mock.
    let reqs_before = refusal_mock.requests().len();
    let err2 = server
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "List teams" })),
            call_for(alice),
        )
        .await
        .unwrap()
        .err()
        .expect("second call should hit daily cap");
    assert_eq!(err2.code.as_ref(), "budget-exceeded", "{err2:?}");
    assert_eq!(err2.status, 429);
    assert_eq!(refusal_mock.requests().len(), reqs_before);

    // 2. Malformed/empty-choices response also charges dataset daily budget; bob hits
    //    `perDatasetPerDay` (60 + 50 = 110 >= 100) so carol is refused with 0 requests.
    let bob = restricted_principal("bob", "org", Level::Read, None, None, None, true);
    let _ = server
        .call(
            "ask",
            args_map(json!({
                "dataset": "org",
                "question": "List teams",
                "provider": "top",
                "model": "m"
            })),
            call_for(bob),
        )
        .await
        .unwrap()
        .err()
        .expect("empty choices should fail");
    assert_eq!(st.asks.tokens_today("org", None), 110);

    let carol = restricted_principal("carol", "org", Level::Read, None, None, None, false);
    let reqs_before_carol = refusal_mock.requests().len();
    let err_ds = server
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "List teams" })),
            call_for(carol),
        )
        .await
        .unwrap()
        .err()
        .expect("dataset daily budget should refuse carol");
    assert_eq!(err_ds.code.as_ref(), "budget-exceeded", "{err_ds:?}");
    assert_eq!(refusal_mock.requests().len(), reqs_before_carol);

    // 3. Runtime `maxTokens` cap stops a multi-step ask after the first step spends >= maxTokens.
    let bad_query_mock = MockModel::start(|_, _| {
        (
            200,
            json!({
                "choices": [{
                    "message": { "content": draft_json("SELECT ?x WHERE { ?x a ex:NoSuchClass }") },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 30, "completion_tokens": 25 }
            }),
        )
    });
    let st2 = make_state(Some(make_models(
        &bad_query_mock.url(),
        &bad_query_mock.url(),
        json!({ "draft": [{ "provider": "cheap", "model": "m" }] }),
    )));
    put_assistant(&st2, "org", json!({ "enabled": true, "send": "schema" })).await;
    let server2 = McpServer::new(st2, McpConfig::default());
    let err_cap = server2
        .call(
            "ask",
            args_map(json!({
                "dataset": "org",
                "question": "List teams",
                "maxTokens": 50
            })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .err()
        .expect("runtime maxTokens cap should stop before repair step");
    assert_eq!(err_cap.code.as_ref(), "budget-exceeded", "{err_cap:?}");
    assert_eq!(
        bad_query_mock.requests().len(),
        1,
        "repair step must not run once draft crossed maxTokens"
    );

    // 4. Two-pair draft role: a refusal on pair 1 that exhausts `maxTokens` must return
    //    `budget-exceeded` with first-step usage retained and zero requests to pair 2.
    let refusal_pair1 = MockModel::start(|_, _| {
        (
            200,
            json!({
                "choices": [{
                    "message": { "refusal": "cannot comply" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 45, "completion_tokens": 15 }
            }),
        )
    });
    let fallback_pair2 = scripted(vec![draft_json(TEAMS_QUERY)]);
    let st3 = make_state(Some(make_models(
        &refusal_pair1.url(),
        &fallback_pair2.url(),
        json!({
            "draft": [
                { "provider": "cheap", "model": "m" },
                { "provider": "top", "model": "m" }
            ]
        }),
    )));
    put_assistant(&st3, "org", json!({ "enabled": true, "send": "schema" })).await;
    let server3 = McpServer::new(st3, McpConfig::default());
    let err_fallback = server3
        .call(
            "ask",
            args_map(json!({
                "dataset": "org",
                "question": "List teams",
                "maxTokens": 50
            })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .err()
        .expect("refusal exhausting maxTokens must stop before fallback pair");
    assert_eq!(
        err_fallback.code.as_ref(),
        "budget-exceeded",
        "{err_fallback:?}"
    );
    let fallback_usage = err_fallback.data.as_ref().unwrap()["usage"].clone();
    assert_eq!(fallback_usage["inputTokens"], 45, "{fallback_usage}");
    assert_eq!(fallback_usage["outputTokens"], 15, "{fallback_usage}");
    assert_eq!(refusal_pair1.requests().len(), 1);
    assert_eq!(
        fallback_pair2.requests().len(),
        0,
        "second provider must receive zero requests after refusal exhausted maxTokens"
    );
}

struct GateGuard(Arc<(parking_lot::Mutex<bool>, parking_lot::Condvar)>);

impl GateGuard {
    fn open(&self) {
        let mut open = self.0.0.lock();
        *open = true;
        self.0.1.notify_all();
    }
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        self.open();
    }
}

fn wait_gate(gate: &(parking_lot::Mutex<bool>, parking_lot::Condvar), timeout: Duration) {
    let mut open = gate.0.lock();
    let deadline = Instant::now() + timeout;
    while !*open {
        if gate.1.wait_until(&mut open, deadline).timed_out() {
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_cancellation_and_slot_rpm_deadlines() {
    // 1. Mid-flight cancellation synchronized on mock arrival returns promptly while
    //    retaining the provider concurrency slot until the in-flight HTTP call finishes,
    //    and records late reported usage for both provider and Ask budgets.
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel::<()>();
    let gate = Arc::new((parking_lot::Mutex::new(false), parking_lot::Condvar::new()));
    let gate_guard = GateGuard(gate.clone());
    let slow = {
        let gate = gate.clone();
        let arrived_tx = parking_lot::Mutex::new(Some(arrived_tx));
        MockModel::start(move |_, _| {
            if let Some(tx) = arrived_tx.lock().take() {
                let _ = tx.send(());
                wait_gate(&gate, Duration::from_secs(10));
            }
            (
                200,
                json!({
                    "choices": [{
                        "message": { "content": draft_json(TEAMS_QUERY) },
                        "finish_reason": "stop"
                    }],
                    "usage": { "prompt_tokens": 45, "completion_tokens": 15 }
                }),
            )
        })
    };
    let cancel_cfg = json!({
        "providers": {
            "cheap": {
                "kind": "openai",
                "endpoint": slow.url(),
                "concurrency": 1,
                "requestTimeoutSecs": 5,
                "budget": { "tokensPerDay": 500 },
                "allowedModels": ["m"]
            }
        },
        "roles": { "draft": [{ "provider": "cheap", "model": "m" }] }
    });
    let cancel_models = Models::new(
        ModelsConfig::parse(&cancel_cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    );
    let st_cancel = make_state(Some(cancel_models));
    put_assistant(
        &st_cancel,
        "org",
        json!({ "enabled": true, "send": "schema" }),
    )
    .await;
    let srv_cancel = McpServer::new(st_cancel.clone(), McpConfig::default());
    let alice = restricted_principal("alice", "org", Level::Read, None, None, None, false);
    let call = call_for(alice.clone());
    let flag = call.cancel.clone();
    let first_handle = {
        let srv = srv_cancel.clone();
        tokio::spawn(async move {
            srv.call(
                "ask",
                args_map(
                    json!({ "dataset": "org", "question": "List teams", "timeoutSeconds": 5 }),
                ),
                call,
            )
            .await
            .unwrap()
        })
    };
    // Synchronize cancellation with the mock's actual receipt of the first request.
    arrived_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("first request should arrive at mock provider");
    let t0 = Instant::now();
    flag.store(true, std::sync::atomic::Ordering::Relaxed);
    let err = first_handle
        .await
        .unwrap()
        .err()
        .expect("mid-flight cancel should fail");
    assert_eq!(err.code.as_ref(), "cancelled", "{err:?}");
    assert!(
        t0.elapsed() < Duration::from_millis(500),
        "cancellation should return promptly while the in-flight HTTP worker finishes (took {:?})",
        t0.elapsed()
    );

    // While the cancelled HTTP request is still in flight, concurrency: 1 must block a
    // second Ask so it times out without reaching the provider.
    let blocked_err = srv_cancel
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "List teams", "timeoutSeconds": 0.15 })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .err()
        .expect("second ask must be blocked while cancelled HTTP request is in flight");
    assert_eq!(blocked_err.code.as_ref(), "timeout", "{blocked_err:?}");
    assert_eq!(
        slow.requests().len(),
        1,
        "second ask must not bypass concurrency: 1 while cancelled request is in flight"
    );

    // Release the in-flight HTTP response and verify late usage settlement + slot release.
    gate_guard.open();
    let settle_wait = Instant::now();
    while (st_cancel.asks.tokens_today("org", Some(&alice.id())) < 60
        || st_cancel.models().unwrap().describe()["providers"][0]["budget"]["usedToday"] != 60)
        && settle_wait.elapsed() < Duration::from_secs(3)
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        st_cancel.asks.tokens_today("org", Some(&alice.id())),
        60,
        "late usage from cancelled HTTP response must be recorded for principal daily budget"
    );
    assert_eq!(
        st_cancel.asks.tokens_today("org", None),
        60,
        "late usage from cancelled HTTP response must be recorded for dataset daily budget"
    );
    let desc = st_cancel.models().unwrap().describe();
    assert_eq!(
        desc["providers"][0]["budget"]["usedToday"], 60,
        "late usage from cancelled HTTP response must be recorded for provider daily budget: {desc}"
    );
    let Outcome::Structured(after_release) = srv_cancel
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "List teams", "timeoutSeconds": 5 })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured ask output after slot release");
    };
    assert_eq!(after_release["outcome"], "answered", "{after_release}");
    assert_eq!(slow.requests().len(), 2);

    // 1b. Queued Ask waiting on concurrency: 1 must be refused after late settlement
    //     exhausts the provider's tokensPerDay cap.
    let (cap_tx, cap_rx) = std::sync::mpsc::channel::<()>();
    let cap_gate = Arc::new((parking_lot::Mutex::new(false), parking_lot::Condvar::new()));
    let cap_guard = GateGuard(cap_gate.clone());
    let cap_mock = {
        let gate = cap_gate.clone();
        let cap_tx = parking_lot::Mutex::new(Some(cap_tx));
        MockModel::start(move |_, _| {
            if let Some(tx) = cap_tx.lock().take() {
                let _ = tx.send(());
                wait_gate(&gate, Duration::from_secs(10));
            }
            (
                200,
                json!({
                    "choices": [{
                        "message": { "content": draft_json(TEAMS_QUERY) },
                        "finish_reason": "stop"
                    }],
                    "usage": { "prompt_tokens": 45, "completion_tokens": 15 }
                }),
            )
        })
    };
    let cap_cfg = json!({
        "providers": {
            "cheap": {
                "kind": "openai",
                "endpoint": cap_mock.url(),
                "concurrency": 1,
                "requestTimeoutSecs": 5,
                "budget": { "tokensPerDay": 50 },
                "allowedModels": ["m"]
            }
        },
        "roles": { "draft": [{ "provider": "cheap", "model": "m" }] }
    });
    let st_cap = make_state(Some(Models::new(
        ModelsConfig::parse(&cap_cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    )));
    put_assistant(&st_cap, "org", json!({ "enabled": true, "send": "schema" })).await;
    let srv_cap = McpServer::new(st_cap.clone(), McpConfig::default());
    let cap_call = call_for(alice.clone());
    let cap_flag = cap_call.cancel.clone();
    let cap_first = {
        let srv = srv_cap.clone();
        tokio::spawn(async move {
            srv.call(
                "ask",
                args_map(
                    json!({ "dataset": "org", "question": "List teams", "timeoutSeconds": 5 }),
                ),
                cap_call,
            )
            .await
            .unwrap()
        })
    };
    cap_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("first ask should reach cap mock");
    cap_flag.store(true, std::sync::atomic::Ordering::Relaxed);
    let cap_err1 = cap_first.await.unwrap().err().unwrap();
    assert_eq!(cap_err1.code.as_ref(), "cancelled", "{cap_err1:?}");

    let cap_second = {
        let srv = srv_cap.clone();
        tokio::spawn(async move {
            srv.call(
                "ask",
                args_map(
                    json!({ "dataset": "org", "question": "List teams", "timeoutSeconds": 5 }),
                ),
                call_for(Principal::local()),
            )
            .await
            .unwrap()
        })
    };
    let cap_models = st_cap.models().unwrap();
    let wait_park = Instant::now();
    while cap_models.slot_waiters("cheap") == 0 && wait_park.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        cap_models.slot_waiters("cheap") >= 1,
        "second ask must park on concurrency slot before late settlement"
    );
    cap_guard.open();
    let cap_err2 = cap_second
        .await
        .unwrap()
        .err()
        .expect("queued ask must be refused after late settlement exhausts provider cap");
    assert_eq!(cap_err2.code.as_ref(), "budget-exceeded", "{cap_err2:?}");
    assert_eq!(
        cap_mock.requests().len(),
        1,
        "queued ask must not send a second POST after daily cap exhaustion"
    );

    // 1c. Late abandoned retry with cache-read tokens rounds once per step in both
    //     provider daily counters and Ask daily/metric counters while keeping raw tokens/cost.
    let (cache_tx, cache_rx) = std::sync::mpsc::channel::<()>();
    let cache_gate = Arc::new((parking_lot::Mutex::new(false), parking_lot::Condvar::new()));
    let cache_guard = GateGuard(cache_gate.clone());
    let cache_mock = {
        let gate = cache_gate.clone();
        let cache_tx = parking_lot::Mutex::new(Some(cache_tx));
        MockModel::start(move |_, n| {
            if n == 0 {
                return (
                    500,
                    json!({
                        "error": { "message": "temporary failure" },
                        "usage": {
                            "prompt_tokens": 5,
                            "completion_tokens": 0,
                            "prompt_tokens_details": { "cached_tokens": 5 }
                        }
                    }),
                );
            }
            if let Some(tx) = cache_tx.lock().take() {
                let _ = tx.send(());
                wait_gate(&gate, Duration::from_secs(10));
            }
            (
                200,
                json!({
                    "choices": [{
                        "message": { "content": draft_json(TEAMS_QUERY) },
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 5,
                        "completion_tokens": 0,
                        "prompt_tokens_details": { "cached_tokens": 5 }
                    }
                }),
            )
        })
    };
    let cache_cfg = json!({
        "providers": {
            "cheap": {
                "kind": "openai",
                "endpoint": cache_mock.url(),
                "requestTimeoutSecs": 5,
                "budget": { "tokensPerDay": 500 },
                "pricing": { "inputPerMTok": 10.0, "outputPerMTok": 20.0, "cacheReadPerMTok": 1.0 },
                "allowedModels": ["m"]
            }
        },
        "roles": { "draft": [{ "provider": "cheap", "model": "m" }] }
    });
    let st_cache = make_state(Some(Models::new(
        ModelsConfig::parse(&cache_cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    )));
    put_assistant(
        &st_cache,
        "org",
        json!({ "enabled": true, "send": "schema" }),
    )
    .await;
    let srv_cache = McpServer::new(st_cache.clone(), McpConfig::default());
    let cache_call = call_for(alice.clone());
    let cache_flag = cache_call.cancel.clone();
    let cache_handle = {
        let srv = srv_cache.clone();
        tokio::spawn(async move {
            srv.call(
                "ask",
                args_map(
                    json!({ "dataset": "org", "question": "List teams", "timeoutSeconds": 5 }),
                ),
                cache_call,
            )
            .await
            .unwrap()
        })
    };
    cache_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("retry attempt should arrive at cache mock");
    cache_flag.store(true, std::sync::atomic::Ordering::Relaxed);
    let cache_err = cache_handle.await.unwrap().err().unwrap();
    assert_eq!(cache_err.code.as_ref(), "cancelled", "{cache_err:?}");

    cache_guard.open();
    let cache_wait = Instant::now();
    let mut cache_read_seen = 0u64;
    while cache_wait.elapsed() < Duration::from_secs(3) {
        let u = st_cache.asks.usage_json(1);
        cache_read_seen = u["datasets"][0]["tokens"][0]["cacheReadInputTokens"]
            .as_u64()
            .unwrap_or(0);
        if cache_read_seen >= 10 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let u = st_cache.asks.usage_json(1);
    assert_eq!(
        cache_read_seen, 10,
        "raw cache-read tokens must sum to 10: {u}"
    );
    assert_eq!(
        u["datasets"][0]["tokens"][0]["budgetTokens"], 1,
        "Ask metrics budgetTokens must round once per step to 1: {u}"
    );
    assert!(
        u["datasets"][0]["tokens"][0]["estimatedCost"]
            .as_f64()
            .is_some_and(|c| c > 0.0),
        "Ask metrics estimatedCost must retain abandoned usage cost: {u}"
    );
    assert_eq!(
        st_cache.asks.tokens_today("org", Some(&alice.id())),
        1,
        "principal daily tokens must round once per step to 1"
    );
    assert_eq!(
        st_cache.asks.tokens_today("org", None),
        1,
        "dataset daily tokens must round once per step to 1"
    );
    assert_eq!(
        st_cache.models().unwrap().describe()["providers"][0]["budget"]["usedToday"],
        1,
        "provider daily tokens must round once per step to 1"
    );

    // 2. Deadline expiry while waiting for a provider concurrency slot sends 0 requests for caller 2.
    let slot_mock = MockModel::start(|_, _| {
        std::thread::sleep(Duration::from_millis(500));
        (200, mock::openai(&draft_json(TEAMS_QUERY)))
    });
    let slot_cfg = json!({
        "providers": {
            "cheap": {
                "kind": "openai",
                "endpoint": slot_mock.url(),
                "concurrency": 1,
                "requestTimeoutSecs": 5,
                "allowedModels": ["m"]
            }
        },
        "roles": { "draft": [{ "provider": "cheap", "model": "m" }] }
    });
    let st_slot = make_state(Some(Models::new(
        ModelsConfig::parse(&slot_cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    )));
    put_assistant(
        &st_slot,
        "org",
        json!({ "enabled": true, "send": "schema" }),
    )
    .await;
    let srv_slot = McpServer::new(st_slot, McpConfig::default());
    let first_task = {
        let srv = srv_slot.clone();
        tokio::spawn(async move {
            srv.call(
                "ask",
                args_map(
                    json!({ "dataset": "org", "question": "List teams", "timeoutSeconds": 5 }),
                ),
                call_for(Principal::local()),
            )
            .await
            .unwrap()
        })
    };
    // Wait until the first request has entered the provider and holds the single slot.
    let wait_start = Instant::now();
    while slot_mock.requests().is_empty() && wait_start.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let second_err = srv_slot
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "List teams", "timeoutSeconds": 0.12 })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .err()
        .expect("second call waiting for slot should time out");
    assert_eq!(second_err.code.as_ref(), "timeout", "{second_err:?}");
    let _ = first_task.await.unwrap().unwrap();
    assert_eq!(
        slot_mock.requests().len(),
        1,
        "expired slot waiter must never send a request to the provider"
    );

    // 3. Deadline expiry while waiting for RPM rate limit returns timeout immediately with 0 requests.
    let rpm_mock = scripted(vec![draft_json(TEAMS_QUERY)]);
    let rpm_cfg = json!({
        "providers": {
            "cheap": {
                "kind": "openai",
                "endpoint": rpm_mock.url(),
                "requestsPerMinute": 1,
                "requestTimeoutSecs": 5,
                "allowedModels": ["m"]
            }
        },
        "roles": { "draft": [{ "provider": "cheap", "model": "m" }] }
    });
    let st_rpm = make_state(Some(Models::new(
        ModelsConfig::parse(&rpm_cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    )));
    put_assistant(&st_rpm, "org", json!({ "enabled": true, "send": "schema" })).await;
    let srv_rpm = McpServer::new(st_rpm, McpConfig::default());
    let _ = srv_rpm
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "List teams" })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rpm_mock.requests().len(), 1);
    let rpm_t0 = Instant::now();
    let rpm_err = srv_rpm
        .call(
            "ask",
            args_map(json!({ "dataset": "org", "question": "List teams", "timeoutSeconds": 0.15 })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .err()
        .expect("call past RPM window should time out immediately");
    assert_eq!(rpm_err.code.as_ref(), "timeout", "{rpm_err:?}");
    assert!(rpm_t0.elapsed() < Duration::from_millis(500));
    assert_eq!(
        rpm_mock.requests().len(),
        1,
        "RPM-expired call must never hit the provider"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_over_http_mcp_endpoint() {
    let m = scripted(vec![draft_json(TEAMS_QUERY)]);
    let models = make_models(
        &m.url(),
        &m.url(),
        json!({
            "draft": [{ "provider": "cheap", "model": "m" }],
            "summarize": [{ "provider": "cheap", "model": "m" }]
        }),
    );
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    st.limits.max_result_bytes = None;
    st.models = arc_swap::ArcSwapOption::from_pointee(models);
    st.mcp = Some(Arc::new(super::http::HttpConf {
        cfg: McpConfig {
            http: true,
            ..McpConfig::default()
        },
        max_sessions: 4,
        shutdown: tokio_util::sync::CancellationToken::new(),
        require_auth: false,
    }));
    let st = Arc::new(st);
    let ds = st.attach("org", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            ORG.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    put_assistant(&st, "org", json!({ "enabled": true, "send": "rows" })).await;
    let app = crate::http::router(st.clone());

    let post_mcp = |method: &'static str, name: Option<&'static str>, payload: Value| {
        let app = app.clone();
        async move {
            let mut rb = Request::builder()
                .method("POST")
                .uri("/$/mcp")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", method);
            if let Some(n) = name {
                rb = rb.header("mcp-name", n);
            }
            let req = rb.body(Body::from(payload.to_string())).unwrap();
            let res = app.oneshot(req).await.unwrap();
            let status = res.status();
            let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap())
        }
    };

    let (status, list_resp) = post_mcp(
        "tools/list",
        None,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": { "_meta": modern_meta() }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list_resp}");
    assert!(
        list_resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "ask"),
        "ask should be listed over /$/mcp: {list_resp}"
    );

    let (status, call_resp) = post_mcp(
        "tools/call",
        Some("ask"),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "_meta": modern_meta(),
                "name": "ask",
                "arguments": { "dataset": "org", "question": "Which teams exist?" }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{call_resp}");
    assert_eq!(call_resp["result"]["isError"], false, "{call_resp}");
    let sc = &call_resp["result"]["structuredContent"];
    assert_eq!(sc["outcome"], "answered", "{sc}");
    assert_eq!(sc["summary"]["citations"], json!([1, 2]));
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_predicate_use_grounding_preserved_under_short_timeout() {
    // Regression: when the Ask's remaining time falls between 0.5 s and 5 s, predicate-use
    // grounding (`usage_query`) must NOT be rejected by `Tools::timeout` as over `max_timeout`.
    let m = scripted(vec![draft_json(TEAMS_QUERY)]);
    let cfg = json!({
        "providers": {
            "cheap": {
                "kind": "openai",
                "endpoint": m.url(),
                "contextTokens": 32768,
                "requestTimeoutSecs": 5,
                "allowedModels": ["m"]
            }
        },
        "roles": { "draft": [{ "provider": "cheap", "model": "m" }] }
    });
    let models = Models::new(
        ModelsConfig::parse(&cfg.to_string()).unwrap(),
        Default::default(),
        sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
    );
    let st = make_state(Some(models));
    put_assistant(&st, "org", json!({ "enabled": true, "send": "schema" })).await;
    let server = McpServer::new(st, McpConfig::default());

    let Outcome::Structured(out) = server
        .call(
            "ask",
            args_map(json!({
                "dataset": "org",
                "question": "Which person is in the Payments team?",
                "timeoutSeconds": 2.0
            })),
            call_for(Principal::local()),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected structured ask output");
    };
    assert_eq!(out["outcome"], "answered", "{out}");
    let reqs = m.requests();
    assert_eq!(reqs.len(), 1);
    let body = reqs[0].body.to_string();
    assert!(
        body.contains("Predicates that these entities"),
        "predicate-use grounding must reach the draft prompt when remaining time is in (0.5s, 5s): {body}"
    );
}
