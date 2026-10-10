//! One model call to one provider, in the protocol of its kind (§3.4, §3.6): Ollama's
//! `POST /api/chat`, the OpenAI protocol's `POST /chat/completions` and Anthropic's
//! `POST /v1/messages`. Requests go through the server's outbound policy. The key is
//! read from its named secret for each request and never enters a log line, an error
//! message or a response.

use super::config::{DEFAULT_ANTHROPIC_VERSION, Kind, Level, ProviderConfig};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// One message of a conversation.
#[derive(Clone, Debug)]
pub struct Message {
    /// `user` or `assistant`
    pub role: &'static str,
    pub content: String,
}

impl Message {
    pub fn user(s: impl Into<String>) -> Message {
        Message {
            role: "user",
            content: s.into(),
        }
    }

    pub fn assistant(s: impl Into<String>) -> Message {
        Message {
            role: "assistant",
            content: s.into(),
        }
    }
}

/// What one call asks for.
#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    /// the JSON Schema the answer must match, with a name for the protocols that need
    /// one; ignored at the `text` level
    pub schema: Option<(String, Value)>,
    /// a concrete level, never `auto`
    pub level: Level,
    pub max_output_tokens: u32,
    /// `None`: not sent
    pub temperature: Option<f64>,
    pub num_ctx: Option<u64>,
    pub timeout: Duration,
}

/// What a provider answered.
#[derive(Clone, Debug, Default)]
pub struct ChatResponse {
    /// the text blocks of the answer
    pub text: String,
    /// the input of a forced tool call (the `tool` level)
    pub tool_input: Option<Value>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// the provider's reason for stopping, as it gave it
    pub stop: Option<String>,
    pub latency: Duration,
}

/// Why a call produced no answer.
#[derive(Clone, Debug, PartialEq)]
pub enum CallError {
    /// the network failed, the call timed out, or the provider answered `429` or `5xx`
    /// (after the client's own retries)
    Unavailable(String),
    /// the outbound policy refuses the endpoint
    Refused(String),
    /// `401` or `403`: the key is missing, wrong or not allowed
    Auth(String),
    /// another `4xx`: the provider refused the request as built, such as a
    /// `response_format` it does not support
    Rejected(u16, String),
    /// the model declined to answer (`stop_reason: "refusal"`, `content_filter` or a
    /// `refusal` member)
    Refusal(String),
    /// the answer is not a response of the protocol
    Invalid(String),
    /// the named secret is not defined, or cannot be read
    Secret(String),
    /// the provider's `tls.caCert` cannot be read or holds no certificate
    Tls(String),
}

impl CallError {
    pub fn code(&self) -> &'static str {
        match self {
            CallError::Unavailable(_) => "provider-unavailable",
            CallError::Refused(_) => "outbound-refused",
            CallError::Auth(_) => "provider-auth",
            CallError::Rejected(..) => "provider-rejected",
            CallError::Refusal(_) => "refusal",
            CallError::Invalid(_) => "invalid-output",
            CallError::Secret(_) => "secret-missing",
            CallError::Tls(_) => "tls-config",
        }
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Unavailable(m)
            | CallError::Refused(m)
            | CallError::Auth(m)
            | CallError::Refusal(m)
            | CallError::Invalid(m)
            | CallError::Secret(m)
            | CallError::Tls(m) => f.write_str(m),
            CallError::Rejected(s, m) => write!(f, "{m} (status {s})"),
        }
    }
}

/// How the client reaches the network: the outbound policy, and the key of the call.
pub struct Transport<'a> {
    pub outbound: &'a sparkles::outbound::OutboundPolicy,
    /// the key, read from its secret for this call
    pub key: Option<String>,
}

