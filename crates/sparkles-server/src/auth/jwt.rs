//! JWTs signed by an identity provider: a cached JSON Web Key Set that follows key
//! rotation, and the validation shared by ID tokens, API access tokens, back-channel
//! logout tokens and Cloudflare Access assertions (RFC 7519 §7.2, RFC 7515 §4.1.11).
//!
//! Only the asymmetric algorithms of the caller's allow-list are accepted; `none` and
//! HMAC never are, and a header naming critical extensions or an encryption is refused.
//! The key is taken from the provider's key set by `kid` (never from the token's own
//! `jwk`, `jku`, `x5u` or `x5c`), and a key whose `use` or `alg` says otherwise is not
//! used.

use anyhow::{Context, Result, bail};
use jsonwebtoken::jwk::{Jwk, JwkSet, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::{Map, Value as J};
use std::time::{Duration, Instant};

/// Clock skew tolerated on `exp`, `nbf` and `iat`.
pub const LEEWAY_SECS: u64 = 60;
/// The key set is fetched again after this long.
pub const JWKS_MAX_AGE: Duration = Duration::from_secs(3600);
/// A token signed with a key the cached set does not hold fetches the set again, at
/// most this often (keys rotate; random key ids must not make the server hammer the
/// provider).
pub const JWKS_MIN_REFRESH: Duration = Duration::from_secs(300);
/// Longest token accepted (a few KiB in practice).
pub const MAX_TOKEN_LEN: usize = 16 << 10;

/// Why a JWT was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwtError {
    /// not a JWS compact serialization, or an unreadable header
    Malformed,
    /// a bad signature, an unknown key, a wrong issuer or audience, a missing claim …
    Invalid(String),
    Expired,
    /// the key set (or the provider's discovery document) could not be fetched
    Unavailable(String),
}

impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JwtError::Malformed => write!(f, "malformed token"),
            JwtError::Invalid(why) => write!(f, "invalid token: {why}"),
            JwtError::Expired => write!(f, "token expired"),
            JwtError::Unavailable(why) => write!(f, "keys unavailable: {why}"),
        }
    }
}

/// Whether `s` has the shape of a JWS in compact serialization: three base64url
/// segments (the signature may not be empty).
pub fn looks_like_jwt(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    s.len() <= MAX_TOKEN_LEN
        && parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

/// A provider's key set, fetched on first use, kept for [`JWKS_MAX_AGE`] and fetched
/// again for an unknown key id at most once per [`JWKS_MIN_REFRESH`]. Concurrent
/// requests share one fetch.
pub struct Jwks {
    http: reqwest::Client,
    cache: tokio::sync::RwLock<Option<(JwkSet, Instant)>>,
    /// when an unknown key id last caused a fetch
    forced: parking_lot::Mutex<Option<Instant>>,
    fetching: tokio::sync::Mutex<()>,
}

impl Jwks {
    pub fn new(http: reqwest::Client) -> Jwks {
        Jwks {
            http,
            cache: tokio::sync::RwLock::new(None),
            forced: parking_lot::Mutex::new(None),
            fetching: tokio::sync::Mutex::new(()),
        }
    }

    /// The key set at `url`. With `unknown_kid`, a fresh copy unless one was fetched for
    /// that reason within [`JWKS_MIN_REFRESH`] (`None`: no fresher copy may be fetched).
    async fn get(&self, url: &str, unknown_kid: bool) -> Result<Option<JwkSet>> {
        let fresh = |c: &Option<(JwkSet, Instant)>| {
            c.as_ref()
                .filter(|(_, at)| at.elapsed() < JWKS_MAX_AGE)
                .map(|(s, _)| s.clone())
        };
        if !unknown_kid && let Some(s) = fresh(&*self.cache.read().await) {
            return Ok(Some(s));
        }
        let _one = self.fetching.lock().await;
        let started = Instant::now();
        if unknown_kid {
            let mut f = self.forced.lock();
            if f.is_some_and(|t| t.elapsed() < JWKS_MIN_REFRESH) {
                return Ok(None);
            }
            *f = Some(started);
        } else if let Some(s) = fresh(&*self.cache.read().await) {
            // another request fetched it while this one waited
            return Ok(Some(s));
        }
        match get_json::<JwkSet>(&self.http, url).await {
            Ok(set) => {
                *self.cache.write().await = Some((set.clone(), Instant::now()));
                Ok(Some(set))
            }
            Err(e) => {
                // a provider that is briefly down: the keys it published an hour ago
                // still verify its tokens
                if !unknown_kid && let Some((set, _)) = &*self.cache.read().await {
                    tracing::warn!("cannot refresh the key set {url}, using the cached one: {e:#}");
                    return Ok(Some(set.clone()));
                }
                Err(e)
            }
        }
    }
}

/// The largest response body read from an identity provider. Discovery documents, key
/// sets, token responses and UserInfo answers are a few KiB.
pub const MAX_IDP_BODY: usize = 1 << 20;

/// The HTTP client for an identity provider at `base` (an issuer or team domain).
///
/// It has a 10 s timeout and follows no redirects. It uses the proxy named by the
/// environment (`HTTPS_PROXY`, `NO_PROXY`), because a provider outside the network is
/// often reachable only through one. HTTPS goes through such a proxy as a tunnel, so the
/// proxy sees the host name and not the token exchange. A loopback provider over plain
/// HTTP never uses a proxy, since the proxy would then see the codes and tokens.
pub fn idp_client(base: &str) -> Result<reqwest::Client> {
    let b = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none());
    let loopback = reqwest::Url::parse(base).is_ok_and(|u| {
        matches!(
            u.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        )
    });
    Ok(if loopback { b.no_proxy() } else { b }.build()?)
}

