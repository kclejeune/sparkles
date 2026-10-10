//! Model providers and role lists (spec C18 §3.4 to §3.7).
//!
//! The operator defines named providers of three kinds (Ollama, the OpenAI protocol,
//! Anthropic) in `--model-config FILE`, names their keys as secrets with
//! `--model-secret NAME=env:VAR|file:PATH`, and gives each role (`draft`, `repair`,
//! `summarize`, `extract`, `explain`, `optimize`) an ordered list of provider and model
//! pairs. [`Models`] holds that configuration with the state of each provider: its
//! concurrency slots, its request spacing, its daily token count, and the
//! structured-output level detected for each pair.
//!
//! The entry point for a step that needs a model is [`Models::call`]: one answer from
//! one pair, matching an [`OutputSchema`], with the degradation of §3.6 and a
//! [`StepRecord`] of what it cost. The pipeline of §4 (`crate::ask`) chooses the pair
//! from [`Models::pairs`]. Nothing here chooses between pairs: escalation is the
//! caller's (§5.5).
//!
//! Keys are read from their secrets for each request. They never appear in a response,
//! a log line or an error message, and no HTTP route can create a provider or change
//! its endpoint.

mod client;
mod config;
pub mod http;
#[cfg(test)]
pub(crate) mod mock;
mod structured;

pub use client::{CallError, ChatRequest, ChatResponse, Message};
pub use config::{Kind, Level, ModelsConfig, Pair, ProviderConfig, Role};
pub use structured::{OutputSchema, fenced, validate};

use anyhow::{Context, Result};
use parking_lot::{Condvar, Mutex};
use serde_json::{Value, json};
use sparkles::vector::embed::SecretSource;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// `--model-config` and `--model-secret`, shared by `serve` and `ask`.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct ModelArgs {
    /// The model providers and role lists, as JSON (spec C18 §3.4): `{"models":
    /// {"providers": {...}, "roles": {...}}}`. Without it the server has no model
    #[arg(long, value_name = "FILE")]
    pub model_config: Option<std::path::PathBuf>,
    /// A secret a provider may name as its API key (`"apiKey": {"secret": NAME}`), read
    /// from an environment variable or a file at each request: NAME=env:VARIABLE or
    /// NAME=file:PATH (repeatable)
    #[arg(long, value_name = "NAME=SOURCE")]
    pub model_secret: Vec<String>,
}

impl ModelArgs {
    /// The providers, or `None` without `--model-config`. Requests go through
    /// `outbound`.
    pub fn load(
        &self,
        outbound: sparkles::outbound::OutboundPolicy,
    ) -> Result<Option<Arc<Models>>> {
        let secrets = parse_secrets(&self.model_secret)?;
        let Some(path) = &self.model_config else {
            if !secrets.is_empty() {
                tracing::warn!("--model-secret has no effect without --model-config");
            }
            return Ok(None);
        };
        let cfg = ModelsConfig::load(path)?;
        Ok(Some(Arc::new(Models::new(cfg, secrets, outbound))))
    }
}

/// `--model-secret NAME=env:VAR|file:PATH` flags.
pub fn parse_secrets(flags: &[String]) -> Result<BTreeMap<String, SecretSource>> {
    let mut out = BTreeMap::new();
    for f in flags {
        let (name, src) = f.split_once('=').with_context(|| {
            format!("--model-secret {f}: expected NAME=env:VAR or NAME=file:PATH")
        })?;
        if name.is_empty() {
            anyhow::bail!("--model-secret {f}: the name is empty");
        }
        let src = src
            .parse()
            .map_err(|e: String| anyhow::anyhow!("--model-secret {e}"))?;
        out.insert(name.to_string(), src);
    }
    Ok(out)
}