/// The request body and URL of a call, for its provider's kind.
pub fn build(p: &ProviderConfig, req: &ChatRequest) -> (String, Value) {
    let base = p.endpoint.trim_end_matches('/');
    match p.kind {
        Kind::Ollama => {
            let mut messages = vec![json!({"role": "system", "content": req.system})];
            messages.extend(
                req.messages
                    .iter()
                    .map(|m| json!({"role": m.role, "content": m.content})),
            );
            let mut options = json!({ "num_predict": req.max_output_tokens });
            if let Some(t) = req.temperature {
                options["temperature"] = json!(t);
            }
            if let Some(n) = req.num_ctx {
                options["num_ctx"] = json!(n);
            }
            let mut body = json!({
                "model": req.model,
                "messages": messages,
                "stream": false,
                "options": options,
            });
            if let Some(k) = &p.keep_alive {
                body["keep_alive"] = json!(k);
            }
            match (req.level, &req.schema) {
                (Level::JsonSchema, Some((_, s))) => body["format"] = s.clone(),
                (Level::JsonObject, _) | (Level::JsonSchema, None) => {
                    body["format"] = json!("json")
                }
                _ => {}
            }
            (format!("{base}/api/chat"), body)
        }
        Kind::Openai => {
            let mut messages = vec![json!({"role": "system", "content": req.system})];
            messages.extend(
                req.messages
                    .iter()
                    .map(|m| json!({"role": m.role, "content": m.content})),
            );
            let mut body = json!({
                "model": req.model,
                "messages": messages,
                "max_tokens": req.max_output_tokens,
            });
            if let Some(t) = req.temperature {
                body["temperature"] = json!(t);
            }
            match (req.level, &req.schema) {
                (Level::JsonSchema, Some((name, s))) => {
                    body["response_format"] = json!({
                        "type": "json_schema",
                        "json_schema": { "name": name, "schema": s, "strict": true },
                    });
                }
                (Level::JsonObject, _) | (Level::JsonSchema, None) => {
                    body["response_format"] = json!({ "type": "json_object" });
                }
                (Level::Tool, Some((name, s))) => {
                    body["tools"] = json!([{
                        "type": "function",
                        "function": { "name": name, "description": "Give the answer.", "parameters": s },
                    }]);
                    body["tool_choice"] =
                        json!({ "type": "function", "function": { "name": name } });
                }
                _ => {}
            }
            (format!("{base}/chat/completions"), body)
        }
        Kind::Anthropic => {
            let messages: Vec<Value> = req
                .messages
                .iter()
                .map(|m| json!({"role": m.role, "content": m.content}))
                .collect();
            let mut body = json!({
                "model": req.model,
                "max_tokens": req.max_output_tokens,
                "system": req.system,
                "messages": messages,
            });
            if let Some(t) = req.temperature {
                body["temperature"] = json!(t);
            }
            match (req.level, &req.schema) {
                (Level::JsonSchema, Some((_, s))) => {
                    body["output_config"] =
                        json!({ "format": { "type": "json_schema", "schema": s } });
                }
                (Level::Tool, Some((name, s))) => {
                    body["tools"] = json!([{
                        "name": name, "description": "Give the answer.", "input_schema": s,
                    }]);
                    body["tool_choice"] = json!({ "type": "tool", "name": name });
                }
                _ => {}
            }
            (format!("{base}/v1/messages"), body)
        }
    }
}

/// The answer of a successful (`2xx`) response.
pub fn parse(kind: Kind, body: &[u8]) -> Result<ChatResponse, CallError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| CallError::Invalid("the provider's answer is not JSON".into()))?;
    let n = |x: &Value| x.as_u64().unwrap_or(0);
    match kind {
        Kind::Ollama => {
            let text = v["message"]["content"]
                .as_str()
                .ok_or_else(|| CallError::Invalid("the answer has no message content".into()))?;
            Ok(ChatResponse {
                text: text.to_string(),
                tool_input: None,
                input_tokens: n(&v["prompt_eval_count"]),
                output_tokens: n(&v["eval_count"]),
                stop: v["done_reason"].as_str().map(str::to_string),
                latency: Duration::ZERO,
            })
        }
        Kind::Openai => {
            let choice = &v["choices"][0];
            if choice.is_null() {
                return Err(CallError::Invalid("the answer has no choices".into()));
            }
            let msg = &choice["message"];
            let finish = choice["finish_reason"].as_str().map(str::to_string);
            if let Some(r) = msg["refusal"].as_str().filter(|r| !r.is_empty()) {
                return Err(CallError::Refusal(format!(
                    "the model refused: {}",
                    cut(r, 200)
                )));
            }
            if finish.as_deref() == Some("content_filter") {
                return Err(CallError::Refusal(
                    "the provider's content filter stopped the answer".into(),
                ));
            }
            let tool_input = match msg["tool_calls"][0]["function"]["arguments"].as_str() {
                Some(a) => Some(serde_json::from_str(a).map_err(|_| {
                    CallError::Invalid("the tool call's arguments are not JSON".into())
                })?),
                None => None,
            };
            Ok(ChatResponse {
                text: msg["content"].as_str().unwrap_or("").to_string(),
                tool_input,
                input_tokens: n(&v["usage"]["prompt_tokens"]),
                output_tokens: n(&v["usage"]["completion_tokens"]),
                stop: finish,
                latency: Duration::ZERO,
            })
        }
        Kind::Anthropic => {
            let stop = v["stop_reason"].as_str().map(str::to_string);
            if stop.as_deref() == Some("refusal") {
                return Err(CallError::Refusal("the model refused to answer".into()));
            }
            let blocks = v["content"]
                .as_array()
                .ok_or_else(|| CallError::Invalid("the answer has no content".into()))?;
            let mut text = String::new();
            let mut tool_input = None;
            for b in blocks {
                match b["type"].as_str() {
                    Some("text") => text.push_str(b["text"].as_str().unwrap_or("")),
                    Some("tool_use") if tool_input.is_none() => {
                        tool_input = Some(b["input"].clone())
                    }
                    _ => {}
                }
            }
            Ok(ChatResponse {
                text,
                tool_input,
                input_tokens: n(&v["usage"]["input_tokens"]),
                output_tokens: n(&v["usage"]["output_tokens"]),
                stop,
                latency: Duration::ZERO,
            })
        }
    }
}

