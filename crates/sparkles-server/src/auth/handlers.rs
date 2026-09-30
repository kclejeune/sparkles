//! The `/$/auth/*` routes: discovery, UI logins and logout, OIDC, the token API and the
//! CLI grants (device codes, loopback codes and the token endpoint).

use super::grants::{Issued, Poll};
use super::policy::{DEVICE, DEVICE_CODE, Failure, MINT, MINT_VIA, Policy};
use super::routes::json_error;
use super::session::Method;
use super::store::rfc3339;
use super::tokens::{Client, TokenRecord};
use super::{Auth, Identity, Kind, Level, Principal, Scheme, Scope, ServerPerm, crypto};
use crate::ratelimit::{Admission, AuthFailed, ClientAddr, ClientKey};
use crate::state::AppState;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Extension, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde::Deserialize;
use serde_json::{Value as J, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use zeroize::Zeroizing;

type St = State<Arc<AppState>>;

/// Name of the session cookie (`__Host-` prefixed over https).
pub const SESSION_COOKIE: &str = "sparkles_session";
/// Name of the cookie binding an OIDC login to its browser.
const OIDC_COOKIE: &str = "sparkles_oidc";

/// Largest body of an `/$/auth/*` request (JSON or form, a few fields): `413` past it.
pub const MAX_AUTH_BODY: usize = 64 << 10;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/auth/config", get(config))
        .route("/$/auth/login", post(login))
        .route("/$/auth/logout", post(logout))
        .route("/$/auth/oidc/login", get(oidc_login))
        .route("/$/auth/oidc/callback", get(oidc_callback))
        .route(
            "/$/auth/tokens",
            get(list_tokens).post(mint_token).delete(revoke_by_owner),
        )
        .route("/$/auth/tokens/{id}", delete(revoke_token))
        .route("/$/auth/device", post(device_start))
        .route("/$/auth/device/{user_code}", get(device_info))
        .route("/$/auth/device/{user_code}/approve", post(device_approve))
        .route("/$/auth/device/{user_code}/deny", post(device_deny))
        .route("/$/auth/cli/authorize", post(cli_authorize))
        .route("/$/auth/token", post(token_endpoint))
        .layer(DefaultBodyLimit::max(MAX_AUTH_BODY))
}

fn not_found() -> Response {
    json_error(StatusCode::NOT_FOUND, "not found")
}

fn bad(msg: &str) -> Response {
    json_error(StatusCode::BAD_REQUEST, msg)
}

/// The auth state, or a 404 (every `/$/auth/*` route but `config` is absent without
/// auth).
#[allow(clippy::result_large_err)]
fn auth_of(st: &AppState) -> Result<Arc<Auth>, Response> {
    st.auth.clone().ok_or_else(not_found)
}

fn no_store(mut r: Response) -> Response {
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// The external base URL: `server.public_url`, or the request's scheme and `Host`.
pub fn base_url(policy: &Policy, h: &HeaderMap) -> String {
    if let Some(u) = &policy.public_url {
        return u.clone();
    }
    let scheme = h
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|p| p.split(',').next().unwrap_or("http").trim().to_string())
        .filter(|p| p == "https" || p == "http")
        .unwrap_or_else(|| "http".into());
    let host = h
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    format!("{scheme}://{host}")
}

/// A `/ui/…` path to return to after a login; anything else becomes `/ui/`.
pub fn safe_return_to(r: Option<&str>) -> String {
    match r {
        Some(r)
            if r.starts_with("/ui/")
                && !r.starts_with("//")
                && !r.contains(['\\', '\r', '\n'])
                && !r.contains("/..") =>
        {
            r.to_string()
        }
        _ => "/ui/".into(),
    }
}

/// A JSON or form body as a flat map of strings (form) or values (JSON).
fn body_map(h: &HeaderMap, body: &[u8]) -> Option<serde_json::Map<String, J>> {
    let ct = h
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ct.starts_with("application/json") {
        return match serde_json::from_slice(body).ok()? {
            J::Object(m) => Some(m),
            _ => None,
        };
    }
    Some(
        form_urlencoded::parse(body)
            .map(|(k, v)| (k.into_owned(), J::String(v.into_owned())))
            .collect(),
    )
}

fn audit_token_minted(rec: &TokenRecord) {
    tracing::info!(
        target: "sparkles::audit",
        event = "token_minted",
        id = rec.id.as_str(),
        owner = rec.owner.log_name().as_str(),
        via = rec.via.as_str(),
        scope = rec.scope.summary().as_str(),
        expires = rec.expires.as_str(),
    );
}

// ----------------------------------------------------------------------- config ------