/// Read a provider's response body, refusing one larger than [`MAX_IDP_BODY`].
pub async fn read_capped(mut r: reqwest::Response, what: &str) -> Result<Vec<u8>> {
    let too_large = || anyhow::anyhow!("{what}: the response is larger than {MAX_IDP_BODY} bytes");
    if r.content_length().is_some_and(|n| n > MAX_IDP_BODY as u64) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = r.chunk().await.with_context(|| what.to_string())? {
        if body.len() + chunk.len() > MAX_IDP_BODY {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// GET a JSON document (no redirects are followed: the client is built without them).
pub async fn get_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
) -> Result<T> {
    let r = http
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = r.status();
    let body = read_capped(r, &format!("GET {url}")).await?;
    if !status.is_success() {
        bail!("GET {url}: {status}");
    }
    serde_json::from_slice(&body).with_context(|| format!("GET {url}: invalid JSON"))
}

/// What a token must satisfy beyond its signature.
pub struct Expect<'a> {
    pub issuer: &'a str,
    /// one of them must be in `aud`
    pub audiences: &'a [String],
    pub algorithms: &'a [Algorithm],
    /// registered claims that must be present besides `iss` and `aud` (`exp`, `sub`,
    /// `nbf`)
    pub required: &'a [&'a str],
}

/// The key of `set` for a token with this header: by key id, or the only signing key
/// when the token names none; a key whose `use` or `alg` contradicts the header is not
/// a match.
fn pick<'a>(set: &'a JwkSet, kid: Option<&str>, alg: Algorithm) -> Option<&'a Jwk> {
    // a key without `alg` may serve any algorithm of its type (the decoder refuses one
    // of another family)
    let usable = |k: &&Jwk| {
        !matches!(&k.common.public_key_use, Some(u) if *u != PublicKeyUse::Signature)
            && k.common
                .key_algorithm
                .is_none_or(|a| a.to_string() == format!("{alg:?}"))
    };
    match kid {
        Some(kid) => set
            .keys
            .iter()
            .filter(usable)
            .find(|k| k.common.key_id.as_deref() == Some(kid)),
        None => {
            let mut keys = set.keys.iter().filter(usable);
            let first = keys.next()?;
            keys.next().is_none().then_some(first)
        }
    }
}