/// The headers of a call: the key in the kind's header, and the kind's own.
fn headers(p: &ProviderConfig, key: Option<&str>) -> Vec<(String, String)> {
    let mut h = Vec::new();
    match p.kind {
        Kind::Ollama => {
            if let Some(k) = key {
                h.push(("Authorization".into(), format!("Bearer {k}")));
            }
        }
        Kind::Openai => {
            if let Some(k) = key {
                h.push(("Authorization".into(), format!("Bearer {k}")));
            }
            for (k, v) in &p.headers {
                h.push((k.clone(), v.clone()));
            }
        }
        Kind::Anthropic => {
            if let Some(k) = key {
                h.push(("x-api-key".into(), k.to_string()));
            }
            h.push((
                "anthropic-version".into(),
                p.version
                    .clone()
                    .unwrap_or_else(|| DEFAULT_ANTHROPIC_VERSION.into()),
            ));
        }
    }
    h
}

/// The first `n` characters of `s`.
pub(crate) fn cut(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// `s` without `key` in it.
fn scrub(s: String, key: Option<&str>) -> String {
    match key {
        Some(k) if k.len() >= 4 && s.contains(k) => s.replace(k, "[redacted]"),
        _ => s,
    }
}

/// The provider's own message from an error body, at most 300 characters.
fn provider_message(body: &[u8]) -> String {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return String::new();
    };
    let m = v["error"]["message"]
        .as_str()
        .or_else(|| v["error"].as_str())
        .or_else(|| v["message"].as_str())
        .unwrap_or("");
    if m.is_empty() {
        String::new()
    } else {
        format!(": {}", cut(m, 300))
    }
}

/// Retries of a `429` or `5xx` within the call's timeout.
const RETRIES: u32 = 2;