/// `GET /$/auth/config`: the login methods, for the UI and the CLI.
async fn config(State(st): St) -> Response {
    let Some(auth) = st.auth.clone() else {
        return no_store(axum::Json(json!({ "enabled": false })).into_response());
    };
    let p = auth.policy();
    let mut methods = Vec::new();
    let mut doc = json!({ "enabled": true });
    if let Some(o) = &p.oidc {
        methods.push("oidc");
        doc["oidc"] = json!({ "loginUrl": "/$/auth/oidc/login", "displayName": o.display_name });
    }
    methods.push("token");
    if p.has_users() {
        methods.push("password");
    }
    if p.proxy.is_some() {
        methods.push("proxy");
    }
    doc["methods"] = json!(methods);
    doc["cli"] = json!({
        "authorizeUrl": "/ui/cli/authorize",
        "deviceAuthorizationEndpoint": "/$/auth/device",
        "tokenEndpoint": "/$/auth/token",
        "deviceVerificationUri": "/ui/cli/device",
    });
    no_store(axum::Json(doc).into_response())
}

/// The members `whoami` adds with auth enabled.
pub fn whoami_details(auth: &Auth, p: &Principal, doc: &mut J) {
    let policy = auth.policy();
    if let Some(o) = &p.info.owner {
        if p.kind == Kind::Token {
            doc["principal"]["owner"] = o.log_name().into();
        }
        if let Some(d) = &o.display_name {
            doc["principal"]["displayName"] = d.clone().into();
        }
        if !o.groups.is_empty() && matches!(p.kind, Kind::Oidc | Kind::Proxy) {
            doc["principal"]["groups"] = json!(o.groups);
        }
    }
    if let Some(e) = p.info.expires {
        doc["expires"] = rfc3339(e).into();
    }
    if let Some(c) = &p.info.csrf {
        doc["csrfToken"] = c.clone().into();
    }
    if let Some(id) = &p.info.token_id {
        doc["tokenId"] = id.clone().into();
    }
    doc["canMintTokens"] = (!p.is_anonymous() && !p.info.static_token).into();
    let logout = match p.scheme {
        Scheme::Session => true,
        Scheme::Proxy => policy
            .proxy
            .as_ref()
            .is_some_and(|x| x.logout_url.is_some()),
        _ => false,
    };
    doc["logout"] = logout.into();
    doc["tokensPolicy"] = json!({
        "defaultTtlSeconds": policy.default_ttl,
        "maxTtlSeconds": policy.max_ttl,
    });
}

// ------------------------------------------------------------ password / token login ------

#[derive(Deserialize)]
struct LoginBody {
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

fn session_cookie(auth: &Auth, h: &HeaderMap, raw: &str, max_age: i64) -> HeaderValue {
    auth.cookie_mode(h).set(
        SESSION_COOKIE,
        &auth.keys.sign(SESSION_COOKIE, raw),
        max_age,
    )
}

/// A failed credential check: charged to the client's `preauth` budget.
fn failed(mut r: Response) -> Response {
    r.extensions_mut().insert(AuthFailed);
    r
}

/// The network of the request's client, whose password checks are capped.
fn network(addr: &Option<Extension<ClientAddr>>) -> Option<ClientKey> {
    addr.as_ref().map(|Extension(c)| c.0.network())
}

/// `POST /$/auth/login` with `{user, password}` or `{token}`: a session cookie.
async fn login(
    State(st): St,
    adm: Option<Extension<Admission>>,
    addr: Option<Extension<ClientAddr>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Ok(b) = serde_json::from_slice::<LoginBody>(&body) else {
        return bad("expected {\"user\", \"password\"} or {\"token\"}");
    };
    let b_password = b.password.map(Zeroizing::new);
    let b_token = b.token.map(Zeroizing::new);
    let policy = auth.policy();
    let now = auth.now();
    let invalid = || failed(json_error(StatusCode::UNAUTHORIZED, "invalid credentials"));
    let client = network(&addr);
    let (method, who, token_id, expires) = match (b.user, b_password, b_token) {
        (Some(user), Some(pw), None) => match auth
            .check_password(&user, &pw, adm.as_deref(), client.as_ref())
            .await
        {
            Ok(Some(_)) => (
                Method::Password,
                Identity {
                    kind: Kind::User,
                    name: user,
                    groups: Vec::new(),
                    display_name: None,
                },
                None,
                now + policy.session_ttl,
            ),
            Ok(None) => {
                auth.count_login(Method::Password, "denied");
                auth.count_failure(super::policy::AuthError {
                    scheme: "session",
                    failure: Failure::Invalid,
                });
                return invalid();
            }
            Err(e) if e.failure == Failure::Limited => {
                auth.count_failure(e);
                match adm {
                    Some(Extension(a)) => return a.refusal(),
                    None => return json_error(StatusCode::TOO_MANY_REQUESTS, "too many failures"),
                }
            }
            Err(_) => {
                auth.count_login(Method::Password, "error");
                let mut r = json_error(StatusCode::SERVICE_UNAVAILABLE, "authentication busy");
                r.headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                return r;
            }
        },
        (None, None, Some(t)) => {
            let rec = match auth.token_principal(&policy, &t, Scheme::Bearer) {
                Ok(p) if !p.info.static_token => p
                    .info
                    .token_id
                    .as_deref()
                    .and_then(|id| auth.tokens.get(id)),
                _ => None,
            };
            let Some(rec) = rec else {
                // an address without failures left has its unknown tokens refused
                if let Some(Extension(a)) = &adm
                    && a.exhausted()
                {
                    auth.count_failure(super::policy::AuthError {
                        scheme: "session",
                        failure: Failure::Limited,
                    });
                    return a.refusal();
                }
                auth.count_login(Method::Token, "denied");
                auth.count_failure(super::policy::AuthError {
                    scheme: "session",
                    failure: Failure::Invalid,
                });
                return invalid();
            };
            let exp = (now + policy.session_ttl).min(rec.expires_at());
            (Method::Token, rec.owner.clone(), Some(rec.id.clone()), exp)
        }
        _ => return bad("expected {\"user\", \"password\"} or {\"token\"}"),
    };
    let log = match &token_id {
        Some(id) => format!("token:{id}"),
        None => who.log_name(),
    };
    let (raw, _) = match auth.sessions.create(method, who, token_id, now, expires) {
        Ok(x) => x,
        Err(e) => {
            tracing::error!("cannot store the session: {e:#}");
            auth.count_login(method, "error");
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot store the session",
            );
        }
    };
    auth.count_login(method, "ok");
    tracing::info!(target: "sparkles::audit", event = "login", method = method.as_str(), principal = log.as_str(), result = "ok");
    let mut r = StatusCode::NO_CONTENT.into_response();
    r.headers_mut().append(
        header::SET_COOKIE,
        session_cookie(&auth, &headers, &raw, expires - now),
    );
    no_store(r)
}

