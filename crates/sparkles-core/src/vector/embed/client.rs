//! The client of an OpenAI-compatible embeddings endpoint (`POST /v1/embeddings`):
//! one request, retries with backoff, and splitting a rejected batch.

use super::config::{ApiKey, EmbeddingConfig};
use super::{Environment, SecretSource};
use std::time::Duration;

/// Why a request produced no vectors.
#[derive(Clone, Debug, PartialEq)]
pub enum CallError {
    /// worth retrying: a network error, a timeout, `429` or `5xx` (with the provider's
    /// `Retry-After`, if any)
    Transient(String, Option<Duration>),
    /// `401` or `403`: the key is missing, wrong or not allowed
    Auth(String),
    /// another `4xx`: the provider refused these inputs
    Rejected(String),
    /// the outbound policy refuses the endpoint
    Refused(String),
    /// the configuration cannot work (a missing secret, an answer that is not an
    /// embeddings response)
    Fatal(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Transient(m, _)
            | CallError::Auth(m)
            | CallError::Rejected(m)
            | CallError::Refused(m)
            | CallError::Fatal(m) => f.write_str(m),
        }
    }
}

/// The bearer token of `key`, read now.
pub(crate) fn resolve_key(env: &Environment, key: &ApiKey) -> Result<String, String> {
    let read_env =
        |v: &str| std::env::var(v).map_err(|_| format!("the environment variable {v} is not set"));
    let read_file = |p: &std::path::Path| {
        std::fs::read_to_string(p)
            .map(|s| s.trim().to_string())
            .map_err(|e| format!("cannot read the API key file {}: {e}", p.display()))
    };
    let k = match key {
        ApiKey::Env(v) => read_env(v)?,
        ApiKey::File(p) => read_file(std::path::Path::new(p))?,
        ApiKey::Secret(n) => match env.secrets.get(n) {
            Some(SecretSource::Env(v)) => read_env(v)?,
            Some(SecretSource::File(p)) => read_file(p)?,
            None => return Err(format!("no embedding secret named {n:?} is defined")),
        },
    };
    if k.trim().is_empty() {
        return Err("the API key is empty".into());
    }
    Ok(k.trim().to_string())
}

/// The vectors of a model run in the process, checked like a provider's answer.
fn local(
    m: &dyn super::LocalModel,
    cfg: &EmbeddingConfig,
    dimension: usize,
    inputs: &[String],
    query: bool,
) -> Result<Vec<Result<Vec<f32>, String>>, CallError> {
    let vs = m.embed(inputs, query, cfg.send_dimensions.then_some(dimension))?;
    if vs.len() != inputs.len() {
        return Err(CallError::Fatal(format!(
            "{}: {} vectors for {} inputs",
            cfg.endpoint(),
            vs.len(),
            inputs.len()
        )));
    }
    Ok(vs
        .into_iter()
        .map(|v| {
            if v.len() != dimension {
                Err(format!(
                    "the model returned a vector of dimension {}; the index expects {dimension}",
                    v.len()
                ))
            } else if v.iter().any(|x| !x.is_finite()) {
                Err("the vector holds a value that is not a finite number".to_string())
            } else {
                Ok(v)
            }
        })
        .collect())
}

/// One request for the vectors of `inputs`, in order. `query` tells a local model to
/// use its query prompt.
pub(crate) fn request(
    env: &Environment,
    cfg: &EmbeddingConfig,
    dimension: usize,
    inputs: &[String],
    query: bool,
) -> Result<Vec<Result<Vec<f32>, String>>, CallError> {
    let mut body = serde_json::json!({ "model": cfg.model, "input": inputs });
    if cfg.send_dimensions {
        body["dimensions"] = dimension.into();
    }
    let (url, auth) = match &cfg.provider {
        Some(p) => {
            let providers = env.providers.as_ref().ok_or_else(|| {
                CallError::Fatal(format!(
                    "no model configuration defines the provider {p:?} in this process"
                ))
            })?;
            match providers.resolve(p, &cfg.model)? {
                super::Target::Local(m) => return local(&*m, cfg, dimension, inputs, query),
                super::Target::Remote { url, bearer } => {
                    (url, bearer.map(|k| format!("Bearer {k}")))
                }
            }
        }
        None => {
            let key = match &cfg.api_key {
                Some(k) => Some(resolve_key(env, k).map_err(CallError::Fatal)?),
                None => None,
            };
            (cfg.url.clone(), key.map(|k| format!("Bearer {k}")))
        }
    };
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(a) = &auth {
        headers.push(("Authorization", a.as_str()));
    }
    let timeout = Duration::from_secs_f64(cfg.timeout_secs);
    let posted = crate::outbound::post_json(
        &env.outbound,
        &url,
        &headers,
        serde_json::to_vec(&body).expect("serializable"),
        timeout,
    )
    .map_err(|f| match f {
        crate::outbound::Failure::Refused(m) => {
            CallError::Refused(format!("{}: {m}", cfg.endpoint()))
        }
        f => CallError::Transient(format!("{}: {f}", cfg.endpoint()), None),
    })?;
    let status = posted.status.as_u16();
    if !(200..300).contains(&status) {
        let detail = provider_message(&posted.body);
        let m = format!("{} answered {}{detail}", cfg.endpoint(), posted.status);
        return Err(match status {
            401 | 403 => CallError::Auth(m),
            408 | 409 | 425 | 429 | 500..=599 => {
                CallError::Transient(m, posted.retry_after.as_deref().and_then(retry_after))
            }
            _ => CallError::Rejected(m),
        });
    }
    parse_response(&posted.body, inputs.len(), dimension)
        .map_err(|m| CallError::Fatal(format!("{}: {m}", cfg.endpoint())))
}

