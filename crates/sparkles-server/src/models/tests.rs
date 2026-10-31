//! The provider clients against mock endpoints: the request of each kind, capability
//! detection and the degradation steps of §3.6, retries, refusals, budgets, and keys
//! that never leave the request headers (C18 A19, A26).

use super::mock::{self, MockModel, Received};
use super::*;
use serde_json::json;

fn policy() -> sparkles::outbound::OutboundPolicy {
    sparkles::outbound::OutboundPolicy {
        allow_private: true,
        ..Default::default()
    }
}

fn models(providers: Value, secrets: &[(&str, &str)]) -> Models {
    let cfg = ModelsConfig::parse(&json!({ "providers": providers }).to_string()).unwrap();
    let secrets = secrets
        .iter()
        .map(|(n, s)| (n.to_string(), s.parse().unwrap()))
        .collect();
    Models::new(cfg, secrets, policy())
}

fn draft_schema() -> OutputSchema {
    OutputSchema {
        name: "Draft",
        schema: json!({
            "type": "object",
            "properties": { "query": { "type": "string" } },
            "required": ["query"],
            "additionalProperties": false
        }),
        from_text: |t| fenced(t, &["sparql"]).map(|q| json!({ "query": q })),
        text_instruction: "Write the query in one fenced sparql block.",
    }
}

fn call(m: &Models, provider: &str) -> Result<Answer, Failure> {
    m.call(
        Some(Role::Draft),
        &Pair::new(provider, "m1"),
        "system text",
        "user text",
        &draft_schema(),
        Instant::now() + Duration::from_secs(10),
    )
}