/// `POST /$/auth/logout`: end the session; `{redirect}` is the IdP's or proxy's logout
/// page when there is one.
async fn logout(State(st): St, Extension(p): Extension<Principal>, headers: HeaderMap) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let policy = auth.policy();
    let mut redirect = J::Null;
    let mut r_headers = Vec::new();
    if let Some(digest) = p.info.session {
        let id_token = auth.sessions.id_token(&digest);
        let rec = auth.sessions.remove(&digest).ok().flatten();
        r_headers.push(auth.cookie_mode(&headers).set(SESSION_COOKIE, "", 0));
        if rec.as_ref().is_some_and(|r| r.method == Method::Oidc)
            && let (Some(client), Some(public)) = (auth.oidc.client(), &policy.public_url)
            && let Some(end) = client.end_session_endpoint()
            && let Ok(mut u) = reqwest::Url::parse(&end)
        {
            {
                let mut q = u.query_pairs_mut();
                match &id_token {
                    Some(t) => q.append_pair("id_token_hint", t),
                    None => q.append_pair("client_id", &client.settings.client_id),
                };
                q.append_pair("post_logout_redirect_uri", &format!("{public}/ui/"));
            }
            redirect = J::String(u.into());
        }
        tracing::info!(target: "sparkles::audit", event = "logout", principal = p.id().as_str());
    } else if p.scheme == Scheme::Proxy
        && let Some(u) = policy.proxy.as_ref().and_then(|x| x.logout_url.clone())
    {
        redirect = J::String(u);
    }
    let mut r = axum::Json(json!({ "redirect": redirect })).into_response();
    for h in r_headers {
        r.headers_mut().append(header::SET_COOKIE, h);
    }
    no_store(r)
}

// ------------------------------------------------------------------------- OIDC ------

fn login_error(code: &str) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(&format!("/ui/login?error={code}")) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    no_store(r)
}

/// `GET /$/auth/oidc/login?return_to=/ui/…`: redirect to the provider.
async fn oidc_login(
    State(st): St,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(client) = auth.oidc.client() else {
        return not_found();
    };
    let return_to = safe_return_to(q.get("return_to").map(String::as_str));
    let (state, nonce, verifier) = auth.oidc.begin(return_to, auth.now());
    let url = match client.authorize_url(&state, &nonce, &verifier).await {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!("OIDC login unavailable: {e:#}");
            auth.count_login(Method::Oidc, "error");
            return login_error("idp_unavailable");
        }
    };
    let mut r = StatusCode::FOUND.into_response();
    if let Ok(v) = HeaderValue::from_str(&url) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r.headers_mut().append(
        header::SET_COOKIE,
        auth.cookie_mode(&headers).set(
            OIDC_COOKIE,
            &auth.keys.sign(OIDC_COOKIE, &state),
            super::oidc::PENDING_TTL,
        ),
    );
    no_store(r)
}