/// Send one call, retrying a `429` or `5xx` twice within its timeout (with the
/// provider's `Retry-After` when it is shorter than the time left).
pub fn send(
    t: &Transport<'_>,
    name: &str,
    p: &ProviderConfig,
    req: &ChatRequest,
) -> Result<ChatResponse, CallError> {
    let (url, body) = build(p, req);
    let hs = headers(p, t.key.as_deref());
    let hrefs: Vec<(&str, &str)> = hs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let bytes = serde_json::to_vec(&body).expect("serializable");
    let started = Instant::now();
    let key = t.key.as_deref();
    let mut attempt = 0;
    loop {
        let left = req.timeout.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(CallError::Unavailable(format!(
                "provider {name} did not answer within {:.0} s",
                req.timeout.as_secs_f64()
            )));
        }
        let posted = sparkles::outbound::post_json(t.outbound, &url, &hrefs, bytes.clone(), left)
            .map_err(|f| match f {
            sparkles::outbound::Failure::Refused(m) => CallError::Refused(scrub(
                format!("provider {name}: the outbound policy refuses {m}"),
                key,
            )),
            f => CallError::Unavailable(scrub(format!("provider {name}: {f}"), key)),
        })?;
        let status = posted.status.as_u16();
        if (200..300).contains(&status) {
            let mut r = parse(p.kind, &posted.body).map_err(|e| match e {
                CallError::Invalid(m) => CallError::Invalid(format!("provider {name}: {m}")),
                e => e,
            })?;
            r.latency = started.elapsed();
            return Ok(r);
        }
        let m = scrub(
            format!(
                "provider {name} answered {}{}",
                posted.status,
                provider_message(&posted.body)
            ),
            key,
        );
        match status {
            401 | 403 => return Err(CallError::Auth(m)),
            408 | 409 | 425 | 429 | 500..=599 => {
                if attempt >= RETRIES {
                    return Err(CallError::Unavailable(m));
                }
                let wait = posted
                    .retry_after
                    .as_deref()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map_or(Duration::from_millis(250 << attempt), Duration::from_secs);
                if wait >= req.timeout.saturating_sub(started.elapsed()) {
                    return Err(CallError::Unavailable(m));
                }
                tracing::info!(target: "sparkles::models", "retrying in {wait:?}: {m}");
                std::thread::sleep(wait);
                attempt += 1;
            }
            _ => return Err(CallError::Rejected(status, m)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(kind: Kind) -> ProviderConfig {
        serde_json::from_value(json!({ "kind": kind.as_str(), "endpoint": "http://h/v1/" }))
            .unwrap()
    }

    fn req(level: Level) -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            system: "sys".into(),
            messages: vec![Message::user("q")],
            schema: Some(("Draft".into(), json!({"type": "object"}))),
            level,
            max_output_tokens: 100,
            temperature: Some(0.0),
            num_ctx: None,
            timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn bodies_by_kind() {
        let (u, b) = build(&provider(Kind::Ollama), &req(Level::JsonSchema));
        assert_eq!(u, "http://h/v1/api/chat");
        assert_eq!(b["format"], json!({"type": "object"}));
        assert_eq!(b["stream"], json!(false));
        let (_, b) = build(&provider(Kind::Ollama), &req(Level::JsonObject));
        assert_eq!(b["format"], json!("json"));
        let (_, b) = build(&provider(Kind::Ollama), &req(Level::Text));
        assert!(b.get("format").is_none());

        let (u, b) = build(&provider(Kind::Openai), &req(Level::JsonSchema));
        assert_eq!(u, "http://h/v1/chat/completions");
        assert_eq!(b["response_format"]["type"], "json_schema");
        assert_eq!(b["response_format"]["json_schema"]["strict"], true);
        assert_eq!(b["messages"][0]["role"], "system");
        let (_, b) = build(&provider(Kind::Openai), &req(Level::Tool));
        assert_eq!(b["tool_choice"]["function"]["name"], "Draft");

        let (u, b) = build(&provider(Kind::Anthropic), &req(Level::JsonSchema));
        assert_eq!(u, "http://h/v1/v1/messages");
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        assert_eq!(b["system"], "sys");
        let (_, b) = build(&provider(Kind::Anthropic), &req(Level::Tool));
        assert_eq!(b["tool_choice"]["type"], "tool");
        assert_eq!(
            headers(&provider(Kind::Anthropic), Some("k"))[1].0,
            "anthropic-version"
        );
    }

    #[test]
    fn answers_by_kind() {
        let r = parse(
            Kind::Ollama,
            br#"{"message":{"content":"{}"},"prompt_eval_count":3,"eval_count":4}"#,
        )
        .unwrap();
        assert_eq!(
            (r.text.as_str(), r.input_tokens, r.output_tokens),
            ("{}", 3, 4)
        );
        let r = parse(
            Kind::Openai,
            br#"{"choices":[{"message":{"content":"x"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2}}"#,
        )
        .unwrap();
        assert_eq!(r.text, "x");
        assert!(matches!(
            parse(
                Kind::Openai,
                br#"{"choices":[{"message":{"content":null,"refusal":"no"}}]}"#
            ),
            Err(CallError::Refusal(_))
        ));
        assert!(matches!(
            parse(
                Kind::Openai,
                br#"{"choices":[{"message":{"content":""},"finish_reason":"content_filter"}]}"#
            ),
            Err(CallError::Refusal(_))
        ));
        let r = parse(
            Kind::Openai,
            br#"{"choices":[{"message":{"tool_calls":[{"function":{"name":"D","arguments":"{\"a\":1}"}}]}}]}"#,
        )
        .unwrap();
        assert_eq!(r.tool_input, Some(json!({"a": 1})));
        let r = parse(
            Kind::Anthropic,
            br#"{"content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":6}}"#,
        )
        .unwrap();
        assert_eq!((r.text.as_str(), r.input_tokens), ("hi", 5));
        assert!(matches!(
            parse(
                Kind::Anthropic,
                br#"{"content":[],"stop_reason":"refusal"}"#
            ),
            Err(CallError::Refusal(_))
        ));
        assert!(matches!(
            parse(Kind::Anthropic, b"<html>"),
            Err(CallError::Invalid(_))
        ));
    }

    #[test]
    fn keys_are_scrubbed() {
        assert_eq!(
            scrub("bad key sk-secret-123".into(), Some("sk-secret-123")),
            "bad key [redacted]"
        );
    }
}