/// The schema reaches each kind in its own member (A26).
#[test]
fn each_kind_gets_the_schema_in_its_member() {
    for kind in [Kind::Ollama, Kind::Openai, Kind::Anthropic] {
        let mock = MockModel::start(move |_, _| (200, mock::answer(kind, r#"{"query":"ASK {}"}"#)));
        let m = models(
            json!({ "p": { "kind": kind.as_str(), "endpoint": mock.url() } }),
            &[],
        );
        let a = call(&m, "p").unwrap();
        assert_eq!(a.value, json!({"query": "ASK {}"}));
        assert_eq!(a.level, Level::JsonSchema);
        assert_eq!((a.record.input_tokens, a.record.output_tokens), (10, 5));
        let r = &mock.requests()[0];
        let schema = json!({"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"], "additionalProperties": false});
        match kind {
            Kind::Ollama => {
                assert_eq!(r.path, "/api/chat");
                assert_eq!(r.body["format"], schema);
                assert_eq!(r.body["options"]["temperature"], json!(0.0));
            }
            Kind::Openai => {
                assert_eq!(r.path, "/chat/completions");
                assert_eq!(r.body["response_format"]["json_schema"]["schema"], schema);
            }
            Kind::Anthropic => {
                assert_eq!(r.path, "/v1/messages");
                assert_eq!(r.body["output_config"]["format"]["schema"], schema);
                assert_eq!(r.header("anthropic-version"), Some("2023-06-01"));
                // current Claude models refuse temperature: it is never sent
                assert!(r.body.get("temperature").is_none());
            }
            Kind::Local => unreachable!("not in the list"),
        }
        // the detected level is remembered for the pair
        assert_eq!(m.level_of(&Pair::new("p", "m1")), Some(Level::JsonSchema));
    }
}

/// An `openai` endpoint that refuses `response_format` and answers plain text: the test
/// call reports the plain-text level, and the draft is taken from its fenced block (A26).
#[test]
fn detection_falls_back_to_plain_text() {
    let mock = MockModel::start(|r: &Received, _| {
        if r.body.get("response_format").is_some() {
            (
                400,
                json!({"error": {"message": "response_format is not supported"}}),
            )
        } else {
            (
                200,
                mock::openai(
                    "Here it is.\n```sparql\nSELECT * WHERE { ?s ?p ?o } LIMIT 1\n```\nIt lists one triple.",
                ),
            )
        }
    });
    let m = models(
        json!({ "gw": { "kind": "openai", "endpoint": mock.url() } }),
        &[],
    );
    let t = m.test("gw", Some("m1"), Duration::from_secs(10)).unwrap();
    assert_eq!(t["ok"], true, "{t}");
    assert_eq!(t["level"], "text", "{t}");
    assert_eq!(t["structuredOutput"], false);
    let kinds: Vec<String> = mock
        .requests()
        .iter()
        .map(|r| {
            r.body["response_format"]["type"]
                .as_str()
                .unwrap_or("none")
                .to_string()
        })
        .collect();
    assert_eq!(kinds, ["json_schema", "json_object", "none"]);
    let a = call(&m, "gw").unwrap();
    assert_eq!(a.level, Level::Text);
    assert_eq!(a.value["query"], "SELECT * WHERE { ?s ?p ?o } LIMIT 1");
    // remembered: no further detection requests
    assert!(mock.requests()[3].body.get("response_format").is_none());
    assert_eq!(mock.requests().len(), 4);
    assert!(mock::prompt_of(&mock.requests()[3]).contains("fenced sparql block"));
}

/// Invalid output is retried once with the errors, then fails without trying other
/// levels when the level is configured.
#[test]
fn invalid_output_is_retried_once() {
    let mock = MockModel::start(|_, n| match n {
        0 => (200, mock::ollama(r#"{"wrong": 1}"#)),
        _ => (200, mock::ollama("not json")),
    });
    let m = models(
        json!({ "l": { "kind": "ollama", "endpoint": mock.url(), "structuredOutput": "json-schema" } }),
        &[],
    );
    let f = call(&m, "l").unwrap_err();
    assert_eq!(f.error.code(), "invalid-output");
    assert_eq!(f.record.requests, 2);
    let retry = &mock.requests()[1];
    let msgs = retry.body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 4, "system, user, the bad answer, the errors");
    assert!(mock::prompt_of(retry).contains("lacks the member query"));
    // a valid retry succeeds
    let mock = MockModel::start(|_, n| match n {
        0 => (200, mock::ollama("{}")),
        _ => (200, mock::ollama(r#"{"query":"ASK {}"}"#)),
    });
    let m = models(
        json!({ "l": { "kind": "ollama", "endpoint": mock.url() } }),
        &[],
    );
    let a = call(&m, "l").unwrap();
    assert_eq!(a.record.requests, 2);
    assert_eq!((a.record.input_tokens, a.record.output_tokens), (20, 10));
}

/// An Anthropic request leaves out the numeric bounds the API refuses, so the bounds
/// are checked locally and an answer outside them is retried with the error.
#[test]
fn bounds_left_out_for_anthropic_are_checked_locally() {
    let mock = MockModel::start(|_, n| match n {
        0 => (200, mock::answer(Kind::Anthropic, r#"{"rating":11}"#)),
        _ => (200, mock::answer(Kind::Anthropic, r#"{"rating":7}"#)),
    });
    let m = models(
        json!({ "a": { "kind": "anthropic", "endpoint": mock.url() } }),
        &[],
    );
    let out = OutputSchema {
        name: "Report",
        schema: json!({
            "type": "object",
            "properties": { "rating": { "type": "integer", "minimum": 0, "maximum": 10 } },
            "required": ["rating"],
            "additionalProperties": false
        }),
        from_text: |_| None,
        text_instruction: "",
    };
    let a = m
        .call(
            Some(Role::Draft),
            &Pair::new("a", "m1"),
            "system text",
            "user text",
            &out,
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(a.value, json!({"rating": 7}));
    assert_eq!(a.record.requests, 2);
    let first = &mock.requests()[0];
    assert!(
        first.body["output_config"]["format"]["schema"]["properties"]["rating"]
            .get("maximum")
            .is_none()
    );
    assert!(mock::prompt_of(&mock.requests()[1]).contains("$.rating is above 10"));
}

#[test]
fn refusals_and_outages() {
    let mock = MockModel::start(|_, _| {
        (
            200,
            json!({"content": [], "stop_reason": "refusal", "usage": {"input_tokens": 3, "output_tokens": 0}}),
        )
    });
    let m = models(
        json!({ "a": { "kind": "anthropic", "endpoint": mock.url() } }),
        &[],
    );
    let f = call(&m, "a").unwrap_err();
    assert_eq!(
        f.error,
        StepError::Call(CallError::Refusal("the model refused to answer".into()))
    );
    // 429 twice, then an answer: the client's own retries
    let mock = MockModel::start(|_, n| {
        if n < 2 {
            (429, json!({"error": {"message": "slow down"}}))
        } else {
            (200, mock::openai(r#"{"query":"ASK {}"}"#))
        }
    });
    let m = models(
        json!({ "o": { "kind": "openai", "endpoint": mock.url() } }),
        &[],
    );
    assert!(call(&m, "o").is_ok());
    assert_eq!(mock.requests().len(), 3);
    // 503 always: unavailable after the retries
    let mock = MockModel::start(|_, _| (503, json!({"error": "down"})));
    let m = models(
        json!({ "o": { "kind": "openai", "endpoint": mock.url() } }),
        &[],
    );
    let f = call(&m, "o").unwrap_err();
    assert_eq!(f.error.code(), "provider-unavailable");
    assert_eq!(mock.requests().len(), 3);
    // nothing listening
    let url = {
        let mock = MockModel::start(|_, _| (200, json!({})));
        mock.url()
    };
    std::thread::sleep(Duration::from_millis(50));
    let m = models(json!({ "o": { "kind": "openai", "endpoint": url } }), &[]);
    assert_eq!(
        call(&m, "o").unwrap_err().error.code(),
        "provider-unavailable"
    );
    // the outbound policy refuses a private address unless allowed
    let cfg = ModelsConfig::parse(
        &json!({ "providers": { "o": { "kind": "openai", "endpoint": "http://127.0.0.1:9" } } })
            .to_string(),
    )
    .unwrap();
    let strict = Models::new(
        cfg,
        BTreeMap::new(),
        sparkles::outbound::OutboundPolicy {
            allow_private: false,
            ..Default::default()
        },
    );
    assert_eq!(
        call(&strict, "o").unwrap_err().error.code(),
        "outbound-refused"
    );
}

/// Keys come from named secrets, go out only in the request header, and never appear
/// in `GET /$/models`, in an error or in a record (A19).
#[test]
fn keys_stay_in_headers() {
    let key = "sk-test-0123456789-secret";
    // SAFETY: tests that read this variable run in this test only
    unsafe { std::env::set_var("SPARKLES_TEST_MODEL_KEY_A19", key) };
    let mock = MockModel::start(move |r: &Received, _| {
        // an error that echoes the key back
        (
            401,
            json!({"error": {"message": format!("bad key {}", r.header("x-api-key").unwrap_or(""))}}),
        )
    });
    let m = models(
        json!({
            "a": { "kind": "anthropic", "endpoint": mock.url(), "apiKey": {"secret": "anth"}, "models": {"m1": {}} },
            "b": { "kind": "openai", "endpoint": mock.url(), "apiKey": {"secret": "missing"} }
        }),
        &[("anth", "env:SPARKLES_TEST_MODEL_KEY_A19")],
    );
    let f = call(&m, "a").unwrap_err();
    assert_eq!(f.error.code(), "provider-auth");
    assert!(!f.error.message().contains(key), "{}", f.error.message());
    assert!(f.error.message().contains("[redacted]"));
    assert_eq!(mock.requests()[0].header("x-api-key"), Some(key));
    let f = call(&m, "b").unwrap_err();
    assert_eq!(f.error.code(), "secret-missing");
    let d = m.describe().to_string();
    assert!(!d.contains(key), "{d}");
    assert!(!d.contains("SPARKLES_TEST_MODEL_KEY_A19"), "{d}");
    let v = m.describe();
    assert_eq!(v["providers"][0]["status"], "ok");
    assert_eq!(v["providers"][1]["status"], "secret-missing");
    assert_eq!(
        v["providers"][1]["apiKey"],
        json!({"secret": "missing", "source": "missing"})
    );
    assert_eq!(v["providers"][0]["models"][0]["status"]["state"], "failing");
}

#[test]
fn daily_budget_and_cost() {
    let mock = MockModel::start(|_, _| (200, mock::ollama(r#"{"query":"ASK {}"}"#)));
    let m = models(
        json!({ "l": {
            "kind": "ollama", "endpoint": mock.url(), "budget": {"tokensPerDay": 20},
            "models": {"m1": {"pricing": {"inputPerMTok": 1.0, "outputPerMTok": 2.0}}}
        } }),
        &[],
    );
    let a = call(&m, "l").unwrap();
    assert_eq!(a.record.estimated_cost, Some((10.0 + 10.0) / 1e6));
    assert!(call(&m, "l").is_ok());
    let f = call(&m, "l").unwrap_err();
    assert_eq!(f.error.code(), "budget-exceeded");
    assert_eq!(mock.requests().len(), 2, "no request over the cap");
    assert_eq!(m.describe()["providers"][0]["budget"]["usedToday"], 30);
}

/// An Anthropic answer that reports cache writes and reads is charged each class at its
/// price: in the step's record and cost, in the provider's daily budget, and in the
/// budget tokens of its JSON. Counting only `input_tokens` would count 10 of the
/// 4010 prompt tokens.
#[test]
fn cache_tokens_are_charged_at_their_prices() {
    let mock = MockModel::start(|_, n| {
        let (written, read) = if n == 0 { (4000, 0) } else { (0, 4000) };
        (
            200,
            json!({
                "content": [{ "type": "text", "text": r#"{"query":"ASK {}"}"# }],
                "stop_reason": "end_turn",
                "usage": { "input_tokens": 10, "output_tokens": 5,
                    "cache_creation_input_tokens": written, "cache_read_input_tokens": read,
                    "cache_creation": { "ephemeral_5m_input_tokens": written, "ephemeral_1h_input_tokens": 0 } }
            }),
        )
    });
    let m = models(
        json!({ "a": {
            "kind": "anthropic", "endpoint": mock.url(), "budget": {"tokensPerDay": 5500},
            "models": {"m1": {"pricing": {"inputPerMTok": 1.0, "outputPerMTok": 5.0}}}
        } }),
        &[],
    );
    let a = call(&m, "a").unwrap();
    assert_eq!(a.record.cache_write_5m_tokens, 4000);
    // 10 + 1.25 × 4000 input-token equivalents, and 5 output tokens
    assert_eq!(a.record.budget_tokens(), 5015);
    let cost = a.record.estimated_cost.unwrap();
    assert!(
        (cost - (10.0 + 5000.0 + 25.0) / 1e6).abs() < 1e-12,
        "{cost}"
    );
    let j = a.record.json();
    assert_eq!(
        (
            j["cacheWriteInputTokens"].clone(),
            j["cacheReadInputTokens"].clone()
        ),
        (json!(4000), json!(0))
    );
    assert_eq!(j["budgetTokens"], 5015);
    assert_eq!(usage_json(&j).budget(), 5015);
    assert_eq!(m.describe()["providers"][0]["budget"]["usedToday"], 5015);
    // the read is a tenth of the price: 10 + 400 + 5
    let b = call(&m, "a").unwrap();
    assert_eq!(b.record.cache_read_tokens, 4000);
    assert_eq!(b.record.budget_tokens(), 415);
    assert!((b.record.estimated_cost.unwrap() - (10.0 + 400.0 + 25.0) / 1e6).abs() < 1e-12);
    assert_eq!(m.describe()["providers"][0]["budget"]["usedToday"], 5430);
    // the cap holds: 5430 of 5500 leaves room for one more, and then none
    assert!(call(&m, "a").is_ok());
    assert_eq!(call(&m, "a").unwrap_err().error.code(), "budget-exceeded");
    assert_eq!(mock.requests().len(), 3);
    let p = &m.describe()["providers"][0];
    assert_eq!(p["promptCaching"], json!({ "enabled": true, "ttl": "5m" }));
    assert_eq!(p["models"][0]["pricing"]["cacheReadPerMTok"], json!(0.1));
    // the request marked the system text for the cache
    let r = &mock.requests()[0];
    assert_eq!(
        r.body["system"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
}

/// The budget of several steps is the sum of each step's own, as the ask's budget
/// counts it, not the budget of their summed usage: two reads of 513 tokens count 52
/// each, 104 together, where their sum would count 103.
#[test]
fn step_budgets_are_rounded_per_step() {
    let step = StepRecord {
        cache_read_tokens: 513,
        ..Default::default()
    };
    assert_eq!(step.budget_tokens(), 52);
    let steps = [step.clone(), step];
    let mut j = json!({ "inputTokens": 0, "outputTokens": 0 });
    steps_json(&mut j, &steps);
    assert_eq!(j["budgetTokens"], 104);
    assert_eq!(j["cacheReadInputTokens"], 1026);
    assert_eq!(budget_of(&j), 104);
    assert_eq!(usage_total(&steps).budget(), 103);
    // a usage without cache members counts its input and output
    assert_eq!(
        budget_of(&json!({ "inputTokens": 7, "outputTokens": 3 })),
        10
    );
}

/// A step retried after a cache-bearing answer charges the provider's budget what the
/// step's budget grew by, so both round the step's cache reads once: two reads of 513
/// tokens count 103, not 52 + 52.
#[test]
fn the_provider_budget_rounds_each_step_once() {
    let mock = MockModel::start(|_, n| {
        let text = if n == 0 {
            r#"{"wrong":1}"#
        } else {
            r#"{"query":"ASK {}"}"#
        };
        (
            200,
            json!({
                "content": [{ "type": "text", "text": text }], "stop_reason": "end_turn",
                "usage": { "input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 513 }
            }),
        )
    });
    let m = models(
        json!({ "a": { "kind": "anthropic", "endpoint": mock.url(),
            "structuredOutput": "json-schema", "budget": { "tokensPerDay": 1000 } } }),
        &[],
    );
    let a = call(&m, "a").unwrap();
    assert_eq!(a.record.requests, 2);
    assert_eq!(a.record.budget_tokens(), 103);
    assert_eq!(m.describe()["providers"][0]["budget"]["usedToday"], 103);
}

/// At the `tool` level the answer comes as the tool's input with no text. A retry for
/// a bound the strict tool could not carry shows that input as the assistant's turn,
/// since Anthropic refuses an empty assistant message.
#[test]
fn a_tool_answer_is_replayed_in_its_retry() {
    let mock = MockModel::start(|_, n| {
        let q = if n == 0 {
            "SELECT * WHERE { ?s ?p ?o }"
        } else {
            "ASK {}"
        };
        (
            200,
            json!({
                "content": [{ "type": "tool_use", "id": "t", "name": "Draft", "input": { "query": q } }],
                "stop_reason": "tool_use", "usage": { "input_tokens": 10, "output_tokens": 5 }
            }),
        )
    });
    let m = models(
        json!({ "a": { "kind": "anthropic", "endpoint": mock.url(), "structuredOutput": "tool" } }),
        &[],
    );
    let mut out = draft_schema();
    out.schema["properties"]["query"]["maxLength"] = json!(10);
    let a = m
        .call(
            Some(Role::Draft),
            &Pair::new("a", "m1"),
            "system text",
            "user text",
            &out,
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(a.value, json!({ "query": "ASK {}" }));
    let r = mock.requests();
    assert_eq!(r[0].body["tool_choice"], json!({ "type": "auto" }));
    assert_eq!(
        r[0].body["tools"][0]["input_schema"]["properties"]["query"],
        json!({ "type": "string" })
    );
    let said = &r[1].body["messages"][1];
    assert_eq!(said["role"], "assistant");
    assert_eq!(
        said["content"],
        r#"{"query":"SELECT * WHERE { ?s ?p ?o }"}"#
    );
}

#[test]
fn a_reload_shares_the_token_count_with_requests_on_the_old_config() {
    let mock = MockModel::start(|_, _| (200, mock::ollama(r#"{"query":"ASK {}"}"#)));
    let provider = json!({ "l": {
        "kind": "ollama", "endpoint": mock.url(), "budget": {"tokensPerDay": 100},
        "models": {"m1": {}}
    } });
    let old = models(provider.clone(), &[]);
    call(&old, "l").unwrap();
    let mut new = models(provider, &[]);
    new.inherit(&old);
    // a request that started before the reload finishes on the old configuration
    call(&old, "l").unwrap();
    call(&new, "l").unwrap();
    // 15 tokens a call, three calls counted once
    assert_eq!(new.describe()["providers"][0]["budget"]["usedToday"], 45);
    assert_eq!(old.describe()["providers"][0]["budget"]["usedToday"], 45);
}

#[test]
fn describe_lists_roles_and_models() {
    let cfg = ModelsConfig::parse(
        &json!({
            "providers": {
                "local": { "kind": "ollama", "endpoint": "http://127.0.0.1:11434", "models": {"qwen3:8b": {"contextTokens": 16384}} }
            },
            "roles": { "draft": [{"provider": "local", "model": "qwen3:8b"}, {"provider": "local", "model": "qwen3:32b"}] }
        })
        .to_string(),
    )
    .unwrap();
    let m = Models::new(cfg, BTreeMap::new(), policy());
    let d = m.describe();
    let names: Vec<&str> = d["providers"][0]["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["qwen3:8b", "qwen3:32b"]);
    assert_eq!(d["providers"][0]["models"][0]["contextTokens"], 16384);
    assert_eq!(
        d["providers"][0]["models"][1]["status"]["state"],
        "untested"
    );
    assert_eq!(d["roles"]["repair"], d["roles"]["draft"]);
    assert_eq!(d["roles"]["summarize"], json!([]));
}

/// `GET /$/models` and `POST /$/models/{name}/test` over HTTP (A19, A26).
#[tokio::test(flavor = "multi_thread")]
async fn the_routes() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    let dir = tempfile::tempdir().unwrap();
    let state = || {
        crate::state::AppState::new(
            dir.path(),
            sparkles::store::StoreOptions::default(),
            Duration::from_secs(30),
        )
        .unwrap()
    };
    let send = |app: axum::Router, req: Request<Body>| async move {
        let res = app.oneshot(req).await.unwrap();
        let status = res.status();
        let b = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&b).into_owned())
    };
    let get = || Request::get("/$/models").body(Body::empty()).unwrap();
    // no configuration
    let app = crate::http::router(Arc::new(state()));
    let (s, b) = send(app.clone(), get()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&b).unwrap()["configured"],
        false
    );
    let (s, b) = send(
        app,
        Request::post("/$/models/x/test")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(b.contains("no-models"), "{b}");

    let key = "sk-route-test-key-987";
    // SAFETY: only this test reads this variable
    unsafe { std::env::set_var("SPARKLES_TEST_MODEL_KEY_ROUTES", key) };
    let mock = MockModel::start(|r: &Received, _| {
        if r.body.get("response_format").is_some() {
            (
                400,
                json!({"error": {"message": "response_format is not supported"}}),
            )
        } else {
            (
                200,
                mock::openai("```json\n{\"answer\": \"it works\"}\n```"),
            )
        }
    });
    let mut st = state();
    st.models = arc_swap::ArcSwapOption::from_pointee(models(
        json!({
            "gw": { "kind": "openai", "endpoint": mock.url(), "apiKey": {"secret": "gw"}, "models": {"m1": {}} },
            "gone": { "kind": "openai", "endpoint": mock.url(), "apiKey": {"secret": "nothing"} }
        }),
        &[("gw", "env:SPARKLES_TEST_MODEL_KEY_ROUTES")],
    ));
    let app = crate::http::router(Arc::new(st));
    let (s, b) = send(
        app.clone(),
        Request::post("/$/models/gw/test")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model":"m1","timeoutSeconds":10}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let t: Value = serde_json::from_str(&b).unwrap();
    assert_eq!(
        (t["ok"].clone(), t["level"].clone()),
        (json!(true), json!("text")),
        "{t}"
    );
    assert_eq!(t["structuredOutput"], false);
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some(&*format!("Bearer {key}"))
    );
    let (s, b) = send(app.clone(), get()).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        !b.contains(key) && !b.contains("SPARKLES_TEST_MODEL_KEY_ROUTES"),
        "{b}"
    );
    let v: Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v["configured"], true);
    assert_eq!(v["providers"][0]["name"], "gone");
    assert_eq!(v["providers"][0]["status"], "secret-missing");
    assert_eq!(v["providers"][1]["models"][0]["detected"], "text");
    assert_eq!(v["providers"][1]["models"][0]["status"]["state"], "ok");
    let (s, b) = send(
        app.clone(),
        Request::post("/$/models/nope/test")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(b.contains("unknown-provider"));
    let (s, _) = send(
        app,
        Request::post("/$/models/gw/test")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"endpoint":"http://evil.example"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "no member points a provider anywhere"
    );
}

/// A provider whose certificate no system trusts (spec C19 §11.6): refused by default,
/// reached with its CA as `tls.caCert` from a file or a secret, and reached with
/// `insecureSkipVerify`, which `GET /$/models` reports as unverified. The outbound
/// policy still applies with either option.
#[cfg(feature = "tls")]
#[test]
fn provider_tls_options() {
    let mock = MockModel::start_tls(|_, _| (200, mock::openai(r#"{"query":"ASK {}"}"#)));
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, mock::TEST_CA).unwrap();
    let not_pem = dir.path().join("not.pem");
    std::fs::write(&not_pem, "not a certificate").unwrap();
    let provider =
        |tls: Value| json!({ "p": { "kind": "openai", "endpoint": mock.url(), "tls": tls } });

    // the system roots alone: the handshake fails, and the message says why
    let m = models(provider(json!({})), &[]);
    let e = call(&m, "p").unwrap_err();
    assert_eq!(e.error.code(), "provider-unavailable", "{:?}", e.error);
    assert!(
        e.error.message().contains("certificate is not trusted"),
        "{}",
        e.error.message()
    );
    assert_eq!(m.describe()["providers"][0]["unverified"], false);

    // the CA from a file, then from a secret
    let m = models(
        provider(json!({ "caCert": { "file": ca.to_str().unwrap() } })),
        &[],
    );
    call(&m, "p").unwrap();
    let d = m.describe();
    assert_eq!(d["providers"][0]["tls"]["verification"], "custom-ca", "{d}");
    assert_eq!(d["providers"][0]["tls"]["caCert"]["status"], "ok", "{d}");
    let secret = format!("file:{}", ca.display());
    let m = models(
        provider(json!({ "caCert": { "secret": "internal-ca" } })),
        &[("internal-ca", &secret)],
    );
    call(&m, "p").unwrap();
    assert_eq!(
        m.describe()["providers"][0]["tls"]["caCert"]["source"],
        "declared"
    );

    // a CA that cannot be read or is not PEM fails before any request
    let n = mock.requests().len();
    for tls in [
        json!({ "caCert": { "file": dir.path().join("missing.pem").to_str().unwrap() } }),
        json!({ "caCert": { "file": not_pem.to_str().unwrap() } }),
        json!({ "caCert": { "secret": "nowhere" } }),
    ] {
        let m = models(provider(tls.clone()), &[]);
        let e = call(&m, "p").unwrap_err();
        assert_eq!(e.error.code(), "tls-config", "{tls}: {:?}", e.error);
        assert_eq!(
            m.describe()["providers"][0]["tls"]["caCert"]["status"],
            "unreadable"
        );
    }
    assert_eq!(mock.requests().len(), n);

    // no check at all: reached, and reported as unverified
    let m = models(provider(json!({ "insecureSkipVerify": true })), &[]);
    call(&m, "p").unwrap();
    let d = m.describe();
    assert_eq!(d["providers"][0]["unverified"], true, "{d}");
    assert_eq!(d["providers"][0]["tls"]["verification"], "off", "{d}");

    // the outbound policy still refuses a loopback address it does not allow
    let cfg = ModelsConfig::parse(
        &json!({ "providers": provider(json!({ "insecureSkipVerify": true })) }).to_string(),
    )
    .unwrap();
    let closed = Models::new(cfg, Default::default(), Default::default());
    let e = call(&closed, "p").unwrap_err();
    assert_eq!(e.error.code(), "outbound-refused", "{:?}", e.error);
}