/// A claim as one string or a list of strings.
fn strings(v: Option<&J>) -> Vec<String> {
    match v {
        Some(J::String(s)) => vec![s.clone()],
        Some(J::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// `GET /$/auth/oidc/callback?code&state`: finish the login and start a session.
async fn oidc_callback(
    State(st): St,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(client) = auth.oidc.client() else {
        return not_found();
    };
    let policy = auth.policy();
    let Some(ocfg) = policy.oidc.clone() else {
        return not_found();
    };
    let now = auth.now();
    let fail = |failure: Failure, code: &str| {
        auth.count_failure(super::policy::AuthError {
            scheme: "oidc",
            failure,
        });
        auth.count_login(
            Method::Oidc,
            if failure == Failure::NotAllowed {
                "denied"
            } else {
                "error"
            },
        );
        tracing::info!(target: "sparkles::audit", event = "login", method = "oidc", result = code);
        let mut r = login_error(code);
        // a forged or replayed state, or a code the provider refused
        if failure != Failure::NotAllowed {
            r.extensions_mut().insert(AuthFailed);
        }
        r.headers_mut().append(
            header::SET_COOKIE,
            auth.cookie_mode(&headers).set(OIDC_COOKIE, "", 0),
        );
        r
    };
    // the state must match the cookie of this browser and a pending login (one-time)
    let mode = auth.cookie_mode(&headers);
    let cookie_state = super::session::cookie(&headers, &mode.name(OIDC_COOKIE))
        .and_then(|c| auth.keys.verify(OIDC_COOKIE, c));
    let state = q.get("state").map(String::as_str).unwrap_or("");
    let bound = cookie_state.is_some_and(|c| crypto::ct_eq(c.as_bytes(), state.as_bytes()));
    let pending = if bound {
        auth.oidc.take(state, now)
    } else {
        None
    };
    let Some(pending) = pending else {
        return fail(Failure::State, "state");
    };
    if q.contains_key("error") {
        return fail(Failure::Idp, "idp");
    }
    let Some(code) = q.get("code") else {
        return fail(Failure::Idp, "idp");
    };
    let needed = [ocfg.name_claim.as_str(), ocfg.groups_claim.as_str()];
    let (claims, id_token) = match client
        .redeem(code, &pending.verifier, &pending.nonce, &needed)
        .await
    {
        Ok(x) => x,
        Err(e) => {
            tracing::warn!("OIDC login failed: {e:#}");
            return fail(Failure::Idp, "idp");
        }
    };
    let Some(name) = claims
        .get(&ocfg.name_claim)
        .and_then(J::as_str)
        .filter(|n| !n.is_empty() && n.len() <= 256)
        .map(str::to_string)
    else {
        tracing::warn!(
            "OIDC login failed: the ID token has no {} claim",
            ocfg.name_claim
        );
        return fail(Failure::Idp, "idp");
    };
    let groups = strings(claims.get(&ocfg.groups_claim));
    if !policy.admitted(&name, &groups) {
        return fail(Failure::NotAllowed, "not_allowed");
    }
    let who = Identity {
        kind: Kind::Oidc,
        name,
        groups,
        display_name: claims.get("name").and_then(J::as_str).map(str::to_string),
    };
    let log = who.log_name();
    let expires = now + policy.session_ttl;
    let (raw, digest) = match auth.sessions.create(Method::Oidc, who, None, now, expires) {
        Ok(x) => x,
        Err(e) => {
            tracing::error!("cannot store the session: {e:#}");
            return fail(Failure::Idp, "idp");
        }
    };
    auth.sessions.set_id_token(digest, id_token);
    auth.count_login(Method::Oidc, "ok");
    tracing::info!(target: "sparkles::audit", event = "login", method = "oidc", principal = log.as_str(), result = "ok");
    let mut r = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(&pending.return_to) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r.headers_mut().append(
        header::SET_COOKIE,
        session_cookie(&auth, &headers, &raw, expires - now),
    );
    r.headers_mut()
        .append(header::SET_COOKIE, mode.set(OIDC_COOKIE, "", 0));
    no_store(r)
}

// ------------------------------------------------------------------------- tokens ------

/// A mint request: name, scope and lifetime.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MintBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    datasets: Option<BTreeMap<String, Level>>,
    #[serde(default)]
    server: Option<Vec<String>>,
    #[serde(default)]
    expires_in: Option<String>,
    /// `api` (default) or `ui`
    #[serde(default)]
    via: Option<String>,
}

/// Mint a token for `p` (the owner is `p`'s identity; a token minter becomes the
/// parent). Returns the token and its record, or an error response.
#[allow(clippy::result_large_err)]
fn mint(
    auth: &Auth,
    p: &Principal,
    body: MintBody,
    via: &str,
    client: Option<Client>,
) -> Result<(Zeroizing<String>, TokenRecord), Response> {
    if p.info.static_token {
        return Err(json_error(
            StatusCode::FORBIDDEN,
            "static tokens cannot mint tokens",
        ));
    }
    let Some(owner) = p.info.owner.clone() else {
        return Err(json_error(StatusCode::FORBIDDEN, "cannot mint tokens"));
    };
    let policy = auth.policy();
    let name = body.name.unwrap_or_else(|| "token".into());
    if name.trim().is_empty() || name.chars().count() > 80 {
        return Err(bad("name must be 1 to 80 characters"));
    }
    let datasets = body
        .datasets
        .unwrap_or_else(|| [("*".to_string(), Level::Admin)].into());
    for pat in datasets.keys() {
        if !super::config::valid_pattern(pat) {
            return Err(bad(&format!("invalid dataset pattern '{pat}'")));
        }
    }
    let server = body.server.unwrap_or_default();
    for s in &server {
        if s != "*" && !ServerPerm::ALL.iter().any(|p| p.as_str() == s) {
            return Err(bad(&format!("unknown server permission '{s}'")));
        }
    }
    let ttl = match body.expires_in.as_deref() {
        None | Some("") => policy.default_ttl,
        Some(d) => match super::config::parse_duration(d) {
            Ok(t) => t,
            Err(e) => return Err(bad(&format!("{e:#}"))),
        },
    };
    if ttl > policy.max_ttl {
        return Err(bad(&format!(
            "expiresIn exceeds the maximum of {} days",
            policy.max_ttl / 86400
        )));
    }
    let now = auth.now();
    let expires = now + ttl;
    // a token (or a session opened with one) mints children that die no later than it;
    // a person's session does not bound the tokens they mint
    let parent_expiry = match p.kind {
        Kind::Token => p
            .info
            .token_id
            .as_deref()
            .and_then(|id| auth.tokens.get(id))
            .map(|t| t.expires_at()),
        _ => None,
    };
    if parent_expiry.is_some_and(|e| expires > e) {
        return Err(bad(
            "expiresIn exceeds the lifetime of the token minting it",
        ));
    }
    let parent = match p.kind {
        Kind::Token => p.info.token_id.clone(),
        _ => None,
    };
    // per owner, whichever credential mints: a rate, and a cap on unexpired tokens
    auth.throttle
        .acquire(MINT, ClientKey::principal(&owner.log_name()), 1)?;
    let token = Zeroizing::new(super::policy::new_token());
    let rec = TokenRecord {
        id: super::tokens::new_id(),
        name: name.trim().to_string(),
        hash: super::policy::token_hash(&token),
        owner,
        parent,
        scope: Scope { datasets, server },
        created: rfc3339(now),
        expires: rfc3339(expires),
        via: via.to_string(),
        client,
        last_used: None,
    };
    match auth
        .tokens
        .insert_within(rec.clone(), now, policy.max_tokens_per_owner)
    {
        Ok(true) => {}
        Ok(false) => {
            return Err(json_error(
                StatusCode::CONFLICT,
                &format!(
                    "at most {} active tokens per owner: revoke unused tokens first",
                    policy.max_tokens_per_owner
                ),
            ));
        }
        Err(e) => {
            tracing::error!("cannot store the token: {e:#}");
            return Err(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot store the token",
            ));
        }
    }
    if let Some(i) = MINT_VIA.iter().position(|v| *v == via) {
        auth.metrics.minted[i].fetch_add(1, Ordering::Relaxed);
    }
    audit_token_minted(&rec);
    Ok((token, rec))
}

fn token_json(auth: &Auth, t: &TokenRecord) -> J {
    let mut j = json!({
        "id": t.id,
        "name": t.name,
        "scope": t.scope,
        "created": t.created,
        "expires": t.expires,
        "lastUsed": auth.tokens.last_used(&t.id),
        "via": t.via,
        "owner": t.owner.log_name(),
    });
    if let Some(c) = &t.client {
        j["client"] = json!(c);
    }
    if let Some(parent) = &t.parent {
        j["parent"] = parent.clone().into();
    }
    j
}

fn same_owner(a: &Identity, b: &Identity) -> bool {
    a.kind == b.kind && a.name == b.name
}

/// `GET /$/auth/tokens[?all=true]`
async fn list_tokens(
    State(st): St,
    Extension(p): Extension<Principal>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let all = q.get("all").is_some_and(|v| v == "true" || v == "1");
    if all && !p.has(ServerPerm::ServerAdmin) {
        return super::forbidden(&p, "server-admin permission required");
    }
    let now = auth.now();
    let owner = p.info.owner.clone();
    let mut tokens: Vec<J> = auth
        .tokens
        .list(|t| {
            t.expires_at() > now && (all || owner.as_ref().is_some_and(|o| same_owner(o, &t.owner)))
        })
        .iter()
        .map(|t| token_json(&auth, t))
        .collect();
    if all {
        for (id, name, expires, summary) in &auth.policy().static_list {
            tokens.push(json!({
                "id": id,
                "name": name,
                "static": true,
                "grants": summary,
                "expires": expires.map(rfc3339),
            }));
        }
    }
    no_store(axum::Json(json!({ "tokens": tokens })).into_response())
}

/// `POST /$/auth/tokens`
async fn mint_token(State(st): St, Extension(p): Extension<Principal>, body: Bytes) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let body: MintBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return bad(&format!("invalid token request: {e}")),
    };
    let via = match body.via.as_deref() {
        None | Some("api") => "api",
        Some("ui") => "ui",
        Some(_) => return bad("via must be api or ui"),
    };
    match mint(&auth, &p, body, via, None) {
        Ok((token, rec)) => {
            let mut j = token_json(&auth, &rec);
            j["token"] = token.as_str().into();
            no_store((StatusCode::CREATED, axum::Json(j)).into_response())
        }
        Err(r) => r,
    }
}