/// The record of one step: which pair answered, at what level, at what cost.
#[derive(Clone, Debug, Default)]
pub struct StepRecord {
    pub role: Option<Role>,
    pub provider: String,
    pub model: String,
    /// the level of the last request
    pub level: Option<Level>,
    /// requests sent, retries included
    pub requests: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub latency: Duration,
    /// `ok`, or the code of the failure
    pub outcome: String,
    /// at the pair's `pricing`, when it has one
    pub estimated_cost: Option<f64>,
}

impl StepRecord {
    pub fn json(&self) -> Value {
        let mut j = json!({
            "provider": self.provider,
            "model": self.model,
            "requests": self.requests,
            "inputTokens": self.input_tokens,
            "outputTokens": self.output_tokens,
            "latencyMs": self.latency.as_millis() as u64,
            "outcome": self.outcome,
        });
        if let Some(r) = self.role {
            j["role"] = r.as_str().into();
        }
        if let Some(l) = self.level {
            j["level"] = l.as_str().into();
        }
        if let Some(c) = self.estimated_cost {
            j["estimatedCost"] = number(c);
        }
        j
    }
}

/// A step's answer.
#[derive(Debug)]
pub struct Answer {
    pub value: Value,
    pub level: Level,
    pub record: StepRecord,
}

/// Why a step has no answer.
#[derive(Debug)]
pub struct Failure {
    pub error: StepError,
    pub record: StepRecord,
}

/// The cause of a [`Failure`].
#[derive(Debug, Clone, PartialEq)]
pub enum StepError {
    /// the provider or the network failed (`provider-failure` of §5.5 when it is a
    /// timeout, a refusal or an outage)
    Call(CallError),
    /// the answer still did not match the schema after its retry
    InvalidOutput(Vec<String>),
    /// the provider's daily token budget is spent
    Budget(String),
    /// no provider of that name, or the role list names one that is gone
    UnknownPair(String),
    /// the deadline passed before the step could start
    Deadline,
}

impl StepError {
    pub fn code(&self) -> &'static str {
        match self {
            StepError::Call(e) => e.code(),
            StepError::InvalidOutput(_) => "invalid-output",
            StepError::Budget(_) => "budget-exceeded",
            StepError::UnknownPair(_) => "unknown-pair",
            StepError::Deadline => "deadline",
        }
    }

    pub fn message(&self) -> String {
        match self {
            StepError::Call(e) => e.to_string(),
            StepError::InvalidOutput(e) => {
                format!("the answer does not match the schema: {}", e.join("; "))
            }
            StepError::Budget(m) | StepError::UnknownPair(m) => m.clone(),
            StepError::Deadline => "the deadline passed".into(),
        }
    }
}

/// Counting slots with a deadline.
struct Slots {
    max: usize,
    used: Mutex<usize>,
    cv: Condvar,
}

struct SlotGuard<'a>(&'a Slots);

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        *self.0.used.lock() -= 1;
        self.0.cv.notify_one();
    }
}

impl Slots {
    fn acquire(&self, deadline: Instant) -> Option<SlotGuard<'_>> {
        let mut used = self.used.lock();
        while *used >= self.max {
            if self.cv.wait_until(&mut used, deadline).timed_out() && *used >= self.max {
                return None;
            }
        }
        *used += 1;
        Some(SlotGuard(self))
    }
}

/// The state of one provider.
struct Runtime {
    slots: Slots,
    /// the earliest time of the next request (`requestsPerMinute`)
    next: Mutex<Instant>,
    /// (day number, tokens counted that day)
    tokens: Mutex<(u64, u64)>,
}

/// The last outcome of a pair, for `GET /$/models`.
#[derive(Clone, Debug)]
struct PairStatus {
    ok: bool,
    message: Option<String>,
    at: SystemTime,
}

/// The providers, the role lists and their state.
pub struct Models {
    pub config: ModelsConfig,
    secrets: BTreeMap<String, SecretSource>,
    outbound: sparkles::outbound::OutboundPolicy,
    runtime: BTreeMap<String, Runtime>,
    /// the level detected for each (provider, model) pair under `auto`
    levels: Mutex<HashMap<(String, String), Level>>,
    status: Mutex<HashMap<(String, String), PairStatus>>,
}