/// `Retry-After` in seconds (an HTTP date is not followed; the backoff applies).
fn retry_after(v: &str) -> Option<Duration> {
    v.trim()
        .parse::<u64>()
        .ok()
        .map(|s| Duration::from_secs(s.min(3600)))
}

/// `: message` from an OpenAI-style error body (`{"error": {"message": …}}` or
/// `{"error": "…"}`), at most 300 characters.
fn provider_message(body: &[u8]) -> String {
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let m = v["error"]["message"]
        .as_str()
        .or_else(|| v["error"].as_str())
        .or_else(|| v["message"].as_str())
        .or_else(|| v["detail"].as_str())
        .unwrap_or("");
    if m.is_empty() {
        return String::new();
    }
    let m: String = m.chars().take(300).collect();
    format!(": {m}")
}

/// The vectors of an embeddings response: `data[i].embedding` ordered by `data[i].index`.
/// A vector of another dimension, or with a value that is not a finite `f32`, fails
/// its input alone.
pub(crate) fn parse_response(
    body: &[u8],
    n: usize,
    dimension: usize,
) -> Result<Vec<Result<Vec<f32>, String>>, String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| "the answer is not JSON".to_string())?;
    let data = v["data"]
        .as_array()
        .ok_or("the answer has no data array (is the URL an embeddings endpoint?)")?;
    if data.len() != n {
        return Err(format!("{} vectors for {n} inputs", data.len()));
    }
    let mut out: Vec<Option<Result<Vec<f32>, String>>> = vec![None; n];
    for (pos, d) in data.iter().enumerate() {
        let i = d["index"].as_u64().map_or(pos, |i| i as usize);
        if i >= n || out[i].is_some() {
            return Err(format!("the answer's index {i} is out of place"));
        }
        let r = match d["embedding"].as_array() {
            None => Err("the answer has no embedding array (only float encoding is read)".into()),
            Some(a) if a.len() != dimension => Err(format!(
                "the provider returned a vector of dimension {}; the index expects {dimension}",
                a.len()
            )),
            Some(a) => a
                .iter()
                .map(|x| {
                    x.as_f64()
                        .map(|f| f as f32)
                        .filter(|f| f.is_finite())
                        .ok_or_else(|| {
                            "the vector holds a value that is not a finite number".to_string()
                        })
                })
                .collect(),
        };
        out[i] = Some(r);
    }
    Ok(out
        .into_iter()
        .map(|r| r.expect("every index filled"))
        .collect())
}

/// What [`embed`] needs from its caller: whether to stop, and a way to wait.
pub(crate) struct Waits<'a> {
    /// waits up to the duration; `false` when the caller is closing
    pub sleep: &'a dyn Fn(Duration) -> bool,
}

/// The vectors of `inputs`, retrying transient failures up to `maxRetries` times
/// (exponential backoff from 1 s to 60 s with jitter, or `Retry-After`) and splitting a
/// rejected batch in halves until the inputs at fault fail alone. `Err` keeps the whole
/// batch for later; counts requests in `requests`.
pub(crate) fn embed(
    env: &Environment,
    cfg: &EmbeddingConfig,
    dimension: usize,
    inputs: &[String],
    query: bool,
    waits: &Waits<'_>,
    requests: &mut u64,
) -> Result<Vec<Result<Vec<f32>, String>>, CallError> {
    let mut attempt = 0u32;
    loop {
        *requests += 1;
        match request(env, cfg, dimension, inputs, query) {
            Ok(v) => return Ok(v),
            Err(CallError::Transient(m, after)) => {
                if attempt >= cfg.max_retries {
                    return Err(CallError::Transient(m, after));
                }
                let base = Duration::from_secs(1 << attempt.min(6)).min(Duration::from_secs(60));
                let jitter = Duration::from_millis(rand::random::<u64>() % 250);
                let wait = after.unwrap_or(base + jitter);
                tracing::info!(target: "sparkles::embed", "retrying in {wait:?}: {m}");
                if !(waits.sleep)(wait) {
                    return Err(CallError::Transient(m, after));
                }
                attempt += 1;
            }
            Err(CallError::Rejected(m)) if inputs.len() > 1 => {
                let (a, b) = inputs.split_at(inputs.len() / 2);
                tracing::info!(target: "sparkles::embed", "splitting a batch of {}: {m}", inputs.len());
                let mut out = embed(env, cfg, dimension, a, query, waits, requests)?;
                out.extend(embed(env, cfg, dimension, b, query, waits, requests)?);
                return Ok(out);
            }
            Err(CallError::Rejected(m)) => return Ok(vec![Err(m)]),
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses() {
        let ok = br#"{"data":[{"index":1,"embedding":[3,4]},{"index":0,"embedding":[1,2]}]}"#;
        assert_eq!(
            parse_response(ok, 2, 2).unwrap(),
            vec![Ok(vec![1.0, 2.0]), Ok(vec![3.0, 4.0])]
        );
        let wrong_dim = br#"{"data":[{"embedding":[1,2,3]}]}"#;
        assert!(
            parse_response(wrong_dim, 1, 2).unwrap()[0]
                .as_ref()
                .unwrap_err()
                .contains("dimension 3")
        );
        assert!(
            parse_response(br#"{"data":[]}"#, 1, 2)
                .unwrap_err()
                .contains("0 vectors")
        );
        assert!(parse_response(b"<html>", 1, 2).is_err());
        assert!(parse_response(br#"{"data":[{"embedding":[1e40]}]}"#, 1, 1).unwrap()[0].is_err());
        assert_eq!(
            provider_message(br#"{"error":{"message":"bad input"}}"#),
            ": bad input"
        );
        assert_eq!(retry_after("7"), Some(Duration::from_secs(7)));
        assert_eq!(retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    }
}