/// `DELETE /$/auth/tokens/{id}` (`self`: the token in use)
async fn revoke_token(
    State(st): St,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let id = if id == "self" {
        match &p.info.token_id {
            Some(t) => t.clone(),
            None => return json_error(StatusCode::NOT_FOUND, "no such token"),
        }
    } else {
        id
    };
    if id.starts_with("cfg-") {
        return json_error(
            StatusCode::FORBIDDEN,
            "static tokens are revoked in the auth config",
        );
    }
    let allowed = auth.tokens.get(&id).is_some_and(|t| {
        p.has(ServerPerm::ServerAdmin)
            || p.info
                .owner
                .as_ref()
                .is_some_and(|o| same_owner(o, &t.owner))
    });
    if !allowed {
        return json_error(StatusCode::NOT_FOUND, "no such token");
    }
    match auth.tokens.remove(std::slice::from_ref(&id)) {
        Ok(n) => {
            auth.metrics.revoked.fetch_add(n as u64, Ordering::Relaxed);
            tracing::info!(target: "sparkles::audit", event = "token_revoked", id = id.as_str(), by = p.id().as_str());
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!("cannot revoke the token: {e:#}");
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "cannot store the change")
        }
    }
}

/// `DELETE /$/auth/tokens?owner=oidc:alice@…` (server-admin)
async fn revoke_by_owner(
    State(st): St,
    Extension(p): Extension<Principal>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(owner) = q.get("owner") else {
        return bad("missing owner parameter");
    };
    let ids: Vec<String> = auth
        .tokens
        .list(|t| t.owner.log_name() == *owner)
        .into_iter()
        .map(|t| t.id)
        .collect();
    match auth.tokens.remove(&ids) {
        Ok(n) => {
            auth.metrics.revoked.fetch_add(n as u64, Ordering::Relaxed);
            tracing::info!(target: "sparkles::audit", event = "token_revoked", owner = owner.as_str(), count = n, by = p.id().as_str());
            axum::Json(json!({ "revoked": n })).into_response()
        }
        Err(e) => {
            tracing::error!("cannot revoke tokens: {e:#}");
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "cannot store the change")
        }
    }
}

