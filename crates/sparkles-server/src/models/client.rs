//! One model call to one provider, in the protocol of its kind (§3.4, §3.6): Ollama's
//! `POST /api/chat`, the OpenAI protocol's `POST /chat/completions` and Anthropic's
//! `POST /v1/messages`. Requests go through the server's outbound policy. The key is
//! read from its named secret for each request and never enters a log line, an error
//! message or a response.

use super::admission::Usage;
use super::config::{CacheTtl, DEFAULT_ANTHROPIC_VERSION, Kind, Level, ProviderConfig};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// A run of prompt text. A part marked `cache` ends a prefix that a provider with prompt
/// caching keeps for the requests after it (spec C18 §3.6). The provider reads the parts
/// of a system text or a message joined, so the marks only say where a reusable prefix
/// ends and change nothing that the model is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub text: String,
    pub cache: bool,
}

impl Part {
    /// A part that ends no cached prefix.
    pub fn text(s: impl Into<String>) -> Part {
        Part {
            text: s.into(),
            cache: false,
        }
    }

    /// A part that ends a cached prefix.
    pub fn cached(s: impl Into<String>) -> Part {
        Part {
            text: s.into(),
            cache: true,
        }
    }
}

/// The parts joined.
pub fn joined(parts: &[Part]) -> String {
    parts.iter().map(|p| p.text.as_str()).collect()
}

/// One message of a conversation.
#[derive(Clone, Debug)]
pub struct Message {
    /// `user` or `assistant`
    pub role: &'static str,
    pub parts: Vec<Part>,
}

impl Message {
    pub fn user(s: impl Into<String>) -> Message {
        Message::user_parts(vec![Part::text(s)])
    }

    pub fn user_parts(parts: Vec<Part>) -> Message {
        Message {
            role: "user",
            parts,
        }
    }

    pub fn assistant(s: impl Into<String>) -> Message {
        Message {
            role: "assistant",
            parts: vec![Part::text(s)],
        }
    }

    /// The message's text, its parts joined.
    pub fn text(&self) -> String {
        joined(&self.parts)
    }
}

/// What one call asks for.
#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub model: String,
    pub system: Vec<Part>,
    pub messages: Vec<Message>,
    /// the JSON Schema the answer must match, with a name for the protocols that need
    /// one; ignored at the `text` level
    pub schema: Option<(String, Value)>,
    /// a concrete level, never `auto`
    pub level: Level,
    pub max_output_tokens: u32,
    /// `None`: not sent
    pub temperature: Option<f64>,
    /// `None`: not sent, and an openai request limits its output with `max_tokens`
    pub reasoning_effort: Option<super::config::Effort>,
    pub num_ctx: Option<u64>,
    pub timeout: Duration,
}

