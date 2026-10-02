//! OpenID Connect relying party: discovery, the authorization code flow with PKCE, ID
//! token validation (signature against the provider's JWKS, `iss`, `aud`, `azp`, `exp`,
//! `iat` and `nonce`), the provider's access tokens on the API (`iss`, `aud`, `exp`,
//! `nbf` and required scopes) and back-channel logout tokens (OpenID Connect
//! Back-Channel Logout 1.0).
//!
//! Signatures are checked by `jsonwebtoken` through [`super::jwt`]; everything else
//! (discovery, the token request, claim checks) is here. Only asymmetric algorithms can
//! be configured.

use super::crypto;
use super::jwt::{self, Expect, JwtError};
use anyhow::{Context, Result, anyhow, bail};
use jsonwebtoken::Algorithm;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Map, Value as J};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;

/// An asymmetric JWS algorithm by name (`RS256`, `ES256`, …); HMAC and `none` are
/// refused.
pub fn parse_algorithm(name: &str) -> Result<Algorithm> {
    Ok(match name {
        "RS256" => Algorithm::RS256,
        "RS384" => Algorithm::RS384,
        "RS512" => Algorithm::RS512,
        "PS256" => Algorithm::PS256,
        "PS384" => Algorithm::PS384,
        "PS512" => Algorithm::PS512,
        "ES256" => Algorithm::ES256,
        "ES384" => Algorithm::ES384,
        "EdDSA" => Algorithm::EdDSA,
        _ => bail!("oidc.algorithms: '{name}' is not an accepted asymmetric algorithm"),
    })
}

/// Clock skew tolerated on `exp`, `nbf` and `iat`.
const LEEWAY_SECS: u64 = jwt::LEEWAY_SECS;
/// The event a back-channel logout token carries.
pub const BACKCHANNEL_EVENT: &str = "http://schemas.openid.net/event/backchannel-logout";
/// A logout token issued longer ago than this is refused (and its `jti` is remembered
/// twice as long).
pub const LOGOUT_TOKEN_MAX_AGE: i64 = 600;
/// Logout token ids remembered at most.
const MAX_SEEN_JTI: usize = 10_000;
/// After a failed discovery, logins fail fast for this long.
const DISCOVERY_BACKOFF: Duration = Duration::from_secs(30);
/// Pending logins: at most this many, each valid this long (seconds).
const MAX_PENDING: usize = 10_000;
pub const PENDING_TTL: i64 = 600;

/// Relying-party settings (from the `[oidc]` section of the auth configuration).
#[derive(Clone, Debug)]
pub struct OidcSettings {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    /// the callback URL registered with the provider (`…/$/auth/oidc/callback`)
    pub redirect_url: String,
    pub scopes: Vec<String>,
    pub algorithms: Vec<Algorithm>,
    /// audiences of access tokens accepted on the API; empty: none are
    pub api_audiences: Vec<String>,
    /// scopes an API access token must all grant
    pub api_scopes: Vec<String>,
}

/// The parts of the provider's discovery document the flow uses.
#[derive(Clone, Debug, Deserialize)]
pub struct Metadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
    #[serde(default)]
    pub end_session_endpoint: Option<String>,
    #[serde(default)]
    pub id_token_signing_alg_values_supported: Vec<String>,
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Vec<String>,
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
}

pub struct Oidc {
    pub settings: OidcSettings,
    http: reqwest::Client,
    metadata: OnceCell<Metadata>,
    discovery_failed: Mutex<Option<Instant>>,
    keys: jwt::Jwks,
    /// ids of the logout tokens seen recently, with when they were seen (a replay is
    /// refused)
    seen_jti: Mutex<HashMap<String, i64>>,
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
}

/// `http://` is accepted only for loopback hosts (development and tests).
pub fn check_url(what: &str, url: &str) -> Result<()> {
    let u = reqwest::Url::parse(url).with_context(|| format!("{what}: invalid URL '{url}'"))?;
    match u.scheme() {
        "https" => Ok(()),
        "http"
            if matches!(
                u.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
            ) =>
        {
            Ok(())
        }
        _ => bail!("{what} must use https: {url}"),
    }
}

