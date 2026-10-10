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
    assert_eq!(v["providers"][1]["apiKey"], json!({"secret": "missing"}));
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