/// What a provider answered.
#[derive(Clone, Debug, Default)]
pub struct ChatResponse {
    /// the text blocks of the answer
    pub text: String,
    /// the input of a tool call (the `tool` level)
    pub tool_input: Option<Value>,
    /// the tokens the provider reported
    pub usage: super::admission::Usage,
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
    /// the caller's admission refused an attempt before it was sent (spec C22 §3.5),
    /// with its code, and whether an earlier attempt of the same request was sent
    Admission(&'static str, String, bool),
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
            CallError::Admission(code, ..) => code,
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
            | CallError::Tls(m)
            | CallError::Admission(_, m, _) => f.write_str(m),
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

/// The most cache breakpoints one Anthropic request may carry.
pub const MAX_BREAKPOINTS: usize = 4;

/// The TTL of the prefixes that `req` asks `p` to cache: the provider's prompt caching
/// when it is on and the request marks at least one part, else `None`.
pub fn cache_ttl(p: &ProviderConfig, req: &ChatRequest) -> Option<CacheTtl> {
    let marked = req
        .system
        .iter()
        .chain(req.messages.iter().flat_map(|m| &m.parts))
        .any(|x| x.cache);
    p.cache_ttl().filter(|_| marked)
}

/// The marks a request keeps: the last [`MAX_BREAKPOINTS`] marked parts, in order of
/// the prompt (the system text, then each message), as (message, part) with `None` for
/// the system text. A later breakpoint covers a longer prefix, so the earliest marks
/// are the ones dropped.
fn kept_marks(req: &ChatRequest) -> Vec<(Option<usize>, usize)> {
    let mut marks: Vec<(Option<usize>, usize)> = req
        .system
        .iter()
        .enumerate()
        .filter(|(_, x)| x.cache)
        .map(|(i, _)| (None, i))
        .collect();
    for (m, msg) in req.messages.iter().enumerate() {
        marks.extend(
            msg.parts
                .iter()
                .enumerate()
                .filter(|(_, x)| x.cache)
                .map(|(i, _)| (Some(m), i)),
        );
    }
    let drop = marks.len().saturating_sub(MAX_BREAKPOINTS);
    marks.split_off(drop)
}

/// The `cache_control` member of a breakpoint.
fn cache_control(ttl: CacheTtl) -> Value {
    match ttl {
        CacheTtl::FiveMinutes => json!({ "type": "ephemeral" }),
        CacheTtl::OneHour => json!({ "type": "ephemeral", "ttl": "1h" }),
    }
}

/// The Anthropic content of `parts`: one string when no breakpoint falls in them, as
/// without caching, else text blocks that end at each kept breakpoint, with the rest
/// in a last block. Joined, the blocks hold the same text as the string.
fn anthropic_content(parts: &[Part], kept: &[usize], ttl: CacheTtl) -> Value {
    if kept.is_empty() {
        return Value::String(joined(parts));
    }
    let mut blocks = Vec::new();
    let mut text = String::new();
    for (i, x) in parts.iter().enumerate() {
        text.push_str(&x.text);
        // a block must hold text, so a breakpoint on an empty run is dropped
        if kept.contains(&i) && !text.is_empty() {
            blocks.push(json!({
                "type": "text",
                "text": std::mem::take(&mut text),
                "cache_control": cache_control(ttl),
            }));
        }
    }
    if !text.is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
    Value::Array(blocks)
}

/// The request body and URL of a call, for its provider's kind.
pub fn build(p: &ProviderConfig, req: &ChatRequest) -> (String, Value) {
    let base = p.endpoint.trim_end_matches('/');
    let system = joined(&req.system);
    match p.kind {
        Kind::Ollama => {
            let mut messages = vec![json!({"role": "system", "content": system})];
            messages.extend(
                req.messages
                    .iter()
                    .map(|m| json!({"role": m.role, "content": m.text()})),
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
            let mut messages = vec![json!({"role": "system", "content": system})];
            messages.extend(
                req.messages
                    .iter()
                    .map(|m| json!({"role": m.role, "content": m.text()})),
            );
            let mut body = json!({ "model": req.model, "messages": messages });
            // a reasoning model counts its reasoning in max_completion_tokens and
            // refuses max_tokens
            match req.reasoning_effort {
                Some(e) => {
                    body["max_completion_tokens"] = json!(req.max_output_tokens);
                    body["reasoning_effort"] = json!(e.as_str());
                }
                None => body["max_tokens"] = json!(req.max_output_tokens),
            }
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
            let ttl = cache_ttl(p, req);
            let kept = if ttl.is_some() {
                kept_marks(req)
            } else {
                Vec::new()
            };
            let ttl = ttl.unwrap_or_default();
            let marks_of = |at: Option<usize>| -> Vec<usize> {
                kept.iter()
                    .filter(|(m, _)| *m == at)
                    .map(|(_, i)| *i)
                    .collect()
            };
            let messages: Vec<Value> = req
                .messages
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    json!({
                        "role": m.role,
                        "content": anthropic_content(&m.parts, &marks_of(Some(i)), ttl),
                    })
                })
                .collect();
            let mut body = json!({
                "model": req.model,
                "max_tokens": req.max_output_tokens,
                "system": anthropic_content(&req.system, &marks_of(None), ttl),
                "messages": messages,
            });
            if let Some(t) = req.temperature {
                body["temperature"] = json!(t);
            }
            match (req.level, &req.schema) {
                (Level::JsonSchema, Some((_, s))) => {
                    body["output_config"] = json!({
                        "format": { "type": "json_schema", "schema": anthropic_schema(s) }
                    });
                }
                (Level::Tool, Some((name, s))) => {
                    // Claude Opus 5.5, Sonnet 5.5 and Fable 5.1 answer a forced tool
                    // choice with a 400, so the model may call the strict tool, and an
                    // answer in text is read and validated like any other
                    body["tools"] = json!([{
                        "name": name,
                        "description": "Give the answer.",
                        "input_schema": anthropic_schema(s),
                        "strict": true,
                    }]);
                    body["tool_choice"] = json!({ "type": "auto" });
                }
                _ => {}
            }
            if let Some(e) = req.reasoning_effort {
                body["output_config"]["effort"] = json!(e.as_str());
            }
            (format!("{base}/v1/messages"), body)
        }
        // `Models::call` refuses local providers before building a request
        Kind::Local => (String::new(), Value::Null),
    }
}

/// The usage a response body reports, and whether it reported every member a
/// settlement needs. An OpenAI answer counts its cache reads (`cached_tokens`) and
/// writes (`cache_write_tokens`) inside `prompt_tokens`, so they are taken out of the
/// uncached input, and its writes, which keep a prefix for 30 minutes at 1.25 times
/// the input price, are counted in the shorter-TTL class. An Anthropic answer reports its cache writes apart from its
/// uncached input, split by TTL. A write the answer does not split is taken at `ttl`,
/// the TTL of every breakpoint of the request, and a split that does not add up, or a
/// write without a known TTL, is taken at the dearer 1-hour rate.
fn read_usage(kind: Kind, v: &Value, ttl: Option<CacheTtl>) -> (Usage, bool) {
    let (i, o) = match kind {
        Kind::Ollama => (&v["prompt_eval_count"], &v["eval_count"]),
        Kind::Openai => (
            &v["usage"]["prompt_tokens"],
            &v["usage"]["completion_tokens"],
        ),
        Kind::Anthropic => (&v["usage"]["input_tokens"], &v["usage"]["output_tokens"]),
        Kind::Local => return (Usage::default(), false),
    };
    let mut complete = i.as_u64().is_some() && o.as_u64().is_some();
    let mut u = Usage {
        input: i.as_u64().unwrap_or(0),
        output: o.as_u64().unwrap_or(0),
        ..Default::default()
    };
    if kind == Kind::Openai {
        let d = &v["usage"]["prompt_tokens_details"];
        // a member that is present but not a count, details that are not an object,
        // and counts that pass the prompt leave the usage unknown
        let count = |k: &str| match d.get(k) {
            None | Some(Value::Null) => Some(0),
            Some(x) => x.as_u64(),
        };
        let split = if d.is_null() || d.is_object() {
            count("cached_tokens").zip(count("cache_write_tokens"))
        } else {
            None
        };
        let rest = split
            .and_then(|(read, written)| read.checked_add(written))
            .and_then(|cached| u.input.checked_sub(cached));
        match (split, rest) {
            (Some((read, written)), Some(rest)) => {
                (u.input, u.cache_read, u.cache_write_5m) = (rest, read, written);
            }
            _ => complete = false,
        }
    }
    if kind == Kind::Anthropic {
        let r = &v["usage"];
        // a cache member that is present, even as null, but not a count, a split that is
        // not an object, and a split whose sum overflows leave the usage unknown, so that
        // a settlement keeps its reservation
        let count = |o: &Value, k: &str| -> Result<Option<u64>, ()> {
            match o.get(k) {
                None => Ok(None),
                Some(x) => x.as_u64().map(Some).ok_or(()),
            }
        };
        let split = r.get("cache_creation");
        let read = (|| {
            let written = count(r, "cache_creation_input_tokens")?.unwrap_or(0);
            let (five, hour) = match split {
                None => (None, None),
                Some(o) if o.is_object() => (
                    count(o, "ephemeral_5m_input_tokens")?,
                    count(o, "ephemeral_1h_input_tokens")?,
                ),
                Some(_) => return Err(()),
            };
            let classes = match (five, hour) {
                (None, None) if ttl == Some(CacheTtl::FiveMinutes) => (written, 0),
                (None, None) => (0, written),
                (a, b) => {
                    let (a, b) = (a.unwrap_or(0), b.unwrap_or(0));
                    let sum = a.checked_add(b).ok_or(())?;
                    if sum == written {
                        (a, b)
                    } else {
                        (0, written.max(sum))
                    }
                }
            };
            Ok::<_, ()>((classes, count(r, "cache_read_input_tokens")?.unwrap_or(0)))
        })();
        match read {
            Ok(((five, hour), read)) => {
                (u.cache_write_5m, u.cache_write_1h, u.cache_read) = (five, hour, read);
            }
            Err(()) => complete = false,
        }
    }
    (u, complete)
}

/// The usage a response body reports, read apart from its content so that an answer
/// whose content is refused or malformed still settles what it used. `ttl` is the TTL
/// of the request's breakpoints ([`cache_ttl`]).
pub fn usage_of(kind: Kind, body: &[u8], ttl: Option<CacheTtl>) -> Option<Usage> {
    let v: Value = serde_json::from_slice(body).ok()?;
    match read_usage(kind, &v, ttl) {
        (u, true) => Some(u),
        _ => None,
    }
}

/// The answer of a successful (`2xx`) response.
pub fn parse(kind: Kind, body: &[u8], ttl: Option<CacheTtl>) -> Result<ChatResponse, CallError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| CallError::Invalid("the provider's answer is not JSON".into()))?;
    let usage = read_usage(kind, &v, ttl).0;
    match kind {
        Kind::Ollama => {
            let text = v["message"]["content"]
                .as_str()
                .ok_or_else(|| CallError::Invalid("the answer has no message content".into()))?;
            Ok(ChatResponse {
                text: text.to_string(),
                tool_input: None,
                usage,
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
                usage,
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
                usage,
                stop,
                latency: Duration::ZERO,
            })
        }
        Kind::Local => Err(CallError::Invalid(
            "a local provider computes embeddings and does not chat".into(),
        )),
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
        Kind::Local => {}
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

/// The URL and body of the provider's own token count of `req`, for the kinds that
/// offer one: Anthropic's `POST /v1/messages/count_tokens`, which takes the request's
/// model, system text, messages and tools without its output limit.
pub fn count_request(p: &ProviderConfig, req: &ChatRequest) -> Option<(String, Value)> {
    match p.kind {
        Kind::Anthropic => {
            let (url, mut body) = build(p, req);
            if let Some(o) = body.as_object_mut() {
                for k in ["max_tokens", "temperature"] {
                    o.remove(k);
                }
            }
            Some((format!("{url}/count_tokens"), body))
        }
        Kind::Ollama | Kind::Openai | Kind::Local => None,
    }
}

/// The provider's own token count of `req`, through the call's transport and key.
fn provider_count(
    t: &Transport<'_>,
    name: &str,
    p: &ProviderConfig,
    req: &ChatRequest,
    timeout: Duration,
) -> Result<u64, CallError> {
    if timeout.is_zero() {
        return Err(CallError::Unavailable(format!(
            "provider {name}: no time is left to count tokens"
        )));
    }
    let Some((url, body)) = count_request(p, req) else {
        return Err(CallError::Invalid(format!(
            "provider {name} has no token counting endpoint"
        )));
    };
    let hs = headers(p, t.key.as_deref());
    let hrefs: Vec<(&str, &str)> = hs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let bytes = body_bytes(&body);
    let key = t.key.as_deref();
    let posted = sparkles::outbound::post_json(t.outbound, &url, &hrefs, bytes, timeout)
        .map_err(|f| CallError::Unavailable(scrub(format!("provider {name}: {f}"), key)))?;
    if !(200..300).contains(&posted.status.as_u16()) {
        return Err(CallError::Unavailable(scrub(
            format!(
                "provider {name} answered {} to a token count",
                posted.status
            ),
            key,
        )));
    }
    let v: Value = serde_json::from_slice(&posted.body)
        .map_err(|_| CallError::Invalid("the token count is not JSON".into()))?;
    v["input_tokens"]
        .as_u64()
        .ok_or_else(|| CallError::Invalid("the token count has no input_tokens".into()))
}

/// Send one call, retrying a `429` or `5xx` twice within its timeout (with the
/// provider's `Retry-After` when it is shorter than the time left). With an
/// `admission`, every HTTP attempt, retries included, is admitted before it is sent,
/// and each admitted attempt is settled with the usage its answer reported.
pub fn send_admitted(
    t: &Transport<'_>,
    name: &str,
    p: &ProviderConfig,
    req: &ChatRequest,
    admission: Option<&dyn super::admission::Admission>,
) -> Result<ChatResponse, CallError> {
    let (url, body) = build(p, req);
    let ttl = cache_ttl(p, req);
    let hs = headers(p, t.key.as_deref());
    let hrefs: Vec<(&str, &str)> = hs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let bytes = body_bytes(&body);
    let started = Instant::now();
    let key = t.key.as_deref();
    // counting spends the request's own time
    let counter = || {
        provider_count(
            t,
            name,
            p,
            req,
            req.timeout.saturating_sub(started.elapsed()),
        )
    };
    let has_counter = count_request(p, req).is_some();
    // an admission that must see every send gets no redirects, each of which would
    // send the body again unadmitted
    let unredirected;
    let outbound = if admission.is_some_and(|a| !a.follows_redirects()) {
        unredirected = sparkles::outbound::OutboundPolicy {
            max_redirects: 0,
            ..t.outbound.clone()
        };
        &unredirected
    } else {
        t.outbound
    };
    let mut attempt = 0;
    loop {
        let timed_out = || {
            CallError::Unavailable(format!(
                "provider {name} did not answer within {:.0} s",
                req.timeout.as_secs_f64()
            ))
        };
        if req.timeout.saturating_sub(started.elapsed()).is_zero() {
            return Err(timed_out());
        }
        let ticket = match admission {
            None => None,
            Some(a) => Some(
                a.admit(&super::admission::Attempt {
                    provider: name,
                    config: p,
                    request: req,
                    url: &url,
                    body: &body,
                    bytes: &bytes,
                    cache_ttl: ttl,
                    provider_count: if has_counter { Some(&counter) } else { None },
                })
                .map_err(|e| match e {
                    CallError::Admission(c, m, _) => CallError::Admission(c, m, attempt > 0),
                    e => e,
                })?,
            ),
        };
        // admission may have spent time counting
        let left = req.timeout.saturating_sub(started.elapsed());
        if left.is_zero() {
            if let Some(tk) = ticket {
                tk.not_sent();
            }
            return Err(timed_out());
        }
        // a ticket dropped on any path below leaves its usage unknown
        let posted =
            match sparkles::outbound::post_json(outbound, &url, &hrefs, bytes.clone(), left) {
                Ok(posted) => posted,
                Err(sparkles::outbound::Failure::Refused(m)) => {
                    // a refusal can come from a redirect after the request was sent, so
                    // the reservation stays counted
                    drop(ticket);
                    return Err(CallError::Refused(scrub(
                        format!("provider {name}: the outbound policy refuses {m}"),
                        key,
                    )));
                }
                Err(f) => {
                    return Err(CallError::Unavailable(scrub(
                        format!("provider {name}: {f}"),
                        key,
                    )));
                }
            };
        // the usage settles the reservation whatever the content holds
        if let Some(tk) = ticket {
            tk.settle(usage_of(p.kind, &posted.body, ttl));
        }
        let status = posted.status.as_u16();
        if (200..300).contains(&status) {
            let mut r = parse(p.kind, &posted.body, ttl).map_err(|e| match e {
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

/// The keyword with which a schema object names the order of its properties.
pub const PROPERTY_ORDER: &str = "x-sparkles-order";

/// The JSON text of a request body. `serde_json` sorts the members of an object, and a
/// provider's constrained decoding writes an answer's members in the order its schema
/// lists them, so a schema object can name the order of its properties under
/// [`PROPERTY_ORDER`]. The properties it names come first in that order, the others follow
/// sorted, and the keyword itself is never sent.
pub fn body_bytes(body: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_body(body, None, &mut out);
    out
}

/// `members` is set when `v` is the `properties` map of a schema, with the order its
/// schema names, so that a property may itself be called like the keyword.
fn write_body(v: &Value, members: Option<&[Value]>, out: &mut Vec<u8>) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = Vec::with_capacity(m.len());
            for k in members.unwrap_or(&[]).iter().filter_map(Value::as_str) {
                if let Some((key, _)) = m.get_key_value(k)
                    && !keys.contains(&key)
                {
                    keys.push(key);
                }
            }
            for k in m.keys() {
                if (members.is_some() || k != PROPERTY_ORDER) && !keys.contains(&k) {
                    keys.push(k);
                }
            }
            let order = m
                .get(PROPERTY_ORDER)
                .and_then(Value::as_array)
                .map(Vec::as_slice);
            out.push(b'{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.extend(serde_json::to_vec(k).expect("a string serializes"));
                out.push(b':');
                let inner = (members.is_none() && k == "properties").then(|| order.unwrap_or(&[]));
                write_body(&m[k], inner, out);
            }
            out.push(b'}');
        }
        Value::Array(a) => {
            out.push(b'[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_body(x, None, out);
            }
            out.push(b']');
        }
        other => out.extend(serde_json::to_vec(other).expect("a scalar serializes")),
    }
}

/// A schema that Anthropic's structured output accepts. It refuses length, count and
/// numeric bounds with a 400, so they are left out of the request. Every answer is still
/// validated against the full schema (`structured::validate`), so a bound the model
/// passes costs a retry rather than a wrong value.
fn anthropic_schema(s: &Value) -> Value {
    const BOUNDS: [&str; 8] = [
        "maxItems",
        "maxLength",
        "minLength",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
    ];
    let Value::Object(m) = s else {
        return s.clone();
    };
    let mut out = serde_json::Map::new();
    for (k, v) in m {
        // only a minItems of 0 or 1 is accepted
        if BOUNDS.contains(&k.as_str())
            || (k.as_str() == "minItems" && v.as_u64().is_some_and(|n| n > 1))
        {
            continue;
        }
        let v = match (k.as_str(), v) {
            // the members are named schemas, so a member called `maximum` stays
            ("properties", Value::Object(p)) => Value::Object(
                p.iter()
                    .map(|(name, sub)| (name.clone(), anthropic_schema(sub)))
                    .collect(),
            ),
            ("items", sub) => anthropic_schema(sub),
            _ => v.clone(),
        };
        out.insert(k.clone(), v);
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A schema object's named order reaches the provider, the keyword does not, and a
    /// property may carry the keyword's name.
    #[test]
    fn request_bodies_keep_the_named_property_order() {
        let body = json!({
            "model": "m",
            "schema": {
                "type": "object",
                "x-sparkles-order": ["text", "score", "findings"],
                "properties": {
                    "findings": { "type": "array", "items": { "type": "object",
                        "x-sparkles-order": ["x-sparkles-order", "a"],
                        "properties": { "a": {}, "x-sparkles-order": {} } } },
                    "score": { "type": "integer" },
                    "text": { "type": "string" },
                    "extra": { "type": "string" }
                },
                "required": ["text", "score", "findings"]
            }
        });
        let text = String::from_utf8(body_bytes(&body)).unwrap();
        assert_eq!(
            text,
            r#"{"model":"m","schema":{"properties":{"text":{"type":"string"},"score":{"type":"integer"},"findings":{"items":{"properties":{"x-sparkles-order":{},"a":{}},"type":"object"},"type":"array"},"extra":{"type":"string"}},"required":["text","score","findings"],"type":"object"}}"#
        );
        // without the keyword the text is serde_json's own
        let plain =
            json!({ "b": [1, "two", null, 2.5, { "d": true, "c": false }], "a": "\u{1}\"" });
        assert_eq!(body_bytes(&plain), serde_json::to_vec(&plain).unwrap());
    }

    #[test]
    fn anthropic_schemas_leave_out_the_bounds_it_refuses() {
        let mut r = req(Level::JsonSchema);
        r.schema = Some((
            "Report".into(),
            json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "maxLength": 200 },
                    "findings": { "type": "array", "maxItems": 12, "minItems": 2,
                                  "items": { "type": "integer", "minimum": 0 } },
                    "tags": { "type": "array", "minItems": 1, "items": { "type": "string" } },
                    "rating": { "type": "integer", "minimum": 0, "maximum": 10 }
                },
                "required": ["title"],
                "additionalProperties": false
            }),
        ));
        let (_, b) = build(&provider(Kind::Anthropic), &r);
        assert_eq!(
            b["output_config"]["format"]["schema"],
            json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "findings": { "type": "array", "items": { "type": "integer" } },
                    "tags": { "type": "array", "minItems": 1, "items": { "type": "string" } },
                    "rating": { "type": "integer" }
                },
                "required": ["title"],
                "additionalProperties": false
            })
        );
        // a member named after a bound is a member, not a bound
        r.schema = Some((
            "M".into(),
            json!({ "type": "object",
                    "properties": { "maximum": { "type": "integer", "maximum": 3 } },
                    "required": ["maximum"], "additionalProperties": false }),
        ));
        let (_, b) = build(&provider(Kind::Anthropic), &r);
        assert_eq!(
            b["output_config"]["format"]["schema"]["properties"],
            json!({ "maximum": { "type": "integer" } })
        );
        r.schema = Some((
            "Report".into(),
            json!({ "type": "object", "properties": { "rating": { "type": "integer", "maximum": 10 } } }),
        ));
        // other kinds keep the schema as written
        let (_, b) = build(&provider(Kind::Openai), &r);
        assert_eq!(
            b["response_format"]["json_schema"]["schema"]["properties"]["rating"]["maximum"],
            10
        );
    }

    fn provider(kind: Kind) -> ProviderConfig {
        serde_json::from_value(json!({ "kind": kind.as_str(), "endpoint": "http://h/v1/" }))
            .unwrap()
    }

    fn req(level: Level) -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            system: vec![Part::cached("sys")],
            messages: vec![Message::user("q")],
            schema: Some(("Draft".into(), json!({"type": "object"}))),
            level,
            max_output_tokens: 100,
            temperature: Some(0.0),
            reasoning_effort: None,
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
        assert_eq!(
            b["system"],
            json!([{ "type": "text", "text": "sys", "cache_control": { "type": "ephemeral" } }])
        );
        let (_, b) = build(&provider(Kind::Anthropic), &req(Level::Tool));
        assert_eq!(b["tool_choice"], json!({ "type": "auto" }));
        assert_eq!(
            headers(&provider(Kind::Anthropic), Some("k"))[1].0,
            "anthropic-version"
        );
    }

    /// A reasoning effort reaches each kind in its own member, and an openai request
    /// with one limits its output with max_completion_tokens.
    #[test]
    fn reasoning_effort_by_kind() {
        let mut r = req(Level::JsonSchema);
        r.reasoning_effort = Some(super::super::config::Effort::Medium);
        let (_, b) = build(&provider(Kind::Openai), &r);
        assert_eq!(b["reasoning_effort"], "medium");
        assert_eq!(b["max_completion_tokens"], 100);
        assert!(b.get("max_tokens").is_none());
        let (_, b) = build(&provider(Kind::Anthropic), &r);
        assert_eq!(b["output_config"]["effort"], "medium");
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        let (_, b) = build(&provider(Kind::Openai), &req(Level::JsonSchema));
        assert_eq!(b["max_tokens"], 100);
        assert!(b.get("reasoning_effort").is_none() && b.get("max_completion_tokens").is_none());
    }

    /// OpenAI counts cache reads and writes inside prompt_tokens.
    #[test]
    fn openai_cache_usage() {
        let usage = |details: &str| {
            let body = format!(
                r#"{{"choices":[{{"message":{{"content":"x"}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":1000,"completion_tokens":7{details}}}}}"#
            );
            usage_of(Kind::Openai, body.as_bytes(), None)
        };
        let u = usage(r#","prompt_tokens_details":{"cached_tokens":600,"cache_write_tokens":300,"audio_tokens":0}"#).unwrap();
        assert_eq!(
            (
                u.input,
                u.cache_read,
                u.cache_write_5m,
                u.cache_write_1h,
                u.output
            ),
            (100, 600, 300, 0, 7)
        );
        let u = usage("").unwrap();
        assert_eq!((u.input, u.cache_read, u.cache_write_5m), (1000, 0, 0));
        let u = usage(r#","prompt_tokens_details":{"cached_tokens":null}"#).unwrap();
        assert_eq!(u.input, 1000);
        for bad in [
            r#","prompt_tokens_details":{"cached_tokens":900,"cache_write_tokens":200}"#,
            r#","prompt_tokens_details":{"cached_tokens":"5"}"#,
            r#","prompt_tokens_details":[]"#,
        ] {
            assert!(usage(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn answers_by_kind() {
        let r = parse(
            Kind::Ollama,
            br#"{"message":{"content":"{}"},"prompt_eval_count":3,"eval_count":4}"#,
            None,
        )
        .unwrap();
        assert_eq!(
            (r.text.as_str(), r.usage.input, r.usage.output),
            ("{}", 3, 4)
        );
        let r = parse(
            Kind::Openai,
            br#"{"choices":[{"message":{"content":"x"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2}}"#,
            None,
        )
        .unwrap();
        assert_eq!(r.text, "x");
        assert!(matches!(
            parse(
                Kind::Openai,
                br#"{"choices":[{"message":{"content":null,"refusal":"no"}}]}"#,
                None,
            ),
            Err(CallError::Refusal(_))
        ));
        assert!(matches!(
            parse(
                Kind::Openai,
                br#"{"choices":[{"message":{"content":""},"finish_reason":"content_filter"}]}"#,
                None,
            ),
            Err(CallError::Refusal(_))
        ));
        let r = parse(
            Kind::Openai,
            br#"{"choices":[{"message":{"tool_calls":[{"function":{"name":"D","arguments":"{\"a\":1}"}}]}}]}"#,
            None,
        )
        .unwrap();
        assert_eq!(r.tool_input, Some(json!({"a": 1})));
        let r = parse(
            Kind::Anthropic,
            br#"{"content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":6}}"#,
            None,
        )
        .unwrap();
        assert_eq!((r.text.as_str(), r.usage.input), ("hi", 5));
        assert!(matches!(
            parse(
                Kind::Anthropic,
                br#"{"content":[],"stop_reason":"refusal"}"#,
                None,
            ),
            Err(CallError::Refusal(_))
        ));
        assert!(matches!(
            parse(Kind::Anthropic, b"<html>", None),
            Err(CallError::Invalid(_))
        ));
    }

    fn anthropic(extra: Value) -> ProviderConfig {
        let mut v = json!({ "kind": "anthropic", "endpoint": "http://h/" });
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        serde_json::from_value(v).unwrap()
    }

    /// A request whose user message has a dataset part, a question part and a tail.
    fn layered(question: &str) -> ChatRequest {
        let mut r = req(Level::JsonSchema);
        r.messages = vec![Message::user_parts(vec![
            Part::cached("Context:\n<data>\nstable vocabulary\n"),
            Part::cached(format!(
                "question context\n</data>\n\nQuestion: {question}\n"
            )),
            Part::text("\nThe format."),
        ])];
        r
    }

    /// The breakpoints of a body, as the paths of the blocks that carry them.
    fn breakpoints(body: &Value) -> Vec<String> {
        let mut out = Vec::new();
        for (i, b) in body["system"].as_array().into_iter().flatten().enumerate() {
            if !b["cache_control"].is_null() {
                out.push(format!("system.{i}"));
            }
        }
        for (m, msg) in body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            for (i, b) in msg["content"].as_array().into_iter().flatten().enumerate() {
                if !b["cache_control"].is_null() {
                    out.push(format!("messages.{m}.{i}"));
                }
            }
        }
        out
    }

    /// The text of a system or content member, a string or text blocks.
    fn text_of(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Array(a) => a.iter().filter_map(|b| b["text"].as_str()).collect(),
            _ => String::new(),
        }
    }

    /// Prompt caching marks the end of each stable prefix and leaves the text as it was:
    /// the blocks join to the text the other kinds send in one string.
    #[test]
    fn anthropic_bodies_mark_the_cached_prefixes() {
        let r = layered("Who leads payments?");
        let (_, b) = build(&anthropic(json!({})), &r);
        assert_eq!(
            breakpoints(&b),
            ["system.0", "messages.0.0", "messages.0.1"]
        );
        let content = &b["messages"][0]["content"];
        assert_eq!(content.as_array().unwrap().len(), 3);
        assert!(content[2]["cache_control"].is_null());
        assert_eq!(content[0]["cache_control"], json!({ "type": "ephemeral" }));
        let (_, o) = build(&provider(Kind::Openai), &r);
        assert_eq!(
            text_of(content),
            o["messages"][1]["content"].as_str().unwrap()
        );
        assert_eq!(
            text_of(&b["system"]),
            o["messages"][0]["content"].as_str().unwrap()
        );
        // the other kinds never see a mark
        for kind in [Kind::Openai, Kind::Ollama] {
            let (_, o) = build(&provider(kind), &r);
            assert!(
                !String::from_utf8(body_bytes(&o))
                    .unwrap()
                    .contains("cache_control")
            );
        }
    }

    /// Two questions on the same dataset share every byte of the body up to the end of
    /// the dataset's part, the second breakpoint.
    #[test]
    fn two_questions_share_the_prefix_to_the_dataset_breakpoint() {
        let p = anthropic(json!({}));
        let prefix = |q: &str| {
            let (_, mut b) = build(&p, &layered(q));
            // the cacheable prefix in render order: the system, then the first block
            let content = b["messages"][0]["content"].as_array().unwrap()[0].clone();
            b["messages"][0]["content"] = json!([content]);
            body_bytes(&json!({ "system": b["system"], "messages": b["messages"],
                "output_config": b["output_config"], "model": b["model"] }))
        };
        assert_eq!(
            prefix("Who leads payments?"),
            prefix("How many teams are there?")
        );
        let (_, a) = build(&p, &layered("Who leads payments?"));
        let (_, c) = build(&p, &layered("How many teams are there?"));
        assert_ne!(
            a["messages"][0]["content"][1],
            c["messages"][0]["content"][1]
        );
    }

    /// `promptCaching.enabled: false` sends the body of a provider without caching, and
    /// `ttl: "1h"` puts the 1-hour TTL on every breakpoint.
    #[test]
    fn the_off_switch_and_the_hour_ttl() {
        let r = layered("q");
        let (_, off) = build(
            &anthropic(json!({ "promptCaching": { "enabled": false } })),
            &r,
        );
        assert_eq!(off["system"], "sys");
        assert_eq!(
            off["messages"][0]["content"],
            text_of(&build(&anthropic(json!({})), &r).1["messages"][0]["content"]).as_str()
        );
        assert!(
            !String::from_utf8(body_bytes(&off))
                .unwrap()
                .contains("cache_control")
        );
        assert_eq!(
            cache_ttl(
                &anthropic(json!({ "promptCaching": { "enabled": false } })),
                &r
            ),
            None
        );
        let p = anthropic(json!({ "promptCaching": { "ttl": "1h" } }));
        let (_, hour) = build(&p, &r);
        assert_eq!(breakpoints(&hour).len(), 3);
        for b in [&hour["system"][0], &hour["messages"][0]["content"][0]] {
            assert_eq!(
                b["cache_control"],
                json!({ "type": "ephemeral", "ttl": "1h" })
            );
        }
        assert_eq!(cache_ttl(&p, &r), Some(CacheTtl::OneHour));
        // a request that marks nothing is sent as without caching
        let mut plain = r.clone();
        plain.system = vec![Part::text("sys")];
        plain.messages = vec![Message::user("q")];
        let (_, b) = build(&anthropic(json!({})), &plain);
        assert_eq!(
            (b["system"].clone(), b["messages"][0]["content"].clone()),
            (json!("sys"), json!("q"))
        );
        assert_eq!(cache_ttl(&anthropic(json!({})), &plain), None);
    }

    /// A request may carry at most four breakpoints. The earliest marks are dropped,
    /// their text kept, and a mark on an empty run is dropped too.
    #[test]
    fn at_most_four_breakpoints() {
        let mut r = req(Level::JsonSchema);
        r.system = vec![Part::cached("a"), Part::cached("b")];
        r.messages = vec![
            Message::user_parts(vec![Part::cached("c"), Part::cached("d")]),
            Message::assistant("e"),
            Message::user_parts(vec![Part::cached("f"), Part::text("g")]),
        ];
        let (_, b) = build(&anthropic(json!({})), &r);
        assert_eq!(
            breakpoints(&b),
            ["system.0", "messages.0.0", "messages.0.1", "messages.2.0"]
        );
        // the dropped mark's text is kept in the block of the next kept mark
        assert_eq!(b["system"][0]["text"], "ab");
        assert_eq!(b["messages"][1]["content"], "e");
        assert_eq!(text_of(&b["messages"][2]["content"]), "fg");
        let n = String::from_utf8(body_bytes(&b))
            .unwrap()
            .matches("cache_control")
            .count();
        assert_eq!(n, MAX_BREAKPOINTS);
        r.system = vec![Part::cached(""), Part::text("a")];
        r.messages = vec![Message::user("q")];
        let (_, b) = build(&anthropic(json!({})), &r);
        assert_eq!(b["system"], json!([{ "type": "text", "text": "a" }]));
        // a mark on an empty part ends the text before it
        r.system = vec![Part::text("a"), Part::cached("")];
        let (_, b) = build(&anthropic(json!({})), &r);
        assert_eq!(breakpoints(&b), ["system.0"]);
    }

    /// The tool level lets the model choose the strict tool, because current models
    /// refuse a forced tool choice, and sends the schema without the bounds the strict
    /// grammar refuses.
    #[test]
    fn anthropic_tool_level_is_not_forced() {
        let mut r = req(Level::Tool);
        r.schema = Some((
            "Draft".into(),
            json!({ "type": "object", "properties": { "q": { "type": "string", "maxLength": 9 } },
                    "required": ["q"], "additionalProperties": false }),
        ));
        let (_, b) = build(&anthropic(json!({})), &r);
        assert_eq!(b["tool_choice"], json!({ "type": "auto" }));
        assert_eq!(b["tools"][0]["strict"], true);
        assert_eq!(
            b["tools"][0]["input_schema"]["properties"]["q"],
            json!({ "type": "string" })
        );
        let (_, c) = count_request(&anthropic(json!({})), &r).unwrap();
        assert_eq!(c["tool_choice"], json!({ "type": "auto" }));
        // the OpenAI protocol keeps its forced function call
        let (_, o) = build(&provider(Kind::Openai), &r);
        assert_eq!(o["tool_choice"]["function"]["name"], "Draft");
    }

    /// Anthropic reports cache writes, by TTL, and cache reads apart from the uncached
    /// input. An unsplit write is taken at the request's TTL, and a split that does not
    /// add up, or a write without a known TTL, at the 1-hour rate.
    #[test]
    fn anthropic_usage_reads_the_cache_classes() {
        let body =
            |u: Value| json!({ "content": [], "stop_reason": "end_turn", "usage": u }).to_string();
        let split = body(json!({ "input_tokens": 7, "output_tokens": 3,
            "cache_creation_input_tokens": 30, "cache_read_input_tokens": 500,
            "cache_creation": { "ephemeral_5m_input_tokens": 20, "ephemeral_1h_input_tokens": 10 } }));
        let u = usage_of(
            Kind::Anthropic,
            split.as_bytes(),
            Some(CacheTtl::FiveMinutes),
        )
        .unwrap();
        assert_eq!(
            u,
            Usage {
                input: 7,
                output: 3,
                cache_write_5m: 20,
                cache_write_1h: 10,
                cache_read: 500
            }
        );
        assert_eq!(
            parse(Kind::Anthropic, split.as_bytes(), None)
                .unwrap()
                .usage,
            u
        );
        let unsplit = body(json!({ "input_tokens": 1, "output_tokens": 1,
            "cache_creation_input_tokens": 40 }));
        let at = |ttl| usage_of(Kind::Anthropic, unsplit.as_bytes(), ttl).unwrap();
        assert_eq!(
            (
                at(Some(CacheTtl::FiveMinutes)).cache_write_5m,
                at(Some(CacheTtl::FiveMinutes)).cache_write_1h
            ),
            (40, 0)
        );
        assert_eq!(
            (
                at(Some(CacheTtl::OneHour)).cache_write_5m,
                at(Some(CacheTtl::OneHour)).cache_write_1h
            ),
            (0, 40)
        );
        assert_eq!(at(None).cache_write_1h, 40);
        let wrong = body(json!({ "input_tokens": 1, "output_tokens": 1,
            "cache_creation_input_tokens": 40, "cache_creation": { "ephemeral_5m_input_tokens": 50 } }));
        let u = usage_of(
            Kind::Anthropic,
            wrong.as_bytes(),
            Some(CacheTtl::FiveMinutes),
        )
        .unwrap();
        assert_eq!((u.cache_write_5m, u.cache_write_1h), (0, 50));
        // the usage is unknown without input or output
        assert_eq!(
            usage_of(
                Kind::Anthropic,
                body(json!({ "input_tokens": 1 })).as_bytes(),
                None
            ),
            None
        );
        // or with a cache member that is not a count
        for bad in [
            json!({ "cache_read_input_tokens": "9000" }),
            json!({ "cache_creation_input_tokens": -1 }),
            json!({ "cache_creation_input_tokens": 5, "cache_creation": { "ephemeral_1h_input_tokens": 5.5 } }),
            json!({ "cache_creation": [5] }),
            json!({ "cache_read_input_tokens": null }),
            json!({ "cache_creation": { "ephemeral_5m_input_tokens": null } }),
            json!({ "cache_creation_input_tokens": 1,
                "cache_creation": { "ephemeral_5m_input_tokens": u64::MAX, "ephemeral_1h_input_tokens": 1 } }),
        ] {
            let mut u = json!({ "input_tokens": 1, "output_tokens": 1 });
            u.as_object_mut()
                .unwrap()
                .extend(bad.as_object().unwrap().clone());
            assert_eq!(
                usage_of(Kind::Anthropic, body(u.clone()).as_bytes(), None),
                None,
                "{u}"
            );
        }
    }

    #[test]
    fn keys_are_scrubbed() {
        assert_eq!(
            scrub("bad key sk-secret-123".into(), Some("sk-secret-123")),
            "bad key [redacted]"
        );
    }
}