impl Oidc {
    pub fn new(settings: OidcSettings) -> Result<Oidc> {
        check_url("oidc.issuer", &settings.issuer)?;
        // redirects are not followed (a discovery or token endpoint must not bounce
        // requests elsewhere)
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Oidc {
            settings,
            keys: jwt::Jwks::new(http.clone()),
            http,
            metadata: OnceCell::new(),
            discovery_failed: Mutex::new(None),
            seen_jti: Mutex::new(HashMap::new()),
        })
    }

    /// The discovery document, fetched on first use (and again after a failure).
    pub async fn metadata(&self) -> Result<&Metadata> {
        if let Some(m) = self.metadata.get() {
            return Ok(m);
        }
        if self
            .discovery_failed
            .lock()
            .is_some_and(|t| t.elapsed() < DISCOVERY_BACKOFF)
        {
            bail!("the OIDC provider was unreachable moments ago");
        }
        let r = self
            .metadata
            .get_or_try_init(|| async {
                let url = format!(
                    "{}/.well-known/openid-configuration",
                    self.settings.issuer.trim_end_matches('/')
                );
                let m: Metadata = self.get_json(&url).await.context("OIDC discovery")?;
                // OpenID Connect Discovery 1.0, section 4.3
                if m.issuer != self.settings.issuer {
                    bail!(
                        "OIDC discovery: the provider's issuer '{}' differs from the configured '{}'",
                        m.issuer,
                        self.settings.issuer
                    );
                }
                for (what, u) in [
                    ("authorization_endpoint", &m.authorization_endpoint),
                    ("token_endpoint", &m.token_endpoint),
                    ("jwks_uri", &m.jwks_uri),
                ] {
                    check_url(what, u)?;
                }
                if !m.code_challenge_methods_supported.is_empty()
                    && !m.code_challenge_methods_supported.iter().any(|c| c == "S256")
                {
                    bail!("the OIDC provider does not support PKCE with S256");
                }
                Ok(m)
            })
            .await;
        if r.is_err() {
            *self.discovery_failed.lock() = Some(Instant::now());
        }
        r
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        jwt::get_json(&self.http, url).await
    }

    /// The URL that starts a login at the provider.
    pub async fn authorize_url(&self, state: &str, nonce: &str, verifier: &str) -> Result<String> {
        let m = self.metadata().await?;
        let mut u = reqwest::Url::parse(&m.authorization_endpoint)?;
        u.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.settings.client_id)
            .append_pair("redirect_uri", &self.settings.redirect_url)
            .append_pair("scope", &self.settings.scopes.join(" "))
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge", &crypto::pkce_challenge(verifier))
            .append_pair("code_challenge_method", "S256");
        Ok(u.into())
    }

    /// Redeem an authorization code; returns the validated ID token's claims and the ID
    /// token itself. When a claim of `needed` is missing from the ID token and the
    /// provider has a UserInfo endpoint, its response fills the gaps (OIDC Core 5.3).
    pub async fn redeem(
        &self,
        code: &str,
        verifier: &str,
        nonce: &str,
        needed: &[&str],
    ) -> Result<(Map<String, J>, String)> {
        let m = self.metadata().await?;
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.settings.redirect_url.as_str()),
            ("code_verifier", verifier),
        ];
        let mut req = self.http.post(&m.token_endpoint);
        match &self.settings.client_secret {
            Some(secret)
                if m.token_endpoint_auth_methods_supported.is_empty()
                    || m.token_endpoint_auth_methods_supported
                        .iter()
                        .any(|a| a == "client_secret_basic") =>
            {
                // RFC 6749 2.3.1: both parts are form-encoded before Basic encoding
                let enc =
                    |s: &str| form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
                req = req.basic_auth(enc(&self.settings.client_id), Some(enc(secret)));
            }
            Some(secret) => {
                form.push(("client_id", &self.settings.client_id));
                form.push(("client_secret", secret));
            }
            None => form.push(("client_id", &self.settings.client_id)),
        }
        let r = req
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&form)
            .send()
            .await
            .context("OIDC token request")?;
        let status = r.status();
        let body = r.bytes().await?;
        if !status.is_success() {
            // the error body is OAuth's {error, error_description}: no secrets
            let e: J = serde_json::from_slice(&body).unwrap_or(J::Null);
            bail!(
                "OIDC token request failed ({status}): {}",
                e.get("error")
                    .and_then(J::as_str)
                    .unwrap_or("no error code")
            );
        }
        let tr: TokenResponse =
            serde_json::from_slice(&body).context("OIDC token response is not JSON")?;
        let id_token = tr
            .id_token
            .ok_or_else(|| anyhow!("the OIDC token response has no id_token"))?;
        let mut claims = self.verify_id_token(&id_token, nonce).await?;
        if needed.iter().any(|c| !claims.contains_key(*c))
            && let (Some(ui), Some(at)) = (&m.userinfo_endpoint, &tr.access_token)
        {
            match self.userinfo(ui, at).await {
                // OIDC Core 5.3.2: the UserInfo `sub` must match the ID token's
                Ok(extra) if extra.get("sub") == claims.get("sub") => {
                    for (k, v) in extra {
                        claims.entry(k).or_insert(v);
                    }
                }
                Ok(_) => bail!("OIDC UserInfo subject differs from the ID token's"),
                Err(e) => tracing::warn!("OIDC UserInfo request failed: {e:#}"),
            }
        }
        Ok((claims, id_token))
    }

    async fn userinfo(&self, url: &str, access_token: &str) -> Result<Map<String, J>> {
        check_url("userinfo_endpoint", url)?;
        let r = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;
        let status = r.status();
        let body = r.bytes().await?;
        if !status.is_success() {
            bail!("{status}");
        }
        Ok(serde_json::from_slice(&body)?)
    }

    /// Validate an ID token (OpenID Connect Core 1.0, section 3.1.3.7) and return its
    /// claims.
    pub async fn verify_id_token(&self, token: &str, nonce: &str) -> Result<Map<String, J>> {
        let m = self.metadata().await?;
        let header = jsonwebtoken::decode_header(token).context("malformed ID token")?;
        let alg_name = serde_json::to_value(header.alg)?;
        if !m.id_token_signing_alg_values_supported.is_empty()
            && !m
                .id_token_signing_alg_values_supported
                .iter()
                .any(|a| Some(a.as_str()) == alg_name.as_str())
        {
            bail!("ID token algorithm {alg_name} is not advertised by the provider");
        }
        let claims = jwt::verify(
            token,
            &self.keys,
            &m.jwks_uri,
            &Expect {
                issuer: &m.issuer,
                audiences: std::slice::from_ref(&self.settings.client_id),
                algorithms: &self.settings.algorithms,
                required: &["exp", "sub"],
            },
        )
        .await
        .map_err(|e| anyhow!("ID token: {e}"))?;
        check_claims(&claims, &self.settings.client_id, nonce)?;
        Ok(claims)
    }

    /// Whether access tokens of the provider are accepted on the API.
    pub fn accepts_access_tokens(&self) -> bool {
        !self.settings.api_audiences.is_empty()
    }

    /// Validate an access token presented as `Authorization: Bearer` (RFC 7519 §7.2,
    /// shaped by RFC 9068): the signature, `iss`, an `aud` of `api_audience`, `exp` and
    /// `nbf` with leeway, and the required scopes. Returns its claims.
    pub async fn verify_access_token(&self, token: &str) -> Result<Map<String, J>, JwtError> {
        if !self.accepts_access_tokens() {
            return Err(JwtError::Invalid("access tokens are not accepted".into()));
        }
        let m = self
            .metadata()
            .await
            .map_err(|e| JwtError::Unavailable(format!("{e:#}")))?;
        let claims = jwt::verify(
            token,
            &self.keys,
            &m.jwks_uri,
            &Expect {
                issuer: &m.issuer,
                audiences: &self.settings.api_audiences,
                algorithms: &self.settings.algorithms,
                required: &["exp"],
            },
        )
        .await?;
        let granted = jwt::scopes(&claims);
        if let Some(missing) = self
            .settings
            .api_scopes
            .iter()
            .find(|s| !granted.contains(s))
        {
            return Err(JwtError::Invalid(format!("scope {missing} not granted")));
        }
        Ok(claims)
    }

    /// Validate a back-channel logout token (OpenID Connect Back-Channel Logout 1.0,
    /// section 2.6): the signature, `iss`, `aud` naming this client, a recent `iat`, the
    /// logout event, `sid` or `sub`, no `nonce`, and a `jti` not seen before. Returns
    /// `(sid, sub)`.
    pub async fn verify_logout_token(
        &self,
        token: &str,
        now: i64,
    ) -> Result<(Option<String>, Option<String>), JwtError> {
        let m = self
            .metadata()
            .await
            .map_err(|e| JwtError::Unavailable(format!("{e:#}")))?;
        let claims = jwt::verify(
            token,
            &self.keys,
            &m.jwks_uri,
            &Expect {
                issuer: &m.issuer,
                audiences: std::slice::from_ref(&self.settings.client_id),
                algorithms: &self.settings.algorithms,
                required: &[],
            },
        )
        .await?;
        let bad = |why: &str| Err(JwtError::Invalid(why.into()));
        match claims.get("iat").and_then(J::as_f64) {
            Some(iat) if (iat as i64) >= now - LOGOUT_TOKEN_MAX_AGE => {}
            Some(_) => return bad("logout token issued too long ago"),
            None => return bad("logout token has no iat"),
        }
        let has_event = claims
            .get("events")
            .and_then(J::as_object)
            .is_some_and(|e| e.get(BACKCHANNEL_EVENT).is_some_and(J::is_object));
        if !has_event {
            return bad("not a back-channel logout token");
        }
        if claims.contains_key("nonce") {
            return bad("a logout token must not carry a nonce");
        }
        let text = |k: &str| {
            claims
                .get(k)
                .and_then(J::as_str)
                .filter(|v| !v.is_empty() && v.len() <= 256)
                .map(str::to_string)
        };
        let (sid, sub) = (text("sid"), text("sub"));
        if sid.is_none() && sub.is_none() {
            return bad("a logout token needs sid or sub");
        }
        let Some(jti) = text("jti") else {
            return bad("a logout token needs a jti");
        };
        let mut seen = self.seen_jti.lock();
        seen.retain(|_, at| *at >= now - 2 * LOGOUT_TOKEN_MAX_AGE);
        if seen.contains_key(&jti) {
            return bad("a replayed logout token");
        }
        if seen.len() >= MAX_SEEN_JTI {
            return Err(JwtError::Unavailable("too many logout tokens".into()));
        }
        seen.insert(jti, now);
        Ok((sid, sub))
    }

    /// Where to send the browser after a local logout, when the provider supports
    /// RP-initiated logout.
    pub fn end_session_endpoint(&self) -> Option<String> {
        self.metadata.get()?.end_session_endpoint.clone()
    }
}