/// A JSON number without a trailing `.0` for whole values.
fn number(x: f64) -> Value {
    if x.fract() == 0.0 && (0.0..9e15).contains(&x) {
        Value::from(x as u64)
    } else {
        Value::from(x)
    }
}

fn today() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400)
}

impl Models {
    pub fn new(
        config: ModelsConfig,
        secrets: BTreeMap<String, SecretSource>,
        outbound: sparkles::outbound::OutboundPolicy,
    ) -> Models {
        let runtime = config
            .providers
            .iter()
            .map(|(n, p)| {
                (
                    n.clone(),
                    Runtime {
                        slots: Slots {
                            max: p.concurrency.unwrap_or(config::DEFAULT_CONCURRENCY),
                            used: Mutex::new(0),
                            cv: Condvar::new(),
                        },
                        next: Mutex::new(Instant::now()),
                        tokens: Mutex::new((today(), 0)),
                    },
                )
            })
            .collect();
        Models {
            config,
            secrets,
            outbound,
            runtime,
            levels: Mutex::new(HashMap::new()),
            status: Mutex::new(HashMap::new()),
        }
    }

    /// The pairs of `role`, in order (§3.7).
    pub fn pairs(&self, role: Role) -> Vec<Pair> {
        self.config.pairs(role).to_vec()
    }

