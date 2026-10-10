//! The model configuration of spec C18 §3.4 and §3.7: named providers with their
//! endpoint, key reference and limits, and the ordered provider and model pairs of each
//! role. The operator declares it with `--model-config FILE`, and server administrators
//! may change it through `/$/server/settings/models` (spec C19 §11) unless the settings
//! file locks the fields. Users with `admin` on a dataset only choose among the providers
//! by name, so they cannot point a configured key at an endpoint of their choice.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The protocol a provider speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Ollama's native `POST /api/chat`
    Ollama,
    /// `POST {endpoint}/chat/completions` of the OpenAI protocol
    Openai,
    /// Anthropic's `POST {endpoint}/v1/messages`
    Anthropic,
    /// embedding models run in the server process (spec F12); no endpoint, no chat
    Local,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Ollama => "ollama",
            Kind::Openai => "openai",
            Kind::Anthropic => "anthropic",
            Kind::Local => "local",
        }
    }
}

/// How the server constrains a response to a JSON Schema (§3.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Level {
    /// try `json-schema`, then `json-object`, then `text`, and remember the first that
    /// works for the pair
    Auto,
    /// the provider's schema-constrained output
    JsonSchema,
    /// JSON mode, with the schema as text in the prompt
    JsonObject,
    /// a single forced tool whose input is the schema
    Tool,
    /// a fenced block in plain text
    Text,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Auto => "auto",
            Level::JsonSchema => "json-schema",
            Level::JsonObject => "json-object",
            Level::Tool => "tool",
            Level::Text => "text",
        }
    }
}

/// The steps that call a model (§3.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Draft,
    Repair,
    Summarize,
    Extract,
    Explain,
    Optimize,
}

impl Role {
    pub const ALL: [Role; 6] = [
        Role::Draft,
        Role::Repair,
        Role::Summarize,
        Role::Extract,
        Role::Explain,
        Role::Optimize,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Draft => "draft",
            Role::Repair => "repair",
            Role::Summarize => "summarize",
            Role::Extract => "extract",
            Role::Explain => "explain",
            Role::Optimize => "optimize",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

/// A key, named and never given: `{"secret": NAME}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    pub secret: String,
}

/// Prices per million tokens, for estimates only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Pricing {
    #[serde(default)]
    pub input_per_m_tok: f64,
    #[serde(default)]
    pub output_per_m_tok: f64,
}

/// Token caps per day.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Budget {
    /// input and output tokens together, per day
    pub tokens_per_day: Option<u64>,
}

/// Members that describe one model; each can be set on the provider as the default of
/// its models, or in an entry of its `models`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelOptions {
    pub context_tokens: Option<u64>,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub structured_output: Option<Level>,
    pub pricing: Option<Pricing>,
    pub request_timeout_secs: Option<f64>,
    /// Ollama: the context option (`num_ctx`)
    pub num_ctx: Option<u64>,
    /// local (spec F12): the Hugging Face repository of the model's snapshot
    pub repo: Option<String>,
    /// local: the snapshot's full 40-character commit
    pub revision: Option<String>,
    /// local: a snapshot directory the operator provides, instead of repo and revision
    pub path: Option<String>,
    /// local: `f32` or `bf16`
    pub dtype: Option<String>,
    /// local: threads of the model's inference pool
    pub threads: Option<usize>,
    /// local: seconds without work before the weights are dropped (0 keeps them)
    pub idle_unload_secs: Option<u64>,
    /// local: Matryoshka truncation of the vectors to this many components
    pub dimensions: Option<usize>,
    /// local: tokens per text, at most the model's own limit
    pub max_tokens: Option<usize>,
    /// local: text put before queries, replacing the snapshot's query prompt
    pub query_prefix: Option<String>,
    /// local: text put before stored documents, replacing the snapshot's prompt
    pub document_prefix: Option<String>,
}

