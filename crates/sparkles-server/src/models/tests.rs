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

/// A prompt's follow-up form reaches an `openai` provider with `promptCaching.enabled` as
/// two user messages, the earlier call's message first and unchanged. Without it, and for
/// the anthropic kind, the provider gets the prompt's one user message.
#[test]
fn the_follow_up_form_reaches_only_an_openai_provider_that_asks_for_it() {
    let prompt = Prompt {
        system: vec![Part::cached("system text")],
        user: vec![Part::cached("grounding"), Part::text("repair and format")],
        follow: Some((
            vec![Part::cached("grounding"), Part::text("format")],
            vec![Part::text("repair and format")],
        )),
    };
    for (kind, caching, want) in [
        (
            "openai",
            json!({ "enabled": true }),
            vec![
                "grounding".to_string() + "format",
                "repair and format".into(),
            ],
        ),
        (
            "openai",
            json!({ "enabled": false }),
            vec!["groundingrepair and format".to_string()],
        ),
        (
            "anthropic",
            json!({ "enabled": true }),
            vec!["groundingrepair and format".to_string()],
        ),
    ] {
        let k = if kind == "openai" {
            Kind::Openai
        } else {
            Kind::Anthropic
        };
        let mock = MockModel::start(move |_, _| (200, mock::answer(k, r#"{"query":"ASK {}"}"#)));
        let m = models(
            json!({ "p": { "kind": kind, "endpoint": mock.url(), "promptCaching": caching } }),
            &[],
        );
        m.call_prompt(
            Some(Role::Repair),
            &Pair::new("p", "m1"),
            &prompt,
            &draft_schema(),
            Instant::now() + Duration::from_secs(10),
            None,
        )
        .unwrap();
        let body = &mock.requests()[0].body;
        let users: Vec<String> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "user")
            .map(|m| match &m["content"] {
                Value::String(s) => s.clone(),
                Value::Array(b) => b.iter().map(|x| x["text"].as_str().unwrap()).collect(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(users, want, "{kind} {caching}");
        assert!(
            body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["role"] != "developer")
        );
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

/// A view with other secrets shares the provider's daily budget and concurrency slots,
/// so a request through either counts against the same limits.
#[test]
fn a_secret_view_shares_provider_limits() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&a, "key-a").unwrap();
    std::fs::write(&b, "key-b").unwrap();
    let mock = MockModel::start(|r, _| {
        if r.header("authorization") == Some("Bearer key-b") {
            std::thread::sleep(Duration::from_millis(300));
        }
        (200, mock::openai(r#"{"query":"ASK {}"}"#))
    });
    let fa = format!("file:{}", a.display());
    let m = models(
        json!({ "o": {
            "kind": "openai", "endpoint": mock.url(), "apiKey": {"secret": "k"},
            "concurrency": 1, "budget": {"tokensPerDay": 60}
        } }),
        &[("k", fa.as_str())],
    );
    let view = m.with_secrets(
        [(
            "k".to_string(),
            format!("file:{}", b.display()).parse().unwrap(),
        )]
        .into(),
    );
    // the view's slow request holds the one slot, so a request through the base
    // waits for it instead of running beside it
    std::thread::scope(|s| {
        let slow = s.spawn(|| call(&view, "o"));
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        assert!(call(&m, "o").is_ok());
        assert!(started.elapsed() >= Duration::from_millis(150));
        assert!(slow.join().unwrap().is_ok());
    });
    // each of the two answers counts 15 tokens in the one shared count
    assert_eq!(m.describe()["providers"][0]["budget"]["usedToday"], 30);
    assert_eq!(view.describe()["providers"][0]["budget"]["usedToday"], 30);
    let keys: Vec<_> = mock
        .requests()
        .iter()
        .filter_map(|r| r.header("authorization").map(str::to_string))
        .collect();
    assert!(
        keys.contains(&"Bearer key-a".to_string()) && keys.contains(&"Bearer key-b".to_string())
    );
    // the budget both spent stops further requests through either
    let more = (0..10).take_while(|_| call(&view, "o").is_ok()).count();
    assert!(more < 10);
    assert_eq!(call(&m, "o").unwrap_err().error.code(), "budget-exceeded");
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
    for preamble in ["", "Here is the query."] {
        let mock = MockModel::start(move |_, n| {
            let q = if n == 0 {
                "SELECT * WHERE { ?s ?p ?o }"
            } else {
                "ASK {}"
            };
            let mut content = vec![
                json!({ "type": "tool_use", "id": "t", "name": "Draft", "input": { "query": q } }),
            ];
            if !preamble.is_empty() {
                content.insert(0, json!({ "type": "text", "text": preamble }));
            }
            (
                200,
                json!({
                    "content": content,
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
        let input = r#"{"query":"SELECT * WHERE { ?s ?p ?o }"}"#;
        let expected = if preamble.is_empty() {
            input.to_string()
        } else {
            format!("{preamble}\n{input}")
        };
        assert_eq!(said["content"], expected.as_str());
    }
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

#[test]
fn delayed_send_past_absolute_deadline_does_not_post() {
    use super::admission::{Admission, Attempt, Settle, Ticket, Usage};

    struct RecordSettle(Arc<Mutex<Vec<Option<Usage>>>>);
    impl Settle for RecordSettle {
        fn reported(self: Box<Self>, usage: Usage) {
            self.0.lock().push(Some(usage));
        }
        fn unknown(self: Box<Self>) {
            self.0.lock().push(None);
        }
    }
    struct SlowAdmit {
        delay: Duration,
        settled: Arc<Mutex<Vec<Option<Usage>>>>,
    }
    impl Admission for SlowAdmit {
        fn admit(&self, _: &Attempt<'_>) -> Result<Ticket, CallError> {
            std::thread::sleep(self.delay);
            Ok(Ticket::new(Box::new(RecordSettle(self.settled.clone()))))
        }
    }

    let mock = MockModel::start(|_, _| (200, mock::openai(r#"{"query":"ASK {}"}"#)));
    let m = models(
        json!({ "p": { "kind": "openai", "endpoint": mock.url(), "requestTimeoutSecs": 5 } }),
        &[],
    );
    let settled = Arc::new(Mutex::new(Vec::new()));
    let admit = SlowAdmit {
        delay: Duration::from_millis(80),
        settled: settled.clone(),
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let err = m
        .call_prompt_with_cancel(
            Some(Role::Draft),
            &Pair::new("p", "m1"),
            &Prompt::plain("system text", "user text"),
            &draft_schema(),
            Instant::now() + Duration::from_millis(30),
            Some(&admit),
            Some(&cancel),
        )
        .unwrap_err();
    assert_eq!(err.error.code(), "deadline", "{:?}", err.error);
    assert_eq!(
        mock.requests().len(),
        0,
        "provider send delayed past absolute deadline must never POST"
    );
    assert_eq!(
        *settled.lock(),
        vec![Some(Usage::default())],
        "unsent ticket must settle as not_sent (0 usage)"
    );
}

/// RAII guard that opens a condvar gate on drop so an assertion failure cannot leak a
/// blocked mock handler thread.
struct GateGuard(Arc<(Mutex<bool>, Condvar)>);

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

fn wait_gate(gate: &(Mutex<bool>, Condvar), timeout: Duration) {
    let mut open = gate.0.lock();
    let deadline = Instant::now() + timeout;
    while !*open {
        if gate.1.wait_until(&mut open, deadline).timed_out() {
            break;
        }
    }
}

#[test]
fn cancelled_http_holds_slot_and_settles_admission_ticket() {
    use super::admission::{Admission, Attempt, Settle, Ticket, Usage};

    struct RecordSettle(Arc<Mutex<Vec<Option<Usage>>>>);
    impl Settle for RecordSettle {
        fn reported(self: Box<Self>, usage: Usage) {
            self.0.lock().push(Some(usage));
        }
        fn unknown(self: Box<Self>) {
            self.0.lock().push(None);
        }
    }
    struct TrackAdmit(Arc<Mutex<Vec<Option<Usage>>>>);
    impl Admission for TrackAdmit {
        fn admit(&self, _: &Attempt<'_>) -> Result<Ticket, CallError> {
            Ok(Ticket::new(Box::new(RecordSettle(self.0.clone()))))
        }
    }

    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel::<()>();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let gate_guard = GateGuard(gate.clone());
    let mock = {
        let gate = gate.clone();
        let arrived_tx = Mutex::new(Some(arrived_tx));
        MockModel::start(move |_, _| {
            if let Some(tx) = arrived_tx.lock().take() {
                let _ = tx.send(());
                wait_gate(&gate, Duration::from_secs(10));
            }
            (
                200,
                json!({
                    "choices": [{
                        "message": { "content": r#"{"query":"ASK {}"}"# },
                        "finish_reason": "stop"
                    }],
                    "usage": { "prompt_tokens": 45, "completion_tokens": 15 }
                }),
            )
        })
    };
    let m = Arc::new(models(
        json!({ "p": { "kind": "openai", "endpoint": mock.url(), "concurrency": 1, "requestTimeoutSecs": 5, "budget": { "tokensPerDay": 500 } } }),
        &[],
    ));
    let settled = Arc::new(Mutex::new(Vec::new()));
    let cancel = Arc::new(AtomicBool::new(false));
    let h = {
        let m = m.clone();
        let settled = settled.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let admit = TrackAdmit(settled);
            m.call_prompt_with_cancel(
                Some(Role::Draft),
                &Pair::new("p", "m1"),
                &Prompt::plain("system text", "user text"),
                &draft_schema(),
                Instant::now() + Duration::from_secs(5),
                Some(&admit),
                Some(&cancel),
            )
        })
    };
    arrived_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("request should reach mock");
    cancel.store(true, Ordering::Relaxed);
    let err = h.join().unwrap().unwrap_err();
    assert_eq!(err.error.code(), "cancelled", "{:?}", err.error);
    assert!(
        settled.lock().is_empty(),
        "ticket must remain unsettled while HTTP response is still in flight"
    );

    // Second call must block on concurrency: 1 and time out without reaching mock.
    let err2 = m
        .call_prompt(
            Some(Role::Draft),
            &Pair::new("p", "m1"),
            &Prompt::plain("system text", "user text"),
            &draft_schema(),
            Instant::now() + Duration::from_millis(120),
            None,
        )
        .unwrap_err();
    assert_eq!(err2.error.code(), "deadline", "{:?}", err2.error);
    assert_eq!(mock.requests().len(), 1);

    // Release the mock response and verify both late ticket settlement and provider accounting.
    gate_guard.open();
    let start = Instant::now();
    while (settled.lock().is_empty() || m.describe()["providers"][0]["budget"]["usedToday"] != 60)
        && start.elapsed() < Duration::from_secs(3)
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        *settled.lock(),
        vec![Some(Usage {
            input: 45,
            output: 15,
            ..Default::default()
        })],
        "abandoned HTTP response must settle admission ticket with reported usage"
    );
    assert_eq!(m.describe()["providers"][0]["budget"]["usedToday"], 60);
    // Slot is now free for a subsequent call.
    call(&m, "p").unwrap();
    assert_eq!(mock.requests().len(), 2);
}

#[test]
fn queued_call_refused_after_late_settlement_exhausts_provider_cap() {
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel::<()>();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let gate_guard = GateGuard(gate.clone());
    let mock = {
        let gate = gate.clone();
        let arrived_tx = Mutex::new(Some(arrived_tx));
        MockModel::start(move |_, _| {
            if let Some(tx) = arrived_tx.lock().take() {
                let _ = tx.send(());
                wait_gate(&gate, Duration::from_secs(10));
            }
            (
                200,
                json!({
                    "choices": [{
                        "message": { "content": r#"{"query":"ASK {}"}"# },
                        "finish_reason": "stop"
                    }],
                    "usage": { "prompt_tokens": 45, "completion_tokens": 15 }
                }),
            )
        })
    };
    let m = Arc::new(models(
        json!({ "p": { "kind": "openai", "endpoint": mock.url(), "concurrency": 1, "requestTimeoutSecs": 5, "budget": { "tokensPerDay": 50 } } }),
        &[],
    ));
    let cancel = Arc::new(AtomicBool::new(false));
    let h1 = {
        let m = m.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            m.call_prompt_with_cancel(
                Some(Role::Draft),
                &Pair::new("p", "m1"),
                &Prompt::plain("system text", "user text"),
                &draft_schema(),
                Instant::now() + Duration::from_secs(5),
                None,
                Some(&cancel),
            )
        })
    };
    arrived_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("first request should reach mock");
    cancel.store(true, Ordering::Relaxed);
    let err1 = h1.join().unwrap().unwrap_err();
    assert_eq!(err1.error.code(), "cancelled", "{:?}", err1.error);

    // Queue a second call while the first response is still held so it passes the initial
    // daily-cap read at 0 and parks waiting for the single concurrency slot.
    let h2 = {
        let m = m.clone();
        std::thread::spawn(move || {
            m.call_prompt(
                Some(Role::Draft),
                &Pair::new("p", "m1"),
                &Prompt::plain("system text", "user text"),
                &draft_schema(),
                Instant::now() + Duration::from_secs(5),
                None,
            )
        })
    };
    let wait_start = Instant::now();
    while m.slot_waiters("p") == 0 && wait_start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        m.slot_waiters("p") >= 1,
        "second call must be queued on concurrency slot before late settlement"
    );

    // Release the first response (60 tokens > 50 cap). When the slot frees, the queued
    // call must re-check the daily cap and refuse without posting.
    gate_guard.open();
    let err2 = h2.join().unwrap().unwrap_err();
    assert_eq!(err2.error.code(), "budget-exceeded", "{:?}", err2.error);
    assert_eq!(
        mock.requests().len(),
        1,
        "queued call must not send a POST after late settlement exhausted the daily cap"
    );
    assert_eq!(m.describe()["providers"][0]["budget"]["usedToday"], 60);
}

#[test]
fn paused_worker_during_http_setup_aborts_without_posting() {
    use super::admission::{Admission, Attempt, Settle, Ticket, Usage};

    struct RecordSettle(Arc<Mutex<Vec<Option<Usage>>>>);
    impl Settle for RecordSettle {
        fn reported(self: Box<Self>, usage: Usage) {
            self.0.lock().push(Some(usage));
        }
        fn unknown(self: Box<Self>) {
            self.0.lock().push(None);
        }
    }
    struct TrackAdmit(Arc<Mutex<Vec<Option<Usage>>>>);
    impl Admission for TrackAdmit {
        fn admit(&self, _: &Attempt<'_>) -> Result<Ticket, CallError> {
            Ok(Ticket::new(Box::new(RecordSettle(self.0.clone()))))
        }
    }

    // 1. Cancellation during HTTP setup (after admission, request construction, and header hook).
    let mock_cancel = MockModel::start(|_, _| (200, mock::openai(r#"{"query":"ASK {}"}"#)));
    let url_cancel = format!("{}/chat/completions", mock_cancel.url());
    let (hook_tx, hook_rx) = std::sync::mpsc::channel::<()>();
    let gate_cancel = Arc::new((Mutex::new(false), Condvar::new()));
    let gate_cancel_guard = GateGuard(gate_cancel.clone());
    let _before_send_cancel = {
        let gate = gate_cancel.clone();
        let hook_tx = Mutex::new(Some(hook_tx));
        sparkles::outbound::probe::BeforeSendHook::install(url_cancel, move || {
            if let Some(tx) = hook_tx.lock().take() {
                let _ = tx.send(());
                wait_gate(&gate, Duration::from_secs(10));
            }
        })
    };
    let m_cancel = Arc::new(models(
        json!({ "p": { "kind": "openai", "endpoint": mock_cancel.url(), "requestTimeoutSecs": 5 } }),
        &[],
    ));
    let settled_cancel = Arc::new(Mutex::new(Vec::new()));
    let cancel = Arc::new(AtomicBool::new(false));
    let h_cancel = {
        let m = m_cancel.clone();
        let settled = settled_cancel.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let admit = TrackAdmit(settled);
            m.call_prompt_with_cancel(
                Some(Role::Draft),
                &Pair::new("p", "m1"),
                &Prompt::plain("system text", "user text"),
                &draft_schema(),
                Instant::now() + Duration::from_secs(5),
                Some(&admit),
                Some(&cancel),
            )
        })
    };
    hook_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("worker should reach pre-send hook after admission and setup");
    cancel.store(true, Ordering::Relaxed);
    let err = h_cancel.join().unwrap().unwrap_err();
    assert_eq!(err.error.code(), "cancelled", "{:?}", err.error);
    gate_cancel_guard.open();
    let start = Instant::now();
    while settled_cancel.lock().is_empty() && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        mock_cancel.requests().len(),
        0,
        "cancellation during HTTP setup must prevent POST"
    );
    assert_eq!(
        *settled_cancel.lock(),
        vec![Some(Usage::default())],
        "ticket aborted before send must settle as not_sent"
    );

    // 2. Deadline expiry during HTTP setup (after admission, request construction, and header hook).
    let mock_dl = MockModel::start(|_, _| (200, mock::openai(r#"{"query":"ASK {}"}"#)));
    let url_dl = format!("{}/chat/completions", mock_dl.url());
    let (dl_tx, dl_rx) = std::sync::mpsc::channel::<()>();
    let gate_dl = Arc::new((Mutex::new(false), Condvar::new()));
    let gate_dl_guard = GateGuard(gate_dl.clone());
    let _before_send_dl = {
        let gate = gate_dl.clone();
        let dl_tx = Mutex::new(Some(dl_tx));
        sparkles::outbound::probe::BeforeSendHook::install(url_dl, move || {
            if let Some(tx) = dl_tx.lock().take() {
                let _ = tx.send(());
                wait_gate(&gate, Duration::from_secs(10));
            }
        })
    };
    let m_dl = Arc::new(models(
        json!({ "p": { "kind": "openai", "endpoint": mock_dl.url(), "requestTimeoutSecs": 5 } }),
        &[],
    ));
    let settled_dl = Arc::new(Mutex::new(Vec::new()));
    let cancel_dl = Arc::new(AtomicBool::new(false));
    let h_dl = {
        let m = m_dl.clone();
        let settled = settled_dl.clone();
        let cancel = cancel_dl.clone();
        std::thread::spawn(move || {
            let admit = TrackAdmit(settled);
            m.call_prompt_with_cancel(
                Some(Role::Draft),
                &Pair::new("p", "m1"),
                &Prompt::plain("system text", "user text"),
                &draft_schema(),
                Instant::now() + Duration::from_millis(80),
                Some(&admit),
                Some(&cancel),
            )
        })
    };
    dl_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("worker should reach pre-send hook before deadline expires");
    let err_dl = h_dl.join().unwrap().unwrap_err();
    assert_eq!(err_dl.error.code(), "deadline", "{:?}", err_dl.error);
    gate_dl_guard.open();
    let start = Instant::now();
    while settled_dl.lock().is_empty() && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        mock_dl.requests().len(),
        0,
        "deadline expiry during HTTP setup must prevent POST"
    );
    assert_eq!(
        *settled_dl.lock(),
        vec![Some(Usage::default())],
        "ticket expired before send must settle as not_sent"
    );
}

#[test]
fn completed_response_observed_after_deadline_fails_and_settles_usage() {
    use super::admission::{Admission, Attempt, Settle, Ticket, Usage};

    struct RecordSettle(Arc<Mutex<Vec<Option<Usage>>>>);
    impl Settle for RecordSettle {
        fn reported(self: Box<Self>, usage: Usage) {
            self.0.lock().push(Some(usage));
        }
        fn unknown(self: Box<Self>) {
            self.0.lock().push(None);
        }
    }
    struct TrackAdmit(Arc<Mutex<Vec<Option<Usage>>>>);
    impl Admission for TrackAdmit {
        fn admit(&self, _: &Attempt<'_>) -> Result<Ticket, CallError> {
            Ok(Ticket::new(Box::new(RecordSettle(self.0.clone()))))
        }
    }

    let mock = MockModel::start(|_, _| {
        (
            200,
            json!({
                "choices": [{
                    "message": { "content": r#"{"query":"ASK {}"}"# },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 45, "completion_tokens": 15 }
            }),
        )
    });
    let url = format!("{}/chat/completions", mock.url());
    let _consume_delay = super::client::test_hooks::BeforeConsumeHook::install(url, || {
        std::thread::sleep(Duration::from_millis(120));
    });
    let m = models(
        json!({ "p": { "kind": "openai", "endpoint": mock.url(), "requestTimeoutSecs": 5, "budget": { "tokensPerDay": 500 } } }),
        &[],
    );
    let settled = Arc::new(Mutex::new(Vec::new()));
    let admit = TrackAdmit(settled.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    let err = m
        .call_prompt_with_cancel(
            Some(Role::Draft),
            &Pair::new("p", "m1"),
            &Prompt::plain("system text", "user text"),
            &draft_schema(),
            Instant::now() + Duration::from_millis(60),
            Some(&admit),
            Some(&cancel),
        )
        .unwrap_err();
    assert_eq!(err.error.code(), "deadline", "{:?}", err.error);
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(
        *settled.lock(),
        vec![Some(Usage {
            input: 45,
            output: 15,
            ..Default::default()
        })],
        "completed response rejected after deadline must still settle admission ticket"
    );
    assert_eq!(
        m.describe()["providers"][0]["budget"]["usedToday"],
        60,
        "completed response rejected after deadline must still charge provider daily budget"
    );
}

#[test]
fn late_abandoned_usage_rounds_cache_reads_once_per_step() {
    use super::admission::Usage;

    let (retry_tx, retry_rx) = std::sync::mpsc::channel::<()>();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let gate_guard = GateGuard(gate.clone());
    let mock = {
        let gate = gate.clone();
        let retry_tx = Mutex::new(Some(retry_tx));
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
            if let Some(tx) = retry_tx.lock().take() {
                let _ = tx.send(());
                wait_gate(&gate, Duration::from_secs(10));
            }
            (
                200,
                json!({
                    "choices": [{
                        "message": { "content": r#"{"query":"ASK {}"}"# },
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
    let m = Arc::new(models(
        json!({
            "p": {
                "kind": "openai",
                "endpoint": mock.url(),
                "requestTimeoutSecs": 5,
                "budget": { "tokensPerDay": 500 },
                "pricing": { "inputPerMTok": 10.0, "outputPerMTok": 20.0, "cacheReadPerMTok": 1.0 }
            }
        }),
        &[],
    ));
    let cancel = Arc::new(AtomicBool::new(false));
    let (abandoned_tx, abandoned_rx) = std::sync::mpsc::channel::<(Usage, u64, Option<f64>)>();
    let h = {
        let m = m.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let cb = Arc::new(move |u: Usage, budget: u64, cost: Option<f64>| {
                let _ = abandoned_tx.send((u, budget, cost));
            });
            m.call_prompt_with_cancel_and_abandoned(
                Some(Role::Draft),
                &Pair::new("p", "m1"),
                &Prompt::plain("system text", "user text"),
                &draft_schema(),
                Instant::now() + Duration::from_secs(5),
                None,
                Some(&cancel),
                Some(cb),
            )
        })
    };
    retry_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("retry attempt should reach mock");
    cancel.store(true, Ordering::Relaxed);
    let err = h.join().unwrap().unwrap_err();
    assert_eq!(err.error.code(), "cancelled", "{:?}", err.error);
    assert_eq!(err.record.cache_read_tokens, 5);
    assert_eq!(err.record.budget_tokens(), 1);
    assert_eq!(m.describe()["providers"][0]["budget"]["usedToday"], 1);

    gate_guard.open();
    let (late_usage, delta_budget, late_cost) = abandoned_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("abandoned retry should report late usage");
    assert_eq!(late_usage.cache_read, 5);
    assert_eq!(
        delta_budget, 0,
        "10 cumulative cache-read tokens in one step must round once to 1 budget token"
    );
    assert!(
        late_cost.is_some_and(|c| c > 0.0),
        "raw cost for abandoned tokens must be preserved: {late_cost:?}"
    );
    assert_eq!(
        m.describe()["providers"][0]["budget"]["usedToday"],
        1,
        "provider daily budget must count 1 token, not 2"
    );
}

#[test]
fn large_request_timeout_clamped_to_caller_deadline_without_overflow() {
    let mock = MockModel::start(|_, _| (200, mock::openai(r#"{"query":"ASK {}"}"#)));
    let m = models(
        json!({
            "p1": { "kind": "openai", "endpoint": mock.url(), "requestTimeoutSecs": 1e19 },
            "p2": { "kind": "openai", "endpoint": mock.url(), "requestTimeoutSecs": 1e300 }
        }),
        &[],
    );
    call(&m, "p1").expect("1e19 requestTimeoutSecs must clamp to caller deadline without overflow");
    call(&m, "p2").expect("1e300 requestTimeoutSecs must clamp to caller deadline without panic");
    assert_eq!(mock.requests().len(), 2);
}