// ---------------------------------------------------------------- device grants ------

/// `POST /$/auth/device` (RFC 8628 §3.1)
async fn device_start(
    State(st): St,
    addr: Option<Extension<ClientAddr>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    // per client network (an IPv6 /48), with or without the pre-authentication limit
    let client = network(&addr).unwrap_or(ClientKey::Unknown);
    if let Err(r) = auth.throttle.acquire(DEVICE, client, 1) {
        return r;
    }
    let m = body_map(&headers, &body).unwrap_or_default();
    let field = |k: &str| -> String {
        m.get(k)
            .and_then(J::as_str)
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect()
    };
    let (label, hostname) = (field("label"), field("hostname"));
    let Some((device_code, user_code)) = auth.cli.start(&label, &hostname, auth.now()) else {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "too many pending logins");
    };
    let base = base_url(&auth.policy(), &headers);
    let uri = format!("{base}/ui/cli/device");
    no_store(
        axum::Json(json!({
            "device_code": device_code,
            "user_code": user_code,
            "verification_uri": uri,
            "verification_uri_complete": format!("{uri}?code={user_code}"),
            "expires_in": super::grants::DEVICE_TTL,
            "interval": super::grants::DEVICE_INTERVAL,
        }))
        .into_response(),
    )
}

/// The keys that bound failed user-code lookups: the client's network, and the
/// principal's owner (whichever sessions and addresses it uses).
fn lookup_keys(p: &Principal, addr: &Option<Extension<ClientAddr>>) -> [ClientKey; 2] {
    [
        network(addr).unwrap_or(ClientKey::Unknown),
        ClientKey::principal(&p.rate_key()),
    ]
}