/// Check a JWT's signature against the key set at `jwks_url` and its claims against
/// `e`; returns the claims.
pub async fn verify(
    token: &str,
    jwks: &Jwks,
    jwks_url: &str,
    e: &Expect<'_>,
) -> Result<Map<String, J>, JwtError> {
    if !looks_like_jwt(token) {
        return Err(JwtError::Malformed);
    }
    // `alg: none` and unknown algorithms fail to parse here
    let header = jsonwebtoken::decode_header(token).map_err(|_| JwtError::Malformed)?;
    if !e.algorithms.contains(&header.alg) {
        return Err(JwtError::Invalid(format!(
            "algorithm {:?} is not accepted",
            header.alg
        )));
    }
    if header.crit.as_ref().is_some_and(|c| !c.is_empty()) {
        return Err(JwtError::Invalid(
            "the header names critical extensions".into(),
        ));
    }
    if header.enc.is_some() {
        return Err(JwtError::Invalid("an encrypted token".into()));
    }
    let kid = header.kid.as_deref();
    let set = jwks
        .get(jwks_url, false)
        .await
        .map_err(|e| JwtError::Unavailable(format!("{e:#}")))?
        .unwrap_or(JwkSet { keys: Vec::new() });
    let jwk = match pick(&set, kid, header.alg) {
        Some(k) => k.clone(),
        None => {
            // the provider may have rotated its keys since the set was fetched
            let fresh = jwks
                .get(jwks_url, true)
                .await
                .map_err(|e| JwtError::Unavailable(format!("{e:#}")))?;
            fresh
                .as_ref()
                .and_then(|s| pick(s, kid, header.alg))
                .cloned()
                .ok_or_else(|| JwtError::Invalid("no key of the provider matches".into()))?
        }
    };
    let key =
        DecodingKey::from_jwk(&jwk).map_err(|e| JwtError::Invalid(format!("unusable key: {e}")))?;
    let mut v = Validation::new(header.alg);
    v.leeway = LEEWAY_SECS;
    v.validate_nbf = true;
    v.validate_exp = true;
    v.set_issuer(&[e.issuer]);
    v.set_audience(e.audiences);
    let mut required = vec!["iss", "aud"];
    required.extend_from_slice(e.required);
    v.set_required_spec_claims(&required);
    let data = jsonwebtoken::decode::<Map<String, J>>(token, &key, &v).map_err(|err| {
        use jsonwebtoken::errors::ErrorKind;
        match err.kind() {
            ErrorKind::ExpiredSignature => JwtError::Expired,
            k => JwtError::Invalid(format!("{k:?}")),
        }
    })?;
    let claims = data.claims;
    // an `iss` array satisfies jsonwebtoken when it contains the issuer: it must be the
    // issuer itself
    if claims.get("iss").and_then(J::as_str) != Some(e.issuer) {
        return Err(JwtError::Invalid("wrong issuer".into()));
    }
    if let Some(iat) = claims.get("iat") {
        let now = jsonwebtoken::get_current_timestamp() as f64;
        match iat.as_f64() {
            Some(t) if t <= now + LEEWAY_SECS as f64 => {}
            Some(_) => return Err(JwtError::Invalid("issued in the future".into())),
            None => return Err(JwtError::Invalid("iat is not a number".into())),
        }
    }
    Ok(claims)
}