/// A login started at `/$/auth/oidc/login`, waiting for its callback.
pub struct PendingLogin {
    pub nonce: String,
    pub verifier: String,
    pub return_to: String,
    created: i64,
}

/// The relying party of the current policy and the logins in flight.
pub struct OidcState {
    client: parking_lot::RwLock<Option<Arc<Oidc>>>,
    /// the `[oidc]` settings the client was built from
    built_from: Mutex<Option<String>>,
    pending: Mutex<HashMap<String, PendingLogin>>,
}

fn settings_of(p: &super::policy::Policy) -> Result<Option<OidcSettings>> {
    let (Some(o), Some(public)) = (&p.oidc, &p.public_url) else {
        return Ok(None);
    };
    let client_secret = match &o.client_secret_file {
        Some(f) => Some(
            std::fs::read_to_string(f)
                .with_context(|| format!("reading oidc.client_secret_file {f}"))?
                .trim()
                .to_string(),
        ),
        None => None,
    };
    Ok(Some(OidcSettings {
        issuer: o.issuer.clone(),
        client_id: o.client_id.clone(),
        client_secret,
        redirect_url: format!("{public}/$/auth/oidc/callback"),
        scopes: o.scopes.clone(),
        algorithms: o
            .algorithms
            .iter()
            .map(|a| parse_algorithm(a))
            .collect::<Result<_>>()?,
        api_audiences: o.api_audience.clone(),
        api_scopes: o.api_scopes.clone(),
    }))
}