    /// The provider `name`.
    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.config.providers.get(name)
    }

    /// The members of a pair's model after the provider's defaults.
    pub fn resolved(&self, pair: &Pair) -> Option<config::Resolved> {
        self.provider(&pair.provider).map(|p| p.model(&pair.model))
    }

    /// The key of provider `name`, read now: `Ok(None)` when it names none.
    fn key(&self, p: &ProviderConfig) -> Result<Option<String>, String> {
        let Some(r) = &p.api_key else {
            return Ok(None);
        };
        let read = match self.secrets.get(&r.secret) {
            None => return Err(format!("no model secret named {:?} is defined", r.secret)),
            Some(SecretSource::Env(v)) => std::env::var(v).map_err(|_| {
                format!(
                    "the environment variable of secret {:?} is not set",
                    r.secret
                )
            }),
            Some(SecretSource::File(path)) => std::fs::read_to_string(path)
                .map_err(|_| format!("the file of secret {:?} cannot be read", r.secret)),
        }?;
        let k = read.trim().to_string();
        if k.is_empty() {
            return Err(format!("secret {:?} is empty", r.secret));
        }
        Ok(Some(k))
    }

    /// The level a pair is called at first, and the levels to try after it when the
    /// provider refuses a level (detection under `auto`).
    fn levels(&self, p: &ProviderConfig, pair: &Pair) -> (Vec<Level>, bool) {
        let configured = p.model(&pair.model).structured_output;
        if configured != Level::Auto {
            return (vec![configured], false);
        }
        if let Some(l) = self
            .levels
            .lock()
            .get(&(pair.provider.clone(), pair.model.clone()))
        {
            return (vec![*l], false);
        }
        let seq = match p.kind {
            Kind::Anthropic => vec![Level::JsonSchema, Level::Text],
            _ => vec![Level::JsonSchema, Level::JsonObject, Level::Text],
        };
        (seq, true)
    }

    /// The level detected or configured for a pair, if known.
    pub fn level_of(&self, pair: &Pair) -> Option<Level> {
        let p = self.provider(&pair.provider)?;
        match p.model(&pair.model).structured_output {
            Level::Auto => self
                .levels
                .lock()
                .get(&(pair.provider.clone(), pair.model.clone()))
                .copied(),
            l => Some(l),
        }
    }

    /// One answer from `pair` that matches `out`, by `deadline`. `role` names the step
    /// in the record. The answer is retried once with the validation errors, and under
    /// `auto` a level the provider refuses moves to the next (§3.6).
    pub fn call(
        &self,
        role: Option<Role>,
        pair: &Pair,
        system: &str,
        user: &str,
        out: &OutputSchema,
        deadline: Instant,
    ) -> Result<Answer, Failure> {
        let mut rec = StepRecord {
            role,
            provider: pair.provider.clone(),
            model: pair.model.clone(),
            ..Default::default()
        };
        let fail = |error: StepError, mut rec: StepRecord| {
            rec.outcome = error.code().to_string();
            Err(Failure { error, record: rec })
        };
        let (Some(p), Some(rt)) = (
            self.provider(&pair.provider),
            self.runtime.get(&pair.provider),
        ) else {
            return fail(
                StepError::UnknownPair(format!("no provider named {:?}", pair.provider)),
                rec,
            );
        };
        let key = match self.key(p) {
            Ok(k) => k,
            Err(m) => {
                self.note(pair, false, Some(m.clone()));
                return fail(StepError::Call(CallError::Secret(m)), rec);
            }
        };
        let m = p.model(&pair.model);
        let pricing = m.pricing;
        let started = Instant::now();
        let (levels, detecting) = self.levels(p, pair);
        let mut last: Option<StepError> = None;
        for (i, &level) in levels.iter().enumerate() {
            let has_next = detecting && i + 1 < levels.len();
            let mut messages = structured::messages(user, out, level);
            for attempt in 0..2 {
                if let Some(cap) = p.budget.and_then(|b| b.tokens_per_day) {
                    let mut t = rt.tokens.lock();
                    if t.0 != today() {
                        *t = (today(), 0);
                    }
                    if t.1 >= cap {
                        rec.latency = started.elapsed();
                        return fail(
                            StepError::Budget(format!(
                                "provider {}'s daily budget of {cap} tokens is used up",
                                pair.provider
                            )),
                            rec,
                        );
                    }
                }
                let timeout = pair
                    .request_timeout_secs
                    .map_or(m.request_timeout_secs, |s| s.secs());
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    rec.latency = started.elapsed();
                    return fail(StepError::Deadline, rec);
                }
                let req = ChatRequest {
                    model: pair.model.clone(),
                    system: system.to_string(),
                    messages: messages.clone(),
                    schema: Some((out.name.to_string(), out.schema.clone())),
                    level,
                    max_output_tokens: pair.max_output_tokens.unwrap_or(m.max_output_tokens),
                    temperature: m.temperature,
                    num_ctx: m.num_ctx,
                    timeout: Duration::from_secs_f64(timeout).min(left),
                };
                let Some(_slot) = rt.slots.acquire(deadline) else {
                    rec.latency = started.elapsed();
                    return fail(
                        StepError::Call(CallError::Unavailable(format!(
                            "provider {} had no free slot before the deadline",
                            pair.provider
                        ))),
                        rec,
                    );
                };
                if let Some(rpm) = p.requests_per_minute.filter(|r| *r > 0) {
                    let wait = {
                        let mut next = rt.next.lock();
                        let now = Instant::now();
                        let at = (*next).max(now);
                        *next = at + Duration::from_secs_f64(60.0 / rpm as f64);
                        at - now
                    };
                    if !wait.is_zero() {
                        std::thread::sleep(wait);
                    }
                }
                rec.requests += 1;
                rec.level = Some(level);
                let t = client::Transport {
                    outbound: &self.outbound,
                    key: key.clone(),
                };
                let resp = client::send(&t, &pair.provider, p, &req);
                drop(_slot);
                let resp = match resp {
                    Ok(r) => r,
                    Err(e) if has_next && structured::unsupported(&e) => {
                        tracing::info!(target: "sparkles::models", "{}: level {} refused, trying the next: {e}", pair.label(), level.as_str());
                        last = Some(StepError::Call(e));
                        break;
                    }
                    Err(e) => {
                        rec.latency = started.elapsed();
                        self.note(pair, false, Some(e.to_string()));
                        return fail(StepError::Call(e), rec);
                    }
                };
                rec.input_tokens += resp.input_tokens;
                rec.output_tokens += resp.output_tokens;
                {
                    let mut t = rt.tokens.lock();
                    if t.0 != today() {
                        *t = (today(), 0);
                    }
                    t.1 += resp.input_tokens + resp.output_tokens;
                }
                match structured::value_of(&resp, out, level) {
                    Ok(value) => {
                        if detecting {
                            self.levels
                                .lock()
                                .insert((pair.provider.clone(), pair.model.clone()), level);
                        }
                        rec.latency = started.elapsed();
                        rec.outcome = "ok".into();
                        rec.estimated_cost = pricing.map(|pr| {
                            (rec.input_tokens as f64 * pr.input_per_m_tok
                                + rec.output_tokens as f64 * pr.output_per_m_tok)
                                / 1e6
                        });
                        self.note(pair, true, None);
                        return Ok(Answer {
                            value,
                            level,
                            record: rec,
                        });
                    }
                    Err(errors) => {
                        last = Some(StepError::InvalidOutput(errors.clone()));
                        if attempt == 0 {
                            messages.push(Message::assistant(resp.text.clone()));
                            messages.push(structured::retry_message(&errors));
                        }
                    }
                }
            }
            if !has_next {
                break;
            }
        }
        rec.latency = started.elapsed();
        rec.estimated_cost = pricing.map(|pr| {
            (rec.input_tokens as f64 * pr.input_per_m_tok
                + rec.output_tokens as f64 * pr.output_per_m_tok)
                / 1e6
        });
        let error = last.unwrap_or(StepError::InvalidOutput(vec!["no answer".into()]));
        self.note(pair, false, Some(error.message()));
        fail(error, rec)
    }

    fn note(&self, pair: &Pair, ok: bool, message: Option<String>) {
        self.status.lock().insert(
            (pair.provider.clone(), pair.model.clone()),
            PairStatus {
                ok,
                message,
                at: SystemTime::now(),
            },
        );
    }

    /// The test call of `POST /$/models/{name}/test`: a short prompt to the pair, after
    /// forgetting its detected level so that detection runs again.
    pub fn test(
        &self,
        provider: &str,
        model: Option<&str>,
        timeout: Duration,
    ) -> Result<Value, String> {
        let p = self
            .provider(provider)
            .ok_or_else(|| format!("no provider named {provider:?}"))?;
        let model = match model {
            Some(m) => {
                if !p.allows(m) {
                    return Err(format!(
                        "provider {provider} does not allow the model {m:?}"
                    ));
                }
                m.to_string()
            }
            None => self.models_of(provider).into_iter().next().ok_or_else(|| {
                format!(
                    "provider {provider} has no model in a role list or in its models; give model"
                )
            })?,
        };
        let pair = Pair::new(provider, &model);
        self.levels
            .lock()
            .remove(&(provider.to_string(), model.clone()));
        let out = structured::ping();
        let r = self.call(
            None,
            &pair,
            "You are a connectivity test. Follow the format you are asked for.",
            "Say that the test works.",
            &out,
            Instant::now() + timeout,
        );
        Ok(match r {
            Ok(a) => json!({
                "provider": provider,
                "model": model,
                "ok": true,
                "level": a.level.as_str(),
                "structuredOutput": a.level != Level::Text,
                "latencyMs": a.record.latency.as_millis() as u64,
                "inputTokens": a.record.input_tokens,
                "outputTokens": a.record.output_tokens,
                "requests": a.record.requests,
            }),
            Err(f) => json!({
                "provider": provider,
                "model": model,
                "ok": false,
                "level": f.record.level.map(Level::as_str),
                "structuredOutput": false,
                "latencyMs": f.record.latency.as_millis() as u64,
                "inputTokens": f.record.input_tokens,
                "outputTokens": f.record.output_tokens,
                "requests": f.record.requests,
                "error": { "code": f.error.code(), "message": f.error.message() },
            }),
        })
    }

    /// The models of a provider: those the role lists name, then its `models` entries.
    fn models_of(&self, provider: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for role in Role::ALL {
            for pair in self.config.pairs(role) {
                if pair.provider == provider && !out.contains(&pair.model) {
                    out.push(pair.model.clone());
                }
            }
        }
        if let Some(p) = self.provider(provider) {
            for m in p.models.keys() {
                if !out.contains(m) {
                    out.push(m.clone());
                }
            }
        }
        out
    }

    /// `GET /$/models`: the providers with their kind, endpoint, status and models, and
    /// the role lists. No key and no secret's source is included.
    pub fn describe(&self) -> Value {
        let status = self.status.lock().clone();
        let providers: Vec<Value> = self
            .config
            .providers
            .iter()
            .map(|(name, p)| {
                let key_status = match self.key(p) {
                    Ok(_) => "ok",
                    Err(_) => "secret-missing",
                };
                let models: Vec<Value> = self
                    .models_of(name)
                    .into_iter()
                    .map(|m| {
                        let r = p.model(&m);
                        let pair = Pair::new(name, &m);
                        let mut j = json!({
                            "name": m,
                            "contextTokens": r.context_tokens,
                            "maxOutputTokens": r.max_output_tokens,
                            "structuredOutput": r.structured_output.as_str(),
                            "requestTimeoutSecs": number(r.request_timeout_secs),
                            "detected": self.level_of(&pair).map(Level::as_str),
                        });
                        if let Some(pr) = r.pricing {
                            j["pricing"] = json!({
                                "inputPerMTok": number(pr.input_per_m_tok),
                                "outputPerMTok": number(pr.output_per_m_tok),
                            });
                        }
                        j["status"] = match status.get(&(name.clone(), m.clone())) {
                            None => json!({ "state": "untested" }),
                            Some(s) => {
                                let mut x = json!({
                                    "state": if s.ok { "ok" } else { "failing" },
                                    "at": chrono::DateTime::<chrono::Utc>::from(s.at)
                                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                                });
                                if let Some(msg) = &s.message {
                                    x["message"] = msg.clone().into();
                                }
                                x
                            }
                        };
                        j
                    })
                    .collect();
                let mut j = json!({
                    "name": name,
                    "kind": p.kind.as_str(),
                    "endpoint": p.endpoint,
                    "status": key_status,
                    "concurrency": p.concurrency.unwrap_or(config::DEFAULT_CONCURRENCY),
                    "models": models,
                });
                if let Some(k) = &p.api_key {
                    j["apiKey"] = json!({ "secret": k.secret });
                }
                if let Some(a) = &p.allowed_models {
                    j["allowedModels"] = json!(a);
                }
                if let Some(r) = p.requests_per_minute {
                    j["requestsPerMinute"] = r.into();
                }
                if let Some(t) = p.budget.and_then(|b| b.tokens_per_day) {
                    let used = self.runtime.get(name).map_or(0, |rt| {
                        let t = rt.tokens.lock();
                        if t.0 == today() { t.1 } else { 0 }
                    });
                    j["budget"] = json!({ "tokensPerDay": t, "usedToday": used });
                }
                j
            })
            .collect();
        let roles: serde_json::Map<String, Value> = Role::ALL
            .into_iter()
            .map(|r| {
                (
                    r.as_str().to_string(),
                    serde_json::to_value(self.config.pairs(r)).unwrap_or_default(),
                )
            })
            .collect();
        let mut out = json!({ "providers": providers, "roles": roles });
        if let Some(r) = &self.config.routing {
            out["routing"] = serde_json::to_value(r).unwrap_or_default();
        }
        out
    }
}

#[cfg(test)]
mod tests;