/// A claim as one string or a list of strings (`aud`, `groups`).
pub fn strings(v: Option<&J>) -> Vec<String> {
    match v {
        Some(J::String(s)) => vec![s.clone()],
        Some(J::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// The scopes an access token grants: `scope` (space-separated, RFC 8693 §4.2 and RFC
/// 9068 §2.2.3) or `scp` (a list or a string, as some providers write it).
pub fn scopes(claims: &Map<String, J>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in ["scope", "scp"] {
        match claims.get(k) {
            Some(J::String(s)) => out.extend(s.split_whitespace().map(str::to_string)),
            Some(J::Array(a)) => {
                out.extend(a.iter().filter_map(|x| x.as_str().map(str::to_string)))
            }
            _ => {}
        }
    }
    out
}

/// An account name from a claim: 1–256 bytes without control characters.
pub fn name_claim(claims: &Map<String, J>, claim: &str) -> Option<String> {
    claims
        .get(claim)
        .and_then(J::as_str)
        .filter(|n| !n.is_empty() && n.len() <= 256 && !n.chars().any(char::is_control))
        .map(str::to_string)
}

// ------------------------------------------------------------------ Cloudflare ------

/// Cloudflare Access (`[cloudflare_access]`): the `Cf-Access-Jwt-Assertion` header the
/// Access edge adds to every request it lets through, verified against the team's keys.
pub struct CloudflareAccess {
    /// `https://<team>.cloudflareaccess.com`, the tokens' issuer
    pub team_domain: String,
    /// the application's AUD tags
    pub audiences: Vec<String>,
    pub groups_claim: Option<String>,
    keys: Jwks,
}

/// The header Cloudflare Access adds.
pub const CF_ASSERTION: &str = "cf-access-jwt-assertion";

impl CloudflareAccess {
    pub fn new(
        team_domain: &str,
        audiences: Vec<String>,
        groups_claim: Option<String>,
    ) -> Result<CloudflareAccess> {
        let http = idp_client(team_domain)?;
        Ok(CloudflareAccess {
            team_domain: team_domain.trim_end_matches('/').to_string(),
            audiences,
            groups_claim,
            keys: Jwks::new(http),
        })
    }

    fn certs_url(&self) -> String {
        format!("{}/cdn-cgi/access/certs", self.team_domain)
    }

    /// Verify an assertion; returns the claims.
    pub async fn verify(&self, token: &str) -> Result<Map<String, J>, JwtError> {
        verify(
            token,
            &self.keys,
            &self.certs_url(),
            &Expect {
                issuer: &self.team_domain,
                audiences: &self.audiences,
                algorithms: &[Algorithm::RS256],
                required: &["exp"],
            },
        )
        .await
    }

    /// The account an assertion names: the user's `email`, or a service token's
    /// `common_name` (its client id); `true` for a service token.
    pub fn account(claims: &Map<String, J>) -> Option<(String, bool)> {
        match name_claim(claims, "email") {
            Some(e) => Some((e, false)),
            None => name_claim(claims, "common_name").map(|c| (c, true)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A body over the cap fails, with or without a `Content-Length`.
    #[tokio::test]
    async fn provider_bodies_are_capped() {
        use axum::body::Body;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let big = || vec![b' '; MAX_IDP_BODY + 1];
        let app = axum::Router::new()
            .route("/small", axum::routing::get(|| async { "{}" }))
            .route("/sized", axum::routing::get(move || async move { big() }))
            .route(
                "/chunked",
                axum::routing::get(move || async move {
                    let chunks = (0..17).map(|_| Ok::<_, std::io::Error>(vec![b' '; 64 * 1024]));
                    Body::from_stream(futures_util::stream::iter(chunks))
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = idp_client(&base).unwrap();
        let ok: J = get_json(&http, &format!("{base}/small")).await.unwrap();
        assert_eq!(ok, json!({}));
        for path in ["sized", "chunked"] {
            let e = get_json::<J>(&http, &format!("{base}/{path}"))
                .await
                .unwrap_err();
            assert!(format!("{e:#}").contains("larger than"), "{path}: {e:#}");
        }
    }

    #[test]
    fn shapes() {
        assert!(looks_like_jwt("eyJh.eyJi.c2ln"));
        assert!(!looks_like_jwt("eyJh.eyJi."));
        assert!(!looks_like_jwt("eyJh.eyJi"));
        assert!(!looks_like_jwt("a.b.c.d"));
        assert!(!looks_like_jwt("spk_abc"));
        assert!(!looks_like_jwt("a+b.c.d"));
        assert!(!looks_like_jwt(&format!(
            "a.b.{}",
            "c".repeat(MAX_TOKEN_LEN)
        )));
    }

    #[test]
    fn claims() {
        let c = json!({ "scope": "openid sparkles", "scp": ["a"], "email": "x@y", "bad": "a\nb" });
        let c = c.as_object().unwrap();
        assert_eq!(scopes(c), ["openid", "sparkles", "a"]);
        assert_eq!(name_claim(c, "email").as_deref(), Some("x@y"));
        assert_eq!(name_claim(c, "bad"), None);
        assert_eq!(name_claim(c, "none"), None);
        assert_eq!(strings(Some(&json!(["a", 1, "b"]))), ["a", "b"]);
        assert_eq!(strings(Some(&json!("a"))), ["a"]);
        let cf = json!({ "common_name": "abc.access" });
        assert_eq!(
            CloudflareAccess::account(cf.as_object().unwrap()),
            Some(("abc.access".into(), true))
        );
    }
}