impl OidcState {
    pub fn new(p: &super::policy::Policy) -> Result<OidcState> {
        let s = OidcState {
            client: parking_lot::RwLock::new(None),
            built_from: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
        };
        s.reconfigure(p)?;
        Ok(s)
    }

    /// Rebuild the client when `[oidc]` changed (discovery then runs again).
    pub fn reconfigure(&self, p: &super::policy::Policy) -> Result<()> {
        let settings = settings_of(p)?;
        let key = settings.as_ref().map(|s| {
            format!(
                "{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{}",
                s.issuer,
                s.client_id,
                s.redirect_url,
                s.scopes,
                s.algorithms,
                s.api_audiences,
                s.api_scopes,
                s.client_secret.as_deref().map_or("", |x| x)
            )
        });
        let mut built = self.built_from.lock();
        if *built == key {
            return Ok(());
        }
        *self.client.write() = match settings {
            Some(s) => Some(Arc::new(Oidc::new(s)?)),
            None => None,
        };
        *built = key;
        Ok(())
    }

    pub fn client(&self) -> Option<Arc<Oidc>> {
        self.client.read().clone()
    }

    /// Remember a new login; returns `(state, nonce, verifier)`.
    pub fn begin(&self, return_to: String, now: i64) -> (String, String, String) {
        let (state, nonce, verifier) = (
            crypto::random_token(32),
            crypto::random_token(32),
            crypto::random_token(32),
        );
        let mut p = self.pending.lock();
        p.retain(|_, l| now - l.created < PENDING_TTL);
        while p.len() >= MAX_PENDING {
            let Some(oldest) = p
                .iter()
                .min_by_key(|(_, l)| l.created)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            p.remove(&oldest);
        }
        p.insert(
            state.clone(),
            PendingLogin {
                nonce: nonce.clone(),
                verifier: verifier.clone(),
                return_to,
                created: now,
            },
        );
        (state, nonce, verifier)
    }