impl ModelOptions {
    /// `self` with the members it leaves out taken from `base`.
    fn over(&self, base: &ModelOptions) -> ModelOptions {
        ModelOptions {
            context_tokens: self.context_tokens.or(base.context_tokens),
            max_output_tokens: self.max_output_tokens.or(base.max_output_tokens),
            temperature: self.temperature.or(base.temperature),
            structured_output: self.structured_output.or(base.structured_output),
            pricing: self.pricing.or(base.pricing),
            request_timeout_secs: self.request_timeout_secs.or(base.request_timeout_secs),
            num_ctx: self.num_ctx.or(base.num_ctx),
            repo: self.repo.clone().or_else(|| base.repo.clone()),
            revision: self.revision.clone().or_else(|| base.revision.clone()),
            path: self.path.clone().or_else(|| base.path.clone()),
            dtype: self.dtype.clone().or_else(|| base.dtype.clone()),
            threads: self.threads.or(base.threads),
            idle_unload_secs: self.idle_unload_secs.or(base.idle_unload_secs),
            dimensions: self.dimensions.or(base.dimensions),
            max_tokens: self.max_tokens.or(base.max_tokens),
            query_prefix: self
                .query_prefix
                .clone()
                .or_else(|| base.query_prefix.clone()),
            document_prefix: self
                .document_prefix
                .clone()
                .or_else(|| base.document_prefix.clone()),
        }
    }
}

/// One provider: an endpoint with its key and limits. Unknown members reach the
/// flattened [`ModelOptions`], which refuses them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    pub kind: Kind,
    /// empty for the local kind
    #[serde(default)]
    pub endpoint: String,
    pub api_key: Option<SecretRef>,
    pub connect_timeout_secs: Option<f64>,
    pub concurrency: Option<usize>,
    pub requests_per_minute: Option<u32>,
    pub budget: Option<Budget>,
    pub allowed_models: Option<Vec<String>>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelOptions>,
    /// Ollama: how long it keeps the model loaded
    pub keep_alive: Option<String>,
    /// OpenAI protocol: extra headers that are not secret
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Anthropic: the `anthropic-version` header value
    pub version: Option<String>,
    /// the defaults of this provider's models
    #[serde(flatten)]
    pub defaults: ModelOptions,
}

/// The defaults of §3.4.
pub const DEFAULT_CONTEXT_TOKENS: u64 = 8192;
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 2048;
pub const DEFAULT_REQUEST_TIMEOUT_SECS: f64 = 60.0;
pub const DEFAULT_CONNECT_TIMEOUT_SECS: f64 = 10.0;
pub const DEFAULT_CONCURRENCY: usize = 4;
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

/// The members of one model after the provider's defaults and the documented ones.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub context_tokens: u64,
    pub max_output_tokens: u32,
    /// `None`: the parameter is not sent
    pub temperature: Option<f64>,
    pub structured_output: Level,
    pub pricing: Option<Pricing>,
    pub request_timeout_secs: f64,
    pub num_ctx: Option<u64>,
}

impl ProviderConfig {
    /// The members of `model`, from its entry, the provider's defaults and §3.4.
    pub fn model(&self, model: &str) -> Resolved {
        let o = self
            .models
            .get(model)
            .cloned()
            .unwrap_or_default()
            .over(&self.defaults);
        Resolved {
            context_tokens: o.context_tokens.unwrap_or(DEFAULT_CONTEXT_TOKENS),
            max_output_tokens: o.max_output_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
            // drafts run deterministic; current Claude models refuse the parameter, so
            // the anthropic kind sends it only when the configuration sets it
            temperature: match self.kind {
                Kind::Anthropic => o.temperature,
                _ => Some(o.temperature.unwrap_or(0.0)),
            },
            structured_output: o.structured_output.unwrap_or(Level::Auto),
            pricing: o.pricing,
            request_timeout_secs: o
                .request_timeout_secs
                .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS),
            num_ctx: o.num_ctx,
        }
    }

    /// Whether role lists may name `model` with this provider.
    pub fn allows(&self, model: &str) -> bool {
        self.allowed_models
            .as_ref()
            .is_none_or(|a| a.iter().any(|m| m == model))
    }
}

/// One entry of a role list.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Pair {
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_timeout_secs: Option<OrderedSecs>,
}