/// Whether both keys have failed lookups left.
#[allow(clippy::result_large_err)]
fn lookups_left(auth: &Auth, keys: &[ClientKey; 2]) -> Result<(), Response> {
    for k in keys {
        auth.throttle.check(DEVICE_CODE, k)?;
    }
    Ok(())
}

/// An unknown user code: charged to both keys, and to the address's `preauth` budget.
fn unknown_code(auth: &Auth, keys: [ClientKey; 2]) -> Response {
    for k in keys {
        auth.throttle.charge(DEVICE_CODE, k, 1);
    }
    failed(json_error(
        StatusCode::NOT_FOUND,
        "no such code, or it expired",
    ))
}

/// `GET /$/auth/device/{user_code}` (interactive): the grant, for the approval page.
async fn device_info(
    State(st): St,
    Extension(p): Extension<Principal>,
    addr: Option<Extension<ClientAddr>>,
    Path(code): Path<String>,
) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let now = auth.now();
    let keys = lookup_keys(&p, &addr);
    if let Err(r) = lookups_left(&auth, &keys) {
        return r;
    }
    match auth.cli.lookup(&code, now) {
        Some(i) => no_store(
            axum::Json(json!({
                "userCode": i.user_code,
                "label": i.label,
                "hostname": i.hostname,
                "expiresIn": i.expires_in,
                "status": i.status,
            }))
            .into_response(),
        ),
        None => unknown_code(&auth, keys),
    }
}

fn issued(token: &Zeroizing<String>, rec: &TokenRecord, p: &Principal, now: i64) -> Issued {
    Issued {
        token: token.clone(),
        token_id: rec.id.clone(),
        principal: p.id(),
        expires_in: rec.expires_at() - now,
    }
}

/// `POST /$/auth/device/{user_code}/approve` (interactive): mint the token the CLI's
/// next poll retrieves.
async fn device_approve(
    State(st): St,
    Extension(p): Extension<Principal>,
    addr: Option<Extension<ClientAddr>>,
    Path(code): Path<String>,
    body: Bytes,
) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let now = auth.now();
    let keys = lookup_keys(&p, &addr);
    if let Err(r) = lookups_left(&auth, &keys) {
        return r;
    }
    if !auth.cli.is_pending(&code, now) {
        if auth.cli.lookup(&code, now).is_none() {
            return unknown_code(&auth, keys);
        }
        return json_error(StatusCode::NOT_FOUND, "no such code, or it expired");
    }
    let body: MintBody = if body.is_empty() {
        MintBody::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(e) => return bad(&format!("invalid approval: {e}")),
        }
    };
    let body = MintBody {
        server: body.server.or_else(|| Some(vec!["*".into()])),
        ..body
    };
    let client = auth
        .cli
        .client_of(&code)
        .map(|(label, hostname)| Client { label, hostname });
    let (token, rec) = match mint(&auth, &p, body, "cli-device", client) {
        Ok(x) => x,
        Err(r) => return r,
    };
    if !auth
        .cli
        .decide(&code, Some(issued(&token, &rec, &p, now)), now)
    {
        let _ = auth.tokens.remove(std::slice::from_ref(&rec.id));
        return json_error(StatusCode::NOT_FOUND, "no such code, or it expired");
    }
    let prefix: String = code.chars().take(4).collect();
    tracing::info!(target: "sparkles::audit", event = "device_approved", code = format!("{prefix}-…").as_str(), approver = p.id().as_str());
    no_store(axum::Json(json!({ "approved": true, "tokenId": rec.id })).into_response())
}

/// `POST /$/auth/device/{user_code}/deny` (interactive)
async fn device_deny(
    State(st): St,
    Extension(p): Extension<Principal>,
    addr: Option<Extension<ClientAddr>>,
    Path(code): Path<String>,
) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let now = auth.now();
    let keys = lookup_keys(&p, &addr);
    if let Err(r) = lookups_left(&auth, &keys) {
        return r;
    }
    if !auth.cli.decide(&code, None, now) {
        if auth.cli.lookup(&code, now).is_none() {
            return unknown_code(&auth, keys);
        }
        return json_error(StatusCode::NOT_FOUND, "no such code, or it expired");
    }
    let prefix: String = code.chars().take(4).collect();
    tracing::info!(target: "sparkles::audit", event = "device_denied", code = format!("{prefix}-…").as_str(), approver = p.id().as_str());
    axum::Json(json!({ "denied": true })).into_response()
}

// ------------------------------------------------------------------- loopback ------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizeBody {
    port: u32,
    state: String,
    code_challenge: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    hostname: Option<String>,
    #[serde(flatten)]
    mint: MintBody,
}