    /// Take (one-time) the pending login of `state`.
    pub fn take(&self, state: &str, now: i64) -> Option<PendingLogin> {
        self.pending
            .lock()
            .remove(state)
            .filter(|l| now - l.created < PENDING_TTL)
    }
}

/// The checks `jsonwebtoken` does not do: `iat`, `azp` and `nonce`.
fn check_claims(claims: &Map<String, J>, client_id: &str, nonce: &str) -> Result<()> {
    let now = jsonwebtoken::get_current_timestamp();
    match claims.get("iat").and_then(J::as_f64) {
        Some(iat) if iat <= (now + LEEWAY_SECS) as f64 => {}
        Some(_) => bail!("ID token issued in the future"),
        None => bail!("ID token has no iat"),
    }
    let multi_aud = claims
        .get("aud")
        .and_then(J::as_array)
        .is_some_and(|a| a.len() > 1);
    match claims.get("azp").and_then(J::as_str) {
        Some(azp) if azp != client_id => bail!("ID token azp is not this client"),
        None if multi_aud => bail!("ID token with several audiences has no azp"),
        _ => {}
    }
    let got = claims.get("nonce").and_then(J::as_str).unwrap_or("");
    if !crypto::ct_eq(got.as_bytes(), nonce.as_bytes()) {
        bail!("ID token nonce mismatch");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn claims(v: J) -> Map<String, J> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn claim_checks() {
        let now = jsonwebtoken::get_current_timestamp();
        let ok = claims(json!({ "iat": now, "nonce": "n1", "aud": "c" }));
        check_claims(&ok, "c", "n1").unwrap();
        assert!(check_claims(&ok, "c", "n2").is_err());
        let future = claims(json!({ "iat": now + 3600, "nonce": "n1" }));
        assert!(check_claims(&future, "c", "n1").is_err());
        let no_iat = claims(json!({ "nonce": "n1" }));
        assert!(check_claims(&no_iat, "c", "n1").is_err());
        let multi = claims(json!({ "iat": now, "nonce": "n1", "aud": ["c", "d"] }));
        assert!(check_claims(&multi, "c", "n1").is_err());
        let multi_azp = claims(json!({ "iat": now, "nonce": "n1", "aud": ["c", "d"], "azp": "c" }));
        check_claims(&multi_azp, "c", "n1").unwrap();
        let other_azp = claims(json!({ "iat": now, "nonce": "n1", "azp": "d" }));
        assert!(check_claims(&other_azp, "c", "n1").is_err());
    }

    #[test]
    fn https_required_except_loopback() {
        check_url("x", "https://idp.example.com").unwrap();
        check_url("x", "http://127.0.0.1:8080/realms/x").unwrap();
        check_url("x", "http://localhost:9000").unwrap();
        assert!(check_url("x", "http://idp.example.com").is_err());
        assert!(check_url("x", "ftp://idp.example.com").is_err());
    }
}