impl Pair {
    pub fn new(provider: &str, model: &str) -> Pair {
        Pair {
            provider: provider.into(),
            model: model.into(),
            max_output_tokens: None,
            request_timeout_secs: None,
        }
    }

    /// `PROVIDER/MODEL` (the model may hold `/` itself).
    pub fn parse(s: &str) -> Option<Pair> {
        let (p, m) = s.split_once('/')?;
        (!p.is_empty() && !m.is_empty()).then(|| Pair::new(p, m))
    }

    pub fn label(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

/// Seconds as a JSON number that can be compared and hashed (whole milliseconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OrderedSecs(pub u64);

impl OrderedSecs {
    pub fn secs(self) -> f64 {
        self.0 as f64 / 1000.0
    }
}

impl Serialize for OrderedSecs {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(self.secs())
    }
}

impl<'de> Deserialize<'de> for OrderedSecs {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<OrderedSecs, D::Error> {
        let f = f64::deserialize(d)?;
        if !(f.is_finite() && f > 0.0) {
            return Err(serde::de::Error::custom(
                "requestTimeoutSecs must be a positive number",
            ));
        }
        Ok(OrderedSecs((f * 1000.0).round() as u64))
    }
}

/// Routing settings of §5.5 (read by the escalation of Phase 2).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Routing {
    pub complexity_threshold: Option<u32>,
    pub example_score: Option<f64>,
}

/// The whole configuration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelsConfig {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub roles: BTreeMap<Role, Vec<Pair>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<Routing>,
}

/// The members a provider may have: its own and the model defaults it flattens.
pub const PROVIDER_MEMBERS: &[&str] = &[
    "kind",
    "endpoint",
    "apiKey",
    "connectTimeoutSecs",
    "concurrency",
    "requestsPerMinute",
    "budget",
    "allowedModels",
    "models",
    "keepAlive",
    "headers",
    "version",
    "contextTokens",
    "maxOutputTokens",
    "temperature",
    "structuredOutput",
    "pricing",
    "requestTimeoutSecs",
    "numCtx",
    "repo",
    "revision",
    "path",
    "dtype",
    "threads",
    "idleUnloadSecs",
    "dimensions",
    "maxTokens",
    "queryPrefix",
    "documentPrefix",
];

/// Refuse unknown provider members, which serde's `flatten` would ignore.
fn known_members(cfg: &serde_json::Value) -> Result<()> {
    if let Some(ps) = cfg.get("providers").and_then(|p| p.as_object()) {
        for (name, p) in ps {
            if let Some(o) = p.as_object()
                && let Some(k) = o.keys().find(|k| !PROVIDER_MEMBERS.contains(&k.as_str()))
            {
                bail!(
                    "provider {name}: unknown field `{k}`, expected one of {}",
                    PROVIDER_MEMBERS.join(", ")
                );
            }
        }
    }
    Ok(())
}

/// Refuse an `apiKey` that is not `{"secret": NAME}` without quoting it, since a key's
/// value pasted there must not reach an error message.
fn key_references(cfg: &serde_json::Value) -> Result<()> {
    if let Some(ps) = cfg.get("providers").and_then(|p| p.as_object()) {
        for (name, p) in ps {
            let Some(k) = p.get("apiKey") else {
                continue;
            };
            let ok = k.is_null()
                || k.as_object().is_some_and(|o| {
                    o.len() == 1 && o.get("secret").is_some_and(serde_json::Value::is_string)
                });
            if !ok {
                bail!(
                    "provider {name}: apiKey must be {{\"secret\": NAME}}; a key's value is stored with PUT /$/server/secrets/{{name}} or --model-secret"
                );
            }
        }
    }
    Ok(())
}

/// Whether `name` may name a secret (`--model-secret NAME=…`, `apiKey.secret` and the
/// files of runtime secrets): 1 to 128 letters, digits, `_`, `-` and `.`, not starting
/// with `.` or `-`.
pub fn check_secret_name(name: &str) -> std::result::Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
        && !name.starts_with(['.', '-']);
    if ok {
        Ok(())
    } else {
        Err(format!(
            "secret name {name:?}: use 1 to 128 letters, digits, '_', '-' and '.', not starting with '.' or '-'"
        ))
    }
}