/// `POST /$/auth/cli/authorize` (interactive): mint a token for a CLI waiting on
/// `127.0.0.1:port`, and hand the browser a one-time code for it.
async fn cli_authorize(State(st): St, Extension(p): Extension<Principal>, body: Bytes) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let b: AuthorizeBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return bad(&format!("invalid authorization: {e}")),
    };
    if !(1024..=65535).contains(&b.port) {
        return bad("port must be between 1024 and 65535");
    }
    let challenge_ok = b.code_challenge.len() == 43
        && b.code_challenge
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_');
    if !challenge_ok {
        return bad("codeChallenge must be 43 base64url characters (S256)");
    }
    if b.state.is_empty() || b.state.len() > 256 || !b.state.bytes().all(|c| c.is_ascii_graphic()) {
        return bad("invalid state");
    }
    let clip = |s: Option<String>| -> String {
        s.unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect()
    };
    let client = Client {
        label: clip(b.label),
        hostname: clip(b.hostname),
    };
    let mint_body = MintBody {
        server: b.mint.server.clone().or_else(|| Some(vec!["*".into()])),
        ..b.mint
    };
    let now = auth.now();
    let (token, rec) = match mint(&auth, &p, mint_body, "cli-loopback", Some(client)) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let code = auth.cli.loopback_insert(
        &b.code_challenge,
        b.port as u16,
        issued(&token, &rec, &p, now),
        now,
    );
    let state: String = form_urlencoded::byte_serialize(b.state.as_bytes()).collect();
    no_store(
        axum::Json(json!({
            "redirect": format!("http://127.0.0.1:{}/callback?code={code}&state={state}", b.port),
        }))
        .into_response(),
    )
}

// --------------------------------------------------------------- token endpoint ------

fn oauth_error(code: &str, desc: &str) -> Response {
    no_store(
        (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({ "error": code, "error_description": desc })),
        )
            .into_response(),
    )
}

fn token_response(i: Issued) -> Response {
    no_store(
        axum::Json(json!({
            "access_token": i.token.as_str(),
            "token_type": "Bearer",
            "expires_in": i.expires_in,
            "token_id": i.token_id,
            "principal": i.principal,
        }))
        .into_response(),
    )
}

/// The port of a loopback redirect URI `http://127.0.0.1:P/callback`.
fn loopback_port(uri: &str) -> Option<u16> {
    let rest = uri.strip_prefix("http://127.0.0.1:")?;
    let (port, path) = rest.split_once('/')?;
    (path == "callback").then(|| port.parse().ok())?
}

/// `POST /$/auth/token`: the OAuth token endpoint of both CLI grants.
async fn token_endpoint(State(st): St, headers: HeaderMap, body: Bytes) -> Response {
    let auth = match auth_of(&st) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(m) = body_map(&headers, &body) else {
        return oauth_error("invalid_request", "unreadable body");
    };
    let s = |k: &str| m.get(k).and_then(J::as_str).unwrap_or("");
    let now = auth.now();
    match s("grant_type") {
        "urn:ietf:params:oauth:grant-type:device_code" => {
            if s("device_code").is_empty() {
                return oauth_error("invalid_request", "missing device_code");
            }
            match auth.cli.poll(s("device_code"), now) {
                Poll::Pending => oauth_error("authorization_pending", "waiting for approval"),
                Poll::SlowDown => oauth_error("slow_down", "polling too fast"),
                Poll::Denied => oauth_error("access_denied", "the login was denied"),
                Poll::Expired => oauth_error("expired_token", "the device code expired"),
                Poll::Token(i) => token_response(i),
            }
        }
        "authorization_code" => {
            let (code, verifier) = (s("code"), s("code_verifier"));
            if code.is_empty() || verifier.is_empty() {
                return oauth_error("invalid_request", "missing code or code_verifier");
            }
            let port = match s("redirect_uri") {
                "" => None,
                u => match loopback_port(u) {
                    Some(p) => Some(p),
                    None => return oauth_error("invalid_grant", "invalid redirect_uri"),
                },
            };
            match auth.cli.loopback_redeem(code, verifier, port, now) {
                Some(i) => token_response(i),
                None => failed(oauth_error(
                    "invalid_grant",
                    "invalid, used or expired code",
                )),
            }
        }
        "" => oauth_error("invalid_request", "missing grant_type"),
        _ => oauth_error("unsupported_grant_type", "unsupported grant_type"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_to_and_ports() {
        assert_eq!(safe_return_to(Some("/ui/datasets")), "/ui/datasets");
        assert_eq!(safe_return_to(Some("https://evil.example")), "/ui/");
        assert_eq!(safe_return_to(Some("//evil.example/ui/")), "/ui/");
        assert_eq!(safe_return_to(Some("/ui/../$/x")), "/ui/");
        assert_eq!(safe_return_to(None), "/ui/");
        assert_eq!(
            loopback_port("http://127.0.0.1:50123/callback"),
            Some(50123)
        );
        assert_eq!(loopback_port("http://localhost:50123/callback"), None);
        assert_eq!(loopback_port("http://127.0.0.1:50123/other"), None);
    }
}