/// A configuration file: `{"models": {...}}`, as §3.4 writes it, or the inner object.
#[derive(Deserialize)]
#[serde(untagged)]
enum File {
    Wrapped { models: ModelsConfig },
    Bare(ModelsConfig),
}

impl ModelsConfig {
    /// Parse and check a configuration file's text.
    pub fn parse(text: &str) -> Result<ModelsConfig> {
        // untagged enums hide the reason of a failure: try the wrapped form first when
        // the object has a `models` member, and report that form's error
        let v: serde_json::Value = serde_json::from_str(text).context("not JSON")?;
        known_members(v.get("models").unwrap_or(&v))?;
        key_references(v.get("models").unwrap_or(&v))?;
        let cfg = if v.get("models").is_some() {
            match serde_json::from_value::<File>(v.clone()) {
                Ok(File::Wrapped { models }) | Ok(File::Bare(models)) => models,
                Err(_) => serde_json::from_value::<ModelsConfig>(v["models"].clone())?,
            }
        } else {
            serde_json::from_value::<ModelsConfig>(v)?
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// Read and check `path`.
    pub fn load(path: &std::path::Path) -> Result<ModelsConfig> {
        Ok(ModelsConfig::load_value(path)?.0)
    }

    /// Read and check `path`, and answer the configuration with its JSON object, the
    /// inner object of the wrapped form (the declared layer of spec C19 §11.1).
    pub fn load_value(path: &std::path::Path) -> Result<(ModelsConfig, serde_json::Value)> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let read = || -> Result<(ModelsConfig, serde_json::Value)> {
            let cfg = ModelsConfig::parse(&text)?;
            let v: serde_json::Value = serde_json::from_str(&text)?;
            let inner = match v {
                serde_json::Value::Object(mut m) if m.contains_key("models") => {
                    m.remove("models").unwrap_or_default()
                }
                v => v,
            };
            Ok((cfg, inner))
        };
        read().with_context(|| format!("model configuration {}", path.display()))
    }

    /// Check the bare form of a configuration, as the `models` settings kind holds it:
    /// only `providers`, `roles` and `routing` at the top.
    pub fn from_value(v: &serde_json::Value) -> Result<ModelsConfig> {
        let Some(m) = v.as_object() else {
            bail!("the model configuration must be a JSON object");
        };
        if let Some(k) = m
            .keys()
            .find(|k| !matches!(k.as_str(), "providers" | "roles" | "routing"))
        {
            bail!("unknown field `{k}`, expected one of providers, roles, routing");
        }
        known_members(v)?;
        key_references(v)?;
        let cfg: ModelsConfig = serde_json::from_value(v.clone())?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// The checks of §3.4 and §3.7.
    pub fn validate(&self) -> Result<()> {
        for (name, p) in &self.providers {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
            {
                bail!("provider name {name:?}: use letters, digits, '_', '-' and '.'");
            }
            if p.kind == Kind::Local {
                check_local(p).with_context(|| format!("provider {name}"))?;
            } else {
                check_endpoint(&p.endpoint).with_context(|| format!("provider {name}"))?;
            }
            if let Some(k) = &p.api_key {
                if k.secret.is_empty() {
                    bail!("provider {name}: apiKey names an empty secret");
                }
                check_secret_name(&k.secret)
                    .map_err(|e| anyhow::anyhow!("provider {name}: apiKey: {e}"))?;
            }
            for (h, v) in &p.headers {
                let lower = h.to_ascii_lowercase();
                if matches!(
                    lower.as_str(),
                    "authorization" | "x-api-key" | "api-key" | "proxy-authorization"
                ) {
                    bail!(
                        "provider {name}: the header {h} carries a key; name the key with apiKey and a secret"
                    );
                }
                if axum::http::HeaderName::from_bytes(h.as_bytes()).is_err()
                    || axum::http::HeaderValue::from_str(v).is_err()
                {
                    bail!("provider {name}: invalid header {h}");
                }
            }
            if p.keep_alive.is_some() && p.kind != Kind::Ollama {
                bail!("provider {name}: keepAlive applies to the ollama kind only");
            }
            if !p.headers.is_empty() && p.kind != Kind::Openai {
                bail!("provider {name}: headers apply to the openai kind only");
            }
            if p.version.is_some() && p.kind != Kind::Anthropic {
                bail!("provider {name}: version applies to the anthropic kind only");
            }
            if p.concurrency == Some(0) {
                bail!("provider {name}: concurrency must be at least 1");
            }
            for (m, o) in std::iter::once(("(defaults)", &p.defaults))
                .chain(p.models.iter().map(|(m, o)| (m.as_str(), o)))
            {
                check_options(o).with_context(|| format!("provider {name}, model {m}"))?;
                if p.kind != Kind::Local && local_members_elsewhere(o) {
                    bail!(
                        "provider {name}, model {m}: repo, revision, path, dtype, threads, idleUnloadSecs, dimensions, maxTokens, queryPrefix and documentPrefix apply to the local kind only"
                    );
                }
                if o.num_ctx.is_some() && p.kind != Kind::Ollama {
                    bail!("provider {name}, model {m}: numCtx applies to the ollama kind only");
                }
                if o.structured_output == Some(Level::JsonObject) && p.kind == Kind::Anthropic {
                    bail!(
                        "provider {name}, model {m}: the anthropic kind offers no json-object level"
                    );
                }
                if o.structured_output == Some(Level::Tool) && p.kind == Kind::Ollama {
                    bail!("provider {name}, model {m}: the ollama kind offers no tool level");
                }
            }
            for t in [p.connect_timeout_secs] {
                if let Some(t) = t
                    && !(t.is_finite() && t > 0.0)
                {
                    bail!("provider {name}: timeouts must be positive numbers of seconds");
                }
            }
        }
        for (role, list) in &self.roles {
            for pair in list {
                let Some(p) = self.providers.get(&pair.provider) else {
                    bail!(
                        "role {}: no provider named {:?} is defined",
                        role.as_str(),
                        pair.provider
                    );
                };
                if pair.model.is_empty() {
                    bail!("role {}: an entry names no model", role.as_str());
                }
                if p.kind == Kind::Local {
                    bail!(
                        "role {}: provider {} is of the local kind, which computes embeddings and does not answer prompts",
                        role.as_str(),
                        pair.provider
                    );
                }
                if !p.allows(&pair.model) {
                    bail!(
                        "role {}: provider {} does not allow the model {:?} (allowedModels)",
                        role.as_str(),
                        pair.provider,
                        pair.model
                    );
                }
                if pair.max_output_tokens == Some(0) {
                    bail!("role {}: maxOutputTokens must be at least 1", role.as_str());
                }
            }
        }
        Ok(())
    }

    /// The pairs of `role`: its own list, or for `repair` without one, the `draft`
    /// list. Other roles without a list are off (§3.7).
    pub fn pairs(&self, role: Role) -> &[Pair] {
        match self.roles.get(&role) {
            Some(l) => l,
            None if role == Role::Repair => self.roles.get(&Role::Draft).map_or(&[], |l| l),
            None => &[],
        }
    }
}

fn check_options(o: &ModelOptions) -> Result<()> {
    if o.context_tokens == Some(0) {
        bail!("contextTokens must be at least 1");
    }
    if o.max_output_tokens == Some(0) {
        bail!("maxOutputTokens must be at least 1");
    }
    if let Some(t) = o.temperature
        && !(t.is_finite() && (0.0..=2.0).contains(&t))
    {
        bail!("temperature must be between 0 and 2");
    }
    if let Some(t) = o.request_timeout_secs
        && !(t.is_finite() && t > 0.0)
    {
        bail!("requestTimeoutSecs must be a positive number of seconds");
    }
    if let Some(p) = o.pricing
        && !(p.input_per_m_tok.is_finite()
            && p.input_per_m_tok >= 0.0
            && p.output_per_m_tok.is_finite()
            && p.output_per_m_tok >= 0.0)
    {
        bail!("pricing must hold non-negative numbers");
    }
    Ok(())
}

/// The local kind (spec F12): no endpoint or key, and each model names a snapshot by
/// `repo` and a pinned `revision`, or by `path`. The other kinds take none of the local
/// members.
fn check_local(p: &ProviderConfig) -> Result<()> {
    if !p.endpoint.is_empty() {
        bail!("the local kind takes no endpoint");
    }
    if p.api_key.is_some() {
        bail!("the local kind takes no apiKey");
    }
    for (m, o) in &p.models {
        let o = o.over(&p.defaults);
        let what = format!("model {m}");
        match (&o.repo, &o.revision, &o.path) {
            (Some(r), Some(v), None) => {
                sparkles_modelstore::check_repo(r).map_err(|e| anyhow::anyhow!("{what}: {e}"))?;
                if !sparkles_modelstore::is_pinned(v) {
                    bail!("{what}: revision must be a full 40-character commit id, not {v:?}");
                }
            }
            (None, None, Some(path)) => {
                if !std::path::Path::new(path).is_absolute() {
                    bail!("{what}: path must be absolute");
                }
            }
            _ => bail!("{what}: give repo and revision, or path"),
        }
        if let Some(d) = &o.dtype
            && !matches!(d.as_str(), "f32" | "bf16")
        {
            bail!("{what}: dtype must be f32 or bf16");
        }
        if o.threads.is_some_and(|t| t == 0 || t > 256) {
            bail!("{what}: threads must be 1 to 256");
        }
        if o.dimensions == Some(0) || o.max_tokens.is_some_and(|t| t < 2) {
            bail!("{what}: dimensions must be at least 1 and maxTokens at least 2");
        }
        for t in [&o.query_prefix, &o.document_prefix].into_iter().flatten() {
            if t.len() > 4096 {
                bail!("{what}: queryPrefix and documentPrefix take at most 4096 bytes");
            }
        }
    }
    Ok(())
}

/// Whether a provider of another kind sets a member of the local kind.
fn local_members_elsewhere(o: &ModelOptions) -> bool {
    o.repo.is_some()
        || o.revision.is_some()
        || o.path.is_some()
        || o.dtype.is_some()
        || o.threads.is_some()
        || o.idle_unload_secs.is_some()
        || o.dimensions.is_some()
        || o.max_tokens.is_some()
        || o.query_prefix.is_some()
        || o.document_prefix.is_some()
}

/// An endpoint is an absolute `http` or `https` URL without credentials, a query or a
/// fragment.
fn check_endpoint(e: &str) -> Result<()> {
    let iri = oxiri::Iri::parse(e).map_err(|err| anyhow::anyhow!("endpoint {e:?}: {err}"))?;
    if !matches!(iri.scheme(), "http" | "https") {
        bail!("endpoint {e:?}: use an http or https URL");
    }
    match iri.authority() {
        None | Some("") => bail!("endpoint {e:?}: the URL names no host"),
        Some(a) if a.contains('@') => {
            bail!(
                "endpoint {e:?}: the URL holds credentials; name the key with apiKey and a secret"
            )
        }
        _ => {}
    }
    if iri.query().is_some() || iri.fragment().is_some() {
        bail!("endpoint {e:?}: the URL must have no query or fragment");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"{
      "models": {
        "providers": {
          "local": {
            "kind": "ollama", "endpoint": "http://127.0.0.1:11434", "keepAlive": "10m",
            "models": { "qwen3:14b": { "contextTokens": 32768 } }
          },
          "gateway": {
            "kind": "openai", "endpoint": "https://llm.internal.example/v1",
            "apiKey": { "secret": "gateway" }, "structuredOutput": "json-schema"
          },
          "anthropic": {
            "kind": "anthropic", "endpoint": "https://api.anthropic.com",
            "apiKey": { "secret": "anthropic" },
            "allowedModels": ["claude-haiku-5-5", "claude-sonnet-5-5"],
            "models": { "claude-haiku-5-5": { "pricing": { "inputPerMTok": 0.10, "outputPerMTok": 0.50 } } }
          }
        },
        "roles": {
          "draft": [ { "provider": "local", "model": "qwen3:14b" },
                     { "provider": "anthropic", "model": "claude-sonnet-5-5", "maxOutputTokens": 1024 } ]
        }
      }
    }"#;

    #[test]
    fn the_spec_example_parses() {
        let c = ModelsConfig::parse(EXAMPLE).unwrap();
        assert_eq!(c.providers.len(), 3);
        let local = &c.providers["local"];
        assert_eq!(local.model("qwen3:14b").context_tokens, 32768);
        assert_eq!(local.model("other").context_tokens, DEFAULT_CONTEXT_TOKENS);
        assert_eq!(local.model("other").temperature, Some(0.0));
        assert_eq!(
            c.providers["gateway"].model("m").structured_output,
            Level::JsonSchema
        );
        // never sent to Claude models unless configured
        assert_eq!(
            c.providers["anthropic"]
                .model("claude-haiku-5-5")
                .temperature,
            None
        );
        assert_eq!(c.pairs(Role::Draft).len(), 2);
        // repair falls back to draft; the other roles are off
        assert_eq!(c.pairs(Role::Repair), c.pairs(Role::Draft));
        assert!(c.pairs(Role::Summarize).is_empty());
        // the bare form parses too
        let v: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
        assert_eq!(ModelsConfig::parse(&v["models"].to_string()).unwrap(), c);
    }

    #[test]
    fn refusals() {
        let bad = |patch: &str| {
            let mut v: serde_json::Value = serde_json::from_str(EXAMPLE).unwrap();
            let p: serde_json::Value = serde_json::from_str(patch).unwrap();
            for (k, val) in p.as_object().unwrap() {
                let mut at = &mut v["models"];
                let parts: Vec<&str> = k.split('.').collect();
                for part in &parts[..parts.len() - 1] {
                    at = &mut at[*part];
                }
                at[parts[parts.len() - 1]] = val.clone();
            }
            format!("{:#}", ModelsConfig::parse(&v.to_string()).unwrap_err())
        };
        assert!(
            bad(r#"{"roles.draft": [{"provider": "nope", "model": "m"}]}"#).contains("no provider")
        );
        assert!(
            bad(r#"{"roles.draft": [{"provider": "anthropic", "model": "claude-opus-5-5"}]}"#)
                .contains("allowedModels")
        );
        assert!(bad(r#"{"roles.drafts": []}"#).contains("unknown variant"));
        assert!(
            bad(r#"{"providers.gateway.endpoint": "https://user:pw@x.example/v1"}"#)
                .contains("credentials")
        );
        // a key's value pasted as apiKey is refused without being quoted
        let e = bad(r#"{"providers.gateway.apiKey": "sk-123"}"#);
        assert!(e.contains("apiKey must be"), "{e}");
        assert!(!e.contains("sk-123"), "{e}");
        let e = bad(r#"{"providers.gateway.apiKey": {"secret": "gw", "value": "sk-123"}}"#);
        assert!(!e.contains("sk-123"), "{e}");
        assert!(bad(r#"{"providers.gateway.apiKey": {"secret": "../x"}}"#).contains("secret name"));
        assert!(
            bad(r#"{"providers.gateway.headers": {"Authorization": "Bearer x"}}"#)
                .contains("carries a key")
        );
        assert!(bad(r#"{"providers.local.version": "1"}"#).contains("anthropic kind"));
        assert!(
            bad(r#"{"providers.anthropic.structuredOutput": "json-object"}"#)
                .contains("json-object")
        );
        assert!(bad(r#"{"providers.local.endpoint": "ftp://x"}"#).contains("http"));
        assert!(bad(r#"{"providers.local.apiKeys": {"secret": "x"}}"#).contains("unknown field"));
    }

    #[test]
    fn pairs_parse() {
        assert_eq!(
            Pair::parse("local/qwen3:8b"),
            Some(Pair::new("local", "qwen3:8b"))
        );
        assert_eq!(
            Pair::parse("gw/org/model"),
            Some(Pair::new("gw", "org/model"))
        );
        assert_eq!(Pair::parse("x"), None);
        assert_eq!(Pair::parse("/m"), None);
    }
}
